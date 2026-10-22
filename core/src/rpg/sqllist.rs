//! The SQL precompiler's listing (CRTSQLRPGI OUTPUT(*PRINT)): the
//! command's options, the source, and a cross reference of the names the
//! SQL statements use (host variables, tables and their columns).
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-22, tools/golden/
//! listings2.txt). The precompiler's own generated RPG source is not
//! shown (as4tainr runs embedded SQL itself); the ILE RPG listing that
//! follows lists the source as written.

use std::collections::BTreeMap;

use super::data::DType;
use crate::report::{at, Stamp};

/// The command's values the listing shows.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub library: String,
    pub object: String,
    pub objtype: String,
    pub commit: String,
    pub text: String,
    pub replace: String,
    pub dbgview: String,
}

/// A table an SQL statement uses: its name and columns (name, SQL type,
/// null-capable).
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<(String, String, bool)>,
}

/// A host variable: its name, where it is declared and its SQL type.
#[derive(Debug, Clone, Default)]
pub struct HostVar {
    pub name: String,
    pub line: usize,
    pub sql_type: String,
}

/// Lines on a page, the header included (the options fit on the first,
/// as observed).
const PAGE: usize = 60;

/// An SQL statement of the source: the line it starts on and its text.
/// Free-form `EXEC SQL ... ;` (also in the free-form calculations of a
/// fixed-form source, from column 8) and fixed-form `C/EXEC SQL` ...
/// `C/END-EXEC`.
pub fn statements(source: &str) -> Vec<(usize, String)> {
    let free = super::parse::is_fully_free(source);
    let mut out: Vec<(usize, String)> = Vec::new();
    // (start line, text, free form)
    let mut cur: Option<(usize, String, bool)> = None;
    for (i, raw) in source.lines().enumerate() {
        let n = i + 1;
        let cs: Vec<char> = raw.chars().collect();
        let fixed_body: String = cs.iter().skip(6).take(74).collect();
        let up = fixed_body.trim().to_ascii_uppercase();
        if !free
            && cur.is_none()
            && cs.get(5).is_some_and(|c| c.eq_ignore_ascii_case(&'C'))
            && up.starts_with("/EXEC SQL")
        {
            cur = Some((n, fixed_body.trim()[9..].to_string(), false));
            continue;
        }
        if let Some((_, acc, false)) = cur.as_mut() {
            if up.starts_with("/END-EXEC") {
                if let Some((l, t, _)) = cur.take() {
                    out.push((l, t.trim().to_string()));
                }
            } else if cs.get(6) == Some(&'+') {
                acc.push(' ');
                acc.push_str(&fixed_body[1.min(fixed_body.len())..]);
            }
            continue;
        }
        // Free form (from column 8 in a fixed-form source).
        let content: String = if free {
            raw.to_string()
        } else {
            cs.iter().skip(7).collect()
        };
        let text = match &cur {
            Some(_) => content,
            None => {
                let t = content.trim_start();
                if t.len() >= 8 && t[..8].eq_ignore_ascii_case("exec sql") {
                    cur = Some((n, String::new(), true));
                    t[8..].to_string()
                } else {
                    continue;
                }
            }
        };
        // The statement ends with ';' outside literals.
        let mut q = false;
        let mut end = None;
        for (k, c) in text.char_indices() {
            if c == '\'' {
                q = !q;
            } else if c == ';' && !q {
                end = Some(k);
                break;
            }
        }
        if let Some((_, acc, _)) = cur.as_mut() {
            acc.push(' ');
            acc.push_str(&text[..end.unwrap_or(text.len())]);
        }
        if end.is_some() {
            if let Some((l, t, _)) = cur.take() {
                out.push((l, t.trim().to_string()));
            }
        }
    }
    out
}

