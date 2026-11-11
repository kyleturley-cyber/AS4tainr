//! Mapepire: the database protocol of Code for IBM i (the VS Code
//! extension) and the other Mapepire clients (mapepire-js, -python,
//! -java). `as4tainr mapepire --single` serves one client on standard
//! input and output, as the Mapepire server's single mode does when a
//! client starts it over SSH.
//!
//! Written from the protocol as the open-source Mapepire server and
//! mapepire-js (Apache-2.0) define it: each request is one line of JSON
//! with an `id` and a `type`; each reply is one line of JSON with the same
//! `id`, the request type's fields, `success` and `execution_time` (ms),
//! and on failure `error` (plus `sql_state` and `sql_rc` for an SQL
//! error). Types: connect, sql, prepare_sql, prepare_sql_execute,
//! execute, sqlmore, sqlclose, cl and rccl, getdbjob, getversion, ping,
//! setconfig, gettracedata, exit; any other type fails. Result rows come
//! in blocks (`rows`, 1000 by default) with `is_done`; a row is an object
//! keyed by column name, or an array (`terse`). Fixed-length character
//! values lose their trailing blanks; numbers are JSON numbers written
//! with the database's own digits (never through floating point).
//!
//! The statements run as as4tainr's STRSQL runs them (data definition,
//! object locks, the job's library list); the job is a database server
//! job (QP0ZSPWT under QUSER, as the Java server started over SSH runs:
//! PUB400 2026-11-09, verify64 m64_mapepire, for the signed-on user).
//! What as4tainr does differently from IBM's server is listed in
//! docs/fidelity-gaps.md ("Mapepire").

use crate::commands::{Job, Outcome};
use crate::sql::{ColType, Exec};
use anyhow::{anyhow, Result};
use std::collections::{HashMap, VecDeque};

// ------------------------------------------------------------------ JSON

/// A JSON value; numbers keep their text.
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as text (a string, number or boolean).
    pub fn text(&self) -> Option<String> {
        match self {
            J::Str(s) | J::Num(s) => Some(s.clone()),
            J::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        self.text().and_then(|t| t.trim().parse().ok())
    }

    fn bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            J::Str(s) => match s.to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn str(s: impl Into<String>) -> J {
        J::Str(s.into())
    }

    pub fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(n),
            J::Str(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
            J::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for J {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();
        self.write(&mut s);
        f.write_str(&s)
    }
}

/// Parse one JSON text.
pub fn parse(text: &str) -> Result<J> {
    let b: Vec<char> = text.chars().collect();
    let mut p = 0;
    let v = value(&b, &mut p)?;
    skip(&b, &mut p);
    if p != b.len() {
        return Err(anyhow!("text after the JSON value"));
    }
    Ok(v)
}

fn skip(b: &[char], p: &mut usize) {
    while *p < b.len() && b[*p].is_whitespace() {
        *p += 1;
    }
}

fn value(b: &[char], p: &mut usize) -> Result<J> {
    skip(b, p);
    let c = *b.get(*p).ok_or_else(|| anyhow!("JSON ends early"))?;
    match c {
        '{' => {
            *p += 1;
            let mut m = Vec::new();
            skip(b, p);
            if b.get(*p) == Some(&'}') {
                *p += 1;
                return Ok(J::Obj(m));
            }
            loop {
                skip(b, p);
                let J::Str(k) = value(b, p)? else {
                    return Err(anyhow!("an object key is not a string"));
                };
                skip(b, p);
                if b.get(*p) != Some(&':') {
                    return Err(anyhow!("':' expected"));
                }
                *p += 1;
                let v = value(b, p)?;
                m.push((k, v));
                skip(b, p);
                match b.get(*p) {
                    Some(',') => *p += 1,
                    Some('}') => {
                        *p += 1;
                        return Ok(J::Obj(m));
                    }
                    _ => return Err(anyhow!("',' or '}}' expected")),
                }
            }
        }
        '[' => {
            *p += 1;
            let mut a = Vec::new();
            skip(b, p);
            if b.get(*p) == Some(&']') {
                *p += 1;
                return Ok(J::Arr(a));
            }
            loop {
                a.push(value(b, p)?);
                skip(b, p);
                match b.get(*p) {
                    Some(',') => *p += 1,
                    Some(']') => {
                        *p += 1;
                        return Ok(J::Arr(a));
                    }
                    _ => return Err(anyhow!("',' or ']' expected")),
                }
            }
        }
        '"' => {
            *p += 1;
            let mut s = String::new();
            loop {
                let c = *b.get(*p).ok_or_else(|| anyhow!("string not ended"))?;
                *p += 1;
                match c {
                    '"' => return Ok(J::Str(s)),
                    '\\' => {
                        let e = *b.get(*p).ok_or_else(|| anyhow!("string not ended"))?;
                        *p += 1;
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            'u' => {
                                let hex = |p: &mut usize| -> Result<u32> {
                                    let h: String =
                                        b.get(*p..*p + 4).unwrap_or(&[]).iter().collect();
                                    *p += 4;
                                    u32::from_str_radix(&h, 16)
                                        .map_err(|_| anyhow!("bad \\u escape"))
                                };
                                let mut u = hex(p)?;
                                if (0xD800..0xDC00).contains(&u)
                                    && b.get(*p) == Some(&'\\')
                                    && b.get(*p + 1) == Some(&'u')
                                {
                                    *p += 2;
                                    let lo = hex(p)?;
                                    u = 0x10000
                                        + ((u - 0xD800) << 10)
                                        + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                }
                                s.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
                            }
                            other => s.push(other),
                        }
                    }
                    other => s.push(other),
                }
            }
        }
        't' | 'f' | 'n' => {
            let word: String = b[*p..]
                .iter()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            *p += word.len();
            match word.as_str() {
                "true" => Ok(J::Bool(true)),
                "false" => Ok(J::Bool(false)),
                "null" => Ok(J::Null),
                _ => Err(anyhow!("unexpected '{word}'")),
            }
        }
        _ => {
            let n: String = b[*p..]
                .iter()
                .take_while(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
                .collect();
            if n.is_empty() || n.parse::<f64>().is_err() {
                return Err(anyhow!("unexpected character '{c}'"));
            }
            *p += n.chars().count();
            Ok(J::Num(n))
        }
    }
}

// ------------------------------------------------------------ the server

/// A failure the reply reports: its text, and for an SQL error its
/// SQLSTATE and SQLCODE.
#[derive(Debug)]
pub struct Failure {
    pub text: String,
    pub sql: Option<(String, i64)>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Failure {}

fn fail(text: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Failure {
        text: text.into(),
        sql: None,
    })
}

/// An error from as4tainr's SQL ("SQL0204: text") as Mapepire's server
/// reports it: the message's text alone, with its SQLSTATE and SQLCODE
/// (PUB400 2026-11-09, verify64 m64_mapepire: "error":"NOPE64 in
/// AS4TRAINR type *FILE not found.", "sql_state":"42704", "sql_rc":-204).
pub(crate) fn sql_failure(e: &anyhow::Error) -> anyhow::Error {
    if let Some(f) = e.downcast_ref::<Failure>() {
        return anyhow::Error::new(Failure {
            text: f.text.clone(),
            sql: f.sql.clone(),
        });
    }
    let all = format!("{e:#}");
    // The last message is the one that ended the statement.
    let line = all
        .lines()
        .rev()
        .find(|l| crate::jobs::message_id(l).len() == 7)
        .unwrap_or_else(|| all.lines().next().unwrap_or(""))
        .to_string();
    let id = crate::jobs::message_id(&line).to_string();
    if id.len() != 7 {
        return fail(line);
    }
    let text = crate::jobs::message_text(&line).trim().to_string();
    let sql = id
        .strip_prefix("SQL")
        .and_then(|d| d.parse::<i64>().ok())
        .map(|n| {
            let code = -n;
            (
                crate::rpg::interp::exec_sql::sqlstate(code).to_string(),
                code,
            )
        });
    anyhow::Error::new(Failure { text, sql })
}

/// The rows of a result not yet sent.
pub(crate) struct Results {
    pub(crate) columns: Vec<String>,
    pub(crate) types: Vec<ColType>,
    pub(crate) rows: VecDeque<Vec<Option<String>>>,
    done: bool,
}

/// A statement a request opened: its result, and (prepared) its text.
struct Open {
    sql: Option<String>,
    results: Option<Results>,
    terse: bool,
}

/// What a statement did.
pub(crate) enum Done {
    Rows(Results),
    Count(u64),
    /// CALL: its first result set, and its parameters.
    Call(Option<Results>, Vec<J>),
}

/// One client's server: its job and its open statements.
pub struct Server<'a> {
    pub job: Job<'a>,
    /// SET SCHEMA's schema (unqualified names), else the library list.
    schema: Option<String>,
    session: Option<(Vec<String>, crate::sql::Session)>,
    open: HashMap<String, Open>,
    unparseable: u32,
    incomplete: u32,
    /// Answer `cl` as Mapepire servers before 2.5 do (job log rows), as
    /// the server Code for IBM i installs (2.3) does.
    pub legacy_cl: bool,
}

