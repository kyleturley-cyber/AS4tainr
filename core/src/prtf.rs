//! Printer files (*FILE PRTF).
//!
//! Written from IBM's public DDS reference for printer files and the
//! CRTPRTF command description. A printer file has a page size and an
//! overflow line (CRTPRTF PAGESIZE, OVRFLW); with DDS source it also has
//! record formats a program writes:
//!
//! - record formats (`R`), named fields (length, data type A, S or blank,
//!   decimal positions, usage blank/O printed, P not printed), constants
//!   ('text', DATE, TIME, PAGNBR), option indicators (positions 7-16, `N`,
//!   `O` lines for OR);
//! - line (positions 39-41) and position (42-44, or `+n` after the field
//!   before);
//! - keywords: SPACEB, SPACEA, SKIPB, SKIPA (record and field level),
//!   EDTCDE, EDTWRD, DATE, TIME, PAGNBR, TEXT; others (font, CPI, LPI,
//!   UNDERLINE, HIGHLIGHT, BOLD, REF, REFFLD...) are read and do nothing
//!   to the text printed.
//!
//! Printing a record: the record's SKIPB and SPACEB, then the fields in
//! order (a field's own SKIPB or SPACEB ends the line before it; its
//! SPACEA or SKIPA the line after it), then the record's SPACEA and SKIPA.
//! A record without spacing or skipping prints on the line the printer is
//! on.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;

use crate::catalog::validate_name;
use crate::dspf::{cols, cond_true, parse_indicators, split_keywords, Cond};

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.printer_files (
    library     text NOT NULL,
    name        text NOT NULL,
    source      text NOT NULL DEFAULT '',
    page_length int  NOT NULL DEFAULT 66,
    page_width  int  NOT NULL DEFAULT 132,
    overflow    int  NOT NULL DEFAULT 60,
    PRIMARY KEY (library, name)
);
"#;

/// Spacing and skipping, each with its option indicators.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spacing {
    pub space_before: Vec<(Option<Cond>, u32)>,
    pub space_after: Vec<(Option<Cond>, u32)>,
    pub skip_before: Vec<(Option<Cond>, u32)>,
    pub skip_after: Vec<(Option<Cond>, u32)>,
}

impl Spacing {
    fn first(v: &[(Option<Cond>, u32)], ind: &[bool; 100]) -> Option<u32> {
        v.iter().find(|(c, _)| cond_true(c, ind)).map(|(_, n)| *n)
    }

    fn is_empty(&self) -> bool {
        self.space_before.is_empty()
            && self.space_after.is_empty()
            && self.skip_before.is_empty()
            && self.skip_after.is_empty()
    }
}

