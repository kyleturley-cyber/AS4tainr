//! The system APIs done here, called from CL (`CALL PGM(QUSROBJD)`) and
//! RPG (EXTPGM prototypes): which module does each, and the retrieve
//! APIs QUSROBJD, QUSRMBRD and QUSRSPLA.
//!
//! Written from IBM's documentation (Object APIs: "Retrieve Object
//! Description (QUSROBJD)"; Database and File APIs: "Retrieve Member
//! Description (QUSRMBRD)"; Print APIs: "Retrieve Spooled File
//! Attributes (QUSRSPLA)"): parameter lists, the OBJD0100/OBJD0200,
//! MBRD0100/MBRD0200 and SPLA0100 formats and their error message IDs.

use crate::authority::{self, Aut};
use crate::dtaq::{ApiArgs, Caller};
use crate::rpg::data::{a, e};
use anyhow::{bail, Result};
use tokio_postgres::Client;

/// Is NAME one of the system APIs done here?
pub fn is_api(name: &str) -> bool {
    crate::dtaq::is_api(name)
        || crate::usrspc::is_api(name)
        || matches!(
            name.to_ascii_uppercase().as_str(),
            "QUSROBJD" | "QUSRMBRD" | "QUSRSPLA" | "QWCRNETA" | "QLICVTTP"
        )
}

/// Call a system API.
pub async fn call_api(
    client: &Client,
    caller: &Caller,
    name: &str,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    let name = name.to_ascii_uppercase();
    if crate::dtaq::is_api(&name) {
        return crate::dtaq::call_api(client, caller, &name, args).await;
    }
    if crate::usrspc::is_api(&name) {
        return crate::usrspc::call_api(client, caller, &name, args).await;
    }
    let (r, ec) = match name.as_str() {
        "QUSROBJD" => (object_description(client, caller, args).await, 5),
        "QUSRMBRD" => (member_description(client, caller, args).await, 6),
        "QUSRSPLA" => (spooled_file_attributes(client, caller, args).await, 8),
        "QWCRNETA" => (network_attributes(args), 4),
        "QLICVTTP" => (convert_object_type(args), 3),
        _ => bail!("{name} is not a system API done here."),
    };
    crate::dtaq::error_code(args, Some(ec), r)
}

fn need(args: &dyn ApiArgs, n: usize) -> Result<()> {
    if args.count() < n {
        bail!(
            "CPF3C36: Number of parameters, {}, entered for this API was not valid.",
            args.count()
        );
    }
    Ok(())
}

fn text(args: &dyn ApiArgs, i: usize) -> String {
    a(&args.bytes(i)).trim().to_ascii_uppercase()
}

pub(crate) fn field(s: &str, n: usize) -> Vec<u8> {
    let mut b = e(s);
    b.resize(n, 0x40);
    b
}

fn bin4(n: i64) -> [u8; 4] {
    (n as i32).to_be_bytes()
}

/// A qualified name: (name, library).
fn qualified(b: &[u8]) -> (String, String) {
    let name = a(b.get(..10).unwrap_or(b)).trim().to_ascii_uppercase();
    let lib = a(b.get(10..20).unwrap_or(&[])).trim().to_ascii_uppercase();
    (name, lib)
}

/// The receiver: the bytes returned (no more than its length) with bytes
/// returned and bytes available in their first 8 bytes.
fn give(args: &mut dyn ApiArgs, mut r: Vec<u8>) -> Result<()> {
    let len = args.num(1)?;
    if len < 8 {
        bail!("CPF3C24: Length of the receiver variable is not valid.");
    }
    let avail = r.len();
    let ret = (len as usize).min(avail);
    r[0..4].copy_from_slice(&bin4(ret as i64));
    r[4..8].copy_from_slice(&bin4(avail as i64));
    args.set_bytes(0, &r[..ret]);
    Ok(())
}

