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

/// What QLICOBJD changes and QUSROBJD's OBJD0300 and OBJD0400 return
/// for any object: where its source is, when the source last changed,
/// and the user-defined attribute.
pub const DDL: &str = "
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                  WHERE table_schema = 'as4tainr' AND table_name = 'objects' AND column_name = 'user_attr') THEN
    ALTER TABLE as4tainr.objects
      ADD COLUMN IF NOT EXISTS src_file text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS src_lib text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS src_mbr text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS src_changed text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS user_attr text NOT NULL DEFAULT '';
  END IF;
END $$;
";

/// Is NAME one of the system APIs done here?
pub fn is_api(name: &str) -> bool {
    crate::dtaq::is_api(name)
        || crate::usrspc::is_api(name)
        || matches!(
            name.to_ascii_uppercase().as_str(),
            "QUSROBJD"
                | "QUSRMBRD"
                | "QUSRSPLA"
                | "QWCRNETA"
                | "QLICVTTP"
                | "QBNRMODI"
                | "QDBRTVFD"
                | "QLICOBJD"
                | "QUSRJOBI"
                | "QSZRTVPR"
                | "QWDRJOBD"
        )
}

/// A retrieve API whose first parameter is the receiver and second its
/// length: the receiver can be longer than the variable passed for it
/// (OPTIONS(*VARSIZE): a based structure over storage of the length
/// given).
pub fn receiver_first(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "QUSROBJD"
            | "QUSRMBRD"
            | "QUSRSPLA"
            | "QWCRNETA"
            | "QBNRMODI"
            | "QDBRTVFD"
            | "QUSRJOBI"
            | "QSZRTVPR"
            | "QWDRJOBD"
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
        "QBNRMODI" => (module_information(client, caller, args).await, 4),
        "QDBRTVFD" => (file_description(client, caller, args).await, 9),
        "QLICOBJD" => (change_object_description(client, caller, args).await, 4),
        "QUSRJOBI" => (job_information(caller, args), 5),
        "QSZRTVPR" => (product_information(args), 4),
        "QWDRJOBD" => (job_description(client, caller, args).await, 4),
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
    // The job's own QTEMP library is QTEMP to the program.
    let mut b = e(&crate::qtemp::logical(s));
    b.resize(n, 0x40);
    b
}

fn bin4(n: i64) -> [u8; 4] {
    (n as i32).to_be_bytes()
}

/// A qualified name: (name, library).
fn qualified(b: &[u8]) -> (String, String) {
    let name = a(b.get(..10).unwrap_or(b)).trim().to_ascii_uppercase();
    let lib = crate::qtemp::real(&a(b.get(10..20).unwrap_or(&[])).trim().to_ascii_uppercase());
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
        l => vec![crate::qtemp::real(l)],
    }
}

/// A timestamp column as CYYMMDDHHMMSS.
const STAMP: &str = "CASE WHEN extract(year FROM {}) >= 2000 THEN '1' ELSE '0' END || to_char({}, 'YYMMDDHH24MISS')";

fn stamp(col: &str) -> String {
    STAMP.replace("{}", col)
}

