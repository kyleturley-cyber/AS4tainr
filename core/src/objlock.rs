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
//! An RPG program's file open, and a CL program's first RCVF of a
//! declared file, take *SHRRD on the file (*SHRUPD for update; `open_file`); the close (CL's
//! CLOSE or the program's end) gives it back. Conflicts are read from the allocations
//! recorded by job (`owner`), a job's other sessions (its RPG session) not
//! counting.
//! Not done: SQL and data area `IN *LOCK` take no object
//! locks; member and data locks; CONFLICT(*RQSRLS); docs/fidelity-gaps.md.

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
/// Read from the allocations recorded in `as4tainr.object_locks` by job
/// (`owner`: the job, or `PID<n>` for a process without one), of
/// sessions still there: a job's own locks never conflict, whichever of
/// its sessions holds them (its connection, its RPG session), as on IBM i.
async fn conflicting(client: &Client, t: &Target, state: &str, owner: &str) -> Result<bool> {
    let states: Vec<String> = conflicts(state).iter().map(|s| s.to_string()).collect();
    if states.is_empty() {
        return Ok(false);
    }
    let n: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.object_locks o
               JOIN pg_stat_activity a ON a.pid = o.pid AND a.backend_start = o.started
              WHERE o.key = $1 AND o.state = ANY($2) AND o.owner <> $3",
            &[&t.key(), &states, &owner],
        )
        .await?
        .get(0);
    Ok(n > 0)
}

/// Does another job hold a lock on the object that `state` cannot be
/// held with? For commands that use an object without allocating it
/// themselves (CHGDTAARA).
pub async fn kept_out(client: &Client, t: &Target, state: &str, owner: &str) -> Result<bool> {
    conflicting(client, t, state, owner).await
}

/// Record an allocation (before checking for conflicts, so two jobs
/// asking at once see each other).
async fn record(client: &Client, t: &Target, state: &str, owner: &str, kind: &str) -> Result<i64> {
    Ok(client
        .query_one(
            &format!(
                "INSERT INTO as4tainr.object_locks (pid, started, key, state, owner, kind)
                 SELECT pid, backend_start, $1, $2, $3, $4 FROM {ME} m RETURNING seq"
            ),
            &[&t.key(), &state, &owner, &kind],
        )
        .await?
        .get(0))
}

async fn unrecord(client: &Client, seq: i64) -> Result<()> {
    client
        .execute("DELETE FROM as4tainr.object_locks WHERE seq = $1", &[&seq])
        .await?;
    Ok(())
}

/// A file opened by a program: *SHRRD on the file (IBM i's open takes a
/// shared read lock on the file, which keeps out another job's *EXCL;
/// ALCOBJ in the CL reference, lock states). Waits up to `wait` for a
/// conflicting lock to go; false when it did not.
pub async fn open_file(
    client: &Client,
    library: &str,
    file: &str,
    wait: Duration,
    owner: &str,
    update: bool,
) -> Result<bool> {
    let t = Target {
        library: library.to_string(),
        name: file.to_string(),
        objtype: "*FILE".into(),
    };
    if !update {
        // An open for input is not kept out by another job's ALCOBJ, even
        // *EXCL (PUB400 2026-11-05, tools/golden/verify48.txt q48_held:
        // the RPG program's OPEN of the file another job held *EXCL
        // succeeded, while a third job's ALCOBJ *SHRRD got CPF1002). Its
        // *SHRRD is recorded, so other jobs' ALCOBJ see it.
        take_shared(client, &t, "*SHRRD").await?;
        record(client, &t, "*SHRRD", owner, OPEN_READ).await?;
        return Ok(true);
    }
    // An open for update takes *SHRUPD (its member data lock, verify52
    // u52_open) and waits for conflicting locks.
    take(client, &t, "*SHRUPD", Some(wait), owner, false, OPEN_UPDATE).await
}

/// What a recorded lock is besides an allocation: an open for input or
/// for update (its member locks are listed by WRKOBJLCK).
const OPEN_READ: &str = "OPEN";
const OPEN_UPDATE: &str = "OPENUPD";

