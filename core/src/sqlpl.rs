//! SQL procedures and functions: CREATE PROCEDURE, CREATE FUNCTION and
//! CALL, with bodies in the SQL procedural language (SQL PL).
//!
//! Written from IBM's public Db2 for i SQL Reference (CREATE PROCEDURE
//! (SQL), CREATE FUNCTION (SQL scalar), CALL, compound statement, SET,
//! IF, CASE, WHILE, REPEAT, LOOP, FOR, LEAVE, ITERATE, RETURN, SIGNAL,
//! RESIGNAL, GET DIAGNOSTICS, DECLARE HANDLER, DECLARE CURSOR) and the SQL
//! programming topics on SQL routines.
//!
//! A procedure's body runs here, statement by statement: each SQL
//! statement in it goes to the SQL layer like any other statement, with
//! the routine's variables and parameters in it as values, on the session
//! of the statement that called it (so under its commitment control).
//! Handlers, conditions, cursors, loops and diagnostics are kept here. A
//! function is called by PostgreSQL inside queries, so it is defined
//! there: as4tainr writes a PostgreSQL function from the parsed body (a
//! RETURN expression, or a compound statement with DECLARE, SET, IF,
//! WHILE and RETURN), with every expression translated like any statement;
//! it is defined by the restricted SQL login, as views are.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_postgres::Client;

use crate::catalog::{self, ident};
use crate::sql::{tokenize, Exec, Session, Tok};

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.routines (
    library    text NOT NULL,
    name       text NOT NULL,
    kind       text NOT NULL,         -- PROCEDURE or FUNCTION
    definition text NOT NULL,         -- JSON (Routine)
    PRIMARY KEY (library, name, kind)
);
"#;

/// A routine parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parm {
    /// IN, OUT or INOUT.
    pub mode: String,
    pub name: String,
    /// The data type as written (`DECIMAL(9, 2)`).
    pub ty: String,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub library: String,
    pub name: String,
    /// PROCEDURE or FUNCTION.
    pub kind: String,
    pub parms: Vec<Parm>,
    /// A function's RETURNS type.
    pub returns: Option<String>,
    /// DYNAMIC RESULT SETS.
    pub result_sets: u32,
    /// The body as written (empty for an external procedure).
    pub body: String,
    /// An external procedure: the program it calls (`LIB/PGM`, or a
    /// name found through the library list), PARAMETER STYLE GENERAL.
    #[serde(default)]
    pub external: Option<String>,
}

// ------------------------------------------------------------ parsing

