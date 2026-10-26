//! REXX procedures (REXX/400): STRREXPRC runs one from a source member;
//! a command made with CRTCMD can have one as its command processing
//! program.
//!
//! Written from IBM's REXX/400 Reference and Programmer's Guide and the
//! REXX language definition: instructions (assignment, SAY, IF, DO,
//! SELECT, CALL and functions, internal routines with PROCEDURE EXPOSE,
//! PARSE, SIGNAL, condition traps ERROR, FAILURE, NOVALUE, SYNTAX, HALT,
//! INTERPRET, DROP, NUMERIC DIGITS, PUSH, QUEUE, PULL, EXIT, RETURN),
//! decimal arithmetic to NUMERIC DIGITS (9) significant digits, the
//! built-in functions (`bif.rs`), and commands: a clause that is only an
//! expression goes to the command environment, *COMMAND by default: a CL
//! command, where `&name` is a REXX variable (its value; a retrieve
//! command's return value comes back into it), RC is 0 or the escape
//! message's identifier, and a command that fails raises ERROR.
//!
//! Strings are text here; where REXX/400 works on EBCDIC bytes (C2X,
//! X2C, C2D, hexadecimal strings, comparisons) the text is taken as CCSID
//! 37.

mod bif;
pub mod parse;

use crate::commands::Job;
use anyhow::{anyhow, bail, Result};
use parse::{DoCtl, Expr, Line, Parser, Pos, Source, Stmt, TItem};
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A REXX procedure ready to run.
pub struct Program {
    pub name: String,
    pub lines: Vec<Line>,
    labels: HashMap<String, usize>,
    pub source: Vec<String>,
}

impl Program {
    pub fn parse(name: &str, src: &str) -> Result<Program> {
        let lines = Parser::program(src)?;
        let mut labels = HashMap::new();
        for (i, l) in lines.iter().enumerate() {
            if let Stmt::Label(n) = &l.stmt {
                labels.entry(n.clone()).or_insert(i);
            }
        }
        Ok(Program {
            name: name.to_string(),
            lines,
            labels,
            source: src.lines().map(str::to_string).collect(),
        })
    }
}

/// A condition raised while evaluating (NOVALUE), or an error that ends
/// the procedure whatever its traps (an escape message sent to its
/// caller).
#[derive(Debug)]
enum Raised {
    NoValue(String),
    Fatal(anyhow::Error),
}

impl std::fmt::Display for Raised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Raised::NoValue(n) => write!(f, "REX0046: Variable {n} has no value (NOVALUE)."),
            Raised::Fatal(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for Raised {}

enum Flow {
    Next,
    Leave(Option<String>),
    Iterate(Option<String>),
    Return(Option<String>),
    Exit(Option<String>),
    Signal(String),
}

#[derive(Clone, Default)]
struct Trap {
    /// SIGNAL (true) or CALL (false), and the label.
    on: Option<(bool, String)>,
}

#[derive(Clone, Default)]
struct Traps {
    t: HashMap<String, Trap>,
}

struct Scope {
    vars: HashMap<String, String>,
    exposed: HashSet<String>,
    parent: Option<usize>,
}

struct Frame {
    scope: usize,
    args: Vec<Option<String>>,
    traps: Traps,
    /// An internal routine (RETURN goes back) or the main program.
    internal: bool,
    /// PROCEDURE is allowed (the first instruction of a routine).
    can_procedure: bool,
}

/// What CONDITION() tells.
#[derive(Clone, Default)]
struct Condition {
    name: String,
    desc: String,
    instruction: String,
}

/// The external data queue of each job (PUSH, QUEUE, PULL, QUEUED).
static QUEUES: std::sync::Mutex<Option<HashMap<String, std::collections::VecDeque<String>>>> =
    std::sync::Mutex::new(None);

fn with_queue<R>(job: &str, f: impl FnOnce(&mut std::collections::VecDeque<String>) -> R) -> R {
    let mut g = QUEUES.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new)
        .entry(job.to_string())
        .or_default())
}

pub struct Rx<'j, 'a> {
    job: &'j mut Job<'a>,
    prog: Arc<Program>,
    scopes: Vec<Scope>,
    frames: Vec<Frame>,
    digits: u32,
    address: String,
    prev_address: String,
    condition: Option<Condition>,
    line: usize,
    /// How the procedure was started (PARSE SOURCE).
    how: String,
    queue_key: String,
    depth: usize,
}

/// Run a procedure with its argument string; what EXIT or RETURN gives.
pub async fn run(
    job: &mut Job<'_>,
    prog: Arc<Program>,
    arg: &str,
    env: &str,
) -> Result<Option<String>> {
    let key = job.job_key();
    let mut rx = Rx {
        job,
        prog: prog.clone(),
        scopes: vec![Scope {
            vars: HashMap::new(),
            exposed: HashSet::new(),
            parent: None,
        }],
        frames: vec![Frame {
            scope: 0,
            args: vec![Some(arg.to_string())],
            traps: Traps::default(),
            internal: false,
            can_procedure: false,
        }],
        digits: 9,
        address: env.to_string(),
        prev_address: env.to_string(),
        condition: None,
        line: 0,
        how: "COMMAND".into(),
        queue_key: key,
        depth: 0,
    };
    rx.job.calls.enter();
    let r = rx.run_top(0).await;
    rx.job.calls.leave();
    match r? {
        Flow::Return(v) | Flow::Exit(v) => Ok(v),
        _ => Ok(None),
    }
}

