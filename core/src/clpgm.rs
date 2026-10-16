//! CL programs (CLLE): parsing ("compiling") and running them.
//!
//! Written from IBM's public CL reference. A program is checked when it is
//! created (`ibmish crtbndcl`) and its source is kept; CALL runs it here,
//! in the job that called it. Supported:
//!
//! - `PGM PARM(&a ...)` / `ENDPGM`, labels (`LABEL: command`), comments
//!   `/* */`, continuation with `+` and `-`
//! - `DCL VAR TYPE(*CHAR|*DEC|*LGL|*INT) LEN VALUE`
//! - `CHGVAR`, expressions with `*CAT *BCAT *TCAT` (`|| |> |<`), arithmetic,
//!   relational (`*EQ`... or `= > <` ...) and logical (`*AND *OR *NOT`)
//!   operators, `%SST`/`%SUBSTRING`, `%TRIM`/`%TRIML`/`%TRIMR`, `%CHAR`,
//!   `%DEC`, `%INT`, `*ON`/`*OFF`, `*BLANKS`
//! - `IF COND THEN` / `ELSE CMD`, `DO` / `ENDDO`, `DOWHILE`, `DOUNTIL`,
//!   `DOFOR`, `LEAVE`, `ITERATE`, `SELECT` / `WHEN` / `OTHERWISE` /
//!   `ENDSELECT`, `GOTO CMDLBL`, `RETURN`
//! - `MONMSG MSGID EXEC` after a command, or program-level (right after
//!   the declarations); generic IDs like `CPF0000` and `CPF9800`
//! - `SNDPGMMSG` (MSG, MSGID/MSGDTA, MSGTYPE *INFO *COMP *DIAG *ESCAPE),
//!   `CHKOBJ`, `RTVJOBA` (JOB, USER, NBR, CURLIB, TYPE), `RTVDTAARA`
//! - `CALL PGM PARM` (CL programs here, others through the RPG runner)
//! - any other command the job knows, with `&variables` replaced by
//!   their values
//!
//! Not supported yet: see docs/cl-programs.md.

use anyhow::{anyhow, bail, Result};
use rust_decimal::prelude::*;
use std::collections::HashMap;

use crate::catalog;
use crate::commands::{Job, Outcome};

// ------------------------------------------------------------ source

/// One statement of the source: label, command name, parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// 1-based line number where it starts (for messages).
    pub line: usize,
    pub label: Option<String>,
    pub name: String,
    pub params: Vec<Param>,
}

/// A parameter: `KEYWORD(value)` or a positional value. The value is the
/// text as written (inside the parentheses for keyword form).
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub keyword: Option<String>,
    pub value: String,
}

/// Remove comments and join continuation lines. A line ending in `+`
/// continues with the next line's first non-blank; `-` keeps the next
/// line's leading blanks. Returns (line number, text) per statement.
fn logical_lines(source: &str) -> Result<Vec<(usize, String)>> {
    // Comments first, outside quotes.
    let mut text = String::new();
    let cs: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut in_q = false;
    while i < cs.len() {
        if !in_q && cs[i] == '/' && cs.get(i + 1) == Some(&'*') {
            let mut j = i + 2;
            while j + 1 < cs.len() && !(cs[j] == '*' && cs[j + 1] == '/') {
                if cs[j] == '\n' {
                    text.push('\n'); // keep line numbers
                }
                j += 1;
            }
            if j + 1 >= cs.len() {
                bail!("a comment is not closed with */");
            }
            i = j + 2;
            text.push(' ');
            continue;
        }
        if cs[i] == '\'' {
            in_q = !in_q;
        }
        text.push(cs[i]);
        i += 1;
    }
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut pending: Option<(usize, String, bool)> = None; // (line, text, skip leading blanks)
    for (n, raw) in text.lines().enumerate() {
        let lineno = n + 1;
        let line = raw.trim_end_matches('\r');
        let piece = match &pending {
            Some((_, _, true)) => line.trim_start(),
            _ => line,
        };
        let t = piece.trim_end();
        let (body, cont) = if let Some(b) = t.strip_suffix('+') {
            (b.to_string(), Some(true))
        } else if let Some(b) = t.strip_suffix('-') {
            (b.to_string(), Some(false))
        } else {
            (t.to_string(), None)
        };
        let (start, mut acc) = match pending.take() {
            Some((s, a, _)) => (s, a),
            None => (lineno, String::new()),
        };
        acc.push_str(&body);
        match cont {
            Some(skip) => pending = Some((start, acc, skip)),
            None => {
                if !acc.trim().is_empty() {
                    out.push((start, acc.trim().to_string()));
                }
            }
        }
    }
    if let Some((s, a, _)) = pending {
        if !a.trim().is_empty() {
            out.push((s, a.trim().to_string()));
        }
    }
    Ok(out)
}

/// Split the text after a command name into parameters.
fn split_params(text: &str) -> Result<Vec<Param>> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    // A balanced parenthesized group starting at `i` ('('), quote-aware.
    let group = |i: usize| -> Result<(String, usize)> {
        let mut depth = 0;
        let mut j = i;
        let mut in_q = false;
        while j < cs.len() {
            match cs[j] {
                '\'' => in_q = !in_q,
                '(' if !in_q => depth += 1,
                ')' if !in_q => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok((cs[i + 1..j].iter().collect(), j + 1));
                    }
                }
                _ => {}
            }
            j += 1;
        }
        bail!("a closing parenthesis is missing")
    };
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            let (v, j) = group(i)?;
            out.push(Param {
                keyword: None,
                value: format!("({v})"),
            });
            i = j;
        } else if c == '\'' {
            let s = i;
            i += 1;
            while i < cs.len() {
                if cs[i] == '\'' && cs.get(i + 1) == Some(&'\'') {
                    i += 2;
                } else if cs[i] == '\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            out.push(Param {
                keyword: None,
                value: cs[s..i].iter().collect(),
            });
        } else {
            let s = i;
            while i < cs.len() && !cs[i].is_whitespace() && cs[i] != '(' {
                i += 1;
            }
            let w: String = cs[s..i].iter().collect();
            if i < cs.len() && cs[i] == '(' && !w.starts_with('&') {
                let (v, j) = group(i)?;
                out.push(Param {
                    keyword: Some(w.to_ascii_uppercase()),
                    value: v,
                });
                i = j;
            } else {
                out.push(Param {
                    keyword: None,
                    value: w,
                });
            }
        }
    }
    Ok(out)
}

