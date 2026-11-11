//! The database host server (as-database, port 8471): the server JTOpen's
//! JDBC driver (and so ACS's Run SQL Scripts) talks to (hostsrv.rs for
//! the common data stream).
//!
//! A connection starts with an exchange of random seeds (0x7001: the
//! server's seed and password level back) and a start-server request
//! (0x7002: the user and the password substitute; the user and the job
//! back). Then database requests: a header, a 20-byte template (the
//! operation result bitmap at 20, the return ORS handle at 28, the fill
//! ORS handle at 30, the RPB handle at 34, the parameter marker handle at
//! 36, the parameter count at 38) and LL/CP parameters (JTOpen
//! DBBaseRequestDS). Each is answered with reply 0x2800: the template's
//! error class (34) and return code (36), and the results the bitmap asks
//! for (JTOpen DBBaseReplyDS, DBReplyServerAttributes).
//!
//! Done: the set and retrieve attributes requests (0x1F80, 0x1F81) with
//! the server attributes, and SQL statements (server 0xE004): RPBs,
//! prepare and describe, execute, execute immediate, open, describe and
//! fetch, fetch, close, the last error's message, run on the
//! connection's job by the statement engine of mapepire.rs. Statements
//! with parameter markers, catalog requests (server 0xE006) and native
//! database requests (0xE005) are not done yet (docs/fidelity-gaps.md).

use crate::hostsrv::{ebcdic, job_name_param, param, read_ds, reply, seed, text37, Ds};
use anyhow::Result;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// The operation result bitmap's bits (JTOpen DBBaseRequestDS).
const ORS_SERVER_ATTRIBUTES: u32 = 0x0100_0000;

/// The database server's functional level: "V7R5M0" and a level the
/// JDBC driver reads as a number (it uses the server job's identifier
/// from level 5).
const FUNCTIONAL_LEVEL: &str = "V7R5M00005";

/// What a connection has agreed on.
#[derive(Debug, Clone)]
struct Attributes {
    date_format: u16,
    date_separator: u16,
    time_format: u16,
    time_separator: u16,
    decimal_separator: u16,
    naming: u16,
    ignore_decimal_errors: u16,
    commit_level: u16,
    package_size: u16,
    translate: u8,
    default_library: String,
}

impl Default for Attributes {
    fn default() -> Self {
        Attributes {
            date_format: 5,
            date_separator: 0,
            time_format: 2,
            time_separator: 0,
            decimal_separator: 0,
            naming: 0,
            ignore_decimal_errors: 0,
            commit_level: 0,
            package_size: 0,
            translate: 0,
            default_library: String::new(),
        }
    }
}

