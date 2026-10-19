//! SQL in the DB2 for i dialect, run on PostgreSQL.
//!
//! Written from IBM's public DB2 for i SQL reference. A statement is
//! translated, then run in its own transaction on a separate connection
//! that logs in as the `ibmish_sql` role. That role is not a superuser and
//! can use the library schemas and nothing else (not the `ibmish`
//! catalog, where password hashes live); its password is random and kept
//! in the catalog. Because the connection's own login is the restricted
//! role, a statement has no more powerful role to switch to. The job's
//! library list is the search path. Supported:
//!
//! - SELECT, WITH, VALUES, INSERT, UPDATE, DELETE; one statement at a time
//! - system naming `LIB/FILE` (when LIB is a library) and SQL naming
//!   `LIB.FILE`; unqualified names are found through the library list
//! - `CURRENT DATE`, `CURRENT TIME`, `CURRENT TIMESTAMP`, the `CONCAT`
//!   operator, `UCASE`/`LCASE`, `RRN(t)`, `SYSIBM.SYSDUMMY1`, and the
//!   `WITH NC|UR|CS|RS|RR` and `OPTIMIZE FOR n ROWS` clauses (accepted and
//!   ignored)
//! - `INSERT INTO file VALUES (...)` without a column list fills the
//!   file's fields in order
//! - the relative record number column is hidden from results, as on
//!   IBM i, unless the statement asks for `RRN(...)`
//!
//! See docs/sql.md for what is not supported yet.

use anyhow::{anyhow, bail, Result};
use tokio_postgres::{Client, SimpleQueryMessage};

use crate::catalog::{self, ident};

/// The PostgreSQL role user SQL runs as.
pub const SQL_ROLE: &str = "ibmish_sql";

/// Create the SQL login role with a random password (made once, then
/// kept, so processes starting together agree on it) and give it the
/// library schemas (idempotent). Called from `catalog::init`. Fails if
/// the database user may not create roles; SQL statements are then
/// refused.
pub async fn init(client: &Client) -> Result<()> {
    use argon2::password_hash::{rand_core::OsRng, SaltString};
    client
        .batch_execute(&format!(
            "DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{SQL_ROLE}') THEN
                 CREATE ROLE {SQL_ROLE};
               END IF;
             END $$;
             ALTER ROLE {SQL_ROLE} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
             ALTER DEFAULT PRIVILEGES GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {SQL_ROLE};
             ALTER DEFAULT PRIVILEGES GRANT USAGE, SELECT ON SEQUENCES TO {SQL_ROLE};
             REVOKE ALL ON SCHEMA ibmish FROM {SQL_ROLE};
             CREATE TABLE IF NOT EXISTS ibmish.sql_login (id int PRIMARY KEY, password text NOT NULL);"
        ))
        .await?;
    let fresh = SaltString::generate(&mut OsRng).to_string();
    client
        .execute(
            "INSERT INTO ibmish.sql_login VALUES (1, $1) ON CONFLICT (id) DO NOTHING",
            &[&fresh],
        )
        .await?;
    let password: String = client
        .query_one("SELECT password FROM ibmish.sql_login WHERE id = 1", &[])
        .await?
        .get(0);
    // A random base64 string: no quotes to escape.
    if !password
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/".contains(c))
    {
        bail!("the stored SQL password has unexpected characters");
    }
    client
        .batch_execute(&format!("ALTER ROLE {SQL_ROLE} PASSWORD '{password}'"))
        .await?;
    for lib in catalog::list_libraries(client).await? {
        grant_library(client, &lib).await?;
    }
    Ok(())
}

/// A connection that logs in as the SQL role, to the same database.
async fn connect_as_sql_role(client: &Client, database_url: &str) -> Result<Client> {
    let password: String = client
        .query_opt("SELECT password FROM ibmish.sql_login WHERE id = 1", &[])
        .await?
        .map(|r| r.get(0))
        .ok_or_else(|| {
            anyhow!(
                "SQL is not available: run `ibmish init` as a database user who may create roles."
            )
        })?;
    let mut cfg: tokio_postgres::Config = database_url.parse()?;
    cfg.user(SQL_ROLE);
    cfg.password(password);
    let (c, conn) = cfg
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|e| anyhow!("SQL is not available: cannot log in as {SQL_ROLE} ({e})."))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(c)
}

