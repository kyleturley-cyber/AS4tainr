//! The SQL call level interface (DB2 for i SQL CLI): an ILE program
//! allocates environment, connection and statement handles, runs
//! statements (SQLExecDirect, SQLPrepare and SQLExecute with parameters
//! bound by SQLBindParameter), and reads their results (SQLNumResultCols,
//! SQLDescribeCol, SQLBindCol, SQLFetch, SQLGetCol), with diagnostics
//! (SQLGetDiagRec, SQLError) and transactions (SQLEndTran).
//!
//! Written from the X/Open CLI as DB2 for i documents it; the constants
//! are the ones IBM i's header defines (XMLSERVICE, an IBM i application
//! that calls the CLI, defines the same in plugcli_h.rpgle). A
//! connection is an SQL session of its own on the job's library list
//! (system naming and SQL naming both find unqualified names through
//! it); with autocommit off its changes are one unit of work until
//! SQLEndTran. C types are the SQL type codes (SQL_C_CHAR = SQL_CHAR...),
//! SQL_C_DEFAULT (99) the column's own form: binary integers, packed
//! DECIMAL, zoned NUMERIC, floating point, characters.

use super::super::data::e;
use super::super::mem::Ptr;
use super::*;
use std::sync::Arc;

/// The procedures here (bound like the C runtime's, `crt::C_FUNCTIONS`).
pub const CLI_FUNCTIONS: &[&str] = &[
    "SQLALLOCENV",
    "SQLALLOCCONNECT",
    "SQLALLOCSTMT",
    "SQLALLOCHANDLE",
    "SQLFREEENV",
    "SQLFREECONNECT",
    "SQLFREESTMT",
    "SQLFREEHANDLE",
    "SQLSETENVATTR",
    "SQLSETCONNECTATTR",
    "SQLSETSTMTATTR",
    "SQLCONNECT",
    "SQLDISCONNECT",
    "SQLENDTRAN",
    "SQLTRANSACT",
    "SQLPREPARE",
    "SQLEXECUTE",
    "SQLEXECDIRECT",
    "SQLNUMPARAMS",
    "SQLDESCRIBEPARAM",
    "SQLBINDPARAMETER",
    "SQLNUMRESULTCOLS",
    "SQLDESCRIBECOL",
    "SQLBINDCOL",
    "SQLFETCH",
    "SQLFETCHSCROLL",
    "SQLGETCOL",
    "SQLGETDATA",
    "SQLROWCOUNT",
    "SQLERROR",
    "SQLGETDIAGREC",
    "SQLCLOSECURSOR",
];

// Return codes.
const SQL_SUCCESS: i64 = 0;
const SQL_SUCCESS_WITH_INFO: i64 = 1;
const SQL_NO_DATA: i64 = 100;
const SQL_ERROR: i64 = -1;
const SQL_INVALID_HANDLE: i64 = -2;
// Lengths.
const SQL_NTS: i64 = -3;
const SQL_NULL_DATA: i32 = -1;
// Handle types.
const SQL_HANDLE_ENV: i64 = 1;
const SQL_HANDLE_DBC: i64 = 2;
const SQL_HANDLE_STMT: i64 = 3;
// Data types (and C types).
const SQL_CHAR: i16 = 1;
const SQL_NUMERIC: i16 = 2;
const SQL_DECIMAL: i16 = 3;
const SQL_INTEGER: i16 = 4;
const SQL_SMALLINT: i16 = 5;
const SQL_FLOAT: i16 = 6;
const SQL_REAL: i16 = 7;
const SQL_DOUBLE: i16 = 8;
const SQL_VARCHAR: i16 = 12;
const SQL_BIGINT: i16 = 19;
const SQL_DATE: i16 = 91;
const SQL_TIME: i16 = 92;
const SQL_TIMESTAMP: i16 = 93;
const SQL_BINARY: i16 = 97;
const SQL_VARBINARY: i16 = 98;
const SQL_DEFAULT: i16 = 99;
// Attributes.
const SQL_ATTR_AUTOCOMMIT: i64 = 10003;
// SQLEndTran.
const SQL_COMMIT: i64 = 0;
const SQL_COMMIT_HOLD: i64 = 2;
// SQLFreeStmt.
const SQL_CLOSE: i64 = 0;
const SQL_DROP: i64 = 1;
const SQL_UNBIND: i64 = 2;
const SQL_RESET_PARAMS: i64 = 3;
// SQLFetchScroll.
const SQL_FETCH_NEXT: i64 = 1;
const SQL_FETCH_FIRST: i64 = 2;
const SQL_FETCH_LAST: i64 = 3;
const SQL_FETCH_PRIOR: i64 = 4;
const SQL_FETCH_ABSOLUTE: i64 = 5;
const SQL_FETCH_RELATIVE: i64 = 6;
// SQLBindParameter's parameter types.
const SQL_PARAM_INPUT: i64 = 1;
const SQL_PARAM_OUTPUT: i64 = 2;

/// A diagnostic record: SQLSTATE, native error (SQLCODE), message.
#[derive(Clone)]
struct Diag {
    state: String,
    native: i32,
    text: String,
}

#[derive(Default)]
struct Conn {
    autocommit: bool,
    connected: bool,
    session: Option<Arc<crate::sql::Session>>,
}

/// A bound parameter: its direction, C type, SQL type, column size,
/// scale, buffer, buffer length and length/indicator.
#[derive(Clone)]
struct Param {
    io: i64,
    ctype: i16,
    sqltype: i16,
    size: i32,
    scale: i16,
    data: Option<Ptr>,
    buflen: i32,
    ind: Option<Ptr>,
}

#[derive(Clone)]
struct Bound {
    ctype: i16,
    data: Option<Ptr>,
    buflen: i32,
    ind: Option<Ptr>,
}

/// A column of a result: name, SQL type, size (precision), scale.
#[derive(Clone)]
struct Col {
    name: String,
    sqltype: i16,
    size: i32,
    scale: i16,
}

#[derive(Default)]
struct Stmt {
    conn: i32,
    text: Option<String>,
    params: BTreeMap<u16, Param>,
    bound: BTreeMap<u16, Bound>,
    cols: Vec<Col>,
    rows: Vec<Vec<Option<String>>>,
    /// The row fetched last: -1 before the first.
    pos: i64,
    has_result: bool,
    rowcount: i64,
}

