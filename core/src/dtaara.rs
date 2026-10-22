//! Data areas (*DTAARA): small named values that programs share, such as
//! the next order number or a company name.
//!
//! Written from IBM's public documentation of CRTDTAARA, CHGDTAARA,
//! RTVDTAARA, DSPDTAARA and DLTDTAARA, and of the RPG `IN`/`OUT`/`UNLOCK`
//! operations on `*DTAARA DEFINE`d fields. A data area is an object
//! (`*DTAARA`) in a library; its description and value live in the catalog
//! table `as4tainr.data_areas`.
//!
//! Types: `*CHAR` (1-2000 characters, kept padded with blanks to its
//! length), `*DEC` (up to 24 digits, up to 9 of them decimals, kept as exact
//! decimal text, never floating point) and `*LGL` ('0' or '1').
//!
//! Locks: RPG `IN *LOCK` takes a PostgreSQL advisory lock for the data area
//! on the program's database session, held until `OUT` (or the end of the
//! program, when the session ends). CHGDTAARA waits for that lock, so a
//! program incrementing a counter under `*LOCK` is never interleaved with
//! another change.

use anyhow::{anyhow, bail, Result};
use rust_decimal::prelude::*;
use tokio_postgres::GenericClient;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.data_areas (
    library  text NOT NULL,
    name     text NOT NULL,
    type     text NOT NULL,
    length   int  NOT NULL,
    decimals int  NOT NULL DEFAULT 0,
    value    text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.local_data_areas (
    job   bigint PRIMARY KEY,
    value bytea NOT NULL
);
"#;

/// The local data area (*LDA): 1024 characters each job has, blanks when
/// the job starts; a job submitted with SBMJOB starts with a copy of the
/// submitting job's (CL Programming, "Local data area").
pub const LDA_LENGTH: usize = 1024;

/// The local data area of `as4tainr call` and the tests (no job).
static NO_JOB_LDA: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// The bytes (EBCDIC) of a job's local data area.
pub async fn lda_read<C: GenericClient>(client: &C, job: Option<i64>) -> Result<Vec<u8>> {
    let blank = || vec![0x40u8; LDA_LENGTH];
    let Some(job) = job else {
        return Ok(NO_JOB_LDA
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(blank));
    };
    let row = client
        .query_opt(
            "SELECT value FROM as4tainr.local_data_areas WHERE job=$1",
            &[&job],
        )
        .await?;
    Ok(row.map(|r| r.get::<_, Vec<u8>>(0)).unwrap_or_else(blank))
}

/// Replace a job's local data area (padded or cut to 1024 bytes).
pub async fn lda_write<C: GenericClient>(client: &C, job: Option<i64>, value: &[u8]) -> Result<()> {
    let mut v = value.to_vec();
    v.resize(LDA_LENGTH, 0x40);
    let Some(job) = job else {
        if let Ok(mut g) = NO_JOB_LDA.lock() {
            *g = Some(v);
        }
        return Ok(());
    };
    client
        .execute(
            "INSERT INTO as4tainr.local_data_areas (job, value) VALUES ($1, $2)
             ON CONFLICT (job) DO UPDATE SET value = EXCLUDED.value",
            &[&job, &v],
        )
        .await?;
    Ok(())
}

/// The local data area as CL sees it (CHGDTAARA, RTVDTAARA, DSPDTAARA
/// with DTAARA(*LDA)): a *CHAR data area of 1024.
pub async fn lda_area<C: GenericClient>(client: &C, job: Option<i64>) -> Result<DataArea> {
    let b = lda_read(client, job).await?;
    Ok(DataArea {
        library: String::new(),
        name: "*LDA".into(),
        ty: Type::Char,
        length: LDA_LENGTH as u32,
        decimals: 0,
        value: crate::rpg::data::a(&b),
        text: String::new(),
    })
}

/// CHGDTAARA DTAARA(*LDA [(start length)]) VALUE(...).
pub async fn lda_change<C: GenericClient>(
    client: &C,
    job: Option<i64>,
    part: Option<(u32, u32)>,
    value: &str,
) -> Result<()> {
    let d = lda_area(client, job).await?;
    let new = d.changed(part, value)?;
    lda_write(client, job, &crate::rpg::data::e(&new)).await
}

/// A new job's local data area: blanks, or a copy of another job's
/// (SBMJOB).
pub async fn lda_start<C: GenericClient>(client: &C, job: i64, copy_of: Option<i64>) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainr.local_data_areas WHERE job=$1",
            &[&job],
        )
        .await?;
    if let Some(from) = copy_of {
        client
            .execute(
                "INSERT INTO as4tainr.local_data_areas (job, value)
                 SELECT $1, value FROM as4tainr.local_data_areas WHERE job=$2",
                &[&job, &from],
            )
            .await?;
    }
    Ok(())
}

