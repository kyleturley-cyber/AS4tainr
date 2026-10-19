//! Record-level access (RLA): the RPG file operations `SETLL`, `SETGT`,
//! `CHAIN`, `READ`, `READP`, `READE`, `READPE`, `WRITE`, `UPDATE`, `DELETE`
//! implemented over a PostgreSQL table.
//!
//! An open file keeps a cursor position, like an IBM i open data path. The
//! position is not a database cursor: each read is a small keyset query
//! ("the next row after this key"), so many files can be open per job
//! without holding transactions open.
//!
//! Access path order is (key fields..., rrn), so records with duplicate
//! keys come back in arrival order, as on IBM i. Unkeyed files are read in
//! arrival (rrn) order.

use crate::catalog::{ident, FileRef};
use crate::dds::{DataType, FieldDef};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Row};

/// Record values as strings keyed by field name, matching what Jariko's
/// reload layer exchanges. Character fields keep their trailing blanks.
pub type Record = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
enum Position {
    /// Before the first record (file just opened, or `SETLL *LOVAL`).
    Start,
    /// After the last record (`SETGT *HIVAL`, or read past the end).
    End,
    /// `SETLL key`: before the first record whose key >= key.
    BeforeKey(Vec<String>),
    /// `SETGT key`: after the last record whose key <= key.
    AfterKey(Vec<String>),
    /// On a specific record (after `CHAIN` or a successful read).
    AtRecord { keys: Vec<String>, rrn: i64 },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReadResult {
    /// %FOUND for CHAIN; for reads, true when a record was returned.
    pub found: bool,
    /// %EOF after READ/READE (or beginning-of-file after READP/READPE).
    pub eof: bool,
    pub record: Option<Record>,
    pub rrn: Option<i64>,
}

impl ReadResult {
    fn none() -> Self {
        ReadResult {
            found: false,
            eof: true,
            record: None,
            rrn: None,
        }
    }
}

pub struct OpenFile {
    pub file: FileRef,
    pos: Position,
    /// The key used by the last SETLL/SETGT/CHAIN, for READE/READPE with no
    /// search argument.
    last_key: Option<Vec<String>>,
    /// The record the next UPDATE/DELETE acts on.
    current_rrn: Option<i64>,
    eof: bool,
    equal: bool,
    /// Opened for update (RPG file type U): reads lock the record found.
    pub update: bool,
    /// The record this open file holds locked, if any.
    locked: Option<i64>,
    /// What locks are taken on: the physical file (`LIB/FILE`), also when
    /// this is a logical file over it.
    lock_name: String,
}

/// Advisory lock class for record locks (a hash of the physical file
/// name is the first key, the relative record number the second).
const RECORD_LOCK_SQL: &str = "SELECT pg_advisory_lock(hashtext($1), ($2::bigint)::int)";
const RECORD_TRY_LOCK_SQL: &str = "SELECT pg_try_advisory_lock(hashtext($1), ($2::bigint)::int)";
const RECORD_UNLOCK_SQL: &str = "SELECT pg_advisory_unlock(hashtext($1), ($2::bigint)::int)";

/// The job holding a record lock, by its database session's
/// application_name (the bridge sets it to the job's name).
async fn lock_holder(client: &Client, lock_name: &str, rrn: i64) -> Option<String> {
    let row = client
        .query_opt(
            "SELECT a.application_name FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid
             WHERE l.locktype = 'advisory' AND l.granted AND l.objsubid = 2
               AND l.classid = (hashtext($1)::bigint & 4294967295)::oid
               AND l.objid = ($2::bigint & 4294967295)::oid
               AND l.pid <> pg_backend_pid() LIMIT 1",
            &[&lock_name, &rrn],
        )
        .await
        .ok()??;
    let name: String = row.get(0);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

enum Dir {
    Forward,
    Backward,
}

impl OpenFile {
    pub fn new(file: FileRef) -> Self {
        OpenFile {
            file,
            pos: Position::Start,
            last_key: None,
            current_rrn: None,
            eof: false,
            equal: false,
            update: false,
            locked: None,
            lock_name: String::new(),
        }
    }

    pub fn eof(&self) -> bool {
        self.eof
    }

    /// Open for update: reads lock records. `lock_name` names the physical
    /// file the records belong to.
    pub fn for_update(mut self, lock_name: String) -> Self {
        self.update = true;
        self.lock_name = lock_name;
        self
    }

    /// The record this open file holds locked.
    pub fn locked(&self) -> Option<i64> {
        self.locked
    }

    /// Record locks after a read (IBM i rules for a file opened for
    /// update): reading releases the record locked before, and the record
    /// read is locked, unless `lock` is false (the N extender). When another
    /// job holds it, the read waits (the session's lock_timeout, WAITRCD)
    /// and then fails with CPF5027; the record is read again once it is
    /// locked, so the program sees the other job's changes.
    pub async fn lock_after_read(
        &mut self,
        client: &Client,
        mut r: ReadResult,
        lock: bool,
        job_number: Option<i64>,
    ) -> Result<ReadResult> {
        let want = if self.update && lock && r.found {
            r.rrn
        } else {
            None
        };
        if self.locked.is_some() && self.locked != want {
            self.release(client).await?;
        }
        let Some(rrn) = want else { return Ok(r) };
        if self.locked == Some(rrn) {
            return Ok(r);
        }
        let got: bool = client
            .query_one(RECORD_TRY_LOCK_SQL, &[&self.lock_name, &rrn])
            .await?
            .get(0);
        if !got {
            // WRKACTJOB shows the job waiting for a lock (LCKW).
            let set = |a: &'static str| async move {
                if let Some(n) = job_number {
                    let _ = client
                        .execute(
                            "UPDATE as4tainer.jobs SET activity=$2 WHERE number=$1",
                            &[&n, &a],
                        )
                        .await;
                }
            };
            set("LCKW").await;
            let waited = client
                .execute(RECORD_LOCK_SQL, &[&self.lock_name, &rrn])
                .await;
            set("RUN").await;
            if let Err(e) = waited {
                if e.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE) {
                    self.current_rrn = None;
                    let by = lock_holder(client, &self.lock_name, rrn)
                        .await
                        .map(|j| format!(" by job {j}"))
                        .unwrap_or_default();
                    bail!(
                        "CPF5027: Record {rrn} in file {} is in use{by}.",
                        self.lock_name
                    );
                }
                return Err(e.into());
            }
            // Another job may have changed or deleted it meanwhile.
            match self
                .fetch_one(
                    client,
                    "t.rrn = CAST($1::text AS bigint)",
                    "t.rrn",
                    &[rrn.to_string()],
                )
                .await?
            {
                Some((rec, _)) => r.record = Some(rec),
                None => {
                    client
                        .execute(RECORD_UNLOCK_SQL, &[&self.lock_name, &rrn])
                        .await?;
                    self.current_rrn = None;
                    bail!(
                        "CPF5013: Record {rrn} in file {} was deleted by another job.",
                        self.lock_name
                    );
                }
            }
        }
        self.locked = Some(rrn);
        Ok(r)
    }

