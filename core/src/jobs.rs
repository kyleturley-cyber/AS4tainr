//! Work management: jobs, the job log, job queues and user message queues.
//!
//! Written from IBM's public work management documentation. Every
//! interactive session and every submitted batch job is a job with a
//! qualified name `number/user/name` (`000123/QSECOFR/QPADEV0001`). Jobs
//! live in the catalog table `as4tainr.jobs`, so every process sees them:
//! WRKACTJOB in one session lists the others.
//!
//! - Interactive jobs run in subsystem QINTER, one per signed-on session.
//! - SBMJOB puts a batch job on job queue QBATCH; the QBATCH subsystem
//!   ([`run_subsystem`], started by the 5250 server) takes jobs off the
//!   queue in order, runs each command in a new job and keeps its job log.
//!   When it ends, the user who submitted it gets CPF1241 (completed
//!   normally) or CPF1240 (ended abnormally) on their message queue.
//! - Each job has a job log (`as4tainr.job_log`): the commands it ran and
//!   the messages they sent.
//! - Each user profile has a message queue (`as4tainr.messages`), for
//!   SNDMSG, DSPMSG and the job completion messages.

use anyhow::{anyhow, bail, Result};
use std::time::Duration;
use tokio_postgres::{Client, Row};

pub const DDL: &str = r#"
CREATE SEQUENCE IF NOT EXISTS as4tainr.job_number MINVALUE 1 MAXVALUE 999999;
CREATE TABLE IF NOT EXISTS as4tainr.jobs (
    number       bigint PRIMARY KEY,
    name         text NOT NULL,
    user_name    text NOT NULL,
    type         text NOT NULL,           -- INT or BCH
    subsystem    text NOT NULL,
    jobq         text NOT NULL DEFAULT '',
    status       text NOT NULL,           -- JOBQ, ACTIVE, OUTQ (ended)
    function     text NOT NULL DEFAULT '',
    activity     text NOT NULL DEFAULT '', -- RUN, DSPW
    command      text NOT NULL DEFAULT '',
    submitted_by text NOT NULL DEFAULT '',
    submitter    bigint,
    curlib       text,
    libl         text[] NOT NULL DEFAULT '{}',
    entered      timestamptz NOT NULL DEFAULT now(),
    started      timestamptz,
    ended        timestamptz,
    end_code     text NOT NULL DEFAULT ''  -- NORMAL, ABNORMAL
);
CREATE TABLE IF NOT EXISTS as4tainr.job_log (
    seq    bigserial PRIMARY KEY,
    job    bigint NOT NULL,
    sent   timestamptz NOT NULL DEFAULT now(),
    msgid  text NOT NULL DEFAULT '',
    kind   text NOT NULL,                 -- *RQS (request), *INFO, *COMP, *ESCAPE
    text   text NOT NULL
);
CREATE INDEX IF NOT EXISTS job_log_job ON as4tainr.job_log (job, seq);
CREATE TABLE IF NOT EXISTS as4tainr.messages (
    id        bigserial PRIMARY KEY,
    msgq      text NOT NULL,
    sent      timestamptz NOT NULL DEFAULT now(),
    from_user text NOT NULL,
    from_job  text NOT NULL DEFAULT '',
    msgid     text NOT NULL DEFAULT '',
    kind      text NOT NULL DEFAULT '*INFO',
    text      text NOT NULL,
    is_new    boolean NOT NULL DEFAULT true
);
CREATE INDEX IF NOT EXISTS messages_msgq ON as4tainr.messages (msgq, id);
"#;

/// The job queue and subsystem for batch jobs (as shipped: QBATCH).
pub const BATCH_QUEUE: &str = "QBATCH";
/// The subsystem interactive jobs run in.
pub const INTERACTIVE_SUBSYSTEM: &str = "QINTER";
/// SBMJOB JOB(*JOBD): the name of the default job description.
pub const DEFAULT_BATCH_JOB_NAME: &str = "QDFTJOBD";