/// Longest *CHAR data area (CRTDTAARA LEN).
pub const MAX_CHAR: u32 = 2000;
/// Most digits of a *DEC data area, and most decimal positions.
pub const MAX_DIGITS: u32 = 24;
pub const MAX_DECIMALS: u32 = 9;

/// Advisory lock class for data areas ("DT").
const LOCK_CLASS: i32 = 0x4454;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Char,
    Dec,
    Lgl,
}

impl Type {
    pub fn parse(s: &str) -> Result<Type> {
        match s.trim().to_ascii_uppercase().as_str() {
            "*CHAR" => Ok(Type::Char),
            "*DEC" => Ok(Type::Dec),
            "*LGL" => Ok(Type::Lgl),
            other => Err(crate::cl::value_error(&format!(
                "TYPE({other}) is not one of the data area types *CHAR, *DEC and *LGL."
            ))),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Type::Char => "*CHAR",
            Type::Dec => "*DEC",
            Type::Lgl => "*LGL",
        }
    }

    /// CRTDTAARA LEN(*DFT) per type: 32 for *CHAR, (15 5) for *DEC, 1 for *LGL.
    pub fn default_length(self) -> (u32, u32) {
        match self {
            Type::Char => (32, 0),
            Type::Dec => (15, 5),
            Type::Lgl => (1, 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataArea {
    pub library: String,
    pub name: String,
    pub ty: Type,
    pub length: u32,
    pub decimals: u32,
    /// *CHAR: exactly `length` characters. *DEC: decimal text with
    /// `decimals` places ("1004", "-12.50"). *LGL: "0" or "1".
    pub value: String,
    pub text: String,
}

/// Check LEN for a type.
pub fn check_length(ty: Type, length: u32, decimals: u32) -> Result<()> {
    let ok = match ty {
        Type::Char => (1..=MAX_CHAR).contains(&length) && decimals == 0,
        Type::Dec => {
            (1..=MAX_DIGITS).contains(&length) && decimals <= MAX_DECIMALS && decimals <= length
        }
        Type::Lgl => length == 1 && decimals == 0,
    };
    if !ok {
        let d = if decimals > 0 {
            format!(" {decimals}")
        } else {
            String::new()
        };
        bail!(
            "Length {length}{d} is not valid for a {} data area ({}).",
            ty.code(),
            match ty {
                Type::Char => format!("1 to {MAX_CHAR}"),
                Type::Dec =>
                    format!("1 to {MAX_DIGITS} digits, at most {MAX_DECIMALS} decimal positions"),
                Type::Lgl => "1".into(),
            }
        );
    }
    Ok(())
}

/// A *DEC value that fits (length, decimals), as text with exactly
/// `decimals` places.
pub fn dec_value(input: &str, length: u32, decimals: u32) -> Result<String> {
    let t = input.trim();
    let t = if t.is_empty() { "0" } else { t };
    let d = Decimal::from_str(t)
        .map_err(|_| anyhow!("Value '{input}' is not valid for a *DEC data area."))?;
    if d.scale() > decimals {
        // More decimal places than the data area has is an error, not a
        // rounding (CHGDTAARA rejects it).
        let r = d.round_dp(decimals);
        if r != d {
            bail!("Value '{input}' has more than {decimals} decimal positions.");
        }
    }
    let mut d = d.round_dp(decimals);
    d.rescale(decimals);
    let int_digits = {
        let a = d.abs().trunc();
        if a.is_zero() {
            0
        } else {
            a.to_string().len() as u32
        }
    };
    if int_digits > length - decimals {
        bail!("Value '{input}' is too large for a *DEC data area of length ({length} {decimals}).");
    }
    if d.is_zero() {
        d.set_sign_positive(true);
    }
    Ok(d.to_string())
}

/// The stored value for a new value of a data area of this type.
pub fn normalize(ty: Type, length: u32, decimals: u32, input: &str) -> Result<String> {
    match ty {
        Type::Char => {
            let n = input.chars().count();
            if n > length as usize {
                bail!("Value is {n} characters, longer than the data area ({length}).");
            }
            Ok(format!("{input:<w$}", w = length as usize))
        }
        Type::Dec => dec_value(input, length, decimals),
        Type::Lgl => match input.trim() {
            "0" | "" => Ok("0".into()),
            "1" => Ok("1".into()),
            other => bail!("Value '{other}' is not valid for a *LGL data area; use '0' or '1'."),
        },
    }
}

impl DataArea {
    /// The value, or for *CHAR a substring of it (1-based start, length).
    pub fn substring(&self, part: Option<(u32, u32)>) -> Result<String> {
        match part {
            None => Ok(self.value.clone()),
            Some((start, len)) => {
                if self.ty != Type::Char {
                    bail!("A substring can only be used with a *CHAR data area.");
                }
                check_substring(start, len, self.length)?;
                Ok(self
                    .value
                    .chars()
                    .skip(start as usize - 1)
                    .take(len as usize)
                    .collect())
            }
        }
    }

    /// The new stored value after changing all of it or a substring.
    pub fn changed(&self, part: Option<(u32, u32)>, input: &str) -> Result<String> {
        match part {
            None => normalize(self.ty, self.length, self.decimals, input),
            Some((start, len)) => {
                if self.ty != Type::Char {
                    bail!("A substring can only be changed in a *CHAR data area.");
                }
                check_substring(start, len, self.length)?;
                if input.chars().count() > len as usize {
                    bail!("Value is longer than the substring ({len}).");
                }
                let piece = format!("{input:<w$}", w = len as usize);
                let mut cs: Vec<char> = self.value.chars().collect();
                for (i, c) in piece.chars().enumerate() {
                    cs[start as usize - 1 + i] = c;
                }
                Ok(cs.into_iter().collect())
            }
        }
    }

    /// LEN as DSPDTAARA shows it: "7 0" for *DEC, "32" otherwise.
    pub fn length_text(&self) -> String {
        if self.ty == Type::Dec {
            format!("{} {}", self.length, self.decimals)
        } else {
            self.length.to_string()
        }
    }
}

fn check_substring(start: u32, len: u32, length: u32) -> Result<()> {
    if start < 1 || len < 1 || start + len - 1 > length {
        bail!("Substring ({start} {len}) is outside the data area (length {length}).");
    }
    Ok(())
}

/// Parse a DTAARA parameter's substring part: "*ALL" or "start len".
pub fn parse_substring(s: &str) -> Result<Option<(u32, u32)>> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        [] => Ok(None),
        [a] if a.eq_ignore_ascii_case("*ALL") => Ok(None),
        [a, b] => {
            let start = a
                .parse()
                .map_err(|_| anyhow!("Substring start '{a}' is not a number."))?;
            let len = b
                .parse()
                .map_err(|_| anyhow!("Substring length '{b}' is not a number."))?;
            Ok(Some((start, len)))
        }
        _ => bail!("Substring '{s}' is not valid; use *ALL or a start position and a length."),
    }
}

/// Split "LIB/NAME (start len)" into the qualified name and the substring.
pub fn split_dtaara_param(s: &str) -> Result<(String, Option<(u32, u32)>)> {
    let s = s.trim();
    match s.find('(') {
        Some(i) => {
            let rest = s[i + 1..].trim_end();
            let inner = rest
                .strip_suffix(')')
                .ok_or_else(|| anyhow!("missing ')' in DTAARA({s})"))?;
            Ok((s[..i].trim().to_string(), parse_substring(inner)?))
        }
        None => {
            // Positional form: "NAME *ALL" or "NAME 1 10".
            let mut it = s.splitn(2, char::is_whitespace);
            let name = it.next().unwrap_or("").to_string();
            Ok((name, parse_substring(it.next().unwrap_or(""))?))
        }
    }
}

/// The libraries to search and the object name for a qualified name
/// (`LIB/NAME`, `*LIBL/NAME`, `*CURLIB/NAME` or `NAME`).
pub fn search_libraries(
    qualified: &str,
    libl: &[String],
    curlib: &str,
) -> Result<(Vec<String>, String, String)> {
    let q = qualified.trim().to_ascii_uppercase();
    let (libs, name, label) = match q.split_once('/') {
        Some(("*LIBL", n)) => (libl.to_vec(), n.to_string(), "*LIBL".to_string()),
        Some(("*CURLIB", n)) => (vec![curlib.to_string()], n.to_string(), curlib.to_string()),
        Some((l, n)) => (
            vec![validate_name("library", l)?],
            n.to_string(),
            l.to_string(),
        ),
        None => (libl.to_vec(), q.clone(), "*LIBL".to_string()),
    };
    if name.starts_with('*') {
        bail!("Data area {name} (local, group or program initialization parameter data area) is not supported yet.");
    }
    let name = validate_name("data area", &name)?;
    Ok((libs, name, label))
}

fn from_row(r: &tokio_postgres::Row) -> Result<DataArea> {
    Ok(DataArea {
        library: r.get(0),
        name: r.get(1),
        ty: Type::parse(r.get::<_, &str>(2))?,
        length: r.get::<_, i32>(3) as u32,
        decimals: r.get::<_, i32>(4) as u32,
        value: r.get(5),
        text: r.get::<_, Option<String>>(6).unwrap_or_default(),
    })
}

const SELECT: &str = "SELECT d.library, d.name, d.type, d.length, d.decimals, d.value,
    (SELECT text FROM as4tainr.objects o WHERE o.library=d.library AND o.name=d.name AND o.objtype='*DTAARA')
    FROM as4tainr.data_areas d";

/// Find a data area: the first library of `libs` that has it. CPF1015 if
/// none does (`label` names the library in the message).
pub async fn find<C: GenericClient>(
    client: &C,
    libs: &[String],
    name: &str,
    label: &str,
) -> Result<DataArea> {
    for lib in libs {
        if let Some(r) = client
            .query_opt(
                &format!("{SELECT} WHERE d.library=$1 AND d.name=$2"),
                &[lib, &name],
            )
            .await?
        {
            return from_row(&r);
        }
    }
    bail!("CPF1015: Data area {name} in library {label} does not exist.")
}

/// Find a data area by a qualified name through a library list.
pub async fn resolve<C: GenericClient>(
    client: &C,
    qualified: &str,
    libl: &[String],
    curlib: &str,
) -> Result<DataArea> {
    let (libs, name, label) = search_libraries(qualified, libl, curlib)?;
    find(client, &libs, &name, &label).await
}

/// CRTDTAARA.
#[allow(clippy::too_many_arguments)]
pub async fn create<C: GenericClient>(
    client: &C,
    library: &str,
    name: &str,
    ty: Type,
    length: u32,
    decimals: u32,
    value: &str,
    text: &str,
) -> Result<DataArea> {
    let lib = validate_name("library", library)?;
    let name = validate_name("data area", name)?;
    check_length(ty, length, decimals)?;
    let value = normalize(ty, length, decimals, value)?;
    if client
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        // CPF1021 (observed on IBM i 7.5 for CRTDTAARA).
        bail!("CPF1021: Library {lib} for data area {name} does not exist.");
    }
    // One statement, so the object and its value appear together.
    let r = client
        .execute(
            "WITH o AS (
                INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                VALUES ($1, $2, '*DTAARA', '', $7) ON CONFLICT DO NOTHING RETURNING 1)
             INSERT INTO as4tainr.data_areas
             SELECT $1, $2, $3, $4, $5, $6 FROM o",
            &[
                &lib,
                &name,
                &ty.code(),
                &(length as i32),
                &(decimals as i32),
                &value,
                &text,
            ],
        )
        .await?;
    if r == 0 {
        bail!("CPF1023: Data area {name} already exists in library {lib}.");
    }
    Ok(DataArea {
        library: lib,
        name,
        ty,
        length,
        decimals,
        value,
        text: text.to_string(),
    })
}

