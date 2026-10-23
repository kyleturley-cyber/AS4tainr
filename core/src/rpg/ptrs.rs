//! Pointers in a running program (ILE RPG Reference, "Basing Pointer Data
//! Type", the BASED keyword, %ADDR, %ALLOC, %REALLOC, %STR, ALLOC,
//! DEALLOC): addresses of variables and heap storage, based variables
//! found through their pointers.

use super::super::mem::{
    Ptr, BAD_BUF, HEAP_BASE, KIND_HEAP, KIND_PROC, KIND_STORAGE, KIND_SYS, LENT_BASE,
};
use super::*;

/// The longest string %STR reads without a maximum (ILE RPG Reference:
/// %STR without a maximum reads up to the null terminator; as4tainr
/// stops at the end of the storage the pointer addresses).
const STR_LIMIT: usize = 16 * 1024 * 1024;

impl<'a> Machine<'a> {
    /// The pointer to a place in this program's storage (an absolute
    /// buffer, as `sym` gives).
    pub(super) fn ptr_to(&self, buf: usize, off: usize) -> Value {
        if buf == BAD_BUF {
            return Value::Fig(Fig::Null);
        }
        let p = if buf >= LENT_BASE {
            let k = (buf - LENT_BASE) >> 24;
            Ptr {
                kind: KIND_STORAGE,
                owner: self.bufs.lent[k].0,
                buf: ((buf - LENT_BASE) & 0xFF_FFFF) as u32,
                off: off as u32,
            }
        } else if buf >= HEAP_BASE {
            Ptr {
                kind: KIND_HEAP,
                owner: 0,
                buf: (buf - HEAP_BASE) as u32,
                off: off as u32,
            }
        } else {
            Ptr {
                kind: KIND_STORAGE,
                owner: self.owner,
                buf: buf as u32,
                off: off as u32,
            }
        };
        Value::Ptr(p)
    }

    /// Where a pointer points: (absolute buffer, offset), with `size`
    /// bytes there.
    pub(super) fn addr(&self, v: &Value, size: usize) -> Result<(usize, usize)> {
        let p = match v {
            Value::Ptr(p) => *p,
            Value::Fig(Fig::Null) => {
                bail!("MCH3601: the pointer is not set (*NULL) where storage is used through it.")
            }
            other => bail!("a pointer is expected, not {}", other.describe()),
        };
        let buf = match p.kind {
            KIND_PROC | KIND_SYS => {
                bail!("MCH3601: a procedure or system pointer does not address storage.")
            }
            KIND_HEAP => {
                let h = p.buf as usize;
                if self.bufs.heap.freed.get(h).copied().unwrap_or(true) {
                    bail!("MCH3601: the pointer addresses heap storage that was freed (DEALLOC).");
                }
                HEAP_BASE + h
            }
            _ if p.owner == self.owner => p.buf as usize,
            _ => match self.bufs.lent.iter().position(|l| l.0 == p.owner) {
                Some(k) => LENT_BASE + (k << 24) + p.buf as usize,
                None => bail!(
                    "MCH3601: the pointer addresses storage of a program that is not running, or that this call cannot reach."
                ),
            },
        };
        let Some(b) = self.bufs.get(buf) else {
            bail!("MCH3601: the pointer addresses storage that no longer exists.");
        };
        if p.off as usize + size > b.len() {
            bail!(
                "MCH0601: {size} bytes at offset {} are outside the {} bytes of storage the pointer addresses.",
                p.off,
                b.len()
            );
        }
        Ok((buf, p.off as usize))
    }

    /// A based variable's storage now: where its basing pointer points
    /// (plus `off` into the based item, `size` bytes). A pointer that
    /// addresses nothing is the statement's error; meanwhile the
    /// variable is scratch storage.
    pub(super) fn based_place(&self, ptr: &str, off: usize, size: usize) -> (usize, usize) {
        let v = match self.sym(ptr) {
            Some(Sym::Var(pv)) if pv.dtype == DType::Pointer => self.load_var(&pv, None),
            _ => Err(anyhow!("BASED({ptr}): {ptr} is not a pointer")),
        };
        match v.and_then(|v| self.addr(&v, off + size)) {
            Ok((b, o)) => (b, o + off),
            Err(e) => {
                self.fault(e.to_string());
                (BAD_BUF, off)
            }
        }
    }

    /// The size of the largest based item: scratch storage for faults.
    pub(super) fn largest_based(prog: &Program) -> usize {
        let globals = prog
            .based
            .keys()
            .filter_map(|b| prog.globals.get(*b).map(|x| x.len()));
        let locals = prog.procs.values().flat_map(|p| {
            p.based
                .keys()
                .filter_map(|b| p.frame.get(*b).map(|x| x.len()))
        });
        globals.chain(locals).max().unwrap_or(0)
    }

    /// %ALLOC(size): new heap storage.
    pub(super) fn alloc(&mut self, size: &Value) -> Result<Value> {
        let n = size.num()?.to_i128()?;
        if !(1..=16_776_704).contains(&n) {
            bail!("RNQ0425: {n} bytes cannot be allocated (1 to 16,776,704 bytes).");
        }
        let h = self.bufs.heap.alloc(n as usize);
        Ok(self.ptr_to(HEAP_BASE + h, 0))
    }

