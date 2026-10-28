//! The printed job log (printer file QPJOBLOG): DSPJOBLOG OUTPUT(*PRINT).
//!
//! The layout is IBM i 7.5's, measured on output captured on PUB400
//! (2026-10-28, tools/golden/verify17.txt x17_own and x17_joblog,
//! tools/golden/verify13.txt t13_joblog, verify12.txt n12_keys; the
//! captures are not kept in the repository):
//!
//! - each page starts with four lines: the report header (product and
//!   level at column 2, "Display Job Log" at 47, the system name at 85,
//!   date at 95, time and time zone at 105, "Page" at 122 with the number
//!   right-aligned to column 130), the job's name, user and number, its
//!   job description, and the column headings;
//! - a page has 56 lines; an entry that does not fit goes on on the next
//!   page, after its header;
//! - each message is one line (message ID or *NONE at column 1, type at
//!   12, severity at 36 (blank for a message without an ID), date at 41,
//!   time with microseconds at 51, the sending program, its library and
//!   instruction at 68, 81 and 93, the receiving one's at 102, 114, 126),
//!   then its details at column 38: the sending user (when it is not the
//!   job's user), the sending ILE module, procedure and statement, the
//!   receiving ones, and the message text ("Message . . . . :", text at
//!   column 58, continued at column 40, lines up to column 115), and its
//!   second-level text when it has one (from column 38, its format
//!   controls left out: its "Cause" and "Recovery" parts run on).
//!
//! The texts of the lines are as4tainr's (a layout, labels as the report
//! shows them).

use crate::commands::dsp_label;
use crate::report::{at, Stamp};

/// Lines per page (header included).
pub const PAGE_LINES: usize = 56;
/// The last column of a message text line.
const TEXT_END: usize = 115;

/// A program on either end of a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct End {
    pub pgm: String,
    pub lib: String,
    /// The instruction: `*STMT` for an ILE procedure (its statement is in
    /// the details), a number for a system program, `*N` when not known.
    pub inst: String,
    /// ILE module, procedure and statement (empty for programs that are
    /// not ILE).
    pub module: String,
    pub proc: String,
    pub stmt: String,
}

/// One message in the job log.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Blank for an impromptu message (`*NONE`).
    pub msgid: String,
    /// *INFO, *COMP, *DIAG, *ESCAPE, *NOTIFY, *INQ, *RPY, *RQS.
    pub ty: String,
    pub sev: i32,
    /// Date as the job shows it, and the time with microseconds.
    pub date: String,
    pub time: String,
    /// The user who sent it; blank or the job's user: not shown.
    pub user: String,
    pub from: End,
    pub to: End,
    pub text: String,
    pub seclvl: String,
}

/// The job the log is of.
#[derive(Debug, Clone, Default)]
pub struct Job {
    pub name: String,
    pub user: String,
    pub number: String,
    pub jobd: String,
    pub jobd_lib: String,
}

/// The word a message type has in the TYPE column (IBM i job log, from
/// the message type names of the message handling documentation).
pub fn type_word(ty: &str) -> &'static str {
    match ty {
        "*COMP" => "Completion",
        "*DIAG" => "Diagnostic",
        "*ESCAPE" => "Escape",
        "*NOTIFY" => "Notify",
        "*INQ" => "Inquiry",
        "*RPY" => "Reply",
        "*RQS" => "Request",
        _ => "Information",
    }
}

/// Which job log: printed by DSPJOBLOG OUTPUT(*PRINT) ("Display Job
/// Log", at column 47: x17_joblog), or when the job ends ("Job Log", at
/// column 50: PUB400 2026-10-27, tools/golden/verify8.txt c8_joblog7,
/// a batch job's QPJOBLOG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Title {
    Display,
    Ended,
}

impl Title {
    fn at(self) -> (usize, &'static str) {
        match self {
            Title::Display => (47, "Display Job Log"),
            Title::Ended => (50, "Job Log"),
        }
    }
}

/// The page header: four lines.
fn header(job: &Job, title: Title, stamp: &Stamp, system: &str, page: usize) -> [String; 4] {
    let title = title.at();
    let level = format!("{} {}", crate::report::PRODUCT, crate::report::LEVEL);
    let time = format!("{} {}", stamp.time, stamp.zone);
    let page = format!("Page {page:>4}");
    [
        at(&[
            (2, &level),
            title,
            (85, system),
            (95, &stamp.date),
            (105, time.trim_end()),
            (122, &page),
        ]),
        at(&[
            (3, &dsp_label("Job name", 27)),
            (36, &job.name),
            (52, &dsp_label("User", 16)),
            (74, &job.user),
            (87, &dsp_label("Number", 27)),
            (120, &job.number),
        ]),
        at(&[
            (3, &dsp_label("Job description", 27)),
            (36, &job.jobd),
            (52, &dsp_label("Library", 16)),
            (74, &job.jobd_lib),
        ]),
        at(&[
            (1, "MSGID"),
            (12, "TYPE"),
            (36, "SEV"),
            (41, "DATE"),
            (51, "TIME"),
            (68, "FROM PGM"),
            (81, "LIBRARY"),
            (93, "INST"),
            (102, "TO PGM"),
            (114, "LIBRARY"),
            (126, "INST"),
        ]),
    ]
}

