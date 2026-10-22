//! Command definition objects (*CMD).
//!
//! Written from IBM's public CL documentation ("Defining commands",
//! "Command definition statements": CMD, PARM, ELEM, QUAL, DEP, PMTCTL;
//! "Data passed to the command processing program"; CRTCMD, DLTCMD,
//! DSPCMD). CRTCMD turns command definition source (type CMD) into a
//! command: its parameters, their types, defaults, special and single
//! values, restricted values, ranges, lists, mixed lists (ELEM) and
//! qualified names (QUAL), and the command processing program (CPP) it
//! calls.
//!
//! Running the command checks what was typed against the definition and
//! calls the CPP with one parameter per PARM, in PARM order, in the form
//! the CL documentation gives:
//!
//! - `*CHAR`, `*NAME`, `*SNAME`, `*CNAME`, `*GENERIC`, `*PNAME`, `*CMDSTR`:
//!   the characters, padded with blanks to LEN (VARY(*YES): a 2-byte
//!   binary length first).
//! - `*DEC`: packed decimal of LEN digits; `*LGL`: '1' or '0'; `*INT2`,
//!   `*INT4`, `*UINT2`, `*UINT4`: binary; `*HEX`: the bytes; `*DATE`:
//!   CYYMMDD; `*TIME`: HHMMSS.
//! - A list (MAX greater than 1): a 2-byte binary count, then the values.
//! - A qualified name (QUAL): its parts one after another, the object
//!   first.
//! - A mixed list (ELEM): a 2-byte count of elements, then the elements; a
//!   list of mixed lists: the count, a 2-byte offset of each mixed list
//!   from the start of the parameter, then the mixed lists.
//!
//! The definition is kept as JSON in `as4tainr.commands`; the object is a
//! `*CMD` row in the catalog.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio_postgres::Client;

use crate::catalog::validate_name;
use crate::clpgm::{logical_lines, named, parse_statement};

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.commands (
    library    text NOT NULL,
    name       text NOT NULL,
    definition text NOT NULL,
    PRIMARY KEY (library, name)
);
"#;

/// A command as CRTCMD made it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Definition {
    pub library: String,
    pub name: String,
    /// The command processing program: LIB/NAME (*LIBL/NAME).
    pub pgm: String,
    /// The validity checking program (VLDCKR), if any.
    #[serde(default)]
    pub vldckr: Option<String>,
    pub prompt: String,
    pub text: String,
    /// MAXPOS: how many parameters may be given without their keywords.
    #[serde(default)]
    pub maxpos: Option<usize>,
    pub parms: Vec<Parm>,
    /// Mixed lists (ELEM) and qualified names (QUAL) by statement label.
    pub lists: BTreeMap<String, List>,
}

/// The ELEM or QUAL statements under one label.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct List {
    pub qual: bool,
    pub items: Vec<Parm>,
}

/// A PARM, ELEM or QUAL statement.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Parm {
    /// KWD (PARM only).
    pub kwd: String,
    /// *CHAR, *DEC, *LGL, *NAME... or the label of ELEM or QUAL statements.
    pub ty: String,
    pub len: u32,
    pub dec: u32,
    pub constant: Option<String>,
    pub rstd: bool,
    pub dft: Option<String>,
    pub values: Vec<String>,
    /// RANGE(low high) and REL(operator value).
    pub range: Option<(String, String)>,
    pub rel: Option<(String, String)>,
    /// SPCVAL((from to) ...) and SNGVAL((from to) ...).
    pub spcval: Vec<(String, String)>,
    pub sngval: Vec<(String, String)>,
    pub min: u32,
    pub max: u32,
    pub vary: bool,
    /// CASE(*MIXED): quoted values keep their case.
    pub mixed: bool,
    /// FULL(*YES): the value must be exactly LEN long.
    pub full: bool,
    pub prompt: String,
    /// PMTCTL(*PMTRQS): shown when F9 asks for all parameters.
    pub pmtrqs: bool,
}

impl Parm {
    fn is_list(&self) -> bool {
        self.max > 1
    }
}

// ------------------------------------------------------------ values

/// A value as typed: a word or quoted string, or a parenthesized list.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Atom { text: String, quoted: bool },
    List(Vec<Item>),
}

impl Item {
    fn word(t: &str) -> Item {
        Item::Atom {
            text: t.to_string(),
            quoted: false,
        }
    }

    /// The items of a list (an atom is a list of one).
    fn elements(&self) -> Vec<Item> {
        match self {
            Item::List(v) => v.clone(),
            a => vec![a.clone()],
        }
    }

    fn text(&self) -> String {
        match self {
            Item::Atom { text, .. } => text.clone(),
            Item::List(v) => format!(
                "({})",
                v.iter().map(|i| i.text()).collect::<Vec<_>>().join(" ")
            ),
        }
    }
}

