//! C runtime and UNIX-type APIs a program can call through prototypes
//! (EXTPROC('open') and so on, bound from QC2LE): the integrated file
//! system (open, read, write, close, lseek, unlink, mkdir, rmdir, access,
//! rename), errno (__errno, strerror, perror), strings and memory
//! (strlen, strcpy, strcmp, memcpy, memmove, memset, memcmp, atoi, atol),
//! system() and Qp0zLprintf.
//!
//! Written from IBM's UNIX-type API and ILE C runtime references. File
//! descriptors belong to the job (as on IBM i): a descriptor opened by one
//! program can be used by another program of the job until it is closed.
//! A stream file's bytes are read when it is opened and written back when
//! it is closed (and when the program that wrote ends).

use super::super::mem::HEAP_BASE;
use super::*;
use crate::ifs::{self, codec, errno};
use std::sync::Mutex;

/// open() flags (QSYSINC/H FCNTL).
const O_RDONLY: i64 = 1;
const O_WRONLY: i64 = 2;
const O_RDWR: i64 = 4;
const O_CREAT: i64 = 8;
const O_EXCL: i64 = 16;
const O_CCSID: i64 = 32;
const O_TRUNC: i64 = 64;
const O_APPEND: i64 = 256;
const O_CODEPAGE: i64 = 8_388_608;
const O_TEXTDATA: i64 = 16_777_216;
const O_TEXT_CREAT: i64 = 33_554_432;

/// The job's CCSID: as4tainr's character data is CCSID 37.
const JOB_CCSID: i32 = 37;
/// Open files a job may have.
const MAX_FDS: usize = 200;

/// The C functions as4tainr provides, as bound names (upper case).
pub const C_FUNCTIONS: &[&str] = &[
    "OPEN",
    "OPEN64",
    "READ",
    "WRITE",
    "CLOSE",
    "LSEEK",
    "LSEEK64",
    "UNLINK",
    "MKDIR",
    "RMDIR",
    "ACCESS",
    "RENAME",
    "__ERRNO",
    "STRERROR",
    "PERROR",
    "STRLEN",
    "STRCPY",
    "STRNCPY",
    "STRCMP",
    "STRNCMP",
    "STRCAT",
    "MEMCPY",
    "MEMMOVE",
    "MEMSET",
    "MEMCMP",
    "ATOI",
    "ATOL",
    "ATOLL",
    "SYSTEM",
    "QP0ZLPRINTF",
];

/// An open stream file.
#[derive(Debug, Clone)]
struct Stream {
    path: String,
    data: Vec<u8>,
    pos: usize,
    read: bool,
    write: bool,
    append: bool,
    /// O_TEXTDATA: (file CCSID, open CCSID) for conversion.
    text: Option<(i32, i32)>,
    dirty: bool,
    /// Writes of this program not yet stored.
    owner: u32,
}

/// Open files by job, by descriptor.
static FDS: Mutex<Option<HashMap<String, HashMap<i32, Stream>>>> = Mutex::new(None);

fn with_fds<R>(job: &str, f: impl FnOnce(&mut HashMap<i32, Stream>) -> R) -> R {
    let mut g = FDS.lock().unwrap_or_else(|e| e.into_inner());
    let jobs = g.get_or_insert_with(HashMap::new);
    f(jobs.entry(job.to_string()).or_default())
}

/// A job ended: its open files are closed (written).
pub fn end_job(job: &str) -> Vec<(String, Vec<u8>)> {
    let mut g = FDS.lock().unwrap_or_else(|e| e.into_inner());
    g.as_mut()
        .and_then(|j| j.remove(job))
        .map(|fds| {
            fds.into_values()
                .filter(|s| s.dirty)
                .map(|s| (s.path, s.data))
                .collect()
        })
        .unwrap_or_default()
}

impl<'a> Machine<'a> {
    /// A C function, called with the prototype's arguments. None when
    /// `name` is not one.
    pub(super) fn c_call(&mut self, name: &str, args: &[Arg]) -> Option<Result<Value>> {
        if !C_FUNCTIONS.contains(&name) {
            return None;
        }
        Some(self.c_call_inner(name, args))
    }

