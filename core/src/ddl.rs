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
        replace: bool,
        file: PhysicalFile,
        /// PRIMARY KEY and UNIQUE constraints: (constraint name, columns).
        uniques: Vec<(String, Vec<String>)>,
        /// CHECK constraints: (constraint name, condition as SQL).
        checks: Vec<(String, String)>,
    },
    CreateIndex {
        name: Name,
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
    /// LABEL ON TABLE / INDEX / VIEW: the object's text.
    LabelObject { name: Name, text: String },
    /// LABEL ON COLUMN with TEXT IS: the fields' text. Column headings
    /// (plain `IS`) are not kept.
    LabelColumns {
        table: Name,
        texts: Vec<(String, String)>,
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
                    p.i += 1;
                    create_table(&mut p, replace)?
                }
                Some("INDEX") if !replace => {
                    p.i += 1;
                    create_index(&mut p, unique)?
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
        "ALTER" | "RENAME" | "TRUNCATE" | "GRANT" | "REVOKE" => {
            bail!("SQL0084: {first} is not supported yet.")
        }
        _ => return Ok(None),
    };
    if let Some(t) = p.t.get(p.i) {
        bail!("SQL0104: Token {} not valid here.", show(t));
    }
    Ok(Some(ddl))
}

fn sql0104(e: anyhow::Error) -> anyhow::Error {
    anyhow!("SQL0104: {e}.")
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
        Tok::Str(s) => format!("'{s}'"),
        Tok::Quoted(s) => format!("\"{s}\""),
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

    /// A field name (system names: 10 characters at most).
    fn field(&mut self) -> Result<String> {
        let n = self.ident()?;
        validate_name("column", &n).map_err(|_| {
            anyhow!("SQL0084: column name {n}: names longer than 10 characters or with special characters are not supported yet.")
        })
    }

    /// `name`, `lib.name` or `lib/name`.
    fn name(&mut self) -> Result<Name> {
        let first = self.ident()?;
        let (lib, name) = if self.punct(".") || self.punct("/") {
            (Some(first), self.ident()?)
        } else {
            (None, first)
        };
        let check = |kind: &str, n: &str| {
            validate_name(kind, n).map_err(|_| {
                anyhow!("SQL0084: {kind} name {n}: names longer than 10 characters or with special characters are not supported yet.")
            })
        };
        Ok(Name {
            lib: lib.map(|l| check("schema", &l)).transpose()?,
            name: check("object", &name)?,
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
        "DATE" => (DataType::Date, 10, 0, false),
        "TIME" => (DataType::Time, 8, 0, false),
        "TIMESTAMP" => {
            let n = size(p, 6)?;
            if n != 6 {
                bail!("SQL0084: TIMESTAMP({n}): only 6 fractional digits are supported yet.");
            }
            (DataType::Timestamp, 26, 0, false)
        }
        other => bail!("SQL0084: data type {other} is not supported yet."),
    };
    match t {
        (DataType::Char, n, _, _) if n == 0 || n > 32740 => {
            bail!("SQL0604: Length {n} is not valid for a character column.")
        }
        (DataType::Packed | DataType::Zoned, d, s, _) if d == 0 || d > 63 || s > d => {
            bail!("SQL0604: Precision {d},{s} is not valid.")
        }
        _ => {}
    }
    // CCSID and FOR BIT DATA: character data is kept as text.
    loop {
        if p.eat("CCSID") {
            p.number()?;
        } else if p.eat("FOR") {
            p.expect("BIT")?;
            p.expect("DATA")?;
        } else {
            break;
        }
    }
    Ok(t)
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
    let mut out = Vec::new();
    for t in toks {
        let piece = match t {
            Tok::Word(w) | Tok::Quoted(w) => {
                let up = if matches!(t, Tok::Word(_)) {
                    w.to_ascii_uppercase()
                } else {
                    w.clone()
                };
                if matches!(t, Tok::Word(_)) && WORDS.contains(&up.as_str()) {
                    up
                } else if fields.iter().any(|f| f.name == up) {
                    ident(&up)
                } else {
                    bail!("SQL0084: CHECK ({}): only comparisons of the table's columns with constants are supported yet.", toks.iter().map(show).collect::<Vec<_>>().join(" "));
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

fn create_table(p: &mut P, replace: bool) -> Result<Ddl> {
    let name = p.name()?;
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
                if fields.iter().any(|f| f.name == fname) {
                    bail!("SQL0612: {fname} is a duplicate column name.");
                }
                let (t, len, dec, varlen) = data_type(p)?;
                let mut f = FieldDef::new(&fname, t, len, dec);
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
                        not_null.push(fname.clone());
                        uniques.push(("PK".into(), vec![fname.clone()]));
                    } else if p.eat("UNIQUE") {
                        uniques.push((format!("U{}", uniques.len() + 1), vec![fname.clone()]));
                    } else if p.eat("CHECK") {
                        p.expect_punct("(")?;
                        checks.push((None, p.group()?));
                    } else if p.eat("CONSTRAINT") {
                        p.ident()?;
                    } else if p.eat("CCSID") {
                        p.number()?;
                    } else if p.eat("GENERATED") {
                        bail!("SQL0084: GENERATED columns (identity, row change timestamp, expressions) are not supported yet.");
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
    let mut format = name.name.clone();
    loop {
        if p.eat("RCDFMT") {
            format = p.field()?;
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
        file: PhysicalFile {
            name: name.name.clone(),
            format,
            text: String::new(),
            unique: false,
            fields,
            keys: Vec::new(),
            descending: Vec::new(),
        },
        name,
        replace,
        uniques,
        checks,
    })
}

fn create_index(p: &mut P, unique: bool) -> Result<Ddl> {
    let name = p.name()?;
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
        format = Some(p.field()?);
        if p.eat("ADD") {
            bail!("SQL0084: RCDFMT ... ADD is not supported yet.");
        }
    }
    if p.eat("WHERE") {
        bail!("SQL0084: indexes with WHERE are not supported yet.");
    }
    Ok(Ddl::CreateIndex {
        name,
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
            let col = validate_name("column", &col).map_err(sql0104)?;
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
    let v = |k: &str, n: &str| validate_name(k, n).map_err(sql0104);
    match parts {
        [n] => Ok(Name {
            lib: None,
            name: v("table", n)?,
        }),
        [l, n] => Ok(Name {
            lib: Some(v("schema", l)?),
            name: v("table", n)?,
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

    /// (library, attribute) of an existing file.
    async fn find(&self, n: &Name) -> Result<Option<(String, String)>> {
        for lib in self.search(n) {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[&lib, &n.name],
                )
                .await?
            {
                return Ok(Some((lib, r.get(0))));
            }
        }
        Ok(None)
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
            replace,
            file,
            uniques,
            checks,
        } => {
            let lib = ctx.target(name);
            if !catalog::library_exists(ctx.client, &lib).await? {
                bail!("SQL0204: Schema {lib} not found.");
            }
            let existing = Name {
                lib: Some(lib.clone()),
                name: name.name.clone(),
            };
            if let Some((_, attr)) = ctx.find(&existing).await? {
                if !replace || attr != "PF" {
                    bail!("SQL0601: {} in {lib} type *FILE already exists.", name.name);
                }
                ctx.may_delete(&lib, &name.name).await?;
                catalog::delete_file(ctx.client, &lib, &name.name, true).await?;
            }
            catalog::create_physical_file(ctx.client, &lib, file).await?;
            if let Err(e) = constraints(ctx.client, &lib, &name.name, uniques, checks).await {
                let _ = catalog::delete_file(ctx.client, &lib, &name.name, true).await;
                return Err(e);
            }
            authority::set_owner(ctx.client, &lib, &name.name, "*FILE", ctx.user).await?;
            Ok(format!("Table {} created in {lib}.", name.name))
        }
        Ddl::CreateIndex {
            name,
            table,
            unique,
            keys,
            format,
        } => {
            let Some((tlib, attr)) = ctx.find(table).await? else {
                return Err(not_found(table, "table"));
            };
            if attr != "PF" {
                bail!("SQL0156: {} is not a table.", table.name);
            }
            authority::check(ctx.client, ctx.user, &tlib, &table.name, "*FILE", Aut::Use).await?;
            let lib = ctx.target(name);
            let tfile =
                catalog::resolve_file(ctx.client, std::slice::from_ref(&tlib), &table.name).await?;
            let lf = LogicalFile {
                name: name.name.clone(),
                // The table's record format unless RCDFMT names another.
                format: format.clone().unwrap_or_else(|| tfile.file.format.clone()),
                text: String::new(),
                unique: *unique,
                based_on: format!("{tlib}/{}", table.name),
                fields: Vec::new(),
                keys: keys.iter().map(|(k, _)| k.clone()).collect(),
                descending: keys
                    .iter()
                    .filter(|(_, d)| *d)
                    .map(|(k, _)| k.clone())
                    .collect(),
                select_omit: Vec::new(),
            };
            let libl = ctx.libl.to_vec();
            catalog::create_logical_file(ctx.client, &libl, &lib, &lf)
                .await
                .map_err(|e| {
                    let m = e.to_string();
                    if m.starts_with("CPF7302") {
                        anyhow!("SQL0601: {} in {lib} type *FILE already exists.", name.name)
                    } else {
                        e
                    }
                })?;
            authority::set_owner(ctx.client, &lib, &name.name, "*FILE", ctx.user).await?;
            Ok(format!(
                "Index {} created in {lib} over {tlib}/{}.",
                name.name, table.name
            ))
        }
        Ddl::Drop {
            kind,
            name,
            if_exists,
        } => {
            let found = ctx.find(name).await?;
            let Some((lib, attr)) = found else {
                if *if_exists {
                    return Ok(format!("{} does not exist; nothing dropped.", name.name));
                }
                return Err(not_found(name, &kind.to_ascii_lowercase()));
            };
            let fits = match *kind {
                "TABLE" => attr == "PF",
                _ => attr == "LF",
            };
            if !fits {
                bail!(
                    "SQL0156: {} in {lib} is not a {}.",
                    name.name,
                    kind.to_ascii_lowercase()
                );
            }
            ctx.may_delete(&lib, &name.name).await?;
            catalog::delete_file(ctx.client, &lib, &name.name, true).await?;
            Ok(format!("{} {} in {lib} dropped.", title(kind), name.name))
        }
        Ddl::LabelObject { name, text } => {
            let Some((lib, _)) = ctx.find(name).await? else {
                return Err(not_found(name, "label"));
            };
            authority::check(ctx.client, ctx.user, &lib, &name.name, "*FILE", Aut::Change).await?;
            catalog::set_file_text(ctx.client, &lib, &name.name, text).await?;
            Ok(format!("Label on {} in {lib} changed.", name.name))
        }
        Ddl::LabelColumns { table, texts } => {
            let Some((lib, _)) = ctx.find(table).await? else {
                return Err(not_found(table, "label"));
            };
            authority::check(
                ctx.client,
                ctx.user,
                &lib,
                &table.name,
                "*FILE",
                Aut::Change,
            )
            .await?;
            for (c, t) in texts {
                catalog::set_field_text(ctx.client, &lib, &table.name, c, t).await?;
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
    let Some(ddl) = parse(stmt)? else {
        return Ok(None);
    };
    let mut client = crate::runtime::connect_db(database_url).await?;
    let mut ctx = Ctx {
        client: &mut client,
        user,
        libl,
        curlib,
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
        if c == ';' {
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
            "CREATE TABLE T (A FLOAT)",
            "CREATE TABLE T (A INT GENERATED ALWAYS AS IDENTITY)",
            "CREATE VIEW V AS SELECT 1 FROM T",
            "ALTER TABLE T ADD COLUMN B INT",
            "CREATE TABLE VERY_LONG_TABLE_NAME (A INT)",
        ] {
            let e = parse(sql).unwrap_err().to_string();
            assert!(e.starts_with("SQL0084"), "{sql}: {e}");
        }
        // Queries and changes are not data definition.
        assert_eq!(parse("SELECT * FROM T").unwrap(), None);
        assert_eq!(parse("SET PATH = X").unwrap(), None);
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
        assert!(parse("DROP VIEW VW_FOOD_PUBLIC").is_err());
    }
}
