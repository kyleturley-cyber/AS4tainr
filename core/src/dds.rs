//! Parser for the subset of DDS (Data Description Specifications) that
//! describes database files: physical files (one record format, fields,
//! key fields) and logical files over one physical file (a record format
//! with some or all of its fields, their own key fields, select/omit).
//!
//! Written from IBM's public DDS reference (positional A-specs). Columns
//! below are 1-based, as in the manuals:
//!
//! | Cols  | Meaning                                  |
//! |-------|------------------------------------------|
//! | 6     | Form type `A`                            |
//! | 7     | `*` marks a comment line                 |
//! | 17    | Name type: `R` record, `K` key, blank    |
//! | 19-28 | Name                                     |
//! | 30-34 | Length (right-aligned)                   |
//! | 35    | Data type (`A`, `P`, `S`)                |
//! | 36-37 | Decimal positions                        |
//! | 45-80 | Keywords (`TEXT`, `UNIQUE`, ...)         |

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    /// Character, fixed length.
    #[serde(rename = "A")]
    Char,
    /// Packed decimal.
    #[serde(rename = "P")]
    Packed,
    /// Zoned decimal.
    #[serde(rename = "S")]
    Zoned,
}

impl DataType {
    pub fn code(self) -> &'static str {
        match self {
            DataType::Char => "A",
            DataType::Packed => "P",
            DataType::Zoned => "S",
        }
    }

    pub fn from_code(c: &str) -> Result<Self> {
        Ok(match c {
            "A" => DataType::Char,
            "P" => DataType::Packed,
            "S" => DataType::Zoned,
            other => bail!("unsupported DDS data type '{other}' (supported: A, P, S)"),
        })
    }

    pub fn is_numeric(self) -> bool {
        !matches!(self, DataType::Char)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub data_type: DataType,
    pub length: u32,
    pub decimals: u32,
    pub text: String,
}

impl FieldDef {
    /// PostgreSQL column type used to store this field.
    pub fn sql_type(&self) -> String {
        match self.data_type {
            DataType::Char => format!("char({})", self.length),
            DataType::Packed | DataType::Zoned => {
                format!("numeric({},{})", self.length, self.decimals)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalFile {
    pub name: String,
    pub format: String,
    pub text: String,
    pub unique: bool,
    pub fields: Vec<FieldDef>,
    pub keys: Vec<String>,
    /// Key fields in descending order (DESCEND on the K line).
    #[serde(default)]
    pub descending: Vec<String>,
}

impl PhysicalFile {
    pub fn is_descending(&self, key: &str) -> bool {
        self.descending.iter().any(|d| d == key)
    }
}

/// A comparison in a select/omit specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Test {
    /// COMP(op value): op is EQ NE LT NL GT NG LE GE.
    Comp(String, String),
    /// VALUES(v1 v2 ...)
    Values(Vec<String>),
    /// RANGE(low high), inclusive.
    Range(String, String),
}

/// One select or omit rule: all its conditions must be true (AND).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOmit {
    /// S (select) or O (omit).
    pub select: bool,
    /// (field, test); empty for ALL.
    pub conds: Vec<(String, Test)>,
}

/// A logical file over one physical file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalFile {
    pub name: String,
    pub format: String,
    pub text: String,
    pub unique: bool,
    /// PFILE: the physical file, `FILE` or `LIB/FILE`.
    pub based_on: String,
    /// Field names; empty means all fields of the physical file.
    pub fields: Vec<String>,
    pub keys: Vec<String>,
    pub descending: Vec<String>,
    /// Rules in order; the first one whose conditions hold decides.
    pub select_omit: Vec<SelectOmit>,
}

fn cols(line: &str, from: usize, to: usize) -> &str {
    // 1-based inclusive columns; tolerate short lines.
    let start = from - 1;
    if line.len() <= start {
        return "";
    }
    let end = to.min(line.len());
    line.get(start..end).unwrap_or("").trim()
}

/// Extract the argument of `KEYWORD('...')` from a keyword area.
fn keyword_arg(area: &str, keyword: &str) -> Option<String> {
    let upper = area.to_ascii_uppercase();
    let pos = upper.find(&format!("{keyword}("))?;
    let rest = &area[pos + keyword.len() + 1..];
    let rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('\'') {
        let end = stripped.find('\'')?;
        Some(stripped[..end].to_string())
    } else {
        let end = rest.find(')')?;
        Some(rest[..end].trim().to_string())
    }
}

/// Is a keyword (with or without arguments) in a keyword area?
fn has_keyword(area: &str, keyword: &str) -> bool {
    area.to_ascii_uppercase()
        .split([' ', '('])
        .any(|w| w == keyword)
}

/// Split the arguments of `KEYWORD(a b 'c d')` into values; quoted values
/// keep their quotes so a character constant can be told from a number.
fn keyword_args(area: &str, keyword: &str) -> Option<Vec<String>> {
    let upper = area.to_ascii_uppercase();
    let start = upper.find(&format!("{keyword}("))? + keyword.len() + 1;
    let cs: Vec<char> = area[start..].chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            ' ' => i += 1,
            ')' => return Some(out),
            '\'' => {
                let mut v = String::from("'");
                i += 1;
                while i < cs.len() {
                    if cs[i] == '\'' && cs.get(i + 1) == Some(&'\'') {
                        v.push_str("''");
                        i += 2;
                    } else if cs[i] == '\'' {
                        i += 1;
                        break;
                    } else {
                        v.push(cs[i]);
                        i += 1;
                    }
                }
                v.push('\'');
                out.push(v);
            }
            _ => {
                let s = i;
                while i < cs.len() && cs[i] != ' ' && cs[i] != ')' {
                    i += 1;
                }
                out.push(cs[s..i].iter().collect());
            }
        }
    }
    None
}