/// QWCRNETA (receiver, length, number of attributes, attribute names
/// (CHAR(10) each), error code) (Retrieve Network Attributes API): the
/// number of attributes returned, their offsets, then for each its name,
/// type of data (C), information status (blank: available), length and
/// value. as4tainr knows SYSNAME, LCLCPNAME and LCLLOCNAME (the system's
/// name), LCLNETID (APPN), PNDSYSNAME (blanks) and DFTMODE (BLANK); other
/// attributes come back with no data (docs/fidelity-gaps.md).
fn network_attributes(args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    let len = args.num(1)?.max(0) as usize;
    let n = args.num(2)?.clamp(0, 256);
    let names = args.bytes(3);
    let sys = crate::report::system_name();
    let mut entries: Vec<Vec<u8>> = Vec::new();
    for k in 0..n as usize {
        let nm = a(names.get(k * 10..k * 10 + 10).unwrap_or(&[]))
            .trim()
            .to_ascii_uppercase();
        let value: Option<String> = match nm.as_str() {
            "SYSNAME" | "LCLCPNAME" | "LCLLOCNAME" => Some(format!("{sys:<8}")),
            "PNDSYSNAME" => Some(" ".repeat(8)),
            "LCLNETID" => Some(format!("{:<8}", "APPN")),
            "DFTMODE" => Some(format!("{:<8}", "BLANK")),
            _ => None,
        };
        let data = value.map(|v| e(&v)).unwrap_or_default();
        let mut entry = field(&nm, 10);
        entry.extend_from_slice(&e("C "));
        entry.extend_from_slice(&bin4(data.len() as i64));
        entry.extend_from_slice(&data);
        entries.push(entry);
    }
    let mut r = bin4(entries.len() as i64).to_vec();
    let mut at = 4 + 4 * entries.len();
    let mut offsets = Vec::new();
    for en in &entries {
        offsets.extend_from_slice(&bin4(at as i64));
        at += en.len();
    }
    r.extend_from_slice(&offsets);
    for en in entries {
        r.extend_from_slice(&en);
    }
    r.truncate(len);
    args.set_bytes(0, &r);
    Ok(())
}

/// External object types and the machine's type and subtype for them
/// (two bytes; *FILE 1901, *BNDDIR 1937, *AUTL 1B01 as published lists of
/// MI object types show; *PGM 0201, *SRVPGM 0203, *LIB 0401).
pub const MI_TYPES: &[(&str, u16)] = &[
    ("*PGM", 0x0201),
    ("*SRVPGM", 0x0203),
    ("*LIB", 0x0401),
    ("*FILE", 0x1901),
    ("*BNDDIR", 0x1937),
    ("*AUTL", 0x1B01),
];

/// QLICVTTP (conversion type *SYMTOHEX or *HEXTOSYM, object type, hex
/// object type, error code) (Convert Object Type API).
fn convert_object_type(args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 3)?;
    match text(args, 0).as_str() {
        "*SYMTOHEX" => {
            let t = text(args, 1);
            let Some((_, h)) = MI_TYPES.iter().find(|(n, _)| *n == t) else {
                bail!("QLICVTTP: object type {t} is not one as4tainr knows.");
            };
            args.set_bytes(2, &h.to_be_bytes());
        }
        "*HEXTOSYM" => {
            let b = args.bytes(2);
            let h = u16::from_be_bytes([
                b.first().copied().unwrap_or(0),
                b.get(1).copied().unwrap_or(0),
            ]);
            let Some((n, _)) = MI_TYPES.iter().find(|(_, x)| *x == h) else {
                bail!("QLICVTTP: hexadecimal object type {h:04X} is not one as4tainr knows.");
            };
            args.set_bytes(1, &field(n, 10));
        }
        other => bail!("QLICVTTP: conversion type {other} is not valid."),
    }
    Ok(())
}

/// The libraries for a library value (*LIBL, *CURLIB, a name).
fn libraries(lib: &str, caller: &Caller) -> Vec<String> {
    match lib {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![l.to_string()],
    }
}

/// A timestamp column as CYYMMDDHHMMSS.
const STAMP: &str = "CASE WHEN extract(year FROM {}) >= 2000 THEN '1' ELSE '0' END || to_char({}, 'YYMMDDHH24MISS')";

fn stamp(col: &str) -> String {
    STAMP.replace("{}", col)
}

