//! Source member → [`Source`]: fixed-form specifications (H F D I C O P),
//! free-form statements in `/FREE` regions or in positions 8-80 of fixed
//! source, fully free-form `**FREE` source, compiler directives
//! (`/COPY`, `/INCLUDE`, `/IF DEFINED`...), and compile-time data.
//!
//! Layout of each specification: ILE RPG Reference, "Specifications".

use super::ast::*;
use super::data::{e, DType};
use super::expr::{self, Parser, Tok};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;

/// Characters `from..=to` (1-based) of a fixed-form line.
fn col(line: &[char], from: usize, to: usize) -> String {
    if from > line.len() {
        return String::new();
    }
    line[from - 1..to.min(line.len())].iter().collect()
}

fn colt(line: &[char], from: usize, to: usize) -> String {
    col(line, from, to).trim().to_string()
}

/// The include named by a `/COPY` or `/INCLUDE` directive, as written:
/// `QRPGLESRC,MBR`, `LIB/FILE,MBR`, `MBR`, or a quoted stream file path.
fn include_target(directive_rest: &str) -> String {
    let t = directive_rest.trim();
    if let Some(q) = t.strip_prefix('\'') {
        return q.split('\'').next().unwrap_or("").to_string();
    }
    t.split_whitespace().next().unwrap_or("").to_string()
}

/// Is the source fully free-form (`**FREE` in the first line)?
pub fn is_fully_free(source: &str) -> bool {
    source
        .lines()
        .next()
        .is_some_and(|l| l.trim_end().to_ascii_uppercase().starts_with("**FREE"))
}

/// A directive line: (directive word, rest), e.g. ("COPY", "QRPGLESRC,X").
fn directive(content: &str) -> Option<(String, String)> {
    let t = content.trim_start();
    let rest = t.strip_prefix('/')?;
    if !rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let after = rest[word.len()..].to_string();
    Some((word.to_ascii_uppercase(), after))
}

/// Every include this source asks for (to be fetched before parsing).
pub fn includes(source: &str) -> Vec<String> {
    let free = is_fully_free(source);
    let mut out = Vec::new();
    for line in source.lines() {
        let content = if free {
            line.to_string()
        } else {
            let cs: Vec<char> = line.chars().collect();
            if cs.len() < 7 {
                continue;
            }
            col(&cs, 7, 80)
        };
        if let Some((w, rest)) = directive(&content) {
            if w == "COPY" || w == "INCLUDE" {
                out.push(include_target(&rest));
            }
        }
    }
    out
}

/// One unit of source after layout handling.
#[derive(Debug, Clone)]
enum Unit {
    /// A fixed-form specification line (positions 1-80) and its type.
    Spec(char, Vec<char>, usize),
    /// A free-form statement (without the ';').
    Free(String, usize),
}

struct Lines<'a> {
    includes: &'a dyn Fn(&str) -> Option<String>,
    defined: HashSet<String>,
    units: Vec<Unit>,
    ctdata: Vec<(Option<String>, Vec<String>)>,
    depth: usize,
}

/// Strip `//` comments (outside literals) from free-form text.
fn strip_comment(s: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            quoted = !quoted;
        }
        if !quoted && c == '/' && cs.get(i + 1) == Some(&'/') {
            break;
        }
        out.push(c);
        i += 1;
    }
    out
}

