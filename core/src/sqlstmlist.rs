//! RUNSQLSTM's listing (OUTPUT(*PRINT), the default): the command's
//! options, the source with its record numbers, and a message for each
//! statement with its severity and record, then a message summary.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-26, tools/golden/
//! verify2.txt w_script and w_rb). The messages' texts are as4tainr's
//! own; a message has IBM's identifier where a capture shows which one
//! that kind of statement gets (`Msg::id`), and none otherwise.

use crate::report::Stamp;

/// A statement's message in the listing.
#[derive(Debug, Clone, Default)]
pub struct Msg {
    /// SQLxxxx, or empty when the identifier is not known.
    pub id: String,
    pub severity: u32,
    /// The record (line) and position the statement starts at.
    pub record: usize,
    pub position: usize,
    pub text: String,
}

/// What the listing shows besides the messages.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// The name in the page header: the member, or the stream file's name
    /// without its extension (gldtmp.sql → GLDTMP).
    pub name: String,
    /// ("Source stream file", path) or ("Source file", LIB/FILE) and
    /// ("Member", name).
    pub source: Vec<(String, String)>,
    pub commit: String,
    pub naming: String,
    pub errlvl: u32,
    pub dftrdbcol: String,
    pub source_ccsid: String,
    pub job_ccsid: String,
    /// When the source was last changed.
    pub changed: Option<Stamp>,
    pub lines: Vec<String>,
}

/// Lines on a page, the header included.
const PAGE: usize = 60;

