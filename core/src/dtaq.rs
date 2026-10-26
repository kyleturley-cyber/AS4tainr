//! Data queues (*DTAQ): CRTDTAQ, DLTDTAQ and the data queue APIs
//! QSNDDTAQ, QRCVDTAQ, QCLRDTAQ and QMHQRDQD, called from CL and RPG.
//!
//! Written from IBM's documentation: the CL reference (CRTDTAQ) and the
//! Object APIs ("Data Queue APIs": Send Data Queue, Receive Data Queue,
//! Clear Data Queue, Retrieve Data Queue Description; parameter lists,
//! the sender information and RDQD0100 formats, error messages).
//!
//! A data queue keeps entries of up to MAXLEN bytes, received in first
//! in first out order (*FIFO), last in first out (*LIFO) or by key
//! (*KEYED: lowest key first, entries with equal keys in the order they
//! were sent). Entries are bytes as programs see them (EBCDIC text from
//! CL); a receive removes the entry it returns (or leaves it, REMOVE
//! *NO) in one statement, so two jobs never get the same entry. A
//! receive that waits looks again every fifth of a second.

use crate::authority::{self, Aut};
use crate::catalog::validate_name;
use crate::rpg::data::{a, e};
use anyhow::{anyhow, bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.data_queues (
    library   text NOT NULL,
    name      text NOT NULL,
    maxlen    int NOT NULL,
    seq       text NOT NULL,            -- F (*FIFO), L (*LIFO), K (*KEYED)
    keylen    int NOT NULL DEFAULT 0,
    senderid  boolean NOT NULL DEFAULT false,
    force     boolean NOT NULL DEFAULT false,
    autorcl   boolean NOT NULL DEFAULT false,
    size_max  int NOT NULL DEFAULT -1,  -- -1 *MAX16MB, -2 *MAX2GB
    size_init int NOT NULL DEFAULT 16,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.data_queue_entries (
    id        bigserial PRIMARY KEY,
    library   text NOT NULL,
    name      text NOT NULL,
    key       bytea NOT NULL DEFAULT ''::bytea,
    data      bytea NOT NULL,
    enqueued  timestamptz NOT NULL DEFAULT clock_timestamp(),
    job_name  text NOT NULL DEFAULT '',
    job_user  text NOT NULL DEFAULT '',
    job_nbr   text NOT NULL DEFAULT '',
    cur_user  text NOT NULL DEFAULT '',
    FOREIGN KEY (library, name) REFERENCES as4tainr.data_queues ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS data_queue_entries_q ON as4tainr.data_queue_entries (library, name, key, id);
"#;

/// The most a data queue entry can hold (CRTDTAQ MAXLEN).
pub const MAX_ENTRY: i32 = 64512;
/// The longest key (CRTDTAQ KEYLEN).
pub const MAX_KEY: i32 = 256;

#[derive(Debug, Clone)]
pub struct DataQueue {
    pub library: String,
    pub name: String,
    pub maxlen: i32,
    /// 'F', 'L' or 'K'.
    pub seq: char,
    pub keylen: i32,
    pub senderid: bool,
    pub force: bool,
    pub autorcl: bool,
    pub size_max: i32,
    pub size_init: i32,
    pub text: String,
}

/// CRTDTAQ's values.
pub struct Create<'a> {
    pub library: &'a str,
    pub name: &'a str,
    pub maxlen: i32,
    pub seq: char,
    pub keylen: i32,
    pub senderid: bool,
    pub force: bool,
    pub autorcl: bool,
    pub size_max: i32,
    pub size_init: i32,
    pub text: &'a str,
}

/// CRTDTAQ. CPF9810 when the library is not there, CPF9870 when the
/// queue already exists (both in IBM's lists for the data queue
/// commands and APIs).
pub async fn create(client: &Client, c: &Create<'_>) -> Result<()> {
    let lib = validate_name("library", c.library)?;
    let name = validate_name("data queue", c.name)?;
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF9810: There is no library {lib}.");
    }
    let n = client
        .execute(
            "WITH o AS (
                INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                VALUES ($1, $2, '*DTAQ', '', $3) ON CONFLICT DO NOTHING RETURNING 1)
             INSERT INTO as4tainr.data_queues
                 (library, name, maxlen, seq, keylen, senderid, force, autorcl, size_max, size_init)
             SELECT $1, $2, $4, $5, $6, $7, $8, $9, $10, $11 FROM o",
            &[
                &lib,
                &name,
                &c.text,
                &c.maxlen,
                &c.seq.to_string(),
                &(if c.seq == 'K' { c.keylen } else { 0 }),
                &c.senderid,
                &c.force,
                &c.autorcl,
                &c.size_max,
                &c.size_init,
            ],
        )
        .await?;
    if n == 0 {
        bail!("CPF9870: Object {name} type *DTAQ already exists in library {lib}.");
    }
    Ok(())
}

