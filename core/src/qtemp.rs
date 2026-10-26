//! QTEMP: each job's own temporary library.
//!
//! On IBM i every job has a QTEMP of its own, empty when the job starts
//! and deleted when it ends; no other job sees it. as4tainr gives a job a
//! real library with a name of its own (`#QT` and the job number, or
//! `#QP` and the process ID for a process that is not a job the system
//! knows, like `as4tainr system`), and the name QTEMP stands for it
//! wherever the job names a library: `catalog::validate_name`, command
//! parameters (`cl::parse`), the library list (`Job::library_list`),
//! qualified names of APIs, SQL. What the job is shown says QTEMP again
//! (`logical`).
//!
//! The library in use is kept for the running task (`scope`) and for the
//! thread an RPG program runs on (`enter_thread`). Code that runs outside
//! a job's scope (tests that make a `Job` of their own) uses the library
//! QTEMP the catalog has, as before.

use anyhow::Result;
use std::cell::RefCell;
use std::future::Future;
use tokio_postgres::Client;

tokio::task_local! {
    static TASK: String;
}

thread_local! {
    static THREAD: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The library QTEMP stands for here, if this is a job's work.
pub fn current() -> Option<String> {
    TASK.try_with(|n| n.clone())
        .ok()
        .or_else(|| THREAD.with(|t| t.borrow().clone()))
}

/// A library name as the job means it: QTEMP is its own library.
pub fn real(lib: &str) -> String {
    if lib.eq_ignore_ascii_case("QTEMP") {
        if let Some(n) = current() {
            return n;
        }
    }
    lib.to_string()
}

/// Is this the job's QTEMP (by either name)?
pub fn is_qtemp(lib: &str) -> bool {
    lib.eq_ignore_ascii_case("QTEMP") || current().is_some_and(|n| n.eq_ignore_ascii_case(lib))
}

/// Text for the job to see: its QTEMP library's own name is QTEMP
/// (padded to the same width in a column, so columns stay).
pub fn logical(text: &str) -> String {
    let Some(n) = current() else {
        return text.to_string();
    };
    if !text.contains(&n) {
        return text.to_string();
    }
    let padded = format!("{:<w$}", "QTEMP", w = n.len());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(&n) {
        out.push_str(&rest[..i]);
        let after = &rest[i + n.len()..];
        // In a column (blanks after it), the same width; in a sentence
        // (one blank and a word), just the name.
        if after.starts_with("  ") || (after.starts_with(' ') && after.trim().is_empty()) {
            out.push_str(&padded);
        } else {
            out.push_str("QTEMP");
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Replace QTEMP in a command's words: `QTEMP`, `QTEMP/X`, and
/// `/QSYS.LIB/QTEMP.LIB/` in paths.
pub fn in_value(v: &str) -> String {
    let Some(n) = current() else {
        return v.to_string();
    };
    let up = v.to_ascii_uppercase();
    if up == "QTEMP" {
        return n;
    }
    if let Some(rest) = up.strip_prefix("QTEMP/") {
        return format!("{n}/{}", &v[v.len() - rest.len()..]);
    }
    if let Some(i) = up.find("/QTEMP.LIB") {
        return format!("{}/{n}.LIB{}", &v[..i], &v[i + "/QTEMP.LIB".len()..]);
    }
    v.to_string()
}

/// The library a job with this key (`Job::job_key`) gets.
pub fn name_for(job_key: &str) -> String {
    if let Some(pid) = job_key.strip_prefix("PID") {
        let n: u64 = pid.parse().unwrap_or(0);
        format!("#QP{:07}", n % 10_000_000)
    } else {
        let n: u64 = job_key
            .split('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        format!("#QT{:06}", n % 1_000_000)
    }
}

/// Run a job's work with its own QTEMP (made empty first, deleted after)
/// and its own current directory (`ifs::start_dir`).
pub async fn scope<F: Future>(client: &Client, job_key: &str, user: &str, f: F) -> F::Output {
    let name = name_for(job_key);
    if let Err(e) = make(client, &name).await {
        eprintln!("as4tainr: QTEMP of {job_key} not made: {e:#}");
    }
    let dir = crate::ifs::start_dir(client, user, job_key.starts_with("PID")).await;
    let r = TASK
        .scope(name.clone(), crate::ifs::with_job_dir(&dir, f))
        .await;
    drop_library(client, &name).await;
    r
}

/// The QTEMP of the job whose RPG program runs on this thread.
pub fn enter_thread(name: Option<String>) {
    THREAD.with(|t| *t.borrow_mut() = name);
}

/// The job's QTEMP for this thread while it lives (a pooled thread is
/// given back as it was).
pub struct ThreadScope(Option<String>);

impl ThreadScope {
    pub fn new(name: Option<String>) -> ThreadScope {
        let before = THREAD.with(|t| t.replace(name));
        ThreadScope(before)
    }
}

impl Drop for ThreadScope {
    fn drop(&mut self) {
        let before = self.0.take();
        THREAD.with(|t| *t.borrow_mut() = before);
    }
}

async fn make(client: &Client, name: &str) -> Result<()> {
    drop_library(client, name).await;
    crate::catalog::create_library(client, name, "Temporary library of a job").await
}

async fn drop_library(client: &Client, name: &str) {
    let r = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                crate::catalog::ident(name)
            ))
            .await?;
        // Its rows in every catalog table with a library (as DLTLIB).
        let tables = client
            .query(
                "SELECT table_name::text, column_name::text FROM information_schema.columns
                  WHERE table_schema = 'as4tainr' AND column_name IN ('library', 'journal_lib')
                    AND table_name <> 'objects'",
                &[],
            )
            .await?;
        for r in &tables {
            let (t, c): (String, String) = (r.get(0), r.get(1));
            client
                .execute(&format!("DELETE FROM as4tainr.{t} WHERE {c} = $1"), &[&name])
                .await?;
        }
        client
            .execute(
                "DELETE FROM as4tainr.object_auths WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB'",
                &[&name],
            )
            .await?;
        client
            .execute(
                "DELETE FROM as4tainr.objects WHERE (library = 'QSYS' AND name = $1 AND objtype = '*LIB') OR library = $1",
                &[&name],
            )
            .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(e) = r {
        eprintln!("as4tainr: QTEMP library {name} not deleted: {e:#}");
    }
}

/// At catalog initialization: the QTEMP libraries of jobs that ended
/// without deleting them (a process that was stopped).
pub async fn cleanup(client: &Client) -> Result<()> {
    let rows = client
        .query(
            "SELECT name FROM as4tainr.objects
              WHERE library = 'QSYS' AND objtype = '*LIB' AND (name LIKE '#QP%' OR name LIKE '#QT%')",
            &[],
        )
        .await?;
    for r in rows {
        let name: String = r.get(0);
        let live = if let Some(pid) = name.strip_prefix("#QP") {
            pid.parse::<u32>()
                .is_ok_and(|p| std::path::Path::new(&format!("/proc/{p}")).exists())
        } else {
            // A job the system knows that has ended (one it does not know
            // is left alone).
            let n: i64 = name[3..].parse().unwrap_or(-1);
            client
                .query_opt(
                    "SELECT 1 FROM as4tainr.jobs WHERE number = $1 AND (status = 'OUTQ' OR ended IS NOT NULL)",
                    &[&n],
                )
                .await
                .ok()
                .flatten()
                .is_none()
        };
        if !live {
            drop_library(client, &name).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_mapping() {
        assert_eq!(name_for("PID4242"), "#QP0004242");
        assert_eq!(name_for("000123/QSECOFR/QPADEV0001"), "#QT000123");
        enter_thread(Some("#QP0000001".into()));
        assert_eq!(real("qtemp"), "#QP0000001");
        assert_eq!(real("QGPL"), "QGPL");
        assert_eq!(in_value("QTEMP/STDOUTQSH"), "#QP0000001/STDOUTQSH");
        assert_eq!(
            in_value("/QSYS.LIB/QTEMP.LIB/TMP.FILE/TMP.MBR"),
            "/QSYS.LIB/#QP0000001.LIB/TMP.FILE/TMP.MBR"
        );
        assert_eq!(
            logical("File X created in library #QP0000001."),
            "File X created in library QTEMP."
        );
        assert_eq!(logical("#QP0000001  USR"), "QTEMP       USR");
        assert_eq!(logical("in #QP0000001 type"), "in QTEMP type");
        enter_thread(None);
        assert_eq!(real("QTEMP"), "QTEMP");
    }
}
