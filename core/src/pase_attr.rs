//! PASE's `attr` and `setccsid` utilities for SSH and QSH commands: an
//! IFS object's attributes (CCSID, DATA_SIZE, OBJTYPE) shown, and its
//! CCSID set.
//!
//! As IBM documents them (PASE commands): `attr [-hp] path [name[=value]
//! ...]` shows the named attributes' values one per line (`-p`: as
//! NAME=value), or sets those given a value; `setccsid ccsid file...`
//! tags files with a CCSID. Code for IBM i runs `attr path CCSID=1208` on
//! a file it made and `attr -p path DATA_SIZE OBJTYPE` (a directory's
//! DATA_SIZE is 8192 there: its tests, content.test.ts "should get
//! attributes").
//!
//! The command runs on the IFS's mirror (qshfs.rs), where a file the same
//! command made is not in the catalog yet: a CCSID set during an SSH
//! command is noted in the file AS4TAINR_ATTR_NOTES names and applied
//! when the mirror is written back (sshd.rs); without it, at once.

use anyhow::{bail, Result};
use tokio_postgres::Client;

/// What a utility wrote and its exit status.
#[derive(Debug, Default)]
pub struct Output {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub status: u8,
}

/// The CCSIDs noted for paths in this command (the last for each).
fn noted(path: &str) -> Option<i32> {
    let f = std::env::var("AS4TAINR_ATTR_NOTES").ok()?;
    let text = std::fs::read_to_string(f).ok()?;
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(p, _)| *p == path)
        .filter_map(|(_, c)| c.trim().parse().ok())
        .next_back()
}

fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        return crate::ifs::normalize(path).unwrap_or_else(|_| path.to_string());
    }
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".into());
    let full = format!("{}/{path}", cwd.trim_end_matches('/'));
    crate::ifs::normalize(&full).unwrap_or(full)
}

async fn set_ccsid(client: &Client, path: &str, ccsid: i32) -> Result<()> {
    if let Ok(f) = std::env::var("AS4TAINR_ATTR_NOTES") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(f)?;
        writeln!(file, "{path}\t{ccsid}")?;
        return Ok(());
    }
    crate::ifs::set_ccsid(client, path, ccsid).await
}

async fn value(
    client: &Client,
    path: &str,
    meta: &std::fs::Metadata,
    name: &str,
) -> Result<String> {
    Ok(match name {
        "OBJTYPE" => if meta.is_dir() {
            "*DIR"
        } else if meta.file_type().is_symlink() {
            "*SYMLNK"
        } else {
            "*STMF"
        }
        .into(),
        "DATA_SIZE" => {
            if meta.is_dir() {
                "8192".into()
            } else {
                meta.len().to_string()
            }
        }
        "CCSID" => {
            if let Some(c) = noted(path) {
                c.to_string()
            } else {
                match crate::ifs::stat(client, path).await? {
                    Some(e) => e.ccsid.to_string(),
                    // Made by this command: the mirror tags it 1208.
                    None => "1208".into(),
                }
            }
        }
        other => bail!("{other}"),
    })
}

