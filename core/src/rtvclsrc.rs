//! RTVCLSRC's output: the source of a CL program written again from the
//! program, in the form IBM i 7.5 gives it (PUB400, verify42 k42_rows:
//! RTVCLSRC of a program made from tools/golden/verify42/gld42c.clle,
//! printed with CPYF):
//!
//! - a comment box of 18 lines, 70 characters each: a title line with the
//!   product, "RTVCLSRC Output" and the date and time, then the program,
//!   its module, the original source file, library and member, the
//!   source's change date and time, the text, the owner and whether
//!   included source was retrieved, each value at column 48 and a
//!   two-letter tag before the closing `*/` (PN, PL, MN, ML, SN, SL, SM,
//!   SC, TX, OW, RI, ED);
//! - each statement on its own line, five blanks in, comments left out,
//!   continued lines joined, names and special values in upper case
//!   (quoted strings as written), every value with its keyword in the
//!   command's order (`DCL &X *CHAR 20` is `DCL VAR(&X) TYPE(*CHAR)
//!   LEN(20)`), a positional value's own parentheses taken off when the
//!   keyword's go round it (`CHGVAR &X ('a' *CAT &IN)` is `CHGVAR VAR(&X)
//!   VALUE('a' *CAT &IN)`).
//!
//! Not seen yet (docs/fidelity-gaps.md): how labels are written, how a
//! statement longer than the record is continued (here: `+` and the rest
//! on the next line, ten blanks in), nested commands (THEN, ELSE CMD,
//! MONMSG EXEC: written again the same way here).

use anyhow::Result;

/// The comment box's facts.
pub struct Header<'a> {
    pub program: &'a str,
    pub library: &'a str,
    /// Original source file, library, member (blank for a stream file).
    pub source: (&'a str, &'a str, &'a str),
    pub text: &'a str,
    pub owner: &'a str,
    /// "YY-MM-DD HH:MM:SS" as the job shows dates.
    pub when: &'a str,
}

/// The positional parameters of the CL program statements, in order (CL
/// reference: DCL's are VAR, TYPE, LEN and VALUE; STG, BASPTR and DEFVAR
/// are given by keyword).
fn statement_keywords(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "PGM" => &["PARM"],
        "DCL" => &["VAR", "TYPE", "LEN", "VALUE"],
        "DCLF" => &[
            "FILE",
            "RCDFMT",
            "OPNID",
            "ALWVARLEN",
            "ALWNULL",
            "ALWGRAPHIC",
            "DCLBINDEC",
        ],
        "DCLPRCOPT" => &[],
        "COPYRIGHT" => &["TEXT"],
        "INCLUDE" => &["SRCMBR", "SRCFILE", "SRCSTMF"],
        "CHGVAR" => &["VAR", "VALUE"],
        "IF" => &["COND", "THEN"],
        "ELSE" => &["CMD"],
        "DO" | "SELECT" | "ENDSELECT" | "ENDDO" | "ENDPGM" | "RETURN" => &[],
        "DOWHILE" | "DOUNTIL" => &["COND"],
        "DOFOR" => &["VAR", "FROM", "TO", "BY"],
        "WHEN" => &["COND", "THEN"],
        "OTHERWISE" => &["CMD"],
        "GOTO" => &["CMDLBL"],
        "ITERATE" | "LEAVE" => &["CMDLBL"],
        "MONMSG" => &["MSGID", "CMPDTA", "EXEC"],
        "SUBR" => &["SUBR"],
        "ENDSUBR" | "RTNSUBR" => &["RTNVAL"],
        "CALLSUBR" => &["SUBR", "RTNVAL"],
        "CALL" => &["PGM", "PARM"],
        "CALLPRC" => &["PRC", "PARM", "RTNVAL"],
        "SNDPGMMSG" => &["MSG"],
        "RCVF" | "SNDF" | "SNDRCVF" => &["DEV", "RCDFMT", "OPNID", "WAIT"],
        "CLOSE" => &["OPNID"],
        _ => return None,
    })
}

