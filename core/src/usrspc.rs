//! User spaces (*USRSPC): QUSCRTUS, QUSDLTUS, QUSRTVUS, QUSCHGUS,
//! QUSRUSAT and, from RPG, QUSPTRUS (`rpg/interp.rs`).
//!
//! Written from IBM's documentation (Object APIs, "User Space APIs":
//! Create User Space, Change User Space, Retrieve User Space Attributes;
//! their parameter lists and the SPCA0100 format). A user space is bytes
//! (up to 16,776,704) kept with its extended attribute, the value new
//! bytes get and whether it grows when written past its end. Error code
//! parameters work as for the data queue APIs (`dtaq::call_api`).
//!
//! The list APIs QUSLMBR and QUSLOBJ put their lists in a user space
//! after the generic header (format 0100: the user area in bytes 0-63,
//! the offsets and sizes of the input parameter, header and list data
//! sections at 108-128, the number of entries at 132 and the size of
//! each at 136; IBM's "List APIs" and "User space format for list APIs"),
//! growing the space when the list needs it.

use crate::authority::{self, Aut};
use crate::catalog::validate_name;
use crate::dtaq::{ApiArgs, Caller};
use crate::rpg::data::{a, e};
use anyhow::anyhow;
use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.user_spaces (
    library  text NOT NULL,
    name     text NOT NULL,
    extattr  text NOT NULL DEFAULT '',
    data     bytea NOT NULL,
    initval  int NOT NULL DEFAULT 0,
    autoext  boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, name)
);
"#;

/// The largest user space (QUSCRTUS initial size).
pub const MAX_SIZE: i64 = 16_776_704;

/// The user space APIs done here (QUSPTRUS is the RPG engine's).
pub fn is_api(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "QUSCRTUS"
            | "QUSDLTUS"
            | "QUSRTVUS"
            | "QUSCHGUS"
            | "QUSRUSAT"
            | "QUSCUSAT"
            | "QUSLMBR"
            | "QUSLOBJ"
            | "QUSLFLD"
            | "QUSLSPL"
            | "QBNLSPGM"
    )
}

fn error_code_param(name: &str) -> Option<usize> {
    match name {
        "QUSCRTUS" => Some(7),
        "QUSDLTUS" => Some(1),
        "QUSRTVUS" => Some(4),
        "QUSCHGUS" => Some(5),
        "QUSRUSAT" => Some(4),
        "QUSCUSAT" => Some(3),
        "QUSLMBR" => Some(5),
        "QUSLOBJ" => Some(4),
        "QUSLFLD" => Some(5),
        "QUSLSPL" => Some(6),
        "QBNLSPGM" => Some(3),
        _ => None,
    }
}

/// Call a user space API (errors in the error code parameter when it has
/// room for them, as `dtaq::call_api`).
pub async fn call_api(
    client: &Client,
    caller: &Caller,
    name: &str,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    let name = name.to_ascii_uppercase();
    let r = match name.as_str() {
        "QUSCRTUS" => create(client, caller, args).await,
        "QUSDLTUS" => delete_api(client, caller, args).await,
        "QUSRTVUS" => retrieve(client, caller, args).await,
        "QUSCHGUS" => change(client, caller, args).await,
        "QUSRUSAT" => attributes(client, caller, args).await,
        "QUSCUSAT" => change_attributes(client, caller, args).await,
        "QUSLMBR" => list_members(client, caller, args).await,
        "QUSLOBJ" => list_objects(client, caller, args).await,
        "QUSLFLD" => list_fields(client, caller, args).await,
        "QUSLSPL" => list_spooled_files(client, caller, args).await,
        "QBNLSPGM" => list_service_program(client, caller, args).await,
        _ => bail!("{name} is not a user space API."),
    };
    crate::dtaq::error_code(args, error_code_param(&name), r)
}

/// A qualified name (name in bytes 1-10, library in 11-20).
fn qualified(b: &[u8]) -> (String, String) {
    let name = a(b.get(..10).unwrap_or(b)).trim().to_ascii_uppercase();
    let lib = a(b.get(10..20).unwrap_or(&[])).trim().to_ascii_uppercase();
    (name, lib)
}

