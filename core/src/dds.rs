//! Parser for the subset of DDS (Data Description Specifications) that
//! describes physical files: one record format, fields, and key fields.
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
    };

    for (idx, raw) in source.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        if !cols(line, 6, 6).eq_ignore_ascii_case("A") {
            bail!("line {lineno}: expected form type 'A' in column 6");
        }
        if cols(line, 7, 7) == "*" {
            continue;
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
