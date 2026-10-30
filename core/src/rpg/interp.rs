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
use super::program::{Display, DsInfo, ProcInfo, Program, Sym, VarInfo};
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
#[path = "cee.rs"]
pub mod cee;
#[path = "crt.rs"]
pub mod crt;
#[path = "cycle.rs"]
mod cycle;
#[path = "errors.rs"]
mod errors;
#[path = "exec_sql.rs"]
mod exec_sql;
#[path = "ops.rs"]
mod ops;
#[path = "printer.rs"]
mod printer;
#[path = "ptrs.rs"]
mod ptrs;
#[path = "xmlinto.rs"]
mod xmlinto;
#[path = "xmlsax.rs"]
pub mod xmlsax;

/// What the program runs with: the database, the job.
pub struct Io<'a> {
    pub rt: Handle,
    pub client: &'a Client,
    /// Record changes of files not under commitment control go to this
    /// session instead of `client` (a trigger program runs in the session
    /// of the change that called it).
    pub data: Option<&'a Client>,
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
    /// The activation group the program runs in (ILE Concepts,
    /// "Activation groups"): *DFTACTGRP, a name, or *NEWn.
    pub actgrp: String,
    /// The program object running: LIB/NAME.
    pub program: String,
    /// Exported data (EXPORT/IMPORT) of the program's modules and service
    /// programs, by symbol.
    pub shared: SharedData,
}

/// Exported data by symbol: the bytes of the exporting definition.
pub type SharedData = std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>;

/// A program that ended without LR (ILE RPG Reference, "Program
/// termination"): what its next call in the activation group finds — its
/// storage, indicators, open files and the storage of its bound modules.
struct Active {
    bufs: super::mem::Mem,
    inds: HashMap<String, bool>,
    files: Vec<Option<OpenFile>>,
    modules: HashMap<String, Vec<Vec<u8>>>,
    occur: HashMap<usize, (usize, u32, u32)>,
    table_cur: HashMap<String, i128>,
    var_elems: HashMap<String, u32>,
    shared: HashMap<String, Vec<u8>>,
}

/// Programs active in each job's activation groups: (job, group,
/// program) → its state.
type ActivePrograms = HashMap<(String, String, String), Active>;

static PROGRAMS: std::sync::Mutex<Option<ActivePrograms>> = std::sync::Mutex::new(None);

/// The open files and printers of service program modules in each job's
/// activation groups: (job, group, unit) → files (by index), printers.
type ModuleFiles =
    HashMap<(String, String, String), (Vec<Option<OpenFile>>, HashMap<String, Printer>)>;

static MODULE_FILES: std::sync::Mutex<Option<ModuleFiles>> = std::sync::Mutex::new(None);

/// Spooled files of ended activation groups' printers, to be closed
/// (status RDY) by whoever has the database at hand ([`close_ended_spools`]).
static ENDED_SPOOLS: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Close the spooled files of printers whose activation group ended.
pub async fn close_ended_spools(client: &Client) {
    let ids = ENDED_SPOOLS
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default();
    for id in ids {
        let _ = crate::spool::close(client, id).await;
    }
}

/// A module that bound calls run: its unit (LIB/SRVPGM/MODULE), how its
/// service program is bound, the module compiled.
type BoundModule = (
    u64,
    String,
    std::sync::Arc<crate::ile::Binding>,
    std::sync::Arc<Program>,
);

/// Bound calls resolved in each job's activation groups: (job, group,
/// caller and target) → the module.
type BoundCache = HashMap<(String, String, String), BoundModule>;
static BOUND: std::sync::Mutex<Option<BoundCache>> = std::sync::Mutex::new(None);

/// RCLACTGRP, RCLRSC, the end of a job: the programs active in those
/// activation groups end (their files close; the next call starts them
/// anew). The number ended.
pub fn end_programs(job: &str, groups: &crate::ile::Groups) -> usize {
    // Their cursors close too (service programs' modules included).
    exec_sql::end_activations(job, groups);
    if let Ok(mut g) = BOUND.lock() {
        if let Some(m) = g.as_mut() {
            m.retain(|(j, grp, _), _| !(j == job && groups.covers(grp)));
        }
    }
    if let Ok(mut g) = MODULE_FILES.lock() {
        if let Some(m) = g.as_mut() {
            // Their printer files' spooled files are complete.
            let mut ended = Vec::new();
            m.retain(|(j, grp, _), (_, printers)| {
                let end = j == job && groups.covers(grp);
                if end {
                    ended.extend(printers.values().filter_map(|p| p.spl));
                }
                !end
            });
            if let Ok(mut e) = ENDED_SPOOLS.lock() {
                e.extend(ended);
            }
        }
    }
    let mut n = 0;
    if let Ok(mut g) = PROGRAMS.lock() {
        if let Some(m) = g.as_mut() {
            let before = m.len();
            m.retain(|(j, grp, _), _| !(j == job && groups.covers(grp)));
            n = before - m.len();
        }
    }
    n
}

/// Whether a program is active (ended without LR) in a job.
pub fn program_active(job: &str, group: &str, program: &str) -> bool {
    PROGRAMS
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|m| m.contains_key(&(job.into(), group.into(), program.into())))
        })
        .unwrap_or(false)
}

/// The activation group a program runs in (ILE RPG Reference, DFTACTGRP
/// and ACTGRP keywords; CRTBNDRPG, CRTPGM): DFTACTGRP(*YES), the
/// default for a program without ACTGRP or BNDDIR, is the default
/// activation group; ACTGRP(*CALLER) the caller's; *NEW a new one each
/// call; *STGMDL (the default with DFTACTGRP(*NO)) QILE.
pub fn program_group(prog: &Program, caller: &str) -> String {
    let ctl = |k: &str| keyword(&prog.control, k).and_then(|k| k.args.first().cloned());
    let dft = ctl("DFTACTGRP").map(|a| a.trim().eq_ignore_ascii_case("*YES"));
    let actgrp = ctl("ACTGRP").map(|a| a.trim().trim_matches('\'').to_ascii_uppercase());
    let bnddir = keyword(&prog.control, "BNDDIR").is_some();
    match (dft, actgrp) {
        (Some(true), _) => crate::ile::DEFAULT_GROUP.into(),
        (None, None) if !bnddir => crate::ile::DEFAULT_GROUP.into(),
        (_, Some(a)) if a == "*CALLER" => caller.into(),
        (_, Some(a)) if a == "*NEW" => {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            format!(
                "*NEW{}",
                N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            )
        }
        (_, Some(a)) if a != "*STGMDL" && a != "*ENTMOD" => a,
        _ => "QILE".into(),
    }
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
    /// The status code (ILE RPG Reference, "File Status Codes",
    /// "Program Status Codes"): 0 normal, 11 end of file, 12 not found,
    /// an error's code.
    code: u32,
    /// The message identifier of the error (INFDS 46-52).
    msgid: String,
    /// The last operation (INFDS *OPCODE) and record (INFDS *RECORD).
    opcode: String,
    record: String,
}

struct FileState {
    open: Option<OpenFile>,
    /// A program-described file: the image of the record last read.
    image: Option<Vec<u8>>,
    /// A display file is open.
    ws_open: bool,
    status: Status,
    /// I/O feedback: writes, reads and other operations.
    writes: i64,
    reads: i64,
    others: i64,
    /// A display file: the key that ended the last input.
    aid: Option<String>,
    /// Opened under commitment control (the COMMIT keyword): its I/O runs
    /// in the commitment definition.
    cmt: Option<std::sync::Arc<crate::cmtctl::Definition>>,
    /// Has trigger programs (ADDPFTRG); None until the first change.
    pftrg: Option<bool>,
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
    bufs: super::mem::Mem,
    /// The number in pointers to this module's global storage: its
    /// activation's (a service program module's), else this run's.
    owner: u32,
    /// This run's number in pointers to its procedures' local storage.
    frame_owner: u32,
    /// A pointer that addressed nothing during the statement running.
    ptr_fault: std::cell::RefCell<Option<String>>,
    /// Heap blocks holding OPTIONS(*STRING) arguments of calls running.
    temp_strings: Vec<usize>,
    /// The heap block of errno (C runtime).
    errno_block: Option<usize>,
    /// The heap is the caller's: a program called in the caller's
    /// activation group shares its heap (ILE Concepts: the default heap
    /// belongs to the activation group), so it neither keeps nor
    /// restores one of its own.
    heap_lent: bool,
    /// Indicators: 01-99 and the named ones (LR, RT, KA-KY, H1-H9,
    /// L1-L9, OF, OV, OA-OG, U1-U8, 1P, MR).
    inds: HashMap<String, bool>,
    files: Vec<FileState>,
    last: Status,
    io: Io<'a>,
    /// DSPLY output, in order.
    pub messages: Vec<String>,
    /// The keys of the messages this program (and those it called) sent
    /// with the message APIs, for its job's printed job log.
    pub logged: Vec<u32>,
    /// The entry parameters' values when the program ended (`run`).
    pub returned: Vec<String>,
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
    /// Called again while still active (it ended without LR): no
    /// initialization (*INZSR, first-cycle output) this time.
    reactivated: bool,
    /// The routine running (*INIT, *DETC, *DETL, *GETIN, *TOTC, *TOTL,
    /// *OFL, *TERM, a subroutine or a procedure) and the statement, for
    /// the status data structures.
    routine: String,
    line: usize,
    /// Error subroutines running for errors in themselves.
    in_handler: u32,
    /// The file of the last file error.
    err_file: Option<usize>,
    /// Printer files whose overflow lines were printed since their
    /// overflow indicator last came on (ILE RPG Reference, "Overflow
    /// Routine": not printed again until overflow occurs again).
    ofl_written: std::collections::HashSet<String>,
    /// Overflow indicators set on during detail calculations or output:
    /// the cycle does not set them off at the start of the next cycle.
    of_late: std::collections::HashSet<String>,
    /// The cycle is in detail calculations or detail output.
    in_detail: bool,
    /// Overflow indicators of the printer files: (indicator, file).
    ofl_inds: Vec<(String, String)>,
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
    /// File parameters (LIKEFILE): the caller's file.
    files: HashMap<String, usize>,
    /// Each argument as an operational descriptor gives it (CEEDOD,
    /// CEEGSI): its current length, its data type code (2 character, 4
    /// character with a 2-byte length, 9 graphic, 11 graphic with a 2-byte
    /// length) and its maximum length.
    lengths: Vec<Option<(usize, i32, usize)>>,
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
    /// The lines also go to `printed` (standard output of `system`).
    echo: bool,
}

/// The bytes a variable (all its elements) takes.
/// Function key indicators *INKA-*INKY for F1-F24 (there is no *INKO).
const FKEYS: &[u8; 24] = b"ABCDEFGHIJKLMNPQRSTUVWXY";

fn ind_name(n: &str) -> String {
    let t = n.trim().to_ascii_uppercase();
    match t.parse::<u32>() {
        Ok(v) if t.len() <= 2 => format!("{v:02}"),
        _ => t,
    }
}

/// A value as a float: a big float as it is, a number converted.
/// A module's global storage put where a call back finds it during a
/// call out: (its activation unit, or the program's module key, and the
/// storage as it was).
type Reentry = Option<(Option<String>, String, Vec<Vec<u8>>)>;

fn float_value(v: &Value) -> Result<f64> {
    match v {
        Value::Float(x) => Ok(*x),
        other => Ok(other.num()?.to_string().parse().unwrap_or(0.0)),
    }
}

/// Number text as %CHAR gives it: leading zeros removed (also the zero
/// before the decimal point), a leading '-' for negative values.
pub fn char_of_num(d: &Dec) -> String {
    char_of_num_edit(d, '.', false)
}