impl Lines<'_> {
    fn read(&mut self, source: &str, first_line: usize) -> Result<()> {
        let free_source = is_fully_free(source);
        let mut free_region = false;
        let mut pending = String::new();
        let mut pending_line = 0;
        // Conditional compilation: a stack of (this branch active, some
        // branch taken).
        let mut cond: Vec<(bool, bool)> = Vec::new();
        let active = |c: &Vec<(bool, bool)>| c.iter().all(|x| x.0);
        for (idx, raw) in source.lines().enumerate() {
            let lineno = first_line + idx;
            if free_source && idx == 0 {
                continue;
            }
            let raw = raw.trim_end_matches('\r');
            let cs: Vec<char> = raw.chars().collect();
            // Compile-time data starts the end of the program.
            if !free_source && raw.starts_with("**") || free_source && raw.starts_with("**CTDATA") {
                if active(&cond) {
                    self.read_ctdata(source, idx);
                }
                break;
            }
            // The free-form text of this line, if it is free-form.
            let (free_text, spec): (Option<String>, Option<char>) = if free_source {
                (Some(raw.to_string()), None)
            } else {
                if cs.len() < 6 || col(&cs, 6, 80).trim().is_empty() {
                    continue;
                }
                // A line starting with // is a comment, wherever it starts
                // (documentation comments are often written in column 1,
                // in the sequence number area).
                if raw.trim_start().starts_with("//") {
                    continue;
                }
                let c7 = cs.get(6).copied().unwrap_or(' ');
                let c6 = cs[5];
                if c7 == '*' {
                    continue;
                }
                let content = col(&cs, 7, 80);
                if let Some((w, _)) = directive(&content) {
                    if c6 == ' ' || c6.is_ascii_digit() || c6 == 'C' || c6 == 'c' {
                        match w.as_str() {
                            "FREE" => {
                                free_region = true;
                                continue;
                            }
                            "END-FREE" => {
                                free_region = false;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    (Some(content), None)
                } else if free_region || (c6 == ' ' && c7 == ' ') {
                    (Some(col(&cs, 8, 80)), None)
                } else {
                    (None, Some(c6.to_ascii_uppercase()))
                }
            };
            if let Some(text) = free_text {
                // Directives (outside a statement).
                if pending.trim().is_empty() {
                    if let Some((w, rest)) = directive(&text) {
                        let arg = rest.trim().to_ascii_uppercase();
                        let defined_arg = |a: &str| -> String {
                            a.trim_start_matches("NOT")
                                .trim()
                                .trim_start_matches("DEFINED")
                                .trim()
                                .trim_matches(|c| c == '(' || c == ')')
                                .trim()
                                .to_string()
                        };
                        match w.as_str() {
                            "IF" => {
                                let neg = arg.starts_with("NOT");
                                let d = self.defined.contains(&defined_arg(&arg));
                                let on = d != neg;
                                cond.push((on, on));
                            }
                            "ELSEIF" => {
                                let neg = arg.starts_with("NOT");
                                let d = self.defined.contains(&defined_arg(&arg));
                                if let Some(top) = cond.last_mut() {
                                    let on = !top.1 && (d != neg);
                                    *top = (on, top.1 || on);
                                }
                            }
                            "ELSE" => {
                                if let Some(top) = cond.last_mut() {
                                    *top = (!top.1, true);
                                }
                            }
                            "ENDIF" => {
                                cond.pop();
                            }
                            _ if !active(&cond) => {}
                            "DEFINE" => {
                                self.defined.insert(arg.clone());
                            }
                            "UNDEFINE" => {
                                self.defined.remove(&arg);
                            }
                            "EOF" => break,
                            "COPY" | "INCLUDE" => {
                                let id = include_target(&rest);
                                let text = (self.includes)(&id).ok_or_else(|| {
                                    anyhow!(
                                        "line {lineno}: the /{w} member or file {id} was not found"
                                    )
                                })?;
                                if self.depth > 32 {
                                    bail!("line {lineno}: /{w} nested too deep");
                                }
                                self.depth += 1;
                                self.read(&text, lineno)?;
                                self.depth -= 1;
                            }
                            // /TITLE, /EJECT, /SPACE, /SET, /RESTORE:
                            // listing and CCSID directives.
                            _ => {}
                        }
                        continue;
                    }
                }
                if !active(&cond) {
                    continue;
                }
                let text = strip_comment(&text);
                if text.trim().is_empty() {
                    continue;
                }
                if pending.trim().is_empty() {
                    pending_line = lineno;
                }
                pending.push(' ');
                pending.push_str(&text);
                // Statements end with ';' outside literals.
                loop {
                    let mut quoted = false;
                    let mut at = None;
                    for (i, c) in pending.char_indices() {
                        if c == '\'' {
                            quoted = !quoted;
                        } else if c == ';' && !quoted {
                            at = Some(i);
                            break;
                        }
                    }
                    let Some(i) = at else { break };
                    let st = pending[..i].trim().to_string();
                    pending = pending[i + 1..].to_string();
                    if !st.is_empty() {
                        self.units.push(Unit::Free(st, pending_line));
                    }
                    pending_line = lineno;
                }
                continue;
            }
            if !active(&cond) {
                continue;
            }
            if let Some(s) = spec {
                let mut line = cs.clone();
                line.truncate(80);
                self.units.push(Unit::Spec(s, line, lineno));
            }
        }
        if !pending.trim().is_empty() {
            bail!(
                "line {pending_line}: a free-form statement does not end with ';': {}",
                pending.trim()
            );
        }
        Ok(())
    }

    fn read_ctdata(&mut self, source: &str, start: usize) {
        let mut current: Option<(Option<String>, Vec<String>)> = None;
        for raw in source.lines().skip(start) {
            let raw = raw.trim_end_matches('\r');
            if let Some(rest) = raw.strip_prefix("**") {
                if let Some(c) = current.take() {
                    self.ctdata.push(c);
                }
                let up = rest.trim().to_ascii_uppercase();
                let name = up
                    .strip_prefix("CTDATA")
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty());
                current = Some((name, Vec::new()));
            } else if let Some(c) = current.as_mut() {
                c.1.push(raw.to_string());
            }
        }
        if let Some(c) = current.take() {
            self.ctdata.push(c);
        }
    }
}

/// Parse keywords as written (`DIM(10) INZ('A B') EXTPGM('X')`).
pub fn keywords(text: &str) -> Result<Vec<Keyword>> {
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < cs.len() {
        if cs[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < cs.len() && !cs[i].is_whitespace() && cs[i] != '(' {
            i += 1;
        }
        let name: String = cs[start..i].iter().collect::<String>().to_ascii_uppercase();
        let mut args = Vec::new();
        while i < cs.len() && cs[i].is_whitespace() {
            i += 1;
        }
        if i < cs.len() && cs[i] == '(' {
            i += 1;
            let mut depth = 0;
            let mut quoted = false;
            let mut cur = String::new();
            while i < cs.len() {
                let c = cs[i];
                if c == '\'' {
                    quoted = !quoted;
                }
                if !quoted {
                    if c == '(' {
                        depth += 1;
                    } else if c == ')' {
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                        depth -= 1;
                    } else if c == ':' && depth == 0 {
                        args.push(cur.trim().to_string());
                        cur.clear();
                        i += 1;
                        continue;
                    }
                }
                cur.push(c);
                i += 1;
            }
            args.push(cur.trim().to_string());
        }
        if name.is_empty() {
            bail!("keyword missing in: {text}");
        }
        out.push(Keyword { name, args });
    }
    Ok(out)
}

/// A type from a fixed-form D spec: internal type, length (digits or
/// characters, or bytes for from/to positions), decimals.
fn fixed_type(
    t: char,
    len: u32,
    decimals: Option<u32>,
    by_positions: bool,
    in_ds: bool,
    kws: &[Keyword],
) -> Result<Option<DType>> {
    let t = t.to_ascii_uppercase();
    let varying = keyword(kws, "VARYING").is_some();
    Ok(Some(match t {
        ' ' => {
            if len == 0 {
                return Ok(None);
            }
            match decimals {
                // Numeric without a type: packed, but zoned in a data
                // structure (ILE RPG reference, D spec position 40).
                Some(d) => {
                    if in_ds {
                        DType::Zoned(len, d)
                    } else {
                        DType::Packed(len, d)
                    }
                }
                None if varying => DType::Varchar(len),
                None => DType::Char(len),
            }
        }
        'A' => {
            if varying {
                DType::Varchar(if by_positions {
                    len.saturating_sub(2)
                } else {
                    len
                })
            } else {
                DType::Char(len)
            }
        }
        'S' => DType::Zoned(len, decimals.unwrap_or(0)),
        'P' => DType::Packed(
            if by_positions { len * 2 - 1 } else { len },
            decimals.unwrap_or(0),
        ),
        'B' => {
            let d = if by_positions {
                if len <= 2 {
                    4
                } else {
                    9
                }
            } else {
                len
            };
            DType::Binary(d, decimals.unwrap_or(0))
        }
        'I' | 'U' => {
            let d = if by_positions {
                match len {
                    1 => 3,
                    2 => 5,
                    4 => 10,
                    _ => 20,
                }
            } else {
                len
            };
            if t == 'I' {
                DType::Int(d)
            } else {
                DType::Uns(d)
            }
        }
        'N' => DType::Ind,
        'D' => DType::Date,
        'T' => DType::Time,
        'Z' => DType::Timestamp,
        '*' => DType::Pointer,
        other => bail!("data type {other} is not supported yet"),
    }))
}

/// A free-form type: CHAR(10), VARCHAR(20), PACKED(7:2), ZONED(5),
/// INT(10), UNS(5), BINDEC(4), IND, DATE, TIME, TIMESTAMP, POINTER.
/// Returns None for LIKE/LIKEDS (resolved later, kept as keywords).
fn free_type(word: &str, args: &[String]) -> Result<Option<DType>> {
    let n = |i: usize| -> Result<u32> {
        args.get(i)
            .ok_or_else(|| anyhow!("{word} needs a length"))?
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow!("{word}: '{}' is not a number", args[i]))
    };
    let dec = |i: usize| -> Result<u32> {
        if args.len() > i {
            n(i)
        } else {
            Ok(0)
        }
    };
    Ok(Some(match word {
        "CHAR" => DType::Char(n(0)?),
        "VARCHAR" => DType::Varchar(n(0)?),
        "PACKED" => DType::Packed(n(0)?, dec(1)?),
        "ZONED" => DType::Zoned(n(0)?, dec(1)?),
        "BINDEC" => DType::Binary(n(0)?, dec(1)?),
        "INT" => DType::Int(n(0)?),
        "UNS" => DType::Uns(n(0)?),
        "IND" => DType::Ind,
        "DATE" => DType::Date,
        "TIME" => DType::Time,
        "TIMESTAMP" => DType::Timestamp,
        "POINTER" => DType::Pointer,
        _ => return Ok(None),
    }))
}

const FREE_TYPES: &[&str] = &[
    "CHAR",
    "VARCHAR",
    "PACKED",
    "ZONED",
    "BINDEC",
    "INT",
    "UNS",
    "IND",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "POINTER",
    "GRAPH",
    "VARGRAPH",
    "UCS2",
    "VARUCS2",
    "FLOAT",
    "OBJECT",
];

/// A free-form declaration's name, type and keywords from its words.
fn free_decl(text: &str) -> Result<(String, Option<DType>, Vec<Keyword>)> {
    let kws = keywords(text)?;
    let mut it = kws.into_iter();
    let name = it
        .next()
        .ok_or_else(|| anyhow!("a name is needed: {text}"))?
        .name;
    let mut dtype = None;
    let mut rest = Vec::new();
    for k in it {
        if dtype.is_none() && FREE_TYPES.contains(&k.name.as_str()) {
            let numeric_args = k
                .args
                .iter()
                .all(|a| a.trim().chars().all(|c| c.is_ascii_digit()));
            if !numeric_args {
                // A length given by a named constant: resolved when the
                // program is checked (the keyword stays).
                rest.push(Keyword {
                    name: format!("TYPE:{}", k.name),
                    args: k.args,
                });
                continue;
            }
            match free_type(&k.name, &k.args)? {
                Some(t) => dtype = Some(t),
                None => bail!("type {} is not supported yet", k.name),
            }
        } else {
            rest.push(k);
        }
    }
    Ok((name, dtype, rest))
}

/// What the parser is in the middle of.
struct Ctx {
    src: Source,
    /// The open DS, PR or PI (index into defs), for subfields and parms.
    open: Option<DefKind>,
    proc: Option<Proc>,
    /// A D spec name continued with '...'.
    long_name: String,
    /// Inside a free-form DCL-DS / DCL-PR / DCL-PI.
    free_block: Option<DefKind>,
    /// The fixed-form PLIST or KLIST being built.
    list: Option<(bool, String)>,
    /// The CALL whose PARMs follow.
    call: Option<usize>,
}

impl Ctx {
    fn defs(&mut self) -> &mut Vec<Def> {
        match self.proc.as_mut() {
            Some(p) => &mut p.defs,
            None => &mut self.src.defs,
        }
    }

    fn calcs(&mut self) -> &mut Vec<CalcLine> {
        match self.proc.as_mut() {
            Some(p) => &mut p.calcs,
            None => &mut self.src.calcs,
        }
    }

    fn add_def(&mut self, d: Def) {
        self.defs().push(d);
    }

    fn calc(&mut self, line: usize, cond: Conditions, level: Option<String>, what: Line) {
        self.calcs().push(CalcLine {
            line,
            cond,
            level,
            what,
        });
    }
}

/// Parse a source member. `includes` gives the text of `/COPY` and
/// `/INCLUDE` members by the id written in the directive.
pub fn parse(source: &str, includes: &dyn Fn(&str) -> Option<String>) -> Result<Source> {
    let mut lines = Lines {
        includes,
        defined: [
            "*ILERPG",
            "*CRTBNDRPG",
            "*V7R5M0",
            "*V7R4M0",
            "*V7R3M0",
            "*V7R2M0",
            "*V7R1M0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        units: Vec::new(),
        ctdata: Vec::new(),
        depth: 0,
    };
    lines.read(source, 1)?;
    let mut ctx = Ctx {
        src: Source::default(),
        open: None,
        proc: None,
        long_name: String::new(),
        free_block: None,
        list: None,
        call: None,
    };
    ctx.src.ctdata = lines.ctdata;
    let units = std::mem::take(&mut lines.units);
    let mut i = 0;
    while i < units.len() {
        match &units[i] {
            Unit::Spec(kind, cs, lineno) => {
                let r = match kind {
                    'H' => {
                        ctx.src.control.extend(keywords(&col(cs, 7, 80))?);
                        Ok(())
                    }
                    'F' => fixed_f(&mut ctx, cs, *lineno),
                    'D' => fixed_d(&mut ctx, cs, *lineno),
                    'C' => {
                        // Extended factor 2 continues on following lines
                        // with no opcode.
                        let mut ext = col(cs, 36, 80);
                        while let Some(Unit::Spec('C', next, _)) = units.get(i + 1) {
                            if colt(next, 26, 35).is_empty()
                                && colt(next, 12, 25).is_empty()
                                && !colt(next, 36, 80).is_empty()
                                && is_extended(&colt(cs, 26, 35))
                            {
                                ext.push(' ');
                                ext.push_str(&col(next, 36, 80));
                                i += 1;
                            } else {
                                break;
                            }
                        }
                        fixed_c(&mut ctx, cs, &ext, *lineno)
                    }
                    'O' => fixed_o(&mut ctx, cs, *lineno),
                    'I' => fixed_i(&mut ctx, cs, *lineno),
                    'P' => fixed_p(&mut ctx, cs, *lineno),
                    other => Err(anyhow!("specification type {other} is not valid")),
                };
                r.with_context(|| format!("line {lineno}"))?;
            }
            Unit::Free(text, lineno) => {
                ctx.list = None;
                free(&mut ctx, text, *lineno).with_context(|| format!("line {lineno}: {text}"))?;
            }
        }
        i += 1;
    }
    if let Some(p) = ctx.proc.take() {
        bail!("procedure {} has no end (P spec E or END-PROC)", p.name);
    }
    Ok(ctx.src)
}

/// Opcodes whose factor 2 is an extended factor 2 (positions 36-80).
fn is_extended(op: &str) -> bool {
    let base = op
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "EVAL"
            | "EVALR"
            | "EVAL-CORR"
            | "IF"
            | "ELSEIF"
            | "DOW"
            | "DOU"
            | "WHEN"
            | "FOR"
            | "RETURN"
            | "CALLP"
            | "ON-ERROR"
            | "SORTA"
    )
}

fn fixed_f(ctx: &mut Ctx, cs: &[char], lineno: usize) -> Result<()> {
    let name = colt(cs, 7, 16).to_ascii_uppercase();
    let kw = keywords(&col(cs, 44, 80))?;
    if name.is_empty() {
        // A keyword continuation line.
        if let Some(f) = ctx.src.files.last_mut() {
            f.keywords.extend(kw);
        }
        return Ok(());
    }
    let ch = |p: usize| cs.get(p - 1).copied().unwrap_or(' ').to_ascii_uppercase();
    let reclen = colt(cs, 23, 27);
    ctx.src.files.push(FileDecl {
        name,
        usage: ch(17),
        designation: ch(18),
        add: ch(20) == 'A',
        external: ch(22) == 'E',
        keyed: ch(34) == 'K',
        record_len: reclen.parse().ok(),
        device: colt(cs, 36, 42).to_ascii_uppercase(),
        keywords: kw,
        line: lineno,
    });
    Ok(())
}

fn fixed_d(ctx: &mut Ctx, cs: &[char], lineno: usize) -> Result<()> {
    let raw_name = colt(cs, 7, 21);
    // A long name: NAME... on this line, the definition on the next.
    if let Some(prefix) = raw_name.strip_suffix("...") {
        ctx.long_name.push_str(prefix);
        return Ok(());
    }
    let name = format!("{}{}", ctx.long_name, raw_name).to_ascii_uppercase();
    ctx.long_name.clear();
    let ext = cs
        .get(21)
        .copied()
        .unwrap_or(' ')
        .eq_ignore_ascii_case(&'E');
    let dtype_code = colt(cs, 24, 25).to_ascii_uppercase();
    let from = colt(cs, 26, 32);
    let to = colt(cs, 33, 39);
    let tchar = cs.get(39).copied().unwrap_or(' ');
    let decs = colt(cs, 41, 42);
    let kws = keywords(&col(cs, 44, 80))?;
    // A keyword continuation line (nothing in positions 7-43).
    if name.is_empty()
        && dtype_code.is_empty()
        && from.is_empty()
        && to.is_empty()
        && tchar == ' '
        && decs.is_empty()
        && !ext
    {
        let target = ctx
            .defs()
            .last_mut()
            .ok_or_else(|| anyhow!("keywords continue no definition"))?;
        target.keywords.extend(kws);
        return Ok(());
    }
    let kind = match dtype_code.as_str() {
        "S" => DefKind::Field,
        "C" => DefKind::Const,
        "DS" => DefKind::Ds,
        "PR" => DefKind::Proto,
        "PI" => DefKind::Interface,
        "" => match ctx.open {
            Some(DefKind::Ds) => DefKind::Subfield,
            Some(DefKind::Proto) | Some(DefKind::Interface) => DefKind::Param,
            _ => bail!("a definition type (S, C, DS, PR, PI) is needed for {name}"),
        },
        other => bail!("definition type {other} is not valid"),
    };
    match kind {
        DefKind::Ds | DefKind::Proto | DefKind::Interface => ctx.open = Some(kind.clone()),
        DefKind::Field | DefKind::Const => ctx.open = None,
        _ => {}
    }
    let decimals: Option<u32> = if decs.is_empty() {
        None
    } else {
        Some(
            decs.parse()
                .map_err(|_| anyhow!("decimal positions '{decs}'"))?,
        )
    };
    let (from_n, to_n): (Option<u32>, Option<u32>) =
        (from.parse().ok(), to.trim_start_matches('+').parse().ok());
    let by_positions = from_n.is_some();
    let len = match (from_n, to_n) {
        (Some(f), Some(t)) => t + 1 - f,
        (None, Some(t)) => t,
        _ => 0,
    };
    let in_ds = kind == DefKind::Subfield || kind == DefKind::Ds;
    let dtype = if kind == DefKind::Const || kind == DefKind::Ds && len == 0 {
        None
    } else {
        fixed_type(tchar, len, decimals, by_positions, in_ds, &kws)?
    };
    ctx.add_def(Def {
        name,
        kind,
        dtype,
        from: from_n,
        to: if by_positions { to_n } else { None },
        external: ext,
        keywords: kws,
        line: lineno,
    });
    Ok(())
}

fn fixed_p(ctx: &mut Ctx, cs: &[char], lineno: usize) -> Result<()> {
    let name = colt(cs, 7, 21).to_ascii_uppercase();
    let be = cs.get(23).copied().unwrap_or(' ').to_ascii_uppercase();
    let kws = keywords(&col(cs, 44, 80))?;
    match be {
        'B' => {
            if ctx.proc.is_some() {
                bail!("procedure {name} begins inside another procedure");
            }
            ctx.open = None;
            ctx.proc = Some(Proc {
                name,
                export: keyword(&kws, "EXPORT").is_some(),
                defs: vec![],
                calcs: vec![],
                line: lineno,
            });
        }
        'E' => {
            let p = ctx
                .proc
                .take()
                .ok_or_else(|| anyhow!("P spec E without a begin"))?;
            ctx.open = None;
            ctx.src.procs.push(p);
        }
        _ => bail!("P spec needs B or E in position 24"),
    }
    Ok(())
}

fn indicator(s: &str) -> Option<String> {
    let t = s.trim().to_ascii_uppercase();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Conditioning indicators in positions 9-11 (N and an indicator).
fn conditions(cs: &[char], from: usize) -> Conditions {
    let n = cs
        .get(from - 1)
        .copied()
        .unwrap_or(' ')
        .eq_ignore_ascii_case(&'N');
    match indicator(&col(cs, from + 1, from + 2)) {
        Some(i) => vec![(n, i)],
        None => vec![],
    }
}

fn opt_expr(s: &str) -> Result<Option<Expr>> {
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        Ok(Some(expr::parse(t)?))
    }
}

/// Fixed-form comparison codes: EQ, NE, LT, LE, GT, GE.
fn cmp_code(s: &str) -> Result<BinOp> {
    Ok(match s {
        "EQ" => BinOp::Eq,
        "NE" => BinOp::Ne,
        "LT" => BinOp::Lt,
        "LE" => BinOp::Le,
        "GT" => BinOp::Gt,
        "GE" => BinOp::Ge,
        other => bail!("comparison {other} is not valid"),
    })
}

fn fixed_c(ctx: &mut Ctx, cs: &[char], ext2: &str, lineno: usize) -> Result<()> {
    let level = indicator(&col(cs, 7, 8));
    let level = level.filter(|l| l != "AN" && l != "OR");
    let mut cond = conditions(cs, 9);
    let f1t = colt(cs, 12, 25);
    let op_full = colt(cs, 26, 35).to_ascii_uppercase();
    // Opcodes with an extended factor 2 use positions 36-80 for it.
    let extended = is_extended(&op_full);
    let f2t = if extended {
        String::new()
    } else {
        colt(cs, 36, 49)
    };
    let rt = if extended {
        String::new()
    } else {
        colt(cs, 50, 63)
    };
    let lent = if extended {
        String::new()
    } else {
        colt(cs, 64, 68)
    };
    let dect = if extended {
        String::new()
    } else {
        colt(cs, 69, 70)
    };
    let ind = ResultInd {
        hi: indicator(&col(cs, 71, 72)),
        lo: indicator(&col(cs, 73, 74)),
        eq: indicator(&col(cs, 75, 76)),
    };
    let (op, extender) = match op_full.split_once('(') {
        Some((o, x)) => (o.trim().to_string(), x.trim_end_matches(')').to_string()),
        None => (op_full.clone(), String::new()),
    };
    let ext = Ext::parse(&extender);
    // A result field with a length defines it.
    if !lent.is_empty() && !rt.is_empty() {
        let len: u32 = lent.parse().map_err(|_| anyhow!("field length '{lent}'"))?;
        let dt = if dect.is_empty() {
            DType::Char(len)
        } else {
            DType::Packed(len, dect.parse()?)
        };
        let name = rt.to_ascii_uppercase();
        let exists = ctx.defs().iter().any(|d| d.name == name);
        if !exists {
            ctx.add_def(Def {
                name,
                kind: DefKind::Field,
                dtype: Some(dt),
                from: None,
                to: None,
                external: false,
                keywords: vec![],
                line: lineno,
            });
        }
    }
    let f1 = || opt_expr(&f1t);
    let f2 = || opt_expr(&f2t);
    let res = || opt_expr(&rt);
    // AND/OR lines continue the conditions of an IFxx/DOWxx/WHENxx...
    if (op.starts_with("AND") || op.starts_with("OR")) && op.len() >= 4 && !op.contains('-') {
        let (join, code) = if let Some(c) = op.strip_prefix("AND") {
            (BinOp::And, c)
        } else {
            (BinOp::Or, &op[2..])
        };
        let cmp = Expr::Bin(
            cmp_code(code)?,
            Box::new(f1()?.ok_or_else(|| anyhow!("factor 1 needed"))?),
            Box::new(f2()?.ok_or_else(|| anyhow!("factor 2 needed"))?),
        );
        let last = ctx
            .calcs()
            .last_mut()
            .ok_or_else(|| anyhow!("{op} follows no comparison"))?;
        let target = match &mut last.what {
            Line::If(e) | Line::Dow(e) | Line::Dou(e) | Line::When(e) | Line::ElseIf(e) => e,
            _ => bail!("{op} must follow IFxx, DOWxx, DOUxx or WHENxx"),
        };
        let prev = std::mem::replace(target, Expr::Num(Default::default()));
        *target = Expr::Bin(join, Box::new(prev), Box::new(cmp));
        return Ok(());
    }
    // PLIST / KLIST bodies.
    match op.as_str() {
        "PLIST" => {
            ctx.list = Some((true, f1t.to_ascii_uppercase()));
            if f1t.eq_ignore_ascii_case("*ENTRY") {
                ctx.src.entry = Some(vec![]);
            } else {
                ctx.src.plists.push((f1t.to_ascii_uppercase(), vec![]));
            }
            return Ok(());
        }
        "KLIST" => {
            ctx.list = Some((false, f1t.to_ascii_uppercase()));
            ctx.src.klists.push((f1t.to_ascii_uppercase(), vec![]));
            return Ok(());
        }
        "PARM" => {
            let parm = Parm {
                target: res()?.ok_or_else(|| anyhow!("PARM needs a result field"))?,
                value: f2()?,
            };
            if let Some(ci) = ctx.call {
                if let Some(CalcLine {
                    what: Line::Stmt(Stmt::Call { parms, .. }),
                    ..
                }) = ctx.calcs().get_mut(ci)
                {
                    parms.push(parm);
                    return Ok(());
                }
            }
            match &ctx.list {
                Some((true, n)) if n == "*ENTRY" => {
                    ctx.src.entry.get_or_insert_with(Vec::new).push(parm)
                }
                Some((true, n)) => {
                    let n = n.clone();
                    if let Some(p) = ctx.src.plists.iter_mut().find(|p| p.0 == n) {
                        p.1.push(parm);
                    }
                }
                _ => bail!("PARM must follow PLIST or CALL"),
            }
            return Ok(());
        }
        "KFLD" => {
            let Some((false, n)) = ctx.list.clone() else {
                bail!("KFLD must follow KLIST")
            };
            let k = res()?.ok_or_else(|| anyhow!("KFLD needs a result field"))?;
            if let Some(kl) = ctx.src.klists.iter_mut().find(|k| k.0 == n) {
                kl.1.push(k);
            }
            return Ok(());
        }
        _ => {
            ctx.list = None;
        }
    }
    if op != "PARM" {
        ctx.call = None;
    }
    let ext2 = ext2.trim();
    let xf2 = || opt_expr(ext2);
    let file_op = |o: FileOp| -> Result<Line> {
        let key = match o {
            FileOp::Read
            | FileOp::ReadP
            | FileOp::Write
            | FileOp::Update
            | FileOp::Unlock
            | FileOp::Open
            | FileOp::Close
            | FileOp::Exfmt
            | FileOp::ReadC
            | FileOp::Feod => None,
            _ => f1()?.map(|k| vec![k]),
        };
        Ok(Line::Stmt(Stmt::File {
            op: o,
            file: f2t.to_ascii_uppercase(),
            key,
            ds: if rt.is_empty() {
                None
            } else {
                Some(rt.to_ascii_uppercase())
            },
            ext: ext.clone(),
            ind: ind.clone(),
        }))
    };
    let need = |e: Result<Option<Expr>>, what: &str| -> Result<Expr> {
        e?.ok_or_else(|| anyhow!("{op}: {what} is needed"))
    };
    let cmp_line = |code: &str| -> Result<Expr> {
        Ok(Expr::Bin(
            cmp_code(code)?,
            Box::new(need(f1(), "factor 1")?),
            Box::new(need(f2(), "factor 2")?),
        ))
    };
    let what = match op.as_str() {
        "" => return Ok(()),
        "EVAL" | "EVALR" => {
            let (t, v) = split_assign(ext2)?;
            Line::Stmt(Stmt::Eval {
                target: t,
                value: v,
                ext: ext.clone(),
                right: op == "EVALR",
            })
        }
        "IF" => Line::If(need(xf2(), "a condition")?),
        "ELSEIF" => Line::ElseIf(need(xf2(), "a condition")?),
        "DOW" => Line::Dow(need(xf2(), "a condition")?),
        "DOU" => Line::Dou(need(xf2(), "a condition")?),
        "WHEN" => Line::When(need(xf2(), "a condition")?),
        "FOR" => free_for(ext2)?,
        "ELSE" => Line::Else,
        "ENDIF" => Line::EndIf,
        "ENDDO" => Line::EndDo,
        "ENDFOR" => Line::EndFor,
        "ENDSL" => Line::EndSl,
        "END" | "ENDCS" => Line::End,
        "SELECT" => Line::Select,
        "OTHER" => Line::Other,
        "ITER" => Line::Iter,
        "LEAVE" => Line::Leave,
        "LEAVESR" => Line::LeaveSr,
        "BEGSR" => Line::BegSr(f1t.to_ascii_uppercase()),
        "ENDSR" => Line::EndSr,
        "EXSR" => Line::ExSr(f2t.to_ascii_uppercase()),
        "RETURN" => Line::Return(xf2()?),
        "GOTO" => Line::Goto(f2t.to_ascii_uppercase()),
        "TAG" => Line::Tag(f1t.to_ascii_uppercase()),
        "MONITOR" => Line::Monitor,
        "ON-ERROR" => Line::OnError(
            ext2.split(':')
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        "ENDMON" => Line::EndMon,
        "DO" => Line::Do {
            start: f1()?,
            limit: f2()?,
            index: res()?,
        },
        "CALLP" => Line::Stmt(Stmt::CallP(need(xf2(), "a call")?)),
        "CALL" => {
            let parms = match res()? {
                Some(Expr::Name { parts, .. }) => {
                    let n = parts.join(".");
                    ctx.src
                        .plists
                        .iter()
                        .find(|p| p.0 == n)
                        .map(|p| p.1.clone())
                        .unwrap_or_default()
                }
                _ => vec![],
            };
            let l = Line::Stmt(Stmt::Call {
                program: need(f2(), "a program")?,
                parms,
                ind: ind.clone(),
            });
            let line = CalcLine {
                line: lineno,
                cond: cond.clone(),
                level: level.clone(),
                what: l,
            };
            ctx.calcs().push(line);
            ctx.call = Some(ctx.calcs().len() - 1);
            return Ok(());
        }
        "READ" => file_op(FileOp::Read)?,
        "READE" => file_op(FileOp::ReadE)?,
        "READP" => file_op(FileOp::ReadP)?,
        "READPE" => file_op(FileOp::ReadPE)?,
        "CHAIN" => file_op(FileOp::Chain)?,
        "SETLL" => file_op(FileOp::SetLL)?,
        "SETGT" => file_op(FileOp::SetGT)?,
        "WRITE" => file_op(FileOp::Write)?,
        "UPDATE" => file_op(FileOp::Update)?,
        "DELETE" => file_op(FileOp::Delete)?,
        "UNLOCK" => file_op(FileOp::Unlock)?,
        "OPEN" => file_op(FileOp::Open)?,
        "CLOSE" => file_op(FileOp::Close)?,
        "EXFMT" => file_op(FileOp::Exfmt)?,
        "READC" => file_op(FileOp::ReadC)?,
        "FEOD" => file_op(FileOp::Feod)?,
        "DSPLY" => Line::Stmt(Stmt::Dsply {
            message: f1()?,
            response: res()?,
        }),
        "SETON" | "SETOF" | "SETOFF" => Line::Stmt(Stmt::SetInd {
            on: op == "SETON",
            inds: [ind.hi.clone(), ind.lo.clone(), ind.eq.clone()]
                .into_iter()
                .flatten()
                .collect(),
        }),
        "Z-ADD" | "Z-SUB" | "ADD" | "SUB" | "MULT" | "DIV" | "MVR" => Line::Stmt(Stmt::Arith {
            op: match op.as_str() {
                "Z-ADD" => Arith::ZAdd,
                "Z-SUB" => Arith::ZSub,
                "ADD" => Arith::Add,
                "SUB" => Arith::Sub,
                "MULT" => Arith::Mult,
                "DIV" => Arith::Div,
                _ => Arith::Mvr,
            },
            f1: f1()?,
            f2: if op == "MVR" {
                Expr::Num(Default::default())
            } else {
                need(f2(), "factor 2")?
            },
            result: need(res(), "a result field")?,
            ext: ext.clone(),
            ind: ind.clone(),
        }),
        "MOVE" | "MOVEL" => Line::Stmt(Stmt::Move {
            left: op == "MOVEL",
            pad: ext.other.contains('P'),
            from: need(f2(), "factor 2")?,
            to: need(res(), "a result field")?,
            ind: ind.clone(),
        }),
        "COMP" => Line::Stmt(Stmt::Comp {
            f1: need(f1(), "factor 1")?,
            f2: need(f2(), "factor 2")?,
            ind: ind.clone(),
        }),
        "CLEAR" => Line::Stmt(Stmt::Clear(need(res(), "a result field")?)),
        "RESET" => Line::Stmt(Stmt::Reset(need(res(), "a result field")?)),
        "DEFINE" | "COMMIT" | "ROLBK" | "FORCE" | "DUMP" => Line::Stmt(Stmt::Op {
            code: op.clone(),
            f1: f1()?,
            f2: f2()?,
            result: res()?,
            ext: ext.clone(),
            ind: ind.clone(),
        }),
        "CAT" | "SUBST" | "SCAN" | "CHECK" | "CHECKR" | "XLATE" | "LOOKUP" | "TIME" | "IN"
        | "OUT" | "MOVEA" | "XFOOT" | "SQRT" | "TESTN" | "TESTB" | "BITON" | "BITOFF"
        | "EXCEPT" | "OCCUR" | "SORTA" | "DEALLOC" => {
            let f2v = if op == "EXCEPT" { None } else { f2()? };
            Line::Stmt(Stmt::Op {
                code: op.clone(),
                f1: f1()?,
                f2: f2v,
                result: res()?,
                ext: ext.clone(),
                ind: ind.clone(),
            })
            .with_except_name(if op == "EXCEPT" {
                Some(f2t.to_ascii_uppercase())
            } else {
                None
            })
        }
        _ => {
            // IFxx, DOWxx, DOUxx, WHENxx, CABxx: comparisons in the opcode.
            if let Some(code) = op.strip_prefix("IF") {
                Line::If(cmp_line(code)?)
            } else if let Some(code) = op.strip_prefix("DOW") {
                Line::Dow(cmp_line(code)?)
            } else if let Some(code) = op.strip_prefix("DOU") {
                Line::Dou(cmp_line(code)?)
            } else if let Some(code) = op.strip_prefix("WHEN") {
                Line::When(cmp_line(code)?)
            } else if let Some(code) = op.strip_prefix("CAB") {
                Line::Cab {
                    cmp: if code.is_empty() {
                        None
                    } else {
                        Some(cmp_code(code)?)
                    },
                    f1: need(f1(), "factor 1")?,
                    f2: need(f2(), "factor 2")?,
                    tag: rt.to_ascii_uppercase(),
                    ind: ind.clone(),
                }
            } else {
                Line::Stmt(Stmt::Unsupported(op.clone()))
            }
        }
    };
    if !cond.is_empty() && !matches!(what, Line::Stmt(_)) {
        // Conditioning indicators on a structured operation: kept for the
        // statement form only.
        if !matches!(
            what,
            Line::Goto(_)
                | Line::ExSr(_)
                | Line::Return(_)
                | Line::Cab { .. }
                | Line::Iter
                | Line::Leave
                | Line::LeaveSr
        ) {
            cond.clear();
        }
    }
    ctx.calc(lineno, cond, level, what);
    Ok(())
}

trait ExceptName {
    fn with_except_name(self, name: Option<String>) -> Line;
}

impl ExceptName for Line {
    fn with_except_name(self, name: Option<String>) -> Line {
        match (self, name) {
            (
                Line::Stmt(Stmt::Op {
                    code,
                    f1,
                    result,
                    ext,
                    ind,
                    ..
                }),
                Some(n),
            ) if !n.is_empty() => Line::Stmt(Stmt::Op {
                code,
                f1,
                f2: Some(Expr::name(&n)),
                result,
                ext,
                ind,
            }),
            (l, _) => l,
        }
    }
}

/// `target = value`, or `target += value` and the other compound forms.
fn split_assign(text: &str) -> Result<(Expr, Expr)> {
    let toks = expr::tokens(text)?;
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Op("(") => depth += 1,
            Tok::Op(")") => depth -= 1,
            Tok::Op(op @ ("=" | "+=" | "-=" | "*=" | "/=")) if depth == 0 => {
                let mut lp = Parser::new(toks[..i].to_vec());
                let target = lp.expr()?;
                if !lp.at_end() {
                    bail!("the target of an assignment is not valid: {text}");
                }
                let mut rp = Parser::new(toks[i + 1..].to_vec());
                let mut value = rp.expr()?;
                if !rp.at_end() {
                    bail!("unexpected {:?} in: {text}", rp.peek());
                }
                let bin = match *op {
                    "+=" => Some(BinOp::Add),
                    "-=" => Some(BinOp::Sub),
                    "*=" => Some(BinOp::Mul),
                    "/=" => Some(BinOp::Div),
                    _ => None,
                };
                if let Some(b) = bin {
                    value = Expr::Bin(b, Box::new(target.clone()), Box::new(value));
                }
                return Ok((target, value));
            }
            _ => {}
        }
    }
    bail!("an assignment needs '=': {text}")
}

