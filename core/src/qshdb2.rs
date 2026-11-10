//! Qshell's db2 utility (IBM i documentation, "db2 - Run SQL
//! statements"): `db2 [options] 'statement'` runs an SQL statement and
//! writes a query's rows as a table.
//!
//! The table's layout is IBM i 7.5's (PUB400 2026-11-10, verify74
//! u74_exprows, u74_basrows, u74_improws): an empty line; the column
//! names, each column as wide as its data and at least one more than its
//! name, a blank between columns; a line of dashes under each; the rows
//! (character data on the left, numbers on the right); an empty line;
//! the count of rows, and an empty line. A decimal column's width is its
//! precision and two.

use crate::sql::{ColType, Exec, Session};
use anyhow::Result;

/// What the utility wrote and its exit status.
pub struct Output {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub status: u8,
}

/// How a column is shown.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Align {
    Left,
    /// Integers: on the right.
    Right,
    /// Decimal numbers: the sign in the first place, the digits on the
    /// right (u75_db2sel: DECIMAL(7,2) -12.5 is "-   12.50").
    Signed,
}

/// A column's width for its type, and its alignment (u75_db2sel: INTEGER
/// 11, SMALLINT 6, BIGINT 20, DOUBLE 24 on the left, DATE 10, TIME 8,
/// TIMESTAMP 26, DECIMAL(p,s) p+2).
fn width(t: &ColType) -> (usize, Align) {
    let m = t.typmod;
    match t.pg.as_str() {
        "bpchar" | "varchar" | "text" if m > 4 => ((m - 4) as usize, Align::Left),
        "numeric" if m > 4 => ((((m - 4) >> 16) & 0xffff) as usize + 2, Align::Signed),
        "numeric" => (33, Align::Signed),
        "int2" => (6, Align::Right),
        "int4" => (11, Align::Right),
        "int8" => (20, Align::Right),
        "float4" | "float8" => (24, Align::Left),
        "date" => (10, Align::Left),
        "time" => (8, Align::Left),
        "timestamp" | "timestamptz" => (26, Align::Left),
        _ => (1, Align::Left),
    }
}

/// A value as the utility shows it: times as hh.mm.ss, timestamps in
/// Db2's form with six fractional digits (u75_db2sel).
fn shown(t: Option<&ColType>, v: &str) -> String {
    match t.map(|t| t.pg.as_str()) {
        Some("time") => v.get(..8).unwrap_or(v).replace(':', "."),
        Some("timestamp" | "timestamptz") if v.len() >= 19 => {
            let (date, rest) = v.split_at(10);
            let time = rest[1..9].replace(':', ".");
            let frac = rest.get(10..).unwrap_or("");
            let frac: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
            format!("{date}-{time}.{frac:0<6}")
        }
        _ => v.to_string(),
    }
}

/// The table of a query's rows (u74_exprows, u75_db2sel).
pub fn table(columns: &[String], types: &[ColType], rows: &[Vec<Option<String>>]) -> Vec<String> {
    let rows: Vec<Vec<Option<String>>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, v)| v.as_ref().map(|v| shown(types.get(i), v)))
                .collect()
        })
        .collect();
    let cols: Vec<(usize, Align)> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (w, a) = types.get(i).map(width).unwrap_or((1, Align::Left));
            let w = rows
                .iter()
                .map(|r| {
                    r.get(i)
                        .and_then(|v| v.as_ref())
                        .map_or(1, |v| v.chars().count() + usize::from(a == Align::Signed))
                })
                .fold(w, usize::max);
            (w.max(c.chars().count() + 1), a)
        })
        .collect();
    let line = |cells: Vec<String>| -> String { cells.join(" ") };
    let mut out = vec![String::new()];
    out.push(line(
        columns
            .iter()
            .zip(&cols)
            .map(|(c, (w, _))| format!("{c:<w$}"))
            .collect(),
    ));
    out.push(line(cols.iter().map(|(w, _)| "-".repeat(*w)).collect()));
    for r in &rows {
        out.push(line(
            cols.iter()
                .enumerate()
                .map(|(i, (w, a))| match r.get(i).cloned().flatten() {
                    // A null value (u75_db2sel B).
                    None => format!("{:<w$}", "-"),
                    Some(v) => match a {
                        Align::Left => format!("{v:<w$}"),
                        Align::Right => format!("{v:>w$}"),
                        Align::Signed => {
                            let (sign, digits) = match v.strip_prefix('-') {
                                Some(d) => ('-', d.to_string()),
                                None => (' ', v.clone()),
                            };
                            let rest = w - 1;
                            format!("{sign}{digits:>rest$}")
                        }
                    },
                })
                .collect(),
        ));
    }
    out.push(String::new());
    out.push(format!("  {} RECORD(S) SELECTED.", rows.len()));
    out.push(String::new());
    out
}

