//! What ILE programs call of PASE (IBM i Information Center, "PASE for i
//! APIs": Qp2RunPase, Qp2dlopen, Qp2dlsym, Qp2malloc, Qp2CallPase...) and
//! iconv (QtqIconvOpen, iconv, iconv_close). as4tainr has no PASE: these
//! give an ILE program what it needs to call IBM i programs through
//! PASE's libc (XMLSERVICE calls programs with _RSLOBJ2 and _PGMCALL
//! through Qp2CallPase), with storage from Qp2malloc addressed both ways:
//! a PASE address (32 bits) and an ILE pointer to the same heap block.

use super::super::data::e;
use super::super::mem::{Ptr, HEAP_BASE, KIND_HEAP, KIND_SYS};
use super::*;

/// The procedures here (bound like the C runtime's, `crt::C_FUNCTIONS`).
pub const PASE_FUNCTIONS: &[&str] = &[
    "QP2PTRSIZE",
    "QP2JOBCCSID",
    "QP2PASECCSID",
    "QP2RUNPASE",
    "QP2DLOPEN",
    "QP2DLSYM",
    "QP2MALLOC",
    "QP2FREE",
    "QP2ERRNOP",
    "QP2SIGNALPASE",
    "QP2ENDPASE",
    "QP2CALLPASE",
    "QTQICONVOPEN",
    "ICONV",
    "ICONV_CLOSE",
    "QXXDTOP",
    "QXXDTOZ",
    "QXXPTOD",
    "QXXZTOD",
    "VSPRINTF",
    "_CALLPGMV",
];

/// Where PASE addresses of Qp2malloc storage start (a 32-bit PASE
/// program's heap is in its low 4 GB).
const PASE_BASE: u64 = 0x3000_0000;

/// Qp2malloc storage per job: (PASE address, length, heap block).
type Allocs = Vec<(u64, usize, usize)>;
static ALLOCS: std::sync::Mutex<Option<HashMap<String, Allocs>>> = std::sync::Mutex::new(None);

/// Qp2CallPase's signature codes (Qp2CallPase: arg types and results).
const ARG_WORD: i16 = -1;
const ARG_DWORD: i16 = -2;
const ARG_PTR32: i16 = -5;
const ARG_PTR64: i16 = -6;
const RESULT_WORD: i16 = -1;
const RESULT_DWORD: i16 = -2;
const RESULT_PTR32: i16 = -5;
const RESULT_PTR64: i16 = -6;

/// The output of commands popen ran, by job and stream: the bytes (in
/// PASE's CCSID, 819), how far fgets has read, the exit status.
type Popened = HashMap<u64, (Vec<u8>, usize, i32)>;
static POPENED: std::sync::Mutex<Option<HashMap<String, Popened>>> = std::sync::Mutex::new(None);
/// Where popen's stream handles start (not storage: fgets and pclose
/// take them back).
const POPEN_BASE: u64 = 0x7000_0000;
/// Qp2CallPase's return codes: normal; no PASE program (a function
/// as4tainr's PASE does not have).
const CALLPASE_NORMAL: i64 = 0;
const CALLPASE_ENVIRON_ERROR: i64 = 2;
/// _PGMCALL flags: the arguments themselves, not pointers.
const PGMCALL_DIRECTARGS: u64 = 1;
/// _ILELOAD: what identifies the service program (a path, "LIB/NAME",
/// a system pointer).
const ILELOAD_LIBOBJ: u64 = 1;
const ILELOAD_PGMPTR: u64 = 2;
/// _ILECALL's argument types (a positive code: an aggregate of that many
/// bytes) and result types.
const ARG_END: i16 = 0;
const ARG_INT8: i16 = -1;
const ARG_UINT8: i16 = -2;
const ARG_INT16: i16 = -3;
const ARG_UINT16: i16 = -4;
const ARG_INT32: i16 = -5;
const ARG_UINT32: i16 = -6;
const ARG_INT64: i16 = -7;
const ARG_UINT64: i16 = -8;
const ARG_FLOAT32: i16 = -9;
const ARG_FLOAT64: i16 = -10;
const ARG_MEMPTR: i16 = -11;
const ARG_SPCPTR: i16 = -12;
const ARG_OPENPTR: i16 = -13;
const ARG_MEMTS64: i16 = -14;
const RESULT_INT8: i16 = -1;
const RESULT_UINT8: i16 = -2;
const RESULT_INT16: i16 = -3;
const RESULT_UINT16: i16 = -4;
const RESULT_INT32: i16 = -5;
const RESULT_UINT32: i16 = -6;
const RESULT_FLOAT64: i16 = -10;

/// Text in a CCSID as4tainr converts: 37 (and 0, the job's), 819
/// (ISO 8859-1, PASE's default), 1208 (UTF-8), 367 (ASCII).
fn decode(ccsid: i32, b: &[u8]) -> Option<String> {
    Some(match ccsid {
        0 | 37 | 65535 => a(b),
        819 | 367 | 1252 => b.iter().map(|&c| c as char).collect(),
        1208 => String::from_utf8_lossy(b).into_owned(),
        _ => return None,
    })
}

fn encode(ccsid: i32, s: &str) -> Option<Vec<u8>> {
    Some(match ccsid {
        0 | 37 | 65535 => e(s),
        819 | 367 | 1252 => s
            .chars()
            .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
            .collect(),
        1208 => s.as_bytes().to_vec(),
        _ => return None,
    })
}

impl<'a> Machine<'a> {
    pub(super) fn pase_call(&mut self, name: &str, args: &[Arg]) -> Option<Result<Value>> {
        if !PASE_FUNCTIONS.contains(&name) {
            return None;
        }
        Some(self.pase_call_inner(name, args))
    }

