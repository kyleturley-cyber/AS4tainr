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
-- The last spooled file number the job gave (numbers are not reused
-- within a job when files are deleted).
ALTER TABLE as4tainr.jobs ADD COLUMN IF NOT EXISTS last_splf integer NOT NULL DEFAULT 0;
-- ENDSBS and STRSBS: whether a subsystem starts jobs (QBATCH).
CREATE TABLE IF NOT EXISTS as4tainr.subsystems (
    name   text PRIMARY KEY,
    active boolean NOT NULL DEFAULT true
);
-- HLDJOBQ: a job queue the subsystem takes no jobs from until RLSJOBQ
-- (by the queue's name, as jobs carry it).
CREATE TABLE IF NOT EXISTS as4tainr.held_job_queues (name text PRIMARY KEY);
-- HLDJOB of a job on a job queue: the subsystem passes it by until RLSJOB.
ALTER TABLE as4tainr.jobs ADD COLUMN IF NOT EXISTS held boolean NOT NULL DEFAULT false;
CREATE TABLE IF NOT EXISTS as4tainr.job_log (
    seq    bigserial PRIMARY KEY,
    job    bigint NOT NULL,
    sent   timestamptz NOT NULL DEFAULT now(),
    msgid  text NOT NULL DEFAULT '',
    kind   text NOT NULL,                 -- *RQS (request), *INFO, *COMP, *ESCAPE
    text   text NOT NULL
);
CREATE INDEX IF NOT EXISTS job_log_job ON as4tainr.job_log (job, seq);
-- CHGMSGQ's DLVRY and SEV of a message queue (key as msgq_key gives it).
CREATE TABLE IF NOT EXISTS as4tainr.message_queue_attributes (
    msgq  text NOT NULL,
    attr  text NOT NULL,
    value text NOT NULL,
    PRIMARY KEY (msgq, attr)
);
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

impl JobId {
    /// A qualified job name NUMBER/USER/NAME.
    pub fn parse(text: &str) -> Option<JobId> {
        let mut p = text.trim().splitn(3, '/');
        let number = p.next()?.trim().parse().ok()?;
        let user = p.next()?.trim().to_string();
        let name = p.next()?.trim().to_string();
        Some(JobId { number, user, name })
    }
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

/// A server job for a client (QP0ZSPWT for a program started over SSH,
/// in QUSRWRK, running under QUSER) starts serving `user`.
pub async fn start_server_job(
    client: &Client,
    name: &str,
    user: &str,
    curlib: Option<&str>,
    libl: &[String],
) -> Result<JobId> {
    let number = next_number(client).await?;
    client
        .execute(
            "INSERT INTO as4tainr.jobs (number, name, user_name, type, subsystem, status, activity, curlib, libl, started)
             VALUES ($1, $2, 'QUSER', 'BCI', 'QUSRWRK', 'ACTIVE', 'RUN', $3, $4, now())",
            &[&number, &name, &curlib, &libl],
        )
        .await?;
    crate::dtaara::lda_start(client, number, None).await?;
    let _ = user;
    Ok(JobId {
        number,
        user: "QUSER".to_string(),
        name: name.to_string(),
    })
}

/// The job ends: interactive jobs at sign-off, batch jobs when their
/// command ends. Ended jobs keep their row and job log (status OUTQ).
pub async fn end(client: &Client, id: &JobId, normal: bool) -> Result<()> {
    // Its commitment definitions end: what is pending is rolled back.
    crate::cmtctl::end_job(&id.to_string()).await;
    // Its activation groups and program message queues end.
    crate::ile::end_activations(&id.to_string(), &crate::ile::Groups::All);
    crate::rpg::interp::close_ended_spools(client).await;
    crate::pgmmsg::named_end_job(&id.to_string());
    crate::envvar::end_job(&id.to_string());
    // Its open stream files are closed.
    for (path, data) in crate::rpg::interp::crt::end_job(&id.to_string()) {
        let _ = crate::ifs::write_file(client, &path, &data, 37, 0o644, "QSECOFR").await;
    }
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
            // A letter, two letters or digits (SQL messages like SQ20524:
            // PUB400 2026-10-27, verify3 x_errors), four hexadecimal digits.
            if id.len() == 7
                && id.as_bytes()[0].is_ascii_uppercase()
                && id[1..3]
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && id[3..].chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            id
        }
        _ => "",
    }
}