/// QBNRMODI (receiver, length, MODI0100, qualified module name, error
/// code): MODI0100's leading fields — bytes returned and available, the
/// module, its library and attribute, when it was created
/// (CYYMMDDHHMMSS), its source file, library and member, when the source
/// last changed — then blanks, and the optimization level, binary(4) at
/// offset 204, 10 (*NONE: as4tainr does not optimize). The offsets of
/// the source fields and of the optimization level are those iRPGUnit's
/// CMPMODCPP reads; the rest of IBM's format is not filled in
/// (fidelity-gaps).
async fn module_information(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 4)?;
    let format = text(args, 2);
    if format != "MODI0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (name, lib) = qualified(&args.bytes(3));
    let mut found = None;
    for l in libraries(&lib, caller) {
        if let Some(r) = client
            .query_opt(
                &format!(
                    "SELECT m.attribute, {}, m.src_file, m.src_lib, m.src_mbr,
                            coalesce({}, '')
                       FROM as4tainr.modules m JOIN as4tainr.objects o
                         ON o.library = m.library AND o.name = m.name AND o.objtype = '*MODULE'
                      WHERE m.library=$1 AND m.name=$2",
                    stamp("o.created"),
                    stamp("m.src_changed")
                ),
                &[&l, &name],
            )
            .await?
        {
            found = Some((l, r));
            break;
        }
    }
    let Some((mlib, r)) = found else {
        bail!("CPF9801: Object {name} is not in library {lib}.");
    };
    authority::check(client, &caller.user, &mlib, &name, "*MODULE", Aut::Use).await?;
    let mut o = vec![0u8; 8];
    o.extend(field(&name, 10));
    o.extend(field(&mlib, 10));
    o.extend(field(&r.get::<_, String>(0), 10));
    o.extend(field(&r.get::<_, String>(1), 13));
    o.extend(field(&r.get::<_, String>(2), 10));
    o.extend(field(&r.get::<_, String>(3), 10));
    o.extend(field(&r.get::<_, String>(4), 10));
    o.extend(field(&r.get::<_, String>(5), 13));
    o.resize(204, 0x40);
    o.extend_from_slice(&bin4(10));
    give(args, o)
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
    if !matches!(
        format.as_str(),
        "OBJD0100" | "OBJD0200" | "OBJD0300" | "OBJD0400"
    ) {
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
                    "SELECT attribute, text, owner, {}, {}, src_file, src_lib, src_mbr,
                            src_changed, user_attr FROM as4tainr.objects
                      WHERE library=$1 AND name=$2 AND objtype=$3",
                    stamp("created"),
                    stamp("changed")
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
            bail!("CPF9810: There is no library {lib}.");
        }
        match objtype.as_str() {
            "*PGM" => bail!("CPF9811: Program {name} in library {lib} not found."),
            "*FILE" => bail!("CPF9812: There is no file {name} in library {lib}."),
            _ => bail!("CPF9801: Object {name} is not in library {lib}."),
        }
    };
    authority::check(client, &caller.user, &olib, &name, &objtype, Aut::Use).await?;
    let mut o = vec![0u8; 8];
    let shown = olib.clone();
    o.extend(field(&name, 10));
    o.extend(field(&shown, 10));
    o.extend(field(&objtype, 10));
    // Return library: the library the object is in, for a library
    // named as well as *LIBL and *CURLIB (iRPGUnit's
    // Object_resolveLibrary, used on real systems, takes it for an
    // explicitly qualified source file).
    o.extend(field(&shown, 10));
    o.extend_from_slice(&bin4(1)); // the system ASP
    o.extend(field(&r.get::<_, String>(2), 10));
    // User spaces are in the user domain; other objects in the system's.
    o.extend(field(if objtype == "*USRSPC" { "*U" } else { "*S" }, 2));
    o.extend(field(&r.get::<_, String>(3), 13));
    o.extend(field(&r.get::<_, String>(4), 13));
    if format == "OBJD0100" {
        return give(args, o);
    }
    o.extend(field(&r.get::<_, String>(0), 10));
    o.extend(field(&r.get::<_, String>(1), 50));
    // Where the object came from: what QLICOBJD set, else the source a
    // program or module was compiled from.
    let mut src: (String, String, String, String) = (r.get(5), r.get(6), r.get(7), r.get(8));
    if src.0.is_empty() && matches!(objtype.as_str(), "*PGM" | "*MODULE") {
        let table = if objtype == "*PGM" {
            "programs"
        } else {
            "modules"
        };
        if let Ok(Some(x)) = client
            .query_opt(
                &format!(
                    "SELECT src_file, src_lib, src_mbr,
                            coalesce(CASE WHEN extract(year FROM src_changed) >= 2000 THEN '1' ELSE '0' END
                                     || to_char(src_changed AT TIME ZONE 'UTC', 'YYMMDDHH24MISS'), '')
                       FROM as4tainr.{table} WHERE library=$1 AND name=$2"
                ),
                &[&olib, &name],
            )
            .await
        {
            src = (x.get(0), x.get(1), x.get(2), x.get(3));
        }
    }
    o.extend(field(&src.0, 10));
    o.extend(field(&src.1, 10));
    o.extend(field(&src.2, 10));
    if format == "OBJD0200" {
        return give(args, o);
    }
    // OBJD0300 (Retrieve Object Description API): the source's change
    // date, save and restore dates (not shown here: blank), creator,
    // system created on, reset date, save size and sequence, storage,
    // save command, volume, device, save file, label, system level,
    // compiler, object level, user changed, licensed program, PTF, APAR.
    o.extend(field(&src.3, 13));
    o.extend(field("", 13)); // saved
    o.extend(field("", 13)); // restored
    o.extend(field(&r.get::<_, String>(2), 10)); // creator: the owner
    o.extend(field(&crate::report::system_name(), 8));
    o.extend(field("", 7)); // reset date
    o.extend_from_slice(&bin4(0)); // save size
    o.extend_from_slice(&bin4(0)); // save sequence number
    o.extend(field("*KEEP", 10)); // storage
    o.extend(field("", 10)); // save command
    o.extend(field("", 71)); // save volume
    o.extend(field("", 10)); // save device
    o.extend(field("", 10)); // save file
    o.extend(field("", 10)); // save file library
    o.extend(field("", 17)); // save label
    o.extend(field("", 9)); // system level
    o.extend(field("", 16)); // compiler
    o.extend(field("", 8)); // object level
    o.extend(field("0", 1)); // user changed
    o.extend(field("", 16)); // licensed program
    o.extend(field("", 10)); // PTF
    o.extend(field("", 10)); // APAR
    if format == "OBJD0300" {
        return give(args, o);
    }
    // OBJD0400: last used date, usage information updated, days used,
    // size and multiplier, compression status, program change allowed,
    // changed by program, the user-defined attribute (QLICOBJD), ...
    o.extend(field("", 7));
    o.extend(field("Y", 1));
    o.extend_from_slice(&bin4(0));
    o.extend_from_slice(&bin4(0));
    o.extend_from_slice(&bin4(1));
    o.extend(field("N", 1));
    o.extend(field("1", 1));
    o.extend(field("0", 1));
    o.extend(field(&r.get::<_, String>(9), 10));
    o.extend(field("0", 1)); // overflowed ASP
    o.extend(field("", 13)); // save active date
    o.extend(field("*NONE", 10)); // auditing value
    o.extend(field("*NONE", 10)); // primary group
    o.extend(field("0", 1)); // journaling status
    o.extend(field("", 10));
    o.extend(field("", 10));
    give(args, o)
}