/// A job's qualified name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobId {
    pub number: i64,
    pub user: String,
    pub name: String,
}

impl std::fmt::Display for JobId {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{:06}/{}/{}", self.number, self.user, self.name)
    }
}

/// One row of `as4tainr.jobs`.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: JobId,
    pub job_type: String,
    pub subsystem: String,
    pub jobq: String,
    pub status: String,
    pub function: String,
    pub activity: String,
    pub command: String,
    pub submitted_by: String,
    pub curlib: Option<String>,
    pub libl: Vec<String>,
    pub entered: String,
    pub end_code: String,
}

const JOB_COLS: &str = "number, user_name, name, type, subsystem, jobq, status, function, activity,
    command, submitted_by, curlib, libl, to_char(entered, 'MM/DD/YY HH24:MI:SS'), end_code";

fn job_row(r: &Row) -> JobInfo {
    JobInfo {
        id: JobId {
            number: r.get(0),
            user: r.get(1),
            name: r.get(2),
        },
        job_type: r.get(3),
        subsystem: r.get(4),
        jobq: r.get(5),
        status: r.get(6),
        function: r.get(7),
        activity: r.get(8),
        command: r.get(9),
        submitted_by: r.get(10),
        curlib: r.get(11),
        libl: r.get(12),
        entered: r.get(13),
        end_code: r.get(14),
    }
}

/// A job specification as commands take it: `number/user/name`,
/// `user/name` or `name` (`*` is the current job).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    pub number: Option<i64>,
    pub user: Option<String>,
    pub name: String,
}

pub fn parse_spec(s: &str) -> Result<JobSpec> {
    let parts: Vec<String> = s
        .trim()
        .split('/')
        .map(|p| p.trim().to_ascii_uppercase())
        .collect();
    let num = |p: &str| -> Result<i64> {
        if p.len() != 6 || !p.chars().all(|c| c.is_ascii_digit()) {
            bail!("Job number {p} is not valid; it has 6 digits.");
        }
        Ok(p.parse()?)
    };
    match parts.as_slice() {
        [n] if !n.is_empty() => Ok(JobSpec {
            number: None,
            user: None,
            name: n.clone(),
        }),
        [u, n] => Ok(JobSpec {
            number: None,
            user: Some(u.clone()),
            name: n.clone(),
        }),
        [nb, u, n] => Ok(JobSpec {
            number: Some(num(nb)?),
            user: Some(u.clone()),
            name: n.clone(),
        }),
        _ => bail!("Job name '{s}' is not valid; use number/user/name."),
    }
}

async fn next_number(client: &Client) -> Result<i64> {
    Ok(client
        .query_one("SELECT nextval('as4tainr.job_number')", &[])
        .await?
        .get(0))
}

/// A signed-on session starts an interactive job in QINTER.
pub async fn start_interactive(
    client: &Client,
    device: &str,
    user: &str,
    curlib: Option<&str>,
    libl: &[String],
) -> Result<JobId> {
    let number = next_number(client).await?;
    client
        .execute(
            "INSERT INTO as4tainr.jobs (number, name, user_name, type, subsystem, status, activity, curlib, libl, started)
             VALUES ($1, $2, $3, 'INT', $4, 'ACTIVE', 'DSPW', $5, $6, now())",
            &[&number, &device, &user, &INTERACTIVE_SUBSYSTEM, &curlib, &libl],
        )
        .await?;
    crate::dtaara::lda_start(client, number, None).await?;
    Ok(JobId {
        number,
        user: user.to_string(),
        name: device.to_string(),
    })
}

/// The job ends: interactive jobs at sign-off, batch jobs when their
/// command ends. Ended jobs keep their row and job log (status OUTQ).
pub async fn end(client: &Client, id: &JobId, normal: bool) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.jobs SET status='OUTQ', ended=now(), function='', activity='',
                 end_code=$2 WHERE number=$1",
            &[&id.number, &if normal { "NORMAL" } else { "ABNORMAL" }],
        )
        .await?;
    Ok(())
}

