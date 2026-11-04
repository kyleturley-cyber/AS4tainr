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
                msg(format!("CPCA083: Directory {dir} was created."))
            }
            // CHGCURDIR (CL reference): the job's current directory,
            // where relative path names start.
            "CHGCURDIR" | "CD" => {
                let dir = cmd.get("DIR");
                match ifs::stat(self.client, dir)
                    .await
                    .map_err(|e| cl_error(e, dir))?
                {
                    Some(e) if e.dir => {
                        ifs::set_current_dir(&e.path);
                        msg("Current directory changed.".into())
                    }
                    Some(_) => Err(cl_error(ifs::Errno(errno::ENOTDIR).into(), dir)),
                    None => Err(cl_error(ifs::Errno(errno::ENOENT).into(), dir)),
                }
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
                msg(format!("CPCA085: Directory {dir} was removed."))
            }
            "RMVLNK" | "DEL" | "ERASE" => {
                let obj = cmd.get("OBJLNK");
                let n = self.remove_links(obj).await?;
                msg(format!("{n} object(s) removed."))
            }
            // CPY and MOV (CL reference): a stream file copied or moved
            // to TOOBJ, or into TODIR under its own name; CPY keeps a
            // target unless REPLACE(*YES). Directories are not copied.
            "CPY" | "MOV" => {
                let obj = cmd.get("OBJ");
                let from = ifs::normalize(obj)?;
                let leaf = from.rsplit('/').next().unwrap_or("").to_string();
                let to = match (cmd.get("TOOBJ").trim(), cmd.get("TODIR").trim()) {
                    (t, _) if !t.is_empty() && t != "*NONE" => ifs::normalize(t)?,
                    (_, d) if !d.is_empty() && d != "*NONE" && d != "." => {
                        format!("{}/{leaf}", ifs::normalize(d)?.trim_end_matches('/'))
                    }
                    (_, ".") => {
                        let here = ifs::current_dir();
                        format!("{}/{leaf}", here.trim_end_matches('/'))
                    }
                    _ => bail!("CPD0084: TOOBJ or TODIR is needed.\n{}", crate::cl::CPF0006),
                };
                let Some(e) = ifs::stat(self.client, &from)
                    .await
                    .map_err(|e| cl_error(e, obj))?
                else {
                    return Err(cl_error(ifs::Errno(errno::ENOENT).into(), obj));
                };
                let moving = name == "MOV";
                if moving {
                    ifs::rename(self.client, &from, &to)
                        .await
                        .map_err(|e| cl_error(e, &to))?;
                    // CPCA086, CPCA087 (PUB400, verify41 j41_mv, j41_cpy).
                    return msg(format!("CPCA086: Object {from} moved to {to}."));
                }
                if e.dir {
                    bail!("CPFA0AB: {from} is a directory: as4tainr's CPY copies stream files.");
                }
                let replace = cmd.get("REPLACE").eq_ignore_ascii_case("*YES");
                if !replace && ifs::stat(self.client, &to).await?.is_some() {
                    return Err(cl_error(ifs::Errno(errno::EEXIST).into(), &to));
                }
                let (bytes, ccsid) = ifs::read_file(self.client, &from)
                    .await
                    .map_err(|e| cl_error(e, obj))?;
                ifs::write_file(self.client, &to, &bytes, ccsid, e.mode, &self.user)
                    .await
                    .map_err(|e| cl_error(e, &to))?;
                msg(format!("CPCA087: Object {from} copied to {to}."))
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
            // DSPAUT (CL reference): an IFS object's owner and *PUBLIC
            // with their data authorities, here from the mode's owner and
            // other bits.
            "DSPAUT" => {
                let obj = cmd.get("OBJ");
                let Some(e) = ifs::stat(self.client, obj)
                    .await
                    .map_err(|e| cl_error(e, obj))?
                else {
                    return Err(cl_error(ifs::Errno(errno::ENOENT).into(), obj));
                };
                let fmt = |u: &str, a: &str| format!("{u:<10}  {a}");
                // *PUBLIC first, then the owner (PUB400, verify43
                // l43_dspaut).
                let rows = vec![
                    fmt("*PUBLIC", ifs::data_authority(e.mode)),
                    fmt(&e.owner, ifs::data_authority(e.mode >> 6)),
                ];
                let print = dspaut_report(&e, &crate::report::Stamp::now());
                Ok(Some(Outcome::Display(ListDisplay {
                    print,
                    title: "Display Authority".into(),
                    info: vec![
                        ("Object".into(), e.path.clone()),
                        ("Owner".into(), e.owner.clone()),
                        (
                            "Type of object".into(),
                            if e.dir { "*DIR" } else { "*STMF" }.into(),
                        ),
                    ],
                    heading: fmt("User", "Data authority"),
                    rows,
                    ..Default::default()
                })))
            }
            // CHGAUT (CL reference): a user's data authority to an IFS
            // object. *PUBLIC's and the owner's are the mode's other and
            // owner bits; other users' are not kept here (fidelity-gaps).
            "CHGAUT" => {
                let obj = cmd.get("OBJ");
                let Some(e) = ifs::stat(self.client, obj)
                    .await
                    .map_err(|e| cl_error(e, obj))?
                else {
                    return Err(cl_error(ifs::Errno(errno::ENOENT).into(), obj));
                };
                if e.owner != self.user {
                    self.require_secadm("change the authority of objects you do not own")
                        .await?;
                }
                let dta = cmd.get("DTAAUT").trim().to_ascii_uppercase();
                // *EXCLUDE takes no object authorities: OBJAUT(*NONE)
                // only, CPD2212 and CPF0006 otherwise (verify43 l43_chgaut,
                // with OBJAUT's default *SAME).
                let objaut = cmd.list("OBJAUT").join(" ").trim().to_ascii_uppercase();
                if dta == "*EXCLUDE" && objaut != "*NONE" {
                    return Err(crate::cl::value_error_id(
                        "CPD2212",
                        "With DTAAUT(*EXCLUDE), OBJAUT must be *NONE.",
                    ));
                }
                if dta == "*SAME" || dta.is_empty() {
                    return Ok(Some(Outcome::Messages(Vec::new())));
                }
                let Some(bits) = ifs::authority_bits(&dta) else {
                    return Err(crate::cl::value_error(&format!(
                        "DTAAUT({dta}) is not *RWX, *RW, *RX, *WX, *R, *W, *X, *EXCLUDE or *SAME."
                    )));
                };
                let mut mode = e.mode;
                let mut kept = true;
                for u in cmd.list("USER") {
                    let u = u.trim().to_ascii_uppercase();
                    if u == "*PUBLIC" {
                        mode = (mode & !0o7) | bits;
                    } else if u == e.owner {
                        mode = (mode & !0o700) | (bits << 6);
                    } else {
                        kept = false;
                    }
                }
                ifs::set_mode(self.client, obj, mode)
                    .await
                    .map_err(|e| cl_error(e, obj))?;
                let mut out = Vec::new();
                if !kept {
                    out.push(format!(
                        "Authorities of users other than *PUBLIC and the owner ({}) are not kept for {obj}.",
                        e.owner
                    ));
                }
                Ok(Some(Outcome::Messages(out)))
            }
            // CHGOWN (CL reference): an IFS object's new owner.
            "CHGOWN" => {
                let obj = cmd.get("OBJ");
                let Some(e) = ifs::stat(self.client, obj)
                    .await
                    .map_err(|e| cl_error(e, obj))?
                else {
                    return Err(cl_error(ifs::Errno(errno::ENOENT).into(), obj));
                };
                if e.owner != self.user {
                    self.require_secadm("change the owner of objects you do not own")
                        .await?;
                }
                let new = cmd.get("NEWOWN").trim().to_ascii_uppercase();
                if crate::users::get(self.client, &new).await?.is_none() {
                    bail!("CPF2204: There is no user profile {new}.");
                }
                ifs::set_owner(self.client, obj, &new)
                    .await
                    .map_err(|e| cl_error(e, obj))?;
                // CPC221B (verify43 l43_chgown).
                Ok(Some(Outcome::Messages(vec![format!(
                    "CPC221B: The owner of {obj} is now {new}."
                )])))
            }
            // DSPATR (CL reference): an IFS object's attributes, here
            // those the IFS keeps (as4tainr's own layout).
            "DSPATR" => {
                let obj = cmd.get("OBJ");
                let Some(e) = ifs::stat(self.client, obj)
                    .await
                    .map_err(|e| cl_error(e, obj))?
                else {
                    return Err(cl_error(ifs::Errno(errno::ENOENT).into(), obj));
                };
                let changed = {
                    let secs = e.changed_us.div_euclid(1_000_000);
                    let row = self
                        .client
                        .query_one(
                            "SELECT to_char(to_timestamp($1::bigint), 'MM/DD/YY HH24:MI:SS')",
                            &[&secs],
                        )
                        .await?;
                    row.get::<_, String>(0)
                };
                let info: Vec<(String, String)> = vec![
                    ("Object".into(), e.path.clone()),
                    (
                        "Type of object".into(),
                        if e.dir { "*DIR" } else { "*STMF" }.into(),
                    ),
                    ("Owner".into(), e.owner.clone()),
                    ("Coded character set ID".into(), e.ccsid.to_string()),
                    ("Size of object data in bytes".into(), e.size.to_string()),
                    ("Last change date/time".into(), changed),
                    (
                        "Mode (permissions)".into(),
                        format!("{:04o}", e.mode & 0o7777),
                    ),
                ];
                let print: Vec<String> = info
                    .iter()
                    .map(|(k, v)| format!(" {:.<40}: {v}", format!("{k} ")))
                    .collect();
                Ok(Some(Outcome::Display(ListDisplay {
                    title: "Display Attributes".into(),
                    info,
                    print,
                    ..Default::default()
                })))
            }
            "WRKLNK" | "DSPLNK" => {
                let obj = match cmd.get("OBJ") {
                    "" | "*" => "/",
                    o => o,
                };
                Ok(Some(self.link_list(obj).await?))
            }
            "CPYTOIMPF" => Ok(Some(self.cpytoimpf(cmd).await?)),
            "CPYFRMIMPF" => Ok(Some(self.cpyfrmimpf(cmd).await?)),
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
                msg(format!(
                    "CPCA081: Stream file {from} was copied to save file {name}."
                ))
            }
            "CPYTOSTMF" => {
                let (lib, file, mbr) = member_path(cmd.get("FROMMBR"))?;
                let to = cmd.get("TOSTMF");
                let libs = [lib.clone()];
                let m = crate::source::find_member(self.client, &libs, &file, &mbr).await?;
                crate::source::note_used(self.client, &m.library, &m.file, &m.name).await;
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
                    Some(e) => ccsid_param(stmf_ccsid(cmd), e.ccsid)?,
                    None => ccsid_param(stmf_ccsid(cmd), 37)?,
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
                let ccsid = ccsid_param(stmf_ccsid(cmd), ccsid)?;
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
                // A database file (not a source file): one record per
                // line, in its one character field (CRTPF RCDLEN).
                if let Ok(f) = crate::catalog::resolve_file(self.client, &libs, &file).await {
                    if f.file.source.is_none() {
                        return self
                            .stmf_to_data(&f, &lines, &opt, from, &mbr)
                            .await
                            .map(Some);
                    }
                }
                let old = crate::source::find_member(self.client, &libs, &file, &mbr)
                    .await
                    .ok();
                if old.is_some() && !(opt == "*REPLACE" || opt == "*ADD") {
                    // CPDA089, then CPFA095 (observed on IBM i 7.5).
                    bail!("CPDA089: Member {mbr} is already in file {lib}/{file}; MBROPT(*NONE) does not replace it.\nCPFA095: Stream file {from} was not copied.");
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
                    ..Default::default()
                };
                crate::source::put_member(self.client, &m, true).await?;
                crate::source::note_used(self.client, &lib, &file, &mbr).await;
                msg(format!(
                    "CPCA081: Stream file {from} was copied to member {mbr}."
                ))
            }
            _ => Ok(None),
        }
    }

    /// CPYFRMSTMF into a database file's member: a record per line, the
    /// line padded or cut to the record (a file with one character
    /// field, as CRTPF RCDLEN makes).
    /// The database file of FROMFILE/TOFILE (file and member elements):
    /// the file, and the member name it was given (the file's own).
    async fn impf_file(&self, list: &[String]) -> Result<(crate::catalog::FileRef, String)> {
        let q = list
            .first()
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_default();
        let (libs, name) = self.search(&q);
        let f = crate::catalog::resolve_file(self.client, &libs, &name).await?;
        if f.file.source.is_some() {
            bail!(
                "File {}/{} is a source file: CPYTOIMPF and CPYFRMIMPF copy database files.",
                f.library,
                f.file.name
            );
        }
        let mbr = match list.get(1).map(|m| m.to_ascii_uppercase()) {
            None => f.file.name.clone(),
            Some(m) if m == "*FIRST" || m == f.file.name => f.file.name.clone(),
            Some(m) => bail!(
                "CPF9815: Member {m} of file {} in library {} does not exist.",
                f.file.name,
                f.library
            ),
        };
        Ok((f, mbr))
    }

    /// CPYTOIMPF: a database file's records as delimited text in a
    /// stream file (`impf.rs`).
    async fn cpytoimpf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (f, mbr) = self.impf_file(&cmd.list("FROMFILE")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &f.library,
            &f.file.name,
            "*FILE",
            crate::authority::Aut::Use,
        )
        .await?;
        if !cmd.get("DTAFMT").eq_ignore_ascii_case("*DLM") {
            bail!(
                "DTAFMT({}) is not supported yet; *DLM is.",
                cmd.get("DTAFMT")
            );
        }
        let form = impf_form(cmd)?;
        let eol = match cmd.get("RCDDLM").trim().to_ascii_uppercase().as_str() {
            "" | "*EOR" => return Err(eor_stream_file()),
            "*CRLF" => "\r\n",
            "*LF" => "\n",
            "*CR" => "\r",
            "*LFCR" => "\n\r",
            o => bail!("RCDDLM({o}) is not supported."),
        };
        let to = cmd.get("TOSTMF").trim().trim_matches('\'').to_string();
        let mut text = String::new();
        let names = match cmd.get("ADDCOLNAM").trim().to_ascii_uppercase().as_str() {
            "*SYS" => Some(false),
            "*SQL" => Some(true),
            _ => None,
        };
        if let Some(sql) = names {
            text.push_str(&crate::impf::header(&f.file.fields, sql, &form));
            text.push_str(eol);
        }
        let fields = f.file.fields.clone();
        let mut of = crate::rla::OpenFile::new(f.clone());
        let mut n = 0usize;
        loop {
            let r = of.read(self.client).await?;
            let Some(rec) = r.record else { break };
            if r.eof {
                break;
            }
            let nulls = crate::rla::null_fields(&rec);
            let values: Vec<Option<String>> = fields
                .iter()
                .map(|fd| {
                    if nulls.iter().any(|x| x == &fd.name) {
                        None
                    } else {
                        rec.get(&fd.name).cloned()
                    }
                })
                .collect();
            text.push_str(&crate::impf::format_record(&fields, &values, &form));
            text.push_str(eol);
            n += 1;
        }
        let existing = ifs::stat(self.client, &to).await?;
        // STMFCCSID(*FILE): a new stream file gets the database file's
        // CCSID (37); an existing one keeps its own.
        let ccsid = match &existing {
            Some(e) => e.ccsid,
            None => ccsid_param(cmd.get("STMFCCSID"), 37)?,
        };
        let mut bytes = Vec::new();
        if cmd.get("MBROPT").eq_ignore_ascii_case("*ADD") && existing.is_some() {
            if let Ok((old, _)) = ifs::read_file(self.client, &to).await {
                bytes = old;
            }
        }
        for c in text.chars() {
            codec::encode_char(ccsid, c, &mut bytes);
        }
        ifs::write_file(self.client, &to, &bytes, ccsid, 0o644, &self.user)
            .await
            .map_err(|e| cl_error(e, &to))?;
        // CPC2958 (PUB400, IBM i 7.5, verify36 b36_exp1).
        let _ = (n, mbr);
        Ok(Outcome::Messages(vec![format!(
            "CPC2958: The records of file {} in {} were copied to stream file {to}.",
            f.file.name,
            crate::qtemp::logical(&f.library)
        )]))
    }

    /// CPYFRMIMPF: delimited text in a stream file as a database file's
    /// records (`impf.rs`). A value that does not fit its field stops the
    /// copy with the records before it written.
    async fn cpyfrmimpf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (f, mbr) = self.impf_file(&cmd.list("TOFILE")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &f.library,
            &f.file.name,
            "*FILE",
            crate::authority::Aut::Change,
        )
        .await?;
        if !cmd.get("DTAFMT").eq_ignore_ascii_case("*DLM") {
            bail!(
                "DTAFMT({}) is not supported yet; *DLM is.",
                cmd.get("DTAFMT")
            );
        }
        let form = impf_form(cmd)?;
        let from = cmd.get("FROMSTMF").trim().trim_matches('\'').to_string();
        let (bytes, ccsid) = ifs::read_file(self.client, &from).await.map_err(|e| {
            anyhow::anyhow!(
                "Stream file {from} cannot be read: {}",
                ifs::errno_text(errno_of(&e))
            )
        })?;
        let text = codec::text(ccsid, &bytes);
        let lines: Vec<&str> = match cmd.get("RCDDLM").trim().to_ascii_uppercase().as_str() {
            "*LF" => text.split('\n').collect(),
            "*CR" => text.split('\r').collect(),
            "*CRLF" => text.split("\r\n").collect(),
            "*LFCR" => text.split("\n\r").collect(),
            "" | "*EOR" => return Err(eor_stream_file()),
            o => bail!("RCDDLM({o}) is not valid for a stream file: use *CRLF, *LF, *CR or *LFCR."),
        };
        let lines: Vec<&str> = lines.into_iter().filter(|l| !l.is_empty()).collect();
        let from_rcd = cmd.list("FROMRCD");
        let mut start = match from_rcd.first().map(|s| s.trim().to_ascii_uppercase()) {
            None => 1,
            Some(s) if s.is_empty() => 1,
            Some(s) => s
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("FROMRCD({s}) is not a record number."))?
                .max(1),
        };
        if cmd.get("RMVCOLNAM").eq_ignore_ascii_case("*YES") {
            start = start.max(2);
        }
        let end = match from_rcd.get(1).map(|s| s.trim().to_ascii_uppercase()) {
            Some(s) if s != "*END" && !s.is_empty() => s.parse::<usize>().unwrap_or(usize::MAX),
            _ => usize::MAX,
        };
        let table = format!(
            "{}.{}",
            crate::catalog::ident(&f.library),
            crate::catalog::ident(&f.file.name)
        );
        if cmd.get("MBROPT").eq_ignore_ascii_case("*REPLACE") {
            crate::catalog::replace_records(self.client, &table).await?;
        }
        let dft = cmd.get("RPLNULLVAL").eq_ignore_ascii_case("*FLDDFT");
        let fields = f.file.fields.clone();
        let mut of = crate::rla::OpenFile::new(f.clone());
        let mut n = 0usize;
        for (k, line) in lines.iter().enumerate() {
            let number = k + 1;
            if number < start || number > end {
                continue;
            }
            let values = crate::impf::parse_line(line, &form);
            let mut rec = crate::rla::Record::new();
            let mut nulls = Vec::new();
            for (i, fd) in fields.iter().enumerate() {
                match values.get(i).cloned().flatten() {
                    Some(v) => {
                        let v = crate::impf::field_value(fd, &v, &form).map_err(|e| {
                            anyhow::anyhow!("Record {number} of stream file {from} was not copied: {e} {n} records were copied before it.")
                        })?;
                        rec.insert(fd.name.clone(), v);
                    }
                    None if fd.null_capable && !dft => nulls.push(fd.name.clone()),
                    None => {}
                }
            }
            if !nulls.is_empty() {
                rec.insert(crate::rla::NULLS.to_string(), nulls.join(","));
            }
            of.write(self.client, &rec).await.map_err(|e| {
                anyhow::anyhow!("Record {number} of stream file {from} was not copied: {e} {n} records were copied before it.")
            })?;
            n += 1;
        }
        // CPC2959 (PUB400, IBM i 7.5, verify38 g38_impa).
        Ok(Outcome::Messages(vec![format!(
            "CPC2959: {n} records from stream file {from} were put in member {mbr} of file {}/{}.",
            crate::qtemp::logical(&f.library),
            f.file.name
        )]))
    }

    async fn stmf_to_data(
        &mut self,
        f: &crate::catalog::FileRef,
        lines: &[String],
        opt: &str,
        from: &str,
        mbr: &str,
    ) -> Result<Outcome> {
        let [field] = f.file.fields.as_slice() else {
            bail!(
                "CPFA095: Stream file {from} was not copied: file {}/{} has more than one field.",
                f.library,
                f.file.name
            );
        };
        let table = format!(
            "{}.{}",
            crate::catalog::ident(&f.library),
            crate::catalog::ident(&f.file.name)
        );
        let count: i64 = self
            .client
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await?
            .get(0);
        match opt {
            "*REPLACE" => {
                crate::catalog::replace_records(self.client, &table).await?;
            }
            "*ADD" => {}
            _ if count > 0 => bail!(
                "CPDA089: Member {mbr} is already in file {}/{}; MBROPT(*NONE) does not replace it.\nCPFA095: Stream file {from} was not copied.",
                f.library,
                f.file.name
            ),
            _ => {}
        }
        let len = field.length as usize;
        let mut lines = lines.to_vec();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let sql = format!("INSERT INTO {table} ({}) VALUES ($1)", field.column());
        for l in &lines {
            let t: String = l.chars().take(len).collect();
            let t = format!("{t:<len$}");
            self.client.execute(&sql, &[&t]).await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "CPCA081: Stream file {from} was copied to member {mbr}."
        )]))
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
            ibm: None,
            print_msg: None,
            spool: None,
            member: None,
        }))
    }
}