/// QLICOBJD (returned library, qualified object name, object type,
/// changed object information, error code) (Change Object Description
/// API). The changed object information: the number of records, then
/// each record's key, length of its data and the data. Keys done here:
/// 1 source file, library and member (CHAR(30)), 2 source file updated
/// date and time (CYYMMDDHHMMSS), 3 text description (CHAR(50)), 9
/// user-defined attribute (CHAR(10)); other keys are CPF3C82.
async fn change_object_description(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 5)?;
    let (name, lib) = qualified(&args.bytes(1));
    let objtype = text(args, 2);
    let mut found = None;
    for l in libraries(&lib, caller) {
        if crate::catalog::object_exists(client, std::slice::from_ref(&l), &name, &objtype).await? {
            found = Some(l);
            break;
        }
    }
    let Some(olib) = found else {
        if !lib.starts_with('*') && !crate::catalog::library_exists(client, &lib).await? {
            bail!("CPF9810: There is no library {lib}.");
        }
        bail!("CPF9801: Object {name} is not in library {lib}.");
    };
    authority::check(client, &caller.user, &olib, &name, &objtype, Aut::Change).await?;
    let info = args.bytes(3);
    let i4 = |at: usize| -> Option<i32> {
        info.get(at..at + 4)
            .map(|b| i32::from_be_bytes(b.try_into().unwrap()))
    };
    let n = i4(0).unwrap_or(0);
    let mut at = 4;
    let mut sets: Vec<(&str, String)> = Vec::new();
    for _ in 0..n.max(0) {
        let (Some(key), Some(len)) = (i4(at), i4(at + 4)) else {
            bail!("CPF3C1D: Length of the record in the changed object information is not valid.");
        };
        let len = len.max(0) as usize;
        let data = info.get(at + 8..at + 8 + len).unwrap_or(&[]);
        let txt = a(data);
        match key {
            1 => {
                let t = format!("{txt:<30}");
                sets.push(("src_file", t[0..10].trim().to_string()));
                sets.push(("src_lib", t[10..20].trim().to_string()));
                sets.push(("src_mbr", t[20..30].trim().to_string()));
            }
            2 => sets.push(("src_changed", txt.trim().to_string())),
            3 => sets.push(("text", txt.trim_end().to_string())),
            9 => sets.push(("user_attr", txt.trim().to_string())),
            k => bail!(
                "CPF3C82: Key {k} is not valid for this API (as4tainr changes keys 1, 2, 3 and 9)."
            ),
        }
        at += 8 + len;
    }
    for (col, v) in sets {
        client
            .execute(
                &format!(
                    "UPDATE as4tainr.objects SET {} = $4 WHERE library=$1 AND name=$2 AND objtype=$3",
                    crate::catalog::ident(col)
                ),
                &[&olib, &name, &objtype, &v],
            )
            .await?;
    }
    let mut rl = field(&olib, 10);
    rl.truncate(10);
    args.set_bytes(0, &rl);
    Ok(())
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
            bail!("CPF9810: There is no library {lib}.");
        }
        bail!("CPF9812: There is no file {file} in library {lib}.");
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
                       FROM as4tainr.source_members WHERE library=$1 AND file=$2 ORDER BY member{}",
                    stamp("changed"),
                    crate::collate::clause(client).await?
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