/// REXX's numbers: an optional sign, digits with a decimal point, an
/// exponent; blanks around.
pub(crate) fn number(s: &str) -> Option<Decimal> {
    let t = s.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t.strip_prefix('+').map(str::trim_start).unwrap_or(t)),
    };
    if body.is_empty() {
        return None;
    }
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    if mant.is_empty()
        || mant == "."
        || !mant.chars().all(|c| c.is_ascii_digit() || c == '.')
        || mant.matches('.').count() > 1
    {
        return None;
    }
    let mut d = Decimal::from_str(if mant.starts_with('.') {
        // ".5"
        return number(&format!(
            "{}0{mant}{}",
            if neg { "-" } else { "" },
            exp.map(|e| format!("E{e}")).unwrap_or_default()
        ));
    } else {
        mant.trim_end_matches('.')
    })
    .ok()?;
    if let Some(e) = exp {
        let n: i32 = e.parse().ok()?;
        if n.unsigned_abs() > 28 {
            return None;
        }
        let ten = Decimal::from(10u32);
        for _ in 0..n.unsigned_abs() {
            d = if n > 0 {
                d.checked_mul(ten)?
            } else {
                d.checked_div(ten)?
            };
        }
    }
    Some(if neg { -d } else { d })
}

fn rounded(d: Decimal, digits: u32) -> Decimal {
    if d.is_zero() {
        return Decimal::ZERO;
    }
    let m = d.mantissa().unsigned_abs().to_string();
    let sig = m.len() as u32;
    if sig <= digits {
        return d;
    }
    let drop = sig - digits;
    if drop <= d.scale() {
        d.round_dp_with_strategy(d.scale() - drop, RoundingStrategy::MidpointAwayFromZero)
    } else {
        // Rounding in the integer part: to a multiple of 10^k.
        let k = drop - d.scale();
        let p = Decimal::from_i128_with_scale(10i128.pow(k), 0);
        ((d / p).round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)) * p
    }
}

fn show(d: Decimal) -> String {
    if d.is_zero() {
        return if d.scale() > 0 {
            format!("0.{}", "0".repeat(d.scale() as usize))
        } else {
            "0".into()
        };
    }
    d.to_string()
}

fn truth(v: &str) -> Result<bool> {
    match v.trim() {
        "1" => Ok(true),
        "0" => Ok(false),
        other => bail!("REX0034: Logical value '{other}' is not 0 or 1."),
    }
}

fn ebcdic(s: &str) -> Vec<u8> {
    crate::tn5250::ebcdic::encode(s)
}

/// The value as REXX compares it: numbers numerically, other strings
/// without leading and trailing blanks, the shorter padded with blanks,
/// in EBCDIC.
fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    if let (Some(x), Some(y)) = (number(a), number(b)) {
        return x.cmp(&y);
    }
    let (a, b) = (a.trim_matches(' '), b.trim_matches(' '));
    let n = a.chars().count().max(b.chars().count());
    ebcdic(&format!("{a:<n$}")).cmp(&ebcdic(&format!("{b:<n$}")))
}

impl<'j, 'a> Rx<'j, 'a> {
    // ---------------------------------------------------------- variables

    fn scope_for(&self, name: &str) -> usize {
        let mut s = self.frames.last().map_or(0, |f| f.scope);
        loop {
            let sc = &self.scopes[s];
            let stem = name.find('.').map(|i| &name[..=i]);
            let exposed =
                sc.exposed.contains(name) || stem.is_some_and(|st| sc.exposed.contains(st));
            match (exposed, sc.parent) {
                (true, Some(p)) => s = p,
                _ => return s,
            }
        }
    }

    /// The name a symbol stands for: a compound symbol's tail with its
    /// variables' values.
    fn derived(&self, sym: &str) -> String {
        let Some(dot) = sym.find('.') else {
            return sym.to_string();
        };
        let (stem, tail) = (&sym[..=dot], &sym[dot + 1..]);
        if tail.is_empty() {
            return stem.to_string();
        }
        let parts: Vec<String> = tail
            .split('.')
            .map(|p| {
                if p.is_empty() || p.starts_with(|c: char| c.is_ascii_digit()) {
                    p.to_string()
                } else {
                    self.lookup(p).unwrap_or_else(|| p.to_string())
                }
            })
            .collect();
        format!("{stem}{}", parts.join("."))
    }

    fn lookup(&self, name: &str) -> Option<String> {
        let s = self.scope_for(name);
        let vars = &self.scopes[s].vars;
        if let Some(v) = vars.get(name) {
            return Some(v.clone());
        }
        // A compound variable not set: its stem's value, if the stem was
        // given one.
        match name.find('.') {
            Some(i) if i + 1 < name.len() => vars.get(&name[..=i]).cloned(),
            _ => None,
        }
    }

    /// A variable's value (by symbol), None when it has none.
    fn get(&self, sym: &str) -> Option<String> {
        self.lookup(&self.derived(sym))
    }