fn text(args: &dyn ApiArgs, i: usize) -> String {
    a(&args.bytes(i)).trim().to_ascii_uppercase()
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

/// A user space: its library and its attributes.
pub struct Space {
    pub library: String,
    pub name: String,
    pub size: i64,
    pub initval: u8,
    pub autoext: bool,
}

/// Find a user space by name and library (*LIBL, *CURLIB, a name).
pub async fn find(client: &Client, name: &str, lib: &str, caller: &Caller) -> Result<Space> {
    let libs = match lib {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![l.to_string()],
    };
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT length(data)::bigint, initval, autoext FROM as4tainr.user_spaces
                  WHERE library=$1 AND name=$2",
                &[l, &name],
            )
            .await?
        {
            return Ok(Space {
                library: l.clone(),
                name: name.to_string(),
                size: r.get(0),
                initval: r.get::<_, i32>(1) as u8,
                autoext: r.get(2),
            });
        }
    }
    if !lib.starts_with('*')
        && !lib.is_empty()
        && !crate::catalog::library_exists(client, lib).await?
    {
        bail!("CPF9810: Library {lib} not found.");
    }
    bail!(
        "CPF9801: Object {name} in library {} not found.",
        if lib.is_empty() { "*LIBL" } else { lib }
    )
}

/// QUSCRTUS (qualified name, extended attribute, initial size, initial
/// value, public authority, text [, replace, error code [, domain
/// [, transfer size, alignment]]]).
async fn create(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 6)?;
    let (name, lib) = qualified(&args.bytes(0));
    let lib = match lib.as_str() {
        "*CURLIB" => caller.curlib.clone(),
        l => l.to_string(),
    };
    let lib = validate_name("library", &lib)?;
    let name = validate_name("user space", &name)?;
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF9810: Library {lib} not found.");
    }
    let extattr = text(args, 1);
    let size = args.num(2)?;
    if !(1..=MAX_SIZE).contains(&size) {
        bail!("CPF3CF2: Initial size {size} is not valid for user space {name} (1 to {MAX_SIZE} bytes).");
    }
    let init = args.bytes(3).first().copied().unwrap_or(0);
    let public = text(args, 4);
    let desc = a(&args.bytes(5)).trim_end().to_string();
    let replace = if args.count() > 6 {
        match text(args, 6).as_str() {
            "*YES" => true,
            "*NO" | "" => false,
            v => bail!("CPF3CF2: Replace value {v} is not valid."),
        }
    } else {
        false
    };
    let data = vec![init; size as usize];
    let exists = client
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*USRSPC'",
            &[&lib, &name],
        )
        .await?
        .is_some();
    if exists {
        if !replace {
            bail!("CPF9870: Object {name} type *USRSPC already exists in library {lib}.");
        }
        // Replaced: the new space keeps the old one's authorities.
        authority::check(client, &caller.user, &lib, &name, "*USRSPC", Aut::All).await?;
        client
            .execute(
                "UPDATE as4tainr.user_spaces SET extattr=$3, data=$4, initval=$5, autoext=false
                  WHERE library=$1 AND name=$2",
                &[&lib, &name, &extattr, &data, &(init as i32)],
            )
            .await?;
        client
            .execute(
                "UPDATE as4tainr.objects SET text=$3, attribute=$4
                  WHERE library=$1 AND name=$2 AND objtype='*USRSPC'",
                &[&lib, &name, &desc, &extattr],
            )
            .await?;
        return Ok(());
    }
    client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             VALUES ($1, $2, '*USRSPC', $3, $4)",
            &[&lib, &name, &extattr, &desc],
        )
        .await?;
    client
        .execute(
            "INSERT INTO as4tainr.user_spaces (library, name, extattr, data, initval)
             VALUES ($1, $2, $3, $4, $5)",
            &[&lib, &name, &extattr, &data, &(init as i32)],
        )
        .await?;
    authority::set_owner(client, &lib, &name, "*USRSPC", &caller.user).await?;
    let public = match public.as_str() {
        "*ALL" | "*CHANGE" | "*USE" | "*EXCLUDE" => public,
        // *LIBCRTAUT and authorization lists: *CHANGE, the shipped
        // CRTAUT of libraries (as4tainr keeps no authorization lists).
        _ => "*CHANGE".into(),
    };
    authority::grant(
        client,
        &lib,
        &name,
        "*USRSPC",
        "*PUBLIC",
        Aut::parse(&public)?,
    )
    .await?;
    Ok(())
}

/// Delete a user space (QUSDLTUS; DLTLIB through the catalog).
pub async fn delete(client: &Client, library: &str, name: &str) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainr.user_spaces WHERE library=$1 AND name=$2",
            &[&library, &name],
        )
        .await?;
    authority::forget(client, library, name, "*USRSPC").await?;
    client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*USRSPC'",
            &[&library, &name],
        )
        .await?;
    Ok(())
}

/// QUSDLTUS (qualified name, error code).
async fn delete_api(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 2)?;
    let (name, lib) = qualified(&args.bytes(0));
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::All,
    )
    .await?;
    delete(client, &s.library, &s.name).await
}