/// QDBRTVFD (receiver, length, returned file name, FILD0100 or FILD0200,
/// qualified file, record format, override processing, system, format
/// type, error code) (Retrieve Database File Description API). FILD0100:
/// the 400-byte file definition header (Qdb_Qdbfh): attribute bytes
/// (x'2000' logical, x'0800' source, x'0200' keyed access path), number
/// of based-on files, key fields and record formats, maximum members,
/// members, creation date, text, CCSID and maximum record length.
/// FILD0200: the format definition header (Qdb_Qddfmt): CCSID, the
/// common CCSID flag (x'04'), record length, format name, level
/// identifier, text and number of fields; the field definitions that
/// follow on IBM i are not returned (docs/fidelity-gaps.md).
async fn file_description(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 9)?;
    let format = text(args, 3);
    if format != "FILD0100" && format != "FILD0200" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (file, flib) = qualified(&args.bytes(4));
    let mut found = None;
    for l in libraries(&flib, caller) {
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
    let Some((lib, r)) = found else {
        if !flib.starts_with('*') && !crate::catalog::library_exists(client, &flib).await? {
            bail!("CPF9810: There is no library {flib}.");
        }
        bail!("CPF9812: There is no file {file} in library {flib}.");
    };
    authority::check(client, &caller.user, &lib, &file, "*FILE", Aut::Use).await?;
    let attr: String = r.get(0);
    let ftext: String = r.get(1);
    let created: String = r.get(2);
    let (fmt, fields, _) = crate::objdesc::record_format(client, &lib, &file, &attr)
        .await
        .map_err(|_| {
            anyhow::anyhow!("CPF3C22: Cannot get information about file {file} in {lib}.")
        })?;
    let want = text(args, 5);
    if want != "*FIRST" && !want.is_empty() && want != fmt {
        bail!("CPF3C28: Record format {want} not found in file {file} in {lib}.");
    }
    let mut name = field(&file, 10);
    name.extend(field(&lib, 10));
    args.set_bytes(2, &name);
    let source = attr == "PF-SRC";
    let logical = attr == "LF";
    let keys = if source {
        0
    } else {
        crate::catalog::resolve_file(client, std::slice::from_ref(&lib), &file)
            .await
            .map(|f| f.file.keys.len())
            .unwrap_or(0)
    };
    let reclen: u32 = fields.iter().map(|f| f.buffer_bytes()).sum();
    let put = |o: &mut Vec<u8>, at: usize, b: &[u8]| o[at..at + b.len()].copy_from_slice(b);
    let bin2 = |n: i64| (n as i16).to_be_bytes();
    if format == "FILD0100" {
        let members: i64 = if source {
            client
                .query_one(
                    "SELECT count(*) FROM as4tainr.source_members WHERE library=$1 AND file=$2",
                    &[&lib, &file],
                )
                .await
                .map(|r| r.get(0))
                .unwrap_or(0)
        } else {
            1
        };
        let mut o = vec![0u8; 400];
        let mut flags: u16 = 0;
        if logical {
            flags |= 0x2000;
        }
        if source {
            flags |= 0x0800;
        }
        if keys > 0 {
            flags |= 0x0200;
        }
        put(&mut o, 8, &flags.to_be_bytes());
        put(&mut o, 14, &bin2(i64::from(logical))); // based-on files
        put(&mut o, 16, &bin2(keys as i64));
        put(&mut o, 30, &field("*CHANGE", 10)); // public authority
        put(&mut o, 41, &bin2(if source { 32767 } else { 1 })); // MAXMBRS
        put(&mut o, 47, &bin2(members));
        put(&mut o, 61, &bin2(1)); // record formats
        put(&mut o, 69, &field(&created, 13));
        put(&mut o, 84, &field(&ftext, 50));
        put(&mut o, 201, &bin2(37)); // CCSID
        put(&mut o, 304, &bin2(i64::from(reclen)));
        give(args, o)
    } else {
        let mut o = vec![0u8; 256];
        put(&mut o, 45, &bin2(37)); // the format's CCSID
        let mut flags: u8 = 0x04; // one CCSID for every character field
        if fields.iter().any(|f| f.varlen) {
            flags |= 0x40;
        }
        if fields.iter().any(|f| f.null_capable) {
            flags |= 0x08;
        }
        o[61] = flags;
        put(&mut o, 66, &bin4(i64::from(reclen)));
        put(&mut o, 70, &field(&fmt, 10));
        put(
            &mut o,
            80,
            &field(&crate::objdesc::level_id(&fmt, &fields), 13),
        );
        put(&mut o, 93, &field(&ftext, 50));
        put(&mut o, 143, &bin2(fields.len() as i64));
        give(args, o)
    }
}