/// Parse one statement's text ("LABEL: CMD params").
fn parse_statement(line: usize, text: &str) -> Result<Statement> {
    let mut rest = text.trim();
    let mut label = None;
    // A label: a name followed by ':' before the command.
    if let Some(colon) = rest.find(':') {
        let head = &rest[..colon];
        if !head.is_empty()
            && !head.contains(char::is_whitespace)
            && !head.contains('\'')
            && !head.contains('(')
        {
            label = Some(head.to_ascii_uppercase());
            rest = rest[colon + 1..].trim();
        }
    }
    if rest.is_empty() {
        return Ok(Statement {
            line,
            label,
            name: String::new(),
            params: vec![],
        });
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(rest.len());
    let mut name = rest[..end].to_ascii_uppercase();
    // A qualified command name (QSYS/CRTLIB): the library is ignored.
    if let Some((_, n)) = name.split_once('/') {
        name = n.to_string();
    }
    let params = split_params(&rest[end..]).map_err(|e| anyhow!("line {line}: {e}"))?;
    Ok(Statement {
        line,
        label,
        name,
        params,
    })
}

/// Parameter values by keyword, positional ones assigned in `order`.
fn named(st: &Statement, order: &[&str]) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut pos = order.iter();
    for p in &st.params {
        let key = match &p.keyword {
            Some(k) => {
                if !order.contains(&k.as_str()) {
                    bail!("line {}: keyword {k} is not valid for {}", st.line, st.name);
                }
                k.clone()
            }
            None => pos
                .find(|k| !out.contains_key(**k))
                .ok_or_else(|| {
                    anyhow!(
                        "line {}: too many positional parameters for {}",
                        st.line,
                        st.name
                    )
                })?
                .to_string(),
        };
        // A positional value in parentheses, like LEN (5 2) or COND (&A = 1).
        let value = match (
            &p.keyword,
            p.value.strip_prefix('(').and_then(|v| v.strip_suffix(')')),
        ) {
            (None, Some(inner)) => inner.to_string(),
            _ => p.value.clone(),
        };
        if out.insert(key.clone(), value).is_some() {
            bail!("line {}: {key} is given more than once", st.line);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------ values

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    Char,
    Dec,
    Lgl,
    Int,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Char(String),
    Dec(Decimal),
    Lgl(bool),
}

#[derive(Debug, Clone)]
pub struct Var {
    pub ty: VarType,
    pub len: u32,
    pub decimals: u32,
    pub value: Value,
}

/// The text of a *DEC value as CL shows it for %CHAR: no leading zeros,
/// a leading minus sign, the decimal positions of the value.
fn dec_text(d: &Decimal) -> String {
    d.to_string()
}

impl Value {
    fn as_lgl(&self) -> Result<bool> {
        match self {
            Value::Lgl(b) => Ok(*b),
            Value::Char(s) if s == "1" => Ok(true),
            Value::Char(s) if s == "0" => Ok(false),
            other => bail!(
                "a logical value ('0' or '1') is needed, not {}",
                other.describe()
            ),
        }
    }

    fn as_dec(&self) -> Result<Decimal> {
        match self {
            Value::Dec(d) => Ok(*d),
            other => bail!("a number is needed, not {}", other.describe()),
        }
    }

    fn as_char(&self) -> Result<String> {
        match self {
            Value::Char(s) => Ok(s.clone()),
            Value::Lgl(b) => Ok(if *b { "1" } else { "0" }.into()),
            other => bail!("a character value is needed, not {}", other.describe()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Value::Char(s) => format!("'{s}'"),
            Value::Dec(d) => d.to_string(),
            Value::Lgl(b) => format!("'{}'", if *b { 1 } else { 0 }),
        }
    }

    /// The value as a CL command parameter: names and numbers as they are,
    /// other text in apostrophes.
    fn as_parameter(&self) -> String {
        match self {
            Value::Dec(d) => dec_text(d),
            Value::Lgl(b) => format!("'{}'", if *b { 1 } else { 0 }),
            Value::Char(s) => {
                let t = s.trim_end();
                let plain = !t.is_empty()
                    && t.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "*$#@_./".contains(c));
                if plain {
                    t.to_string()
                } else {
                    format!("'{}'", t.replace('\'', "''"))
                }
            }
        }
    }
}

// ------------------------------------------------------------ expressions

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Var(String),
    Lit(Value),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(String, Box<Expr>, Box<Expr>),
    Builtin(String, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum ET {
    Var(String),
    Str(String),
    Num(String),
    Op(String),
    Builtin(String),
    Open,
    Close,
    Colon,
}

fn expr_tokens(s: &str) -> Result<Vec<ET>> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let next = cs.get(i + 1).copied();
        match c {
            ' ' | '\t' => i += 1,
            '(' => {
                out.push(ET::Open);
                i += 1;
            }
            ')' => {
                out.push(ET::Close);
                i += 1;
            }
            ':' => {
                out.push(ET::Colon);
                i += 1;
            }
            '&' if next.is_some_and(|n| n.is_ascii_alphabetic() || "$#@_".contains(n)) => {
                let s0 = i;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_alphanumeric() || "$#@_".contains(cs[i])) {
                    i += 1;
                }
                out.push(ET::Var(
                    cs[s0..i].iter().collect::<String>().to_ascii_uppercase(),
                ));
            }
            '&' => {
                out.push(ET::Op("*AND".into()));
                i += 1;
            }
            '\'' => {
                let mut v = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => bail!("a closing apostrophe is missing"),
                        Some('\'') if cs.get(i + 1) == Some(&'\'') => {
                            v.push('\'');
                            i += 2;
                        }
                        Some('\'') => {
                            i += 1;
                            break;
                        }
                        Some(&x) => {
                            v.push(x);
                            i += 1;
                        }
                    }
                }
                out.push(ET::Str(v));
            }
            '0'..='9' | '.' => {
                let s0 = i;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.' || cs[i] == ',') {
                    i += 1;
                }
                out.push(ET::Num(
                    cs[s0..i].iter().collect::<String>().replace(',', "."),
                ));
            }
            '*' if next.is_some_and(|n| n.is_ascii_alphabetic()) => {
                let s0 = i;
                i += 1;
                while i < cs.len() && cs[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                let w = cs[s0..i].iter().collect::<String>().to_ascii_uppercase();
                match w.as_str() {
                    "*ON" => out.push(ET::Str("1".into())),
                    "*OFF" => out.push(ET::Str("0".into())),
                    "*BLANK" | "*BLANKS" => out.push(ET::Str(" ".into())),
                    _ => out.push(ET::Op(w)),
                }
            }
            '%' => {
                let s0 = i;
                i += 1;
                while i < cs.len() && cs[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                out.push(ET::Builtin(
                    cs[s0..i].iter().collect::<String>().to_ascii_uppercase(),
                ));
            }
            '|' if next == Some('|') => {
                out.push(ET::Op("*CAT".into()));
                i += 2;
            }
            '|' if next == Some('>') => {
                out.push(ET::Op("*BCAT".into()));
                i += 2;
            }
            '|' if next == Some('<') => {
                out.push(ET::Op("*TCAT".into()));
                i += 2;
            }
            '|' => {
                out.push(ET::Op("*OR".into()));
                i += 1;
            }
            '¬' | '!' if next == Some('=') => {
                out.push(ET::Op("*NE".into()));
                i += 2;
            }
            '¬' | '!' if next == Some('>') => {
                out.push(ET::Op("*NG".into()));
                i += 2;
            }
            '¬' | '!' if next == Some('<') => {
                out.push(ET::Op("*NL".into()));
                i += 2;
            }
            '¬' | '!' => {
                out.push(ET::Op("*NOT".into()));
                i += 1;
            }
            '<' if next == Some('>') => {
                out.push(ET::Op("*NE".into()));
                i += 2;
            }
            '>' | '<' if next == Some('=') => {
                out.push(ET::Op(if c == '>' { "*GE" } else { "*LE" }.into()));
                i += 2;
            }
            '>' => {
                out.push(ET::Op("*GT".into()));
                i += 1;
            }
            '<' => {
                out.push(ET::Op("*LT".into()));
                i += 1;
            }
            '=' => {
                out.push(ET::Op("*EQ".into()));
                i += 1;
            }
            '+' | '-' | '*' | '/' => {
                out.push(ET::Op(c.to_string()));
                i += 1;
            }
            _ => {
                // A bare word: a character constant (names, special values).
                let s0 = i;
                while i < cs.len() && !" ()&'|*+-/=<>¬!:".contains(cs[i]) {
                    i += 1;
                }
                if i == s0 {
                    bail!("'{c}' is not valid in an expression");
                }
                out.push(ET::Str(
                    cs[s0..i].iter().collect::<String>().to_ascii_uppercase(),
                ));
            }
        }
    }
    Ok(out)
}

/// Precedence, low to high, from the CL reference: *OR; *AND; relational;
/// *CAT/*BCAT/*TCAT; + -; * /; *NOT and unary minus.
fn prec(op: &str) -> Option<u8> {
    Some(match op {
        "*OR" => 1,
        "*AND" => 2,
        "*EQ" | "*NE" | "*GT" | "*LT" | "*GE" | "*LE" | "*NG" | "*NL" => 3,
        "*CAT" | "*BCAT" | "*TCAT" => 4,
        "+" | "-" => 5,
        "*" | "/" => 6,
        _ => return None,
    })
}

struct ExprParser {
    toks: Vec<ET>,
    pos: usize,
}

