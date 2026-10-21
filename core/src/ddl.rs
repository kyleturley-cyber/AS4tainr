//! SQL data definition: CREATE TABLE, CREATE INDEX, DROP, LABEL ON,
//! COMMENT ON and SET SCHEMA, in the Db2 for i dialect.
//!
//! Written from the Db2 for i SQL Reference. A table is a physical file and
//! an index a keyed logical file over it, in the same catalog as files
//! created from DDS: programs read them with record-level access like any
//! other file.
//!
//! Statements are parsed here, never passed on: the PostgreSQL objects are
//! built from the parsed description with quoted identifiers and quoted
//! constants only (see CLAUDE.md, conventions). CHECK constraints are
//! limited to comparisons of the table's columns with constants.

use anyhow::{anyhow, bail, Result};
use tokio_postgres::Client;

use crate::authority::{self, Aut};
use crate::catalog::{self, ident, validate_name};
use crate::dds::{
    check_number, quote, DataType, FieldDef, FieldDefault, LogicalFile, PhysicalFile,
};
use crate::sql::{tokenize, Tok};

/// PRIMARY KEY and UNIQUE constraints: (constraint name, columns).
pub type Uniques = Vec<(String, Vec<String>)>;
/// CHECK constraints: (constraint name, condition as SQL).
pub type Checks = Vec<(String, String)>;

/// An object name, qualified (`LIB.NAME` or `LIB/NAME`) or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub lib: Option<String>,
    pub name: String,
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.lib {
            Some(l) => write!(f, "{l}/{}", self.name),
            None => write!(f, "{}", self.name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ddl {
    CreateTable {
        name: Name,
        /// FOR SYSTEM NAME
        system: Option<String>,
        replace: bool,
        file: PhysicalFile,
        /// PRIMARY KEY and UNIQUE constraints: (constraint name, columns).
        uniques: Vec<(String, Vec<String>)>,
        /// CHECK constraints: (constraint name, condition as SQL).
        checks: Vec<(String, String)>,
    },
    CreateIndex {
        name: Name,
        /// FOR SYSTEM NAME
        system: Option<String>,
        table: Name,
        unique: bool,
        /// (column, descending)
        keys: Vec<(String, bool)>,
        format: Option<String>,
    },
    Drop {
        /// TABLE, INDEX or VIEW
        kind: &'static str,
        name: Name,
        if_exists: bool,
    },
    CreateView {
        name: Name,
        /// FOR SYSTEM NAME
        system: Option<String>,
        replace: bool,
        /// Column names given after the view name.
        columns: Vec<String>,
        /// The query, as written.
        query: String,
        check_option: bool,
        format: Option<String>,
    },
    AlterTable {
        table: Name,
        action: Alter,
    },
    /// LABEL ON TABLE / INDEX / VIEW: the object's text.
    LabelObject {
        name: Name,
        text: String,
    },
    /// LABEL ON COLUMN with TEXT IS: the fields' text. Column headings
    /// (plain `IS`) are not kept.
    LabelColumns {
        table: Name,
        texts: Vec<(String, String)>,
    },
    /// CREATE TABLE ... AS (query) WITH [NO] DATA: a table whose columns
    /// are the query's.
    CreateTableAs {
        name: Name,
        system: Option<String>,
        replace: bool,
        /// Column names given after the table name.
        columns: Vec<String>,
        query: String,
        with_data: bool,
        format: Option<String>,
    },
    /// COMMENT ON: accepted, not kept.
    Comment,
    /// SET SCHEMA: the default library for unqualified names.
    SetSchema(String),
}

/// Is this statement data definition? `Ok(None)` for other statements
/// (queries and changes, which `sql::run` handles); an error for data
/// definition that is not supported yet.
pub fn parse(sql: &str) -> Result<Option<Ddl>> {
    let toks: Vec<Tok> = tokenize(sql)?
        .into_iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut p = P { t: toks, i: 0 };
    let first = p.word().unwrap_or_default();
    let ddl = match first.as_str() {
        "CREATE" => {
            p.i += 1;
            let replace = p.eat("OR");
            if replace {
                p.expect("REPLACE")?;
            }
            let unique = p.eat("UNIQUE");
            match p.word().as_deref() {
                Some("TABLE") if !unique => {
                    // CREATE TABLE name [(columns)] AS (query) WITH DATA.
                    let words = top_words(sql);
                    if let Some(k) = words.iter().position(|(_, w)| w == "TABLE") {
                        if words[k + 1..].iter().any(|(_, w)| w == "AS") {
                            return create_table_as(sql, replace, k, &words).map(Some);
                        }
                    }
                    p.i += 1;
                    create_table(&mut p, replace)?
                }
                Some("INDEX") if !replace => {
                    p.i += 1;
                    create_index(&mut p, unique)?
                }
                Some("VIEW") if !unique => {
                    // The query is taken from the statement's text.
                    return create_view(sql, replace).map(Some);
                }
                Some(other) => {
                    bail!("SQL0084: CREATE {other} is not supported yet.")
                }
                None => bail!("SQL0104: CREATE needs an object type."),
            }
        }
        "DROP" => {
            p.i += 1;
            let kind = match p.word().as_deref() {
                Some("TABLE") => "TABLE",
                Some("INDEX") => "INDEX",
                Some("VIEW") => "VIEW",
                Some(other) => bail!("SQL0084: DROP {other} is not supported yet."),
                None => bail!("SQL0104: DROP needs an object type."),
            };
            p.i += 1;
            let if_exists = p.eat("IF");
            if if_exists {
                p.expect("EXISTS")?;
            }
            // as4tainr creates no objects with long SQL names, so with IF
            // EXISTS such a name is simply not there.
            let save = p.i;
            let name = match p.name() {
                Err(_) if if_exists => {
                    p.i = save;
                    let n = p.ident()?;
                    if p.punct(".") || p.punct("/") {
                        Name {
                            lib: Some(n),
                            name: p.ident()?,
                        }
                    } else {
                        Name { lib: None, name: n }
                    }
                }
                r => r?,
            };
            // RESTRICT/CASCADE: dependent indexes are dropped either way.
            let _ = p.eat("CASCADE") || p.eat("RESTRICT");
            Ddl::Drop {
                kind,
                name,
                if_exists,
            }
        }
        "LABEL" => {
            p.i += 1;
            p.expect("ON")?;
            label(&mut p)?
        }
        "COMMENT" => {
            // The rest is not looked at: COMMENT ON is accepted as is.
            return Ok(Some(Ddl::Comment));
        }
        "SET" => {
            p.i += 1;
            let current = p.eat("CURRENT");
            if !p.eat("SCHEMA") {
                if current {
                    p.i -= 1;
                }
                p.i -= 1;
                return Ok(None);
            }
            let _ = p.punct("=");
            let schema = match p.next() {
                Some(Tok::Word(w)) => w.to_ascii_uppercase(),
                Some(Tok::Quoted(q)) => q,
                Some(Tok::Str(s)) => s.to_ascii_uppercase(),
                _ => bail!("SQL0104: SET SCHEMA needs a schema name."),
            };
            Ddl::SetSchema(validate_name("library", &schema).map_err(sql0104)?)
        }
        "ALTER" => {
            p.i += 1;
            p.expect("TABLE")?;
            alter_table(&mut p)?
        }
        "RENAME" | "TRUNCATE" | "GRANT" | "REVOKE" => {
            bail!("SQL0084: {first} is not supported yet.")
        }
        _ => return Ok(None),
    };
    if let Some(t) = p.t.get(p.i) {
        bail!("SQL0104: Token {} not valid here.", show(t));
    }
    Ok(Some(ddl))
}

/// A data type as SQL writes it (`DECIMAL(9, 2)`, `VARCHAR(50)`): the
/// field it describes (SQL routines' parameters and variables).
pub fn parse_type(text: &str) -> Result<FieldDef> {
    let toks: Vec<Tok> = tokenize(text)?
        .into_iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut p = P { t: toks, i: 0 };
    let (t, len, dec, varlen) = data_type(&mut p)?;
    let mut f = FieldDef::new("V", t, len, dec);
    f.double = t == DataType::Float && len > 9;
    f.varlen = varlen;
    Ok(f)
}

/// An SQL name (Db2 for i: up to 128 characters). Names with lower case
/// letters or special characters (quoted names) are not supported yet.
fn sql_name(kind: &str, n: &str) -> Result<String> {
    let ok = !n.is_empty()
        && n.len() <= 128
        && n.chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || "$#@".contains(c))
        && n.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_$#@".contains(c));
    if !ok {
        bail!("SQL0084: {kind} name \"{n}\": names with lower case letters or special characters are not supported yet.");
    }
    Ok(n.to_string())
}

/// A generated system name (Db2 for i, rules for system names of SQL
/// objects and columns): the first five characters of the SQL name and a
/// five-digit number that makes it unique, e.g. CUSTO00001.
pub fn generated_name(sql: &str, taken: &dyn Fn(&str) -> bool) -> Result<String> {
    let prefix: String = sql.chars().take(5).collect();
    for n in 1..=99999u32 {
        let cand = format!("{prefix}{n:05}");
        if !taken(&cand) {
            return Ok(cand);
        }
    }
    bail!("SQL0084: no system name is left for {sql}.")
}

fn sql0104(e: anyhow::Error) -> anyhow::Error {
    anyhow!("SQL0104: {e}.")
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
        Tok::Str(s) => quote(s),
        Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn word(&self) -> Option<String> {
        match self.t.get(self.i) {
            Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }

    fn eat(&mut self, w: &str) -> bool {
        if self.word().as_deref() == Some(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, w: &str) -> Result<()> {
        if self.eat(w) {
            Ok(())
        } else {
            bail!("SQL0104: {w} expected{}.", self.at())
        }
    }

    fn at(&self) -> String {
        match self.t.get(self.i) {
            Some(t) => format!(" at {}", show(t)),
            None => " at the end of the statement".into(),
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Punct(x)) if x == p)
    }

    fn punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Result<()> {
        if self.punct(p) {
            Ok(())
        } else {
            bail!("SQL0104: {p} expected{}.", self.at())
        }
    }

    /// A column or object name: upper case unless it was quoted.
    fn ident(&mut self) -> Result<String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Word(w)) => {
                self.i += 1;
                Ok(w.to_ascii_uppercase())
            }
            Some(Tok::Quoted(q)) => {
                self.i += 1;
                Ok(q)
            }
            _ => bail!("SQL0104: a name is expected{}.", self.at()),
        }
    }

    /// A column name as written: an SQL name (up to 128 characters).
    fn field(&mut self) -> Result<String> {
        let n = self.ident()?;
        sql_name("column", &n)
    }

    /// A system name (10 characters at most): FOR COLUMN, FOR SYSTEM
    /// NAME, RCDFMT.
    fn system_name(&mut self, kind: &str) -> Result<String> {
        let n = self.ident()?;
        validate_name(kind, &n).map_err(sql0104)
    }

    /// `name`, `lib.name` or `lib/name`; the name can be an SQL name.
    fn name(&mut self) -> Result<Name> {
        let first = self.ident()?;
        let (lib, name) = if self.punct(".") || self.punct("/") {
            (Some(first), self.ident()?)
        } else {
            (None, first)
        };
        Ok(Name {
            lib: lib
                .map(|l| {
                    validate_name("schema", &l).map_err(|_| {
                        anyhow!("SQL0084: schema name {l}: schema names longer than 10 characters are not supported yet.")
                    })
                })
                .transpose()?,
            name: sql_name("object", &name)?,
        })
    }

    fn number(&mut self) -> Result<u32> {
        match self.next() {
            Some(Tok::Number(n)) => n
                .parse()
                .map_err(|_| anyhow!("SQL0104: {n} is not a valid length here.")),
            other => bail!(
                "SQL0104: a number is expected{}.",
                other
                    .map(|t| format!(" at {}", show(&t)))
                    .unwrap_or_default()
            ),
        }
    }

    fn string(&mut self) -> Result<String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(s),
            _ => bail!("SQL0104: a string constant is expected."),
        }
    }

    /// The tokens up to the closing parenthesis that matches one already
    /// read; the parenthesis is consumed.
    fn group(&mut self) -> Result<Vec<Tok>> {
        let mut depth = 1;
        let mut out = Vec::new();
        loop {
            let Some(t) = self.next() else {
                bail!("SQL0104: a closing parenthesis is missing.");
            };
            match &t {
                Tok::Punct(p) if p == "(" => depth += 1,
                Tok::Punct(p) if p == ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(out);
                    }
                }
                _ => {}
            }
            out.push(t);
        }
    }
}

