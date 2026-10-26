//! The as4tainr RPG engine: ILE RPG compiled and run in Rust. See
//! docs/rpg-engine.md.
//!
//! - [`parse`]: source member → [`ast::Source`]
//! - [`program`]: names, types, storage layout → [`program::Program`]
//! - [`code`]: calculations → instructions
//! - [`interp`]: the cycle, operations, built-in functions, file I/O

pub mod ast;
pub mod check;
pub mod code;
pub mod data;
pub mod dates;
pub mod dec;
pub mod expr;
pub mod interp;
pub mod listing;
pub mod mem;
pub mod parse;
pub mod program;
pub mod rpg3;
pub mod rpg3list;
pub mod sqllist;
pub mod tofree;

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tokio_postgres::Client;

/// Where a compile looks for /COPY and /INCLUDE files (ILE RPG
/// Programmer's Guide, "Search Path Within The IFS"; ILE RPG Reference,
/// "/COPY or /INCLUDE"): recorded when the object is created, as
/// `INCPATH(*IFS|*QSYS 'dir' ...)` in its compile options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IncludePath {
    /// The source is a stream file: the IFS is searched before the
    /// library list (a member: the library list first).
    pub ifs_first: bool,
    /// The directories in order: the current directory, INCDIR, the
    /// RPGINCDIR environment variable, the source's directory.
    pub dirs: Vec<String>,
}

impl IncludePath {
    /// The compile option that records it.
    pub fn option(&self) -> String {
        let mut s = format!("INCPATH({}", if self.ifs_first { "*IFS" } else { "*QSYS" });
        for d in &self.dirs {
            s.push_str(&format!(" '{}'", d.replace('\'', "''")));
        }
        s.push(')');
        s
    }

    /// The candidates in the IFS for an include name, in search order: a
    /// name without a suffix also as name.rpgleinc and name.rpgle, all in
    /// one directory before the next.
    fn candidates(&self, id: &str) -> Vec<String> {
        let base = id.rsplit('/').next().unwrap_or(id);
        let names: Vec<String> = if base.contains('.') {
            vec![id.to_string()]
        } else {
            vec![id.to_string(), format!("{id}.rpgleinc"), format!("{id}.rpgle")]
        };
        let mut out = Vec::new();
        if id.starts_with('/') {
            out.extend(names);
        } else {
            for d in &self.dirs {
                for n in &names {
                    out.push(format!("{}/{n}", d.trim_end_matches('/')));
                }
            }
        }
        out.into_iter()
            .filter_map(|p| crate::ifs::normalize(&p).ok())
            .collect()
    }
}

/// INCPATH(...) out of compile options: (the other options, the path).
fn take_incpath(options: &str) -> (String, Option<IncludePath>) {
    let up = options.to_ascii_uppercase();
    let Some(at) = up.find("INCPATH(") else {
        return (options.to_string(), None);
    };
    let rest = &options[at + 8..];
    // Up to the closing parenthesis outside quotes.
    let mut end = rest.len();
    let mut quoted = false;
    for (i, c) in rest.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            ')' if !quoted => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let inner = &rest[..end];
    let mut path = IncludePath::default();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            let mut v = String::new();
            while let Some(c) = chars.next() {
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        v.push('\'');
                    } else {
                        break;
                    }
                } else {
                    v.push(c);
                }
            }
            path.dirs.push(v);
        } else if c == '*' {
            let w: String = std::iter::once(c)
                .chain(std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_alphanumeric())))
                .collect();
            path.ifs_first = w.eq_ignore_ascii_case("*IFS");
        }
    }
    let others = format!("{}{}", &options[..at], rest.get(end + 1..).unwrap_or(""));
    (others.trim().to_string(), Some(path))
}

/// An include file found: a member, or a stream file (its path).
enum Found {
    Member(crate::source::Member),
    Stream(String, String),
}

impl Found {
    fn text(&self) -> &str {
        match self {
            Found::Member(m) => &m.source,
            Found::Stream(_, t) => t,
        }
    }
}