/// Parse values: words (folded to upper case), quoted strings ('' for an
/// apostrophe), X'..' hex literals as quoted text, and lists in
/// parentheses. Returns the items and, for each, whether a '(' followed it
/// at once (a keyword): (keyword, its values).
fn parse_values(s: &str) -> Result<Vec<(Option<String>, Vec<Item>)>> {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    fn list(cs: &[char], i: &mut usize) -> Result<Vec<Item>> {
        // At '(': items to the matching ')'.
        *i += 1;
        let mut items = Vec::new();
        loop {
            while *i < cs.len() && cs[*i].is_whitespace() {
                *i += 1;
            }
            match cs.get(*i) {
                None => {
                    bail!("CPF0006: The command has errors and was not run. A closing parenthesis is missing.")
                }
                Some(')') => {
                    *i += 1;
                    return Ok(items);
                }
                Some('(') => items.push(Item::List(list(cs, i)?)),
                _ => items.push(atom(cs, i)?),
            }
        }
    }
    fn atom(cs: &[char], i: &mut usize) -> Result<Item> {
        if cs[*i] == '\'' {
            let mut v = String::new();
            *i += 1;
            loop {
                match cs.get(*i) {
                    None => bail!(
                        "CPF0006: The command has errors and was not run. A closing apostrophe is missing."
                    ),
                    Some('\'') if cs.get(*i + 1) == Some(&'\'') => {
                        v.push('\'');
                        *i += 2;
                    }
                    Some('\'') => {
                        *i += 1;
                        break;
                    }
                    Some(&c) => {
                        v.push(c);
                        *i += 1;
                    }
                }
            }
            return Ok(Item::Atom {
                text: v,
                quoted: true,
            });
        }
        let s = *i;
        while *i < cs.len() && !cs[*i].is_whitespace() && cs[*i] != '(' && cs[*i] != ')' {
            // X'C1C2': a hexadecimal literal.
            if cs[*i] == '\'' {
                let mut j = *i + 1;
                while j < cs.len() && cs[j] != '\'' {
                    j += 1;
                }
                *i = (j + 1).min(cs.len());
                continue;
            }
            *i += 1;
        }
        let w: String = cs[s..*i].iter().collect();
        Ok(Item::word(&w.to_ascii_uppercase()))
    }
    while i < cs.len() {
        if cs[i].is_whitespace() {
            i += 1;
            continue;
        }
        if cs[i] == '(' {
            out.push((None, vec![Item::List(list(&cs, &mut i)?)]));
            continue;
        }
        if cs[i] == ')' {
            bail!("CPF0006: The command has errors and was not run. Unexpected parenthesis.");
        }
        let a = atom(&cs, &mut i)?;
        match (&a, cs.get(i)) {
            (
                Item::Atom {
                    text,
                    quoted: false,
                },
                Some('('),
            ) => {
                let vals = list(&cs, &mut i)?;
                out.push((Some(text.clone()), vals));
            }
            _ => out.push((None, vec![a])),
        }
    }
    Ok(out)
}

/// The items of a definition keyword's raw text (inside its parentheses).
fn raw_items(raw: &str) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    for (k, v) in parse_values(raw)? {
        if let Some(k) = k {
            // A word followed by '(' inside a value: keep it as text.
            out.push(Item::word(&format!(
                "{k}({})",
                v.iter().map(|x| x.text()).collect::<Vec<_>>().join(" ")
            )));
        } else {
            out.extend(v);
        }
    }
    Ok(out)
}

/// One text value of a definition keyword (quotes removed).
fn raw_text(raw: &str) -> Result<String> {
    Ok(raw_items(raw)?
        .first()
        .map(|i| match i {
            Item::Atom { text, .. } => text.clone(),
            other => other.text(),
        })
        .unwrap_or_default())
}

// ------------------------------------------------------------ CRTCMD

/// The default length of a type (CRTCMD: PARM LEN).
fn default_len(ty: &str) -> (u32, u32) {
    match ty {
        "*DEC" => (15, 5),
        "*LGL" => (1, 0),
        "*CHAR" => (32, 0),
        "*NAME" | "*SNAME" | "*CNAME" | "*GENERIC" => (10, 0),
        "*VARNAME" => (11, 0),
        "*HEX" => (1, 0),
        "*PNAME" => (32, 0),
        "*CMDSTR" => (256, 0),
        "*DATE" => (7, 0),
        "*TIME" => (6, 0),
        "*INT2" | "*UINT2" => (2, 0),
        "*INT4" | "*UINT4" => (4, 0),
        _ => (0, 0),
    }
}

const SIMPLE_TYPES: &[&str] = &[
    "*DEC",
    "*LGL",
    "*CHAR",
    "*NAME",
    "*SNAME",
    "*CNAME",
    "*GENERIC",
    "*VARNAME",
    "*HEX",
    "*PNAME",
    "*CMDSTR",
    "*DATE",
    "*TIME",
    "*INT2",
    "*INT4",
    "*UINT2",
    "*UINT4",
    "*NULL",
    "*ZEROELEM",
    "*X",
];

/// The pairs of SPCVAL and SNGVAL: ((from to) ...), or single values.
fn pairs(raw: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for it in raw_items(raw)? {
        match it {
            Item::List(v) => {
                let from = v.first().map(|x| x.text()).unwrap_or_default();
                let to = v
                    .get(1)
                    .map(|x| match x {
                        Item::Atom { text, .. } => text.clone(),
                        other => other.text(),
                    })
                    .unwrap_or_else(|| from.clone());
                out.push((from.to_ascii_uppercase(), to));
            }
            Item::Atom { text, .. } => out.push((text.to_ascii_uppercase(), text)),
        }
    }
    Ok(out)
}

fn parse_parm(st: &crate::clpgm::Statement, order: &[&str]) -> Result<Parm> {
    let p = named(st, order)?;
    let get = |k: &str| p.get(k).map(|s| s.as_str());
    let ty = get("TYPE")
        .map(|t| t.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "*CHAR".into());
    let (mut len, mut dec) = default_len(&ty);
    if let Some(l) = get("LEN") {
        let mut w = l.split_whitespace();
        if let Some(n) = w.next() {
            len = n
                .parse()
                .map_err(|_| anyhow!("line {}: LEN({l}) is not valid", st.line))?;
        }
        dec = w.next().and_then(|d| d.parse().ok()).unwrap_or(0);
    }
    let yes = |k: &str| get(k).is_some_and(|v| v.trim().eq_ignore_ascii_case("*YES"));
    let num = |k: &str, d: u32| -> Result<u32> {
        match get(k) {
            None => Ok(d),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|_| anyhow!("line {}: {k}({v}) is not valid", st.line)),
        }
    };
    let constant = get("CONSTANT").map(raw_text).transpose()?;
    let min = num("MIN", 0)?;
    let max = num("MAX", 1)?;
    if max < min.max(1) {
        bail!("line {}: MAX must be at least MIN and 1", st.line);
    }
    let range = match get("RANGE") {
        Some(r) => {
            let v = raw_items(r)?;
            if v.len() != 2 {
                bail!("line {}: RANGE needs a low and a high value", st.line);
            }
            Some((v[0].text(), v[1].text()))
        }
        None => None,
    };
    let rel = match get("REL") {
        Some(r) => {
            let v = raw_items(r)?;
            if v.len() != 2 {
                bail!("line {}: REL needs an operator and a value", st.line);
            }
            Some((v[0].text(), v[1].text()))
        }
        None => None,
    };
    Ok(Parm {
        kwd: get("KWD")
            .map(|k| k.trim().to_ascii_uppercase())
            .unwrap_or_default(),
        ty,
        len,
        dec,
        constant,
        rstd: yes("RSTD"),
        dft: get("DFT").map(|d| d.trim().to_string()),
        values: raw_items(get("VALUES").unwrap_or(""))?
            .iter()
            .map(|v| match v {
                Item::Atom { text, .. } => text.clone(),
                other => other.text(),
            })
            .collect(),
        range,
        rel,
        spcval: pairs(get("SPCVAL").unwrap_or(""))?,
        sngval: pairs(get("SNGVAL").unwrap_or(""))?,
        min,
        max,
        vary: yes("VARY"),
        mixed: get("CASE").is_some_and(|c| c.trim().eq_ignore_ascii_case("*MIXED")),
        full: yes("FULL"),
        prompt: get("PROMPT").map(raw_text).transpose()?.unwrap_or_default(),
        pmtrqs: get("PMTCTL").is_some_and(|c| c.trim().eq_ignore_ascii_case("*PMTRQS")),
    })
}