/// DLTDTAQ: the queue, its entries and its authorities.
pub async fn delete(client: &Client, library: &str, name: &str) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainr.data_queues WHERE library=$1 AND name=$2",
            &[&library, &name],
        )
        .await?;
    authority::forget(client, library, name, "*DTAQ").await?;
    client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*DTAQ'",
            &[&library, &name],
        )
        .await?;
    Ok(())
}

/// The libraries to look in for LIB/NAME (*LIBL, *CURLIB or a name).
fn libraries(lib: &str, caller: &Caller) -> Vec<String> {
    match lib.trim().to_ascii_uppercase().as_str() {
        "*LIBL" | "" => caller.libl.clone(),
        "*CURLIB" => vec![caller.curlib.clone()],
        l => vec![crate::qtemp::real(l)],
    }
}

/// Find a data queue: CPF9810 when a named library is not there, CPF9801
/// when the queue is not found.
pub async fn find(client: &Client, lib: &str, name: &str, caller: &Caller) -> Result<DataQueue> {
    let name = name.trim().to_ascii_uppercase();
    let libs = libraries(lib, caller);
    let label = lib.trim().to_ascii_uppercase();
    let label = if label.is_empty() {
        "*LIBL".into()
    } else {
        label
    };
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT q.maxlen, q.seq, q.keylen, q.senderid, q.force, q.autorcl, q.size_max,
                        q.size_init, o.text
                   FROM as4tainr.data_queues q JOIN as4tainr.objects o
                     ON o.library=q.library AND o.name=q.name AND o.objtype='*DTAQ'
                  WHERE q.library=$1 AND q.name=$2",
                &[l, &name],
            )
            .await?
        {
            return Ok(DataQueue {
                library: l.clone(),
                name,
                maxlen: r.get(0),
                seq: r.get::<_, String>(1).chars().next().unwrap_or('F'),
                keylen: r.get(2),
                senderid: r.get(3),
                force: r.get(4),
                autorcl: r.get(5),
                size_max: r.get(6),
                size_init: r.get(7),
                text: r.get(8),
            });
        }
    }
    if !label.starts_with('*') && !crate::catalog::library_exists(client, &label).await? {
        bail!("CPF9810: There is no library {label}.");
    }
    bail!("CPF9801: Object {name} is not in library {label}.")
}

/// Who calls a data queue API.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub user: String,
    pub job: Option<crate::jobs::JobId>,
    pub libl: Vec<String>,
    pub curlib: String,
}

/// An API's parameters as the caller passed them: CL variables (text) or
/// RPG storage (bytes of a type).
pub trait ApiArgs: Send {
    fn count(&self) -> usize;
    /// The parameter's bytes (character data in EBCDIC).
    fn bytes(&self, i: usize) -> Vec<u8>;
    /// A numeric parameter (packed, zoned, binary or integer).
    fn num(&self, i: usize) -> Result<i64>;
    /// Put bytes at the start of a parameter (no further than its end).
    fn set_bytes(&mut self, i: usize, b: &[u8]);
    fn set_num(&mut self, i: usize, n: i64) -> Result<()>;
}