/// `attr [-hp] path [name[=value] ...]`.
pub async fn attr(client: &Client, args: &[String]) -> Output {
    let mut out = Output::default();
    let mut pairs = false;
    let mut rest: Vec<&String> = Vec::new();
    for a in args {
        if rest.is_empty() && a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'p' => pairs = true,
                    'h' => {}
                    _ => {
                        out.stderr.push(format!("attr: -{c}: option not valid"));
                        out.status = 1;
                        return out;
                    }
                }
            }
            continue;
        }
        rest.push(a);
    }
    let Some((path, names)) = rest.split_first() else {
        out.stderr
            .push("usage: attr [-hp] path [attribute[=value] ...]".into());
        out.status = 2;
        return out;
    };
    let path = absolute(path);
    // A QSYS.LIB object: its attributes from the catalog (a command
    // through Qshell's standard input has no /QSYS.LIB tree: Code for IBM
    // i's getAttributes of a member).
    if path.to_ascii_uppercase().starts_with("/QSYS.LIB") {
        return qsys_attr(client, &path, names, pairs).await;
    }
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => {
            out.stderr
                .push(format!("attr: {path}: No such path or directory."));
            out.status = 1;
            return out;
        }
    };
    let all = ["OBJTYPE", "DATA_SIZE", "CCSID"];
    let mut wanted: Vec<String> = if names.is_empty() {
        all.iter().map(|s| s.to_string()).collect()
    } else {
        names.iter().map(|s| s.to_string()).collect()
    };
    // Shown in attr's own order, not the order asked (PUB400 2026-11-09,
    // verify64 m64_attr: asked DATA_SIZE OBJTYPE CCSID, shown OBJTYPE,
    // DATA_SIZE, CCSID).
    let rank = |n: &str| match n
        .split('=')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "OBJTYPE" => 0,
        "DATA_SIZE" => 1,
        "CCSID" => 2,
        _ => 3,
    };
    wanted.sort_by_key(|n| rank(n));
    for w in wanted {
        if let Some((name, v)) = w.split_once('=') {
            let name = name.trim().to_ascii_uppercase();
            if name != "CCSID" {
                out.stderr
                    .push(format!("attr: {name}: the attribute cannot be set here."));
                out.status = 1;
                continue;
            }
            match v.trim().parse::<i32>() {
                Ok(c) => {
                    if let Err(e) = set_ccsid(client, &path, c).await {
                        out.stderr.push(format!("attr: {path}: {e:#}"));
                        out.status = 1;
                    }
                }
                Err(_) => {
                    out.stderr.push(format!("attr: {v}: not a valid CCSID."));
                    out.status = 1;
                }
            }
            continue;
        }
        let name = w.trim().to_ascii_uppercase();
        match value(client, &path, &meta, &name).await {
            Ok(v) if pairs => out.stdout.push(format!("{name}={v}")),
            Ok(v) => out.stdout.push(v),
            Err(_) => {
                out.stderr
                    .push(format!("attr: {name}: not a valid attribute."));
                out.status = 1;
            }
        }
    }
    out
}

/// `setccsid ccsid file...`.
pub async fn setccsid(client: &Client, args: &[String]) -> Output {
    let mut out = Output::default();
    let args: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let (Some(c), files) = (args.first(), &args[1.min(args.len())..]) else {
        out.stderr.push("usage: setccsid ccsid file ...".into());
        out.status = 2;
        return out;
    };
    let Ok(ccsid) = c.parse::<i32>() else {
        out.stderr
            .push(format!("setccsid: {c}: not a valid CCSID."));
        out.status = 1;
        return out;
    };
    for f in files {
        let path = absolute(f);
        if std::fs::symlink_metadata(&path).is_err() {
            out.stderr
                .push(format!("setccsid: {path}: No such path or directory."));
            out.status = 1;
            continue;
        }
        if let Err(e) = set_ccsid(client, &path, ccsid).await {
            out.stderr.push(format!("setccsid: {path}: {e:#}"));
            out.status = 1;
        }
    }
    out
}

/// After an SSH command: the CCSIDs it set, on the files written back.
pub async fn apply_notes(client: &Client, notes: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(notes) else {
        return;
    };
    for (p, c) in text.lines().filter_map(|l| l.split_once('\t')) {
        if let Ok(c) = c.trim().parse::<i32>() {
            let _ = crate::ifs::set_ccsid(client, p, c).await;
        }
    }
    let _ = std::fs::remove_file(notes);
}