/// The statements of SQL PL.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Compound(Box<Compound>),
    /// SET target = expression, ...
    Set(Vec<(String, String)>),
    /// IF: (condition, statements) for IF and each ELSEIF; ELSE.
    If(Vec<(String, Vec<Stmt>)>, Option<Vec<Stmt>>),
    /// CASE: the expression of a simple CASE; (WHEN value or condition,
    /// statements); ELSE.
    Case(Option<String>, Vec<(String, Vec<Stmt>)>, Option<Vec<Stmt>>),
    While(Option<String>, String, Vec<Stmt>),
    Repeat(Option<String>, Vec<Stmt>, String),
    Loop(Option<String>, Vec<Stmt>),
    /// FOR v AS [c CURSOR FOR] query DO ... END FOR.
    For(Option<String>, String, String, Vec<Stmt>),
    Leave(String),
    Iterate(String),
    Return(Option<String>),
    /// SIGNAL / RESIGNAL: SQLSTATE (or condition name), MESSAGE_TEXT.
    Signal(Option<String>, Option<String>, bool),
    Open(String, Vec<String>),
    /// FETCH cursor INTO targets.
    Fetch(String, Vec<String>),
    Close(String),
    /// CALL name (arguments).
    Call(String, Vec<String>),
    /// GET DIAGNOSTICS (target, item).
    Diagnostics(Vec<(String, String)>),
    /// Any other SQL statement.
    Sql(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compound {
    pub label: Option<String>,
    pub atomic: bool,
    /// Variables: (name, type, DEFAULT).
    pub vars: Vec<(String, String, Option<String>)>,
    /// Conditions: name → SQLSTATE.
    pub conditions: Vec<(String, String)>,
    /// Cursors: (name, query, WITH RETURN).
    pub cursors: Vec<(String, String, bool)>,
    pub handlers: Vec<Handler>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    /// CONTINUE, EXIT or UNDO.
    pub kind: String,
    /// SQLEXCEPTION, SQLWARNING, NOT FOUND, an SQLSTATE or a condition.
    pub conditions: Vec<String>,
    pub body: Box<Stmt>,
}

struct Ps {
    t: Vec<Tok>,
    i: usize,
}

fn render(toks: &[Tok]) -> String {
    toks.iter()
        .map(|t| match t {
            Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
            Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
            Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

impl Ps {
    fn new(text: &str) -> Result<Ps> {
        Ok(Ps {
            t: tokenize(text)?,
            i: 0,
        })
    }

    fn skip(&mut self) {
        while matches!(self.t.get(self.i), Some(Tok::Space(_))) {
            self.i += 1;
        }
    }

    fn done(&mut self) -> bool {
        self.skip();
        self.i >= self.t.len()
    }

    fn word(&mut self) -> Option<String> {
        self.skip();
        match self.t.get(self.i) {
            Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    }

    /// The n-th word or punctuation ahead (0: the next), upper case.
    fn ahead(&self, n: usize) -> Option<String> {
        let mut k = self.i;
        let mut seen = 0;
        while k < self.t.len() {
            match &self.t[k] {
                Tok::Space(_) => {}
                t => {
                    if seen == n {
                        return Some(match t {
                            Tok::Word(w) | Tok::Punct(w) | Tok::Number(w) => w.to_ascii_uppercase(),
                            Tok::Str(s) => format!("'{s}'"),
                            Tok::Quoted(q) => q.clone(),
                            Tok::Space(_) => String::new(),
                        });
                    }
                    seen += 1;
                }
            }
            k += 1;
        }
        None
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

    fn at(&mut self) -> String {
        self.skip();
        match self.t.get(self.i) {
            Some(t) => format!(" at {}", render(std::slice::from_ref(t))),
            None => " at the end of the statement".into(),
        }
    }

    fn punct(&mut self, p: &str) -> bool {
        self.skip();
        if matches!(self.t.get(self.i), Some(Tok::Punct(x)) if x == p) {
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

    fn ident(&mut self) -> Result<String> {
        self.skip();
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

    /// `name`, `lib.name` or `lib/name`.
    fn qualified(&mut self) -> Result<(Option<String>, String)> {
        let first = self.ident()?;
        if self.punct(".") || self.punct("/") {
            Ok((Some(first), self.ident()?))
        } else {
            Ok((None, first))
        }
    }

    fn string(&mut self) -> Result<String> {
        self.skip();
        match self.t.get(self.i).cloned() {
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(s)
            }
            _ => bail!("SQL0104: a string constant is expected{}.", self.at()),
        }
    }

    /// Text up to (not including) one of the words, or a `;`, or a `,`
    /// when `comma`, at parenthesis depth 0. CASE ... END inside is kept
    /// together.
    fn until(&mut self, words: &[&str], comma: bool) -> String {
        self.skip();
        let start = self.i;
        let mut depth = 0i32;
        let mut cases = 0i32;
        while self.i < self.t.len() {
            match &self.t[self.i] {
                Tok::Punct(p) if p == "(" => depth += 1,
                Tok::Punct(p) if p == ")" => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1
                }
                Tok::Punct(p) if depth == 0 && (p == ";" || (comma && p == ",")) => break,
                Tok::Word(w) if depth == 0 => {
                    let u = w.to_ascii_uppercase();
                    if u == "CASE" {
                        cases += 1;
                    } else if u == "END" && cases > 0 {
                        cases -= 1;
                    } else if cases == 0 && words.contains(&u.as_str()) {
                        break;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        render(&self.t[start..self.i])
    }

    /// The rest of a statement, through its `;`.
    fn statement_text(&mut self) -> String {
        let s = self.until(&[], false);
        self.punct(";");
        s
    }

    fn end_semicolon(&mut self) {
        self.punct(";");
    }
}

/// A data type as written: `DECIMAL(9, 2)`, `VARCHAR(50)`, `CHAR(10) FOR
/// BIT DATA`, `TIMESTAMP`.
fn data_type_text(p: &mut Ps) -> Result<String> {
    let mut w = p.ident()?;
    // Two-word types.
    for (a, b) in [
        ("DOUBLE", "PRECISION"),
        ("CHARACTER", "VARYING"),
        ("CHAR", "VARYING"),
        ("LONG", "VARCHAR"),
    ] {
        if w == a && p.eat(b) {
            w = format!("{a} {b}");
        }
    }
    if p.punct("(") {
        let inner = p.until(&[], false);
        p.expect_punct(")")?;
        w = format!("{w}({inner})");
    }
    if p.eat("FOR") {
        let a = p.ident()?;
        let b = p.ident()?;
        w = format!("{w} FOR {a} {b}");
    }
    if p.eat("CCSID") {
        let _ = p.until(&["DEFAULT", "NOT", "NULL"], true);
    }
    Ok(w)
}

fn label_ahead(p: &Ps) -> bool {
    p.ahead(1).as_deref() == Some(":")
        && matches!(
            p.ahead(2).as_deref(),
            Some("BEGIN" | "LOOP" | "WHILE" | "REPEAT" | "FOR")
        )
}

/// Statements up to one of `ends` (not consumed).
fn statements(p: &mut Ps, ends: &[&str]) -> Result<Vec<Stmt>> {
    let mut out = Vec::new();
    loop {
        if p.done() {
            bail!(
                "SQL0104: {} expected at the end of the statement.",
                ends.join(" or ")
            );
        }
        if let Some(w) = p.word() {
            if ends.contains(&w.as_str()) {
                return Ok(out);
            }
        }
        out.push(statement(p)?);
    }
}

fn end_label(p: &mut Ps) {
    // An optional label after END xxx.
    if let Some(w) = p.word() {
        if !matches!(w.as_str(), "ELSE" | "ELSEIF" | "END" | "WHEN" | "UNTIL")
            && (p.ahead(1).as_deref() == Some(";") || p.ahead(1).is_none())
        {
            p.i += 1;
        }
    }
    p.end_semicolon();
}

/// One statement (with its `;`).
fn statement(p: &mut Ps) -> Result<Stmt> {
    let label = if label_ahead(p) {
        let l = p.ident()?;
        p.expect_punct(":")?;
        Some(l)
    } else {
        None
    };
    let w = p.word().unwrap_or_default();
    match w.as_str() {
        "BEGIN" => {
            p.i += 1;
            let atomic = if p.eat("NOT") {
                p.expect("ATOMIC")?;
                false
            } else {
                p.eat("ATOMIC")
            };
            let mut c = Compound {
                label,
                atomic,
                vars: vec![],
                conditions: vec![],
                cursors: vec![],
                handlers: vec![],
                body: vec![],
            };
            while p.word().as_deref() == Some("DECLARE") {
                p.i += 1;
                declaration(p, &mut c)?;
            }
            c.body = statements(p, &["END"])?;
            p.expect("END")?;
            end_label(p);
            Ok(Stmt::Compound(Box::new(c)))
        }
        "SET" if p.ahead(2).as_deref() == Some("=") || p.ahead(1).as_deref() == Some("(") => {
            p.i += 1;
            let mut out = Vec::new();
            if p.punct("(") {
                // SET (a, b) = (x, y)
                let mut targets = Vec::new();
                loop {
                    targets.push(p.ident()?);
                    if !p.punct(",") {
                        break;
                    }
                }
                p.expect_punct(")")?;
                p.expect_punct("=")?;
                p.expect_punct("(")?;
                for (k, t) in targets.iter().enumerate() {
                    let e = p.until(&[], true);
                    out.push((t.clone(), e));
                    if k + 1 < targets.len() {
                        p.expect_punct(",")?;
                    }
                }
                p.expect_punct(")")?;
            } else {
                loop {
                    let t = p.ident()?;
                    p.expect_punct("=")?;
                    let e = p.until(&[], true);
                    out.push((t, e));
                    if !p.punct(",") {
                        break;
                    }
                }
            }
            p.end_semicolon();
            Ok(Stmt::Set(out))
        }
        "IF" => {
            p.i += 1;
            let mut arms = Vec::new();
            let cond = p.until(&["THEN"], false);
            p.expect("THEN")?;
            arms.push((cond, statements(p, &["ELSEIF", "ELSE", "END"])?));
            let mut els = None;
            loop {
                if p.eat("ELSEIF") {
                    let c = p.until(&["THEN"], false);
                    p.expect("THEN")?;
                    arms.push((c, statements(p, &["ELSEIF", "ELSE", "END"])?));
                } else if p.eat("ELSE") {
                    els = Some(statements(p, &["END"])?);
                } else {
                    break;
                }
            }
            p.expect("END")?;
            p.expect("IF")?;
            p.end_semicolon();
            Ok(Stmt::If(arms, els))
        }
        "CASE" => {
            p.i += 1;
            let subject = if p.word().as_deref() == Some("WHEN") {
                None
            } else {
                Some(p.until(&["WHEN"], false))
            };
            let mut arms = Vec::new();
            let mut els = None;
            loop {
                if p.eat("WHEN") {
                    let v = p.until(&["THEN"], false);
                    p.expect("THEN")?;
                    arms.push((v, statements(p, &["WHEN", "ELSE", "END"])?));
                } else if p.eat("ELSE") {
                    els = Some(statements(p, &["END"])?);
                } else {
                    break;
                }
            }
            p.expect("END")?;
            p.expect("CASE")?;
            p.end_semicolon();
            Ok(Stmt::Case(subject, arms, els))
        }
        "WHILE" => {
            p.i += 1;
            let cond = p.until(&["DO"], false);
            p.expect("DO")?;
            let body = statements(p, &["END"])?;
            p.expect("END")?;
            p.expect("WHILE")?;
            end_label(p);
            Ok(Stmt::While(label, cond, body))
        }
        "REPEAT" => {
            p.i += 1;
            let body = statements(p, &["UNTIL"])?;
            p.expect("UNTIL")?;
            let cond = p.until(&["END"], false);
            p.expect("END")?;
            p.expect("REPEAT")?;
            end_label(p);
            Ok(Stmt::Repeat(label, body, cond))
        }
        "LOOP" => {
            p.i += 1;
            let body = statements(p, &["END"])?;
            p.expect("END")?;
            p.expect("LOOP")?;
            end_label(p);
            Ok(Stmt::Loop(label, body))
        }
        "FOR" => {
            p.i += 1;
            let var = p.ident()?;
            p.expect("AS")?;
            // An optional cursor name: c CURSOR [WITH HOLD] FOR.
            if p.ahead(1).as_deref() == Some("CURSOR") {
                p.i += 1;
                p.expect("CURSOR")?;
                if p.eat("WITH") {
                    p.expect("HOLD")?;
                }
                p.expect("FOR")?;
            }
            let query = p.until(&["DO"], false);
            p.expect("DO")?;
            let body = statements(p, &["END"])?;
            p.expect("END")?;
            p.expect("FOR")?;
            end_label(p);
            Ok(Stmt::For(label, var, query, body))
        }
        "LEAVE" | "ITERATE" => {
            p.i += 1;
            let l = p.ident()?;
            p.end_semicolon();
            Ok(if w == "LEAVE" {
                Stmt::Leave(l)
            } else {
                Stmt::Iterate(l)
            })
        }
        "RETURN" => {
            p.i += 1;
            let e = p.until(&[], false);
            p.end_semicolon();
            Ok(Stmt::Return((!e.is_empty()).then_some(e)))
        }
        "SIGNAL" | "RESIGNAL" => {
            p.i += 1;
            let state = if p.eat("SQLSTATE") {
                p.eat("VALUE");
                Some(p.string()?)
            } else if p.word().is_some() && p.word().as_deref() != Some("SET") {
                Some(p.ident()?)
            } else {
                None
            };
            let mut msg = None;
            if p.eat("SET") {
                loop {
                    let item = p.ident()?;
                    p.expect_punct("=")?;
                    let e = p.until(&[], true);
                    if item == "MESSAGE_TEXT" {
                        msg = Some(e);
                    }
                    if !p.punct(",") {
                        break;
                    }
                }
            } else if p.punct("(") {
                // SIGNAL SQLSTATE 'xxxxx' ('text'): the older form.
                msg = Some(p.until(&[], false));
                p.expect_punct(")")?;
            }
            p.end_semicolon();
            Ok(Stmt::Signal(state, msg, w == "RESIGNAL"))
        }
        "OPEN" => {
            p.i += 1;
            let c = p.ident()?;
            let mut using = Vec::new();
            if p.eat("USING") {
                loop {
                    using.push(p.until(&[], true));
                    if !p.punct(",") {
                        break;
                    }
                }
            }
            p.end_semicolon();
            Ok(Stmt::Open(c, using))
        }
        "FETCH" => {
            p.i += 1;
            p.eat("NEXT");
            p.eat("FROM");
            let c = p.ident()?;
            p.expect("INTO")?;
            let mut into = Vec::new();
            loop {
                into.push(p.ident()?);
                if !p.punct(",") {
                    break;
                }
            }
            p.end_semicolon();
            Ok(Stmt::Fetch(c, into))
        }
        "CLOSE" => {
            p.i += 1;
            let c = p.ident()?;
            p.end_semicolon();
            Ok(Stmt::Close(c))
        }
        "CALL" => {
            p.i += 1;
            let (lib, n) = p.qualified()?;
            let mut args = Vec::new();
            if p.punct("(") && !p.punct(")") {
                loop {
                    args.push(p.until(&[], true));
                    if p.punct(")") {
                        break;
                    }
                    p.expect_punct(",")?;
                }
            }
            p.end_semicolon();
            Ok(Stmt::Call(
                match lib {
                    Some(l) => format!("{l}/{n}"),
                    None => n,
                },
                args,
            ))
        }
        "GET" => {
            p.i += 1;
            let _ = p.eat("CURRENT") || p.eat("STACKED");
            p.expect("DIAGNOSTICS")?;
            if p.eat("EXCEPTION") {
                // The condition number (1): a constant or a variable.
                p.skip();
                if p.i < p.t.len() {
                    p.i += 1;
                }
            }
            let mut items = Vec::new();
            loop {
                let t = p.ident()?;
                p.expect_punct("=")?;
                let item = p.ident()?;
                items.push((t, item));
                if !p.punct(",") {
                    break;
                }
            }
            p.end_semicolon();
            Ok(Stmt::Diagnostics(items))
        }
        "" => bail!("SQL0104: a statement is expected{}.", p.at()),
        _ => Ok(Stmt::Sql(p.statement_text())),
    }
}

fn declaration(p: &mut Ps, c: &mut Compound) -> Result<()> {
    match p.ahead(1).as_deref() {
        // DECLARE CONTINUE|EXIT|UNDO HANDLER FOR ...
        Some("HANDLER") => {
            let kind = p.ident()?;
            p.expect("HANDLER")?;
            p.expect("FOR")?;
            let mut conditions = Vec::new();
            loop {
                let cond = if p.eat("SQLEXCEPTION") {
                    "SQLEXCEPTION".to_string()
                } else if p.eat("SQLWARNING") {
                    "SQLWARNING".to_string()
                } else if p.eat("NOT") {
                    p.expect("FOUND")?;
                    "NOT FOUND".to_string()
                } else if p.eat("SQLSTATE") {
                    p.eat("VALUE");
                    p.string()?
                } else {
                    p.ident()?
                };
                conditions.push(cond);
                if !p.punct(",") {
                    break;
                }
            }
            let body = statement(p)?;
            c.handlers.push(Handler {
                kind,
                conditions,
                body: Box::new(body),
            });
        }
        _ => {
            // DECLARE name CONDITION FOR SQLSTATE '...' | name ... CURSOR
            // | names type [DEFAULT ...].
            let mut names = vec![p.ident()?];
            if p.eat("CONDITION") {
                p.expect("FOR")?;
                p.eat("SQLSTATE");
                p.eat("VALUE");
                c.conditions.push((names[0].clone(), p.string()?));
                p.end_semicolon();
                return Ok(());
            }
            // DECLARE c [NO] SCROLL|INSENSITIVE|SENSITIVE... CURSOR
            let save = p.i;
            let mut is_cursor = false;
            for _ in 0..4 {
                if p.eat("CURSOR") {
                    is_cursor = true;
                    break;
                }
                if !(p.eat("NO")
                    || p.eat("SCROLL")
                    || p.eat("INSENSITIVE")
                    || p.eat("SENSITIVE")
                    || p.eat("ASENSITIVE")
                    || p.eat("DYNAMIC"))
                {
                    break;
                }
            }
            if is_cursor {
                let mut returns = false;
                while p.eat("WITH") || p.eat("WITHOUT") {
                    if p.eat("HOLD") {
                        continue;
                    }
                    if p.eat("RETURN") {
                        returns = true;
                        if p.eat("TO") {
                            let _ = p.eat("CALLER") || p.eat("CLIENT");
                        }
                    }
                }
                p.expect("FOR")?;
                let q = p.statement_text();
                c.cursors.push((names.remove(0), q, returns));
                return Ok(());
            }
            p.i = save;
            while p.punct(",") {
                names.push(p.ident()?);
            }
            let ty = data_type_text(p)?;
            let default = if p.eat("DEFAULT") {
                Some(p.until(&[], false))
            } else {
                None
            };
            p.end_semicolon();
            for n in names {
                c.vars.push((n, ty.clone(), default.clone()));
            }
        }
    }
    Ok(())
}

/// The parameters of CREATE PROCEDURE / FUNCTION: `([IN|OUT|INOUT] name
/// type [DEFAULT x], ...)`.
fn parameters(p: &mut Ps, function: bool) -> Result<Vec<Parm>> {
    let mut out = Vec::new();
    if !p.punct("(") {
        return Ok(out);
    }
    if p.punct(")") {
        return Ok(out);
    }
    loop {
        let mode = if !function && (p.eat("IN") || p.eat("OUT") || p.eat("INOUT")) {
            p.t[p.i - 1].clone()
        } else {
            Tok::Word("IN".into())
        };
        let mode = match mode {
            Tok::Word(w) => w.to_ascii_uppercase(),
            _ => "IN".into(),
        };
        let name = p.ident()?;
        let ty = data_type_text(p)?;
        let default = if p.eat("DEFAULT") {
            Some(p.until(&[], true))
        } else {
            None
        };
        out.push(Parm {
            mode,
            name,
            ty,
            default,
        });
        if p.punct(")") {
            break;
        }
        p.expect_punct(",")?;
    }
    Ok(out)
}

/// CREATE [OR REPLACE] PROCEDURE / FUNCTION, parsed. `Ok(None)`: not
/// one.
pub fn parse_create(sql: &str) -> Result<Option<(bool, Routine)>> {
    let mut p = Ps::new(sql)?;
    if !p.eat("CREATE") {
        return Ok(None);
    }
    let replace = if p.eat("OR") {
        p.expect("REPLACE")?;
        true
    } else {
        false
    };
    let kind = match p.word().as_deref() {
        Some("PROCEDURE") => "PROCEDURE",
        Some("FUNCTION") => "FUNCTION",
        _ => return Ok(None),
    };
    p.i += 1;
    let (lib, name) = p.qualified()?;
    let function = kind == "FUNCTION";
    let parms = parameters(&mut p, function)?;
    let mut returns = None;
    let mut result_sets = 0;
    let mut language: Option<String> = None;
    let mut external: Option<String> = None;
    let mut style_other: Option<String> = None;
    // Options, in any order, up to the body.
    while let Some(w) = p.word() {
        match w.as_str() {
            "RETURNS" if function => {
                p.i += 1;
                if p.eat("TABLE") {
                    // RETURNS TABLE (column type, ...): a table function,
                    // kept as TABLE(column type, ...).
                    p.expect_punct("(")?;
                    let mut cols = Vec::new();
                    loop {
                        let c = p.ident()?;
                        let t = data_type_text(&mut p)?;
                        cols.push(format!("{c} {t}"));
                        if !p.punct(",") {
                            break;
                        }
                    }
                    p.expect_punct(")")?;
                    returns = Some(format!("TABLE({})", cols.join(", ")));
                } else {
                    returns = Some(data_type_text(&mut p)?);
                }
            }
            "LANGUAGE" => {
                p.i += 1;
                let l = p.ident()?;
                if l != "SQL" {
                    language = Some(l);
                }
            }
            "EXTERNAL" if p.ahead(1).as_deref() == Some("NAME") => {
                p.i += 1;
                p.expect("NAME")?;
                p.skip();
                let n = match p.t.get(p.i).cloned() {
                    Some(Tok::Str(s)) => {
                        p.i += 1;
                        s.to_ascii_uppercase()
                    }
                    _ => {
                        let (l, n) = p.qualified()?;
                        match l {
                            Some(l) => format!("{l}/{n}"),
                            None => n,
                        }
                    }
                };
                external = Some(n.replace('.', "/"));
            }
            "EXTERNAL" if p.ahead(1).as_deref() != Some("ACTION") => {
                p.i += 1;
                external = Some(name.clone());
            }
            "SPECIFIC" => {
                p.i += 1;
                let _ = p.qualified()?;
            }
            "DYNAMIC" => {
                p.i += 1;
                p.expect("RESULT")?;
                p.expect("SETS")?;
                p.skip();
                match p.t.get(p.i).cloned() {
                    Some(Tok::Number(n)) => {
                        p.i += 1;
                        result_sets = n.parse().unwrap_or(0);
                    }
                    _ => bail!("SQL0104: a number is expected{}.", p.at()),
                }
            }
            "DETERMINISTIC" | "FENCED" | "AUTONOMOUS" | "SECURED" => p.i += 1,
            "NOT" => {
                p.i += 1;
                // DETERMINISTIC, FENCED, SECURED
                let _ = p.ident()?;
            }
            "MODIFIES" | "READS" => {
                p.i += 1;
                p.expect("SQL")?;
                p.expect("DATA")?;
            }
            "CONTAINS" => {
                p.i += 1;
                p.expect("SQL")?;
            }
            "NO" => {
                p.i += 1;
                let x = p.ident()?;
                if x == "EXTERNAL" {
                    p.expect("ACTION")?;
                }
            }
            "EXTERNAL" => {
                p.i += 1;
                p.expect("ACTION")?;
            }
            "CALLED" => {
                p.i += 1;
                p.expect("ON")?;
                p.expect("NULL")?;
                p.expect("INPUT")?;
            }
            "RETURNS" => bail!("SQL0104: RETURNS is for functions."),
            "ALLOW" | "DISALLOW" => {
                p.i += 1;
                p.expect("PARALLEL")?;
            }
            "INHERIT" => {
                p.i += 1;
                p.expect("SPECIAL")?;
                p.expect("REGISTERS")?;
            }
            "OLD" | "NEW" => {
                p.i += 1;
                p.expect("SAVEPOINT")?;
                p.expect("LEVEL")?;
            }
            "COMMIT" => {
                p.i += 1;
                p.expect("ON")?;
                p.expect("RETURN")?;
                let _ = p.ident()?;
            }
            "PROGRAM" => {
                p.i += 1;
                p.expect("TYPE")?;
                let _ = p.ident()?;
            }
            "SET" if p.ahead(1).as_deref() == Some("OPTION") => {
                p.i += 1;
                p.expect("OPTION")?;
                loop {
                    let _ = p.ident()?;
                    p.expect_punct("=")?;
                    // The value: *WORD, a word, a number, a string, or a
                    // list in parentheses (DECRESULT = (31, 31, 00)).
                    if p.punct("*") {
                        let _ = p.ident()?;
                    } else if p.punct("(") {
                        let _ = p.until(&[], false);
                        p.expect_punct(")")?;
                    } else {
                        p.skip();
                        p.i += 1;
                    }
                    if !p.punct(",") {
                        break;
                    }
                }
            }
            "PARAMETER" => {
                p.i += 1;
                p.expect("STYLE")?;
                let style = p.ident()?;
                if style == "GENERAL" {
                    if p.eat("WITH") {
                        p.expect("NULLS")?;
                    }
                } else if style != "JAVA" {
                    style_other = Some(style);
                }
            }
            "STATEMENT" => {
                p.i += 1;
                p.expect("DETERMINISTIC")?;
            }
            "CONCURRENT" => {
                p.i += 1;
                p.expect("ACCESS")?;
                p.expect("RESOLUTION")?;
                let _ = p.ident()?;
                let _ = p.eat("COMMITTED") || p.eat("DEFAULT") || p.eat("FOR");
                let _ = p.eat("COMPLETION");
            }
            _ => break,
        }
    }
    if let Some(pgm) = external.or_else(|| language.as_ref().map(|_| name.clone())) {
        if function {
            bail!("SQL0084: external functions are not supported yet.");
        }
        if let Some(st) = style_other {
            bail!(
                "SQL0084: PARAMETER STYLE {st} is not supported yet; use PARAMETER STYLE GENERAL."
            );
        }
        return Ok(Some((
            replace,
            Routine {
                library: lib.unwrap_or_default(),
                name,
                kind: kind.to_string(),
                parms,
                returns,
                result_sets,
                body: String::new(),
                external: Some(pgm),
            },
        )));
    }
    finish_create(
        &mut p,
        replace,
        kind,
        lib,
        name,
        parms,
        returns,
        result_sets,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_create(
    p: &mut Ps,
    replace: bool,
    kind: &str,
    lib: Option<String>,
    name: String,
    parms: Vec<Parm>,
    returns: Option<String>,
    result_sets: u32,
) -> Result<Option<(bool, Routine)>> {
    p.skip();
    let body = render(&p.t[p.i..]);
    let body = body.trim_end_matches(';').trim().to_string();
    if body.is_empty() {
        bail!("SQL0104: the routine has no body.");
    }
    if kind == "FUNCTION" && returns.is_none() {
        bail!("SQL0104: a function needs RETURNS.");
    }
    // The body must parse.
    parse_body(&body)?;
    Ok(Some((
        replace,
        Routine {
            library: lib.unwrap_or_default(),
            name,
            kind: kind.to_string(),
            parms,
            returns,
            result_sets,
            body,
            external: None,
        },
    )))
}

/// A routine body: one statement (usually a compound statement).
pub fn parse_body(body: &str) -> Result<Stmt> {
    let mut p = Ps::new(body)?;
    let s = statement(&mut p)?;
    if !p.done() {
        bail!("SQL0104: Token not valid after the routine body{}.", p.at());
    }
    Ok(s)
}

/// DROP PROCEDURE / FUNCTION [IF EXISTS] name.
/// DROP PROCEDURE/FUNCTION: kind, library, name, IF EXISTS.
pub type DropRoutine = (String, Option<String>, String, bool);

pub fn parse_drop(sql: &str) -> Result<Option<DropRoutine>> {
    let mut p = Ps::new(sql)?;
    if !p.eat("DROP") {
        return Ok(None);
    }
    let kind = match p.word().as_deref() {
        Some("PROCEDURE") => "PROCEDURE",
        Some("FUNCTION") => "FUNCTION",
        Some("SPECIFIC") => {
            p.i += 1;
            match p.word().as_deref() {
                Some("PROCEDURE") => "PROCEDURE",
                Some("FUNCTION") => "FUNCTION",
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    p.i += 1;
    let if_exists = if p.eat("IF") {
        p.expect("EXISTS")?;
        true
    } else {
        false
    };
    let (lib, name) = p.qualified()?;
    if p.punct("(") {
        let _ = p.until(&[], false);
        p.expect_punct(")")?;
    }
    Ok(Some((kind.to_string(), lib, name, if_exists)))
}

/// CALL name(args): the name (LIB/NAME or NAME) and the argument texts.
pub fn parse_call(sql: &str) -> Result<Option<(String, Vec<String>)>> {
    let mut p = Ps::new(sql)?;
    if p.word().as_deref() != Some("CALL") {
        return Ok(None);
    }
    match statement(&mut p)? {
        Stmt::Call(n, a) => {
            if !p.done() {
                bail!("SQL0104: Token not valid after CALL{}.", p.at());
            }
            Ok(Some((n, a)))
        }
        _ => Ok(None),
    }
}

// ------------------------------------------------------------ running

/// SIGNAL: an error with the SQLSTATE the routine gave it (SQLCODE
/// -438).
#[derive(Debug)]
pub struct Signaled {
    pub state: String,
    pub text: String,
}

impl std::fmt::Display for Signaled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SQL0438: {}", self.text)
    }
}

impl std::error::Error for Signaled {}

/// No row (SQLSTATE 02000, SQLCODE +100): SELECT INTO found none, FETCH
/// is past the last row.
#[derive(Debug)]
pub struct NotFound;

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SQL0100: No row was found.")
    }
}

impl std::error::Error for NotFound {}

/// SQLSTATE for an SQLCODE, as Db2 for i reports them.
pub fn sqlstate_for(code: i64) -> &'static str {
    match code {
        0 => "00000",
        100 => "02000",
        -84 => "42612",
        -104 => "42601",
        -204 => "42704",
        -206 => "42703",
        -302 => "22003",
        -305 => "22002",
        -407 => "23502",
        -438 => "75000",
        -440 => "42884",
        -501 => "24501",
        -502 => "24502",
        -504 => "34000",
        -518 => "07003",
        -545 => "23513",
        -551 => "42501",
        -601 => "42710",
        -802 => "22012",
        -803 => "23505",
        -811 => "21000",
        _ => "58004",
    }
}

/// An error's SQLCODE, SQLSTATE and message text.
pub fn condition_of(e: &anyhow::Error) -> (i64, String, String) {
    if let Some(s) = e.downcast_ref::<Signaled>() {
        return (-438, s.state.clone(), s.text.clone());
    }
    if e.downcast_ref::<NotFound>().is_some() {
        return (100, "02000".into(), String::new());
    }
    let m = format!("{e:#}");
    let digits: String = m
        .strip_prefix("SQL")
        .map(|r| r.chars().take(4).collect())
        .unwrap_or_default();
    match digits.parse::<i64>() {
        Ok(n) if digits.len() == 4 => (-n, sqlstate_for(-n).to_string(), m),
        _ => (-901, "58004".into(), m),
    }
}

#[derive(Debug, Clone)]
struct Var {
    /// The declared type (none: a FOR loop's column).
    ty: Option<String>,
    val: Option<String>,
}

fn is_numeric_type(ty: &str) -> bool {
    let t = ty.to_ascii_uppercase();
    [
        "DEC", "NUMERIC", "INT", "SMALLINT", "BIGINT", "REAL", "FLOAT", "DOUBLE",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// A Db2 timestamp as text (2026-10-19-14.05.30.000000) in the form
/// statements take.
fn statement_form(v: &str) -> String {
    let b = v.as_bytes();
    if v.len() >= 19
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'-'
        && b[13] == b'.'
        && b[16] == b'.'
        && v[..4].chars().all(|c| c.is_ascii_digit())
    {
        format!("{} {}:{}:{}", &v[..10], &v[11..13], &v[14..16], &v[17..])
    } else {
        v.to_string()
    }
}

impl Var {
    fn literal(&self) -> String {
        match (&self.val, &self.ty) {
            (None, Some(t)) => format!("CAST(NULL AS {t})"),
            (None, None) => "NULL".into(),
            (Some(v), Some(t)) if is_numeric_type(t) => v.trim().to_string(),
            // Typed, so that CHAR values compare as CHAR (blank padded)
            // and dates as dates.
            (Some(v), Some(t)) => {
                format!("CAST('{}' AS {t})", statement_form(v).replace('\'', "''"))
            }
            (Some(v), None) => format!("'{}'", statement_form(v).replace('\'', "''")),
        }
    }
}

struct Cursor {
    query: String,
    returns: bool,
    rows: Option<ResultSet>,
    pos: usize,
}

struct Frame {
    label: Option<String>,
    vars: HashMap<String, Var>,
    conditions: HashMap<String, String>,
    cursors: HashMap<String, Cursor>,
    handlers: Vec<Handler>,
}

impl Frame {
    fn new(label: Option<String>) -> Frame {
        Frame {
            label,
            vars: HashMap::new(),
            conditions: HashMap::new(),
            cursors: HashMap::new(),
            handlers: vec![],
        }
    }
}

enum Flow {
    Next,
    Leave(String),
    Iterate(String),
    Return(Option<String>),
    /// An EXIT handler of the compound at this frame ran: that compound
    /// ends.
    Exit(usize),
}

/// What a routine's statements run with.
pub struct Env<'a> {
    pub session: &'a Session,
    /// The main connection: the catalog (routines, file descriptions).
    pub catalog: &'a Client,
    pub database_url: &'a str,
    pub libl: &'a [String],
    pub user: &'a str,
    pub curlib: Option<String>,
}

/// A result set a procedure returns (a cursor declared WITH RETURN and
/// left open).
pub type ResultSet = (Vec<String>, Vec<Vec<Option<String>>>);

/// What a CALL gives back.
#[derive(Debug, Default)]
pub struct CallResult {
    /// The parameters' values when the procedure ended (OUT and INOUT
    /// parameters; IN ones as passed).
    pub values: Vec<Option<String>>,
    pub result_sets: Vec<ResultSet>,
    /// RETURN's value (DB2_RETURN_STATUS).
    pub status: i64,
}

type BoxFut<'s, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 's>>;

struct Run<'a, 'e> {
    env: &'e Env<'a>,
    routine: String,
    frames: Vec<Frame>,
    /// The last condition (GET DIAGNOSTICS, RESIGNAL).
    diag: (i64, String, String),
    row_count: i64,
    /// Handlers of frames at or above this index are not looked at (a
    /// condition in a handler goes to the compounds around it).
    handler_floor: usize,
    result_sets: Vec<ResultSet>,
    depth: usize,
}

const MAX_DEPTH: usize = 64;

impl<'a, 'e> Run<'a, 'e> {
    fn lookup(&self, name: &str) -> Option<&Var> {
        self.frames.iter().rev().find_map(|f| f.vars.get(name))
    }

    fn lookup_mut(&mut self, name: &str) -> Option<&mut Var> {
        self.frames
            .iter_mut()
            .rev()
            .find_map(|f| f.vars.get_mut(name))
    }

    /// `label.name`: a variable of the compound (or FOR loop, or the
    /// routine's parameters) with that label.
    fn lookup_qualified(&self, label: &str, name: &str) -> Option<&Var> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.label.as_deref() == Some(label))
            .and_then(|f| f.vars.get(name))
    }

    /// The statement with the routine's variables put in as values.
    fn subst(&self, text: &str) -> Result<String> {
        let toks = tokenize(text)?;
        let mut out = String::new();
        let sig = |k: usize, dir: isize| -> Option<&Tok> {
            let mut j = k as isize + dir;
            while j >= 0 && (j as usize) < toks.len() {
                match &toks[j as usize] {
                    Tok::Space(_) => j += dir,
                    t => return Some(t),
                }
            }
            None
        };
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            if let Tok::Word(w) = t {
                let up = w.to_ascii_uppercase();
                let prev = sig(i, -1);
                let next = sig(i, 1);
                let after_dot = matches!(prev, Some(Tok::Punct(p)) if p == ".");
                // label.var
                if matches!(next, Some(Tok::Punct(p)) if p == ".") && !after_dot {
                    // find the word after the dot
                    let mut j = i + 1;
                    while matches!(toks.get(j), Some(Tok::Space(_))) {
                        j += 1;
                    }
                    let mut k = j + 1;
                    while matches!(toks.get(k), Some(Tok::Space(_))) {
                        k += 1;
                    }
                    if let Some(Tok::Word(n)) = toks.get(k) {
                        if let Some(v) = self.lookup_qualified(&up, &n.to_ascii_uppercase()) {
                            out.push_str(&v.literal());
                            i = k + 1;
                            continue;
                        }
                    }
                }
                let host = matches!(prev, Some(Tok::Punct(p)) if p == ":");
                let call = matches!(next, Some(Tok::Punct(p)) if p == "(" || p == ".");
                if !after_dot && !call {
                    if let Some(v) = self.lookup(&up) {
                        if host {
                            out.pop();
                        }
                        out.push_str(&v.literal());
                        i += 1;
                        continue;
                    }
                }
            }
            out.push_str(&match t {
                Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
                Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
                Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
            });
            i += 1;
        }
        Ok(out)
    }

    async fn query(&self, sql: &str) -> Result<(Vec<String>, Vec<Vec<Option<String>>>)> {
        match self.env.session.exec(self.env.catalog, sql).await? {
            Exec::Rows { columns, rows } => Ok((columns, rows)),
            Exec::Count(..) => bail!("SQL0104: a query was expected: {sql}"),
        }
    }

    /// An expression's value (cast to `ty` when given).
    async fn value(&self, expr: &str, ty: Option<&str>) -> Result<Option<String>> {
        let e = expr.trim();
        // A variable by itself, or NULL: no statement needed.
        if e.eq_ignore_ascii_case("NULL") {
            return Ok(None);
        }
        if let Some(v) = self.lookup(&e.to_ascii_uppercase()) {
            if ty.is_none() || v.ty.as_deref() == ty {
                return Ok(v.val.clone());
            }
        }
        let e = self.subst(e)?;
        let q = match ty {
            Some(t) => format!("SELECT CAST(({e}) AS {t}) FROM SYSIBM.SYSDUMMY1"),
            None => format!("SELECT {e} FROM SYSIBM.SYSDUMMY1"),
        };
        let (_, rows) = self.query(&q).await?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next())
            .flatten())
    }

    /// A condition: true, or false/unknown.
    async fn truth(&self, cond: &str) -> Result<bool> {
        let c = self.subst(cond)?;
        let (_, rows) = self
            .query(&format!(
                "SELECT CASE WHEN ({c}) THEN 1 ELSE 0 END FROM SYSIBM.SYSDUMMY1"
            ))
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_deref())
            == Some("1"))
    }

    fn assign(&mut self, target: &str, v: Option<String>) -> Result<()> {
        let t = target.trim().trim_start_matches(':').to_ascii_uppercase();
        // label.var
        if let Some((l, n)) = t.split_once('.') {
            let (l, n) = (l.trim().to_string(), n.trim().to_string());
            if let Some(f) = self
                .frames
                .iter_mut()
                .rev()
                .find(|f| f.label.as_deref() == Some(l.as_str()))
            {
                if let Some(var) = f.vars.get_mut(&n) {
                    var.val = v;
                    return Ok(());
                }
            }
        }
        match self.lookup_mut(&t) {
            Some(var) => {
                var.val = v;
                Ok(())
            }
            None => bail!("SQL0206: Variable {t} is not declared."),
        }
    }

    async fn assign_typed(&mut self, target: &str, v: Option<String>) -> Result<()> {
        let t = target.trim().trim_start_matches(':').to_ascii_uppercase();
        let ty = self.lookup(&t).and_then(|x| x.ty.clone());
        let v = match (v, ty) {
            (Some(val), Some(ty)) => {
                let lit = Var {
                    ty: None,
                    val: Some(val.clone()),
                }
                .literal();
                let lit = if is_numeric_type(&ty) {
                    val.trim().to_string()
                } else {
                    lit
                };
                self.value(&lit, Some(&ty)).await?
            }
            (v, _) => v,
        };
        self.assign(&t, v)
    }

    /// The SQLSTATE and SQLCODE variables, when the routine declared them.
    fn set_status(&mut self, code: i64, state: &str) {
        if let Some(v) = self.lookup_mut("SQLSTATE") {
            v.val = Some(state.to_string());
        }
        if let Some(v) = self.lookup_mut("SQLCODE") {
            v.val = Some(code.to_string());
        }
    }

    fn condition_state(&self, name: &str) -> Option<String> {
        self.frames
            .iter()
            .rev()
            .find_map(|f| f.conditions.get(name).cloned())
    }

    /// A condition was raised: the handler for it runs (CONTINUE: the
    /// statement after the one that raised it is next; EXIT: the
    /// compound with the handler ends). Without a handler, a warning or
    /// NOT FOUND goes on; an exception ends the routine.
    fn raise(&mut self, e: anyhow::Error) -> BoxFut<'_, Result<Flow>> {
        Box::pin(async move {
            let (code, state, text) = condition_of(&e);
            self.diag = (code, state.clone(), text);
            self.set_status(code, &state);
            let class = &state[..2];
            let matches = |c: &str, me: &Run| -> u8 {
                match c {
                    "SQLEXCEPTION" if !matches!(class, "00" | "01" | "02") => 1,
                    "SQLWARNING" if class == "01" => 1,
                    "NOT FOUND" if class == "02" => 1,
                    s if s.len() == 5 && s == state => 2,
                    name => match me.condition_state(name) {
                        Some(s) if s == state => 2,
                        _ => 0,
                    },
                }
            };
            let mut found: Option<(usize, Handler)> = None;
            for k in (0..self.handler_floor.min(self.frames.len())).rev() {
                let best = self.frames[k]
                    .handlers
                    .iter()
                    .filter_map(|h| {
                        let m = h
                            .conditions
                            .iter()
                            .map(|c| matches(c, self))
                            .max()
                            .unwrap_or(0);
                        (m > 0).then_some((m, h))
                    })
                    .max_by_key(|(m, _)| *m)
                    .map(|(_, h)| h.clone());
                if let Some(h) = best {
                    found = Some((k, h));
                    break;
                }
            }
            match found {
                Some((k, h)) => {
                    let floor = self.handler_floor;
                    self.handler_floor = k;
                    let r = self.exec(&h.body).await;
                    self.handler_floor = floor;
                    match r? {
                        Flow::Next | Flow::Exit(_) => {}
                        other => return Ok(other),
                    }
                    if h.kind == "CONTINUE" {
                        Ok(Flow::Next)
                    } else {
                        Ok(Flow::Exit(k))
                    }
                }
                None if matches!(class, "00" | "01" | "02") => Ok(Flow::Next),
                None => Err(e),
            }
        })
    }

    /// Run a simple statement's work; a condition it raises goes to
    /// `raise`.
    async fn guarded(&mut self, r: Result<()>) -> Result<Flow> {
        match r {
            Ok(()) => {
                self.set_status(0, "00000");
                Ok(Flow::Next)
            }
            Err(e) => self.raise(e).await,
        }
    }

    fn exec<'s>(&'s mut self, s: &'s Stmt) -> BoxFut<'s, Result<Flow>> {
        Box::pin(async move {
            match s {
                Stmt::Compound(c) => self.compound(c).await,
                Stmt::Set(list) => {
                    let mut r = Ok(());
                    for (t, e) in list {
                        let tn = t.to_ascii_uppercase();
                        let ty = self.lookup(&tn).and_then(|v| v.ty.clone());
                        let v = match self.value(e, ty.as_deref()).await {
                            Ok(v) => v,
                            Err(e) => {
                                r = Err(e);
                                break;
                            }
                        };
                        if let Err(e) = self.assign(&tn, v) {
                            r = Err(e);
                            break;
                        }
                    }
                    self.guarded(r).await
                }
                Stmt::If(arms, els) => {
                    for (cond, body) in arms {
                        match self.truth(cond).await {
                            Ok(true) => return self.block(body).await,
                            Ok(false) => {}
                            Err(e) => return self.raise(e).await,
                        }
                    }
                    match els {
                        Some(b) => self.block(b).await,
                        None => Ok(Flow::Next),
                    }
                }
                Stmt::Case(subject, arms, els) => {
                    for (w, body) in arms {
                        let cond = match subject {
                            Some(x) => format!("({x}) = ({w})"),
                            None => w.clone(),
                        };
                        match self.truth(&cond).await {
                            Ok(true) => return self.block(body).await,
                            Ok(false) => {}
                            Err(e) => return self.raise(e).await,
                        }
                    }
                    match els {
                        Some(b) => self.block(b).await,
                        // No WHEN and no ELSE: SQLSTATE 20000.
                        None => {
                            self.raise(anyhow::Error::new(Signaled {
                                state: "20000".into(),
                                text: "No WHEN of the CASE statement was true.".into(),
                            }))
                            .await
                        }
                    }
                }
                Stmt::While(label, cond, body) => loop {
                    match self.truth(cond).await {
                        Ok(true) => {}
                        Ok(false) => return Ok(Flow::Next),
                        Err(e) => match self.raise(e).await? {
                            Flow::Next => return Ok(Flow::Next),
                            other => return Ok(other),
                        },
                    }
                    match self.looped(label, body).await? {
                        Some(f) => return Ok(f),
                        None => continue,
                    }
                },
                Stmt::Repeat(label, body, cond) => loop {
                    if let Some(f) = self.looped(label, body).await? {
                        return Ok(f);
                    }
                    match self.truth(cond).await {
                        Ok(true) => return Ok(Flow::Next),
                        Ok(false) => {}
                        Err(e) => return self.raise(e).await,
                    }
                },
                Stmt::Loop(label, body) => loop {
                    if let Some(f) = self.looped(label, body).await? {
                        return Ok(f);
                    }
                },
                Stmt::For(label, var, query, body) => {
                    let q = match self.subst(query) {
                        Ok(q) => q,
                        Err(e) => return self.raise(e).await,
                    };
                    let (cols, rows) = match self.query(&q).await {
                        Ok(r) => r,
                        Err(e) => return self.raise(e).await,
                    };
                    for row in rows {
                        let mut f = Frame::new(Some(var.clone()));
                        for (c, v) in cols.iter().zip(row) {
                            f.vars
                                .insert(c.to_ascii_uppercase(), Var { ty: None, val: v });
                        }
                        self.frames.push(f);
                        let r = self.looped(label, body).await;
                        self.frames.pop();
                        if let Some(f) = r? {
                            return Ok(f);
                        }
                    }
                    Ok(Flow::Next)
                }
                Stmt::Leave(l) => Ok(Flow::Leave(l.clone())),
                Stmt::Iterate(l) => Ok(Flow::Iterate(l.clone())),
                Stmt::Return(e) => {
                    let v = match e {
                        Some(e) => match self.value(e, None).await {
                            Ok(v) => v,
                            Err(err) => return self.raise(err).await,
                        },
                        None => None,
                    };
                    Ok(Flow::Return(v))
                }
                Stmt::Signal(state, msg, resignal) => {
                    let state = match state {
                        Some(s) if s.len() == 5 && s.chars().all(|c| c.is_ascii_alphanumeric()) => {
                            s.clone()
                        }
                        Some(name) => self
                            .condition_state(name)
                            .ok_or_else(|| anyhow!("SQL0104: condition {name} is not declared."))?,
                        None if *resignal => self.diag.1.clone(),
                        None => bail!("SQL0104: SIGNAL needs an SQLSTATE."),
                    };
                    let text = match msg {
                        Some(m) => self.value(m, None).await?.unwrap_or_default(),
                        None if *resignal => self.diag.2.clone(),
                        None => format!("SQLSTATE {state} was signaled."),
                    };
                    if *resignal && self.handler_floor < self.frames.len() {
                        // RESIGNAL in a handler: to the compounds around.
                    }
                    self.raise(anyhow::Error::new(Signaled { state, text }))
                        .await
                }
                Stmt::Open(c, _) => {
                    let q = self
                        .frames
                        .iter()
                        .rev()
                        .find_map(|f| f.cursors.get(c).map(|x| x.query.clone()));
                    let Some(q) = q else {
                        return self
                            .raise(anyhow!("SQL0504: Cursor {c} is not declared."))
                            .await;
                    };
                    let r = async {
                        let q = self.subst(&q)?;
                        self.query(&q).await
                    }
                    .await;
                    match r {
                        Ok(rows) => {
                            if let Some(cur) = self
                                .frames
                                .iter_mut()
                                .rev()
                                .find_map(|f| f.cursors.get_mut(c))
                            {
                                if cur.rows.is_some() {
                                    return self
                                        .raise(anyhow!("SQL0502: Cursor {c} is already open."))
                                        .await;
                                }
                                cur.rows = Some(rows);
                                cur.pos = 0;
                            }
                            self.guarded(Ok(())).await
                        }
                        Err(e) => self.raise(e).await,
                    }
                }
                Stmt::Fetch(c, into) => {
                    let row = match self
                        .frames
                        .iter_mut()
                        .rev()
                        .find_map(|f| f.cursors.get_mut(c))
                    {
                        None => Err(anyhow!("SQL0504: Cursor {c} is not declared.")),
                        Some(cur) => match &cur.rows {
                            None => Err(anyhow!("SQL0501: Cursor {c} is not open.")),
                            Some((_, rows)) => {
                                if cur.pos < rows.len() {
                                    cur.pos += 1;
                                    Ok(rows[cur.pos - 1].clone())
                                } else {
                                    Err(anyhow::Error::new(NotFound))
                                }
                            }
                        },
                    };
                    let r = match row {
                        Ok(row) => {
                            let mut r = Ok(());
                            for (t, v) in into.iter().zip(row) {
                                if let Err(e) = self.assign_typed(t, v).await {
                                    r = Err(e);
                                    break;
                                }
                            }
                            r
                        }
                        Err(e) => Err(e),
                    };
                    self.guarded(r).await
                }
                Stmt::Close(c) => {
                    let r = match self
                        .frames
                        .iter_mut()
                        .rev()
                        .find_map(|f| f.cursors.get_mut(c))
                    {
                        Some(cur) if cur.rows.is_some() => {
                            cur.rows = None;
                            Ok(())
                        }
                        Some(_) => Err(anyhow!("SQL0501: Cursor {c} is not open.")),
                        None => Err(anyhow!("SQL0504: Cursor {c} is not declared.")),
                    };
                    self.guarded(r).await
                }
                Stmt::Call(name, args) => {
                    let r = self.call_nested(name, args).await;
                    self.guarded(r).await
                }
                Stmt::Diagnostics(items) => {
                    for (t, item) in items {
                        let v = match item.as_str() {
                            "ROW_COUNT" => Some(self.row_count.to_string()),
                            "MESSAGE_TEXT" => Some(self.diag.2.clone()),
                            "RETURNED_SQLSTATE" => Some(self.diag.1.clone()),
                            "DB2_RETURNED_SQLCODE" => Some(self.diag.0.to_string()),
                            "MORE" => Some("N".into()),
                            "NUMBER" => Some("1".into()),
                            other => {
                                bail!("SQL0104: GET DIAGNOSTICS {other} is not supported yet.")
                            }
                        };
                        self.assign_typed(t, v).await?;
                    }
                    Ok(Flow::Next)
                }
                Stmt::Sql(text) => {
                    let r = self.sql(text).await;
                    self.guarded(r).await
                }
            }
        })
    }

    /// Statements in sequence (a THEN or ELSE part).
    async fn block(&mut self, body: &[Stmt]) -> Result<Flow> {
        for s in body {
            match self.exec(s).await? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    /// One pass of a loop's body: None to go on looping, Some(flow) to
    /// stop (LEAVE of this loop is Next).
    async fn looped(&mut self, label: &Option<String>, body: &[Stmt]) -> Result<Option<Flow>> {
        match self.block(body).await? {
            Flow::Next => Ok(None),
            Flow::Iterate(l) if label.as_deref() == Some(l.as_str()) => Ok(None),
            Flow::Leave(l) if label.as_deref() == Some(l.as_str()) => Ok(Some(Flow::Next)),
            other => Ok(Some(other)),
        }
    }

    fn compound<'s>(&'s mut self, c: &'s Compound) -> BoxFut<'s, Result<Flow>> {
        Box::pin(async move {
            let mut f = Frame::new(c.label.clone());
            for (n, s) in &c.conditions {
                f.conditions.insert(n.clone(), s.clone());
            }
            for (n, q, r) in &c.cursors {
                f.cursors.insert(
                    n.clone(),
                    Cursor {
                        query: q.clone(),
                        returns: *r,
                        rows: None,
                        pos: 0,
                    },
                );
            }
            f.handlers = c.handlers.clone();
            self.frames.push(f);
            let me = self.frames.len() - 1;
            // Variables, with their defaults (SQLSTATE and SQLCODE start
            // as '00000' and 0).
            for (n, ty, d) in &c.vars {
                let v = match d {
                    Some(e) => match self.value(e, Some(ty)).await {
                        Ok(v) => v,
                        Err(e) => {
                            self.frames.pop();
                            return Err(e);
                        }
                    },
                    None if n == "SQLSTATE" => Some("00000".into()),
                    None if n == "SQLCODE" => Some("0".into()),
                    None => None,
                };
                self.frames[me].vars.insert(
                    n.clone(),
                    Var {
                        ty: Some(ty.clone()),
                        val: v,
                    },
                );
            }
            let floor = self.handler_floor;
            self.handler_floor = self.frames.len();
            let mut result = Ok(Flow::Next);
            for s in &c.body {
                match self.exec(s).await {
                    Ok(Flow::Next) => {}
                    Ok(Flow::Exit(k)) if k == me => break,
                    Ok(Flow::Leave(l)) if c.label.as_deref() == Some(l.as_str()) => break,
                    other => {
                        result = other;
                        break;
                    }
                }
            }
            self.handler_floor = floor;
            // Cursors declared WITH RETURN and left open: the
            // procedure's result sets.
            let f = self.frames.pop().expect("pushed above");
            let mut sets: Vec<(String, Cursor)> = f
                .cursors
                .into_iter()
                .filter(|(_, c)| c.returns && c.rows.is_some())
                .collect();
            sets.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, cur) in sets {
                if let Some((cols, rows)) = cur.rows {
                    let rest = rows.into_iter().skip(cur.pos).collect();
                    self.result_sets.push((cols, rest));
                }
            }
            result
        })
    }

    /// SELECT ... INTO, VALUES ... INTO, COMMIT, ROLLBACK, and the other
    /// statements.
    async fn sql(&mut self, text: &str) -> Result<()> {
        let up: Vec<String> = text
            .split_whitespace()
            .take(2)
            .map(|w| w.to_ascii_uppercase())
            .collect();
        let first = up.first().map(String::as_str).unwrap_or("");
        match first {
            "COMMIT" | "ROLLBACK" => {
                if let Some(d) = self.env.session.commitment() {
                    if first == "COMMIT" {
                        d.commit().await?;
                    } else {
                        d.rollback().await?;
                    }
                }
                return Ok(());
            }
            "SELECT" | "VALUES" | "WITH" => {
                if let Some((query, targets)) = split_into(text)? {
                    let q = self.subst(&query)?;
                    let (_, rows) = self.query(&q).await?;
                    if rows.len() > 1 {
                        bail!("SQL0811: The result of SELECT INTO has more than one row.");
                    }
                    let Some(row) = rows.into_iter().next() else {
                        return Err(anyhow::Error::new(NotFound));
                    };
                    for (t, v) in targets.iter().zip(row) {
                        self.assign_typed(t, v).await?;
                    }
                    self.row_count = 1;
                    return Ok(());
                }
            }
            "CREATE" | "DROP" | "ALTER" | "LABEL" | "COMMENT" => {
                let mut schema = None;
                let stmt = self.subst(text)?;
                if crate::ddl::try_run(
                    self.env.database_url,
                    self.env.user,
                    self.env.libl,
                    self.env.curlib.clone(),
                    &mut schema,
                    &stmt,
                )
                .await?
                .is_some()
                {
                    return Ok(());
                }
            }
            _ => {}
        }
        let stmt = self.subst(text)?;
        match self.env.session.exec(self.env.catalog, &stmt).await? {
            Exec::Count(_, n) => {
                self.row_count = n as i64;
                if n == 0 && matches!(first, "UPDATE" | "DELETE") {
                    // No rows: NOT FOUND (a warning).
                    return Err(anyhow::Error::new(NotFound));
                }
            }
            Exec::Rows { rows, .. } => self.row_count = rows.len() as i64,
        }
        Ok(())
    }

    async fn call_nested(&mut self, name: &str, args: &[String]) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            bail!("SQL0901: Procedures call each other more than {MAX_DEPTH} levels deep.");
        }
        let r = find(self.env.catalog, self.env.libl, name, "PROCEDURE")
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "SQL0440: Procedure {name} with {} parameters not found.",
                    args.len()
                )
            })?;
        let mut values = Vec::new();
        for (p, a) in r.parms.iter().zip(args) {
            values.push(if p.mode == "OUT" {
                None
            } else {
                self.value(a, None).await?
            });
        }
        let out = Box::pin(run_procedure(self.env, &r, values, self.depth + 1)).await?;
        for ((p, a), v) in r.parms.iter().zip(args).zip(out.values) {
            if p.mode != "IN" {
                self.assign_typed(a, v).await?;
            }
        }
        self.result_sets.extend(out.result_sets);
        Ok(())
    }
}

