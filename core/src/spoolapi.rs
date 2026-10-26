//! The spooled file APIs: QGYOLSPL (Open List of Spooled Files), QGYGTLE
//! (Get List Entries) and QGYCLST (Close List) over open lists, QSPOPNSP,
//! QSPGETSP and QSPCLOSP (Open, Get and Close Spooled File Data).
//!
//! Written from IBM's public API documentation (the formats' field
//! tables) and RPGUnit's templates for them (RPGUnit/RPGUnit, CPL-1.0,
//! whose acceptance tests read its report through these APIs). The print
//! data of a spooled file is SNA character string (SCS) data built from
//! the stored lines (spool.rs): new lines (x'15') before a line's text,
//! as many as it moves down, a carriage return (x'0D') for a line
//! printed over, a form feed (x'0C') at a new page.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use tokio_postgres::Client;

use crate::rpg::data::e as ebcdic;

/// An open list (QGYOLSPL, QGYOLJBL): its records, all of one length or
/// each its own (offsets inside them are from a receiver's start, so the
/// list keeps the records as built for one receiver each).
struct OpenList {
    records: Vec<Vec<u8>>,
}

static LISTS: std::sync::Mutex<Option<HashMap<u32, OpenList>>> = std::sync::Mutex::new(None);
static NEXT_HANDLE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Keep a list; its request handle.
pub fn list_open(records: Vec<Vec<u8>>) -> u32 {
    let h = NEXT_HANDLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = LISTS.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(h, OpenList { records });
    }
    h
}

/// Records `start` (1-based) on, at most `count` (-1: all).
pub fn list_get(handle: u32, start: i32, count: i32) -> Option<(usize, Vec<Vec<u8>>)> {
    let g = LISTS.lock().ok()?;
    let l = g.as_ref()?.get(&handle)?;
    let from = (start.max(1) - 1) as usize;
    let n = if count < 0 {
        usize::MAX
    } else {
        count as usize
    };
    Some((
        l.records.len(),
        l.records.iter().skip(from).take(n).cloned().collect(),
    ))
}

pub fn list_close(handle: u32) {
    if let Ok(mut g) = LISTS.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(&handle);
        }
    }
}

/// The list information (80 bytes) every list API returns: total records,
/// records returned, request handle, record length (0: they vary),
/// information complete ('C'), date and time created (CYYMMDDHHMMSS),
/// list status ('2': complete), a reserved byte, length of the
/// information returned in the receiver, first record in the receiver
/// (0: none), 40 reserved bytes. The reserved bytes are x'00', the length
/// 0 with no record returned (QGYOLSPL, PUB400 2026-10-23).
pub fn list_info(
    total: usize,
    returned: usize,
    handle: u32,
    rec_len: usize,
    first: usize,
    info_len: usize,
) -> [u8; 80] {
    let mut li = [0u8; 80];
    li[0..4].copy_from_slice(&(total as i32).to_be_bytes());
    li[4..8].copy_from_slice(&(returned as i32).to_be_bytes());
    li[8..12].copy_from_slice(&handle.to_be_bytes());
    li[12..16].copy_from_slice(&(rec_len as i32).to_be_bytes());
    li[16] = ebcdic("C")[0];
    let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::now();
    let created = format!(
        "{}{:02}{mo:02}{d:02}{h:02}{mi:02}{s:02}",
        u8::from(y >= 2000),
        y % 100
    );
    li[17..30].copy_from_slice(&ebcdic(&created));
    li[30] = ebcdic("2")[0];
    li[32..36].copy_from_slice(&(info_len as i32).to_be_bytes());
    li[36..40].copy_from_slice(&(first as i32).to_be_bytes());
    li
}

fn field(s: &str, n: usize) -> Vec<u8> {
    let mut b = ebcdic(&crate::qtemp::logical(s));
    b.resize(n, 0x40);
    b.truncate(n);
    b
}

/// A spooled file as the APIs see it.
#[derive(Debug, Clone)]
pub struct Splf {
    pub id: i64,
    pub job_number: i64,
    pub job_name: String,
    pub job_user: String,
    pub file: String,
    pub number: i32,
    pub status: String,
    pub user_data: String,
    pub outq: String,
    pub outq_lib: String,
    pub pages: i32,
    /// CYYMMDD and HHMMSS it was opened.
    pub opened: (String, String),
    pub bytes: i64,
}

const SPLF_COLS: &str = "s.id, s.job, s.job_name, s.file, s.number, s.status, s.user_data, s.outq, s.outq_lib, s.pages,
    to_char(s.created AT TIME ZONE 'UTC', 'YYYYMMDD'), to_char(s.created AT TIME ZONE 'UTC', 'HH24MISS'),
    (SELECT coalesce(sum(length(l.text) + 1), 0) FROM as4tainr.spool_lines l WHERE l.spl = s.id)";

