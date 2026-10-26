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
//! | 35    | Data type (`A`, `P`, `S`, `B`, `L`, `T`, `Z`) |
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
    /// Binary: 1-18 digits, stored in 2, 4 or 8 bytes.
    #[serde(rename = "B")]
    Binary,
    /// Date (DDS L), *ISO form `YYYY-MM-DD`.
    #[serde(rename = "L")]
    Date,
    /// Time (DDS T), *ISO form `HH.MM.SS`.
    #[serde(rename = "T")]
    Time,
    /// Timestamp (DDS Z), `YYYY-MM-DD-HH.MM.SS.ffffff`.
    #[serde(rename = "Z")]
    Timestamp,
    /// Floating point (DDS F): single precision, or double with
    /// FLTPCN(*DOUBLE) (`FieldDef::double`).
    #[serde(rename = "F")]
    Float,
    /// Graphic (DDS G; SQL GRAPHIC, VARGRAPHIC, NCHAR): UCS-2 characters
    /// (CCSID 13488/1200); the length is in characters.
    #[serde(rename = "G")]
    Graphic,
    /// Hexadecimal (DDS H; SQL BINARY, VARBINARY, FOR BIT DATA): bytes the
    /// system does not translate. Records carry it as hex digits.
    #[serde(rename = "H")]
    Hex,
    /// SQL large objects. A program's externally described record leaves
    /// them out (ILE RPG: RNF7575); SQL reads and writes them.
    #[serde(rename = "CLOB")]
    Clob,
    #[serde(rename = "BLOB")]
    Blob,
    #[serde(rename = "DBCLOB")]
    Dbclob,
    /// SQL DECFLOAT(16) and DECFLOAT(34): decimal floating point, kept as
    /// an exact PostgreSQL numeric (the length is 16 or 34 digits). Like
    /// the large objects, SQL only.
    #[serde(rename = "DECFLOAT")]
    Decfloat,
    /// SQL BOOLEAN (Db2 for i 7.5). SQL only.
    #[serde(rename = "BOOLEAN")]
    Boolean,
    /// SQL XML, kept as text. SQL only.
    #[serde(rename = "XML")]
    Xml,
}

impl DataType {
    pub fn code(self) -> &'static str {
        match self {
            DataType::Char => "A",
            DataType::Packed => "P",
            DataType::Zoned => "S",
            DataType::Binary => "B",
            DataType::Date => "L",
            DataType::Time => "T",
            DataType::Timestamp => "Z",
            DataType::Float => "F",
            DataType::Hex => "H",
            DataType::Graphic => "G",
            DataType::Clob => "CLOB",
            DataType::Blob => "BLOB",
            DataType::Dbclob => "DBCLOB",
            DataType::Decfloat => "DECFLOAT",
            DataType::Boolean => "BOOLEAN",
            DataType::Xml => "XML",
        }
    }

    pub fn from_code(c: &str) -> Result<Self> {
        Ok(match c {
            "A" => DataType::Char,
            "P" => DataType::Packed,
            "S" => DataType::Zoned,
            "B" => DataType::Binary,
            "L" => DataType::Date,
            "T" => DataType::Time,
            "Z" => DataType::Timestamp,
            "F" => DataType::Float,
            "H" => DataType::Hex,
            "G" => DataType::Graphic,
            "CLOB" => DataType::Clob,
            "BLOB" => DataType::Blob,
            "DBCLOB" => DataType::Dbclob,
            "DECFLOAT" => DataType::Decfloat,
            "BOOLEAN" => DataType::Boolean,
            "XML" => DataType::Xml,
            other => {
                bail!("unsupported DDS data type '{other}' (supported: A, P, S, B, F, H, L, T, Z)")
            }
        })
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            DataType::Packed | DataType::Zoned | DataType::Binary | DataType::Float
        )
    }

    /// An SQL large object (CLOB, BLOB, DBCLOB).
    /// Columns only SQL uses: large objects, DECFLOAT, BOOLEAN, XML (a
    /// program's externally described record leaves them out).
    pub fn is_lob(self) -> bool {
        matches!(
            self,
            DataType::Clob
                | DataType::Blob
                | DataType::Dbclob
                | DataType::Decfloat
                | DataType::Boolean
                | DataType::Xml
        )
    }

    /// Date, time or timestamp.
    pub fn is_datetime(self) -> bool {
        matches!(self, DataType::Date | DataType::Time | DataType::Timestamp)
    }

    /// The length of a date, time or timestamp field in its *ISO form
    /// (DDS reference: L is 10, T is 8, Z is 26 characters).
    pub fn fixed_length(self) -> Option<u32> {
        match self {
            DataType::Date => Some(10),
            DataType::Time => Some(8),
            DataType::Timestamp => Some(26),
            _ => None,
        }
    }
}

