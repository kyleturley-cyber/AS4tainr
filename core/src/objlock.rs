//! Object locks: ALCOBJ, DLCOBJ and WRKOBJLCK.
//!
//! A job's object lock is a PostgreSQL session advisory lock on the job's
//! connection, so it lasts until DLCOBJ or the end of the job, and a job
//! holds the same lock more than once when it allocates it more than once
//! (DLCOBJ takes one away). Each lock state has its own key for the
//! object; a job holds that key shared, and the states that conflict with
//! it (CL reference, ALCOBJ: "Lock state" table) are checked by trying
//! each one's key exclusively, which succeeds only when no other job holds
//! it (a job's own locks never conflict with each other, as on IBM i).
//! The job's own key is taken first, so two jobs asking at the same time
//! for conflicting states both see the other's and wait (neither is given
//! its lock wrongly). WAIT is the seconds to keep trying.
//!
//! Not done: file opens and data area `IN *LOCK` take no object locks, so
//! ALCOBJ keeps out other ALCOBJs only; member locks; CONFLICT(*RQSRLS);
//! docs/fidelity-gaps.md.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use tokio_postgres::Client;

/// The lock states, strongest first.
pub const STATES: [&str; 5] = ["*EXCL", "*EXCLRD", "*SHRUPD", "*SHRNUP", "*SHRRD"];

/// The states a lock state cannot be held with by another job (CL
/// reference, ALCOBJ, the lock state table: *EXCL with none, *EXCLRD with
/// *SHRRD only, *SHRUPD with *SHRUPD and *SHRRD, *SHRNUP with *SHRNUP and
/// *SHRRD, *SHRRD with all but *EXCL).
pub fn conflicts(state: &str) -> &'static [&'static str] {
    match state {
        "*EXCL" => &STATES,
        "*EXCLRD" => &["*EXCL", "*EXCLRD", "*SHRUPD", "*SHRNUP"],
        "*SHRUPD" => &["*EXCL", "*EXCLRD", "*SHRNUP"],
        "*SHRNUP" => &["*EXCL", "*EXCLRD", "*SHRUPD"],
        _ => &["*EXCL"],
    }
}

/// An object as its lock knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub library: String,
    pub name: String,
    pub objtype: String,
}

impl Target {
    fn key(&self) -> String {
        format!("{}/{} {}", self.library, self.name, self.objtype)
    }
}

fn class(state: &str) -> String {
    format!("as4tainr object lock {state}")
}

async fn take_shared(client: &Client, t: &Target, state: &str) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_lock_shared(hashtext($1), hashtext($2))",
            &[&class(state), &t.key()],
        )
        .await?;
    Ok(())
}

async fn drop_shared(client: &Client, t: &Target, state: &str) -> Result<bool> {
    Ok(client
        .query_one(
            "SELECT pg_advisory_unlock_shared(hashtext($1), hashtext($2))",
            &[&class(state), &t.key()],
        )
        .await?
        .get(0))
}

/// Does another job hold one of the states that conflict with `state`?
async fn conflicting(client: &Client, t: &Target, state: &str) -> Result<bool> {
    for c in conflicts(state) {
        let free: bool = client
            .query_one(
                "SELECT pg_try_advisory_lock(hashtext($1), hashtext($2))",
                &[&class(c), &t.key()],
            )
            .await?
            .get(0);
        if !free {
            return Ok(true);
        }
        client
            .execute(
                "SELECT pg_advisory_unlock(hashtext($1), hashtext($2))",
                &[&class(c), &t.key()],
            )
            .await?;
    }
    Ok(false)
}

