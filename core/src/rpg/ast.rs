//! The program as parsed: one form for fixed-form and free-form source.

use super::data::{DType, Fig};
use super::dec::Dec;

/// An expression (ILE RPG reference, "Expressions").
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(Dec),
    /// A character literal, EBCDIC.
    Str(Vec<u8>),
    /// X'C1C2': a hexadecimal literal.
    Hex(Vec<u8>),
    Fig(Fig),
    /// A name, possibly qualified (`DS.FIELD`), with optional indexes or
    /// arguments: an array element, a procedure call, or a plain field.
    Name {
        parts: Vec<String>,
        args: Option<Vec<Expr>>,
    },
    /// A qualified name with an index before its last part:
    /// `list.users(i).name`. `idx[k]` is the index of `parts[k]`; `args`
    /// that of the last part.
    Path {
        parts: Vec<String>,
        idx: Vec<Option<Box<Expr>>>,
        args: Option<Vec<Expr>>,
    },
    /// *INxx, *INLR, *INKC...: one indicator.
    Ind(String),
    /// *IN(n), *IN: the indicator array.
    IndArray(Option<Box<Expr>>),
    /// %BIF(args...).
    Bif(String, Vec<Expr>),
    /// *OMIT in a parameter list.
    Omit,
    Unary(UnOp, Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

impl Expr {
    pub fn name(n: &str) -> Expr {
        Expr::Name {
            parts: vec![n.to_ascii_uppercase()],
            args: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    /// `x IN %RANGE(a:b)`, `x IN %LIST(...)`, `x IN array` (7.5).
    In,
}

/// Operation extenders: (H) half adjust, (N) no lock, (E) errors to
/// %ERROR, (R) result decimal positions, (M) maximum digits...
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ext {
    pub half_adjust: bool,
    pub no_lock: bool,
    pub error: bool,
    pub other: String,
}

impl Ext {
    pub fn parse(s: &str) -> Ext {
        let s = s.to_ascii_uppercase();
        Ext {
            half_adjust: s.contains('H'),
            no_lock: s.contains('N'),
            error: s.contains('E'),
            other: s,
        }
    }
}

/// Resulting indicators (positions 71-76 of a fixed-form calculation):
/// high, low, equal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultInd {
    pub hi: Option<String>,
    pub lo: Option<String>,
    pub eq: Option<String>,
}

impl ResultInd {
    pub fn is_empty(&self) -> bool {
        self.hi.is_none() && self.lo.is_none() && self.eq.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOp {
    Read,
    ReadE,
    ReadP,
    ReadPE,
    Chain,
    SetLL,
    SetGT,
    Write,
    Update,
    Delete,
    Unlock,
    Open,
    Close,
    Exfmt,
    Feod,
    /// READC: the next changed subfile record.
    ReadC,
}

/// Fixed-form arithmetic and data movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    ZAdd,
    ZSub,
    Add,
    Sub,
    Mult,
    Div,
    Mvr,
}

/// A statement that does not change the flow of control.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// EXEC SQL: the statement as written (host variables `:name`).
    Sql(String),
    Eval {
        target: Expr,
        value: Expr,
        ext: Ext,
        right: bool,
    },
    /// A procedure called for its effect (CALLP, or a free-form call).
    CallP(Expr),
    /// CALL with a PLIST or PARMs.
    Call {
        program: Expr,
        parms: Vec<Parm>,
        ind: ResultInd,
    },
    File {
        op: FileOp,
        /// File or record format name.
        file: String,
        /// Search argument: a value, a KLIST name, or a list (%KDS or
        /// free-form `(a: b)`).
        key: Option<Vec<Expr>>,
        /// Result data structure (READ file ds).
        ds: Option<String>,
        /// UPDATE rec %FIELDS(a : b): only these fields change.
        fields: Option<Vec<String>>,
        ext: Ext,
        ind: ResultInd,
    },
    Dsply {
        message: Option<Expr>,
        response: Option<Expr>,
    },
    SetInd {
        on: bool,
        inds: Vec<String>,
    },
    Arith {
        op: Arith,
        f1: Option<Expr>,
        f2: Expr,
        result: Expr,
        ext: Ext,
        ind: ResultInd,
    },
    Move {
        left: bool,
        /// P: pad the result.
        pad: bool,
        /// Factor 1: the date or time format of the character or numeric
        /// side when the other is a date, time or timestamp (*MDY, *USA/...).
        fmt: Option<String>,
        from: Expr,
        to: Expr,
        ind: ResultInd,
    },
    /// COMP: set resulting indicators from comparing factor 1 with 2.
    Comp {
        f1: Expr,
        f2: Expr,
        ind: ResultInd,
    },
    Clear(Expr),
    Reset(Expr),
    /// DEFINE, KLIST, PLIST and such: declarations kept in the calcs.
    Nop,
    /// An operation as4tainr does not run yet: fails when reached.
    Unsupported(String),
    /// CAT, SUBST, SCAN, CHECK, CHECKR, XLATE, LOOKUP, TIME, IN, OUT...:
    /// the fixed-form string and misc operations.
    Op {
        code: String,
        f1: Option<Expr>,
        f2: Option<Expr>,
        result: Option<Expr>,
        ext: Ext,
        ind: ResultInd,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parm {
    pub target: Expr,
    pub value: Option<Expr>,
}

/// Conditioning indicators of a fixed-form line: all must be satisfied
/// (`N` reverses one).
pub type Conditions = Vec<(bool, String)>;

/// One line of calculations before blocks are built: a statement or a
/// structured operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Stmt(Stmt),
    If(Expr),
    ElseIf(Expr),
    Else,
    EndIf,
    Dow(Expr),
    Dou(Expr),
    /// DO with a start, limit and index (fixed form), or none.
    Do {
        start: Option<Expr>,
        limit: Option<Expr>,
        index: Option<Expr>,
    },
    For {
        var: Expr,
        from: Expr,
        to: Option<Expr>,
        by: Option<Expr>,
        down: bool,
    },
    /// FOR-EACH item IN array or list (7.5): ENDFOR ends it.
    ForEach {
        var: Expr,
        list: Expr,
    },
    EndDo,
    EndFor,
    Select,
    When(Expr),
    Other,
    EndSl,
    Iter,
    Leave,
    LeaveSr,
    BegSr(String),
    /// ENDSR with its return point (factor 2): a field or literal
    /// holding *DETL, *GETIN, *TOTC, *TOTL, *OFL, *DETC or *CANCL.
    EndSr(Option<Expr>),
    ExSr(String),
    Return(Option<Expr>),
    Goto(String),
    Tag(String),
    /// CABxx: compare and branch to a tag.
    Cab {
        cmp: Option<BinOp>,
        f1: Expr,
        f2: Expr,
        tag: String,
        ind: ResultInd,
    },
    Monitor,
    OnError(Vec<String>),
    EndMon,
    /// A fixed-form `END` (ends whatever is open).
    End,
}

/// A calculation line with its source line number, conditioning
/// indicators and control level (L1-L9, LR: total calculations).
#[derive(Clone, Debug, PartialEq)]
pub struct CalcLine {
    pub line: usize,
    pub cond: Conditions,
    pub level: Option<String>,
    pub what: Line,
}

/// A file (F spec or DCL-F).
#[derive(Clone, Debug, PartialEq)]
pub struct FileDecl {
    pub name: String,
    /// I, O, U, C
    pub usage: char,
    /// P primary, S secondary, F full procedural.
    pub designation: char,
    /// A: records can be added (WRITE) to an input or update file.
    pub add: bool,
    pub external: bool,
    pub keyed: bool,
    /// Record length of a program-described file.
    pub record_len: Option<u32>,
    /// DISK, WORKSTN, PRINTER, SEQ, SPECIAL.
    pub device: String,
    pub keywords: Vec<Keyword>,
    pub line: usize,
    /// Declared inside this procedure: a local file (ILE RPG Reference,
    /// "File Considerations": opened when the procedure is called and
    /// closed when it returns, unless STATIC).
    pub proc: Option<String>,
}

/// A keyword with its arguments as written: `DIM(10)`, `INZ('X')`.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyword {
    pub name: String,
    pub args: Vec<String>,
}

pub fn keyword<'a>(kws: &'a [Keyword], name: &str) -> Option<&'a Keyword> {
    kws.iter().find(|k| k.name == name)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DefKind {
    /// A standalone field (S).
    Field,
    /// A named constant (C).
    Const,
    /// A data structure (DS).
    Ds,
    /// A subfield of the data structure before it.
    Subfield,
    /// A prototype (PR) and its parameters.
    Proto,
    /// A procedure interface (PI) and its parameters.
    Interface,
    /// A parameter of the PR or PI before it.
    Param,
}

/// A definition (D spec or DCL-*).
#[derive(Clone, Debug, PartialEq)]
pub struct Def {
    pub name: String,
    pub kind: DefKind,
    /// Type, when given explicitly.
    pub dtype: Option<DType>,
    /// From and to positions in a data structure (fixed form), or POS.
    pub from: Option<u32>,
    pub to: Option<u32>,
    pub external: bool,
    pub keywords: Vec<Keyword>,
    pub line: usize,
}

/// A procedure (P spec B ... E, or DCL-PROC ... END-PROC).
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub name: String,
    /// The name as written (its case kept), for EXTPROC(*DCLCASE).
    pub written: String,
    pub export: bool,
    pub defs: Vec<Def>,
    pub calcs: Vec<CalcLine>,
    pub line: usize,
}