/// A field's default value (DDS DFT, SQL DEFAULT).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldDefault {
    /// A constant, as text in the field's external form.
    Value(String),
    /// The type's own default (SQL `WITH DEFAULT` with no value): blanks,
    /// zero, or the current date, time or timestamp when the record is
    /// added.
    System,
    /// The null value.
    Null,
    /// No default: an SQL INSERT must give a value (SQL `NOT NULL` with no
    /// DEFAULT).
    Required,
    /// SQL DEFAULT USER (SESSION_USER, SYSTEM_USER): the user profile of
    /// the job that adds the row (Db2 for i SQL Reference, CREATE TABLE,
    /// default-clause).
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub data_type: DataType,
    pub length: u32,
    pub decimals: u32,
    pub text: String,
    /// Variable length (DDS VARLEN, SQL VARCHAR): up to `length` characters.
    #[serde(default)]
    pub varlen: bool,
    /// Null-capable (DDS ALWNULL, an SQL column without NOT NULL).
    #[serde(default)]
    pub null_capable: bool,
    /// Explicit default (DDS DFT, SQL DEFAULT); None means the type's.
    #[serde(default)]
    pub default: Option<FieldDefault>,
    /// The alternative (SQL) name: an SQL column name longer than a
    /// system name, or DDS ALIAS. `name` is the system name.
    #[serde(default)]
    pub alias: Option<String>,
    /// An identity column (SQL GENERATED ... AS IDENTITY).
    #[serde(default)]
    pub identity: Option<Identity>,
    /// A float of double precision (DDS FLTPCN(*DOUBLE); SQL DOUBLE).
    #[serde(default)]
    pub double: bool,
    /// A column the database sets (SQL GENERATED ... ROW CHANGE TIMESTAMP,
    /// or an auditing column).
    #[serde(default)]
    pub generated: Option<Generated>,
}

/// What a generated column holds (Db2 for i SQL Programming, "Creating
/// auditing columns"; SQL Reference, CREATE TABLE: ROW CHANGE TIMESTAMP).
/// Set for every insert and update, by SQL and by record-level access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Generated {
    /// FOR EACH ROW ON UPDATE AS ROW CHANGE TIMESTAMP: when the row was
    /// last changed.
    RowChangeTimestamp,
    /// AS (DATA CHANGE OPERATION): I for an insert, U for an update.
    DataChangeOperation,
    /// AS (QSYS2.JOB_NAME): the job that changed the row.
    JobName,
    /// AS (SESSION_USER), (USER), (CURRENT USER): its user.
    SessionUser,
    /// AS (CURRENT SERVER): the relational database.
    CurrentServer,
}

impl Generated {
    /// The name the trigger function knows it by.
    pub fn code(self) -> &'static str {
        match self {
            Generated::RowChangeTimestamp => "ROWCHG",
            Generated::DataChangeOperation => "DCO",
            Generated::JobName => "JOB",
            Generated::SessionUser => "USER",
            Generated::CurrentServer => "SERVER",
        }
    }
}

/// An identity column's generation (Db2 for i, CREATE TABLE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// GENERATED ALWAYS (else BY DEFAULT).
    pub always: bool,
    pub start: Option<i64>,
    pub increment: Option<i64>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub cycle: bool,
}

impl Identity {
    /// The PostgreSQL column clause.
    pub fn sql(&self) -> String {
        let mut opts = Vec::new();
        if let Some(n) = self.start {
            opts.push(format!("START WITH {n}"));
        }
        if let Some(n) = self.increment {
            opts.push(format!("INCREMENT BY {n}"));
        }
        if let Some(n) = self.min {
            opts.push(format!("MINVALUE {n}"));
        }
        if let Some(n) = self.max {
            opts.push(format!("MAXVALUE {n}"));
        }
        if self.cycle {
            opts.push("CYCLE".into());
        }
        format!(
            " GENERATED {} AS IDENTITY{}",
            if self.always { "ALWAYS" } else { "BY DEFAULT" },
            if opts.is_empty() {
                String::new()
            } else {
                format!(" ({})", opts.join(" "))
            }
        )
    }
}