/// The words of an SQL statement outside literals, upper-cased (names
/// keep their punctuation: `:N`, `LIB/T`, `LIB.T`).
fn words(stmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    for c in stmt.chars() {
        if c == '\'' {
            q = !q;
            continue;
        }
        if q {
            continue;
        }
        if c.is_ascii_alphanumeric() || "_@#$:/.".contains(c) {
            cur.push(c.to_ascii_uppercase());
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if c == '(' {
                out.push("(".into());
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The tables a statement names (FROM, JOIN, UPDATE, DELETE FROM, INSERT
/// INTO), without their library.
pub fn tables(stmt: &str) -> Vec<String> {
    let w = words(stmt);
    let mut out = Vec::new();
    let insert = w.first().is_some_and(|x| x == "INSERT");
    for (k, x) in w.iter().enumerate() {
        let takes = matches!(x.as_str(), "FROM" | "JOIN" | "UPDATE") || (x == "INTO" && insert);
        if !takes {
            continue;
        }
        if let Some(t) = w.get(k + 1) {
            if t == "(" || t.starts_with(':') {
                continue;
            }
            let name = t.rsplit(['/', '.']).next().unwrap_or(t).to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// The host variables a statement uses (`:name`), without their colon.
pub fn host_variables(stmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    for x in words(stmt) {
        if let Some(v) = x.strip_prefix(':') {
            if !v.is_empty() && !v.starts_with(':') {
                let v = v.to_string();
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// The SQL type of an RPG field (as the precompiler's cross reference
/// shows it).
pub fn sql_type(t: &DType) -> String {
    match t {
        DType::Char(n) => format!("CHARACTER({n})"),
        DType::Varchar(n) => format!("VARCHAR({n})"),
        DType::Int(3) | DType::Int(5) => "SMALLINT PRECISION(4,0)".into(),
        DType::Int(10) => "INTEGER PRECISION(9,0)".into(),
        DType::Int(_) => "BIGINT PRECISION(19,0)".into(),
        DType::Uns(n) => format!("UNSIGNED({n},0)"),
        DType::Packed(p, s) => format!("DECIMAL({p},{s})"),
        DType::Zoned(p, s) => format!("NUMERIC({p},{s})"),
        DType::Binary(p, s) => format!("NUMERIC({p},{s})"),
        DType::Float(4) => "FLOAT(24)".into(),
        DType::Float(_) => "FLOAT(53)".into(),
        DType::Date(_) => "DATE".into(),
        DType::Time(_) => "TIME".into(),
        DType::Timestamp => "TIMESTAMP".into(),
        DType::Ind => "CHARACTER(1)".into(),
        DType::Pointer => "POINTER".into(),
        DType::Ucs2(n) => format!("GRAPHIC({n})"),
        DType::VarUcs2(n) => format!("VARGRAPHIC({n})"),
    }
}

/// The SQL type of a column.
pub fn column_type(f: &crate::dds::FieldDef, ccsid: i32) -> String {
    use crate::dds::DataType as T;
    match f.data_type {
        T::Char if f.varlen => format!("VARCHAR({}) CCSID {ccsid}", f.length),
        T::Char => format!("CHARACTER({}) CCSID {ccsid}", f.length),
        T::Packed => format!("DECIMAL({},{})", f.length, f.decimals),
        T::Zoned => format!("NUMERIC({},{})", f.length, f.decimals),
        T::Binary if f.decimals == 0 && f.length <= 4 => "SMALLINT".into(),
        T::Binary if f.decimals == 0 && f.length <= 9 => "INTEGER".into(),
        T::Binary if f.decimals == 0 => "BIGINT".into(),
        T::Binary => format!("NUMERIC({},{})", f.length, f.decimals),
        T::Date => "DATE".into(),
        T::Time => "TIME".into(),
        T::Timestamp => "TIMESTAMP".into(),
        T::Float if f.double => "FLOAT(53)".into(),
        T::Float => "FLOAT(24)".into(),
        T::Graphic if f.varlen => format!("VARGRAPHIC({})", f.length),
        T::Graphic => format!("GRAPHIC({})", f.length),
        T::Hex if f.varlen => format!("VARBINARY({})", f.length),
        T::Hex => format!("BINARY({})", f.length),
        T::Clob => "CLOB".into(),
        T::Blob => "BLOB".into(),
        T::Dbclob => "DBCLOB".into(),
        T::Decfloat => format!("DECFLOAT({})", f.length),
        T::Boolean => "BOOLEAN".into(),
        T::Xml => "XML".into(),
    }
}

/// The precompiler's listing.
pub fn listing(
    o: &Opts,
    m: &crate::source::Member,
    tables_used: &[Table],
    host: &[HostVar],
    stamp: &Stamp,
) -> Vec<String> {
    let header = |page: usize| {
        at(&[
            (1, "AS4T001 V0R1M0"),
            (16, crate::report::LEVEL),
            (34, "Create SQL ILE RPG Object"),
            (96, &stamp.date),
            (105, &stamp.time),
            (123, &format!("Page {page:>5}")),
        ])
    };
    let opt = |label: &str, value: &str| -> String {
        let mut l = label.to_string();
        while l.len() < 26 {
            l.push('.');
        }
        format!("{l}{value}")
    };
    let changed = m
        .changed_us
        .map(|us| {
            let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::from_epoch_us(us);
            let st = Stamp::of((y, mo, d), (h, mi, s));
            format!("{}  {}", st.date, st.time)
        })
        .unwrap_or_default();
    let mut first = vec![
        opt("Source type", "RPG"),
        opt("Object name", &format!("{}/{}", o.library, o.object)),
    ];
    if m.stmf.is_empty() {
        first.push(opt("Source file", &format!("{}/{}", m.library, m.file)));
        first.push(opt("Member", &m.name));
    } else {
        first.push(opt("Source stream file", &m.stmf));
    }
    // Labels as IBM i 7.5 prints them (some with a blank before their
    // dots, two on two lines).
    let rest: Vec<String> = vec![
        opt("To source file", "QTEMP/QSQLTEMP1"),
        opt("Options", "*XREF"),
        opt("RPG preprocessor options", "*NONE"),
        opt("Listing option", "*PRINT"),
        opt("Target release", "V7R5M0"),
        opt("SQL INCLUDE directory", "*NONE"),
        opt("Commit", &o.commit),
        opt("Allow copy of data", "*OPTIMIZE"),
        opt("Close SQL cursor", "*ENDACTGRP"),
        opt("Allow blocking", "*ALLREAD"),
        opt("Delay PREPARE", "*NO"),
        "Concurrent access".into(),
        opt("  resolution", "*DFT"),
        opt("Generation level", "10"),
        opt("Printer file", "*LIBL/QSYSPRT"),
        opt("Date format", "*JOB"),
        opt("Date separator", "*JOB"),
        opt("Time format", "*HMS"),
        opt("Time separator ", "*JOB"),
        opt("Replace", &o.replace),
        opt("Relational database", "*LOCAL"),
        opt("User ", "*CURRENT"),
        opt("RDB connect method", "*DUW"),
        opt("Default collection", "*NONE"),
        "Dynamic default".into(),
        opt("  collection", "*NO"),
        opt("Package name", "*OBJLIB/*OBJ"),
        opt("Path", "*NAMING"),
        opt("SQL rules", "*DB2"),
        opt("Created object type", &o.objtype),
        opt("Debugging view", &o.dbgview),
        opt("Debugging encryption key", "*NONE"),
        opt("User profile ", "*NAMING"),
        opt("Dynamic user profile", "*USER"),
        opt("Sort sequence", "*JOB"),
        opt("Language ID", "*JOB"),
        opt("IBM SQL flagging", "*NOFLAG"),
        opt("ANS flagging", "*NONE"),
        opt("Text", &o.text),
        opt("Source file CCSID", &m.ccsid.to_string()),
        opt("Job CCSID", "37"),
        "Decimal result options:".into(),
        opt("  Maximum precision", "31"),
        opt("  Maximum scale", "31"),
        opt("  Minimum divide scale", "0"),
        opt("DECFLOAT rounding mode", "*HALFEVEN"),
        opt("Compiler options", "*NONE"),
        format!("Source member changed on {changed}"),
    ];
    first.extend(rest);

    let mut source = vec![
        "Record  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+... 0   SEQNBR  Last change".to_string(),
    ];
    let lines: Vec<&str> = m.source.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        source.push(format!("{:>5}   {}", i + 1, l.trim_end()));
    }
    source.push(
        "                                * * * * *  E N D  O F  S O U R C E  * * * * *".into(),
    );

    // Cross reference: (name, define, reference text, reference lines).
    let stmts = statements(&m.source);
    let mut entries: BTreeMap<Vec<u8>, (String, String, String, Vec<usize>)> = BTreeMap::new();
    for t in tables_used {
        let refs: Vec<usize> = stmts
            .iter()
            .filter(|(_, s)| tables(s).contains(&t.name))
            .map(|(l, _)| *l)
            .collect();
        let first = refs.first().copied().unwrap_or(0);
        entries.insert(
            crate::rpg::data::e(&t.name),
            (t.name.clone(), "****".into(), "TABLE".into(), refs),
        );
        for (c, ty, null) in &t.columns {
            let nn = if *null { "" } else { " (NOT NULL)" };
            entries.entry(crate::rpg::data::e(c)).or_insert((
                c.clone(),
                format!("{first:>6}"),
                format!("{ty} COLUMN{nn} IN {}", t.name),
                vec![],
            ));
        }
    }
    for h in host {
        let refs: Vec<usize> = stmts
            .iter()
            .filter(|(_, s)| host_variables(s).contains(&h.name))
            .map(|(l, _)| *l)
            .collect();
        entries.insert(
            crate::rpg::data::e(&h.name),
            (
                h.name.clone(),
                format!("{:>6}", h.line),
                h.sql_type.clone(),
                refs,
            ),
        );
    }
    let mut xref = vec![
        at(&[(48, "CROSS REFERENCE")]),
        "Data Names                    Define    Reference".to_string(),
    ];
    for (name, define, text, refs) in entries.values() {
        let d = if define == "****" {
            format!("{:5}****", "")
        } else {
            define.clone()
        };
        xref.push(at(&[(1, name), (29, &d), (43, text)]));
        if !refs.is_empty() {
            let r: Vec<String> = refs.iter().map(|x| x.to_string()).collect();
            xref.push(at(&[(43, &r.join(" "))]));
        }
    }
    xref.push("No errors found in source".into());
    xref.push(format!("{:>5} Source records processed", lines.len()));
    xref.push(
        "                               * * * * *  E N D  O F  L I S T I N G  * * * * *".into(),
    );

    let mut out = Vec::new();
    let mut page = 0;
    for section in [first, source, xref] {
        for chunk in section.chunks(PAGE - 1) {
            page += 1;
            out.push(header(page));
            out.extend(chunk.iter().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_statements() {
        let s = "**FREE\ndcl-s n int(10);\nexec sql select count(*) into :n\n  from GLDLSTF;\n";
        let st = statements(s);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].0, 3);
        assert_eq!(tables(&st[0].1), ["GLDLSTF"]);
        assert_eq!(host_variables(&st[0].1), ["N"]);
        assert_eq!(tables("insert into lib/t values(:a)"), ["T"]);
    }
}
