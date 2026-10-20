//! Fixed-form ILE RPG to free form. IBM i has no command for this (the
//! conversion is offered by editors and third-party tools); as4tainr's
//! CVTRPGFREE writes the converted member. Free-form statements and fixed
//! specifications can be mixed in one member (ILE RPG Reference,
//! "Free-form statements": columns 8-80, since 7.1 TR7), so what has no
//! free form stays fixed: I and O specifications, primary and secondary
//! files, total calculations (L1-L9, LR), GOTO/TAG/CABxx, CALL/PARM,
//! PLIST/KLIST/KFLD, MOVE/MOVEL, MOVEA, LOOKUP, TESTx, the bit and zone
//! operations, and operations whose free form would behave differently
//! (MVR after DIV, SUBST/CAT/XLATE without P, SCAN into an array, DSPLY
//! with a response). When nothing stays fixed the member is written as
//! `**FREE`.
//!
//! Fixed-form arithmetic (ADD, SUB, MULT, DIV, Z-ADD, Z-SUB) drops digits
//! that do not fit; the assignment it becomes ends with RNQ0103 instead.
//! The conversion reports that.

use anyhow::Result;

/// The converted member and the conversion's notes.
#[derive(Debug, Default)]
pub struct Converted {
    pub source: String,
    pub messages: Vec<String>,
    /// Lines left in fixed form.
    pub fixed_lines: usize,
}

fn pad(line: &str) -> Vec<char> {
    let mut v: Vec<char> = line.trim_end_matches('\r').chars().collect();
    if v.len() < 80 {
        v.resize(80, ' ');
    }
    v
}

fn col(cs: &[char], from: usize, to: usize) -> String {
    cs.iter()
        .skip(from - 1)
        .take(to + 1 - from)
        .collect::<String>()
}

fn colt(cs: &[char], from: usize, to: usize) -> String {
    col(cs, from, to).trim().to_string()
}

fn lower(s: &str) -> String {
    // Names and keywords in lower case; literals as written.
    let mut out = String::new();
    let mut quoted = false;
    for c in s.chars() {
        if c == '\'' {
            quoted = !quoted;
        }
        if quoted {
            out.push(c);
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// A factor that may be `value:value`: (value, second part).
fn split_colon(s: &str) -> (String, Option<String>) {
    let mut quoted = false;
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ':' if !quoted && depth == 0 => {
                return (
                    s[..i].trim().to_string(),
                    Some(s[i + 1..].trim().to_string()),
                )
            }
            _ => {}
        }
    }
    (s.trim().to_string(), None)
}

fn ind(i: &str) -> String {
    format!("*in{}", lower(i))
}

/// The comparison of an IFxx, DOWxx, DOUxx, WHENxx, ANDxx, ORxx, CASxx.
fn cmp(code: &str) -> Option<&'static str> {
    Some(match code {
        "EQ" => "=",
        "NE" => "<>",
        "LT" => "<",
        "LE" => "<=",
        "GT" => ">",
        "GE" => ">=",
        _ => return None,
    })
}

/// A structured operation open in the calculations.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Block {
    If,
    Do,
    For,
    Select,
    Cas,
    Monitor,
    /// Opened by a line left fixed: its END stays fixed too.
    Fixed,
}

#[derive(Default)]
struct Conv {
    out: Vec<String>,
    /// Definitions made by calculations (result field lengths, *LIKE
    /// DEFINE), placed before the first calculation.
    decls: Vec<String>,
    /// Where the definitions go.
    decl_at: Option<usize>,
    blocks: Vec<Block>,
    messages: Vec<String>,
    fixed: usize,
    defined: Vec<String>,
    /// Arrays (DIM): SCAN and CHECK into an array stay fixed.
    arrays: Vec<String>,
    /// The open free-form data structure, prototype or interface.
    open: Option<&'static str>,
    /// Conditions of AN/OR lines not finished yet.
    pending: Vec<String>,
    /// A DIV whose remainder a following MVR takes: kept fixed.
    div_kept: bool,
    /// Long name continued with `...`.
    long_name: String,
    ctdata: Vec<String>,
}

fn d_type(
    t: char,
    len: u32,
    decs: &str,
    by_pos: bool,
    in_ds: bool,
    varying: bool,
) -> Option<String> {
    let d = if decs.is_empty() { "0" } else { decs };
    Some(match t {
        ' ' if decs.is_empty() => {
            if varying {
                format!(
                    "varchar({})",
                    if by_pos { len.saturating_sub(2) } else { len }
                )
            } else {
                format!("char({len})")
            }
        }
        ' ' if in_ds => format!("zoned({len}:{d})"),
        ' ' => format!("packed({len}:{d})"),
        'A' if varying => format!(
            "varchar({})",
            if by_pos { len.saturating_sub(2) } else { len }
        ),
        'A' => format!("char({len})"),
        'S' => format!("zoned({len}:{d})"),
        'P' => format!("packed({}:{d})", if by_pos { len * 2 - 1 } else { len }),
        'B' => format!(
            "bindec({}:{d})",
            if by_pos {
                if len <= 2 {
                    4
                } else {
                    9
                }
            } else {
                len
            }
        ),
        'I' | 'U' => {
            let n = if by_pos {
                match len {
                    1 => 3,
                    2 => 5,
                    4 => 10,
                    _ => 20,
                }
            } else {
                len
            };
            format!("{}({n})", if t == 'I' { "int" } else { "uns" })
        }
        'F' => format!("float({len})"),
        'D' => "date".into(),
        'T' => "time".into(),
        'Z' => "timestamp".into(),
        'N' => "ind".into(),
        '*' => "pointer".into(),
        'C' => format!("ucs2({})", if by_pos { len / 2 } else { len }),
        'G' => format!("graph({})", if by_pos { len / 2 } else { len }),
        _ => return None,
    })
}