/// A message's text without the message ID before it.
pub fn message_text(text: &str) -> &str {
    if message_id(text).is_empty() {
        text
    } else {
        text[8..].trim_start()
    }
}

/// The escape message's ID in an error's text: the messages of a failed
/// command are its diagnostics, one per line, then the escape message
/// (e.g. "CPD0030: ...\nCPF0006: ..."), so the last line with an ID.
pub fn escape_id(text: &str) -> &str {
    text.lines()
        .rev()
        .map(message_id)
        .find(|id| !id.is_empty())
        .unwrap_or("")
}

/// The message a display shows for an error: a command the command
/// analyzer refused shows its first diagnostic (CPDxxxx), which says why;
/// otherwise the escape message (the last line).
pub fn display_message(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if escape_id(text) == "CPF0006" {
        if let Some(d) = lines.iter().find(|l| message_id(l).starts_with("CPD")) {
            return d.to_string();
        }
    }
    lines.last().map(|l| l.to_string()).unwrap_or_default()
}

/// A message as a 5250 display shows it: the text without its message ID.
/// Command Entry's log and the message line of IBM i 7.5 show texts only
/// (observed on PUB400: "Parameter TYPE required." on line 24 of a command
/// prompt, the messages under each command on Command Entry).
pub fn shown_text(line: &str) -> &str {
    let id = message_id(line);
    if id.is_empty() {
        return line;
    }
    line[id.len()..]
        .strip_prefix(':')
        .map(str::trim_start)
        .unwrap_or(line)
}

/// [`display_message`] without the message ID, for 5250 displays.
pub fn display_text(text: &str) -> String {
    shown_text(&display_message(text)).to_string()
}