impl FieldDef {
    /// A field with no keywords.
    pub fn new(name: &str, data_type: DataType, length: u32, decimals: u32) -> FieldDef {
        FieldDef {
            name: name.to_string(),
            data_type,
            length,
            decimals,
            text: String::new(),
            varlen: false,
            null_capable: false,
            default: None,
            alias: None,
            identity: None,
            double: false,
            generated: None,
        }
    }

    /// The PostgreSQL column holding this field: the SQL name when it has
    /// one (so SQL statements use it), else the system name; quoted.
    /// Bytes the field takes in a record buffer (what DSPFD's maximum
    /// record length adds up; as4tainr_svc.field_bytes in SQL).
    pub fn buffer_bytes(&self) -> u32 {
        let v = if self.varlen { 2 } else { 0 };
        match self.data_type {
            DataType::Char | DataType::Hex => self.length + v,
            DataType::Graphic => 2 * self.length + v,
            DataType::Packed => self.length / 2 + 1,
            DataType::Binary => match self.length {
                0..=4 => 2,
                5..=9 => 4,
                _ => 8,
            },
            DataType::Float => {
                if self.double || self.length > 9 {
                    8
                } else {
                    4
                }
            }
            DataType::Date => 10,
            DataType::Time => 8,
            DataType::Decfloat => {
                if self.length == 16 {
                    8
                } else {
                    16
                }
            }
            DataType::Boolean => 1,
            _ => self.length,
        }
    }

    pub fn column(&self) -> String {
        crate::catalog::ident(self.alias.as_deref().unwrap_or(&self.name))
    }

