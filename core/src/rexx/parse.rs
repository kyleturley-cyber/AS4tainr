//! REXX source: clauses, instructions and expressions.
//!
//! Written from IBM's REXX/400 Reference and the REXX language as Mike
//! Cowlishaw defined it (clauses end at a semicolon or the end of a line;
//! a comma at the end of a line continues the clause; comments nest;
//! labels are a symbol and a colon; THEN, ELSE, OTHERWISE and WHEN start
//! clauses of their own).

use anyhow::{anyhow, bail, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// A literal string (hexadecimal and binary strings already turned
    /// into their characters).
    Str(String),
    /// A symbol as written (upper case: REXX symbols are not case
    /// sensitive).
    Sym(String),
    Op(String),
    Comma,
    LParen,
    RParen,
    Colon,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// A blank (or comment) came before it in the clause.
    pub space: bool,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub struct Clause {
    pub line: usize,
    pub toks: Vec<Token>,
}

fn is_sym_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._!?@#$".contains(c)
}

/// The characters of a hexadecimal or binary string: EBCDIC bytes as
/// text (REXX/400 strings are EBCDIC).
fn string_bytes(s: &str, kind: char, line: usize) -> Result<String> {
    let digits: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = if kind == 'X' {
        let d = if digits.len() % 2 == 1 {
            format!("0{digits}")
        } else {
            digits
        };
        (0..d.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&d[i..i + 2], 16))
            .collect::<std::result::Result<Vec<u8>, _>>()
            .map_err(|_| anyhow!("REX0015: line {line}: '{s}'X is not a hexadecimal string."))?
    } else {
        if !digits.chars().all(|c| c == '0' || c == '1') {
            bail!("REX0015: line {line}: '{s}'B is not a binary string.");
        }
        let pad = (8 - digits.len() % 8) % 8;
        let d = format!("{}{digits}", "0".repeat(pad));
        (0..d.len())
            .step_by(8)
            .map(|i| u8::from_str_radix(&d[i..i + 8], 2).unwrap_or(0))
            .collect()
    };
    Ok(crate::rpg::data::a(&bytes))
}

/// The source as clauses.
pub fn clauses(src: &str) -> Result<Vec<Clause>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut cur: Vec<Token> = Vec::new();
    let mut cur_line = 1;
    let mut line = 1;
    let mut i = 0;
    let mut space = false;
    let flush = |cur: &mut Vec<Token>, out: &mut Vec<Clause>, l: usize| {
        if !cur.is_empty() {
            out.push(Clause {
                line: l,
                toks: std::mem::take(cur),
            });
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            // A comma last on the line continues the clause.
            if let Some(last) = cur.last() {
                if last.tok == Tok::Comma && last.line == line {
                    cur.pop();
                    space = true;
                    line += 1;
                    i += 1;
                    continue;
                }
            }
            flush(&mut cur, &mut out, cur_line);
            line += 1;
            i += 1;
            space = false;
            continue;
        }
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            if depth != 0 {
                bail!("REX0006: line {line}: a comment is not ended.");
            }
            space = true;
            continue;
        }
        if cur.is_empty() {
            cur_line = line;
        }
        let tok = if c == '\'' || c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => bail!("REX0006: line {line}: a string is not ended."),
                    Some(&q) if q == c => {
                        if chars.get(i + 1) == Some(&c) {
                            s.push(c);
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    }
                    Some('\n') => bail!("REX0006: line {line}: a string is not ended."),
                    Some(&x) => {
                        s.push(x);
                        i += 1;
                    }
                }
            }
            // 'C1'x, '0101'b: not when a symbol character follows.
            match chars.get(i).map(|c| c.to_ascii_uppercase()) {
                Some(k @ ('X' | 'B')) if !chars.get(i + 1).is_some_and(|&n| is_sym_char(n)) => {
                    i += 1;
                    Tok::Str(string_bytes(&s, k, line)?)
                }
                _ => Tok::Str(s),
            }
        } else if is_sym_char(c) {
            let mut s = String::new();
            while i < chars.len() && is_sym_char(chars[i]) {
                s.push(chars[i]);
                i += 1;
            }
            // 1E+5: the sign of an exponent is part of a number.
            let up = s.to_ascii_uppercase();
            if up.ends_with('E')
                && up[..up.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.')
                && up
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '.')
                && matches!(chars.get(i), Some('+' | '-'))
                && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit())
            {
                s.push(chars[i]);
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    s.push(chars[i]);
                    i += 1;
                }
            }
            Tok::Sym(s.to_ascii_uppercase())
        } else {
            i += 1;
            match c {
                ';' => {
                    flush(&mut cur, &mut out, cur_line);
                    space = false;
                    continue;
                }
                ',' => Tok::Comma,
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                ':' => Tok::Colon,
                _ => {
                    let not = |c: char| c == '\\' || c == '¬';
                    let mut op = String::from(if not(c) { '\\' } else { c });
                    let two = chars.get(i).copied();
                    let three = chars.get(i + 1).copied();
                    let push = |op: &mut String, i: &mut usize, x: char| {
                        op.push(if not(x) { '\\' } else { x });
                        *i += 1;
                    };
                    match (c, two, three) {
                        ('*', Some('*'), _)
                        | ('/', Some('/'), _)
                        | ('|', Some('|'), _)
                        | ('&', Some('&'), _) => push(&mut op, &mut i, two.unwrap()),
                        ('=', Some('='), _) => push(&mut op, &mut i, '='),
                        ('>' | '<', Some(x), Some('=')) if x == c => {
                            push(&mut op, &mut i, x);
                            push(&mut op, &mut i, '=');
                        }
                        ('>', Some(x @ ('>' | '<' | '=')), _)
                        | ('<', Some(x @ ('<' | '>' | '=')), _) => push(&mut op, &mut i, x),
                        (n, Some(x @ ('=' | '>' | '<')), _) if not(n) => {
                            push(&mut op, &mut i, x);
                            match (x, chars.get(i).copied()) {
                                ('=', Some('=')) => push(&mut op, &mut i, '='),
                                ('>', Some('>')) => push(&mut op, &mut i, '>'),
                                ('<', Some('<')) => push(&mut op, &mut i, '<'),
                                _ => {}
                            }
                        }
                        ('+' | '-' | '*' | '/' | '%' | '|' | '&' | '=' | '>' | '<', _, _) => {}
                        (n, _, _) if not(n) => {}
                        _ => bail!("REX0013: line {line}: the character '{c}' is not valid here."),
                    }
                    Tok::Op(op)
                }
            }
        };
        cur.push(Token { tok, space, line });
        space = false;
    }
    flush(&mut cur, &mut out, cur_line);
    Ok(split_keywords(out))
}