/// Find an include: `FILE,MEMBER`, `LIB/FILE,MEMBER`, `MEMBER`
/// (QRPGLESRC), or a stream file path. With a recorded search path: the
/// library list and the IFS directories in the order the source's kind
/// gives; then (as4tainr's own fallback, as before search paths were
/// recorded) a stream file of that name anywhere.
async fn find_include(
    client: &Client,
    libl: &[String],
    id: &str,
    path: Option<&IncludePath>,
) -> Option<Found> {
    let ifs_form = id.contains('.') && !id.contains(',') || id.starts_with('/');
    let member = || async {
        if id.starts_with('/') {
            return None;
        }
        let up = id.to_ascii_uppercase();
        let (libs, file, member) = match up.split_once(',') {
            Some((f, m)) => match f.split_once('/') {
                Some((l, f)) => (vec![l.to_string()], f.to_string(), m.to_string()),
                None => (libl.to_vec(), f.to_string(), m.to_string()),
            },
            None => (libl.to_vec(), "QRPGLESRC".to_string(), up.clone()),
        };
        crate::source::find_member(client, &libs, &file, &member)
            .await
            .ok()
            .map(Found::Member)
    };
    if let Some(path) = path {
        let ifs = || async {
            crate::source::first_stream_file(client, &path.candidates(id))
                .await
                .ok()
                .flatten()
                .map(|(p, t)| Found::Stream(p, t))
        };
        let found = if path.ifs_first {
            match ifs().await {
                Some(f) => Some(f),
                None if !ifs_form => member().await,
                None => None,
            }
        } else {
            match if ifs_form { None } else { member().await } {
                Some(f) => Some(f),
                None => ifs().await,
            }
        };
        if found.is_some() {
            return found;
        }
    } else if !ifs_form {
        if let Some(f) = member().await {
            return Some(f);
        }
    }
    crate::source::find_stream_file(client, id)
        .await
        .ok()
        .flatten()
        .map(|(p, t)| Found::Stream(p, t))
}

/// The search path for an include of a file found: one found in the IFS
/// searches its own directory and the IFS first.
fn nested_path(path: Option<&IncludePath>, found: &Found) -> Option<IncludePath> {
    let path = path?;
    match found {
        Found::Stream(p, _) => {
            let dir = match p.rfind('/') {
                Some(0) | None => "/".to_string(),
                Some(i) => p[..i].to_string(),
            };
            let mut dirs = path.dirs.clone();
            if path.ifs_first {
                dirs.pop();
            }
            dirs.push(dir);
            Some(IncludePath {
                ifs_first: true,
                dirs,
            })
        }
        Found::Member(_) => Some(path.clone()),
    }
}

/// Fetch every include a source needs (and theirs).
pub async fn load_includes(
    client: &Client,
    libl: &[String],
    source: &str,
    path: Option<&IncludePath>,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut todo: Vec<(String, Option<IncludePath>)> = parse::includes(source)
        .into_iter()
        .map(|i| (i, path.cloned()))
        .collect();
    while let Some((id, p)) = todo.pop() {
        if out.contains_key(&id) {
            continue;
        }
        if let Some(f) = find_include(client, libl, &id, p.as_ref()).await {
            let inner = nested_path(p.as_ref(), &f);
            todo.extend(parse::includes(f.text()).into_iter().map(|i| (i, inner.clone())));
            out.insert(id, f.text().to_string());
        }
    }
    out
}

