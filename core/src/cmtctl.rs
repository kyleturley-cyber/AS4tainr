//! Commitment control: commitment definitions, COMMIT and ROLLBACK.
//!
//! Written from IBM's public documentation (STRCMTCTL, ENDCMTCTL, COMMIT
//! and ROLLBACK commands; ILE RPG Reference COMMIT and ROLBK, the COMMIT
//! file keyword; Db2 for i SQL Reference COMMIT, ROLLBACK and the COMMIT
//! precompiler option; the "Commitment control" topics). A commitment
//! definition belongs to an activation group (CMTSCOPE(*ACTGRP), the
//! default) or to the job (CMTSCOPE(*JOB)); a program uses its activation
//! group's definition, else the job's. Changes made through it (records
//! written, updated or deleted in files opened under commitment control,
//! embedded SQL run with a commit level other than *NONE) are pending
//! until COMMIT makes them permanent or ROLLBACK removes them; the records
//! changed stay locked until then. Ending the definition (ENDCMTCTL) or
//! the job rolls back what is pending.
//!
//! Here a definition is one database session (as the restricted SQL
//! login) with a transaction open on it; everything done under the
//! definition runs on that session.

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_postgres::Client;

/// The activation group of the job-level definition.
pub const JOB_SCOPE: &str = "*JOB";

pub struct Definition {
    /// *CHG, *CS, *ALL (STRCMTCTL LCKLVL), or the SQL levels *RR.
    pub lcklvl: String,
    /// *ACTGRP or *JOB.
    pub scope: String,
    pub client: Client,
    /// Changes since the last COMMIT or ROLLBACK.
    changes: AtomicU64,
}

impl std::fmt::Debug for Definition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Definition")
            .field("lcklvl", &self.lcklvl)
            .field("scope", &self.scope)
            .finish()
    }
}

/// The definitions of all jobs, by (job, activation group or *JOB).
type Definitions = HashMap<(String, String), Arc<Definition>>;

static DEFINITIONS: Mutex<Option<Definitions>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Definitions) -> R) -> R {
    let mut g = DEFINITIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// The isolation level a lock level maps to: PostgreSQL never reads
/// uncommitted data, so *CHG and *CS read committed data; *ALL keeps
/// what it read (repeatable read); *RR is serializable.
fn isolation(lcklvl: &str) -> &'static str {
    match lcklvl {
        "*ALL" | "*RS" => "REPEATABLE READ",
        "*RR" => "SERIALIZABLE",
        _ => "READ COMMITTED",
    }
}

impl Definition {
    async fn begin(&self) -> Result<()> {
        self.client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL {}",
                isolation(&self.lcklvl)
            ))
            .await?;
        Ok(())
    }

    /// A change made under the definition.
    pub fn changed(&self) {
        self.changes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn pending(&self) -> u64 {
        self.changes.load(Ordering::Relaxed)
    }

    /// COMMIT: the pending changes become permanent; a new unit of work
    /// starts.
    pub async fn commit(&self) -> Result<()> {
        self.client.batch_execute("COMMIT").await?;
        self.changes.store(0, Ordering::Relaxed);
        self.begin().await
    }

    /// ROLLBACK: the pending changes are removed.
    pub async fn rollback(&self) -> Result<()> {
        self.client.batch_execute("ROLLBACK").await?;
        self.changes.store(0, Ordering::Relaxed);
        self.begin().await
    }
}

/// The definition a program in `actgrp` uses: its activation group's,
/// else the job's.
pub fn find(job: &str, actgrp: &str) -> Option<Arc<Definition>> {
    with(|m| {
        m.get(&(job.to_string(), actgrp.to_string()))
            .or_else(|| m.get(&(job.to_string(), JOB_SCOPE.to_string())))
            .cloned()
    })
}

/// STRCMTCTL (or SQL starting commitment control for a program run with a
/// commit level): a definition for the activation group or the job.
/// CPF8351 when the scope already has one.
pub async fn start(
    catalog: &Client,
    database_url: &str,
    job: &str,
    actgrp: &str,
    lcklvl: &str,
    scope_job: bool,
) -> Result<Arc<Definition>> {
    let scope = if scope_job { JOB_SCOPE } else { actgrp };
    let key = (job.to_string(), scope.to_string());
    if with(|m| m.contains_key(&key)) {
        bail!(
            "CPF8351: Commitment control is already started for {}.",
            if scope_job {
                "the job".to_string()
            } else {
                format!("activation group {actgrp}")
            }
        );
    }
    let client = crate::sql::connect_as_sql_role(catalog, database_url).await?;
    crate::runtime::job_session(&client, Some(job), crate::runtime::default_record_wait()).await?;
    let d = Arc::new(Definition {
        lcklvl: lcklvl.to_string(),
        scope: if scope_job {
            "*JOB".into()
        } else {
            "*ACTGRP".into()
        },
        client,
        changes: AtomicU64::new(0),
    });
    d.begin().await?;
    with(|m| m.insert(key, d.clone()));
    Ok(d)
}

/// ENDCMTCTL: the definition ends; pending changes are rolled back and
/// their number returned. CPF8350 when there is no definition.
pub async fn end(job: &str, actgrp: &str) -> Result<u64> {
    let d = with(|m| {
        m.remove(&(job.to_string(), actgrp.to_string()))
            .or_else(|| m.remove(&(job.to_string(), JOB_SCOPE.to_string())))
    });
    let Some(d) = d else {
        bail!("CPF8350: Commitment definition not found.");
    };
    let n = d.pending();
    let _ = d.client.batch_execute("ROLLBACK").await;
    Ok(n)
}

/// The job ended: its definitions end, rolling back what is pending.
pub async fn end_job(job: &str) {
    let ds: Vec<Arc<Definition>> = with(|m| {
        let keys: Vec<_> = m.keys().filter(|(j, _)| j == job).cloned().collect();
        keys.into_iter().filter_map(|k| m.remove(&k)).collect()
    });
    for d in ds {
        let _ = d.client.batch_execute("ROLLBACK").await;
    }
}
