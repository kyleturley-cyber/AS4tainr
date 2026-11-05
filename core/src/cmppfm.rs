//! CMPPFM: compare two source members line by line and print what
//! differs.
//!
//! The report is laid out as IBM i 7.5 prints it (PUB400,
//! tools/golden/verify44.txt m44_cmppfm and m44_cmpsame): page 1 the
//! command's values, then the listing section (every line with I and D
//! marks, the type and length of each run, the new and old line
//! numbers; left out when the members are the same), the summary and
//! statistics, and a page explaining the abbreviations (written here in
//! as4tainr's own words). Lines are compared in their source data
//! columns (1 to the record length less 12). Matching lines are found as
//! the longest common subsequence; in a run of changes the inserted lines
//! are listed before the deleted ones and pair with them one for one
//! (paired changes), as in the capture. Not here (docs/fidelity-gaps.md):
//! CMPTYPE *WORD and *FILE, reformatted and moved lines (RFM, IMV...),
//! data members, statement files, RPTTYPE *DETAIL's own form (it is
//! listed as *CHANGE). Messages: SPC6001 (no differences), SPC6002
//! (differences), SPC5010 (the compare could not be done).

use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use crate::report::at;
use anyhow::{bail, Result};

/// One line of the comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// The same in both: (new number, old number).
    Match(usize, usize),
    /// Only in the new member.
    Insert(usize),
    /// Only in the old member.
    Delete(usize),
}

/// The lines of `new` and `old` compared: matches, then in each run of
/// changes the insertions before the deletions.
pub fn compare(new: &[String], old: &[String]) -> Vec<Line> {
    let (n, m) = (new.len(), old.len());
    // lcs[i][j]: the longest common subsequence of new[i..] and old[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if new[i] == old[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut ins = Vec::new();
    let mut del = Vec::new();
    let flush = |out: &mut Vec<Line>, ins: &mut Vec<usize>, del: &mut Vec<usize>| {
        out.extend(ins.drain(..).map(Line::Insert));
        out.extend(del.drain(..).map(Line::Delete));
    };
    while i < n || j < m {
        if i < n && j < m && new[i] == old[j] {
            flush(&mut out, &mut ins, &mut del);
            out.push(Line::Match(i + 1, j + 1));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            del.push(j + 1);
            j += 1;
        } else {
            ins.push(i + 1);
            i += 1;
        }
    }
    flush(&mut out, &mut ins, &mut del);
    out
}

/// The summary's numbers.
#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub matches: usize,
    pub paired: usize,
    pub inserted: usize,
    pub deleted: usize,
}

pub fn stats(lines: &[Line]) -> Stats {
    let mut s = Stats::default();
    let (mut ins, mut del) = (0, 0);
    let end_run = |s: &mut Stats, ins: &mut usize, del: &mut usize| {
        s.paired += (*ins).min(*del);
        s.inserted += *ins;
        s.deleted += *del;
        *ins = 0;
        *del = 0;
    };
    for l in lines {
        match l {
            Line::Match(..) => {
                end_run(&mut s, &mut ins, &mut del);
                s.matches += 1;
            }
            Line::Insert(_) => ins += 1,
            Line::Delete(_) => del += 1,
        }
    }
    end_run(&mut s, &mut ins, &mut del);
    s
}

/// What the report shows about the command and its members.
pub struct Request {
    pub new: (String, String, String),
    pub old: (String, String, String),
    pub rpttype: String,
    pub output: String,
    pub width: usize,
}

fn header(stamp: &crate::report::Stamp, page: usize) -> String {
    let hhmm: String = stamp.time.chars().take(5).collect();
    let n = page.to_string();
    at(&[
        (7, crate::report::PRODUCT),
        (27, crate::report::LEVEL),
        (71, &stamp.date),
        (83, &hhmm),
        (104, "PAGE"),
        (121 - n.len(), &n),
    ])
}

/// `label.....:` with the colon in column `colon` (from column `start`).
fn dotted(start: usize, label: &str, colon: usize) -> String {
    let mut s = label.to_string();
    while start + s.chars().count() < colon {
        s.push('.');
    }
    s.push(':');
    s
}

fn names(r: &Request) -> String {
    let q = |(l, f, m): &(String, String, String)| format!("{l}/{f}({m})");
    at(&[
        (1, "NEW:"),
        (11, &q(&r.new)),
        (65, "OLD:"),
        (75, &q(&r.old)),
    ])
}

/// Lines of the listing section on one page of the report (IBM i's page
/// length was not captured: 60 lines here, with the headings).
const LISTING_LINES: usize = 54;

