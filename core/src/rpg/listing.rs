//! The ILE RPG compiler listing (CRTBNDRPG OUTPUT(*PRINT), the default):
//! the prologue (the command's values), the source with the structured
//! operations' nesting levels, the compiler options and file boxes and
//! the output specifications generated for externally described files,
//! the diagnostic messages, the output buffer positions, the cross
//! reference, the external references, the message summary and the final
//! summary.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-22, CRTBNDRPG listings
//! printed by `system`, tools/golden/listings.txt). Texts that are not
//! layout (messages, the compiler's name) are as4tainr's own.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::ast::{keyword, CalcLine, Expr, Line, Source, Stmt};
use super::check::Diag;
use super::data::DType;
use super::parse::COPY_LINES;
use super::program::{Program, Sym};
use crate::report::{at, Stamp};

/// The create command's values the listing shows.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub command: String,
    pub user: String,
    pub program: String,
    pub library: String,
    /// TEXT as given (*SRCMBRTXT, *BLANK or a text).
    pub text: String,
    pub dftactgrp: String,
    pub actgrp: String,
    pub replace: String,
    pub dbgview: String,
    pub tgtrls: String,
    pub alwnull: String,
    pub bnddir: String,
    pub genlvl: u8,
    /// CRTRPGMOD: a module.
    pub module: bool,
    /// Sort sequence and language identifier ("": *HEX and *JOBRUN).
    pub srtseq: String,
    pub langid: String,
}

/// Lines on a listing page, the page header included (observed: 52).
pub const PAGE: usize = 52;

const END_SOURCE: &str = "       * * * * *   E N D   O F   S O U R C E   * * * * *";
const END_DIAG: &str = " * * * * *   E N D   O F   A D D I T I O N A L   D I A G N O S T I C   M E S S A G E S   * * * * *";
const END_BUFFER: &str =
    " * * * * *   E N D   O F   O U T P U T   B U F F E R   P O S I T I O N   *  * * * *";
const END_XREF: &str = "       * * * * *   E N D   O F   C R O S S   R E F E R E N C E   * * * * *";
const END_EXT: &str =
    "   * * * * *   E N D   O F   E X T E R N A L   R E F E R E N C E S   * * * * *";
const END_MSGS: &str = "       * * * * *   E N D   O F   M E S S A G E   S U M M A R Y   * * * * *";
const END_FINAL: &str = "         * * * * *   E N D   O F   F I N A L   S U M M A R Y   * * * * *";
const END_COMPILE: &str = "          * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *";

/// A row of the source section.
enum Row {
    /// A main source line: its number in the source.
    Src(usize),
    /// A line of /COPY member `c` (an index into the copies): its line,
    /// and which copy directive this is (1 for the first).
    Copy(usize, usize, usize),
    /// A generated specification (its text after "=") with its source ID
    /// and what it is.
    Gen(String, usize, Gen),
    /// A box line, with the source ID it belongs to (if any).
    Box(String, Option<usize>),
    /// The two ruler lines (fixed form: with the comments columns).
    Ruler(bool),
}

/// A group of generated specifications: record format, external format
/// and its field lines.
type GenGroup<F> = (String, String, Vec<F>);

/// What a generated specification line is.
#[derive(Clone)]
enum Gen {
    /// A record format.
    Record(String),
    /// An input field; true: of an update file (its references are M).
    Input(String, bool),
    /// An output field, with its positions in the buffer.
    Output(String, u32, u32),
}

/// A /COPY or /INCLUDE member the source brings in.
#[derive(Debug, Clone)]
pub struct Copy {
    /// The id as the directive writes it.
    pub id: String,
    pub member: crate::source::Member,
}

/// A field or other name in the cross reference.
#[derive(Debug, Clone)]
struct XName {
    name: String,
    attrs: Vec<String>,
    /// Listing line where it is defined.
    def: Option<usize>,
    children: Vec<XName>,
    undefined: bool,
}

pub struct Input<'a> {
    pub opts: &'a Opts,
    /// The /COPY members, by the id their directives write.
    pub copies: &'a [Copy],
    pub member: &'a crate::source::Member,
    pub src: Option<&'a Source>,
    pub prog: Option<&'a Program>,
    pub diags: &'a [Diag],
    pub undefined: &'a [(String, usize)],
    pub created: bool,
}

fn changed_stamp(us: Option<i64>) -> Option<Stamp> {
    let ((y, m, d), (h, mi, s), _) = crate::rpg::dates::from_epoch_us(us?);
    Some(Stamp::of((y, m, d), (h, mi, s)))
}

/// The attributes the cross reference shows for a type.
fn attr(t: &DType) -> String {
    match t {
        DType::Char(n) => format!("A({n})"),
        DType::Varchar(n) => format!("A({n}) VARYING"),
        DType::Zoned(p, s) => format!("S({p},{s})"),
        DType::Packed(p, s) => format!("P({p},{s})"),
        DType::Binary(p, s) => format!("B({p},{s})"),
        DType::Int(n) => format!("I({n},0)"),
        DType::Uns(n) => format!("U({n},0)"),
        DType::Ind => "N(1)".into(),
        DType::Date(_) => "D(10*ISO-)".into(),
        DType::Time(_) => "T(8*ISO.)".into(),
        DType::Timestamp(n) => format!("Z({})", super::data::ts_len(*n)),
        DType::Pointer => "*(16)".into(),
        DType::Float(n) => format!("F({n})"),
        DType::Ucs2(n) => format!("C({n})"),
        DType::VarUcs2(n) => format!("C({n}) VARYING"),
    }
}

/// A generated output field's data type, external attribute and length.
fn out_type(t: &DType) -> (char, &'static str, String) {
    match t {
        DType::Char(n) | DType::Varchar(n) => ('A', "CHAR", n.to_string()),
        DType::Zoned(p, s) => ('S', "ZONE", format!("{p},{s}")),
        DType::Packed(p, s) => ('P', "PACK", format!("{p},{s}")),
        DType::Binary(p, s) => ('B', "BINR", format!("{p},{s}")),
        DType::Int(n) => ('I', "INT", format!("{n},0")),
        DType::Uns(n) => ('U', "UNS", format!("{n},0")),
        DType::Float(n) => ('F', "FLTB", n.to_string()),
        DType::Date(_) => ('D', "DATE", "10".into()),
        DType::Time(_) => ('T', "TIME", "8".into()),
        DType::Timestamp(n) => ('Z', "TIMESTAMP", super::data::ts_len(*n).to_string()),
        DType::Ucs2(n) | DType::VarUcs2(n) => ('C', "UCS2", n.to_string()),
        DType::Ind => ('N', "IND", "1".into()),
        DType::Pointer => ('*', "PTR", "16".into()),
    }
}

/// The structured operations' nesting: for each calculation line, its
/// "Do Num" column (B01 begins level 1, E01 ends it, X01 is ELSE, WHEN...).
fn do_numbers(calcs: &[CalcLine], out: &mut HashMap<usize, String>) {
    let mut level = 0u32;
    for c in calcs {
        let s = match &c.what {
            Line::If(_)
            | Line::Dow(_)
            | Line::Dou(_)
            | Line::Do { .. }
            | Line::For { .. }
            | Line::ForEach { .. }
            | Line::Select
            | Line::Monitor => {
                level += 1;
                format!("B{level:02}")
            }
            Line::Else | Line::ElseIf(_) | Line::When(_) | Line::Other | Line::OnError(_) => {
                format!("X{level:02}")
            }
            Line::EndIf | Line::EndDo | Line::EndFor | Line::EndSl | Line::EndMon | Line::End => {
                let s = format!("E{:02}", level.max(1));
                level = level.saturating_sub(1);
                s
            }
            _ if level > 0 => format!(" {level:02}"),
            _ => continue,
        };
        out.insert(c.line, s);
    }
}