/// Keywords of the statements (positional order first).
const PARM_KWS: &[&str] = &[
    "KWD",
    "TYPE",
    "LEN",
    "CONSTANT",
    "RSTD",
    "DFT",
    "VALUES",
    "REL",
    "RANGE",
    "SPCVAL",
    "SNGVAL",
    "MIN",
    "MAX",
    "ALWUNPRT",
    "ALWVAR",
    "PGM",
    "DTAARA",
    "FILE",
    "FULL",
    "EXPR",
    "VARY",
    "PASSATR",
    "PASSVAL",
    "CASE",
    "LISTDSPL",
    "DSPINPUT",
    "CHOICE",
    "CHOICEPGM",
    "PMTCTL",
    "PMTCTLPGM",
    "PROMPT",
    "INLPMTLEN",
    "KEYPARM",
    "RTNVAL",
    "CCSID",
    "ALWHEX",
];
const ELEM_KWS: &[&str] = &[
    "TYPE",
    "LEN",
    "CONSTANT",
    "RSTD",
    "DFT",
    "VALUES",
    "REL",
    "RANGE",
    "SPCVAL",
    "SNGVAL",
    "MIN",
    "MAX",
    "ALWUNPRT",
    "ALWVAR",
    "PGM",
    "DTAARA",
    "FILE",
    "FULL",
    "EXPR",
    "VARY",
    "PASSATR",
    "CASE",
    "LISTDSPL",
    "DSPINPUT",
    "CHOICE",
    "CHOICEPGM",
    "INLPMTLEN",
    "PROMPT",
    "CCSID",
    "ALWHEX",
];
const QUAL_KWS: &[&str] = &[
    "TYPE",
    "LEN",
    "CONSTANT",
    "RSTD",
    "DFT",
    "VALUES",
    "REL",
    "RANGE",
    "SPCVAL",
    "MIN",
    "ALWUNPRT",
    "ALWVAR",
    "PGM",
    "DTAARA",
    "FILE",
    "FULL",
    "EXPR",
    "VARY",
    "PASSATR",
    "CASE",
    "DSPINPUT",
    "CHOICE",
    "CHOICEPGM",
    "INLPMTLEN",
    "PROMPT",
    "CCSID",
    "ALWHEX",
];

/// Read command definition source (CRTCMD).
pub fn compile(library: &str, name: &str, pgm: &str, source: &str) -> Result<Definition> {
    let mut d = Definition {
        library: library.to_string(),
        name: name.to_string(),
        pgm: pgm.to_string(),
        ..Default::default()
    };
    let mut seen_cmd = false;
    let mut label_of: Option<String> = None;
    let mut current: Option<String> = None;
    for (line, text) in logical_lines(source)? {
        let st = parse_statement(line, &text)?;
        // A label on a line of its own names the next statement.
        let label = st.label.clone().or_else(|| label_of.take());
        if st.name.is_empty() {
            label_of = label;
            continue;
        }
        match st.name.as_str() {
            "CMD" => {
                let p = named(
                    &st,
                    &[
                        "PROMPT",
                        "PMTFILE",
                        "MSGF",
                        "CCSID",
                        "HLPPNLGRP",
                        "HLPID",
                        "TEXT",
                        "MAXPOS",
                    ],
                )?;
                d.prompt = p
                    .get("PROMPT")
                    .map(|x| raw_text(x))
                    .transpose()?
                    .unwrap_or_default();
                seen_cmd = true;
            }
            "PARM" => {
                current = None;
                if !seen_cmd {
                    bail!("line {line}: the CMD statement must come first");
                }
                let p = parse_parm(&st, PARM_KWS)?;
                if p.kwd.is_empty() {
                    bail!("line {line}: PARM needs KWD");
                }
                if d.parms.iter().any(|x| x.kwd == p.kwd) {
                    bail!("line {line}: keyword {} is defined twice", p.kwd);
                }
                d.parms.push(p);
            }
            "ELEM" | "QUAL" => {
                // The first ELEM or QUAL of a list has the label; the
                // ones after it belong to the same list.
                let label = match label {
                    Some(l) => {
                        current = Some(l.clone());
                        l
                    }
                    None => current
                        .clone()
                        .ok_or_else(|| anyhow!("line {line}: {} needs a label", st.name))?,
                };
                let p = parse_parm(
                    &st,
                    if st.name == "ELEM" {
                        ELEM_KWS
                    } else {
                        QUAL_KWS
                    },
                )?;
                let qual = st.name == "QUAL";
                let l = d.lists.entry(label.clone()).or_insert_with(|| List {
                    qual,
                    items: vec![],
                });
                if l.qual != qual {
                    bail!("line {line}: label {label} has both ELEM and QUAL statements");
                }
                l.items.push(p);
            }
            // Dependencies and prompt control are kept out: as4tainr
            // checks values, not relations between parameters.
            "DEP" | "PMTCTL" => {}
            other => bail!("line {line}: {other} is not a command definition statement"),
        }
    }
    if !seen_cmd {
        bail!("The source has no CMD statement.");
    }
    if d.parms.len() > 99 {
        bail!("A command can have at most 99 parameters.");
    }
    // Every type that names a label has its statements.
    let all: Vec<&Parm> = d
        .parms
        .iter()
        .chain(d.lists.values().flat_map(|l| l.items.iter()))
        .collect();
    for p in all {
        if !SIMPLE_TYPES.contains(&p.ty.as_str()) && !d.lists.contains_key(&p.ty) {
            bail!(
                "TYPE({}) of {}: no ELEM or QUAL statements have that label",
                p.ty,
                if p.kwd.is_empty() {
                    "an element"
                } else {
                    &p.kwd
                }
            );
        }
    }
    Ok(d)
}

