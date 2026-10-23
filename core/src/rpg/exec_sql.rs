//! Embedded SQL (EXEC SQL) in RPG programs, written from IBM's public
//! "Embedded SQL programming" and DB2 for i SQL reference.
//!
//! Each statement is run through `crate::sql` on the program's own SQL
//! session (the restricted SQL login, the job's library list, autocommit).
//! Host variables (`:name`, `:ds.field`, with an indicator `:ind` or
//! `INDICATOR :ind`) are put into the statement as literals of their
//! current values: numbers as exact decimal text, character data quoted
//! with quotes doubled. Results go back into the host variables, and the
//! SQLCA (SQLCODE, SQLSTATE, SQLERRD) says how it went; an SQL error does
//! not end the program.
//!
//! A cursor's rows are read when it is opened (an insensitive cursor);
//! FETCH moves through them.

use super::*;
use crate::sql::{Exec, Session};

#[derive(Default)]
pub(super) struct SqlState {
    session: Option<Session>,
    cursors: HashMap<String, OpenCursor>,
    /// PREPARE: statement name → text (with ? markers).
    prepared: HashMap<String, String>,
    /// How the last statement other than GET DIAGNOSTICS ended.
    last: Option<Outcome>,
}

/// The cursors and prepared statements of a module that stays active (a
/// service program's module in its activation group, a program that ended
/// without LR): with CLOSQLCSR(*ENDACTGRP), the default, they stay open
/// until the activation group ends (Db2 for i SQL Programming, "Cursor
/// scope"), so a cursor opened by one procedure can be fetched by
/// another.
#[derive(Default)]
pub(super) struct KeptSql {
    cursors: HashMap<String, OpenCursor>,
    prepared: HashMap<String, String>,
}

type Key = (String, String, String);
static KEPT: std::sync::Mutex<Option<HashMap<Key, KeptSql>>> = std::sync::Mutex::new(None);

/// The activation group ends: its modules' cursors close.
pub fn end_activations(job: &str, groups: &crate::ile::Groups) {
    if let Ok(mut g) = KEPT.lock() {
        if let Some(m) = g.as_mut() {
            m.retain(|(j, grp, _), _| !(j == job && groups.covers(grp)));
        }
    }
}

impl SqlState {
    /// Keep the module's cursors for its next call (job, group, unit).
    pub(super) fn keep(&mut self, key: Key) {
        let k = KeptSql {
            cursors: std::mem::take(&mut self.cursors),
            prepared: std::mem::take(&mut self.prepared),
        };
        if k.cursors.is_empty() && k.prepared.is_empty() {
            if let Ok(mut g) = KEPT.lock() {
                if let Some(m) = g.as_mut() {
                    m.remove(&key);
                }
            }
            return;
        }
        if let Ok(mut g) = KEPT.lock() {
            g.get_or_insert_with(HashMap::new).insert(key, k);
        }
    }

    /// The module's cursors as its last call left them.
    pub(super) fn restore(&mut self, key: &Key) {
        let k = KEPT
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|m| m.remove(key)));
        if let Some(k) = k {
            self.cursors = k.cursors;
            self.prepared = k.prepared;
        }
    }
}

struct OpenCursor {
    rows: Vec<Vec<Option<String>>>,
    /// The current row: -1 before the first, rows.len() after the last.
    pos: isize,
}

/// How a statement ended: SQLCODE, SQLSTATE, rows (SQLERRD(3)), message.
#[derive(Clone)]
struct Outcome {
    code: i64,
    state: String,
    rows: i64,
    text: String,
}

impl Outcome {
    fn ok(rows: i64) -> Outcome {
        Outcome {
            code: 0,
            state: "00000".into(),
            rows,
            text: String::new(),
        }
    }
    fn not_found() -> Outcome {
        Outcome {
            code: 100,
            state: "02000".into(),
            rows: 0,
            text: String::new(),
        }
    }
    fn error(code: i64, text: impl Into<String>) -> Outcome {
        Outcome {
            code,
            state: sqlstate(code).into(),
            rows: 0,
            text: text.into(),
        }
    }
}

/// SQLSTATE for an SQLCODE, as DB2 for i reports them.
fn sqlstate(code: i64) -> &'static str {
    match code {
        0 => "00000",
        100 => "02000",
        -84 => "42612",
        -104 => "42601",
        -204 => "42704",
        -206 => "42703",
        -302 => "22003",
        -305 => "22002",
        -501 => "24501",
        -502 => "24502",
        -504 => "34000",
        -518 => "07003",
        -551 => "42501",
        -803 => "23505",
        -407 => "23502",
        -545 => "23513",
        -601 => "42710",
        -811 => "21000",
        _ => "58004",
    }
}