/// Allocate one object: keep trying until `wait` has passed. Ok(false)
/// when another job's lock was in the way.
pub async fn allocate(
    client: &Client,
    t: &Target,
    state: &str,
    wait: Option<Duration>,
) -> Result<bool> {
    if !STATES.contains(&state) {
        bail!("Lock state {state} is not valid; use *EXCL, *EXCLRD, *SHRUPD, *SHRNUP or *SHRRD.");
    }
    let start = Instant::now();
    loop {
        take_shared(client, t, state).await?;
        if !conflicting(client, t, state).await? {
            return Ok(true);
        }
        drop_shared(client, t, state).await?;
        if wait.is_some_and(|w| start.elapsed() >= w) {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Deallocate one lock; false when the job held none in that state.
pub async fn deallocate(client: &Client, t: &Target, state: &str) -> Result<bool> {
    drop_shared(client, t, state).await
}

/// A lock someone holds: the job (its connection's application name), the
/// state, and how many times it is held.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub job: String,
    pub state: String,
    pub count: i64,
}

/// The locks held on an object, strongest state first.
pub async fn held(client: &Client, t: &Target) -> Result<Vec<Held>> {
    let mut out = Vec::new();
    for s in STATES {
        // A lock held more than once is one pg_locks row: the count is not
        // shown there, so a held lock counts once.
        let rows = client
            .query(
                "SELECT a.application_name, count(*)
                   FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
                  WHERE l.locktype = 'advisory' AND l.objsubid = 2 AND l.granted
                    AND l.mode = 'ShareLock'
                    AND l.classid = (hashtext($1)::bigint & 4294967295)::oid
                    AND l.objid = (hashtext($2)::bigint & 4294967295)::oid
                  GROUP BY 1 ORDER BY 1",
                &[&class(s), &t.key()],
            )
            .await?;
        for r in rows {
            out.push(Held {
                job: r.get(0),
                state: s.to_string(),
                count: r.get(1),
            });
        }
    }
    Ok(out)
}

/// One object of ALCOBJ's or DLCOBJ's OBJ list: "(NAME TYPE STATE MBR)"
/// or the words without parentheses. Name (qualified or not), type and
/// state, upper case.
pub fn parse_element(e: &str) -> Result<(String, String, String)> {
    let inner = e.trim().trim_start_matches('(').trim_end_matches(')');
    let words: Vec<String> = inner
        .split_whitespace()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    match words.as_slice() {
        [name, ty, state] | [name, ty, state, _] => Ok((name.clone(), ty.clone(), state.clone())),
        _ => bail!("CPD0084: Object specification ({inner}) needs a name, an object type and a lock state."),
    }
}

/// WAIT: *IMMED none, a number of seconds, *NOMAX no limit, *CLS the
/// job's class's default wait (30 seconds in QSYS/QBATCH's and QINTER's
/// classes).
pub fn wait_time(w: &str) -> Result<Option<Duration>> {
    Ok(match w.trim().to_ascii_uppercase().as_str() {
        "*IMMED" | "0" => Some(Duration::ZERO),
        "*NOMAX" => None,
        "*CLS" | "" => Some(Duration::from_secs(30)),
        n => match n.parse::<u64>() {
            Ok(s) if s <= 32767 => Some(Duration::from_secs(s)),
            _ => bail!("CPD0084: WAIT({n}) is not valid; use 0-32767, *IMMED, *CLS or *NOMAX."),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // CL reference, ALCOBJ, lock state table: which states can be held at
    // once by different jobs.
    #[test]
    fn lock_state_conflicts() {
        let ok = |a: &str, b: &str| !conflicts(a).contains(&b);
        assert!(!ok("*EXCL", "*SHRRD"));
        assert!(ok("*EXCLRD", "*SHRRD"));
        assert!(!ok("*EXCLRD", "*SHRUPD"));
        assert!(ok("*SHRUPD", "*SHRUPD"));
        assert!(!ok("*SHRUPD", "*SHRNUP"));
        assert!(ok("*SHRNUP", "*SHRNUP"));
        assert!(ok("*SHRRD", "*SHRUPD"));
        assert!(!ok("*SHRRD", "*EXCL"));
        // The table is symmetric.
        for a in STATES {
            for b in STATES {
                assert_eq!(ok(a, b), ok(b, a), "{a} {b}");
            }
        }
    }

    #[test]
    fn object_elements_and_waits() {
        assert_eq!(
            parse_element("(custmast *file *exclrd)").unwrap(),
            ("CUSTMAST".into(), "*FILE".into(), "*EXCLRD".into())
        );
        assert!(parse_element("(X *FILE)").is_err());
        assert_eq!(wait_time("5").unwrap(), Some(Duration::from_secs(5)));
        assert_eq!(wait_time("*NOMAX").unwrap(), None);
    }
}
