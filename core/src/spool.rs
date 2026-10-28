//! Spooled files: printed output kept for the user to display, print or
//! delete (WRKSPLF, DSPSPLF, DLTSPLF).
//!
//! Written from IBM's public documentation of spooled files and output
//! queues. A spooled file belongs to the job that created it and to that
//! job's user; it has a file name (the printer file: QSYSPRT, QPJOBLOG,
//! ...), a number within the job, user data (for an RPG program, the
//! program name), an output queue (QPRINT) and pages of lines.
//!
//! Sources of spooled files:
//! - RPG programs printing through program-described printer files
//!   (O specs and EXCEPT; the runner lays out the pages, see
//!   docs/spooled-files.md),
//! - display commands run in a batch job (their list is printed to
//!   QSYSPRT instead of shown),
//! - job logs: `SIGNOFF LOG(*LIST)`, `DSPJOBLOG OUTPUT(*PRINT)`, and a
//!   batch job that ends abnormally (QDFTJOBD's LOG(4 0 *NOLIST) keeps the
//!   job log only then).

use anyhow::{anyhow, Result};
use tokio_postgres::Client;

use crate::jobs::JobId;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.spooled_files (
    id        bigserial PRIMARY KEY,
    job       bigint NOT NULL,
    job_name  text NOT NULL,          -- number/user/name
    user_name text NOT NULL,
    file      text NOT NULL,
    number    int NOT NULL,           -- spooled file number within the job
    outq      text NOT NULL DEFAULT 'QPRINT',
    status    text NOT NULL DEFAULT 'OPN',  -- OPN while written, then RDY
    user_data text NOT NULL DEFAULT '',
    pages     int NOT NULL DEFAULT 0,
    created   timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS as4tainr.spool_lines (
    spl   bigint NOT NULL,
    seq   int NOT NULL,
    page  int NOT NULL,
    line  int NOT NULL,
    text  text NOT NULL,
    PRIMARY KEY (spl, seq)
);
"#;

/// The output queue spooled files go to (QGPL/QPRINT as shipped).
pub const DEFAULT_OUTQ: &str = "QPRINT";
/// Lines per page of a program-described printer file (IBM's default form
/// length 66, overflow line 60).
pub const PAGE_LENGTH: i32 = 66;
pub const OVERFLOW_LINE: i32 = 60;

#[derive(Debug, Clone)]
pub struct SpooledFile {
    pub id: i64,
    pub job: i64,
    pub job_name: String,
    pub user: String,
    pub file: String,
    pub number: i32,
    pub outq: String,
    pub status: String,
    pub user_data: String,
    pub pages: i32,
    pub created: String,
}

const COLS: &str = "id, job, job_name, user_name, file, number, outq, status, user_data, pages,
    to_char(created, 'MM/DD/YY HH24:MI:SS')";

fn row(r: &tokio_postgres::Row) -> SpooledFile {
    SpooledFile {
        id: r.get(0),
        job: r.get(1),
        job_name: r.get(2),
        user: r.get(3),
        file: r.get(4),
        number: r.get(5),
        outq: r.get(6),
        status: r.get(7),
        user_data: r.get(8),
        pages: r.get(9),
        created: r.get(10),
    }
}

/// `as4tainr system` and `call` (no job of as4tainr's): a program's
/// spooled files belong to the user's print job QPRTJOB while the command
/// runs (other programs of the command see them: list APIs, WRKSPLF),
/// and go when it ends, their lines having gone to standard output (as
/// PASE `system` on IBM i 7.5: PUB400 2026-10-26, tools/golden/spool.txt
/// s_dump lists GLDSPL under QPRTJOB while the command runs; s_wrksplf
/// finds no spooled file afterwards).
static PRINT_JOB_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TEMPORARY: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Turn the print job mode on (main.rs: `system`, `call`).
pub fn set_print_job_mode(on: bool) {
    PRINT_JOB_MODE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn print_job_mode() -> bool {
    PRINT_JOB_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// A spooled file to delete when the command ends.
pub fn note_temporary(id: i64) {
    TEMPORARY.lock().unwrap_or_else(|e| e.into_inner()).push(id);
}

/// The command ended: its print job's spooled files go.
pub async fn delete_temporary(client: &Client) {
    let ids = std::mem::take(&mut *TEMPORARY.lock().unwrap_or_else(|e| e.into_inner()));
    for id in ids {
        let _ = delete(client, id).await;
    }
}

/// The user's print job (QPRTJOB), made the first time: the job the
/// spooled files of work without a job of its own belong to.
pub async fn print_job(client: &Client, user: &str) -> Result<JobId> {
    let user = user.to_ascii_uppercase();
    if let Some(r) = client
        .query_opt(
            "SELECT number FROM as4tainr.jobs WHERE name = 'QPRTJOB' AND user_name = $1
              ORDER BY number LIMIT 1",
            &[&user],
        )
        .await?
    {
        return Ok(JobId {
            number: r.get(0),
            user,
            name: "QPRTJOB".into(),
        });
    }
    let number: i64 = client
        .query_one("SELECT nextval('as4tainr.job_number')", &[])
        .await?
        .get(0);
    client
        .execute(
            "INSERT INTO as4tainr.jobs (number, name, user_name, type, subsystem, status, started, ended)
             VALUES ($1, 'QPRTJOB', $2, 'BCH', '', 'OUTQ', now(), now())",
            &[&number, &user],
        )
        .await?;
    Ok(JobId {
        number,
        user,
        name: "QPRTJOB".into(),
    })
}

/// Start a spooled file for a job (status OPN until [`close`]).
pub async fn open(client: &Client, job: &JobId, file: &str, user_data: &str) -> Result<i64> {
    // The output queue of the job's user (outq.rs: QGPL/QPRINT for
    // OUTQ(*WRKSTN)).
    let (qlib, q) = crate::outq::for_user(client, &job.user).await?;
    // The job's next spooled file number: one more than the last it
    // gave, also when that file was deleted (IBM i numbers a job's
    // spooled files 1, 2, 3... without reusing them).
    let r = client
        .query_one(
            "WITH n AS (
                UPDATE as4tainr.jobs SET last_splf = greatest(last_splf,
                    (SELECT coalesce(max(number), 0) FROM as4tainr.spooled_files WHERE job = $1)) + 1
                 WHERE number = $1 RETURNING last_splf)
             INSERT INTO as4tainr.spooled_files (job, job_name, user_name, file, number, user_data, outq_lib, outq)
             VALUES ($1, $2, $3, $4,
                     coalesce((SELECT last_splf FROM n),
                              (SELECT coalesce(max(number), 0) + 1 FROM as4tainr.spooled_files WHERE job = $1)),
                     $5, $6, $7)
             RETURNING id",
            &[
                &job.number,
                &job.to_string(),
                &job.user,
                &file.to_ascii_uppercase(),
                &user_data,
                &qlib,
                &q,
            ],
        )
        .await?;
    Ok(r.get(0))
}

/// Add lines (page, line, text) to an open spooled file.
pub async fn write(client: &Client, id: i64, lines: &[(i32, i32, String)]) -> Result<()> {
    for (page, line, text) in lines {
        client
            .execute(
                "INSERT INTO as4tainr.spool_lines (spl, seq, page, line, text)
                 VALUES ($1, (SELECT coalesce(max(seq), 0) + 1 FROM as4tainr.spool_lines WHERE spl = $1), $2, $3, $4)",
                // The job's QTEMP library is printed as QTEMP.
                &[&id, page, line, &crate::qtemp::logical(text.trim_end()).trim_end()],
            )
            .await?;
    }
    Ok(())
}

/// The spooled file is complete: ready (RDY) on its output queue.
pub async fn close(client: &Client, id: i64) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.spooled_files SET status = 'RDY',
                 pages = (SELECT coalesce(max(page), 0) FROM as4tainr.spool_lines WHERE spl = $1)
             WHERE id = $1",
            &[&id],
        )
        .await?;
    Ok(())
}

