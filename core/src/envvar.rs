//! Environment variables: job-level (kept in this process, by job) and
//! system-level (`as4tainr.environment_variables`). ADDENVVAR, CHGENVVAR,
//! RMVENVVAR and WRKENVVAR; the C runtime's getenv and putenv (and
//! Qp0zGetEnv, Qp0zPutEnv) in `rpg/crt.rs`; QSH passes the job's
//! variables to the commands it runs.
//!
//! Written from IBM's CL reference (ADDENVVAR, CHGENVVAR, RMVENVVAR): a
//! name is up to 128 bytes with no `=`, blank or null character, its case
//! kept as typed in apostrophes; a value up to 1024 bytes or *NULL;
//! LEVEL(*SYS) needs *JOBCTL special authority (CPFA98E); CPFA980 when
//! ADDENVVAR finds the variable with REPLACE(*NO), CPFA981 when CHGENVVAR
//! or RMVENVVAR does not find it, CPFA982 for a name that is not valid. A
//! job starts with a copy of the system-level variables (Environment
//! variable APIs, "Environment variables"): here the copy is made the
//! first time the job uses its variables.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.environment_variables (
    name   text PRIMARY KEY,
    value  text,
    ccsid  integer NOT NULL DEFAULT 0
);
"#;

/// A variable's value (None: *NULL) and CCSID (0: the job's).
#[derive(Debug, Clone, PartialEq)]
pub struct Var {
    pub value: Option<String>,
    pub ccsid: i32,
}

type Vars = BTreeMap<String, Var>;

/// Job-level variables, by job key (`Job::job_key`).
static JOBS: Mutex<Option<HashMap<String, Vars>>> = Mutex::new(None);

pub async fn init(client: &Client) -> Result<()> {
    client.batch_execute(DDL).await?;
    Ok(())
}

/// The system-level variables.
pub async fn system_vars(client: &Client) -> Result<Vars> {
    let rows = client
        .query(
            "SELECT name, value, ccsid FROM as4tainr.environment_variables ORDER BY name",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<_, String>(0),
                Var {
                    value: r.get(1),
                    ccsid: r.get(2),
                },
            )
        })
        .collect())
}

/// The job's variables (a copy of the system-level ones the first time).
pub async fn job_vars(client: &Client, job: &str) -> Result<Vars> {
    if let Some(v) = JOBS.lock().unwrap().as_ref().and_then(|m| m.get(job)) {
        return Ok(v.clone());
    }
    let sys = system_vars(client).await.unwrap_or_default();
    let mut g = JOBS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    Ok(m.entry(job.to_string()).or_insert(sys).clone())
}

/// Change the job's variables (after `job_vars` made them).
fn with_job<R>(job: &str, f: impl FnOnce(&mut Vars) -> R) -> R {
    let mut g = JOBS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    f(m.entry(job.to_string()).or_default())
}

/// The job ends: its variables go.
pub fn end_job(job: &str) {
    if let Some(m) = JOBS.lock().unwrap().as_mut() {
        m.remove(job);
    }
}

/// CPFA982 unless the name is valid.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains('=')
        || name.contains(' ')
        || name.contains('\0')
    {
        bail!("CPFA982: Environment variable name {name} is not valid.");
    }
    Ok(())
}

/// A job's variable (getenv): None when it is not set.
pub async fn get(client: &Client, job: &str, name: &str) -> Result<Option<Var>> {
    Ok(job_vars(client, job).await?.get(name).cloned())
}

/// Which variables a command changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Job,
    System,
}

/// ADDENVVAR (`replace`: REPLACE(*YES)); putenv adds or replaces.
pub async fn add(
    client: &Client,
    job: &str,
    level: Level,
    name: &str,
    var: Var,
    replace: bool,
) -> Result<()> {
    check_name(name)?;
    match level {
        Level::Job => {
            job_vars(client, job).await?;
            with_job(job, |v| {
                if v.contains_key(name) && !replace {
                    bail!("CPFA980: Environment variable {name} already exists.");
                }
                v.insert(name.to_string(), var);
                Ok(())
            })
        }
        Level::System => {
            let sql = if replace {
                "INSERT INTO as4tainr.environment_variables (name, value, ccsid) VALUES ($1, $2, $3)
                 ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value, ccsid = EXCLUDED.ccsid"
            } else {
                "INSERT INTO as4tainr.environment_variables (name, value, ccsid) VALUES ($1, $2, $3)
                 ON CONFLICT (name) DO NOTHING"
            };
            let n = client
                .execute(sql, &[&name, &var.value, &var.ccsid])
                .await?;
            if n == 0 {
                bail!("CPFA980: Environment variable {name} already exists.");
            }
            Ok(())
        }
    }
}

/// CHGENVVAR: `value` None keeps it (*SAME), `ccsid` None keeps it.
pub async fn change(
    client: &Client,
    job: &str,
    level: Level,
    name: &str,
    value: Option<Option<String>>,
    ccsid: Option<i32>,
) -> Result<()> {
    check_name(name)?;
    let missing = || anyhow::anyhow!("CPFA981: Environment variable {name} does not exist.");
    match level {
        Level::Job => {
            job_vars(client, job).await?;
            with_job(job, |v| {
                let var = v.get_mut(name).ok_or_else(missing)?;
                if let Some(x) = value {
                    var.value = x;
                }
                if let Some(c) = ccsid {
                    var.ccsid = c;
                }
                Ok(())
            })
        }
        Level::System => {
            let Some(old) = system_vars(client).await?.remove(name) else {
                return Err(missing());
            };
            let value = value.unwrap_or(old.value);
            let ccsid = ccsid.unwrap_or(old.ccsid);
            client
                .execute(
                    "UPDATE as4tainr.environment_variables SET value = $2, ccsid = $3 WHERE name = $1",
                    &[&name, &value, &ccsid],
                )
                .await?;
            Ok(())
        }
    }
}

/// RMVENVVAR.
pub async fn remove(client: &Client, job: &str, level: Level, name: &str) -> Result<()> {
    check_name(name)?;
    let removed = match level {
        Level::Job => {
            job_vars(client, job).await?;
            with_job(job, |v| v.remove(name).is_some())
        }
        Level::System => {
            client
                .execute(
                    "DELETE FROM as4tainr.environment_variables WHERE name = $1",
                    &[&name],
                )
                .await?
                > 0
        }
    };
    if !removed {
        bail!("CPFA981: Environment variable {name} does not exist.");
    }
    Ok(())
}

/// The job's variables as `NAME=value` for a process (a *NULL value: the
/// name with an empty value).
pub async fn for_process(client: &Client, job: &str) -> Result<Vec<(String, String)>> {
    Ok(job_vars(client, job)
        .await?
        .into_iter()
        .map(|(k, v)| (k, v.value.unwrap_or_default()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // CL reference, ADDENVVAR ENVVAR: no equal sign, blank or null
    // character; at most 128 bytes.
    #[test]
    fn names_with_equal_sign_or_blank_are_not_valid() {
        assert!(check_name("QIBM_QSH_CMD_OUTPUT").is_ok());
        assert!(check_name("lower_Case").is_ok());
        for bad in ["A=B", "A B", "", &"X".repeat(129)] {
            let e = check_name(bad).unwrap_err().to_string();
            assert!(e.starts_with("CPFA982"), "{bad}: {e}");
        }
    }
}