/// `attr` of a QSYS.LIB path: OBJTYPE (*LIB, *FILE, *MBR, the object's
/// type), DATA_SIZE and CCSID (PUB400 2026-11-10, verify72 s72_attr: a
/// member OBJTYPE=*MBR, a source file OBJTYPE=*FILE DATA_SIZE=16384, the
/// CCSID the file's data has; as4tainr's source files are CCSID 37).
async fn qsys_attr(client: &Client, path: &str, names: &[&String], pairs: bool) -> Output {
    let mut out = Output::default();
    let parts: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let p = p.to_ascii_uppercase();
            let (n, ext) = p
                .rsplit_once('.')
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .unwrap_or((p.clone(), String::new()));
            (n.trim_matches('"').to_string(), ext)
        })
        .map(|(n, e)| if e.is_empty() { n } else { format!("{n}.{e}") })
        .collect();
    let split = |p: &str| -> (String, String) {
        p.rsplit_once('.')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((p.to_string(), String::new()))
    };
    let missing = |out: &mut Output| {
        out.stderr
            .push(format!("attr: {path}: No such path or directory."));
        out.status = 1;
    };
    let found: Option<(String, String, String)> = match parts.as_slice() {
        [q] if q == "QSYS.LIB" => Some(("*LIB".into(), "8192".into(), "37".into())),
        [_, lib] => {
            let (l, e) = split(lib);
            let lib = crate::qtemp::real(&l);
            let n = client
                .query_opt(
                    "SELECT 1 FROM as4tainr.objects WHERE library = 'QSYS' AND objtype = '*LIB' AND name = $1",
                    &[&lib],
                )
                .await
                .ok()
                .flatten();
            (e == "LIB" && n.is_some()).then(|| ("*LIB".into(), "8192".into(), "37".into()))
        }
        [_, lib, obj] => {
            let (l, _) = split(lib);
            let (o, t) = split(obj);
            let ty = format!("*{t}");
            let lib = crate::qtemp::real(&l);
            client
                .query_opt(
                    "SELECT 1 FROM as4tainr.objects WHERE library = $1 AND name = $2 AND objtype = $3",
                    &[&lib, &o, &ty],
                )
                .await
                .ok()
                .flatten()
                .map(|_| {
                    let size = if t == "FILE" { "16384" } else { "8192" };
                    (ty.clone(), size.to_string(), String::new())
                })
        }
        [_, lib, file, mbr] => {
            let (l, _) = split(lib);
            let (f, _) = split(file);
            let (m, e) = split(mbr);
            let lib = crate::qtemp::real(&l);
            let r = client
                .query_opt(
                    "SELECT m.source, coalesce(sf.rcdlen, 92) FROM as4tainr.source_members m
                       LEFT JOIN as4tainr.source_files sf ON sf.library = m.library AND sf.name = m.file
                      WHERE m.library = $1 AND m.file = $2 AND m.member = $3",
                    &[&lib, &f, &m],
                )
                .await
                .ok()
                .flatten();
            match (e.as_str(), r) {
                ("MBR", Some(r)) => {
                    let source: String = r.get(0);
                    let rcdlen: i32 = r.get(1);
                    let n = source.lines().count() as i64;
                    let size = n * (rcdlen as i64 - 12).max(0);
                    Some(("*MBR".into(), size.to_string(), "37".into()))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let Some((objtype, size, mut ccsid)) = found else {
        missing(&mut out);
        return out;
    };
    // A file's and its members' CCSID: CRTSRCPF's (37 by default).
    if ccsid.is_empty() || objtype == "*MBR" {
        ccsid = match parts.as_slice() {
            [_, lib, file, ..] => {
                let l = lib.rsplit_once('.').map(|(a, _)| a).unwrap_or(lib);
                let f = file.rsplit_once('.').map(|(a, _)| a).unwrap_or(file);
                crate::source::file_ccsid(client, &crate::qtemp::real(l), f)
                    .await
                    .to_string()
            }
            _ => "37".into(),
        };
    }
    let mut wanted: Vec<String> = if names.is_empty() {
        vec!["OBJTYPE".into(), "DATA_SIZE".into(), "CCSID".into()]
    } else {
        names
            .iter()
            .map(|s| s.trim().to_ascii_uppercase())
            .collect()
    };
    let rank = |n: &str| match n {
        "OBJTYPE" => 0,
        "DATA_SIZE" => 1,
        "CCSID" => 2,
        _ => 3,
    };
    wanted.sort_by_key(|n| rank(n));
    for name in wanted {
        let v = match name.as_str() {
            "OBJTYPE" => objtype.clone(),
            "DATA_SIZE" => size.clone(),
            "CCSID" => ccsid.clone(),
            _ => {
                out.stderr
                    .push(format!("attr: {name}: not a valid attribute."));
                out.status = 1;
                continue;
            }
        };
        out.stdout
            .push(if pairs { format!("{name}={v}") } else { v });
    }
    out
}
