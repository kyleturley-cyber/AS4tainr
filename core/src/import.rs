//! `as4tainr import DIR --lib LIB`: bring an application's source and data
//! into a library and build its objects.
//!
//! The directory is laid out the way IBM i source is usually kept in git:
//! one folder per source file (`QDDSSRC/`, `QRPGLESRC/`, `QCLSRC/`...),
//! one stream file per member, named `MEMBER.TYPE` (`CUSTMAST.PF`,
//! `ORDENT.RPGLE`, `ORDENTC.CLLE`; any case). Members found directly in
//! DIR go to the usual source file for their type. CSV files (in DIR,
//! `data/`, or any folder; header row = field names) are the initial
//! records of the physical file of the same name.
//!
//! The import runs the IBM i create commands, so it does what an operator
//! would do by hand:
//!
//! 1. CRTLIB (if needed), CRTSRCPF for each source file, then every member
//!    is added (replacing a member of the same name).
//! 2. CRTPF for each PF member, and loads its CSV when the file is new
//!    (an existing file keeps its records);
//! 3. CRTLF, CRTDSPF, CRTPRTF;
//! 4. CRTBNDCL (CLLE) and CRTCLPGM (CLP), CRTBNDRPG (RPGLE), CRTSQLRPGI
//!    (SQLRPGLE).
//!
//! Members as4tainr cannot build yet (PRTF, COBOL, CMD)
//! are kept as source and reported.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::commands::{Job, Outcome};
use crate::{catalog, rla, source};

/// Minimal CSV: comma-separated, double quotes for fields containing
/// commas or quotes ("" escapes a quote).
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Write the records of a CSV text (header row = field names) to a file.
/// Returns the number of records written.
pub async fn load_csv(
    client: &tokio_postgres::Client,
    file: catalog::FileRef,
    text: &str,
) -> Result<usize> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<String> = parse_csv_line(lines.next().context("empty CSV")?)
        .into_iter()
        .map(|h| h.trim().to_ascii_uppercase())
        .collect();
    let mut of = rla::OpenFile::new(file);
    let mut n = 0;
    for line in lines {
        let vals = parse_csv_line(line);
        let rec: rla::Record = header.iter().cloned().zip(vals).collect();
        of.write(client, &rec).await?;
        n += 1;
    }
    Ok(n)
}

/// The usual source file for a member type.
pub fn default_source_file(srctype: &str) -> &'static str {
    match srctype {
        "PF" | "LF" | "DSPF" | "PRTF" => "QDDSSRC",
        "RPGLE" | "SQLRPGLE" => "QRPGLESRC",
        "RPG" => "QRPGSRC",
        "CLLE" | "CLP" => "QCLSRC",
        "CMD" => "QCMDSRC",
        "CBLLE" => "QCBLLESRC",
        "SQL" => "QSQLSRC",
        "BND" => "QSRVSRC",
        "BNDDIR" => "QBNDDIRSRC",
        _ => "QTXTSRC",
    }
}

/// One member found in the directory.
#[derive(Debug, Clone)]
struct Found {
    file: String,
    member: String,
    srctype: String,
    text: String,
    path: PathBuf,
    /// The object type part of a TOBi name (`pgm` in X.pgm.rpgle).
    objtype: Option<String>,
    /// Brought into other sources by name (`/COPY llist_h.rpgle`): a copy
    /// member, not compiled.
    included: bool,
}

/// What the import did, line by line, and whether everything was built.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub failed: usize,
}

/// A stream file's member name, text and extension. Plain names
/// (`ORDENT.RPGLE`) and the TOBi convention (`ORDENT-Order_entry.pgm.rpgle`:
/// object name, then a description with underscores for blanks, then the
/// object type) are both understood.
pub fn member_of(file_name: &str) -> Option<(String, String, String)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    // Drop an object-type part (.pgm, .module, .srvpgm, .cmd...).
    let stem = match stem.rsplit_once('.') {
        Some((s, kind)) if kind.chars().all(|c| c.is_ascii_alphabetic()) => s,
        _ => stem,
    };
    let (name, text) = match stem.split_once('-') {
        Some((n, t)) => (n, t.replace('_', " ")),
        None => (stem, String::new()),
    };
    let name = catalog::validate_name("member", &name.to_ascii_uppercase()).ok()?;
    Some((name, text.trim().to_string(), ext.to_ascii_lowercase()))
}

/// What the directory holds.
#[derive(Debug, Default)]
struct Scan {
    members: Vec<Found>,
    /// CSV data by file name.
    data: BTreeMap<String, PathBuf>,
    /// Include files (`.rpgleinc`, `.clleinc`): path relative to DIR.
    includes: Vec<(String, PathBuf)>,
    /// Same member twice: (member, the path kept, the path left out).
    duplicates: Vec<(String, PathBuf, PathBuf)>,
    /// Build rules from TOBi Rules.mk files: object name -> (object type,
    /// what it is built from).
    rules: BTreeMap<String, (String, Vec<String>)>,
}