/// FOR i = a TO|DOWNTO b BY c.
fn free_for(text: &str) -> Result<Line> {
    let up = text.to_ascii_uppercase();
    let find_word = |w: &str| -> Option<usize> {
        let pat = format!(" {w} ");
        let mut quoted = false;
        let b = up.as_bytes();
        for i in 0..up.len() {
            if b[i] == b'\'' {
                quoted = !quoted;
            }
            if !quoted && up[i..].starts_with(&pat) {
                return Some(i);
            }
        }
        None
    };
    let to_at = find_word("TO");
    let down_at = find_word("DOWNTO");
    let by_at = find_word("BY");
    let first_end = [to_at, down_at, by_at]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let (var, from) = split_assign(&text[..first_end])?;
    let seg = |start: Option<usize>, kw: usize| -> Result<Option<Expr>> {
        let Some(s) = start else { return Ok(None) };
        let begin = s + kw + 2;
        let end = [to_at, down_at, by_at]
            .into_iter()
            .flatten()
            .filter(|&x| x > s)
            .min()
            .unwrap_or(text.len());
        Ok(Some(expr::parse(&text[begin..end])?))
    };
    let (to, down) = match (to_at, down_at) {
        (_, Some(d)) => (seg(Some(d), 6)?, true),
        (Some(t), None) => (seg(Some(t), 2)?, false),
        _ => (None, false),
    };
    Ok(Line::For {
        var,
        from,
        to,
        by: seg(by_at, 2)?,
        down,
    })
}