/// %CHAR of a number with the control specification's DECEDIT: the
/// decimal point character, and a zero before it when the value has no
/// integer digits ('0.' or '0,') (ILE RPG Reference, DECEDIT, %CHAR).
pub fn char_of_num_edit(d: &Dec, point: char, zero: bool) -> String {
    let s = d.abs().to_string();
    let s = if zero && s.starts_with("0.") && d.scale() > 0 {
        s.replacen('.', &point.to_string(), 1)
    } else {
        let t = if d.scale() > 0 {
            s.trim_start_matches('0').to_string()
        } else {
            s
        };
        t.replacen('.', &point.to_string(), 1)
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
        let owner = super::mem::new_owner();
        Machine {
            prog,
            bufs: {
                let mut m = super::mem::Mem::new(prog.globals.clone());
                m.scratch(Self::largest_based(prog));
                m
            },
            owner,
            frame_owner: owner,
            ptr_fault: Default::default(),
            temp_strings: Vec::new(),
            errno_block: None,
            heap_lent: false,
            inds: HashMap::new(),
            files: prog
                .files
                .iter()
                .map(|_| FileState {
                    open: None,
                    image: None,
                    ws_open: false,
                    status: Status::default(),
                    writes: 0,
                    reads: 0,
                    others: 0,
                    aid: None,
                    cmt: None,
                    pftrg: None,
                })
                .collect(),
            last: Status::default(),
            io,
            messages: Vec::new(),
            logged: Vec::new(),
            returned: Vec::new(),
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
            reactivated: false,
            routine: "*INIT".into(),
            line: 0,
            in_handler: 0,
            err_file: None,
            ofl_written: Default::default(),
            of_late: Default::default(),
            in_detail: false,
            ofl_inds: prog
                .files
                .iter()
                .filter(|f| f.decl.device == "PRINTER")
                .filter_map(|f| {
                    let k = keyword(&f.decl.keywords, "OFLIND")?;
                    let a = k.args.first()?.trim().to_ascii_uppercase();
                    let a = a.strip_prefix("*IN").map(str::to_string).unwrap_or(a);
                    Some((ind_name(&a), f.decl.name.clone()))
                })
                .collect(),
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
    /// A name's definition without its storage looked up: what %SIZE and
    /// %ELEM need (based storage need not have an address then).
    fn sym_static(&self, name: &str) -> Option<Sym> {
        self.frames
            .last()
            .and_then(|f| f.proc.syms.get(name))
            .or_else(|| self.prog.syms.get(name))
            .cloned()
    }

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
                        (v.buf, v.off) = match f.proc.based.get(&v.buf) {
                            // Based storage need hold only what is used: one
                            // element here; each element is checked as used.
                            Some(p) => self.based_place(p, v.off, v.dtype.size()),
                            None => place(v.buf, v.off),
                        };
                        v.local = false;
                        Sym::Var(v)
                    }
                    Sym::Ds(mut d) if d.local => {
                        (d.buf, d.off) = match f.proc.based.get(&d.buf) {
                            Some(p) => self.based_place(p, d.off, d.size),
                            None => place(d.buf, d.off),
                        };
                        d.local = false;
                        Sym::Ds(d)
                    }
                    other => other,
                });
            }
        }
        // Based storage: where its pointer points now.
        match self.prog.syms.get(name) {
            Some(Sym::Var(v)) if self.prog.based.contains_key(&v.buf) => {
                let mut v = v.clone();
                (v.buf, v.off) = self.based_place(&self.prog.based[&v.buf], v.off, v.dtype.size());
                return Some(Sym::Var(v));
            }
            Some(Sym::Ds(d)) if self.prog.based.contains_key(&d.buf) => {
                let mut d = d.clone();
                (d.buf, d.off) = self.based_place(&self.prog.based[&d.buf], d.off, d.size);
                return Some(Sym::Ds(d));
            }
            _ => {}
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
                let at = d.off + (i as usize - 1) * d.size;
                self.reach(d.buf, at, d.size)?;
                Ok(at)
            }
        }
    }

    pub fn ind(&self, n: &str) -> bool {
        self.inds.get(&ind_name(n)).copied().unwrap_or(false)
    }

    pub fn set_ind(&mut self, n: &str, v: bool) {
        let n = ind_name(n);
        // An overflow indicator coming on: overflow occurred again, the
        // file's overflow lines are due.
        if v && !self.ofl_inds.is_empty() && self.inds.get(&n) != Some(&true) {
            for (ind, file) in &self.ofl_inds {
                if *ind == n {
                    self.ofl_written.remove(file);
                    if self.in_detail {
                        self.of_late.insert(n.clone());
                    }
                }
            }
        }
        self.inds.insert(n, v);
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
                let at = v.off + (i as usize - 1) * v.stride;
                self.reach(v.buf, at, v.dtype.size())?;
                Ok(at)
            }
        }
    }

    /// An array element of based storage must be in the storage its
    /// pointer addresses (MCH0601 otherwise).
    fn reach(&self, buf: usize, at: usize, size: usize) -> Result<()> {
        if buf == super::mem::BAD_BUF {
            return Ok(());
        }
        if let Some(b) = self.bufs.get(buf) {
            if at + size > b.len() {
                bail!(
                    "MCH0601: {size} bytes at offset {at} are outside the {} bytes of storage the pointer addresses.",
                    b.len()
                );
            }
        }
        Ok(())
    }

    /// Storage reached through a pointer that addresses nothing: the
    /// pointer fault, as the error.
    fn check_buf(&self, buf: usize) -> Result<()> {
        if buf == super::mem::BAD_BUF {
            let f = self
                .take_fault()
                .unwrap_or_else(|| "MCH3601: the pointer does not address storage.".to_string());
            bail!(f);
        }
        Ok(())
    }

    fn load_var(&self, v: &VarInfo, index: Option<i128>) -> Result<Value> {
        self.check_buf(v.buf)?;
        let off = self.elem_off(v, index)?;
        let b = &self.bufs[v.buf];
        let n = v.dtype.size();
        if off + n > b.len() {
            // A parameter longer than the argument passed for it
            // (OPTIONS(*VARSIZE)): what is there, then blanks.
            let mut tmp = b.get(off..).unwrap_or(&[]).to_vec();
            tmp.resize(n, 0x40);
            return data::load(&v.dtype, &tmp);
        }
        data::load(&v.dtype, &b[off..off + n])
    }

    fn store_var(
        &mut self,
        v: &VarInfo,
        index: Option<i128>,
        val: &Value,
        fit: Fit,
        right: bool,
    ) -> Result<()> {
        self.check_buf(v.buf)?;
        let offs: Vec<usize> = match index {
            // Assigning to an array without an index sets every element.
            None if v.dim > 0 => (0..v.dim as usize).map(|k| v.off + k * v.stride).collect(),
            _ => vec![self.elem_off(v, index)?],
        };
        for off in offs {
            let n = v.dtype.size();
            let have = self.bufs[v.buf].len();
            if off + n > have {
                // A parameter longer than its argument (OPTIONS(*VARSIZE)):
                // only the argument's storage is changed.
                let mut tmp = vec![0x40u8; n];
                data::store(&v.dtype, val, &mut tmp, fit, right)?;
                let k = have.saturating_sub(off);
                self.bufs[v.buf][off.min(have)..have].copy_from_slice(&tmp[..k]);
                continue;
            }
            let out = &mut self.bufs[v.buf][off..off + n];
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
                        self.check_buf(d.buf)?;
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
                    // A float stays a float (it may be beyond a decimal's
                    // 63 digits: -1.797693134862315E+308).
                    UnOp::Neg if matches!(v, Value::Float(_)) => match v {
                        Value::Float(f) => Value::Float(-f),
                        _ => unreachable!(),
                    },
                    UnOp::Plus if matches!(v, Value::Float(_)) => v,
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
                if let Some(r) = self.ptr_arith(op, &x, &y) {
                    return r;
                }
                // A date plus a time is a timestamp (ILE RPG Reference,
                // date and time arithmetic in expressions).
                if *op == BinOp::Add {
                    let (lt, rt) = (self.temporal_type(l), self.temporal_type(r));
                    let pair = match (lt, rt) {
                        (Some(DType::Date(_)), Some(DType::Time(_))) => Some((&x, &y)),
                        (Some(DType::Time(_)), Some(DType::Date(_))) => Some((&y, &x)),
                        _ => None,
                    };
                    if let Some((d, t)) = pair {
                        let d = a(&d.chars()?);
                        let t = a(&t.chars()?);
                        let ts = format!("{}-{}.000000", d.trim(), t.trim().replace(':', "."));
                        super::dates::parse_ts(&ts)?;
                        return Ok(Value::str(&ts));
                    }
                }
                // An operand of float type makes the operation a float one:
                // the other operand is converted to float first (ILE RPG
                // Reference, "Floating-Point Arithmetic" / expression
                // operands; OSSILE's arraylist unit test compares a decimal
                // literal of 19 digits with an 8-byte float it stored).
                if matches!(
                    op,
                    BinOp::Add
                        | BinOp::Sub
                        | BinOp::Mul
                        | BinOp::Div
                        | BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                ) && (self.is_float(l) || self.is_float(r))
                    && !matches!(x, Value::Char(_) | Value::Ind(_))
                    && !matches!(y, Value::Char(_) | Value::Ind(_))
                {
                    let (fx, fy) = (float_value(&x)?, float_value(&y)?);
                    let num = |f: f64| -> Value {
                        match data::float_dec(f) {
                            Ok(d) => Value::Num(d),
                            Err(_) => Value::Float(f),
                        }
                    };
                    return Ok(match op {
                        BinOp::Add => num(fx + fy),
                        BinOp::Sub => num(fx - fy),
                        BinOp::Mul => num(fx * fy),
                        BinOp::Div => {
                            if fy == 0.0 {
                                bail!(
                                    "MCH1211: Attempt made to divide by zero for floating point."
                                );
                            }
                            num(fx / fy)
                        }
                        BinOp::Eq => Value::Ind(fx == fy),
                        BinOp::Ne => Value::Ind(fx != fy),
                        BinOp::Lt => Value::Ind(fx < fy),
                        BinOp::Le => Value::Ind(fx <= fy),
                        BinOp::Gt => Value::Ind(fx > fy),
                        _ => Value::Ind(fx >= fy),
                    });
                }
                match op {
                    BinOp::Add => match (&x, &y) {
                        // An indicator is character data of length 1 (ILE
                        // RPG Reference, "Indicator Data Type"): + of two
                        // concatenates them.
                        (Value::Char(_), _)
                        | (_, Value::Char(_))
                        | (Value::Ind(_), Value::Ind(_)) => {
                            let mut s = x.chars()?;
                            s.extend(y.chars()?);
                            Value::Char(s)
                        }
                        _ => Value::Num(self.to_rule_scale(ex, x.num()?.add(&y.num()?)?)),
                    },
                    BinOp::Sub => Value::Num(self.to_rule_scale(ex, x.num()?.sub(&y.num()?)?)),
                    BinOp::Mul => Value::Num(self.to_rule_scale(ex, x.num()?.mul(&y.num()?)?)),
                    BinOp::Div => {
                        let (a, b) = (x.num()?, y.num()?);
                        Value::Num(self.divide(l, r, &a, &b)?)
                    }
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
    /// Division with the decimal places the ILE RPG precision rules give
    /// the quotient (ILE RPG Reference, "Precision of Intermediate
    /// Results": Dr = max(M - ((L1 - D1) + D2), 0)). The reference's table
    /// has M = 63; IBM i 7.5 computes with M = 30 when no operand has more
    /// digits (PUB400, tools/golden/rpg-prec.txt: %CHAR(10 / 3) has 28
    /// decimals, %CHAR(100 / 3) 27, a 7,2 field / 3 25, an INT(10) / 3 20).
    /// Operands whose precision is not known count their own digits. Fewer
    /// places when the quotient would not fit in as4tainr's 38 digits.
    fn divide(&self, l: &Expr, r: &Expr, a: &Dec, b: &Dec) -> Result<Dec> {
        let (l1, d1) = self
            .num_precision(l)
            .unwrap_or(((a.int_digits() + a.scale() as u32).max(1), a.scale() as u32));
        let (l2, d2) = self
            .num_precision(r)
            .unwrap_or(((b.int_digits() + b.scale() as u32).max(1), b.scale() as u32));
        let m: u32 = if l1.max(l2) > 30 { 63 } else { 30 };
        let mut dr = m.saturating_sub(l1.saturating_sub(d1) + d2).min(37);
        loop {
            match a.div_scale(b, dr as u8) {
                Ok(q) => return Ok(q),
                Err(e) if dr == 0 || e.to_string().starts_with("MCH1211") => return Err(e),
                Err(_) => dr -= 1,
            }
        }
    }

    /// A sum, difference or product with the decimal places the precision
    /// rules give it (IBM i 7.5: %CHAR(p * p) of two 7,2 fields holding 10
    /// is 100.0000), when they are known; extra places cut off.
    fn to_rule_scale(&self, ex: &Expr, v: Dec) -> Dec {
        match self.num_precision(ex) {
            Some((_, dr)) if dr <= 37 => v.rescale(dr as u8, false).unwrap_or(v),
            _ => v,
        }
    }

    /// The digits and decimal places of a numeric expression under the ILE
    /// RPG precision rules, when they can be worked out: fields from their
    /// definitions, literals by their digits, +, -, * and / by the rules'
    /// table with 30 digits at most (63 with a longer operand), %DEC(x: p:
    /// s). Checked on IBM i 7.5 (tools/golden/rpg-prec.txt).
    fn num_precision(&self, ex: &Expr) -> Option<(u32, u32)> {
        match ex {
            Expr::Num(d) => {
                let s = d.scale() as u32;
                Some(((d.int_digits() + s).max(1), s))
            }
            Expr::Name { .. } | Expr::Path { .. } => match self.type_of(ex)? {
                DType::Packed(p, s) | DType::Zoned(p, s) | DType::Binary(p, s) => Some((p, s)),
                DType::Int(n) | DType::Uns(n) => Some((n, 0)),
                _ => None,
            },
            Expr::Unary(UnOp::Neg | UnOp::Plus, e) => self.num_precision(e),
            Expr::Bin(op, l, r) => {
                let (l1, d1) = self.num_precision(l)?;
                let (l2, d2) = self.num_precision(r)?;
                let m: u32 = if l1.max(l2) > 30 { 63 } else { 30 };
                match op {
                    BinOp::Add | BinOp::Sub => {
                        let t = (l1.saturating_sub(d1).max(l2.saturating_sub(d2)) + 1).min(m);
                        let dr = d1.max(d2).min(m - t);
                        Some((t + dr, dr))
                    }
                    BinOp::Mul => {
                        let lr = (l1 + l2).min(m);
                        let dr = (d1 + d2)
                            .min(m - (l1.saturating_sub(d1) + l2.saturating_sub(d2)).min(m));
                        Some((lr, dr))
                    }
                    BinOp::Div => Some((m, m.saturating_sub(l1.saturating_sub(d1) + d2))),
                    _ => None,
                }
            }
            Expr::Bif(b, args) if matches!(b.as_str(), "DEC" | "DECH") && args.len() >= 3 => {
                let n = |e: &Expr| match e {
                    Expr::Num(d) => d.to_i128().ok().map(|v| v as u32),
                    _ => None,
                };
                Some((n(&args[1])?, n(&args[2])?))
            }
            Expr::Bif(b, args) if b == "ABS" => self.num_precision(args.first()?),
            _ => None,
        }
    }

    /// Whether an expression has a float type: a float field, a call of
    /// a procedure returning a float, %FLOAT, or arithmetic with one.
    fn is_float(&self, ex: &Expr) -> bool {
        match ex {
            Expr::Name { parts, args } => match self.type_of(ex) {
                Some(t) => matches!(t, DType::Float(_)),
                None if args.is_some() || parts.len() == 1 => {
                    let n = parts.join(".").to_ascii_uppercase();
                    let ret = self
                        .proto(&n)
                        .and_then(|p| p.returns)
                        .or_else(|| self.prog.procs.get(&n).and_then(|p| p.returns));
                    matches!(ret, Some(DType::Float(_)))
                }
                None => false,
            },
            Expr::Path { .. } => matches!(self.type_of(ex), Some(DType::Float(_))),
            Expr::Bif(b, _) => b == "FLOAT",
            Expr::Unary(UnOp::Neg | UnOp::Plus, e) => self.is_float(e),
            Expr::Bin(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow, l, r) => {
                self.is_float(l) || self.is_float(r)
            }
            _ => false,
        }
    }

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
                    .file_idx(&n)
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
                let x = float_value(&arg(self, 0)?)?;
                Value::str(&data::float_text(x, n))
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
                let x = float_value(&arg(self, 0)?)?;
                Value::str(&data::float_text(x, n))
            }
            // %UCS2 and %GRAPH: UCS-2 and graphic values are converted to
            // and from character data when they are used (see data.rs).
            "UCS2" | "GRAPH" => Value::Char(arg(self, 0)?.chars()?),
            // %FLOAT: the value as a float (8 bytes).
            "FLOAT" => {
                // A character argument is a number written as %DEC reads
                // it, or in float form (1.5E3) (ILE RPG Reference,
                // %FLOAT: character data interpreted as a numeric value).
                let x = match arg(self, 0)? {
                    Value::Char(c) => {
                        let t = a(&c);
                        match char_number(&t) {
                            Ok(d) => float_value(&Value::Num(d))?,
                            Err(e) => t.trim().replace(',', ".").parse::<f64>().map_err(|_| e)?,
                        }
                    }
                    v => float_value(&v)?,
                };
                match data::float_dec(x) {
                    Ok(d) => Value::Num(d),
                    Err(_) => Value::Float(x),
                }
            }
            "CHAR" => {
                let v = arg(self, 0)?;
                match v {
                    Value::Num(d) => {
                        let (point, zero) = self.prog.decedit();
                        Value::str(&char_of_num_edit(&d, point, zero))
                    }
                    Value::Ind(x) => Value::Char(vec![if x { 0xF1 } else { 0xF0 }]),
                    Value::Float(x) => Value::str(&data::float_text(x, 8)),
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
                    Some(x) => {
                        let n = self.eval(x)?.num()?.to_i128()?;
                        if n < 0 {
                            bail!("RNQ0100: %SUBST length {n} is negative.");
                        }
                        n as usize
                    }
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
                // %LEN(varying : *MAX): the most characters it can hold
                // (ILE RPG Reference, %LEN).
                if self.special_arg(args, 1).as_deref() == Some("*MAX") {
                    return Ok(match self.type_of(a0) {
                        Some(DType::Varchar(n) | DType::VarUcs2(n)) => {
                            Value::Num(Dec::from_i64(i64::from(n)))
                        }
                        _ => bail!("%LEN(...: *MAX) needs a varying-length value"),
                    });
                }
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
                    Expr::Name { parts, .. } => match self.sym_static(&parts.join(".")) {
                        Some(Sym::Var(v)) => {
                            v.dtype.size()
                                * if args.len() > 1 {
                                    v.dim.max(1) as usize
                                } else {
                                    1
                                }
                        }
                        Some(Sym::Ds(d)) => {
                            d.size
                                * if args.len() > 1 {
                                    d.dim.max(1) as usize
                                } else {
                                    1
                                }
                        }
                        // A named constant (ILE RPG Reference, %SIZE): a
                        // character constant's length; a numeric one's
                        // size as packed decimal of its digits.
                        Some(Sym::Const(Value::Num(d))) => {
                            let digits = (d.int_digits() + d.scale() as u32).max(1);
                            digits as usize / 2 + 1
                        }
                        Some(Sym::Const(c)) => c.chars()?.len(),
                        _ => bail!("%SIZE of {a0:?}"),
                    },
                    _ => self.eval(a0)?.chars()?.len(),
                };
                Value::Num(Dec::from_i64(n as i64))
            }
            "ELEM" => match args.first() {
                Some(Expr::Name { parts, .. }) => match self.sym_static(&parts.join(".")) {
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
                    Value::Char(c) => char_number(&a(&c))?,
                    o => o.num()?,
                };
                Value::Num(d.rescale(0, b == "INTH")?)
            }
            "DEC" | "DECH" => {
                let v = arg(self, 0)?;
                let d = match v {
                    Value::Char(c) => char_number(&a(&c))?,
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
            // %ABS of a float is a float (ILE RPG Reference, %ABS).
            "ABS" => match arg(self, 0)? {
                Value::Float(f) => Value::Float(f.abs()),
                v => Value::Num(v.num()?.abs()),
            },
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
                // The value's length and decimals: a variable's, or an
                // expression's (%EDITC(%DEC(i: 4: 0): 'X') is four digits:
                // PUB400 2026-10-28, tools/golden/verify13.txt t13_rows
                // "J0001").
                let (digits, decs) = match args.first() {
                    Some(x @ (Expr::Bif(..) | Expr::Bin(..) | Expr::Unary(..))) => {
                        match self.num_precision(x) {
                            Some(p) => p,
                            None => (v.int_digits() + v.scale() as u32, v.scale() as u32),
                        }
                    }
                    Some(x) => match self.type_of(x) {
                        Some(t) if t.is_numeric() => t.precision(),
                        _ => (v.int_digits() + v.scale() as u32, v.scale() as u32),
                    },
                    None => (v.int_digits() + v.scale() as u32, v.scale() as u32),
                };
                let mut out = edit_code(&v, digits, decs, code.chars().next().unwrap_or('Z'));
                // The third argument (ILE RPG Reference, %EDITC): *ASTFILL
                // replaces the leading blanks with asterisks; *CURSYM (the
                // currency symbol, '$' unless CURSYM says otherwise) or a
                // one-character literal floats left of the first digit,
                // the result one position longer.
                if let Some(third) = args.get(2) {
                    let special = match third {
                        Expr::Name { parts, .. } if parts.len() == 1 => Some(parts[0].as_str()),
                        _ => None,
                    };
                    let lead = out.len() - out.trim_start().len();
                    match special {
                        Some("*ASTFILL") => out = format!("{}{}", "*".repeat(lead), &out[lead..]),
                        Some("*CURSYM") => out = format!("{}${}", &out[..lead], &out[lead..]),
                        _ => {
                            let sym = a(&arg(self, 2)?.chars()?);
                            out = format!("{}{}{}", &out[..lead], sym.trim_end(), &out[lead..]);
                        }
                    }
                }
                Value::str(&out)
            }
            "ADDR" => {
                // %ADDR(name), %ADDR(array(i)), %ADDR(varying: *DATA).
                let Some(target) = args.first() else {
                    bail!("%ADDR needs a variable");
                };
                // An omitted parameter (*OMIT) has no storage: its
                // address is *NULL (ILE RPG Reference, OPTIONS(*OMIT)).
                if let Expr::Name { parts, args: None } = target {
                    if parts.len() == 1 && self.omitted(&parts[0].to_ascii_uppercase()) {
                        return Ok(Value::Fig(Fig::Null));
                    }
                }
                let l = self.loc_of(target)?;
                let data = args.get(1).is_some_and(
                    |a| matches!(a, Expr::Name { parts, .. } if parts.len() == 1 && parts[0] == "*DATA"),
                );
                let skip = match (data, l.dtype) {
                    (true, DType::Varchar(n)) => data::var_prefix(n),
                    (true, DType::VarUcs2(_)) => 2,
                    _ => 0,
                };
                self.ptr_to(l.buf, l.off + skip)
            }
            // %PADDR(name | 'name'): a procedure pointer (ILE RPG
            // Reference, %PADDR); a prototype's name means its EXTPROC.
            "PADDR" => {
                let n = match args.first() {
                    Some(Expr::Str(b)) => a(b).trim().to_ascii_uppercase(),
                    Some(Expr::Name { parts, args: None }) if parts.len() == 1 => {
                        let n = parts[0].to_ascii_uppercase();
                        match self.proto(&n).and_then(|p| p.extproc.clone()) {
                            Some(e) => e.to_ascii_uppercase(),
                            None => match self.sym(&n) {
                                Some(Sym::Const(v)) => a(&v.chars()?).trim().to_ascii_uppercase(),
                                _ => n,
                            },
                        }
                    }
                    _ => bail!("%PADDR needs a procedure name"),
                };
                // Where the procedure is: this module (in a service
                // program: that service program), or the service program
                // the program is bound to for it.
                // A procedure the module does not export: its module too
                // ("SRVPGM/MODULE"), a pointer reaches it all the same.
                // A procedure of a module bound into a program (not its
                // entry module): "*MODULE" and the module, so that another
                // module of the program reaches it through the pointer.
                let bound_module = self.io.program.strip_prefix("*BOUND/").map(str::to_string);
                let home = if let Some(pi) = self.prog.procs.get(&n) {
                    let exported = pi.export;
                    match self.current_srvpgm() {
                        Some((l, sp)) => Some(if exported {
                            (l, sp)
                        } else {
                            (l, format!("{sp}/{}", self.prog.name))
                        }),
                        None => match bound_module {
                            Some(m) => Some(("*MODULE".to_string(), m)),
                            // A program's entry module: the program and
                            // the activation group it is active in.
                            None if self.io.program.contains('/')
                                && !self.io.program.starts_with('*') =>
                            {
                                Some((
                                    "*PGM".to_string(),
                                    format!("{} {}", self.io.program, self.io.actgrp),
                                ))
                            }
                            None => None,
                        },
                    }
                } else {
                    match self.io.binding.as_ref().and_then(|b| b.imports.get(&n)) {
                        Some(crate::ile::Target::SrvPgm { library, name }) => {
                            Some((library.clone(), name.clone()))
                        }
                        Some(crate::ile::Target::Module(m)) => {
                            Some(("*MODULE".to_string(), m.clone()))
                        }
                        _ => None,
                    }
                };
                Value::Ptr(super::mem::Ptr {
                    kind: super::mem::KIND_PROC,
                    owner: 0,
                    buf: super::mem::proc_id(&n, home),
                    off: 0,
                })
            }
            "ALLOC" => {
                let n = arg(self, 0)?;
                self.alloc(&n)?
            }
            "REALLOC" => {
                let p = arg(self, 0)?;
                let n = arg(self, 1)?;
                self.realloc(&p, &n)?
            }
            "STR" => {
                let p = arg(self, 0)?;
                let max = match args.get(1) {
                    Some(m) => Some(self.eval(m)?.num()?.to_i128()? as usize),
                    None => None,
                };
                self.str_at(&p, max)?
            }
            "PARMS" => Value::Num(Dec::from_i64(match self.frames.last() {
                Some(f) => f.passed,
                None => self.parms_passed,
            } as i64)),
            // The string of the XML-SAX event being handled.
            "_SAXSTRING" => xmlsax::current_string(),
            other => match self.bif_more(other, args)? {
                Some(v) => v,
                None => bail!("%{other} is not supported yet"),
            },
        })
    }

    // ---- assignment -----------------------------------------------------

    fn assign(&mut self, target: &Expr, val: &Value, fit: Fit, right: bool) -> Result<()> {
        self.assign_value(target, val, fit, right)?;
        if let (Expr::Name { parts, args: None }, Value::Ptr(p)) = (target, val) {
            if matches!(p.kind, super::mem::KIND_STORAGE | super::mem::KIND_HEAP) {
                self.room_for_based(&parts.join("."), p);
            }
        }
        Ok(())
    }

    /// A basing pointer set: the storage it addresses made long enough
    /// for the longest item based on it. On IBM i a based item may run
    /// past the variable whose address was taken into the storage after
    /// it (iRPGUnit's ASSERT bases a CHAR(32766) at an offset in a
    /// CHAR(32766) list and uses its first bytes); here the variable's
    /// (or heap block's) storage grows (zeros), up to 16 MB.
    fn room_for_based(&mut self, name: &str, p: &super::mem::Ptr) {
        let mut need = 0usize;
        if let Some(f) = self.frames.last() {
            for (b, ptr) in &f.proc.based {
                if ptr.eq_ignore_ascii_case(name) {
                    need = need.max(f.proc.frame.get(*b).map_or(0, |x| x.len()));
                }
            }
        }
        for (b, ptr) in &self.prog.based {
            if ptr.eq_ignore_ascii_case(name) {
                need = need.max(self.prog.globals.get(*b).map_or(0, |x| x.len()));
            }
        }
        if need == 0 {
            return;
        }
        let Ok((buf, off)) = self.addr(&Value::Ptr(*p), 0) else {
            return;
        };
        let want = off + need;
        // Heap storage too: a based structure longer than the block
        // (iRPGUnit's list of test events: a 5244-byte abstract event
        // over a smaller specific one) reads the heap after it on IBM i.
        if want > 16 * 1024 * 1024 {
            return;
        }
        if let Some(b) = self.bufs.get_mut(buf) {
            if b.len() < want {
                b.resize(want, 0);
            }
        }
    }

    fn assign_value(&mut self, target: &Expr, val: &Value, fit: Fit, right: bool) -> Result<()> {
        // A data structure array given all its elements at once (the
        // value of a procedure returning LIKEDS(ds) DIM(n)).
        if let (Some(d), Value::Char(b)) = (self.whole_ds_array(target), val) {
            if b.len() == d.size * d.dim as usize {
                self.bufs[d.buf][d.off..d.off + b.len()].copy_from_slice(b);
                return Ok(());
            }
        }
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
            // %LEN(varying) = n: its current length (ILE RPG Reference,
            // %LEN used for its result: the characters past the old
            // length are what the storage has).
            Expr::Bif(b, args) if b == "LEN" && args.len() == 1 => {
                let l = self.loc_of(&args[0])?;
                let max = match l.dtype {
                    DType::Varchar(n) | DType::VarUcs2(n) => i128::from(n),
                    _ => {
                        bail!("%LEN as the target of an assignment needs a varying-length variable")
                    }
                };
                let n = val.num()?.to_i128()?;
                if !(0..=max).contains(&n) {
                    bail!("RNQ0100: The length {n} is out of range (0 to {max}) for the varying-length variable.");
                }
                self.bufs[l.buf][l.off..l.off + 2].copy_from_slice(&(n as u16).to_be_bytes());
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
            Expr::Bif(b, args) if b == "STR" => {
                // %STR(ptr: max) = value.
                let (Some(p), Some(m)) = (args.first(), args.get(1)) else {
                    bail!("%STR as a target needs a pointer and a maximum length");
                };
                let p = self.eval(p)?;
                let max = self.eval(m)?.num()?.to_i128()? as usize;
                self.store_str(&p, max, val)?;
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

    /// The service program (library, name) whose module is running, if
    /// one is.
    pub(super) fn current_srvpgm(&self) -> Option<(String, String)> {
        let u = self.io.unit.as_ref()?;
        let mut it = u.split('/');
        Some((it.next()?.to_string(), it.next()?.to_string()))
    }

    /// The file a program file is over: the one opened (EXTFILE may
    /// name another), or the one it was compiled with.
    fn fref_of(&self, i: usize) -> Result<crate::catalog::FileRef> {
        self.files[i]
            .open
            .as_ref()
            .map(|o| o.file.clone())
            .or_else(|| self.prog.files[i].fref.clone())
            .ok_or_else(|| {
                anyhow!(
                    "CPF4101: File {} is not open.",
                    self.prog.files[i].decl.name
                )
            })
    }

    fn open_file(&mut self, i: usize) -> Result<()> {
        self.open_file_inner(i)?;
        self.infds_open(i);
        self.infds_update(i);
        Ok(())
    }

    fn open_file_inner(&mut self, i: usize) -> Result<()> {
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
        // A program-described file not found when the program was
        // compiled: found now, by EXTFILE or by its name.
        let unresolved = fi.fref.is_none() && fi.decl.device == "DISK";
        let mut fref = match fi.fref.clone() {
            Some(f) => f,
            None if unresolved => {
                let has_extfile = crate::rpg::ast::keyword(&fi.decl.keywords, "EXTFILE")
                    .and_then(|k| k.args.first())
                    .is_some_and(|a| !a.trim().eq_ignore_ascii_case("*EXTDESC"));
                let n = fi.decl.name.clone();
                match self.block(crate::catalog::resolve_record_file(
                    self.io.client,
                    &self.io.libl,
                    &n,
                )) {
                    Ok(f) => f,
                    Err(_) if has_extfile => crate::catalog::FileRef {
                        library: String::new(),
                        file: crate::dds::PhysicalFile {
                            name: n,
                            format: String::new(),
                            text: String::new(),
                            unique: false,
                            fields: vec![],
                            keys: vec![],
                            descending: vec![],
                            view: None,
                            source: None,
                        },
                    },
                    Err(_) => bail!("CPF4101: File {n} not found."),
                }
            }
            None => return Ok(()),
        };
        // EXTFILE: the file opened is the one it names when the file is
        // opened (a literal, or a variable's value; *EXTDESC: the one
        // EXTDESC names) (ILE RPG Reference, EXTFILE).
        let fi = &self.prog.files[i];
        if let Some(k) = crate::rpg::ast::keyword(&fi.decl.keywords, "EXTFILE") {
            let arg = k
                .args
                .first()
                .map(|a| a.trim().to_string())
                .unwrap_or_default();
            let name = if arg.eq_ignore_ascii_case("*EXTDESC") {
                None
            } else if let Some(lit) = arg.strip_prefix('\'') {
                Some(lit.trim_end_matches('\'').to_string())
            } else {
                let v = self.eval(&Expr::name(&arg.to_ascii_uppercase()))?;
                Some(a(&v.chars()?))
            };
            if let Some(n) = name {
                let n = n.trim().to_ascii_uppercase();
                let n = n.strip_prefix("*LIBL/").unwrap_or(&n).to_string();
                fref = self
                    .block(crate::catalog::resolve_record_file(
                        self.io.client,
                        &self.io.libl,
                        &n,
                    ))
                    .map_err(|_| anyhow!("CPF4101: File {n} not found."))?;
            }
        }
        let fi = &self.prog.files[i];
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
        // The COMMIT keyword (with a condition: when it is on at open):
        // the file is opened under the commitment definition of the
        // program's activation group or job (ILE RPG Reference, COMMIT
        // keyword); none: the open fails (CPF4326).
        let commit = match crate::rpg::ast::keyword(&fi.decl.keywords, "COMMIT") {
            None => false,
            Some(k) => match k.args.first() {
                None => true,
                Some(a) => {
                    let name = a.clone();
                    let v = self.eval(&Expr::name(&name))?;
                    matches!(v, Value::Ind(true))
                        || v.chars().map(|c| c.first() == Some(&0xF1)).unwrap_or(false)
                }
            },
        };
        let fi = &self.prog.files[i];
        self.files[i].cmt = None;
        if commit {
            match crate::cmtctl::find(&self.io.job_key, &self.io.actgrp) {
                Some(d) => self.files[i].cmt = Some(d),
                None => bail!(
                    "CPF4326: File {} cannot be opened under commitment control: no commitment definition is active.",
                    fi.decl.name
                ),
            }
        }
        // A source file: the member (EXTMBR, else *FIRST) is opened as
        // records (source.rs).
        if fref.file.source.is_some() {
            let member = match crate::rpg::ast::keyword(&fi.decl.keywords, "EXTMBR")
                .and_then(|k| k.args.first().cloned())
            {
                Some(arg) => {
                    let arg = arg.trim().to_string();
                    if let Some(lit) = arg.strip_prefix('\'') {
                        Some(lit.trim_end_matches('\'').to_string())
                    } else if arg.starts_with('*') {
                        Some(arg)
                    } else {
                        let v = self.eval(&Expr::name(&arg.to_ascii_uppercase()))?;
                        Some(a(&v.chars()?).trim().to_string())
                    }
                }
                None => None,
            };
            let m = self.block(crate::source::open_member(
                self.io.client,
                &fref.library,
                &fref.file.name,
                member.as_deref(),
            ))?;
            fref.file.source = Some(m);
            let mut open = OpenFile::new(fref.clone());
            if update {
                open = open.for_update(format!("{}/{}", fref.library, fref.file.name));
            }
            self.files[i].open = Some(open);
            return Ok(());
        }
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

    /// A file is closed: its record lock goes; a source member's records
    /// are its lines again.
    pub(super) fn close_open_file(&mut self, mut f: OpenFile) -> Result<()> {
        let client = self.io.client;
        let _ = self.io.rt.block_on(f.release(client));
        if let Some(m) = f.file.file.source.clone().filter(|m| !m.is_empty()) {
            self.block(crate::source::close_member(
                client,
                &f.file.library,
                &f.file.file.name,
                &m,
            ))?;
        }
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

    fn file_op_inner(
        &mut self,
        op: FileOp,
        file: &str,
        key: &Option<Vec<Expr>>,
        ext: &Ext,
        ind: &ResultInd,
    ) -> Result<()> {
        let i = self
            .file_idx(file)
            .ok_or_else(|| anyhow!("{file} is not a file of this program"))?;
        let device = self.prog.files[i].decl.device.clone();
        if device == "WORKSTN" {
            return self.workstn_op(op, i, file, key, ind);
        }
        if device == "PRINTER" {
            return self.printer_op(op, i, file);
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
        let cmt = self.files[i].cmt.clone();
        let client: &tokio_postgres::Client = match &cmt {
            Some(d) => &d.client,
            None => self.io.data.unwrap_or(self.io.client),
        };
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
            if matches!(op, FileOp::Open | FileOp::Close) {
                return Ok(None);
            }
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
                self.file_failed(i, &err);
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, true);
                }
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
                    ..Status::default()
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
                            self.file_failed(i, &e);
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
                        ..Status::default()
                    }
                } else {
                    Status {
                        found: r.record.is_some(),
                        eof: r.eof,
                        equal: false,
                        error: false,
                        ..Status::default()
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
                let fref = self.fref_of(i)?;
                self.block(crate::authority::check(
                    self.io.client,
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
                let r = self.record_change(i, op, &rec, &keys);
                match r {
                    Ok(()) => {
                        if let Some(d) = &cmt {
                            d.changed();
                        }
                        self.last = Status {
                            found: true,
                            ..Status::default()
                        };
                    }
                    Err(err) if ext.error || ind.lo.is_some() => {
                        self.file_failed(i, &err);
                        if let Some(lo) = &ind.lo {
                            self.set_ind(lo, true);
                        }
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
                if let Some(f) = self.files[i].open.take() {
                    self.close_open_file(f)?;
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
        let name = match name.strip_prefix("*VAR:") {
            Some(v) => {
                let n = self.eval(&Expr::name(v))?.chars()?;
                a(&n).trim().to_ascii_uppercase()
            }
            None => name,
        };
        if name == "*LDA" {
            return self.local_data_area(input, field);
        }
        if name == "*GDA" || name == "*PDA" {
            // A group data area exists only in a group job, a program
            // initialization parameter data area only in a prestart job
            // (status 00402 for *PDA).
            bail!("RNX0402: Data area {name} is not available: this job is not a group or prestart job.");
        }
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

    /// Data areas this program holds locked go free (it ended in error).
    fn release_dtaaras(&mut self) {
        for (lib, name) in std::mem::take(&mut self.dtaara_locks) {
            let libl = vec![lib.clone()];
            let client = self.io.client;
            if let Ok(d) = self.block(crate::dtaara::resolve(client, &name, &libl, &lib)) {
                let _ = self.block(crate::dtaara::unlock(client, &d));
            }
        }
    }

    /// IN and OUT of the local data area: its bytes into the field or data
    /// structure (as long as it is), or the field's bytes over the start
    /// of the local data area.
    fn local_data_area(&mut self, input: bool, field: &str) -> Result<()> {
        let l = self.loc_of(&Expr::name(field))?;
        let job = self.io.job.as_ref().map(|j| j.number);
        let client = self.io.client;
        let mut lda = self.block(crate::dtaara::lda_read(client, job))?;
        let n = l.size.min(lda.len());
        if input {
            self.bufs[l.buf][l.off..l.off + n].copy_from_slice(&lda[..n]);
        } else {
            lda[..n].copy_from_slice(&self.bufs[l.buf][l.off..l.off + n]);
            self.block(crate::dtaara::lda_write(client, job, &lda))?;
        }
        Ok(())
    }

    /// Data area data structures (U in position 23, DTAARA(*AUTO)): read,
    /// and locked when they are data area objects, as the program starts
    /// (ILE RPG Reference, "Detailed RPG IV Object Program Cycle", step 3);
    /// written and unlocked when it ends with LR (steps 35-36).
    /// INZ(*USER): the job's user profile.
    pub(crate) fn user_inz(&mut self) -> Result<()> {
        let user = Value::str(&self.io.user);
        for f in &self.prog.user_inz {
            self.assign(
                &Expr::name(f),
                &user,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        }
        let prog = self.prog;
        self.ptr_inz(&prog.ptr_inz)
    }

    /// Pointers with INZ(%ADDR(x)): their addresses.
    fn ptr_inz(&mut self, list: &[(String, Expr)]) -> Result<()> {
        for (n, e) in list {
            let v = self.eval(e)?;
            self.assign(
                &Expr::name(n),
                &v,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    pub(super) fn auto_dtaaras(&mut self, start: bool) -> Result<()> {
        let prog = self.prog;
        for (ds, name) in &prog.auto_dtaaras {
            if name == "*LDA" {
                self.local_data_area(start, ds)?;
            } else {
                self.data_area(start, start, ds)?;
            }
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

    /// Show a frame on the job's display and take the operator's input:
    /// the fields of the format, response indicators, the key pressed.
    fn ws_interact(
        &mut self,
        i: usize,
        d: &Display,
        file: &std::sync::Arc<crate::dspf::DisplayFile>,
        format: &str,
        frame: crate::workstn::Frame,
    ) -> Result<()> {
        let Some(display) = self.io.display.clone() else {
            bail!(
                "Display file {} cannot be used here: this job has no display (it is not interactive).",
                d.file.name
            );
        };
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
        self.ws.apply(file, format, &resp);
        self.files[i].aid = Some(resp.aid.clone());
        self.ws_fill(d, format, &resp.values)?;
        for (n, v) in &resp.indicators {
            self.ws_set_ind(d, *n, *v);
        }
        // *INKA-*INKY: on for the function key pressed, off for the others
        // (ILE RPG Reference, function key indicators).
        for (k, letter) in FKEYS.iter().enumerate() {
            let on = resp.aid == format!("F{}", k + 1);
            self.set_ind(&format!("K{}", *letter as char), on);
        }
        Ok(())
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
    /// WRITE of a message subfile's control record with SFLINZ (DDS for
    /// display files, SFLMSGRCD, SFLPGMQ, SFLINZ): the subfile is the
    /// messages on the program message queue its SFLPGMQ field names.
    fn message_subfile(
        &self,
        file: &crate::dspf::DisplayFile,
        format: &str,
        mut values: BTreeMap<String, String>,
        inds: &[bool; 100],
    ) -> BTreeMap<String, String> {
        use crate::dspf::Keyword;
        let Some(ctl) = file.record(format) else {
            return values;
        };
        let Some(sfl) = ctl.subfile_control().and_then(|s| file.record(s)) else {
            return values;
        };
        let Some(line) = sfl.message_line() else {
            return values;
        };
        if !ctl.active(inds, |k| *k == Keyword::SflInz) {
            return values;
        }
        let queue = ctl
            .pgmq_field()
            .and_then(|f| values.get(&f.name))
            .map(|q| q.trim().to_string())
            .unwrap_or_default();
        let queue = if queue.is_empty() || queue == "*" {
            self.prog.name.clone()
        } else {
            queue
        };
        let msgs = crate::pgmmsg::named_messages(&self.io.job_key, &queue);
        let pag = ctl.sflpag() as usize;
        values.insert(crate::dspf::MSG_LINE.into(), line.to_string());
        for (i, m) in msgs.iter().take(pag).enumerate() {
            values.insert(format!("{}{i}", crate::dspf::MSG_TEXT), m.text.clone());
        }
        if msgs.len() > pag {
            values.insert(crate::dspf::MSG_MORE.into(), "1".into());
        }
        values
    }

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
        // READ: the record on the display (named, or by the file's name
        // the one with input fields written last), the operator's input
        // into its fields (ILE RPG Reference, READ of a WORKSTN file).
        if op == FileOp::Read {
            let named = d.fields(name).is_some().then_some(name);
            let file = d.file.clone();
            let (frame, format) = self.ws.read_frame(&file, named)?;
            self.ws_interact(i, d, &file, &format, frame)?;
            let st = Status {
                found: true,
                ..Status::default()
            };
            self.files[i].status = st.clone();
            self.last = st;
            if let Some(eq) = &ind.eq {
                self.set_ind(eq, false);
            }
            return Ok(());
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
                    None => {
                        let values = self.message_subfile(&file, &format, values, &inds);
                        self.ws.write(&file, &format, values, inds)?
                    }
                }
                set_status(self, Status::default());
            }
            FileOp::Update => {
                let values = self.ws_values(d, &format)?;
                self.ws.sfl_update(&file, &format, values, inds)?;
                set_status(self, Status::default());
            }
            FileOp::Exfmt => {
                if self.io.display.is_none() {
                    bail!(
                        "Display file {} cannot be used here: this job has no display (it is not interactive).",
                        d.file.name
                    );
                }
                let values = self.ws_values(d, &format)?;
                let frame = self.ws.frame(&file, &format, values, inds)?;
                self.ws_interact(i, d, &file, &format, frame)?;
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

    /// A statement; a pointer that addressed nothing while it ran is its
    /// error (status 00222).
    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        let r = self.stmt_inner(s);
        match (r, self.take_fault()) {
            (Err(e), _) if errors::is_signal(&e) => Err(e),
            (_, Some(f)) => Err(anyhow!(f)),
            (r, None) => r,
        }
    }

    /// The pointer fault of the statement running, if any.
    fn take_fault(&self) -> Option<String> {
        self.ptr_fault.borrow_mut().take()
    }

    fn fault(&self, text: String) {
        let mut f = self.ptr_fault.borrow_mut();
        if f.is_none() {
            *f = Some(text);
        }
    }

    fn stmt_inner(&mut self, s: &Stmt) -> Result<()> {
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
                    // A date or time shows in its format.
                    Some(m)
                        if matches!(
                            self.type_of(m),
                            Some(DType::Date(_)) | Some(DType::Time(_))
                        ) =>
                    {
                        let v = self.bif("CHAR", std::slice::from_ref(m))?;
                        self.text_of(&v)?
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
                fmt,
                from,
                to,
                ind,
            } => {
                if !self.move_temporal(*left, fmt.as_deref(), from, to)? {
                    self.do_move(*left, *pad, from, to, ind)?
                }
            }
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
            Stmt::Reset(Expr::Name { parts, args: None })
                if self.reset_to_initial(&parts.join("."))? => {}
            Stmt::Clear(t) | Stmt::Reset(t) => {
                // CLEAR: blanks, zeros, '0' by type. (RESET of what has no
                // initial image here, e.g. based storage: as CLEAR.)
                match t {
                    Expr::Name { parts, args: None } => match self.sym(&parts.join(".")) {
                        Some(Sym::Var(v)) => {
                            for k in 0..v.dim.max(1) as usize {
                                let at = v.off + k * v.stride;
                                self.bufs[v.buf][at..at + v.dtype.size()]
                                    .copy_from_slice(&v.dtype.initial());
                            }
                        }
                        Some(Sym::Ds(d)) => self.clear_ds(&parts.join("."), &d),
                        _ => bail!("CLEAR of {} is not supported", parts.join(".")),
                    },
                    // One element of a data structure array: its subfields
                    // by type, as for the whole data structure.
                    Expr::Name {
                        parts,
                        args: args @ Some(_),
                    } if matches!(self.sym(&parts.join(".")), Some(Sym::Ds(_))) => {
                        let name = parts.join(".");
                        let Some(Sym::Ds(d)) = self.sym(&name) else {
                            unreachable!()
                        };
                        let at = self.ds_element(&d, args)?;
                        let img = self.ds_clear_image(&name, &d);
                        self.bufs[d.buf][at..at + d.size].copy_from_slice(&img);
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
                ext,
                ind,
            } => {
                if !(self.storage_op(code, f2, result, ext)?
                    || self.string_op(code, f1, f2, result, ext, ind)?
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

    /// MOVE and MOVEL with a date, time or timestamp on either side (ILE
    /// RPG Reference, "Moving Date-Time Data"): false when neither is one.
    fn move_temporal(
        &mut self,
        left: bool,
        fmt: Option<&str>,
        from: &Expr,
        to: &Expr,
    ) -> Result<bool> {
        use crate::rpg::dates::{self, DateFmt, TimeFmt};
        enum K {
            D(DateFmt),
            T(TimeFmt),
            Z,
        }
        let kind = |t: Option<DType>| match t {
            Some(DType::Date(f)) => Some(K::D(f)),
            Some(DType::Time(f)) => Some(K::T(f)),
            Some(DType::Timestamp(_)) => Some(K::Z),
            _ => None,
        };
        let st = self.type_of(from);
        let tt = self.type_of(to);
        let (sk, tk) = (kind(st), kind(tt));
        if sk.is_none() && tk.is_none() {
            return Ok(false);
        }
        let dft_d = self.prog.datfmt()?;
        let dft_t = self.prog.timfmt()?;
        let fit = Fit::Truncate { half_adjust: false };
        match (sk, tk) {
            // Date, time or timestamp to another.
            (Some(s), Some(t)) => {
                if fmt.is_some() {
                    bail!("MOVE between date, time or timestamp fields takes no factor 1");
                }
                let v = self.eval(from)?;
                let cur = self.eval(to)?;
                let iso = a(&v.chars()?);
                let now = a(&cur.chars()?);
                let out = match (s, t) {
                    (K::D(_), K::D(_)) | (K::T(_), K::T(_)) | (K::Z, K::Z) => iso,
                    // The timestamp keeps its time (date) part.
                    (K::D(_), K::Z) => format!("{}{}", iso, &now[10..]),
                    (K::T(_), K::Z) => format!("{}-{}.000000", &now[..10], iso),
                    (K::Z, K::D(_)) => iso[..10].to_string(),
                    (K::Z, K::T(_)) => iso[11..19].to_string(),
                    _ => bail!("MOVE between a date and a time is not valid"),
                };
                self.assign(to, &Value::str(&out), fit, false)?;
            }
            // Date, time or timestamp to character or numeric: in factor
            // 1's format, else the field's.
            (Some(s), None) => {
                let tt = tt.ok_or_else(|| anyhow!("MOVE needs a field as the result"))?;
                let v = self.eval(from)?;
                let iso = a(&v.chars()?);
                let text = match s {
                    K::D(f) => {
                        let f = match fmt {
                            Some(x) => DateFmt::parse(x)?,
                            None => f.or(dft_d),
                        };
                        dates::format_date(dates::parse_date(&iso, &DateFmt::iso())?, &f)
                    }
                    K::T(f) => {
                        let f = match fmt {
                            Some(x) => TimeFmt::parse(x)?,
                            None => f.or(dft_t),
                        };
                        dates::format_time(dates::parse_time(&iso, &TimeFmt::iso())?, &f)
                    }
                    K::Z => iso,
                };
                if tt.is_numeric() {
                    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
                    let (n, _) = tt.precision();
                    let n = n as usize;
                    let take = if digits.len() > n {
                        if left {
                            &digits[..n]
                        } else {
                            &digits[digits.len() - n..]
                        }
                    } else {
                        &digits[..]
                    };
                    let mut cur = self.move_image(to)?;
                    let k = take.len();
                    let bytes = data::e(take);
                    if left {
                        cur[..k].copy_from_slice(&bytes);
                    } else {
                        let l = cur.len();
                        cur[l - k..].copy_from_slice(&bytes);
                    }
                    let (_, decs) = tt.precision();
                    let d = data::decode_num(&DType::Zoned(cur.len() as u32, decs), &cur)?;
                    self.assign(to, &Value::Num(d), fit, false)?;
                } else {
                    let mut cur = self.eval(to)?.chars()?;
                    let src = data::e(&text);
                    let n = cur.len();
                    let k = src.len().min(n);
                    if left {
                        cur[..k].copy_from_slice(&src[..k]);
                    } else {
                        cur[n - k..].copy_from_slice(&src[src.len() - k..]);
                    }
                    self.assign(to, &Value::Char(cur), fit, false)?;
                }
            }
            // Character or numeric to a date, time or timestamp: factor
            // 1's format (else the field's) says how many characters, the
            // rightmost (MOVE) or leftmost (MOVEL).
            (None, Some(t)) => {
                let numeric =
                    st.is_some_and(|t| t.is_numeric()) || matches!(self.eval(from)?, Value::Num(_));
                let src = if numeric {
                    let b = self.move_image(from)?;
                    b.iter()
                        .map(|x| char::from(b'0' + (x & 0x0F).min(9)))
                        .collect::<String>()
                } else {
                    a(&self.eval(from)?.chars()?)
                };
                let pick = |n: usize| -> String {
                    let c: Vec<char> = src.chars().collect();
                    if c.len() <= n {
                        src.clone()
                    } else if left {
                        c[..n].iter().collect()
                    } else {
                        c[c.len() - n..].iter().collect()
                    }
                };
                let out = match t {
                    K::D(f) => {
                        let mut f = match fmt {
                            Some(x) => DateFmt::parse(x)?,
                            None => f.or(dft_d),
                        };
                        if numeric {
                            f.sep = None;
                        }
                        let d = dates::parse_date(&pick(f.len()), &f)?;
                        dates::format_date(d, &DateFmt::iso())
                    }
                    K::T(f) => {
                        let mut f = match fmt {
                            Some(x) => TimeFmt::parse(x)?,
                            None => f.or(dft_t),
                        };
                        if numeric {
                            if f.name == "*USA" {
                                bail!("MOVE between a time and a numeric field cannot use *USA");
                            }
                            f.sep = None;
                        }
                        let n = if f.sep.is_some() { 8 } else { 6 };
                        let h = dates::parse_time(&pick(n), &f)?;
                        dates::format_time(h, &TimeFmt::iso())
                    }
                    K::Z => {
                        let n = if numeric { 20 } else { 26 };
                        dates::format_ts(&dates::parse_ts(&pick(n))?, false)
                    }
                };
                self.assign(to, &Value::str(&out), fit, false)?;
            }
            (None, None) => unreachable!(),
        }
        Ok(true)
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
            // COMMIT and ROLBK: the pending changes of the commitment
            // definition the program uses (ILE RPG Reference); none:
            // status 00802.
            "COMMIT" | "ROLBK" => {
                let Some(d) = crate::cmtctl::find(&self.io.job_key, &self.io.actgrp) else {
                    bail!("RNQ0802: Commitment control is not active.");
                };
                let rt = self.io.rt.clone();
                let r = if code == "COMMIT" {
                    rt.block_on(d.commit())
                } else {
                    rt.block_on(d.rollback())
                };
                r.map_err(|e| {
                    anyhow!(
                        "RNQ080{}: {} failed: {e}",
                        if code == "COMMIT" { 4 } else { 5 },
                        code
                    )
                })?;
            }
            "DEFINE" | "FORCE" | "DUMP" => {}
            // CALLP(E): the call, its errors taken by the extender.
            "CALLP" => {
                if let Some(x) = f2 {
                    self.stmt(&Stmt::CallP(x.clone()))?;
                }
            }
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
            "XML-INTO" | "DATA-INTO" => {
                let target = need(f1, "a receiver")?;
                let Some(Expr::Bif(_, parts)) = f2 else {
                    bail!("{code}: %XML or %DATA is needed");
                };
                let arg = |n: &str| {
                    parts.iter().find_map(|p| match p {
                        Expr::Bif(b, a) if b == n => Some(a.clone()),
                        _ => None,
                    })
                };
                if code == "XML-INTO" {
                    if matches!(&target, Expr::Bif(b, _) if b == "HANDLER") {
                        bail!("XML-INTO %HANDLER is not supported yet");
                    }
                    self.xml_into(&target, &arg("XML").unwrap_or_default())?;
                } else {
                    self.data_into(&target, &arg("DATA").unwrap_or_default(), &arg("PARSER"))?;
                }
            }
            "XML-SAX" => {
                let handler = need(f1, "%HANDLER")?;
                let Some(Expr::Bif(_, parts)) = f2 else {
                    bail!("XML-SAX: %XML is needed");
                };
                let xml = parts
                    .iter()
                    .find_map(|p| match p {
                        Expr::Bif(b, a) if b == "XML" => Some(a.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                self.xml_sax(&handler, &xml)?;
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
        self.run_code_at(code, 0)
    }

    /// Run code from an instruction on (after a GOTO from an error
    /// subroutine).
    fn run_code_at(&mut self, code: &Code, start: usize) -> Result<Flow> {
        let mut pc = start;
        // Active MONITOR groups: (handlers, end of body, end).
        let mut monitors: Vec<MonitorGroup> = Vec::new();
        while pc < code.len() {
            let i = &code[pc];
            if !i.cond.is_empty() && !self.conditions_met(&i.cond) {
                pc += 1;
                continue;
            }
            self.line = i.line;
            let step: Result<Option<usize>> = (|| -> Result<Option<usize>> {
                Ok(match &i.ins {
                    Ins::Stmt(s) => {
                        self.stmt_e(s)?;
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
                        let saved = std::mem::replace(&mut self.routine, name.clone());
                        let r = self.run_code(sub);
                        self.routine = saved;
                        self.line = i.line;
                        match r? {
                            Flow::Return(v) => return Err(ReturnSignal(v).into()),
                            _ => None,
                        }
                    }
                    Ins::Return(v) => {
                        let val = match v {
                            // A data structure array (a procedure returning
                            // LIKEDS(ds) DIM(n)): all its elements.
                            Some(x) if self.whole_ds_array(x).is_some() => {
                                let d = self.whole_ds_array(x).unwrap();
                                let n = d.size * d.dim as usize;
                                Some(Value::Char(self.bufs[d.buf][d.off..d.off + n].to_vec()))
                            }
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
                        out,
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
                        if hit {
                            if let Some(t) = out {
                                return Err(GotoSignal(t.clone()).into());
                            }
                        }
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
                    Ins::Nop | Ins::Label(_) => None,
                    Ins::GotoOut(t) => return Err(GotoSignal(t.clone()).into()),
                    Ins::EndSr(p) => {
                        // The return point, then the field is blank again
                        // (ILE RPG Reference, INFSR: "after the ENDSR
                        // operation, factor 2 is reset to blanks").
                        let point = a(&self.eval(p)?.chars()?).trim().to_ascii_uppercase();
                        if matches!(p, Expr::Name { .. }) {
                            self.assign(
                                p,
                                &Value::str(""),
                                Fit::Truncate { half_adjust: false },
                                false,
                            )?;
                        }
                        if !point.is_empty() {
                            return Err(ReturnPointSignal(point).into());
                        }
                        None
                    }
                })
            })();
            // A pointer fault in a condition (IF, DOW...).
            let step = match (step, self.take_fault()) {
                (Err(e), _) if errors::is_signal(&e) => Err(e),
                (_, Some(f)) => Err(anyhow!(f)),
                (s, None) => s,
            };
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
                    // GOTO from a subroutine to a TAG here.
                    if let Some(GotoSignal(t)) = err.downcast_ref::<GotoSignal>() {
                        let to = code
                            .iter()
                            .position(|x| matches!(&x.ins, Ins::Label(l) if l == t));
                        if let Some(to) = to {
                            monitors.retain(|m| to < m.1);
                            pc = to;
                            continue;
                        }
                        return Err(err);
                    }
                    if err.downcast_ref::<ReturnPointSignal>().is_some() {
                        return Err(err);
                    }
                    // An escape message sent to a caller passes this
                    // procedure by.
                    if self.passes_by(&err) {
                        return Err(err);
                    }
                    // An error inside a monitored body goes to the first
                    // ON-ERROR that lists its status (or *ALL / nothing).
                    let mut handled = None;
                    let code = errors::status_code(&err);
                    let id = errors::message_id(&err);
                    while let Some((handlers, end_body, _)) = monitors.last().cloned() {
                        if pc < end_body {
                            handled = handlers
                                .iter()
                                .find(|(codes, _)| {
                                    codes.is_empty()
                                        || codes.iter().any(|c| {
                                            c.strip_prefix("MSG:").is_some_and(|m| m == id)
                                                || errors::on_error_takes(c, code)
                                        })
                                })
                                .map(|h| h.1);
                            break;
                        }
                        monitors.pop();
                    }
                    match handled {
                        Some(h) => {
                            self.note_error(&err);
                            pc = h;
                        }
                        None if i.line == 0 => return Err(err),
                        // Where the error happened, for its exception
                        // message's sender (shown as "line N").
                        None => {
                            let sender = errors::origin(&err)
                                .map(|a| a.sender)
                                .unwrap_or_else(|| self.sender());
                            return Err(err.context(errors::At {
                                line: i.line,
                                sender,
                            }));
                        }
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
            // Bytes of exactly the parameter's size (a command's
            // parameters for its processing program: a VARYING value with
            // its length first) are the parameter's storage as they are.
            if !t.is_numeric() {
                let raw = data::e(a);
                if raw.len() == t.size() && matches!(t, DType::Varchar(_) | DType::VarUcs2(_)) {
                    parms.push(raw);
                    continue;
                }
                // A caller's character variable longer than the parameter:
                // the program gets all of it (by reference: a pointer to
                // the parameter reaches the rest, as a stub that declares
                // CHAR(1) parameters and writes through their addresses).
                if raw.len() > t.size() && matches!(t, DType::Char(_)) {
                    parms.push(raw);
                    continue;
                }
            }
            let v = if t.is_numeric() {
                Value::Num(Dec::parse(a).map_err(|_| anyhow!("parameter '{a}' is not a number"))?)
            } else {
                Value::str(a)
            };
            let mut buf = t.initial();
            data::store(t, &v, &mut buf, Fit::Strict { half_adjust: false }, false)?;
            parms.push(buf);
        }
        let r = self.run_bytes(&mut parms);
        // The parameters as the program left them: the caller's variables
        // get them back (parameters are passed by reference).
        self.returned = types
            .iter()
            .zip(&parms)
            .map(|(t, b)| match data::load(t, b) {
                Ok(Value::Char(_)) if b.len() > t.size() => a(b),
                Ok(Value::Num(d)) => d.to_string(),
                Ok(other) => other.chars().map(|c| a(&c)).unwrap_or_default(),
                Err(_) => String::new(),
            })
            .collect();
        r
    }

    /// Run the program with parameters passed by reference (a CALL from
    /// another program): each is the bytes of the caller's variable, and
    /// gets the parameter's value back when the program ends.
    pub fn run_bytes(&mut self, parms: &mut [Vec<u8>]) -> Result<()> {
        // The program's main procedure: a call stack entry (its call
        // message queue), for the message APIs.
        let pgm = self.stack_pgm();
        let main = self.prog.main.clone().unwrap_or_else(|| pgm.clone());
        let lib = self.stack_lib();
        let module = self.stack_module();
        let id = crate::pgmmsg::stack_push(
            &self.io.job_key,
            (&lib, &pgm),
            &module,
            &main,
            &self.io.actgrp,
        );
        // Below it, the program's entry procedure (for the message APIs).
        crate::pgmmsg::stack_mark_pep(&self.io.job_key, id);
        let r = self.run_bytes_inner(parms);
        crate::pgmmsg::stack_pop(&self.io.job_key, id);
        r
    }

    /// A data structure array named without an index.
    /// A qualified name ending in a whole data structure array (no index
    /// on its last part: `args`; `idx` indexes the parts before it).
    fn ds_array_path(&mut self, e: &Expr) -> bool {
        let Expr::Path {
            parts,
            idx,
            args: None,
        } = e
        else {
            return false;
        };
        matches!(self.path_sym(parts, idx), Ok(Sym::Ds(d)) if d.dim > 1)
    }

    fn whole_ds_array(&self, e: &Expr) -> Option<super::program::DsInfo> {
        let Expr::Name { parts, args: None } = e else {
            return None;
        };
        match self.sym(&parts.join(".")) {
            Some(Sym::Ds(d)) if d.dim > 1 => Some(d),
            _ => None,
        }
    }

    /// The library of the program or service program the running code
    /// is in.
    pub(super) fn stack_lib(&self) -> String {
        if let Some((lib, _)) = self.current_srvpgm() {
            return lib;
        }
        match self.io.program.split_once('/') {
            Some((l, _)) if !l.starts_with('*') => l.to_string(),
            _ => crate::pgmmsg::stack(&self.io.job_key)
                .last()
                .map(|e| e.lib.clone())
                .unwrap_or_default(),
        }
    }

    /// A procedure's name as the module exports it: EXTPROC of its
    /// prototype as written ('assert'), else the name in capitals.
    pub(super) fn external_proc_name(&self, name: &str) -> String {
        if let Some(w) = self
            .prog
            .procs
            .get(&name.to_ascii_uppercase())
            .and_then(|p| p.written.clone())
            .filter(|e| e.eq_ignore_ascii_case(name))
        {
            return w;
        }
        self.prog
            .protos
            .get(&name.to_ascii_uppercase())
            .and_then(|p| p.written.clone())
            .filter(|e| e.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| name.to_ascii_uppercase())
    }

    /// The program object (or service program) the running code is in,
    /// as the call stack names it.
    /// The module name a call stack entry of this module shows: a
    /// program's entry module by its own name (CRTPGM ENTMOD), which
    /// need not be the program's.
    fn stack_module(&self) -> String {
        if !self.io.program.starts_with("*BOUND/") && self.io.unit.is_none() {
            if let Some(m) = self
                .io
                .binding
                .as_ref()
                .map(|b| b.entry_module.clone())
                .filter(|m| !m.is_empty())
            {
                return m;
            }
        }
        self.prog.name.clone()
    }

    pub(super) fn stack_pgm(&self) -> String {
        if let Some((_, sp)) = self.current_srvpgm() {
            return sp;
        }
        let p = self.io.program.rsplit('/').next().unwrap_or("").to_string();
        if self.io.program.starts_with("*BOUND/") {
            // A module bound into a program: that program's.
            return crate::pgmmsg::stack_top_pgm(&self.io.job_key).unwrap_or(p);
        }
        p
    }

    /// A received message in format RCVM0100, RCVM0200 or RCVM0300 (Receive
    /// Program Message API): bytes returned and available, severity,
    /// message ID, type, key, then (RCVM0100) the replacement data, or
    /// (RCVM0300) the message file, the lengths of the replacement data,
    /// text, help and sender information, and those.
    fn rcvm_format(
        &self,
        format: &str,
        m: &crate::pgmmsg::QMsg,
        receiver: Option<&crate::pgmmsg::Entry>,
    ) -> Vec<u8> {
        let i4 = |n: usize| (n as i32).to_be_bytes();
        let field = |s: &str, n: usize| {
            let mut b = data::e(s);
            b.resize(n, 0x40);
            b
        };
        let mut b: Vec<u8> = Vec::new();
        b.extend([0u8; 8]);
        b.extend_from_slice(&m.sev.to_be_bytes());
        b.extend(field(&m.msgid, 7));
        b.extend(field(crate::pgmmsg::rtntype(&m.ty), 2));
        b.extend(crate::pgmmsg::key_bytes(m.key));
        let text = data::e(&m.text);
        let help = data::e(&m.seclvl);
        if format == "RCVM0100" {
            b.extend(field("", 7));
            b.extend_from_slice(&i4(0));
            b.extend_from_slice(&i4(37));
            b.extend_from_slice(&i4(m.data.len()));
            b.extend_from_slice(&i4(m.data.len()));
            b.extend_from_slice(&m.data);
        } else if format == "RCVM0200" {
            // RCVM0200: the message file, the sending job, program and
            // time, the receiving program, the CCSIDs, then the
            // replacement data, the text and the help.
            let job = self.io.job.clone().unwrap_or(crate::jobs::JobId {
                number: 0,
                user: self.io.user.clone(),
                name: String::new(),
            });
            let (date, time) = &m.sent;
            let rp = receiver
                .map(|r| r.pgm.clone())
                .unwrap_or_else(|| m.receiver.pgm.clone());
            b.extend(field(&m.msgf, 10));
            b.extend(field(&m.msgflib, 10));
            b.extend(field(&m.msgflib, 10));
            b.extend(field(&job.name, 10));
            b.extend(field(&job.user, 10));
            b.extend(field(&format!("{:06}", job.number), 6));
            b.extend(field(&m.sender.pgm, 12));
            b.extend(field("", 4));
            b.extend(field(date, 7));
            b.extend(field(time, 6));
            b.extend(field(&rp, 10));
            b.extend(field("", 4));
            b.extend(field("1", 1));
            b.extend(field("1", 1));
            b.extend(field("", 1));
            b.extend_from_slice(&i4(0));
            b.extend_from_slice(&i4(0));
            b.extend(field("*NO", 9));
            b.extend_from_slice(&i4(37));
            b.extend_from_slice(&i4(37));
            for v in [
                m.data.len(),
                m.data.len(),
                text.len(),
                text.len(),
                help.len(),
                help.len(),
            ] {
                b.extend_from_slice(&i4(v));
            }
            b.extend_from_slice(&m.data);
            b.extend_from_slice(&text);
            b.extend_from_slice(&help);
        } else {
            b.extend(field(&m.msgf, 10));
            b.extend(field(&m.msgflib, 10));
            b.extend(field(&m.msgflib, 10));
            b.extend(field("*NO", 9));
            for v in [0, 0, 37, 37] {
                b.extend_from_slice(&i4(v));
            }
            let (date, time) = &m.sent;
            let job = self.io.job.clone().unwrap_or(crate::jobs::JobId {
                number: 0,
                user: self.io.user.clone(),
                name: "QPADEV0001".into(),
            });
            let mut snd = Vec::new();
            snd.extend(field(&job.name, 10));
            snd.extend(field(&job.user, 10));
            snd.extend(field(&format!("{:06}", job.number), 6));
            snd.extend(field(date, 7));
            snd.extend(field(time, 6));
            snd.extend(field("1", 1));
            snd.extend(field("1", 1));
            snd.extend(field(&m.sender.pgm, 12));
            snd.extend(field(&m.sender.module, 10));
            snd.extend(field(&m.sender.proc, 256));
            snd.extend(field("", 1));
            // The statement: its source sequence number with two
            // decimals, left-adjusted (RPGUnit's PGMMSGT expects '16500'
            // for line 165).
            if m.sender.line > 0 {
                snd.extend_from_slice(&i4(1));
                snd.extend(field(&format!("{:<10}", m.sender.line * 100), 30));
            } else {
                snd.extend_from_slice(&i4(0));
                snd.extend(field("", 30));
            }
            // The entry's, or (the entry has ended) as noted when sent.
            let (rp, rm, rpr) = receiver
                .map(|r| (r.pgm.clone(), r.module.clone(), r.proc.clone()))
                .unwrap_or_else(|| {
                    (
                        m.receiver.pgm.clone(),
                        m.receiver.module.clone(),
                        m.receiver.proc.clone(),
                    )
                });
            snd.extend(field(&rp, 10));
            snd.extend(field(&rm, 10));
            snd.extend(field(&rpr, 256));
            snd.extend(field("", 10));
            snd.extend_from_slice(&i4(0));
            snd.extend(field("", 30));
            snd.extend(field("", 2));
            for v in [
                m.data.len(),
                m.data.len(),
                text.len(),
                text.len(),
                help.len(),
                help.len(),
                snd.len(),
                snd.len(),
            ] {
                b.extend_from_slice(&i4(v));
            }
            b.extend_from_slice(&m.data);
            b.extend_from_slice(&text);
            b.extend_from_slice(&help);
            b.extend_from_slice(&snd);
        }
        let n = b.len();
        b[0..4].copy_from_slice(&i4(n));
        b[4..8].copy_from_slice(&i4(n));
        b
    }

    /// The sender of a message this code sends: its call stack entry.
    /// The job's key (process state: call stack, job log).
    pub(super) fn io_job_key(&self) -> String {
        self.io.job_key.clone()
    }

    pub(super) fn sender(&self) -> crate::pgmmsg::Sender {
        let s = crate::pgmmsg::stack(&self.io.job_key);
        match s.last() {
            Some(e) => crate::pgmmsg::Sender {
                pgm: e.pgm.clone(),
                lib: crate::qtemp::logical(&e.lib),
                module: e.module.clone(),
                proc: e.proc.clone(),
                line: self.line,
                inst: String::new(),
            },
            None => crate::pgmmsg::Sender {
                pgm: self.stack_pgm(),
                lib: crate::qtemp::logical(&self.stack_lib()),
                module: self.prog.name.clone(),
                proc: self.prog.name.clone(),
                line: self.line,
                inst: String::new(),
            },
        }
    }

    fn run_bytes_inner(&mut self, parms: &mut [Vec<u8>]) -> Result<()> {
        // Still active in its activation group (it ended without LR): its
        // storage, indicators and files as it left them.
        let key = (
            self.io.job_key.clone(),
            self.io.actgrp.clone(),
            self.io.program.clone(),
        );
        let active = PROGRAMS
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|m| m.remove(&key)));
        match active {
            Some(a) if a.bufs.len() == self.prog.globals.len() => {
                // Its storage and heap as it left them; what callers lent
                // for this call stays.
                let lent = std::mem::take(&mut self.bufs.lent);
                let heap = std::mem::take(&mut self.bufs.heap);
                self.bufs = a.bufs;
                self.bufs.lent = lent;
                if self.heap_lent {
                    self.bufs.heap = heap;
                }
                self.inds = a.inds;
                for (f, o) in self.files.iter_mut().zip(a.files) {
                    f.open = o;
                }
                if let Ok(mut m) = self.io.modules.lock() {
                    m.extend(a.modules);
                }
                self.occur = a.occur;
                self.table_cur = a.table_cur;
                self.var_elems = a.var_elems;
                if let Ok(mut s) = self.io.shared.lock() {
                    s.extend(a.shared);
                }
                self.sql.restore(&key);
                self.reactivated = true;
                // RT or RETURN ended it: the next cycle starts afresh.
                self.set_ind("RT", false);
            }
            _ => {
                // Global files without USROPN open with the program; a
                // procedure's own files when it is called.
                for i in 0..self.prog.files.len() {
                    let d = &self.prog.files[i].decl;
                    if keyword(&d.keywords, "USROPN").is_none()
                        && keyword(&d.keywords, "TEMPLATE").is_none()
                        && d.proc.is_none()
                    {
                        self.open_file(i)?;
                    }
                }
            }
        }
        self.parms_passed = parms.len();
        self.psds_start();
        // Exported data: the bound modules' initial values, then this
        // module's view of it.
        if !self.reactivated {
            self.seed_bound_modules()?;
        }
        self.share_seed();
        self.share_in();
        let prog = self.prog;
        if !self.reactivated {
            self.user_inz()?;
            self.auto_dtaaras(true)?;
            // The external indicators U1-U8: the job switches when the
            // program starts (ILE RPG Reference, External Indicators).
            let sws = crate::jobattr::switches(&self.io.job_key, None);
            for (i, c) in sws.bytes().enumerate().take(8) {
                self.set_ind(&format!("U{}", i + 1), c == b'1');
            }
        }
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
                // A parameter is the caller's storage (by reference): a
                // parameter with storage of its own gets all the bytes
                // passed, so that a pointer from %ADDR reaches past its
                // declared length (a trigger program's buffer).
                let mut locs = Vec::new();
                for (p, bytes) in prog.entry.iter().zip(parms.iter()) {
                    let l = self.loc_of(&p.target)?;
                    let whole = l.off == 0
                        && bytes.len() > l.size
                        && self.bufs.get(l.buf).is_some_and(|b| {
                            // Storage of its own (rounded up to 16 bytes,
                            // with the slack after it).
                            let own = b.len().saturating_sub(super::mem::SLACK);
                            b.len() == l.size || own == l.size || own == l.size.div_ceil(16) * 16
                        });
                    if whole {
                        self.bufs[l.buf] = bytes.clone();
                    } else {
                        let n = l.size.min(bytes.len());
                        self.bufs[l.buf][l.off..l.off + n].copy_from_slice(&bytes[..n]);
                    }
                    locs.push((l, whole));
                }
                let r = self.cycle();
                for ((l, whole), bytes) in locs.iter().zip(parms.iter_mut()) {
                    if *whole {
                        let b = &mut self.bufs[l.buf];
                        let n = bytes.len().min(b.len());
                        bytes[..n].copy_from_slice(&b[..n]);
                        b.truncate(l.size);
                    } else {
                        let n = l.size.min(bytes.len());
                        bytes[..n].copy_from_slice(&self.bufs[l.buf][l.off..l.off + n]);
                    }
                }
                r
            }
        };
        // Data area data structures go back when the program ends with LR
        // (a linear main procedure: when it returns).
        let r = match r {
            Ok(()) if prog.main.is_some() || self.ind("LR") => self.auto_dtaaras(false),
            other => other,
        };
        // Ended normally with LR on: U1-U8 go back to the job switches
        // (ILE RPG Reference, External Indicators; RETURN without LR
        // leaves them).
        if r.is_ok() && prog.main.is_none() && self.ind("LR") {
            let sws: String = (1..=8)
                .map(|i| if self.ind(&format!("U{i}")) { '1' } else { '0' })
                .collect();
            crate::jobattr::set_switches(&self.io.job_key, &sws);
        }
        if r.is_err() {
            self.release_dtaaras();
        }
        self.share_out();
        let closed = self.close_printers();
        for f in self.files.iter_mut() {
            if let Some(o) = f.open.as_mut() {
                let _ = self.io.rt.block_on(o.release(self.io.client));
            }
        }
        // Ended without LR (a cycle program by RETURN or RT; a program
        // with a linear main procedure always): the program stays active
        // in its activation group, its files open, until the group ends.
        // An activation group *NEW ends with the program.
        // (A source run outside a library, by `as4tainr call` or the
        // tests, is not a program object: nothing stays active.)
        // What the program wrote to stream files still open is stored.
        self.c_flush();
        let r = match self.spaces_flush() {
            Ok(()) => r,
            Err(e) => r.and(Err(e)),
        };
        let keep = r.is_ok()
            && (prog.main.is_some() || !self.ind("LR"))
            && !self.io.actgrp.starts_with("*NEW")
            && !self.io.program.starts_with("*NONE/");
        if self.io.actgrp.starts_with("*NEW") {
            crate::ile::end_activations(
                &self.io.job_key,
                &crate::ile::Groups::Named(&self.io.actgrp),
            );
            let client = self.io.client;
            self.block(close_ended_spools(client));
        }
        if keep {
            let globals = self.prog.globals.len();
            let mut kept = super::mem::Mem::new(self.bufs.own[..globals].to_vec());
            // Heap storage lives as long as the activation (a shared
            // heap stays with the caller).
            if !self.heap_lent {
                kept.heap = std::mem::take(&mut self.bufs.heap);
            }
            let a = Active {
                bufs: kept,
                inds: self.inds.clone(),
                files: self.files.iter_mut().map(|f| f.open.take()).collect(),
                modules: self
                    .io
                    .modules
                    .lock()
                    .map(|m| m.clone())
                    .unwrap_or_default(),
                occur: self.occur.clone(),
                table_cur: self.table_cur.clone(),
                var_elems: self.var_elems.clone(),
                shared: self.io.shared.lock().map(|s| s.clone()).unwrap_or_default(),
            };
            self.sql.keep(key.clone());
            if let Ok(mut g) = PROGRAMS.lock() {
                g.get_or_insert_with(HashMap::new).insert(key, a);
            }
        } else {
            let opened: Vec<OpenFile> = self
                .files
                .iter_mut()
                .filter_map(|f| f.open.take())
                .collect();
            for f in opened {
                self.close_open_file(f)?;
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
            // QCAPCMD (command, length, options control block, its
            // length, CPOP0100, changed command, its length, length of
            // the changed command available, error code) (Process
            // Commands API): type of command processing 0 runs the
            // command as QCMDEXC does; the other types check it and
            // return it with a keyword for every value given
            // (cl::checked_command; the exact text IBM i returns is to
            // be checked against a real system, docs/fidelity-gaps.md).
            "QCAPCMD" => {
                let [cmd, len, opt, _, _, chg, chglen, chgavl, rest @ ..] = locs else {
                    bail!("QCAPCMD needs nine parameters");
                };
                let n = num(self, len)?.to_i128()?.max(0) as usize;
                let c: String = text(self, cmd).chars().take(n).collect();
                let ty = if opt.size >= 4 {
                    i32::from_be_bytes(self.bufs[opt.buf][opt.off..opt.off + 4].try_into().unwrap())
                } else {
                    0
                };
                let out = if ty == 0 {
                    self.run_cl(c.trim()).map(|_| c.trim().to_string())
                } else {
                    crate::cl::checked_command(c.trim())
                };
                let out = match out {
                    Ok(s) => s,
                    Err(e) => {
                        if std::env::var("AS4TAINR_TRACE_ERRORS").is_ok_and(|v| v == "1") {
                            eprintln!("as4tainr: QCAPCMD {}: {e:#}", c.trim());
                        }
                        return self.api_error(rest.first(), e).map(|_| true);
                    }
                };
                let want = (num(self, chglen)?.to_i128()?.max(0) as usize).min(chg.size);
                let mut b = data::e(&out);
                let avail = b.len();
                b.resize(want, 0x40);
                self.bufs[chg.buf][chg.off..chg.off + want].copy_from_slice(&b[..want]);
                if chgavl.size >= 4 {
                    self.bufs[chgavl.buf][chgavl.off..chgavl.off + 4]
                        .copy_from_slice(&(avail as i32).to_be_bytes());
                }
                self.api_ok(rest.first());
            }
            // QSPRILSP (receiver, its length, SPRL0100, error code)
            // (Retrieve Identity of Last Spooled File Created API): the
            // spooled file's name, job name, user and number, its number,
            // the system, and its creation date and time (now: the
            // spooled file was just opened). No spooled file yet: an
            // error without a message ID (IBM i's is to be checked).
            "QSPRILSP" => {
                let [rcv, len, fmt, rest @ ..] = locs else {
                    bail!("QSPRILSP needs 3 parameters and an error code");
                };
                let f = text(self, fmt).trim().to_ascii_uppercase();
                let r = if f != "SPRL0100" {
                    Err(anyhow!("CPF3C21: Format name {f} is not valid."))
                } else {
                    super::last_spooled_file(&self.io.job_key)
                        .ok_or_else(|| anyhow!("No spooled file was created by this job."))
                };
                let (file, jname, juser, jnbr, number) = match r {
                    Ok(x) => x,
                    Err(e) => return self.api_error(rest.first(), e).map(|_| true),
                };
                let fld = |s: &str, n: usize| {
                    let mut b = data::e(s);
                    b.resize(n, 0x40);
                    b.truncate(n);
                    b
                };
                let ((y, mo, d), (h, mi, sec), _) = crate::rpg::dates::now();
                let mut o = vec![0u8; 8];
                o.extend(fld(&file, 10));
                o.extend(fld(&jname, 10));
                o.extend(fld(&juser, 10));
                o.extend(fld(&jnbr, 6));
                o.extend(number.to_be_bytes());
                o.extend(fld(&crate::report::system_name(), 8));
                o.extend(fld(
                    &format!("{}{:02}{mo:02}{d:02}", u8::from(y >= 2000), y % 100),
                    7,
                ));
                o.extend(fld("", 1));
                o.extend(fld(&format!("{h:02}{mi:02}{sec:02}"), 6));
                let room = (num(self, len)?.to_i128()?.max(0) as usize)
                    .min(self.rcv_room(rcv))
                    .min(o.len());
                let avail = o.len() as i32;
                if room >= 8 {
                    o[0..4].copy_from_slice(&(room as i32).to_be_bytes());
                    o[4..8].copy_from_slice(&avail.to_be_bytes());
                }
                self.bufs[rcv.buf][rcv.off..rcv.off + room].copy_from_slice(&o[..room]);
                self.api_ok(rest.first());
            }
            // QLICHGLL (current library, product library 1 and 2, user
            // libraries (CHAR(11) each), their number, error code)
            // (Change Library List API): the job's current library
            // (*SAME keeps it, *CRTDFT: none) and user portion, without a
            // message; product libraries are not kept here.
            "QLICHGLL" => {
                let [cur, _p1, _p2, list, count, rest @ ..] = locs else {
                    bail!("QLICHGLL needs 5 parameters and an error code");
                };
                let cur = text(self, cur).trim().to_ascii_uppercase();
                let n = num(self, count)?.to_i128()?.max(0) as usize;
                let raw = self.bufs[list.buf].get(list.off..).unwrap_or(&[]).to_vec();
                let libs: Vec<String> = (0..n)
                    .map(|i| {
                        a(raw.get(i * 11..i * 11 + 11).unwrap_or(&[]))
                            .trim()
                            .to_ascii_uppercase()
                    })
                    .filter(|l| !l.is_empty())
                    .collect();
                let client = self.io.client;
                for l in &libs {
                    let exists = self.block(crate::catalog::library_exists(
                        client,
                        &crate::qtemp::real(l),
                    ))?;
                    if !exists {
                        return self
                            .api_error(rest.first(), anyhow!("CPF2110: There is no library {l}."))
                            .map(|_| true);
                    }
                }
                if let Some(c) = self.io.cl.as_mut() {
                    match cur.as_str() {
                        "*SAME" | "" => {}
                        "*CRTDFT" => c.curlib = None,
                        l => c.curlib = Some(l.to_string()),
                    }
                    c.user_libl = libs;
                    let mut full: Vec<String> = crate::commands::SYSTEM_LIBRARY_LIST
                        .iter()
                        .map(|l| l.to_string())
                        .collect();
                    full.extend(c.curlib.iter().cloned());
                    for l in &c.user_libl {
                        if !full.contains(l) {
                            full.push(l.clone());
                        }
                    }
                    self.io.libl = full.iter().map(|l| crate::qtemp::real(l)).collect();
                }
                self.api_ok(rest.first());
            }
            // QUSPTRUS (qualified user space name, pointer [, error
            // code]): a pointer to the space's bytes, which this program
            // then changes in place (written back at calls and at its end).
            "QUSPTRUS" => {
                let [qual, ptr, rest @ ..] = locs else {
                    bail!("QUSPTRUS needs a user space name and a pointer");
                };
                let q = self.bufs[qual.buf][qual.off..qual.off + qual.size.min(20)].to_vec();
                let name = a(q.get(..10).unwrap_or(&q)).trim().to_ascii_uppercase();
                let lib = a(q.get(10..20).unwrap_or(&[])).trim().to_ascii_uppercase();
                let caller = crate::dtaq::Caller {
                    user: self.io.user.clone(),
                    job: self.io.job.clone(),
                    libl: self.io.libl.clone(),
                    curlib: self
                        .io
                        .cl
                        .as_ref()
                        .and_then(|c| c.curlib.clone())
                        .unwrap_or_default(),
                };
                let client = self.io.client;
                let found = self.block(async {
                    let s = crate::usrspc::find(client, &name, &lib, &caller).await?;
                    crate::authority::check(
                        client,
                        &caller.user,
                        &s.library,
                        &s.name,
                        "*USRSPC",
                        crate::authority::Aut::Change,
                    )
                    .await?;
                    let data = crate::usrspc::contents(client, &s).await?;
                    Ok::<_, anyhow::Error>((s, data))
                });
                let (s, data) = match found {
                    Ok(x) => x,
                    Err(e) => return self.api_error(rest.first(), e).map(|_| true),
                };
                let buf = match self
                    .bufs
                    .heap
                    .spaces
                    .iter()
                    .find(|(_, l, n, _)| *l == s.library && *n == s.name)
                {
                    Some((b, _, _, _)) => *b,
                    None => {
                        let h = self.bufs.heap.alloc(0);
                        let b = super::mem::HEAP_BASE + h;
                        let len = data.len();
                        self.bufs[b] = space_pages(data, s.initval);
                        self.bufs
                            .heap
                            .spaces
                            .push((b, s.library.clone(), s.name.clone(), len));
                        b
                    }
                };
                if let Value::Ptr(p) = self.ptr_to(buf, 0) {
                    let n = ptr.size.min(16);
                    self.bufs[ptr.buf][ptr.off..ptr.off + n].copy_from_slice(&p.encode()[..n]);
                }
                self.api_ok(rest.first());
            }
            // The data queue APIs (dtaq.rs): the parameters' storage is
            // copied out, and back after the call (by reference).
            n if crate::sysapi::is_api(n) => {
                // A receiver over more storage than its variable (a based
                // structure in heap storage of the length passed): that
                // much, as far as the storage goes.
                let mut sizes: Vec<usize> = locs.iter().map(|l| l.size).collect();
                if crate::sysapi::receiver_first(n) && locs.len() > 1 {
                    let want = num(self, &locs[1])?.to_i128()?.max(0) as usize;
                    let l = &locs[0];
                    let room = self.bufs[l.buf].len().saturating_sub(l.off);
                    sizes[0] = l.size.max(want.min(room));
                }
                let mut args = crate::dtaq::RpgArgs(
                    locs.iter()
                        .zip(&sizes)
                        .map(|(l, &n)| (self.bufs[l.buf][l.off..l.off + n].to_vec(), l.dtype))
                        .collect(),
                );
                let caller = crate::dtaq::Caller {
                    user: self.io.user.clone(),
                    job: self.io.job.clone(),
                    libl: self.io.libl.clone(),
                    curlib: self
                        .io
                        .cl
                        .as_ref()
                        .and_then(|c| c.curlib.clone())
                        .unwrap_or_default(),
                };
                self.block(crate::sysapi::call_api(
                    self.io.client,
                    &caller,
                    n,
                    &mut args,
                ))?;
                for (l, (b, _)) in locs.iter().zip(args.0) {
                    let n = b.len();
                    self.bufs[l.buf][l.off..l.off + n].copy_from_slice(&b);
                }
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
            // QMHSNDPM (message ID, qualified message file, message
            // data, its length, message type, call stack entry, counter,
            // message key returned, error code): Send Program Message.
            "QMHSNDPM" => {
                let [id, msgf, data, dlen, ty, entry, counter, rest @ ..] = locs else {
                    bail!("QMHSNDPM needs 7 parameters before the message key");
                };
                let msgid = text(self, id).trim().to_ascii_uppercase();
                let qual = text(self, msgf);
                let (fname, flib) = (
                    qual.get(0..10).unwrap_or("").trim().to_string(),
                    qual.get(10..20).unwrap_or("").trim().to_string(),
                );
                let n = (num(self, dlen)?.to_i128()?.max(0) as usize).min(data.size);
                let bytes = self.bufs[data.buf][data.off..data.off + n].to_vec();
                let ty = text(self, ty).trim().to_ascii_uppercase();
                let queue = self.stack_entry(&text(self, entry), num(self, counter)?.to_i128()?);
                let msg = if msgid.is_empty() {
                    crate::pgmmsg::QMsg {
                        text: a(&bytes).trim_end().to_string(),
                        ..Default::default()
                    }
                } else {
                    let f = if flib.is_empty() || flib == "*LIBL" {
                        fname
                    } else {
                        format!("{flib}/{fname}")
                    };
                    let r = self.io.rt.block_on(crate::pgmmsg::describe(
                        self.io.client,
                        &self.io.libl,
                        &Default::default(),
                        &msgid,
                        &f,
                        &bytes,
                    ));
                    match r {
                        Ok(m) => m,
                        Err(e) => return self.api_error(rest.get(1), e).map(|_| true),
                    }
                };
                let msg = crate::pgmmsg::QMsg {
                    ty: ty.clone(),
                    data: if msgid.is_empty() {
                        data::e(&msg.text)
                    } else {
                        bytes.clone()
                    },
                    sender: self.sender(),
                    ..msg
                };
                if ty == "*ESCAPE" {
                    // An escape message ends the call stack entries up to
                    // the one it is sent to: here, the call fails with it,
                    // and the procedure that handles it finds it on its
                    // queue.
                    let mut m = msg;
                    if m.msgid.is_empty() {
                        m.msgid = "CPF9898".into();
                    }
                    let entry_text = text(self, entry);
                    let up = num(self, counter)?.to_i128()?.max(0) as usize;
                    // Sent to a program entry procedure: the job log shows
                    // it there, once (PUB400 2026-10-28,
                    // tools/golden/verify21.txt h21_joblog: GLD21Q's
                    // CPF9898 to _QRNP_PEP_GLD21Q, which GLD21A's MONMSG
                    // took); the caller still gets it.
                    if let Some(crate::pgmmsg::Target::Pep(main)) =
                        crate::pgmmsg::stack_target_pep(&self.io.job_key, &entry_text, up)
                    {
                        m.key = crate::pgmmsg::pep_send(&self.io.job_key, &main, m.clone());
                        self.logged.push(m.key);
                    }
                    let target = crate::pgmmsg::stack_target(&self.io.job_key, &entry_text, up);
                    return Err(anyhow::Error::new(crate::pgmmsg::Escape(m, target)));
                }
                // The call stack entry the message goes to (entry and
                // counter), when it is an RPG procedure's.
                let entry_text = text(self, entry);
                let up = num(self, counter)?.to_i128()?.max(0) as usize;
                let target = if entry_text.trim().eq_ignore_ascii_case("*EXT") {
                    None
                } else {
                    crate::pgmmsg::stack_target_pep(&self.io.job_key, &entry_text, up)
                };
                let (stacked, pep) = match target {
                    Some(crate::pgmmsg::Target::Entry(id)) => (Some(id), None),
                    Some(crate::pgmmsg::Target::Pep(main)) => (None, Some(main)),
                    None => (None, None),
                };
                if let Some(id) = stacked {
                    let key =
                        crate::pgmmsg::stack_send(&self.io.job_key, id, msg.clone()).unwrap_or(0);
                    self.logged.push(key);
                    // The program's queue by name too (message subfiles,
                    // SFLPGMQ).
                    if let Some(q) = queue.clone().filter(|q| q != "*EXT") {
                        crate::pgmmsg::named_send(
                            &self.io.job_key,
                            &q,
                            crate::pgmmsg::QMsg { key, ..msg },
                        );
                    }
                    if let Some(k) = rest.first() {
                        let kb = crate::pgmmsg::key_bytes(key);
                        let n = k.size.min(4);
                        self.bufs[k.buf][k.off..k.off + n].copy_from_slice(&kb[..n]);
                    }
                    self.api_ok(rest.get(1));
                    return Ok(true);
                }
                // A program entry procedure: the message goes on its
                // queue, which the job log shows (PUB400 2026-10-28,
                // tools/golden/verify19.txt b19_joblog, verify20.txt
                // e20_joblog: '*' counter 1 from a main procedure goes to
                // _QRNP_PEP_ and the program's name).
                if let Some(main) = pep {
                    let key = crate::pgmmsg::pep_send(&self.io.job_key, &main, msg.clone());
                    self.logged.push(key);
                }
                // *EXT, or a caller above the RPG programs (a command
                // line, a CL program): the message goes to the job log.
                let Some(queue) = queue.filter(|q| q != "*EXT") else {
                    self.job_log(&msg.text);
                    self.api_ok(rest.get(1));
                    return Ok(true);
                };
                let key = crate::pgmmsg::named_send(&self.io.job_key, &queue, msg);
                if let Some(k) = rest.first() {
                    let kb = crate::pgmmsg::key_bytes(key);
                    let n = k.size.min(4);
                    self.bufs[k.buf][k.off..k.off + n].copy_from_slice(&kb[..n]);
                }
                self.api_ok(rest.get(1));
            }
            // QMHRCVPM (receiver, its length, format RCVM0100 or RCVM0300,
            // call stack entry, counter, message type, message key, wait
            // time, action, error code) (Receive Program Message API): a
            // message from the call message queue of a call stack entry.
            "QMHRCVPM" => {
                let [rcv, len, fmt, entry, counter, ty, key, _wait, action, rest @ ..] = locs
                else {
                    bail!("QMHRCVPM needs 9 parameters and an error code");
                };
                let format = text(self, fmt).trim().to_ascii_uppercase();
                if !matches!(format.as_str(), "RCVM0100" | "RCVM0200" | "RCVM0300") {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF3C21: Format name {format} is not valid."),
                        )
                        .map(|_| true);
                }
                let up = num(self, counter)?.to_i128()?.max(0) as usize;
                let target = crate::pgmmsg::stack_target(&self.io.job_key, &text(self, entry), up);
                let msgtype = text(self, ty).trim().to_ascii_uppercase();
                let msgtype = if msgtype.is_empty() {
                    "*ANY".to_string()
                } else {
                    msgtype
                };
                let k = crate::pgmmsg::key_from_bytes(
                    &self.bufs[key.buf][key.off..key.off + key.size.min(4)],
                );
                let action = text(self, action).trim().to_ascii_uppercase();
                let mut m = target.and_then(|id| {
                    crate::pgmmsg::stack_receive(&self.io.job_key, id, &msgtype, k, &action)
                });
                let mut target = target;
                // A key names one message of the job: one on another call
                // stack entry's queue is received from there (iRPGUnit's
                // getLatestEscMsg, run on IBM i, receives the escape
                // messages of its callers' procedures this way; to be
                // checked against a capture).
                if let (None, Some(key)) = (&m, k) {
                    if let Some((x, id)) =
                        crate::pgmmsg::stack_receive_key(&self.io.job_key, &msgtype, key, &action)
                    {
                        m = Some(x);
                        target = id;
                    }
                }
                let receiver_entry =
                    target.and_then(|id| crate::pgmmsg::stack_entry(&self.io.job_key, id));
                // A key no message on that queue has: CPF2410 (QMHRCVPM,
                // error messages).
                if m.is_none() && k.is_some() {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF2410: Message key not found in message queue *."),
                        )
                        .map(|_| true);
                }
                let bytes = match m {
                    None => {
                        let mut b = vec![0u8; 8];
                        b[0..4].copy_from_slice(&8i32.to_be_bytes());
                        b
                    }
                    Some(m) => self.rcvm_format(&format, &m, receiver_entry.as_ref()),
                };
                let n = (num(self, len)?.to_i128()?.max(0) as usize)
                    .min(self.rcv_room(rcv))
                    .min(bytes.len());
                self.bufs[rcv.buf][rcv.off..rcv.off + n].copy_from_slice(&bytes[..n]);
                self.api_ok(rest.first());
            }
            // QGYOLMSG (receiver, its length, list information, number
            // of records to return, sort ('0': none), message selection
            // information, its size, user or queue information, message
            // queues used, error code) (Open List of Messages API): a
            // user's or a message queue's messages in format LSTM0100.
            "QGYOLMSG" => {
                let [rcv, len, info, count, _sort, sel, sel_len, uq, used, rest @ ..] = locs else {
                    bail!("QGYOLMSG needs 9 parameters and an error code");
                };
                let room = self.bufs[sel.buf].len().saturating_sub(sel.off);
                let n = (num(self, sel_len)?.to_i128()?.max(0) as usize).min(room);
                let s = self.bufs[sel.buf][sel.off..sel.off + n].to_vec();
                let q = a(self.bufs[uq.buf].get(uq.off..uq.off + 21).unwrap_or(&[]));
                let (ind, qname, qlib) = (
                    q.get(0..1).unwrap_or("0").to_string(),
                    q.get(1..11).unwrap_or("").trim().to_ascii_uppercase(),
                    q.get(11..21).unwrap_or("").trim().to_ascii_uppercase(),
                );
                let client = self.io.client;
                let (key, shown) = if ind == "1" {
                    let libs: Vec<String> = match qlib.as_str() {
                        "*LIBL" | "" => self.io.libl.clone(),
                        "*CURLIB" => self
                            .io
                            .cl
                            .as_ref()
                            .and_then(|c| c.curlib.clone())
                            .into_iter()
                            .collect(),
                        l => vec![crate::qtemp::real(l)],
                    };
                    match self.block(crate::jobs::msgq_key(client, &libs, &qname))? {
                        Some(k) => {
                            let lib = k
                                .split_once('/')
                                .map(|x| crate::qtemp::logical(x.0))
                                .unwrap_or_else(|| "QUSRSYS".into());
                            (k, (qname.clone(), lib))
                        }
                        None => {
                            return self
                                .api_error(
                                    rest.first(),
                                    anyhow!("CPF2403: Message queue {qname} in {qlib} not found."),
                                )
                                .map(|_| true);
                        }
                    }
                } else {
                    let user = if qname == "*CURRENT" || qname.is_empty() {
                        self.io.user.to_ascii_uppercase()
                    } else {
                        qname.clone()
                    };
                    (user.clone(), (user, "QUSRSYS".to_string()))
                };
                let (msgs, fields, max_len) =
                    lstm_select(self.block(crate::jobs::queue_messages(client, &key))?, &s);
                let room = (num(self, len)?.to_i128()?.max(0) as usize).min(self.rcv_room(rcv));
                let want = num(self, count)?.to_i128()?;
                let mut out: Vec<u8> = Vec::new();
                let mut returned = 0usize;
                for (us, m) in &msgs {
                    let e = lstm0100(*us, m, &shown, &fields, max_len, out.len());
                    if out.len() + e.len() > room || (want >= 0 && returned as i128 >= want) {
                        break;
                    }
                    out.extend(e);
                    returned += 1;
                }
                self.bufs[rcv.buf][rcv.off..rcv.off + out.len()].copy_from_slice(&out);
                let kept: Vec<Vec<u8>> = msgs
                    .iter()
                    .map(|(us, m)| lstm0100(*us, m, &shown, &fields, max_len, 0))
                    .collect();
                let li = crate::spoolapi::list_info(
                    msgs.len(),
                    returned,
                    crate::spoolapi::list_open(kept),
                    0,
                    usize::from(returned > 0),
                    out.len(),
                );
                let k = info.size.min(80);
                self.bufs[info.buf][info.off..info.off + k].copy_from_slice(&li[..k]);
                // Message queues used: their number, then each qualified
                // name.
                let mut u = Vec::with_capacity(44);
                u.extend(1i32.to_be_bytes());
                for (v, n) in [(&shown.0, 10), (&shown.1, 10)] {
                    let mut b = data::e(v);
                    b.resize(n, 0x40);
                    u.extend(b);
                }
                u.resize(44, 0x40);
                let k = used.size.min(44);
                self.bufs[used.buf][used.off..used.off + k].copy_from_slice(&u[..k]);
                self.api_ok(rest.first());
            }
            // QGYOLJBL (receiver, its length, list information, number of
            // records to return, message selection information, its
            // size, error code) (Open List of Job Log Messages API):
            // this job's messages in format OLJL0100.
            "QGYOLJBL" => {
                let [rcv, len, info, count, sel, sel_len, rest @ ..] = locs else {
                    bail!("QGYOLJBL needs 6 parameters and an error code");
                };
                // The size given: the field identifiers and message queue
                // name follow the fixed part in the caller's storage.
                let room = self.bufs[sel.buf].len().saturating_sub(sel.off);
                let n = (num(self, sel_len)?.to_i128()?.max(0) as usize).min(room);
                let s = self.bufs[sel.buf][sel.off..sel.off + n].to_vec();
                let (msgs, fields, max_len) = oljl_select(&self.io.job_key, &s)?;
                let room = (num(self, len)?.to_i128()?.max(0) as usize).min(self.rcv_room(rcv));
                let want = num(self, count)?.to_i128()?;
                // Whole entries that fit (and no more than asked for).
                let mut out: Vec<u8> = Vec::new();
                let mut returned: i32 = 0;
                for m in &msgs {
                    let e = oljl0100(m, &fields, max_len, out.len());
                    if out.len() + e.len() > room || (want >= 0 && returned as i128 >= want) {
                        break;
                    }
                    out.extend(e);
                    returned += 1;
                }
                self.bufs[rcv.buf][rcv.off..rcv.off + out.len()].copy_from_slice(&out);
                let mut li = [0u8; 80];
                li[0..4].copy_from_slice(&(msgs.len() as i32).to_be_bytes());
                li[4..8].copy_from_slice(&returned.to_be_bytes());
                // QGYGTLE gets the entries from the list, each as the
                // first in its receiver.
                let kept: Vec<Vec<u8>> = msgs
                    .iter()
                    .map(|m| oljl0100(m, &fields, max_len, 0))
                    .collect();
                li[8..12].copy_from_slice(&crate::spoolapi::list_open(kept).to_be_bytes());
                li[16] = data::e("C")[0];
                let ((y, mo, d), (h, mi, sec), _) = crate::rpg::dates::now();
                let created = format!(
                    "{}{:02}{mo:02}{d:02}{h:02}{mi:02}{sec:02}",
                    u8::from(y >= 2000),
                    y % 100
                );
                li[17..30].copy_from_slice(&data::e(&created));
                li[30] = data::e("2")[0];
                li[32..36].copy_from_slice(&(out.len() as i32).to_be_bytes());
                li[36..40].copy_from_slice(&i32::from(returned > 0).to_be_bytes());
                for b in &mut li[40..80] {
                    *b = 0;
                }
                let k = info.size.min(80);
                self.bufs[info.buf][info.off..info.off + k].copy_from_slice(&li[..k]);
                self.api_ok(rest.first());
            }
            // QMHRSNEM (message key, error code [, to call stack entry,
            // its length, its format]) (Resend Escape Message API): an
            // escape message this entry received (blank key: the last
            // new one) sent again as an escape message, to this entry's
            // caller.
            "QMHRSNEM" => {
                let [key, rest @ ..] = locs else {
                    bail!("QMHRSNEM needs a message key and an error code");
                };
                if rest.len() > 1 {
                    bail!("QMHRSNEM with a to call stack entry is not supported");
                }
                let k = crate::pgmmsg::key_from_bytes(
                    &self.bufs[key.buf][key.off..key.off + key.size.min(4)],
                );
                let job = self.io.job_key.clone();
                let here = crate::pgmmsg::stack_top_id(&job);
                let m = crate::pgmmsg::stack(&job)
                    .into_iter()
                    .find(|e| Some(e.id) == here)
                    .and_then(|e| {
                        e.queue.into_iter().rev().find(|m| {
                            m.ty == "*ESCAPE"
                                && match k {
                                    Some(k) => m.key == k,
                                    None => !m.old,
                                }
                        })
                    });
                let Some(m) = m else {
                    bail!("QMHRSNEM: no escape message to resend");
                };
                let target = crate::pgmmsg::stack_target(&job, "*", 1);
                return Err(anyhow::Error::new(crate::pgmmsg::Escape(m, target)));
            }
            // QWCRTVCA (receiver, its length, format RTVC0100, number of
            // attributes, their keys, error code) (Retrieve Current
            // Attributes API): the number of attributes returned, then
            // for each its entry length, key, type of data ('C'), 3
            // reserved bytes, data length and data. Key 0305 is the
            // current user profile name, CHAR(10) (RPGUnit's SPLF checks
            // that length).
            "QWCRTVCA" => {
                let [rcv, len, fmt, count, keys, rest @ ..] = locs else {
                    bail!("QWCRTVCA needs 5 parameters and an error code");
                };
                let format = text(self, fmt).trim().to_ascii_uppercase();
                if format != "RTVC0100" {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF3C21: Format name {format} is not valid."),
                        )
                        .map(|_| true);
                }
                let n = num(self, count)?.to_i128()?.max(0) as usize;
                let kb = self.bufs[keys.buf][keys.off..].to_vec();
                let mut b = Vec::new();
                b.extend((n as i32).to_be_bytes());
                for k in 0..n {
                    let key = kb
                        .get(4 * k..4 * k + 4)
                        .map(|x| i32::from_be_bytes(x.try_into().unwrap()))
                        .unwrap_or(0);
                    let value = match key {
                        305 => {
                            let mut v = data::e(&self.io.user);
                            v.resize(10, 0x40);
                            v
                        }
                        other => bail!("QWCRTVCA attribute key {other} is not supported"),
                    };
                    let total = (16 + value.len()).div_ceil(4) * 4;
                    let mut e = Vec::with_capacity(total);
                    e.extend((total as i32).to_be_bytes());
                    e.extend(key.to_be_bytes());
                    e.extend(data::e("C"));
                    e.extend([0u8; 3]);
                    e.extend((value.len() as i32).to_be_bytes());
                    e.extend(value);
                    e.resize(total, 0);
                    b.extend(e);
                }
                let room = (num(self, len)?.to_i128()?.max(0) as usize)
                    .min(self.rcv_room(rcv))
                    .min(b.len());
                self.bufs[rcv.buf][rcv.off..rcv.off + room].copy_from_slice(&b[..room]);
                self.api_ok(rest.first());
            }
            // QGYCLST (request handle, error code) (Close List API): the
            // list's resources go (a list here keeps none).
            "QGYCLST" => {
                let [handle, rest @ ..] = locs else {
                    bail!("QGYCLST needs a request handle and an error code");
                };
                let h = self.bufs[handle.buf][handle.off..handle.off + 4].to_vec();
                crate::spoolapi::list_close(u32::from_be_bytes(h.try_into().unwrap()));
                self.api_ok(rest.first());
            }
            // QGYOLSPL (receiver, its length, list information, number of
            // records to return, sort information, filter information,
            // qualified job name, format, error code) (Open List of
            // Spooled Files API): format OSPL0100 or OSPL0300, filter
            // OSPF0100; the list stays open for QGYGTLE.
            "QGYOLSPL" => {
                let [rcv, len, info, count, _sort, filter, qjob, fmt, rest @ ..] = locs else {
                    bail!("QGYOLSPL needs 8 parameters and an error code");
                };
                let format = text(self, fmt).trim().to_ascii_uppercase();
                if format != "OSPL0300" {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF3C21: Format name {format} is not valid."),
                        )
                        .map(|_| true);
                }
                let fb = self.bufs[filter.buf][filter.off..].to_vec();
                // The filter's format (after the error code): OSPF0100 or
                // OSPF0200.
                let ffmt = rest
                    .get(1)
                    .map(|l| text(self, l).trim().to_ascii_uppercase())
                    .unwrap_or_default();
                let f = if ffmt == "OSPF0200" {
                    crate::spoolapi::parse_filter_0200(&fb)
                } else {
                    crate::spoolapi::parse_filter(&fb)
                };
                let jt = text(self, qjob);
                let job = if jt.trim().is_empty() {
                    None
                } else if jt.trim() == "*" {
                    Some(self.io.job.as_ref().map(|j| j.number).unwrap_or(-1))
                } else {
                    Some(
                        jt.get(20..26)
                            .and_then(|n| n.trim().parse().ok())
                            .unwrap_or(-1),
                    )
                };
                let user = self.io.user.clone();
                let client = self.io.client;
                let files = self.block(crate::spoolapi::select(client, &f, &user, job))?;
                let records: Vec<Vec<u8>> = files.iter().map(crate::spoolapi::ospl0300).collect();
                let total = records.len();
                let room = (num(self, len)?.to_i128()?.max(0) as usize).min(self.rcv_room(rcv));
                let want = num(self, count)?.to_i128()?;
                let mut out = Vec::new();
                let mut returned = 0usize;
                for r in &records {
                    if (want >= 0 && returned as i128 >= want) || out.len() + r.len() > room {
                        break;
                    }
                    out.extend(r);
                    returned += 1;
                }
                self.bufs[rcv.buf][rcv.off..rcv.off + out.len()].copy_from_slice(&out);
                let h = crate::spoolapi::list_open(records);
                let li = crate::spoolapi::list_info(
                    total,
                    returned,
                    h,
                    136,
                    usize::from(returned > 0),
                    out.len(),
                );
                let k = info.size.min(80);
                self.bufs[info.buf][info.off..info.off + k].copy_from_slice(&li[..k]);
                self.api_ok(rest.first());
            }
            // QGYGTLE (receiver, its length, request handle, list
            // information, number of records, starting record, error
            // code) (Get List Entries API).
            "QGYGTLE" => {
                let [rcv, len, handle, info, count, start, rest @ ..] = locs else {
                    bail!("QGYGTLE needs 6 parameters and an error code");
                };
                let hb = self.bufs[handle.buf][handle.off..handle.off + 4].to_vec();
                let h = u32::from_be_bytes(hb.try_into().unwrap());
                let n = num(self, count)?.to_i128()? as i32;
                let s = num(self, start)?.to_i128()? as i32;
                let Some((total, recs)) = crate::spoolapi::list_get(h, s, n) else {
                    return self
                        .api_error(rest.first(), anyhow!("The request handle is not valid."))
                        .map(|_| true);
                };
                let room = (num(self, len)?.to_i128()?.max(0) as usize).min(self.rcv_room(rcv));
                let mut out = Vec::new();
                let mut returned = 0;
                for r in &recs {
                    // A receiver shorter than a record gets the part that
                    // fits: RPGUnit's 132-byte OSPL0300 receiver, written
                    // for releases before the format had its IPP job ID,
                    // reads the spooled file's name and number from it.
                    let k = r.len().min(room - out.len());
                    if k == 0 {
                        break;
                    }
                    out.extend(&r[..k]);
                    returned += 1;
                    if k < r.len() {
                        break;
                    }
                }
                self.bufs[rcv.buf][rcv.off..rcv.off + out.len()].copy_from_slice(&out);
                let rl = recs.first().map_or(0, |r| r.len());
                let li = crate::spoolapi::list_info(
                    total,
                    returned,
                    h,
                    rl,
                    if returned > 0 { s.max(1) as usize } else { 0 },
                    out.len(),
                );
                let k = info.size.min(80);
                self.bufs[info.buf][info.off..info.off + k].copy_from_slice(&li[..k]);
                self.api_ok(rest.first());
            }
            // QSPOPNSP (handle, qualified job name, internal job and
            // spooled file identifiers, spooled file name and number,
            // number of buffers, error code) (Open Spooled File API).
            "QSPOPNSP" => {
                let [handle, qjob, _ijob, _isplf, name, number, buffers, rest @ ..] = locs else {
                    bail!("QSPOPNSP needs 7 parameters and an error code");
                };
                let jt = text(self, qjob);
                let job = if jt.trim() == "*" || jt.trim().is_empty() {
                    None
                } else {
                    jt.get(20..26).and_then(|n| n.trim().parse::<i64>().ok())
                };
                let this = self.io.job.as_ref().map(|j| j.number);
                let file = text(self, name);
                let nbr = num(self, number)?.to_i128()? as i32;
                let nbuf = num(self, buffers)?.to_i128()? as i32;
                let client = self.io.client;
                let r = self.block(crate::spoolapi::open(client, job, this, &file, nbr, nbuf));
                let h = match r {
                    Ok(h) => h,
                    Err(e) => return self.api_error(rest.first(), e).map(|_| true),
                };
                self.bufs[handle.buf][handle.off..handle.off + 4].copy_from_slice(&h.to_be_bytes());
                self.api_ok(rest.first());
            }
            // QSPGETSP (handle, user space, format SPFR0300, ordinal
            // number of the buffer (-1: all), end of file action, error
            // code) (Get Spooled File Data API): into the user space.
            "QSPGETSP" => {
                let [handle, space, fmt, _ord, _end, rest @ ..] = locs else {
                    bail!("QSPGETSP needs 5 parameters and an error code");
                };
                let format = text(self, fmt).trim().to_ascii_uppercase();
                if format != "SPFR0300" {
                    bail!("QSPGETSP format {format} is not supported");
                }
                let h = num(self, handle)?.to_i128()? as i32;
                let Some((id, requested)) = crate::spoolapi::opened(h) else {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("The spooled file handle is not valid."),
                        )
                        .map(|_| true);
                };
                let q = text(self, space);
                let name = q.get(..10).unwrap_or("").trim().to_ascii_uppercase();
                let lib = q.get(10..20).unwrap_or("").trim().to_ascii_uppercase();
                let caller = crate::dtaq::Caller {
                    user: self.io.user.clone(),
                    job: self.io.job.clone(),
                    libl: self.io.libl.clone(),
                    curlib: self
                        .io
                        .cl
                        .as_ref()
                        .and_then(|c| c.curlib.clone())
                        .unwrap_or_default(),
                };
                let client = self.io.client;
                let r = self.block(async {
                    let s = crate::usrspc::find(client, &name, &lib, &caller).await?;
                    let (data, pages) = crate::spoolapi::scs(client, id).await?;
                    let b = crate::spoolapi::spfr0300(&data, &pages, requested);
                    let mut cur = crate::usrspc::contents(client, &s).await?;
                    if cur.len() < b.len() {
                        cur.resize(b.len(), s.initval);
                    }
                    cur[..b.len()].copy_from_slice(&b);
                    crate::usrspc::put_contents(client, &s.library, &s.name, &cur).await
                });
                if let Err(e) = r {
                    return self.api_error(rest.first(), e).map(|_| true);
                }
                self.api_ok(rest.first());
            }
            // QSPCLOSP (handle, error code) (Close Spooled File API).
            "QSPCLOSP" => {
                let [handle, rest @ ..] = locs else {
                    bail!("QSPCLOSP needs a handle and an error code");
                };
                let h = num(self, handle)?.to_i128()? as i32;
                crate::spoolapi::close(h);
                self.api_ok(rest.first());
            }
            // QWVRCSTK (receiver, its length, format CSTK0100, job
            // identification, format JIDF0100, error code) (Retrieve Call
            // Stack API): the call stack of this job's thread.
            "QWVRCSTK" => {
                let [rcv, len, fmt, jid, jfmt, rest @ ..] = locs else {
                    bail!("QWVRCSTK needs 5 parameters and an error code");
                };
                let format = text(self, fmt).trim().to_ascii_uppercase();
                if format != "CSTK0100" {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF3C21: Format name {format} is not valid."),
                        )
                        .map(|_| true);
                }
                let jformat = text(self, jfmt).trim().to_ascii_uppercase();
                if jformat != "JIDF0100" {
                    return self
                        .api_error(
                            rest.first(),
                            anyhow!("CPF3C21: Format name {jformat} is not valid."),
                        )
                        .map(|_| true);
                }
                let job = text(self, jid);
                if !job.starts_with('*') {
                    // Another job's call stack is not kept (fidelity-gaps).
                    bail!("QWVRCSTK of a job other than '*' is not supported");
                }
                crate::pgmmsg::stack_line(&self.io.job_key, self.line);
                if std::env::var_os("AS4TAINR_DEBUG_STACK").is_some() {
                    eprintln!("QWVRCSTK for {}:", self.io.job_key);
                    for e in crate::pgmmsg::stack_listed(&self.io.job_key) {
                        eprintln!(
                            "stack: {}/{} {} {} {}",
                            e.lib, e.pgm, e.module, e.proc, e.actgrp
                        );
                    }
                }
                let bytes = cstk0100(&crate::pgmmsg::stack_listed(&self.io.job_key));
                let n = (num(self, len)?.to_i128()?.max(0) as usize).min(self.rcv_room(rcv));
                let mut out = bytes.clone();
                // Bytes returned: what fits (whole entries only).
                let (fit, count) = cstk_fit(&bytes, n);
                out.truncate(fit);
                out[0..4].copy_from_slice(&(fit as i32).to_be_bytes());
                out[16..20].copy_from_slice(&(count as i32).to_be_bytes());
                // A receiver too small for every entry leaves the
                // information status blank: 'I' and 'N' are about the
                // entries' contents (QWVRCSTK API, "Information status";
                // "the API returns only the data that the area can hold").
                let n = n.min(out.len());
                self.bufs[rcv.buf][rcv.off..rcv.off + n].copy_from_slice(&out[..n]);
                self.api_ok(rest.first());
            }
            // QMHRMVPM (call stack entry, counter, message key, messages
            // to remove, error code): Remove Program Messages.
            "QMHRMVPM" => {
                let [entry, counter, key, which, rest @ ..] = locs else {
                    bail!("QMHRMVPM needs 4 parameters and an error code");
                };
                let queue = self.stack_entry(&text(self, entry), num(self, counter)?.to_i128()?);
                let which = text(self, which).trim().to_ascii_uppercase();
                let k = crate::pgmmsg::key_from_bytes(
                    &self.bufs[key.buf][key.off..key.off + key.size.min(4)],
                );
                if let Some(queue) = queue {
                    crate::pgmmsg::named_remove(&self.io.job_key, &queue, &which, k);
                }
                let up = num(self, counter)?.to_i128()?.max(0) as usize;
                if let Some(id) =
                    crate::pgmmsg::stack_target(&self.io.job_key, &text(self, entry), up)
                {
                    crate::pgmmsg::stack_remove(&self.io.job_key, id, &which, k);
                }
                self.api_ok(rest.first());
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// A call stack entry as the message APIs name it: '*' or '*PGMBDY'
    /// is this program (counter 0) or a caller above it; a name is that
    /// entry.
    /// None: above the outermost RPG program (its caller: the job log).
    fn stack_entry(&self, entry: &str, counter: i128) -> Option<String> {
        let e = entry.trim().to_ascii_uppercase();
        if !matches!(e.as_str(), "*" | "*PGMBDY" | "*CTLBDY" | "") {
            return Some(e);
        }
        if counter <= 0 {
            return Some(self.prog.name.clone());
        }
        let up = counter as usize;
        let c = &self.io.callers;
        (up <= c.len()).then(|| c[c.len() - up].clone())
    }

    /// How far an API may write into a receiver: the storage its
    /// variable is in, from the variable on (IBM i writes as many bytes
    /// as the length parameter says, into whatever storage follows; a
    /// based variable shorter than the length given, as iRPGUnit's
    /// RCVM0300 header over a 2048-byte heap block, gets them all).
    fn rcv_room(&self, rcv: &Loc) -> usize {
        self.bufs
            .get(rcv.buf)
            .map_or(rcv.size, |b| b.len().saturating_sub(rcv.off).max(rcv.size))
    }

    /// An API's error code structure (ERRC0100): bytes provided 0 means
    /// an error is an exception; otherwise the exception ID is returned in
    /// it.
    fn api_error(&mut self, err: Option<&Loc>, e: anyhow::Error) -> Result<()> {
        let provided = err
            .filter(|l| l.size >= 8)
            .map(|l| i32::from_be_bytes(self.bufs[l.buf][l.off..l.off + 4].try_into().unwrap()))
            .unwrap_or(0);
        let Some(l) = err.filter(|_| provided >= 8) else {
            return Err(e);
        };
        let id = crate::clpgm::error_id(&e);
        let mut out = [0x40u8; 7];
        for (o, c) in out.iter_mut().zip(id.bytes()) {
            *o = crate::tn5250::ebcdic::FROM_UNICODE[c as usize];
        }
        let avail: i32 = 15;
        let b = &mut self.bufs[l.buf];
        b[l.off + 4..l.off + 8].copy_from_slice(&avail.min(provided).to_be_bytes());
        let n = ((provided as usize).min(l.size)).saturating_sub(8).min(7);
        b[l.off + 8..l.off + 8 + n].copy_from_slice(&out[..n]);
        Ok(())
    }

    /// The API ended normally: bytes available 0 in the error code.
    fn api_ok(&mut self, err: Option<&Loc>) {
        if let Some(l) = err.filter(|l| l.size >= 8) {
            let provided =
                i32::from_be_bytes(self.bufs[l.buf][l.off..l.off + 4].try_into().unwrap());
            if provided >= 8 {
                self.bufs[l.buf][l.off + 4..l.off + 8].copy_from_slice(&0i32.to_be_bytes());
            }
        }
    }

    /// Run a CL command in the calling job (QCMDEXC, CALL of a CL
    /// program). Its messages go to the job log.
    fn run_cl(&mut self, cmd: &str) -> Result<()> {
        self.run_cl_returning(cmd).map(|_| ())
    }

    /// `run_cl`, with the parameter values a called CL program left (CALL
    /// passes parameters by reference).
    fn run_cl_returning(&mut self, cmd: &str) -> Result<Option<Vec<String>>> {
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
            calls: Default::default(),
        };
        // Overrides in effect for this program, and those QCMDEXC makes
        // (OVRDBF), for the programs it calls next.
        job.calls.overrides = ctx.overrides.clone();
        // QCMDEXC is a call stack entry of its own while the command runs
        // (a program it calls counts its caller's entries past it).
        crate::pgmmsg::stack_line(&self.io.job_key, self.line);
        let qcmdexc = crate::pgmmsg::stack_push(
            &self.io.job_key,
            ("QSYS", "QCMDEXC"),
            "",
            "",
            crate::ile::DEFAULT_GROUP,
        );
        let out = self.io.rt.block_on(job.run(cmd));
        crate::pgmmsg::stack_pop(&self.io.job_key, qcmdexc);
        if let Some(c) = self.io.cl.as_mut() {
            c.overrides = job.calls.overrides.clone();
            // The library list is the job's: what the command changed
            // (CHGLIBL, ADDLIBLE, CHGCURLIB) counts from now on.
            if c.user_libl != job.libl || c.curlib != job.curlib {
                c.user_libl = job.libl.clone();
                c.curlib = job.curlib.clone();
                self.io.libl = job.library_list();
            }
        }
        let returned = job.calls.returned.take();
        let data = job.calls.message_data.take();
        // What the command printed (a program it called, under `system`)
        // is this program's printed output too.
        self.printed.extend(std::mem::take(&mut job.calls.printed));
        let out = out?;
        if let crate::commands::Outcome::Messages(msgs) = out {
            // The command's messages go to the call message queue of the
            // procedure that ran it (QCMDEXC's caller): the last one is its
            // completion message, with the command's message data.
            let target = crate::pgmmsg::stack_target(&self.io.job_key, "*", 0);
            // What programs the command called showed (DSPLY) is not a
            // message to this program.
            let shown = |m: &String| m.starts_with("DSPLY ");
            let n = msgs.iter().rposition(|m| !shown(m)).map_or(0, |p| p + 1);
            for (k, m) in msgs.iter().enumerate() {
                if let (Some(id), false) = (target, shown(m)) {
                    let msgid = crate::jobs::escape_id(m).to_string();
                    let text = if msgid.is_empty() {
                        m.trim_end().to_string()
                    } else {
                        m[msgid.len()..].trim_start_matches(':').trim().to_string()
                    };
                    let last = k + 1 == n;
                    let q = crate::pgmmsg::QMsg {
                        msgf: if msgid.is_empty() {
                            String::new()
                        } else {
                            "QCPFMSG".into()
                        },
                        msgflib: if msgid.is_empty() {
                            String::new()
                        } else {
                            "QSYS".into()
                        },
                        msgid,
                        text: text.clone(),
                        data: if last {
                            data.clone().unwrap_or_else(|| data::e(&text))
                        } else {
                            data::e(&text)
                        },
                        ty: if last { "*COMP" } else { "*INFO" }.into(),
                        sender: crate::pgmmsg::Sender {
                            pgm: "QCMD".into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    if let Some(k) = crate::pgmmsg::stack_send(&self.io.job_key, id, q) {
                        self.logged.push(k);
                    }
                }
                self.job_log(m);
            }
        }
        Ok(returned)
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
    /// A file (LIKEFILE parameter).
    File(usize),
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
        match &sym {
            Some(Sym::Var(v)) => self.check_buf(v.buf)?,
            Some(Sym::Ds(d)) => self.check_buf(d.buf)?,
            _ => {}
        }
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
        self.prog.procs.contains_key(name) || self.proto(name).is_some()
    }

    /// A prototype by name: the running procedure's own first, then the
    /// module's.
    fn proto(&self, name: &str) -> Option<&'a super::program::Proto> {
        let prog: &'a Program = self.prog;
        self.frames
            .last()
            .and_then(|f| f.proc.protos.get(name))
            .or_else(|| prog.protos.get(name))
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
                // Not passed (OPTIONS(*NOPASS)): no argument at all, so
                // %PARMS does not count it (an *OMIT argument is passed).
                continue;
            };
            out.push(if p.file {
                let name = match a {
                    Expr::Name { parts, args: None } => parts.join("."),
                    other => bail!("{what}: parameter {} needs a file, found {other:?}", p.name),
                };
                Arg::File(
                    self.file_idx(&name)
                        .ok_or_else(|| anyhow!("{what}: {name} is not a file"))?,
                )
            } else if matches!(a, Expr::Omit) {
                if !p.omit {
                    bail!("{what}: *OMIT is not allowed for parameter {}", p.name);
                }
                Arg::Omit
            } else if let Some(d) = (p.konst && !p.by_value)
                .then(|| self.whole_ds_array(a))
                .flatten()
            {
                // A whole data structure array for a CONST LIKEDS DIM(n)
                // parameter: all its elements, by reference (the
                // callee's view of them is the same storage).
                Arg::Ref(Loc {
                    buf: d.buf,
                    off: d.off,
                    size: d.size * d.dim as usize,
                    dtype: p.dtype,
                })
            } else if p.konst && !p.by_value && self.ds_array_path(a) {
                // The same for a data structure array subfield of a data
                // structure array element (gV(s).m).
                let l = self.loc_of(a)?;
                Arg::Ref(Loc {
                    dtype: p.dtype,
                    ..l
                })
            } else if p.konst
                && !p.by_value
                && self
                    .array_ref(a)
                    .is_some_and(|r| r.index.is_none() && r.var.dtype == p.dtype)
            {
                // A whole array for an array parameter (CONST DIM(n)): its
                // elements, by reference.
                Arg::Ref(self.loc_of(a)?)
            } else if p.konst
                && !p.by_value
                && !p.trim
                && !p.convert
                && (self.same_type_variable(a, &p.dtype)
                    || p.varsize && self.char_variable(a) && matches!(p.dtype, DType::Char(_)))
            {
                // CONST: a variable of the parameter's own type is passed
                // by reference (ILE RPG Reference, CONST: a temporary is
                // used only when the value is not of the right type), so
                // the callee addresses the caller's storage.
                Arg::Ref(self.loc_of(a)?)
            } else if p.by_value || p.konst {
                // OPTIONS(*CONVERT): %CHAR of a numeric, date, time or
                // timestamp argument for a character parameter.
                let converts = p.convert
                    && matches!(p.dtype, DType::Char(_) | DType::Varchar(_))
                    && !matches!(
                        self.type_of(a),
                        Some(DType::Char(_)) | Some(DType::Varchar(_)) | None
                    );
                let mut v = if converts {
                    self.bif("CHAR", std::slice::from_ref(a))?
                } else {
                    self.eval(a)?
                };
                // OPTIONS(*TRIM): without leading and trailing blanks.
                if p.trim {
                    if let Value::Char(b) = &v {
                        let start = b.iter().position(|&c| c != 0x40).unwrap_or(b.len());
                        let end = b.iter().rposition(|&c| c != 0x40).map_or(start, |e| e + 1);
                        v = Value::Char(b[start..end].to_vec());
                    }
                }
                // OPTIONS(*STRING): a character value goes as a pointer to
                // a null-terminated copy (ILE RPG Reference, OPTIONS).
                if p.string && p.dtype == DType::Pointer && matches!(v, Value::Char(_)) {
                    let mut bytes = v.chars()?;
                    bytes.push(0);
                    let h = self.bufs.heap.alloc(bytes.len());
                    self.bufs.heap.blocks[h] = bytes;
                    self.temp_strings.push(h);
                    Arg::Val(self.ptr_to(super::mem::HEAP_BASE + h, 0))
                } else {
                    Arg::Val(v)
                }
            } else {
                Arg::Ref(self.loc_of(a)?)
            });
        }
        Ok(out)
    }

    /// Is the expression a variable (not an expression or a literal) of
    /// exactly this type?
    /// RESET (ILE RPG Reference, RESET): a variable or data structure
    /// back to its initial value (INZ), from the program's initial
    /// storage (a procedure's for a local one). False when it has none
    /// (based storage), for CLEAR's rule.
    fn reset_to_initial(&mut self, name: &str) -> Result<bool> {
        let local = self
            .frames
            .last()
            .is_some_and(|f| f.proc.syms.contains_key(name));
        let (tbuf, off, len) = match self.sym_static(name) {
            Some(Sym::Var(v)) => (
                v.buf,
                v.off,
                v.stride * (v.dim.max(1) as usize - 1) + v.dtype.size(),
            ),
            Some(Sym::Ds(d)) => (d.buf, d.off, d.size * d.dim.max(1) as usize),
            _ => return Ok(false),
        };
        let based = if local {
            self.frames
                .last()
                .is_some_and(|f| f.proc.based.contains_key(&tbuf))
        } else {
            self.prog.based.contains_key(&tbuf)
        };
        if based {
            return Ok(false);
        }
        let image = if local {
            self.frames
                .last()
                .and_then(|f| f.proc.frame.get(tbuf))
                .cloned()
        } else {
            self.prog.globals.get(tbuf).cloned()
        };
        let Some(image) = image.filter(|b| off + len <= b.len()) else {
            return Ok(false);
        };
        let (buf, at) = match self.sym(name) {
            Some(Sym::Var(v)) => (v.buf, v.off),
            Some(Sym::Ds(d)) => (d.buf, d.off),
            _ => return Ok(false),
        };
        if at + len > self.bufs[buf].len() {
            return Ok(false);
        }
        self.bufs[buf][at..at + len].copy_from_slice(&image[off..off + len]);
        Ok(true)
    }

    /// CLEAR of a data structure: each subfield to its type's initial
    /// value, the subfields of a LIKEDS subfield too (ILE RPG
    /// Reference, CLEAR: every subfield of a data structure is cleared
    /// according to its type).
    fn clear_ds(&mut self, name: &str, d: &DsInfo) {
        let prefix = if d.qualified {
            format!("{name}.")
        } else {
            String::new()
        };
        for sf in &d.subfields {
            let full = format!("{prefix}{sf}");
            match self.sym(&full) {
                Some(Sym::Var(v)) => {
                    for k in 0..v.dim.max(1) as usize {
                        let at = v.off + k * v.stride;
                        self.bufs[v.buf][at..at + v.dtype.size()]
                            .copy_from_slice(&v.dtype.initial());
                    }
                }
                Some(Sym::Ds(inner)) if d.qualified && full != name => {
                    self.clear_ds(&full, &inner);
                    // A LIKEDS DIM subfield: the other elements as the
                    // first (its subfields address the first element).
                    let one = self.bufs[inner.buf][inner.off..inner.off + inner.size].to_vec();
                    for k in 1..inner.dim.max(1) as usize {
                        let at = inner.off + k * inner.size;
                        if at + inner.size <= self.bufs[inner.buf].len() {
                            self.bufs[inner.buf][at..at + inner.size].copy_from_slice(&one);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The bytes of one cleared element of a data structure (array):
    /// CLEAR's result for the first element, the storage left as it was.
    fn ds_clear_image(&mut self, name: &str, d: &DsInfo) -> Vec<u8> {
        let end = (d.off + d.size * d.dim.max(1) as usize).min(self.bufs[d.buf].len());
        let saved = self.bufs[d.buf][d.off..end].to_vec();
        self.clear_ds(name, d);
        let img = self.bufs[d.buf][d.off..d.off + d.size].to_vec();
        self.bufs[d.buf][d.off..end].copy_from_slice(&saved);
        img
    }

    /// A fixed-length character variable (or data structure) the
    /// argument names: CONST OPTIONS(*VARSIZE) passes its storage, of
    /// whatever length (QGYOLJBL's selection information is longer than
    /// the data structure passed for it).
    fn char_variable(&mut self, a: &Expr) -> bool {
        if !matches!(
            a,
            Expr::Name { args: None, .. } | Expr::Path { args: None, .. }
        ) {
            return false;
        }
        if matches!(a, Expr::Name { parts, .. } if parts.len() == 1 && parts[0].starts_with('*')) {
            return false;
        }
        matches!(self.loc_of(a), Ok(l) if matches!(l.dtype, DType::Char(_)))
    }

    fn same_type_variable(&mut self, a: &Expr, t: &DType) -> bool {
        if !matches!(
            a,
            Expr::Name { args: None, .. } | Expr::Path { args: None, .. }
        ) {
            return false;
        }
        if matches!(a, Expr::Name { parts, .. } if parts.len() == 1 && parts[0].starts_with('*')) {
            return false;
        }
        match self.loc_of(a) {
            Ok(l) => l.dtype == *t,
            Err(_) => false,
        }
    }

    /// CALLP, or a call in an expression: a procedure of this program, or
    /// a prototype for one or for a program (EXTPGM). Returns the
    /// procedure's return value.
    fn call_named(&mut self, name: &str, args: &[Expr]) -> Result<Option<Value>> {
        // Where this entry is, for the call stack (QWVRCSTK).
        crate::pgmmsg::stack_line(&self.io.job_key, self.line);
        let mark = self.temp_strings.len();
        let r = self.call_named_inner(name, args);
        // The OPTIONS(*STRING) copies last as long as the call.
        for h in self.temp_strings.split_off(mark) {
            if let (Some(f), Some(b)) = (
                self.bufs.heap.freed.get_mut(h),
                self.bufs.heap.blocks.get_mut(h),
            ) {
                *f = true;
                *b = Vec::new();
            }
        }
        r
    }

    fn call_named_inner(&mut self, name: &str, args: &[Expr]) -> Result<Option<Value>> {
        let prog: &'a Program = self.prog;
        if let Some(pi) = prog.procs.get(name) {
            let a = self.args_for(name, &pi.params, args)?;
            return self.call_proc(pi, a);
        }
        let Some(pr) = self.proto(name) else {
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
                    Arg::File(_) => bail!("{name}: a file cannot be passed to a program"),
                }
            }
            let r = self.call_program(&pgm, &locs);
            self.bufs.truncate(base);
            r?;
            return Ok(None);
        }
        let target = match &pr.extproc_ptr {
            // EXTPROC(procedure pointer): the procedure it addresses now.
            Some(var) => {
                let (pname, home) = match self.eval(&Expr::name(var))? {
                    Value::Ptr(p) if p.kind == super::mem::KIND_PROC => super::mem::proc_ref(p.buf)
                        .ok_or_else(|| anyhow!("MCH3601: procedure pointer {var} is not valid."))?,
                    Value::Ptr(_) => bail!("MCH3601: {var} is not a procedure pointer."),
                    _ => bail!("MCH3601: procedure pointer {var} is not set (*NULL)."),
                };
                // A procedure of a service program (not this module of
                // it): called there, in its activation.
                // A procedure of another module bound into the program.
                if let Some(("*PGM", spec)) = home.as_ref().map(|(l, m)| (l.as_str(), m.clone())) {
                    let (pgm, group) = spec.split_once(' ').unwrap_or((spec.as_str(), ""));
                    let here = self.io.program == pgm && prog.procs.contains_key(&pname);
                    if !here {
                        let (library, name) = pgm.split_once('/').unwrap_or(("", pgm));
                        let a = self.args_for(name, &pr.params, args)?;
                        return self.call_bound_in(
                            &pname,
                            &crate::ile::Target::Pgm {
                                library: library.to_string(),
                                name: name.to_string(),
                                group: group.to_string(),
                            },
                            Some(name),
                            a,
                        );
                    }
                } else if let Some(("*MODULE", m)) =
                    home.as_ref().map(|(l, m)| (l.as_str(), m.clone()))
                {
                    let here =
                        self.io.program == format!("*BOUND/{m}") && prog.procs.contains_key(&pname);
                    if !here {
                        let a = self.args_for(name, &pr.params, args)?;
                        return self.call_bound_in(
                            &pname,
                            &crate::ile::Target::Module(m.clone()),
                            Some(&m),
                            a,
                        );
                    }
                } else if let Some((library, sp)) = home {
                    let (sp, module) = match sp.split_once('/') {
                        Some((s, m)) => (s.to_string(), Some(m.to_string())),
                        None => (sp, None),
                    };
                    let here = self.current_srvpgm() == Some((library.clone(), sp.clone()))
                        && module.as_ref().is_none_or(|m| *m == prog.name)
                        && prog.procs.contains_key(&pname);
                    if !here {
                        let a = self.args_for(name, &pr.params, args)?;
                        return self.call_bound_in(
                            &pname,
                            &crate::ile::Target::SrvPgm { library, name: sp },
                            module.as_deref(),
                            a,
                        );
                    }
                }
                pname
            }
            None => pr.extproc.clone().unwrap_or_else(|| name.to_string()),
        };
        match prog.procs.get(&target) {
            Some(pi) => {
                let a = self.args_for(name, &pr.params, args)?;
                self.call_proc(pi, a)
            }
            None if cee::CEE_APIS.contains(&target.to_ascii_uppercase().as_str()) => {
                let a = self.args_for(name, &pr.params, args)?;
                let sym = target.to_ascii_uppercase();
                self.cee_call(&sym, &a).expect("a CEE API").map(Some)
            }
            None if crt::C_FUNCTIONS.contains(&target.to_ascii_uppercase().as_str()) => {
                // A C runtime or UNIX-type function. Arguments beyond the
                // prototype's go as values (printf's variable list).
                let n = args.len().min(pr.params.len());
                let mut a = self.args_for(name, &pr.params, &args[..n])?;
                a.truncate(n);
                // CONST arguments passed by reference: their values (the C
                // functions take values and pointers); a buffer the function
                // fills (getcwd's, stat's; declared CONST in some
                // prototypes, as iRPGUnit's) goes as its address.
                let sym_name = target.to_ascii_uppercase();
                for (k, (x, p)) in a.iter_mut().zip(&pr.params).enumerate() {
                    if let (Arg::Ref(l), true) = (&*x, p.konst) {
                        let l = *l;
                        if crt::C_OUT_BUFFERS.contains(&(sym_name.as_str(), k)) {
                            *x = Arg::Val(self.ptr_to(l.buf, l.off));
                            continue;
                        }
                        *x = Arg::Val(data::load(
                            &l.dtype,
                            &self.bufs[l.buf][l.off..l.off + l.size],
                        )?);
                    }
                }
                for x in &args[n..] {
                    a.push(Arg::Val(self.eval(x)?));
                }
                let sym = target.to_ascii_uppercase();
                let v = self.c_call(&sym, &a).expect("a C function")?;
                Ok(Some(v))
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
        self.call_bound_in(sym, target, None, args)
    }

    /// `call_bound`; with `module`, a procedure of that module of the
    /// service program, exported or not (called through a procedure
    /// pointer).
    fn call_bound_in(
        &mut self,
        sym: &str,
        target: &crate::ile::Target,
        module: Option<&str>,
        args: Vec<Arg>,
    ) -> Result<Option<Value>> {
        let client = self.io.client;
        let libl = self.io.libl.clone();
        // Found and compiled at the first call in the activation group
        // (ILE Concepts: a service program is activated once, then its
        // procedures are called directly).
        let bound_key = (
            self.io.job_key.clone(),
            self.io.actgrp.clone(),
            format!("{}|{:?}|{module:?}|{sym}", self.io.program, target),
        );
        let cached = BOUND
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|m| m.get(&bound_key).cloned()));
        let generation = crate::ile::BINDING_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
        let (_, unit, binding, prog) = match cached.filter(|c| c.0 == generation) {
            Some(c) => c,
            None => {
                // A CL module's procedure (ILE CL: one procedure, named as
                // the module): it runs in the job as a CL program does, its
                // parameters by reference.
                if let Some(m) = self.cl_module(sym, target, module)? {
                    return self.call_cl_module(&m, args);
                }
                let mut c = self.resolve_bound(sym, target, module, client, &libl)?;
                c.0 = generation;
                if let Ok(mut g) = BOUND.lock() {
                    g.get_or_insert_with(HashMap::new)
                        .insert(bound_key, c.clone());
                }
                c
            }
        };
        let prog: &Program = &prog;
        let pi = prog
            .procs
            .get(sym)
            .filter(|pi| pi.export || module.is_some())
            .ok_or_else(|| anyhow!("CPD5D02: Module does not export {sym}."))?;
        self.run_bound(sym, target, args, unit, binding, prog, pi)
    }

    /// The CL module a bound call of `sym` goes to, if it is one.
    fn cl_module(
        &mut self,
        sym: &str,
        target: &crate::ile::Target,
        module: Option<&str>,
    ) -> Result<Option<crate::ile::ModuleCopy>> {
        use crate::ile::Target;
        let is_cl = |m: &crate::ile::ModuleCopy| {
            m.attribute == "CLLE"
                && m.name.eq_ignore_ascii_case(sym)
                && module.is_none_or(|x| x.eq_ignore_ascii_case(&m.name))
        };
        Ok(match target {
            Target::Module(name) => self.io.binding.as_ref().and_then(|b| {
                b.modules
                    .iter()
                    .find(|x| &x.name == name && is_cl(x))
                    .cloned()
            }),
            Target::SrvPgm { library, name } => {
                let client = self.io.client;
                let libl = self.io.libl.clone();
                let (_, b) = self.block(crate::ile::find_srvpgm(client, &libl, library, name))?;
                b.modules.iter().find(|x| is_cl(x)).cloned()
            }
            Target::Pgm { .. } => None,
        })
    }

    /// Call a CL module's procedure: the arguments as CALL passes them
    /// (text), the values it leaves in its parameters come back.
    fn call_cl_module(
        &mut self,
        m: &crate::ile::ModuleCopy,
        args: Vec<Arg>,
    ) -> Result<Option<Value>> {
        let ctx = self.io.cl.clone().ok_or_else(|| {
            anyhow!(
                "CL procedure {} cannot run here: the program has no job",
                m.name
            )
        })?;
        let client = self.io.client;
        let libl = self.io.libl.clone();
        let pgm = self.block(crate::clpgm::compile_in(client, &libl, &m.name, &m.source))?;
        let mut texts = Vec::new();
        let mut locs = Vec::new();
        for a in &args {
            let v = match a {
                Arg::Ref(l) => {
                    locs.push(Some(*l));
                    data::load(&l.dtype, &self.bufs[l.buf][l.off..l.off + l.size])?
                }
                Arg::Val(v) => {
                    locs.push(None);
                    v.clone()
                }
                Arg::Omit | Arg::File(_) => break,
            };
            texts.push(match v {
                Value::Num(d) => d.to_string(),
                other => a_text(&other.chars()?),
            });
        }
        let mut job = crate::commands::Job {
            client,
            env: &ctx.env,
            user: self.io.user.clone(),
            curlib: ctx.curlib.clone(),
            libl: ctx.user_libl.clone(),
            display: self.io.display.clone(),
            id: self.io.job.clone(),
            calls: Default::default(),
        };
        job.calls.overrides = ctx.overrides.clone();
        let out = self
            .io
            .rt
            .block_on(crate::clpgm::run(&mut job, &pgm, &texts));
        let back = job.calls.returned.take().unwrap_or_default();
        if let Some(c) = self.io.cl.as_mut() {
            if c.user_libl != job.libl || c.curlib != job.curlib {
                c.user_libl = job.libl.clone();
                c.curlib = job.curlib.clone();
                self.io.libl = job.library_list();
            }
        }
        if let crate::commands::Outcome::Messages(msgs) = out? {
            self.messages
                .extend(msgs.into_iter().filter(|x| x.starts_with("DSPLY ")));
        }
        for (l, v) in locs.iter().zip(back) {
            let Some(l) = l else { continue };
            let val = if l.dtype.is_numeric() {
                match Dec::parse(v.trim()) {
                    Ok(d) => Value::Num(d),
                    Err(_) => continue,
                }
            } else {
                let mut b = crate::rpg::data::e(&v);
                let old = &self.bufs[l.buf][l.off..l.off + l.size];
                if b.len() < old.len() {
                    b.extend_from_slice(&old[b.len()..]);
                }
                Value::Char(b)
            };
            let mut buf = self.bufs[l.buf][l.off..l.off + l.size].to_vec();
            data::store(
                &l.dtype,
                &val,
                &mut buf,
                Fit::Truncate { half_adjust: false },
                false,
            )?;
            self.bufs[l.buf][l.off..l.off + l.size].copy_from_slice(&buf);
        }
        Ok(None)
    }

    /// The module a bound call runs: (unit, binding, compiled module).
    fn resolve_bound(
        &mut self,
        sym: &str,
        target: &crate::ile::Target,
        module: Option<&str>,
        client: &'a Client,
        libl: &[String],
    ) -> Result<BoundModule> {
        use crate::ile::Target;
        let (code, unit, binding) = match target {
            // A program's entry module (a procedure pointer into it): the
            // program's own source, its binding.
            Target::Pgm { library, name, .. } => {
                let p = self
                    .block(crate::catalog::find_program(
                        client,
                        std::slice::from_ref(library),
                        name,
                    ))?
                    .ok_or_else(|| anyhow!("MCH3601: program {library}/{name} of a procedure pointer is not there any more."))?;
                let code = crate::ile::ModuleCopy {
                    name: p.name.clone(),
                    source: p.source.clone(),
                    options: p.options.clone(),
                    attribute: p.attribute.clone(),
                };
                (
                    code,
                    format!("PGM {}/{}", p.library, p.name),
                    std::sync::Arc::new(p.binding.clone().unwrap_or_default()),
                )
            }
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
                let (lib, b) = self.block(crate::ile::find_srvpgm(client, libl, library, name))?;
                // Signature checking (ILE Concepts, "Service program
                // activation"): the service program must still have the
                // signature it had when the caller was bound, as its
                // current or a previous (*PRV) signature.
                let bound_sig = self
                    .io
                    .binding
                    .as_ref()
                    .and_then(|x| x.srv_signatures.get(&format!("{lib}/{name}")).cloned());
                if let Some(sig) = bound_sig {
                    if sig != b.signature && !b.prv_signatures.contains(&sig) {
                        bail!("MCH4431: Program signature violation: service program {lib}/{name} no longer has the signature {sig} the caller was bound to; bind the caller again (CRTPGM, UPDPGM).");
                    }
                }
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
                    if module.is_some_and(|x| x != m.name) || m.attribute == "CLLE" {
                        continue;
                    }
                    let p = self.block(super::compile_with(
                        client, libl, &m.name, &m.source, &m.options,
                    ))?;
                    if p.procs
                        .get(sym)
                        .is_some_and(|pi| pi.export || module.is_some())
                    {
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
            libl,
            &code.name,
            &code.source,
            &code.options,
        ))?;
        if !prog
            .procs
            .get(sym)
            .is_some_and(|pi| pi.export || module.is_some())
        {
            bail!("CPD5D02: Module {} does not export {sym}.", code.name);
        }
        let generation = crate::ile::BINDING_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
        Ok((generation, unit, binding, std::sync::Arc::new(prog)))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_bound(
        &mut self,
        _sym: &str,
        target: &crate::ile::Target,
        args: Vec<Arg>,
        unit: String,
        binding: std::sync::Arc<crate::ile::Binding>,
        prog: &Program,
        pi: &ProcInfo,
    ) -> Result<Option<Value>> {
        use crate::ile::Target;
        let client = self.io.client;
        let libl = self.io.libl.clone();
        let srvpgm = matches!(target, Target::SrvPgm { .. });
        let entry_of = match target {
            Target::Pgm {
                library,
                name,
                group,
            } => Some((format!("{library}/{name}"), group.clone())),
            _ => None,
        };
        // A service program runs in its activation group: the caller's
        // (*CALLER) or the one it names; a program's entry module in the
        // group the program is active in.
        let group = if let Some((_, g)) = &entry_of {
            g.clone()
        } else if srvpgm && !binding.actgrp.is_empty() && binding.actgrp != "*CALLER" {
            binding.actgrp.clone()
        } else {
            self.io.actgrp.clone()
        };
        // A module of a service program (called from the service program
        // itself, or from outside it) keeps its storage in the service
        // program's activation.
        let act_unit = if srvpgm || entry_of.is_some() {
            Some(unit.clone())
        } else {
            self.current_srvpgm()
                .map(|(l, sp)| format!("{l}/{sp}/{}", prog.name))
        };
        // The caller's module storage first goes where a call back into
        // that module finds it (its activation, or the program's copy of a
        // bound module's storage), so the callee (or a procedure it calls
        // through a pointer) runs on the same storage; what such a call
        // changed comes back after.
        let reentry = self.reentry_out();
        // The module's global storage: its activation, or its first use.
        let storage = if let Some(u) = &act_unit {
            crate::ile::activation(&self.io.job_key, &group, u)
        } else {
            self.io
                .modules
                .lock()
                .ok()
                .and_then(|m| m.get(&unit).cloned())
        };
        let mut m = Machine::new(
            prog,
            Io {
                rt: self.io.rt.clone(),
                client,
                data: self.io.data,
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
                unit: act_unit.clone(),
                modules: if srvpgm {
                    Default::default()
                } else {
                    self.io.modules.clone()
                },
                actgrp: group.clone(),
                program: match &entry_of {
                    Some((p, _)) => p.clone(),
                    None => format!("*BOUND/{}", prog.name),
                },
                shared: self.io.shared.clone(),
            },
        );
        if let Some(u) = &act_unit {
            m.owner = crate::ile::activation_owner(&self.io.job_key, &group, u);
        }
        // Its files stay open between calls: in the service program's
        // activation, or the program's (a module bound into it).
        let files_key = (
            self.io.job_key.clone(),
            group.clone(),
            act_unit
                .clone()
                .unwrap_or_else(|| format!("{}/{unit}", self.stack_pgm())),
        );
        let kept = MODULE_FILES
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|x| x.remove(&files_key)));
        if let Some((files, printers)) = kept {
            for (f, o) in m.files.iter_mut().zip(files) {
                f.open = o;
            }
            m.printers = printers;
        }
        match storage {
            Some(s) => m.bufs.own = s,
            None => {
                m.user_inz()?;
                // The module's PSDS, set when it is activated.
                m.psds_start();
            }
        }
        // Exported data: the caller's changes go to the shared copy, the
        // callee sees them.
        self.share_out();
        m.share_seed();
        m.share_in();
        let globals = m.bufs.len();
        // A service program module calling out: its global storage goes
        // to its activation first, so a call back into it (through a
        // procedure pointer) runs on the same storage; what that call
        // changed comes back after.
        // (The caller's module storage was put where a call back into it
        // finds it before the callee's storage was fetched: `reentry`.)
        // The caller's storage and the heap are lent to the callee for the
        // call: parameters by reference and pointers address them there.
        let k = self.bufs.lent.len();
        m.bufs.lent = std::mem::take(&mut self.bufs.lent);
        m.bufs.lent.push(super::mem::Lent {
            owner: self.frame_owner,
            act: self.owner,
            globals: self.prog.globals.len(),
            bufs: std::mem::take(&mut self.bufs.own),
        });
        m.bufs.heap = std::mem::take(&mut self.bufs.heap);
        let lent = |b: usize| -> usize {
            if b < super::mem::HEAP_BASE {
                super::mem::LENT_BASE + (k << 24) + b
            } else {
                b
            }
        };
        let args: Vec<Arg> = args
            .into_iter()
            .map(|a| match a {
                Arg::Ref(l) => Arg::Ref(Loc {
                    buf: lent(l.buf),
                    ..l
                }),
                other => other,
            })
            .collect();
        let sql_key = (
            self.io.job_key.clone(),
            group.clone(),
            act_unit.clone().unwrap_or_else(|| unit.clone()),
        );
        if act_unit.is_some() {
            m.sql.restore(&sql_key);
        }
        let r = m.call_proc(pi, args);
        if act_unit.is_some() {
            m.sql.keep(sql_key);
        }
        m.share_out();
        // The caller's storage comes back.
        self.bufs.own = m.bufs.lent.pop().map(|l| l.bufs).unwrap_or_default();
        self.bufs.lent = std::mem::take(&mut m.bufs.lent);
        self.bufs.heap = std::mem::take(&mut m.bufs.heap);
        self.reentry_in(reentry);
        self.share_in();
        self.messages.append(&mut m.messages);
        self.printed.append(&mut m.printed);
        self.logged.append(&mut m.logged);
        let kept: Vec<Vec<u8>> = m.bufs.own[..globals].to_vec();
        // Printed lines go to the spooled file now; the file stays open.
        let names: Vec<String> = m.printers.keys().cloned().collect();
        for n in names {
            m.flush_printer(&n)?;
        }
        let files: Vec<Option<OpenFile>> = m.files.iter_mut().map(|f| f.open.take()).collect();
        let printers = std::mem::take(&mut m.printers);
        if files.iter().any(|f| f.is_some()) || !printers.is_empty() {
            if let Ok(mut g) = MODULE_FILES.lock() {
                g.get_or_insert_with(HashMap::new)
                    .insert(files_key, (files, printers));
            }
        }
        if let Some(u) = &act_unit {
            crate::ile::keep_activation(&self.io.job_key, &group, u, kept);
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
        let mut files = HashMap::new();
        let mut lengths = Vec::new();
        for (i, arg) in args.into_iter().enumerate() {
            // More arguments than the procedure has parameters (a call
            // through a procedure pointer whose prototype lists more): the
            // procedure does not see them; %PARMS still counts them.
            let Some(p) = pi.params.get(i) else {
                passed = i + 1;
                continue;
            };
            lengths.push(match &arg {
                Arg::Ref(l) => {
                    let cur = || {
                        let max = match l.dtype {
                            DType::Varchar(n) => n,
                            _ => 0,
                        };
                        self.bufs
                            .get(l.buf)
                            .and_then(|b| b.get(l.off..l.off + data::var_prefix(max)))
                            .map(|x| data::var_len(max, x))
                            .unwrap_or(0)
                    };
                    Some(match l.dtype {
                        DType::Varchar(n) => (cur(), 4, n as usize),
                        DType::VarUcs2(n) => (cur(), 11, n as usize),
                        DType::Ucs2(n) => (n as usize, 9, n as usize),
                        _ => (l.size, 2, l.size),
                    })
                }
                // A value passed for a CONST or VALUE parameter: the
                // descriptor is the temporary's, of the parameter's type
                // (a VARYING parameter: 2-byte length and characters).
                Arg::Val(v) => {
                    let len = match v {
                        Value::Char(c) => Some(c.len()),
                        Value::Fig(_) => Some(0),
                        _ => None,
                    };
                    match (p.dtype, len) {
                        (DType::Varchar(n), Some(k)) => Some((k.min(n as usize), 4, n as usize)),
                        (DType::VarUcs2(n), Some(k)) => {
                            Some(((k / 2).min(n as usize), 11, n as usize))
                        }
                        // A character value for a character parameter is
                        // passed as it is (OPTIONS(*VARSIZE)): its own length.
                        (_, Some(k)) if matches!(v, Value::Char(_)) => Some((k, 2, k)),
                        (DType::Char(n), Some(_)) => Some((n as usize, 2, n as usize)),
                        _ => None,
                    }
                }
                _ => None,
            });
            match arg {
                Arg::File(f) => {
                    files.insert(p.name.clone(), f);
                    passed = i + 1;
                }
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
            files,
            lengths,
        });
        let pgm = self.stack_pgm();
        let lib = self.stack_lib();
        // The caller's line (a call from this module; a bound call set it
        // in the calling module already).
        if self.line > 0 {
            crate::pgmmsg::stack_line(&self.io.job_key, self.line);
        }
        let ext = self.external_proc_name(&pi.name);
        let module = self.stack_module();
        let stack_id = crate::pgmmsg::stack_push(
            &self.io.job_key,
            (&lib, &pgm),
            &module,
            &ext,
            &self.io.actgrp,
        );
        let saved_routine = std::mem::replace(&mut self.routine, pi.name.clone());
        let saved_line = self.line;
        if !pi.ptr_inz.is_empty() {
            self.ptr_inz(&pi.ptr_inz)?;
        }
        let opened = self.open_local_files(pi);
        let mut r = (|| -> Result<Option<Value>> {
            opened?;
            for (n, v) in values {
                self.assign(
                    &Expr::name(&n),
                    &v,
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
            }
            Ok(match self.guarded(&pi.routines.detail)? {
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
        self.close_local_files(pi);
        crate::pgmmsg::stack_pop(&self.io.job_key, stack_id);
        self.frames.pop();
        self.bufs.truncate(base);
        self.routine = saved_routine;
        self.line = saved_line;
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
    /// The module's global storage (kept in its activation).
    pub fn storage(&self, globals: usize) -> Vec<Vec<u8>> {
        self.bufs.own[..globals].to_vec()
    }

    /// The module's global storage as its activation left it.
    pub fn set_storage(&mut self, s: Vec<Vec<u8>>) {
        self.bufs.own = s;
    }

    /// Call an exported procedure of this module with values (CALLPRC
    /// from a CL procedure): parameters passed by reference come back with
    /// what the procedure left in them; the return value.
    pub fn call_exported(
        &mut self,
        sym: &str,
        args: Vec<Value>,
    ) -> Result<(Vec<Value>, Option<Value>)> {
        let prog: &'a Program = self.prog;
        let pi = prog
            .procs
            .get(sym)
            .filter(|p| p.export)
            .ok_or_else(|| anyhow!("CPD5D02: Module {} does not export {sym}.", prog.name))?;
        if args.len() > pi.params.len() {
            bail!(
                "CPF0001: Procedure {sym} takes {} parameters, {} were passed.",
                pi.params.len(),
                args.len()
            );
        }
        let base = self.bufs.len();
        let mut call_args = Vec::new();
        let mut refs = Vec::new();
        for (v, p) in args.iter().zip(&pi.params) {
            if p.by_value || p.konst {
                call_args.push(Arg::Val(v.clone()));
                refs.push(None);
            } else {
                let mut b = p.dtype.initial();
                let size = b.len();
                // A character variable longer than the parameter (CALLPRC
                // passes the CL variable itself, by reference): the
                // procedure gets all its storage, as through a pointer to
                // the parameter (OPTIONS(*VARSIZE), list parameters).
                match v {
                    Value::Char(c) if c.len() > size && matches!(p.dtype, DType::Char(_)) => {
                        b = c.clone();
                    }
                    _ => data::store(
                        &p.dtype,
                        v,
                        &mut b,
                        Fit::Strict { half_adjust: false },
                        false,
                    )?,
                }
                let idx = self.bufs.len();
                self.bufs.push(b);
                call_args.push(Arg::Ref(Loc {
                    buf: idx,
                    off: 0,
                    size,
                    dtype: p.dtype,
                }));
                refs.push(Some(idx));
            }
        }
        let r = self.call_proc(pi, call_args);
        let mut back = Vec::new();
        for ((r_idx, p), v) in refs.iter().zip(&pi.params).zip(&args) {
            back.push(match r_idx {
                Some(idx) if self.bufs[*idx].len() > p.dtype.initial().len() => {
                    Value::Char(self.bufs[*idx].clone())
                }
                Some(idx) => data::load(&p.dtype, &self.bufs[*idx])?,
                None => v.clone(),
            });
        }
        self.bufs.truncate(base);
        Ok((back, r?))
    }

    /// WRITE, UPDATE or DELETE on file `i` (calculations or cycle
    /// output), with the file's trigger programs (ADDPFTRG), in its
    /// commitment definition or (no commitment control) a transaction of
    /// its own, or the change session of the trigger program running.
    pub(super) fn record_change(
        &mut self,
        i: usize,
        op: FileOp,
        rec: &Record,
        keys: &Option<Vec<String>>,
    ) -> Result<()> {
        let rt = self.io.rt.clone();
        let cmt = self.files[i].cmt.clone();
        let client: &Client = match &cmt {
            Some(d) => &d.client,
            None => self.io.data.unwrap_or(self.io.client),
        };
        let fref = self.fref_of(i)?;
        let triggered = match self.files[i].pftrg {
            Some(t) => t,
            None => {
                let t: bool = rt
                    .block_on(client.query_one(
                        "SELECT as4tainr_svc.has_pftrg($1, $2)",
                        &[&fref.library, &fref.file.name],
                    ))
                    .map(|r| r.get(0))
                    .unwrap_or(false);
                self.files[i].pftrg = Some(t);
                t
            }
        };
        let r: Result<()> = if triggered {
            let own = cmt.is_none() && self.io.data.is_none();
            if own {
                rt.block_on(client.batch_execute("BEGIN"))?;
            }
            let (r, keep) = self.triggered_change(i, op, rec, keys, client, cmt.is_some());
            if own {
                rt.block_on(client.batch_execute(if r.is_ok() || keep {
                    "COMMIT"
                } else {
                    "ROLLBACK"
                }))?;
            }
            r
        } else {
            self.rla_change(i, op, rec, keys, client)
        };
        if r.is_ok() && matches!(op, FileOp::Update | FileOp::Delete) {
            if let Some(f) = self.files[i].open.as_mut() {
                rt.block_on(f.release(client))?;
            }
        }
        r
    }

    /// Make a record change (WRITE, UPDATE, DELETE) on file `i`.
    fn rla_change(
        &mut self,
        i: usize,
        op: FileOp,
        rec: &Record,
        keys: &Option<Vec<String>>,
        client: &Client,
    ) -> Result<()> {
        let rt = self.io.rt.clone();
        let f = self.files[i].open.as_mut().unwrap();
        match op {
            FileOp::Write => rt.block_on(f.write(client, rec)).map(|_| ()),
            FileOp::Update => rt.block_on(f.update(client, rec)),
            _ => rt.block_on(async {
                if let Some(k) = keys {
                    let r = f.chain(client, k).await?;
                    if !r.found {
                        bail!("CPF5013: The record to delete was not found.");
                    }
                }
                f.delete(client).await
            }),
        }
    }

    /// A record change on a file with trigger programs (ADDPFTRG), in the
    /// open transaction of `client` ([`crate::pftrg::Change`]): the
    /// result, and whether to keep the change though it is an error.
    fn triggered_change(
        &mut self,
        i: usize,
        op: FileOp,
        rec: &Record,
        keys: &Option<Vec<String>>,
        client: &Client,
        cmt: bool,
    ) -> (Result<()>, bool) {
        let rt = self.io.rt.clone();
        let catalog = self.io.client;
        let mut ch = match rt.block_on(crate::pftrg::Change::start(client, catalog, cmt)) {
            Ok(c) => c,
            Err(e) => return (Err(e), false),
        };
        let at = self.files[i].open.as_ref().and_then(|f| f.current_rrn());
        let mut r = self.rla_change(i, op, rec, keys, client);
        if rt.block_on(ch.probed(&r)) {
            if let Some(f) = self.files[i].open.as_mut() {
                f.set_current_rrn(at);
            }
            r = self.rla_change(i, op, rec, keys, client);
            let groups = match rt.block_on(ch.trial_made(r.is_ok())) {
                Ok(g) => g,
                Err(e) => return rt.block_on(ch.failed(e)),
            };
            if r.is_err() {
                return (r, false);
            }
            for g in groups {
                let mut new = g[0].new.clone();
                let mut replaced = false;
                for e in &g {
                    let mut e = e.clone();
                    e.new = new.clone();
                    match self.call_trigger(&e, client) {
                        Ok(buf) => {
                            if let Some(img) = crate::pftrg::changed_new_image(&e, &buf) {
                                let nulls = new.as_ref().map(|n| n.1.clone()).unwrap_or_default();
                                new = Some((img, nulls));
                                replaced = true;
                            }
                        }
                        Err(err) => return rt.block_on(ch.failed(err)),
                    }
                }
                if let Err(err) = rt.block_on(ch.redo(&g[0], &new, replaced)) {
                    return rt.block_on(ch.failed(err));
                }
            }
            if let Err(err) = rt.block_on(ch.again()) {
                return rt.block_on(ch.failed(err));
            }
            if let Some(f) = self.files[i].open.as_mut() {
                f.set_current_rrn(at);
            }
            r = self.rla_change(i, op, rec, keys, client);
        }
        let after = match rt.block_on(ch.made(r.is_ok())) {
            Ok(a) => a,
            Err(e) => return rt.block_on(ch.failed(e)),
        };
        if r.is_err() {
            return (r, false);
        }
        for e in &after {
            if let Err(err) = self.call_trigger(e, client) {
                return rt.block_on(ch.failed(err));
            }
        }
        let _ = rt.block_on(ch.done());
        (r, false)
    }

    /// Call a trigger program for a change, in this thread, with its
    /// record changes in session `data` (the change's): the trigger
    /// buffer as it left it. An error is CPF502B.
    fn call_trigger(&mut self, e: &crate::pftrg::Event, data: &Client) -> Result<Vec<u8>> {
        let client = self.io.client;
        let when = crate::pftrg::when(e);
        let p = self
            .block(crate::catalog::find_program(
                client,
                std::slice::from_ref(&e.pgm_lib),
                &e.pgm,
            ))?
            .ok_or_else(|| {
                anyhow!(
                    "CPF502B: Trigger program {}/{} for file {}/{} not found ({when} the change).",
                    e.pgm_lib,
                    e.pgm,
                    e.lib,
                    e.file
                )
            })?;
        if !matches!(p.attribute.as_str(), "RPGLE" | "SQLRPGLE" | "RPG") {
            bail!(
                "CPF502B: Trigger program {}/{} is a {} program; as4tainr calls RPG trigger programs ({when} the change).",
                p.library,
                p.name,
                p.attribute
            );
        }
        if p.name == self.prog.name || self.io.callers.contains(&p.name) {
            bail!(
                "CPF502B: Trigger program {}/{} is already active in the job ({when} the change).",
                p.library,
                p.name
            );
        }
        let libl = self.io.libl.clone();
        let callee = self.block(super::compile_with(
            client, &libl, &p.name, &p.source, &p.options,
        ))?;
        let group = program_group(&callee, &self.io.actgrp);
        let buf = crate::pftrg::buffer(e);
        let mut parms = vec![buf.clone(), (buf.len() as i32).to_be_bytes().to_vec()];
        let rt = self.io.rt.clone();
        let back = rt.block_on(crate::pftrg::set_program(data, &p.name, &p.library));
        let r = {
            let mut m = Machine::new(
                &callee,
                Io {
                    rt: self.io.rt.clone(),
                    client,
                    data: Some(data),
                    user: self.io.user.clone(),
                    job_number: self.io.job_number,
                    libl,
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
                    binding: p.binding.map(std::sync::Arc::new),
                    unit: None,
                    modules: Default::default(),
                    actgrp: group,
                    program: format!("{}/{}", p.library, p.name),
                    shared: Default::default(),
                },
            );
            let r = m.run_bytes(&mut parms);
            self.messages.append(&mut m.messages);
            self.printed.append(&mut m.printed);
            self.logged.append(&mut m.logged);
            self.logged.append(&mut m.logged);
            r
        };
        rt.block_on(crate::pftrg::set_program(data, &back.0, &back.1));
        if let Err(err) = r {
            bail!(
                "CPF502B: Error in trigger program {}/{} for file {}/{} ({when} the change): {err:#}",
                p.library,
                p.name,
                e.lib,
                e.file
            );
        }
        Ok(parms.into_iter().next().unwrap_or_default())
    }

    /// Before a call out of a module (a bound call, a program call, a
    /// command): its global storage goes where a call back into it finds
    /// it (its service program activation, or the program's copy of a
    /// bound module's storage), so the call back runs on the same storage
    /// (ILE Concepts: one static storage per activation).
    fn reentry_out(&mut self) -> Reentry {
        let n = self.prog.globals.len().min(self.bufs.own.len());
        if let Some(u) = self.io.unit.clone() {
            let snap = self.bufs.own[..n].to_vec();
            crate::ile::keep_activation(&self.io.job_key, &self.io.actgrp, &u, snap.clone());
            Some((Some(u), String::new(), snap))
        } else if let Some(m) = self.io.program.strip_prefix("*BOUND/") {
            let key = format!("MODULE {m}");
            let snap = self.bufs.own[..n].to_vec();
            if let Ok(mut g) = self.io.modules.lock() {
                g.insert(key.clone(), snap.clone());
            }
            Some((None, key, snap))
        } else if self.io.program.contains('/') && !self.io.program.starts_with('*') {
            // A program's entry module: where a procedure pointer into it
            // (Target::Pgm) finds its storage.
            let u = format!("PGM {}", self.io.program);
            let snap = self.bufs.own[..n].to_vec();
            crate::ile::keep_activation(&self.io.job_key, &self.io.actgrp, &u, snap.clone());
            Some((Some(u), String::new(), snap))
        } else {
            None
        }
    }

    /// After the call: what a call back changed in the module's storage
    /// comes back.
    fn reentry_in(&mut self, reentry: Reentry) {
        let Some((u, key, snap)) = reentry else {
            return;
        };
        let now = match u {
            Some(u) => crate::ile::activation(&self.io.job_key, &self.io.actgrp, &u),
            None => self
                .io
                .modules
                .lock()
                .ok()
                .and_then(|g| g.get(&key).cloned()),
        };
        if let Some(now) = now {
            for (i, (old, new)) in snap.iter().zip(&now).enumerate() {
                if old != new && old.len() == new.len() {
                    for (j, (o, n)) in old.iter().zip(new).enumerate() {
                        if o != n {
                            self.bufs.own[i][j] = *n;
                        }
                    }
                }
            }
        }
    }

    fn call_program(&mut self, name: &str, locs: &[Loc]) -> Result<()> {
        // A called program sees what this one changed in user spaces
        // through pointers, and this one sees what it changed.
        self.spaces_flush()?;
        crate::pgmmsg::stack_line(&self.io.job_key, self.line);
        let reentry = self.reentry_out();
        let r = self.call_program_inner(name, locs);
        self.reentry_in(reentry);
        self.spaces_reload()?;
        r
    }

    /// User spaces this program addresses through pointers, written back.
    fn spaces_flush(&mut self) -> Result<()> {
        for (buf, lib, name, len) in self.bufs.heap.spaces.clone() {
            let data = self.bufs[buf][..len.min(self.bufs[buf].len())].to_vec();
            self.block(crate::usrspc::put_contents(
                self.io.client,
                &lib,
                &name,
                &data,
            ))?;
        }
        Ok(())
    }

    /// User spaces this program addresses, read again (a call may have
    /// changed them; a deleted one keeps its last bytes).
    fn spaces_reload(&mut self) -> Result<()> {
        for (i, (buf, lib, name, _)) in self.bufs.heap.spaces.clone().into_iter().enumerate() {
            let caller = crate::dtaq::Caller {
                libl: vec![lib.clone()],
                ..Default::default()
            };
            if let Ok(s) = self.block(crate::usrspc::find(self.io.client, &name, &lib, &caller)) {
                let data = self.block(crate::usrspc::contents(self.io.client, &s))?;
                self.bufs.heap.spaces[i].3 = data.len();
                self.bufs[buf] = space_pages(data, s.initval);
            }
        }
        Ok(())
    }

    fn call_program_inner(&mut self, name: &str, locs: &[Loc]) -> Result<()> {
        // A qualified name, LIB/PGM (EXTPGM of a variable holding
        // 'LIB/PGM'; ILE RPG Reference, EXTPGM): that library, *LIBL the
        // library list.
        let (qual, name) = match name.split_once('/') {
            Some((l, n)) => (Some(l.trim().to_ascii_uppercase()), n.trim()),
            None => (None, name),
        };
        if self.system_program(name, locs)? {
            return Ok(());
        }
        let active = name == self.prog.name || self.io.callers.iter().any(|c| c == name);
        let client = self.io.client;
        let libl = self.io.libl.clone();
        let search = match &qual {
            Some(l) if l != "*LIBL" => vec![crate::qtemp::real(l)],
            _ => libl.clone(),
        };
        let found = self.block(crate::catalog::find_program(client, &search, name))?;
        let program_lib = found
            .as_ref()
            .map(|p| p.library.clone())
            .unwrap_or_else(|| "*NONE".into());
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
                // A CL program runs in the job; the values it leaves in its
                // parameters come back (by reference).
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
                let back =
                    self.run_cl_returning(&format!("CALL PGM({}/{}){parm}", p.library, p.name))?;
                for (l, v) in locs.iter().zip(back.unwrap_or_default()) {
                    let val = if l.dtype.is_numeric() {
                        match Dec::parse(v.trim()) {
                            Ok(d) => Value::Num(d),
                            Err(_) => continue,
                        }
                    } else {
                        // A shorter value changes only the first bytes.
                        let mut b = crate::rpg::data::e(&v);
                        let old = &self.bufs[l.buf][l.off..l.off + l.size];
                        if b.len() < old.len() {
                            b.extend_from_slice(&old[b.len()..]);
                        }
                        Value::Char(b)
                    };
                    let mut buf = self.bufs[l.buf][l.off..l.off + l.size].to_vec();
                    data::store(
                        &l.dtype,
                        &val,
                        &mut buf,
                        Fit::Truncate { half_adjust: false },
                        false,
                    )?;
                    self.bufs[l.buf][l.off..l.off + l.size].copy_from_slice(&buf);
                }
                return Ok(());
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
        let ovr = self
            .io
            .cl
            .as_ref()
            .map(|c| c.overrides.clone())
            .unwrap_or_default();
        let callee = self.block(super::compile_ovr(
            client, &libl, name, &source, &options, &ovr,
        ))?;
        // An active program with an RPG cycle cannot be called again (ILE
        // RPG Reference: a cycle-main program is not recursive); one with
        // a linear main procedure (MAIN) can, as procedures can.
        if active && callee.main.is_none() {
            bail!("Program {name} called recursively: it is already active in the job.");
        }
        let group = program_group(&callee, &self.io.actgrp);
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
                    data: self.io.data,
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
                    actgrp: group,
                    program: format!("{}/{name}", program_lib),
                    shared: Default::default(),
                },
            );
            // This program's storage (and what was lent to it) is lent to
            // the called program: a pointer it is given (in a parameter,
            // or kept by a service program) still addresses storage of
            // the programs and procedures active above it.
            m.bufs.lent = std::mem::take(&mut self.bufs.lent);
            m.bufs.lent.push(super::mem::Lent {
                owner: self.frame_owner,
                act: self.owner,
                globals: self.prog.globals.len(),
                bufs: std::mem::take(&mut self.bufs.own),
            });
            // In the same activation group, the same heap.
            let share_heap = m.io.actgrp == self.io.actgrp;
            if share_heap {
                m.bufs.heap = std::mem::take(&mut self.bufs.heap);
                m.heap_lent = true;
            }
            let r = m.run_bytes(&mut parms);
            self.bufs.own = m.bufs.lent.pop().map(|l| l.bufs).unwrap_or_default();
            self.bufs.lent = std::mem::take(&mut m.bufs.lent);
            if share_heap {
                self.bufs.heap = std::mem::take(&mut m.bufs.heap);
            }
            self.messages.append(&mut m.messages);
            self.printed.append(&mut m.printed);
            self.logged.append(&mut m.logged);
            self.logged.append(&mut m.logged);
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
        for r in &prog.outputs {
            // A line conditioned by the overflow indicator while it is
            // on is an overflow line: the overflow routine prints it.
            if kinds.contains(&r.kind) && !self.overflow_line(r) && self.out_cond(&r.cond) {
                self.print_record(r)?;
            }
        }
        Ok(())
    }

    /// A heading, detail or total line conditioned by its printer's
    /// overflow indicator, which is on.
    fn overflow_line(&self, r: &OutRec) -> bool {
        if !matches!(r.kind, 'H' | 'D' | 'T') {
            return false;
        }
        let Some(of) = self.overflow_ind(&r.file).map(|o| ind_name(&o)) else {
            return false;
        };
        self.ind(&of)
            && r.cond
                .iter()
                .any(|g| g.iter().any(|(neg, i)| !neg && ind_name(i) == of))
    }

    /// The overflow routine (ILE RPG Reference, "Overflow Routine"): for
    /// each printer file (or the one given, for fetch overflow) whose
    /// overflow indicator is on, the heading, detail and total lines
    /// conditioned by it whose conditions are met; once each time
    /// overflow occurs.
    pub(super) fn overflow_routine(&mut self, only: Option<&str>) -> Result<()> {
        let prog = self.prog;
        let files: Vec<String> = self
            .ofl_inds
            .iter()
            .filter(|(ind, f)| {
                self.ind(ind) && !self.ofl_written.contains(f) && only.is_none_or(|o| o == f)
            })
            .map(|(_, f)| f.clone())
            .collect();
        for f in files {
            self.ofl_written.insert(f.clone());
            for r in &prog.outputs {
                if r.file == f && self.overflow_line(r) && self.out_cond(&r.cond) {
                    self.print_line(r)?;
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
        // Fetch overflow (F in position 18): the overflow lines first,
        // when overflow has occurred (not on a line conditioned by the
        // overflow indicator itself).
        if r.fetch_overflow {
            let of = self.overflow_ind(&r.file).map(|o| ind_name(&o));
            let on_of = of.as_ref().is_some_and(|of| {
                r.cond
                    .iter()
                    .any(|g| g.iter().any(|(_, i)| ind_name(i) == *of))
            });
            if !on_of {
                let file = r.file.clone();
                self.overflow_routine(Some(&file))?;
            }
        }
        self.print_line(r)
    }

    fn print_line(&mut self, r: &OutRec) -> Result<()> {
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
        // RPG III line counter specification); otherwise the printer file
        // object's (CRTPRTF PAGESIZE, OVRFLW); 66 and 60 without one.
        let (page_length, overflow_line) = self.form_of(i);
        let _ = (DEFAULT_LEN, DEFAULT_OFL);
        self.ensure_printer(&r.file)?;
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
        if let Some(id) = p.spl {
            let client = self.io.client;
            self.io
                .rt
                .block_on(crate::spool::write(client, id, &lines))?;
        }
        match (p.spl, p.echo) {
            (Some(_), false) => {}
            _ => {
                for (page, line, text) in lines {
                    if page > p.out_pos.0 {
                        self.printed.push("\x0c".to_string());
                        p.out_pos = (page, 0);
                    }
                    // A line printed again (no spacing between): the new
                    // text over the old, the old showing through its
                    // blanks (PUB400 2026-10-23).
                    if line == p.out_pos.1 && line > 0 {
                        if let Some(last) = self.printed.last_mut() {
                            *last = overprint(last, &text);
                            continue;
                        }
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

/// A GOTO to a TAG outside the subroutine running.
#[derive(Debug)]
pub(super) struct GotoSignal(pub String);

impl std::fmt::Display for GotoSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GOTO {}: TAG not found", self.0)
    }
}

impl std::error::Error for GotoSignal {}

/// ENDSR with a return point (*DETL, *GETIN, *TOTC, *TOTL, *OFL, *DETC,
/// *CANCL): where the cycle goes on.
#[derive(Debug)]
pub(super) struct ReturnPointSignal(pub String);

impl std::fmt::Display for ReturnPointSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ENDSR return point {}", self.0)
    }
}

impl std::error::Error for ReturnPointSignal {}

#[derive(Debug)]
struct LeaveSrSignal;

impl std::fmt::Display for LeaveSrSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LEAVESR")
    }
}

impl std::error::Error for LeaveSrSignal {}

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
    // X: the digits unedited, a negative value's sign in the last digit as
    // in zoned decimal (IBM i 7.5: %EDITC(-1234.56: 'X') of a 9,2 field is
    // '00012345O').
    if code == 'X' {
        if d.is_negative() {
            let last = raw.as_bytes()[raw.len() - 1] - b'0';
            let zoned = b"}JKLMNOPQR"[last as usize] as char;
            return format!("{}{zoned}", &raw[..raw.len() - 1]);
        }
        return raw;
    }
    // Z: every digit, the decimal positions too, leading zeros blanked, no
    // decimal point and no sign (IBM i 7.5: -1234.56 in 9,2 is '   123456').
    if code == 'Z' {
        let all = format!("{int}{frac}");
        let t = all.trim_start_matches('0');
        return format!("{:>w$}", t, w = digits as usize);
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
    // N-Q: a position for the floating minus sign, blank when the value is
    // not negative (IBM i 7.5: %EDITC(0: 'N') of a 9,2 field is 13
    // positions, '          .00').
    let lead = match (matches!(code, 'N' | 'O' | 'P' | 'Q'), neg) {
        (true, true) => "-",
        (true, false) => " ",
        _ => "",
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

/// The call stack in format CSTK0100 (Retrieve Call Stack API), the
/// most recent entry first. Header: bytes returned and available, the
/// number of entries, the offset to the first, the number returned,
/// thread ID (8) and information status (' ': every entry complete).
/// Each entry: its length, the displacement to and number of statement
/// IDs, the displacement to and length of the procedure name, request
/// level, program and library, MI instruction number, module and
/// library, control boundary ('1' or '0'), activation group number and
/// name, ASP names and numbers, then the statement ID and the name.
/// Statement IDs are the source sequence number with two decimals,
/// zero-padded to 10 (OPTION(*SRCSTMT); RPGUnit's ASSERTT test expects
/// '0000008500' for line 85).
fn cstk0100(stack: &[crate::pgmmsg::Entry]) -> Vec<u8> {
    const HDR: usize = 32;
    const FIXED: usize = 128;
    let field = |s: &str, n: usize| {
        let mut b = data::e(s);
        b.resize(n, 0x40);
        b.truncate(n);
        b
    };
    let i4 = |n: usize| (n as i32).to_be_bytes();
    // Activation group numbers: the default group 2, others numbered
    // from 3 as they first appear (fidelity-gaps).
    let mut groups: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for (i, e) in stack.iter().enumerate().rev() {
        let default = e.actgrp.is_empty() || e.actgrp == crate::ile::DEFAULT_GROUP;
        let number = if default {
            2
        } else {
            match groups.iter().position(|g| *g == e.actgrp) {
                Some(p) => p + 3,
                None => {
                    groups.push(e.actgrp.clone());
                    groups.len() + 2
                }
            }
        };
        let boundary = i == 0 || stack[i - 1].actgrp != e.actgrp;
        let stmt = if e.line > 0 {
            Some(format!("{:010}", e.line * 100))
        } else {
            None
        };
        let name = data::e(&e.proc);
        let stmt_len = if stmt.is_some() { 10 } else { 0 };
        let total = (FIXED + stmt_len + name.len()).div_ceil(4) * 4;
        let mut b = Vec::with_capacity(total);
        b.extend(i4(total));
        b.extend(i4(if stmt.is_some() { FIXED } else { 0 }));
        b.extend(i4(stmt.is_some() as usize));
        b.extend(i4(FIXED + stmt_len));
        b.extend(i4(name.len()));
        b.extend(i4(0));
        b.extend(field(&e.pgm, 10));
        b.extend(field(&e.lib, 10));
        b.extend(i4(0));
        b.extend(field(&e.module, 10));
        b.extend(field(&e.lib, 10));
        b.extend(field(if boundary { "1" } else { "0" }, 1));
        b.extend([0u8; 3]);
        b.extend((number as u32).to_be_bytes());
        let group = if default {
            "*DFTACTGRP"
        } else {
            e.actgrp.as_str()
        };
        b.extend(field(group, 10));
        b.extend([0u8; 2]);
        b.extend(field("*SYSBAS", 10));
        b.extend(field("*SYSBAS", 10));
        b.extend(i4(1));
        b.extend(i4(1));
        b.extend((number as u64).to_be_bytes());
        b.resize(FIXED, 0);
        if let Some(s) = stmt {
            b.extend(field(&s, 10));
        }
        b.extend(name);
        b.resize(total, 0);
        entries.push(b);
    }
    let mut b = Vec::new();
    let total = HDR + entries.iter().map(|e| e.len()).sum::<usize>();
    b.extend(i4(total));
    b.extend(i4(total));
    b.extend(i4(entries.len()));
    b.extend(i4(HDR));
    b.extend(i4(entries.len()));
    b.extend([0u8; 8]);
    b.extend(field(" ", 1));
    b.resize(HDR, 0);
    for e in entries {
        b.extend(e);
    }
    b
}

/// How much of a CSTK0100 list fits in `room` bytes: whole entries only;
/// (bytes, entries).
fn cstk_fit(b: &[u8], room: usize) -> (usize, usize) {
    let mut at = 32;
    let mut n = 0;
    while at + 4 <= b.len() {
        let len = i32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        if at + len > room {
            break;
        }
        at += len;
        n += 1;
    }
    (at.min(b.len()), n)
}

/// A user space's storage as a program addresses it: whole 4096-byte
/// pages (MI allocates space objects in pages, so a pointer may reach a
/// little past the size asked for; the bytes past it are the initial
/// value and are not kept: fidelity-gaps).
fn space_pages(mut data: Vec<u8>, initval: u8) -> Vec<u8> {
    // Whole pages, and one more: a based item read a little past the
    // space's end (iRPGUnit reads a procedure name as CHAR(256) at the
    // end of its QBNLSPGM list) stays in the space's segment on IBM i.
    let n = data.len().div_ceil(4096).max(1) * 4096 + 4096;
    data.resize(n, initval);
    data
}

/// EBCDIC bytes as text (a CL parameter's value).
fn a_text(b: &[u8]) -> String {
    data::a(b)
}

/// The messages QGYOLMSG lists, the fields to return and the most of a
/// message's text to return. Message selection information: list
/// direction (*NEXT or *PRV), 2 reserved, severity criteria, maximum
/// message and help lengths, offset to and number of the selection
/// criteria (CHAR(10) each: *ALL, *MNR, *MNNR, *SCNMNR...), offset to
/// the starting message keys, offset to and number of the field
/// identifiers (Open List of Messages API).
fn lstm_select(
    mut msgs: Vec<(i64, crate::jobs::Message)>,
    s: &[u8],
) -> (Vec<(i64, crate::jobs::Message)>, Vec<i32>, usize) {
    let txt = |a: usize, n: usize| -> String {
        s.get(a..a + n)
            .map(|b| data::a(b).trim().to_ascii_uppercase())
            .unwrap_or_default()
    };
    let i4 = |a: usize| -> i32 {
        s.get(a..a + 4)
            .map(|b| i32::from_be_bytes(b.try_into().unwrap()))
            .unwrap_or(0)
    };
    let direction = txt(0, 10);
    let severity = i4(12);
    let max_len = i4(16);
    let (sel_off, sel_cnt) = (i4(24).max(0) as usize, i4(28).max(0) as usize);
    let key_off = i4(32).max(0) as usize;
    let (fld_off, fld_cnt) = (i4(36).max(0) as usize, i4(40).max(0) as usize);
    let criteria: Vec<String> = (0..sel_cnt).map(|k| txt(sel_off + 10 * k, 10)).collect();
    let fields: Vec<i32> = (0..fld_cnt).map(|k| i4(fld_off + 4 * k)).collect();
    let sev = |m: &crate::jobs::Message| if m.kind == "*INQ" { 99 } else { 0 };
    msgs.retain(|(_, m)| {
        sev(m) >= severity
            && criteria.iter().any(|c| match c.as_str() {
                "*MNR" => m.kind == "*INQ",
                "*MNNR" => m.kind != "*INQ",
                _ => true,
            })
    });
    if direction == "*PRV" {
        msgs.reverse();
    }
    // The starting key: the oldest or newest, or a message's key.
    let start = s
        .get(key_off..key_off + 4)
        .map(|b| b.to_vec())
        .unwrap_or_default();
    if start.len() == 4 && start != [0xFF; 4] && start != [0; 4] {
        let k = i64::from(u32::from_be_bytes(start.try_into().unwrap()));
        if let Some(p) = msgs.iter().position(|(_, m)| m.id == k) {
            msgs.drain(..p);
        }
    }
    let max_len = if max_len < 0 {
        usize::MAX
    } else {
        max_len as usize
    };
    (msgs, fields, max_len)
}

/// One message in format LSTM0100, at `base` in the receiver (offsets
/// are from the receiver's start): offset to the next entry, offset to
/// and number of the fields returned, severity, message ID, type, key,
/// message file and library, message queue and library, date (CYYMMDD),
/// time and microseconds sent, 10 more bytes; then the fields as
/// QGYOLJBL returns them.
fn lstm0100(
    us: i64,
    m: &crate::jobs::Message,
    queue: &(String, String),
    fields: &[i32],
    max_len: usize,
    base: usize,
) -> Vec<u8> {
    let field = |s: &str, n: usize| {
        let mut b = data::e(s);
        b.resize(n, 0x40);
        b.truncate(n);
        b
    };
    let ((y, mo, d), (h, mi, sec), usec) = crate::rpg::dates::from_epoch_us(us);
    let mut b = Vec::new();
    b.extend([0u8; 4]);
    b.extend([0u8; 4]);
    b.extend((fields.len() as i32).to_be_bytes());
    b.extend((if m.kind == "*INQ" { 99i32 } else { 0 }).to_be_bytes());
    b.extend(field(&m.msgid, 7));
    b.extend(field(crate::pgmmsg::rtntype(&m.kind), 2));
    b.extend((m.id as u32).to_be_bytes());
    b.extend(field("", 20));
    b.extend(field(&queue.0, 10));
    b.extend(field(&queue.1, 10));
    b.extend(field(
        &format!("{}{:02}{mo:02}{d:02}", u8::from(y >= 2000), y % 100),
        7,
    ));
    b.extend(field(&format!("{h:02}{mi:02}{sec:02}"), 6));
    b.extend(field(&format!("{usec:06}"), 6));
    b.extend([0u8; 10]);
    b.resize(b.len().div_ceil(4) * 4, 0);
    if !fields.is_empty() {
        let at = (base + b.len()) as i32;
        b[4..8].copy_from_slice(&at.to_be_bytes());
    }
    // An impromptu message's text is kept with its message ID in front
    // ("CPF1234: text") when it has one.
    let text = crate::jobs::shown_text(&m.text);
    let mut text = data::e(text);
    text.truncate(max_len);
    for id in fields {
        let value: Vec<u8> = match id {
            201 | 301 | 302 => text.clone(),
            // Reply status: N (accepts no reply), W (waiting for one).
            1001 => data::e(if m.kind == "*INQ" { "W" } else { "N" }),
            607 => field(&m.from_user, 10),
            _ => Vec::new(),
        };
        let total = (32 + value.len()).div_ceil(4) * 4;
        let next = base + b.len() + total;
        let mut f = Vec::with_capacity(total);
        f.extend((next as i32).to_be_bytes());
        f.extend((total as i32).to_be_bytes());
        f.extend(id.to_be_bytes());
        f.extend(data::e("C"));
        f.extend(data::e(" "));
        f.extend([0u8; 14]);
        f.extend((value.len() as i32).to_be_bytes());
        f.extend(value);
        f.resize(total, 0);
        b.extend(f);
    }
    let next = (base + b.len()) as i32;
    b[0..4].copy_from_slice(&next.to_be_bytes());
    b
}

/// The job log messages QGYOLJBL lists, and the fields to return.
/// Message selection information: list direction (*NEXT or *PRV),
/// qualified job name ('*': this job), internal job identifier,
/// starting message key (x'00000000' the oldest, x'FFFFFFFF' the
/// newest), maximum message and help lengths, offset to and number of
/// the field identifiers to return, offset to and length of the call
/// message queue name ('*': all) (Open List of Job Log Messages API).
type OljlSelection = (Vec<crate::pgmmsg::QMsg>, Vec<i32>, (usize, usize));

fn oljl_select(job: &str, s: &[u8]) -> Result<OljlSelection> {
    let txt = |a: usize, n: usize| -> String {
        s.get(a..a + n)
            .map(|b| data::a(b).trim().to_string())
            .unwrap_or_default()
    };
    let i4 = |a: usize| -> i32 {
        s.get(a..a + 4)
            .map(|b| i32::from_be_bytes(b.try_into().unwrap()))
            .unwrap_or(0)
    };
    let direction = txt(0, 10).to_ascii_uppercase();
    let qjob = txt(10, 26);
    if !qjob.starts_with('*') {
        bail!("QGYOLJBL of a job other than '*' is not supported");
    }
    let start = s.get(52..56).map(|b| b.to_vec()).unwrap_or_default();
    // The most of a message's text and help returned (-1: all of it).
    let most = |n: i32| if n < 0 { usize::MAX } else { n as usize };
    let max_len = (most(i4(56)), most(i4(60)));
    let fld_off = i4(64).max(0) as usize;
    let fld_cnt = i4(68).max(0) as usize;
    let fields: Vec<i32> = (0..fld_cnt).map(|k| i4(fld_off + 4 * k)).collect();
    let mut msgs = crate::pgmmsg::job_log(job);
    if direction == "*PRV" {
        msgs.reverse();
    }
    // The starting key: the oldest or newest, or a message's key.
    if start != [0xFF; 4] && start != [0; 4] {
        let k = crate::pgmmsg::key_from_bytes(&start);
        if let Some(p) = msgs.iter().position(|m| Some(m.key) == k) {
            msgs.drain(..p);
        }
    }
    Ok((msgs, fields, max_len))
}

/// One job log message in format OLJL0100, at `base` in the receiver
/// (offsets are from the receiver's start): offset to the next entry,
/// offset to and number of the fields returned, severity, message ID,
/// type, key, message file and library, date (CYYMMDD), time and
/// microseconds sent, thread; then each field: offset to the next,
/// length of its information, identifier, type of data ('C'), status
/// (' '), 14 reserved, length of the data, the data.
fn oljl0100(
    m: &crate::pgmmsg::QMsg,
    fields: &[i32],
    max_len: (usize, usize),
    base: usize,
) -> Vec<u8> {
    let cut = |mut b: Vec<u8>, n: usize| {
        b.truncate(n);
        b
    };
    let field = |s: &str, n: usize| {
        let mut b = data::e(s);
        b.resize(n, 0x40);
        b.truncate(n);
        b
    };
    let mut b = Vec::new();
    b.extend([0u8; 4]);
    b.extend([0u8; 4]);
    b.extend((fields.len() as i32).to_be_bytes());
    b.extend(m.sev.to_be_bytes());
    b.extend(field(&m.msgid, 7));
    b.extend(field(crate::pgmmsg::rtntype(&m.ty), 2));
    b.extend(crate::pgmmsg::key_bytes(m.key));
    b.extend(field(&m.msgf, 10));
    b.extend(field(&m.msgflib, 10));
    b.extend(field(&m.sent.0, 7));
    b.extend(field(&m.sent.1, 6));
    b.extend(field(if m.usec.is_empty() { "000000" } else { &m.usec }, 6));
    b.extend([0u8; 8]);
    b.resize(b.len().div_ceil(4) * 4, 0);
    if !fields.is_empty() {
        let at = (base + b.len()) as i32;
        b[4..8].copy_from_slice(&at.to_be_bytes());
    }
    for id in fields {
        let value: Vec<u8> = match id {
            // Replacement data.
            201 => m.data.clone(),
            // Message text, with or without its data replaced.
            301 | 302 => cut(data::e(&m.text), max_len.0),
            // Help text.
            401..=404 => cut(data::e(&m.seclvl), max_len.1),
            // Sending program, module and procedure names.
            603 => field(&m.sender.pgm, 12),
            604 => field(&m.sender.module, 10),
            605 => data::e(&m.sender.proc),
            // Sending program library; receiving program, module,
            // procedure and library (QGYOLJBL field identifiers).
            608 => field(&m.sender.lib, 10),
            703 => field(&m.receiver.pgm, 10),
            704 => field(&m.receiver.module, 10),
            705 => data::e(&m.receiver.proc),
            708 => field(&m.receiver.lib, 10),
            _ => Vec::new(),
        };
        let total = (32 + value.len()).div_ceil(4) * 4;
        let next = base + b.len() + total;
        let mut f = Vec::with_capacity(total);
        f.extend((next as i32).to_be_bytes());
        f.extend((total as i32).to_be_bytes());
        f.extend(id.to_be_bytes());
        f.extend(data::e("C"));
        f.extend(data::e(" "));
        f.extend([0u8; 14]);
        f.extend((value.len() as i32).to_be_bytes());
        f.extend(value);
        f.resize(total, 0);
        b.extend(f);
    }
    let next = (base + b.len()) as i32;
    b[0..4].copy_from_slice(&next.to_be_bytes());
    b
}

/// A number in character form (%DEC, %INT): blanks, or anything but a
/// number with an optional sign and decimal point, is an error (status
/// 00105, ILE RPG Reference; RPGUnit's CMDRUNLOG relies on %INT of
/// blanks failing).
fn char_number(t: &str) -> Result<Dec> {
    if t.trim().is_empty() {
        bail!("RNQ0105: The character value '{t}' is not a number.");
    }
    Dec::parse(t).map_err(|_| {
        anyhow!(
            "RNQ0105: The character value '{}' is not a number.",
            t.trim()
        )
    })
}

/// Text printed over a line: its characters, except its blanks, over
/// the line's.
fn overprint(old: &str, new: &str) -> String {
    let mut o: Vec<char> = old.chars().collect();
    for (i, c) in new.chars().enumerate() {
        if i >= o.len() {
            o.push(c);
        } else if c != ' ' {
            o[i] = c;
        }
    }
    o.into_iter().collect()
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
        // DECEDIT('0,'): a decimal comma and a zero before it.
        assert_eq!(
            char_of_num_edit(&Dec::parse("0.50").unwrap(), ',', true),
            "0,50"
        );
        assert_eq!(
            char_of_num_edit(&Dec::parse("-123.00").unwrap(), ',', true),
            "-123,00"
        );
        assert_eq!(
            char_of_num_edit(&Dec::parse("0.50").unwrap(), ',', false),
            ",50"
        );
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
        // Observed on IBM i 7.5 (PUB400, tools/golden/rpg-sql.txt): X keeps
        // a negative sign in the last digit, Z keeps the decimal digits.
        assert_eq!(
            edit_code(&Dec::parse("-1234.56").unwrap(), 9, 2, 'X'),
            "00012345O"
        );
        assert_eq!(
            edit_code(&Dec::parse("-1234.56").unwrap(), 9, 2, 'Z'),
            "   123456"
        );
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