fn lock_args(lib: &str, name: &str) -> String {
    format!("{lib}/{name}")
}

async fn lock<C: GenericClient>(client: &C, d: &DataArea) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_lock($1, hashtext($2))",
            &[&LOCK_CLASS, &lock_args(&d.library, &d.name)],
        )
        .await?;
    Ok(())
}

/// CHGDTAARA: change all of the value or a substring. Waits while a
/// program holds the data area under `*LOCK`.
pub async fn change<C: GenericClient>(
    client: &C,
    qualified: &str,
    libl: &[String],
    curlib: &str,
    part: Option<(u32, u32)>,
    value: &str,
) -> Result<DataArea> {
    let d = resolve(client, qualified, libl, curlib).await?;
    lock(client, &d).await?;
    let result = async {
        // Read again under the lock.
        let d = find(
            client,
            std::slice::from_ref(&d.library),
            &d.name,
            &d.library,
        )
        .await?;
        let new = d.changed(part, value)?;
        client
            .execute(
                "UPDATE as4tainr.data_areas SET value=$3 WHERE library=$1 AND name=$2",
                &[&d.library, &d.name, &new],
            )
            .await?;
        Ok(DataArea { value: new, ..d })
    }
    .await;
    unlock(client, &d).await?;
    result
}

/// DLTDTAARA.
pub async fn delete<C: GenericClient>(
    client: &C,
    qualified: &str,
    libl: &[String],
    curlib: &str,
) -> Result<DataArea> {
    let d = resolve(client, qualified, libl, curlib).await?;
    client
        .execute(
            "WITH d AS (DELETE FROM as4tainr.data_areas WHERE library=$1 AND name=$2 RETURNING 1)
             DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*DTAARA'",
            &[&d.library, &d.name],
        )
        .await?;
    Ok(d)
}