#[derive(Default)]
struct JobCli {
    next: i32,
    envs: Vec<i32>,
    conns: HashMap<i32, Conn>,
    stmts: HashMap<i32, Stmt>,
    diags: HashMap<i32, Vec<Diag>>,
}

static CLI: std::sync::Mutex<Option<HashMap<String, JobCli>>> = std::sync::Mutex::new(None);

fn with<T>(job: &str, f: impl FnOnce(&mut JobCli) -> T) -> T {
    let mut g = CLI.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new)
        .entry(job.to_string())
        .or_default())
}

/// The diagnostic for an error from as4tainr's SQL ("SQL0204: ..."):
/// its SQLCODE and SQLSTATE; other errors are SQLCODE -901.
fn diag_of(err: &anyhow::Error) -> Diag {
    let text = format!("{err:#}");
    let code = text
        .strip_prefix("SQL")
        .map(|r| r.chars().take(4).collect::<String>())
        .filter(|d| d.len() == 4 && d.chars().all(|c| c.is_ascii_digit()))
        .and_then(|d| d.parse::<i32>().ok())
        .map(|n| -n)
        .unwrap_or(-901);
    Diag {
        state: exec_sql::sqlstate(code as i64).to_string(),
        native: code,
        text,
    }
}

/// The SQL type, size and scale of a result column from PostgreSQL's
/// type (and its modifier when the column comes from a table); a column
/// without a length is as long as its longest value.
fn col_type(t: &crate::sql::ColType, values: &[Option<&str>]) -> (i16, i32, i16) {
    let longest = values
        .iter()
        .flatten()
        .map(|v| v.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as i32;
    let m = t.typmod;
    match t.pg.as_str() {
        "int4" => (SQL_INTEGER, 4, 0),
        "int2" | "bool" => (SQL_SMALLINT, 2, 0),
        "int8" => (SQL_BIGINT, 8, 0),
        "float8" => (SQL_DOUBLE, 8, 0),
        "float4" => (SQL_REAL, 4, 0),
        "numeric" if m >= 4 => (SQL_DECIMAL, (m - 4) >> 16, ((m - 4) & 0xFFFF) as i16),
        "numeric" => {
            // An expression: precision 31, the fraction digits its values
            // have.
            let scale = values
                .iter()
                .flatten()
                .filter_map(|v| v.split_once('.').map(|(_, f)| f.len()))
                .max()
                .unwrap_or(0);
            (SQL_DECIMAL, 31, scale as i16)
        }
        "bpchar" if m >= 4 => (SQL_CHAR, m - 4, 0),
        "bpchar" => (SQL_CHAR, longest, 0),
        "varchar" if m >= 4 => (SQL_VARCHAR, m - 4, 0),
        "date" => (SQL_DATE, 10, 0),
        "time" => (SQL_TIME, 8, 0),
        "timestamp" => (SQL_TIMESTAMP, 26, 0),
        "bytea" => (SQL_VARBINARY, longest, 0),
        _ => (SQL_VARCHAR, longest, 0),
    }
}

/// The SQL type, size and scale of a type as written (a procedure's
/// parameter: `VARCHAR(128)`, `DECIMAL(7, 2)`, `INTEGER`).
fn written_type(ty: &str) -> (i16, i32, i16) {
    let t = ty.trim().to_ascii_uppercase();
    let (base, args) = match t.split_once('(') {
        Some((b, r)) => (b.trim().to_string(), r.trim_end_matches(')').to_string()),
        None => (t.clone(), String::new()),
    };
    let nums: Vec<i32> = args
        .split(',')
        .filter_map(|x| x.trim().trim_end_matches(['K', 'M']).parse().ok())
        .collect();
    let n = |i: usize, d: i32| nums.get(i).copied().unwrap_or(d);
    match base.as_str() {
        "INT" | "INTEGER" => (SQL_INTEGER, 4, 0),
        "SMALLINT" => (SQL_SMALLINT, 2, 0),
        "BIGINT" => (SQL_BIGINT, 8, 0),
        "DOUBLE" | "FLOAT" | "DOUBLE PRECISION" => (SQL_DOUBLE, 8, 0),
        "REAL" => (SQL_REAL, 4, 0),
        "DEC" | "DECIMAL" => (SQL_DECIMAL, n(0, 5), n(1, 0) as i16),
        "NUMERIC" => (SQL_NUMERIC, n(0, 5), n(1, 0) as i16),
        "CHAR" | "CHARACTER" => (SQL_CHAR, n(0, 1), 0),
        "DATE" => (SQL_DATE, 10, 0),
        "TIME" => (SQL_TIME, 8, 0),
        "TIMESTAMP" => (SQL_TIMESTAMP, 26, 0),
        "BINARY" => (SQL_BINARY, n(0, 1), 0),
        "VARBINARY" => (SQL_VARBINARY, n(0, 1), 0),
        _ => (SQL_VARCHAR, n(0, 32740), 0),
    }
}

/// The `?` markers of a statement (outside literals).
fn markers(sql: &str) -> usize {
    let mut n = 0;
    let mut quote = None;
    for c in sql.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '?' => n += 1,
            None => {}
        }
    }
    n
}