/// The bytes of a user space from 1-based `start`, `len` of them.
pub async fn read(client: &Client, s: &Space, start: i64, len: i64) -> Result<Vec<u8>> {
    if start < 1 || len < 0 || start - 1 + len > s.size {
        bail!(
            "CPF3CF2: Starting position {start} and length {len} are outside user space {} in {} ({} bytes).",
            s.name,
            s.library,
            s.size
        );
    }
    Ok(client
        .query_one(
            "SELECT substring(data FROM $3::int FOR $4::int) FROM as4tainr.user_spaces
              WHERE library=$1 AND name=$2",
            &[&s.library, &s.name, &(start as i32), &(len as i32)],
        )
        .await?
        .get(0))
}

/// Write bytes at 1-based `start`; past the end only when the space
/// extends itself (QUSCUSAT automatic extendibility), the new bytes
/// getting its initial value.
pub async fn write(client: &Client, s: &Space, start: i64, bytes: &[u8]) -> Result<()> {
    let end = start - 1 + bytes.len() as i64;
    if start < 1 || (end > s.size && !s.autoext) || end > MAX_SIZE {
        bail!(
            "CPF3CF2: Starting position {start} and length {} are outside user space {} in {} ({} bytes).",
            bytes.len(),
            s.name,
            s.library,
            s.size
        );
    }
    if end > s.size {
        let pad = vec![s.initval; (end - s.size) as usize];
        client
            .execute(
                "UPDATE as4tainr.user_spaces SET data = data || $3 WHERE library=$1 AND name=$2",
                &[&s.library, &s.name, &pad],
            )
            .await?;
    }
    client
        .execute(
            "UPDATE as4tainr.user_spaces SET data = overlay(data PLACING $3 FROM $4::int)
              WHERE library=$1 AND name=$2",
            &[&s.library, &s.name, &bytes, &(start as i32)],
        )
        .await?;
    Ok(())
}

/// All of a user space (QUSPTRUS).
pub async fn contents(client: &Client, s: &Space) -> Result<Vec<u8>> {
    Ok(client
        .query_one(
            "SELECT data FROM as4tainr.user_spaces WHERE library=$1 AND name=$2",
            &[&s.library, &s.name],
        )
        .await?
        .get(0))
}

/// Replace all of a user space's bytes (what a program changed through
/// QUSPTRUS's pointer).
pub async fn put_contents(client: &Client, library: &str, name: &str, data: &[u8]) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.user_spaces SET data=$3 WHERE library=$1 AND name=$2",
            &[&library, &name, &data],
        )
        .await?;
    Ok(())
}

/// QUSRTVUS (qualified name, starting position, length, receiver [,
/// error code]).
async fn retrieve(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let (name, lib) = qualified(&args.bytes(0));
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::Use,
    )
    .await?;
    let start = args.num(1)?;
    let len = args.num(2)?;
    let b = read(client, &s, start, len).await?;
    args.set_bytes(3, &b);
    Ok(())
}

/// QUSCHGUS (qualified name, starting position, length, input data,
/// force [, error code [, returned library]]).
async fn change(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    let (name, lib) = qualified(&args.bytes(0));
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::Change,
    )
    .await?;
    let start = args.num(1)?;
    let len = args.num(2)?;
    if len <= 0 {
        bail!("CPF3CF2: Length of data {len} is not valid; it must be greater than 0.");
    }
    match text(args, 4).as_str() {
        "0" | "1" | "2" => {}
        v => bail!("CPF3CF2: Force changes value {v} is not valid (0, 1 or 2)."),
    }
    let mut data = args.bytes(3);
    data.resize(len as usize, 0x40);
    write(client, &s, start, &data).await?;
    if args.count() > 6 {
        let mut l = e(&s.library);
        l.resize(10, 0x40);
        args.set_bytes(6, &l);
    }
    Ok(())
}

/// QUSRUSAT (receiver, length, format SPCA0100, qualified name, error
/// code).
async fn attributes(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    let len = args.num(1)?;
    if len < 8 {
        bail!("CPF3C24: Length of the receiver variable is not valid.");
    }
    let format = text(args, 2);
    if format != "SPCA0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (name, lib) = qualified(&args.bytes(3));
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::Use,
    )
    .await?;
    let mut r = Vec::with_capacity(24);
    r.extend_from_slice(&0i32.to_be_bytes());
    r.extend_from_slice(&24i32.to_be_bytes());
    r.extend_from_slice(&(s.size as i32).to_be_bytes());
    r.extend(e(if s.autoext { "1" } else { "0" }));
    r.push(s.initval);
    let mut l = e(&s.library);
    l.resize(10, 0x40);
    r.extend(l);
    let ret = (len as usize).min(r.len());
    r[..4].copy_from_slice(&(ret as i32).to_be_bytes());
    args.set_bytes(0, &r[..ret]);
    Ok(())
}

