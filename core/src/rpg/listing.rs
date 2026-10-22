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
    /// A source line: its number in the source.
    Src(usize),
    /// A generated output specification line (its text after "=").
    Gen(String),
    /// A box line; `true`: it belongs to generated specifications.
    Box(String, bool),
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
        DType::Timestamp => "Z(26)".into(),
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
        DType::Timestamp => ('Z', "TIMESTAMP", "26".into()),
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
    let free = src_lines
        .first()
        .is_some_and(|l| l.trim_start().to_ascii_uppercase().starts_with("**FREE"));
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
    let mut pro = vec![
        label(3, "Command", &o.command),
        label(5, "Issued by", &o.user),
        label(3, "Program", &o.program),
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
        // a stream file in CCSID 819).
        let target = match m.ccsid {
            819 | 850 | 437 | 1252 => 500,
            1208 | 1200 | 13488 => 37,
            c => c,
        };
        pro.push(label(3, "Target CCSID", &format!("*SRC ({target})")));
    }
    pro.push(label(3, "Text 'description'", &m.text));
    pro.push(label(3, "Last Change", &changed));
    pro.push(label(3, "Generation severity level", &o.genlvl.to_string()));
    pro.push(label(3, "Default activation group", &o.dftactgrp));
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
    for (t, v) in [
        ("Debugging views", o.dbgview.as_str()),
        ("Debug encryption key", "*NONE"),
        ("Output", "*PRINT"),
        ("Optimization level", "*NONE"),
        ("Source listing indentation", "*NONE"),
        ("Type conversion options", "*NONE"),
        ("Sort sequence", "*HEX"),
        ("Language identifier", "*JOBRUN"),
        ("Replace program", o.replace.as_str()),
        ("User profile", "*USER"),
        ("Authority", "*LIBCRTAUT"),
        ("Truncate numeric", "*YES"),
        ("Fix numeric", "*NONE"),
        ("Target release", o.tgtrls.as_str()),
        ("Allow null values", o.alwnull.as_str()),
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
    let mut rows: Vec<Row> = Vec::new();
    let mut dos: HashMap<usize, String> = HashMap::new();
    let mut after: HashMap<usize, Vec<Row>> = HashMap::new();
    let mut before: HashMap<usize, Vec<Row>> = HashMap::new();
    // Generated output specifications: (record format, file, fields).
    let mut generated: Vec<Row> = Vec::new();
    let mut gen_refs: Vec<(usize, String)> = Vec::new(); // (index in generated, name)
    let mut buffer: Vec<(usize, u32, u32, String)> = Vec::new(); // (gen index, start, end, field)
    if let Some(src) = input.src {
        do_numbers(&src.calcs, &mut dos);
        for p in &src.procs {
            do_numbers(&p.calcs, &mut dos);
        }
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
            .min();
        if has_ctl {
            if let Some(l) = first_spec {
                before.insert(l, options_box(o, &m.text, src, &options));
            }
        }
        // A box after each externally described file.
        if let Some(prog) = input.prog {
            for f in &prog.files {
                if !f.decl.external || f.decl.line == 0 {
                    continue;
                }
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
                    Row::Box(dash.clone(), false),
                    Row::Box(
                        boxed(&format!("{:31}RPG name         External name", ""), 102),
                        false,
                    ),
                    Row::Box(
                        boxed(
                            &format!(" File name. . . . . . . . . :  {:<17}{ext}", f.decl.name),
                            102,
                        ),
                        false,
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
                        false,
                    ));
                }
                b.push(Row::Box(dash, false));
                after.entry(f.decl.line).or_default().extend(b);
            }
            // Output specifications for the records of externally
            // described output files (*EXPDDS), after the main source.
            for f in &prog.files {
                let Some(r) = &f.fref else { continue };
                if !f.decl.external || !(f.decl.usage == 'O' || f.decl.add) {
                    continue;
                }
                generated.push(Row::Gen(format!("O{}", f.format)));
                gen_refs.push((generated.len() - 1, f.format.clone()));
                let dash = format!("        *{}*", "-".repeat(92));
                generated.push(Row::Box(dash.clone(), true));
                generated.push(Row::Box(
                    boxed(&format!(" RPG record format  . . . . :  {}", f.format), 102),
                    true,
                ));
                generated.push(Row::Box(
                    boxed(
                        &format!(
                            " External format  . . . . . :  {} : {}/{}",
                            r.file.format, r.library, r.file.name
                        ),
                        102,
                    ),
                    true,
                ));
                generated.push(Row::Box(dash, true));
                let mut end = 0u32;
                // Each field as stored in the record (the record format's
                // fields, by their program names).
                let stored: Vec<DType> = r
                    .file
                    .fields
                    .iter()
                    .map(|fd| {
                        super::program::program_type(fd, true, false).unwrap_or(DType::Char(0))
                    })
                    .collect();
                for (name, t) in f.fields.iter().zip(&stored) {
                    let (c, ext, len) = out_type(t);
                    end += t.size() as u32;
                    let text = format!("O{:23}{:<20}{:>2}{c} {:<10}{:>3}", "", name, end, ext, len);
                    generated.push(Row::Gen(text));
                    gen_refs.push((generated.len() - 1, name.clone()));
                    buffer.push((
                        generated.len() - 1,
                        end + 1 - t.size() as u32,
                        end,
                        name.clone(),
                    ));
                }
            }
        }
    }
    // Where the generated specifications go: before the first procedure.
    let gen_at = input
        .src
        .and_then(|s| s.procs.iter().map(|p| p.line).filter(|l| *l > 0).min())
        .unwrap_or(src_lines.len() + 1);
    for n in 1..=src_lines.len() + 1 {
        if n == gen_at {
            rows.append(&mut generated);
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
    }
    // Number the rows: listing line numbers.
    let mut number: HashMap<usize, usize> = HashMap::new(); // source line → listing line
    let mut gen_number: HashMap<usize, usize> = HashMap::new(); // generated index → listing line
    let mut listing_no = 0usize;
    let mut gen_index = 0usize;
    let mut gen_seen = 0usize;
    let mut lines_out: Vec<String> = Vec::new();
    let seq_of = |n: usize| -> String {
        if member {
            format!("{:06}", n * 100)
        } else {
            format!("{n:06}")
        }
    };
    for r in &rows {
        match r {
            Row::Src(n) => {
                listing_no += 1;
                number.insert(*n, listing_no);
                let text: String = src_lines[n - 1].chars().take(100).collect();
                let d = dos.get(n).cloned().unwrap_or_default();
                let date = if member { "000000" } else { "" };
                let s = format!(
                    "{listing_no:>6} {text:<100} {d:<3}  {date:<6}     {}",
                    seq_of(*n)
                );
                lines_out.push(s.trim_end().to_string());
                gen_index += 0;
            }
            Row::Gen(t) => {
                listing_no += 1;
                gen_seen += 1;
                gen_number.insert(gen_index, listing_no);
                let s = format!("{listing_no:>6}={t:<116}1{gen_seen:06}");
                lines_out.push(s);
                gen_index += 1;
            }
            Row::Box(t, gen) => {
                if *gen {
                    lines_out.push(format!("{t:<123}1"));
                    gen_index += 1;
                } else {
                    lines_out.push(t.clone());
                }
            }
        }
    }
    let total_records = listing_no;
    let mut source_section = vec![at(&[(27, "S o u r c e   L i s t i n g")])];
    source_section.extend(lines_out);
    source_section.push(END_SOURCE.into());
    let lno = |line: usize| number.get(&line).copied().unwrap_or(line);

    // ------------------------------------------------------ diagnostics
    let mut diag_section = Vec::new();
    if !input.diags.is_empty() {
        diag_section.push(at(&[(
            11,
            "A d d i t i o n a l   D i a g n o s t i c   M e s s a g e s",
        )]));
        diag_section.push(" Msg id  Sv Number Seq     Message text".into());
        for d in input.diags {
            let (n, seq) = if d.line > 0 {
                (lno(d.line).to_string(), seq_of(d.line))
            } else {
                (String::new(), String::new())
            };
            let head = format!("*{:<7} {:02} {:>6} {:<6}  ", d.id, d.sev, n, seq);
            diag_section.extend(wrapped(&head, &d.text, 27, 86));
        }
        diag_section.push(END_DIAG.into());
    }
    if !buffer.is_empty() {
        diag_section.push(at(&[(18, "O u t p u t   B u f f e r   P o s i t i o n s")]));
        diag_section.push("Line   Start End   Field or Constant".into());
        diag_section.push("Number Pos   Pos".into());
        for (g, start, end, name) in &buffer {
            let n = gen_number.get(g).copied().unwrap_or(0);
            diag_section.push(format!("{n:>6}{start:>6}{end:>6} {name}"));
        }
        diag_section.push(END_BUFFER.into());
    }

    // -------------------------------------------------- cross reference
    // References: (name → [(listing line, flag)]).
    let mut refs: BTreeMap<String, Vec<(usize, char)>> = BTreeMap::new();
    let mut mods: HashSet<(usize, String)> = HashSet::new();
    if let Some(src) = input.src {
        for c in src
            .calcs
            .iter()
            .chain(src.procs.iter().flat_map(|p| p.calcs.iter()))
        {
            for n in modified(c) {
                mods.insert((c.line, n));
            }
        }
    }
    for (i, l) in src_lines.iter().enumerate() {
        let n = i + 1;
        let mut seen_mod: HashSet<String> = HashSet::new();
        for w in names_in(l, free) {
            let parts: Vec<&str> = w.split('.').collect();
            for (k, p) in parts.iter().enumerate() {
                let key = if k == 0 {
                    p.to_string()
                } else {
                    format!("{}.{}", parts[..k].join("."), p)
                };
                let flag = if k == 0
                    && mods.contains(&(n, p.to_string()))
                    && seen_mod.insert(p.to_string())
                {
                    'M'
                } else {
                    ' '
                };
                refs.entry(key.clone()).or_default().push((lno(n), flag));
                if k > 0 {
                    refs.entry(p.to_string()).or_default().push((lno(n), ' '));
                }
            }
        }
    }
    for (g, name) in &gen_refs {
        if let Some(n) = gen_number.get(g) {
            refs.entry(name.clone()).or_default().push((*n, ' '));
        }
    }
    let xref = cross_reference(input, &refs, &lno);
    let ext = external_references(input, &refs);

    // --------------------------------------------------- message summary
    let mut by_id: BTreeMap<String, (u8, usize, String)> = BTreeMap::new();
    for d in input.diags {
        let e = by_id
            .entry(d.id.clone())
            .or_insert((d.sev, 0, general_text(d)));
        e.1 += 1;
    }
    let mut msgs = vec![
        at(&[(27, "M e s s a g e   S u m m a r y")]),
        " Msg id  Sv Number Message text".into(),
    ];
    for (id, (sev, n, text)) in &by_id {
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
    let comments = src_lines
        .iter()
        .enumerate()
        .filter(|(i, l)| {
            let t = l.trim();
            *i > 0 || !t.to_ascii_uppercase().starts_with("**FREE")
        })
        .filter(|(_, l)| {
            let t = l.trim();
            if free {
                t.is_empty() || t.starts_with("//")
            } else {
                t.is_empty() || l.chars().nth(6) == Some('*') || t.starts_with("//")
            }
        })
        .count();
    let ctdata: usize = input
        .src
        .map(|s| s.ctdata.iter().map(|(_, l)| l.len()).sum())
        .unwrap_or(0);
    let directives = src_lines
        .iter()
        .filter(|l| l.trim_start().to_ascii_uppercase().starts_with("**FREE"))
        .count();
    let gen_records = gen_refs
        .iter()
        .filter(|(g, _)| matches!(rows_gen_is_record(&rows, *g), Some(true)))
        .count();
    let specs = src_lines.len() - comments - directives - ctdata + gen_records;
    let sum = |t: &str, v: usize| {
        let l = crate::commands::dsp_label(t, 30);
        at(&[(5, &l), (39, &format!("{v:>8}"))])
    };
    let max = input.diags.iter().map(|d| d.sev).max().unwrap_or(0);
    let mut fin = vec![
        at(&[(29, "F i n a l   S u m m a r y")]),
        "  Message Totals:".into(),
        sum("Information  (00)", count(0, 9)),
        sum("Warning      (10)", count(10, 19)),
        sum("Error        (20)", count(20, 29)),
        sum("Severe Error (30+)", count(30, 99)),
        "    ---------------------------------  -------".into(),
        sum("Total", input.diags.len()),
        "  Source Totals:".into(),
        sum("Records", total_records),
        sum("Specifications", specs),
        sum("Data records", ctdata),
        sum("Comments", comments),
        END_FINAL.into(),
    ];
    fin.push(if input.created {
        format!(
            "Program {} was created in library {}; highest severity {max:02}. Created on {} at {}.",
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
    let h1 = "Line   <---------------------- Source Specifications ----------------------------------------------------->  Do  Change Src Seq";
    let h2 = "Number ....+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8....+....9....+...10  Num Date   Id  Number";
    paged(source_section, &[h1, h2], &mut out);
    if !diag_section.is_empty() {
        paged(diag_section, &[], &mut out);
    }
    paged(xref, &[], &mut out);
    paged(ext, &[], &mut out);
    paged(msgs, &[], &mut out);
    paged(fin, &[], &mut out);
    out
}

fn rows_gen_is_record(rows: &[Row], g: usize) -> Option<bool> {
    let mut k = 0usize;
    for r in rows {
        match r {
            Row::Gen(t) => {
                if k == g {
                    return Some(!t[1..].starts_with(' '));
                }
                k += 1;
            }
            Row::Box(_, true) => k += 1,
            _ => {}
        }
    }
    None
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
        Row::Box(boxed(&format!("  {l}   {value}"), 78), false)
    };
    let dash = format!("        *{}*", "-".repeat(68));
    let mut b = vec![
        Row::Box(dash.clone(), false),
        Row::Box(boxed(" Compiler Options in Effect:", 78), false),
        Row::Box(dash.clone(), false),
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
                false,
            ));
        }
    }
    for (t, v) in [
        ("Optimization level", "*NONE"),
        ("Source listing indentation", "*NONE"),
        ("Type conversion options", "*NONE"),
        ("Sort sequence", "*HEX"),
        ("Language identifier", "*JOBRUN"),
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
        false,
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
    b.push(Row::Box(dash, false));
    b
}

/// Reference columns: each reference in 8 columns from column 46.
fn ref_lines(refs: &[(usize, char)]) -> Vec<String> {
    refs.chunks(10)
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
    lno: &dyn Fn(usize) -> usize,
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
        let r = with_def(
            get(&f.decl.name)
                .into_iter()
                .filter(|x| Some(x.0) == d)
                .collect(),
            d,
        );
        let l = ref_lines(&r);
        out.push(
            format!(
                "{:<45}{}",
                at(&[(10, &f.decl.name), (28, &f.decl.device)]),
                l.first().cloned().unwrap_or_default()
            )
            .trim_end()
            .to_string(),
        );
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
            defs.entry(d.name.to_ascii_uppercase())
                .or_insert(lno(d.line));
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
    for x in &names {
        let r = with_def(get(&x.name), x.def);
        let prefix = if x.undefined { "*RNF7030" } else { "" };
        out.extend(name_rows(prefix, 0, x, &r));
        let mut kids = x.children.clone();
        kids.sort_by_cached_key(|k| ebc(&k.name));
        for k in &kids {
            let q = format!("{}.{}", x.name, k.name);
            let r = with_def(refs.get(&q).cloned().unwrap_or_else(|| get(&k.name)), k.def);
            out.extend(name_rows("", 2, k, &r));
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
                    .map(|d| lno(d.line));
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
                out.extend(name_rows("", 0, x, &with_def(r, x.def)));
            }
        }
    }
    // Indicators.
    out.push("      Indicator References:".into());
    out.push(
        "         Indicator                            References (D=Defined M=Modified)".into(),
    );
    let mut inds: Vec<String> = refs
        .keys()
        .filter_map(|k| k.strip_prefix("*IN"))
        .filter(|i| i.len() == 2 && !i.starts_with('('))
        .map(|i| i.to_string())
        .collect();
    inds.sort_by_cached_key(|i| ebc(i));
    if inds.is_empty() {
        out.push("         No references in the source.".into());
    }
    for i in inds {
        let r = get(&format!("*IN{i}"));
        let l = ref_lines(&with_def(r, None));
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
    let imports: Vec<String> = prog.map(|p| p.imports.clone()).unwrap_or_default();
    if imports.is_empty() {
        out.push(none.clone());
    }
    for i in imports {
        let r = refs.get(&i).cloned().unwrap_or_default();
        let l = ref_lines(&with_def(r, None));
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