/// An error from the SQL layer ("SQL0204: ...") as SQLCODE and text.
fn sql_error(e: &anyhow::Error) -> Option<Outcome> {
    let m = format!("{e:#}");
    let digits: String = m.strip_prefix("SQL")?.chars().take(4).collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(Outcome::error(-n, m))
}

/// The statement's words, upper case (for recognizing its kind).
fn words(sql: &str) -> Vec<String> {
    sql.split_whitespace()
        .map(|w| w.to_ascii_uppercase())
        .collect()
}

/// Find a keyword at nesting depth 0, outside literals: byte offset.
fn find_word(sql: &str, word: &str) -> Option<usize> {
    let cs: Vec<(usize, char)> = sql.char_indices().collect();
    let (mut depth, mut quote) = (0i32, None::<char>);
    let up = word.to_ascii_uppercase();
    let is_word = |c: char| c.is_ascii_alphanumeric() || "_#@$".contains(c);
    let mut i = 0;
    while i < cs.len() {
        let (at, c) = cs[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ if depth == 0
                    && is_word(c)
                    && (i == 0 || !is_word(cs[i - 1].1))
                    && sql[at..].len() >= up.len()
                    && sql[at..at + up.len()].eq_ignore_ascii_case(&up)
                    && sql[at + up.len()..]
                        .chars()
                        .next()
                        .is_none_or(|n| !is_word(n)) =>
                {
                    return Some(at);
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// `:a :ai, :b INDICATOR :bi, :c` → [(a, Some(ai)), (b, Some(bi)), (c, None)].
fn host_list(text: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let names: Vec<&str> = p
            .split(|c: char| c.is_whitespace() || c == ':')
            .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("INDICATOR"))
            .collect();
        match names.as_slice() {
            [v] => out.push((v.to_ascii_uppercase(), None)),
            [v, i] => out.push((v.to_ascii_uppercase(), Some(i.to_ascii_uppercase()))),
            _ => bail!("SQL0104: host variables not valid: {p}"),
        }
    }
    Ok(out)
}

/// A value as an SQL literal.
fn literal(v: &Value) -> Result<String> {
    Ok(match v {
        Value::Num(d) => d.to_string(),
        Value::Ind(b) => if *b { "'1'" } else { "'0'" }.to_string(),
        Value::Fig(_) => "''".to_string(),
        Value::Char(c) => format!("'{}'", a(c).replace('\0', "").replace('\'', "''")),
        other => anyhow::bail!("{} cannot be an SQL value", other.describe()),
    })
}

/// Replace `?` parameter markers (outside literals) with values.
fn fill_markers(sql: &str, values: &[String]) -> Result<String> {
    let mut out = String::new();
    let mut quote = None;
    let mut n = 0;
    for c in sql.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                out.push(c);
            }
            Some(_) => out.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push(c);
            }
            None if c == '?' => {
                let v = values.get(n).ok_or_else(|| {
                    anyhow!("SQL0313: Not enough values for the parameter markers.")
                })?;
                out.push_str(v);
                n += 1;
            }
            None => out.push(c),
        }
    }
    Ok(out)
}