/// `SELECT a, b INTO x, y FROM ...` → (`SELECT a, b FROM ...`, [x, y]);
/// `VALUES expr INTO x` likewise. None: no INTO.
fn split_into(text: &str) -> Result<Option<(String, Vec<String>)>> {
    let toks = tokenize(text)?;
    let mut depth = 0;
    let mut into_at = None;
    let mut end = toks.len();
    for (k, t) in toks.iter().enumerate() {
        match t {
            Tok::Punct(p) if p == "(" => depth += 1,
            Tok::Punct(p) if p == ")" => depth -= 1,
            Tok::Word(w) if depth == 0 => {
                let u = w.to_ascii_uppercase();
                if u == "INTO" && into_at.is_none() {
                    into_at = Some(k);
                } else if into_at.is_some()
                    && matches!(
                        u.as_str(),
                        "FROM"
                            | "WHERE"
                            | "GROUP"
                            | "ORDER"
                            | "FETCH"
                            | "WITH"
                            | "HAVING"
                            | "UNION"
                    )
                {
                    end = k;
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(at) = into_at else { return Ok(None) };
    let targets: Vec<String> = render(&toks[at + 1..end])
        .split(',')
        .map(|t| t.trim().trim_start_matches(':').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let query = format!("{} {}", render(&toks[..at]), render(&toks[end..]));
    Ok(Some((query.trim().to_string(), targets)))
}

/// Run a procedure with its parameter values (OUT ones None).
pub async fn run_procedure(
    env: &Env<'_>,
    r: &Routine,
    values: Vec<Option<String>>,
    depth: usize,
) -> Result<CallResult> {
    if values.len() > r.parms.len() {
        bail!(
            "SQL0440: Procedure {} has {} parameters; {} were given.",
            r.name,
            r.parms.len(),
            values.len()
        );
    }
    if let Some(pgm) = &r.external {
        return call_external(env, r, pgm, values).await;
    }
    let body = parse_body(&r.body)?;
    let mut run = Run {
        env,
        routine: r.name.clone(),
        frames: vec![],
        diag: (0, "00000".into(), String::new()),
        row_count: 0,
        handler_floor: 0,
        result_sets: vec![],
        depth,
    };
    // The parameters: a frame labelled with the procedure's name.
    let mut f = Frame::new(Some(r.name.clone()));
    for (k, p) in r.parms.iter().enumerate() {
        let given = values.get(k).cloned();
        let v = match given {
            Some(v) => v,
            None => match &p.default {
                Some(d) => run.value(d, Some(&p.ty)).await?,
                None => bail!("SQL0440: Procedure {} needs parameter {}.", r.name, p.name),
            },
        };
        f.vars.insert(
            p.name.clone(),
            Var {
                ty: Some(p.ty.clone()),
                val: v,
            },
        );
    }
    run.frames.push(f);
    // IN values get the parameters' types.
    for p in &r.parms {
        if p.mode != "OUT" {
            let v = run.lookup(&p.name).and_then(|v| v.val.clone());
            run.assign_typed(&p.name, v).await?;
        }
    }
    let flow = run.exec(&body).await?;
    let status = match flow {
        Flow::Return(Some(v)) => v.trim().parse().unwrap_or(0),
        _ => 0,
    };
    let _ = &run.routine;
    let values = r
        .parms
        .iter()
        .map(|p| run.frames[0].vars.get(&p.name).and_then(|v| v.val.clone()))
        .collect();
    Ok(CallResult {
        values,
        result_sets: std::mem::take(&mut run.result_sets),
        status,
    })
}

/// An external procedure: CALL of the program, its parameters passed by
/// reference (PARAMETER STYLE GENERAL); OUT and INOUT parameters are what
/// the program left in them.
async fn call_external(
    env: &Env<'_>,
    r: &Routine,
    pgm: &str,
    values: Vec<Option<String>>,
) -> Result<CallResult> {
    let mut args = Vec::new();
    for (k, p) in r.parms.iter().enumerate() {
        let v = values.get(k).cloned().flatten();
        let numeric = is_numeric_type(&p.ty);
        args.push(match v {
            Some(v) => v,
            None if numeric => "0".into(),
            None => String::new(),
        });
    }
    let environment = crate::commands::Environment {
        database_url: env.database_url.to_string(),
    };
    let mut job = crate::commands::Job {
        client: env.catalog,
        env: &environment,
        user: env.user.to_string(),
        curlib: env.curlib.clone(),
        libl: env.libl.to_vec(),
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.call_program_boxed(pgm, args.clone())
        .await
        .map_err(|e| anyhow!("SQL0443: Procedure {} failed: {e:#}", r.name))?;
    let back = job.calls.returned.take().unwrap_or(args);
    Ok(CallResult {
        values: back.into_iter().map(Some).collect(),
        result_sets: vec![],
        status: 0,
    })
}

// ------------------------------------------------------------ catalog

/// The routine a name means: `LIB/NAME` or `LIB.NAME`, or `NAME` through
/// the library list (the SQL path).
pub async fn find(
    catalog: &Client,
    libl: &[String],
    name: &str,
    kind: &str,
) -> Result<Option<Routine>> {
    let name = name.trim().to_ascii_uppercase().replace('.', "/");
    let (libs, n) = match name.split_once('/') {
        Some((l, n)) => (vec![l.to_string()], n.to_string()),
        None => (libl.to_vec(), name.clone()),
    };
    for lib in libs {
        if let Some(row) = catalog
            .query_opt(
                "SELECT definition FROM as4tainr.routines WHERE library=$1 AND name=$2 AND kind=$3",
                &[&lib, &n, &kind],
            )
            .await?
        {
            let def: String = row.get(0);
            return Ok(Some(serde_json::from_str(&def)?));
        }
    }
    Ok(None)
}

async fn store(catalog: &Client, r: &Routine, replace: bool) -> Result<()> {
    let def = serde_json::to_string(r)?;
    let n = catalog
        .execute(
            if replace {
                "INSERT INTO as4tainr.routines (library, name, kind, definition) VALUES ($1,$2,$3,$4)
                 ON CONFLICT (library, name, kind) DO UPDATE SET definition = EXCLUDED.definition"
            } else {
                "INSERT INTO as4tainr.routines (library, name, kind, definition) VALUES ($1,$2,$3,$4)
                 ON CONFLICT DO NOTHING"
            },
            &[&r.library, &r.name, &r.kind, &def],
        )
        .await?;
    if n == 0 {
        bail!(
            "SQL0454: {} {} in {} already exists.",
            r.kind.to_ascii_lowercase(),
            r.name,
            r.library
        );
    }
    Ok(())
}

/// CREATE PROCEDURE / FUNCTION: the routine goes to the library named,
/// else the default schema, else the current library, else QGPL.
pub async fn create(
    catalog: &Client,
    database_url: &str,
    libl: &[String],
    target: String,
    mut r: Routine,
    replace: bool,
) -> Result<String> {
    if r.library.is_empty() {
        r.library = target;
    }
    if !catalog::library_exists(catalog, &r.library).await? {
        bail!("SQL0204: {} in QSYS type *LIB not found.", r.library);
    }
    if r.kind == "FUNCTION" {
        define_function(catalog, database_url, libl, &r, replace).await?;
    }
    store(catalog, &r, replace).await?;
    Ok(format!(
        "{} {} was created in {}.",
        if r.kind == "FUNCTION" {
            "Function"
        } else {
            "Procedure"
        },
        r.name,
        r.library
    ))
}

/// DROP PROCEDURE / FUNCTION.
pub async fn drop_routine(
    catalog: &Client,
    libl: &[String],
    kind: &str,
    lib: Option<String>,
    name: &str,
    if_exists: bool,
) -> Result<String> {
    let q = match &lib {
        Some(l) => format!("{l}/{name}"),
        None => name.to_string(),
    };
    let Some(r) = find(catalog, libl, &q, kind).await? else {
        if if_exists {
            return Ok(format!("{} {name} does not exist.", title(kind)));
        }
        bail!(
            "SQL0204: {name} in {} type {} not found.",
            lib.as_deref().unwrap_or("*LIBL"),
            kind
        );
    };
    if kind == "FUNCTION" {
        catalog.batch_execute(catalog::ROLE_DDL_LOCK).await?;
        let r2 = catalog
            .batch_execute(&format!(
                "DROP FUNCTION IF EXISTS {}.{}",
                ident(&r.library),
                ident(&r.name)
            ))
            .await;
        let _ = catalog.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
        r2.map_err(|e| crate::sql::map_error(&e, ""))?;
    }
    catalog
        .execute(
            "DELETE FROM as4tainr.routines WHERE library=$1 AND name=$2 AND kind=$3",
            &[&r.library, &r.name, &kind],
        )
        .await?;
    Ok(format!(
        "{} {} in {} was dropped.",
        title(kind),
        r.name,
        r.library
    ))
}

fn title(kind: &str) -> &'static str {
    if kind == "FUNCTION" {
        "Function"
    } else {
        "Procedure"
    }
}

/// CALL with the arguments as expressions (`?` or an OUT parameter's
/// argument: no value), as STRSQL, RUNSQL and embedded SQL write them.
pub async fn call_exprs(
    env: &Env<'_>,
    name: &str,
    args: &[String],
) -> Result<(Routine, CallResult)> {
    let found = find(env.catalog, env.libl, name, "PROCEDURE").await?;
    if found.is_none() {
        if let Some(r) = call_service(env, name, args).await {
            return r;
        }
    }
    let r = found.ok_or_else(|| {
        anyhow!(
            "SQL0440: Procedure {} with {} parameters not found.",
            name.to_ascii_uppercase(),
            args.len()
        )
    })?;
    if args.len() > r.parms.len() {
        bail!(
            "SQL0440: Procedure {} with {} parameters not found.",
            r.name,
            args.len()
        );
    }
    let run = Run {
        env,
        routine: r.name.clone(),
        frames: vec![],
        diag: (0, "00000".into(), String::new()),
        row_count: 0,
        handler_floor: 0,
        result_sets: vec![],
        depth: 0,
    };
    let mut values = Vec::new();
    for (p, a) in r.parms.iter().zip(args) {
        let a = a.trim();
        values.push(if p.mode == "OUT" || a == "?" {
            None
        } else if a.eq_ignore_ascii_case("DEFAULT") {
            match &p.default {
                Some(d) => run.value(d, Some(&p.ty)).await?,
                None => None,
            }
        } else {
            run.value(a, None).await?
        });
    }
    // Parameters left out take their defaults.
    let out = run_procedure(env, &r, values, 0).await?;
    Ok((r, out))
}

/// The IBM i services that are procedures (QSYS2.IFS_WRITE,
/// IFS_WRITE_UTF8, IFS_WRITE_BINARY, QCMDEXC), qualified with QSYS2 or
/// found through the path. None when `name` is not one.
async fn call_service(
    env: &Env<'_>,
    name: &str,
    args: &[String],
) -> Option<Result<(Routine, CallResult)>> {
    let up = name.trim().to_ascii_uppercase().replace('/', ".");
    let base = match up.split_once('.') {
        Some(("QSYS2", n)) => n.to_string(),
        Some(_) => return None,
        None => up,
    };
    let params: &[&str] = match base.as_str() {
        "IFS_WRITE" | "IFS_WRITE_UTF8" | "IFS_WRITE_BINARY" => &[
            "PATH_NAME",
            "LINE",
            "FILE_CCSID",
            "OVERWRITE",
            "END_OF_LINE",
        ],
        "QCMDEXC" => &["COMMAND"],
        _ => return None,
    };
    Some(run_service(env, &base, params, args).await)
}

async fn run_service(
    env: &Env<'_>,
    name: &str,
    params: &[&str],
    args: &[String],
) -> Result<(Routine, CallResult)> {
    let run = Run {
        env,
        routine: name.to_string(),
        frames: vec![],
        diag: (0, "00000".into(), String::new()),
        row_count: 0,
        handler_floor: 0,
        result_sets: vec![],
        depth: 0,
    };
    // Arguments by position, or by name (PATH_NAME => ...).
    let mut values: Vec<Option<String>> = vec![None; params.len()];
    let mut given = vec![false; params.len()];
    for (i, a) in args.iter().enumerate() {
        let (slot, expr) = match a.split_once("=>") {
            Some((n, e)) if params.iter().any(|p| p.eq_ignore_ascii_case(n.trim())) => (
                params
                    .iter()
                    .position(|p| p.eq_ignore_ascii_case(n.trim()))
                    .unwrap(),
                e.trim(),
            ),
            _ => (i, a.trim()),
        };
        if slot >= params.len() {
            bail!(
                "SQL0440: Procedure {name} with {} parameters not found.",
                args.len()
            );
        }
        values[slot] = run.value(expr, None).await?;
        given[slot] = true;
    }
    let get = |n: &str| -> Option<String> {
        params
            .iter()
            .position(|p| *p == n)
            .and_then(|i| values[i].clone())
    };
    if name == "QCMDEXC" {
        let cmd = get("COMMAND").ok_or_else(|| anyhow!("SQL0204: QCMDEXC needs a command."))?;
        let environment = crate::commands::Environment {
            database_url: env.database_url.to_string(),
        };
        let mut job = crate::commands::Job {
            client: env.catalog,
            env: &environment,
            user: env.user.to_string(),
            curlib: env.curlib.clone(),
            libl: env
                .libl
                .iter()
                .filter(|l| !crate::commands::SYSTEM_LIBRARY_LIST.contains(&l.as_str()))
                .cloned()
                .collect(),
            display: None,
            id: None,
            calls: Default::default(),
        };
        Box::pin(job.run(cmd.trim())).await?;
    } else {
        ifs_write(env, name, &get)?.await?;
    }
    let r = Routine {
        library: "QSYS2".into(),
        name: name.to_string(),
        kind: "PROCEDURE".into(),
        parms: params
            .iter()
            .map(|p| Parm {
                mode: "IN".into(),
                name: p.to_string(),
                ty: "VARCHAR(32740)".into(),
                default: None,
            })
            .collect(),
        returns: None,
        result_sets: 0,
        body: String::new(),
        external: None,
    };
    Ok((
        r,
        CallResult {
            values,
            ..Default::default()
        },
    ))
}

/// IFS_WRITE, IFS_WRITE_UTF8, IFS_WRITE_BINARY (IBM i services): a line
/// added to a stream file (OVERWRITE APPEND, the default), replacing it
/// (REPLACE), or only to a new file (NONE); END_OF_LINE after it (CRLF;
/// NONE for binary). A new file's CCSID is FILE_CCSID, or the job's (37)
/// for IFS_WRITE and IFS_WRITE_BINARY and 1208 for IFS_WRITE_UTF8.
fn ifs_write<'e>(
    env: &'e Env<'_>,
    name: &str,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<BoxFut<'e, Result<()>>> {
    use crate::ifs;
    let binary = name == "IFS_WRITE_BINARY";
    let path = get("PATH_NAME").ok_or_else(|| anyhow!("SQL0204: {name} needs PATH_NAME."))?;
    let line = get("LINE").unwrap_or_default();
    let ccsid: i32 = get("FILE_CCSID")
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);
    let overwrite = get("OVERWRITE")
        .unwrap_or_else(|| "APPEND".into())
        .trim()
        .to_ascii_uppercase();
    let eol = get("END_OF_LINE")
        .unwrap_or_else(|| if binary { "NONE".into() } else { "CRLF".into() })
        .trim()
        .to_ascii_uppercase();
    let eol = match eol.as_str() {
        "NONE" => "",
        "CR" => "\r",
        "LF" => "\n",
        "CRLF" => "\r\n",
        "LFCR" => "\n\r",
        other => bail!("SQL0171: END_OF_LINE {other} is not valid."),
    };
    if binary && !eol.is_empty() {
        bail!("SQL0171: END_OF_LINE must be NONE for IFS_WRITE_BINARY.");
    }
    if !["APPEND", "REPLACE", "NONE"].contains(&overwrite.as_str()) {
        bail!("SQL0171: OVERWRITE {overwrite} is not valid.");
    }
    let new_ccsid = match (ccsid, name) {
        (c, _) if c > 0 => c,
        (_, "IFS_WRITE_UTF8") => 1208,
        _ => 37,
    };
    let user = env.user.to_string();
    let client = env.catalog;
    Ok(Box::pin(async move {
        let existing = ifs::stat(client, &path).await?;
        if existing.is_some() && overwrite == "NONE" {
            bail!("SQL0443: IFS_WRITE: stream file {path} exists (OVERWRITE NONE).");
        }
        let file_ccsid = existing.as_ref().map(|e| e.ccsid).unwrap_or(new_ccsid);
        if !binary && !ifs::codec::supported(file_ccsid) {
            bail!("SQL0443: IFS_WRITE: CCSID {file_ccsid} cannot be converted.");
        }
        let mut bytes = if binary {
            // Binary values are shown as hexadecimal digits.
            (0..line.len() / 2)
                .filter_map(|i| u8::from_str_radix(line.get(2 * i..2 * i + 2)?, 16).ok())
                .collect()
        } else {
            let mut b = Vec::new();
            for c in line.chars().chain(eol.chars()) {
                ifs::codec::encode_char(file_ccsid, c, &mut b);
            }
            b
        };
        if existing.is_some() && overwrite == "APPEND" {
            let (mut old, _) = ifs::read_file(client, &path).await?;
            old.append(&mut bytes);
            bytes = old;
        }
        ifs::write_file(client, &path, &bytes, file_ccsid, 0o644, &user)
            .await
            .map_err(|e| anyhow!("SQL0443: IFS_WRITE: {path}: {e}"))
    }))
}

/// Is the statement a compound statement (`BEGIN`, or `label: BEGIN`)?
pub fn is_compound(sql: &str) -> bool {
    match Ps::new(sql) {
        Ok(mut p) => p.word().as_deref() == Some("BEGIN") || label_ahead(&p),
        Err(_) => false,
    }
}

/// A compound statement run by itself (`BEGIN ... END` in STRSQL, RUNSQL
/// or RUNSQLSTM: a dynamic compound statement). `Ok(None)`: the
/// statement is not one.
pub async fn run_dynamic(env: &Env<'_>, sql: &str) -> Result<Option<Vec<ResultSet>>> {
    if !is_compound(sql) {
        return Ok(None);
    }
    let body = parse_body(sql.trim().trim_end_matches(';'))?;
    let mut run = Run {
        env,
        routine: String::new(),
        frames: vec![],
        diag: (0, "00000".into(), String::new()),
        row_count: 0,
        handler_floor: 0,
        result_sets: vec![],
        depth: 0,
    };
    run.exec(&body).await?;
    Ok(Some(std::mem::take(&mut run.result_sets)))
}

// ------------------------------------------------------------ functions

/// The PostgreSQL type of an SQL data type as written.
fn pg_type(ty: &str) -> Result<String> {
    Ok(crate::ddl::parse_type(ty)?.sql_type())
}

/// A simple name for PostgreSQL (parameters and variables are lower case
/// there; unquoted references fold to it).
fn pg_name(n: &str) -> Result<String> {
    if n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && n.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        Ok(n.to_ascii_lowercase())
    } else {
        bail!(
            "SQL0084: the name {n} cannot be used in a function yet (letters, digits and _ only)."
        )
    }
}

struct FnGen {
    libraries: Vec<String>,
    out: String,
}

impl FnGen {
    fn is_library(&self) -> impl Fn(&str) -> bool + '_ {
        |n: &str| self.libraries.iter().any(|l| l == n)
    }

    /// An expression, as a scalar subquery PostgreSQL runs.
    fn expr(&self, e: &str) -> Result<String> {
        let t = crate::sql::translate(
            &format!("SELECT {e} FROM SYSIBM.SYSDUMMY1"),
            &self.is_library(),
        )?;
        Ok(format!("({})", t.sql.trim().trim_end_matches(';')))
    }

    fn cond(&self, c: &str) -> Result<String> {
        let t = crate::sql::translate(
            &format!("SELECT 1 FROM SYSIBM.SYSDUMMY1 WHERE {c}"),
            &self.is_library(),
        )?;
        Ok(format!("EXISTS ({})", t.sql.trim().trim_end_matches(';')))
    }

    fn block(&mut self, body: &[Stmt]) -> Result<()> {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn label(&mut self, l: &Option<String>) -> Result<()> {
        if let Some(l) = l {
            self.out.push_str(&format!("<<{}>> ", pg_name(l)?));
        }
        Ok(())
    }

    fn compound(&mut self, c: &Compound) -> Result<()> {
        if !c.handlers.is_empty() || !c.cursors.is_empty() || !c.conditions.is_empty() {
            bail!("SQL0084: handlers, cursors and conditions are not supported in functions yet.");
        }
        self.label(&c.label)?;
        if !c.vars.is_empty() {
            self.out.push_str("DECLARE ");
            for (n, ty, d) in &c.vars {
                self.out
                    .push_str(&format!("{} {}", pg_name(n)?, pg_type(ty)?));
                if let Some(d) = d {
                    self.out.push_str(&format!(" := {}", self.expr(d)?));
                }
                self.out.push_str("; ");
            }
        }
        self.out.push_str("BEGIN ");
        self.block(&c.body)?;
        self.out.push_str("END; ");
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Compound(c) => self.compound(c)?,
            Stmt::Set(list) => {
                for (t, e) in list {
                    let e = self.expr(e)?;
                    self.out.push_str(&format!("{} := {e}; ", pg_name(t)?));
                }
            }
            Stmt::If(arms, els) => {
                for (k, (c, b)) in arms.iter().enumerate() {
                    let c = self.cond(c)?;
                    self.out.push_str(&format!(
                        "{} {c} THEN ",
                        if k == 0 { "IF" } else { "ELSIF" }
                    ));
                    self.block(b)?;
                }
                if let Some(b) = els {
                    self.out.push_str("ELSE ");
                    self.block(b)?;
                }
                self.out.push_str("END IF; ");
            }
            Stmt::Case(subject, arms, els) => {
                for (k, (w, b)) in arms.iter().enumerate() {
                    let c = match subject {
                        Some(x) => format!("({x}) = ({w})"),
                        None => w.clone(),
                    };
                    let c = self.cond(&c)?;
                    self.out.push_str(&format!(
                        "{} {c} THEN ",
                        if k == 0 { "IF" } else { "ELSIF" }
                    ));
                    self.block(b)?;
                }
                self.out.push_str("ELSE ");
                match els {
                    Some(b) => self.block(b)?,
                    None => self.out.push_str(
                        "RAISE EXCEPTION 'No WHEN of the CASE statement was true.' USING ERRCODE = '20000'; ",
                    ),
                }
                self.out.push_str("END IF; ");
            }
            Stmt::While(l, c, b) => {
                self.label(l)?;
                let c = self.cond(c)?;
                self.out.push_str(&format!("WHILE {c} LOOP "));
                self.block(b)?;
                self.out.push_str("END LOOP; ");
            }
            Stmt::Repeat(l, b, c) => {
                self.label(l)?;
                self.out.push_str("LOOP ");
                self.block(b)?;
                let c = self.cond(c)?;
                self.out.push_str(&format!("EXIT WHEN {c}; END LOOP; "));
            }
            Stmt::Loop(l, b) => {
                self.label(l)?;
                self.out.push_str("LOOP ");
                self.block(b)?;
                self.out.push_str("END LOOP; ");
            }
            Stmt::Leave(l) => self.out.push_str(&format!("EXIT {}; ", pg_name(l)?)),
            Stmt::Iterate(l) => self.out.push_str(&format!("CONTINUE {}; ", pg_name(l)?)),
            Stmt::Return(Some(e)) => {
                let e = self.expr(e)?;
                self.out.push_str(&format!("RETURN {e}; "));
            }
            Stmt::Return(None) => self.out.push_str("RETURN NULL; "),
            Stmt::Signal(Some(state), msg, false)
                if state.len() == 5 && state.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                let m = match msg {
                    Some(m) => self.expr(m)?,
                    None => format!("'SQLSTATE {state} was signaled.'"),
                };
                self.out.push_str(&format!(
                    "RAISE EXCEPTION '%', {m} USING ERRCODE = '{}'; ",
                    state.to_ascii_uppercase()
                ));
            }
            Stmt::Sql(text) => match split_into(text)? {
                Some((q, targets)) => {
                    let t = crate::sql::translate(&q, &self.is_library())?;
                    let names: Vec<String> =
                        targets.iter().map(|t| pg_name(t)).collect::<Result<_>>()?;
                    self.out.push_str(&format!(
                        "SELECT * INTO {} FROM ({}) AS q; ",
                        names.join(", "),
                        t.sql.trim().trim_end_matches(';')
                    ));
                }
                None => bail!(
                    "SQL0084: only SELECT INTO and VALUES INTO are supported in functions yet."
                ),
            },
            _ => bail!("SQL0084: this statement is not supported in functions yet."),
        }
        Ok(())
    }
}

