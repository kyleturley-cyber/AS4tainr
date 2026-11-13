//! The file host server (as-file, port 8473, server ID 0xE002): the
//! integrated file system as the IBM Toolbox for Java's IFSFile,
//! IFSFileInputStream, IFSFileOutputStream and IFSRandomAccessFile (and
//! ACS's file transfers) reach it. A connection starts as the remote
//! command server's does (hostcmd.rs `serve_as`).
//!
//! Requests and replies (JTOpen IFS*Req and IFS*Rep, IFSFileDescriptor-
//! ImplRemote): a header, a chain indicator at 20 (bit 1: more replies
//! follow), then the template:
//!
//! - exchange attributes (0x0016: data stream level at 22, flags 24, the
//!   client's largest block 26, CP 0x000A its CCSIDs; reply 0x8009: the
//!   server's level, largest block and CCSIDs at the same places);
//! - list attributes (0x000A: a file handle at 22, the name's CCSID 26,
//!   the attribute list level 36, then CP 0x0002 the name, its last part
//!   perhaps a pattern with * and ?): a reply 0x8005 for each object
//!   (creation, change and access dates at 22, 30, 38 as seconds and
//!   microseconds, size 46, fixed attributes 50 (0x10 directory), type 54
//!   (1 file, 2 directory), name CCSID 73, 8-byte size 81, symbolic link
//!   91, then CP 0x0002 the name), chained, or a return code;
//! - open (0x0002: the name's CCSID 22, data CCSID 28, access 30,
//!   duplicate option 36 (1 create or open, 2 create or replace, 4
//!   create or fail, 8 open or fail, 16 replace or fail), CP 0x0002 the
//!   name at 64; reply 0x8002: the handle at 22, the data CCSID 34, the
//!   dates 38, 46, 54, the 8-byte size 89);
//! - read (0x0003: the handle 22, the length 34, the 8-byte offset 50;
//!   reply 0x8003: the data's CCSID 22, CP 0x0020 the data at 24);
//! - write (0x0004: the handle 22, the 8-byte offset 46, CP 0x0020 the
//!   data at 54; reply 0x800B: return code 22, the size before 24, bytes
//!   not written 28);
//! - close (0x0009; reply 0x8004), commit (0x0006), change attributes
//!   (0x000B: by handle, the file's size), create directory (0x000D),
//!   delete file (0x000C), delete directory (0x000E), rename (0x000F:
//!   CP 0x0003 the old name, CP 0x0004 the new, flags 34: 1 replace);
//! - other requests: the return code reply (0x8001: the code at 22; 0
//!   done, 2 file not found, 3 path not found, 4 already there, 9
//!   directory not empty, 18 no more files, 22 no more data, 23 not
//!   supported).
//!
//! A file opened is read into the connection and written back when it
//! is committed or closed.

use crate::hostsrv::{param, reply, Ds};
use std::collections::HashMap;

/// The data stream level the server answers with: 16 (8-byte sizes and
/// offsets).
const DS_LEVEL: u16 = 16;
/// The largest block of data in a reply.
const MAX_BLOCK: u32 = 1 << 20;

/// Return codes (JTOpen IFSReturnCodeRep).
const FILE_NOT_FOUND: u16 = 2;
const PATH_NOT_FOUND: u16 = 3;
const DUPLICATE: u16 = 4;
const ACCESS_DENIED: u16 = 13;
const INVALID_HANDLE: u16 = 6;
const INVALID_NAME: u16 = 7;
const DIR_NOT_EMPTY: u16 = 9;
const NO_MORE_FILES: u16 = 18;
const NO_MORE_DATA: u16 = 22;
const NOT_SUPPORTED: u16 = 23;

/// A file open on the connection.
pub struct OpenFile {
    path: String,
    data: Vec<u8>,
    ccsid: i32,
    changed: bool,
}

/// The files a connection has open: by handle the file's path, by path
/// its bytes (one copy for the handles a file is open by, as the Toolbox
/// opens a second handle to change a file's size).
#[derive(Default)]
pub struct Files {
    handles: HashMap<u32, String>,
    open: HashMap<String, (OpenFile, usize)>,
    next: u32,
    /// Object handles of lookups, by handle the path.
    looked: HashMap<u32, String>,
}

impl Files {
    fn get(&self, handle: u32) -> Option<&OpenFile> {
        self.open.get(self.handles.get(&handle)?).map(|(f, _)| f)
    }

    fn get_mut(&mut self, handle: u32) -> Option<&mut OpenFile> {
        let path = self.handles.get(&handle)?;
        self.open.get_mut(path).map(|(f, _)| f)
    }

