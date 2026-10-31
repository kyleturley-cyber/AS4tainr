//! The DDS listing CRTPF and CRTLF print (QSYSPRT): the file's
//! attributes, the source by sequence number with markers for the
//! messages about a line, the expanded source (physical file fields with
//! their attributes, lengths and buffer positions), the messages, and the
//! message summary with the command's own messages.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-27,
//! tools/golden/verify5.txt z_crtpf, z_crtpf_dup, z_crtlf, z_crtpf_bad).
//! The product line and the compiler's name are as4tainr's (report.rs);
//! the message texts are as4tainr's own.

use crate::report::{at, Stamp};

/// A message about the DDS: its ID (none where no capture shows one),
/// severity, the source line (1-based; 0: none) and the position in it
/// the marker points at.
#[derive(Debug, Clone)]
pub struct Msg {
    pub id: Option<String>,
    pub sev: u32,
    pub line: usize,
    pub pos: usize,
    pub text: String,
}

/// An error found in DDS source, with what the listing shows about it.
#[derive(Debug, Clone)]
pub struct DdsError(pub Msg);

impl std::fmt::Display for DdsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.0.line, self.0.text)
    }
}

impl std::error::Error for DdsError {}

/// The message a DDS parse error makes: a `DdsError` as it is, otherwise
/// the line from "line N: " and no ID, severity 30.
pub fn message_of(e: &anyhow::Error) -> Msg {
    if let Some(d) = e.downcast_ref::<DdsError>() {
        return d.0.clone();
    }
    let s = format!("{e:#}");
    let (line, text) = match s
        .strip_prefix("line ")
        .and_then(|r| r.split_once(": "))
        .and_then(|(n, t)| Some((n.parse::<usize>().ok()?, t.to_string())))
    {
        Some((n, t)) => (n, t),
        None => (0, s.clone()),
    };
    Msg {
        id: None,
        sev: 30,
        line,
        pos: 0,
        text,
    }
}

/// What the first page shows.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub name: String,
    pub library: String,
    /// "Physical" or "Logical".
    pub attribute: String,
    pub srcfile: (String, String),
    pub member: String,
    pub changed: Option<Stamp>,
    pub options: Vec<String>,
    pub genlvl: String,
    pub flag: String,
    pub filetype: String,
    pub aut: String,
    pub replace: String,
    pub text: String,
    pub lines: Vec<String>,
    /// Whether the expanded source shows physical file fields with their
    /// attributes (CRTPF).
    pub physical: bool,
    /// A display file (CRTDSPF): its source after reference fields were
    /// expanded (ddsref.rs: attributes made explicit, copied keywords
    /// after the field's own, an `A*@REF` comment after each reference
    /// field); the expanded source is made from it.
    pub display: Option<String>,
    /// A physical file (CRTPF): its source after reference fields were
    /// expanded (ddsref.rs, line for line: attributes made explicit,
    /// copied keywords and `@REF(...)` after the entry's own).
    pub refs: Option<String>,
}

/// Lines on a page after its header (PUB400 2026-10-30, verify29
/// v29_crtp: the expanded source's second page holds 50).
const PAGE: usize = 50;

/// The sequence number a source line is listed with (1-based line n:
/// n * 100, as records 1.00, 2.00 ... are).
pub fn seq(line: usize) -> usize {
    line * 100
}

fn col(line: &str, from: usize, to: usize) -> &str {
    let start = from - 1;
    if line.len() <= start {
        return "";
    }
    line.get(start..to.min(line.len())).unwrap_or("").trim()
}

/// A physical file field line on its own (a record format with that one
/// field), or None when it is not a field or has an error.
fn field_of(line: &str) -> Option<crate::dds::FieldDef> {
    let src = format!("{:5}A{:10}R X\n{line}\n", "", "");
    crate::dds::parse_pf("X", &src)
        .ok()
        .and_then(|pf| pf.fields.into_iter().next())
}

/// The source line with positions 1-7 (sequence number area, form type,
/// comment position) blank, as the expanded source shows it.
fn blanked(line: &str) -> String {
    let rest: String = line.chars().skip(7).collect();
    format!("{:7}{rest}", "").trim_end().to_string()
}