/// The data type of a column (Db2 for i SQL Reference, data types and
/// their DDS equivalents): (type, length or digits, decimals, varying).
fn data_type(p: &mut P) -> Result<(DataType, u32, u32, bool)> {
    let Some(w) = p.word() else {
        bail!("SQL0104: a data type is expected{}.", p.at());
    };
    p.i += 1;
    let size = |p: &mut P, default: u32| -> Result<u32> {
        if p.punct("(") {
            let n = p.number()?;
            p.expect_punct(")")?;
            Ok(n)
        } else {
            Ok(default)
        }
    };
    let precision = |p: &mut P| -> Result<(u32, u32)> {
        if p.punct("(") {
            let digits = p.number()?;
            let scale = if p.punct(",") { p.number()? } else { 0 };
            p.expect_punct(")")?;
            Ok((digits, scale))
        } else {
            Ok((5, 0))
        }
    };
    let t = match w.as_str() {
        "CHAR" | "CHARACTER" => {
            if p.eat("VARYING") {
                let n = size(p, 0)?;
                (DataType::Char, n, 0, true)
            } else {
                (DataType::Char, size(p, 1)?, 0, false)
            }
        }
        "VARCHAR" => (DataType::Char, size(p, 0)?, 0, true),
        "SMALLINT" => (DataType::Binary, 4, 0, false),
        "INTEGER" | "INT" => (DataType::Binary, 9, 0, false),
        "BIGINT" => (DataType::Binary, 18, 0, false),
        "DECIMAL" | "DEC" => {
            let (d, s) = precision(p)?;
            (DataType::Packed, d, s, false)
        }
        "NUMERIC" | "NUM" => {
            let (d, s) = precision(p)?;
            (DataType::Zoned, d, s, false)
        }
        // REAL is a single-precision float, DOUBLE a double-precision one;
        // FLOAT(n) is single for n up to 24 (Db2 for i SQL Reference).
        "REAL" => (DataType::Float, 9, 0, false),
        "DOUBLE" => {
            let _ = p.eat("PRECISION");
            (DataType::Float, 17, 0, false)
        }
        "FLOAT" => {
            let n = size(p, 53)?;
            if n == 0 || n > 53 {
                bail!("SQL0604: FLOAT({n}) is not valid.");
            }
            (DataType::Float, if n <= 24 { 9 } else { 17 }, 0, false)
        }
        "BINARY" => {
            if p.eat("VARYING") {
                (DataType::Hex, size(p, 0)?, 0, true)
            } else {
                (DataType::Hex, size(p, 1)?, 0, false)
            }
        }
        "VARBINARY" => (DataType::Hex, size(p, 0)?, 0, true),
        // Graphic strings (UCS-2): GRAPHIC, VARGRAPHIC, NCHAR, NVARCHAR.
        "GRAPHIC" | "NCHAR" => {
            if p.eat("VARYING") {
                (DataType::Graphic, size(p, 0)?, 0, true)
            } else {
                (DataType::Graphic, size(p, 1)?, 0, false)
            }
        }
        "VARGRAPHIC" | "NVARCHAR" => (DataType::Graphic, size(p, 0)?, 0, true),
        "NATIONAL" => {
            if !(p.eat("CHARACTER") || p.eat("CHAR")) {
                bail!("SQL0104: NATIONAL CHARACTER expected.");
            }
            if p.eat("VARYING") {
                (DataType::Graphic, size(p, 0)?, 0, true)
            } else {
                (DataType::Graphic, size(p, 1)?, 0, false)
            }
        }
        "CLOB" | "BLOB" | "DBCLOB" => {
            let n = if p.punct("(") {
                let n = lob_size(p)?;
                p.expect_punct(")")?;
                n
            } else {
                1024 * 1024
            };
            let t = match w.as_str() {
                "CLOB" => DataType::Clob,
                "BLOB" => DataType::Blob,
                _ => DataType::Dbclob,
            };
            (t, n, 0, true)
        }
        "DATE" => (DataType::Date, 10, 0, false),
        "TIME" => (DataType::Time, 8, 0, false),
        // TIMESTAMP(p), 0-12 fractional digits: 19 characters, or 20 + p.
        "TIMESTAMP" => {
            let n = size(p, 6)?;
            if n > 12 {
                bail!("SQL0604: TIMESTAMP({n}): the precision is 0 to 12.");
            }
            (
                DataType::Timestamp,
                if n == 0 { 19 } else { 20 + n },
                0,
                false,
            )
        }
        // DECFLOAT(16) or DECFLOAT(34) (the default).
        "DECFLOAT" => {
            let n = size(p, 34)?;
            if n != 16 && n != 34 {
                bail!("SQL0604: DECFLOAT({n}): the precision is 16 or 34.");
            }
            (DataType::Decfloat, n, 0, false)
        }
        "BOOLEAN" => (DataType::Boolean, 1, 0, false),
        "XML" => (DataType::Xml, 0, 0, true),
        other => bail!("SQL0084: data type {other} is not supported yet."),
    };
    match t {
        (DataType::Char | DataType::Hex, n, _, _) if n == 0 || n > 32740 => {
            bail!("SQL0604: Length {n} is not valid for a character or binary column.")
        }
        (DataType::Packed | DataType::Zoned, d, s, _) if d == 0 || d > 63 || s > d => {
            bail!("SQL0604: Precision {d},{s} is not valid.")
        }
        _ => {}
    }
    // CCSID: character data is kept as text. FOR BIT DATA: bytes that are
    // not translated (CCSID 65535), as BINARY.
    let mut t = t;
    loop {
        if p.eat("CCSID") {
            let ccsid = p.number()?;
            if ccsid == 65535 && t.0 == DataType::Char {
                t.0 = DataType::Hex;
            }
        } else if p.eat("FOR") {
            if p.eat("COLUMN") || p.eat("SYSTEM") {
                // FOR COLUMN belongs to the column name, before the type.
                bail!("SQL0104: FOR COLUMN comes before the data type.");
            }
            p.expect("BIT")?;
            p.expect("DATA")?;
            if t.0 == DataType::Char {
                t.0 = DataType::Hex;
            }
        } else {
            break;
        }
    }
    Ok(t)
}

/// A large object's length: n, nK, nM or nG.
fn lob_size(p: &mut P) -> Result<u32> {
    let Some(Tok::Number(t)) = p.next() else {
        bail!("SQL0104: a length is expected.");
    };
    let (digits, mult) = match t.chars().last() {
        Some('K') | Some('k') => (&t[..t.len() - 1], 1024u64),
        Some('M') | Some('m') => (&t[..t.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t.as_str(), 1),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| anyhow!("SQL0104: {t} is not a valid length."))?;
    let bytes = n * mult;
    if bytes == 0 || bytes > 2_147_483_647 {
        bail!("SQL0604: Length {t} is not valid for a large object.");
    }
    Ok(bytes as u32)
}

/// A default value: a constant, NULL, or a special register.
fn default_value(p: &mut P, f: &FieldDef) -> Result<FieldDefault> {
    let neg = p.punct("-");
    let t = p.t.get(p.i).cloned();
    let d = match t {
        Some(Tok::Number(n)) if f.data_type.is_numeric() => {
            p.i += 1;
            let v = if neg { format!("-{n}") } else { n };
            check_number(&v).map_err(sql0104)?;
            FieldDefault::Value(v)
        }
        Some(Tok::Str(s)) if !neg && !f.data_type.is_numeric() => {
            p.i += 1;
            FieldDefault::Value(s)
        }
        Some(Tok::Word(w)) if !neg => {
            let w = w.to_ascii_uppercase();
            match w.as_str() {
                "NULL" => {
                    p.i += 1;
                    FieldDefault::Null
                }
                "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" | "CURRENT" => {
                    p.i += 1;
                    let reg = if w == "CURRENT" {
                        p.word().unwrap_or_default()
                    } else {
                        w.trim_start_matches("CURRENT_").to_string()
                    };
                    if w == "CURRENT" {
                        p.i += 1;
                    }
                    let fits = matches!(
                        (reg.as_str(), f.data_type),
                        ("DATE", DataType::Date)
                            | ("TIME", DataType::Time)
                            | ("TIMESTAMP", DataType::Timestamp)
                    );
                    if !fits {
                        bail!(
                            "SQL0574: Default value CURRENT {reg} does not fit column {}.",
                            f.name
                        );
                    }
                    FieldDefault::System
                }
                _ => return Ok(FieldDefault::System),
            }
        }
        _ if neg => bail!("SQL0574: Default value for column {} is not valid.", f.name),
        // DEFAULT with no value: the type's default.
        _ => FieldDefault::System,
    };
    if let FieldDefault::Value(v) = &d {
        if f.data_type == DataType::Char && v.chars().count() > f.length as usize {
            bail!("SQL0574: Default value for column {} is too long.", f.name);
        }
    }
    Ok(d)
}

/// A CHECK condition as SQL: column names, constants, comparisons, IN,
/// BETWEEN, IS [NOT] NULL, AND, OR, NOT and parentheses only.
fn check_sql(toks: &[Tok], fields: &[FieldDef]) -> Result<String> {
    const WORDS: &[&str] = &["AND", "OR", "NOT", "IN", "BETWEEN", "IS", "NULL", "LIKE"];
    const PUNCT: &[&str] = &["(", ")", ",", "=", "<", ">", "-", "+"];
    // Scalar functions a condition may use: Db2 name -> PostgreSQL name.
    const FUNCTIONS: &[(&str, &str)] = &[
        ("LENGTH", "length"),
        ("TRIM", "btrim"),
        ("LTRIM", "ltrim"),
        ("RTRIM", "rtrim"),
        ("UPPER", "upper"),
        ("UCASE", "upper"),
        ("LOWER", "lower"),
        ("LCASE", "lower"),
        ("SUBSTR", "substr"),
        ("ABS", "abs"),
        ("MOD", "mod"),
        ("COALESCE", "coalesce"),
    ];
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        let piece = match t {
            Tok::Word(w) | Tok::Quoted(w) => {
                let up = if matches!(t, Tok::Word(_)) {
                    w.to_ascii_uppercase()
                } else {
                    w.clone()
                };
                let call = matches!(toks.get(k + 1), Some(Tok::Punct(p)) if p == "(");
                if let (true, Some((_, pg))) = (
                    call && matches!(t, Tok::Word(_)),
                    FUNCTIONS.iter().find(|(d, _)| *d == up),
                ) {
                    pg.to_string()
                } else if matches!(t, Tok::Word(_)) && WORDS.contains(&up.as_str()) {
                    up
                } else if let Some(f) = fields.iter().find(|f| f.is_named(&up)) {
                    f.column()
                } else {
                    bail!("SQL0084: CHECK ({}): only the table's columns, constants, comparisons and a few scalar functions are supported yet.", toks.iter().map(show).collect::<Vec<_>>().join(" "));
                }
            }
            Tok::Str(s) => quote(s),
            Tok::Number(n) => {
                check_number(n).map_err(sql0104)?;
                n.clone()
            }
            Tok::Punct(p) if PUNCT.contains(&p.as_str()) => p.clone(),
            other => bail!("SQL0084: CHECK: {} is not supported yet.", show(other)),
        };
        out.push(piece);
    }
    // Put the two-character operators back together.
    let mut s = String::new();
    for (k, piece) in out.iter().enumerate() {
        let glue = k > 0
            && matches!(
                (out[k - 1].as_str(), piece.as_str()),
                ("<", ">") | ("<", "=") | (">", "=")
            );
        if k > 0 && !glue {
            s.push(' ');
        }
        s.push_str(piece);
    }
    Ok(s)
}

