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
             REVOKE ALL ON SCHEMA as4tainr FROM {SQL_ROLE};
             CREATE TABLE IF NOT EXISTS as4tainr.sql_login (id int PRIMARY KEY, password text NOT NULL);"
        ))
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
    client
        .batch_execute(&format!("ALTER ROLE {SQL_ROLE} PASSWORD '{password}'"))
        .await?;
    for lib in catalog::list_libraries(client).await? {
        // Jobs' QTEMP libraries got their grants when they were made, and
        // their jobs drop them at any moment: granting again would race
        // with that DROP SCHEMA.
        if lib.starts_with("#QP") || lib.starts_with("#QT") {
            continue;
        }
        // Another process may drop a library or a table in it while the
        // grants run (a job's QTEMP at its end, parallel jobs): a library
        // that is gone is skipped, and a grant that met a table being
        // dropped ("tuple concurrently deleted/updated") is tried again.
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
            client
                .batch_execute(&format!("ALTER ROLE {SQL_ROLE} PASSWORD '{password}'"))
                .await
                .map_err(|_| anyhow!("SQL is not available: cannot log in as {SQL_ROLE} ({e})."))?;
            cfg.connect(tokio_postgres::NoTls)
                .await
                .map_err(|e| anyhow!("SQL is not available: cannot log in as {SQL_ROLE} ({e})."))?
        }
        Err(e) => bail!("SQL is not available: cannot log in as {SQL_ROLE} ({e})."),
    };
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
    let rows = client
        .query(
            "SELECT DISTINCT name, alias FROM as4tainr.file_fields
              WHERE library = ANY($1) AND alias IS NOT NULL AND alias <> name AND name = ANY($2)",
            &[&libl, &names],
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
            "SELECT DISTINCT alias FROM as4tainr.file_fields
              WHERE library = ANY($1) AND alias = ANY($2)",
            &[&libl, &names],
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
    if let [one] = sig.as_slice() {
        if let Tok::Number(n) = &toks[*one] {
            return num(n);
        }
        return None;
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
        // The left operand: a number, or a call ending at `pe`.
        let ls = match &toks[pe] {
            Tok::Number(_) => Some(pe),
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
        };
        // The right operand: a number, or a call starting at `ns`.
        let re = match &toks[ns] {
            Tok::Number(_) => Some(ns),
            Tok::Word(_) => (ns + 1..toks.len())
                .find(|&j| !matches!(toks[j], Tok::Space(_)))
                .filter(|&o| toks[o] == Tok::Punct("(".into()))
                .and_then(|o| paren_span(&toks, o).map(|(c, _)| c)),
            _ => None,
        };
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
        let mut new: Vec<Tok> = toks[..ls].to_vec();
        new.push(Tok::Word("as4tainr_svc.ddiv".into()));
        new.push(Tok::Punct("(".into()));
        new.extend_from_slice(&toks[ls..=pe]);
        new.push(Tok::Punct(",".into()));
        new.extend_from_slice(&toks[ns..=re]);
        new.push(Tok::Punct(",".into()));
        new.push(Tok::Number(scale.to_string()));
        new.push(Tok::Punct(")".into()));
        let resume = new.len();
        new.extend_from_slice(&toks[re + 1..]);
        toks = new;
        k = resume;
    }
    toks
}