/// QUSROBJD (receiver, length, OBJD0100 or OBJD0200, qualified object
/// name, object type [, error code [, ASP control]]).
async fn object_description(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 5)?;
    let format = text(args, 2);
    if format != "OBJD0100" && format != "OBJD0200" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (name, lib) = qualified(&args.bytes(3));
    let objtype = text(args, 4);
    if !objtype.starts_with('*') || objtype.len() < 2 {
        bail!("CPF2101: Object type {objtype} is not valid.");
    }
    let mut found = None;
    for l in libraries(&lib, caller) {
        if let Some(r) = client
            .query_opt(
                &format!(
                    "SELECT attribute, text, owner, {}, {} FROM as4tainr.objects
                      WHERE library=$1 AND name=$2 AND objtype=$3",
                    stamp("created"),
                    stamp("coalesce(restored, created)")
                ),
                &[&l, &name, &objtype],
            )
            .await?
        {
            found = Some((l, r));
            break;
        }
    }
    let Some((olib, r)) = found else {
        if !lib.starts_with('*') && !crate::catalog::library_exists(client, &lib).await? {
            bail!("CPF9810: Library {lib} not found.");
        }
        match objtype.as_str() {
            "*PGM" => bail!("CPF9811: Program {name} in library {lib} not found."),
            "*FILE" => bail!("CPF9812: File {name} in library {lib} not found."),
            _ => bail!("CPF9801: Object {name} in library {lib} not found."),
        }
    };
    authority::check(client, &caller.user, &olib, &name, &objtype, Aut::Use).await?;
    let mut o = vec![0u8; 8];
    o.extend(field(&name, 10));
    o.extend(field(&olib, 10));
    o.extend(field(&objtype, 10));
    // The library the object was found in: only for *LIBL and *CURLIB.
    o.extend(field(if lib.starts_with('*') { &olib } else { "" }, 10));
    o.extend_from_slice(&bin4(1)); // the system ASP
    o.extend(field(&r.get::<_, String>(2), 10));
    // User spaces are in the user domain; other objects in the system's.
    o.extend(field(if objtype == "*USRSPC" { "*U" } else { "*S" }, 2));
    o.extend(field(&r.get::<_, String>(3), 13));
    o.extend(field(&r.get::<_, String>(4), 13));
    if format == "OBJD0200" {
        o.extend(field(&r.get::<_, String>(0), 10));
        o.extend(field(&r.get::<_, String>(1), 50));
        o.extend(field("", 30)); // source file, library, member: not kept
    }
    give(args, o)
}

/// QUSRMBRD (receiver, length, MBRD0100 or MBRD0200, qualified file
/// name, member (a name, *FIRST, *LAST), override processing [, error
/// code [, find member processing]]).
async fn member_description(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 6)?;
    let format = text(args, 2);
    if format != "MBRD0100" && format != "MBRD0200" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (file, lib) = qualified(&args.bytes(3));
    match text(args, 5).as_str() {
        "0" | "1" => {}
        v => bail!("CPF3C25: Override processing value {v} is not valid."),
    }
    let mut found = None;
    for l in libraries(&lib, caller) {
        if let Some(r) = client
            .query_opt(
                &format!(
                    "SELECT attribute, text, {} FROM as4tainr.objects
                      WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    stamp("created")
                ),
                &[&l, &file],
            )
            .await?
        {
            found = Some((l, r));
            break;
        }
    }
    let Some((flib, r)) = found else {
        if !lib.starts_with('*') && !crate::catalog::library_exists(client, &lib).await? {
            bail!("CPF9810: Library {lib} not found.");
        }
        bail!("CPF9812: File {file} in library {lib} not found.");
    };
    authority::check(client, &caller.user, &flib, &file, "*FILE", Aut::Use).await?;
    let attr: String = r.get(0);
    let source = attr == "PF-SRC";
    if !source && attr != "PF" && attr != "LF" {
        bail!("CPF3C23: Object {file} in {flib} is not a database file.");
    }
    // (member, source type, created, changed, text, records)
    let mut members: Vec<(String, String, String, String, String, i64)> = Vec::new();
    if source {
        for m in client
            .query(
                &format!(
                    "SELECT member, srctype, {}, text,
                            CASE WHEN source = '' THEN 0
                                 ELSE array_length(string_to_array(rtrim(source, E'\\n'), E'\\n'), 1) END
                       FROM as4tainr.source_members WHERE library=$1 AND file=$2 ORDER BY member",
                    stamp("changed")
                ),
                &[&flib, &file],
            )
            .await?
        {
            let st: String = m.get(2);
            members.push((
                m.get(0),
                m.get(1),
                st.clone(),
                st,
                m.get(3),
                m.get::<_, Option<i32>>(4).unwrap_or(0) as i64,
            ));
        }
    } else {
        let records =
            match crate::catalog::resolve_file(client, std::slice::from_ref(&flib), &file).await {
                Ok(f) => client
                    .query_one(&format!("SELECT count(*) FROM {}", f.table()), &[])
                    .await
                    .map(|r| r.get::<_, i64>(0))
                    .unwrap_or(0),
                Err(_) => 0,
            };
        let st: String = r.get(2);
        members.push((
            file.clone(),
            String::new(),
            st.clone(),
            st,
            r.get(1),
            records,
        ));
    }
    if members.is_empty() {
        bail!("CPF3C26: File {file} in library {flib} has no members.");
    }
    let want = text(args, 4);
    let m = match want.as_str() {
        "*FIRST" | "" => members.first(),
        "*LAST" => members.last(),
        w => members.iter().find(|m| m.0 == w),
    };
    let Some((mname, srctype, created, changed, mtext, records)) = m.cloned() else {
        bail!("CPF9815: Member {want} of file {file} in library {flib} does not exist.");
    };
    let mut o = vec![0u8; 8];
    o.extend(field(&file, 10));
    o.extend(field(&flib, 10));
    o.extend(field(&mname, 10));
    o.extend(field(if attr == "LF" { "LF" } else { "PF" }, 10));
    o.extend(field(&srctype, 10));
    o.extend(field(&created, 13));
    o.extend(field(&changed, 13));
    o.extend(field(&mtext, 50));
    o.extend(e(if source { "1" } else { "0" }));
    if format == "MBRD0200" {
        o.extend(e("0")); // local
        o.extend(e(if attr == "LF" { "1" } else { "0" }));
        o.extend(e("0")); // ODP sharing not allowed (SHARE(*NO))
        o.extend(field("", 2));
        o.extend_from_slice(&bin4(records.min(i32::MAX as i64)));
        o.extend_from_slice(&bin4(0)); // deleted records
        o.extend_from_slice(&bin4(0)); // data space size: not kept
        o.extend_from_slice(&bin4(0)); // access path size
        o.extend_from_slice(&bin4(1)); // based-on members
        o.extend(field(&changed, 13));
        o.extend(field("", 13)); // save date
        o.extend(field("", 13)); // restore date
        o.extend(field("", 7)); // expiration date
        o.extend(field("", 4));
        o.extend_from_slice(&0i16.to_be_bytes()); // media preference
        o.extend_from_slice(&bin4(0)); // days used
        o.extend(field("", 7)); // date last used
        o.extend(field("", 7)); // use reset date
        o.extend(field("", 2));
        o.extend_from_slice(&bin4(1)); // data space size multiplier
        o.extend_from_slice(&bin4(1)); // access path size multiplier
        o.extend_from_slice(&bin4(37)); // text CCSID
        o.extend_from_slice(&bin4(0)); // no additional information
        o.extend_from_slice(&bin4(0));
        o.extend_from_slice(&(records.min(u32::MAX as i64) as u32).to_be_bytes());
        o.extend_from_slice(&0u32.to_be_bytes());
        o.extend(field("", 6));
    }
    give(args, o)
}