/// Join DDS continuation lines: a keyword area ending in '-' continues
/// with the next line's keyword area as is; '+' skips its leading blanks.
fn join_continuations(source: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in source.lines() {
        let line = raw.trim_end_matches('\r').to_string();
        if let Some(prev) = lines.last_mut() {
            let t = prev.trim_end();
            if t.len() > 44 && (t.ends_with('-') || t.ends_with('+')) {
                let plus = t.ends_with('+');
                let keep = t.len() - 1;
                prev.truncate(keep);
                let cont = if line.len() >= 45 {
                    line[44..].trim_end()
                } else {
                    ""
                };
                prev.push_str(if plus { cont.trim_start() } else { cont });
                continue;
            }
        }
        lines.push(line);
    }
    lines
}

/// Parse DDS source for a logical file over one physical file
/// (PFILE). Written from IBM's DDS reference for logical files:
///
/// - `R format PFILE(lib/file)`; listed fields (name only) select some of
///   the physical file's fields; none listed means all of them
/// - `K field` with optional DESCEND
/// - select/omit: `S` or `O` in column 17 starts a rule; further lines
///   with a field name and a blank column 17 are ANDed to it. Tests are
///   COMP(op value), VALUES(...), RANGE(low high); `S`/`O` with ALL is the
///   default rule. Rules are tried in order and the first that holds
///   decides; when none holds, a record is omitted if the last rule was a
///   select and selected if it was an omit.
///
/// Not supported yet: join files (JFILE), several record formats, RENAME,
/// CONCAT, SST, field attribute overrides, DYNSLT.
pub fn parse_lf(file_name: &str, source: &str) -> Result<LogicalFile> {
    let mut lf = LogicalFile {
        name: file_name.to_ascii_uppercase(),
        format: String::new(),
        text: String::new(),
        unique: false,
        based_on: String::new(),
        fields: Vec::new(),
        keys: Vec::new(),
        descending: Vec::new(),
        select_omit: Vec::new(),
    };
    for (idx, line) in join_continuations(source).iter().enumerate() {
        let lineno = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        // An asterisk in position 7 makes a comment line; the form type A
        // in position 6 is optional (DDS reference, positions 1-7).
        if cols(line, 7, 7) == "*" {
            continue;
        }
        if !cols(line, 6, 6).eq_ignore_ascii_case("A") && !cols(line, 6, 6).trim().is_empty() {
            bail!("line {lineno}: expected form type 'A' in column 6");
        }
        let name_type = cols(line, 17, 17).to_ascii_uppercase();
        let name = cols(line, 19, 28).to_ascii_uppercase();
        let keywords = if line.len() >= 45 {
            line[44..].trim()
        } else {
            ""
        };
        let test = |lineno: usize| -> Result<Option<Test>> {
            if let Some(a) = keyword_args(keywords, "COMP") {
                let [op, v] = a.as_slice() else {
                    bail!("line {lineno}: COMP needs an operator and a value");
                };
                let op = op.to_ascii_uppercase();
                if !["EQ", "NE", "LT", "NL", "GT", "NG", "LE", "GE"].contains(&op.as_str()) {
                    bail!("line {lineno}: COMP operator {op} is not valid");
                }
                return Ok(Some(Test::Comp(op, v.clone())));
            }
            if let Some(a) = keyword_args(keywords, "VALUES") {
                if a.is_empty() {
                    bail!("line {lineno}: VALUES needs at least one value");
                }
                return Ok(Some(Test::Values(a)));
            }
            if let Some(a) = keyword_args(keywords, "RANGE") {
                let [lo, hi] = a.as_slice() else {
                    bail!("line {lineno}: RANGE needs a low and a high value");
                };
                return Ok(Some(Test::Range(lo.clone(), hi.clone())));
            }
            Ok(None)
        };
        match name_type.as_str() {
            "R" => {
                if !lf.format.is_empty() {
                    bail!("line {lineno}: only one record format per logical file is supported");
                }
                if name.is_empty() {
                    bail!("line {lineno}: record format name missing");
                }
                lf.format = name;
                lf.based_on = keyword_arg(keywords, "PFILE")
                    .map(|p| p.to_ascii_uppercase())
                    .ok_or_else(|| anyhow::anyhow!("line {lineno}: PFILE is required (join logical files are not supported yet)"))?;
                if lf.based_on.split_whitespace().count() > 1 {
                    bail!("line {lineno}: a logical file over several physical files is not supported yet");
                }
                if let Some(t) = keyword_arg(keywords, "TEXT") {
                    lf.text = t;
                }
            }
            "K" => {
                if name.is_empty() {
                    bail!("line {lineno}: key field name missing");
                }
                if has_keyword(keywords, "DESCEND") {
                    lf.descending.push(name.clone());
                }
                lf.keys.push(name);
            }
            "S" | "O" => {
                let select = name_type == "S";
                if has_keyword(keywords, "ALL") && name.is_empty() {
                    lf.select_omit.push(SelectOmit {
                        select,
                        conds: vec![],
                    });
                    continue;
                }
                if name.is_empty() {
                    bail!("line {lineno}: select/omit needs a field name or ALL");
                }
                let Some(t) = test(lineno)? else {
                    bail!("line {lineno}: select/omit field {name} needs COMP, VALUES or RANGE");
                };
                lf.select_omit.push(SelectOmit {
                    select,
                    conds: vec![(name, t)],
                });
            }
            "" if name.is_empty() => {
                if lf.format.is_empty() {
                    if has_keyword(keywords, "UNIQUE") {
                        lf.unique = true;
                    }
                } else if lf.fields.is_empty() && lf.keys.is_empty() {
                    // A keyword line for the record format.
                    if let Some(t) = keyword_arg(keywords, "TEXT") {
                        lf.text = t;
                    }
                }
            }
            "" => {
                if lf.format.is_empty() {
                    bail!(
                        "line {lineno}: field '{name}' appears before the record format (R) line"
                    );
                }
                if let Some(t) = test(lineno)? {
                    // A condition ANDed to the select/omit rule above.
                    let Some(rule) = lf.select_omit.last_mut() else {
                        bail!(
                            "line {lineno}: condition on {name} without an S or O line before it"
                        );
                    };
                    rule.conds.push((name, t));
                    continue;
                }
                if !lf.keys.is_empty() {
                    bail!("line {lineno}: field '{name}' after the key fields");
                }
                if !cols(line, 30, 37).is_empty() {
                    bail!("line {lineno}: field '{name}': changing a field's length or type in a logical file is not supported yet");
                }
                if lf.fields.contains(&name) {
                    bail!("line {lineno}: duplicate field '{name}'");
                }
                lf.fields.push(name);
            }
            other => bail!("line {lineno}: unsupported name type '{other}'"),
        }
    }
    if lf.format.is_empty() {
        bail!("no record format (R) line found");
    }
    if lf.unique && lf.keys.is_empty() {
        bail!("UNIQUE needs key fields");
    }
    Ok(lf)
}