/// CL variables: character values as text, decimal values as numbers.
pub struct ClArgs(pub Vec<String>);

impl ApiArgs for ClArgs {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn bytes(&self, i: usize) -> Vec<u8> {
        self.0.get(i).map(|s| e(s)).unwrap_or_default()
    }
    fn num(&self, i: usize) -> Result<i64> {
        let s = self.0.get(i).map(String::as_str).unwrap_or("");
        let t = s.trim();
        if let Ok(d) = crate::rpg::dec::Dec::parse(t) {
            return Ok(d.to_i128()? as i64);
        }
        // A *CHAR variable holding a binary number (%BIN), or a hex
        // constant: its first four bytes. A character constant is passed
        // as 32 bytes padded with blanks (CL reference, CALL PARM), so
        // X'01F4' is read as X'01F44040'.
        let mut b = e(s);
        if b.is_empty() {
            bail!("CPF24B4: Parameter {} is not a number.", i + 1)
        }
        if b.len() < 4 {
            b.resize(4, 0x40);
        }
        Ok(i32::from_be_bytes(b[..4].try_into().unwrap()) as i64)
    }
    fn set_bytes(&mut self, i: usize, b: &[u8]) {
        if let Some(s) = self.0.get_mut(i) {
            let mut old = e(s);
            let n = b.len().min(old.len());
            old[..n].copy_from_slice(&b[..n]);
            *s = a(&old);
        }
    }
    fn set_num(&mut self, i: usize, n: i64) -> Result<()> {
        if let Some(s) = self.0.get_mut(i) {
            *s = n.to_string();
        }
        Ok(())
    }
}

/// RPG parameters: each one's storage and type.
pub struct RpgArgs(pub Vec<(Vec<u8>, crate::rpg::data::DType)>);

impl ApiArgs for RpgArgs {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn bytes(&self, i: usize) -> Vec<u8> {
        self.0.get(i).map(|x| x.0.clone()).unwrap_or_default()
    }
    fn num(&self, i: usize) -> Result<i64> {
        use crate::rpg::data::{load, DType};
        let (b, t) = self
            .0
            .get(i)
            .ok_or_else(|| anyhow!("CPF3C36: Parameter {} was not passed.", i + 1))?;
        if matches!(t, DType::Char(4)) {
            return Ok(i32::from_be_bytes(b[..4].try_into().unwrap()) as i64);
        }
        Ok(load(t, b)?.num()?.to_i128()? as i64)
    }
    fn set_bytes(&mut self, i: usize, b: &[u8]) {
        if let Some((buf, _)) = self.0.get_mut(i) {
            let n = b.len().min(buf.len());
            buf[..n].copy_from_slice(&b[..n]);
        }
    }
    fn set_num(&mut self, i: usize, n: i64) -> Result<()> {
        use crate::rpg::data::{store, DType, Fit, Value};
        if let Some((buf, t)) = self.0.get_mut(i) {
            if matches!(t, DType::Char(4)) {
                buf[..4].copy_from_slice(&(n as i32).to_be_bytes());
                return Ok(());
            }
            let v = Value::Num(crate::rpg::dec::Dec::parse(&n.to_string())?);
            store(t, &v, buf, Fit::Truncate { half_adjust: false }, false)?;
        }
        Ok(())
    }
}

/// The data queue APIs done here.
pub fn is_api(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "QSNDDTAQ" | "QRCVDTAQ" | "QCLRDTAQ" | "QMHQRDQD"
    )
}

/// The position of an API's error code parameter (0-based).
fn error_code_param(name: &str) -> Option<usize> {
    match name {
        "QRCVDTAQ" => Some(12),
        "QCLRDTAQ" => Some(5),
        _ => None,
    }
}