/// QUSCUSAT (returned library, qualified name, attributes to change,
/// error code). The attributes are variable length records after their
/// count: key, length of data, data. Keys: 1 space size (Binary(4)), 2
/// initial value (Char(1)), 3 automatic extendibility (Char(1), 0 or 1);
/// the others are accepted and change nothing here.
async fn change_attributes(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let (name, lib) = qualified(&args.bytes(1));
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::Change,
    )
    .await?;
    let rec = args.bytes(2);
    let bin = |o: usize| -> Result<i64> {
        rec.get(o..o + 4)
            .map(|b| i32::from_be_bytes(b.try_into().unwrap()) as i64)
            .ok_or_else(|| anyhow!("CPF3CF2: The attributes to change are cut short."))
    };
    let count = bin(0)?;
    let mut o = 4usize;
    for _ in 0..count.max(0) {
        let key = bin(o)?;
        let len = bin(o + 4)?.max(0) as usize;
        let data = rec
            .get(o + 8..o + 8 + len)
            .ok_or_else(|| anyhow!("CPF3CF2: The attributes to change are cut short."))?
            .to_vec();
        match key {
            1 => {
                let size = data
                    .get(..4)
                    .map(|b| i32::from_be_bytes(b.try_into().unwrap()) as i64)
                    .unwrap_or(0);
                if !(1..=MAX_SIZE).contains(&size) {
                    bail!("CPF3CF2: Space size {size} is not valid (1 to {MAX_SIZE} bytes).");
                }
                let mut bytes = contents(client, &s).await?;
                bytes.resize(size as usize, s.initval);
                put_contents(client, &s.library, &s.name, &bytes).await?;
            }
            2 => {
                let v = data.first().copied().unwrap_or(0) as i32;
                client
                    .execute(
                        "UPDATE as4tainr.user_spaces SET initval=$3 WHERE library=$1 AND name=$2",
                        &[&s.library, &s.name, &v],
                    )
                    .await?;
            }
            3 => {
                let on = match a(&data).as_str() {
                    "1" => true,
                    "0" => false,
                    v => bail!("CPF3CF2: Automatic extendibility value {v} is not valid (0 or 1)."),
                };
                client
                    .execute(
                        "UPDATE as4tainr.user_spaces SET autoext=$3 WHERE library=$1 AND name=$2",
                        &[&s.library, &s.name, &on],
                    )
                    .await?;
            }
            _ => {}
        }
        o += 8 + len;
    }
    let mut l = e(&s.library);
    l.resize(10, 0x40);
    args.set_bytes(0, &l);
    Ok(())
}

/// Date and time as list entries give them: CYYMMDDHHMMSS (C 0 for
/// 19xx, 1 for 20xx).
fn cyymmdd(year: i32, rest: &str) -> String {
    format!("{}{rest}", if year >= 2000 { 1 } else { 0 })
}

fn field(s: &str, n: usize) -> Vec<u8> {
    let mut b = e(s);
    b.resize(n, 0x40);
    b
}

/// Put a list in a user space: the generic header (format 0100) at
/// offset 0 (the user area, bytes 0-63, kept), then the input parameter
/// section, the header section and the list entries.
#[allow(clippy::too_many_arguments)]
async fn write_list(
    client: &Client,
    caller: &Caller,
    space: &[u8],
    api: &str,
    format: &str,
    input: &[u8],
    header: &[u8],
    entries: &[Vec<u8>],
    entry_size: usize,
) -> Result<()> {
    let (name, lib) = qualified(space);
    let s = find(client, &name, &lib, caller).await?;
    authority::check(
        client,
        &caller.user,
        &s.library,
        &s.name,
        "*USRSPC",
        Aut::Change,
    )
    .await?;
    const GENERIC: usize = 192;
    let input_off = GENERIC;
    let header_off = input_off + input.len();
    let list_off = header_off + header.len();
    let list_size = entries.len() * entry_size;
    let used = list_off + list_size;
    let ((y, mo, d), (h, mi, sec), _) = crate::rpg::dates::now();
    let created = cyymmdd(
        y,
        &format!("{:02}{mo:02}{d:02}{h:02}{mi:02}{sec:02}", y % 100),
    );
    let mut g = Vec::with_capacity(GENERIC);
    g.extend_from_slice(&(GENERIC as i32).to_be_bytes()); // size of generic header
    g.extend(field("0100", 4)); // structure release and level
    g.extend(field(format, 8));
    g.extend(field(api, 10));
    g.extend(field(&created, 13));
    g.extend(e("C")); // information status: complete
    for n in [
        used,
        input_off,
        input.len(),
        header_off,
        header.len(),
        list_off,
        list_size,
        entries.len(),
        entry_size,
    ] {
        g.extend_from_slice(&(n as i32).to_be_bytes());
    }
    g.extend_from_slice(&37i32.to_be_bytes()); // CCSID of the entries
    g.extend(field("", 2)); // country or region ID
    g.extend(field("", 3)); // language ID
    g.extend(e("0")); // not a subsetted list
    g.resize(GENERIC - 64, 0x00);
    let mut bytes = contents(client, &s).await?;
    if bytes.len() < used {
        if used as i64 > MAX_SIZE {
            bail!(
                "CPF3CAA: The list is too large for user space {} in {}.",
                s.name,
                s.library
            );
        }
        bytes.resize(used, s.initval);
    }
    bytes[64..GENERIC].copy_from_slice(&g);
    bytes[input_off..header_off].copy_from_slice(input);
    bytes[header_off..list_off].copy_from_slice(header);
    for (i, en) in entries.iter().enumerate() {
        let o = list_off + i * entry_size;
        let n = en.len().min(entry_size);
        bytes[o..o + n].copy_from_slice(&en[..n]);
    }
    put_contents(client, &s.library, &s.name, &bytes).await
}