/// A text from column `first` (1-based), continued at column 40, each
/// line ending at column 115 at the latest; words are not split, and the
/// blanks between them are kept (two after a sentence, as written).
pub fn wrap(lead: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = lead.to_string();
    let mut fresh = true;
    let mut rest = text.trim();
    while !rest.is_empty() {
        let gap = rest.len() - rest.trim_start_matches(' ').len();
        let after = &rest[gap..];
        let wlen = after.find(' ').unwrap_or(after.len());
        let word = &after[..wlen];
        let used = line.chars().count();
        let gap_n = if fresh { 0 } else { gap };
        if !fresh && used + gap_n + word.chars().count() > TEXT_END {
            out.push(std::mem::take(&mut line));
            line = " ".repeat(39);
            line.push_str(word);
        } else {
            line.push_str(&" ".repeat(gap_n));
            line.push_str(word);
        }
        fresh = false;
        rest = &after[wlen..];
    }
    if !fresh {
        out.push(line);
    }
    out
}

/// The lines of one entry.
pub fn entry_lines(e: &Entry, job_user: &str) -> Vec<String> {
    let id = if e.msgid.is_empty() {
        "*NONE"
    } else {
        &e.msgid
    };
    let sev = if e.msgid.is_empty() {
        String::new()
    } else {
        format!("{:02}", e.sev)
    };
    let mut out = vec![at(&[
        (1, id),
        (12, type_word(&e.ty)),
        (36, &sev),
        (41, &e.date),
        (51, &e.time),
        (68, &e.from.pgm),
        (81, &e.from.lib),
        (93, &e.from.inst),
        (102, &e.to.pgm),
        (114, &e.to.lib),
        (126, &e.to.inst),
    ])];
    let detail = |label: &str, v: &str| at(&[(38, &dsp_label(label, 26)), (70, v)]);
    if !e.user.is_empty() && e.user != job_user {
        out.push(detail("From user", &e.user));
    }
    for (end, side) in [(&e.from, "From"), (&e.to, "To")] {
        if !end.module.is_empty() {
            out.push(detail(&format!("{side} module"), &end.module));
            out.push(detail(&format!("{side} procedure"), &end.proc));
            out.push(detail("Statement", &end.stmt));
        }
    }
    let lead = |label: &str| format!("{}{}   ", " ".repeat(37), dsp_label(label, 14));
    if e.ty == "*RQS" {
        // A request: the command after a minus sign, one column left
        // (c8_joblog7: "Message . . . . :  -CALL PGM(...)").
        let lead = format!("{}{}  ", " ".repeat(37), dsp_label("Message", 14));
        out.extend(wrap(&lead, &format!("-{}", e.text.trim())));
    } else {
        out.extend(wrap(&lead("Message"), &e.text));
    }
    let seclvl = plain_seclvl(&e.seclvl);
    if !seclvl.is_empty() {
        out.extend(wrap(&" ".repeat(37), &seclvl));
    }
    out
}

/// A second-level text as the job log prints it: from column 38, its
/// format controls (&N, &P, &B and the blank after each) left out, so
/// its parts ("Cause . . . . . :", "Recovery  . . . :", as the message
/// description writes them) run on (x17_joblog: CPF9801's).
pub fn plain_seclvl(s: &str) -> String {
    let mut t = s.to_string();
    for c in ["&N ", "&P ", "&B "] {
        t = t.replace(c, "");
    }
    for c in ["&N", "&P", "&B"] {
        t = t.replace(c, "");
    }
    t.trim().to_string()
}

/// The printed job log: pages of [`PAGE_LINES`] lines, each with its
/// header.
pub fn print(
    job: &Job,
    title: Title,
    entries: &[Entry],
    stamp: &Stamp,
    system: &str,
) -> Vec<String> {
    let body: Vec<String> = entries
        .iter()
        .flat_map(|e| entry_lines(e, &job.user))
        .collect();
    let per_page = PAGE_LINES - 4;
    let mut out = Vec::new();
    let pages = body.len().div_ceil(per_page).max(1);
    for p in 0..pages {
        out.extend(header(job, title, stamp, system, p + 1));
        out.extend(body.iter().skip(p * per_page).take(per_page).cloned());
    }
    out
}