/// Parse DDS source for a physical file. `file_name` is the object name
/// (DDS source does not contain it).
pub fn parse_pf(file_name: &str, source: &str) -> Result<PhysicalFile> {
    let mut pf = PhysicalFile {
        name: file_name.to_ascii_uppercase(),
        format: String::new(),
        text: String::new(),
        unique: false,
        fields: Vec::new(),
        keys: Vec::new(),
        descending: Vec::new(),
    };

    for (idx, raw) in source.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        // An asterisk in position 7 makes a comment line; the form type A
        // in position 6 is optional (DDS reference, positions 1-7).
        if cols(line, 7, 7) == "*" {
            continue;
        }
        if !cols(line, 6, 6).eq_ignore_ascii_case("A") && !cols(line, 6, 6).trim().is_empty() {
            bail!("line {lineno}: expected form type 'A' in column 6");
        }
        let name_type = cols(line, 17, 17).to_ascii_uppercase();
        let name = cols(line, 19, 28).to_ascii_uppercase();
        let keywords = cols(line, 45, 80);

        match name_type.as_str() {
            "R" => {
                if !pf.format.is_empty() {
                    bail!("line {lineno}: only one record format per physical file is supported");
                }
                if name.is_empty() {
                    bail!("line {lineno}: record format name missing");
                }
                pf.format = name;
                if let Some(t) = keyword_arg(keywords, "TEXT") {
                    pf.text = t;
                }
            }
            "K" => {
                if name.is_empty() {
                    bail!("line {lineno}: key field name missing");
                }
                if has_keyword(keywords, "DESCEND") {
                    pf.descending.push(name.clone());
                }
                pf.keys.push(name);
            }
            "" if name.is_empty() => {
                // File-level keyword line, before the record format.
                if keywords.to_ascii_uppercase().contains("UNIQUE") {
                    pf.unique = true;
                }
            }
            "" => {
                if pf.format.is_empty() {
                    bail!(
                        "line {lineno}: field '{name}' appears before the record format (R) line"
                    );
                }
                let length: u32 = cols(line, 30, 34).parse().with_context(|| {
                    format!("line {lineno}: field '{name}' needs a length in columns 30-34")
                })?;
                let dec_txt = cols(line, 36, 37);
                let decimals: Option<u32> = if dec_txt.is_empty() {
                    None
                } else {
                    Some(dec_txt.parse().with_context(|| {
                        format!("line {lineno}: bad decimal positions '{dec_txt}'")
                    })?)
                };
                let type_txt = cols(line, 35, 35).to_ascii_uppercase();
                let data_type = if type_txt.is_empty() {
                    // DDS default for physical files: packed when decimals given.
                    if decimals.is_some() {
                        DataType::Packed
                    } else {
                        DataType::Char
                    }
                } else {
                    DataType::from_code(&type_txt).with_context(|| format!("line {lineno}"))?
                };
                let decimals = decimals.unwrap_or(0);
                if data_type.is_numeric() && (length == 0 || length > 63 || decimals > length) {
                    bail!("line {lineno}: invalid numeric size {length},{decimals}");
                }
                if !data_type.is_numeric() && (length == 0 || length > 32766) {
                    bail!("line {lineno}: invalid character length {length}");
                }
                if pf.fields.iter().any(|f| f.name == name) {
                    bail!("line {lineno}: duplicate field '{name}'");
                }
                pf.fields.push(FieldDef {
                    name,
                    data_type,
                    length,
                    decimals,
                    text: keyword_arg(keywords, "TEXT").unwrap_or_default(),
                });
            }
            other => bail!("line {lineno}: unsupported name type '{other}'"),
        }
    }

    if pf.format.is_empty() {
        bail!("no record format (R) line found");
    }
    if pf.fields.is_empty() {
        bail!("record format {} has no fields", pf.format);
    }
    for k in &pf.keys {
        if !pf.fields.iter().any(|f| &f.name == k) {
            bail!("key field '{k}' is not a field of the record format");
        }
    }
    Ok(pf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUSTMAST: &str = "
     A* Customer master
     A                                      UNIQUE
     A          R CUSTREC                   TEXT('Customer master')
     A            CUSNO          5P 0       TEXT('Customer number')
     A            CUSNAM        30A         TEXT('Name')
     A            CUSCTY        20
     A            CUSBAL         9P 2
     A            CUSZON         3S 0
     A          K CUSNO
";

    #[test]
    fn comment_lines_without_form_type() {
        // DDS reference: an asterisk in position 7 makes a comment line, and
        // the form type A in position 6 is optional.
        let src = "      * a comment, no form type\n     A*  a comment with A\n                R REC\n     A            F1             5A\n";
        let pf = parse_pf("T", src).unwrap();
        assert_eq!(pf.format, "REC");
        assert_eq!(pf.fields.len(), 1);
        assert!(parse_pf("T", "     X          R REC\n").is_err());
    }

    #[test]
    fn parses_custmast() {
        let pf = parse_pf("custmast", CUSTMAST).unwrap();
        assert_eq!(pf.name, "CUSTMAST");
        assert_eq!(pf.format, "CUSTREC");
        assert_eq!(pf.text, "Customer master");
        assert!(pf.unique);
        assert_eq!(pf.keys, vec!["CUSNO"]);
        assert_eq!(pf.fields.len(), 5);
        assert_eq!(pf.fields[0].sql_type(), "numeric(5,0)");
        assert_eq!(pf.fields[1].sql_type(), "char(30)");
        assert_eq!(pf.fields[2].data_type, DataType::Char);
        assert_eq!(pf.fields[3].decimals, 2);
        assert_eq!(pf.fields[4].data_type, DataType::Zoned);
        assert_eq!(pf.fields[1].text, "Name");
    }

    // IBM DDS reference, logical files: PFILE, listed fields, K with
    // DESCEND, select/omit with COMP/VALUES/RANGE and ALL; lines without
    // S/O after an S/O line are ANDed to it.
    const CUSTOWE: &str = "
     A* Customers who owe money, largest balance first
     A                                      UNIQUE
     A          R CUSTOWER                  PFILE(CUSTMAST)
     A                                      TEXT('Customers who owe')
     A            CUSNO
     A            CUSNAM
     A            CUSBAL
     A          K CUSBAL                    DESCEND
     A          K CUSNO
     A          O CUSCTY                    VALUES('Omaha' 'Lincoln')
     A            CUSBAL                    COMP(LT 1000)
     A          S CUSBAL                    COMP(GT 0)
     A          S CUSNO                     RANGE(100 199)
";

    #[test]
    fn parses_logical_file() {
        let lf = parse_lf("custowe", CUSTOWE).unwrap();
        assert_eq!(
            (lf.name.as_str(), lf.format.as_str()),
            ("CUSTOWE", "CUSTOWER")
        );
        assert_eq!(lf.based_on, "CUSTMAST");
        assert_eq!(lf.text, "Customers who owe");
        assert!(lf.unique);
        assert_eq!(lf.fields, ["CUSNO", "CUSNAM", "CUSBAL"]);
        assert_eq!(lf.keys, ["CUSBAL", "CUSNO"]);
        assert_eq!(lf.descending, ["CUSBAL"]);
        assert_eq!(lf.select_omit.len(), 3);
        assert_eq!(
            lf.select_omit[0],
            SelectOmit {
                select: false,
                conds: vec![
                    (
                        "CUSCTY".into(),
                        Test::Values(vec!["'Omaha'".into(), "'Lincoln'".into()])
                    ),
                    ("CUSBAL".into(), Test::Comp("LT".into(), "1000".into())),
                ]
            }
        );
        assert_eq!(
            lf.select_omit[2].conds[0].1,
            Test::Range("100".into(), "199".into())
        );
    }

    #[test]
    fn logical_file_errors() {
        let e = |src: &str| parse_lf("X", src).unwrap_err().to_string();
        assert!(e("     A          R REC\n").contains("PFILE"));
        assert!(e("     A          R REC                       PFILE(P)\n     A          S F1                        COMP(XX 1)\n").contains("not valid"));
        assert!(e("     A          R REC                       PFILE(P)\n     A            F1             5A\n").contains("not supported"));
        assert!(
            e("     A          R REC                       PFILE(P1 P2)\n").contains("several")
        );
        // A physical file key can be DESCEND too.
        let pf = parse_pf("P", "     A          R REC\n     A            F1             5A\n     A          K F1                        DESCEND\n").unwrap();
        assert!(pf.is_descending("F1"));
    }

    #[test]
    fn rejects_unknown_key() {
        let src =
            "     A          R REC\n     A            F1             5A\n     A          K NOPE\n";
        assert!(parse_pf("X", src).is_err());
    }

    #[test]
    fn rejects_bad_type() {
        let src = "     A          R REC\n     A            F1             5L\n";
        let err = parse_pf("X", src).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }
}
