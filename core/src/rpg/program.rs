//! Binding a parsed source to a runnable program: names, types and the
//! storage layout (ILE RPG Reference, "Defining Data and Prototypes").
//! External files are described here, as the compiler does: their
//! fields become program fields.

use super::ast::*;
use super::code::{self, Routines};
use super::data::{self, DType, Fig, Value};
use super::dec::Dec;
use super::expr;
use crate::catalog::FileRef;
use crate::dds::DataType;
use crate::dspf::DisplayFile;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Where a variable's bytes are: a storage buffer and an offset.
#[derive(Clone, Debug, PartialEq)]
pub struct VarInfo {
    pub dtype: DType,
    pub buf: usize,
    pub off: usize,
    /// Number of elements for an array (DIM), 0 for a scalar.
    pub dim: u32,
    /// Distance between elements.
    pub stride: usize,
    /// Local to a procedure (a frame buffer) or global.
    pub local: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DsInfo {
    pub buf: usize,
    pub off: usize,
    pub size: usize,
    pub dim: u32,
    pub qualified: bool,
    pub subfields: Vec<String>,
    pub local: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Sym {
    Var(VarInfo),
    Const(Value),
    Ds(DsInfo),
    Klist(Vec<Expr>),
}

/// A file as the program sees it.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub decl: FileDecl,
    /// The database file, for DISK files.
    pub fref: Option<FileRef>,
    /// Record format name.
    pub format: String,
    /// The record's fields (program field names, in record order).
    pub fields: Vec<String>,
    /// The display file, for WORKSTN files.
    pub display: Option<Display>,
    /// A program-described DISK file: its records are byte images read
    /// by the positions on the input specifications.
    pub program_described: bool,
    /// Where each field of the database record is in the record image
    /// (type in the record, offset), and the record length.
    pub layout: Vec<(DType, usize)>,
    /// The record length (database files: the fields in their stored
    /// form), and the length of the key.
    pub record_len: usize,
    pub key_len: usize,
    /// The input specifications for this file's records.
    pub inputs: Vec<InputRec>,
}

/// A record type (program-described) or record format (externally
/// described) as the input specifications describe it.
#[derive(Clone, Debug)]
pub struct InputRec {
    pub record_ind: Option<String>,
    pub codes: Vec<Vec<RecCode>>,
    pub fields: Vec<InputField>,
}

#[derive(Clone, Debug)]
pub struct InputField {
    /// The program field.
    pub name: String,
    /// Program-described: positions in the record and their format.
    pub from: usize,
    pub to: usize,
    pub dtype: Option<DType>,
    pub level: Option<String>,
    pub matching: Option<String>,
    pub plus: Option<String>,
    pub minus: Option<String>,
    pub zero: Option<String>,
}

/// The type of a program-described input field as stored in the record
/// (ILE RPG Reference, input specifications position 36).
fn input_type(f: &InField) -> Result<DType> {
    let len = f.to + 1 - f.from;
    Ok(match (f.format, f.decimals) {
        ('P', d) => DType::Packed(len * 2 - 1, d.unwrap_or(0)),
        ('B', d) => DType::Binary(if len <= 2 { 4 } else { 9 }, d.unwrap_or(0)),
        ('I', _) => DType::Int(match len {
            1 => 3,
            2 => 5,
            4 => 10,
            _ => 20,
        }),
        ('U', _) => DType::Uns(match len {
            1 => 3,
            2 => 5,
            4 => 10,
            _ => 20,
        }),
        (' ' | 'S', Some(d)) => DType::Zoned(len, d),
        (' ' | 'A', None) => DType::Char(len),
        (c, _) => bail!(
            "input field {}: data format {c} is not supported yet",
            f.name
        ),
    })
}

/// A display (WORKSTN) file as the program sees it.
#[derive(Clone, Debug)]
pub struct Display {
    /// Library the display file was found in (for the authority check).
    pub library: String,
    pub file: Arc<DisplayFile>,
    /// Record formats: (format, [(program field, display field)]).
    pub formats: Vec<(String, Vec<(String, String)>)>,
    /// SFILE(format:rrn): the relative record number field of each
    /// subfile record format.
    pub sfile: HashMap<String, String>,
    /// INDDS(ds): indicators 01-99 are positions 1-99 of this data
    /// structure instead of *IN01-*IN99.
    pub indds: Option<String>,
}

impl Display {
    pub fn fields(&self, format: &str) -> Option<&[(String, String)]> {
        self.formats
            .iter()
            .find(|f| f.0 == format)
            .map(|f| f.1.as_slice())
    }
}

/// A parameter of a prototype or procedure interface.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    /// Its type; a data structure parameter (LIKEDS) is its bytes.
    pub dtype: DType,
    /// VALUE: a copy is passed.
    pub by_value: bool,
    /// CONST: read only (passed as a copy of the value here).
    pub konst: bool,
    /// OPTIONS(*NOPASS), OPTIONS(*OMIT).
    pub nopass: bool,
    pub omit: bool,
    /// In a procedure interface: the buffer the parameter is bound to
    /// (aliased to the caller's storage when passed by reference).
    pub buf: Option<usize>,
}

/// A prototype (DCL-PR / PR): what a call passes and where it goes.
#[derive(Clone, Debug)]
pub struct Proto {
    pub name: String,
    /// EXTPGM: a program call.
    pub extpgm: Option<String>,
    /// EXTPROC, or the prototype name: a procedure.
    pub extproc: Option<String>,
    pub params: Vec<ParamInfo>,
    pub returns: Option<DType>,
    /// OVERLOAD(a : b ...): the prototypes a call of this one may mean,
    /// chosen by the arguments (7.5).
    pub overload: Vec<String>,
}

/// A procedure of the program (DCL-PROC / P specs).
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub name: String,
    /// Local names (variables with `local` set: their buffers are the
    /// frame's), constants and key lists.
    pub syms: HashMap<String, Sym>,
    /// Initial contents of the frame buffers of a call.
    pub frame: Vec<Vec<u8>>,
    pub params: Vec<ParamInfo>,
    pub returns: Option<DType>,
    pub routines: Routines,
    /// ON-EXIT: its indicator variable (on when the procedure ends
    /// abnormally) and its code, run however the procedure ends.
    pub on_exit: Option<(Option<String>, Routines)>,
    /// EXPORT: other modules and programs can call it.
    pub export: bool,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub name: String,
    pub syms: HashMap<String, Sym>,
    /// Initial contents of the global storage buffers.
    pub globals: Vec<Vec<u8>>,
    pub files: Vec<FileInfo>,
    pub routines: Routines,
    pub entry: Vec<Parm>,
    pub plists: Vec<(String, Vec<Parm>)>,
    pub outputs: Vec<OutRec>,
    pub control: Vec<Keyword>,
    /// Data areas: program field → data area name (DEFINE *DTAARA, or
    /// the DTAARA keyword).
    pub dtaaras: HashMap<String, String>,
    /// Prototypes, by name.
    pub protos: HashMap<String, Proto>,
    /// Procedures, by name.
    pub procs: HashMap<String, ProcInfo>,
    /// `ctl-opt main(name)`: the program runs that procedure, no cycle.
    pub main: Option<String>,
    /// Procedures called that are not in this module (bound in by CRTPGM,
    /// CRTSRVPGM or a binding directory).
    pub imports: Vec<String>,
    /// `ctl-opt nomain`: only procedures, no main procedure.
    pub nomain: bool,
    /// Embedded SQL cursors (DECLARE name CURSOR FOR ...): the SELECT as
    /// written, or the name of a prepared statement.
    pub cursors: HashMap<String, Cursor>,
    /// Multiple-occurrence data structures.
    pub occurs: Vec<Occurs>,
    /// Arrays and tables in sequence: 'A' (ASCEND) or 'D' (DESCEND).
    pub sequence: HashMap<String, char>,
    /// Varying-dimension arrays: 'A' DIM(*AUTO:n), 'V' DIM(*VAR:n).
    pub varying: HashMap<String, char>,
    /// Exported data (EXPORT on a field or data structure): (symbol,
    /// program name).
    pub data_exports: Vec<(String, String)>,
    /// Imported data (IMPORT): (symbol, program name).
    pub data_imports: Vec<(String, String)>,
    /// The program status data structure (PSDS, S in position 23).
    pub psds: Option<String>,
    /// Data area data structures read when the program starts and
    /// written when it ends with LR (U in position 23, DTAARA(*AUTO)):
    /// (data structure, data area: a name or *LDA).
    pub auto_dtaaras: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cursor {
    Select(String),
    Prepared(String),
}

