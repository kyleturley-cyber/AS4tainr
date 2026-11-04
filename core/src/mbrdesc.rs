//! DSPFD TYPE(*MBR) printed: the Member Description of a physical
//! (data or source) or logical file in IBM i 7.5's layout (PUB400,
//! tools/golden/verify40.txt i40_mbr1/i40_mbr2: an SQL table keyed by its
//! primary key; tools/golden/verify45.txt n45_mbra: an SQL table in
//! arrival sequence, n45_mbru: named primary key and unique constraints
//! over two key fields, n45_mbrp: a keyed DDS physical file with a
//! deleted record, n45_mbrl: a DDS logical file, n45_mbrs: a source file
//! with two members).
//!
//! Labels end with the colon at column 48; a parameter keyword starts at
//! column 50 and its value at column 61; record counts end at column 70,
//! access path sizes at 75, operation counters at 78. Operation counters
//! as4tainr does not keep (opens, writes, logical reads...) print blank,
//! as IBM i prints a zero counter (docs/fidelity-gaps.md, "DSPFD
//! TYPE(*MBR)").
//!
//! Pages: each member's last part (from "Implicit access path sharing"
//! on) is kept on one page; a page holds 60 lines (no capture had more
//! than 57, and the parts that went to the next page would have made 64
//! or more).

use crate::fdout::FileInfo;

/// One member of the file.
pub struct Member {
    pub name: String,
    pub text: String,
    /// (century, YYMMDD, HHMMSS).
    pub created: (String, String, String),
    pub changed: (String, String, String),
    pub records: i64,
    pub deleted: i64,
    /// A source member's type (None for a data member).
    pub srctype: Option<String>,
}

/// What the report needs beyond the outfiles' FileInfo.
pub struct MemberExtra {
    /// The number of distinct values of each leading part of the
    /// access path's key (the file's keys, or its primary key's columns).
    pub partial_keys: Vec<i64>,
    /// Key constraints (primary key, unique) by the names IBM i shows.
    pub constraints: Vec<String>,
    pub members: Vec<Member>,
}

/// A label with IBM's leader dots: the text, then dots on odd columns
/// (0-based) up to 45 and the colon at 47. A text too long for a blank
/// before its first dot takes the dot right after it ("Records rejected
/// by group-by selection. :", i40_mbr1).
pub fn label(indent: usize, text: &str) -> String {
    let mut s = format!("{}{text}", " ".repeat(indent));
    let len = s.chars().count();
    if len >= 45 {
        s.push('.');
    } else {
        let first = if (len + 1) % 2 == 1 { len + 1 } else { len + 2 };
        while s.chars().count() < first {
            s.push(' ');
        }
        while s.chars().count() <= 45 {
            if s.chars().count() % 2 == 1 {
                s.push('.');
            } else {
                s.push(' ');
            }
        }
    }
    while s.chars().count() < 47 {
        s.push(' ');
    }
    s.push(':');
    s
}

fn kw(indent: usize, text: &str, keyword: &str, value: &str) -> String {
    let mut s = label(indent, text);
    if !keyword.is_empty() {
        s = format!("{s} {keyword}");
    }
    if !value.is_empty() {
        s = format!("{s:<60}{value}");
    }
    s.trim_end().to_string()
}

/// A number right-aligned to end at column `end` (1-based).
fn num(indent: usize, text: &str, n: i64, end: usize) -> String {
    let l = label(indent, text);
    format!("{l}{n:>w$}", w = end - l.chars().count())
}