// ------------------------------------------------------------ catalog

/// Store a command (CRTCMD).
pub async fn create(client: &mut Client, d: &Definition, text: &str, replace: bool) -> Result<()> {
    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&d.library],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {} does not exist.", d.library);
    }
    let exists = tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*CMD'",
            &[&d.library, &d.name],
        )
        .await?
        .is_some();
    if exists && !replace {
        bail!(
            "Command {} already exists in library {}; use REPLACE(*YES) to replace it.",
            d.name,
            d.library
        );
    }
    tx.execute(
        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*CMD'",
        &[&d.library, &d.name],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*CMD','',$3)",
        &[&d.library, &d.name, &text],
    )
    .await?;
    let json = serde_json::to_string(d)?;
    tx.execute(
        "INSERT INTO as4tainr.commands (library, name, definition) VALUES ($1,$2,$3)
         ON CONFLICT (library, name) DO UPDATE SET definition = EXCLUDED.definition",
        &[&d.library, &d.name, &json],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The first library of `libs` with the command.
pub async fn find(client: &Client, libs: &[String], name: &str) -> Result<Option<Definition>> {
    for lib in libs {
        if let Some(r) = client
            .query_opt(
                "SELECT definition FROM as4tainr.commands WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            let d: Definition = serde_json::from_str(r.get::<_, &str>(0))?;
            return Ok(Some(d));
        }
    }
    Ok(None)
}

/// DLTCMD: true when it was there.
pub async fn delete(client: &Client, library: &str, name: &str) -> Result<bool> {
    let n = client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*CMD'",
            &[&library, &name],
        )
        .await?;
    client
        .execute(
            "DELETE FROM as4tainr.commands WHERE library=$1 AND name=$2",
            &[&library, &name],
        )
        .await?;
    Ok(n > 0)
}

// ------------------------------------------------------------ running

fn bad(kwd: &str, msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("CPF0006: The command has errors and was not run. {kwd}: {msg}")
}

/// A PARM and its values as typed (None when not given).
pub type Bound<'a> = (&'a Parm, Option<Vec<Item>>);

/// The parameters of a command line, checked: (PARM, its values or None
/// when not given).
pub fn bind<'a>(d: &'a Definition, line: &str) -> Result<Vec<Bound<'a>>> {
    let line = line.trim();
    let end = line
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(line.len());
    let given = parse_values(&line[end..])?;
    let mut vals: BTreeMap<usize, Vec<Item>> = BTreeMap::new();
    let positional: Vec<usize> = d
        .parms
        .iter()
        .enumerate()
        .filter(|(_, p)| p.constant.is_none())
        .map(|(i, _)| i)
        .take(d.maxpos.unwrap_or(usize::MAX))
        .collect();
    let mut next_pos = positional.iter();
    for (k, v) in given {
        let i = match k {
            Some(k) => d
                .parms
                .iter()
                .position(|p| p.kwd == k && p.constant.is_none())
                .ok_or_else(|| {
                    anyhow!(
                        "CPF0006: The command has errors and was not run. Keyword {k} not valid for command {}.",
                        d.name
                    )
                })?,
            None => *next_pos
                .find(|i| !vals.contains_key(i))
                .ok_or_else(|| {
                    anyhow!(
                        "CPF0006: The command has errors and was not run. Too many positional parameters for command {}.",
                        d.name
                    )
                })?,
        };
        if vals.insert(i, v).is_some() {
            bail!(
                "CPF0006: The command has errors and was not run. Keyword {} specified more than once.",
                d.parms[i].kwd
            );
        }
    }
    Ok(d.parms
        .iter()
        .enumerate()
        .map(|(i, p)| (p, vals.remove(&i)))
        .collect())
}

fn be16(n: usize) -> [u8; 2] {
    (n as u16).to_be_bytes()
}

/// How a top-level parameter goes to the CPP.
pub enum Arg {
    /// A number (*DEC, *INTn): the CPP declares it as a number.
    Number(String),
    /// Bytes (EBCDIC): characters, lists, qualified names.
    Bytes(Vec<u8>),
}

/// Encode every parameter for the CPP.
pub fn encode(d: &Definition, line: &str) -> Result<Vec<Arg>> {
    let mut out = Vec::new();
    for (p, v) in bind(d, line)? {
        out.push(encode_parm(d, p, v)?);
    }
    Ok(out)
}

fn encode_parm(d: &Definition, p: &Parm, given: Option<Vec<Item>>) -> Result<Arg> {
    let items: Option<Vec<Item>> = match (&p.constant, given) {
        (Some(c), _) => Some(vec![Item::Atom {
            text: c.clone(),
            quoted: true,
        }]),
        (None, Some(v)) => Some(v),
        (None, None) => match &p.dft {
            Some(dft) => Some(raw_items(dft)?),
            None if p.min > 0 => bail!(
                "CPF0006: The command has errors and was not run. Value for required parameter {} is missing.",
                p.kwd
            ),
            None => None,
        },
    };
    let simple_number = !p.is_list()
        && matches!(
            p.ty.as_str(),
            "*DEC" | "*INT2" | "*INT4" | "*UINT2" | "*UINT4"
        );
    if simple_number {
        // A single value of a list parameter is not a number.
        let n = match items.as_deref() {
            None | Some([]) => "0".to_string(),
            Some([one]) => number_text(d, p, one)?,
            Some(_) => return Err(bad(&p.kwd, "only one value is allowed")),
        };
        return Ok(Arg::Number(n));
    }
    Ok(Arg::Bytes(encode_value(d, p, items)?))
}