/// Identifiers in a source line (outside literals and comments),
/// upper-cased, with their offset.
fn names_in(line: &str, free: bool) -> Vec<String> {
    let cs: Vec<char> = line.chars().collect();
    // Fixed form: a comment line, and the sequence number area.
    let (start, end) = if free {
        (0, cs.len())
    } else {
        if cs.get(6) == Some(&'*') || cs.get(5).is_some_and(|c| *c == '*') {
            return vec![];
        }
        (6.min(cs.len()), cs.len().min(80))
    };
    let mut out = Vec::new();
    let mut i = start;
    let mut in_q = false;
    while i < end {
        let c = cs[i];
        if c == '\'' {
            in_q = !in_q;
            i += 1;
            continue;
        }
        if in_q {
            i += 1;
            continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'/') {
            break;
        }
        let ident = |c: char| c.is_ascii_alphanumeric() || "_@#$".contains(c);
        if ident(c) || c == '*' && cs.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            let mut j = i + 1;
            while j < end && (ident(cs[j]) || cs[j] == '.') {
                j += 1;
            }
            let w: String = cs[i..j].iter().collect();
            let w = w.trim_end_matches('.').to_ascii_uppercase();
            if !w.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.push(w);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The names an assignment changes on a calculation line (for "M").
fn modified(c: &CalcLine) -> Vec<String> {
    let first = |e: &Expr| -> Option<String> {
        match e {
            Expr::Name { parts, .. } | Expr::Path { parts, .. } => Some(parts[0].clone()),
            Expr::Ind(i) => Some(format!("*IN{i}")),
            _ => None,
        }
    };
    match &c.what {
        Line::Stmt(Stmt::Eval { target, .. }) => first(target).into_iter().collect(),
        Line::Stmt(Stmt::Arith { result, .. }) => first(result).into_iter().collect(),
        Line::Stmt(Stmt::Move { to, .. }) => first(to).into_iter().collect(),
        Line::Stmt(Stmt::SetInd { inds, .. }) => inds.iter().map(|i| format!("*IN{i}")).collect(),
        _ => vec![],
    }
}

/// The two ruler lines of free-form source.
const RULER_FREE: [&str; 2] = [
    "Line   <---------------------- Source Specifications ----------------------------------------------------->  Do  Change Src Seq",
    "Number ....+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8....+....9....+...10  Num Date   Id  Number",
];
/// The two ruler lines of fixed-form source (with the comments columns).
const RULER_FIXED: [&str; 2] = [
    "Line   <---------------------- Source Specifications ----------------------------><---- Comments ----> Do  Page  Change Src Seq",
    "Number ....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8....+....9....+...10 Num Line  Date   Id  Number",
];

/// A source line of a fixed-form calculation with resulting indicators
/// (positions 71-76): the blank ones shown as "--" (observed).
fn fixed_text(line: &str) -> String {
    let mut cs: Vec<char> = line.chars().collect();
    let is_calc =
        cs.get(5).is_some_and(|c| c.eq_ignore_ascii_case(&'C')) && cs.get(6) != Some(&'*');
    if is_calc && cs.len() > 70 {
        let ind: String = cs[70..cs.len().min(76)].iter().collect();
        if !ind.trim().is_empty() {
            while cs.len() < 76 {
                cs.push(' ');
            }
            for k in [70, 72, 74] {
                if cs[k] == ' ' && cs[k + 1] == ' ' {
                    cs[k] = '-';
                    cs[k + 1] = '-';
                }
            }
        }
    }
    cs.iter().skip(5).take(95).collect()
}

/// Is a source line a comment (or blank) for the final summary?
fn is_comment(line: &str, free: bool) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with("//") {
        return true;
    }
    !free && line.chars().nth(6) == Some('*')
}

/// The id of a /COPY or /INCLUDE directive on a line, if it is one.
fn copy_directive(line: &str, free: bool) -> Option<String> {
    let text = if free {
        format!("**FREE\n{line}")
    } else {
        line.to_string()
    };
    super::parse::includes(&text).into_iter().next()
}

/// The listing.
pub fn listing(input: &Input, stamp: &Stamp) -> Vec<String> {
    let o = input.opts;
    let m = input.member;
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (25, "RN"),
            (35, "ILE RPG"),
            (59, &format!("{}/{}", o.library, o.program)),
            (84, &crate::report::system_name()),
            (95, &stamp.date),
            (104, &stamp.time),
            (120, &format!("Page {page:>6}")),
        ])
    };
    let src_lines: Vec<&str> = m.source.lines().collect();
    let free = super::parse::is_fully_free(&m.source);
    let member = m.stmf.is_empty();

    // ---------------------------------------------------------- prologue
    let label = |indent: usize, text: &str, value: &str| -> String {
        let l = crate::commands::dsp_label(text, 34 - indent);
        at(&[(indent, &l), (40 + indent - 3, value)])
    };
    let changed = changed_stamp(m.changed_us)
        .map(|s| format!("{}  {}", s.date, s.time))
        .unwrap_or_default();
    let options = [
        "*XREF",
        "*GEN",
        "*NOSECLVL",
        "*SHOWCPY",
        "*EXPDDS",
        "*EXT",
        "*NOSHOWSKP",
        "*NOSRCSTMT",
        "*DEBUGIO",
        "*UNREF",
        "*NOEVENTF",
    ];
    let what = if o.module { "Module" } else { "Program" };
    let mut pro = vec![
        label(3, "Command", &o.command),
        label(5, "Issued by", &o.user),
        label(3, what, &o.program),
        label(5, "Library", &o.library),
        label(3, "Text 'description'", &o.text),
    ];
    if member {
        pro.push(label(3, "Source Member", &m.name));
        pro.push(label(3, "Source File", &m.file));
        pro.push(label(5, "Library", &m.library));
        pro.push(label(5, "CCSID", &m.ccsid.to_string()));
    } else {
        pro.push(label(3, "Source stream file  ", &m.stmf));
        pro.push(label(5, "CCSID", &m.ccsid.to_string()));
        // The EBCDIC CCSID the source is converted to (observed: 500 for
        // a stream file in CCSID 819; none for a member named by its IFS
        // path, as the SQL precompiler's output).
        let target = match m.ccsid {
            819 | 850 | 437 | 1252 => 500,
            1208 | 1200 | 13488 => 37,
            c => c,
        };
        if !m.stmf.to_ascii_uppercase().starts_with("/QSYS.LIB/") {
            pro.push(label(3, "Target CCSID", &format!("*SRC ({target})")));
        }
    }
    pro.push(label(3, "Text 'description'", &m.text));
    pro.push(label(3, "Last Change", &changed));
    pro.push(label(3, "Generation severity level", &o.genlvl.to_string()));
    if !o.module {
        pro.push(label(3, "Default activation group", &o.dftactgrp));
    }
    for (k, chunk) in options.chunks(4).enumerate() {
        let v: String = chunk
            .iter()
            .map(|x| format!("{x:<11}"))
            .collect::<String>()
            .trim_end()
            .to_string();
        if k == 0 {
            pro.push(label(3, "Compiler options", &v));
        } else {
            pro.push(at(&[(40, &v)]));
        }
    }
    let replace = if o.module {
        "Replace module"
    } else {
        "Replace program"
    };
    let mut rest: Vec<(&str, &str)> = vec![
        ("Debugging views", o.dbgview.as_str()),
        ("Debug encryption key", "*NONE"),
        ("Output", "*PRINT"),
        ("Optimization level", "*NONE"),
        ("Source listing indentation", "*NONE"),
        ("Type conversion options", "*NONE"),
        (
            "Sort sequence",
            if o.srtseq.is_empty() {
                "*HEX"
            } else {
                &o.srtseq
            },
        ),
        (
            "Language identifier",
            if o.langid.is_empty() {
                "*JOBRUN"
            } else {
                &o.langid
            },
        ),
        (replace, o.replace.as_str()),
    ];
    if !o.module {
        rest.push(("User profile", "*USER"));
    }
    rest.extend([
        ("Authority", "*LIBCRTAUT"),
        ("Truncate numeric", "*YES"),
        ("Fix numeric", "*NONE"),
        ("Target release", o.tgtrls.as_str()),
        ("Allow null values", o.alwnull.as_str()),
    ]);
    for (t, v) in rest {
        pro.push(label(3, t, v));
    }
    if o.module {
        // IBM i 7.5 prints this label with two blanks before its dots.
        pro.push("  Storage model . . .  . . . . . . :   *INHERIT".into());
        let dir = if o.bnddir.is_empty() {
            "*NONE"
        } else {
            &o.bnddir
        };
        pro.push(label(3, "Binding directory", dir));
    }
    for (t, v) in [
        ("Define condition names", "*NONE"),
        ("Enable performance collection", "*PEP"),
        ("Profiling data", "*NOCOL"),
        ("Licensed Internal Code options", ""),
        ("Generate program interface", "*NO"),
        ("Include directory", ""),
        ("Preprocessor options", "*NONE"),
        ("Require prototype for export", "*NO"),
    ] {
        pro.push(label(3, t, v));
    }

    // ------------------------------------------------------------ source
    // The n-th /COPY directive of the main source (its line), for the
    // parsed line numbers of copied lines (n * COPY_LINES + line).
    let directives: Vec<usize> = src_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| copy_directive(l, free).is_some())
        .map(|(i, _)| i + 1)
        .collect();
    // Where a parsed line comes in the listing: (main line, copy line).
    let order_key = |l: usize| -> (usize, usize) {
        if l >= COPY_LINES {
            let k = l / COPY_LINES;
            (
                directives.get(k - 1).copied().unwrap_or(usize::MAX),
                l % COPY_LINES,
            )
        } else {
            (l, 0)
        }
    };
    let mut dos: HashMap<usize, String> = HashMap::new();
    let mut after: HashMap<usize, Vec<Row>> = HashMap::new();
    let mut before: HashMap<usize, Vec<Row>> = HashMap::new();
    // Generated specifications: input (before the first calculation) and
    // output (after the main source), each group with its fields.
    let mut gen_in: Vec<GenGroup<(String, String)>> = Vec::new();
    let mut gen_out: Vec<GenGroup<(String, String, u32, u32)>> = Vec::new();
    let mut in_update: HashMap<String, bool> = HashMap::new();
    let mut first_calc: Option<usize> = None;
    if let Some(src) = input.src {
        do_numbers(&src.calcs, &mut dos);
        for p in &src.procs {
            do_numbers(&p.calcs, &mut dos);
        }
        first_calc = src
            .calcs
            .iter()
            .map(|c| c.line)
            .filter(|l| *l > 0 && *l < COPY_LINES)
            .min();
        // The compiler options in effect, before the first specification
        // after the control specifications (when the source has some).
        let has_ctl = src_lines.iter().any(|l| {
            let t = l.trim_start().to_ascii_lowercase();
            t.starts_with("ctl-opt")
                || (!free
                    && l.len() > 6
                    && l.as_bytes()[5].eq_ignore_ascii_case(&b'H')
                    && l.as_bytes().get(6) != Some(&b'*'))
        });
        let first_spec = src
            .files
            .iter()
            .map(|f| f.line)
            .chain(src.defs.iter().map(|d| d.line))
            .chain(src.calcs.iter().map(|c| c.line))
            .chain(src.procs.iter().map(|p| p.line))
            .chain(src.outputs.iter().map(|r| r.line))
            .chain(src.inputs.iter().map(|r| r.line))
            .filter(|l| *l > 0)
            .min_by_key(|l| order_key(*l));
        // (Not in a module's listing, as observed.)
        if has_ctl && !o.module {
            if let Some(l) = first_spec {
                before
                    .entry(l)
                    .or_default()
                    .extend(options_box(o, &m.text, src, &options));
            }
        }
        if let Some(prog) = input.prog {
            for f in &prog.files {
                if !f.decl.external || f.decl.line == 0 {
                    continue;
                }
                // A box after each externally described file.
                let ext = match (&f.fref, &f.display) {
                    (Some(r), _) => format!("{}/{}", r.library, r.file.name),
                    (None, Some(d)) => format!("{}/{}", d.library, f.decl.name),
                    _ => f.decl.name.clone(),
                };
                let formats: Vec<(String, String)> = match &f.display {
                    Some(d) => d
                        .formats
                        .iter()
                        .map(|(n, _)| (n.clone(), n.clone()))
                        .collect(),
                    None => {
                        let rpg = f.format.clone();
                        let external = keyword(&f.decl.keywords, "RENAME")
                            .and_then(|k| k.args.first())
                            .map(|a| a.trim().to_ascii_uppercase())
                            .unwrap_or_else(|| rpg.clone());
                        vec![(rpg, external)]
                    }
                };
                let dash = format!("        *{}*", "-".repeat(92));
                let mut b = vec![
                    Row::Box(dash.clone(), None),
                    Row::Box(
                        boxed(&format!("{:31}RPG name         External name", ""), 102),
                        None,
                    ),
                    Row::Box(
                        boxed(
                            &format!(" File name. . . . . . . . . :  {:<17}{ext}", f.decl.name),
                            102,
                        ),
                        None,
                    ),
                ];
                for (k, (rpg, external)) in formats.iter().enumerate() {
                    let l = if k == 0 {
                        " Record format(s) . . . . . :  "
                    } else {
                        "                               "
                    };
                    b.push(Row::Box(
                        boxed(&format!("{l}{rpg:<17}{external}"), 102),
                        None,
                    ));
                }
                b.push(Row::Box(dash, None));
                after.entry(f.decl.line).or_default().extend(b);
                // Generated specifications (*EXPDDS) of a database file.
                let Some(r) = &f.fref else { continue };
                if f.decl.device != "DISK" {
                    continue;
                }
                let stored: Vec<DType> = r
                    .file
                    .fields
                    .iter()
                    .map(|fd| {
                        super::program::program_type(fd, true, false).unwrap_or(DType::Char(0))
                    })
                    .collect();
                let ext_fmt = format!("{} : {}/{}", r.file.format, r.library, r.file.name);
                if matches!(f.decl.usage, 'I' | 'U' | 'C') {
                    let mut fields = Vec::new();
                    let mut end = 0u32;
                    for (name, t) in f.fields.iter().zip(&stored) {
                        let start = end + 1;
                        end += t.size() as u32;
                        let (c, _, _) = out_type(t);
                        let dec = match t {
                            DType::Packed(_, d) | DType::Zoned(_, d) | DType::Binary(_, d) => {
                                d.to_string()
                            }
                            DType::Int(_) | DType::Uns(_) => "0".into(),
                            _ => String::new(),
                        };
                        fields.push((
                            name.clone(),
                            format!("I{:29}{c}{start:>5}{end:>5} {dec:<1}{name}", ""),
                        ));
                    }
                    in_update.insert(f.format.clone(), f.decl.usage == 'U');
                    gen_in.push((f.format.clone(), ext_fmt.clone(), fields));
                }
                if f.decl.usage == 'O' || f.decl.usage == 'U' || f.decl.add {
                    let mut fields = Vec::new();
                    let mut end = 0u32;
                    for (name, t) in f.fields.iter().zip(&stored) {
                        let (c, ext, len) = out_type(t);
                        end += t.size() as u32;
                        let text =
                            format!("O{:23}{:<20}{:>2}{c} {:<10}{:>3}", "", name, end, ext, len);
                        fields.push((name.clone(), text, end + 1 - t.size() as u32, end));
                    }
                    gen_out.push((f.format.clone(), ext_fmt, fields));
                }
            }
        }
    }
    // Where the generated output specifications go: before the first
    // procedure; the input specifications: before the first calculation.
    let gen_out_at = input
        .src
        .and_then(|s| s.procs.iter().map(|p| p.line).filter(|l| *l > 0).min())
        .unwrap_or(src_lines.len() + 1);
    let gen_in_at = first_calc.unwrap_or(gen_out_at);
    let mut next_id = 0usize;
    let gen_box = |rows: &mut Vec<Row>, id: usize, fmt: &str, ext: &str| {
        let dash = format!("        *{}*", "-".repeat(92));
        rows.push(Row::Box(dash.clone(), Some(id)));
        rows.push(Row::Box(
            boxed(&format!(" RPG record format  . . . . :  {fmt}"), 102),
            Some(id),
        ));
        rows.push(Row::Box(
            boxed(&format!(" External format  . . . . . :  {ext}"), 102),
            Some(id),
        ));
        rows.push(Row::Box(dash, Some(id)));
    };
    let mut rows: Vec<Row> = Vec::new();
    // Copies used: (index into input.copies, src id, line of the directive).
    let mut copies_used: Vec<(usize, usize, usize)> = Vec::new();
    for n in 1..=src_lines.len() + 1 {
        if n == gen_in_at && !gen_in.is_empty() {
            for (fmt, ext, fields) in std::mem::take(&mut gen_in) {
                next_id += 1;
                rows.push(Row::Gen(
                    format!("I{fmt}"),
                    next_id,
                    Gen::Record(fmt.clone()),
                ));
                gen_box(&mut rows, next_id, &fmt, &ext);
                let upd = in_update.get(&fmt).copied().unwrap_or(false);
                for (name, text) in fields {
                    rows.push(Row::Gen(text, next_id, Gen::Input(name, upd)));
                }
            }
        }
        if n == gen_out_at && !gen_out.is_empty() {
            for (fmt, ext, fields) in std::mem::take(&mut gen_out) {
                next_id += 1;
                rows.push(Row::Gen(
                    format!("O{fmt}"),
                    next_id,
                    Gen::Record(fmt.clone()),
                ));
                gen_box(&mut rows, next_id, &fmt, &ext);
                for (name, text, s, e) in fields {
                    rows.push(Row::Gen(text, next_id, Gen::Output(name, s, e)));
                }
            }
        }
        if n > src_lines.len() {
            break;
        }
        if let Some(b) = before.remove(&n) {
            rows.extend(b);
        }
        rows.push(Row::Src(n));
        if let Some(a) = after.remove(&n) {
            rows.extend(a);
        }
        // A /COPY member: its box, then its lines.
        if let Some(id) = copy_directive(src_lines[n - 1], free) {
            if let Some(k) = input.copies.iter().position(|c| c.id == id) {
                next_id += 1;
                copies_used.push((k, next_id, n));
                let c = &input.copies[k].member;
                let dash = format!("        *{}*", "-".repeat(92));
                let external = if c.stmf.is_empty() {
                    format!("{}/{}({})", c.library, c.file, c.name)
                } else {
                    c.stmf.clone()
                };
                let when = changed_stamp(c.changed_us)
                    .map(|s| format!("{}  {}", s.date, s.time))
                    .unwrap_or_default();
                rows.push(Row::Ruler(true));
                rows.push(Row::Box(dash.clone(), None));
                for (l, v) in [
                    (" RPG member name  . . . . . :  ", c.name.as_str()),
                    (" External name  . . . . . . :  ", external.as_str()),
                    (" Last change  . . . . . . . :  ", when.as_str()),
                ] {
                    rows.push(Row::Box(boxed(&format!("{l}{v}"), 102), Some(next_id)));
                }
                rows.push(Row::Box(dash, None));
                rows.push(Row::Ruler(!super::parse::is_fully_free(&c.source)));
                let ordinal = copies_used.len();
                for (i, _) in c.source.lines().enumerate() {
                    if let Some(b) = before.remove(&(ordinal * COPY_LINES + i + 1)) {
                        rows.extend(b);
                    }
                    rows.push(Row::Copy(k, i + 1, ordinal));
                    if let Some(a) = after.remove(&(ordinal * COPY_LINES + i + 1)) {
                        rows.extend(a);
                    }
                }
                // The ID of each copy line is this copy's.
                let last = rows.len();
                let _ = last;
            }
        }
    }
    // The source ID of each copy, by index into input.copies (the first
    // use).
    let copy_id = |k: usize| {
        copies_used
            .iter()
            .find(|(c, _, _)| *c == k)
            .map(|x| x.1)
            .unwrap_or(0)
    };
    // Number the rows.
    let mut number: HashMap<usize, usize> = HashMap::new();
    let mut copy_number: HashMap<(usize, usize), usize> = HashMap::new();
    // Text of each numbered line, for the references: (number, text,
    // free form).
    let mut texts: Vec<(usize, String, bool)> = Vec::new();
    let mut gens: Vec<(usize, Gen)> = Vec::new();
    let mut listing_no = 0usize;
    let mut comments = 0usize;
    let mut not_specs = 0usize;
    let mut gen_seq: HashMap<usize, usize> = HashMap::new();
    let mut lines_out: Vec<String> = Vec::new();
    let seq_of = |n: usize| -> String {
        if member {
            format!("{:06}", n * 100)
        } else {
            format!("{n:06}")
        }
    };
    let count_line = |text: &str, form_free: bool, comments: &mut usize, not_specs: &mut usize| {
        let t = text.trim().to_ascii_uppercase();
        if t.starts_with("**FREE") {
            *not_specs += 1;
        } else if is_comment(text, form_free) {
            *comments += 1;
        } else if form_free
            && matches!(
                t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .next()
                    .unwrap_or(""),
                "END-DS" | "END-PI" | "END-PR"
            )
        {
            *not_specs += 1;
        }
    };
    for r in &rows {
        match r {
            Row::Src(n) => {
                listing_no += 1;
                number.insert(*n, listing_no);
                let raw = src_lines[n - 1];
                count_line(raw, free, &mut comments, &mut not_specs);
                texts.push((listing_no, raw.to_string(), free));
                let d = dos.get(n).cloned().unwrap_or_default();
                let date = if member { "000000" } else { "" };
                let s = if free {
                    let text: String = raw.chars().take(100).collect();
                    format!(
                        "{listing_no:>6} {text:<100} {d:<3}  {date:<6}     {}",
                        seq_of(*n)
                    )
                } else {
                    format!(
                        "{listing_no:>6} {:<95} {d:<3}       {date:<6}     {}",
                        fixed_text(raw),
                        seq_of(*n)
                    )
                };
                lines_out.push(s.trim_end().to_string());
                // Messages that point at a place on the line: a marker
                // line, then the message with the marker's letter
                // (PUB400 2026-10-28, tools/golden/verify13.txt
                // t13_rpge: "======>" and "aaaa" under LIKEREC's argument,
                // "*RNF3546 20 a      000031  ...").
                let marked: Vec<&super::check::Diag> = input
                    .diags
                    .iter()
                    .filter(|d| d.line == *n && d.at.is_some())
                    .collect();
                if !marked.is_empty() {
                    let mut m = String::from("======>");
                    for (k, d) in marked.iter().enumerate() {
                        let (col, len) = d.at.unwrap_or_default();
                        let letter = (b'a' + (k % 26) as u8) as char;
                        while m.chars().count() < 7 + col {
                            m.push(' ');
                        }
                        m.extend(std::iter::repeat_n(letter, len.max(1)));
                    }
                    lines_out.push(m);
                    for (k, d) in marked.iter().enumerate() {
                        let letter = ((b'a' + (k % 26) as u8) as char).to_string();
                        let head = format!(
                            "*{:<7} {:02} {:<6} {:<6}  ",
                            d.id,
                            d.sev,
                            letter,
                            seq_of(*n)
                        );
                        lines_out.extend(wrapped(&head, &d.text, 27, 86));
                    }
                }
            }
            Row::Copy(k, line, ordinal) => {
                listing_no += 1;
                copy_number.insert((*ordinal, *line), listing_no);
                let c = &input.copies[*k].member;
                let cfree = super::parse::is_fully_free(&c.source);
                let raw = c.source.lines().nth(line - 1).unwrap_or("");
                count_line(raw, cfree, &mut comments, &mut not_specs);
                texts.push((listing_no, raw.to_string(), cfree));
                let (date, seq) = if c.stmf.is_empty() {
                    ("000000", format!("{:06}", line * 100))
                } else {
                    ("", format!("{line:06}"))
                };
                let id = copy_id(*k);
                let d = dos
                    .get(&(ordinal * COPY_LINES + line))
                    .cloned()
                    .unwrap_or_default();
                let s = if cfree {
                    let text: String = raw.chars().take(100).collect();
                    format!("{listing_no:>6}+{text:<100} {d:<3}  {date:<6}    {id}{seq}")
                } else {
                    format!(
                        "{listing_no:>6}+{:<95} {d:<3}       {date:<6}    {id}{seq}",
                        fixed_text(raw)
                    )
                };
                lines_out.push(s.trim_end().to_string());
            }
            Row::Gen(t, id, g) => {
                listing_no += 1;
                let k = gen_seq.entry(*id).or_insert(0);
                *k += 1;
                gens.push((listing_no, g.clone()));
                lines_out.push(format!("{listing_no:>6}={t:<116}{id}{k:06}"));
            }
            Row::Box(t, id) => match id {
                Some(id) => lines_out.push(format!("{t:<123}{id}")),
                None => lines_out.push(t.clone()),
            },
            Row::Ruler(fixed) => {
                let r = if *fixed { RULER_FIXED } else { RULER_FREE };
                lines_out.extend(r.iter().map(|x| x.to_string()));
            }
        }
    }
    let total_records = listing_no;
    let mut source_section = vec![at(&[(27, "S o u r c e   L i s t i n g")])];
    source_section.extend(lines_out);
    source_section.push(END_SOURCE.into());
    // The listing line of a parsed line (copied lines: n * COPY_LINES +
    // line).
    let lno = |line: usize| {
        if line >= COPY_LINES {
            copy_number
                .get(&(line / COPY_LINES, line % COPY_LINES))
                .copied()
                .unwrap_or(0)
        } else {
            number.get(&line).copied().unwrap_or(line)
        }
    };
    let def_line = |line: usize, _name: &str| -> usize { lno(line) };

    // ------------------------------------------------------ diagnostics
    let mut diag_section = vec![
        at(&[(
            11,
            "A d d i t i o n a l   D i a g n o s t i c   M e s s a g e s",
        )]),
        " Msg id  Sv Number Seq     Message text".to_string(),
    ];
    for d in input.diags.iter().filter(|d| d.at.is_none()) {
        let (n, seq) = if d.line >= COPY_LINES {
            (
                lno(d.line).to_string(),
                format!("{}{:06}", d.line / COPY_LINES, (d.line % COPY_LINES) * 100),
            )
        } else if d.line > 0 {
            (lno(d.line).to_string(), seq_of(d.line))
        } else {
            (String::new(), String::new())
        };
        let head = format!("*{:<7} {:02} {:>6} {:<6}  ", d.id, d.sev, n, seq);
        diag_section.extend(wrapped(&head, &d.text, 27, 86));
    }
    diag_section.push(END_DIAG.into());
    let buffer: Vec<(usize, u32, u32, String)> = gens
        .iter()
        .filter_map(|(n, g)| match g {
            Gen::Output(name, s, e) => Some((*n, *s, *e, name.clone())),
            _ => None,
        })
        .collect();
    if !buffer.is_empty() {
        diag_section.push(at(&[(18, "O u t p u t   B u f f e r   P o s i t i o n s")]));
        diag_section.push("Line   Start End   Field or Constant".into());
        diag_section.push("Number Pos   Pos".into());
        for (n, start, end, name) in &buffer {
            diag_section.push(format!("{n:>6}{start:>6}{end:>6} {name}"));
        }
        diag_section.push(END_BUFFER.into());
    }
    if !copies_used.is_empty() {
        diag_section.push(at(&[(29, "/ C o p y   M e m b e r s")]));
        diag_section.push(
            "Line   Src  RPG name   <-------- External name -------> CCSID  <- Last change ->"
                .into(),
        );
        diag_section.push(
            "Number Id              Library    File       Member            Date     Time".into(),
        );
        for (k, id, line) in &copies_used {
            let c = &input.copies[*k].member;
            let when = changed_stamp(c.changed_us)
                .map(|s| format!("{} {}", s.date, s.time))
                .unwrap_or_default();
            diag_section.push(format!(
                "{:>6}{id:>5} {:<10} {:<10} {:<10} {:<10}{:>6}  {when}",
                lno(*line),
                c.name,
                c.library,
                c.file,
                c.name,
                c.ccsid
            ));
        }
        diag_section.push(
            "         * * * * *   E N D   O F   / C O P Y   M E M B E R S   * * * * *".into(),
        );
    }

    // -------------------------------------------------- cross reference
    // References: name → [(listing line, flag)]; indicators apart.
    let mut refs: BTreeMap<String, Vec<(usize, char)>> = BTreeMap::new();
    let mut inds: BTreeMap<String, Vec<(usize, char)>> = BTreeMap::new();
    let mut mods: HashSet<(usize, String)> = HashSet::new();
    if let Some(src) = input.src {
        for c in src
            .calcs
            .iter()
            .chain(src.procs.iter().flat_map(|p| p.calcs.iter()))
        {
            for n in modified(c) {
                mods.insert((lno(c.line), n));
            }
        }
    }
    for (n, l, lfree) in &texts {
        let mut seen_mod: HashSet<String> = HashSet::new();
        for w in names_in(l, *lfree) {
            let parts: Vec<&str> = w.split('.').collect();
            for (k, p) in parts.iter().enumerate() {
                let key = if k == 0 {
                    p.to_string()
                } else {
                    format!("{}.{}", parts[..k].join("."), p)
                };
                let flag = if k == 0
                    && mods.contains(&(*n, p.to_string()))
                    && seen_mod.insert(p.to_string())
                {
                    'M'
                } else {
                    ' '
                };
                refs.entry(key.clone()).or_default().push((*n, flag));
                if k > 0 {
                    refs.entry(p.to_string()).or_default().push((*n, ' '));
                }
                if let Some(i) = p.strip_prefix("*IN") {
                    if i.len() == 2 {
                        inds.entry(i.to_string()).or_default().push((*n, flag));
                    }
                }
            }
        }
        // Fixed form: resulting indicators (71-76) are set here (D).
        if !*lfree {
            let cs: Vec<char> = l.chars().collect();
            if cs.get(5).is_some_and(|c| c.eq_ignore_ascii_case(&'C')) && cs.get(6) != Some(&'*') {
                for k in [70usize, 72, 74] {
                    let i: String = cs.iter().skip(k).take(2).collect();
                    let i = i.trim().to_ascii_uppercase();
                    if !i.is_empty() {
                        inds.entry(i).or_default().push((*n, 'D'));
                    }
                }
            }
        }
    }
    for (n, g) in &gens {
        match g {
            Gen::Record(f) => refs.entry(f.clone()).or_default().push((*n, ' ')),
            Gen::Input(f, upd) => refs
                .entry(f.clone())
                .or_default()
                .push((*n, if *upd { 'M' } else { 'D' })),
            Gen::Output(f, _, _) => refs.entry(f.clone()).or_default().push((*n, ' ')),
        }
    }
    let mut unreferenced = 0usize;
    let xref = cross_reference(input, &refs, &inds, &lno, &def_line, &mut unreferenced);
    let ext = external_references(input, &refs);

    // --------------------------------------------------- message summary
    let mut by_id: BTreeMap<String, (u8, usize, String)> = BTreeMap::new();
    for d in input.diags {
        let e = by_id
            .entry(d.id.clone())
            .or_insert((d.sev, 0, general_text(d)));
        e.1 += 1;
    }
    if unreferenced > 0 {
        by_id.insert(
            "RNF7031".into(),
            (0, unreferenced, "A name or indicator is not used.".into()),
        );
    }
    let mut msgs = vec![
        at(&[(27, "M e s s a g e   S u m m a r y")]),
        " Msg id  Sv Number Message text".into(),
    ];
    // By severity, then message ID (verify12 n12_rpgp: 00 RNF7031,
    // RNF7066, RNF7086; 20 RNF3546; 30 RNF7030, RNF7260, RNF7503).
    let mut summary: Vec<_> = by_id.iter().collect();
    summary.sort_by_key(|(id, (sev, _, _))| (*sev, (*id).clone()));
    for (id, (sev, n, text)) in summary {
        let head = format!("*{id:<7} {sev:02} {n:>6} ");
        msgs.extend(wrapped(&head, text, 19, 82));
    }
    msgs.push(END_MSGS.into());

    // ------------------------------------------------------ final summary
    let count = |lo: u8, hi: u8| {
        input
            .diags
            .iter()
            .filter(|d| d.sev >= lo && d.sev <= hi)
            .count()
    };
    let ctdata: usize = input
        .src
        .map(|s| s.ctdata.iter().map(|(_, l)| l.len()).sum())
        .unwrap_or(0);
    let specs = total_records.saturating_sub(comments + not_specs + ctdata);
    let sum = |t: &str, v: usize| {
        let l = crate::commands::dsp_label(t, 30);
        at(&[(5, &l), (39, &format!("{v:>8}"))])
    };
    let max = input.diags.iter().map(|d| d.sev).max().unwrap_or(0);
    let mut fin = vec![
        at(&[(29, "F i n a l   S u m m a r y")]),
        "  Message Totals:".into(),
        sum("Information  (00)", count(0, 9) + unreferenced),
        sum("Warning      (10)", count(10, 19)),
        sum("Error        (20)", count(20, 29)),
        sum("Severe Error (30+)", count(30, 99)),
        "    ---------------------------------  -------".into(),
        sum("Total", input.diags.len() + unreferenced),
        "  Source Totals:".into(),
        sum("Records", total_records),
        sum("Specifications", specs),
        sum("Data records", ctdata),
        sum("Comments", comments),
        END_FINAL.into(),
    ];
    fin.push(if input.created {
        format!(
            "{what} {} was created in library {}; highest severity {max:02}. Created on {} at {}.",
            o.program, o.library, stamp.date, stamp.time
        )
    } else {
        format!("Compilation stopped: errors of severity {max:02} were found in the program.")
    });
    fin.push(END_COMPILE.into());

    // ------------------------------------------------------------- pages
    let mut out = Vec::new();
    let mut page = 0;
    let mut paged = |section: Vec<String>, headings: &[&str], out: &mut Vec<String>| {
        let room = PAGE - 1 - headings.len();
        for chunk in section.chunks(room) {
            page += 1;
            out.push(header(page));
            out.extend(headings.iter().map(|h| h.to_string()));
            out.extend(chunk.iter().cloned());
        }
    };
    paged(pro, &[], &mut out);
    let rulers = if free { RULER_FREE } else { RULER_FIXED };
    paged(source_section, &rulers, &mut out);
    paged(diag_section, &[], &mut out);
    paged(xref, &[], &mut out);
    paged(ext, &[], &mut out);
    paged(msgs, &[], &mut out);
    paged(fin, &[], &mut out);
    out
}

