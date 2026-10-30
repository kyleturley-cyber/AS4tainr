//! C runtime and UNIX-type APIs a program can call through prototypes
//! (EXTPROC('open') and so on, bound from QC2LE): the integrated file
//! system (open, read, write, close, lseek, unlink, mkdir, rmdir, access,
//! rename), errno (__errno, strerror, perror), strings and memory
//! (strlen, strcpy, strcmp, memcpy, memmove, memset, memcmp, atoi, atol,
//! strtok, strstr, strchr, strrchr, strspn, strcspn, strcasecmp and the
//! like, toupper, tolower), abs, rand and srand, tmpnam, system() and
//! Qp0zLprintf.
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
/// C functions whose parameter (by its position) is a buffer they fill:
/// passed by reference, it goes as the buffer's address even when the
/// prototype says CONST.
pub(super) const C_OUT_BUFFERS: &[(&str, usize)] = &[
    ("GETCWD", 0),
    ("STAT", 1),
    ("LSTAT", 1),
    ("STAT64", 1),
    ("LSTAT64", 1),
    ("FSTAT", 1),
    ("FSTAT64", 1),
    // localtime_r and gmtime_r fill the struct tm they are given.
    ("LOCALTIME_R", 1),
    ("GMTIME_R", 1),
    // mitime builds the MI time in the variable it is given.
    ("MITIME", 0),
];

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
    "GETENV",
    "PUTENV",
    "QP0ZGETENV",
    "QP0ZPUTENV",
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
    "PRINTF",
    "PUTS",
    "CHDIR",
    "GETCWD",
    "CVTHC",
    "CVTCH",
    "MITIME",
    "WAITTIME",
    "_ROPEN",
    "_RWRITE",
    "_RCLOSE",
    "PREAD",
    "PWRITE",
    "PREAD64",
    "PWRITE64",
    "GETHOSTNAME",
    "STAT",
    "LSTAT",
    "STAT64",
    "LSTAT64",
    "TIME",
    "LOCALTIME",
    "LOCALTIME_R",
    "GMTIME",
    "GMTIME_R",
    "STRFTIME",
    "STRTOK",
    "STRSTR",
    "STRCHR",
    "STRRCHR",
    "STRSPN",
    "STRCSPN",
    "STRCASECMP",
    "STRNCASECMP",
    "STRCMPI",
    "STRICMP",
    "TOUPPER",
    "TOLOWER",
    "ABS",
    "RAND",
    "SRAND",
    "TMPNAM",
    "_C_IFS_TMPNAM",
    "SLEEP",
    "USLEEP",
    "POW",
];

/// strtok's position in the string it is splitting, by job.
static STRTOK_AT: Mutex<Option<HashMap<String, Option<super::super::mem::Ptr>>>> = Mutex::new(None);