/// Call a data queue API. With an error code parameter whose bytes
/// provided is 8 or more, an error is returned in it (ERRC0100: bytes
/// available, exception ID) instead of as an escape message.
pub async fn call_api(
    client: &Client,
    caller: &Caller,
    name: &str,
    args: &mut dyn ApiArgs,
) -> Result<()> {
    let name = name.to_ascii_uppercase();
    let r = match name.as_str() {
        "QSNDDTAQ" => send(client, caller, args).await,
        "QRCVDTAQ" => receive(client, caller, args).await,
        "QCLRDTAQ" => clear(client, caller, args).await,
        "QMHQRDQD" => describe(client, caller, args).await,
        _ => bail!("{name} is not a data queue API."),
    };
    error_code(args, error_code_param(&name), r)
}

/// An API's result through its error code parameter (ERRC0100), when
/// the caller passed one with bytes provided of 8 or more: bytes
/// available 0 for success, else 15 (or less) and the exception ID at
/// offset 8. Otherwise the error is an escape message, as without one.
pub fn error_code(args: &mut dyn ApiArgs, ec: Option<usize>, r: Result<()>) -> Result<()> {
    let Some(ec) = ec.filter(|i| *i < args.count()) else {
        return r;
    };
    let err = args.bytes(ec);
    let provided = if err.len() >= 4 {
        i32::from_be_bytes(err[..4].try_into().unwrap())
    } else {
        0
    };
    if provided < 8 {
        return r;
    }
    match r {
        Ok(()) => {
            args.set_bytes(ec, &[err[0], err[1], err[2], err[3], 0, 0, 0, 0]);
            Ok(())
        }
        Err(x) => {
            let id = crate::clpgm::error_id(&x);
            let mut out = err[..4].to_vec();
            out.extend_from_slice(&15i32.min(provided).to_be_bytes());
            let mut idb = e(&format!("{id:<7}"));
            idb.truncate(7);
            out.extend(idb.iter().take((provided as usize).saturating_sub(8)));
            args.set_bytes(ec, &out);
            Ok(())
        }
    }
}

fn text_param(args: &dyn ApiArgs, i: usize) -> String {
    a(&args.bytes(i)).trim().to_ascii_uppercase()
}

fn need(args: &dyn ApiArgs, n: usize) -> Result<()> {
    if args.count() < n {
        bail!(
            "CPF3C36: Number of parameters, {}, entered for this API was not valid.",
            args.count()
        );
    }
    Ok(())
}

/// The key a caller passed: `len` bytes of the key parameter, checked
/// against the queue (CPF9501, CPF9502, CPF9506, CPF9507).
fn key_of(q: &DataQueue, len: Option<i64>, key: &[u8]) -> Result<Vec<u8>> {
    let len = len.unwrap_or(0);
    if !(0..=MAX_KEY as i64).contains(&len) {
        bail!("CPF9507: Key length {len} is not valid.");
    }
    if q.seq != 'K' {
        if len != 0 {
            bail!(
                "CPF9502: Data queue {} in {} is not keyed; the key length must be 0.",
                q.name,
                q.library
            );
        }
        return Ok(vec![]);
    }
    if len == 0 {
        bail!(
            "CPF9501: Data queue {} in {} is keyed; a key is needed.",
            q.name,
            q.library
        );
    }
    if len != q.keylen as i64 {
        bail!(
            "CPF9506: Key length {len} is not the key length of data queue {} in {}, {}.",
            q.name,
            q.library,
            q.keylen
        );
    }
    let mut k = key.to_vec();
    k.resize(len as usize, 0x40);
    Ok(k)
}

/// The SQL comparison of a key search order (CPF9504 otherwise).
fn key_order(order: &str) -> Result<&'static str> {
    Ok(match order {
        "EQ" => "=",
        "NE" => "<>",
        "GT" => ">",
        "GE" => ">=",
        "LT" => "<",
        "LE" => "<=",
        o => bail!("CPF9504: Key search order {o:?} is not valid."),
    })
}