/// Service programs a Makefile or a setup script builds with CRTSRVPGM
/// (`system "CRTSRVPGM $(BIN_LIB)/LLIST MODULE($(BIN_LIB)/LLIST
/// $(BIN_LIB)/LLIST_SORT) BNDSRVPGM(...) ..."`, as the OSSILE projects
/// do): a rule LLIST.SRVPGM from those modules (and the service programs
/// it binds, X.SRVPGM).
fn makefile_rules(text: &str, out: &mut BTreeMap<String, (String, Vec<String>)>) {
    let name_of = |q: &str| {
        q.rsplit('/')
            .next()
            .unwrap_or(q)
            .trim_matches(|c: char| c == '"' || c == '\'' || c == ')')
            .to_ascii_uppercase()
    };
    for line in text.lines() {
        let u = line.to_ascii_uppercase();
        let Some(at) = u.find("CRTSRVPGM ") else {
            continue;
        };
        let rest = &u[at + 10..];
        let Some(target) = rest.split_whitespace().next() else {
            continue;
        };
        let target = name_of(target.trim_start_matches("SRVPGM("));
        if target.starts_with('$') {
            continue;
        }
        let Some(m) = rest.find("MODULE(") else {
            continue;
        };
        let mods = &rest[m + 7..];
        let mut depth = 1;
        let end = mods
            .char_indices()
            .find(|&(_, c)| {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map(|(j, _)| j)
            .unwrap_or(mods.len());
        let mut modules: Vec<String> = mods[..end]
            .split_whitespace()
            .map(|x| format!("{}.MODULE", name_of(x)))
            .filter(|x| !x.starts_with('$') && !x.starts_with('*'))
            .collect();
        if let Some(b) = rest.find("BNDSRVPGM(") {
            let list = &rest[b + 10..];
            let end = list.find(')').unwrap_or(list.len());
            modules.extend(
                list[..end]
                    .split_whitespace()
                    .map(|x| format!("{}.SRVPGM", name_of(x)))
                    .filter(|x| !x.starts_with('$') && !x.starts_with('*')),
            );
        }
        if catalog::validate_name("service program", &target).is_ok() && !modules.is_empty() {
            out.entry(target).or_insert(("SRVPGM".into(), modules));
        }
    }
}

/// A file's text: UTF-8, or else Windows-1252, the code page source
/// downloaded from IBM i to a Windows PC is usually in (zlib's changelog in
/// the OSSILE corpus is one).
fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| cp1252(b)).collect(),
    })
}

/// One byte of Windows-1252 as a character (0x80-0x9F are its own
/// characters; the bytes it leaves undefined are kept as C1 controls).
fn cp1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match b {
        0x80..=0x9F => char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('?'),
        _ => b as char,
    }
}

/// TOBi build rules (`Rules.mk`): lines like `CONVSRV.SRVPGM: CONVSRV.bnd
/// CONVSRV.MODULE`.
fn read_rules(text: &str, out: &mut BTreeMap<String, (String, Vec<String>)>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((target, deps)) = line.split_once(':') else {
            continue;
        };
        // A target's variable (`X.SRVPGM: private TEXT := ...`, as makei
        // reads them) is no rule: kept as `X.SRVPGM:TEXT`.
        if let Some((var, value)) = deps.split_once(":=") {
            let var = var
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_ascii_uppercase();
            if !var.is_empty() {
                out.insert(
                    format!("{}:{var}", target.trim().to_ascii_uppercase()),
                    (var, vec![value.trim().to_string()]),
                );
            }
            continue;
        }
        if deps.trim_start().starts_with('=') {
            continue;
        }
        let Some((name, kind)) = target.trim().rsplit_once('.') else {
            continue;
        };
        if name.contains(' ') || name.contains('$') || name.contains('=') {
            continue;
        }
        let (name, kind) = (name.to_ascii_uppercase(), kind.to_ascii_uppercase());
        // X.MODULE and X.SRVPGM (or X.PGM) of the same name (a service
        // program or program made from its module): the service program's
        // or program's rule is kept, the module noted.
        let deps_list: Vec<String> = deps.split_whitespace().map(|d| d.to_string()).collect();
        if kind == "MODULE" {
            out.insert(
                format!("{name}.MODULE"),
                ("MODULE".into(), deps_list.clone()),
            );
        }
        if !matches!(kind.as_str(), "SRVPGM" | "PGM")
            && out
                .get(&name)
                .is_some_and(|r| r.0 == "SRVPGM" || r.0 == "PGM")
        {
            continue;
        }
        out.insert(name, (kind, deps_list));
    }
}

/// Walk DIR and its folders. A folder named like a source file
/// (`QRPGLESRC`) puts its members in that file; other folders (project
/// folders) are only for grouping.
/// Does the directory hold a file of this name (case not compared, as
/// in the IFS's root file system)?
fn dir_has(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|f| f.eq_ignore_ascii_case(name))
        })
    })
}

/// A source that includes files by a relative path (`/COPY
/// ../Copy_Mbrs/X.RPGLE`): such a path is only found from the source's
/// directory in the IFS, so the source is compiled from there (SRCSTMF).
fn includes_relative_paths(path: &Path) -> bool {
    read_text(path).is_ok_and(|t| {
        crate::rpg::parse::includes(&t)
            .iter()
            .any(|id| id.contains('/') && !id.starts_with('/') && !id.contains(','))
    })
}

/// Every source file of a project (by its suffix), hidden folders left
/// out: what a TOBi build finds in the IFS.
fn project_sources(dir: &Path) -> Vec<std::path::PathBuf> {
    const SUFFIXES: &[&str] = &[
        "rpgle", "rpgleinc", "sqlrpgle", "rpg", "clle", "clleinc", "clp", "cmd", "sql", "dspf",
        "prtf", "pf", "lf", "bnd", "bnddir", "cbl", "cblle", "sqlcblle", "c", "h", "rexx", "msgf",
        "pnlgrp", "menu", "table", "view", "index", "sqlprc", "sqludf", "sqltrg", "sqlseq",
        "srvpgm", "pgm", "module",
    ];
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                todo.push(p);
            } else if name
                .rsplit_once('.')
                .is_some_and(|(_, x)| SUFFIXES.contains(&x.to_ascii_lowercase().as_str()))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A CL source with `INCLUDE SRCSTMF('a relative path')`.
fn cl_includes_relative_paths(path: &Path) -> bool {
    read_text(path).is_ok_and(|t| {
        t.lines().any(|l| {
            let u = l.trim_start().to_ascii_uppercase();
            u.starts_with("INCLUDE ")
                && u.split_once("SRCSTMF(")
                    .is_some_and(|(_, r)| !r.trim_start().trim_start_matches('\'').starts_with('/'))
        })
    })
}

/// A TOBi (makei) project: `iproj.json` at its root. Its sources are
/// compiled from the IFS (SRCSTMF), with the project's include path.
fn is_tobi(dir: &Path) -> bool {
    dir.join("iproj.json").is_file()
}