/// The version `getversion` reports: as4tainr's own.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl<'a> Server<'a> {
    /// The default schema for unqualified names (a JDBC client's
    /// default library), as SET SCHEMA sets it.
    pub(crate) fn set_schema(&mut self, schema: Option<String>) {
        self.schema = schema;
    }

    pub fn new(job: Job<'a>) -> Server<'a> {
        Server {
            job,
            schema: None,
            session: None,
            open: HashMap::new(),
            unparseable: 0,
            incomplete: 0,
            legacy_cl: true,
        }
    }

    /// The job's name as the client sees it: NUMBER/USER/NAME.
    /// Servers before 2.5 name it NUMBER-USER-NAME (PUB400's 2.3.2:
    /// "902549-QUSER-QP0ZSPWT", verify64 m64_mapepire).
    pub fn job_name(&self) -> String {
        let name = self
            .job
            .id
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or_else(|| self.job.job_key());
        if self.legacy_cl {
            name.replace('/', "-")
        } else {
            name
        }
    }

    /// Answer one request line: the reply line (None for an empty line or
    /// a `//` comment), and whether the client asked to end.
    pub async fn handle(&mut self, line: &str) -> Option<(String, bool)> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with("//") {
            return None;
        }
        let started = std::time::Instant::now();
        let mut reply: Vec<(String, J)> = Vec::new();
        let req = parse(line).ok().filter(|r| matches!(r, J::Obj(_)));
        let mut exit = false;
        let result: Result<()> = match &req {
            None => {
                self.unparseable += 1;
                reply.push((
                    "id".into(),
                    J::str(format!("unparseable_{}", self.unparseable)),
                ));
                reply.push(("unparseable".into(), J::str(line)));
                Err(fail(format!("The request is not JSON: '{line}'")))
            }
            Some(r) if r.get("id").is_none() || r.get("type").and_then(J::text).is_none() => {
                self.incomplete += 1;
                reply.push((
                    "id".into(),
                    J::str(format!("incomplete_{}", self.incomplete)),
                ));
                reply.push(("incomplete".into(), J::str(line)));
                Err(fail(format!(
                    "The request has no 'id' or no 'type': '{line}'"
                )))
            }
            Some(r) => {
                reply.push(("id".into(), r.get("id").cloned().unwrap_or(J::Null)));
                let ty = r.get("type").and_then(J::text).unwrap_or_default();
                exit = ty == "exit";
                self.request(&ty, r, &mut reply).await
            }
        };
        match result {
            Ok(()) => put(&mut reply, "success", J::Bool(true)),
            Err(e) => {
                let e = sql_failure(&e);
                let f = e.downcast_ref::<Failure>();
                put(
                    &mut reply,
                    "error",
                    J::str(
                        f.map(|f| f.text.clone())
                            .unwrap_or_else(|| format!("{e:#}")),
                    ),
                );
                put(&mut reply, "success", J::Bool(false));
                if let Some((state, code)) = f.and_then(|f| f.sql.clone()) {
                    put(&mut reply, "sql_state", J::str(state));
                    put(&mut reply, "sql_rc", J::Num(code.to_string()));
                }
            }
        }
        put(
            &mut reply,
            "execution_time",
            J::Num(started.elapsed().as_millis().to_string()),
        );
        Some((J::Obj(reply).to_string(), exit))
    }

    async fn request(&mut self, ty: &str, r: &J, reply: &mut Vec<(String, J)>) -> Result<()> {
        let id = r.get("id").and_then(J::text).unwrap_or_default();
        let rows = r.get("rows").and_then(J::int).unwrap_or(1000).max(0) as usize;
        let terse = r.get("terse").and_then(J::bool).unwrap_or(false);
        match ty {
            "ping" => {
                put(reply, "alive", J::Bool(true));
                put(reply, "db_alive", J::Bool(true));
            }
            "getversion" => {
                put(reply, "build_date", J::str(""));
                put(reply, "version", J::str(VERSION));
            }
            "getdbjob" => put(reply, "job", J::str(self.job_name())),
            "connect" => {
                if let Some(props) = r.get("props").and_then(J::text) {
                    self.connect_props(&props)?;
                }
                put(reply, "job", J::str(self.job_name()));
            }
            "setconfig" => {
                let dest = r
                    .get("tracedest")
                    .and_then(J::text)
                    .map(|d| d.trim().to_ascii_uppercase())
                    .unwrap_or_else(|| "IN_MEM".into());
                let level = r
                    .get("tracelevel")
                    .and_then(J::text)
                    .map(|d| d.trim().to_ascii_uppercase())
                    .unwrap_or_else(|| "OFF".into());
                put(reply, "tracedest", J::str(dest));
                put(reply, "tracelevel", J::str(level));
            }
            "gettracedata" => put(reply, "tracedata", J::str("")),
            "exit" => {}
            "sql" => {
                let sql = r
                    .get("sql")
                    .and_then(J::text)
                    .ok_or_else(|| fail("The request has no 'sql'."))?;
                let done = self.statement(&sql).await?;
                let mut open = Open {
                    sql: None,
                    results: None,
                    terse,
                };
                self.reply_done(done, rows, &mut open, reply, false);
                self.open.insert(id, open);
            }
            "prepare_sql" | "prepare_sql_execute" => {
                let sql = r
                    .get("sql")
                    .and_then(J::text)
                    .ok_or_else(|| fail("The request has no 'sql'."))?;
                self.describe(&sql, reply).await?;
                let mut open = Open {
                    sql: Some(sql.clone()),
                    results: None,
                    terse,
                };
                if ty == "prepare_sql_execute" {
                    let parms = r.get("parameters").cloned().unwrap_or(J::Arr(vec![]));
                    self.execute(&sql, &parms, rows, &mut open, reply).await?;
                }
                self.open.insert(id, open);
            }
            "execute" | "sqlmore" | "sqlclose" => {
                let cont = r
                    .get("cont_id")
                    .and_then(J::text)
                    .ok_or_else(|| fail("Bad request: the request has no 'cont_id'."))?;
                let Some(mut open) = self.open.remove(&cont) else {
                    return Err(fail(format!(
                        "Bad request: no statement has the id '{cont}'."
                    )));
                };
                let result = match ty {
                    "sqlmore" => {
                        let terse = open.terse;
                        let (data, done) = match open.results.as_mut() {
                            Some(res) => block(res, rows, terse),
                            None => (vec![], true),
                        };
                        put(reply, "data", J::Arr(data));
                        put(reply, "is_done", J::Bool(done));
                        Ok(())
                    }
                    "sqlclose" => {
                        open.results = None;
                        Ok(())
                    }
                    _ => match open.sql.clone() {
                        // execute: the 2.3 server's native driver fails it
                        // with SQLSTATE HY021, SQLCODE -99999, after
                        // prepare_sql of a query or an UPDATE and after
                        // prepare_sql_execute (verify66, verify68 "exe",
                        // verify69 p69_mapepire "exe2", "exe3").
                        Some(_) if self.legacy_cl => Err(anyhow::Error::new(Failure {
                            text: "The call level interface could not run the statement.".into(),
                            sql: Some(("HY021".into(), -99999)),
                        })),
                        Some(sql) => {
                            let parms = r.get("parameters").cloned().unwrap_or(J::Arr(vec![]));
                            self.execute(&sql, &parms, rows, &mut open, reply).await
                        }
                        None => Err(fail(format!(
                            "Bad request: the statement '{cont}' was not prepared."
                        ))),
                    },
                };
                if ty != "sqlclose" {
                    self.open.insert(cont, open);
                }
                result?;
            }
            "cl" | "rccl" => {
                let cmd = r
                    .get("cmd")
                    .and_then(J::text)
                    .ok_or_else(|| fail("The request has no 'cmd'."))?;
                self.cl(&cmd, reply).await?;
            }
            other => return Err(fail(format!("The request type '{other}' is not known."))),
        }
        Ok(())
    }

    /// The connection's properties (`name=value;...`, as JDBC takes
    /// them): `libraries` sets the library list (the first is also the
    /// default schema with SQL naming).
    fn connect_props(&mut self, props: &str) -> Result<()> {
        for p in props.split(';') {
            let Some((k, v)) = p.split_once('=') else {
                continue;
            };
            if k.trim().eq_ignore_ascii_case("libraries") {
                let mut libs = Vec::new();
                for l in v.split([',', ' ']).map(str::trim).filter(|l| !l.is_empty()) {
                    if l.starts_with('*') {
                        continue;
                    }
                    let l = crate::catalog::validate_name("library", &l.to_ascii_uppercase())
                        .map_err(|e| fail(format!("{e:#}")))?;
                    if !libs.contains(&l) {
                        libs.push(l);
                    }
                }
                if !libs.is_empty() {
                    self.job.libl = libs;
                }
            }
        }
        Ok(())
    }

    fn libl(&self) -> Vec<String> {
        match &self.schema {
            Some(s) => vec![s.clone()],
            None => self.job.library_list(),
        }
    }

    /// The SQL session for the library list (a new one when it changed).
    async fn session(&mut self) -> Result<&crate::sql::Session> {
        let libl = self.libl();
        if self.session.as_ref().map(|(l, _)| l) != Some(&libl) {
            let mut s =
                crate::sql::Session::open(self.job.client, &self.job.env.database_url, &libl)
                    .await?;
            s.describe_types = true;
            s.set_context(
                &self.job.user,
                Some(self.job.current_library()),
                Some(self.job.job_key()),
            )
            .await?;
            self.session = Some((libl, s));
        }
        Ok(&self.session.as_ref().expect("session").1)
    }

    /// Run one statement.
    pub(crate) async fn statement(&mut self, sql: &str) -> Result<Done> {
        let sql = sql.trim().trim_end_matches(';').trim();
        if !crate::sqlpl::is_compound(sql) {
            if let Some((name, args)) = crate::sqlpl::parse_call(sql)? {
                return self.call(&name, &args).await;
            }
        }
        let curlib = Some(self.job.current_library());
        if crate::ddl::try_run(
            &self.job.env.database_url,
            &self.job.user,
            &self.job.library_list(),
            curlib.clone(),
            &mut self.schema,
            sql,
        )
        .await?
        .is_some()
        {
            return Ok(Done::Count(0));
        }
        let libl = self.libl();
        let owner = self.job.lock_owner().await?;
        crate::objlock::sql_tables_free(
            self.job.client,
            sql,
            &libl,
            &owner,
            crate::objlock::default_wait(&self.job.job_key()),
        )
        .await?;
        let first = sql
            .split(|c: char| c.is_whitespace() || c == '(')
            .find(|w| !w.is_empty())
            .unwrap_or("")
            .to_ascii_uppercase();
        let plain = matches!(
            first.as_str(),
            "SELECT" | "WITH" | "VALUES" | "INSERT" | "UPDATE" | "DELETE"
        ) && !crate::sqlpl::calls_qcmdexc(sql);
        if plain && is_query(&first) && sql.to_ascii_uppercase().contains("QCMDEXC") {
            return self.query_with_commands(sql).await;
        }
        if plain {
            let client = self.job.client;
            let session = self.session().await?;
            return Ok(match session.exec(client, sql).await? {
                Exec::Rows {
                    columns,
                    rows,
                    types,
                } => Done::Rows(Results {
                    columns: unnamed_columns(sql, columns),
                    types,
                    rows: rows.into(),
                    done: false,
                }),
                Exec::Count(_, n) => Done::Count(n),
            });
        }
        let out = crate::sql::run_as(
            self.job.client,
            &self.job.env.database_url,
            &libl,
            &self.job.user,
            curlib,
            Some(self.job.job_key()),
            sql,
        )
        .await?;
        Ok(match out {
            Outcome2::Rows { columns, rows, .. } => {
                let types = columns.iter().map(|_| text_type()).collect();
                Done::Rows(Results {
                    columns,
                    types,
                    rows: rows
                        .into_iter()
                        .map(|r| r.into_iter().map(|v| (v != "-").then_some(v)).collect())
                        .collect(),
                    done: false,
                })
            }
            Outcome2::Count(_, n) => Done::Count(n),
            Outcome2::Message(_) => Done::Count(0),
        })
    }

    /// A query that calls the QCMDEXC scalar function for its rows: run
    /// once to gather the commands, which are run (in the order they were
    /// first met), then again with their results (services.rs,
    /// QCMDEXC_FUNCTION).
    async fn query_with_commands(&mut self, sql: &str) -> Result<Done> {
        let key = self.job.job_key();
        let client = self.job.client;
        client
            .execute("DELETE FROM as4tainr.qcmdexc_calls WHERE job = $1", &[&key])
            .await?;
        client
            .execute(
                "INSERT INTO as4tainr.qcmdexc_calls (job, cmd, rc) VALUES ($1, '', 0)",
                &[&key],
            )
            .await?;
        let mut result = None;
        for _ in 0..4 {
            let r = {
                let session = self.session().await?;
                session.exec(client, sql).await
            };
            let pending: Vec<(i64, String)> = client
                .query(
                    "SELECT seq, cmd FROM as4tainr.qcmdexc_calls
                      WHERE job = $1 AND rc IS NULL ORDER BY seq",
                    &[&key],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect();
            if pending.is_empty() {
                result = Some(r);
                break;
            }
            for (seq, cmd) in pending {
                let rc: i32 = if self.job.run(&cmd).await.is_ok() {
                    1
                } else {
                    -1
                };
                client
                    .execute(
                        "UPDATE as4tainr.qcmdexc_calls SET rc = $2 WHERE job = $1 AND seq = $3",
                        &[&key, &rc, &seq],
                    )
                    .await?;
            }
        }
        client
            .execute("DELETE FROM as4tainr.qcmdexc_calls WHERE job = $1", &[&key])
            .await?;
        let r = result.ok_or_else(|| fail("The statement's commands did not end."))??;
        Ok(match r {
            Exec::Rows {
                columns,
                rows,
                types,
            } => Done::Rows(Results {
                columns,
                types,
                rows: rows.into(),
                done: false,
            }),
            Exec::Count(_, n) => Done::Count(n),
        })
    }

    /// CALL a procedure: its first result set and its parameters.
    async fn call(&mut self, name: &str, args: &[String]) -> Result<Done> {
        // CALL QSYS2.QCMDEXC('command'): the command runs in this job (its
        // overrides, library list and job log stay with the job).
        let up = name.trim().to_ascii_uppercase().replace('/', ".");
        if matches!(up.as_str(), "QSYS2.QCMDEXC" | "QCMDEXC") && args.len() == 1 {
            let cmd = self.literal_text(&args[0]).await?;
            let r = self.job.run(&cmd).await;
            if let Some(id) = self.job.id.clone() {
                crate::jobs::log(self.job.client, &id, "*RQS", &cmd).await;
            }
            return match r {
                Ok(_) => Ok(Done::Count(0)),
                Err(e) => {
                    let all = format!("{e:#}");
                    let last = all.lines().last().unwrap_or("").to_string();
                    Err(fail(format!(
                        "[{}] {}",
                        crate::jobs::message_id(&last),
                        crate::jobs::message_text(&last).trim()
                    )))
                }
            };
        }
        // The catalog procedures of SYSIBM a JDBC client calls for its
        // metadata: their result set (sqlmeta.rs).
        if let Some(q) = crate::sqlmeta::query(&up, args) {
            return match Box::pin(self.statement(&q)).await? {
                Done::Rows(r) => Ok(Done::Call(Some(r), Vec::new())),
                other => Ok(other),
            };
        }
        let libl = self.libl();
        let curlib = Some(self.job.current_library());
        let client = self.job.client;
        let url = self.job.env.database_url.clone();
        let user = self.job.user.clone();
        let session = self.session().await?;
        let env = crate::sqlpl::Env {
            session,
            catalog: client,
            database_url: &url,
            libl: &libl,
            user: &user,
            curlib,
        };
        let (routine, out) = crate::sqlpl::call_exprs(&env, name, args).await?;
        let results = out
            .result_sets
            .into_iter()
            .next()
            .map(|(columns, rows)| Results {
                types: columns.iter().map(|_| text_type()).collect(),
                columns,
                rows: rows.into(),
                done: false,
            });
        let parms = routine
            .parms
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (ty, precision, scale) = written(&p.ty);
                let mut m = vec![
                    ("index".to_string(), J::Num((i + 1).to_string())),
                    ("type".to_string(), J::str(ty.clone())),
                    ("precision".to_string(), J::Num(precision.to_string())),
                    ("scale".to_string(), J::Num(scale.to_string())),
                    ("name".to_string(), J::str(p.name.to_ascii_uppercase())),
                ];
                if p.mode != "IN" {
                    let v = out.values.get(i).cloned().flatten();
                    m.push((
                        "value".to_string(),
                        match v {
                            None => J::Null,
                            Some(v) if is_numeric_name(&ty) => number(v.trim()),
                            Some(v) => J::str(v.trim()),
                        },
                    ));
                }
                J::Obj(m)
            })
            .collect();
        Ok(Done::Call(results, parms))
    }

    /// An argument's value as text (a string constant, or an expression
    /// the database works out).
    async fn literal_text(&mut self, arg: &str) -> Result<String> {
        let a = arg.trim();
        if a.len() >= 2 && a.starts_with('\'') && a.ends_with('\'') {
            return Ok(a[1..a.len() - 1].replace("''", "'"));
        }
        match Box::pin(self.statement(&format!("VALUES {a}"))).await? {
            Done::Rows(r) => Ok(r
                .rows
                .front()
                .and_then(|row| row.first().cloned().flatten())
                .unwrap_or_default()),
            _ => Ok(String::new()),
        }
    }

    /// The reply fields of a statement that ran.
    fn reply_done(
        &self,
        done: Done,
        rows: usize,
        open: &mut Open,
        reply: &mut Vec<(String, J)>,
        prepared: bool,
    ) {
        let (results, count, parms) = match done {
            Done::Rows(r) => (Some(r), -1i64, None),
            Done::Count(n) => (None, n as i64, None),
            Done::Call(r, p) => (r, if prepared { 0 } else { -1 }, Some(p)),
        };
        let parms = if prepared {
            Some(J::Arr(parms.unwrap_or_default()))
        } else {
            None
        };
        match results {
            Some(mut res) => {
                put(reply, "has_results", J::Bool(true));
                put(reply, "update_count", J::Num("-1".into()));
                put(reply, "metadata", self.metadata(&res, true));
                let (data, done) = block(&mut res, rows, open.terse);
                put(reply, "data", J::Arr(data));
                if let Some(p) = parms {
                    put(reply, "output_parms", p);
                }
                put(reply, "is_done", J::Bool(done));
                open.results = Some(res);
            }
            None => {
                if prepared {
                    put(reply, "data", J::Arr(vec![]));
                }
                put(reply, "has_results", J::Bool(false));
                put(reply, "update_count", J::Num(count.max(0).to_string()));
                if let Some(p) = parms {
                    put(reply, "output_parms", p);
                    put(reply, "is_done", J::Bool(true));
                }
            }
        }
    }

    /// prepare_sql: the statement's parameters, and a query's columns.
    async fn describe(&mut self, sql: &str, reply: &mut Vec<(String, J)>) -> Result<()> {
        let markers = marker_count(sql);
        let call = crate::sqlpl::parse_call(sql.trim().trim_end_matches(';'))
            .ok()
            .flatten();
        let mut parameters = Vec::new();
        let mut routine_parms = None;
        if let Some((name, _)) = &call {
            let libl = self.libl();
            if let Ok(Some(r)) = crate::sqlpl::find(self.job.client, &libl, name, "PROCEDURE").await
            {
                routine_parms = Some(r.parms.clone());
            }
        }
        // Other statements' parameters: typed by where they are, named
        // "?" (PUB400's 2.3.2, verify68 o68_mapepire "prep", "pins":
        // `I = ?` INTEGER 10, an INSERT's CHAR(5) column CHAR 5).
        let mut inferred = Vec::new();
        if call.is_none() && markers > 0 {
            let numbered: Vec<String> = (1..=markers).map(|k| format!("${k}")).collect();
            if let Ok(text) = bind(sql, &numbered) {
                let catalog = self.job.client;
                if let Ok(s) = self.session().await {
                    inferred = s.parameter_types(catalog, &text).await.unwrap_or_default();
                }
            }
        }
        for i in 0..markers {
            let p = routine_parms.as_ref().and_then(|ps| ps.get(i));
            let guess = inferred.get(i).map(|t| {
                let (ty, precision, scale, _) = column_type(t, 0);
                (ty, precision, scale)
            });
            let (ty, precision, scale) =
                p.map(|p| written(&p.ty))
                    .or(guess)
                    .unwrap_or(("VARCHAR".into(), 0, 0));
            parameters.push(J::Obj(vec![
                ("type".into(), J::str(ty)),
                (
                    "mode".into(),
                    J::str(
                        p.map(|p| p.mode.to_ascii_uppercase())
                            .unwrap_or_else(|| "IN".into()),
                    ),
                ),
                ("precision".into(), J::Num(precision.to_string())),
                ("scale".into(), J::Num(scale.to_string())),
                (
                    "name".into(),
                    p.map(|p| J::str(p.name.to_ascii_uppercase()))
                        .unwrap_or_else(|| if call.is_none() { J::str("?") } else { J::Null }),
                ),
            ]));
        }
        put(reply, "parameter_count", J::Num(markers.to_string()));
        let mut meta = Vec::new();
        // A query's columns: the query run with null parameters.
        let first = sql
            .split(|c: char| c.is_whitespace() || c == '(')
            .find(|w| !w.is_empty())
            .unwrap_or("")
            .to_ascii_uppercase();
        if call.is_none() && matches!(first.as_str(), "SELECT" | "WITH" | "VALUES") {
            let nulls = vec!["NULL".to_string(); markers];
            let text = bind(sql, &nulls)?;
            if let Done::Rows(r) = self.statement(&text).await? {
                if let J::Obj(mut m) = self.metadata(&r, false) {
                    // A prepared statement's columns are described with
                    // name, type, display size and label only (PUB400's
                    // 2.3.2, verify66 n66_mapepire "prep").
                    if self.legacy_cl {
                        for (k, v) in m.iter_mut() {
                            if let (true, J::Arr(cols)) = (k == "columns", v) {
                                for c in cols.iter_mut() {
                                    if let J::Obj(f) = c {
                                        f.truncate(4);
                                    }
                                }
                            }
                        }
                    }
                    meta = m;
                }
            }
        }
        meta.push(("parameters".into(), J::Arr(parameters)));
        put(reply, "metadata", J::Obj(meta));
        Ok(())
    }

    /// execute: the prepared statement with these parameters (an array
    /// of arrays: once for each, the changes counted together).
    async fn execute(
        &mut self,
        sql: &str,
        parms: &J,
        rows: usize,
        open: &mut Open,
        reply: &mut Vec<(String, J)>,
    ) -> Result<()> {
        let list = match parms {
            J::Arr(a) => a.clone(),
            J::Null => vec![],
            _ => return Err(fail("'parameters' is not an array.")),
        };
        if matches!(list.first(), Some(J::Arr(_))) {
            let mut total = 0u64;
            for set in &list {
                let J::Arr(set) = set else {
                    return Err(fail("A batch's parameters are not arrays."));
                };
                let text = bind(sql, &literals(set))?;
                if let Done::Count(n) = self.statement(&text).await? {
                    total += n;
                }
            }
            put(reply, "batch_added", J::Num(list.len().to_string()));
            put(reply, "data", J::Arr(vec![]));
            put(reply, "has_results", J::Bool(false));
            put(reply, "update_count", J::Num(total.to_string()));
            put(reply, "output_parms", J::Arr(vec![]));
            put(reply, "is_done", J::Bool(true));
            return Ok(());
        }
        let text = bind(sql, &literals(&list))?;
        let done = self.statement(&text).await?;
        let done = match done {
            Done::Count(n) => {
                put(reply, "data", J::Arr(vec![]));
                put(reply, "has_results", J::Bool(false));
                put(reply, "update_count", J::Num(n.to_string()));
                put(reply, "output_parms", J::Arr(vec![]));
                put(reply, "is_done", J::Bool(true));
                return Ok(());
            }
            other => other,
        };
        self.reply_done(done, rows, open, reply, true);
        Ok(())
    }

    /// A result's metadata.
    fn metadata(&self, r: &Results, blob_flag: bool) -> J {
        let columns = r
            .columns
            .iter()
            .zip(&r.types)
            .enumerate()
            .map(|(j, (name, t))| {
                let longest = r
                    .rows
                    .iter()
                    .filter_map(|row| row.get(j).cloned().flatten())
                    .map(|v| v.chars().count())
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let (ty, precision, scale, display) = column_type(t, longest);
                let mut m = vec![
                    ("name".to_string(), J::str(name.clone())),
                    ("type".to_string(), J::str(ty)),
                    ("display_size".to_string(), J::Num(display.to_string())),
                    ("label".to_string(), J::str(name.clone())),
                    ("precision".to_string(), J::Num(precision.to_string())),
                    ("scale".to_string(), J::Num(scale.to_string())),
                ];
                // Servers before 2.5 describe a column with those six
                // (PUB400's 2.3.2, verify64 m64_mapepire).
                if self.legacy_cl {
                    return J::Obj(m);
                }
                m.extend([
                    ("autoIncrement".to_string(), J::Bool(false)),
                    // JDBC's columnNullableUnknown: as4tainr does not say.
                    ("nullable".to_string(), J::Num("2".into())),
                    ("readOnly".to_string(), J::Bool(false)),
                    ("writeable".to_string(), J::Bool(true)),
                    ("table".to_string(), J::str("")),
                ]);
                if blob_flag {
                    m.push(("blob_as_url".to_string(), J::Bool(false)));
                }
                J::Obj(m)
            })
            .collect();
        J::Obj(vec![
            ("column_count".into(), J::Num(r.columns.len().to_string())),
            ("job".into(), J::str(self.job_name())),
            ("columns".into(), J::Arr(columns)),
        ])
    }

    /// cl: run a CL command; its messages are the reply's data.
    async fn cl(&mut self, cmd: &str, reply: &mut Vec<(String, J)>) -> Result<()> {
        // The request first in the job log, then what it sent.
        if let Some(id) = self.job.id.clone() {
            crate::jobs::log(self.job.client, &id, "*RQS", cmd).await;
        }
        let log_from = self.job_log_count().await;
        let result = self.job.run(cmd).await;
        // A display command in this job (it has no display) prints what
        // it would show, as a batch job's does (jobs.rs run_batch_job),
        // under the job's printer file override (OVRPRTF FILE(*PRTF):
        // user data, held, owner).
        let mut result = result;
        if let Ok(Outcome::Display(d)) = &result {
            match self.print_display(cmd, d).await {
                Ok(m) => result = Ok(Outcome::Messages(m)),
                Err(e) => result = Err(e),
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        let lines: Vec<String> = match &result {
            Ok(Outcome::Messages(m)) => m.clone(),
            Ok(Outcome::Display(d)) => d.print_msg.iter().cloned().collect(),
            Ok(_) => vec![],
            Err(e) => format!("{e:#}").lines().map(str::to_string).collect(),
        };
        let failed = result.is_err();
        // The messages: a failed command's last is its escape message,
        // those before it diagnostics; a command that ran sent
        // completion messages.
        let with_id: Vec<(String, String, &'static str)> = {
            let ids: Vec<&String> = lines
                .iter()
                .filter(|l| crate::jobs::message_id(l).len() == 7)
                .collect();
            let n = ids.len();
            ids.into_iter()
                .enumerate()
                .map(|(i, l)| {
                    let ty = if !failed {
                        "COMPLETION"
                    } else if i + 1 == n {
                        "ESCAPE"
                    } else {
                        "DIAGNOSTIC"
                    };
                    (
                        crate::jobs::message_id(l).to_string(),
                        crate::jobs::message_text(l).trim().to_string(),
                        ty,
                    )
                })
                .collect()
        };
        // They are in the job log (QSYS2.JOBLOG_INFO('*') shows them).
        if let Some(id) = self.job.id.clone() {
            for (m, text, ty) in &with_id {
                let kind = match *ty {
                    "ESCAPE" => "*ESCAPE",
                    "DIAGNOSTIC" => "*DIAG",
                    _ => "*COMP",
                };
                crate::jobs::log(self.job.client, &id, kind, &format!("{m}: {text}")).await;
            }
        }
        // What the job log got from the request on (messages programs
        // and commands put there besides those returned: CPYFRMSTMF's
        // CPC7305), as the Mapepire server reads them from JOBLOG_INFO.
        let with_id = match self.job.id.clone() {
            Some(id) => crate::jobs::job_log(self.job.client, id.number)
                .await
                .unwrap_or_default()
                .into_iter()
                .skip(log_from)
                .filter(|e| e.kind != "*RQS" && !e.msgid.is_empty())
                .map(|e| {
                    let ty: &'static str = match e.kind.as_str() {
                        "*ESCAPE" => "ESCAPE",
                        "*DIAG" => "DIAGNOSTIC",
                        "*INFO" => "INFORMATIONAL",
                        "*NOTIFY" => "NOTIFY",
                        _ => "COMPLETION",
                    };
                    (
                        e.msgid.clone(),
                        crate::jobs::message_text(&e.text).trim().to_string(),
                        ty,
                    )
                })
                .collect(),
            None => with_id,
        };
        if self.legacy_cl {
            self.cl_job_log_rows(&with_id, now, reply);
        } else {
            let ms = (now / 1000).to_string();
            let data = with_id
                .iter()
                .map(|(id, text, ty)| {
                    let (lib, file) = message_file(id);
                    J::Obj(vec![
                        ("id".into(), J::str(id.clone())),
                        ("text".into(), J::str(text.clone())),
                        ("help".into(), J::Null),
                        ("type".into(), J::str(*ty)),
                        (
                            "severity".into(),
                            J::Num(crate::pgmmsg::system_severity(id).to_string()),
                        ),
                        ("time".into(), J::Num(ms.clone())),
                        ("messageLibrary".into(), lib.map(J::str).unwrap_or(J::Null)),
                        ("messageFile".into(), file.map(J::str).unwrap_or(J::Null)),
                        ("sendingProgram".into(), J::Null),
                        ("sendingInstruction".into(), J::Null),
                        ("receivingProgram".into(), J::Null),
                        ("receivingInstruction".into(), J::Null),
                    ])
                })
                .collect();
            put(reply, "data", J::Arr(data));
        }
        if failed {
            let escape = with_id.iter().rev().find(|(_, _, t)| *t == "ESCAPE");
            // The escape message's text, as the SQL error of QSYS2.QCMDEXC:
            // SQLSTATE 38501, SQLCODE -443 (verify64 m64_mapepire cl2,
            // cl3: "Errors occurred in command." for a command not valid).
            let text = match escape {
                Some((id, _, _)) if id == "CPF0006" && !self.legacy_cl => {
                    "CL command execution failed: Unknown error".to_string()
                }
                Some((_, text, _)) => text.clone(),
                None => lines.join(" "),
            };
            return Err(anyhow::Error::new(Failure {
                text,
                sql: Some(("38501".into(), -443)),
            }));
        }
        Ok(())
    }

    /// How many messages the job's log has.
    async fn job_log_count(&self) -> usize {
        match &self.job.id {
            Some(id) => crate::jobs::job_log(self.job.client, id.number)
                .await
                .map(|l| l.len())
                .unwrap_or(0),
            None => 0,
        }
    }

    /// Print a display command's list to a spooled file of this job.
    async fn print_display(
        &mut self,
        cmd: &str,
        d: &crate::commands::ListDisplay,
    ) -> Result<Vec<String>> {
        let Some(id) = self.job.id.clone() else {
            return Ok(vec![]);
        };
        let first = cmd
            .trim()
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let file = crate::report::printer_file(&first);
        let ovr = self
            .job
            .calls
            .overrides
            .iter()
            .rev()
            .find(|o| {
                o.kind == crate::ovr::Kind::Printer
                    && (o.file.eq_ignore_ascii_case("*PRTF") || o.file.eq_ignore_ascii_case(file))
            })
            .cloned();
        let usrdta = ovr
            .as_ref()
            .and_then(|o| o.usrdta.clone())
            .unwrap_or_else(|| first.clone());
        let lines = crate::jobs::printed_display(d);
        let spl = crate::spool::print_lines(self.job.client, &id, file, &usrdta, &lines).await?;
        if let Some(o) = &ovr {
            let owner = match o.splfown.as_deref() {
                Some("*CURUSRPRF") => Some(self.job.user.clone()),
                _ => None,
            };
            self.job
                .client
                .execute(
                    "UPDATE as4tainr.spooled_files
                        SET status = CASE WHEN $2 THEN 'HLD' ELSE status END,
                            user_name = coalesce($3, user_name)
                      WHERE id = $1",
                    &[&spl, &o.hold, &owner],
                )
                .await?;
        }
        let mut msgs: Vec<String> = d.print_msg.iter().cloned().collect();
        msgs.push(format!("{} printed to spooled file {file}.", d.title));
        Ok(msgs)
    }

    /// cl as servers before 2.5 answer it: the job log messages the
    /// command sent, as rows of QSYS2.JOBLOG_INFO's columns (with a
    /// SUMMARY first: "ID [TYPE]: text", the type's first four letters).
    fn cl_job_log_rows(
        &self,
        msgs: &[(String, String, &'static str)],
        now_us: u128,
        reply: &mut Vec<(String, J)>,
    ) {
        let stamp = {
            let ((y, mo, d), (h, mi, sec), us) = crate::rpg::dates::from_epoch_us(now_us as i64);
            format!("{y:04}-{mo:02}-{d:02}-{h:02}.{mi:02}.{sec:02}.{us:06}")
        };
        // QSYS2.JOBLOG_INFO's columns as the 2.3 server selects them, with
        // their types (verify64 m64_mapepire's metadata: SUMMARY is
        // "ID [TYPE]: text" with the type's first four letters and a
        // blank; MESSAGE_SECOND_LEVEL_TEXT and SEVERITY twice).
        let cols: &[(&str, &str, i32)] = &[
            ("SUMMARY", "nvarchar", 1041),
            ("SEVERITY", "int2", -1),
            ("MESSAGE_SECOND_LEVEL_TEXT", "nvarchar", 4096),
            ("MESSAGE_ID", "varchar", 7),
            ("MESSAGE_TYPE", "varchar", 13),
            ("MESSAGE_SUBTYPE", "varchar", 22),
            ("SEVERITY", "int2", -1),
            ("MESSAGE_TIMESTAMP", "timestamp", -1),
            ("FROM_LIBRARY", "varchar", 10),
            ("FROM_PROGRAM", "varchar", 256),
            ("FROM_MODULE", "varchar", 10),
            ("FROM_PROCEDURE", "varchar", 4096),
            ("FROM_INSTRUCTION", "varchar", 10),
            ("TO_LIBRARY", "varchar", 10),
            ("TO_PROGRAM", "varchar", 10),
            ("TO_MODULE", "varchar", 10),
            ("TO_PROCEDURE", "varchar", 4096),
            ("TO_INSTRUCTION", "varchar", 10),
            ("FROM_USER", "varchar", 10),
            ("MESSAGE_LIBRARY", "varchar", 10),
            ("MESSAGE_FILE", "varchar", 10),
            ("MESSAGE_TOKEN_LENGTH", "int2", -1),
            ("MESSAGE_TOKENS", "varbit_fbd", 12000),
            ("MESSAGE_TEXT", "nvarchar", 1024),
            ("MESSAGE_SECOND_LEVEL_TEXT", "nvarchar", 4096),
            ("MESSAGE_KEY", "binary", 4),
            ("QUALIFIED_JOB_NAME", "varchar", 28),
        ];
        let job = self
            .job
            .id
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or_else(|| self.job.job_key());
        let rows: VecDeque<Vec<Option<String>>> = msgs
            .iter()
            .enumerate()
            .map(|(k, (id, text, ty))| {
                let (lib, file) = message_file(id);
                // Escape messages 40 and diagnostics 30 as PUB400's were
                // (CPF1023, CPD0043); others as pgmmsg.rs knows them.
                let sev = match *ty {
                    "ESCAPE" => 40,
                    "DIAGNOSTIC" => 30,
                    _ => crate::pgmmsg::system_severity(id),
                }
                .to_string();
                vec![
                    Some(format!("{id} [{} ]: {text}", &ty[..4])),
                    Some(sev.clone()),
                    None,
                    Some(id.clone()),
                    Some(ty.to_string()),
                    (*ty == "ESCAPE").then(|| "EXCEPTION HANDLED".to_string()),
                    Some(sev),
                    Some(stamp.clone()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(self.job.user.clone()),
                    lib.map(str::to_string),
                    file.map(str::to_string),
                    None,
                    None,
                    Some(text.clone()),
                    None,
                    Some(format!("\\x{:08x}", k + 1)),
                    Some(job.clone()),
                ]
            })
            .collect();
        let mut res = Results {
            columns: cols.iter().map(|c| c.0.to_string()).collect(),
            types: cols
                .iter()
                .map(|c| ColType {
                    pg: c.1.to_string(),
                    typmod: c.2,
                    db2: None,
                })
                .collect(),
            rows,
            done: false,
        };
        put(reply, "metadata", self.metadata(&res, true));
        let (data, _) = block(&mut res, usize::MAX, false);
        put(reply, "data", J::Arr(data));
        put(reply, "is_done", J::Bool(true));
    }
}

use crate::sql::Outcome as Outcome2;

/// Result columns an expression without a name gave are named by their
/// position, as the IBM i JDBC driver names them ("00001": Code for IBM
/// i reads `VALUES CURRENT TIMESTAMP`'s value by that name). A name is
/// taken for one an expression gave when the statement does not write
/// it (PostgreSQL's ?column?, VALUES' COLUMN1, a function's name), unless
/// the statement selects `*` (whose names it does not write).
pub(crate) fn unnamed_columns(sql: &str, columns: Vec<String>) -> Vec<String> {
    let up = sql.to_ascii_uppercase();
    // The words a column can be named by: not a function's name (SUM(...)
    // makes an unnamed column: verify69 p69_mapepire "num", "ps3").
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '#' | '@' | '$' | '?');
    let mut words: std::collections::HashSet<String> = std::collections::HashSet::new();
    let b: Vec<char> = up.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if is_word(b[i]) {
            let st = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let w: String = b[st..i].iter().collect();
            let mut j = i;
            while j < b.len() && b[j].is_whitespace() {
                j += 1;
            }
            if b.get(j) != Some(&'(') {
                words.insert(w);
            }
        } else {
            i += 1;
        }
    }
    // SELECT * or T.*, not COUNT(*).
    let star = {
        let mut found = false;
        for (k, c) in b.iter().enumerate() {
            if *c == '*' {
                let before: String = b[..k]
                    .iter()
                    .rev()
                    .skip_while(|c| c.is_whitespace())
                    .take(6)
                    .collect();
                if before.starts_with(',') || before.starts_with('.') || before == "TCELES" {
                    found = true;
                }
            }
        }
        found && marker_positions_of(sql, '*') > 0
    };
    columns
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let generated = c == "?COLUMN?"
                || (c.starts_with("COLUMN")
                    && c[6..].chars().all(|d| d.is_ascii_digit())
                    && !words.contains(&c));
            if generated || (!star && !words.contains(&c.to_ascii_uppercase())) {
                format!("{:05}", i + 1)
            } else {
                c
            }
        })
        .collect()
}