impl ExprParser {
    fn peek(&self) -> Option<&ET> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<ET> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expr(&mut self, min: u8) -> Result<Expr> {
        let mut lhs = self.unary()?;
        while let Some(ET::Op(o)) = self.peek() {
            let op = o.clone();
            let Some(p) = prec(&op) else {
                bail!("{op} is not valid here")
            };
            if p < min {
                break;
            }
            self.pos += 1;
            let rhs = self.expr(p + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(ET::Op(o)) if o == "*NOT" => Ok(Expr::Not(Box::new(self.unary()?))),
            Some(ET::Op(o)) if o == "-" => Ok(Expr::Neg(Box::new(self.unary()?))),
            Some(ET::Op(o)) if o == "+" => self.unary(),
            Some(ET::Var(v)) => Ok(Expr::Var(v)),
            Some(ET::Str(s)) => Ok(Expr::Lit(Value::Char(s))),
            Some(ET::Num(n)) => Ok(Expr::Lit(Value::Dec(
                Decimal::from_str(&n).map_err(|_| anyhow!("{n} is not a valid number"))?,
            ))),
            Some(ET::Open) => {
                let e = self.expr(0)?;
                match self.next() {
                    Some(ET::Close) => Ok(e),
                    _ => bail!("a closing parenthesis is missing"),
                }
            }
            Some(ET::Builtin(name)) => {
                if self.next() != Some(ET::Open) {
                    bail!("{name} needs its arguments in parentheses");
                }
                let mut args = Vec::new();
                // One-argument functions take any expression.
                if ["%CHAR", "%TRIM", "%TRIML", "%TRIMR", "%DEC", "%INT"].contains(&name.as_str()) {
                    args.push(self.expr(0)?);
                    if self.next() != Some(ET::Close) {
                        bail!("{name} takes one argument");
                    }
                    return Ok(Expr::Builtin(name, args));
                }
                loop {
                    if self.peek() == Some(&ET::Close) {
                        self.pos += 1;
                        break;
                    }
                    // Arguments are separated by blanks (and ':' in %SST
                    // written the old way); each is a term or a
                    // parenthesized expression.
                    args.push(self.unary_or_paren()?);
                    if self.peek() == Some(&ET::Colon) {
                        self.pos += 1;
                    }
                }
                Ok(Expr::Builtin(name, args))
            }
            other => bail!("an operand is expected, found {other:?}"),
        }
    }

    /// A built-in's argument: one operand, or an expression in parentheses.
    fn unary_or_paren(&mut self) -> Result<Expr> {
        let e = self.unary()?;
        // Allow simple arithmetic like %SST(&A 1 &N+1) without blanks.
        if let Some(ET::Op(o)) = self.peek() {
            if o == "+" || o == "-" {
                let op = o.clone();
                self.pos += 1;
                let rhs = self.unary()?;
                return Ok(Expr::Bin(op, Box::new(e), Box::new(rhs)));
            }
        }
        Ok(e)
    }
}

pub fn parse_expr(s: &str) -> Result<Expr> {
    let mut p = ExprParser {
        toks: expr_tokens(s)?,
        pos: 0,
    };
    let e = p.expr(0)?;
    if p.pos < p.toks.len() {
        bail!("unexpected {:?} in expression {s}", p.toks[p.pos]);
    }
    Ok(e)
}

fn pad_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let n = a.chars().count().max(b.chars().count());
    let pa = format!("{a:<n$}");
    let pb = format!("{b:<n$}");
    pa.cmp(&pb)
}

fn eval(e: &Expr, vars: &HashMap<String, Var>) -> Result<Value> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Var(n) => vars
            .get(n)
            .ok_or_else(|| anyhow!("CPD0727: Variable {n} is not declared."))?
            .value
            .clone(),
        Expr::Not(x) => Value::Lgl(!eval(x, vars)?.as_lgl()?),
        Expr::Neg(x) => Value::Dec(-eval(x, vars)?.as_dec()?),
        Expr::Bin(op, a, b) => {
            let (va, vb) = (eval(a, vars)?, eval(b, vars)?);
            match op.as_str() {
                "+" | "-" | "*" | "/" => {
                    let (x, y) = (va.as_dec()?, vb.as_dec()?);
                    Value::Dec(match op.as_str() {
                        "+" => x + y,
                        "-" => x - y,
                        "*" => x * y,
                        _ => {
                            if y.is_zero() {
                                bail!("MCH1211: Attempt made to divide by zero.");
                            }
                            x / y
                        }
                    })
                }
                "*CAT" => Value::Char(va.as_char()? + &vb.as_char()?),
                "*TCAT" => Value::Char(va.as_char()?.trim_end().to_string() + &vb.as_char()?),
                "*BCAT" => {
                    let a = va.as_char()?.trim_end().to_string();
                    let b = vb.as_char()?;
                    Value::Char(if a.is_empty() { b } else { format!("{a} {b}") })
                }
                "*AND" => Value::Lgl(va.as_lgl()? && vb.as_lgl()?),
                "*OR" => Value::Lgl(va.as_lgl()? || vb.as_lgl()?),
                rel => {
                    use std::cmp::Ordering::*;
                    let ord = match (&va, &vb) {
                        (Value::Dec(x), Value::Dec(y)) => x.cmp(y),
                        (Value::Lgl(x), Value::Lgl(y)) => x.cmp(y),
                        (Value::Dec(_), _) | (_, Value::Dec(_)) => {
                            bail!("cannot compare {} with {}", va.describe(), vb.describe())
                        }
                        _ => pad_cmp(&va.as_char()?, &vb.as_char()?),
                    };
                    Value::Lgl(match rel {
                        "*EQ" => ord == Equal,
                        "*NE" => ord != Equal,
                        "*GT" => ord == Greater,
                        "*LT" => ord == Less,
                        "*GE" | "*NL" => ord != Less,
                        "*LE" | "*NG" => ord != Greater,
                        other => bail!("{other} is not an operator"),
                    })
                }
            }
        }
        Expr::Builtin(name, args) => {
            let arg = |i: usize| -> Result<Value> {
                eval(
                    args.get(i)
                        .ok_or_else(|| anyhow!("{name} needs more arguments"))?,
                    vars,
                )
            };
            match name.as_str() {
                "%SST" | "%SUBSTRING" => {
                    let s: Vec<char> = arg(0)?.as_char()?.chars().collect();
                    let start = arg(1)?.as_dec()?.to_usize().unwrap_or(0);
                    let len = arg(2)?.as_dec()?.to_usize().unwrap_or(0);
                    if start == 0 || len == 0 || start + len - 1 > s.len() {
                        bail!(
                            "CPD0012: %SST({start} {len}) is outside the string of length {}.",
                            s.len()
                        );
                    }
                    Value::Char(s[start - 1..start - 1 + len].iter().collect())
                }
                "%TRIM" => Value::Char(arg(0)?.as_char()?.trim_matches(' ').to_string()),
                "%TRIML" => Value::Char(arg(0)?.as_char()?.trim_start_matches(' ').to_string()),
                "%TRIMR" => Value::Char(arg(0)?.as_char()?.trim_end_matches(' ').to_string()),
                "%CHAR" => Value::Char(match arg(0)? {
                    Value::Dec(d) => dec_text(&d),
                    other => other.as_char()?,
                }),
                "%DEC" | "%INT" => {
                    let v = arg(0)?;
                    let d = match v {
                        Value::Dec(d) => d,
                        other => Decimal::from_str(other.as_char()?.trim()).map_err(|_| {
                            anyhow!(
                                "MCH1202: '{}' is not a valid number.",
                                other.as_char().unwrap_or_default()
                            )
                        })?,
                    };
                    Value::Dec(if name == "%INT" { d.trunc() } else { d })
                }
                other => bail!("built-in function {other} is not supported yet"),
            }
        }
    })
}