/// Write a whole spooled file of simple lines (one after another, new
/// pages every [`OVERFLOW_LINE`] lines) and close it.
pub async fn print_lines(
    client: &Client,
    job: &JobId,
    file: &str,
    user_data: &str,
    lines: &[String],
) -> Result<i64> {
    let id = open(client, job, file, user_data).await?;
    let per_page = OVERFLOW_LINE as usize;
    let numbered: Vec<(i32, i32, String)> = lines
        .iter()
        .enumerate()
        .map(|(i, t)| {
            (
                (i / per_page) as i32 + 1,
                (i % per_page) as i32 + 1,
                t.clone(),
            )
        })
        .collect();
    write(client, id, &numbered).await?;
    close(client, id).await?;
    Ok(id)
}

/// Write a report whose pages start with a page header (a compiler
/// listing: the product line), a new spooled file page at each header,
/// as IBM i's listings print (a listing of three pages is a spooled file
/// of three pages in WRKSPLF, PUB400 2026-10-22). Lines before the first
/// header, if any, are on page 1.
pub async fn print_report(
    client: &Client,
    job: &JobId,
    file: &str,
    user_data: &str,
    lines: &[String],
) -> Result<i64> {
    let id = open(client, job, file, user_data).await?;
    let mut numbered = Vec::new();
    let (mut page, mut line) = (1i32, 0i32);
    for t in lines {
        if is_page_header(t) && line > 0 {
            page += 1;
            line = 0;
        }
        line += 1;
        numbered.push((page, line, t.clone()));
    }
    write(client, id, &numbered).await?;
    close(client, id).await?;
    Ok(id)
}