/// Define a function in PostgreSQL, from the parsed body, as the SQL
/// login (as a view is defined).
async fn define_function(
    catalog: &Client,
    database_url: &str,
    libl: &[String],
    r: &Routine,
    replace: bool,
) -> Result<()> {
    let body = parse_body(&r.body)?;
    let mut g = FnGen {
        libraries: catalog::list_libraries(catalog).await?,
        out: String::new(),
    };
    if let Some(cols) = r
        .returns
        .as_deref()
        .and_then(|t| t.strip_prefix("TABLE("))
        .and_then(|t| t.strip_suffix(')'))
    {
        return define_table_function(catalog, database_url, libl, r, replace, &g, cols, &body)
            .await;
    }
    let ret = pg_type(r.returns.as_deref().unwrap_or("INTEGER"))?;
    let (language, code) = match &body {
        Stmt::Return(Some(e)) => ("sql", format!("SELECT CAST({} AS {ret})", g.expr(e)?)),
        Stmt::Compound(c) => {
            g.compound(c)?;
            (
                "plpgsql",
                g.out.trim_end().trim_end_matches(';').to_string(),
            )
        }
        _ => bail!("SQL0084: a function body is RETURN or BEGIN ... END."),
    };
    let params: Vec<String> = r
        .parms
        .iter()
        .map(|p| Ok(format!("{} {}", pg_name(&p.name)?, pg_type(&p.ty)?)))
        .collect::<Result<_>>()?;
    // A dollar quote that the body does not contain.
    let mut tag = String::from("as4tainr_body");
    while code.contains(&format!("${tag}$")) {
        tag.push('x');
    }
    let text = format!(
        "CREATE {}FUNCTION {}.{}({}) RETURNS {ret} LANGUAGE {language} AS ${tag}${code}${tag}$",
        if replace { "OR REPLACE " } else { "" },
        ident(&r.library),
        ident(&r.name),
        params.join(", ")
    );
    crate::sql::define_as_sql_role(catalog, database_url, libl, &r.library, &text).await
}