    fn pase_call_inner(&mut self, name: &str, args: &[Arg]) -> Result<Value> {
        // An argument passed by reference is its address (C's pointer
        // parameters: a prototype may pass `char **` as a pointer by
        // reference or as a pointer to one by value).
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
                _ => 0,
            }
        };
        let num = |n: i64| Ok(Value::Num(Dec::from_i64(n)));
        match name {
            // A 32-bit PASE: 4-byte pointers.
            "QP2PTRSIZE" => num(4),
            "QP2JOBCCSID" => num(37),
            "QP2PASECCSID" => num(819),
            // The PASE program "runs" and returns, PASE staying alive.
            "QP2RUNPASE" | "QP2SIGNALPASE" | "QP2ENDPASE" | "QP2FREE" => num(0),
            // libc.a: one handle.
            "QP2DLOPEN" => num(1),
            // A function: its name in a heap block, what Qp2CallPase reads.
            "QP2DLSYM" => {
                let sym = a(&self.str_at(&val(1), None)?.chars()?).trim().to_string();
                let h = self.bufs.heap.alloc(64);
                let b = e(&sym);
                let n = b.len().min(63);
                self.bufs.heap.blocks[h][..n].copy_from_slice(&b[..n]);
                Ok(self.ptr_to(HEAP_BASE + h, 0))
            }
            "QP2MALLOC" => {
                let size = int(0).max(1) as usize;
                let h = self.bufs.heap.alloc(size);
                let base = {
                    let mut g = ALLOCS.lock().unwrap_or_else(|e| e.into_inner());
                    let list = g
                        .get_or_insert_with(HashMap::new)
                        .entry(self.io.job_key.clone())
                        .or_default();
                    let next = list
                        .iter()
                        .map(|(b, l, _)| b + *l as u64)
                        .max()
                        .unwrap_or(PASE_BASE);
                    // 64 KB aligned, as PASE's heap hands out large blocks.
                    let base = (next + 0xFFFF) & !0xFFFF;
                    list.push((base, size, h));
                    base
                };
                if !matches!(val(1), Value::Fig(Fig::Null)) {
                    let (b, o) = self.addr(&val(1), 8)?;
                    self.bufs[b][o..o + 8].copy_from_slice(&base.to_be_bytes());
                }
                Ok(self.ptr_to(HEAP_BASE + h, 0))
            }
            "QP2ERRNOP" => {
                let h = self.bufs.heap.alloc(4);
                Ok(self.ptr_to(HEAP_BASE + h, 0))
            }
            "QP2CALLPASE" => {
                let r = self.callpase(&val(0), &val(1), &val(2), int(3) as i16, &val(4))?;
                num(r)
            }
            // QtqIconvOpen(to, from): the converter's two CCSIDs in an
            // iconv_t (return value 0, then cd: from, to).
            "QTQICONVOPEN" => {
                let ccsid = |m: &Self, i: usize| -> Result<i32> {
                    let b = m.arg_bytes(args.get(i), 4)?;
                    Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                };
                let (to, from) = (ccsid(self, 0)?, ccsid(self, 1)?);
                let mut out = vec![0u8; 52];
                let ok = decode(from, b"").is_some() && encode(to, "").is_some();
                out[..4].copy_from_slice(&(if ok { 0i32 } else { -1 }).to_be_bytes());
                out[4..8].copy_from_slice(&from.to_be_bytes());
                out[8..12].copy_from_slice(&to.to_be_bytes());
                Ok(Value::Char(out))
            }
            "ICONV_CLOSE" => num(0),
            // QXXDTOP/QXXDTOZ(target, digits, fraction, value): the value
            // (a float, an exact decimal here) packed or zoned at the
            // target, rounded to the fraction digits; QXXPTOD/QXXZTOD read
            // one back (ILE C/C++ Runtime Library, the QXX functions).
            "QXXDTOP" | "QXXDTOZ" | "QXXPTOD" | "QXXZTOD" => {
                let (digits, frac) = (int(1).clamp(1, 63) as u32, int(2).clamp(0, 63) as u32);
                let t = match name {
                    "QXXDTOP" | "QXXPTOD" => DType::Packed(digits, frac),
                    _ => DType::Zoned(digits, frac),
                };
                let (b, o) = self.addr(&val(0), t.size())?;
                if name == "QXXDTOP" || name == "QXXDTOZ" {
                    let v = Value::Num(val(3).num()?);
                    let out = &mut self.bufs[b][o..o + t.size()];
                    data::store(&t, &v, out, Fit::Strict { half_adjust: true }, false)?;
                    num(0)
                } else {
                    data::load(&t, &self.bufs[b][o..o + t.size()])
                }
            }
            // iconv(cd, &in, &inleft, &out, &outleft).
            "ICONV" => {
                let cd = self.arg_bytes(args.first(), 12)?;
                let from = i32::from_be_bytes([cd[4], cd[5], cd[6], cd[7]]);
                let to = i32::from_be_bytes([cd[8], cd[9], cd[10], cd[11]]);
                let (inp, inleft_at) = (self.load_ptr(&val(1))?, self.addr(&val(2), 4)?);
                let (outp, outleft_at) = (self.load_ptr(&val(3))?, self.addr(&val(4), 4)?);
                let read_u32 = |m: &Self, at: (usize, usize)| -> usize {
                    let b = &m.bufs[at.0][at.1..at.1 + 4];
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
                };
                let inleft = read_u32(self, inleft_at);
                let outleft = read_u32(self, outleft_at);
                let (ib, io) = self.addr(&Value::Ptr(inp), inleft)?;
                let source = self.bufs[ib][io..io + inleft].to_vec();
                let text = decode(from, &source)
                    .ok_or_else(|| anyhow!("iconv: CCSID {from} is not one as4tainr converts"))?;
                let bytes = encode(to, &text)
                    .ok_or_else(|| anyhow!("iconv: CCSID {to} is not one as4tainr converts"))?;
                let n = bytes.len().min(outleft);
                let (ob, oo) = self.addr(&Value::Ptr(outp), n)?;
                self.bufs[ob][oo..oo + n].copy_from_slice(&bytes[..n]);
                // Pointers and counts move on past what was converted.
                let mut inp2 = inp;
                inp2.off += inleft as u32;
                let mut outp2 = outp;
                outp2.off += n as u32;
                self.store_ptr(&val(1), inp2)?;
                self.store_ptr(&val(3), outp2)?;
                self.bufs[inleft_at.0][inleft_at.1..inleft_at.1 + 4]
                    .copy_from_slice(&0u32.to_be_bytes());
                self.bufs[outleft_at.0][outleft_at.1..outleft_at.1 + 4]
                    .copy_from_slice(&((outleft - n) as u32).to_be_bytes());
                num(if n < bytes.len() { -1 } else { 0 })
            }
            // vsprintf(target, format, va_list): the va_list is two
            // pointers, the second where the arguments are (XMLSERVICE's
            // cpyOutFmt builds one so: the format, then the address of
            // its double). The text goes to the target in the job's CCSID
            // with a null after it; the result is its length.
            "VSPRINTF" => {
                let fmt = a(&self.str_at(&val(1), None)?.chars()?);
                let fmt = fmt.split('\0').next().unwrap_or("").to_string();
                let Value::Ptr(list) = val(2) else {
                    bail!("MCH3601: the va_list is not set")
                };
                let second = list.add(16).ok_or_else(|| anyhow!("MCH3601"))?;
                let mut at = self.load_ptr(&Value::Ptr(second))?;
                let mut out = String::new();
                for piece in c_format_pieces(&fmt) {
                    let (spec, conv) = match piece {
                        Piece::Text(t) => {
                            out.push_str(&t);
                            continue;
                        }
                        Piece::Conv(spec, conv) => (spec, conv),
                    };
                    // Arguments on their natural boundaries (a double 8
                    // bytes, a pointer 16, an int 4).
                    let size = match conv {
                        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => 8,
                        's' | 'p' => 16,
                        _ if spec.long_long => 8,
                        _ => 4,
                    };
                    let pad = (size - at.off as usize % size) % size;
                    at = at.add(pad as i64).ok_or_else(|| anyhow!("MCH3601"))?;
                    let (b, o) = self.addr(&Value::Ptr(at), size)?;
                    let raw = self.bufs[b][o..o + size].to_vec();
                    let arg = match conv {
                        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                            CArg::Float(f64::from_be_bytes(raw[..8].try_into()?))
                        }
                        's' => {
                            let p = Ptr::decode(&raw)
                                .ok_or_else(|| anyhow!("MCH3601: the %s pointer is not set"))?;
                            let t = self.str_at(&Value::Ptr(p), None)?.chars()?;
                            CArg::Text(a(&t))
                        }
                        _ if size == 8 => CArg::Int(i64::from_be_bytes(raw[..8].try_into()?)),
                        _ => CArg::Int(i32::from_be_bytes(raw[..4].try_into()?) as i64),
                    };
                    out.push_str(&c_format_one(&spec, conv, &arg));
                    at = at.add(size as i64).ok_or_else(|| anyhow!("MCH3601"))?;
                }
                let mut bytes = e(&out);
                bytes.push(0);
                let (b, o) = self.addr(&val(0), bytes.len())?;
                self.bufs[b][o..o + bytes.len()].copy_from_slice(&bytes);
                num(out.len() as i64)
            }
            // _CALLPGMV(&program, argv, argc) (MI built-in, Call Program
            // with Variable Length Argument List): the program a system
            // pointer addresses, with argc arguments, each one's storage
            // where its pointer in argv points.
            "_CALLPGMV" => {
                let p = self.load_ptr(&val(0))?;
                let (_, lib, pgm) = Some(p)
                    .filter(|p| p.kind == KIND_SYS)
                    .and_then(|p| super::super::mem::sys_ref(p.buf))
                    .ok_or_else(|| {
                        anyhow!("MCH3601: _CALLPGMV needs a system pointer to a program")
                    })?;
                let argc = int(2).max(0) as usize;
                let Value::Ptr(argv) = val(1) else {
                    bail!("MCH3601: _CALLPGMV: the argument list is not set");
                };
                let mut ptrs = Vec::new();
                for k in 0..argc {
                    let at = argv.add(16 * k as i64).ok_or_else(|| anyhow!("MCH3601"))?;
                    ptrs.push(self.load_ptr(&Value::Ptr(at))?);
                }
                let locs = self.arg_locs(&ptrs)?;
                self.call_program(&format!("{lib}/{pgm}"), &locs)?;
                Ok(Value::Fig(Fig::Null))
            }
            _ => bail!("{name} is not available in as4tainr yet"),
        }
    }

    /// The storage of arguments passed as addresses only: each one up to
    /// the next argument in the same storage, or its end.
    fn arg_locs(&self, ptrs: &[Ptr]) -> Result<Vec<Loc>> {
        let mut places = Vec::new();
        for p in ptrs {
            let (b, o) = self.addr(&Value::Ptr(*p), 0)?;
            places.push((b, o));
        }
        let mut locs = Vec::new();
        for (b, o) in &places {
            let end = self.bufs[*b].len();
            let next = places
                .iter()
                .filter(|(b2, o2)| b2 == b && o2 > o)
                .map(|(_, o2)| *o2)
                .min()
                .unwrap_or(end);
            let size = next.min(end) - o;
            locs.push(Loc {
                buf: *b,
                off: *o,
                size,
                dtype: DType::Char(size as u32),
            });
        }
        Ok(locs)
    }

    /// The bytes an argument passed by reference (or by value) holds.
    fn arg_bytes(&self, a: Option<&Arg>, min: usize) -> Result<Vec<u8>> {
        let mut b = match a {
            Some(Arg::Ref(l)) => self.bufs[l.buf][l.off..l.off + l.size].to_vec(),
            Some(Arg::Val(Value::Char(c))) => c.clone(),
            Some(Arg::Val(v @ Value::Ptr(_))) => {
                let (b, o) = self.addr(v, min)?;
                self.bufs[b][o..o + min].to_vec()
            }
            _ => vec![],
        };
        b.resize(b.len().max(min), 0);
        Ok(b)
    }

    /// The pointer stored where `at` points.
    fn load_ptr(&self, at: &Value) -> Result<Ptr> {
        let (b, o) = self.addr(at, 16)?;
        Ptr::decode(&self.bufs[b][o..o + 16])
            .ok_or_else(|| anyhow!("MCH3601: the pointer there is not set"))
    }

    fn store_ptr(&mut self, at: &Value, p: Ptr) -> Result<()> {
        let (b, o) = self.addr(at, 16)?;
        self.bufs[b][o..o + 16].copy_from_slice(&p.encode());
        Ok(())
    }

    /// The heap storage a PASE address is in: (buffer, offset, bytes to
    /// the end of that storage).
    fn pase_addr(&self, addr: u64) -> Result<(usize, usize, usize)> {
        let g = ALLOCS.lock().unwrap_or_else(|e| e.into_inner());
        let list = g.as_ref().and_then(|m| m.get(&self.io.job_key));
        for (base, len, h) in list.into_iter().flatten() {
            if addr >= *base && addr < base + *len as u64 {
                let off = (addr - base) as usize;
                return Ok((HEAP_BASE + h, off, len - off));
            }
        }
        bail!("MCH3601: PASE address {addr:#x} is not storage Qp2malloc gave")
    }

    /// A null-terminated string at a PASE address, in PASE's CCSID (819).
    fn pase_string(&self, addr: u64) -> Result<String> {
        let (b, o, left) = self.pase_addr(addr)?;
        let s = &self.bufs[b][o..o + left];
        let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        Ok(s[..n].iter().map(|&c| c as char).collect())
    }

    /// Qp2CallPase(target, arguments, signature, result type, result): a
    /// libc function as4tainr's PASE has (_RSLOBJ2, _PGMCALL), its
    /// arguments laid out as the signature says.
    fn callpase(
        &mut self,
        target: &Value,
        arglist: &Value,
        sig: &Value,
        result: i16,
        buf: &Value,
    ) -> Result<i64> {
        let func = a(&self.str_at(target, Some(63))?.chars()?)
            .trim()
            .to_string();
        // The signature: 2-byte codes up to a zero.
        let mut codes = Vec::new();
        {
            let (b, o) = self.addr(sig, 2)?;
            let s = &self.bufs[b];
            let mut i = o;
            while i + 2 <= s.len() {
                let c = i16::from_be_bytes([s[i], s[i + 1]]);
                if c == 0 {
                    break;
                }
                codes.push(c);
                i += 2;
            }
        }
        // The arguments: a word or a 32-bit address 4 bytes, a double word
        // or a 64-bit address 8 bytes (on an 8-byte boundary).
        let mut argv = Vec::new();
        {
            let (b, o) = self.addr(arglist, 0)?;
            let s = &self.bufs[b];
            let mut i = o;
            for c in &codes {
                let n = match *c {
                    ARG_WORD | ARG_PTR32 => 4,
                    ARG_DWORD | ARG_PTR64 => {
                        i = o + (i - o).div_ceil(8) * 8;
                        8
                    }
                    n if n > 0 => n as usize,
                    _ => bail!("Qp2CallPase: signature code {c} is not valid"),
                };
                let bytes = s
                    .get(i..i + n)
                    .ok_or_else(|| anyhow!("Qp2CallPase: the arguments are cut short"))?;
                let v = match n {
                    4 => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
                    8 => u64::from_be_bytes(bytes.try_into()?),
                    _ => 0,
                };
                argv.push(v);
                i += n;
            }
        }
        let rc: i64 = match func.as_str() {
            // int _RSLOBJ2(ILEpointer *sysptr, unsigned short type,
            //              const char *name, const char *lib)
            "_RSLOBJ2" => {
                let (sp, ty) = (
                    argv.first().copied().unwrap_or(0),
                    argv.get(1).copied().unwrap_or(0) as u16,
                );
                let obj = self
                    .pase_string(argv.get(2).copied().unwrap_or(0))?
                    .trim()
                    .to_ascii_uppercase();
                let lib = self
                    .pase_string(argv.get(3).copied().unwrap_or(0))?
                    .trim()
                    .to_ascii_uppercase();
                let objtype = crate::sysapi::MI_TYPES
                    .iter()
                    .find(|(_, x)| *x == ty)
                    .map(|(n, _)| n.to_string());
                let libs = if lib.is_empty() || lib == "*LIBL" {
                    self.io.libl.clone()
                } else {
                    vec![crate::qtemp::real(&lib)]
                };
                let mut found = None;
                // A system program as4tainr does itself is in QSYS.
                if objtype.as_deref() == Some("*PGM")
                    && (lib.is_empty() || lib == "*LIBL" || lib == "QSYS")
                    && super::is_system_program(&obj)
                {
                    found = Some("QSYS".to_string());
                }
                if let Some(t) = objtype.as_ref().filter(|_| found.is_none()) {
                    for l in libs {
                        if self.block(crate::catalog::object_exists(
                            self.io.client,
                            std::slice::from_ref(&l),
                            &obj,
                            t,
                        ))? {
                            found = Some(l);
                            break;
                        }
                    }
                }
                match (found, objtype) {
                    (Some(l), Some(t)) => {
                        let p = Ptr {
                            kind: KIND_SYS,
                            owner: 0,
                            buf: super::super::mem::sys_id(&(t, l, obj)),
                            off: 0,
                        };
                        let (b, o, _) = self.pase_addr(sp)?;
                        self.bufs[b][o..o + 16].copy_from_slice(&p.encode());
                        0
                    }
                    _ => -1,
                }
            }
            // int _PGMCALL(const ILEpointer *target, void **argv,
            //              unsigned flags): the program called with the
            // arguments argv addresses (up to a null address).
            "_PGMCALL" => {
                let (b, o, _) = self.pase_addr(argv.first().copied().unwrap_or(0))?;
                let p = Ptr::decode(&self.bufs[b][o..o + 16])
                    .filter(|p| p.kind == KIND_SYS)
                    .ok_or_else(|| anyhow!("_PGMCALL: the target is not a system pointer"))?;
                let (_, lib, pgm) = super::super::mem::sys_ref(p.buf)
                    .ok_or_else(|| anyhow!("_PGMCALL: the target is not an object"))?;
                if argv.get(2).copied().unwrap_or(0) & PGMCALL_DIRECTARGS != 0 {
                    bail!("_PGMCALL with PGMCALL_DIRECTARGS is not available in as4tainr yet");
                }
                let (lb, lo, _) = self.pase_addr(argv.get(1).copied().unwrap_or(0))?;
                let mut addrs = Vec::new();
                let mut i = lo;
                loop {
                    let s = &self.bufs[lb];
                    let Some(w) = s.get(i..i + 4) else { break };
                    let a = u32::from_be_bytes([w[0], w[1], w[2], w[3]]) as u64;
                    if a == 0 {
                        break;
                    }
                    addrs.push(a);
                    i += 4;
                }
                // Each argument's storage: up to the next argument in the
                // same storage, or its end.
                let mut sorted = addrs.clone();
                sorted.sort_unstable();
                let mut locs = Vec::new();
                for a in &addrs {
                    let (buf, off, left) = self.pase_addr(*a)?;
                    let next = sorted.iter().find(|x| **x > *a).map(|x| (x - a) as usize);
                    let size = next.map(|n| n.min(left)).unwrap_or(left);
                    locs.push(Loc {
                        buf,
                        off,
                        size,
                        dtype: DType::Char(size as u32),
                    });
                }
                // A system API's packed decimal parameters get their type.
                if lib == "QSYS" {
                    for (k, digits) in crate::dtaq::packed_params(&pgm) {
                        if let Some(l) = locs.get_mut(*k) {
                            let t = DType::Packed(*digits, 0);
                            l.size = l.size.min(t.size());
                            l.dtype = t;
                        }
                    }
                }
                self.call_program(&format!("{lib}/{pgm}"), &locs)?;
                0
            }
            // int _ILELOAD(const void *id, unsigned flags): a service
            // program activated (named by a system pointer, "LIB/NAME" or
            // a path); its activation mark.
            "_ILELOAD" => {
                let id = argv.first().copied().unwrap_or(0);
                let flags = argv.get(1).copied().unwrap_or(0);
                let named = match flags & 3 {
                    ILELOAD_PGMPTR => {
                        let (b, o, _) = self.pase_addr(id)?;
                        Ptr::decode(&self.bufs[b][o..o + 16])
                            .filter(|p| p.kind == KIND_SYS)
                            .and_then(|p| super::super::mem::sys_ref(p.buf))
                            .map(|(_, l, n)| (l, n))
                    }
                    ILELOAD_LIBOBJ => {
                        let t = self.pase_string(id)?.trim().to_ascii_uppercase();
                        t.split_once('/')
                            .map(|(l, n)| (l.to_string(), n.to_string()))
                    }
                    _ => {
                        // /QSYS.LIB/LIB.LIB/NAME.SRVPGM
                        let t = self.pase_string(id)?.to_ascii_uppercase();
                        let parts: Vec<&str> = t.trim_matches('/').split('/').collect();
                        match parts.as_slice() {
                            ["QSYS.LIB", l, n] => Some((
                                l.trim_end_matches(".LIB").to_string(),
                                n.trim_end_matches(".SRVPGM").to_string(),
                            )),
                            _ => None,
                        }
                    }
                };
                match named {
                    Some((lib, name)) => {
                        match self.block(crate::ile::find_srvpgm(
                            self.io.client,
                            &self.io.libl,
                            &lib,
                            &name,
                        )) {
                            Ok((lib, _)) => {
                                // The marks QleActBndPgm gives.
                                let mut list = super::cee::ACTIVATED
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                let k = match list
                                    .iter()
                                    .position(|x| *x == (lib.clone(), name.clone()))
                                {
                                    Some(k) => k,
                                    None => {
                                        list.push((lib, name));
                                        list.len() - 1
                                    }
                                };
                                k as i64 + 1
                            }
                            Err(_) => -1,
                        }
                    }
                    None => -1,
                }
            }
            // int _ILESYM(ILEpointer *export, int actmark, const char
            // *symbol): a procedure pointer to the activation's export.
            "_ILESYM" => {
                let export = argv.first().copied().unwrap_or(0);
                let mark = argv.get(1).copied().unwrap_or(0) as u32 as i32;
                let symbol = self.pase_string(argv.get(2).copied().unwrap_or(0))?;
                let srv = super::cee::ACTIVATED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get((mark - 1).max(0) as usize)
                    .cloned()
                    .filter(|_| mark > 0);
                let found = match srv {
                    Some((lib, name)) => {
                        match self.block(crate::ile::find_srvpgm(
                            self.io.client,
                            &self.io.libl,
                            &lib,
                            &name,
                        )) {
                            Ok((_, b)) => b
                                .exports
                                .iter()
                                .find(|x| **x == symbol)
                                .or_else(|| {
                                    b.exports.iter().find(|x| x.eq_ignore_ascii_case(&symbol))
                                })
                                .map(|x| (x.clone(), lib, name)),
                            Err(_) => None,
                        }
                    }
                    None => None,
                };
                match found {
                    Some((sym, lib, name)) => {
                        let p = Ptr {
                            kind: super::super::mem::KIND_PROC,
                            owner: 0,
                            buf: super::super::mem::proc_id(&sym, Some((lib, name))),
                            off: 0,
                        };
                        let (b, o, _) = self.pase_addr(export)?;
                        self.bufs[b][o..o + 16].copy_from_slice(&p.encode());
                        // A procedure (the applications look at the sign).
                        1
                    }
                    None => -1,
                }
            }
            // int _ILECALL(const ILEpointer *target, ILEarglist_base
            // *arglist, const arg_type_t *signature, result_type_t
            // result_type): the procedure called with the arguments after
            // the list's 32-byte base, laid out as the signature says.
            "_ILECALL" => {
                self.ilecall(
                    argv.first().copied().unwrap_or(0),
                    argv.get(1).copied().unwrap_or(0),
                    argv.get(2).copied().unwrap_or(0),
                    argv.get(3).copied().unwrap_or(0) as u32 as i32,
                )?;
                0
            }
            // FILE *popen(const char *cmd, const char *mode): the command
            // run by the shell (as QSH CMD runs one: only where
            // AS4TAINR_QSH=1 lets commands run, NULL otherwise), its output
            // kept for fgets. PASE's /QOpenSys tree is the container's
            // root (/QOpenSys/usr/bin/ls is /usr/bin/ls).
            "popen" => {
                let cmd = self.pase_string(argv.first().copied().unwrap_or(0))?;
                if !crate::qsh::enabled() {
                    self.set_errno(crate::ifs::errno::EPERM);
                    0
                } else {
                    // (`system` at its IBM i paths: qsh.rs shell_run.)
                    let line = cmd.replace("/QOpenSys/", "/");
                    let out = self.shell_output(&line)?;
                    let mut g = POPENED.lock().unwrap_or_else(|e| e.into_inner());
                    let m = g
                        .get_or_insert_with(HashMap::new)
                        .entry(self.io.job_key.clone())
                        .or_default();
                    let h = POPEN_BASE + 16 * (m.len() as u64 + 1);
                    m.insert(h, out);
                    h as i64
                }
            }
            // char *fgets(char *s, int n, FILE *stream): the next line of
            // a popen stream (with its new line), up to n-1 bytes, then a
            // null; NULL at the end.
            "fgets" => {
                let s = argv.first().copied().unwrap_or(0);
                let n = argv.get(1).copied().unwrap_or(0) as u32 as i32 as i64;
                let h = argv.get(2).copied().unwrap_or(0);
                let chunk = {
                    let mut g = POPENED.lock().unwrap_or_else(|e| e.into_inner());
                    g.get_or_insert_with(HashMap::new)
                        .get_mut(&self.io.job_key)
                        .and_then(|m| m.get_mut(&h))
                        .map(|(data, pos, _)| {
                            let rest = &data[(*pos).min(data.len())..];
                            let max = (n.max(1) - 1) as usize;
                            let end = rest
                                .iter()
                                .position(|&c| c == b'\n')
                                .map(|i| i + 1)
                                .unwrap_or(rest.len())
                                .min(max);
                            let c = rest[..end].to_vec();
                            *pos += end;
                            c
                        })
                };
                match chunk {
                    Some(c) if !c.is_empty() => {
                        let (b, o, left) = self.pase_addr(s)?;
                        let k = c.len().min(left.saturating_sub(1));
                        self.bufs[b][o..o + k].copy_from_slice(&c[..k]);
                        self.bufs[b][o + k] = 0;
                        s as i64
                    }
                    _ => 0,
                }
            }
            // int pclose(FILE *stream): the command's exit status (as
            // wait() gives it: the code times 256).
            "pclose" => {
                let h = argv.first().copied().unwrap_or(0);
                let mut g = POPENED.lock().unwrap_or_else(|e| e.into_inner());
                match g
                    .get_or_insert_with(HashMap::new)
                    .get_mut(&self.io.job_key)
                    .and_then(|m| m.remove(&h))
                {
                    Some((_, _, status)) => (status as i64) << 8,
                    None => -1,
                }
            }
            _ => return Ok(CALLPASE_ENVIRON_ERROR),
        };
        if !matches!(buf, Value::Fig(Fig::Null)) {
            match result {
                RESULT_PTR32 => {
                    let (b, o) = self.addr(buf, 4)?;
                    self.bufs[b][o..o + 4].copy_from_slice(&(rc as u32).to_be_bytes());
                }
                RESULT_PTR64 => {
                    let (b, o) = self.addr(buf, 8)?;
                    self.bufs[b][o..o + 8].copy_from_slice(&(rc as u64).to_be_bytes());
                }
                RESULT_WORD => {
                    let (b, o) = self.addr(buf, 4)?;
                    self.bufs[b][o..o + 4].copy_from_slice(&(rc as i32).to_be_bytes());
                }
                RESULT_DWORD => {
                    let (b, o) = self.addr(buf, 8)?;
                    self.bufs[b][o..o + 8].copy_from_slice(&rc.to_be_bytes());
                }
                _ => {}
            }
        }
        let _ = KIND_HEAP;
        Ok(CALLPASE_NORMAL)
    }
}