/// Serve one database server connection.
pub async fn serve(url: &str, mut sock: TcpStream) -> Result<()> {
    let client = crate::runtime::connect_db(url).await?;
    let env = crate::commands::Environment {
        database_url: url.to_string(),
    };
    let server_seed = seed();
    let mut client_seed: Vec<u8> = Vec::new();
    let mut user: Option<String> = None;
    let mut attrs = Attributes::default();
    let mut sql: Option<crate::mapepire::Server> = None;
    let mut stmts: std::collections::HashMap<u16, Stmt> = Default::default();
    let mut last_error: Option<SqlError> = None;
    while let Some(ds) = read_ds(&mut sock).await? {
        let corr = ds.correlation();
        trace(&ds, "in");
        let out = match ds.req_id() {
            0x7001 => {
                client_seed = ds.0.get(20..28).unwrap_or_default().to_vec();
                let mut t = 0u32.to_be_bytes().to_vec();
                t.extend(server_seed);
                // Byte 5 of the header: the password level (JTOpen
                // AS400XChgRandSeedReplyDS getServerAttributes).
                reply(0, 2, 0xE004, corr, 0xF001, &t, &[])
            }
            0x7002 => {
                let mut name = String::new();
                let mut substitute: Vec<u8> = Vec::new();
                for (cp, d) in ds.params(22) {
                    match cp {
                        0x1104 => name = text37(d),
                        0x1105 => substitute = d.to_vec(),
                        _ => {}
                    }
                }
                let rc = match crate::users::host_signon(
                    &client,
                    &name,
                    &client_seed,
                    &server_seed,
                    &substitute,
                )
                .await
                {
                    Ok(p) => {
                        let curlib = (p.curlib != "*CRTDFT").then(|| p.curlib.clone());
                        user = Some(p.name.clone());
                        sql = Some(crate::mapepire::Server::new(crate::commands::Job {
                            client: &client,
                            env: &env,
                            user: p.name,
                            curlib,
                            libl: crate::commands::USER_LIBRARY_LIST
                                .iter()
                                .map(|l| l.to_string())
                                .collect(),
                            display: None,
                            id: None,
                            calls: Default::default(),
                        }));
                        0u32
                    }
                    Err(rc) => rc,
                };
                let mut p = Vec::new();
                if rc == 0 {
                    let mut u = 37u32.to_be_bytes().to_vec();
                    u.extend(ebcdic(&name.to_ascii_uppercase(), 10));
                    p.extend(param(0x1104, &u));
                    p.extend(job_name_param("QZDASOINIT", "QUSER"));
                }
                reply(0, 0, 0xE004, corr, 0xF002, &rc.to_be_bytes(), &p)
            }
            _ if user.is_none() => {
                // A database request before the start-server request.
                break;
            }
            0x1F80 | 0x1F81 => {
                if ds.req_id() == 0x1F80 {
                    set_attributes(&ds, &mut attrs);
                    if let Some(s) = sql.as_mut() {
                        if !attrs.default_library.is_empty() {
                            s.set_schema(Some(attrs.default_library.clone()));
                        }
                    }
                }
                let mut p = Vec::new();
                if ds.u32(20) & ORS_SERVER_ATTRIBUTES != 0 {
                    p.extend(server_attributes(&attrs, user.as_deref().unwrap_or("")));
                }
                db_reply(&ds, 0, 0, &p)
            }
            // A function the server does not know: error class 7, return
            // code -201 (JTOpen AS400JDBCConnectionImpl isValid sends
            // function 0x0000 and takes that answer as a live connection).
            0x0000 => db_reply(&ds, 7, -201, &[]),
            id if ds.server_id() == 0xE004 => {
                let srv = sql.as_mut().expect("signed on");
                sql_request(srv, &ds, id, &mut stmts, &mut last_error).await
            }
            _ => db_reply(&ds, 0, 0, &[]),
        };
        trace(&Ds(out.clone()), "out");
        sock.write_all(&out).await?;
        sock.flush().await?;
    }
    Ok(())
}

/// The most rows one reply carries; the client fetches the rest.
const BLOCK_ROWS: usize = 2048;

/// A statement of an RPB (request parameter block): its text, and the
/// rows of its open cursor.
#[derive(Default)]
struct Stmt {
    sql: String,
    /// The query's result: column descriptions and rows not yet sent.
    result: Option<Cursor>,
}

struct Cursor {
    cols: Vec<Col>,
    rows: std::collections::VecDeque<Vec<Option<String>>>,
}

/// The last SQL error, for the client's request for its message (0x1F00
/// send results set, as JTOpen JDError getReason asks).
struct SqlError {
    id: String,
    text: String,
    state: String,
    code: i32,
}