/// QBNLSPGM (qualified user space, format, qualified service program
/// [, error code]) (List Service Program Information API), format
/// SPGL0610, the procedures it exports with their long names: each entry
/// gives its size, the service program and library, the name's CCSID,
/// the offset from the start of the user space to the name and its
/// length (as4tainr's entries: those fields, 10 bytes of argument
/// optimization *NO and 2 reserved, then the name; all one size).
async fn list_service_program(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 3)?;
    let format = text(args, 1);
    if format != "SPGL0610" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (sp, splib) = qualified(&args.bytes(2));
    let (lib, b) = crate::ile::find_srvpgm(client, &caller.libl, &splib, &sp).await?;
    authority::check(client, &caller.user, &lib, &sp, "*SRVPGM", Aut::Use).await?;
    let space = args.bytes(0);
    let mut input = space.get(..20).unwrap_or(&space).to_vec();
    input.resize(20, 0x40);
    input.extend(field(&format, 8));
    input.extend(field(&sp, 10));
    input.extend(field(&lib, 10));
    const FIXED: usize = 48;
    let longest = b.exports.iter().map(|x| x.len()).max().unwrap_or(0);
    let size = (FIXED + longest.max(1)).div_ceil(4) * 4;
    let list_off = 192 + input.len();
    let entries: Vec<Vec<u8>> = b
        .exports
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut en = (size as i32).to_be_bytes().to_vec();
            en.extend(field(&sp, 10));
            en.extend(field(&lib, 10));
            en.extend_from_slice(&37i32.to_be_bytes());
            en.extend_from_slice(&((list_off + i * size + FIXED) as i32).to_be_bytes());
            en.extend_from_slice(&(name.len() as i32).to_be_bytes());
            en.extend(field("*NO", 10));
            en.extend([0x40, 0x40]);
            en.extend(e(name));
            en.resize(size, 0x40);
            en
        })
        .collect();
    write_list(
        client,
        caller,
        &space,
        "QBNLSPGM",
        &format,
        &input,
        &[],
        &entries,
        size,
    )
    .await
}