    fn value_of(&self, sym: &str) -> Result<String> {
        let name = self.derived(sym);
        match self.lookup(&name) {
            Some(v) => Ok(v),
            None => {
                if self.trap("NOVALUE").is_some() {
                    return Err(Raised::NoValue(name).into());
                }
                Ok(name)
            }
        }
    }

    fn set(&mut self, sym: &str, v: String) {
        let name = self.derived(sym);
        let s = self.scope_for(&name);
        let vars = &mut self.scopes[s].vars;
        if name.ends_with('.') && name.matches('.').count() == 1 {
            // A stem: every compound variable with it gets the value.
            vars.retain(|k, _| !k.starts_with(&name));
        }
        vars.insert(name, v);
    }

    fn drop_var(&mut self, sym: &str) {
        let name = self.derived(sym);
        let s = self.scope_for(&name);
        let vars = &mut self.scopes[s].vars;
        if name.ends_with('.') && name.matches('.').count() == 1 {
            vars.retain(|k, _| !k.starts_with(&name));
        } else {
            vars.remove(&name);
        }
    }

    fn trap(&self, cond: &str) -> Option<(bool, String)> {
        self.frames
            .last()
            .and_then(|f| f.traps.t.get(cond))
            .and_then(|t| t.on.clone())
    }

    fn set_trap(&mut self, cond: &str, on: Option<(bool, String)>) {
        if let Some(f) = self.frames.last_mut() {
            f.traps.t.insert(cond.to_string(), Trap { on });
        }
    }

    // ---------------------------------------------------------- running

