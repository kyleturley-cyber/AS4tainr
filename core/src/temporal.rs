//! System-period temporal tables (Db2 for i SQL Programming, "Temporal
//! tables"; SQL Reference, CREATE TABLE PERIOD SYSTEM_TIME, ALTER TABLE
//! ADD VERSIONING): a table whose ROW BEGIN, ROW END and TRANSACTION
//! START ID columns the database sets (catalog.rs generated_columns), and
//! whose old row versions go to a history table when rows are updated or
//! deleted (the trigger function `as4tainr.versioning`).

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::{ident, pg_name};

/// The catalog table and the trigger function, made at catalog
/// initialization.
pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.temporal_tables (
    library      text NOT NULL,
    file         text NOT NULL,
    begin_col    text NOT NULL,
    end_col      text NOT NULL,
    history_lib  text,
    history_file text,
    extra_row    boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, file)
);
-- Old versions into the history table: the row as it was, ending now; a
-- delete ON DELETE ADD EXTRA ROW adds one more, beginning and ending now,
-- with the auditing columns of the delete (DATA CHANGE OPERATION D).
-- Arguments: history table (quoted, qualified), its schema and name, the
-- row begin and row end columns, Y for the extra row, then (column, what
-- it holds) pairs for the auditing columns.
CREATE OR REPLACE FUNCTION as4tainr.versioning() RETURNS trigger
LANGUAGE plpgsql AS $f$
DECLARE
  hist text := TG_ARGV[0];
  bcol text := TG_ARGV[3];
  ecol text := TG_ARGV[4];
  ts text := to_char(now(), 'YYYY-MM-DD HH24:MI:SS.US');
  cols text;
  j jsonb;
  i int;
  v text;
BEGIN
  SELECT string_agg(quote_ident(column_name), ', ' ORDER BY ordinal_position) INTO cols
    FROM information_schema.columns
   WHERE table_schema = TG_ARGV[1] AND table_name = TG_ARGV[2] AND column_name <> 'rrn';
  j := to_jsonb(OLD) - 'rrn' || jsonb_build_object(ecol, ts);
  EXECUTE format('INSERT INTO %s (%s) SELECT %s FROM jsonb_populate_record(NULL::%s, $1)',
                 hist, cols, cols, hist) USING j;
  IF TG_OP = 'DELETE' AND TG_ARGV[5] = 'Y' THEN
    j := to_jsonb(OLD) - 'rrn' || jsonb_build_object(bcol, ts, ecol, ts);
    i := 6;
    WHILE i + 1 < TG_NARGS LOOP
      v := CASE TG_ARGV[i + 1]
        WHEN 'DCO' THEN 'D'
        WHEN 'TSID' THEN ts
        WHEN 'JOB' THEN COALESCE(NULLIF(current_setting('as4tainr.job', true), ''),
                                 NULLIF(current_setting('application_name'), ''))
        WHEN 'USER' THEN COALESCE(NULLIF(current_setting('as4tainr.user', true), ''),
                                  NULLIF(split_part(current_setting('application_name'), '/', 2), ''),
                                  upper(session_user))
      END;
      IF v IS NOT NULL THEN
        j := j || jsonb_build_object(TG_ARGV[i], v);
      END IF;
      i := i + 2;
    END LOOP;
    EXECUTE format('INSERT INTO %s (%s) SELECT %s FROM jsonb_populate_record(NULL::%s, $1)',
                   hist, cols, cols, hist) USING j;
  END IF;
  RETURN NULL;
END $f$;
"#;

/// PERIOD SYSTEM_TIME of a table just made: its row begin and row end
/// columns (system names).
pub async fn set_period(
    client: &Client,
    lib: &str,
    file: &str,
    begin: &str,
    end: &str,
) -> Result<()> {
    client
        .execute(
            "INSERT INTO as4tainr.temporal_tables (library, file, begin_col, end_col)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (library, file) DO UPDATE SET begin_col = $3, end_col = $4,
                history_lib = NULL, history_file = NULL",
            &[&lib, &file, &begin, &end],
        )
        .await?;
    Ok(())
}

