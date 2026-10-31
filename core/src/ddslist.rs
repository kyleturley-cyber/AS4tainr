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
        out.extend(origin_lines(&origin, self.record));
        // Its own keywords (REFFLD not shown) under the number of the
        // line with the first (v29_crtp ADDR: 600; y32_crtp AMT: 500 for
        // keywords on lines 500 and 600).
        let numbered: Vec<(usize, &str)> = self
            .original
            .iter()
            .enumerate()
            .map(|(k, l)| (s + k * 100, l.as_str()))
            .collect();
        let (mine, first) = own_keywords(&numbered);
        if let Some(f) = first {
            out.extend(keyword_lines(&format!("{f:>7}"), &mine));
        }
        // Then those taken, under the field's number (y32_crtp NAME:
        // TEXT('Mine') and the taken COLHDG('NAME') on lines of their
        // own); a TEXT made from its own heading without a number.
        let mut taken = Vec::new();
        let mut unnumbered = Vec::new();
        for k in copied {
            if crate::dds::keyword_name(&k) == "TEXT" && has("COLHDG") && !has("TEXT") {
                unnumbered.push(k);
                continue;
            }
            taken.push(shown_copied(k, !src));
        }
        out.extend(keyword_lines(&format!("{s:>7}"), &taken));
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

/// The keywords of DDS lines' positions 45-80, and the number of the
/// first line with one (REF and REFFLD left out: not shown expanded).
fn own_keywords(lines: &[(usize, &str)]) -> (Vec<String>, Option<usize>) {
    let mut out = Vec::new();
    let mut first = None;
    for (n, l) in lines {
        for k in crate::dds::split_keywords(&l.chars().skip(44).collect::<String>()) {
            if matches!(crate::dds::keyword_name(&k).as_str(), "REF" | "REFFLD") {
                continue;
            }
            first.get_or_insert(*n);
            out.push(k);
        }
    }
    (out, first)
}

/// A reference field's origin lines (PUB400 2026-10-30/31, verify29
/// v29_crtp, verify32 y32_crtd): the referenced field, its record
/// format, file and library; *SRC: this record format, file *SRC, no
/// library.
fn origin_lines(origin: &[String], record: &str) -> Vec<String> {
    if origin.len() < 4 {
        return Vec::new();
    }
    let (fmt, file, lib) = if origin[2] == "*SRC" {
        (record.to_string(), "*SRC".to_string(), String::new())
    } else {
        (origin[2].clone(), origin[1].clone(), origin[0].clone())
    };
    vec![
        format!(
            " * Referenced Field  . . . . . . . . . . . :   {}",
            origin[3]
        ),
        format!(" *   Format  . . . . . . . . . . . . . . . :   {fmt}"),
        format!(" *   File  . . . . . . . . . . . . . . . . :   {file}"),
        format!(" *   Library . . . . . . . . . . . . . . . :   {lib}")
            .trim_end()
            .to_string(),
    ]
}

/// A copied TEXT as the listing shows it: a file field's padded to 50
/// (its text in the file's description), a *SRC field's as written.
fn shown_copied(k: String, from_file: bool) -> String {
    if from_file && crate::dds::keyword_name(&k) == "TEXT" {
        let inner = k.split_once('(').map(|x| x.1).unwrap_or("");
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let t = inner.trim().trim_matches('\'');
        return format!("TEXT('{t:<50}')");
    }
    k
}

/// A line's positions 1-44 as the expanded source shows them (1-7
/// blank), then the first of its keyword lines.
fn with_keywords(head44: &str, first: &str) -> String {
    let rest: String = head44.chars().skip(7).take(37).collect();
    format!("{:7}{rest:<37}{first}", "")
}

