//! CPYTOIMPF and CPYFRMIMPF: a database file's records to and from
//! delimited text in a stream file (comma-separated values and the like).
//!
//! From IBM's CL reference for the two commands: fields separated by the
//! field delimiter (FLDDLM, a comma), character fields between string
//! delimiters (STRDLM, a quotation mark; one inside a value is doubled,
//! STRESCCHR(*STRDLM)), numbers with the decimal point DECPNT, dates and
//! times in ISO form, a record delimiter (RCDDLM) after each record, a null
//! field as nothing between delimiters; ADDCOLNAM puts the field names
//! first, RMVBLANK removes blanks around character values. CPYFRMIMPF
//! reads the same form (FROMRCD or RMVCOLNAM skip records). What IBM i
//! writes in the details these leave open (a number's form, blanks in
//! character fields by default) is queued for a capture (verify36);
//! docs/fidelity-gaps.md.

use anyhow::{bail, Result};

use crate::dds::{DataType, FieldDef};

/// Blanks to remove from character values (RMVBLANK).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Blanks {
    None,
    Leading,
    Trailing,
    Both,
}

/// The delimiters of the text.
#[derive(Debug, Clone)]
pub struct Form {
    /// STRDLM: None for *NONE.
    pub strdlm: Option<char>,
    /// STRESCCHR: the character put before a string delimiter inside a
    /// value (the delimiter itself: doubled); None for *NONE.
    pub escape: Option<char>,
    pub flddlm: String,
    pub decpnt: char,
    pub blanks: Blanks,
}

impl Default for Form {
    fn default() -> Self {
        Form {
            strdlm: Some('"'),
            escape: Some('"'),
            flddlm: ",".into(),
            decpnt: '.',
            blanks: Blanks::None,
        }
    }
}

/// Is a field written between string delimiters?
fn is_text(f: &FieldDef) -> bool {
    matches!(
        f.data_type,
        DataType::Char | DataType::Graphic | DataType::Clob | DataType::Dbclob
    )
}

/// One record as delimited text: the field values as records carry them
/// (`rla::Record`), `None` for a null field.
pub fn format_record(fields: &[FieldDef], values: &[Option<String>], form: &Form) -> String {
    let mut out = Vec::with_capacity(fields.len());
    for (f, v) in fields.iter().zip(values) {
        let Some(v) = v else {
            out.push(String::new());
            continue;
        };
        if is_text(f) {
            let v = match form.blanks {
                Blanks::None => v.as_str(),
                Blanks::Leading => v.trim_start_matches(' '),
                Blanks::Trailing => v.trim_end_matches(' '),
                Blanks::Both => v.trim_matches(' '),
            };
            match form.strdlm {
                Some(d) => {
                    let mut s = String::with_capacity(v.len() + 2);
                    s.push(d);
                    for c in v.chars() {
                        if c == d {
                            if let Some(e) = form.escape {
                                s.push(e);
                            }
                        }
                        s.push(c);
                    }
                    s.push(d);
                    out.push(s);
                }
                None => out.push(v.to_string()),
            }
        } else if matches!(
            f.data_type,
            DataType::Packed
                | DataType::Zoned
                | DataType::Binary
                | DataType::Float
                | DataType::Decfloat
        ) {
            let n = v.trim();
            out.push(if form.decpnt == '.' {
                n.to_string()
            } else {
                n.replace('.', &form.decpnt.to_string())
            });
        } else {
            out.push(v.trim_end().to_string());
        }
    }
    out.join(&form.flddlm)
}

/// The field names as a first record (ADDCOLNAM): system names or SQL
/// names, each between string delimiters.
pub fn header(fields: &[FieldDef], sql: bool, form: &Form) -> String {
    fields
        .iter()
        .map(|f| {
            let n = if sql {
                f.alias.clone().unwrap_or_else(|| f.name.clone())
            } else {
                f.name.clone()
            };
            match form.strdlm {
                Some(d) => format!("{d}{n}{d}"),
                None => n,
            }
        })
        .collect::<Vec<_>>()
        .join(&form.flddlm)
}

