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
    /// An array overlaying an array of a data structure: the bytes from
    /// the start of the overlaid element to this one's (SORTA moves whole
    /// elements).
    pub lead: usize,
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
    /// The printer file's DDS, for externally described PRINTER files.
    pub printer: Option<Arc<crate::prtf::PrinterFile>>,
    /// The printer file object's page length, width and overflow line
    /// (CRTPRTF PAGESIZE, OVRFLW), when there is one.
    pub page: Option<(i32, i32, i32)>,
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
    /// OPTIONS(*STRING) on a pointer: a character value is passed as a
    /// pointer to a null-terminated copy of it.
    pub string: bool,
    /// OPTIONS(*VARSIZE): the argument may be shorter or longer than the
    /// parameter.
    pub varsize: bool,
    /// OPTIONS(*TRIM): a character argument is passed with its leading and
    /// trailing blanks removed (ILE RPG Reference, OPTIONS(*TRIM)).
    pub trim: bool,
    /// OPTIONS(*CONVERT): a numeric, date, time or timestamp argument to a
    /// character parameter passed by value or read-only reference is
    /// converted as %CHAR converts it (ILE RPG Reference, OPTIONS).
    pub convert: bool,
    /// In a procedure interface: the buffer the parameter is bound to
    /// (aliased to the caller's storage when passed by reference).
    pub buf: Option<usize>,
    /// LIKEFILE(file): a file is passed (the caller's open file).
    pub file: bool,
}