/// The table's period columns and history table, when it has a period.
pub async fn period(
    client: &Client,
    lib: &str,
    file: &str,
) -> Result<Option<(String, String, Option<(String, String)>)>> {
    Ok(client
        .query_opt(
            "SELECT begin_col, end_col, history_lib, history_file FROM as4tainr.temporal_tables
              WHERE library = $1 AND file = $2",
            &[&lib, &file],
        )
        .await?
        .map(|r| {
            let h: Option<String> = r.get(2);
            let f: Option<String> = r.get(3);
            (r.get(0), r.get(1), h.zip(f))
        }))
}

/// ADD VERSIONING USE HISTORY TABLE: the history table keeps plain
/// columns (its generated attributes and their trigger go: the database
/// sets nothing in it), and the table's trigger writes old versions there.
pub async fn add_versioning(
    client: &Client,
    lib: &str,
    file: &str,
    fields: &[crate::dds::FieldDef],
    hlib: &str,
    hfile: &str,
    extra_row: bool,
) -> Result<()> {
    let Some((b, e, current)) = period(client, lib, file).await? else {
        bail!("SQL0084: Table {file} in {lib} has no PERIOD SYSTEM_TIME.");
    };
    if current.is_some() {
        bail!("SQL0084: Table {file} in {lib} is already versioned.");
    }
    let htable = format!("{}.{}", ident(hlib), ident(hfile));
    client
        .execute(
            "UPDATE as4tainr.file_fields SET generated = NULL, identity = NULL
              WHERE library = $1 AND file = $2",
            &[&hlib, &hfile],
        )
        .await?;
    client
        .batch_execute(&format!(
            "DROP TRIGGER IF EXISTS as4tainr_generated ON {htable}"
        ))
        .await?;
    let column = |n: &str| {
        fields
            .iter()
            .find(|f| f.name == n)
            .map(|f| f.column().trim_matches('"').replace("\"\"", "\""))
            .unwrap_or_else(|| pg_name(n))
    };
    let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut args = vec![
        lit(&htable),
        lit(&pg_name(hlib)),
        lit(&pg_name(hfile)),
        lit(&column(&b)),
        lit(&column(&e)),
        lit(if extra_row { "Y" } else { "N" }),
    ];
    for f in fields {
        if let Some(g) = f.generated {
            use crate::dds::Generated::*;
            if matches!(
                g,
                DataChangeOperation | JobName | SessionUser | TransactionStartId
            ) {
                args.push(lit(&f.column().trim_matches('"').replace("\"\"", "\"")));
                args.push(lit(g.code()));
            }
        }
    }
    let table = format!("{}.{}", ident(lib), ident(file));
    client
        .batch_execute(&format!(
            "DROP TRIGGER IF EXISTS as4tainr_versioning ON {table};
             CREATE TRIGGER as4tainr_versioning AFTER UPDATE OR DELETE ON {table}
             FOR EACH ROW EXECUTE FUNCTION as4tainr.versioning({})",
            args.join(", ")
        ))
        .await?;
    client
        .execute(
            "UPDATE as4tainr.temporal_tables SET history_lib = $3, history_file = $4, extra_row = $5
              WHERE library = $1 AND file = $2",
            &[&lib, &file, &hlib, &hfile, &extra_row],
        )
        .await?;
    Ok(())
}

/// DROP VERSIONING: the history table stays, no longer written.
pub async fn drop_versioning(client: &Client, lib: &str, file: &str) -> Result<()> {
    if period(client, lib, file).await?.and_then(|p| p.2).is_none() {
        bail!("SQL0084: Table {file} in {lib} is not versioned.");
    }
    let table = format!("{}.{}", ident(lib), ident(file));
    client
        .batch_execute(&format!(
            "DROP TRIGGER IF EXISTS as4tainr_versioning ON {table}"
        ))
        .await?;
    client
        .execute(
            "UPDATE as4tainr.temporal_tables SET history_lib = NULL, history_file = NULL, extra_row = false
              WHERE library = $1 AND file = $2",
            &[&lib, &file],
        )
        .await?;
    Ok(())
}

/// Words that end a period specification's expression or start what
/// follows a table reference.
/// Labeled durations (Db2 for i SQL Reference, "Datetime arithmetic").
const DURATIONS: &[&str] = &[
    "YEAR",
    "YEARS",
    "MONTH",
    "MONTHS",
    "DAY",
    "DAYS",
    "HOUR",
    "HOURS",
    "MINUTE",
    "MINUTES",
    "SECOND",
    "SECONDS",
    "MICROSECOND",
    "MICROSECONDS",
];