/// The expanded source's lines (without the sequence numbers' column
/// header): record, key and select/omit lines as written, physical file
/// fields with their type made explicit, usage B, a default column
/// heading, their length in bytes and buffer positions.
fn expanded(input: &Input, errors: &[usize]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 1u32;
    let refs: Vec<String> = input
        .refs
        .as_deref()
        .map(|r| r.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let mut in_record = false;
    let mut record = String::new();
    let mut i = 0;
    while i < input.lines.len() {
        let n = i + 1;
        let line = input.lines[i].trim_end();
        i += 1;
        if line.trim().is_empty() || col(line, 7, 7) == "*" || errors.contains(&n) {
            continue;
        }
        let name_type = col(line, 17, 17).to_ascii_uppercase();
        let name = col(line, 19, 28).to_ascii_uppercase();
        let s = seq(n);
        if name_type == "R" {
            in_record = true;
            record = name.clone();
        }
        // The file-level reference file is not shown (v29_crtp).
        if !in_record && only_ref_keywords(line) {
            continue;
        }
        let is_field = input.physical && name_type.is_empty() && !name.is_empty();
        if is_field && col(line, 29, 29).eq_ignore_ascii_case("R") {
            // The entry: this line and its keyword lines.
            let mut last = n - 1;
            while last + 1 < input.lines.len() && keyword_only(&input.lines[last + 1]) {
                last += 1;
            }
            i = last + 1;
            let Some(exp) = refs.get(n - 1) else { continue };
            let lines = ReferencedEntry {
                seq: s,
                original: &input.lines[n - 1..=last],
                expanded: &refs[n - 1..=last.min(refs.len() - 1)],
                record: &record,
                pos,
            };
            let (printed, bytes) = lines.listed(exp);
            out.extend(printed);
            pos += bytes;
            continue;
        }
        if is_field {
            let Some(f) = field_of(line) else { continue };
            // The entry's own keywords: this line's and its keyword lines'
            // (v29_crtr: COLHDG on the next line shown on the field's).
            let mut last = n - 1;
            while last + 1 < input.lines.len() && keyword_only(&input.lines[last + 1]) {
                last += 1;
            }
            i = last + 1;
            let own: Vec<String> = input.lines[n - 1..=last]
                .iter()
                .flat_map(|l| crate::dds::split_keywords(&l.chars().skip(44).collect::<String>()))
                .collect();
            let len = match f.data_type.fixed_length() {
                Some(l) => l.to_string(),
                None => f.length.to_string(),
            };
            let dec = if f.data_type.is_numeric() {
                f.decimals.to_string()
            } else {
                String::new()
            };
            // No heading of its own: the field's name; on the field's line
            // when it has no keywords (z_crtpf), else on a line of its own
            // without a number (v29_crtr, w30_crtr).
            let default_hdg = (!own.iter().any(|k| crate::dds::keyword_name(k) == "COLHDG"))
                .then(|| format!("COLHDG('{}')", f.name));
            let mut kws = own.clone();
            let mut after = Vec::new();
            match default_hdg {
                Some(h) if own.is_empty() => kws.push(h),
                Some(h) => after.push(h),
                None => {}
            }
            let packed = keyword_lines("", &kws);
            let first = packed
                .first()
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            let spec = format!(
                "{:18}{:<10} {len:>5}{}{dec:>2}B{:6}{first}",
                "",
                f.name,
                f.data_type.code(),
                ""
            );
            let bytes = f.buffer_bytes();
            out.push(format!("{s:>7}  {spec:<80}{bytes:>6}{pos:>13}{pos:>8}"));
            for l in packed.iter().skip(1) {
                out.push(format!("{s:>7}{}", &l[7..]));
            }
            out.extend(keyword_lines("", &after));
            pos += bytes;
        } else {
            out.push(format!("{s:>7}  {}", blanked(line)).trim_end().to_string());
        }
    }
    out
}

/// A line whose keywords are only REF or REFFLD (not shown expanded).
fn only_ref_keywords(line: &str) -> bool {
    let kws = crate::dds::split_keywords(&line.chars().skip(44).collect::<String>());
    !kws.is_empty()
        && col(line, 17, 44).is_empty()
        && kws
            .iter()
            .all(|k| matches!(crate::dds::keyword_name(k).as_str(), "REF" | "REFFLD"))
}

/// A line with keywords only (positions 17-44 blank).
fn keyword_only(line: &str) -> bool {
    col(line, 7, 7) != "*" && col(line, 17, 44).is_empty() && !col(line, 45, 80).is_empty()
}

/// A physical file's reference field as its expanded source shows it
/// (PUB400 2026-10-30, verify29 v29_crtp): the field line with its
/// attributes, usage B, length and buffer position; where it came from
/// (*SRC: this record format, file *SRC, no library); its own keyword
/// lines as written (REFFLD not shown); the keywords taken from the
/// referenced field (a file field's TEXT padded to 50, the default
/// COLHDG) under the field's number; a TEXT made from its own COLHDG
/// without a number.
struct ReferencedEntry<'a> {
    seq: usize,
    original: &'a [String],
    expanded: &'a [String],
    record: &'a str,
    pos: u32,
}

