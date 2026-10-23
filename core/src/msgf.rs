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
ALTER TABLE as4tainr.message_descriptions ADD COLUMN IF NOT EXISTS attrs text NOT NULL DEFAULT '{}';
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

/// The other attributes of a message description (ADDMSGD TYPE, LEN,
/// VALUES, DFT, ALROPT, LOGPRB, CCSID) and its message level: the date it
/// was added or last changed and a sequence number within that date
/// (DSPMSGD LVL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attrs {
    /// Reply type: *NONE, *CHAR, *DEC, *ALPHA, *NAME.
    pub reply_type: String,
    /// Reply length (and decimal positions of a *DEC reply).
    pub reply_len: (u32, u32),
    pub values: Vec<String>,
    /// Default reply ("" for *NONE).
    pub dft: String,
    pub alropt: String,
    pub logprb: String,
    pub ccsid: String,
    /// YYYY-MM-DD.
    pub changed: String,
    pub level: u32,
}

impl Default for Attrs {
    fn default() -> Attrs {
        Attrs {
            reply_type: "*NONE".into(),
            reply_len: (0, 0),
            values: vec![],
            dft: String::new(),
            alropt: "*NO".into(),
            logprb: "*NO".into(),
            ccsid: "65535".into(),
            changed: String::new(),
            level: 1,
        }
    }
}

/// Today as YYYY-MM-DD (a message level's date).
pub fn today() -> String {
    let ((y, m, d), _, _) = crate::rpg::dates::now();
    format!("{y:04}-{m:02}-{d:02}")
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
    pub attrs: Attrs,
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
        // CPD0075, then CPF0006 (observed on IBM i 7.5 for ADDMSGD).
        return Err(crate::cl::diag_error(
            "CPD0075",
            &format!(
                "MSGID({id}) must be 7 characters: 3 letters or digits, then 4 hexadecimal digits."
            ),
        ));
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
        Some("QSH") => "QZSHMSGF",
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
            bail!("CPF0006: The command has errors and was not run. FMT type {ty} is not valid.");
        }
        let num = |s: &str| -> Result<u32> {
            s.parse().map_err(|_| {
                anyhow!(
                    "CPF0006: The command has errors and was not run. FMT length {s} is not valid."
                )
            })
        };
        let (len, extra) = match (ty.as_str(), w.get(1).copied(), w.get(2).copied()) {
            ("*DEC", Some(l), d) => (Some(num(l)?), d.map(num).transpose()?.unwrap_or(0)),
            ("*DEC", None, _) => {
                bail!("CPF0006: The command has errors and was not run. FMT(*DEC) needs a length.")
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
        bail!("CPF0006: The command has errors and was not run. A message has at most 99 data fields.");
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
        bail!("CPF2110: Library {lib} does not exist.");
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
    let attrs = serde_json::to_string(&d.attrs)?;
    let n = if replace {
        client
            .execute(
                "UPDATE as4tainr.message_descriptions SET msg=$4, seclvl=$5, sev=$6, fmt=$7, attrs=$8
                 WHERE library=$1 AND msgf=$2 AND msgid=$3",
                &[
                    &d.library, &d.msgf, &d.msgid, &d.msg, &d.seclvl, &d.sev, &fmt, &attrs,
                ],
            )
            .await?
    } else {
        client
            .execute(
                "INSERT INTO as4tainr.message_descriptions (library, msgf, msgid, msg, seclvl, sev, fmt, attrs)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING",
                &[&d.library, &d.msgf, &d.msgid, &d.msg, &d.seclvl, &d.sev, &fmt, &attrs],
            )
            .await?
    };
    if n == 0 {
        if replace {
            bail!(
                "CPF2419: Message {} is not in message file {} in library {}.",
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
        bail!("CPF2419: Message {msgid} is not in message file {msgf} in library {library}.");
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
        attrs: serde_json::from_str(r.get::<_, &str>(7)).unwrap_or_default(),
    })
}

const SELECT: &str =
    "SELECT library, msgf, msgid, msg, seclvl, sev, fmt, attrs FROM as4tainr.message_descriptions";

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

/// The descriptions of a message file, in the order of their
/// identifiers in EBCDIC (letters before digits: GLD00A1 before GLD0001,
/// as DSPMSGD lists them on IBM i 7.5).
pub async fn list(client: &Client, library: &str, msgf: &str) -> Result<Vec<Description>> {
    let mut ds = client
        .query(
            &format!("{SELECT} WHERE library=$1 AND msgf=$2"),
            &[&library, &msgf],
        )
        .await?
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>>>()?;
    ds.sort_by_cached_key(|d| crate::rpg::data::e(&d.msgid));
    Ok(ds)
}

// ------------------------------------------------------------ DSPMSGD

/// What a DSPMSGD report shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReportKind {
    /// DETAIL(*FULL) FMTTXT(*YES): second-level text laid out by its
    /// format controls (&N, &P, &B).
    Formatted,
    /// DETAIL(*FULL) FMTTXT(*NO): the texts as stored, in pieces.
    Unformatted,
    /// DETAIL(*BASIC): identifier, severity and first-level text.
    Basic,
}

/// The identifiers line of a DSPMSGD report.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportIds {
    /// "Message IDs:" and one identifier.
    One(String),
    /// "Start message ID" and "Ending message ID".
    Range(String, String),
}