/// QUSLMBR (qualified user space, format MBRL0100 or MBRL0200,
/// qualified file, member (a name, generic*, *ALL), override processing
/// [, error code]). MBRL0100: the member name (10); MBRL0200: name,
/// source type, creation and last change dates (CYYMMDDHHMMSS) and text
/// (96 bytes).
async fn list_members(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    let format = text(args, 1);
    let size = match format.as_str() {
        "MBRL0100" => 10,
        "MBRL0200" => 96,
        f => bail!("CPF3C21: Format name {f} is not valid."),
    };
    let (file, flib) = qualified(&args.bytes(2));
    let libs = match flib.as_str() {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![l.to_string()],
    };
    let mut found = None;
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT attribute, text, extract(year FROM created)::int,
                        to_char(created, 'MMDDHH24MISS'), to_char(created, 'YY')
                   FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                &[l, &file],
            )
            .await?
        {
            found = Some((l.clone(), r));
            break;
        }
    }
    let Some((lib, r)) = found else {
        bail!("CPF9812: File {file} in library {flib} not found.");
    };
    authority::check(client, &caller.user, &lib, &file, "*FILE", Aut::Use).await?;
    let pattern = text(args, 3);
    let pattern = if pattern.is_empty() {
        "*ALL".into()
    } else {
        pattern
    };
    let attr: String = r.get(0);
    let mut members: Vec<(String, String, String, String)> = Vec::new();
    if attr == "PF-SRC" {
        for m in client
            .query(
                "SELECT member, srctype, text, extract(year FROM changed)::int,
                        to_char(changed, 'YYMMDDHH24MISS')
                   FROM as4tainr.source_members WHERE library=$1 AND file=$2 ORDER BY member",
                &[&lib, &file],
            )
            .await?
        {
            let stamp = cyymmdd(m.get(3), &m.get::<_, String>(4));
            members.push((m.get(0), m.get(1), stamp, m.get(2)));
        }
    } else if attr == "PF" || attr == "LF" {
        let stamp = cyymmdd(
            r.get(2),
            &format!("{}{}", r.get::<_, String>(4), r.get::<_, String>(3)),
        );
        members.push((file.clone(), String::new(), stamp, r.get(1)));
    }
    let entries: Vec<Vec<u8>> = members
        .iter()
        .filter(|m| crate::catalog::name_matches(&pattern, &m.0))
        .map(|(name, ty, stamp, txt)| {
            let mut en = field(name, 10);
            if size == 96 {
                en.extend(field(ty, 10));
                en.extend(field(stamp, 13));
                en.extend(field(stamp, 13));
                en.extend(field(txt, 50));
            }
            en
        })
        .collect();
    let mut input = args.bytes(0);
    input.resize(20, 0x40);
    input.extend(field(&format, 8));
    input.extend(field(&file, 10));
    input.extend(field(&flib, 10));
    input.extend(field(&pattern, 10));
    input.extend(field(&text(args, 4), 1));
    // Header section: the file and library used.
    let mut header = field(&file, 10);
    header.extend(field(&lib, 10));
    write_list(
        client,
        caller,
        &args.bytes(0),
        "QUSLMBR",
        &format,
        &input,
        &header,
        &entries,
        size,
    )
    .await
}

/// QUSLOBJ (qualified user space, format OBJL0100 or OBJL0200,
/// qualified object name (name, generic*, *ALL; library *LIBL, *CURLIB,
/// *USRLIBL, *ALL, *ALLUSR or a name), object type [, error code]).
/// OBJL0100: name, library, type (30 bytes); OBJL0200 adds the
/// information status, extended attribute, text and a reserved part
/// (108 bytes).
async fn list_objects(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let format = text(args, 1);
    let size = match format.as_str() {
        "OBJL0100" => 30,
        "OBJL0200" => 108,
        f => bail!("CPF3C21: Format name {f} is not valid."),
    };
    let (obj, olib) = qualified(&args.bytes(2));
    let objtype = text(args, 3);
    let libs: Vec<String> = match olib.as_str() {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        "*ALL" => crate::catalog::list_libraries(client).await?,
        "*ALLUSR" => crate::catalog::list_libraries(client)
            .await?
            .into_iter()
            .filter(|l| !l.starts_with('Q'))
            .collect(),
        l => {
            if !crate::catalog::library_exists(client, l).await? {
                bail!("CPF9810: Library {l} not found.");
            }
            vec![l.to_string()]
        }
    };
    let mut entries = Vec::new();
    for l in &libs {
        let Ok(objs) = crate::catalog::list_objects(client, l).await else {
            continue;
        };
        for o in objs {
            if !crate::catalog::name_matches(&obj, &o.name)
                || !(objtype == "*ALL" || objtype == o.objtype)
            {
                continue;
            }
            let mut en = field(&o.name, 10);
            en.extend(field(l, 10));
            en.extend(field(&o.objtype, 10));
            if size == 108 {
                en.extend(e(" ")); // information status: no error
                en.extend(field(&o.attribute, 10));
                en.extend(field(&o.text, 50));
                en.extend(field("", 17));
            }
            entries.push(en);
        }
    }
    let mut input = args.bytes(0);
    input.resize(20, 0x40);
    input.extend(field(&format, 8));
    input.extend(field(&obj, 10));
    input.extend(field(&olib, 10));
    input.extend(field(&objtype, 10));
    let header = field(if libs.len() == 1 { &libs[0] } else { "" }, 10);
    write_list(
        client,
        caller,
        &args.bytes(0),
        "QUSLOBJ",
        &format,
        &input,
        &header,
        &entries,
        size,
    )
    .await
}