/// How many times a character is in a statement outside strings and
/// delimited names.
fn marker_positions_of(sql: &str, ch: char) -> usize {
    let mut n = 0;
    let mut quote: Option<char> = None;
    for c in sql.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == ch => n += 1,
            None => {}
        }
    }
    n
}

fn is_query(first_word: &str) -> bool {
    matches!(first_word, "SELECT" | "WITH" | "VALUES")
}

fn put(reply: &mut Vec<(String, J)>, key: &str, v: J) {
    match reply.iter_mut().find(|(k, _)| k == key) {
        Some(x) => x.1 = v,
        None => reply.push((key.to_string(), v)),
    }
}

fn text_type() -> ColType {
    ColType {
        pg: "text".into(),
        typmod: -1,
        db2: None,
    }
}

/// The message file of a message ID (as the IBM i message files are
/// named: QCPFMSG for CPx, QSQLMSG for SQL, QRNXMSG for RNx).
fn message_file(id: &str) -> (Option<&'static str>, Option<&'static str>) {
    if id.starts_with("CP") {
        (Some("QSYS"), Some("QCPFMSG"))
    } else if id.starts_with("SQL") {
        (Some("QSYS"), Some("QSQLMSG"))
    } else if id.starts_with("RN") {
        (Some("QSYS"), Some("QRNXMSG"))
    } else {
        (None, None)
    }
}