/// An SQL error as the utility writes it, from as4tainr's message
/// ("SQLnnnn: text", the last such in the error): the CLI error lines
/// with its SQLSTATE and code, and the text (u75_badsel, u75_db2bad).
fn cli_error(e: &anyhow::Error) -> Vec<String> {
    let all = format!("{e:#}");
    let mut found: Option<(i64, String)> = None;
    for (i, _) in all.match_indices("SQL") {
        let id = &all[i..];
        let digits = id.get(3..7).unwrap_or("");
        if digits.len() == 4
            && digits.chars().all(|c| c.is_ascii_digit())
            && id.get(7..9) == Some(": ")
            && digits != "9010"
        {
            let text = id[9..].lines().next().unwrap_or("").to_string();
            let code = digits.parse::<i64>().unwrap_or(901);
            found = Some((code, text));
        }
    }
    let (code, text) = found.unwrap_or((901, all.lines().next().unwrap_or("").to_string()));
    let code = -code;
    vec![
        String::new(),
        " **** CLI ERROR *****".into(),
        format!(
            "         SQLSTATE: {}",
            crate::rpg::interp::exec_sql::sqlstate(code)
        ),
        format!("NATIVE ERROR CODE: {code}"),
        format!("{text} "),
    ]
}

/// Whether a statement is a query (its rows are written).
fn is_query(sql: &str) -> bool {
    let w = sql
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(')
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(w.as_str(), "SELECT" | "WITH" | "VALUES")
}

/// Run the utility for a job: options (-p, -r, -S, -t are taken and not
/// acted on; -v writes the statement first) and one statement. A query
/// writes its rows (exit status 0, 1 when there are none: u75_libinfo);
/// another statement runs as RUNSQL runs it and writes DB20000I; an
/// error writes the CLI error lines (exit status 4).
pub async fn run(job: &mut crate::commands::Job<'_>, args: &[String]) -> Output {
    let mut sql = String::new();
    let mut verbose = false;
    for a in args {
        if sql.is_empty() && a.starts_with('-') && a.len() > 1 {
            verbose |= a.contains('v');
            continue;
        }
        if !sql.is_empty() {
            sql.push(' ');
        }
        sql.push_str(a);
    }
    let sql = sql.trim().trim_end_matches(';').to_string();
    if sql.is_empty() {
        return Output {
            stdout: vec![],
            stderr: vec!["db2: give an SQL statement to run.".into()],
            status: 1,
        };
    }
    let mut stdout = Vec::new();
    if verbose {
        stdout.push(sql.clone());
    }
    if !is_query(&sql) {
        let cmd = format!("RUNSQL SQL('{}') COMMIT(*NONE)", sql.replace('\'', "''"));
        return match job.run(&cmd).await {
            Ok(_) => {
                // A table made in a library without the journal QSQJRN
                // is not journaled: the warning SQL7905, SQLSTATE 01567,
                // exit status 2 (verify75 u75_db2crt).
                if let Ok(Some(crate::ddl::Ddl::CreateTable { name, .. })) = crate::ddl::parse(&sql)
                {
                    let lib = name
                        .lib
                        .clone()
                        .or_else(|| job.curlib.clone())
                        .unwrap_or_else(|| "QGPL".into());
                    let journaled = job
                        .client
                        .query_opt(
                            "SELECT 1 FROM as4tainr.journals WHERE library = $1 AND name = 'QSQJRN'",
                            &[&lib],
                        )
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                    if !journaled {
                        stdout.extend([
                            String::new(),
                            " **** CLI ERROR *****".into(),
                            "         SQLSTATE: 01567".into(),
                            "NATIVE ERROR CODE: 7905".into(),
                            format!("{} in {lib} was made; it is not journaled. ", name.name),
                        ]);
                        return Output {
                            stdout,
                            stderr: vec![],
                            status: 2,
                        };
                    }
                }
                stdout.push("DB20000I  The SQL statement ran.".into());
                Output {
                    stdout,
                    stderr: vec![],
                    status: 0,
                }
            }
            Err(e) => {
                stdout.extend(cli_error(&e));
                Output {
                    stdout,
                    stderr: vec![],
                    status: 4,
                }
            }
        };
    }
    let libl = job.library_list();
    let r: Result<Exec> = async {
        let s = Session::open(job.client, &job.env.database_url, &libl).await?;
        s.exec(job.client, &sql).await
    }
    .await;
    match r {
        Ok(Exec::Rows {
            columns,
            rows,
            types,
        }) => {
            let columns = crate::mapepire::unnamed_columns(&sql, columns);
            let none = rows.is_empty();
            stdout.extend(table(&columns, &types, &rows));
            Output {
                stdout,
                stderr: vec![],
                status: u8::from(none),
            }
        }
        Ok(Exec::Count(..)) => {
            stdout.push("DB20000I  The SQL statement ran.".into());
            Output {
                stdout,
                stderr: vec![],
                status: 0,
            }
        }
        Err(e) => {
            stdout.extend(cli_error(&e));
            Output {
                stdout,
                stderr: vec![],
                status: 4,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pg: &str, typmod: i32) -> ColType {
        ColType {
            pg: pg.into(),
            typmod,
            db2: None,
        }
    }

    #[test]
    fn a_table_as_the_utility_writes_it() {
        // verify74 u74_basrows: BADCEN CHAR(1) is 7 wide (its name and
        // one), BALBNM CHAR(10) 10, BACCSI DECIMAL(5,0) 7 on the right,
        // BANEXS DECIMAL(11,0) 13.
        let cols: Vec<String> = ["BADCEN", "BALBNM", "BACCSI", "BANEXS"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let types = [
            t("bpchar", 5),
            t("bpchar", 14),
            t("numeric", (5 << 16) + 4),
            t("numeric", (11 << 16) + 4),
        ];
        let rows = vec![vec![
            Some("1".to_string()),
            Some("AS4TRAINR1".to_string()),
            Some("273".to_string()),
            Some("1".to_string()),
        ]];
        assert_eq!(
            table(&cols, &types, &rows),
            [
                "",
                "BADCEN  BALBNM     BACCSI  BANEXS       ",
                "------- ---------- ------- -------------",
                "1       AS4TRAINR1     273             1",
                "",
                "  1 RECORD(S) SELECTED.",
                "",
            ]
        );
    }

    #[test]
    fn columns_of_other_types() {
        // verify75 u75_db2sel: INTEGER, VARCHAR(5) null, DATE, TIMESTAMP,
        // SMALLINT, BIGINT, DOUBLE, TIME, DECIMAL(7,2), an unnamed
        // INTEGER column.
        let cols: Vec<String> = ["A", "B", "C", "D", "E", "F", "G", "H", "I", "00010"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let types = [
            t("int4", -1),
            t("varchar", 9),
            t("date", -1),
            t("timestamp", -1),
            t("int2", -1),
            t("int8", -1),
            t("float8", -1),
            t("time", -1),
            t("numeric", (7 << 16) + 2 + 4),
            t("int4", -1),
        ];
        let v = |s: &str| Some(s.to_string());
        let rows = vec![vec![
            v("1"),
            None,
            v("2026-11-10"),
            v("2026-11-10 12:00:00"),
            v("2"),
            v("3"),
            v("1.5"),
            v("12:30:00"),
            v("-12.50"),
            v("1"),
        ]];
        let out = table(&cols, &types, &rows);
        assert_eq!(out[1], "A           B     C          D                          E      F                    G                        H        I         00010      ");
        assert_eq!(out[3], "          1 -     2026-11-10 2026-11-10-12.00.00.000000      2                    3 1.5                      12.30.00 -   12.50           1");
    }
}