/// The /COPY and /INCLUDE members a source brings in (and theirs), with
/// what the compiler listing shows of them.
pub async fn listing_copies(
    client: &Client,
    libl: &[String],
    source: &str,
    options: &str,
) -> Vec<listing::Copy> {
    let (_, path) = take_incpath(options);
    let mut out: Vec<listing::Copy> = Vec::new();
    let mut todo: Vec<(String, Option<IncludePath>)> = parse::includes(source)
        .into_iter()
        .map(|i| (i, path.clone()))
        .collect();
    todo.reverse();
    while let Some((id, p)) = todo.pop() {
        if out.iter().any(|c| c.id == id) {
            continue;
        }
        let Some(found) = find_include(client, libl, &id, p.as_ref()).await else {
            continue;
        };
        let inner_path = nested_path(p.as_ref(), &found);
        let member = match found {
            Found::Member(m) => m,
            Found::Stream(path, text) => {
                let stat = crate::ifs::stat(client, &path).await.ok().flatten();
                crate::source::Member {
                    name: path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&path)
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase(),
                    source: text,
                    ccsid: stat.as_ref().map(|s| s.ccsid).unwrap_or(37),
                    changed_us: stat.map(|s| s.changed_us),
                    stmf: path,
                    ..Default::default()
                }
            }
        };
        let mut inner: Vec<(String, Option<IncludePath>)> = parse::includes(&member.source)
            .into_iter()
            .map(|i| (i, inner_path.clone()))
            .collect();
        inner.reverse();
        todo.extend(inner);
        out.push(listing::Copy { id, member });
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
            Some((l, r)) => (vec![crate::qtemp::real(l)], r.to_string()),
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
    // A program is compiled when it is created on IBM i; here the
    // source is compiled when the program is activated, so the result is
    // kept for the process while nothing it may depend on has changed
    // (files, source members, which carry /COPY members, stream files).
    // Stream files count only for a source that includes one.
    let ids = parse::includes(source);
    let uses_ifs = !ids.is_empty() && options.to_ascii_uppercase().contains("INCPATH(")
        || ids
            .iter()
            .any(|id| id.starts_with('/') || id.contains('.') && !id.contains(','));
    let stamp = compile_stamp(client, uses_ifs).await;
    let key = format!("{name}\u{1}{options}\u{1}{}\u{1}{ovr:?}\u{1}{source}", libl.join(","));
    if let Some(stamp) = &stamp {
        let cache = COMPILED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, p)) = cache.get(&key) {
            if s == stamp {
                return Ok((**p).clone());
            }
        }
    }
    let (_, prog) = compile_parts(client, libl, name, source, options, ovr).await;
    if let (Some(stamp), Ok(p)) = (stamp, &prog) {
        let mut cache = COMPILED.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, (stamp, std::sync::Arc::new(p.clone())));
    }
    prog
}

/// Compiled programs of this process, by what they were compiled from.
#[allow(clippy::type_complexity)]
static COMPILED: std::sync::Mutex<
    std::sync::LazyLock<HashMap<String, (String, std::sync::Arc<program::Program>)>>,
> = std::sync::Mutex::new(std::sync::LazyLock::new(HashMap::new));

/// What a compiled program depends on, as of now: the files (created,
/// deleted, changed, and their source members), and the stream files.
async fn compile_stamp(client: &Client, uses_ifs: bool) -> Option<String> {
    let files = "SELECT (SELECT count(*)::text || '/' || coalesce(max(created)::text, '') || '/' \
                 || coalesce(max(changed)::text, '') FROM as4tainr.objects WHERE objtype = '*FILE') \
             || '|' || (SELECT count(*)::text FROM as4tainr.file_fields)";
    let sql = if uses_ifs {
        format!(
            "{files} || '|' || (SELECT count(*)::text || '/' || coalesce(max(changed)::text, '') FROM as4tainr.ifs)"
        )
    } else {
        files.to_string()
    };
    let row = client.query_one(sql.as_str(), &[]).await.ok()?;
    row.try_get::<_, String>(0).ok()
}

/// DEFINE(...) out of compile options: (the other options, the names).
fn take_defines(options: &str) -> (String, Vec<String>) {
    let up = options.to_ascii_uppercase();
    let Some(at) = up.find("DEFINE(") else {
        return (options.to_string(), Vec::new());
    };
    let rest = &options[at + 7..];
    let end = rest.find(')').unwrap_or(rest.len());
    let names: Vec<String> = rest[..end]
        .split(|c: char| c.is_whitespace() || c == '\'')
        .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("*NONE"))
        .map(|n| n.to_ascii_uppercase())
        .collect();
    let others = format!("{}{}", &options[..at], rest.get(end + 1..).unwrap_or(""));
    (others.trim().to_string(), names)
}