/// The next block of rows, and whether the result has no more (it ends
/// when fewer than `n` rows are left, as a JDBC result set's `next`
/// tells it).
fn block(r: &mut Results, n: usize, terse: bool) -> (Vec<J>, bool) {
    let mut data = Vec::new();
    if r.done {
        return (data, true);
    }
    for _ in 0..n {
        let Some(row) = r.rows.pop_front() else {
            r.done = true;
            break;
        };
        let cells: Vec<J> = row
            .iter()
            .enumerate()
            .map(|(j, v)| cell(r.types.get(j), v.as_deref()))
            .collect();
        data.push(if terse {
            J::Arr(cells)
        } else {
            // A name twice: one key, the later value (as a map keeps it).
            let mut obj: Vec<(String, J)> = Vec::new();
            for (c, v) in r.columns.iter().cloned().zip(cells) {
                put(&mut obj, &c, v);
            }
            J::Obj(obj)
        });
    }
    (data, r.done)
}

/// A number's text as a JSON number (a value JSON cannot write, such as
/// NaN, as text).
fn number(v: &str) -> J {
    let ok = !v.is_empty()
        && v.parse::<f64>().is_ok_and(|f| f.is_finite())
        && v.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '.' | 'e' | 'E' | '+'));
    if ok {
        // JSON allows no leading '+', nor '.5' or '5.'.
        let t = v.trim_start_matches('+');
        let t = if t.starts_with('.') {
            format!("0{t}")
        } else if t.starts_with("-.") {
            format!("-0{}", &t[1..])
        } else {
            t.to_string()
        };
        J::Num(t.trim_end_matches('.').to_string())
    } else {
        J::str(v)
    }
}