impl Conv {
    fn note(&mut self, line: usize, msg: &str) {
        self.messages.push(format!("line {line}: {msg}"));
    }

    fn indent(&self) -> String {
        "  ".repeat(self.blocks.iter().filter(|b| **b != Block::Fixed).count())
    }

    fn free(&mut self, stmt: &str) {
        let line = format!("       {}{}", self.indent(), stmt);
        self.out.push(line);
    }

    fn keep(&mut self, raw: &str) {
        self.fixed += 1;
        self.out.push(raw.trim_end().to_string());
    }

    fn close_open(&mut self) {
        if let Some(end) = self.open.take() {
            self.out.push(format!("       {end};"));
        }
    }

    // ---- H, F, D, P --------------------------------------------------

    fn h(&mut self, cs: &[char]) {
        let kw = colt(cs, 7, 80);
        if !kw.is_empty() {
            self.free(&format!("ctl-opt {};", lower(&kw)));
        }
    }

    fn f(&mut self, cs: &[char], raw: &str, line: usize) {
        let name = colt(cs, 7, 16);
        let kws = colt(cs, 44, 80);
        if name.is_empty() {
            // Keyword continuation of the file before: on its own line.
            if let Some(last) = self.out.last_mut() {
                if last.trim_start().starts_with("dcl-f") {
                    let mut l = last.trim_end_matches(';').to_string();
                    l.push(' ');
                    l.push_str(&lower(&kws));
                    l.push(';');
                    *last = l;
                    return;
                }
            }
            self.keep(raw);
            return;
        }
        let ch = |p: usize| cs[p - 1].to_ascii_uppercase();
        // Primary and secondary files have no free form.
        if matches!(ch(18), 'P' | 'S' | 'R' | 'T') {
            self.note(
                line,
                &format!(
                    "file {name}: primary, secondary, record address and table files stay fixed"
                ),
            );
            self.keep(raw);
            return;
        }
        let device = colt(cs, 36, 42).to_ascii_uppercase();
        let mut usage = Vec::new();
        match (ch(17), device.as_str()) {
            ('I', _) => usage.push("*input"),
            ('O', _) => usage.push("*output"),
            ('U', _) => {
                usage.push("*update");
                usage.push("*delete");
            }
            ('C', _) => {
                usage.push("*input");
                usage.push("*output");
            }
            _ => {}
        }
        if ch(20) == 'A' && !usage.contains(&"*output") {
            usage.push("*output");
        }
        let default_usage: &[&str] = match device.as_str() {
            "WORKSTN" => &["*input", "*output"],
            "PRINTER" => &["*output"],
            _ => &["*input"],
        };
        let mut s = format!("dcl-f {}", lower(&name));
        let reclen = colt(cs, 23, 27);
        let program_described = ch(22) == 'F';
        s.push(' ');
        if program_described {
            s.push_str(&format!("{}({})", lower(&device), reclen));
        } else {
            s.push_str(&lower(&device));
        }
        if usage != default_usage {
            s.push_str(&format!(" usage({})", usage.join(":")));
        }
        if ch(34) == 'K' {
            s.push_str(" keyed");
        }
        if !kws.is_empty() {
            s.push(' ');
            s.push_str(&lower(&kws));
        }
        s.push(';');
        self.free(&s);
    }

