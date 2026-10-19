//! The RPG expression language, shared by free-form statements and the
//! extended factor 2 of fixed-form calculations (ILE RPG reference,
//! "Expressions": operators and their precedence).

use super::ast::{BinOp, Expr, UnOp};
use super::data::{e, Fig};
use super::dec::Dec;
use anyhow::{anyhow, bail, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Name(String),
    Bif(String),
    Special(String),
    Num(String),
    Str(String),
    Hex(Vec<u8>),
    Op(&'static str),
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '#' | '@' | '$')
}

/// Split expression text into tokens.
pub fn tokens(text: &str) -> Result<Vec<Tok>> {
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Character literal, or X'..' hex literal.
        if c == '\'' || ((c == 'x' || c == 'X') && cs.get(i + 1) == Some(&'\'')) {
            let hex = c != '\'';
            if hex {
                i += 1;
            }
            i += 1;
            let mut s = String::new();
            loop {
                match cs.get(i) {
                    None => bail!("a literal is not ended: {text}"),
                    Some('\'') if cs.get(i + 1) == Some(&'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            if hex {
                if !s.len().is_multiple_of(2) {
                    bail!("a hexadecimal literal needs an even number of digits: X'{s}'");
                }
                let bytes = (0..s.len())
                    .step_by(2)
                    .map(|j| u8::from_str_radix(&s[j..j + 2], 16))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| anyhow!("X'{s}' is not a valid hexadecimal literal"))?;
                out.push(Tok::Hex(bytes));
            } else {
                out.push(Tok::Str(s));
            }
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                i += 1;
            }
            out.push(Tok::Num(cs[start..i].iter().collect()));
            continue;
        }
        if c == '%' {
            let start = i + 1;
            i += 1;
            while i < cs.len() && is_name_char(cs[i]) {
                i += 1;
            }
            out.push(Tok::Bif(
                cs[start..i].iter().collect::<String>().to_ascii_uppercase(),
            ));
            continue;
        }
        if c == '*' && cs.get(i + 1).is_some_and(|d| d.is_ascii_alphabetic()) {
            let start = i;
            i += 1;
            while i < cs.len() && is_name_char(cs[i]) {
                i += 1;
            }
            out.push(Tok::Special(
                cs[start..i].iter().collect::<String>().to_ascii_uppercase(),
            ));
            continue;
        }
        // `.NAME` right after an index: `list.users(i).name`.
        let after_index = c == '.'
            && matches!(out.last(), Some(Tok::Op(")")))
            && cs.get(i + 1).is_some_and(|d| d.is_ascii_alphabetic());
        if is_name_char(c) || after_index {
            let start = i;
            if after_index {
                i += 1;
            }
            while i < cs.len()
                && (is_name_char(cs[i])
                    || (cs[i] == '.' && cs.get(i + 1).is_some_and(|d| d.is_ascii_alphabetic())))
            {
                i += 1;
            }
            out.push(Tok::Name(
                cs[start..i].iter().collect::<String>().to_ascii_uppercase(),
            ));
            continue;
        }
        let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        let op: &'static str = match two.as_str() {
            "**" => "**",
            "<>" => "<>",
            "<=" => "<=",
            ">=" => ">=",
            "+=" => "+=",
            "-=" => "-=",
            "*=" => "*=",
            "/=" => "/=",
            _ => match c {
                '+' => "+",
                '-' => "-",
                '*' => "*",
                '/' => "/",
                '=' => "=",
                '<' => "<",
                '>' => ">",
                '(' => "(",
                ')' => ")",
                ':' => ":",
                ';' => ";",
                _ => bail!("unexpected character '{c}' in: {text}"),
            },
        };
        i += op.len();
        out.push(Tok::Op(op));
    }
    Ok(out)
}

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    pub fn new(toks: Vec<Tok>) -> Parser {
        Parser { toks, pos: 0 }
    }

    pub fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    pub fn rest(&self) -> &[Tok] {
        &self.toks[self.pos..]
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Name(n)) if n == w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect_op(&mut self, op: &str) -> Result<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            bail!("expected '{op}', found {:?}", self.peek())
        }
    }

    pub fn expr(&mut self) -> Result<Expr> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.eat_word("OR") {
            let r = self.and()?;
            l = Expr::Bin(BinOp::Or, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr> {
        let mut l = self.not()?;
        while self.eat_word("AND") {
            let r = self.not()?;
            l = Expr::Bin(BinOp::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn not(&mut self) -> Result<Expr> {
        if self.eat_word("NOT") {
            let e = self.not()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(e)));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr> {
        let l = self.add()?;
        let op = match self.peek() {
            Some(Tok::Op("=")) => BinOp::Eq,
            Some(Tok::Op("<>")) => BinOp::Ne,
            Some(Tok::Op("<")) => BinOp::Lt,
            Some(Tok::Op("<=")) => BinOp::Le,
            Some(Tok::Op(">")) => BinOp::Gt,
            Some(Tok::Op(">=")) => BinOp::Ge,
            _ => return Ok(l),
        };
        self.pos += 1;
        let r = self.add()?;
        Ok(Expr::Bin(op, Box::new(l), Box::new(r)))
    }

    fn add(&mut self) -> Result<Expr> {
        let mut l = self.mul()?;
        loop {
            let op = if self.eat_op("+") {
                BinOp::Add
            } else if self.eat_op("-") {
                BinOp::Sub
            } else {
                return Ok(l);
            };
            let r = self.mul()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn mul(&mut self) -> Result<Expr> {
        let mut l = self.pow()?;
        loop {
            let op = if self.eat_op("*") {
                BinOp::Mul
            } else if self.eat_op("/") {
                BinOp::Div
            } else {
                return Ok(l);
            };
            let r = self.pow()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn pow(&mut self) -> Result<Expr> {
        let l = self.unary()?;
        if self.eat_op("**") {
            let r = self.pow()?;
            return Ok(Expr::Bin(BinOp::Pow, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_op("-") {
            let e = self.unary()?;
            return Ok(match e {
                Expr::Num(d) => Expr::Num(d.neg()),
                e => Expr::Unary(UnOp::Neg, Box::new(e)),
            });
        }
        if self.eat_op("+") {
            let e = self.unary()?;
            return Ok(Expr::Unary(UnOp::Plus, Box::new(e)));
        }
        self.primary()
    }

    /// Arguments in parentheses, separated by ':'.
    fn args(&mut self) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        if self.eat_op(")") {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat_op(":") {
                continue;
            }
            self.expect_op(")")?;
            return Ok(args);
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(Dec::parse(&n)?)),
            Some(Tok::Str(s)) => Ok(Expr::Str(e(&s))),
            Some(Tok::Hex(b)) => Ok(Expr::Hex(b)),
            Some(Tok::Op("(")) => {
                let e = self.expr()?;
                self.expect_op(")")?;
                Ok(e)
            }
            Some(Tok::Bif(b)) => {
                let args = if self.eat_op("(") {
                    self.args()?
                } else {
                    vec![]
                };
                Ok(Expr::Bif(b, args))
            }
            Some(Tok::Special(s)) => self.special(&s),
            Some(Tok::Name(n)) => {
                let mut parts: Vec<String> = n.split('.').map(|p| p.to_string()).collect();
                let mut args = if self.eat_op("(") {
                    Some(self.args()?)
                } else {
                    None
                };
                // DS(i).FIELD, A.B(i).C(j): an index before the last part.
                let mut idx: Vec<Option<Box<Expr>>> = vec![None; parts.len() - 1];
                let mut path = false;
                while args.is_some() {
                    let Some(Tok::Name(m)) = self.peek().cloned() else {
                        break;
                    };
                    let Some(rest) = m.strip_prefix('.') else {
                        break;
                    };
                    self.pos += 1;
                    let a = args.take().unwrap();
                    if a.len() != 1 {
                        bail!("{}: one index is expected before .{rest}", parts.join("."));
                    }
                    idx.push(Some(Box::new(a.into_iter().next().unwrap())));
                    let more: Vec<String> = rest.split('.').map(|p| p.to_string()).collect();
                    idx.extend(std::iter::repeat_n(None, more.len() - 1));
                    parts.extend(more);
                    path = true;
                    args = if self.eat_op("(") {
                        Some(self.args()?)
                    } else {
                        None
                    };
                }
                if path {
                    return Ok(Expr::Path { parts, idx, args });
                }
                Ok(Expr::Name { parts, args })
            }
            other => bail!("expected a value, found {other:?}"),
        }
    }

    fn special(&mut self, s: &str) -> Result<Expr> {
        Ok(match s {
            "*BLANK" | "*BLANKS" => Expr::Fig(Fig::Blanks),
            "*ZERO" | "*ZEROS" => Expr::Fig(Fig::Zeros),
            "*HIVAL" => Expr::Fig(Fig::HiVal),
            "*LOVAL" => Expr::Fig(Fig::LoVal),
            "*ON" => Expr::Fig(Fig::On),
            "*OFF" => Expr::Fig(Fig::Off),
            "*NULL" => Expr::Fig(Fig::Null),
            "*OMIT" => Expr::Omit,
            "*ALL" => match self.next() {
                Some(Tok::Str(p)) => Expr::Fig(Fig::All(e(&p))),
                Some(Tok::Hex(b)) => Expr::Fig(Fig::All(b)),
                other => bail!("*ALL needs a literal, found {other:?}"),
            },
            "*IN" => {
                if self.eat_op("(") {
                    let i = self.expr()?;
                    self.expect_op(")")?;
                    Expr::IndArray(Some(Box::new(i)))
                } else {
                    Expr::IndArray(None)
                }
            }
            s if s.starts_with("*IN") && s.len() > 3 => Expr::Ind(s[3..].to_string()),
            s => Expr::Name {
                parts: vec![s.to_string()],
                args: None,
            },
        })
    }
}

/// Parse a whole expression.
pub fn parse(text: &str) -> Result<Expr> {
    let mut p = Parser::new(tokens(text)?);
    let e = p.expr()?;
    if !p.at_end() {
        bail!("unexpected {:?} in: {text}", p.peek());
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence() {
        // ILE RPG reference: * before +, comparisons before NOT, AND
        // before OR.
        let e = parse("a + b * 2 = 7 or not x and y").unwrap();
        let Expr::Bin(BinOp::Or, l, r) = e else {
            panic!("{e:?}")
        };
        assert!(matches!(*l, Expr::Bin(BinOp::Eq, _, _)));
        assert!(matches!(*r, Expr::Bin(BinOp::And, _, _)));
        assert!(matches!(
            parse("-2 ** 2").unwrap(),
            Expr::Bin(BinOp::Pow, _, _)
        ));
    }

    #[test]
    fn literals_and_names() {
        assert_eq!(parse("'it''s'").unwrap(), Expr::Str(e("it's")));
        assert_eq!(parse("x'C1F0'").unwrap(), Expr::Hex(vec![0xC1, 0xF0]));
        assert_eq!(parse("*in03").unwrap(), Expr::Ind("03".into()));
        assert_eq!(parse("*inlr").unwrap(), Expr::Ind("LR".into()));
        assert!(matches!(parse("*in(30)").unwrap(), Expr::IndArray(Some(_))));
        assert!(matches!(
            parse("%trim(name) + 'x'").unwrap(),
            Expr::Bin(BinOp::Add, _, _)
        ));
        assert_eq!(
            parse("cust.name").unwrap(),
            Expr::Name {
                parts: vec!["CUST".into(), "NAME".into()],
                args: None
            }
        );
        assert_eq!(parse(".5").unwrap(), Expr::Num(Dec::parse("0.5").unwrap()));
    }
}