/// STMFCCSID, or the code page of the old keyword STMFCODPAG when only
/// that one is given.
/// RCDDLM(*EOR), the default, with a stream file: CPYTOIMPF and
/// CPYFRMIMPF end before copying anything with CPF2845 reason code 11
/// and CPF2817 (PUB400, IBM i 7.5, tools/golden/verify37.txt: every
/// import without RCDDLM; *EOR is for database files).
fn eor_stream_file() -> anyhow::Error {
    anyhow::anyhow!(
        "CPF2845: Reason code 11: record delimiter *EOR cannot be used with a stream file; give RCDDLM(*CRLF), *LF, *CR or *LFCR.\nCPF2817: Nothing was copied."
    )
}

/// CPYTOIMPF's and CPYFRMIMPF's delimiters.
fn impf_form(cmd: &crate::cl::Command) -> Result<crate::impf::Form> {
    use crate::impf::{Blanks, Form};
    let ch = |v: &str| -> Option<char> {
        let v = v.trim();
        let v = v
            .strip_prefix('\'')
            .and_then(|x| x.strip_suffix('\''))
            .unwrap_or(v);
        let v = v.replace("''", "'");
        let mut cs = v.chars();
        match (cs.next(), cs.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    let strdlm = match cmd.get("STRDLM").trim().to_ascii_uppercase().as_str() {
        "" | "*DBLQUOTE" => Some('"'),
        "*NONE" => None,
        _ => Some(
            ch(cmd.get("STRDLM"))
                .ok_or_else(|| anyhow::anyhow!("STRDLM must be one character."))?,
        ),
    };
    let escape = match cmd.get("STRESCCHR").trim().to_ascii_uppercase().as_str() {
        "" | "*STRDLM" => strdlm,
        "*NONE" => None,
        _ => Some(
            ch(cmd.get("STRESCCHR"))
                .ok_or_else(|| anyhow::anyhow!("STRESCCHR must be one character."))?,
        ),
    };
    let flddlm = match cmd.get("FLDDLM").trim() {
        "" => ",".to_string(),
        v => {
            let v = v
                .strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .unwrap_or(v);
            match v.to_ascii_uppercase().as_str() {
                "*TAB" => "\t".to_string(),
                _ => v.replace("''", "'"),
            }
        }
    };
    let decpnt = if cmd.get("DECPNT").eq_ignore_ascii_case("*COMMA") {
        ','
    } else {
        '.'
    };
    let blanks = match cmd.get("RMVBLANK").trim().to_ascii_uppercase().as_str() {
        "*LEADING" => Blanks::Leading,
        "*TRAILING" => Blanks::Trailing,
        "*BOTH" => Blanks::Both,
        _ => Blanks::None,
    };
    if flddlm.is_empty() || Some(flddlm.as_str()) == strdlm.map(|c| c.to_string()).as_deref() {
        bail!("The field delimiter must be one character other than the string delimiter.");
    }
    Ok(Form {
        strdlm,
        escape,
        flddlm,
        decpnt,
        blanks,
    })
}

fn stmf_ccsid(cmd: &crate::cl::Command) -> &str {
    let c = cmd.get("STMFCCSID");
    let old = cmd.get("STMFCODPAG");
    if c.eq_ignore_ascii_case("*STMF") && !old.eq_ignore_ascii_case("*STMF") && !old.is_empty() {
        old
    } else {
        c
    }
}

/// DSPAUT OUTPUT(*PRINT) in IBM i 7.5's layout (PUB400, verify43
/// l43_dspaut): a style-B header (title at column 50), the object, its
/// type without the asterisk, owner, primary group and authorization list
/// (*NONE: there are none here), the two heading lines, then *PUBLIC and
/// the owner with their data authority and an X under each authority it
/// gives (r: *OBJOPR and Read; w: Add, Update, Delete; x: Execute; the
/// owner also *OBJEXIST, *OBJMGT, *OBJALTER, *OBJREF).
pub fn dspaut_report(e: &ifs::Entry, stamp: &crate::report::Stamp) -> Vec<String> {
    use crate::report::at;
    let [h1, h2] = crate::report::header_b(
        50,
        "Display Authority",
        &crate::report::system_name(),
        stamp,
        1,
    );
    let label = |l: &str, v: &str| at(&[(2, &crate::commands::dsp_label(l, 29)), (37, v)]);
    let row = |user: &str, bits: i32, owner: bool| {
        let r = bits & 4 != 0;
        let w = bits & 2 != 0;
        let x = bits & 1 != 0;
        let mut f: Vec<(usize, &str)> = vec![(2, user), (17, ifs::data_authority(bits))];
        let marks = [
            (33, r || w || x),
            (43, r),
            (52, w),
            (61, w),
            (72, w),
            (83, x),
            (95, owner),
            (104, owner),
            (113, owner),
            (122, owner),
        ];
        for (c, on) in marks {
            if on {
                f.push((c, "X"));
            }
        }
        at(&f)
    };
    vec![
        h1,
        h2,
        label("Object", &e.path),
        label("Type", if e.dir { "DIR" } else { "STMF" }),
        label("Owner", &e.owner),
        label("Primary group", "*NONE"),
        label("Authorization list", "*NONE"),
        at(&[
            (19, "Data"),
            (31, "--------------------Data Authorities---------------------"),
            (93, "------Object Authorities-------"),
        ]),
        at(&[
            (2, "User"),
            (17, "Authority"),
            (31, "Objopr"),
            (42, "Read"),
            (51, "Add"),
            (59, "Update"),
            (70, "Delete"),
            (81, "Execute"),
            (93, "Exist"),
            (103, "Mgt"),
            (111, "Alter"),
            (121, "Ref"),
        ]),
        row("*PUBLIC", e.mode & 7, false),
        row(&e.owner, (e.mode >> 6) & 7, true),
        "                                        * * * * *   E N D   O F   L I S T I N G   * * * * *".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // DSPAUT OUTPUT(*PRINT): PUB400 (IBM i 7.5), verify43 l43_dspaut (a
    // stream file with mode 0644 owned by AS4TRAINR; header lines left
    // to report.rs's own tests).
    #[test]
    fn dspaut_report_as_ibm_i_prints_it() {
        let e = ifs::Entry {
            path: "/home/AS4TRAINR/gld/g43.txt".into(),
            dir: false,
            ccsid: 1208,
            mode: 0o644,
            owner: "AS4TRAINR".into(),
            size: 2,
            changed_us: 0,
        };
        let r = super::dspaut_report(&e, &crate::report::Stamp::default());
        let want = [
            " Object . . . . . . . . . . . . :   /home/AS4TRAINR/gld/g43.txt",
            " Type . . . . . . . . . . . . . :   STMF",
            " Owner  . . . . . . . . . . . . :   AS4TRAINR",
            " Primary group  . . . . . . . . :   *NONE",
            " Authorization list . . . . . . :   *NONE",
            "                  Data        --------------------Data Authorities---------------------     ------Object Authorities-------",
            " User           Authority     Objopr     Read     Add     Update     Delete     Execute     Exist     Mgt     Alter     Ref",
            " *PUBLIC        *R              X         X",
            " AS4TRAINR      *RW             X         X        X        X          X                      X        X        X        X",
            "                                        * * * * *   E N D   O F   L I S T I N G   * * * * *",
        ];
        assert_eq!(&r[2..], want);
    }
}