/// A prototype (DCL-PR / PR): what a call passes and where it goes.
#[derive(Clone, Debug)]
pub struct Proto {
    pub name: String,
    /// EXTPGM: a program call.
    pub extpgm: Option<String>,
    /// EXTPROC, or the prototype name: a procedure.
    pub extproc: Option<String>,
    /// EXTPROC(pointer): a call goes to the procedure the procedure
    /// pointer of this name addresses (ILE RPG Reference, EXTPROC).
    pub extproc_ptr: Option<String>,
    /// EXTPROC('name') as written: the procedure's exported name, its
    /// case kept (the call stack shows it).
    pub written: Option<String>,
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
    /// The external name its interface gives (EXTPROC('name'),
    /// EXTPROC(*DCLCASE)), its case kept.
    pub written: Option<String>,
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
    /// Based storage (BASED): frame buffer → the basing pointer's name.
    pub based: HashMap<usize, String>,
    /// Local pointers with INZ(%ADDR(x)): set at each call.
    pub ptr_inz: Vec<(String, Expr)>,
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
    /// The commit level embedded SQL runs with: SET OPTION COMMIT, else
    /// the CRTSQLRPGI COMMIT parameter (kept as the SQLCOMMIT option),
    /// else *NONE: *NONE, *CHG, *CS, *ALL or *RR.
    pub sql_commit: String,
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
    /// Based storage (BASED): global buffer → the basing pointer's name.
    pub based: HashMap<usize, String>,
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
    /// Global fields with INZ(*USER).
    pub user_inz: Vec<String>,
    /// Global pointers with INZ(%ADDR(x)) or INZ(%PADDR(x)): set as the
    /// program starts.
    pub ptr_inz: Vec<(String, Expr)>,
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

/// Statements the SQL precompiler takes as declarations, not calculations
/// (Db2 for i SQL Reference: SET OPTION, DECLARE CURSOR, INCLUDE and
/// WHENEVER are not executable), so a NOMAIN module may have them in its
/// global section.
fn declarative_sql(c: &CalcLine) -> bool {
    let Line::Stmt(Stmt::Sql(s)) = &c.what else {
        return false;
    };
    let w: Vec<String> = s
        .split_whitespace()
        .take(4)
        .map(|x| x.to_ascii_uppercase())
        .collect();
    match w.first().map(String::as_str) {
        Some("SET") => w.get(1).map(String::as_str) == Some("OPTION"),
        Some("INCLUDE" | "WHENEVER") => true,
        Some("DECLARE") => w.iter().any(|x| x == "CURSOR" || x == "STATEMENT"),
        _ => false,
    }
}

/// The commit level of a program's embedded SQL (Db2 for i SQL Reference,
/// SET OPTION COMMIT: *NONE or *NC, *CHG or *UR, *CS, *ALL or *RS, *RR).
fn sql_commit(src: &Source) -> String {
    let norm = |v: &str| -> String {
        match v.trim().trim_end_matches(';').to_ascii_uppercase().as_str() {
            "*CHG" | "*UR" => "*CHG",
            "*CS" => "*CS",
            "*ALL" | "*RS" => "*ALL",
            "*RR" => "*RR",
            _ => "*NONE",
        }
        .to_string()
    };
    for s in
        sql_statements(&src.calcs).chain(src.procs.iter().flat_map(|p| sql_statements(&p.calcs)))
    {
        let up = s.to_ascii_uppercase();
        let w: Vec<&str> = up.split_whitespace().collect();
        if w.len() >= 2 && w[0] == "SET" && w[1] == "OPTION" {
            let rest = up.split_once("OPTION").map(|x| x.1).unwrap_or("");
            for opt in rest.split(',') {
                if let Some((k, v)) = opt.split_once('=') {
                    if k.trim() == "COMMIT" {
                        return norm(v);
                    }
                }
            }
        }
    }
    match keyword(&src.control, "SQLCOMMIT").and_then(|k| k.args.first()) {
        Some(v) => norm(v),
        None => "*NONE".into(),
    }
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
    // DTAARA(*VAR : name): the name is a variable (or a literal) whose
    // value names the data area when the program runs: "*VAR:" + it.
    if let Some(k) = keyword(&d.keywords, "DTAARA") {
        if let Some(i) = k
            .args
            .iter()
            .position(|a| a.trim().eq_ignore_ascii_case("*VAR"))
        {
            if let Some(v) = k.args.get(i + 1) {
                let v = v.trim();
                return if v.starts_with('\'') {
                    v.trim_matches('\'').to_ascii_uppercase()
                } else {
                    format!("*VAR:{}", v.to_ascii_uppercase())
                };
            }
        }
    }
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
            .or_else(|| {
                self.files
                    .iter()
                    .position(|f| f.printer.as_ref().is_some_and(|p| p.record(name).is_some()))
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

/// A printer file for the program: its attributes and its DDS (none when
/// it was created without DDS).
pub type PrinterInfo = (
    crate::prtf::Attributes,
    Option<Arc<crate::prtf::PrinterFile>>,
);

/// The printer files a source uses: (file, the name of the object).
pub fn printer_files(src: &Source) -> Vec<String> {
    src.files
        .iter()
        .filter(|f| f.device == "PRINTER")
        .map(|f| {
            keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone())
        })
        .collect()
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
    match f.dtype {
        'L' => return DType::Date(crate::rpg::dates::DateFmt::iso()),
        'T' => return DType::Time(crate::rpg::dates::TimeFmt::iso()),
        'Z' => return DType::Timestamp(6),
        _ => {}
    }
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
        // %LEN of a defined item of fixed length: its length in
        // characters, or its digits (ILE RPG Reference, %LEN: allowed in
        // definitions for such items).
        Expr::Bif(f, a) if f == "LEN" && a.len() == 1 => {
            let n = match &a[0] {
                Expr::Name { parts, args: None } => match syms.get(&parts.join(".")) {
                    Some(Sym::Var(v)) => match v.dtype {
                        DType::Char(n) | DType::Ucs2(n) => n as i64,
                        DType::Varchar(_) | DType::VarUcs2(_) => {
                            bail!("%LEN of a varying-length item is not a constant")
                        }
                        t if t.is_numeric() => t.precision().0 as i64,
                        t => t.size() as i64,
                    },
                    Some(Sym::Ds(d)) => d.size as i64,
                    Some(Sym::Const(Value::Char(c))) => c.len() as i64,
                    _ => bail!(
                        "%LEN({}): it is not defined before it is used",
                        parts.join(".")
                    ),
                },
                Expr::Str(c) => c.len() as i64,
                other => bail!("a constant value is needed, found {other:?}"),
            };
            Value::Num(Dec::from_i64(n))
        }
        // %DECPOS of a numeric item (ILE RPG Reference, %DECPOS: allowed
        // in definitions).
        // A date or time literal (D'...', T'...'): its *ISO text.
        Expr::Bif(f, a)
            if (f == "DATE" || f == "TIME")
                && matches!(a.as_slice(), [Expr::Str(_), Expr::Name { parts, .. }] if parts[0] == "*ISO") =>
        {
            let Expr::Str(s) = &a[0] else { unreachable!() };
            Value::Char(s.clone())
        }
        Expr::Bif(f, a) if f == "DECPOS" && a.len() == 1 => {
            let Expr::Name { parts, args: None } = &a[0] else {
                bail!("%DECPOS needs the name of a numeric item");
            };
            let name = parts.join(".");
            match syms.get(&name) {
                Some(Sym::Var(v)) if v.dtype.is_numeric() => {
                    Value::Num(Dec::from_i64(v.dtype.precision().1 as i64))
                }
                _ => bail!("%DECPOS({name}): it is not a numeric item defined before it is used"),
            }
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
        // By the name other modules call it by (EXTPROC aliases too).
        let mut v: Vec<String> = self
            .procs
            .iter()
            .filter(|(_, p)| p.export)
            .map(|(k, _)| k.clone())
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
pub fn program_type(f: &crate::dds::FieldDef, in_ds: bool, extbinint: bool) -> Option<DType> {
    Some(match f.data_type {
        DataType::Char if f.varlen => DType::Varchar(f.length),
        DataType::Char => DType::Char(f.length),
        // Hexadecimal: character data the program gets byte for byte.
        DataType::Hex if f.varlen => DType::Varchar(f.length),
        DataType::Hex => DType::Char(f.length),
        DataType::Float => DType::Float(if f.double { 8 } else { 4 }),
        DataType::Graphic if f.varlen => DType::VarUcs2(f.length),
        DataType::Graphic => DType::Ucs2(f.length),
        DataType::Clob
        | DataType::Blob
        | DataType::Dbclob
        | DataType::Decfloat
        | DataType::Boolean
        | DataType::Xml => return None,
        DataType::Binary if extbinint && f.decimals == 0 => DType::Int(match f.length {
            0..=4 => 5,
            5..=9 => 10,
            _ => 20,
        }),
        DataType::Binary => DType::Binary(f.length, f.decimals),
        DataType::Date => DType::Date(crate::rpg::dates::DateFmt::iso()),
        DataType::Time => DType::Time(crate::rpg::dates::TimeFmt::iso()),
        DataType::Timestamp => DType::Timestamp(6),
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
    /// INZ(*LIKEDS) copies of a template's storage made once the pending
    /// INZ values are in: (template in this binder's storage (not the
    /// globals), its buffer, offset, size, target buffer, target offsets).
    inz_copies: Vec<(bool, usize, usize, usize, usize, Vec<usize>)>,
    /// INZ(*USER): set to the job's user profile when the program starts.
    user_inz: Vec<String>,
    /// INZ(%ADDR(x)) of pointers: (pointer, the expression).
    ptr_inz: Vec<(String, String)>,
    /// Binding a procedure's local storage.
    local: bool,
    /// The procedure interface's parameters LIKEDS a data structure: name
    /// → the template (a local DS may be LIKEDS such a parameter, and the
    /// interface is bound after the data structures).
    param_likeds: HashMap<String, String>,
    /// Local buffers of STATIC variables (moved to global storage).
    statics: Vec<usize>,
    /// Global storage, seen (for templates) while binding a procedure.
    globals: Vec<Vec<u8>>,
    /// EXTBININT(*YES): external binary fields are integers.
    extbinint: bool,
    /// The control specification's DATFMT and TIMFMT (*ISO by default).
    datfmt: crate::rpg::dates::DateFmt,
    timfmt: crate::rpg::dates::TimeFmt,
    /// Multiple-occurrence data structures (OCCURS).
    occurs: Vec<Occurs>,
    /// Record formats of the program's files (for LIKEREC): the fields
    /// (program names, types) and the key fields.
    formats: HashMap<String, RecordFormat>,
    /// File information data structures (INFDS on a file).
    infds: Vec<String>,
    /// Based storage: buffer → basing pointer name.
    based: HashMap<usize, String>,
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
        let first = k.args.first().map(|a| a.trim().to_ascii_uppercase());
        // EXTPROC(*CL | *CWIDEN | *CNOWIDEN : 'name'): the name is the
        // second argument; EXTPROC(*DCLCASE): the prototype's name as
        // declared (ILE RPG Reference, EXTPROC). Names are matched
        // without regard to case here.
        let arg = match first.as_deref() {
            Some("*DCLCASE") => return None,
            Some("*CL" | "*CWIDEN" | "*CNOWIDEN") => k.args.get(1),
            _ => k.args.first(),
        };
        arg.map(|a| a.trim().trim_matches('\'').to_ascii_uppercase())
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
        {
            let mut in_pi = false;
            for d in defs {
                match d.kind {
                    DefKind::Interface => in_pi = true,
                    DefKind::Param if in_pi => {
                        if let Some(t) = keyword(&d.keywords, "LIKEDS").and_then(|k| k.args.first())
                        {
                            self.param_likeds
                                .insert(d.name.to_ascii_uppercase(), t.trim().to_ascii_uppercase());
                        }
                    }
                    _ => in_pi = false,
                }
            }
        }
        // Prototypes are bound last: they allocate nothing, and a
        // parameter's LIKEDS may name a data structure defined after them.
        let mut order: Vec<usize> = Vec::new();
        let mut later: Vec<usize> = Vec::new();
        let mut like: Vec<usize> = Vec::new();
        let after_like: Vec<usize>;
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
        // A data structure with a subfield LIKEDS a data structure, or LIKE
        // a field, defined later comes after it (the compiler takes
        // forward references: OSSILE's message_t has a subfield
        // LIKEDS(messageSender_t), defined after it; RPGUnit's templates
        // have subfields LIKE(ProcNm_t)).
        {
            let ds_deps = |i: usize| -> Vec<String> {
                let mut v = Vec::new();
                let mut k = i;
                loop {
                    for kwn in ["LIKEDS", "LIKE"] {
                        if let Some(kw) = keyword(&defs[k].keywords, kwn) {
                            if let Some(a) = kw.args.first() {
                                // LIKE(ds.subfield): the data structure.
                                let a = a.trim().to_ascii_uppercase();
                                v.push(a.split('.').next().unwrap_or(&a).to_string());
                            }
                        }
                    }
                    k += 1;
                    if k >= defs.len() || defs[k].kind != DefKind::Subfield {
                        break;
                    }
                }
                v
            };
            let is_ds = |i: usize| defs[i].kind == DefKind::Ds;
            let mut placed: Vec<usize> = Vec::new();
            let mut pending: Vec<usize> = order.clone();
            loop {
                let unplaced: Vec<String> = pending
                    .iter()
                    .filter(|&&i| is_ds(i) || defs[i].kind == DefKind::Field)
                    .map(|&i| defs[i].name.clone())
                    .collect();
                let (ready, wait): (Vec<usize>, Vec<usize>) = pending.iter().partition(|&&i| {
                    !is_ds(i)
                        || !ds_deps(i)
                            .iter()
                            .any(|d| *d != defs[i].name && unplaced.contains(d))
                });
                if ready.is_empty() {
                    placed.extend(wait);
                    break;
                }
                placed.extend(ready);
                if wait.is_empty() {
                    break;
                }
                pending = wait;
            }
            order = placed;
            // A data structure with a subfield LIKE a standalone field that
            // is itself LIKE another (bound with the LIKE fields, below)
            // comes after those fields; so do data structures that depend
            // on it (iBMi-topics' QWc_JIDF0100: JobUser like(JobUser), the
            // global JobUser like(UserProfile)).
            let like_fields: Vec<String> = like
                .iter()
                .map(|&i| defs[i].name.to_ascii_uppercase())
                .collect();
            let mut moved: Vec<usize> = Vec::new();
            loop {
                let moved_names: Vec<String> = moved
                    .iter()
                    .map(|&i| defs[i].name.to_ascii_uppercase())
                    .collect();
                let more: Vec<usize> = order
                    .iter()
                    .copied()
                    .filter(|&i| is_ds(i) && !moved.contains(&i))
                    .filter(|&i| {
                        ds_deps(i).iter().any(|d| {
                            *d != defs[i].name.to_ascii_uppercase()
                                && (like_fields.contains(d) || moved_names.contains(d))
                        })
                    })
                    .collect();
                if more.is_empty() {
                    break;
                }
                moved.extend(more);
            }
            order.retain(|i| !moved.contains(i));
            moved.sort();
            after_like = moved;
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
        // Constants whose value names a field LIKE another (%LEN(x),
        // %DECPOS(x) of such a field) come after those fields.
        let like_names: Vec<String> = like
            .iter()
            .map(|&i| defs[i].name.to_ascii_uppercase())
            .collect();
        let mut late_consts: Vec<usize> = Vec::new();
        if !like_names.is_empty() {
            order.retain(|&i| {
                if defs[i].kind != DefKind::Const {
                    return true;
                }
                let text: String = defs[i]
                    .keywords
                    .iter()
                    .flat_map(|k| k.args.iter())
                    .map(|a| a.to_ascii_uppercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                let names = text
                    .split(|c: char| !(c.is_ascii_alphanumeric() || "_#$@".contains(c)))
                    .any(|w| like_names.iter().any(|n| n == w));
                if names {
                    late_consts.push(i);
                }
                !names
            });
        }
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
        order.extend(after_like);
        order.extend(late_consts);
        order.extend(later);
        // Named constants of a literal value may be used before they are
        // defined (DIM(n) above DCL-C n): they come first.
        let literal_const = |i: usize| {
            defs[i].kind == DefKind::Const
                && defs[i].keywords.iter().all(|k| {
                    k.args.iter().all(|a| {
                        let t = a.trim();
                        t.starts_with('\'')
                            || t.to_ascii_uppercase().starts_with("X'")
                            || (!t.is_empty()
                                && t.chars().all(|c| c.is_ascii_digit() || ".,+-".contains(c)))
                    })
                })
        };
        let (first, rest): (Vec<usize>, Vec<usize>) =
            order.iter().partition(|&&i| literal_const(i));
        order = first.into_iter().chain(rest).collect();
        // A procedure's local field may be LIKE one of its parameters
        // (ILE RPG Reference, LIKE: any field, parameters included),
        // although the interface is bound after the fields: the
        // parameters' types are known for LIKE meanwhile.
        let mut param_types: Vec<String> = Vec::new();
        let mut in_pi = false;
        for d in defs {
            match d.kind {
                DefKind::Interface => in_pi = true,
                DefKind::Param if in_pi => {
                    if !self.file_fields.contains_key(&d.name) && !self.syms.contains_key(&d.name) {
                        if let Ok(Some(t)) = self.resolve_type(d) {
                            self.file_fields.insert(d.name.clone(), t);
                            param_types.push(d.name.clone());
                        }
                    }
                }
                _ => in_pi = false,
            }
        }
        let r = self.bind_ordered(defs, &order, protos, &mut pi);
        for n in param_types {
            self.file_fields.remove(&n);
        }
        r?;
        Ok(pi)
    }

    fn bind_ordered(
        &mut self,
        defs: &[Def],
        order: &[usize],
        protos: &mut HashMap<String, Proto>,
        pi: &mut Option<Interface>,
    ) -> Result<()> {
        for &i0 in order {
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
                    if let Some(p) = basing_pointer(d) {
                        self.based.insert(v.buf, p);
                    } else {
                        self.inz(&d.name.clone(), d)?;
                    }
                }
                DefKind::Ds => {
                    self.ds(defs, i)
                        .map_err(|e| anyhow!("line {}: {e}", d.line))?;
                    if self.local && keyword(&d.keywords, "STATIC").is_some() {
                        self.statics.push(self.bufs.len() - 1);
                    }
                    if let (Some(p), Some(Sym::Ds(ds))) =
                        (basing_pointer(d), self.syms.get(&d.name))
                    {
                        self.based.insert(ds.buf, p);
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
                        let likefile = keyword(&p.keywords, "LIKEFILE").is_some();
                        let (dtype, buf) = if likefile {
                            (DType::Char(1), None)
                        } else if interface {
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
                            string: keyword(&p.keywords, "OPTIONS").is_some_and(|k| {
                                k.args
                                    .iter()
                                    .any(|a| a.trim().eq_ignore_ascii_case("*STRING"))
                            }),
                            varsize: keyword(&p.keywords, "OPTIONS").is_some_and(|k| {
                                k.args
                                    .iter()
                                    .any(|a| a.trim().eq_ignore_ascii_case("*VARSIZE"))
                            }),
                            trim: keyword(&p.keywords, "OPTIONS").is_some_and(|k| {
                                k.args
                                    .iter()
                                    .any(|a| a.trim().eq_ignore_ascii_case("*TRIM"))
                            }),
                            convert: keyword(&p.keywords, "OPTIONS").is_some_and(|k| {
                                k.args
                                    .iter()
                                    .any(|a| a.trim().eq_ignore_ascii_case("*CONVERT"))
                            }),
                            buf,
                            file: likefile,
                        });
                        j += 1;
                    }
                    // A return value LIKEDS a data structure (ILE RPG
                    // Reference, LIKEDS on a prototype or interface): its
                    // bytes.
                    let returns = match self.resolve_type(d)? {
                        Some(t) => Some(t),
                        None => match keyword(&d.keywords, "LIKEDS")
                            .and_then(|k| k.args.first())
                            .map(|a| a.trim().to_ascii_uppercase())
                        {
                            // With DIM: the elements' bytes, one after another.
                            Some(n) => match self.syms.get(&n) {
                                Some(Sym::Ds(ds)) => {
                                    let dim = match keyword(&d.keywords, "DIM")
                                        .and_then(|k| k.args.first())
                                    {
                                        Some(a) => num_arg(a, &self.syms)?.max(1),
                                        None => 1,
                                    };
                                    Some(DType::Char(ds.size as u32 * dim))
                                }
                                _ => bail!("line {}: LIKEDS({n}): not a data structure", d.line),
                            },
                            None => None,
                        },
                    };
                    if interface {
                        *pi = Some((d.name.clone(), returns, params, d.keywords.clone()));
                    } else {
                        let extpgm = ext_name(&d.keywords, "EXTPGM")
                            .map(|n| n.unwrap_or_else(|| d.name.clone()));
                        // EXTPROC(name) without quotes: a named constant
                        // with the procedure's name, or a procedure
                        // pointer.
                        let unquoted = keyword(&d.keywords, "EXTPROC")
                            .and_then(|k| k.args.first().cloned())
                            .map(|a| a.trim().to_string())
                            .filter(|a| {
                                !a.is_empty() && !a.starts_with('\'') && !a.starts_with('*')
                            })
                            .map(|a| a.to_ascii_uppercase());
                        let mut extproc_ptr = None;
                        let extproc = if extpgm.is_some() {
                            None
                        } else if let Some(n) = unquoted {
                            match self.syms.get(&n) {
                                Some(Sym::Const(v)) => Some(
                                    crate::rpg::data::a(&v.chars()?).trim().to_ascii_uppercase(),
                                ),
                                _ => {
                                    extproc_ptr = Some(n);
                                    None
                                }
                            }
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
                                extproc_ptr,
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
                                written: keyword(&d.keywords, "EXTPROC")
                                    .and_then(|k| match k.args.first() {
                                        Some(a) if a.trim().starts_with('*') => k.args.get(1),
                                        a => a,
                                    })
                                    .map(|a| a.trim())
                                    .filter(|a| a.starts_with('\''))
                                    .map(|a| a.trim_matches('\'').to_string()),
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
        // A basing pointer that is not defined is defined implicitly
        // (ILE RPG Reference, BASED keyword).
        let mut implicit: Vec<String> = self.based.values().cloned().collect();
        implicit.sort();
        implicit.dedup();
        for p in implicit {
            if !self.syms.contains_key(&p) && !p.contains('.') {
                self.add_var(&p, DType::Pointer, 0);
            }
        }
        Ok(())
    }

    /// Store the pending INZ values.
    fn apply_inz(&mut self) -> Result<()> {
        for (name, v) in std::mem::take(&mut self.inz) {
            // INZ(*LIKEDS) on the data structure: the template's values
            // were copied already.
            if matches!(&v, Err(t) if t.trim().eq_ignore_ascii_case("*LIKEDS")) {
                continue;
            }
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
        // Then INZ(*LIKEDS) data structures take their templates' storage,
        // in the order they were defined (a template can itself hold an
        // INZ(*LIKEDS) subfield).
        for (own, sbuf, soff, size, dbuf, at) in std::mem::take(&mut self.inz_copies) {
            let from = if own { &self.bufs } else { &self.globals };
            let Some(bytes) = from
                .get(sbuf)
                .and_then(|b| b.get(soff..soff + size))
                .map(|b| b.to_vec())
            else {
                continue;
            };
            for a in at {
                if let Some(dst) = self.bufs[dbuf].get_mut(a..a + size) {
                    dst.copy_from_slice(&bytes);
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
        // A date or time: the DATFMT (TIMFMT) of the definition, else the
        // control specification's, else *ISO (ILE RPG Reference, "Date
        // Data Type", "Time Data Type").
        match d.dtype {
            Some(DType::Date(f)) => {
                let f = match keyword(&d.keywords, "DATFMT").and_then(|k| k.args.first()) {
                    Some(a) => crate::rpg::dates::DateFmt::parse(a)?,
                    None => f.or(self.datfmt),
                };
                return Ok(Some(DType::Date(f)));
            }
            Some(DType::Time(f)) => {
                let f = match keyword(&d.keywords, "TIMFMT").and_then(|k| k.args.first()) {
                    Some(a) => crate::rpg::dates::TimeFmt::parse(a)?,
                    None => f.or(self.timfmt),
                };
                return Ok(Some(DType::Time(f)));
            }
            Some(t) => return Ok(Some(t)),
            None => {}
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
                // POINTER(*PROC): a procedure pointer.
                "POINTER" => DType::Pointer,
                "DATE" => {
                    let f = match k.args.first() {
                        Some(a) if !a.trim().is_empty() => crate::rpg::dates::DateFmt::parse(a)?,
                        _ => self.datfmt,
                    };
                    DType::Date(f)
                }
                "TIME" => {
                    let f = match k.args.first() {
                        Some(a) if !a.trim().is_empty() => crate::rpg::dates::TimeFmt::parse(a)?,
                        _ => self.timfmt,
                    };
                    DType::Time(f)
                }
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
                // LIKE of a data structure (a template too): character,
                // as long as the data structure (ILE RPG Reference, LIKE
                // keyword).
                Some(Sym::Ds(ds)) => DType::Char(ds.size as u32),
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
            if let Some(a) = k
                .args
                .first()
                .filter(|a| a.trim().eq_ignore_ascii_case("*USER"))
            {
                // INZ(*USER) (ILE RPG Reference, INZ): the name of the
                // job's user profile, known when the program runs.
                let _ = a;
                if self.local {
                    bail!("INZ(*USER) on a local variable of a procedure is not supported yet");
                }
                self.user_inz.push(name.to_string());
                return Ok(());
            }
            // A pointer's INZ(%ADDR(x)) or INZ(%PADDR(x)) (ILE RPG
            // Reference, INZ: a pointer can be initialized with %ADDR or
            // %PADDR): set when the storage is (program start, call).
            if let Some(a) = k.args.first().filter(|a| {
                let u = a.trim().to_ascii_uppercase();
                u.starts_with("%ADDR") || u.starts_with("%PADDR")
            }) {
                self.ptr_inz.push((name.to_string(), a.trim().to_string()));
                return Ok(());
            }
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
        // A program's storage is one piece: a pointer to a short field
        // may read on past it (a 16-byte pointer read through a 5-byte
        // handle, RPGUnit's EXTTSTT). Some bytes after the field stand in
        // for the storage that follows (fidelity-gaps).
        if dim == 0 && buf.len() < 16 {
            buf.resize(16, 0);
        }
        self.bufs.push(buf);
        let v = VarInfo {
            dtype,
            buf: self.bufs.len() - 1,
            off: 0,
            dim,
            stride: dtype.size(),
            local: self.local,
            lead: 0,
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
            // PREFIX(prefix [: n]) (ILE RPG Reference, PREFIX): the
            // prefix put in front of each name, in place of its first n
            // characters.
            let prefix = keyword(&d.keywords, "PREFIX").map(|k| {
                (
                    k.args
                        .first()
                        .map(|p| p.trim().trim_matches('\'').to_ascii_uppercase())
                        .unwrap_or_default(),
                    k.args
                        .get(1)
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .unwrap_or(0),
                )
            });
            for fd in &fr.file.fields {
                let Some(t) = program_type(fd, true, self.extbinint) else {
                    continue;
                };
                let n = match (&fd.alias, alias) {
                    (Some(a), true) => a.clone(),
                    _ => fd.name.clone(),
                };
                let n = match &prefix {
                    Some((p, k)) => format!("{p}{}", n.chars().skip(*k).collect::<String>()),
                    None => n,
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
            // A qualified file's format: FILE.FORMAT.
            let fmt = fmt.rsplit('.').next().unwrap_or(fmt);
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
        let mut template: Option<(String, DsInfo)> = None;
        // Arrays that overlay an array subfield (ILE RPG Reference,
        // OVERLAY: "the overlaying field is also an array"): name →
        // (elements, stride, lead).
        let mut shape: HashMap<String, (u32, usize, usize)> = HashMap::new();
        // Group subfields (no type or length) → their index in `subs`.
        let mut groups: HashMap<String, usize> = HashMap::new();
        if let Some(other) = &likeds {
            let found = match self.syms.get(other) {
                Some(Sym::Ds(o)) => Some((other.clone(), o.clone())),
                _ => self
                    .param_likeds
                    .get(other)
                    .and_then(|t| match self.syms.get(t) {
                        Some(Sym::Ds(o)) => Some((t.clone(), o.clone())),
                        _ => None,
                    }),
            };
            let Some((tname, o)) = found else {
                bail!("LIKEDS({other}): not a data structure")
            };
            let other = &tname;
            template = Some((other.clone(), o.clone()));
            for sf in &o.subfields {
                let key = if o.qualified {
                    format!("{other}.{sf}")
                } else {
                    sf.clone()
                };
                match self.syms.get(&key).cloned() {
                    Some(Sym::Var(v)) => {
                        if v.stride != v.dtype.size() || v.lead > 0 {
                            shape.insert(sf.clone(), (v.dim, v.stride, v.lead));
                            subs.push((sf.clone(), v.dtype, v.off - o.off, 0, d));
                        } else {
                            subs.push((sf.clone(), v.dtype, v.off - o.off, v.dim, d));
                        }
                    }
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
            // A subfield with no type or length that later subfields
            // overlay: a group, as long as they reach (ILE RPG Reference,
            // OVERLAY keyword).
            let is_group = t.is_none()
                && s.to.is_none()
                && defs[j + 1..]
                    .iter()
                    .take_while(|x| x.kind == DefKind::Subfield)
                    .any(|x| {
                        keyword(&x.keywords, "OVERLAY")
                            .and_then(|k| k.args.first())
                            .is_some_and(|a| a.trim().eq_ignore_ascii_case(&s.name))
                    });
            if is_group {
                groups.insert(s.name.to_ascii_uppercase(), subs.len());
                subs.push((s.name.clone(), DType::Char(0), off, 0, s));
                j += 1;
                continue;
            }
            let t = match t {
                Some(t) => t,
                None if s.to.is_some() => DType::Char(s.to.unwrap() + 1 - s.from.unwrap_or(1)),
                None => bail!("subfield {} needs a type and length", s.name),
            };
            let sdim = self.dim(s)?;
            // A subfield overlaying a group makes the group longer.
            if let Some(k) = keyword(&s.keywords, "OVERLAY") {
                let target = k
                    .args
                    .first()
                    .map(|a| a.trim().to_ascii_uppercase())
                    .unwrap_or_default();
                if let Some(&g) = groups.get(&target) {
                    let end = off + t.size() * sdim.max(1) as usize;
                    let len = end.saturating_sub(subs[g].2);
                    if let DType::Char(n) = subs[g].1 {
                        if len as u32 > n {
                            subs[g].1 = DType::Char(len as u32);
                        }
                    }
                }
            }
            if let Some(k) = keyword(&s.keywords, "OVERLAY") {
                let target = k
                    .args
                    .first()
                    .map(|a| a.trim().to_ascii_uppercase())
                    .unwrap_or_default();
                if sdim == 0 {
                    if let Some(x) = subs.iter().find(|x| x.0 == target) {
                        let (tdim, tstride, tlead) = match shape.get(&target) {
                            Some(&sh) => sh,
                            None => (x.3, x.1.size(), 0),
                        };
                        if tdim > 0 {
                            shape.insert(s.name.clone(), (tdim, tstride, tlead + off - x.2));
                        }
                    }
                }
            }
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
            .or(d.to.map(|t| t as usize))
            // LIKEDS: the template's length, LEN included (ILE RPG
            // Reference, LIKEDS: the new data structure has the same
            // subfields and length).
            .or(template.as_ref().map(|(_, o)| o.size));
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
            let (vdim, stride, lead) = shape.get(sname).copied().unwrap_or((*sdim, t.size(), 0));
            let v = VarInfo {
                dtype: *t,
                buf,
                off: *off,
                dim: vdim,
                stride,
                local: self.local,
                lead,
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
        // INZ(*LIKEDS): the subfields start with the template's initial
        // values (ILE RPG Reference, INZ keyword: "*LIKEDS").
        let inz_likeds = keyword(&d.keywords, "INZ").is_some_and(|k| {
            k.args
                .first()
                .is_some_and(|a| a.trim().eq_ignore_ascii_case("*LIKEDS"))
        });
        if let (true, Some((other, o))) = (inz_likeds, &template) {
            let src = if o.local == self.local {
                self.bufs.get(o.buf)
            } else if !o.local {
                self.globals.get(o.buf)
            } else {
                None
            };
            if let Some(bytes) = src
                .and_then(|b| b.get(o.off..o.off + o.size))
                .map(|b| b.to_vec())
            {
                for e in 0..reps as usize {
                    let at = e * size;
                    let n = bytes.len().min(size);
                    self.bufs[buf][at..at + n].copy_from_slice(&bytes[..n]);
                }
            }
            // A template in this storage gets its INZ values later: copy
            // it again then.
            if o.local == self.local {
                self.inz_copies.push((
                    true,
                    o.buf,
                    o.off,
                    o.size.min(size),
                    buf,
                    (0..reps as usize).map(|e| e * size).collect(),
                ));
            }
            let pending: Vec<(String, Result<Value, String>)> = self
                .inz
                .iter()
                .filter_map(|(n, v)| {
                    let sf = if o.qualified {
                        n.strip_prefix(&format!("{other}."))?.to_string()
                    } else if o.subfields.contains(n) {
                        n.clone()
                    } else {
                        return None;
                    };
                    Some((format!("{name}.{sf}"), v.clone()))
                })
                .collect();
            self.inz.extend(pending);
        }
        // A LIKEDS subfield is a qualified data structure at its offset,
        // with the template's subfields (and their own nested ones) under
        // its name: LIST.USERS.NAME.
        for (k, template, o) in &nested {
            let (sname, _, off, sdim, sdef) = &subs[*k];
            let sub_inz_likeds = keyword(&sdef.keywords, "INZ").is_some_and(|k| {
                k.args
                    .first()
                    .is_some_and(|a| a.trim().eq_ignore_ascii_case("*LIKEDS"))
            });
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
            // INZ(*LIKEDS) on the subfield: the template's initial values
            // (ILE RPG Reference, INZ keyword: "*LIKEDS"), known once the
            // pending INZ values of this storage are in.
            if sub_inz_likeds && o.local == self.local {
                let mut at = Vec::new();
                for e in 0..reps as usize {
                    for n in 0..(*sdim).max(1) as usize {
                        at.push(e * size + off + n * o.size);
                    }
                }
                self.inz_copies.push((true, o.buf, o.off, o.size, buf, at));
            }
            // INZ on the data structure: the LIKEDS subfield's own
            // subfields get their types' initial values too (ILE RPG
            // Reference, INZ: every subfield of the data structure; a
            // numeric one in a nested data structure is zero, not blanks).
            if has_inz && !inz_likeds {
                for (_, sym) in &add {
                    let Sym::Var(v) = sym else { continue };
                    let init = v.dtype.initial();
                    let n = init.len();
                    for e in 0..reps as usize {
                        for s in 0..(*sdim).max(1) as usize {
                            for k in 0..v.dim.max(1) as usize {
                                let at = e * size + s * o.size + v.off + k * v.stride;
                                if let Some(dst) = self.bufs[buf].get_mut(at..at + n) {
                                    dst.copy_from_slice(&init);
                                }
                            }
                        }
                    }
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

/// BASED(ptr): the basing pointer's name.
fn basing_pointer(d: &Def) -> Option<String> {
    keyword(&d.keywords, "BASED")
        .and_then(|k| k.args.first())
        .map(|a| a.trim().to_ascii_uppercase())
        .filter(|a| !a.is_empty())
}

/// Bind a parsed source. `externals`: descriptions of the external files
/// (by file name), from the catalog.
impl Program {
    /// The control specification's DATFMT.
    pub fn datfmt(&self) -> Result<crate::rpg::dates::DateFmt> {
        control_datfmt(&self.control)
    }

    /// The control specification's TIMFMT.
    pub fn timfmt(&self) -> Result<crate::rpg::dates::TimeFmt> {
        control_timfmt(&self.control)
    }

    /// The control specification's DECEDIT: the decimal point
    /// character, and whether a zero goes before it ('0.', '0,').
    /// *JOBRUN and no DECEDIT: the job's decimal format, which as4tainr
    /// keeps as IBM ships QDECFMT (blank: a period, no zero).
    pub fn decedit(&self) -> (char, bool) {
        match keyword(&self.control, "DECEDIT").and_then(|k| k.args.first()) {
            Some(a) => {
                let v = a.trim().trim_matches('\'');
                match v {
                    "," => (',', false),
                    "0," => (',', true),
                    "0." => ('.', true),
                    _ => ('.', false),
                }
            }
            None => ('.', false),
        }
    }
}

/// The control specification's DATFMT (*ISO when it has none).
pub fn control_datfmt(control: &[Keyword]) -> Result<crate::rpg::dates::DateFmt> {
    match keyword(control, "DATFMT").and_then(|k| k.args.first()) {
        Some(a) => crate::rpg::dates::DateFmt::parse(a),
        None => Ok(crate::rpg::dates::DateFmt::iso()),
    }
}

/// The control specification's TIMFMT (*ISO when it has none).
pub fn control_timfmt(control: &[Keyword]) -> Result<crate::rpg::dates::TimeFmt> {
    match keyword(control, "TIMFMT").and_then(|k| k.args.first()) {
        Some(a) => crate::rpg::dates::TimeFmt::parse(a),
        None => Ok(crate::rpg::dates::TimeFmt::iso()),
    }
}

pub fn bind(
    name: &str,
    src: &Source,
    externals: &HashMap<String, FileRef>,
    displays: &HashMap<String, (String, Arc<DisplayFile>)>,
    printers: &HashMap<String, PrinterInfo>,
) -> Result<Program> {
    let mut b = Binder {
        syms: HashMap::new(),
        bufs: vec![],
        externals,
        file_fields: HashMap::new(),
        inz: vec![],
        inz_copies: vec![],
        user_inz: vec![],
        ptr_inz: vec![],
        globals: vec![],
        extbinint: keyword(&src.control, "EXTBININT")
            .and_then(|k| k.args.first())
            .is_some_and(|a| a.trim().eq_ignore_ascii_case("*YES")),
        datfmt: control_datfmt(&src.control)?,
        timfmt: control_timfmt(&src.control)?,
        local: false,
        param_likeds: HashMap::new(),
        statics: vec![],
        occurs: vec![],
        based: HashMap::new(),
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
            printer: None,
            page: None,
        };
        if f.device == "PRINTER" {
            let ext = keyword(&f.keywords, "EXTDESC")
                .and_then(|k| k.args.first())
                .map(|a| a.trim_matches('\'').to_ascii_uppercase())
                .unwrap_or_else(|| f.name.clone());
            match (printers.get(&ext), f.external) {
                (Some((attrs, dds)), _) => {
                    info.page = Some((attrs.page_length, attrs.page_width, attrs.overflow));
                    if f.external {
                        let dds = dds.clone().ok_or_else(|| {
                            anyhow!("printer file {} has no record formats (it was created without DDS)", f.name)
                        })?;
                        // The record formats' fields become program fields:
                        // numbers zoned, as in a display file.
                        for r in &dds.records {
                            for fd in &r.fields {
                                if fd.content != crate::prtf::Content::Field || f.proc.is_some() {
                                    continue;
                                }
                                let t = match fd.decimals {
                                    Some(d) if fd.dtype == 'S' => {
                                        DType::Zoned(fd.length as u32, d as u32)
                                    }
                                    _ => DType::Char(fd.length as u32),
                                };
                                b.file_fields.entry(fd.name.clone()).or_insert(t);
                            }
                        }
                        info.printer = Some(dds);
                    }
                }
                (None, true) => bail!("printer file {} is not described (not found)", f.name),
                (None, false) => {}
            }
        }
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
            // Not found when compiled: the file opened (EXTFILE) gives the
            // layout then; the record length is the declared one.
            let found = externals.get(&f.name).cloned();
            info.program_described = true;
            match &found {
                None => {
                    info.record_len = f.record_len.unwrap_or(0) as usize;
                    info.format = f.name.clone();
                }
                Some(fr) => {
                    let mut at = 0usize;
                    for fd in &fr.file.fields {
                        let t = program_type(fd, true, false).unwrap_or(DType::Char(0));
                        info.layout.push((t, at));
                        at += t.size();
                    }
                    info.record_len = at;
                    info.format = fr.file.format.clone();
                }
            }
            // Its input specifications: the record's fields by position
            // (also when the file is not there at compile time).
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
            info.fref = found;
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
            inz_copies: vec![],
            user_inz: vec![],
            ptr_inz: vec![],
            local: true,
            param_likeds: HashMap::new(),
            statics: vec![],
            globals: b.bufs.clone(),
            extbinint: b.extbinint,
            datfmt: b.datfmt,
            timfmt: b.timfmt,
            occurs: vec![],
            based: HashMap::new(),
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
                written: pr
                    .defs
                    .iter()
                    .find(|d| d.kind == DefKind::Interface)
                    .and_then(|d| keyword(&d.keywords, "EXTPROC"))
                    .and_then(|k| match k.args.first() {
                        Some(a) if a.trim().starts_with('*') => k.args.get(1),
                        a => a,
                    })
                    .map(|a| a.trim())
                    .filter(|a| a.starts_with('\''))
                    .map(|a| a.trim_matches('\'').to_string()),
                syms,
                frame: pb.bufs.into_iter().map(super::mem::padded).collect(),
                params,
                returns,
                routines: code::build(body).map_err(|e| anyhow!("procedure {}: {e}", pr.name))?,
                on_exit,
                export: pr.export,
                based: pb.based,
                ptr_inz: pb
                    .ptr_inz
                    .iter()
                    .map(|(n, e)| Ok((n.clone(), expr::parse(e)?)))
                    .collect::<Result<Vec<_>>>()?,
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
    // A procedure whose prototype has EXTPROC('name') is that name to
    // other modules (ILE RPG Reference, EXTPROC: the external name of the
    // procedure): it is found by both.
    let aliases: Vec<(String, String)> = procs
        .keys()
        .filter_map(|n| {
            protos
                .get(n)
                .and_then(|p| p.extproc.clone())
                .map(|e| e.to_ascii_uppercase())
                .filter(|e| e != n && !procs.contains_key(e))
                .map(|e| (n.clone(), e))
        })
        .collect();
    for (n, e) in aliases {
        if let Some(pi) = procs.get(&n).cloned() {
            procs.insert(e, pi);
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
    if let Some(c) = src
        .calcs
        .iter()
        .find(|c| !declarative_sql(c))
        .filter(|_| nomain)
    {
        bail!(
            "line {}: ctl-opt NOMAIN: a module without a main procedure has no calculations outside procedures (RNF3788)",
            c.line
        );
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
        based: b.based.clone(),
        imports,
        nomain,
        name: name.to_ascii_uppercase(),
        syms: b.syms,
        globals: b.bufs.into_iter().map(super::mem::padded).collect(),
        files,
        routines,
        entry,
        plists: src.plists.clone(),
        outputs: src.outputs.clone(),
        control: src.control.clone(),
        sql_commit: sql_commit(src),
        dtaaras: data_areas(src),
        psds: src
            .defs
            .iter()
            .find(|d| d.kind == DefKind::Ds && keyword(&d.keywords, "PSDS").is_some())
            .map(ds_key),
        auto_dtaaras: auto_dtaaras(src),
        user_inz: b.user_inz.clone(),
        ptr_inz: b
            .ptr_inz
            .iter()
            .map(|(n, e)| Ok((n.clone(), expr::parse(e)?)))
            .collect::<Result<Vec<_>>>()?,
        protos,
        procs,
        main,
        cursors,
    })
}