/// QUSLFLD (qualified user space, FLDL0100, qualified file, record
/// format (a name or *FIRST), override processing [, error code]). The
/// header section: file and library used, file type, record format,
/// record length, format level identifier, text, and whether the format
/// has varying-length, graphic, date and time, and null-capable fields.
/// FLDL0100 entries (560 bytes): name, data type, use (B), output and
/// input buffer positions, length in bytes, digits and decimal
/// positions, text, column heading 1 (the field name), the internal and
/// alternative names, null values allowed, date and time format and
/// separator, varying length, CCSIDs.
async fn list_fields(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    use crate::dds::DataType;
    need(args, 5)?;
    let format = text(args, 1);
    if format != "FLDL0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let (file, flib) = qualified(&args.bytes(2));
    let libs = match flib.as_str() {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![l.to_string()],
    };
    let mut found = None;
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT attribute, text FROM as4tainr.objects
                  WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                &[l, &file],
            )
            .await?
        {
            found = Some((l.clone(), r.get::<_, String>(0), r.get::<_, String>(1)));
            break;
        }
    }
    let Some((lib, attr, ftext)) = found else {
        if !flib.starts_with('*') && !crate::catalog::library_exists(client, &flib).await? {
            bail!("CPF9810: Library {flib} not found.");
        }
        bail!("CPF9812: File {file} in library {flib} not found.");
    };
    authority::check(client, &caller.user, &lib, &file, "*FILE", Aut::Use).await?;
    let (fmt, fields, _) = crate::objdesc::record_format(client, &lib, &file, &attr)
        .await
        .map_err(|_| anyhow!("CPF3C22: Cannot get information about file {file} in {lib}."))?;
    let want = text(args, 3);
    if want != "*FIRST" && !want.is_empty() && want != fmt {
        bail!("CPF3C28: Record format {want} not found in file {file} in {lib}.");
    }
    let reclen: u32 = fields.iter().map(|f| f.buffer_bytes()).sum();
    let yn = |b: bool| if b { "1" } else { "0" };
    let mut header = field(&file, 10);
    header.extend(field(&lib, 10));
    header.extend(field("*DATA", 10)); // file type (a source file is *DATA too)
    header.extend(field(&fmt, 10));
    header.extend_from_slice(&(reclen as i32).to_be_bytes());
    header.extend(field(&crate::objdesc::level_id(&fmt, &fields), 13));
    header.extend(field(&ftext, 50));
    header.extend(field("", 1));
    header.extend_from_slice(&37i32.to_be_bytes());
    header.extend(e(yn(fields.iter().any(|f| f.varlen))));
    header.extend(e(yn(fields
        .iter()
        .any(|f| f.data_type == DataType::Graphic))));
    header.extend(e(yn(fields.iter().any(|f| {
        matches!(
            f.data_type,
            DataType::Date | DataType::Time | DataType::Timestamp
        )
    }))));
    header.extend(e(yn(fields.iter().any(|f| f.null_capable))));
    let bin = |n: i64| (n as i32).to_be_bytes();
    let mut entries = Vec::new();
    let mut pos = 1i64;
    for f in &fields {
        let code = match f.data_type {
            DataType::Clob => "2",
            DataType::Blob => "1",
            DataType::Dbclob => "3",
            DataType::Decfloat => "6",
            DataType::Xml => "7",
            t => t.code(),
        };
        let numeric = matches!(
            f.data_type,
            DataType::Packed | DataType::Zoned | DataType::Binary | DataType::Float
        );
        let size = f.buffer_bytes() as i64;
        let mut en = field(&f.name, 10);
        en.extend(field(code, 1));
        en.extend(e("B"));
        en.extend_from_slice(&bin(pos));
        en.extend_from_slice(&bin(pos));
        en.extend_from_slice(&bin(size));
        en.extend_from_slice(&bin(if numeric { f.length as i64 } else { 0 }));
        en.extend_from_slice(&bin(if numeric { f.decimals as i64 } else { 0 }));
        en.extend(field(&f.text, 50));
        en.extend(field("", 2)); // edit code
        en.extend_from_slice(&bin(0)); // edit word length
        en.extend(field("", 64));
        en.extend(field(&f.name, 20)); // column heading 1
        en.extend(field("", 40)); // column headings 2 and 3
        en.extend(field(&f.name, 10)); // internal name
        let alias = f.alias.clone().unwrap_or_default();
        en.extend(field(&alias, 30));
        en.extend_from_slice(&bin(alias.len() as i64));
        en.extend_from_slice(&bin(0)); // DBCS characters
        en.extend(e(yn(f.null_capable)));
        en.extend(e("0")); // not a host variable
        let (dfmt, dsep) = match f.data_type {
            DataType::Date => ("*ISO", "-"),
            DataType::Time => ("*ISO", "."),
            DataType::Timestamp => ("", ""),
            _ => ("", ""),
        };
        en.extend(field(dfmt, 4));
        en.extend(field(dsep, 1));
        en.extend(e(yn(f.varlen)));
        en.extend_from_slice(&bin(if f.text.is_empty() { 0 } else { 37 }));
        let chars = matches!(f.data_type, DataType::Char | DataType::Clob);
        en.extend_from_slice(&bin(if chars { 37 } else { 0 }));
        en.extend_from_slice(&bin(37)); // column headings CCSID
        en.extend_from_slice(&bin(0)); // edit words CCSID
        en.resize(560, 0);
        entries.push(en);
        pos += size;
    }
    let mut input = args.bytes(0);
    input.resize(20, 0x40);
    input.extend(field(&format, 8));
    input.extend(field(&file, 10));
    input.extend(field(&flib, 10));
    input.extend(field(&want, 10));
    input.extend(field(&text(args, 4), 1));
    write_list(
        client,
        caller,
        &args.bytes(0),
        "QUSLFLD",
        &format,
        &input,
        &header,
        &entries,
        560,
    )
    .await
}