    /// Release the record lock this open file holds (UPDATE, DELETE,
    /// UNLOCK, close).
    pub async fn release(&mut self, client: &Client) -> Result<()> {
        if let Some(rrn) = self.locked.take() {
            client
                .execute(RECORD_UNLOCK_SQL, &[&self.lock_name, &rrn])
                .await?;
        }
        Ok(())
    }

    /// %EQUAL after SETLL.
    pub fn equal(&self) -> bool {
        self.equal
    }

    fn field(&self, name: &str) -> Result<&FieldDef> {
        self.file
            .file
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                anyhow!(
                    "field {name} not in record format {}",
                    self.file.file.format
                )
            })
    }

    fn key_fields(&self) -> Result<Vec<&FieldDef>> {
        self.file.file.keys.iter().map(|k| self.field(k)).collect()
    }

    fn select_list(&self) -> String {
        let mut cols: Vec<String> = self
            .file
            .file
            .fields
            .iter()
            .map(|f| match f.data_type {
                // bpchar::text drops trailing blanks; RPG expects them.
                DataType::Char => format!("rpad(t.{}::text, {})", ident(&f.name), f.length),
                _ => format!("t.{}::text", ident(&f.name)),
            })
            .collect();
        cols.push("t.rrn".into());
        cols.join(", ")
    }

    fn row_to_record(&self, row: &Row) -> (Record, i64) {
        let mut rec = Record::new();
        for (i, f) in self.file.file.fields.iter().enumerate() {
            rec.insert(f.name.clone(), row.get::<_, String>(i));
        }
        let rrn: i64 = row.get(self.file.file.fields.len());
        (rec, rrn)
    }

    fn record_keys(&self, rec: &Record) -> Vec<String> {
        self.file
            .file
            .keys
            .iter()
            .map(|k| rec.get(k).cloned().unwrap_or_default())
            .collect()
    }

    /// Normalize a value for a field before it is bound as a parameter.
    fn normalize(f: &FieldDef, v: &str) -> String {
        if f.data_type.is_numeric() {
            let t = v.trim();
            if t.is_empty() {
                "0".into()
            } else {
                t.to_string()
            }
        } else {
            v.to_string()
        }
    }

    /// Build `(k1, ..., km) <op> (CAST($n AS t1), ...)` for a key prefix.
    fn key_cond(
        &self,
        prefix: &[String],
        op: &str,
        params: &mut Vec<String>,
        with_rrn: Option<i64>,
    ) -> Result<String> {
        let kf = self.key_fields()?;
        if prefix.len() > kf.len() {
            bail!(
                "{} key values given, file {} has {} key fields",
                prefix.len(),
                self.file.file.name,
                kf.len()
            );
        }
        let mut lhs = Vec::new();
        let mut rhs = Vec::new();
        let mut desc = Vec::new();
        for (f, v) in kf.iter().zip(prefix) {
            params.push(Self::normalize(f, v));
            lhs.push(format!("t.{}", ident(&f.name)));
            rhs.push(format!("CAST(${}::text AS {})", params.len(), f.sql_type()));
            desc.push(self.file.file.is_descending(&f.name));
        }
        if let Some(rrn) = with_rrn {
            params.push(rrn.to_string());
            lhs.push("t.rrn".into());
            rhs.push(format!("CAST(${}::text AS bigint)", params.len()));
            desc.push(false);
        }
        if op == "=" {
            let eq: Vec<String> = lhs
                .iter()
                .zip(&rhs)
                .map(|(l, r)| format!("{l} = {r}"))
                .collect();
            return Ok(format!("({})", eq.join(" AND ")));
        }
        if !desc.contains(&true) {
            return Ok(format!("({}) {op} ({})", lhs.join(", "), rhs.join(", ")));
        }
        // A descending key: compare in access path order field by field.
        // "after" (> or >=) means greater for ascending keys and smaller
        // for descending ones.
        let after = op.starts_with('>');
        let strict = op.len() == 1;
        let mut terms = Vec::new();
        for i in 0..lhs.len() {
            let mut t: Vec<String> = (0..i).map(|j| format!("{} = {}", lhs[j], rhs[j])).collect();
            let cmp = if after != desc[i] { ">" } else { "<" };
            t.push(format!("{} {cmp} {}", lhs[i], rhs[i]));
            terms.push(format!("({})", t.join(" AND ")));
        }
        if !strict {
            let eq: Vec<String> = lhs
                .iter()
                .zip(&rhs)
                .map(|(l, r)| format!("{l} = {r}"))
                .collect();
            terms.push(format!("({})", eq.join(" AND ")));
        }
        Ok(format!("({})", terms.join(" OR ")))
    }

    fn order_by(&self, dir: &Dir) -> String {
        let forward = matches!(dir, Dir::Forward);
        let d = |descending: bool| if forward != descending { "ASC" } else { "DESC" };
        let mut cols: Vec<String> = self
            .file
            .file
            .keys
            .iter()
            .map(|k| format!("t.{} {}", ident(k), d(self.file.file.is_descending(k))))
            .collect();
        cols.push(format!("t.rrn {}", d(false)));
        cols.join(", ")
    }

    /// Condition selecting the records after (or before) the current position.
    fn position_cond(&self, dir: &Dir, params: &mut Vec<String>) -> Result<Option<String>> {
        let keyed = !self.file.file.keys.is_empty();
        Ok(match (&self.pos, dir) {
            (Position::Start, Dir::Forward) | (Position::End, Dir::Backward) => Some("TRUE".into()),
            (Position::Start, Dir::Backward) | (Position::End, Dir::Forward) => None,
            (Position::BeforeKey(p), Dir::Forward) => Some(self.key_cond(p, ">=", params, None)?),
            (Position::BeforeKey(p), Dir::Backward) => Some(self.key_cond(p, "<", params, None)?),
            (Position::AfterKey(p), Dir::Forward) => Some(self.key_cond(p, ">", params, None)?),
            (Position::AfterKey(p), Dir::Backward) => Some(self.key_cond(p, "<=", params, None)?),
            (Position::AtRecord { keys, rrn }, d) => {
                let op = if matches!(d, Dir::Forward) { ">" } else { "<" };
                if keyed {
                    Some(self.key_cond(keys, op, params, Some(*rrn))?)
                } else {
                    params.push(rrn.to_string());
                    Some(format!(
                        "t.rrn {op} CAST(${}::text AS bigint)",
                        params.len()
                    ))
                }
            }
        })
    }

    async fn fetch_one(
        &self,
        client: &Client,
        where_: &str,
        order: &str,
        params: &[String],
    ) -> Result<Option<(Record, i64)>> {
        let sql = format!(
            // Every column is qualified with `t.`: an unqualified ORDER BY
            // name binds to the *output* column (`ordno::text`) and sorts
            // numeric keys as text ("10" before "9").
            "SELECT {} FROM {} AS t WHERE {where_} ORDER BY {order} LIMIT 1",
            self.select_list(),
            self.file.table()
        );
        let p: Vec<&(dyn ToSql + Sync)> = params.iter().map(|s| s as &(dyn ToSql + Sync)).collect();
        let row = client.query_opt(&sql, &p).await?;
        Ok(row.map(|r| self.row_to_record(&r)))
    }

    fn land(&mut self, found: Option<(Record, i64)>, at_end: Position) -> ReadResult {
        match found {
            Some((rec, rrn)) => {
                self.pos = Position::AtRecord {
                    keys: self.record_keys(&rec),
                    rrn,
                };
                self.current_rrn = Some(rrn);
                self.eof = false;
                ReadResult {
                    found: true,
                    eof: false,
                    record: Some(rec),
                    rrn: Some(rrn),
                }
            }
            None => {
                self.pos = at_end;
                self.current_rrn = None;
                self.eof = true;
                ReadResult::none()
            }
        }
    }

    async fn read_dir(
        &mut self,
        client: &Client,
        dir: Dir,
        equal_key: Option<Vec<String>>,
    ) -> Result<ReadResult> {
        let mut params = Vec::new();
        let Some(pos) = self.position_cond(&dir, &mut params)? else {
            self.eof = true;
            self.current_rrn = None;
            return Ok(ReadResult::none());
        };
        let mut where_ = pos;
        if let Some(k) = &equal_key {
            where_ = format!("{where_} AND {}", self.key_cond(k, "=", &mut params, None)?);
        }
        let row = self
            .fetch_one(client, &where_, &self.order_by(&dir), &params)
            .await?;
        let at_end = match dir {
            Dir::Forward => Position::End,
            Dir::Backward => Position::Start,
        };
        // READE/READPE that runs out of matching records leaves the file at
        // the end (or start) of that key's range.
        let at_end = match (&equal_key, &dir) {
            (Some(k), Dir::Forward) => Position::AfterKey(k.clone()),
            (Some(k), Dir::Backward) => Position::BeforeKey(k.clone()),
            _ => at_end,
        };
        Ok(self.land(row, at_end))
    }

    // ---- positioning --------------------------------------------------

    /// SETLL. An empty key means *LOVAL. Returns %EQUAL.
    pub async fn setll(&mut self, client: &Client, key: &[String]) -> Result<bool> {
        self.current_rrn = None;
        self.eof = false;
        if key.is_empty() {
            self.pos = Position::Start;
            self.last_key = None;
            self.equal = false;
            return Ok(false);
        }
        let mut params = Vec::new();
        let cond = self.key_cond(key, "=", &mut params, None)?;
        self.equal = self
            .fetch_one(client, &cond, "t.rrn", &params)
            .await?
            .is_some();
        self.pos = Position::BeforeKey(key.to_vec());
        self.last_key = Some(key.to_vec());
        Ok(self.equal)
    }

    /// SETGT. An empty key means *HIVAL.
    pub async fn setgt(&mut self, _client: &Client, key: &[String]) -> Result<bool> {
        self.current_rrn = None;
        self.eof = false;
        self.equal = false;
        if key.is_empty() {
            self.pos = Position::End;
            self.last_key = None;
            return Ok(false);
        }
        self.pos = Position::AfterKey(key.to_vec());
        self.last_key = Some(key.to_vec());
        Ok(false)
    }

    // ---- reads ----------------------------------------------------------

    pub async fn chain(&mut self, client: &Client, key: &[String]) -> Result<ReadResult> {
        if key.is_empty() {
            bail!("CHAIN needs a search argument");
        }
        let mut params = Vec::new();
        let cond = self.key_cond(key, "=", &mut params, None)?;
        let row = self
            .fetch_one(client, &cond, &self.order_by(&Dir::Forward), &params)
            .await?;
        self.last_key = Some(key.to_vec());
        let r = match row {
            Some(found) => self.land(Some(found), Position::Start),
            None => {
                // Not found: no current record. A following READ starts at
                // the key position, which matches what programs rely on.
                self.pos = Position::BeforeKey(key.to_vec());
                self.current_rrn = None;
                ReadResult {
                    found: false,
                    eof: false,
                    record: None,
                    rrn: None,
                }
            }
        };
        // CHAIN does not set %EOF.
        self.eof = false;
        Ok(ReadResult { eof: false, ..r })
    }

    pub async fn read(&mut self, client: &Client) -> Result<ReadResult> {
        self.read_dir(client, Dir::Forward, None).await
    }

    pub async fn read_previous(&mut self, client: &Client) -> Result<ReadResult> {
        self.read_dir(client, Dir::Backward, None).await
    }

    /// READE. With no key, uses the key of the last SETLL/SETGT/CHAIN.
    pub async fn read_equal(
        &mut self,
        client: &Client,
        key: Option<&[String]>,
    ) -> Result<ReadResult> {
        let key = self.equal_key(key)?;
        self.read_dir(client, Dir::Forward, Some(key)).await
    }

    pub async fn read_previous_equal(
        &mut self,
        client: &Client,
        key: Option<&[String]>,
    ) -> Result<ReadResult> {
        let key = self.equal_key(key)?;
        self.read_dir(client, Dir::Backward, Some(key)).await
    }

    fn equal_key(&mut self, key: Option<&[String]>) -> Result<Vec<String>> {
        match key {
            Some(k) if !k.is_empty() => {
                self.last_key = Some(k.to_vec());
                Ok(k.to_vec())
            }
            _ => self.last_key.clone().ok_or_else(|| {
                anyhow!("READE/READPE without a key needs a prior SETLL, SETGT or CHAIN")
            }),
        }
    }

    // ---- writes -----------------------------------------------------------

    fn values_for(&self, rec: &Record, params: &mut Vec<String>) -> Result<Vec<(String, String)>> {
        for k in rec.keys() {
            self.field(k)?;
        }
        let mut out = Vec::new();
        for f in &self.file.file.fields {
            if let Some(v) = rec.get(&f.name) {
                params.push(Self::normalize(f, v));
                out.push((
                    ident(&f.name),
                    format!("CAST(${}::text AS {})", params.len(), f.sql_type()),
                ));
            }
        }
        Ok(out)
    }

    pub async fn write(&mut self, client: &Client, rec: &Record) -> Result<i64> {
        let mut params = Vec::new();
        let vals = self.values_for(rec, &mut params)?;
        let sql = if vals.is_empty() {
            format!(
                "INSERT INTO {} DEFAULT VALUES RETURNING rrn",
                self.file.table()
            )
        } else {
            let (cols, exprs): (Vec<_>, Vec<_>) = vals.into_iter().unzip();
            format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING rrn",
                self.file.table(),
                cols.join(", "),
                exprs.join(", ")
            )
        };
        let p: Vec<&(dyn ToSql + Sync)> = params.iter().map(|s| s as &(dyn ToSql + Sync)).collect();
        let row = client.query_one(&sql, &p).await.map_err(map_db_error)?;
        Ok(row.get(0))
    }

    fn require_current(&self, op: &str) -> Result<i64> {
        self.current_rrn.ok_or_else(|| {
            anyhow!(
                "CPF5013: {op} on file {} requires a prior successful read",
                self.file.file.name
            )
        })
    }

    pub async fn update(&mut self, client: &Client, rec: &Record) -> Result<()> {
        let rrn = self.require_current("UPDATE")?;
        let mut params = Vec::new();
        let vals = self.values_for(rec, &mut params)?;
        if vals.is_empty() {
            return Ok(());
        }
        let sets: Vec<String> = vals
            .into_iter()
            .map(|(c, e)| format!("{c} = {e}"))
            .collect();
        params.push(rrn.to_string());
        let sql = format!(
            "UPDATE {} SET {} WHERE rrn = CAST(${}::text AS bigint)",
            self.file.table(),
            sets.join(", "),
            params.len()
        );
        let p: Vec<&(dyn ToSql + Sync)> = params.iter().map(|s| s as &(dyn ToSql + Sync)).collect();
        let n = client.execute(&sql, &p).await.map_err(map_db_error)?;
        if n == 0 {
            bail!(
                "CPF5013: record {rrn} of file {} no longer exists",
                self.file.file.name
            );
        }
        // An update can change key values; stay on the same record.
        let keys = self.record_keys_after_update(rec);
        if let Some(keys) = keys {
            self.pos = Position::AtRecord { keys, rrn };
        }
        Ok(())
    }

    fn record_keys_after_update(&self, rec: &Record) -> Option<Vec<String>> {
        match &self.pos {
            Position::AtRecord { keys, .. } => Some(
                self.file
                    .file
                    .keys
                    .iter()
                    .zip(keys)
                    .map(|(k, old)| rec.get(k).cloned().unwrap_or_else(|| old.clone()))
                    .collect(),
            ),
            _ => None,
        }
    }

    pub async fn delete(&mut self, client: &Client) -> Result<()> {
        let rrn = self.require_current("DELETE")?;
        let n = client
            .execute(
                &format!("DELETE FROM {} WHERE rrn = $1", self.file.table()),
                &[&rrn],
            )
            .await?;
        if n == 0 {
            bail!(
                "CPF5013: record {rrn} of file {} no longer exists",
                self.file.file.name
            );
        }
        // Position stays where the deleted record was, so READ continues
        // with the next one.
        self.current_rrn = None;
        Ok(())
    }
}

fn map_db_error(e: tokio_postgres::Error) -> anyhow::Error {
    if let Some(db) = e.as_db_error() {
        return match db.code().code() {
            "23505" => anyhow!("CPF5026: duplicate key on a UNIQUE file"),
            "22003" => anyhow!(
                "RNX0103: value does not fit the numeric field ({})",
                db.message()
            ),
            "22P02" => anyhow!("RNX0105: value is not a valid number ({})", db.message()),
            _ => anyhow!("database error {}: {}", db.code().code(), db.message()),
        };
    }
    anyhow!(e)
}