    fn d(&mut self, cs: &[char], line: usize) {
        let raw_name = colt(cs, 7, 21);
        if let Some(p) = raw_name.strip_suffix("...") {
            self.long_name.push_str(p);
            return;
        }
        let name = format!("{}{}", self.long_name, raw_name);
        self.long_name.clear();
        let ext = cs[21].eq_ignore_ascii_case(&'E');
        let ds_type = cs[22].to_ascii_uppercase();
        let def = colt(cs, 24, 25).to_ascii_uppercase();
        let from = colt(cs, 26, 32);
        // A special keyword (*ROUTINE runs into position 33).
        let from = if from.starts_with('*') {
            colt(cs, 26, 39)
        } else {
            from
        };
        let to = colt(cs, 33, 39);
        let t = cs[39].to_ascii_uppercase();
        let decs = colt(cs, 41, 42);
        let mut kws = colt(cs, 44, 80);
        // A keyword continuation: onto the statement before.
        if name.is_empty()
            && def.is_empty()
            && from.is_empty()
            && to.is_empty()
            && t == ' '
            && decs.is_empty()
            && !ext
        {
            if let Some(last) = self.out.last_mut() {
                let mut l = last.trim_end_matches(';').to_string();
                l.push(' ');
                l.push_str(&lower(&kws));
                l.push(';');
                *last = l;
            }
            return;
        }
        let nm = if name.is_empty() {
            "*n".to_string()
        } else {
            lower(&name)
        };
        let upper_kws = kws.to_ascii_uppercase();
        let varying = upper_kws.contains("VARYING");
        if varying {
            // VARYING becomes the VARCHAR type.
            let re: String = kws
                .split_whitespace()
                .filter(|w| !w.eq_ignore_ascii_case("VARYING"))
                .collect::<Vec<_>>()
                .join(" ");
            kws = re;
        }
        let by_pos = !from.is_empty() && !from.starts_with('*');
        let len: u32 = match (
            from.parse::<u32>(),
            to.trim_start_matches(['+', '-']).parse::<u32>(),
        ) {
            (Ok(f), Ok(t)) => t + 1 - f,
            (_, Ok(t)) => t,
            _ => 0,
        };
        let in_ds = self.open == Some("end-ds");
        // LIKE with a length change: LIKE(x : +n).
        if (to.starts_with('+') || to.starts_with('-')) && upper_kws.contains("LIKE(") {
            if let Some(p) = kws.to_ascii_uppercase().find("LIKE(") {
                let end = kws[p..].find(')').map(|e| p + e).unwrap_or(kws.len());
                kws = format!("{}{} : {}{}", &kws[..end], "", to, &kws[end..]);
            }
        }
        let typ = if len > 0 && !(to.starts_with('+') || to.starts_with('-')) {
            d_type(t, len, &decs, by_pos, in_ds || def == "DS", varying)
        } else if t != ' ' {
            d_type(t, 0, &decs, false, false, false).filter(|x| !x.contains('('))
        } else {
            None
        };
        let mut parts: Vec<String> = Vec::new();
        let kw = lower(&kws);
        if kw.contains("dim(") {
            self.arrays.push(name.to_ascii_uppercase());
        }
        match def.as_str() {
            "S" | "C" => {
                self.close_open();
                if def == "C" {
                    // CONST(value) or the value itself.
                    let v = if kw.starts_with("const(") {
                        kws[6..kws.len().saturating_sub(1)].to_string()
                    } else {
                        kws.clone()
                    };
                    self.free(&format!("dcl-c {nm} {v};"));
                    return;
                }
                parts.push(format!("dcl-s {nm}"));
                if let Some(t) = typ {
                    parts.push(t);
                }
                if !kw.is_empty() {
                    parts.push(kw);
                }
                self.free(&format!("{};", parts.join(" ")));
                self.defined.push(name.to_ascii_uppercase());
            }
            "DS" | "PR" | "PI" => {
                self.close_open();
                let kind = match def.as_str() {
                    "DS" => "ds",
                    "PR" => "pr",
                    _ => "pi",
                };
                parts.push(format!("dcl-{kind} {nm}"));
                if def != "DS" {
                    if let Some(t) = typ {
                        parts.push(t);
                    }
                } else if len > 0 {
                    parts.push(format!("len({len})"));
                }
                if ext && def == "DS" && !upper_kws.contains("EXTNAME") {
                    parts.push(format!("extname({nm})"));
                }
                match ds_type {
                    'U' => parts.push("dtaara(*auto)".into()),
                    'S' => parts.push("psds".into()),
                    _ => {}
                }
                if !kw.is_empty() {
                    parts.push(kw);
                }
                self.free(&format!("{};", parts.join(" ")));
                self.open = Some(match def.as_str() {
                    "DS" => "end-ds",
                    "PR" => "end-pr",
                    _ => "end-pi",
                });
            }
            "" => {
                // A subfield or parameter.
                let kw_word = if self.open == Some("end-ds") {
                    "dcl-subf"
                } else {
                    "dcl-parm"
                };
                // A name that is an operation code needs DCL-SUBF/DCL-PARM.
                let reserved = crate::rpg::parse::is_free_op(&name);
                let mut p = if reserved {
                    format!("{kw_word} {nm}")
                } else {
                    nm.clone()
                };
                if from.starts_with('*') {
                    // *STATUS, *ROUTINE...: the special subfield.
                    p.push(' ');
                    p.push_str(&lower(&from));
                } else {
                    if let Some(t) = typ {
                        p.push(' ');
                        p.push_str(&t);
                    }
                    if by_pos {
                        p.push_str(&format!(" pos({from})"));
                    }
                }
                if ext {
                    // A field of an external data structure.
                }
                if !kw.is_empty() {
                    p.push(' ');
                    p.push_str(&kw);
                }
                p.push(';');
                let indented = format!("         {p}");
                self.out.push(indented);
                if self.open.is_none() {
                    self.note(line, "a subfield outside a data structure");
                }
            }
            other => {
                self.note(line, &format!("definition type {other} is not converted"));
            }
        }
    }

