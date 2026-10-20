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
use super::program::{Display, ProcInfo, Program, Sym, VarInfo};
use crate::rla::{OpenFile, ReadResult, Record};
use crate::workstn::Workstation;
use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::HashMap;
use tokio::runtime::Handle;
use tokio_postgres::Client;

#[path = "bifs.rs"]
mod bifs;
#[path = "cycle.rs"]
mod cycle;
#[path = "exec_sql.rs"]
mod exec_sql;
#[path = "ops.rs"]
mod ops;

/// What the program runs with: the database, the job.
pub struct Io<'a> {
    pub rt: Handle,
    pub client: &'a Client,
    pub user: String,
    pub job_number: Option<i64>,
    /// The library list (data areas).
    pub libl: Vec<String>,
    /// The job (spooled files). Without one, printed lines are returned
    /// in [`Machine::printed`].
    pub job: Option<crate::jobs::JobId>,
    /// The job's display (interactive jobs): EXFMT goes there.
    pub display: Option<crate::runtime::DisplayHandle>,
    /// For CL commands run from RPG (QCMDEXC, CALL of a CL program).
    pub cl: Option<super::ClContext>,
    /// RPG programs active in the call stack above this one.
    pub callers: Vec<String>,
    /// The database, for the embedded SQL session.
    pub database_url: String,
    /// The job, as service program activations know it.
    pub job_key: String,
    /// How the running object's modules are bound: where its imported
    /// procedures are.
    pub binding: Option<std::sync::Arc<crate::ile::Binding>>,
    /// A service program's module being run: its activation (LIB/SRVPGM/MODULE).
    pub unit: Option<String>,
    /// The storage of the other modules of the running program, shared by
    /// the calls into them while it runs.
    pub modules: ModuleStorage,
}

/// Global storage of bound modules, by module.
pub type ModuleStorage = std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<Vec<u8>>>>>;

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
    /// A program-described file: the image of the record last read.
    image: Option<Vec<u8>>,
    /// A display file is open.
    ws_open: bool,
    status: Status,
}

/// How a program handles null values in externally described database
/// files (ILE RPG Reference, "Database Null Value Support"; the ALWNULL
/// control keyword and CRTBNDRPG parameter, default *NO).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlwNull {
    /// A record with a null value cannot be read: data mapping error.
    No,
    /// Null values are read as the field's default; records are written
    /// without nulls (*INPUTONLY, and *YES).
    InputOnly,
    /// The program sees and sets null indicators with %NULLIND.
    UsrCtl,
}

impl AlwNull {
    fn of(prog: &Program) -> AlwNull {
        let v = prog
            .control
            .iter()
            .find(|k| k.name.eq_ignore_ascii_case("ALWNULL"))
            .and_then(|k| k.args.first())
            .map(|a| a.trim().to_ascii_uppercase());
        match v.as_deref() {
            Some("*USRCTL") => AlwNull::UsrCtl,
            Some("*INPUTONLY") | Some("*YES") => AlwNull::InputOnly,
            _ => AlwNull::No,
        }
    }
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
    /// Printer files in use, by file name.
    printers: HashMap<String, Printer>,
    /// Printed lines when the call has no job (`as4tainr call`), as a
    /// printer would put them on paper: form feeds between pages.
    pub printed: Vec<String>,
    /// Procedure calls in progress, innermost last.
    frames: Vec<Frame<'a>>,
    /// Parameters passed to the program (%PARMS outside a procedure).
    parms_passed: usize,
    /// Embedded SQL: the session (opened by the first statement), open
    /// cursors and prepared statements.
    sql: exec_sql::SqlState,
    /// ALWNULL: how the program handles null values in database records.
    alwnull: AlwNull,
    /// ALWNULL(*USRCTL): the program fields whose null indicator is on.
    nullind: std::collections::HashSet<String>,
    /// Multiple-occurrence data structures by buffer: (occurrence size,
    /// occurrences, current occurrence).
    occur: HashMap<usize, (usize, u32, u32)>,
    /// Tables (TABxxx arrays): the current element, set by LOOKUP.
    table_cur: HashMap<String, i128>,
    /// The cycle: records read ahead and not processed, by file; files
    /// at their end; control field values of the last record, by level.
    ahead: Vec<Option<cycle::Ahead>>,
    cycle_eof: Vec<bool>,
    control: HashMap<u8, String>,
    /// Varying-dimension arrays (DIM(*AUTO) and DIM(*VAR)): elements in
    /// use, by array name.
    var_elems: HashMap<String, u32>,
    /// FOR-EACH loops in progress: (code, EachInit, call depth) → the
    /// values and the next one.
    each: HashMap<(usize, usize, usize), (Vec<Value>, usize)>,
    /// The result data structure and %FIELDS of the file operation
    /// running.
    io_ds: Option<String>,
    io_fields: Option<Vec<String>>,
}

/// One procedure call: its local storage starts at `base` in the
/// machine's buffers; parameters passed by reference are aliases of the
/// caller's storage.
struct Frame<'a> {
    proc: &'a ProcInfo,
    base: usize,
    /// Local buffer → (absolute buffer, offset) for reference parameters.
    alias: HashMap<usize, (usize, usize)>,
    passed: usize,
    /// Parameters passed as *OMIT.
    omitted: Vec<String>,
}

/// Where an argument passed by reference lives.
#[derive(Clone, Copy, Debug)]
struct Loc {
    buf: usize,
    off: usize,
    size: usize,
    /// The variable's type (a data structure or whole array: its bytes).
    dtype: DType,
}

