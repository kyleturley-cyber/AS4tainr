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
        if let Some(k) = keyword(&d.keywords, "DTAARA") {
            let name = k
                .args
                .iter()
                .map(|a| a.trim().trim_matches('\'').to_ascii_uppercase())
                .find(|a| !a.starts_with('*'))
                .unwrap_or_else(|| d.name.clone());
            out.insert(d.name.clone(), name);
        }
    }
    out
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
        .filter(|f| f.external && f.device == "DISK")
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

/// External field type as the program sees it outside a data structure:
/// zoned and packed fields are packed (ILE RPG Reference: numeric fields
/// of externally described files are packed in the program).
fn program_type(f: &crate::dds::FieldDef, in_ds: bool) -> DType {
    match f.data_type {
        DataType::Char => DType::Char(f.length),
        DataType::Packed => DType::Packed(f.length, f.decimals),
        DataType::Zoned => {
            if in_ds {
                DType::Zoned(f.length, f.decimals)
            } else {
                DType::Packed(f.length, f.decimals)
            }
        }
    }
}

struct Binder<'a> {
    syms: HashMap<String, Sym>,
    bufs: Vec<Vec<u8>>,
    externals: &'a HashMap<String, FileRef>,
    /// Types of external file fields, by name.
    file_fields: HashMap<String, DType>,
    /// Pending INZ values: (variable, value).
    inz: Vec<(String, Value)>,
    /// Binding a procedure's local storage.
    local: bool,
    /// Local buffers of STATIC variables (moved to global storage).
    statics: Vec<usize>,
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
        let mut i = 0;
        while i < defs.len() {
            let d = &defs[i];
            match d.kind {
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
                    i += 1;
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
                    i += 1;
                }
                DefKind::Ds => {
                    let next = self
                        .ds(defs, i)
                        .map_err(|e| anyhow!("line {}: {e}", d.line))?;
                    if self.local && keyword(&d.keywords, "STATIC").is_some() {
                        self.statics.push(self.bufs.len() - 1);
                    }
                    i = next;
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
                            },
                        );
                    }
                    i = j;
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
        match keyword(&d.keywords, "DIM").and_then(|k| k.args.first()) {
            Some(a) => num_arg(a, &self.syms),
            None => Ok(0),
        }
    }

    fn inz(&mut self, name: &str, d: &Def) -> Result<()> {
        if let Some(k) = keyword(&d.keywords, "INZ") {
            if let Some(a) = k.args.first().filter(|a| !a.is_empty()) {
                let v = literal_value(a, &self.syms)?;
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
            for fd in &fr.file.fields {
                let t = program_type(fd, true);
                subs.push((fd.name.clone(), t, next, 0, d));
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
        while j < defs.len() && defs[j].kind == DefKind::Subfield {
            let s = &defs[j];
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
        let total = size * dim.max(1) as usize;
        let buf = self.new_buf(total, data::BLANK);
        let mut names = Vec::new();
        for (sname, t, off, sdim, sdef) in &subs {
            let key = if qualified || likeds.is_some() {
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
                for e in 0..dim.max(1) as usize {
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
            let prefix = if qualified || likeds.is_some() {
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
                    Sym::Var(v) if v.buf == o.buf => add.push((
                        format!("{prefix}.{rest}"),
                        Sym::Var(VarInfo {
                            buf,
                            off: off + rel(v.off),
                            local: self.local,
                            ..v.clone()
                        }),
                    )),
                    Sym::Ds(d) if d.buf == o.buf => add.push((
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
            let src = self.bufs[o.buf][o.off..o.off + o.size].to_vec();
            for e in 0..dim.max(1) as usize {
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
        let ds_name = if name.is_empty() {
            format!("*DS{i}")
        } else {
            name.clone()
        };
        self.syms.insert(
            ds_name,
            Sym::Ds(DsInfo {
                buf,
                off: 0,
                size,
                dim,
                qualified: qualified || likeds.is_some(),
                subfields: names,
                local: self.local,
            }),
        );
        Ok(j)
    }
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
        local: false,
        statics: vec![],
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
                    b.file_fields
                        .entry(pname.clone())
                        .or_insert_with(|| display_type(fd));
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
            for fd in &fr.file.fields {
                let pname = match &prefix {
                    Some(p) => format!("{p}{}", fd.name),
                    None => fd.name.clone(),
                };
                b.file_fields
                    .entry(pname.clone())
                    .or_insert_with(|| program_type(fd, false));
                info.fields.push(pname);
            }
            info.fref = Some(fr);
        }
        files.push(info);
    }
    // Definitions, in order.
    let defs = &src.defs;
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
    // Compile-time arrays (CTDATA).
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
        if let Some(Sym::Var(v)) = b.syms.get(&d.name).cloned() {
            let per = keyword(&d.keywords, "PERRCD")
                .and_then(|k| k.args.first())
                .and_then(|a| a.trim().parse::<usize>().ok())
                .unwrap_or(1);
            let mut n = 0usize;
            for row in rows {
                let chars: Vec<char> = row.chars().collect();
                for k in 0..per {
                    if n >= v.dim as usize {
                        break;
                    }
                    let w = v.dtype.len() as usize;
                    let piece: String = chars.iter().skip(k * w).take(w).collect();
                    let val = if v.dtype.is_numeric() {
                        Value::Num(Dec::parse(&piece).unwrap_or_default())
                    } else {
                        Value::str(&piece)
                    };
                    let at = v.off + n * v.stride;
                    data::store(
                        &v.dtype,
                        &val,
                        &mut b.bufs[v.buf][at..at + v.dtype.size()],
                        data::Fit::Strict { half_adjust: false },
                        false,
                    )?;
                    n += 1;
                }
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
    Ok(Program {
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
        protos,
        procs,
        main,
    })
}
