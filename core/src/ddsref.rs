//! Reference fields in DDS (physical, display and printer files): R in
//! position 29 gives a field the attributes of another one, found with
//! REFFLD and the file-level REF keyword. Written from IBM's DDS
//! reference for physical and logical files (position 29; REF, REFFLD)
//! and DDS concepts (appendix B: when to specify REF and REFFLD):
//!
//! - the referenced field is the field's own name, or REFFLD's
//!   `[format/]field`; it is looked for in REFFLD's file, `*SRC` (a
//!   field defined earlier in the same source), or else the REF file, or
//!   else the same source;
//! - the length, data type and decimal positions are copied unless the
//!   referring field gives them; a length or decimal positions of `+n` or
//!   `-n` changes the referenced one;
//! - keywords copied unless the referring field has its own: ALIAS,
//!   COLHDG, DATFMT, DATSEP (not with DATFMT *ISO, *EUR, *USA or *JIS),
//!   FLTPCN, REFSHIFT, TEXT, TIMFMT, TIMSEP, VARLEN; the editing keywords
//!   (EDTCDE, EDTWRD) unless the field has one, and the validity checking
//!   keywords (CHECK, CHKMSGID, COMP, CMP, RANGE, VALUES) unless it has
//!   one; neither when it gives its own data type, length or decimals.
//!
//! `expand` rewrites the source with each referring field's attributes in
//! positions 30-37 and the copied keywords at the end of its keyword
//! area, so the parsers (dds.rs, dspf.rs, prtf.rs) see plain fields; line
//! numbers stay as they were. A display or printer file has no packed or
//! binary fields: a reference to one is zoned (S).

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use tokio_postgres::Client;

use crate::dds::{keyword_name, split_keywords, DataType, PhysicalFile};

/// The kind of file the source describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Database,
    /// A display file.
    Device,
    /// A printer file (no numeric-only Y fields).
    Printer,
}

fn cols(line: &str, from: usize, to: usize) -> String {
    line.chars()
        .skip(from - 1)
        .take(to.saturating_sub(from).saturating_add(1))
        .collect::<String>()
        .trim()
        .to_string()
}

/// A field's attributes as a reference copies them.
#[derive(Clone, Debug)]
struct Attrs {
    code: String,
    length: u32,
    decimals: u32,
    numeric: bool,
    /// Keywords as written (TEXT, COLHDG, EDTCDE...).
    keywords: Vec<String>,
    /// The field's name: its column heading when it has no COLHDG.
    name: String,
}

fn of_field(f: &crate::dds::FieldDef) -> Attrs {
    let mut keywords = Vec::new();
    if !f.text.is_empty() {
        keywords.push(format!("TEXT('{}')", f.text.replace('\'', "''")));
    }
    if let Some(a) = &f.alias {
        keywords.push(format!("ALIAS({a})"));
    }
    if f.varlen {
        keywords.push("VARLEN".into());
    }
    if f.double {
        keywords.push("FLTPCN(*DOUBLE)".into());
    }
    keywords.extend(split_keywords(&f.keywords));
    Attrs {
        code: f.data_type.code().to_string(),
        length: f.length,
        decimals: f.decimals,
        numeric: f.data_type.is_numeric(),
        keywords,
        name: f.name.clone(),
    }
}

/// REFFLD's arguments: (field, file or `*SRC`, if given).
fn reffld(keywords: &[String]) -> Option<(String, Option<String>)> {
    let k = keywords.iter().find(|k| keyword_name(k) == "REFFLD")?;
    let inner = k.split_once('(')?.1.trim_end_matches(')');
    let mut parts = inner.split_whitespace();
    let field = parts.next()?.to_ascii_uppercase();
    let field = field.rsplit('/').next().unwrap_or(&field).to_string();
    Some((field, parts.next().map(|f| f.to_ascii_uppercase())))
}

