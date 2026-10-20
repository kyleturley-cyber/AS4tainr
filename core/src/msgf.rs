//! Message files (*MSGF) and message descriptions.
//!
//! Written from IBM's public CL documentation (CRTMSGF, ADDMSGD, CHGMSGD,
//! RMVMSGD, DLTMSGF, DSPMSGD, RTVMSG, SNDPGMMSG). A message description
//! has a message identifier, first- and second-level text with
//! substitution variables &1-&99, a severity and the formats of its
//! message data fields (FMT). Sending a message with message data fills
//! the variables from the data, field by field, as ADDMSGD describes:
//! `*CHAR` without its trailing blanks, `*QTDCHAR` in apostrophes, `*DEC`
//! with its decimal point and without leading zeros, `*BIN`/`*UBIN` as
//! numbers, `*HEX` as X'..'; `*VARY` lengths come first in the data
//! (2 or 4 bytes); data shorter than a field leaves the field empty.
//!
//! The system's own message files (QCPFMSG and the like) have no
//! descriptions here: as4tainr's messages carry their own text (written
//! fresh, never IBM's).

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.message_descriptions (
    library text NOT NULL,
    msgf    text NOT NULL,
    msgid   text NOT NULL,
    msg     text NOT NULL,
    seclvl  text NOT NULL DEFAULT '',
    sev     int  NOT NULL DEFAULT 0,
    fmt     text NOT NULL DEFAULT '[]',
    PRIMARY KEY (library, msgf, msgid)
);
"#;

/// The format of one message data field (ADDMSGD FMT).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// *CHAR, *QTDCHAR, *HEX, *DEC, *BIN, *UBIN...
    pub ty: String,
    /// Length (bytes; digits for *DEC); None: *VARY.
    pub len: Option<u32>,
    /// *DEC decimal positions; *VARY: the length field's size (2 or 4).
    pub extra: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Description {
    pub library: String,
    pub msgf: String,
    pub msgid: String,
    pub msg: String,
    pub seclvl: String,
    pub sev: i32,
    pub fmt: Vec<Field>,
}

/// A message identifier: three letters or digits, four hexadecimal
/// digits (ADDMSGD MSGID: pppmmnn).
pub fn check_msgid(id: &str) -> Result<String> {
    let id = id.trim().to_ascii_uppercase();
    let ok = id.len() == 7
        && id[..3].chars().all(|c| c.is_ascii_alphanumeric())
        && id.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && id[3..].chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        bail!("CPF0006: Errors occurred in command. '{id}' is not a valid message identifier.");
    }
    Ok(id)
}

/// The message file of a system message identifier (for RCVMSG MSGF):
/// CPF, CPA, CPD, CPI, MCH messages QCPFMSG; RNQ and RNX QRNXMSG; SQL
/// QSQLMSG.
pub fn system_msgf(id: &str) -> &'static str {
    match id.get(..3) {
        Some("RNQ") | Some("RNX") => "QRNXMSG",
        Some("SQL") => "QSQLMSG",
        _ => "QCPFMSG",
    }
}

/// Parse ADDMSGD FMT values: `(*CHAR 10) (*DEC 7 2) (*CHAR *VARY 2)`.
pub fn parse_fmt(items: &[String]) -> Result<Vec<Field>> {
    let mut out = Vec::new();
    for it in items {
        let inner = it.trim().trim_start_matches('(').trim_end_matches(')');
        let w: Vec<&str> = inner.split_whitespace().collect();
        if w.is_empty() || w[0].eq_ignore_ascii_case("*NONE") {
            continue;
        }
        let ty = w[0].to_ascii_uppercase();
        if !matches!(
            ty.as_str(),
            "*QTDCHAR"
                | "*CHAR"
                | "*CCHAR"
                | "*HEX"
                | "*DEC"
                | "*BIN"
                | "*UBIN"
                | "*DTS"
                | "*ITV"
                | "*UTC"
                | "*UTCD"
                | "*UTCT"
                | "*SPP"
                | "*SYP"
        ) {
            bail!("CPF0006: Errors occurred in command. FMT type {ty} is not valid.");
        }
        let num = |s: &str| -> Result<u32> {
            s.parse().map_err(|_| {
                anyhow!("CPF0006: Errors occurred in command. FMT length {s} is not valid.")
            })
        };
        let (len, extra) = match (ty.as_str(), w.get(1).copied(), w.get(2).copied()) {
            ("*DEC", Some(l), d) => (Some(num(l)?), d.map(num).transpose()?.unwrap_or(0)),
            ("*DEC", None, _) => {
                bail!("CPF0006: Errors occurred in command. FMT(*DEC) needs a length.")
            }
            ("*BIN" | "*UBIN", l, _) => (Some(l.map(num).transpose()?.unwrap_or(2)), 0),
            ("*DTS" | "*ITV" | "*UTC" | "*UTCD" | "*UTCT", _, _) => (Some(8), 0),
            ("*SPP" | "*SYP", _, _) => (Some(16), 0),
            (_, None, _) => (None, 2),
            (_, Some(v), b) if v.eq_ignore_ascii_case("*VARY") => {
                (None, b.map(num).transpose()?.unwrap_or(2))
            }
            (_, Some(l), _) => (Some(num(l)?), 0),
        };
        out.push(Field { ty, len, extra });
    }
    if out.len() > 99 {
        bail!("CPF0006: Errors occurred in command. A message has at most 99 data fields.");
    }
    Ok(out)
}