/// The listing's lines.
pub fn listing(input: &Input, msgs: &[Msg], stamp: &Stamp) -> Vec<String> {
    let header = |page: usize| {
        // The first page's number has two blanks fewer than the others'
        // (as captured).
        let number = if page == 1 {
            format!("PAGE {page:>3}")
        } else {
            format!("PAGE {page:>5}")
        };
        crate::report::at(&[
            // The product line names as4tainr (report.rs), in IBM's
            // columns: product and release, one blank, the level.
            (
                1,
                &format!("{} {}", crate::report::PRODUCT, crate::report::LEVEL),
            ),
            (37, "Run SQL Statements"),
            (68, &input.name),
            (103, &format!("{} {}", stamp.date, stamp.time)),
            (123, &number),
        ])
    };
    let opt = |k: &str, v: &str| {
        let mut s = k.to_string();
        while s.len() < 26 {
            s.push('.');
        }
        s.push_str(v);
        s
    };
    let mut first: Vec<String> = input.source.iter().map(|(k, v)| opt(k, v)).collect();
    first.extend([
        opt("Target release", "V7R5M0"),
        opt("Commit", &input.commit),
        opt("Naming", &input.naming),
        opt("Generation level", &input.errlvl.to_string()),
        opt("Date format", "*JOB"),
        opt("Date separator", "*JOB"),
        opt("Time format", "*HMS"),
        opt("Time separator ", "*JOB"),
        opt("Right margin", "80"),
        opt("Default collection", &input.dftrdbcol),
        opt("IBM SQL flagging", "*NOFLAG"),
        opt("ANS flagging", "*NONE"),
        opt("Decimal point", "*JOB"),
        opt("Sort sequence", "*JOB"),
        opt("Language ID", "*JOB"),
        opt("Printer file", "*LIBL/QSYSPRT"),
        opt("Source file CCSID", &input.source_ccsid),
        opt("Job CCSID", &input.job_ccsid),
        opt("Statement processing", "*RUN"),
        opt("Allow copy of data", "*OPTIMIZE"),
        opt("Allow blocking", "*ALLREAD"),
        opt("SQL rules", "*DB2"),
        "Decimal result options:".to_string(),
        opt("  Maximum precision", "31"),
        opt("  Maximum scale", "31"),
        opt("  Minimum divide scale", "0"),
        "Concurrent access".to_string(),
        opt("  resolution", "*DFT"),
        opt("System time sensitive", "*YES"),
    ]);
    if let Some(c) = &input.changed {
        first.push(format!("Source member changed on {}  {}", c.date, c.time));
    }
    let ruler = "Record  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8   SEQNBR  Last change";
    // The source: 80 characters a line, a longer record continued on the
    // next line with the same record number.
    let mut source = Vec::new();
    for (i, l) in input.lines.iter().enumerate() {
        let cs: Vec<char> = l.trim_end_matches('\r').trim_end().chars().collect();
        if cs.is_empty() {
            source.push(format!("{:>7}", i + 1));
            continue;
        }
        for part in cs.chunks(80) {
            let t: String = part.iter().collect();
            source.push(format!("{:>7} {t}", i + 1));
        }
    }
    source.push(format!(
        "{:33}* * * * *  E N D  O F  S O U R C E  * * * * *",
        ""
    ));
    // The messages: the text from column 22, wrapped at word boundaries.
    let mut messages = vec!["MSG ID  SEV  RECORD  TEXT".to_string()];
    for m in msgs {
        let text = if m.record == 0 {
            m.text.clone()
        } else {
            format!("Position {} {}", m.position, m.text)
        };
        let mut rows: Vec<String> = Vec::new();
        let mut cur = String::new();
        for w in text.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > 53 {
                rows.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        if !cur.is_empty() {
            rows.push(cur);
        }
        for (k, r) in rows.iter().enumerate() {
            if k == 0 {
                // A message of no statement (the commit at the end) has
                // no record (verify3 x_cmt).
                let record = if m.record == 0 {
                    String::new()
                } else {
                    m.record.to_string()
                };
                messages.push(format!("{:<7} {:>3} {:>7}  {r}", m.id, m.severity, record));
            } else {
                messages.push(format!("{:21}{r}", ""));
            }
        }
    }
    let count = |lo: u32, hi: u32| {
        msgs.iter()
            .filter(|m| m.severity >= lo && m.severity <= hi)
            .count()
    };
    messages.push(format!("{:20}Message Summary", ""));
    messages.push("  Total     Info   Warning      Error   Severe  Terminal".into());
    messages.push(format!(
        "{:>7}{:>9}{:>10}{:>11}{:>9}{:>10}",
        msgs.len(),
        count(0, 9),
        count(10, 19),
        count(20, 29),
        count(30, 39),
        count(40, 99)
    ));
    // Also with no errors: "00 level ..." (verify3 x_ok).
    let worst = msgs.iter().map(|m| m.severity).max().unwrap_or(0);
    messages.push(format!("{worst:02} level severity errors found in source"));
    messages.push(format!(
        "{:32}* * * * *  E N D  O F  L I S T I N G  * * * * *",
        ""
    ));
    let mut out = Vec::new();
    let mut page = 0;
    for (section, ruled) in [(first, false), (source, true), (messages, true)] {
        let room = if ruled { PAGE - 2 } else { PAGE - 1 };
        for chunk in section.chunks(room) {
            page += 1;
            out.push(header(page));
            if ruled {
                out.push(ruler.to_string());
            }
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected lines: PUB400 (IBM i 7.5) RUNSQLSTM listings, 2026-10-26,
    // tools/golden/verify2.txt w_rb and w_script.
    #[test]
    fn listing_layout() {
        let stamp = Stamp {
            date: "26-10-26".into(),
            time: "23:45:35".into(),
            zone: String::new(),
        };
        let input = Input {
            name: "GLDRB".into(),
            source: vec![(
                "Source stream file".into(),
                "/home/AS4TRAINR/gld/gldrb.sql".into(),
            )],
            commit: "*NONE".into(),
            naming: "*SYS".into(),
            errlvl: 10,
            dftrdbcol: "*NONE".into(),
            source_ccsid: "273".into(),
            job_ccsid: "273".into(),
            changed: None,
            lines: vec!["rollback;".into(), format!("{}ext", "x".repeat(80))],
        };
        let msgs = vec![Msg {
            id: "SQL7007".into(),
            severity: 30,
            record: 3,
            position: 1,
            text: "x".into(),
        }];
        let l = listing(&input, &msgs, &stamp);
        // IBM's columns; the product line is as4tainr's (report.rs).
        assert_eq!(
            l[0],
            "AS4T001 V0R1M0 261021               Run SQL Statements             GLDRB                              26-10-26 23:45:35   PAGE   1"
        );
        assert_eq!(
            l[1],
            "Source stream file......../home/AS4TRAINR/gld/gldrb.sql"
        );
        assert_eq!(l[2], "Target release............V7R5M0");
        assert!(l.contains(&"Time separator ...........*JOB".to_string()));
        assert!(l.contains(&"  Minimum divide scale....0".to_string()));
        assert!(l.contains(&"  resolution..............*DFT".to_string()));
        let p2 = l.iter().position(|x| x.ends_with("PAGE     2")).unwrap();
        assert!(l[p2].starts_with("AS4T001 V0R1M0 261021               Run SQL Statements"));
        assert_eq!(l[p2 + 2], "      1 rollback;");
        // A record over 80 characters goes on with the same number
        // (w_script record 17).
        assert_eq!(l[p2 + 4], "      2 ext");
        assert_eq!(
            l[p2 + 5],
            "                                 * * * * *  E N D  O F  S O U R C E  * * * * *"
        );
        assert!(l[p2 + 6].ends_with("PAGE     3"));
        assert_eq!(l[p2 + 8], "MSG ID  SEV  RECORD  TEXT");
        assert!(l[p2 + 9].starts_with("SQL7007  30       3  Position 1 "));
        assert_eq!(l[p2 + 10], "                    Message Summary");
        assert_eq!(
            l[p2 + 11],
            "  Total     Info   Warning      Error   Severe  Terminal"
        );
        assert_eq!(
            l[p2 + 12],
            "      1        0         0          0        1         0"
        );
        assert_eq!(l[p2 + 13], "30 level severity errors found in source");
        assert_eq!(
            l[p2 + 14],
            "                                * * * * *  E N D  O F  L I S T I N G  * * * * *"
        );
    }

    // A message text longer than the column goes on under it, from column
    // 22, after at most 53 characters (w_script: SQL7905 record 4 fills
    // 53, SQL7988 record 15 wraps at 58).
    #[test]
    fn long_message_texts_wrap() {
        let stamp = Stamp {
            date: "26-10-26".into(),
            time: "23:45:27".into(),
            zone: String::new(),
        };
        let msgs = vec![Msg {
            id: "SQL7988".into(),
            severity: 0,
            record: 15,
            position: 1,
            text: "Table GLDTMP in AS4TRAINR1 was altered as asked.".into(),
        }];
        let l = listing(&Input::default(), &msgs, &stamp);
        let i = l.iter().position(|x| x.starts_with("SQL7988")).unwrap();
        assert_eq!(
            l[i],
            "SQL7988   0      15  Position 1 Table GLDTMP in AS4TRAINR1 was altered as"
        );
        assert_eq!(l[i + 1], "                     asked.");
    }

    // A script without errors still has the severity line, "00 level";
    // the commit at the end of a script under commitment control has no
    // record and no position (PUB400 2026-10-27, tools/golden/verify3.txt
    // x_ok and x_cmt).
    #[test]
    fn no_errors_and_the_final_commit() {
        let stamp = Stamp::default();
        let msgs = vec![Msg {
            id: "SQL7960".into(),
            severity: 0,
            record: 0,
            position: 0,
            text: "Done.".into(),
        }];
        let l = listing(&Input::default(), &msgs, &stamp);
        assert!(
            l.contains(&"SQL7960   0          Done.".to_string()),
            "{l:#?}"
        );
        assert!(l.contains(&"00 level severity errors found in source".to_string()));
    }
}