impl Machine<'_> {
    /// EXEC SQL: run the statement, then set the SQLCA.
    pub(super) fn exec_sql(&mut self, text: &str) -> Result<()> {
        let w = words(text);
        let diagnostics = w.first().map(String::as_str) == Some("GET")
            && w.iter().take(3).any(|x| x == "DIAGNOSTICS");
        let out = match if diagnostics {
            self.get_diagnostics(text.trim())
        } else {
            self.sql_statement(text.trim())
        } {
            Ok(o) => o,
            Err(e) => match sql_error(&e) {
                Some(o) => o,
                None => return Err(e),
            },
        };
        if !diagnostics {
            self.sql.last = Some(out.clone());
        }
        let set = |m: &mut Self, name: &str, v: Value| {
            m.assign(
                &Expr::name(name),
                &v,
                Fit::Truncate { half_adjust: false },
                false,
            )
        };
        set(self, "SQLCODE", Value::Num(Dec::from_i64(out.code)))?;
        set(self, "SQLSTATE", Value::str(&out.state))?;
        set(self, "SQLERRMC", Value::str(&out.text))?;
        let errd3 = Expr::Name {
            parts: vec!["SQLERRD".into()],
            args: Some(vec![Expr::Num(Dec::from_i64(3))]),
        };
        self.assign(
            &errd3,
            &Value::Num(Dec::from_i64(out.rows)),
            Fit::Truncate { half_adjust: false },
            false,
        )?;
        Ok(())
    }

    /// GET [CURRENT] DIAGNOSTICS (Db2 for i SQL Reference): about the
    /// last statement, :hv = ROW_COUNT; or CONDITION n (EXCEPTION n) with
    /// :hv = MESSAGE_TEXT, MESSAGE_LENGTH, RETURNED_SQLSTATE,
    /// DB2_RETURNED_SQLCODE, MESSAGE_ID or DB2_MESSAGE_ID. It does not
    /// change what the next GET DIAGNOSTICS sees.
    fn get_diagnostics(&mut self, sql: &str) -> Result<Outcome> {
        let last = self.sql.last.clone().unwrap_or_else(|| Outcome::ok(0));
        let up = sql.to_ascii_uppercase();
        let at = up.find("DIAGNOSTICS").unwrap() + "DIAGNOSTICS".len();
        let mut rest = sql[at..].trim().trim_end_matches(';').trim();
        let first = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let condition = matches!(first.as_str(), "CONDITION" | "EXCEPTION");
        if condition {
            // The condition number: only one condition is kept.
            let mut it = rest.splitn(3, char::is_whitespace);
            it.next();
            it.next();
            rest = it.next().unwrap_or("").trim();
        }
        let id = last
            .text
            .split(':')
            .next()
            .filter(|p| p.len() == 7 && p.starts_with("SQL"))
            .unwrap_or("")
            .to_string();
        let text = if id.is_empty() {
            last.text.clone()
        } else {
            last.text[id.len() + 1..].trim().to_string()
        };
        for item in rest.split(',') {
            let Some((target, name)) = item.split_once('=') else {
                return Ok(Outcome::error(
                    -104,
                    format!("SQL0104: GET DIAGNOSTICS item {item} is not valid."),
                ));
            };
            let target = target.trim().trim_start_matches(':');
            let v = match (condition, name.trim().to_ascii_uppercase().as_str()) {
                (false, "ROW_COUNT") => Value::Num(Dec::from_i64(last.rows)),
                (true, "MESSAGE_TEXT") => Value::str(&text),
                (true, "MESSAGE_LENGTH") => Value::Num(Dec::from_i64(text.chars().count() as i64)),
                (true, "RETURNED_SQLSTATE") => Value::str(&last.state),
                (true, "DB2_RETURNED_SQLCODE") => Value::Num(Dec::from_i64(last.code)),
                (true, "MESSAGE_ID" | "DB2_MESSAGE_ID") => Value::str(&id),
                (_, other) => {
                    return Ok(Outcome::error(
                        -104,
                        format!("SQL0104: GET DIAGNOSTICS item {other} is not supported."),
                    ))
                }
            };
            for t in self.host_targets(target)? {
                self.assign(&t, &v, Fit::Truncate { half_adjust: false }, false)?;
            }
        }
        Ok(Outcome::ok(0))
    }

    fn session(&mut self) -> Result<&Session> {
        if self.sql.session.is_none() {
            // A commit level other than *NONE: the statements run under
            // the commitment definition of the program's activation group
            // or job, which SQL starts (at the activation group) when
            // there is none.
            let level = self.prog.sql_commit.clone();
            let s = if level == "*NONE" {
                self.block(Session::open(
                    self.io.client,
                    &self.io.database_url,
                    &self.io.libl,
                ))?
            } else {
                let d = match crate::cmtctl::find(&self.io.job_key, &self.io.actgrp) {
                    Some(d) => d,
                    None => self.block(crate::cmtctl::start(
                        self.io.client,
                        &self.io.database_url,
                        &self.io.job_key,
                        &self.io.actgrp,
                        &level,
                        false,
                    ))?,
                };
                self.block(Session::open_in(
                    self.io.client,
                    &self.io.database_url,
                    &self.io.libl,
                    d,
                ))?
            };
            let mut s = s;
            let job = self.io.job.as_ref().map(|j| j.to_string());
            let user = self.io.user.clone();
            // The program does not know which entry is the current
            // library.
            self.block(s.set_context(&user, None, job))?;
            // The program (journal entries name it): LIB/NAME.
            if let Some((l, n)) = self.io.program.split_once('/') {
                let (l, n) = (l.to_string(), n.to_string());
                self.block(s.set_program(&n, &l))?;
            }
            self.sql.session = Some(s);
        }
        Ok(self.sql.session.as_ref().unwrap())
    }

    fn run_sql(&mut self, sql: &str) -> Result<Exec> {
        let client = self.io.client;
        let rt = self.io.rt.clone();
        let s = self.session()?;
        rt.block_on(s.exec(client, sql))
    }

    /// The value of a host variable as a literal (`:ds` is its subfields,
    /// separated by commas); NULL when its indicator is negative.
    fn host_literal(&mut self, name: &str, ind: Option<&str>) -> Result<String> {
        if let Some(i) = ind {
            let iv = self.eval(&crate::rpg::expr::parse(i)?)?.num()?;
            if iv.is_negative() {
                return Ok("NULL".into());
            }
        }
        if let Some((len, data)) = self.lob(name) {
            let n = self.eval(&len)?.num()?.to_i128()?.max(0) as usize;
            let bytes = self.eval(&data)?.chars()?;
            return literal(&Value::Char(bytes[..n.min(bytes.len())].to_vec()));
        }
        let targets = self.host_targets(name)?;
        let mut parts = Vec::new();
        for t in targets {
            let v = self.eval(&t)?;
            // A fixed-length character host variable is a CHAR(n) value
            // (Db2 for i SQL Programming, "Host variables in ILE RPG"):
            // compared with blanks added to the shorter string, so a
            // blank CHAR(5) equals ' '.
            match (self.type_of(&t), &v) {
                (Some(super::data::DType::Char(n)), Value::Char(_))
                    if (1..=10_485_760).contains(&n) =>
                {
                    parts.push(format!("CAST({} AS char({n}))", literal(&v)?))
                }
                _ => parts.push(literal(&v)?),
            }
        }
        Ok(parts.join(", "))
    }

    /// A LOB host variable (SQLTYPE(CLOB:n)): its _LEN and _DATA fields.
    fn lob(&self, name: &str) -> Option<(Expr, Expr)> {
        let up = name.to_ascii_uppercase();
        match self.sym(&up) {
            Some(Sym::Ds(d)) if d.subfields == [format!("{up}_LEN"), format!("{up}_DATA")] => {
                Some((
                    Expr::name(&format!("{up}_LEN")),
                    Expr::name(&format!("{up}_DATA")),
                ))
            }
            _ => None,
        }
    }

    /// A host variable as targets: a data structure is its subfields.
    fn host_targets(&mut self, name: &str) -> Result<Vec<Expr>> {
        let e = crate::rpg::expr::parse(name)
            .map_err(|_| anyhow!("SQL0312: host variable {name} is not valid"))?;
        if let Expr::Name { parts, args: None } = &e {
            let full = parts.join(".");
            if let Some(Sym::Ds(d)) = self.sym(&full) {
                return Ok(d
                    .subfields
                    .iter()
                    .map(|sf| {
                        if d.qualified {
                            Expr::Name {
                                parts: vec![full.clone(), sf.clone()],
                                args: None,
                            }
                        } else {
                            Expr::name(sf)
                        }
                    })
                    .collect());
            }
            if self.sym(&full).is_none() {
                bail!("SQL0312: host variable {name} is not defined");
            }
        }
        Ok(vec![e])
    }

    /// Put the host variables into a statement as literals.
    fn bind_host(&mut self, sql: &str) -> Result<String> {
        let cs: Vec<char> = sql.chars().collect();
        let mut out = String::new();
        let mut quote = None;
        let mut i = 0;
        let name_char = |c: char| c.is_ascii_alphanumeric() || "_#@$.".contains(c);
        let read_name = |i: &mut usize| -> String {
            let start = *i;
            while *i < cs.len() && name_char(cs[*i]) {
                *i += 1;
            }
            cs[start..*i].iter().collect()
        };
        while i < cs.len() {
            let c = cs[i];
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                out.push(c);
                i += 1;
                continue;
            }
            if c == '\'' || c == '"' {
                quote = Some(c);
                out.push(c);
                i += 1;
                continue;
            }
            if c == ':'
                && cs
                    .get(i + 1)
                    .is_some_and(|n| n.is_ascii_alphabetic() || "_#@$".contains(*n))
            {
                i += 1;
                let name = read_name(&mut i);
                // An indicator variable: `:ind` or `INDICATOR :ind` next.
                let mut j = i;
                while j < cs.len() && cs[j].is_whitespace() {
                    j += 1;
                }
                let rest: String = cs[j..].iter().collect();
                let mut ind = None;
                if rest.starts_with(':') {
                    let mut k = j + 1;
                    ind = Some(read_name(&mut k));
                    i = k;
                } else if rest.len() > 10 && rest[..9].eq_ignore_ascii_case("INDICATOR") {
                    let r2 = rest[9..].trim_start();
                    if let Some(after) = r2.strip_prefix(':') {
                        let skip = rest.len() - after.len();
                        let mut k = j + skip;
                        ind = Some(read_name(&mut k));
                        i = k;
                    }
                }
                out.push_str(&self.host_literal(&name, ind.as_deref())?);
                continue;
            }
            out.push(c);
            i += 1;
        }
        Ok(out)
    }

    /// Values of a row into host variables (and their indicators).
    fn assign_hosts(
        &mut self,
        hosts: &[(String, Option<String>)],
        row: &[Option<String>],
    ) -> Result<Option<Outcome>> {
        let mut values = row.iter();
        for (name, ind) in hosts {
            if let Some((len, data)) = self.lob(name) {
                let Some(v) = values.next() else {
                    return Ok(None);
                };
                let text = v.clone().unwrap_or_default();
                let bytes = super::data::e(&text);
                self.assign(
                    &len,
                    &Value::Num(Dec::from_i64(bytes.len() as i64)),
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
                self.assign(
                    &data,
                    &Value::Char(bytes),
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
                continue;
            }
            for t in self.host_targets(name)? {
                let Some(v) = values.next() else {
                    return Ok(None);
                };
                match v {
                    None => match ind {
                        Some(i) => self.assign(
                            &crate::rpg::expr::parse(i)?,
                            &Value::Num(Dec::from_i64(-1)),
                            Fit::Truncate { half_adjust: false },
                            false,
                        )?,
                        None => {
                            return Ok(Some(Outcome::error(
                                -305,
                                "SQL0305: A null value has no indicator variable.",
                            )))
                        }
                    },
                    Some(text) => {
                        if let Some(i) = ind {
                            self.assign(
                                &crate::rpg::expr::parse(i)?,
                                &Value::Num(Dec::ZERO),
                                Fit::Truncate { half_adjust: false },
                                false,
                            )?;
                        }
                        let numeric = self.type_of(&t).is_some_and(|t| t.is_numeric());
                        let val = if numeric {
                            match Dec::parse(text.trim()) {
                                Ok(d) => Value::Num(d),
                                Err(_) => {
                                    return Ok(Some(Outcome::error(
                                        -302,
                                        format!(
                                        "SQL0302: '{text}' does not fit a numeric host variable."
                                    ),
                                    )))
                                }
                            }
                        } else {
                            match self.type_of(&t) {
                                Some(DType::Timestamp) => Value::str(&db2_timestamp(text)),
                                Some(DType::Time(_)) => Value::str(&text.replace(':', ".")),
                                _ => Value::str(text),
                            }
                        };
                        if let Err(e) =
                            self.assign(&t, &val, Fit::Strict { half_adjust: false }, false)
                        {
                            return Ok(Some(Outcome::error(-302, format!("SQL0302: {e}"))));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn sql_statement(&mut self, sql: &str) -> Result<Outcome> {
        let w = words(sql);
        let first = w.first().map(String::as_str).unwrap_or("");
        let second = w.get(1).map(String::as_str).unwrap_or("");
        match first {
            // Precompiler directives and declarations.
            "SET" if second == "OPTION" => return Ok(Outcome::ok(0)),
            "INCLUDE" | "WHENEVER" | "BEGIN" | "END" => return Ok(Outcome::ok(0)),
            "DECLARE"
                if w.get(2).map(String::as_str) == Some("CURSOR")
                    || w.iter().any(|x| x == "CURSOR") =>
            {
                return Ok(Outcome::ok(0))
            }
            // COMMIT and ROLLBACK: the unit of work of the commitment
            // definition the program uses (with COMMIT(*NONE) each
            // statement was committed when it ended: nothing to do).
            "COMMIT" | "ROLLBACK" => {
                let d = match self.sql.session.as_ref().and_then(|s| s.commitment()) {
                    Some(d) => Some(d.clone()),
                    None => crate::cmtctl::find(&self.io.job_key, &self.io.actgrp),
                };
                if let Some(d) = d {
                    let r = if first == "COMMIT" {
                        self.block(d.commit())
                    } else {
                        self.block(d.rollback())
                    };
                    if let Err(e) = r {
                        return Ok(Outcome::error(-901, format!("SQL0901: {e}")));
                    }
                }
                return Ok(Outcome::ok(0));
            }
            "OPEN" => return self.sql_open(sql),
            "FETCH" => return self.sql_fetch(sql),
            "CLOSE" => {
                let name = second.trim_end_matches(';').to_string();
                return Ok(match self.sql.cursors.remove(&name) {
                    Some(_) => Outcome::ok(0),
                    None => Outcome::error(-501, format!("SQL0501: Cursor {name} is not open.")),
                });
            }
            "PREPARE" => {
                let name = second.to_string();
                let at =
                    find_word(sql, "FROM").ok_or_else(|| anyhow!("SQL0104: PREPARE needs FROM"))?;
                let text = self.statement_text(sql[at + 4..].trim())?;
                self.sql.prepared.insert(name, text);
                return Ok(Outcome::ok(0));
            }
            "EXECUTE" if second == "IMMEDIATE" => {
                let at = find_word(sql, "IMMEDIATE").unwrap();
                let text = self.statement_text(sql[at + 9..].trim())?;
                return self.sql_change(&text);
            }
            "EXECUTE" => {
                let text = self
                    .sql
                    .prepared
                    .get(second)
                    .cloned()
                    .ok_or_else(|| anyhow!("SQL0518: Statement {second} was not prepared."))?;
                let values = self.using_values(sql)?;
                let text = fill_markers(&text, &values)?;
                return self.sql_change(&text);
            }
            "SET" if second.starts_with(':') => {
                // SET :host = expression.
                let eq = sql
                    .find('=')
                    .ok_or_else(|| anyhow!("SQL0104: SET needs ="))?;
                let hosts = host_list(&sql[3..eq])?;
                let query = format!("VALUES ({})", self.bind_host(sql[eq + 1..].trim())?);
                return self.single_row(&query, &hosts);
            }
            "SELECT" | "VALUES" | "WITH" => {
                if let Some(into) = find_word(sql, "INTO") {
                    let end = find_word(&sql[into..], "FROM")
                        .map(|e| into + e)
                        .unwrap_or(sql.len());
                    let hosts = host_list(&sql[into + 4..end])?;
                    let query = format!("{} {}", &sql[..into], &sql[end..]);
                    let query = self.bind_host(&query)?;
                    return self.single_row(&query, &hosts);
                }
            }
            "CREATE" | "DROP" | "LABEL" | "COMMENT" => {
                // Data definition runs as it does in RUNSQLSTM; the
                // statement has no host variables.
                let curlib = self.io.cl.as_ref().and_then(|c| c.curlib.clone());
                let rt = self.io.rt.clone();
                let r = rt.block_on(crate::ddl::try_run(
                    &self.io.database_url,
                    &self.io.user,
                    &self.io.libl,
                    curlib,
                    &mut None,
                    sql,
                ))?;
                if r.is_some() {
                    return Ok(Outcome::ok(0));
                }
            }
            "CALL" => return self.sql_call(sql),
            "GET" | "ALTER" | "GRANT" | "REVOKE" | "RENAME" | "LOCK" | "MERGE" => {
                return Ok(Outcome::error(
                    -84,
                    format!("SQL0084: {first} statements are not supported in embedded SQL yet."),
                ))
            }
            _ => {}
        }
        if find_word(sql, "CURRENT").is_some()
            && w.windows(2).any(|p| p[0] == "CURRENT" && p[1] == "OF")
        {
            return Ok(Outcome::error(
                -84,
                "SQL0084: WHERE CURRENT OF (positioned UPDATE and DELETE) is not supported yet.",
            ));
        }
        let text = self.bind_host(sql)?;
        self.sql_change(&text)
    }

    /// The statement of PREPARE / EXECUTE IMMEDIATE: a host variable or
    /// a literal.
    fn statement_text(&mut self, from: &str) -> Result<String> {
        let from = from.trim();
        if let Some(name) = from.strip_prefix(':') {
            let v = self.eval(&crate::rpg::expr::parse(name.trim())?)?;
            Ok(a(&v.chars()?).trim().to_string())
        } else {
            let v = self.eval(&crate::rpg::expr::parse(from)?)?;
            Ok(a(&v.chars()?).trim().to_string())
        }
    }

    /// USING :a, :b → literals.
    fn using_values(&mut self, sql: &str) -> Result<Vec<String>> {
        let Some(at) = find_word(sql, "USING") else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for (name, ind) in host_list(&sql[at + 5..])? {
            out.push(self.host_literal(&name, ind.as_deref())?);
        }
        Ok(out)
    }

    /// INSERT, UPDATE, DELETE (or a query run for nothing).
    fn sql_change(&mut self, text: &str) -> Result<Outcome> {
        Ok(match self.run_sql(text)? {
            // A searched UPDATE or DELETE, or an INSERT from a SELECT,
            // that changes no rows: SQLCODE +100.
            Exec::Count(_, 0) => Outcome::not_found(),
            Exec::Count(_, n) => Outcome::ok(n as i64),
            Exec::Rows { rows, .. } => Outcome::ok(rows.len() as i64),
        })
    }

    /// SELECT INTO, VALUES INTO, SET: exactly one row.
    /// CALL of an SQL procedure: arguments are host variables (OUT and
    /// INOUT parameters come back into them) or expressions.
    fn sql_call(&mut self, sql: &str) -> Result<Outcome> {
        let Some((name, args)) = crate::sqlpl::parse_call(sql)? else {
            return Ok(Outcome::error(-104, "SQL0104: CALL statement not valid."));
        };
        let mut exprs = Vec::new();
        let mut targets: Vec<Option<(String, Option<String>)>> = Vec::new();
        for a in &args {
            let t = a.trim();
            if let Some(rest) = t.strip_prefix(':') {
                // :hv, :hv :ind, :hv INDICATOR :ind
                let parts: Vec<&str> = rest
                    .split_whitespace()
                    .filter(|w| !w.eq_ignore_ascii_case("INDICATOR"))
                    .collect();
                let hv = parts.first().copied().unwrap_or("").to_string();
                let ind = parts.get(1).map(|i| i.trim_start_matches(':').to_string());
                exprs.push(self.host_literal(&hv, ind.as_deref())?);
                targets.push(Some((hv, ind)));
            } else {
                exprs.push(t.to_string());
                targets.push(None);
            }
        }
        let curlib = self.io.cl.as_ref().and_then(|c| c.curlib.clone());
        let client = self.io.client;
        let url = self.io.database_url.clone();
        let libl = self.io.libl.clone();
        let user = self.io.user.clone();
        let rt = self.io.rt.clone();
        self.session()?;
        let session = self.sql.session.as_ref().expect("opened above");
        let env = crate::sqlpl::Env {
            session,
            catalog: client,
            database_url: &url,
            libl: &libl,
            user: &user,
            curlib,
        };
        let r = rt.block_on(crate::sqlpl::call_exprs(&env, &name, &exprs));
        let (routine, out) = match r {
            Ok(x) => x,
            Err(e) => {
                let (code, state, text) = crate::sqlpl::condition_of(&e);
                let mut o = Outcome::error(code, text);
                if code == -438 {
                    o.state = state;
                }
                return Ok(o);
            }
        };
        for ((p, t), v) in routine.parms.iter().zip(&targets).zip(&out.values) {
            if p.mode == "IN" {
                continue;
            }
            if let Some((hv, ind)) = t {
                if let Some(o) =
                    self.assign_hosts(&[(hv.clone(), ind.clone())], std::slice::from_ref(v))?
                {
                    return Ok(o);
                }
            }
        }
        Ok(Outcome::ok(0))
    }

    fn single_row(&mut self, query: &str, hosts: &[(String, Option<String>)]) -> Result<Outcome> {
        let Exec::Rows { rows, .. } = self.run_sql(query)? else {
            bail!("SQL0104: INTO needs a query");
        };
        match rows.len() {
            0 => Ok(Outcome::not_found()),
            1 => Ok(self
                .assign_hosts(hosts, &rows[0])?
                .unwrap_or(Outcome::ok(1))),
            _ => {
                // The first row's values are still assigned (DB2 for i
                // does so too), and SQLCODE -811 says there were more.
                self.assign_hosts(hosts, &rows[0])?;
                Ok(Outcome::error(
                    -811,
                    "SQL0811: The result of SELECT INTO has more than one row.",
                ))
            }
        }
    }

    fn sql_open(&mut self, sql: &str) -> Result<Outcome> {
        let w = words(sql);
        let name = w.get(1).cloned().unwrap_or_default();
        if self.sql.cursors.contains_key(&name) {
            return Ok(Outcome::error(
                -502,
                format!("SQL0502: Cursor {name} is already open."),
            ));
        }
        let query = match self.prog.cursors.get(&name).cloned() {
            Some(crate::rpg::program::Cursor::Select(q)) => self.bind_host(&q)?,
            Some(crate::rpg::program::Cursor::Prepared(s)) => {
                let text = self
                    .sql
                    .prepared
                    .get(&s)
                    .cloned()
                    .ok_or_else(|| anyhow!("SQL0518: Statement {s} was not prepared."))?;
                let values = self.using_values(sql)?;
                fill_markers(&text, &values)?
            }
            None => {
                return Ok(Outcome::error(
                    -504,
                    format!("SQL0504: Cursor {name} is not declared."),
                ))
            }
        };
        let rows = match self.run_sql(&query)? {
            Exec::Rows { rows, .. } => rows,
            Exec::Count(..) => bail!("SQL0104: cursor {name} is not a query"),
        };
        self.sql.cursors.insert(name, OpenCursor { rows, pos: -1 });
        Ok(Outcome::ok(0))
    }

    fn sql_fetch(&mut self, sql: &str) -> Result<Outcome> {
        let into = find_word(sql, "INTO");
        let head = &sql[..into.unwrap_or(sql.len())];
        let w = words(head);
        // FETCH [orientation] [FROM] cursor
        let mut k = 1;
        let mut orient = ("NEXT".to_string(), 0i64);
        if let Some(o) = w.get(k) {
            match o.as_str() {
                "NEXT" | "PRIOR" | "FIRST" | "LAST" | "BEFORE" | "AFTER" | "CURRENT" => {
                    orient = (o.clone(), 0);
                    k += 1;
                }
                "RELATIVE" | "ABSOLUTE" => {
                    let n = w.get(k + 1).and_then(|x| x.parse().ok()).unwrap_or(0);
                    orient = (o.clone(), n);
                    k += 2;
                }
                _ => {}
            }
        }
        if w.get(k).map(String::as_str) == Some("FROM") {
            k += 1;
        }
        let name = w.get(k).cloned().unwrap_or_default();
        if w.iter().any(|x| x == "ROWS") {
            return Ok(Outcome::error(
                -84,
                "SQL0084: multiple-row FETCH (FOR n ROWS) is not supported yet.",
            ));
        }
        let Some(c) = self.sql.cursors.get_mut(&name) else {
            return Ok(Outcome::error(
                -501,
                format!("SQL0501: Cursor {name} is not open."),
            ));
        };
        let len = c.rows.len() as isize;
        c.pos = match orient.0.as_str() {
            "NEXT" => c.pos + 1,
            "PRIOR" => c.pos - 1,
            "FIRST" => 0,
            "LAST" => len - 1,
            "BEFORE" => -1,
            "AFTER" => len,
            "CURRENT" => c.pos,
            "RELATIVE" => c.pos + orient.1 as isize,
            _ => orient.1 as isize - 1,
        }
        .clamp(-1, len);
        if orient.0 == "BEFORE" || orient.0 == "AFTER" {
            return Ok(Outcome::ok(0));
        }
        if c.pos < 0 || c.pos >= len {
            return Ok(Outcome::not_found());
        }
        let row = c.rows[c.pos as usize].clone();
        let Some(into) = into else {
            return Ok(Outcome::ok(1));
        };
        let hosts = host_list(&sql[into + 4..])?;
        Ok(self.assign_hosts(&hosts, &row)?.unwrap_or(Outcome::ok(1)))
    }
}

/// A timestamp as PostgreSQL gives it ('2026-10-23 13:38:38.84'), in the
/// form an RPG timestamp holds ('2026-10-23-13.38.38.840000'); other
/// text as it is.
fn db2_timestamp(t: &str) -> String {
    let t = t.trim();
    let Some((date, time)) = t.split_once(' ') else {
        return t.to_string();
    };
    let time = time.split(['+', '-', 'Z']).next().unwrap_or(time);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut f = frac.to_string();
    f.truncate(6);
    format!("{date}-{}.{f:0<6}", hms.replace(':', "."))
}