fn fixed_o(ctx: &mut Ctx, cs: &[char], lineno: usize) -> Result<()> {
    let ch = |p: usize| cs.get(p - 1).copied().unwrap_or(' ').to_ascii_uppercase();
    let file = colt(cs, 7, 16).to_ascii_uppercase();
    let and_or = colt(cs, 16, 18).to_ascii_uppercase();
    let mut cond: Conditions = Vec::new();
    for start in [21, 24, 27] {
        let n = ch(start) == 'N';
        if let Some(i) = indicator(&col(cs, start + 1, start + 2)) {
            cond.push((n, i));
        }
    }
    let kind = ch(17);
    // A record line has its type (H, D, T, E) in position 17; field
    // lines leave positions 16-20 blank.
    if !(and_or == "AND" || and_or == "OR")
        && (!file.is_empty() || matches!(kind, 'H' | 'D' | 'T' | 'E'))
    {
        let num = |a: usize, b: usize| colt(cs, a, b).parse::<u32>().ok();
        let file = if file.is_empty() {
            ctx.src
                .outputs
                .last()
                .map(|o| o.file.clone())
                .unwrap_or_default()
        } else {
            file
        };
        ctx.src.outputs.push(OutRec {
            file,
            kind,
            fetch_overflow: ch(18) == 'F',
            cond: vec![cond],
            name: indicator(&col(cs, 30, 39)),
            space_before: num(40, 42),
            space_after: num(43, 45),
            skip_before: num(46, 48),
            skip_after: num(49, 51),
            fields: vec![],
            line: lineno,
        });
        return Ok(());
    }
    if and_or == "AND" || and_or == "OR" {
        if let Some(o) = ctx.src.outputs.last_mut() {
            if and_or == "OR" {
                o.cond.push(cond);
            } else if let Some(g) = o.cond.last_mut() {
                g.extend(cond);
            }
        }
        return Ok(());
    }
    let rec = ctx
        .src
        .outputs
        .last_mut()
        .ok_or_else(|| anyhow!("an output field needs a record line first"))?;
    let name = colt(cs, 30, 43);
    let endt = colt(cs, 47, 51);
    let constant = col(cs, 53, 80);
    let (end, relative) = if let Some(r) = endt.strip_prefix('+') {
        (None, r.trim().parse().ok())
    } else {
        (endt.parse().ok(), None)
    };
    let literal = {
        let t = constant.trim();
        if t.starts_with('\'') && name.is_empty() {
            let inner = t.trim_start_matches('\'');
            let inner = inner.strip_suffix('\'').unwrap_or(inner);
            Some(e(&inner.replace("''", "'")))
        } else {
            None
        }
    };
    let edit_word = if !name.is_empty() && constant.trim().starts_with('\'') {
        Some(constant.trim().trim_matches('\'').to_string())
    } else {
        None
    };
    rec.fields.push(OutField {
        cond,
        name: if name.is_empty() {
            None
        } else {
            Some(name.to_ascii_uppercase())
        },
        literal,
        edit_code: Some(ch(44)).filter(|c| *c != ' '),
        blank_after: ch(45) == 'B',
        end,
        relative,
        edit_word,
    });
    Ok(())
}