/// rand()'s state, by job (srand sets it; 1 before).
static RAND_STATE: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// Names tmpnam made, for the next one.
static TMPNAM_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
            // The MI functions cvthc(result, source, n): n hexadecimal
            // digits (EBCDIC '0'-'9', 'A'-'F') of the source's bytes; and
            // cvtch(result, source, n): the bytes n hexadecimal digits
            // stand for (ILE C/MI built-ins, bound from the system).
            "CVTHC" | "CVTCH" => {
                let n = int(2).max(0) as usize;
                let digits = b"0123456789ABCDEF";
                if name == "CVTHC" {
                    let (sb, so) = self.addr(&val(1), n.div_ceil(2))?;
                    let src: Vec<u8> = self.bufs[sb][so..so + n.div_ceil(2)].to_vec();
                    let hex: Vec<u8> = (0..n)
                        .map(|i| {
                            let byte = src[i / 2];
                            let d = if i % 2 == 0 { byte >> 4 } else { byte & 15 };
                            data::e(&(digits[d as usize] as char).to_string())[0]
                        })
                        .collect();
                    let (rb, ro) = self.addr(&val(0), n)?;
                    self.bufs[rb][ro..ro + n].copy_from_slice(&hex);
                } else {
                    let (sb, so) = self.addr(&val(1), n)?;
                    let src = data::a(&self.bufs[sb][so..so + n]);
                    let nib: Vec<u8> = src
                        .chars()
                        .map(|c| c.to_digit(16).unwrap_or(0) as u8)
                        .collect();
                    let out: Vec<u8> = nib
                        .chunks(2)
                        .map(|c| (c[0] << 4) | c.get(1).copied().unwrap_or(0))
                        .collect();
                    let (rb, ro) = self.addr(&val(0), out.len())?;
                    self.bufs[rb][ro..ro + out.len()].copy_from_slice(&out);
                }
                Ok(Value::Fig(Fig::Null))
            }
            // Record I/O to a printer file (ILE C _Ropen, _Rwrite,
            // _Rclose): each _Rwrite prints a line on the job's spooled
            // file (overrides apply as for RPG printer files). The _RFILE
            // pointer addresses the file's name. Database files are not
            // opened this way here (NULL).
            "_ROPEN" => {
                let f = self.c_string(&val(0))?.trim().to_ascii_uppercase();
                let file = f.rsplit('/').next().unwrap_or(&f).to_string();
                let libs = self.io.libl.clone();
                let is_db = self
                    .block(crate::catalog::object_exists(
                        self.io.client,
                        &libs,
                        &file,
                        "*FILE",
                    ))
                    .unwrap_or(false)
                    && file != "QSYSPRT";
                if is_db {
                    return Ok(Value::Fig(Fig::Null));
                }
                self.ensure_printer(&file)?;
                Ok(self.c_static_string(&file))
            }
            "_RWRITE" => {
                let file = self.c_string(&val(0))?;
                let n = int(2).max(0) as usize;
                let (b, o) = self.addr(&val(1), n)?;
                let text = data::a(&self.bufs[b][o..o + n]);
                if let Some(p) = self.printers.get_mut(&file) {
                    if p.line > crate::spool::PAGE_LENGTH {
                        p.page += 1;
                        p.line = 1;
                    }
                    p.buffer.push((p.page, p.line, text.trim_end().to_string()));
                    p.line += 1;
                }
                self.flush_printer(&file)?;
                Ok(self.c_static_string(""))
            }
            "_RCLOSE" => {
                let file = self.c_string(&val(0))?;
                self.flush_printer(&file)?;
                if let Some(p) = self.printers.remove(&file) {
                    if let Some(id) = p.spl {
                        let client = self.io.client;
                        self.block(crate::spool::close(client, id))?;
                    }
                }
                num(0)
            }
            // pread/pwrite(fd, buf, n, offset): at the offset, the file
            // position left as it was.
            "PREAD" | "PWRITE" | "PREAD64" | "PWRITE64" => {
                let fd = int(0) as i32;
                let r = (|| -> Result<i64> {
                    let here = self.c_lseek(fd, 0, 1)?;
                    self.c_lseek(fd, int(3), 0)?;
                    let n = if name.starts_with("PREAD") {
                        self.c_read(fd, &val(1), int(2))
                    } else {
                        self.c_write(fd, &val(1), int(2))
                    };
                    self.c_lseek(fd, here, 0)?;
                    n
                })();
                self.c_result(r)
            }
            // gethostname(buf, len): the system's name.
            "GETHOSTNAME" => {
                let host =
                    std::env::var("AS4TAINR_SYSTEM_NAME").unwrap_or_else(|_| "AS4TAINR".into());
                let mut bytes = data::e(&host);
                bytes.push(0);
                if (int(1).max(0) as usize) < bytes.len() {
                    return self.c_result(Err(ifs::Errno(errno::EINVAL).into()));
                }
                let (b, o) = self.addr(&val(0), bytes.len())?;
                self.bufs[b][o..o + bytes.len()].copy_from_slice(&bytes);
                num(0)
            }
            // stat(path, buf), lstat: the IFS object's attributes in
            // struct stat (QSYSINC/H SYSSTAT): mode (S_IFDIR 0040000 or
            // S_IFREG 0100000 and the permission bits), size, access,
            // modification and change times, block size, allocated size,
            // object type (*DIR, *STMF), CCSID; 0, or -1 with errno.
            "STAT" | "LSTAT" | "STAT64" | "LSTAT64" => {
                let path = self.c_string(&val(0))?;
                let r = self.block(ifs::stat(self.io.client, &path));
                let e = match r {
                    Ok(Some(e)) => e,
                    Ok(None) => return self.c_result(Err(ifs::Errno(errno::ENOENT).into())),
                    Err(e) => return self.c_result(Err(e)),
                };
                let secs = (e.changed_us / 1_000_000) as i32;
                let mode: u32 = if e.dir { 0o040000 } else { 0o100000 } | (e.mode as u32 & 0o7777);
                let mut b = Vec::with_capacity(128);
                b.extend(mode.to_be_bytes());
                b.extend(0u32.to_be_bytes()); // ino
                b.extend(1u16.to_be_bytes()); // nlink
                b.extend([0u8; 2]);
                b.extend(0u32.to_be_bytes()); // uid
                b.extend(0u32.to_be_bytes()); // gid
                b.extend((e.size as i32).to_be_bytes());
                for _ in 0..3 {
                    b.extend(secs.to_be_bytes());
                }
                b.extend(0u32.to_be_bytes()); // dev
                b.extend(4096u32.to_be_bytes()); // blksize
                b.extend((((e.size + 4095) / 4096 * 4096) as u32).to_be_bytes());
                b.extend(data::e(&format!(
                    "{:<11}",
                    if e.dir { "*DIR" } else { "*STMF" }
                )));
                b.push(0);
                b.extend(0u16.to_be_bytes()); // codepage
                b.extend((e.ccsid as u16).to_be_bytes());
                b.resize(128, 0);
                match args.get(1) {
                    Some(Arg::Ref(l)) => {
                        let (bf, o) = (l.buf, l.off);
                        let n = 128.min(self.bufs[bf].len().saturating_sub(o));
                        self.bufs[bf][o..o + n].copy_from_slice(&b[..n]);
                    }
                    _ => {
                        let (bf, o) = self.addr(&val(1), 128)?;
                        self.bufs[bf][o..o + 128].copy_from_slice(&b);
                    }
                }
                num(0)
            }
            // time(ptr): seconds since 1970 (UTC, the job's time zone),
            // also stored where ptr points.
            "TIME" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if let Value::Ptr(_) = val(0) {
                    let (b, o) = self.addr(&val(0), 4)?;
                    self.bufs[b][o..o + 4].copy_from_slice(&(now as i32).to_be_bytes());
                }
                num(now)
            }
            // localtime(time), localtime_r(time, tm), gmtime, gmtime_r:
            // the broken-down time (struct tm: nine int(10): sec, min,
            // hour, mday, mon 0-11, year - 1900, wday 0 Sunday, yday 0-365,
            // isdst), UTC.
            "LOCALTIME" | "LOCALTIME_R" | "GMTIME" | "GMTIME_R" => {
                let t: i64 = match args.first() {
                    Some(Arg::Ref(l)) => self.bufs[l.buf]
                        .get(l.off..l.off + 4)
                        .map(|x| i32::from_be_bytes(x.try_into().unwrap_or([0; 4])) as i64)
                        .unwrap_or(0),
                    Some(Arg::Val(Value::Ptr(_))) => {
                        let (b, o) = self.addr(&val(0), 4)?;
                        i32::from_be_bytes(self.bufs[b][o..o + 4].try_into().unwrap_or([0; 4]))
                            as i64
                    }
                    _ => int(0),
                };
                let tm = broken_down(t);
                let bytes: Vec<u8> = tm.iter().flat_map(|v| v.to_be_bytes()).collect();
                if name.ends_with("_R") {
                    match args.get(1) {
                        Some(Arg::Ref(l)) => {
                            let (b, o) = (l.buf, l.off);
                            if let Some(d) = self.bufs[b].get_mut(o..o + 36) {
                                d.copy_from_slice(&bytes);
                            }
                            Ok(self.ptr_to(b, o))
                        }
                        _ => {
                            let (b, o) = self.addr(&val(1), 36)?;
                            self.bufs[b][o..o + 36].copy_from_slice(&bytes);
                            Ok(val(1))
                        }
                    }
                } else {
                    let p = self.alloc(&Value::Num(Dec::from_i64(36)))?;
                    let (b, o) = self.addr(&p, 36)?;
                    self.bufs[b][o..o + 36].copy_from_slice(&bytes);
                    Ok(p)
                }
            }
            // strftime(dest, max, format, tm): %Y %y %m %d %H %M %S %j %a
            // %A %b %B %p %e %I %%; the length, or 0 when it does not fit.
            "STRFTIME" => {
                let fmt = self.c_string(&val(2))?;
                let tm: Vec<i32> = {
                    let raw: Vec<u8> = match args.get(3) {
                        Some(Arg::Ref(l)) => self.bufs[l.buf]
                            .get(l.off..l.off + 36)
                            .map(|x| x.to_vec())
                            .unwrap_or_default(),
                        Some(Arg::Val(Value::Char(c))) => c.clone(),
                        _ => {
                            let (b, o) = self.addr(&val(3), 36)?;
                            self.bufs[b][o..o + 36].to_vec()
                        }
                    };
                    raw.chunks(4)
                        .take(9)
                        .map(|c| i32::from_be_bytes(c.try_into().unwrap_or([0; 4])))
                        .collect()
                };
                let text = strftime(&fmt, &tm);
                let bytes = data::e(&text);
                let max = int(1).max(0) as usize;
                if bytes.len() + 1 > max {
                    return num(0);
                }
                let (b, o) = self.addr(&val(0), bytes.len() + 1)?;
                self.bufs[b][o..o + bytes.len()].copy_from_slice(&bytes);
                self.bufs[b][o + bytes.len()] = 0;
                num(bytes.len() as i64)
            }
            // mitime(time, hours, minutes, seconds, hundredths): an MI
            // time interval in the 8-byte system clock format (bit 51 is
            // one microsecond: microseconds × 4096); waittime(time,
            // option): wait that long (at most a minute here).
            "MITIME" => {
                let us = ((int(1) * 3600 + int(2) * 60 + int(3)) * 1_000_000 + int(4) * 10_000)
                    .max(0) as u64;
                let bytes = (us << 12).to_be_bytes();
                if let Some(Arg::Ref(l)) = args.first() {
                    let (b, o) = (l.buf, l.off);
                    if let Some(dst) = self.bufs[b].get_mut(o..o + 8) {
                        dst.copy_from_slice(&bytes);
                    }
                    return Ok(self.ptr_to(b, o));
                }
                // The variable's address (a CONST-declared one included).
                if let Some(Arg::Val(Value::Ptr(_))) = args.first() {
                    let (b, o) = self.addr(&val(0), 8)?;
                    self.bufs[b][o..o + 8].copy_from_slice(&bytes);
                    return Ok(val(0));
                }
                Ok(Value::Fig(Fig::Null))
            }
            "WAITTIME" => {
                let us = match args.first() {
                    Some(Arg::Ref(l)) => self.bufs[l.buf]
                        .get(l.off..l.off + 8)
                        .map(|x| u64::from_be_bytes(x.try_into().unwrap_or([0; 8])) >> 12)
                        .unwrap_or(0),
                    Some(Arg::Val(Value::Char(c))) if c.len() >= 8 => {
                        u64::from_be_bytes(c[..8].try_into().unwrap_or([0; 8])) >> 12
                    }
                    _ => 0,
                };
                std::thread::sleep(std::time::Duration::from_micros(us.min(60_000_000)));
                Ok(Value::Fig(Fig::Null))
            }
            // chdir(path): the job's current directory (an existing
            // directory: ENOENT, ENOTDIR). getcwd(buf, size): the path
            // into buf, or NULL with ERANGE when it does not fit.
            "CHDIR" => {
                let path = self.c_string(&val(0))?;
                let r = self
                    .block(ifs::stat(self.io.client, &path))
                    .and_then(|e| match e {
                        Some(e) if e.dir => {
                            ifs::set_current_dir(&e.path);
                            Ok(0)
                        }
                        Some(_) => Err(ifs::Errno(errno::ENOTDIR).into()),
                        None => Err(ifs::Errno(errno::ENOENT).into()),
                    });
                self.c_result(r)
            }
            "GETCWD" => {
                let mut bytes = data::e(&ifs::current_dir());
                bytes.push(0);
                let dst = val(0);
                if matches!(dst, Value::Fig(Fig::Null)) {
                    self.set_errno(errno::EINVAL);
                    return Ok(Value::Fig(Fig::Null));
                }
                if (int(1).max(0) as usize) < bytes.len() {
                    self.set_errno(errno::ERANGE);
                    return Ok(Value::Fig(Fig::Null));
                }
                let (b, o) = self.addr(&dst, bytes.len())?;
                self.bufs[b][o..o + bytes.len()].copy_from_slice(&bytes);
                Ok(dst)
            }
            "RENAME" => {
                let old = self.c_string(&val(0))?;
                let new = self.c_string(&val(1))?;
                let r = self.block(ifs::rename(self.io.client, &old, &new));
                self.c_result(r.map(|_| 0))
            }
            "__ERRNO" => Ok(self.errno_ptr()),
            // getenv(), putenv(), Qp0zGetEnv(), Qp0zPutEnv() (IBM i
            // Environment variable APIs): the job's variables; getenv
            // returns NULL for one that is not set; putenv takes
            // "name=value" and returns 0, or -1 with EINVAL.
            "GETENV" | "QP0ZGETENV" => {
                let name = a(&self.str_at(&val(0), None)?.chars()?);
                let job = self.job();
                match self.block(crate::envvar::get(self.io.client, &job, &name))? {
                    Some(crate::envvar::Var { value: Some(v), .. }) => Ok(self.c_static_string(&v)),
                    Some(_) => Ok(self.c_static_string("")),
                    None => Ok(Value::Fig(Fig::Null)),
                }
            }
            "PUTENV" | "QP0ZPUTENV" => {
                let s = a(&self.str_at(&val(0), None)?.chars()?);
                let job = self.job();
                let r = match s.split_once('=') {
                    Some((n, v)) if crate::envvar::check_name(n).is_ok() => {
                        self.block(crate::envvar::add(
                            self.io.client,
                            &job,
                            crate::envvar::Level::Job,
                            n,
                            crate::envvar::Var {
                                value: Some(v.to_string()),
                                ccsid: 0,
                            },
                            true,
                        ))
                    }
                    _ => Err(ifs::Errno(errno::EINVAL).into()),
                };
                self.c_result(r.map(|_| 0))
            }
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
            // pow(x, y): x to the power y, a double (C library).
            "POW" => {
                let x = super::float_value(&val(0))?;
                let y = super::float_value(&val(1))?;
                Ok(Value::Float(x.powf(y)))
            }
            // sleep(seconds), usleep(microseconds): the job waits; 0
            // (C library: both return 0 when the time has passed).
            "SLEEP" | "USLEEP" => {
                let n = val(0).num()?.to_i128()?.max(0) as u64;
                let d = if name == "SLEEP" {
                    std::time::Duration::from_secs(n)
                } else {
                    std::time::Duration::from_micros(n)
                };
                std::thread::sleep(d);
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
                // A failure's exception message ID goes to the runtime's
                // exported data item _EXCP_MSGID (ILE C, system()).
                match self.run_cl(cmd.trim()) {
                    Ok(()) => num(0),
                    Err(e) => {
                        let id = crate::clpgm::error_id(&e);
                        let mut b = data::e(&id);
                        b.resize(7, 0x40);
                        if let Ok(mut sh) = self.io.shared.lock() {
                            sh.insert("_EXCP_MSGID".to_string(), b);
                        }
                        self.share_in();
                        num(1)
                    }
                }
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
            // printf and puts: the job's standard output, which PASE
            // `system` writes out (to be checked against a real system);
            // without a job (`as4tainr call`), the program's output.
            "PRINTF" | "PUTS" => {
                let text = if name == "PUTS" {
                    let s = a(&self.str_at(&val(0), None)?.chars()?);
                    format!("{s}\n")
                } else {
                    let mut vals = Vec::new();
                    for i in 0..args.len() {
                        let v = val(i);
                        vals.push(match v {
                            Value::Ptr(_) => self.str_at(&v, None)?,
                            other => other,
                        });
                    }
                    self.printf(&vals)?
                };
                if self.io.job.is_some() || !self.io.job_key.is_empty() {
                    crate::pgmmsg::write_stdout(&self.io.job_key, &text);
                } else {
                    self.messages.push(text.trim_end_matches('\n').to_string());
                }
                num(if name == "PUTS" { 1 } else { text.len() as i64 })
            }
            // strtok(string, delimiters): the next token, the string
            // changed (a null character after the token); NULL for the
            // string goes on from the last token. The first call copies
            // the string to storage of its own, so the tokens stay valid
            // when the argument was a temporary OPTIONS(*STRING) copy.
            "STRTOK" => {
                let delims = self.str_at(&val(1), None)?.chars()?;
                let job = self.job();
                let start = match val(0) {
                    Value::Fig(Fig::Null) => {
                        let g = STRTOK_AT.lock().unwrap_or_else(|e| e.into_inner());
                        match g.as_ref().and_then(|m| m.get(&job).copied().flatten()) {
                            Some(p) => Value::Ptr(p),
                            None => return Ok(Value::Fig(Fig::Null)),
                        }
                    }
                    p => {
                        let text = self.str_at(&p, None)?.chars()?;
                        let copy = self.alloc(&Value::Num(Dec::from_i64(text.len() as i64 + 1)))?;
                        let (b, o) = self.addr(&copy, text.len() + 1)?;
                        self.bufs[b][o..o + text.len()].copy_from_slice(&text);
                        self.bufs[b][o + text.len()] = 0;
                        copy
                    }
                };
                let text = self.str_at(&start, None)?.chars()?;
                let Value::Ptr(base) = start else {
                    return Ok(Value::Fig(Fig::Null));
                };
                let set = |p: Option<super::super::mem::Ptr>| {
                    let mut g = STRTOK_AT.lock().unwrap_or_else(|e| e.into_inner());
                    g.get_or_insert_with(HashMap::new).insert(job.clone(), p);
                };
                let Some(k) = text.iter().position(|c| !delims.contains(c)) else {
                    set(None);
                    return Ok(Value::Fig(Fig::Null));
                };
                let end = text[k..]
                    .iter()
                    .position(|c| delims.contains(c))
                    .map(|j| k + j);
                match end {
                    Some(j) => {
                        let (b, o) = self.addr(&start, j + 1)?;
                        self.bufs[b][o + j] = 0;
                        set(base.add(j as i64 + 1));
                    }
                    None => set(None),
                }
                Ok(Value::Ptr(base.add(k as i64).unwrap_or(base)))
            }
            // strstr(string, substring): where it is first found, or NULL.
            "STRSTR" => {
                let h = self.str_at(&val(0), None)?.chars()?;
                let n = self.str_at(&val(1), None)?.chars()?;
                let at = if n.is_empty() {
                    Some(0)
                } else {
                    h.windows(n.len()).position(|w| w == n.as_slice())
                };
                Ok(match (at, val(0)) {
                    (Some(i), Value::Ptr(p)) => p
                        .add(i as i64)
                        .map(Value::Ptr)
                        .unwrap_or(Value::Fig(Fig::Null)),
                    _ => Value::Fig(Fig::Null),
                })
            }
            // strchr and strrchr(string, character): the first or last
            // place of the character (the null character counts).
            "STRCHR" | "STRRCHR" => {
                let h = self.str_at(&val(0), None)?.chars()?;
                let c = int(1) as u8;
                let at = if c == 0 {
                    Some(h.len())
                } else if name == "STRCHR" {
                    h.iter().position(|&x| x == c)
                } else {
                    h.iter().rposition(|&x| x == c)
                };
                Ok(match (at, val(0)) {
                    (Some(i), Value::Ptr(p)) => p
                        .add(i as i64)
                        .map(Value::Ptr)
                        .unwrap_or(Value::Fig(Fig::Null)),
                    _ => Value::Fig(Fig::Null),
                })
            }
            // strspn and strcspn: how many characters at the start are
            // (are not) in the set.
            "STRSPN" | "STRCSPN" => {
                let h = self.str_at(&val(0), None)?.chars()?;
                let set = self.str_at(&val(1), None)?.chars()?;
                let inside = name == "STRSPN";
                num(h.iter().take_while(|c| set.contains(c) == inside).count() as i64)
            }
            // Comparisons that ignore case.
            "STRCASECMP" | "STRNCASECMP" | "STRCMPI" | "STRICMP" => {
                let mut x = a(&self.str_at(&val(0), None)?.chars()?).to_lowercase();
                let mut y = a(&self.str_at(&val(1), None)?.chars()?).to_lowercase();
                if name == "STRNCASECMP" {
                    let n = int(2).max(0) as usize;
                    x = x.chars().take(n).collect();
                    y = y.chars().take(n).collect();
                }
                num(match x.cmp(&y) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                })
            }
            // toupper and tolower of a character (EBCDIC).
            "TOUPPER" | "TOLOWER" => {
                let c = int(0);
                let ch = a(&[c as u8]);
                let t = if name == "TOUPPER" {
                    ch.to_uppercase()
                } else {
                    ch.to_lowercase()
                };
                let b = super::super::data::e(&t);
                num(if b.len() == 1 && (0..=255).contains(&c) {
                    b[0] as i64
                } else {
                    c
                })
            }
            "ABS" => num(int(0).abs()),
            // rand(): 0 to 32767 (RAND_MAX), the series srand(seed) starts
            // (the ANSI C example generator).
            "RAND" | "SRAND" => {
                let job = self.job();
                let mut g = RAND_STATE.lock().unwrap_or_else(|e| e.into_inner());
                let m = g.get_or_insert_with(HashMap::new);
                if name == "SRAND" {
                    m.insert(job, int(0) as u32);
                    return num(0);
                }
                let st = m.entry(job).or_insert(1);
                *st = st.wrapping_mul(1_103_515_245).wrapping_add(12345);
                num(((*st / 65536) % 32768) as i64)
            }
            // tmpnam(buffer): a name no file has (QTEMP/QACXnnnnnn for
            // database files, /tmp/QACXnnnnnn in the IFS), in the buffer
            // or in storage of its own.
            "TMPNAM" | "_C_IFS_TMPNAM" => {
                let n = TMPNAM_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_micros())
                    .unwrap_or(0);
                let id = format!(
                    "{:06}",
                    (n.wrapping_mul(7919).wrapping_add(stamp)) % 1_000_000
                );
                let text = if name == "TMPNAM" {
                    format!("QTEMP/QACX{id}")
                } else {
                    format!("/tmp/QACX{id}")
                };
                let bytes = super::super::data::e(&text);
                let dst = match (args.first(), val(0)) {
                    (Some(Arg::Ref(l)), _) => {
                        let (b, o) = (l.buf, l.off);
                        self.ptr_to(b, o)
                    }
                    (_, Value::Ptr(p)) => Value::Ptr(p),
                    _ => self.alloc(&Value::Num(Dec::from_i64(40)))?,
                };
                let (b, o) = self.addr(&dst, bytes.len() + 1)?;
                self.bufs[b][o..o + bytes.len()].copy_from_slice(&bytes);
                self.bufs[b][o + bytes.len()] = 0;
                Ok(dst)
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
        // What this job wrote through other descriptors is in the file
        // (IBM i: writes are seen by other opens at once).
        self.c_flush();
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