/// Let the SQL role use a library's schema and the files already in it.
pub async fn grant_library(client: &Client, library: &str) -> Result<()> {
    let schema = ident(library);
    client
        .batch_execute(&format!(
            "GRANT USAGE ON SCHEMA {schema} TO {SQL_ROLE};
             GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {schema} TO {SQL_ROLE};
             GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA {schema} TO {SQL_ROLE};"
        ))
        .await?;
    Ok(())
}

// ------------------------------------------------------------ tokens

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Space(String),
    /// A word: identifier or keyword (as written).
    Word(String),
    /// 'string'
    Str(String),
    /// "quoted identifier" (without the quotes)
    Quoted(String),
    Number(String),
    /// Any other character, or `||`.
    Punct(String),
}

fn tokenize(sql: &str) -> Result<Vec<Tok>> {
    let cs: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            let s = i;
            while i < cs.len() && cs[i].is_whitespace() {
                i += 1;
            }
            out.push(Tok::Space(cs[s..i].iter().collect()));
        } else if c == '-' && cs.get(i + 1) == Some(&'-') {
            // Comment to end of line.
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            out.push(Tok::Space(" ".into()));
        } else if c == '\'' || c == '"' {
            let mut v = String::new();
            i += 1;
            loop {
                match cs.get(i) {
                    None => bail!("SQL0010: A closing {c} is missing."),
                    Some(&x) if x == c && cs.get(i + 1) == Some(&c) => {
                        v.push(c);
                        i += 2;
                    }
                    Some(&x) if x == c => {
                        i += 1;
                        break;
                    }
                    Some(&x) => {
                        v.push(x);
                        i += 1;
                    }
                }
            }
            out.push(if c == '\'' {
                Tok::Str(v)
            } else {
                Tok::Quoted(v)
            });
        } else if c.is_ascii_digit()
            || (c == '.' && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let s = i;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '.') {
                i += 1;
            }
            out.push(Tok::Number(cs[s..i].iter().collect()));
        } else if c.is_alphabetic() || "_$#@".contains(c) {
            let s = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || "_$#@".contains(cs[i])) {
                i += 1;
            }
            out.push(Tok::Word(cs[s..i].iter().collect()));
        } else if c == '|' && cs.get(i + 1) == Some(&'|') {
            out.push(Tok::Punct("||".into()));
            i += 2;
        } else {
            out.push(Tok::Punct(c.to_string()));
            i += 1;
        }
    }
    Ok(out)
}