impl ReferencedEntry<'_> {
    fn listed(&self, exp: &str) -> (Vec<String>, u32) {
        let s = self.seq;
        let kw = |l: &str| l.chars().skip(44).collect::<String>().trim().to_string();
        let own_last = self.original.last().map(|l| kw(l)).unwrap_or_default();
        let exp_last = self.expanded.last().map(|l| kw(l)).unwrap_or_default();
        let added = exp_last
            .strip_prefix(own_last.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut origin: Vec<String> = Vec::new();
        let mut copied = Vec::new();
        for k in crate::dds::split_keywords(&added) {
            if crate::dds::keyword_name(&k) == "@REF" {
                let inner = k.split_once('(').map(|x| x.1).unwrap_or("");
                let inner = inner.strip_suffix(')').unwrap_or(inner);
                origin = inner.split_whitespace().map(str::to_string).collect();
            } else {
                copied.push(k);
            }
        }
        let own: Vec<String> = self
            .original
            .iter()
            .flat_map(|l| crate::dds::split_keywords(&kw(l)))
            .collect();
        let has = |n: &str| own.iter().any(|k| crate::dds::keyword_name(k) == n);
        // The field line, from the expanded source's positions 8-37.
        let ty = col(exp, 35, 35).to_ascii_uppercase();
        let dec = col(exp, 36, 37).to_string();
        let dtype = match ty.chars().next() {
            Some(c) => c,
            None if !dec.is_empty() => 'P',
            None => 'A',
        };
        let len: String = match dtype {
            'L' => "10".into(),
            'T' => "8".into(),
            'Z' => "26".into(),
            _ => col(exp, 30, 34).to_string(),
        };
        let attrs: String = exp.chars().take(37).collect();
        let bytes = field_of(&attrs).map(|f| f.buffer_bytes()).unwrap_or(0);
        let head: String = exp.chars().skip(7).take(21).collect();
        let spec = format!("{:7}{head:<21} {len:>5}{dtype}{dec:>2}B", "");
        let p = self.pos;
        let mut out = vec![format!("{s:>7}  {spec:<80}{bytes:>6}{p:>13}{p:>8}")
            .trim_end()
            .to_string()];
        let src = origin.get(2).is_some_and(|f| f == "*SRC");
        if origin.len() >= 4 {
            out.push(format!(
                " * Referenced Field  . . . . . . . . . . . :   {}",
                origin[3]
            ));
            let (fmt, file, lib) = if src {
                (self.record.to_string(), "*SRC".to_string(), String::new())
            } else {
                (origin[2].clone(), origin[1].clone(), origin[0].clone())
            };
            out.push(format!(
                " *   Format  . . . . . . . . . . . . . . . :   {fmt}"
            ));
            out.push(format!(
                " *   File  . . . . . . . . . . . . . . . . :   {file}"
            ));
            out.push(
                format!(" *   Library . . . . . . . . . . . . . . . :   {lib}")
                    .trim_end()
                    .to_string(),
            );
        }
        // Own keyword lines after the field line, as written.
        for (k, l) in self.original.iter().enumerate().skip(1) {
            if !only_ref_keywords(l) {
                out.push(
                    format!("{:>7}  {}", s + k * 100, blanked(l))
                        .trim_end()
                        .to_string(),
                );
            }
        }
        // Own keywords on the field line (but REFFLD).
        let mut numbered: Vec<String> = crate::dds::split_keywords(&kw(&self.original[0]))
            .into_iter()
            .filter(|k| crate::dds::keyword_name(k) != "REFFLD")
            .collect();
        let mut unnumbered = Vec::new();
        for k in copied {
            if crate::dds::keyword_name(&k) == "TEXT" {
                if has("COLHDG") && !has("TEXT") {
                    unnumbered.push(k);
                    continue;
                }
                if !src {
                    let inner = k.split_once('(').map(|x| x.1).unwrap_or("");
                    let inner = inner.strip_suffix(')').unwrap_or(inner);
                    let t = inner.trim().trim_matches('\'');
                    numbered.push(format!("TEXT('{t:<50}')"));
                    continue;
                }
            }
            numbered.push(k);
        }
        out.extend(keyword_lines(&format!("{s:>7}"), &numbered));
        out.extend(keyword_lines("", &unnumbered));
        (out, bytes)
    }
}

