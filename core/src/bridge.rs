//! The JVM bridge: a line-delimited JSON protocol over TCP (loopback only).
//!
//! Jariko runs RPG on the JVM; every file operation it performs is sent here
//! and executed by the record-level access layer. One TCP connection is one
//! job's view of the database: it owns a PostgreSQL session and the open
//! files (handles) created on it. See docs/bridge-protocol.md.

use crate::authority::Aut;
use crate::catalog;
use crate::dspf;
use crate::dtaara;
use crate::rla::{OpenFile, ReadResult, Record};
use crate::workstn::{Frame, Workstation};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_postgres::{Client, NoTls};

pub const PROTOCOL_VERSION: u32 = 6;

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub database_url: String,
    pub library_list: Vec<String>,
    /// The job's display, when it has one (interactive jobs). EXFMT fails
    /// without it.
    pub display: Option<DisplayHandle>,
    /// The job's qualified name (`000123/USER/NAME`), shown to other jobs
    /// waiting for a record this job has locked.
    pub job: Option<String>,
    /// How long a read waits for a record another job has locked (the
    /// file's WAITRCD). None: `AS4TAINR_WAITRCD` seconds, else 60.
    pub record_wait: Option<std::time::Duration>,
}

/// The record wait when the bridge config does not say: AS4TAINR_WAITRCD
/// seconds, else 60. On IBM i each file has its own WAITRCD (CRTPF,
/// OVRDBF); as4tainr has one wait time for every file (a known gap).
fn job_number_of(config: &BridgeConfig) -> Option<i64> {
    config
        .job
        .as_deref()
        .and_then(|j| j.split('/').next())
        .and_then(|n| n.parse().ok())
}

/// Set up a job's database session: other jobs see who holds a record
/// lock by application_name; reads wait for locked records up to WAITRCD
/// (lock_timeout).
pub async fn job_session(
    client: &Client,
    job: Option<&str>,
    wait: std::time::Duration,
) -> Result<()> {
    client
        .batch_execute(&format!(
            "SET application_name = '{}'; SET lock_timeout = '{}ms'",
            job.unwrap_or("")
                .replace('\'', "")
                .chars()
                .take(60)
                .collect::<String>(),
            wait.as_millis().max(1)
        ))
        .await?;
    Ok(())
}

pub fn default_record_wait() -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var("AS4TAINR_WAITRCD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60),
    )
}

/// One EXFMT, sent to whatever owns the job's display.
#[derive(Debug)]
pub struct DisplayRequest {
    /// What to show: the records on the screen, the one being read last.
    pub frame: Frame,
    pub reply: tokio::sync::oneshot::Sender<std::result::Result<dspf::Response, String>>,
}

pub type DisplayHandle = tokio::sync::mpsc::Sender<DisplayRequest>;

#[derive(Deserialize, Debug)]
struct Request {
    #[serde(default)]
    id: Value,
    op: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    handle: Option<u32>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    record: Option<Record>,
    /// EXFMT: record format name.
    #[serde(default)]
    format: Option<String>,
    /// EXFMT: field values of the record format.
    #[serde(default)]
    fields: Option<BTreeMap<String, String>>,
    /// EXFMT: indicators 01-99 as a string of 99 '0'/'1' characters.
    #[serde(default)]
    indicators: Option<String>,
    /// Subfile operations: relative record number.
    #[serde(default)]
    rrn: Option<i64>,
    /// Data areas: the name (from `*DTAARA DEFINE`).
    #[serde(default)]
    name: Option<String>,
    /// Data areas: the value to write.
    #[serde(default)]
    value: Option<String>,
    /// Data areas: `IN *LOCK`. Reads: false for the N extender (read
    /// without locking).
    #[serde(default)]
    lock: Option<bool>,
    /// open: the file is opened for update (RPG file type U).
    #[serde(default)]
    update: Option<bool>,
    /// Spooled files: the spooled file (from splopen).
    #[serde(default)]
    spl: Option<i64>,
    /// splopen: user data (the program name).
    #[serde(default)]
    user_data: Option<String>,
    /// splwrite: lines as [page, line, text].
    #[serde(default)]
    lines: Option<Vec<(i32, i32, String)>>,
    /// pgmsrc, srcmbr: a library (absent: the library list).
    #[serde(default)]
    library: Option<String>,
    /// srcmbr: the source member.
    #[serde(default)]
    member: Option<String>,
}

