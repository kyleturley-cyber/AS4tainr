//! CL commands for the integrated file system: MKDIR (MD, CRTDIR),
//! RMVDIR (RMDIR, RD), RMVLNK (DEL, ERASE), RNM, WRKLNK and DSPLNK,
//! CPYTOSTMF and CPYFRMSTMF.
//!
//! Written from IBM's CL command reference. Errors programs monitor for:
//! CPFA0A0 (the object already exists), CPFA0A9 (the object is not
//! found), CPFA0AB (the operation failed for the object).

use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use crate::ifs::{self, codec, errno, errno_of};
use anyhow::{bail, Result};

/// The CL error for an IFS error on an object.
fn cl_error(e: anyhow::Error, path: &str) -> anyhow::Error {
    let n = errno_of(&e);
    let reason = ifs::errno_text(n);
    match n {
        errno::EEXIST => anyhow::anyhow!("CPFA0A0: Object {path} already exists."),
        errno::ENOENT => anyhow::anyhow!("CPFA0A9: Object {path} not found."),
        _ => anyhow::anyhow!("CPFA0AB: The operation failed for object {path}: {reason}"),
    }
}

/// A library member path: /QSYS.LIB/LIB.LIB/FILE.FILE/MBR.MBR.
/// A save file's path: /QSYS.LIB/lib.LIB/file.FILE.
fn savf_path(p: &str) -> Option<(String, String)> {
    let up = p.trim().to_ascii_uppercase();
    let parts: Vec<&str> = up.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["QSYS.LIB", lib, file] => Some((
            lib.strip_suffix(".LIB")?.to_string(),
            file.strip_suffix(".FILE")?.to_string(),
        )),
        _ => None,
    }
}

fn member_path(p: &str) -> Result<(String, String, String)> {
    let up = p.trim().to_ascii_uppercase();
    let parts: Vec<&str> = up.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["QSYS.LIB", lib, file, mbr] => {
            let strip = |s: &str, ext: &str| s.strip_suffix(ext).map(|x| x.to_string());
            match (strip(lib, ".LIB"), strip(file, ".FILE"), strip(mbr, ".MBR")) {
                (Some(l), Some(f), Some(m)) => Ok((l, f, m)),
                _ => bail!(
                    "CPFA0AB: {p} is not a member path (/QSYS.LIB/lib.LIB/file.FILE/member.MBR)."
                ),
            }
        }
        _ => bail!("CPFA0AB: {p} is not a member path (/QSYS.LIB/lib.LIB/file.FILE/member.MBR)."),
    }
}

/// A CCSID parameter: a number or a special value.
fn ccsid_param(v: &str, default: i32) -> Result<i32> {
    Ok(match v.trim().to_ascii_uppercase().as_str() {
        "" | "*STMF" | "*FILE" | "*JOB" | "*SAME" => default,
        "*PCASCII" => 1252,
        "*STDASCII" => 819,
        "*UTF8" => 1208,
        n => match n.parse::<i32>() {
            Ok(c) if codec::supported(c) => c,
            _ => bail!("CPFA0AB: CCSID {v} is not supported for conversion."),
        },
    })
}

fn line_end(v: &str) -> Result<&'static str> {
    Ok(match v.trim().to_ascii_uppercase().as_str() {
        "" | "*CRLF" => "\r\n",
        "*LF" => "\n",
        "*CR" => "\r",
        "*LFCR" => "\n\r",
        other => bail!("ENDLINFMT({other}) is not supported."),
    })
}