/// Take a lock: the session's shared advisory lock (it goes with the
/// session) and its recorded allocation; given back when another job's
/// allocation conflicts, tried again until `wait` has passed. `sweep`
/// clears ended sessions' rows first (ALCOBJ; opens are too frequent).
async fn take(
    client: &Client,
    t: &Target,
    state: &str,
    wait: Option<Duration>,
    owner: &str,
    sweep: bool,
    kind: &str,
) -> Result<bool> {
    if sweep {
        client
            .execute(
                "DELETE FROM as4tainr.object_locks o WHERE NOT EXISTS
                   (SELECT 1 FROM pg_stat_activity a WHERE a.pid = o.pid AND a.backend_start = o.started)",
                &[],
            )
            .await?;
    }
    let start = Instant::now();
    loop {
        take_shared(client, t, state).await?;
        let seq = record(client, t, state, owner, kind).await?;
        if !conflicting(client, t, state, owner).await? {
            return Ok(true);
        }
        unrecord(client, seq).await?;
        drop_shared(client, t, state).await?;
        if wait.is_some_and(|w| start.elapsed() >= w) {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The file closed: its open lock goes.
pub async fn close_file(client: &Client, library: &str, file: &str, update: bool) -> Result<()> {
    let t = Target {
        library: library.to_string(),
        name: file.to_string(),
        objtype: "*FILE".into(),
    };
    deallocate(client, &t, if update { "*SHRUPD" } else { "*SHRRD" }).await?;
    Ok(())
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
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='object_locks' AND column_name='owner') THEN
    ALTER TABLE as4tainr.object_locks ADD COLUMN IF NOT EXISTS owner text NOT NULL DEFAULT '';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='object_locks' AND column_name='kind') THEN
    ALTER TABLE as4tainr.object_locks ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT '';
  END IF;
END $$;
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
    owner: &str,
) -> Result<bool> {
    if !STATES.contains(&state) {
        bail!("Lock state {state} is not valid; use *EXCL, *EXCLRD, *SHRUPD, *SHRNUP or *SHRRD.");
    }
    take(client, t, state, wait, owner, true, "").await
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
    /// The lock of a file open: None for an allocation, else the open's
    /// member data lock (*SHRRD for input, *SHRUPD for update).
    pub open: Option<String>,
}

/// The allocations held on an object: by job (in the order the jobs
/// first allocated it), each job's in the order it made them.
pub async fn held(client: &Client, t: &Target) -> Result<Vec<Held>> {
    Ok(client
        .query(
            "SELECT COALESCE(NULLIF(o.owner, ''), a.application_name), o.state, o.kind
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
            // An open for update holds *SHRRD on the file and *SHRUPD on
            // the member's data (PUB400 2026-11-05,
            // tools/golden/verify52.txt u52_open); it is recorded as
            // *SHRUPD, which keeps out what the member lock does
            // (another job's ALCOBJ *SHRNUP and *EXCLRD got CPF1002,
            // *SHRRD and *SHRUPD were allocated).
            state: if r.get::<_, String>(2) == OPEN_UPDATE {
                "*SHRRD".into()
            } else {
                r.get(1)
            },
            open: match r.get::<_, String>(2).as_str() {
                OPEN_READ => Some("*SHRRD".into()),
                OPEN_UPDATE => Some("*SHRUPD".into()),
                _ => None,
            },
        })
        .collect())
}

