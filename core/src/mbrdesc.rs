//! DSPFD TYPE(*MBR) printed: the Member Description of a data physical
//! file in IBM i 7.5's layout (PUB400 2026-11-03,
//! tools/golden/verify40.txt i40_mbr1 and i40_mbr2: an SQL table keyed
//! by its primary key, before and after RGZPFM).
//!
//! Labels end with the colon at column 48; a parameter keyword starts at
//! column 50 and its value at column 61; counts of records and access
//! path values start at column 70; operation counters end at column 78.
//! Operation counters as4tainr does not keep (opens, writes, logical
//! reads...) are printed blank, as IBM i prints a zero counter
//! (docs/fidelity-gaps.md, "DSPFD TYPE(*MBR)").

use crate::fdout::FileInfo;

/// What the report needs beyond the outfiles' FileInfo.
pub struct MemberExtra {
    /// The key fields of the member's access path (the file's keys, or
    /// its primary key's columns) with the number of distinct values of
    /// each leading part of the key.
    pub partial_keys: Vec<i64>,
    /// Key constraints (primary key, unique) by the names IBM i shows.
    pub constraints: Vec<String>,
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

/// A count from column 70.
fn count(indent: usize, text: &str, n: Option<i64>) -> String {
    match n {
        Some(n) => format!("{:<69}{n}", label(indent, text)),
        None => label(indent, text),
    }
}

/// An operation counter, ending at column 78; blank when not kept.
fn counter(text: &str) -> String {
    label(7, text)
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

/// Lines before the page break of page 1 (i40_mbr1: the header, six
/// lines of what was asked, the file line, the section title and 39
/// lines of the member).
const FIRST_PAGE: usize = 48;

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
    let file_line = format!(
        " File . : {:<10}   Library . : {lib:<10}   Type of file . : {:<26}Auxiliary Storage Pool ID . : 00001",
        f.name, "Physical *DATA"
    );
    let title = crate::report::at(&[(41, "Member Description")]);
    let sql = matches!(f.sql_type.as_str(), "TABLE" | "INDEX");
    let keyed = !x.partial_keys.is_empty();
    let created = stamp(&f.created);
    let changed = stamp(&f.changed);
    let mut body = vec![
        kw(3, "Member", "MBR", &f.name),
        kw(
            5,
            "Member level identifier",
            "",
            &format!("{}{}{}", f.created.0, f.created.1, f.created.2),
        ),
        kw(5, "Member creation date", "", &created.date),
        kw(5, "Text 'description'", "TEXT", &f.text),
        kw(5, "Expiration date for member", "EXPDATE", "*NONE"),
        kw(5, "Access path maintenance", "MAINT", "*IMMED"),
        kw(
            5,
            "Access path recovery",
            "RECOVER",
            if sql { "*AFTIPL" } else { "*NO" },
        ),
    ];
    if sql {
        body.push(kw(5, "Member size", "SIZE", "*NOMAX"));
    } else {
        // A DDS file's SIZE as DSPFD TYPE(*ATR) prints it (CRTPF's
        // default 10000 1000 3; not captured for *MBR).
        body.push(kw(5, "Member size", "SIZE", ""));
        body.push(count(7, "Initial number of records", Some(10000)));
        body.push(count(7, "Increment number of records", Some(1000)));
        body.push(count(7, "Maximum number of increments", Some(3)));
    }
    body.extend([
        count(5, "Current number of records", Some(f.records)),
        count(5, "Number of deleted records", Some(f.deleted)),
        kw(5, "Allocate storage", "ALLOCATE", "*NO"),
        kw(5, "Contiguous storage", "CONTIG", "*NO"),
        kw(5, "Preferred storage unit", "UNIT", "*ANY"),
        kw(5, "Records to force a write", "FRCRATIO", "*NONE"),
        kw(5, "Share open data path", "SHARE", "*NO"),
        kw(5, "Max % deleted records allowed", "DLTPCT", "*NONE"),
        count(5, "Number of member accesses", Some(0)),
        label(5, "Data Space Activity Statistics"),
        format!(
            "{:<62}{}",
            label(7, "Data space size in bytes"),
            if sql { 12288 } else { 8192 }
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
        body.push(counter(c));
    }
    let path_size = if sql { 155648 } else { 90112 };
    if keyed {
        body.extend([
            label(5, "Access Path Activity Statistics"),
            label(7, "Access path logical reads"),
            label(7, "Access path physical reads"),
            count(7, "Access path size", Some(path_size)),
            count(7, "Number of access path entries", Some(f.records)),
            kw(7, "Access path valid", "", "Yes"),
            kw(7, "Implicit access path sharing", "", "No"),
            kw(7, "Access path journaled", "", "No"),
            label(7, "Number of unique partial key values"),
        ]);
        for (i, n) in x.partial_keys.iter().enumerate() {
            body.push(format!(
                "{:<74}{n}",
                label(9, &format!("Key field {}", i + 1))
            ));
        }
    }
    body.extend([
        kw(
            5,
            "Last change date/time",
            "",
            &format!("{}  {}", changed.date, changed.time),
        ),
        label(5, "Last save date/time"),
        label(5, "Last restore date/time"),
        kw(5, "Last used date", "", &changed.date),
        format!("{:<64}1", label(5, "Days used count")),
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
        body.push(label(5, "Constraint access path attributes"));
        for c in &x.constraints {
            body.extend([
                kw(7, "Constraint", "CST", c),
                kw(9, "Access path recovery", "", "*AFTIPL"),
                kw(9, "Forced key access path", "", "*NO"),
                kw(9, "Key must be unique", "", "*YES"),
                kw(9, "Access path journaled", "", "*NO"),
                kw(9, "Access path valid", "", "*YES"),
                count(9, "Number of access path entries", Some(f.records)),
                count(9, "Access path size", Some(155648)),
                label(9, "Access path logical reads"),
                label(9, "Access path physical reads"),
            ]);
        }
    }
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
    let mut limit = FIRST_PAGE;
    for line in body {
        if on_page == limit {
            page += 1;
            out.extend([header(page), file_line.clone(), title.clone()]);
            on_page = 3;
            limit = FIRST_PAGE;
        }
        out.push(line);
        on_page += 1;
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
mod tests {
    use super::*;

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
                5,
                "Text 'description'",
                "     Text 'description'  . . . . . . . . . . . :",
            ),
            (
                7,
                "Records rejected by non-key selection",
                "       Records rejected by non-key selection . :",
            ),
            (
                7,
                "Records rejected by group-by selection",
                "       Records rejected by group-by selection. :",
            ),
            (
                9,
                "Key field 1",
                "         Key field 1 . . . . . . . . . . . . . :",
            ),
            (
                7,
                "apply journal changes required",
                "       apply journal changes required  . . . . :",
            ),
        ] {
            assert_eq!(label(indent, text), want);
        }
    }

    // The member of an SQL table keyed by its primary key with three
    // records and one deleted: i40_mbr1 line for line from the section
    // title on, but the dates, the level identifier, the counters
    // as4tainr does not keep and PUB400's own page footer.
    #[test]
    fn sql_table_member() {
        let pf = crate::dds::parse_pf(
            "GLD40T",
            "     A          R GLD40T\n     A            K              5P 0\n     A            NAME          10A\n",
        )
        .unwrap();
        let f = FileInfo {
            library: "AS4TRAINR1".into(),
            name: "GLD40T".into(),
            logical: false,
            source: false,
            sql_type: "TABLE".into(),
            text: String::new(),
            file: pf,
            based_on: None,
            select_omit: false,
            now: ("1".into(), "261103".into(), "172721".into()),
            created: ("1".into(), "261103".into(), "172719".into()),
            changed: ("1".into(), "261103".into(), "172720".into()),
            records: 3,
            deleted: 1,
            journal: None,
            system: "PUB400".into(),
        };
        let x = MemberExtra {
            partial_keys: vec![3],
            constraints: vec![constraint_name("AS4TRAINR1", "GLD40T", "PK")],
        };
        let now = crate::report::Stamp::of((2026, 11, 3), (17, 27, 21));
        let r = report(&f, &x, &now);
        let lines: Vec<&str> = r.iter().map(|s| s.as_str()).collect();
        let want = [
            " File . : GLD40T       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001",
            "                                        Member Description",
            "   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD40T",
            "     Member level identifier . . . . . . . . . :            1261103172719",
            "     Text 'description'  . . . . . . . . . . . : TEXT",
            "     Expiration date for member  . . . . . . . : EXPDATE    *NONE",
            "     Access path maintenance . . . . . . . . . : MAINT      *IMMED",
            "     Access path recovery  . . . . . . . . . . : RECOVER    *AFTIPL",
            "     Member size . . . . . . . . . . . . . . . : SIZE       *NOMAX",
            "     Current number of records . . . . . . . . :                     3",
            "     Number of deleted records . . . . . . . . :                     1",
            "     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO",
            "     Share open data path  . . . . . . . . . . : SHARE      *NO",
            "     Max % deleted records allowed . . . . . . : DLTPCT     *NONE",
            "     Number of member accesses . . . . . . . . :                     0",
            "     Data Space Activity Statistics  . . . . . :",
            "       Data space size in bytes  . . . . . . . :              12288",
            "       Update operations . . . . . . . . . . . :",
            "       Records rejected by group-by selection. :",
            "     Access Path Activity Statistics . . . . . :",
            "       Access path size  . . . . . . . . . . . :                     155648",
            "       Number of access path entries . . . . . :                     3",
            "       Access path valid . . . . . . . . . . . :            Yes",
            "       Implicit access path sharing  . . . . . :            No",
            "       Access path journaled . . . . . . . . . :            No",
            "       Number of unique partial key values . . :",
            "         Key field 1 . . . . . . . . . . . . . :                          3",
            "     Last save date/time . . . . . . . . . . . :",
            "     Days used count . . . . . . . . . . . . . :                1",
            "       Reset date  . . . . . . . . . . . . . . :",
            "     Object restored with partial transactions,",
            "       apply journal changes required  . . . . :            *NO",
            "     Partial transaction exists,",
            "       roll back ended . . . . . . . . . . . . :            *NO",
            "     Starting journal receiver for apply . . . :",
            "       ASP Device  . . . . . . . . . . . . . . :",
            "     Constraint access path attributes . . . . :",
            "       Constraint  . . . . . . . . . . . . . . : CST        Q_AS4TRAINR1_GLD40T_K_00001",
            "         Access path recovery  . . . . . . . . :            *AFTIPL",
            "         Key must be unique  . . . . . . . . . :            *YES",
            "         Access path valid . . . . . . . . . . :            *YES",
            "         Number of access path entries . . . . :                     3",
            "         Access path size  . . . . . . . . . . :                     155648",
            "         Access path physical reads  . . . . . :",
        ];
        for w in want {
            assert!(lines.contains(&w), "missing {w:?} in {lines:#?}");
        }
        // Page 2 begins after the 48th line with the header, the file
        // line and the title, then "Implicit access path sharing".
        assert!(lines[48].contains("Page    2"), "{:?}", lines[48]);
        assert!(lines[49].starts_with(" File . : GLD40T"));
        assert!(lines[51].starts_with("       Implicit access path sharing"));
        assert_eq!(
            lines[47],
            "       Access path valid . . . . . . . . . . . :            Yes"
        );
        // i40_mbr1 has 79 lines besides PUB400's own page footers.
        assert_eq!(lines.len(), 79);
    }
}
