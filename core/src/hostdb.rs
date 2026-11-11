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
        let ds = decompressed(ds);
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
                sql_request(srv, &ds, id, &mut stmts, &mut last_error, Dt::of(&attrs)).await
            }
            _ => db_reply(&ds, 0, 0, &[]),
        };
        trace(&Ds(out.clone()), "out");
        sock.write_all(&out).await?;
        sock.flush().await?;
    }
    Ok(())
}

/// A request as sent, its data run-length compressed (JTOpen
/// DBBaseRequestDS with the request RLE compression bit, 0x00080000, of
/// the bitmap): after the header and template (40 bytes) comes CP
/// 0x3832 with the data's length and the compressed data, where escape
/// 0x1B, 0x1B stands for 0x1B and 0x1B, two bytes, a 2-byte count for
/// the two bytes that many times (JTOpen DataStreamCompression).
fn decompressed(ds: Ds) -> Ds {
    let v = &ds.0;
    if v.len() < 50
        || ds.server_id() != 0xE004
        || ds.u32(20) & 0x0008_0000 == 0
        || ds.u16(44) != 0x3832
    {
        return ds;
    }
    let full = ds.u32(46) as usize;
    let mut out = Vec::with_capacity(40 + full);
    out.extend(&v[..40]);
    let mut i = 50;
    while i < v.len() {
        if v[i] != 0x1B {
            out.push(v[i]);
            i += 1;
        } else if v.get(i + 1) == Some(&0x1B) {
            out.push(0x1B);
            i += 2;
        } else if i + 4 < v.len() {
            let count = u16::from_be_bytes([v[i + 3], v[i + 4]]) as usize;
            for _ in 0..count {
                out.extend([v[i + 1], v[i + 2]]);
            }
            i += 5;
        } else {
            break;
        }
    }
    let len = out.len() as u32;
    out[..4].copy_from_slice(&len.to_be_bytes());
    Ds(out)
}

/// The most rows one reply carries, and about the most bytes; the
/// client fetches the rest.
const BLOCK_ROWS: usize = 2048;
const BLOCK_BYTES: usize = 1 << 20;