/// A program-described printer file while the program runs: where the
/// next line goes, and lines not yet written to the spooled file.
#[derive(Debug)]
struct Printer {
    /// The spooled file (with a job).
    spl: Option<i64>,
    page: i32,
    /// The line the next record prints on, before its spacing.
    line: i32,
    printed_on_page: bool,
    buffer: Vec<(i32, i32, String)>,
    /// Without a job: (page, line) last written to `printed`.
    out_pos: (i32, i32),
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
                    image: None,
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
            printers: HashMap::new(),
            printed: Vec::new(),
            frames: Vec::new(),
            parms_passed: 0,
            sql: Default::default(),
            alwnull: AlwNull::of(prog),
            nullind: Default::default(),
            occur: prog
                .occurs
                .iter()
                .map(|o| (o.buf, (o.size, o.count, 1)))
                .collect(),
            table_cur: HashMap::new(),
            ahead: prog.files.iter().map(|_| None).collect(),
            cycle_eof: vec![false; prog.files.len()],
            control: HashMap::new(),
            var_elems: HashMap::new(),
            each: HashMap::new(),
            io_ds: None,
            io_fields: None,
        }
    }

    /// The field of %NULLIND(field): a null-capable field of an
    /// externally described file, in a program compiled with
    /// ALWNULL(*USRCTL) (ILE RPG Reference, %NULLIND).
    fn nullind_field(&self, args: &[Expr]) -> Result<String> {
        let Some(Expr::Name { parts, args: None }) = args.first() else {
            bail!("%NULLIND needs the name of a field");
        };
        let name = parts.join(".");
        if self.alwnull != AlwNull::UsrCtl {
            bail!("%NULLIND({name}) needs ALWNULL(*USRCTL).");
        }
        let capable = self.prog.files.iter().any(|f| {
            f.fref.as_ref().is_some_and(|r| {
                r.file
                    .fields
                    .iter()
                    .zip(&f.fields)
                    .any(|(fd, p)| fd.null_capable && *p == name)
            })
        });
        if !capable {
            bail!("%NULLIND({name}): {name} is not a null-capable field.");
        }
        Ok(name)
    }

    fn block<F: std::future::Future>(&self, f: F) -> F::Output {
        self.io.rt.block_on(f)
    }

    /// A name as the running code sees it: the current procedure's
    /// locals first, then the program's globals. Local storage comes back
    /// with its absolute buffer.
    fn sym(&self, name: &str) -> Option<Sym> {
        if let Some(f) = self.frames.last() {
            if let Some(s) = f.proc.syms.get(name) {
                let place = |buf: usize, off: usize| -> (usize, usize) {
                    match f.alias.get(&buf) {
                        Some((b, o)) => (*b, off + o),
                        None => (f.base + buf, off),
                    }
                };
                return Some(match s.clone() {
                    Sym::Var(mut v) if v.local => {
                        (v.buf, v.off) = place(v.buf, v.off);
                        v.local = false;
                        Sym::Var(v)
                    }
                    Sym::Ds(mut d) if d.local => {
                        (d.buf, d.off) = place(d.buf, d.off);
                        d.local = false;
                        Sym::Ds(d)
                    }
                    other => other,
                });
            }
        }
        // A subfield of a multiple-occurrence data structure: in the
        // current occurrence.
        match self.prog.syms.get(name).cloned() {
            Some(Sym::Var(mut v)) => {
                if let Some((size, _, cur)) = self.occur.get(&v.buf) {
                    v.off += (*cur as usize - 1) * size;
                }
                Some(Sym::Var(v))
            }
            Some(Sym::Ds(mut d)) => {
                if let Some((size, _, cur)) = self.occur.get(&d.buf) {
                    d.off += (*cur as usize - 1) * size;
                }
                Some(Sym::Ds(d))
            }
            other => other,
        }
    }

    /// A table name (TABxxx) without an index means its current element.
    fn table_index(&self, name: &str, v: &VarInfo, i: Option<i128>) -> Option<i128> {
        if i.is_none() && v.dim > 0 && name.starts_with("TAB") {
            Some(self.table_cur.get(name).copied().unwrap_or(1))
        } else {
            i
        }
    }

    /// The storage a qualified name with indexes names: each index moves
    /// to that element of a data structure array (`list.users(3).name`).
    fn path_sym(&mut self, parts: &[String], idx: &[Option<Box<Expr>>]) -> Result<Sym> {
        let mut extra = 0usize;
        let mut name = String::new();
        for (k, part) in parts.iter().enumerate() {
            if k > 0 {
                name.push('.');
            }
            name.push_str(part);
            if let Some(Some(ix)) = idx.get(k) {
                let i = self.eval(ix)?.num()?.to_i128()?;
                match self.sym(&name) {
                    Some(Sym::Ds(d)) => {
                        if d.dim == 0 {
                            bail!("{name} is not a data structure array");
                        }
                        if i < 1 || i > d.dim as i128 {
                            bail!(
                                "RNQ0121: An array index is out of range (index {i}, {} elements).",
                                d.dim
                            );
                        }
                        extra += (i as usize - 1) * d.size;
                    }
                    _ => bail!("{name} is not a data structure array"),
                }
            }
        }
        Ok(match self.sym(&name) {
            Some(Sym::Var(mut v)) => {
                v.off += extra;
                Sym::Var(v)
            }
            Some(Sym::Ds(mut d)) => {
                d.off += extra;
                Sym::Ds(d)
            }
            _ => bail!("{name} is not defined"),
        })
    }

    /// A data structure, or one element of a data structure array.
    fn ds_element(
        &mut self,
        d: &super::program::DsInfo,
        args: &Option<Vec<Expr>>,
    ) -> Result<usize> {
        match self.index_of(args)? {
            None => Ok(d.off),
            Some(i) => {
                if d.dim == 0 || i < 1 || i > d.dim as i128 {
                    bail!(
                        "RNQ0121: An array index is out of range (index {i}, {} elements).",
                        d.dim
                    );
                }
                Ok(d.off + (i as usize - 1) * d.size)
            }
        }
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
            Expr::Path { parts, idx, args } => match self.path_sym(parts, idx)? {
                Sym::Var(v) => {
                    let i = self.index_of(args)?;
                    if i.is_none() && v.dim > 0 {
                        bail!("{} is an array: an index is needed here", parts.join("."));
                    }
                    self.load_var(&v, i)?
                }
                Sym::Ds(d) => {
                    let off = self.ds_element(&d, args)?;
                    Value::Char(self.bufs[d.buf][off..off + d.size].to_vec())
                }
                _ => bail!("{} is not a field", parts.join(".")),
            },
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                match self.sym(&name) {
                    Some(Sym::Var(v)) => {
                        let i = self.index_of(args)?;
                        let i = self.table_index(&name, &v, i);
                        if i.is_none() && v.dim > 0 {
                            bail!("{name} is an array: an index is needed here");
                        }
                        if let Some(k) = i {
                            if self.prog.varying.contains_key(&name) {
                                let n = self.elems_in_use(&name, &v);
                                if k < 1 || k > n as i128 {
                                    bail!("RNQ0121: An array index is out of range (index {k}, {n} elements in use).");
                                }
                            }
                        }
                        self.load_var(&v, i)?
                    }
                    Some(Sym::Const(c)) => c,
                    Some(Sym::Ds(d)) => {
                        let off = self.ds_element(&d, args)?;
                        Value::Char(self.bufs[d.buf][off..off + d.size].to_vec())
                    }
                    Some(Sym::Klist(_)) => bail!("{name} is a key list, not a value"),
                    None => match name.as_str() {
                        "*DATE" | "UDATE" => Value::Num(Dec::from_i64(today_num(&name))),
                        "*DAY" | "UDAY" => Value::Num(Dec::from_i64(today().2 as i64)),
                        "*MONTH" | "UMONTH" => Value::Num(Dec::from_i64(today().1 as i64)),
                        "*YEAR" => Value::Num(Dec::from_i64(today().0 as i64)),
                        "UYEAR" => Value::Num(Dec::from_i64((today().0 % 100) as i64)),
                        _ if args.is_some() && self.callable(&name) => {
                            let args = args.clone().unwrap_or_default();
                            self.call_named(&name, &args)?.ok_or_else(|| {
                                anyhow!("procedure {name} returns no value; it cannot be used in an expression")
                            })?
                        }
                        _ if args.is_some() => bail!("{name} is not an array or a procedure"),
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
                if *op == BinOp::In {
                    return self.eval_in(l, r);
                }
                let x = self.eval(l)?;
                let y = self.eval(r)?;
                // Date, time and timestamp arithmetic with durations.
                if matches!(op, BinOp::Add | BinOp::Sub)
                    && (matches!(y, Value::Dur(..)) || matches!(x, Value::Dur(..)))
                {
                    return self.date_arith(*op, x, y);
                }
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
            Expr::Name { parts, .. } | Expr::Path { parts, .. } => {
                match self.sym(&parts.join(".")) {
                    Some(Sym::Var(v)) => Some(v.dtype),
                    Some(Sym::Ds(d)) => Some(DType::Char(d.size as u32)),
                    _ => None,
                }
            }
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
        // Forms of the built-ins handled in bifs.rs (dates for %CHAR and
        // %DEC, varying arrays for %ELEM...).
        if let Some(v) = self.bif_override(b, args)? {
            return Ok(v);
        }
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
            "NULLIND" => {
                let name = self.nullind_field(args)?;
                Value::Ind(self.nullind.contains(&name))
            }
            // %EDITFLT, and %CHAR of a float: the external display form
            // (ILE RPG Reference, float format).
            "EDITFLT" => {
                let n = match args.first().and_then(|a| self.type_of(a)) {
                    Some(DType::Float(4)) => 4,
                    _ => 8,
                };
                let x = arg(self, 0)?.num()?;
                Value::str(&data::float_text(x.to_string().parse().unwrap_or(0.0), n))
            }
            "CHAR"
                if matches!(
                    args.first().and_then(|a| self.type_of(a)),
                    Some(DType::Float(_))
                ) =>
            {
                let Some(DType::Float(n)) = args.first().and_then(|a| self.type_of(a)) else {
                    unreachable!()
                };
                let x = arg(self, 0)?.num()?;
                Value::str(&data::float_text(x.to_string().parse().unwrap_or(0.0), n))
            }
            // %UCS2 and %GRAPH: UCS-2 and graphic values are converted to
            // and from character data when they are used (see data.rs).
            "UCS2" | "GRAPH" => Value::Char(arg(self, 0)?.chars()?),
            // %FLOAT: the value as a float (8 bytes).
            "FLOAT" => {
                let x: f64 = arg(self, 0)?.num()?.to_string().parse().unwrap_or(0.0);
                Value::Num(data::float_dec(x)?)
            }
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
                    Expr::Name { parts, .. } => match self.sym(&parts.join(".")) {
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
                Some(Expr::Name { parts, .. }) => match self.sym(&parts.join(".")) {
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
                // %XLATE(from : to : string : start): from the start
                // position on (ILE RPG Reference, %XLATE).
                let start = match args.get(3) {
                    Some(x) => self.eval(x)?.num()?.to_i128()?,
                    None => 1,
                };
                if start < 1 || start as usize > s.len().max(1) {
                    bail!("RNQ0100: %XLATE start position {start} is outside the string.");
                }
                Value::Char(
                    s.iter()
                        .enumerate()
                        .map(|(k, c)| match from.iter().position(|f| f == c) {
                            Some(p) if p < to.len() && k + 1 >= start as usize => to[p],
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
            "PARMS" => Value::Num(Dec::from_i64(match self.frames.last() {
                Some(f) => f.passed,
                None => self.parms_passed,
            } as i64)),
            other => match self.bif_more(other, args)? {
                Some(v) => v,
                None => bail!("%{other} is not supported yet"),
            },
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
            Expr::Bif(b, args) if b == "ELEM" => {
                // %ELEM(array) = n: the elements in use of a varying-
                // dimension array; new ones get their initial value.
                let arr = args
                    .first()
                    .and_then(|e| self.array_ref(e))
                    .filter(|a| self.prog.varying.contains_key(&a.name))
                    .ok_or_else(|| {
                        anyhow!("%ELEM can only be set for a varying-dimension array")
                    })?;
                let n = val.num()?.to_i128()?;
                if n < 0 || n > arr.var.dim as i128 {
                    bail!("RNQ0121: %ELEM {n} is outside 0 to {}.", arr.var.dim);
                }
                let cur = self.elems_in_use(&arr.name, &arr.var) as usize;
                for k in cur..n as usize {
                    let off = arr.var.off + k * arr.var.stride;
                    self.bufs[arr.var.buf][off..off + arr.var.dtype.size()]
                        .copy_from_slice(&arr.var.dtype.initial());
                }
                self.var_elems.insert(arr.name, n as u32);
            }
            Expr::Bif(b, args) if b == "OCCUR" => {
                let n = val.num()?.to_i128()?;
                let ds = args
                    .first()
                    .ok_or_else(|| anyhow!("%OCCUR needs a data structure"))?;
                self.set_occurrence(ds, n)?;
            }
            Expr::Bif(b, args) if b == "NULLIND" => {
                let name = self.nullind_field(args)?;
                if val.truth()? {
                    self.nullind.insert(name);
                } else {
                    self.nullind.remove(&name);
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
            Expr::Path { parts, idx, args } => match self.path_sym(parts, idx)? {
                Sym::Var(v) => {
                    let i = self.index_of(args)?;
                    self.store_var(&v, i, val, fit, right)?;
                }
                Sym::Ds(d) => {
                    let off = self.ds_element(&d, args)?;
                    let t = DType::Char(d.size as u32);
                    let out = &mut self.bufs[d.buf][off..off + d.size];
                    data::store(&t, val, out, fit, right)?;
                }
                _ => bail!("{} cannot be changed", parts.join(".")),
            },
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                match self.sym(&name) {
                    Some(Sym::Var(v)) => {
                        // A list (%LIST, %SPLIT, another array...) into an
                        // array: element by element; the elements after
                        // it are cleared, or for a varying-dimension
                        // array no longer in use.
                        if let (Value::List(items), None, true) = (val, args, v.dim > 0) {
                            if items.len() > v.dim as usize {
                                bail!(
                                    "RNQ0121: {} values do not fit in array {name} ({} elements).",
                                    items.len(),
                                    v.dim
                                );
                            }
                            for k in 0..v.dim as usize {
                                let off = v.off + k * v.stride;
                                match items.get(k) {
                                    Some(x) => {
                                        let out = &mut self.bufs[v.buf][off..off + v.dtype.size()];
                                        data::store(&v.dtype, x, out, fit, right)?;
                                    }
                                    None => self.bufs[v.buf][off..off + v.dtype.size()]
                                        .copy_from_slice(&v.dtype.initial()),
                                }
                            }
                            if self.prog.varying.contains_key(&name) {
                                self.var_elems.insert(name.clone(), items.len() as u32);
                            }
                            return Ok(());
                        }
                        let next = matches!(args.as_deref(), Some([Expr::Name { parts, args: None }]) if parts.len() == 1 && parts[0] == "*NEXT");
                        let i = if next {
                            Some(self.elems_in_use(&name, &v) as i128 + 1)
                        } else {
                            self.index_of(args)?
                        };
                        let i = self.table_index(&name, &v, i);
                        // A varying-dimension array: *AUTO grows to the
                        // index assigned (and *NEXT appends); *VAR only
                        // takes elements in use (ILE RPG Reference,
                        // "Varying-dimension arrays").
                        if let (Some(kind), Some(k)) = (self.prog.varying.get(&name).copied(), i) {
                            let n = self.elems_in_use(&name, &v);
                            if k > n as i128 {
                                if (kind == 'A' || next) && k <= v.dim as i128 {
                                    self.var_elems.insert(name.clone(), k as u32);
                                } else {
                                    bail!("RNQ0121: An array index is out of range (index {k}, {n} elements in use).");
                                }
                            }
                        }
                        self.store_var(&v, i, val, fit, right)?;
                    }
                    Some(Sym::Ds(d)) => {
                        let off = self.ds_element(&d, args)?;
                        let t = DType::Char(d.size as u32);
                        let out = &mut self.bufs[d.buf][off..off + d.size];
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
            if pname.is_empty() {
                continue;
            }
            let v = self.eval(&Expr::name(pname))?;
            let text = match v {
                Value::Num(d) if fd.data_type == crate::dds::DataType::Float => d.to_string(),
                Value::Num(d) => d.rescale(fd.decimals as u8, false)?.to_string(),
                // Hexadecimal fields: the bytes as hex digits, untranslated.
                other if fd.data_type == crate::dds::DataType::Hex => {
                    other.chars()?.iter().map(|b| format!("{b:02X}")).collect()
                }
                other => a(&other.chars()?),
            };
            rec.insert(fd.name.clone(), text);
        }
        if self.alwnull == AlwNull::UsrCtl {
            let nulls: Vec<String> = fref
                .file
                .fields
                .iter()
                .zip(&fi.fields)
                .filter(|(fd, p)| fd.null_capable && self.nullind.contains(*p))
                .map(|(fd, _)| fd.name.clone())
                .collect();
            if !nulls.is_empty() {
                rec.insert(crate::rla::NULLS.to_string(), nulls.join(","));
            }
        }
        Ok(rec)
    }

    /// Record → program fields.
    fn fill_fields(&mut self, i: usize, rec: &Record) -> Result<()> {
        let fi = self.prog.files[i].clone();
        let fref = fi.fref.as_ref().unwrap();
        // Null values in fields the program has (large objects are left
        // out of its record).
        let nulls: Vec<String> = crate::rla::null_fields(rec)
            .into_iter()
            .filter(|n| {
                fref.file
                    .fields
                    .iter()
                    .any(|f| &f.name == n && !f.data_type.is_lob())
            })
            .collect();
        if !nulls.is_empty() && self.alwnull == AlwNull::No {
            // ALWNULL(*NO): none of the record is available to the program.
            bail!(
                "CPF5035: Data mapping error on file {}: field {} has the null value, and the program does not allow null values (ALWNULL(*NO)).",
                fref.file.name,
                nulls[0]
            );
        }
        if self.alwnull == AlwNull::UsrCtl {
            for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
                if fd.null_capable {
                    if nulls.contains(&fd.name) {
                        self.nullind.insert(pname.clone());
                    } else {
                        self.nullind.remove(pname);
                    }
                }
            }
        }
        for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
            if pname.is_empty() {
                continue;
            }
            let Some(text) = rec.get(&fd.name) else {
                continue;
            };
            let v = if fd.data_type.is_numeric() {
                Value::Num(Dec::parse(text)?)
            } else if fd.data_type == crate::dds::DataType::Hex {
                Value::Char(
                    (0..text.len() / 2)
                        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap_or(0))
                        .collect(),
                )
            } else {
                Value::str(text)
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
        // %KDS(ds : n): the first n subfields of a data structure.
        if let [Expr::Bif(b, args)] = key {
            if b == "KDS" {
                let ds = match args.first() {
                    Some(Expr::Name { parts, .. }) => parts.join("."),
                    _ => bail!("%KDS needs a data structure"),
                };
                let mut members = self.ds_members(&ds)?;
                if let Some(n) = args.get(1) {
                    let n = self.eval(n)?.num()?.to_i128()? as usize;
                    members.truncate(n);
                }
                let exprs: Vec<Expr> = members.into_iter().map(|(_, k)| Expr::name(&k)).collect();
                return self.key_values(&exprs);
            }
        }
        // A KLIST name stands for its key fields.
        let exprs: Vec<Expr> = match key {
            [Expr::Name { parts, args: None }] => match self.sym(&parts.join(".")) {
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
                    let moved = match self.io_ds.clone() {
                        Some(ds) => self.record_to_ds(i, rec, &ds),
                        None => self.move_input(i, rec),
                    };
                    if let Err(e) = moved {
                        // A data mapping error is an error on the
                        // operation: the E extender or the error
                        // indicator takes it.
                        if ext.error || ind.lo.is_some() {
                            self.last = Status {
                                error: true,
                                ..Status::default()
                            };
                            self.files[i].status.error = true;
                            if let Some(lo) = &ind.lo {
                                self.set_ind(lo, true);
                            }
                            return Ok(());
                        }
                        return Err(e);
                    }
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
                    let mut rec = match self.io_ds.clone() {
                        Some(ds) => self.ds_to_record(i, &ds)?,
                        None => self.record_of(i)?,
                    };
                    if let Some(only) = self.io_fields.clone() {
                        let fi = &self.prog.files[i];
                        let fref = fi.fref.as_ref().unwrap();
                        let keep: Vec<String> = fref
                            .file
                            .fields
                            .iter()
                            .zip(&fi.fields)
                            .filter(|(fd, p)| only.contains(p) || only.contains(&fd.name))
                            .map(|(fd, _)| fd.name.clone())
                            .collect();
                        rec.retain(|k, _| keep.contains(k));
                    }
                    rec
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
        match d.indds.as_ref().and_then(|n| self.sym(n)) {
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
        match d.indds.as_ref().and_then(|x| self.sym(x)) {
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
                        .send(crate::runtime::DisplayRequest { frame, reply: tx })
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
            other => bail!("{} cannot be shown", other.describe()),
        })
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Sql(text) => self.exec_sql(text)?,
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
                    // A float shows in its external display form.
                    Some(m) if matches!(self.type_of(m), Some(DType::Float(_))) => {
                        let Some(DType::Float(n)) = self.type_of(m) else {
                            unreachable!()
                        };
                        let x = self.eval(m)?.num()?;
                        data::float_text(x.to_string().parse().unwrap_or(0.0), n)
                    }
                    Some(m) => {
                        let v = self.eval(m)?;
                        self.text_of(&v)?
                    }
                    None => String::new(),
                };
                match (response, self.io.display.clone()) {
                    // An interactive job: the message and a reply line on
                    // the screen; the reply goes into the response field.
                    (Some(r), Some(display)) => {
                        let reply = self.dsply_screen(&display, text.trim_end(), r)?;
                        self.assign(r, &reply, Fit::Truncate { half_adjust: false }, false)?;
                    }
                    // No operator to answer in a batch call: the response
                    // stays as it was.
                    _ => self.messages.push(text.trim_end().to_string()),
                }
            }
            Stmt::File {
                op,
                file,
                key,
                ds,
                fields,
                ext,
                ind,
            } => {
                // A result data structure: the record goes into it (or
                // comes from it) instead of the program fields; %FIELDS:
                // an update changes only those fields.
                self.io_ds = ds.clone();
                self.io_fields = fields.clone();
                let r = self.file_op(*op, file, key, ext, ind);
                self.io_ds = None;
                self.io_fields = None;
                r?;
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
                    Expr::Name { parts, args: None } => match self.sym(&parts.join(".")) {
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
                                if let Some(Sym::Var(v)) = self.sym(&format!("{prefix}{sf}")) {
                                    for k in 0..v.dim.max(1) as usize {
                                        let at = v.off + k * v.stride;
                                        self.bufs[v.buf][at..at + v.dtype.size()]
                                            .copy_from_slice(&v.dtype.initial());
                                    }
                                }
                            }
                        }
                        _ => bail!("CLEAR of {} is not supported", parts.join(".")),
                    },
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
                ext,
                ind,
            } => {
                if !(self.string_op(code, f1, f2, result, ext, ind)?
                    || self.array_op(code, f1, f2, result, ext, ind)?
                    || self.bit_op(code, f1, f2, result, ind)?)
                {
                    self.misc_op(code, f1, f2, result, ind)?
                }
            }
            Stmt::Nop => {}
            Stmt::CallP(e) => match e {
                Expr::Name { parts, args } => {
                    let name = parts.join(".");
                    self.call_named(&name, args.as_deref().unwrap_or(&[]))?;
                }
                other => bail!("CALLP: a procedure or program call is needed, found {other:?}"),
            },
            Stmt::Call {
                program,
                parms,
                ind,
            } => {
                let name = match program {
                    Expr::Name { parts, args: None } if self.sym(&parts.join(".")).is_none() => {
                        parts.join(".")
                    }
                    other => a(&self.eval(other)?.chars()?).trim().to_ascii_uppercase(),
                };
                // PARM: factor 2 is moved to the result field before the
                // call.
                let mut locs = Vec::new();
                for p in parms {
                    if let Some(v) = &p.value {
                        let val = self.eval(v)?;
                        self.assign(&p.target, &val, Fit::Strict { half_adjust: false }, false)?;
                    }
                    locs.push(self.loc_of(&p.target)?);
                }
                match self.call_program(&name, &locs) {
                    Ok(()) => {
                        if let Some(lo) = &ind.lo {
                            self.set_ind(lo, false);
                        }
                    }
                    // Positions 73-74: on if the called program ends in
                    // error, instead of the error going on.
                    Err(e) if ind.lo.is_some() => {
                        let _ = e;
                        self.set_ind(ind.lo.as_deref().unwrap(), true);
                    }
                    Err(e) => return Err(e),
                }
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
            "DEFINE" | "COMMIT" | "ROLBK" | "FORCE" | "DUMP" | "DEALLOC" => {}
            "SND-MSG" => {
                // *INFO (and the other informational types) to the job
                // log; *ESCAPE ends the procedure with the message, which
                // a MONITOR group (ON-EXCP) can handle.
                let kind = expr_name(f1).unwrap_or_else(|| "*INFO".into());
                let v = self.eval(&need(f2, "a message")?)?;
                let (id, text) = match v {
                    Value::List(parts) => {
                        let id = match parts.first() {
                            Some(x) => a(&x.chars()?).trim().to_string(),
                            None => String::new(),
                        };
                        let data = match parts.get(2) {
                            Some(d) => a(&d.chars()?).trim_end().to_string(),
                            None => String::new(),
                        };
                        (Some(id), data)
                    }
                    other => (None, a(&other.chars()?).trim_end().to_string()),
                };
                let full = match &id {
                    Some(i) => format!("{i}: {text}"),
                    None => text.clone(),
                };
                if kind == "*ESCAPE" {
                    if id.is_some() {
                        bail!("{full}");
                    }
                    bail!("CPF9898: {text}");
                }
                self.job_log(&full);
            }
            "EXCEPT" => {
                let name = expr_name(f2);
                self.except(name.as_deref())?;
            }
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

    /// Conditioning indicators: all of a group must be met; groups joined
    /// by OR lines are alternatives.
    fn conditions_met(&self, cond: &Conditions) -> bool {
        cond.split(|(_, i)| i == crate::rpg::parse::OR_MARK)
            .any(|g| g.iter().all(|(neg, i)| self.ind(i) != *neg))
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
                    Ins::EachInit { list } => {
                        let vals = self.list_of(list)?;
                        let key = (code.as_ptr() as usize, pc, self.frames.len());
                        self.each.insert(key, (vals, 0));
                        None
                    }
                    Ins::EachNext { var, init, end } => {
                        let key = (code.as_ptr() as usize, *init, self.frames.len());
                        let next = self.each.get_mut(&key).and_then(|(v, at)| {
                            let x = v.get(*at).cloned();
                            *at += 1;
                            x
                        });
                        match next {
                            Some(v) => {
                                self.assign(var, &v, Fit::Strict { half_adjust: false }, false)?;
                                None
                            }
                            None => {
                                self.each.remove(&key);
                                Some(*end)
                            }
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
                        let prog = self.prog;
                        let sub = self
                            .frames
                            .last()
                            .and_then(|f| f.proc.routines.subroutines.get(name))
                            .or_else(|| prog.routines.subroutines.get(name))
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
                                            c.strip_prefix("MSG:")
                                                .is_some_and(|id| err.to_string().starts_with(id))
                                                || c == "*ALL"
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
    /// Run the program with parameters given as text (a CL CALL, the
    /// command line): each is converted to its entry parameter's type.
    pub fn run(&mut self, args: &[String]) -> Result<()> {
        let types: Vec<DType> = match &self.prog.main {
            Some(m) => self.prog.procs[m].params.iter().map(|p| p.dtype).collect(),
            None => self
                .prog
                .entry
                .iter()
                .map(|p| self.type_of(&p.target).unwrap_or(DType::Char(32)))
                .collect(),
        };
        let mut parms = Vec::new();
        for (t, a) in types.iter().zip(args) {
            let v = if t.is_numeric() {
                Value::Num(Dec::parse(a).map_err(|_| anyhow!("parameter '{a}' is not a number"))?)
            } else {
                Value::str(a)
            };
            let mut buf = t.initial();
            data::store(t, &v, &mut buf, Fit::Strict { half_adjust: false }, false)?;
            parms.push(buf);
        }
        self.run_bytes(&mut parms)
    }

    /// Run the program with parameters passed by reference (a CALL from
    /// another program): each is the bytes of the caller's variable, and
    /// gets the parameter's value back when the program ends.
    pub fn run_bytes(&mut self, parms: &mut [Vec<u8>]) -> Result<()> {
        for i in 0..self.prog.files.len() {
            if keyword(&self.prog.files[i].decl.keywords, "USROPN").is_none() {
                self.open_file(i)?;
            }
        }
        self.parms_passed = parms.len();
        let prog = self.prog;
        let r = match &prog.main {
            // ctl-opt main: the main procedure, with no cycle.
            Some(m) => {
                let base = self.bufs.len();
                let mut call_args = Vec::new();
                for (i, p) in parms.iter().enumerate() {
                    self.bufs.push(p.clone());
                    call_args.push(Arg::Ref(Loc {
                        buf: base + i,
                        off: 0,
                        size: p.len(),
                        dtype: DType::Char(p.len() as u32),
                    }));
                }
                let r = self.call_proc(&prog.procs[m], call_args).map(|_| ());
                for (i, p) in parms.iter_mut().enumerate() {
                    *p = self.bufs[base + i].clone();
                }
                self.bufs.truncate(base);
                r
            }
            None => {
                // *ENTRY PLIST (or a PI outside procedures): the bytes go
                // into the parameter fields, and come back after.
                let mut locs = Vec::new();
                for (p, bytes) in prog.entry.iter().zip(parms.iter()) {
                    let l = self.loc_of(&p.target)?;
                    let n = l.size.min(bytes.len());
                    self.bufs[l.buf][l.off..l.off + n].copy_from_slice(&bytes[..n]);
                    locs.push(l);
                }
                let r = self.cycle();
                for (l, bytes) in locs.iter().zip(parms.iter_mut()) {
                    let n = l.size.min(bytes.len());
                    bytes[..n].copy_from_slice(&self.bufs[l.buf][l.off..l.off + n]);
                }
                r
            }
        };
        let closed = self.close_printers();
        for f in self.files.iter_mut() {
            if let Some(mut o) = f.open.take() {
                let _ = self.io.rt.block_on(o.release(self.io.client));
            }
        }
        r?;
        closed
    }
}

impl Machine<'_> {
    /// Programs of the operating system that RPG programs call, done
    /// here (true when `name` is one of them).
    fn system_program(&mut self, name: &str, locs: &[Loc]) -> Result<bool> {
        let text = |m: &Self, l: &Loc| a(&m.bufs[l.buf][l.off..l.off + l.size]);
        let num = |m: &Self, l: &Loc| -> Result<Dec> {
            data::load(&l.dtype, &m.bufs[l.buf][l.off..l.off + l.size])?.num()
        };
        match name {
            // QCMDEXC (command, length): run a CL command in the job.
            "QCMDEXC" => {
                let [cmd, len, ..] = locs else {
                    bail!("QCMDEXC needs a command and its length");
                };
                let n = num(self, len)?.to_i128()?.max(0) as usize;
                let c: String = text(self, cmd).chars().take(n).collect();
                self.run_cl(c.trim())?;
            }
            // QDCXLATE (length, data, table): translate bytes in place
            // between EBCDIC (CCSID 37) and ASCII (QTCPASC, QTCPEBC).
            "QDCXLATE" => {
                let [len, data_loc, table, ..] = locs else {
                    bail!("QDCXLATE needs a length, the data and a table");
                };
                let n = (num(self, len)?.to_i128()?.max(0) as usize).min(data_loc.size);
                let t = text(self, table).trim().to_ascii_uppercase();
                let bytes = &mut self.bufs[data_loc.buf][data_loc.off..data_loc.off + n];
                match t.as_str() {
                    "QTCPASC" | "QASCII" => {
                        for b in bytes.iter_mut() {
                            let u = crate::tn5250::ebcdic::TO_UNICODE[*b as usize];
                            *b = if u < 256 { u as u8 } else { b'?' };
                        }
                    }
                    "QTCPEBC" | "QEBCDIC" => {
                        for b in bytes.iter_mut() {
                            *b = crate::tn5250::ebcdic::FROM_UNICODE[*b as usize];
                        }
                    }
                    other => bail!("QDCXLATE: table {other} is not supported (QTCPASC, QTCPEBC)"),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Run a CL command in the calling job (QCMDEXC, CALL of a CL
    /// program). Its messages go to the job log.
    fn run_cl(&mut self, cmd: &str) -> Result<()> {
        let ctx =
            self.io.cl.clone().ok_or_else(|| {
                anyhow!("CL command '{cmd}' cannot run here: the program has no job")
            })?;
        let mut job = crate::commands::Job {
            client: self.io.client,
            env: &ctx.env,
            user: self.io.user.clone(),
            curlib: ctx.curlib.clone(),
            libl: ctx.user_libl.clone(),
            display: self.io.display.clone(),
            id: self.io.job.clone(),
        };
        let out = self.io.rt.block_on(job.run(cmd))?;
        if let crate::commands::Outcome::Messages(msgs) = out {
            for m in msgs {
                self.job_log(&m);
            }
        }
        Ok(())
    }

    /// A message to the job log; without a job (`as4tainr call`), to the
    /// program's output.
    fn job_log(&mut self, text: &str) {
        match &self.io.job {
            Some(id) => {
                let client = self.io.client;
                let id = id.clone();
                self.io
                    .rt
                    .block_on(crate::jobs::log(client, &id, "INFO", text.trim_end()));
            }
            None => self.messages.push(text.trim_end().to_string()),
        }
    }

    /// printf formatting: %s, %d, %i, %u, %%; the text ends at a null.
    fn printf(&self, vals: &[Value]) -> Result<String> {
        let show = |v: &Value| -> Result<String> {
            Ok(match v {
                Value::Num(d) => d.to_string(),
                Value::Ind(b) => if *b { "1" } else { "0" }.into(),
                Value::Fig(_) => String::new(),
                Value::Char(c) => a(c),
                other => bail!("{} cannot be formatted", other.describe()),
            })
        };
        let fmt = match vals.first() {
            Some(v) => show(v)?,
            None => return Ok(String::new()),
        };
        let fmt = fmt.split('\0').next().unwrap_or("").to_string();
        let mut out = String::new();
        let mut args = vals[1..].iter();
        let mut cs = fmt.chars().peekable();
        while let Some(c) = cs.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            // Flags, width and length modifiers are skipped.
            let mut conv = None;
            for d in cs.by_ref() {
                if d.is_ascii_alphabetic() && !matches!(d, 'l' | 'h') || d == '%' {
                    conv = Some(d);
                    break;
                }
            }
            match conv {
                Some('%') => out.push('%'),
                Some(_) => {
                    if let Some(v) = args.next() {
                        out.push_str(show(v)?.split('\0').next().unwrap_or(""));
                    }
                }
                None => {}
            }
        }
        Ok(out)
    }
}

impl Machine<'_> {
    /// DSPLY with a response in an interactive job: a Display Program
    /// Messages screen with the message and a reply field as long as the
    /// response (at most 52 characters, ILE RPG Reference, DSPLY).
    fn dsply_screen(
        &mut self,
        display: &crate::runtime::DisplayHandle,
        text: &str,
        response: &Expr,
    ) -> Result<Value> {
        let t = self
            .type_of(response)
            .ok_or_else(|| anyhow!("DSPLY: the response must be a variable"))?;
        let numeric = t.is_numeric();
        let len = if numeric {
            t.precision().0 + 2
        } else {
            t.len()
        }
        .clamp(1, 52);
        // DDS, columns placed as the DDS form has them.
        let line = |parts: &[(usize, &str)]| -> String {
            let mut l = vec![' '; 80];
            l[5] = 'A';
            for (pos, s) in parts {
                for (k, c) in s.chars().enumerate() {
                    if pos - 1 + k < 80 {
                        l[pos - 1 + k] = c;
                    }
                }
            }
            l.into_iter().collect::<String>().trim_end().to_string()
        };
        let dds = [
            line(&[(45, "DSPSIZ(24 80 *DS3)")]),
            line(&[(17, "R"), (19, "DSPLYR")]),
            line(&[(39, "  1"), (42, " 24"), (45, "'Display Program Messages'")]),
            line(&[(45, "DSPATR(HI)")]),
            line(&[
                (19, "MSG"),
                (30, "   76"),
                (38, "O"),
                (39, "  3"),
                (42, "  4"),
            ]),
            line(&[(39, " 22"), (42, "  2"), (45, "'Type reply, press Enter.'")]),
            line(&[(39, " 23"), (42, "  4"), (45, "'Reply . . .'")]),
            line(&[
                (19, "REPLY"),
                (30, &format!("{len:>5}")),
                (38, "B"),
                (39, " 23"),
                (42, " 17"),
            ]),
            line(&[(45, "DSPATR(UL)")]),
            // The reply keeps lower case letters as typed.
            line(&[(45, "CHECK(LC)")]),
        ]
        .join("\n");
        let file = std::sync::Arc::new(crate::dspf::parse("QDSPLY", &dds)?);
        let mut values = std::collections::BTreeMap::new();
        values.insert("MSG".to_string(), text.to_string());
        values.insert("REPLY".to_string(), String::new());
        let mut ws = Workstation::new();
        let frame = ws.frame(&file, "DSPLYR", values, [false; 100])?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let display = display.clone();
        let resp = self.block(async move {
            display
                .send(crate::runtime::DisplayRequest { frame, reply: tx })
                .await
                .map_err(|_| anyhow!("the job's display is gone"))?;
            rx.await
                .map_err(|_| anyhow!("the job's display is gone"))?
                .map_err(|e| anyhow!(e))
        })?;
        let reply = resp.values.get("REPLY").cloned().unwrap_or_default();
        Ok(if numeric {
            Value::Num(
                Dec::parse(reply.trim())
                    .map_err(|_| anyhow!("DSPLY: the reply '{}' is not a number", reply.trim()))?,
            )
        } else {
            Value::str(&reply)
        })
    }
}

/// An argument of a procedure call.
enum Arg {
    /// Passed by reference: the caller's storage.
    Ref(Loc),
    /// VALUE or CONST: a value.
    Val(Value),
    /// *OMIT, or not passed (OPTIONS(*NOPASS)).
    Omit,
}

impl<'a> Machine<'a> {
    /// The storage an argument passed by reference names: a variable, an
    /// array element, a whole array or a data structure.
    fn loc_of(&mut self, ex: &Expr) -> Result<Loc> {
        let (parts, args, sym) = match ex {
            Expr::Name { parts, args } => (parts, args, self.sym(&parts.join("."))),
            Expr::Path { parts, idx, args } => (parts, args, Some(self.path_sym(parts, idx)?)),
            _ => bail!("a variable is needed for a parameter passed by reference, found {ex:?}"),
        };
        let name = parts.join(".");
        match sym {
            Some(Sym::Var(v)) => {
                let index = self.index_of(args)?;
                Ok(match index {
                    Some(_) => Loc {
                        buf: v.buf,
                        off: self.elem_off(&v, index)?,
                        size: v.dtype.size(),
                        dtype: v.dtype,
                    },
                    None => {
                        let size = v.stride * (v.dim.max(1) as usize - 1) + v.dtype.size();
                        Loc {
                            buf: v.buf,
                            off: v.off,
                            size,
                            dtype: if v.dim > 0 {
                                DType::Char(size as u32)
                            } else {
                                v.dtype
                            },
                        }
                    }
                })
            }
            Some(Sym::Ds(d)) => {
                let one = args.is_some();
                let off = self.ds_element(&d, args)?;
                let size = if one {
                    d.size
                } else {
                    d.size * d.dim.max(1) as usize
                };
                Ok(Loc {
                    buf: d.buf,
                    off,
                    size,
                    dtype: DType::Char(size as u32),
                })
            }
            _ => bail!("{name} is not a variable; it cannot be passed by reference"),
        }
    }

    /// A procedure or prototype of this program.
    fn callable(&self, name: &str) -> bool {
        self.prog.procs.contains_key(name) || self.prog.protos.contains_key(name)
    }

    /// The arguments of a call, as the parameters take them.
    fn args_for(
        &mut self,
        what: &str,
        params: &[super::program::ParamInfo],
        args: &[Expr],
    ) -> Result<Vec<Arg>> {
        if args.len() > params.len() {
            bail!(
                "{what}: {} parameters passed, it takes {}",
                args.len(),
                params.len()
            );
        }
        let mut out = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let Some(a) = args.get(i) else {
                if !p.nopass {
                    bail!("{what}: parameter {} is missing", p.name);
                }
                out.push(Arg::Omit);
                continue;
            };
            out.push(if matches!(a, Expr::Omit) {
                if !p.omit {
                    bail!("{what}: *OMIT is not allowed for parameter {}", p.name);
                }
                Arg::Omit
            } else if p.by_value || p.konst {
                Arg::Val(self.eval(a)?)
            } else {
                Arg::Ref(self.loc_of(a)?)
            });
        }
        Ok(out)
    }

    /// CALLP, or a call in an expression: a procedure of this program, or
    /// a prototype for one or for a program (EXTPGM). Returns the
    /// procedure's return value.
    fn call_named(&mut self, name: &str, args: &[Expr]) -> Result<Option<Value>> {
        let prog: &'a Program = self.prog;
        if let Some(pi) = prog.procs.get(name) {
            let a = self.args_for(name, &pi.params, args)?;
            return self.call_proc(pi, a);
        }
        let Some(pr) = prog.protos.get(name) else {
            bail!("{name} is not a procedure or prototype of this program");
        };
        if !pr.overload.is_empty() {
            let target = self.overload_target(pr, args)?;
            return self.call_named(&target, args);
        }
        if let Some(pgm) = &pr.extpgm {
            // The program name may be a named constant or a variable.
            let pgm = match self.sym(pgm) {
                Some(Sym::Const(v)) => a(&v.chars()?).trim().to_ascii_uppercase(),
                Some(Sym::Var(_)) => a(&self.eval(&Expr::name(pgm))?.chars()?)
                    .trim()
                    .to_ascii_uppercase(),
                _ => pgm.clone(),
            };
            let call_args = self.args_for(name, &pr.params, args)?;
            // Values (VALUE, CONST) are passed as copies in the
            // parameter's type.
            let base = self.bufs.len();
            let mut locs = Vec::new();
            for (arg, p) in call_args.into_iter().zip(&pr.params) {
                match arg {
                    Arg::Ref(l) => locs.push(l),
                    Arg::Val(v) => {
                        let mut buf = p.dtype.initial();
                        data::store(
                            &p.dtype,
                            &v,
                            &mut buf,
                            Fit::Strict { half_adjust: false },
                            false,
                        )?;
                        locs.push(Loc {
                            buf: self.bufs.len(),
                            off: 0,
                            size: buf.len(),
                            dtype: p.dtype,
                        });
                        self.bufs.push(buf);
                    }
                    Arg::Omit => break,
                }
            }
            let r = self.call_program(&pgm, &locs);
            self.bufs.truncate(base);
            r?;
            return Ok(None);
        }
        let target = pr.extproc.clone().unwrap_or_else(|| name.to_string());
        match prog.procs.get(&target) {
            Some(pi) => {
                let a = self.args_for(name, &pr.params, args)?;
                self.call_proc(pi, a)
            }
            None if target == "QP0ZLPRINTF" => {
                // Qp0zLprintf (C runtime): printf-style text to the job log.
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval(a)?);
                }
                let text = self.printf(&vals)?;
                self.job_log(&text);
                Ok(Some(Value::Num(Dec::from_i64(text.len() as i64))))
            }
            None => {
                let sym = target.to_ascii_uppercase();
                let bound = self
                    .io
                    .binding
                    .as_ref()
                    .and_then(|b| b.imports.get(&sym))
                    .cloned();
                let Some(t) = bound else {
                    bail!(
                        "CPD5D02: Definition for procedure {target} not found: it is not in this module, and the program is not bound to a module or service program that exports it (CRTPGM, CRTSRVPGM, BNDDIR)."
                    );
                };
                let a = self.args_for(name, &pr.params, args)?;
                self.call_bound(&sym, &t, a)
            }
        }
    }

    /// Call a procedure of another module: one bound into this program, or
    /// one exported by a bound service program. The module runs on its own
    /// global storage (a service program's is kept in the job's
    /// activation); parameters by reference are passed in and come back.
    fn call_bound(
        &mut self,
        sym: &str,
        target: &crate::ile::Target,
        args: Vec<Arg>,
    ) -> Result<Option<Value>> {
        use crate::ile::Target;
        let client = self.io.client;
        let libl = self.io.libl.clone();
        let (code, unit, binding) = match target {
            Target::Module(m) => {
                let b = self.io.binding.clone().unwrap_or_default();
                let code = b
                    .modules
                    .iter()
                    .find(|x| &x.name == m)
                    .cloned()
                    .ok_or_else(|| anyhow!("module {m} is not bound into the program"))?;
                (code, format!("MODULE {m}"), b)
            }
            Target::SrvPgm { library, name } => {
                let (lib, b) = self.block(crate::ile::find_srvpgm(client, &libl, library, name))?;
                self.block(crate::authority::check(
                    client,
                    &self.io.user,
                    &lib,
                    name,
                    "*SRVPGM",
                    crate::authority::Aut::Use,
                ))?;
                // The module that exports the procedure.
                let mut found = None;
                for m in &b.modules {
                    let p = self.block(super::compile_with(
                        client, &libl, &m.name, &m.source, &m.options,
                    ))?;
                    if p.procs.get(sym).is_some_and(|pi| pi.export) {
                        found = Some(m.clone());
                        break;
                    }
                }
                let code = found.ok_or_else(|| {
                    anyhow!("CPD5D02: Service program {lib}/{name} does not export {sym}.")
                })?;
                let unit = format!("{lib}/{name}/{}", code.name);
                (code, unit, std::sync::Arc::new(b))
            }
        };
        let prog = self.block(super::compile_with(
            client,
            &libl,
            &code.name,
            &code.source,
            &code.options,
        ))?;
        let pi = prog
            .procs
            .get(sym)
            .filter(|pi| pi.export)
            .ok_or_else(|| anyhow!("CPD5D02: Module {} does not export {sym}.", code.name))?;
        let srvpgm = matches!(target, Target::SrvPgm { .. });
        // The module's global storage: its activation, or its first use.
        let storage = if srvpgm {
            crate::ile::activation(&self.io.job_key, &unit)
        } else {
            self.io
                .modules
                .lock()
                .ok()
                .and_then(|m| m.get(&unit).cloned())
        };
        let mut m = Machine::new(
            &prog,
            Io {
                rt: self.io.rt.clone(),
                client,
                user: self.io.user.clone(),
                job_number: self.io.job_number,
                libl: libl.clone(),
                job: self.io.job.clone(),
                display: self.io.display.clone(),
                cl: self.io.cl.clone(),
                callers: self.io.callers.clone(),
                database_url: self.io.database_url.clone(),
                job_key: self.io.job_key.clone(),
                binding: Some(binding),
                unit: srvpgm.then(|| unit.clone()),
                modules: if srvpgm {
                    Default::default()
                } else {
                    self.io.modules.clone()
                },
            },
        );
        if let Some(s) = storage {
            m.bufs = s;
        }
        let globals = m.bufs.len();
        // Parameters by reference: the callee works on a copy that comes
        // back when it returns.
        let mut back = Vec::new();
        let args: Vec<Arg> = args
            .into_iter()
            .map(|a| match a {
                Arg::Ref(l) => {
                    let idx = m.bufs.len();
                    m.bufs
                        .push(self.bufs[l.buf][l.off..l.off + l.size].to_vec());
                    back.push((idx, l));
                    Arg::Ref(Loc {
                        buf: idx,
                        off: 0,
                        size: l.size,
                        dtype: l.dtype,
                    })
                }
                other => other,
            })
            .collect();
        let r = m.call_proc(pi, args);
        self.messages.append(&mut m.messages);
        self.printed.append(&mut m.printed);
        for (idx, l) in &back {
            let bytes = m.bufs[*idx].clone();
            self.bufs[l.buf][l.off..l.off + l.size].copy_from_slice(&bytes[..l.size]);
        }
        let kept: Vec<Vec<u8>> = m.bufs[..globals].to_vec();
        if srvpgm {
            crate::ile::keep_activation(&self.io.job_key, &unit, kept);
        } else if let Ok(mut g) = self.io.modules.lock() {
            g.insert(unit, kept);
        }
        r
    }

    /// Call a procedure of this program: a new frame of local storage,
    /// parameters by reference as aliases, its calculations, its return
    /// value in its return type.
    fn call_proc(&mut self, pi: &'a ProcInfo, args: Vec<Arg>) -> Result<Option<Value>> {
        if self.frames.len() > 1000 {
            bail!("procedure {}: calls are nested too deeply", pi.name);
        }
        let base = self.bufs.len();
        let mut alias = HashMap::new();
        let mut values = Vec::new();
        let mut passed = 0;
        let mut omitted = Vec::new();
        for (i, arg) in args.into_iter().enumerate() {
            let p = &pi.params[i];
            match arg {
                Arg::Ref(l) => {
                    if let Some(b) = p.buf {
                        alias.insert(b, (l.buf, l.off));
                    }
                    passed = i + 1;
                }
                Arg::Val(v) => {
                    values.push((p.name.clone(), v));
                    passed = i + 1;
                }
                Arg::Omit => {
                    if i < passed || p.omit {
                        passed = passed.max(i + 1);
                        omitted.push(p.name.clone());
                    }
                }
            }
        }
        self.bufs.extend(pi.frame.iter().cloned());
        self.frames.push(Frame {
            proc: pi,
            base,
            alias,
            passed,
            omitted,
        });
        let mut r = (|| -> Result<Option<Value>> {
            for (n, v) in values {
                self.assign(
                    &Expr::name(&n),
                    &v,
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
            }
            Ok(match self.run_code(&pi.routines.detail)? {
                Flow::Return(v) => v,
                _ => None,
            })
        })();
        // ON-EXIT runs however the procedure ends; its indicator says
        // whether it ended abnormally. A RETURN there replaces the value.
        if let Some((ind, exit)) = &pi.on_exit {
            let abnormal = r.is_err();
            let x = (|| -> Result<Option<Value>> {
                if let Some(n) = ind {
                    self.assign(
                        &Expr::name(n),
                        &Value::Ind(abnormal),
                        Fit::Strict { half_adjust: false },
                        false,
                    )?;
                }
                Ok(match self.run_code(&exit.detail)? {
                    Flow::Return(v) => v,
                    _ => None,
                })
            })();
            match x {
                Ok(Some(v)) if !abnormal => r = Ok(Some(v)),
                Ok(_) => {}
                Err(e) => r = Err(e),
            }
        }
        self.frames.pop();
        self.bufs.truncate(base);
        let v = r?;
        Ok(match (pi.returns, v) {
            (Some(t), Some(v)) => {
                let mut buf = t.initial();
                data::store(&t, &v, &mut buf, Fit::Strict { half_adjust: false }, false)?;
                Some(data::load(&t, &buf)?)
            }
            (Some(_), None) => bail!("procedure {} ended without returning a value", pi.name),
            (None, _) => None,
        })
    }

    /// CALL a program: an RPG program object (CRTBNDRPG) in the library
    /// list, or a program source in AS4TAINR_PGM_PATH. Parameters are
    /// passed by reference: the callee works on copies that come back
    /// when it returns.
    fn call_program(&mut self, name: &str, locs: &[Loc]) -> Result<()> {
        if self.system_program(name, locs)? {
            return Ok(());
        }
        // An RPG program that is active cannot be called again (ILE RPG
        // Reference: a program is not recursive; only procedures are).
        if name == self.prog.name || self.io.callers.iter().any(|c| c == name) {
            bail!("Program {name} called recursively: it is already active in the job.");
        }
        let client = self.io.client;
        let libl = self.io.libl.clone();
        let found = self.block(crate::catalog::find_program(client, &libl, name))?;
        let source = match found {
            Some(p)
                if p.attribute == "RPGLE" || p.attribute == "SQLRPGLE" || p.attribute == "RPG" =>
            {
                self.block(crate::authority::check(
                    client,
                    &self.io.user,
                    &p.library,
                    &p.name,
                    "*PGM",
                    crate::authority::Aut::Use,
                ))?;
                (p.source, p.options, p.binding)
            }
            Some(p) if p.attribute == "CLLE" || p.attribute == "CLP" => {
                // A CL program runs in the job; parameters go in as
                // values (changes it makes do not come back yet).
                let mut parms = Vec::new();
                for l in locs {
                    let bytes = &self.bufs[l.buf][l.off..l.off + l.size];
                    parms.push(match data::load(&l.dtype, bytes)? {
                        Value::Num(d) => d.to_string(),
                        other => format!("'{}'", a(&other.chars()?).replace('\'', "''")),
                    });
                }
                let parm = if parms.is_empty() {
                    String::new()
                } else {
                    format!(" PARM({})", parms.join(" "))
                };
                return self.run_cl(&format!("CALL PGM({}/{}){parm}", p.library, p.name));
            }
            Some(p) => bail!(
                "CALL {name}: {} programs cannot be called from RPG on the native engine yet",
                p.attribute
            ),
            None => (
                super::source_from_path(name)
                    .ok_or_else(|| anyhow!("CPF4101: Program {name} not found."))?,
                String::new(),
                None,
            ),
        };
        let (source, options, binding) = source;
        let callee = self.block(super::compile_with(client, &libl, name, &source, &options))?;
        let mut parms: Vec<Vec<u8>> = locs
            .iter()
            .map(|l| self.bufs[l.buf][l.off..l.off + l.size].to_vec())
            .collect();
        let r = {
            let mut m = Machine::new(
                &callee,
                Io {
                    rt: self.io.rt.clone(),
                    client,
                    user: self.io.user.clone(),
                    job_number: self.io.job_number,
                    libl: libl.clone(),
                    job: self.io.job.clone(),
                    display: self.io.display.clone(),
                    cl: self.io.cl.clone(),
                    callers: {
                        let mut c = self.io.callers.clone();
                        c.push(self.prog.name.clone());
                        c
                    },
                    database_url: self.io.database_url.clone(),
                    job_key: self.io.job_key.clone(),
                    binding: binding.map(std::sync::Arc::new),
                    unit: None,
                    modules: Default::default(),
                },
            );
            let r = m.run_bytes(&mut parms);
            self.messages.append(&mut m.messages);
            self.printed.append(&mut m.printed);
            r
        };
        r.map_err(|e| anyhow!("{e:#}\nProgram {name} ended abnormally."))?;
        for (l, bytes) in locs.iter().zip(parms) {
            self.bufs[l.buf][l.off..l.off + l.size].copy_from_slice(&bytes[..l.size]);
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

impl Machine<'_> {
    // ---- printer files (O specs, EXCEPT) -------------------------------

    fn out_cond(&self, groups: &[Conditions]) -> bool {
        groups.iter().all(|g| g.is_empty())
            || groups
                .iter()
                .any(|g| !g.is_empty() && self.conditions_met(g))
    }

    /// EXCEPT: the exception (E) records with that name (or, without a
    /// name, those without one) whose conditions are met.
    fn except(&mut self, name: Option<&str>) -> Result<()> {
        let prog = self.prog;
        for r in &prog.outputs {
            if r.kind == 'E' && r.name.as_deref() == name && self.out_cond(&r.cond) {
                self.print_record(r)?;
            }
        }
        Ok(())
    }

    /// Cycle output: heading and detail (H, D) or total (T) records. A
    /// printer whose overflow indicator was on has it set off afterwards.
    fn output(&mut self, kinds: &[char]) -> Result<()> {
        let prog = self.prog;
        let mut used: Vec<String> = Vec::new();
        for r in &prog.outputs {
            if kinds.contains(&r.kind) {
                if !used.contains(&r.file) {
                    used.push(r.file.clone());
                }
                if self.out_cond(&r.cond) {
                    self.print_record(r)?;
                }
            }
        }
        if kinds.contains(&'D') {
            for f in used {
                if let Some(of) = self.overflow_ind(&f) {
                    self.set_ind(&of, false);
                }
            }
        }
        Ok(())
    }

    /// OFLIND(*INOF), OFLIND(*IN10)... of a printer file: the indicator.
    fn overflow_ind(&self, file: &str) -> Option<String> {
        let i = self.prog.file_index(file)?;
        keyword(&self.prog.files[i].decl.keywords, "OFLIND")
            .and_then(|k| k.args.first())
            .map(|a| {
                let a = a.trim().to_ascii_uppercase();
                a.strip_prefix("*IN").map(str::to_string).unwrap_or(a)
            })
    }

    /// The printed text of one output field.
    fn out_text(&mut self, f: &OutField, page: i32) -> Result<String> {
        if let Some(lit) = &f.literal {
            return Ok(a(lit));
        }
        let name = f.name.clone().unwrap_or_default();
        let (value, digits, decs) = match name.as_str() {
            "PAGE" | "PAGE1" | "PAGE2" | "PAGE3" | "PAGE4" | "PAGE5" | "PAGE6" | "PAGE7" => {
                (Dec::from_i64(page as i64), 4, 0)
            }
            "UDATE" => (Dec::from_i64(today_num("UDATE")), 6, 0),
            "*DATE" => (Dec::from_i64(today_num("*DATE")), 8, 0),
            _ => {
                let ex = Expr::name(&name);
                let t = self
                    .type_of(&ex)
                    .ok_or_else(|| anyhow!("O spec field {name} is not defined"))?;
                let v = self.eval(&ex)?;
                if !t.is_numeric() {
                    return Ok(a(&v.chars()?));
                }
                let (p, s) = t.precision();
                (v.num()?, p, s)
            }
        };
        if let Some(w) = &f.edit_word {
            // A named constant holds the edit word when it is not quoted.
            let word = match w.strip_prefix('\u{1}') {
                Some(n) => match self.sym(n) {
                    Some(Sym::Const(v)) => a(&v.chars()?),
                    _ => bail!("edit word {n} is not a named constant"),
                },
                None => w.clone(),
            };
            return edit_word(&value, digits, decs, &word);
        }
        Ok(match f.edit_code {
            Some(c) => edit_code(&value, digits, decs, c),
            None => {
                // No edit code: every digit, a minus sign after a
                // negative number.
                let d = value.rescale(decs as u8, false).unwrap_or(value);
                let raw = format!("{:0>w$}", d.mantissa().unsigned_abs(), w = digits as usize);
                format!("{raw}{}", if d.is_negative() { "-" } else { "" })
            }
        })
    }

    /// Print one output record: space and skip before, the line, space
    /// and skip after (nothing given: space 1 after). Pages are
    /// `spool::PAGE_LENGTH` lines; printing on or spacing past
    /// `spool::OVERFLOW_LINE` sets the overflow indicator.
    fn print_record(&mut self, r: &OutRec) -> Result<()> {
        use crate::spool::{OVERFLOW_LINE as DEFAULT_OFL, PAGE_LENGTH as DEFAULT_LEN};
        let i = self
            .prog
            .file_index(&r.file)
            .ok_or_else(|| anyhow!("output to {}: not a file of this program", r.file))?;
        let decl = &self.prog.files[i].decl;
        if decl.device == "DISK" {
            return self.disk_output(i, r);
        }
        if decl.device != "PRINTER" {
            bail!(
                "output to {} ({}): only printer and database files are written by O specs in the native engine yet",
                r.file,
                decl.device
            );
        }
        let width = decl.record_len.unwrap_or(132) as usize;
        // FORMLEN and FORMOFL: the form length and overflow line (from an
        // RPG III line counter specification); 66 and 60 otherwise.
        let form = |k: &str, d: i32| -> i32 {
            keyword(&decl.keywords, k)
                .and_then(|k| k.args.first())
                .and_then(|a| a.trim().parse().ok())
                .unwrap_or(d)
        };
        let page_length = form("FORMLEN", DEFAULT_LEN);
        let overflow_line = form("FORMOFL", DEFAULT_OFL);
        if !self.printers.contains_key(&r.file) {
            let spl = match &self.io.job {
                Some(job) => Some(self.block(crate::spool::open(
                    self.io.client,
                    job,
                    &r.file,
                    &self.prog.name,
                ))?),
                None => None,
            };
            self.printers.insert(
                r.file.clone(),
                Printer {
                    spl,
                    page: 1,
                    line: 1,
                    printed_on_page: false,
                    buffer: Vec::new(),
                    out_pos: (1, 0),
                },
            );
        }
        let new_page = |p: &mut Printer| {
            p.page += 1;
            p.line = 1;
            p.printed_on_page = false;
        };
        let page = {
            let p = self.printers.get_mut(&r.file).unwrap();
            if let Some(s) = r.skip_before.filter(|s| *s > 0) {
                let s = s as i32;
                if s < p.line || s == p.line && p.printed_on_page {
                    new_page(p);
                }
                p.line = s;
            }
            if let Some(n) = r.space_before.filter(|n| *n > 0) {
                p.line += n as i32;
            }
            if p.line > page_length {
                new_page(p);
            }
            p.page
        };
        let mut buf = vec![' '; width];
        let mut prev_end = 0usize;
        for f in &r.fields {
            if !f.cond.is_empty() && !self.conditions_met(&f.cond) {
                continue;
            }
            let text: Vec<char> = self.out_text(f, page)?.chars().collect();
            let end = match (f.end, f.relative) {
                (Some(e), _) => e as usize,
                (None, Some(n)) => prev_end + n as usize + text.len(),
                (None, None) => prev_end + text.len(),
            };
            let start = end.saturating_sub(text.len());
            for (k, c) in text.iter().enumerate() {
                if let Some(slot) = buf.get_mut(start + k) {
                    *slot = *c;
                }
            }
            prev_end = end;
            if f.blank_after {
                if let Some(n) = &f.name {
                    self.stmt(&Stmt::Clear(Expr::name(n)))?;
                }
            }
        }
        let text: String = buf.into_iter().collect::<String>().trim_end().to_string();
        let overflow_ind = self.overflow_ind(&r.file);
        let p = self.printers.get_mut(&r.file).unwrap();
        p.buffer.push((p.page, p.line, text));
        p.printed_on_page = true;
        let (printed_page, printed_line) = (p.page, p.line);
        let mut moved = false;
        if let Some(s) = r.skip_after.filter(|s| *s > 0) {
            let s = s as i32;
            if s <= p.line {
                new_page(p);
            }
            p.line = s;
            moved = true;
        }
        if let Some(n) = r.space_after {
            p.line += n as i32;
            moved = true;
        }
        if !moved && r.skip_before.is_none() && r.space_before.is_none() {
            p.line += 1;
        }
        let overflow =
            printed_line >= overflow_line || p.page == printed_page && p.line > overflow_line;
        if p.line > page_length {
            new_page(p);
        }
        let full = p.buffer.len() >= 50;
        if overflow {
            if let Some(of) = overflow_ind {
                self.set_ind(&of, true);
            }
        }
        if full {
            self.flush_printer(&r.file)?;
        }
        Ok(())
    }

    fn flush_printer(&mut self, file: &str) -> Result<()> {
        let Some(p) = self.printers.get_mut(file) else {
            return Ok(());
        };
        let lines = std::mem::take(&mut p.buffer);
        match p.spl {
            Some(id) => {
                let client = self.io.client;
                self.io
                    .rt
                    .block_on(crate::spool::write(client, id, &lines))?;
            }
            None => {
                for (page, line, text) in lines {
                    if page > p.out_pos.0 {
                        self.printed.push("\x0c".to_string());
                        p.out_pos = (page, 0);
                    }
                    while p.out_pos.1 + 1 < line {
                        self.printed.push(String::new());
                        p.out_pos.1 += 1;
                    }
                    self.printed.push(text);
                    p.out_pos.1 = line;
                }
            }
        }
        Ok(())
    }

    /// The program ends: its spooled files are complete (RDY).
    fn close_printers(&mut self) -> Result<()> {
        let mut names: Vec<String> = self.printers.keys().cloned().collect();
        names.sort();
        for n in names {
            self.flush_printer(&n)?;
            if let Some(id) = self.printers.get(&n).and_then(|p| p.spl) {
                let client = self.io.client;
                self.io.rt.block_on(crate::spool::close(client, id))?;
            }
        }
        self.printers.clear();
        Ok(())
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
/// Edit a number with an edit word (ILE RPG Reference, "Edit Words"):
/// the body (blanks and the first 0 or * take digits; zero suppression
/// runs through the position of that 0 or *, which fills suppressed
/// positions with asterisks; constants print once a digit has, or after
/// the 0 or *; `&` is a blank; a `$` just before the 0 or * floats to the
/// first digit, one in the first position stays), then the status (CR or
/// a final minus, with what comes before it, printed for negative
/// values only), then the expansion (constants, `&` as blanks).
pub fn edit_word(v: &Dec, digits: u32, decs: u32, word: &str) -> Result<String> {
    let w: Vec<char> = word.chars().collect();
    let stop = w.iter().position(|c| *c == '0' || *c == '*');
    let last_blank = w.iter().rposition(|c| *c == ' ');
    let body_end = match (last_blank, stop) {
        (Some(b), Some(s)) => b.max(s),
        (Some(b), None) => b,
        (None, Some(s)) => s,
        (None, None) => bail!("the edit word '{word}' has no digit positions"),
    };
    let floating = stop.is_some_and(|s| s > 0 && w[s - 1] == '$');
    let positions = (0..=body_end)
        .filter(|&i| w[i] == ' ' || Some(i) == stop)
        .count();
    if (positions as u32) < digits {
        bail!("the edit word '{word}' has {positions} digit positions, the field {digits} digits");
    }
    let d = v.rescale(decs as u8, false).unwrap_or(*v);
    let text = format!("{:0>w$}", d.mantissa().unsigned_abs(), w = positions);
    let mut ds = text.chars().skip(text.len().saturating_sub(positions));
    let fill = if stop.is_some_and(|s| w[s] == '*') {
        '*'
    } else {
        ' '
    };
    let mut out: Vec<char> = Vec::with_capacity(w.len());
    let mut significant = false;
    let mut forced = false;
    let mut first_digit: Option<usize> = None;
    for (i, &c) in w.iter().enumerate().take(body_end + 1) {
        // A floating currency symbol takes no position of its own: it
        // goes in front of the first digit printed, after the body is
        // built.
        if floating && Some(i + 1) == stop {
            continue;
        }
        if c == ' ' || Some(i) == stop {
            let dg = ds.next().unwrap_or('0');
            if significant || forced || dg != '0' {
                if !significant && !forced {
                    first_digit = Some(out.len());
                }
                significant = true;
                out.push(dg);
            } else {
                out.push(fill);
            }
            if Some(i) == stop {
                forced = true;
                if first_digit.is_none() {
                    first_digit = Some(out.len());
                }
            }
        } else if c == '&' {
            out.push(' ');
        } else if c == '$' && i == 0 {
            out.push('$');
        } else if significant || forced {
            out.push(c);
        } else {
            out.push(fill);
        }
    }
    if floating {
        let k = first_digit.unwrap_or(out.len());
        out.insert(k, '$');
    }
    // Status: up to and including the first CR, or a minus that ends the
    // edit word.
    let rest: Vec<char> = w[body_end + 1..].to_vec();
    let rest_s: String = rest.iter().collect();
    let status_len = match rest_s.find("CR") {
        Some(p) => p + 2,
        None if rest.last() == Some(&'-') => rest.len(),
        None => 0,
    };
    let negative = d.is_negative();
    for &c in &rest[..status_len] {
        out.push(if negative {
            if c == '&' {
                ' '
            } else {
                c
            }
        } else {
            ' '
        });
    }
    for &c in &rest[status_len..] {
        out.push(if c == '&' { ' ' } else { c });
    }
    Ok(out.into_iter().collect())
}

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
        // Date edit (ILE RPG Reference, edit codes): nn/nn/nn for six
        // digits, nn/nn/nnnn for eight; the leftmost zero is suppressed.
        let s = if digits == 8 {
            format!("{}/{}/{}", &raw[0..2], &raw[2..4], &raw[4..8])
        } else {
            let s = format!("{:0>6}", raw);
            format!(
                "{}/{}/{}",
                &s[s.len() - 6..s.len() - 4],
                &s[s.len() - 4..s.len() - 2],
                &s[s.len() - 2..]
            )
        };
        return match s.strip_prefix('0') {
            Some(rest) => format!(" {rest}"),
            None => s,
        };
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
    } else if int_out.is_empty() {
        // Codes that do not blank zero show one zero.
        "0".to_string()
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
    fn edit_words() {
        // ILE RPG Reference, "Edit Words" (body, status, expansion): the
        // 0 ends zero suppression at its own position; constants print
        // after a significant digit or the 0; CR prints for negative
        // values only. Examples as in the RPG/400 and RPG II edit word
        // tables (e.g. 'bbAMT0' for 100 prints 10AMT0; for 5, b5).
        let ew =
            |v: &str, n: u32, d: u32, w: &str| edit_word(&Dec::parse(v).unwrap(), n, d, w).unwrap();
        assert_eq!(ew("100", 3, 0, "  AMT0"), "10AMT0");
        assert_eq!(ew("5", 3, 0, "  AMT0"), "     5");
        // A 0 that is not the last position: what follows always prints.
        assert_eq!(ew("5", 3, 0, "  0AMT "), "   AMT5");
        // Social security number: no suppression before the first digit
        // position (the 0 is first).
        assert_eq!(ew("123456789", 9, 0, "0   -  -    "), " 123-45-6789");
        // Commas and the decimal point.
        assert_eq!(ew("1000.00", 6, 2, "  ,  0.  "), " 1,000.00");
        assert_eq!(ew(".05", 6, 2, "  ,  0.  "), "      .05");
        // Status: CR for negative values, blanks otherwise; asterisks after
        // it are expansion and always print.
        assert_eq!(ew("-10", 3, 0, "  0CR*"), " 10CR*");
        assert_eq!(ew("10", 3, 0, "  0CR*"), " 10  *");
        assert_eq!(ew("-10", 3, 0, "  0-"), " 10-");
        // Asterisk protection.
        assert_eq!(ew(".10", 6, 2, "$  ,  *.  "), "$******.10");
        assert_eq!(ew("500.00", 6, 2, "$  ,  *.  "), "$***500.00");
        // Floating currency symbol: just left of the first digit.
        assert_eq!(ew(".05", 6, 2, "  ,  $0.  "), "      $.05");
        assert_eq!(ew("1000.00", 6, 2, "  ,  $0.  "), " $1,000.00");
        // The ampersand is a blank.
        assert_eq!(ew("12", 2, 0, "  &DOLLARS"), "12 DOLLARS");
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
        // J does not blank a zero value: one zero, then the sign position.
        assert_eq!(edit_code(&Dec::parse("0").unwrap(), 3, 0, 'J'), "  0 ");
        // Y: nn/nn/nn, nn/nn/nnnn for eight digits, leftmost zero
        // suppressed (ILE RPG Reference, "Y edit code").
        assert_eq!(
            edit_code(&Dec::parse("101626").unwrap(), 6, 0, 'Y'),
            "10/16/26"
        );
        assert_eq!(
            edit_code(&Dec::parse("51626").unwrap(), 6, 0, 'Y'),
            " 5/16/26"
        );
        assert_eq!(
            edit_code(&Dec::parse("10162026").unwrap(), 8, 0, 'Y'),
            "10/16/2026"
        );
    }

    #[test]
    fn indicator_names() {
        assert_eq!(ind_name("3"), "03");
        assert_eq!(ind_name("lr"), "LR");
    }
}