    fn c_call_inner(&mut self, name: &str, args: &[Arg]) -> Result<Value> {
        let val = |i: usize| -> Value {
            match args.get(i) {
                Some(Arg::Val(v)) => v.clone(),
                _ => Value::Fig(Fig::Null),
            }
        };
        let int = |i: usize| -> i64 {
            match args.get(i) {
                Some(Arg::Val(v)) => {
                    v.num().ok().and_then(|d| d.to_i128().ok()).unwrap_or(0) as i64
                }
                _ => 0,
            }
        };
        let num = |n: i64| Ok(Value::Num(Dec::from_i64(n)));
        match name {
            "OPEN" | "OPEN64" => {
                let path = self.c_string(&val(0))?;
                let r = self.c_open(&path, int(1), int(2), int(3), int(4), args.len());
                self.c_result(r)
            }
            "CLOSE" => {
                let r = self.c_close(int(0) as i32);
                self.c_result(r.map(|_| 0))
            }
            "READ" => {
                let r = self.c_read(int(0) as i32, &val(1), int(2));
                self.c_result(r)
            }
            "WRITE" => {
                let r = self.c_write(int(0) as i32, &val(1), int(2));
                self.c_result(r)
            }
            "LSEEK" | "LSEEK64" => {
                let r = self.c_lseek(int(0) as i32, int(1), int(2));
                self.c_result(r)
            }
            "UNLINK" | "RMDIR" | "MKDIR" | "ACCESS" => {
                let path = self.c_string(&val(0))?;
                let client = self.io.client;
                let user = self.io.user.clone();
                let r = match name {
                    "UNLINK" => self.block(ifs::unlink(client, &path)),
                    "RMDIR" => self.block(ifs::rmdir(client, &path)),
                    "MKDIR" => {
                        self.block(ifs::mkdir(client, &path, (int(1) & 0o7777) as i32, &user))
                    }
                    _ => self.block(ifs::stat(client, &path)).and_then(|e| match e {
                        Some(_) => Ok(()),
                        None => Err(ifs::Errno(errno::ENOENT).into()),
                    }),
                };
                self.c_result(r.map(|_| 0))
            }
            "RENAME" => {
                let old = self.c_string(&val(0))?;
                let new = self.c_string(&val(1))?;
                let r = self.block(ifs::rename(self.io.client, &old, &new));
                self.c_result(r.map(|_| 0))
            }
            "__ERRNO" => Ok(self.errno_ptr()),
            "STRERROR" => {
                let text = ifs::errno_text(int(0) as i32);
                Ok(self.c_static_string(&text))
            }
            "PERROR" => {
                let prefix = match val(0) {
                    Value::Fig(Fig::Null) => String::new(),
                    p => a(&self.str_at(&p, None)?.chars()?),
                };
                let n = self.errno_value();
                let text = ifs::errno_text(n);
                self.job_log(&if prefix.is_empty() {
                    text
                } else {
                    format!("{prefix}: {text}")
                });
                num(0)
            }
            "STRLEN" => {
                let s = self.str_at(&val(0), None)?.chars()?;
                num(s.len() as i64)
            }
            "STRCPY" | "STRCAT" | "STRNCPY" => {
                let mut src = self.str_at(&val(1), None)?.chars()?;
                let dst = val(0);
                let at = if name == "STRCAT" {
                    self.str_at(&dst, None)?.chars()?.len()
                } else {
                    0
                };
                if name == "STRNCPY" {
                    // At most n bytes; null characters fill the rest.
                    src.resize(int(2).max(0) as usize, 0);
                } else {
                    src.push(0);
                }
                let (b, o) = self.addr(&dst, at + src.len())?;
                self.bufs[b][o + at..o + at + src.len()].copy_from_slice(&src);
                Ok(dst)
            }
            "STRCMP" | "STRNCMP" => {
                let mut x = self.str_at(&val(0), None)?.chars()?;
                let mut y = self.str_at(&val(1), None)?.chars()?;
                if name == "STRNCMP" {
                    let n = int(2).max(0) as usize;
                    x.truncate(n);
                    y.truncate(n);
                }
                num(match x.cmp(&y) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                })
            }
            "MEMCPY" | "MEMMOVE" => {
                let n = int(2).max(0) as usize;
                let (sb, so) = self.addr(&val(1), n)?;
                let bytes = self.bufs[sb][so..so + n].to_vec();
                let dst = val(0);
                let (db, d) = self.addr(&dst, n)?;
                self.bufs[db][d..d + n].copy_from_slice(&bytes);
                Ok(dst)
            }
            "MEMSET" => {
                let n = int(2).max(0) as usize;
                let dst = val(0);
                let (db, d) = self.addr(&dst, n)?;
                self.bufs[db][d..d + n].fill(int(1) as u8);
                Ok(dst)
            }
            "MEMCMP" => {
                let n = int(2).max(0) as usize;
                let (xb, xo) = self.addr(&val(0), n)?;
                let (yb, yo) = self.addr(&val(1), n)?;
                let o = self.bufs[xb][xo..xo + n].cmp(&self.bufs[yb][yo..yo + n]);
                num(match o {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                })
            }
            "ATOI" | "ATOL" | "ATOLL" => {
                let s = a(&self.str_at(&val(0), None)?.chars()?);
                let t = s.trim_start();
                let end = t
                    .char_indices()
                    .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+'))))
                    .map(|(i, _)| i)
                    .unwrap_or(t.len());
                num(t[..end].parse::<i64>().unwrap_or(0))
            }
            "SYSTEM" => {
                // ILE C system(): 0 when the command ran, 1 when it failed,
                // -1 for a null pointer.
                let cmd = match val(0) {
                    Value::Fig(Fig::Null) => return num(-1),
                    p => a(&self.str_at(&p, None)?.chars()?),
                };
                num(if self.run_cl(cmd.trim()).is_ok() {
                    0
                } else {
                    1
                })
            }
            "QP0ZLPRINTF" => {
                let mut vals = Vec::new();
                for i in 0..args.len() {
                    let v = val(i);
                    // %s arguments may be pointers to strings.
                    vals.push(match v {
                        Value::Ptr(_) => self.str_at(&v, None)?,
                        other => other,
                    });
                }
                let text = self.printf(&vals)?;
                self.job_log(&text);
                num(text.len() as i64)
            }
            _ => bail!("{name} is not available"),
        }
    }

    /// A C function's result: the value, or -1 with errno set.
    fn c_result(&mut self, r: Result<i64>) -> Result<Value> {
        match r {
            Ok(n) => Ok(Value::Num(Dec::from_i64(n))),
            Err(e) => {
                self.set_errno(ifs::errno_of(&e));
                Ok(Value::Num(Dec::from_i64(-1)))
            }
        }
    }

    /// The null-terminated string a pointer argument addresses, as text
    /// (path names are in the job's CCSID).
    fn c_string(&self, p: &Value) -> Result<String> {
        if matches!(p, Value::Fig(Fig::Null)) {
            return Err(ifs::Errno(errno::EFAULT).into());
        }
        Ok(a(&self.str_at(p, None)?.chars()?))
    }

    /// The errno variable: 4 bytes of heap storage per program run.
    fn errno_ptr(&mut self) -> Value {
        let h = match self.errno_block {
            Some(h) => h,
            None => {
                let h = self.bufs.heap.alloc(4);
                self.errno_block = Some(h);
                h
            }
        };
        self.ptr_to(HEAP_BASE + h, 0)
    }

    fn set_errno(&mut self, n: i32) {
        let Value::Ptr(p) = self.errno_ptr() else {
            return;
        };
        self.bufs[HEAP_BASE + p.buf as usize][..4].copy_from_slice(&n.to_be_bytes());
    }

    fn errno_value(&mut self) -> i32 {
        let Value::Ptr(p) = self.errno_ptr() else {
            return 0;
        };
        let b = &self.bufs[HEAP_BASE + p.buf as usize];
        i32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    /// A null-terminated string in heap storage (strerror's result).
    fn c_static_string(&mut self, text: &str) -> Value {
        let mut bytes = data::e(text);
        bytes.push(0);
        let h = self.bufs.heap.alloc(bytes.len());
        self.bufs.heap.blocks[h] = bytes;
        self.ptr_to(HEAP_BASE + h, 0)
    }

    fn job(&self) -> String {
        self.io.job_key.clone()
    }

    /// open(): (IBM i open() API) the access mode, O_CREAT with the mode
    /// and O_EXCL, O_TRUNC, O_APPEND, and the CCSIDs: O_CCSID or
    /// O_CODEPAGE tags a new file (else the job's CCSID); O_TEXTDATA
    /// converts between the file's CCSID and the open CCSID (the 4th
    /// argument, the 5th with O_TEXT_CREAT, else the job's).
    fn c_open(
        &mut self,
        path: &str,
        flags: i64,
        mode: i64,
        conv: i64,
        text_conv: i64,
        nargs: usize,
    ) -> Result<i64> {
        let access = flags & (O_RDONLY | O_WRONLY | O_RDWR);
        if ![O_RDONLY, O_WRONLY, O_RDWR].contains(&access) {
            return Err(ifs::Errno(errno::EINVAL).into());
        }
        if flags & O_CCSID != 0 && flags & O_CODEPAGE != 0 {
            return Err(ifs::Errno(errno::EINVAL).into());
        }
        if flags & O_TEXT_CREAT != 0
            && (flags & O_CREAT == 0
                || flags & O_TEXTDATA == 0
                || flags & (O_CCSID | O_CODEPAGE) == 0)
        {
            return Err(ifs::Errno(errno::EINVAL).into());
        }
        let given = flags & (O_CCSID | O_CODEPAGE) != 0 && nargs >= 4;
        // A code page is taken as the CCSID of that number.
        let conv = if given && conv > 0 { conv as i32 } else { 0 };
        let client = self.io.client;
        let existing = self.block(ifs::stat(client, path))?;
        let (data, file_ccsid, created) = match existing {
            Some(e) if e.dir => {
                if access != O_RDONLY || flags & O_TRUNC != 0 {
                    return Err(ifs::Errno(errno::EISDIR).into());
                }
                (Vec::new(), e.ccsid, false)
            }
            Some(_) if flags & O_CREAT != 0 && flags & O_EXCL != 0 => {
                return Err(ifs::Errno(errno::EEXIST).into())
            }
            Some(e) => {
                let (d, c) = self.block(ifs::read_file(client, path))?;
                let d = if flags & O_TRUNC != 0 && access != O_RDONLY {
                    Vec::new()
                } else {
                    d
                };
                let _ = e;
                (d, c, false)
            }
            None if flags & O_CREAT == 0 => return Err(ifs::Errno(errno::ENOENT).into()),
            None => {
                let ccsid = if conv > 0 { conv } else { JOB_CCSID };
                let user = self.io.user.clone();
                self.block(ifs::write_file(
                    client,
                    path,
                    &[],
                    ccsid,
                    (mode & 0o7777) as i32,
                    &user,
                ))?;
                (Vec::new(), ccsid, true)
            }
        };
        let text = if flags & O_TEXTDATA != 0 {
            let open_ccsid = if flags & O_TEXT_CREAT != 0 && created {
                // 0: the job's CCSID.
                if text_conv > 0 {
                    text_conv as i32
                } else {
                    JOB_CCSID
                }
            } else if created {
                // A new file without O_TEXT_CREAT: no conversion.
                file_ccsid
            } else if conv > 0 {
                conv
            } else {
                JOB_CCSID
            };
            if file_ccsid != open_ccsid
                && !(codec::supported(file_ccsid) && codec::supported(open_ccsid))
            {
                return Err(ifs::Errno(errno::ECONVERT).into());
            }
            (file_ccsid != open_ccsid).then_some((file_ccsid, open_ccsid))
        } else {
            None
        };
        let owner = self.owner;
        let truncated = flags & O_TRUNC != 0 && access != O_RDONLY && !created;
        let stream = Stream {
            path: path.to_string(),
            data,
            pos: 0,
            read: access != O_WRONLY,
            write: access != O_RDONLY,
            append: flags & O_APPEND != 0,
            text,
            dirty: truncated,
            owner,
        };
        with_fds(&self.job(), |fds| {
            let fd = (0..MAX_FDS as i32).find(|n| !fds.contains_key(n));
            match fd {
                Some(fd) => {
                    fds.insert(fd, stream);
                    Ok(fd as i64)
                }
                None => Err(ifs::Errno(errno::EMFILE).into()),
            }
        })
    }

    fn c_close(&mut self, fd: i32) -> Result<()> {
        let s = with_fds(&self.job(), |fds| fds.remove(&fd));
        let Some(s) = s else {
            return Err(ifs::Errno(errno::EBADF).into());
        };
        if s.dirty {
            let user = self.io.user.clone();
            self.block(ifs::write_file(
                self.io.client,
                &s.path,
                &s.data,
                37,
                0o644,
                &user,
            ))?;
        }
        Ok(())
    }

    /// The program ends: what it wrote to files still open is stored.
    pub(super) fn c_flush(&mut self) {
        let pending: Vec<(String, Vec<u8>)> = with_fds(&self.job(), |fds| {
            fds.values_mut()
                .filter(|s| s.dirty)
                .map(|s| {
                    s.dirty = false;
                    (s.path.clone(), s.data.clone())
                })
                .collect()
        });
        let user = self.io.user.clone();
        for (path, data) in pending {
            let _ = self.block(ifs::write_file(
                self.io.client,
                &path,
                &data,
                37,
                0o644,
                &user,
            ));
        }
    }

    fn c_read(&mut self, fd: i32, buf: &Value, n: i64) -> Result<i64> {
        if n < 0 {
            return Err(ifs::Errno(errno::EINVAL).into());
        }
        let n = n as usize;
        let (b, o) = self
            .addr(buf, n)
            .map_err(|_| anyhow::Error::from(ifs::Errno(errno::EFAULT)))?;
        let out = with_fds(&self.job(), |fds| -> Result<Vec<u8>> {
            let s = fds.get_mut(&fd).ok_or(ifs::Errno(errno::EBADF))?;
            if !s.read {
                return Err(ifs::Errno(errno::EBADF).into());
            }
            let rest = &s.data[s.pos.min(s.data.len())..];
            Ok(match s.text {
                None => {
                    let k = rest.len().min(n);
                    s.pos += k;
                    rest[..k].to_vec()
                }
                Some((file, open)) => {
                    // Whole characters, converted, up to n bytes.
                    let mut out = Vec::new();
                    let mut used = 0;
                    while let Some((c, len)) = codec::decode_char(file, &rest[used..]) {
                        let mut enc = Vec::new();
                        codec::encode_char(open, c, &mut enc);
                        if out.len() + enc.len() > n {
                            break;
                        }
                        out.extend(enc);
                        used += len;
                    }
                    s.pos += used;
                    out
                }
            })
        })?;
        self.bufs[b][o..o + out.len()].copy_from_slice(&out);
        Ok(out.len() as i64)
    }

    fn c_write(&mut self, fd: i32, buf: &Value, n: i64) -> Result<i64> {
        if n < 0 {
            return Err(ifs::Errno(errno::EINVAL).into());
        }
        let n = n as usize;
        let (b, o) = self
            .addr(buf, n)
            .map_err(|_| anyhow::Error::from(ifs::Errno(errno::EFAULT)))?;
        let bytes = self.bufs[b][o..o + n].to_vec();
        let owner = self.owner;
        with_fds(&self.job(), |fds| -> Result<i64> {
            let s = fds.get_mut(&fd).ok_or(ifs::Errno(errno::EBADF))?;
            if !s.write {
                return Err(ifs::Errno(errno::EBADF).into());
            }
            let data = match s.text {
                None => bytes,
                Some((file, open)) => codec::convert(open, file, &bytes),
            };
            if s.append {
                s.pos = s.data.len();
            }
            if s.pos > s.data.len() {
                s.data.resize(s.pos, 0);
            }
            let end = s.pos + data.len();
            if end > s.data.len() {
                s.data.resize(end, 0);
            }
            s.data[s.pos..end].copy_from_slice(&data);
            s.pos = end;
            s.dirty = true;
            s.owner = owner;
            // The number of bytes taken from the buffer.
            Ok(n as i64)
        })
    }

    fn c_lseek(&mut self, fd: i32, off: i64, whence: i64) -> Result<i64> {
        with_fds(&self.job(), |fds| -> Result<i64> {
            let s = fds.get_mut(&fd).ok_or(ifs::Errno(errno::EBADF))?;
            let base = match whence {
                0 => 0,
                1 => s.pos as i64,
                2 => s.data.len() as i64,
                _ => return Err(ifs::Errno(errno::EINVAL).into()),
            };
            let to = base + off;
            if to < 0 {
                return Err(ifs::Errno(errno::EINVAL).into());
            }
            s.pos = to as usize;
            Ok(to)
        })
    }
}