/// The SQL requests (server 0xE004, JTOpen DBSQLRequestDS, DBSQLRPBDS,
/// DBSQLResultSetDS).
async fn sql_request(
    srv: &mut crate::mapepire::Server<'_>,
    ds: &Ds,
    id: u16,
    stmts: &mut std::collections::HashMap<u16, Stmt>,
    last: &mut Option<SqlError>,
) -> Vec<u8> {
    let rpb = ds.u16(34);
    let ors = ds.u32(20);
    let tl = ds.template_len();
    let text = statement_text(ds, 20 + tl);
    match id {
        // Create, change, reset and delete the RPB.
        0x1D00 | 0x1D01 | 0x1D03 | 0x1D04 => {
            let s = stmts.entry(rpb).or_default();
            if let Some(t) = text {
                s.sql = t;
            }
            db_reply(ds, 0, 0, &[])
        }
        0x1D02 => {
            stmts.remove(&rpb);
            db_reply(ds, 0, 0, &[])
        }
        // Send results set: the last error's message and SQLCA.
        0x1F00 => {
            let mut p = Vec::new();
            if let Some(e) = last.as_ref() {
                let mut m = 37u16.to_be_bytes().to_vec();
                m.extend(ebcdic(&e.id, 7));
                p.extend(param(0x3801, &m));
                let t = crate::ifs::codec::bytes(37, &e.text);
                let mut m = 37u16.to_be_bytes().to_vec();
                m.extend((t.len() as u16).to_be_bytes());
                m.extend(t);
                p.extend(param(0x3802, &m));
                // No second-level text (help texts are IBM's).
                p.extend(param(0x3803, &[0, 37, 0, 0]));
                p.extend(sqlca(e.code, &e.state, 0));
            }
            db_reply(ds, 0, 0, &p)
        }
        0x1F01 => db_reply(ds, 0, 0, &[]),
        // Prepare (0x1800), prepare and describe (0x1803): a query is run
        // now and its rows kept for the open, its columns described.
        0x1800 | 0x1803 => {
            let sql = text.unwrap_or_default();
            let s = stmts.entry(rpb).or_default();
            s.sql = sql.clone();
            s.result = None;
            if !is_query(&sql) {
                return db_reply(ds, 0, 0, &sqlca(0, "00000", 0));
            }
            match run(srv, &sql).await {
                Ok(Ran::Rows(c)) => {
                    let mut p = Vec::new();
                    if ors & ORS_DATA_FORMAT != 0 {
                        p.extend(data_format(&c.cols));
                    }
                    p.extend(sqlca(0, "00000", 0));
                    s.result = Some(c);
                    db_reply(ds, 0, 0, &p)
                }
                Ok(Ran::Count(_)) => db_reply(ds, 0, 0, &sqlca(0, "00000", 0)),
                Err(e) => fail(ds, e, last),
            }
        }
        // Execute (0x1805), execute immediate (0x1806): the statement run,
        // the rows it changed in the SQLCA (errd 3).
        0x1805 | 0x1806 => {
            let sql = match (id, text) {
                (0x1806, Some(t)) => t,
                _ => stmts.get(&rpb).map(|s| s.sql.clone()).unwrap_or_default(),
            };
            match run(srv, &sql).await {
                Ok(Ran::Count(n)) => db_reply(ds, 0, 0, &sqlca(0, "00000", n as i32)),
                Ok(Ran::Rows(_)) => db_reply(ds, 0, 0, &sqlca(0, "00000", 0)),
                Err(e) => fail(ds, e, last),
            }
        }
        // Open and describe (0x1804), open, describe and fetch (0x180E),
        // execute or open and describe (0x1812): the cursor over the
        // prepared query's rows (run again when they were sent before).
        0x1804 | 0x180E | 0x1812 => {
            let s = stmts.entry(rpb).or_default();
            if let Some(t) = text {
                s.sql = t;
                s.result = None;
            }
            if s.result.is_none() {
                let sql = s.sql.clone();
                match run(srv, &sql).await {
                    Ok(Ran::Rows(c)) => s.result = Some(c),
                    Ok(Ran::Count(n)) => return db_reply(ds, 0, 0, &sqlca(0, "00000", n as i32)),
                    Err(e) => return fail(ds, e, last),
                }
            }
            let c = s.result.as_mut().expect("result");
            let mut p = Vec::new();
            if ors & ORS_DATA_FORMAT != 0 {
                p.extend(data_format(&c.cols));
            }
            let (class, rc) = if id == 0x180E || ors & ORS_RESULT_DATA != 0 {
                let (data, more) = result_data(c);
                p.extend(data);
                if more {
                    (0, 0)
                } else {
                    (2, 701)
                }
            } else {
                (0, 0)
            };
            if ors & ORS_SQLCA != 0 {
                p.extend(sqlca(0, "00000", 0));
            }
            db_reply(ds, class, rc, &p)
        }
        // Fetch: the next block, end of data (class 2, 701) at the last.
        0x180B => {
            let Some(c) = stmts.get_mut(&rpb).and_then(|s| s.result.as_mut()) else {
                return db_reply(ds, 1, 100, &[]);
            };
            if c.rows.is_empty() {
                return db_reply(ds, 1, 100, &[]);
            }
            let (data, more) = result_data(c);
            db_reply(
                ds,
                if more { 0 } else { 2 },
                if more { 0 } else { 701 },
                &data,
            )
        }
        // Close the cursor.
        0x180A => {
            if let Some(s) = stmts.get_mut(&rpb) {
                s.result = None;
            }
            db_reply(ds, 0, 0, &[])
        }
        // Commit, rollback: statements run committed (autocommit).
        _ => db_reply(ds, 0, 0, &sqlca(0, "00000", 0)),
    }
}