/// iproj.json's includePath: directories relative to the project.
fn tobi_include_path(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("iproj.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("includePath").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn scan(dir: &Path) -> Result<Scan> {
    let tobi = is_tobi(dir);
    let mut out = Scan::default();
    // Files whose names cannot be member names (ARRAYLIST_H is 11
    // characters): still include files when a source names them.
    let mut loose: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<(PathBuf, Option<String>)> = vec![(dir.to_path_buf(), None)];
    while let Some((folder, file_of_folder)) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&folder)
            .with_context(|| format!("reading {}", folder.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries.iter().rev().filter(|p| p.is_dir()) {
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(
                fname,
                ".git" | ".github" | ".vscode" | ".idea" | "node_modules"
            ) {
                continue;
            }
            let upper = fname.to_ascii_uppercase();
            let file = (upper.starts_with('Q') && upper.ends_with("SRC"))
                .then(|| catalog::validate_name("file", &upper).ok())
                .flatten();
            stack.push((p.clone(), file.or_else(|| file_of_folder.clone())));
        }
        for p in entries.iter().filter(|p| p.is_file()) {
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if fname.eq_ignore_ascii_case("Rules.mk") {
                if let Ok(t) = read_text(p) {
                    read_rules(&t, &mut out.rules);
                }
                continue;
            }
            if fname.eq_ignore_ascii_case("Makefile")
                || fname.eq_ignore_ascii_case("setup")
                || fname.to_ascii_lowercase().ends_with(".sh")
            {
                if let Ok(t) = read_text(p) {
                    makefile_rules(&t, &mut out.rules);
                }
                continue;
            }
            let ext = fname
                .rsplit_once('.')
                .map(|x| x.1.to_ascii_lowercase())
                .unwrap_or_default();
            if ext.ends_with("inc") {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.includes.push((rel, p.clone()));
                // In a folder named for a source file (QCPYSRC/): also a
                // member of that file, for /COPY QCPYSRC,NAME.
                let stem = fname.rsplit_once('.').map(|x| x.0).unwrap_or(fname);
                if let (Some(file), Ok(member)) = (
                    file_of_folder.clone(),
                    catalog::validate_name("member", &stem.to_ascii_uppercase()),
                ) {
                    if !out
                        .members
                        .iter()
                        .any(|m| m.file == file && m.member == member)
                    {
                        out.members.push(Found {
                            file,
                            member,
                            srctype: ext.to_ascii_uppercase(),
                            text: String::new(),
                            path: p.clone(),
                            objtype: None,
                            included: true,
                        });
                    }
                }
                continue;
            }
            let Some((name, text, ext)) = member_of(fname) else {
                loose.push(p.clone());
                continue;
            };
            if ext == "csv" {
                out.data.insert(name, p.clone());
                continue;
            }
            let Some(srctype) = source::type_for_extension(&ext) else {
                continue;
            };
            let mut file = file_of_folder
                .clone()
                .unwrap_or_else(|| default_source_file(srctype).to_string());
            // Two sources of one name in a folder (EMPDET.bnd and
            // EMPDET.sqlrpgle, as the build tools keep them): the one whose
            // usual source file is another goes there (binder source to
            // QSRVSRC), as on IBM i, where a source file holds one member
            // of a name.
            if let Some(i) = out
                .members
                .iter()
                .position(|m| m.file == file && m.member == name)
            {
                let taken =
                    |f: &str, ms: &[Found]| ms.iter().any(|m| m.file == f && m.member == name);
                let mine = default_source_file(srctype);
                let theirs = default_source_file(&out.members[i].srctype);
                if mine != file && !taken(mine, &out.members) {
                    file = mine.to_string();
                } else if theirs != file && !taken(theirs, &out.members) {
                    out.members[i].file = theirs.to_string();
                } else {
                    out.duplicates
                        .push((name, out.members[i].path.clone(), p.clone()));
                    continue;
                }
            }
            let objtype = fname
                .rsplit_once('.')
                .and_then(|(stem, _)| stem.rsplit_once('.'))
                .map(|(_, k)| k.to_ascii_lowercase())
                .filter(|k| k.chars().all(|c| c.is_ascii_alphabetic()));
            out.members.push(Found {
                file,
                member: name,
                srctype: srctype.to_string(),
                text,
                path: p.clone(),
                objtype,
                included: false,
            });
        }
    }
    // A build rule that names a source of another name (TEMPDET.MODULE:
    // empdet.test.sqlrpgle): the member takes the object's name.
    let renames: Vec<(String, String)> = out
        .rules
        .iter()
        .filter_map(|(target, (_, deps))| {
            // X.MODULE: the module of X (also when X is a service program).
            let target = match target.split_once('.') {
                Some((t, "MODULE")) => t.to_string(),
                Some(_) => return None,
                None => target.clone(),
            };
            deps.first().map(|d| (target, d.to_ascii_lowercase()))
        })
        .collect();
    for (target, dep) in renames {
        let Some(i) = out.members.iter().position(|m| {
            m.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(&dep))
        }) else {
            continue;
        };
        let file = out.members[i].file.clone();
        if out.members[i].member != target
            && catalog::validate_name("member", &target).is_ok()
            && !out
                .members
                .iter()
                .any(|m| m.file == file && m.member == target)
        {
            out.members[i].member = target;
        }
    }
    // Sources other sources bring in by file name (`/COPY llist_h.rpgle`,
    // `/INCLUDE 'iconv_h.rpgle'`): copy members, and stream files for the
    // compiler to find by name.
    let mut wanted = std::collections::BTreeSet::new();
    let mut members_named = std::collections::BTreeSet::new();
    for m in &out.members {
        if !matches!(m.srctype.as_str(), "RPGLE" | "SQLRPGLE" | "RPGLEINC") {
            continue;
        }
        let Ok(t) = read_text(&m.path) else { continue };
        for id in crate::rpg::parse::includes(&t) {
            let id = id.trim().trim_matches(|c| c == '\'' || c == '"');
            let base = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
            // A TOBi project compiles from its stream files: a name
            // without a suffix is first an IFS file next to the source
            // (name, name.rpgleinc, name.rpgle; ILE RPG Programmer's
            // Guide, "Search Path Within The IFS"), then a member.
            let sibling = if tobi && !id.contains(',') && !base.contains('.') {
                m.path.parent().and_then(|d| {
                    [
                        base.clone(),
                        format!("{base}.rpgleinc"),
                        format!("{base}.rpgle"),
                    ]
                    .into_iter()
                    .find(|n| dir_has(d, n))
                })
            } else {
                None
            };
            if let Some(n) = sibling {
                wanted.insert(n);
            } else if base.contains('.') && !id.contains(',') {
                wanted.insert(base);
            } else {
                // A member: MBR (in QRPGLESRC), FILE,MBR or LIB/FILE,MBR.
                let up = id.to_ascii_uppercase();
                let (file, member) = match up.split_once(',') {
                    Some((f, m)) => (f.rsplit('/').next().unwrap_or(f).to_string(), m.to_string()),
                    None => ("QRPGLESRC".to_string(), up.clone()),
                };
                members_named.insert((file, member));
            }
        }
    }
    for m in out.members.iter_mut() {
        if members_named.contains(&(m.file.clone(), m.member.clone())) {
            m.included = true;
        }
    }
    for p in loose {
        let fname = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if wanted.contains(&fname) {
            let rel = p
                .strip_prefix(dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            out.includes.push((rel, p.clone()));
        }
    }
    for m in out.members.iter_mut() {
        let fname = m
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if wanted.contains(&fname) && !out.includes.iter().any(|(_, q)| *q == m.path) {
            m.included = true;
            let rel = m
                .path
                .strip_prefix(dir)
                .unwrap_or(&m.path)
                .to_string_lossy()
                .replace('\\', "/");
            out.includes.push((rel, m.path.clone()));
        }
    }
    Ok(out)
}

/// Run one create command and record the result.
async fn step(job: &mut Job<'_>, report: &mut Report, what: &str, command: &str) -> bool {
    match job.run(command).await {
        Ok(Outcome::Messages(m)) => {
            report.lines.push(format!(
                "  {what}: {}",
                m.last().cloned().unwrap_or_else(|| "done".into())
            ));
            true
        }
        Ok(_) => true,
        Err(e) => {
            report.failed += 1;
            let text = format!("{e:#}");
            report.lines.push(format!(
                "  {what}: FAILED: {}",
                text.lines().last().unwrap_or("")
            ));
            false
        }
    }
}

/// Import DIR into LIB with this job (which should be QSECOFR's, or a
/// user who may create objects in LIB).
pub async fn import(job: &mut Job<'_>, dir: &Path, library: &str) -> Result<Report> {
    let lib = catalog::validate_name("library", library)?;
    if !dir.is_dir() {
        bail!("{} is not a directory.", dir.display());
    }
    let Scan {
        members,
        data,
        includes,
        duplicates,
        rules,
    } = scan(dir)?;
    if members.is_empty() {
        bail!(
            "No source members found in {} (expected folders like QRPGLESRC/ with files like ORDENT.RPGLE).",
            dir.display()
        );
    }
    let mut report = Report::default();
    // A TOBi project: RPG sources compiled from the IFS (SRCSTMF, as
    // makei does), copied under /<library>/ with the include files, the
    // project's includePath as INCDIR.
    // (makei: TOBi's src/mk/def_rules.mk, COMMIT := *NONE and so on.)
    let tobi = is_tobi(dir);
    let ifs_root = format!("/{}", lib.to_ascii_lowercase());
    let incdir: String = tobi_include_path(dir)
        .iter()
        .map(|d| {
            format!(
                " '{ifs_root}/{}'",
                d.trim_start_matches("./").trim_end_matches('/')
            )
        })
        .collect();
    let incdir = if incdir.is_empty() {
        String::new()
    } else {
        format!(" INCDIR({})", incdir.trim())
    };
    let rel_of = |p: &Path| {
        p.strip_prefix(dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    // The library goes in front: its files and programs are found first.
    job.curlib = Some(lib.clone());
    if !catalog::library_exists(job.client, &lib).await? {
        step(job, &mut report, "library", &format!("CRTLIB LIB({lib})")).await;
    }

    // 1. Source files and members.
    let files: std::collections::BTreeSet<&str> = members.iter().map(|m| m.file.as_str()).collect();
    for f in &files {
        if !source::is_source_file(job.client, &lib, f).await? {
            step(
                job,
                &mut report,
                &format!("source file {f}"),
                &format!("CRTSRCPF FILE({lib}/{f})"),
            )
            .await;
        }
    }
    for m in &members {
        let text = read_text(&m.path).with_context(|| format!("reading {}", m.path.display()))?;
        source::put_member(
            job.client,
            &source::Member {
                library: lib.clone(),
                file: m.file.clone(),
                name: m.member.clone(),
                srctype: m.srctype.clone(),
                text: m.text.chars().take(50).collect(),
                source: text,
                ..Default::default()
            },
            true,
        )
        .await?;
    }
    report.lines.push(format!(
        "  {} source members in {}",
        members.len(),
        files.iter().copied().collect::<Vec<_>>().join(", ")
    ));

    // A TOBi project is built from the IFS: every source is there at its
    // path in the project (includes name each other by relative paths).
    if tobi {
        for p in project_sources(dir) {
            let path = format!("{ifs_root}/{}", rel_of(&p));
            source::put_stream_file(job.client, &path, &read_text(&p)?).await?;
        }
    }
    // Include files go to the stream files, under /<library>/ (RPG
    // `/INCLUDE 'name.rpgleinc'` finds them by name).
    for (rel, path) in &includes {
        let text = read_text(path).with_context(|| format!("reading {}", path.display()))?;
        source::put_stream_file(
            job.client,
            &format!("/{}/{rel}", lib.to_ascii_lowercase()),
            &text,
        )
        .await?;
    }
    if !includes.is_empty() {
        report.lines.push(format!(
            "  {} include files in /{}/",
            includes.len(),
            lib.to_ascii_lowercase()
        ));
    }
    for (name, kept, left) in &duplicates {
        report.lines.push(format!(
            "  {name}: two members of that name; kept {}, left out {}",
            kept.display(),
            left.display()
        ));
    }

    let of_type = |t: &'static str| members.iter().filter(move |m| m.srctype == t);

    // 2. Physical files and their data.
    for m in of_type("PF") {
        let existed =
            catalog::object_exists(job.client, std::slice::from_ref(&lib), &m.member, "*FILE")
                .await?;
        if existed {
            report.lines.push(format!(
                "  {}: file exists, kept with its records",
                m.member
            ));
            continue;
        }
        let cmd = format!(
            "CRTPF FILE({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
            m.member, m.file, m.member
        );
        if step(job, &mut report, &m.member, &cmd).await {
            if let Some(csv) = data.get(&m.member) {
                let text = read_text(csv)?;
                let f = catalog::resolve_file(job.client, std::slice::from_ref(&lib), &m.member)
                    .await?;
                match load_csv(job.client, f, &text).await {
                    Ok(n) => report
                        .lines
                        .push(format!("  {}: {n} records loaded", m.member)),
                    Err(e) => {
                        report.failed += 1;
                        report
                            .lines
                            .push(format!("  {}: loading data FAILED: {e}", m.member));
                    }
                }
            }
        }
    }
    // 2b. SQL scripts (RUNSQLSTM with ERRLVL(30): every statement runs,
    // and the ones that fail are reported). Objects go to the import
    // library: it is the default schema and SET SCHEMA is skipped. Scripts
    // that create tables run first.
    let mut scripts: Vec<&Found> = of_type("SQL").collect();
    let mut sources = BTreeMap::new();
    for m in &scripts {
        let src = source::find_member(job.client, std::slice::from_ref(&lib), &m.file, &m.member)
            .await?
            .source;
        sources.insert(m.member.clone(), src);
    }
    let creates_table = |src: &str| {
        crate::ddl::split_statements(src).iter().any(|st| {
            matches!(
                crate::ddl::parse(st),
                Ok(Some(crate::ddl::Ddl::CreateTable { .. }))
            )
        })
    };
    scripts.sort_by_key(|m| !creates_table(&sources[&m.member]));
    let mut sql_files = Vec::new();
    for m in scripts {
        let what = format!("member {} in {lib}/{}", m.member, m.file);
        match job
            .run_sql_script(&sources[&m.member], &what, 30, Some(lib.clone()), false)
            .await
        {
            Ok(lines) => {
                let failed: Vec<&String> = lines
                    .iter()
                    .filter(|l| l.contains(": SQL") && !l.contains("SET SCHEMA skipped"))
                    .collect();
                if failed.is_empty() {
                    report.lines.push(format!(
                        "  {}: {}",
                        m.member,
                        lines.last().cloned().unwrap_or_default()
                    ));
                } else {
                    report.failed += 1;
                    report.lines.push(format!(
                        "  {}: FAILED: {} (first: {})",
                        m.member,
                        lines.last().cloned().unwrap_or_default(),
                        failed[0]
                    ));
                }
            }
            Err(e) => {
                report.failed += 1;
                report.lines.push(format!("  {}: FAILED: {e:#}", m.member));
            }
        }
        for st in crate::ddl::split_statements(&sources[&m.member]) {
            if let Ok(Some(crate::ddl::Ddl::CreateTable { name, .. })) = crate::ddl::parse(&st) {
                sql_files.push(name.name);
            }
        }
    }
    // Data for tables created by the scripts.
    for name in &sql_files {
        let Some(csv) = data.get(name) else { continue };
        let Ok(f) = catalog::resolve_file(job.client, std::slice::from_ref(&lib), name).await
        else {
            continue;
        };
        let text = read_text(csv)?;
        match load_csv(job.client, f, &text).await {
            Ok(n) => report.lines.push(format!("  {name}: {n} records loaded")),
            Err(e) => {
                report.failed += 1;
                report
                    .lines
                    .push(format!("  {name}: loading data FAILED: {e}"));
            }
        }
    }

    // 3. Logical and display files.
    for (t, command) in [("LF", "CRTLF"), ("DSPF", "CRTDSPF"), ("PRTF", "CRTPRTF")] {
        for m in of_type(t) {
            if catalog::object_exists(job.client, std::slice::from_ref(&lib), &m.member, "*FILE")
                .await?
            {
                report
                    .lines
                    .push(format!("  {}: file exists, kept", m.member));
                continue;
            }
            let cmd = format!(
                "{command} FILE({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
                m.member, m.file, m.member
            );
            step(job, &mut report, &m.member, &cmd).await;
        }
    }
    // 4. ILE: modules (TOBi rules X.MODULE, X.module.rpgle, or source with
    // ctl-opt NOMAIN), service programs from binder source (.bnd, with the
    // modules its rule names), binding directories (.bnddir scripts).
    let mut is_module = std::collections::BTreeSet::new();
    for m in members
        .iter()
        .filter(|m| matches!(m.srctype.as_str(), "RPGLE" | "SQLRPGLE") && !m.included)
    {
        let rule = if rules.contains_key(&format!("{}.MODULE", m.member)) {
            Some("MODULE")
        } else {
            rules.get(&m.member).map(|r| r.0.as_str())
        };
        let nomain = read_text(&m.path)
            .map(|t| {
                let u = t.to_ascii_uppercase();
                u.lines().any(|l| {
                    let l = l.trim_start();
                    (l.starts_with("CTL-OPT") || l.starts_with('H') || u.starts_with("**FREE"))
                        && l.contains("NOMAIN")
                })
            })
            .unwrap_or(false);
        let module = match (rule, m.objtype.as_deref()) {
            (Some("PGM"), _) | (_, Some("pgm")) => false,
            (Some("MODULE"), _) | (_, Some("module")) => true,
            _ => nomain,
        };
        if !module {
            continue;
        }
        is_module.insert((m.file.clone(), m.member.clone()));
        let stmf = if tobi || includes_relative_paths(&m.path) {
            let path = format!("{ifs_root}/{}", rel_of(&m.path));
            source::put_stream_file(job.client, &path, &read_text(&m.path)?).await?;
            Some(path)
        } else {
            None
        };
        let cmd = if let Some(path) = &stmf {
            if m.srctype == "SQLRPGLE" {
                format!(
                    "CRTSQLRPGI OBJ({lib}/{}) SRCSTMF('{path}') OBJTYPE(*MODULE){}{incdir}",
                    m.member,
                    if tobi { " COMMIT(*NONE)" } else { "" }
                )
            } else {
                format!(
                    "CRTRPGMOD MODULE({lib}/{}) SRCSTMF('{path}'){incdir}",
                    m.member
                )
            }
        } else if m.srctype == "SQLRPGLE" {
            format!(
                "CRTSQLRPGI OBJ({lib}/{}) SRCFILE({lib}/{}) SRCMBR({}) OBJTYPE(*MODULE)",
                m.member, m.file, m.member
            )
        } else {
            format!(
                "CRTRPGMOD MODULE({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
                m.member, m.file, m.member
            )
        };
        step(job, &mut report, &m.member, &cmd).await;
    }
    // Service programs after those they bind (BNDSRVPGM), as make builds
    // a rule's prerequisites first.
    let mut bnds: Vec<&Found> = of_type("BND").collect();
    {
        let srv_deps = |m: &Found| -> Vec<String> {
            match rules.get(&m.member) {
                Some((kind, deps)) if kind == "SRVPGM" => deps
                    .iter()
                    .filter_map(|d| {
                        d.to_ascii_uppercase()
                            .strip_suffix(".SRVPGM")
                            .map(String::from)
                    })
                    .collect(),
                _ => vec![],
            }
        };
        let names: Vec<String> = bnds.iter().map(|m| m.member.clone()).collect();
        let mut ordered: Vec<&Found> = Vec::new();
        let mut done: std::collections::BTreeSet<String> = Default::default();
        while !bnds.is_empty() {
            let pick = bnds
                .iter()
                .position(|m| {
                    srv_deps(m)
                        .iter()
                        .all(|d| done.contains(d) || !names.contains(d))
                })
                .unwrap_or(0);
            let m = bnds.remove(pick);
            done.insert(m.member.clone());
            ordered.push(m);
        }
        bnds = ordered;
    }
    for m in bnds {
        let deps_of = |suffix: &str| -> Vec<String> {
            match rules.get(&m.member) {
                Some((kind, deps)) if kind == "SRVPGM" => deps
                    .iter()
                    .filter_map(|d| {
                        d.to_ascii_uppercase()
                            .strip_suffix(suffix)
                            .map(String::from)
                    })
                    .map(|d| format!("{lib}/{d}"))
                    .collect(),
                _ => vec![],
            }
        };
        let mut modules = deps_of(".MODULE");
        if modules.is_empty() {
            modules.push(format!("{lib}/{}", m.member));
        }
        let srvpgms = deps_of(".SRVPGM");
        let cmd = format!(
            "CRTSRVPGM SRVPGM({lib}/{}) MODULE({}) SRCFILE({lib}/{}) SRCMBR({}){}",
            m.member,
            modules.join(" "),
            m.file,
            m.member,
            if srvpgms.is_empty() {
                String::new()
            } else {
                format!(" BNDSRVPGM({})", srvpgms.join(" "))
            }
        );
        let cmd = match rules
            .get(&format!("{}.SRVPGM:TEXT", m.member))
            .and_then(|r| r.1.first())
        {
            Some(t) => format!("{cmd} TEXT('{}')", t.replace('\'', "''")),
            None => cmd,
        };
        step(job, &mut report, &m.member, &cmd).await;
    }
    // A source named NAME.srvpgm.rpgle with no binder source of its name:
    // a service program of its module exporting all its procedures.
    for m in members.iter().filter(|m| {
        m.objtype.as_deref() == Some("srvpgm")
            && matches!(m.srctype.as_str(), "RPGLE" | "SQLRPGLE" | "CLLE")
            && !members
                .iter()
                .any(|b| b.srctype == "BND" && b.member == m.member)
            && !rules.contains_key(&m.member)
    }) {
        let cmd = format!(
            "CRTSRVPGM SRVPGM({lib}/{0}) MODULE({lib}/{0}) EXPORT(*ALL)",
            m.member
        );
        step(job, &mut report, &m.member, &cmd).await;
    }
    // Programs bound from modules (TOBi `X.PGM: X.MODULE Y.SRVPGM`), with
    // the rule's TEXT and ACTGRP.
    for (name, (kind, deps)) in rules.iter() {
        if kind != "PGM" || name.contains('.') || name.contains(':') {
            continue;
        }
        let pick = |suffix: &str| -> Vec<String> {
            deps.iter()
                .filter_map(|d| {
                    d.to_ascii_uppercase()
                        .strip_suffix(suffix)
                        .map(String::from)
                })
                .map(|d| format!("{lib}/{d}"))
                .collect()
        };
        let modules = pick(".MODULE");
        if modules.is_empty() {
            continue;
        }
        let srvpgms = pick(".SRVPGM");
        let var = |v: &str| {
            rules
                .get(&format!("{name}.PGM:{v}"))
                .and_then(|r| r.1.first().cloned())
        };
        let mut cmd = format!("CRTPGM PGM({lib}/{name}) MODULE({})", modules.join(" "));
        if !srvpgms.is_empty() {
            cmd.push_str(&format!(" BNDSRVPGM({})", srvpgms.join(" ")));
        }
        if let Some(a) = var("ACTGRP") {
            cmd.push_str(&format!(" ACTGRP({a})"));
        }
        if let Some(t) = var("TEXT") {
            cmd.push_str(&format!(" TEXT('{}')", t.replace('\'', "''")));
        }
        step(job, &mut report, name, &cmd).await;
    }
    for m in of_type("BNDDIR") {
        // A TOBi binding directory script: CL commands with &O (the
        // library) and &N (the object); a line starting with ! may fail.
        let text = read_text(&m.path)?;
        let mut ok = true;
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with("/*") {
                continue;
            }
            let (may_fail, l) = match l.strip_prefix('!') {
                Some(r) => (true, r.trim()),
                None => (false, l),
            };
            let c = l.replace("&O", &lib).replace("&N", &m.member);
            if let Err(e) = job.run(&c).await {
                if !may_fail {
                    ok = false;
                    report.failed += 1;
                    report
                        .lines
                        .push(format!("  {}: FAILED: {c}: {e:#}", m.member));
                    break;
                }
            }
        }
        if ok {
            report.lines.push(format!(
                "  {}: binding directory created in {lib}.",
                m.member
            ));
        }
    }

    // 5a. Commands, before the programs: a CL program's commands must
    // exist when it is compiled; the command processing program is the
    // program of the same name (as build tools for IBM i name them by
    // default), and need not exist yet.
    for m in of_type("CMD") {
        let cmd = format!(
            "CRTCMD CMD({lib}/{}) PGM({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
            m.member, m.member, m.file, m.member
        );
        step(job, &mut report, &m.member, &cmd).await;
    }
    // 5. Programs (replaced: they are built from the source just added).
    for (t, command) in [
        ("CLLE", "CRTBNDCL"),
        ("CLP", "CRTCLPGM"),
        ("RPGLE", "CRTBNDRPG"),
        ("SQLRPGLE", "CRTSQLRPGI"),
        ("RPG", "CRTRPGPGM"),
    ] {
        for m in of_type(t) {
            if m.included || is_module.contains(&(m.file.clone(), m.member.clone())) {
                continue;
            }
            let target = if command == "CRTSQLRPGI" {
                "OBJ"
            } else {
                "PGM"
            };
            let cmd = if t == "CLLE" && (tobi || cl_includes_relative_paths(&m.path)) {
                // From the IFS, where its INCLUDE SRCSTMF paths resolve
                // from the source's directory.
                let path = format!("{ifs_root}/{}", rel_of(&m.path));
                source::put_stream_file(job.client, &path, &read_text(&m.path)?).await?;
                format!("{command} {target}({lib}/{}) SRCSTMF('{path}')", m.member)
            } else if matches!(t, "RPGLE" | "SQLRPGLE")
                && (tobi || includes_relative_paths(&m.path))
            {
                let path = format!("{ifs_root}/{}", rel_of(&m.path));
                source::put_stream_file(job.client, &path, &read_text(&m.path)?).await?;
                // makei's defaults (TOBi def_rules.mk): COMMIT(*NONE) for
                // SQL, DFTACTGRP(*NO) for bound programs.
                let extra = match command {
                    "CRTSQLRPGI" if tobi => " COMMIT(*NONE)",
                    "CRTBNDRPG" if tobi => " DFTACTGRP(*NO)",
                    _ => "",
                };
                format!(
                    "{command} {target}({lib}/{}) SRCSTMF('{path}'){extra}{incdir}",
                    m.member
                )
            } else {
                format!(
                    "{command} {target}({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
                    m.member, m.file, m.member
                )
            };
            step(job, &mut report, &m.member, &cmd).await;
        }
    }
    for m in members.iter().filter(|m| {
        !matches!(
            m.srctype.as_str(),
            "PF" | "LF"
                | "CMD"
                | "DSPF"
                | "PRTF"
                | "CLLE"
                | "CLP"
                | "RPGLE"
                | "SQLRPGLE"
                | "RPG"
                | "SQL"
                | "TXT"
                | "BND"
                | "BNDDIR"
        )
    }) {
        report.lines.push(format!(
            "  {}: {} source kept; as4tainr cannot create {} objects yet",
            m.member, m.srctype, m.srctype
        ));
    }
    for name in data.keys() {
        if !members
            .iter()
            .any(|m| m.srctype == "PF" && &m.member == name)
            && !sql_files.contains(name)
        {
            report.lines.push(format!(
                "  {name}.csv: no PF member of that name, not loaded"
            ));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_in_windows_1252() {
        // Not UTF-8: read as Windows-1252 (0x93/0x94 are its curly double
        // quotes, 0xE9 is e acute in it and in ISO 8859-1).
        let p = std::env::temp_dir().join(format!("as4tainr-1252-{}", std::process::id()));
        std::fs::write(&p, b"caf\xe9 \x93x\x94").unwrap();
        let t = read_text(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(t, "caf\u{e9} \u{201c}x\u{201d}");
    }

    #[test]
    fn csv_lines() {
        assert_eq!(
            parse_csv_line(r#"1,"Acme, Inc.","say ""hi""""#),
            vec!["1", "Acme, Inc.", r#"say "hi""#]
        );
    }

    #[test]
    fn member_names() {
        // TOBi naming (IBM's open-source build tool for IBM i):
        // OBJECT-Description.objtype.srctype.
        assert_eq!(
            member_of("SIMPLESFL1-AS400_RPG_subfile.pgm.rpgle"),
            Some((
                "SIMPLESFL1".into(),
                "AS400 RPG subfile".into(),
                "rpgle".into()
            ))
        );
        assert_eq!(
            member_of("CUSTMAST.PF"),
            Some(("CUSTMAST".into(), String::new(), "pf".into()))
        );
        assert_eq!(member_of("TOO_LONG_A_NAME.rpgle"), None);
    }

    #[test]
    fn source_files_by_type() {
        // The source files IBM's create commands default to (SRCFILE).
        assert_eq!(default_source_file("PF"), "QDDSSRC");
        assert_eq!(default_source_file("RPGLE"), "QRPGLESRC");
        assert_eq!(default_source_file("CLLE"), "QCLSRC");
    }

    #[test]
    fn build_scripts_and_copy_members() {
        // OSSILE's projects build with Makefiles and setup scripts that
        // run `system "CRTSRVPGM ..."`: LUTIL from the modules LUTIL and
        // PARMEVAL, bound to the service program LLIST. Sources other
        // sources /COPY by file name or member name (llist_h.rpgle,
        // LUTIL_H, arraylist_h.rpgle, whose name is too long for a
        // member) are copy members, not programs.
        let dir = std::env::temp_dir().join(format!("as4tainr-mk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("setup"),
            "system -kpieb \"CRTSRVPGM $TARGET_LIB/LUTIL MODULE($TARGET_LIB/LUTIL $TARGET_LIB/PARMEVAL) BNDSRVPGM($TARGET_LIB/LLIST) EXPORT(*SRCFILE)\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Makefile"),
            "\tsystem \"CRTSRVPGM $(BIN_LIB)/LLIST MODULE($(BIN_LIB)/LLIST $(BIN_LIB)/LLIST_SORT) $(LFLAGS)\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("llist.rpgle"),
            "      /copy llist_h.rpgle\n      /copy LUTIL_H\n      /include 'arraylist_h.rpgle'\n",
        )
        .unwrap();
        std::fs::write(dir.join("llist_h.rpgle"), "\n").unwrap();
        std::fs::write(dir.join("lutil_h.rpgle"), "\n").unwrap();
        std::fs::write(dir.join("arraylist_h.rpgle"), "\n").unwrap();
        let s = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            s.rules.get("LUTIL"),
            Some(&(
                "SRVPGM".to_string(),
                vec![
                    "LUTIL.MODULE".to_string(),
                    "PARMEVAL.MODULE".to_string(),
                    "LLIST.SRVPGM".to_string()
                ]
            ))
        );
        assert_eq!(
            s.rules.get("LLIST").map(|r| r.1.clone()),
            Some(vec![
                "LLIST.MODULE".to_string(),
                "LLIST_SORT.MODULE".to_string()
            ])
        );
        let included: Vec<(&str, bool)> = s
            .members
            .iter()
            .map(|m| (m.member.as_str(), m.included))
            .collect();
        assert!(included.contains(&("LLIST", false)), "{included:?}");
        assert!(included.contains(&("LLIST_H", true)), "{included:?}");
        assert!(included.contains(&("LUTIL_H", true)), "{included:?}");
        let mut inc: Vec<String> = s.includes.iter().map(|x| x.0.clone()).collect();
        inc.sort();
        assert_eq!(inc, ["arraylist_h.rpgle", "llist_h.rpgle"]);
    }

    #[test]
    fn sources_of_one_name_and_build_rule_names() {
        // The company_system corpus (IBM/ibmi-company_system): qrpglesrc
        // holds empdet.sqlrpgle and empdet.bnd. A source file holds one
        // member of a name (IBM i), so the binder source goes to QSRVSRC,
        // where CRTSRVPGM looks for it (SRCFILE default). Its Rules.mk
        // builds TEMPDET.MODULE from empdet.test.sqlrpgle: the member
        // takes the object's name, as the build tool's CRTSQLRPGI would.
        let dir = std::env::temp_dir().join(format!("as4tainr-dup-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("qrpglesrc")).unwrap();
        std::fs::create_dir_all(dir.join("qtestsrc")).unwrap();
        std::fs::write(dir.join("qrpglesrc/empdet.sqlrpgle"), "**free\n").unwrap();
        std::fs::write(dir.join("qrpglesrc/empdet.bnd"), "STRPGMEXP\nENDPGMEXP\n").unwrap();
        std::fs::write(dir.join("qtestsrc/empdet.test.sqlrpgle"), "**free\n").unwrap();
        std::fs::write(
            dir.join("qtestsrc/Rules.mk"),
            "TEMPDET.MODULE: empdet.test.sqlrpgle\n",
        )
        .unwrap();
        let s = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let mut m: Vec<(&str, &str, &str)> = s
            .members
            .iter()
            .map(|m| (m.file.as_str(), m.member.as_str(), m.srctype.as_str()))
            .collect();
        m.sort();
        assert_eq!(
            m,
            [
                ("QRPGLESRC", "EMPDET", "SQLRPGLE"),
                ("QSRVSRC", "EMPDET", "BND"),
                ("QTESTSRC", "TEMPDET", "SQLRPGLE"),
            ]
        );
        assert!(s.duplicates.is_empty());
    }

    #[test]
    fn include_files_in_source_file_folders_are_members() {
        // A copy member lives in a source file (ILE RPG Reference, /COPY
        // file,member): QCPYSRC/NWUTILPR.rpgleinc is member NWUTILPR of
        // QCPYSRC, and still an include file for /INCLUDE 'path'.
        let dir = std::env::temp_dir().join(format!("as4tainr-inc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("qcpysrc")).unwrap();
        std::fs::write(dir.join("qcpysrc/nwutilpr.rpgleinc"), "dcl-pr x; end-pr;\n").unwrap();
        std::fs::write(dir.join("loose.rpgleinc"), "\n").unwrap();
        let s = scan(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(s.includes.len(), 2);
        let m: Vec<(&str, &str, &str)> = s
            .members
            .iter()
            .map(|m| (m.file.as_str(), m.member.as_str(), m.srctype.as_str()))
            .collect();
        assert_eq!(m, [("QCPYSRC", "NWUTILPR", "RPGLEINC")]);
    }

    /// makei's Rules.mk: a service program and its module of one name
    /// (REPL_PM.MODULE, REPL_PM.SRVPGM), and target variables
    /// (`X.SRVPGM: private TEXT := ...`), which are no rules.
    #[test]
    fn rules_keep_service_program_and_module_of_one_name() {
        let mut r = BTreeMap::new();
        read_rules(
            "REPL_PM.MODULE: REPL_PM.RPGLE\nREPL_PM.SRVPGM: private TEXT := Program messages\nREPL_PM.SRVPGM: REPL_PM.BND REPL_PM.MODULE\nREPL_USR.SRVPGM: REPL_USR.BND REPL_USR.MODULE REPL_PM.SRVPGM\nREPL_USR.MODULE: REPL_USR.RPGLE\n",
            &mut r,
        );
        assert_eq!(
            r.get("REPL_PM"),
            Some(&(
                "SRVPGM".to_string(),
                vec!["REPL_PM.BND".to_string(), "REPL_PM.MODULE".to_string()]
            ))
        );
        assert_eq!(r.get("REPL_USR").map(|x| x.1.len()), Some(3));
        assert!(r.contains_key("REPL_PM.MODULE") && r.contains_key("REPL_USR.MODULE"));
    }
}