fn word(t: &Tok) -> Option<String> {
    match t {
        Tok::Word(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

fn render(toks: &[Tok]) -> String {
    toks.iter()
        .map(|t| match t {
            Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
            Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
            // DB2 names are upper case; ibmish stores them lower case.
            Tok::Quoted(s) => ident(s),
        })
        .collect()
}

/// What kind of statement it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// SELECT, WITH, VALUES: returns rows.
    Query,
    Insert,
    Update,
    Delete,
}

/// A statement translated to PostgreSQL.
#[derive(Debug, Clone, PartialEq)]
pub struct Translated {
    pub kind: Kind,
    pub sql: String,
    /// The statement asked for the relative record number (RRN).
    pub wants_rrn: bool,
    /// INSERT without a column list: the target file, and where the
    /// column list goes (byte offset in `sql`).
    pub insert_needs_columns: Option<(String, usize)>,
}

/// Translate one DB2 for i statement. `is_library` says whether a name is
/// a library, for system naming (`LIB/FILE`).
pub fn translate(sql: &str, is_library: &dyn Fn(&str) -> bool) -> Result<Translated> {
    let mut toks = tokenize(sql.trim())?;
    // One statement: a trailing ';' is allowed, no other.
    while matches!(toks.last(), Some(Tok::Space(_))) {
        toks.pop();
    }
    if matches!(toks.last(), Some(Tok::Punct(p)) if p == ";") {
        toks.pop();
    }
    if toks.iter().any(|t| matches!(t, Tok::Punct(p) if p == ";")) {
        bail!("SQL0104: Only one statement can be run at a time.");
    }
    let first = toks
        .iter()
        .find_map(word)
        .ok_or_else(|| anyhow!("SQL0104: Type an SQL statement."))?;
    let kind = match first.as_str() {
        "SELECT" | "WITH" | "VALUES" => Kind::Query,
        "INSERT" => Kind::Insert,
        "UPDATE" => Kind::Update,
        "DELETE" => Kind::Delete,
        other => bail!(
            "SQL0084: {other} statements are not supported yet; use SELECT, VALUES, INSERT, UPDATE or DELETE."
        ),
    };
    // Statements may not reach the catalog or PostgreSQL's own schemas
    // (the role cannot either; this gives a clearer message).
    for t in &toks {
        if let Some(w) = word(t) {
            if w == "IBMISH"
                || w.starts_with("PG_")
                || w == "INFORMATION_SCHEMA"
                || w == "SET_CONFIG"
                || w == "DBLINK"
            {
                bail!("SQL0551: Not authorized to {w}.");
            }
        }
    }
    let wants_rrn = toks.iter().any(|t| word(t).as_deref() == Some("RRN"));

    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    let next_non_space = |toks: &[Tok], from: usize| -> Option<usize> {
        (from..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)))
    };
    while i < toks.len() {
        let t = &toks[i];
        let w = word(t);
        // System naming: LIB/FILE, written without blanks.
        if let (Tok::Word(lib), Some(Tok::Punct(p)), Some(Tok::Word(file))) =
            (t, toks.get(i + 1), toks.get(i + 2))
        {
            if p == "/" && is_library(&lib.to_ascii_uppercase()) {
                out.push(Tok::Word(format!("{}.{}", ident(lib), ident(file))));
                i += 3;
                continue;
            }
        }
        // A field named like a PostgreSQL system column (CTID, XMIN...).
        if let (Tok::Word(name), Some(_)) = (t, &w) {
            if crate::catalog::pg_name(name).starts_with('~') {
                out.push(Tok::Word(ident(name)));
                i += 1;
                continue;
            }
        }
        match w.as_deref() {
            Some("CURRENT") => {
                if let Some(j) = next_non_space(&toks, i + 1) {
                    if let Some(n) = word(&toks[j]) {
                        if ["DATE", "TIME", "TIMESTAMP"].contains(&n.as_str()) {
                            out.push(Tok::Word(format!("CURRENT_{n}")));
                            i = j + 1;
                            continue;
                        }
                    }
                }
            }
            Some("CONCAT") => {
                let call =
                    next_non_space(&toks, i + 1).is_some_and(|j| toks[j] == Tok::Punct("(".into()));
                if !call {
                    out.push(Tok::Punct("||".into()));
                    i += 1;
                    continue;
                }
            }
            Some("UCASE") => {
                out.push(Tok::Word("UPPER".into()));
                i += 1;
                continue;
            }
            Some("LCASE") => {
                out.push(Tok::Word("LOWER".into()));
                i += 1;
                continue;
            }
            Some("RRN") => {
                // RRN(t) -> t.rrn
                let open = next_non_space(&toks, i + 1);
                let name = open.and_then(|o| next_non_space(&toks, o + 1));
                let close = name.and_then(|n| next_non_space(&toks, n + 1));
                if let (Some(o), Some(n), Some(c)) = (open, name, close) {
                    if let (Tok::Punct(po), Tok::Word(nm), Tok::Punct(pc)) =
                        (&toks[o], &toks[n], &toks[c])
                    {
                        if po == "(" && pc == ")" {
                            out.push(Tok::Word(format!("{nm}.rrn")));
                            i = c + 1;
                            continue;
                        }
                    }
                }
            }
            Some("SYSIBM") => {
                // SYSIBM.SYSDUMMY1: a one-row table.
                if let (Some(Tok::Punct(p)), Some(n)) =
                    (toks.get(i + 1), toks.get(i + 2).and_then(word))
                {
                    if p == "." && n == "SYSDUMMY1" {
                        out.push(Tok::Word("(VALUES ('Y')) AS sysdummy1(ibmreqd)".into()));
                        i += 3;
                        continue;
                    }
                }
            }
            Some("WITH") if i > 0 => {
                // Isolation clause at the end: WITH NC|UR|CS|RS|RR.
                if let Some(j) = next_non_space(&toks, i + 1) {
                    let iso = word(&toks[j]).unwrap_or_default();
                    if ["NC", "UR", "CS", "RS", "RR"].contains(&iso.as_str())
                        && next_non_space(&toks, j + 1).is_none()
                    {
                        break;
                    }
                }
            }
            Some("OPTIMIZE") => {
                // OPTIMIZE FOR n ROW(S)
                let f = next_non_space(&toks, i + 1);
                let n = f.and_then(|f| next_non_space(&toks, f + 1));
                let r = n.and_then(|n| next_non_space(&toks, n + 1));
                if let (Some(f), Some(n), Some(r)) = (f, n, r) {
                    if word(&toks[f]).as_deref() == Some("FOR")
                        && matches!(toks[n], Tok::Number(_))
                        && matches!(word(&toks[r]).as_deref(), Some("ROW" | "ROWS"))
                    {
                        i = r + 1;
                        continue;
                    }
                }
            }
            _ => {}
        }
        out.push(t.clone());
        i += 1;
    }
    while matches!(out.last(), Some(Tok::Space(_))) {
        out.pop();
    }

    // DB2 VALUES without parentheses: each expression is a row
    // (VALUES 1, 2 is two rows). PostgreSQL needs VALUES (1), (2).
    if first == "VALUES" {
        if let Some(v) = out
            .iter()
            .position(|t| word(t).as_deref() == Some("VALUES"))
        {
            let rest_start = (v + 1..out.len()).find(|&j| !matches!(out[j], Tok::Space(_)));
            if let Some(r) = rest_start {
                if out[r] != Tok::Punct("(".into()) {
                    let mut rows: Vec<Vec<Tok>> = vec![vec![]];
                    let mut depth = 0i32;
                    for t in &out[r..] {
                        match t {
                            Tok::Punct(p) if p == "(" => depth += 1,
                            Tok::Punct(p) if p == ")" => depth -= 1,
                            Tok::Punct(p) if p == "," && depth == 0 => {
                                rows.push(vec![]);
                                continue;
                            }
                            _ => {}
                        }
                        rows.last_mut().unwrap().push(t.clone());
                    }
                    let mut new = out[..r].to_vec();
                    for (n, mut row) in rows.into_iter().enumerate() {
                        while matches!(row.first(), Some(Tok::Space(_))) {
                            row.remove(0);
                        }
                        while matches!(row.last(), Some(Tok::Space(_))) {
                            row.pop();
                        }
                        if n > 0 {
                            new.push(Tok::Punct(", ".into()));
                        }
                        new.push(Tok::Punct("(".into()));
                        new.extend(row);
                        new.push(Tok::Punct(")".into()));
                    }
                    out = new;
                }
            }
        }
    }

    // INSERT INTO name VALUES|SELECT (no column list).
    let mut insert_needs_columns = None;
    if kind == Kind::Insert {
        let idx: Vec<usize> = (0..out.len())
            .filter(|&j| !matches!(out[j], Tok::Space(_)))
            .collect();
        if idx.len() >= 4
            && word(&out[idx[1]]).as_deref() == Some("INTO")
            && matches!(
                word(&out[idx[3]]).as_deref(),
                Some("VALUES" | "SELECT" | "WITH")
            )
        {
            let target = match &out[idx[2]] {
                Tok::Word(w) => w.clone(),
                Tok::Quoted(q) => q.clone(),
                _ => bail!("SQL0104: A table name is expected after INSERT INTO."),
            };
            let before: String = render(&out[..=idx[2]]);
            insert_needs_columns = Some((target, before.len()));
        }
    }
    Ok(Translated {
        kind,
        sql: render(&out),
        wants_rrn,
        insert_needs_columns,
    })
}