/// What the job is doing, as WRKACTJOB shows it: function (`CMD-DSPLIB`,
/// `PGM-ORDENT`) and activity (`RUN`, `DSPW` waiting for the display).
pub async fn set_function(client: &Client, id: &JobId, function: &str, activity: &str) {
    // Display only: a failure here must not fail the command.
    let _ = client
        .execute(
            "UPDATE as4tainr.jobs SET function=$2, activity=$3 WHERE number=$1",
            &[&id.number, &function, &activity],
        )
        .await;
}

/// The message ID at the start of a message text ("CPF9801: ..."), if any.
pub fn message_id(text: &str) -> &str {
    match text.split_once(':') {
        Some((id, _))
            if id.len() == 7
                && id[..3].chars().all(|c| c.is_ascii_uppercase())
                && id[3..].chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            id
        }
        _ => "",
    }
}

/// Add a message to a job's log.
pub async fn log(client: &Client, id: &JobId, kind: &str, text: &str) {
    let _ = client
        .execute(
            "INSERT INTO as4tainr.job_log (job, msgid, kind, text) VALUES ($1, $2, $3, $4)",
            &[&id.number, &message_id(text), &kind, &text],
        )
        .await;
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub sent: String,
    pub msgid: String,
    pub kind: String,
    pub text: String,
}

pub async fn job_log(client: &Client, number: i64) -> Result<Vec<LogEntry>> {
    Ok(client
        .query(
            "SELECT to_char(sent, 'HH24:MI:SS'), msgid, kind, text FROM as4tainr.job_log
             WHERE job=$1 ORDER BY seq",
            &[&number],
        )
        .await?
        .iter()
        .map(|r| LogEntry {
            sent: r.get(0),
            msgid: r.get(1),
            kind: r.get(2),
            text: r.get(3),
        })
        .collect())
}

/// SBMJOB's parameters, resolved.
#[derive(Debug, Clone)]
pub struct Submit {
    pub name: String,
    pub user: String,
    pub command: String,
    pub jobq: String,
    pub curlib: Option<String>,
    pub libl: Vec<String>,
    pub submitted_by: String,
    pub submitter: Option<i64>,
}

/// SBMJOB: put a batch job on a job queue.
pub async fn submit(client: &Client, s: &Submit) -> Result<JobId> {
    let number = next_number(client).await?;
    client
        .execute(
            "INSERT INTO as4tainr.jobs (number, name, user_name, type, subsystem, jobq, status, command,
                 submitted_by, submitter, curlib, libl)
             VALUES ($1, $2, $3, 'BCH', $4, $4, 'JOBQ', $5, $6, $7, $8, $9)",
            &[
                &number,
                &s.name,
                &s.user,
                &s.jobq,
                &s.command,
                &s.submitted_by,
                &s.submitter,
                &s.curlib,
                &s.libl,
            ],
        )
        .await?;
    // The submitted job's local data area: a copy of the submitter's.
    crate::dtaara::lda_start(client, number, s.submitter).await?;
    Ok(JobId {
        number,
        user: s.user.clone(),
        name: s.name.clone(),
    })
}

/// Take the next job off a job queue (first in, first out) and mark it
/// active. Safe with several subsystems polling the same queue.
pub async fn claim_next(client: &Client, jobq: &str) -> Result<Option<JobInfo>> {
    let row = client
        .query_opt(
            &format!(
                "UPDATE as4tainr.jobs SET status='ACTIVE', activity='RUN', started=now()
                 WHERE number = (SELECT number FROM as4tainr.jobs WHERE status='JOBQ' AND jobq=$1
                                 ORDER BY entered, number LIMIT 1 FOR UPDATE SKIP LOCKED)
                 RETURNING {JOB_COLS}"
            ),
            &[&jobq],
        )
        .await?;
    Ok(row.as_ref().map(job_row))
}