/// A time of day as the job log shows it: hours, minutes and seconds
/// with the time separator (QTIMSEP), then the microseconds after the
/// decimal point (QDECFMT: a comma with J, else a period).
pub fn time_text(hhmmss: &str, usec: &str) -> String {
    let sv = |n: &str| {
        crate::clpgm::system_value_text(n)
            .and_then(|v| v.ok())
            .unwrap_or_default()
    };
    let tsep = sv("QTIMSEP");
    let point = if sv("QDECFMT").trim() == "J" {
        ","
    } else {
        "."
    };
    let h = |r: std::ops::Range<usize>| hhmmss.get(r).unwrap_or("00").to_string();
    let us = if usec.is_empty() { "000000" } else { usec };
    format!("{}{tsep}{}{tsep}{}{point}{us}", h(0..2), h(2..4), h(4..6))
}

/// A CYYMMDD date as the job shows it (QDATFMT, QDATSEP).
pub fn date_text(cyymmdd: &str) -> String {
    let n = |r: std::ops::Range<usize>| -> u32 {
        cyymmdd.get(r).and_then(|s| s.parse().ok()).unwrap_or(0)
    };
    let century = if cyymmdd.starts_with('1') { 2000 } else { 1900 };
    let y = century + n(1..3) as i32;
    Stamp::of((y, n(3..5), n(5..7)), (0, 0, 0)).date
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(l: &str) -> Vec<(usize, String)> {
        // Each run of non-blank text (single blanks inside) with its
        // 1-based column.
        let mut v = Vec::new();
        let b = l.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b' ' {
                i += 1;
                continue;
            }
            let s = i;
            while i < b.len() && !(b[i] == b' ' && (i + 1 >= b.len() || b[i + 1] == b' ')) {
                i += 1;
            }
            v.push((s + 1, l[s..i].to_string()));
        }
        v
    }

    fn stamp() -> Stamp {
        Stamp {
            date: "26-10-28".into(),
            time: "15:39:05".into(),
            zone: "UTC".into(),
        }
    }

    fn job() -> Job {
        Job {
            name: "QP0ZSPWT".into(),
            user: "QUSER".into(),
            number: "736302".into(),
            jobd: "QDFTSVR".into(),
            jobd_lib: "QGPL".into(),
        }
    }

    fn cl(pgm: &str, stmt: &str) -> End {
        End {
            pgm: pgm.into(),
            lib: "AS4TRAINR1".into(),
            inst: "*STMT".into(),
            module: pgm.into(),
            proc: pgm.into(),
            stmt: stmt.into(),
        }
    }

    // The columns of the header and of an entry, as on PUB400
    // (2026-10-28, tools/golden/verify17.txt x17_joblog: lines 1-4, the
    // impromptu message GLD17A sent itself at statement 700).
    #[test]
    fn header_and_entry_columns() {
        let e = Entry {
            ty: "*INFO".into(),
            date: "26-10-28".into(),
            time: "15:39:05,787930".into(),
            user: "AS4TRAINR".into(),
            from: cl("GLD17A", "700"),
            to: cl("GLD17A", "700"),
            text: "GLD17 info to self".into(),
            ..Default::default()
        };
        let p = print(&job(), Title::Display, &[e], &stamp(), "PUB400");
        let c: Vec<Vec<usize>> = p
            .iter()
            .map(|l| cols(l).iter().map(|x| x.0).collect())
            .collect();
        assert_eq!(c[0], vec![2, 47, 85, 95, 105, 122, 130]);
        assert_eq!(c[1], vec![3, 36, 52, 58, 74, 87, 120]);
        assert_eq!(c[2], vec![3, 20, 36, 52, 74]);
        assert_eq!(c[3], vec![1, 12, 36, 41, 51, 68, 81, 93, 102, 114, 126]);
        assert_eq!(c[4], vec![1, 12, 41, 51, 68, 81, 93, 102, 114, 126]);
        assert_eq!(p[5].trim(), "From user . . . . . . . . . :   AS4TRAINR");
        assert_eq!(c[5], vec![38, 70]);
        assert_eq!(p[6].trim(), "From module . . . . . . . . :   GLD17A");
        assert_eq!(p[7].trim(), "From procedure  . . . . . . :   GLD17A");
        assert_eq!(p[8].trim(), "Statement . . . . . . . . . :   700");
        assert_eq!(p[9].trim(), "To module . . . . . . . . . :   GLD17A");
        assert_eq!(p[10].trim(), "To procedure  . . . . . . . :   GLD17A");
        assert_eq!(p[12].trim(), "Message . . . . :   GLD17 info to self");
        assert_eq!(c[12], vec![38, 58]);
        assert_eq!(p.len(), 13);
    }

    // A message from a system program to an ILE one has no From module
    // lines; one to the caller of the top program (QCMDEXC) no To lines;
    // a sending user that is the job's is not shown (x17_joblog: CPF9801
    // from QLICKOBJ, CPF9898 to QCMDEXC; CPF1124 has no From user line).
    #[test]
    fn system_programs_have_no_module_lines() {
        let qsys = |pgm: &str, inst: &str| End {
            pgm: pgm.into(),
            lib: "QSYS".into(),
            inst: inst.into(),
            ..Default::default()
        };
        let a = Entry {
            msgid: "CPF9801".into(),
            ty: "*ESCAPE".into(),
            sev: 40,
            user: "AS4TRAINR".into(),
            from: qsys("QLICKOBJ", "025B"),
            to: cl("GLD17A", "1100"),
            text: "x".into(),
            ..Default::default()
        };
        let l = entry_lines(&a, "QUSER");
        assert_eq!(l.len(), 6);
        assert!(l[0].starts_with("CPF9801    Escape                  40"));
        assert!(l[2].trim().starts_with("To module"));
        let b = Entry {
            msgid: "CPF1124".into(),
            user: "QUSER".into(),
            from: qsys("QWTPIIPP", "04DC"),
            to: End {
                pgm: "*EXT".into(),
                inst: "*N".into(),
                ..Default::default()
            },
            text: "y".into(),
            ..Default::default()
        };
        let l = entry_lines(&b, "QUSER");
        assert_eq!(l.len(), 2);
        let c: Vec<usize> = cols(&l[0]).iter().map(|x| x.0).collect();
        assert_eq!(c, vec![1, 12, 36, 68, 81, 93, 102, 126]);
    }

    // Texts continue at column 40 and end by column 115; the blanks
    // after a sentence stay (x17_joblog's CPF9801 second-level text:
    // a line of 112 columns not continued with a two-blank gap and a
    // two-letter word). Second-level texts lose their format controls.
    #[test]
    fn message_text_wraps_at_column_115() {
        let lead = format!("{}{}   ", " ".repeat(37), dsp_label("Message", 14));
        let five = ["abcdefghij"; 5].join(" ");
        // 57 + 55 = 112 columns; "If" after two blanks would end at 116.
        let l = wrap(&lead, &format!("{five}.  If more"));
        assert_eq!(l[0].len(), 112);
        assert_eq!(l[1], format!("{}If more", " ".repeat(39)));
        // After one blank it ends at 115: on the line.
        let l = wrap(&lead, &format!("{five}. If more"));
        assert_eq!(l[0].len(), 115);
        assert_eq!(l[1], format!("{}more", " ".repeat(39)));
        assert_eq!(
            plain_seclvl("&N Cause . . . . . :   Why. &N Recovery  . . . :   Do."),
            "Cause . . . . . :   Why. Recovery  . . . :   Do."
        );
    }

    // 56 lines a page, the header on each (x17_joblog: the entry of
    // "GLD17 last" goes on on page 2 after its four header lines).
    #[test]
    fn pages_of_56_lines() {
        let e = Entry {
            ty: "*INFO".into(),
            from: cl("GLD17A", "1900"),
            to: cl("GLD17A", "1900"),
            user: "AS4TRAINR".into(),
            text: "t".into(),
            ..Default::default()
        };
        let p = print(&job(), Title::Display, &vec![e; 6], &stamp(), "PUB400");
        assert_eq!(p.len(), 4 + 9 * 6 + 4);
        assert!(p[56].contains("Display Job Log") && p[56].ends_with("Page    2"));
        assert!(p[59].starts_with("MSGID"));
        assert!(p[60].trim().starts_with("Statement"));
        assert!(p[61].trim().starts_with("Message"));
    }

    // A job log printed when the job ends has the title "Job Log" at
    // column 50; a request is its command after a minus sign at column
    // 57 (c8_joblog7, a batch job's QPJOBLOG).
    #[test]
    fn ended_title_and_requests() {
        let e = Entry {
            ty: "*RQS".into(),
            date: "26-10-27".into(),
            time: "18:10:39,930449".into(),
            from: End {
                pgm: "QWTSCSBJ".into(),
                inst: "*N".into(),
                ..Default::default()
            },
            to: End {
                pgm: "*EXT".into(),
                inst: "*N".into(),
                ..Default::default()
            },
            text: "CALL PGM(AS4TRAINR1/GLD6SPL)".into(),
            ..Default::default()
        };
        let p = print(&job(), Title::Ended, &[e], &stamp(), "PUB400");
        assert_eq!(cols(&p[0])[1], (50, "Job Log".to_string()));
        assert!(p[4].starts_with("*NONE      Request                      "));
        assert_eq!(
            cols(&p[4]).iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![1, 12, 41, 51, 68, 93, 102, 126]
        );
        assert_eq!(
            p[5],
            format!(
                "{}Message . . . . :  -CALL PGM(AS4TRAINR1/GLD6SPL)",
                " ".repeat(37)
            )
        );
    }
}