/// A decimal's digits as they are, without leading zeros (JSON has
/// none): `-0012.50` → `-12.50`.
fn decimal_text(v: &str) -> String {
    match v.split_once('.') {
        Some((int, frac)) if frac.bytes().all(|c| c.is_ascii_digit()) => {
            format!("{}.{frac}", integer_text(int))
        }
        None if !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit() || c == b'-') => {
            integer_text(v)
        }
        _ => v.to_string(),
    }
}

/// PostgreSQL's text of a double (the shortest digits that read back as
/// the same value: `12345678.5`, `1e-05`, `100`) in Java's
/// Double.toString form: plain with at least one fraction digit from
/// 10^-3 up to 10^7, else one digit, a point, the others and `E` with
/// the exponent (`1.23456785E7`, `1.0E-5`). Text only: the digits are
/// PostgreSQL's.
fn java_double(v: &str) -> String {
    let (neg, body) = match v.strip_prefix('-') {
        Some(b) => ("-", b),
        None => ("", v),
    };
    let (mant, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => match e.parse::<i32>() {
            Ok(e) => (m, e),
            Err(_) => return v.to_string(),
        },
        None => (body, 0),
    };
    let (ip, fp) = mant.split_once('.').unwrap_or((mant, ""));
    if !(ip.bytes().all(|c| c.is_ascii_digit()) && fp.bytes().all(|c| c.is_ascii_digit()))
        || ip.is_empty()
    {
        return v.to_string(); // NaN, Infinity
    }
    // All digits, and the position of the point after the first digit.
    let all = format!("{ip}{fp}");
    let lead = all.len() - all.trim_start_matches('0').len();
    let digits = all.trim_start_matches('0').trim_end_matches('0');
    if digits.is_empty() {
        return format!("{neg}0.0");
    }
    // value = 0.digits × 10^point
    let point = ip.len() as i32 - lead as i32 + exp;
    if (-2..=7).contains(&point) {
        let (i, f) = if point <= 0 {
            (
                "0".to_string(),
                format!("{}{digits}", "0".repeat((-point) as usize)),
            )
        } else if digits.len() as i32 <= point {
            (
                format!(
                    "{digits}{}",
                    "0".repeat((point - digits.len() as i32) as usize)
                ),
                String::new(),
            )
        } else {
            (
                digits[..point as usize].to_string(),
                digits[point as usize..].to_string(),
            )
        };
        format!("{neg}{i}.{}", if f.is_empty() { "0" } else { &f })
    } else {
        let rest = &digits[1..];
        format!(
            "{neg}{}.{}E{}",
            &digits[..1],
            if rest.is_empty() { "0" } else { rest },
            point - 1
        )
    }
}