/// Store a value in a variable: character values are padded or
/// truncated to its length; decimals are truncated to its decimal
/// positions and must fit its digits.
fn assign(name: &str, var: &mut Var, v: Value) -> Result<()> {
    var.value = match var.ty {
        VarType::Char => {
            let s = match v {
                // Number to character: the digits with leading zeros, a
                // decimal point, and '-' first when negative.
                Value::Dec(d) => {
                    let neg = d.is_sign_negative() && !d.is_zero();
                    let mut a = d.abs();
                    a.rescale(var.decimals.min(28));
                    let t = a.to_string();
                    if neg {
                        format!("-{t}")
                    } else {
                        t
                    }
                }
                other => other.as_char()?,
            };
            let n = var.len as usize;
            let mut s: String = s.chars().take(n).collect();
            while s.chars().count() < n {
                s.push(' ');
            }
            Value::Char(s)
        }
        VarType::Lgl => Value::Lgl(v.as_lgl()?),
        VarType::Dec | VarType::Int => {
            let d = match v {
                Value::Dec(d) => d,
                Value::Char(s) => Decimal::from_str(s.trim()).map_err(|_| {
                    anyhow!("MCH1202: '{}' is not a valid number for {name}.", s.trim())
                })?,
                Value::Lgl(_) => bail!("a logical value cannot be stored in {name}"),
            };
            let d = d.trunc_with_scale(var.decimals);
            let int_digits = var.len.saturating_sub(var.decimals);
            let limit = Decimal::from(10u64.pow(int_digits.min(18)));
            if d.abs().trunc() >= limit {
                bail!(
                    "MCH1210: The value {d} does not fit in {name} ({} digits, {} decimals).",
                    var.len,
                    var.decimals
                );
            }
            Value::Dec(d)
        }
    };
    Ok(())
}

// ------------------------------------------------------------ program

/// What the program does, step by step.
#[derive(Debug, Clone)]
enum Ins {
    /// Run a command; on an error, the monitors (generic IDs and where
    /// their EXEC starts) are tried in order.
    Cmd {
        st: Statement,
        monitors: Vec<(Vec<String>, usize)>,
    },
    ChgVar(String, Expr),
    JumpIfFalse(Expr, usize),
    Jump(usize),
    /// Jump to a label (resolved when the program is checked).
    Goto(String),
    Return,
    /// End of a program-level MONMSG's EXEC: go on after the command that
    /// failed.
    Resume,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub name: String,
    pub params: Vec<String>,
    pub decls: Vec<(String, Var)>,
    code: Vec<Ins>,
    labels: HashMap<String, usize>,
    /// Program-level MONMSG: (IDs, EXEC start).
    global: Vec<(Vec<String>, usize)>,
}

