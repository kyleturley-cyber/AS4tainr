//! DDS display files (*DSPF): parsing, drawing a record format on a 5250
//! screen, and turning the operator's input back into field values and
//! response indicators.
//!
//! Written from IBM's public DDS reference for display files. Supported:
//!
//! - record formats (`R`), named fields (lengths, data types A/Y/S, decimal
//!   positions, usage O/I/B/H, line/position) and constants ('text', DATE,
//!   TIME, USER, SYSNAME)
//! - option indicators (positions 7-16, up to three per line, `N` for not,
//!   and `O` lines for OR)
//! - keywords: CA/CF (file or record level), DSPATR (HI RI UL ND PR PC BL),
//!   COLOR, EDTCDE (1 2 3 4 J K L M Z), ERRMSG, CHECK(LC), TEXT, OVERLAY
//! - subfiles: SFL, SFLCTL, SFLSIZ, SFLPAG, SFLDSP, SFLDSPCTL, SFLCLR,
//!   SFLDLT, SFLEND (*MORE, *PLUS), SFLNXTCHG, SFLMSG, PAGEUP/PAGEDOWN
//!   (ROLLDOWN/ROLLUP). Subfile state and paging live in `crate::workstn`.
//!
//! Anything else is parsed and ignored; see docs/fidelity-gaps.md.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

use crate::tn5250::ds::{self, attr, ffw, Aid, Screen};

/// OR of ANDs of (negated, indicator).
pub type Cond = Vec<Vec<(bool, u8)>>;