const STOP: &[&str] = &[
    "AS",
    "WHERE",
    "ORDER",
    "GROUP",
    "HAVING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "ON",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "FETCH",
    "LIMIT",
    "OFFSET",
    "WITH",
    "FOR",
    "USING",
    "WINDOW",
];

/// A table reference with FOR SYSTEM_TIME AS OF t, FROM t1 TO t2 or
/// BETWEEN t1 AND t2 (Db2 for i SQL Reference, table-reference,
/// period-specification): the rows of the table and of its history
/// table whose period covers the time (AS OF: begin <= t < end), meets
/// [t1, t2) (FROM ... TO: begin < t2 and end > t1) or [t1, t2] (BETWEEN:
/// begin <= t2 and end > t1), as a nested table expression named for the
/// table unless it has a correlation name. Done on the statement as
/// written, before it is translated.
pub async fn rewrite_system_time(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    if !sql.to_ascii_uppercase().contains("SYSTEM_TIME") {
        return Ok(sql.to_string());
    }
    use crate::sql::Tok;
    let toks = crate::sql::tokenize(sql)?;
    let text = |t: &Tok| match t {
        Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
        Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
        Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
    };
    let up = |t: Option<&Tok>| match t {
        Some(Tok::Word(w)) => w.to_ascii_uppercase(),
        _ => String::new(),
    };
    // Significant tokens (no spaces), with their positions.
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&i| !matches!(toks[i], Tok::Space(_)))
        .collect();
    let mut out = String::new();
    let mut copied = 0; // tokens before this are in `out`
    let mut k = 0;
    while k < sig.len() {
        if !(up(toks.get(sig[k])) == "FOR"
            && up(sig.get(k + 1).map(|&i| &toks[i])) == "SYSTEM_TIME")
        {
            k += 1;
            continue;
        }
        // The table name before FOR: name, or lib.name / lib/name.
        let Some(&ni) = k.checked_sub(1).and_then(|j| sig.get(j)) else {
            bail!("SQL0104: a table is expected before FOR SYSTEM_TIME.");
        };
        let name_of = |t: &Tok| match t {
            Tok::Word(w) => Some(w.to_ascii_uppercase()),
            Tok::Quoted(q) => Some(q.clone()),
            _ => None,
        };
        let Some(name) = name_of(&toks[ni]) else {
            bail!("SQL0104: a table is expected before FOR SYSTEM_TIME.");
        };
        let mut start = ni;
        let mut lib: Option<String> = None;
        if k >= 3 {
            let sep = &toks[sig[k - 2]];
            if matches!(sep, Tok::Punct(p) if p == "." || p == "/") {
                if let Some(l) = name_of(&toks[sig[k - 3]]) {
                    lib = Some(l);
                    start = sig[k - 3];
                }
            }
        }
        // The period specification.
        let mut j = k + 2;
        let kind = up(sig.get(j).map(|&i| &toks[i]));
        j += 1;
        if kind == "AS" {
            if up(sig.get(j).map(|&i| &toks[i])) != "OF" {
                bail!("SQL0104: OF expected after FOR SYSTEM_TIME AS.");
            }
            j += 1;
        } else if kind != "FROM" && kind != "BETWEEN" {
            bail!("SQL0104: AS OF, FROM or BETWEEN expected after FOR SYSTEM_TIME.");
        }
        // An expression: tokens up to a stop word, a comma or a closing
        // parenthesis at its own level (`until` ends the first one of
        // FROM ... TO and BETWEEN ... AND).
        let expr = |j: &mut usize, until: &str| -> String {
            let mut depth = 0i32;
            let first = *j;
            while *j < sig.len() {
                let t = &toks[sig[*j]];
                let w = up(Some(t));
                // A name right after a complete operand (a constant, a
                // parenthesis): a correlation name, not more expression
                // (durations like 1 DAY excepted).
                let after_operand = *j > first
                    && matches!(&toks[sig[*j - 1]], Tok::Str(_) | Tok::Number(_))
                    || (*j > first && matches!(&toks[sig[*j - 1]], Tok::Punct(p) if p == ")"));
                if depth == 0
                    && after_operand
                    && matches!(t, Tok::Word(_) | Tok::Quoted(_))
                    && !DURATIONS.contains(&w.as_str())
                    && w != until
                    && !matches!(w.as_str(), "AND" | "OR" | "CONCAT")
                {
                    break;
                }
                if depth == 0
                    && ((!until.is_empty() && w == until)
                        || (until.is_empty() && STOP.contains(&w.as_str()))
                        || matches!(t, Tok::Punct(p) if p == "," || p == ")" || p == ";"))
                {
                    break;
                }
                if matches!(t, Tok::Punct(p) if p == "(") {
                    depth += 1;
                } else if matches!(t, Tok::Punct(p) if p == ")") {
                    depth -= 1;
                }
                *j += 1;
            }
            if *j == first {
                return String::new();
            }
            (sig[first]..sig[*j - 1] + 1)
                .map(|i| text(&toks[i]))
                .collect()
        };
        let (t1, t2) = match kind.as_str() {
            "AS" => {
                let e = expr(&mut j, "");
                (e.clone(), e)
            }
            "FROM" => {
                let a = expr(&mut j, "TO");
                j += 1;
                (a, expr(&mut j, ""))
            }
            _ => {
                let a = expr(&mut j, "AND");
                j += 1;
                (a, expr(&mut j, ""))
            }
        };
        if t1.trim().is_empty() || t2.trim().is_empty() {
            bail!("SQL0104: a time is expected in FOR SYSTEM_TIME.");
        }
        // The table and its history table.
        let libs: Vec<String> = match &lib {
            Some(l) => vec![l.clone()],
            None => libl.to_vec(),
        };
        let file = crate::catalog::resolve_file(client, &libs, &name)
            .await
            .map_err(|_| anyhow::anyhow!("SQL0204: {name} not found."))?;
        let Some((b, e, history)) = period(client, &file.library, &file.file.name).await? else {
            bail!(
                "SQL0084: {name} is not a system-period temporal table: FOR SYSTEM_TIME needs one."
            );
        };
        let col = |n: &str| {
            file.file
                .fields
                .iter()
                .find(|f| f.name == n)
                .map(|f| f.alias.clone().unwrap_or_else(|| f.name.clone()))
                .unwrap_or_else(|| n.to_string())
        };
        // The times must not contain a column reference or a scalar
        // fullselect (SQL Reference, table-reference, period-specification;
        // PUB400 2026-10-26, verify2 w_asof: AS OF (SELECT MIN(...)) fails).
        for v in [&t1, &t2] {
            for t in crate::sql::tokenize(v)? {
                let Tok::Word(w) = t else { continue };
                let w = w.to_ascii_uppercase();
                if w == "SELECT" {
                    bail!("SQL0104: The time in FOR SYSTEM_TIME cannot be a fullselect.");
                }
                if file
                    .file
                    .fields
                    .iter()
                    .any(|f| f.name == w || f.alias.as_deref() == Some(w.as_str()))
                {
                    bail!("SQL0104: The time in FOR SYSTEM_TIME cannot name column {w}.");
                }
            }
        }
        let (bc, ec) = (col(&b), col(&e));
        // A string is a timestamp in any of Db2's formats (TIMESTAMP()).
        let (t1, t2) = (format!("TIMESTAMP({t1})"), format!("TIMESTAMP({t2})"));
        let cond = match kind.as_str() {
            "AS" => format!("{bc} <= {t1} AND {ec} > {t1}"),
            "FROM" => format!("{bc} < {t2} AND {ec} > {t1}"),
            _ => format!("{bc} <= {t2} AND {ec} > {t1}"),
        };
        let cols: Vec<String> = file
            .file
            .fields
            .iter()
            .map(|f| f.alias.clone().unwrap_or_else(|| f.name.clone()))
            .collect();
        let cols = cols.join(", ");
        let mut derived = format!(
            "(SELECT {cols} FROM {}.{} WHERE {cond}",
            file.library, file.file.name
        );
        if let Some((hl, hf)) = history {
            derived.push_str(&format!(
                " UNION ALL SELECT {cols} FROM {hl}.{hf} WHERE {cond}"
            ));
        }
        derived.push(')');
        // A correlation name after the specification stays; else the
        // table's name.
        let next = up(sig.get(j).map(|&i| &toks[i]));
        let has_alias = next == "AS"
            || (matches!(
                sig.get(j).map(|&i| &toks[i]),
                Some(Tok::Word(_) | Tok::Quoted(_))
            ) && !STOP.contains(&next.as_str()));
        if !has_alias {
            derived.push_str(&format!(" AS {name}"));
        }
        out.extend((copied..start).map(|i| text(&toks[i])));
        out.push_str(&derived);
        copied = if j < sig.len() { sig[j] } else { toks.len() };
        // Keep the space before what follows.
        if j < sig.len() && copied > 0 && matches!(toks[copied - 1], Tok::Space(_)) {
            copied -= 1;
        }
        k = j;
    }
    out.extend((copied..toks.len()).map(|i| text(&toks[i])));
    Ok(out)
}