/// Output specifications (O specs): a record, its conditions and fields.
#[derive(Clone, Debug, PartialEq)]
pub struct OutRec {
    pub file: String,
    /// H heading, D detail, T total, E exception.
    pub kind: char,
    pub fetch_overflow: bool,
    /// ADD (a new record) or DEL (delete the record) in positions 18-20,
    /// for database files.
    pub add: bool,
    pub del: bool,
    /// Conditioning indicators: groups joined by OR, the indicators in a
    /// group by AND (record lines with AND/OR in positions 16-18).
    pub cond: Vec<Conditions>,
    /// EXCEPT name.
    pub name: Option<String>,
    /// Space before and after (positions 40-42, 43-45); None when blank.
    pub space_before: Option<u32>,
    pub space_after: Option<u32>,
    pub skip_before: Option<u32>,
    pub skip_after: Option<u32>,
    pub fields: Vec<OutField>,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutField {
    pub cond: Conditions,
    /// Field name, or a literal ('text'), or PAGE/UDATE/*DATE...
    pub name: Option<String>,
    pub literal: Option<Vec<u8>>,
    pub edit_code: Option<char>,
    pub blank_after: bool,
    /// End position in the record.
    pub end: Option<u32>,
    /// '+n': n blanks after the previous field.
    pub relative: Option<u32>,
    pub edit_word: Option<String>,
    /// Data format in the record (position 52): blank, P, B, L, R.
    pub format: char,
}

/// A whole source member, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source {
    pub control: Vec<Keyword>,
    pub files: Vec<FileDecl>,
    pub defs: Vec<Def>,
    pub calcs: Vec<CalcLine>,
    pub procs: Vec<Proc>,
    pub outputs: Vec<OutRec>,
    /// Compile-time data (**CTDATA name, or ** in order of arrays).
    pub ctdata: Vec<(Option<String>, Vec<String>)>,
    /// Program-described input (I specs): file, fields (name, from, to,
    /// decimals) per record.
    pub inputs: Vec<InRec>,
    /// *ENTRY PLIST: the program's parameters.
    pub entry: Option<Vec<Parm>>,
    /// Named parameter lists (PLIST) for CALL.
    pub plists: Vec<(String, Vec<Parm>)>,
    /// Key lists (KLIST/KFLD).
    pub klists: Vec<(String, Vec<Expr>)>,
}