/// QUSRJOBI (receiver, length, format, qualified job name, internal job
/// identifier [, error code [, reset performance statistics]]) (Retrieve
/// Job Information API), for this job ('*'). JOBI0100: job name, user,
/// number, internal identifier (blank), status *ACTIVE, type (I with a
/// display, else B), subtype, run priority 20, time slice 2000, default
/// wait 30, purge *YES. JOBI0700: the same name part, then the number of
/// system, product, current and user libraries and the library list
/// (CHAR(11) each), as DSPLIBL shows it. JOBI0400: the job's
/// attributes (decimal format, date format and separators...). Other
/// formats are CPF3C21.
fn job_information(caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    let format = text(args, 2);
    let qjob = text(args, 3);
    if !qjob.starts_with('*') {
        bail!("CPF3C58: Job name {qjob} is not valid (as4tainr retrieves the current job, '*').");
    }
    let (jname, juser, jnbr) = match &caller.job {
        Some(j) => (j.name.clone(), j.user.clone(), format!("{:06}", j.number)),
        // No job of as4tainr's (`as4tainr system`, `call`): blank, as
        // RTVJOBA JOB and NBR give there.
        None => (String::new(), caller.user.clone(), String::new()),
    };
    let mut o = vec![0u8; 8];
    o.extend(field(&jname, 10));
    o.extend(field(&juser, 10));
    o.extend(field(&jnbr, 6));
    o.extend(vec![0u8; 16]); // internal job identifier
    o.extend(field("*ACTIVE", 10));
    o.extend(field("B", 1));
    o.extend(field("", 1));
    o.extend(field("", 2));
    match format.as_str() {
        "JOBI0100" => {
            o.extend_from_slice(&bin4(20));
            o.extend_from_slice(&bin4(2000));
            o.extend_from_slice(&bin4(30));
            o.extend(field("*YES", 10));
        }
        "JOBI0700" => {
            let sys: Vec<String> = crate::commands::SYSTEM_LIBRARY_LIST
                .iter()
                .map(|l| l.to_string())
                .collect();
            let cur: Vec<String> = if caller.curlib.is_empty() {
                Vec::new()
            } else {
                vec![caller.curlib.clone()]
            };
            let usr: Vec<String> = caller
                .libl
                .iter()
                .filter(|l| !sys.contains(l) && !cur.contains(l))
                .cloned()
                .collect();
            o.extend_from_slice(&bin4(sys.len() as i64));
            o.extend_from_slice(&bin4(0));
            o.extend_from_slice(&bin4(cur.len() as i64));
            o.extend_from_slice(&bin4(usr.len() as i64));
            for l in sys.iter().chain(&cur).chain(&usr) {
                o.extend(field(l, 11));
            }
        }
        "JOBI0400" => {
            // From offset 62 (Retrieve Job Information API, JOBI0400):
            // the job's attributes as the system values give them
            // (clpgm::system_value); what as4tainr does not keep is
            // blank or zero (docs/fidelity-gaps.md).
            o.truncate(62);
            let ((y, mo, d), (h, mi, sec), _) = crate::rpg::dates::now();
            let now = format!(
                "{}{:02}{mo:02}{d:02}{h:02}{mi:02}{sec:02}",
                u8::from(y >= 2000),
                y % 100
            );
            o.extend(field(&now, 13)); // date and time entered system
            o.extend(field(&now, 13)); // date and time active
            o.extend(field("", 15)); // job accounting code
            o.extend(field("QDFTJOBD", 10));
            o.extend(field("QGPL", 10));
            o.extend(field("", 24)); // unit of work ID
            o.extend(field("", 8)); // mode
            o.extend(field("*RQD", 10)); // inquiry message reply
            o.extend(field("*NO", 10)); // log CL programs
            o.extend(field("*NORMAL", 10)); // break message handling
            o.extend(field("*NORMAL", 10)); // status message handling
            o.extend(field("", 13)); // device recovery action
            o.extend(field("", 10)); // DDM conversation handling
            o.extend(field("/", 1)); // date separator
            o.extend(field("*MDY", 4)); // date format
            o.extend(field("", 30)); // print text
            o.extend(field("", 26)); // submitter's job
            o.extend(field("", 20)); // submitter's message queue
            o.extend(field(":", 1)); // time separator
            o.extend_from_slice(&bin4(65535)); // coded character set ID
            o.extend(field("", 8)); // date and time scheduled
            o.extend(field("", 10)); // print key format
            o.extend(field("*HEX", 10)); // sort sequence
            o.extend(field("", 10));
            o.extend(field("ENU", 3)); // language ID
            o.extend(field("US", 2)); // country or region ID
            o.extend(field("", 1)); // completion status
            o.extend(field("0", 1)); // signed-on job
            o.extend(field("00000000", 8)); // job switches
            o.extend(field("", 10)); // message queue full action
            o.extend(field("", 1));
            o.extend_from_slice(&bin4(0)); // message queue maximum size
            o.extend_from_slice(&bin4(37)); // default coded character set ID
            o.extend(field("", 80)); // routing data
            o.extend(field(" ", 1)); // decimal format (blank: a period)
            o.extend(field("", 10)); // character identifier control
            o.extend(field("", 30)); // server type
            o.extend(field("0", 1)); // allow multiple threads
            o.extend(field("0", 1)); // job log pending
            o.extend(field("", 1));
            o.extend_from_slice(&bin4(0)); // job end reason
            o.extend_from_slice(&bin4(0)); // job type enhanced
            o.extend(field("", 13)); // date and time ended
            o.extend(field("", 1));
            o.extend(field("", 10)); // spooled file action
            o.extend_from_slice(&bin4(0)); // offset to ASP group information
            o.extend_from_slice(&bin4(0)); // number of entries
            o.extend_from_slice(&bin4(0)); // length of an entry
            o.extend(field("Q0000UTC", 10)); // time zone description
            o.extend(field("", 10)); // job log output
            o.extend(field("", 10)); // job description library ASP group
        }
        f => bail!("CPF3C21: Format name {f} is not valid."),
    }
    give(args, o)
}