fn is_msgid(s: &str) -> bool {
    s.len() == 7
        && s[..3].chars().all(|c| c.is_ascii_uppercase())
        && s[3..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Does a message ID match a MONMSG ID (exact, or generic: CPF0000 all
/// CPF messages, CPF9800 all CPF98xx)?
pub fn msgid_matches(monitor: &str, id: &str) -> bool {
    if monitor.len() != 7 || id.len() != 7 || monitor[..3] != id[..3] {
        return false;
    }
    if &monitor[3..] == "0000" {
        true
    } else if &monitor[5..] == "00" {
        monitor[3..5] == id[3..5]
    } else {
        monitor == id
    }
}

/// The message ID an error carries ("CPF2110: ..."), or CPF9898 for an
/// error without one.
pub fn error_id(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let head = s.split(':').next().unwrap_or("");
    if is_msgid(head) {
        head.to_string()
    } else {
        "CPF9898".into()
    }
}

struct Compiler<'a> {
    sts: &'a [Statement],
    i: usize,
    code: Vec<Ins>,
    labels: HashMap<String, usize>,
    /// Open loops, innermost last.
    loops: Vec<Loop>,
}

/// A loop being compiled: where ITERATE and LEAVE go.
struct Loop {
    /// ITERATE target, once known (DOWHILE: the test; DOUNTIL and DOFOR:
    /// the end of the body, known at ENDDO).
    iterate: Option<usize>,
    /// ITERATE jumps waiting for that target.
    pending_iterates: Vec<usize>,
    /// Jumps to the loop end (LEAVE, and the DOWHILE/DOFOR test).
    leaves: Vec<usize>,
}

impl Compiler<'_> {
    fn err(&self, st: &Statement, msg: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("line {}: {}: {msg}", st.line, st.name)
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn patch(&mut self, at: usize, to: usize) {
        match &mut self.code[at] {
            Ins::Jump(t) | Ins::JumpIfFalse(_, t) => *t = to,
            _ => unreachable!("patching a jump"),
        }
    }

    /// Compile statements until one of `ends` (returned) or the end.
    fn block(&mut self, ends: &[&str]) -> Result<Option<String>> {
        while self.i < self.sts.len() {
            let st = self.sts[self.i].clone();
            if ends.contains(&st.name.as_str()) {
                self.i += 1;
                if let Some(l) = &st.label {
                    self.labels.insert(l.clone(), self.here());
                }
                return Ok(Some(st.name));
            }
            self.i += 1;
            self.statement(&st)?;
        }
        Ok(None)
    }

    /// A command given as a parameter (THEN, ELSE CMD, EXEC, OTHERWISE):
    /// `DO` opens a block up to ENDDO.
    fn nested(&mut self, outer: &Statement, text: &str) -> Result<()> {
        let st = parse_statement(outer.line, text)?;
        if st.name == "DO" {
            match self.block(&["ENDDO"])? {
                Some(_) => Ok(()),
                None => Err(self.err(outer, "DO has no ENDDO")),
            }
        } else if st.name.is_empty() {
            Ok(())
        } else {
            self.statement(&st)
        }
    }

    fn statement(&mut self, st: &Statement) -> Result<()> {
        if let Some(l) = &st.label {
            if self.labels.insert(l.clone(), self.here()).is_some() {
                return Err(self.err(st, format!("label {l} is defined twice")));
            }
        }
        match st.name.as_str() {
            "" => {}
            "DCL" | "PGM" => return Err(self.err(st, "must come at the start of the program")),
            "ENDPGM" => self.code.push(Ins::Return),
            "CHGVAR" => {
                let p = named(st, &["VAR", "VALUE"])?;
                let var = p
                    .get("VAR")
                    .ok_or_else(|| self.err(st, "VAR is required"))?;
                let value = p
                    .get("VALUE")
                    .ok_or_else(|| self.err(st, "VALUE is required"))?;
                let target = var.trim().to_ascii_uppercase();
                if !target.starts_with('&') {
                    return Err(self.err(st, format!("{var} is not a variable")));
                }
                let e = parse_expr(value).map_err(|e| self.err(st, e))?;
                self.code.push(Ins::ChgVar(target, e));
            }
            "IF" => {
                let p = named(st, &["COND", "THEN"])?;
                let cond = parse_expr(
                    p.get("COND")
                        .ok_or_else(|| self.err(st, "COND is required"))?,
                )
                .map_err(|e| self.err(st, e))?;
                let jf = self.here();
                self.code.push(Ins::JumpIfFalse(cond, 0));
                if let Some(t) = p.get("THEN") {
                    self.nested(st, t)?;
                }
                // ELSE right after the IF (and its DO group)?
                if self.sts.get(self.i).is_some_and(|n| n.name == "ELSE") {
                    let els = self.sts[self.i].clone();
                    self.i += 1;
                    let j = self.here();
                    self.code.push(Ins::Jump(0));
                    let else_start = self.here();
                    self.patch(jf, else_start);
                    if let Some(l) = &els.label {
                        self.labels.insert(l.clone(), else_start);
                    }
                    let p = named(&els, &["CMD"])?;
                    if let Some(c) = p.get("CMD") {
                        self.nested(&els, c)?;
                    }
                    let end = self.here();
                    self.patch(j, end);
                } else {
                    let end = self.here();
                    self.patch(jf, end);
                }
            }
            "ELSE" => return Err(self.err(st, "ELSE must follow an IF")),
            "DO" => {
                if self.block(&["ENDDO"])?.is_none() {
                    return Err(self.err(st, "DO has no ENDDO"));
                }
            }
            "ENDDO" | "ENDSELECT" | "WHEN" | "OTHERWISE" => {
                return Err(self.err(st, "has no matching start"))
            }
            "DOWHILE" => {
                let p = named(st, &["COND"])?;
                let cond = parse_expr(
                    p.get("COND")
                        .ok_or_else(|| self.err(st, "COND is required"))?,
                )
                .map_err(|e| self.err(st, e))?;
                let top = self.here();
                self.code.push(Ins::JumpIfFalse(cond, 0));
                self.loops.push(Loop {
                    iterate: Some(top),
                    pending_iterates: vec![],
                    leaves: vec![top],
                });
                if self.block(&["ENDDO"])?.is_none() {
                    return Err(self.err(st, "DOWHILE has no ENDDO"));
                }
                self.code.push(Ins::Jump(top));
                self.close_loop();
            }
            "DOUNTIL" => {
                let p = named(st, &["COND"])?;
                let cond = parse_expr(
                    p.get("COND")
                        .ok_or_else(|| self.err(st, "COND is required"))?,
                )
                .map_err(|e| self.err(st, e))?;
                let top = self.here();
                // ITERATE goes to the test at the end; patched below.
                self.loops.push(Loop {
                    iterate: None,
                    pending_iterates: vec![],
                    leaves: vec![],
                });
                if self.block(&["ENDDO"])?.is_none() {
                    return Err(self.err(st, "DOUNTIL has no ENDDO"));
                }
                let test = self.here();
                self.retarget_iterate(test);
                self.code.push(Ins::JumpIfFalse(cond, top));
                self.close_loop();
            }
            "DOFOR" => {
                let p = named(st, &["VAR", "FROM", "TO", "BY"])?;
                let var = p
                    .get("VAR")
                    .ok_or_else(|| self.err(st, "VAR is required"))?
                    .trim()
                    .to_ascii_uppercase();
                let from = parse_expr(
                    p.get("FROM")
                        .ok_or_else(|| self.err(st, "FROM is required"))?,
                )?;
                let to = parse_expr(p.get("TO").ok_or_else(|| self.err(st, "TO is required"))?)?;
                let by = parse_expr(p.get("BY").map(String::as_str).unwrap_or("1"))?;
                self.code.push(Ins::ChgVar(var.clone(), from));
                let top = self.here();
                // by >= 0: run while var <= to; by < 0: while var >= to.
                let cond = Expr::Bin(
                    "*OR".into(),
                    Box::new(Expr::Bin(
                        "*AND".into(),
                        Box::new(Expr::Bin(
                            "*GE".into(),
                            Box::new(by.clone()),
                            Box::new(Expr::Lit(Value::Dec(Decimal::ZERO))),
                        )),
                        Box::new(Expr::Bin(
                            "*LE".into(),
                            Box::new(Expr::Var(var.clone())),
                            Box::new(to.clone()),
                        )),
                    )),
                    Box::new(Expr::Bin(
                        "*AND".into(),
                        Box::new(Expr::Bin(
                            "*LT".into(),
                            Box::new(by.clone()),
                            Box::new(Expr::Lit(Value::Dec(Decimal::ZERO))),
                        )),
                        Box::new(Expr::Bin(
                            "*GE".into(),
                            Box::new(Expr::Var(var.clone())),
                            Box::new(to),
                        )),
                    )),
                );
                self.code.push(Ins::JumpIfFalse(cond, 0));
                self.loops.push(Loop {
                    iterate: None,
                    pending_iterates: vec![],
                    leaves: vec![top],
                });
                if self.block(&["ENDDO"])?.is_none() {
                    return Err(self.err(st, "DOFOR has no ENDDO"));
                }
                let step = self.here();
                self.retarget_iterate(step);
                self.code.push(Ins::ChgVar(
                    var.clone(),
                    Expr::Bin("+".into(), Box::new(Expr::Var(var)), Box::new(by)),
                ));
                self.code.push(Ins::Jump(top));
                self.close_loop();
            }
            "LEAVE" | "ITERATE" => {
                if self.loops.is_empty() {
                    return Err(self.err(st, "is not inside a DOWHILE, DOUNTIL or DOFOR"));
                }
                let at = self.here();
                self.code.push(Ins::Jump(0));
                let lp = self.loops.last_mut().unwrap();
                if st.name == "LEAVE" {
                    lp.leaves.push(at);
                } else {
                    match lp.iterate {
                        Some(t) => self.patch(at, t),
                        None => lp.pending_iterates.push(at),
                    }
                }
            }
            "SELECT" => {
                let mut ends = Vec::new();
                loop {
                    let Some(n) = self.sts.get(self.i).cloned() else {
                        return Err(self.err(st, "SELECT has no ENDSELECT"));
                    };
                    self.i += 1;
                    match n.name.as_str() {
                        "WHEN" => {
                            let p = named(&n, &["COND", "THEN"])?;
                            let cond = parse_expr(
                                p.get("COND")
                                    .ok_or_else(|| self.err(&n, "COND is required"))?,
                            )
                            .map_err(|e| self.err(&n, e))?;
                            let jf = self.here();
                            self.code.push(Ins::JumpIfFalse(cond, 0));
                            if let Some(t) = p.get("THEN") {
                                self.nested(&n, t)?;
                            }
                            ends.push(self.here());
                            self.code.push(Ins::Jump(0));
                            let next = self.here();
                            self.patch(jf, next);
                        }
                        "OTHERWISE" => {
                            let p = named(&n, &["CMD"])?;
                            if let Some(c) = p.get("CMD") {
                                self.nested(&n, c)?;
                            }
                        }
                        "ENDSELECT" => break,
                        _ => {
                            return Err(self
                                .err(&n, "only WHEN, OTHERWISE and ENDSELECT can follow SELECT"))
                        }
                    }
                }
                let end = self.here();
                for e in ends {
                    self.patch(e, end);
                }
            }
            "GOTO" => {
                let p = named(st, &["CMDLBL"])?;
                let l = p
                    .get("CMDLBL")
                    .ok_or_else(|| self.err(st, "CMDLBL is required"))?;
                self.code.push(Ins::Goto(l.trim().to_ascii_uppercase()));
            }
            "RETURN" => self.code.push(Ins::Return),
            "MONMSG" => return Err(self.err(st, "must follow a command (or the declarations)")),
            _ => {
                // A command, with its MONMSGs.
                let at = self.here();
                self.code.push(Ins::Cmd {
                    st: st.clone(),
                    monitors: vec![],
                });
                let mut skips = Vec::new();
                while self.sts.get(self.i).is_some_and(|n| n.name == "MONMSG") {
                    let m = self.sts[self.i].clone();
                    self.i += 1;
                    if skips.is_empty() {
                        // Normal flow jumps over the handlers.
                        skips.push(self.here());
                        self.code.push(Ins::Jump(0));
                    }
                    let (ids, exec) = monmsg(&m)?;
                    let start = self.here();
                    if let Ins::Cmd { monitors, .. } = &mut self.code[at] {
                        monitors.push((ids, start));
                    }
                    if let Some(x) = exec {
                        self.nested(&m, &x)?;
                    }
                    skips.push(self.here());
                    self.code.push(Ins::Jump(0));
                }
                let end = self.here();
                for s in skips {
                    self.patch(s, end);
                }
            }
        }
        Ok(())
    }

    fn retarget_iterate(&mut self, to: usize) {
        let pending = match self.loops.last_mut() {
            Some(l) => {
                l.iterate = Some(to);
                std::mem::take(&mut l.pending_iterates)
            }
            None => vec![],
        };
        for at in pending {
            self.patch(at, to);
        }
    }

    fn close_loop(&mut self) {
        let lp = self.loops.pop().expect("open loop");
        let end = self.here();
        for at in lp.leaves {
            self.patch(at, end);
        }
    }
}

fn monmsg(m: &Statement) -> Result<(Vec<String>, Option<String>)> {
    let p = named(m, &["MSGID", "CMPDTA", "EXEC"])?;
    let ids: Vec<String> = p
        .get("MSGID")
        .ok_or_else(|| anyhow!("line {}: MONMSG needs MSGID", m.line))?
        .split_whitespace()
        .map(|s| s.to_ascii_uppercase())
        .collect();
    for id in &ids {
        if !is_msgid(id) {
            bail!("line {}: {id} is not a message ID", m.line);
        }
    }
    Ok((ids, p.get("EXEC").cloned()))
}

fn parse_dcl(st: &Statement) -> Result<(String, Var)> {
    // Positional order VAR TYPE LEN VALUE; STG only by keyword in practice.
    let p = named(st, &["VAR", "TYPE", "LEN", "VALUE", "STG"])?;
    let name = p
        .get("VAR")
        .ok_or_else(|| anyhow!("line {}: DCL needs VAR", st.line))?
        .trim()
        .to_ascii_uppercase();
    if !name.starts_with('&') || name.len() < 2 || name.len() > 11 {
        bail!("line {}: {name} is not a valid variable name", st.line);
    }
    let ty = match p
        .get("TYPE")
        .map(|t| t.trim().to_ascii_uppercase())
        .as_deref()
    {
        Some("*CHAR") => VarType::Char,
        Some("*DEC") => VarType::Dec,
        Some("*LGL") => VarType::Lgl,
        Some("*INT") | Some("*UINT") => VarType::Int,
        Some(other) => bail!("line {}: TYPE({other}) is not supported yet", st.line),
        None => bail!("line {}: DCL needs TYPE", st.line),
    };
    let lens: Vec<u32> = match p.get("LEN") {
        Some(l) => l
            .split_whitespace()
            .map(|x| {
                x.parse()
                    .map_err(|_| anyhow!("line {}: LEN({l}) is not valid", st.line))
            })
            .collect::<Result<_>>()?,
        None => vec![],
    };
    // Defaults from the CL reference: *CHAR 32, *DEC (15 5), *LGL 1, *INT 4.
    let (len, decimals) = match ty {
        VarType::Char => (lens.first().copied().unwrap_or(32), 0),
        VarType::Dec => (
            lens.first().copied().unwrap_or(15),
            lens.get(1)
                .copied()
                .unwrap_or(if lens.is_empty() { 5 } else { 0 }),
        ),
        VarType::Lgl => (1, 0),
        VarType::Int => (lens.first().copied().unwrap_or(4), 0),
    };
    if ty == VarType::Dec && (len == 0 || len > 15 || decimals > len) {
        bail!(
            "line {}: *DEC length must be 1-15 digits with up to that many decimals",
            st.line
        );
    }
    let (len, decimals, digits) = match ty {
        // *INT LEN(2|4|8) bytes: stored as a decimal with that many digits.
        VarType::Int => (
            len,
            0,
            match len {
                2 => 5,
                4 => 10,
                8 => 19,
                _ => bail!("line {}: *INT length must be 2, 4 or 8", st.line),
            },
        ),
        _ => (len, decimals, len),
    };
    let mut var = Var {
        ty,
        len: digits,
        decimals,
        value: match ty {
            VarType::Char => Value::Char(" ".repeat(len as usize)),
            VarType::Lgl => Value::Lgl(false),
            _ => Value::Dec(Decimal::ZERO),
        },
    };
    if ty == VarType::Char {
        var.len = len;
    }
    if let Some(v) = p.get("VALUE") {
        let e = parse_expr(v).map_err(|e| anyhow!("line {}: VALUE({v}): {e}", st.line))?;
        let val = eval(&e, &HashMap::new())?;
        assign(&name, &mut var, val).map_err(|e| anyhow!("line {}: {e}", st.line))?;
    }
    Ok((name, var))
}

/// Check a CL program and turn it into steps (CRTBNDCL).
pub fn compile(name: &str, source: &str) -> Result<Program> {
    let mut sts = Vec::new();
    for (line, text) in logical_lines(source)? {
        sts.push(parse_statement(line, &text)?);
    }
    sts.retain(|s| !(s.name.is_empty() && s.label.is_none()));
    let mut i = 0;
    let mut params = Vec::new();
    if sts.first().is_some_and(|s| s.name == "PGM") {
        let p = named(&sts[0], &["PARM"])?;
        if let Some(parm) = p.get("PARM") {
            params = parm
                .split_whitespace()
                .map(|s| s.to_ascii_uppercase())
                .collect();
        }
        i = 1;
    }
    let mut decls = Vec::new();
    while i < sts.len() && sts[i].name == "DCL" {
        let (n, v) = parse_dcl(&sts[i])?;
        if decls.iter().any(|(d, _): &(String, Var)| *d == n) {
            bail!("line {}: {n} is declared twice", sts[i].line);
        }
        decls.push((n, v));
        i += 1;
    }
    for p in &params {
        if !decls.iter().any(|(d, _)| d == p) {
            bail!("parameter {p} is not declared");
        }
    }
    // Program-level MONMSGs.
    let mut global_src = Vec::new();
    while i < sts.len() && sts[i].name == "MONMSG" {
        global_src.push(sts[i].clone());
        i += 1;
    }
    let mut c = Compiler {
        sts: &sts,
        i,
        code: vec![],
        labels: HashMap::new(),
        loops: vec![],
    };
    c.block(&["ENDPGM"])?;
    c.code.push(Ins::Return);
    // Handlers of program-level MONMSGs go after the program.
    let mut global = Vec::new();
    for m in &global_src {
        let (ids, exec) = monmsg(m)?;
        let start = c.here();
        global.push((ids, start));
        if let Some(x) = exec {
            c.nested(m, &x)?;
        }
        c.code.push(Ins::Resume);
    }
    for ins in &c.code {
        if let Ins::Goto(l) = ins {
            if !c.labels.contains_key(l) {
                bail!("GOTO CMDLBL({l}): there is no label {l}");
            }
        }
    }
    let declared: Vec<&String> = decls.iter().map(|(n, _)| n).collect();
    for ins in &c.code {
        if let Ins::ChgVar(v, _) = ins {
            if !declared.contains(&v) {
                bail!("CPD0727: Variable {v} is not declared.");
            }
        }
    }
    Ok(Program {
        name: name.to_ascii_uppercase(),
        params,
        decls,
        labels: c.labels,
        code: c.code,
        global,
    })
}

// ------------------------------------------------------------ running

/// At most this many steps per call: a program that loops forever ends
/// with an error instead of hanging the job.
pub const MAX_STEPS: u64 = 10_000_000;

/// Replace `&VAR` in a command's parameters with the variables' values.
fn substitute(st: &Statement, vars: &HashMap<String, Var>) -> Result<String> {
    let mut out = st.name.clone();
    for p in &st.params {
        let v = substitute_text(&p.value, vars)?;
        match &p.keyword {
            Some(k) => out.push_str(&format!(" {k}({v})")),
            None => out.push_str(&format!(" {v}")),
        }
    }
    Ok(out)
}

fn substitute_text(text: &str, vars: &HashMap<String, Var>) -> Result<String> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut in_q = false;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            in_q = !in_q;
            out.push(c);
            i += 1;
        } else if c == '&'
            && !in_q
            && cs
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || "$#@_".contains(*n))
        {
            let s = i;
            i += 1;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || "$#@_".contains(cs[i])) {
                i += 1;
            }
            let name: String = cs[s..i].iter().collect::<String>().to_ascii_uppercase();
            let v = vars
                .get(&name)
                .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
            out.push_str(&v.value.as_parameter());
        } else {
            out.push(c);
            i += 1;
        }
    }
    Ok(out)
}

