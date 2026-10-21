//! The as4tainr RPG engine: ILE RPG compiled and run in Rust. See
//! docs/rpg-engine.md.
//!
//! - [`parse`]: source member → [`ast::Source`]
//! - [`program`]: names, types, storage layout → [`program::Program`]
//! - [`code`]: calculations → instructions
//! - [`interp`]: the cycle, operations, built-in functions, file I/O

pub mod ast;
pub mod code;
pub mod data;
pub mod dates;
pub mod dec;
pub mod expr;
pub mod interp;
pub mod mem;
pub mod parse;
pub mod program;
pub mod rpg3;
pub mod tofree;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio_postgres::Client;

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

/// The /COPY members an RPG III source names (and theirs), by the id
/// `rpg3::convert` asks for: `FILE,MEMBER` or `LIB/FILE,MEMBER`.
async fn rpg3_copies(client: &Client, libl: &[String], source: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut todo: Vec<String> = rpg3::copy_ids(source);
    while let Some(id) = todo.pop() {
        if out.contains_key(&id) {
            continue;
        }
        let (libs, rest) = match id.split_once('/') {
            Some((l, r)) => (vec![l.to_string()], r.to_string()),
            None => (libl.to_vec(), id.clone()),
        };
        let Some((file, member)) = rest.split_once(',') else {
            continue;
        };
        if let Ok(m) = crate::source::find_member(client, &libs, file, member).await {
            todo.extend(rpg3::copy_ids(&m.source));
            out.insert(id, m.source);
        }
    }
    out
}

/// Convert an RPG III (RPG/400) member to RPG IV: with its /COPY members
/// in it when `expand` (CRTRPGPGM always; CVTRPGSRC EXPCPY(*YES)).
pub async fn convert_rpg3(
    client: &Client,
    libl: &[String],
    source: &str,
    expand: bool,
) -> Result<rpg3::Converted> {
    if expand {
        let copies = rpg3_copies(client, libl, source).await;
        let f = |id: &str| copies.get(id).cloned();
        rpg3::convert(source, Some(&f))
    } else {
        rpg3::convert(source, None)
    }
}

/// Parse and bind a program: what CRTBNDRPG checks, and what CALL runs.
pub async fn compile(
    client: &Client,
    libl: &[String],
    name: &str,
    source: &str,
) -> Result<program::Program> {
    compile_with(client, libl, name, source, "").await
}

/// Compile with the create command's options (control keywords such as
/// `ALWNULL(*USRCTL)`); keywords in the source's control specifications
/// take precedence over them, as on IBM i.
pub async fn compile_with(
    client: &Client,
    libl: &[String],
    name: &str,
    source: &str,
    options: &str,
) -> Result<program::Program> {
    compile_ovr(client, libl, name, source, options, &[]).await
}