struct Session {
    client: Client,
    config: Arc<BridgeConfig>,
    /// Display files and subfiles: shared by all connections of one bridge
    /// (one program call).
    workstation: Arc<tokio::sync::Mutex<Workstation>>,
    displays: HashMap<String, Arc<dspf::DisplayFile>>,
    files: HashMap<u32, OpenFile>,
    next_handle: u32,
    /// Data areas this session holds under `*LOCK` (library/name).
    locked: Vec<(String, String)>,
    /// The job's number, for WRKACTJOB's LCKW while waiting for a record.
    job_number: Option<i64>,
    /// Spooled files opened on this connection. Without a job (`as4tainr
    /// call`), printed lines go to standard output instead: the last
    /// (page, line) printed per spooled file.
    stdout_spool: HashMap<i64, (i32, i32)>,
    next_stdout_spool: i64,
    /// Open files the job may change (*CHANGE checked).
    change_ok: std::collections::HashSet<u32>,
}

fn indicators(s: Option<String>) -> [bool; 100] {
    let mut ind = [false; 100];
    for (i, c) in s.unwrap_or_default().chars().take(99).enumerate() {
        ind[i + 1] = c == '1';
    }
    ind
}

fn read_json(r: ReadResult) -> Value {
    json!({ "found": r.found, "eof": r.eof, "record": r.record, "rrn": r.rrn })
}

impl Session {
    /// The job this bridge serves, when it is a system job.
    fn job_id(&self) -> Option<crate::jobs::JobId> {
        let spec = crate::jobs::parse_spec(self.config.job.as_deref()?).ok()?;
        Some(crate::jobs::JobId {
            number: spec.number?,
            user: spec.user?,
            name: spec.name,
        })
    }

    /// Object authority for the job's user (no job: `as4tainr call`, which
    /// runs as QSECOFR, is not checked).
    async fn require(&self, library: &str, name: &str, objtype: &str, need: Aut) -> Result<()> {
        match self.job_id() {
            Some(job) => {
                crate::authority::check(&self.client, &job.user, library, name, objtype, need).await
            }
            None => Ok(()),
        }
    }

    /// WRITE, UPDATE and DELETE change the file: *CHANGE (checked once
    /// per open file).
    async fn require_change(&mut self, handle: Option<u32>) -> Result<()> {
        let h = handle.ok_or_else(|| anyhow!("missing 'handle'"))?;
        if self.change_ok.contains(&h) {
            return Ok(());
        }
        let (lib, name) = {
            let f = self
                .files
                .get(&h)
                .ok_or_else(|| anyhow!("unknown handle {h}"))?;
            (f.file.library.clone(), f.file.file.name.clone())
        };
        self.require(&lib, &name, "*FILE", Aut::Change).await?;
        self.change_ok.insert(h);
        Ok(())
    }

    async fn display_file(&mut self, name: &str) -> Result<Arc<dspf::DisplayFile>> {
        let key = name.to_ascii_uppercase();
        if let Some(f) = self.displays.get(&key) {
            return Ok(f.clone());
        }
        let (library, source) =
            catalog::resolve_display_file(&self.client, &self.config.library_list, name).await?;
        self.require(&library, &key, "*FILE", Aut::Use).await?;
        let f = Arc::new(dspf::parse(name, &source)?);
        self.displays.insert(key, f.clone());
        Ok(f)
    }

    fn file(&mut self, handle: Option<u32>) -> Result<&mut OpenFile> {
        let h = handle.ok_or_else(|| anyhow!("missing 'handle'"))?;
        self.files
            .get_mut(&h)
            .ok_or_else(|| anyhow!("unknown handle {h}"))
    }