/// A statement of an RPB (request parameter block): its text, and the
/// rows of its open cursor.
#[derive(Default)]
struct Stmt {
    sql: String,
    /// The query's result: column descriptions and rows not yet sent.
    result: Option<Cursor>,
    /// The columns of a query with parameter markers, described when
    /// it was prepared (its rows come when it is opened with values).
    described: Option<Vec<Col>>,
    /// Its parameter markers as described, or as the client's
    /// descriptor (change descriptor) has them since.
    params: Vec<Col>,
    /// A CALL's markers: the parameter each is the argument for.
    marker_args: Vec<Option<usize>>,
    /// A CALL's result sets after the open one, opened in turn (JTOpen
    /// AS400JDBCStatement getMoreResults: close, then open and
    /// describe).
    more: std::collections::VecDeque<Cursor>,
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
    dt: Dt,
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
        // Change descriptor (JTOpen DBSQLDescriptorDS): the parameter
        // markers' format as the client lays out their values.
        0x1E00 => {
            let cols = ds.params(20 + tl).into_iter().find_map(|(cp, d)| match cp {
                0x381E => Some(descriptor(d, true)),
                0x3801 => Some(descriptor(d, false)),
                _ => None,
            });
            if let Some(mut cols) = cols {
                // The directions stay as the prepare described them.
                let s = stmts.entry(rpb).or_default();
                for (c, old) in cols.iter_mut().zip(&s.params) {
                    c.ptype = old.ptype;
                }
                s.params = cols;
            }
            db_reply(ds, 0, 0, &[])
        }
        0x1E01 => db_reply(ds, 0, 0, &[]),
        // Prepare (0x1800), prepare and describe (0x1803): a query is run
        // now and its rows kept for the open, its columns described.
        0x1800 | 0x1803 => {
            let sql = text.unwrap_or_default();
            let s = stmts.entry(rpb).or_default();
            s.sql = sql.clone();
            s.result = None;
            s.described = None;
            s.more.clear();
            let markers = crate::mapepire::marker_positions(&sql).len();
            (s.params, s.marker_args) = if markers > 0 {
                match call_markers(srv, &sql).await {
                    Some(x) => x,
                    None => (marker_cols(srv, &sql).await, Vec::new()),
                }
            } else {
                Default::default()
            };
            let mut p = Vec::new();
            if markers > 0 && ors & ORS_PARAMETER_MARKER_FORMAT != 0 {
                p.extend(format(0x380D, &s.params));
            }
            if !is_query(&sql) {
                p.extend(sqlca(0, "00000", 0));
                return db_reply(ds, 0, 0, &p);
            }
            // A query with markers is described by running it with nulls
            // for them; its rows come when it is opened with its values.
            let probe = if markers > 0 {
                match crate::mapepire::bind(&sql, &vec!["NULL".to_string(); markers]) {
                    Ok(q) => q,
                    Err(e) => return fail(ds, e, last),
                }
            } else {
                sql.clone()
            };
            match run(srv, &probe, dt, markers > 0).await {
                Ok(Ran::Rows(mut c)) => {
                    if markers > 0 {
                        c.rows.clear();
                        s.described = Some(c.cols.clone());
                    }
                    if ors & ORS_DATA_FORMAT != 0 {
                        p.extend(format(0x380C, &c.cols));
                    }
                    p.extend(sqlca(0, "00000", 0));
                    if markers == 0 {
                        s.result = Some(c);
                    }
                    db_reply(ds, 0, 0, &p)
                }
                Ok(Ran::Count(_)) | Ok(Ran::Call(..)) => {
                    p.extend(sqlca(0, "00000", 0));
                    db_reply(ds, 0, 0, &p)
                }
                Err(e) => fail(ds, e, last),
            }
        }
        // Execute (0x1805), execute immediate (0x1806): the statement run,
        // the rows it changed in the SQLCA (errd 3).
        0x1805 | 0x1806 => {
            let (sql, cols, args) = match (id, text) {
                (0x1806, Some(t)) => (t, Vec::new(), Vec::new()),
                _ => stmts
                    .get(&rpb)
                    .map(|s| (s.sql.clone(), s.params.clone(), s.marker_args.clone()))
                    .unwrap_or_default(),
            };
            let mut outputs: Option<Vec<Option<String>>> = None;
            // A row of values for each run (more than one: a batch).
            let rows = match marker_values(ds, 20 + tl, &cols) {
                Ok(r) => r,
                Err(e) => return fail(ds, e, last),
            };
            let mut count = 0u64;
            let mut results = 0;
            for values in rows {
                let q = if values.is_empty() {
                    Ok(sql.clone())
                } else {
                    crate::mapepire::bind(&sql, &values)
                };
                match async { run(srv, &q?, dt, false).await }.await {
                    Ok(Ran::Count(n)) => count += n,
                    // A CALL's result set: SQLERRD(2) says there is one,
                    // the client opens it (open and describe).
                    Ok(Ran::Rows(c)) => {
                        let s = stmts.entry(rpb).or_default();
                        s.result = Some(c);
                        s.described = None;
                        results = 1;
                    }
                    Ok(Ran::Call(r, vals)) => {
                        if let Some(c) = r {
                            let s = stmts.entry(rpb).or_default();
                            s.result = Some(c);
                            s.described = None;
                            s.more = std::mem::take(&mut srv.more_results)
                                .into_iter()
                                .map(|r| cursor(r, dt, false))
                                .collect();
                            results = 1 + s.more.len() as i32;
                        }
                        outputs = Some(vals);
                    }
                    Err(e) => return fail(ds, e, last),
                }
            }
            let mut p = Vec::new();
            // A CALL's output parameters: a row of the markers' format,
            // the OUT and INOUT values in their places (JTOpen
            // AS400JDBCPreparedStatementImpl commonExecuteAfter).
            if let Some(vals) = outputs {
                if ors & ORS_RESULT_DATA != 0 && cols.iter().any(|c| c.ptype != 0xF0) {
                    let row: Vec<Option<String>> = cols
                        .iter()
                        .zip(args.iter().chain(std::iter::repeat(&None)))
                        .map(|(c, a)| {
                            if c.ptype == 0xF0 {
                                return None;
                            }
                            a.and_then(|a| vals.get(a).cloned().flatten())
                        })
                        .collect();
                    let mut out = Cursor {
                        cols: cols.clone(),
                        rows: std::iter::once(row).collect(),
                    };
                    p.extend(result_data(&mut out).0);
                }
            }
            p.extend(sqlca_with(0, "00000", count as i32, results));
            db_reply(ds, 0, 0, &p)
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
            let values = match marker_values(ds, 20 + tl, &s.params) {
                Ok(mut r) => r.pop().unwrap_or_default(),
                Err(e) => return fail(ds, e, last),
            };
            if s.result.is_none() && values.is_empty() && !s.more.is_empty() {
                s.result = s.more.pop_front();
            }
            if s.result.is_none() || !values.is_empty() {
                let sql = if values.is_empty() {
                    Ok(s.sql.clone())
                } else {
                    crate::mapepire::bind(&s.sql, &values)
                };
                match async { run(srv, &sql?, dt, false).await }.await {
                    Ok(Ran::Rows(mut c)) => {
                        // The columns as the prepare described them.
                        if let Some(d) = &s.described {
                            if d.len() == c.cols.len() {
                                c.cols = d.clone();
                            }
                        }
                        s.result = Some(c)
                    }
                    Ok(Ran::Call(Some(c), _)) => s.result = Some(c),
                    Ok(Ran::Count(n)) => return db_reply(ds, 0, 0, &sqlca(0, "00000", n as i32)),
                    Ok(Ran::Call(None, _)) => return db_reply(ds, 0, 0, &sqlca(0, "00000", 0)),
                    Err(e) => return fail(ds, e, last),
                }
            }
            let c = s.result.as_mut().expect("result");
            let mut p = Vec::new();
            if ors & ORS_DATA_FORMAT != 0 {
                p.extend(format(0x380C, &c.cols));
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
const ORS_PARAMETER_MARKER_FORMAT: u32 = 0x0080_0000;

enum Ran {
    Rows(Cursor),
    Count(u64),
    /// A CALL: its result set, the values of its parameters (OUT and
    /// INOUT; None for IN and null).
    Call(Option<Cursor>, Vec<Option<String>>),
}

/// Run a statement on the connection's job.
/// A probe (a query run to describe its columns before its rows)
/// describes them as `col_for` does without values.
async fn run(srv: &mut crate::mapepire::Server<'_>, sql: &str, dt: Dt, probe: bool) -> Result<Ran> {
    Ok(match srv.statement(sql).await? {
        crate::mapepire::Done::Rows(r) => Ran::Rows(cursor(r, dt, probe)),
        crate::mapepire::Done::Call(r, parms) => {
            let values = parms
                .iter()
                .map(|p| match p.get("value") {
                    Some(crate::mapepire::J::Num(n)) | Some(crate::mapepire::J::Str(n)) => {
                        Some(n.clone())
                    }
                    Some(crate::mapepire::J::Bool(b)) => Some(if *b { "1" } else { "0" }.into()),
                    _ => None,
                })
                .collect();
            Ran::Call(r.map(|r| cursor(r, dt, probe)), values)
        }
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
    /// How a date or time column is written.
    dt: Dt,
    /// A parameter marker's direction: 0xF0 input, 0xF1 output, 0xF2
    /// both (JTOpen JDServerRow isInput, isOutput).
    ptype: u8,
}

impl Col {
    /// A parameter marker whose column is not worked out: VARCHAR(32739)
    /// in UTF-8, the client's values converted to text.
    fn marker(dt: Dt) -> Col {
        Col {
            name: String::new(),
            sql_type: 449,
            length: 32741,
            precision: 32739,
            scale: 0,
            ccsid: 1208,
            dt,
            ptype: 0xF0,
        }
    }
}

/// The parameter markers of a statement described as the columns they
/// stand for: an INSERT's values (the columns listed, else the table's
/// in order), and the column compared with a marker (`col = ?`, `? <
/// col`, `col LIKE ?`, `col BETWEEN ? AND ?`, `col IN (?, ?)`, `SET col
/// = ?`), its type from a query of the statement's table (or FROM
/// clause) that returns no rows. Other markers are VARCHAR.
async fn marker_cols(srv: &mut crate::mapepire::Server<'_>, sql: &str) -> Vec<Col> {
    let at = crate::mapepire::marker_positions(sql);
    let mut cols: Vec<Col> = at.iter().map(|_| Col::marker(Dt::default())).collect();
    let m = masked(sql);
    let (from, targets) = match insert_targets(&m, sql, &at) {
        Some(x) => x,
        None => (
            from_clause(&m, sql),
            at.iter().map(|p| comparand(&m, sql, *p)).collect(),
        ),
    };
    let Some((prefix, from)) = from else {
        return cols;
    };
    let mut names: Vec<String> = Vec::new();
    for t in targets.iter().flatten() {
        if t != "*" && !names.contains(t) {
            names.push(t.clone());
        }
    }
    let star = targets.iter().any(|t| t.as_deref() == Some("*"));
    if names.is_empty() && !star {
        return cols;
    }
    let list = if star {
        "*".to_string()
    } else {
        names.join(", ")
    };
    let probe = format!("{prefix}SELECT {list} FROM {from} WHERE 1 = 0");
    let probe = match crate::mapepire::bind(
        &probe,
        &vec!["NULL".to_string(); crate::mapepire::marker_positions(&probe).len()],
    ) {
        Ok(q) => q,
        Err(_) => return cols,
    };
    let Ok(crate::mapepire::Done::Rows(r)) = srv.statement(&probe).await else {
        return cols;
    };
    for (i, t) in targets.iter().enumerate() {
        let k = match t.as_deref() {
            // An INSERT without its columns: the table's i-th column.
            Some("*") => insert_ordinal(&m, &at, i),
            Some(name) => names.iter().position(|n| n == name),
            None => None,
        };
        if let Some(t) = k.and_then(|k| r.types.get(k)) {
            // The client sends dates and times in ISO form whatever the
            // connection's format (JTOpen SQLDate, SQLTime
            // convertToRawBytes).
            let mut c = col_for("", t, None, Dt::default());
            c.name.clear();
            cols[i] = c;
        }
    }
    cols
}

/// A CALL's parameter markers described as the procedure's parameters
/// they are the arguments for (their types, and 0xF1 output or 0xF2
/// input and output for OUT and INOUT), and the parameter of each. None
/// when the statement is not a CALL of a procedure found.
async fn call_markers(
    srv: &mut crate::mapepire::Server<'_>,
    sql: &str,
) -> Option<(Vec<Col>, Vec<Option<usize>>)> {
    let m = masked(sql);
    let words = top_words(&m);
    if words.first()?.1 != "CALL" {
        return None;
    }
    let mut i = words[0].0 + 4;
    while i < m.len() && m[i].is_ascii_whitespace() {
        i += 1;
    }
    let ns = i;
    while i < m.len() && (is_name_byte(m[i]) || m[i] == b'/') {
        i += 1;
    }
    let name = sql[ns..i].to_string();
    let routine = srv.procedure(&name).await?;
    let at = crate::mapepire::marker_positions(sql);
    let open = i + m[i..].iter().position(|c| *c == b'(')?;
    let mut cols = Vec::new();
    let mut args = Vec::new();
    for p in at {
        let k = (p > open).then(|| value_position(&m, open, p)).flatten();
        let parm = k.and_then(|k| routine.parms.get(k));
        let mut c = match parm {
            Some(parm) => written_col(&parm.ty),
            None => Col::marker(Dt::default()),
        };
        c.ptype = match parm.map(|p| p.mode.as_str()) {
            Some("OUT") => 0xF1,
            Some("INOUT") => 0xF2,
            _ => 0xF0,
        };
        cols.push(c);
        args.push(if parm.is_some() { k } else { None });
    }
    Some((cols, args))
}

/// A column of a type as written (`DECIMAL(9, 2)`).
fn written_col(ty: &str) -> Col {
    let (name, p, s) = crate::mapepire::written(ty);
    let t = crate::sql::ColType {
        pg: match name.as_str() {
            "SMALLINT" => "int2",
            "INTEGER" => "int4",
            "BIGINT" => "int8",
            "REAL" => "float4",
            "DOUBLE" | "FLOAT" => "float8",
            "DECIMAL" | "NUMERIC" => "numeric",
            "DATE" => "date",
            "TIME" => "time",
            "TIMESTAMP" => "timestamp",
            "CHAR" => "bpchar",
            "BINARY" | "VARBINARY" => "bytea",
            _ => "varchar",
        }
        .into(),
        typmod: match name.as_str() {
            "DECIMAL" | "NUMERIC" => (((p << 16) | s) + 4) as i32,
            "CHAR" | "VARCHAR" | "BINARY" | "VARBINARY" if p > 0 => (p + 4) as i32,
            _ => -1,
        },
        db2: matches!(name.as_str(), "NUMERIC" | "BINARY").then(|| name.clone()),
    };
    col_for("", &t, None, Dt::default())
}

/// A statement with its strings' contents blanked (so that a keyword or
/// parenthesis in a string is not taken for one) and in upper case.
fn masked(sql: &str) -> Vec<u8> {
    let mut b = sql.as_bytes().to_ascii_uppercase();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        b[i] = b'_';
                        b[i + 1] = b'_';
                        i += 2;
                        continue;
                    }
                    break;
                }
                b[i] = b'_';
                i += 1;
            }
        }
        i += 1;
    }
    b
}