/// QUSLSPL (qualified user space, SPLF0100, user name (*CURRENT, *ALL,
/// a name), qualified output queue (*ALL or a name), form type (*ALL),
/// user data (*ALL or a value) [, error code [, qualified job name ('*'
/// this job; blank: any)]]). SPLF0100 entries (88 bytes): user, output
/// queue and its library, form type, user data, internal job and
/// spooled file identifiers (for QUSRSPLA), ASP. Header section (110
/// bytes): user, output queue, user space, job, dates used.
async fn list_spooled_files(
    client: &Client,
    caller: &Caller,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    need(args, 6)?;
    let format = text(args, 1);
    if format != "SPLF0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let user = match text(args, 2).as_str() {
        "*CURRENT" | "" => caller.user.clone(),
        u => u.to_string(),
    };
    let (outq, outqlib) = qualified(&args.bytes(3));
    if outq == "*ALL" && !outqlib.is_empty() {
        bail!("CPF3C30: The library must be blank for output queue *ALL.");
    }
    let udata = text(args, 5);
    let job: Option<i64> = if args.count() > 7 {
        let q = a(&args.bytes(7));
        let name = q.get(..10).unwrap_or("").trim().to_ascii_uppercase();
        match name.as_str() {
            "" => None,
            "*" => caller.job.as_ref().map(|j| j.number),
            _ => {
                let n: i64 = q.get(20..26).unwrap_or("").trim().parse().unwrap_or(-1);
                Some(n)
            }
        }
    } else {
        None
    };
    // Another user's spooled files: *SPLCTL (or *ALLOBJ).
    if user != caller.user
        && !crate::users::has_special(client, &caller.user, "*SPLCTL").await?
        && !crate::users::has_special(client, &caller.user, "*ALLOBJ").await?
    {
        bail!("CPF9802: Not authorized to the spooled files of user {user}.");
    }
    let rows = client
        .query(
            "SELECT id, job, user_name, outq, user_data FROM as4tainr.spooled_files
              WHERE ($1 = '*ALL' OR user_name = $1)
                AND ($2 = '*ALL' OR outq = $2)
                AND ($3 = '*ALL' OR user_data = $3)
                AND ($4::bigint IS NULL OR job = $4)
              ORDER BY id",
            &[&user, &outq, &udata, &job],
        )
        .await?;
    let entries: Vec<Vec<u8>> = rows
        .iter()
        .map(|r| {
            let (ij, is) = crate::sysapi::internal_ids(r.get(1), r.get(0));
            let mut en = field(&r.get::<_, String>(2), 10);
            en.extend(field(&r.get::<_, String>(3), 10));
            en.extend(field("QGPL", 10));
            en.extend(field("*STD", 10));
            en.extend(field(&r.get::<_, String>(4), 10));
            en.extend(ij);
            en.extend(is);
            en.extend(field("", 2));
            en.extend_from_slice(&1i32.to_be_bytes());
            en
        })
        .collect();
    let (spc, spclib) = qualified(&args.bytes(0));
    let mut header = field(&user, 10);
    header.extend(field(&outq, 10));
    header.extend(field(if outq == "*ALL" { "" } else { "QGPL" }, 10));
    header.extend(field(&spc, 10));
    header.extend(field(&spclib, 10));
    let qjob = if args.count() > 7 {
        a(&args.bytes(7))
    } else {
        String::new()
    };
    header.extend(field(&qjob, 26));
    header.extend(field("", 8 + 7 + 6 + 7 + 6));
    let mut input = args.bytes(0);
    input.resize(20, 0x40);
    input.extend(field(&format, 8));
    input.extend(field(&user, 10));
    input.extend(field(&outq, 10));
    input.extend(field(&outqlib, 10));
    input.extend(field(&text(args, 4), 10));
    input.extend(field(&udata, 10));
    write_list(
        client,
        caller,
        &args.bytes(0),
        "QUSLSPL",
        &format,
        &input,
        &header,
        &entries,
        88,
    )
    .await
}