// ------------------------------------------------------------ running

/// The result of one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Rows {
        columns: Vec<String>,
        /// Numeric columns (right-aligned when shown).
        numeric: Vec<bool>,
        rows: Vec<Vec<String>>,
    },
    /// INSERT/UPDATE/DELETE: rows affected.
    Count(Kind, u64),
}

/// The most rows one SELECT returns to a display.
pub const MAX_ROWS: usize = 9999;

fn map_error(e: &tokio_postgres::Error, sql: &str) -> anyhow::Error {
    let Some(db) = e.as_db_error() else {
        return anyhow!("SQL0901: {e}");
    };
    let msg = db.message().to_string();
    let code = db.code().code();
    let upper = |s: &str| s.to_ascii_uppercase().replace('"', "");
    match code {
        "42P01" => anyhow!(
            "SQL0204: {} not found.",
            upper(
                msg.trim_start_matches("relation ")
                    .trim_end_matches(" does not exist")
            )
        ),
        "42703" => anyhow!(
            "SQL0206: Column {} not in the tables of the statement.",
            upper(
                msg.trim_start_matches("column ")
                    .trim_end_matches(" does not exist")
            )
        ),
        "42601" => anyhow!("SQL0104: Statement not valid: {msg}."),
        "23505" => anyhow!("SQL0803: Duplicate key value: the record was not written."),
        "42501" => anyhow!("SQL0551: Not authorized to an object in the statement."),
        "22P02" | "22003" => anyhow!("SQL0302: A value does not fit its column: {msg}."),
        _ => {
            let _ = sql;
            anyhow!("SQL0901: {msg}.")
        }
    }
}

