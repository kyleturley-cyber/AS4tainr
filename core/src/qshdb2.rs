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
use tokio_postgres::Client;

/// What the utility wrote and its exit status.
pub struct Output {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub status: u8,
}

/// A column's width for its type (and whether it is a number).
fn width(t: &ColType) -> (usize, bool) {
    let m = t.typmod;
    match t.pg.as_str() {
        "bpchar" | "varchar" | "text" if m > 4 => ((m - 4) as usize, false),
        "numeric" if m > 4 => ((((m - 4) >> 16) & 0xffff) as usize + 2, true),
        "numeric" => (33, true),
        "int2" => (6, true),
        "int4" => (11, true),
        "int8" => (20, true),
        "float4" | "float8" => (24, true),
        "date" => (10, false),
        "time" => (8, false),
        "timestamp" | "timestamptz" => (26, false),
        _ => (1, false),
    }
}

/// The table of a query's rows (u74_exprows).
pub fn table(columns: &[String], types: &[ColType], rows: &[Vec<Option<String>>]) -> Vec<String> {
    let cols: Vec<(usize, bool)> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (w, num) = types.get(i).map(width).unwrap_or((1, false));
            let w = rows
                .iter()
                .map(|r| {
                    r.get(i)
                        .and_then(|v| v.as_ref())
                        .map_or(1, |v| v.chars().count())
                })
                .fold(w, usize::max);
            (w.max(c.chars().count() + 1), num)
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
    for r in rows {
        out.push(line(
            cols.iter()
                .enumerate()
                .map(|(i, (w, num))| {
                    let v = r.get(i).cloned().flatten().unwrap_or_else(|| "-".into());
                    if *num {
                        format!("{v:>w$}")
                    } else {
                        format!("{v:<w$}")
                    }
                })
                .collect(),
        ));
    }
    out.push(String::new());
    out.push(format!("  {} RECORD(S) SELECTED.", rows.len()));
    out.push(String::new());
    out
}

/// Run the utility: options (-p, -r, -S, -t, -v are taken and not acted
/// on) and one statement.
pub async fn run(client: &Client, database_url: &str, libl: &[String], args: &[String]) -> Output {
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
    let r: Result<Exec> = async {
        let s = Session::open(client, database_url, libl).await?;
        s.exec(client, &sql).await
    }
    .await;
    match r {
        Ok(Exec::Rows {
            columns,
            rows,
            types,
        }) => {
            stdout.extend(table(&columns, &types, &rows));
            Output {
                stdout,
                stderr: vec![],
                status: 0,
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
            stdout.push(format!("{e:#}"));
            Output {
                stdout,
                stderr: vec![],
                status: 1,
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
}