impl Machine<'_> {
    /// A shell command's output (in PASE's CCSID, 819) and exit status,
    /// run as QSH runs one (`Job::shell_run`).
    fn shell_output(&mut self, line: &str) -> Result<(Vec<u8>, usize, i32)> {
        let ctx = self
            .io
            .cl
            .clone()
            .ok_or_else(|| anyhow!("popen: the program has no job to run '{line}' in"))?;
        let mut job = crate::commands::Job {
            client: self.io.client,
            env: &ctx.env,
            user: self.io.user.clone(),
            curlib: ctx.curlib.clone(),
            libl: ctx.user_libl.clone(),
            display: None,
            id: self.io.job.clone(),
            calls: Default::default(),
        };
        let out = self.block(job.shell_run(line))?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let bytes = crate::ifs::codec::convert(1208, 819, text.as_bytes());
        Ok((bytes, 0, out.status.code().unwrap_or(1)))
    }

    fn ilecall(&mut self, target: u64, base: u64, sig: u64, result: i32) -> Result<()> {
        let (b, o, _) = self.pase_addr(target)?;
        let p = Ptr::decode(&self.bufs[b][o..o + 16])
            .filter(|p| p.kind == super::super::mem::KIND_PROC)
            .ok_or_else(|| anyhow!("_ILECALL: the target is not a procedure pointer"))?;
        let (sym, home) = super::super::mem::proc_ref(p.buf)
            .ok_or_else(|| anyhow!("_ILECALL: the target is not a procedure"))?;
        let (library, name) =
            home.ok_or_else(|| anyhow!("_ILECALL: {sym} is not in a service program"))?;
        // The signature: codes up to a zero.
        let mut codes = Vec::new();
        {
            let (b, o, left) = self.pase_addr(sig)?;
            let s = &self.bufs[b][o..o + left];
            let mut i = 0;
            while i + 2 <= s.len() {
                let c = i16::from_be_bytes([s[i], s[i + 1]]);
                if c == ARG_END {
                    break;
                }
                codes.push(c);
                i += 2;
            }
        }
        // Each argument's bytes, on its boundary after the base.
        let (bb, bo, left) = self.pase_addr(base)?;
        let mut at = 32usize;
        let mut raw = Vec::new();
        for c in &codes {
            let (size, align) = match *c {
                ARG_INT8 | ARG_UINT8 => (1, 1),
                ARG_INT16 | ARG_UINT16 => (2, 2),
                ARG_INT32 | ARG_UINT32 | ARG_FLOAT32 | ARG_MEMPTR => (4, 4),
                ARG_INT64 | ARG_UINT64 | ARG_FLOAT64 | ARG_MEMTS64 => (8, 8),
                ARG_SPCPTR | ARG_OPENPTR => (16, 16),
                n if n > 0 => {
                    let n = n as usize;
                    let align = match n {
                        1 => 1,
                        2 => 2,
                        3 | 4 => 4,
                        5..=8 => 8,
                        _ => 16,
                    };
                    (n, align)
                }
                _ => bail!("_ILECALL: signature code {c} is not available in as4tainr"),
            };
            at = at.div_ceil(align) * align;
            if at + size > left {
                bail!("_ILECALL: the arguments are cut short");
            }
            raw.push((*c, self.bufs[bb][bo + at..bo + at + size].to_vec()));
            at += size;
        }
        // What an argument that is an address addresses.
        let mut targets = Vec::new();
        for (c, bytes) in &raw {
            targets.push(match *c {
                ARG_SPCPTR | ARG_OPENPTR => Ptr::decode(bytes),
                ARG_MEMPTR | ARG_MEMTS64 => {
                    let a = if *c == ARG_MEMPTR {
                        u32::from_be_bytes(bytes[..4].try_into()?) as u64
                    } else {
                        u64::from_be_bytes(bytes[..8].try_into()?)
                    };
                    if a == 0 {
                        None
                    } else {
                        let (b, o, _) = self.pase_addr(a)?;
                        match self.ptr_to(b, o) {
                            Value::Ptr(p) => Some(p),
                            _ => None,
                        }
                    }
                }
                _ => None,
            });
        }
        let target = crate::ile::Target::SrvPgm { library, name };
        let mut build = |m: &mut Self, pi: &ProcInfo| -> Result<Vec<Arg>> {
            let mut args = Vec::new();
            for (k, p) in pi.params.iter().enumerate() {
                let Some((c, bytes)) = raw.get(k) else { break };
                let address = matches!(*c, ARG_SPCPTR | ARG_OPENPTR | ARG_MEMPTR | ARG_MEMTS64);
                if address && !p.by_value {
                    let Some(ptr) = targets[k] else {
                        args.push(Arg::Omit);
                        continue;
                    };
                    let size = p.dtype.size();
                    let (b, o) = m.addr(&Value::Ptr(ptr), size)?;
                    if p.konst {
                        args.push(Arg::Val(data::load(&p.dtype, &m.bufs[b][o..o + size])?));
                    } else {
                        args.push(Arg::Ref(Loc {
                            buf: b,
                            off: o,
                            size,
                            dtype: p.dtype,
                        }));
                    }
                } else if address {
                    args.push(Arg::Val(match targets[k] {
                        Some(ptr) => Value::Ptr(ptr),
                        None => Value::Fig(Fig::Null),
                    }));
                } else {
                    // By value: the bytes as the parameter's type (an
                    // integer or a float in its size, a character or
                    // decimal value as its bytes).
                    let mut v = bytes.clone();
                    v.resize(p.dtype.size().max(v.len()), 0);
                    args.push(Arg::Val(data::load(&p.dtype, &v[..p.dtype.size()])?));
                }
            }
            Ok(args)
        };
        let (r, rtype) = self.call_bound_built(&sym, &target, &mut build)?;

        // The return value: a scalar in the base (right-aligned in its 8
        // bytes after the descriptor), an aggregate where the base's
        // address field (bytes 24-31) says.
        if result == 0 {
            return Ok(());
        }
        let (Some(v), Some(t)) = (r, rtype) else {
            return Ok(());
        };
        let mut out = t.initial();
        data::store(&t, &v, &mut out, Fit::Strict { half_adjust: false }, false)?;
        if result > 0 {
            // All of it: the result type's size stops at 32767 (an
            // arg_type_t), XMLSERVICE gets larger values so.
            let a = u64::from_be_bytes(self.bufs[bb][bo + 24..bo + 32].try_into()?);
            let (b, o, left) = self.pase_addr(a)?;
            let n = out.len().min(left);

            self.bufs[b][o..o + n].copy_from_slice(&out[..n]);
        } else {
            let n = match result as i16 {
                RESULT_INT8 | RESULT_UINT8 => 1,
                RESULT_INT16 | RESULT_UINT16 => 2,
                RESULT_INT32 | RESULT_UINT32 => 4,
                _ => 8,
            };
            let mut slot = [0u8; 8];
            let k = out.len().min(n);
            slot[8 - k..].copy_from_slice(&out[out.len() - k..]);
            if result as i16 == RESULT_FLOAT64 {
                slot = [0u8; 8];
                let x = match &v_float(&t, &out) {
                    Some(x) => *x,
                    None => 0.0,
                };
                slot.copy_from_slice(&x.to_be_bytes());
            }
            self.bufs[bb][bo + 16..bo + 24].copy_from_slice(&slot);
        }
        Ok(())
    }
}