    /// Is this the field's system name or its alternative name?
    pub fn is_named(&self, n: &str) -> bool {
        self.name.eq_ignore_ascii_case(n)
            || self
                .alias
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(n))
    }

    /// PostgreSQL column type used to store this field.
    pub fn sql_type(&self) -> String {
        match self.data_type {
            DataType::Char | DataType::Graphic if self.varlen => {
                format!("varchar({})", self.length)
            }
            DataType::Char | DataType::Graphic => format!("char({})", self.length),
            DataType::Packed | DataType::Zoned => {
                format!("numeric({},{})", self.length, self.decimals)
            }
            // Binary with no decimals is an SQL integer: SMALLINT is
            // binary 4 digits, INTEGER 9, BIGINT 18 (Db2 for i SQL
            // reference, data types and DDS equivalents).
            DataType::Binary if self.decimals == 0 => match self.length {
                0..=4 => "smallint".into(),
                5..=9 => "integer".into(),
                _ => "bigint".into(),
            },
            DataType::Binary => format!("numeric({},{})", self.length, self.decimals),
            DataType::Date => "date".into(),
            DataType::Time => "time(0)".into(),
            // TIMESTAMP(p): 26 characters for 6 fractional digits, 19 for
            // none; PostgreSQL keeps at most 6.
            DataType::Timestamp => format!("timestamp({})", self.length.saturating_sub(20).min(6)),
            // Exact decimal, as DECFLOAT is decimal (never binary floating
            // point).
            DataType::Decfloat => "numeric".into(),
            DataType::Boolean => "boolean".into(),
            DataType::Xml => "text".into(),
            DataType::Float if self.double => "double precision".into(),
            DataType::Float => "real".into(),
            DataType::Hex | DataType::Blob => "bytea".into(),
            DataType::Clob | DataType::Dbclob => "text".into(),
        }
    }

    /// The value a program sees for this field when it holds no value of
    /// its own (blanks, zero, and the lowest date, time and timestamp),
    /// in the external form records use.
    pub fn blank_value(&self) -> String {
        match self.data_type {
            DataType::Char | DataType::Graphic if self.varlen => String::new(),
            DataType::Char | DataType::Graphic => " ".repeat(self.length as usize),
            DataType::Packed | DataType::Zoned | DataType::Binary | DataType::Float => "0".into(),
            // Hexadecimal blanks (X'40').
            DataType::Hex if !self.varlen => "40".repeat(self.length as usize),
            DataType::Hex | DataType::Clob | DataType::Blob | DataType::Dbclob | DataType::Xml => {
                String::new()
            }
            DataType::Decfloat => "0".into(),
            DataType::Boolean => "false".into(),
            DataType::Date => "0001-01-01".into(),
            DataType::Time => "00.00.00".into(),
            DataType::Timestamp => "0001-01-01-00.00.00.000000".into(),
        }
    }

    /// A value in the external form (`HH.MM.SS` times, `YYYY-MM-DD-HH.MM.SS.ffffff`
    /// timestamps) in the form PostgreSQL reads. Empty values are the
    /// field's blank value.
    pub fn to_db(&self, v: &str) -> String {
        let t = v.trim();
        match self.data_type {
            DataType::Packed
            | DataType::Zoned
            | DataType::Binary
            | DataType::Float
            | DataType::Decfloat => {
                if t.is_empty() {
                    "0".into()
                } else {
                    t.to_string()
                }
            }
            // Hex digits as a bytea constant.
            DataType::Hex | DataType::Blob => format!("\\x{t}"),
            DataType::Clob | DataType::Dbclob | DataType::Graphic | DataType::Xml => v.to_string(),
            DataType::Boolean => t.to_string(),
            DataType::Date if t.is_empty() => "0001-01-01".into(),
            DataType::Date => t.to_string(),
            DataType::Time => {
                if t.is_empty() {
                    "00:00:00".into()
                } else {
                    t.replace('.', ":")
                }
            }
            DataType::Timestamp => {
                if t.is_empty() {
                    return "0001-01-01 00:00:00".into();
                }
                // 2026-10-19-14.05.00.000000 -> 2026-10-19 14:05:00.000000
                match (t.get(..10), t.get(10..11), t.get(11..)) {
                    (Some(d), Some("-"), Some(rest)) => {
                        let mut r = rest.to_string();
                        // HH.MM.SS[.ffffff]: the first two dots become colons.
                        for _ in 0..2 {
                            if let Some(i) = r.find('.') {
                                r.replace_range(i..i + 1, ":");
                            }
                        }
                        format!("{d} {r}")
                    }
                    _ => t.to_string(),
                }
            }
            DataType::Char => v.to_string(),
        }
    }

    /// SQL expression reading this field (column `col`) as text in the
    /// external form.
    pub fn read_expr(&self, col: &str) -> String {
        let e = match self.data_type {
            // bpchar::text drops trailing blanks; programs expect them.
            DataType::Char | DataType::Graphic if !self.varlen => {
                format!("rpad({col}::text, {})", self.length)
            }
            DataType::Date => format!("to_char({col}, 'YYYY-MM-DD')"),
            DataType::Time => format!("to_char({col}, 'HH24.MI.SS')"),
            DataType::Timestamp => format!("to_char({col}, 'YYYY-MM-DD-HH24.MI.SS.US')"),
            // A float as a plain decimal number (no exponent).
            DataType::Float => format!("({col})::numeric::text"),
            DataType::Hex | DataType::Blob => format!("upper(encode({col}, 'hex'))"),
            _ => format!("{col}::text"),
        };
        if self.null_capable {
            // A program that does not handle nulls sees the default value
            // (ILE RPG Reference, null-capable fields without ALWNULL(*USRCTL)).
            format!("coalesce({e}, {})", quote(&self.blank_value()))
        } else {
            e
        }
    }

    /// The column's DEFAULT clause as SQL (empty for no default).
    /// Without an explicit default, a null-capable field defaults to null
    /// and other fields to the type's default (DDS DFT, SQL DEFAULT).
    pub fn default_clause(&self) -> String {
        match &self.default {
            Some(FieldDefault::Required) => String::new(),
            Some(FieldDefault::Null) => " DEFAULT NULL".into(),
            None if self.null_capable => " DEFAULT NULL".into(),
            Some(FieldDefault::Value(v)) => format!(
                " DEFAULT CAST({} AS {})",
                quote(&self.to_db(v)),
                self.sql_type()
            ),
            Some(FieldDefault::System) | None => format!(" DEFAULT {}", self.system_default()),
            Some(FieldDefault::User) => {
                format!(" DEFAULT CAST({} AS {})", crate::sql::USER_SQL, self.sql_type())
            }
        }
    }

    fn system_default(&self) -> String {
        if self.data_type == DataType::Hex && !self.varlen {
            return format!("'\\x{}'::bytea", "40".repeat(self.length as usize));
        }
        match self.data_type {
            DataType::Hex | DataType::Blob => "''::bytea",
            DataType::Clob | DataType::Dbclob => "''",
            // Db2 for i: the default for a date, time or timestamp is
            // the current one when the record is added (CREATE TABLE,
            // WITH DEFAULT; DDS DFT not given).
            DataType::Date => "CURRENT_DATE",
            DataType::Time => "LOCALTIME(0)",
            DataType::Timestamp => "LOCALTIMESTAMP(6)",
            DataType::Char | DataType::Graphic if self.varlen => "''",
            DataType::Char | DataType::Graphic => "' '",
            _ => "0",
        }
        .to_string()
    }
}