/// The values the message data gives the substitution variables.
pub fn values(fmt: &[Field], data: &[u8]) -> Vec<String> {
    use crate::rpg::data::{self, DType};
    let mut at = 0usize;
    let mut out = Vec::new();
    for f in fmt {
        // A *VARY field: its length first.
        let (len, lenfield) = match f.len {
            Some(l) => {
                let bytes = match f.ty.as_str() {
                    "*DEC" => l as usize / 2 + 1,
                    _ => l as usize,
                };
                (bytes, 0)
            }
            None => {
                let n = f.extra as usize;
                let Some(b) = data.get(at..at + n) else {
                    out.push(String::new());
                    continue;
                };
                let l = if n == 4 {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
                } else {
                    u16::from_be_bytes([b[0], b[1]]) as usize
                };
                (l, n)
            }
        };
        let start = at + lenfield;
        let Some(b) = data.get(start..start + len) else {
            // Shorter than the field: an empty value.
            out.push(String::new());
            at = data.len();
            continue;
        };
        at = start + len;
        out.push(match f.ty.as_str() {
            "*CHAR" | "*CCHAR" => data::a(b).trim_end().to_string(),
            "*QTDCHAR" => format!("'{}'", data::a(b).trim_end().replace('\'', "''")),
            "*HEX" => format!(
                "X'{}'",
                b.iter().map(|x| format!("{x:02X}")).collect::<String>()
            ),
            "*DEC" => match data::load(&DType::Packed(f.len.unwrap_or(0), f.extra), b)
                .and_then(|v| v.num())
            {
                Ok(n) => {
                    // Leading zeros removed; the decimal point when there
                    // are decimal positions.
                    let t = n.to_string();
                    let neg = t.starts_with('-');
                    let digits = t.trim_start_matches('-');
                    let digits = if digits.starts_with("0.") {
                        &digits[1..]
                    } else {
                        digits
                    };
                    format!("{}{digits}", if neg { "-" } else { "" })
                }
                Err(_) => String::new(),
            },
            "*BIN" => match b.len() {
                2 => i16::from_be_bytes([b[0], b[1]]).to_string(),
                4 => i32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
                _ => i64::from_be_bytes(b.try_into().unwrap_or([0; 8])).to_string(),
            },
            "*UBIN" => match b.len() {
                2 => u16::from_be_bytes([b[0], b[1]]).to_string(),
                4 => u32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
                _ => u64::from_be_bytes(b.try_into().unwrap_or([0; 8])).to_string(),
            },
            "*ITV" => i64::from_be_bytes(b.try_into().unwrap_or([0; 8])).to_string(),
            _ => String::new(),
        });
    }
    out
}

/// Text with &1-&99 replaced by the values (a variable with no value:
/// empty).
pub fn substitute(text: &str, vals: &[String]) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '&' && cs.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            let mut j = i + 1;
            while j < cs.len() && j < i + 3 && cs[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = cs[i + 1..j].iter().collect::<String>().parse().unwrap_or(0);
            if n >= 1 {
                if let Some(v) = vals.get(n - 1) {
                    out.push_str(v);
                }
                i = j;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

impl Description {
    /// First- and second-level text with the message data substituted.
    pub fn format(&self, data: &[u8]) -> (String, String) {
        let v = values(&self.fmt, data);
        (substitute(&self.msg, &v), substitute(&self.seclvl, &v))
    }
}

// ------------------------------------------------------------ catalog

async fn library_exists(client: &Client, lib: &str) -> Result<()> {
    if client
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} not found.");
    }
    Ok(())
}

/// CRTMSGF.
pub async fn create(client: &Client, library: &str, name: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("message file", name)?;
    library_exists(client, &lib).await?;
    let n = client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             VALUES ($1,$2,'*MSGF','',$3) ON CONFLICT DO NOTHING",
            &[&lib, &name, &text],
        )
        .await?;
    if n == 0 {
        bail!("CPF2112: Object {name} in library {lib} type *MSGF already exists.");
    }
    Ok(())
}

/// The library a message file is in (through the library list).
pub async fn locate(client: &Client, libs: &[String], name: &str) -> Result<Option<String>> {
    for lib in libs {
        if client
            .query_opt(
                "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*MSGF'",
                &[lib, &name],
            )
            .await?
            .is_some()
        {
            return Ok(Some(lib.clone()));
        }
    }
    Ok(None)
}