/// One line of delimited text as values: a value between string
/// delimiters is text (a doubled delimiter inside it is one); a value
/// with nothing in it is None (null, or the field's default).
pub fn parse_line(line: &str, form: &Form) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let chars: Vec<char> = line.chars().collect();
    let fd: Vec<char> = form.flddlm.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if quoted {
            if Some(c) == form.strdlm {
                if form.escape == form.strdlm && chars.get(i + 1) == Some(&c) {
                    cur.push(c);
                    i += 2;
                    continue;
                }
                quoted = false;
                i += 1;
                continue;
            }
            if form.escape.is_some()
                && Some(c) == form.escape
                && form.escape != form.strdlm
                && chars.get(i + 1).copied() == form.strdlm
            {
                cur.push(chars[i + 1]);
                i += 2;
                continue;
            }
            cur.push(c);
            i += 1;
            continue;
        }
        if Some(c) == form.strdlm {
            quoted = true;
            was_quoted = true;
            i += 1;
            continue;
        }
        if !fd.is_empty() && chars[i..].starts_with(&fd) {
            out.push(take(&mut cur, &mut was_quoted));
            i += fd.len();
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push(take(&mut cur, &mut was_quoted));
    out
}

fn take(cur: &mut String, was_quoted: &mut bool) -> Option<String> {
    let v = std::mem::take(cur);
    let q = std::mem::replace(was_quoted, false);
    if !q && v.trim().is_empty() {
        None
    } else {
        Some(v)
    }
}

/// A value read from the text as the record's value for a field (the
/// form `rla` writes): character values cut to the field's length, a
/// number with its decimal point made a period. Err for a number that is
/// not one.
pub fn field_value(f: &FieldDef, v: &str, form: &Form) -> Result<String> {
    Ok(match f.data_type {
        DataType::Packed
        | DataType::Zoned
        | DataType::Binary
        | DataType::Decfloat
        | DataType::Float => {
            let t = v.trim().replace(form.decpnt, ".");
            let body = t.trim_start_matches(['+', '-']);
            if body.is_empty()
                || !body.chars().all(|c| c.is_ascii_digit() || c == '.')
                || body.matches('.').count() > 1
            {
                bail!("'{v}' is not a number for field {}.", f.name);
            }
            t.trim_start_matches('+').to_string()
        }
        DataType::Char | DataType::Graphic if !f.varlen => {
            v.chars().take(f.length as usize).collect()
        }
        _ => v.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, t: DataType, len: u32, dec: u32) -> FieldDef {
        FieldDef::new(name, t, len, dec)
    }

    // CL reference, CPYTOIMPF/CPYFRMIMPF: comma between fields, quotation
    // marks around character fields (one inside doubled), null as nothing.
    #[test]
    fn records_to_text_and_back() {
        let fields = vec![
            field("NAME", DataType::Char, 10, 0),
            field("BAL", DataType::Packed, 9, 2),
            field("SINCE", DataType::Date, 10, 0),
            field("NOTE", DataType::Char, 12, 0),
        ];
        let form = Form::default();
        let line = format_record(
            &fields,
            &[
                Some("Say \"hi\"  ".into()),
                Some("-65.00".into()),
                Some("2026-11-02".into()),
                None,
            ],
            &form,
        );
        assert_eq!(line, "\"Say \"\"hi\"\"  \",-65.00,2026-11-02,");
        let back = parse_line(&line, &form);
        assert_eq!(
            back,
            [
                Some("Say \"hi\"  ".to_string()),
                Some("-65.00".into()),
                Some("2026-11-02".into()),
                None
            ]
        );
        let trimmed = Form {
            blanks: Blanks::Trailing,
            flddlm: ";".into(),
            decpnt: ',',
            ..Form::default()
        };
        assert_eq!(
            format_record(
                &fields[..2],
                &[Some("ab  ".into()), Some("1.50".into())],
                &trimmed
            ),
            "\"ab\";1,50"
        );
        assert_eq!(header(&fields[..2], false, &form), "\"NAME\",\"BAL\"");
    }

    #[test]
    fn values_for_fields() {
        let form = Form::default();
        let n = field("N", DataType::Zoned, 5, 2);
        assert_eq!(field_value(&n, " +12.5 ", &form).unwrap(), "12.5");
        assert!(field_value(&n, "1x", &form).is_err());
        let c = field("C", DataType::Char, 3, 0);
        assert_eq!(field_value(&c, "abcdef", &form).unwrap(), "abc");
    }
}