/// (century, YYMMDD, HHMMSS) as the job's date and time.
fn stamp((c, d, t): &(String, String, String)) -> crate::report::Stamp {
    let n = |s: &str, i: usize| {
        s.get(i..i + 2)
            .and_then(|x| x.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let year = if c == "1" { 2000 } else { 1900 } + n(d, 0) as i32;
    crate::report::Stamp::of((year, n(d, 2), n(d, 4)), (n(t, 0), n(t, 2), n(t, 4)))
}

/// Lines a page holds.
const PAGE: usize = 60;

/// A member's lines: the part before "Implicit access path sharing" and
/// the part kept on one page.
fn member_lines(f: &FileInfo, x: &MemberExtra, m: &Member) -> (Vec<String>, Vec<String>) {
    let sql = matches!(f.sql_type.as_str(), "TABLE" | "INDEX");
    let keyed = !x.partial_keys.is_empty();
    let source = m.srctype.is_some();
    let created = stamp(&m.created);
    let changed = stamp(&m.changed);
    let mut head = vec![
        kw(3, "Member", "MBR", &m.name),
        kw(
            5,
            "Member level identifier",
            "",
            &format!("{}{}{}", m.created.0, m.created.1, m.created.2),
        ),
        kw(5, "Member creation date", "", &created.date),
        kw(5, "Text 'description'", "TEXT", &m.text),
        kw(5, "Expiration date for member", "EXPDATE", "*NONE"),
    ];
    // An access path's maintenance and recovery: keyed members only
    // (n45_mbra and n45_mbrs have none); an SQL table's recovers
    // *AFTIPL, a DDS file's *NO (n45_mbrp, n45_mbrl).
    if keyed {
        head.push(kw(5, "Access path maintenance", "MAINT", "*IMMED"));
        head.push(kw(
            5,
            "Access path recovery",
            "RECOVER",
            if sql { "*AFTIPL" } else { "*NO" },
        ));
    }
    if f.logical {
        head.extend([
            kw(5, "Preferred storage unit", "UNIT", "*ANY"),
            kw(5, "Record format selector program", "FMTSLR", "*NONE"),
            kw(5, "Records to force a write", "FRCRATIO", "*NONE"),
            kw(5, "Share open data path", "SHARE", "*NO"),
            label(5, "Access Path Activity Statistics"),
            label(7, "Access path logical reads"),
            label(7, "Access path physical reads"),
            num(7, "Index size", 90112, 75),
            kw(7, "Access path valid", "", "Yes"),
        ]);
    } else {
        if sql {
            head.push(kw(5, "Member size", "SIZE", "*NOMAX"));
        } else {
            // CRTPF's SIZE(10000 1000 3), CRTSRCPF's (10000 1000 499)
            // (n45_mbrp, n45_mbrs).
            let incs = if source { 499 } else { 3 };
            head.extend([
                format!("{:<49}SIZE", "     Member size"),
                num(7, "Initial number of records", 10000, 70),
                num(7, "Increment number of records", 1000, 65),
                num(7, "Maximum number of increments", incs, 65),
                num(5, "Current number of increments", 0, 70),
                num(5, "Record capacity", 10000 + 1000 * incs, 70),
            ]);
        }
        head.extend([
            num(5, "Current number of records", m.records, 70),
            num(5, "Number of deleted records", m.deleted, 70),
            kw(5, "Allocate storage", "ALLOCATE", "*NO"),
            kw(5, "Contiguous storage", "CONTIG", "*NO"),
            kw(5, "Preferred storage unit", "UNIT", "*ANY"),
            kw(5, "Records to force a write", "FRCRATIO", "*NONE"),
            kw(5, "Share open data path", "SHARE", "*NO"),
            kw(5, "Max % deleted records allowed", "DLTPCT", "*NONE"),
        ]);
        if keyed {
            head.push(num(5, "Number of member accesses", 0, 70));
        }
        head.extend([
            label(5, "Data Space Activity Statistics"),
            num(
                7,
                "Data space size in bytes",
                if sql { 12288 } else { 8192 },
                67,
            ),
        ]);
        for c in [
            "Physical file open accesses",
            "Physical file close accesses",
            "Write operations",
            "Update operations",
            "Delete operations",
            "Logical Reads",
            "Physical Reads",
            "Clear operations",
            "Data space copy operations",
            "Reorganize operations",
            "Access paths builds/rebuilds",
            "Records rejected by key selection",
            "Records rejected by non-key selection",
            "Records rejected by group-by selection",
        ] {
            head.push(label(7, c));
        }
        head.extend([
            label(5, "Access Path Activity Statistics"),
            label(7, "Access path logical reads"),
            label(7, "Access path physical reads"),
        ]);
        if keyed {
            head.extend([
                num(7, "Access path size", if sql { 155648 } else { 90112 }, 75),
                num(7, "Number of access path entries", m.records, 70),
                kw(7, "Access path valid", "", "Yes"),
            ]);
        }
    }
    let mut tail = vec![kw(7, "Implicit access path sharing", "", "No")];
    if keyed {
        tail.extend([
            kw(7, "Access path journaled", "", "No"),
            label(7, "Number of unique partial key values"),
        ]);
        for (i, n) in x.partial_keys.iter().enumerate() {
            let text = if i == 0 {
                "Key field 1".to_string()
            } else {
                format!("Key fields 1 - {}", i + 1)
            };
            tail.push(num(9, &text, *n, 75));
        }
    }
    if let Some(t) = &m.srctype {
        tail.push(kw(5, "Source type", "", t));
    }
    let when = format!("{}  {}", changed.date, changed.time);
    tail.push(kw(5, "Last change date/time", "", &when));
    if source {
        tail.push(kw(5, "Last source update date/time", "", &when));
    }
    tail.extend([
        label(5, "Last save date/time"),
        label(5, "Last restore date/time"),
    ]);
    if f.logical {
        // A logical member is not used itself (n45_mbrl).
        tail.extend([
            label(5, "Last used date"),
            num(5, "Days used count", 0, 65),
            label(7, "Reset date"),
            num(5, "Number of data members", 1, 65),
        ]);
        let (bl, bf) = f.based_on.clone().unwrap_or_default();
        tail.extend([
            kw(5, "Based on file", "", &bf),
            kw(7, "Library", "", &crate::qtemp::logical(&bl)),
            kw(7, "Member", "", &bf),
            kw(7, "Logical file format", "", &f.file.format),
            num(7, "Number of index entries", m.records, 70),
        ]);
    } else {
        tail.extend([
            kw(5, "Last used date", "", &changed.date),
            num(5, "Days used count", 1, 65),
            label(7, "Reset date"),
            "     Object restored with partial transactions,".into(),
            kw(7, "apply journal changes required", "", "*NO"),
            "     Partial transaction exists,".into(),
            kw(7, "roll back ended", "", "*NO"),
            label(5, "Starting journal receiver for apply"),
            label(7, "Library"),
            label(7, "ASP Device"),
        ]);
        if !x.constraints.is_empty() {
            tail.push(label(5, "Constraint access path attributes"));
            for c in &x.constraints {
                tail.extend([
                    kw(7, "Constraint", "CST", c),
                    kw(9, "Access path recovery", "", "*AFTIPL"),
                    kw(9, "Forced key access path", "", "*NO"),
                    kw(9, "Key must be unique", "", "*YES"),
                    kw(9, "Access path journaled", "", "*NO"),
                    kw(9, "Access path valid", "", "*YES"),
                    num(9, "Number of access path entries", m.records, 70),
                    num(9, "Access path size", 155648, 75),
                    label(9, "Access path logical reads"),
                    label(9, "Access path physical reads"),
                ]);
            }
        }
    }
    (head, tail)
}

pub fn report(f: &FileInfo, x: &MemberExtra, now: &crate::report::Stamp) -> Vec<String> {
    let lib = crate::qtemp::logical(&f.library);
    let header = |page: usize| {
        let when = format!("{}  {}", now.date, now.time);
        crate::report::at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (39, "Display File Description"),
            (97, &when),
            (122, &format!("Page {page:>4}")),
        ])
    };
    let kind = if f.logical {
        "Logical *DATA"
    } else if f.source {
        "Physical *SRC"
    } else {
        "Physical *DATA"
    };
    let file_line = format!(
        " File . : {:<10}   Library . : {lib:<10}   Type of file . : {kind:<26}Auxiliary Storage Pool ID . : 00001",
        f.name
    );
    let title = crate::report::at(&[(41, "Member Description")]);
    let mut out = vec![
        header(1),
        format!("   File  . . . . . . . . . . . : {}", f.name),
        format!("     Library . . . . . . . . . : {lib}"),
        "   Type of information . . . . : *MBR".into(),
        "   File attributes . . . . . . : *ALL".into(),
        "   System  . . . . . . . . . . : *LCL".into(),
        "   Processor . . . . . . . . . : AS4TAINR Display File Description Processor".into(),
        file_line.clone(),
        title.clone(),
    ];
    let mut page = 1;
    let mut on_page = out.len();
    let mut new_page = |out: &mut Vec<String>, on_page: &mut usize| {
        page += 1;
        out.extend([header(page), file_line.clone(), title.clone()]);
        *on_page = 3;
    };
    for m in &x.members {
        let (head, tail) = member_lines(f, x, m);
        for line in head {
            if on_page == PAGE {
                new_page(&mut out, &mut on_page);
            }
            out.push(line);
            on_page += 1;
        }
        if on_page + tail.len() > PAGE {
            new_page(&mut out, &mut on_page);
        }
        for line in tail {
            if on_page == PAGE {
                new_page(&mut out, &mut on_page);
            }
            out.push(line);
            on_page += 1;
        }
    }
    out
}