/// The number a single numeric value stands for (special values mapped).
fn number_text(d: &Definition, p: &Parm, it: &Item) -> Result<String> {
    let b = encode_one(d, p, Some(it))?;
    let t = crate::rpg::data::DType::Packed(p.len, p.dec);
    Ok(match p.ty.as_str() {
        "*DEC" => crate::rpg::data::load(&t, &b)?.num()?.to_string(),
        "*INT2" => i16::from_be_bytes([b[0], b[1]]).to_string(),
        "*UINT2" => u16::from_be_bytes([b[0], b[1]]).to_string(),
        "*UINT4" => u32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
        _ => i32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
    })
}

/// A parameter's or element's whole value: a list, a qualified name, a
/// mixed list, or one value.
fn encode_value(d: &Definition, p: &Parm, items: Option<Vec<Item>>) -> Result<Vec<u8>> {
    let label = d.lists.get(&p.ty);
    if p.is_list() {
        let vals = items.unwrap_or_default();
        // A single value (SNGVAL) stands for the whole list.
        if let [Item::Atom { text, .. }] = vals.as_slice() {
            let up = text.to_ascii_uppercase();
            if let Some((_, to)) = p.sngval.iter().find(|(f, _)| *f == up) {
                let mut out = be16(1).to_vec();
                out.extend(single(d, p, to)?);
                return Ok(out);
            }
        }
        // Each list entry is one value: a mixed list is one item in
        // parentheses.
        let entries: Vec<Item> = vals;
        if (entries.len() as u32) < p.min {
            return Err(bad(&p.kwd, format!("at least {} values are needed", p.min)));
        }
        if entries.len() as u32 > p.max {
            return Err(bad(&p.kwd, format!("at most {} values are allowed", p.max)));
        }
        return match label {
            // A list of mixed lists: the count, an offset of each from
            // the start of the parameter, then the mixed lists.
            Some(l) if !l.qual => {
                let parts: Vec<Vec<u8>> = entries
                    .iter()
                    .map(|e| mixed(d, &p.kwd, l, Some(e.elements())))
                    .collect::<Result<_>>()?;
                let mut out = be16(parts.len()).to_vec();
                let mut at = 2 + 2 * parts.len();
                for part in &parts {
                    out.extend(be16(at));
                    at += part.len();
                }
                for part in parts {
                    out.extend(part);
                }
                Ok(out)
            }
            Some(l) => {
                let mut out = be16(entries.len()).to_vec();
                for e in &entries {
                    out.extend(qualified(d, &p.kwd, l, Some(e))?);
                }
                Ok(out)
            }
            None => {
                let mut out = be16(entries.len()).to_vec();
                for e in &entries {
                    out.extend(encode_one(d, p, Some(e))?);
                }
                Ok(out)
            }
        };
    }
    match label {
        Some(l) if l.qual => {
            let one = match items.as_deref() {
                None | Some([]) => None,
                Some([x]) => Some(x.clone()),
                Some(_) => return Err(bad(&p.kwd, "only one value is allowed")),
            };
            qualified(d, &p.kwd, l, one.as_ref())
        }
        Some(l) => {
            // A mixed list: its elements as typed (KWD(a b c)).
            let elems = items.map(|v| match v.as_slice() {
                [Item::List(inner)] => inner.clone(),
                _ => v,
            });
            // A single value for the mixed list.
            if let Some(v) = &elems {
                if let [Item::Atom { text, .. }] = v.as_slice() {
                    let up = text.to_ascii_uppercase();
                    if let Some((_, to)) = p.sngval.iter().find(|(f, _)| *f == up) {
                        let first = &l.items[0];
                        let mut out = be16(1).to_vec();
                        out.extend(single(d, first, to)?);
                        return Ok(out);
                    }
                }
            }
            mixed(d, &p.kwd, l, elems)
        }
        None => match items.as_deref() {
            None | Some([]) => encode_one(d, p, None),
            Some([x]) => encode_one(d, p, Some(x)),
            Some(_) => Err(bad(&p.kwd, "only one value is allowed")),
        },
    }
}

/// A mixed list: the number of elements, then each element (omitted ones
/// take their defaults).
fn mixed(d: &Definition, kwd: &str, l: &List, elems: Option<Vec<Item>>) -> Result<Vec<u8>> {
    let elems = elems.unwrap_or_default();
    if elems.len() > l.items.len() {
        return Err(bad(
            kwd,
            format!("at most {} elements are allowed", l.items.len()),
        ));
    }
    let mut out = be16(l.items.len()).to_vec();
    for (k, e) in l.items.iter().enumerate() {
        let given = elems.get(k).map(|x| x.elements());
        // *N: the element is not given.
        let given = given.filter(
            |g| !matches!(g.as_slice(), [Item::Atom { text, quoted: false }] if text == "*N"),
        );
        let vals = match (&e.constant, given) {
            (Some(c), _) => Some(vec![Item::Atom {
                text: c.clone(),
                quoted: true,
            }]),
            (None, Some(v)) => Some(v),
            (None, None) => match &e.dft {
                Some(dft) => Some(raw_items(dft)?),
                None if e.min > 0 => {
                    return Err(bad(kwd, format!("element {} is required", k + 1)))
                }
                None => None,
            },
        };
        out.extend(encode_value(d, e, vals)?);
    }
    Ok(out)
}