fn file_level_ref(source: &str) -> Option<String> {
    for line in source.lines() {
        if cols(line, 7, 7) == "*" || line.trim().is_empty() {
            continue;
        }
        if !cols(line, 17, 17).is_empty() {
            return None;
        }
        for k in split_keywords(&cols(line, 45, usize::MAX)) {
            if keyword_name(&k) == "REF" {
                let inner = k.split_once('(')?.1.trim_end_matches(')').to_string();
                return inner
                    .split_whitespace()
                    .next()
                    .map(|f| f.to_ascii_uppercase());
            }
        }
    }
    None
}

/// The database files the source refers to (REF, REFFLD), as written.
pub fn referenced_files(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(f) = file_level_ref(source) {
        out.push(f);
    }
    let mut joined: Vec<String> = Vec::new();
    for line in source.lines() {
        if cols(line, 7, 7) == "*" {
            continue;
        }
        let area = cols(line, 45, usize::MAX);
        match joined.last_mut() {
            Some(prev) if prev.ends_with('-') => {
                prev.pop();
                let raw: String = line.chars().skip(44).collect();
                prev.push_str(raw.trim_end());
            }
            Some(prev) if prev.ends_with('+') => {
                prev.pop();
                prev.push_str(&area);
            }
            _ => joined.push(area),
        }
    }
    for area in &joined {
        let kws = split_keywords(area);
        if let Some((_, Some(f))) = reffld(&kws) {
            if f != "*SRC" && !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// Positions 7-44 blank: a line of keywords for the entry above it.
fn keyword_line(line: &str) -> bool {
    cols(line, 7, 44).is_empty() && !cols(line, 45, usize::MAX).is_empty()
}

fn adjust(given: &str, referenced: u32) -> Result<(u32, bool)> {
    let given = given.trim();
    if given.is_empty() {
        return Ok((referenced, false));
    }
    let n = |s: &str| {
        s.parse::<u32>()
            .map_err(|_| anyhow!("'{given}' is not a length or decimal positions"))
    };
    Ok(if let Some(d) = given.strip_prefix('+') {
        (referenced + n(d)?, true)
    } else if let Some(d) = given.strip_prefix('-') {
        (referenced.saturating_sub(n(d)?), true)
    } else {
        (n(given)?, true)
    })
}

/// Give each referring field (R in position 29) its attributes; `files`
/// holds the referenced database files by the name the source gives them.
pub fn expand(
    source: &str,
    kind: Kind,
    files: &HashMap<String, (String, PhysicalFile)>,
) -> Result<String> {
    let mut lines: Vec<String> = source
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let file_ref = file_level_ref(source);
    // Fields defined so far in this source (*SRC), by name.
    let mut earlier: HashMap<String, Attrs> = HashMap::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].clone();
        if cols(&line, 7, 7) == "*" || line.trim().is_empty() || keyword_line(&line) {
            i += 1;
            continue;
        }
        // The entry: this line and the keyword lines after it.
        let mut last = i;
        let mut keyword_text = cols(&line, 45, usize::MAX);
        while last + 1 < lines.len() {
            let next = &lines[last + 1];
            if cols(next, 7, 7) == "*" {
                break;
            }
            let more = cols(next, 45, usize::MAX);
            if keyword_text.ends_with('-') {
                // Continued as is (positions 45 on of the next line).
                keyword_text.pop();
                let raw: String = next.chars().skip(44).collect();
                keyword_text.push_str(raw.trim_end());
                last += 1;
            } else if keyword_text.ends_with('+') {
                keyword_text.pop();
                keyword_text.push_str(&more);
                last += 1;
            } else if keyword_line(next) {
                keyword_text.push(' ');
                keyword_text.push_str(&more);
                last += 1;
            } else {
                break;
            }
        }
        let name = cols(&line, 19, 28).to_ascii_uppercase();
        let named_field = cols(&line, 17, 17).is_empty() && !name.is_empty();
        if !named_field {
            i = last + 1;
            continue;
        }
        let keywords = split_keywords(&keyword_text);
        let own = |k: &str| keywords.iter().any(|w| keyword_name(w) == k);
        if !cols(&line, 29, 29).eq_ignore_ascii_case("R") {
            // A field of its own: noted for *SRC references.
            let (len, _) = adjust(&cols(&line, 30, 34), 0).unwrap_or((0, false));
            let (dec, _) = adjust(&cols(&line, 36, 37), 0).unwrap_or((0, false));
            let code = cols(&line, 35, 35).to_ascii_uppercase();
            let numeric = !cols(&line, 36, 37).is_empty()
                || matches!(code.as_str(), "P" | "S" | "B" | "F" | "Y" | "I" | "N");
            earlier.insert(
                name.clone(),
                Attrs {
                    code: if code.is_empty() {
                        if numeric {
                            if kind == Kind::Database { "P" } else { "S" }.into()
                        } else {
                            "A".into()
                        }
                    } else {
                        code
                    },
                    length: len,
                    decimals: dec,
                    numeric,
                    keywords: keywords.clone(),
                    name: name.clone(),
                },
            );
            i = last + 1;
            continue;
        }
        let (ref_name, ref_file) = match reffld(&keywords) {
            Some((f, file)) => (f, file),
            None => (name.clone(), None),
        };
        let from = ref_file.or_else(|| file_ref.clone());
        // Where the field came from (DSPFFD's referenced information):
        // library, file and record format; *SRC: none, this file's format.
        let mut origin = ("*N".to_string(), "*N".to_string(), "*SRC".to_string());
        let referenced = match from.as_deref() {
            None | Some("*SRC") => earlier.get(&ref_name).cloned().ok_or_else(|| {
                anyhow!(
                    "line {}: field {name} refers to {ref_name}, not defined before it",
                    i + 1
                )
            })?,
            Some(file) => {
                let (library, pf) = files.get(file).ok_or_else(|| {
                    anyhow!(
                        "line {}: file {file} that field {name} refers to not found",
                        i + 1
                    )
                })?;
                let f = pf
                    .fields
                    .iter()
                    .find(|f| f.name == ref_name)
                    .ok_or_else(|| {
                        anyhow!(
                        "line {}: field {name} refers to {ref_name}, not a field of file {file}",
                        i + 1
                    )
                    })?;
                origin = (library.clone(), pf.name.clone(), pf.format.clone());
                of_field(f)
            }
        };
        let (length, len_given) = adjust(&cols(&line, 30, 34), referenced.length)
            .map_err(|e| anyhow!("line {}: {e}", i + 1))?;
        let (decimals, dec_given) = adjust(&cols(&line, 36, 37), referenced.decimals)
            .map_err(|e| anyhow!("line {}: {e}", i + 1))?;
        let given_code = cols(&line, 35, 35).to_ascii_uppercase();
        let type_given = !given_code.is_empty();
        let mut code = if type_given {
            given_code
        } else {
            referenced.code.clone()
        };
        if kind != Kind::Database && matches!(code.as_str(), "P" | "B") {
            code = "S".into();
        }
        let numeric = match DataType::from_code(&code) {
            Ok(t) => t.is_numeric(),
            Err(_) => referenced.numeric || code == "Y",
        };
        let fixed = DataType::from_code(&code)
            .ok()
            .and_then(|t| t.fixed_length())
            .is_some();
        let attrs_given = type_given || len_given || dec_given;
        // Keywords copied.
        let mut copied: Vec<String> = Vec::new();
        let datfmt_iso = keywords
            .iter()
            .chain(referenced.keywords.iter())
            .find(|k| keyword_name(k) == "DATFMT")
            .is_some_and(|k| {
                let u = k.to_ascii_uppercase();
                ["*ISO", "*EUR", "*USA", "*JIS"]
                    .iter()
                    .any(|f| u.contains(f))
            });
        let editing = ["EDTCDE", "EDTWRD"];
        let validity = ["CHECK", "CHKMSGID", "COMP", "CMP", "RANGE", "VALUES"];
        let has_editing = editing.iter().any(|k| own(k));
        let has_validity = validity.iter().any(|k| own(k));
        for k in &referenced.keywords {
            let n = keyword_name(k);
            let take = match n.as_str() {
                // An input-only or hidden display field takes no editing
                // (PUB400 2026-10-30, verify30 w30_crtd: ZD, usage I, is
                // 7S 2 without its EDTCDE(J)).
                "EDTCDE" | "EDTWRD" => {
                    !has_editing
                        && !attrs_given
                        && !(kind != Kind::Database
                            && matches!(
                                cols(&line, 38, 38).to_ascii_uppercase().as_str(),
                                "I" | "H"
                            ))
                }
                "CHECK" | "CHKMSGID" | "COMP" | "CMP" | "RANGE" | "VALUES" => {
                    !has_validity && !attrs_given
                }
                "DATSEP" => !own("DATSEP") && !datfmt_iso,
                "COLHDG" | "ALIAS" | "VARLEN" | "FLTPCN" if kind != Kind::Database => false,
                "TEXT" | "COLHDG" | "ALIAS" | "VARLEN" | "FLTPCN" | "DATFMT" | "TIMFMT"
                | "TIMSEP" | "REFSHIFT" => !own(&n),
                _ => false,
            };
            // A heading of its own: the referenced text is not taken; the
            // heading is the field's text (PUB400 2026-10-30, verify29
            // v29_ffdp: ADDR, COLHDG('Address'), shows text Address).
            let take = take && !(n == "TEXT" && own("COLHDG"));
            if take {
                copied.push(k.clone());
            }
        }
        if kind == Kind::Database {
            if own("COLHDG") && !own("TEXT") {
                if let Some(h) = keywords.iter().find(|k| keyword_name(k) == "COLHDG") {
                    let inner = h.split_once('(').map(|x| x.1).unwrap_or("");
                    let inner = inner.strip_suffix(')').unwrap_or(inner);
                    let parts: Vec<String> = inner
                        .split('\'')
                        .enumerate()
                        .filter(|(j, _)| j % 2 == 1)
                        .map(|(_, p)| p.trim().to_string())
                        .collect();
                    copied.push(format!("TEXT('{}')", parts.join(" ")));
                }
            }
            // No heading here or there: the referenced field's name
            // (v29_ffdp: BIGAMT, referring to AMOUNT, is headed AMOUNT).
            if !own("COLHDG")
                && !referenced
                    .keywords
                    .iter()
                    .any(|k| keyword_name(k) == "COLHDG")
            {
                copied.push(format!("COLHDG('{}')", referenced.name));
            }
        }
        // What was changed from the referenced field (DSPFFD's "Attributes
        // changed": v29_ffdp: None, Field length for +2 and -5, Other for
        // a heading of its own).
        let changed = if type_given || len_given || dec_given {
            "L"
        } else if keywords.iter().any(|k| {
            crate::dds::REFERENCED_KEYWORDS.contains(&keyword_name(k).as_str())
                || matches!(keyword_name(k).as_str(), "TEXT" | "ALIAS")
        }) {
            "O"
        } else {
            "N"
        };
        // For a physical file's description (dds.rs); display and printer
        // file parsers do not read it.
        if kind == Kind::Database {
            copied.push(format!(
                "@REF({} {} {} {} {changed})",
                origin.0, origin.1, origin.2, ref_name
            ));
        }
        // A display file's numeric field with an edit code or word is
        // numeric-only, Y (PUB400 2026-10-30, verify29 v29_crtd:
        // AMOUNT, packed with EDTCDE(2), expanded as 7Y 2).
        if kind == Kind::Device
            && !type_given
            && code == "S"
            && (has_editing
                || copied
                    .iter()
                    .any(|k| matches!(keyword_name(k).as_str(), "EDTCDE" | "EDTWRD")))
        {
            code = "Y".into();
        }
        // Positions 29-37 rewritten.
        let mut chars: Vec<char> = line.chars().collect();
        if chars.len() < 44 {
            chars.resize(44, ' ');
        }
        let len_text = if fixed {
            String::new()
        } else {
            length.to_string()
        };
        let dec_text = if numeric && !fixed {
            decimals.to_string()
        } else {
            String::new()
        };
        let positional = format!(" {len_text:>5}{code:1}{dec_text:>2}");
        if positional.chars().count() != 9 {
            bail!("line {}: field {name}: attributes do not fit", i + 1);
        }
        for (k, c) in positional.chars().enumerate() {
            chars[28 + k] = c;
        }
        lines[i] = chars.into_iter().collect::<String>();
        if !copied.is_empty() {
            let mut l = lines[last].clone();
            if l.chars().count() < 44 {
                let n = 44 - l.chars().count();
                l.push_str(&" ".repeat(n));
            }
            l.push(' ');
            l.push_str(&copied.join(" "));
            lines[last] = l;
        }
        // A display file's field: where it came from, on a comment line
        // after it (the display file parser skips it; DSPFFD reads it).
        let mut next = last + 1;
        if kind == Kind::Device {
            lines.insert(
                last + 1,
                format!(
                    "     A*@REF {} {} {} {} {changed}",
                    origin.0, origin.1, origin.2, ref_name
                ),
            );
            next += 1;
        }
        let mut all = keywords.clone();
        all.extend(copied);
        earlier.insert(
            name.clone(),
            Attrs {
                code,
                length,
                decimals,
                numeric,
                keywords: all,
                name: name.clone(),
            },
        );
        i = next;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// `expand` with the referenced files found in the catalog (a library
/// given, or the library list).
pub async fn expand_from_catalog(
    client: &Client,
    libl: &[String],
    source: &str,
    kind: Kind,
) -> Result<String> {
    let mut files = HashMap::new();
    let mut has_ref = false;
    for line in source.lines() {
        if cols(line, 7, 7) != "*" && cols(line, 29, 29).eq_ignore_ascii_case("R") {
            has_ref = true;
            break;
        }
    }
    if !has_ref {
        return Ok(source.to_string());
    }
    for spec in referenced_files(source) {
        let spec_lookup = spec.trim_start_matches("*LIBL/").to_string();
        if let Ok(f) = crate::catalog::resolve_file(client, libl, &spec_lookup).await {
            files.insert(spec, (f.library, f.file));
        }
    }
    expand(source, kind, &files)
}

#[cfg(test)]
mod tests {
    use super::*;

    // DDS concepts, appendix B and the example of a field reference file:
    // R with REFFLD takes the referenced field's attributes; a field's own
    // keyword (COLHDG) wins over the referenced one; "+2" in the length
    // makes the field two longer (DDS reference, position 29 example
    // `SUPNBR R +2 REFFLD(SUPID SUPLIB/SUPMST)`).
    #[test]
    fn referring_fields_take_the_referenced_attributes() {
        let refsrc = "     A          R REFR
     A            NAME          20A         TEXT('Name') COLHDG('Name')
     A            AMOUNT         7P 2       EDTCDE(2)
";
        let refpf = crate::dds::parse_pf("REFF", refsrc).unwrap();
        let mut files = HashMap::new();
        files.insert("REFF".to_string(), ("LIB".to_string(), refpf));
        let src = "     A                                      REF(REFF)
     A          R REC
     A            NAME      R
     A            ADDR      R               REFFLD(NAME)
     A                                      COLHDG('Address')
     A            CITY      R               REFFLD(ADDR *SRC)
     A            AMOUNT    R
     A            BIGAMT    R   +2          REFFLD(AMOUNT)
";
        let out = expand(src, Kind::Database, &files).unwrap();
        let pf = crate::dds::parse_pf("X", &out).unwrap();
        let f = |n: &str| pf.fields.iter().find(|f| f.name == n).unwrap().clone();
        assert_eq!(
            (f("NAME").data_type, f("NAME").length),
            (DataType::Char, 20)
        );
        assert_eq!(f("NAME").text, "Name");
        assert_eq!(f("ADDR").keywords, "COLHDG('Address')");
        // A heading of its own and no text: the heading is its text
        // (PUB400 2026-10-30, verify29 v29_ffdp: ADDR's text is Address);
        // CITY takes both from ADDR.
        assert_eq!(f("ADDR").text, "Address");
        assert_eq!(f("CITY").keywords, "COLHDG('Address')");
        assert_eq!(f("CITY").text, "Address");
        let r = f("ADDR").reference.unwrap();
        assert_eq!(
            (
                r.library.as_str(),
                r.file.as_str(),
                r.format.as_str(),
                r.field.as_str(),
                r.changed.as_str()
            ),
            ("LIB", "REFF", "REFR", "NAME", "Other")
        );
        // From the same source: no file or library, this file's format.
        let r = f("CITY").reference.unwrap();
        assert_eq!(
            (
                r.file.as_str(),
                r.format.as_str(),
                r.field.as_str(),
                r.changed.as_str()
            ),
            ("", "REC", "ADDR", "None")
        );
        assert_eq!(
            (
                f("AMOUNT").data_type,
                f("AMOUNT").length,
                f("AMOUNT").decimals
            ),
            (DataType::Packed, 7, 2)
        );
        // No heading anywhere: the referenced field's name (v29_ffdp:
        // AMOUNT and BIGAMT are headed AMOUNT).
        assert_eq!(f("AMOUNT").keywords, "EDTCDE(2) COLHDG('AMOUNT')");
        // A length of its own: the editing keywords are not copied.
        assert_eq!((f("BIGAMT").length, f("BIGAMT").decimals), (9, 2));
        assert_eq!(f("BIGAMT").keywords, "COLHDG('AMOUNT')");
        assert_eq!(f("BIGAMT").reference.unwrap().changed, "Field length");
        // Line numbers stay.
        assert_eq!(out.lines().count(), src.lines().count());
        // A display file: packed becomes zoned (S).
        let d = expand(
            "     A                                      REF(REFF)\n     A          R SCR\n     A            AMOUNT    R        B  5  2\n",
            Kind::Device,
            &files,
        )
        .unwrap();
        // With its edit code: Y (verify29 v29_crtd: 7Y 2).
        assert!(d.contains("AMOUNT         7Y 2B  5  2"), "{d}");
        assert!(
            d.contains("2B  5  2 EDTCDE(2)\n     A*@REF LIB REFF REFR AMOUNT N\n"),
            "{d}"
        );
        // Input only: no editing, so zoned (verify30 w30_crtd: ZD).
        let i = expand(
            "     A                                      REF(REFF)\n     A          R SCR\n     A            AMOUNT    R        I  5  2\n",
            Kind::Device,
            &files,
        )
        .unwrap();
        assert!(
            i.contains("AMOUNT         7S 2I  5  2") && !i.contains("EDTCDE"),
            "{i}"
        );
        // A printer file has no Y fields (DDS for printer files, data
        // type): zoned, S.
        let p = expand(
            "     A                                      REF(REFF)\n     A          R LINE\n     A            AMOUNT    R        O    +2\n",
            Kind::Printer,
            &files,
        )
        .unwrap();
        assert!(p.contains("AMOUNT         7S 2O"), "{p}");
        assert!(expand(
            "     A          R REC\n     A            X         R\n",
            Kind::Database,
            &HashMap::new()
        )
        .is_err());
    }
}