/// An integer part without leading zeros (`-0012` → `-12`, `000` → `0`).
fn integer_text(v: &str) -> String {
    let (sign, digits) = match v.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", v),
    };
    let d = digits.trim_start_matches('0');
    format!("{sign}{}", if d.is_empty() { "0" } else { d })
}

/// A value of a column, as the reply carries it.
fn cell(t: Option<&ColType>, v: Option<&str>) -> J {
    let Some(v) = v else {
        return J::Null;
    };
    match t.map(|t| t.pg.as_str()).unwrap_or("text") {
        // A decimal with all its digits, scale kept (verify68 o68_mapepire
        // "sel": DECIMAL(7, 2) -12.50; verify69 p69_mapepire "num": 100.00,
        // 0.50).
        "numeric" => number(&decimal_text(v)),
        // DOUBLE as Java writes a double (verify69 "num": 12345678.5 as
        // 1.23456785E7, 0.00001 as 1.0E-5, 1.5 as 1.5).
        "float4" | "float8" => number(&java_double(v)),
        "int2" | "int4" | "int8" => number(v),
        "bool" => J::Bool(matches!(v, "t" | "true" | "TRUE")),
        // CHAR: without trailing blanks.
        "bpchar" => J::str(v.trim_end()),
        // Binary: the bytes read as UTF-8, a byte that is not UTF-8 as
        // U+FFFD (verify68 o68_mapepire "sel": BINARY(2) X'C1C2' came as
        // two U+FFFD).
        "bytea" => {
            let hex = v.trim_start_matches("\\x");
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
                .collect();
            J::str(String::from_utf8_lossy(&bytes).into_owned())
        }
        // Binary job log columns: each byte a character, as the server
        // gives them (verify64: MESSAGE_KEY "\u0000\u0000\u0001\r").
        "binary" | "varbit_fbd" => {
            let hex = v.trim_start_matches("\\x");
            J::str(
                (0..hex.len() / 2)
                    .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
                    .map(char::from)
                    .collect::<String>(),
            )
        }
        // TIMESTAMP: as JDBC writes it (yyyy-mm-dd hh:mm:ss.ffffff).
        // TIMESTAMP(0): no fraction (verify68 o68_mapepire "sysf":
        // LAST_ALTERED_TIMESTAMP "2026-11-09 20:17:56").
        "timestamp" if t.is_some_and(|t| t.typmod == 0) => {
            let s = jdbc_timestamp(v);
            J::str(s.split_once('.').map(|(a, _)| a.to_string()).unwrap_or(s))
        }
        "timestamp" => J::str(jdbc_timestamp(v)),
        // TIME: hh.mm.ss, the job's ISO time format (verify68 o68_mapepire
        // "sel": "13.14.15").
        "time" => J::str(v.replace(':', ".")),
        _ => J::str(v),
    }
}