/// Take one particular job off its job queue, if it is still there.
pub async fn claim(client: &Client, number: i64) -> Result<Option<JobInfo>> {
    let row = client
        .query_opt(
            &format!(
                "UPDATE as4tainr.jobs SET status='ACTIVE', activity='RUN', started=now()
                 WHERE number=$1 AND status='JOBQ' RETURNING {JOB_COLS}"
            ),
            &[&number],
        )
        .await?;
    Ok(row.as_ref().map(job_row))
}

/// Jobs that are running (WRKACTJOB), interactive and batch.
pub async fn active(client: &Client) -> Result<Vec<JobInfo>> {
    Ok(client
        .query(
            &format!(
                "SELECT {JOB_COLS} FROM as4tainr.jobs WHERE status='ACTIVE' ORDER BY subsystem, name, number"
            ),
            &[],
        )
        .await?
        .iter()
        .map(job_row)
        .collect())
}

/// Jobs a user submitted (WRKSBMJOB), newest last.
pub async fn submitted_by(client: &Client, user: &str) -> Result<Vec<JobInfo>> {
    Ok(client
        .query(
            &format!(
                "SELECT {JOB_COLS} FROM as4tainr.jobs WHERE submitted_by=$1 AND type='BCH' ORDER BY number"
            ),
            &[&user],
        )
        .await?
        .iter()
        .map(job_row)
        .collect())
}

/// Find one job. Without a number, the newest job of that name (and user)
/// is taken, as on IBM i when the name is unique.
pub async fn find(client: &Client, spec: &JobSpec) -> Result<JobInfo> {
    let rows = client
        .query(
            &format!(
                "SELECT {JOB_COLS} FROM as4tainr.jobs
                 WHERE name=$1 AND ($2::text IS NULL OR user_name=$2) AND ($3::bigint IS NULL OR number=$3)
                 ORDER BY number DESC"
            ),
            &[&spec.name, &spec.user, &spec.number],
        )
        .await?;
    let shown = match (&spec.number, &spec.user) {
        (Some(n), Some(u)) => format!("{n:06}/{u}/{}", spec.name),
        (_, Some(u)) => format!("{u}/{}", spec.name),
        _ => spec.name.clone(),
    };
    match rows.len() {
        0 => bail!("Job {shown} not found."),
        1 => Ok(job_row(&rows[0])),
        _ if spec.number.is_some() => Ok(job_row(&rows[0])),
        n => bail!("{n} jobs are named {shown}; give the job number as well (number/user/name)."),
    }
}

pub async fn get(client: &Client, number: i64) -> Result<JobInfo> {
    client
        .query_opt(
            &format!("SELECT {JOB_COLS} FROM as4tainr.jobs WHERE number=$1"),
            &[&number],
        )
        .await?
        .map(|r| job_row(&r))
        .ok_or_else(|| anyhow!("Job number {number:06} not found."))
}

/// ENDJOB of a job still on its job queue: it is taken off and never runs.
pub async fn end_queued(client: &Client, id: &JobId) -> Result<bool> {
    let n = client
        .execute(
            "UPDATE as4tainr.jobs SET status='OUTQ', ended=now(), end_code='ABNORMAL'
             WHERE number=$1 AND status='JOBQ'",
            &[&id.number],
        )
        .await?;
    if n == 1 {
        log(
            client,
            id,
            "*COMP",
            "Job ended with ENDJOB before it started.",
        )
        .await;
    }
    Ok(n == 1)
}

/// Jobs left active by a server that stopped (crash, container restart)
/// are no longer running: mark them ended abnormally.
pub async fn recover(client: &Client) -> Result<u64> {
    let ended = client
        .query(
            "UPDATE as4tainr.jobs SET status='OUTQ', ended=now(), function='', activity='', end_code='ABNORMAL'
             WHERE status='ACTIVE' RETURNING number",
            &[],
        )
        .await?;
    for r in &ended {
        let n: i64 = r.get(0);
        let _ = client
            .execute(
                "INSERT INTO as4tainr.job_log (job, kind, text) VALUES ($1, '*INFO', $2)",
                &[
                    &n,
                    &"The job was still active when the system stopped; it was ended.",
                ],
            )
            .await;
    }
    Ok(ended.len() as u64)
}