fn splf_row(r: &tokio_postgres::Row) -> Splf {
    let job_name: String = r.get(2);
    // number/user/name
    let mut it = job_name.split('/');
    let (_, user, name) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""));
    let ymd: String = r.get(10);
    let c = if ymd.as_str() >= "20000000" { "1" } else { "0" };
    Splf {
        id: r.get(0),
        job_number: r.get(1),
        job_name: name.to_string(),
        job_user: user.to_string(),
        file: r.get(3),
        number: r.get(4),
        status: r.get(5),
        user_data: r.get(6),
        outq: r.get(7),
        outq_lib: r.get(8),
        pages: r.get(9),
        opened: (format!("{c}{}", &ymd[2..]), r.get(11)),
        bytes: r.get(12),
    }
}

/// Filter information, format OSPF0100: number of user names, user
/// names (10 characters and 2 reserved each), number of output queues,
/// output queue names and libraries (20 each), form type, user-specified
/// data, number of statuses, statuses (12 each), number of devices,
/// devices (12 each).
#[derive(Debug, Default)]
pub struct Filter {
    pub users: Vec<String>,
    pub outqs: Vec<(String, String)>,
    pub form_type: String,
    pub user_data: String,
    pub statuses: Vec<String>,
    /// OSPF0200: the first and last moments created (CYYMMDDHHMMSS), as
    /// given (*ALL and *LAST: none).
    pub created_from: Option<String>,
    pub created_to: Option<String>,
}

/// Filter information, format OSPF0200 (QGYOLSPL's filter format
/// parameter): its length; offset to, number and length of the user
/// names, output queues, statuses and devices; form type, user-specified
/// data, system name, starting date and time created (*ALL), ending date
/// and time created (*LAST).
pub fn parse_filter_0200(b: &[u8]) -> Filter {
    let i4 = |a: usize| -> usize {
        b.get(a..a + 4)
            .map(|x| i32::from_be_bytes(x.try_into().unwrap()).max(0) as usize)
            .unwrap_or(0)
    };
    let txt = |a: usize, n: usize| -> String {
        b.get(a..a + n)
            .map(|x| crate::rpg::data::a(x).trim().to_ascii_uppercase())
            .unwrap_or_default()
    };
    let list = |off: usize, n: usize, len: usize| -> Vec<String> {
        (0..n).map(|k| txt(off + k * len, len)).collect()
    };
    let mut f = Filter {
        users: list(i4(4), i4(8), i4(12).max(1)),
        ..Default::default()
    };
    let qlen = i4(24).max(1);
    f.outqs = (0..i4(20))
        .map(|k| {
            let at = i4(16) + k * qlen;
            (txt(at, 10), txt(at + 10, 10))
        })
        .collect();
    f.statuses = list(i4(28), i4(32), i4(36).max(1));
    f.form_type = txt(52, 10);
    f.user_data = txt(62, 10);
    let moment = |d: String, t: String, special: &str| -> Option<String> {
        (!d.is_empty() && d != special).then(|| format!("{d:0<7}{t:0<6}"))
    };
    f.created_from = moment(txt(80, 7), txt(87, 6), "*ALL");
    f.created_to = moment(txt(93, 7), txt(100, 6), "*LAST");
    f
}

pub fn parse_filter(b: &[u8]) -> Filter {
    let i4 = |a: usize| -> usize {
        b.get(a..a + 4)
            .map(|x| i32::from_be_bytes(x.try_into().unwrap()).max(0) as usize)
            .unwrap_or(0)
    };
    let txt = |a: usize, n: usize| -> String {
        b.get(a..a + n)
            .map(|x| crate::rpg::data::a(x).trim().to_ascii_uppercase())
            .unwrap_or_default()
    };
    let mut f = Filter::default();
    let mut at = 0;
    let n = i4(at);
    at += 4;
    for _ in 0..n {
        f.users.push(txt(at, 10));
        at += 12;
    }
    let n = i4(at);
    at += 4;
    for _ in 0..n {
        f.outqs.push((txt(at, 10), txt(at + 10, 10)));
        at += 20;
    }
    f.form_type = txt(at, 10);
    at += 10;
    f.user_data = txt(at, 10);
    at += 10;
    let n = i4(at);
    at += 4;
    for _ in 0..n {
        f.statuses.push(txt(at, 10));
        at += 12;
    }
    f
}