/// A command's keywords in order: a CL statement's, or the positional
/// parameters of a command as4tainr defines (the others by keyword only).
fn keywords(name: &str) -> Vec<String> {
    if let Some(k) = statement_keywords(name) {
        return k.iter().map(|s| s.to_string()).collect();
    }
    crate::cl::COMMANDS
        .iter()
        .find(|d| d.name == name)
        .map(|d| {
            d.params
                .iter()
                .filter(|p| p.positional)
                .map(|p| p.keyword.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Upper case and single blanks outside quoted strings.
fn canonical(v: &str) -> String {
    let mut out = String::new();
    let mut in_q = false;
    let mut blank = false;
    for c in v.trim().chars() {
        if in_q {
            out.push(c);
            if c == '\'' {
                in_q = false;
            }
            continue;
        }
        if c == '\'' {
            if blank {
                out.push(' ');
                blank = false;
            }
            in_q = true;
            out.push(c);
            continue;
        }
        if c.is_whitespace() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push(' ');
            blank = false;
        }
        out.extend(c.to_uppercase());
    }
    out
}

/// A positional value wholly in parentheses: what is inside.
fn unwrap_parens(v: &str) -> &str {
    let t = v.trim();
    if !(t.starts_with('(') && t.ends_with(')')) {
        return t;
    }
    // The first '(' must close at the end.
    let mut depth = 0;
    let mut in_q = false;
    for (i, c) in t.char_indices() {
        match c {
            '\'' => in_q = !in_q,
            '(' if !in_q => depth += 1,
            ')' if !in_q => {
                depth -= 1;
                if depth == 0 && i != t.len() - 1 {
                    return t;
                }
            }
            _ => {}
        }
    }
    &t[1..t.len() - 1]
}

/// One statement written again: `NAME KW(value) ...`.
fn statement(st: &crate::clpgm::Statement) -> Result<String> {
    let order = keywords(&st.name);
    let mut used: Vec<String> = Vec::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    for p in &st.params {
        let (kw, value) = match &p.keyword {
            Some(k) => (k.to_ascii_uppercase(), p.value.trim().to_string()),
            None => {
                let k = order
                    .iter()
                    .find(|k| !used.contains(k))
                    .cloned()
                    .unwrap_or_default();
                (k, unwrap_parens(&p.value).to_string())
            }
        };
        used.push(kw.clone());
        let value = if matches!(kw.as_str(), "THEN" | "CMD" | "EXEC") && !value.is_empty() {
            let inner = crate::clpgm::parse_statement(st.line, &value)?;
            statement(&inner)?
        } else {
            canonical(&value)
        };
        parts.push((kw, value));
    }
    let mut out = st.name.clone();
    for (kw, v) in parts {
        out.push(' ');
        if kw.is_empty() {
            out.push_str(&v);
        } else {
            out.push_str(&format!("{kw}({v})"));
        }
    }
    Ok(out)
}

/// The member's data width (its record length less SRCSEQ and SRCDAT)
/// decides where long statements are continued.
/// Data columns of QCLSRC's 92-byte records (less SRCSEQ and SRCDAT).
pub const WIDTH: usize = 80;

/// A long line as several: `+` at the end of each but the last, the
/// rest ten blanks in.
fn wrap(line: String, width: usize) -> Vec<String> {
    if line.chars().count() <= width {
        return vec![line];
    }
    let mut out = Vec::new();
    let mut rest: Vec<char> = line.chars().collect();
    let mut first = true;
    loop {
        let indent = if first { 0 } else { 10 };
        let room = width - indent;
        if rest.len() <= room {
            out.push(format!(
                "{}{}",
                " ".repeat(indent),
                rest.iter().collect::<String>()
            ));
            break;
        }
        // Break at a blank before the room left for " +".
        let limit = room - 2;
        let at = (1..=limit).rev().find(|&i| rest[i] == ' ').unwrap_or(limit);
        let head: String = rest[..at].iter().collect();
        out.push(format!("{}{} +", " ".repeat(indent), head.trim_end()));
        rest = rest[at..]
            .iter()
            .skip_while(|c| **c == ' ')
            .cloned()
            .collect();
        first = false;
    }
    out
}

/// The member's lines: the comment box, then the statements.
pub fn retrieve(source: &str, h: &Header, width: usize) -> Result<Vec<String>> {
    let boxed = |s: String| format!("/*{s:<66}*/");
    let field =
        |label: &str, value: &str, tag: &str| boxed(format!(" {label:<41}:   {value:<18}{tag}"));
    let mut out = vec![
        format!("/*{}*/", "*".repeat(66)),
        boxed(String::new()),
        boxed(format!(
            " {} {}     RTVCLSRC Output     {}",
            crate::report::PRODUCT,
            crate::report::LEVEL,
            h.when
        )),
        boxed(String::new()),
        field("Program name . . . . . . . . . . . . . .", h.program, "PN"),
        field("Library name . . . . . . . . . . . . . .", h.library, "PL"),
        field("Module name  . . . . . . . . . . . . . .", h.program, "MN"),
        field("Library name . . . . . . . . . . . . . .", "*LIBL", "ML"),
        field("Original source file . . . . . . . . . .", h.source.0, "SN"),
        field("Library name . . . . . . . . . . . . . .", h.source.1, "SL"),
        field("Original source member . . . . . . . . .", h.source.2, "SM"),
        boxed(" Source file change".into()),
        field("  date/time  . . . . . . . . . . . . . .", "", "SC"),
        boxed(format!(" Text . . . :   {:<48}TX", h.text)),
        field("Owner  . . . . . . . . . . . . . . . . .", h.owner, "OW"),
        field("Retrieve included source . . . . . . . .", "*NO", "RI"),
        boxed(format!("{:<64}ED", "")),
        format!("/*{}*/", "*".repeat(66)),
    ];
    for (line, text) in crate::clpgm::logical_lines(source)? {
        let st = crate::clpgm::parse_statement(line, &text)?;
        if st.name.is_empty() && st.label.is_none() {
            continue;
        }
        let body = statement(&st)?;
        let body = match &st.label {
            Some(l) if st.name.is_empty() => format!("{l}:"),
            Some(l) => format!("{l}: {body}"),
            None => body,
        };
        out.extend(wrap(format!("     {body}"), width.max(40)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // PUB400 (IBM i 7.5), verify42 k42_rows: RTVCLSRC of a program made
    // from tools/golden/verify42/gld42c.clle (IBM's product line and
    // level swapped for ours, same columns).
    #[test]
    fn source_as_rtvclsrc_writes_it() {
        let src = include_str!("../../tools/golden/verify42/gld42c.clle");
        let lines = retrieve(
            src,
            &Header {
                program: "GLD42C",
                library: "AS4TRAINR1",
                source: ("", "", ""),
                text: "",
                owner: "AS4TRAINR",
                when: "26-11-04 00:22:29",
            },
            WIDTH,
        )
        .unwrap();
        let want = [
            "/********************************************************************/",
            "/*                                                                  */",
            "/* AS4T001 V0R1M0 261021     RTVCLSRC Output     26-11-04 00:22:29  */",
            "/*                                                                  */",
            "/* Program name . . . . . . . . . . . . . . :   GLD42C            PN*/",
            "/* Library name . . . . . . . . . . . . . . :   AS4TRAINR1        PL*/",
            "/* Module name  . . . . . . . . . . . . . . :   GLD42C            MN*/",
            "/* Library name . . . . . . . . . . . . . . :   *LIBL             ML*/",
            "/* Original source file . . . . . . . . . . :                     SN*/",
            "/* Library name . . . . . . . . . . . . . . :                     SL*/",
            "/* Original source member . . . . . . . . . :                     SM*/",
            "/* Source file change                                               */",
            "/*   date/time  . . . . . . . . . . . . . . :                     SC*/",
            "/* Text . . . :                                                   TX*/",
            "/* Owner  . . . . . . . . . . . . . . . . . :   AS4TRAINR         OW*/",
            "/* Retrieve included source . . . . . . . . :   *NO               RI*/",
            "/*                                                                ED*/",
            "/********************************************************************/",
            "     PGM PARM(&IN)",
            "     DCL VAR(&IN) TYPE(*CHAR) LEN(10)",
            "     DCL VAR(&X) TYPE(*CHAR) LEN(20)",
            "     CHGVAR VAR(&X) VALUE('a' *CAT &IN)",
            "     IF COND(&X *EQ ' ') THEN(DO)",
            "     SNDPGMMSG MSG('blank')",
            "     ENDDO",
            "     ENDPGM",
        ];
        assert_eq!(lines, want);
    }

    #[test]
    fn long_statements_continue() {
        let l = format!("     CALL PGM(X) PARM({})", "'a' ".repeat(30));
        let w = wrap(l, WIDTH);
        assert!(w.len() > 1);
        assert!(w.iter().all(|x| x.chars().count() <= WIDTH));
        assert!(w[0].ends_with(" +"));
    }
}