// ------------------------------------------------------------ messages

#[derive(Debug, Clone)]
pub struct Message {
    pub id: i64,
    pub sent: String,
    pub from_user: String,
    pub from_job: String,
    pub msgid: String,
    pub kind: String,
    pub text: String,
    pub is_new: bool,
}

/// Send a message to a user's message queue.
pub async fn send_message(
    client: &Client,
    to_user: &str,
    from_user: &str,
    from_job: &str,
    kind: &str,
    text: &str,
) -> Result<()> {
    client
        .execute(
            "INSERT INTO as4tainr.messages (msgq, from_user, from_job, msgid, kind, text)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &to_user,
                &from_user,
                &from_job,
                &message_id(text),
                &kind,
                &text,
            ],
        )
        .await?;
    Ok(())
}

/// The messages on a user's queue, oldest first. With `mark_old`, new
/// messages become old (DSPMSG shows them).
pub async fn messages(client: &Client, user: &str, mark_old: bool) -> Result<Vec<Message>> {
    let rows = client
        .query(
            "SELECT id, to_char(sent, 'MM/DD/YY HH24:MI:SS'), from_user, from_job, msgid, kind, text, is_new
             FROM as4tainr.messages WHERE msgq=$1 ORDER BY id",
            &[&user],
        )
        .await?;
    if mark_old {
        client
            .execute(
                "UPDATE as4tainr.messages SET is_new=false WHERE msgq=$1 AND is_new",
                &[&user],
            )
            .await?;
    }
    Ok(rows
        .iter()
        .map(|r| Message {
            id: r.get(0),
            sent: r.get(1),
            from_user: r.get(2),
            from_job: r.get(3),
            msgid: r.get(4),
            kind: r.get(5),
            text: r.get(6),
            is_new: r.get(7),
        })
        .collect())
}

/// Number of new messages waiting on a user's queue.
pub async fn new_messages(client: &Client, user: &str) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT count(*) FROM as4tainr.messages WHERE msgq=$1 AND is_new",
            &[&user],
        )
        .await?
        .get(0))
}

/// Remove old messages (DSPMSG F16 / CLRMSGQ CLEAR(*OLD) style).
pub async fn remove_messages(client: &Client, user: &str, all: bool) -> Result<u64> {
    Ok(client
        .execute(
            "DELETE FROM as4tainr.messages WHERE msgq=$1 AND ($2 OR NOT is_new)",
            &[&user, &all],
        )
        .await?)
}

// ------------------------------------------------------------ subsystem

/// How many batch jobs QBATCH runs at once (its job queue entry's
/// MAXACT; IBM ships QBATCH with 1).
pub fn batch_max_active() -> usize {
    std::env::var("AS4TAINR_QBATCH_MAXACT")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
}