/// QWDRJOBD (receiver, length, JOBD0100, qualified job description,
/// error code) (Retrieve Job Description Information API): the user,
/// job queue, text and initial library list as4tainr keeps (CRTJOBD);
/// the other attributes are the defaults CRTJOBD gives (*NO, *RQD,
/// QPRINT ... see docs/fidelity-gaps.md). The initial library list
/// follows the fixed part (CHAR(11) each), then the request data. A job
/// description not found: CPF9801.
async fn job_description(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let format = text(args, 2);
    if format != "JOBD0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (name, lib) = qualified(&args.bytes(3));
    let libs = match lib.as_str() {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![l.to_string()],
    };
    let Some(d) = crate::jobd::find(client, &libs, &name).await? else {
        bail!("CPF9801: Object {name} is not in library {lib}.");
    };
    authority::check(client, &caller.user, &d.library, &d.name, "*JOBD", Aut::Use).await?;
    let desc: String = client
        .query_opt(
            "SELECT text FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*JOBD'",
            &[&d.library, &d.name],
        )
        .await?
        .map(|r| r.get(0))
        .unwrap_or_default();
    let (jq_lib, jq) = d.jobq.split_once('/').unwrap_or(("*LIBL", &d.jobq));
    let mut o = vec![0u8; 8];
    o.extend(field(&d.name, 10));
    o.extend(field(&d.library, 10));
    o.extend(field(&d.user, 10));
    o.extend(field("*SYSVAL", 8)); // job date
    o.extend(field("00000000", 8)); // job switches
    o.extend(field(jq, 10));
    o.extend(field(jq_lib, 10));
    o.extend(field("5", 2)); // job queue priority
    o.extend(field("*NO", 10)); // hold on job queue
    o.extend(field("*USRPRF", 10)); // output queue
    o.extend(field("", 10));
    o.extend(field("5", 2)); // output priority
    o.extend(field("*USRPRF", 10)); // printer device
    o.extend(field("*SYSVAL", 30)); // print text
    o.extend_from_slice(&bin4(-1)); // syntax check severity (*NOCHK)
    o.extend_from_slice(&bin4(30)); // end severity
    o.extend_from_slice(&bin4(0)); // message logging severity
    o.extend(field("4", 1)); // message logging level
    o.extend(field("*NOLIST", 10)); // message logging text
    o.extend(field("*NO", 10)); // log CL program commands
    o.extend(field("*RQD", 10)); // inquiry message reply
    o.extend(field("*SYSVAL", 13)); // device recovery action
    o.extend(field("*SYSVAL", 10)); // time slice end pool
    o.extend(field("*USRPRF", 15)); // accounting code
    o.extend(field("QCMDB", 80)); // routing data
    o.extend(field(&desc, 50));
    o.extend(field("", 1));
    let libl: Vec<String> = match &d.inllibl {
        Some(l) => l.clone(),
        None => crate::commands::USER_LIBRARY_LIST
            .iter()
            .map(|x| x.to_string())
            .collect(),
    };
    // The fixed part ends at 452 (after the log output); the library
    // list and request data follow it.
    let fixed = 452usize;
    let rqs = e(&d.rqsdta);
    o.extend_from_slice(&bin4(fixed as i64)); // offset to the initial library list
    o.extend_from_slice(&bin4(libl.len() as i64));
    o.extend_from_slice(&bin4((fixed + 11 * libl.len()) as i64)); // offset to the request data
    o.extend_from_slice(&bin4(rqs.len() as i64));
    o.extend_from_slice(&bin4(0)); // job message queue maximum size
    o.extend(field("*SYSVAL", 10)); // job message queue full action
    o.extend(field("", 10)); // date (CYYMMDD)
    o.extend(field("*NO", 10)); // allow multiple threads
    o.extend(field("*SYSVAL", 10)); // spooled file action
    o.extend_from_slice(&bin4(0)); // offset to the ASP group information
    o.extend_from_slice(&bin4(0));
    o.extend_from_slice(&bin4(0));
    o.extend(field("*KEEP", 10)); // DDM conversation
    o.extend(field("*SYSVAL", 10)); // job log output
    debug_assert_eq!(o.len(), fixed);
    for l in &libl {
        o.extend(field(l, 11));
    }
    o.extend(rqs);
    give(args, o)
}