fn fixed_i(ctx: &mut Ctx, cs: &[char], _lineno: usize) -> Result<()> {
    let file = colt(cs, 7, 16).to_ascii_uppercase();
    if !file.is_empty() {
        ctx.src.inputs.push(InRec {
            file,
            record_ind: indicator(&col(cs, 21, 22)),
            fields: vec![],
        });
        return Ok(());
    }
    let rec = ctx
        .src
        .inputs
        .last_mut()
        .ok_or_else(|| anyhow!("an input field needs a record line first"))?;
    let name = colt(cs, 49, 62).to_ascii_uppercase();
    if name.is_empty() {
        return Ok(());
    }
    let from: u32 = colt(cs, 37, 41)
        .parse()
        .map_err(|_| anyhow!("input field from position"))?;
    let to: u32 = colt(cs, 42, 46)
        .parse()
        .map_err(|_| anyhow!("input field to position"))?;
    let decs = colt(cs, 47, 48).parse().ok();
    let fmt = cs.get(35).copied().unwrap_or(' ').to_ascii_uppercase();
    rec.fields.push((name, from, to, decs, fmt));
    Ok(())
}

// ---- free form -------------------------------------------------------

const FREE_OPS: &[&str] = &[
    "IF",
    "ELSEIF",
    "ELSE",
    "ENDIF",
    "DOW",
    "DOU",
    "ENDDO",
    "FOR",
    "ENDFOR",
    "SELECT",
    "WHEN",
    "OTHER",
    "ENDSL",
    "ITER",
    "LEAVE",
    "LEAVESR",
    "BEGSR",
    "ENDSR",
    "EXSR",
    "RETURN",
    "EVAL",
    "EVALR",
    "EVAL-CORR",
    "CALLP",
    "READ",
    "READE",
    "READP",
    "READPE",
    "CHAIN",
    "SETLL",
    "SETGT",
    "WRITE",
    "UPDATE",
    "DELETE",
    "UNLOCK",
    "OPEN",
    "CLOSE",
    "EXFMT",
    "READC",
    "FEOD",
    "DSPLY",
    "CLEAR",
    "RESET",
    "MONITOR",
    "ON-ERROR",
    "ENDMON",
    "EXCEPT",
    "IN",
    "OUT",
    "SORTA",
    "DEALLOC",
    "COMMIT",
    "ROLBK",
    "DUMP",
    "FORCE",
    "TEST",
    "POST",
    "ACQ",
    "REL",
    "NEXT",
    "XML-INTO",
    "XML-SAX",
    "DATA-INTO",
    "DATA-GEN",
    "SND-MSG",
    "ON-EXIT",
    "ENDON-EXIT",
];