/// IBM i's name for a key constraint SQL made without one
/// (Q_library_file_K_00001 for a primary key; i40_mbr1).
pub fn constraint_name(lib: &str, file: &str, name: &str) -> String {
    if name == "PK" {
        format!("Q_{lib}_{file}_K_00001")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
#[path = "mbrdesc_cases.rs"]
mod cases;

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture's report lines: carriage returns, PUB400's page footer
    /// and the exit status line left out.
    fn capture(text: &str) -> Vec<String> {
        text.lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Our lines and the capture's compared line by line, but for the
    /// product line, the processor, the dates, times and level
    /// identifiers, and the counters `counters` names (as4tainr keeps
    /// none: blank here).
    fn same(ours: &[String], ibm: &[String], counters: &[&str]) {
        let norm = |l: &str| -> String {
            if l.starts_with(" 5770SS1") || l.starts_with(" AS4T001") {
                return l.split("Page").nth(1).unwrap_or("").trim().to_string();
            }
            if l.contains("Processor . .") {
                return String::new();
            }
            let mut s = String::new();
            for w in l.split(' ') {
                let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
                let dated = w.len() == 8
                    && digits == 6
                    && matches!(w.chars().nth(2), Some('-') | Some('/'));
                let timed = w.len() == 8 && digits == 6 && w.chars().nth(2) == Some(':');
                let level = w.len() == 13 && digits == 13;
                s.push_str(if dated || timed || level { "X" } else { w });
                s.push(' ');
            }
            if counters.iter().any(|c| l.contains(c)) {
                return l.split(':').next().unwrap_or("").to_string();
            }
            s
        };
        assert_eq!(ours.len(), ibm.len(), "lines\n{ours:#?}\n{ibm:#?}");
        for (o, i) in ours.iter().zip(ibm) {
            assert_eq!(norm(o), norm(i), "\nours {o:?}\nibm  {i:?}");
        }
    }

    fn info(name: &str, sql: &str, fields: &str) -> FileInfo {
        FileInfo {
            library: "AS4TRAINR1".into(),
            name: name.into(),
            logical: false,
            source: false,
            sql_type: sql.into(),
            text: String::new(),
            file: crate::dds::parse_pf(name, fields).unwrap(),
            based_on: None,
            select_omit: false,
            now: ("1".into(), "261104".into(), "225600".into()),
            created: ("1".into(), "261104".into(), "225600".into()),
            changed: ("1".into(), "261104".into(), "225600".into()),
            records: 0,
            deleted: 0,
            journal: None,
            system: "PUB400".into(),
        }
    }

    fn member(name: &str, records: i64, deleted: i64) -> Member {
        Member {
            name: name.into(),
            text: String::new(),
            created: ("1".into(), "261104".into(), "225600".into()),
            changed: ("1".into(), "261104".into(), "225600".into()),
            records,
            deleted,
            srctype: None,
        }
    }

    fn now() -> crate::report::Stamp {
        crate::report::Stamp::of((2026, 11, 4), (22, 56, 0))
    }

    const COUNTERS: [&str; 5] = [
        "Physical file open",
        "Physical file close",
        "Write operations",
        "Delete operations",
        "Access paths builds",
    ];

    // Labels as PUB400 printed them (2026-11-03, verify40 i40_mbr1).
    #[test]
    fn leader_dots() {
        for (indent, text, want) in [
            (
                3,
                "Member",
                "   Member  . . . . . . . . . . . . . . . . . . :",
            ),
            (
                5,
                "Member level identifier",
                "     Member level identifier . . . . . . . . . :",
            ),
            (
                7,
                "Records rejected by group-by selection",
                "       Records rejected by group-by selection. :",
            ),
            (
                9,
                "Key fields 1 - 2",
                "         Key fields 1 - 2  . . . . . . . . . . :",
            ),
        ] {
            assert_eq!(label(indent, text), want);
        }
    }

    // i40_mbr1: an SQL table keyed by its primary key, 3 records, 1
    // deleted, the unnamed constraint.
    #[test]
    fn sql_table_keyed_by_its_primary_key() {
        let f = info(
            "GLD40T",
            "TABLE",
            "     A          R GLD40T\n     A            K              5P 0\n     A            NAME          10A\n",
        );
        let x = MemberExtra {
            partial_keys: vec![3],
            constraints: vec![constraint_name("AS4TRAINR1", "GLD40T", "PK")],
            members: vec![member("GLD40T", 3, 1)],
        };
        let ibm = capture(cases::I40_MBR1);
        same(
            &report(&f, &x, &now()),
            &ibm,
            &[&COUNTERS[..], &["Logical Reads"]].concat(),
        );
    }

    // n45_mbra: an SQL table in arrival sequence; n45_mbru: named
    // primary key and unique constraints, two key fields.
    #[test]
    fn sql_tables_in_arrival_sequence_and_with_named_constraints() {
        let f = info(
            "GLD45A",
            "TABLE",
            "     A          R GLD45A\n     A            ID             9B 0\n",
        );
        let x = MemberExtra {
            partial_keys: vec![],
            constraints: vec![],
            members: vec![member("GLD45A", 2, 0)],
        };
        let ibm = capture(cases::N45_MBRA);
        same(&report(&f, &x, &now()), &ibm, &COUNTERS);
        let f = info(
            "GLD45U",
            "TABLE",
            "     A          R GLD45U\n     A            A              3A\n",
        );
        let x = MemberExtra {
            partial_keys: vec![2, 3],
            constraints: vec!["GLD45UPK".into(), "GLD45UQ".into()],
            members: vec![member("GLD45U", 3, 0)],
        };
        let ibm = capture(cases::N45_MBRU);
        same(&report(&f, &x, &now()), &ibm, &COUNTERS);
    }

    // n45_mbrp: a keyed DDS physical file, 2 records and 1 deleted;
    // n45_mbrl: a DDS logical file over it keyed by NAME.
    #[test]
    fn dds_physical_and_logical_files() {
        let f = info(
            "GLD45P",
            "",
            "     A          R GLD45R\n     A            CODE           5P 0\n     A            NAME          20A\n     A          K CODE\n",
        );
        let x = MemberExtra {
            partial_keys: vec![2],
            constraints: vec![],
            members: vec![member("GLD45P", 2, 1)],
        };
        let ibm = capture(cases::N45_MBRP);
        same(&report(&f, &x, &now()), &ibm, &COUNTERS);
        let mut f = info(
            "GLD45L",
            "",
            "     A          R GLD45R\n     A            CODE           5P 0\n     A            NAME          20A\n     A          K NAME\n",
        );
        f.logical = true;
        f.based_on = Some(("AS4TRAINR1".into(), "GLD45P".into()));
        let x = MemberExtra {
            partial_keys: vec![2],
            constraints: vec![],
            members: vec![member("GLD45L", 2, 0)],
        };
        let ibm = capture(cases::N45_MBRL);
        same(&report(&f, &x, &now()), &ibm, &COUNTERS);
    }

    // n45_mbrs: a source file with two members of 6 and 2 lines, no
    // source type; each member's last part on a page of its own.
    #[test]
    fn source_file_members() {
        let mut f = info(
            "GLD45S",
            "",
            "     A          R GLD45S\n     A            SRCDTA        80A\n",
        );
        f.source = true;
        let src = |name: &str, n: i64| Member {
            srctype: Some(String::new()),
            ..member(name, n, 0)
        };
        let ibm = capture(cases::N45_MBRS);
        let recs: Vec<i64> = ibm
            .iter()
            .filter(|l| l.contains("Current number of records"))
            .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
            .collect();
        let x = MemberExtra {
            partial_keys: vec![],
            constraints: vec![],
            members: vec![src("GLD44C", recs[0]), src("GLD44B", recs[1])],
        };
        same(&report(&f, &x, &now()), &ibm, &COUNTERS);
    }
}