/// QSNDDTAQ (name, library, length of data, data [, length of key, key
/// [, asynchronous request [, data is from a journal entry]]]).
async fn send(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let q = find(client, &text_param(args, 1), &text_param(args, 0), caller).await?;
    authority::check(
        client,
        &caller.user,
        &q.library,
        &q.name,
        "*DTAQ",
        Aut::Change,
    )
    .await?;
    if args.count() > 6 {
        match text_param(args, 6).as_str() {
            "*NO" => {}
            // Only a DDM data queue sends asynchronously.
            "*YES" => bail!(
                "CPF9513: Asynchronous request must be *NO for data queue {}.",
                q.name
            ),
            v => bail!("CPF9512: Asynchronous request value {v} is not valid."),
        }
    }
    let journal = if args.count() > 7 {
        match text_param(args, 7).as_str() {
            "*NO" => false,
            "*YES" => true,
            v => bail!("CPF9520: Data is from a journal entry value {v} is not valid."),
        }
    } else {
        false
    };
    let len = args.num(2)?;
    if len < 0 || (len > q.maxlen as i64 && !journal) {
        bail!(
            "CPF2498: Length of data {len} is not valid for data queue {} in {} (largest entry {}).",
            q.name,
            q.library,
            q.maxlen
        );
    }
    let mut data = args.bytes(3);
    data.resize(len as usize, 0x40);
    let (klen, kbytes) = if args.count() > 4 {
        (Some(args.num(4)?), args.bytes(5))
    } else {
        (None, vec![])
    };
    let key = key_of(&q, klen, &kbytes)?;
    let (jname, juser, jnbr) = match &caller.job {
        Some(j) => (j.name.clone(), j.user.clone(), format!("{:06}", j.number)),
        None => (String::new(), caller.user.clone(), String::new()),
    };
    client
        .execute(
            "INSERT INTO as4tainr.data_queue_entries
                 (library, name, key, data, job_name, job_user, job_nbr, cur_user)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &q.library,
                &q.name,
                &key,
                &data,
                &jname,
                &juser,
                &jnbr,
                &caller.user,
            ],
        )
        .await?;
    Ok(())
}

/// A packed decimal of 7 digits (4 bytes), as the sender information
/// fields are.
fn packed7(n: i64) -> [u8; 4] {
    let s = format!("{:07}", n.unsigned_abs().min(9_999_999));
    let d: Vec<u8> = s.bytes().map(|c| c - b'0').collect();
    let sign = if n < 0 { 0x0D } else { 0x0F };
    [
        d[0] << 4 | d[1],
        d[2] << 4 | d[3],
        d[4] << 4 | d[5],
        d[6] << 4 | sign,
    ]
}

fn padded(s: &str, n: usize) -> Vec<u8> {
    let mut b = e(s);
    b.resize(n, 0x40);
    b
}