/// A float return value's bytes as a double.
fn v_float(t: &DType, out: &[u8]) -> Option<f64> {
    match t {
        DType::Float(8) => Some(f64::from_be_bytes(out.try_into().ok()?)),
        DType::Float(4) => Some(f32::from_be_bytes(out.try_into().ok()?) as f64),
        _ => None,
    }
}

/// A printf format: text, and conversions with their flags, width and
/// precision (ILE C/C++ Runtime Library Functions, printf).
enum Piece {
    Text(String),
    Conv(Spec, char),
}

#[derive(Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alt: bool,
    width: usize,
    precision: Option<usize>,
    long_long: bool,
}

enum CArg {
    Int(i64),
    Float(f64),
    Text(String),
}

fn c_format_pieces(fmt: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut cs = fmt.chars().peekable();
    while let Some(c) = cs.next() {
        if c != '%' {
            text.push(c);
            continue;
        }
        if cs.peek() == Some(&'%') {
            cs.next();
            text.push('%');
            continue;
        }
        let mut spec = Spec::default();
        while let Some(&f) = cs.peek() {
            match f {
                '-' => spec.left = true,
                '+' => spec.plus = true,
                ' ' => spec.space = true,
                '0' => spec.zero = true,
                '#' => spec.alt = true,
                _ => break,
            }
            cs.next();
        }
        while let Some(d) = cs.peek().and_then(|c| c.to_digit(10)) {
            spec.width = spec.width * 10 + d as usize;
            cs.next();
        }
        if cs.peek() == Some(&'.') {
            cs.next();
            let mut p = 0;
            while let Some(d) = cs.peek().and_then(|c| c.to_digit(10)) {
                p = p * 10 + d as usize;
                cs.next();
            }
            spec.precision = Some(p);
        }
        let mut longs = 0;
        while let Some(&l) = cs.peek() {
            match l {
                'l' => longs += 1,
                'h' | 'L' => {}
                _ => break,
            }
            cs.next();
        }
        spec.long_long = longs >= 2;
        let Some(conv) = cs.next() else { break };
        if !text.is_empty() {
            out.push(Piece::Text(std::mem::take(&mut text)));
        }
        out.push(Piece::Conv(spec, conv));
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// One conversion. %f rounds the double's exact binary value to the
/// precision (6 when none), as C's printf does.
fn c_format_one(spec: &Spec, conv: char, arg: &CArg) -> String {
    let sign = |neg: bool| -> &str {
        if neg {
            "-"
        } else if spec.plus {
            "+"
        } else if spec.space {
            " "
        } else {
            ""
        }
    };
    let (prefix, body) = match (conv, arg) {
        ('d' | 'i', CArg::Int(n)) => (sign(*n < 0).to_string(), n.unsigned_abs().to_string()),
        ('u', CArg::Int(n)) => (String::new(), (*n as u64 & u32::MAX as u64).to_string()),
        ('x', CArg::Int(n)) => (String::new(), format!("{:x}", *n as u32)),
        ('X', CArg::Int(n)) => (String::new(), format!("{:X}", *n as u32)),
        ('c', CArg::Int(n)) => (String::new(), ((*n as u8) as char).to_string()),
        ('f' | 'F', CArg::Float(x)) => {
            let p = spec.precision.unwrap_or(6);
            let mut t = format!("{:.*}", p, x.abs());
            if spec.alt && p == 0 {
                t.push('.');
            }
            (sign(x.is_sign_negative() && *x != 0.0).to_string(), t)
        }
        ('e' | 'E' | 'g' | 'G', CArg::Float(x)) => {
            let p = spec.precision.unwrap_or(6);
            let t = format!("{:.*e}", p, x.abs());
            // C writes the exponent with a sign and two digits at least.
            let (m, ex) = t.split_once('e').unwrap_or((&t, "0"));
            let n: i32 = ex.parse().unwrap_or(0);
            let t = format!("{m}e{}{:02}", if n < 0 { '-' } else { '+' }, n.abs());
            let t = if conv.is_ascii_uppercase() {
                t.to_uppercase()
            } else {
                t
            };
            (sign(x.is_sign_negative() && *x != 0.0).to_string(), t)
        }
        ('s', CArg::Text(s)) => match spec.precision {
            Some(p) => (String::new(), s.chars().take(p).collect()),
            None => (String::new(), s.clone()),
        },
        _ => (String::new(), String::new()),
    };
    let len = prefix.chars().count() + body.chars().count();
    if len >= spec.width {
        return prefix + &body;
    }
    let fill = spec.width - len;
    if spec.left {
        format!("{prefix}{body}{}", " ".repeat(fill))
    } else if spec.zero && !matches!(conv, 's' | 'c') {
        format!("{prefix}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{prefix}{body}", " ".repeat(fill))
    }
}
