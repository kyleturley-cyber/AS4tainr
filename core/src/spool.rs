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

/// Start a spooled file for a job (status OPN until [`close`]).
pub async fn open(client: &Client, job: &JobId, file: &str, user_data: &str) -> Result<i64> {
    let r = client
        .query_one(
            "INSERT INTO as4tainr.spooled_files (job, job_name, user_name, file, number, user_data)
             VALUES ($1, $2, $3, $4,
                     (SELECT coalesce(max(number), 0) + 1 FROM as4tainr.spooled_files WHERE job = $1), $5)
             RETURNING id",
            &[
                &job.number,
                &job.to_string(),
                &job.user,
                &file.to_ascii_uppercase(),
                &user_data,
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
                &[&id, page, line, &text.trim_end()],
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
                "Spooled file {} not found in job number {job:06}.",
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

/// A job log as printed lines (QPJOBLOG).
pub async fn job_log_lines(client: &Client, job: &JobId) -> Result<Vec<String>> {
    let mut out = vec![
        format!("Job Log   Job: {}", job),
        String::new(),
        format!("{:<9} {:<8} {:<7} Message", "Time", "Type", "ID"),
    ];
    for e in crate::jobs::job_log(client, job.number).await? {
        out.push(format!(
            "{:<9} {:<8} {:<7} {}",
            e.sent, e.kind, e.msgid, e.text
        ));
    }
    Ok(out)
}