/// A table function (RETURNS TABLE): its body is RETURN fullselect (Db2 for
/// i SQL Reference, CREATE FUNCTION (SQL table)); in PostgreSQL an SQL
/// function returning a table whose columns are cast to the declared
/// types. It is called as TABLE(f(...)) in a FROM clause.
#[allow(clippy::too_many_arguments)]
async fn define_table_function(
    catalog: &Client,
    database_url: &str,
    libl: &[String],
    r: &Routine,
    replace: bool,
    g: &FnGen,
    cols: &str,
    body: &Stmt,
) -> Result<()> {
    let Stmt::Return(Some(q)) = body else {
        bail!("SQL0084: a table function's body must be RETURN fullselect yet.");
    };
    // The column definitions, split at commas outside parentheses.
    let mut defs = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    for c in cols.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                defs.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    defs.push(cur);
    let mut out_cols = Vec::new();
    let mut types = Vec::new();
    for d in &defs {
        let d = d.trim();
        let (n, t) = d.split_once(' ').unwrap_or((d, "INTEGER"));
        let t = pg_type(t.trim())?;
        out_cols.push(format!("{} {t}", pg_name(n)?));
        types.push(t);
    }
    let mut q = q.trim();
    while q.starts_with('(') && q.ends_with(')') {
        q = q[1..q.len() - 1].trim();
    }
    let t = crate::sql::translate(q, &g.is_library())?;
    // The query's result columns (without the relative record number),
    // found with the parameters as typed null values.
    let probe_names: Vec<(String, String)> = r
        .parms
        .iter()
        .map(|p| Ok((p.name.clone(), format!("CAST(NULL AS {})", pg_type(&p.ty)?))))
        .collect::<Result<_>>()?;
    let probe = crate::sql::substitute_names(q, &probe_names)?;
    let fields = crate::sql::query_fields(catalog, database_url, libl, &probe).await?;
    if fields.len() != defs.len() {
        bail!(
            "SQL0117: the query has {} columns and RETURNS TABLE {}.",
            fields.len(),
            defs.len()
        );
    }
    let casts: Vec<String> = fields
        .iter()
        .zip(&types)
        .map(|(f, t)| format!("CAST(as4tainr_q.{} AS {t})", ident(&f.name)))
        .collect();
    let code = format!(
        "SELECT {} FROM ({}) AS as4tainr_q",
        casts.join(", "),
        t.sql.trim().trim_end_matches(';')
    );
    let params: Vec<String> = r
        .parms
        .iter()
        .map(|p| Ok(format!("{} {}", pg_name(&p.name)?, pg_type(&p.ty)?)))
        .collect::<Result<_>>()?;
    let mut tag = String::from("as4tainr_body");
    while code.contains(&format!("${tag}$")) {
        tag.push('x');
    }
    let text = format!(
        "CREATE {}FUNCTION {}.{}({}) RETURNS TABLE ({}) LANGUAGE sql STABLE AS ${tag}${code}${tag}$",
        if replace { "OR REPLACE " } else { "" },
        ident(&r.library),
        ident(&r.name),
        params.join(", "),
        out_cols.join(", ")
    );
    crate::sql::define_as_sql_role(catalog, database_url, libl, &r.library, &text).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Db2 for i SQL Reference: a compound statement's statements end with
    // semicolons; END IF / END WHILE / END CASE end their statements, and a
    // CASE expression ends with END.
    #[test]
    fn scripts_and_bodies() {
        let script = "CREATE PROCEDURE P1 (IN A INT) BEGIN DECLARE X INT DEFAULT 0; \
            IF A > 0 THEN SET X = CASE WHEN A > 5 THEN 2 ELSE 1 END; END IF; \
            WHILE X < 3 DO SET X = X + 1; END WHILE; END; \
            INSERT INTO T VALUES (1); CREATE FUNCTION F1 (A INT) RETURNS INT RETURN A + 1;";
        let st = crate::ddl::split_statements(script);
        assert_eq!(st.len(), 3, "{st:?}");
        assert!(st[0].ends_with("END"), "{}", st[0]);
        let (_, r) = parse_create(&st[0]).unwrap().unwrap();
        assert_eq!(r.parms[0].name, "A");
        let Stmt::Compound(c) = parse_body(&r.body).unwrap() else {
            panic!("compound expected")
        };
        assert_eq!(c.vars[0].0, "X");
        assert!(matches!(&c.body[0], Stmt::If(arms, None) if arms.len() == 1));
        assert!(matches!(&c.body[1], Stmt::While(None, _, b) if b.len() == 1));
        // Options, including SET OPTION and NOT DETERMINISTIC.
        let (_, o) = parse_create(
            "CREATE PROCEDURE P2 () LANGUAGE SQL NOT DETERMINISTIC SET OPTION COMMIT = *NONE, DATFMT = *ISO BEGIN END",
        )
        .unwrap()
        .unwrap();
        assert!(o.external.is_none() && o.body.starts_with("BEGIN"));
        let (_, x) = parse_create(
            "CREATE PROCEDURE P3 (INOUT A CHAR(5)) LANGUAGE RPGLE EXTERNAL NAME 'MYLIB/MYPGM' PARAMETER STYLE GENERAL",
        )
        .unwrap()
        .unwrap();
        assert_eq!(x.external.as_deref(), Some("MYLIB/MYPGM"));
        let (_, f) = parse_create(&st[2]).unwrap().unwrap();
        assert_eq!(f.returns.as_deref(), Some("INT"));
        assert_eq!(
            parse_body(&f.body).unwrap(),
            Stmt::Return(Some("A + 1".into()))
        );
        assert_eq!(
            split_into("SELECT A, B INTO X, Y FROM T WHERE K = 1").unwrap(),
            Some((
                "SELECT A, B FROM T WHERE K = 1".into(),
                vec!["X".into(), "Y".into()]
            ))
        );
        assert_eq!(
            parse_call("CALL LIB.P (1, 'a', ?)").unwrap(),
            Some(("LIB/P".into(), vec!["1".into(), "'a'".into(), "?".into()]))
        );
    }
}