/// A Db2 timestamp (`YYYY-MM-DD-HH.MM.SS.ffffff`) as JDBC's
/// `yyyy-mm-dd hh:mm:ss.ffffff`.
fn jdbc_timestamp(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 19 && b[10] == b'-' && b[13] == b'.' && b[16] == b'.' {
        format!("{} {}:{}:{}", &v[..10], &v[11..13], &v[14..16], &v[17..])
    } else {
        v.to_string()
    }
}

fn is_numeric_name(ty: &str) -> bool {
    matches!(
        ty,
        "INTEGER"
            | "SMALLINT"
            | "BIGINT"
            | "DECIMAL"
            | "NUMERIC"
            | "DOUBLE"
            | "REAL"
            | "FLOAT"
            | "DECFLOAT"
    )
}

/// A column's type name, precision, scale and display size (the JDBC
/// driver's: a decimal shows its digits, sign and point).
pub(crate) fn column_type(t: &ColType, longest: usize) -> (String, i64, i64, i64) {
    let m = t.typmod as i64;
    let l = longest as i64;
    match t.pg.as_str() {
        "int2" => ("SMALLINT".into(), 5, 0, 6),
        "int4" => ("INTEGER".into(), 10, 0, 11),
        "int8" => ("BIGINT".into(), 19, 0, 20),
        "float4" => ("REAL".into(), 24, 0, 13),
        "float8" => ("DOUBLE".into(), 53, 0, 22),
        // DECFLOAT(16) and (34) (verify69 p69_mapepire "num": DECFLOAT,
        // display 23, precision 16).
        "numeric" if t.db2.as_deref() == Some("DECFLOAT") => {
            ("DECFLOAT".into(), m, 0, if m > 16 { 42 } else { 23 })
        }
        // NUMERIC (zoned) and DECIMAL (packed) as the file has them
        // (verify68 o68_mapepire "sel": Z NUMERIC(5, 0), display 7).
        "numeric" if m >= 4 => {
            let p = (m - 4) >> 16;
            let s = (m - 4) & 0xFFFF;
            let name = if t.db2.as_deref() == Some("NUMERIC") {
                "NUMERIC"
            } else {
                "DECIMAL"
            };
            (name.into(), p, s, p + 2)
        }
        "numeric" => ("DECIMAL".into(), 31, 0, 33),
        // The job log's own columns (cl_job_log_rows): typmod is the length.
        "nvarchar" => ("NVARCHAR".into(), m, 0, m),
        "varbit_fbd" => ("VARCHAR () FOR BIT DATA".into(), m, 0, m),
        "binary" => ("BINARY".into(), m, 0, m),
        // GRAPHIC and VARGRAPHIC in Unicode as JDBC names them; CLOB
        // with its length (verify69 "oth").
        "bpchar" if m >= 4 && t.db2.as_deref() == Some("NCHAR") => {
            ("NCHAR".into(), m - 4, 0, m - 4)
        }
        "varchar" if m >= 4 && t.db2.as_deref() == Some("NVARCHAR") => {
            ("NVARCHAR".into(), m - 4, 0, m - 4)
        }
        "text" if m >= 4 && t.db2.as_deref() == Some("CLOB") => ("CLOB".into(), m - 4, 0, m - 4),
        "bpchar" if m >= 4 => ("CHAR".into(), m - 4, 0, m - 4),
        "bpchar" => ("CHAR".into(), l, 0, l),
        "varchar" if m >= 4 => ("VARCHAR".into(), m - 4, 0, m - 4),
        "date" => ("DATE".into(), 10, 0, 10),
        "time" => ("TIME".into(), 8, 0, 8),
        "timestamp" => ("TIMESTAMP".into(), 26, 6, 26),
        // BOOLEAN: display size 1 (verify69 "oth").
        "bool" => ("BOOLEAN".into(), 1, 0, 1),
        // BINARY(n) (verify68 o68_mapepire "sel": BINARY, display 2,
        // precision 2).
        "bytea" if t.db2.as_deref() == Some("BINARY") && m >= 4 => {
            ("BINARY".into(), m - 4, 0, m - 4)
        }
        "bytea" if m >= 4 => ("VARBINARY".into(), m - 4, 0, m - 4),
        "bytea" => ("VARBINARY".into(), l, 0, l * 2),
        "xml" => ("XML".into(), l, 0, l),
        _ => ("VARCHAR".into(), l, 0, l),
    }
}

/// A parameter type as written (`DECIMAL(9, 2)`): its name, precision
/// and scale.
fn written(ty: &str) -> (String, i64, i64) {
    let t = ty.trim().to_ascii_uppercase();
    let (base, args) = match t.split_once('(') {
        Some((b, r)) => (b.trim().to_string(), r.trim_end_matches(')').to_string()),
        None => (t.clone(), String::new()),
    };
    let nums: Vec<i64> = args
        .split(',')
        .filter_map(|x| x.trim().trim_end_matches(['K', 'M']).parse().ok())
        .collect();
    let n = |i: usize, d: i64| nums.get(i).copied().unwrap_or(d);
    let name = match base.as_str() {
        "INT" => "INTEGER".to_string(),
        "DEC" => "DECIMAL".to_string(),
        "CHARACTER" => "CHAR".to_string(),
        "CHARACTER VARYING" | "CHAR VARYING" => "VARCHAR".to_string(),
        other => other.to_string(),
    };
    let (p, s) = match name.as_str() {
        "INTEGER" => (10, 0),
        "SMALLINT" => (5, 0),
        "BIGINT" => (19, 0),
        "DECIMAL" | "NUMERIC" => (n(0, 5), n(1, 0)),
        "DATE" => (10, 0),
        "TIME" => (8, 0),
        "TIMESTAMP" => (26, 6),
        "CHAR" => (n(0, 1), 0),
        _ => (n(0, 0), 0),
    };
    (name, p, s)
}

