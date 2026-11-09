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