/// QRCVDTAQ (name, library, length of data, data, wait time [, key
/// order, length of key, key, length of sender information, sender
/// information [, remove message, size of data receiver, error code]]).
async fn receive(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 5)?;
    if args.count() > 5 && args.count() < 10 {
        bail!(
            "CPF3C36: Number of parameters, {}, entered for this API was not valid.",
            args.count()
        );
    }
    let q = find(client, &text_param(args, 1), &text_param(args, 0), caller).await?;
    authority::check(client, &caller.user, &q.library, &q.name, "*DTAQ", Aut::Use).await?;
    let wait = args.num(4)?;
    let (order, klen, kbytes, slen) = if args.count() >= 10 {
        (
            text_param(args, 5),
            Some(args.num(6)?),
            args.bytes(7),
            args.num(8)?,
        )
    } else {
        (String::new(), None, vec![], 0)
    };
    let key = key_of(&q, klen, &kbytes)?;
    let cmp = if key.is_empty() {
        "="
    } else {
        key_order(&order)?
    };
    if slen < 0 {
        bail!("CPF9505: Length of sender information {slen} is not valid.");
    }
    if (1..8).contains(&slen) {
        bail!("CPF9508: Length of sender information {slen} is too short; it must be 0 or at least 8.");
    }
    let remove = if args.count() > 10 {
        match text_param(args, 10).as_str() {
            "*YES" => true,
            "*NO" => false,
            v => bail!("CPF9515: Remove message value {v} is not valid."),
        }
    } else {
        true
    };
    let size = if args.count() > 11 {
        let n = args.num(11)?;
        if n < 0 {
            bail!("CPF9514: Size of data receiver {n} is not valid.");
        }
        Some(n as usize)
    } else {
        None
    };
    // Which entry comes first: by key (lowest first), else by the order
    // the queue was created with; equal keys in the order they were sent.
    let order_by = match q.seq {
        'L' => "id DESC",
        'K' => "key, id",
        _ => "id",
    };
    let filter = if key.is_empty() {
        String::new()
    } else {
        format!(" AND key {cmp} $3")
    };
    let pick = format!(
        "SELECT id FROM as4tainr.data_queue_entries
          WHERE library=$1 AND name=$2{filter} ORDER BY {order_by} LIMIT 1"
    );
    let cols = "data, key, job_name, job_user, job_nbr, cur_user";
    let sql = if remove {
        format!(
            "DELETE FROM as4tainr.data_queue_entries
              WHERE id = ({pick} FOR UPDATE SKIP LOCKED) RETURNING {cols}"
        )
    } else {
        format!("SELECT {cols} FROM as4tainr.data_queue_entries WHERE id = ({pick})")
    };
    // A wait of less than 0 is forever; 0, no wait; otherwise seconds.
    let deadline = (wait >= 0)
        .then(|| std::time::Instant::now() + std::time::Duration::from_secs(wait as u64));
    let row = loop {
        let r = if key.is_empty() {
            client.query_opt(&sql, &[&q.library, &q.name]).await?
        } else {
            client.query_opt(&sql, &[&q.library, &q.name, &key]).await?
        };
        if r.is_some() {
            break r;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // The queue may have been deleted while waiting.
        find(client, &q.library, &q.name, caller).await?;
    };
    let Some(row) = row else {
        args.set_num(2, 0)?;
        return Ok(());
    };
    let data: Vec<u8> = row.get(0);
    args.set_num(2, data.len() as i64)?;
    let n = size.map_or(data.len(), |s| s.min(data.len()));
    args.set_bytes(3, &data[..n]);
    if !key.is_empty() {
        let k: Vec<u8> = row.get(1);
        args.set_bytes(7, &k);
    }
    if slen >= 8 {
        let mut info = Vec::with_capacity(44);
        let avail: i64 = if q.senderid { 44 } else { 8 };
        let ret = avail.min(slen);
        info.extend_from_slice(&packed7(ret));
        info.extend_from_slice(&packed7(avail));
        if q.senderid {
            info.extend(padded(&row.get::<_, String>(2), 10));
            info.extend(padded(&row.get::<_, String>(3), 10));
            info.extend(padded(&row.get::<_, String>(4), 6));
            info.extend(padded(&row.get::<_, String>(5), 10));
        }
        info.truncate(slen as usize);
        args.set_bytes(9, &info);
    }
    Ok(())
}