    fn run_top(&mut self, start: usize) -> Fut<'_, Flow> {
        Box::pin(async move {
            let prog = self.prog.clone();
            let mut pc = start;
            while pc < prog.lines.len() {
                self.line = prog.lines[pc].line;
                let f = self.exec_trapped(&prog.lines[pc].stmt).await?;
                if let Some(fr) = self.frames.last_mut() {
                    if !matches!(prog.lines[pc].stmt, Stmt::Label(_)) {
                        fr.can_procedure = false;
                    }
                }
                match f {
                    Flow::Next => pc += 1,
                    Flow::Signal(l) => {
                        pc = *prog
                            .labels
                            .get(&l)
                            .ok_or_else(|| anyhow!("REX0016: Label {l} not found."))?;
                        self.set("SIGL", self.line.to_string());
                    }
                    Flow::Leave(_) | Flow::Iterate(_) => {
                        bail!(
                            "REX0028: line {}: LEAVE or ITERATE is not in a loop.",
                            self.line
                        )
                    }
                    f @ (Flow::Return(_) | Flow::Exit(_)) => return Ok(f),
                }
            }
            Ok(Flow::Return(None))
        })
    }

    /// An instruction, with SYNTAX and NOVALUE trapped as the procedure
    /// asked.
    async fn exec_trapped(&mut self, st: &Stmt) -> Result<Flow> {
        match self.exec(st).await {
            Ok(f) => Ok(f),
            Err(e) => {
                if e.downcast_ref::<Exit>().is_some() {
                    return Err(e);
                }
                if let Some(r) = e.downcast_ref::<Raised>() {
                    match r {
                        Raised::Fatal(_) => return Err(e),
                        Raised::NoValue(n) => {
                            if let Some((_, label)) = self.trap("NOVALUE") {
                                self.set_trap("NOVALUE", None);
                                self.condition = Some(Condition {
                                    name: "NOVALUE".into(),
                                    desc: n.clone(),
                                    instruction: "SIGNAL".into(),
                                });
                                return Ok(Flow::Signal(label));
                            }
                        }
                    }
                }
                let text = format!("{e:#}");
                if let Some((_, label)) = self.trap("SYNTAX") {
                    let rc = text
                        .strip_prefix("REX")
                        .and_then(|r| r.get(..4))
                        .and_then(|n| n.parse::<u32>().ok())
                        .unwrap_or(49);
                    self.set_trap("SYNTAX", None);
                    self.set("RC", rc.to_string());
                    self.condition = Some(Condition {
                        name: "SYNTAX".into(),
                        desc: text,
                        instruction: "SIGNAL".into(),
                    });
                    return Ok(Flow::Signal(label));
                }
                if text.contains("Error in REXX procedure") {
                    Err(e)
                } else {
                    Err(anyhow!(
                        "{text}\nError in REXX procedure {} at line {}.",
                        self.prog.name,
                        self.line
                    ))
                }
            }
        }
    }

    fn exec<'s>(&'s mut self, st: &'s Stmt) -> Fut<'s, Flow> {
        Box::pin(async move {
            Ok(match st {
                Stmt::Nop | Stmt::Label(_) | Stmt::Ignore => Flow::Next,
                Stmt::Say(e) => {
                    let v = match e {
                        Some(e) => self.eval(e).await?,
                        None => String::new(),
                    };
                    let key = self.job.job_key();
                    crate::pgmmsg::write_stdout(&key, &format!("{v}\n"));
                    Flow::Next
                }
                Stmt::Assign(n, e) => {
                    let v = match e {
                        Some(e) => self.eval(e).await?,
                        None => String::new(),
                    };
                    self.set(n, v);
                    Flow::Next
                }
                Stmt::Command(e) => {
                    let text = self.eval(e).await?;
                    self.command(&text).await?
                }
                Stmt::If(c, then, els) => {
                    let v = self.eval(c).await?;
                    if truth(&v)? {
                        self.exec(then).await?
                    } else if let Some(e) = els {
                        self.exec(e).await?
                    } else {
                        Flow::Next
                    }
                }
                Stmt::Do(ctl, body) => self.do_loop(ctl, body).await?,
                Stmt::Select(whens, other) => {
                    for (c, s) in whens {
                        let v = self.eval(c).await?;
                        if truth(&v)? {
                            return self.exec(s).await;
                        }
                    }
                    match other {
                        Some(list) => {
                            for s in list {
                                match self.exec(s).await? {
                                    Flow::Next => {}
                                    f => return Ok(f),
                                }
                            }
                            Flow::Next
                        }
                        None => bail!(
                            "REX0007: line {}: no WHEN was true and there is no OTHERWISE.",
                            self.line
                        ),
                    }
                }
                Stmt::Leave(n) => Flow::Leave(n.clone()),
                Stmt::Iterate(n) => Flow::Iterate(n.clone()),
                Stmt::Call(name, quoted, args) => {
                    let mut vals = Vec::new();
                    for a in args {
                        vals.push(match a {
                            Some(e) => Some(self.eval(e).await?),
                            None => None,
                        });
                    }
                    match self.call(name, *quoted, vals).await? {
                        Some(v) => self.set("RESULT", v),
                        None => self.drop_var("RESULT"),
                    }
                    Flow::Next
                }
                Stmt::Trap(signal, on, cond, name) => {
                    let cond = cond.to_ascii_uppercase();
                    if !["ERROR", "FAILURE", "NOVALUE", "SYNTAX", "HALT", "NOTREADY"]
                        .contains(&cond.as_str())
                    {
                        bail!("REX0025: line {}: {cond} is not a condition.", self.line);
                    }
                    self.set_trap(
                        &cond,
                        on.then(|| (*signal, name.clone().unwrap_or_else(|| cond.clone()))),
                    );
                    Flow::Next
                }
                Stmt::Signal(l) => Flow::Signal(l.to_ascii_uppercase()),
                Stmt::SignalValue(e) => Flow::Signal(self.eval(e).await?.to_ascii_uppercase()),
                Stmt::Return(e) => {
                    let v = match e {
                        Some(e) => Some(self.eval(e).await?),
                        None => None,
                    };
                    if self.frames.last().is_some_and(|f| f.internal) {
                        Flow::Return(v)
                    } else {
                        Flow::Exit(v)
                    }
                }
                Stmt::Exit(e) => {
                    let v = match e {
                        Some(e) => Some(self.eval(e).await?),
                        None => None,
                    };
                    Flow::Exit(v)
                }
                Stmt::Procedure(names) => {
                    let f = self.frames.last().expect("a frame");
                    if !f.internal || !f.can_procedure {
                        bail!("REX0017: line {}: PROCEDURE is not the first instruction of an internal routine.", self.line);
                    }
                    let parent = f.scope;
                    let mut exposed = HashSet::new();
                    for n in names {
                        if let Some(list) = n.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
                            exposed.insert(list.to_string());
                            let v = self.get(list).unwrap_or_default();
                            for w in v.split_whitespace() {
                                exposed.insert(w.to_ascii_uppercase());
                            }
                        } else {
                            exposed.insert(n.clone());
                        }
                    }
                    self.scopes.push(Scope {
                        vars: HashMap::new(),
                        exposed,
                        parent: Some(parent),
                    });
                    let s = self.scopes.len() - 1;
                    self.frames.last_mut().expect("a frame").scope = s;
                    Flow::Next
                }
                Stmt::Parse(upper, src, tpl) => {
                    let mut strings: Vec<String> = match src {
                        Source::Arg => self
                            .frames
                            .last()
                            .map(|f| {
                                f.args
                                    .iter()
                                    .map(|a| a.clone().unwrap_or_default())
                                    .collect()
                            })
                            .unwrap_or_default(),
                        Source::Pull => vec![self.pull()],
                        Source::Var(n) => vec![self.value_of(n)?],
                        Source::Value(e) => vec![match e {
                            Some(e) => self.eval(e).await?,
                            None => String::new(),
                        }],
                        Source::SourceInfo => {
                            vec![format!("OS/400 {} {}", self.how, self.prog.name)]
                        }
                        Source::Version => vec!["REXXSAA 3.48 14 May 1992".into()],
                    };
                    if *upper {
                        for s in strings.iter_mut() {
                            *s = s.to_uppercase();
                        }
                    }
                    for (k, t) in tpl.iter().enumerate() {
                        let s = strings.get(k).cloned().unwrap_or_default();
                        self.parse_template(&s, t).await?;
                    }
                    Flow::Next
                }
                Stmt::Drop(names) => {
                    for n in names {
                        self.drop_var(n);
                    }
                    Flow::Next
                }
                Stmt::Interpret(e) => {
                    let src = self.eval(e).await?;
                    let lines = Parser::program(&src)?;
                    for l in &lines {
                        match self.exec(&l.stmt).await? {
                            Flow::Next => {}
                            f => return Ok(f),
                        }
                    }
                    Flow::Next
                }
                Stmt::Address(env, cmd) => {
                    match (env, cmd) {
                        (None, _) => std::mem::swap(&mut self.address, &mut self.prev_address),
                        (Some(env), None) => {
                            self.prev_address = std::mem::replace(&mut self.address, env.clone())
                        }
                        (Some(env), Some(c)) => {
                            let text = self.eval(c).await?;
                            let keep = std::mem::replace(&mut self.address, env.clone());
                            let f = self.command(&text).await;
                            self.address = keep;
                            return f;
                        }
                    }
                    Flow::Next
                }
                Stmt::AddressValue(e) => {
                    let env = self.eval(e).await?.to_ascii_uppercase();
                    self.prev_address = std::mem::replace(&mut self.address, env);
                    Flow::Next
                }
                Stmt::Numeric(what, e) => {
                    if what == "DIGITS" {
                        self.digits = match e {
                            Some(e) => {
                                let v = self.eval(e).await?;
                                v.trim().parse().map_err(|_| {
                                    anyhow!("REX0026: NUMERIC DIGITS {v} is not a whole number.")
                                })?
                            }
                            None => 9,
                        };
                        if !(1..=28).contains(&self.digits) {
                            bail!(
                                "REX0033: NUMERIC DIGITS {} is more than as4tainr's 28.",
                                self.digits
                            );
                        }
                    }
                    Flow::Next
                }
                Stmt::Push(lifo, e) => {
                    let v = match e {
                        Some(e) => self.eval(e).await?,
                        None => String::new(),
                    };
                    with_queue(&self.queue_key, |q| {
                        if *lifo {
                            q.push_front(v)
                        } else {
                            q.push_back(v)
                        }
                    });
                    Flow::Next
                }
            })
        })
    }

    /// PULL: the next line of the external data queue (no terminal
    /// input here: an empty line when the queue is empty).
    fn pull(&mut self) -> String {
        with_queue(&self.queue_key, |q| q.pop_front()).unwrap_or_default()
    }

    async fn do_loop(&mut self, ctl: &DoCtl, body: &[Stmt]) -> Result<Flow> {
        let name = ctl.var.as_ref().map(|(n, _)| n.clone());
        if ctl.simple {
            for s in body {
                match self.exec(s).await? {
                    Flow::Next => {}
                    f => return Ok(f),
                }
            }
            return Ok(Flow::Next);
        }
        let num = |rx: &Self, v: String, what: &str| -> Result<Decimal> {
            let _ = rx;
            number(&v).ok_or_else(|| anyhow!("REX0041: DO {what} '{v}' is not a number."))
        };
        let mut i = match &ctl.var {
            Some((_, e)) => {
                let v = self.eval(e).await?;
                Some(num(self, v, "start")?)
            }
            None => None,
        };
        let to = match &ctl.to {
            Some(e) => {
                let v = self.eval(e).await?;
                Some(num(self, v, "TO")?)
            }
            None => None,
        };
        let by = match &ctl.by {
            Some(e) => {
                let v = self.eval(e).await?;
                num(self, v, "BY")?
            }
            None => Decimal::ONE,
        };
        let count = match &ctl.count {
            Some(e) => {
                let v = self.eval(e).await?;
                let n = num(self, v.clone(), "count")?;
                if n.is_sign_negative() || n.fract() != Decimal::ZERO {
                    bail!("REX0026: DO count '{v}' is not a whole number.");
                }
                Some(n.to_u64().unwrap_or(0))
            }
            None => None,
        };
        let mut done = 0u64;
        loop {
            if let (Some(name), Some(v)) = (&name, i) {
                self.set(name, show(rounded(v, self.digits)));
            }
            if let (Some(v), Some(t)) = (i, to) {
                if (by.is_sign_positive() && v > t) || (by.is_sign_negative() && v < t) {
                    break;
                }
            }
            if count.is_some_and(|c| done >= c) {
                break;
            }
            if let Some(w) = &ctl.while_ {
                let v = self.eval(w).await?;
                if !truth(&v)? {
                    break;
                }
            }
            let mut leave = false;
            for s in body {
                match self.exec(s).await? {
                    Flow::Next => {}
                    Flow::Leave(n) if n.is_none() || n == name => {
                        leave = true;
                        break;
                    }
                    Flow::Iterate(n) if n.is_none() || n == name => break,
                    f => return Ok(f),
                }
            }
            if leave {
                break;
            }
            done += 1;
            if let Some(u) = &ctl.until {
                let v = self.eval(u).await?;
                if truth(&v)? {
                    break;
                }
            }
            if let Some(name) = &name {
                // The control variable may have been changed in the body.
                let cur = self
                    .get(name)
                    .and_then(|v| number(&v))
                    .unwrap_or(i.unwrap_or_default());
                i = Some(cur + by);
            }
        }
        Ok(Flow::Next)
    }

    // ---------------------------------------------------------- templates

    async fn parse_template(&mut self, s: &str, t: &[TItem]) -> Result<()> {
        let chars: Vec<char> = s.chars().collect();
        let len = chars.len();
        let sub = |a: usize, b: usize| -> String {
            chars[a.min(len)..b.min(len).max(a.min(len))]
                .iter()
                .collect()
        };
        // Targets waiting for the next pattern.
        let mut pending: Vec<&TItem> = Vec::new();
        let mut start = 0usize;
        let mut k = 0;
        while k <= t.len() {
            let item = t.get(k);
            match item {
                Some(TItem::Var(_)) | Some(TItem::Dot) => {
                    pending.push(item.unwrap());
                    k += 1;
                    continue;
                }
                _ => {}
            }
            // A pattern (or the end): where the piece for the pending
            // targets ends, and where the next one starts.
            let (end, next) = match item {
                None => (len, len),
                Some(TItem::Pattern(e)) => {
                    let p = self.eval(e).await?;
                    let pc: Vec<char> = p.chars().collect();
                    let found = if pc.is_empty() {
                        None
                    } else {
                        (start..len.saturating_sub(pc.len() - 1))
                            .find(|&i| chars[i..i + pc.len()] == pc[..])
                    };
                    match found {
                        Some(i) => (i, i + pc.len()),
                        None => (len, len),
                    }
                }
                Some(TItem::Pos(p)) => {
                    let (e, kind) = match p {
                        Pos::Abs(e) => (e, 0),
                        Pos::Plus(e) => (e, 1),
                        Pos::Minus(e) => (e, -1),
                    };
                    let v = self.eval(e).await?;
                    let n: i64 = v.trim().parse().map_err(|_| {
                        anyhow!("REX0026: Position {v} in a template is not a whole number.")
                    })?;
                    let at = match kind {
                        0 => (n - 1).max(0) as usize,
                        1 => (start as i64 + n).max(0) as usize,
                        _ => (start as i64 - n).max(0) as usize,
                    }
                    .min(len);
                    if at <= start {
                        // Moving back: the targets get the rest.
                        (len, at)
                    } else {
                        (at, at)
                    }
                }
                Some(_) => unreachable!(),
            };
            let piece = sub(start, end);
            self.assign_words(&piece, &pending)?;
            pending.clear();
            start = next;
            k += 1;
        }
        Ok(())
    }

    /// Words of `piece` to the targets: one each, the last the rest
    /// (less the one blank before it).
    fn assign_words(&mut self, piece: &str, targets: &[&TItem]) -> Result<()> {
        let mut rest = piece.to_string();
        for (k, t) in targets.iter().enumerate() {
            let last = k + 1 == targets.len();
            let value = if last {
                rest.clone()
            } else {
                let r = rest.trim_start_matches(' ');
                let (w, after) = match r.find(' ') {
                    Some(i) => (&r[..i], &r[i + 1..]),
                    None => (r, ""),
                };
                let w = w.to_string();
                rest = after.to_string();
                w
            };
            if let TItem::Var(n) = t {
                self.set(n, value);
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------- expressions

    fn eval<'s>(&'s mut self, e: &'s Expr) -> Fut<'s, String> {
        Box::pin(async move {
            Ok(match e {
                Expr::Lit(s) => s.clone(),
                Expr::Var(n) => self.value_of(n)?,
                Expr::Call(name, quoted, args) => {
                    let mut vals = Vec::new();
                    for a in args {
                        vals.push(match a {
                            Some(e) => Some(self.eval(e).await?),
                            None => None,
                        });
                    }
                    self.call(name, *quoted, vals).await?.ok_or_else(|| {
                        anyhow!("REX0044: Function {name} did not return a value.")
                    })?
                }
                Expr::Prefix(op, x) => {
                    let v = self.eval(x).await?;
                    match op.as_str() {
                        "\\" => if truth(&v)? { "0" } else { "1" }.into(),
                        o => {
                            let d = number(&v)
                                .ok_or_else(|| anyhow!("REX0041: '{v}' is not a number ({o})."))?;
                            show(rounded(if o == "-" { -d } else { d }, self.digits))
                        }
                    }
                }
                Expr::Bin(op, l, r) => {
                    let a = self.eval(l).await?;
                    // & and | evaluate both sides (REXX does not short
                    // circuit).
                    let b = self.eval(r).await?;
                    self.binary(op, &a, &b)?
                }
            })
        })
    }

    fn binary(&self, op: &str, a: &str, b: &str) -> Result<String> {
        use std::cmp::Ordering::*;
        let tf = |x: bool| if x { "1" } else { "0" }.to_string();
        Ok(match op {
            "" => format!("{a}{b}"),
            " " => format!("{a} {b}"),
            "||" => format!("{a}{b}"),
            "&" => tf(truth(a)? && truth(b)?),
            "|" => tf(truth(a)? || truth(b)?),
            "&&" => tf(truth(a)? != truth(b)?),
            "=" => tf(compare(a, b) == Equal),
            "\\=" | "<>" | "><" => tf(compare(a, b) != Equal),
            ">" => tf(compare(a, b) == Greater),
            "<" => tf(compare(a, b) == Less),
            ">=" | "\\<" => tf(compare(a, b) != Less),
            "<=" | "\\>" => tf(compare(a, b) != Greater),
            "==" => tf(a == b),
            "\\==" => tf(a != b),
            ">>" => tf(ebcdic(a) > ebcdic(b)),
            "<<" => tf(ebcdic(a) < ebcdic(b)),
            ">>=" | "\\<<" => tf(ebcdic(a) >= ebcdic(b)),
            "<<=" | "\\>>" => tf(ebcdic(a) <= ebcdic(b)),
            _ => {
                let x =
                    number(a).ok_or_else(|| anyhow!("REX0041: '{a}' is not a number ({op})."))?;
                let y =
                    number(b).ok_or_else(|| anyhow!("REX0041: '{b}' is not a number ({op})."))?;
                let over = || anyhow!("REX0042: Arithmetic overflow ({a} {op} {b}).");
                let zero = || anyhow!("REX0042: Division by zero ({a} {op} {b}).");
                let d = match op {
                    "+" => x.checked_add(y).ok_or_else(over)?,
                    "-" => x.checked_sub(y).ok_or_else(over)?,
                    "*" => x.checked_mul(y).ok_or_else(over)?,
                    "/" => {
                        if y.is_zero() {
                            return Err(zero());
                        }
                        return Ok(show(
                            rounded(x.checked_div(y).ok_or_else(over)?, self.digits).normalize(),
                        ));
                    }
                    "%" => {
                        if y.is_zero() {
                            return Err(zero());
                        }
                        x.checked_div(y).ok_or_else(over)?.trunc()
                    }
                    "//" => {
                        if y.is_zero() {
                            return Err(zero());
                        }
                        let q = x.checked_div(y).ok_or_else(over)?.trunc();
                        x - q * y
                    }
                    "**" => {
                        if y.fract() != Decimal::ZERO {
                            bail!("REX0026: The power {b} is not a whole number.");
                        }
                        let n = y.to_i64().ok_or_else(over)?;
                        let mut r = Decimal::ONE;
                        for _ in 0..n.unsigned_abs() {
                            r = rounded(r.checked_mul(x).ok_or_else(over)?, self.digits + 2);
                        }
                        if n < 0 {
                            if r.is_zero() {
                                return Err(zero());
                            }
                            return Ok(show(rounded(Decimal::ONE / r, self.digits).normalize()));
                        }
                        r
                    }
                    other => bail!("REX0035: Operator {other} is not known."),
                };
                show(rounded(d, self.digits))
            }
        })
    }

    // ---------------------------------------------------------- routines

    /// A routine: an internal label, a built-in function, or an external
    /// REXX procedure (not here). Its result.
    fn call<'s>(
        &'s mut self,
        name: &'s str,
        quoted: bool,
        args: Vec<Option<String>>,
    ) -> Fut<'s, Option<String>> {
        Box::pin(async move {
            let up = name.to_ascii_uppercase();
            if !quoted {
                if let Some(&at) = self.prog.labels.get(&up) {
                    return self.internal(at, args).await;
                }
            }
            if let Some(v) = bif::call(self, &up, &args)? {
                return Ok(Some(v));
            }
            bail!("REX0043: Routine {up} not found.")
        })
    }

    async fn internal(&mut self, at: usize, args: Vec<Option<String>>) -> Result<Option<String>> {
        if self.depth > 200 {
            bail!("REX0011: Control stack full (routine calls nested too deeply).");
        }
        let scope = self.frames.last().map_or(0, |f| f.scope);
        let traps = self
            .frames
            .last()
            .map(|f| f.traps.clone())
            .unwrap_or_default();
        self.set("SIGL", self.line.to_string());
        self.frames.push(Frame {
            scope,
            args,
            traps,
            internal: true,
            can_procedure: true,
        });
        let line = self.line;
        let saved_digits = self.digits;
        let saved_addr = (self.address.clone(), self.prev_address.clone());
        self.depth += 1;
        let r = self.run_top(at + 1).await;
        self.depth -= 1;
        let f = self.frames.pop().expect("pushed");
        if f.scope != scope {
            // Scopes made by PROCEDURE are not used again.
            if f.scope + 1 == self.scopes.len() {
                self.scopes.pop();
            }
        }
        self.line = line;
        self.digits = saved_digits;
        (self.address, self.prev_address) = saved_addr;
        match r? {
            Flow::Return(v) => Ok(v),
            Flow::Exit(v) => Err(Exit(v).into()),
            _ => Ok(None),
        }
    }

    // ---------------------------------------------------------- commands

    async fn command(&mut self, text: &str) -> Result<Flow> {
        let env = self.address.clone();
        if env != "*COMMAND" && env != "COMMAND" {
            bail!("REX0042: The command environment {env} is not available here (only *COMMAND).");
        }
        let text = text.trim();
        if text.is_empty() {
            self.set("RC", "0".into());
            return Ok(Flow::Next);
        }
        // &name: REXX variables, as CL variables of the length of their
        // value.
        let mut vars: HashMap<String, crate::clpgm::Var> = HashMap::new();
        let mut unset: HashSet<String> = HashSet::new();
        let cs: Vec<char> = text.chars().collect();
        let mut i = 0;
        let mut in_q = false;
        while i < cs.len() {
            if cs[i] == '\'' {
                in_q = !in_q;
            }
            if cs[i] == '&'
                && !in_q
                && cs
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || "$#@_".contains(*c))
            {
                let s = i + 1;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_alphanumeric() || "$#@_.".contains(cs[i])) {
                    i += 1;
                }
                let sym: String = cs[s..i].iter().collect::<String>().to_ascii_uppercase();
                let (value, set) = match self.get(&sym) {
                    Some(v) => (v, true),
                    None => (String::new(), false),
                };
                if !set {
                    unset.insert(sym.clone());
                }
                let len = if set {
                    value.chars().count().max(1)
                } else {
                    2000
                };
                vars.insert(format!("&{sym}"), crate::clpgm::char_var(&value, len));
                continue;
            }
            i += 1;
        }
        let before: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.clone(), crate::clpgm::var_text(v)))
            .collect();
        self.job.calls.escape_to_caller = false;
        self.job.calls.escape = None;
        let r = crate::clpgm::command_with_vars(self.job, text, &mut vars).await;
        for (k, v) in &vars {
            let now = crate::clpgm::var_text(v);
            if before.get(k) != Some(&now) {
                let sym = &k[1..];
                let now = if unset.contains(sym) {
                    now.trim_end().to_string()
                } else {
                    now
                };
                self.set(sym, now);
            }
        }
        match r {
            Ok(_) => {
                self.set("RC", "0".into());
                Ok(Flow::Next)
            }
            Err(e) => {
                if self.job.calls.escape_to_caller && self.job.calls.escape.is_some() {
                    // SNDPGMMSG MSGTYPE(*ESCAPE) to the caller ends the
                    // procedure.
                    return Err(Raised::Fatal(e).into());
                }
                if std::env::var("AS4TAINR_TRACE_ERRORS").is_ok_and(|v| v == "1") {
                    eprintln!(
                        "as4tainr: REXX {} line {}: {text}: {e:#}",
                        self.prog.name, self.line
                    );
                }
                let id = crate::clpgm::error_id(&e);
                self.set("RC", if id.is_empty() { "1".into() } else { id });
                let cond = "ERROR";
                if let Some((signal, label)) = self.trap(cond) {
                    self.condition = Some(Condition {
                        name: cond.into(),
                        desc: text.to_string(),
                        instruction: if signal { "SIGNAL" } else { "CALL" }.into(),
                    });
                    if signal {
                        self.set_trap(cond, None);
                        return Ok(Flow::Signal(label.to_ascii_uppercase()));
                    }
                    // CALL ON ERROR: the routine, then on.
                    if let Some(&at) = self.prog.labels.get(&label.to_ascii_uppercase()) {
                        self.set_trap(cond, None);
                        let r = self.internal(at, Vec::new()).await;
                        self.set_trap(cond, Some((false, label)));
                        r?;
                    }
                }
                Ok(Flow::Next)
            }
        }
    }
}