/// The SQL communication area every program with embedded SQL has
/// (DB2 for i SQL reference, "SQLCA"), with the short names RPG programs
/// use for its fields (SQLCOD, SQLSTT, SQLER1-SQLER6).
const SQLCA: &str = "**FREE
dcl-ds SQLCA;
  SQLCAID char(8) inz('SQLCA');
  SQLCABC int(10) inz(136);
  SQLCODE int(10);
  SQLERRML int(5);
  SQLERRMC char(70);
  SQLERRP char(8);
  SQLERRD int(10) dim(6);
  SQLWARN char(11);
  SQLSTATE char(5) inz('00000');
  SQLCOD int(10) overlay(SQLCODE);
  SQLER1 int(10) overlay(SQLERRD);
  SQLER2 int(10) overlay(SQLERRD:5);
  SQLER3 int(10) overlay(SQLERRD:9);
  SQLER4 int(10) overlay(SQLERRD:13);
  SQLER5 int(10) overlay(SQLERRD:17);
  SQLER6 int(10) overlay(SQLERRD:21);
  SQLSTT char(5) overlay(SQLSTATE);
end-ds;
";

fn sql_statements(calcs: &[CalcLine]) -> impl Iterator<Item = &str> {
    calcs.iter().filter_map(|c| match &c.what {
        Line::Stmt(Stmt::Sql(s)) => Some(s.as_str()),
        _ => None,
    })
}

/// DECLARE c CURSOR [WITH HOLD...] FOR select | statement-name.
fn declared_cursor(sql: &str) -> Option<(String, Cursor)> {
    let words: Vec<&str> = sql.split_whitespace().collect();
    if words.len() < 4 || !words[0].eq_ignore_ascii_case("DECLARE") {
        return None;
    }
    let name = words[1].to_ascii_uppercase();
    let up = sql.to_ascii_uppercase();
    let at = up.find(" FOR ")?;
    let rest = sql[at + 5..].trim();
    let first = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    Some((
        name,
        if matches!(first.as_str(), "SELECT" | "WITH" | "VALUES") || rest.starts_with('(') {
            Cursor::Select(rest.to_string())
        } else {
            Cursor::Prepared(first)
        },
    ))
}

fn expr_name(e: &Option<Expr>) -> Option<String> {
    match e {
        Some(Expr::Name { parts, args: None }) => Some(parts.join(".")),
        _ => None,
    }
}

/// The data areas a program names: `*DTAARA DEFINE name field` (name
/// defaults to the field) and `DTAARA(name)` on a definition (ILE RPG
/// Reference, DEFINE and DTAARA).
fn data_areas(src: &Source) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for c in &src.calcs {
        if let Line::Stmt(Stmt::Op {
            code,
            f1,
            f2,
            result,
            ..
        }) = &c.what
        {
            if code == "DEFINE" && expr_name(f1).as_deref() == Some("*DTAARA") {
                if let Some(field) = expr_name(result) {
                    let name = expr_name(f2).unwrap_or_else(|| field.clone());
                    out.insert(field, name);
                }
            }
        }
    }
    for d in &src.defs {
        if keyword(&d.keywords, "DTAARA").is_some() || keyword(&d.keywords, "UDS").is_some() {
            out.insert(ds_key(d), dtaara_name(d));
        }
    }
    out
}

/// The name a data structure is known by (an unnamed one: *DSn, its
/// place among the definitions).
fn ds_key(d: &Def) -> String {
    if d.name.is_empty() || d.name == "*N" {
        format!("*DS{}", d.line)
    } else {
        d.name.clone()
    }
}

/// The data area a definition names (ILE RPG Reference, DTAARA keyword
/// and data area data structures): the DTAARA name (*LDA, *GDA, *PDA or a
/// data area), else the definition's own name; an unnamed data area data
/// structure is the local data area.
fn dtaara_name(d: &Def) -> String {
    keyword(&d.keywords, "DTAARA")
        .and_then(|k| {
            k.args
                .iter()
                .map(|a| a.trim().trim_matches('\'').to_ascii_uppercase())
                .find(|a| !matches!(a.as_str(), "*AUTO" | "*USRCTL" | "*VAR" | ""))
        })
        .unwrap_or_else(|| {
            if d.name.is_empty() || d.name == "*N" {
                "*LDA".into()
            } else {
                d.name.clone()
            }
        })
}

/// Data area data structures the program reads at start and writes at
/// its end: U in position 23, or DTAARA(*AUTO).
fn auto_dtaaras(src: &Source) -> Vec<(String, String)> {
    src.defs
        .iter()
        .filter(|d| d.kind == DefKind::Ds)
        .filter(|d| {
            keyword(&d.keywords, "UDS").is_some()
                || keyword(&d.keywords, "DTAARA").is_some_and(|k| {
                    k.args
                        .iter()
                        .any(|a| a.trim().eq_ignore_ascii_case("*AUTO"))
                })
        })
        .map(|d| (ds_key(d), dtaara_name(d)))
        .collect()
}

impl Program {
    /// The file a name refers to: a file name or a record format name.
    pub fn file_index(&self, name: &str) -> Option<usize> {
        self.files
            .iter()
            .position(|f| f.decl.name == name)
            .or_else(|| self.files.iter().position(|f| f.format == name))
            .or_else(|| {
                self.files
                    .iter()
                    .position(|f| f.display.as_ref().is_some_and(|d| d.fields(name).is_some()))
            })
    }
}