/// What a statement becomes before it is translated: `*` without the
/// IMPLICITLY HIDDEN columns (`expand_hidden`), then FOR SYSTEM_TIME
/// table references (`rewrite_system_time`).
pub async fn rewrite(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    let sql = expand_hidden(client, libl, sql).await?;
    let sql = long_timestamp_chars(client, libl, &sql).await?;
    rewrite_system_time(client, libl, &sql).await
}

/// CHAR(column) of a TIMESTAMP(p) column with more than 6 fractional
/// digits: all p digits (PUB400 2026-10-26, verify2 w_cur: CHAR of a
/// TIMESTAMP(12) row end is 9999-12-30-00.00.00.000000000000).
/// PostgreSQL keeps 6, so the rest are zeros. The column is found by
/// name among the library list's files (one length for the name).
async fn long_timestamp_chars(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    if !sql.to_ascii_uppercase().contains("CHAR") || libl.is_empty() {
        return Ok(sql.to_string());
    }
    use crate::sql::Tok;
    let rows = client
        .query(
            "SELECT upper(coalesce(alias, name)), min(length), max(length) FROM as4tainr.file_fields
              WHERE library = ANY($1) AND data_type = 'Z' AND length > 26
              GROUP BY 1",
            &[&libl],
        )
        .await?;
    if rows.is_empty() {
        return Ok(sql.to_string());
    }
    let long: std::collections::HashMap<String, i32> = rows
        .iter()
        .filter(|r| r.get::<_, i32>(1) == r.get::<_, i32>(2))
        .map(|r| (r.get::<_, String>(0), r.get::<_, i32>(1)))
        .collect();
    let toks = crate::sql::tokenize(sql)?;
    let text = |t: &Tok| match t {
        Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
        Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
        Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
    };
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&i| !matches!(toks[i], Tok::Space(_)))
        .collect();
    let word = |k: usize| match sig.get(k).map(|&i| &toks[i]) {
        Some(Tok::Word(w)) => w.to_ascii_uppercase(),
        _ => String::new(),
    };
    let punct =
        |k: usize, p: &str| matches!(sig.get(k).map(|&i| &toks[i]), Some(Tok::Punct(q)) if q == p);
    let mut out = String::new();
    let mut at = 0;
    let mut k = 0;
    while k < sig.len() {
        // CHAR ( name ) or CHAR ( q . name ), not after a dot.
        if word(k) == "CHAR" && punct(k + 1, "(") && !(k > 0 && punct(k - 1, ".")) {
            let (name_at, close) = if punct(k + 3, ".") {
                (k + 4, k + 5)
            } else {
                (k + 2, k + 3)
            };
            if punct(close, ")") {
                if let Some(len) = long.get(&word(name_at)) {
                    out.extend((at..sig[k]).map(|i| text(&toks[i])));
                    let inner: String = (sig[k]..=sig[close]).map(|i| text(&toks[i])).collect();
                    out.push_str(&format!("RPAD({inner}, {len}, '0')"));
                    at = sig[close] + 1;
                    k = close + 1;
                    continue;
                }
            }
        }
        k += 1;
    }
    out.extend((at..toks.len()).map(|i| text(&toks[i])));
    Ok(out)
}

