//! The compiler's checks that CRTBNDRPG reports in its listing before it
//! creates a program (ILE RPG Reference; observed on IBM i 7.5 with
//! tools/golden/listings.txt):
//!
//! - RNF7030 (30): a name used in the calculations is not defined;
//! - RNF7503 (30): an expression has such an operand;
//! - RNF7416 (30): EVAL assigns a numeric value to a character target, or
//!   a character value to a numeric one;
//! - RNF7086 (00): RPG blocks the records of a file (the conditions of
//!   the BLOCK keyword's description).
//!
//! The texts are as4tainr's own.

use super::ast::{keyword, BinOp, CalcLine, Expr, FileOp, Line, Source, Stmt};
use super::data::DType;
use super::program::{Program, Sym};

/// A message of the compiler about source line `line` (0: none).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diag {
    pub line: usize,
    pub id: String,
    pub sev: u8,
    pub text: String,
}

/// The checks' messages, and the names that are not defined (with the
/// lines that use them), for the cross reference.
#[derive(Debug, Default)]
pub struct Checked {
    pub diags: Vec<Diag>,
    pub undefined: Vec<(String, usize)>,
}

/// Built-in functions whose arguments name files, procedures or fields
/// rather than being values.
const NAME_BIFS: &[&str] = &[
    "%OPEN", "%EOF", "%FOUND", "%EQUAL", "%STATUS", "%ERROR", "%FIELDS", "%PADDR", "%PARMNUM",
    "%TARGET", "%HANDLER", "%XML", "%DATA", "%GEN", "%NULLIND", "%KDS", "%PROC", "%SIZE", "%ELEM",
    "%ADDR", "%LEN", "%DECPOS", "%PASSED", "%OMITTED",
];

/// Special names the calculations may use without defining them.
fn special(name: &str) -> bool {
    name.starts_with('*')
        || matches!(
            name,
            "UDATE" | "UDAY" | "UMONTH" | "UYEAR" | "PAGE" | "PAGE1" | "PAGE2" | "PAGE3"
        )
        || name.starts_with("PAGE")
}

struct Walker<'a> {
    prog: &'a Program,
    proc: Option<&'a super::program::ProcInfo>,
    out: Checked,
}