/// A spooled file's internal identifiers (16 bytes each, opaque to
/// programs): the job's number and the spooled file's own number in this
/// system's catalog.
pub fn internal_ids(job: i64, spl: i64) -> (Vec<u8>, Vec<u8>) {
    let mut j = job.to_be_bytes().to_vec();
    j.resize(16, 0);
    let mut s = spl.to_be_bytes().to_vec();
    s.resize(16, 0);
    (j, s)
}

/// IBM's status names for a spooled file's status.
pub fn status_name(s: &str) -> &'static str {
    match s {
        "OPN" => "*OPEN",
        "HLD" => "*HELD",
        "WTR" => "*WRITING",
        "SAV" => "*SAVED",
        _ => "*READY",
    }
}

/// QUSRSPLA (receiver, length, SPLA0100, qualified job name, internal job
/// ID, internal spooled file ID, spooled file name, spooled file number
/// [, error code [, job system name, create date, create time]]). The job
/// '*' is this job; '*INT' with the spooled file name '*INT' takes the
/// internal identifiers (from QUSLSPL). Number -1: the highest numbered
/// file of that name, 0: the only one.
async fn spooled_file_attributes(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 8)?;
    let format = text(args, 2);
    if format != "SPLA0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let qjob = a(&args.bytes(3));
    let jname = qjob.get(..10).unwrap_or("").trim().to_ascii_uppercase();
    let juser = qjob.get(10..20).unwrap_or("").trim().to_ascii_uppercase();
    let jnbr = qjob.get(20..26).unwrap_or("").trim().to_string();
    let splname = text(args, 6);
    let number = args.num(7)?;
    let cols = "s.id, s.job, s.job_name, s.user_name, s.file, s.number, s.outq, s.status,
                s.user_data, s.pages, CASE WHEN extract(year FROM s.created) >= 2000 THEN '1' ELSE '0' END
                || to_char(s.created, 'YYMMDD'), to_char(s.created, 'HH24MISS')";
    let row = if jname == "*INT" || splname == "*INT" {
        let b = args.bytes(5);
        let id = i64::from_be_bytes(b.get(..8).unwrap_or(&[0; 8]).try_into().unwrap());
        client
            .query_opt(
                &format!("SELECT {cols} FROM as4tainr.spooled_files s WHERE s.id=$1"),
                &[&id],
            )
            .await?
    } else {
        let job = if jname == "*" {
            match &caller.job {
                Some(j) => j.number,
                None => bail!("CPF3342: Job * not found."),
            }
        } else {
            let n: i64 = jnbr.parse().unwrap_or(-1);
            match client
                .query_opt(
                    "SELECT number FROM as4tainr.jobs WHERE number=$1 AND user_name=$2 AND name=$3",
                    &[&n, &juser, &jname],
                )
                .await
            {
                Ok(Some(r)) => r.get(0),
                _ => bail!("CPF3342: Job {jnbr}/{juser}/{jname} not found."),
            }
        };
        let rows = client
            .query(
                &format!(
                    "SELECT {cols} FROM as4tainr.spooled_files s
                      WHERE s.job=$1 AND s.file=$2 ORDER BY s.number"
                ),
                &[&job, &splname],
            )
            .await?;
        match number {
            -1 => rows.last().cloned(),
            0 => {
                if rows.len() > 1 {
                    bail!("CPF3C20: More than one spooled file {splname} in the job; give its number.");
                }
                rows.first().cloned()
            }
            n => rows.into_iter().find(|r| r.get::<_, i32>(5) as i64 == n),
        }
    };
    let Some(r) = row else {
        bail!("CPF3C20: Spooled file {splname} number {number} not found.");
    };
    let user: String = r.get(3);
    // Another user's spooled file: *SPLCTL (or *ALLOBJ) special authority.
    if user != caller.user
        && !crate::users::has_special(client, &caller.user, "*SPLCTL").await?
        && !crate::users::has_special(client, &caller.user, "*ALLOBJ").await?
    {
        bail!("CPF9802: Not authorized to spooled file {splname} of user {user}.");
    }
    let job_name: String = r.get(2);
    let parts: Vec<&str> = job_name.split('/').collect();
    let (nbr, juser, jname) = match parts.as_slice() {
        [n, u, j] => (n.to_string(), u.to_string(), j.to_string()),
        _ => (String::new(), user.clone(), String::new()),
    };
    let (ij, is) = internal_ids(r.get(1), r.get(0));
    let mut o = vec![0u8; 8];
    o.extend(ij);
    o.extend(is);
    o.extend(field(&jname, 10));
    o.extend(field(&juser, 10));
    o.extend(field(&nbr, 6));
    o.extend(field(&r.get::<_, String>(4), 10));
    o.extend_from_slice(&bin4(r.get::<_, i32>(5) as i64));
    o.extend(field("*STD", 10)); // form type
    o.extend(field(&r.get::<_, String>(8), 10));
    o.extend(field(status_name(&r.get::<_, String>(7)), 10));
    o.extend(field("*FILEEND", 10)); // file available
    o.extend(field("*NO", 10)); // hold file before written
    o.extend(field("*NO", 10)); // save file after written
    let pages = r.get::<_, i32>(9) as i64;
    o.extend_from_slice(&bin4(pages));
    o.extend_from_slice(&bin4(0)); // page being written
    o.extend_from_slice(&bin4(1)); // starting page
    o.extend_from_slice(&bin4(0)); // ending page (*END)
    o.extend_from_slice(&bin4(0)); // last page printed
    o.extend_from_slice(&bin4(0)); // restart printing
    o.extend_from_slice(&bin4(1)); // total copies
    o.extend_from_slice(&bin4(1)); // copies left
    o.extend_from_slice(&bin4(6)); // lines per inch
    o.extend_from_slice(&bin4(10)); // characters per inch
    o.extend(field("5", 2)); // output priority
    o.extend(field(&r.get::<_, String>(6), 10));
    o.extend(field("QGPL", 10));
    o.extend(field(&r.get::<_, String>(10), 7));
    o.extend(field(&r.get::<_, String>(11), 6));
    o.extend(field(&r.get::<_, String>(4), 10)); // device file
    o.extend(field("QSYS", 10));
    o.extend(field("", 20)); // program that opened the file
    o.extend(field("", 15)); // accounting code
    o.extend(field("", 30)); // print text
    o.extend_from_slice(&bin4(132)); // record length
    o.extend_from_slice(&bin4(0)); // maximum records (*NOMAX)
    o.extend(field("*PRINTER", 10)); // device type
    o.extend(field("*SCS", 10)); // printer device type
    o.extend(field("", 12)); // document name
    o.extend(field("", 64)); // folder name
    o.extend(field("", 8)); // System/36 procedure name
    give(args, o)
}