/// RPG `IN`: read a data area, with `*LOCK` taking the lock first (held on
/// this database session until [`write`] or [`unlock`], or the session
/// ends).
pub async fn read<C: GenericClient>(
    client: &C,
    qualified: &str,
    libl: &[String],
    curlib: &str,
    lock_it: bool,
) -> Result<DataArea> {
    let d = resolve(client, qualified, libl, curlib).await?;
    if !lock_it {
        return Ok(d);
    }
    lock(client, &d).await?;
    find(
        client,
        std::slice::from_ref(&d.library),
        &d.name,
        &d.library,
    )
    .await
}

/// RPG `OUT`: write the whole value; releases a `*LOCK` this session holds.
pub async fn write<C: GenericClient>(
    client: &C,
    d: &DataArea,
    value: &str,
    unlock_after: bool,
) -> Result<()> {
    let new = normalize(d.ty, d.length, d.decimals, value)?;
    client
        .execute(
            "UPDATE as4tainr.data_areas SET value=$3 WHERE library=$1 AND name=$2",
            &[&d.library, &d.name, &new],
        )
        .await?;
    if unlock_after {
        unlock(client, d).await?;
    }
    Ok(())
}

/// Release a lock taken by [`read`] with `lock`.
pub async fn unlock<C: GenericClient>(client: &C, d: &DataArea) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_unlock($1, hashtext($2))",
            &[&LOCK_CLASS, &lock_args(&d.library, &d.name)],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lengths and defaults from the CRTDTAARA documentation: *CHAR 1-2000
    // (default 32), *DEC up to 24 digits with up to 9 decimal positions
    // (default 15 5), *LGL 1.
    #[test]
    fn lengths() {
        assert_eq!(Type::Char.default_length(), (32, 0));
        assert_eq!(Type::Dec.default_length(), (15, 5));
        assert_eq!(Type::Lgl.default_length(), (1, 0));
        assert!(check_length(Type::Char, 2000, 0).is_ok());
        assert!(check_length(Type::Char, 2001, 0).is_err());
        assert!(check_length(Type::Dec, 24, 9).is_ok());
        assert!(check_length(Type::Dec, 24, 10).is_err());
        assert!(check_length(Type::Dec, 25, 0).is_err());
        assert!(check_length(Type::Lgl, 2, 0).is_err());
    }

    #[test]
    fn values() {
        assert_eq!(normalize(Type::Char, 5, 0, "AB").unwrap(), "AB   ");
        assert!(normalize(Type::Char, 2, 0, "ABC").is_err());
        assert_eq!(normalize(Type::Dec, 7, 0, "1004").unwrap(), "1004");
        assert_eq!(normalize(Type::Dec, 7, 0, "").unwrap(), "0");
        assert_eq!(normalize(Type::Dec, 5, 2, "12.5").unwrap(), "12.50");
        assert_eq!(normalize(Type::Dec, 5, 2, "-0").unwrap(), "0.00");
        assert_eq!(normalize(Type::Dec, 5, 2, "-1.25").unwrap(), "-1.25");
        // (5 2) holds at most 999.99.
        assert!(normalize(Type::Dec, 5, 2, "1000").is_err());
        assert!(normalize(Type::Dec, 5, 2, "1.234").is_err());
        assert_eq!(normalize(Type::Dec, 5, 2, "1.230").unwrap(), "1.23");
        assert!(normalize(Type::Dec, 7, 0, "abc").is_err());
        assert_eq!(normalize(Type::Lgl, 1, 0, "1").unwrap(), "1");
        assert!(normalize(Type::Lgl, 1, 0, "Y").is_err());
    }

    #[test]
    fn substrings() {
        let d = DataArea {
            library: "QGPL".into(),
            name: "X".into(),
            ty: Type::Char,
            length: 10,
            decimals: 0,
            value: "ABCDEFGHIJ".into(),
            text: String::new(),
        };
        assert_eq!(d.substring(Some((3, 4))).unwrap(), "CDEF");
        assert!(d.substring(Some((8, 4))).is_err());
        assert_eq!(d.changed(Some((3, 4)), "xy").unwrap(), "ABxy  GHIJ");
        assert!(d.changed(Some((3, 2)), "xyz").is_err());
        assert_eq!(d.changed(None, "Q").unwrap(), "Q         ");
        assert_eq!(parse_substring("*ALL").unwrap(), None);
        assert_eq!(parse_substring("3 4").unwrap(), Some((3, 4)));
        assert_eq!(
            split_dtaara_param("QGPL/X (3 4)").unwrap(),
            ("QGPL/X".into(), Some((3, 4)))
        );
        assert_eq!(split_dtaara_param("X *ALL").unwrap(), ("X".into(), None));
        let n = DataArea { ty: Type::Dec, ..d };
        assert!(n.substring(Some((1, 1))).is_err());
    }

    #[test]
    fn names() {
        let libl = vec!["QGPL".to_string(), "QTEMP".to_string()];
        let (libs, name, _) = search_libraries("ordnxt", &libl, "QGPL").unwrap();
        assert_eq!((libs, name.as_str()), (libl.clone(), "ORDNXT"));
        let (libs, _, _) = search_libraries("*CURLIB/ORDNXT", &libl, "MYLIB").unwrap();
        assert_eq!(libs, vec!["MYLIB".to_string()]);
        assert!(search_libraries("*LDA", &libl, "QGPL").is_err());
    }
}