/// QCLRDTAQ (name, library [, key order, length of key, key, error
/// code]): all entries, or a keyed queue's entries whose keys match.
async fn clear(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 2)?;
    if args.count() > 2 && args.count() < 6 {
        bail!(
            "CPF3C36: Number of parameters, {}, entered for this API was not valid.",
            args.count()
        );
    }
    let q = find(client, &text_param(args, 1), &text_param(args, 0), caller).await?;
    authority::check(
        client,
        &caller.user,
        &q.library,
        &q.name,
        "*DTAQ",
        Aut::Change,
    )
    .await?;
    let klen = if args.count() >= 6 { args.num(3)? } else { 0 };
    if klen == 0 {
        client
            .execute(
                "DELETE FROM as4tainr.data_queue_entries WHERE library=$1 AND name=$2",
                &[&q.library, &q.name],
            )
            .await?;
        return Ok(());
    }
    let key = key_of(&q, Some(klen), &args.bytes(4))?;
    let cmp = key_order(&text_param(args, 2))?;
    client
        .execute(
            &format!(
                "DELETE FROM as4tainr.data_queue_entries WHERE library=$1 AND name=$2 AND key {cmp} $3"
            ),
            &[&q.library, &q.name, &key],
        )
        .await?;
    Ok(())
}

/// How many entries fit when the queue is full: *MAX16MB or *MAX2GB of
/// storage over the entry size (as4tainr's estimate; IBM i counts its
/// own overhead).
fn max_entries(q: &DataQueue) -> i32 {
    let bytes: i64 = if q.size_max == -2 { 2 << 30 } else { 16 << 20 };
    (bytes / (q.maxlen + q.keylen).max(1) as i64).min(i32::MAX as i64) as i32
}

/// QMHQRDQD (receiver, length of receiver, format RDQD0100, qualified
/// data queue name).
async fn describe(client: &Client, caller: &Caller, args: &mut dyn ApiArgs) -> Result<()> {
    need(args, 4)?;
    let len = args.num(1)?;
    if len < 8 {
        bail!("CPF3C24: Length of the receiver variable is not valid.");
    }
    let format = text_param(args, 2);
    if format != "RDQD0100" {
        bail!("CPF3C21: Format name {format} is not valid.");
    }
    let qual = args.bytes(3);
    let qname = a(qual.get(..10).unwrap_or(&qual));
    let qlib = a(qual.get(10..20).unwrap_or(&[]));
    let q = find(client, &qlib, &qname, caller).await?;
    authority::check(client, &caller.user, &q.library, &q.name, "*DTAQ", Aut::Use).await?;
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.data_queue_entries WHERE library=$1 AND name=$2",
            &[&q.library, &q.name],
        )
        .await?
        .get(0);
    let yn = |b: bool| if b { "Y" } else { "N" };
    let mut r: Vec<u8> = Vec::with_capacity(120);
    r.extend_from_slice(&0i32.to_be_bytes()); // bytes returned (below)
    r.extend_from_slice(&120i32.to_be_bytes()); // bytes available
    r.extend_from_slice(&q.maxlen.to_be_bytes());
    r.extend_from_slice(&q.keylen.to_be_bytes());
    r.extend(e(&q.seq.to_string()));
    r.extend(e(yn(q.senderid)));
    r.extend(e(yn(q.force)));
    r.extend(padded(&q.text, 50));
    r.extend(e("0")); // a standard data queue (not DDM)
    r.extend(e(if q.autorcl { "1" } else { "0" }));
    r.extend(e("0")); // data queue locks are not enforced
    r.extend_from_slice(&(count as i32).to_be_bytes());
    r.extend_from_slice(&(count as i32).max(q.size_init).to_be_bytes());
    r.extend(padded(&q.name, 10));
    r.extend(padded(&q.library, 10));
    r.extend_from_slice(&max_entries(&q).to_be_bytes());
    r.extend_from_slice(&q.size_init.to_be_bytes());
    r.extend_from_slice(&q.size_max.to_be_bytes());
    r.extend_from_slice(&[0u8; 8]); // never reclaimed
    let ret = (len as usize).min(r.len());
    r[..4].copy_from_slice(&(ret as i32).to_be_bytes());
    args.set_bytes(0, &r[..ret]);
    Ok(())
}