fn is_kw(t: &Token, kw: &str) -> bool {
    matches!(&t.tok, Tok::Sym(s) if s == kw)
}

/// Labels, THEN, ELSE, OTHERWISE as clauses of their own; IF and WHEN
/// end before their THEN.
fn split_keywords(raw: Vec<Clause>) -> Vec<Clause> {
    let mut out = Vec::new();
    let mut todo: std::collections::VecDeque<Clause> = raw.into();
    while let Some(c) = todo.pop_front() {
        let t = &c.toks;
        if t.is_empty() {
            continue;
        }
        let assign = t.len() > 1 && matches!(&t[1].tok, Tok::Op(o) if o == "=");
        let split_at =
            |n: usize, out: &mut Vec<Clause>, todo: &mut std::collections::VecDeque<Clause>| {
                let rest: Vec<Token> = c.toks[n..].to_vec();
                out.push(Clause {
                    line: c.line,
                    toks: c.toks[..n].to_vec(),
                });
                if !rest.is_empty() {
                    todo.push_front(Clause {
                        line: rest[0].line,
                        toks: rest,
                    });
                }
            };
        if t.len() > 1 && matches!(t[0].tok, Tok::Sym(_)) && t[1].tok == Tok::Colon {
            split_at(2, &mut out, &mut todo);
            continue;
        }
        if !assign && (is_kw(&t[0], "THEN") || is_kw(&t[0], "ELSE") || is_kw(&t[0], "OTHERWISE")) {
            split_at(1, &mut out, &mut todo);
            continue;
        }
        if !assign && (is_kw(&t[0], "IF") || is_kw(&t[0], "WHEN")) {
            let mut depth = 0;
            let mut at = None;
            for (k, x) in t.iter().enumerate().skip(1) {
                match x.tok {
                    Tok::LParen => depth += 1,
                    Tok::RParen => depth -= 1,
                    _ => {}
                }
                if depth == 0 && is_kw(x, "THEN") {
                    at = Some(k);
                    break;
                }
            }
            if let Some(k) = at {
                split_at(k, &mut out, &mut todo);
                continue;
            }
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------ expressions

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Lit(String),
    /// A variable (simple or compound), as written (upper case).
    Var(String),
    /// A function call: name, whether the name was a string (no internal
    /// routine), the arguments (None: omitted).
    Call(String, bool, Vec<Option<Expr>>),
    Prefix(String, Box<Expr>),
    Bin(String, Box<Expr>, Box<Expr>),
}

pub struct Exp<'a> {
    t: &'a [Token],
    pub i: usize,
}

const COMPARISONS: &[&str] = &[
    "=", "\\=", "<>", "><", ">", "<", ">=", "<=", "\\>", "\\<", "==", "\\==", ">>", "<<", ">>=",
    "<<=", "\\>>", "\\<<",
];

impl<'a> Exp<'a> {
    pub fn new(t: &'a [Token]) -> Exp<'a> {
        Exp { t, i: 0 }
    }

    fn peek(&self) -> Option<&'a Token> {
        self.t.get(self.i)
    }

    fn op(&self) -> Option<&'a str> {
        match self.peek().map(|t| &t.tok) {
            Some(Tok::Op(o)) => Some(o.as_str()),
            _ => None,
        }
    }

    /// A whole expression; None when there are no tokens.
    pub fn parse_all(t: &'a [Token]) -> Result<Option<Expr>> {
        if t.is_empty() {
            return Ok(None);
        }
        let mut p = Exp::new(t);
        let e = p.expr()?;
        if let Some(x) = p.peek() {
            bail!(
                "REX0035: line {}: unexpected {:?} in an expression.",
                x.line,
                x.tok
            );
        }
        Ok(Some(e))
    }

    pub fn expr(&mut self) -> Result<Expr> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while let Some(o @ ("|" | "&&")) = self.op() {
            self.i += 1;
            let r = self.and()?;
            l = Expr::Bin(o.into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr> {
        let mut l = self.cmp()?;
        while let Some("&") = self.op() {
            self.i += 1;
            let r = self.cmp()?;
            l = Expr::Bin("&".into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn cmp(&mut self) -> Result<Expr> {
        let mut l = self.concat()?;
        while let Some(o) = self.op().filter(|o| COMPARISONS.contains(o)) {
            self.i += 1;
            let r = self.concat()?;
            l = Expr::Bin(o.into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn starts_term(&self) -> bool {
        matches!(
            self.peek().map(|t| &t.tok),
            Some(Tok::Str(_) | Tok::Sym(_) | Tok::LParen)
        )
    }

    fn concat(&mut self) -> Result<Expr> {
        let mut l = self.add()?;
        loop {
            if let Some("||") = self.op() {
                self.i += 1;
                let r = self.add()?;
                l = Expr::Bin("||".into(), Box::new(l), Box::new(r));
            } else if self.starts_term() {
                let blank = self.peek().is_some_and(|t| t.space);
                let r = self.add()?;
                l = Expr::Bin(
                    if blank { " " } else { "" }.into(),
                    Box::new(l),
                    Box::new(r),
                );
            } else {
                return Ok(l);
            }
        }
    }

    fn add(&mut self) -> Result<Expr> {
        let mut l = self.mul()?;
        while let Some(o @ ("+" | "-")) = self.op() {
            self.i += 1;
            let r = self.mul()?;
            l = Expr::Bin(o.into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn mul(&mut self) -> Result<Expr> {
        let mut l = self.pow()?;
        while let Some(o @ ("*" | "/" | "%" | "//")) = self.op() {
            self.i += 1;
            let r = self.pow()?;
            l = Expr::Bin(o.into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn pow(&mut self) -> Result<Expr> {
        let mut l = self.prefix()?;
        while let Some("**") = self.op() {
            self.i += 1;
            let r = self.prefix()?;
            l = Expr::Bin("**".into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn prefix(&mut self) -> Result<Expr> {
        if let Some(o @ ("\\" | "-" | "+")) = self.op() {
            self.i += 1;
            let e = self.prefix()?;
            return Ok(Expr::Prefix(o.into(), Box::new(e)));
        }
        self.term()
    }

    fn term(&mut self) -> Result<Expr> {
        let Some(t) = self.peek() else {
            bail!("REX0035: an expression is missing a term.");
        };
        self.i += 1;
        let call_follows = |s: &Self| s.peek().is_some_and(|n| n.tok == Tok::LParen && !n.space);
        match &t.tok {
            Tok::Str(s) => {
                if call_follows(self) {
                    self.i += 1;
                    let args = self.args()?;
                    return Ok(Expr::Call(s.to_ascii_uppercase(), true, args));
                }
                Ok(Expr::Lit(s.clone()))
            }
            Tok::Sym(s) => {
                if call_follows(self) {
                    self.i += 1;
                    let args = self.args()?;
                    return Ok(Expr::Call(s.clone(), false, args));
                }
                if s.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
                    Ok(Expr::Lit(s.clone()))
                } else {
                    Ok(Expr::Var(s.clone()))
                }
            }
            Tok::LParen => {
                let e = self.expr()?;
                match self.peek() {
                    Some(Token {
                        tok: Tok::RParen, ..
                    }) => {
                        self.i += 1;
                        Ok(e)
                    }
                    _ => bail!("REX0036: line {}: a right parenthesis is missing.", t.line),
                }
            }
            other => bail!("REX0035: line {}: {other:?} cannot start a term.", t.line),
        }
    }

    /// Arguments after the left parenthesis, to the right one (None:
    /// an omitted argument).
    fn args(&mut self) -> Result<Vec<Option<Expr>>> {
        let mut out = Vec::new();
        if let Some(Tok::RParen) = self.peek().map(|t| &t.tok) {
            self.i += 1;
            return Ok(out);
        }
        loop {
            match self.peek().map(|t| &t.tok) {
                Some(Tok::Comma | Tok::RParen) => out.push(None),
                None => bail!("REX0036: a right parenthesis is missing in a function call."),
                _ => out.push(Some(self.expr()?)),
            }
            match self.peek().map(|t| &t.tok) {
                Some(Tok::Comma) => self.i += 1,
                Some(Tok::RParen) => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => bail!("REX0036: a right parenthesis is missing in a function call."),
            }
        }
    }
}

/// Comma-separated expressions at the top level (CALL's arguments):
/// None for an omitted one.
pub fn arg_list(t: &[Token]) -> Result<Vec<Option<Expr>>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    for (k, x) in t.iter().enumerate() {
        match x.tok {
            Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            Tok::Comma if depth == 0 => {
                out.push(Exp::parse_all(&t[start..k])?);
                start = k + 1;
            }
            _ => {}
        }
    }
    if !t.is_empty() {
        out.push(Exp::parse_all(&t[start..])?);
    }
    Ok(out)
}

// ------------------------------------------------------------ templates

#[derive(Clone, Debug, PartialEq)]
pub enum Pos {
    Abs(Expr),
    Plus(Expr),
    Minus(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TItem {
    Var(String),
    Dot,
    /// A literal pattern, or a variable pattern `(name)`.
    Pattern(Expr),
    Pos(Pos),
}

/// Templates separated by commas.
pub fn templates(t: &[Token]) -> Result<Vec<Vec<TItem>>> {
    let mut all = vec![Vec::new()];
    let mut i = 0;
    while i < t.len() {
        let cur = all.last_mut().unwrap();
        match &t[i].tok {
            Tok::Comma => {
                all.push(Vec::new());
                i += 1;
            }
            Tok::Str(s) => {
                cur.push(TItem::Pattern(Expr::Lit(s.clone())));
                i += 1;
            }
            Tok::Sym(s) if s == "." => {
                cur.push(TItem::Dot);
                i += 1;
            }
            Tok::Sym(s) if s.starts_with(|c: char| c.is_ascii_digit()) => {
                cur.push(TItem::Pos(Pos::Abs(Expr::Lit(s.clone()))));
                i += 1;
            }
            Tok::Sym(s) => {
                cur.push(TItem::Var(s.clone()));
                i += 1;
            }
            Tok::LParen => {
                let (e, n) = paren_var(t, i)?;
                cur.push(TItem::Pattern(e));
                i = n;
            }
            Tok::Op(o) if o == "+" || o == "-" || o == "=" => {
                let o = o.clone();
                let (e, n) = match t.get(i + 1).map(|x| &x.tok) {
                    Some(Tok::Sym(s)) => (Expr::Lit(s.clone()), i + 2),
                    Some(Tok::LParen) => paren_var(t, i + 1)?,
                    _ => bail!(
                        "REX0038: line {}: a position is missing after {o}.",
                        t[i].line
                    ),
                };
                cur.push(TItem::Pos(match o.as_str() {
                    "+" => Pos::Plus(e),
                    "-" => Pos::Minus(e),
                    _ => Pos::Abs(e),
                }));
                i = n;
            }
            other => bail!(
                "REX0038: line {}: {other:?} is not valid in a template.",
                t[i].line
            ),
        }
    }
    Ok(all)
}

fn paren_var(t: &[Token], i: usize) -> Result<(Expr, usize)> {
    match (t.get(i + 1).map(|x| &x.tok), t.get(i + 2).map(|x| &x.tok)) {
        (Some(Tok::Sym(s)), Some(Tok::RParen)) => Ok((Expr::Var(s.clone()), i + 3)),
        _ => bail!("REX0038: line {}: a variable pattern is (name).", t[i].line),
    }
}

// ------------------------------------------------------------ instructions

#[derive(Clone, Debug)]
pub enum Source {
    Arg,
    Pull,
    Var(String),
    Value(Option<Expr>),
    SourceInfo,
    Version,
}

#[derive(Clone, Debug)]
pub struct DoCtl {
    /// DO name = start [TO] [BY] [FOR].
    pub var: Option<(String, Expr)>,
    pub to: Option<Expr>,
    pub by: Option<Expr>,
    pub count: Option<Expr>,
    pub forever: bool,
    pub while_: Option<Expr>,
    pub until: Option<Expr>,
    /// A simple DO group (no repetition).
    pub simple: bool,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Nop,
    Label(String),
    Say(Option<Expr>),
    Assign(String, Option<Expr>),
    Command(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Do(Box<DoCtl>, Vec<Stmt>),
    Select(Vec<(Expr, Stmt)>, Option<Vec<Stmt>>),
    Leave(Option<String>),
    Iterate(Option<String>),
    Call(String, bool, Vec<Option<Expr>>),
    /// CALL/SIGNAL ON|OFF condition [NAME label]: (signal?, on?, cond, name).
    Trap(bool, bool, String, Option<String>),
    Signal(String),
    SignalValue(Expr),
    Return(Option<Expr>),
    Exit(Option<Expr>),
    Procedure(Vec<String>),
    Parse(bool, Source, Vec<Vec<TItem>>),
    Drop(Vec<String>),
    Interpret(Expr),
    Address(Option<String>, Option<Expr>),
    AddressValue(Expr),
    Numeric(String, Option<Expr>),
    Push(bool, Option<Expr>),
    Ignore,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub line: usize,
    pub stmt: Stmt,
}

pub struct Parser {
    cl: Vec<Clause>,
    i: usize,
}

fn kw(c: &Clause) -> Option<&str> {
    match c.toks.first().map(|t| &t.tok) {
        Some(Tok::Sym(s)) => {
            if c.toks
                .get(1)
                .is_some_and(|t| matches!(&t.tok, Tok::Op(o) if o == "="))
            {
                None
            } else {
                Some(s.as_str())
            }
        }
        _ => None,
    }
}

/// Split tokens at top-level keywords (DO TO/BY/FOR/WHILE/UNTIL...).
fn split_kw<'a>(t: &'a [Token], words: &[&str]) -> Vec<(Option<String>, &'a [Token])> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut cur: Option<String> = None;
    let mut depth = 0i32;
    for (k, x) in t.iter().enumerate() {
        match &x.tok {
            Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            Tok::Sym(s) if depth == 0 && words.contains(&s.as_str()) => {
                out.push((cur.take(), &t[start..k]));
                cur = Some(s.clone());
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push((cur, &t[start..]));
    out
}

impl Parser {
    pub fn program(src: &str) -> Result<Vec<Line>> {
        let mut p = Parser {
            cl: clauses(src)?,
            i: 0,
        };
        let mut out = Vec::new();
        while p.i < p.cl.len() {
            if let Some(k @ ("END" | "THEN" | "ELSE" | "WHEN" | "OTHERWISE")) = kw(&p.cl[p.i]) {
                bail!(
                    "REX0010: line {}: {k} has no matching instruction.",
                    p.cl[p.i].line
                );
            }
            out.push(p.instruction()?);
        }
        Ok(out)
    }

    fn next(&mut self) -> Result<Clause> {
        let c = self.cl.get(self.i).cloned().ok_or_else(|| {
            anyhow!("REX0014: an instruction is incomplete at the end of the program.")
        })?;
        self.i += 1;
        Ok(c)
    }

    fn peek_kw(&self) -> Option<&str> {
        self.cl.get(self.i).and_then(kw)
    }

    fn instruction(&mut self) -> Result<Line> {
        let c = self.next()?;
        let line = c.line;
        let t = &c.toks;
        let stmt = if t.len() > 1 && t[1].tok == Tok::Colon {
            match &t[0].tok {
                Tok::Sym(s) => Stmt::Label(s.clone()),
                _ => unreachable!(),
            }
        } else if t.len() > 1
            && matches!(&t[1].tok, Tok::Op(o) if o == "=")
            && matches!(t[0].tok, Tok::Sym(_))
        {
            let Tok::Sym(name) = &t[0].tok else {
                unreachable!()
            };
            if name.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
                bail!("REX0031: line {line}: {name} cannot be assigned a value.");
            }
            Stmt::Assign(name.clone(), Exp::parse_all(&t[2..])?)
        } else {
            match kw(&c).unwrap_or("") {
                "SAY" => Stmt::Say(Exp::parse_all(&t[1..])?),
                "NOP" => Stmt::Nop,
                "IF" => {
                    let cond = Exp::parse_all(&t[1..])?
                        .ok_or_else(|| anyhow!("REX0035: line {line}: IF needs an expression."))?;
                    if self.peek_kw() != Some("THEN") {
                        bail!("REX0018: line {line}: THEN is missing.");
                    }
                    self.i += 1;
                    let then = self.instruction()?;
                    let els = if self.peek_kw() == Some("ELSE") {
                        self.i += 1;
                        Some(Box::new(self.instruction()?.stmt))
                    } else {
                        None
                    };
                    Stmt::If(cond, Box::new(then.stmt), els)
                }
                "DO" => {
                    let ctl = do_control(&t[1..], line)?;
                    let body = self.until_end(line)?;
                    Stmt::Do(Box::new(ctl), body)
                }
                "SELECT" => {
                    let mut whens = Vec::new();
                    let mut other = None;
                    loop {
                        match self.peek_kw() {
                            Some("WHEN") => {
                                let w = self.next()?;
                                let cond = Exp::parse_all(&w.toks[1..])?.ok_or_else(|| {
                                    anyhow!("REX0035: line {}: WHEN needs an expression.", w.line)
                                })?;
                                if self.peek_kw() != Some("THEN") {
                                    bail!("REX0018: line {}: THEN is missing.", w.line);
                                }
                                self.i += 1;
                                whens.push((cond, self.instruction()?.stmt));
                            }
                            Some("OTHERWISE") => {
                                self.i += 1;
                                let mut list = Vec::new();
                                while self.peek_kw() != Some("END") {
                                    if self.i >= self.cl.len() {
                                        bail!("REX0014: line {line}: SELECT has no END.");
                                    }
                                    list.push(self.instruction()?.stmt);
                                }
                                other = Some(list);
                            }
                            Some("END") => {
                                self.i += 1;
                                break;
                            }
                            _ => bail!(
                                "REX0007: line {}: WHEN, OTHERWISE or END expected in SELECT.",
                                self.cl.get(self.i).map_or(line, |c| c.line)
                            ),
                        }
                    }
                    Stmt::Select(whens, other)
                }
                "LEAVE" | "ITERATE" => {
                    let name = match t.get(1).map(|x| &x.tok) {
                        Some(Tok::Sym(s)) => Some(s.clone()),
                        _ => None,
                    };
                    if kw(&c) == Some("LEAVE") {
                        Stmt::Leave(name)
                    } else {
                        Stmt::Iterate(name)
                    }
                }
                "CALL" | "SIGNAL" => {
                    let signal = kw(&c) == Some("SIGNAL");
                    match t.get(1).map(|x| &x.tok) {
                        Some(Tok::Sym(s)) if s == "ON" || s == "OFF" => {
                            let cond = match t.get(2).map(|x| &x.tok) {
                                Some(Tok::Sym(c)) => c.clone(),
                                _ => bail!("REX0025: line {line}: a condition is missing."),
                            };
                            let name = match (t.get(3).map(|x| &x.tok), t.get(4).map(|x| &x.tok)) {
                                (Some(Tok::Sym(n)), Some(Tok::Sym(l))) if n == "NAME" => {
                                    Some(l.clone())
                                }
                                (Some(Tok::Sym(n)), Some(Tok::Str(l))) if n == "NAME" => {
                                    Some(l.clone())
                                }
                                _ => None,
                            };
                            Stmt::Trap(signal, s == "ON", cond, name)
                        }
                        Some(Tok::Sym(s)) if signal && s == "VALUE" => {
                            Stmt::SignalValue(Exp::parse_all(&t[2..])?.ok_or_else(|| {
                                anyhow!("REX0035: line {line}: SIGNAL VALUE needs an expression.")
                            })?)
                        }
                        Some(Tok::Sym(s)) if signal => Stmt::Signal(s.clone()),
                        Some(Tok::Str(s)) if signal => Stmt::Signal(s.clone()),
                        Some(Tok::LParen) if signal => {
                            Stmt::SignalValue(Exp::parse_all(&t[1..])?.expect("tokens"))
                        }
                        Some(Tok::Sym(s)) => Stmt::Call(s.clone(), false, arg_list(&t[2..])?),
                        Some(Tok::Str(s)) => {
                            Stmt::Call(s.to_ascii_uppercase(), true, arg_list(&t[2..])?)
                        }
                        _ => bail!("REX0019: line {line}: a routine name is missing."),
                    }
                }
                "RETURN" => Stmt::Return(Exp::parse_all(&t[1..])?),
                "EXIT" => Stmt::Exit(Exp::parse_all(&t[1..])?),
                "PROCEDURE" => {
                    let mut names = Vec::new();
                    if t.len() > 1 {
                        if !is_kw(&t[1], "EXPOSE") {
                            bail!("REX0025: line {line}: EXPOSE expected after PROCEDURE.");
                        }
                        let mut k = 2;
                        while k < t.len() {
                            match &t[k].tok {
                                Tok::Sym(s) => names.push(s.clone()),
                                Tok::LParen => {
                                    if let Some(Tok::Sym(s)) = t.get(k + 1).map(|x| &x.tok) {
                                        names.push(format!("({s})"));
                                    }
                                    k += 2;
                                }
                                _ => {}
                            }
                            k += 1;
                        }
                    }
                    Stmt::Procedure(names)
                }
                "PARSE" | "ARG" | "PULL" => {
                    let (upper, src, rest) = match kw(&c) {
                        Some("ARG") => (true, Source::Arg, &t[1..]),
                        Some("PULL") => (true, Source::Pull, &t[1..]),
                        _ => {
                            let mut k = 1;
                            let mut upper = false;
                            if t.get(k).is_some_and(|x| is_kw(x, "UPPER")) {
                                upper = true;
                                k += 1;
                            } else if t
                                .get(k)
                                .is_some_and(|x| is_kw(x, "LOWER") || is_kw(x, "CASELESS"))
                            {
                                k += 1;
                            }
                            let w = match t.get(k).map(|x| &x.tok) {
                                Some(Tok::Sym(s)) => s.clone(),
                                _ => bail!(
                                    "REX0025: line {line}: PARSE needs ARG, PULL, VAR or VALUE."
                                ),
                            };
                            k += 1;
                            match w.as_str() {
                                "ARG" => (upper, Source::Arg, &t[k..]),
                                "PULL" => (upper, Source::Pull, &t[k..]),
                                "SOURCE" => (upper, Source::SourceInfo, &t[k..]),
                                "VERSION" => (upper, Source::Version, &t[k..]),
                                "VAR" => match t.get(k).map(|x| &x.tok) {
                                    Some(Tok::Sym(v)) => {
                                        (upper, Source::Var(v.clone()), &t[k + 1..])
                                    }
                                    _ => bail!("REX0019: line {line}: PARSE VAR needs a variable."),
                                },
                                "VALUE" => {
                                    let rest = &t[k..];
                                    let with = rest
                                        .iter()
                                        .position(|x| is_kw(x, "WITH"))
                                        .ok_or_else(|| {
                                            anyhow!("REX0038: line {line}: PARSE VALUE needs WITH.")
                                        })?;
                                    (
                                        upper,
                                        Source::Value(Exp::parse_all(&rest[..with])?),
                                        &rest[with + 1..],
                                    )
                                }
                                other => {
                                    bail!("REX0025: line {line}: PARSE {other} is not supported.")
                                }
                            }
                        }
                    };
                    Stmt::Parse(upper, src, templates(rest)?)
                }
                "DROP" => Stmt::Drop(
                    t[1..]
                        .iter()
                        .filter_map(|x| match &x.tok {
                            Tok::Sym(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect(),
                ),
                "INTERPRET" => Stmt::Interpret(Exp::parse_all(&t[1..])?.ok_or_else(|| {
                    anyhow!("REX0035: line {line}: INTERPRET needs an expression.")
                })?),
                "ADDRESS" => match t.get(1).map(|x| &x.tok) {
                    None => Stmt::Address(None, None),
                    Some(Tok::Sym(s)) if s == "VALUE" => {
                        Stmt::AddressValue(Exp::parse_all(&t[2..])?.ok_or_else(|| {
                            anyhow!("REX0035: line {line}: ADDRESS VALUE needs an expression.")
                        })?)
                    }
                    Some(Tok::Sym(s)) | Some(Tok::Str(s)) => {
                        Stmt::Address(Some(s.to_ascii_uppercase()), Exp::parse_all(&t[2..])?)
                    }
                    Some(Tok::Op(o)) if o == "*" => {
                        // ADDRESS *COMMAND written without quotes.
                        let name = match t.get(2).map(|x| &x.tok) {
                            Some(Tok::Sym(s)) => format!("*{s}"),
                            _ => bail!("REX0025: line {line}: an environment name is missing."),
                        };
                        Stmt::Address(Some(name), Exp::parse_all(&t[3..])?)
                    }
                    _ => bail!("REX0025: line {line}: an environment name is missing."),
                },
                "NUMERIC" => match t.get(1).map(|x| &x.tok) {
                    Some(Tok::Sym(s)) => Stmt::Numeric(s.clone(), Exp::parse_all(&t[2..])?),
                    _ => bail!("REX0025: line {line}: NUMERIC DIGITS, FUZZ or FORM."),
                },
                "PUSH" => Stmt::Push(true, Exp::parse_all(&t[1..])?),
                "QUEUE" => Stmt::Push(false, Exp::parse_all(&t[1..])?),
                "TRACE" | "OPTIONS" => Stmt::Ignore,
                "END" | "THEN" | "ELSE" | "WHEN" | "OTHERWISE" => {
                    bail!(
                        "REX0010: line {line}: {} has no matching instruction.",
                        kw(&c).unwrap()
                    )
                }
                _ => Stmt::Command(Exp::parse_all(t)?.expect("a clause has tokens")),
            }
        };
        Ok(Line { line, stmt })
    }

    fn until_end(&mut self, line: usize) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        loop {
            if self.i >= self.cl.len() {
                bail!("REX0014: line {line}: DO has no END.");
            }
            if self.peek_kw() == Some("END") {
                self.i += 1;
                return Ok(body);
            }
            let l = self.instruction()?;
            body.push(l.stmt);
        }
    }
}

fn do_control(t: &[Token], line: usize) -> Result<DoCtl> {
    let mut c = DoCtl {
        var: None,
        to: None,
        by: None,
        count: None,
        forever: false,
        while_: None,
        until: None,
        simple: t.is_empty(),
    };
    if t.is_empty() {
        return Ok(c);
    }
    let parts = split_kw(t, &["TO", "BY", "FOR", "WHILE", "UNTIL", "FOREVER"]);
    for (k, toks) in parts {
        let e = || -> Result<Expr> {
            Exp::parse_all(toks)?
                .ok_or_else(|| anyhow!("REX0035: line {line}: an expression is missing in DO."))
        };
        match k.as_deref() {
            None => {
                if toks.is_empty() {
                    continue;
                }
                let assign = toks.len() > 1
                    && matches!(toks[0].tok, Tok::Sym(_))
                    && matches!(&toks[1].tok, Tok::Op(o) if o == "=");
                if assign {
                    let Tok::Sym(n) = &toks[0].tok else {
                        unreachable!()
                    };
                    let start = Exp::parse_all(&toks[2..])?
                        .ok_or_else(|| anyhow!("REX0035: line {line}: DO {n} = needs a value."))?;
                    c.var = Some((n.clone(), start));
                } else {
                    c.count = Some(e()?);
                }
            }
            Some("TO") => c.to = Some(e()?),
            Some("BY") => c.by = Some(e()?),
            Some("FOR") => c.count = Some(e()?),
            Some("WHILE") => c.while_ = Some(e()?),
            Some("UNTIL") => c.until = Some(e()?),
            Some("FOREVER") => c.forever = true,
            _ => {}
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    // REXX/400 Reference: clauses, continuation, labels, THEN on the
    // same line, nested comments.
    #[test]
    fn clauses_and_instructions() {
        let p = Parser::program(
            "/* a /* nested */ comment */\n x = 1; y = 'a''b'\n say x,\n  y\nlab: if x = 1 then say 'one'; else nop\n do i = 1 to 3 by 1\n  say i\n end\n",
        )
        .unwrap();
        assert_eq!(p.len(), 6);
        assert!(
            matches!(&p[1].stmt, Stmt::Assign(n, Some(Expr::Lit(v))) if n == "Y" && v == "a'b")
        );
        assert!(matches!(&p[3].stmt, Stmt::Label(n) if n == "LAB"));
        assert!(matches!(&p[4].stmt, Stmt::If(_, _, Some(_))));
        assert!(matches!(&p[5].stmt, Stmt::Do(c, b) if c.var.is_some() && b.len() == 1));
        // 'C1'x is EBCDIC 'A'.
        let c = clauses("x = 'C1'x").unwrap();
        assert_eq!(c[0].toks[2].tok, Tok::Str("A".into()));
    }
}