/// Add a message to a job's log.
pub async fn log(client: &Client, id: &JobId, kind: &str, text: &str) {
    // A command's messages: CPCxxxx are completion messages, CPDxxxx
    // diagnostics (IBM i message ID prefixes).
    let msgid = message_id(text);
    let kind = match (kind, &msgid.get(..3)) {
        ("*INFO", Some("CPC")) => "*COMP",
        ("*INFO" | "*ESCAPE", Some("CPD")) => "*DIAG",
        _ => kind,
    };
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
                 WHERE number = (SELECT number FROM as4tainr.jobs WHERE status='JOBQ' AND jobq=$1 AND NOT held
                                   AND NOT EXISTS (SELECT 1 FROM as4tainr.held_job_queues h WHERE h.name = $1)
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

/// A user's jobs (WRKUSRJOB): all of them, or those with one status
/// (JOBQ, ACTIVE, OUTQ), oldest first.
pub async fn of_user(client: &Client, user: &str, status: Option<&str>) -> Result<Vec<JobInfo>> {
    Ok(client
        .query(
            &format!(
                "SELECT {JOB_COLS} FROM as4tainr.jobs
                 WHERE user_name=$1 AND ($2::text IS NULL OR status=$2) ORDER BY number"
            ),
            &[&user, &status],
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
        // CPF3342 (PUB400 2026-10-26, tools/golden/spool-job.txt j_joblog:
        // CPYSPLF of a job that is gone).
        0 => bail!("CPF3342: There is no job {shown}."),
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

/// HLDJOB (true) or RLSJOB (false) of a job on its job queue: a held job
/// stays there, passed by, until it is released. False when the job is
/// not on a job queue (any more).
pub async fn set_held(client: &Client, number: i64, held: bool) -> Result<bool> {
    Ok(client
        .execute(
            "UPDATE as4tainr.jobs SET held=$2 WHERE number=$1 AND status='JOBQ'",
            &[&number, &held],
        )
        .await?
        == 1)
}

/// Whether a job is held on its job queue.
pub async fn is_held(client: &Client, number: i64) -> Result<bool> {
    Ok(client
        .query_opt("SELECT held FROM as4tainr.jobs WHERE number=$1", &[&number])
        .await?
        .map(|r| r.get(0))
        .unwrap_or(false))
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

/// RCVMSG from a message queue: the oldest new message (`msgtype` *ANY,
/// *FIRST... or a type such as *INFO), removed (`rmv`) or kept as an old
/// message.
pub async fn receive_message(
    client: &Client,
    msgq: &str,
    msgtype: &str,
    rmv: bool,
) -> Result<Option<Message>> {
    let (newest, kinds): (bool, Vec<String>) = match msgtype {
        "*ANY" | "*NEXT" | "*FIRST" => (false, vec![]),
        "*LAST" => (true, vec![]),
        t => (false, vec![t.to_string()]),
    };
    let row = client
        .query_opt(
            &format!(
                "SELECT id, to_char(sent, 'MM/DD/YY HH24:MI:SS'), from_user, from_job, msgid, kind, text, is_new
                 FROM as4tainr.messages
                 WHERE msgq=$1 AND (is_new OR $2) AND (cardinality($3::text[]) = 0 OR kind = ANY($3))
                 ORDER BY id {} LIMIT 1 FOR UPDATE SKIP LOCKED",
                if newest { "DESC" } else { "ASC" }
            ),
            &[&msgq, &(msgtype == "*FIRST" || msgtype == "*LAST"), &kinds],
        )
        .await?;
    let Some(r) = row else { return Ok(None) };
    let m = Message {
        id: r.get(0),
        sent: r.get(1),
        from_user: r.get(2),
        from_job: r.get(3),
        msgid: r.get(4),
        kind: r.get(5),
        text: r.get(6),
        is_new: r.get(7),
    };
    if rmv {
        client
            .execute("DELETE FROM as4tainr.messages WHERE id=$1", &[&m.id])
            .await?;
    } else {
        client
            .execute(
                "UPDATE as4tainr.messages SET is_new=false WHERE id=$1",
                &[&m.id],
            )
            .await?;
    }
    Ok(Some(m))
}

/// The key a message queue's messages are kept under: a queue made with
/// CRTMSGQ by "LIB/NAME" (the library's real name, so QTEMP is the
/// job's), a user profile's queue (QUSRSYS/user, QSYS/QSYSOPR) by the
/// user's name. `libs`: the libraries to look in, in order (real
/// names). None: no such queue.
pub async fn msgq_key(client: &Client, libs: &[String], name: &str) -> Result<Option<String>> {
    let name = name.trim().to_ascii_uppercase();
    for l in libs {
        let hit = client
            .query_opt(
                "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*MSGQ'",
                &[l, &name],
            )
            .await?
            .is_some();
        if hit {
            return Ok(Some(format!("{l}/{name}")));
        }
    }
    if libs.iter().any(|l| l == "QUSRSYS" || l == "QSYS")
        && (name == "QSYSOPR" || crate::users::get(client, &name).await?.is_some())
    {
        return Ok(Some(name));
    }
    Ok(None)
}

/// A message queue's messages for the list APIs (QGYOLMSG), oldest
/// first, with the moment each was sent in microseconds since 1970.
pub async fn queue_messages(client: &Client, key: &str) -> Result<Vec<(i64, Message)>> {
    let rows = client
        .query(
            "SELECT id, (extract(epoch FROM sent) * 1000000)::bigint, from_user, from_job,
                    msgid, kind, text, is_new
             FROM as4tainr.messages WHERE msgq=$1 ORDER BY id",
            &[&key],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<_, i64>(1),
                Message {
                    id: r.get(0),
                    sent: String::new(),
                    from_user: r.get(2),
                    from_job: r.get(3),
                    msgid: r.get(4),
                    kind: r.get(5),
                    text: r.get(6),
                    is_new: r.get(7),
                },
            )
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
        calls: Default::default(),
    };
    job.calls.batch = true;
    let first = info
        .command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    set_function(&client, &id, &format!("CMD-{first}"), "RUN").await;
    // The job's own QTEMP, deleted when it ends.
    let key = id.to_string();
    let user = job.user.clone();
    let outcome = crate::qtemp::scope(&client, &key, &user, job.run(&info.command)).await;
    // What the job's programs left on their message queues is in its job
    // log too, in the order sent (CL programs' SNDPGMMSG to *SAME, the
    // commands they ran): IUNIT's I_RUNALL logs each test that way. The
    // request's own messages that are among them are not logged twice.
    job.flush_log().await;
    let mut already: Vec<String> = std::mem::take(&mut job.calls.flushed_lines);
    let mut first_time = |line: &str| match already.iter().position(|a| a == line.trim_end()) {
        Some(i) => {
            already.remove(i);
            false
        }
        None => true,
    };
    let normal = match outcome {
        Ok(crate::commands::Outcome::Messages(m)) => {
            for line in m.into_iter().filter(|l| first_time(l)) {
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
            // Commands with IBM's report layout print it (report.rs).
            let lines = printed_display(&d);
            // The command's own printer file (report.rs printer_file;
            // verify9 d9_prt_rows).
            let file = crate::report::printer_file(first.as_str());
            crate::spool::print_lines(&client, &id, file, first.as_str(), &lines).await?;
            if let Some(m) = &d.print_msg {
                log(&client, &id, "*COMP", m).await;
            }
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
            for line in format!("{e:#}").lines().filter(|l| first_time(l)) {
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
        let lines = crate::spool::job_log_lines(&client, &id, crate::joblog::Title::Ended).await?;
        crate::spool::print_report(&client, &id, "QPJOBLOG", &id.name, &lines).await?;
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
    set_subsystem_active(&client, BATCH_QUEUE, true).await?;
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(max_active));
    let env = std::sync::Arc::new(env);
    loop {
        // PWRDWNSYS *CNTRLD or ENDSBS QBATCH: no new jobs start; those
        // running end.
        if ENDING.load(std::sync::atomic::Ordering::Relaxed)
            || !subsystem_active(&client, BATCH_QUEUE).await.unwrap_or(true)
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        // Job schedule entries that are due put their jobs on their
        // queues (jobscde.rs).
        if let Err(e) = crate::jobscde::submit_due(&client).await {
            eprintln!("as4tainr: job schedule: {e:#}");
        }
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

/// Is a job queue held (HLDJOBQ)?
pub async fn job_queue_held(client: &Client, name: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM as4tainr.held_job_queues WHERE name = $1",
            &[&name],
        )
        .await?
        .is_some())
}

/// HLDJOBQ (true) and RLSJOBQ (false); false when it already was so.
pub async fn set_job_queue_held(client: &Client, name: &str, held: bool) -> Result<bool> {
    let n = if held {
        client
            .execute(
                "INSERT INTO as4tainr.held_job_queues VALUES ($1) ON CONFLICT DO NOTHING",
                &[&name],
            )
            .await?
    } else {
        client
            .execute(
                "DELETE FROM as4tainr.held_job_queues WHERE name = $1",
                &[&name],
            )
            .await?
    };
    Ok(n == 1)
}

/// Is a subsystem active (not ended with ENDSBS)?
pub async fn subsystem_active(client: &Client, name: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT active FROM as4tainr.subsystems WHERE name = $1",
            &[&name],
        )
        .await?
        .map(|r| r.get(0))
        .unwrap_or(true))
}

/// ENDSBS (false) and STRSBS (true) of a subsystem; the server's start
/// starts QBATCH again (as an IPL starts the subsystems).
pub async fn set_subsystem_active(client: &Client, name: &str, active: bool) -> Result<()> {
    client
        .execute(
            "INSERT INTO as4tainr.subsystems VALUES ($1, $2) ON CONFLICT (name) DO UPDATE SET active = EXCLUDED.active",
            &[&name, &active],
        )
        .await?;
    Ok(())
}

/// The system is ending (PWRDWNSYS): the subsystems start no more jobs.
pub static ENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The notification channel PWRDWNSYS sends on, and the application name
/// of the server's session that listens to it.
pub const POWER_DOWN_CHANNEL: &str = "as4tainr_pwrdwnsys";
pub const POWER_DOWN_LISTENER: &str = "as4tainr power-down listener";

/// What PWRDWNSYS asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerDown {
    /// OPTION(*IMMED): end at once; *CNTRLD: let batch jobs finish first.
    pub immediate: bool,
    /// DELAY for *CNTRLD, in seconds (None: *NOLIMIT).
    pub delay: Option<u64>,
    /// RESTART(*YES): start again (the server runs itself anew).
    pub restart: bool,
}

impl PowerDown {
    pub fn payload(&self) -> String {
        format!(
            "{} {} {}",
            if self.immediate { "*IMMED" } else { "*CNTRLD" },
            self.delay
                .map(|d| d.to_string())
                .unwrap_or_else(|| "*NOLIMIT".into()),
            if self.restart { "*YES" } else { "*NO" }
        )
    }

    pub fn parse(s: &str) -> Option<PowerDown> {
        let mut w = s.split_whitespace();
        let immediate = w.next()? == "*IMMED";
        let delay = match w.next()? {
            "*NOLIMIT" => None,
            d => Some(d.parse().ok()?),
        };
        let restart = w.next()? == "*YES";
        Some(PowerDown {
            immediate,
            delay,
            restart,
        })
    }
}

/// The notification channel SNDBRKMSG sends on.
pub const BREAK_CHANNEL: &str = "as4tainr_brkmsg";

/// A break message (SNDBRKMSG): shown at once on the display sessions it
/// is for, over what they show.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BreakMessage {
    /// *ALLWS, or the display devices' names.
    pub to: Vec<String>,
    pub from: String,
    pub sent: String,
    pub text: String,
}

impl BreakMessage {
    /// Is it for the display device `device`?
    pub fn is_for(&self, device: &str) -> bool {
        self.to
            .iter()
            .any(|t| t == "*ALLWS" || t.eq_ignore_ascii_case(device))
    }
}

static BREAKS: std::sync::OnceLock<tokio::sync::broadcast::Sender<BreakMessage>> =
    std::sync::OnceLock::new();

fn breaks() -> &'static tokio::sync::broadcast::Sender<BreakMessage> {
    BREAKS.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

/// A display session's receiver of break messages.
pub fn break_receiver() -> tokio::sync::broadcast::Receiver<BreakMessage> {
    breaks().subscribe()
}

/// Give a break message to this process's display sessions.
pub fn deliver_break(m: BreakMessage) {
    let _ = breaks().send(m);
}

/// The server waits here for PWRDWNSYS (a PostgreSQL notification, so a
/// PWRDWNSYS from any process of the system reaches it); break messages
/// (SNDBRKMSG) on the same connection go to the display sessions.
pub async fn wait_for_power_down(database_url: &str) -> Result<PowerDown> {
    use futures_util::StreamExt;
    let (client, mut conn) = tokio_postgres::connect(database_url, tokio_postgres::NoTls).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut stream = futures_util::stream::poll_fn(move |cx| conn.poll_message(cx));
        while let Some(m) = stream.next().await {
            match m {
                Ok(tokio_postgres::AsyncMessage::Notification(n)) => {
                    if n.channel() == BREAK_CHANNEL {
                        if let Ok(m) = serde_json::from_str::<BreakMessage>(n.payload()) {
                            deliver_break(m);
                        }
                    } else {
                        let _ = tx.send(n.payload().to_string());
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    client
        .batch_execute(&format!(
            "SET application_name = '{POWER_DOWN_LISTENER}'; LISTEN {POWER_DOWN_CHANNEL}; LISTEN {BREAK_CHANNEL}"
        ))
        .await?;
    loop {
        let Some(p) = rx.recv().await else {
            bail!("the power-down listener lost its database connection");
        };
        if let Some(pd) = PowerDown::parse(&p) {
            // Keep the session (and its LISTEN) until the end.
            std::mem::forget(client);
            return Ok(pd);
        }
    }
}

/// Is a server listening for PWRDWNSYS?
pub async fn power_down_listeners(client: &Client) -> Result<i64> {
    Ok(client
        .query_one(
            "SELECT count(*) FROM pg_stat_activity WHERE application_name = $1 AND datname = current_database()",
            &[&POWER_DOWN_LISTENER],
        )
        .await?
        .get(0))
}

/// PWRDWNSYS *CNTRLD: wait for the active batch jobs to end, up to the
/// delay; true when they all ended.
pub async fn wait_for_batch_jobs(client: &Client, delay: Option<u64>) -> Result<bool> {
    let start = std::time::Instant::now();
    loop {
        let n: i64 = client
            .query_one(
                "SELECT count(*) FROM as4tainr.jobs WHERE status='ACTIVE' AND type='BCH'",
                &[],
            )
            .await?
            .get(0);
        if n == 0 {
            return Ok(true);
        }
        if delay.is_some_and(|d| start.elapsed().as_secs() >= d) {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// What a display command prints in a batch job instead of showing it:
/// its report in IBM's layout when it has one, otherwise the display's
/// title, information and rows.
pub(crate) fn printed_display(d: &crate::commands::ListDisplay) -> Vec<String> {
    if !d.print.is_empty() {
        return d.print.clone();
    }
    let mut lines = vec![d.title.clone(), String::new()];
    lines.extend(d.info.iter().map(|(k, v)| format!("{k}: {v}")));
    if !d.info.is_empty() {
        lines.push(String::new());
    }
    lines.push(d.heading.clone());
    lines.extend(d.rows.iter().cloned());
    lines
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
        assert_eq!(
            escape_id("CPD0030: Command X not found.\nCPF0006: Not run."),
            "CPF0006"
        );
        assert_eq!(escape_id("CPF5026: Duplicate key.\nmore text"), "CPF5026");
        assert_eq!(escape_id("plain"), "");
    }
}