/// Input specifications for a record (ILE RPG Reference, "Input
/// Specifications"): of a program-described file, a record type and its
/// fields by position; of an externally described file, a record format
/// with renamed fields, control levels, matching fields and field
/// indicators.
#[derive(Clone, Debug, PartialEq)]
pub struct InRec {
    /// File name (program-described) or record format name.
    pub file: String,
    /// Sequence (positions 17-18), number (19), option (20).
    pub sequence: String,
    pub number: char,
    pub option: char,
    pub record_ind: Option<String>,
    /// Record identification codes: groups joined by OR, codes in a
    /// group by AND.
    pub codes: Vec<Vec<RecCode>>,
    pub fields: Vec<InField>,
    pub line: usize,
}

/// One record identification code: the character at a position, all of
/// it (C), its zone (Z) or its digit portion (D), or NOT that.
#[derive(Clone, Debug, PartialEq)]
pub struct RecCode {
    pub position: u32,
    pub not: bool,
    pub part: char,
    pub ch: char,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InField {
    pub name: String,
    /// The external field (positions 21-30) a field line renames.
    pub external: Option<String>,
    pub from: u32,
    pub to: u32,
    pub decimals: Option<u32>,
    /// Data format (position 36): blank, P, B, I, U, L, R, A, S.
    pub format: char,
    /// L1-L9 (63-64), M1-M9 (65-66), field record relation (67-68).
    pub level: Option<String>,
    pub matching: Option<String>,
    pub relation: Option<String>,
    /// Field indicators: plus (69-70), minus (71-72), zero or blank
    /// (73-74).
    pub plus: Option<String>,
    pub minus: Option<String>,
    pub zero: Option<String>,
}