    /// %REALLOC(ptr: size): the heap storage made that size, its bytes
    /// kept.
    pub(super) fn realloc(&mut self, p: &Value, size: &Value) -> Result<Value> {
        let n = size.num()?.to_i128()?;
        if !(1..=16_776_704).contains(&n) {
            bail!("RNQ0425: {n} bytes cannot be allocated (1 to 16,776,704 bytes).");
        }
        let (buf, off) = self.addr(p, 0)?;
        if !(HEAP_BASE..LENT_BASE).contains(&buf) || off != 0 {
            bail!("RNQ0426: the pointer for %REALLOC does not address heap storage from ALLOC or %ALLOC.");
        }
        self.bufs[buf].resize(n as usize, 0);
        Ok(p.clone())
    }

    /// DEALLOC: the heap storage freed.
    pub(super) fn dealloc(&mut self, p: &Value) -> Result<()> {
        if matches!(p, Value::Fig(Fig::Null)) {
            return Ok(());
        }
        let (buf, off) = self.addr(p, 0)?;
        if !(HEAP_BASE..LENT_BASE).contains(&buf) || off != 0 {
            bail!("RNQ0426: the pointer for DEALLOC does not address heap storage from ALLOC or %ALLOC.");
        }
        let h = buf - HEAP_BASE;
        self.bufs.heap.freed[h] = true;
        self.bufs.heap.blocks[h] = Vec::new();
        Ok(())
    }

    /// %STR(ptr) and %STR(ptr: max): the null-terminated string there
    /// (at most max - 1 characters with a maximum).
    pub(super) fn str_at(&self, p: &Value, max: Option<usize>) -> Result<Value> {
        let (buf, off) = self.addr(p, 0)?;
        let b = &self.bufs[buf];
        let end = match max {
            Some(m) => (off + m).min(b.len()),
            None => (off + STR_LIMIT).min(b.len()),
        };
        let s = &b[off..end];
        let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        Ok(Value::Char(s[..n].to_vec()))
    }

    /// %STR(ptr: max) = value: the value, cut to max - 1 characters, and
    /// a null character after it.
    pub(super) fn store_str(&mut self, p: &Value, max: usize, val: &Value) -> Result<()> {
        if max < 1 {
            bail!("the %STR maximum must be at least 1");
        }
        let text = val.chars()?;
        let n = text.len().min(max - 1);
        let (buf, off) = self.addr(p, n + 1)?;
        let b = &mut self.bufs[buf];
        b[off..off + n].copy_from_slice(&text[..n]);
        b[off + n] = 0;
        Ok(())
    }

    /// Pointer arithmetic: ptr + n, ptr - n, ptr - ptr (ILE RPG Reference,
    /// "Pointer arithmetic"); None when neither side is a pointer.
    pub(super) fn ptr_arith(&self, op: &BinOp, x: &Value, y: &Value) -> Option<Result<Value>> {
        let moved = |p: &Ptr, n: Result<Dec>, neg: bool| -> Result<Value> {
            let n = n?.to_i128()? as i64;
            p.add(if neg { -n } else { n })
                .map(Value::Ptr)
                .ok_or_else(|| {
                    anyhow!("MCH0601: pointer arithmetic gave an offset outside the storage.")
                })
        };
        match (op, x, y) {
            (BinOp::Add, Value::Ptr(p), n) => Some(moved(p, n.num(), false)),
            (BinOp::Add, n, Value::Ptr(p)) => Some(moved(p, n.num(), false)),
            (BinOp::Sub, Value::Ptr(p), Value::Ptr(q)) => {
                Some(if (p.kind, p.owner, p.buf) == (q.kind, q.owner, q.buf) {
                    Ok(Value::Num(Dec::from_i64(p.off as i64 - q.off as i64)))
                } else {
                    Err(anyhow!(
                        "the pointers do not address the same storage; they cannot be subtracted"
                    ))
                })
            }
            (BinOp::Sub, Value::Ptr(p), n) => Some(moved(p, n.num(), true)),
            (BinOp::Add | BinOp::Sub, Value::Fig(Fig::Null), Value::Num(_)) => Some(Err(anyhow!(
                "MCH3601: pointer arithmetic on a pointer that is not set (*NULL)."
            ))),
            _ => None,
        }
    }

    /// ALLOC, REALLOC and DEALLOC (fixed form: length in factor 2, pointer
    /// in the result field; free-form DEALLOC: the pointer). DEALLOC(N)
    /// sets the pointer to *NULL.
    pub(super) fn storage_op(
        &mut self,
        code: &str,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ext: &Ext,
    ) -> Result<bool> {
        match code {
            "ALLOC" | "REALLOC" => {
                let (Some(len), Some(target)) = (f2, result) else {
                    bail!("{code}: a length (factor 2) and a pointer (result) are needed");
                };
                let n = self.eval(len)?;
                let p = if code == "ALLOC" {
                    self.alloc(&n)?
                } else {
                    let old = self.eval(target)?;
                    self.realloc(&old, &n)?
                };
                self.assign(target, &p, Fit::Strict { half_adjust: false }, false)?;
                Ok(true)
            }
            "DEALLOC" => {
                let Some(target) = result.as_ref().or(f2.as_ref()) else {
                    bail!("DEALLOC: a pointer is needed");
                };
                let p = self.eval(target)?;
                self.dealloc(&p)?;
                if ext.other.contains('N') {
                    self.assign(
                        target,
                        &Value::Fig(Fig::Null),
                        Fit::Strict { half_adjust: false },
                        false,
                    )?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
