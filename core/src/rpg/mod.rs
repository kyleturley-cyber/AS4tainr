//! The ibmish RPG engine: ILE RPG compiled and run in Rust. See
//! docs/rpg-engine.md.
//!
//! - [`parse`]: source member → [`ast::Source`]
//! - [`program`]: names, types, storage layout → [`program::Program`]
//! - [`code`]: calculations → instructions
//! - [`interp`]: the cycle, operations, built-in functions, file I/O

pub mod ast;
pub mod code;
pub mod data;
pub mod dec;
pub mod expr;
pub mod interp;
pub mod parse;
pub mod program;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio_postgres::Client;

/// Which engine runs RPG programs: `IBMISH_RPG_ENGINE=native` (this
/// one) or `jariko` (the JVM runner; the default until the native engine
/// passes everything, see docs/rpg-engine.md).
pub fn native_enabled() -> bool {
    std::env::var("IBMISH_RPG_ENGINE")
        .map(|v| v.eq_ignore_ascii_case("native"))
        .unwrap_or(false)
}

/// The text of an include: `FILE,MEMBER`, `LIB/FILE,MEMBER`, `MEMBER`
/// (QRPGLESRC), or a stream file path.
async fn fetch_include(client: &Client, libl: &[String], id: &str) -> Option<String> {
    if id.contains('.') && !id.contains(',') || id.starts_with('/') {
        return crate::source::find_stream_file(client, id)
            .await
            .ok()
            .flatten()
            .map(|x| x.1);
    }
    let up = id.to_ascii_uppercase();
    let (libs, file, member) = match up.split_once(',') {
        Some((f, m)) => match f.split_once('/') {
            Some((l, f)) => (vec![l.to_string()], f.to_string(), m.to_string()),
            None => (libl.to_vec(), f.to_string(), m.to_string()),
        },
        None => (libl.to_vec(), "QRPGLESRC".to_string(), up.clone()),
    };
    match crate::source::find_member(client, &libs, &file, &member).await {
        Ok(m) => Some(m.source),
        Err(_) => crate::source::find_stream_file(client, id)
            .await
            .ok()
            .flatten()
            .map(|x| x.1),
    }
}

/// Fetch every include a source needs (and theirs).
pub async fn load_includes(
    client: &Client,
    libl: &[String],
    source: &str,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut todo: Vec<String> = parse::includes(source);
    while let Some(id) = todo.pop() {
        if out.contains_key(&id) {
            continue;
        }
        if let Some(text) = fetch_include(client, libl, &id).await {
            todo.extend(parse::includes(&text));
            out.insert(id, text);
        }
    }
    out
}

/// Parse and bind a program: what CRTBNDRPG checks, and what CALL runs.
pub async fn compile(
    client: &Client,
    libl: &[String],
    name: &str,
    source: &str,
) -> Result<program::Program> {
    let includes = load_includes(client, libl, source).await;
    let src = parse::parse(source, &|id| includes.get(id).cloned())?;
    let mut externals = HashMap::new();
    for f in program::external_files(&src) {
        let fr = crate::catalog::resolve_file(client, libl, &f)
            .await
            .map_err(|e| anyhow!("file {f} for the program: {e}"))?;
        externals.insert(f, fr);
    }
    let mut displays = HashMap::new();
    for f in program::display_files(&src) {
        let (library, text) = crate::catalog::resolve_display_file(client, libl, &f)
            .await
            .map_err(|e| anyhow!("display file {f} for the program: {e}"))?;
        let parsed = crate::dspf::parse(&f, &text)?;
        displays.insert(f, (library, std::sync::Arc::new(parsed)));
    }
    program::bind(name, &src, &externals, &displays)
}

/// What a call produced: DSPLY output, and an error if it ended
/// abnormally.
pub struct CallResult {
    pub messages: Vec<String>,
    pub error: Option<anyhow::Error>,
}

/// Run a program in its own database session, on a blocking thread.
#[allow(clippy::too_many_arguments)]
pub async fn call(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    display: Option<crate::bridge::DisplayHandle>,
    name: String,
    source: String,
    args: Vec<String>,
) -> Result<CallResult> {
    let rt = tokio::runtime::Handle::current();
    let url = database_url.to_string();
    tokio::task::spawn_blocking(move || -> Result<CallResult> {
        let client = rt.block_on(crate::bridge::connect_db(&url))?;
        rt.block_on(crate::bridge::job_session(
            &client,
            job.as_deref(),
            crate::bridge::default_record_wait(),
        ))?;
        // The job number of "000123/USER/NAME".
        let job_number = job
            .as_deref()
            .and_then(|j| j.split('/').next())
            .and_then(|n| n.parse().ok());
        let prog = rt.block_on(compile(&client, &libl, &name, &source))?;
        let mut m = interp::Machine::new(
            &prog,
            interp::Io {
                rt: rt.clone(),
                client: &client,
                user,
                job_number,
                libl: libl.clone(),
                display,
            },
        );
        let r = m.run(&args);
        Ok(CallResult {
            messages: std::mem::take(&mut m.messages),
            error: r.err(),
        })
    })
    .await?
}

/// The source of an RPG program kept as a file in IBMISH_PGM_PATH (the
/// samples): NAME.rpgle in any of its directories.
pub fn source_from_path(name: &str) -> Option<String> {
    let path = std::env::var("IBMISH_PGM_PATH").unwrap_or_else(|_| "qrpglesrc".into());
    for dir in std::env::split_paths(&path) {
        for n in [
            name.to_string(),
            name.to_ascii_uppercase(),
            name.to_ascii_lowercase(),
        ] {
            let p = dir.join(format!("{n}.rpgle"));
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
    }
    None
}