/// A report's page header line: the product and release at the start.
pub fn is_page_header(line: &str) -> bool {
    line.trim_start().starts_with(crate::report::PRODUCT)
}

/// Spooled files of a user (WRKSPLF), oldest first.
pub async fn for_user(client: &Client, user: &str) -> Result<Vec<SpooledFile>> {
    Ok(client
        .query(
            &format!("SELECT {COLS} FROM as4tainr.spooled_files WHERE user_name = $1 ORDER BY id"),
            &[&user],
        )
        .await?
        .iter()
        .map(row)
        .collect())
}

/// Find a spooled file by name, job and number (`None` = the last one of
/// that name in the job, SPLNBR(*LAST)).
pub async fn find(
    client: &Client,
    file: &str,
    job: i64,
    number: Option<i32>,
) -> Result<SpooledFile> {
    client
        .query_opt(
            &format!(
                "SELECT {COLS} FROM as4tainr.spooled_files
                 WHERE file = $1 AND job = $2 AND ($3::int IS NULL OR number = $3)
                 ORDER BY number DESC LIMIT 1"
            ),
            &[&file.to_ascii_uppercase(), &job, &number],
        )
        .await?
        .map(|r| row(&r))
        .ok_or_else(|| {
            anyhow!(
                "CPF3303: Spooled file {} is not in job number {job:06}.",
                file.to_ascii_uppercase()
            )
        })
}

/// The pages of a spooled file as they print: each page from line 1 to
/// its last printed line, blank lines where nothing was printed. Lines
/// printed twice at the same place (overprinting) keep the last.
pub async fn pages(client: &Client, id: i64) -> Result<Vec<Vec<String>>> {
    let rows = client
        .query(
            "SELECT page, line, text FROM as4tainr.spool_lines WHERE spl = $1 ORDER BY seq",
            &[&id],
        )
        .await?;
    let mut pages: Vec<Vec<String>> = Vec::new();
    for r in rows {
        let (page, line, text): (i32, i32, String) = (r.get(0), r.get(1), r.get(2));
        let (p, l) = (page.max(1) as usize, line.max(1) as usize);
        while pages.len() < p {
            pages.push(Vec::new());
        }
        let pg = &mut pages[p - 1];
        while pg.len() < l {
            pg.push(String::new());
        }
        pg[l - 1] = text;
    }
    Ok(pages)
}

/// DLTSPLF.
pub async fn delete(client: &Client, id: i64) -> Result<()> {
    client
        .execute("DELETE FROM as4tainr.spool_lines WHERE spl = $1", &[&id])
        .await?;
    client
        .execute("DELETE FROM as4tainr.spooled_files WHERE id = $1", &[&id])
        .await?;
    Ok(())
}

/// A job log of the catalog as printed lines (QPJOBLOG), in IBM i's
/// layout (crate::joblog). What the catalog keeps of a message is its
/// type, ID, text and time: the sending and receiving programs are not
/// known (`*N`, and `*EXT`), and the job description is QGPL/QDFTJOBD,
/// the one jobs get here (docs/fidelity-gaps.md).
pub async fn job_log_lines(
    client: &Client,
    job: &JobId,
    title: crate::joblog::Title,
) -> Result<Vec<String>> {
    use crate::joblog::{date_text, time_text, End, Entry};
    let rows = client
        .query(
            "SELECT '1' || to_char(sent, 'YYMMDD'), to_char(sent, 'HH24MISS'), to_char(sent, 'US'),
                    msgid, kind, text
             FROM as4tainr.job_log WHERE job = $1 ORDER BY seq",
            &[&job.number],
        )
        .await?;
    let entries: Vec<Entry> = rows
        .iter()
        .map(|r| {
            let msgid: String = r.get(3);
            let kind: String = r.get(4);
            let text: String = r.get(5);
            let text = match text.split_once(": ") {
                Some((id, t)) if !msgid.is_empty() && id == msgid => t.to_string(),
                _ => text,
            };
            Entry {
                sev: crate::pgmmsg::system_severity(&msgid),
                msgid,
                ty: format!("*{}", kind.trim_start_matches('*')),
                date: date_text(&r.get::<_, String>(0)),
                time: time_text(&r.get::<_, String>(1), &r.get::<_, String>(2)),
                from: End {
                    pgm: "*N".into(),
                    inst: "*N".into(),
                    ..Default::default()
                },
                to: End {
                    pgm: "*EXT".into(),
                    inst: "*N".into(),
                    ..Default::default()
                },
                text,
                ..Default::default()
            }
        })
        .collect();
    let header = crate::joblog::Job {
        name: job.name.clone(),
        user: job.user.clone(),
        number: format!("{:06}", job.number),
        jobd: "QDFTJOBD".into(),
        jobd_lib: "QGPL".into(),
    };
    Ok(crate::joblog::print(
        &header,
        title,
        &entries,
        &crate::report::Stamp::now(),
        &crate::report::system_name(),
    ))
}