/// The spooled files a filter selects, oldest first. `current`: the
/// user *CURRENT names; `job`: a job number to keep to.
pub async fn select(
    client: &Client,
    f: &Filter,
    current: &str,
    job: Option<i64>,
) -> Result<Vec<Splf>> {
    let rows = client
        .query(
            &format!("SELECT {SPLF_COLS} FROM as4tainr.spooled_files s ORDER BY s.id"),
            &[],
        )
        .await?;
    let mut out = Vec::new();
    for r in &rows {
        let s = splf_row(r);
        let user: String = {
            let jn: String = r.get(2);
            jn.split('/').nth(1).unwrap_or("").to_string()
        };
        let user_ok = f.users.is_empty()
            || f.users.iter().any(|u| match u.as_str() {
                "*ALL" => true,
                "*CURRENT" => user.eq_ignore_ascii_case(current),
                u => user.eq_ignore_ascii_case(u),
            });
        let outq_ok = f.outqs.is_empty()
            || f.outqs.iter().any(|(q, l)| {
                q == "*ALL"
                    || (q.eq_ignore_ascii_case(&s.outq)
                        && (l.is_empty() || l == "*LIBL" || l.eq_ignore_ascii_case(&s.outq_lib)))
            });
        let data_ok = f.user_data.is_empty()
            || f.user_data == "*ALL"
            || f.user_data.eq_ignore_ascii_case(s.user_data.trim());
        let status_ok = f.statuses.is_empty()
            || f.statuses.iter().any(|st| {
                st == "*ALL"
                    || st
                        .trim_start_matches('*')
                        .eq_ignore_ascii_case(status_word(&s.status))
            });
        let form_ok = f.form_type.is_empty() || f.form_type == "*ALL" || f.form_type == "*STD";
        let job_ok = job.is_none_or(|j| j == s.job_number);
        let at = format!("{}{}", s.opened.0, s.opened.1);
        let time_ok = f.created_from.as_ref().is_none_or(|x| at >= *x)
            && f.created_to.as_ref().is_none_or(|x| at <= *x);
        if user_ok && outq_ok && data_ok && status_ok && form_ok && job_ok && time_ok {
            out.push(s);
        }
    }
    Ok(out)
}

/// A stored status as the filter's special values name it (*READY...).
fn status_word(st: &str) -> &'static str {
    match st {
        "RDY" => "READY",
        "OPN" => "OPEN",
        "HLD" => "HELD",
        "SAV" => "SAVED",
        "CLO" => "CLOSED",
        "WTR" => "WRITING",
        _ => "",
    }
}

/// The status number of format OSPL0300 (1 ready, 2 open, 3 closed, 4
/// saved, 5 writing, 6 held).
fn status_number(st: &str) -> i32 {
    match st {
        "RDY" => 1,
        "OPN" => 2,
        "CLO" => 3,
        "SAV" => 4,
        "WTR" => 5,
        "HLD" => 6,
        _ => 0,
    }
}

/// One spooled file in format OSPL0300 (136 bytes): job name, user and
/// number, spooled file name and number, status, date and time opened,
/// schedule, job system name, user-specified data, form type, output
/// queue and library, auxiliary storage pool, size, size multiplier,
/// total pages, copies left, priority, 3 reserved, the Internet print
/// protocol job identifier (QGYOLSPL API, OSPL0300 format).
pub fn ospl0300(s: &Splf) -> Vec<u8> {
    let mut b = Vec::with_capacity(136);
    b.extend(field(&s.job_name, 10));
    b.extend(field(&s.job_user, 10));
    b.extend(field(&format!("{:06}", s.job_number), 6));
    b.extend(field(&s.file, 10));
    b.extend(s.number.to_be_bytes());
    b.extend(status_number(&s.status).to_be_bytes());
    b.extend(field(&s.opened.0, 7));
    b.extend(field(&s.opened.1, 6));
    b.extend(field("2", 1));
    b.extend(field(&crate::report::system_name(), 10));
    b.extend(field(&s.user_data, 10));
    b.extend(field("*STD", 10));
    b.extend(field(&s.outq, 10));
    b.extend(field(&s.outq_lib, 10));
    b.extend(1i32.to_be_bytes());
    b.extend((((s.bytes + 4095) / 4096) as i32).max(1).to_be_bytes());
    b.extend(4096i32.to_be_bytes());
    b.extend(s.pages.to_be_bytes());
    b.extend(1i32.to_be_bytes());
    b.extend(field("5", 1));
    b.extend([0u8; 3]);
    // The Internet print protocol job identifier (offset 132): no IPP
    // jobs here.
    b.extend(0i32.to_be_bytes());
    b
}