/// Compile with the job's file overrides in effect (OVRDBF TOFILE,
/// OVRPRTF TOFILE, PAGESIZE and OVRFLW): the program opens the files they
/// name.
pub async fn compile_ovr(
    client: &Client,
    libl: &[String],
    name: &str,
    source: &str,
    options: &str,
    ovr: &[crate::ovr::Override],
) -> Result<program::Program> {
    use crate::ovr::{self, Kind};
    let includes = load_includes(client, libl, source).await;
    let mut src = parse::parse(source, &|id| includes.get(id).cloned())?;
    if !options.trim().is_empty() {
        src.control.extend(parse::keywords(options)?);
    }
    let mut externals = HashMap::new();
    for f in program::external_files(&src) {
        let fr = crate::catalog::resolve_file(client, libl, &ovr::file_name(ovr, Kind::Db, &f))
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
    // Printer files: the object's page size and overflow line; the record
    // formats of one described by DDS.
    let mut printers = HashMap::new();
    for f in program::printer_files(&src) {
        if let Some(mut a) =
            crate::prtf::resolve(client, libl, &ovr::file_name(ovr, Kind::Printer, &f)).await?
        {
            if let Some(o) = ovr::effective(ovr, Kind::Printer, &f) {
                if let Some((len, width)) = o.pagesize {
                    a.page_length = len;
                    a.page_width = width;
                }
                if let Some(of) = o.ovrflw {
                    a.overflow = of;
                }
            }
            let dds = if a.source.trim().is_empty() {
                None
            } else {
                Some(std::sync::Arc::new(crate::prtf::parse(&f, &a.source)?))
            };
            printers.insert(f, (a, dds));
        }
    }
    program::bind(name, &src, &externals, &displays, &printers)
}

/// What CL run from RPG (QCMDEXC, CALL of a CL program) needs of the
/// calling job.
#[derive(Clone, Debug)]
pub struct ClContext {
    pub env: crate::commands::Environment,
    pub curlib: Option<String>,
    /// The user part of the library list.
    pub user_libl: Vec<String>,
    /// The file overrides in effect for the call.
    pub overrides: Vec<crate::ovr::Override>,
}

/// What a call produced: DSPLY output, and an error if it ended
/// abnormally.
pub struct CallResult {
    pub messages: Vec<String>,
    /// Printer output of a call without a job (otherwise it is in the
    /// job's spooled files).
    pub printed: Vec<String>,
    pub error: Option<anyhow::Error>,
    /// The parameters' values when the program ended (passed by
    /// reference: the caller's variables get them back).
    pub params: Vec<String>,
}

/// Run a program in its own database session, on a blocking thread.
#[allow(clippy::too_many_arguments)]
pub async fn call(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    display: Option<crate::runtime::DisplayHandle>,
    cl: Option<ClContext>,
    name: String,
    source: String,
    args: Vec<String>,
) -> Result<CallResult> {
    call_with(
        database_url,
        libl,
        user,
        job,
        display,
        cl,
        name,
        ProgramCode {
            library: None,
            source,
            options: String::new(),
            binding: None,
        },
        args,
    )
    .await
}

/// A program object's code: its (entry module's) source, the create
/// command's options, and how its modules are bound (CRTPGM, binding
/// directories).
#[derive(Clone, Debug, Default)]
pub struct ProgramCode {
    /// The program object's library (None: a source not in a library).
    pub library: Option<String>,
    pub source: String,
    pub options: String,
    pub binding: Option<crate::ile::Binding>,
}

/// `call` of a program object.
#[allow(clippy::too_many_arguments)]
pub async fn call_with(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    display: Option<crate::runtime::DisplayHandle>,
    cl: Option<ClContext>,
    name: String,
    code: ProgramCode,
    args: Vec<String>,
) -> Result<CallResult> {
    call_any(
        database_url,
        libl,
        user,
        job,
        display,
        cl,
        name,
        code,
        Ok(args),
    )
    .await
    .map(|r| r.0)
}

/// A program object called with parameters as bytes (passed by
/// reference, as the database passes a trigger program its buffer):
/// the result and the parameters as the program left them.
#[allow(clippy::too_many_arguments)]
pub async fn call_bytes(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    cl: Option<ClContext>,
    name: String,
    code: ProgramCode,
    parms: Vec<Vec<u8>>,
) -> Result<(CallResult, Vec<Vec<u8>>)> {
    call_any(
        database_url,
        libl,
        user,
        job,
        None,
        cl,
        name,
        code,
        Err(parms),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn call_any(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    display: Option<crate::runtime::DisplayHandle>,
    cl: Option<ClContext>,
    name: String,
    code: ProgramCode,
    args: std::result::Result<Vec<String>, Vec<Vec<u8>>>,
) -> Result<(CallResult, Vec<Vec<u8>>)> {
    let ProgramCode {
        library,
        source,
        options,
        binding,
    } = code;
    let rt = tokio::runtime::Handle::current();
    let url = database_url.to_string();
    tokio::task::spawn_blocking(move || -> Result<(CallResult, Vec<Vec<u8>>)> {
        let client = rt.block_on(crate::runtime::connect_db(&url))?;
        rt.block_on(crate::runtime::job_session(
            &client,
            job.as_deref(),
            crate::runtime::default_record_wait(),
        ))?;
        // The program, for the journal entries of its changes.
        rt.block_on(client.execute(
            "SELECT set_config('as4tainr.program', $1, false), set_config('as4tainr.program_lib', $2, false),
                    set_config('as4tainr.user', $3, false)",
            &[&name.to_ascii_uppercase(), &library.clone().unwrap_or_default(), &user],
        ))?;
        // The job number of "000123/USER/NAME".
        let job_number = job
            .as_deref()
            .and_then(|j| j.split('/').next())
            .and_then(|n| n.parse().ok());
        let job_id = job.as_deref().and_then(|j| {
            let mut parts = j.split('/');
            Some(crate::jobs::JobId {
                number: parts.next()?.parse().ok()?,
                user: parts.next()?.to_string(),
                name: parts.next()?.to_string(),
            })
        });
        let ovr = cl.as_ref().map(|c| c.overrides.clone()).unwrap_or_default();
        let prog = rt.block_on(compile_ovr(&client, &libl, &name, &source, &options, &ovr))?;
        // Called from CL: the caller is in the default activation group.
        let actgrp = interp::program_group(&prog, crate::ile::DEFAULT_GROUP);
        let mut m = interp::Machine::new(
            &prog,
            interp::Io {
                rt: rt.clone(),
                client: &client,
                user,
                job_number,
                libl: libl.clone(),
                job: job_id,
                display,
                cl,
                callers: vec![],
                database_url: url.clone(),
                job_key: job
                    .clone()
                    .unwrap_or_else(|| format!("PID{}", std::process::id())),
                binding: binding.map(std::sync::Arc::new),
                unit: None,
                modules: Default::default(),
                actgrp,
                program: format!(
                    "{}/{}",
                    library.as_deref().unwrap_or("*NONE"),
                    name.to_ascii_uppercase()
                ),
                shared: Default::default(),
            },
        );
        let (r, bytes) = match args {
            Ok(args) => (m.run(&args), Vec::new()),
            Err(mut parms) => {
                let r = m.run_bytes(&mut parms);
                (r, parms)
            }
        };
        Ok((
            CallResult {
                messages: std::mem::take(&mut m.messages),
                printed: std::mem::take(&mut m.printed),
                error: r.err(),
                params: std::mem::take(&mut m.returned),
            },
            bytes,
        ))
    })
    .await?
}

/// CALLPRC from a CL procedure: an RPG procedure the CL program is bound
/// to (a module bound into it, or a service program), with values;
/// returns the parameters' values after the call and the return value.
#[allow(clippy::too_many_arguments)]
pub async fn call_procedure(
    database_url: &str,
    libl: Vec<String>,
    user: String,
    job: Option<String>,
    program: String,
    binding: crate::ile::Binding,
    group: String,
    sym: String,
    args: Vec<data::Value>,
) -> Result<(Vec<data::Value>, Option<data::Value>, Vec<String>)> {
    use crate::ile::Target;
    let rt = tokio::runtime::Handle::current();
    let url = database_url.to_string();
    tokio::task::spawn_blocking(move || {
        let client = rt.block_on(crate::runtime::connect_db(&url))?;
        let target = binding.imports.get(&sym).cloned().ok_or_else(|| {
            anyhow!("CPD5D02: Procedure {sym} is not bound to program {program}.")
        })?;
        let (code, unit, b, group) = match &target {
            Target::Module(m) => {
                let code = binding
                    .modules
                    .iter()
                    .find(|x| &x.name == m)
                    .cloned()
                    .ok_or_else(|| anyhow!("module {m} is not bound into the program"))?;
                (code, format!("{program}/MODULE {m}"), binding.clone(), group.clone())
            }
            Target::SrvPgm { library, name } => {
                let (lib, sb) = rt.block_on(crate::ile::find_srvpgm(&client, &libl, library, name))?;
                if let Some(sig) = binding.srv_signatures.get(&format!("{lib}/{name}")) {
                    if *sig != sb.signature && !sb.prv_signatures.contains(sig) {
                        anyhow::bail!("MCH4431: Program signature violation: service program {lib}/{name} no longer has the signature {sig} the caller was bound to.");
                    }
                }
                let mut found = None;
                for m in &sb.modules {
                    let p = rt.block_on(compile_with(&client, &libl, &m.name, &m.source, &m.options))?;
                    if p.procs.get(&sym).is_some_and(|pi| pi.export) {
                        found = Some(m.clone());
                        break;
                    }
                }
                let code = found.ok_or_else(|| {
                    anyhow!("CPD5D02: Service program {lib}/{name} does not export {sym}.")
                })?;
                let g = if sb.actgrp.is_empty() || sb.actgrp == "*CALLER" {
                    group.clone()
                } else {
                    sb.actgrp.clone()
                };
                (code.clone(), format!("{lib}/{name}/{}", code.name), sb, g)
            }
        };
        let prog = rt.block_on(compile_with(&client, &libl, &code.name, &code.source, &code.options))?;
        let job_key = job
            .clone()
            .unwrap_or_else(|| format!("PID{}", std::process::id()));
        let job_id = job.as_deref().and_then(|j| {
            let mut parts = j.split('/');
            Some(crate::jobs::JobId {
                number: parts.next()?.parse().ok()?,
                user: parts.next()?.to_string(),
                name: parts.next()?.to_string(),
            })
        });
        let mut m = interp::Machine::new(
            &prog,
            interp::Io {
                rt: rt.clone(),
                client: &client,
                user,
                job_number: job_id.as_ref().map(|j| j.number),
                libl: libl.clone(),
                job: job_id,
                display: None,
                cl: None,
                callers: vec![],
                database_url: url.clone(),
                job_key: job_key.clone(),
                binding: Some(std::sync::Arc::new(b)),
                unit: Some(unit.clone()),
                modules: Default::default(),
                actgrp: group.clone(),
                program: format!("*BOUND/{}", code.name),
                shared: Default::default(),
            },
        );
        if let Some(s) = crate::ile::activation(&job_key, &group, &unit) {
            m.set_storage(s);
        }
        let globals = prog.globals.len();
        let (back, ret) = m.call_exported(&sym, args)?;
        crate::ile::keep_activation(&job_key, &group, &unit, m.storage(globals));
        Ok((back, ret, std::mem::take(&mut m.messages)))
    })
    .await?
}

/// The source of an RPG program kept as a file in AS4TAINR_PGM_PATH (the
/// samples): NAME.rpgle in any of its directories.
pub fn source_from_path(name: &str) -> Option<String> {
    let path = std::env::var("AS4TAINR_PGM_PATH").unwrap_or_else(|_| "qrpglesrc".into());
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