/// DLTMSGF.
pub async fn delete(client: &Client, library: &str, name: &str) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainr.message_descriptions WHERE library=$1 AND msgf=$2",
            &[&library, &name],
        )
        .await?;
    client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*MSGF'",
            &[&library, &name],
        )
        .await?;
    Ok(())
}

/// ADDMSGD (replace: CHGMSGD).
pub async fn add(client: &Client, d: &Description, replace: bool) -> Result<()> {
    let fmt = serde_json::to_string(&d.fmt)?;
    let n = if replace {
        client
            .execute(
                "UPDATE as4tainr.message_descriptions SET msg=$4, seclvl=$5, sev=$6, fmt=$7
                 WHERE library=$1 AND msgf=$2 AND msgid=$3",
                &[
                    &d.library, &d.msgf, &d.msgid, &d.msg, &d.seclvl, &d.sev, &fmt,
                ],
            )
            .await?
    } else {
        client
            .execute(
                "INSERT INTO as4tainr.message_descriptions (library, msgf, msgid, msg, seclvl, sev, fmt)
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
                &[&d.library, &d.msgf, &d.msgid, &d.msg, &d.seclvl, &d.sev, &fmt],
            )
            .await?
    };
    if n == 0 {
        if replace {
            bail!(
                "CPF2419: Message identifier {} not found in message file {} in {}.",
                d.msgid,
                d.msgf,
                d.library
            );
        }
        bail!(
            "CPF2412: Message identifier {} already exists in message file {} in {}.",
            d.msgid,
            d.msgf,
            d.library
        );
    }
    Ok(())
}

/// RMVMSGD.
pub async fn remove(client: &Client, library: &str, msgf: &str, msgid: &str) -> Result<()> {
    let n = client
        .execute(
            "DELETE FROM as4tainr.message_descriptions WHERE library=$1 AND msgf=$2 AND msgid=$3",
            &[&library, &msgf, &msgid],
        )
        .await?;
    if n == 0 {
        bail!("CPF2419: Message identifier {msgid} not found in message file {msgf} in {library}.");
    }
    Ok(())
}

fn from_row(r: &tokio_postgres::Row) -> Result<Description> {
    Ok(Description {
        library: r.get(0),
        msgf: r.get(1),
        msgid: r.get(2),
        msg: r.get(3),
        seclvl: r.get(4),
        sev: r.get(5),
        fmt: serde_json::from_str(r.get::<_, &str>(6))?,
    })
}

const SELECT: &str =
    "SELECT library, msgf, msgid, msg, seclvl, sev, fmt FROM as4tainr.message_descriptions";

/// A message description in a message file found in `libs`.
pub async fn find(
    client: &Client,
    libs: &[String],
    msgf: &str,
    msgid: &str,
) -> Result<Option<Description>> {
    let Some(lib) = locate(client, libs, msgf).await? else {
        return Ok(None);
    };
    client
        .query_opt(
            &format!("{SELECT} WHERE library=$1 AND msgf=$2 AND msgid=$3"),
            &[&lib, &msgf, &msgid],
        )
        .await?
        .map(|r| from_row(&r))
        .transpose()
}

/// The descriptions of a message file, by identifier (DSPMSGD).
pub async fn list(client: &Client, library: &str, msgf: &str) -> Result<Vec<Description>> {
    client
        .query(
            &format!("{SELECT} WHERE library=$1 AND msgf=$2 ORDER BY msgid"),
            &[&library, &msgf],
        )
        .await?
        .iter()
        .map(from_row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADDMSGD FMT (CL reference): *CHAR without trailing blanks,
    // *QTDCHAR in apostrophes, *DEC with its decimal point and no leading
    // zeros, *BIN as a number, *VARY with its length first; short data
    // leaves the variable empty.
    #[test]
    fn substitution() {
        let fmt = parse_fmt(&[
            "(*CHAR 10)".into(),
            "(*DEC 7 2)".into(),
            "(*BIN 2)".into(),
            "(*QTDCHAR *VARY 2)".into(),
        ])
        .unwrap();
        let mut data = crate::rpg::data::e("CUSTMAST  ");
        data.extend([0x00, 0x12, 0x80, 0x0F]); // packed 0012800 → 128.00
        data.extend([0xFF, 0xFE]); // -2
        data.extend([0x00, 0x02]);
        data.extend(crate::rpg::data::e("ok"));
        let v = values(&fmt, &data);
        assert_eq!(v, ["CUSTMAST", "128.00", "-2", "'ok'"]);
        assert_eq!(
            substitute("File &1 has &2 (&3) &4 &5.", &v),
            "File CUSTMAST has 128.00 (-2) 'ok' ."
        );
        assert_eq!(values(&fmt, &crate::rpg::data::e("AB"))[0], "");
        assert!(check_msgid("USR0001").is_ok() && check_msgid("US0001").is_err());
    }
}