/// Open spooled files (QSPOPNSP): handle → spooled file.
static OPEN: std::sync::Mutex<Option<HashMap<i32, (i64, i32)>>> = std::sync::Mutex::new(None);
static NEXT_SPLF_HANDLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// QSPOPNSP: the spooled file of a job (`job`: None for this job) by
/// name and number (-1: the last, 0: the only one).
pub async fn open(
    client: &Client,
    job: Option<i64>,
    this_job: Option<i64>,
    file: &str,
    number: i32,
    buffers: i32,
) -> Result<i32> {
    let job = job
        .or(this_job)
        .ok_or_else(|| anyhow!("The job named was not found or has no spooled files."))?;
    let rows = client
        .query(
            "SELECT id, number FROM as4tainr.spooled_files WHERE job = $1 AND file = $2 ORDER BY number",
            &[&job, &file.trim().to_ascii_uppercase()],
        )
        .await?;
    let found: Option<i64> = match number {
        -1 => rows.last().map(|r| r.get(0)),
        0 if rows.len() == 1 => rows.first().map(|r| r.get(0)),
        n => rows
            .iter()
            .find(|r| r.get::<_, i32>(1) == n)
            .map(|r| r.get(0)),
    };
    let Some(id) = found else {
        bail!(
            "Spooled file {} number {number} was not found.",
            file.trim()
        );
    };
    let h = NEXT_SPLF_HANDLE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = OPEN.lock() {
        g.get_or_insert_with(HashMap::new).insert(h, (id, buffers));
    }
    Ok(h)
}

/// The spooled file a handle opened and the number of buffers asked for
/// at the open.
pub fn opened(handle: i32) -> Option<(i64, i32)> {
    OPEN.lock().ok()?.as_ref()?.get(&handle).copied()
}

pub fn close(handle: i32) -> bool {
    OPEN.lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|m| m.remove(&handle)))
        .is_some()
}

/// The print data of a spooled file in SCS: per page a form feed before
/// it (not before the first), then for each line as many new lines as it
/// moves down from the line before (from line 1 at the top of a page), its text and
/// a carriage return (a line printed over the one before: its text after
/// that carriage return); the last page ends with a form feed. IBM i 7.5
/// writes a report so (PUB400 2026-10-26, tools/golden/spool.txt s_dump,
/// GLDSPL's spooled file: "text CR NL text CR ... CR FF"), with NUL bytes
/// before some controls and absolute vertical positions for skips, which
/// as4tainr does not write (fidelity-gaps).
pub async fn scs(client: &Client, id: i64) -> Result<(Vec<u8>, Vec<usize>)> {
    let rows = client
        .query(
            "SELECT page, line, text FROM as4tainr.spool_lines WHERE spl = $1 ORDER BY seq",
            &[&id],
        )
        .await?;
    let mut b = Vec::new();
    let mut page_starts = Vec::new();
    let (mut page, mut line) = (0i32, 0i32);
    for r in &rows {
        let (p, l, t): (i32, i32, String) = (r.get(0), r.get(1), r.get(2));
        if p != page {
            if page != 0 {
                b.push(0x0C);
            }
            page_starts.push(b.len());
            page = p;
            line = 1;
        }
        if l > line {
            b.extend(std::iter::repeat_n(0x15u8, (l - line) as usize));
            line = l;
        }
        b.extend(ebcdic(t.trim_end()));
        b.push(0x0D);
    }
    if page != 0 {
        b.push(0x0C);
    }
    Ok((b, page_starts))
}

/// QSPGETSP format SPFR0300 into a user space (Get Spooled File Data
/// API, generic header section; the values as IBM i 7.5 gives them,
/// PUB400 2026-10-26 tools/golden/spool.txt s_dump): 64 bytes for the
/// user, the header's size (64), structure level 0200, spooled file
/// level V6R1M0, the format, complete ('C'), the size of the space used,
/// the offset to the first buffer (for SPFR0300 the print data, at 128),
/// buffers requested (as at the open) and returned (1), the size of the
/// print data, complete pages, the first page (1) and the offset to it in
/// the print data (0); then the print data.
pub fn spfr0300(data: &[u8], page_starts: &[usize], requested: i32) -> Vec<u8> {
    const HDR: usize = 128;
    let mut b = vec![0u8; HDR];
    b[64..68].copy_from_slice(&64i32.to_be_bytes());
    b[68..72].copy_from_slice(&ebcdic("0200"));
    b[72..78].copy_from_slice(&ebcdic("V6R1M0"));
    b[78..86].copy_from_slice(&ebcdic("SPFR0300"));
    b[86] = ebcdic("C")[0];
    let used = HDR + data.len();
    b[88..92].copy_from_slice(&(used as i32).to_be_bytes());
    b[92..96].copy_from_slice(&(HDR as i32).to_be_bytes());
    b[96..100].copy_from_slice(&requested.to_be_bytes());
    b[100..104].copy_from_slice(&1i32.to_be_bytes());
    b[104..108].copy_from_slice(&(data.len() as i32).to_be_bytes());
    b[108..112].copy_from_slice(&(page_starts.len() as i32).to_be_bytes());
    b[112..116].copy_from_slice(&i32::from(!page_starts.is_empty()).to_be_bytes());
    b[116..120].copy_from_slice(&(page_starts.first().copied().unwrap_or(0) as i32).to_be_bytes());
    b.extend(data);
    b
}