impl Machine<'_> {
    pub(super) fn cli_call(&mut self, name: &str, args: &[Arg]) -> Option<Result<Value>> {
        if !CLI_FUNCTIONS.contains(&name) {
            return None;
        }
        Some(
            self.cli_call_inner(name, args)
                .map(|n| Value::Num(Dec::from_i64(n))),
        )
    }

    fn cli_call_inner(&mut self, name: &str, args: &[Arg]) -> Result<i64> {
        // A by-reference argument is its address (handles returned
        // through 10I 0 parameters, pointers through `*` VALUE ones).
        let refs: Vec<Option<Value>> = args
            .iter()
            .map(|a| match a {
                Arg::Ref(l) => Some(self.ptr_to(l.buf, l.off)),
                _ => None,
            })
            .collect();
        let val = |i: usize| -> Value {
            match args.get(i) {
                Some(Arg::Val(v)) => v.clone(),
                _ => refs
                    .get(i)
                    .cloned()
                    .flatten()
                    .unwrap_or(Value::Fig(Fig::Null)),
            }
        };
        let int = |i: usize| -> i64 {
            match args.get(i) {
                Some(Arg::Val(v)) => {
                    v.num().ok().and_then(|d| d.to_i128().ok()).unwrap_or(0) as i64
                }
                Some(Arg::Ref(l)) => data::load(&l.dtype, &self.bufs[l.buf][l.off..l.off + l.size])
                    .ok()
                    .and_then(|v| v.num().ok())
                    .and_then(|d| d.to_i128().ok())
                    .unwrap_or(0) as i64,
                _ => 0,
            }
        };
        let ptr = |i: usize| -> Option<Ptr> {
            match val(i) {
                Value::Ptr(p) => Some(p),
                _ => None,
            }
        };
        let job = self.io.job_key.clone();
        match name {
            "SQLALLOCENV" => {
                let h = with(&job, |c| {
                    c.next += 1;
                    let h = c.next;
                    c.envs.push(h);
                    h
                });
                self.put_int(&val(0), h as i64, 4)?;
                Ok(SQL_SUCCESS)
            }
            "SQLALLOCCONNECT" => {
                let h = with(&job, |c| {
                    c.next += 1;
                    let h = c.next;
                    c.conns.insert(
                        h,
                        Conn {
                            autocommit: true,
                            ..Default::default()
                        },
                    );
                    h
                });
                self.put_int(&val(1), h as i64, 4)?;
                Ok(SQL_SUCCESS)
            }
            "SQLALLOCSTMT" => {
                let conn = int(0) as i32;
                let h = with(&job, |c| {
                    if !c.conns.get(&conn).is_some_and(|x| x.connected) {
                        return None;
                    }
                    c.next += 1;
                    let h = c.next;
                    c.stmts.insert(
                        h,
                        Stmt {
                            conn,
                            pos: -1,
                            ..Default::default()
                        },
                    );
                    Some(h)
                });
                match h {
                    Some(h) => {
                        self.put_int(&val(1), h as i64, 4)?;
                        Ok(SQL_SUCCESS)
                    }
                    None => {
                        Ok(self.fail(conn, "08003", -900, "SQL0900: The connection is not open."))
                    }
                }
            }
            "SQLALLOCHANDLE" => {
                let (kind, input) = (int(0), int(1));
                let sub = match kind {
                    SQL_HANDLE_ENV => "SQLALLOCENV",
                    SQL_HANDLE_DBC => "SQLALLOCCONNECT",
                    SQL_HANDLE_STMT => "SQLALLOCSTMT",
                    _ => return Ok(SQL_ERROR),
                };
                let a = if kind == SQL_HANDLE_ENV {
                    vec![Arg::Val(val(2))]
                } else {
                    vec![Arg::Val(Value::Num(Dec::from_i64(input))), Arg::Val(val(2))]
                };
                self.cli_call_inner(sub, &a)
            }
            "SQLFREEENV" | "SQLFREECONNECT" | "SQLFREEHANDLE"
                if name != "SQLFREEHANDLE" || int(0) != SQL_HANDLE_STMT =>
            {
                let h = if name == "SQLFREEHANDLE" {
                    int(1)
                } else {
                    int(0)
                } as i32;
                with(&job, |c| {
                    c.envs.retain(|x| *x != h);
                    c.conns.remove(&h);
                    c.diags.remove(&h);
                });
                Ok(SQL_SUCCESS)
            }
            "SQLFREEHANDLE" | "SQLFREESTMT" => {
                let (h, opt) = if name == "SQLFREEHANDLE" {
                    (int(1) as i32, SQL_DROP)
                } else {
                    (int(0) as i32, int(1))
                };
                let found = with(&job, |c| {
                    let Some(s) = c.stmts.get_mut(&h) else {
                        return false;
                    };
                    match opt {
                        SQL_CLOSE => {
                            s.rows.clear();
                            s.cols.clear();
                            s.has_result = false;
                            s.pos = -1;
                        }
                        SQL_UNBIND => s.bound.clear(),
                        SQL_RESET_PARAMS => s.params.clear(),
                        _ => {
                            c.stmts.remove(&h);
                            c.diags.remove(&h);
                        }
                    }
                    true
                });
                Ok(if found {
                    SQL_SUCCESS
                } else {
                    SQL_INVALID_HANDLE
                })
            }
            "SQLCLOSECURSOR" => {
                let h = int(0) as i32;
                with(&job, |c| {
                    if let Some(s) = c.stmts.get_mut(&h) {
                        s.rows.clear();
                        s.has_result = false;
                        s.pos = -1;
                    }
                });
                Ok(SQL_SUCCESS)
            }
            // Attributes: autocommit kept (a pointer to the value, or the
            // value); the others (server mode, commit level, naming, date
            // formats...) accepted.
            "SQLSETENVATTR" | "SQLSETSTMTATTR" => Ok(SQL_SUCCESS),
            "SQLSETCONNECTATTR" => {
                let (h, attr) = (int(0) as i32, int(1));
                if attr == SQL_ATTR_AUTOCOMMIT {
                    let on = match val(2) {
                        v @ Value::Ptr(_) => {
                            let (b, o) = self.addr(&v, 4)?;
                            i32::from_be_bytes(self.bufs[b][o..o + 4].try_into()?) != 0
                        }
                        v => v.num().map(|d| !d.is_zero()).unwrap_or(true),
                    };
                    with(&job, |c| {
                        if let Some(x) = c.conns.get_mut(&h) {
                            if x.autocommit != on {
                                x.autocommit = on;
                                // A new session at the next statement.
                                x.session = None;
                            }
                        }
                    });
                }
                Ok(SQL_SUCCESS)
            }
            // SQLConnect(hdbc, server, len, user, len, password, len): the
            // local database, as the job's user (the user and password
            // given are not checked).
            "SQLCONNECT" => {
                let h = int(0) as i32;
                let ok = with(&job, |c| match c.conns.get_mut(&h) {
                    Some(x) => {
                        x.connected = true;
                        true
                    }
                    None => false,
                });
                Ok(if ok { SQL_SUCCESS } else { SQL_INVALID_HANDLE })
            }
            "SQLDISCONNECT" => {
                let h = int(0) as i32;
                with(&job, |c| {
                    if let Some(x) = c.conns.get_mut(&h) {
                        x.connected = false;
                        x.session = None;
                    }
                    c.stmts.retain(|_, s| s.conn != h);
                });
                Ok(SQL_SUCCESS)
            }
            // SQLEndTran(handle type, handle, completion) and
            // SQLTransact(henv, hdbc, completion): COMMIT or ROLLBACK of
            // the connection's unit of work (all of the environment's).
            "SQLENDTRAN" | "SQLTRANSACT" => {
                let (kind, h, what) = if name == "SQLENDTRAN" {
                    (int(0), int(1) as i32, int(2))
                } else if int(1) != 0 {
                    (SQL_HANDLE_DBC, int(1) as i32, int(2))
                } else {
                    (SQL_HANDLE_ENV, int(0) as i32, int(2))
                };
                let sessions: Vec<Arc<crate::sql::Session>> = with(&job, |c| {
                    c.conns
                        .iter()
                        .filter(|(k, _)| kind == SQL_HANDLE_ENV || **k == h)
                        .filter_map(|(_, x)| x.session.clone())
                        .collect()
                });
                for s in sessions {
                    if let Some(d) = s.commitment() {
                        let r = if what == SQL_COMMIT || what == SQL_COMMIT_HOLD {
                            self.block(d.commit())
                        } else {
                            self.block(d.rollback())
                        };
                        if let Err(e) = r {
                            return Ok(self.fail_with(h, diag_of(&e)));
                        }
                    }
                }
                Ok(SQL_SUCCESS)
            }
            // SQLPrepare(hstmt, text, length), SQLExecDirect(same).
            "SQLPREPARE" | "SQLEXECDIRECT" => {
                let h = int(0) as i32;
                let text = self.cli_text(&val(1), int(2))?;
                let known = with(&job, |c| match c.stmts.get_mut(&h) {
                    Some(s) => {
                        s.text = Some(text.clone());
                        s.rows.clear();
                        s.cols.clear();
                        s.has_result = false;
                        s.pos = -1;
                        c.diags.remove(&h);
                        true
                    }
                    None => false,
                });
                if !known {
                    return Ok(SQL_INVALID_HANDLE);
                }
                if name == "SQLPREPARE" {
                    Ok(SQL_SUCCESS)
                } else {
                    self.cli_execute(h)
                }
            }
            "SQLEXECUTE" => self.cli_execute(int(0) as i32),
            "SQLNUMPARAMS" => {
                let h = int(0) as i32;
                let n = with(&job, |c| {
                    c.stmts.get(&h).and_then(|s| s.text.as_deref().map(markers))
                });
                let Some(n) = n else {
                    return Ok(SQL_INVALID_HANDLE);
                };
                self.put_int(&val(1), n as i64, 2)?;
                Ok(SQL_SUCCESS)
            }
            // SQLDescribeParam(hstmt, n, *type, *size, *scale, *nullable):
            // a procedure's parameters as the procedure declares them;
            // other markers VARCHAR.
            "SQLDESCRIBEPARAM" => {
                let (h, n) = (int(0) as i32, int(1).max(1) as usize);
                let text = with(&job, |c| c.stmts.get(&h).and_then(|s| s.text.clone()));
                let Some(text) = text else {
                    return Ok(SQL_INVALID_HANDLE);
                };
                // The procedure's parameter where the n-th marker is.
                let at = crate::sqlpl::parse_call(&text)
                    .ok()
                    .flatten()
                    .and_then(|(_, args)| {
                        args.iter()
                            .enumerate()
                            .filter(|(_, a)| a.trim() == "?")
                            .nth(n - 1)
                            .map(|(i, _)| i)
                    });
                let (t, size, scale) = match (self.call_parms(&text)?, at) {
                    (Some(parms), Some(i)) => parms
                        .get(i)
                        .map(|(_, ty)| written_type(ty))
                        .unwrap_or((SQL_VARCHAR, 32740, 0)),
                    _ => (SQL_VARCHAR, 32740, 0),
                };
                self.put_int(&val(2), t as i64, 2)?;
                self.put_int(&val(3), size as i64, 4)?;
                self.put_int(&val(4), scale as i64, 2)?;
                self.put_int(&val(5), 1, 2)?;
                Ok(SQL_SUCCESS)
            }
            // SQLBindParameter(hstmt, n, io, C type, SQL type, size,
            // scale, buffer, buffer length, *length/indicator).
            "SQLBINDPARAMETER" => {
                let h = int(0) as i32;
                let p = Param {
                    io: int(2),
                    ctype: int(3) as i16,
                    sqltype: int(4) as i16,
                    size: int(5) as i32,
                    scale: int(6) as i16,
                    data: ptr(7),
                    buflen: int(8) as i32,
                    ind: ptr(9),
                };
                let n = int(1) as u16;
                let ok = with(&job, |c| match c.stmts.get_mut(&h) {
                    Some(s) => {
                        s.params.insert(n, p);
                        true
                    }
                    None => false,
                });
                Ok(if ok { SQL_SUCCESS } else { SQL_INVALID_HANDLE })
            }
            "SQLNUMRESULTCOLS" => {
                let h = int(0) as i32;
                let n = with(&job, |c| c.stmts.get(&h).map(|s| s.cols.len()));
                let Some(n) = n else {
                    return Ok(SQL_INVALID_HANDLE);
                };
                self.put_int(&val(1), n as i64, 2)?;
                Ok(SQL_SUCCESS)
            }
            // SQLDescribeCol(hstmt, n, name, name length, *name length,
            // *type, *size, *scale, *nullable).
            "SQLDESCRIBECOL" => {
                let (h, n) = (int(0) as i32, int(1));
                let col = with(&job, |c| {
                    c.stmts
                        .get(&h)
                        .and_then(|s| s.cols.get((n - 1).max(0) as usize).cloned())
                });
                let Some(col) = col else {
                    return Ok(self.fail(
                        h,
                        "07009",
                        -999,
                        "SQL0999: The column number is not valid.",
                    ));
                };
                // The name and a null, blanks after it to the buffer's
                // end (XMLSERVICE takes the name's length as that of the
                // buffer trimmed of blanks, less one).
                if let Some(p) = ptr(2) {
                    let max = int(3).max(0) as usize;
                    if max > 0 {
                        let mut b = e(&col.name);
                        b.truncate(max - 1);
                        b.push(0);
                        b.resize(max, 0x40);
                        self.put_bytes(&Value::Ptr(p), &b)?;
                    }
                }
                self.put_int(&val(4), col.name.len() as i64, 2)?;
                self.put_int(&val(5), col.sqltype as i64, 2)?;
                self.put_int(&val(6), col.size as i64, 4)?;
                self.put_int(&val(7), col.scale as i64, 2)?;
                self.put_int(&val(8), 1, 2)?;
                Ok(SQL_SUCCESS)
            }
            // SQLBindCol(hstmt, n, C type, buffer, buffer length,
            // *length/indicator).
            "SQLBINDCOL" => {
                let h = int(0) as i32;
                let b = Bound {
                    ctype: int(2) as i16,
                    data: ptr(3),
                    buflen: int(4) as i32,
                    ind: ptr(5),
                };
                let n = int(1) as u16;
                let ok = with(&job, |c| match c.stmts.get_mut(&h) {
                    Some(s) => {
                        s.bound.insert(n, b);
                        true
                    }
                    None => false,
                });
                Ok(if ok { SQL_SUCCESS } else { SQL_INVALID_HANDLE })
            }
            "SQLFETCH" => self.cli_fetch(int(0) as i32, SQL_FETCH_NEXT, 0),
            "SQLFETCHSCROLL" => self.cli_fetch(int(0) as i32, int(1), int(2)),
            // SQLGetCol / SQLGetData(hstmt, n, C type, buffer, buffer
            // length, *length/indicator): a column of the row fetched.
            "SQLGETCOL" | "SQLGETDATA" => {
                let (h, n) = (int(0) as i32, int(1));
                let got = with(&job, |c| {
                    let s = c.stmts.get(&h)?;
                    let row = s.rows.get(s.pos.max(0) as usize).filter(|_| s.pos >= 0)?;
                    let i = (n - 1).max(0) as usize;
                    Some((row.get(i).cloned().flatten(), s.cols.get(i).cloned()?))
                });
                let Some((v, col)) = got else {
                    return Ok(self.fail(h, "24000", -501, "SQL0501: The cursor is not on a row."));
                };
                let b = Bound {
                    ctype: int(2) as i16,
                    data: ptr(3),
                    buflen: int(4) as i32,
                    ind: ptr(5),
                };
                self.put_column(&b, &col, v.as_deref())?;
                Ok(SQL_SUCCESS)
            }
            "SQLROWCOUNT" => {
                let h = int(0) as i32;
                let n = with(&job, |c| c.stmts.get(&h).map(|s| s.rowcount));
                let Some(n) = n else {
                    return Ok(SQL_INVALID_HANDLE);
                };
                self.put_int(&val(1), n, 4)?;
                Ok(SQL_SUCCESS)
            }
            // SQLGetDiagRec(handle type, handle, record, *state, *native,
            // message, message length, *text length).
            "SQLGETDIAGREC" => {
                let (h, rec) = (int(1) as i32, int(2).max(1) as usize);
                let d = with(&job, |c| {
                    c.diags.get(&h).and_then(|v| v.get(rec - 1).cloned())
                });
                let Some(d) = d else {
                    return Ok(SQL_NO_DATA);
                };
                self.put_diag(&d, &val(3), &val(4), &val(5), int(6), &val(7))?;
                Ok(SQL_SUCCESS)
            }
            // SQLError(henv, hdbc, hstmt, *state, *native, message,
            // message length, *text length): the most specific handle's
            // next diagnostic, which it takes away.
            "SQLERROR" => {
                let h = [int(2), int(1), int(0)]
                    .into_iter()
                    .find(|x| *x != 0)
                    .unwrap_or(0) as i32;
                let d = with(&job, |c| {
                    c.diags
                        .get_mut(&h)
                        .filter(|v| !v.is_empty())
                        .map(|v| v.remove(0))
                });
                let Some(d) = d else {
                    return Ok(SQL_NO_DATA);
                };
                self.put_diag(&d, &val(3), &val(4), &val(5), int(6), &val(7))?;
                Ok(SQL_SUCCESS)
            }
            _ => bail!("{name} is not available in as4tainr yet"),
        }
    }

    /// An error for a handle: its diagnostic, SQL_ERROR.
    fn fail(&self, h: i32, state: &str, native: i32, text: &str) -> i64 {
        self.fail_with(
            h,
            Diag {
                state: state.into(),
                native,
                text: text.into(),
            },
        )
    }

    fn fail_with(&self, h: i32, d: Diag) -> i64 {
        with(&self.io.job_key, |c| c.diags.entry(h).or_default().push(d));
        SQL_ERROR
    }

    /// A string argument: `len` bytes, or up to a null when SQL_NTS.
    fn cli_text(&self, v: &Value, len: i64) -> Result<String> {
        let b = if len == SQL_NTS || len < 0 {
            self.str_at(v, None)?.chars()?
        } else {
            let (b, o) = self.addr(v, len as usize)?;
            self.bufs[b][o..o + len as usize].to_vec()
        };
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(a(&b[..n]))
    }

    /// An integer of `size` bytes where `at` points (nothing for *NULL).
    fn put_int(&mut self, at: &Value, n: i64, size: usize) -> Result<()> {
        if matches!(at, Value::Fig(Fig::Null)) {
            return Ok(());
        }
        let b: Vec<u8> = match size {
            2 => (n as i16).to_be_bytes().to_vec(),
            8 => n.to_be_bytes().to_vec(),
            _ => (n as i32).to_be_bytes().to_vec(),
        };
        self.put_bytes(at, &b)
    }

    fn put_bytes(&mut self, at: &Value, b: &[u8]) -> Result<()> {
        let (buf, o) = self.addr(at, b.len())?;
        self.bufs[buf][o..o + b.len()].copy_from_slice(b);
        Ok(())
    }

    fn put_diag(
        &mut self,
        d: &Diag,
        state: &Value,
        native: &Value,
        msg: &Value,
        max: i64,
        len: &Value,
    ) -> Result<()> {
        if !matches!(state, Value::Fig(Fig::Null)) {
            let mut b = e(&format!("{:<5}", d.state));
            b.truncate(5);
            b.push(0);
            self.put_bytes(state, &b)?;
        }
        self.put_int(native, d.native as i64, 4)?;
        if !matches!(msg, Value::Fig(Fig::Null)) && max > 0 {
            let mut b = e(&d.text);
            b.truncate(max as usize - 1);
            b.push(0);
            self.put_bytes(msg, &b)?;
        }
        self.put_int(len, d.text.len() as i64, 2)
    }

    /// A procedure's parameters (name, type as written) when the
    /// statement is a CALL.
    fn call_parms(&mut self, text: &str) -> Result<Option<Vec<(String, String)>>> {
        let Some((name, _)) = crate::sqlpl::parse_call(text).ok().flatten() else {
            return Ok(None);
        };
        let r = self.block(crate::sqlpl::find(
            self.io.client,
            &self.io.libl,
            &name,
            "PROCEDURE",
        ))?;
        Ok(r.map(|r| {
            r.parms
                .iter()
                .map(|p| (p.mode.clone(), p.ty.clone()))
                .collect()
        }))
    }

    /// The connection's session (made at its first statement).
    fn cli_session(&mut self, conn: i32) -> Result<Arc<crate::sql::Session>> {
        let job = self.io.job_key.clone();
        let (have, autocommit) = with(&job, |c| {
            let x = c.conns.get(&conn);
            (
                x.and_then(|x| x.session.clone()),
                x.map(|x| x.autocommit).unwrap_or(true),
            )
        });
        if let Some(s) = have {
            return Ok(s);
        }
        let s = if autocommit {
            self.block(crate::sql::Session::open(
                self.io.client,
                &self.io.database_url,
                &self.io.libl,
            ))?
        } else {
            let d = self.block(crate::cmtctl::private(
                self.io.client,
                &self.io.database_url,
                Some(&job),
                "*CHG",
            ))?;
            self.block(crate::sql::Session::open_in(
                self.io.client,
                &self.io.database_url,
                &self.io.libl,
                d,
            ))?
        };
        let s = Arc::new(s);
        with(&job, |c| {
            if let Some(x) = c.conns.get_mut(&conn) {
                x.session = Some(s.clone());
            }
        });
        Ok(s)
    }

    /// A bound parameter's value as an SQL literal (NULL for a null
    /// indicator).
    fn param_literal(&self, p: &Param) -> Result<String> {
        if let Some(ind) = p.ind {
            let (b, o) = self.addr(&Value::Ptr(ind), 4)?;
            if i32::from_be_bytes(self.bufs[b][o..o + 4].try_into()?) == SQL_NULL_DATA {
                return Ok("NULL".into());
            }
        }
        let Some(data) = p.data else {
            return Ok("NULL".into());
        };
        let at = Value::Ptr(data);
        let ctype = if p.ctype == SQL_DEFAULT {
            p.sqltype
        } else {
            p.ctype
        };
        let number = |d: Dec| d.to_string();
        Ok(match ctype {
            SQL_INTEGER | SQL_SMALLINT | SQL_BIGINT => {
                let n = match ctype {
                    SQL_SMALLINT => 2,
                    SQL_BIGINT => 8,
                    _ => 4,
                };
                let (b, o) = self.addr(&at, n)?;
                let x = &self.bufs[b][o..o + n];
                match n {
                    2 => i16::from_be_bytes(x.try_into()?) as i64,
                    8 => i64::from_be_bytes(x.try_into()?),
                    _ => i32::from_be_bytes(x.try_into()?) as i64,
                }
                .to_string()
            }
            SQL_DECIMAL | SQL_NUMERIC => {
                let (digits, scale) = if p.buflen >= 256 {
                    ((p.buflen / 256) as u32, (p.buflen % 256) as u32)
                } else {
                    (p.size.max(1) as u32, p.scale.max(0) as u32)
                };
                let t = if ctype == SQL_DECIMAL {
                    DType::Packed(digits, scale)
                } else {
                    DType::Zoned(digits, scale)
                };
                let (b, o) = self.addr(&at, t.size())?;
                number(data::load(&t, &self.bufs[b][o..o + t.size()])?.num()?)
            }
            SQL_DOUBLE | SQL_FLOAT => {
                let (b, o) = self.addr(&at, 8)?;
                format!(
                    "{:e}",
                    f64::from_be_bytes(self.bufs[b][o..o + 8].try_into()?)
                )
            }
            SQL_REAL => {
                let (b, o) = self.addr(&at, 4)?;
                format!(
                    "{:e}",
                    f32::from_be_bytes(self.bufs[b][o..o + 4].try_into()?)
                )
            }
            _ => {
                // Characters: the length the indicator gives, else up to a
                // null within the buffer (blanks after the value left out).
                let given = match p.ind {
                    Some(ind) => {
                        let (b, o) = self.addr(&Value::Ptr(ind), 4)?;
                        i32::from_be_bytes(self.bufs[b][o..o + 4].try_into()?)
                    }
                    None => SQL_NTS as i32,
                };
                let max = if p.buflen > 0 {
                    p.buflen as usize
                } else {
                    32740
                };
                let (b, o) = self.addr(&at, 0)?;
                let avail = self.bufs[b].len().saturating_sub(o);
                let raw = &self.bufs[b][o..o + max.min(avail)];
                let text = if given >= 0 {
                    a(&raw[..(given as usize).min(raw.len())])
                } else {
                    let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                    a(&raw[..n]).trim_end().to_string()
                };
                format!("'{}'", text.replace('\'', "''"))
            }
        })
    }

    /// SQLExecute: the statement with its parameters' values; a query's
    /// rows (or a procedure's result set) for SQLFetch, OUT parameters
    /// back into their buffers.
    fn cli_execute(&mut self, h: i32) -> Result<i64> {
        let job = self.io.job_key.clone();
        let st = with(&job, |c| {
            c.diags.remove(&h);
            c.stmts
                .get(&h)
                .map(|s| (s.conn, s.text.clone(), s.params.clone()))
        });
        let Some((conn, Some(text), params)) = st else {
            return Ok(SQL_INVALID_HANDLE);
        };
        let session = match self.cli_session(conn) {
            Ok(s) => s,
            Err(e) => return Ok(self.fail_with(h, diag_of(&e))),
        };
        let n = markers(&text);
        let mut literals = Vec::new();
        for k in 1..=n {
            literals.push(match params.get(&(k as u16)) {
                Some(p) if p.io != SQL_PARAM_OUTPUT => self.param_literal(p)?,
                Some(_) => "?".to_string(),
                None => {
                    return Ok(self.fail(
                        h,
                        "07002",
                        -313,
                        "SQL0313: Not enough values for the parameter markers.",
                    ));
                }
            });
        }
        // CALL: through the procedure (OUT and INOUT parameters come back).
        if let Some((name, args)) = crate::sqlpl::parse_call(&text).ok().flatten() {
            let mut k = 0;
            let args: Vec<String> = args
                .into_iter()
                .map(|a| {
                    if a.trim() == "?" {
                        k += 1;
                        literals.get(k - 1).cloned().unwrap_or_else(|| "?".into())
                    } else {
                        a
                    }
                })
                .collect();
            let curlib = self.io.cl.as_ref().and_then(|c| c.curlib.clone());
            let env = crate::sqlpl::Env {
                session: &session,
                catalog: self.io.client,
                database_url: &self.io.database_url,
                libl: &self.io.libl,
                user: &self.io.user,
                curlib,
            };
            let r = self.block(crate::sqlpl::call_exprs(&env, &name, &args));
            let (routine, out) = match r {
                Ok(x) => x,
                Err(e) => return Ok(self.fail_with(h, diag_of(&e))),
            };
            // OUT and INOUT parameters: the markers' bound buffers.
            let mut marker = 0;
            let originals = crate::sqlpl::parse_call(&text)?
                .map(|(_, a)| a)
                .unwrap_or_default();
            for (i, a) in originals.iter().enumerate() {
                if a.trim() != "?" {
                    continue;
                }
                marker += 1;
                let mode = routine
                    .parms
                    .get(i)
                    .map(|p| p.mode.as_str())
                    .unwrap_or("IN");
                if mode == "IN" {
                    continue;
                }
                let Some(p) = params.get(&(marker as u16)) else {
                    continue;
                };
                if p.io == SQL_PARAM_INPUT {
                    continue;
                }
                let (t, size, scale) = routine
                    .parms
                    .get(i)
                    .map(|x| written_type(&x.ty))
                    .unwrap_or((SQL_VARCHAR, 32740, 0));
                let col = Col {
                    name: String::new(),
                    sqltype: t,
                    size,
                    scale,
                };
                let b = Bound {
                    ctype: p.ctype,
                    data: p.data,
                    buflen: p.buflen,
                    ind: p.ind,
                };
                let v = out.values.get(i).cloned().flatten();
                self.put_value(&b, &col, v.as_deref(), true)?;
            }
            let (cols, rows) = match out.result_sets.into_iter().next() {
                Some((names, rows)) => {
                    let cols = names
                        .iter()
                        .enumerate()
                        .map(|(j, nm)| {
                            let vals: Vec<Option<&str>> = rows
                                .iter()
                                .map(|r| r.get(j).and_then(|x| x.as_deref()))
                                .collect();
                            let t = crate::sql::ColType {
                                pg: "text".into(),
                                typmod: -1,
                            };
                            let (sqltype, size, scale) = col_type(&t, &vals);
                            Col {
                                name: nm.clone(),
                                sqltype,
                                size,
                                scale,
                            }
                        })
                        .collect();
                    (cols, rows)
                }
                None => (vec![], vec![]),
            };
            with(&job, |c| {
                if let Some(s) = c.stmts.get_mut(&h) {
                    s.has_result = !cols.is_empty();
                    s.cols = cols;
                    s.rowcount = rows.len() as i64;
                    s.rows = rows;
                    s.pos = -1;
                }
            });
            return Ok(SQL_SUCCESS);
        }
        let sql = match exec_sql::fill_markers(&text, &literals) {
            Ok(s) => s,
            Err(e) => return Ok(self.fail_with(h, diag_of(&e))),
        };
        let r = self.block(session.exec(self.io.client, &sql));
        match r {
            Ok(crate::sql::Exec::Rows {
                columns,
                rows,
                types,
            }) => {
                let cols: Vec<Col> = columns
                    .iter()
                    .enumerate()
                    .map(|(j, nm)| {
                        let vals: Vec<Option<&str>> = rows
                            .iter()
                            .map(|r| r.get(j).and_then(|x| x.as_deref()))
                            .collect();
                        let t = types.get(j).cloned().unwrap_or(crate::sql::ColType {
                            pg: "text".into(),
                            typmod: -1,
                        });
                        let (sqltype, size, scale) = col_type(&t, &vals);
                        Col {
                            name: nm.clone(),
                            sqltype,
                            size,
                            scale,
                        }
                    })
                    .collect();
                with(&job, |c| {
                    if let Some(s) = c.stmts.get_mut(&h) {
                        s.cols = cols;
                        s.rowcount = rows.len() as i64;
                        s.rows = rows;
                        s.has_result = true;
                        s.pos = -1;
                    }
                });
                Ok(SQL_SUCCESS)
            }
            Ok(crate::sql::Exec::Count(_, n)) => {
                with(&job, |c| {
                    if let Some(s) = c.stmts.get_mut(&h) {
                        s.cols.clear();
                        s.rows.clear();
                        s.has_result = false;
                        s.rowcount = n as i64;
                    }
                });
                // A searched UPDATE or DELETE of no rows: SQL_NO_DATA.
                let up = sql.trim_start().to_ascii_uppercase();
                if n == 0 && (up.starts_with("UPDATE") || up.starts_with("DELETE")) {
                    return Ok(SQL_NO_DATA);
                }
                Ok(SQL_SUCCESS)
            }
            Err(e) => Ok(self.fail_with(h, diag_of(&e))),
        }
    }

    /// SQLFetch / SQLFetchScroll: the next (or chosen) row into the bound
    /// columns; SQL_NO_DATA after the last.
    fn cli_fetch(&mut self, h: i32, orient: i64, offset: i64) -> Result<i64> {
        let job = self.io.job_key.clone();
        let got = with(&job, |c| {
            let s = c.stmts.get_mut(&h)?;
            if !s.has_result {
                return Some(None);
            }
            let n = s.rows.len() as i64;
            let to = match orient {
                SQL_FETCH_FIRST => 0,
                SQL_FETCH_LAST => n - 1,
                SQL_FETCH_PRIOR => s.pos - 1,
                SQL_FETCH_ABSOLUTE => offset - 1,
                SQL_FETCH_RELATIVE => s.pos + offset,
                _ => s.pos + 1,
            };
            if to < 0 || to >= n {
                s.pos = if to < 0 { -1 } else { n };
                return Some(Some(None));
            }
            s.pos = to;
            Some(Some(Some((
                s.rows[to as usize].clone(),
                s.cols.clone(),
                s.bound.clone(),
            ))))
        });
        let Some(got) = got else {
            return Ok(SQL_INVALID_HANDLE);
        };
        let Some(got) = got else {
            return Ok(self.fail(h, "24000", -501, "SQL0501: The cursor is not open."));
        };
        let Some((row, cols, bound)) = got else {
            return Ok(SQL_NO_DATA);
        };
        let mut info = false;
        for (k, b) in &bound {
            let i = (*k as usize).saturating_sub(1);
            let (Some(col), Some(v)) = (cols.get(i), row.get(i)) else {
                continue;
            };
            info |= self.put_column(b, col, v.as_deref())?;
        }
        Ok(if info {
            SQL_SUCCESS_WITH_INFO
        } else {
            SQL_SUCCESS
        })
    }

    /// A value into a bound buffer, in the C type asked for (the
    /// column's own form for SQL_C_DEFAULT); its length (or
    /// SQL_NULL_DATA) to the length/indicator. True when characters
    /// were cut to fit.
    fn put_column(&mut self, b: &Bound, col: &Col, v: Option<&str>) -> Result<bool> {
        self.put_value(b, col, v, false)
    }

    /// The same for an OUT or INOUT parameter: characters blank-padded to
    /// the buffer's length, no null (the buffer held the INOUT value;
    /// XMLSERVICE reads the parameter as the buffer trimmed of blanks and
    /// nulls).
    fn put_value(&mut self, b: &Bound, col: &Col, v: Option<&str>, param: bool) -> Result<bool> {
        let Some(v) = v else {
            if let Some(ind) = b.ind {
                self.put_int(&Value::Ptr(ind), SQL_NULL_DATA as i64, 4)?;
            }
            return Ok(false);
        };
        let Some(data) = b.data else {
            return Ok(false);
        };
        let at = Value::Ptr(data);
        let ctype = if b.ctype == SQL_DEFAULT {
            col.sqltype
        } else {
            b.ctype
        };
        let mut cut = false;
        let len: i64 = match ctype {
            SQL_INTEGER | SQL_SMALLINT | SQL_BIGINT => {
                let n: i64 = v
                    .trim()
                    .split('.')
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                let size = match ctype {
                    SQL_SMALLINT => 2,
                    SQL_BIGINT => 8,
                    _ => 4,
                };
                self.put_int(&at, n, size)?;
                size as i64
            }
            SQL_DECIMAL | SQL_NUMERIC => {
                let (digits, scale) = if b.buflen >= 256 {
                    ((b.buflen / 256) as u32, (b.buflen % 256) as u32)
                } else {
                    (col.size.max(1) as u32, col.scale.max(0) as u32)
                };
                let t = if ctype == SQL_DECIMAL {
                    DType::Packed(digits, scale)
                } else {
                    DType::Zoned(digits, scale)
                };
                let d = Dec::parse(v.trim()).unwrap_or_else(|_| Dec::from_i64(0));
                let mut out = vec![0u8; t.size()];
                data::store(
                    &t,
                    &Value::Num(d),
                    &mut out,
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
                self.put_bytes(&at, &out)?;
                t.size() as i64
            }
            SQL_DOUBLE | SQL_FLOAT => {
                let x: f64 = v.trim().parse().unwrap_or(0.0);
                self.put_bytes(&at, &x.to_be_bytes())?;
                8
            }
            SQL_REAL => {
                let x: f32 = v.trim().parse().unwrap_or(0.0);
                self.put_bytes(&at, &x.to_be_bytes())?;
                4
            }
            SQL_BINARY | SQL_VARBINARY => {
                let bytes: Vec<u8> = (0..v.len() / 2)
                    .filter_map(|i| u8::from_str_radix(v.get(2 * i..2 * i + 2)?, 16).ok())
                    .collect();
                let n = bytes.len().min(b.buflen.max(0) as usize);
                cut = n < bytes.len();
                self.put_bytes(&at, &bytes[..n])?;
                bytes.len() as i64
            }
            _ => {
                // Characters, a null after them when there is room; a
                // value longer than the buffer cut to its length less one
                // and a null (a value as long as the buffer fills it:
                // XMLSERVICE binds CHAR(n) columns to n bytes).
                let text = e(v);
                let max = b.buflen.max(0) as usize;
                if param && max > 0 {
                    let mut out = text.clone();
                    cut = out.len() > max;
                    out.resize(max, 0x40);
                    self.put_bytes(&at, &out)?;
                    if let Some(ind) = b.ind {
                        self.put_int(&Value::Ptr(ind), text.len() as i64, 4)?;
                    }
                    return Ok(cut);
                }
                let n = if max == 0 {
                    0
                } else if text.len() <= max {
                    text.len()
                } else {
                    cut = true;
                    max - 1
                };
                let mut out = text[..n].to_vec();
                if n < max {
                    out.push(0);
                }
                self.put_bytes(&at, &out)?;
                text.len() as i64
            }
        };
        if let Some(ind) = b.ind {
            self.put_int(&Value::Ptr(ind), len, 4)?;
        }
        Ok(cut)
    }
}