pub fn report(
    r: &Request,
    new: &[String],
    old: &[String],
    lines: &[Line],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    let mut out = vec![header(stamp, 1)];
    let v = |label: &str, value: &str| at(&[(1, &dotted(1, label, 50)), (53, value)]);
    let lib = |value: &str| at(&[(3, &dotted(3, "Library", 50)), (55, value)]);
    let cmptype = "*LINE";
    out.extend([
        v("New file", &r.new.1),
        lib(&r.new.0),
        v("New member", &r.new.2),
        v("Old file", &r.old.1),
        lib(&r.old.0),
        v("Old member", &r.old.2),
        v("Compare type", cmptype),
        v("Report type", &r.rpttype),
        v("Output", &r.output),
        v("File to receive output", "QSYSPRT"),
        lib("*LIBL"),
        v("Select source type", "*ALL"),
        v("Process option", ""),
        v("Statement file", ""),
        lib(""),
        v("Statement member", ""),
    ]);
    let mut page = 1;
    let s = stats(lines);
    let differ = s.inserted + s.deleted > 0;
    if differ && r.rpttype != "*SUMMARY" {
        let only_changes = r.rpttype == "*DIFF";
        let shown: Vec<&Line> = lines
            .iter()
            .filter(|l| !only_changes || !matches!(l, Line::Match(..)))
            .collect();
        // Each run's type and length on its first line.
        let mut runs: Vec<Option<(&str, usize)>> = vec![None; shown.len()];
        let mut k = 0;
        while k < shown.len() {
            let kind = |l: &Line| match l {
                Line::Match(..) => 0,
                Line::Insert(_) => 1,
                Line::Delete(_) => 2,
            };
            let t = kind(shown[k]);
            let mut e = k;
            while e < shown.len() && kind(shown[e]) == t {
                e += 1;
            }
            runs[k] = Some((["MAT=", "INS=", "DEL="][t], e - k));
            k = e;
        }
        for (chunk_no, chunk) in shown.chunks(LISTING_LINES).enumerate() {
            page += 1;
            out.push(header(stamp, page));
            out.push(names(r));
            out.push(at(&[(23, "LISTING OUTPUT SECTION (LINE COMPARE)")]));
            out.push(at(&[(95, "N-LN"), (103, "O-LN")]));
            out.push(at(&[
                (1, "ID"),
                (9, "SOURCE LINES"),
                (85, "TYPE"),
                (91, "LEN NUM"),
                (103, "NUM"),
            ]));
            out.push(at(&[(
                4,
                "----+----1----+----2----+----3----+----4----+----5----+----6----+----7----+----8",
            )]));
            for (x, l) in chunk.iter().enumerate() {
                let idx = chunk_no * LISTING_LINES + x;
                let (id, text, nn, on) = match l {
                    Line::Match(a, b) => ("", &new[a - 1], Some(*a), Some(*b)),
                    Line::Insert(a) => ("I -", &new[a - 1], Some(*a), None),
                    Line::Delete(b) => ("D -", &old[b - 1], None, Some(*b)),
                };
                let text: String = text.chars().take(80).collect();
                let mut line = format!("{id:<3}{text:<81}");
                if let Some((t, len)) = runs[idx] {
                    line.push_str(&format!("{t}{len:>5}"));
                }
                let mut s = line;
                let mut put = |col: usize, v: &str| {
                    while s.chars().count() < col - 1 {
                        s.push(' ');
                    }
                    s.push_str(v);
                };
                if let Some(n) = nn {
                    put(95, &format!("{n:07}"));
                }
                if let Some(n) = on {
                    put(103, &format!("{n:07}"));
                }
                out.push(s.trim_end().to_string());
            }
        }
    }
    page += 1;
    out.push(header(stamp, page));
    out.push(names(r));
    out.push(at(&[(22, "LINE COMPARE SUMMARY AND STATISTICS")]));
    let count = |label: &str, n: usize| {
        let n = n.to_string();
        at(&[(1, &dotted(1, label, 70)), (81 - n.len(), &n)])
    };
    let nonpaired_ins = s.inserted - s.paired;
    let nonpaired_del = s.deleted - s.paired;
    out.extend([
        count("NUMBER OF LINE MATCHES", s.matches),
        count(
            "TOTAL CHANGES (PAIRED+NONPAIRED CHNG)",
            s.paired + nonpaired_ins + nonpaired_del,
        ),
        count("REFORMATTED LINES", 0),
        count("PAIRED CHANGES (REFM+PAIRED INS/DEL)", s.paired),
        count("NEW FILE LINE INSERTIONS", s.inserted),
        count("NON-PAIRED INSERTS", nonpaired_ins),
        count("OLD FILE LINE DELETIONS", s.deleted),
        count("NON-PAIRED DELETIONS", nonpaired_del),
        count("NEW FILE LINES PROCESSED", new.len()),
        count("OLD FILE LINES PROCESSED", old.len()),
    ]);
    let cols = format!("1:{}", r.width);
    let longest = format!("LONGEST LINE = {}", r.width);
    out.push(at(&[
        (
            1,
            &format!("LISTING-TYPE = {}", r.rpttype.trim_start_matches('*')),
        ),
        (25, "SOURCE COMPARE COLUMNS ="),
        (59 - cols.len(), &cols),
        (64, &longest),
    ]));
    out.push("PROCESSING OPTIONS SPECIFIED : NONE".into());
    page += 1;
    out.push(header(stamp, page));
    out.push(names(r));
    out.push(at(&[(22, "ABBREVIATIONS DESCRIPTION")]));
    // The codes as IBM i prints them; what they mean in as4tainr's words.
    for l in [
        "   ID SECTION",
        "I    - In the new member only",
        "D    - In the old member only",
        "RN   - Changed only in its spacing, as in the new member",
        "RO   - Changed only in its spacing, as in the old member",
        "IM   - Moved: where it is in the new member",
        "DM   - Moved: where it was in the old member",
        "   TYPE OF DIFFERENCES",
        "MAT= Lines the same in both members",
        "RFM= Lines changed only in their spacing",
        "RPL= Lines replaced by others",
        "INS= Lines only the new member has.",
        "DEL= Lines only the old member has.",
        "IMR= Lines whose spacing the new member changed.",
        "DMR= Lines whose spacing the old member had.",
        "IMV= Lines the new member has somewhere else.",
        "DMV= Lines the old member had somewhere else.",
        "   COLUMN HEADINGS",
        "N-LN - Numbers of the new member's lines.",
        "O-LN - Numbers of the old member's lines.",
        "NUM  - Line number",
        "INS  - Inserted",
        "DEL  - Deleted",
        "PROC - Processed",
        "CHNG - Changed",
        "REFM - Changed only in spacing",
    ] {
        out.push(l.to_string());
    }
    out.push(
        "                       * * * * * * *   E N D   O F   L I S T I N G    * * * * * * *"
            .into(),
    );
    out
}