impl Walker<'_> {
    fn sym(&self, name: &str) -> Option<&Sym> {
        self.proc
            .and_then(|p| p.syms.get(name))
            .or_else(|| self.prog.syms.get(name))
    }

    fn defined(&self, name: &str) -> bool {
        special(name)
            || self.sym(name).is_some()
            || self.prog.procs.contains_key(name)
            || self.prog.protos.contains_key(name)
            || self.prog.imports.iter().any(|i| i == name)
            || self
                .prog
                .files
                .iter()
                .any(|f| f.decl.name == name || f.format == name)
    }

    /// Note the names of an expression that are not defined; true when
    /// there is one.
    fn expr(&mut self, line: usize, e: &Expr) -> bool {
        let mut bad = false;
        match e {
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                if !self.defined(&name) && !self.defined(&parts[0]) {
                    self.undefined(line, &name);
                    bad = true;
                }
                for a in args.iter().flatten() {
                    bad |= self.expr(line, a);
                }
            }
            Expr::Path { parts, idx, args } => {
                for i in idx.iter().flatten() {
                    bad |= self.expr(line, i);
                }
                for a in args.iter().flatten() {
                    bad |= self.expr(line, a);
                }
                if !self.defined(&parts[0]) && !self.defined(&parts.join(".")) {
                    self.undefined(line, &parts.join("."));
                    bad = true;
                }
            }
            Expr::IndArray(Some(x)) => bad |= self.expr(line, x),
            Expr::Bif(b, args) if !NAME_BIFS.contains(&b.to_ascii_uppercase().as_str()) => {
                for a in args {
                    bad |= self.expr(line, a);
                }
            }
            Expr::Unary(_, x) => bad |= self.expr(line, x),
            Expr::Bin(_, a, b) => {
                bad |= self.expr(line, a);
                bad |= self.expr(line, b);
            }
            _ => {}
        }
        bad
    }

    fn undefined(&mut self, line: usize, name: &str) {
        if !self
            .out
            .undefined
            .iter()
            .any(|(n, l)| n == name && *l == line)
        {
            self.out.undefined.push((name.to_string(), line));
            self.out.diags.push(Diag {
                line,
                id: "RNF7030".into(),
                sev: 30,
                text: format!("Name or indicator {name} is not defined."),
            });
        }
    }

    /// A value: RNF7503 when it uses a name that is not defined.
    fn value(&mut self, line: usize, e: &Expr) {
        if self.expr(line, e) {
            self.out.diags.push(Diag {
                line,
                id: "RNF7503".into(),
                sev: 30,
                text: "The expression has an operand that is not defined.".into(),
            });
        }
    }

    fn opt(&mut self, line: usize, e: &Option<Expr>) {
        if let Some(e) = e {
            self.value(line, e);
        }
    }

    fn calc(&mut self, c: &CalcLine) {
        let l = c.line;
        match &c.what {
            Line::Stmt(s) => self.stmt(l, s),
            Line::If(e) | Line::ElseIf(e) | Line::Dow(e) | Line::Dou(e) | Line::When(e) => {
                self.value(l, e)
            }
            Line::For {
                var, from, to, by, ..
            } => {
                self.expr(l, var);
                self.value(l, from);
                self.opt(l, to);
                self.opt(l, by);
            }
            Line::ForEach { var, list } => {
                self.expr(l, var);
                self.value(l, list);
            }
            Line::Do {
                start,
                limit,
                index,
            } => {
                self.opt(l, start);
                self.opt(l, limit);
                if let Some(i) = index {
                    self.expr(l, i);
                }
            }
            Line::Return(e) => self.opt(l, e),
            Line::Cab { f1, f2, .. } => {
                self.value(l, f1);
                self.value(l, f2);
            }
            _ => {}
        }
    }

    fn stmt(&mut self, l: usize, s: &Stmt) {
        match s {
            Stmt::Eval { target, value, .. } => {
                self.expr(l, target);
                self.value(l, value);
                if let (Some(t), Some(v)) = (self.class_of(target), self.class_of(value)) {
                    if t != v {
                        self.out.diags.push(Diag {
                            line: l,
                            id: "RNF7416".into(),
                            sev: 30,
                            text: "The types of the target and the value of the EVAL operation do not match.".into(),
                        });
                    }
                }
            }
            Stmt::CallP(e) => {
                self.value(l, e);
            }
            Stmt::Call { parms, .. } => {
                for p in parms {
                    self.expr(l, &p.target);
                    self.opt(l, &p.value);
                }
            }
            Stmt::File { key, .. } => {
                for k in key.iter().flatten() {
                    self.value(l, k);
                }
            }
            Stmt::Dsply { message, response } => {
                self.opt(l, message);
                if let Some(r) = response {
                    self.expr(l, r);
                }
            }
            Stmt::Comp { f1, f2, .. } => {
                self.value(l, f1);
                self.value(l, f2);
            }
            _ => {}
        }
    }

    /// Whether a value is numeric (Some(true)) or character (Some(false)),
    /// when that is certain.
    fn class_of(&self, e: &Expr) -> Option<bool> {
        match e {
            Expr::Num(_) => Some(true),
            Expr::Str(_) => Some(false),
            Expr::Name { parts, args } => {
                let name = parts.join(".");
                match self.sym(&name)? {
                    Sym::Var(v) if args.is_none() || v.dim > 0 => match v.dtype {
                        DType::Char(_) | DType::Varchar(_) => Some(false),
                        DType::Zoned(..)
                        | DType::Packed(..)
                        | DType::Binary(..)
                        | DType::Int(_)
                        | DType::Uns(_)
                        | DType::Float(_) => Some(true),
                        _ => None,
                    },
                    _ => None,
                }
            }
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.class_of(a)?, self.class_of(b)?);
                match op {
                    BinOp::Add if x == y => Some(x),
                    BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow if x && y => Some(true),
                    _ => None,
                }
            }
            Expr::Bif(b, _) => match b.to_ascii_uppercase().as_str() {
                "%CHAR" | "%TRIM" | "%TRIML" | "%TRIMR" | "%SUBST" | "%EDITC" | "%EDITW"
                | "%XLATE" | "%REPLACE" | "%UPPER" | "%LOWER" | "%STR" => Some(false),
                "%DEC" | "%DECH" | "%INT" | "%INTH" | "%UNS" | "%UNSH" | "%SCAN" | "%CHECK"
                | "%CHECKR" | "%LEN" | "%ELEM" | "%SIZE" | "%ABS" | "%REM" | "%DIV" => Some(true),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Run the checks on a program's calculations.
pub fn check(src: &Source, prog: &Program) -> Checked {
    let mut w = Walker {
        prog,
        proc: None,
        out: Checked::default(),
    };
    for c in &src.calcs {
        w.calc(c);
    }
    for p in &src.procs {
        w.proc = prog.procs.get(&p.name.to_ascii_uppercase());
        for c in &p.calcs {
            w.calc(c);
        }
    }
    let mut out = w.out;
    out.diags.extend(blocking(src));
    out.diags.sort_by_key(|d| d.line);
    out
}

/// RNF7086 for each file whose records RPG blocks (ILE RPG Reference,
/// BLOCK keyword): a DISK or SEQ file that is output-only, or input-only
/// and read only with READ, without RECNO, COMMIT, SFILE or BLOCK(*NO).
fn blocking(src: &Source) -> Vec<Diag> {
    let mut ops: Vec<(String, FileOp)> = Vec::new();
    let mut note = |calcs: &[CalcLine]| {
        for c in calcs {
            if let Line::Stmt(Stmt::File { op, file, .. }) = &c.what {
                ops.push((file.to_ascii_uppercase(), *op));
            }
        }
    };
    note(&src.calcs);
    for p in &src.procs {
        note(&p.calcs);
    }
    let mut out = Vec::new();
    for f in &src.files {
        if !matches!(f.device.as_str(), "DISK" | "SEQ") {
            continue;
        }
        let kw = |k: &str| keyword(&f.keywords, k).is_some();
        if kw("RECNO") || kw("COMMIT") || kw("SFILE") {
            continue;
        }
        if keyword(&f.keywords, "BLOCK")
            .and_then(|k| k.args.first())
            .is_some_and(|a| a.trim().eq_ignore_ascii_case("*NO"))
        {
            continue;
        }
        let name = f.name.to_ascii_uppercase();
        let formats: Vec<String> = Vec::new();
        let mine = |n: &str| n == name || formats.iter().any(|x| x == n);
        let blocked = match f.usage {
            'O' => true,
            'I' if !f.add => !ops.iter().any(|(n, op)| {
                mine(n)
                    && !matches!(
                        op,
                        FileOp::Read | FileOp::Open | FileOp::Close | FileOp::Feod
                    )
            }),
            _ => false,
        };
        if blocked {
            out.push(Diag {
                line: f.line,
                id: "RNF7086".into(),
                sev: 0,
                text: format!(
                    "RPG blocks the records of file {name}; its INFDS changes only when a block is read or written."
                ),
            });
        }
    }
    out
}