pub fn translate(sql: &str, is_library: &dyn Fn(&str) -> bool) -> Result<Translated> {
    let mut toks = listagg(decimal_division(sequence_values(
        crate::sqljsonfn::rewrite(crate::sqljson::rewrite(tokenize(sql.trim())?)?)?,
        is_library,
    )));
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
    // Binary string constants BX'C1C2' and hexadecimal constants X'C1C2':
    // bytes, as PostgreSQL bytea constants.
    let mut i = 0;
    while i + 1 < toks.len() {
        let hex = matches!(&toks[i], Tok::Word(w) if w.eq_ignore_ascii_case("BX") || w.eq_ignore_ascii_case("X"));
        if let (true, Tok::Str(h)) = (hex, &toks[i + 1]) {
            if h.len() % 2 != 0 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("SQL0110: {h} is not a valid hexadecimal constant.");
            }
            let lit = format!("'\\x{h}'::bytea");
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
    let sql = &crate::temporal::rewrite_system_time(client, libl, sql).await?;
    let t = translate(sql, &|n| libraries.iter().any(|l| l == n))?;
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
        let f = catalog::resolve_sql_file(client, libl, &name)
            .await
            .map_err(|_| anyhow!("SQL0204: {name} not found."))?;
        let cols: Vec<String> = f.file.fields.iter().map(|fd| fd.column()).collect();
        text.insert_str(*at, &format!(" ({})", cols.join(", ")));
    }
    let path: Vec<String> = libl.iter().map(|l| ident(l)).collect();
    // One transaction per statement (COMMIT(*NONE)), on a connection of
    // its own as the SQL role.
    let sql_client = std::sync::Arc::new(connect_as_sql_role(client, database_url).await?);
    sql_client.batch_execute(&context).await?;
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
    tx.batch_execute(&format!("SET LOCAL search_path TO {}", joined_path(path)))
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
    },
    Count(Kind, u64),
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
            .batch_execute(&format!("SET search_path TO {}", joined_path(&path)))
            .await?;
        Ok(Session {
            context: None,
            program: None,
            database_url: database_url.to_string(),
            client: std::sync::Arc::new(sql_client),
            cmt: None,
            libraries: catalog::list_libraries(client).await?,
            libl: libl.to_vec(),
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
        let sql = &crate::temporal::rewrite_system_time(catalog_client, &self.libl, sql).await?;
        let t = translate(sql, &|n| self.libraries.iter().any(|l| l == n))?;
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
            let f = catalog::resolve_sql_file(catalog_client, &self.libl, &name)
                .await
                .map_err(|_| anyhow!("SQL0204: {name} not found."))?;
            let cols: Vec<String> = f.file.fields.iter().map(|fd| fd.column()).collect();
            text.insert_str(*at, &format!(" ({})", cols.join(", ")));
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
                "SAVEPOINT as4tainr_stmt; SET LOCAL search_path TO {}",
                joined_path(&path)
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
            Ok(Exec::Rows { columns, rows })
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
    client
        .batch_execute(&format!("GRANT CREATE ON SCHEMA {schema} TO {SQL_ROLE}"))
        .await?;
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
    let revoke = client
        .batch_execute(&format!("REVOKE CREATE ON SCHEMA {schema} FROM {SQL_ROLE}"))
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
    let granted = client
        .batch_execute(&format!("GRANT CREATE ON SCHEMA {schema} TO {SQL_ROLE}"))
        .await;
    if let Err(e) = granted {
        let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
        return Err(e.into());
    }
    let r = session
        .client
        .batch_execute(&format!("BEGIN; {text}; COMMIT;"))
        .await;
    if r.is_err() {
        let _ = session.client.batch_execute("ROLLBACK").await;
    }
    let revoke = client
        .batch_execute(&format!("REVOKE CREATE ON SCHEMA {schema} FROM {SQL_ROLE}"))
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
    let t = translate(query, &|n| libraries.iter().any(|l| l == n))?;
    if t.kind != Kind::Query {
        bail!("SQL0104: a query (SELECT) is expected after AS.");
    }
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
    let _ = c.batch_execute("DROP VIEW as4tainr_ctas").await;
    let _ = client.batch_execute(catalog::ROLE_DDL_UNLOCK).await;
    let cols = cols?;
    let (fields, _) = catalog::fields_of_columns(&cols)?;
    if fields.is_empty() {
        bail!("SQL0104: the query has no columns.");
    }
    Ok(fields)
}

/// A role name quoted as is (role names are not IBM i names).
fn ident_role(r: &str) -> String {
    format!("\"{}\"", r.replace('"', "\"\""))
}
