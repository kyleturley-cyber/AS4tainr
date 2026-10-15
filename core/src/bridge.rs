//! The JVM bridge: a line-delimited JSON protocol over TCP (loopback only).
//!
//! Jariko runs RPG on the JVM; every file operation it performs is sent here
//! and executed by the record-level access layer. One TCP connection is one
//! job's view of the database: it owns a PostgreSQL session and the open
//! files (handles) created on it. See docs/bridge-protocol.md.

use crate::catalog;
use crate::rla::{OpenFile, ReadResult, Record};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_postgres::{Client, NoTls};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub database_url: String,
    pub library_list: Vec<String>,
}

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
}

struct Session {
    client: Client,
    config: Arc<BridgeConfig>,
    files: HashMap<u32, OpenFile>,
    next_handle: u32,
}

fn read_json(r: ReadResult) -> Value {
    json!({ "found": r.found, "eof": r.eof, "record": r.record, "rrn": r.rrn })
}

impl Session {
    fn file(&mut self, handle: Option<u32>) -> Result<&mut OpenFile> {
        let h = handle.ok_or_else(|| anyhow!("missing 'handle'"))?;
        self.files
            .get_mut(&h)
            .ok_or_else(|| anyhow!("unknown handle {h}"))
    }

    async fn dispatch(&mut self, req: Request) -> Result<Value> {
        let keys = req.keys.clone().unwrap_or_default();
        match req.op.as_str() {
            "hello" => Ok(json!({ "server": "ibmish-core", "version": PROTOCOL_VERSION })),
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
                let h = self.next_handle;
                self.next_handle += 1;
                self.files.insert(h, OpenFile::new(f));
                Ok(json!({ "handle": h }))
            }
            "close" => {
                let h = req.handle.ok_or_else(|| anyhow!("missing 'handle'"))?;
                self.files.remove(&h);
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
                Ok(read_json(r))
            }
            "write" | "update" => {
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
                    Ok(json!({}))
                }
            }
            "delete" => {
                let client = &self.client;
                let f = self
                    .files
                    .get_mut(&req.handle.unwrap_or(0))
                    .ok_or_else(|| anyhow!("unknown handle"))?;
                f.delete(client).await?;
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
            eprintln!("ibmish: database connection error: {e}");
        }
    });
    Ok(client)
}

async fn handle_conn(stream: TcpStream, config: Arc<BridgeConfig>) -> Result<()> {
    let client = connect_db(&config.database_url).await?;
    let mut session = Session {
        client,
        config,
        files: HashMap::new(),
        next_handle: 1,
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
    loop {
        let (stream, _) = listener.accept().await?;
        let cfg = config.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, cfg).await {
                eprintln!("ibmish: bridge session ended with error: {e:#}");
            }
        });
    }
}