/// The first word of a free-form statement and its operation extender:
/// `CHAIN(N) key file` → ("CHAIN", "N", rest).
fn free_op(text: &str) -> Option<(String, String, String)> {
    let t = text.trim_start();
    let word: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let up = word.to_ascii_uppercase();
    if !FREE_OPS.contains(&up.as_str()) {
        return None;
    }
    let rest = &t[word.len()..];
    // An extender: (H), (E), (N), (HE)...: letters only.
    if let Some(r) = rest.strip_prefix('(') {
        if let Some(close) = r.find(')') {
            let inside = &r[..close];
            if !inside.is_empty()
                && inside.len() <= 4
                && inside.chars().all(|c| "HNEMRDTZPAhnemrdtzpa".contains(c))
            {
                return Some((up, inside.to_ascii_uppercase(), r[close + 1..].to_string()));
            }
        }
    }
    // An assignment to a field named like an opcode (`in = 5`)?
    if rest.trim_start().starts_with('=') && !rest.trim_start().starts_with("==") {
        return None;
    }
    Some((up, String::new(), rest.to_string()))
}

fn words(text: &str) -> Result<Vec<Tok>> {
    expr::tokens(text)
}

fn free(ctx: &mut Ctx, text: &str, lineno: usize) -> Result<()> {
    let up = text.trim_start().to_ascii_uppercase();
    let first: String = up
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let rest = text.trim_start()[first.len()..].trim().to_string();
    // Declarations.
    match first.as_str() {
        "CTL-OPT" => {
            ctx.src.control.extend(keywords(&rest)?);
            return Ok(());
        }
        "DCL-F" => {
            let (name, _, kws) = free_decl(&rest)?;
            let device = ["DISK", "WORKSTN", "PRINTER", "SEQ", "SPECIAL"]
                .into_iter()
                .find(|d| keyword(&kws, d).is_some())
                .unwrap_or("DISK")
                .to_string();
            let usage: Vec<String> = keyword(&kws, "USAGE")
                .map(|k| {
                    k.args
                        .iter()
                        .map(|a| a.trim().to_ascii_uppercase())
                        .collect()
                })
                .unwrap_or_else(|| match device.as_str() {
                    "WORKSTN" => vec!["*INPUT".into(), "*OUTPUT".into()],
                    "PRINTER" => vec!["*OUTPUT".into()],
                    _ => vec!["*INPUT".into()],
                });
            let has = |u: &str| usage.iter().any(|x| x == u);
            let u = if device == "WORKSTN" && has("*INPUT") && has("*OUTPUT") {
                'C'
            } else if has("*UPDATE") || has("*DELETE") {
                'U'
            } else if has("*INPUT") {
                'I'
            } else {
                'O'
            };
            let program_described = keyword(&kws, &device)
                .and_then(|k| k.args.first())
                .and_then(|a| a.trim().parse::<u32>().ok());
            ctx.src.files.push(FileDecl {
                name,
                usage: u,
                designation: 'F',
                add: has("*OUTPUT") && u != 'O' && u != 'C',
                external: program_described.is_none(),
                keyed: keyword(&kws, "KEYED").is_some(),
                record_len: program_described,
                device,
                keywords: kws,
                line: lineno,
            });
            return Ok(());
        }
        "DCL-S" | "DCL-C" => {
            let (name, dtype, kws) = if first == "DCL-C" {
                // DCL-C NAME 'value' or CONST('value').
                let mut it = rest.splitn(2, char::is_whitespace);
                let n = it.next().unwrap_or("").to_ascii_uppercase();
                let v = it.next().unwrap_or("").trim().to_string();
                let kws = if v.to_ascii_uppercase().starts_with("CONST(") {
                    keywords(&v)?
                } else {
                    vec![Keyword {
                        name: "CONST".into(),
                        args: vec![v],
                    }]
                };
                (n, None, kws)
            } else {
                free_decl(&rest)?
            };
            ctx.add_def(Def {
                name,
                kind: if first == "DCL-C" {
                    DefKind::Const
                } else {
                    DefKind::Field
                },
                dtype,
                from: None,
                to: None,
                external: false,
                keywords: kws,
                line: lineno,
            });
            return Ok(());
        }
        "DCL-DS" | "DCL-PR" | "DCL-PI" => {
            let (name, dtype, mut kws) = free_decl(&rest)?;
            // `dcl-pi *n int(10) end-pi;`: an empty list ends on its line.
            let ends_here = kws.iter().any(|k| {
                matches!(k.name.as_str(), "END-DS" | "END-PR" | "END-PI") && k.args.is_empty()
            });
            kws.retain(|k| !matches!(k.name.as_str(), "END-DS" | "END-PR" | "END-PI"));
            let kind = match first.as_str() {
                "DCL-DS" => DefKind::Ds,
                "DCL-PR" => DefKind::Proto,
                _ => DefKind::Interface,
            };
            // No END-DS after LIKEDS/LIKEREC; a DS with EXTNAME and no
            // subfields still has END-DS... unless written on one line.
            let single = kind == DefKind::Ds
                && (keyword(&kws, "LIKEDS").is_some() || keyword(&kws, "LIKEREC").is_some());
            ctx.add_def(Def {
                name: if name == "*N" { String::new() } else { name },
                kind: kind.clone(),
                dtype,
                from: None,
                to: None,
                external: keyword(&kws, "EXTNAME").is_some(),
                keywords: kws,
                line: lineno,
            });
            ctx.free_block = if single || ends_here {
                None
            } else {
                Some(kind)
            };
            return Ok(());
        }
        "END-DS" | "END-PR" | "END-PI" => {
            ctx.free_block = None;
            return Ok(());
        }
        "DCL-SUBF" | "DCL-PARM" => {
            let (name, dtype, kws) = free_decl(&rest)?;
            return sub_def(ctx, name, dtype, kws, lineno);
        }
        "DCL-PROC" => {
            let (name, _, kws) = free_decl(&rest)?;
            if ctx.proc.is_some() {
                bail!("procedure {name} begins inside another procedure");
            }
            ctx.proc = Some(Proc {
                name,
                export: keyword(&kws, "EXPORT").is_some(),
                defs: vec![],
                calcs: vec![],
                line: lineno,
            });
            return Ok(());
        }
        "END-PROC" => {
            let p = ctx
                .proc
                .take()
                .ok_or_else(|| anyhow!("END-PROC without DCL-PROC"))?;
            ctx.src.procs.push(p);
            return Ok(());
        }
        _ => {}
    }
    // Subfields and parameters inside a free-form DS/PR/PI.
    if ctx.free_block.is_some() && free_op(text).is_none() {
        let (name, dtype, kws) = free_decl(text)?;
        return sub_def(ctx, name, dtype, kws, lineno);
    }
    let what = free_calc(text)?;
    ctx.calc(lineno, vec![], None, what);
    Ok(())
}