/// GENERATED ALWAYS | BY DEFAULT AS IDENTITY [(options)], after
/// GENERATED (Db2 for i, CREATE TABLE): only integers and decimals with no
/// decimal places can be identity columns.
fn identity(p: &mut P, f: &FieldDef) -> Result<crate::dds::Identity> {
    let always = if p.eat("ALWAYS") {
        true
    } else {
        p.expect("BY")?;
        p.expect("DEFAULT")?;
        false
    };
    if !p.eat("AS") || !p.eat("IDENTITY") {
        bail!("SQL0084: GENERATED columns other than identity columns (row change timestamps, expressions) are not supported yet.");
    }
    if !(f.data_type.is_numeric() && f.decimals == 0) {
        bail!("SQL0574: Identity column {} must be an integer or a decimal number with no decimal places.", f.name);
    }
    let mut id = crate::dds::Identity {
        always,
        start: None,
        increment: None,
        min: None,
        max: None,
        cycle: false,
    };
    let int = |p: &mut P| -> Result<i64> {
        let neg = p.punct("-");
        match p.next() {
            Some(Tok::Number(n)) => {
                let v: i64 = n
                    .parse()
                    .map_err(|_| anyhow!("SQL0104: {n} is not an integer."))?;
                Ok(if neg { -v } else { v })
            }
            _ => bail!("SQL0104: an integer is expected."),
        }
    };
    if p.punct("(") {
        loop {
            if p.eat("START") {
                p.expect("WITH")?;
                id.start = Some(int(p)?);
            } else if p.eat("INCREMENT") {
                p.expect("BY")?;
                id.increment = Some(int(p)?);
            } else if p.eat("MINVALUE") {
                id.min = Some(int(p)?);
            } else if p.eat("MAXVALUE") {
                id.max = Some(int(p)?);
            } else if p.eat("CYCLE") {
                id.cycle = true;
            } else if p.eat("CACHE") {
                // Caching changes performance only.
                int(p)?;
            } else if p.eat("NO") {
                // NO MINVALUE, NO MAXVALUE, NO CYCLE, NO CACHE, NO ORDER
                let _ = p.eat("MINVALUE")
                    || p.eat("MAXVALUE")
                    || p.eat("CYCLE")
                    || p.eat("CACHE")
                    || p.eat("ORDER");
            } else if p.eat("ORDER") {
            } else {
                break;
            }
            let _ = p.punct(",");
        }
        p.expect_punct(")")?;
    }
    Ok(id)
}

