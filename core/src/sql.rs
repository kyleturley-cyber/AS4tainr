//! SQL in the DB2 for i dialect, run on PostgreSQL.
//!
//! Written from IBM's public DB2 for i SQL reference. A statement is
//! translated, then run in its own transaction on a separate connection
//! that logs in as the `as4tainr_sql` role. That role is not a superuser and
//! can use the library schemas and nothing else (not the `as4tainr`
//! catalog, where password hashes live); its password is random and kept
//! in the catalog. Because the connection's own login is the restricted
//! role, a statement has no more powerful role to switch to. The job's
//! library list is the search path. Supported:
//!
//! - SELECT, WITH, VALUES, INSERT, UPDATE, DELETE; one statement at a time
//! - system naming `LIB/FILE` (when LIB is a library; either name may be
//!   delimited, `"QGPL"/"CUSTMAST"`) and SQL naming
//!   `LIB.FILE`; unqualified names are found through the library list
//! - `CURRENT DATE`, `CURRENT TIME`, `CURRENT TIMESTAMP`, the `CONCAT`
//!   operator, `UCASE`/`LCASE`, `RRN(t)`, `SYSIBM.SYSDUMMY1`, and the
//!   `WITH NC|UR|CS|RS|RR`, `OPTIMIZE FOR n ROWS` and `FOR READ ONLY` /
//!   `FOR FETCH ONLY` clauses (accepted and ignored)
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
pub const SQL_ROLE: &str = "as4tainr_sql";

/// The schema of the operators and functions for Db2's implicit casts
/// between strings and numbers (`sqlfn::implicit_casts`), last in every
/// SQL search path.
pub const CAST_SCHEMA: &str = "as4tainr_cast";

/// The PostgreSQL search path of a library list: its libraries, QGPL for
/// none, then the implicit-cast schema.
pub fn search_path(libl: &[String]) -> String {
    let path: Vec<String> = libl.iter().map(|l| ident(l)).collect();
    joined_path(&path)
}

/// `search_path` of library names already quoted.
pub fn joined_path(quoted: &[String]) -> String {
    let mut path = quoted.to_vec();
    if path.is_empty() {
        path.push("\"qgpl\"".into());
    }
    path.push(CAST_SCHEMA.into());
    path.join(", ")
}

/// The job's QTEMP library for the IBM i services (as4tainr.qtemp, read
/// by OBJECT_STATISTICS, whose schema parameter may be QTEMP while QTEMP
/// is not in the library list): a statement to follow SET search_path.
pub fn qtemp_setting(local: bool) -> String {
    match crate::qtemp::current() {
        Some(q) => format!(
            "; SELECT set_config('as4tainr.qtemp', '{}', {local})",
            q.replace('\'', "''")
        ),
        None => String::new(),
    }
}

/// Create the SQL login role with a random password (made once, then
/// kept, so processes starting together agree on it) and give it the
/// library schemas (idempotent). Called from `catalog::init`. Fails if
/// the database user may not create roles; SQL statements are then
/// refused.
pub async fn init(client: &Client) -> Result<()> {
    use argon2::password_hash::{rand_core::OsRng, SaltString};
    // The role is the cluster's, not the database's: processes starting
    // at once on other databases alter it too ("tuple concurrently
    // updated"), so these statements are tried again.
    batch_retrying(
        client,
        &format!(
            "DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{SQL_ROLE}') THEN
                 CREATE ROLE {SQL_ROLE};
               END IF;
             END $$;
             ALTER ROLE {SQL_ROLE} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
             ALTER DEFAULT PRIVILEGES GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {SQL_ROLE};
             ALTER DEFAULT PRIVILEGES GRANT USAGE, SELECT ON SEQUENCES TO {SQL_ROLE};
             REVOKE ALL ON SCHEMA as4tainr FROM {SQL_ROLE};
             CREATE TABLE IF NOT EXISTS as4tainr.sql_login (id int PRIMARY KEY, password text NOT NULL);"
        ),
    )
    .await?;
    let fresh = SaltString::generate(&mut OsRng).to_string();
    client
        .execute(
            "INSERT INTO as4tainr.sql_login VALUES (1, $1) ON CONFLICT (id) DO NOTHING",
            &[&fresh],
        )
        .await?;
    let password: String = client
        .query_one("SELECT password FROM as4tainr.sql_login WHERE id = 1", &[])
        .await?
        .get(0);
    // A random base64 string: no quotes to escape.
    if !password
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/".contains(c))
    {
        bail!("the stored SQL password has unexpected characters");
    }
    batch_retrying(
        client,
        &format!("ALTER ROLE {SQL_ROLE} PASSWORD '{password}'"),
    )
    .await?;
    // Jobs' QTEMP libraries got their grants when they were made, and
    // their jobs drop them at any moment: granting again would race with
    // that DROP SCHEMA.
    let libs: Vec<String> = catalog::list_libraries(client)
        .await?
        .into_iter()
        .filter(|l| !l.starts_with("#QP") && !l.starts_with("#QT"))
        .collect();
    // Asked in one query for every library: each process initializes, and
    // a database with thousands of libraries took seconds asking one by
    // one.
    for lib in missing_grants(client, &libs).await {
        // Another process may drop a library or a table in it while the
        // grants run (a job's QTEMP at its end, parallel jobs): a library
        // that is gone is skipped, and a grant that met a table being
        // dropped ("tuple concurrently deleted/updated") is tried again.
        //
        // Only a library where the SQL role is missing a grant is granted
        // again (a database made before the default privileges, a restore):
        // GRANT ... ON ALL TABLES changes every table's catalog row, and
        // another process's TRUNCATE or ALTER of one of them at that moment
        // fails with "tuple concurrently updated" (tests in parallel, each
        // with its own init).
        let mut tries = 0;
        while let Err(e) = grant_library(client, &lib).await {
            let db = e.downcast_ref::<tokio_postgres::Error>();
            if db.and_then(|e| e.code())
                == Some(&tokio_postgres::error::SqlState::INVALID_SCHEMA_NAME)
            {
                break;
            }
            let concurrent = db
                .and_then(|e| e.as_db_error())
                .is_some_and(|d| d.message().starts_with("tuple concurrently"));
            tries += 1;
            if !concurrent || tries >= 5 {
                return Err(e);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50 * tries)).await;
        }
    }
    crate::services::init(client).await?;
    Ok(())
}

/// A connection that logs in as the SQL role, to the same database.
pub(crate) async fn connect_as_sql_role(client: &Client, database_url: &str) -> Result<Client> {
    let password: String = client
        .query_opt("SELECT password FROM as4tainr.sql_login WHERE id = 1", &[])
        .await?
        .map(|r| r.get(0))
        .ok_or_else(|| {
            anyhow!(
                "SQL is not available: run `as4tainr init` as a database user who may create roles."
            )
        })?;
    let mut cfg: tokio_postgres::Config = database_url.parse()?;
    cfg.user(SQL_ROLE);
    cfg.password(password.clone());
    let first = cfg.connect(tokio_postgres::NoTls).await;
    let (c, conn) = match first {
        Ok(x) => x,
        // The role belongs to the PostgreSQL cluster, the password to
        // each database: another as4tainr database of the cluster
        // initialized after this one gave the role its own. This
        // database's password again, then once more.
        Err(e)
            if e.code() == Some(&tokio_postgres::error::SqlState::INVALID_PASSWORD)
                && password
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+/".contains(c)) =>
        {
            // Processes of other databases may be doing the same at this
            // moment: a few tries.
            let mut tries = 0u64;
            loop {
                tries += 1;
                let set = batch_retrying(
                    client,
                    &format!("ALTER ROLE {SQL_ROLE} PASSWORD '{password}'"),
                )
                .await;
                match (set, cfg.connect(tokio_postgres::NoTls).await) {
                    (Ok(()), Ok(x)) => break x,
                    (_, Err(_)) | (Err(_), _) if tries < 5 => {
                        tokio::time::sleep(std::time::Duration::from_millis(30 * tries)).await;
                    }
                    (_, Err(e)) => {
                        bail!("SQL is not available: cannot log in as {SQL_ROLE} ({e}).")
                    }
                    (Err(_), Ok(x)) => break x,
                }
            }
        }
        Err(e) => bail!("SQL is not available: cannot log in as {SQL_ROLE} ({e})."),
    };
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(c)
}

/// The libraries whose schema the SQL role lacks USAGE of, or a grant on
/// a table, view or sequence in (all of them when that cannot be told).
async fn missing_grants(client: &Client, libraries: &[String]) -> Vec<String> {
    client
        .query(
            // One pass over pg_class joined to the libraries (an EXISTS
            // per library scanned pg_class once for each).
            "WITH libs AS (
               SELECT l, to_regnamespace(quote_ident(s)) AS n
                 FROM unnest($1::text[], $3::text[]) AS u(l, s))
             SELECT l FROM libs WHERE n IS NOT NULL AND NOT has_schema_privilege($2, n, 'USAGE')
             UNION
             SELECT l FROM libs JOIN pg_class c ON c.relnamespace = libs.n
              WHERE (c.relkind IN ('r','p','v','m','f') AND NOT (
                       coalesce(has_table_privilege($2, c.oid, 'SELECT'), true)
                       AND coalesce(has_table_privilege($2, c.oid, 'INSERT'), true)
                       AND coalesce(has_table_privilege($2, c.oid, 'UPDATE'), true)
                       AND coalesce(has_table_privilege($2, c.oid, 'DELETE'), true)))
                 OR (c.relkind = 'S' AND NOT (
                       coalesce(has_sequence_privilege($2, c.oid, 'USAGE'), true)
                       AND coalesce(has_sequence_privilege($2, c.oid, 'SELECT'), true)))",
            &[
                &libraries,
                &SQL_ROLE,
                &libraries
                    .iter()
                    .map(|l| catalog::pg_name(l))
                    .collect::<Vec<_>>(),
            ],
        )
        .await
        .map(|rows| rows.iter().map(|r| r.get(0)).collect())
        .unwrap_or_else(|_| libraries.to_vec())
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

/// GRANT CREATE ON SCHEMA to the SQL role, while a routine or view is
/// defined. Another process granting on the same schema at that moment
/// (`init`'s grants to every library) makes it fail with "tuple
/// concurrently updated": it is tried again.
/// A batch tried again (up to five times) when another process changed
/// the same catalog row at the same moment.
async fn batch_retrying(client: &Client, sql: &str) -> Result<(), tokio_postgres::Error> {
    let mut tries = 0;
    loop {
        match client.batch_execute(sql).await {
            Err(e)
                if tries < 5
                    && e.as_db_error()
                        .is_some_and(|d| d.message().starts_with("tuple concurrently")) =>
            {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50 * tries)).await;
            }
            r => return r,
        }
    }
}

async fn grant_create(client: &Client, schema: &str) -> Result<(), tokio_postgres::Error> {
    batch_retrying(
        client,
        &format!("GRANT CREATE ON SCHEMA {schema} TO {SQL_ROLE}"),
    )
    .await
}

// ------------------------------------------------------------ tokens

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
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

// NEXT VALUE FOR seq (NEXTVAL FOR) and PREVIOUS VALUE FOR seq (PREVVAL
// FOR) (Db2 for i SQL Reference, sequence reference): the sequence's
// next value, and the last one NEXT VALUE gave in this session.

/// The special registers CURRENT DATE, CURRENT TIME and CURRENT TIMESTAMP
/// as PostgreSQL computes them: the time the statement started.
const STMT_DATE: &str = "CAST(statement_timestamp() AS date)";
const STMT_TIME: &str = "CAST(statement_timestamp() AS time(0))";
const STMT_TIMESTAMP: &str = "CAST(statement_timestamp() AS timestamp)";

fn sequence_values(toks: Vec<Tok>, is_library: &dyn Fn(&str) -> bool) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut i = 0;
    let at = |j: usize| -> Option<String> { toks.get(j).and_then(word) };
    // The next word after j, skipping blanks.
    let skip = |j: usize| -> usize {
        let mut k = j;
        while matches!(toks.get(k), Some(Tok::Space(_))) {
            k += 1;
        }
        k
    };
    while i < toks.len() {
        let w = at(i);
        let (func, after) = match w.as_deref() {
            Some("NEXT") | Some("PREVIOUS") => {
                let v = skip(i + 1);
                let f = skip(v + 1);
                if at(v).as_deref() == Some("VALUE") && at(f).as_deref() == Some("FOR") {
                    (
                        if w.as_deref() == Some("NEXT") {
                            "nextval"
                        } else {
                            "currval"
                        },
                        skip(f + 1),
                    )
                } else {
                    ("", 0)
                }
            }
            Some("NEXTVAL") | Some("PREVVAL") => {
                let f = skip(i + 1);
                if at(f).as_deref() == Some("FOR") {
                    (
                        if w.as_deref() == Some("NEXTVAL") {
                            "nextval"
                        } else {
                            "currval"
                        },
                        skip(f + 1),
                    )
                } else {
                    ("", 0)
                }
            }
            _ => ("", 0),
        };
        if func.is_empty() {
            out.push(toks[i].clone());
            i += 1;
            continue;
        }
        let name_of = |t: Option<&Tok>| -> Option<String> {
            match t {
                Some(Tok::Word(w)) => Some(crate::catalog::pg_name(w)),
                Some(Tok::Quoted(q)) => Some(q.to_ascii_lowercase()),
                _ => None,
            }
        };
        let Some(first) = name_of(toks.get(after)) else {
            out.push(toks[i].clone());
            i += 1;
            continue;
        };
        let (name, end) = match (toks.get(after + 1), name_of(toks.get(after + 2))) {
            (Some(Tok::Punct(p)), Some(n))
                if p == "." || (p == "/" && is_library(&first.to_ascii_uppercase())) =>
            {
                (format!("\"{first}\".\"{n}\""), after + 3)
            }
            _ => (format!("\"{first}\""), after + 1),
        };
        out.push(Tok::Punct(format!(
            "{func}('{}')",
            name.replace('\'', "''")
        )));
        i = end;
    }
    out
}

pub(crate) fn tokenize(sql: &str) -> Result<Vec<Tok>> {
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
            while i < cs.len()
                && (cs[i].is_ascii_alphanumeric()
                    || cs[i] == '.'
                    // 1.0E-3: the exponent's sign.
                    || ("+-".contains(cs[i])
                        && matches!(cs.get(i - 1), Some('e' | 'E'))
                        && cs[s..i - 1].iter().all(|c| c.is_ascii_digit() || *c == '.')
                        && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit())))
            {
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

/// The closing parenthesis of the one at `open`, and the commas at its
/// level.
fn paren_span(toks: &[Tok], open: usize) -> Option<(usize, Vec<usize>)> {
    let mut depth = 0i32;
    let mut commas = Vec::new();
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t {
            Tok::Punct(p) if p == "(" => depth += 1,
            Tok::Punct(p) if p == ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some((j, commas));
                }
            }
            Tok::Punct(p) if p == "," && depth == 1 => commas.push(j),
            _ => {}
        }
    }
    None
}

/// Whether a type name followed by `(` is a function (`INTEGER(x)`) and not
/// a data type (`AS VARCHAR(10)`, `RETURNS DECIMAL(5, 2)`, a column or
/// variable's type after its name): what comes before it starts an
/// expression.
fn function_position(out: &[Tok]) -> bool {
    match out.iter().rev().find(|t| !matches!(t, Tok::Space(_))) {
        None => true,
        Some(Tok::Punct(p)) => p != ")" && p != "." && !p.ends_with(" AS"),
        Some(t) => word(t).is_some_and(|w| {
            matches!(
                w.as_str(),
                "SELECT"
                    | "WHERE"
                    | "AND"
                    | "OR"
                    | "NOT"
                    | "THEN"
                    | "ELSE"
                    | "WHEN"
                    | "RETURN"
                    | "VALUES"
                    | "ON"
                    | "HAVING"
                    | "BY"
                    | "DISTINCT"
                    | "ALL"
                    | "IN"
                    | "LIKE"
                    | "BETWEEN"
                    | "IS"
                    | "CASE"
                    | "EXISTS"
                    | "ANY"
                    | "SOME"
            )
        }),
    }
}

/// The USER special register (and SESSION_USER, SYSTEM_USER, CURRENT
/// USER): the job's user profile; record-level access and triggers know
/// the job by its name (number/user/name).
pub const USER_SQL: &str = "CAST(COALESCE(NULLIF(current_setting('as4tainr.user', true), ''), \
     NULLIF(split_part(current_setting('application_name'), '/', 2), ''), \
     upper(session_user)) AS varchar(18))";

/// Columns named by their system name (`FOR COLUMN` of CREATE TABLE, DDS
/// ALIAS's field name): SQL statements may use either name (Db2 for i SQL
/// Reference, "Column names"); the PostgreSQL column carries the SQL name.
/// A system name of the files in the library list whose SQL name differs
/// is written as that SQL name (when its files agree on it).
async fn system_column_names(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    // The words outside literals and quoted names: (start, end).
    let cs: Vec<(usize, char)> = sql.char_indices().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || "_#@$".contains(c);
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i].1;
        if c == '\'' || c == '"' {
            i += 1;
            while i < cs.len() && cs[i].1 != c {
                i += 1;
            }
            i += 1;
            continue;
        }
        if is_word(c) && !c.is_ascii_digit() && (i == 0 || !is_word(cs[i - 1].1)) {
            let start = cs[i].0;
            while i < cs.len() && is_word(cs[i].1) {
                i += 1;
            }
            let end = if i < cs.len() { cs[i].0 } else { sql.len() };
            words.push((start, end));
            continue;
        }
        i += 1;
    }
    let names: Vec<String> = words
        .iter()
        .map(|(a, b)| sql[*a..*b].to_ascii_uppercase())
        .filter(|w| w.len() <= 10)
        .collect();
    if names.is_empty() || libl.is_empty() {
        return Ok(sql.to_string());
    }
    // The files the statement names (by system or SQL name): only their
    // columns count, when it names any.
    let all_words: Vec<String> = words
        .iter()
        .map(|(a, b)| sql[*a..*b].to_ascii_uppercase())
        .collect();
    let named_files: Vec<String> = client
        .query(
            "SELECT DISTINCT name FROM as4tainr.files
              WHERE library = ANY($1) AND (name = ANY($2) OR upper(sql_name) = ANY($2))",
            &[&libl, &all_words],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let scope = !named_files.is_empty();
    let rows = client
        .query(
            "SELECT DISTINCT name, alias FROM as4tainr.file_fields
              WHERE library = ANY($1) AND alias IS NOT NULL AND alias <> name AND name = ANY($2)
                AND (NOT $4 OR file = ANY($3))",
            &[&libl, &names, &named_files, &scope],
        )
        .await?;
    if rows.is_empty() {
        return Ok(sql.to_string());
    }
    let mut map: std::collections::HashMap<String, Option<String>> = Default::default();
    for r in rows {
        let (n, a): (String, String) = (r.get(0), r.get(1));
        map.entry(n)
            .and_modify(|v| {
                if v.as_deref() != Some(a.as_str()) {
                    *v = None;
                }
            })
            .or_insert(Some(a));
    }
    // A name that is some file's own column (SQL name) stays as it is.
    let real: Vec<String> = client
        .query(
            "SELECT DISTINCT coalesce(alias, name) FROM as4tainr.file_fields
              WHERE library = ANY($1) AND coalesce(alias, name) = ANY($2)
                AND (NOT $4 OR file = ANY($3))",
            &[&libl, &names, &named_files, &scope],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    // Nor do names of files and libraries.
    let objects: Vec<String> = client
        .query(
            "SELECT DISTINCT name FROM as4tainr.objects
              WHERE (library = ANY($1) AND objtype = '*FILE' OR objtype = '*LIB') AND name = ANY($2)",
            &[&libl, &names],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut out = String::new();
    let mut last = 0;
    for (a, b) in words {
        let w = sql[a..b].to_ascii_uppercase();
        if let Some(Some(alias)) = map.get(&w) {
            if !real.contains(&w) && !objects.contains(&w) {
                out.push_str(&sql[last..a]);
                out.push_str(&crate::catalog::pg_name(alias));
                last = b;
            }
        }
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

/// The built-in global variable QSYS2.JOB_NAME (Db2 for i SQL Reference,
/// built-in global variables): the job's qualified name, number/user/name.
pub const JOB_NAME_SQL: &str = "as4tainr_svc.job_name()";

/// A string constant concatenated (CONCAT, ||) is VARCHAR, as in Db2
/// for i (verify68 o68_mapepire "exp": 'x' CONCAT C of a CHAR(5) is
/// VARCHAR "xab   "): cast, so that PostgreSQL does not take it as the
/// CHAR operand's type (whose blanks do not count).
fn concat_strings(toks: Vec<Tok>) -> Vec<Tok> {
    let is_concat = |toks: &[Tok], j: usize| match &toks[j] {
        Tok::Punct(p) => p == "||",
        Tok::Word(w) if w.eq_ignore_ascii_case("CONCAT") => !matches!(
            (j + 1..toks.len()).find(|&k| !matches!(toks[k], Tok::Space(_))).map(|k| &toks[k]),
            Some(Tok::Punct(p)) if p == "("
        ),
        _ => false,
    };
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        // Not X'..', BX'..', G'..' (a word right before the quote).
        let prefixed = i > 0 && matches!(toks[i - 1], Tok::Word(_));
        let joined = matches!(t, Tok::Str(_)) && !prefixed && {
            let prev = (0..i).rev().find(|&k| !matches!(toks[k], Tok::Space(_)));
            let next = (i + 1..toks.len()).find(|&k| !matches!(toks[k], Tok::Space(_)));
            prev.is_some_and(|k| is_concat(&toks, k)) || next.is_some_and(|k| is_concat(&toks, k))
        };
        if joined {
            out.extend([
                Tok::Word("CAST".into()),
                Tok::Punct("(".into()),
                t.clone(),
                Tok::Space(" ".into()),
                Tok::Word("AS".into()),
                Tok::Space(" ".into()),
                Tok::Word("varchar".into()),
                Tok::Punct(")".into()),
            ]);
        } else {
            out.push(t.clone());
        }
    }
    out
}

/// COUNT(...) [OVER (...)] is INTEGER in Db2 for i (SQL Reference,
/// COUNT: "The result of the function is a large integer"; verify69
/// p69_mapepire "ps3": INTEGER, display 11), BIGINT in PostgreSQL: cast.
fn count_integer(toks: Vec<Tok>) -> Vec<Tok> {
    let next_non_space =
        |toks: &[Tok], from: usize| (from..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)));
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut i = 0;
    let mut ends: Vec<usize> = Vec::new();
    while i < toks.len() {
        let is_count = matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("COUNT"))
            && !matches!(out.iter().rev().find(|t| !matches!(t, Tok::Space(_))), Some(Tok::Punct(p)) if p == ".");
        let open = if is_count {
            next_non_space(&toks, i + 1)
        } else {
            None
        };
        if let Some(o) = open.filter(|&o| toks[o] == Tok::Punct("(".into())) {
            if let Some((close, _)) = paren_span(&toks, o) {
                let mut end = close;
                if let Some(w) = next_non_space(&toks, close + 1) {
                    if matches!(&toks[w], Tok::Word(x) if x.eq_ignore_ascii_case("OVER")) {
                        if let Some(o2) = next_non_space(&toks, w + 1)
                            .filter(|&o2| toks[o2] == Tok::Punct("(".into()))
                        {
                            if let Some((c2, _)) = paren_span(&toks, o2) {
                                end = c2;
                            }
                        }
                    }
                }
                out.push(Tok::Word("CAST".into()));
                out.push(Tok::Punct("(".into()));
                ends.push(end);
            }
        }
        // COUNT_BIG: BIGINT, PostgreSQL's count (verify70 q70_mapepire
        // "ar": COUNT_BIG(*) OVER () BIGINT).
        match &toks[i] {
            Tok::Word(w) if w.eq_ignore_ascii_case("COUNT_BIG") => {
                out.push(Tok::Word("count".into()))
            }
            t => out.push(t.clone()),
        }
        while ends.last() == Some(&i) {
            ends.pop();
            out.push(Tok::Space(" ".into()));
            out.push(Tok::Word("AS".into()));
            out.push(Tok::Space(" ".into()));
            out.push(Tok::Word("integer".into()));
            out.push(Tok::Punct(")".into()));
        }
        i += 1;
    }
    out
}

