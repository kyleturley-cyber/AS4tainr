//! Printed reports: the listing a display command makes when its output
//! goes to a printer file (OUTPUT(*PRINT), a batch job, the PASE `system`
//! utility). Each command lays its report out in fixed columns on a
//! 132-column page.
//!
//! Layouts come from real output (PUB400, IBM i 7.5, captured with
//! `system` on 2026-10-21; target/ibmi/golden, not kept in the
//! repository). Two page header styles were seen:
//!
//! - style A (DSPLIBL, DSPDTAARA, DSPFD, DSPMSGD): one line, product and
//!   level at column 2, the title, date and time at column 97, "Page" at
//!   column 122 with the number right-aligned to column 130;
//! - style B (DSPSYSVAL, DSPOBJD): the title with "Page" at column 121 on
//!   the first line, then product and level at column 1, the system name
//!   at column 99, date at 109 and time with the time zone at 119.
//!
//! The product line names as4tainr, not IBM's licensed program.

/// Product identifier and release on the first header line: 14
/// characters like IBM's ("5770SS1 V7R5M0"), so the level that follows it
/// keeps its column.
pub const PRODUCT: &str = "AS4T001 V0R1M0";
/// Level (IBM prints the release's build date, YYMMDD).
pub const LEVEL: &str = "261021";

/// A line with each text starting at a 1-based column (texts in column
/// order; a text that runs into the next one's column pushes it right by
/// one blank, as nothing on a real report overlaps). Trailing blanks are
/// removed.
pub fn at(fields: &[(usize, &str)]) -> String {
    let mut s = String::new();
    for (col, text) in fields {
        let want = col.saturating_sub(1);
        let have = s.chars().count();
        if have < want {
            s.push_str(&" ".repeat(want - have));
        } else if have > 0 {
            s.push(' ');
        }
        s.push_str(text);
    }
    s.trim_end().to_string()
}

/// The date and time a report shows: the job's date format and
/// separators (system values QDATFMT, QDATSEP, QTIMSEP), from the clock.
#[derive(Debug, Clone, Default)]
pub struct Stamp {
    pub date: String,
    pub time: String,
    /// Time zone abbreviation (from QTIMZON), shown by style B headers.
    pub zone: String,
}

impl Stamp {
    pub fn now() -> Stamp {
        let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::now();
        Stamp::of((y, mo, d), (h, mi, s))
    }

    pub fn of((y, mo, d): (i32, u32, u32), (h, mi, s): (u32, u32, u32)) -> Stamp {
        let sv = |n: &str| {
            crate::clpgm::system_value_text(n)
                .and_then(|v| v.ok())
                .unwrap_or_default()
        };
        let dsep = sv("QDATSEP");
        let tsep = sv("QTIMSEP");
        let yy = format!("{:02}", y.rem_euclid(100));
        let mm = format!("{mo:02}");
        let dd = format!("{d:02}");
        let parts = match sv("QDATFMT").as_str() {
            "YMD" => [yy, mm, dd],
            "DMY" => [dd, mm, yy],
            // JUL is not used for reports here; MDY is shipped.
            _ => [mm, dd, yy],
        };
        let zone = sv("QTIMZON");
        Stamp {
            date: parts.join(&dsep),
            time: format!("{h:02}{tsep}{mi:02}{tsep}{s:02}"),
            // Q0000UTC → UTC: the zone's abbreviation follows the offset.
            zone: zone
                .trim_start_matches(|c: char| c == 'Q' || c.is_ascii_digit())
                .to_string(),
        }
    }
}

/// Style A page header: `title` starts at `title_col`; `subject` (an
/// object name, or "") at `subject_col`.
pub fn header_a(
    level_col: usize,
    title_col: usize,
    title: &str,
    subject_col: usize,
    subject: &str,
    stamp: &Stamp,
    page: u32,
) -> String {
    let when = format!("{} {}", stamp.date, stamp.time);
    let mut f: Vec<(usize, &str)> = vec![(2, PRODUCT), (level_col, LEVEL), (title_col, title)];
    if !subject.is_empty() {
        f.push((subject_col, subject));
    }
    f.push((97, &when));
    let page = format!("Page {page:>4}");
    f.push((122, &page));
    at(&f)
}

/// Style B page header (two lines): `title` starts at `title_col`.
pub fn header_b(
    title_col: usize,
    title: &str,
    system: &str,
    stamp: &Stamp,
    page: u32,
) -> [String; 2] {
    let page = format!("Page {page:>5}");
    let time = format!("{} {}", stamp.time, stamp.zone);
    [
        at(&[(title_col, title), (121, &page)]),
        at(&[
            (1, PRODUCT),
            (17, LEVEL),
            (99, system),
            (109, &stamp.date),
            (119, time.trim_end()),
        ]),
    ]
}