/// Run a CL program in a job, with CALL's parameter values. Returns the
/// messages it sent (and those of the commands it ran).
pub async fn run(job: &mut Job<'_>, pgm: &Program, args: &[String]) -> Result<Outcome> {
    let mut vars: HashMap<String, Var> = pgm.decls.iter().cloned().collect();
    if args.len() > pgm.params.len() {
        bail!(
            "CPD0172: Program {} takes {} parameters, {} were passed.",
            pgm.name,
            pgm.params.len(),
            args.len()
        );
    }
    for (p, a) in pgm.params.iter().zip(args) {
        let var = vars.get_mut(p).expect("checked at compile time");
        let v = match var.ty {
            VarType::Char | VarType::Lgl => Value::Char(a.clone()),
            _ => Value::Dec(
                Decimal::from_str(a.trim())
                    .map_err(|_| anyhow!("MCH1202: Parameter {p}: '{a}' is not a number."))?,
            ),
        };
        assign(p, var, v)?;
    }
    let mut msgs: Vec<String> = Vec::new();
    let mut pc = 0usize;
    let mut steps = 0u64;
    let mut resume_at: Option<usize> = None;
    let fail = |pgm: &Program, st: &Statement, e: &anyhow::Error| -> anyhow::Error {
        anyhow!(
            "{e:#}\nError in CL program {} at line {} ({}).",
            pgm.name,
            st.line,
            st.name
        )
    };
    while pc < pgm.code.len() {
        steps += 1;
        if steps > MAX_STEPS {
            bail!(
                "CL program {} was ended after {MAX_STEPS} steps (a loop that does not end?).",
                pgm.name
            );
        }
        match &pgm.code[pc] {
            Ins::Return => return Ok(Outcome::Messages(msgs)),
            Ins::Jump(t) => pc = *t,
            Ins::Goto(l) => pc = pgm.labels[l],
            Ins::Resume => match resume_at.take() {
                Some(r) => pc = r,
                None => pc += 1,
            },
            Ins::JumpIfFalse(cond, t) => {
                let ok = eval(cond, &vars).and_then(|v| v.as_lgl());
                match ok {
                    Ok(true) => pc += 1,
                    Ok(false) => pc = *t,
                    Err(e) => bail!("{e:#}\nError in CL program {} (a condition).", pgm.name),
                }
            }
            Ins::ChgVar(name, e) => {
                let v = eval(e, &vars)?;
                let var = vars
                    .get_mut(name)
                    .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
                assign(name, var, v)?;
                pc += 1;
            }
            Ins::Cmd { st, monitors } => {
                let result = command(job, st, &mut vars, &mut msgs).await;
                match result {
                    Ok(Some(o)) => return Ok(o),
                    Ok(None) => pc += 1,
                    Err(e) => {
                        let id = error_id(&e);
                        if let Some((_, start)) = monitors
                            .iter()
                            .find(|(ids, _)| ids.iter().any(|m| msgid_matches(m, &id)))
                        {
                            pc = *start;
                        } else if let Some((_, start)) = pgm
                            .global
                            .iter()
                            .find(|(ids, _)| ids.iter().any(|m| msgid_matches(m, &id)))
                        {
                            resume_at = Some(pc + 1);
                            pc = *start;
                        } else {
                            let mut lines = msgs.clone();
                            lines.push(fail(pgm, st, &e).to_string());
                            bail!(lines.join("\n"));
                        }
                    }
                }
            }
        }
    }
    Ok(Outcome::Messages(msgs))
}