/// A box line: `text` after "        *", the closing "*" in column `end`.
fn boxed(text: &str, end: usize) -> String {
    format!("        *{text:<w$}*", w = end - 10)
}

/// A message: `head`, then the text wrapped at `width` with the
/// following lines indented to column `indent` + 1.
fn wrapped(head: &str, text: &str, indent: usize, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = head.to_string();
    for w in text.split(' ') {
        if cur.chars().count() + w.len() + 1 > width && cur.trim().len() > head.trim().len() {
            out.push(cur.trim_end().to_string());
            cur = " ".repeat(indent);
        }
        if !cur.ends_with(' ') && !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    out.push(cur.trim_end().to_string());
    out
}

/// A message's text without the names it is about, for the summary.
fn general_text(d: &Diag) -> String {
    match d.id.as_str() {
        "RNF7030" => "A name or indicator is not defined.".into(),
        "RNF7503" => "The expression has an operand that is not defined.".into(),
        "RNF7416" => {
            "The types of the target and the value of the EVAL operation do not match.".into()
        }
        "RNF7031" => "A name or indicator is not used.".into(),
        "RNF7086" => {
            "RPG blocks the records of the file; its INFDS changes only when a block is read or written."
                .into()
        }
        _ => d.text.clone(),
    }
}

/// The compiler options box.
fn options_box(o: &Opts, text: &str, src: &Source, options: &[&str]) -> Vec<Row> {
    let ctl = |k: &str| {
        keyword(&src.control, k).map(|kw| {
            kw.args
                .first()
                .map(|a| a.trim().to_ascii_uppercase())
                .unwrap_or_default()
        })
    };
    let dftactgrp = ctl("DFTACTGRP").unwrap_or_else(|| {
        if ctl("ACTGRP").is_some() || ctl("BNDDIR").is_some() {
            "*NO".into()
        } else {
            o.dftactgrp.clone()
        }
    });
    let actgrp = ctl("ACTGRP").unwrap_or_else(|| o.actgrp.clone());
    let srcdir = ctl("BNDDIR").unwrap_or_else(|| "*NONE".into());
    let line = |label: &str, value: &str| -> Row {
        let l = crate::commands::dsp_label(label, 31);
        Row::Box(boxed(&format!("  {l}   {value}"), 78), None)
    };
    let dash = format!("        *{}*", "-".repeat(68));
    let mut b = vec![
        Row::Box(dash.clone(), None),
        Row::Box(boxed(" Compiler Options in Effect:", 78), None),
        Row::Box(dash.clone(), None),
        line("Text 'description'", text),
        line("Generation severity level", &o.genlvl.to_string()),
        line("Default activation group", &dftactgrp),
    ];
    for (k, pair) in options.chunks(2).enumerate() {
        let v = format!("{:<11}{}", pair[0], pair.get(1).copied().unwrap_or(""));
        if k == 0 {
            b.push(line("Compiler options", v.trim_end()));
        } else {
            b.push(Row::Box(
                boxed(&format!("{:39}{}", "", v.trim_end()), 78),
                None,
            ));
        }
    }
    for (t, v) in [
        ("Optimization level", "*NONE"),
        ("Source listing indentation", "*NONE"),
        ("Type conversion options", "*NONE"),
        (
            "Sort sequence",
            if o.srtseq.is_empty() {
                "*HEX"
            } else {
                &o.srtseq
            },
        ),
        (
            "Language identifier",
            if o.langid.is_empty() {
                "*JOBRUN"
            } else {
                &o.langid
            },
        ),
        ("User profile", "*USER"),
        ("Authority", "*LIBCRTAUT"),
        ("Truncate numeric", "*YES"),
        ("Fix numeric", "*NONE"),
        ("Allow null values", o.alwnull.as_str()),
    ] {
        b.push(line(t, v));
    }
    // IBM i 7.5 prints this label with two blanks before its dots.
    b.push(Row::Box(
        boxed("  Storage model . . .  . . . . . . :   *SNGLVL", 78),
        None,
    ));
    let cmd_dir = if o.bnddir.is_empty() {
        "*NONE"
    } else {
        &o.bnddir
    };
    for (t, v) in [
        ("Binding directory from Command", cmd_dir),
        ("Binding directory from Source", srcdir.as_str()),
        ("Activation group", actgrp.as_str()),
        ("Enable performance collection", "*PEP"),
        ("Profiling data", "*NOCOL"),
        ("Generate program interface", "*NO"),
        ("Require prototype for export", "*NO"),
    ] {
        b.push(line(t, v));
    }
    b.push(Row::Box(dash, None));
    b
}

/// Reference columns: each reference in 8 columns from column 46, four
/// on a line (observed).
fn ref_lines(refs: &[(usize, char)]) -> Vec<String> {
    refs.chunks(4)
        .map(|c| {
            c.iter()
                .map(|(n, f)| format!("{n:>7}{f}"))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// References of a name: the defining line gets D (once).
fn with_def(mut r: Vec<(usize, char)>, def: Option<usize>) -> Vec<(usize, char)> {
    r.sort_by_key(|x| x.0);
    if let Some(d) = def {
        if let Some(x) = r.iter_mut().find(|x| x.0 == d && x.1 == ' ') {
            x.1 = 'D';
        } else if !r.iter().any(|x| x.0 == d) {
            r.push((d, 'D'));
            r.sort_by_key(|x| x.0);
        }
        // The name again on its own definition (`dcl-ds psds psds`, a
        // file's RENAME(file: format)) is not a reference (PUB400
        // 2026-10-28, tools/golden/verify12.txt n12_rpgp: "PSDS DS(46)
        // 8D" and "GLD12OUT DISK 6D", both RNF7031).
        let mut seen = false;
        r.retain(|x| {
            if x.0 != d {
                return true;
            }
            if x.1 == 'D' && !seen {
                seen = true;
                return true;
            }
            x.1 != ' '
        });
    }
    r
}

fn name_rows(prefix: &str, indent: usize, x: &XName, refs: &[(usize, char)]) -> Vec<String> {
    let mut out = Vec::new();
    let lines = ref_lines(refs);
    let first = at(&[
        (1, prefix),
        (10 + indent, &x.name),
        (28, x.attrs.first().map(|s| s.as_str()).unwrap_or("")),
    ]);
    let first = format!("{first:<45}{}", lines.first().cloned().unwrap_or_default());
    out.push(first.trim_end().to_string());
    for a in x.attrs.iter().skip(1) {
        out.push(at(&[(28, a)]));
    }
    for l in lines.iter().skip(1) {
        out.push(format!("{:45}{l}", ""));
    }
    out
}

fn cross_reference(
    input: &Input,
    refs: &BTreeMap<String, Vec<(usize, char)>>,
    inds: &BTreeMap<String, Vec<(usize, char)>>,
    lno: &dyn Fn(usize) -> usize,
    def_line: &dyn Fn(usize, &str) -> usize,
    unreferenced: &mut usize,
) -> Vec<String> {
    let mut out = vec![
        at(&[(27, "C r o s s   R e f e r e n c e")]),
        "      File and Record References:".into(),
        "         File              Device             References (D=Defined)".into(),
        "           Record".into(),
    ];
    let get = |n: &str| refs.get(n).cloned().unwrap_or_default();
    let ebc = |s: &str| crate::rpg::data::e(s);
    let mut files: Vec<_> = input
        .prog
        .map(|p| p.files.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    files.sort_by_cached_key(|f| ebc(&f.decl.name));
    if files.is_empty() {
        out.push("         No references in the source.".into());
    }
    for f in &files {
        let d = Some(lno(f.decl.line));
        let r = with_def(get(&f.decl.name), d);
        let l = ref_lines(&r);
        // A file neither named nor read or written through a record
        // format: RNF7031 (verify12 n12_rpgp: "*RNF7031 GLD12OUT DISK
        // 6D", its only WRITE not valid, RNF7066).
        let formats_unused = input
            .diags
            .iter()
            .any(|x| x.id == "RNF7066" && x.line == f.decl.line);
        let prefix = if formats_unused && r.iter().all(|x| x.1 == 'D') {
            *unreferenced += 1;
            "*RNF7031"
        } else {
            ""
        };
        out.push(
            format!(
                "{:<45}{}",
                format!(
                    "{prefix}{}",
                    &at(&[(10, &f.decl.name), (28, &f.decl.device)])[prefix.len()..]
                ),
                l.first().cloned().unwrap_or_default()
            )
            .trim_end()
            .to_string(),
        );
        for x in l.iter().skip(1) {
            out.push(format!("{:45}{x}", ""));
        }
        let mut formats: Vec<String> = match &f.display {
            Some(d) => d.formats.iter().map(|(n, _)| n.clone()).collect(),
            None => vec![f.format.clone()],
        };
        formats.sort_by_cached_key(|n| ebc(n));
        for fm in formats {
            let r = with_def(get(&fm), d);
            let l = ref_lines(&r);
            out.push(
                format!(
                    "{:<45}{}",
                    at(&[(12, &fm)]),
                    l.first().cloned().unwrap_or_default()
                )
                .trim_end()
                .to_string(),
            );
            for x in l.iter().skip(1) {
                out.push(format!("{:45}{x}", ""));
            }
        }
    }
    let heading = "         Field             Attributes         References (D=Defined M=Modified)";
    out.push("      Global Field References:".into());
    out.push(heading.into());
    let Some(prog) = input.prog else {
        out.push(END_XREF.into());
        return out;
    };
    let src = input.src;
    // Where each name is defined (listing lines).
    let mut defs: HashMap<String, usize> = HashMap::new();
    if let Some(s) = src {
        for d in &s.defs {
            let n = d.name.to_ascii_uppercase();
            let l = def_line(d.line, &n);
            defs.entry(n).or_insert(l);
        }
    }
    let mut names = global_names(prog, &defs, refs);
    for (n, _) in input.undefined {
        if !names.iter().any(|x| x.name == *n) {
            names.push(XName {
                name: n.clone(),
                attrs: vec!["**UNDEF**".into()],
                def: None,
                children: vec![],
                undefined: true,
            });
        }
    }
    names.sort_by_cached_key(|x| ebc(&x.name));
    // A name defined and never used: RNF7031 (00), in the cross
    // reference and the summaries (observed on IBM i 7.5).
    let mut unused = |r: &[(usize, char)]| -> bool {
        let u = !r.is_empty() && r.iter().all(|x| x.1 == 'D');
        if u {
            *unreferenced += 1;
        }
        u
    };
    for x in &names {
        let r = with_def(get(&x.name), x.def);
        let prefix = if x.undefined {
            "*RNF7030"
        } else if !x.name.starts_with('*') && unused(&r) {
            "*RNF7031"
        } else {
            ""
        };
        out.extend(name_rows(prefix, 0, x, &r));
        let mut kids = x.children.clone();
        kids.sort_by_cached_key(|k| ebc(&k.name));
        for k in &kids {
            let q = format!("{}.{}", x.name, k.name);
            let r = with_def(refs.get(&q).cloned().unwrap_or_else(|| get(&k.name)), k.def);
            let prefix = if unused(&r) { "*RNF7031" } else { "" };
            out.extend(name_rows(prefix, 2, k, &r));
        }
    }
    // Each procedure's own fields.
    if let Some(s) = src {
        for p in &s.procs {
            let pname = p.name.to_ascii_uppercase();
            let Some(info) = prog.procs.get(&pname) else {
                continue;
            };
            let mut locals: Vec<XName> = Vec::new();
            for (n, sym) in &info.syms {
                if n.contains('.') {
                    continue;
                }
                let def = p
                    .defs
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(n))
                    .map(|d| def_line(d.line, n));
                let attrs = match sym {
                    Sym::Var(v) => vec![attr(&v.dtype)],
                    Sym::Ds(d) => vec![format!("DS({})", d.size)],
                    Sym::Const(_) => vec!["CONST".into()],
                    Sym::Klist(_) => vec!["KLIST".into()],
                };
                locals.push(XName {
                    name: n.clone(),
                    attrs,
                    def,
                    children: vec![],
                    undefined: false,
                });
            }
            if locals.is_empty() {
                continue;
            }
            out.push(format!("      Field References for subprocedure {pname}"));
            out.push(heading.into());
            locals.sort_by_cached_key(|x| ebc(&x.name));
            // References inside the procedure only.
            let span = proc_span(s, p.line, lno);
            for x in &locals {
                let r: Vec<_> = get(&x.name)
                    .into_iter()
                    .filter(|(l, _)| *l >= span.0 && *l <= span.1)
                    .collect();
                let r = with_def(r, x.def);
                let prefix = if unused(&r) { "*RNF7031" } else { "" };
                out.extend(name_rows(prefix, 0, x, &r));
            }
        }
    }
    // Indicators.
    out.push("      Indicator References:".into());
    out.push(
        "         Indicator                            References (D=Defined M=Modified)".into(),
    );
    let mut list: Vec<&String> = inds.keys().collect();
    list.sort_by_cached_key(|i| ebc(i));
    for i in list {
        let mut r = inds[i].clone();
        r.sort_by_key(|x| x.0);
        let l = ref_lines(&r);
        out.push(
            format!(
                "{:<45}{}",
                at(&[(10, i)]),
                l.first().cloned().unwrap_or_default()
            )
            .trim_end()
            .to_string(),
        );
    }
    out.push(END_XREF.into());
    out
}

/// The listing lines a procedure spans: from its first line to the line
/// before the next procedure.
fn proc_span(s: &Source, start: usize, lno: &dyn Fn(usize) -> usize) -> (usize, usize) {
    let next = s
        .procs
        .iter()
        .map(|p| p.line)
        .filter(|l| *l > start)
        .min()
        .map(|l| lno(l) - 1)
        .unwrap_or(usize::MAX);
    (lno(start), next)
}

/// The program's global names: fields, data structures with their
/// subfields, constants, procedures and prototypes, and the indicators
/// used as fields (*INLR).
fn global_names(
    prog: &Program,
    defs: &HashMap<String, usize>,
    refs: &BTreeMap<String, Vec<(usize, char)>>,
) -> Vec<XName> {
    let mut out: Vec<XName> = Vec::new();
    let mut subfields: HashSet<String> = HashSet::new();
    for s in prog.syms.values() {
        if let Sym::Ds(d) = s {
            for f in &d.subfields {
                subfields.insert(f.to_ascii_uppercase());
            }
        }
    }
    for (n, s) in &prog.syms {
        if n.contains('.') || subfields.contains(n) || n.starts_with('*') {
            continue;
        }
        let (attrs, children) = match s {
            Sym::Var(v) => {
                let mut a = vec![attr(&v.dtype)];
                if v.dim > 0 {
                    a.push(format!("DIM({})", v.dim));
                }
                (a, vec![])
            }
            Sym::Ds(d) => {
                let kids = d
                    .subfields
                    .iter()
                    .map(|f| {
                        let f = f.to_ascii_uppercase();
                        let q = if d.qualified {
                            format!("{n}.{f}")
                        } else {
                            f.clone()
                        };
                        let attrs = match prog.syms.get(&q).or_else(|| prog.syms.get(&f)) {
                            Some(Sym::Var(v)) => vec![attr(&v.dtype)],
                            Some(Sym::Ds(x)) => vec![format!("DS({})", x.size)],
                            _ => vec![],
                        };
                        XName {
                            name: f.clone(),
                            attrs,
                            def: defs.get(&f).copied(),
                            children: vec![],
                            undefined: false,
                        }
                    })
                    .collect();
                (vec![format!("DS({})", d.size)], kids)
            }
            Sym::Const(_) => (vec!["CONST".into()], vec![]),
            Sym::Klist(_) => (vec!["KLIST".into()], vec![]),
        };
        out.push(XName {
            name: n.clone(),
            attrs,
            def: defs.get(n).copied(),
            children,
            undefined: false,
        });
    }
    let mut procs: Vec<(&String, Option<&DType>)> = prog
        .procs
        .iter()
        .map(|(n, p)| (n, p.returns.as_ref()))
        .collect();
    for (n, p) in &prog.protos {
        if !prog.procs.contains_key(n) {
            procs.push((n, p.returns.as_ref()));
        }
    }
    for (n, ret) in procs {
        if out.iter().any(|x| x.name == *n) {
            continue;
        }
        let mut attrs = vec![ret.map(attr).unwrap_or_default()];
        attrs.push("PROTOTYPE".into());
        out.push(XName {
            name: n.clone(),
            attrs,
            def: defs.get(n).copied(),
            children: vec![],
            undefined: false,
        });
    }
    for k in refs.keys() {
        if let Some(i) = k.strip_prefix("*IN") {
            if i.len() == 2 {
                out.push(XName {
                    name: k.clone(),
                    attrs: vec!["N(1)".into()],
                    def: None,
                    children: vec![],
                    undefined: false,
                });
            }
        }
    }
    out
}

fn external_references(input: &Input, refs: &BTreeMap<String, Vec<(usize, char)>>) -> Vec<String> {
    let none = "         No references in the source.".to_string();
    let mut out = vec![
        at(&[(23, "E x t e r n a l   R e f e r e n c e s")]),
        "      Statically bound procedures:".into(),
        "         Procedure                            References".into(),
    ];
    let prog = input.prog;
    // Prototypes of procedures that are not in this module (EXTPROC or
    // the name), whether called or not.
    let mut imports: Vec<String> = prog
        .map(|p| {
            p.protos
                .values()
                .filter(|x| x.extpgm.is_none())
                .filter(|x| {
                    let t = x.extproc.clone().unwrap_or_else(|| x.name.clone());
                    !p.procs.contains_key(&t.to_ascii_uppercase()) && !p.procs.contains_key(&x.name)
                })
                .map(|x| x.name.clone())
                .collect()
        })
        .unwrap_or_default();
    imports.sort_by_cached_key(|n| crate::rpg::data::e(n));
    if imports.is_empty() {
        out.push(none.clone());
    }
    for i in imports {
        let mut r = refs.get(&i).cloned().unwrap_or_default();
        r.sort_by_key(|x| x.0);
        for x in r.iter_mut() {
            x.1 = ' ';
        }
        let l = ref_lines(&r);
        out.push(
            format!(
                "{:<45}{}",
                at(&[(10, &i)]),
                l.first().cloned().unwrap_or_default()
            )
            .trim_end()
            .to_string(),
        );
    }
    for (title, list) in [
        (
            "      Imported fields:",
            prog.map(|p| p.data_imports.clone()).unwrap_or_default(),
        ),
        (
            "      Exported fields:",
            prog.map(|p| p.data_exports.clone()).unwrap_or_default(),
        ),
    ] {
        out.push(title.into());
        out.push("         Field             Attributes         Defined".into());
        if list.is_empty() {
            out.push(none.clone());
        }
        for (_, n) in list {
            let a = match prog.and_then(|p| p.syms.get(&n)) {
                Some(Sym::Var(v)) => attr(&v.dtype),
                Some(Sym::Ds(d)) => format!("DS({})", d.size),
                _ => String::new(),
            };
            out.push(at(&[(10, &n), (28, &a)]));
        }
    }
    out.push(END_EXT.into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_skip_literals_and_comments() {
        assert_eq!(
            names_in("  ID = 'ROW' + %char(i); // note", true),
            ["ID", "CHAR", "I"]
        );
        assert_eq!(names_in("*inlr = *on;", true), ["*INLR", "*ON"]);
        assert_eq!(names_in("  totals.count += 1;", true), ["TOTALS.COUNT"]);
    }
}