/// Run one statement with the library list as search path, as the SQL
/// role, committed when it succeeds (COMMIT(*NONE)).
pub async fn run(
    client: &Client,
    database_url: &str,
    libl: &[String],
    sql: &str,
) -> Result<Outcome> {
    let libraries = catalog::list_libraries(client).await?;
    let t = translate(sql, &|n| libraries.iter().any(|l| l == n))?;
    let mut text = t.sql.clone();
    if let Some((target, at)) = &t.insert_needs_columns {
        // The file's fields, in order, as the column list.
        let name = if target.contains('.') {
            target
                .replace('"', "")
                .replace('.', "/")
                .to_ascii_uppercase()
        } else {
            target.to_ascii_uppercase()
        };
        let f = catalog::resolve_file(client, libl, &name)
            .await
            .map_err(|_| anyhow!("SQL0204: {name} not found."))?;
        let cols: Vec<String> = f.file.fields.iter().map(|fd| ident(&fd.name)).collect();
        text.insert_str(*at, &format!(" ({})", cols.join(", ")));
    }
    let path: Vec<String> = libl.iter().map(|l| ident(l)).collect();
    // One transaction per statement (COMMIT(*NONE)), on a connection of
    // its own as the SQL role.
    let sql_client = connect_as_sql_role(client, database_url).await?;
    sql_client.batch_execute("BEGIN").await?;
    let result = run_in_transaction(&sql_client, &t, &text, &path).await;
    match &result {
        Ok(_) => sql_client.batch_execute("COMMIT").await?,
        Err(_) => sql_client.batch_execute("ROLLBACK").await?,
    }
    result
}