/// Parameter values as SQL literals: null, numbers as they are, others
/// as character strings (as JDBC's setString passes them).
fn literals(vals: &[J]) -> Vec<String> {
    vals.iter()
        .map(|v| match v {
            J::Null => "NULL".to_string(),
            J::Num(n) => n.clone(),
            J::Bool(b) => format!("'{b}'"),
            J::Str(s) => format!("'{}'", s.replace('\'', "''")),
            other => format!("'{}'", other.to_string().replace('\'', "''")),
        })
        .collect()
}

/// The parameter markers (`?`) of a statement, outside strings,
/// delimited names and comments.
pub(crate) fn marker_positions(sql: &str) -> Vec<usize> {
    let b = sql.as_bytes();
    let mut at = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'?' => at.push(i),
            _ => {}
        }
        i += 1;
    }
    at
}

fn marker_count(sql: &str) -> usize {
    marker_positions(sql).len()
}

/// The statement with its markers replaced by the values (SQL0313 when
/// there are fewer values than markers).
pub(crate) fn bind(sql: &str, values: &[String]) -> Result<String> {
    let at = marker_positions(sql);
    if values.len() < at.len() {
        return Err(sql_failure(&anyhow!(
            "SQL0313: Number of host variables not valid."
        )));
    }
    let mut out = String::new();
    let mut last = 0;
    for (k, p) in at.iter().enumerate() {
        out.push_str(&sql[last..*p]);
        out.push_str(&values[k]);
        last = p + 1;
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

// ------------------------------------------------------------- serving

/// Serve one client on standard input and output (single mode) as
/// `user`: the job starts, each line is answered, the job ends at `exit`
/// or the end of the input.
pub async fn serve_stdio(
    client: &tokio_postgres::Client,
    env: &crate::commands::Environment,
    user: &str,
    libl: Vec<String>,
    legacy_cl: bool,
) -> Result<()> {
    serve(
        client,
        env,
        user,
        libl,
        legacy_cl,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}

/// Whether the server the client started (`-jar .../mapepire-server-
/// X.Y.Z.jar`) answers `cl` as servers before 2.5 do: yes when the
/// version is before 2.5 or not known.
pub fn legacy_cl_for<S: AsRef<str>>(words: &[S]) -> bool {
    for w in words {
        let name = w.as_ref().rsplit('/').next().unwrap_or("");
        if let Some(v) = name
            .strip_prefix("mapepire-server-")
            .and_then(|r| r.strip_suffix(".jar"))
        {
            let n: Vec<u32> = v.split('.').filter_map(|x| x.parse().ok()).collect();
            if n.len() == 3 {
                return (n[0], n[1]) < (2, 5);
            }
        }
    }
    true
}

/// Serve one client on a stream (standard input and output, an SSH
/// channel).
pub async fn serve<R, W>(
    client: &tokio_postgres::Client,
    env: &crate::commands::Environment,
    user: &str,
    libl: Vec<String>,
    legacy_cl: bool,
    input: R,
    mut out: W,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let profile = crate::users::get(client, user)
        .await?
        .ok_or_else(|| anyhow!("CPF2204: There is no user profile {user}."))?;
    let curlib = Some(profile.curlib.clone()).filter(|c| !c.is_empty() && c != "*CRTDFT");
    let id =
        crate::jobs::start_server_job(client, "QP0ZSPWT", user, curlib.as_deref(), &libl).await?;
    let job = Job {
        client,
        env,
        user: user.to_string(),
        curlib,
        libl,
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };
    job.name_session().await?;
    let key = job.job_key();
    let mut server = Server::new(job);
    server.legacy_cl = legacy_cl;
    let run = async {
        let mut lines = tokio::io::BufReader::new(input).lines();
        while let Some(line) = lines.next_line().await? {
            let reply = server.handle(&line).await;
            // AS4TAINR_MAPEPIRE_TRACE: a file each request and reply is
            // added to.
            if let Ok(path) = std::env::var("AS4TAINR_MAPEPIRE_TRACE") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "> {line}");
                    if let Some((r, _)) = &reply {
                        let _ = writeln!(f, "< {r}");
                    }
                }
            }
            if let Some((reply, exit)) = reply {
                out.write_all(reply.as_bytes()).await?;
                out.write_all(b"\n").await?;
                out.flush().await?;
                if exit {
                    break;
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    let r = crate::qtemp::scope(client, &key, user, run).await;
    crate::jobs::end(client, &id, r.is_ok()).await?;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_keeps_numbers() {
        let v =
            parse(r#"{"id":"1","n":12345678901234567890.123,"a":[1,null,true,"x\"yé"]}"#).unwrap();
        assert_eq!(v.get("n"), Some(&J::Num("12345678901234567890.123".into())));
        assert_eq!(
            v.to_string(),
            r#"{"id":"1","n":12345678901234567890.123,"a":[1,null,true,"x\"yé"]}"#
        );
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn markers_outside_strings_and_comments() {
        let sql = "SELECT '?', \"A?\" FROM T WHERE X = ? -- ?\n AND Y = ? /* ? */";
        assert_eq!(marker_count(sql), 2);
        assert_eq!(
            bind(sql, &["1".into(), "'b'".into()]).unwrap(),
            "SELECT '?', \"A?\" FROM T WHERE X = 1 -- ?\n AND Y = 'b' /* ? */"
        );
        assert!(bind("VALUES ?", &[]).is_err());
    }

    #[test]
    fn values_as_json() {
        let t = |pg: &str| ColType {
            pg: pg.into(),
            typmod: -1,
            db2: None,
        };
        // PUB400's 2.3.2 server wrote DECIMAL(7, 2) -12.50 with its scale
        // (verify68 o68_mapepire "sel"); JSON has no leading zeros.
        assert_eq!(
            cell(Some(&t("numeric")), Some("-0012.50")),
            J::Num("-12.50".into())
        );
        // Doubles as Java writes them (verify69 p69_mapepire "num").
        assert_eq!(
            cell(Some(&t("float8")), Some("12345678.5")),
            J::Num("1.23456785E7".into())
        );
        assert_eq!(
            cell(Some(&t("float8")), Some("1e-05")),
            J::Num("1.0E-5".into())
        );
        assert_eq!(cell(Some(&t("float8")), Some("1.5")), J::Num("1.5".into()));
        assert_eq!(
            cell(Some(&t("float8")), Some("100")),
            J::Num("100.0".into())
        );
        assert_eq!(
            cell(Some(&t("float8")), Some("0.001")),
            J::Num("0.001".into())
        );
        assert_eq!(
            cell(Some(&t("float8")), Some("-0.0005")),
            J::Num("-5.0E-4".into())
        );
        assert_eq!(
            cell(Some(&t("float8")), Some("1e+07")),
            J::Num("1.0E7".into())
        );
        assert_eq!(
            cell(Some(&t("float8")), Some("9999999")),
            J::Num("9999999.0".into())
        );
        assert_eq!(cell(Some(&t("numeric")), Some("7")), J::Num("7".into()));
        assert_eq!(cell(Some(&t("numeric")), Some(".5")), J::Num("0.5".into()));
        assert_eq!(cell(Some(&t("numeric")), Some("NaN")), J::str("NaN"));
        assert_eq!(cell(Some(&t("bpchar")), Some("AB  ")), J::str("AB"));
        assert_eq!(cell(Some(&t("varchar")), Some("AB  ")), J::str("AB  "));
        assert_eq!(cell(Some(&t("bool")), Some("t")), J::Bool(true));
        // Binary: the bytes as UTF-8 (verify68 "sel": X'C1C2' as two
        // U+FFFD).
        assert_eq!(cell(Some(&t("bytea")), Some("\\x4142")), J::str("AB"));
        assert_eq!(
            cell(Some(&t("bytea")), Some("\\xc1c2")),
            J::str("\u{fffd}\u{fffd}")
        );
        assert_eq!(
            cell(Some(&t("timestamp")), Some("2026-11-09-12.34.56.000001")),
            J::str("2026-11-09 12:34:56.000001")
        );
        assert_eq!(cell(Some(&t("int4")), None), J::Null);
    }

    #[test]
    fn server_version_from_the_jar_name() {
        assert!(legacy_cl_for(&[
            "-jar",
            "/home/ME/.mapepire/mapepire-server-2.3.6.jar"
        ]));
        assert!(!legacy_cl_for(&[
            "-jar",
            "mapepire-server-2.5.1.jar",
            "--single"
        ]));
        assert!(legacy_cl_for(&["--single"]));
    }

    #[test]
    fn rows_come_in_blocks() {
        let mut r = Results {
            columns: vec!["A".into()],
            types: vec![text_type()],
            rows: vec![vec![Some("1".to_string())], vec![Some("2".to_string())]].into(),
            done: false,
        };
        // Exactly the rows left: not yet known to be done.
        let (d, done) = block(&mut r, 2, false);
        assert_eq!(d.len(), 2);
        assert!(!done);
        assert_eq!(d[0].to_string(), r#"{"A":"1"}"#);
        let (d, done) = block(&mut r, 2, true);
        assert!(d.is_empty() && done);
    }
}