    fn p(&mut self, cs: &[char], raw: &str) {
        self.close_open();
        let name = colt(cs, 7, 21);
        let kws = colt(cs, 44, 80);
        match cs[23].to_ascii_uppercase() {
            'B' => {
                let k = if kws.is_empty() {
                    String::new()
                } else {
                    format!(" {}", lower(&kws))
                };
                self.out
                    .push(format!("       dcl-proc {}{k};", lower(&name)));
            }
            'E' => self.out.push("       end-proc;".into()),
            _ => self.keep(raw),
        }
    }

    // ---- C ------------------------------------------------------------

    /// The conditions of positions 7-11 (with AN/OR lines before) as an
    /// expression, or None.
    fn conditions(&mut self, cs: &[char]) -> Option<String> {
        let n = cs[8].eq_ignore_ascii_case(&'N');
        let i = colt(cs, 10, 11);
        let mut c = std::mem::take(&mut self.pending);
        if !i.is_empty() {
            let join = colt(cs, 7, 8).to_ascii_uppercase();
            let one = if n {
                format!("not {}", ind(&i))
            } else {
                ind(&i)
            };
            if join == "OR" && !c.is_empty() {
                c.push("or".into());
            } else if !c.is_empty() {
                c.push("and".into());
            }
            c.push(one);
        }
        if c.is_empty() {
            None
        } else {
            Some(c.join(" "))
        }
    }