/// A job's name, user and number from the name its connection carries
/// ("number/user/name"); another name is the job's name alone.
/// A `system` job (no as4tainr job; its key is "PID<n>") is QP0ZSPWT of
/// QUSER with the process's number, as IBM i's (PUB400 2026-11-05,
/// tools/golden/verify50.txt s50_open).
pub fn job_parts(job: &str) -> (String, String, String) {
    if let Some(n) = job.strip_prefix("PID").and_then(|n| n.parse::<u32>().ok()) {
        return (
            "QP0ZSPWT".into(),
            "QUSER".into(),
            format!("{:06}", n % 1_000_000),
        );
    }
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
/// A file's type shows its attribute (*FILE-PHY) and its opens are
/// listed again under "Member Locks", each a MBR and a DATA lock of the
/// file's member (PUB400 2026-11-05, tools/golden/verify50.txt s50_open:
/// a CL program's RCVF holds MBR *SHRRD and DATA *SHRRD); `member_type`
/// is PHY or LGL for a file.
pub fn print(
    t: &Target,
    library: &str,
    shown_type: &str,
    member_type: Option<&str>,
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
            (87, shown_type),
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
    let opens: Vec<&Held> = locks.iter().filter(|h| h.open.is_some()).collect();
    if let (Some(mt), false) = (member_type, opens.is_empty()) {
        out.push(at(&[(37, "Member Locks")]));
        out.push(at(&[
            (1, "File . . . . . :"),
            (20, &t.name),
            (35, "Library . . . . :"),
            (55, library),
            (70, "Type . . . . :"),
            (87, mt),
            (102, "ASP device . . :"),
            (121, "*SYSBAS"),
        ]));
        out.push(at(&[(46, "Lock")]));
        out.push(at(&[
            (4, "Member"),
            (15, "Job"),
            (26, "User"),
            (37, "Number"),
            (46, "Type"),
            (55, "Lock"),
            (62, "Status"),
            (80, "Scope"),
            (90, "Thread"),
        ]));
        let mut last = String::new();
        for (i, h) in opens.iter().enumerate() {
            let member = if i == 0 {
                t.name.clone()
            } else {
                String::new()
            };
            let (name, user, number) = if h.job == last {
                Default::default()
            } else {
                job_parts(&h.job)
            };
            last = h.job.clone();
            out.push(at(&[
                (4, &member),
                (15, &name),
                (26, &user),
                (37, &number),
                (46, "MBR"),
                (54, "*SHRRD"),
                (63, "HELD"),
                (80, "*JOB"),
            ]));
            out.push(at(&[
                (46, "DATA"),
                (54, h.open.as_deref().unwrap_or_default()),
                (63, "HELD"),
                (80, "*JOB"),
            ]));
        }
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

/// The tables an SQL statement names after FROM, JOIN, INTO, UPDATE,
/// MERGE INTO, USING and DELETE FROM: (library or None, name, whether the
/// statement changes it). Names are upper case unless quoted; `lib/name`
/// and `lib.name` both qualify. Used to find another job's object lock in
/// the way (PUB400 2026-11-06, tools/golden/verify56.txt).
pub fn sql_tables(stmt: &str) -> Vec<(Option<String>, String, bool)> {
    // Words and names outside string constants.
    let mut toks: Vec<String> = Vec::new();
    let cs: Vec<char> = stmt.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            i += 1;
            while i < cs.len() {
                if cs[i] == '\'' {
                    if cs.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            toks.push("'".into());
            continue;
        }
        if c.is_alphanumeric() || "_$#@\"/.".contains(c) {
            let mut w = String::new();
            let mut quoted = false;
            while i < cs.len() {
                let d = cs[i];
                if d == '"' {
                    quoted = !quoted;
                    w.push(d);
                } else if quoted || d.is_alphanumeric() || "_$#@/.".contains(d) {
                    w.push(d);
                } else {
                    break;
                }
                i += 1;
            }
            toks.push(w);
            continue;
        }
        if !c.is_whitespace() {
            toks.push(c.to_string());
        }
        i += 1;
    }
    let up: Vec<String> = toks.iter().map(|t| t.to_ascii_uppercase()).collect();
    let first = up.first().cloned().unwrap_or_default();
    let changes = matches!(first.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE");
    let name_of = |t: &str| -> Option<(Option<String>, String)> {
        let part = |p: &str| {
            if p.starts_with('"') {
                p.trim_matches('"').to_string()
            } else {
                p.to_ascii_uppercase()
            }
        };
        if t.is_empty()
            || !(t.starts_with('"')
                || t.chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || "$#@".contains(c)))
        {
            return None;
        }
        match t.split_once(['/', '.']) {
            Some((l, n)) if !n.is_empty() => Some((Some(part(l)), part(n))),
            _ => Some((None, part(t))),
        }
    };
    let mut out: Vec<(Option<String>, String, bool)> = Vec::new();
    let mut push = |n: (Option<String>, String), w: bool| {
        if let Some(e) = out.iter_mut().find(|e| e.0 == n.0 && e.1 == n.1) {
            e.2 |= w;
        } else {
            out.push((n.0, n.1, w));
        }
    };
    let mut k = 0;
    while k < up.len() {
        let target = match up[k].as_str() {
            // The table the statement changes.
            "INTO" | "UPDATE" if changes => true,
            "FROM" if first == "DELETE" && k == 1 => true,
            "FROM" | "JOIN" | "USING" => false,
            _ => {
                k += 1;
                continue;
            }
        };
        // FROM a, b: each name of the list, until a keyword or '('.
        let mut j = k + 1;
        loop {
            match up.get(j).map(String::as_str) {
                Some("(") | Some("LATERAL") | Some("TABLE") | None => break,
                Some(_) => {
                    let Some(n) = name_of(&toks[j]) else { break };
                    push(n, target);
                    j += 1;
                    // An alias (with or without AS).
                    if up.get(j).map(String::as_str) == Some("AS") {
                        j += 2;
                    } else if up.get(j).is_some_and(|t| {
                        !matches!(
                            t.as_str(),
                            "," | "WHERE"
                                | "JOIN"
                                | "INNER"
                                | "LEFT"
                                | "RIGHT"
                                | "FULL"
                                | "CROSS"
                                | "ON"
                                | "GROUP"
                                | "ORDER"
                                | "HAVING"
                                | "UNION"
                                | "EXCEPT"
                                | "INTERSECT"
                                | "FETCH"
                                | "LIMIT"
                                | "OFFSET"
                                | "SET"
                                | "VALUES"
                                | "SELECT"
                                | "WITH"
                                | ")"
                                | "("
                                | "FOR"
                                | "OVERRIDING"
                        )
                    }) {
                        j += 1;
                    }
                    if up.get(j).map(String::as_str) == Some(",") && !target {
                        j += 1;
                        continue;
                    }
                    break;
                }
            }
        }
        k = j.max(k + 1);
    }
    out
}

/// Wait up to `wait` for the tables an SQL statement reads (*SHRRD) or
/// changes (*SHRUPD) to be free of other jobs' conflicting locks; the
/// SQL0913 error when one stays locked (PUB400 2026-11-06,
/// tools/golden/verify56.txt, verify57.txt). Unqualified names are
/// looked for in the library list.
pub async fn sql_tables_free(
    client: &Client,
    stmt: &str,
    libl: &[String],
    owner: &str,
    wait: Duration,
) -> Result<()> {
    let tables = sql_tables(stmt);
    if tables.is_empty() {
        return Ok(());
    }
    // Most of the time no job holds any object lock: one query then.
    let any: bool = client
        .query_one("SELECT EXISTS (SELECT 1 FROM as4tainr.object_locks)", &[])
        .await?
        .get(0);
    if !any {
        return Ok(());
    }
    for (lib, name, write) in tables {
        let libs = match &lib {
            Some(l) => vec![crate::qtemp::real(l)],
            None => libl.to_vec(),
        };
        let mut found = None;
        for l in &libs {
            if crate::catalog::object_exists(client, std::slice::from_ref(l), &name, "*FILE")
                .await
                .unwrap_or(false)
            {
                found = Some(l.clone());
                break;
            }
        }
        let Some(l) = found else { continue };
        let t = Target {
            library: l.clone(),
            name: name.clone(),
            objtype: "*FILE".into(),
        };
        let state = if write { "*SHRUPD" } else { "*SHRRD" };
        let start = Instant::now();
        while conflicting(client, &t, state, owner).await? {
            if start.elapsed() >= wait {
                bail!(
                    "SQL0913: Table {name} in {} is in use by another job.",
                    crate::qtemp::logical(&l)
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}

/// Connections that hold, for a job, the locks a program left when it
/// returned with LR off (IN *LOCK of a data area stays locked while the
/// job goes on: PUB400 2026-11-06, tools/golden/verify59.txt b59_ret),
/// by job key.
static KEEPERS: std::sync::Mutex<
    Option<std::collections::HashMap<String, std::sync::Arc<Client>>>,
> = std::sync::Mutex::new(None);

/// Hold `state` on the object for the job on a connection of its own.
pub async fn keep_for_job(
    database_url: &str,
    job_key: &str,
    t: &Target,
    state: &str,
) -> Result<()> {
    let have = KEEPERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(job_key).cloned());
    let client = match have {
        Some(c) => c,
        None => {
            let c = std::sync::Arc::new(crate::runtime::connect_db(database_url).await?);
            KEEPERS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(Default::default)
                .insert(job_key.to_string(), c.clone());
            c
        }
    };
    take_shared(&client, t, state).await?;
    record(&client, t, state, job_key, "").await?;
    Ok(())
}

/// Give back a lock [`keep_for_job`] holds for the job (false: none).
pub async fn release_kept(job_key: &str, t: &Target, state: &str) -> Result<bool> {
    let have = KEEPERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(job_key).cloned());
    match have {
        Some(c) => deallocate(&c, t, state).await,
        None => Ok(false),
    }
}

/// A job's default wait time (DFTWAIT; 30 seconds when not changed, as
/// for a `system` job, DSPJOB).
pub fn default_wait(job_key: &str) -> Duration {
    match crate::jobattr::get(job_key, "DFTWAIT").as_deref() {
        Some("*NOMAX") => Duration::from_secs(u64::MAX / 4),
        Some(n) => Duration::from_secs(n.trim().parse::<u64>().unwrap_or(30)),
        None => Duration::from_secs(30),
    }
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
            open: None,
        };
        let lines = print(
            &t,
            "AS4TRAINR1",
            "*DTAARA",
            None,
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
        let none = print(&t, "AS4TRAINR1", "*DTAARA", None, &[], &stamp);
        assert_eq!(none.len(), 6);
        assert_eq!(none[3], want[3]);
        assert_eq!(none[5], want[7]);
    }

    /// A file a `system` job's CL program has open (PUB400 2026-11-05,
    /// tools/golden/verify50.txt s50_open: WRKOBJLCK's lines with *SHRRD,
    /// cut at 90 columns there; the job's number is the process's here).
    #[test]
    fn printed_locks_of_an_open_file() {
        let t = Target {
            library: "AS4TRAINR1".into(),
            name: "GLD50F".into(),
            objtype: "*FILE".into(),
        };
        let stamp = crate::report::Stamp {
            date: "26-11-05".into(),
            time: "15:42:07".into(),
            zone: String::new(),
        };
        let lines = print(
            &t,
            "AS4TRAINR1",
            "*FILE-PHY",
            Some("PHY"),
            &[Held {
                job: "PID1786218".into(),
                state: "*SHRRD".into(),
                open: Some("*SHRRD".into()),
            }],
            &stamp,
        );
        let want = [
            "Object . . . . :   GLD50F         Library . . . . :   AS4TRAINR1     Type . . . . :   *FILE-PHY      ASP device . . :   *SYSBAS",
            "                                    Object Locks",
            "   Job         User        Number    Lock    Status   Scope     Thread",
            "   QP0ZSPWT    QUSER       786218   *SHRRD    HELD    *JOB",
            "                                    Member Locks",
            "File . . . . . :   GLD50F         Library . . . . :   AS4TRAINR1     Type . . . . :   PHY            ASP device . . :   *SYSBAS",
            "                                             Lock",
            "   Member     Job        User       Number   Type     Lock   Status            Scope     Thread",
            "   GLD50F     QP0ZSPWT   QUSER      786218   MBR     *SHRRD   HELD             *JOB",
            "                                             DATA    *SHRRD   HELD             *JOB",
            "                        * * * * *   E N D   O F   L I S T I N G   * * * * *",
        ];
        assert_eq!(lines[1..], want);
    }

    /// The tables of the statements verify56 ran, and a join and a
    /// subquery.
    #[test]
    fn tables_an_sql_statement_names() {
        let t = |s: &str| sql_tables(s);
        let lib = |l: &str, n: &str, w: bool| (Some(l.to_string()), n.to_string(), w);
        assert_eq!(
            t("DELETE FROM AS4TRAINR1/GLD56F WHERE K = 1"),
            [lib("AS4TRAINR1", "GLD56F", true)]
        );
        assert_eq!(
            t("INSERT INTO AS4TRAINR1/GLD56F VALUES(4)"),
            [lib("AS4TRAINR1", "GLD56F", true)]
        );
        assert_eq!(
            t("UPDATE AS4TRAINR1/GLD56F SET K = 9 WHERE K = 2"),
            [lib("AS4TRAINR1", "GLD56F", true)]
        );
        assert_eq!(
            t("CREATE TABLE AS4TRAINR1/GLD56C AS (SELECT K FROM AS4TRAINR1/GLD56F) WITH DATA"),
            [lib("AS4TRAINR1", "GLD56F", false)]
        );
        assert_eq!(
            t("insert into a select x.k from lib.b x join c on x.k = c.k where x.k in (select k from \"d\")"),
            [
                (None, "A".to_string(), true),
                lib("LIB", "B", false),
                (None, "C".to_string(), false),
                (None, "d".to_string(), false)
            ]
        );
        assert_eq!(
            t("UPDATE t SET v = 'FROM x' WHERE k = 1"),
            [(None, "T".to_string(), true)]
        );
    }
}