/// A qualified name: LIB/OBJ is typed library first; the parts go to the
/// CPP object first.
fn qualified(d: &Definition, kwd: &str, l: &List, item: Option<&Item>) -> Result<Vec<u8>> {
    let mut parts: Vec<Item> = match item {
        None => vec![],
        Some(Item::Atom { text, quoted }) => text
            .split('/')
            .rev()
            .map(|t| Item::Atom {
                text: t.to_string(),
                quoted: *quoted,
            })
            .collect(),
        Some(Item::List(_)) => return Err(bad(kwd, "a qualified name is expected")),
    };
    if parts.len() > l.items.len() {
        return Err(bad(kwd, "the name has too many qualifiers"));
    }
    let mut out = Vec::new();
    for (k, q) in l.items.iter().enumerate() {
        let given = if k < parts.len() {
            Some(std::mem::replace(&mut parts[k], Item::word("")))
        } else {
            None
        };
        let it = match (&q.constant, given) {
            (Some(c), _) => Some(Item::Atom {
                text: c.clone(),
                quoted: true,
            }),
            (None, Some(v)) => Some(v),
            (None, None) => match &q.dft {
                Some(dft) => raw_items(dft)?.into_iter().next(),
                None if q.min > 0 => {
                    return Err(bad(kwd, format!("qualifier {} is required", k + 1)))
                }
                None => None,
            },
        };
        out.extend(encode_one(d, q, it.as_ref())?);
    }
    Ok(out)
}

/// A special value's replacement as the parameter's bytes.
fn single(d: &Definition, p: &Parm, to: &str) -> Result<Vec<u8>> {
    // The replacement is taken as written, without the checks a typed
    // value gets.
    let raw = Parm {
        rstd: false,
        spcval: vec![],
        range: None,
        rel: None,
        values: vec![],
        ..p.clone()
    };
    let _ = d;
    encode_scalar(&raw, to, true, true)
}

/// One simple value: special values, restricted values, the type.
fn encode_one(d: &Definition, p: &Parm, it: Option<&Item>) -> Result<Vec<u8>> {
    let Some(it) = it else {
        // Not given, no default: blanks, zero, '0' (the CPP gets the
        // parameter all the same).
        return encode_scalar(p, "", true, true);
    };
    let (text, quoted) = match it {
        Item::Atom { text, quoted } => (text.clone(), *quoted),
        Item::List(_) => return Err(bad(&p.kwd, "a single value is expected, not a list")),
    };
    let up = text.to_ascii_uppercase();
    if !quoted || !p.mixed {
        if let Some((_, to)) = p.spcval.iter().find(|(f, _)| *f == up) {
            return single(d, p, to);
        }
    }
    if p.rstd {
        let ok = p.values.iter().any(|v| v.eq_ignore_ascii_case(&text))
            || p.spcval.iter().any(|(f, _)| *f == up);
        if !ok {
            return Err(bad(
                &p.kwd,
                format!(
                    "'{text}' is not one of the values allowed ({})",
                    p.values.join(" ")
                ),
            ));
        }
    }
    encode_scalar(p, &text, quoted, false)
}