/// struct tm of a time in seconds since 1970, UTC.
fn broken_down(t: i64) -> [i32; 9] {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let before = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][(m - 1) as usize]
        + i64::from(leap && m > 2);
    [
        (secs % 60) as i32,
        (secs / 60 % 60) as i32,
        (secs / 3600) as i32,
        d as i32,
        (m - 1) as i32,
        (y - 1900) as i32,
        (days + 4).rem_euclid(7) as i32,
        (before + d - 1) as i32,
        0,
    ]
}

/// strftime's conversions on a struct tm.
fn strftime(fmt: &str, tm: &[i32]) -> String {
    const MON: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const DAY: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let g = |i: usize| tm.get(i).copied().unwrap_or(0);
    let mut out = String::new();
    let mut cs = fmt.chars();
    while let Some(c) = cs.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match cs.next() {
            Some('Y') => out.push_str(&format!("{:04}", g(5) + 1900)),
            Some('y') => out.push_str(&format!("{:02}", (g(5) + 1900) % 100)),
            Some('m') => out.push_str(&format!("{:02}", g(4) + 1)),
            Some('d') => out.push_str(&format!("{:02}", g(3))),
            Some('e') => out.push_str(&format!("{:2}", g(3))),
            Some('H') => out.push_str(&format!("{:02}", g(2))),
            Some('I') => out.push_str(&format!(
                "{:02}",
                if g(2) % 12 == 0 { 12 } else { g(2) % 12 }
            )),
            Some('M') => out.push_str(&format!("{:02}", g(1))),
            Some('S') => out.push_str(&format!("{:02}", g(0))),
            Some('j') => out.push_str(&format!("{:03}", g(7) + 1)),
            Some('p') => out.push_str(if g(2) < 12 { "AM" } else { "PM" }),
            Some('a') => out.push_str(&DAY[g(6).rem_euclid(7) as usize][..3]),
            Some('A') => out.push_str(DAY[g(6).rem_euclid(7) as usize]),
            Some('b') | Some('h') => out.push_str(&MON[g(4).rem_euclid(12) as usize][..3]),
            Some('B') => out.push_str(MON[g(4).rem_euclid(12) as usize]),
            Some('%') => out.push('%'),
            Some(x) => {
                out.push('%');
                out.push(x);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod time_tests {
    // The C runtime's struct tm (ISO C): 2026-10-24 12:34:56 UTC is a
    // Saturday, day 296 of the year (0-based 296: Jan 1 is 0).
    #[test]
    fn broken_down_time() {
        let tm = super::broken_down(1_792_845_296);
        assert_eq!(tm[..8], [56, 34, 12, 24, 9, 126, 6, 296]);
        assert_eq!(
            super::strftime("%Y-%m-%d%H.%M.%S %a", &tm),
            "2026-10-2412.34.56 Sat"
        );
    }
}
