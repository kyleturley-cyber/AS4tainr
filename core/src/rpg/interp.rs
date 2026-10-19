//! Running a program: the RPG cycle, indicators, operations, built-in
//! functions and file I/O (ILE RPG Reference: "Program Cycle",
//! "Operation Codes", "Built-in Functions").
//!
//! The interpreter is synchronous; it runs on a blocking thread and does
//! database work with `block_on` on the runtime handle.

use super::ast::*;
use super::code::{Code, Ins};
use super::data::{self, a, DType, Fig, Fit, Value};
use super::dec::Dec;
use super::program::{Display, Program, Sym, VarInfo};
use crate::rla::{OpenFile, ReadResult, Record};
use crate::workstn::Workstation;
use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::HashMap;
use tokio::runtime::Handle;
use tokio_postgres::Client;

/// What the program runs with: the database, the job.
pub struct Io<'a> {
    pub rt: Handle,
    pub client: &'a Client,
    pub user: String,
    pub job_number: Option<i64>,
    /// The library list (data areas).
    pub libl: Vec<String>,
    /// The job's display (interactive jobs): EXFMT goes there.
    pub display: Option<crate::bridge::DisplayHandle>,
}

/// Status of the last operation on a file (%FOUND, %EOF, %EQUAL, %ERROR).
#[derive(Clone, Debug, Default)]
struct Status {
    found: bool,
    eof: bool,
    equal: bool,
    error: bool,
}

struct FileState {
    open: Option<OpenFile>,
    /// A display file is open.
    ws_open: bool,
    status: Status,
}

/// How a routine ended.
enum Flow {
    Normal,
    Return(Option<Value>),
    LeaveSr,
}

pub struct Machine<'a> {
    prog: &'a Program,
    bufs: Vec<Vec<u8>>,
    /// Indicators: 01-99 and the named ones (LR, RT, KA-KY, H1-H9,
    /// L1-L9, OF, OV, OA-OG, U1-U8, 1P, MR).
    inds: HashMap<String, bool>,
    files: Vec<FileState>,
    last: Status,
    io: Io<'a>,
    /// DSPLY output, in order.
    pub messages: Vec<String>,
    /// The remainder of the last DIV, for MVR.
    remainder: Dec,
    /// The display files' screen and subfiles.
    ws: Workstation,
    /// Data areas this program holds locked (IN *LOCK): (library, name).
    dtaara_locks: Vec<(String, String)>,
}

/// Function key indicators *INKA-*INKY for F1-F24 (there is no *INKO).
const FKEYS: &[u8; 24] = b"ABCDEFGHIJKLMNPQRSTUVWXY";

fn ind_name(n: &str) -> String {
    let t = n.trim().to_ascii_uppercase();
    match t.parse::<u32>() {
        Ok(v) if t.len() <= 2 => format!("{v:02}"),
        _ => t,
    }
}

/// Number text as %CHAR gives it: leading zeros removed (also the zero
/// before the decimal point), a leading '-' for negative values.
pub fn char_of_num(d: &Dec) -> String {
    let s = d.abs().to_string();
    let s = if d.scale() > 0 {
        s.trim_start_matches('0').to_string()
    } else {
        s
    };
    let s = if s.is_empty() { "0".to_string() } else { s };
    if d.is_negative() {
        format!("-{s}")
    } else {
        s
    }
}