/// LISTAGG(x [, separator]) WITHIN GROUP (ORDER BY ...) (Db2 for i SQL
/// Reference, LISTAGG) as PostgreSQL's string_agg(x, separator ORDER BY
/// ...); the separator is an empty string when there is none.
fn listagg(mut toks: Vec<Tok>) -> Vec<Tok> {
    let mut i = 0;
    while i < toks.len() {
        if word(&toks[i]).as_deref() != Some("LISTAGG") {
            i += 1;
            continue;
        }
        let next = |toks: &[Tok], from: usize| {
            (from..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)))
        };
        let Some(o) = next(&toks, i + 1).filter(|&o| toks[o] == Tok::Punct("(".into())) else {
            i += 1;
            continue;
        };
        let Some((c, commas)) = paren_span(&toks, o) else {
            break;
        };
        toks[i] = Tok::Word("string_agg".into());
        let mut order: Vec<Tok> = Vec::new();
        let within = next(&toks, c + 1).filter(|&j| word(&toks[j]).as_deref() == Some("WITHIN"));
        let group = within
            .and_then(|w| next(&toks, w + 1))
            .filter(|&j| word(&toks[j]).as_deref() == Some("GROUP"));
        let gopen = group
            .and_then(|g| next(&toks, g + 1))
            .filter(|&j| toks[j] == Tok::Punct("(".into()));
        let mut end = c;
        if let Some(go) = gopen {
            if let Some((gc, _)) = paren_span(&toks, go) {
                order = toks[go + 1..gc].to_vec();
                end = gc;
            }
        }
        let mut tail: Vec<Tok> = Vec::new();
        if commas.is_empty() {
            tail.push(Tok::Punct(",".into()));
            tail.push(Tok::Str(String::new()));
        }
        if !order.is_empty() {
            tail.push(Tok::Space(" ".into()));
            tail.extend(order);
        }
        tail.push(Tok::Punct(")".into()));
        toks.splice(c..=end, tail);
        i += 1;
    }
    toks
}