/// The external files a source needs described (DISK files with E in
/// position 22, EXTNAME and LIKEREC references).
pub fn external_files(src: &Source) -> Vec<String> {
    let mut out: Vec<String> = src
        .files
        .iter()
        .filter(|f| f.device == "DISK")
        .map(|f| {
            keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone())
        })
        .collect();
    for d in &src.defs {
        if let Some(k) = keyword(&d.keywords, "EXTNAME") {
            if let Some(a) = k.args.first() {
                out.push(a.trim_matches('\'').to_ascii_uppercase());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The display files a source uses (WORKSTN files with E in position 22).
pub fn display_files(src: &Source) -> Vec<String> {
    src.files
        .iter()
        .filter(|f| f.external && f.device == "WORKSTN")
        .map(|f| {
            keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone())
        })
        .collect()
}

/// The program field type of a display file field: zoned for numbers,
/// character otherwise (ILE RPG Reference, externally described
/// WORKSTN files).
fn display_type(f: &crate::dspf::Field) -> DType {
    match f.decimals {
        Some(d) => DType::Zoned(f.length as u32, d as u32),
        None => DType::Char(f.length as u32),
    }
}

fn literal_value(text: &str, syms: &HashMap<String, Sym>) -> Result<Value> {
    let e = expr::parse(text)?;
    const_value(&e, syms)
}

fn const_value(e: &Expr, syms: &HashMap<String, Sym>) -> Result<Value> {
    Ok(match e {
        Expr::Num(d) => Value::Num(*d),
        Expr::Str(s) | Expr::Hex(s) => Value::Char(s.clone()),
        Expr::Fig(f) => Value::Fig(f.clone()),
        Expr::Unary(UnOp::Neg, x) => Value::Num(const_value(x, syms)?.num()?.neg()),
        Expr::Name { parts, args: None } => match syms.get(&parts.join(".")) {
            Some(Sym::Const(v)) => v.clone(),
            _ => bail!("{} is not a constant", parts.join(".")),
        },
        Expr::Name { parts, .. } if parts[0].starts_with('*') => Value::Char(data::e(&parts[0])),
        // %SIZE and %ELEM of a defined item are known when the program is
        // compiled (ILE RPG Reference: both are allowed in definition
        // keywords such as INZ and DIM). %SIZE of an array is the size of
        // one element.
        Expr::Bif(f, a) if (f == "SIZE" || f == "ELEM") && a.len() == 1 => {
            let Expr::Name { parts, args: None } = &a[0] else {
                bail!("%{f} needs the name of a defined item");
            };
            let name = parts.join(".");
            let (size, dim) = match syms.get(&name) {
                Some(Sym::Var(v)) => (v.dtype.size(), v.dim),
                Some(Sym::Ds(d)) => (d.size, d.dim),
                Some(Sym::Const(Value::Char(c))) if f == "SIZE" => (c.len(), 0),
                _ => bail!("%{f}({name}): {name} is not defined before it is used"),
            };
            let n = if f == "SIZE" { size as i64 } else { dim as i64 };
            Value::Num(Dec::from_i64(n))
        }
        Expr::Bin(op @ (BinOp::Add | BinOp::Sub | BinOp::Mul), l, r) => {
            let (l, r) = (const_value(l, syms)?.num()?, const_value(r, syms)?.num()?);
            Value::Num(match op {
                BinOp::Add => l.add(&r)?,
                BinOp::Sub => l.sub(&r)?,
                _ => l.mul(&r)?,
            })
        }
        other => bail!("a constant value is needed, found {other:?}"),
    })
}

fn num_arg(a: &str, syms: &HashMap<String, Sym>) -> Result<u32> {
    if let Ok(n) = a.trim().parse() {
        return Ok(n);
    }
    match syms.get(&a.trim().to_ascii_uppercase()) {
        Some(Sym::Const(Value::Num(d))) => Ok(d.to_i128()? as u32),
        _ => bail!("'{a}' is not a number or a numeric constant"),
    }
}

/// Procedures the system provides (not imports to resolve).
pub const SYSTEM_PROCEDURES: &[&str] = &["QP0ZLPRINTF"];

impl Program {
    /// Procedures this module exports.
    pub fn exports(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .procs
            .values()
            .filter(|p| p.export)
            .map(|p| p.name.clone())
            .collect();
        v.sort();
        v
    }
}

/// External field type as the program sees it outside a data structure:
/// zoned and packed fields are packed (ILE RPG Reference: numeric fields
/// of externally described files are packed in the program).
/// With EXTBININT(*YES), binary fields with no decimal places are
/// integers (5, 10 or 20 digits). Large objects have no program type
/// (None): the program's record leaves them out (RNF7575).
fn program_type(f: &crate::dds::FieldDef, in_ds: bool, extbinint: bool) -> Option<DType> {
    Some(match f.data_type {
        DataType::Char if f.varlen => DType::Varchar(f.length),
        DataType::Char => DType::Char(f.length),
        // Hexadecimal: character data the program gets byte for byte.
        DataType::Hex if f.varlen => DType::Varchar(f.length),
        DataType::Hex => DType::Char(f.length),
        DataType::Float => DType::Float(if f.double { 8 } else { 4 }),
        DataType::Graphic if f.varlen => DType::VarUcs2(f.length),
        DataType::Graphic => DType::Ucs2(f.length),
        DataType::Clob | DataType::Blob | DataType::Dbclob => return None,
        DataType::Binary if extbinint && f.decimals == 0 => DType::Int(match f.length {
            0..=4 => 5,
            5..=9 => 10,
            _ => 20,
        }),
        DataType::Binary => DType::Binary(f.length, f.decimals),
        DataType::Date => DType::Date,
        DataType::Time => DType::Time,
        DataType::Timestamp => DType::Timestamp,
        DataType::Packed => DType::Packed(f.length, f.decimals),
        DataType::Zoned => {
            if in_ds {
                DType::Zoned(f.length, f.decimals)
            } else {
                DType::Packed(f.length, f.decimals)
            }
        }
    })
}

/// A record format's fields (program names, types) and key fields.
type RecordFormat = (Vec<(String, DType)>, Vec<String>);

struct Binder<'a> {
    syms: HashMap<String, Sym>,
    bufs: Vec<Vec<u8>>,
    externals: &'a HashMap<String, FileRef>,
    /// Types of external file fields, by name.
    file_fields: HashMap<String, DType>,
    /// Pending INZ values: (variable, value).
    /// Pending INZ values: (variable, value, or its text when it refers to
    /// a name defined later, as %SIZE of a later subfield can).
    inz: Vec<(String, Result<Value, String>)>,
    /// Binding a procedure's local storage.
    local: bool,
    /// Local buffers of STATIC variables (moved to global storage).
    statics: Vec<usize>,
    /// Global storage, seen (for templates) while binding a procedure.
    globals: Vec<Vec<u8>>,
    /// EXTBININT(*YES): external binary fields are integers.
    extbinint: bool,
    /// Multiple-occurrence data structures (OCCURS).
    occurs: Vec<Occurs>,
    /// Record formats of the program's files (for LIKEREC): the fields
    /// (program names, types) and the key fields.
    formats: HashMap<String, RecordFormat>,
    /// File information data structures (INFDS on a file).
    infds: Vec<String>,
}

/// A multiple-occurrence data structure (OCCURS keyword): its storage
/// holds every occurrence; names refer to the current one (OCCUR).
#[derive(Clone, Debug, PartialEq)]
pub struct Occurs {
    pub name: String,
    pub buf: usize,
    pub size: usize,
    pub count: u32,
}

/// What a definitions list declared besides storage: its procedure
/// interface (name, return type, parameters, keywords).
type Interface = (String, Option<DType>, Vec<ParamInfo>, Vec<Keyword>);

fn param_flags(d: &Def) -> (bool, bool, bool, bool) {
    let opts: Vec<String> = keyword(&d.keywords, "OPTIONS")
        .map(|k| {
            k.args
                .iter()
                .map(|a| a.trim().to_ascii_uppercase())
                .collect()
        })
        .unwrap_or_default();
    (
        keyword(&d.keywords, "VALUE").is_some(),
        keyword(&d.keywords, "CONST").is_some(),
        opts.iter().any(|o| o == "*NOPASS"),
        opts.iter().any(|o| o == "*OMIT"),
    )
}

fn ext_name(kws: &[Keyword], key: &str) -> Option<Option<String>> {
    keyword(kws, key).map(|k| {
        k.args
            .first()
            .map(|a| a.trim().trim_matches('\'').to_ascii_uppercase())
            .filter(|a| !a.is_empty())
    })
}

impl Binder<'_> {
    /// Bind a list of definitions (global, or a procedure's): constants,
    /// fields, data structures, prototypes (into `protos`), and the
    /// procedure interface, whose parameters become variables.
    fn defs(
        &mut self,
        defs: &[Def],
        protos: &mut HashMap<String, Proto>,
    ) -> Result<Option<Interface>> {
        let mut pi = None;
        // Prototypes are bound last: they allocate nothing, and a
        // parameter's LIKEDS may name a data structure defined after them.
        let mut order: Vec<usize> = Vec::new();
        let mut later: Vec<usize> = Vec::new();
        let mut like: Vec<usize> = Vec::new();
        {
            let mut k = 0;
            while k < defs.len() {
                let start = k;
                k += 1;
                let follows = match defs[start].kind {
                    DefKind::Ds => DefKind::Subfield,
                    DefKind::Proto | DefKind::Interface => DefKind::Param,
                    _ => DefKind::Const,
                };
                if defs[start].kind == DefKind::Ds
                    || defs[start].kind == DefKind::Proto
                    || defs[start].kind == DefKind::Interface
                {
                    while k < defs.len() && defs[k].kind == follows {
                        k += 1;
                    }
                }
                // A procedure interface too: its parameters may be LIKEDS
                // a data structure defined after it.
                if defs[start].kind == DefKind::Proto || defs[start].kind == DefKind::Interface {
                    later.push(start);
                } else if defs[start].kind == DefKind::Field
                    && keyword(&defs[start].keywords, "LIKE").is_some()
                {
                    like.push(start);
                } else {
                    order.push(start);
                }
            }
        }
        // Fields LIKE another field (D specs, *LIKE DEFINE) come after the
        // fields they are like, which may be defined anywhere in the
        // source.
        let target = |i: usize| {
            keyword(&defs[i].keywords, "LIKE")
                .and_then(|k| k.args.first())
                .map(|a| a.trim().to_ascii_uppercase())
                .unwrap_or_default()
        };
        while !like.is_empty() {
            let pending: Vec<String> = like.iter().map(|&i| defs[i].name.clone()).collect();
            let (ready, wait): (Vec<usize>, Vec<usize>) = like
                .iter()
                .partition(|&&i| !pending.contains(&target(i)) || target(i) == defs[i].name);
            if ready.is_empty() {
                order.extend(wait);
                break;
            }
            order.extend(ready);
            like = wait;
        }
        order.extend(later);
        for &i0 in &order {
            let i = i0;
            let d = &defs[i];
            match d.kind {
                DefKind::Const if keyword(&d.keywords, "ENUM").is_some() => {
                    // An enumeration: the list of its constants' values.
                    let mut vals = Vec::new();
                    for n in &keyword(&d.keywords, "ENUM").unwrap().args {
                        match self.syms.get(n) {
                            Some(Sym::Const(v)) => vals.push(v.clone()),
                            _ => bail!("line {}: enumeration constant {n} has no value", d.line),
                        }
                    }
                    self.syms
                        .insert(d.name.clone(), Sym::Const(Value::List(vals)));
                }
                DefKind::Const => {
                    let text = keyword(&d.keywords, "CONST")
                        .and_then(|k| k.args.first().cloned())
                        .or_else(|| {
                            // A literal written in the keyword area.
                            let joined = d
                                .keywords
                                .iter()
                                .map(|k| k.name.clone())
                                .collect::<Vec<_>>()
                                .join(" ");
                            if joined.is_empty() {
                                None
                            } else {
                                Some(joined)
                            }
                        })
                        .ok_or_else(|| {
                            anyhow!("line {}: constant {} has no value", d.line, d.name)
                        })?;
                    let v = literal_value(&text, &self.syms)
                        .map_err(|e| anyhow!("line {}: {e}", d.line))?;
                    self.syms.insert(d.name.clone(), Sym::Const(v));
                }
                DefKind::Field => {
                    let t = self
                        .resolve_type(d)?
                        .or_else(|| self.file_fields.get(&d.name).copied())
                        .ok_or_else(|| {
                            anyhow!("line {}: {} needs a type and length", d.line, d.name)
                        })?;
                    let dim = self.dim(d)?;
                    let v = self.add_var(&d.name, t, dim);
                    if self.local && keyword(&d.keywords, "STATIC").is_some() {
                        self.statics.push(v.buf);
                    }
                    self.inz(&d.name.clone(), d)?;
                }
                DefKind::Ds => {
                    self.ds(defs, i)
                        .map_err(|e| anyhow!("line {}: {e}", d.line))?;
                    if self.local && keyword(&d.keywords, "STATIC").is_some() {
                        self.statics.push(self.bufs.len() - 1);
                    }
                }
                DefKind::Proto | DefKind::Interface => {
                    let interface = d.kind == DefKind::Interface;
                    let mut params = Vec::new();
                    let mut j = i + 1;
                    while j < defs.len() && defs[j].kind == DefKind::Param {
                        let p = &defs[j];
                        let (by_value, konst, nopass, omit) = param_flags(p);
                        let likeds = keyword(&p.keywords, "LIKEDS").is_some();
                        let (dtype, buf) = if interface {
                            // The parameter is a variable of the procedure.
                            if likeds {
                                self.ds(defs, j)
                                    .map_err(|e| anyhow!("line {}: {e}", p.line))?;
                                let Some(Sym::Ds(ds)) = self.syms.get(&p.name) else {
                                    bail!("line {}: parameter {} is not bound", p.line, p.name)
                                };
                                (DType::Char(ds.size as u32), Some(ds.buf))
                            } else {
                                let t = self.resolve_type(p)?.ok_or_else(|| {
                                    anyhow!("line {}: {} needs a type and length", p.line, p.name)
                                })?;
                                let dim = self.dim(p)?;
                                let v = self.add_var(&p.name, t, dim);
                                (t, Some(v.buf))
                            }
                        } else if likeds {
                            let other = keyword(&p.keywords, "LIKEDS")
                                .and_then(|k| k.args.first())
                                .map(|a| a.trim().to_ascii_uppercase())
                                .unwrap_or_default();
                            match self.syms.get(&other) {
                                Some(Sym::Ds(ds)) => (DType::Char(ds.size as u32), None),
                                _ => {
                                    bail!("line {}: LIKEDS({other}): not a data structure", p.line)
                                }
                            }
                        } else {
                            let t = self.resolve_type(p)?.ok_or_else(|| {
                                anyhow!(
                                    "line {}: parameter {} needs a type and length",
                                    p.line,
                                    p.name
                                )
                            })?;
                            (t, None)
                        };
                        params.push(ParamInfo {
                            name: p.name.clone(),
                            dtype,
                            by_value,
                            konst,
                            nopass,
                            omit,
                            buf,
                        });
                        j += 1;
                    }
                    let returns = self.resolve_type(d)?;
                    if interface {
                        pi = Some((d.name.clone(), returns, params, d.keywords.clone()));
                    } else {
                        let extpgm = ext_name(&d.keywords, "EXTPGM")
                            .map(|n| n.unwrap_or_else(|| d.name.clone()));
                        let extproc = if extpgm.is_some() {
                            None
                        } else {
                            Some(
                                ext_name(&d.keywords, "EXTPROC")
                                    .flatten()
                                    .unwrap_or_else(|| d.name.clone()),
                            )
                        };
                        protos.insert(
                            d.name.clone(),
                            Proto {
                                name: d.name.clone(),
                                extpgm,
                                extproc,
                                params,
                                returns,
                                overload: keyword(&d.keywords, "OVERLOAD")
                                    .map(|k| {
                                        k.args
                                            .iter()
                                            .map(|a| a.trim().to_ascii_uppercase())
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            },
                        );
                    }
                }
                DefKind::Subfield | DefKind::Param => bail!(
                    "line {}: {} has no data structure or prototype",
                    d.line,
                    d.name
                ),
            }
        }
        Ok(pi)
    }

    /// Store the pending INZ values.
    fn apply_inz(&mut self) -> Result<()> {
        for (name, v) in std::mem::take(&mut self.inz) {
            let v = match v {
                Ok(v) => v,
                Err(text) => literal_value(&text, &self.syms)?,
            };
            if let Some(Sym::Var(var)) = self.syms.get(&name).cloned() {
                for k in 0..var.dim.max(1) as usize {
                    let at = var.off + k * var.stride;
                    let out = &mut self.bufs[var.buf][at..at + var.dtype.size()];
                    data::store(
                        &var.dtype,
                        &v,
                        out,
                        data::Fit::Strict { half_adjust: false },
                        false,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn new_buf(&mut self, size: usize, fill: u8) -> usize {
        self.bufs.push(vec![fill; size]);
        self.bufs.len() - 1
    }

    fn resolve_type(&self, d: &Def) -> Result<Option<DType>> {
        if let Some(t) = d.dtype {
            return Ok(Some(t));
        }
        // A free-form type with a constant length: CHAR(MAXLEN).
        if let Some(k) = d.keywords.iter().find(|k| k.name.starts_with("TYPE:")) {
            let n = |i: usize| -> Result<u32> {
                num_arg(k.args.get(i).map(|s| s.as_str()).unwrap_or("0"), &self.syms)
            };
            return Ok(Some(match &k.name[5..] {
                "CHAR" => DType::Char(n(0)?),
                "VARCHAR" => DType::Varchar(n(0)?),
                "PACKED" => DType::Packed(n(0)?, if k.args.len() > 1 { n(1)? } else { 0 }),
                "ZONED" => DType::Zoned(n(0)?, if k.args.len() > 1 { n(1)? } else { 0 }),
                "INT" => DType::Int(n(0)?),
                "UNS" => DType::Uns(n(0)?),
                other => bail!("type {other} is not supported yet"),
            }));
        }
        if let Some(k) = keyword(&d.keywords, "LIKE") {
            let other = k
                .args
                .first()
                .map(|a| a.trim().to_ascii_uppercase())
                .unwrap_or_default();
            let base = match self.syms.get(&other) {
                Some(Sym::Var(v)) => v.dtype,
                _ => *self
                    .file_fields
                    .get(&other)
                    .ok_or_else(|| anyhow!("LIKE({other}): {other} is not defined before"))?,
            };
            // LIKE(x: +n) changes the length.
            if let Some(adj) = k.args.get(1) {
                let n: i64 = adj.trim().trim_start_matches('+').parse()?;
                return Ok(Some(match base {
                    DType::Char(l) => DType::Char((l as i64 + n) as u32),
                    DType::Varchar(l) => DType::Varchar((l as i64 + n) as u32),
                    DType::Packed(l, p) => DType::Packed((l as i64 + n) as u32, p),
                    DType::Zoned(l, p) => DType::Zoned((l as i64 + n) as u32, p),
                    other => other,
                }));
            }
            return Ok(Some(base));
        }
        Ok(None)
    }

    fn dim(&self, d: &Def) -> Result<u32> {
        let Some(k) = keyword(&d.keywords, "DIM") else {
            return Ok(0);
        };
        // DIM(*AUTO : n), DIM(*VAR : n): storage for the maximum.
        let a = match k.args.first().map(|a| a.trim().to_ascii_uppercase()) {
            Some(f) if f == "*AUTO" || f == "*VAR" => k.args.get(1),
            _ => k.args.first(),
        };
        match a {
            Some(a) => num_arg(a, &self.syms),
            None => Ok(0),
        }
    }

    fn inz(&mut self, name: &str, d: &Def) -> Result<()> {
        if let Some(k) = keyword(&d.keywords, "INZ") {
            if let Some(a) = k.args.first().filter(|a| !a.is_empty()) {
                // A value that names something not defined yet is worked
                // out once all definitions are in.
                let v = literal_value(a, &self.syms).map_err(|_| a.clone());
                self.inz.push((name.to_string(), v));
            }
        }
        Ok(())
    }

    fn add_var(&mut self, name: &str, dtype: DType, dim: u32) -> VarInfo {
        let n = dim.max(1) as usize;
        let mut buf = Vec::with_capacity(dtype.size() * n);
        for _ in 0..n {
            buf.extend(dtype.initial());
        }
        self.bufs.push(buf);
        let v = VarInfo {
            dtype,
            buf: self.bufs.len() - 1,
            off: 0,
            dim,
            stride: dtype.size(),
            local: self.local,
        };
        self.syms.insert(name.to_string(), Sym::Var(v.clone()));
        v
    }

    /// A data structure and its subfields (defs[i] is the DS).
    fn ds(&mut self, defs: &[Def], i: usize) -> Result<usize> {
        let d = &defs[i];
        let name = d.name.clone();
        let qualified = keyword(&d.keywords, "QUALIFIED").is_some();
        let dim = self.dim(d)?;
        // (name, type, offset, dim)
        let mut subs: Vec<(String, DType, usize, u32, &Def)> = Vec::new();
        let mut next = 0usize;
        let mut j = i + 1;
        // EXTNAME: subfields from a file's record, external types.
        if let Some(k) = keyword(&d.keywords, "EXTNAME") {
            let f = k
                .args
                .first()
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_default();
            let fr = self
                .externals
                .get(&f)
                .ok_or_else(|| anyhow!("EXTNAME({f}): file not found"))?;
            // ALIAS: the subfields have the fields' alternative (SQL)
            // names where they have one (ILE RPG Reference, ALIAS).
            let alias = keyword(&d.keywords, "ALIAS").is_some();
            for fd in &fr.file.fields {
                let Some(t) = program_type(fd, true, self.extbinint) else {
                    continue;
                };
                let n = match (&fd.alias, alias) {
                    (Some(a), true) => a.clone(),
                    _ => fd.name.clone(),
                };
                subs.push((n, t, next, 0, d));
                next += t.size();
            }
        }
        // LIKEREC(format [: *ALL | *KEY ...]): a qualified data structure
        // with the record format's fields (ILE RPG Reference, LIKEREC).
        let likerec = keyword(&d.keywords, "LIKEREC").map(|k| {
            (
                k.args
                    .first()
                    .map(|a| a.trim().to_ascii_uppercase())
                    .unwrap_or_default(),
                k.args.get(1).map(|a| a.trim().to_ascii_uppercase()),
            )
        });
        if let Some((fmt, which)) = &likerec {
            let (fields, keys) = self.formats.get(fmt).cloned().ok_or_else(|| {
                anyhow!("LIKEREC({fmt}): not a record format of a file of the program")
            })?;
            for (n, t) in fields {
                if which.as_deref() == Some("*KEY") && !keys.contains(&n) {
                    continue;
                }
                subs.push((n, t, next, 0, d));
                next += t.size();
            }
        }
        // Subfields that are data structures themselves (LIKEDS):
        // (subfield index, the template's name, the template).
        let mut nested: Vec<(usize, String, DsInfo)> = Vec::new();
        // LIKEDS: the same subfields as another DS (qualified).
        let likeds = keyword(&d.keywords, "LIKEDS")
            .and_then(|k| k.args.first())
            .map(|a| a.trim().to_ascii_uppercase());
        if let Some(other) = &likeds {
            let Some(Sym::Ds(o)) = self.syms.get(other).cloned() else {
                bail!("LIKEDS({other}): not a data structure")
            };
            for sf in &o.subfields {
                let key = if o.qualified {
                    format!("{other}.{sf}")
                } else {
                    sf.clone()
                };
                match self.syms.get(&key).cloned() {
                    Some(Sym::Var(v)) => subs.push((sf.clone(), v.dtype, v.off - o.off, v.dim, d)),
                    Some(Sym::Ds(n)) => {
                        nested.push((subs.len(), key.clone(), n.clone()));
                        subs.push((
                            sf.clone(),
                            DType::Char(n.size as u32),
                            n.off - o.off,
                            n.dim,
                            d,
                        ));
                    }
                    _ => {}
                }
            }
            next = o.size;
        }
        let psds = keyword(&d.keywords, "PSDS").is_some();
        let infds = self.infds.contains(&name);
        while j < defs.len() && defs[j].kind == DefKind::Subfield {
            let s = &defs[j];
            // *STATUS, *ROUTINE...: a subfield at its place in the program
            // status or file information data structure.
            if let Some(k) = s.keywords.iter().find(|k| k.name.starts_with('*')) {
                let (off, t) = special_subfield(&k.name, psds, infds).ok_or_else(|| {
                    anyhow!(
                        "subfield {}: {} is not a keyword of a {} data structure",
                        s.name,
                        k.name,
                        if psds {
                            "program status"
                        } else if infds {
                            "file information"
                        } else {
                            "plain"
                        }
                    )
                })?;
                next = next.max(off + t.size());
                subs.push((s.name.clone(), t, off, 0, s));
                j += 1;
                continue;
            }
            let mut t = self.resolve_type(s)?;
            if t.is_none() {
                if let Some(k) = keyword(&s.keywords, "LIKEDS") {
                    let other = k
                        .args
                        .first()
                        .map(|a| a.trim().to_ascii_uppercase())
                        .unwrap_or_default();
                    let Some(Sym::Ds(o)) = self.syms.get(&other).cloned() else {
                        bail!("LIKEDS({other}): not a data structure");
                    };
                    t = Some(DType::Char(o.size as u32));
                    nested.push((subs.len(), other, o));
                }
            }
            if t.is_none() {
                t = self.file_fields.get(&s.name).copied().map(|t| match t {
                    // In a DS, an external numeric field keeps its type.
                    DType::Packed(l, p) => DType::Packed(l, p),
                    o => o,
                });
            }
            let off = if let Some(k) = keyword(&s.keywords, "OVERLAY") {
                let target = k
                    .args
                    .first()
                    .map(|a| a.trim().to_ascii_uppercase())
                    .unwrap_or_default();
                let base = if target == name {
                    0
                } else {
                    subs.iter()
                        .find(|x| x.0 == target)
                        .map(|x| x.2)
                        .ok_or_else(|| anyhow!("OVERLAY({target}): not a subfield before"))?
                };
                match k.args.get(1).map(|a| a.trim().to_ascii_uppercase()) {
                    Some(p) if p == "*NEXT" => next,
                    Some(p) => base + p.parse::<usize>()? - 1,
                    None => base,
                }
            } else if let Some(f) = s.from {
                (f - 1) as usize
            } else {
                next
            };
            let t = match t {
                Some(t) => t,
                None if s.to.is_some() => DType::Char(s.to.unwrap() + 1 - s.from.unwrap_or(1)),
                None => bail!("subfield {} needs a type and length", s.name),
            };
            let sdim = self.dim(s)?;
            let end = off + t.size() * sdim.max(1) as usize;
            if keyword(&s.keywords, "OVERLAY").is_none() || end > next {
                next = next.max(end);
            }
            subs.push((s.name.clone(), t, off, sdim, s));
            j += 1;
        }
        let explicit = keyword(&d.keywords, "LEN")
            .and_then(|k| k.args.first())
            .map(|a| num_arg(a, &self.syms))
            .transpose()?
            .map(|n| n as usize)
            .or(d.to.map(|t| t as usize));
        let size = explicit
            .unwrap_or(0)
            .max(
                subs.iter()
                    .map(|s| s.2 + s.1.size() * s.3.max(1) as usize)
                    .max()
                    .unwrap_or(0),
            )
            .max(1);
        // A DS starts as blanks, unless INZ: then each subfield gets its
        // type's initial value (ILE RPG Reference, INZ keyword).
        let has_inz = keyword(&d.keywords, "INZ").is_some();
        let occurs = keyword(&d.keywords, "OCCURS")
            .and_then(|k| k.args.first())
            .map(|a| num_arg(a, &self.syms))
            .transpose()?
            .unwrap_or(0);
        if occurs > 0 && dim > 0 {
            bail!("a data structure cannot have both DIM and OCCURS");
        }
        let reps = dim.max(occurs).max(1);
        let total = size * reps as usize;
        let buf = self.new_buf(total, data::BLANK);
        let mut names = Vec::new();
        for (sname, t, off, sdim, sdef) in &subs {
            let key = if qualified || likeds.is_some() || likerec.is_some() {
                format!("{name}.{sname}")
            } else {
                sname.clone()
            };
            let v = VarInfo {
                dtype: *t,
                buf,
                off: *off,
                dim: *sdim,
                stride: t.size(),
                local: self.local,
            };
            if has_inz || keyword(&sdef.keywords, "INZ").is_some() && !std::ptr::eq(*sdef, d) {
                for e in 0..reps as usize {
                    for k in 0..(*sdim).max(1) as usize {
                        let at = e * size + off + k * t.size();
                        self.bufs[buf][at..at + t.size()].copy_from_slice(&t.initial());
                    }
                }
            }
            self.syms.insert(key.clone(), Sym::Var(v));
            if !std::ptr::eq(*sdef, d) {
                self.inz(&key, sdef)?;
            }
            names.push(sname.clone());
        }
        // A LIKEDS subfield is a qualified data structure at its offset,
        // with the template's subfields (and their own nested ones) under
        // its name: LIST.USERS.NAME.
        for (k, template, o) in &nested {
            let (sname, _, off, sdim, _) = &subs[*k];
            let prefix = if qualified || likeds.is_some() || likerec.is_some() {
                format!("{name}.{sname}")
            } else {
                sname.clone()
            };
            let template_prefix = if o.qualified {
                format!("{template}.")
            } else {
                String::new()
            };
            let mut add: Vec<(String, Sym)> = Vec::new();
            for (key, sym) in &self.syms {
                let rest = if o.qualified {
                    key.strip_prefix(&template_prefix)
                } else if o.subfields.contains(key) {
                    Some(key.as_str())
                } else {
                    None
                };
                let Some(rest) = rest else { continue };
                let rel = |x: usize| x - o.off;
                match sym {
                    Sym::Var(v) if v.buf == o.buf && v.local == o.local => add.push((
                        format!("{prefix}.{rest}"),
                        Sym::Var(VarInfo {
                            buf,
                            off: off + rel(v.off),
                            local: self.local,
                            ..v.clone()
                        }),
                    )),
                    Sym::Ds(d) if d.buf == o.buf && d.local == o.local => add.push((
                        format!("{prefix}.{rest}"),
                        Sym::Ds(DsInfo {
                            buf,
                            off: off + rel(d.off),
                            local: self.local,
                            ..d.clone()
                        }),
                    )),
                    _ => {}
                }
            }
            // The template's initial contents (INZ) come along.
            // A procedure's LIKEDS of a global template reads global storage.
            let from = if self.local && !o.local {
                &self.globals
            } else {
                &self.bufs
            };
            let src = from[o.buf][o.off..o.off + o.size].to_vec();
            for e in 0..reps as usize {
                for n in 0..(*sdim).max(1) as usize {
                    let at = e * size + off + n * o.size;
                    self.bufs[buf][at..at + o.size].copy_from_slice(&src);
                }
            }
            for (key, sym) in add {
                self.syms.insert(key, sym);
            }
            self.syms.insert(
                prefix,
                Sym::Ds(DsInfo {
                    buf,
                    off: *off,
                    size: o.size,
                    dim: *sdim,
                    qualified: true,
                    subfields: o.subfields.clone(),
                    local: self.local,
                }),
            );
        }
        let ds_name = ds_key(d);
        if occurs > 0 {
            self.occurs.push(Occurs {
                name: ds_name.clone(),
                buf,
                size,
                count: occurs,
            });
        }
        self.syms.insert(
            ds_name,
            Sym::Ds(DsInfo {
                buf,
                off: 0,
                size,
                dim,
                qualified: qualified || likeds.is_some() || likerec.is_some(),
                subfields: names,
                local: self.local,
            }),
        );
        Ok(j)
    }
}

/// Where a special subfield is (offset) and its type: the program status
/// data structure's *PROC (1-10), *STATUS (11-15), *ROUTINE (29-36) and
/// *PARMS (37-39); the file information data structure's *FILE (1-8),
/// *STATUS (11-15), *OPCODE (16-21), *ROUTINE (22-29), *RECORD (38-45),
/// *SIZE (67-70), *INP (71-72), *OUT (73-74) and *MODE (75-76) (ILE RPG
/// Reference, "Program Status Data Structure" and "File Feedback
/// Information").
fn special_subfield(k: &str, psds: bool, infds: bool) -> Option<(usize, DType)> {
    Some(match (k, psds, infds) {
        ("*PROC", true, _) => (0, DType::Char(10)),
        ("*STATUS", true, _) => (10, DType::Zoned(5, 0)),
        ("*ROUTINE", true, _) => (28, DType::Char(8)),
        ("*PARMS", true, _) => (36, DType::Zoned(3, 0)),
        ("*FILE", _, true) => (0, DType::Char(8)),
        ("*STATUS", _, true) => (10, DType::Zoned(5, 0)),
        ("*OPCODE", _, true) => (15, DType::Char(6)),
        ("*ROUTINE", _, true) => (21, DType::Char(8)),
        ("*RECORD", _, true) => (37, DType::Char(8)),
        ("*SIZE", _, true) => (66, DType::Zoned(4, 0)),
        ("*INP", _, true) => (70, DType::Zoned(2, 0)),
        ("*OUT", _, true) => (72, DType::Zoned(2, 0)),
        ("*MODE", _, true) => (74, DType::Zoned(2, 0)),
        _ => return None,
    })
}

/// Bind a parsed source. `externals`: descriptions of the external files
/// (by file name), from the catalog.
pub fn bind(
    name: &str,
    src: &Source,
    externals: &HashMap<String, FileRef>,
    displays: &HashMap<String, (String, Arc<DisplayFile>)>,
) -> Result<Program> {
    let mut b = Binder {
        syms: HashMap::new(),
        bufs: vec![],
        externals,
        file_fields: HashMap::new(),
        inz: vec![],
        globals: vec![],
        extbinint: keyword(&src.control, "EXTBININT")
            .and_then(|k| k.args.first())
            .is_some_and(|a| a.trim().eq_ignore_ascii_case("*YES")),
        local: false,
        statics: vec![],
        occurs: vec![],
        formats: HashMap::new(),
        infds: src
            .files
            .iter()
            .filter_map(|f| keyword(&f.keywords, "INFDS"))
            .filter_map(|k| k.args.first())
            .map(|a| a.trim().to_ascii_uppercase())
            .collect(),
    };
    // Files and their record fields.
    let mut files = Vec::new();
    for f in &src.files {
        let mut info = FileInfo {
            decl: f.clone(),
            fref: None,
            format: f.name.clone(),
            fields: vec![],
            display: None,
            program_described: false,
            layout: vec![],
            record_len: 0,
            key_len: 0,
            inputs: vec![],
        };
        if f.device == "WORKSTN" && f.external {
            let ext = keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone());
            let (library, file) = displays
                .get(&ext)
                .ok_or_else(|| anyhow!("display file {} is not described (not found)", f.name))?
                .clone();
            let prefix = keyword(&f.keywords, "PREFIX")
                .and_then(|k| k.args.first())
                .map(|p| p.trim().to_ascii_uppercase());
            let mut formats = Vec::new();
            for r in &file.records {
                let mut fields = Vec::new();
                for fd in &r.fields {
                    let pname = match &prefix {
                        Some(p) => format!("{p}{}", fd.name),
                        None => fd.name.to_ascii_uppercase(),
                    };
                    if f.proc.is_none() {
                        b.file_fields
                            .entry(pname.clone())
                            .or_insert_with(|| display_type(fd));
                    }
                    fields.push((pname, fd.name.clone()));
                }
                formats.push((r.name.to_ascii_uppercase(), fields));
            }
            let mut sfile = HashMap::new();
            for k in f.keywords.iter().filter(|k| k.name == "SFILE") {
                if k.args.len() == 2 {
                    sfile.insert(
                        k.args[0].trim().to_ascii_uppercase(),
                        k.args[1].trim().to_ascii_uppercase(),
                    );
                }
            }
            let indds = keyword(&f.keywords, "INDDS")
                .and_then(|k| k.args.first())
                .map(|a| a.trim().to_ascii_uppercase());
            info.display = Some(Display {
                library,
                file,
                formats,
                sfile,
                indds,
            });
        }
        if f.device == "DISK" && f.external {
            let ext = keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone());
            let fr = externals
                .get(&ext)
                .ok_or_else(|| anyhow!("file {} is not described (not found)", f.name))?
                .clone();
            info.format = fr.file.format.clone();
            // RENAME(ext:int) on the F spec.
            if let Some(k) = keyword(&f.keywords, "RENAME") {
                if k.args.len() == 2 && k.args[0].trim().eq_ignore_ascii_case(&info.format) {
                    info.format = k.args[1].trim().to_ascii_uppercase();
                }
            }
            let prefix = keyword(&f.keywords, "PREFIX")
                .and_then(|k| k.args.first())
                .map(|p| p.trim().to_ascii_uppercase());
            // ALIAS: the program uses the fields' alternative (SQL) names
            // where they have one (ILE RPG Reference, ALIAS keyword for
            // files).
            let alias = keyword(&f.keywords, "ALIAS").is_some();
            for fd in &fr.file.fields {
                // Large objects are not in the program's record.
                if fd.data_type.is_lob() {
                    info.fields.push(String::new());
                    continue;
                }
                let base = match (&fd.alias, alias) {
                    (Some(a), true) => a.clone(),
                    _ => fd.name.clone(),
                };
                let pname = match &prefix {
                    Some(p) => format!("{p}{base}"),
                    None => base,
                };
                // A field line of the input specifications renames it.
                let pname = src
                    .inputs
                    .iter()
                    .filter(|r| r.file == info.format || r.file == fr.file.format)
                    .flat_map(|r| &r.fields)
                    .find(|x| x.external.as_deref() == Some(fd.name.as_str()))
                    .map(|x| x.name.clone())
                    .unwrap_or(pname);
                // A file declared in a procedure has no fields of its
                // own in the program: its records are read into and
                // written from data structures (ILE RPG Reference, "File
                // Considerations", local files).
                if f.proc.is_none() {
                    b.file_fields.entry(pname.clone()).or_insert_with(|| {
                        program_type(fd, false, b.extbinint).unwrap_or(DType::Char(1))
                    });
                }
                info.fields.push(pname);
            }
            // The record format for LIKEREC.
            let mut fmt_fields = Vec::new();
            for (fd, p) in fr.file.fields.iter().zip(&info.fields) {
                if p.is_empty() {
                    continue;
                }
                if let Some(t) = program_type(fd, true, b.extbinint) {
                    fmt_fields.push((p.clone(), t));
                }
            }
            let keys: Vec<String> = fr
                .file
                .keys
                .iter()
                .filter_map(|k| {
                    fr.file
                        .fields
                        .iter()
                        .position(|fd| &fd.name == k)
                        .map(|i| info.fields[i].clone())
                })
                .collect();
            b.formats.insert(info.format.clone(), (fmt_fields, keys));
            let stored = |fd: &crate::dds::FieldDef| {
                program_type(fd, true, false).map(|t| t.size()).unwrap_or(0)
            };
            info.record_len = fr.file.fields.iter().map(stored).sum();
            info.key_len = fr
                .file
                .keys
                .iter()
                .filter_map(|k| fr.file.fields.iter().find(|fd| &fd.name == k))
                .map(stored)
                .sum();
            // The input specifications for the record format: record
            // identifying indicator, control levels, matching fields and
            // field indicators by program field.
            for r in src
                .inputs
                .iter()
                .filter(|r| r.file == info.format || r.file == fr.file.format)
            {
                info.inputs.push(InputRec {
                    record_ind: r.record_ind.clone(),
                    codes: vec![],
                    fields: r
                        .fields
                        .iter()
                        .map(|x| InputField {
                            name: x.name.clone(),
                            from: 0,
                            to: 0,
                            dtype: None,
                            level: x.level.clone(),
                            matching: x.matching.clone(),
                            plus: x.plus.clone(),
                            minus: x.minus.clone(),
                            zero: x.zero.clone(),
                        })
                        .collect(),
                });
            }
            info.fref = Some(fr);
        }
        if f.device == "DISK" && !f.external {
            // A program-described file over a database file: the record
            // image is the fields of its record format, each in its
            // stored form (zoned, packed, binary, characters).
            let fr = externals
                .get(&f.name)
                .ok_or_else(|| anyhow!("file {} is not described (not found)", f.name))?
                .clone();
            let mut at = 0usize;
            for fd in &fr.file.fields {
                let t = program_type(fd, true, false).unwrap_or(DType::Char(0));
                info.layout.push((t, at));
                at += t.size();
            }
            info.record_len = at;
            info.program_described = true;
            info.format = fr.file.format.clone();
            for r in src.inputs.iter().filter(|r| r.file == f.name) {
                let mut fields = Vec::new();
                for x in &r.fields {
                    let t = input_type(x).map_err(|e| anyhow!("line {}: {e}", r.line))?;
                    // The program field: packed for numbers (characters
                    // as they are), unless defined elsewhere.
                    let pt = match t {
                        DType::Char(n) => DType::Char(n),
                        other => {
                            let (n, d) = other.precision();
                            DType::Packed(n, d)
                        }
                    };
                    b.file_fields.entry(x.name.clone()).or_insert(pt);
                    fields.push(InputField {
                        name: x.name.clone(),
                        from: x.from as usize,
                        to: x.to as usize,
                        dtype: Some(t),
                        level: x.level.clone(),
                        matching: x.matching.clone(),
                        plus: x.plus.clone(),
                        minus: x.minus.clone(),
                        zero: x.zero.clone(),
                    });
                }
                info.inputs.push(InputRec {
                    record_ind: r.record_ind.clone(),
                    codes: r.codes.clone(),
                    fields,
                });
            }
            info.fref = Some(fr);
        }
        files.push(info);
    }
    // Embedded SQL: the SQLCA comes first, and the declared cursors.
    let mut cursors = HashMap::new();
    let mut uses_sql = false;
    for s in
        sql_statements(&src.calcs).chain(src.procs.iter().flat_map(|p| sql_statements(&p.calcs)))
    {
        uses_sql = true;
        if let Some((n, c)) = declared_cursor(s) {
            cursors.insert(n, c);
        }
    }
    let with_sqlca;
    let defs = if uses_sql && !src.defs.iter().any(|d| d.name == "SQLCA") {
        let mut v = super::parse::parse(SQLCA, &|_| None)?.defs;
        v.extend(src.defs.iter().cloned());
        with_sqlca = v;
        &with_sqlca
    } else {
        &src.defs
    };
    // Definitions, in order.
    let mut protos = HashMap::new();
    let global_pi = b.defs(defs, &mut protos)?;
    // File fields not defined otherwise become standalone fields.
    let names: Vec<(String, DType)> = b.file_fields.iter().map(|(k, v)| (k.clone(), *v)).collect();
    for (n, t) in names {
        if !b.syms.contains_key(&n) {
            b.add_var(&n, t, 0);
        }
    }
    // Calculation result fields that were not declared (fixed-form
    // result field lengths came through as definitions already).
    for (n, list) in &src.klists {
        b.syms.insert(n.clone(), Sym::Klist(list.clone()));
    }
    // INZ values.
    b.apply_inz()?;
    // Compile-time arrays and tables (CTDATA). An alternating array
    // (ALT) takes its entries from the same records, each entry after
    // the main array's (ILE RPG Reference, "Arrays and Tables"). Numeric
    // entries are written as digits, the decimal point implied, the sign
    // in the zone of the last digit.
    let mut unnamed = src.ctdata.iter().filter(|c| c.0.is_none());
    for d in defs
        .iter()
        .filter(|d| keyword(&d.keywords, "CTDATA").is_some())
    {
        let rows = src
            .ctdata
            .iter()
            .find(|c| c.0.as_deref() == Some(&d.name))
            .or_else(|| unnamed.next())
            .map(|c| c.1.clone())
            .unwrap_or_default();
        let Some(Sym::Var(v)) = b.syms.get(&d.name).cloned() else {
            continue;
        };
        let alt = defs
            .iter()
            .find(|x| {
                keyword(&x.keywords, "ALT")
                    .and_then(|k| k.args.first())
                    .is_some_and(|a| a.trim().eq_ignore_ascii_case(&d.name))
            })
            .and_then(|x| match b.syms.get(&x.name) {
                Some(Sym::Var(av)) => Some(av.clone()),
                _ => None,
            });
        let per = keyword(&d.keywords, "PERRCD")
            .and_then(|k| k.args.first())
            .and_then(|a| a.trim().parse::<usize>().ok())
            .unwrap_or(1);
        let width = |t: &DType| -> usize {
            if t.is_numeric() {
                t.precision().0 as usize
            } else {
                t.len() as usize
            }
        };
        let mut n = 0usize;
        for row in rows {
            let chars: Vec<char> = row.chars().collect();
            let mut at = 0usize;
            for _ in 0..per {
                if n >= v.dim as usize {
                    break;
                }
                for (var, w) in std::iter::once((&v, width(&v.dtype)))
                    .chain(alt.as_ref().map(|a| (a, width(&a.dtype))))
                {
                    let piece: String = chars.iter().skip(at).take(w).collect();
                    at += w;
                    let piece = format!("{piece:<w$}");
                    let val = if var.dtype.is_numeric() {
                        let (digits, decs) = var.dtype.precision();
                        let zoned: Vec<u8> = data::e(&piece)
                            .iter()
                            .map(|c| if *c == data::BLANK { 0xF0 } else { *c })
                            .collect();
                        Value::Num(data::decode_num(&DType::Zoned(digits, decs), &zoned)?)
                    } else {
                        Value::str(&piece)
                    };
                    let off = var.off + n * var.stride;
                    data::store(
                        &var.dtype,
                        &val,
                        &mut b.bufs[var.buf][off..off + var.dtype.size()],
                        data::Fit::Strict { half_adjust: false },
                        false,
                    )?;
                }
                n += 1;
            }
        }
    }
    let _ = Fig::Blanks;
    // Procedures: each with its own local storage, seeing the globals.
    let mut procs = HashMap::new();
    for pr in &src.procs {
        let mut pb = Binder {
            syms: b.syms.clone(),
            bufs: vec![],
            externals,
            file_fields: b.file_fields.clone(),
            inz: vec![],
            local: true,
            statics: vec![],
            globals: b.bufs.clone(),
            extbinint: b.extbinint,
            occurs: vec![],
            formats: b.formats.clone(),
            infds: b.infds.clone(),
        };
        let pi = pb
            .defs(&pr.defs, &mut protos)
            .map_err(|e| anyhow!("procedure {}: {e}", pr.name))?;
        pb.apply_inz()?;
        // STATIC variables keep their values between calls: they live in
        // global storage.
        let mut moved: HashMap<usize, usize> = HashMap::new();
        for lb in pb.statics.clone() {
            b.bufs.push(pb.bufs[lb].clone());
            moved.insert(lb, b.bufs.len() - 1);
        }
        let mut syms = HashMap::new();
        for (k, v) in pb.syms {
            let keep = match &v {
                Sym::Var(x) => x.local,
                Sym::Ds(x) => x.local,
                _ => b.syms.get(&k) != Some(&v),
            };
            if !keep {
                continue;
            }
            let v = match v {
                Sym::Var(mut x) if x.local && moved.contains_key(&x.buf) => {
                    x.buf = moved[&x.buf];
                    x.local = false;
                    Sym::Var(x)
                }
                Sym::Ds(mut x) if x.local && moved.contains_key(&x.buf) => {
                    x.buf = moved[&x.buf];
                    x.local = false;
                    Sym::Ds(x)
                }
                other => other,
            };
            syms.insert(k, v);
        }
        let (params, returns) = match pi {
            Some((_, r, p, _)) => (p, r),
            None => (vec![], None),
        };
        // ON-EXIT splits the calculations: the rest is the exit code.
        let split = pr.calcs.iter().position(
            |c| matches!(&c.what, Line::Stmt(Stmt::Op { code, .. }) if code == "ON-EXIT"),
        );
        let (body, on_exit) = match split {
            Some(k) => {
                let ind = match &pr.calcs[k].what {
                    Line::Stmt(Stmt::Op { f2, .. }) => expr_name(f2),
                    _ => None,
                };
                let exit = code::build(&pr.calcs[k + 1..])
                    .map_err(|e| anyhow!("procedure {} ON-EXIT: {e}", pr.name))?;
                (&pr.calcs[..k], Some((ind, exit)))
            }
            None => (&pr.calcs[..], None),
        };
        procs.insert(
            pr.name.clone(),
            ProcInfo {
                name: pr.name.clone(),
                syms,
                frame: pb.bufs,
                params,
                returns,
                routines: code::build(body).map_err(|e| anyhow!("procedure {}: {e}", pr.name))?,
                on_exit,
                export: pr.export,
            },
        );
    }
    // A procedure interface outside any procedure is the program's entry
    // parameter list (free-form *ENTRY PLIST).
    let mut entry = src.entry.clone().unwrap_or_default();
    if let Some((_, _, params, _)) = &global_pi {
        if entry.is_empty() {
            entry = params
                .iter()
                .map(|p| Parm {
                    target: Expr::name(&p.name),
                    value: None,
                })
                .collect();
        }
    }
    let main = keyword(&src.control, "MAIN")
        .and_then(|k| k.args.first())
        .map(|a| a.trim().to_ascii_uppercase());
    if let Some(m) = &main {
        if !procs.contains_key(m) {
            bail!("ctl-opt main({m}): there is no procedure {m}");
        }
    }
    let routines = code::build(&src.calcs)?;
    // Imports: procedures the code calls through a prototype that no
    // procedure here defines (ILE: resolved when the module is bound).
    let calls = format!(
        "{:?}{:?}",
        src.calcs,
        src.procs.iter().map(|p| &p.calcs).collect::<Vec<_>>()
    );
    // A call of an OVERLOAD prototype may be a call of any of its
    // candidates.
    let called = |n: &str| calls.contains(&format!("parts: [{n:?}]"));
    let via_overload: Vec<String> = protos
        .values()
        .filter(|p| !p.overload.is_empty() && called(&p.name))
        .flat_map(|p| p.overload.clone())
        .collect();
    let mut imports: Vec<String> = protos
        .values()
        .filter(|p| p.extpgm.is_none() && p.overload.is_empty())
        .filter(|p| called(&p.name) || via_overload.contains(&p.name))
        .filter_map(|p| p.extproc.clone())
        .map(|t| t.to_ascii_uppercase())
        .filter(|t| !procs.contains_key(t) && !SYSTEM_PROCEDURES.contains(&t.as_str()))
        .collect();
    imports.sort();
    imports.dedup();
    let nomain = keyword(&src.control, "NOMAIN").is_some();
    if nomain && !src.calcs.is_empty() {
        bail!("ctl-opt NOMAIN: a module without a main procedure has no calculations outside procedures (RNF3788)");
    }
    // Every occurrence of a multiple-occurrence data structure starts as
    // the first one was set up (blanks, INZ values).
    for o in &b.occurs {
        let first = b.bufs[o.buf][..o.size].to_vec();
        for k in 1..o.count as usize {
            b.bufs[o.buf][k * o.size..(k + 1) * o.size].copy_from_slice(&first);
        }
    }
    let sequence: HashMap<String, char> = src
        .defs
        .iter()
        .filter_map(|d| {
            if keyword(&d.keywords, "DESCEND").is_some() {
                Some((d.name.clone(), 'D'))
            } else if keyword(&d.keywords, "ASCEND").is_some() {
                Some((d.name.clone(), 'A'))
            } else {
                None
            }
        })
        .collect();
    let varying: HashMap<String, char> = src
        .defs
        .iter()
        .filter_map(|d| {
            let k = keyword(&d.keywords, "DIM")?;
            match k.args.first()?.trim().to_ascii_uppercase().as_str() {
                "*AUTO" => Some((d.name.clone(), 'A')),
                "*VAR" => Some((d.name.clone(), 'V')),
                _ => None,
            }
        })
        .collect();
    // EXPORT and IMPORT on definitions (ILE RPG Reference): data shared
    // between the modules of a program and its service programs, by
    // symbol (the keyword's name, or the definition's).
    let shared = |k: &str| -> Vec<(String, String)> {
        src.defs
            .iter()
            .filter(|d| matches!(d.kind, DefKind::Field | DefKind::Ds))
            .filter_map(|d| {
                let kw = keyword(&d.keywords, k)?;
                let sym = kw
                    .args
                    .first()
                    .map(|a| a.trim().trim_matches('\'').to_ascii_uppercase())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| d.name.clone());
                Some((sym, d.name.clone()))
            })
            .collect()
    };
    let data_exports = shared("EXPORT");
    let data_imports = shared("IMPORT");
    Ok(Program {
        data_exports,
        data_imports,
        varying,
        sequence,
        occurs: b.occurs.clone(),
        imports,
        nomain,
        name: name.to_ascii_uppercase(),
        syms: b.syms,
        globals: b.bufs,
        files,
        routines,
        entry,
        plists: src.plists.clone(),
        outputs: src.outputs.clone(),
        control: src.control.clone(),
        dtaaras: data_areas(src),
        psds: src
            .defs
            .iter()
            .find(|d| d.kind == DefKind::Ds && keyword(&d.keywords, "PSDS").is_some())
            .map(ds_key),
        auto_dtaaras: auto_dtaaras(src),
        protos,
        procs,
        main,
        cursors,
    })
}