/// Run one command of a program. Ok(Some(..)) ends the program (SIGNOFF).
async fn command(
    job: &mut Job<'_>,
    st: &Statement,
    vars: &mut HashMap<String, Var>,
    msgs: &mut Vec<String>,
) -> Result<Option<Outcome>> {
    match st.name.as_str() {
        "SNDPGMMSG" => {
            let p = named(
                st,
                &[
                    "MSG", "MSGID", "MSGF", "MSGDTA", "TOPGMQ", "TOMSGQ", "MSGTYPE", "KEYVAR",
                ],
            )?;
            let text = |s: &str| -> Result<String> {
                Ok(eval(&parse_expr(s)?, vars)?
                    .as_char()?
                    .trim_end()
                    .to_string())
            };
            let msg = match (p.get("MSG"), p.get("MSGID")) {
                (Some(m), _) => text(m)?,
                (None, Some(id)) => {
                    let data = p
                        .get("MSGDTA")
                        .map(|d| text(d))
                        .transpose()?
                        .unwrap_or_default();
                    format!("{}: {data}", id.trim().to_ascii_uppercase())
                }
                (None, None) => bail!("line {}: SNDPGMMSG needs MSG or MSGID", st.line),
            };
            let kind = p
                .get("MSGTYPE")
                .map(|t| t.trim().to_ascii_uppercase())
                .unwrap_or_else(|| "*INFO".into());
            match kind.as_str() {
                "*ESCAPE" => {
                    // Ends the program; the caller may monitor for it.
                    let with_id = if is_msgid(msg.split(':').next().unwrap_or("")) {
                        msg
                    } else {
                        format!("CPF9898: {msg}")
                    };
                    bail!(with_id)
                }
                "*INFO" | "*COMP" | "*DIAG" | "*NOTIFY" | "*STATUS" => msgs.push(msg),
                other => bail!("line {}: MSGTYPE({other}) is not supported", st.line),
            }
            Ok(None)
        }
        "CHKOBJ" => {
            let p = named(st, &["OBJ", "OBJTYPE", "MBR", "AUT"])?;
            let obj = substitute_text(
                p.get("OBJ")
                    .ok_or_else(|| anyhow!("line {}: CHKOBJ needs OBJ", st.line))?,
                vars,
            )?
            .trim_matches('\'')
            .to_ascii_uppercase();
            let objtype = p
                .get("OBJTYPE")
                .map(|t| t.trim().to_ascii_uppercase())
                .ok_or_else(|| anyhow!("line {}: CHKOBJ needs OBJTYPE", st.line))?;
            let (libs, name) = match obj.split_once('/') {
                Some(("*LIBL", n)) => (job.library_list(), n.to_string()),
                Some(("*CURLIB", n)) => (vec![job.current_library()], n.to_string()),
                Some((l, n)) => (vec![l.to_string()], n.to_string()),
                None => (job.library_list(), obj.clone()),
            };
            let lib_label = match obj.split_once('/') {
                Some((l, _)) => l.to_string(),
                None => "*LIBL".into(),
            };
            if !catalog::object_exists(job.client, &libs, &name, &objtype).await? {
                bail!("CPF9801: Object {name} in library {lib_label} not found.");
            }
            Ok(None)
        }
        "RTVJOBA" => {
            let p = named(st, &["JOB", "USER", "NBR", "CURUSER", "TYPE", "CURLIB"])?;
            for (k, target) in &p {
                let v = match k.as_str() {
                    "USER" | "CURUSER" => Value::Char(job.user.clone()),
                    "CURLIB" => Value::Char(job.curlib.clone().unwrap_or_else(|| "*CRTDFT".into())),
                    "TYPE" => Value::Char(if job.display.is_some() { "1" } else { "0" }.into()),
                    "JOB" => {
                        Value::Char(job.id.as_ref().map(|i| i.name.clone()).unwrap_or_default())
                    }
                    "NBR" => Value::Char(
                        job.id
                            .as_ref()
                            .map(|i| format!("{:06}", i.number))
                            .unwrap_or_default(),
                    ),
                    other => bail!("line {}: RTVJOBA {other} is not supported yet", st.line),
                };
                let name = target.trim().to_ascii_uppercase();
                let var = vars
                    .get_mut(&name)
                    .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
                assign(&name, var, v)?;
            }
            Ok(None)
        }
        "RTVDTAARA" => {
            let p = named(st, &["DTAARA", "RTNVAR"])?;
            let raw = substitute_text(
                p.get("DTAARA")
                    .ok_or_else(|| anyhow!("line {}: RTVDTAARA needs DTAARA", st.line))?,
                vars,
            )?;
            let (qualified, part) = crate::dtaara::split_dtaara_param(&raw)?;
            let d = crate::dtaara::resolve(
                job.client,
                &qualified,
                &job.library_list(),
                &job.current_library(),
            )
            .await?;
            crate::authority::check(
                job.client,
                &job.user,
                &d.library,
                &d.name,
                "*DTAARA",
                crate::authority::Aut::Use,
            )
            .await?;
            let target = p
                .get("RTNVAR")
                .ok_or_else(|| anyhow!("line {}: RTVDTAARA needs RTNVAR", st.line))?
                .trim()
                .to_ascii_uppercase();
            let var = vars
                .get_mut(&target)
                .ok_or_else(|| anyhow!("CPD0727: Variable {target} is not declared."))?;
            // The variable's type must suit the data area's: a *DEC data
            // area goes to a *DEC variable, *CHAR and *LGL to *CHAR or *LGL.
            let v = match (d.ty, var.ty) {
                (crate::dtaara::Type::Dec, VarType::Dec) => {
                    Value::Dec(Decimal::from_str(&d.value).map_err(|e| anyhow!("{e}"))?)
                }
                (crate::dtaara::Type::Dec, _) => bail!(
                    "Data area {} is *DEC; RTNVAR({target}) must be a *DEC variable.",
                    d.name
                ),
                (_, VarType::Char) | (crate::dtaara::Type::Lgl, VarType::Lgl) => {
                    Value::Char(d.substring(part)?)
                }
                _ => bail!(
                    "Data area {} is {}; RTNVAR({target}) must be a *CHAR variable.",
                    d.name,
                    d.ty.code()
                ),
            };
            assign(&target, var, v)?;
            Ok(None)
        }
        "CALL" => {
            let p = named(st, &["PGM", "PARM"])?;
            let pgm = substitute_text(
                p.get("PGM")
                    .ok_or_else(|| anyhow!("line {}: CALL needs PGM", st.line))?,
                vars,
            )?
            .trim_matches('\'')
            .to_ascii_uppercase();
            let mut args = Vec::new();
            if let Some(parm) = p.get("PARM") {
                for tok in split_params(parm)? {
                    let v = tok.value;
                    let val = if v.starts_with('&') {
                        let name = v.to_ascii_uppercase();
                        match &vars
                            .get(&name)
                            .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?
                            .value
                        {
                            Value::Dec(d) => dec_text(d),
                            other => other.as_char()?,
                        }
                    } else {
                        eval(&parse_expr(&v)?, vars).and_then(|x| match x {
                            Value::Dec(d) => Ok(dec_text(&d)),
                            other => other.as_char(),
                        })?
                    };
                    args.push(val);
                }
            }
            let o = job.call_program_boxed(&pgm, args).await?;
            match o {
                Outcome::Messages(m) => msgs.extend(m),
                Outcome::SignOff => return Ok(Some(Outcome::SignOff)),
                _ => {}
            }
            Ok(None)
        }
        _ => {
            let line = substitute(st, vars)?;
            match job.run_boxed(&line).await? {
                Outcome::Messages(m) => msgs.extend(m),
                Outcome::SignOff => return Ok(Some(Outcome::SignOff)),
                Outcome::Display(_)
                | Outcome::ChangePassword
                | Outcome::StartSql
                | Outcome::Menu(_)
                | Outcome::CommandEntry => {
                    bail!(
                        "{} shows a display; display commands cannot run in a CL program yet.",
                        st.name
                    )
                }
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(p: &Program) -> HashMap<String, Var> {
        p.decls.iter().cloned().collect()
    }

    fn ev(src: &str, v: &HashMap<String, Var>) -> Value {
        eval(&parse_expr(src).unwrap(), v).unwrap()
    }

    // IBM CL reference: continuation with + (skip leading blanks) and -
    // (keep them); comments; labels.
    #[test]
    fn source_lines() {
        let src = "PGM /* start */\n  DCL VAR(&A) TYPE(*CHAR) +\n        LEN(10) VALUE('a -\n  b')\nTOP:  CHGVAR &A 'x'\nENDPGM\n";
        let l = logical_lines(src).unwrap();
        assert_eq!(
            l[1],
            (2, "DCL VAR(&A) TYPE(*CHAR) LEN(10) VALUE('a   b')".into())
        );
        let st = parse_statement(l[2].0, &l[2].1).unwrap();
        assert_eq!(st.label.as_deref(), Some("TOP"));
        assert_eq!(st.name, "CHGVAR");
        assert_eq!(st.params[0].value, "&A");
    }

    // CL reference: *CAT keeps blanks, *TCAT drops the first operand's
    // trailing blanks, *BCAT puts exactly one blank between.
    #[test]
    fn concatenation_and_substrings() {
        let p = compile("T", "PGM\nDCL &A *CHAR 5 'ab'\nDCL &B *CHAR 3 'cd'\nENDPGM").unwrap();
        let v = vars(&p);
        assert_eq!(ev("&A *CAT &B", &v), Value::Char("ab   cd ".into()));
        assert_eq!(ev("&A *TCAT &B", &v), Value::Char("abcd ".into()));
        assert_eq!(ev("&A *BCAT &B", &v), Value::Char("ab cd ".into()));
        assert_eq!(ev("&A || 'x'", &v), Value::Char("ab   x".into()));
        assert_eq!(ev("%SST(&A 2 1)", &v), Value::Char("b".into()));
        assert_eq!(ev("%TRIM('  q  ')", &v), Value::Char("q".into()));
    }

    // CL reference: operator precedence (*NOT, then * /, then + -, then
    // concatenation, then relational, then *AND, then *OR); character
    // comparisons pad the shorter operand with blanks.
    #[test]
    fn precedence_and_comparisons() {
        let v = HashMap::new();
        assert_eq!(ev("2 + 3 * 4", &v), Value::Dec(Decimal::from(14)));
        assert_eq!(ev("(2 + 3) * 4", &v), Value::Dec(Decimal::from(20)));
        assert_eq!(ev("1 = 1 *AND 2 > 3 *OR 'A' = 'A  '", &v), Value::Lgl(true));
        assert_eq!(ev("*NOT ('a' *LT 'b')", &v), Value::Lgl(false));
        assert_eq!(
            ev("10 / 4", &v),
            Value::Dec(Decimal::from_str("2.5").unwrap())
        );
        assert_eq!(ev("*ON", &v), Value::Char("1".into()));
        assert!(eval(&parse_expr("1 / 0").unwrap(), &v).is_err());
    }

    // CL reference: *DEC results are truncated (not rounded) to the
    // variable's decimal positions; a value that does not fit its digits
    // is an error. Character variables are padded/truncated to LEN.
    #[test]
    fn assignment_rules() {
        let p = compile("T", "PGM\nDCL &D *DEC (5 2)\nDCL &C *CHAR 3\nENDPGM").unwrap();
        let mut v = vars(&p);
        let d = v.get_mut("&D").unwrap();
        assign("&D", d, Value::Dec(Decimal::from_str("12.349").unwrap())).unwrap();
        assert_eq!(d.value, Value::Dec(Decimal::from_str("12.34").unwrap()));
        assert!(assign("&D", d, Value::Dec(Decimal::from(1000))).is_err());
        let c = v.get_mut("&C").unwrap();
        assign("&C", c, Value::Char("abcdef".into())).unwrap();
        assert_eq!(c.value, Value::Char("abc".into()));
        assign("&C", c, Value::Char("a".into())).unwrap();
        assert_eq!(c.value, Value::Char("a  ".into()));
    }

    #[test]
    fn generic_message_ids() {
        // CL reference, MONMSG: CPF0000 monitors every CPF message;
        // CPF9800 every CPF98xx message.
        assert!(msgid_matches("CPF0000", "CPF2110"));
        assert!(msgid_matches("CPF9800", "CPF9801"));
        assert!(!msgid_matches("CPF9800", "CPF9701"));
        assert!(msgid_matches("CPF2110", "CPF2110"));
        assert!(!msgid_matches("CPF0000", "MCH1210"));
        assert_eq!(
            error_id(&anyhow!("CPF2110: Library X not found.")),
            "CPF2110"
        );
        assert_eq!(error_id(&anyhow!("no id here")), "CPF9898");
    }

    #[test]
    fn compile_errors() {
        let e = |s: &str| compile("T", s).unwrap_err().to_string();
        assert!(e("PGM\nDO\nENDPGM").contains("ENDDO"));
        assert!(e("PGM\nGOTO NOWHERE\nENDPGM").contains("NOWHERE"));
        assert!(e("PGM\nCHGVAR &X 1\nENDPGM").contains("CPD0727"));
        assert!(e("PGM\nLEAVE\nENDPGM").contains("not inside"));
        assert!(e("PGM\nELSE\nENDPGM").contains("ELSE"));
        assert!(e("PGM PARM(&P)\nENDPGM").contains("not declared"));
        assert!(e("PGM\nMONMSG CPF0000\nDCL &A *CHAR 1\nENDPGM").contains("start"));
    }
}