impl Job<'_> {
    pub(crate) async fn ifs_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        let name = cmd.def.name;
        let msg = |m: String| Ok(Some(Outcome::Messages(vec![m])));
        match name {
            "MKDIR" | "MD" | "CRTDIR" => {
                let dir = cmd.get("DIR");
                ifs::mkdir(self.client, dir, 0o755, &self.user)
                    .await
                    .map_err(|e| cl_error(e, dir))?;
                msg(format!("Directory {dir} created."))
            }
            "RMVDIR" | "RMDIR" | "RD" => {
                let dir = cmd.get("DIR");
                if cmd.get("SUBTREE").eq_ignore_ascii_case("*ALL") {
                    self.remove_tree(dir).await.map_err(|e| cl_error(e, dir))?;
                } else {
                    ifs::rmdir(self.client, dir)
                        .await
                        .map_err(|e| cl_error(e, dir))?;
                }
                msg(format!("Directory {dir} removed."))
            }
            "RMVLNK" | "DEL" | "ERASE" => {
                let obj = cmd.get("OBJLNK");
                let n = self.remove_links(obj).await?;
                msg(format!("{n} object(s) removed."))
            }
            "RNM" => {
                let obj = cmd.get("OBJ");
                let new = cmd.get("NEWOBJ");
                if new.contains('/') {
                    bail!("CPFA0AB: The new name {new} must be a name, not a path.");
                }
                let path = ifs::normalize(obj)?;
                let parent = &path[..path.rfind('/').unwrap_or(0)];
                let to = format!("{parent}/{new}");
                ifs::rename(self.client, &path, &to)
                    .await
                    .map_err(|e| cl_error(e, obj))?;
                msg(format!("Object {obj} renamed to {new}."))
            }
            "WRKLNK" | "DSPLNK" => {
                let obj = match cmd.get("OBJ") {
                    "" | "*" => "/",
                    o => o,
                };
                Ok(Some(self.link_list(obj).await?))
            }
            "CPYTOSTMF" if savf_path(cmd.get("FROMMBR")).is_some() => {
                // A save file is copied as it is (binary, CCSID 65535).
                let (lib, name) = savf_path(cmd.get("FROMMBR")).unwrap_or_default();
                let to = cmd.get("TOSTMF");
                let data = self.save_file_data(&lib, &name).await?;
                let opt = cmd.get("STMFOPT").to_ascii_uppercase();
                if ifs::stat(self.client, to).await?.is_some() && opt != "*REPLACE" {
                    bail!("CPFA097: Save file {name} not copied: stream file {to} exists (STMFOPT(*NONE)).");
                }
                ifs::write_file(self.client, to, &data, 65535, 0o644, &self.user)
                    .await
                    .map_err(|e| cl_error(e, to))?;
                msg(format!("Save file {name} copied to stream file {to}."))
            }
            "CPYFRMSTMF" if savf_path(cmd.get("TOMBR")).is_some() => {
                let (lib, name) = savf_path(cmd.get("TOMBR")).unwrap_or_default();
                let from = cmd.get("FROMSTMF");
                let (bytes, _) = ifs::read_file(self.client, from).await.map_err(|e| {
                    anyhow::anyhow!(
                        "CPFA095: Stream file {from} not copied: {}",
                        ifs::errno_text(errno_of(&e))
                    )
                })?;
                let opt = cmd.get("MBROPT").to_ascii_uppercase();
                self.put_save_file_data(&lib, &name, &bytes, opt == "*REPLACE")
                    .await?;
                msg(format!("Stream file {from} copied to save file {name}."))
            }
            "CPYTOSTMF" => {
                let (lib, file, mbr) = member_path(cmd.get("FROMMBR"))?;
                let to = cmd.get("TOSTMF");
                let libs = [lib.clone()];
                let m = crate::source::find_member(self.client, &libs, &file, &mbr).await?;
                let eol = if cmd.get("ENDLINFMT").eq_ignore_ascii_case("*FIXED") {
                    ""
                } else {
                    line_end(cmd.get("ENDLINFMT"))?
                };
                let existing = ifs::stat(self.client, to).await?;
                let opt = cmd.get("STMFOPT").to_ascii_uppercase();
                if existing.is_some() && !(opt == "*REPLACE" || opt == "*ADD") {
                    bail!("CPFA097: Member {mbr} not copied: stream file {to} exists (STMFOPT(*NONE)).");
                }
                // STMFCCSID(*STMF): an existing file's CCSID, else the
                // database file's (37).
                let file_ccsid = match &existing {
                    Some(e) => ccsid_param(cmd.get("STMFCCSID"), e.ccsid)?,
                    None => ccsid_param(cmd.get("STMFCCSID"), 37)?,
                };
                let fixed = cmd.get("ENDLINFMT").eq_ignore_ascii_case("*FIXED");
                // Trailing blanks go, except with *FIXED (no line ends).
                let mut text = String::new();
                for line in m.source.lines() {
                    if fixed {
                        text.push_str(line);
                    } else {
                        text.push_str(line.trim_end());
                        text.push_str(eol);
                    }
                }
                let mut bytes = Vec::new();
                for c in text.chars() {
                    codec::encode_char(file_ccsid, c, &mut bytes);
                }
                if opt == "*ADD" {
                    if let Ok((old, _)) = ifs::read_file(self.client, to).await {
                        let mut all = old;
                        all.extend(bytes);
                        bytes = all;
                    }
                }
                ifs::write_file(self.client, to, &bytes, file_ccsid, 0o644, &self.user)
                    .await
                    .map_err(|e| cl_error(e, to))?;
                msg(format!("Member {mbr} copied to stream file {to}."))
            }
            "CPYFRMSTMF" => {
                let from = cmd.get("FROMSTMF");
                let (lib, file, mbr) = member_path(cmd.get("TOMBR"))?;
                let (bytes, ccsid) = ifs::read_file(self.client, from).await.map_err(|e| {
                    anyhow::anyhow!(
                        "CPFA095: Stream file {from} not copied: {}",
                        ifs::errno_text(errno_of(&e))
                    )
                })?;
                let ccsid = ccsid_param(cmd.get("STMFCCSID"), ccsid)?;
                let text = codec::text(ccsid, &bytes);
                // ENDLINFMT(*ALL): CR, LF, CRLF or LFCR ends a line;
                // TABEXPN(*YES): tabs become blanks to the next stop (every
                // 8 positions).
                let expand = !cmd.get("TABEXPN").eq_ignore_ascii_case("*NO");
                let lines: Vec<String> = text
                    .replace("\r\n", "\n")
                    .replace("\n\r", "\n")
                    .replace('\r', "\n")
                    .split('\n')
                    .map(|l| {
                        if !expand {
                            return l.to_string();
                        }
                        let mut out = String::new();
                        for c in l.chars() {
                            if c == '\t' {
                                let n = 8 - out.chars().count() % 8;
                                out.push_str(&" ".repeat(n));
                            } else {
                                out.push(c);
                            }
                        }
                        out
                    })
                    .collect();
                let mut body = lines.join("\n");
                while body.ends_with('\n') {
                    body.pop();
                }
                let opt = cmd.get("MBROPT").to_ascii_uppercase();
                let libs = [lib.clone()];
                let old = crate::source::find_member(self.client, &libs, &file, &mbr)
                    .await
                    .ok();
                if old.is_some() && !(opt == "*REPLACE" || opt == "*ADD") {
                    bail!("CPFA095: Stream file {from} not copied: member {mbr} exists in file {lib}/{file} (MBROPT(*NONE)).");
                }
                let source = match (&old, opt.as_str()) {
                    (Some(o), "*ADD") if !o.source.is_empty() => format!("{}\n{body}", o.source),
                    _ => body,
                };
                let m = crate::source::Member {
                    library: lib.clone(),
                    file: file.clone(),
                    name: mbr.clone(),
                    srctype: old.as_ref().map(|o| o.srctype.clone()).unwrap_or_default(),
                    text: old.as_ref().map(|o| o.text.clone()).unwrap_or_default(),
                    source,
                };
                crate::source::put_member(self.client, &m, true).await?;
                msg(format!("Stream file {from} copied to member {mbr}."))
            }
            _ => Ok(None),
        }
    }

    /// RMVDIR SUBTREE(*ALL): the directory and everything in it.
    async fn remove_tree(&mut self, dir: &str) -> Result<()> {
        for (name, is_dir) in ifs::list(self.client, dir).await? {
            let p = format!("{}/{name}", ifs::normalize(dir)?.trim_end_matches('/'));
            if is_dir {
                Box::pin(self.remove_tree(&p)).await?;
            } else {
                ifs::unlink(self.client, &p).await?;
            }
        }
        ifs::rmdir(self.client, dir).await
    }

    /// RMVLNK: a stream file, or the files a generic last name (ending in
    /// `*`) matches.
    async fn remove_links(&mut self, obj: &str) -> Result<usize> {
        let path = ifs::normalize(obj)?;
        let (dir, last) = path.rsplit_once('/').unwrap_or(("", &path));
        let dir = if dir.is_empty() { "/" } else { dir };
        if let Some(prefix) = last.strip_suffix('*') {
            let prefix = prefix.to_lowercase();
            let mut n = 0;
            for (name, is_dir) in ifs::list(self.client, dir)
                .await
                .map_err(|e| cl_error(e, obj))?
            {
                if !is_dir && name.to_lowercase().starts_with(&prefix) {
                    ifs::unlink(
                        self.client,
                        &format!("{}/{name}", dir.trim_end_matches('/')),
                    )
                    .await
                    .map_err(|e| cl_error(e, obj))?;
                    n += 1;
                }
            }
            if n == 0 {
                bail!("CPFA0A9: Object {obj} not found.");
            }
            return Ok(n);
        }
        ifs::unlink(self.client, &path)
            .await
            .map_err(|e| cl_error(e, obj))?;
        Ok(1)
    }

    /// WRKLNK, DSPLNK: a directory's objects.
    async fn link_list(&mut self, obj: &str) -> Result<Outcome> {
        let path = ifs::normalize(obj)?;
        let e = ifs::stat(self.client, &path)
            .await?
            .ok_or_else(|| anyhow::anyhow!("CPFA0A9: Object {obj} not found."))?;
        let (dir, rows) = if e.dir {
            let rows = ifs::list(self.client, &path).await?;
            (path.clone(), rows)
        } else {
            let (d, n) = path.rsplit_once('/').unwrap_or(("/", &path));
            (
                if d.is_empty() {
                    "/".into()
                } else {
                    d.to_string()
                },
                vec![(n.to_string(), false)],
            )
        };
        let text_rows: Vec<String> = rows
            .iter()
            .map(|(n, d)| format!("{:<40}  {}", n, if *d { "DIR" } else { "STMF" }))
            .collect();
        let keys: Vec<String> = rows
            .iter()
            .map(|(n, _)| format!("'{}/{n}'", dir.trim_end_matches('/')))
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Object Links".into(),
            info: vec![("Directory".into(), dir.clone())],
            heading: format!("{:<40}  {}", "Object link", "Type"),
            rows: text_rows,
            options: vec![
                (4, "Remove".into(), "RMVLNK OBJLNK({})".into()),
                (5, "Display".into(), "DSPLNK OBJ({})".into()),
            ],
            keys,
            refresh: Some(format!("WRKLNK OBJ('{obj}')")),
            print: vec![],
        }))
    }
}