const ORS_DATA_FORMAT: u32 = 0x0800_0000;
const ORS_RESULT_DATA: u32 = 0x0400_0000;
const ORS_SQLCA: u32 = 0x0200_0000;

enum Ran {
    Rows(Cursor),
    Count(u64),
}

/// Run a statement on the connection's job.
async fn run(srv: &mut crate::mapepire::Server<'_>, sql: &str) -> Result<Ran> {
    Ok(match srv.statement(sql).await? {
        crate::mapepire::Done::Rows(r) => Ran::Rows(cursor(r)),
        crate::mapepire::Done::Call(Some(r), _) => Ran::Rows(cursor(r)),
        crate::mapepire::Done::Call(None, _) => Ran::Count(0),
        crate::mapepire::Done::Count(n) => Ran::Count(n),
    })
}

/// An error as the reply reports it: error class 1 (SQL), the SQLCODE as
/// return code, an SQLCA; kept for the client's request for its text.
fn fail(ds: &Ds, e: anyhow::Error, last: &mut Option<SqlError>) -> Vec<u8> {
    let e = crate::mapepire::sql_failure(&e);
    let (text, sql) = match e.downcast_ref::<crate::mapepire::Failure>() {
        Some(f) => (f.text.clone(), f.sql.clone()),
        None => (format!("{e:#}"), None),
    };
    let (state, code) = sql.unwrap_or_else(|| ("58004".into(), -901));
    let id = format!("SQL{:04}", code.unsigned_abs());
    let p = sqlca(code as i32, &state, 0);
    *last = Some(SqlError {
        id,
        text,
        state,
        code: code as i32,
    });
    db_reply(ds, 1, code as i32, &p)
}

/// The statement text of a request: CP 0x3831 (CCSID, 4-byte length,
/// text) or 0x3807 (CCSID, 2-byte length, text), in its CCSID (13488 and
/// 1200 UTF-16, 1208 UTF-8, else EBCDIC).
fn statement_text(ds: &Ds, from: usize) -> Option<String> {
    for (cp, d) in ds.params(from) {
        let (ccsid, body) = match cp {
            0x3831 if d.len() >= 6 => (u16::from_be_bytes([d[0], d[1]]), &d[6..]),
            0x3807 if d.len() >= 4 => (u16::from_be_bytes([d[0], d[1]]), &d[4..]),
            _ => continue,
        };
        return Some(decode(ccsid, body));
    }
    None
}