/// A value of a simple type as bytes (CPP format).
fn encode_scalar(p: &Parm, text: &str, quoted: bool, special: bool) -> Result<Vec<u8>> {
    use crate::rpg::data::{self, DType, Fit};
    let kwd = if p.kwd.is_empty() { "element" } else { &p.kwd };
    let len = p.len as usize;
    let chars = |t: &str| -> Result<Vec<u8>> {
        let t = if p.mixed && quoted {
            t.to_string()
        } else {
            t.to_ascii_uppercase()
        };
        let n = t.chars().count();
        if n > len {
            return Err(bad(kwd, format!("'{t}' is longer than {len}")));
        }
        if p.full && n != len && !t.is_empty() {
            return Err(bad(kwd, format!("'{t}' must be exactly {len} long")));
        }
        let mut b = data::e(&t);
        b.resize(len, 0x40);
        if p.vary {
            let mut v = be16(n).to_vec();
            v.extend(b);
            return Ok(v);
        }
        Ok(b)
    };
    let check_range = |v: &str| -> Result<()> {
        let cmp = |a: &str, b: &str| -> std::cmp::Ordering {
            match (
                crate::rpg::dec::Dec::parse(a.trim()),
                crate::rpg::dec::Dec::parse(b.trim()),
            ) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        };
        if let Some((lo, hi)) = &p.range {
            if cmp(v, lo).is_lt() || cmp(v, hi).is_gt() {
                return Err(bad(kwd, format!("'{v}' is not in the range {lo} to {hi}")));
            }
        }
        if let Some((op, x)) = &p.rel {
            use std::cmp::Ordering::*;
            let o = cmp(v, x);
            let ok = match op.as_str() {
                "*EQ" => o == Equal,
                "*NE" => o != Equal,
                "*GT" => o == Greater,
                "*LT" => o == Less,
                "*GE" | "*NL" => o != Less,
                "*LE" | "*NG" => o != Greater,
                _ => true,
            };
            if !ok {
                return Err(bad(kwd, format!("'{v}' is not {op} {x}")));
            }
        }
        Ok(())
    };
    match p.ty.as_str() {
        "*CHAR" | "*PNAME" | "*CMDSTR" | "*X" => {
            if !special {
                check_range(text)?;
            }
            chars(text)
        }
        "*NAME" | "*SNAME" | "*CNAME" | "*GENERIC" | "*VARNAME" => {
            if !special && !text.is_empty() {
                let t = text.to_ascii_uppercase();
                let base = match t.strip_suffix('*') {
                    Some(b) if p.ty == "*GENERIC" && !b.is_empty() => b.to_string(),
                    _ => t.clone(),
                };
                let base = base
                    .strip_prefix('&')
                    .filter(|_| p.ty == "*VARNAME")
                    .unwrap_or(&base);
                validate_name("object", base).map_err(|e| bad(kwd, e))?;
                check_range(text)?;
            }
            chars(text)
        }
        "*LGL" => {
            let v = match text.trim() {
                "" | "0" => "0",
                "1" => "1",
                other => {
                    return Err(bad(
                        kwd,
                        format!("'{other}' is not a logical value ('0' or '1')"),
                    ))
                }
            };
            Ok(data::e(v))
        }
        "*DEC" => {
            let t = if text.trim().is_empty() {
                "0"
            } else {
                text.trim()
            };
            let n = crate::rpg::dec::Dec::parse(t)
                .map_err(|_| bad(kwd, format!("'{t}' is not a number")))?;
            if !special {
                check_range(t)?;
            }
            let ty = DType::Packed(p.len, p.dec);
            let mut b = ty.initial();
            data::store(
                &ty,
                &data::Value::Num(n),
                &mut b,
                Fit::Strict { half_adjust: false },
                false,
            )
            .map_err(|_| {
                bad(
                    kwd,
                    format!(
                        "'{t}' does not fit in {} digits with {} decimals",
                        p.len, p.dec
                    ),
                )
            })?;
            Ok(b)
        }
        "*INT2" | "*INT4" | "*UINT2" | "*UINT4" => {
            let t = if text.trim().is_empty() {
                "0"
            } else {
                text.trim()
            };
            let n: i64 = t
                .parse()
                .map_err(|_| bad(kwd, format!("'{t}' is not an integer")))?;
            if !special {
                check_range(t)?;
            }
            let (lo, hi) = match p.ty.as_str() {
                "*INT2" => (i16::MIN as i64, i16::MAX as i64),
                "*UINT2" => (0, u16::MAX as i64),
                "*UINT4" => (0, u32::MAX as i64),
                _ => (i32::MIN as i64, i32::MAX as i64),
            };
            if n < lo || n > hi {
                return Err(bad(kwd, format!("{n} is out of range")));
            }
            Ok(if p.ty.ends_with('2') {
                (n as u16).to_be_bytes().to_vec()
            } else {
                (n as u32).to_be_bytes().to_vec()
            })
        }
        "*HEX" => {
            let h = text
                .trim()
                .strip_prefix("X'")
                .or_else(|| text.trim().strip_prefix("x'"))
                .and_then(|x| x.strip_suffix('\''))
                .unwrap_or(text.trim());
            if !h.len().is_multiple_of(2) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad(kwd, format!("'{text}' is not a hexadecimal value")));
            }
            let mut b: Vec<u8> = (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or(0))
                .collect();
            if b.len() > len {
                return Err(bad(kwd, format!("'{text}' is longer than {len} bytes")));
            }
            // Right-adjusted, zeros on the left.
            let mut out = vec![0u8; len - b.len()];
            out.append(&mut b);
            Ok(out)
        }
        // *DATE: CYYMMDD (C: 0 for 19xx, 1 for 20xx); typed as YYYY-MM-DD,
        // MMDDYY, MMDDYYYY or with separators in month-day-year order.
        "*DATE" => {
            if text.trim().is_empty() {
                return chars("");
            }
            let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
            let (y, m, dd) = if text.trim().len() == 10 && text.as_bytes().get(4) == Some(&b'-') {
                (&digits[0..4], &digits[4..6], &digits[6..8])
            } else {
                match digits.len() {
                    8 => (&digits[4..8], &digits[0..2], &digits[2..4]),
                    6 => (&digits[4..6], &digits[0..2], &digits[2..4]),
                    _ => return Err(bad(kwd, format!("'{text}' is not a date"))),
                }
            };
            let y: u32 = y.parse().unwrap_or(0);
            let y = if y < 100 {
                if y < 40 {
                    2000 + y
                } else {
                    1900 + y
                }
            } else {
                y
            };
            let (m, dd): (u32, u32) = (m.parse().unwrap_or(0), dd.parse().unwrap_or(0));
            if !(1..=12).contains(&m) || !(1..=31).contains(&dd) || !(1900..=2899).contains(&y) {
                return Err(bad(kwd, format!("'{text}' is not a valid date")));
            }
            Ok(data::e(&format!(
                "{}{:02}{m:02}{dd:02}",
                (y - 1900) / 100,
                y % 100
            )))
        }
        "*TIME" => {
            if text.trim().is_empty() {
                return chars("");
            }
            let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() != 6 {
                return Err(bad(kwd, format!("'{text}' is not a time")));
            }
            let (h, m, s): (u32, u32, u32) = (
                digits[0..2].parse().unwrap_or(99),
                digits[2..4].parse().unwrap_or(99),
                digits[4..6].parse().unwrap_or(99),
            );
            if h > 23 || m > 59 || s > 59 {
                return Err(bad(kwd, format!("'{text}' is not a valid time")));
            }
            Ok(data::e(&digits))
        }
        "*NULL" | "*ZEROELEM" => Ok(vec![]),
        other => Err(bad(kwd, format!("type {other} is not supported"))),
    }
}

/// The CPP's parameters as CALL passes them: numbers as their digits,
/// everything else as its bytes (each byte one character).
pub fn call_args(args: Vec<Arg>) -> Vec<String> {
    args.into_iter()
        .map(|a| match a {
            Arg::Number(n) => n,
            Arg::Bytes(b) => crate::rpg::data::a(&b),
        })
        .collect()
}

// ------------------------------------------------------------ prompting