async fn run_in_transaction(
    tx: &Client,
    t: &Translated,
    text: &str,
    path: &[String],
) -> Result<Outcome> {
    tx.batch_execute(&format!(
        "SET LOCAL search_path TO {}",
        if path.is_empty() {
            "\"qgpl\"".to_string()
        } else {
            path.join(", ")
        }
    ))
    .await?;
    let stmt = tx.prepare(text).await.map_err(|e| map_error(&e, text))?;
    let outcome = if t.kind == Kind::Query {
        let cols = stmt.columns();
        let keep: Vec<bool> = cols
            .iter()
            .map(|c| t.wants_rrn || c.name() != "rrn")
            .collect();
        let numeric: Vec<bool> = cols
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(c, _)| {
                matches!(
                    c.type_().name(),
                    "numeric" | "int2" | "int4" | "int8" | "float4" | "float8"
                )
            })
            .collect();
        // Columns without a name (expressions, VALUES) are named by their
        // position, as DB2 does.
        let is_values = t
            .sql
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("VALUES");
        let columns: Vec<String> = cols
            .iter()
            .enumerate()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|((n, c), _)| {
                let values_col = is_values
                    && c.name()
                        .strip_prefix("column")
                        .is_some_and(|d| d.chars().all(|x| x.is_ascii_digit()));
                if c.name() == "?column?" || values_col {
                    (n + 1).to_string()
                } else {
                    crate::catalog::ibm_name(c.name())
                }
            })
            .collect();
        let mut rows = Vec::new();
        for m in tx
            .simple_query(text)
            .await
            .map_err(|e| map_error(&e, text))?
        {
            if let SimpleQueryMessage::Row(r) = m {
                if rows.len() >= MAX_ROWS {
                    break;
                }
                rows.push(
                    (0..r.len())
                        .filter(|&j| keep.get(j).copied().unwrap_or(true))
                        .map(|j| r.get(j).unwrap_or("-").to_string())
                        .collect(),
                );
            }
        }
        Outcome::Rows {
            columns,
            numeric,
            rows,
        }
    } else {
        let n = tx
            .execute(&stmt, &[])
            .await
            .map_err(|e| map_error(&e, text))?;
        Outcome::Count(t.kind, n)
    };
    Ok(outcome)
}

/// A completion message like IBM i's for INSERT/UPDATE/DELETE.
pub fn count_message(kind: Kind, n: u64) -> String {
    let what = match kind {
        Kind::Insert => "inserted",
        Kind::Update => "updated",
        Kind::Delete => "deleted",
        Kind::Query => "selected",
    };
    format!("{n} rows {what}.")
}