fn is_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'#' | b'@' | b'.' | b'"') || c >= 0x80
}

const NOT_NAMES: &[&str] = &[
    "AND", "OR", "NOT", "WHERE", "SET", "ON", "WHEN", "THEN", "ELSE", "HAVING", "SELECT", "VALUES",
    "BY", "IS", "NULL", "CASE", "END", "IN", "LIKE", "BETWEEN",
];

/// The name (a column, perhaps qualified) that ends where `end` is, after
/// blanks.
fn name_before(m: &[u8], sql: &str, end: usize) -> Option<String> {
    let mut e = end;
    while e > 0 && m[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let mut b = e;
    while b > 0 && is_name_byte(m[b - 1]) {
        b -= 1;
    }
    name_at(m, sql, b, e)
}

fn name_at(m: &[u8], sql: &str, b: usize, e: usize) -> Option<String> {
    if b >= e || m[b].is_ascii_digit() || m[b] == b'.' {
        return None;
    }
    let w = std::str::from_utf8(&m[b..e]).ok()?;
    if NOT_NAMES.contains(&w) {
        return None;
    }
    Some(sql[b..e].to_string())
}

/// The word ending where `end` is (after blanks), and where it starts.
fn word_before(m: &[u8], end: usize) -> (usize, String) {
    let mut e = end;
    while e > 0 && m[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let mut b = e;
    while b > 0 && (m[b - 1].is_ascii_alphanumeric() || m[b - 1] == b'_') {
        b -= 1;
    }
    (b, String::from_utf8_lossy(&m[b..e]).into_owned())
}

/// The column a marker at `p` is compared with.
fn comparand(m: &[u8], sql: &str, p: usize) -> Option<String> {
    let mut e = p;
    while e > 0 && m[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let ops: [&[u8]; 7] = [b"<=", b">=", b"<>", b"!=", b"=", b"<", b">"];
    if let Some(op) = ops.iter().find(|op| m[..e].ends_with(op)) {
        if let Some(n) = name_before(m, sql, e - op.len()) {
            return Some(n);
        }
    }
    let (b, w) = word_before(m, e);
    let skip_not = |b: usize| {
        let (nb, nw) = word_before(m, b);
        if nw == "NOT" {
            nb
        } else {
            b
        }
    };
    match w.as_str() {
        "LIKE" | "BETWEEN" => return name_before(m, sql, skip_not(b)),
        "AND" => {
            // col BETWEEN x AND ?
            let mut x = b;
            while x > 0 && m[x - 1].is_ascii_whitespace() {
                x -= 1;
            }
            while x > 0 && !m[x - 1].is_ascii_whitespace() {
                x -= 1;
            }
            let (bb, bw) = word_before(m, x);
            if bw == "BETWEEN" {
                return name_before(m, sql, skip_not(bb));
            }
        }
        _ => {}
    }
    // col IN (?, ?)
    if e > 0 && matches!(m[e - 1], b'(' | b',') {
        let mut depth = 0i32;
        let mut x = e;
        while x > 0 {
            x -= 1;
            match m[x] {
                b')' => depth += 1,
                b'(' if depth == 0 => {
                    let (ib, iw) = word_before(m, x);
                    if iw == "IN" {
                        return name_before(m, sql, skip_not(ib));
                    }
                    break;
                }
                b'(' => depth -= 1,
                _ => {}
            }
        }
    }
    // ? = col
    let mut a = p + 1;
    while a < m.len() && m[a].is_ascii_whitespace() {
        a += 1;
    }
    if let Some(op) = ops.iter().find(|op| m[a..].starts_with(op)) {
        let mut b = a + op.len();
        while b < m.len() && m[b].is_ascii_whitespace() {
            b += 1;
        }
        let mut e = b;
        while e < m.len() && is_name_byte(m[e]) {
            e += 1;
        }
        return name_at(m, sql, b, e);
    }
    None
}

/// Where each top-level word of a statement starts (outside
/// parentheses).
fn top_words(m: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < m.len() {
        match m[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b'"' => {
                i += 1;
                while i < m.len() && m[i] != b'"' {
                    i += 1;
                }
            }
            c if depth == 0 && (c.is_ascii_alphabetic()) && (i == 0 || !is_name_byte(m[i - 1])) => {
                let b = i;
                while i < m.len() && (m[i].is_ascii_alphanumeric() || m[i] == b'_') {
                    i += 1;
                }
                out.push((b, String::from_utf8_lossy(&m[b..i]).into_owned()));
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The tables a statement's markers' columns are in: an UPDATE's table,
/// a DELETE's, a query's FROM clause (with the common table expressions
/// before it).
fn from_clause(m: &[u8], sql: &str) -> Option<(String, String)> {
    let words = top_words(m);
    let first = words.first()?.1.as_str();
    let end_of = |from: usize, stops: &[&str]| {
        words
            .iter()
            .find(|(at, w)| *at > from && stops.contains(&w.as_str()))
            .map_or(sql.len(), |(at, _)| *at)
    };
    const STOPS: &[&str] = &[
        "WHERE",
        "GROUP",
        "HAVING",
        "ORDER",
        "FETCH",
        "LIMIT",
        "OFFSET",
        "FOR",
        "UNION",
        "EXCEPT",
        "INTERSECT",
        "WINDOW",
        "OPTIMIZE",
        "WITH",
        "SET",
    ];
    match first {
        "UPDATE" => {
            let b = words[0].0 + 6;
            Some((
                String::new(),
                sql[b..end_of(b, &["SET"])].trim().to_string(),
            ))
        }
        "DELETE" => {
            let (fb, _) = words.iter().find(|(_, w)| w == "FROM")?;
            let b = fb + 4;
            Some((String::new(), sql[b..end_of(b, STOPS)].trim().to_string()))
        }
        "SELECT" | "WITH" => {
            let (fb, _) = words.iter().find(|(_, w)| w == "FROM")?;
            let (sb, _) = words
                .iter()
                .rev()
                .find(|(at, w)| at < fb && w == "SELECT")?;
            let b = fb + 4;
            // The common table expressions before the final SELECT.
            let from = sql[b..end_of(b, STOPS)].trim();
            Some((sql[..*sb].to_string(), from.to_string()))
        }
        _ => None,
    }
}

/// An INSERT's markers in its VALUES: the table, and each marker's
/// column ("*" for the table's column in the marker's position).
#[allow(clippy::type_complexity)]
fn insert_targets(
    m: &[u8],
    sql: &str,
    at: &[usize],
) -> Option<(Option<(String, String)>, Vec<Option<String>>)> {
    let words = top_words(m);
    if words.first()?.1 != "INSERT" || words.get(1)?.1 != "INTO" {
        return None;
    }
    let tb = words[1].0 + 4;
    let mut i = tb;
    while i < m.len() && m[i].is_ascii_whitespace() {
        i += 1;
    }
    let ts = i;
    while i < m.len() && is_name_byte(m[i]) {
        i += 1;
    }
    let table = sql[ts..i].to_string();
    while i < m.len() && m[i].is_ascii_whitespace() {
        i += 1;
    }
    let listed: Option<Vec<String>> = if m.get(i) == Some(&b'(') {
        let close = i + m[i..].iter().position(|c| *c == b')')?;
        let list = sql[i + 1..close]
            .split(',')
            .map(|c| c.trim().to_string())
            .collect();
        Some(list)
    } else {
        None
    };
    let vb = words.iter().find(|(_, w)| w == "VALUES")?.0;
    let targets = at
        .iter()
        .map(|p| {
            if *p < vb {
                return None;
            }
            let k = value_position(m, vb, *p)?;
            match &listed {
                Some(l) => l.get(k).cloned(),
                None => Some("*".to_string()),
            }
        })
        .collect();
    Some((Some((String::new(), table)), targets))
}

/// The position of the value at `p` in its row of a VALUES clause
/// starting at `vb`, when the marker is a value by itself.
fn value_position(m: &[u8], vb: usize, p: usize) -> Option<usize> {
    let mut depth = 0;
    let mut k = 0;
    let mut start = vb;
    for (i, c) in m.iter().enumerate().take(p).skip(vb) {
        match c {
            b'(' => {
                depth += 1;
                if depth == 1 {
                    k = 0;
                    start = i + 1;
                }
            }
            b')' => depth -= 1,
            b',' if depth == 1 => {
                k += 1;
                start = i + 1;
            }
            _ => {}
        }
    }
    let before = std::str::from_utf8(&m[start..p]).ok()?.trim();
    let mut e = p + 1;
    while e < m.len() && m[e].is_ascii_whitespace() {
        e += 1;
    }
    (depth == 1 && before.is_empty() && matches!(m.get(e), Some(b',' | b')'))).then_some(k)
}

/// The column position of an INSERT marker without a column list.
fn insert_ordinal(m: &[u8], at: &[usize], i: usize) -> Option<usize> {
    let vb = top_words(m).iter().find(|(_, w)| w == "VALUES")?.0;
    value_position(m, vb, at[i])
}

/// The rows of a statement's result as a cursor: each column's type
/// (Db2 SQL type codes: 496 INTEGER, 500 SMALLINT, 492 BIGINT, 480
/// floating point, 484 DECIMAL packed, 488 NUMERIC zoned, 452 CHAR, 448
/// VARCHAR, 912 BINARY, 908 VARBINARY; nullable +1), its length from its
/// type and its longest value.
fn cursor(r: crate::mapepire::Results, dt: Dt, probe: bool) -> Cursor {
    let rows: std::collections::VecDeque<Vec<Option<String>>> = r.rows;
    let cols = r
        .columns
        .iter()
        .zip(&r.types)
        .enumerate()
        .map(|(i, (name, t))| {
            let values: Vec<&str> = rows
                .iter()
                .filter_map(|row| row.get(i).and_then(|v| v.as_deref()))
                .collect();
            col_for(name, t, (!probe).then_some(&values[..]), dt)
        })
        .collect();
    Cursor { cols, rows }
}

/// A column's description from its type and, when its rows are known,
/// its values (the length of a column of no declared length, the scale
/// of a decimal of no declared precision); before its rows, the longest
/// a varying-length column can be and DECIMAL(31, 10).
fn col_for(name: &str, t: &crate::sql::ColType, values: Option<&[&str]>, dt: Dt) -> Col {
    let longest = match values {
        Some(v) => v.iter().map(|v| shown(t, v).len()).max().unwrap_or(0),
        None => 32739,
    };
    let (ty, p, s, _) = crate::mapepire::column_type(t, longest);
    let (sql_type, length, precision, scale, ccsid) = match ty.as_str() {
        "SMALLINT" => (500, 2, 5, 0, 0),
        "INTEGER" => (496, 4, 10, 0, 0),
        "BIGINT" => (492, 8, 19, 0, 0),
        "REAL" | "DOUBLE" => (480, 8, 53, 0, 0),
        // A decimal of no declared precision (an expression):
        // DECIMAL(31, s), s the most decimals of its values.
        "DECIMAL" if t.typmod < 4 && t.pg == "numeric" => {
            let s = match values {
                Some(v) => v
                    .iter()
                    .map(|v| v.split_once('.').map_or(0, |(_, f)| f.len()))
                    .max()
                    .unwrap_or(0)
                    .min(30),
                None => 10,
            };
            (484, 16, 31, s as u16, 0)
        }
        "DECIMAL" => (484, (p as usize) / 2 + 1, p as u16, s as u16, 0),
        "NUMERIC" => (488, p as usize, p as u16, s as u16, 0),
        "DATE" => (384, dt.date_len(), 0, 0, 1208),
        "TIME" => (388, 8, 0, 0, 1208),
        "TIMESTAMP" => (392, 26, 26, 6, 1208),
        "CHAR" | "NCHAR" => {
            let n = (p as usize).max(1);
            (452, n, n as u16, 0, 1208)
        }
        "BINARY" => (912, (p as usize).max(1), p as u16, 0, 65535),
        "VARBINARY" | "VARCHAR () FOR BIT DATA" => {
            let n = (p as usize).clamp(1, 32739);
            (908, n + 2, n as u16, 0, 65535)
        }
        _ => {
            let n = (p as usize).clamp(1, 32739);
            (448, n + 2, n as u16, 0, 1208)
        }
    };
    Col {
        name: name.to_string(),
        sql_type: sql_type + 1,
        length,
        precision,
        scale,
        ccsid,
        dt,
        ptype: 0xF0,
    }
}

/// How the connection writes dates and times (its date format and
/// separator, time format and separator, as the set attributes request
/// agreed them; JTOpen SQLConversionSettings: date formats 0 JUL, 1 MDY,
/// 2 DMY, 3 YMD, 4 USA, 5 ISO, 6 EUR, 7 JIS; separators 0 /, 1 -, 2 .,
/// 3 ,, 4 blank; time formats 0 HMS, 1 USA, 2 ISO, 3 EUR, 4 JIS;
/// separators 0 :, 1 ., 2 ,, 3 blank).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Dt {
    date: u16,
    date_sep: u16,
    time: u16,
    time_sep: u16,
}

impl Default for Dt {
    fn default() -> Self {
        Dt {
            date: 5,
            date_sep: 1,
            time: 2,
            time_sep: 1,
        }
    }
}

impl Dt {
    fn of(a: &Attributes) -> Dt {
        Dt {
            date: a.date_format,
            date_sep: a.date_separator,
            time: a.time_format,
            time_sep: a.time_separator,
        }
    }

    fn date_len(&self) -> usize {
        match self.date {
            0 => 6,
            1..=3 => 8,
            _ => 10,
        }
    }

    fn dsep(&self) -> char {
        ['/', '-', '.', ',', ' '][(self.date_sep as usize).min(4)]
    }

    fn tsep(&self) -> char {
        [':', '.', ',', ' '][(self.time_sep as usize).min(3)]
    }

    /// An ISO date (yyyy-mm-dd) in the connection's format.
    fn date_text(&self, iso: &str) -> String {
        let d: Vec<u32> = iso
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|x| x.parse().ok())
            .collect();
        let (y, m, day) = match d[..] {
            [y, m, day, ..] => (y, m, day),
            _ => return iso.to_string(),
        };
        let s = self.dsep();
        match self.date {
            0 => {
                let n = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
                let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
                let doy = n[(m as usize).clamp(1, 12) - 1] + day + u32::from(leap && m > 2);
                format!("{:02}{s}{doy:03}", y % 100)
            }
            1 => format!("{m:02}{s}{day:02}{s}{:02}", y % 100),
            2 => format!("{day:02}{s}{m:02}{s}{:02}", y % 100),
            3 => format!("{:02}{s}{m:02}{s}{day:02}", y % 100),
            4 => format!("{m:02}/{day:02}/{y:04}"),
            6 => format!("{day:02}.{m:02}.{y:04}"),
            _ => format!("{y:04}-{m:02}-{day:02}"),
        }
    }

    /// A date in the connection's format as ISO (yyyy-mm-dd); two-digit
    /// years 40-99 in the 1900s, 00-39 in the 2000s.
    fn date_iso(&self, t: &str) -> String {
        let d: Vec<u32> = t
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|x| x.parse().ok())
            .collect();
        let year = |y: u32| {
            if y >= 100 {
                y
            } else if y >= 40 {
                1900 + y
            } else {
                2000 + y
            }
        };
        let iso = t
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .map(str::len)
            == Some(4);
        let (y, m, day) = match (if iso { 5 } else { self.date }, &d[..]) {
            (0, [y, doy, ..]) => {
                let n = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365];
                let y = year(*y);
                let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
                let mut m = 1;
                while m < 12 && *doy > n[m] + u32::from(leap && m >= 2) {
                    m += 1;
                }
                let before = n[m - 1] + u32::from(leap && m > 2);
                (y, m as u32, doy - before)
            }
            (1 | 4, [m, day, y, ..]) => (year(*y), *m, *day),
            (2 | 6, [day, m, y, ..]) => (year(*y), *m, *day),
            (3, [y, m, day, ..]) => (year(*y), *m, *day),
            (_, [y, m, day, ..]) => (*y, *m, *day),
            _ => return t.to_string(),
        };
        format!("{y:04}-{m:02}-{day:02}")
    }

    /// A time (hh.mm.ss or hh:mm:ss) in the connection's format.
    fn time_text(&self, v: &str) -> String {
        let d: Vec<u32> = v
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|x| x.parse().ok())
            .collect();
        let (h, m, sec) = match d[..] {
            [h, m, sec, ..] => (h, m, sec),
            [h, m] => (h, m, 0),
            _ => return v.to_string(),
        };
        match self.time {
            0 => {
                let s = self.tsep();
                format!("{h:02}{s}{m:02}{s}{sec:02}")
            }
            1 => {
                let (h12, ap) = match h {
                    0 => (12, "AM"),
                    1..=11 => (h, "AM"),
                    12 => (12, "PM"),
                    _ => (h - 12, "PM"),
                };
                format!("{h12:02}:{m:02} {ap}")
            }
            4 => format!("{h:02}:{m:02}:{sec:02}"),
            _ => format!("{h:02}.{m:02}.{sec:02}"),
        }
    }

    /// A time in the connection's format as hh:mm:ss.
    fn time_iso(&self, t: &str) -> String {
        let d: Vec<u32> = t
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|x| x.parse().ok())
            .collect();
        let pm = t.to_ascii_uppercase().contains("PM");
        let (mut h, m, sec) = match d[..] {
            [h, m, sec, ..] => (h, m, sec),
            [h, m] => (h, m, 0),
            _ => return t.to_string(),
        };
        if self.time == 1 || t.to_ascii_uppercase().contains('M') {
            h %= 12;
            if pm {
                h += 12;
            }
        }
        format!("{h:02}:{m:02}:{sec:02}")
    }
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

/// A data format (JTOpen DBExtendedDataFormat), of a result (CP
/// 0x380C) or of parameter markers (0x380D, parameter type 0xF0 input):
/// a consistency token (4), the number of fields (4), reserved (4), the
/// record size (4), then 64 bytes for each field: its description's
/// length, SQL type, length (4), scale, precision, CCSID, parameter type,
/// LOB locator and maximum size, the name's length, its CCSID and the
/// name (30 bytes).
fn format(cp: u16, cols: &[Col]) -> Vec<u8> {
    let size: usize = cols.iter().map(|c| c.length).sum();
    let mut d = 1u32.to_be_bytes().to_vec();
    d.extend((cols.len() as u32).to_be_bytes());
    d.extend([0u8; 4]);
    d.extend((size as u32).to_be_bytes());
    for c in cols {
        let mut f = Vec::with_capacity(64);
        f.extend(64u16.to_be_bytes());
        f.extend(c.sql_type.to_be_bytes());
        f.extend((c.length as u32).to_be_bytes());
        f.extend(c.scale.to_be_bytes());
        f.extend(c.precision.to_be_bytes());
        f.extend(c.ccsid.to_be_bytes());
        f.push(if cp == 0x380D { c.ptype } else { 0 });
        f.extend([0u8; 15]);
        let mut name = c.name.as_bytes().to_vec();
        name.truncate(30);
        f.extend((name.len() as u16).to_be_bytes());
        f.extend(1208u16.to_be_bytes());
        name.resize(30, 0);
        f.extend(name);
        d.extend(f);
    }
    param(cp, &d)
}

/// The fields of a client's descriptor: extended (CP 0x381E, as
/// `format` lays it out) or original (0x3801: a 2-byte count and record
/// size, 54 bytes a field with a 2-byte length).
fn descriptor(d: &[u8], extended: bool) -> Vec<Col> {
    let u16_at = |i: usize| {
        d.get(i..i + 2)
            .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        d.get(i..i + 4)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (n, start, each) = if extended {
        (u32_at(4) as usize, 16, 64)
    } else {
        (u16_at(4) as usize, 8, 54)
    };
    (0..n)
        .map(|i| {
            let f = start + i * each;
            let length = if extended {
                u32_at(f + 4) as usize
            } else {
                u16_at(f + 4) as usize
            };
            let (scale, precision, ccsid) = if extended {
                (u16_at(f + 8), u16_at(f + 10), u16_at(f + 12))
            } else {
                (u16_at(f + 6), u16_at(f + 8), u16_at(f + 10))
            };
            Col {
                name: String::new(),
                sql_type: u16_at(f + 2),
                length,
                precision,
                scale,
                ccsid,
                // Dates and times come in ISO form (`marker_cols`).
                dt: Dt::default(),
                ptype: 0xF0,
            }
        })
        .collect()
}

/// The values of a request's parameter markers (JTOpen DBExtendedData,
/// CP 0x381F: a consistency token (4), the row count (4), the column
/// count (2), the indicator size (2), reserved (4), the row size (4);
/// DBOriginalData, 0x3811: the same without the reserved bytes and with
/// a 2-byte row size), then the indicators (-1: null) and the rows laid
/// out as the descriptor says: a row of SQL literals for each row.
fn marker_values(ds: &Ds, from: usize, cols: &[Col]) -> Result<Vec<Vec<String>>> {
    let Some((cp, d)) = ds
        .params(from)
        .into_iter()
        .find(|(cp, _)| matches!(cp, 0x381F | 0x3811))
    else {
        return Ok(vec![Vec::new()]);
    };
    let bad = || anyhow::anyhow!("SQL0313: Number of host variables not valid.");
    let u16_at = |i: usize| d.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |i: usize| {
        d.get(i..i + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let rows = u32_at(4).ok_or_else(bad)? as usize;
    let ncols = u16_at(8).ok_or_else(bad)? as usize;
    let ind = u16_at(10).ok_or_else(bad)? as usize;
    let (row_size, head) = if cp == 0x381F {
        (u32_at(16).ok_or_else(bad)? as usize, 20)
    } else {
        (u16_at(12).ok_or_else(bad)? as usize, 14)
    };
    if ncols != cols.len() {
        return Err(crate::mapepire::sql_failure(&bad()));
    }
    let data = head + rows * ncols * ind;
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut at = data + r * row_size;
        let mut row = Vec::with_capacity(ncols);
        for (c, col) in cols.iter().enumerate() {
            let null =
                ind > 0 && u16_at(head + (r * ncols + c) * ind).is_some_and(|v| (v as i16) < 0);
            let bytes = d.get(at..at + col.length).ok_or_else(bad)?;
            row.push(if null {
                "NULL".into()
            } else {
                literal(col, bytes)
            });
            at += col.length;
        }
        out.push(row);
    }
    Ok(out)
}

/// A value in its descriptor's form as an SQL literal.
fn literal(c: &Col, b: &[u8]) -> String {
    let quote = |t: &str| format!("'{}'", t.replace('\'', "''"));
    let hex = |b: &[u8]| {
        format!(
            "X'{}'",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        )
    };
    let varying = || {
        let n = b
            .get(..2)
            .map_or(0, |l| u16::from_be_bytes([l[0], l[1]]) as usize);
        &b[2.min(b.len())..(2 + n).min(b.len())]
    };
    match c.sql_type & !1 {
        500 if b.len() >= 2 => i16::from_be_bytes([b[0], b[1]]).to_string(),
        496 if b.len() >= 4 => i32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
        492 if b.len() >= 8 => i64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8])).to_string(),
        480 if b.len() == 4 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]).to_string(),
        480 if b.len() >= 8 => f64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8])).to_string(),
        484 => unpacked(b, c.scale as usize),
        384 => quote(&c.dt.date_iso(&decode(c.ccsid, b))),
        388 => quote(&c.dt.time_iso(&decode(c.ccsid, b))),
        488 => unzoned(b, c.scale as usize),
        912 => hex(b),
        908 => hex(varying()),
        448 | 456 | 464 | 472 if c.ccsid == 65535 => hex(varying()),
        452 | 468 if c.ccsid == 65535 => hex(b),
        448 | 456 | 464 | 472 => quote(&decode(c.ccsid, varying())),
        _ => quote(decode(c.ccsid, b).trim_end_matches([' ', '\0'])),
    }
}