/// EXIT in an internal routine: the whole procedure ends.
#[derive(Debug)]
struct Exit(Option<String>);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EXIT")
    }
}

impl std::error::Error for Exit {}

/// STRREXPRC: the procedure in SRCFILE/SRCMBR with PARM. A non-zero
/// numeric EXIT value is the escape message CPF7CFF (REXX/400).
pub async fn strrexprc(
    job: &mut Job<'_>,
    srcfile: &str,
    srcmbr: &str,
    parm: &str,
    env: &str,
) -> Result<Vec<String>> {
    let (libs, file) = job.search(srcfile);
    let file = file.to_ascii_uppercase();
    let m = crate::source::find_member(job.client, &libs, &file, &srcmbr.to_ascii_uppercase())
        .await
        .map_err(|e| anyhow!("CPF7CF3: REXX procedure {srcmbr} not found: {e:#}"))?;
    let prog = Arc::new(Program::parse(&srcmbr.to_ascii_uppercase(), &m.source)?);
    let env = match env.to_ascii_uppercase().as_str() {
        "" | "*COMMAND" => "*COMMAND".to_string(),
        other => other.to_string(),
    };
    let r = Box::pin(run(job, prog, parm, &env)).await;
    let v = match r {
        Ok(v) => v,
        Err(e) => match e.downcast::<Exit>() {
            Ok(Exit(v)) => v,
            Err(e) => match e.downcast::<Raised>() {
                Ok(Raised::Fatal(e)) => return Err(e),
                Ok(other) => return Err(anyhow!("{other}")),
                Err(e) => return Err(e),
            },
        },
    };
    if let Some(v) = v {
        if let Some(n) = number(&v) {
            if !n.is_zero() {
                bail!("CPF7CFF: REXX procedure {srcmbr} ended with exit code {v}.");
            }
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    // REXX arithmetic: NUMERIC DIGITS 9, division rounds and drops
    // trailing zeros, addition keeps decimals (REXX/400 Reference,
    // "Numbers and arithmetic").
    #[test]
    fn arithmetic() {
        assert_eq!(
            show(rounded(Decimal::ONE / Decimal::from(3), 9).normalize()),
            "0.333333333"
        );
        assert_eq!(
            show(rounded(number("1.50").unwrap() + Decimal::ONE, 9)),
            "2.50"
        );
        assert_eq!(number(" -12 ").unwrap(), Decimal::from(-12));
        assert_eq!(number("1E3").unwrap(), Decimal::from(1000));
        assert!(number("abc").is_none());
        assert_eq!(compare(" a", "a  "), std::cmp::Ordering::Equal);
        assert_eq!(compare("10", "9"), std::cmp::Ordering::Greater);
    }
}