/// A display file's expanded source (PUB400 2026-10-30/31, verify29
/// v29_crtd, verify30 w30_crtd, verify32 y32_crtd): file-level keywords
/// as written (REF not shown), DSPSIZ(*DS3) when the source names none,
/// the indicator area's indicators after INDARA; record lines as
/// written; a constant with its keywords on its line and its length; a
/// field with its length, type and decimals made explicit (a date 10L),
/// its own keywords on its line, its length in bytes and its output and
/// input buffer positions (output: O, B, H fields; input: I, B, H; each
/// record from 1); a reference field without keywords on its line, its
/// origin, its own keywords under the number of the line with the first,
/// then those taken from the referenced field (a file field's TEXT padded
/// to 50) under its own number.
fn display_expanded(input: &Input, expanded_src: &str) -> Vec<String> {
    // The expanded lines with their original line numbers; an A*@REF
    // comment goes with the line before it.
    let mut items: Vec<(usize, String, Option<Vec<String>>)> = Vec::new();
    let mut n = 0usize;
    for raw in expanded_src.lines() {
        let line = raw.trim_end();
        if let Some(r) = line.trim_start().strip_prefix("A*@REF ") {
            if let Some(last) = items.last_mut() {
                last.2 = Some(r.split_whitespace().map(str::to_string).collect());
            }
            continue;
        }
        n += 1;
        items.push((n, line.to_string(), None));
    }
    let orig = |n: usize| {
        input
            .lines
            .get(n - 1)
            .map(|l| l.trim_end().to_string())
            .unwrap_or_default()
    };
    let is_comment = |l: &str| l.trim().is_empty() || col(l, 7, 7) == "*";
    let kw_only = |l: &str| !is_comment(l) && col(l, 17, 44).is_empty();
    let upper = expanded_src.to_ascii_uppercase();
    let mut out = Vec::new();
    let mut in_record = false;
    let mut record = String::new();
    let (mut outpos, mut inpos) = (1u32, 1u32);
    let mut i = 0;
    while i < items.len() {
        let (n, line, _) = items[i].clone();
        i += 1;
        if is_comment(&line) {
            continue;
        }
        let s = seq(n);
        if !in_record && kw_only(&line) {
            // A file-level keyword line.
            let (kws, _) = own_keywords(&[(n, &orig(n))]);
            if !kws.is_empty() {
                out.push(
                    format!("{s:>7}  {}", blanked(&orig(n)))
                        .trim_end()
                        .to_string(),
                );
            }
            continue;
        }
        // The entry: this line and its keyword lines.
        let start = i - 1;
        while i < items.len() && kw_only(&items[i].1) {
            i += 1;
        }
        let entry = &items[start..i];
        let origin = entry.iter().rev().find_map(|e| e.2.clone());
        let originals: Vec<(usize, String)> = entry.iter().map(|e| (e.0, orig(e.0))).collect();
        let refs: Vec<(usize, &str)> = originals.iter().map(|(n, l)| (*n, l.as_str())).collect();
        let (own, own_first) = own_keywords(&refs);
        let name_type = col(&line, 17, 17).to_ascii_uppercase();
        let name = col(&line, 19, 28).to_ascii_uppercase();
        if name_type == "R" {
            if !in_record {
                in_record = true;
                if !upper.contains("DSPSIZ(") {
                    out.push(format!("{:53}DSPSIZ(*DS3)", ""));
                }
                if upper
                    .lines()
                    .take_while(|l| col(l, 17, 17) != "R")
                    .any(|l| l.contains("INDARA"))
                {
                    out.push(format!("{:15}* Indicator Area:", ""));
                    for ind in indicators(expanded_src) {
                        out.push(format!("{:15}*  *IN{ind:02}", ""));
                    }
                }
            }
            record = name.clone();
            (outpos, inpos) = (1, 1);
            for (n, l) in &originals {
                out.push(
                    format!("{:>7}  {}", seq(*n), blanked(l))
                        .trim_end()
                        .to_string(),
                );
            }
            continue;
        }
        let head44: String = line.chars().take(44).collect();
        if name.is_empty() {
            // A constant: its text and keywords on its line, its length.
            let packed = keyword_lines("", &own);
            let first = packed
                .first()
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            let len = constant_length(&own);
            out.push(
                format!("{s:>7}  {:<80}{len:>6}", with_keywords(&head44, &first))
                    .trim_end()
                    .to_string(),
            );
            for l in packed.iter().skip(1) {
                out.push(format!("{s:>7}{}", &l[7..]));
            }
            continue;
        }
        // A field.
        let ty = col(&line, 35, 35).to_ascii_uppercase();
        let dec_txt = col(&line, 36, 37).to_string();
        let dtype = match ty.chars().next() {
            Some(c) => c,
            None if !dec_txt.is_empty() => 'S',
            None => 'A',
        };
        let len: u32 = match dtype {
            'L' => 10,
            'T' => 8,
            'Z' => 26,
            _ => col(&line, 30, 34).parse().unwrap_or(0),
        };
        let usage = col(&line, 38, 38).to_ascii_uppercase();
        let usage = if usage.is_empty() {
            "B".to_string()
        } else {
            usage
        };
        let fhead: String = line.chars().skip(7).take(21).collect();
        let spec = format!(
            "{:7}{fhead:<21} {len:>5}{dtype}{dec_txt:>2}{usage:1}{:>3}{:>3}",
            "",
            col(&line, 39, 41),
            col(&line, 42, 44)
        );
        let (o, inp) = match usage.as_str() {
            "O" => (Some(outpos), None),
            "I" => (None, Some(inpos)),
            _ => (Some(outpos), Some(inpos)),
        };
        if o.is_some() {
            outpos += len;
        }
        if inp.is_some() {
            inpos += len;
        }
        let o = o.map(|v| v.to_string()).unwrap_or_default();
        let inp = inp.map(|v| v.to_string()).unwrap_or_default();
        let tail = |l: String| format!("{l:<80}{len:>6}{o:>13}{inp:>8}");
        match &origin {
            None => {
                let packed = keyword_lines("", &own);
                let first = packed
                    .first()
                    .map(|l| l.trim().to_string())
                    .unwrap_or_default();
                let l = tail(with_keywords(&spec, &first));
                out.push(format!("{s:>7}  {l}").trim_end().to_string());
                for l in packed.iter().skip(1) {
                    out.push(format!("{s:>7}{}", &l[7..]));
                }
            }
            Some(org) => {
                out.push(format!("{s:>7}  {}", tail(spec)).trim_end().to_string());
                out.extend(origin_lines(org, &record));
                if let Some(f) = own_first {
                    out.extend(keyword_lines(&format!("{:>7}", seq(f)), &own));
                }
                // Taken from the referenced field: what the expanded source
                // has after the entry's own keywords.
                let last_orig = originals.last().map(|x| x.1.clone()).unwrap_or_default();
                let last_exp = entry.last().map(|e| e.1.clone()).unwrap_or_default();
                let mine: String = last_orig
                    .chars()
                    .skip(44)
                    .collect::<String>()
                    .trim()
                    .to_string();
                let all: String = last_exp
                    .chars()
                    .skip(44)
                    .collect::<String>()
                    .trim()
                    .to_string();
                let added = all
                    .strip_prefix(mine.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let from_file = org.get(2).is_some_and(|f| f != "*SRC");
                let copied: Vec<String> = crate::dds::split_keywords(&added)
                    .into_iter()
                    .map(|k| shown_copied(k, from_file))
                    .collect();
                out.extend(keyword_lines(&format!("{s:>7}"), &copied));
            }
        }
    }
    out
}

/// A constant's length: its text's, a DATE's 6 (8 with EDTCDE(Y)), a
/// TIME's 6 (PUB400 2026-10-31, verify32 y32_crtd: DATE EDTCDE(Y) is 8).
fn constant_length(kws: &[String]) -> String {
    let Some(first) = kws.first() else {
        return String::new();
    };
    if let Some(r) = first.strip_prefix('\'') {
        if let Some(e) = r.rfind('\'') {
            return r[..e].replace("''", "'").chars().count().to_string();
        }
    }
    match crate::dds::keyword_name(first).as_str() {
        "DATE" => {
            if kws
                .iter()
                .any(|k| k.to_ascii_uppercase().replace(' ', "") == "EDTCDE(Y)")
            {
                "8".into()
            } else {
                "6".into()
            }
        }
        "TIME" => "6".into(),
        _ => String::new(),
    }
}

/// The indicators a display file uses: response indicators of CAnn and
/// CFnn, and option indicators (positions 8-16), in order.
fn indicators(src: &str) -> Vec<u32> {
    let mut out = std::collections::BTreeSet::new();
    for line in src.lines() {
        if col(line, 7, 7) == "*" {
            continue;
        }
        for k in [(8, 10), (11, 13), (14, 16)] {
            let c = col(line, k.0, k.1);
            let d = c.trim_start_matches(['N', 'n']);
            if let Ok(v) = d.trim().parse::<u32>() {
                if (1..=99).contains(&v) {
                    out.insert(v);
                }
            }
        }
        for kw in crate::dds::split_keywords(&line.chars().skip(44).collect::<String>()) {
            let name = crate::dds::keyword_name(&kw);
            if (name.starts_with("CA") || name.starts_with("CF")) && name.len() == 4 {
                let inner = kw.split_once('(').map(|x| x.1).unwrap_or("");
                if let Some(v) = inner
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.trim_end_matches(')').parse::<u32>().ok())
                {
                    out.insert(v);
                }
            }
        }
    }
    out.into_iter().collect()
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

    // PUB400 (IBM i 7.5) CRTDSPF listing, 2026-10-31, tools/golden/
    // verify32.txt y32_crtd: file-level INDARA, then DSPSIZ(*DS3) and the
    // indicator area; record keywords; a constant's and a field's own
    // keywords on their line; a reference field's own keywords, then the
    // taken ones; a *SRC reference; DATE EDTCDE(Y) 8 long.
    #[test]
    fn display_file_listing_with_own_keywords() {
        let pf_src = include_str!("../../tools/golden/verify32/gld32r.pf");
        let dspf_src = include_str!("../../tools/golden/verify32/gld32d.dspf");
        let pf = crate::dds::parse_pf("GLD32R", pf_src).unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert("GLD32R".to_string(), ("AS4TRAINR1".to_string(), pf));
        let expanded =
            crate::ddsref::expand(dspf_src, crate::ddsref::Kind::Device, &files).unwrap();
        let mut i = input(&dspf_src.lines().collect::<Vec<_>>());
        i.name = "GLD32D".into();
        i.display = Some(expanded);
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
            "    400                                              INDARA",
            "                                                     DSPSIZ(*DS3)",
            "               * Indicator Area:",
            "               *  *IN03",
            "    500                  R GLD32A                    CA03(03 'Exit')",
            "    600                                          1  2'Title' DSPATR(HI)                       5",
            "    800                    NAME          10A  B  3  2                                        10            1       1",
            " * Referenced Field  . . . . . . . . . . . :   NAME",
            " *   Format  . . . . . . . . . . . . . . . :   GLD32RR",
            " *   File  . . . . . . . . . . . . . . . . :   GLD32R",
            " *   Library . . . . . . . . . . . . . . . :   AS4TRAINR1",
            "    800                                              COLOR(WHT)",
            "    800                                              TEXT('Name                         -",
            "    800                                                                   ')",
            "    900                    OWN           10A  O  4  2TEXT('Own text') DSPATR(HI)             10           11",
            "   1000                    COPY          10A  O  5  2                                        10           21",
            " * Referenced Field  . . . . . . . . . . . :   OWN",
            " *   Format  . . . . . . . . . . . . . . . :   GLD32A",
            " *   File  . . . . . . . . . . . . . . . . :   *SRC",
            " *   Library . . . . . . . . . . . . . . . :",
            "   1000                                              TEXT('Own text')",
            "   1100                    AMT            7Y 2B  6  2                                         7           31      11",
        ];
        assert_eq!(&l[e + 3..e + 3 + want.len()], want);
        assert!(l.contains(
            &"   1100                                              EDTCDE(J)".to_string()
        ));
        assert!(l.contains(&"   1200                                          7  2DATE EDTCDE(Y)                           8".to_string()));
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