/// What a field or constant prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    /// A named field: the program's value.
    Field,
    Literal(String),
    /// DATE: the job's date (MMDDYY); with *YY a four-digit year.
    Date {
        four_digit: bool,
    },
    /// TIME: HHMMSS.
    Time,
    /// PAGNBR: the page number (4 digits).
    PageNumber,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PField {
    pub name: String,
    pub content: Content,
    pub length: u16,
    /// 'A' character, 'S' zoned decimal, 'L' date, 'T' time, 'Z'
    /// timestamp.
    pub dtype: char,
    /// A date's or time's format (DATFMT, TIMFMT): `*ISO`, `*MDY`, `*JOB`...
    #[serde(default)]
    pub format: Option<String>,
    pub decimals: Option<u16>,
    /// Usage P: passed to the system, not printed.
    pub printed: bool,
    pub line: Option<u32>,
    /// Absolute position, or `+n` after the field before.
    pub pos: Option<u32>,
    pub relative: Option<u32>,
    pub cond: Option<Cond>,
    pub edit_code: Option<char>,
    pub edit_word: Option<String>,
    pub spacing: Spacing,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PRecord {
    pub name: String,
    pub spacing: Spacing,
    pub fields: Vec<PField>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterFile {
    pub name: String,
    pub records: Vec<PRecord>,
    /// INDARA: indicators in a separate area (the program's INDDS).
    pub indara: bool,
}

impl PrinterFile {
    pub fn record(&self, name: &str) -> Option<&PRecord> {
        self.records.iter().find(|r| r.name == name)
    }
}

enum Target {
    File,
    Record,
    Field,
}

/// Parse printer file DDS.
pub fn parse(name: &str, source: &str) -> Result<PrinterFile> {
    let mut f = PrinterFile {
        name: name.to_ascii_uppercase(),
        ..Default::default()
    };
    // Join continuation lines (a keyword area ending in '-' or '+').
    let mut lines: Vec<String> = Vec::new();
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
            // A constant's position alone on a line, its text (or DATE,
            // TIME...) on the keyword lines after it, as SDA writes them.
            let position_only =
                cols(prev, 7, 38).trim().is_empty() && !cols(prev, 39, 44).trim().is_empty();
            if position_only
                && cols(&line, 7, 7) != "*"
                && cols(&line, 7, 44).trim().is_empty()
                && !cols(&line, 45, usize::MAX).trim().is_empty()
            {
                let w = prev.chars().count();
                if w < 44 {
                    prev.push_str(&" ".repeat(44 - w));
                }
                if !cols(prev, 45, usize::MAX).trim().is_empty() {
                    prev.push(' ');
                }
                prev.push_str(cols(&line, 45, usize::MAX).trim());
                continue;
            }
        }
        lines.push(line);
    }
    let mut target = Target::File;
    let mut pending: Option<Cond> = None;
    for (idx, line) in lines.iter().enumerate() {
        let lineno = idx + 1;
        if line.trim().is_empty() || cols(line, 7, 7) == "*" {
            continue;
        }
        if !cols(line, 6, 6).eq_ignore_ascii_case("A") && !cols(line, 6, 6).trim().is_empty() {
            bail!("line {lineno}: expected form type 'A' in column 6");
        }
        let andor = cols(line, 7, 7).trim().to_ascii_uppercase();
        let inds =
            parse_indicators(cols(line, 8, 16)).map_err(|e| anyhow!("line {lineno}: {e}"))?;
        let name_type = cols(line, 17, 17).trim().to_ascii_uppercase();
        let fname = cols(line, 19, 28).trim().to_ascii_uppercase();
        let length = cols(line, 30, 34).trim();
        let dtype = cols(line, 35, 35).trim().to_ascii_uppercase();
        let decimals = cols(line, 36, 37).trim();
        let usage = cols(line, 38, 38).trim().to_ascii_uppercase();
        let row = cols(line, 39, 41).trim();
        let col = cols(line, 42, 44).trim();
        let kwarea = cols(line, 45, usize::MAX).trim().to_string();
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
        if name_type.is_empty()
            && fname.is_empty()
            && row.is_empty()
            && col.is_empty()
            && kwarea.is_empty()
        {
            pending = cond;
            continue;
        }
        let kws = split_keywords(&kwarea).map_err(|e| anyhow!("line {lineno}: {e}"))?;
        let num = |a: &[String], k: &str| -> Result<u32> {
            a.first()
                .and_then(|x| x.trim().parse().ok())
                .ok_or_else(|| anyhow!("line {lineno}: {k} needs a number"))
        };
        // Spacing keywords go to the spacing given; the rest are returned.
        let apply = |sp: &mut Spacing, k: &str, a: &[String], c: &Option<Cond>| -> Result<bool> {
            let v = match k {
                "SPACEB" => &mut sp.space_before,
                "SPACEA" => &mut sp.space_after,
                "SKIPB" => &mut sp.skip_before,
                "SKIPA" => &mut sp.skip_after,
                _ => return Ok(false),
            };
            let n = num(a, k)?;
            // SPACEx 0-255, SKIPx 1-255 (DDS reference).
            if (k.starts_with("SKIP") && n == 0) || n > 255 {
                bail!("line {lineno}: {k}({n}) is not valid");
            }
            v.push((c.clone(), n));
            Ok(true)
        };
        if name_type == "R" {
            let mut r = PRecord {
                name: fname.clone(),
                ..Default::default()
            };
            for (k, a) in &kws {
                apply(&mut r.spacing, k, a, &cond)?;
            }
            f.records.push(r);
            target = Target::Record;
            continue;
        }
        if !name_type.is_empty() {
            continue;
        }
        let line_no: Option<u32> = if row.is_empty() {
            None
        } else {
            Some(
                row.parse()
                    .map_err(|_| anyhow!("line {lineno}: bad line number '{row}'"))?,
            )
        };
        let (pos, relative) = match col.strip_prefix('+') {
            Some(n) => (
                None,
                Some(
                    n.trim()
                        .parse()
                        .map_err(|_| anyhow!("line {lineno}: bad position '{col}'"))?,
                ),
            ),
            None if col.is_empty() => (None, None),
            None => (
                Some(
                    col.parse()
                        .map_err(|_| anyhow!("line {lineno}: bad position '{col}'"))?,
                ),
                None,
            ),
        };
        let is_constant = fname.is_empty()
            && (pos.is_some()
                || relative.is_some()
                || kws
                    .iter()
                    .any(|(k, _)| matches!(k.as_str(), "'" | "DATE" | "TIME" | "PAGNBR")));
        if !fname.is_empty() || is_constant {
            let rec = f
                .records
                .last_mut()
                .ok_or_else(|| anyhow!("line {lineno}: a field before any record format"))?;
            let mut fld = PField {
                name: fname.clone(),
                content: Content::Field,
                length: 0,
                dtype: 'A',
                decimals: None,
                printed: usage != "P",
                line: line_no,
                pos,
                relative,
                cond: cond.clone(),
                edit_code: None,
                edit_word: None,
                format: None,
                spacing: Spacing::default(),
            };
            // Date, time and timestamp fields (L, T, Z) take their length
            // from their format (DDS for printer files: DATFMT *ISO, *USA,
            // *EUR, *JIS 10, *MDY, *DMY, *YMD 8, *JUL 6, *JOB the job's;
            // times 8; timestamps 26).
            if !fname.is_empty() && matches!(dtype.as_str(), "L" | "T" | "Z") {
                let fmt = kws
                    .iter()
                    .find(|(k, _)| k == if dtype == "T" { "TIMFMT" } else { "DATFMT" })
                    .and_then(|(_, a)| a.first())
                    .map(|a| a.trim().to_ascii_uppercase())
                    .unwrap_or_else(|| "*ISO".into());
                fld.length = match dtype.as_str() {
                    "T" => 8,
                    "Z" => 26,
                    _ => match fmt.as_str() {
                        "*MDY" | "*DMY" | "*YMD" | "*JOB" => 8,
                        "*JUL" => 6,
                        _ => 10,
                    },
                };
                fld.dtype = dtype.chars().next().unwrap_or('L');
                fld.format = Some(fmt);
            } else if !fname.is_empty() {
                fld.length = length
                    .parse()
                    .map_err(|_| anyhow!("line {lineno}: field {fname} needs a length"))?;
                fld.decimals = if decimals.is_empty() {
                    None
                } else {
                    Some(decimals.parse()?)
                };
                fld.dtype = match (dtype.as_str(), fld.decimals) {
                    ("", Some(_)) => 'S',
                    ("", None) => 'A',
                    ("S", _) | ("A", _) => dtype.chars().next().unwrap(),
                    (other, _) => bail!(
                        "line {lineno}: data type {other} is not supported in printer files yet"
                    ),
                };
                if fld.dtype == 'S' && fld.decimals.is_none() {
                    fld.decimals = Some(0);
                }
                if !matches!(usage.as_str(), "" | "O" | "P") {
                    bail!("line {lineno}: usage {usage} is not valid in a printer file");
                }
            }
            for (k, a) in &kws {
                if apply(&mut fld.spacing, k, a, &None)? {
                    continue;
                }
                match k.as_str() {
                    "'" => fld.content = Content::Literal(a[0].clone()),
                    "DATE" if fname.is_empty() => {
                        fld.content = Content::Date {
                            four_digit: a.iter().any(|x| x.eq_ignore_ascii_case("*YY")),
                        }
                    }
                    "TIME" if fname.is_empty() => fld.content = Content::Time,
                    "PAGNBR" if fname.is_empty() => fld.content = Content::PageNumber,
                    "EDTCDE" => {
                        fld.edit_code = a
                            .first()
                            .and_then(|c| c.trim().chars().next())
                            .map(|c| c.to_ascii_uppercase())
                    }
                    "EDTWRD" => fld.edit_word = a.first().cloned(),
                    _ => {}
                }
            }
            if fname.is_empty() && fld.content == Content::Field {
                bail!("line {lineno}: a constant needs text, DATE, TIME or PAGNBR");
            }
            rec.fields.push(fld);
            target = Target::Field;
            continue;
        }
        // A keyword line for the item above.
        for (k, a) in &kws {
            match target {
                Target::File => {
                    if k == "INDARA" {
                        f.indara = true;
                    }
                }
                Target::Record => {
                    let r = f.records.last_mut().unwrap();
                    apply(&mut r.spacing, k, a, &cond)?;
                }
                Target::Field => {
                    let fld = f.records.last_mut().unwrap().fields.last_mut().unwrap();
                    if !apply(&mut fld.spacing, k, a, &cond)? {
                        match k.as_str() {
                            "EDTCDE" => {
                                fld.edit_code = a
                                    .first()
                                    .and_then(|c| c.trim().chars().next())
                                    .map(|c| c.to_ascii_uppercase())
                            }
                            "EDTWRD" => fld.edit_word = a.first().cloned(),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    if f.records.is_empty() {
        bail!("no record formats (R) found");
    }
    Ok(f)
}

/// What printing a record does, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum PrintOp {
    /// Skip to a line (a new page when it is not below the current one).
    Skip(u32),
    Space(u32),
    /// Print text on the current line.
    Text(String),
    /// Go to a line (positions 39-41) to print there.
    Line(u32),
}

/// Lay out a record: `text_of` gives the printed text of a field (its
/// value edited) or of DATE, TIME and PAGNBR.
pub fn layout(
    rec: &PRecord,
    ind: &[bool; 100],
    text_of: &mut dyn FnMut(&PField) -> Result<String>,
) -> Result<Vec<PrintOp>> {
    let mut ops = Vec::new();
    let sp = &rec.spacing;
    if let Some(n) = Spacing::first(&sp.skip_before, ind) {
        ops.push(PrintOp::Skip(n));
    }
    if let Some(n) = Spacing::first(&sp.space_before, ind) {
        ops.push(PrintOp::Space(n));
    }
    let mut cur: Vec<char> = Vec::new();
    let mut has = false;
    let mut prev_end = 0usize;
    let mut current_line: Option<u32> = None;
    let flush = |cur: &mut Vec<char>, has: &mut bool, ops: &mut Vec<PrintOp>| {
        if *has {
            let t: String = cur.iter().collect::<String>().trim_end().to_string();
            ops.push(PrintOp::Text(t));
        }
        cur.clear();
        *has = false;
    };
    for f in &rec.fields {
        if !f.printed || !cond_true(&f.cond, ind) {
            continue;
        }
        if let Some(l) = f.line {
            if current_line != Some(l) {
                flush(&mut cur, &mut has, &mut ops);
                ops.push(PrintOp::Line(l));
                current_line = Some(l);
                prev_end = 0;
            }
        }
        let fs = &f.spacing;
        let skipb = Spacing::first(&fs.skip_before, ind);
        let spaceb = Spacing::first(&fs.space_before, ind);
        if skipb.is_some() || spaceb.is_some() {
            flush(&mut cur, &mut has, &mut ops);
            if let Some(n) = skipb {
                ops.push(PrintOp::Skip(n));
            }
            if let Some(n) = spaceb {
                ops.push(PrintOp::Space(n));
            }
            prev_end = 0;
        }
        let text: Vec<char> = match &f.content {
            Content::Literal(t) => t.chars().collect(),
            _ => text_of(f)?.chars().collect(),
        };
        let start = match (f.pos, f.relative) {
            (Some(p), _) => p.saturating_sub(1) as usize,
            (None, Some(n)) => prev_end + n as usize,
            (None, None) => prev_end,
        };
        if cur.len() < start + text.len() {
            cur.resize(start + text.len(), ' ');
        }
        for (k, c) in text.iter().enumerate() {
            cur[start + k] = *c;
        }
        has = true;
        prev_end = start + text.len();
        let skipa = Spacing::first(&fs.skip_after, ind);
        let spacea = Spacing::first(&fs.space_after, ind);
        if skipa.is_some() || spacea.is_some() {
            flush(&mut cur, &mut has, &mut ops);
            if let Some(n) = spacea {
                ops.push(PrintOp::Space(n));
            }
            if let Some(n) = skipa {
                ops.push(PrintOp::Skip(n));
            }
            prev_end = 0;
        }
    }
    flush(&mut cur, &mut has, &mut ops);
    if let Some(n) = Spacing::first(&sp.space_after, ind) {
        ops.push(PrintOp::Space(n));
    }
    if let Some(n) = Spacing::first(&sp.skip_after, ind) {
        ops.push(PrintOp::Skip(n));
    }
    let _ = Spacing::is_empty;
    Ok(ops)
}

/// A printer file's attributes: page length and width, overflow line.
#[derive(Debug, Clone, PartialEq)]
pub struct Attributes {
    pub library: String,
    pub page_length: i32,
    pub page_width: i32,
    pub overflow: i32,
    pub source: String,
}

/// CRTPRTF.
pub async fn create(
    client: &Client,
    library: &str,
    name: &str,
    source: &str,
    attrs: (i32, i32, i32),
    text: &str,
    replace: bool,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    if !source.trim().is_empty() {
        parse(&name, source)?;
    }
    let (len, width, ovf) = attrs;
    if !(1..=255).contains(&len) || !(1..=378).contains(&width) {
        bail!("PAGESIZE({len} {width}) is not valid");
    }
    if ovf < 1 || ovf > len {
        bail!("OVRFLW({ovf}) must be from 1 to the page length ({len})");
    }
    let tx = client;
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} does not exist.");
    }
    let existing = tx
        .query_opt(
            "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?;
    match existing {
        Some(r) if replace && r.get::<_, &str>(0) == "PRTF" => {
            tx.execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                &[&lib, &name],
            )
            .await?;
        }
        Some(_) => return Err(crate::catalog::file_exists_error(&name, &lib)),
        None => {}
    }
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','PRTF',$3)",
        &[&lib, &name, &text],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.printer_files (library, name, source, page_length, page_width, overflow)
         VALUES ($1,$2,$3,$4,$5,$6)
         ON CONFLICT (library, name) DO UPDATE SET source=EXCLUDED.source,
           page_length=EXCLUDED.page_length, page_width=EXCLUDED.page_width, overflow=EXCLUDED.overflow",
        &[&lib, &name, &source, &len, &width, &ovf],
    )
    .await?;
    Ok(())
}

/// A printer file through the library list (or its library).
pub async fn resolve(client: &Client, libl: &[String], name: &str) -> Result<Option<Attributes>> {
    let (libs, name) = match name.split_once('/') {
        Some((l, n)) => (vec![crate::qtemp::real(l)], n.to_string()),
        None => (libl.to_vec(), name.to_string()),
    };
    for lib in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT source, page_length, page_width, overflow FROM as4tainr.printer_files
                 WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            return Ok(Some(Attributes {
                library: lib.clone(),
                source: r.get(0),
                page_length: r.get(1),
                page_width: r.get(2),
                overflow: r.get(3),
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // DDS reference for printer files: SKIPB and SPACEB before the
    // record, SPACEA after it; a field's SPACEB starts a new line; '+n'
    // places a field n positions after the one before; option indicators
    // condition fields.
    // As SDA writes them (IBM's tobi-example ORD500O): a constant's
    // position alone on its line and its text and keywords on the lines
    // after it (DDS: a field's keywords may go on following lines with
    // positions 7-44 blank); a date field (L) with DATFMT(*JOB) is the
    // job's format, 8 characters for *MDY, *DMY and *YMD (DDS reference
    // for printer files, DATFMT).
    #[test]
    fn constants_with_text_on_following_lines_and_date_fields() {
        let src = concat!(
            "     A          R HEADER\n",
            "     A                                      SKIPB(005)\n",
            "     A                                     3\n",
            "     A                                      'Company Sample'\n",
            "     A                                    66\n",
            "     A                                      SPACEB(001)\n",
            "     A                                      'Net'\n",
            "     A            DATORD          L       79DATFMT(*JOB)\n",
        );
        let f = parse("X", src).unwrap();
        let r = f.record("HEADER").unwrap();
        assert_eq!(
            r.fields[0].content,
            Content::Literal("Company Sample".into())
        );
        assert_eq!(r.fields[0].pos, Some(3));
        assert_eq!(r.fields[1].content, Content::Literal("Net".into()));
        assert_eq!(r.fields[1].spacing.space_before, vec![(None, 1)]);
        assert_eq!((r.fields[2].dtype, r.fields[2].length), ('L', 8));
        assert_eq!(r.fields[2].format.as_deref(), Some("*JOB"));
    }

    const SRC: &str = concat!(
        "     A                                      INDARA\n",
        "     A          R HEAD                      SKIPB(3) SPACEA(2)\n",
        "     A                                     2'Customer list'\n",
        "     A                                    40PAGNBR\n",
        "     A          R DETAIL                    SPACEA(1)\n",
        "     A            NAME          10A        2\n",
        "     A            BAL            7  2     +2EDTCDE(J)\n",
        "     A  50                                +1'*'\n",
        "     A            NOTE          10         2SPACEB(1)\n",
    );

    #[test]
    fn layout_of_records() {
        let f = parse("RPT", SRC).unwrap();
        assert!(f.indara);
        let mut ind = [false; 100];
        let mut text = |fld: &PField| -> Result<String> {
            Ok(match (&fld.content, fld.name.as_str()) {
                (Content::PageNumber, _) => "   1".into(),
                (_, "NAME") => "Acme      ".into(),
                (_, "BAL") => "   12.50 ".into(),
                (_, "NOTE") => "late".into(),
                _ => String::new(),
            })
        };
        let ops = layout(f.record("HEAD").unwrap(), &ind, &mut text).unwrap();
        assert_eq!(
            ops,
            [
                PrintOp::Skip(3),
                PrintOp::Text(format!(" Customer list{}   1", " ".repeat(25))),
                PrintOp::Space(2)
            ]
        );
        ind[50] = true;
        let ops = layout(f.record("DETAIL").unwrap(), &ind, &mut text).unwrap();
        assert_eq!(
            ops,
            [
                // NAME in 2-11, two blanks, BAL in 14-22, one blank, '*'.
                PrintOp::Text(format!(" Acme{}12.50  *", " ".repeat(11))),
                PrintOp::Space(1),
                PrintOp::Text(" late".into()),
                PrintOp::Space(1)
            ]
        );
    }
}