impl Job<'_> {
    async fn compared_member(&self, spec: &str, mbr: &str) -> Result<crate::source::Member> {
        let (libs, file) = self.search(spec);
        crate::source::find_member(self.client, &libs, &file, mbr)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "SPC5010: The members were not compared: {}",
                    crate::jobs::message_text(&e.to_string())
                )
            })
    }

    pub(crate) async fn cmppfm_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        if cmd.def.name != "CMPPFM" {
            return Ok(None);
        }
        let rpttype = cmd.get("RPTTYPE").to_ascii_uppercase();
        if !matches!(
            rpttype.as_str(),
            "*DIFF" | "*SUMMARY" | "*CHANGE" | "*DETAIL"
        ) {
            // PUB400 verify44 m44_cmplin: CPD0084 then CPF0006.
            return Err(crate::cl::diag_error(
                "CPD0084",
                &format!("RPTTYPE cannot be {rpttype}: use *DIFF, *SUMMARY, *CHANGE or *DETAIL."),
            ));
        }
        let cmptype = cmd.get("CMPTYPE").to_ascii_uppercase();
        if cmptype != "*LINE" {
            bail!("CMPPFM CMPTYPE({cmptype}) is not available here: use CMPTYPE(*LINE).");
        }
        let member = |file: &str, mbr: &str| (file.to_ascii_uppercase(), mbr.to_ascii_uppercase());
        let (newfile, newmbr) = member(cmd.get("NEWFILE"), cmd.get("NEWMBR"));
        let oldfile = match cmd.get("OLDFILE") {
            "*NEWFILE" => newfile.clone(),
            f => f.to_ascii_uppercase(),
        };
        let oldmbr = match cmd.get("OLDMBR") {
            "*NEWMBR" => newmbr.clone(),
            m => m.to_ascii_uppercase(),
        };
        let new = self.compared_member(&newfile, &newmbr).await?;
        let old = self.compared_member(&oldfile, &oldmbr).await?;
        let width = crate::source::record_length(self.client, &new.library, &new.file)
            .await
            .unwrap_or(92)
            .saturating_sub(12)
            .max(1) as usize;
        let cut = |src: &str| -> Vec<String> {
            src.lines()
                .map(|l| {
                    l.chars()
                        .take(width)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };
        let (nl, ol) = (cut(&new.source), cut(&old.source));
        let lines = compare(&nl, &ol);
        let s = stats(&lines);
        let msg = if s.inserted + s.deleted == 0 {
            "SPC6001: CMPPFM is done: the members are the same."
        } else {
            "SPC6002: CMPPFM is done: the members differ."
        };
        let lib = |l: &str| crate::qtemp::logical(l);
        let req = Request {
            new: (lib(&new.library), new.file.clone(), new.name.clone()),
            old: (lib(&old.library), old.file.clone(), old.name.clone()),
            rpttype: rpttype.clone(),
            output: cmd.get("OUTPUT").to_ascii_uppercase(),
            width,
        };
        let print = report(&req, &nl, &ol, &lines, &crate::report::Stamp::now());
        Ok(Some(Outcome::Display(ListDisplay {
            title: "Compare Physical File Member".into(),
            rows: print.clone(),
            print,
            print_msg: Some(msg.into()),
            ..Default::default()
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.trim_end().to_string()).collect()
    }

    // PUB400 (IBM i 7.5), verify44 m44_cmppfm: GLD44B (new) against
    // GLD44C (old).
    const NEW: &str = "/* c */\n PGM\n DCL y\n CALL\n CHGVAR\n ENDPGM\n";
    const OLD: &str = "/* c */\n PGM\n DCLF\n DCL x\n CALL\n ENDPGM\n";

    #[test]
    fn line_compare_as_captured() {
        let (n, o) = (lines(NEW), lines(OLD));
        let l = compare(&n, &o);
        use Line::*;
        assert_eq!(
            l,
            [
                Match(1, 1),
                Match(2, 2),
                Insert(3),
                Delete(3),
                Delete(4),
                Match(4, 5),
                Insert(5),
                Match(6, 6)
            ]
        );
        // The summary's numbers in the capture: 4 matches, 1 paired, 2
        // inserted, 2 deleted (3 changes, 1 non-paired of each).
        assert_eq!(
            stats(&l),
            Stats {
                matches: 4,
                paired: 1,
                inserted: 2,
                deleted: 2
            }
        );
    }

    #[test]
    fn report_columns_as_captured() {
        let (n, o) = (lines(NEW), lines(OLD));
        let l = compare(&n, &o);
        let req = Request {
            new: ("AS4TRAINR1".into(), "GLD44S".into(), "GLD44B".into()),
            old: ("AS4TRAINR1".into(), "GLD44S".into(), "GLD44C".into()),
            rpttype: "*CHANGE".into(),
            output: "*PRINT".into(),
            width: 80,
        };
        let r = report(&req, &n, &o, &l, &crate::report::Stamp::default());
        let has = |want: &str| assert!(r.iter().any(|x| x == want), "{want:?} not in {r:#?}");
        // Lines of the capture whose source text is not the member's.
        has("New file.........................................:  GLD44S");
        has("  Library........................................:    AS4TRAINR1");
        has("Report type......................................:  *CHANGE");
        has("Statement member.................................:");
        has("NEW:      AS4TRAINR1/GLD44S(GLD44B)                             OLD:      AS4TRAINR1/GLD44S(GLD44C)");
        has("                                                                                              N-LN    O-LN");
        has("ID      SOURCE LINES                                                                TYPE  LEN NUM     NUM");
        has("NUMBER OF LINE MATCHES...............................................:         4");
        has("TOTAL CHANGES (PAIRED+NONPAIRED CHNG)................................:         3");
        has("NON-PAIRED DELETIONS.................................................:         1");
        has("LISTING-TYPE = CHANGE   SOURCE COMPARE COLUMNS =      1:80     LONGEST LINE = 80");
        // The listing's columns: ID, source from column 4, type and
        // length from 85, line numbers at 95 and 103.
        let row = |id: &str, text: &str, tail: &str| format!("{id:<3}{text:<81}{tail}");
        has(&row("", "/* c */", "MAT=    2 0000001 0000001"));
        has(&row("", " PGM", "          0000002 0000002"));
        has(&row("I -", " DCL y", "INS=    1 0000003"));
        has(&row("D -", " DCLF", "DEL=    2         0000003"));
        has(&row("D -", " DCL x", "                  0000004"));
    }
}