fn word(t: &Tok) -> Option<String> {
    match t {
        Tok::Word(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

/// A token as an error message names it.
fn show_tok(t: &Tok) -> String {
    render(std::slice::from_ref(t))
}

fn render(toks: &[Tok]) -> String {
    toks.iter()
        .map(|t| match t {
            Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
            Tok::Str(s) => format!("'{}'", s.replace('\'', "''")),
            // DB2 names are upper case; as4tainr stores them lower case.
            Tok::Quoted(s) => ident(s),
        })
        .collect()
}

/// Replace unqualified names (variables, parameters) by expressions,
/// e.g. to find out a routine query's result columns without its
/// parameters' values.
pub(crate) fn substitute_names(sql: &str, names: &[(String, String)]) -> Result<String> {
    let toks = tokenize(sql)?;
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        let dot = |j: Option<&Tok>| matches!(j, Some(Tok::Punct(p)) if p == ".");
        if let Some(w) = word(t) {
            let qualified = dot(i.checked_sub(1).and_then(|j| toks.get(j))) || dot(toks.get(i + 1));
            if let Some((_, e)) = names
                .iter()
                .find(|(n, _)| !qualified && n.eq_ignore_ascii_case(&w))
            {
                out.push(Tok::Punct(e.clone()));
                continue;
            }
        }
        out.push(t.clone());
    }
    Ok(render(&out))
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

/// The column list an INSERT without one gets: the file's fields in
/// order, IMPLICITLY HIDDEN columns left out (PUB400 2026-10-26, verify2
/// w_ins_nocols: five values for a table of eight columns, three
/// hidden); for an alias over a source file member, SRCSEQ, SRCDAT and
/// SRCDTA (PUB400 2026-10-28, verify12 n12_ins_nocols).
pub async fn insert_columns(client: &Client, libl: &[String], name: &str) -> Result<Vec<String>> {
    if let Ok(f) = catalog::resolve_sql_file(client, libl, name).await {
        return Ok(f
            .file
            .fields
            .iter()
            .filter(|fd| !fd.hidden)
            .map(|fd| fd.column())
            .collect());
    }
    let (libs, file) = match name.split_once('/') {
        Some((l, f)) => (vec![l.to_string()], f.to_string()),
        None => (libl.to_vec(), name.to_string()),
    };
    for l in &libs {
        let alias = client
            .query_opt(
                "SELECT 1 FROM as4tainr.sql_aliases a JOIN as4tainr.objects o
                    ON o.library = a.target_library AND o.name = a.target_file
                   AND o.objtype = '*FILE' AND o.attribute = 'PF-SRC'
                 WHERE a.library = $1 AND a.name = $2",
                &[l, &file],
            )
            .await?;
        // A source file named itself (its first member: verify12
        // n12_ins_file).
        let source = client
            .query_opt(
                "SELECT 1 FROM as4tainr.objects WHERE library = $1 AND name = $2
                    AND objtype = '*FILE' AND attribute = 'PF-SRC'",
                &[l, &file],
            )
            .await?;
        if alias.is_some() || source.is_some() {
            return Ok(vec!["srcseq".into(), "srcdat".into(), "srcdta".into()]);
        }
    }
    bail!("SQL0204: {name} not found.")
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
/// The decimal type of a simple operand of `/`: a numeric constant, an
/// integer constant (DECIMAL(11, 0) in decimal arithmetic, as INTEGER is),
/// DECIMAL(x, p, s) / DEC(...) with constant precision and scale, or
/// CAST(... AS DECIMAL(p, s)). (precision, scale, is a decimal).
fn operand_type(toks: &[Tok], from: usize, to: usize) -> Option<(u32, u32, bool)> {
    let sig: Vec<usize> = (from..=to)
        .filter(|&j| !matches!(toks[j], Tok::Space(_)))
        .collect();
    let num = |n: &str| -> Option<(u32, u32, bool)> {
        if !n.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        match n.split_once('.') {
            Some((i, f)) => {
                let i = i.trim_start_matches('0');
                Some(((i.len() + f.len()).max(1) as u32, f.len() as u32, true))
            }
            None => Some((11, 0, false)),
        }
    };
    let column = |w: &Tok| -> Option<(u32, u32, bool)> {
        let n = match w {
            Tok::Word(w) => w.to_ascii_uppercase(),
            Tok::Quoted(q) => q.clone(),
            _ => return None,
        };
        COLUMN_TYPES.with(|c| c.borrow().get(&n).copied())
    };
    if let [one] = sig.as_slice() {
        if let Tok::Number(n) = &toks[*one] {
            return num(n);
        }
        return column(&toks[*one]);
    }
    // A qualified column: T.COL.
    if let [_, dot, col] = sig.as_slice() {
        if toks[*dot] == Tok::Punct(".".into()) {
            return column(&toks[*col]);
        }
    }
    let first = word(&toks[*sig.first()?])?;
    let open = *sig.get(1)?;
    if toks[open] != Tok::Punct("(".into()) {
        return None;
    }
    let (close, commas) = paren_span(toks, open)?;
    if close != to {
        return None;
    }
    let int_at = |a: usize, b: usize| -> Option<u32> {
        let v: Vec<&Tok> = toks[a + 1..b]
            .iter()
            .filter(|t| !matches!(t, Tok::Space(_)))
            .collect();
        match v.as_slice() {
            [Tok::Number(n)] => n.parse().ok(),
            _ => None,
        }
    };
    match first.as_str() {
        "DEC" | "DECIMAL" => {
            let ends: Vec<usize> = commas.iter().copied().chain([close]).collect();
            let p = if commas.is_empty() {
                15
            } else {
                int_at(ends[0], ends[1])?
            };
            let sc = if commas.len() < 2 {
                0
            } else {
                int_at(ends[1], ends[2])?
            };
            Some((p, sc, true))
        }
        "CAST" => {
            let a = (open..close)
                .rev()
                .find(|&j| word(&toks[j]).as_deref() == Some("AS"))?;
            let t = (a + 1..close).find(|&j| !matches!(toks[j], Tok::Space(_)))?;
            if !matches!(
                word(&toks[t]).as_deref(),
                Some("DEC" | "DECIMAL" | "NUMERIC")
            ) {
                return None;
            }
            let tp = (t + 1..close).find(|&j| !matches!(toks[j], Tok::Space(_)));
            match tp {
                Some(tp) if toks[tp] == Tok::Punct("(".into()) => {
                    let (c2, cm) = paren_span(toks, tp)?;
                    let p = int_at(tp, *cm.first().unwrap_or(&c2))?;
                    let sc = match cm.first() {
                        Some(&k) => int_at(k, c2)?,
                        None => 0,
                    };
                    Some((p, sc, true))
                }
                _ => Some((5, 0, true)),
            }
        }
        _ => None,
    }
}

thread_local! {
    /// The numeric columns of the files a statement names, for
    /// `operand_type` while it is translated (`with_column_types`): name
    /// (upper case) → precision, scale, decimal (false: an integer).
    static COLUMN_TYPES: std::cell::RefCell<std::collections::HashMap<String, (u32, u32, bool)>> =
        Default::default();
}

/// Whether a statement's values depend on its columns' types (a
/// division's scale, AVG of integers): then they are looked up.
fn needs_column_types(sql: &str) -> bool {
    sql.contains('/') || sql.to_ascii_uppercase().contains("AVG")
}

/// Translate with the numeric columns' types known (`numeric_columns`).
pub(crate) fn translate_typed(
    sql: &str,
    is_library: &dyn Fn(&str) -> bool,
    types: std::collections::HashMap<String, (u32, u32, bool)>,
) -> Result<Translated> {
    COLUMN_TYPES.with(|c| *c.borrow_mut() = types);
    let r = translate(sql, is_library);
    COLUMN_TYPES.with(|c| c.borrow_mut().clear());
    r
}

/// The numeric columns of the files a statement names after FROM, JOIN,
/// UPDATE and INTO: name → (precision, scale, decimal). A name two files
/// give different types is left out. Integers are DECIMAL(5, 0),
/// (11, 0) and (19, 0) in arithmetic (SQL Reference, "Arithmetic with an
/// integer and a decimal operand").
pub(crate) async fn numeric_columns(
    client: &Client,
    libl: &[String],
    sql: &str,
) -> std::collections::HashMap<String, (u32, u32, bool)> {
    let mut out: std::collections::HashMap<String, (u32, u32, bool)> = Default::default();
    let mut clash: std::collections::HashSet<String> = Default::default();
    let Ok(toks) = tokenize(sql.trim()) else {
        return out;
    };
    let sig: Vec<&Tok> = toks
        .iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let mut names = Vec::new();
    for (j, t) in sig.iter().enumerate() {
        let Some(w) = word(t) else { continue };
        if !matches!(w.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            continue;
        }
        let mut name = match sig.get(j + 1) {
            Some(Tok::Word(n)) => n.to_ascii_uppercase(),
            _ => continue,
        };
        if let (Some(Tok::Punct(d)), Some(Tok::Word(f))) = (sig.get(j + 2), sig.get(j + 3)) {
            if d == "." || d == "/" {
                name = format!("{name}/{}", f.to_ascii_uppercase());
            }
        }
        names.push(name);
    }
    for n in names {
        let Ok(f) = crate::catalog::resolve_sql_file(client, libl, &n).await else {
            continue;
        };
        for fd in &f.file.fields {
            use crate::dds::DataType as D;
            let ty = match fd.data_type {
                D::Packed | D::Zoned => (fd.length, fd.decimals, true),
                D::Binary if fd.decimals == 0 => match fd.length {
                    0..=4 => (5, 0, false),
                    5..=9 => (11, 0, false),
                    _ => (19, 0, false),
                },
                _ => continue,
            };
            for key in [
                Some(fd.name.to_ascii_uppercase()),
                fd.alias.as_ref().map(|a| a.to_ascii_uppercase()),
            ]
            .into_iter()
            .flatten()
            {
                match out.get(&key) {
                    Some(t) if *t != ty => {
                        clash.insert(key);
                    }
                    _ => {
                        out.insert(key, ty);
                    }
                }
            }
        }
    }
    for k in clash {
        out.remove(&k);
    }
    out
}

/// Where the operand ending at `pe` starts: a number, a column (T.COL),
/// or a call.
fn left_operand(toks: &[Tok], pe: usize) -> Option<usize> {
    match &toks[pe] {
        Tok::Number(_) => Some(pe),
        // A column, or a qualified one (T.COL).
        Tok::Word(_) | Tok::Quoted(_) => {
            let before = (0..pe).rev().find(|&j| !matches!(toks[j], Tok::Space(_)));
            match before {
                Some(d) if toks[d] == Tok::Punct(".".into()) => {
                    (0..d).rev().find(|&j| !matches!(toks[j], Tok::Space(_)))
                }
                _ => Some(pe),
            }
        }
        Tok::Punct(p) if p == ")" => {
            let mut depth = 0i32;
            let open = (0..=pe).rev().find(|&j| {
                match &toks[j] {
                    Tok::Punct(p) if p == ")" => depth += 1,
                    Tok::Punct(p) if p == "(" => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            open.and_then(|o| (0..o).rev().find(|&j| !matches!(toks[j], Tok::Space(_))))
        }
        _ => None,
    }
}

/// Where the operand starting at `ns` ends: a number, a column (T.COL),
/// or a call.
fn right_operand(toks: &[Tok], ns: usize) -> Option<usize> {
    match &toks[ns] {
        Tok::Number(_) => Some(ns),
        Tok::Word(_) | Tok::Quoted(_) => {
            let after = (ns + 1..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)));
            match after {
                Some(o) if toks[o] == Tok::Punct("(".into()) => paren_span(toks, o).map(|(c, _)| c),
                // T.COL
                Some(d) if toks[d] == Tok::Punct(".".into()) => {
                    (d + 1..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)))
                }
                _ => Some(ns),
            }
        }
        _ => None,
    }
}

/// ` AS numeric(p, s))`: the end of a CAST.
fn numeric_type_toks(p: u32, s: u32) -> Vec<Tok> {
    vec![
        Tok::Space(" ".into()),
        Tok::Word("AS".into()),
        Tok::Space(" ".into()),
        Tok::Word("numeric".into()),
        Tok::Punct("(".into()),
        Tok::Number(p.to_string()),
        Tok::Punct(",".into()),
        Tok::Space(" ".into()),
        Tok::Number(s.to_string()),
        Tok::Punct(")".into()),
        Tok::Punct(")".into()),
    ]
}

/// Db2 for i's types of decimal arithmetic (SQL Reference, "Decimal
/// arithmetic in SQL", "Arithmetic with an integer and a decimal
/// operand", SUM, AVG): a + b and a - b DECIMAL(max(p - s, p' - s') +
/// max(s, s') + 1, max(s, s')), a * b DECIMAL(p + p', s + s'), a / b
/// DECIMAL(31, 31 - p + s - s') truncated, an integer operand
/// DECIMAL(5, 0), (11, 0) or (19, 0); SUM of a decimal DECIMAL(31, s),
/// AVG DECIMAL(31, 31 - p + s); SUM and AVG of SMALLINT or INTEGER
/// INTEGER (AVG truncated), of BIGINT BIGINT; ROUND, TRUNCATE, ABS, MIN,
/// MAX and unary minus keep the operand's type (verify70 q70_mapepire
/// "ar", verify71 r71_mapepire "agg", "mix", "vw").
///
/// Each expression made of numbers, columns whose types are known
/// (`COLUMN_TYPES`), calls and the operators is rewritten: divisions
/// with a decimal operand to as4tainr_svc.ddiv with Db2's scale, AVG to
/// a truncated quotient of SUM and COUNT, and the whole cast to its
/// DECIMAL(p, s) when it computes something. Values are PostgreSQL's
/// otherwise (exact: the same digits).
fn decimal_types(toks: Vec<Tok>) -> Vec<Tok> {
    rewrite_spans(&toks)
}

type NumType = (u32, u32, bool);

const SPAN_STOP: &[&str] = &[
    "FROM",
    "AS",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "ORDER",
    "GROUP",
    "BY",
    "HAVING",
    "THEN",
    "ELSE",
    "WHEN",
    "END",
    "CASE",
    "ON",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "OUTER",
    "FULL",
    "CROSS",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "FETCH",
    "FIRST",
    "NEXT",
    "ROWS",
    "ROW",
    "ONLY",
    "LIMIT",
    "OFFSET",
    "IS",
    "NULL",
    "IN",
    "LIKE",
    "BETWEEN",
    "DESC",
    "ASC",
    "INTO",
    "VALUES",
    "SET",
    "SELECT",
    "DISTINCT",
    "ALL",
    "WITH",
    "FOR",
    "UPDATE",
    "DELETE",
    "INSERT",
    "OVER",
    "PARTITION",
    "CURRENT",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "DAY",
    "DAYS",
    "MONTH",
    "MONTHS",
    "YEAR",
    "YEARS",
    "HOUR",
    "HOURS",
    "MINUTE",
    "MINUTES",
    "SECOND",
    "SECONDS",
    "MICROSECOND",
    "MICROSECONDS",
    "ESCAPE",
    "EXISTS",
    "ANY",
    "SOME",
    "USING",
    "NATURAL",
    "CONCAT",
    "RETURN",
    "DEFAULT",
    "COLLATE",
    "CAST",
    "TRUE",
    "FALSE",
];

/// Rewrite every arithmetic expression in `toks` (at any depth).
fn rewrite_spans(toks: &[Tok]) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        match span_end(toks, i) {
            Some(e) => {
                out.extend(rewrite_expr(&toks[i..e]));
                i = e;
            }
            None => {
                // Not an expression here: the token as it is, and what is
                // inside parentheses on its own.
                if toks[i] == Tok::Punct("(".into()) {
                    if let Some((c, _)) = paren_span(toks, i) {
                        out.push(toks[i].clone());
                        out.extend(rewrite_spans(&toks[i + 1..c]));
                        out.push(toks[c].clone());
                        i = c + 1;
                        continue;
                    }
                }
                out.push(toks[i].clone());
                i += 1;
            }
        }
    }
    out
}

fn is_stop_word(w: &str) -> bool {
    SPAN_STOP.iter().any(|s| s.eq_ignore_ascii_case(w))
}

/// Where an expression starting at `i` ends (exclusive): the run of
/// operands and arithmetic operators; None when there is no operand at
/// `i`.
fn span_end(toks: &[Tok], i: usize) -> Option<usize> {
    let starts = match &toks[i] {
        Tok::Number(_) | Tok::Quoted(_) => true,
        Tok::Word(w) => !is_stop_word(w),
        Tok::Punct(p) => p == "-" || p == "+" || p == "(",
        _ => false,
    };
    if !starts {
        return None;
    }
    // Not after something an expression continues from (a word or a
    // closing parenthesis: then this is not the start).
    let mut j = i;
    let mut last = None;
    while j < toks.len() {
        match &toks[j] {
            Tok::Space(_) => {}
            Tok::Number(_) | Tok::Quoted(_) => last = Some(j),
            Tok::Word(w) if !is_stop_word(w) => {
                let open = (j + 1..toks.len()).find(|&k| !matches!(toks[k], Tok::Space(_)));
                if let Some(o) = open.filter(|&o| toks[o] == Tok::Punct("(".into())) {
                    let (c, _) = paren_span(toks, o)?;
                    j = c;
                    // OVER (...)
                    let w2 = (c + 1..toks.len()).find(|&k| !matches!(toks[k], Tok::Space(_)));
                    if let Some(w2) = w2 {
                        if matches!(&toks[w2], Tok::Word(x) if x.eq_ignore_ascii_case("OVER")) {
                            let o2 =
                                (w2 + 1..toks.len()).find(|&k| !matches!(toks[k], Tok::Space(_)));
                            if let Some(o2) = o2.filter(|&o2| toks[o2] == Tok::Punct("(".into())) {
                                let (c2, _) = paren_span(toks, o2)?;
                                j = c2;
                            }
                        }
                    }
                }
                last = Some(j);
            }
            Tok::Punct(p) if p == "(" => {
                let (c, _) = paren_span(toks, j)?;
                j = c;
                last = Some(j);
            }
            Tok::Punct(p) if matches!(p.as_str(), "+" | "-" | "*" | "/" | ".") => {}
            _ => break,
        }
        j += 1;
    }
    last.map(|l| l + 1)
}

/// The tokens of an expression, rewritten, when it parses as arithmetic
/// on typed operands; else as they are, with the insides of its calls
/// and parentheses rewritten on their own.
fn rewrite_expr(toks: &[Tok]) -> Vec<Tok> {
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&j| !matches!(toks[j], Tok::Space(_)))
        .collect();
    let mut p = ExprParser {
        toks,
        sig: &sig,
        at: 0,
    };
    if let Some(e) = p.sum() {
        if p.at == sig.len() {
            let (mut out, ty, computes) = e;
            // Nothing rewritten: the tokens as written (LIB/FILE is a
            // qualified name, not a division).
            let same = |a: &[Tok], b: &[Tok]| {
                a.iter()
                    .filter(|t| !matches!(t, Tok::Space(_)))
                    .eq(b.iter().filter(|t| !matches!(t, Tok::Space(_))))
            };
            let typed = matches!((ty, computes), (Some((_, _, true)), true));
            if !typed && same(&out, toks) {
                return toks.to_vec();
            }
            if let (Some((pp, s, true)), true) = (ty, computes) {
                let mut w = vec![Tok::Word("CAST".into()), Tok::Punct("(".into())];
                w.append(&mut out);
                w.extend(numeric_type_toks(pp, s));
                return w;
            }
            return out;
        }
    }
    // Not arithmetic as a whole: the parts on their own.
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i] == Tok::Punct("(".into()) {
            if let Some((c, _)) = paren_span(toks, i) {
                out.push(toks[i].clone());
                out.extend(rewrite_spans(&toks[i + 1..c]));
                out.push(toks[c].clone());
                i = c + 1;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// A parsed piece: its tokens, its type, whether it computes something
/// (an operator or a typed function: then it is cast).
type Piece = (Vec<Tok>, Option<NumType>, bool);

struct ExprParser<'a> {
    toks: &'a [Tok],
    sig: &'a [usize],
    at: usize,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.sig.get(self.at).map(|&j| &self.toks[j])
    }

    fn punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if x == p)
    }

    fn sum(&mut self) -> Option<Piece> {
        let mut left = self.product()?;
        while self.punct("+") || self.punct("-") {
            let op = if self.punct("+") { "+" } else { "-" };
            self.at += 1;
            let right = self.product()?;
            let ty = match (left.1, right.1) {
                (Some((p1, s1, d1)), Some((p2, s2, d2))) if d1 || d2 => {
                    let s = s1.max(s2);
                    let p = (p1 - s1).max(p2 - s2) + s + 1;
                    (p <= 31).then_some((p, s, true))
                }
                (Some((p1, _, false)), Some((p2, _, false))) => Some((p1.max(p2), 0, false)),
                _ => None,
            };
            let mut t = left.0;
            t.push(Tok::Space(" ".into()));
            t.push(Tok::Punct(op.into()));
            t.push(Tok::Space(" ".into()));
            t.extend(right.0);
            left = (t, ty, true);
        }
        Some(left)
    }

    fn product(&mut self) -> Option<Piece> {
        let mut left = self.unary()?;
        while self.punct("*") || self.punct("/") {
            let div = self.punct("/");
            self.at += 1;
            let right = self.unary()?;
            let (t, ty) = match (div, left.1, right.1) {
                (false, Some((p1, s1, d1)), Some((p2, s2, d2))) => {
                    let ty = if d1 || d2 {
                        (p1 + p2 <= 31).then_some((p1 + p2, s1 + s2, true))
                    } else {
                        Some((p1.max(p2), 0, false))
                    };
                    let mut t = left.0;
                    t.push(Tok::Space(" ".into()));
                    t.push(Tok::Punct("*".into()));
                    t.push(Tok::Space(" ".into()));
                    t.extend(right.0);
                    (t, ty)
                }
                (true, Some((p1, s1, d1)), Some((_, s2, d2))) if d1 || d2 => {
                    let scale = (31i64 - p1 as i64 + s1 as i64 - s2 as i64).max(0) as u32;
                    let mut t = vec![
                        Tok::Word("as4tainr_svc.ddiv".into()),
                        Tok::Punct("(".into()),
                    ];
                    t.extend(left.0);
                    t.push(Tok::Punct(",".into()));
                    t.push(Tok::Space(" ".into()));
                    t.extend(right.0);
                    t.push(Tok::Punct(",".into()));
                    t.push(Tok::Space(" ".into()));
                    t.push(Tok::Number(scale.to_string()));
                    t.push(Tok::Punct(")".into()));
                    (t, Some((31, scale, true)))
                }
                (_, l, r) => {
                    // Integer division, or an operand of unknown type.
                    let ty = match (div, l, r) {
                        (true, Some((p1, _, false)), Some((p2, _, false))) => {
                            Some((p1.max(p2), 0, false))
                        }
                        _ => None,
                    };
                    let mut t = left.0;
                    t.push(Tok::Space(" ".into()));
                    t.push(Tok::Punct(if div { "/" } else { "*" }.into()));
                    t.push(Tok::Space(" ".into()));
                    t.extend(right.0);
                    (t, ty)
                }
            };
            left = (t, ty, true);
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Piece> {
        if self.punct("-") || self.punct("+") {
            let neg = self.punct("-");
            self.at += 1;
            // A negative number is a constant.
            if let Some(Tok::Number(n)) = self.peek() {
                let n = n.clone();
                self.at += 1;
                // As written: the sign apart (a later pass reads -1.5E-3
                // as a floating point constant).
                let t = vec![
                    Tok::Punct(if neg { "-" } else { "+" }.into()),
                    Tok::Number(n.clone()),
                ];
                return Some((t, number_type(&n), false));
            }
            let (t, ty, _) = self.unary()?;
            let mut out = vec![Tok::Punct(if neg { "-" } else { "+" }.into())];
            out.extend(t);
            return Some((out, ty, true));
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<Piece> {
        let j = *self.sig.get(self.at)?;
        match &self.toks[j] {
            Tok::Number(n) => {
                self.at += 1;
                Some((vec![self.toks[j].clone()], number_type(n), false))
            }
            Tok::Punct(p) if p == "(" => {
                let (c, _) = paren_span(self.toks, j)?;
                let inner = &self.toks[j + 1..c];
                self.skip_to(c);
                let isig: Vec<usize> = (0..inner.len())
                    .filter(|&k| !matches!(inner[k], Tok::Space(_)))
                    .collect();
                let mut ip = ExprParser {
                    toks: inner,
                    sig: &isig,
                    at: 0,
                };
                let mut t = vec![Tok::Punct("(".into())];
                match ip.sum() {
                    Some((it, ity, comp)) if ip.at == isig.len() => {
                        t.extend(it);
                        t.push(Tok::Punct(")".into()));
                        Some((t, ity, comp))
                    }
                    _ => {
                        t.extend(rewrite_spans(inner));
                        t.push(Tok::Punct(")".into()));
                        Some((t, None, false))
                    }
                }
            }
            Tok::Word(w) if !is_stop_word(w) => {
                let name = w.to_ascii_uppercase();
                let next = self.sig.get(self.at + 1).map(|&k| &self.toks[k]);
                if matches!(next, Some(Tok::Punct(p)) if p == "(") {
                    return self.call(j, &name);
                }
                // T.COL
                if matches!(next, Some(Tok::Punct(p)) if p == ".") {
                    let c = *self.sig.get(self.at + 2)?;
                    self.at += 3;
                    let ty = column_type(&self.toks[c]);
                    return Some((self.toks[j..=c].to_vec(), ty, false));
                }
                self.at += 1;
                Some((
                    vec![self.toks[j].clone()],
                    column_type(&self.toks[j]),
                    false,
                ))
            }
            Tok::Quoted(_) => {
                self.at += 1;
                Some((
                    vec![self.toks[j].clone()],
                    column_type(&self.toks[j]),
                    false,
                ))
            }
            _ => None,
        }
    }

    /// Move past the token at `j` (an index into `toks`).
    fn skip_to(&mut self, j: usize) {
        while self.at < self.sig.len() && self.sig[self.at] <= j {
            self.at += 1;
        }
    }

    fn call(&mut self, j: usize, name: &str) -> Option<Piece> {
        let o = self.sig[self.at + 1];
        let (c, commas) = paren_span(self.toks, o)?;
        let mut end = c;
        // OVER (...)
        let after = self.sig.iter().position(|&k| k > c).map(|q| self.sig[q]);
        if let Some(w) = after {
            if matches!(&self.toks[w], Tok::Word(x) if x.eq_ignore_ascii_case("OVER")) {
                let o2 = self.sig.iter().find(|&&k| k > w).copied();
                if let Some(o2) = o2.filter(|&o2| self.toks[o2] == Tok::Punct("(".into())) {
                    end = paren_span(self.toks, o2)?.0;
                }
            }
        }
        self.skip_to(end);
        let whole = &self.toks[j..=end];
        // CAST, DEC and DECIMAL: typed as they say.
        if matches!(name, "CAST" | "DEC" | "DECIMAL") {
            let ty = operand_type(self.toks, j, c);
            return Some((whole.to_vec(), ty, false));
        }
        let first_end = commas.first().copied().unwrap_or(c);
        let arg = &self.toks[o + 1..first_end];
        let asig: Vec<usize> = (0..arg.len())
            .filter(|&k| !matches!(arg[k], Tok::Space(_)))
            .collect();
        let mut ap = ExprParser {
            toks: arg,
            sig: &asig,
            at: 0,
        };
        let parsed = ap.sum().filter(|_| ap.at == asig.len());
        let distinct = matches!(asig.first().map(|&k| &arg[k]), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("DISTINCT"));
        // The call with its arguments rewritten, the first as parsed.
        let rest = || -> Vec<Tok> { rewrite_spans(&self.toks[first_end..=end]) };
        let rebuilt = |first: Vec<Tok>| -> Vec<Tok> {
            let mut t = self.toks[j..=o].to_vec();
            t.extend(first);
            t.extend(rest());
            t
        };
        let (atoks, aty) = match parsed {
            Some((t, ty, _)) => (t, ty),
            None => {
                let t = rebuilt(rewrite_spans(arg));
                return Some((t, None, false));
            }
        };
        match (name, aty) {
            ("ROUND" | "TRUNCATE" | "TRUNC" | "ABS" | "MIN" | "MAX", Some(ty @ (_, _, true))) => {
                Some((rebuilt(atoks), Some(ty), true))
            }
            ("SUM", Some((_, s, true))) => Some((rebuilt(atoks), Some((31, s, true)), true)),
            ("AVG", Some((p, s, true))) if !distinct => {
                // SUM / COUNT with Db2's scale, the rest cut off.
                let scale = 31 - p + s;
                let over = self.toks[c + 1..=end].to_vec();
                let mut t = vec![
                    Tok::Word("as4tainr_svc.ddiv".into()),
                    Tok::Punct("(".into()),
                    Tok::Word("SUM".into()),
                    Tok::Punct("(".into()),
                ];
                t.extend(atoks.clone());
                t.push(Tok::Punct(")".into()));
                t.extend(over.clone());
                t.push(Tok::Punct(",".into()));
                t.push(Tok::Space(" ".into()));
                t.push(Tok::Word("COUNT".into()));
                t.push(Tok::Punct("(".into()));
                t.extend(atoks);
                t.push(Tok::Punct(")".into()));
                t.extend(over);
                t.push(Tok::Punct(",".into()));
                t.push(Tok::Space(" ".into()));
                t.push(Tok::Number(scale.to_string()));
                t.push(Tok::Punct(")".into()));
                Some((t, Some((31, scale, true)), true))
            }
            // SUM and AVG of integers: INTEGER (BIGINT for BIGINT), AVG
            // truncated.
            ("SUM" | "AVG", Some((p, _, false))) => {
                let int = if p > 11 { "bigint" } else { "integer" };
                let mut t = vec![Tok::Word("CAST".into()), Tok::Punct("(".into())];
                if name == "AVG" {
                    t.push(Tok::Word("trunc".into()));
                    t.push(Tok::Punct("(".into()));
                }
                t.extend(rebuilt(atoks));
                if name == "AVG" {
                    t.push(Tok::Punct(")".into()));
                }
                t.extend([
                    Tok::Space(" ".into()),
                    Tok::Word("AS".into()),
                    Tok::Space(" ".into()),
                    Tok::Word(int.into()),
                    Tok::Punct(")".into()),
                ]);
                Some((t, Some((p.max(11), 0, false)), false))
            }
            _ => Some((rebuilt(atoks), None, false)),
        }
    }
}

fn number_type(n: &str) -> Option<NumType> {
    if !n.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    match n.split_once('.') {
        Some((i, f)) => {
            let i = i.trim_start_matches('0');
            Some(((i.len() + f.len()).max(1) as u32, f.len() as u32, true))
        }
        None => Some((11, 0, false)),
    }
}

fn column_type(t: &Tok) -> Option<NumType> {
    let n = match t {
        Tok::Word(w) => w.to_ascii_uppercase(),
        Tok::Quoted(q) => q.clone(),
        _ => return None,
    };
    COLUMN_TYPES.with(|c| c.borrow().get(&n).copied())
}

/// Decimal division as Db2 for i does it (IBM i SQL Reference, decimal
/// arithmetic: "The precision of the result of division is 31"; the scale
/// is 31 - p + s - s'): `a / b` of operands whose types are known here is
/// rewritten to as4tainr_svc.ddiv(a, b, scale). Other divisions keep
/// PostgreSQL's scale.
fn decimal_division(toks: Vec<Tok>) -> Vec<Tok> {
    let mut toks = toks;
    let mut k = 0;
    while k < toks.len() {
        if toks[k] != Tok::Punct("/".into()) {
            k += 1;
            continue;
        }
        let prev = (0..k).rev().find(|&j| !matches!(toks[j], Tok::Space(_)));
        let next = (k + 1..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_)));
        let (Some(pe), Some(ns)) = (prev, next) else {
            k += 1;
            continue;
        };
        let ls = left_operand(&toks, pe);
        let re = right_operand(&toks, ns);
        let (Some(ls), Some(re)) = (ls, re) else {
            k += 1;
            continue;
        };
        let (Some((p, s, ld)), Some((_, s2, rd))) =
            (operand_type(&toks, ls, pe), operand_type(&toks, ns, re))
        else {
            k += 1;
            continue;
        };
        // Integer by integer stays integer division.
        if !ld && !rd {
            k += 1;
            continue;
        }
        let scale = (31i64 - p as i64 + s as i64 - s2 as i64).max(0);
        // DECIMAL(31, scale) (verify70 q70_mapepire "ar": D / D of
        // DECIMAL(7, 2) is DECIMAL(31, 24)).
        let mut new: Vec<Tok> = toks[..ls].to_vec();
        new.push(Tok::Word("CAST".into()));
        new.push(Tok::Punct("(".into()));
        new.push(Tok::Word("as4tainr_svc.ddiv".into()));
        new.push(Tok::Punct("(".into()));
        new.extend_from_slice(&toks[ls..=pe]);
        new.push(Tok::Punct(",".into()));
        new.extend_from_slice(&toks[ns..=re]);
        new.push(Tok::Punct(",".into()));
        new.push(Tok::Number(scale.to_string()));
        new.push(Tok::Punct(")".into()));
        new.extend(numeric_type_toks(31, scale as u32));
        let resume = new.len();
        new.extend_from_slice(&toks[re + 1..]);
        toks = new;
        k = resume;
    }
    toks
}

/// A searched WHEN of a CASE expression takes no EXISTS predicate
/// (SQL0104 at EXISTS) and no IN with a fullselect (SQL0115): PUB400
/// 2026-10-27, tools/golden/verify3.txt x_errors (a select list) and
/// verify4.txt y_ids (a VALUES list, an UPDATE's SET, CASE WHEN 1 IN
/// (SELECT ...)); EXISTS in a WHERE clause is fine (y_ids).
fn check_case_conditions(toks: &[Tok]) -> Result<()> {
    if !toks
        .iter()
        .any(|t| matches!(t, Tok::Word(w) if w.eq_ignore_ascii_case("CASE")))
    {
        return Ok(());
    }
    let sig: Vec<&Tok> = toks
        .iter()
        .filter(|t| !matches!(t, Tok::Space(_)))
        .collect();
    let up = |t: &Tok| match t {
        Tok::Word(w) => w.to_ascii_uppercase(),
        _ => String::new(),
    };
    let punct = |t: Option<&&Tok>, p: &str| matches!(t, Some(Tok::Punct(q)) if q == p);
    // The CASE expressions open around a token: (paren depth, in a WHEN
    // condition).
    let mut cases: Vec<(i32, bool)> = Vec::new();
    let mut depth = 0i32;
    for (k, t) in sig.iter().enumerate() {
        if punct(Some(t), "(") {
            depth += 1;
            continue;
        }
        if punct(Some(t), ")") {
            depth -= 1;
            continue;
        }
        let w = up(t);
        match w.as_str() {
            "CASE" => cases.push((depth, false)),
            "END" if cases.last().is_some_and(|c| c.0 == depth) => {
                cases.pop();
            }
            "WHEN" if cases.last().is_some_and(|c| c.0 == depth) => {
                cases.last_mut().unwrap().1 = true;
            }
            "THEN" | "ELSE" if cases.last().is_some_and(|c| c.0 == depth) => {
                cases.last_mut().unwrap().1 = false;
            }
            "EXISTS" if cases.last().is_some_and(|c| c.1 && c.0 == depth) => {
                bail!("SQL0104: EXISTS is not valid in the condition of a CASE WHEN.");
            }
            "IN" if cases.last().is_some_and(|c| c.1 && c.0 == depth)
                && punct(sig.get(k + 1), "(")
                && sig.get(k + 2).is_some_and(|t| up(t) == "SELECT") =>
            {
                bail!(
                    "SQL0115: IN with a fullselect is not valid in the condition of a CASE WHEN."
                );
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn translate(sql: &str, is_library: &dyn Fn(&str) -> bool) -> Result<Translated> {
    check_case_conditions(&tokenize(sql.trim())?)?;
    let mut toks = concat_strings(count_integer(listagg(decimal_division(decimal_types(
        sequence_values(
            crate::sqljsonfn::rewrite(crate::sqljson::rewrite(tokenize(sql.trim())?)?)?,
            is_library,
        ),
    )))));
    // QTEMP.X and QTEMP/X: the job's own library (qtemp.rs). SESSION.X
    // names a declared global temporary table, which lives in QTEMP.
    for i in 0..toks.len() {
        let dot = matches!(toks.get(i + 1), Some(Tok::Punct(p)) if p == ".");
        if dot
            && matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("SESSION"))
            && !is_library("SESSION")
        {
            toks[i] = Tok::Word("QTEMP".into());
        }
    }
    if let Some(real) = crate::qtemp::current() {
        let mut i = 0;
        while i < toks.len() {
            let next = toks.get(i + 1).cloned();
            if let (Tok::Word(w), Some(Tok::Punct(p))) = (&toks[i], next) {
                if w.eq_ignore_ascii_case("QTEMP") {
                    if p == "." {
                        toks[i] = Tok::Word(ident(&real));
                    } else if p == "/" {
                        if let Some(Tok::Word(file)) = toks.get(i + 2).cloned() {
                            toks.splice(
                                i..i + 3,
                                [Tok::Word(format!("{}.{}", ident(&real), ident(&file)))],
                            );
                        }
                    }
                }
            }
            i += 1;
        }
    }
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
    // SELECT FROM: Db2 takes FROM as a column and the next name as its
    // correlation name, and refuses the token after them, naming the
    // tokens that could come there (PUB400 2026-11-10, verify75
    // u75_badsel: "select from qiws.qcustcdt" refuses ".", tokens ", FROM
    // INTO"; Code for IBM i's "Test runSQL (bad basic select)"; PostgreSQL
    // takes an empty select list).
    {
        let solid: Vec<&Tok> = toks
            .iter()
            .filter(|t| !matches!(t, Tok::Space(_)))
            .collect();
        if let Some(k) = solid.windows(2).position(|w| {
            matches!((word(w[0]), word(w[1])), (Some(a), Some(b)) if a == "SELECT" && b == "FROM")
        }) {
            let bad = match solid.get(k + 2) {
                Some(t) if word(t).is_some() => solid.get(k + 3).map(|t| show_tok(t)),
                Some(t) => Some(show_tok(t)),
                None => None,
            };
            if let Some(bad) = bad {
                bail!("SQL0104: {bad} is not expected here; what can come there: , FROM INTO.");
            }
        }
    }
    // VALUES 'A', 'B': one row for each expression (Db2: VALUES with a
    // list of expressions, none in parentheses); PostgreSQL wants each
    // row in parentheses. Code for IBM i lists libraries so.
    values_rows(&mut toks);
    // CAST(... AS VARCHAR(10) FOR BIT DATA): the characters as they are
    // (CCSID 65535 here is the job's: as4tainr keeps one code page).
    drop_for_data(&mut toks);
    // Binary string constants BX'C1C2' and hexadecimal constants X'C1C2':
    // bytes, as PostgreSQL bytea constants.
    let mut i = 0;
    while i + 1 < toks.len() {
        let hex = matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("BX") || w.eq_ignore_ascii_case("X"));
        if let (true, Tok::Str(h)) = (hex, &toks[i + 1]) {
            if h.len() % 2 != 0 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("SQL0110: {h} is not a valid hexadecimal constant.");
            }
            // CAST(X'7B' AS VARCHAR(1)): characters, the bytes in the
            // job's CCSID (37 here): Code for IBM i reads the system's
            // variant characters so (X'7B7C5B' is "#@$" in CCSID 37).
            let lit = if cast_to_character(&toks, i) {
                let bytes: Vec<u8> = (0..h.len() / 2)
                    .filter_map(|k| u8::from_str_radix(&h[2 * k..2 * k + 2], 16).ok())
                    .collect();
                let text = crate::ifs::codec::text(37, &bytes);
                format!("'{}'", text.replace('\'', "''"))
            } else {
                format!("'\\x{h}'::bytea")
            };
            toks.splice(i..i + 2, [Tok::Punct(lit)]);
        }
        i += 1;
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
            if w == "AS4TAINR"
                || w == "AS4TAINR_SVC"
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
    // Closing parentheses of TABLE(...) table references, left out.
    let mut skip_close: Vec<usize> = Vec::new();
    // Closing parentheses replaced: (position, text).
    let mut close_with: Vec<(usize, String)> = Vec::new();
    while i < toks.len() {
        if skip_close.contains(&i) {
            i += 1;
            continue;
        }
        // The closing parenthesis of a function written differently in
        // PostgreSQL.
        if let Some(k) = close_with.iter().position(|(at, _)| *at == i) {
            out.push(Tok::Punct(close_with.remove(k).1));
            i += 1;
            continue;
        }
        let t = &toks[i];
        let w = word(t);
        // System naming: LIB/FILE, written without blanks; either name
        // may be a delimited identifier ("QGPL"/"CUSTMAST", as IBM i
        // Access Client Solutions' Data Transfer writes them; Db2 for i
        // SQL Reference, naming conventions).
        let part = |t: Option<&Tok>| match t {
            Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
            Some(Tok::Quoted(q)) => Some(q.clone()),
            _ => None,
        };
        if let (Some(lib), Some(Tok::Punct(p)), Some(file)) =
            (part(Some(t)), toks.get(i + 1), part(toks.get(i + 2)))
        {
            if p == "/" && is_library(&lib) {
                out.push(Tok::Word(format!("{}.{}", ident(&lib), ident(&file))));
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
        // A number with an exponent is floating point in Db2 (Db2 for i SQL
        // Reference, constants); PostgreSQL would read it as numeric.
        if let Tok::Number(n) = t {
            if n.contains(['e', 'E'])
                && n.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '.')
            {
                out.push(Tok::Punct(format!("CAST({n} AS double precision)")));
                i += 1;
                continue;
            }
        }
        // Labeled durations (Db2 for i SQL Reference, datetime arithmetic):
        // `expr + 1 MONTH`, `(n) DAYS` as intervals; a month added to the
        // 31st ends at the month's last day, as in Db2.
        if let Some(unit) = w.as_deref().and_then(|w| match w {
            "YEAR" | "YEARS" => Some("year"),
            "MONTH" | "MONTHS" => Some("month"),
            "DAY" | "DAYS" => Some("day"),
            "HOUR" | "HOURS" => Some("hour"),
            "MINUTE" | "MINUTES" => Some("minute"),
            "SECOND" | "SECONDS" => Some("second"),
            "MICROSECOND" | "MICROSECONDS" => Some("microsecond"),
            _ => None,
        }) {
            let after_value = matches!(
                out.iter().rev().find(|t| !matches!(t, Tok::Space(_))),
                Some(Tok::Number(_))
            ) || matches!(
                out.iter().rev().find(|t| !matches!(t, Tok::Space(_))),
                Some(Tok::Punct(p)) if p == ")"
            );
            let call =
                next_non_space(&toks, i + 1).is_some_and(|o| toks[o] == Tok::Punct("(".into()));
            if after_value && !call {
                out.push(Tok::Punct(format!("* interval '1 {unit}'")));
                i += 1;
                continue;
            }
        }
        let prev_word = out
            .iter()
            .rev()
            .find(|t| !matches!(t, Tok::Space(_)))
            .and_then(|t| match t {
                Tok::Punct(p) if p.ends_with(" AS") => Some("AS".to_string()),
                t => word(t),
            });
        let call_open = next_non_space(&toks, i + 1).filter(|&o| toks[o] == Tok::Punct("(".into()));
        match w.as_deref() {
            // CAST(x AS INTEGER | SMALLINT | BIGINT | DECIMAL(p, s) |
            // NUMERIC(p, s)): Db2 for i cuts off the fraction digits that do
            // not fit (Db2 for i SQL Reference, numeric assignments: "the
            // fractional part of the number is eliminated"; trailing digits
            // of a decimal are eliminated); PostgreSQL would round.
            Some("CAST") if call_open.is_some() => {
                let o = call_open.unwrap();
                if let Some((c, _)) = paren_span(&toks, o) {
                    let mut depth = 0i32;
                    let as_at = (o..c).find(|&j| {
                        match &toks[j] {
                            Tok::Punct(p) if p == "(" => depth += 1,
                            Tok::Punct(p) if p == ")" => depth -= 1,
                            t if depth == 1 && word(t).as_deref() == Some("AS") => return true,
                            _ => {}
                        }
                        false
                    });
                    if let Some(a) = as_at {
                        let target = next_non_space(&toks, a + 1).and_then(|j| word(&toks[j]));
                        let scale = || -> Option<String> {
                            let t = next_non_space(&toks, a + 1)?;
                            let open = next_non_space(&toks, t + 1)
                                .filter(|&k| toks[k] == Tok::Punct("(".into()));
                            let Some(open) = open else {
                                return Some("0".into());
                            };
                            let (close, commas) = paren_span(&toks, open)?;
                            if commas.is_empty() {
                                return Some("0".into());
                            }
                            let v: Vec<&Tok> = toks[commas[0] + 1..close]
                                .iter()
                                .filter(|t| !matches!(t, Tok::Space(_)))
                                .collect();
                            match v.as_slice() {
                                [Tok::Number(n)] => Some(n.clone()),
                                _ => None,
                            }
                        };
                        let rewrite = match target.as_deref() {
                            Some("INTEGER" | "INT" | "SMALLINT" | "BIGINT") => {
                                Some(") AS numeric)) AS".to_string())
                            }
                            Some("DECIMAL" | "DEC" | "NUMERIC") => {
                                scale().map(|sc| format!(") AS numeric), {sc}) AS"))
                            }
                            _ => None,
                        };
                        if let Some(r) = rewrite {
                            out.push(Tok::Punct("CAST(trunc(CAST((".into()));
                            close_with.push((a, r));
                            i = o + 1;
                            continue;
                        }
                    }
                }
            }
            // CHAR(value [, ISO | USA | EUR | JIS | LOCAL | length]) as a
            // function (not a data type after AS): Db2's formats
            // (sqlfn.rs).
            Some("CHAR" | "CHARACTER")
                if call_open.is_some() && prev_word.as_deref() != Some("AS") =>
            {
                let o = call_open.unwrap();
                if let Some((_, commas)) = paren_span(&toks, o) {
                    if let [comma] = commas.as_slice() {
                        if let Some(f) = next_non_space(&toks, comma + 1) {
                            if let Some(fmt) = word(&toks[f]) {
                                if ["ISO", "USA", "EUR", "JIS", "LOCAL"].contains(&fmt.as_str()) {
                                    let fmt = if fmt == "LOCAL" {
                                        "ISO".to_string()
                                    } else {
                                        fmt
                                    };
                                    close_with.push((f, format!("'{fmt}'")));
                                }
                            }
                        }
                    }
                }
                out.push(Tok::Word("as4tainr_svc.char".into()));
                i += 1;
                continue;
            }
            // DIGITS(x): the precision of DECIMAL(x, p, s) or CAST(x AS
            // DECIMAL(p, s)) gives the number of digits (Db2 for i SQL
            // Reference, DIGITS).
            Some("DIGITS") if call_open.is_some() => {
                let o = call_open.unwrap();
                if let Some((c, _)) = paren_span(&toks, o) {
                    let inner = next_non_space(&toks, o + 1);
                    let number_at = |k: usize| -> Option<String> {
                        let j = next_non_space(&toks, k + 1)?;
                        match &toks[j] {
                            Tok::Number(n) if n.chars().all(|c| c.is_ascii_digit()) => {
                                Some(n.clone())
                            }
                            _ => None,
                        }
                    };
                    let p = inner.and_then(|f| match word(&toks[f]).as_deref() {
                        Some("DEC" | "DECIMAL") => {
                            let open = next_non_space(&toks, f + 1)?;
                            let (_, commas) = paren_span(&toks, open)?;
                            match commas.first() {
                                Some(&k) => number_at(k),
                                None => Some("15".into()),
                            }
                        }
                        Some("CAST") => {
                            let open = next_non_space(&toks, f + 1)?;
                            let (close, _) = paren_span(&toks, open)?;
                            let a = (open..close)
                                .rev()
                                .find(|&j| word(&toks[j]).as_deref() == Some("AS"))?;
                            let t = next_non_space(&toks, a + 1)?;
                            if !matches!(
                                word(&toks[t]).as_deref(),
                                Some("DEC" | "DECIMAL" | "NUMERIC")
                            ) {
                                return None;
                            }
                            let tp = next_non_space(&toks, t + 1)?;
                            if toks[tp] != Tok::Punct("(".into()) {
                                return Some("5".into());
                            }
                            number_at(tp)
                        }
                        _ => None,
                    });
                    out.push(Tok::Word("as4tainr_svc.digits".into()));
                    if let Some(p) = p {
                        close_with.push((c, format!(", {p})")));
                    }
                    i += 1;
                    continue;
                }
            }
            // TIMESTAMP(string | date [, time]) as a function: the Db2
            // string form yyyy-mm-dd-hh.mm.ss.nnnnnn (sqlfn.rs); TIMESTAMP(6)
            // after AS is the data type.
            Some("TIMESTAMP")
                if call_open.is_some()
                    && prev_word.as_deref() != Some("AS")
                    && !matches!(
                        next_non_space(&toks, call_open.unwrap() + 1).map(|j| &toks[j]),
                        Some(Tok::Number(_))
                    ) =>
            {
                out.push(Tok::Word("as4tainr_svc.timestamp".into()));
                i += 1;
                continue;
            }
            Some("VARCHAR_FORMAT") if call_open.is_some() => {
                out.push(Tok::Word("as4tainr_svc.varchar_format".into()));
                i += 1;
                continue;
            }
            // CURRENT_TIMESTAMP, CURRENT_DATE, CURRENT_TIME: Db2's other
            // spelling of the special registers (Db2 for i SQL Reference,
            // special registers), read once for the statement as
            // CURRENT TIMESTAMP is.
            Some(w @ ("CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME"))
                if call_open.is_none() =>
            {
                out.push(Tok::Word(
                    match w {
                        "CURRENT_TIME" => STMT_TIME,
                        "CURRENT_TIMESTAMP" => STMT_TIMESTAMP,
                        _ => STMT_DATE,
                    }
                    .into(),
                ));
                i += 1;
                continue;
            }
            Some("CURRENT") => {
                if let Some(j) = next_non_space(&toks, i + 1) {
                    if let Some(n) = word(&toks[j]) {
                        // CURRENT TIME and CURRENT TIMESTAMP are local
                        // values without a time zone, TIME without
                        // fractions; each is read once for the statement,
                        // not for its unit of work (Db2 for i SQL
                        // Reference, special registers): PostgreSQL's
                        // statement_timestamp(), not its transaction's
                        // LOCALTIMESTAMP (a program under commitment
                        // control would see the time its transaction
                        // started).
                        if ["DATE", "TIME", "TIMESTAMP"].contains(&n.as_str()) {
                            out.push(Tok::Word(
                                match n.as_str() {
                                    "TIME" => STMT_TIME,
                                    "TIMESTAMP" => STMT_TIMESTAMP,
                                    _ => STMT_DATE,
                                }
                                .into(),
                            ));
                            i = j + 1;
                            continue;
                        }
                        // CURRENT USER (Db2 for i SQL Reference, special
                        // registers): the user profile, as USER here (no
                        // adopted authority). CURRENT SERVER: the
                        // relational database, the system's name.
                        if n == "USER" {
                            out.push(Tok::Punct(USER_SQL.into()));
                            i = j + 1;
                            continue;
                        }
                        if n == "SERVER" {
                            out.push(Tok::Punct(format!(
                                "CAST('{}' AS varchar(18))",
                                crate::report::system_name().replace('\'', "''")
                            )));
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
            // Date and time parts (Db2 for i SQL Reference, scalar
            // functions): integers, DAYOFWEEK 1 for Sunday, DAYS the days
            // since 0001-01-01 counting that day as 1.
            Some(
                f @ ("YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" | "MICROSECOND"
                | "DAYOFWEEK" | "DAYOFYEAR" | "QUARTER" | "DAYS"),
            ) if next_non_space(&toks, i + 1)
                .is_some_and(|o| toks[o] == Tok::Punct("(".into())) =>
            {
                let o = next_non_space(&toks, i + 1).unwrap();
                let mut depth = 0i32;
                let close = (o..toks.len()).find(|&j| {
                    match &toks[j] {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                });
                if let Some(c) = close {
                    let (open, end) = match f {
                        "DAYOFWEEK" => ("CAST(extract(dow FROM (", ")) AS integer) + 1"),
                        "DAYOFYEAR" => ("CAST(extract(doy FROM (", ")) AS integer)"),
                        "MICROSECOND" => (
                            "CAST(extract(microseconds FROM (",
                            ")) AS integer) % 1000000",
                        ),
                        "SECOND" => ("CAST(floor(extract(second FROM (", "))) AS integer)"),
                        "DAYS" => ("(CAST((", ") AS date) - DATE '0001-01-01' + 1)"),
                        other => {
                            out.push(Tok::Punct(format!(
                                "CAST(extract({} FROM (",
                                other.to_ascii_lowercase()
                            )));
                            close_with.push((c, ")) AS integer)".into()));
                            i = o + 1;
                            continue;
                        }
                    };
                    out.push(Tok::Punct(open.into()));
                    close_with.push((c, end.into()));
                    i = o + 1;
                    continue;
                }
            }
            // DECIMAL(numeric-expression [, precision [, scale]]), DEC
            // (Db2 for i SQL Reference, DECIMAL or DEC): the value as a
            // decimal number, extra fraction digits cut off (not
            // rounded), precision 15 and scale 0 by default.
            // (DECIMAL(5, 2) after a name, AS or RETURNS is the data type.)
            Some("DEC" | "DECIMAL" | "ZONED")
                if next_non_space(&toks, i + 1)
                    .is_some_and(|o| toks[o] == Tok::Punct("(".into()))
                    && function_position(&out) =>
            {
                let o = next_non_space(&toks, i + 1).unwrap();
                let mut depth = 0i32;
                let mut commas = Vec::new();
                let close = (o..toks.len()).find(|&j| {
                    match &toks[j] {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        Tok::Punct(p) if p == "," && depth == 1 => commas.push(j),
                        _ => {}
                    }
                    depth == 0
                });
                let number = |a: usize, b: usize| -> Option<String> {
                    let v: Vec<&Tok> = toks[a + 1..b]
                        .iter()
                        .filter(|t| !matches!(t, Tok::Space(_)))
                        .collect();
                    match v.as_slice() {
                        [Tok::Number(n)] if n.chars().all(|c| c.is_ascii_digit()) => {
                            Some(n.clone())
                        }
                        _ => None,
                    }
                };
                if let Some(c) = close {
                    let ends: Vec<usize> = commas.iter().copied().chain([c]).collect();
                    let p = if commas.is_empty() {
                        Some("15".to_string())
                    } else {
                        number(ends[0], ends[1])
                    };
                    let sc = if commas.len() < 2 {
                        Some("0".to_string())
                    } else {
                        number(ends[1], ends[2])
                    };
                    if let (Some(p), Some(sc), true) = (p, sc, commas.len() <= 2) {
                        out.push(Tok::Punct("CAST(trunc(CAST((".into()));
                        let first = ends[0];
                        close_with
                            .push((first, format!(") AS numeric), {sc}) AS numeric({p},{sc}))")));
                        skip_close.extend(first + 1..=c);
                        i = o + 1;
                        continue;
                    }
                }
            }
            // ROUND and TRUNCATE (TRUNC) of any number (Db2 for i SQL
            // Reference): PostgreSQL rounds a double precision value only
            // to an integer, so the value is rounded as a decimal number.
            Some(f @ ("ROUND" | "TRUNCATE" | "TRUNC"))
                if next_non_space(&toks, i + 1)
                    .is_some_and(|o| toks[o] == Tok::Punct("(".into())) =>
            {
                let o = next_non_space(&toks, i + 1).unwrap();
                let mut depth = 0i32;
                let mut comma = None;
                let close = (o..toks.len()).find(|&j| {
                    match &toks[j] {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        Tok::Punct(p) if p == "," && depth == 1 && comma.is_none() => {
                            comma = Some(j)
                        }
                        _ => {}
                    }
                    depth == 0
                });
                if let (Some(_), Some(k)) = (close, comma) {
                    // The result keeps the argument's precision and scale
                    // (Db2 for i SQL Reference, ROUND and TRUNCATE;
                    // ROUND(2.675, 2) is 2.680 on IBM i 7.5).
                    let name = if f == "ROUND" {
                        "as4tainr_svc.round"
                    } else {
                        "as4tainr_svc.truncate"
                    };
                    out.push(Tok::Punct(format!("{name}(CAST((")));
                    close_with.push((k, ") AS numeric),".into()));
                    i = o + 1;
                    continue;
                }
            }
            // QSYS2.JOB_NAME: the built-in global variable.
            Some("QSYS2")
                if next_non_space(&toks, i + 1)
                    .is_some_and(|j| matches!(&toks[j], Tok::Punct(p) if p == "."))
                    && {
                        let j = next_non_space(&toks, i + 1).unwrap_or(i);
                        next_non_space(&toks, j + 1)
                            .is_some_and(|k| word(&toks[k]).as_deref() == Some("JOB_NAME"))
                            && !next_non_space(&toks, j + 1)
                                .and_then(|k| next_non_space(&toks, k + 1))
                                .is_some_and(|m| matches!(&toks[m], Tok::Punct(p) if p == "("))
                    } =>
            {
                let j = next_non_space(&toks, i + 1).unwrap_or(i);
                let k = next_non_space(&toks, j + 1).unwrap_or(j);
                out.push(Tok::Punct(JOB_NAME_SQL.into()));
                i = k + 1;
                continue;
            }
            // USER (Db2 for i SQL Reference, special registers): the
            // job's user profile; record-level access and triggers know
            // the job by its name (number/user/name).
            Some("USER" | "SESSION_USER" | "SYSTEM_USER" | "CURRENT_USER")
                if !out
                    .iter()
                    .rev()
                    .find(|t| !matches!(t, Tok::Space(_)))
                    .is_some_and(|t| matches!(t, Tok::Punct(p) if p == "."))
                    && !next_non_space(&toks, i + 1).is_some_and(
                        |j| matches!(&toks[j], Tok::Punct(p) if p == "." || p == "("),
                    ) =>
            {
                out.push(Tok::Punct(USER_SQL.into()));
                i += 1;
                continue;
            }
            // CAST(x AS CLOB(10M)): large objects are text and bytea
            // here (the length, with its K, M or G, is not kept).
            Some(w @ ("CLOB" | "DBCLOB" | "BLOB"))
                if out
                    .iter()
                    .rev()
                    .find(|t| !matches!(t, Tok::Space(_)))
                    .and_then(word)
                    .as_deref()
                    == Some("AS") =>
            {
                out.push(Tok::Word(if w == "BLOB" { "bytea" } else { "text" }.into()));
                i += 1;
                if let Some(o) = next_non_space(&toks, i) {
                    if toks[o] == Tok::Punct("(".into()) {
                        if let Some(c) =
                            (o..toks.len()).find(|&j| toks[j] == Tok::Punct(")".into()))
                        {
                            i = c + 1;
                        }
                    }
                }
                continue;
            }
            // CAST(x AS DOUBLE): Db2's DOUBLE is DOUBLE PRECISION.
            Some("DOUBLE")
                if out
                    .iter()
                    .rev()
                    .find(|t| !matches!(t, Tok::Space(_)))
                    .and_then(word)
                    .as_deref()
                    == Some("AS")
                    && next_non_space(&toks, i + 1)
                        .and_then(|j| word(&toks[j]))
                        .as_deref()
                        != Some("PRECISION") =>
            {
                out.push(Tok::Word("double precision".into()));
                i += 1;
                continue;
            }
            // TIME(expression) (Db2 for i SQL Reference, TIME): the
            // time of a timestamp or time; TIME(6) as a data type stays.
            Some("TIME")
                if next_non_space(&toks, i + 1)
                    .is_some_and(|o| toks[o] == Tok::Punct("(".into()))
                    && {
                        let o = next_non_space(&toks, i + 1).unwrap();
                        !matches!(
                            (next_non_space(&toks, o + 1).map(|j| &toks[j]), next_non_space(&toks, o + 1).and_then(|j| next_non_space(&toks, j + 1)).map(|j| &toks[j])),
                            (Some(Tok::Number(_)), Some(Tok::Punct(p))) if p == ")"
                        )
                    } =>
            {
                let o = next_non_space(&toks, i + 1).unwrap();
                let mut depth = 0i32;
                let close = (o..toks.len()).find(|&j| {
                    match &toks[j] {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                });
                if let Some(c) = close {
                    out.push(Tok::Punct("CAST((".into()));
                    close_with.push((c, ") AS time(0))".into()));
                    i = o + 1;
                    continue;
                }
            }
            // INTERPRET(bytes AS type) (Db2 for i SQL Reference): bytes
            // read as the type's internal form (packed, zoned, binary,
            // EBCDIC characters).
            Some("INTERPRET")
                if next_non_space(&toks, i + 1)
                    .is_some_and(|o| toks[o] == Tok::Punct("(".into())) =>
            {
                let o = next_non_space(&toks, i + 1).unwrap();
                let mut depth = 0i32;
                let mut as_at = None;
                let close = (o..toks.len()).find(|&j| {
                    match &toks[j] {
                        Tok::Punct(p) if p == "(" => depth += 1,
                        Tok::Punct(p) if p == ")" => depth -= 1,
                        t if depth == 1 && word(t).as_deref() == Some("AS") => as_at = Some(j),
                        _ => {}
                    }
                    depth == 0
                });
                let (Some(c), Some(a)) = (close, as_at) else {
                    bail!("SQL0104: INTERPRET needs (expression AS data type).");
                };
                let ty: Vec<&Tok> = toks[a + 1..c]
                    .iter()
                    .filter(|t| !matches!(t, Tok::Space(_)))
                    .collect();
                let name = ty.first().and_then(|t| word(t)).unwrap_or_default();
                let nums: Vec<u32> = ty
                    .iter()
                    .filter_map(|t| match t {
                        Tok::Number(n) => n.parse().ok(),
                        _ => None,
                    })
                    .collect();
                let n0 = nums.first().copied();
                let n1 = nums.get(1).copied().unwrap_or(0);
                let (code, p, sc, pg) = match name.as_str() {
                    "DECIMAL" | "DEC" => {
                        let p = n0.unwrap_or(5);
                        ("P", p, n1, format!("numeric({p},{n1})"))
                    }
                    "NUMERIC" => {
                        let p = n0.unwrap_or(5);
                        ("S", p, n1, format!("numeric({p},{n1})"))
                    }
                    "SMALLINT" => ("B", 2, 0, "smallint".to_string()),
                    "INTEGER" | "INT" => ("B", 4, 0, "integer".to_string()),
                    "BIGINT" => ("B", 8, 0, "bigint".to_string()),
                    "CHAR" | "CHARACTER" => {
                        let n = n0.unwrap_or(1);
                        ("A", n, 0, format!("char({n})"))
                    }
                    "VARCHAR" => {
                        let n = n0.unwrap_or(1);
                        ("V", n, 0, format!("varchar({n})"))
                    }
                    "BINARY" | "VARBINARY" => ("", 0, 0, "bytea".to_string()),
                    other => bail!("SQL0084: INTERPRET AS {other} is not supported yet."),
                };
                if code.is_empty() {
                    out.push(Tok::Punct("CAST((".into()));
                    close_with.push((a, ") AS bytea)".into()));
                } else {
                    out.push(Tok::Punct("CAST(as4tainr_svc.interpret((".into()));
                    close_with.push((a, format!("), '{code}', {p}, {sc}) AS {pg})")));
                }
                skip_close.extend(a + 1..=c);
                i = o + 1;
                continue;
            }
            // Scalar functions PostgreSQL lacks or has under other rules
            // (sqlfn.rs).
            Some(
                f @ ("LOCATE" | "LOCATE_IN_STRING" | "SPACE" | "TRANSLATE" | "INSERT"
                | "MIDNIGHT_SECONDS" | "WEEK" | "WEEK_ISO" | "DAYOFWEEK_ISO" | "MONTHNAME"
                | "DAYNAME" | "JULIAN_DAY" | "RAND" | "ADD_MONTHS" | "LAST_DAY" | "NEXT_DAY"
                | "MONTHS_BETWEEN" | "TIMESTAMP_FORMAT" | "TO_DATE" | "GENERATE_UNIQUE"
                | "TIMESTAMPDIFF" | "VARCHAR" | "DATE"),
            ) if call_open.is_some()
                && (!matches!(f, "VARCHAR" | "DATE") || function_position(&out))
                && !matches!(out.iter().rev().find(|t| !matches!(t, Tok::Space(_))), Some(Tok::Punct(p)) if p == ".") =>
            {
                out.push(Tok::Word(format!(
                    "as4tainr_svc.{}",
                    f.to_ascii_lowercase()
                )));
                i += 1;
                continue;
            }
            // IFNULL and VALUE are COALESCE; POSSTR(source, search) is
            // strpos; CURDATE() and CURTIME() are CURRENT DATE and
            // CURRENT TIME.
            Some(f @ ("IFNULL" | "VALUE" | "POSSTR"))
                if call_open.is_some()
                    && !matches!(out.iter().rev().find(|t| !matches!(t, Tok::Space(_))), Some(Tok::Punct(p)) if p == ".") =>
            {
                out.push(Tok::Word(
                    if f == "POSSTR" { "strpos" } else { "COALESCE" }.into(),
                ));
                i += 1;
                continue;
            }
            Some(f @ ("CURDATE" | "CURTIME")) if call_open.is_some() => {
                if let Some((c, commas)) = paren_span(&toks, call_open.unwrap()) {
                    if commas.is_empty()
                        && c == next_non_space(&toks, call_open.unwrap() + 1).unwrap_or(0)
                    {
                        out.push(Tok::Word(
                            if f == "CURDATE" { STMT_DATE } else { STMT_TIME }.into(),
                        ));
                        i = c + 1;
                        continue;
                    }
                }
            }
            // STRIP(x [, BOTH | B | LEADING | L | TRAILING | T [, c]]): the
            // keyword passed as a string.
            Some("STRIP") if call_open.is_some() => {
                if let Some((_, commas)) = paren_span(&toks, call_open.unwrap()) {
                    if let Some(&k) = commas.first() {
                        if let Some(f) = next_non_space(&toks, k + 1) {
                            let m = match word(&toks[f]).as_deref() {
                                Some("B" | "BOTH") => Some('B'),
                                Some("L" | "LEADING") => Some('L'),
                                Some("T" | "TRAILING") => Some('T'),
                                _ => None,
                            };
                            if let Some(m) = m {
                                close_with.push((f, format!("'{m}'")));
                            }
                        }
                    }
                }
                out.push(Tok::Word("as4tainr_svc.strip".into()));
                i += 1;
                continue;
            }
            // INTEGER(x), INT, SMALLINT, BIGINT: the fraction cut off, as
            // CAST (a string is read as a number first); DOUBLE(x), FLOAT,
            // DOUBLE_PRECISION, REAL: floating point (Db2 for i SQL
            // Reference, scalar functions).
            Some(
                f @ ("INTEGER" | "INT" | "SMALLINT" | "BIGINT" | "DOUBLE" | "DOUBLE_PRECISION"
                | "FLOAT" | "REAL"),
            ) if call_open.is_some() && function_position(&out) => {
                let o = call_open.unwrap();
                if let Some((c, commas)) = paren_span(&toks, o) {
                    if commas.is_empty() {
                        let (open, ty) = match f {
                            "INTEGER" | "INT" => {
                                ("CAST(trunc(CAST((", ") AS numeric)) AS integer)")
                            }
                            "SMALLINT" => ("CAST(trunc(CAST((", ") AS numeric)) AS smallint)"),
                            "BIGINT" => ("CAST(trunc(CAST((", ") AS numeric)) AS bigint)"),
                            "REAL" => ("CAST((", ") AS real)"),
                            _ => ("CAST((", ") AS double precision)"),
                        };
                        out.push(Tok::Punct(open.into()));
                        close_with.push((c, ty.into()));
                        i = o + 1;
                        continue;
                    }
                }
            }
            // MAX and MIN with two or more arguments are scalar functions.
            Some(f @ ("MAX" | "MIN")) if call_open.is_some() => {
                if paren_span(&toks, call_open.unwrap()).is_some_and(|(_, c)| !c.is_empty()) {
                    out.push(Tok::Word(format!(
                        "as4tainr_svc.{}",
                        f.to_ascii_lowercase()
                    )));
                    i += 1;
                    continue;
                }
            }
            Some(f @ ("HEX" | "SUBSTR"))
                if next_non_space(&toks, i + 1)
                    .is_some_and(|o| toks[o] == Tok::Punct("(".into())) =>
            {
                out.push(Tok::Word(format!(
                    "as4tainr_svc.{}",
                    f.to_ascii_lowercase()
                )));
                i += 1;
                continue;
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
            Some("TABLE") => {
                // A table function reference: FROM TABLE(f(args)) AS x
                // (Db2 for i SQL Reference, table-reference) is f(args) AS
                // x in PostgreSQL.
                let prev = out
                    .iter()
                    .rev()
                    .find(|t| !matches!(t, Tok::Space(_)))
                    .cloned();
                let in_from = match &prev {
                    Some(Tok::Punct(p)) => p.trim() == ",",
                    Some(t) => matches!(word(t).as_deref(), Some("FROM" | "JOIN" | "LATERAL")),
                    None => false,
                };
                if let Some(o) = next_non_space(&toks, i + 1).filter(|_| in_from) {
                    // TABLE(VALUES ...) AS t(x), TABLE(SELECT ...): a
                    // subquery, which keeps its parentheses.
                    let inner = next_non_space(&toks, o + 1).and_then(|k| word(&toks[k]));
                    if toks[o] == Tok::Punct("(".into())
                        && matches!(inner.as_deref(), Some("VALUES" | "SELECT" | "WITH"))
                    {
                        i += 1;
                        continue;
                    }
                    if toks[o] == Tok::Punct("(".into()) {
                        let mut depth = 0i32;
                        let close = (o..toks.len()).find(|&j| {
                            match &toks[j] {
                                Tok::Punct(p) if p == "(" => depth += 1,
                                Tok::Punct(p) if p == ")" => depth -= 1,
                                _ => {}
                            }
                            depth == 0
                        });
                        if let Some(c) = close {
                            skip_close.push(c);
                            i = o + 1;
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
            Some("FOR") => {
                // The read-only clause: FOR READ ONLY or FOR FETCH ONLY
                // (Db2 for i SQL Reference, select-statement): it only
                // says the result is not updated.
                let m = next_non_space(&toks, i + 1);
                let o = m.and_then(|m| next_non_space(&toks, m + 1));
                if let (Some(m), Some(o)) = (m, o) {
                    if matches!(word(&toks[m]).as_deref(), Some("READ" | "FETCH"))
                        && word(&toks[o]).as_deref() == Some("ONLY")
                    {
                        i = o + 1;
                        continue;
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
                            Tok::Punct(p) if p == "," && depth == 0 => {
                                rows.push(vec![]);
                                continue;
                            }
                            // Rewritten calls are punctuation with
                            // parentheses in them.
                            Tok::Punct(p) => {
                                depth +=
                                    p.matches('(').count() as i32 - p.matches(')').count() as i32
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
        // The table name: NAME, or LIB.NAME (LIB/NAME is already
        // LIB.NAME here).
        let qualified = idx.len() >= 6 && matches!(&out[idx[3]], Tok::Punct(p) if p == ".");
        let last = if qualified { 4 } else { 2 };
        if idx.len() > last + 1
            && word(&out[idx[1]]).as_deref() == Some("INTO")
            && matches!(
                word(&out[idx[last + 1]]).as_deref(),
                Some("VALUES" | "SELECT" | "WITH")
            )
        {
            let part = |t: &Tok| -> Result<String> {
                Ok(match t {
                    Tok::Word(w) => w.clone(),
                    Tok::Quoted(q) => q.clone(),
                    _ => bail!("SQL0104: A table name is expected after INSERT INTO."),
                })
            };
            let target = if qualified {
                format!("{}.{}", part(&out[idx[2]])?, part(&out[idx[4]])?)
            } else {
                part(&out[idx[2]])?
            };
            let before: String = render(&out[..=idx[last]]);
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
    /// CALL of a procedure without result sets: what happened.
    Message(String),
}

/// The most rows one SELECT returns to a display.
pub const MAX_ROWS: usize = 9999;

/// A constraint's name as the SQL statement gave it, and its library:
/// constraints are named FILE_NAME in PostgreSQL (ddl.rs), the name
/// being per table there and per schema in Db2.
fn constraint_name(db: &tokio_postgres::error::DbError) -> Option<(String, String)> {
    let c = db.constraint()?;
    let t = db.table().unwrap_or("");
    let name = c
        .strip_prefix(&format!("{t}_"))
        .filter(|_| !t.is_empty())
        .unwrap_or(c);
    Some((
        name.to_ascii_uppercase(),
        db.schema().unwrap_or("").to_ascii_uppercase(),
    ))
}

pub(crate) fn map_error(e: &tokio_postgres::Error, sql: &str) -> anyhow::Error {
    let Some(db) = e.as_db_error() else {
        return anyhow!("SQL0901: {e}");
    };
    let msg = db.message().to_string();
    let code = db.code().code();
    // A service function's own error ("SQL0204: ..." raised by
    // QSYS2.LIBRARY_INFO): as it is.
    if msg.len() > 8
        && msg.starts_with("SQL")
        && msg[3..7].chars().all(|c| c.is_ascii_digit())
        && &msg[7..8] == ":"
    {
        return anyhow!("{msg}");
    }
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
        // A VALUES list longer or shorter than the INSERT's columns:
        // SQL0117 (PUB400 2026-10-27, tools/golden/verify3.txt x_errors).
        "42601" if msg.starts_with("INSERT has more") => {
            anyhow!("SQL0117: The INSERT gives a different number of values than it has columns.")
        }
        "42601" => anyhow!("SQL0104: Statement not valid: {msg}."),
        // A file not valid for the operation (as4tainr's temporal
        // versioning: a table that is not journaled): SQL7008.
        "55019" => anyhow!("SQL7008: {msg}"),
        "23505" => anyhow!("SQL0803: Duplicate key value: the record was not written."),
        "23514" => match constraint_name(db) {
            Some((c, l)) => {
                anyhow!("SQL0545: The row does not satisfy check constraint {c} in {l}.")
            }
            None => anyhow!("SQL0545: The row does not satisfy a check constraint: {msg}."),
        },
        // A referential constraint: a foreign key with no parent row
        // (INSERT, UPDATE), or a parent row still referenced (DELETE).
        "23503" if msg.contains("is still referenced") => match constraint_name(db) {
            Some((c, l)) => anyhow!(
                "SQL0532: Referential constraint {c} in {l} keeps the row from being deleted."
            ),
            None => anyhow!("SQL0532: Delete prevented by referential constraint: {msg}."),
        },
        "23503" => match constraint_name(db) {
            Some((c, l)) => {
                anyhow!("SQL0530: Referential constraint {c} in {l} has no parent row for the key.")
            }
            None => anyhow!("SQL0530: Operation not allowed by referential constraint: {msg}."),
        },
        "2BP01" => anyhow!("SQL0478: Other objects depend on this one: {msg}."),
        "23502" => anyhow!("SQL0407: Null value not allowed in a NOT NULL column: {msg}."),
        "42501" => anyhow!("SQL0551: Not authorized to an object in the statement."),
        "22P02" | "22003" => anyhow!("SQL0302: A value does not fit its column: {msg}."),
        // A date, time or timestamp string that is not a valid value (Db2
        // for i SQL messages and codes: SQL0181, SQLSTATE 22007).
        "22007" | "22008" => {
            anyhow!("SQL0181: The date, time or timestamp value is not valid: {msg}.")
        }
        "22012" => anyhow!("SQL0802: Division by zero: {msg}."),
        // Sequences: no more values (NO CYCLE); PREVIOUS VALUE before a
        // NEXT VALUE in the session.
        "2200H" => anyhow!("SQL0359: The range of values of the sequence is used up: {msg}."),
        "55000" if msg.contains("currval") => {
            anyhow!("SQL0845: PREVIOUS VALUE cannot be used before NEXT VALUE gives a value for the sequence in this job.")
        }
        // An SQLSTATE an SQL function signaled (user-defined classes 7-9
        // and I-Z; 20000 CASE not found): SQLCODE -438.
        c if c.starts_with(['7', '8', '9'])
            || c.chars()
                .next()
                .is_some_and(|x| ('I'..='Z').contains(&x) && !matches!(x, 'P' | 'X'))
            || c == "20000" =>
        {
            anyhow::Error::new(crate::sqlpl::Signaled {
                state: c.to_string(),
                text: msg,
            })
        }
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
    run_as(client, database_url, libl, "QSECOFR", None, None, sql).await
}

/// The job a statement runs for, as the IBM i services see it (the
/// session settings `as4tainr.user`, `as4tainr.curlib`, `as4tainr.job`:
/// set by as4tainr, never by user SQL, which cannot run SET).
pub(crate) fn context_sql(user: &str, curlib: Option<&str>, job: Option<&str>) -> String {
    context_sql_in(user, curlib, job, false)
}

/// `context_sql`, for the transaction only (`local`) or the session.
fn context_sql_in(user: &str, curlib: Option<&str>, job: Option<&str>, local: bool) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    format!(
        "SELECT set_config('as4tainr.user', {u}, {local}), set_config('as4tainr.curlib', {c}, {local}), set_config('as4tainr.job', {j}, {local}), set_config('as4tainr.job_key', {k}, {local})",
        u = q(user),
        c = q(curlib.unwrap_or("")),
        j = q(job.unwrap_or("")),
        // The key of the job's variables kept in this process (Job::job_key).
        k = q(&job
            .map(str::to_string)
            .unwrap_or_else(|| format!("PID{}", std::process::id())))
    )
}

/// `run` for a user (data definition in SQL procedures is done for them)
/// with a current library. CALL runs a procedure: its first result set
/// is the outcome's rows.
pub async fn run_as(
    client: &Client,
    database_url: &str,
    libl: &[String],
    user: &str,
    curlib: Option<String>,
    job: Option<String>,
    sql: &str,
) -> Result<Outcome> {
    let context = context_sql(user, curlib.as_deref(), job.as_deref());
    // A data change statement that calls the QSYS2.QCMDEXC scalar
    // function (RUNSQL INSERT ... SELECT QSYS2.QCMDEXC(...): PUB400
    // 2026-10-26, tools/golden/verify1.txt v_qcmdexc1/2) runs as a
    // compound statement, where its calls are made.
    let compound;
    let sql = if !crate::sqlpl::is_compound(sql) && crate::sqlpl::calls_qcmdexc(sql) {
        compound = format!("BEGIN {}; END", sql.trim().trim_end_matches(';'));
        compound.as_str()
    } else {
        sql
    };
    // A dynamic compound statement (BEGIN ... END).
    if crate::sqlpl::is_compound(sql) {
        let session = Session::open(client, database_url, libl).await?;
        session.client.batch_execute(&context).await?;
        let env = crate::sqlpl::Env {
            session: &session,
            catalog: client,
            database_url,
            libl,
            user,
            curlib: curlib.clone(),
        };
        if crate::sqlpl::run_dynamic(&env, sql).await?.is_some() {
            return Ok(Outcome::Message("Compound statement ran.".into()));
        }
    }
    if let Some((name, args)) = crate::sqlpl::parse_call(sql)? {
        let session = Session::open(client, database_url, libl).await?;
        session.client.batch_execute(&context).await?;
        let env = crate::sqlpl::Env {
            session: &session,
            catalog: client,
            database_url,
            libl,
            user,
            curlib,
        };
        let (r, out) = crate::sqlpl::call_exprs(&env, &name, &args).await?;
        if let Some((columns, rows)) = out.result_sets.into_iter().next() {
            let numeric = columns.iter().map(|_| false).collect();
            return Ok(Outcome::Rows {
                columns,
                numeric,
                rows: rows
                    .into_iter()
                    .map(|r| {
                        r.into_iter()
                            .map(|v| v.unwrap_or_else(|| "-".into()))
                            .collect()
                    })
                    .collect(),
            });
        }
        let outs: Vec<String> = r
            .parms
            .iter()
            .zip(&out.values)
            .filter(|(p, _)| p.mode != "IN")
            .map(|(p, v)| format!("{} = {}", p.name, v.as_deref().unwrap_or("NULL")))
            .collect();
        return Ok(Outcome::Message(if outs.is_empty() {
            format!("Procedure {} ran.", r.name)
        } else {
            format!("Procedure {} ran: {}.", r.name, outs.join(", "))
        }));
    }
    let libraries = catalog::list_libraries(client).await?;
    let sql = &crate::sqlvar::substitute(client, libl, sql).await?;
    let sql = &crate::temporal::rewrite(client, libl, sql).await?;
    let types = if needs_column_types(sql) {
        numeric_columns(client, libl, sql).await
    } else {
        Default::default()
    };
    let t = translate_typed(sql, &|n| libraries.iter().any(|l| l == n), types)?;
    let mut text = system_column_names(client, libl, &t.sql).await?;
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
        let cols = insert_columns(client, libl, &name).await?;
        text.insert_str(*at, &format!(" ({})", cols.join(", ")));
    }
    let path: Vec<String> = libl.iter().map(|l| ident(l)).collect();
    // One transaction per statement (COMMIT(*NONE)), on a connection of
    // its own as the SQL role.
    let sql_client = std::sync::Arc::new(connect_as_sql_role(client, database_url).await?);
    sql_client.batch_execute(&context).await?;
    // Source files the statement names: views of their first members.
    let sources = crate::source::sql_sources(client, libl, &text).await?;
    let text = crate::source::sql_source_views(&sql_client, &sources, &text).await?;
    sql_client.batch_execute("BEGIN").await?;
    // Trigger programs (ADDPFTRG) for the changes, in this transaction; one
    // that fails ends the statement in SQL0443.
    let (result, keep) = if t.kind != Kind::Query && crate::pftrg::any(&sql_client).await {
        let who = crate::pftrg::Caller {
            database_url,
            libl: libl.to_vec(),
            user: user.to_string(),
            job: job.clone(),
            curlib: curlib.clone(),
        };
        let data = crate::runtime::DataSession::Sql(sql_client.clone());
        let (r, keep) = crate::pftrg::run(data, client, &who, false, || {
            run_in_transaction(&sql_client, &t, &text, &path)
        })
        .await;
        (r.map_err(trigger_failed_if), keep)
    } else {
        (
            run_in_transaction(&sql_client, &t, &text, &path).await,
            false,
        )
    };
    if result.is_ok() || keep {
        sql_client.batch_execute("COMMIT").await?
    } else {
        sql_client.batch_execute("ROLLBACK").await?
    }
    result
}

/// FOR BIT DATA, FOR SBCS DATA, FOR MIXED DATA after a character type:
/// left out.
fn drop_for_data(toks: &mut Vec<Tok>) {
    let word = |t: &Tok, w: &str| matches!(t, Tok::Word(x) if x.eq_ignore_ascii_case(w));
    let mut i = 0;
    while i < toks.len() {
        if word(&toks[i], "FOR") {
            let sig: Vec<usize> = (i + 1..toks.len())
                .filter(|&k| !matches!(toks[k], Tok::Space(_)))
                .take(2)
                .collect();
            if let [a, b] = sig[..] {
                let kind = ["BIT", "SBCS", "MIXED"].iter().any(|k| word(&toks[a], k));
                if kind && word(&toks[b], "DATA") {
                    toks.drain(i..=b);
                    continue;
                }
            }
        }
        i += 1;
    }
}

/// VALUES e1, e2, ...: each expression a row of its own, in parentheses.
fn values_rows(toks: &mut Vec<Tok>) {
    let mut i = 0;
    while i < toks.len() {
        if !matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("VALUES")) {
            i += 1;
            continue;
        }
        // The expressions: to a closing parenthesis, the end or a clause.
        let start = i + 1;
        let mut depth = 0i32;
        let mut commas = Vec::new();
        let mut end = toks.len();
        let mut j = start;
        while j < toks.len() {
            match &toks[j] {
                Tok::Punct(p) if p == "(" => depth += 1,
                Tok::Punct(p) if p == ")" => {
                    if depth == 0 {
                        end = j;
                        break;
                    }
                    depth -= 1;
                }
                Tok::Punct(p) if p == "," && depth == 0 => commas.push(j),
                Tok::Punct(p) if p == ";" && depth == 0 => {
                    end = j;
                    break;
                }
                Tok::Word(w)
                    if depth == 0
                        && matches!(
                            w.to_ascii_uppercase().as_str(),
                            "ORDER"
                                | "FETCH"
                                | "UNION"
                                | "EXCEPT"
                                | "INTERSECT"
                                | "INTO"
                                | "LIMIT"
                                | "OFFSET"
                                | "FOR"
                        ) =>
                {
                    end = j;
                    break;
                }
                _ => {}
            }
            j += 1;
        }
        let first = toks[start..end]
            .iter()
            .find(|t| !matches!(t, Tok::Space(_)));
        let parenthesized = matches!(first, Some(Tok::Punct(p)) if p == "(");
        // One expression in a nested VALUES (`TABLE(VALUES 'A') AS t(x)`)
        // is a row too; a statement's own VALUES is left to `translate`.
        let nested = toks[..i].iter().any(|t| !matches!(t, Tok::Space(_)));
        if parenthesized || (commas.is_empty() && !nested) {
            i = end.max(i + 1);
            continue;
        }
        // Wrap each expression, from the last.
        let mut bounds = vec![start];
        bounds.extend(commas.iter().map(|c| c + 1));
        let mut ends: Vec<usize> = commas.clone();
        ends.push(end);
        for (mut b, mut e) in bounds.into_iter().zip(ends).rev() {
            // Inside the blanks around the expression.
            while b < e && matches!(toks[b], Tok::Space(_)) {
                b += 1;
            }
            while e > b && matches!(toks[e - 1], Tok::Space(_)) {
                e -= 1;
            }
            toks.insert(e, Tok::Punct(")".into()));
            toks.insert(b, Tok::Punct("(".into()));
        }
        i = end + 2 * (commas.len() + 1);
    }
}

/// Is the token at `at` (a hexadecimal constant: X, then its string)
/// the whole operand of CAST( ... AS CHAR/VARCHAR(n)) without FOR BIT
/// DATA?
fn cast_to_character(toks: &[Tok], at: usize) -> bool {
    let sig = |from: isize, dir: isize| -> Option<usize> {
        let mut j = from + dir;
        while j >= 0 && (j as usize) < toks.len() {
            if !matches!(toks[j as usize], Tok::Space(_)) {
                return Some(j as usize);
            }
            j += dir;
        }
        None
    };
    let word = |j: Option<usize>| -> String {
        match j.map(|j| &toks[j]) {
            Some(Tok::Word(w)) => w.to_ascii_uppercase(),
            Some(Tok::Punct(p)) => p.clone(),
            _ => String::new(),
        }
    };
    let open = sig(at as isize, -1);
    if word(open) != "(" || word(open.and_then(|o| sig(o as isize, -1))) != "CAST" {
        return false;
    }
    let as_ = sig(at as isize + 1, 1);
    if word(as_) != "AS" {
        return false;
    }
    let ty = word(as_.and_then(|a| sig(a as isize, 1)));
    if !matches!(ty.as_str(), "CHAR" | "VARCHAR" | "CHARACTER") {
        return false;
    }
    // Up to the CAST's closing parenthesis: no FOR BIT DATA.
    let mut depth = 0;
    let mut j = as_.unwrap_or(at);
    while j < toks.len() {
        match &toks[j] {
            Tok::Punct(p) if p == "(" => depth += 1,
            Tok::Punct(p) if p == ")" => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            Tok::Word(w) if w.eq_ignore_ascii_case("BIT") => return false,
            _ => {}
        }
        j += 1;
    }
    true
}

/// SQL0443 for a trigger program's error (CPF502B); others as they are.
fn trigger_failed_if(e: anyhow::Error) -> anyhow::Error {
    if format!("{e}").starts_with("CPF502B") {
        trigger_failed(e)
    } else {
        e
    }
}

/// A trigger program failed: SQL0443 for the statement (Db2 for i: a
/// trigger program or external routine detected an error).
fn trigger_failed(e: anyhow::Error) -> anyhow::Error {
    anyhow!("SQL0443: Trigger program detected an error: {e:#}")
}

async fn run_in_transaction(
    tx: &Client,
    t: &Translated,
    text: &str,
    path: &[String],
) -> Result<Outcome> {
    tx.batch_execute(&format!(
        "SET LOCAL search_path TO {}{}",
        joined_path(path),
        qtemp_setting(true)
    ))
    .await?;
    let text = &db2_datetime_literals(tx, text.to_string(), true).await;
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
        let types: Vec<&str> = cols.iter().map(|c| c.type_().name()).collect();
        // A timestamp column's precision (TIMESTAMP(p): p fractional
        // digits shown), when it comes from a table.
        let mut precisions: Vec<Option<i32>> = Vec::new();
        for c in cols.iter() {
            let p = match (c.type_().name(), c.table_oid(), c.column_id()) {
                ("timestamp", Some(oid), Some(col)) => tx
                    .query_opt(
                        "SELECT atttypmod FROM pg_catalog.pg_attribute WHERE attrelid = $1 AND attnum = $2",
                        &[&oid, &col],
                    )
                    .await
                    .ok()
                    .flatten()
                    .map(|r| r.get::<_, i32>(0))
                    .filter(|m| *m >= 0),
                _ => None,
            };
            precisions.push(p);
        }
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
                        .map(|j| match r.get(j) {
                            Some(v) => match precisions.get(j).copied().flatten() {
                                Some(p) => db2_timestamp_p(v, p as usize),
                                None => db2_text(types.get(j).copied().unwrap_or(""), v),
                            },
                            None => "-".to_string(),
                        })
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

/// Db2 for i's string forms of dates, times and timestamps that
/// PostgreSQL does not read (SQL reference, "String representations of
/// datetime values": time hh.mm.ss or hh.mm (ISO, EUR), date dd.mm.yyyy
/// (EUR), timestamp yyyy-mm-dd-hh.mm.ss.nnnnnn, PUB400 2026-11-09
/// verify68 o68_make): written in PostgreSQL's form where the statement
/// uses the string as a date, time or timestamp. Which literals those are
/// is what PostgreSQL infers for them: each candidate is made a parameter
/// and the statement prepared (a string compared with or put into a
/// character column stays as it is). `in_tx`: the client is in a
/// transaction, so the trial runs in a savepoint of its own.
async fn db2_datetime_literals(client: &Client, text: String, in_tx: bool) -> String {
    let lits = string_literals(&text);
    let cands: Vec<(usize, usize, String)> = lits
        .into_iter()
        .filter_map(|(a, b)| {
            let v = text[a + 1..b - 1].to_string();
            pg_datetime_form(&v).map(|_| (a, b, v))
        })
        .collect();
    if cands.is_empty() {
        return text;
    }
    let base = max_parameter(&text);
    let mut probe = String::new();
    let mut at = 0;
    for (i, (a, b, _)) in cands.iter().enumerate() {
        probe.push_str(&text[at..*a]);
        probe.push_str(&format!("${}", base + i + 1));
        at = *b;
    }
    probe.push_str(&text[at..]);
    if in_tx
        && client
            .batch_execute("SAVEPOINT as4tainr_dtprobe")
            .await
            .is_err()
    {
        return text;
    }
    let types: Option<Vec<String>> = client
        .prepare(&probe)
        .await
        .ok()
        .map(|st| st.params().iter().map(|t| t.name().to_string()).collect());
    if in_tx {
        let end = if types.is_some() {
            "RELEASE SAVEPOINT as4tainr_dtprobe"
        } else {
            "ROLLBACK TO SAVEPOINT as4tainr_dtprobe; RELEASE SAVEPOINT as4tainr_dtprobe"
        };
        let _ = client.batch_execute(end).await;
    }
    let Some(types) = types else {
        return text;
    };
    let mut out = String::new();
    let mut at = 0;
    for (i, (a, b, v)) in cands.iter().enumerate() {
        out.push_str(&text[at..*a]);
        let ty = types.get(base + i).map(String::as_str).unwrap_or("");
        match pg_datetime_form(v) {
            Some(f) if matches!(ty, "date" | "time" | "timestamp" | "timestamptz" | "timetz") => {
                out.push_str(&format!("'{f}'"))
            }
            _ => out.push_str(&text[*a..*b]),
        }
        at = *b;
    }
    out.push_str(&text[at..]);
    out
}

/// The byte ranges of a statement's string constants, quotes included
/// (not in double-quoted names or comments).
fn string_literals(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' => {
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                // X'..', BX'..', N'..' and E'..' constants are not strings
                // of characters.
                let prefixed = start > 0 && b[start - 1].is_ascii_alphanumeric();
                if !prefixed && i <= b.len() {
                    out.push((start, i));
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// The highest `$n` parameter in a statement (outside its strings).
fn max_parameter(text: &str) -> usize {
    let lits = string_literals(text);
    let b = text.as_bytes();
    let mut max = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some(&(_, e)) = lits.iter().find(|(a, _)| *a == i) {
            i = e;
            continue;
        }
        if b[i] == b'$' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if let Ok(n) = text[i + 1..j].parse::<usize>() {
                max = max.max(n);
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    max
}

/// A Db2 string form of a date, time or timestamp in PostgreSQL's form,
/// when PostgreSQL would not read it as it is.
fn pg_datetime_form(v: &str) -> Option<String> {
    let digits = |s: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&s.len()) && s.bytes().all(|c| c.is_ascii_digit())
    };
    let time = |t: &str| -> Option<String> {
        let p: Vec<&str> = t.split('.').collect();
        match p.as_slice() {
            [h, m] if digits(h, 1..=2) && digits(m, 2..=2) => Some(format!("{h}:{m}:00")),
            [h, m, s] if digits(h, 1..=2) && digits(m, 2..=2) && digits(s, 2..=2) => {
                Some(format!("{h}:{m}:{s}"))
            }
            [h, m, s, f]
                if digits(h, 1..=2)
                    && digits(m, 2..=2)
                    && digits(s, 2..=2)
                    && digits(f, 0..=12) =>
            {
                Some(format!("{h}:{m}:{s}.{f}"))
            }
            _ => None,
        }
    };
    let v = v.trim();
    // Timestamp: yyyy-mm-dd-hh.mm.ss[.n...] (or a blank for the dash).
    if v.len() > 11 && v.is_char_boundary(10) && v.is_char_boundary(11) {
        let (d, rest) = v.split_at(10);
        let dp: Vec<&str> = d.split('-').collect();
        if let [y, m, dd] = dp.as_slice() {
            if digits(y, 4..=4)
                && digits(m, 2..=2)
                && digits(dd, 2..=2)
                && (rest.starts_with('-') || rest.starts_with(' '))
            {
                let t = &rest[1..];
                if t.contains('.') && !t.contains(':') {
                    let p: Vec<&str> = t.split('.').collect();
                    if p.len() >= 3 {
                        return time(t).map(|t| format!("{d} {t}"));
                    }
                }
            }
        }
        return None;
    }
    // Date dd.mm.yyyy.
    let p: Vec<&str> = v.split('.').collect();
    if let [d, m, y] = p.as_slice() {
        if digits(d, 2..=2) && digits(m, 2..=2) && digits(y, 4..=4) {
            return Some(format!("{y}-{m}-{d}"));
        }
    }
    time(v)
}

/// For each parameter `$1`..`$n` of a translated statement, the tables
/// it may belong to and the column it is compared with or put into
/// (`col = $k`, `$k = col`, an INSERT's column list), lowercase.
fn parameter_columns(text: &str, n: usize) -> Vec<Option<(Vec<String>, String)>> {
    // Words and symbols outside strings.
    let lits = string_literals(text);
    let b = text.as_bytes();
    let mut toks: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(&(_, e)) = lits.iter().find(|(a, _)| *a == i) {
            toks.push("'".into());
            i = e;
            continue;
        }
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphanumeric() || "_#@$\".".contains(c) {
            let st = i;
            while i < b.len()
                && ((b[i] as char).is_ascii_alphanumeric() || "_#@$\".".contains(b[i] as char))
            {
                i += 1;
            }
            toks.push(text[st..i].to_string());
        } else if "<>=!".contains(c) {
            let st = i;
            while i < b.len() && "<>=!".contains(b[i] as char) {
                i += 1;
            }
            toks.push(text[st..i].to_string());
        } else {
            toks.push(c.to_string());
            i += 1;
        }
    }
    let up: Vec<String> = toks.iter().map(|t| t.to_ascii_uppercase()).collect();
    let mut tables = Vec::new();
    for (j, t) in up.iter().enumerate() {
        if matches!(t.as_str(), "FROM" | "INTO" | "UPDATE" | "JOIN") {
            if let Some(nx) = toks.get(j + 1) {
                if nx != "(" {
                    tables.push(nx.clone());
                }
            }
        }
    }
    let unq = |s: &str| {
        let s = s.rsplit('.').next().unwrap_or(s);
        if s.starts_with('"') {
            s.trim_matches('"').to_string()
        } else {
            s.to_ascii_lowercase()
        }
    };
    let mut out = vec![None; n];
    // An INSERT's column list and its VALUES.
    if let Some(ins) = up.iter().position(|t| t == "INSERT") {
        let open = up.iter().skip(ins).position(|t| t == "(").map(|p| p + ins);
        let vals = up.iter().position(|t| t == "VALUES");
        if let (Some(o), Some(v)) = (open, vals) {
            if o < v {
                let cols: Vec<String> = toks[o + 1..v]
                    .iter()
                    .filter(|t| !matches!(t.as_str(), "," | "(" | ")"))
                    .map(|t| unq(t))
                    .collect();
                let mut pos = 0;
                let mut depth = 0;
                for t in &toks[v + 1..] {
                    match t.as_str() {
                        "(" => {
                            depth += 1;
                            if depth == 1 {
                                pos = 0;
                            }
                        }
                        ")" => depth -= 1,
                        "," if depth == 1 => pos += 1,
                        p if p.starts_with('$') && depth == 1 => {
                            if let Ok(k) = p[1..].parse::<usize>() {
                                if (1..=n).contains(&k) {
                                    if let Some(c) = cols.get(pos) {
                                        out[k - 1] = Some((tables.clone(), c.clone()));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let is_cmp = |t: &str| matches!(t, "=" | "<>" | "<" | ">" | "<=" | ">=" | "!=");
    for (j, t) in toks.iter().enumerate() {
        let Some(k) = t.strip_prefix('$').and_then(|d| d.parse::<usize>().ok()) else {
            continue;
        };
        if !(1..=n).contains(&k) || out[k - 1].is_some() {
            continue;
        }
        let col = if j >= 2 && is_cmp(&toks[j - 1]) {
            Some(&toks[j - 2])
        } else if is_cmp(toks.get(j + 1).map(String::as_str).unwrap_or("")) {
            toks.get(j + 2)
        } else {
            None
        };
        if let Some(c) = col {
            if c.chars()
                .next()
                .is_some_and(|x| x.is_ascii_alphabetic() || x == '"')
            {
                out[k - 1] = Some((tables.clone(), unq(c)));
            }
        }
    }
    out
}

/// File columns' SQL types as SYSCOLUMNS has them, where PostgreSQL's
/// type does not tell (one query for the statement): NUMERIC (zoned)
/// and DECIMAL (packed), BINARY and VARBINARY (the length in `typmod`),
/// DECFLOAT (`typmod` 16 or 34 digits), CLOB (the length in `typmod`),
/// GRAPHIC and VARGRAPHIC in CCSID 1200 or 13488 (NCHAR and NVARCHAR,
/// as JDBC names them: verify69 p69_mapepire "oth").
async fn describe_types(client: &Client, cols: &[&tokio_postgres::Column], types: &mut [ColType]) {
    let mut oids: Vec<u32> = Vec::new();
    let mut nums: Vec<i16> = Vec::new();
    let mut at: Vec<usize> = Vec::new();
    for (j, c) in cols.iter().enumerate() {
        if let (Some(o), Some(n), true) = (
            c.table_oid(),
            c.column_id(),
            matches!(
                c.type_().name(),
                "numeric" | "bytea" | "bpchar" | "varchar" | "text"
            ),
        ) {
            oids.push(o);
            nums.push(n);
            at.push(j);
        }
    }
    if oids.is_empty() {
        return;
    }
    let Ok(rows) = client
        .query(
            "SELECT CAST(k.i AS int4), CAST(s.data_type AS text), CAST(s.length AS int4), CAST(s.ccsid AS int4)
               FROM unnest($1::oid[], $2::int2[]) WITH ORDINALITY k(o, a, i)
               JOIN pg_catalog.pg_attribute pa ON pa.attrelid = k.o AND pa.attnum = k.a
               JOIN pg_catalog.pg_class r ON r.oid = pa.attrelid
               JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace
               JOIN qsys2.syscolumns s ON s.system_table_schema = upper(n.nspname) COLLATE \"C\"
                AND s.system_table_name = upper(r.relname) COLLATE \"C\"
                AND s.system_column_name = upper(pa.attname) COLLATE \"C\"",
            &[&oids, &nums],
        )
        .await
    else {
        return;
    };
    for r in rows {
        let i: i32 = r.get(0);
        let Some(&j) = at.get(i as usize - 1) else {
            continue;
        };
        let dt: String = r.get::<_, String>(1).trim().to_string();
        let len: i32 = r.try_get::<_, Option<i32>>(2).ok().flatten().unwrap_or(0);
        let ccsid: Option<i32> = r.try_get(3).ok().flatten();
        let t = &mut types[j];
        let unicode = matches!(ccsid, Some(1200 | 13488));
        t.db2 = Some(match dt.as_str() {
            "GRAPHIC" if unicode => "NCHAR".to_string(),
            "VARG" if unicode => "NVARCHAR".to_string(),
            other => other.to_string(),
        });
        match dt.as_str() {
            "BINARY" | "VARBIN" | "CLOB" | "BLOB" => t.typmod = len + 4,
            "DECFLOAT" => t.typmod = if len > 8 { 34 } else { 16 },
            _ => {}
        }
    }
}

/// A value of a PostgreSQL type as Db2 for i shows it: timestamps in
/// the Db2 form, binary data as hex digits.
fn db2_text(pg_type: &str, v: &str) -> String {
    match pg_type {
        "timestamp" => db2_timestamp(v),
        // BOOLEAN as Db2 shows it.
        "bool" => if v == "t" { "TRUE" } else { "FALSE" }.into(),
        "bytea" => v.trim_start_matches("\\x").to_ascii_uppercase(),
        _ => v.to_string(),
    }
}

/// A timestamp as Db2 for i shows it: `YYYY-MM-DD-HH.MM.SS.ffffff`
/// (PostgreSQL: `YYYY-MM-DD HH:MM:SS[.ffffff]`).
pub fn db2_timestamp(v: &str) -> String {
    let Some((d, t)) = v.split_once(' ') else {
        return v.to_string();
    };
    let (hms, frac) = t.split_once('.').unwrap_or((t, ""));
    format!("{d}-{}.{frac:0<6}", hms.replace(':', "."))
}

/// A timestamp with `p` fractional digits (TIMESTAMP(p); none: no
/// fraction).
pub fn db2_timestamp_p(v: &str, p: usize) -> String {
    let full = db2_timestamp(v);
    match full.rsplit_once('.') {
        Some((head, frac)) if p == 0 => {
            let _ = frac;
            head.to_string()
        }
        Some((head, frac)) => format!("{head}.{:0<p$}", &frac[..frac.len().min(p)]),
        None => full,
    }
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
        // Delimited names in system naming (Db2 for i SQL Reference,
        // naming conventions), as ACS Data Transfer writes a file.
        assert_eq!(
            tr("SELECT * FROM \"QGPL\"/\"CUSTMAST\" FOR FETCH ONLY").sql,
            "SELECT * FROM \"qgpl\".\"custmast\""
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
        // column (PostgreSQL manual, "System Columns"), so as4tainr stores it
        // as "~ctid".
        assert_eq!(
            tr("select ctid, name from mylib/contact where ctid > 3").sql,
            "select \"~ctid\", name from \"mylib\".\"contact\" where \"~ctid\" > 3"
        );
        assert_eq!(crate::catalog::ibm_name("~ctid"), "CTID");
    }

    // Db2 for i SQL reference, CLOB/BLOB data types: a length with K, M
    // or G (CLOB(10M)); large objects are text and bytea here.
    #[test]
    fn lob_types_in_cast() {
        assert_eq!(
            tr("select cast(x as clob(10M)), cast(y as blob(1K)), cast(z as dbclob) from t").sql,
            "select cast(x as text), cast(y as bytea), cast(z as text) from t"
        );
    }

    #[test]
    fn decimal_arithmetic_types() {
        // Db2 for i SQL Reference, decimal arithmetic; verify70
        // q70_mapepire "ar".
        let types: std::collections::HashMap<String, (u32, u32, bool)> = [
            ("D".to_string(), (7, 2, true)),
            ("I".to_string(), (11, 0, false)),
        ]
        .into();
        let t = translate_typed(
            "SELECT D + D, D * D, D + I, I * 1.5, D / 7.0, ROUND(D / 3, 2), I + I, D - 1.5 FROM T",
            &|_| false,
            types,
        )
        .unwrap();
        let q = t.sql;
        // Each cast as translate writes a Db2 decimal cast (truncating).
        for want in [
            "(D + D ) AS numeric), 2) AS numeric(8, 2))",
            "(D * D ) AS numeric), 4) AS numeric(14, 4))",
            "(D + I ) AS numeric), 2) AS numeric(14, 2))",
            "(I * 1.5 ) AS numeric), 1) AS numeric(13, 1))",
            "ddiv(D, 7.0, 25) ) AS numeric), 25) AS numeric(31, 25))",
            "ddiv(D, 3, 26)) AS numeric), 2) ) AS numeric), 26) AS numeric(31, 26))",
            "(D - 1.5 ) AS numeric), 2) AS numeric(8, 2))",
        ] {
            assert!(q.contains(want), "{want} in {q}");
        }
        assert!(q.contains(", I + I,"), "{q}");
    }

    #[test]
    fn decimal_expression_types() {
        // verify71 r71_mapepire "mix" and "agg".
        let types: std::collections::HashMap<String, (u32, u32, bool)> = [
            ("D".to_string(), (7, 2, true)),
            ("I".to_string(), (11, 0, false)),
            ("Z".to_string(), (5, 0, true)),
        ]
        .into();
        let q = translate_typed(
            "SELECT D * 2 + 1, (D + 1) * 2, D + D + D, -D, D * I / 4, SUM(D), AVG(D), SUM(I), AVG(I), AVG(Z) FROM LIB/T",
            &|l| l == "LIB",
            types,
        )
        .unwrap()
        .sql;
        for want in [
            "AS numeric(19, 2))",
            "AS numeric(25, 2))",
            "AS numeric(9, 2))",
            "AS numeric(7, 2))",
            "ddiv(D * I, 4, 15)",
            "AS numeric(31, 2))",
            "ddiv(SUM(D), CAST(trunc(CAST((COUNT(D) ) AS numeric)) AS integer), 26)",
            "CAST(trunc(CAST((SUM(I) ) AS numeric)) AS integer)",
            "CAST(trunc(CAST((trunc(AVG(I)) ) AS numeric)) AS integer)",
            "ddiv(SUM(Z), CAST(trunc(CAST((COUNT(Z) ) AS numeric)) AS integer), 26)",
            "FROM \"lib\".\"t\"",
        ] {
            assert!(q.contains(want), "{want} in {q}");
        }
    }

    #[test]
    fn db2_datetime_string_forms() {
        // Db2 for i SQL Reference, "String representations of datetime
        // values"; verify68 o68_make inserted '13.14.15' and
        // '2026-11-09-13.14.15.123456'.
        assert_eq!(pg_datetime_form("13.14.15").as_deref(), Some("13:14:15"));
        assert_eq!(pg_datetime_form("8.05").as_deref(), Some("8:05:00"));
        assert_eq!(
            pg_datetime_form("2026-11-09-13.14.15.123456").as_deref(),
            Some("2026-11-09 13:14:15.123456")
        );
        assert_eq!(
            pg_datetime_form("2026-11-09-13.14.15").as_deref(),
            Some("2026-11-09 13:14:15")
        );
        assert_eq!(
            pg_datetime_form("09.11.2026").as_deref(),
            Some("2026-11-09")
        );
        // Forms PostgreSQL reads itself, and other strings.
        assert_eq!(pg_datetime_form("13:14:15"), None);
        assert_eq!(pg_datetime_form("2026-11-09"), None);
        assert_eq!(pg_datetime_form("2026-11-09 13:14:15"), None);
        assert_eq!(pg_datetime_form("abc"), None);
        assert_eq!(pg_datetime_form("1.5"), None);
        let t = "SELECT 'a''b', X'C1', \"x'y\" FROM t WHERE c = '13.14' -- 'no'";
        let l: Vec<&str> = string_literals(t).iter().map(|(a, b)| &t[*a..*b]).collect();
        assert_eq!(l, ["'a''b'", "'13.14'"]);
        assert_eq!(max_parameter("SELECT $2, '$9' FROM t WHERE a = $10"), 10);
        let cols = parameter_columns("INSERT INTO lib.t (i, c) VALUES ($1, $2)", 2);
        assert_eq!(cols[1], Some((vec!["lib.t".to_string()], "c".to_string())));
        let cols = parameter_columns("SELECT c FROM lib.t WHERE i = $1", 1);
        assert_eq!(cols[0], Some((vec!["lib.t".to_string()], "i".to_string())));
    }

    #[test]
    fn db2_dialect() {
        assert_eq!(
            tr("values current date").sql,
            "values (CAST(statement_timestamp() AS date))"
        );
        // Db2 for i SQL Reference, special registers: CURRENT_TIMESTAMP is
        // CURRENT TIMESTAMP (iRPGUnit's getFullTimestamp sets a host
        // variable from it).
        assert_eq!(
            tr("values current_timestamp").sql,
            tr("values current timestamp").sql
        );
        // DB2: VALUES with a list of expressions gives one row per expression.
        assert_eq!(
            tr("values 1, (2), substr('ab', 1, 1)").sql,
            "values (1), ((2)), (as4tainr_svc.substr('ab', 1, 1))"
        );
        assert_eq!(tr("values (1, 2)").sql, "values (1, 2)");
        // In a common table expression too (Code for IBM i's
        // validateLibraryList: `with LIBRARIES (LIBRARY) as (values 'A','B')`).
        assert_eq!(
            tr("with l (x) as (values 'A','B') select x from l").sql,
            "with l (x) as (values ('A'),('B')) select x from l"
        );
        // One value (Code for IBM i's getLibraryList: `TABLE(VALUES 'LIB')
        // AS libs(ELEMENT)`).
        assert_eq!(
            tr("select x from table(values 'A') as l(x)").sql,
            "select x from (values ('A')) as l(x)"
        );
        assert_eq!(tr("select a concat b from t").sql, "select a || b from t");
        assert_eq!(
            tr("select concat(a, b) from t").sql,
            "select concat(a, b) from t"
        );
        assert_eq!(
            tr("select ucase(cusnam) from t").sql,
            "select UPPER(cusnam) from t"
        );
        // Db2 for i SQL Reference, table-reference: TABLE(function) with a
        // correlation name; in PostgreSQL the call itself is the table.
        assert_eq!(
            tr("select x.objname from table (qsys2.object_statistics('A', '*PGM')) as x, t").sql,
            "select x.objname from qsys2.object_statistics('A', '*PGM') as x, t"
        );
        assert_eq!(
            tr("select * from t join table(f(t.a, (1))) x on 1 = 1").sql,
            "select * from t join f(t.a, (1)) x on 1 = 1"
        );
        let t = tr("select rrn(c), cusnam from custmast c");
        assert_eq!(t.sql, "select c.rrn, cusnam from custmast c");
        assert!(t.wants_rrn);
        assert!(!tr("select * from custmast").wants_rrn);
        // CURRENT TIMESTAMP is a TIMESTAMP without a time zone, read once
        // for the statement (Db2 for i SQL Reference, special registers).
        assert_eq!(
            tr("select current timestamp from sysibm.sysdummy1").sql,
            "select CAST(statement_timestamp() AS timestamp) from (VALUES ('Y')) AS sysdummy1(ibmreqd)"
        );
        assert_eq!(tr("select * from t with nc").sql, "select * from t");
        assert_eq!(
            tr("select * from t optimize for 10 rows").sql,
            "select * from t"
        );
        assert_eq!(tr("select 'it''s' from t;").sql, "select 'it''s' from t");
        // The read-only clause (Db2 for i SQL Reference, select-statement:
        // FOR READ ONLY, or FOR FETCH ONLY), as IBM i Access Client
        // Solutions' Data Transfer sends it.
        assert_eq!(tr("select a from t for fetch only").sql, "select a from t");
        assert_eq!(
            tr("select a from t for read only with ur").sql,
            "select a from t"
        );
    }

    // CASE WHEN takes no EXISTS and no IN (fullselect); WHERE EXISTS is
    // fine (PUB400 2026-10-27, verify3 x_errors, verify4 y_ids).
    #[test]
    fn case_conditions_without_subquery_predicates() {
        let err = |s: &str| translate(s, &|_| false).unwrap_err().to_string();
        assert!(
            err("select case when exists (select 1 from t) then 1 else 0 end from u")
                .starts_with("SQL0104")
        );
        assert!(
            err("update t set v = case when exists (select 1 from u) then 'Y' end")
                .starts_with("SQL0104")
        );
        assert!(
            err("select case when 1 in (select 1 from t) then 'IN' end from u")
                .starts_with("SQL0115")
        );
        assert!(translate("select 1 from t where exists (select 1 from u)", &|_| false).is_ok());
        assert!(translate("select case when a in (1, 2) then 1 end from t", &|_| false).is_ok());
        assert!(translate(
            "select case when a = 1 then (select 1 from u where exists (select 1 from v)) end from t",
            &|_| false
        )
        .is_ok());
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
        // Code for IBM i's "Test runSQL (bad basic select)" (IBM i's
        // message ends with the tokens ", FROM INTO").
        let e = err("select from qiws.qcustcdt");
        assert!(
            e.starts_with("SQL0104") && e.ends_with(": , FROM INTO."),
            "{e}"
        );
        assert!(err("select * from as4tainr.user_profiles").starts_with("SQL0551"));
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

// ------------------------------------------------------------ programs

/// What an embedded statement returned: rows (each value as text, NULL as
/// None) or a count of rows changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Exec {
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
        /// Each column's type (the SQL CLI describes columns by it).
        types: Vec<ColType>,
    },
    Count(Kind, u64),
}

/// A result column's type: PostgreSQL's type name, and its modifier when
/// the column comes from a table (length, precision and scale), else -1.
#[derive(Debug, Clone, PartialEq)]
pub struct ColType {
    pub pg: String,
    pub typmod: i32,
    /// A file column's SQL data type as SYSCOLUMNS shows it, where
    /// PostgreSQL's type does not tell (NUMERIC zoned and DECIMAL packed
    /// are both numeric; BINARY and VARBIN both bytea, with the length in
    /// `typmod` as for the character types).
    pub db2: Option<String>,
}

/// The SQL session of a running program (embedded SQL): one connection as
/// the SQL role, the job's library list as the search path, each
/// statement committed when it succeeds (COMMIT(*NONE)).
pub struct Session {
    client: std::sync::Arc<Client>,
    /// Run under commitment control: statements run in the commitment
    /// definition's unit of work, not committed when they end.
    cmt: Option<std::sync::Arc<crate::cmtctl::Definition>>,
    libraries: Vec<String>,
    libl: Vec<String>,
    /// The job it runs for: (user, current library, job).
    context: Option<(String, Option<String>, Option<String>)>,
    /// The program running the statements (journal entries name it).
    program: Option<(String, String)>,
    database_url: String,
    /// Describe result columns by their files' SQL types (SYSCOLUMNS:
    /// NUMERIC, BINARY, GRAPHIC, CLOB, DECFLOAT): one more query per
    /// statement, for clients that show the types (Mapepire).
    pub describe_types: bool,
}

impl Session {
    /// The job the session's statements run for (QSYS2 services such as
    /// JOBLOG_INFO('*') and LIBRARY_LIST_INFO use it).
    pub async fn set_context(
        &mut self,
        user: &str,
        curlib: Option<String>,
        job: Option<String>,
    ) -> Result<()> {
        self.client
            .batch_execute(&context_sql(user, curlib.as_deref(), job.as_deref()))
            .await?;
        self.context = Some((user.to_string(), curlib, job));
        Ok(())
    }

    /// The call stack QSYS2.STACK_INFO('*') lists for the statements (a
    /// JSON array, oldest entry first): as4tainr.stack.
    pub async fn set_stack(&self, json: &str) -> Result<()> {
        let q = "SELECT set_config('as4tainr.stack', $1, false)";
        self.client.execute(q, &[&json]).await?;
        if let Some(d) = &self.cmt {
            d.client.execute(q, &[&json]).await?;
        }
        Ok(())
    }

    /// The program the statements run for: its name and library.
    pub async fn set_program(&mut self, name: &str, library: &str) -> Result<()> {
        self.client
            .execute(
                "SELECT set_config('as4tainr.program', $1, false), set_config('as4tainr.program_lib', $2, false)",
                &[&name, &library],
            )
            .await?;
        self.program = Some((name.to_string(), library.to_string()));
        Ok(())
    }

    /// A session whose statements run under a commitment definition (a
    /// program with a commit level other than *NONE).
    pub async fn open_in(
        client: &Client,
        database_url: &str,
        libl: &[String],
        cmt: std::sync::Arc<crate::cmtctl::Definition>,
    ) -> Result<Session> {
        let mut s = Session::open(client, database_url, libl).await?;
        s.cmt = Some(cmt);
        Ok(s)
    }

    /// The commitment definition the session runs under, if any.
    pub fn commitment(&self) -> Option<&std::sync::Arc<crate::cmtctl::Definition>> {
        self.cmt.as_ref()
    }

    pub async fn open(client: &Client, database_url: &str, libl: &[String]) -> Result<Session> {
        let sql_client = connect_as_sql_role(client, database_url).await?;
        let path: Vec<String> = libl.iter().map(|l| ident(l)).collect();
        sql_client
            .batch_execute(&format!(
                "SET search_path TO {}{}",
                joined_path(&path),
                qtemp_setting(false)
            ))
            .await?;
        Ok(Session {
            context: None,
            program: None,
            database_url: database_url.to_string(),
            client: std::sync::Arc::new(sql_client),
            cmt: None,
            libraries: catalog::list_libraries(client).await?,
            libl: libl.to_vec(),
            describe_types: false,
        })
    }

    /// A temporary view of this name in the session the statements run
    /// in (it comes before the library list), or none (`body` None): what
    /// a database file override (OVRDBF TOFILE, MBR) makes of a file name
    /// for SQL.
    pub async fn temp_view(&self, name: &str, body: Option<&str>) -> Result<()> {
        let client: &Client = match &self.cmt {
            Some(d) => &d.client,
            None => &self.client,
        };
        let view = format!("pg_temp.{}", ident(name));
        client
            .batch_execute(&format!("DROP VIEW IF EXISTS {view}"))
            .await?;
        if let Some(b) = body {
            client
                .batch_execute(&format!("CREATE TEMP VIEW {} AS {b}", ident(name)))
                .await?;
        }
        Ok(())
    }

    /// Run one statement (host variables already in it as literals).
    /// `catalog_client` reads file descriptions (INSERT without columns).
    pub async fn exec(&self, catalog_client: &Client, sql: &str) -> Result<Exec> {
        let sql = &crate::sqlvar::substitute(catalog_client, &self.libl, sql).await?;
        let sql = &crate::temporal::rewrite(catalog_client, &self.libl, sql).await?;
        // Column types change a division's value; for +, - and * only
        // the described type, which Mapepire shows (describe_types).
        let types = if self.describe_types || needs_column_types(sql) {
            numeric_columns(catalog_client, &self.libl, sql).await
        } else {
            Default::default()
        };
        let t = translate_typed(sql, &|n| self.libraries.iter().any(|l| l == n), types)?;
        let mut text = system_column_names(catalog_client, &self.libl, &t.sql).await?;
        if let Some((target, at)) = &t.insert_needs_columns {
            let name = if target.contains('.') {
                target
                    .replace('"', "")
                    .replace('.', "/")
                    .to_ascii_uppercase()
            } else {
                target.to_ascii_uppercase()
            };
            let cols = insert_columns(catalog_client, &self.libl, &name).await?;
            text.insert_str(*at, &format!(" ({})", cols.join(", ")));
        }
        // Source files the statement names: views of their first members.
        let sources = crate::source::sql_sources(catalog_client, &self.libl, &text).await?;
        if !sources.is_empty() {
            let on: &Client = match &self.cmt {
                Some(d) => &d.client,
                None => &self.client,
            };
            text = crate::source::sql_source_views(on, &sources, &text).await?;
        }
        // Under commitment control: on the definition's session, inside a
        // savepoint, so that a statement that fails takes back only its
        // own changes.
        let Some(d) = &self.cmt else {
            if t.kind == Kind::Query {
                return self.exec_on(&self.client, &t, text).await;
            }
            // A change: committed when its trigger programs (if any) end
            // normally.
            self.client.batch_execute("BEGIN").await?;
            let data = crate::runtime::DataSession::Sql(self.client.clone());
            let (r, keep) = self
                .exec_change(data, catalog_client, &t, &text, false)
                .await;
            if r.is_ok() || keep {
                self.client.batch_execute("COMMIT").await?
            } else {
                self.client.batch_execute("ROLLBACK").await?
            }
            return r;
        };
        let path: Vec<String> = self.libl.iter().map(|l| ident(l)).collect();
        d.client
            .batch_execute(&format!(
                "SAVEPOINT as4tainr_stmt; SET LOCAL search_path TO {}{}",
                joined_path(&path),
                qtemp_setting(true)
            ))
            .await?;
        if let Some((u, c, j)) = &self.context {
            d.client
                .batch_execute(&context_sql_in(u, c.as_deref(), j.as_deref(), true))
                .await?;
        }
        if let Some((n, l)) = &self.program {
            d.client
                .execute(
                    "SELECT set_config('as4tainr.program', $1, true), set_config('as4tainr.program_lib', $2, true)",
                    &[n, l],
                )
                .await?;
        }
        let r = if t.kind == Kind::Query {
            self.exec_on(&d.client, &t, text).await
        } else {
            let data = crate::runtime::DataSession::Cmt(d.clone());
            self.exec_change(data, catalog_client, &t, &text, true)
                .await
                .0
        };
        match &r {
            Ok(_) => {
                d.client
                    .batch_execute("RELEASE SAVEPOINT as4tainr_stmt")
                    .await?
            }
            // The statement's changes are taken back (and journaled).
            Err(_) => crate::journal::rollback_to(&d.client, "as4tainr_stmt").await?,
        }
        r
    }

    /// The types of a statement's parameters (`$1`, `$2`... where its
    /// markers are) as PostgreSQL infers them from where they are: the
    /// type, and the length (typmod) of the column a parameter is put
    /// into or compared with, when the statement shows it (`COL = $n`,
    /// an INSERT's column list), else -1.
    pub async fn parameter_types(
        &self,
        catalog_client: &Client,
        sql: &str,
    ) -> Result<Vec<ColType>> {
        let sql = &crate::temporal::rewrite(catalog_client, &self.libl, sql).await?;
        // Column types change a division's value; for +, - and * only
        // the described type, which Mapepire shows (describe_types).
        let types = if self.describe_types || needs_column_types(sql) {
            numeric_columns(catalog_client, &self.libl, sql).await
        } else {
            Default::default()
        };
        let t = translate_typed(sql, &|n| self.libraries.iter().any(|l| l == n), types)?;
        let mut text = system_column_names(catalog_client, &self.libl, &t.sql).await?;
        if let Some((target, at)) = &t.insert_needs_columns {
            let name = if target.contains('.') {
                target
                    .replace('"', "")
                    .replace('.', "/")
                    .to_ascii_uppercase()
            } else {
                target.to_ascii_uppercase()
            };
            let cols = insert_columns(catalog_client, &self.libl, &name).await?;
            text.insert_str(*at, &format!(" ({})", cols.join(", ")));
        }
        let stmt = self
            .client
            .prepare(&text)
            .await
            .map_err(|e| map_error(&e, &text))?;
        let names = parameter_columns(&text, stmt.params().len());
        let mut out = Vec::new();
        for (k, ty) in stmt.params().iter().enumerate() {
            let mut typmod = -1;
            let mut db2 = None;
            if let Some((tables, col)) = names.get(k).cloned().flatten() {
                for tb in tables {
                    let r = self
                        .client
                        .query_opt(
                            "SELECT a.atttypmod,
                                    EXISTS (SELECT 1 FROM qsys2.syscolumns s
                                     WHERE s.system_table_schema = upper(n.nspname) COLLATE \"C\"
                                       AND s.system_table_name = upper(r.relname) COLLATE \"C\"
                                       AND s.system_column_name = upper(a.attname) COLLATE \"C\"
                                       AND s.data_type = 'NUMERIC')
                               FROM pg_catalog.pg_attribute a
                               JOIN pg_catalog.pg_class r ON r.oid = a.attrelid
                               JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace
                              WHERE a.attrelid = to_regclass($1) AND a.attname = $2",
                            &[&tb, &col],
                        )
                        .await
                        .ok()
                        .flatten();
                    if let Some(r) = r {
                        typmod = r.get(0);
                        if r.get::<_, bool>(1) {
                            db2 = Some("NUMERIC".to_string());
                        }
                        break;
                    }
                }
            }
            out.push(ColType {
                pg: ty.name().to_string(),
                typmod,
                db2,
            });
        }
        Ok(out)
    }

    /// Make a change with the trigger programs of its files (ADDPFTRG):
    /// the result, and whether to keep the change though it is an error.
    async fn exec_change(
        &self,
        data: crate::runtime::DataSession,
        catalog: &Client,
        t: &Translated,
        text: &str,
        cmt: bool,
    ) -> (Result<Exec>, bool) {
        if !crate::pftrg::any(data.client()).await {
            return (
                self.exec_on(data.client(), t, text.to_string()).await,
                false,
            );
        }
        let (user, curlib, job) = match &self.context {
            Some((u, c, j)) => (u.clone(), c.clone(), j.clone()),
            None => ("QSECOFR".to_string(), None, None),
        };
        let who = crate::pftrg::Caller {
            database_url: &self.database_url,
            libl: self.libl.clone(),
            user,
            job,
            curlib,
        };
        let c = data.clone();
        let (r, keep) = crate::pftrg::run(data, catalog, &who, cmt, || {
            self.exec_on(c.client(), t, text.to_string())
        })
        .await;
        (r.map_err(trigger_failed_if), keep)
    }

    async fn exec_on(&self, client: &Client, t: &Translated, text: String) -> Result<Exec> {
        let in_tx = t.kind != Kind::Query || self.cmt.is_some();
        let text = db2_datetime_literals(client, text, in_tx).await;
        if t.kind == Kind::Query {
            let stmt = client
                .prepare(&text)
                .await
                .map_err(|e| map_error(&e, &text))?;
            let keep: Vec<bool> = stmt
                .columns()
                .iter()
                .map(|c| t.wants_rrn || c.name() != "rrn")
                .collect();
            let columns = stmt
                .columns()
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(c, _)| crate::catalog::ibm_name(c.name()))
                .collect();
            let stamps: Vec<bool> = stmt
                .columns()
                .iter()
                .map(|c| c.type_().name() == "timestamp")
                .collect();
            let mut types = Vec::new();
            for c in stmt
                .columns()
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(c, _)| c)
            {
                // The length from the statement's description, else from
                // the column's table.
                let typmod = match (c.type_modifier(), c.table_oid(), c.column_id()) {
                    (m, _, _) if m >= 0 => m,
                    (_, Some(oid), Some(col)) => client
                        .query_opt(
                            "SELECT atttypmod FROM pg_catalog.pg_attribute WHERE attrelid = $1 AND attnum = $2",
                            &[&oid, &col],
                        )
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.get::<_, i32>(0))
                        .unwrap_or(-1),
                    _ => -1,
                };
                types.push(ColType {
                    pg: c.type_().name().to_string(),
                    typmod,
                    db2: None,
                });
            }
            if self.describe_types {
                let cols: Vec<&tokio_postgres::Column> = stmt
                    .columns()
                    .iter()
                    .zip(&keep)
                    .filter(|(_, k)| **k)
                    .map(|(c, _)| c)
                    .collect();
                describe_types(client, &cols, &mut types).await;
            }
            let mut rows = Vec::new();
            for m in client
                .simple_query(&text)
                .await
                .map_err(|e| map_error(&e, &text))?
            {
                if let SimpleQueryMessage::Row(r) = m {
                    rows.push(
                        (0..r.len())
                            .filter(|&j| keep.get(j).copied().unwrap_or(true))
                            .map(|j| {
                                r.get(j).map(|v| {
                                    if stamps.get(j) == Some(&true) {
                                        db2_timestamp(v)
                                    } else {
                                        v.to_string()
                                    }
                                })
                            })
                            .collect(),
                    );
                }
            }
            Ok(Exec::Rows {
                columns,
                rows,
                types,
            })
        } else {
            let n = client
                .execute(&text, &[])
                .await
                .map_err(|e| map_error(&e, &text))?;
            if n > 0 {
                if let Some(d) = &self.cmt {
                    d.changed();
                }
            }
            Ok(Exec::Count(t.kind, n))
        }
    }
}

/// CREATE VIEW: the view's query, translated like any statement, is
/// defined by the SQL role (which owns the view, so the query reads only
/// what the SQL role may read). The SQL role may create objects in the
/// library only while this runs. `columns`: the view's column names, if
/// given. `alias`: a second name for the view (its SQL name, when it has
/// a system name of its own).
#[allow(clippy::too_many_arguments)]
pub async fn create_view(
    client: &Client,
    database_url: &str,
    libl: &[String],
    library: &str,
    name: &str,
    alias: Option<&str>,
    columns: &[String],
    query: &str,
    check_option: bool,
) -> Result<()> {
    let libraries = catalog::list_libraries(client).await?;
    let query = &crate::temporal::rewrite(client, libl, query).await?;
    let t = translate(query, &|n| libraries.iter().any(|l| l == n))?;
    if t.kind != Kind::Query {
        bail!("SQL0104: a view is defined by a query (SELECT).");
    }
    let session = Session::open(client, database_url, libl).await?;
    let me: String = client
        .query_one("SELECT current_user::text", &[])
        .await?
        .get(0);
    let schema = ident(library);
    client.batch_execute(catalog::ROLE_DDL_LOCK).await?;
    let r = create_view_locked(
        client,
        &session,
        &schema,
        name,
        alias,
        columns,
        &t,
        check_option,
        &me,
    )
    .await;
    let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
    r
}

#[allow(clippy::too_many_arguments)]
async fn create_view_locked(
    client: &Client,
    session: &Session,
    schema: &str,
    name: &str,
    alias: Option<&str>,
    columns: &[String],
    t: &Translated,
    check_option: bool,
    me: &str,
) -> Result<()> {
    grant_create(client, schema).await?;
    let cols = if columns.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            columns
                .iter()
                .map(|c| ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let view = format!("{schema}.{}", ident(name));
    let mut text = format!(
        "CREATE VIEW {view}{cols} AS {}{};",
        t.sql.trim_end().trim_end_matches(';'),
        if check_option {
            " WITH CASCADED CHECK OPTION"
        } else {
            ""
        }
    );
    text.push_str(&format!(" GRANT SELECT ON {view} TO {};", ident_role(me)));
    if let Some(a) = alias {
        let second = format!("{schema}.{}", ident(a));
        text.push_str(&format!(
            " CREATE VIEW {second} AS SELECT * FROM {view}; GRANT SELECT ON {second} TO {};",
            ident_role(me)
        ));
    }
    let r = session
        .client
        .batch_execute(&format!("BEGIN; {text} COMMIT;"))
        .await;
    if r.is_err() {
        let _ = session.client.batch_execute("ROLLBACK").await;
    }
    let revoke = batch_retrying(
        client,
        &format!("REVOKE CREATE ON SCHEMA {schema} FROM {SQL_ROLE}"),
    )
    .await;
    r.map_err(|e| map_error(&e, &text))?;
    revoke?;
    Ok(())
}

/// Run data definition that as4tainr wrote (a function from an SQL
/// routine) as the SQL role, which may create objects in the library only
/// while it runs.
pub(crate) async fn define_as_sql_role(
    client: &Client,
    database_url: &str,
    libl: &[String],
    library: &str,
    text: &str,
) -> Result<()> {
    let session = Session::open(client, database_url, libl).await?;
    let schema = ident(library);
    client.batch_execute(catalog::ROLE_DDL_LOCK).await?;
    let granted = grant_create(client, &schema).await;
    if let Err(e) = granted {
        let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
        return Err(e.into());
    }
    // Retried when PostgreSQL saw a catalog row changed by another
    // session at the same moment (concurrent definitions on one cluster).
    let mut tries = 0;
    let r = loop {
        let r = session
            .client
            .batch_execute(&format!("BEGIN; {text}; COMMIT;"))
            .await;
        if r.is_err() {
            let _ = session.client.batch_execute("ROLLBACK").await;
        }
        match r {
            Err(e)
                if tries < 5
                    && e.as_db_error()
                        .is_some_and(|d| d.message().starts_with("tuple concurrently")) =>
            {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(50 * tries)).await;
            }
            r => break r,
        }
    };
    let revoke = batch_retrying(
        client,
        &format!("REVOKE CREATE ON SCHEMA {schema} FROM {SQL_ROLE}"),
    )
    .await;
    let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
    r.map_err(|e| map_error(&e, text))?;
    revoke?;
    Ok(())
}

/// The columns a query returns, as fields (CREATE TABLE AS): the query
/// is translated, defined as a temporary view by the SQL role, and its
/// columns read from information_schema.
pub(crate) async fn query_fields(
    client: &Client,
    database_url: &str,
    libl: &[String],
    query: &str,
) -> Result<Vec<crate::dds::FieldDef>> {
    let libraries = catalog::list_libraries(client).await?;
    let query = &crate::temporal::rewrite(client, libl, query).await?;
    let types = numeric_columns(client, libl, query).await;
    let t = translate_typed(query, &|n| libraries.iter().any(|l| l == n), types)?;
    if t.kind != Kind::Query {
        bail!("SQL0104: a query (SELECT) is expected after AS.");
    }
    // A column without a name needs the table's column list (verify71
    // r71_mapepire "ctas": SQL0153, SQLSTATE 42908).
    let no_name =
        || anyhow!("SQL0153: A column list is needed: a column of the query has no name.");
    let session = Session::open(client, database_url, libl).await?;
    let c = &session.client;
    client.batch_execute(catalog::ROLE_DDL_LOCK).await?;
    let made = c
        .batch_execute(&format!(
            "CREATE TEMP VIEW as4tainr_ctas AS {}",
            t.sql.trim().trim_end_matches(';')
        ))
        .await;
    if let Err(e) = made {
        let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
        if e.to_string().contains("?column?") {
            return Err(no_name());
        }
        return Err(map_error(&e, &t.sql));
    }
    let cols = c
        .query(
            "SELECT column_name::text, data_type::text, character_maximum_length, numeric_precision, numeric_scale
               FROM information_schema.columns
              WHERE table_schema = pg_my_temp_schema()::regnamespace::text AND table_name = 'as4tainr_ctas'
              ORDER BY ordinal_position",
            &[],
        )
        .await;
    // The table columns the query reads, to give a result column the
    // decimal form (zoned or packed) of the column it is (Db2 for i keeps
    // a ZONED column zoned in CREATE TABLE AS; PostgreSQL's numeric has
    // no form: PUB400 2026-11-04, verify46 o46_refcv printed the zoned
    // fields of QADSPPGM copied with CREATE TABLE AS as digits).
    let used: Vec<(String, String, String)> = c
        .query(
            "SELECT DISTINCT n.nspname::text, t.relname::text, a.attname::text
               FROM pg_rewrite w
               JOIN pg_depend d ON d.classid = 'pg_rewrite'::regclass AND d.objid = w.oid
               JOIN pg_class t ON t.oid = d.refobjid
               JOIN pg_namespace n ON n.oid = t.relnamespace
               JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
              WHERE w.ev_class = 'pg_temp.as4tainr_ctas'::regclass AND d.refobjsubid > 0",
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| (r.get(0), r.get(1), r.get(2)))
                .collect()
        })
        .unwrap_or_default();
    let _ = c.batch_execute("DROP VIEW as4tainr_ctas").await;
    let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
    let cols = cols?;
    // A column the query does not name (an expression: PostgreSQL names
    // it after its function, or ?column?).
    let names: Vec<String> = cols
        .iter()
        .map(|r| r.get::<_, String>(0).to_ascii_uppercase())
        .collect();
    let named = crate::mapepire::unnamed_columns(query, names.clone());
    if names.iter().zip(&named).any(|(a, b)| a != b) {
        return Err(no_name());
    }
    let (mut fields, _) = catalog::fields_of_columns(&cols)?;
    for f in fields
        .iter_mut()
        .filter(|f| f.data_type == crate::dds::DataType::Packed)
    {
        let col = f.name.to_ascii_lowercase();
        let from: Vec<&(String, String, String)> = used.iter().filter(|u| u.2 == col).collect();
        // Only a name one table gives: an expression's column keeps packed.
        if let [(schema, table, _)] = from.as_slice() {
            let q = format!(
                "{}/{}",
                schema.to_ascii_uppercase(),
                table.to_ascii_uppercase()
            );
            if let Ok(fr) = catalog::resolve_file(client, libl, &q).await {
                if let Some(src) = fr
                    .file
                    .fields
                    .iter()
                    .find(|x| x.name.eq_ignore_ascii_case(&f.name))
                {
                    if src.data_type == crate::dds::DataType::Zoned
                        && src.length == f.length
                        && src.decimals == f.decimals
                    {
                        f.data_type = crate::dds::DataType::Zoned;
                    }
                }
            }
        }
    }
    if fields.is_empty() {
        bail!("SQL0104: the query has no columns.");
    }
    Ok(fields)
}

/// A role name quoted as is (role names are not IBM i names).
fn ident_role(r: &str) -> String {
    format!("\"{}\"", r.replace('"', "\"\""))
}