/// Keywords as an expanded listing shows them at positions 45-80: as many
/// as fit on a line, '+' before the next line; a keyword too long for
/// the line goes on in pieces of 35 with '-' (PUB400 2026-10-30, verify29
/// v29_crtp and v29_crtd, verify30 w30_crtd).
fn keyword_lines(number: &str, keywords: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for k in keywords {
        let cand = if cur.is_empty() {
            k.clone()
        } else {
            format!("{cur} {k}")
        };
        if cand.chars().count() <= 36 {
            cur = cand;
            continue;
        }
        let mut rest = if cur.is_empty() {
            cand
        } else {
            lines.push(format!("{cur} +"));
            k.clone()
        };
        while rest.chars().count() > 36 {
            let head: String = rest.chars().take(35).collect();
            lines.push(format!("{head}-"));
            rest = rest.chars().skip(35).collect();
        }
        cur = rest;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
        .into_iter()
        .map(|l| format!("{number:>7}{:46}{l}", "").trim_end().to_string())
        .collect()
}

/// A display file's expanded source (PUB400 2026-10-30, verify29 v29_crtd
/// and verify30 w30_crtd): DSPSIZ(*DS3) when the source names none; the
/// file-level REF is not shown; record lines as written; a constant with
/// its length; a field with its length, type and decimals made explicit
/// (a date 10L), its length in bytes and its output and input buffer
/// positions (output: O, B, H fields; input: I, B, H; each record from 1);
/// a reference field's origin (referenced field, format, file, library)
/// on comment lines; then the field's keywords (a referenced TEXT padded
/// to 50 characters) in pieces at position 45.
fn display_expanded(input: &Input, expanded_src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = expanded_src.lines().collect();
    let own_lines: Vec<String> = input
        .lines
        .iter()
        .map(|l| l.trim_end().to_string())
        .collect();
    let has_dspsiz = expanded_src.to_ascii_uppercase().contains("DSPSIZ(");
    if !has_dspsiz {
        out.push(format!("{:53}DSPSIZ(*DS3)", ""));
    }
    let mut n = 0usize; // the original line number
    let mut in_record = false;
    let (mut outpos, mut inpos) = (1u32, 1u32);
    // The field being listed: its line, keywords and origin.
    struct Pending {
        seq: usize,
        line: String,
        keywords: String,
        origin: Option<Vec<String>>,
    }
    let mut pending: Option<Pending> = None;
    let flush = |p: Pending, out: &mut Vec<String>| {
        out.push(p.line);
        if let Some(o) = &p.origin {
            out.push(format!(
                " * Referenced Field  . . . . . . . . . . . :   {}",
                o[3]
            ));
            if o.len() >= 4 && o[2] != "*SRC" {
                out.push(format!(
                    " *   Format  . . . . . . . . . . . . . . . :   {}",
                    o[2]
                ));
                out.push(format!(
                    " *   File  . . . . . . . . . . . . . . . . :   {}",
                    o[1]
                ));
                out.push(format!(
                    " *   Library . . . . . . . . . . . . . . . :   {}",
                    o[0]
                ));
            }
        }
        if !p.keywords.trim().is_empty() {
            let kws = crate::dds::split_keywords(p.keywords.trim());
            out.extend(keyword_lines(&format!("{:>7}", p.seq), &kws));
        }
    };
    for raw in &lines {
        let line = raw.trim_end();
        if let Some(r) = line.trim_start().strip_prefix("A*@REF ") {
            if let Some(p) = pending.as_mut() {
                p.origin = Some(r.split_whitespace().map(str::to_string).collect());
            }
            continue;
        }
        n += 1;
        if line.trim().is_empty() || col(line, 7, 7) == "*" {
            continue;
        }
        let s = seq(n);
        let own = own_lines.get(n - 1).cloned().unwrap_or_default();
        let kw_of = |l: &str| l.chars().skip(44).collect::<String>().trim().to_string();
        let name_type = col(line, 17, 17).to_ascii_uppercase();
        let name = col(line, 19, 28).to_ascii_uppercase();
        let position = col(line, 39, 44);
        if name_type == "R" {
            if let Some(p) = pending.take() {
                flush(p, &mut out);
            }
            in_record = true;
            (outpos, inpos) = (1, 1);
            out.push(format!("{s:>7}  {}", blanked(line)).trim_end().to_string());
            continue;
        }
        if !in_record {
            // File-level keywords; the reference file is not shown.
            let kws = crate::dds::split_keywords(&kw_of(line));
            if kws
                .iter()
                .all(|k| matches!(crate::dds::keyword_name(k).as_str(), "REF" | "REFFLD"))
            {
                continue;
            }
            out.push(format!("{s:>7}  {}", blanked(line)).trim_end().to_string());
            continue;
        }
        if !name.is_empty() && name_type.is_empty() {
            if let Some(p) = pending.take() {
                flush(p, &mut out);
            }
            let ty = col(line, 35, 35).to_ascii_uppercase();
            let dec_txt = col(line, 36, 37).to_string();
            let dtype = match ty.chars().next() {
                Some(c) => c,
                None if !dec_txt.is_empty() => 'S',
                None => 'A',
            };
            let len: u32 = match dtype {
                'L' => 10,
                'T' => 8,
                'Z' => 26,
                _ => col(line, 30, 34).parse().unwrap_or(0),
            };
            let usage = col(line, 38, 38).to_ascii_uppercase();
            let usage = if usage.is_empty() {
                "B".to_string()
            } else {
                usage
            };
            let head: String = line.chars().skip(7).take(21).collect();
            let spec = format!(
                "{:7}{head:<21} {len:>5}{dtype}{dec_txt:>2}{usage:1}{:>3}{:>3}",
                "",
                col(line, 39, 41),
                col(line, 42, 44)
            );
            let (o, i) = match usage.as_str() {
                "O" => (Some(outpos), None),
                "I" => (None, Some(inpos)),
                _ => (Some(outpos), Some(inpos)),
            };
            if o.is_some() {
                outpos += len;
            }
            if i.is_some() {
                inpos += len;
            }
            let o = o.map(|v| v.to_string()).unwrap_or_default();
            let i = i.map(|v| v.to_string()).unwrap_or_default();
            // Keywords copied from the referenced field follow the
            // field's own on the same line here; a copied TEXT is shown
            // padded to 50.
            let all = kw_of(line);
            let mine = kw_of(&own);
            let copied = all
                .strip_prefix(mine.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let copied: Vec<String> = crate::dds::split_keywords(&copied)
                .into_iter()
                .map(|k| {
                    if crate::dds::keyword_name(&k) == "TEXT" {
                        let inner = k.split_once('(').map(|x| x.1).unwrap_or("");
                        let inner = inner.strip_suffix(')').unwrap_or(inner);
                        let t = inner.trim().trim_matches('\'');
                        format!("TEXT('{t:<50}')")
                    } else {
                        k
                    }
                })
                .collect();
            let keywords = format!("{mine} {}", copied.join(" ")).trim().to_string();
            pending = Some(Pending {
                seq: s,
                line: format!("{s:>7}  {spec:<80}{len:>6}{o:>13}{i:>8}")
                    .trim_end()
                    .to_string(),
                keywords,
                origin: None,
            });
            continue;
        }
        if name.is_empty() && !position.is_empty() {
            // A constant: its length.
            if let Some(p) = pending.take() {
                flush(p, &mut out);
            }
            let kw = kw_of(line);
            let len = kw
                .strip_prefix('\'')
                .and_then(|r| {
                    r.rfind('\'')
                        .map(|e| r[..e].replace("''", "'").chars().count())
                })
                .map(|n| n.to_string())
                .unwrap_or_default();
            let spec = blanked(line);
            out.push(format!("{s:>7}  {spec:<80}{len:>6}").trim_end().to_string());
            continue;
        }
        // A keyword line: the field's, or the record's.
        match pending.as_mut() {
            Some(p) => {
                p.keywords = format!("{} {}", p.keywords, kw_of(line)).trim().to_string();
            }
            None => out.push(format!("{s:>7}  {}", blanked(line)).trim_end().to_string()),
        }
    }
    if let Some(p) = pending.take() {
        flush(p, &mut out);
    }
    out
}

/// The listing's lines. `finals`: the command's messages (ID, severity,
/// text) the summary page ends with; `created`: whether the file was
/// created (the messages page is printed when it was not).
pub fn listing(
    input: &Input,
    msgs: &[Msg],
    finals: &[(String, u32, String)],
    created: bool,
    stamp: &Stamp,
) -> Vec<String> {
    let opt = |o: &str| input.options.iter().any(|x| x.eq_ignore_ascii_case(o));
    let source = !opt("*NOSRC") && !opt("*NOSOURCE");
    let list = !opt("*NOLIST");
    if !source && !list {
        // z_crtpf_nosrc: OPTION(*NOSRC *NOLIST) prints nothing.
        return Vec::new();
    }
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (42, "Data Description"),
            (73, &format!("{}/{}", input.library, input.name)),
            (97, &format!("{} {}", stamp.date, stamp.time)),
            (122, &format!("Page {page:>4}")),
        ])
    };
    let label = |indent: usize, text: &str, value: &str| {
        // The colon in column 54, the value in column 58 (as captured).
        let l = crate::commands::dsp_label(text, 52 - indent);
        format!("{}{l}   {value}", " ".repeat(indent - 1))
            .trim_end()
            .to_string()
    };
    // Sections, each with the lines repeated at the top of its pages.
    let mut sections: Vec<(usize, Vec<String>)> = Vec::new();
    let mut first = vec![
        label(2, "File name", &input.name),
        label(4, "Library name", &input.library),
        label(2, "File attribute", &input.attribute),
        label(2, "Source file containing DDS", &input.srcfile.1),
        label(4, "Library name", &input.srcfile.0),
        label(2, "Source member containing DDS", &input.member),
        label(
            2,
            "Source member last changed",
            &input
                .changed
                .as_ref()
                .map(|c| format!("{}  {}", c.date, c.time))
                .unwrap_or_default(),
        ),
        label(
            2,
            "Source listing options",
            &input
                .options
                .iter()
                .map(|o| format!("{o:<11}"))
                .collect::<String>(),
        ),
        label(2, "DDS generation severity level", &input.genlvl),
        label(2, "DDS flagging severity level", &input.flag),
        label(2, "File type", &input.filetype),
        label(2, "Authority", &input.aut),
        label(2, "Replace file", &input.replace),
        label(2, "Text", &input.text),
        label(2, "Compiler", "as4tainr Data Description Processor"),
    ];
    if input.display.is_some() {
        // CRTDSPF's first page has no file type (w30_crtd).
        first.retain(|l| !l.starts_with(" File type "));
    }
    if source {
        first.push(format!("{:38}Data Description Source", ""));
        first.push(
            " SEQNBR  *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8  Date"
                .into(),
        );
        for (i, l) in input.lines.iter().enumerate() {
            first.push(
                format!("{:>7}  {}", seq(i + 1), l.trim_end())
                    .trim_end()
                    .to_string(),
            );
            // The marker: the message ID and -* ending at the position
            // the message is about (z_crtpf_bad: CPD7419 at position 35).
            for m in msgs.iter().filter(|m| m.line == i + 1) {
                let tag = format!("{}-*", m.id.as_deref().unwrap_or(""));
                let width = (8 + m.pos.max(1)).max(tag.len() + 1);
                first.push(format!("*{tag:>width$}"));
            }
        }
        first.push(format!(
            "{:25}* * * * *   E N D   O F   S O U R C E   * * * * *",
            ""
        ));
    }
    sections.push((0, first));
    if list {
        let errors: Vec<usize> = msgs.iter().map(|m| m.line).collect();
        let mut exp = vec![
            format!("{:42}Expanded Source", ""),
            format!("{:90}Field       Buffer position", ""),
            " SEQNBR  *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8 length        Out     In".into(),
        ];
        match &input.display {
            Some(d) => exp.extend(display_expanded(input, d)),
            None => exp.extend(expanded(input, &errors)),
        }
        exp.push(format!(
            "{:17}* * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *",
            ""
        ));
        // Its three heading lines start each of its pages (v29_crtp).
        sections.push((3, exp));
    }
    let row = |id: &str, sev: u32, n: &str, text: &str| {
        format!(
            "* {id:<7}{:>8}{n:>9}      Message . . . . :   {text}",
            format!("{sev:02}")
        )
    };
    if !created || !msgs.is_empty() {
        let mut page = vec![
            format!("{:45}Messages", ""),
            "    ID      Severity  Number".to_string(),
        ];
        let mut seen: Vec<(Option<String>, String)> = Vec::new();
        for m in msgs {
            let key = (
                m.id.clone(),
                if m.id.is_some() {
                    String::new()
                } else {
                    m.text.clone()
                },
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let n = msgs
                .iter()
                .filter(|x| x.id == m.id && (m.id.is_some() || x.text == m.text))
                .count();
            page.push(row(
                m.id.as_deref().unwrap_or(""),
                m.sev,
                &n.to_string(),
                &m.text,
            ));
        }
        sections.push((0, page));
    }
    let count = |lo: u32, hi: u32| msgs.iter().filter(|m| (lo..=hi).contains(&m.sev)).count();
    let mut summary = vec![
        format!("{:42}Message Summary", ""),
        "    Total          Informational          Warning        Error       Severe".into(),
        "                       (0-9)              (10-19)       (20-29)      (30-99)".into(),
        format!(
            "{:>9}{:>18}{:>21}{:>14}{:>13}",
            msgs.len(),
            count(0, 9),
            count(10, 19),
            count(20, 29),
            count(30, 99)
        ),
    ];
    for (id, sev, text) in finals {
        summary.push(row(id, *sev, "", text));
    }
    summary.push(format!(
        "{:25}* * * * *   E N D   O F   C O M P I L A T I O N   * * * * *",
        ""
    ));
    sections.push((0, summary));
    let mut out = Vec::new();
    let mut page = 0;
    for (heads, section) in sections {
        let (head, body) = section.split_at(heads);
        if body.is_empty() {
            page += 1;
            out.push(header(page));
            out.extend(head.iter().cloned());
            continue;
        }
        for chunk in body.chunks(PAGE - heads) {
            page += 1;
            out.push(header(page));
            out.extend(head.iter().cloned());
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(lines: &[&str]) -> Input {
        Input {
            name: "GLD5PF".into(),
            library: "AS4TRAINR1".into(),
            attribute: "Physical".into(),
            srcfile: ("AS4TRAINR1".into(), "GLD5SRC".into()),
            member: "GLD5PF".into(),
            changed: Some(Stamp {
                date: "26-10-27".into(),
                time: "16:45:33".into(),
                zone: String::new(),
            }),
            options: ["*SOURCE", "*LIST", "*NOSECLVL", "*NOEVENTF"]
                .map(String::from)
                .to_vec(),
            genlvl: "20".into(),
            flag: "00".into(),
            filetype: "*DATA".into(),
            aut: "*LIBCRTAUT".into(),
            replace: "*NO".into(),
            text: String::new(),
            lines: lines.iter().map(|l| l.to_string()).collect(),
            physical: true,
            display: None,
            refs: None,
        }
    }

    // PUB400 (IBM i 7.5) CRTPF listings, 2026-10-30, tools/golden/
    // verify29.txt v29_crtr and v29_crtp: a field's keyword lines shown
    // on its line, the default heading on a line of its own without a
    // number when it has keywords; reference fields with where they came
    // from, the keywords taken (a file field's TEXT padded to 50, '-' and
    // '+' continuations), *SRC references, a TEXT made from a heading
    // without a number; the file-level REF not shown; 50 lines a page,
    // the expanded source's headings again on its next page.
    #[test]
    fn physical_file_reference_listing() {
        let r_src = include_str!("../../tools/golden/verify29/gld29r.pf");
        let p_src = include_str!("../../tools/golden/verify29/gld29p.pf");
        let mut i = input(&r_src.lines().collect::<Vec<_>>());
        i.name = "GLD29R".into();
        let l = listing(
            &i,
            &[],
            &[("CPC7301".into(), 0, "x".into())],
            true,
            &stamp(),
        );
        let e = l
            .iter()
            .position(|x| x.contains("Expanded Source"))
            .unwrap();
        assert_eq!(
            &l[e + 3..e + 10],
            [
                "    200                  R GLD29RR",
                "    300                    NAME          20A  B      TEXT('Name') COLHDG('Name')             20            1       1",
                "    500                    AMOUNT         7P 2B      TEXT('Amount') EDTCDE(2)                 4           21      21",
                "                                                     COLHDG('AMOUNT')",
                "    700                    WHEN          10L  B      TEXT('When')                            10           25      25",
                "                                                     COLHDG('WHEN')",
                "                 * * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *",
            ]
        );
        let pf = crate::dds::parse_pf("GLD29R", r_src).unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert("GLD29R".to_string(), ("AS4TRAINR1".to_string(), pf));
        let refs = crate::ddsref::expand(p_src, crate::ddsref::Kind::Database, &files).unwrap();
        let mut i = input(&p_src.lines().collect::<Vec<_>>());
        i.name = "GLD29P".into();
        i.refs = Some(refs);
        let l = listing(
            &i,
            &[],
            &[("CPC7301".into(), 0, "x".into())],
            true,
            &stamp(),
        );
        let e = l
            .iter()
            .position(|x| x.contains("Expanded Source"))
            .unwrap();
        let want = [
            "    300                  R GLD29PR",
            "    400                    NAME          20A  B                                              20            1       1",
            " * Referenced Field  . . . . . . . . . . . :   NAME",
            " *   Format  . . . . . . . . . . . . . . . :   GLD29RR",
            " *   File  . . . . . . . . . . . . . . . . :   GLD29R",
            " *   Library . . . . . . . . . . . . . . . :   AS4TRAINR1",
            "    400                                              TEXT('Name                         -",
            "    400                                                                   ') +",
            "    400                                              COLHDG('Name')",
            "    500                    ADDR          20A  B                                              20           21      21",
            " * Referenced Field  . . . . . . . . . . . :   NAME",
            " *   Format  . . . . . . . . . . . . . . . :   GLD29RR",
            " *   File  . . . . . . . . . . . . . . . . :   GLD29R",
            " *   Library . . . . . . . . . . . . . . . :   AS4TRAINR1",
            "    600                                              COLHDG('Address')",
            "                                                     TEXT('Address')",
            "    700                    CITY          20A  B                                              20           41      41",
            " * Referenced Field  . . . . . . . . . . . :   ADDR",
            " *   Format  . . . . . . . . . . . . . . . :   GLD29PR",
            " *   File  . . . . . . . . . . . . . . . . :   *SRC",
            " *   Library . . . . . . . . . . . . . . . :",
            "    700                                              COLHDG('Address') TEXT('Address')",
            "    800                    AMOUNT         7P 2B                                               4           61      61",
        ];
        assert_eq!(&l[e + 3..e + 3 + want.len()], want);
        assert!(l.contains(
            &"    800                                                                   ') EDTCDE(2) +".to_string()
        ));
        // The next page: the header, the three heading lines, then WHEN's
        // reference.
        let p3 = l.iter().position(|x| x.ends_with("Page    3")).unwrap();
        assert_eq!(l[p3 - 1].trim_start(), "1100                    WHEN          10L  B                                              10           85      85");
        assert!(l[p3 + 1].contains("Expanded Source"));
        assert_eq!(
            l[p3 + 4],
            " * Referenced Field  . . . . . . . . . . . :   WHEN"
        );
    }

    // PUB400 (IBM i 7.5) CRTDSPF listing, 2026-10-30,
    // tools/golden/verify30.txt w30_crtd: the expanded source of a display
    // file with reference fields (GLD30R's), its own fields, a constant,
    // a hidden field; the first page without a file type.
    #[test]
    fn display_file_listing() {
        let pf_src = include_str!("../../tools/golden/verify30/gld30r.pf");
        let dspf_src = include_str!("../../tools/golden/verify30/gld30d.dspf");
        let pf = crate::dds::parse_pf("GLD30R", pf_src).unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert("GLD30R".to_string(), ("AS4TRAINR1".to_string(), pf));
        let expanded =
            crate::ddsref::expand(dspf_src, crate::ddsref::Kind::Device, &files).unwrap();
        let mut i = input(&dspf_src.lines().collect::<Vec<_>>());
        i.name = "GLD30D".into();
        i.attribute = "Display".into();
        i.replace = "*YES".into();
        i.display = Some(expanded);
        let l = listing(
            &i,
            &[],
            &[("CPC7301".into(), 0, "x".into())],
            true,
            &stamp(),
        );
        assert!(!l.iter().any(|x| x.starts_with(" File type ")), "{l:#?}");
        assert!(l.contains(
            &" File attribute  . . . . . . . . . . . . . . . . . . :   Display".to_string()
        ));
        let start = l
            .iter()
            .position(|x| x.contains("Expanded Source"))
            .unwrap();
        let want = [
            "                                          Expanded Source",
            "                                                                                          Field       Buffer position",
            " SEQNBR  *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8 length        Out     In",
            "                                                     DSPSIZ(*DS3)",
            "    300                  R GLD30A",
            "    400                                          1  2'Header'                                 6",
            "    500                    NAME          10A  B  3  2                                        10            1       1",
            " * Referenced Field  . . . . . . . . . . . :   NAME",
            " *   Format  . . . . . . . . . . . . . . . :   GLD30RR",
            " *   File  . . . . . . . . . . . . . . . . :   GLD30R",
            " *   Library . . . . . . . . . . . . . . . :   AS4TRAINR1",
            "    500                                              TEXT('Name                         -",
            "    500                                                                   ')",
            "    600                    PK             5S 0O  4  2                                         5           11",
            " * Referenced Field  . . . . . . . . . . . :   PK",
            " *   Format  . . . . . . . . . . . . . . . :   GLD30RR",
            " *   File  . . . . . . . . . . . . . . . . :   GLD30R",
            " *   Library . . . . . . . . . . . . . . . :   AS4TRAINR1",
            "    600                                              TEXT('Packed                       -",
            "    600                                                                   ')",
            "    700                    BIN            9S 0B  5  2                                         9           16      11",
        ];
        for (k, w) in want.iter().enumerate() {
            assert_eq!(l[start + k], *w, "line {k}");
        }
        let rest = &l[start + want.len()..];
        for w in [
            "    800                    ZD             7S 2I  6  2                                         7                   20",
            "    900                    OWNY           5Y 0B  7  2                                         5           25      27",
            "   1000                    OWNA          10A  I  8  2                                        10                   32",
            "   1100                    OWNS           3S 1O  9  2                                         3           30",
            "   1200                  R GLD30B",
            "   1300                    DT            10L  B  2  2                                        10            1       1",
            "   1400                    HID           10A  H                                              10           11      11",
            "                 * * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *",
        ] {
            assert!(rest.iter().any(|x| x == w), "{w}\n{rest:#?}");
        }
    }

    fn stamp() -> Stamp {
        Stamp {
            date: "26-10-27".into(),
            time: "16:45:33".into(),
            zone: String::new(),
        }
    }

    // Expected lines: PUB400 (IBM i 7.5) CRTPF listing, 2026-10-27,
    // tools/golden/verify5.txt z_crtpf (the product line, the compiler's
    // name and the message texts are as4tainr's).
    #[test]
    fn listing_layout() {
        let src = [
            "     A          R GLD5R",
            "     A            CODE           5P 0",
            "     A            NAME          20A",
            "     A          K CODE",
        ];
        let l = listing(
            &input(&src),
            &[],
            &[("CPC7301".into(), 0, "x".into())],
            true,
            &stamp(),
        );
        assert_eq!(
            l[0],
            format!(
                " {}  {}                  Data Description               AS4TRAINR1/GLD5PF       26-10-27 16:45:33        Page    1",
                crate::report::PRODUCT,
                crate::report::LEVEL
            )
        );
        assert_eq!(
            l[1],
            " File name . . . . . . . . . . . . . . . . . . . . . :   GLD5PF"
        );
        assert_eq!(
            l[2],
            "   Library name  . . . . . . . . . . . . . . . . . . :   AS4TRAINR1"
        );
        assert_eq!(
            l[4],
            " Source file containing DDS  . . . . . . . . . . . . :   GLD5SRC"
        );
        assert_eq!(
            l[7],
            " Source member last changed  . . . . . . . . . . . . :   26-10-27  16:45:33"
        );
        assert_eq!(l[8], " Source listing options  . . . . . . . . . . . . . . :   *SOURCE    *LIST      *NOSECLVL  *NOEVENTF");
        assert_eq!(
            l[14],
            " Text  . . . . . . . . . . . . . . . . . . . . . . . :"
        );
        assert_eq!(
            l[16],
            "                                      Data Description Source"
        );
        assert_eq!(l[18], "    100       A          R GLD5R");
        assert_eq!(
            l[22],
            "                         * * * * *   E N D   O F   S O U R C E   * * * * *"
        );
        let e = l
            .iter()
            .position(|x| x.ends_with("Expanded Source"))
            .unwrap();
        assert_eq!(l[e - 1].trim_end(), l[0].replace("Page    1", "Page    2"));
        assert_eq!(l[e + 1], "                                                                                          Field       Buffer position");
        assert_eq!(l[e + 3], "    100                  R GLD5R");
        assert_eq!(l[e + 4], "    200                    CODE           5P 0B      COLHDG('CODE')                           3            1       1");
        assert_eq!(l[e + 5], "    300                    NAME          20A  B      COLHDG('NAME')                          20            4       4");
        assert_eq!(l[e + 6], "    400                  K CODE");
        assert_eq!(
            l[e + 7],
            "                 * * * * *   E N D   O F   E X P A N D E D   S O U R C E   * * * * *"
        );
        // No messages page when the file is created without messages.
        assert!(!l.iter().any(|x| x.trim() == "Messages"));
        let s = l
            .iter()
            .position(|x| x.ends_with("Message Summary"))
            .unwrap();
        assert!(l[s - 1].ends_with("Page    3"));
        assert_eq!(
            l[s + 1],
            "    Total          Informational          Warning        Error       Severe"
        );
        assert_eq!(
            l[s + 3],
            "        0                 0                    0             0            0"
        );
        assert_eq!(
            l[s + 4],
            "* CPC7301      00               Message . . . . :   x"
        );
        assert_eq!(
            l[s + 5],
            "                         * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *"
        );
    }

    // PUB400 2026-10-27, verify5 z_crtpf_bad: the marker under the line,
    // the messages page, the summary's counts; z_crtpf_dup: a messages
    // page with no messages when the file is not created.
    #[test]
    fn errors_and_messages_page() {
        let src = [
            "     A          R GLD5R",
            "     A            CODE           5X 0",
        ];
        let m = Msg {
            id: Some("CPD7419".into()),
            sev: 30,
            line: 2,
            pos: 35,
            text: "t".into(),
        };
        let l = listing(
            &input(&src),
            &[m],
            &[("CPF7302".into(), 40, "y".into())],
            false,
            &stamp(),
        );
        let i = l.iter().position(|x| x.starts_with("    200")).unwrap();
        assert_eq!(l[i + 1], "*                                  CPD7419-*");
        let e = l
            .iter()
            .position(|x| x.ends_with("Expanded Source"))
            .unwrap();
        assert_eq!(l[e + 3], "    100                  R GLD5R");
        assert!(l[e + 4].contains("E N D   O F   E X P A N D E D"));
        let p = l.iter().position(|x| x.trim() == "Messages").unwrap();
        assert!(l[p - 1].ends_with("Page    3"));
        assert_eq!(l[p + 1], "    ID      Severity  Number");
        assert_eq!(
            l[p + 2],
            "* CPD7419      30        1      Message . . . . :   t"
        );
        let s = l
            .iter()
            .position(|x| x.ends_with("Message Summary"))
            .unwrap();
        assert!(l[s - 1].ends_with("Page    4"));
        assert_eq!(
            l[s + 3],
            "        1                 0                    0             0            1"
        );
        assert_eq!(
            l[s + 4],
            "* CPF7302      40               Message . . . . :   y"
        );
        let dup = listing(&input(&src[..1]), &[], &[], false, &stamp());
        let p = dup.iter().position(|x| x.trim() == "Messages").unwrap();
        assert!(dup[p + 2].ends_with("Page    4"));
        let mut nosrc = input(&src);
        nosrc.options = vec!["*NOSRC".into(), "*NOLIST".into()];
        assert!(listing(&nosrc, &[], &[], true, &stamp()).is_empty());
    }
}