    /// A handle closed: the file's bytes kept while another handle has it.
    fn close(&mut self, handle: u32) {
        let Some(path) = self.handles.remove(&handle) else {
            return;
        };
        if let Some((_, n)) = self.open.get_mut(&path) {
            *n -= 1;
            if *n == 0 {
                self.open.remove(&path);
            }
        }
    }
}

/// Answer one file server request: its replies (more than one for a
/// list).
pub async fn request(
    srv: &mut crate::mapepire::Server<'_>,
    files: &mut Files,
    ds: &Ds,
) -> Vec<Vec<u8>> {
    let corr = ds.correlation();
    let client = srv.job.client;
    let user = srv.job.user.clone();
    let rc = |code: u16| vec![return_code(corr, code)];
    if std::env::var_os("AS4TAINR_HOSTIFS_TRACE").is_some() {
        eprintln!(
            "as-file request {:04X} {:02X?}",
            ds.req_id(),
            &ds.0[20..ds.0.len().min(90)]
        );
    }
    match ds.req_id() {
        0x0016 => vec![exchange_attributes(corr)],
        0x000A => list(client, files, ds).await,
        0x0002 => vec![open(client, files, ds, &user).await],
        0x0003 => vec![read(files, ds)],
        0x0004 => vec![write(files, ds)],
        0x0006 => rc(flush(client, files, ds.u32(22), &user).await),
        0x0009 => {
            let code = flush(client, files, ds.u32(22), &user).await;
            files.close(ds.u32(22));
            vec![close_reply(corr, code)]
        }
        0x000B => rc(change_attributes(files, ds)),
        0x000C => rc(on_name(client, ds, 28, Op::DeleteFile, &user).await),
        0x000D => rc(on_name(client, ds, 28, Op::CreateDir, &user).await),
        0x000E => rc(on_name(client, ds, 30, Op::DeleteDir, &user).await),
        0x000F => rc(rename(client, ds).await),
        // User handles (JTOpen AS400ImplRemote createUserHandle): a seed
        // (0x0023, reply 0x8010 with the server's at 22), then the user
        // handle (0x0024, reply 0x8011: return code 22, handle 24). The
        // connection is signed on already: its user is the handle's.
        0x0023 => {
            let mut t = vec![0u8; 2];
            t.extend(crate::hostsrv::seed());
            vec![reply(0, 0, 0xE002, corr, 0x8010, &t, &[])]
        }
        0x0024 | 0x002B => {
            let mut t = vec![0u8; 2];
            t.extend(0u16.to_be_bytes());
            t.extend(1u32.to_be_bytes());
            let id = if ds.req_id() == 0x0024 {
                0x8011
            } else {
                0x802B
            };
            vec![reply(0, 0, 0xE002, corr, id, &t, &[])]
        }
        0x001A => vec![lookup(client, files, ds).await],
        // Get file system (0x0021; reply 0x800F: from 22 an LL and CP and
        // the type, 4 bytes), free handle (0x001D).
        0x0021 => {
            let p = param(0x0001, &0u32.to_be_bytes());
            vec![reply(0, 0, 0xE002, corr, 0x800F, &[0, 0], &p)]
        }
        0x001D => {
            files.looked.remove(&ds.u32(22));
            rc(0)
        }
        _ => rc(NOT_SUPPORTED),
    }
}

/// A reply of the file server: chain indicator (0: the last), then the
/// rest of the template.
fn ifs_reply(corr: u32, id: u16, chained: bool, rest: &[u8], params: &[u8]) -> Vec<u8> {
    let mut t = (chained as u16).to_be_bytes().to_vec();
    t.extend(rest);
    reply(0, 0, 0xE002, corr, id, &t, params)
}

fn return_code(corr: u32, code: u16) -> Vec<u8> {
    ifs_reply(corr, 0x8001, false, &code.to_be_bytes(), &[])
}

fn close_reply(corr: u32, code: u16) -> Vec<u8> {
    let mut t = code.to_be_bytes().to_vec();
    t.extend([0u8; 12]);
    ifs_reply(corr, 0x8004, false, &t, &[])
}

fn exchange_attributes(corr: u32) -> Vec<u8> {
    let mut t = DS_LEVEL.to_be_bytes().to_vec();
    t.extend(4u16.to_be_bytes()); // dates GMT
    t.extend(MAX_BLOCK.to_be_bytes());
    let mut p = Vec::new();
    p.extend(1200u16.to_be_bytes());
    let ccsids = {
        let mut d = 8u32.to_be_bytes().to_vec();
        d.extend(0x000Au16.to_be_bytes());
        d.extend(p);
        d
    };
    t.extend(ccsids);
    ifs_reply(corr, 0x8009, false, &t, &[])
}