/// Run one batch job to its end: its command in a new job with the
/// submitter's library list, the messages in its job log, and the
/// completion message to the submitting user's message queue.
pub async fn run_batch_job(env: &crate::commands::Environment, info: &JobInfo) -> Result<bool> {
    let client = crate::runtime::connect_db(&env.database_url).await?;
    let id = info.id.clone();
    log(
        &client,
        &id,
        "*INFO",
        &format!("Job {id} started in subsystem {}.", info.subsystem),
    )
    .await;
    log(&client, &id, "*RQS", &info.command).await;
    let mut job = crate::commands::Job {
        client: &client,
        env,
        user: id.user.clone(),
        curlib: info.curlib.clone(),
        libl: info.libl.clone(),
        display: None,
        id: Some(id.clone()),
    };
    let first = info
        .command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    set_function(&client, &id, &format!("CMD-{first}"), "RUN").await;
    let normal = match job.run(&info.command).await {
        Ok(crate::commands::Outcome::Messages(m)) => {
            for line in m {
                log(
                    &client,
                    &id,
                    if message_id(&line).is_empty() {
                        "*INFO"
                    } else {
                        "*COMP"
                    },
                    &line,
                )
                .await;
            }
            true
        }
        Ok(crate::commands::Outcome::Display(d)) => {
            // A display command in batch prints what it would show, as on
            // IBM i (spooled file QSYSPRT).
            let mut lines = vec![d.title.clone(), String::new()];
            lines.extend(d.info.iter().map(|(k, v)| format!("{k}: {v}")));
            if !d.info.is_empty() {
                lines.push(String::new());
            }
            lines.push(d.heading.clone());
            lines.extend(d.rows.iter().cloned());
            crate::spool::print_lines(&client, &id, "QSYSPRT", first.as_str(), &lines).await?;
            log(
                &client,
                &id,
                "*INFO",
                &format!("{} printed to spooled file QSYSPRT.", d.title),
            )
            .await;
            true
        }
        Ok(_) => {
            log(
                &client,
                &id,
                "*INFO",
                "That command is only for interactive jobs.",
            )
            .await;
            false
        }
        Err(e) => {
            for line in format!("{e:#}").lines() {
                log(&client, &id, "*ESCAPE", line).await;
            }
            false
        }
    };
    let (msg, kind) = if normal {
        (format!("CPF1241: Job {id} completed normally."), "*COMP")
    } else {
        (format!("CPF1240: Job {id} ended abnormally."), "*ESCAPE")
    };
    log(&client, &id, kind, &msg).await;
    if !normal {
        // QDFTJOBD's LOG(4 0 *NOLIST): the job log is kept as a spooled
        // file only when the job ends abnormally.
        let lines = crate::spool::job_log_lines(&client, &id).await?;
        crate::spool::print_lines(&client, &id, "QPJOBLOG", &id.name, &lines).await?;
    }
    end(&client, &id, normal).await?;
    if !info.submitted_by.is_empty() {
        send_message(
            &client,
            &info.submitted_by,
            "QSYS",
            &id.to_string(),
            "*INFO",
            &msg,
        )
        .await?;
    }
    Ok(normal)
}

/// The QBATCH subsystem: take jobs off job queue QBATCH and run them, at
/// most `max_active` at a time, until the task is dropped.
pub async fn run_subsystem(env: crate::commands::Environment, max_active: usize) -> Result<()> {
    let client = crate::runtime::connect_db(&env.database_url).await?;
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(max_active));
    let env = std::sync::Arc::new(env);
    loop {
        let permit = slots.clone().acquire_owned().await?;
        match claim_next(&client, BATCH_QUEUE).await {
            Ok(Some(info)) => {
                let env = env.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_batch_job(&env, &info).await {
                        eprintln!("as4tainr: QBATCH: job {}: {e:#}", info.id);
                    }
                    drop(permit);
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                drop(permit);
                eprintln!("as4tainr: QBATCH: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names() {
        let id = JobId {
            number: 123,
            user: "QSECOFR".into(),
            name: "QPADEV0001".into(),
        };
        // Qualified job names are number/user/name with a 6-digit number.
        assert_eq!(id.to_string(), "000123/QSECOFR/QPADEV0001");
        assert_eq!(
            parse_spec("000123/qsecofr/qpadev0001").unwrap(),
            JobSpec {
                number: Some(123),
                user: Some("QSECOFR".into()),
                name: "QPADEV0001".into()
            }
        );
        assert_eq!(parse_spec("myjob").unwrap().name, "MYJOB");
        assert!(parse_spec("123/U/N").is_err());
        assert!(parse_spec("a/b/c/d").is_err());
    }

    #[test]
    fn message_ids() {
        assert_eq!(message_id("CPF9801: Object not found."), "CPF9801");
        assert_eq!(message_id("CPF1241: Job ..."), "CPF1241");
        assert_eq!(message_id("Library MYLIB created."), "");
        assert_eq!(message_id("Note: x"), "");
    }
}