/// Parse and bind a program, keeping the parsed source (for the compiler
/// listing) even when binding fails.
pub async fn compile_parts(
    client: &Client,
    libl: &[String],
    name: &str,
    source: &str,
    options: &str,
    ovr: &[crate::ovr::Override],
) -> (Option<ast::Source>, Result<program::Program>) {
    // INCPATH(...): where the includes are searched, recorded when the
    // object was created.
    // AS4TAINR_DUMP_SOURCE=dir: each source compiled is written there
    // (NAME.rpgle), to look at code programs generate and compile.
    if let Some(dir) = std::env::var_os("AS4TAINR_DUMP_SOURCE") {
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{name}.rpgle")), source);
    }
    let (options, incpath) = take_incpath(options);
    let includes = load_includes(client, libl, source, incpath.as_ref()).await;
    // DEFINE(names) from the create command: condition names for /IF
    // DEFINED, not a control keyword.
    let (options, defines) = take_defines(&options);
    let options = options.as_str();
    let mut src = match parse::parse_defined(source, &|id| includes.get(id).cloned(), &defines) {
        Ok(s) => s,
        Err(e) => return (None, Err(e)),
    };
    if !options.trim().is_empty() {
        match parse::keywords(options) {
            Ok(k) => src.control.extend(k),
            Err(e) => return (Some(src), Err(e)),
        }
    }
    let prog = bind_source(client, libl, name, &src, ovr).await;
    (Some(src), prog)
}

async fn bind_source(
    client: &Client,
    libl: &[String],
    name: &str,
    src: &ast::Source,
    ovr: &[crate::ovr::Override],
) -> Result<program::Program> {
    use crate::ovr::{self, Kind};
    let src = src.clone();
    let mut externals = HashMap::new();
    for f in program::external_files(&src) {
        let r =
            crate::catalog::resolve_record_file(client, libl, &ovr::file_name(ovr, Kind::Db, &f))
                .await;
        // A program-described file need not exist when the program is
        // compiled (it is opened later, by EXTFILE perhaps): the compiler
        // knows its record length only.
        let described = src
            .files
            .iter()
            .any(|d| d.name == f && (d.external || d.record_len.is_none()));
        let fr = match r {
            Ok(fr) => fr,
            Err(_) if !described => continue,
            Err(e) => return Err(anyhow!("file {f} for the program: {e}")),
        };
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
        None,
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
    data: Option<crate::runtime::DataSession>,
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
        data,
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
    data: Option<crate::runtime::DataSession>,
) -> Result<(CallResult, Vec<Vec<u8>>)> {
    let ProgramCode {
        library,
        source,
        options,
        binding,
    } = code;
    let rt = tokio::runtime::Handle::current();
    let url = database_url.to_string();
    // The job's QTEMP, for the thread the program runs on.
    let qtemp = crate::qtemp::current();
    let dir = crate::ifs::job_dir();
    tokio::task::spawn_blocking(move || -> Result<(CallResult, Vec<Vec<u8>>)> {
        let _q = crate::qtemp::ThreadScope::new(qtemp);
        let _d = crate::ifs::DirScope::new(dir);
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
                data: data.as_ref().map(|d| d.client()),
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
    let qtemp = crate::qtemp::current();
    let dir = crate::ifs::job_dir();
    tokio::task::spawn_blocking(move || {
        let _q = crate::qtemp::ThreadScope::new(qtemp);
        let _d = crate::ifs::DirScope::new(dir);
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
                for m in sb.modules.iter().filter(|m| m.attribute != "CLLE") {
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
                data: None,
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

/// A job's last spooled file opened (QSPRILSP): file, job name, user,
/// job number, spooled file number.
pub type LastSplf = (String, String, String, String, i32);

static LAST_SPLF: std::sync::Mutex<Option<std::collections::HashMap<String, LastSplf>>> =
    std::sync::Mutex::new(None);

pub fn set_last_spooled_file(job: &str, s: LastSplf) {
    if let Ok(mut g) = LAST_SPLF.lock() {
        g.get_or_insert_with(Default::default)
            .insert(job.to_string(), s);
    }
}

pub fn last_spooled_file(job: &str) -> Option<LastSplf> {
    LAST_SPLF
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(job).cloned()))
}