/// Rows as text lines: a heading line, then one line per row; numbers
/// right-aligned, text left-aligned, one blank between columns.
pub fn format_rows(
    columns: &[String],
    numeric: &[bool],
    rows: &[Vec<String>],
) -> (String, Vec<String>) {
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for r in rows {
        for (w, v) in widths.iter_mut().zip(r) {
            *w = (*w).max(v.chars().count());
        }
    }
    let line = |vals: &[String]| -> String {
        vals.iter()
            .zip(&widths)
            .zip(numeric)
            .map(|((v, w), num)| {
                if *num {
                    format!("{v:>w$}")
                } else {
                    format!("{v:<w$}")
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    (line(columns), rows.iter().map(|r| line(r)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(sql: &str) -> Translated {
        translate(sql, &|n| n == "QGPL" || n == "MYLIB").unwrap()
    }

    // DB2 for i SQL reference: system naming LIB/FILE, SQL naming LIB.FILE.
    #[test]
    fn system_and_sql_naming() {
        assert_eq!(
            tr("select * from qgpl/custmast").sql,
            "select * from \"qgpl\".\"custmast\""
        );
        assert_eq!(
            tr("SELECT * FROM QGPL.CUSTMAST").sql,
            "SELECT * FROM QGPL.CUSTMAST"
        );
        // Division stays division when the left side is not a library.
        assert_eq!(
            tr("select qty/2 from ordlin").sql,
            "select qty/2 from ordlin"
        );
        // Quoted names: DB2 upper case, stored lower case here.
        assert_eq!(
            tr("select \"CUSNAM\" from \"CUSTMAST\"").sql,
            "select \"cusnam\" from \"custmast\""
        );
    }

    // DB2 for i SQL reference: special registers, CONCAT operator, scalar
    // functions UCASE/LCASE, RRN, SYSIBM.SYSDUMMY1, isolation clause.
    #[test]
    fn fields_named_like_postgresql_system_columns() {
        // CTID is a valid IBM i field name; in PostgreSQL ctid is a system
        // column (PostgreSQL manual, "System Columns"), so ibmish stores it
        // as "~ctid".
        assert_eq!(
            tr("select ctid, name from mylib/contact where ctid > 3").sql,
            "select \"~ctid\", name from \"mylib\".\"contact\" where \"~ctid\" > 3"
        );
        assert_eq!(crate::catalog::ibm_name("~ctid"), "CTID");
    }

    #[test]
    fn db2_dialect() {
        assert_eq!(tr("values current date").sql, "values (CURRENT_DATE)");
        // DB2: VALUES with a list of expressions gives one row per expression.
        assert_eq!(
            tr("values 1, (2), substr('ab', 1, 1)").sql,
            "values (1), ((2)), (substr('ab', 1, 1))"
        );
        assert_eq!(tr("values (1, 2)").sql, "values (1, 2)");
        assert_eq!(tr("select a concat b from t").sql, "select a || b from t");
        assert_eq!(
            tr("select concat(a, b) from t").sql,
            "select concat(a, b) from t"
        );
        assert_eq!(
            tr("select ucase(cusnam) from t").sql,
            "select UPPER(cusnam) from t"
        );
        let t = tr("select rrn(c), cusnam from custmast c");
        assert_eq!(t.sql, "select c.rrn, cusnam from custmast c");
        assert!(t.wants_rrn);
        assert!(!tr("select * from custmast").wants_rrn);
        assert_eq!(
            tr("select current timestamp from sysibm.sysdummy1").sql,
            "select CURRENT_TIMESTAMP from (VALUES ('Y')) AS sysdummy1(ibmreqd)"
        );
        assert_eq!(tr("select * from t with nc").sql, "select * from t");
        assert_eq!(
            tr("select * from t optimize for 10 rows").sql,
            "select * from t"
        );
        assert_eq!(tr("select 'it''s' from t;").sql, "select 'it''s' from t");
    }

    #[test]
    fn statements_allowed() {
        assert_eq!(tr("delete from t where x = 1").kind, Kind::Delete);
        let t = tr("insert into custmast values (9, 'X', 'Y', 0)");
        assert_eq!(t.kind, Kind::Insert);
        let (target, at) = t.insert_needs_columns.unwrap();
        assert_eq!(target, "custmast");
        assert_eq!(&t.sql[..at], "insert into custmast");
        assert!(tr("insert into custmast (cusno) values (9)")
            .insert_needs_columns
            .is_none());
        let err = |s: &str| translate(s, &|_| false).unwrap_err().to_string();
        assert!(err("drop table t").starts_with("SQL0084"));
        assert!(err("select 1; drop table t").starts_with("SQL0104"));
        assert!(err("select * from ibmish.user_profiles").starts_with("SQL0551"));
        assert!(err("select * from pg_shadow").starts_with("SQL0551"));
        assert!(err("select 'abc").starts_with("SQL0010"));
        // ';' inside a string is data.
        assert_eq!(tr("select 'a;b' from t").sql, "select 'a;b' from t");
    }

    #[test]
    fn rows_are_aligned() {
        let (h, r) = format_rows(
            &["CUSNO".into(), "CUSNAM".into()],
            &[true, false],
            &[
                vec!["1".into(), "Acme".into()],
                vec!["12345".into(), "B".into()],
            ],
        );
        assert_eq!(h, "CUSNO  CUSNAM");
        assert_eq!(r, ["    1  Acme", "12345  B"]);
    }
}