/// A name in its CCSID (UTF-16 for 1200 and 13488, else UTF-8).
fn name_text(ccsid: u16, b: &[u8]) -> String {
    match ccsid {
        1200 | 13488 | 0xF200 => {
            let u: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&u)
        }
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

/// The data of code point `cp` among the parameters from `from`.
fn cp_at(ds: &Ds, from: usize, cp: u16) -> Option<Vec<u8>> {
    ds.params(from)
        .into_iter()
        .find(|(c, _)| *c == cp)
        .map(|(_, d)| d.to_vec())
}

fn u64_at(ds: &Ds, at: usize) -> u64 {
    ds.0.get(at..at + 8)
        .map(|b| u64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
        .unwrap_or(0)
}

/// An IFS error as a return code.
fn code_of(e: &anyhow::Error) -> u16 {
    use crate::ifs::errno;
    match crate::ifs::errno_of(e) {
        errno::ENOENT => FILE_NOT_FOUND,
        errno::ENOTDIR => PATH_NOT_FOUND,
        errno::EEXIST => DUPLICATE,
        errno::ENOTEMPTY => DIR_NOT_EMPTY,
        errno::EACCES | errno::EPERM => ACCESS_DENIED,
        errno::EISDIR | errno::EINVAL | errno::ENAMETOOLONG => INVALID_NAME,
        _ => 25,
    }
}

/// A date as the file server writes it: seconds and microseconds.
fn date(us: i64) -> Vec<u8> {
    let mut d = ((us / 1_000_000) as u32).to_be_bytes().to_vec();
    d.extend(((us % 1_000_000) as u32).to_be_bytes());
    d
}

/// An object's attributes reply (0x8005).
fn attrs_reply(
    corr: u32,
    e: &crate::ifs::Entry,
    name: &str,
    chained: bool,
    restart: u32,
) -> Vec<u8> {
    let mut t = Vec::with_capacity(72);
    for _ in 0..3 {
        t.extend(date(e.changed_us));
    }
    t.extend((e.size.min(u32::MAX as i64) as u32).to_be_bytes()); // 46
    t.extend((if e.dir { 0x10u32 } else { 0x20 }).to_be_bytes()); // 50
    t.extend((if e.dir { 2u16 } else { 1 }).to_be_bytes()); // 54
    t.extend([0u8; 2]); // 56 extended attributes
    t.extend([0u8; 4]); // 58
    t.extend([0u8; 4]); // 62
    t.extend([0u8; 4]); // 66 version
    t.extend([0u8; 2]); // 70 amount accessed
    t.push(0); // 72 access history
    t.extend(1200u16.to_be_bytes()); // 73 name CCSID
    t.extend([0u8; 2]); // 75
    t.extend(restart.to_be_bytes()); // 77 restart ID
    t.extend((e.size as u64).to_be_bytes()); // 81
    t.push(0); // 89 file system type
    t.push(0);
    t.push(0); // 91 symbolic link
    let mut p = param(0x0002, &utf16(name));
    p.extend(param(0x0014, &(e.size as u64).to_be_bytes()));
    ifs_reply(corr, 0x8005, chained, &t, &p)
}

/// Whether a name matches a pattern of * and ?.
fn matches(pat: &str, name: &str) -> bool {
    fn m(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], n) || (!n.is_empty() && m(p, &n[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a == b && m(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    m(&p, &n)
}

/// List attributes: of an open file (by handle), of an object, or of
/// the objects of a directory a pattern names.
async fn list(client: &tokio_postgres::Client, files: &Files, ds: &Ds) -> Vec<Vec<u8>> {
    let corr = ds.correlation();
    let handle = ds.u32(22);
    if handle != 0 {
        let Some(f) = files.get(handle) else {
            return vec![return_code(corr, INVALID_HANDLE)];
        };
        return match stat(client, &f.path).await {
            Ok(Some(mut e)) => {
                e.size = f.data.len() as i64;
                let name = f.path.rsplit('/').next().unwrap_or("").to_string();
                vec![attrs_reply(corr, &e, &name, false, 0)]
            }
            _ => vec![return_code(corr, FILE_NOT_FOUND)],
        };
    }
    let ccsid = ds.u16(26);
    let Some(raw) = cp_at(ds, 40, 0x0002) else {
        return vec![return_code(corr, INVALID_NAME)];
    };
    let path = name_text(ccsid, &raw);
    let (dir, last) = match path.rsplit_once('/') {
        Some(("", l)) => ("/".to_string(), l.to_string()),
        Some((d, l)) => (d.to_string(), l.to_string()),
        None => (crate::ifs::current_dir(), path.clone()),
    };
    if !last.contains(['*', '?']) {
        return match stat(client, &path).await {
            Ok(Some(e)) => vec![attrs_reply(corr, &e, &last, false, 0)],
            Ok(None) => {
                let parent = stat(client, &dir).await.ok().flatten();
                let code = if parent.is_some_and(|p| p.dir) {
                    FILE_NOT_FOUND
                } else {
                    PATH_NOT_FOUND
                };
                vec![return_code(corr, code)]
            }
            Err(e) => vec![return_code(corr, code_of(&e))],
        };
    }
    let names = match list_dir(client, &dir).await {
        Ok(n) => n,
        Err(_) => return vec![return_code(corr, PATH_NOT_FOUND)],
    };
    if std::env::var_os("AS4TAINR_HOSTIFS_TRACE").is_some() {
        eprintln!("as-file list {dir} {last}: {names:?}");
    }
    let max = match ds.u16(34) {
        0xFFFF | 0 => usize::MAX,
        n => n as usize,
    };
    // The entries matching, each with its restart ID (its place in the
    // list, from 1). A request with a restart ID (CP 0x000E) or restart
    // name (CP 0x0007) continues after that entry (JTOpen IFSListAttrsReq;
    // IFSFileEnumeration passes the last entry it got).
    let matching: Vec<(u32, &String)> = names
        .iter()
        .filter(|(n, _)| matches(&last, n))
        .enumerate()
        .map(|(i, (n, _))| (i as u32 + 1, n))
        .collect();
    let after = if let Some(id) = cp_at(ds, 40, 0x000E).filter(|b| b.len() >= 4) {
        u32::from_be_bytes([id[0], id[1], id[2], id[3]])
    } else if let Some(rn) = cp_at(ds, 40, 0x0007) {
        let rn = name_text(ccsid, &rn);
        matching
            .iter()
            .find(|(_, n)| **n == rn)
            .map(|(i, _)| *i)
            // Gone since: nothing more rather than the list again.
            .unwrap_or(u32::MAX)
    } else {
        0
    };
    let mut out = Vec::new();
    for (id, name) in matching.into_iter().filter(|(i, _)| *i > after).take(max) {
        let full = if dir == "/" {
            format!("/{name}")
        } else {
            format!("{dir}/{name}")
        };
        if let Ok(Some(e)) = stat(client, &full).await {
            out.push(attrs_reply(corr, &e, name, true, id));
        }
    }
    out.push(return_code(corr, NO_MORE_FILES));
    out
}

/// Open: the file read into the connection (made first when the
/// duplicate file option says so).
async fn open(client: &tokio_postgres::Client, files: &mut Files, ds: &Ds, user: &str) -> Vec<u8> {
    let corr = ds.correlation();
    let ccsid = ds.u16(22);
    let data_ccsid = ds.u16(28);
    let option = ds.u16(36);
    let Some(raw) = cp_at(ds, 20 + ds.template_len(), 0x0002) else {
        return return_code(corr, INVALID_NAME);
    };
    let path = name_text(ccsid, &raw);
    let existing = match stat(client, &path).await {
        Ok(e) => e,
        Err(e) => return return_code(corr, code_of(&e)),
    };
    if existing.as_ref().is_some_and(|e| e.dir) {
        return return_code(corr, ACCESS_DENIED);
    }
    // Already open on the connection: the same bytes.
    if let Some((f, n)) = files.open.get_mut(&path) {
        match option {
            4 => return return_code(corr, DUPLICATE),
            2 | 16 => {
                f.data.clear();
                f.changed = true;
            }
            _ => {}
        }
        *n += 1;
        files.next += 1;
        files.handles.insert(files.next, path.clone());
        let (size, c) = (f.data.len() as u64, f.ccsid);
        let now = existing.as_ref().map_or(0, |e| e.changed_us);
        return open_reply(corr, files.next, c, 1, now, size);
    }
    let (data, file_ccsid, action) = match (&existing, option) {
        (Some(_), 4) => return return_code(corr, DUPLICATE),
        (None, 8 | 16) => return return_code(corr, FILE_NOT_FOUND),
        (Some(e), 2 | 16) => (Vec::new(), e.ccsid, 3u16),
        (Some(_), _) => match read_file(client, &path).await {
            Ok((d, c)) => (d, c, 1),
            Err(e) => return return_code(corr, code_of(&e)),
        },
        (None, _) => {
            let c = match data_ccsid {
                0 | 0xFFFF => 819,
                c => c as i32,
            };
            if let Err(e) = write_file(client, &path, &[], c, user).await {
                return return_code(corr, code_of(&e));
            }
            (Vec::new(), c, 2)
        }
    };
    files.next += 1;
    let handle = files.next;
    let changed = existing.is_some() && matches!(option, 2 | 16);
    let now = existing.as_ref().map_or(0, |e| e.changed_us);
    let size = data.len() as u64;
    files.handles.insert(handle, path.clone());
    files.open.insert(
        path.clone(),
        (
            OpenFile {
                path: path.clone(),
                data,
                ccsid: file_ccsid,
                changed,
            },
            1,
        ),
    );
    open_reply(corr, handle, file_ccsid, action, now, size)
}

/// The open reply (0x8002).
fn open_reply(
    corr: u32,
    handle: u32,
    file_ccsid: i32,
    action: u16,
    now: i64,
    size: u64,
) -> Vec<u8> {
    let mut t = handle.to_be_bytes().to_vec(); // 22
    t.extend((handle as u64).to_be_bytes()); // 26 file ID
    t.extend((file_ccsid as u16).to_be_bytes()); // 34
    t.extend(action.to_be_bytes()); // 36
    for _ in 0..3 {
        t.extend(date(now)); // 38, 46, 54
    }
    t.extend([0u8; 4]); // 62 size (8-byte below)
    t.extend(0x20u32.to_be_bytes()); // 66
    t.extend([0u8; 2]); // 70
    t.extend([0u8; 2]); // 72
    t.extend([0u8; 4]); // 74
    t.extend([0u8; 4]); // 78
    t.extend([0u8; 4]); // 82
    t.extend([0u8; 2]); // 86
    t.push(0); // 88
    t.extend(size.to_be_bytes()); // 89
    ifs_reply(corr, 0x8002, false, &t, &[])
}

fn read(files: &Files, ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let Some(f) = files.get(ds.u32(22)) else {
        return return_code(corr, INVALID_HANDLE);
    };
    let len = (ds.u32(34) as usize).min(MAX_BLOCK as usize);
    let at = if ds.template_len() >= 38 {
        u64_at(ds, 50) as usize
    } else {
        ds.u32(30) as usize
    };
    if at >= f.data.len() {
        return return_code(corr, NO_MORE_DATA);
    }
    let end = (at + len).min(f.data.len());
    let t = (f.ccsid as u16).to_be_bytes();
    ifs_reply(corr, 0x8003, false, &t, &param(0x0020, &f.data[at..end]))
}

fn write(files: &mut Files, ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let Some(f) = files.get_mut(ds.u32(22)) else {
        return return_code(corr, INVALID_HANDLE);
    };
    let at = if ds.template_len() >= 34 {
        u64_at(ds, 46) as usize
    } else {
        ds.u32(30) as usize
    };
    let data = cp_at(ds, 20 + ds.template_len(), 0x0020).unwrap_or_default();
    let before = f.data.len();
    if f.data.len() < at + data.len() {
        f.data.resize(at + data.len(), 0);
    }
    f.data[at..at + data.len()].copy_from_slice(&data);
    f.changed = true;
    let mut t = 0u16.to_be_bytes().to_vec();
    t.extend((before as u32).to_be_bytes());
    t.extend(0u32.to_be_bytes());
    t.extend((before as u64).to_be_bytes());
    ifs_reply(corr, 0x800B, false, &t, &[])
}

/// An open file's bytes written back when they changed.
async fn flush(client: &tokio_postgres::Client, files: &mut Files, handle: u32, user: &str) -> u16 {
    let Some(f) = files.get_mut(handle) else {
        return INVALID_HANDLE;
    };
    if !f.changed {
        return 0;
    }
    match write_file(client, &f.path, &f.data, f.ccsid, user).await {
        Ok(()) => {
            f.changed = false;
            0
        }
        Err(e) => code_of(&e),
    }
}

/// Change attributes of an open file: its size (the 8-byte size at
/// 68, set flag 0x0004 at 58... the Toolbox's setLength).
fn change_attributes(files: &mut Files, ds: &Ds) -> u16 {
    let handle = ds.u32(22);
    if handle == 0 {
        // Dates and attributes by name: not kept here.
        return 0;
    }
    let Some(f) = files.get_mut(handle) else {
        return INVALID_HANDLE;
    };
    let size = if ds.template_len() >= 56 {
        u64_at(ds, 68) as usize
    } else {
        ds.u32(64) as usize
    };
    f.data.resize(size, 0);
    f.changed = true;
    0
}

enum Op {
    CreateDir,
    DeleteFile,
    DeleteDir,
}

/// A request on a name (CP 0x0001 or 0x0002 at `at`).
async fn on_name(client: &tokio_postgres::Client, ds: &Ds, at: usize, op: Op, user: &str) -> u16 {
    let ccsid = ds.u16(22);
    let Some((_, raw)) = ds.params(at).into_iter().next() else {
        return INVALID_NAME;
    };
    let path = name_text(ccsid, raw);
    // Libraries, objects and members are made and deleted by commands.
    if qsys(&path).is_some() {
        return ACCESS_DENIED;
    }
    let r = match op {
        Op::CreateDir => crate::ifs::mkdir(client, &path, 0o755, user).await,
        Op::DeleteFile => match stat(client, &path).await {
            Ok(Some(e)) if e.dir => return ACCESS_DENIED,
            _ => crate::ifs::unlink(client, &path).await,
        },
        Op::DeleteDir => crate::ifs::rmdir(client, &path).await,
    };
    match r {
        Ok(()) => 0,
        Err(e) => code_of(&e),
    }
}

async fn rename(client: &tokio_postgres::Client, ds: &Ds) -> u16 {
    let ccsid = ds.u16(22);
    let replace = ds.u16(34) & 1 == 1;
    let (Some(from), Some(to)) = (cp_at(ds, 36, 0x0003), cp_at(ds, 36, 0x0004)) else {
        return INVALID_NAME;
    };
    let (from, to) = (name_text(ccsid, &from), name_text(ccsid, &to));
    if qsys(&from).is_some() || qsys(&to).is_some() {
        return ACCESS_DENIED;
    }
    if let Ok(Some(_)) = stat(client, &to).await {
        if !replace {
            return DUPLICATE;
        }
        if let Err(e) = crate::ifs::unlink(client, &to).await {
            return code_of(&e);
        }
    }
    match crate::ifs::rename(client, &from, &to).await {
        Ok(()) => 0,
        Err(e) => code_of(&e),
    }
}

/// Lookup (0x001A: the name's CCSID at 30, the attribute list level 36,
/// CP 0x0002 the name at 42): an object handle (reply 0x800C: the handle
/// at 22, then CP 0x0010 the object's attributes 1 (its ASP at 220 and
/// owner at 224, JTOpen IFSObjAttrs1) and CP 0x000F its attributes 2 (its
/// CCSID at 134, IFSObjAttrs2 for data stream level 16)).
async fn lookup(client: &tokio_postgres::Client, files: &mut Files, ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let Some(raw) = cp_at(ds, 42, 0x0002) else {
        return return_code(corr, INVALID_NAME);
    };
    let path = name_text(ds.u16(30), &raw);
    let e = match stat(client, &path).await {
        Ok(Some(e)) => e,
        Ok(None) => return return_code(corr, FILE_NOT_FOUND),
        Err(e) => return return_code(corr, code_of(&e)),
    };
    files.next += 1;
    let handle = files.next;
    files.looked.insert(handle, path);
    let mut oa1 = vec![0u8; 256];
    oa1[220..222].copy_from_slice(&1u16.to_be_bytes());
    oa1[224..234].copy_from_slice(&crate::hostsrv::ebcdic(&e.owner, 10));
    let mut oa2 = vec![0u8; 160];
    oa2[134..136].copy_from_slice(&(e.ccsid as u16).to_be_bytes());
    let mut p = param(0x0010, &oa1);
    p.extend(param(0x000F, &oa2));
    let mut t = vec![0u8; 2];
    t.extend(handle.to_be_bytes());
    reply(0, 0, 0xE002, corr, 0x800C, &t, &p)
}

// ------------------------------------------------------------ QSYS.LIB

/// A /QSYS.LIB path's parts (QSYS.LIB, LIB.LIB, OBJ.TYPE, MBR.MBR), in
/// upper case (object names are kept in upper case: IBM i documentation,
/// "QSYS.LIB file system"); None for another file system.
fn qsys(path: &str) -> Option<Vec<String>> {
    let up = path.to_ascii_uppercase();
    let parts: Vec<String> = up
        .trim_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    (parts.first().map(String::as_str) == Some("QSYS.LIB")).then_some(parts)
}

fn split_dot<'a>(p: &'a str, ty: &str) -> Option<&'a str> {
    p.strip_suffix(&format!(".{ty}"))
}

fn entry(path: &str, dir: bool, size: i64, changed_us: i64) -> crate::ifs::Entry {
    crate::ifs::Entry {
        path: path.to_string(),
        dir,
        ccsid: 37,
        mode: if dir { 0o755 } else { 0o644 },
        owner: "QSYS".into(),
        size,
        changed_us,
    }
}

/// A library's or object's last change in microseconds.
async fn object_time(
    client: &tokio_postgres::Client,
    lib: &str,
    name: &str,
    ty: &str,
) -> Option<i64> {
    client
        .query_opt(
            "SELECT (extract(epoch FROM coalesce(changed, created)) * 1000000)::bigint
               FROM as4tainr.objects WHERE library = $1 AND name = $2 AND objtype = $3",
            &[&lib, &name, &format!("*{ty}")],
        )
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<_, Option<i64>>(0).unwrap_or(0))
}

/// The record length's text width of a source file, or None for a data
/// file (or no file).
async fn source_width(client: &tokio_postgres::Client, lib: &str, file: &str) -> Option<usize> {
    client
        .query_opt(
            "SELECT rcdlen FROM as4tainr.source_files WHERE library = $1 AND name = $2",
            &[&lib, &file],
        )
        .await
        .ok()
        .flatten()
        .map(|r| (r.get::<_, i32>(0) - 12).max(0) as usize)
}

/// A member's bytes as the file server reads it: the text Qshell reads
/// (qsyslib.rs), in CCSID 37.
async fn member_bytes(
    client: &tokio_postgres::Client,
    lib: &str,
    file: &str,
    mbr: &str,
) -> Option<Vec<u8>> {
    let text = match source_width(client, lib, file).await {
        Some(width) => {
            let src: String = client
                .query_opt(
                    "SELECT source FROM as4tainr.source_members
                      WHERE library = $1 AND file = $2 AND member = $3",
                    &[&lib, &file, &mbr],
                )
                .await
                .ok()
                .flatten()?
                .get(0);
            crate::qsyslib::member_text(&src, width)
        }
        None if mbr == file => crate::qsyslib::data_member_text(client, lib, file)
            .await
            .ok()?,
        None => return None,
    };
    Some(crate::ifs::codec::bytes(37, &text))
}

/// stat() over the IFS and /QSYS.LIB.
async fn stat(
    client: &tokio_postgres::Client,
    path: &str,
) -> anyhow::Result<Option<crate::ifs::Entry>> {
    let Some(p) = qsys(path) else {
        return crate::ifs::stat(client, path).await;
    };
    Ok(match p.as_slice() {
        [_] => Some(entry("/QSYS.LIB", true, 0, 0)),
        [_, l] => {
            let Some(lib) = split_dot(l, "LIB") else {
                return Ok(None);
            };
            object_time(client, "QSYS", lib, "LIB")
                .await
                .map(|t| entry(path, true, 0, t))
        }
        [_, l, o] => {
            let Some(lib) = split_dot(l, "LIB") else {
                return Ok(None);
            };
            let Some((name, ty)) = o.rsplit_once('.') else {
                return Ok(None);
            };
            object_time(client, lib, name, ty)
                .await
                .map(|t| entry(path, ty == "FILE", 0, t))
        }
        [_, l, f, m] => {
            let (Some(lib), Some(file), Some(mbr)) = (
                split_dot(l, "LIB"),
                split_dot(f, "FILE"),
                split_dot(m, "MBR"),
            ) else {
                return Ok(None);
            };
            let t = object_time(client, lib, file, "FILE").await.unwrap_or(0);
            member_bytes(client, lib, file, mbr)
                .await
                .map(|b| entry(path, false, b.len() as i64, t))
        }
        _ => None,
    })
}

/// A directory's names over the IFS and /QSYS.LIB (libraries, a
/// library's objects, a file's members).
async fn list_dir(
    client: &tokio_postgres::Client,
    dir: &str,
) -> anyhow::Result<Vec<(String, bool)>> {
    let Some(p) = qsys(dir) else {
        return crate::ifs::list(client, dir).await;
    };
    let names =
        |rows: Vec<tokio_postgres::Row>| -> Vec<String> { rows.iter().map(|r| r.get(0)).collect() };
    Ok(match p.as_slice() {
        [_] => names(
            client
                .query(
                    "SELECT name || '.LIB' FROM as4tainr.objects WHERE library = 'QSYS'
                      AND objtype = '*LIB' AND name NOT LIKE '#Q%' ORDER BY name",
                    &[],
                )
                .await?,
        )
        .into_iter()
        .map(|n| (n, true))
        .collect(),
        [_, l] => {
            let lib = split_dot(l, "LIB").unwrap_or("");
            client
                .query(
                    "SELECT name || '.' || substr(objtype, 2), objtype = '*FILE'
                       FROM as4tainr.objects WHERE library = $1 ORDER BY name, objtype",
                    &[&lib],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect()
        }
        [_, l, f] => {
            let lib = split_dot(l, "LIB").unwrap_or("");
            let file = split_dot(f, "FILE").unwrap_or("");
            if source_width(client, lib, file).await.is_some() {
                names(
                    client
                        .query(
                            "SELECT member || '.MBR' FROM as4tainr.source_members
                              WHERE library = $1 AND file = $2 ORDER BY member",
                            &[&lib, &file],
                        )
                        .await?,
                )
                .into_iter()
                .map(|n| (n, false))
                .collect()
            } else {
                vec![(format!("{file}.MBR"), false)]
            }
        }
        _ => vec![],
    })
}

/// A file's bytes and CCSID over the IFS and /QSYS.LIB members.
async fn read_file(client: &tokio_postgres::Client, path: &str) -> anyhow::Result<(Vec<u8>, i32)> {
    let Some(p) = qsys(path) else {
        return crate::ifs::read_file(client, path).await;
    };
    if let [_, l, f, m] = p.as_slice() {
        if let (Some(lib), Some(file), Some(mbr)) = (
            split_dot(l, "LIB"),
            split_dot(f, "FILE"),
            split_dot(m, "MBR"),
        ) {
            if let Some(b) = member_bytes(client, lib, file, mbr).await {
                return Ok((b, 37));
            }
        }
    }
    Err(anyhow::Error::new(crate::ifs::Errno(
        crate::ifs::errno::EISDIR,
    )))
}

/// A file written over the IFS and /QSYS.LIB: a source member's lines
/// (CCSID 37 text, each line a record, as CPYFRMSTMF puts them), made
/// when it is not there.
async fn write_file(
    client: &tokio_postgres::Client,
    path: &str,
    data: &[u8],
    ccsid: i32,
    user: &str,
) -> anyhow::Result<()> {
    let Some(p) = qsys(path) else {
        return crate::ifs::write_file(client, path, data, ccsid, 0o644, user).await;
    };
    let denied = || anyhow::Error::new(crate::ifs::Errno(crate::ifs::errno::EACCES));
    let [_, l, f, m] = p.as_slice() else {
        return Err(denied());
    };
    let (Some(lib), Some(file), Some(mbr)) = (
        split_dot(l, "LIB"),
        split_dot(f, "FILE"),
        split_dot(m, "MBR"),
    ) else {
        return Err(denied());
    };
    if source_width(client, lib, file).await.is_none() {
        return Err(denied());
    }
    let text = crate::ifs::codec::text(37, data).replace("\r\n", "\n");
    let source = text
        .strip_suffix('\n')
        .unwrap_or(&text)
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let (srctype, mtext): (String, String) = client
        .query_opt(
            "SELECT srctype, text FROM as4tainr.source_members
              WHERE library = $1 AND file = $2 AND member = $3",
            &[&lib, &file, &mbr],
        )
        .await?
        .map(|r| (r.get(0), r.get(1)))
        .unwrap_or_default();
    crate::source::put_member(
        client,
        &crate::source::Member {
            library: lib.to_string(),
            file: file.to_string(),
            name: mbr.to_string(),
            srctype,
            text: mtext,
            source,
            stmf: String::new(),
            ccsid: 37,
            changed_us: None,
        },
        true,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_and_replies_where_jtopen_reads_them() {
        assert!(matches("*", "a.txt"));
        assert!(matches("*.txt", "a.txt") && !matches("*.txt", "a.csv"));
        assert!(matches("a?c", "abc") && !matches("a?c", "ac"));
        // IFSExchangeAttrRep: level at 22, largest block 26, CCSID 36.
        let r = exchange_attributes(1);
        assert_eq!(u16::from_be_bytes([r[18], r[19]]), 0x8009);
        assert_eq!(u16::from_be_bytes([r[22], r[23]]), 16);
        assert_eq!(u16::from_be_bytes([r[36], r[37]]), 1200);
        // IFSListAttrsRep: dates 22, size 46, type 54, the 8-byte size
        // 81, the name after the template (its length at 16).
        let e = crate::ifs::Entry {
            path: "/d/a.txt".into(),
            dir: false,
            ccsid: 819,
            mode: 0o644,
            owner: "U".into(),
            size: 5,
            changed_us: 1_700_000_000_123_456,
        };
        let r = attrs_reply(1, &e, "a.txt", true, 0);
        assert_eq!(u16::from_be_bytes([r[20], r[21]]) & 1, 1, "chained");
        assert_eq!(
            u32::from_be_bytes([r[22], r[23], r[24], r[25]]),
            1_700_000_000
        );
        assert_eq!(u32::from_be_bytes([r[26], r[27], r[28], r[29]]), 123_456);
        assert_eq!(u32::from_be_bytes([r[46], r[47], r[48], r[49]]), 5);
        assert_eq!(u16::from_be_bytes([r[54], r[55]]), 1);
        assert_eq!(u64::from_be_bytes(r[81..89].try_into().unwrap()), 5);
        let name_at = 20 + u16::from_be_bytes([r[16], r[17]]) as usize;
        assert_eq!(&r[name_at + 4..name_at + 6], &[0, 2]);
        assert_eq!(name_text(1200, &r[name_at + 6..name_at + 16]), "a.txt");
    }
}