/// Lines on a page of a DSPMSGD report, the page header included
/// (observed on IBM i 7.5: 37).
const MSGD_PAGE: usize = 37;
/// The longest line of a DSPMSGD report's message texts.
const MSGD_WIDTH: usize = 81;
/// Unformatted texts are printed in pieces of this many characters.
const MSGD_PIECE: usize = 76;

/// End-of-listing line of DSPMSGD.
pub const END_MSGD: &str = "                        * * * * *  E N D  O F  L I S T I N G * * * * *";

/// A DSPMSGD report in IBM i 7.5's layout (PUB400, 2026-10-22, captured
/// with tools/golden/listings.txt): a page header, the message file and
/// the identifiers, then each description.
pub fn report(
    kind: ReportKind,
    lib: &str,
    msgf: &str,
    ids: &ReportIds,
    ds: &[Description],
    stamp: &crate::report::Stamp,
    system: &str,
) -> Vec<String> {
    use crate::commands::dsp_label;
    use crate::report::at;
    let (title, title_col) = match kind {
        ReportKind::Formatted => ("Message Description (Formatted Text)", 38),
        ReportKind::Unformatted => ("Message Description (Unformatted Text)", 38),
        ReportKind::Basic => ("Message File", 45),
    };
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (17, crate::report::LEVEL),
            (title_col, title),
            (88, system),
            (97, &stamp.date),
            (106, &stamp.time),
            (119, &format!("Page {page:>7}")),
        ])
    };
    let mut body = vec![at(&[
        (2, &dsp_label("Message file", 30)),
        (38, msgf),
        (52, &dsp_label("Library", 30)),
        (88, lib),
    ])];
    body.push(match ids {
        ReportIds::One(id) => at(&[(2, "Message IDs:"), (24, id)]),
        ReportIds::Range(a, b) => at(&[
            (2, &dsp_label("Start message ID", 30)),
            (38, a),
            (52, &dsp_label("Ending message ID", 30)),
            (88, b),
        ]),
    });
    if kind == ReportKind::Basic {
        body.push(" MSGID   SEV   MSG".into());
        for d in ds {
            body.push(at(&[
                (2, &d.msgid),
                (10, &format!("{:>3}", d.sev)),
                (16, &d.msg),
            ]));
        }
    } else {
        for d in ds {
            body.extend(entry(kind, d, stamp));
        }
    }
    body.push(END_MSGD.into());
    let mut out = Vec::new();
    for (i, chunk) in body.chunks(MSGD_PAGE - 1).enumerate() {
        out.push(header(i + 1));
        out.extend(chunk.iter().cloned());
    }
    out
}

/// A label of a description's attribute: text from column `indent`,
/// dots up to column 45, the colon in 47, the keyword in 52.
fn attr(indent: usize, text: &str, keyword: &str) -> String {
    let label = crate::commands::dsp_label(text, 45 - indent);
    crate::report::at(&[(indent, &label), (52, keyword)])
}

