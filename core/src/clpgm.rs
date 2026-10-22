//! CL programs (CLLE): parsing ("compiling") and running them.
//!
//! Written from IBM's public CL reference. A program is checked when it is
//! created (`as4tainr crtbndcl`) and its source is kept; CALL runs it here,
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
//!   `RTVJOBA` (JOB, USER, NBR, CURLIB, TYPE), `RTVDTAARA`
//! - `CALL PGM PARM` (CL programs here, others through the RPG runner)
//! - any other command the job knows, with `&variables` replaced by
//!   their values
//!
//! Not supported yet: see docs/cl-programs.md.

use anyhow::{anyhow, bail, Result};
use rust_decimal::prelude::*;
use std::collections::{BTreeMap, HashMap};

use crate::commands::{Job, Outcome};
use crate::jobs;

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
pub(crate) fn logical_lines(source: &str) -> Result<Vec<(usize, String)>> {
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
            // A built-in function as a positional value: %SST(&A 1 2).
            if i < cs.len() && cs[i] == '(' && w.starts_with('%') {
                let (v, j) = group(i)?;
                out.push(Param {
                    keyword: None,
                    value: format!("{w}({v})"),
                });
                i = j;
                continue;
            }
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
pub(crate) fn parse_statement(line: usize, text: &str) -> Result<Statement> {
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
pub(crate) fn named(st: &Statement, order: &[&str]) -> Result<HashMap<String, String>> {
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
    /// A pointer (TYPE(*PTR)).
    Ptr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Char(String),
    Dec(Decimal),
    Lgl(bool),
    /// A pointer: the variable it addresses and the offset in it (None:
    /// the null pointer).
    Ptr(Option<(String, usize)>),
}

#[derive(Debug, Clone)]
pub struct Var {
    pub ty: VarType,
    pub len: u32,
    pub decimals: u32,
    pub value: Value,
    /// STG(*DEFINED) DEFVAR(&base start): its storage is part of another
    /// variable's (CL reference, DCL): (base variable, start, 1-based).
    pub defined: Option<(String, usize)>,
    /// STG(*BASED) BASPTR(&ptr): its storage is where the pointer points.
    pub based: Option<String>,
    /// The value as last taken from its storage (*DEFINED, *BASED): a
    /// change since is written back.
    pub shadow: Option<Value>,
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
            Value::Ptr(None) => "*NULL".into(),
            Value::Ptr(Some((v, o))) => format!("pointer to {v} + {o}"),
        }
    }

    /// The value as a CL command parameter: names and numbers as they are,
    /// other text in apostrophes.
    fn as_parameter(&self) -> String {
        match self {
            Value::Dec(d) => dec_text(d),
            Value::Lgl(b) => format!("'{}'", if *b { 1 } else { 0 }),
            Value::Ptr(_) => "*NULL".into(),
            Value::Char(s) => {
                let t = s.trim_end();
                // A variable's value goes to the command as it is: text
                // with lower case letters stays as it is (quoted).
                let plain = !t.is_empty()
                    && t.chars().all(|c| {
                        c.is_ascii_uppercase() || c.is_ascii_digit() || "*$#@_./".contains(c)
                    });
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
            // X'C1C2': a hexadecimal character constant (CL reference,
            // "Hexadecimal constants"): its bytes as characters.
            'X' | 'x' if next == Some('\'') => {
                let mut j = i + 2;
                let mut hex = String::new();
                while j < cs.len() && cs[j] != '\'' {
                    hex.push(cs[j]);
                    j += 1;
                }
                if j >= cs.len()
                    || !hex.len().is_multiple_of(2)
                    || !hex.chars().all(|c| c.is_ascii_hexdigit())
                {
                    bail!("X'{hex}' is not a valid hexadecimal constant");
                }
                let bytes: Vec<u8> = (0..hex.len() / 2)
                    .map(|k| u8::from_str_radix(&hex[2 * k..2 * k + 2], 16).unwrap_or(0))
                    .collect();
                out.push(ET::Str(crate::rpg::data::a(&bytes)));
                i = j + 1;
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
                    "*EQ" | "*NE" | "*GT" | "*LT" | "*GE" | "*LE" | "*NG" | "*NL" | "*AND"
                    | "*OR" | "*NOT" | "*CAT" | "*BCAT" | "*TCAT" => out.push(ET::Op(w)),
                    // Any other special value is a character constant
                    // (&OPT *EQ *YES).
                    _ => out.push(ET::Str(w)),
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
                // These take any expression first (more arguments may
                // follow).
                if [
                    "%CHAR", "%TRIM", "%TRIML", "%TRIMR", "%DEC", "%INT", "%UPPER", "%LOWER",
                ]
                .contains(&name.as_str())
                {
                    args.push(self.expr(0)?);
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
        Expr::Var(n) => {
            not_passed(n, vars)?;
            vars.get(n)
                .ok_or_else(|| anyhow!("CPD0727: Variable {n} is not declared."))?
                .value
                .clone()
        }
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
                // %ADDRESS(&var): a pointer to the variable's storage;
                // %OFFSET(&ptr): where in it the pointer points.
                "%ADDRESS" | "%ADDR" => match args.first() {
                    Some(Expr::Var(v)) => {
                        let target = vars
                            .get(v)
                            .ok_or_else(|| anyhow!("CPD0727: Variable {v} is not declared."))?;
                        // A defined variable: the pointer is into its base.
                        match &target.defined {
                            Some((base, start)) => Value::Ptr(Some((base.clone(), start - 1))),
                            None => Value::Ptr(Some((v.clone(), 0))),
                        }
                    }
                    _ => bail!("{name} needs a variable"),
                },
                "%OFFSET" | "%OFS" => match arg(0)? {
                    Value::Ptr(Some((_, o))) => Value::Dec(Decimal::from(o as u64)),
                    Value::Ptr(None) => Value::Dec(Decimal::ZERO),
                    other => bail!("%OFFSET needs a pointer, not {}", other.describe()),
                },
                // %TRIM(string [characters]): blanks, or the characters
                // given, from both ends (left, right).
                "%TRIM" | "%TRIML" | "%TRIMR" => {
                    let s = arg(0)?.as_char()?;
                    let set: Vec<char> = match args.get(1) {
                        Some(_) => arg(1)?.as_char()?.chars().collect(),
                        None => vec![' '],
                    };
                    let f = |c: char| set.contains(&c);
                    Value::Char(match name.as_str() {
                        "%TRIML" => s.trim_start_matches(f).to_string(),
                        "%TRIMR" => s.trim_end_matches(f).to_string(),
                        _ => s.trim_matches(f).to_string(),
                    })
                }
                // %SCAN(search source [start]): where the search argument
                // first is in the source, from the start; 0 when it is not.
                "%SCAN" => {
                    let pat: Vec<char> = arg(0)?.as_char()?.chars().collect();
                    let src: Vec<char> = arg(1)?.as_char()?.chars().collect();
                    let start = match args.get(2) {
                        Some(_) => arg(2)?.as_dec()?.to_usize().unwrap_or(0),
                        None => 1,
                    };
                    if start == 0 || start > src.len() + 1 {
                        bail!(
                            "CPD0012: %SCAN start {start} is not valid for a string of length {}.",
                            src.len()
                        );
                    }
                    let found = if pat.is_empty() {
                        None
                    } else {
                        (start - 1..=src.len().saturating_sub(pat.len())).find(|&i| {
                            i + pat.len() <= src.len() && src[i..i + pat.len()] == pat[..]
                        })
                    };
                    Value::Dec(Decimal::from(found.map(|i| i + 1).unwrap_or(0) as u64))
                }
                // %CHECK(comparator base [start]): the first character of the
                // base not in the comparator; %CHECKR from the right.
                "%CHECK" | "%CHECKR" => {
                    let cmp: Vec<char> = arg(0)?.as_char()?.chars().collect();
                    let base: Vec<char> = arg(1)?.as_char()?.chars().collect();
                    let start = match args.get(2) {
                        Some(_) => arg(2)?.as_dec()?.to_usize().unwrap_or(0),
                        None if name == "%CHECK" => 1,
                        None => base.len(),
                    };
                    if start == 0 || start > base.len() {
                        bail!(
                            "CPD0012: {name} start {start} is not valid for a string of length {}.",
                            base.len()
                        );
                    }
                    let found = if name == "%CHECK" {
                        (start - 1..base.len()).find(|&i| !cmp.contains(&base[i]))
                    } else {
                        (0..start).rev().find(|&i| !cmp.contains(&base[i]))
                    };
                    Value::Dec(Decimal::from(found.map(|i| i + 1).unwrap_or(0) as u64))
                }
                "%UPPER" => Value::Char(arg(0)?.as_char()?.to_uppercase()),
                "%LOWER" => Value::Char(arg(0)?.as_char()?.to_lowercase()),
                // %LEN: a character variable's length, a decimal one's
                // digits; %SIZE: the bytes it takes.
                "%LEN" | "%LENGTH" | "%SIZE" => {
                    let n = match args.first() {
                        Some(Expr::Var(v)) => {
                            let var = vars
                                .get(v)
                                .ok_or_else(|| anyhow!("CPD0727: Variable {v} is not declared."))?;
                            if name == "%SIZE" {
                                storage_size(var)
                            } else {
                                var.len as usize
                            }
                        }
                        Some(_) => arg(0)?.as_char()?.chars().count(),
                        None => bail!("{name} needs a variable"),
                    };
                    Value::Dec(Decimal::from(n as u64))
                }
                // %PARMS(): how many parameters were passed.
                "%PARMS" => vars
                    .get(PARMS_VAR)
                    .map(|v| v.value.clone())
                    .unwrap_or(Value::Dec(Decimal::ZERO)),
                // %BIN(&var [start length]): the bytes of a character
                // variable as a binary integer (2 or 4 bytes).
                "%BIN" | "%BINARY" => {
                    let Some(Expr::Var(v)) = args.first() else {
                        bail!("{name} needs a character variable");
                    };
                    let var = vars
                        .get(v)
                        .ok_or_else(|| anyhow!("CPD0727: Variable {v} is not declared."))?;
                    let bytes = to_bytes(var)?;
                    let (start, len) = match (args.get(1), args.get(2)) {
                        (Some(_), Some(_)) => (
                            arg(1)?.as_dec()?.to_usize().unwrap_or(0),
                            arg(2)?.as_dec()?.to_usize().unwrap_or(0),
                        ),
                        _ => (1, bytes.len()),
                    };
                    if start == 0 || !matches!(len, 2 | 4) || start + len - 1 > bytes.len() {
                        bail!("CPD0012: %BIN({v} {start} {len}) is not valid: the length is 2 or 4 inside the variable.");
                    }
                    let b = &bytes[start - 1..start - 1 + len];
                    let n: i64 = if len == 2 {
                        i16::from_be_bytes([b[0], b[1]]) as i64
                    } else {
                        i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
                    };
                    Value::Dec(Decimal::from(n))
                }
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
                // Number to character (CL programming, "Changing the
                // value of a variable"): right-aligned and padded with
                // leading zeros, its decimal point kept, '-' leftmost
                // when negative (123 of a *DEC (5 2) in *CHAR 6 is
                // '123.00').
                Value::Dec(d) => {
                    let neg = d.is_sign_negative() && !d.is_zero();
                    let t = d.abs().to_string();
                    let n = var.len as usize;
                    let room = n.saturating_sub(usize::from(neg));
                    let t = if t.len() < room {
                        format!("{}{t}", "0".repeat(room - t.len()))
                    } else {
                        t
                    };
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
        VarType::Ptr => match v {
            Value::Ptr(p) => Value::Ptr(p),
            other => bail!(
                "only a pointer can be stored in {name}, not {}",
                other.describe()
            ),
        },
        VarType::Dec | VarType::Int => {
            let d = match v {
                Value::Dec(d) => d,
                Value::Char(s) => Decimal::from_str(s.trim()).map_err(|_| {
                    anyhow!("MCH1202: '{}' is not a valid number for {name}.", s.trim())
                })?,
                Value::Lgl(_) => bail!("a logical value cannot be stored in {name}"),
                Value::Ptr(_) => bail!("a pointer cannot be stored in {name}"),
            };
            let mut d = d.trunc_with_scale(var.decimals);
            if var.ty == VarType::Dec {
                d.rescale(var.decimals);
            }
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
    /// CHGVAR %SST(&var start length) value: part of a variable.
    ChgSst(String, Expr, Expr, Expr),
    /// CHGVAR %OFFSET(&ptr) value: where a pointer points in its storage.
    ChgOfs(String, Expr),
    /// CHGVAR %BIN(&var start length) value: a binary integer in part of a
    /// character variable.
    ChgBin(String, Option<(Expr, Expr)>, Expr),
    JumpIfFalse(Expr, usize),
    Jump(usize),
    /// Jump to a label (resolved when the program is checked).
    Goto(String),
    Return,
    /// End of a program-level MONMSG's EXEC: go on after the command that
    /// failed.
    Resume,
    /// CALLSUBR: the subroutine, and the *INT variable for its RTNVAL.
    CallSubr(String, Option<String>),
    /// RTNSUBR or ENDSUBR: back to after the CALLSUBR, with RTNVAL.
    RtnSubr(Option<Expr>),
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
    /// MONMSGs after CHGVAR: place -> (IDs, handler).
    guarded: HashMap<usize, Vec<(Vec<String>, usize)>>,
    /// An ILE CL program bound to modules and service programs (CRTPGM):
    /// how, its object (LIB/NAME) and its activation group — for
    /// CALLPRC.
    pub binding: Option<crate::ile::Binding>,
    pub object: String,
    pub actgrp: String,
    /// The files DCLF declares, in order.
    pub files: Vec<DeclaredFile>,
}

impl Program {
    /// The procedures the program calls with CALLPRC (named by a literal).
    pub fn called_procedures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for ins in &self.code {
            if let Ins::Cmd { st, .. } = ins {
                if st.name == "CALLPRC" {
                    if let Ok(p) = named(st, &["PRC", "PARM", "RTNVAL"]) {
                        if let Some(prc) = p.get("PRC") {
                            out.push(prc.trim().trim_matches('\'').to_ascii_uppercase());
                        }
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
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
    match crate::jobs::escape_id(&s) {
        "" => "CPF9898".into(),
        id => id.to_string(),
    }
}

struct Compiler<'a> {
    sts: &'a [Statement],
    i: usize,
    code: Vec<Ins>,
    labels: HashMap<String, usize>,
    /// Open loops, innermost last.
    loops: Vec<Loop>,
    /// MONMSGs of instructions that are not commands: place -> (IDs,
    /// handler).
    guarded: HashMap<usize, Vec<(Vec<String>, usize)>>,
    /// Compiling the command of an EXEC.
    in_exec: bool,
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
            // Commands in the DO group have MONMSGs of their own.
            let was = std::mem::replace(&mut self.in_exec, false);
            let r = self.block(&["ENDDO"]);
            self.in_exec = was;
            match r? {
                Some(_) => Ok(()),
                None => Err(self.err(outer, "DO has no ENDDO")),
            }
        } else if st.name.is_empty() {
            Ok(())
        } else {
            self.statement(&st)
        }
    }

    /// MONMSG's EXEC: the MONMSGs that follow belong to the statement
    /// being monitored, not to the EXEC's command.
    fn exec(&mut self, m: &Statement, x: &str) -> Result<()> {
        let was = std::mem::replace(&mut self.in_exec, true);
        let r = self.nested(m, x);
        self.in_exec = was;
        r
    }

    /// CHGVAR: one instruction.
    fn chgvar(&mut self, st: &Statement) -> Result<()> {
        let p = named(st, &["VAR", "VALUE"])?;
        let var = p
            .get("VAR")
            .ok_or_else(|| self.err(st, "VAR is required"))?;
        let value = p
            .get("VALUE")
            .ok_or_else(|| self.err(st, "VALUE is required"))?;
        let target = var.trim().to_ascii_uppercase();
        let e = parse_expr(value).map_err(|e| self.err(st, e))?;
        // %SST(&var start length) and %OFFSET(&ptr) as targets
        // (CL reference, CHGVAR).
        if target.starts_with('%') {
            match parse_expr(&target).map_err(|e| self.err(st, e))? {
                Expr::Builtin(f, a) if (f == "%SST" || f == "%SUBSTRING") && a.len() == 3 => {
                    let Expr::Var(v) = &a[0] else {
                        return Err(self.err(st, "%SST needs a variable here"));
                    };
                    self.code
                        .push(Ins::ChgSst(v.clone(), a[1].clone(), a[2].clone(), e));
                }
                Expr::Builtin(f, a)
                    if (f == "%BIN" || f == "%BINARY") && (a.len() == 1 || a.len() == 3) =>
                {
                    let Expr::Var(v) = &a[0] else {
                        return Err(self.err(st, "%BIN needs a variable"));
                    };
                    let part = (a.len() == 3).then(|| (a[1].clone(), a[2].clone()));
                    self.code.push(Ins::ChgBin(v.clone(), part, e));
                }
                Expr::Builtin(f, a) if (f == "%OFFSET" || f == "%OFS") && a.len() == 1 => {
                    let Expr::Var(v) = &a[0] else {
                        return Err(self.err(st, "%OFFSET needs a variable"));
                    };
                    self.code.push(Ins::ChgOfs(v.clone(), e));
                }
                _ => return Err(self.err(st, format!("{var} is not a variable"))),
            }
            return Ok(());
        }
        if !target.starts_with('&') {
            return Err(self.err(st, format!("{var} is not a variable")));
        }
        self.code.push(Ins::ChgVar(target, e));
        Ok(())
    }

    /// MONMSGs after a statement that is not run as a command (CHGVAR):
    /// kept by the instruction's place (CL reference, MONMSG: it can
    /// follow any command).
    fn guard(&mut self, at: usize) -> Result<()> {
        if self.in_exec {
            return Ok(());
        }
        let mut skips = Vec::new();
        let mut monitors = Vec::new();
        while self.sts.get(self.i).is_some_and(|n| n.name == "MONMSG") {
            let m = self.sts[self.i].clone();
            self.i += 1;
            if skips.is_empty() {
                skips.push(self.here());
                self.code.push(Ins::Jump(0));
            }
            let (ids, exec) = monmsg(&m)?;
            monitors.push((ids, self.here()));
            if let Some(x) = exec {
                self.exec(&m, &x)?;
            }
            skips.push(self.here());
            self.code.push(Ins::Jump(0));
        }
        let end = self.here();
        for s in skips {
            self.patch(s, end);
        }
        if !monitors.is_empty() {
            self.guarded.insert(at, monitors);
        }
        Ok(())
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
                let at = self.here();
                self.chgvar(st)?;
                self.guard(at)?;
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
            "CALLSUBR" => {
                let p = named(st, &["SUBR", "RTNVAL"])?;
                let n = p
                    .get("SUBR")
                    .ok_or_else(|| self.err(st, "CALLSUBR needs SUBR"))?
                    .trim()
                    .to_ascii_uppercase();
                let r = p.get("RTNVAL").map(|v| v.trim().to_ascii_uppercase());
                self.code.push(Ins::CallSubr(n, r));
            }
            "RTNSUBR" => {
                let p = named(st, &["RTNVAL"])?;
                let r = p.get("RTNVAL").map(|v| parse_expr(v)).transpose()?;
                self.code.push(Ins::RtnSubr(r));
            }
            "SUBR" | "ENDSUBR" => {
                return Err(self.err(
                    st,
                    "subroutines go after the program's statements and cannot be nested",
                ))
            }
            "MONMSG" => return Err(self.err(st, "must follow a command (or the declarations)")),
            _ => {
                // A command, with its MONMSGs.
                let at = self.here();
                self.code.push(Ins::Cmd {
                    st: st.clone(),
                    monitors: vec![],
                });
                let mut skips = Vec::new();
                while !self.in_exec && self.sts.get(self.i).is_some_and(|n| n.name == "MONMSG") {
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
                        self.exec(&m, &x)?;
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
    // Positional order VAR TYPE LEN VALUE; STG, BASPTR, DEFVAR and ADDRESS
    // by keyword in practice.
    let p = named(
        st,
        &[
            "VAR", "TYPE", "LEN", "VALUE", "STG", "BASPTR", "DEFVAR", "ADDRESS",
        ],
    )?;
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
        Some("*PTR") => VarType::Ptr,
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
        // A pointer: 16 bytes.
        VarType::Ptr => (16, 0),
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
    // Storage (CL reference, DCL): *AUTO (its own), *DEFINED (part of
    // DEFVAR's variable, from a start position, 1 by default) or *BASED
    // (where BASPTR's pointer points).
    let stg = p
        .get("STG")
        .map(|t| t.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "*AUTO".into());
    let (defined, based) = match stg.as_str() {
        "*AUTO" => (None, None),
        "*DEFINED" => {
            let d = p
                .get("DEFVAR")
                .ok_or_else(|| anyhow!("line {}: STG(*DEFINED) needs DEFVAR", st.line))?;
            let mut w = d.split_whitespace();
            let base = w.next().unwrap_or("").to_ascii_uppercase();
            let start: usize = match w.next() {
                Some(n) => n.parse().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    anyhow!("line {}: DEFVAR({d}): the start is not valid", st.line)
                })?,
                None => 1,
            };
            (Some((base, start)), None)
        }
        "*BASED" => {
            let b = p
                .get("BASPTR")
                .ok_or_else(|| anyhow!("line {}: STG(*BASED) needs BASPTR", st.line))?;
            (None, Some(b.trim().to_ascii_uppercase()))
        }
        other => bail!("line {}: STG({other}) is not valid", st.line),
    };
    if (defined.is_some() || based.is_some()) && p.contains_key("VALUE") {
        bail!("line {}: VALUE cannot be given with STG({stg})", st.line);
    }
    let mut var = Var {
        ty,
        len: digits,
        decimals,
        value: match ty {
            VarType::Char => Value::Char(" ".repeat(len as usize)),
            VarType::Lgl => Value::Lgl(false),
            VarType::Ptr => Value::Ptr(None),
            _ => Value::Dec(Decimal::ZERO),
        },
        defined,
        based,
        shadow: None,
    };
    // ADDRESS(&var): a pointer's first value.
    if let Some(a) = p.get("ADDRESS") {
        if ty != VarType::Ptr {
            bail!("line {}: ADDRESS is only for TYPE(*PTR)", st.line);
        }
        let mut w = a.split_whitespace();
        let target = w.next().unwrap_or("").to_ascii_uppercase();
        let off: usize = w.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        var.value = Value::Ptr(Some((target, off)));
    }
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

/// The hidden variable holding how many parameters were passed (%PARMS).
const PARMS_VAR: &str = "%PARMS";
/// The parameters the caller did not pass, as " &A &B ".
const UNPASSED_VAR: &str = "\0UNPASSED";

/// A parameter the caller did not pass has no storage: using it is
/// MCH3601 (CL reference, PGM: a program can check %PARMS, or monitor
/// for MCH3601, before it uses an optional parameter).
fn not_passed(name: &str, vars: &HashMap<String, Var>) -> Result<()> {
    if let Some(Var {
        value: Value::Char(list),
        ..
    }) = vars.get(UNPASSED_VAR)
    {
        if list.contains(&format!(" {name} ")) {
            bail!("MCH3601: Parameter {name} was not passed.");
        }
    }
    Ok(())
}

/// How many bytes a variable takes: *CHAR its length, *LGL 1, *DEC packed
/// (digits / 2 + 1), *INT its length, *PTR 16.
fn storage_size(v: &Var) -> usize {
    match v.ty {
        VarType::Char => v.len as usize,
        VarType::Lgl => 1,
        VarType::Dec => v.len as usize / 2 + 1,
        VarType::Int => match v.len {
            5 => 2,
            19 => 8,
            _ => 4,
        },
        VarType::Ptr => 16,
    }
}

/// A variable's value from its bytes (EBCDIC as the system keeps them).
fn from_bytes(v: &Var, b: &[u8]) -> Result<Value> {
    use crate::rpg::data::{self, DType};
    Ok(match v.ty {
        VarType::Char => Value::Char(data::a(b)),
        VarType::Lgl => Value::Lgl(data::a(b) == "1"),
        VarType::Dec => {
            let n = data::load(&DType::Packed(v.len, v.decimals), b)?.num()?;
            Value::Dec(Decimal::from_str(&n.to_string()).map_err(|e| anyhow!("{e}"))?)
        }
        VarType::Int => {
            let n: i64 = match b.len() {
                2 => i16::from_be_bytes([b[0], b[1]]) as i64,
                8 => i64::from_be_bytes(b.try_into().unwrap_or([0; 8])),
                _ => i32::from_be_bytes(b.try_into().unwrap_or([0; 4])) as i64,
            };
            Value::Dec(Decimal::from(n))
        }
        VarType::Ptr => v.value.clone(),
    })
}

/// A variable's bytes.
fn to_bytes(v: &Var) -> Result<Vec<u8>> {
    use crate::rpg::data::{self, DType, Fit};
    let n = storage_size(v);
    Ok(match (&v.ty, &v.value) {
        (VarType::Char, val) => {
            let mut b = data::e(&val.as_char()?);
            b.resize(n, 0x40);
            b
        }
        (VarType::Lgl, val) => data::e(if val.as_lgl()? { "1" } else { "0" }),
        (VarType::Dec, val) => {
            let t = DType::Packed(v.len, v.decimals);
            let mut b = t.initial();
            let d = crate::rpg::dec::Dec::parse(&dec_text(&val.as_dec()?))?;
            data::store(
                &t,
                &data::Value::Num(d),
                &mut b,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
            b
        }
        (VarType::Int, val) => {
            let x = val.as_dec()?.trunc().to_i64().unwrap_or(0);
            match n {
                2 => (x as i16).to_be_bytes().to_vec(),
                8 => x.to_be_bytes().to_vec(),
                _ => (x as i32).to_be_bytes().to_vec(),
            }
        }
        (VarType::Ptr, _) => vec![0; 16],
    })
}

/// Variables whose storage is another's (*DEFINED, *BASED), after a step:
/// a value changed since it was taken from its storage is written there,
/// then each takes its value from its storage again.
fn sync_storage(vars: &mut HashMap<String, Var>, overlays: &[String]) -> Result<()> {
    // Where each lies: (variable with the storage, offset).
    let place = |vars: &HashMap<String, Var>, n: &str| -> Option<(String, usize)> {
        let v = vars.get(n)?;
        if let Some((b, s)) = &v.defined {
            return Some((b.clone(), s - 1));
        }
        match vars.get(v.based.as_ref()?).map(|p| &p.value) {
            Some(Value::Ptr(Some((t, o)))) => Some((t.clone(), *o)),
            _ => None,
        }
    };
    for n in overlays {
        let changed = vars.get(n).is_some_and(|v| {
            v.ty != VarType::Ptr && v.shadow.as_ref().is_some_and(|s| *s != v.value)
        });
        if !changed {
            continue;
        }
        let Some((base, off)) = place(vars, n) else {
            continue;
        };
        let bytes = to_bytes(&vars[n])?;
        if let Some(b) = vars.get_mut(&base) {
            let mut all = to_bytes(b)?;
            if off + bytes.len() > all.len() {
                bail!("MCH0603: {n} goes past the end of {base}.");
            }
            all[off..off + bytes.len()].copy_from_slice(&bytes);
            b.value = from_bytes(b, &all)?;
        }
    }
    for n in overlays {
        let Some((base, off)) = place(vars, n) else {
            continue;
        };
        let Some(b) = vars.get(&base) else { continue };
        let all = to_bytes(b)?;
        let v = &vars[n];
        let size = storage_size(v);
        if off + size > all.len() {
            continue;
        }
        // Bytes that are not a valid number stay as they are: the error
        // comes when the variable is used (decimal data error).
        let val = from_bytes(v, &all[off..off + size])
            .unwrap_or_else(|_| Value::Char(crate::rpg::data::a(&all[off..off + size])));
        let v = vars.get_mut(n).unwrap();
        v.value = val.clone();
        v.shadow = Some(val);
    }
    Ok(())
}

/// Check a CL program and turn it into steps (CRTBNDCL).
pub fn compile(name: &str, source: &str) -> Result<Program> {
    compile_with(name, source, &HashMap::new())
}

/// Compile a program in a job: the files its DCLF statements declare are
/// found through the library list first.
pub async fn compile_in(
    client: &tokio_postgres::Client,
    libl: &[String],
    name: &str,
    source: &str,
) -> Result<Program> {
    let files = resolve_files(client, libl, source).await?;
    compile_with(name, source, &files)
}

/// The FILE values of a program's DCLF statements, as written.
pub fn declared_file_names(source: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (line, text) in logical_lines(source)? {
        let st = parse_statement(line, &text)?;
        if st.name == "DCLF" {
            if let Some(f) = named(&st, DCLF_KEYWORDS)?.get("FILE") {
                out.push(f.trim().to_ascii_uppercase());
            }
        }
    }
    Ok(out)
}

const DCLF_KEYWORDS: &[&str] = &[
    "FILE",
    "RCDFMT",
    "OPNID",
    "ALWVARLEN",
    "ALWNULL",
    "ALWGRAPHIC",
    "DCLBINFLD",
];

/// Find the files a program declares: a database file (physical or
/// logical) or a display file.
pub async fn resolve_files(
    client: &tokio_postgres::Client,
    libl: &[String],
    source: &str,
) -> Result<HashMap<String, (String, ClFile)>> {
    let mut out = HashMap::new();
    for f in declared_file_names(source)? {
        let q = f.strip_prefix("*LIBL/").unwrap_or(&f).to_string();
        let found = match crate::catalog::resolve_file(client, libl, &q).await {
            Ok(r) => (r.library.clone(), ClFile::Db(r)),
            Err(_) => match crate::catalog::resolve_display_file(client, libl, &q).await {
                Ok((library, text)) => {
                    let name = q.rsplit('/').next().unwrap_or(&q).to_string();
                    let d = crate::dspf::parse(&name, &text)?;
                    (library, ClFile::Dsp(std::sync::Arc::new(d)))
                }
                Err(_) => continue,
            },
        };
        out.insert(f, found);
    }
    Ok(out)
}

/// The variables DCLF declares for a file: one per field of its record
/// formats (`&OPNID_FIELD` with an open identifier), numbers as *DEC, and
/// for a display file the indicators &IN01-&IN99 (*LGL).
fn file_variables(f: &DeclaredFile, formats: &[String]) -> Result<Vec<(String, Var)>> {
    let prefix = f
        .opnid
        .as_ref()
        .map(|o| format!("{o}_"))
        .unwrap_or_default();
    let mut out: Vec<(String, Var)> = Vec::new();
    let mut add = |n: &str, ty: VarType, len: u32, dec: u32| {
        let name = format!("&{prefix}{}", n.to_ascii_uppercase());
        if !out.iter().any(|(x, _)| *x == name) {
            out.push((name, new_var(ty, len, dec)));
        }
    };
    let wanted =
        |fmt: &str| formats.is_empty() || formats.iter().any(|w| w.eq_ignore_ascii_case(fmt));
    match &f.kind {
        ClFile::Db(r) => {
            if !wanted(&r.file.format) {
                bail!(
                    "DCLF {}: record format {} is not in the file",
                    f.name,
                    formats.join(" ")
                );
            }
            for fd in &r.file.fields {
                use crate::dds::DataType as T;
                match fd.data_type {
                    T::Packed | T::Zoned | T::Binary => {
                        add(&fd.name, VarType::Dec, fd.length, fd.decimals)
                    }
                    T::Float => bail!(
                        "DCLF {}: field {} is a floating point field, which CL cannot declare",
                        f.name,
                        fd.name
                    ),
                    _ => add(&fd.name, VarType::Char, fd.length.max(1), 0),
                }
            }
        }
        ClFile::Dsp(d) => {
            for rec in d.records.iter().filter(|r| wanted(&r.name)) {
                for fd in &rec.fields {
                    match fd.decimals {
                        Some(dec) => add(&fd.name, VarType::Dec, fd.length as u32, dec as u32),
                        None => add(&fd.name, VarType::Char, fd.length.max(1) as u32, 0),
                    }
                }
            }
            for n in 1..=99 {
                add(&format!("IN{n:02}"), VarType::Lgl, 1, 0);
            }
        }
    }
    Ok(out)
}

fn new_var(ty: VarType, len: u32, decimals: u32) -> Var {
    Var {
        ty,
        len,
        decimals,
        value: match ty {
            VarType::Char => Value::Char(" ".repeat(len as usize)),
            VarType::Lgl => Value::Lgl(false),
            VarType::Ptr => Value::Ptr(None),
            _ => Value::Dec(Decimal::ZERO),
        },
        defined: None,
        based: None,
        shadow: None,
    }
}

/// A file a CL program declares.
#[derive(Debug, Clone)]
pub enum ClFile {
    Db(crate::catalog::FileRef),
    Dsp(std::sync::Arc<crate::dspf::DisplayFile>),
}

#[derive(Debug, Clone)]
pub struct DeclaredFile {
    /// The file's name (without its library).
    pub name: String,
    pub library: String,
    /// OPNID: the prefix of its variables and the name RCVF, SNDF,
    /// SNDRCVF and CLOSE use for it.
    pub opnid: Option<String>,
    pub kind: ClFile,
}

/// Compile with the files DCLF names already found (`files`: by FILE
/// value as written).
pub fn compile_with(
    name: &str,
    source: &str,
    files: &HashMap<String, (String, ClFile)>,
) -> Result<Program> {
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
    // DCLPRCOPT (compile options) and COPYRIGHT (text kept in the
    // program) belong with the declarations; they change nothing here.
    let is_declare = |n: &str| matches!(n, "DCL" | "DCLF" | "DCLPRCOPT" | "COPYRIGHT");
    let mut declared_files: Vec<DeclaredFile> = Vec::new();
    let mut file_vars: Vec<(String, Var)> = Vec::new();
    while i < sts.len() && is_declare(&sts[i].name) {
        if sts[i].name == "DCLF" {
            let st = &sts[i];
            let p = named(st, DCLF_KEYWORDS)?;
            let file = p
                .get("FILE")
                .map(|f| f.trim().to_ascii_uppercase())
                .ok_or_else(|| anyhow!("line {}: DCLF needs FILE", st.line))?;
            let (library, kind) = files.get(&file).cloned().ok_or_else(|| {
                anyhow!("line {}: DCLF FILE({file}): the file is not found (CL needs it when the program is created)", st.line)
            })?;
            let opnid = p
                .get("OPNID")
                .map(|o| o.trim().to_ascii_uppercase())
                .filter(|o| o != "*NONE");
            if declared_files.iter().any(|d| d.opnid == opnid) {
                bail!("line {}: a file with OPNID({}) is already declared; each DCLF needs its own OPNID", st.line, opnid.as_deref().unwrap_or("*NONE"));
            }
            if declared_files.len() == 5 {
                bail!("line {}: a CL program declares at most 5 files", st.line);
            }
            let formats: Vec<String> = p
                .get("RCDFMT")
                .map(|r| {
                    r.split_whitespace()
                        .map(|x| x.to_ascii_uppercase())
                        .filter(|x| x != "*ALL")
                        .collect()
                })
                .unwrap_or_default();
            let d = DeclaredFile {
                name: file.rsplit('/').next().unwrap_or(&file).to_string(),
                library,
                opnid,
                kind,
            };
            file_vars.extend(file_variables(&d, &formats)?);
            declared_files.push(d);
            i += 1;
            continue;
        }
        if sts[i].name != "DCL" {
            i += 1;
            continue;
        }
        let (n, v) = parse_dcl(&sts[i])?;
        if decls.iter().any(|(d, _): &(String, Var)| *d == n) {
            bail!("line {}: {n} is declared twice", sts[i].line);
        }
        decls.push((n, v));
        i += 1;
    }
    // The file's variables; a variable the program declares itself wins.
    for (n, v) in file_vars {
        if !decls.iter().any(|(d, _)| *d == n) {
            decls.push((n, v));
        }
    }
    for p in &params {
        if !decls.iter().any(|(d, _)| d == p) {
            bail!("parameter {p} is not declared");
        }
    }
    // A defined variable lies inside its base variable, which is *CHAR;
    // a based one's pointer is a pointer.
    for (n, v) in &decls {
        if let Some((base, start)) = &v.defined {
            let b = decls
                .iter()
                .find(|(d, _)| d == base)
                .map(|(_, b)| b)
                .ok_or_else(|| anyhow!("DEFVAR({base}) of {n}: {base} is not declared"))?;
            if b.ty != VarType::Char || b.defined.is_some() || b.based.is_some() {
                bail!("DEFVAR({base}) of {n}: the defining variable must be a *CHAR variable with its own storage");
            }
            if start - 1 + storage_size(v) > b.len as usize {
                bail!(
                    "DEFVAR({base} {start}) of {n}: it does not fit in {base} ({} bytes)",
                    b.len
                );
            }
        }
        if let Some(ptr) = &v.based {
            let ok = decls.iter().any(|(d, x)| d == ptr && x.ty == VarType::Ptr);
            if !ok {
                bail!("BASPTR({ptr}) of {n}: {ptr} is not a *PTR variable");
            }
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
        guarded: HashMap::new(),
        in_exec: false,
    };
    // The program, then its subroutines (SUBR ... ENDSUBR, after the
    // program's other statements).
    let mut end = c.block(&["ENDPGM", "SUBR"])?;
    c.code.push(Ins::Return);
    while end.as_deref() == Some("SUBR") {
        let st = c.sts[c.i - 1].clone();
        let p = named(&st, &["SUBR"])?;
        let name = p
            .get("SUBR")
            .ok_or_else(|| c.err(&st, "SUBR needs a name"))?
            .trim()
            .to_ascii_uppercase();
        if c.labels
            .insert(format!("\0SUBR:{name}"), c.here())
            .is_some()
        {
            return Err(c.err(&st, format!("subroutine {name} is defined twice")));
        }
        if c.block(&["ENDSUBR"])?.is_none() {
            return Err(c.err(&st, "SUBR has no ENDSUBR"));
        }
        let e = &c.sts[c.i - 1];
        let rtn = named(e, &["RTNVAL"])?
            .get("RTNVAL")
            .map(|v| parse_expr(v))
            .transpose()?;
        c.code.push(Ins::RtnSubr(rtn));
        end = match c.sts.get(c.i) {
            Some(n) if n.name == "SUBR" => {
                c.i += 1;
                Some("SUBR".into())
            }
            Some(n) if n.name == "ENDPGM" => {
                c.i += 1;
                Some("ENDPGM".into())
            }
            None => None,
            Some(n) => {
                return Err(c.err(n, "only subroutines (SUBR) can follow a subroutine"));
            }
        };
    }
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
        if let Ins::CallSubr(n, _) = ins {
            if !c.labels.contains_key(&format!("\0SUBR:{n}")) {
                bail!("CALLSUBR SUBR({n}): there is no subroutine {n}");
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
        guarded: c.guarded,
        binding: None,
        object: String::new(),
        actgrp: crate::ile::DEFAULT_GROUP.into(),
        files: declared_files,
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
    // The program's own message queue, for as long as it runs.
    job.calls.enter();
    let r = run_program(job, pgm, args).await;
    job.calls.leave();
    if let Ok((_, values)) = &r {
        job.calls.returned = Some(values.clone());
    }
    r.map(|(o, _)| o)
}

/// The parameters' values, for the caller.
fn param_values(pgm: &Program, vars: &HashMap<String, Var>) -> Vec<String> {
    pgm.params
        .iter()
        .map(|p| vars.get(p).map(returned_text).unwrap_or_default())
        .collect()
}

/// A parameter's value as the caller gets it back.
fn returned_text(v: &Var) -> String {
    match &v.value {
        Value::Dec(d) => dec_text(d),
        Value::Lgl(b) => if *b { "1" } else { "0" }.into(),
        Value::Char(c) => c.clone(),
        Value::Ptr(_) => String::new(),
    }
}

async fn run_program(
    job: &mut Job<'_>,
    pgm: &Program,
    args: &[String],
) -> Result<(Outcome, Vec<String>)> {
    let mut vars: HashMap<String, Var> = pgm.decls.iter().cloned().collect();
    if args.len() > pgm.params.len() {
        bail!(
            "CPD0172: Program {} takes {} parameters, {} were passed.",
            pgm.name,
            pgm.params.len(),
            args.len()
        );
    }
    vars.insert(
        PARMS_VAR.into(),
        Var {
            ty: VarType::Dec,
            len: 3,
            decimals: 0,
            value: Value::Dec(Decimal::from(args.len() as u64)),
            defined: None,
            based: None,
            shadow: None,
        },
    );
    if args.len() < pgm.params.len() {
        let list: String = pgm.params[args.len()..]
            .iter()
            .map(|p| format!(" {p}"))
            .collect();
        vars.insert(
            UNPASSED_VAR.into(),
            Var {
                ty: VarType::Char,
                len: 0,
                decimals: 0,
                value: Value::Char(format!("{list} ")),
                defined: None,
                based: None,
                shadow: None,
            },
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
    // Variables over others' storage (*DEFINED, *BASED), kept in step.
    let overlays: Vec<String> = pgm
        .decls
        .iter()
        .filter(|(_, v)| v.defined.is_some() || v.based.is_some())
        .map(|(n, _)| n.clone())
        .collect();
    let mut msgs: Vec<String> = Vec::new();
    let mut io = FileIo::default();
    let mut pc = 0usize;
    let mut steps = 0u64;
    let mut resume_at: Option<usize> = None;
    // CALLSUBR return points.
    let mut subrs: Vec<(usize, Option<String>)> = Vec::new();
    let fail = |pgm: &Program, st: &Statement, e: &anyhow::Error| -> anyhow::Error {
        anyhow!(
            "{e:#}\nError in CL program {} at line {} ({}).",
            pgm.name,
            st.line,
            st.name
        )
    };
    while pc < pgm.code.len() {
        if !overlays.is_empty() {
            sync_storage(&mut vars, &overlays)?;
        }
        steps += 1;
        if steps > MAX_STEPS {
            bail!(
                "CL program {} was ended after {MAX_STEPS} steps (a loop that does not end?).",
                pgm.name
            );
        }
        match &pgm.code[pc] {
            Ins::Return => return Ok((Outcome::Messages(msgs), param_values(pgm, &vars))),
            Ins::Jump(t) => pc = *t,
            Ins::Goto(l) => pc = pgm.labels[l],
            Ins::CallSubr(name, rtn) => {
                // CALLSUBR: at most 99 levels deep (DCLPRCOPT SUBRSTACK's
                // default, IBM CL reference).
                if subrs.len() >= 99 {
                    bail!(
                        "CL program {}: subroutines called more than 99 levels deep (at {name}).",
                        pgm.name
                    );
                }
                subrs.push((pc + 1, rtn.clone()));
                pc = pgm.labels[&format!("\0SUBR:{name}")];
            }
            Ins::RtnSubr(e) => {
                let Some((back, rtn)) = subrs.pop() else {
                    bail!(
                        "CL program {}: RTNSUBR or ENDSUBR outside a subroutine.",
                        pgm.name
                    );
                };
                if let Some(name) = rtn {
                    let v = match e {
                        Some(e) => eval(e, &vars)?,
                        None => Value::Dec(Decimal::ZERO),
                    };
                    let var = vars
                        .get_mut(&name)
                        .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
                    assign(&name, var, v)?;
                }
                pc = back;
            }
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
            Ins::ChgVar(..) | Ins::ChgSst(..) | Ins::ChgBin(..) | Ins::ChgOfs(..) => {
                match change(&pgm.code[pc], &mut vars) {
                    Ok(()) => pc += 1,
                    Err(e) => {
                        // MONMSG after the CHGVAR, else the program's.
                        let id = error_id(&e);
                        let hit = |m: &[(Vec<String>, usize)]| {
                            m.iter()
                                .find(|(ids, _)| ids.iter().any(|x| msgid_matches(x, &id)))
                                .map(|x| x.1)
                        };
                        let local = pgm.guarded.get(&pc).and_then(|m| hit(m));
                        let global = hit(&pgm.global);
                        if local.is_none() && global.is_none() {
                            return Err(e);
                        }
                        job.calls.put(crate::pgmmsg::from_error(&e, &id), false);
                        match local {
                            Some(start) => pc = start,
                            None => {
                                resume_at = Some(pc + 1);
                                pc = global.unwrap_or(pc + 1);
                            }
                        }
                    }
                }
            }
            Ins::Cmd { st, monitors } => {
                job.calls.escape = None;
                job.calls.escape_to_caller = false;
                let result = command(job, pgm, st, &mut vars, &mut msgs, &mut io).await;
                match result {
                    Ok(Some(o)) => return Ok((o, param_values(pgm, &vars))),
                    Ok(None) => pc += 1,
                    // An escape message sent to the caller ends this
                    // program; its own MONMSGs do not see it.
                    Err(e) if job.calls.escape_to_caller && st.name == "SNDPGMMSG" => {
                        job.calls.escape_to_caller = false;
                        let mut lines = msgs.clone();
                        lines.push(format!("{e:#}"));
                        bail!(lines.join("\n"));
                    }
                    Err(e) => {
                        let id = job
                            .calls
                            .escape
                            .as_ref()
                            .map(|m| m.msgid.clone())
                            .filter(|i| !i.is_empty())
                            .unwrap_or_else(|| error_id(&e));
                        let monitored = monitors
                            .iter()
                            .chain(pgm.global.iter())
                            .any(|(ids, _)| ids.iter().any(|m| msgid_matches(m, &id)));
                        if monitored {
                            // The handled exception goes on the program's
                            // queue, for RCVMSG MSGTYPE(*EXCP).
                            let m = job
                                .calls
                                .escape
                                .take()
                                .unwrap_or_else(|| crate::pgmmsg::from_error(&e, &id));
                            job.calls.put(m, false);
                        }
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
    Ok((Outcome::Messages(msgs), param_values(pgm, &vars)))
}

/// CHGVAR (its instruction forms): the variable changes or the error.
fn change(ins: &Ins, vars: &mut HashMap<String, Var>) -> Result<()> {
    if let Ins::ChgVar(n, _) | Ins::ChgSst(n, ..) | Ins::ChgBin(n, ..) | Ins::ChgOfs(n, _) = ins {
        not_passed(n, vars)?;
    }
    match ins {
        Ins::ChgVar(name, e) => {
            let v = eval(e, vars)?;
            let var = vars
                .get_mut(name)
                .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
            assign(name, var, v)?;
            Ok(())
        }
        Ins::ChgSst(name, start, len, e) => {
            let v = eval(e, vars)?.as_char()?;
            let start = eval(start, vars)?.as_dec()?.to_usize().unwrap_or(0);
            let len = eval(len, vars)?.as_dec()?.to_usize().unwrap_or(0);
            let var = vars
                .get_mut(name)
                .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
            let mut cs: Vec<char> = var.value.as_char()?.chars().collect();
            if start == 0 || len == 0 || start + len - 1 > cs.len() {
                bail!(
                    "CPD0012: %SST({start} {len}) is outside {name} (length {}).",
                    cs.len()
                );
            }
            let piece: Vec<char> = format!("{v:<len$}").chars().take(len).collect();
            cs.splice(start - 1..start - 1 + len, piece);
            let text: String = cs.into_iter().collect();
            assign(name, var, Value::Char(text))?;
            Ok(())
        }
        Ins::ChgBin(name, part, e) => {
            let n = eval(e, vars)?.as_dec()?.trunc().to_i64().unwrap_or(0);
            let (start, len) = match part {
                Some((a, b)) => (
                    eval(a, vars)?.as_dec()?.to_usize().unwrap_or(0),
                    eval(b, vars)?.as_dec()?.to_usize().unwrap_or(0),
                ),
                None => (1, vars.get(name).map(storage_size).unwrap_or(0)),
            };
            let var = vars
                .get_mut(name)
                .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
            let mut bytes = to_bytes(var)?;
            if start == 0 || !matches!(len, 2 | 4) || start + len - 1 > bytes.len() {
                bail!("CPD0012: %BIN({name} {start} {len}) is not valid: the length is 2 or 4 inside the variable.");
            }
            let b: Vec<u8> = if len == 2 {
                (n as i16).to_be_bytes().to_vec()
            } else {
                (n as i32).to_be_bytes().to_vec()
            };
            bytes[start - 1..start - 1 + len].copy_from_slice(&b);
            let v = from_bytes(var, &bytes)?;
            var.value = v;
            Ok(())
        }
        Ins::ChgOfs(name, e) => {
            let off = eval(e, vars)?.as_dec()?.to_usize().unwrap_or(0);
            let var = vars
                .get_mut(name)
                .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
            match &mut var.value {
                Value::Ptr(Some((_, o))) => *o = off,
                _ => bail!("MCH3601: Pointer {name} is not set."),
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The files of one program call: database files open for RCVF, the
/// display's records.
#[derive(Default)]
struct FileIo {
    open: HashMap<usize, crate::rla::OpenFile>,
    ws: crate::workstn::Workstation,
}

/// The declared file a file command names with OPNID (or the one
/// declared without an open identifier).
fn file_for<'p>(
    pgm: &'p Program,
    st: &Statement,
    opnid: Option<&String>,
) -> Result<(usize, &'p DeclaredFile)> {
    let want = opnid
        .map(|o| o.trim().to_ascii_uppercase())
        .filter(|o| o != "*NONE");
    pgm.files
        .iter()
        .enumerate()
        .find(|(_, f)| f.opnid == want)
        .ok_or_else(|| match &want {
            Some(o) => anyhow!(
                "line {}: {}: no file is declared with OPNID({o})",
                st.line,
                st.name
            ),
            None => anyhow!("line {}: {}: no file is declared (DCLF)", st.line, st.name),
        })
}

fn var_name(f: &DeclaredFile, field: &str) -> String {
    match &f.opnid {
        Some(o) => format!("&{o}_{}", field.to_ascii_uppercase()),
        None => format!("&{}", field.to_ascii_uppercase()),
    }
}

/// A record's values into the file's variables.
fn fill_vars(
    vars: &mut HashMap<String, Var>,
    f: &DeclaredFile,
    values: &BTreeMap<String, String>,
) -> Result<()> {
    for (k, v) in values {
        let name = var_name(f, k);
        let Some(var) = vars.get_mut(&name) else {
            continue;
        };
        let val = match var.ty {
            VarType::Dec | VarType::Int => {
                Value::Dec(Decimal::from_str(v.trim()).map_err(|_| {
                    anyhow!("MCH1202: '{}' is not a valid number for {name}.", v.trim())
                })?)
            }
            _ => Value::Char(v.clone()),
        };
        assign(&name, var, val)?;
    }
    Ok(())
}

/// The record format a display file command names (RCDFMT(*FILE): the
/// file's first record format that is not a subfile record).
fn display_format(
    d: &crate::dspf::DisplayFile,
    f: &DeclaredFile,
    rcdfmt: Option<&String>,
) -> Result<String> {
    match rcdfmt.map(|r| r.trim().to_ascii_uppercase()) {
        Some(r) if r != "*FILE" => Ok(r),
        _ => d
            .records
            .iter()
            .find(|r| !r.is_subfile())
            .map(|r| r.name.clone())
            .ok_or_else(|| anyhow!("display file {} has no record format", f.name)),
    }
}

/// The values and indicators a display record format shows, from the
/// file's variables.
fn display_values(
    vars: &HashMap<String, Var>,
    d: &crate::dspf::DisplayFile,
    f: &DeclaredFile,
    format: &str,
) -> Result<(BTreeMap<String, String>, [bool; 100])> {
    let rec = d.record(format).ok_or_else(|| {
        anyhow!(
            "CPF4131: Record format {format} is not in display file {}.",
            f.name
        )
    })?;
    let mut values = BTreeMap::new();
    for fd in &rec.fields {
        if let Some(var) = vars.get(&var_name(f, &fd.name)) {
            let text = match &var.value {
                Value::Dec(n) => {
                    let mut n = *n;
                    n.rescale(fd.decimals.unwrap_or(0) as u32);
                    n.to_string()
                }
                other => other.as_char()?,
            };
            values.insert(fd.name.clone(), text);
        }
    }
    let mut ind = [false; 100];
    for (n, slot) in ind.iter_mut().enumerate().skip(1) {
        if let Some(Value::Lgl(true)) = vars
            .get(&var_name(f, &format!("IN{n:02}")))
            .map(|v| &v.value)
        {
            *slot = true;
        }
    }
    Ok((values, ind))
}

/// Show a frame on the job's display and wait for the operator.
async fn interact(
    job: &Job<'_>,
    f: &DeclaredFile,
    frame: crate::workstn::Frame,
) -> Result<crate::dspf::Response> {
    let Some(display) = job.display.clone() else {
        bail!(
            "Display file {} cannot be used here: this job has no display (it is not interactive).",
            f.name
        );
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    display
        .send(crate::runtime::DisplayRequest { frame, reply: tx })
        .await
        .map_err(|_| anyhow!("the job's display is gone"))?;
    rx.await
        .map_err(|_| anyhow!("the job's display is gone"))?
        .map_err(|e| anyhow!(e))
}

/// What the operator entered into the file's variables and indicators.
fn take_response(
    vars: &mut HashMap<String, Var>,
    f: &DeclaredFile,
    resp: &crate::dspf::Response,
) -> Result<()> {
    fill_vars(vars, f, &resp.values)?;
    for (n, on) in &resp.indicators {
        let name = var_name(f, &format!("IN{n:02}"));
        if let Some(v) = vars.get_mut(&name) {
            v.value = Value::Lgl(*on);
        }
    }
    Ok(())
}

/// RCVF, SNDF, SNDRCVF and CLOSE.
async fn file_command(
    job: &mut Job<'_>,
    pgm: &Program,
    st: &Statement,
    vars: &mut HashMap<String, Var>,
    io: &mut FileIo,
) -> Result<()> {
    let p = named(st, &["DEV", "RCDFMT", "OPNID", "WAIT"])?;
    let (i, f) = file_for(pgm, st, p.get("OPNID"))?;
    match (&f.kind, st.name.as_str()) {
        (_, "CLOSE") => {
            // The next RCVF opens the file again, at its first record.
            io.open.remove(&i);
        }
        (ClFile::Db(r), "RCVF") => {
            if let std::collections::hash_map::Entry::Vacant(slot) = io.open.entry(i) {
                // The file is opened at the first RCVF: an override made
                // before then (OVRDBF TOFILE) names the file opened.
                let ovr =
                    crate::ovr::effective(&job.calls.overrides, crate::ovr::Kind::Db, &f.name)
                        .cloned();
                let fr = match ovr.as_ref().and_then(|o| o.tofile.clone()) {
                    Some(t) => {
                        let t = t.strip_prefix("*LIBL/").unwrap_or(&t).to_string();
                        let to = crate::catalog::resolve_file(job.client, &job.library_list(), &t)
                            .await?;
                        // The record format must be the one the program
                        // was created with, unless LVLCHK(*NO).
                        let shape = |p: &crate::dds::PhysicalFile| {
                            p.fields
                                .iter()
                                .map(|x| (x.name.clone(), x.data_type, x.length, x.decimals))
                                .collect::<Vec<_>>()
                        };
                        let same =
                            to.file.format == r.file.format && shape(&to.file) == shape(&r.file);
                        if !same && !ovr.as_ref().is_some_and(|o| o.no_lvlchk) {
                            bail!(
                                "CPF4131: Level check on file {} in library {} with member {}.",
                                to.file.name,
                                to.library,
                                to.file.name
                            );
                        }
                        to
                    }
                    None => r.clone(),
                };
                slot.insert(crate::rla::OpenFile::new(fr));
            }
            let of = io.open.get_mut(&i).expect("opened above");
            let got = of.read(job.client).await?;
            match got.record {
                Some(rec) if got.found => fill_vars(vars, f, &rec)?,
                _ => bail!(
                    "CPF0864: End of file detected for file {} in {}.",
                    f.name,
                    f.library
                ),
            }
        }
        (ClFile::Db(_), other) => bail!(
            "line {}: {other} is for display files; {} is a database file",
            st.line,
            f.name
        ),
        (ClFile::Dsp(d), cmd) => {
            let format = display_format(d, f, p.get("RCDFMT"))?;
            match cmd {
                "SNDF" => {
                    let (values, ind) = display_values(vars, d, f, &format)?;
                    io.ws.write(d, &format, values, ind)?;
                }
                "SNDRCVF" => {
                    let (values, ind) = display_values(vars, d, f, &format)?;
                    let frame = io.ws.frame(d, &format, values, ind)?;
                    let resp = interact(job, f, frame).await?;
                    io.ws.apply(d, &format, &resp);
                    take_response(vars, f, &resp)?;
                }
                _ => {
                    let (frame, format) = io.ws.read_frame(
                        d,
                        p.get("RCDFMT")
                            .filter(|r| !r.trim().eq_ignore_ascii_case("*FILE"))
                            .map(|r| r.trim()),
                    )?;
                    let resp = interact(job, f, frame).await?;
                    io.ws.apply(d, &format, &resp);
                    take_response(vars, f, &resp)?;
                }
            }
        }
    }
    Ok(())
}

/// The system values as4tainr has (RTVSYSVAL, QSYS2.SYSTEM_VALUE_INFO).
pub(crate) const SYSTEM_VALUE_NAMES: &[&str] = &[
    "QDATE",
    "QYEAR",
    "QMONTH",
    "QDAY",
    "QCENTURY",
    "QTIME",
    "QHOUR",
    "QMINUTE",
    "QSECOND",
    "QDAYOFWEEK",
    "QDATFMT",
    "QDATSEP",
    "QTIMSEP",
    "QDECFMT",
    "QCURSYM",
    "QLANGID",
    "QCNTRYID",
    "QCCSID",
    "QUTCOFFSET",
    "QTIMZON",
    "QSECURITY",
    "QSYSLIBL",
    "QUSRLIBL",
    "QMODEL",
    "QSRLNBR",
    "QMAXSIGN",
    "QPWDEXPITV",
];

/// What a system value is for (DSPSYSVAL), in our own words. None: not
/// a system value this system has.
pub(crate) fn system_value_description(name: &str) -> Option<&'static str> {
    Some(match name {
        "QDATE" => "Current date",
        "QYEAR" => "Year of the current date",
        "QMONTH" => "Month of the current date",
        "QDAY" => "Day of the current date",
        "QCENTURY" => "Century of the current date",
        "QTIME" => "Current time",
        "QHOUR" => "Hour of the current time",
        "QMINUTE" => "Minute of the current time",
        "QSECOND" => "Second of the current time",
        "QDAYOFWEEK" => "Day of the week",
        "QDATFMT" => "Order of year, month and day in dates",
        "QDATSEP" => "Character between date parts",
        "QTIMSEP" => "Character between time parts",
        "QDECFMT" => "Decimal point and zero suppression",
        "QCURSYM" => "Currency sign",
        "QLANGID" => "Language of the system",
        "QCNTRYID" => "Country or region of the system",
        "QCCSID" => "Coded character set of the system",
        "QUTCOFFSET" => "Hours and minutes from UTC",
        "QTIMZON" => "Time zone description",
        "QSECURITY" => "Security level",
        "QSYSLIBL" => "System part of the library list",
        "QUSRLIBL" => "User part of the library list",
        "QMODEL" => "System model",
        "QSRLNBR" => "System serial number",
        "QMAXSIGN" => "Sign-on attempts before the device or profile is disabled",
        "QPWDEXPITV" => "Days before a password must be changed",
        _ => return None,
    })
}

/// A system value as IBM ships it. This system's system values cannot be
/// changed yet (no CHGSYSVAL), so it is the current value.
pub(crate) fn shipped_system_value_text(name: &str) -> Option<Result<String, i64>> {
    system_value_text(name)
}

/// A system value as text, or as a number (QSYS2.SYSTEM_VALUE_INFO).
pub(crate) fn system_value_text(name: &str) -> Option<Result<String, i64>> {
    match system_value(name).ok()? {
        Value::Dec(d) => Some(Err(d.trunc().to_string().parse().unwrap_or(0))),
        other => other.as_char().ok().map(Ok),
    }
}

/// A system value (RTVSYSVAL): the values IBM ships, the date and time
/// from the clock.
fn system_value(name: &str) -> Result<Value> {
    let ((y, mo, d), (h, mi, sec), us) = crate::rpg::dates::now();
    let c = |s: String| Value::Char(s);
    // A list of libraries: ten characters each.
    let list = |libs: &[&str], len: usize| {
        let s: String = libs.iter().map(|l| format!("{l:<10}")).collect();
        format!("{s:<len$}")
    };
    Ok(match name {
        "QDATE" => c(format!("{mo:02}{d:02}{:02}", y % 100)),
        "QYEAR" => c(format!("{:02}", y % 100)),
        "QMONTH" => c(format!("{mo:02}")),
        "QDAY" => c(format!("{d:02}")),
        "QCENTURY" => c(if y >= 2000 { "1" } else { "0" }.into()),
        "QTIME" => c(format!("{h:02}{mi:02}{sec:02}{:03}", us / 1000)),
        "QHOUR" => c(format!("{h:02}")),
        "QMINUTE" => c(format!("{mi:02}")),
        "QSECOND" => c(format!("{sec:02}")),
        "QDAYOFWEEK" => {
            let n = crate::rpg::dates::days((y, mo, d));
            // 1970-01-01 was a Thursday.
            let names = ["*THU", "*FRI", "*SAT", "*SUN", "*MON", "*TUE", "*WED"];
            let base = crate::rpg::dates::days((1970, 1, 1));
            c(names[((n - base).rem_euclid(7)) as usize].into())
        }
        "QDATFMT" => c("MDY".into()),
        "QDATSEP" => c("/".into()),
        "QTIMSEP" => c(":".into()),
        "QDECFMT" => c(" ".into()),
        "QCURSYM" => c("$".into()),
        "QLANGID" => c("ENU".into()),
        "QCNTRYID" => c("US".into()),
        "QCCSID" => Value::Dec(Decimal::from(65535)),
        "QUTCOFFSET" => c("+0000".into()),
        "QTIMZON" => c("Q0000UTC".into()),
        "QSECURITY" => c("40".into()),
        "QSYSLIBL" => c(list(crate::commands::SYSTEM_LIBRARY_LIST, 150)),
        "QUSRLIBL" => c(list(crate::commands::USER_LIBRARY_LIST, 250)),
        "QMODEL" => c("0000".into()),
        "QSRLNBR" => c("00000000".into()),
        // Shipped values (DSPSYSVAL on IBM i 7.5 shows them).
        "QMAXSIGN" => c("3".into()),
        "QPWDEXPITV" => c("*NOMAX".into()),
        other => bail!("CPF1028: System value {other} not found."),
    })
}

/// Store a value in a return variable (`&NAME` as written)./// Store a value in a return variable (`&NAME` as written).
fn set_var(vars: &mut HashMap<String, Var>, target: &str, v: Value) -> Result<()> {
    let name = target.trim().to_ascii_uppercase();
    let var = vars
        .get_mut(&name)
        .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
    assign(&name, var, v)
}

/// Message data (SNDPGMMSG and RTVMSG MSGDTA): a variable's storage as it
/// is, or a character expression.
fn message_data(d: &str, vars: &HashMap<String, Var>) -> Result<Vec<u8>> {
    let name = d.to_ascii_uppercase();
    if let Some(v) = vars.get(&name) {
        return to_bytes(v);
    }
    Ok(crate::rpg::data::e(
        &eval(&parse_expr(d)?, vars)?.as_char()?,
    ))
}

/// Run one command of a program. Ok(Some(..)) ends the program (SIGNOFF).
async fn command(
    job: &mut Job<'_>,
    pgm: &Program,
    st: &Statement,
    vars: &mut HashMap<String, Var>,
    msgs: &mut Vec<String>,
    io: &mut FileIo,
) -> Result<Option<Outcome>> {
    match st.name.as_str() {
        "RCVF" | "SNDF" | "SNDRCVF" | "CLOSE" => {
            file_command(job, pgm, st, vars, io).await?;
            Ok(None)
        }
        "SNDPGMMSG" => {
            let p = named(
                st,
                &[
                    "MSG", "MSGID", "MSGF", "MSGDTA", "TOPGMQ", "TOMSGQ", "MSGTYPE", "RPYMSGQ",
                    "KEYVAR", "CCSID",
                ],
            )?;
            let text = |s: &str| -> Result<String> {
                Ok(eval(&parse_expr(s)?, vars)?
                    .as_char()?
                    .trim_end()
                    .to_string())
            };
            let kind = p
                .get("MSGTYPE")
                .map(|t| t.trim().to_ascii_uppercase())
                .unwrap_or_else(|| "*INFO".into());
            let mut m = match (p.get("MSG"), p.get("MSGID")) {
                (Some(t), _) => crate::pgmmsg::QMsg {
                    text: text(t)?,
                    ..Default::default()
                },
                (None, Some(id)) => {
                    let id = substitute_text(id, vars)?
                        .trim()
                        .trim_matches('\'')
                        .to_string();
                    let msgf = match p.get("MSGF") {
                        Some(f) => substitute_text(f, vars)?.replace(' ', ""),
                        None => bail!("line {}: SNDPGMMSG MSGID needs MSGF", st.line),
                    };
                    let data = match p.get("MSGDTA").map(|d| d.trim()) {
                        None => Vec::new(),
                        Some(d) if d.eq_ignore_ascii_case("*NONE") => Vec::new(),
                        Some(d) => message_data(d, vars)?,
                    };
                    crate::pgmmsg::describe(
                        job.client,
                        &job.library_list(),
                        &job.calls,
                        &id,
                        &msgf,
                        &data,
                    )
                    .await?
                }
                (None, None) => bail!("line {}: SNDPGMMSG needs MSG or MSGID", st.line),
            };
            m.ty = kind.clone();
            // TOPGMQ: (*PRV *) by default, (*SAME *), *EXT. TOMSGQ: a
            // user's message queue instead.
            let topgmq = p
                .get("TOPGMQ")
                .map(|t| t.trim().trim_start_matches('(').trim().to_ascii_uppercase())
                .unwrap_or_else(|| "*PRV".into());
            let to = topgmq
                .split_whitespace()
                .next()
                .unwrap_or("*PRV")
                .to_string();
            if let Some(q) = p.get("TOMSGQ") {
                let q = substitute_text(q, vars)?
                    .trim()
                    .trim_matches('\'')
                    .to_ascii_uppercase();
                if q != "*TOPGMQ" {
                    let queue = q.rsplit('/').next().unwrap_or(&q).to_string();
                    let from_job = job.id.as_ref().map(|i| i.to_string()).unwrap_or_default();
                    let line = if m.msgid.is_empty() {
                        m.text.clone()
                    } else {
                        format!("{}: {}", m.msgid, m.text)
                    };
                    let kind_q = if kind == "*INQ" { "*INQ" } else { "*INFO" };
                    jobs::send_message(job.client, &queue, &job.user, &from_job, kind_q, &line)
                        .await?;
                    return Ok(None);
                }
            }
            let key = match (kind.as_str(), to.as_str()) {
                ("*ESCAPE", _) => {
                    // Ends the program; the caller may monitor for it.
                    let id = if m.msgid.is_empty() {
                        "CPF9898".to_string()
                    } else {
                        m.msgid.clone()
                    };
                    let line = format!("{id}: {}", m.text);
                    if m.msgid.is_empty() {
                        m.msgid = id;
                        m.msgf = "QCPFMSG".into();
                        m.msgflib = "QSYS".into();
                        m.sev = 40;
                    }
                    job.calls.escape_to_caller = to != "*SAME";
                    job.calls.escape = Some(m);
                    bail!(line)
                }
                ("*INFO" | "*COMP" | "*DIAG" | "*NOTIFY" | "*STATUS" | "*RQS", "*SAME") => {
                    job.calls.put(m, false)
                }
                ("*STATUS", _) => {
                    msgs.push(m.text.clone());
                    0
                }
                ("*INFO" | "*COMP" | "*DIAG" | "*NOTIFY", "*EXT") => {
                    msgs.push(m.text.clone());
                    0
                }
                ("*INFO" | "*COMP" | "*DIAG" | "*NOTIFY", _) => {
                    msgs.push(m.text.clone());
                    job.calls.put(m, true)
                }
                (other, _) => bail!("line {}: MSGTYPE({other}) is not supported", st.line),
            };
            if let Some(k) = p.get("KEYVAR") {
                set_var(vars, k, Value::Char(crate::pgmmsg::key_text(key)))?;
            }
            Ok(None)
        }
        // RCVMSG: a message from the program's own queue into variables;
        // with no message, character variables are blank and numbers zero.
        "RCVMSG" => {
            let p = named(
                st,
                &[
                    "PGMQ",
                    "MSGQ",
                    "MSGTYPE",
                    "MSGKEY",
                    "WAIT",
                    "RMV",
                    "CCSID",
                    "KEYVAR",
                    "MSG",
                    "MSGLEN",
                    "SECLVL",
                    "SECLVLLEN",
                    "MSGDTA",
                    "MSGDTALEN",
                    "MSGID",
                    "SEV",
                    "SENDER",
                    "SENDERFMT",
                    "RTNTYPE",
                    "ALROPT",
                    "MSGF",
                    "MSGFLIB",
                    "SNDMSGFLIB",
                    "TEXTCCSID",
                    "MSGDTACCSID",
                ],
            )?;
            let up = |k: &str, d: &str| -> String {
                p.get(k)
                    .map(|v| {
                        v.trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .trim()
                            .to_ascii_uppercase()
                    })
                    .unwrap_or_else(|| d.to_string())
            };
            let pgmq = up("PGMQ", "*SAME");
            let levels = match pgmq.split_whitespace().next().unwrap_or("*SAME") {
                "*SAME" => 0,
                "*PRV" => 1,
                other => bail!(
                    "line {}: RCVMSG PGMQ({other}) is not supported yet: *SAME or *PRV",
                    st.line
                ),
            };
            let msgq = match p.get("MSGQ") {
                Some(q) => substitute_text(q, vars)?
                    .trim()
                    .trim_matches('\'')
                    .to_ascii_uppercase(),
                None => "*PGMQ".into(),
            };
            let msgtype = up("MSGTYPE", "*ANY");
            let key = match p.get("MSGKEY") {
                None => None,
                Some(k) => {
                    let v = eval(&parse_expr(k.trim())?, vars)?.as_char()?;
                    if v.trim().eq_ignore_ascii_case("*NONE") {
                        None
                    } else {
                        Some(crate::pgmmsg::parse_key(&v).unwrap_or(0))
                    }
                }
            };
            let rmv = up("RMV", "*YES");
            let got = if msgq == "*PGMQ" {
                job.calls.receive_from(levels, &msgtype, key, &rmv)
            } else {
                // A message queue (a user's, or QSYSOPR): WAIT seconds for
                // a message when there is none (*MAX: until one comes).
                let name = msgq.rsplit('/').next().unwrap_or(&msgq).to_string();
                if name != "QSYSOPR" && crate::users::get(job.client, &name).await?.is_none() {
                    bail!("CPF2403: Message queue {name} in *LIBL not found.");
                }
                let wait = match up("WAIT", "0").as_str() {
                    "*MAX" => u64::MAX,
                    w => w.parse::<u64>().unwrap_or(0),
                };
                let started = std::time::Instant::now();
                loop {
                    if let Some(msg) =
                        jobs::receive_message(job.client, &name, &msgtype, rmv != "*NO").await?
                    {
                        let text = match msg.text.split_once(": ") {
                            Some((id, t)) if id == msg.msgid => t.to_string(),
                            _ => msg.text.clone(),
                        };
                        break Some(crate::pgmmsg::QMsg {
                            key: msg.id as u32,
                            msgf: if msg.msgid.is_empty() {
                                String::new()
                            } else {
                                crate::msgf::system_msgf(&msg.msgid).into()
                            },
                            msgflib: if msg.msgid.is_empty() {
                                String::new()
                            } else {
                                "QSYS".into()
                            },
                            msgid: msg.msgid,
                            text,
                            ty: msg.kind,
                            ..Default::default()
                        });
                    }
                    if started.elapsed().as_secs() >= wait {
                        break None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            };
            let m = got.clone().unwrap_or_default();
            let found = got.is_some();
            let n = |x: usize| Value::Dec(Decimal::from(x as u64));
            let c = |s: &str| Value::Char(s.to_string());
            for (k, target) in &p {
                let v = match k.as_str() {
                    "KEYVAR" => c(&if found {
                        crate::pgmmsg::key_text(m.key)
                    } else {
                        String::new()
                    }),
                    "MSG" => c(&m.text),
                    "MSGLEN" => n(m.text.len()),
                    "SECLVL" => c(&m.seclvl),
                    "SECLVLLEN" => n(m.seclvl.len()),
                    "MSGDTA" => c(&crate::rpg::data::a(&m.data)),
                    "MSGDTALEN" => n(m.data.len()),
                    "MSGID" => c(&m.msgid),
                    "SEV" => Value::Dec(Decimal::from(m.sev)),
                    "RTNTYPE" => c(crate::pgmmsg::rtntype(&m.ty)),
                    "MSGF" => c(&m.msgf),
                    "MSGFLIB" => c(&m.msgflib),
                    "SNDMSGFLIB" => c(&m.sndmsgflib),
                    "SENDER" => c(""),
                    "ALROPT" => c(""),
                    "CCSID" | "TEXTCCSID" | "MSGDTACCSID" => {
                        Value::Dec(Decimal::from(if found { 37 } else { 0 }))
                    }
                    _ => continue,
                };
                set_var(vars, target, v)?;
            }
            Ok(None)
        }
        // RTVMSG: a message description's text, with data substituted.
        "RTVMSG" => {
            let p = named(
                st,
                &[
                    "MSGID",
                    "MSGF",
                    "MSGDTA",
                    "MSG",
                    "MSGLEN",
                    "SECLVL",
                    "SECLVLLEN",
                    "SEV",
                    "ALROPT",
                    "LOGPRB",
                    "CCSID",
                    "TEXTCCSID",
                ],
            )?;
            let id = substitute_text(
                p.get("MSGID")
                    .ok_or_else(|| anyhow!("line {}: RTVMSG needs MSGID", st.line))?,
                vars,
            )?
            .trim()
            .trim_matches('\'')
            .to_string();
            let msgf = substitute_text(
                p.get("MSGF")
                    .ok_or_else(|| anyhow!("line {}: RTVMSG needs MSGF", st.line))?,
                vars,
            )?
            .replace(' ', "");
            let data = match p.get("MSGDTA").map(|d| d.trim()) {
                None => Vec::new(),
                Some(d) if d.eq_ignore_ascii_case("*NONE") => Vec::new(),
                Some(d) => message_data(d, vars)?,
            };
            let m = crate::pgmmsg::describe(
                job.client,
                &job.library_list(),
                &job.calls,
                &id,
                &msgf,
                &data,
            )
            .await?;
            for (k, target) in &p {
                let v = match k.as_str() {
                    "MSG" => Value::Char(m.text.clone()),
                    "MSGLEN" => Value::Dec(Decimal::from(m.text.len() as u64)),
                    "SECLVL" => Value::Char(m.seclvl.clone()),
                    "SECLVLLEN" => Value::Dec(Decimal::from(m.seclvl.len() as u64)),
                    "SEV" => Value::Dec(Decimal::from(m.sev)),
                    "ALROPT" => Value::Char("*NO".into()),
                    "LOGPRB" => Value::Char("*NO".into()),
                    "CCSID" | "TEXTCCSID" => Value::Dec(Decimal::from(37)),
                    _ => continue,
                };
                set_var(vars, target, v)?;
            }
            Ok(None)
        }
        // RTVSYSVAL: system values as shipped (date and time ones from
        // the clock, in UTC).
        "RTVSYSVAL" => {
            let p = named(st, &["SYSVAL", "RTNVAR"])?;
            let name = p
                .get("SYSVAL")
                .ok_or_else(|| anyhow!("line {}: RTVSYSVAL needs SYSVAL", st.line))?
                .trim()
                .to_ascii_uppercase();
            let target = p
                .get("RTNVAR")
                .ok_or_else(|| anyhow!("line {}: RTVSYSVAL needs RTNVAR", st.line))?;
            let v = system_value(&name)?;
            set_var(vars, target, v)?;
            Ok(None)
        }
        // RTVOBJD: an object's description into variables (return
        // values not kept here are blank or zero).
        "RTVOBJD" => {
            let keys = [
                "OBJ",
                "OBJTYPE",
                "ASPDEV",
                "RTNLIB",
                "OBJATR",
                "USRDFNATR",
                "TEXT",
                "OWNER",
                "PGP",
                "PRMGRP",
                "ASP",
                "LIBASP",
                "OBJASPDEV",
                "OVFASP",
                "CRTDATE",
                "CHGDATE",
                "SAVDATE",
                "SAVACTDATE",
                "RSTDATE",
                "CRTUSER",
                "CRTSYSTEM",
                "OBJDMN",
                "USEDATE",
                "USEUPD",
                "NBRDAYSUSE",
                "RESETDATE",
                "SRCF",
                "SRCFLIB",
                "SRCMBR",
                "SRCDATE",
                "SAVCMD",
                "SAVVOL",
                "SAVDEV",
                "SAVF",
                "SAVFLIB",
                "SAVLABEL",
                "SAVSEQNBR",
                "SAVACT",
                "SYSLVL",
                "COMPILER",
                "OBJLVL",
                "USRCHG",
                "LICPGM",
                "PTF",
                "APAR",
                "SIZE",
                "STG",
                "COMPRESSED",
                "OBJOVFASP",
                "JRNSTS",
                "JRN",
                "JRNLIB",
                "JRNIMG",
                "JRNOMTE",
                "JRNSTRDATE",
                "ALWAPICHG",
                "APICHG",
            ];
            let p = named(st, &keys)?;
            let obj = substitute_text(
                p.get("OBJ")
                    .ok_or_else(|| anyhow!("line {}: RTVOBJD needs OBJ", st.line))?,
                vars,
            )?
            .trim_matches('\'')
            .to_ascii_uppercase();
            let objtype = substitute_text(
                p.get("OBJTYPE")
                    .ok_or_else(|| anyhow!("line {}: RTVOBJD needs OBJTYPE", st.line))?,
                vars,
            )?
            .trim()
            .to_ascii_uppercase();
            let (libs, name, label) = match obj.split_once('/') {
                Some(("*LIBL", n)) => (job.library_list(), n.to_string(), "*LIBL".to_string()),
                Some(("*CURLIB", n)) => {
                    (vec![job.current_library()], n.to_string(), "*CURLIB".into())
                }
                Some((l, n)) => (vec![l.to_string()], n.to_string(), l.to_string()),
                None => (job.library_list(), obj.clone(), "*LIBL".into()),
            };
            let libs = if objtype == "*LIB" {
                vec!["QSYS".to_string()]
            } else {
                libs
            };
            let mut row = None;
            for lib in &libs {
                if let Some(r) = job
                    .client
                    .query_opt(
                        "SELECT library, attribute, text, owner,
                                to_char(created AT TIME ZONE 'UTC', 'YYMMDDHH24MISS'),
                                extract(year FROM created)::int
                           FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                        &[lib, &name, &objtype],
                    )
                    .await?
                {
                    row = Some(r);
                    break;
                }
            }
            let Some(r) = row else {
                bail!("CPF9801: Object {name} was not found in library {label}.");
            };
            let lib: String = r.get(0);
            crate::authority::check(
                job.client,
                &job.user,
                &lib,
                &name,
                &objtype,
                crate::authority::Aut::Use,
            )
            .await?;
            // Dates: CYYMMDDHHMMSS (C: 0 for 19xx, 1 for 20xx).
            let year: i32 = r.get(5);
            let stamp = format!(
                "{}{}",
                if year >= 2000 { 1 } else { 0 },
                r.get::<_, String>(4)
            );
            let owner: String = r.get(3);
            for (k, target) in &p {
                let v = match k.as_str() {
                    "OBJ" | "OBJTYPE" | "ASPDEV" => continue,
                    "RTNLIB" => Value::Char(lib.clone()),
                    "OBJATR" => Value::Char(r.get(1)),
                    "TEXT" => Value::Char(r.get(2)),
                    "OWNER" | "CRTUSER" => Value::Char(owner.clone()),
                    "CRTDATE" | "CHGDATE" => Value::Char(stamp.clone()),
                    "OBJDMN" => Value::Char("*SYSTEM".into()),
                    "PGP" => Value::Char("*NONE".into()),
                    _ => {
                        let name = target.trim().to_ascii_uppercase();
                        match vars.get(&name).map(|v| v.ty) {
                            Some(VarType::Char) => Value::Char(String::new()),
                            Some(VarType::Lgl) => Value::Lgl(false),
                            _ => Value::Dec(Decimal::ZERO),
                        }
                    }
                };
                set_var(vars, target, v)?;
            }
            Ok(None)
        }
        // RTVMBRD: a database file member's description (a database file
        // has one member, named as the file; a source file one per source
        // member).
        "RTVMBRD" => {
            let p = named(
                st,
                &[
                    "FILE",
                    "MBR",
                    "RTNSYSNAME",
                    "RTNLIB",
                    "RTNMBR",
                    "FILEATR",
                    "FILETYPE",
                    "SRCTYPE",
                    "SRCF",
                    "SRCFLIB",
                    "SRCMBR",
                    "SRCCHGDATE",
                    "CRTDATE",
                    "CHGDATE",
                    "TEXT",
                    "NBRCURRCD",
                    "NBRDLTRCD",
                    "DTASPCSIZ",
                    "ACCPTHSIZ",
                    "NBRDTAMBRS",
                    "SHARE",
                    "EXPDATE",
                    "RESETDATE",
                    "SAVDATE",
                    "RSTDATE",
                    "USEDATE",
                    "USECOUNT",
                    "SQLFILETYPE",
                ],
            )?;
            let file = substitute_text(
                p.get("FILE")
                    .ok_or_else(|| anyhow!("line {}: RTVMBRD needs FILE", st.line))?,
                vars,
            )?
            .trim_matches('\'')
            .to_ascii_uppercase();
            let mbr_arg = p
                .get("MBR")
                .map(|m| substitute_text(m, vars))
                .transpose()?
                .map(|m| m.trim().trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| "*FIRST".into());
            let mbr_arg = mbr_arg
                .split_whitespace()
                .next()
                .unwrap_or("*FIRST")
                .to_string();
            let (libs, fname, label) = match file.split_once('/') {
                Some(("*LIBL", n)) => (job.library_list(), n.to_string(), "*LIBL".to_string()),
                Some(("*CURLIB", n)) => {
                    (vec![job.current_library()], n.to_string(), "*CURLIB".into())
                }
                Some((l, n)) => (vec![l.to_string()], n.to_string(), l.to_string()),
                None => (job.library_list(), file.clone(), "*LIBL".into()),
            };
            let mut found = None;
            for lib in &libs {
                if let Some(r) = job
                    .client
                    .query_opt(
                        "SELECT attribute, text, to_char(created AT TIME ZONE 'UTC', 'YYMMDDHH24MISS'),
                                extract(year FROM created)::int
                           FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                        &[lib, &fname],
                    )
                    .await?
                {
                    found = Some((lib.clone(), r));
                    break;
                }
            }
            let Some((lib, r)) = found else {
                bail!("CPF9812: File {fname} in library {label} not found.");
            };
            let attr: String = r.get(0);
            let cent = |y: i32| if y >= 2000 { 1 } else { 0 };
            let year: i32 = r.get(3);
            let mut stamp = format!("{}{}", cent(year), r.get::<_, String>(2));
            let mut text: String = r.get(1);
            let source = attr.ends_with("-SRC") || attr == "PF-SRC";
            let (member, srctype, count): (String, String, i64) = if source {
                let rows = job
                    .client
                    .query(
                        "SELECT member, srctype, text, source,
                                to_char(changed AT TIME ZONE 'UTC', 'YYMMDDHH24MISS'),
                                extract(year FROM changed)::int
                           FROM as4tainr.source_members WHERE library=$1 AND file=$2 ORDER BY member",
                        &[&lib, &fname],
                    )
                    .await?;
                let pick = match mbr_arg.as_str() {
                    "*FIRST" => rows.first(),
                    "*LAST" => rows.last(),
                    m => rows.iter().find(|r| r.get::<_, String>(0) == m),
                };
                let Some(m) = pick else {
                    bail!("CPF9815: Member {mbr_arg} of file {fname} in library {lib} does not exist.");
                };
                text = m.get(2);
                stamp = format!("{}{}", cent(m.get(5)), m.get::<_, String>(4));
                let lines = m.get::<_, String>(3).lines().count() as i64;
                (m.get(0), m.get(1), lines)
            } else {
                if !matches!(mbr_arg.as_str(), "*FIRST" | "*LAST") && mbr_arg != fname {
                    bail!("CPF9815: Member {mbr_arg} of file {fname} in library {lib} does not exist.");
                }
                let count = if attr == "PF" || attr == "LF" {
                    let fr = crate::catalog::resolve_file(
                        job.client,
                        std::slice::from_ref(&lib),
                        &fname,
                    )
                    .await?;
                    job.client
                        .query_one(&format!("SELECT count(*) FROM {}", fr.table()), &[])
                        .await?
                        .get(0)
                } else {
                    0
                };
                (fname.clone(), String::new(), count)
            };
            for (k, target) in &p {
                let v = match k.as_str() {
                    "FILE" | "MBR" => continue,
                    "RTNLIB" => Value::Char(lib.clone()),
                    "RTNSYSNAME" => Value::Char(String::new()),
                    "RTNMBR" => Value::Char(member.clone()),
                    "FILEATR" => Value::Char(if attr.starts_with("LF") {
                        "LF".into()
                    } else {
                        "PF".into()
                    }),
                    "FILETYPE" => Value::Char(if source {
                        "*SRC".into()
                    } else {
                        "*DATA".into()
                    }),
                    "SRCTYPE" => Value::Char(srctype.clone()),
                    "CRTDATE" | "CHGDATE" | "SRCCHGDATE" => Value::Char(stamp.clone()),
                    "TEXT" => Value::Char(text.clone()),
                    "NBRCURRCD" => Value::Dec(Decimal::from(count)),
                    "NBRDLTRCD" => Value::Dec(Decimal::ZERO),
                    "NBRDTAMBRS" => Value::Dec(Decimal::from(1)),
                    "SQLFILETYPE" => Value::Char(String::new()),
                    _ => {
                        let name = target.trim().to_ascii_uppercase();
                        match vars.get(&name).map(|v| v.ty) {
                            Some(VarType::Char) => Value::Char(String::new()),
                            Some(VarType::Lgl) => Value::Lgl(false),
                            _ => Value::Dec(Decimal::ZERO),
                        }
                    }
                };
                set_var(vars, target, v)?;
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
            let d = if qualified.eq_ignore_ascii_case("*LDA") {
                crate::dtaara::lda_area(job.client, job.id.as_ref().map(|j| j.number)).await?
            } else {
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
                d
            };
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
        // CALLPRC (ILE CL): a bound procedure, parameters by reference
        // (*BYVAL: by value), RTNVAL its return value.
        "CALLPRC" => {
            let p = named(st, &["PRC", "PARM", "RTNVAL"])?;
            let prc = p
                .get("PRC")
                .ok_or_else(|| anyhow!("line {}: CALLPRC needs PRC", st.line))?
                .trim()
                .trim_matches('\'')
                .to_ascii_uppercase();
            let binding = pgm.binding.clone().ok_or_else(|| {
                anyhow!("CPD5D02: Procedure {prc} is not bound to program {} (create it with CRTCLMOD and CRTPGM).", pgm.name)
            })?;
            let mut args = Vec::new();
            let mut targets = Vec::new();
            if let Some(parm) = p.get("PARM") {
                for tok in split_params(parm)? {
                    let text = tok
                        .value
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .to_string();
                    let mut words = text.split_whitespace();
                    let v = words.next().unwrap_or("").to_string();
                    let byval = words.any(|w| w.eq_ignore_ascii_case("*BYVAL"));
                    let val = eval(&parse_expr(&v)?, vars)?;
                    args.push(match val {
                        Value::Dec(d) => crate::rpg::data::Value::Num(crate::rpg::dec::Dec::parse(
                            &dec_text(&d),
                        )?),
                        Value::Lgl(b) => crate::rpg::data::Value::str(if b { "1" } else { "0" }),
                        Value::Char(s) => crate::rpg::data::Value::str(&s),
                        Value::Ptr(_) => bail!("pointers cannot be passed to procedures yet"),
                    });
                    targets.push((v.starts_with('&') && !byval).then(|| v.to_ascii_uppercase()));
                }
            }
            let job_name = job.id.as_ref().map(|i| i.to_string());
            let (back, ret, messages) = crate::rpg::call_procedure(
                &job.env.database_url,
                job.library_list(),
                job.user.clone(),
                job_name,
                pgm.object.clone(),
                binding,
                pgm.actgrp.clone(),
                prc,
                args,
            )
            .await?;
            msgs.extend(messages);
            let to_cl = |v: crate::rpg::data::Value, var: &Var| -> Result<Value> {
                Ok(match v {
                    crate::rpg::data::Value::Num(d) => {
                        Value::Dec(Decimal::from_str(&d.to_string()).map_err(|e| anyhow!("{e}"))?)
                    }
                    crate::rpg::data::Value::Ind(b) => Value::Lgl(b),
                    other => {
                        let s = crate::rpg::data::a(&other.chars()?);
                        if var.ty == VarType::Lgl {
                            Value::Lgl(s.starts_with('1'))
                        } else {
                            Value::Char(s)
                        }
                    }
                })
            };
            for (t, v) in targets.into_iter().zip(back) {
                if let Some(name) = t {
                    if let Some(var) = vars.get_mut(&name) {
                        let cv = to_cl(v, var)?;
                        assign(&name, var, cv)?;
                    }
                }
            }
            if let (Some(r), Some(v)) = (p.get("RTNVAL"), ret) {
                let name = r.trim().to_ascii_uppercase();
                let var = vars
                    .get_mut(&name)
                    .ok_or_else(|| anyhow!("CPD0727: Variable {name} is not declared."))?;
                let cv = to_cl(v, var)?;
                assign(&name, var, cv)?;
            }
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
            let mut targets: Vec<Option<String>> = Vec::new();
            if let Some(parm) = p.get("PARM") {
                for tok in split_params(parm)? {
                    let v = tok.value;
                    let is_var = v.starts_with('&') && vars.contains_key(&v.to_ascii_uppercase());
                    targets.push(is_var.then(|| v.to_ascii_uppercase()));
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
            job.calls.returned = None;
            let o = job.call_program_boxed(&pgm, args).await?;
            // Parameters are passed by reference: a variable gets back
            // the value the called program left in it (a character
            // parameter shorter than the variable changes only its first
            // characters).
            if let Some(values) = job.calls.returned.take() {
                for (t, v) in targets.iter().zip(values) {
                    let Some(name) = t else { continue };
                    let Some(var) = vars.get_mut(name) else {
                        continue;
                    };
                    let new = match var.ty {
                        VarType::Char => {
                            let old: Vec<char> = var.value.as_char()?.chars().collect();
                            let mut cs: Vec<char> = v.chars().collect();
                            if cs.len() < old.len() {
                                cs.extend(&old[cs.len()..]);
                            }
                            Value::Char(cs.into_iter().collect())
                        }
                        VarType::Lgl => Value::Lgl(v.starts_with('1')),
                        VarType::Ptr => continue,
                        _ => match Decimal::from_str(v.trim()) {
                            Ok(d) => Value::Dec(d),
                            Err(_) => continue,
                        },
                    };
                    assign(name, var, new)?;
                }
            }
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
                Outcome::Messages(m) => {
                    // The command's messages go on the program's queue
                    // (completion messages, for RCVMSG).
                    for t in &m {
                        let (id, text) = match t.split_once(": ") {
                            Some((i, x)) if is_msgid(i) => (i.to_string(), x.to_string()),
                            _ => (String::new(), t.clone()),
                        };
                        job.calls.put(
                            crate::pgmmsg::QMsg {
                                msgf: if id.is_empty() {
                                    String::new()
                                } else {
                                    crate::msgf::system_msgf(&id).into()
                                },
                                msgflib: if id.is_empty() {
                                    String::new()
                                } else {
                                    "QSYS".into()
                                },
                                msgid: id,
                                text,
                                ty: "*COMP".into(),
                                ..Default::default()
                            },
                            false,
                        );
                    }
                    msgs.extend(m)
                }
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
    #[test]
    fn decimal_to_character_is_right_aligned_with_zeros() {
        // CL programming, "Changing the value of a variable": a decimal
        // variable of length 5 with 2 decimals holding 123, assigned to a
        // *CHAR 6, is '123.00'; unused positions are leading zeros.
        use super::*;
        let dec = |len, decimals| Var {
            ty: VarType::Dec,
            len,
            decimals,
            value: Value::Dec(Decimal::ZERO),
            defined: None,
            based: None,
            shadow: None,
        };
        let chr = |len| Var {
            ty: VarType::Char,
            decimals: 0,
            ..dec(len, 0)
        };
        let mut n = dec(5, 2);
        assign("&N", &mut n, Value::Dec(Decimal::from(123))).unwrap();
        let mut c = chr(6);
        assign("&C", &mut c, n.value.clone()).unwrap();
        assert!(
            matches!(&c.value, Value::Char(s) if s == "123.00"),
            "{:?}",
            c.value
        );
        let mut c = chr(3);
        assign("&C", &mut c, Value::Dec(Decimal::from(42))).unwrap();
        assert!(
            matches!(&c.value, Value::Char(s) if s == "042"),
            "{:?}",
            c.value
        );
        let mut c = chr(5);
        assign("&C", &mut c, Value::Dec(Decimal::from(-7))).unwrap();
        assert!(
            matches!(&c.value, Value::Char(s) if s == "-0007"),
            "{:?}",
            c.value
        );
        // Observed on IBM i 7.5: DCL &D *DEC (7 2) VALUE(-12.5) assigned to
        // a *CHAR 10 is '-000012,50' with QDECFMT(J); with the shipped
        // QDECFMT (blank) the decimal point is a period.
        let mut d = dec(7, 2);
        assign("&D", &mut d, Value::Dec("-12.5".parse().unwrap())).unwrap();
        let mut c = chr(10);
        assign("&C", &mut c, d.value.clone()).unwrap();
        assert!(
            matches!(&c.value, Value::Char(s) if s == "-000012.50"),
            "{:?}",
            c.value
        );
    }

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

    // CL reference, built-in functions: %SCAN gives the position of the
    // search argument (0 when absent, from a start position); %CHECK the
    // first character not in the comparator, %CHECKR the last; %TRIM with
    // characters to trim; %UPPER, %LOWER; %LEN a variable's length (digits
    // for *DEC), %SIZE its bytes (a *DEC (7 2) is packed in 4); %BIN the
    // bytes of part of a character variable as a binary number.
    #[test]
    fn builtins() {
        let p = compile(
            "T",
            "PGM\nDCL &S *CHAR 10 VALUE('ab,cd,ef')\nDCL &D *DEC (7 2)\nDCL &B *CHAR 4 VALUE(X'0001FFFE')\nENDPGM",
        )
        .unwrap();
        let v = vars(&p);
        let n = |x: i64| Value::Dec(Decimal::from(x));
        assert_eq!(ev("%SCAN(',' &S)", &v), n(3));
        assert_eq!(ev("%SCAN(',' &S 4)", &v), n(6));
        assert_eq!(ev("%SCAN('x' &S)", &v), n(0));
        assert_eq!(ev("%CHECK('abc' &S)", &v), n(3));
        assert_eq!(ev("%CHECKR(' ' &S)", &v), n(8));
        assert_eq!(ev("%TRIM(&S 'abf ')", &v), Value::Char(",cd,e".into()));
        assert_eq!(ev("%UPPER(&S)", &v), Value::Char("AB,CD,EF  ".into()));
        assert_eq!(ev("%LEN(&S)", &v), n(10));
        assert_eq!(ev("%LEN(&D)", &v), n(7));
        assert_eq!(ev("%SIZE(&D)", &v), n(4));
        assert_eq!(ev("%BIN(&B 1 2)", &v), n(1));
        assert_eq!(ev("%BIN(&B 3 2)", &v), n(-2));
        assert_eq!(ev("%BIN(&B)", &v), n(0x0001FFFE));
    }

    // CL reference, hexadecimal constants: X'C1C2' is the characters
    // whose EBCDIC codes those are ("AB"); X'25' is the line feed.
    #[test]
    fn hexadecimal_constants() {
        let p = compile("T", "PGM\nDCL &A *CHAR 2 VALUE(X'C1C2')\nENDPGM").unwrap();
        let v = vars(&p);
        assert_eq!(ev("&A", &v), Value::Char("AB".into()));
        assert_eq!(ev("X'C1' *CAT 'b'", &v), Value::Char("Ab".into()));
        assert!(compile("T", "PGM\nDCL &A *CHAR 2 VALUE(X'C1C')\nENDPGM").is_err());
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
        // A special value in an expression is a character constant.
        assert_eq!(
            ev("'*YES' *EQ *YES *AND *NO *NE *YES", &v),
            Value::Lgl(true)
        );
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
            error_id(&anyhow!("CPF2110: Library X does not exist.")),
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