/// The release as4tainr's behavior follows (its golden outputs come from
/// IBM i 7.5).
pub const RELEASE: &str = "V7R5M0";

/// QSZRTVPR (receiver, length, PRDR0100, product information, error
/// code) (Retrieve Product Information API) for the operating system
/// (*OPSYS or 5770SS1, *CUR or its release, option 0000, *CODE): product
/// 5770SS1, release V7R5M0, option, load ID *CODE, load type *CODE,
/// symbolic load state *INSTALLED, load error indicator *NONE, load
/// state 90; the rest of PRDR0100 blank or zero. Other products are
/// CPF0C1F (not installed here).
fn product_information(args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let format = text(args, 2);
    if format != "PRDR0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let info = a(&args.bytes(3));
    let info = format!("{info:<27}");
    let product = info[0..7].trim().to_ascii_uppercase();
    let release = info[7..13].trim().to_ascii_uppercase();
    let option = info[13..17].trim().to_string();
    if !matches!(product.as_str(), "*OPSYS" | "5770SS1")
        || !matches!(release.as_str(), "*CUR" | "*ONLY" | RELEASE)
    {
        bail!("CPF0C1F: Product {product} {release} is not installed.");
    }
    let mut o = vec![0u8; 12];
    o.extend(field("5770SS1", 7));
    o.extend(field(RELEASE, 6));
    o.extend(field(if option.is_empty() { "0000" } else { &option }, 4));
    o.extend(field("*CODE", 4));
    o.extend(field("*CODE", 10));
    o.extend(field("*INSTALLED", 10));
    o.extend(field("*NONE", 10));
    o.extend(field("90", 2));
    o.extend(field("1", 1));
    o.extend(field("", 2));
    o.extend(field("", 14));
    o.extend(field("", 2));
    o.extend_from_slice(&bin4(0));
    o.extend(field("2924", 4));
    o.extend(field(RELEASE, 6));
    o.extend(field(RELEASE, 6));
    o.extend(field("1", 1));
    give(args, o)
}