/// A packed decimal's value as text.
fn unpacked(b: &[u8], scale: usize) -> String {
    let mut digits = String::new();
    for (i, x) in b.iter().enumerate() {
        digits.push(char::from(b'0' + (x >> 4)));
        if i + 1 < b.len() {
            digits.push(char::from(b'0' + (x & 0x0F)));
        }
    }
    let neg = b.last().is_some_and(|x| matches!(x & 0x0F, 0xB | 0xD));
    decimal_text(&digits, scale, neg)
}

/// A zoned decimal's value as text.
fn unzoned(b: &[u8], scale: usize) -> String {
    let digits: String = b.iter().map(|x| char::from(b'0' + (x & 0x0F))).collect();
    let neg = b.last().is_some_and(|x| matches!(x >> 4, 0xB | 0xD));
    decimal_text(&digits, scale, neg)
}

fn decimal_text(digits: &str, scale: usize, neg: bool) -> String {
    let scale = scale.min(digits.len());
    let (i, f) = digits.split_at(digits.len() - scale);
    let i = i.trim_start_matches('0');
    let i = if i.is_empty() { "0" } else { i };
    let sign = if neg { "-" } else { "" };
    if f.is_empty() {
        format!("{sign}{i}")
    } else {
        format!("{sign}{i}.{f}")
    }
}