/// Check a numeric constant: digits with an optional sign and decimal point.
pub fn check_number(v: &str) -> Result<()> {
    let t = v.trim().trim_start_matches(['+', '-']);
    let (i, f) = t.split_once('.').unwrap_or((t, ""));
    if t.is_empty()
        || !i.chars().all(|c| c.is_ascii_digit())
        || !f.chars().all(|c| c.is_ascii_digit())
    {
        bail!("{v} is not a number");
    }
    Ok(())
}

/// A string as an SQL literal.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
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
    /// An SQL view (a logical file defined by a query): Some(true) when
    /// its rows have the record number of a single underlying table.
    #[serde(default)]
    pub view: Option<bool>,
    /// A source physical file read with record-level access: Some(member
    /// opened) (empty until it is opened). Its records SRCSEQ, SRCDAT,
    /// SRCDTA are the member's lines (source.rs).
    #[serde(default)]
    pub source: Option<String>,
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
        view: None,
        source: None,
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
                let type_txt = cols(line, 35, 35).to_ascii_uppercase();
                // Dates, times and timestamps have no length of their own
                // (DDS reference, data type L, T, Z).
                let fixed = DataType::from_code(&type_txt)
                    .ok()
                    .and_then(|t| t.fixed_length());
                let len_txt = cols(line, 30, 34);
                let length: u32 = match fixed {
                    Some(n) if len_txt.is_empty() => n,
                    _ => len_txt.parse().with_context(|| {
                        format!("line {lineno}: field '{name}' needs a length in columns 30-34")
                    })?,
                };
                let dec_txt = cols(line, 36, 37);
                let decimals: Option<u32> = if dec_txt.is_empty() {
                    None
                } else {
                    Some(dec_txt.parse().with_context(|| {
                        format!("line {lineno}: bad decimal positions '{dec_txt}'")
                    })?)
                };
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
                // Float: single precision has 1-9 digits, FLTPCN(*DOUBLE)
                // 1-17 (DDS reference, data type F).
                let double = keyword_arg(keywords, "FLTPCN")
                    .is_some_and(|a| a.trim().eq_ignore_ascii_case("*DOUBLE"));
                if data_type == DataType::Float && (length > if double { 17 } else { 9 }) {
                    bail!(
                        "line {lineno}: a float field has 1 to {} digits",
                        if double { 17 } else { 9 }
                    );
                }
                if data_type == DataType::Binary && (length > 18 || decimals > length) {
                    bail!("line {lineno}: binary fields have 1 to 18 digits");
                }
                if let Some(n) = data_type.fixed_length() {
                    if length != n || decimals != 0 {
                        bail!(
                            "line {lineno}: a {} field has no length or decimals of its own",
                            data_type.code()
                        );
                    }
                }
                if !data_type.is_numeric() && (length == 0 || length > 32766) {
                    bail!("line {lineno}: invalid character length {length}");
                }
                if pf.fields.iter().any(|f| f.name == name) {
                    bail!("line {lineno}: duplicate field '{name}'");
                }
                let varlen = has_keyword(keywords, "VARLEN");
                if varlen
                    && !matches!(
                        data_type,
                        DataType::Char | DataType::Graphic | DataType::Hex
                    )
                {
                    bail!("line {lineno}: VARLEN is only valid for character fields");
                }
                let mut f = FieldDef::new(&name, data_type, length, decimals);
                f.text = keyword_arg(keywords, "TEXT").unwrap_or_default();
                f.varlen = varlen;
                f.null_capable = has_keyword(keywords, "ALWNULL");
                f.double = double;
                // ALIAS(name): the field's alternative name (what SQL uses).
                if let Some(a) = keyword_arg(keywords, "ALIAS") {
                    let a = a.trim().to_ascii_uppercase();
                    if a.is_empty()
                        || a.len() > 30
                        || !a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        bail!("line {lineno}: ALIAS({a}) is not a valid alternative name");
                    }
                    f.alias = Some(a);
                }
                if let Some(a) = keyword_args(keywords, "DFT") {
                    let [v] = a.as_slice() else {
                        bail!("line {lineno}: DFT needs one value");
                    };
                    f.default = Some(match v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
                        Some(inner) => FieldDefault::Value(inner.replace("''", "'")),
                        None if data_type.is_numeric() => {
                            check_number(v).with_context(|| format!("line {lineno}: DFT"))?;
                            FieldDefault::Value(v.clone())
                        }
                        None => bail!("line {lineno}: DFT for a non-numeric field needs a value in apostrophes"),
                    });
                }
                pf.fields.push(f);
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

    #[test]
    fn binary_date_time_varlen_and_null_fields() {
        // DDS reference: B is binary (1-18 digits); L, T and Z take no
        // length (10, 8 and 26 characters in *ISO form); VARLEN makes a
        // character field variable length; ALWNULL makes it null-capable;
        // DFT gives its default.
        let src = "     A          R REC
     A            QTY            9B 0
     A            WHEN            L
     A            AT              T
     A            STAMP           Z
     A            NOTE          50A         VARLEN ALWNULL
     A            CODE           2A         DFT('XY')
";
        let pf = parse_pf("X", src).unwrap();
        let f = |n: &str| pf.fields.iter().find(|f| f.name == n).unwrap().clone();
        assert_eq!(f("QTY").sql_type(), "integer");
        assert_eq!(
            (f("WHEN").data_type, f("WHEN").length),
            (DataType::Date, 10)
        );
        assert_eq!(f("AT").length, 8);
        assert_eq!(f("STAMP").length, 26);
        assert!(f("NOTE").varlen && f("NOTE").null_capable);
        assert_eq!(f("NOTE").sql_type(), "varchar(50)");
        assert_eq!(f("CODE").default, Some(FieldDefault::Value("XY".into())));
        // External forms: *ISO time HH.MM.SS, timestamp YYYY-MM-DD-HH.MM.SS.ffffff.
        assert_eq!(f("AT").to_db("14.05.30"), "14:05:30");
        assert_eq!(
            f("STAMP").to_db("2026-10-19-14.05.30.123456"),
            "2026-10-19 14:05:30.123456"
        );
        assert_eq!(f("STAMP").blank_value(), "0001-01-01-00.00.00.000000");
        let e = parse_pf(
            "X",
            "     A          R REC\n     A            N             20B 0\n",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("1 to 18"), "{e}");
    }

    #[test]
    fn float_hex_graphic_fields() {
        // DDS reference, position 35: F is single-precision float unless
        // FLTPCN(*DOUBLE); H is hexadecimal; G is graphic (UCS-2).
        let src = "     A          R REC
     A            AMT           15F 2       FLTPCN(*DOUBLE)
     A            SMALL          7F
     A            CODE           4H
     A            NAME          10G         VARLEN
";
        let pf = parse_pf("X", src).unwrap();
        assert!(pf.fields[0].double && pf.fields[0].sql_type() == "double precision");
        assert_eq!(pf.fields[1].sql_type(), "real");
        assert_eq!(pf.fields[2].sql_type(), "bytea");
        assert_eq!(pf.fields[2].blank_value(), "40404040");
        assert_eq!(pf.fields[3].sql_type(), "varchar(10)");
        let e = parse_pf(
            "X",
            "     A          R REC\n     A            F1            12F\n",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("1 to 9"), "{e}");
    }
}
