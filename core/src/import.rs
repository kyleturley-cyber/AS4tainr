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
        let Some((name, kind)) = target.trim().rsplit_once('.') else {
            continue;
        };
        if name.contains(' ') || name.contains('$') || name.contains('=') {
            continue;
        }
        out.insert(
            name.to_ascii_uppercase(),
            (
                kind.to_ascii_uppercase(),
                deps.split_whitespace().map(|d| d.to_string()).collect(),
            ),
        );
    }
}

/// Walk DIR and its folders. A folder named like a source file
/// (`QRPGLESRC`) puts its members in that file; other folders (project
/// folders) are only for grouping.
fn scan(dir: &Path) -> Result<Scan> {
    let mut out = Scan::default();
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
                if let Ok(t) = std::fs::read_to_string(p) {
                    read_rules(&t, &mut out.rules);
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
                continue;
            }
            let Some((name, text, ext)) = member_of(fname) else {
                continue;
            };
            if ext == "csv" {
                out.data.insert(name, p.clone());
                continue;
            }
            let Some(srctype) = source::type_for_extension(&ext) else {
                continue;
            };
            let file = file_of_folder
                .clone()
                .unwrap_or_else(|| default_source_file(srctype).to_string());
            if let Some(first) = out
                .members
                .iter()
                .find(|m| m.file == file && m.member == name)
            {
                out.duplicates.push((name, first.path.clone(), p.clone()));
                continue;
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
            });
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
        let text = std::fs::read_to_string(&m.path)
            .with_context(|| format!("reading {}", m.path.display()))?;
        source::put_member(
            job.client,
            &source::Member {
                library: lib.clone(),
                file: m.file.clone(),
                name: m.member.clone(),
                srctype: m.srctype.clone(),
                text: m.text.chars().take(50).collect(),
                source: text,
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

    // Include files go to the stream files, under /<library>/ (RPG
    // `/INCLUDE 'name.rpgleinc'` finds them by name).
    for (rel, path) in &includes {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
                let text = std::fs::read_to_string(csv)?;
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
        let text = std::fs::read_to_string(csv)?;
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
        .filter(|m| matches!(m.srctype.as_str(), "RPGLE" | "SQLRPGLE"))
    {
        let rule = rules.get(&m.member).map(|r| r.0.as_str());
        let nomain = std::fs::read_to_string(&m.path)
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
        let cmd = if m.srctype == "SQLRPGLE" {
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
    for m in of_type("BND") {
        let modules: Vec<String> = match rules.get(&m.member) {
            Some((kind, deps)) if kind == "SRVPGM" => deps
                .iter()
                .filter_map(|d| {
                    d.to_ascii_uppercase()
                        .strip_suffix(".MODULE")
                        .map(String::from)
                })
                .map(|d| format!("{lib}/{d}"))
                .collect(),
            _ => vec![format!("{lib}/{}", m.member)],
        };
        let cmd = format!(
            "CRTSRVPGM SRVPGM({lib}/{}) MODULE({}) SRCFILE({lib}/{}) SRCMBR({})",
            m.member,
            modules.join(" "),
            m.file,
            m.member
        );
        step(job, &mut report, &m.member, &cmd).await;
    }
    for m in of_type("BNDDIR") {
        // A TOBi binding directory script: CL commands with &O (the
        // library) and &N (the object); a line starting with ! may fail.
        let text = std::fs::read_to_string(&m.path)?;
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

    // 5. Programs (replaced: they are built from the source just added).
    for (t, command) in [
        ("CLLE", "CRTBNDCL"),
        ("CLP", "CRTCLPGM"),
        ("RPGLE", "CRTBNDRPG"),
        ("SQLRPGLE", "CRTSQLRPGI"),
        ("RPG", "CRTRPGPGM"),
    ] {
        for m in of_type(t) {
            if is_module.contains(&(m.file.clone(), m.member.clone())) {
                continue;
            }
            let target = if command == "CRTSQLRPGI" {
                "OBJ"
            } else {
                "PGM"
            };
            let cmd = format!(
                "{command} {target}({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
                m.member, m.file, m.member
            );
            step(job, &mut report, &m.member, &cmd).await;
        }
    }
    // 6. Commands: the command processing program is the program of the
    // same name (as build tools for IBM i name them by default).
    for m in of_type("CMD") {
        let cmd = format!(
            "CRTCMD CMD({lib}/{}) PGM({lib}/{}) SRCFILE({lib}/{}) SRCMBR({})",
            m.member, m.member, m.file, m.member
        );
        step(job, &mut report, &m.member, &cmd).await;
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
}