fn decode(ccsid: u16, b: &[u8]) -> String {
    match ccsid {
        13488 | 1200 => {
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        1208 => String::from_utf8_lossy(b).to_string(),
        c => crate::ifs::codec::text(c as i32, b),
    }
}

fn is_query(sql: &str) -> bool {
    let w = sql
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(')
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(w.as_str(), "SELECT" | "WITH" | "VALUES")
}

/// A result column as the data format describes it.
#[derive(Debug, Clone)]
struct Col {
    name: String,
    /// The SQL type (nullable: +1).
    sql_type: u16,
    /// Bytes in the row.
    length: usize,
    precision: u16,
    scale: u16,
    ccsid: u16,
}

/// The rows of a statement's result as a cursor: each column's type
/// (Db2 SQL type codes: 496 INTEGER, 500 SMALLINT, 492 BIGINT, 480
/// floating point, 484 DECIMAL packed, 488 NUMERIC zoned, 452 CHAR, 448
/// VARCHAR, 912 BINARY, 908 VARBINARY; nullable +1), its length from its
/// type and its longest value.
fn cursor(r: crate::mapepire::Results) -> Cursor {
    let rows: std::collections::VecDeque<Vec<Option<String>>> = r.rows;
    let cols = r
        .columns
        .iter()
        .zip(&r.types)
        .enumerate()
        .map(|(i, (name, t))| {
            let longest = rows
                .iter()
                .filter_map(|row| row.get(i).cloned().flatten())
                .map(|v| shown(t, &v).len())
                .max()
                .unwrap_or(0);
            let (ty, p, s, _) = crate::mapepire::column_type(t, longest);
            let (sql_type, length, precision, scale, ccsid) = match ty.as_str() {
                "SMALLINT" => (500, 2, 5, 0, 0),
                "INTEGER" => (496, 4, 10, 0, 0),
                "BIGINT" => (492, 8, 19, 0, 0),
                "REAL" | "DOUBLE" => (480, 8, 53, 0, 0),
                "DECIMAL" => (484, (p as usize) / 2 + 1, p as u16, s as u16, 0),
                "NUMERIC" => (488, p as usize, p as u16, s as u16, 0),
                "CHAR" | "NCHAR" => {
                    let n = (p as usize).max(longest).max(1);
                    (452, n, n as u16, 0, 1208)
                }
                "BINARY" => (912, (p as usize).max(longest).max(1), p as u16, 0, 65535),
                "VARBINARY" | "VARCHAR () FOR BIT DATA" => {
                    let n = (p as usize).max(longest).max(1);
                    (908, n + 2, n as u16, 0, 65535)
                }
                _ => {
                    let n = (p as usize).max(longest).clamp(1, 32739);
                    (448, n + 2, n as u16, 0, 1208)
                }
            };
            Col {
                name: name.clone(),
                sql_type: sql_type + 1,
                length,
                precision,
                scale,
                ccsid,
            }
        })
        .collect();
    Cursor { cols, rows }
}

/// A value as Db2 shows it: dates ISO, times hh.mm.ss, timestamps
/// yyyy-mm-dd-hh.mm.ss.ffffff.
fn shown(t: &crate::sql::ColType, v: &str) -> String {
    match t.pg.as_str() {
        "time" => v.get(..8).unwrap_or(v).replace(':', "."),
        "timestamp" | "timestamptz" if v.len() >= 19 => {
            let (date, rest) = v.split_at(10);
            let time = rest[1..9].replace(':', ".");
            let frac: String = rest
                .get(10..)
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            format!("{date}-{time}.{frac:0<6}")
        }
        "bool" => (if v == "t" { "1" } else { "0" }).to_string(),
        _ => v.to_string(),
    }
}

/// The data format (CP 0x3805, JTOpen DBOriginalDataFormat): a
/// consistency token (4), the number of fields (2), the record size (2),
/// then 54 bytes for each field: its description's length, SQL type,
/// length, scale, precision, CCSID, parameter type, reserved, the name's
/// length, its CCSID and the name (30 bytes).
fn data_format(cols: &[Col]) -> Vec<u8> {
    let size: usize = cols.iter().map(|c| c.length).sum();
    let mut d = 1u32.to_be_bytes().to_vec();
    d.extend((cols.len() as u16).to_be_bytes());
    d.extend((size as u16).to_be_bytes());
    for c in cols {
        let mut f = Vec::with_capacity(54);
        f.extend(54u16.to_be_bytes());
        f.extend(c.sql_type.to_be_bytes());
        f.extend((c.length as u16).to_be_bytes());
        f.extend(c.scale.to_be_bytes());
        f.extend(c.precision.to_be_bytes());
        f.extend(c.ccsid.to_be_bytes());
        f.push(0); // parameter type
        f.extend([0u8; 7]);
        let mut name = c.name.as_bytes().to_vec();
        name.truncate(30);
        f.extend((name.len() as u16).to_be_bytes());
        f.extend(1208u16.to_be_bytes());
        name.resize(30, 0);
        f.extend(name);
        d.extend(f);
    }
    param(0x3805, &d)
}

/// The next block of rows (CP 0x3806, JTOpen DBOriginalData): a
/// consistency token (4), the row count (4), the column count (2), the
/// indicator size (2), the row size (2), each row's indicators (0, or
/// -1 for null), then the rows. Whether rows remain.
fn result_data(c: &mut Cursor) -> (Vec<u8>, bool) {
    let n = c.rows.len().min(BLOCK_ROWS);
    let row_size: usize = c.cols.iter().map(|c| c.length).sum();
    let mut ind = Vec::with_capacity(n * c.cols.len() * 2);
    let mut data = Vec::with_capacity(n * row_size);
    for _ in 0..n {
        let row = c.rows.pop_front().unwrap_or_default();
        for (i, col) in c.cols.iter().enumerate() {
            let v = row.get(i).cloned().flatten();
            ind.extend(if v.is_some() { 0i16 } else { -1i16 }.to_be_bytes());
            data.extend(field_bytes(col, v.as_deref()));
        }
    }
    let mut d = 1u32.to_be_bytes().to_vec();
    d.extend((n as u32).to_be_bytes());
    d.extend((c.cols.len() as u16).to_be_bytes());
    d.extend(2u16.to_be_bytes());
    d.extend((row_size as u16).to_be_bytes());
    d.extend(ind);
    d.extend(data);
    (param(0x3806, &d), !c.rows.is_empty())
}

/// One value in its column's form (blanks or zeros for null).
fn field_bytes(c: &Col, v: Option<&str>) -> Vec<u8> {
    let v = v.unwrap_or("");
    let mut b = match c.sql_type & !1 {
        500 => v.trim().parse::<i16>().unwrap_or(0).to_be_bytes().to_vec(),
        496 => v.trim().parse::<i32>().unwrap_or(0).to_be_bytes().to_vec(),
        492 => v.trim().parse::<i64>().unwrap_or(0).to_be_bytes().to_vec(),
        480 => v
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
            .to_be_bytes()
            .to_vec(),
        484 => packed(v, c.precision as usize, c.scale as usize),
        488 => zoned(v, c.precision as usize, c.scale as usize),
        452 => {
            let mut b = v.as_bytes().to_vec();
            b.truncate(c.length);
            b
        }
        912 => decode_hex(v),
        908 | 448 => {
            let mut body = if c.sql_type & !1 == 908 {
                decode_hex(v)
            } else {
                v.as_bytes().to_vec()
            };
            body.truncate(c.length - 2);
            let mut b = (body.len() as u16).to_be_bytes().to_vec();
            b.extend(body);
            b
        }
        _ => v.as_bytes().to_vec(),
    };
    let pad = if c.sql_type & !1 == 452 { b' ' } else { 0 };
    b.resize(c.length, pad);
    b
}

/// A PostgreSQL bytea value (\x hex) as its bytes.
fn decode_hex(v: &str) -> Vec<u8> {
    let h = v.strip_prefix("\\x").unwrap_or(v);
    (0..h.len() / 2)
        .filter_map(|i| u8::from_str_radix(h.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

/// A decimal's digits for precision p and scale s, and its sign.
fn digits(v: &str, p: usize, s: usize) -> (Vec<u8>, bool) {
    let v = v.trim();
    let neg = v.starts_with('-');
    let v = v.trim_start_matches(['-', '+']);
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    let mut f: String = frac.chars().take(s).collect();
    while f.len() < s {
        f.push('0');
    }
    let all = format!("{int}{f}");
    let all: Vec<u8> = all
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let mut d = vec![0u8; p.saturating_sub(all.len())];
    d.extend(all.iter().skip(all.len().saturating_sub(p)));
    (d, neg)
}

/// Packed decimal: two digits a byte, the sign (C or D) in the last half.
fn packed(v: &str, p: usize, s: usize) -> Vec<u8> {
    let n = p / 2 + 1;
    let (d, neg) = digits(v, n * 2 - 1, s);
    let mut out = Vec::with_capacity(n);
    let mut nibbles = d;
    nibbles.push(if neg { 0xD } else { 0xF });
    for pair in nibbles.chunks(2) {
        out.push((pair[0] << 4) | pair.get(1).copied().unwrap_or(0));
    }
    out
}

/// Zoned decimal: a digit a byte (zone F), the sign in the last zone.
fn zoned(v: &str, p: usize, s: usize) -> Vec<u8> {
    let (d, neg) = digits(v, p, s);
    let mut out: Vec<u8> = d.iter().map(|x| 0xF0 | x).collect();
    if let (true, Some(last)) = (neg, out.last_mut()) {
        *last = 0xD0 | (*last & 0x0F);
    }
    out
}

/// An SQLCA (CP 0x3807): 136 bytes as Db2 lays it out (SQLCAID, SQLCABC,
/// SQLCODE at 12, SQLERRMC, SQLERRP, SQLERRD at 96 (rows changed in the
/// third), SQLWARN at 120, SQLSTATE at 131 in EBCDIC).
fn sqlca(code: i32, state: &str, rows: i32) -> Vec<u8> {
    let mut d = ebcdic("SQLCA", 8);
    d.extend(136u32.to_be_bytes());
    d.extend(code.to_be_bytes());
    d.extend([0u8; 2]); // SQLERRML
    d.extend([0u8; 70]); // SQLERRMC
    d.extend(ebcdic("QSQ", 8)); // SQLERRP
    for i in 0..6 {
        d.extend(if i == 2 { rows } else { 0 }.to_be_bytes());
    }
    d.extend(ebcdic("", 11)); // SQLWARN
    d.extend(ebcdic(state, 5));
    param(0x3807, &d)
}

/// The parser options a set attributes request asks for.
fn set_attributes(ds: &Ds, a: &mut Attributes) {
    let tl = ds.template_len();
    for (cp, d) in ds.params(20 + tl) {
        let v16 = || d.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        match cp {
            0x3807 => a.date_format = v16().unwrap_or(a.date_format),
            0x3808 => a.date_separator = v16().unwrap_or(a.date_separator),
            0x3809 => a.time_format = v16().unwrap_or(a.time_format),
            0x380A => a.time_separator = v16().unwrap_or(a.time_separator),
            0x380B => a.decimal_separator = v16().unwrap_or(a.decimal_separator),
            0x380C => a.naming = v16().unwrap_or(a.naming),
            0x380D => a.ignore_decimal_errors = v16().unwrap_or(a.ignore_decimal_errors),
            0x380E => a.commit_level = v16().unwrap_or(a.commit_level),
            0x3806 => a.package_size = v16().unwrap_or(a.package_size),
            0x3805 => a.translate = d.first().copied().unwrap_or(a.translate),
            // Default library: CCSID (2), length (2), the name.
            0x380F if d.len() > 4 => a.default_library = text37(&d[4..]),
            _ => {}
        }
    }
}

/// A database reply (0x2800) to request `ds`: its template with the
/// error class and return code, then the parameters.
fn db_reply(ds: &Ds, error_class: u16, rc: i32, params: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 20];
    // The return data function ID: the request's.
    t[10..12].copy_from_slice(&ds.req_id().to_be_bytes());
    t[14..16].copy_from_slice(&error_class.to_be_bytes());
    t[16..20].copy_from_slice(&rc.to_be_bytes());
    reply(0, 0, 0xE004, ds.correlation(), 0x2800, &t, params)
}

/// The server attributes (CP 0x3804): CCSID 37 for its text, then the
/// layout of JTOpen DBReplyServerAttributes (offsets from the data).
fn server_attributes(a: &Attributes, user: &str) -> Vec<u8> {
    let mut d = Vec::with_capacity(130);
    for v in [
        a.date_format,
        a.date_separator,
        a.time_format,
        a.time_separator,
        a.decimal_separator,
        a.naming,
        a.ignore_decimal_errors,
        a.commit_level,
        a.package_size,
    ] {
        d.extend(v.to_be_bytes());
    }
    d.push(a.translate); // 18
    d.extend(37u16.to_be_bytes()); // 19: server CCSID
    d.extend(0u16.to_be_bytes()); // 21: NLSS value
    d.extend(ebcdic("ENU", 3)); // 23: language ID
    d.extend(ebcdic("*HEX", 10)); // 26: language table
    d.extend(ebcdic("", 10)); // 36: its library
    d.extend(ebcdic("2924", 4)); // 46: language feature code
    d.extend(ebcdic(FUNCTIONAL_LEVEL, 10)); // 50
    d.extend(ebcdic(&crate::report::system_name(), 18)); // 60: RDB name
    let lib = if a.default_library.is_empty() {
        "QGPL"
    } else {
        &a.default_library
    };
    d.extend(ebcdic(lib, 10)); // 78: default SQL library
    let number = format!("{:06}", std::process::id() % 1_000_000);
    let job = format!("{:<10}{:<10}{number}", "QZDASOINIT", "QUSER");
    d.extend(ebcdic(&job, 26)); // 88: job identifier
    d.extend((lib.len() as u16).to_be_bytes()); // 114: default schema length
    d.extend(ebcdic(lib, lib.len())); // 116
    let _ = user;
    let mut v = 37u16.to_be_bytes().to_vec();
    v.extend(d);
    param(0x3804, &v)
}

/// With AS4TAINR_HOSTDB_TRACE naming a file, each request and reply
/// appended to it in hex ("in"/"out", the request or reply ID).
fn trace(ds: &Ds, dir: &str) {
    let Ok(path) = std::env::var("AS4TAINR_HOSTDB_TRACE") else {
        return;
    };
    let hex: String = ds.0.iter().map(|b| format!("{b:02X}")).collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{dir} {:04X} {hex}", ds.req_id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_packed_and_zoned() {
        // Packed: two digits a byte, sign F (or C) positive, D negative,
        // p/2+1 bytes; zoned: zone F, the sign in the last digit's zone
        // (IBM i ILE RPG Reference, "Packed-Decimal Format", "Zoned-
        // Decimal Format").
        assert_eq!(packed("-12.34", 7, 2), vec![0x00, 0x01, 0x23, 0x4D]);
        assert_eq!(packed("5", 3, 0), vec![0x00, 0x5F]);
        assert_eq!(packed("1.5", 4, 2), vec![0x00, 0x15, 0x0F]);
        assert_eq!(zoned("-4.5", 5, 1), vec![0xF0, 0xF0, 0xF0, 0xF4, 0xD5]);
        assert_eq!(zoned("12", 3, 0), vec![0xF0, 0xF1, 0xF2]);
    }

    #[test]
    fn statements_that_are_queries() {
        assert!(is_query("  select 1 from sysibm.sysdummy1"));
        assert!(is_query("(SELECT 1 FROM X)"));
        assert!(is_query("WITH A AS (SELECT 1 FROM X) SELECT * FROM A"));
        assert!(is_query("VALUES CURRENT USER"));
        assert!(!is_query("INSERT INTO X VALUES 1"));
        assert_eq!(decode(13488, &[0, b'A', 0, b'b']), "Ab");
        assert_eq!(decode(37, &[0xC1, 0x82]), "Ab");
    }

    #[test]
    fn sqlca_layout() {
        // DBReplySQLCA: SQLCODE at 12, SQLERRD at 96 (the third at 104),
        // SQLSTATE at 131, after the 6-byte parameter header.
        let p = sqlca(-204, "42704", 3);
        let d = &p[6..];
        assert_eq!(d.len(), 136);
        assert_eq!(i32::from_be_bytes([d[12], d[13], d[14], d[15]]), -204);
        assert_eq!(i32::from_be_bytes([d[104], d[105], d[106], d[107]]), 3);
        assert_eq!(&d[131..136], &ebcdic("42704", 5)[..]);
    }

    #[test]
    fn server_attributes_where_jtopen_reads_them() {
        // DBReplyServerAttributes: the data starts at the parameter's
        // offset + 8; server CCSID at 19, functional level at 50 (its
        // VRM digits at 51, 53, 55), default library at 78.
        let p = server_attributes(&Attributes::default(), "SSHU");
        let d = &p[8..];
        assert_eq!(u16::from_be_bytes([d[19], d[20]]), 37);
        assert_eq!(text37(&d[50..60]), "V7R5M00005");
        assert_eq!(d[51] & 0x0F, 7);
        assert_eq!(d[53] & 0x0F, 5);
        assert_eq!(text37(&d[78..88]), "QGPL");
        assert_eq!(u16::from_be_bytes([d[114], d[115]]), 4);
    }
}
