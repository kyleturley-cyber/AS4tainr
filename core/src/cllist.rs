//! The CL compiler's listing (CRTBNDCL, CRTCLPGM with OUTPUT(*PRINT)):
//! the program's attributes, the source with the compiler's messages
//! under the statements they are about, a cross reference of the
//! variables, and a summary of the messages by severity.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-22, CRTBNDCL listings
//! printed by `system`, tools/golden/listings.txt). The checks are the
//! ones that listing shows the compiler making before it creates a
//! program: a variable used but not declared (CPD0727, severity 40), a
//! command that does not exist (CPD0030, 30), a variable declared but not
//! used (CPD0726, 10), no labels (CPD0791, 00). Texts are as4tainr's own.

use std::collections::BTreeMap;

use crate::clpgm::{logical_lines, named, parse_statement};
use crate::report::{at, Stamp};

/// A message of the compiler, under source line `line` (0: none).
#[derive(Debug, Clone, PartialEq)]
pub struct Diag {
    pub line: usize,
    pub id: String,
    pub sev: u8,
    pub text: String,
}

/// A declared variable.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub name: String,
    pub line: usize,
    pub ty: String,
    pub len: String,
    /// Declared by DCLF (the file's fields).
    pub from_file: bool,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub diags: Vec<Diag>,
    pub vars: Vec<Declared>,
    /// References to each variable: the lines of the statements.
    pub refs: BTreeMap<String, Vec<usize>>,
    /// Labels: (name, line).
    pub labels: Vec<(String, usize)>,
    /// References to each label (GOTO CMDLBL...): statement lines.
    pub label_refs: BTreeMap<String, Vec<usize>>,
}

impl Analysis {
    pub fn max_sev(&self) -> u8 {
        self.diags.iter().map(|d| d.sev).max().unwrap_or(0)
    }
}

/// Statements that are part of the CL language rather than commands.
const STATEMENTS: &[&str] = &[
    "PGM",
    "ENDPGM",
    "IF",
    "ELSE",
    "DO",
    "ENDDO",
    "DOWHILE",
    "DOUNTIL",
    "DOFOR",
    "ITERATE",
    "LEAVE",
    "SELECT",
    "WHEN",
    "OTHERWISE",
    "ENDSELECT",
    "GOTO",
    "SUBR",
    "ENDSUBR",
    "RTNSUBR",
    "CALLSUBR",
    "CALLPRC",
    "RETURN",
    "INCLUDE",
    "DCLPRCOPT",
    "COPYRIGHT",
    "CLOSE",
    "RCVF",
    "SNDF",
    "SNDRCVF",
    "MONMSG",
    "CHGVAR",
    "DCL",
    "DCLF",
];