    fn c(&mut self, cs: &[char], raw: &str, line: usize, next_op: &str) {
        let level = colt(cs, 7, 8).to_ascii_uppercase();
        let opfull = colt(cs, 26, 35).to_ascii_uppercase();
        let (op, ext) = match opfull.split_once('(') {
            Some((o, e)) => (o.trim().to_string(), e.trim_end_matches(')').to_string()),
            None => (opfull.clone(), String::new()),
        };
        let f1 = colt(cs, 12, 25);
        let f2 = colt(cs, 36, 49);
        let x2 = colt(cs, 36, 80);
        let res = colt(cs, 50, 63);
        let len = colt(cs, 64, 68);
        let dec = colt(cs, 69, 70);
        let (hi, lo, eq) = (colt(cs, 71, 72), colt(cs, 73, 74), colt(cs, 75, 76));
        // Total calculations stay fixed.
        let total = level == "LR"
            || level.len() == 2
                && level.starts_with('L')
                && level[1..].chars().all(|c| c.is_ascii_digit());
        if total {
            self.note(line, "total calculations (control level) stay fixed");
            self.keep(raw);
            return;
        }
        // A line of conditions only (AN/OR follow).
        if op.is_empty() && f1.is_empty() && x2.is_empty() {
            let n = cs[8].eq_ignore_ascii_case(&'N');
            let i = colt(cs, 10, 11);
            if !i.is_empty() {
                let one = if n {
                    format!("not {}", ind(&i))
                } else {
                    ind(&i)
                };
                if !self.pending.is_empty() {
                    self.pending.push(if level == "OR" {
                        "or".into()
                    } else {
                        "and".into()
                    });
                }
                self.pending.push(one);
            }
            return;
        }
        let keep_fixed = |c: &mut Conv, why: &str| {
            c.note(line, why);
            // The conditions waiting stay as they were written: the lines
            // before were AN/OR lines, also kept.
            c.pending.clear();
            c.keep(raw);
        };
        let lf1 = lower(&f1);
        let lf2 = lower(&f2);
        let lres = lower(&res);
        let lx2 = lower(&x2);
        // A result field with a length is defined.
        if !len.is_empty() && !res.is_empty() && op != "DEFINE" {
            let u = res.to_ascii_uppercase();
            if !self.defined.contains(&u) {
                let len_n: u32 = len.parse().unwrap_or(0);
                let t = if dec.is_empty() {
                    format!("char({len_n})")
                } else {
                    format!("packed({len_n}:{dec})")
                };
                self.decls.push(format!("       dcl-s {lres} {t};"));
                self.defined.push(u);
            }
        }
        let e = if ext.is_empty() {
            String::new()
        } else {
            format!("({})", lower(&ext))
        };
        let mut stmts: Vec<String> = Vec::new();
        // Operations whose result field gets indicators: + - 0.
        let num_inds = |stmts: &mut Vec<String>, target: &str| {
            if !hi.is_empty() {
                stmts.push(format!("{} = {target} > 0;", ind(&hi)));
            }
            if !lo.is_empty() {
                stmts.push(format!("{} = {target} < 0;", ind(&lo)));
            }
            if !eq.is_empty() {
                stmts.push(format!("{} = {target} = 0;", ind(&eq)));
            }
        };
        let file = lf2.clone();
        // Structured operations.
        let structured = matches!(
            op.as_str(),
            "IF" | "ELSE"
                | "ELSEIF"
                | "ENDIF"
                | "DOW"
                | "DOU"
                | "ENDDO"
                | "FOR"
                | "ENDFOR"
                | "SELECT"
                | "WHEN"
                | "OTHER"
                | "ENDSL"
                | "END"
                | "ENDCS"
                | "MONITOR"
                | "ON-ERROR"
                | "ENDMON"
                | "BEGSR"
                | "ENDSR"
        ) || ["IF", "DOW", "DOU", "WHEN", "AND", "OR", "CAS"]
            .iter()
            .any(|p| op.starts_with(p) && op.len() == p.len() + 2 && cmp(&op[p.len()..]).is_some())
            || op == "CAS"
            || op == "DO";
        let cond = if structured {
            None
        } else {
            self.conditions(cs)
        };
        if structured && !self.pending.is_empty() {
            self.pending.clear();
        }
        match op.as_str() {
            "IF" | "DOW" | "DOU" | "WHEN" | "ELSEIF" => {
                let kw = lower(&op);
                self.free(&format!("{kw}{e} {lx2};"));
                if op != "WHEN" && op != "ELSEIF" {
                    self.blocks
                        .push(if op == "IF" { Block::If } else { Block::Do });
                }
                return;
            }
            "ELSE" | "OTHER" => {
                let save = self.blocks.pop();
                self.free(&format!("{};", lower(&op)));
                if let Some(b) = save {
                    self.blocks.push(b);
                }
                return;
            }
            "ON-ERROR" => {
                let save = self.blocks.pop();
                self.free(&format!("on-error {lx2};").replace(" ;", ";"));
                if let Some(b) = save {
                    self.blocks.push(b);
                }
                return;
            }
            "SELECT" | "MONITOR" => {
                self.free(&format!("{};", lower(&op)));
                self.blocks.push(if op == "SELECT" {
                    Block::Select
                } else {
                    Block::Monitor
                });
                return;
            }
            "ENDIF" | "ENDDO" | "ENDFOR" | "ENDSL" | "ENDMON" | "END" | "ENDCS" => {
                match self.blocks.pop() {
                    Some(Block::Fixed) => {
                        self.blocks.push(Block::Fixed);
                        self.blocks.pop();
                        self.keep(raw);
                    }
                    Some(b) => {
                        let word = match b {
                            Block::If => "endif",
                            Block::Do => "enddo",
                            Block::For => "endfor",
                            Block::Select | Block::Cas => "endsl",
                            Block::Monitor => "endmon",
                            Block::Fixed => unreachable!(),
                        };
                        self.free(&format!("{word};"));
                    }
                    None => self.keep(raw),
                }
                return;
            }
            "BEGSR" => {
                self.free(&format!("begsr {lf1};"));
                return;
            }
            "ENDSR" => {
                if !lf1.is_empty() {
                    // A TAG on ENDSR: GOTO can go there.
                    keep_fixed(self, "ENDSR with a label stays fixed");
                    return;
                }
                // The return point (factor 2) stays.
                if lf2.is_empty() {
                    self.free("endsr;");
                } else {
                    self.free(&format!("endsr {lf2};"));
                }
                return;
            }
            _ => {}
        }
        // IFxx, DOWxx, DOUxx, WHENxx, ANDxx, ORxx, CASxx.
        for p in ["IF", "DOW", "DOU", "WHEN", "AND", "OR", "CAS"] {
            if op.starts_with(p) && op.len() == p.len() + 2 {
                if let Some(c) = cmp(&op[p.len()..]) {
                    let test = format!("{lf1} {c} {lf2}");
                    match p {
                        "AND" | "OR" => {
                            // Joins the condition of the line before (when
                            // that one was converted).
                            let joins = self.out.last().is_some_and(|l| {
                                let t = l.trim_start();
                                ["if ", "dow ", "dou ", "when "]
                                    .iter()
                                    .any(|k| t.starts_with(k))
                            });
                            if !joins {
                                keep_fixed(self, &format!("{op} stays fixed with its operation"));
                                return;
                            }
                            if let Some(last) = self.out.last_mut() {
                                let l = last.trim_end_matches(';').to_string();
                                *last = format!("{l} {} {test};", lower(p));
                            }
                        }
                        "CAS" => {
                            if self.blocks.last() != Some(&Block::Cas) {
                                self.free("select;");
                                self.blocks.push(Block::Cas);
                            }
                            let save = self.blocks.pop();
                            self.free(&format!("when {test};"));
                            if let Some(b) = save {
                                self.blocks.push(b);
                            }
                            self.free(&format!("  exsr {lres};"));
                        }
                        _ => {
                            let kw = lower(p);
                            self.free(&format!("{kw} {test};"));
                            if p != "WHEN" {
                                self.blocks
                                    .push(if p == "IF" { Block::If } else { Block::Do });
                            } else {
                                // WHEN inside SELECT: no block of its own.
                            }
                        }
                    }
                    return;
                }
            }
        }
        if op == "CAS" {
            if self.blocks.last() != Some(&Block::Cas) {
                self.free("select;");
                self.blocks.push(Block::Cas);
            }
            let save = self.blocks.pop();
            self.free("other;");
            if let Some(b) = save {
                self.blocks.push(b);
            }
            self.free(&format!("  exsr {lres};"));
            return;
        }
        if op == "DO" {
            if res.is_empty() {
                keep_fixed(self, "DO without an index stays fixed");
                self.blocks.push(Block::Fixed);
                return;
            }
            let start = if lf1.is_empty() {
                "1".to_string()
            } else {
                lf1.clone()
            };
            let limit = if lf2.is_empty() {
                "1".to_string()
            } else {
                lf2.clone()
            };
            self.free(&format!("for {lres} = {start} to {limit};"));
            self.blocks.push(Block::For);
            return;
        }
        // Statements.
        match op.as_str() {
            "EVAL" | "EVALR" | "CALLP" | "EVAL-CORR" => {
                let kw = if op == "EVAL" && ext.is_empty() {
                    String::new()
                } else {
                    format!("{}{e} ", lower(&op))
                };
                stmts.push(format!("{kw}{lx2};"));
            }
            "RETURN" => stmts.push(if lx2.is_empty() {
                "return;".into()
            } else {
                format!("return {lx2};")
            }),
            "LEAVE" | "ITER" | "LEAVESR" => stmts.push(format!("{};", lower(&op))),
            "EXSR" => stmts.push(format!("exsr {lf2};")),
            "SETON" | "SETOFF" | "SETOF" => {
                let v = if op == "SETON" { "*on" } else { "*off" };
                for i in [&hi, &lo, &eq] {
                    if !i.is_empty() {
                        stmts.push(format!("{} = {v};", ind(i)));
                    }
                }
            }
            "Z-ADD" | "Z-SUB" | "ADD" | "SUB" | "MULT" | "DIV" => {
                if op == "DIV" && next_op == "MVR" {
                    self.div_kept = true;
                    keep_fixed(self, "DIV followed by MVR stays fixed");
                    return;
                }
                let target = lres.clone();
                let a = if lf1.is_empty() {
                    target.clone()
                } else {
                    lf1.clone()
                };
                let expr = match op.as_str() {
                    "Z-ADD" => lf2.clone(),
                    "Z-SUB" => format!("-({lf2})"),
                    "ADD" => format!("{a} + {lf2}"),
                    "SUB" => format!("{a} - {lf2}"),
                    "MULT" => format!("{a} * {lf2}"),
                    _ => format!("{a} / {lf2}"),
                };
                let h = if ext.to_ascii_uppercase().contains('H') {
                    "eval(h) "
                } else {
                    ""
                };
                stmts.push(format!("{h}{target} = {expr};"));
                num_inds(&mut stmts, &target);
                self.note(line, &format!("{op}: an overflowing result is an error (RNQ0103) in free form, where the fixed form dropped the high-order digits"));
            }
            "MVR" if self.div_kept => {
                self.div_kept = false;
                keep_fixed(self, "MVR stays fixed with its DIV");
                return;
            }
            "COMP" => {
                if !hi.is_empty() {
                    stmts.push(format!("{} = {lf1} > {lf2};", ind(&hi)));
                }
                if !lo.is_empty() {
                    stmts.push(format!("{} = {lf1} < {lf2};", ind(&lo)));
                }
                if !eq.is_empty() {
                    stmts.push(format!("{} = {lf1} = {lf2};", ind(&eq)));
                }
            }
            "CHAIN" | "READ" | "READE" | "READP" | "READPE" | "SETLL" | "SETGT" | "WRITE"
            | "UPDATE" | "DELETE" | "EXFMT" | "READC" | "OPEN" | "CLOSE" | "UNLOCK" | "FEOD" => {
                // The error indicator becomes the E extender and %ERROR.
                let mut x = ext.to_ascii_lowercase();
                if !lo.is_empty() && !x.contains('e') {
                    x.push('e');
                }
                let ex = if x.is_empty() {
                    String::new()
                } else {
                    format!("({x})")
                };
                let key = if lf1.is_empty()
                    || matches!(
                        op.as_str(),
                        "READ"
                            | "READP"
                            | "WRITE"
                            | "UPDATE"
                            | "EXFMT"
                            | "READC"
                            | "OPEN"
                            | "CLOSE"
                            | "UNLOCK"
                            | "FEOD"
                    ) {
                    String::new()
                } else {
                    format!("{lf1} ")
                };
                let ds = if res.is_empty() {
                    String::new()
                } else {
                    format!(" {lres}")
                };
                stmts.push(format!("{}{ex} {key}{file}{ds};", lower(&op)));
                // Resulting indicators: not found, error, end of file or
                // equal (ILE RPG Reference, each operation).
                match op.as_str() {
                    "CHAIN" | "DELETE" if !hi.is_empty() => {
                        stmts.push(format!("{} = not %found({file});", ind(&hi)))
                    }
                    "SETLL" | "SETGT" if !hi.is_empty() => {
                        stmts.push(format!("{} = not %found({file});", ind(&hi)))
                    }
                    _ => {}
                }
                if !lo.is_empty() {
                    stmts.push(format!("{} = %error();", ind(&lo)));
                }
                if !eq.is_empty() {
                    match op.as_str() {
                        "SETLL" => stmts.push(format!("{} = %equal({file});", ind(&eq))),
                        "READ" | "READE" | "READP" | "READPE" | "READC" => {
                            stmts.push(format!("{} = %eof({file});", ind(&eq)))
                        }
                        _ => {
                            keep_fixed(
                                self,
                                &format!("{op} with an indicator in 75-76 stays fixed"),
                            );
                            return;
                        }
                    }
                }
            }
            "EXCEPT" => stmts.push(if lf2.is_empty() {
                "except;".into()
            } else {
                format!("except {lf2};")
            }),
            "DSPLY" => {
                if !res.is_empty() || !f2.is_empty() {
                    keep_fixed(self, "DSPLY with a response or message queue stays fixed");
                    return;
                }
                stmts.push(format!("dsply {lf1};"));
            }
            "CLEAR" | "RESET" => {
                let opts = [lf1.as_str(), lf2.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                let sep = if opts.is_empty() {
                    String::new()
                } else {
                    format!("{opts} ")
                };
                stmts.push(format!("{} {sep}{lres};", lower(&op)));
            }
            "SORTA" => stmts.push(format!(
                "sorta{e} {};",
                if lx2.is_empty() { &lf2 } else { &lx2 }
            )),
            "XFOOT" => {
                let h = if ext.to_ascii_uppercase().contains('H') {
                    "eval(h) "
                } else {
                    ""
                };
                stmts.push(format!("{h}{lres} = %xfoot({lf2});"));
                num_inds(&mut stmts, &lres);
            }
            "SQRT" => {
                let h = if ext.to_ascii_uppercase().contains('H') {
                    "eval(h) "
                } else {
                    ""
                };
                stmts.push(format!("{h}{lres} = %sqrt({lf2});"));
            }
            "OCCUR" => {
                if !lf1.is_empty() {
                    stmts.push(format!("%occur({lf2}) = {lf1};"));
                }
                if !lres.is_empty() {
                    stmts.push(format!("{lres} = %occur({lf2});"));
                }
                if !lo.is_empty() {
                    keep_fixed(self, "OCCUR with an error indicator stays fixed");
                    return;
                }
            }
            "IN" | "OUT" => {
                let lock = if lf1.eq_ignore_ascii_case("*LOCK") {
                    "*lock "
                } else {
                    ""
                };
                stmts.push(format!("{}{e} {lock}{lf2};", lower(&op)));
            }
            "DEFINE" if f1.eq_ignore_ascii_case("*LIKE") => {
                let adj = if len.is_empty() {
                    String::new()
                } else {
                    format!(" : {len}")
                };
                self.decls
                    .push(format!("       dcl-s {lres} like({lf2}{adj});"));
                self.defined.push(res.to_ascii_uppercase());
                return;
            }
            "SUBST" if ext.to_ascii_uppercase().contains('P') && !lf1.is_empty() => {
                let (base, start) = split_colon(&lf2);
                let start = start.unwrap_or_else(|| "1".into());
                stmts.push(format!("{lres} = %subst({base} : {start} : {lf1});"));
                if !lo.is_empty() {
                    keep_fixed(self, "SUBST with an error indicator stays fixed");
                    return;
                }
            }
            "CAT" if ext.to_ascii_uppercase().contains('P') => {
                let a = if lf1.is_empty() {
                    lres.clone()
                } else {
                    lf1.clone()
                };
                let (b, n) = split_colon(&lf2);
                stmts.push(match n {
                    Some(n) => match n.parse::<usize>() {
                        Ok(0) => format!("{lres} = %trimr({a}) + {b};"),
                        Ok(k) => format!("{lres} = %trimr({a}) + '{}' + {b};", " ".repeat(k)),
                        Err(_) => {
                            keep_fixed(self, "CAT with a variable number of blanks stays fixed");
                            return;
                        }
                    },
                    None => format!("{lres} = {a} + {b};"),
                });
            }
            "XLATE" if ext.to_ascii_uppercase().contains('P') => {
                let (from, to) = split_colon(&lf1);
                let (base, start) = split_colon(&lf2);
                let to = to.unwrap_or_default();
                stmts.push(match start {
                    Some(s) => format!("{lres} = %xlate({from} : {to} : {base} : {s});"),
                    None => format!("{lres} = %xlate({from} : {to} : {base});"),
                });
            }
            "SCAN" | "CHECK" | "CHECKR" if !lres.is_empty() || !eq.is_empty() => {
                if self.arrays.contains(&res.to_ascii_uppercase()) {
                    keep_fixed(self, &format!("{op} into an array stays fixed"));
                    return;
                }
                if !lo.is_empty() {
                    keep_fixed(self, &format!("{op} with an error indicator stays fixed"));
                    return;
                }
                let (a, alen) = split_colon(&lf1);
                let a = match (op.as_str(), alen) {
                    ("SCAN", Some(n)) => format!("%subst({a} : 1 : {n})"),
                    _ => a,
                };
                let (base, start) = split_colon(&lf2);
                let bif = format!(
                    "%{}({a} : {base}{})",
                    lower(&op),
                    start.map(|s| format!(" : {s}")).unwrap_or_default()
                );
                if !lres.is_empty() {
                    stmts.push(format!("{lres} = {bif};"));
                    if !eq.is_empty() {
                        stmts.push(format!("{} = {lres} > 0;", ind(&eq)));
                    }
                } else {
                    stmts.push(format!("{} = {bif} > 0;", ind(&eq)));
                }
            }
            "COMMIT" | "ROLBK" | "FORCE" | "DUMP" | "DEALLOC" | "POST" | "NEXT" | "ACQ" | "REL" => {
                stmts.push(
                    format!(
                        "{}{e} {};",
                        lower(&op),
                        [lf1.as_str(), lf2.as_str()].join(" ").trim()
                    )
                    .replace(" ;", ";"),
                );
            }
            _ => {
                keep_fixed(
                    self,
                    &format!("{op} stays fixed (no free form, or a different behavior)"),
                );
                return;
            }
        }
        match cond {
            Some(c) => {
                self.free(&format!("if {c};"));
                for s in stmts {
                    self.free(&format!("  {s}"));
                }
                self.free("endif;");
            }
            None => {
                for s in stmts {
                    self.free(&s);
                }
            }
        }
    }
}

/// Convert a fixed-form (or mixed) ILE RPG member to free form.
pub fn convert(source: &str) -> Result<Converted> {
    let mut cv = Conv::default();
    let lines: Vec<&str> = source.lines().collect();
    let mut in_data = false;
    let mut free_region = false;
    for (idx, raw) in lines.iter().enumerate() {
        let lineno = idx + 1;
        let raw = raw.trim_end_matches('\r');
        if in_data || raw.starts_with("**") {
            in_data = true;
            cv.ctdata.push(raw.to_string());
            continue;
        }
        if raw.trim().is_empty() {
            cv.out.push(String::new());
            continue;
        }
        let cs = pad(raw);
        let spec = cs[5].to_ascii_uppercase();
        let c7 = cs[6];
        let content = col(&cs, 7, 80);
        let directive = content.trim_start().to_ascii_uppercase();
        if directive.starts_with("/FREE") {
            free_region = true;
            continue;
        }
        if directive.starts_with("/END-FREE") {
            free_region = false;
            continue;
        }
        // Already free form: kept, from position 8.
        if free_region || (spec == ' ' && c7 == ' ') {
            cv.out
                .push(format!("       {}", col(&cs, 8, 80).trim_end()));
            continue;
        }
        if c7 == '*' {
            // A comment: // in free form.
            let text = col(&cs, 8, 80).trim_end().to_string();
            cv.out
                .push(format!("       // {text}").trim_end().to_string());
            continue;
        }
        if c7 == '/' {
            // Directives: /COPY, /INCLUDE, /IF..., /TITLE...
            cv.out.push(format!("       {}", content.trim_end()));
            continue;
        }
        if spec != 'D' && spec != 'P' {
            cv.close_open();
        }
        match spec {
            'H' => cv.h(&cs),
            'F' => cv.f(&cs, raw, lineno),
            'D' => cv.d(&cs, lineno),
            'P' => cv.p(&cs, raw),
            'C' => {
                if cv.decl_at.is_none() {
                    cv.decl_at = Some(cv.out.len());
                }
                // The next operation (MVR after DIV).
                let next_op = lines
                    .iter()
                    .skip(idx + 1)
                    .map(|l| pad(l))
                    .find(|n| n[5].eq_ignore_ascii_case(&'C') && n[6] != '*')
                    .map(|n| colt(&n, 26, 35).to_ascii_uppercase())
                    .unwrap_or_default();
                cv.c(&cs, raw, lineno, &next_op);
            }
            'I' | 'O' => cv.keep(raw),
            _ => cv.keep(raw),
        }
    }
    cv.close_open();
    // Definitions made by the calculations go before them.
    let at = cv.decl_at.unwrap_or(cv.out.len());
    let decls = std::mem::take(&mut cv.decls);
    for (k, d) in decls.into_iter().enumerate() {
        cv.out.insert(at + k, d);
    }
    // **FREE needs compile-time data named (**CTDATA name).
    let all_free = cv.fixed == 0
        && cv
            .ctdata
            .iter()
            .filter(|l| l.starts_with("**"))
            .all(|l| l.to_ascii_uppercase().starts_with("**CTDATA"));
    let mut out = Vec::new();
    if all_free {
        out.push("**FREE".to_string());
        for l in &cv.out {
            out.push(l.strip_prefix("       ").unwrap_or(l).to_string());
        }
        // Compile-time data in **FREE: **CTDATA name.
        out.extend(cv.ctdata.iter().cloned());
    } else {
        out.extend(cv.out.iter().cloned());
        out.extend(cv.ctdata.iter().cloned());
    }
    let mut source = out.join("\n");
    source.push('\n');
    Ok(Converted {
        source,
        messages: cv.messages,
        fixed_lines: cv.fixed,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_and_calculations() {
        // ILE RPG Reference, "Free-Form Definition Statements": a packed
        // field P with decimals is PACKED(n:d), from-to positions in a
        // data structure are POS(from) with the length of the range, a
        // named constant is DCL-C; SETON becomes an assignment.
        let src = "     DAMT              S              7P 2\n     DREC              DS\n     DCODE                     1      3\n     DQTY                      4      8S 0\n     DMAX              C                   CONST(99)\n     C                   SETON                                        LR\n";
        let c = convert(src).unwrap();
        assert_eq!(
            c.source,
            "**FREE\ndcl-s amt packed(7:2);\ndcl-ds rec;\n  code char(3) pos(1);\n  qty zoned(5:0) pos(4);\nend-ds;\ndcl-c max 99;\n*inlr = *on;\n"
        );
    }
}