pub fn cond_true(cond: &Option<Cond>, ind: &[bool; 100]) -> bool {
    match cond {
        None => true,
        Some(groups) => groups
            .iter()
            .any(|g| g.iter().all(|&(neg, n)| ind[n as usize] != neg)),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Keyword {
    /// CAnn or CFnn with an optional response indicator.
    FunctionKey {
        key: u8,
        data: bool,
        indicator: Option<u8>,
    },
    DspAtr(Vec<String>),
    Color(String),
    EdtCde(char),
    ErrMsg {
        text: String,
        indicator: Option<u8>,
    },
    CheckLc,
    Overlay,
    /// SFL: this record format is a subfile record.
    Sfl,
    /// SFLCTL(subfile): this record format controls the named subfile.
    SflCtl(String),
    SflSiz(u16),
    SflPag(u16),
    SflDsp,
    SflDspCtl,
    SflClr,
    SflDlt,
    SflInz,
    SflNxtChg,
    /// SFLEND: *MORE, *PLUS or *SCRBAR.
    SflEnd(String),
    SflMsg {
        text: String,
        indicator: Option<u8>,
    },
    /// PAGEDOWN/ROLLUP: return to the program when paging past the last record.
    PageDown(Option<u8>),
    /// PAGEUP/ROLLDOWN: return to the program when paging before the first record.
    PageUp(Option<u8>),
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Kw {
    pub cond: Option<Cond>,
    pub kw: Keyword,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Usage {
    Output,
    Input,
    Both,
    Hidden,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub length: u16,
    /// 'A' character, 'Y'/'S' numeric.
    pub dtype: char,
    pub decimals: Option<u16>,
    pub usage: Usage,
    pub pos: Option<(u8, u8)>,
    pub cond: Option<Cond>,
    pub keywords: Vec<Kw>,
}

impl Field {
    pub fn is_numeric(&self) -> bool {
        self.decimals.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstText {
    Literal(String),
    Date,
    Time,
    User,
    SysName,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Constant {
    pub pos: (u8, u8),
    pub text: ConstText,
    pub cond: Option<Cond>,
    pub keywords: Vec<Kw>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    pub name: String,
    pub keywords: Vec<Kw>,
    pub fields: Vec<Field>,
    pub constants: Vec<Constant>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayFile {
    pub name: String,
    pub keywords: Vec<Kw>,
    pub records: Vec<Record>,
}

impl DisplayFile {
    pub fn record(&self, name: &str) -> Option<&Record> {
        self.records
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }
}

impl Record {
    /// Is a keyword present (whatever its condition)?
    pub fn has(&self, pred: impl Fn(&Keyword) -> bool) -> bool {
        self.keywords.iter().any(|k| pred(&k.kw))
    }

    /// Is a keyword present and its condition true?
    pub fn active(&self, ind: &[bool; 100], pred: impl Fn(&Keyword) -> bool) -> bool {
        self.keywords
            .iter()
            .any(|k| pred(&k.kw) && cond_true(&k.cond, ind))
    }

    pub fn is_subfile(&self) -> bool {
        self.has(|k| *k == Keyword::Sfl)
    }

    /// For a subfile control record, the subfile record format it controls.
    pub fn subfile_control(&self) -> Option<&str> {
        self.keywords.iter().find_map(|k| match &k.kw {
            Keyword::SflCtl(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn sflsiz(&self) -> u16 {
        self.keywords
            .iter()
            .find_map(|k| match k.kw {
                Keyword::SflSiz(n) => Some(n),
                _ => None,
            })
            .unwrap_or(1)
    }

    pub fn sflpag(&self) -> u16 {
        self.keywords
            .iter()
            .find_map(|k| match k.kw {
                Keyword::SflPag(n) => Some(n),
                _ => None,
            })
            .unwrap_or(1)
    }

    /// Screen rows used by the record's fields and constants (hidden
    /// fields have no row), lowest first.
    pub fn rows(&self) -> Vec<u8> {
        let mut rows: Vec<u8> = self
            .fields
            .iter()
            .filter_map(|f| f.pos.map(|p| p.0))
            .chain(self.constants.iter().map(|c| c.pos.0))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }
}

// ---------------------------------------------------------------- parsing

fn cols(line: &str, from: usize, to: usize) -> &str {
    let start = from - 1;
    if line.len() <= start {
        return "";
    }
    line.get(start..to.min(line.len())).unwrap_or("")
}

fn parse_indicators(area: &str) -> Result<Vec<(bool, u8)>> {
    // Positions 8-16: three slots of N + two digits.
    let mut out = Vec::new();
    let chars: Vec<char> = format!("{area:<9}").chars().collect();
    for slot in 0..3 {
        let n = chars[slot * 3];
        let digits: String = chars[slot * 3 + 1..slot * 3 + 3].iter().collect();
        if digits.trim().is_empty() {
            continue;
        }
        let ind: u8 = digits
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("bad indicator '{digits}'"))?;
        if !(1..=99).contains(&ind) {
            bail!("indicator {ind} out of range 01-99");
        }
        out.push((n == 'N', ind));
    }
    Ok(out)
}

/// Split a keyword area into keywords: NAME or NAME(args), args may
/// contain quoted strings.
fn split_keywords(area: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut out = Vec::new();
    let cs: Vec<char> = area.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == ' ' {
            i += 1;
            continue;
        }
        if cs[i] == '\'' {
            // A bare literal (constant text).
            let (lit, next) = quoted(&cs, i)?;
            out.push(("'".to_string(), vec![lit]));
            i = next;
            continue;
        }
        let start = i;
        while i < cs.len() && cs[i] != '(' && cs[i] != ' ' {
            i += 1;
        }
        let name: String = cs[start..i].iter().collect::<String>().to_ascii_uppercase();
        let mut args = Vec::new();
        if i < cs.len() && cs[i] == '(' {
            i += 1;
            loop {
                while i < cs.len() && cs[i] == ' ' {
                    i += 1;
                }
                match cs.get(i) {
                    None => bail!("missing ')' after {name}"),
                    Some(')') => {
                        i += 1;
                        break;
                    }
                    Some('\'') => {
                        let (lit, next) = quoted(&cs, i)?;
                        args.push(lit);
                        i = next;
                    }
                    Some(_) => {
                        let s = i;
                        while i < cs.len() && cs[i] != ' ' && cs[i] != ')' {
                            i += 1;
                        }
                        args.push(cs[s..i].iter().collect());
                    }
                }
            }
        }
        out.push((name, args));
    }
    Ok(out)
}

fn quoted(cs: &[char], start: usize) -> Result<(String, usize)> {
    let mut i = start + 1;
    let mut s = String::new();
    loop {
        match cs.get(i) {
            None => bail!("missing closing apostrophe"),
            Some('\'') if cs.get(i + 1) == Some(&'\'') => {
                s.push('\'');
                i += 2;
            }
            Some('\'') => return Ok((s, i + 1)),
            Some(&c) => {
                s.push(c);
                i += 1;
            }
        }
    }
}

fn to_keyword(name: &str, args: &[String]) -> Result<Keyword> {
    let ind = |s: Option<&String>| -> Option<u8> { s.and_then(|v| v.trim().parse().ok()) };
    Ok(match name {
        n if (n.starts_with("CA") || n.starts_with("CF"))
            && n.len() == 4
            && n[2..].chars().all(|c| c.is_ascii_digit()) =>
        {
            let key: u8 = n[2..].parse()?;
            if !(1..=24).contains(&key) {
                bail!("{n}: function key must be 01-24");
            }
            Keyword::FunctionKey {
                key,
                data: n.starts_with("CF"),
                indicator: ind(args.first()),
            }
        }
        "DSPATR" => Keyword::DspAtr(args.iter().map(|a| a.to_ascii_uppercase()).collect()),
        "COLOR" => Keyword::Color(
            args.first()
                .cloned()
                .unwrap_or_default()
                .to_ascii_uppercase(),
        ),
        "EDTCDE" => Keyword::EdtCde(
            args.first()
                .and_then(|a| a.chars().next())
                .unwrap_or(' ')
                .to_ascii_uppercase(),
        ),
        "ERRMSG" => Keyword::ErrMsg {
            text: args.first().cloned().unwrap_or_default(),
            indicator: ind(args.get(1)),
        },
        "CHECK" if args.iter().any(|a| a.eq_ignore_ascii_case("LC")) => Keyword::CheckLc,
        "OVERLAY" => Keyword::Overlay,
        "SFL" => Keyword::Sfl,
        "SFLCTL" => Keyword::SflCtl(
            args.first()
                .map(|a| a.to_ascii_uppercase())
                .ok_or_else(|| anyhow::anyhow!("SFLCTL needs the subfile record format name"))?,
        ),
        "SFLSIZ" | "SFLPAG" => {
            let n: u16 = args
                .first()
                .and_then(|a| a.trim().parse().ok())
                .filter(|n| (1..=9999).contains(n))
                .ok_or_else(|| {
                    anyhow::anyhow!("{name} needs a number of records from 1 to 9999")
                })?;
            if name == "SFLSIZ" {
                Keyword::SflSiz(n)
            } else {
                Keyword::SflPag(n)
            }
        }
        "SFLDSP" => Keyword::SflDsp,
        "SFLDSPCTL" => Keyword::SflDspCtl,
        "SFLCLR" => Keyword::SflClr,
        "SFLDLT" => Keyword::SflDlt,
        "SFLINZ" => Keyword::SflInz,
        "SFLNXTCHG" => Keyword::SflNxtChg,
        "SFLEND" => Keyword::SflEnd(
            args.first()
                .map(|a| a.to_ascii_uppercase())
                .unwrap_or_else(|| "*PLUS".into()),
        ),
        "SFLMSG" => Keyword::SflMsg {
            text: args.first().cloned().unwrap_or_default(),
            indicator: ind(args.get(1)),
        },
        "PAGEDOWN" | "ROLLUP" => Keyword::PageDown(ind(args.first())),
        "PAGEUP" | "ROLLDOWN" => Keyword::PageUp(ind(args.first())),
        other => Keyword::Other(other.to_string()),
    })
}

enum Target {
    File,
    Record,
    Field,
    Constant,
}

pub fn parse(name: &str, source: &str) -> Result<DisplayFile> {
    let mut f = DisplayFile {
        name: name.to_ascii_uppercase(),
        ..Default::default()
    };
    let mut target = Target::File;
    let mut pending: Option<Cond> = None; // conditions on a line with nothing else
    let mut lines: Vec<String> = Vec::new();
    // Join continuation lines: a keyword area ending in '-' continues with
    // the next line's keyword area as is; '+' skips its leading blanks.
    for raw in source.lines() {
        let line = raw.trim_end_matches('\r').to_string();
        if let Some(prev) = lines.last_mut() {
            let t = prev.trim_end();
            if t.len() > 44 && (t.ends_with('-') || t.ends_with('+')) {
                let plus = t.ends_with('+');
                let keep = t.len() - 1;
                prev.truncate(keep);
                let cont = cols(&line, 45, 80).trim_end();
                prev.push_str(if plus { cont.trim_start() } else { cont });
                continue;
            }
        }
        lines.push(line);
    }

    for (idx, line) in lines.iter().enumerate() {
        let lineno = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        if !cols(line, 6, 6).eq_ignore_ascii_case("A") && !cols(line, 6, 6).trim().is_empty() {
            bail!("line {lineno}: expected form type 'A' in column 6");
        }
        if cols(line, 7, 7) == "*" {
            continue;
        }
        let andor = cols(line, 7, 7).trim().to_ascii_uppercase();
        let inds = parse_indicators(cols(line, 8, 16))
            .map_err(|e| anyhow::anyhow!("line {lineno}: {e}"))?;
        let name_type = cols(line, 17, 17).trim().to_ascii_uppercase();
        let name = cols(line, 19, 28).trim().to_ascii_uppercase();
        let length = cols(line, 30, 34).trim();
        let dtype = cols(line, 35, 35).trim().to_ascii_uppercase();
        let decimals = cols(line, 36, 37).trim();
        let usage = cols(line, 38, 38).trim().to_ascii_uppercase();
        let row = cols(line, 39, 41).trim();
        let col = cols(line, 42, 44).trim();
        // To the end of the line: continuation lines are already joined.
        let kwarea = cols(line, 45, usize::MAX).trim().to_string();

        // Build this line's condition, merging pending lines.
        let mut cond: Option<Cond> = pending.take();
        if !inds.is_empty() {
            cond = Some(match (cond, andor.as_str()) {
                (Some(mut c), "O") => {
                    c.push(inds);
                    c
                }
                (Some(mut c), _) => {
                    if let Some(last) = c.last_mut() {
                        last.extend(inds);
                    }
                    c
                }
                (None, _) => vec![inds],
            });
        }
        if name_type.is_empty() && name.is_empty() && row.is_empty() && kwarea.is_empty() {
            pending = cond; // indicators continue on the next line
            continue;
        }

        let pos = if row.is_empty() && col.is_empty() {
            None
        } else {
            let r: u8 = row
                .parse()
                .map_err(|_| anyhow::anyhow!("line {lineno}: bad line number '{row}'"))?;
            let c: u8 = col
                .parse()
                .map_err(|_| anyhow::anyhow!("line {lineno}: bad position '{col}'"))?;
            if !(1..=24).contains(&r) || !(1..=80).contains(&c) {
                bail!("line {lineno}: location {r},{c} is outside a 24x80 display");
            }
            Some((r, c))
        };
        let kws = split_keywords(&kwarea).map_err(|e| anyhow::anyhow!("line {lineno}: {e}"))?;

        if name_type == "R" {
            f.records.push(Record {
                name: name.clone(),
                ..Default::default()
            });
            target = Target::Record;
            for (k, a) in &kws {
                let kw = to_keyword(k, a).map_err(|e| anyhow::anyhow!("line {lineno}: {e}"))?;
                f.records.last_mut().unwrap().keywords.push(Kw {
                    cond: cond.clone(),
                    kw,
                });
            }
            continue;
        }
        if !name_type.is_empty() {
            // Help specifications and the like: not supported yet.
            continue;
        }

        if !name.is_empty() {
            let Some(rec) = f.records.last_mut() else {
                bail!("line {lineno}: field {name} before any record format")
            };
            let length: u16 = length
                .parse()
                .map_err(|_| anyhow::anyhow!("line {lineno}: field {name} needs a length"))?;
            let decimals: Option<u16> = if decimals.is_empty() {
                None
            } else {
                Some(decimals.parse()?)
            };
            let dtype = match (dtype.as_str(), decimals) {
                ("", Some(_)) => 'Y',
                ("", None) => 'A',
                (d, _) => d.chars().next().unwrap(),
            };
            if !"AYSNXW".contains(dtype) {
                bail!("line {lineno}: data type {dtype} is not supported");
            }
            let usage = match usage.as_str() {
                "" | "O" => Usage::Output,
                "I" => Usage::Input,
                "B" => Usage::Both,
                "H" => Usage::Hidden,
                other => bail!("line {lineno}: usage {other} is not supported"),
            };
            if usage != Usage::Hidden && pos.is_none() {
                bail!("line {lineno}: field {name} needs a line and position");
            }
            let mut fld = Field {
                name,
                length,
                dtype,
                decimals,
                usage,
                pos,
                cond: cond.clone(),
                keywords: vec![],
            };
            for (k, a) in &kws {
                fld.keywords.push(Kw {
                    cond: None,
                    kw: to_keyword(k, a).map_err(|e| anyhow::anyhow!("line {lineno}: {e}"))?,
                });
            }
            rec.fields.push(fld);
            target = Target::Field;
            continue;
        }

        if let Some(pos) = pos {
            // A constant: literal or DATE/TIME/USER/SYSNAME.
            let Some(rec) = f.records.last_mut() else {
                bail!("line {lineno}: constant before any record format")
            };
            let mut text = None;
            let mut keywords = Vec::new();
            for (k, a) in &kws {
                match k.as_str() {
                    "'" => text = Some(ConstText::Literal(a[0].clone())),
                    "DATE" => text = Some(ConstText::Date),
                    "TIME" => text = Some(ConstText::Time),
                    "USER" => text = Some(ConstText::User),
                    "SYSNAME" => text = Some(ConstText::SysName),
                    _ => keywords.push(Kw {
                        cond: None,
                        kw: to_keyword(k, a)?,
                    }),
                }
            }
            let Some(text) = text else {
                bail!("line {lineno}: a constant needs text, DATE, TIME, USER or SYSNAME")
            };
            rec.constants.push(Constant {
                pos,
                text,
                cond: cond.clone(),
                keywords,
            });
            target = Target::Constant;
            continue;
        }

        // A keyword line for the current item.
        for (k, a) in &kws {
            let kw = Kw {
                cond: cond.clone(),
                kw: to_keyword(k, a).map_err(|e| anyhow::anyhow!("line {lineno}: {e}"))?,
            };
            match target {
                Target::File => f.keywords.push(kw),
                Target::Record => f.records.last_mut().unwrap().keywords.push(kw),
                Target::Field => f
                    .records
                    .last_mut()
                    .unwrap()
                    .fields
                    .last_mut()
                    .unwrap()
                    .keywords
                    .push(kw),
                Target::Constant => f
                    .records
                    .last_mut()
                    .unwrap()
                    .constants
                    .last_mut()
                    .unwrap()
                    .keywords
                    .push(kw),
            }
        }
    }
    if f.records.is_empty() {
        bail!("no record formats (R) found");
    }
    check_subfiles(&f)?;
    Ok(f)
}

/// The DDS rules for subfiles that the rest of ibmish relies on: a control
/// record comes right after its subfile record and names it, and has
/// SFLSIZ and SFLPAG; one page of subfile records fits on the display.
fn check_subfiles(f: &DisplayFile) -> Result<()> {
    for (i, r) in f.records.iter().enumerate() {
        if r.is_subfile() {
            let ctl = f.records.get(i + 1);
            if ctl.and_then(|c| c.subfile_control()) != Some(r.name.as_str()) {
                bail!(
                    "subfile record {} must be followed by its control record (SFLCTL({}))",
                    r.name,
                    r.name
                );
            }
            if r.rows().is_empty() {
                bail!("subfile record {} has nothing to display", r.name);
            }
        }
        if let Some(sfl) = r.subfile_control() {
            let prev = if i > 0 { f.records.get(i - 1) } else { None };
            let Some(sflrec) = prev.filter(|p| p.name == sfl && p.is_subfile()) else {
                bail!(
                    "SFLCTL({sfl}) on {}: the subfile record {sfl} must come just before it",
                    r.name
                );
            };
            if !r.has(|k| matches!(k, Keyword::SflSiz(_)))
                || !r.has(|k| matches!(k, Keyword::SflPag(_)))
            {
                bail!("subfile control record {} needs SFLSIZ and SFLPAG", r.name);
            }
            let rows = sflrec.rows();
            let height = (rows[rows.len() - 1] - rows[0] + 1) as u32;
            if rows[0] as u32 + height * r.sflpag() as u32 - 1 > 24 {
                bail!(
                    "SFLPAG({}) on {}: {} lines of {} starting at line {} do not fit on the display",
                    r.sflpag(),
                    r.name,
                    r.sflpag(),
                    sfl,
                    rows[0]
                );
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------- editing

/// Width of a numeric field on the screen.
pub fn display_width(f: &Field) -> u16 {
    let dec = f.decimals.unwrap_or(0);
    match edit_code(f) {
        Some(code) => edit_width(f.length, dec, code),
        // Unedited: digits, decimal point, sign.
        None => f.length + if dec > 0 { 1 } else { 0 } + 1,
    }
}

fn edit_code(f: &Field) -> Option<char> {
    f.keywords.iter().find_map(|k| {
        if let Keyword::EdtCde(c) = k.kw {
            Some(c)
        } else {
            None
        }
    })
}

struct EditRules {
    commas: bool,
    zero_blank: bool,
    sign: bool,
    point: bool,
}

fn rules(code: char) -> Option<EditRules> {
    // IBM edit codes: 1-4 no sign, J-M minus sign after the number;
    // 1, 2, J, K use commas; 2, 4, K, M leave a zero value blank;
    // Z drops the decimal point and sign and leaves zero blank.
    let r = |commas, zero_blank, sign| {
        Some(EditRules {
            commas,
            zero_blank,
            sign,
            point: true,
        })
    };
    match code {
        '1' => r(true, false, false),
        '2' => r(true, true, false),
        '3' => r(false, false, false),
        '4' => r(false, true, false),
        'J' => r(true, false, true),
        'K' => r(true, true, true),
        'L' => r(false, false, true),
        'M' => r(false, true, true),
        'Z' => Some(EditRules {
            commas: false,
            zero_blank: true,
            sign: false,
            point: false,
        }),
        _ => None,
    }
}

fn edit_width(len: u16, dec: u16, code: char) -> u16 {
    let Some(r) = rules(code) else { return len + 2 };
    let int = len - dec;
    let commas = if r.commas && int > 0 {
        (int - 1) / 3
    } else {
        0
    };
    len + commas + if r.point && dec > 0 { 1 } else { 0 } + if r.sign { 1 } else { 0 }
}

/// Split a decimal string into (negative, integer digits, fraction digits).
fn split_number(v: &str, dec: u16) -> (bool, String, String) {
    let v = v.trim();
    let neg = v.starts_with('-') || v.ends_with('-');
    let v = v.trim_matches('-').trim_start_matches('+');
    let (i, f) = v.split_once('.').unwrap_or((v, ""));
    let i = i.trim_start_matches('0').to_string();
    let mut f = f.to_string();
    while f.len() < dec as usize {
        f.push('0');
    }
    f.truncate(dec as usize);
    (neg, i, f)
}

/// Edit a numeric value for output, right-aligned in the field's width.
pub fn edit(value: &str, len: u16, dec: u16, code: Option<char>) -> String {
    let (neg, int, frac) = split_number(value, dec);
    let is_zero = int.is_empty() && frac.chars().all(|c| c == '0');
    let neg = neg && !is_zero;
    let Some(r) = code.and_then(rules) else {
        // No edit code: all digits with leading zeros, decimal point, sign.
        let int_digits = (len - dec) as usize;
        let mut s = format!("{:0>w$}", int, w = int_digits);
        if dec > 0 {
            s.push('.');
            s.push_str(&frac);
        }
        s.push(if neg { '-' } else { ' ' });
        return s;
    };
    let width = edit_width(len, dec, code.unwrap()) as usize;
    if is_zero && r.zero_blank {
        return " ".repeat(width);
    }
    let mut int_part = int.clone();
    if r.commas && int_part.len() > 3 {
        let mut out = String::new();
        for (i, ch) in int_part.chars().enumerate() {
            if i > 0 && (int_part.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(ch);
        }
        int_part = out;
    }
    let mut s = int_part;
    if dec > 0 {
        if r.point {
            s.push('.');
        }
        s.push_str(&frac);
    } else if s.is_empty() {
        s.push('0');
    }
    if !r.point && s.is_empty() {
        s.push('0');
    }
    if r.sign {
        s.push(if neg { '-' } else { ' ' });
    }
    format!("{s:>width$}")
}

/// Validate and normalize numeric input ("1,234.5-" -> "-1234.5").
pub fn parse_numeric_input(text: &str, len: u16, dec: u16) -> Result<String> {
    let t: String = text
        .trim()
        .chars()
        .filter(|c| *c != ',' && *c != ' ')
        .collect();
    if t.is_empty() {
        return Ok("0".into());
    }
    let neg = t.starts_with('-') || t.ends_with('-');
    let body = t.trim_matches('-').trim_start_matches('+');
    let (i, f) = body.split_once('.').unwrap_or((body, ""));
    if i.is_empty() && f.is_empty()
        || !i.chars().all(|c| c.is_ascii_digit())
        || !f.chars().all(|c| c.is_ascii_digit())
        || body.matches('.').count() > 1
    {
        bail!("not a number");
    }
    let i = i.trim_start_matches('0');
    if i.len() > (len - dec) as usize || f.trim_end_matches('0').len() > dec as usize {
        bail!("too many digits");
    }
    let f = f.trim_end_matches('0');
    let mut out = if i.is_empty() {
        "0".to_string()
    } else {
        i.to_string()
    };
    if !f.is_empty() {
        out.push('.');
        out.push_str(f);
    }
    if neg && out != "0" {
        out.insert(0, '-');
    }
    Ok(out)
}

// -------------------------------------------------------------- drawing

/// Things the screen shows that are not program fields.
pub struct Context<'a> {
    pub user: &'a str,
    pub system: &'a str,
    pub date: String,
    pub time: String,
}

fn attribute_for(kws: &[Kw], ind: &[bool; 100], input: bool) -> (u8, bool, bool, bool) {
    // Returns (attribute, protect, position cursor, nondisplay).
    let mut color: Option<u8> = None;
    let (mut hi, mut ri, mut ul, mut nd, mut pr, mut pc) =
        (false, false, input, false, false, false);
    for k in kws.iter().filter(|k| cond_true(&k.cond, ind)) {
        match &k.kw {
            Keyword::DspAtr(a) => {
                for x in a {
                    match x.as_str() {
                        "HI" => hi = true,
                        "RI" => ri = true,
                        "UL" => ul = true,
                        "ND" => nd = true,
                        "PR" => pr = true,
                        "PC" => pc = true,
                        _ => {}
                    }
                }
            }
            Keyword::Color(c) => {
                color = Some(match c.as_str() {
                    "WHT" => attr::HIGH,
                    "RED" => attr::RED,
                    "BLU" => attr::BLUE,
                    "TRQ" => attr::TURQUOISE,
                    "YLW" => attr::YELLOW,
                    "PNK" => attr::PINK,
                    _ => attr::NORMAL,
                })
            }
            _ => {}
        }
    }
    if nd {
        return (attr::NONDISPLAY, pr, pc, true);
    }
    let mut a = color.unwrap_or(if hi { attr::HIGH } else { attr::NORMAL });
    if ri {
        a |= attr::REVERSE_BIT;
    }
    if ul {
        a |= attr::UNDERLINE_BIT;
    }
    (a, pr, pc, false)
}

/// What [`render`] produced: the screen, and how to read it back.
pub struct Rendered {
    pub screen: Screen,
    /// Input-capable fields shown, by name.
    pub inputs: Vec<String>,
}

/// How to draw one record format.
#[derive(Default)]
pub struct DrawOpts<'a> {
    /// The record being read: input-capable fields take input, and its
    /// ERRMSG and SFLMSG messages show. Other records on the screen are
    /// output only.
    pub input: bool,
    /// Added to every line number (subfile records below the first).
    pub row_offset: u8,
    /// Prefix for input field names on the screen (subfile records).
    pub prefix: &'a str,
    /// Field (unprefixed name) to show in error: reverse image, cursor.
    pub error_field: Option<&'a str>,
}

/// What [`draw`] put on the screen.
#[derive(Default)]
pub struct Drawn {
    /// Input fields drawn, with their prefixed names.
    pub inputs: Vec<String>,
    /// Where DSPATR(PC) or an error wants the cursor.
    pub cursor: Option<(u8, u8)>,
    /// ERRMSG or SFLMSG text to show on the message line.
    pub message: Option<String>,
}

/// Draw a record format onto a screen with the program's field values and
/// indicators.
pub fn draw(
    s: &mut Screen,
    record: &Record,
    values: &BTreeMap<String, String>,
    ind: &[bool; 100],
    ctx: &Context,
    o: &DrawOpts,
) -> Drawn {
    let mut out = Drawn::default();
    let off = o.row_offset;
    if o.input {
        for k in &record.keywords {
            if let Keyword::SflMsg { text, .. } = &k.kw {
                if cond_true(&k.cond, ind) && out.message.is_none() {
                    out.message = Some(text.clone());
                }
            }
        }
    }

    for c in record.constants.iter().filter(|c| cond_true(&c.cond, ind)) {
        let text = match &c.text {
            ConstText::Literal(t) => t.clone(),
            ConstText::Date => ctx.date.clone(),
            ConstText::Time => ctx.time.clone(),
            ConstText::User => format!("{:<10}", ctx.user),
            ConstText::SysName => format!("{:<8}", ctx.system),
        };
        let (a, _, _, nd) = attribute_for(&c.keywords, ind, false);
        if !nd {
            s.text(c.pos.0 + off, c.pos.1, a, &text);
        }
    }

    for f in record
        .fields
        .iter()
        .filter(|f| f.usage != Usage::Hidden && cond_true(&f.cond, ind))
    {
        let (row, col) = f.pos.unwrap();
        let row = row + off;
        let value = values.get(&f.name).cloned().unwrap_or_default();
        let mut kws = f.keywords.clone();
        // ERRMSG: show the message, highlight the field, put the cursor there.
        if o.input {
            for k in &f.keywords {
                if let Keyword::ErrMsg { text, .. } = &k.kw {
                    if cond_true(&k.cond, ind) && out.message.is_none() {
                        out.message = Some(text.clone());
                        kws.push(Kw {
                            cond: None,
                            kw: Keyword::DspAtr(vec!["RI".into(), "PC".into()]),
                        });
                    }
                }
            }
        }
        if o.error_field == Some(f.name.as_str()) {
            kws.push(Kw {
                cond: None,
                kw: Keyword::DspAtr(vec!["RI".into(), "PC".into()]),
            });
        }
        let input_capable = o.input && matches!(f.usage, Usage::Input | Usage::Both);
        let (a, protect, pc, _) = attribute_for(&kws, ind, input_capable);
        let shown = if f.usage == Usage::Input {
            String::new()
        } else if f.is_numeric() {
            let dec = f.decimals.unwrap_or(0);
            if input_capable {
                // Show what was typed if it is not a valid number (retry).
                parse_numeric_input(&value, f.length, dec).unwrap_or(value)
            } else {
                edit(&value, f.length, dec, edit_code(f))
            }
        } else {
            value
        };
        let width = if f.is_numeric() {
            display_width(f)
        } else {
            f.length
        };
        if input_capable && !protect {
            let shift = if f.is_numeric() {
                if f.dtype == 'Y' {
                    ffw::NUMERIC_ONLY
                } else {
                    ffw::SIGNED_NUMERIC
                }
            } else {
                ffw::ALPHA
            };
            let lc = kws
                .iter()
                .any(|k| k.kw == Keyword::CheckLc && cond_true(&k.cond, ind));
            let ffw2 = if f.is_numeric() || lc {
                0
            } else {
                ffw::MONOCASE
            };
            let name = format!("{}{}", o.prefix, f.name);
            s.input_field(&name, row, col, width, shift, ffw2, a, &shown);
            out.inputs.push(name);
        } else {
            let shown: String = format!("{shown:<w$}", w = width as usize)
                .chars()
                .take(width as usize)
                .collect();
            s.text(row, col, a, &shown);
        }
        if pc && out.cursor.is_none() {
            out.cursor = Some((row, col));
        }
    }
    out
}

/// Draw one record format on its own screen, as EXFMT of a record without
/// OVERLAY or subfile shows it.
pub fn render(
    file: &DisplayFile,
    record: &Record,
    values: &BTreeMap<String, String>,
    ind: &[bool; 100],
    ctx: &Context,
    message: Option<&str>,
    error_field: Option<&str>,
) -> Rendered {
    let _ = file;
    let mut s = Screen::new();
    let d = draw(
        &mut s,
        record,
        values,
        ind,
        ctx,
        &DrawOpts {
            input: true,
            error_field,
            ..Default::default()
        },
    );
    if let Some(c) = d.cursor {
        s.cursor_at(c.0, c.1);
    }
    if let Some(m) = message.map(str::to_string).or(d.message) {
        s.message(&m);
    }
    Rendered {
        screen: s,
        inputs: d.inputs,
    }
}

// --------------------------------------------------------------- input

/// The result of one EXFMT as the program sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub aid: String,
    /// New values of input-capable fields (empty for CA keys).
    pub values: BTreeMap<String, String>,
    /// Response indicators to set (true) or reset (false).
    pub indicators: BTreeMap<u8, bool>,
    /// Subfile records the operator typed into: new values of their input
    /// fields, by relative record number.
    pub changed: BTreeMap<u16, BTreeMap<String, String>>,
}

/// What to do with the operator's input.
pub enum Outcome {
    Done(Response),
    /// Show the record again with an error message (and field, if any).
    Retry {
        message: String,
        field: Option<String>,
    },
}

fn function_keys(
    file: &DisplayFile,
    record: &Record,
    ind: &[bool; 100],
) -> Vec<(u8, bool, Option<u8>)> {
    record
        .keywords
        .iter()
        .chain(file.keywords.iter())
        .filter(|k| cond_true(&k.cond, ind))
        .filter_map(|k| match k.kw {
            Keyword::FunctionKey {
                key,
                data,
                indicator,
            } => Some((key, data, indicator)),
            _ => None,
        })
        .collect()
}

/// Does this key return the record's input fields to the program? Enter
/// and CF keys do, CA keys do not.
pub fn returns_data(file: &DisplayFile, record: &Record, ind: &[bool; 100], aid: Aid) -> bool {
    match aid {
        Aid::Enter => true,
        Aid::F(n) => function_keys(file, record, ind)
            .iter()
            .any(|(k, data, _)| *k == n && *data),
        _ => false,
    }
}

pub fn handle_input(
    file: &DisplayFile,
    record: &Record,
    ind: &[bool; 100],
    input: &ds::Input,
) -> Outcome {
    let keys = function_keys(file, record, ind);
    let (return_data, pressed): (bool, Option<u8>) = match input.aid {
        Aid::Enter => (true, None),
        Aid::F(n) => match keys.iter().find(|(k, _, _)| *k == n) {
            Some(&(_, data, indicator)) => (data, indicator),
            None => {
                return Outcome::Retry {
                    message: format!("Function key F{n} is not valid on this display."),
                    field: None,
                }
            }
        },
        _ => {
            return Outcome::Retry {
                message: "That key is not valid on this display.".into(),
                field: None,
            }
        }
    };
    // Response indicators: all of this record's keys off, the pressed one on.
    let mut indicators = BTreeMap::new();
    for (_, _, i) in &keys {
        if let Some(i) = i {
            indicators.insert(*i, false);
        }
    }
    for f in &record.fields {
        for k in &f.keywords {
            if let Keyword::ErrMsg {
                indicator: Some(i), ..
            } = k.kw
            {
                indicators.insert(i, false);
            }
        }
    }
    for k in &record.keywords {
        match k.kw {
            Keyword::SflMsg {
                indicator: Some(i), ..
            }
            | Keyword::PageDown(Some(i))
            | Keyword::PageUp(Some(i)) => {
                indicators.insert(i, false);
            }
            _ => {}
        }
    }
    if let Some(i) = pressed {
        indicators.insert(i, true);
    }
    let mut values = BTreeMap::new();
    if return_data {
        for f in record
            .fields
            .iter()
            .filter(|f| matches!(f.usage, Usage::Input | Usage::Both) && cond_true(&f.cond, ind))
        {
            let Some((_, text)) = input.values.iter().find(|(n, _)| n == &f.name) else {
                continue;
            };
            let v = if f.is_numeric() {
                match parse_numeric_input(text, f.length, f.decimals.unwrap_or(0)) {
                    Ok(v) => v,
                    Err(_) => {
                        return Outcome::Retry {
                            message: format!(
                                "Field {} needs a number with at most {} digits ({} decimals).",
                                f.name,
                                f.length,
                                f.decimals.unwrap_or(0)
                            ),
                            field: Some(f.name.clone()),
                        }
                    }
                }
            } else {
                text.clone()
            };
            values.insert(f.name.clone(), v);
        }
    }
    Outcome::Done(Response {
        aid: input.aid.name(),
        values,
        indicators,
        changed: BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const CUSTINQD: &str = include_str!("../../samples/qddssrc/CUSTINQD.dspf");

    #[test]
    fn parses_the_sample_display_file() {
        let f = parse("custinqd", CUSTINQD).unwrap();
        assert_eq!(f.name, "CUSTINQD");
        assert_eq!(
            f.records
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["PROMPT", "DETAIL"]
        );
        assert!(f.keywords.iter().any(|k| k.kw
            == Keyword::FunctionKey {
                key: 3,
                data: false,
                indicator: Some(3)
            }));
        let prompt = &f.records[0];
        let cusnum = prompt.fields.iter().find(|x| x.name == "CUSNUM").unwrap();
        assert_eq!(
            (cusnum.length, cusnum.decimals, cusnum.dtype, &cusnum.usage),
            (5, Some(0), 'Y', &Usage::Both)
        );
        let err = cusnum
            .keywords
            .iter()
            .find(|k| matches!(k.kw, Keyword::ErrMsg { .. }))
            .unwrap();
        assert_eq!(err.cond, Some(vec![vec![(false, 30)]]));
        assert_eq!(
            err.kw,
            Keyword::ErrMsg {
                text: "Customer not found.".into(),
                indicator: Some(30)
            }
        );
        assert!(prompt
            .constants
            .iter()
            .any(|c| c.text == ConstText::Literal("Customer Inquiry".into())));
        let detail = &f.records[1];
        assert!(detail.keywords.iter().any(|k| k.kw
            == Keyword::FunctionKey {
                key: 12,
                data: false,
                indicator: Some(12)
            }));
        let bal = detail.fields.iter().find(|x| x.name == "CUSBAL").unwrap();
        assert_eq!(edit_code(bal), Some('J'));
    }

    #[test]
    fn indicator_conditions() {
        let src = format!("     A          R R1\n     A            F1            10A  O  2  2\n{:<44}ERRMSG('x')\n", "     A  30N31");
        // Keyword lines need a field before them to attach to; test the parser directly.
        let inds = parse_indicators(" 30N31").unwrap();
        assert_eq!(inds, vec![(false, 30), (true, 31)]);
        let mut ind = [false; 100];
        let c = Some(vec![inds]);
        assert!(!cond_true(&c, &ind));
        ind[30] = true;
        assert!(cond_true(&c, &ind));
        ind[31] = true;
        assert!(!cond_true(&c, &ind));
        let f = parse("X", &src).unwrap();
        assert_eq!(
            f.records[0].fields[0].keywords[0].cond,
            Some(vec![vec![(false, 30), (true, 31)]])
        );
    }

    #[test]
    fn edit_codes() {
        // Expected results follow IBM's published edit code table.
        assert_eq!(edit("1234567.89", 9, 2, Some('J')), "1,234,567.89 ");
        assert_eq!(edit("-75.25", 9, 2, Some('J')), "       75.25-");
        assert_eq!(edit("0", 9, 2, Some('J')), "         .00 ");
        assert_eq!(edit("0", 9, 2, Some('K')), "             ");
        assert_eq!(edit("1234.5", 7, 2, Some('3')), " 1234.50");
        assert_eq!(edit("0", 7, 2, Some('4')), "        ");
        assert_eq!(edit("-12", 5, 0, Some('L')), "   12-");
        assert_eq!(edit("1002", 7, 0, Some('Z')), "   1002");
        assert_eq!(edit("0", 5, 0, Some('Z')), "     ");
        assert_eq!(edit("0.5", 5, 2, Some('1')), "   .50");
        // No edit code: leading zeros, decimal point, sign position.
        assert_eq!(edit("-75.25", 9, 2, None), "0000075.25-");
        assert_eq!(edit("3", 5, 0, None), "00003 ");
    }

    #[test]
    fn numeric_input() {
        assert_eq!(parse_numeric_input("  42", 5, 0).unwrap(), "42");
        assert_eq!(parse_numeric_input("1,234.50", 7, 2).unwrap(), "1234.5");
        assert_eq!(parse_numeric_input("12.5-", 5, 2).unwrap(), "-12.5");
        assert_eq!(parse_numeric_input("", 5, 0).unwrap(), "0");
        assert!(
            parse_numeric_input("123456", 5, 0).is_err(),
            "too many digits"
        );
        assert!(
            parse_numeric_input("1.234", 5, 2).is_err(),
            "too many decimals"
        );
        assert!(parse_numeric_input("12a", 5, 0).is_err());
    }

    fn input(aid: Aid, values: &[(&str, &str)]) -> ds::Input {
        ds::Input {
            aid,
            cursor: (1, 1),
            values: values
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            modified: vec![],
        }
    }

    #[test]
    fn function_keys_and_response_indicators() {
        let f = parse("CUSTINQD", CUSTINQD).unwrap();
        let prompt = &f.records[0];
        let mut ind = [false; 100];
        ind[30] = true; // error shown last time
                        // Enter returns the field and resets *IN03 and the ERRMSG indicator.
        let Outcome::Done(r) =
            handle_input(&f, prompt, &ind, &input(Aid::Enter, &[("CUSNUM", "  3")]))
        else {
            panic!()
        };
        assert_eq!(r.values.get("CUSNUM").map(String::as_str), Some("3"));
        assert_eq!(r.indicators.get(&3), Some(&false));
        assert_eq!(r.indicators.get(&30), Some(&false));
        // F3 (CA03) sets *IN03 and returns no data.
        let Outcome::Done(r) =
            handle_input(&f, prompt, &ind, &input(Aid::F(3), &[("CUSNUM", "9")]))
        else {
            panic!()
        };
        assert_eq!(r.indicators.get(&3), Some(&true));
        assert!(r.values.is_empty(), "CA keys do not return data");
        // F5 is not defined.
        assert!(matches!(
            handle_input(&f, prompt, &ind, &input(Aid::F(5), &[])),
            Outcome::Retry { .. }
        ));
        // Bad numbers are rejected with the field named.
        let Outcome::Retry { field, .. } =
            handle_input(&f, prompt, &ind, &input(Aid::Enter, &[("CUSNUM", "12x")]))
        else {
            panic!()
        };
        assert_eq!(field.as_deref(), Some("CUSNUM"));
    }

    #[test]
    fn render_shows_error_message_and_edited_numbers() {
        let f = parse("CUSTINQD", CUSTINQD).unwrap();
        let ctx = Context {
            user: "QSECOFR",
            system: "IBMISH",
            date: "10/15/26".into(),
            time: "12:00:00".into(),
        };
        let mut ind = [false; 100];
        ind[30] = true;
        let r = render(&f, &f.records[0], &BTreeMap::new(), &ind, &ctx, None, None);
        assert_eq!(r.inputs, vec!["CUSNUM"]);
        let st = r.screen.to_stream();
        let text = crate::tn5250::ebcdic::decode(&st);
        assert!(text.contains("Customer not found."));
        let mut v = BTreeMap::new();
        v.insert("CUSBAL".to_string(), "-75.25".to_string());
        let r = render(&f, &f.records[1], &v, &[false; 100], &ctx, None, None);
        let text = crate::tn5250::ebcdic::decode(&r.screen.to_stream());
        assert!(text.contains("75.25-"), "{text}");
        assert!(r.inputs.is_empty());
    }
}