/// The variable names (`&NAME`) in a statement's text, outside quotes,
/// upper-cased, in order.
fn variables(text: &str) -> Vec<String> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_q = false;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            in_q = !in_q;
        } else if !in_q && c == '&' {
            let mut j = i + 1;
            while j < cs.len() && (cs[j].is_ascii_alphanumeric() || "@#$_".contains(cs[j])) {
                j += 1;
            }
            if j > i + 1 && !cs[i + 1].is_ascii_digit() {
                out.push(cs[i..j].iter().collect::<String>().to_ascii_uppercase());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// The variables a DCLF statement declares: (name, type, length).
pub type FileVars<'a> = dyn Fn(&crate::clpgm::Statement) -> Vec<(String, String, String)> + 'a;

/// Check a CL program's source as the compiler does for its listing.
/// `known`: whether a command exists (system or user-defined);
/// `file_vars`: the variables each DCLF declares, by DCLF line.
pub fn analyze(source: &str, known: &dyn Fn(&str) -> bool, file_vars: &FileVars) -> Analysis {
    let mut a = Analysis::default();
    let lines = match logical_lines(source) {
        Ok(l) => l,
        Err(e) => {
            a.diags.push(Diag {
                line: 0,
                id: String::new(),
                sev: 30,
                text: format!("{e:#}"),
            });
            return a;
        }
    };
    let mut uses: Vec<(usize, String)> = Vec::new();
    for (line, text) in &lines {
        let st = match parse_statement(*line, text) {
            Ok(s) => s,
            Err(e) => {
                a.diags.push(Diag {
                    line: *line,
                    id: crate::jobs::message_id(&format!("{e:#}")).to_string(),
                    sev: 30,
                    text: format!("{e:#}"),
                });
                continue;
            }
        };
        if let Some(l) = &st.label {
            a.labels.push((l.clone(), *line));
        }
        // The command as written, LIB/CMD included (the statement keeps
        // the bare name).
        let body = match &st.label {
            Some(_) => text.split_once(':').map(|x| x.1).unwrap_or(text),
            None => text.as_str(),
        };
        let written = body
            .trim_start()
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let name = if written.ends_with(&format!("/{}", st.name.to_ascii_uppercase())) {
            written
        } else {
            st.name.to_ascii_uppercase()
        };
        let bare = name.rsplit('/').next().unwrap_or(&name).to_string();
        let mut vars = variables(text);
        match bare.as_str() {
            "" => {}
            "DCL" => {
                if let Ok(p) = named(
                    &st,
                    &[
                        "VAR", "TYPE", "LEN", "VALUE", "STG", "BASPTR", "DEFVAR", "ADDRESS",
                    ],
                ) {
                    let var = p.get("VAR").map(|v| v.trim().to_ascii_uppercase());
                    if let Some(v) = &var {
                        let ty = p
                            .get("TYPE")
                            .map(|t| t.trim().to_ascii_uppercase())
                            .unwrap_or_default();
                        let len = p
                            .get("LEN")
                            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                            .unwrap_or_else(|| {
                                // The CL reference's defaults.
                                match ty.as_str() {
                                    "*CHAR" => "32".into(),
                                    "*DEC" => "15 5".into(),
                                    "*LGL" => "1".into(),
                                    "*INT" | "*UINT" => "4".into(),
                                    "*PTR" => "16".into(),
                                    _ => String::new(),
                                }
                            });
                        a.vars.push(Declared {
                            name: v.clone(),
                            line: *line,
                            ty,
                            len,
                            from_file: false,
                        });
                        // The declared name is not a reference.
                        if let Some(k) = vars.iter().position(|x| x == v) {
                            vars.remove(k);
                        }
                    }
                }
            }
            "DCLF" => {
                for (n, ty, len) in file_vars(&st) {
                    if !a.vars.iter().any(|d| d.name == n) {
                        a.vars.push(Declared {
                            name: n,
                            line: *line,
                            ty,
                            len,
                            from_file: true,
                        });
                    }
                }
            }
            n if STATEMENTS.contains(&n) || crate::cl::CL_PROGRAM_ONLY.contains(&n) || known(&name) => {
            }
            // A qualified command is looked for in its library.
            _ => a.diags.push(Diag {
                line: *line,
                id: "CPD0030".into(),
                sev: 30,
                text: format!(
                    "Command {bare} was not found in library {}.",
                    name.rsplit_once('/').map(|x| x.0).unwrap_or("*LIBL")
                ),
            }),
        }
        for v in vars {
            uses.push((*line, v));
        }
    }
    // Label references: a word of a statement (after its own label)
    // that names a label.
    let names: Vec<String> = a.labels.iter().map(|(l, _)| l.clone()).collect();
    for (line, text) in &lines {
        let body = match parse_statement(*line, text) {
            Ok(st) if st.label.is_some() => text.split_once(':').map(|x| x.1).unwrap_or(text),
            _ => text.as_str(),
        };
        let mut in_q = false;
        let mut word = String::new();
        for c in body.chars().chain(std::iter::once(' ')) {
            if c == '\'' {
                in_q = !in_q;
            }
            if !in_q && (c.is_ascii_alphanumeric() || "@#$_".contains(c)) {
                word.push(c.to_ascii_uppercase());
            } else {
                if names.contains(&word) {
                    a.label_refs.entry(word.clone()).or_default().push(*line);
                }
                word.clear();
            }
        }
    }
    let declared = |n: &str| a.vars.iter().any(|d| d.name == n);
    let mut undeclared: Vec<(usize, String)> = Vec::new();
    for (line, v) in &uses {
        if declared(v) {
            a.refs.entry(v.clone()).or_default().push(*line);
        } else if !undeclared.contains(&(*line, v.clone())) {
            undeclared.push((*line, v.clone()));
        }
    }
    for (line, v) in undeclared {
        a.diags.push(Diag {
            line,
            id: "CPD0727".into(),
            sev: 40,
            text: format!("Variable {v} is used but it is not declared."),
        });
    }
    a
}

/// What the listing's first page shows about the program and the
/// command that creates it.
pub struct Prologue {
    pub program: String,
    pub library: String,
    pub member: crate::source::Member,
    pub dftactgrp: String,
    pub actgrp: String,
    pub replace: String,
    pub text: String,
    pub dbgview: String,
    /// CRTCLMOD: a module.
    pub module: bool,
}

/// Lines on a listing page, the page header included (as the ILE RPG
/// listing breaks its pages on IBM i 7.5).
pub const PAGE: usize = 52;

const END_SOURCE: &str =
    "                               * * * * *   E N D   O F   S O U R C E   * * * * *";
const END_XREF: &str =
    "                     * * * * *   E N D   O F   C R O S S   R E F E R E N C E   * * * * *";
const END_SUMMARY: &str =
    "                               * * * * *   E N D   O F   M E S S A G E   S U M M A R Y   * * * * *";
const END_COMPILE: &str =
    "                                 * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *";

fn stamp_of(us: Option<i64>) -> Option<Stamp> {
    let ((y, m, d), (h, mi, s), _) = crate::rpg::dates::from_epoch_us(us?);
    Some(Stamp::of((y, m, d), (h, mi, s)))
}

/// One diagnostic as the listing prints it.
fn diag_line(d: &Diag) -> String {
    format!("* {:<7} {:02}  {}", d.id, d.sev, d.text)
}

/// The listing. `created`: whether the program was created.
pub fn listing(p: &Prologue, a: &Analysis, created: bool, stamp: &Stamp) -> Vec<String> {
    let label = |indent: usize, text: &str, value: &str| -> String {
        let l = crate::commands::dsp_label(text, 46 - indent);
        at(&[(indent, &l), (52 + indent - 2, value)])
    };
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (42, "Control Language"),
            (67, &format!("{}/{}", p.library, p.program)),
            (89, &crate::report::system_name()),
            (96, &stamp.date),
            (105, &stamp.time),
            (122, &format!("Page {page:>4}")),
        ])
    };
    let m = &p.member;
    let changed = stamp_of(m.changed_us)
        .map(|s| format!("{} {}", s.date, s.time))
        .unwrap_or_default();
    let what = if p.module { "Module" } else { "Program" };
    let mut first = vec![label(2, what, &p.program), label(4, "Library", &p.library)];
    if !m.stmf.is_empty() {
        first.push(label(2, "Source stream file", &m.stmf));
        // (IBM i 7.5 leaves three blanks after this label.)
        first.push(label(2, "Last Change  ", &changed));
    } else {
        first.push(label(2, "Source file", &m.file));
        first.push(label(4, "Library", &m.library));
        first.push(label(
            2,
            "Source member name",
            &format!("{}   {changed}", m.name),
        ));
    }
    let activation = if p.dftactgrp == "*YES" { "" } else { &p.actgrp };
    let items: Vec<(&str, &str)> = if p.module {
        vec![
            (
                "Source printing options",
                "*XREF  *GEN  *NOSECLVL  *NOEVENTF  *NODOSLTLVL",
            ),
            ("Module logging", "*JOB"),
            ("Allow RTVCLSRC command", "*YES"),
            ("Replace module object", &p.replace),
            ("Target release", "V7R5M0"),
            ("Authority", "*LIBCRTAUT"),
            ("Sort sequence", "*HEX"),
            ("Language identifier", "*JOBRUN"),
            ("Text", &p.text),
            ("Optimization", "*NONE"),
            ("Debugging view", &p.dbgview),
            ("Debug encryption key", "*NONE"),
            ("Enable performance collection", "*PEP"),
            ("Compiler", "as4tainr CL compiler"),
        ]
    } else {
        vec![
            (
                "Source printing options",
                "*XREF  *NOSECLVL  *NOEVENTF  *NODOSLTLVL",
            ),
            ("User profile", "*USER"),
            ("Program logging", "*JOB"),
            ("Allow RTVCLSRC command", "*YES"),
            ("Default activation group", &p.dftactgrp),
            ("Activation group", activation),
            ("Replace program", &p.replace),
            ("Target release", "V7R5M0"),
            ("Authority", "*LIBCRTAUT"),
            ("Sort sequence", "*HEX"),
            ("Language identifier", "*JOBRUN"),
            ("Text", &p.text),
            ("Optimization", "*NONE"),
            ("Debugging view", &p.dbgview),
            ("Debug encryption key", "*NONE"),
            ("Enable performance collection", "*PEP"),
            ("Storage model", "*SNGLVL"),
            ("Compiler", "as4tainr CL compiler"),
        ]
    };
    for (t, v) in items {
        first.push(label(2, t, v));
    }
    // The source, with each statement's messages after its last line.
    first.push(at(&[(39, "Control Language Source")]));
    let ruler = " SEQNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+.  DATE";
    first.push(ruler.into());
    let src: Vec<&str> = m.source.lines().collect();
    // The last line of each statement: the line before the next one
    // starts, back over lines without code.
    let starts: Vec<usize> = logical_lines(&m.source)
        .map(|v| v.iter().map(|(l, _)| *l).collect())
        .unwrap_or_default();
    let code_lines = code_lines(&m.source);
    let end_of = |line: usize| -> usize {
        let next = starts
            .iter()
            .copied()
            .find(|&s| s > line)
            .unwrap_or(src.len() + 1);
        (line..next)
            .rev()
            .find(|l| code_lines.contains(l))
            .unwrap_or(line)
    };
    let mut after: BTreeMap<usize, Vec<&Diag>> = BTreeMap::new();
    for d in a.diags.iter().filter(|d| d.line > 0) {
        after.entry(end_of(d.line)).or_default().push(d);
    }
    for (i, l) in src.iter().enumerate() {
        let n = i + 1;
        first.push(
            format!("{:>7}- {}", n * 100, l.trim_end())
                .trim_end()
                .to_string(),
        );
        for d in after.get(&n).into_iter().flatten() {
            first.push(diag_line(d));
        }
    }
    for d in a.diags.iter().filter(|d| d.line == 0) {
        first.push(diag_line(d));
    }
    first.push(END_SOURCE.into());

    // Cross reference.
    let mut xref = vec![
        at(&[(43, "Cross Reference")]),
        " Declared Variables".to_string(),
        " Name                        Defined     Type            Length      References".into(),
    ];
    let mut vars = a.vars.clone();
    vars.sort_by_cached_key(|v| crate::rpg::data::e(&v.name));
    let mut xdiags = Vec::new();
    for v in &vars {
        let len = match v.len.split_once(' ') {
            Some((l, d)) => format!("{l:>5} {d}"),
            None => format!("{:>5}", v.len),
        };
        let refs = a.refs.get(&v.name).cloned().unwrap_or_default();
        let mut row = at(&[
            (2, &v.name),
            (29, &format!("{:>7}", v.line * 100)),
            (42, &v.ty),
            (58, &len),
        ]);
        let mut chunks = refs.chunks(8);
        if let Some(c) = chunks.next() {
            row = format!("{row:<69}{}", refs_text(c));
        }
        xref.push(row.trim_end().to_string());
        for c in chunks {
            xref.push(format!("{:<69}{}", "", refs_text(c)).trim_end().to_string());
        }
        if refs.is_empty() && !v.from_file {
            let d = Diag {
                line: 0,
                id: "CPD0726".into(),
                sev: 10,
                text: format!("Variable {} is declared but not used.", v.name),
            };
            xref.push(diag_line(&d));
            xdiags.push(d);
        }
    }
    if a.labels.is_empty() {
        let d = Diag {
            line: 0,
            id: "CPD0791".into(),
            sev: 0,
            text: "The program has no labels.".into(),
        };
        xref.push(diag_line(&d));
        xdiags.push(d);
    } else {
        // Layout observed on IBM i 7.5 (CRTCLMOD, tools/golden/listings2.txt).
        xref.push(" Defined Labels".into());
        xref.push(" Label            Defined     References".into());
        let mut labels = a.labels.clone();
        labels.sort_by_cached_key(|(l, _)| crate::rpg::data::e(l));
        for (l, line) in labels {
            let refs: String = a
                .label_refs
                .get(&l)
                .map(|r| r.iter().map(|n| format!("{:>12}", n * 100)).collect())
                .unwrap_or_default();
            xref.push(
                format!(" {l:<16}{:>7}{refs}", line * 100)
                    .trim_end()
                    .to_string(),
            );
        }
    }
    xref.push(END_XREF.into());

    // Message summary.
    let all: Vec<&Diag> = a.diags.iter().chain(xdiags.iter()).collect();
    let max = all.iter().map(|d| d.sev).max().unwrap_or(0);
    let mut counts = [0usize; 10];
    for d in &all {
        counts[(d.sev / 10).min(9) as usize] += 1;
    }
    let mut summary = vec![
        at(&[(43, "Message Summary")]),
        at(&[(16, "Severity")]),
        " Total         0-9  10-19  20-29  30-39  40-49  50-59  60-69  70-79  80-89  90-99".into(),
    ];
    // No messages: the total one column to the left (observed).
    let mut row = if all.is_empty() {
        format!("{:>6}     ", 0)
    } else {
        format!("{:>7}    ", all.len())
    };
    for c in counts.iter() {
        row.push_str(&format!("{c:>7}"));
    }
    summary.push(row);
    let (pgm, lib) = (&p.program, &p.library);
    if created {
        summary.push(format!(
            " {what} {pgm} was created in library {lib} on {} at {}.",
            stamp.date, stamp.time
        ));
        summary.push(format!(
            " {what} {pgm} was created in library {lib}; highest error severity {max:02}."
        ));
    } else {
        summary.push(format!(
            " {what} {pgm} was not created in library {lib}; highest error severity {max:02}."
        ));
    }
    summary.push(END_SUMMARY.into());
    summary.push(END_COMPILE.into());

    let mut out = Vec::new();
    let mut page = 0;
    for section in [first, xref, summary] {
        for chunk in section.chunks(PAGE - 1) {
            page += 1;
            out.push(header(page));
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

/// References in columns of 7 from column 70.
fn refs_text(refs: &[usize]) -> String {
    refs.iter().map(|r| format!("{:>7}", r * 100)).collect()
}

/// The lines (1-based) that hold code, not only blanks and comments.
fn code_lines(source: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut in_comment = false;
    for (i, l) in source.lines().enumerate() {
        let cs: Vec<char> = l.chars().collect();
        let mut k = 0;
        let mut code = false;
        let mut in_q = false;
        while k < cs.len() {
            if in_comment {
                if cs[k] == '*' && cs.get(k + 1) == Some(&'/') {
                    in_comment = false;
                    k += 2;
                    continue;
                }
            } else if !in_q
                && cs[k] == '/'
                && cs.get(k + 1) == Some(&'*')
                && !crate::clpgm::qualifier_slash(&cs, k)
            {
                in_comment = true;
                k += 2;
                continue;
            } else {
                if cs[k] == '\'' {
                    in_q = !in_q;
                }
                if !cs[k].is_whitespace() {
                    code = true;
                }
            }
            k += 1;
        }
        if code {
            out.push(i + 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_outside_quotes() {
        assert_eq!(
            variables("CHGVAR VAR(&TEXT) VALUE('Hello &X' *CAT &NAME)"),
            ["&TEXT", "&NAME"]
        );
    }

    // Observed on IBM i 7.5 (tools/golden/listings.txt, l_cl_err): an
    // undeclared variable (40), a command that does not exist (30); a
    // variable declared and not used (10) and no labels (00) are in the
    // cross reference.
    #[test]
    fn checks() {
        let src = "PGM\n  DCL        VAR(&A) TYPE(*CHAR) LEN(5)\n  CHGVAR     VAR(&B) VALUE('X')\n  NOSUCHCMD  PARM(1)\nENDPGM\n";
        let a = analyze(src, &|c| c == "CHGVAR", &|_| vec![]);
        let ids: Vec<(usize, &str, u8)> = a
            .diags
            .iter()
            .map(|d| (d.line, d.id.as_str(), d.sev))
            .collect();
        assert_eq!(ids, [(4, "CPD0030", 30), (3, "CPD0727", 40)]);
        assert_eq!(a.max_sev(), 40);
        assert!(a.labels.is_empty());
    }

    /// A qualified command (LIB/CMD) is looked for in its library, and
    /// the message names that library.
    #[test]
    fn qualified_command_is_looked_for_in_its_library() {
        let src = "PGM\nAPPLIB/MYCMD\nOTHER/MYCMD\nENDPGM\n";
        let a = analyze(src, &|c| c == "APPLIB/MYCMD", &|_| vec![]);
        let d: Vec<&str> = a.diags.iter().filter(|d| d.id == "CPD0030").map(|d| d.text.as_str()).collect();
        assert_eq!(d, ["Command MYCMD was not found in library OTHER."]);
    }

}