/// The lines of one description in a full report.
fn entry(kind: ReportKind, d: &Description, stamp: &crate::report::Stamp) -> Vec<String> {
    use crate::report::at;
    let mut out = vec![format!(" {}", d.msgid)];
    if kind == ReportKind::Formatted {
        out.extend(wrap(&format!("   Message . . . . :   {}", d.msg), 0, 5));
        out.extend(format_seclvl(&d.seclvl));
    } else {
        out.push(attr(4, "First level message text", "MSG"));
        out.extend(pieces(&d.msg));
        if !d.seclvl.is_empty() {
            out.push(attr(4, "Second level message text", "SECLVL"));
            out.extend(pieces(&d.seclvl));
        }
    }
    out.push(at(&[
        (1, &attr(4, "Severity", "SEV")),
        (63, &format!("{:>2}", d.sev)),
    ]));
    if !d.fmt.is_empty() {
        out.push(attr(4, "Message data field formats", "FMT"));
        out.push(
            "     Field        Data Type          Length            Vary Bytes Or Dec Pos".into(),
        );
        for (i, f) in d.fmt.iter().enumerate() {
            let var = format!("&{}", i + 1);
            let len = match f.len {
                Some(l) => format!("{l:>5}"),
                None => "*VARY".into(),
            };
            let mut fields: Vec<(usize, String)> = vec![(7, var), (21, f.ty.clone()), (39, len)];
            if f.len.is_none() || (f.ty == "*DEC" && f.extra > 0) {
                fields.push((64, format!("{:>2}", f.extra)));
            }
            let refs: Vec<(usize, &str)> = fields.iter().map(|(c, t)| (*c, t.as_str())).collect();
            out.push(at(&refs));
        }
    }
    let a = &d.attrs;
    if a.reply_type != "*NONE" {
        out.push("   Reply specifications:".into());
        out.push(at(&[
            (1, &attr(6, "Reply type", "TYPE")),
            (63, &a.reply_type),
        ]));
        let len = if a.reply_type == "*DEC" {
            format!("{:>5} {}", a.reply_len.0, a.reply_len.1)
        } else {
            format!("{:>5}", a.reply_len.0)
        };
        out.push(at(&[(1, &attr(6, "Reply length", "LEN")), (63, &len)]));
        if a.values.is_empty() {
            out.push(at(&[
                (1, &attr(6, "Valid reply values", "VALUES")),
                (63, "*NONE"),
            ]));
        } else {
            out.push(attr(6, "Valid reply values", "VALUES"));
            for pair in a.values.chunks(2) {
                let mut f: Vec<(usize, &str)> = vec![(9, &pair[0])];
                if let Some(v) = pair.get(1) {
                    f.push((43, v));
                }
                out.push(at(&f));
            }
        }
        let dft = if a.dft.is_empty() { "*NONE" } else { &a.dft };
        out.push(at(&[(1, &attr(6, "Default reply", "DFT")), (63, dft)]));
    }
    out.push(at(&[
        (1, &attr(4, "Data to be dumped", "DMPLST")),
        (63, "*JOB"),
    ]));
    let level = match a.changed.split('-').collect::<Vec<_>>()[..] {
        [y, m, dd] => {
            let st = crate::report::Stamp::of(
                (
                    y.parse().unwrap_or(1970),
                    m.parse().unwrap_or(1),
                    dd.parse().unwrap_or(1),
                ),
                (0, 0, 0),
            );
            format!("{} {:02}", st.date, a.level)
        }
        _ => format!("{} {:02}", stamp.date, a.level),
    };
    out.push(at(&[(1, &attr(4, "Message level", "LVL")), (63, &level)]));
    out.push(at(&[
        (1, &attr(4, "Alert option", "ALROPT")),
        (63, &a.alropt),
    ]));
    out.push(at(&[
        (1, &attr(4, "Log problem", "LOGPRB")),
        (63, &a.logprb),
    ]));
    out.push(at(&[
        (1, &attr(4, "Coded character set ID", "CCSID")),
        (63, &a.ccsid),
    ]));
    out
}

/// A text in pieces of MSGD_PIECE characters from column 6 (FMTTXT(*NO)).
fn pieces(text: &str) -> Vec<String> {
    let cs: Vec<char> = text.chars().collect();
    cs.chunks(MSGD_PIECE)
        .map(|c| format!("     {}", c.iter().collect::<String>()))
        .collect()
}

/// `line` (its first `first` characters already the indentation) broken
/// at blanks into lines of at most MSGD_WIDTH characters; the lines after
/// the first start with `cont` blanks.
fn wrap(line: &str, first: usize, cont: usize) -> Vec<String> {
    let _ = first;
    let mut out = Vec::new();
    let mut cur: String = line.trim_end().to_string();
    loop {
        let n = cur.chars().count();
        if n <= MSGD_WIDTH {
            out.push(cur);
            break;
        }
        let cs: Vec<char> = cur.chars().collect();
        // The last blank at which the line fits, after the indentation.
        let lead = cs.iter().take_while(|c| **c == ' ').count();
        let cut = (lead + 1..=MSGD_WIDTH)
            .rev()
            .find(|&i| cs[i] == ' ')
            .unwrap_or(MSGD_WIDTH);
        let head: String = cs[..cut].iter().collect();
        out.push(head.trim_end().to_string());
        let rest: String = cs[cut..].iter().collect();
        cur = format!("{}{}", " ".repeat(cont), rest.trim_start());
    }
    out
}

/// Second-level text laid out by its format controls (ADDMSGD SECLVL):
/// &N starts a line in column 2 of the text (later lines in column 4),
/// &P a line in column 6 (later lines in 4), &B a line in column 4 (later
/// lines in 6). The text's column 1 is column 2 of the report.
pub fn format_seclvl(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // (first, continuation) indentation of the current paragraph.
    let mut ind = (3usize, 5usize);
    let mut cur = String::new();
    let flush = |cur: &mut String, ind: (usize, usize), out: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            out.extend(wrap(&format!("{}{t}", " ".repeat(ind.0)), ind.0, ind.1));
        }
        cur.clear();
    };
    let mut rest = text;
    while !rest.is_empty() {
        let next = ["&N", "&P", "&B"]
            .iter()
            .filter_map(|c| {
                rest.find(c).filter(|&i| {
                    // A control is followed by a blank (or ends the text).
                    rest[i + 2..].is_empty() || rest[i + 2..].starts_with(' ')
                })
            })
            .min();
        match next {
            Some(i) => {
                cur.push_str(&rest[..i]);
                flush(&mut cur, ind, &mut out);
                ind = match &rest[i..i + 2] {
                    "&N" => (3, 5),
                    "&P" => (7, 5),
                    _ => (5, 7),
                };
                rest = &rest[i + 2..];
            }
            None => {
                cur.push_str(rest);
                rest = "";
            }
        }
    }
    flush(&mut cur, ind, &mut out);
    out
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