fn sub_def(
    ctx: &mut Ctx,
    name: String,
    dtype: Option<DType>,
    kws: Vec<Keyword>,
    lineno: usize,
) -> Result<()> {
    let kind = match ctx.free_block {
        Some(DefKind::Ds) => DefKind::Subfield,
        Some(DefKind::Proto) | Some(DefKind::Interface) => DefKind::Param,
        _ => bail!("{name}: a subfield or parameter outside DCL-DS, DCL-PR or DCL-PI"),
    };
    let from = keyword(&kws, "POS")
        .and_then(|k| k.args.first())
        .and_then(|a| a.trim().parse().ok());
    ctx.add_def(Def {
        name,
        kind,
        dtype,
        from,
        to: None,
        external: false,
        keywords: kws,
        line: lineno,
    });
    Ok(())
}

/// A free-form calculation.
fn free_calc(text: &str) -> Result<Line> {
    let Some((op, x, rest)) = free_op(text) else {
        // An assignment, or a procedure call.
        let toks = words(text)?;
        let has_assign = {
            let mut depth = 0;
            toks.iter().any(|t| match t {
                Tok::Op("(") => {
                    depth += 1;
                    false
                }
                Tok::Op(")") => {
                    depth -= 1;
                    false
                }
                Tok::Op("=" | "+=" | "-=" | "*=" | "/=") => depth == 0,
                _ => false,
            })
        };
        if has_assign {
            let (t, v) = split_assign(text)?;
            return Ok(Line::Stmt(Stmt::Eval {
                target: t,
                value: v,
                ext: Ext::default(),
                right: false,
            }));
        }
        return Ok(Line::Stmt(Stmt::CallP(expr::parse(text)?)));
    };
    let ext = Ext::parse(&x);
    let rest_t = rest.trim();
    let e_opt = || opt_expr(rest_t);
    let need = |what: &str| -> Result<Expr> {
        opt_expr(rest_t)?.ok_or_else(|| anyhow!("{op}: {what} is needed"))
    };
    // File operations: [key] name [result].
    let file_stmt = |fop: FileOp, keyed: bool| -> Result<Line> {
        let toks = words(rest_t)?;
        let mut p = Parser::new(toks);
        let key = if keyed && !p.at_end() && p.rest().len() > 1 {
            // (k1: k2) is a key list; otherwise one expression.
            if matches!(p.peek(), Some(Tok::Op("("))) {
                let mut list = Vec::new();
                p.expect_op("(")?;
                loop {
                    list.push(p.expr()?);
                    if matches!(p.peek(), Some(Tok::Op(":"))) {
                        p.expect_op(":")?;
                        continue;
                    }
                    p.expect_op(")")?;
                    break;
                }
                Some(list)
            } else {
                Some(vec![p.expr()?])
            }
        } else {
            None
        };
        let file = match p.expr()? {
            Expr::Name { parts, .. } => parts.join("."),
            other => bail!("{op}: a file or record format name is needed, found {other:?}"),
        };
        let ds = if p.at_end() {
            None
        } else {
            match p.expr()? {
                Expr::Name { parts, .. } => Some(parts.join(".")),
                Expr::Bif(_, _) => None,
                other => bail!("{op}: a data structure name is needed, found {other:?}"),
            }
        };
        Ok(Line::Stmt(Stmt::File {
            op: fop,
            file,
            key,
            ds,
            ext: ext.clone(),
            ind: ResultInd::default(),
        }))
    };
    Ok(match op.as_str() {
        "IF" => Line::If(need("a condition")?),
        "ELSEIF" => Line::ElseIf(need("a condition")?),
        "ELSE" => Line::Else,
        "ENDIF" => Line::EndIf,
        "DOW" => Line::Dow(need("a condition")?),
        "DOU" => Line::Dou(need("a condition")?),
        "ENDDO" => Line::EndDo,
        "FOR" => free_for(rest_t)?,
        "ENDFOR" => Line::EndFor,
        "SELECT" => Line::Select,
        "WHEN" => Line::When(need("a condition")?),
        "OTHER" => Line::Other,
        "ENDSL" => Line::EndSl,
        "ITER" => Line::Iter,
        "LEAVE" => Line::Leave,
        "LEAVESR" => Line::LeaveSr,
        "BEGSR" => Line::BegSr(rest_t.to_ascii_uppercase()),
        "ENDSR" => Line::EndSr,
        "EXSR" => Line::ExSr(rest_t.to_ascii_uppercase()),
        "RETURN" => Line::Return(e_opt()?),
        "MONITOR" => Line::Monitor,
        "ON-ERROR" => Line::OnError(
            rest_t
                .split(':')
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        "ENDMON" => Line::EndMon,
        "EVAL" | "EVALR" => {
            let (t, v) = split_assign(rest_t)?;
            Line::Stmt(Stmt::Eval {
                target: t,
                value: v,
                ext,
                right: op == "EVALR",
            })
        }
        "CALLP" => Line::Stmt(Stmt::CallP(need("a call")?)),
        "READ" => file_stmt(FileOp::Read, false)?,
        "READP" => file_stmt(FileOp::ReadP, false)?,
        "READE" => file_stmt(FileOp::ReadE, true)?,
        "READPE" => file_stmt(FileOp::ReadPE, true)?,
        "CHAIN" => file_stmt(FileOp::Chain, true)?,
        "SETLL" => file_stmt(FileOp::SetLL, true)?,
        "SETGT" => file_stmt(FileOp::SetGT, true)?,
        "DELETE" => file_stmt(FileOp::Delete, true)?,
        "WRITE" => file_stmt(FileOp::Write, false)?,
        "UPDATE" => file_stmt(FileOp::Update, false)?,
        "UNLOCK" => file_stmt(FileOp::Unlock, false)?,
        "OPEN" => file_stmt(FileOp::Open, false)?,
        "CLOSE" => file_stmt(FileOp::Close, false)?,
        "EXFMT" => file_stmt(FileOp::Exfmt, false)?,
        "READC" => file_stmt(FileOp::ReadC, false)?,
        "FEOD" => file_stmt(FileOp::Feod, false)?,
        "DSPLY" => {
            let toks = words(rest_t)?;
            let mut p = Parser::new(toks);
            let message = if p.at_end() { None } else { Some(p.expr()?) };
            let _queue = if p.at_end() { None } else { Some(p.expr()?) };
            let response = if p.at_end() { None } else { Some(p.expr()?) };
            Line::Stmt(Stmt::Dsply { message, response })
        }
        "CLEAR" | "RESET" => {
            let t = rest_t
                .split_whitespace()
                .filter(|w| !w.eq_ignore_ascii_case("*NOKEY") && !w.eq_ignore_ascii_case("*ALL"))
                .collect::<Vec<_>>()
                .join(" ");
            let target = expr::parse(&t)?;
            Line::Stmt(if op == "CLEAR" {
                Stmt::Clear(target)
            } else {
                Stmt::Reset(target)
            })
        }
        "EXCEPT" => Line::Stmt(Stmt::Op {
            code: op.clone(),
            f1: None,
            f2: if rest_t.is_empty() {
                None
            } else {
                Some(Expr::name(rest_t))
            },
            result: None,
            ext,
            ind: ResultInd::default(),
        }),
        "IN" | "OUT" => {
            let lock = rest_t.to_ascii_uppercase().starts_with("*LOCK");
            let name = rest_t
                .trim_start_matches(|c: char| c == '*' || c.is_ascii_alphabetic() && lock)
                .trim();
            let name = if lock { rest_t[5..].trim() } else { name };
            Line::Stmt(Stmt::Op {
                code: op.clone(),
                f1: if lock {
                    Some(Expr::name("*LOCK"))
                } else {
                    None
                },
                f2: Some(expr::parse(name)?),
                result: None,
                ext,
                ind: ResultInd::default(),
            })
        }
        other => Line::Stmt(Stmt::Op {
            code: other.to_string(),
            f1: None,
            f2: e_opt().ok().flatten(),
            result: None,
            ext,
            ind: ResultInd::default(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Source {
        parse(src, &|_| None).unwrap()
    }

    #[test]
    fn fixed_form_specs() {
        let s = p("     FCUSTMAST  IF   E           K DISK\n     D msg             S             52\n     D n               S              5  0\n     C     key           chain     CUSTMAST                           50\n     C                   eval      msg = 'Hi' + %trim(x)\n     C                   seton                                        lr\n");
        assert_eq!(s.files[0].name, "CUSTMAST");
        assert!(s.files[0].external && s.files[0].keyed && s.files[0].usage == 'I');
        assert_eq!(s.defs[0].dtype, Some(DType::Char(52)));
        // A numeric standalone field without a type is packed.
        assert_eq!(s.defs[1].dtype, Some(DType::Packed(5, 0)));
        assert!(
            matches!(&s.calcs[0].what, Line::Stmt(Stmt::File { op: FileOp::Chain, ind, .. }) if ind.hi.as_deref() == Some("50"))
        );
        assert!(
            matches!(&s.calcs[2].what, Line::Stmt(Stmt::SetInd { on: true, inds }) if inds == &vec!["LR".to_string()])
        );
    }

    #[test]
    fn output_specs() {
        // ILE RPG Reference, output specifications: record type in 17,
        // conditioning indicators in 21-29 (N negates), EXCEPT name in
        // 30-39, space before/after in 40-45, skip before/after in 46-51;
        // an OR line starts another group of conditions; field end
        // position in 47-51, constant in 53-80.
        let s = p("     OQSYSPRT   E            HEADING        2  1\n     O          D    10N11                  1\n     O         OR    12\n     O               50                          84 '*'\n");
        let h = &s.outputs[0];
        assert_eq!((h.kind, h.name.as_deref()), ('E', Some("HEADING")));
        assert_eq!((h.space_before, h.space_after), (None, Some(2)));
        assert_eq!((h.skip_before, h.skip_after), (Some(1), None));
        let d = &s.outputs[1];
        assert_eq!(d.file, "QSYSPRT");
        assert_eq!(
            d.cond,
            vec![
                vec![(false, "10".to_string()), (true, "11".to_string())],
                vec![(false, "12".to_string())]
            ]
        );
        let f = &d.fields[0];
        assert_eq!(f.cond, vec![(false, "50".to_string())]);
        assert_eq!((f.end, f.literal.as_deref()), (Some(84), Some(&e("*")[..])));
    }

    #[test]
    fn free_form() {
        let s = p("**FREE\nctl-opt dftactgrp(*no);\ndcl-f CUSTMAST keyed;\ndcl-s total packed(11:2) inz(0);\ndcl-ds rec qualified;\n  id zoned(5);\n  name char(30);\nend-ds;\n// comment\ntotal += 1; // trailing\nchain (a: b) CUSTMAST;\nif %found and total > 0;\n  dsply 'yes';\nendif;\n*inlr = *on;\n");
        assert_eq!(s.files[0].usage, 'I');
        assert_eq!(s.defs[0].dtype, Some(DType::Packed(11, 2)));
        assert_eq!(s.defs[1].kind, DefKind::Ds);
        assert_eq!(s.defs[2].kind, DefKind::Subfield);
        assert_eq!(s.defs[2].dtype, Some(DType::Zoned(5, 0)));
        assert!(
            matches!(&s.calcs[1].what, Line::Stmt(Stmt::File { op: FileOp::Chain, key: Some(k), .. }) if k.len() == 2)
        );
        assert!(matches!(s.calcs[2].what, Line::If(_)));
    }

    #[test]
    fn mixed_free_in_fixed() {
        // Free-form calculations in positions 8-80 (no /FREE): IBM i 7.1
        // TR7 and later.
        let s = p("     FNOOB      CF   E             WORKSTN\n       EXSR LOADSFL;\n       dou (*inkc = *on);\n       enddo;\n     C     LOADSFL       BEGSR\n     C                   ENDSR\n");
        assert_eq!(s.calcs.len(), 5);
        assert_eq!(s.calcs[0].what, Line::ExSr("LOADSFL".into()));
    }

    #[test]
    fn copy_and_conditions() {
        let inc = |id: &str| {
            if id == "QRPGLESRC,DEFS" {
                Some("     D X               S              3  0\n".to_string())
            } else {
                None
            }
        };
        let s = parse("      /COPY QRPGLESRC,DEFS\n      /IF DEFINED(NOPE)\n     D Y               S              3  0\n      /ENDIF\n", &inc).unwrap();
        assert_eq!(s.defs.len(), 1);
        assert_eq!(s.defs[0].name, "X");
        assert!(parse("      /COPY MISSING\n", &|_| None).is_err());
    }
}

#[cfg(test)]
mod corpus {
    /// Parse every .rpgle file under $RPG_CORPUS (manual check).
    #[test]
    #[ignore]
    fn parse_corpus() {
        let Ok(dir) = std::env::var("RPG_CORPUS") else {
            return;
        };
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("rpgle"))
                {
                    files.push(p);
                }
            }
        }
        files.sort();
        let (mut ok, mut bad) = (0, 0);
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let base = f.parent().unwrap().to_path_buf();
            let inc = |id: &str| -> Option<String> {
                let name = id.rsplit(',').next().unwrap_or(id);
                let mut st = vec![
                    base.clone(),
                    std::path::PathBuf::from(std::env::var("RPG_CORPUS").unwrap()),
                ];
                while let Some(d) = st.pop() {
                    for e in std::fs::read_dir(&d).ok()?.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            st.push(p);
                            continue;
                        }
                        let fname = p.file_name()?.to_str()?.to_ascii_lowercase();
                        let want = name
                            .trim_start_matches('/')
                            .rsplit('/')
                            .next()?
                            .to_ascii_lowercase();
                        if fname == want
                            || fname.starts_with(&format!("{want}."))
                            || fname.starts_with(&format!("{want}-"))
                        {
                            return std::fs::read_to_string(p).ok();
                        }
                    }
                }
                None
            };
            match super::parse(&src, &inc) {
                Ok(_) => ok += 1,
                Err(e) => {
                    bad += 1;
                    println!("FAIL {}: {e:#}", f.display());
                }
            }
        }
        println!("parsed {ok}, failed {bad}");
    }
}