    async fn dispatch(&mut self, req: Request) -> Result<Value> {
        let keys = req.keys.clone().unwrap_or_default();
        match req.op.as_str() {
            "hello" => Ok(json!({ "server": "as4tainr-core", "version": PROTOCOL_VERSION })),
            "metadata" => {
                let name = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                let f =
                    catalog::resolve_file(&self.client, &self.config.library_list, &name).await?;
                Ok(json!({
                    "library": f.library,
                    "name": f.file.name,
                    "format": f.file.format,
                    "text": f.file.text,
                    "unique": f.file.unique,
                    "keys": f.file.keys,
                    "fields": f.file.fields.iter().map(|fd| json!({
                        "name": fd.name,
                        "type": fd.data_type.code(),
                        "length": fd.length,
                        "decimals": fd.decimals,
                        "numeric": fd.data_type.is_numeric(),
                        "text": fd.text,
                    })).collect::<Vec<_>>(),
                }))
            }
            "open" => {
                let name = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                let f =
                    catalog::resolve_file(&self.client, &self.config.library_list, &name).await?;
                let update = req.update.unwrap_or(false);
                self.require(
                    &f.library,
                    &f.file.name,
                    "*FILE",
                    if update { Aut::Change } else { Aut::Use },
                )
                .await?;
                let mut open = OpenFile::new(f.clone());
                if update {
                    let (lib, pf) =
                        catalog::physical_file_of(&self.client, &f.library, &f.file.name).await?;
                    open = open.for_update(format!("{lib}/{pf}"));
                }
                let h = self.next_handle;
                self.next_handle += 1;
                self.files.insert(h, open);
                Ok(json!({ "handle": h }))
            }
            "close" => {
                let h = req.handle.ok_or_else(|| anyhow!("missing 'handle'"))?;
                if let Some(mut f) = self.files.remove(&h) {
                    f.release(&self.client).await?;
                }
                Ok(json!({}))
            }
            "unlock" => {
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                f.release(client).await?;
                Ok(json!({}))
            }
            "eof" => Ok(json!({ "eof": self.file(req.handle)?.eof() })),
            "equal" => Ok(json!({ "equal": self.file(req.handle)?.equal() })),
            "setll" => {
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                Ok(json!({ "equal": f.setll(client, &keys).await? }))
            }
            "setgt" => {
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                f.setgt(client, &keys).await?;
                Ok(json!({}))
            }
            "chain" | "read" | "readp" | "reade" | "readpe" => {
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                let key = req.keys.as_deref();
                let r = match req.op.as_str() {
                    "chain" => f.chain(client, &keys).await?,
                    "read" => f.read(client).await?,
                    "readp" => f.read_previous(client).await?,
                    "reade" => f.read_equal(client, key).await?,
                    _ => f.read_previous_equal(client, key).await?,
                };
                let r = f
                    .lock_after_read(client, r, req.lock.unwrap_or(true), self.job_number)
                    .await?;
                Ok(read_json(r))
            }
            "write" | "update" => {
                self.require_change(req.handle).await?;
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                let rec = req.record.ok_or_else(|| anyhow!("missing 'record'"))?;
                if req.op == "write" {
                    Ok(json!({ "rrn": f.write(client, &rec).await? }))
                } else {
                    f.update(client, &rec).await?;
                    // UPDATE releases the record lock.
                    f.release(client).await?;
                    Ok(json!({}))
                }
            }
            "delete" => {
                self.require_change(req.handle).await?;
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                f.delete(client).await?;
                f.release(client).await?;
                Ok(json!({}))
            }
            "dspf" => {
                let name = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                let (library, source) =
                    catalog::resolve_display_file(&self.client, &self.config.library_list, &name)
                        .await?;
                self.require(&library, &name.to_ascii_uppercase(), "*FILE", Aut::Use)
                    .await?;
                let parsed = dspf::parse(&name, &source)?;
                Ok(json!({
                    "library": library,
                    "name": parsed.name,
                    "source": source,
                    "formats": parsed.records.iter().map(|r| &r.name).collect::<Vec<_>>(),
                }))
            }
            "exfmt" => {
                let name = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                let format = req.format.ok_or_else(|| anyhow!("missing 'format'"))?;
                let Some(display) = self.config.display.clone() else {
                    bail!("Display file {name} cannot be used here: this job has no display (it is not interactive).");
                };
                let file = self.display_file(&name).await?;
                let frame = self.workstation.lock().await.frame(
                    &file,
                    &format,
                    req.fields.unwrap_or_default(),
                    indicators(req.indicators),
                )?;
                let (tx, rx) = tokio::sync::oneshot::channel();
                display
                    .send(DisplayRequest { frame, reply: tx })
                    .await
                    .map_err(|_| anyhow!("the job's display is gone"))?;
                let resp = rx
                    .await
                    .map_err(|_| anyhow!("the job's display is gone"))?
                    .map_err(|e| anyhow!(e))?;
                self.workstation.lock().await.apply(&file, &format, &resp);
                Ok(json!({
                    "aid": resp.aid,
                    "fields": resp.values,
                    "indicators": resp.indicators.iter().map(|(k, v)| (format!("{k:02}"), *v)).collect::<BTreeMap<_, _>>(),
                }))
            }
            "dspwrite" | "sflwrite" | "sflchain" | "sflreadc" | "sflupdate" => {
                let name = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                let format = req.format.ok_or_else(|| anyhow!("missing 'format'"))?;
                let file = self.display_file(&name).await?;
                let mut ws = self.workstation.lock().await;
                let fields = req.fields.unwrap_or_default();
                let ind = indicators(req.indicators);
                let rrn = || req.rrn.ok_or_else(|| anyhow!("missing 'rrn'"));
                match req.op.as_str() {
                    "dspwrite" => {
                        ws.write(&file, &format, fields, ind)?;
                        Ok(json!({}))
                    }
                    "sflwrite" => {
                        ws.sfl_write(&file, &format, rrn()?, fields, ind)?;
                        Ok(json!({}))
                    }
                    "sflupdate" => {
                        ws.sfl_update(&file, &format, fields, ind)?;
                        Ok(json!({}))
                    }
                    "sflchain" => {
                        let n = rrn()?;
                        Ok(match ws.sfl_chain(&file, &format, n)? {
                            Some(r) => {
                                json!({ "found": true, "eof": false, "record": r, "rrn": n })
                            }
                            None => {
                                json!({ "found": false, "eof": false, "record": {}, "rrn": null })
                            }
                        })
                    }
                    _ => Ok(match ws.sfl_readc(&file, &format)? {
                        Some((n, r)) => {
                            json!({ "found": true, "eof": false, "record": r, "rrn": n })
                        }
                        None => json!({ "found": false, "eof": true, "record": {}, "rrn": null }),
                    }),
                }
            }
            "dtaara_in" | "dtaara_out" | "dtaara_unlock" => {
                let name = req.name.ok_or_else(|| anyhow!("missing 'name'"))?;
                // RPG names a data area without a library (factor 2 of
                // DEFINE), so only the library list matters here.
                let libl = &self.config.library_list;
                let cur = crate::commands::DEFAULT_CURRENT_LIBRARY;
                match req.op.as_str() {
                    "dtaara_in" => {
                        let lock = req.lock.unwrap_or(false);
                        let key_of = |d: &dtaara::DataArea| (d.library.clone(), d.name.clone());
                        let held = {
                            let d = dtaara::resolve(&self.client, &name, libl, cur).await?;
                            self.require(&d.library, &d.name, "*DTAARA", Aut::Use)
                                .await?;
                            self.locked.contains(&key_of(&d))
                        };
                        // A lock this session already holds is not taken again.
                        let d = dtaara::read(&self.client, &name, libl, cur, lock && !held).await?;
                        if lock && !held {
                            self.locked.push(key_of(&d));
                        }
                        Ok(json!({
                            "library": d.library,
                            "name": d.name,
                            "type": d.ty.code(),
                            "length": d.length,
                            "decimals": d.decimals,
                            "value": d.value,
                        }))
                    }
                    "dtaara_out" => {
                        let value = req.value.ok_or_else(|| anyhow!("missing 'value'"))?;
                        let d = dtaara::resolve(&self.client, &name, libl, cur).await?;
                        self.require(&d.library, &d.name, "*DTAARA", Aut::Change)
                            .await?;
                        let key = (d.library.clone(), d.name.clone());
                        let held = self.locked.contains(&key);
                        dtaara::write(&self.client, &d, &value, held).await?;
                        self.locked.retain(|k| *k != key);
                        Ok(json!({}))
                    }
                    _ => {
                        let d = dtaara::resolve(&self.client, &name, libl, cur).await?;
                        let key = (d.library.clone(), d.name.clone());
                        if self.locked.contains(&key) {
                            dtaara::unlock(&self.client, &d).await?;
                            self.locked.retain(|k| *k != key);
                        }
                        Ok(json!({}))
                    }
                }
            }
            "pgmsrc" => {
                // The source of an RPG program object (CRTBNDRPG), for the
                // runner: the program called, and programs it calls.
                let name = req.name.ok_or_else(|| anyhow!("missing 'name'"))?;
                let libs = match req.library.as_deref() {
                    Some(l) if !l.is_empty() => vec![l.to_ascii_uppercase()],
                    _ => self.config.library_list.clone(),
                };
                match catalog::find_program(&self.client, &libs, &name).await? {
                    Some(p) if p.attribute == "RPGLE" => {
                        self.require(&p.library, &p.name, "*PGM", Aut::Use).await?;
                        Ok(json!({ "found": true, "library": p.library, "source": p.source }))
                    }
                    _ => Ok(json!({ "found": false })),
                }
            }
            "srcmbr" => {
                // /COPY: a member of a source file (default QRPGLESRC).
                let member = req.member.ok_or_else(|| anyhow!("missing 'member'"))?;
                // `/INCLUDE 'path/name.rpgleinc'`: a stream file.
                if member.contains('.') || member.contains('/') {
                    return match crate::source::find_stream_file(&self.client, &member).await? {
                        Some((path, data)) => {
                            Ok(json!({ "found": true, "library": path, "source": data }))
                        }
                        None => Ok(json!({ "found": false })),
                    };
                }
                let file = req.file.unwrap_or_else(|| "QRPGLESRC".into());
                let libs = match req.library.as_deref() {
                    Some(l) if !l.is_empty() && l != "*LIBL" => vec![l.to_ascii_uppercase()],
                    _ => self.config.library_list.clone(),
                };
                match crate::source::find_member(&self.client, &libs, &file, &member).await {
                    Ok(m) => Ok(json!({ "found": true, "library": m.library, "source": m.source })),
                    Err(_) => Ok(json!({ "found": false })),
                }
            }
            "splopen" => {
                let file = req.file.ok_or_else(|| anyhow!("missing 'file'"))?;
                match self.job_id() {
                    Some(job) => {
                        let id = crate::spool::open(
                            &self.client,
                            &job,
                            &file,
                            req.user_data.as_deref().unwrap_or(""),
                        )
                        .await?;
                        Ok(json!({ "spl": id }))
                    }
                    None => {
                        // No job: print to standard output.
                        let id = self.next_stdout_spool;
                        self.next_stdout_spool -= 1;
                        self.stdout_spool.insert(id, (1, 0));
                        Ok(json!({ "spl": id }))
                    }
                }
            }
            "splwrite" => {
                let id = req.spl.ok_or_else(|| anyhow!("missing 'spl'"))?;
                let lines = req.lines.unwrap_or_default();
                if let Some(pos) = self.stdout_spool.get_mut(&id) {
                    for (page, line, text) in lines {
                        if page > pos.0 {
                            println!("\x0c");
                            *pos = (page, 0);
                        }
                        while pos.1 + 1 < line {
                            println!();
                            pos.1 += 1;
                        }
                        println!("{}", text.trim_end());
                        pos.1 = line;
                    }
                } else {
                    crate::spool::write(&self.client, id, &lines).await?;
                }
                Ok(json!({}))
            }
            "splclose" => {
                let id = req.spl.ok_or_else(|| anyhow!("missing 'spl'"))?;
                if self.stdout_spool.remove(&id).is_none() {
                    crate::spool::close(&self.client, id).await?;
                }
                Ok(json!({}))
            }
            other => Err(anyhow!("unknown op '{other}'")),
        }
    }
}

