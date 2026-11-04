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

/// Each allocation a job holds, in the order it was made (WRKOBJLCK
/// lists a lock allocated twice twice, in that order: PUB400, verify39
/// h39_call): the PostgreSQL backend that holds it and the object's key.
/// Rows of ended backends are passed by (the lock went with the session)
/// and cleared at the next allocation.
pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS as4tainr.object_locks (
    seq      bigserial PRIMARY KEY,
    pid      integer NOT NULL,
    started  timestamptz NOT NULL,
    key      text NOT NULL,
    state    text NOT NULL
);
";

/// The backend's identity: its PID and when it started (a PID can be
/// reused by a later session).
const ME: &str = "(SELECT pid, backend_start FROM pg_stat_activity WHERE pid = pg_backend_pid())";

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
            client
                .execute(
                    "DELETE FROM as4tainr.object_locks o WHERE NOT EXISTS
                       (SELECT 1 FROM pg_stat_activity a WHERE a.pid = o.pid AND a.backend_start = o.started)",
                    &[],
                )
                .await?;
            client
                .execute(
                    &format!(
                        "INSERT INTO as4tainr.object_locks (pid, started, key, state)
                         SELECT pid, backend_start, $1, $2 FROM {ME} m"
                    ),
                    &[&t.key(), &state],
                )
                .await?;
            return Ok(true);
        }
        drop_shared(client, t, state).await?;
        if wait.is_some_and(|w| start.elapsed() >= w) {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Deallocate one allocation (the latest); false when the job held none
/// in that state.
pub async fn deallocate(client: &Client, t: &Target, state: &str) -> Result<bool> {
    if !drop_shared(client, t, state).await? {
        return Ok(false);
    }
    client
        .execute(
            &format!(
                "DELETE FROM as4tainr.object_locks WHERE seq = (
                   SELECT max(o.seq) FROM as4tainr.object_locks o, {ME} m
                    WHERE o.pid = m.pid AND o.started = m.backend_start
                      AND o.key = $1 AND o.state = $2)"
            ),
            &[&t.key(), &state],
        )
        .await?;
    Ok(true)
}

/// An allocation someone holds: the job (its connection's application
/// name) and the state.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub job: String,
    pub state: String,
}

/// The allocations held on an object: by job (in the order the jobs
/// first allocated it), each job's in the order it made them.
pub async fn held(client: &Client, t: &Target) -> Result<Vec<Held>> {
    Ok(client
        .query(
            "SELECT a.application_name, o.state
               FROM as4tainr.object_locks o
               JOIN pg_stat_activity a ON a.pid = o.pid AND a.backend_start = o.started
              WHERE o.key = $1
              ORDER BY min(o.seq) OVER (PARTITION BY o.pid), o.seq",
            &[&t.key()],
        )
        .await?
        .iter()
        .map(|r| Held {
            job: r.get(0),
            state: r.get(1),
        })
        .collect())
}

/// A job's name, user and number from the name its connection carries
/// ("number/user/name"); another name is the job's name alone.
pub fn job_parts(job: &str) -> (String, String, String) {
    let p: Vec<&str> = job.split('/').collect();
    match p.as_slice() {
        [n, u, j] => (j.to_string(), u.to_string(), n.to_string()),
        _ => (job.to_string(), String::new(), String::new()),
    }
}

/// WRKOBJLCK OUTPUT(*PRINT) in IBM i 7.5's layout (PUB400, verify39
/// h39_wrklck and h39_call): style A header with the title at column 39,
/// the object line, "Object Locks", the column heading, one line per
/// allocation (the job's name, user and number on its first only), the
/// end-of-listing line. The line for no locks is as4tainr's own text.
pub fn print(
    t: &Target,
    library: &str,
    locks: &[Held],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    use crate::report::at;
    let mut out = vec![
        crate::report::header_a(17, 39, "Work with Object Locks", 0, "", stamp, 1),
        at(&[
            (1, "Object . . . . :"),
            (20, &t.name),
            (35, "Library . . . . :"),
            (55, library),
            (70, "Type . . . . :"),
            (87, &t.objtype),
            (102, "ASP device . . :"),
            (121, "*SYSBAS"),
        ]),
        at(&[(37, "Object Locks")]),
        at(&[
            (4, "Job"),
            (16, "User"),
            (28, "Number"),
            (38, "Lock"),
            (46, "Status"),
            (55, "Scope"),
            (65, "Thread"),
        ]),
    ];
    if locks.is_empty() {
        out.push(" No job holds a lock on this object.".into());
    }
    let mut last = String::new();
    for h in locks {
        let (name, user, number) = if h.job == last {
            Default::default()
        } else {
            job_parts(&h.job)
        };
        last = h.job.clone();
        out.push(at(&[
            (4, &name),
            (16, &user),
            (28, &number),
            (37, &h.state),
            (47, "HELD"),
            (55, "*JOB"),
        ]));
    }
    out.push("                        * * * * *   E N D   O F   L I S T I N G   * * * * *".into());
    out
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

    // WRKOBJLCK OUTPUT(*PRINT): PUB400 (IBM i 7.5), verify39 h39_call (a
    // job holding *SHRRD twice and *EXCL) and h39_wrklck (no locks), with
    // IBM's product line swapped for ours (same columns) and the line for
    // no locks in as4tainr's own words.
    #[test]
    fn printed_locks_as_ibm_i_lays_them_out() {
        let t = Target {
            library: "AS4TRAINR1".into(),
            name: "GLD39D".into(),
            objtype: "*DTAARA".into(),
        };
        let stamp = crate::report::Stamp {
            date: "26-11-03".into(),
            time: "14:32:15".into(),
            zone: String::new(),
        };
        let job = "094772/QUSER/QP0ZSPWT".to_string();
        let held = |s: &str| Held {
            job: job.clone(),
            state: s.into(),
        };
        let lines = print(
            &t,
            "AS4TRAINR1",
            &[held("*SHRRD"), held("*SHRRD"), held("*EXCL")],
            &stamp,
        );
        let want = [
            " AS4T001 V0R1M0 261021                Work with Object Locks                                    26-11-03 14:32:15        Page    1",
            "Object . . . . :   GLD39D         Library . . . . :   AS4TRAINR1     Type . . . . :   *DTAARA        ASP device . . :   *SYSBAS",
            "                                    Object Locks",
            "   Job         User        Number    Lock    Status   Scope     Thread",
            "   QP0ZSPWT    QUSER       094772   *SHRRD    HELD    *JOB",
            "                                    *SHRRD    HELD    *JOB",
            "                                    *EXCL     HELD    *JOB",
            "                        * * * * *   E N D   O F   L I S T I N G   * * * * *",
        ];
        assert_eq!(lines, want);
        let none = print(&t, "AS4TRAINR1", &[], &stamp);
        assert_eq!(none.len(), 6);
        assert_eq!(none[3], want[3]);
        assert_eq!(none[5], want[7]);
    }
}