/// The command as the prompter (F4) sees it: a static definition made
/// once per command version.
pub fn prompt_def(d: &Definition) -> &'static crate::cl::CommandDef {
    use crate::cl::{CommandDef, Kind, ParamDef};
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, &'static CommandDef>>> = Mutex::new(None);
    let key = serde_json::to_string(d).unwrap_or_default();
    if let Some(def) = CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&key).copied()))
    {
        return def;
    }
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let params: Vec<ParamDef> = d
        .parms
        .iter()
        .filter(|p| p.constant.is_none())
        .map(|p| {
            let list = d.lists.get(&p.ty);
            let kind = match list {
                Some(l) if l.qual && l.items.len() == 2 && !p.is_list() => Kind::Qualified,
                Some(_) => Kind::List,
                None if p.is_list() => Kind::List,
                None if matches!(p.ty.as_str(), "*NAME" | "*SNAME" | "*CNAME" | "*GENERIC") => {
                    Kind::Name
                }
                None => Kind::Text,
            };
            let mut choices: Vec<String> = Vec::new();
            if !p.rstd {
                choices.push(
                    match p.ty.as_str() {
                        "*DEC" | "*INT2" | "*INT4" | "*UINT2" | "*UINT4" => "Number",
                        "*LGL" => "0, 1",
                        "*NAME" | "*SNAME" | "*CNAME" => "Name",
                        "*GENERIC" => "Generic name",
                        "*DATE" => "Date",
                        "*TIME" => "Time",
                        _ if list.is_some() => "Values",
                        _ => "Character value",
                    }
                    .to_string(),
                );
            }
            choices.extend(p.values.iter().cloned());
            choices.extend(p.spcval.iter().map(|(f, _)| f.clone()));
            choices.extend(p.sngval.iter().map(|(f, _)| f.clone()));
            choices.dedup();
            let default = p.dft.as_ref().map(|t| {
                let t = raw_items(t)
                    .ok()
                    .and_then(|v| v.into_iter().next())
                    .map(|i| match i {
                        Item::Atom { text, .. } => text,
                        other => other.text(),
                    })
                    .unwrap_or_default();
                leak(t)
            });
            ParamDef {
                keyword: leak(p.kwd.clone()),
                kind,
                required: p.min > 0,
                positional: true,
                default,
                prompt: leak(if p.prompt.is_empty() {
                    p.kwd.clone()
                } else {
                    p.prompt.clone()
                }),
                choices: leak(choices.join(", ")),
            }
        })
        .collect();
    let def: &'static CommandDef = Box::leak(Box::new(CommandDef {
        name: leak(d.name.clone()),
        description: leak(if d.prompt.is_empty() {
            d.name.clone()
        } else {
            d.prompt.clone()
        }),
        params: Box::leak(params.into_boxed_slice()),
    }));
    if let Ok(mut g) = CACHE.lock() {
        g.get_or_insert_with(HashMap::new).insert(key, def);
    }
    crate::cl::register_user(def);
    def
}

#[cfg(test)]
mod tests {
    use super::*;

    // CL Programming, "Data passed to the command processing program":
    // a qualified name is its parts, object first; a simple list is a
    // 2-byte count and the values; a mixed list is the count of elements
    // and the elements; *DEC is passed as a number; SPCVAL replaces a
    // special value; CASE(*MONO) folds quoted text to upper case.
    const SRC: &str = "
             CMD        PROMPT('Test command')
             PARM       KWD(OUTQ) TYPE(Q1) MIN(1) PROMPT('Output queue')
Q1:          QUAL       TYPE(*NAME) LEN(10) MIN(1)
             QUAL       TYPE(*NAME) LEN(10) DFT(*LIBL) SPCVAL((*LIBL) (*CURLIB))
             PARM       KWD(COUNT) TYPE(*DEC) LEN(5 0) DFT(1) RANGE(1 999)
             PARM       KWD(NAMES) TYPE(*CHAR) LEN(4) MAX(3)
             PARM       KWD(FLAG) TYPE(*CHAR) LEN(4) RSTD(*YES) DFT(*NO) VALUES(*YES *NO)
             PARM       KWD(TEXT) TYPE(*CHAR) LEN(6) CASE(*MIXED) DFT(*BLANK) SPCVAL((*BLANK ' '))
             PARM       KWD(PAIR) TYPE(E1)
E1:          ELEM       TYPE(*CHAR) LEN(2) DFT(AA)
             ELEM       TYPE(*INT2) DFT(7)
";

    fn def() -> Definition {
        compile("LIB", "TST", "LIB/TSTPGM", SRC).unwrap()
    }

    #[test]
    fn parameters_in_cpp_form() {
        let d = def();
        let args = encode(&d, "TST MYQ COUNT(25) NAMES(ab cd) TEXT('Hi') PAIR(XY 300)").unwrap();
        let bytes = |a: &Arg| match a {
            Arg::Bytes(b) => crate::rpg::data::a(b),
            Arg::Number(n) => n.clone(),
        };
        assert_eq!(bytes(&args[0]), "MYQ       *LIBL     ");
        assert_eq!(bytes(&args[1]), "25");
        // \u{0}\u{2}: the count 2, then the values.
        let names = match &args[2] {
            Arg::Bytes(b) => b.clone(),
            _ => panic!(),
        };
        assert_eq!(&names[..2], &[0, 2]);
        assert_eq!(crate::rpg::data::a(&names[2..]), "AB  CD  ");
        assert_eq!(bytes(&args[3]), "*NO ");
        assert_eq!(bytes(&args[4]), "Hi    ");
        let pair = match &args[5] {
            Arg::Bytes(b) => b.clone(),
            _ => panic!(),
        };
        assert_eq!(pair, [0, 2, 0xE7, 0xE8, 0x01, 0x2C]);
        // Library first when typed; the default when not.
        let a = encode(&d, "TST OUTQ(QGPL/PRT01) TEXT(*BLANK)").unwrap();
        assert_eq!(bytes(&a[0]), "PRT01     QGPL      ");
        assert_eq!(bytes(&a[4]), "      ");
        let pair = match &a[5] {
            Arg::Bytes(b) => b.clone(),
            _ => panic!(),
        };
        assert_eq!(pair, [0, 2, 0xC1, 0xC1, 0x00, 0x07]);
    }

    #[test]
    fn values_are_checked() {
        let d = def();
        assert!(encode(&d, "TST").is_err(), "OUTQ is required");
        assert!(encode(&d, "TST Q COUNT(1000)").is_err(), "out of range");
        assert!(encode(&d, "TST Q FLAG(*MAYBE)").is_err(), "restricted");
        assert!(encode(&d, "TST Q NAMES(a b c d)").is_err(), "too many");
        assert!(encode(&d, "TST Q BOGUS(1)").is_err(), "no such keyword");
        assert!(encode(&d, "TST '1BAD'").is_err(), "not a name");
        assert!(
            compile("L", "X", "L/X", "PARM KWD(A)").is_err(),
            "no CMD first"
        );
    }
}