pub async fn connect_db(url: &str) -> Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("as4tainr: database connection error: {e}");
        }
    });
    Ok(client)
}

async fn handle_conn(
    stream: TcpStream,
    config: Arc<BridgeConfig>,
    workstation: Arc<tokio::sync::Mutex<Workstation>>,
) -> Result<()> {
    let client = connect_db(&config.database_url).await?;
    // Other jobs see who holds a record lock by application_name; reads
    // wait for locked records up to WAITRCD (lock_timeout).
    let wait = config.record_wait.unwrap_or_else(default_record_wait);
    job_session(&client, config.job.as_deref(), wait).await?;
    let job_number = job_number_of(&config);
    let mut session = Session {
        client,
        config,
        workstation,
        displays: HashMap::new(),
        files: HashMap::new(),
        next_handle: 1,
        locked: Vec::new(),
        job_number,
        stdout_spool: HashMap::new(),
        next_stdout_spool: -1,
        change_ok: std::collections::HashSet::new(),
    };
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let id = req.id.clone();
                match session.dispatch(req).await {
                    Ok(Value::Object(mut m)) => {
                        m.insert("id".into(), id);
                        m.insert("ok".into(), Value::Bool(true));
                        Value::Object(m)
                    }
                    Ok(other) => json!({ "id": id, "ok": true, "value": other }),
                    Err(e) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
                }
            }
            Err(e) => json!({ "id": null, "ok": false, "error": format!("bad request: {e}") }),
        };
        let mut out = serde_json::to_vec(&resp)?;
        out.push(b'\n');
        wr.write_all(&out).await?;
    }
    Ok(())
}

/// Bind the bridge on loopback. Port 0 picks a free port.
pub async fn bind(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind(("127.0.0.1", port)).await?)
}

/// Serve connections until the listener task is dropped.
pub async fn serve(listener: TcpListener, config: BridgeConfig) -> Result<()> {
    let config = Arc::new(config);
    let workstation = Arc::new(tokio::sync::Mutex::new(Workstation::new()));
    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = config.clone();
        let ws = workstation.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, cfg, ws).await {
                eprintln!("as4tainr: bridge session ended with error: {e:#}");
            }
        });
    }
}