impl<'a> Machine<'a> {
    pub fn new(prog: &'a Program, io: Io<'a>) -> Machine<'a> {
        Machine {
            prog,
            bufs: prog.globals.clone(),
            inds: HashMap::new(),
            files: prog
                .files
                .iter()
                .map(|_| FileState {
                    open: None,
                    ws_open: false,
                    status: Status::default(),
                })
                .collect(),
            last: Status::default(),
            io,
            messages: Vec::new(),
            remainder: Dec::ZERO,
            ws: Workstation::new(),
            dtaara_locks: Vec::new(),
        }
    }

    fn block<F: std::future::Future>(&self, f: F) -> F::Output {
        self.io.rt.block_on(f)
    }

    pub fn ind(&self, n: &str) -> bool {
        self.inds.get(&ind_name(n)).copied().unwrap_or(false)
    }

    pub fn set_ind(&mut self, n: &str, v: bool) {
        self.inds.insert(ind_name(n), v);
    }

    // ---- storage ------------------------------------------------------

    fn elem_off(&self, v: &VarInfo, index: Option<i128>) -> Result<usize> {
        match index {
            None => Ok(v.off),
            Some(i) => {
                if v.dim == 0 {
                    bail!("this field is not an array");
                }
                if i < 1 || i > v.dim as i128 {
                    bail!(
                        "RNQ0121: An array index is out of range (index {i}, {} elements).",
                        v.dim
                    );
                }
                Ok(v.off + (i as usize - 1) * v.stride)
            }
        }
    }

    fn load_var(&self, v: &VarInfo, index: Option<i128>) -> Result<Value> {
        let off = self.elem_off(v, index)?;
        data::load(&v.dtype, &self.bufs[v.buf][off..off + v.dtype.size()])
    }

    fn store_var(
        &mut self,
        v: &VarInfo,
        index: Option<i128>,
        val: &Value,
        fit: Fit,
        right: bool,
    ) -> Result<()> {
        let offs: Vec<usize> = match index {
            // Assigning to an array without an index sets every element.
            None if v.dim > 0 => (0..v.dim as usize).map(|k| v.off + k * v.stride).collect(),
            _ => vec![self.elem_off(v, index)?],
        };
        for off in offs {
            let out = &mut self.bufs[v.buf][off..off + v.dtype.size()];
            data::store(&v.dtype, val, out, fit, right)?;
        }
        Ok(())
    }

    // ---- expressions --------------------------------------------------

    fn index_of(&mut self, args: &Option<Vec<Expr>>) -> Result<Option<i128>> {
        match args {
            Some(a) if a.len() == 1 => Ok(Some(self.eval(&a[0])?.num()?.to_i128()?)),
            Some(_) => bail!("one array index is expected"),
            None => Ok(None),
        }
    }

    pub fn eval(&mut self, ex: &Expr) -> Result<Value> {
        Ok(match ex {
            Expr::Num(d) => Value::Num(*d),
            Expr::Str(s) | Expr::Hex(s) => Value::Char(s.clone()),
            Expr::Fig(f) => Value::Fig(f.clone()),
            Expr::Ind(n) => Value::Ind(self.ind(n)),
            Expr::IndArray(Some(i)) => {
                let n = self.eval(i)?.num()?.to_i128()?;
                Value::Ind(self.ind(&format!("{n:02}")))
            }
            Expr::IndArray(None) => {
                let s: Vec<u8> = (1..=99)
                    .map(|n| {
                        if self.ind(&format!("{n:02}")) {
                            0xF1
                        } else {
                            0xF0
                        }
                    })
                    .collect();
                Value::Char(s)
            }
            Expr::Omit => Value::Fig(Fig::Null),
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                match self.prog.syms.get(&name).cloned() {
                    Some(Sym::Var(v)) => {
                        let i = self.index_of(args)?;
                        if i.is_none() && v.dim > 0 {
                            bail!("{name} is an array: an index is needed here");
                        }
                        self.load_var(&v, i)?
                    }
                    Some(Sym::Const(c)) => c,
                    Some(Sym::Ds(d)) => Value::Char(self.bufs[d.buf][d.off..d.off + d.size].to_vec()),
                    Some(Sym::Klist(_)) => bail!("{name} is a key list, not a value"),
                    None => match name.as_str() {
                        "*DATE" | "UDATE" => Value::Num(Dec::from_i64(today_num(&name))),
                        "*DAY" | "UDAY" => Value::Num(Dec::from_i64(today().2 as i64)),
                        "*MONTH" | "UMONTH" => Value::Num(Dec::from_i64(today().1 as i64)),
                        "*YEAR" => Value::Num(Dec::from_i64(today().0 as i64)),
                        "UYEAR" => Value::Num(Dec::from_i64((today().0 % 100) as i64)),
                        _ if args.is_some() => bail!("{name} is not an array or a procedure ibmish knows (procedures come in a later milestone)"),
                        _ => bail!("{name} is not defined"),
                    },
                }
            }
            Expr::Bif(b, args) => self.bif(b, args)?,
            Expr::Unary(op, x) => {
                let v = self.eval(x)?;
                match op {
                    UnOp::Neg => Value::Num(v.num()?.neg()),
                    UnOp::Plus => Value::Num(v.num()?),
                    UnOp::Not => Value::Ind(!v.truth()?),
                }
            }
            Expr::Bin(op, l, r) => {
                // AND and OR stop early (ILE RPG Reference: the right
                // operand is not evaluated when the result is known).
                if *op == BinOp::And {
                    return Ok(Value::Ind(self.eval(l)?.truth()? && self.eval(r)?.truth()?));
                }
                if *op == BinOp::Or {
                    return Ok(Value::Ind(self.eval(l)?.truth()? || self.eval(r)?.truth()?));
                }
                let x = self.eval(l)?;
                let y = self.eval(r)?;
                match op {
                    BinOp::Add => match (&x, &y) {
                        (Value::Char(_), _) | (_, Value::Char(_)) => {
                            let mut s = x.chars()?;
                            s.extend(y.chars()?);
                            Value::Char(s)
                        }
                        _ => Value::Num(x.num()?.add(&y.num()?)?),
                    },
                    BinOp::Sub => Value::Num(x.num()?.sub(&y.num()?)?),
                    BinOp::Mul => Value::Num(x.num()?.mul(&y.num()?)?),
                    BinOp::Div => Value::Num(x.num()?.div(&y.num()?)?),
                    BinOp::Pow => {
                        let base = x.num()?;
                        let n = y.num()?.to_i128().map_err(|_| {
                            anyhow!("** with a fractional exponent is not supported yet")
                        })?;
                        let mut acc = Dec::from_i64(1);
                        for _ in 0..n.unsigned_abs() {
                            acc = acc.mul(&base)?;
                        }
                        if n < 0 {
                            acc = Dec::from_i64(1).div(&acc)?;
                        }
                        Value::Num(acc)
                    }
                    cmp => {
                        let o = data::compare(&x, &y)?;
                        Value::Ind(match cmp {
                            BinOp::Eq => o == Ordering::Equal,
                            BinOp::Ne => o != Ordering::Equal,
                            BinOp::Lt => o == Ordering::Less,
                            BinOp::Le => o != Ordering::Greater,
                            BinOp::Gt => o == Ordering::Greater,
                            BinOp::Ge => o != Ordering::Less,
                            _ => unreachable!(),
                        })
                    }
                }
            }
        })
    }

    /// The type of what an expression names, when it names a field.
    fn type_of(&self, ex: &Expr) -> Option<DType> {
        match ex {
            Expr::Name { parts, .. } => match self.prog.syms.get(&parts.join(".")) {
                Some(Sym::Var(v)) => Some(v.dtype),
                Some(Sym::Ds(d)) => Some(DType::Char(d.size as u32)),
                _ => None,
            },
            Expr::Ind(_) => Some(DType::Ind),
            _ => None,
        }
    }

    fn file_status(&self, args: &[Expr]) -> Result<Status> {
        match args.first() {
            None => Ok(self.last.clone()),
            Some(Expr::Name { parts, .. }) => {
                let n = parts.join(".");
                let i = self
                    .prog
                    .file_index(&n)
                    .ok_or_else(|| anyhow!("{n} is not a file"))?;
                Ok(self.files[i].status.clone())
            }
            Some(other) => bail!("a file name is expected, found {other:?}"),
        }
    }

    fn bif(&mut self, b: &str, args: &[Expr]) -> Result<Value> {
        let arg = |m: &mut Self, i: usize| -> Result<Value> {
            m.eval(
                args.get(i)
                    .ok_or_else(|| anyhow!("%{b} needs more arguments"))?,
            )
        };
        Ok(match b {
            "EOF" => Value::Ind(self.file_status(args)?.eof),
            "FOUND" => Value::Ind(self.file_status(args)?.found),
            "EQUAL" => Value::Ind(self.file_status(args)?.equal),
            "ERROR" => Value::Ind(self.last.error),
            "CHAR" => {
                let v = arg(self, 0)?;
                match v {
                    Value::Num(d) => Value::str(&char_of_num(&d)),
                    Value::Ind(x) => Value::Char(vec![if x { 0xF1 } else { 0xF0 }]),
                    other => Value::Char(other.chars()?),
                }
            }
            "TRIM" | "TRIML" | "TRIMR" => {
                let s = arg(self, 0)?.chars()?;
                let set: Vec<u8> = match args.get(1) {
                    Some(x) => self.eval(x)?.chars()?,
                    None => vec![data::BLANK],
                };
                let mut lo = 0;
                let mut hi = s.len();
                if b != "TRIMR" {
                    while lo < hi && set.contains(&s[lo]) {
                        lo += 1;
                    }
                }
                if b != "TRIML" {
                    while hi > lo && set.contains(&s[hi - 1]) {
                        hi -= 1;
                    }
                }
                Value::Char(s[lo..hi].to_vec())
            }
            "SUBST" => {
                let s = arg(self, 0)?.chars()?;
                let start = arg(self, 1)?.num()?.to_i128()?;
                if start < 1 || start as usize > s.len() + 1 {
                    bail!(
                        "RNQ0100: %SUBST start {start} is outside the string (length {}).",
                        s.len()
                    );
                }
                let from = start as usize - 1;
                let len = match args.get(2) {
                    Some(x) => self.eval(x)?.num()?.to_i128()? as usize,
                    None => s.len() - from,
                };
                if from + len > s.len() {
                    bail!(
                        "RNQ0100: %SUBST length {len} goes past the end of the string (length {}).",
                        s.len()
                    );
                }
                Value::Char(s[from..from + len].to_vec())
            }
            "LEN" => {
                let a0 = args
                    .first()
                    .ok_or_else(|| anyhow!("%LEN needs an argument"))?;
                match (self.type_of(a0), self.eval(a0)?) {
                    (Some(DType::Varchar(_)), Value::Char(c)) => {
                        Value::Num(Dec::from_i64(c.len() as i64))
                    }
                    (Some(t), _) => Value::Num(Dec::from_i64(t.len() as i64)),
                    (None, Value::Char(c)) => Value::Num(Dec::from_i64(c.len() as i64)),
                    (None, Value::Num(d)) => Value::Num(Dec::from_i64(
                        d.int_digits().max(1) as i64 + d.scale() as i64,
                    )),
                    (None, _) => Value::Num(Dec::from_i64(1)),
                }
            }
            "SIZE" => {
                let a0 = args
                    .first()
                    .ok_or_else(|| anyhow!("%SIZE needs an argument"))?;
                let n = match a0 {
                    Expr::Name { parts, .. } => match self.prog.syms.get(&parts.join(".")) {
                        Some(Sym::Var(v)) => {
                            v.dtype.size()
                                * if args.len() > 1 {
                                    v.dim.max(1) as usize
                                } else {
                                    1
                                }
                        }
                        Some(Sym::Ds(d)) => d.size,
                        _ => bail!("%SIZE of {a0:?}"),
                    },
                    _ => self.eval(a0)?.chars()?.len(),
                };
                Value::Num(Dec::from_i64(n as i64))
            }
            "ELEM" => match args.first() {
                Some(Expr::Name { parts, .. }) => match self.prog.syms.get(&parts.join(".")) {
                    Some(Sym::Var(v)) => Value::Num(Dec::from_i64(v.dim as i64)),
                    Some(Sym::Ds(d)) => Value::Num(Dec::from_i64(d.dim as i64)),
                    _ => bail!("%ELEM needs an array"),
                },
                _ => bail!("%ELEM needs an array"),
            },
            "SCAN" => {
                let pat = arg(self, 0)?.chars()?;
                let s = arg(self, 1)?.chars()?;
                let start = match args.get(2) {
                    Some(x) => self.eval(x)?.num()?.to_i128()? as usize,
                    None => 1,
                };
                let found = if pat.is_empty() || start == 0 || start > s.len() {
                    0
                } else {
                    s[start - 1..]
                        .windows(pat.len())
                        .position(|w| w == pat.as_slice())
                        .map(|p| p + start)
                        .unwrap_or(0)
                };
                Value::Num(Dec::from_i64(found as i64))
            }
            "REPLACE" => {
                let rep = arg(self, 0)?.chars()?;
                let s = arg(self, 1)?.chars()?;
                let start = match args.get(2) {
                    Some(x) => self.eval(x)?.num()?.to_i128()? as usize,
                    None => 1,
                };
                let len = match args.get(3) {
                    Some(x) => self.eval(x)?.num()?.to_i128()? as usize,
                    None => rep.len(),
                };
                let from = start.saturating_sub(1).min(s.len());
                let to = (from + len).min(s.len());
                let mut out = s[..from].to_vec();
                out.extend(&rep);
                out.extend(&s[to..]);
                Value::Char(out)
            }
            "XLATE" => {
                let from = arg(self, 0)?.chars()?;
                let to = arg(self, 1)?.chars()?;
                let s = arg(self, 2)?.chars()?;
                Value::Char(
                    s.iter()
                        .map(|c| match from.iter().position(|f| f == c) {
                            Some(p) if p < to.len() => to[p],
                            _ => *c,
                        })
                        .collect(),
                )
            }
            "UPPER" | "LOWER" => {
                let s = a(&arg(self, 0)?.chars()?);
                Value::str(&if b == "UPPER" {
                    s.to_uppercase()
                } else {
                    s.to_lowercase()
                })
            }
            "CHECK" | "CHECKR" => {
                let set = arg(self, 0)?.chars()?;
                let s = arg(self, 1)?.chars()?;
                let pos = if b == "CHECK" {
                    s.iter()
                        .position(|c| !set.contains(c))
                        .map(|p| p + 1)
                        .unwrap_or(0)
                } else {
                    s.iter()
                        .rposition(|c| !set.contains(c))
                        .map(|p| p + 1)
                        .unwrap_or(0)
                };
                Value::Num(Dec::from_i64(pos as i64))
            }
            "INT" | "INTH" => {
                let v = arg(self, 0)?;
                let d = match v {
                    Value::Char(c) => Dec::parse(&a(&c))?,
                    o => o.num()?,
                };
                Value::Num(d.rescale(0, b == "INTH")?)
            }
            "DEC" | "DECH" => {
                let v = arg(self, 0)?;
                let d = match v {
                    Value::Char(c) => Dec::parse(&a(&c))?,
                    o => o.num()?,
                };
                match (args.get(1), args.get(2)) {
                    (Some(p), Some(s)) => {
                        let p = self.eval(p)?.num()?.to_i128()? as u32;
                        let s = self.eval(s)?.num()?.to_i128()? as u32;
                        let r = d.rescale(s as u8, b == "DECH")?;
                        if !r.fits(p, s) {
                            bail!("RNQ0103: The target for a numeric operation is too small to hold the result.");
                        }
                        Value::Num(r)
                    }
                    _ => Value::Num(d),
                }
            }
            "ABS" => Value::Num(arg(self, 0)?.num()?.abs()),
            "DIV" => {
                let x = arg(self, 0)?.num()?;
                let y = arg(self, 1)?.num()?;
                Value::Num(x.div_scale(&y, 0)?)
            }
            "REM" => {
                let x = arg(self, 0)?.num()?;
                let y = arg(self, 1)?.num()?;
                Value::Num(x.rem(&y)?)
            }
            "EDITC" => {
                let v = arg(self, 0)?.num()?;
                let code = a(&arg(self, 1)?.chars()?);
                let (digits, decs) = match args.first().and_then(|x| self.type_of(x)) {
                    Some(t) if t.is_numeric() => t.precision(),
                    _ => (v.int_digits() + v.scale() as u32, v.scale() as u32),
                };
                Value::str(&edit_code(
                    &v,
                    digits,
                    decs,
                    code.chars().next().unwrap_or('Z'),
                ))
            }
            "ADDR" | "PADDR" => Value::Fig(Fig::Null),
            "NULLIND" => Value::Ind(false),
            "PARMS" => Value::Num(Dec::from_i64(self.prog.entry.len() as i64)),
            other => bail!("%{other} is not supported yet"),
        })
    }

    // ---- assignment -----------------------------------------------------

    fn assign(&mut self, target: &Expr, val: &Value, fit: Fit, right: bool) -> Result<()> {
        match target {
            Expr::Ind(n) => {
                let v = val.truth()?;
                self.set_ind(n, v);
            }
            Expr::IndArray(Some(i)) => {
                let n = self.eval(i)?.num()?.to_i128()?;
                let v = val.truth()?;
                self.set_ind(&format!("{n:02}"), v);
            }
            Expr::IndArray(None) => {
                let s = match val {
                    Value::Fig(Fig::On) => vec![0xF1; 99],
                    Value::Fig(Fig::Off) => vec![0xF0; 99],
                    other => other.chars()?,
                };
                for n in 1..=99usize {
                    let on = s.get(n - 1) == Some(&0xF1);
                    self.set_ind(&format!("{n:02}"), on);
                }
            }
            Expr::Bif(b, args) if b == "SUBST" => {
                // %SUBST as a target: replace part of a character field.
                let base = args
                    .first()
                    .ok_or_else(|| anyhow!("%SUBST needs a field"))?;
                let mut cur = self.eval(base)?.chars()?;
                let start = self.eval(&args[1])?.num()?.to_i128()? as usize;
                let len = match args.get(2) {
                    Some(x) => self.eval(x)?.num()?.to_i128()? as usize,
                    None => cur.len() + 1 - start,
                };
                if start == 0 || start - 1 + len > cur.len() {
                    bail!("RNQ0100: %SUBST is outside the field.");
                }
                let mut piece = vec![data::BLANK; len];
                data::store(&DType::Char(len as u32), val, &mut piece, fit, right)?;
                cur[start - 1..start - 1 + len].copy_from_slice(&piece);
                self.assign(base, &Value::Char(cur), fit, false)?;
            }
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                match self.prog.syms.get(&name).cloned() {
                    Some(Sym::Var(v)) => {
                        let i = self.index_of(args)?;
                        self.store_var(&v, i, val, fit, right)?;
                    }
                    Some(Sym::Ds(d)) => {
                        let t = DType::Char(d.size as u32);
                        let out = &mut self.bufs[d.buf][d.off..d.off + d.size];
                        data::store(&t, val, out, fit, right)?;
                    }
                    Some(_) => bail!("{name} cannot be changed"),
                    None => bail!("{name} is not defined"),
                }
            }
            other => bail!("cannot assign to {other:?}"),
        }
        Ok(())
    }

    fn set_result_inds(&mut self, ind: &ResultInd, hi: bool, lo: bool, eq: bool) {
        if let Some(i) = &ind.hi {
            self.set_ind(i, hi);
        }
        if let Some(i) = &ind.lo {
            self.set_ind(i, lo);
        }
        if let Some(i) = &ind.eq {
            self.set_ind(i, eq);
        }
    }

    fn num_inds(&mut self, ind: &ResultInd, d: &Dec) {
        if !ind.is_empty() {
            self.set_result_inds(ind, d.mantissa() > 0, d.mantissa() < 0, d.is_zero());
        }
    }

    // ---- files ----------------------------------------------------------

    fn open_file(&mut self, i: usize) -> Result<()> {
        let fi = &self.prog.files[i];
        if let Some(d) = &fi.display {
            self.block(crate::authority::check(
                self.io.client,
                &self.io.user,
                &d.library,
                &d.file.name.to_ascii_uppercase(),
                "*FILE",
                crate::authority::Aut::Use,
            ))?;
            self.files[i].ws_open = true;
            return Ok(());
        }
        if fi.decl.device == "WORKSTN" {
            bail!(
                "WORKSTN file {} is program-described; the native engine needs a display file (E in position 22)",
                fi.decl.name
            );
        }
        let Some(fref) = fi.fref.clone() else {
            return Ok(());
        };
        let update = fi.decl.usage == 'U';
        let need = if update || fi.decl.usage == 'O' || fi.decl.add {
            crate::authority::Aut::Change
        } else {
            crate::authority::Aut::Use
        };
        self.block(crate::authority::check(
            self.io.client,
            &self.io.user,
            &fref.library,
            &fref.file.name,
            "*FILE",
            need,
        ))?;
        let mut open = OpenFile::new(fref.clone());
        if update {
            let (lib, pf) = self.block(crate::catalog::physical_file_of(
                self.io.client,
                &fref.library,
                &fref.file.name,
            ))?;
            open = open.for_update(format!("{lib}/{pf}"));
        }
        self.files[i].open = Some(open);
        Ok(())
    }

    /// Program fields → record.
    fn record_of(&mut self, i: usize) -> Result<Record> {
        let fi = &self.prog.files[i];
        let fref = fi
            .fref
            .as_ref()
            .ok_or_else(|| anyhow!("{} is not a database file", fi.decl.name))?;
        let mut rec = Record::new();
        for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
            let v = self.eval(&Expr::name(pname))?;
            let text = match v {
                Value::Num(d) => d.rescale(fd.decimals as u8, false)?.to_string(),
                other => a(&other.chars()?),
            };
            rec.insert(fd.name.clone(), text);
        }
        Ok(rec)
    }

    /// Record → program fields.
    fn fill_fields(&mut self, i: usize, rec: &Record) -> Result<()> {
        let fi = self.prog.files[i].clone();
        let fref = fi.fref.as_ref().unwrap();
        for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
            let Some(text) = rec.get(&fd.name) else {
                continue;
            };
            let v = if fd.data_type == crate::dds::DataType::Char {
                Value::str(text)
            } else {
                Value::Num(Dec::parse(text)?)
            };
            self.assign(
                &Expr::name(pname),
                &v,
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    fn key_values(&mut self, key: &[Expr]) -> Result<Vec<String>> {
        // A KLIST name stands for its key fields.
        let exprs: Vec<Expr> = match key {
            [Expr::Name { parts, args: None }] => match self.prog.syms.get(&parts.join(".")) {
                Some(Sym::Klist(k)) => k.clone(),
                _ => key.to_vec(),
            },
            _ => key.to_vec(),
        };
        let mut out = Vec::new();
        for k in &exprs {
            out.push(match self.eval(k)? {
                Value::Num(d) => d.to_string(),
                Value::Fig(Fig::LoVal) | Value::Fig(Fig::HiVal) => {
                    bail!("*LOVAL/*HIVAL here is handled by the caller")
                }
                other => a(&other.chars()?),
            });
        }
        Ok(out)
    }

    fn file_op(
        &mut self,
        op: FileOp,
        file: &str,
        key: &Option<Vec<Expr>>,
        ext: &Ext,
        ind: &ResultInd,
    ) -> Result<()> {
        let i = self
            .prog
            .file_index(file)
            .ok_or_else(|| anyhow!("{file} is not a file of this program"))?;
        let device = self.prog.files[i].decl.device.clone();
        if device == "WORKSTN" {
            return self.workstn_op(op, i, file, key, ind);
        }
        if device != "DISK" {
            bail!(
                "{} files are not supported by the native engine yet ({op:?} {file})",
                device
            );
        }
        if self.files[i].open.is_none() && !matches!(op, FileOp::Open | FileOp::Close) {
            bail!("CPF4135: File {file} is not open.");
        }
        let lock = !ext.no_lock;
        let client = self.io.client;
        let job = self.io.job_number;
        // *LOVAL/*START and *HIVAL/*END positioning.
        let special = match key.as_deref() {
            Some([Expr::Fig(Fig::LoVal)]) => Some(false),
            Some([Expr::Fig(Fig::HiVal)]) => Some(true),
            Some([Expr::Name { parts, .. }]) if parts[0] == "*START" => Some(false),
            Some([Expr::Name { parts, .. }]) if parts[0] == "*END" => Some(true),
            _ => None,
        };
        let keys = match (key, special) {
            (Some(k), None) => Some(self.key_values(k)?),
            _ => None,
        };
        let rt = self.io.rt.clone();
        let result: Result<Option<ReadResult>> = (|| {
            let f = self.files[i]
                .open
                .as_mut()
                .ok_or_else(|| anyhow!("file not open"))?;
            Ok(match op {
                FileOp::SetLL | FileOp::SetGT => {
                    let eq = match (op, special) {
                        (FileOp::SetLL, Some(false)) => rt.block_on(f.setll(client, &[]))?,
                        (FileOp::SetGT, Some(true)) | (FileOp::SetLL, Some(true)) => {
                            rt.block_on(f.setgt(client, &[]))?;
                            false
                        }
                        (FileOp::SetGT, Some(false)) => rt.block_on(f.setll(client, &[]))?,
                        (FileOp::SetLL, None) => {
                            rt.block_on(f.setll(client, keys.as_deref().unwrap_or(&[])))?
                        }
                        _ => rt.block_on(f.setgt(client, keys.as_deref().unwrap_or(&[])))?,
                    };
                    Some(ReadResult {
                        found: eq,
                        eof: false,
                        record: None,
                        rrn: None,
                    })
                }
                FileOp::Chain => {
                    let r = rt.block_on(f.chain(client, keys.as_deref().unwrap_or(&[])))?;
                    Some(rt.block_on(f.lock_after_read(client, r, lock, job))?)
                }
                FileOp::Read | FileOp::ReadP | FileOp::ReadE | FileOp::ReadPE => {
                    let r = match op {
                        FileOp::Read => rt.block_on(f.read(client))?,
                        FileOp::ReadP => rt.block_on(f.read_previous(client))?,
                        FileOp::ReadE => rt.block_on(f.read_equal(client, keys.as_deref()))?,
                        _ => rt.block_on(f.read_previous_equal(client, keys.as_deref()))?,
                    };
                    Some(rt.block_on(f.lock_after_read(client, r, lock, job))?)
                }
                FileOp::Unlock => {
                    rt.block_on(f.release(client))?;
                    None
                }
                _ => None,
            })
        })();
        let result = match result {
            Ok(r) => r,
            Err(err) if ext.error || ind.lo.is_some() => {
                self.last = Status {
                    error: true,
                    ..Status::default()
                };
                self.files[i].status.error = true;
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, true);
                }
                let _ = err;
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        match op {
            FileOp::SetLL | FileOp::SetGT => {
                let r = result.unwrap();
                let st = Status {
                    found: r.found,
                    equal: op == FileOp::SetLL && r.found,
                    eof: false,
                    error: false,
                };
                self.files[i].status = st.clone();
                self.last = st;
                // SETLL: positions 71-72 on when no record has a key >=
                // the search argument; 75-76 on for an exact match.
                if let Some(eq) = &ind.eq {
                    self.set_ind(eq, r.found && op == FileOp::SetLL);
                }
            }
            FileOp::Chain | FileOp::Read | FileOp::ReadP | FileOp::ReadE | FileOp::ReadPE => {
                let r = result.unwrap();
                if let Some(rec) = &r.record {
                    self.fill_fields(i, rec)?;
                }
                let st = if op == FileOp::Chain {
                    Status {
                        found: r.found,
                        eof: false,
                        equal: false,
                        error: false,
                    }
                } else {
                    Status {
                        found: r.record.is_some(),
                        eof: r.eof,
                        equal: false,
                        error: false,
                    }
                };
                self.files[i].status = st.clone();
                self.last = st;
                // CHAIN: 71-72 on when not found; reads: 75-76 at end of
                // file (ILE RPG Reference, CHAIN and READ).
                if op == FileOp::Chain {
                    if let Some(hi) = &ind.hi {
                        self.set_ind(hi, !r.found);
                    }
                } else if let Some(eq) = &ind.eq {
                    self.set_ind(eq, r.eof);
                }
            }
            FileOp::Write | FileOp::Update | FileOp::Delete => {
                let fref = self.prog.files[i].fref.clone().unwrap();
                self.block(crate::authority::check(
                    client,
                    &self.io.user,
                    &fref.library,
                    &fref.file.name,
                    "*FILE",
                    crate::authority::Aut::Change,
                ))?;
                let rec = if op == FileOp::Delete {
                    Record::new()
                } else {
                    self.record_of(i)?
                };
                let f = self.files[i].open.as_mut().unwrap();
                let r: Result<()> = match op {
                    FileOp::Write => rt.block_on(f.write(client, &rec)).map(|_| ()),
                    FileOp::Update => rt.block_on(async {
                        f.update(client, &rec).await?;
                        f.release(client).await
                    }),
                    _ => rt.block_on(async {
                        if let Some(k) = &keys {
                            let r = f.chain(client, k).await?;
                            if !r.found {
                                bail!("CPF5013: The record to delete was not found.");
                            }
                        }
                        f.delete(client).await?;
                        f.release(client).await
                    }),
                };
                match r {
                    Ok(()) => {
                        self.last = Status {
                            found: true,
                            ..Status::default()
                        };
                    }
                    Err(err) if ext.error || ind.lo.is_some() => {
                        self.last = Status {
                            error: true,
                            ..Status::default()
                        };
                        if let Some(lo) = &ind.lo {
                            self.set_ind(lo, true);
                        }
                        let _ = err;
                    }
                    Err(err) => return Err(err),
                }
            }
            FileOp::Open => {
                if self.files[i].open.is_some() {
                    bail!("CPF4161: File {file} is already open.");
                }
                self.open_file(i)?;
            }
            FileOp::Close => {
                if let Some(mut f) = self.files[i].open.take() {
                    let _ = rt.block_on(f.release(client));
                }
            }
            FileOp::Unlock | FileOp::Feod => {}
            FileOp::Exfmt | FileOp::ReadC => bail!("{op:?} {file}: not a display file"),
        }
        Ok(())
    }

    // ---- data areas (IN, OUT; ILE RPG Reference) ------------------------

    fn data_area(&mut self, input: bool, lock: bool, field: &str) -> Result<()> {
        let name = self
            .prog
            .dtaaras
            .get(field)
            .cloned()
            .ok_or_else(|| anyhow!("{field} is not defined as a data area (DEFINE *DTAARA)"))?;
        let libl = self.io.libl.clone();
        let cur = crate::commands::DEFAULT_CURRENT_LIBRARY;
        let client = self.io.client;
        let d = self.block(crate::dtaara::resolve(client, &name, &libl, cur))?;
        let key = (d.library.clone(), d.name.clone());
        let held = self.dtaara_locks.contains(&key);
        if input {
            self.block(crate::authority::check(
                client,
                &self.io.user,
                &d.library,
                &d.name,
                "*DTAARA",
                crate::authority::Aut::Use,
            ))?;
            let take = lock && !held;
            let d = self.block(crate::dtaara::read(client, &name, &libl, cur, take))?;
            if take {
                self.dtaara_locks.push(key);
            }
            let numeric = self
                .type_of(&Expr::name(field))
                .is_some_and(|t| t.is_numeric());
            let v = if numeric {
                Value::Num(Dec::parse(d.value.trim())?)
            } else {
                Value::str(&d.value)
            };
            self.assign(
                &Expr::name(field),
                &v,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        } else {
            self.block(crate::authority::check(
                client,
                &self.io.user,
                &d.library,
                &d.name,
                "*DTAARA",
                crate::authority::Aut::Change,
            ))?;
            let v = self.eval(&Expr::name(field))?;
            let text = match v {
                Value::Num(n) => n.to_string(),
                other => a(&other.chars()?).trim_end().to_string(),
            };
            self.block(crate::dtaara::write(client, &d, &text, held))?;
            self.dtaara_locks.retain(|k| *k != key);
        }
        Ok(())
    }

    // ---- display files (WORKSTN) ----------------------------------------

    /// Indicators 01-99 as a display file sees them: *IN01-*IN99, or the
    /// INDDS data structure.
    fn ws_inds(&self, d: &Display) -> [bool; 100] {
        let mut out = [false; 100];
        match d.indds.as_ref().and_then(|n| self.prog.syms.get(n)) {
            Some(Sym::Ds(ds)) => {
                let bytes = &self.bufs[ds.buf][ds.off..ds.off + ds.size];
                for (n, slot) in out.iter_mut().enumerate().skip(1) {
                    *slot = bytes.get(n - 1) == Some(&0xF1);
                }
            }
            _ => {
                for (n, slot) in out.iter_mut().enumerate().skip(1) {
                    *slot = self.ind(&format!("{n:02}"));
                }
            }
        }
        out
    }

    fn ws_set_ind(&mut self, d: &Display, n: u8, v: bool) {
        if !(1..=99).contains(&n) {
            return;
        }
        match d.indds.as_ref().and_then(|x| self.prog.syms.get(x)) {
            Some(Sym::Ds(ds)) => {
                let at = ds.off + n as usize - 1;
                if at < ds.off + ds.size {
                    self.bufs[ds.buf][at] = if v { 0xF1 } else { 0xF0 };
                }
            }
            _ => self.set_ind(&format!("{n:02}"), v),
        }
    }

    /// The program fields of a record format, as display text by display
    /// field name (numbers as plain decimal text, "-12.50").
    fn ws_values(&mut self, d: &Display, format: &str) -> Result<BTreeMap<String, String>> {
        let rec = d.file.record(format);
        let mut out = BTreeMap::new();
        for (pname, dname) in d.fields(format).unwrap_or_default() {
            let decimals = rec
                .and_then(|r| r.fields.iter().find(|f| &f.name == dname))
                .and_then(|f| f.decimals);
            let v = self.eval(&Expr::name(pname))?;
            let text = match (v, decimals) {
                (Value::Num(n), Some(dec)) => n.rescale(dec as u8, false)?.to_string(),
                (Value::Num(n), None) => n.to_string(),
                (other, _) => a(&other.chars()?),
            };
            out.insert(dname.clone(), text);
        }
        Ok(out)
    }

    /// Display values → the program fields of a record format.
    fn ws_fill(
        &mut self,
        d: &Display,
        format: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<()> {
        let rec = d.file.record(format);
        for (pname, dname) in d.fields(format).unwrap_or_default() {
            let Some(text) = values.get(dname) else {
                continue;
            };
            let numeric = rec
                .and_then(|r| r.fields.iter().find(|f| &f.name == dname))
                .is_some_and(|f| f.is_numeric());
            let v = if numeric {
                Value::Num(Dec::parse(text.trim())?)
            } else {
                Value::str(text)
            };
            self.assign(
                &Expr::name(pname),
                &v,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    fn workstn_op(
        &mut self,
        op: FileOp,
        i: usize,
        name: &str,
        key: &Option<Vec<Expr>>,
        ind: &ResultInd,
    ) -> Result<()> {
        let prog = self.prog;
        let d = prog.files[i]
            .display
            .as_ref()
            .ok_or_else(|| anyhow!("{name} is not a display file"))?;
        match op {
            FileOp::Open => {
                if self.files[i].ws_open {
                    bail!("CPF4161: File {name} is already open.");
                }
                return self.open_file(i);
            }
            FileOp::Close => {
                self.files[i].ws_open = false;
                return Ok(());
            }
            _ => {}
        }
        if !self.files[i].ws_open {
            bail!("CPF4135: File {} is not open.", prog.files[i].decl.name);
        }
        if d.fields(name).is_none() {
            bail!(
                "{op:?} {name}: name a record format of display file {}",
                d.file.name
            );
        }
        let format = name.to_string();
        let file = d.file.clone();
        let inds = self.ws_inds(d);
        let set_status = |m: &mut Self, st: Status| {
            m.files[i].status = st.clone();
            m.last = st;
        };
        match op {
            FileOp::Write => {
                let values = self.ws_values(d, &format)?;
                match d.sfile.get(&format) {
                    Some(rrn_field) => {
                        let rrn = self.eval(&Expr::name(rrn_field))?.num()?.to_i128()?;
                        let rrn = i64::try_from(rrn).unwrap_or(-1);
                        self.ws.sfl_write(&file, &format, rrn, values, inds)?;
                    }
                    None => self.ws.write(&file, &format, values, inds)?,
                }
                set_status(self, Status::default());
            }
            FileOp::Update => {
                let values = self.ws_values(d, &format)?;
                self.ws.sfl_update(&file, &format, values, inds)?;
                set_status(self, Status::default());
            }
            FileOp::Exfmt => {
                let Some(display) = self.io.display.clone() else {
                    bail!(
                        "Display file {} cannot be used here: this job has no display (it is not interactive).",
                        d.file.name
                    );
                };
                let values = self.ws_values(d, &format)?;
                let frame = self.ws.frame(&file, &format, values, inds)?;
                let (tx, rx) = tokio::sync::oneshot::channel();
                let resp = self.block(async move {
                    display
                        .send(crate::bridge::DisplayRequest { frame, reply: tx })
                        .await
                        .map_err(|_| anyhow!("the job's display is gone"))?;
                    rx.await
                        .map_err(|_| anyhow!("the job's display is gone"))?
                        .map_err(|e| anyhow!(e))
                })?;
                self.ws.apply(&file, &format, &resp);
                self.ws_fill(d, &format, &resp.values)?;
                for (n, v) in &resp.indicators {
                    self.ws_set_ind(d, *n, *v);
                }
                // *INKA-*INKY: on for the function key pressed, off for
                // the others (ILE RPG Reference, function key indicators).
                for (k, letter) in FKEYS.iter().enumerate() {
                    let on = resp.aid == format!("F{}", k + 1);
                    self.set_ind(&format!("K{}", *letter as char), on);
                }
                set_status(self, Status::default());
            }
            FileOp::ReadC => {
                let r = self.ws.sfl_readc(&file, &format)?;
                let found = r.is_some();
                if let Some((rrn, values)) = r {
                    self.ws_fill(d, &format, &values)?;
                    if let Some(f) = d.sfile.get(&format) {
                        self.assign(
                            &Expr::name(f),
                            &Value::Num(Dec::from_i64(rrn as i64)),
                            Fit::Strict { half_adjust: false },
                            false,
                        )?;
                    }
                }
                set_status(
                    self,
                    Status {
                        found,
                        eof: !found,
                        ..Status::default()
                    },
                );
                // READC: positions 75-76 on at end of file.
                if let Some(eq) = &ind.eq {
                    self.set_ind(eq, !found);
                }
            }
            FileOp::Chain => {
                let k = key
                    .as_deref()
                    .and_then(|k| k.first())
                    .ok_or_else(|| anyhow!("CHAIN {format}: a relative record number is needed"))?;
                let rrn = self.eval(k)?.num()?.to_i128()?;
                let r = self
                    .ws
                    .sfl_chain(&file, &format, i64::try_from(rrn).unwrap_or(0))?;
                let found = r.is_some();
                if let Some(values) = r {
                    self.ws_fill(d, &format, &values)?;
                }
                set_status(
                    self,
                    Status {
                        found,
                        ..Status::default()
                    },
                );
                if let Some(hi) = &ind.hi {
                    self.set_ind(hi, !found);
                }
            }
            other => bail!(
                "{other:?} of display file record {format} is not supported by the native engine yet"
            ),
        }
        Ok(())
    }

    // ---- statements -----------------------------------------------------

    fn text_of(&self, v: &Value) -> Result<String> {
        Ok(match v {
            Value::Num(d) => char_of_num(d),
            Value::Ind(b) => if *b { "1" } else { "0" }.into(),
            Value::Fig(_) => String::new(),
            Value::Char(c) => a(c),
        })
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Eval {
                target,
                value,
                ext,
                right,
            } => {
                let v = self.eval(value)?;
                self.assign(
                    target,
                    &v,
                    Fit::Strict {
                        half_adjust: ext.half_adjust,
                    },
                    *right,
                )?;
            }
            Stmt::SetInd { on, inds } => {
                for i in inds {
                    self.set_ind(i, *on);
                }
            }
            Stmt::Dsply { message, response } => {
                let text = match message {
                    Some(m) => {
                        let v = self.eval(m)?;
                        self.text_of(&v)?
                    }
                    None => String::new(),
                };
                self.messages.push(text.trim_end().to_string());
                if let Some(r) = response {
                    // No operator to answer in a batch call: the response
                    // stays as it was.
                    let _ = r;
                }
            }
            Stmt::File {
                op,
                file,
                key,
                ds,
                ext,
                ind,
            } => {
                if ds.is_some() && matches!(op, FileOp::Write | FileOp::Update) {
                    bail!("{op:?} with a result data structure is not supported yet");
                }
                self.file_op(*op, file, key, ext, ind)?;
            }
            Stmt::Arith {
                op,
                f1,
                f2,
                result,
                ext,
                ind,
            } => {
                let fit = Fit::Truncate {
                    half_adjust: ext.half_adjust,
                };
                let cur = || -> Expr { result.clone() };
                let v = match op {
                    Arith::ZAdd => self.eval(f2)?.num()?,
                    Arith::ZSub => self.eval(f2)?.num()?.neg(),
                    Arith::Add | Arith::Sub | Arith::Mult | Arith::Div => {
                        let a1 = match f1 {
                            Some(x) => self.eval(x)?.num()?,
                            None => self.eval(&cur())?.num()?,
                        };
                        let a2 = self.eval(f2)?.num()?;
                        match op {
                            Arith::Add => a1.add(&a2)?,
                            Arith::Sub => a1.sub(&a2)?,
                            Arith::Mult => a1.mul(&a2)?,
                            _ => {
                                // DIV: the quotient gets the result
                                // field's decimal places (truncated unless
                                // H); MVR takes the remainder.
                                let (_, decs) = self
                                    .type_of(result)
                                    .map(|t| t.precision())
                                    .unwrap_or((0, 0));
                                let q = a1.div_scale(
                                    &a2,
                                    decs as u8 + if ext.half_adjust { 1 } else { 0 },
                                )?;
                                let qt = q.rescale(decs as u8, false)?;
                                self.remainder = a1.sub(&qt.mul(&a2)?)?;
                                q
                            }
                        }
                    }
                    Arith::Mvr => self.remainder,
                };
                self.assign(result, &Value::Num(v), fit, false)?;
                let stored = self.eval(result)?.num()?;
                self.num_inds(ind, &stored);
            }
            Stmt::Move {
                left,
                pad,
                from,
                to,
                ind,
            } => self.do_move(*left, *pad, from, to, ind)?,
            Stmt::Comp { f1, f2, ind } => {
                let x = self.eval(f1)?;
                let y = self.eval(f2)?;
                let o = data::compare(&x, &y)?;
                self.set_result_inds(
                    ind,
                    o == Ordering::Greater,
                    o == Ordering::Less,
                    o == Ordering::Equal,
                );
            }
            Stmt::Clear(t) | Stmt::Reset(t) => {
                // CLEAR: blanks, zeros, '0' by type. (RESET to the INZ
                // values: same as CLEAR here for now.)
                match t {
                    Expr::Name { parts, args: None } => {
                        match self.prog.syms.get(&parts.join(".")).cloned() {
                            Some(Sym::Var(v)) => {
                                for k in 0..v.dim.max(1) as usize {
                                    let at = v.off + k * v.stride;
                                    self.bufs[v.buf][at..at + v.dtype.size()]
                                        .copy_from_slice(&v.dtype.initial());
                                }
                            }
                            Some(Sym::Ds(d)) => {
                                let prefix = if d.qualified {
                                    format!("{}.", parts.join("."))
                                } else {
                                    String::new()
                                };
                                for sf in &d.subfields {
                                    if let Some(Sym::Var(v)) =
                                        self.prog.syms.get(&format!("{prefix}{sf}")).cloned()
                                    {
                                        for k in 0..v.dim.max(1) as usize {
                                            let at = v.off + k * v.stride;
                                            self.bufs[v.buf][at..at + v.dtype.size()]
                                                .copy_from_slice(&v.dtype.initial());
                                        }
                                    }
                                }
                            }
                            _ => bail!("CLEAR of {} is not supported", parts.join(".")),
                        }
                    }
                    Expr::Ind(n) => self.set_ind(n, false),
                    Expr::IndArray(None) => {
                        for n in 1..=99 {
                            self.set_ind(&format!("{n:02}"), false);
                        }
                    }
                    other => {
                        let t = self
                            .type_of(other)
                            .ok_or_else(|| anyhow!("CLEAR of {other:?}"))?;
                        let v = if t.is_numeric() {
                            Value::Num(Dec::ZERO)
                        } else {
                            Value::Fig(Fig::Blanks)
                        };
                        self.assign(other, &v, Fit::Strict { half_adjust: false }, false)?;
                    }
                }
            }
            Stmt::Op {
                code,
                f1,
                f2,
                result,
                ext: _,
                ind,
            } => self.misc_op(code, f1, f2, result, ind)?,
            Stmt::Nop => {}
            Stmt::CallP(e) => {
                bail!("CALLP / procedure calls are not supported by the native engine yet: {e:?}")
            }
            Stmt::Call { program, .. } => {
                bail!("CALL from RPG is not supported by the native engine yet: {program:?}")
            }
            Stmt::Unsupported(op) => bail!("operation {op} is not supported yet"),
        }
        Ok(())
    }

    /// The bytes MOVE and MOVEL work with: character data as is, numbers
    /// as their zoned digits (sign in the zone of the last digit).
    fn move_image(&mut self, ex: &Expr) -> Result<Vec<u8>> {
        let t = self.type_of(ex);
        let v = self.eval(ex)?;
        Ok(match v {
            Value::Num(d) => {
                let (digits, decs) = match t {
                    Some(t) if t.is_numeric() => t.precision(),
                    _ => ((d.int_digits() + d.scale() as u32).max(1), d.scale() as u32),
                };
                let zt = DType::Zoned(digits, decs);
                let mut b = zt.initial();
                data::store(
                    &zt,
                    &Value::Num(d),
                    &mut b,
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
                b
            }
            Value::Fig(f) => {
                let n = t.map(|t| t.len() as usize).unwrap_or(1);
                let mut b = vec![0u8; n];
                data::store(
                    &DType::Char(n as u32),
                    &Value::Fig(f),
                    &mut b,
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
                b
            }
            other => other.chars()?,
        })
    }

    fn do_move(
        &mut self,
        left: bool,
        pad: bool,
        from: &Expr,
        to: &Expr,
        ind: &ResultInd,
    ) -> Result<()> {
        let src = self.move_image(from)?;
        let tt = self
            .type_of(to)
            .ok_or_else(|| anyhow!("MOVE needs a field as the result"))?;
        let mut dst = self.move_image(to)?;
        let n = dst.len();
        if pad {
            let fill = if tt.is_numeric() { 0xF0 } else { data::BLANK };
            dst.fill(fill);
        }
        let k = src.len().min(n);
        if left {
            dst[..k].copy_from_slice(&src[..k]);
        } else {
            dst[n - k..].copy_from_slice(&src[src.len() - k..]);
        }
        let val = if tt.is_numeric() {
            // The result's digits; the sign comes from the zone of the
            // last byte.
            let (_, decs) = tt.precision();
            let mut digits: Vec<u8> = dst.iter().map(|b| 0xF0 | (b & 0x0F).min(9)).collect();
            if let Some(last) = dst.last() {
                let neg = last >> 4 == 0xD;
                let l = digits.len() - 1;
                digits[l] = if neg { 0xD0 } else { 0xF0 } | (last & 0x0F).min(9);
            }
            Value::Num(data::decode_num(
                &DType::Zoned(digits.len() as u32, decs),
                &digits,
            )?)
        } else {
            Value::Char(dst)
        };
        self.assign(to, &val, Fit::Truncate { half_adjust: false }, false)?;
        if !ind.is_empty() {
            let v = self.eval(to)?;
            match v {
                Value::Num(d) => self.num_inds(ind, &d),
                Value::Char(c) => {
                    let blank = c.iter().all(|b| *b == data::BLANK);
                    if let Some(eq) = &ind.eq {
                        self.set_ind(eq, blank);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn misc_op(
        &mut self,
        code: &str,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ind: &ResultInd,
    ) -> Result<()> {
        let need = |x: &Option<Expr>, what: &str| -> Result<Expr> {
            x.clone().ok_or_else(|| anyhow!("{code}: {what} is needed"))
        };
        match code {
            "CAT" => {
                // CAT f1:blanks f2 → result. (Blanks count in factor 2
                // after a colon: not parsed yet, so none.)
                let a1 = match f1 {
                    Some(x) => self.eval(x)?.chars()?,
                    None => self.eval(&need(result, "a result")?)?.chars()?,
                };
                let mut s = a1;
                while s.last() == Some(&data::BLANK) {
                    s.pop();
                }
                s.extend(self.eval(&need(f2, "factor 2")?)?.chars()?);
                self.assign(
                    &need(result, "a result")?,
                    &Value::Char(s),
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
            }
            "TIME" => {
                let (h, m, s) = now_hms();
                let r = need(result, "a result")?;
                let digits = self.type_of(&r).map(|t| t.precision().0).unwrap_or(6);
                let (y, mo, d) = today();
                let v = if digits >= 12 {
                    format!(
                        "{h:02}{m:02}{s:02}{mo:02}{d:02}{:0w$}",
                        if digits >= 14 { y } else { y % 100 },
                        w = if digits >= 14 { 4 } else { 2 }
                    )
                } else {
                    format!("{h:02}{m:02}{s:02}")
                };
                self.assign(
                    &r,
                    &Value::Num(Dec::parse(&v)?),
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
            }
            "SCAN" | "CHECK" | "CHECKR" | "SUBST" | "XLATE" | "LOOKUP" | "MOVEA" | "XFOOT"
            | "SQRT" | "TESTN" | "OCCUR" | "SORTA" => {
                bail!("operation {code} is not supported by the native engine yet")
            }
            "DEFINE" | "COMMIT" | "ROLBK" | "FORCE" | "DUMP" | "DEALLOC" => {}
            "EXCEPT" => bail!("EXCEPT (printer output) is not supported by the native engine yet"),
            "IN" | "OUT" => {
                let lock = expr_name(f1).as_deref() == Some("*LOCK");
                let target =
                    expr_name(f2).ok_or_else(|| anyhow!("{code}: a data area is needed"))?;
                let fields: Vec<String> = if target == "*DTAARA" {
                    let mut v: Vec<String> = self.prog.dtaaras.keys().cloned().collect();
                    v.sort();
                    v
                } else {
                    vec![target]
                };
                for field in fields {
                    self.data_area(code == "IN", lock, &field)?;
                }
            }
            other => bail!("operation {other} is not supported yet"),
        }
        let _ = ind;
        Ok(())
    }

    fn conditions_met(&self, cond: &Conditions) -> bool {
        cond.iter().all(|(neg, i)| self.ind(i) != *neg)
    }

    fn run_code(&mut self, code: &Code) -> Result<Flow> {
        let mut pc = 0usize;
        // Active MONITOR groups: (handlers, end of body, end).
        let mut monitors: Vec<MonitorGroup> = Vec::new();
        while pc < code.len() {
            let i = &code[pc];
            if !i.cond.is_empty() && !self.conditions_met(&i.cond) {
                pc += 1;
                continue;
            }
            let step: Result<Option<usize>> = (|| -> Result<Option<usize>> {
                Ok(match &i.ins {
                    Ins::Stmt(s) => {
                        self.stmt(s)?;
                        None
                    }
                    Ins::Jump(t) => Some(*t),
                    Ins::JumpIfNot(c, t) => {
                        if self.eval(c)?.truth()? {
                            None
                        } else {
                            Some(*t)
                        }
                    }
                    Ins::LoopTest {
                        var,
                        limit,
                        down,
                        end,
                    } => {
                        let v = self.eval(var)?.num()?;
                        match limit {
                            Some(l) => {
                                let l = self.eval(l)?.num()?;
                                let past = if *down { v < l } else { v > l };
                                if past {
                                    Some(*end)
                                } else {
                                    None
                                }
                            }
                            None => None,
                        }
                    }
                    Ins::LoopStep { var, by, down } => {
                        let step = match by {
                            Some(b) => self.eval(b)?.num()?,
                            None => Dec::from_i64(1),
                        };
                        let v = self.eval(var)?.num()?;
                        let n = if *down { v.sub(&step)? } else { v.add(&step)? };
                        self.assign(
                            var,
                            &Value::Num(n),
                            Fit::Strict { half_adjust: false },
                            false,
                        )?;
                        None
                    }
                    Ins::ExSr(name) => {
                        let sub = self
                            .prog
                            .routines
                            .subroutines
                            .get(name)
                            .ok_or_else(|| anyhow!("subroutine {name} is not defined"))?;
                        match self.run_code(sub)? {
                            Flow::Return(v) => return Err(ReturnSignal(v).into()),
                            _ => None,
                        }
                    }
                    Ins::Return(v) => {
                        let val = match v {
                            Some(x) => Some(self.eval(x)?),
                            None => None,
                        };
                        return Err(ReturnSignal(val).into());
                    }
                    Ins::LeaveSr => return Err(LeaveSrSignal.into()),
                    Ins::Cab {
                        cmp,
                        f1,
                        f2,
                        to,
                        ind,
                    } => {
                        let x = self.eval(f1)?;
                        let y = self.eval(f2)?;
                        let o = data::compare(&x, &y)?;
                        self.set_result_inds(
                            ind,
                            o == Ordering::Greater,
                            o == Ordering::Less,
                            o == Ordering::Equal,
                        );
                        let hit = match cmp {
                            None => true,
                            Some(BinOp::Eq) => o == Ordering::Equal,
                            Some(BinOp::Ne) => o != Ordering::Equal,
                            Some(BinOp::Lt) => o == Ordering::Less,
                            Some(BinOp::Le) => o != Ordering::Greater,
                            Some(BinOp::Gt) => o == Ordering::Greater,
                            Some(BinOp::Ge) => o != Ordering::Less,
                            _ => false,
                        };
                        if hit && *to != usize::MAX {
                            Some(*to)
                        } else {
                            None
                        }
                    }
                    Ins::Monitor {
                        handlers,
                        end_body,
                        end,
                    } => {
                        monitors.push((handlers.clone(), *end_body, *end));
                        None
                    }
                    Ins::EndMonitor { end } => {
                        monitors.pop();
                        Some(*end)
                    }
                    Ins::Nop => None,
                })
            })();
            match step {
                Ok(Some(t)) => pc = t,
                Ok(None) => pc += 1,
                Err(err) => {
                    if let Some(r) = err.downcast_ref::<ReturnSignal>() {
                        return Ok(Flow::Return(r.0.clone()));
                    }
                    if err.downcast_ref::<LeaveSrSignal>().is_some() {
                        return Ok(Flow::LeaveSr);
                    }
                    // An error inside a monitored body goes to the first
                    // ON-ERROR that lists its status (or *ALL / nothing).
                    let mut handled = None;
                    while let Some((handlers, end_body, _)) = monitors.last().cloned() {
                        if pc < end_body {
                            let status = error_status(&err);
                            handled = handlers
                                .iter()
                                .find(|(codes, _)| {
                                    codes.is_empty()
                                        || codes.iter().any(|c| {
                                            c == "*ALL"
                                                || c == &status
                                                || (c == "*PROGRAM" && status != "*FILE")
                                                || (c == "*FILE" && status == "*FILE")
                                        })
                                })
                                .map(|h| h.1);
                            break;
                        }
                        monitors.pop();
                    }
                    match handled {
                        Some(h) => {
                            self.last.error = true;
                            pc = h;
                        }
                        None => return Err(err.context(format!("line {}", i.line))),
                    }
                }
            }
        }
        Ok(Flow::Normal)
    }

    /// Run the program: open the files, take the parameters, then the
    /// cycle: detail calculations until LR (or RETURN), then total
    /// calculations, then close.
    pub fn run(&mut self, args: &[String]) -> Result<()> {
        for i in 0..self.prog.files.len() {
            if keyword(&self.prog.files[i].decl.keywords, "USROPN").is_none() {
                self.open_file(i)?;
            }
        }
        // *ENTRY PLIST: parameters arrive as text.
        let entry = self.prog.entry.clone();
        for (p, a) in entry.iter().zip(args) {
            let t = self.type_of(&p.target);
            let v = match t {
                Some(t) if t.is_numeric() => Value::Num(
                    Dec::parse(a).map_err(|_| anyhow!("parameter '{a}' is not a number"))?,
                ),
                _ => Value::str(a),
            };
            self.assign(&p.target, &v, Fit::Strict { half_adjust: false }, false)?;
        }
        let prog = self.prog;
        // *INZSR runs first.
        if let Some(inz) = prog.routines.subroutines.get("*INZSR") {
            self.run_code(inz)?;
        }
        let mut passes = 0u64;
        loop {
            passes += 1;
            let flow = self.run_code(&prog.routines.detail)?;
            if matches!(flow, Flow::Return(_)) {
                break;
            }
            if self.ind("LR") {
                self.run_code(&prog.routines.total)?;
                break;
            }
            if self.ind("RT") {
                break;
            }
            // No primary file: the cycle starts over until LR is set. A
            // program that never sets LR runs until it is cancelled, as
            // on IBM i; stop runaway loops in tests at a large bound.
            if passes > 10_000_000 {
                bail!("the program does not set on LR and keeps running");
            }
        }
        for f in self.files.iter_mut() {
            if let Some(mut o) = f.open.take() {
                let _ = self.io.rt.block_on(o.release(self.io.client));
            }
        }
        Ok(())
    }
}

fn expr_name(e: &Option<Expr>) -> Option<String> {
    match e {
        Some(Expr::Name { parts, args: None }) => Some(parts.join(".")),
        _ => None,
    }
}

/// An active MONITOR: (handlers, end of body, end).
type MonitorGroup = (Vec<(Vec<String>, usize)>, usize, usize);

#[derive(Debug)]
struct ReturnSignal(Option<Value>);

impl std::fmt::Display for ReturnSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RETURN")
    }
}

impl std::error::Error for ReturnSignal {}

#[derive(Debug)]
struct LeaveSrSignal;

impl std::fmt::Display for LeaveSrSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LEAVESR")
    }
}

impl std::error::Error for LeaveSrSignal {}

/// The status code an error stands for (ON-ERROR): 01211 file not open,
/// 00100 string, 00102 divide by zero, 00103 too small, 00121 array
/// index, 00907 decimal data; *FILE for file errors.
fn error_status(err: &anyhow::Error) -> String {
    let t = err.to_string();
    let code = |s: &str| s.to_string();
    if t.starts_with("MCH1211") {
        code("00102")
    } else if t.starts_with("RNQ0103") {
        code("00103")
    } else if t.starts_with("RNQ0100") {
        code("00100")
    } else if t.starts_with("RNQ0121") {
        code("00121")
    } else if t.starts_with("RNQ0907") {
        code("00907")
    } else if t.starts_with("CPF") {
        code("*FILE")
    } else {
        code("*PROGRAM")
    }
}

/// Edit codes 1-4, A-D, J-Q, X, Y, Z (ILE RPG Reference, "Edit Codes"):
/// commas, a decimal point, zero suppression, sign display.
pub fn edit_code(v: &Dec, digits: u32, decs: u32, code: char) -> String {
    let code = code.to_ascii_uppercase();
    let d = v.rescale(decs as u8, false).unwrap_or(*v);
    let raw = d.mantissa().unsigned_abs().to_string();
    let raw = format!("{:0>w$}", raw, w = digits as usize);
    let (int, frac) = raw.split_at(raw.len() - decs as usize);
    let commas = matches!(code, '1' | '2' | 'A' | 'B' | 'J' | 'K' | 'N' | 'O');
    let zero_blank = matches!(code, '2' | '4' | 'B' | 'D' | 'K' | 'M' | 'O' | 'Q' | 'Z');
    if code == 'X' {
        return raw;
    }
    if code == 'Y' {
        // Date edit: nn/nn/nn.
        let s = format!("{:0>6}", raw);
        return format!(
            "{}/{}/{}",
            &s[s.len() - 6..s.len() - 4],
            &s[s.len() - 4..s.len() - 2],
            &s[s.len() - 2..]
        );
    }
    let int_trim = int.trim_start_matches('0');
    let mut int_out = String::new();
    for (k, c) in int_trim.chars().enumerate() {
        if commas && k > 0 && (int_trim.len() - k) % 3 == 0 {
            int_out.push(',');
        }
        int_out.push(c);
    }
    let body = if decs > 0 && code != 'Z' {
        format!("{int_out}.{frac}")
    } else {
        int_out.clone()
    };
    let neg = d.is_negative();
    let sign = match code {
        'A' | 'B' | 'C' | 'D' => {
            if neg {
                "CR"
            } else {
                "  "
            }
        }
        'J' | 'K' | 'L' | 'M' => {
            if neg {
                "-"
            } else {
                " "
            }
        }
        'N' | 'O' | 'P' | 'Q' => "",
        _ => "",
    };
    let lead = if matches!(code, 'N' | 'O' | 'P' | 'Q') && neg {
        "-"
    } else {
        ""
    };
    let width = digits as usize
        + if decs > 0 && code != 'Z' { 1 } else { 0 }
        + if commas {
            (digits.saturating_sub(decs).saturating_sub(1) / 3) as usize
        } else {
            0
        };
    if d.is_zero() && zero_blank {
        return " ".repeat(width + sign.len() + lead.len());
    }
    let out = format!("{lead}{body}");
    format!("{:>w$}{sign}", out, w = width + lead.len())
}

fn today() -> (i32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

fn now_hms() -> (u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    (
        (secs / 3600) as u32,
        (secs % 3600 / 60) as u32,
        (secs % 60) as u32,
    )
}

/// UDATE (MMDDYY) and *DATE (MMDDYYYY), job date = today (UTC).
fn today_num(name: &str) -> i64 {
    let (y, m, d) = today();
    if name == "*DATE" {
        (m as i64) * 1_000_000 + (d as i64) * 10_000 + y as i64
    } else {
        (m as i64) * 10_000 + (d as i64) * 100 + (y % 100) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_of_numbers() {
        // %CHAR: leading zeros are not shown; the decimal places are
        // (ILE RPG Reference, %CHAR).
        assert_eq!(char_of_num(&Dec::parse("73.77").unwrap()), "73.77");
        assert_eq!(char_of_num(&Dec::parse("-1234.56").unwrap()), "-1234.56");
        assert_eq!(char_of_num(&Dec::parse("0.50").unwrap()), ".50");
        assert_eq!(char_of_num(&Dec::parse("5").unwrap()), "5");
        assert_eq!(char_of_num(&Dec::parse("0").unwrap()), "0");
    }

    #[test]
    fn edit_codes() {
        // ILE RPG Reference, edit codes: 1 = commas, decimal point, zero
        // shown; Z = no commas, no decimal point, zero suppressed; J =
        // commas and a trailing minus.
        assert_eq!(
            edit_code(&Dec::parse("1234.50").unwrap(), 7, 2, '1'),
            " 1,234.50"
        );
        assert_eq!(edit_code(&Dec::parse("12").unwrap(), 3, 0, 'Z'), " 12");
        assert_eq!(
            edit_code(&Dec::parse("-1234.50").unwrap(), 7, 2, 'J'),
            " 1,234.50-"
        );
        assert_eq!(edit_code(&Dec::parse("0").unwrap(), 3, 0, 'Z'), "   ");
    }

    #[test]
    fn indicator_names() {
        assert_eq!(ind_name("3"), "03");
        assert_eq!(ind_name("lr"), "LR");
    }
}