/// The next block of rows (CP 0x380E, JTOpen DBExtendedData): a
/// consistency token (4), the row count (4), the column count (2), the
/// indicator size (2), reserved (4), the row size (4), each row's
/// indicators (0, or -1 for null), then the rows. Whether rows remain.
fn result_data(c: &mut Cursor) -> (Vec<u8>, bool) {
    let row_size: usize = c.cols.iter().map(|c| c.length).sum();
    let n = c
        .rows
        .len()
        .min(BLOCK_ROWS)
        .min((BLOCK_BYTES / row_size.max(1)).max(1));
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
    d.extend([0u8; 4]);
    d.extend((row_size as u32).to_be_bytes());
    d.extend(ind);
    d.extend(data);
    (param(0x380E, &d), !c.rows.is_empty())
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
        384 => c.dt.date_text(v).into_bytes(),
        388 => c.dt.time_text(v).into_bytes(),
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
    let pad = if matches!(c.sql_type & !1, 452 | 384 | 388 | 392) {
        b' '
    } else {
        0
    };
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
    sqlca_with(code, state, rows, 0)
}

/// An SQLCA with the number of result sets of a CALL in SQLERRD(2).
fn sqlca_with(code: i32, state: &str, rows: i32, results: i32) -> Vec<u8> {
    let mut d = ebcdic("SQLCA", 8);
    d.extend(136u32.to_be_bytes());
    d.extend(code.to_be_bytes());
    d.extend([0u8; 2]); // SQLERRML
    d.extend([0u8; 70]); // SQLERRMC
    d.extend(ebcdic("QSQ", 8)); // SQLERRP
    for i in 0..6 {
        let v = match i {
            1 => results,
            2 => rows,
            _ => 0,
        };
        d.extend(v.to_be_bytes());
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
    fn parameters_described_from_their_types_as_written() {
        let t = |ty: &str| {
            let c = written_col(ty);
            (c.sql_type, c.length, c.precision, c.scale)
        };
        assert_eq!(t("INTEGER"), (497, 4, 10, 0));
        assert_eq!(t("DEC(9, 2)"), (485, 5, 9, 2));
        assert_eq!(t("NUMERIC(5)"), (489, 5, 5, 0));
        assert_eq!(t("VARCHAR(20)"), (449, 22, 20, 0));
        assert_eq!(t("CHAR(3)"), (453, 3, 3, 0));
        assert_eq!(t("DATE"), (385, 10, 0, 0));
        assert_eq!(t("TIMESTAMP"), (393, 26, 26, 6));
    }

    #[test]
    fn the_columns_markers_stand_for() {
        let cmp = |sql: &str| -> Vec<Option<String>> {
            let m = masked(sql);
            crate::mapepire::marker_positions(sql)
                .into_iter()
                .map(|p| comparand(&m, sql, p))
                .collect()
        };
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            cmp("SELECT * FROM T WHERE A = ? AND B.X<>? AND ? <= C"),
            vec![s("A"), s("B.X"), s("C")]
        );
        assert_eq!(
            cmp("SELECT * FROM T WHERE N NOT LIKE ? OR D BETWEEN ? AND ? OR E IN (?, ?)"),
            vec![s("N"), s("D"), s("D"), s("E"), s("E")]
        );
        assert_eq!(
            cmp("UPDATE T SET A = ?, B = SUBSTR(?, 1, 2) WHERE '=?' = 'x'"),
            vec![s("A"), None]
        );
        let m = masked("SELECT A FROM T X JOIN U ON X.K = U.K WHERE X.A = ? ORDER BY 1");
        assert_eq!(
            from_clause(
                &m,
                "SELECT A FROM T X JOIN U ON X.K = U.K WHERE X.A = ? ORDER BY 1"
            ),
            Some((String::new(), "T X JOIN U ON X.K = U.K".into()))
        );
        let w = "WITH Q AS (SELECT * FROM T WHERE A > 1) SELECT B FROM Q WHERE B = ?";
        assert_eq!(
            from_clause(&masked(w), w),
            Some((
                "WITH Q AS (SELECT * FROM T WHERE A > 1) ".into(),
                "Q".into()
            ))
        );
        let u = "update lib.t set a = ? where b = ?";
        assert_eq!(
            from_clause(&masked(u), u),
            Some((String::new(), "lib.t".into()))
        );
        let i = "INSERT INTO L.T (A, B, C) VALUES (?, 1, ?), (?, ?, CAST(? AS INT))";
        let at = crate::mapepire::marker_positions(i);
        let (from, t) = insert_targets(&masked(i), i, &at).unwrap();
        assert_eq!(from, Some((String::new(), "L.T".into())));
        assert_eq!(t, vec![s("A"), s("C"), s("A"), s("B"), None]);
        let i = "INSERT INTO T VALUES (?, ?)";
        let at = crate::mapepire::marker_positions(i);
        assert_eq!(
            insert_targets(&masked(i), i, &at).unwrap().1,
            vec![s("*"), s("*")]
        );
        assert_eq!(insert_ordinal(&masked(i), &at, 1), Some(1));
    }

    #[test]
    fn dates_and_times_in_the_connections_formats() {
        // JTOpen SQLDate and SQLTime: MDY mm/dd/yy, JUL yy/ddd, USA
        // mm/dd/yyyy, EUR dd.mm.yyyy; times USA hh:mm AM, ISO hh.mm.ss.
        let dt = |date, date_sep, time, time_sep| Dt {
            date,
            date_sep,
            time,
            time_sep,
        };
        assert_eq!(dt(1, 0, 2, 0).date_text("2026-11-10"), "11/10/26");
        assert_eq!(dt(0, 0, 2, 0).date_text("2024-03-01"), "24/061");
        assert_eq!(dt(4, 1, 2, 0).date_text("2026-01-05"), "01/05/2026");
        assert_eq!(dt(6, 0, 2, 0).date_text("2026-01-05"), "05.01.2026");
        assert_eq!(Dt::default().date_text("2026-01-05"), "2026-01-05");
        assert_eq!(dt(1, 0, 2, 0).date_iso("11/10/26"), "2026-11-10");
        assert_eq!(dt(0, 0, 2, 0).date_iso("24/061"), "2024-03-01");
        assert_eq!(dt(1, 0, 2, 0).date_iso("2026-11-10"), "2026-11-10");
        assert_eq!(dt(2, 2, 2, 0).date_iso("10.11.99"), "1999-11-10");
        assert_eq!(dt(5, 1, 1, 0).time_text("13:05:09"), "01:05 PM");
        assert_eq!(dt(5, 1, 0, 3).time_text("13.05.09"), "13 05 09");
        assert_eq!(Dt::default().time_text("13:05:09"), "13.05.09");
        assert_eq!(dt(5, 1, 1, 0).time_iso("01:05 PM"), "13:05:00");
        assert_eq!(Dt::default().time_iso("13.05.09"), "13:05:09");
    }

    #[test]
    fn values_of_markers_as_literals() {
        let col = |sql_type, length, scale, ccsid| Col {
            name: String::new(),
            sql_type,
            length,
            precision: 0,
            scale,
            ccsid,
            dt: Dt::default(),
            ptype: 0xF0,
        };
        assert_eq!(literal(&col(497, 4, 0, 0), &[0xFF, 0xFF, 0xFF, 0xFE]), "-2");
        assert_eq!(
            literal(&col(485, 4, 2, 0), &[0x00, 0x01, 0x23, 0x4D]),
            "-12.34"
        );
        assert_eq!(literal(&col(489, 3, 1, 0), &[0xF0, 0xF4, 0xD5]), "-4.5");
        assert_eq!(
            literal(&col(449, 7, 0, 1208), &[0, 4, b'a', b'\'', b'b', b'c', 0]),
            "'a''bc'"
        );
        assert_eq!(
            literal(&col(453, 4, 0, 37), &[0xC1, 0x40, 0x40, 0x40]),
            "'A'"
        );
        assert_eq!(literal(&col(909, 4, 0, 65535), &[0, 2, 1, 0xAB]), "X'01AB'");
        assert_eq!(
            literal(&col(385, 10, 0, 1208), b"2026-11-10"),
            "'2026-11-10'"
        );
    }

    #[test]
    fn run_length_compressed_requests() {
        // JTOpen DataStreamCompression: 0x1B 0x1B is 0x1B; 0x1B, two
        // bytes and a count repeats them.
        let mut v = vec![0u8; 40];
        v[6..8].copy_from_slice(&0xE004u16.to_be_bytes());
        v[16..18].copy_from_slice(&20u16.to_be_bytes());
        v[20..24].copy_from_slice(&0x0008_0000u32.to_be_bytes());
        let data = [9u8, 0x1B, 0x1B, 0x1B, 0, 7, 0, 3, 5];
        v.extend(((data.len() + 10) as u32).to_be_bytes());
        v.extend(0x3832u16.to_be_bytes());
        v.extend(9u32.to_be_bytes());
        v.extend(data);
        let l = v.len() as u32;
        v[..4].copy_from_slice(&l.to_be_bytes());
        let d = decompressed(Ds(v));
        assert_eq!(&d.0[40..], &[9, 0x1B, 0, 7, 0, 7, 0, 7, 5]);
        assert_eq!(
            d.0.len() as u32,
            u32::from_be_bytes([d.0[0], d.0[1], d.0[2], d.0[3]])
        );
    }

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