/// Names (system and SQL) of the files that have IMPLICITLY HIDDEN
/// columns.
async fn hidden_names(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT DISTINCT f.name, COALESCE(f.sql_name, '')
               FROM as4tainr.file_fields ff
               JOIN as4tainr.files f ON f.library = ff.library AND f.name = ff.file
              WHERE ff.hidden",
            &[],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let a: String = r.get(0);
        let b: String = r.get(1);
        out.push(a.to_ascii_uppercase());
        if !b.is_empty() {
            out.push(b.to_ascii_uppercase());
        }
    }
    Ok(out)
}

/// Words that end a FROM clause (at its own level).
const FROM_END: &[&str] = &[
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "FETCH",
    "LIMIT",
    "OFFSET",
    "WINDOW",
    "OPTIMIZE",
];

/// Words that are not a correlation name after a table name.
const NOT_CORRELATION: &[&str] = &[
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "FETCH",
    "LIMIT",
    "OFFSET",
    "WINDOW",
    "OPTIMIZE",
    "FOR",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "EXCEPTION",
    "ON",
    "USING",
    "WITH",
];

/// `*` and `name.*` in a select list, for tables with IMPLICITLY HIDDEN
/// columns: the columns that are not hidden, by name (Db2 for i SQL
/// Reference, CREATE TABLE, IMPLICITLY HIDDEN: "not visible in SQL
/// statements unless referred to explicitly by name"; PUB400 2026-10-26,
/// verify2 w_star: a table made from SELECT * has no hidden columns).
/// Only select lists whose table references are tables or views by name
/// are changed (a nested table expression or a common table expression
/// keeps `*`: fidelity-gaps).
pub async fn expand_hidden(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    if !sql.contains('*') {
        return Ok(sql.to_string());
    }
    use crate::sql::Tok;
    let names = hidden_names(client).await?;
    if names.is_empty() {
        return Ok(sql.to_string());
    }
    let toks = crate::sql::tokenize(sql)?;
    if !toks.iter().any(|t| match t {
        Tok::Word(w) => names.contains(&w.to_ascii_uppercase()),
        Tok::Quoted(q) => names.contains(q),
        _ => false,
    }) {
        return Ok(sql.to_string());
    }
    let text = |t: &Tok| match t {
        Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
        Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
        Tok::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
    };
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&i| !matches!(toks[i], Tok::Space(_)))
        .collect();
    let n = sig.len();
    let up = |k: usize| match toks.get(*sig.get(k).unwrap_or(&usize::MAX)) {
        Some(Tok::Word(w)) => w.to_ascii_uppercase(),
        _ => String::new(),
    };
    let punct = |k: usize, p: &str| matches!(sig.get(k).and_then(|&i| toks.get(i)), Some(Tok::Punct(q)) if q == p);
    let name = |k: usize| match sig.get(k).and_then(|&i| toks.get(i)) {
        Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
        Some(Tok::Quoted(q)) => Some(q.clone()),
        _ => None,
    };
    // The parenthesis depth of each significant token.
    let mut depth = vec![0i32; n];
    let mut d = 0;
    for (k, dk) in depth.iter_mut().enumerate() {
        if punct(k, ")") {
            d -= 1;
        }
        *dk = d;
        if punct(k, "(") {
            d += 1;
        }
    }
    // (first, last significant token, replacement)
    let mut repl: Vec<(usize, usize, String)> = Vec::new();
    for k in 0..n {
        if up(k) != "SELECT" {
            continue;
        }
        let d = depth[k];
        let mut f = k + 1;
        while f < n && depth[f] >= d && !(depth[f] == d && up(f) == "FROM") {
            f += 1;
        }
        if f >= n || depth[f] != d || up(f) != "FROM" {
            continue;
        }
        // `*` and `q.*` items of the select list.
        let item_start = |m: usize| {
            m == k + 1
                || punct(m - 1, ",")
                || (matches!(up(m - 1).as_str(), "DISTINCT" | "ALL") && m - 1 == k + 1)
        };
        let mut stars: Vec<(usize, usize, Option<String>)> = Vec::new();
        for (m, &dm) in depth.iter().enumerate().take(f).skip(k + 1) {
            if dm != d || !punct(m, "*") || !(m + 1 == f || punct(m + 1, ",")) {
                continue;
            }
            if item_start(m) {
                stars.push((m, m, None));
            } else if m >= k + 3 && punct(m - 1, ".") && item_start(m - 2) {
                if let Some(q) = name(m - 2) {
                    stars.push((m - 2, m, Some(q)));
                }
            }
        }
        if stars.is_empty() {
            continue;
        }
        // The table references: (library, name, correlation name).
        let mut refs: Vec<(Option<String>, String, Option<String>)> = Vec::new();
        let mut known = true;
        let mut m = f + 1;
        'refs: while m < n && depth[m] >= d {
            let Some(mut tname) = name(m).filter(|_| depth[m] == d) else {
                known = false;
                break;
            };
            m += 1;
            let mut lib = None;
            if (punct(m, ".") || punct(m, "/")) && name(m + 1).is_some() {
                lib = Some(tname);
                tname = name(m + 1).unwrap();
                m += 2;
            }
            if punct(m, "(") {
                // A table function.
                known = false;
                break;
            }
            let mut corr = None;
            if up(m) == "FOR" && up(m + 1) == "SYSTEM_TIME" {
                // A period specification: its correlation name, if any,
                // is not looked for (an unqualified `*` still works).
                m += 2;
            } else if up(m) == "AS" {
                corr = name(m + 1);
                m += 2;
            } else if let Some(c) = name(m) {
                if !NOT_CORRELATION.contains(&c.as_str()) {
                    corr = Some(c);
                    m += 1;
                }
            }
            refs.push((lib, tname, corr));
            // On to the next table reference (a comma or JOIN), past ON
            // conditions and period specifications.
            while m < n && depth[m] >= d {
                if depth[m] == d {
                    if punct(m, ",") || up(m) == "JOIN" {
                        m += 1;
                        continue 'refs;
                    }
                    if FROM_END.contains(&up(m).as_str())
                        || punct(m, ";")
                        || (up(m) == "FOR" && up(m + 1) != "SYSTEM_TIME")
                    {
                        break 'refs;
                    }
                }
                m += 1;
            }
        }
        if refs.is_empty() {
            continue;
        }
        // The columns of each reference (None: not a file by name, e.g.
        // a common table expression).
        let mut cols: Vec<Option<(Vec<String>, bool)>> = Vec::new();
        for (lib, tname, _) in &refs {
            let q = match lib {
                Some(l) => format!("{l}/{tname}"),
                None => tname.clone(),
            };
            cols.push(
                crate::catalog::resolve_sql_file(client, libl, &q)
                    .await
                    .ok()
                    .map(|f| {
                        let hidden = f.file.fields.iter().any(|x| x.hidden);
                        let c = f
                            .file
                            .fields
                            .iter()
                            .filter(|x| !x.hidden)
                            .map(|x| x.alias.clone().unwrap_or_else(|| x.name.clone()))
                            .collect();
                        (c, hidden)
                    }),
            );
        }
        let exposed = |r: &(Option<String>, String, Option<String>)| {
            r.2.clone().unwrap_or_else(|| r.1.clone())
        };
        for (first, last, q) in stars {
            let s = match q {
                None => {
                    if !known || cols.iter().any(|c| c.is_none()) {
                        continue;
                    }
                    if !cols.iter().any(|c| c.as_ref().unwrap().1) {
                        continue;
                    }
                    if refs.len() == 1 {
                        cols[0].as_ref().unwrap().0.join(", ")
                    } else {
                        refs.iter()
                            .zip(&cols)
                            .flat_map(|(r, c)| {
                                let e = exposed(r);
                                c.as_ref()
                                    .unwrap()
                                    .0
                                    .iter()
                                    .map(move |x| format!("{e}.{x}"))
                                    .collect::<Vec<_>>()
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                }
                Some(q) => {
                    let Some((r, Some((c, true)))) =
                        refs.iter().zip(&cols).find(|(r, _)| exposed(r) == q)
                    else {
                        continue;
                    };
                    let e = exposed(r);
                    c.iter()
                        .map(|x| format!("{e}.{x}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            if !s.is_empty() {
                repl.push((first, last, s));
            }
        }
    }
    if repl.is_empty() {
        return Ok(sql.to_string());
    }
    repl.sort_by_key(|r| r.0);
    let mut out = String::new();
    let mut at = 0;
    for (first, last, s) in repl {
        out.extend((at..sig[first]).map(|i| text(&toks[i])));
        out.push_str(&s);
        at = sig[last] + 1;
    }
    out.extend((at..toks.len()).map(|i| text(&toks[i])));
    Ok(out)
}