/// The system name (AS4TAINR_SYSTEM_NAME, as the 5250 sign-on screen
/// shows it).
pub fn system_name() -> String {
    std::env::var("AS4TAINR_SYSTEM_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "AS4TAINR".into())
        .to_ascii_uppercase()
}

/// End-of-listing line of style A reports (DSPLIBL, DSPDTAARA).
pub const END_A: &str = "                          * * * * *  E N D  O F  L I S T I N G  * * * * *";
/// End-of-listing line of DSPSYSVAL.
pub const END_SYSVAL: &str =
    "                                         * * * * *   E N D  O F  L I S T I N G   * * * * *";
/// End-of-listing line of DSPOBJD.
pub const END_OBJD: &str =
    "                                        * * * * *   E N D   O F   L I S T I N G   * * * * *";

/// The printer file a display command prints its report with, as its
/// spooled file's name (PUB400 2026-10-27, tools/golden/verify9.txt
/// d9_prt_rows, `system -k` of each command): QSYSPRT for the others.
pub fn printer_file(command: &str) -> &'static str {
    match command.to_ascii_uppercase().as_str() {
        "DSPOBJD" => "QPRTOBJD",
        "DSPFD" => "QPDSPFD",
        "DSPFFD" => "QPDSPFFD",
        "DSPDBR" => "QPDSPDBR",
        "DSPLIBL" => "QPRTLIBL",
        "DSPMSGD" => "QPMSGD",
        "DSPJOB" => "QPDSPJOB",
        "DSPLIB" => "QPDSPLIB",
        "DSPUSRPRF" => "QPUSRPRF",
        _ => "QSYSPRT",
    }
}

#[cfg(test)]
mod tests {
    // PUB400 2026-10-27, verify9 d9_prt_rows: the spooled files `system
    // -k` kept for each command, newest first.
    #[test]
    fn printer_files() {
        for (c, f) in [
            ("DSPOBJD", "QPRTOBJD"),
            ("DSPFD", "QPDSPFD"),
            ("DSPFFD", "QPDSPFFD"),
            ("DSPDBR", "QPDSPDBR"),
            ("DSPLIBL", "QPRTLIBL"),
            ("DSPSYSVAL", "QSYSPRT"),
            ("DSPMSGD", "QPMSGD"),
            ("DSPJOB", "QPDSPJOB"),
            ("DSPLIB", "QPDSPLIB"),
            ("CPYF", "QSYSPRT"),
            ("DSPUSRPRF", "QPUSRPRF"),
        ] {
            assert_eq!(super::printer_file(c), f, "{c}");
        }
    }

    use super::*;

    // Expected lines: PUB400 (IBM i 7.5) output of `system "DSPLIBL"`,
    // `system "DSPSYSVAL SYSVAL(QCCSID)"` and `system "DSPOBJD
    // OBJ(QSYS/QCMD) OBJTYPE(*PGM)"`, 2026-10-21, with IBM's product line
    // swapped for ours (same columns) and PUB400's own footer left out.
    fn stamp() -> Stamp {
        Stamp {
            date: "26-10-21".into(),
            time: "23:43:37".into(),
            zone: "UTC".into(),
        }
    }

    #[test]
    fn style_a_header_columns() {
        let h = header_a(18, 44, "Library List", 0, "", &stamp(), 1);
        let ibm = " 5770SS1 V7R5M0  220415                    Library List                                         26-10-21 23:43:37        Page    1";
        assert_eq!(h.len(), ibm.len());
        assert_eq!(&h[23..], &ibm[23..]);
        assert_eq!(&h[1..15], PRODUCT);
        assert_eq!(&h[17..23], LEVEL);
        let h = header_a(
            17,
            51,
            "Display Data Area",
            73,
            "AS4TRAINR1/GOLDDA",
            &stamp(),
            1,
        );
        let ibm = " 5770SS1 V7R5M0 220415                            Display Data Area     AS4TRAINR1/GOLDDA       26-10-21 23:49:39        Page    1";
        assert_eq!(&h[22..96], &ibm[22..96]);
    }

    #[test]
    fn style_b_header_columns() {
        let [a, b] = header_b(52, "System Values", "PUB400", &stamp(), 1);
        assert_eq!(
            a,
            "                                                   System Values                                                        Page     1"
        );
        let ibm = "5770SS1 V7R5M0  220415                                                                            PUB400    26-10-21  23:43:37 UTC";
        assert_eq!(&b[22..], &ibm[22..]);
        assert_eq!(&b[..14], PRODUCT);
    }

    #[test]
    fn date_in_the_system_date_format() {
        // Shipped QDATFMT MDY, QDATSEP '/', QTIMSEP ':' (IBM i system
        // value documentation).
        let s = Stamp::of((2026, 10, 21), (23, 43, 37));
        assert_eq!(s.date, "10/21/26");
        assert_eq!(s.time, "23:43:37");
        assert_eq!(s.zone, "UTC");
    }
}