fn create_table(p: &mut P, replace: bool) -> Result<Ddl> {
    let name = p.name()?;
    let system = if p.eat("FOR") {
        p.expect("SYSTEM")?;
        p.expect("NAME")?;
        Some(p.system_name("table")?)
    } else {
        None
    };
    p.expect_punct("(")?;
    let mut fields: Vec<FieldDef> = Vec::new();
    let mut uniques: Vec<(String, Vec<String>)> = Vec::new();
    // (constraint name, condition tokens), checked once all columns are known.
    let mut checks: Vec<(Option<String>, Vec<Tok>)> = Vec::new();
    let mut not_null: Vec<String> = Vec::new();
    loop {
        let constraint = if p.eat("CONSTRAINT") {
            Some(p.ident()?)
        } else {
            None
        };
        match p.word().as_deref() {
            Some("PRIMARY") | Some("UNIQUE") => {
                let primary = p.eat("PRIMARY");
                if primary {
                    p.expect("KEY")?;
                } else {
                    p.i += 1;
                }
                p.expect_punct("(")?;
                let mut cols = Vec::new();
                loop {
                    cols.push(p.field()?);
                    if !p.punct(",") {
                        break;
                    }
                }
                p.expect_punct(")")?;
                if primary {
                    not_null.extend(cols.clone());
                }
                let cname = constraint.unwrap_or_else(|| {
                    if primary {
                        "PK".into()
                    } else {
                        format!("U{}", uniques.len() + 1)
                    }
                });
                uniques.push((cname, cols));
            }
            Some("CHECK") => {
                p.i += 1;
                p.expect_punct("(")?;
                checks.push((constraint, p.group()?));
            }
            Some("FOREIGN") => bail!("SQL0084: FOREIGN KEY constraints are not supported yet."),
            _ if constraint.is_some() => {
                bail!("SQL0104: a constraint is expected{}.", p.at())
            }
            _ => {
                let fname = p.field()?;
                if fields.iter().any(|f| f.is_named(&fname)) {
                    bail!("SQL0612: {fname} is a duplicate column name.");
                }
                // The system column name: FOR COLUMN, the name itself when
                // it is a valid system name, or a generated one.
                let given = if p.eat("FOR") {
                    p.expect("COLUMN")?;
                    Some(p.system_name("column")?)
                } else {
                    None
                };
                let sys = match given {
                    Some(g) => g,
                    None if validate_name("column", &fname).is_ok() => fname.clone(),
                    None => generated_name(&fname, &|c| {
                        fields.iter().any(|f: &FieldDef| f.is_named(c))
                    })?,
                };
                if fields.iter().any(|f| f.is_named(&sys)) {
                    bail!("SQL0612: {sys} is a duplicate column name.");
                }
                let (t, len, dec, varlen) = data_type(p)?;
                let mut f = FieldDef::new(&sys, t, len, dec);
                f.double = t == DataType::Float && len > 9;
                if sys != fname {
                    f.alias = Some(fname.clone());
                }
                f.varlen = varlen;
                f.null_capable = true;
                // Column options, in any order.
                loop {
                    if p.eat("NOT") {
                        p.expect("NULL")?;
                        f.null_capable = false;
                        if f.default.is_none() {
                            f.default = Some(FieldDefault::Required);
                        }
                    } else if p.eat("NULL") {
                        f.null_capable = true;
                    } else if p.eat("WITH") {
                        p.expect("DEFAULT")?;
                        f.default = Some(default_value(p, &f)?);
                    } else if p.eat("DEFAULT") {
                        f.default = Some(default_value(p, &f)?);
                    } else if p.eat("PRIMARY") {
                        p.expect("KEY")?;
                        not_null.push(sys.clone());
                        uniques.push(("PK".into(), vec![sys.clone()]));
                    } else if p.eat("UNIQUE") {
                        uniques.push((format!("U{}", uniques.len() + 1), vec![sys.clone()]));
                    } else if p.eat("CHECK") {
                        p.expect_punct("(")?;
                        checks.push((None, p.group()?));
                    } else if p.eat("CONSTRAINT") {
                        p.ident()?;
                    } else if p.eat("CCSID") {
                        p.number()?;
                    } else if p.eat("GENERATED") {
                        f.identity = Some(identity(p, &f)?);
                        f.null_capable = false;
                        f.default = None;
                    } else if p.eat("REFERENCES") {
                        bail!("SQL0084: REFERENCES constraints are not supported yet.");
                    } else if p.eat("IMPLICITLY") {
                        p.expect("HIDDEN")?;
                        bail!("SQL0084: IMPLICITLY HIDDEN columns are not supported yet.");
                    } else {
                        break;
                    }
                }
                if f.default == Some(FieldDefault::Null) && !f.null_capable {
                    bail!("SQL0574: Default NULL is not valid for NOT NULL column {fname}.");
                }
                fields.push(f);
            }
        }
        if p.punct(",") {
            continue;
        }
        p.expect_punct(")")?;
        break;
    }
    if fields.is_empty() {
        bail!("SQL0104: a table needs at least one column.");
    }
    // Constraint columns by system name (they may be written either way).
    let system_of = |c: &str| {
        fields
            .iter()
            .find(|f| f.is_named(c))
            .map(|f| f.name.clone())
    };
    let mut not_null: Vec<String> = not_null
        .iter()
        .map(|c| system_of(c).unwrap_or_else(|| c.clone()))
        .collect();
    not_null.dedup();
    let uniques: Vec<(String, Vec<String>)> = uniques
        .into_iter()
        .map(|(n, cols)| {
            (
                n,
                cols.iter()
                    .map(|c| system_of(c).unwrap_or_else(|| c.clone()))
                    .collect(),
            )
        })
        .collect();
    // A primary key's columns are NOT NULL (an SQL table has no null keys).
    for c in &not_null {
        let Some(f) = fields.iter_mut().find(|f| &f.name == c) else {
            bail!("SQL0205: Column {c} not in table {}.", name.name);
        };
        f.null_capable = false;
        if f.default.is_none() || f.default == Some(FieldDefault::Null) {
            f.default = Some(FieldDefault::Required);
        }
    }
    for (_, cols) in &uniques {
        for c in cols {
            if !fields.iter().any(|f| &f.name == c) {
                bail!("SQL0205: Column {c} not in table {}.", name.name);
            }
        }
    }
    if uniques.iter().filter(|(n, _)| n == "PK").count() > 1 {
        bail!(
            "SQL0624: Table {} has more than one primary key.",
            name.name
        );
    }
    // The record format: RCDFMT, else the system table name (set when the
    // table is created, if it is generated).
    let mut format = String::new();
    loop {
        if p.eat("RCDFMT") {
            format = p.system_name("record format")?;
        } else if p.eat("ON") {
            p.expect("REPLACE")?;
            if p.eat("PRESERVE") {
                bail!("SQL0084: ON REPLACE PRESERVE ROWS is not supported yet.");
            }
            p.expect("DELETE")?;
            p.expect("ROWS")?;
        } else if p.eat("NOT") {
            p.expect("LOGGED")?;
            p.expect("INITIALLY")?;
        } else {
            break;
        }
    }
    let checks = checks
        .into_iter()
        .enumerate()
        .map(|(k, (n, toks))| {
            Ok((
                n.unwrap_or_else(|| format!("C{}", k + 1)),
                check_sql(&toks, &fields)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Ddl::CreateTable {
        system,
        file: PhysicalFile {
            name: name.name.clone(),
            format,
            text: String::new(),
            unique: false,
            fields,
            keys: Vec::new(),
            descending: Vec::new(),
            view: None,
        },
        name,
        replace,
        uniques,
        checks,
    })
}

/// What ALTER TABLE does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alter {
    /// ADD COLUMN: the field (its system name may be renumbered when the
    /// column is added), its unique and check constraints.
    AddColumn {
        field: FieldDef,
        uniques: Uniques,
        checks: Checks,
    },
    /// ADD PRIMARY KEY / UNIQUE (columns as written).
    AddUnique {
        name: String,
        primary: bool,
        columns: Vec<String>,
    },
    /// ADD CHECK: the condition's tokens, checked against the table.
    AddCheck {
        name: Option<String>,
        condition: String,
    },
    DropColumn {
        column: String,
        cascade: bool,
    },
    /// DROP PRIMARY KEY (name PK), DROP CONSTRAINT / UNIQUE / CHECK name.
    DropConstraint {
        name: String,
    },
    AlterColumn {
        column: String,
        change: ColumnChange,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnChange {
    /// SET DATA TYPE: (type, length or digits, decimals, varying).
    DataType(DataType, u32, u32, bool),
    SetDefault(FieldDefault),
    DropDefault,
    SetNotNull,
    DropNotNull,
}

/// ALTER TABLE name action (Db2 for i SQL Reference, ALTER TABLE): one
/// action per statement.
fn alter_table(p: &mut P) -> Result<Ddl> {
    let table = p.name()?;
    let action = if p.eat("ADD") {
        let constraint = if p.eat("CONSTRAINT") {
            Some(p.field()?)
        } else {
            None
        };
        match p.word().as_deref() {
            Some("PRIMARY") | Some("UNIQUE") => {
                let primary = p.eat("PRIMARY");
                if primary {
                    p.expect("KEY")?;
                } else {
                    p.i += 1;
                }
                p.expect_punct("(")?;
                let mut columns = Vec::new();
                loop {
                    columns.push(p.field()?);
                    if !p.punct(",") {
                        break;
                    }
                }
                p.expect_punct(")")?;
                Alter::AddUnique {
                    name: constraint.unwrap_or_else(|| {
                        if primary {
                            "PK".into()
                        } else {
                            format!("U_{}", columns.join("_"))
                        }
                    }),
                    primary,
                    columns,
                }
            }
            Some("CHECK") => {
                p.i += 1;
                p.expect_punct("(")?;
                let toks = p.group()?;
                Alter::AddCheck {
                    name: constraint,
                    condition: toks.iter().map(show).collect::<Vec<_>>().join(" "),
                }
            }
            Some("FOREIGN") => bail!("SQL0084: FOREIGN KEY constraints are not supported yet."),
            _ if constraint.is_some() => bail!("SQL0104: a constraint is expected{}.", p.at()),
            _ => {
                let _ = p.eat("COLUMN");
                // The column definition, read as CREATE TABLE reads one.
                let rest: Vec<String> = p.t[p.i..].iter().map(show).collect();
                p.i = p.t.len();
                if rest.iter().any(|w| w.eq_ignore_ascii_case("BEFORE")) {
                    bail!("SQL0084: ADD COLUMN ... BEFORE is not supported yet.");
                }
                let text = format!("CREATE TABLE X ({})", rest.join(" "));
                let Some(Ddl::CreateTable {
                    file,
                    uniques,
                    checks,
                    ..
                }) = parse(&text)?
                else {
                    bail!("SQL0104: a column definition is expected.");
                };
                let [field] = file.fields.as_slice() else {
                    bail!("SQL0104: ADD COLUMN adds one column.");
                };
                Alter::AddColumn {
                    field: field.clone(),
                    uniques,
                    checks,
                }
            }
        }
    } else if p.eat("DROP") {
        if p.eat("PRIMARY") {
            p.expect("KEY")?;
            Alter::DropConstraint { name: "PK".into() }
        } else if p.eat("CONSTRAINT") || p.eat("UNIQUE") || p.eat("CHECK") {
            Alter::DropConstraint { name: p.field()? }
        } else if p.eat("FOREIGN") {
            bail!("SQL0084: FOREIGN KEY constraints are not supported yet.");
        } else {
            let _ = p.eat("COLUMN");
            let column = p.field()?;
            // CASCADE is the default (Db2 for i, ALTER TABLE DROP COLUMN).
            let cascade = !p.eat("RESTRICT");
            let _ = p.eat("CASCADE");
            Alter::DropColumn { column, cascade }
        }
    } else if p.eat("ALTER") {
        let _ = p.eat("COLUMN");
        let column = p.field()?;
        let change = if p.eat("SET") {
            if p.eat("DATA") {
                p.expect("TYPE")?;
                let (t, l, d, v) = data_type(p)?;
                ColumnChange::DataType(t, l, d, v)
            } else if p.eat("NOT") {
                p.expect("NULL")?;
                ColumnChange::SetNotNull
            } else {
                let _ = p.eat("WITH");
                p.expect("DEFAULT")?;
                // The value is checked against the column when it runs.
                let rest: Vec<String> = p.t[p.i..].iter().map(show).collect();
                p.i = p.t.len();
                ColumnChange::SetDefault(FieldDefault::Value(rest.join(" ")))
            }
        } else if p.eat("DROP") {
            if p.eat("DEFAULT") {
                ColumnChange::DropDefault
            } else {
                p.expect("NOT")?;
                p.expect("NULL")?;
                ColumnChange::DropNotNull
            }
        } else {
            bail!("SQL0084: this ALTER COLUMN change is not supported yet.");
        };
        Alter::AlterColumn { column, change }
    } else {
        bail!("SQL0084: this ALTER TABLE action is not supported yet.");
    };
    Ok(Ddl::AlterTable { table, action })
}

/// Top-level words of a statement with their byte offsets (outside
/// constants, quoted names, comments and parentheses).
fn top_words(sql: &str) -> Vec<(usize, String)> {
    let b: Vec<(usize, char)> = sql.char_indices().collect();
    let mut out = Vec::new();
    let (mut i, mut depth) = (0, 0i32);
    while i < b.len() {
        let c = b[i].1;
        if c == '\'' || c == '"' {
            i += 1;
            while i < b.len() && b[i].1 != c {
                i += 1;
            }
            i += 1;
        } else if c == '-' && b.get(i + 1).map(|x| x.1) == Some('-') {
            while i < b.len() && b[i].1 != '\n' {
                i += 1;
            }
        } else if c == '(' {
            depth += 1;
            i += 1;
        } else if c == ')' {
            depth -= 1;
            i += 1;
        } else if c.is_alphanumeric() || "_$#@".contains(c) {
            let start = i;
            while i < b.len() && (b[i].1.is_alphanumeric() || "_$#@".contains(b[i].1)) {
                i += 1;
            }
            if depth == 0 {
                let w: String = b[start..i].iter().map(|x| x.1).collect();
                out.push((b[start].0, w.to_ascii_uppercase()));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// CREATE [OR REPLACE] TABLE name [FOR SYSTEM NAME s] [(columns)] AS
/// (query) WITH [NO] DATA [RCDFMT name] (Db2 for i SQL Reference, CREATE
/// TABLE, as-result-table).
fn create_table_as(sql: &str, replace: bool, k: usize, words: &[(usize, String)]) -> Result<Ddl> {
    let as_at = words[k + 1..]
        .iter()
        .position(|(_, w)| w == "AS")
        .map(|j| j + k + 1)
        .expect("checked by the caller");
    let head = &sql[words[k].0 + 5..words[as_at].0];
    let toks: Vec<Tok> = tokenize(head)?
        .into_iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut p = P { t: toks, i: 0 };
    let name = p.name()?;
    let system = if p.eat("FOR") {
        p.expect("SYSTEM")?;
        p.expect("NAME")?;
        Some(p.system_name("table")?)
    } else {
        None
    };
    let mut columns = Vec::new();
    if p.punct("(") {
        loop {
            columns.push(p.field()?);
            if !p.punct(",") {
                break;
            }
        }
        p.expect_punct(")")?;
    }
    if let Some(t) = p.t.get(p.i) {
        bail!("SQL0104: Token {} not valid here.", show(t));
    }
    // The query in parentheses right after AS.
    let rest = &sql[words[as_at].0 + 2..];
    let open = rest
        .find(|c: char| !c.is_whitespace())
        .filter(|i| rest[*i..].starts_with('('))
        .ok_or_else(|| anyhow!("SQL0104: ( expected after AS."))?;
    let cs: Vec<(usize, char)> = rest.char_indices().collect();
    let (mut depth, mut close, mut q) = (0i32, None, false);
    for &(i, c) in cs.iter().filter(|(i, _)| *i >= open) {
        match c {
            '\'' => q = !q,
            '(' if !q => depth += 1,
            ')' if !q => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close.ok_or_else(|| anyhow!("SQL0104: ) expected after the query."))?;
    let query = rest[open + 1..close].trim().to_string();
    let tail = rest[close + 1..].trim().trim_end_matches(';');
    let toks: Vec<Tok> = tokenize(tail)?
        .into_iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut p = P { t: toks, i: 0 };
    let mut with_data = None;
    let mut format = None;
    while p.i < p.t.len() {
        if p.eat("WITH") {
            let no = p.eat("NO");
            p.expect("DATA")?;
            with_data = Some(!no);
        } else if p.eat("RCDFMT") {
            format = Some(p.system_name("record format")?);
        } else if p.eat("NOT") {
            p.expect("LOGGED")?;
            p.expect("INITIALLY")?;
        } else if p.eat("INCLUDING") || p.eat("EXCLUDING") {
            // IDENTITY / COLUMN DEFAULTS / IMPLICITLY HIDDEN...: the copy
            // options; columns are made from the query either way.
            while p.i < p.t.len()
                && !matches!(
                    p.word().as_deref(),
                    Some("WITH" | "RCDFMT" | "INCLUDING" | "EXCLUDING")
                )
            {
                p.i += 1;
            }
        } else {
            bail!("SQL0104: Token {} not valid here.", show(&p.t[p.i]));
        }
    }
    let Some(with_data) = with_data else {
        bail!("SQL0104: WITH DATA or WITH NO DATA is expected after the query.");
    };
    Ok(Ddl::CreateTableAs {
        name,
        system,
        replace,
        columns,
        query,
        with_data,
        format,
    })
}

/// CREATE [OR REPLACE] VIEW name [FOR SYSTEM NAME s] [(columns)] AS query
/// [WITH [CASCADED | LOCAL] CHECK OPTION] [RCDFMT name].
fn create_view(sql: &str, replace: bool) -> Result<Ddl> {
    let words = top_words(sql);
    let Some(k) = words.iter().position(|(_, w)| w == "VIEW") else {
        bail!("SQL0104: VIEW expected.");
    };
    let Some(as_at) = words[k + 1..]
        .iter()
        .position(|(_, w)| w == "AS")
        .map(|j| j + k + 1)
    else {
        bail!("SQL0104: AS and a query are expected in CREATE VIEW.");
    };
    // The head: VIEW name [FOR SYSTEM NAME s] [(columns)].
    let head = &sql[words[k].0 + 4..words[as_at].0];
    let toks: Vec<Tok> = tokenize(head)?
        .into_iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut p = P { t: toks, i: 0 };
    let name = p.name()?;
    let system = if p.eat("FOR") {
        p.expect("SYSTEM")?;
        p.expect("NAME")?;
        Some(p.system_name("view")?)
    } else {
        None
    };
    let mut columns = Vec::new();
    if p.punct("(") {
        loop {
            columns.push(p.field()?);
            if !p.punct(",") {
                break;
            }
        }
        p.expect_punct(")")?;
    }
    if let Some(t) = p.t.get(p.i) {
        bail!("SQL0104: Token {} not valid here.", show(t));
    }
    // The tail: WITH ... CHECK OPTION, RCDFMT name.
    let mut end = sql.trim_end().trim_end_matches(';').len();
    let mut format = None;
    let mut check_option = false;
    let tail: Vec<&(usize, String)> = words[as_at + 1..].iter().collect();
    if let Some(r) = tail.iter().rposition(|(_, w)| w == "RCDFMT") {
        let (at, _) = tail[r];
        let rest: Vec<Tok> = tokenize(&sql[*at + 6..end])?
            .into_iter()
            .filter(|t| !matches!(t, Tok::Space(_)))
            .collect();
        let mut q = P { t: rest, i: 0 };
        format = Some(q.system_name("record format")?);
        if q.t.get(q.i).is_some() {
            bail!("SQL0104: RCDFMT must end the statement.");
        }
        end = *at;
    }
    let ws: Vec<&String> = tail
        .iter()
        .filter(|(o, _)| *o < end)
        .map(|(_, w)| w)
        .collect();
    if ws.len() >= 3 && ws[ws.len() - 2] == "CHECK" && ws[ws.len() - 1] == "OPTION" {
        let w = tail
            .iter()
            .filter(|(o, _)| *o < end)
            .rev()
            .find(|(_, w)| w == "WITH")
            .map(|(o, _)| *o)
            .ok_or_else(|| anyhow!("SQL0104: WITH CHECK OPTION expected."))?;
        if tail.iter().any(|(o, x)| *o > w && *o < end && x == "LOCAL") {
            bail!("SQL0084: WITH LOCAL CHECK OPTION is not supported yet.");
        }
        check_option = true;
        end = w;
    }
    let query = sql[words[as_at].0 + 2..end].trim().to_string();
    if query.is_empty() {
        bail!("SQL0104: a query is expected after AS.");
    }
    Ok(Ddl::CreateView {
        name,
        system,
        replace,
        columns,
        query,
        check_option,
        format,
    })
}

fn create_index(p: &mut P, unique: bool) -> Result<Ddl> {
    let name = p.name()?;
    let system = if p.eat("FOR") {
        p.expect("SYSTEM")?;
        p.expect("NAME")?;
        Some(p.system_name("index")?)
    } else {
        None
    };
    p.expect("ON")?;
    let table = p.name()?;
    p.expect_punct("(")?;
    let mut keys = Vec::new();
    loop {
        let c = p.field()?;
        let desc = if p.eat("DESC") {
            true
        } else {
            let _ = p.eat("ASC");
            false
        };
        keys.push((c, desc));
        if !p.punct(",") {
            break;
        }
    }
    p.expect_punct(")")?;
    let mut format = None;
    if p.eat("RCDFMT") {
        format = Some(p.system_name("record format")?);
        if p.eat("ADD") {
            bail!("SQL0084: RCDFMT ... ADD is not supported yet.");
        }
    }
    if p.eat("WHERE") {
        bail!("SQL0084: indexes with WHERE are not supported yet.");
    }
    Ok(Ddl::CreateIndex {
        name,
        system,
        table,
        unique,
        keys,
        format,
    })
}

fn label(p: &mut P) -> Result<Ddl> {
    let text = |p: &mut P| -> Result<String> {
        let s = p.string()?;
        if s.chars().count() > 50 {
            bail!("SQL0102: The text '{s}' is longer than 50 characters.");
        }
        Ok(s)
    };
    // (column [TEXT] IS 'x', ...): only TEXT IS changes the field text.
    let columns = |p: &mut P| -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        loop {
            let c = p.field()?;
            let is_text = p.eat("TEXT");
            p.expect("IS")?;
            let s = p.string()?;
            if is_text {
                if s.chars().count() > 50 {
                    bail!("SQL0102: The text '{s}' is longer than 50 characters.");
                }
                out.push((c, s));
            } else if s.chars().count() > 60 {
                bail!("SQL0102: The column heading '{s}' is longer than 60 characters.");
            }
            if !p.punct(",") {
                break;
            }
        }
        p.expect_punct(")")?;
        Ok(out)
    };
    match p.word().as_deref() {
        Some("TABLE") | Some("INDEX") | Some("VIEW") => {
            p.i += 1;
            let name = p.name()?;
            p.expect("IS")?;
            Ok(Ddl::LabelObject {
                name,
                text: text(p)?,
            })
        }
        Some("COLUMN") => {
            p.i += 1;
            let first = p.ident()?;
            // COLUMN t.c [TEXT] IS 'x', COLUMN lib.t.c, or COLUMN t (...)
            let mut parts = vec![first];
            while p.punct(".") || p.punct("/") {
                parts.push(p.ident()?);
            }
            if p.punct("(") {
                let table = name_of(&parts)?;
                return Ok(Ddl::LabelColumns {
                    table,
                    texts: columns(p)?,
                });
            }
            if parts.len() < 2 {
                bail!("SQL0104: LABEL ON COLUMN needs table.column.");
            }
            let col = parts.pop().unwrap_or_default();
            let col = sql_name("column", &col)?;
            let table = name_of(&parts)?;
            let is_text = p.eat("TEXT");
            p.expect("IS")?;
            let s = text(p)?;
            Ok(Ddl::LabelColumns {
                table,
                texts: if is_text { vec![(col, s)] } else { vec![] },
            })
        }
        Some("ALIAS") | Some("PACKAGE") | Some("PROCEDURE") | Some("FUNCTION")
        | Some("SEQUENCE") | Some("TRIGGER") | Some("MASK") | Some("PERMISSION") | Some("TYPE")
        | Some("VARIABLE") | Some("CONSTRAINT") => {
            bail!(
                "SQL0084: LABEL ON {} is not supported yet.",
                p.word().unwrap_or_default()
            )
        }
        _ => {
            // LABEL ON table (column [TEXT] IS 'x', ...)
            let table = p.name()?;
            p.expect_punct("(")?;
            Ok(Ddl::LabelColumns {
                table,
                texts: columns(p)?,
            })
        }
    }
}

fn name_of(parts: &[String]) -> Result<Name> {
    match parts {
        [n] => Ok(Name {
            lib: None,
            name: sql_name("table", n)?,
        }),
        [l, n] => Ok(Name {
            lib: Some(validate_name("schema", l).map_err(sql0104)?),
            name: sql_name("table", n)?,
        }),
        _ => bail!("SQL0104: {} is not a table name.", parts.join(".")),
    }
}

/// What a statement runs with.
pub struct Ctx<'a> {
    pub client: &'a mut Client,
    pub user: &'a str,
    pub libl: &'a [String],
    pub curlib: Option<String>,
    /// For CREATE VIEW (a connection as the SQL role).
    pub database_url: String,
    /// SET SCHEMA (or RUNSQLSTM DFTRDBCOL): the library for unqualified
    /// names instead of the current library and the library list.
    pub schema: Option<String>,
}

impl Ctx<'_> {
    /// The library a new object goes to: the one named, else the default
    /// schema, else the current library, else QGPL (system naming).
    fn target(&self, n: &Name) -> String {
        n.lib
            .clone()
            .or_else(|| self.schema.clone())
            .or_else(|| self.curlib.clone())
            .unwrap_or_else(|| "QGPL".into())
    }

    /// The libraries searched for an existing object.
    fn search(&self, n: &Name) -> Vec<String> {
        match (&n.lib, &self.schema) {
            (Some(l), _) => vec![l.clone()],
            (None, Some(s)) => vec![s.clone()],
            (None, None) => self.libl.to_vec(),
        }
    }

    /// (library, attribute, system name) of an existing file, found by
    /// its system name or its SQL name.
    async fn find(&self, n: &Name) -> Result<Option<(String, String, String)>> {
        for lib in self.search(n) {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT o.attribute, o.name FROM as4tainr.objects o
                       LEFT JOIN as4tainr.files f ON f.library=o.library AND f.name=o.name
                     WHERE o.library=$1 AND o.objtype='*FILE' AND (o.name=$2 OR f.sql_name=$2)
                     ORDER BY o.name=$2 DESC LIMIT 1",
                    &[&lib, &n.name],
                )
                .await?
            {
                return Ok(Some((lib, r.get(0), r.get(1))));
            }
        }
        Ok(None)
    }

    /// The system name of a new object: FOR SYSTEM NAME, the name itself
    /// when it is a valid system name, or a generated one.
    async fn new_system_name(&self, lib: &str, n: &Name, given: &Option<String>) -> Result<String> {
        if let Some(g) = given {
            return Ok(g.clone());
        }
        if validate_name("file", &n.name).is_ok() {
            return Ok(n.name.clone());
        }
        let taken: Vec<String> = self
            .client
            .query(
                "SELECT name FROM as4tainr.objects WHERE library=$1 AND name LIKE $2",
                &[
                    &lib,
                    &format!("{}%", n.name.chars().take(5).collect::<String>()),
                ],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        generated_name(&n.name, &|c| taken.iter().any(|t| t == c))
    }

    async fn may_delete(&self, lib: &str, name: &str) -> Result<()> {
        if authority::effective(self.client, self.user, lib, name, "*FILE").await? < Aut::All {
            bail!("SQL0551: Not authorized to drop {lib}/{name}.");
        }
        Ok(())
    }
}

fn not_found(n: &Name, kind: &str) -> anyhow::Error {
    anyhow!(
        "SQL0204: {} in {} type *FILE not found ({kind}).",
        n.name,
        n.lib.as_deref().unwrap_or("*LIBL")
    )
}

/// Run a data definition statement; the message says what was done.
pub async fn run(ctx: &mut Ctx<'_>, ddl: &Ddl) -> Result<String> {
    match ddl {
        Ddl::SetSchema(s) => {
            if !catalog::library_exists(ctx.client, s).await? {
                bail!("SQL0204: Schema {s} not found.");
            }
            ctx.schema = Some(s.clone());
            Ok(format!("Default schema set to {s}."))
        }
        Ddl::Comment => Ok("Comment accepted (not kept).".into()),
        Ddl::CreateTable {
            name,
            system,
            replace,
            file,
            uniques,
            checks,
        } => {
            let lib = ctx.target(name);
            if !catalog::library_exists(ctx.client, &lib).await? {
                bail!("SQL0204: Schema {lib} not found.");
            }
            let here = Name {
                lib: Some(lib.clone()),
                name: name.name.clone(),
            };
            if let Some((_, attr, sys)) = ctx.find(&here).await? {
                if !replace || attr != "PF" {
                    bail!("SQL0601: {} in {lib} type *FILE already exists.", name.name);
                }
                ctx.may_delete(&lib, &sys).await?;
                catalog::delete_file(ctx.client, &lib, &sys, true).await?;
            }
            let sys = ctx.new_system_name(&lib, name, system).await?;
            let mut file = file.clone();
            file.name = sys.clone();
            if file.format.is_empty() {
                file.format = sys.clone();
            }
            catalog::create_physical_file(ctx.client, &lib, &file).await?;
            let mut done = constraints(ctx.client, &lib, &sys, uniques, checks).await;
            if done.is_ok() && sys != name.name {
                done = catalog::set_sql_name(ctx.client, &lib, &sys, &name.name, true).await;
            }
            if let Err(e) = done {
                let _ = catalog::delete_file(ctx.client, &lib, &sys, true).await;
                return Err(e);
            }
            authority::set_owner(ctx.client, &lib, &sys, "*FILE", ctx.user).await?;
            Ok(if sys == name.name {
                format!("Table {sys} created in {lib}.")
            } else {
                format!("Table {} created in {lib} (system name {sys}).", name.name)
            })
        }
        Ddl::CreateIndex {
            name,
            system,
            table,
            unique,
            keys,
            format,
        } => {
            let Some((tlib, attr, tsys)) = ctx.find(table).await? else {
                return Err(not_found(table, "table"));
            };
            if attr != "PF" {
                bail!("SQL0156: {} is not a table.", table.name);
            }
            authority::check(ctx.client, ctx.user, &tlib, &tsys, "*FILE", Aut::Use).await?;
            let lib = ctx.target(name);
            let here = Name {
                lib: Some(lib.clone()),
                name: name.name.clone(),
            };
            if ctx.find(&here).await?.is_some() {
                bail!("SQL0601: {} in {lib} type *FILE already exists.", name.name);
            }
            let sys = ctx.new_system_name(&lib, name, system).await?;
            let tfile =
                catalog::resolve_file(ctx.client, std::slice::from_ref(&tlib), &tsys).await?;
            // Key columns by system name.
            let mut ks = Vec::new();
            for (k, d) in keys {
                let Some(f) = tfile.file.fields.iter().find(|f| f.is_named(k)) else {
                    bail!("SQL0205: Column {k} not in table {}.", table.name);
                };
                ks.push((f.name.clone(), *d));
            }
            let lf = LogicalFile {
                name: sys.clone(),
                // The table's record format unless RCDFMT names another.
                format: format.clone().unwrap_or_else(|| tfile.file.format.clone()),
                text: String::new(),
                unique: *unique,
                based_on: format!("{tlib}/{tsys}"),
                fields: Vec::new(),
                keys: ks.iter().map(|(k, _)| k.clone()).collect(),
                descending: ks
                    .iter()
                    .filter(|(_, d)| *d)
                    .map(|(k, _)| k.clone())
                    .collect(),
                select_omit: Vec::new(),
            };
            let libl = ctx.libl.to_vec();
            catalog::create_logical_file(ctx.client, &libl, &lib, &lf).await?;
            if sys != name.name {
                catalog::set_sql_name(ctx.client, &lib, &sys, &name.name, false).await?;
            }
            authority::set_owner(ctx.client, &lib, &sys, "*FILE", ctx.user).await?;
            Ok(format!(
                "Index {} created in {lib} over {tlib}/{tsys}{}.",
                name.name,
                if sys == name.name {
                    String::new()
                } else {
                    format!(" (system name {sys})")
                }
            ))
        }
        Ddl::CreateView {
            name,
            system,
            replace,
            columns,
            query,
            check_option,
            format,
        } => {
            let lib = ctx.target(name);
            if !catalog::library_exists(ctx.client, &lib).await? {
                bail!("SQL0204: Schema {lib} not found.");
            }
            let here = Name {
                lib: Some(lib.clone()),
                name: name.name.clone(),
            };
            if let Some((_, _, sys)) = ctx.find(&here).await? {
                let is_view = catalog::resolve_file(ctx.client, std::slice::from_ref(&lib), &sys)
                    .await
                    .map(|f| f.file.view.is_some())
                    .unwrap_or(false);
                if !replace || !is_view {
                    bail!("SQL0601: {} in {lib} type *FILE already exists.", name.name);
                }
                ctx.may_delete(&lib, &sys).await?;
                catalog::delete_file(ctx.client, &lib, &sys, true).await?;
            }
            let sys = ctx.new_system_name(&lib, name, system).await?;
            // The query runs with the job's library list (or the default
            // schema) for unqualified names.
            let libl = match &ctx.schema {
                Some(s) => vec![s.clone()],
                None => ctx.libl.to_vec(),
            };
            let def = ViewDef {
                sql_name: name.name.clone(),
                query: query.clone(),
                columns: columns.clone(),
                check_option: *check_option,
                format: format.clone(),
                libl,
            };
            let url = ctx.database_url.clone();
            make_view(ctx.client, &url, &lib, &sys, &def, ctx.user).await?;
            Ok(if sys == name.name {
                format!("View {sys} created in {lib}.")
            } else {
                format!("View {} created in {lib} (system name {sys}).", name.name)
            })
        }
        Ddl::CreateTableAs {
            name,
            system,
            replace,
            columns,
            query,
            with_data,
            format,
        } => {
            // The query's columns (names and types, as a view's are).
            let libl = match &ctx.schema {
                Some(s) => vec![s.clone()],
                None => ctx.libl.to_vec(),
            };
            let mut fields =
                crate::sql::query_fields(ctx.client, &ctx.database_url, &libl, query).await?;
            // The query's own column names, to select them by.
            let source: Vec<String> = fields
                .iter()
                .map(|f| f.alias.clone().unwrap_or_else(|| f.name.clone()))
                .collect();
            if !columns.is_empty() {
                if columns.len() != fields.len() {
                    bail!(
                        "SQL0117: {} column names for a query with {} columns.",
                        columns.len(),
                        fields.len()
                    );
                }
                for (f, c) in fields.iter_mut().zip(columns) {
                    let sys = if validate_name("column", c).is_ok() {
                        c.clone()
                    } else {
                        f.name.clone()
                    };
                    f.alias = (sys != *c).then(|| c.clone());
                    f.name = sys;
                }
            }
            let file = PhysicalFile {
                name: name.name.clone(),
                format: format.clone().unwrap_or_default(),
                text: String::new(),
                unique: false,
                fields: fields.clone(),
                keys: Vec::new(),
                descending: Vec::new(),
                view: None,
            };
            let create = Ddl::CreateTable {
                name: name.clone(),
                system: system.clone(),
                replace: *replace,
                file,
                uniques: vec![],
                checks: vec![],
            };
            let msg = Box::pin(run(ctx, &create)).await?;
            if *with_data {
                let lib = ctx.target(name);
                let cols: Vec<String> = fields
                    .iter()
                    .map(|f| f.alias.clone().unwrap_or_else(|| f.name.clone()))
                    .collect();
                let target = format!("{lib}.{}", name.name);
                let insert = format!(
                    "INSERT INTO {target} ({}) SELECT {} FROM ({query}) AS AS4TAINR_Q",
                    cols.join(", "),
                    source.join(", ")
                );
                crate::sql::run(ctx.client, &ctx.database_url, &libl, &insert).await?;
            }
            Ok(msg)
        }
        Ddl::AlterTable { table, action } => alter(ctx, table, action).await,
        Ddl::Drop {
            kind,
            name,
            if_exists,
        } => {
            let Some((lib, attr, sys)) = ctx.find(name).await? else {
                if *if_exists {
                    return Ok(format!("{} does not exist; nothing dropped.", name.name));
                }
                return Err(not_found(name, &kind.to_ascii_lowercase()));
            };
            let is_view = catalog::resolve_file(ctx.client, std::slice::from_ref(&lib), &sys)
                .await
                .map(|f| f.file.view.is_some())
                .unwrap_or(false);
            let fits = match *kind {
                "TABLE" => attr == "PF",
                "VIEW" => attr == "LF" && is_view,
                _ => attr == "LF" && !is_view,
            };
            if !fits {
                bail!(
                    "SQL0156: {} in {lib} is not a {}.",
                    name.name,
                    kind.to_ascii_lowercase()
                );
            }
            ctx.may_delete(&lib, &sys).await?;
            catalog::delete_file(ctx.client, &lib, &sys, true).await?;
            Ok(format!("{} {} in {lib} dropped.", title(kind), name.name))
        }
        Ddl::LabelObject { name, text } => {
            let Some((lib, _, sys)) = ctx.find(name).await? else {
                return Err(not_found(name, "label"));
            };
            authority::check(ctx.client, ctx.user, &lib, &sys, "*FILE", Aut::Change).await?;
            catalog::set_file_text(ctx.client, &lib, &sys, text).await?;
            Ok(format!("Label on {} in {lib} changed.", name.name))
        }
        Ddl::LabelColumns { table, texts } => {
            let Some((lib, _, sys)) = ctx.find(table).await? else {
                return Err(not_found(table, "label"));
            };
            authority::check(ctx.client, ctx.user, &lib, &sys, "*FILE", Aut::Change).await?;
            for (c, t) in texts {
                catalog::set_field_text(ctx.client, &lib, &sys, c, t).await?;
            }
            Ok(format!("Labels on {} in {lib} changed.", table.name))
        }
    }
}

/// Run a statement if it is data definition: `Ok(None)` when it is not
/// (the caller runs it as a query or change). `schema` is the default
/// schema, which SET SCHEMA changes.
pub async fn try_run(
    database_url: &str,
    user: &str,
    libl: &[String],
    curlib: Option<String>,
    schema: &mut Option<String>,
    stmt: &str,
) -> Result<Option<String>> {
    // SQL procedures and functions.
    if let Some((replace, r)) = crate::sqlpl::parse_create(stmt)? {
        let client = crate::runtime::connect_db(database_url).await?;
        let target = schema
            .clone()
            .or_else(|| curlib.clone())
            .unwrap_or_else(|| "QGPL".into());
        let search = match schema {
            Some(s) => vec![s.clone()],
            None => libl.to_vec(),
        };
        return crate::sqlpl::create(&client, database_url, &search, target, r, replace)
            .await
            .map(Some);
    }
    if let Some((kind, lib, name, if_exists)) = crate::sqlpl::parse_drop(stmt)? {
        let client = crate::runtime::connect_db(database_url).await?;
        let search = match (&lib, schema) {
            (Some(_), _) => vec![],
            (None, Some(s)) => vec![s.clone()],
            (None, None) => libl.to_vec(),
        };
        return crate::sqlpl::drop_routine(&client, &search, &kind, lib, &name, if_exists)
            .await
            .map(Some);
    }
    let Some(ddl) = parse(stmt)? else {
        return Ok(None);
    };
    let mut client = crate::runtime::connect_db(database_url).await?;
    let mut ctx = Ctx {
        client: &mut client,
        user,
        libl,
        curlib,
        database_url: database_url.to_string(),
        schema: schema.clone(),
    };
    let msg = run(&mut ctx, &ddl).await?;
    *schema = ctx.schema;
    Ok(Some(msg))
}

/// Split an SQL script into statements at semicolons outside constants,
/// quoted names and comments (`--` to the end of the line, `/* */`).
/// Comments are left out; empty statements are skipped.
pub fn split_statements(script: &str) -> Vec<String> {
    let cs: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut blocks = 0i32;
    let mut cases = 0i32;
    while i < cs.len() {
        let c = cs[i];
        if c == '-' && cs.get(i + 1) == Some(&'-') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            cur.push(' ');
            continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while i < cs.len() && !(cs[i] == '*' && cs.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            cur.push(' ');
            continue;
        }
        if c == '\'' || c == '"' {
            cur.push(c);
            i += 1;
            while i < cs.len() {
                cur.push(cs[i]);
                if cs[i] == c {
                    if cs.get(i + 1) == Some(&c) {
                        cur.push(c);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Words: BEGIN ... END of compound statements (routine bodies)
        // keep their semicolons.
        if c.is_ascii_alphabetic() || c == '_' {
            let s0 = i;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || "_$#@".contains(cs[i])) {
                i += 1;
            }
            let w: String = cs[s0..i].iter().collect::<String>().to_ascii_uppercase();
            let prev_ok =
                s0 == 0 || !(cs[s0 - 1].is_ascii_alphanumeric() || "_$#@.".contains(cs[s0 - 1]));
            if prev_ok {
                match w.as_str() {
                    "BEGIN" => blocks += 1,
                    "CASE" => cases += 1,
                    "END" => {
                        // The word after END.
                        let mut j = i;
                        while j < cs.len() && cs[j].is_whitespace() {
                            j += 1;
                        }
                        let k0 = j;
                        while j < cs.len() && cs[j].is_ascii_alphanumeric() {
                            j += 1;
                        }
                        let next: String =
                            cs[k0..j].iter().collect::<String>().to_ascii_uppercase();
                        match next.as_str() {
                            "IF" | "WHILE" | "LOOP" | "REPEAT" | "FOR" => {}
                            "CASE" => cases = (cases - 1).max(0),
                            _ if cases > 0 => cases -= 1,
                            _ => blocks = (blocks - 1).max(0),
                        }
                    }
                    _ => {}
                }
            }
            cur.extend(&cs[s0..i]);
            continue;
        }
        if c == ';' && blocks == 0 {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn db(e: tokio_postgres::Error) -> anyhow::Error {
    crate::sql::map_error(&e, "")
}

/// What a view was created from: kept with the view, so that it can be
/// made again when the tables under it change (Db2 for i recreates
/// dependent views on ALTER TABLE).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewDef {
    pub sql_name: String,
    pub query: String,
    pub columns: Vec<String>,
    pub check_option: bool,
    pub format: Option<String>,
    /// The library list the query's unqualified names were found in.
    pub libl: Vec<String>,
}

/// Create a view and describe it as a file.
async fn make_view(
    client: &mut Client,
    url: &str,
    lib: &str,
    sys: &str,
    def: &ViewDef,
    owner: &str,
) -> Result<()> {
    let alias = (sys != def.sql_name).then_some(def.sql_name.as_str());
    crate::sql::create_view(
        client,
        url,
        &def.libl,
        lib,
        sys,
        alias,
        &def.columns,
        &def.query,
        def.check_option,
    )
    .await?;
    let registered = async {
        catalog::register_view(client, lib, sys, def.format.as_deref().unwrap_or(sys), def).await?;
        if let Some(a) = alias {
            catalog::set_sql_name(client, lib, sys, a, false).await?;
        }
        authority::set_owner(client, lib, sys, "*FILE", owner).await
    }
    .await;
    if let Err(e) = registered {
        let _ = client
            .batch_execute(&format!(
                "DROP VIEW IF EXISTS {}.{} CASCADE",
                ident(lib),
                ident(sys)
            ))
            .await;
        let _ = catalog::delete_file(client, lib, sys, true).await;
        return Err(e);
    }
    Ok(())
}

/// The views over a table, with what they were made from, in the order
/// they can be made again (a view before the views over it).
async fn views_to_rebuild(
    client: &mut Client,
    lib: &str,
    sys: &str,
) -> Result<Vec<(String, String, ViewDef, String)>> {
    let mut out = Vec::new();
    for (l, n) in catalog::views_over(client, lib, sys).await? {
        let r = client
            .query_one(
                "SELECT f.view_def, o.owner FROM as4tainr.files f
                   JOIN as4tainr.objects o ON o.library=f.library AND o.name=f.name AND o.objtype='*FILE'
                  WHERE f.library=$1 AND f.name=$2",
                &[&l, &n],
            )
            .await?;
        let def: Option<String> = r.get(0);
        let Some(def) = def.and_then(|d| serde_json::from_str::<ViewDef>(&d).ok()) else {
            continue;
        };
        out.push((l, n, def, r.get(1)));
    }
    Ok(out)
}

/// Make again the view that gives a table its SQL name (it lists the
/// table's columns, so it follows a change of columns).
async fn remake_alias(c: &Client, view: &Option<String>, tbl: &str) -> Result<()> {
    if let Some(v) = view {
        // A new column comes last: the view can be replaced in place,
        // keeping the views built over it.
        if c.batch_execute(&format!(
            "CREATE OR REPLACE VIEW {v} AS SELECT * FROM {tbl}"
        ))
        .await
        .is_err()
        {
            c.batch_execute(&format!(
                "DROP VIEW IF EXISTS {v}; CREATE VIEW {v} AS SELECT * FROM {tbl}"
            ))
            .await
            .map_err(db)?;
        }
    }
    Ok(())
}

/// ALTER TABLE.
async fn alter(ctx: &mut Ctx<'_>, table: &Name, action: &Alter) -> Result<String> {
    let Some((lib, attr, sys)) = ctx.find(table).await? else {
        return Err(not_found(table, "table"));
    };
    let f = catalog::resolve_file(ctx.client, std::slice::from_ref(&lib), &sys).await?;
    if attr != "PF" || f.file.view.is_some() {
        bail!("SQL0156: {} is not a table.", table.name);
    }
    ctx.may_delete(&lib, &sys).await?;
    let tbl = format!("{}.{}", ident(&lib), ident(&sys));
    let alias: Option<String> = ctx
        .client
        .query_one(
            "SELECT sql_name FROM as4tainr.files WHERE library=$1 AND name=$2",
            &[&lib, &sys],
        )
        .await?
        .get(0);
    let fields = f.file.fields.clone();
    let field = |c: &str| -> Result<FieldDef> {
        fields
            .iter()
            .find(|x| x.is_named(c))
            .cloned()
            .ok_or_else(|| anyhow!("SQL0205: Column {c} not in table {}.", table.name))
    };
    // The view giving the table its SQL name lists the table's columns:
    // it is made again after the columns change.
    let alias_view = alias
        .as_ref()
        .map(|a| format!("{}.{}", ident(&lib), ident(a)));
    match action {
        Alter::AddColumn {
            field: new,
            uniques,
            checks,
        } => {
            let mut new = new.clone();
            let mut uniques = uniques.clone();
            if let Some(a) = new.alias.clone() {
                if fields.iter().any(|x| x.is_named(&a)) {
                    bail!("SQL0612: {a} is a duplicate column name.");
                }
                // A generated system name is numbered among the table's
                // columns.
                if fields.iter().any(|x| x.is_named(&new.name)) && new.name.ends_with("00001") {
                    let old = new.name.clone();
                    new.name = generated_name(&a, &|c| fields.iter().any(|x| x.is_named(c)))?;
                    for (_, cols) in uniques.iter_mut() {
                        for c in cols.iter_mut() {
                            if *c == old {
                                *c = new.name.clone();
                            }
                        }
                    }
                }
            }
            if fields.iter().any(|x| x.is_named(&new.name)) {
                bail!("SQL0612: {} is a duplicate column name.", new.name);
            }
            let tx = ctx.client.transaction().await?;
            let seq = fields.len() as i32;
            catalog::insert_field(&tx, &lib, &sys, seq, &new).await?;
            let coll = if new.data_type == DataType::Char {
                " COLLATE \"C\""
            } else {
                ""
            };
            tx.batch_execute(&format!(
                "ALTER TABLE {tbl} ADD COLUMN {} {}{coll}{}{}",
                new.column(),
                new.sql_type(),
                if new.null_capable { "" } else { " NOT NULL" },
                match &new.identity {
                    Some(i) => i.sql(),
                    None => new.default_clause(),
                }
            ))
            .await
            .map_err(db)?;
            tx.commit().await?;
            constraints(ctx.client, &lib, &sys, &uniques, checks).await?;
            remake_alias(ctx.client, &alias_view, &tbl).await?;
            Ok(format!(
                "Column {} added to {}.",
                new.alias.as_ref().unwrap_or(&new.name),
                table.name
            ))
        }
        Alter::AddUnique {
            name,
            primary,
            columns,
        } => {
            let mut cols = Vec::new();
            for c in columns {
                let fd = field(c)?;
                if *primary && fd.null_capable {
                    bail!("SQL0542: Column {c} of a primary key must be NOT NULL.");
                }
                cols.push(fd.name);
            }
            constraints(ctx.client, &lib, &sys, &[(name.clone(), cols)], &[]).await?;
            Ok(format!("Constraint {name} added to {}.", table.name))
        }
        Alter::AddCheck { name, condition } => {
            let toks: Vec<Tok> = tokenize(condition)?
                .into_iter()
                .filter(|t| !matches!(t, Tok::Space(_)))
                .collect();
            let cond = check_sql(&toks, &fields)?;
            let cname = name
                .clone()
                .unwrap_or_else(|| format!("C_{}", std::process::id() % 100000));
            constraints(ctx.client, &lib, &sys, &[], &[(cname.clone(), cond)]).await?;
            Ok(format!("Check constraint {cname} added to {}.", table.name))
        }
        Alter::DropConstraint { name } => {
            let obj = format!("{sys}_{name}");
            let index = format!("{}.{}", ident(&lib), ident(&obj));
            let is_index = ctx
                .client
                .query_one("SELECT to_regclass($1) IS NOT NULL", &[&index])
                .await?
                .get::<_, bool>(0);
            if is_index {
                ctx.client
                    .batch_execute(&format!("DROP INDEX {index}"))
                    .await
                    .map_err(db)?;
            } else {
                let found = ctx
                    .client
                    .query_opt(
                        "SELECT 1 FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
                           JOIN pg_namespace n ON n.oid = t.relnamespace
                          WHERE n.nspname = lower($1) AND t.relname = lower($2) AND c.conname = lower($3)",
                        &[&lib, &sys, &obj],
                    )
                    .await?
                    .is_some();
                if !found {
                    bail!("SQL0204: Constraint {name} not found for {}.", table.name);
                }
                ctx.client
                    .batch_execute(&format!(
                        "ALTER TABLE {tbl} DROP CONSTRAINT {}",
                        ident(&obj)
                    ))
                    .await
                    .map_err(db)?;
            }
            Ok(format!("Constraint {name} dropped from {}.", table.name))
        }
        Alter::DropColumn { column, cascade } => {
            let fd = field(column)?;
            if fields.len() == 1 {
                bail!(
                    "SQL0195: The last column of {} cannot be dropped.",
                    table.name
                );
            }
            if f.file.keys.contains(&fd.name) {
                bail!(
                    "SQL0084: dropping a key field of a keyed physical file is not supported yet."
                );
            }
            // Logical files and SQL views that use the column.
            let mut users: Vec<(String, String)> = ctx
                .client
                .query(
                    "SELECT DISTINCT l.library, l.name FROM as4tainr.logical_files l
                       JOIN as4tainr.file_fields ff ON ff.library = l.library AND ff.file = l.name
                      WHERE l.pf_library = $1 AND l.pf_name = $2 AND ff.name = $3",
                    &[&lib, &sys, &fd.name],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect();
            users.extend(catalog::views_using_column(ctx.client, &lib, &sys, &fd).await?);
            if !cascade {
                // RESTRICT: no constraint or index may use the column either.
                let used = ctx
                    .client
                    .query_opt(
                        "SELECT 1 FROM pg_attribute a
                          WHERE a.attrelid = to_regclass(format('%I.%I', lower($1), lower($2)))
                            AND a.attname = $3
                            AND (EXISTS (SELECT 1 FROM pg_constraint c
                                          WHERE c.conrelid = a.attrelid AND a.attnum = ANY(c.conkey))
                              OR EXISTS (SELECT 1 FROM pg_index i
                                          WHERE i.indrelid = a.attrelid AND a.attnum = ANY(i.indkey::int2[])))",
                        &[&lib, &sys, &catalog::pg_name(fd.alias.as_deref().unwrap_or(&fd.name))],
                    )
                    .await?
                    .is_some();
                if used {
                    bail!("SQL0478: Column {column} cannot be dropped: a constraint or index uses it.");
                }
            }
            if let Some((l, n)) = users.first() {
                if !cascade {
                    bail!("SQL0478: Column {column} cannot be dropped: {l}/{n} uses it.");
                }
            }
            // The views over the table go, and those that do not use the
            // column are made again afterwards (the others are dropped with
            // the column, with the views over them).
            let views = views_to_rebuild(ctx.client, &lib, &sys).await?;
            for (l, n) in users
                .iter()
                .cloned()
                .chain(views.iter().map(|(l, n, _, _)| (l.clone(), n.clone())))
            {
                if catalog::object_exists(ctx.client, std::slice::from_ref(&l), &n, "*FILE").await?
                {
                    catalog::delete_file(ctx.client, &l, &n, true).await?;
                }
            }
            let tx = ctx.client.transaction().await?;
            tx.batch_execute(&format!(
                "ALTER TABLE {tbl} DROP COLUMN {} CASCADE",
                fd.column()
            ))
            .await
            .map_err(db)?;
            let seq: i32 = tx
                .query_one(
                    "DELETE FROM as4tainr.file_fields WHERE library=$1 AND file=$2 AND name=$3 RETURNING seq",
                    &[&lib, &sys, &fd.name],
                )
                .await?
                .get(0);
            tx.execute(
                "UPDATE as4tainr.file_fields SET seq = seq - 1 WHERE library=$1 AND file=$2 AND seq > $3",
                &[&lib, &sys, &seq],
            )
            .await?;
            tx.commit().await?;
            remake_alias(ctx.client, &alias_view, &tbl).await?;
            let url = ctx.database_url.clone();
            let mut gone: Vec<(String, String)> = users.clone();
            for (l, n, def, owner) in &views {
                if gone.contains(&(l.clone(), n.clone())) {
                    continue;
                }
                if make_view(ctx.client, &url, l, n, def, owner).await.is_err() {
                    gone.push((l.clone(), n.clone()));
                }
            }
            Ok(format!("Column {column} dropped from {}.", table.name))
        }
        Alter::AlterColumn { column, change } => {
            let fd = field(column)?;
            let mut nf = fd.clone();
            let col = fd.column();
            let pg = match change {
                ColumnChange::DataType(t, l, d, v) => {
                    nf.data_type = *t;
                    nf.length = *l;
                    nf.decimals = *d;
                    nf.varlen = *v;
                    nf.double = *t == DataType::Float && *l > 9;
                    if fd.data_type.is_numeric() != t.is_numeric() && fd.data_type != *t {
                        // Character and number columns do not convert.
                        if fd.data_type == DataType::Char || *t == DataType::Char {
                            bail!("SQL0408: Column {column} cannot change between character and numeric types.");
                        }
                    }
                    format!(
                        "ALTER TABLE {tbl} ALTER COLUMN {col} TYPE {} USING {col}::{}",
                        nf.sql_type(),
                        nf.sql_type()
                    )
                }
                ColumnChange::SetDefault(FieldDefault::Value(text)) => {
                    let toks: Vec<Tok> = tokenize(text)?
                        .into_iter()
                        .filter(|t| !matches!(t, Tok::Space(_)))
                        .collect();
                    let mut q = P { t: toks, i: 0 };
                    nf.default = Some(default_value(&mut q, &fd)?);
                    if let Some(t) = q.t.get(q.i) {
                        bail!("SQL0104: Token {} not valid here.", show(t));
                    }
                    let clause = nf.default_clause();
                    format!("ALTER TABLE {tbl} ALTER COLUMN {col} SET{clause}")
                }
                ColumnChange::SetDefault(_) => unreachable!("parsed as a value"),
                ColumnChange::DropDefault => {
                    nf.default = if fd.null_capable {
                        None
                    } else {
                        Some(FieldDefault::Required)
                    };
                    format!("ALTER TABLE {tbl} ALTER COLUMN {col} DROP DEFAULT")
                }
                ColumnChange::SetNotNull => {
                    nf.null_capable = false;
                    if nf.default.is_none() {
                        nf.default = Some(FieldDefault::Required);
                    }
                    format!("ALTER TABLE {tbl} ALTER COLUMN {col} SET NOT NULL")
                }
                ColumnChange::DropNotNull => {
                    nf.null_capable = true;
                    format!("ALTER TABLE {tbl} ALTER COLUMN {col} DROP NOT NULL")
                }
            };
            let retype = matches!(change, ColumnChange::DataType(..));
            // Changing a column's type: the views over the table are made
            // again afterwards.
            let views = if retype {
                let v = views_to_rebuild(ctx.client, &lib, &sys).await?;
                for (l, n, _, _) in &v {
                    if catalog::object_exists(ctx.client, std::slice::from_ref(l), n, "*FILE")
                        .await?
                    {
                        catalog::delete_file(ctx.client, l, n, true).await?;
                    }
                }
                v
            } else {
                Vec::new()
            };
            let tx = ctx.client.transaction().await?;
            if retype {
                if let Some(v) = &alias_view {
                    tx.batch_execute(&format!("DROP VIEW IF EXISTS {v}"))
                        .await
                        .map_err(db)?;
                }
            }
            tx.batch_execute(&pg).await.map_err(db)?;
            // The field, in the table and in the logical files over it.
            tx.execute(
                "UPDATE as4tainr.file_fields ff SET data_type=$4, length=$5, decimals=$6, varlen=$7, null_capable=$8, dft=$9
                  WHERE ff.name=$3 AND ((ff.library=$1 AND ff.file=$2) OR EXISTS (
                    SELECT 1 FROM as4tainr.logical_files l
                     WHERE l.library=ff.library AND l.name=ff.file AND l.pf_library=$1 AND l.pf_name=$2))",
                &[
                    &lib,
                    &sys,
                    &fd.name,
                    &nf.data_type.code(),
                    &(nf.length as i32),
                    &(nf.decimals as i32),
                    &nf.varlen,
                    &nf.null_capable,
                    &nf.default.as_ref().map(|d| serde_json::to_string(d).unwrap_or_default()),
                ],
            )
            .await?;
            tx.commit().await?;
            if retype {
                remake_alias(ctx.client, &alias_view, &tbl).await?;
            }
            let url = ctx.database_url.clone();
            for (l, n, def, owner) in &views {
                make_view(ctx.client, &url, l, n, def, owner).await?;
            }
            Ok(format!("Column {column} of {} changed.", table.name))
        }
    }
}

/// A new table's unique and check constraints.
async fn constraints(
    client: &Client,
    lib: &str,
    table: &str,
    uniques: &[(String, Vec<String>)],
    checks: &[(String, String)],
) -> Result<()> {
    for (cname, cols) in uniques {
        catalog::add_unique_constraint(client, lib, table, cname, cols).await?;
    }
    for (cname, cond) in checks {
        // The condition was rebuilt from checked tokens: quoted columns of
        // this table and quoted constants.
        client
            .batch_execute(&format!(
                "ALTER TABLE {}.{} ADD CONSTRAINT {} CHECK ({cond})",
                ident(lib),
                ident(table),
                ident(&format!("{table}_{cname}")),
            ))
            .await
            .map_err(|e| anyhow!("SQL0546: CHECK condition {cond} is not valid: {e}"))?;
    }
    Ok(())
}

fn title(kind: &str) -> String {
    let mut c = kind.to_ascii_lowercase();
    if let Some(f) = c.get_mut(..1) {
        f.make_ascii_uppercase();
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(sql: &str) -> (PhysicalFile, Uniques, Checks) {
        match parse(sql).unwrap().unwrap() {
            Ddl::CreateTable {
                file,
                uniques,
                checks,
                ..
            } => (file, uniques, checks),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn create_table_columns() {
        // Db2 for i SQL Reference, CREATE TABLE: a column is null-capable
        // unless NOT NULL; WITH DEFAULT without a value is the type's
        // default; PRIMARY KEY columns are NOT NULL; RCDFMT names the
        // record format (the table name otherwise). Data types and their
        // DDS equivalents: SMALLINT/INTEGER/BIGINT are binary 4/9/18
        // digits, DECIMAL packed, NUMERIC zoned.
        let (f, u, c) = table(
            "CREATE OR REPLACE TABLE NICK.PERSON (
               ID INTEGER NOT NULL PRIMARY KEY,
               NAME VARCHAR(50) NOT NULL WITH DEFAULT,
               BAL DECIMAL(9, 2) DEFAULT -1.5,
               QTY NUMERIC(5),
               DOB DATE,
               CODE CHAR(1) CCSID 37 NOT NULL DEFAULT 'Y' CHECK (CODE IN ('Y', 'N')),
               CONSTRAINT PERSON_U UNIQUE (NAME)
             ) RCDFMT PERSONR",
        );
        assert_eq!(f.format, "PERSONR");
        let g = |n: &str| f.fields.iter().find(|x| x.name == n).unwrap().clone();
        assert_eq!((g("ID").data_type, g("ID").length), (DataType::Binary, 9));
        assert!(!g("ID").null_capable);
        assert!(g("NAME").varlen && !g("NAME").null_capable);
        assert_eq!(g("NAME").default, Some(FieldDefault::System));
        assert_eq!(g("BAL").default, Some(FieldDefault::Value("-1.5".into())));
        assert!(g("BAL").null_capable);
        assert_eq!(
            (g("QTY").data_type, g("QTY").length, g("QTY").decimals),
            (DataType::Zoned, 5, 0)
        );
        assert_eq!(g("DOB").data_type, DataType::Date);
        assert_eq!(
            u,
            vec![
                ("PK".to_string(), vec!["ID".to_string()]),
                ("PERSON_U".to_string(), vec!["NAME".to_string()])
            ]
        );
        assert_eq!(
            c,
            vec![("C1".to_string(), "\"code\" IN ( 'Y' , 'N' )".to_string())]
        );
    }

    #[test]
    fn checks_are_limited_to_columns_and_constants() {
        let (_, _, c) = table("CREATE TABLE T (N VARCHAR(10), CHECK (LENGTH(TRIM(N)) > 0))");
        assert_eq!(c[0].1, "length ( btrim ( \"n\" ) ) > 0");
        let e = parse("CREATE TABLE T (A INT CHECK (A > PG_SLEEP(1)))")
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("SQL0084"), "{e}");
        let (_, _, c) = table("CREATE TABLE T (A INT, B INT, CHECK (A <> B AND A >= 0))");
        assert_eq!(c[0].1, "\"a\" <> \"b\" AND \"a\" >= 0");
    }

    #[test]
    fn unsupported_definitions_say_so() {
        for sql in [
            "CREATE TABLE T (A INT GENERATED ALWAYS AS (B + 1))",
            "CREATE VIEW V AS SELECT 1 FROM T WITH LOCAL CHECK OPTION",
            "ALTER TABLE T ADD COLUMN B INT BEFORE A",
            "CREATE TABLE \"lower\" (A INT)",
        ] {
            let e = parse(sql).unwrap_err().to_string();
            assert!(e.starts_with("SQL0084"), "{sql}: {e}");
        }
        // Queries and changes are not data definition.
        assert_eq!(parse("SELECT * FROM T").unwrap(), None);
        assert_eq!(parse("SET PATH = X").unwrap(), None);
    }

    #[test]
    fn long_names_get_system_names() {
        // Db2 for i: an SQL name that is not a valid system name gets a
        // system name of its first five characters and a five-digit number
        // (COUNTRY_CODE -> COUNT00001, the next COUNT... -> COUNT00002);
        // FOR COLUMN names it; RCDFMT defaults to the system table name.
        let (f, u, _) = table(
            "CREATE TABLE COUNTRY_LIST FOR SYSTEM NAME COUNTRIES (
               COUNTRY_CODE CHAR(2) NOT NULL PRIMARY KEY,
               COUNTRY_NAME VARCHAR(40),
               POPULATION_COUNT FOR COLUMN POP BIGINT,
               ID INT)",
        );
        let names: Vec<(String, Option<String>)> = f
            .fields
            .iter()
            .map(|x| (x.name.clone(), x.alias.clone()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("COUNT00001".into(), Some("COUNTRY_CODE".into())),
                ("COUNT00002".into(), Some("COUNTRY_NAME".into())),
                ("POP".into(), Some("POPULATION_COUNT".into())),
                ("ID".into(), None),
            ]
        );
        assert_eq!(u, vec![("PK".to_string(), vec!["COUNT00001".to_string()])]);
        assert_eq!(f.format, "");
        assert_eq!(
            generated_name("CUSTOMER_MASTER", &|c| c == "CUSTO00001").unwrap(),
            "CUSTO00002"
        );
    }

    #[test]
    fn views_and_identity_columns() {
        // CREATE VIEW: the query is kept as written; WITH CHECK OPTION
        // and RCDFMT come after it (Db2 for i SQL Reference, CREATE VIEW).
        assert_eq!(
            parse("CREATE OR REPLACE VIEW BIG_ORDERS (NUM, AMT) AS SELECT ORDNO, AMOUNT FROM ORDERS WHERE AMOUNT > 100 WITH CHECK OPTION RCDFMT BIGR").unwrap(),
            Some(Ddl::CreateView {
                name: Name { lib: None, name: "BIG_ORDERS".into() },
                system: None,
                replace: true,
                columns: vec!["NUM".into(), "AMT".into()],
                query: "SELECT ORDNO, AMOUNT FROM ORDERS WHERE AMOUNT > 100".into(),
                check_option: true,
                format: Some("BIGR".into()),
            })
        );
        // Identity columns: integers or decimals with no decimal places,
        // ALWAYS or BY DEFAULT, with START WITH and INCREMENT BY.
        let (f, _, _) = table(
            "CREATE TABLE T (ID INTEGER GENERATED ALWAYS AS IDENTITY (START WITH 100 INCREMENT BY 10), N INT GENERATED BY DEFAULT AS IDENTITY)",
        );
        assert_eq!(
            f.fields[0].identity.as_ref().unwrap().sql(),
            " GENERATED ALWAYS AS IDENTITY (START WITH 100 INCREMENT BY 10)"
        );
        assert_eq!(
            f.fields[1].identity.as_ref().unwrap().sql(),
            " GENERATED BY DEFAULT AS IDENTITY"
        );
        assert!(parse("CREATE TABLE T (X DECIMAL(5,2) GENERATED ALWAYS AS IDENTITY)").is_err());
    }

    #[test]
    fn scripts_split_at_semicolons() {
        let s = split_statements(
            "-- a comment; still a comment\nINSERT INTO T VALUES ('a;b', \"x;y\");\n/* block; */ DROP TABLE T;\n;\n  ",
        );
        assert_eq!(
            s,
            vec!["INSERT INTO T VALUES ('a;b', \"x;y\")", "DROP TABLE T"]
        );
    }

    #[test]
    fn index_drop_label() {
        assert_eq!(
            parse("CREATE UNIQUE INDEX PERSONIX ON NICK/PERSON (NAME DESC, ID) RCDFMT PERSONR")
                .unwrap(),
            Some(Ddl::CreateIndex {
                name: Name {
                    lib: None,
                    name: "PERSONIX".into()
                },
                system: None,
                table: Name {
                    lib: Some("NICK".into()),
                    name: "PERSON".into()
                },
                unique: true,
                keys: vec![("NAME".into(), true), ("ID".into(), false)],
                format: Some("PERSONR".into()),
            })
        );
        assert_eq!(
            parse("drop table if exists person").unwrap(),
            Some(Ddl::Drop {
                kind: "TABLE",
                name: Name {
                    lib: None,
                    name: "PERSON".into()
                },
                if_exists: true
            })
        );
        // LABEL ON COLUMN: TEXT IS is the field text; plain IS is the
        // column heading, which is not kept.
        assert_eq!(
            parse("LABEL ON COLUMN PERSON (NAME IS 'Name', NAME TEXT IS 'Full name')").unwrap(),
            Some(Ddl::LabelColumns {
                table: Name {
                    lib: None,
                    name: "PERSON".into()
                },
                texts: vec![("NAME".into(), "Full name".into())],
            })
        );
        assert_eq!(
            parse("label on table nick.person is 'People'").unwrap(),
            Some(Ddl::LabelObject {
                name: Name {
                    lib: Some("NICK".into()),
                    name: "PERSON".into()
                },
                text: "People".into()
            })
        );
        assert_eq!(
            parse("SET SCHEMA = mylib").unwrap(),
            Some(Ddl::SetSchema("MYLIB".into()))
        );
        // A long SQL name is accepted by DROP ... IF EXISTS (nothing has it).
        assert!(matches!(
            parse("DROP VIEW IF EXISTS VW_FOOD_PUBLIC").unwrap(),
            Some(Ddl::Drop {
                if_exists: true,
                ..
            })
        ));
        assert!(parse("DROP VIEW VW_FOOD_PUBLIC").is_ok());
    }
}
