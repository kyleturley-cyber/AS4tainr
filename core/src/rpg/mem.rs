//! A program's storage as pointers see it: its own buffers (global and
//! procedure frames), the heap (%ALLOC, ALLOC), and the storage of the
//! callers a bound call lent it.
//!
//! A pointer (ILE RPG Reference, "Basing Pointer Data Type") is 16 bytes.
//! *NULL is 16 bytes of zeros. as4tainr writes an address as: byte 0 the
//! kind (1 a program's storage, 2 the heap), bytes 4-7 the storage's
//! owner (a program's run, see `Machine`), bytes 8-11 the buffer, bytes
//! 12-15 the offset. A program can only see the bytes through a pointer;
//! their layout is not something IBM i programs depend on (they cannot
//! make a pointer from bytes there either).

use std::ops::{Index, IndexMut};
use std::sync::atomic::{AtomicU32, Ordering};

/// Buffer numbers from here on are heap blocks.
pub const HEAP_BASE: usize = 1 << 40;
/// From here on, storage lent by a caller: (lender << 24) + its buffer.
pub const LENT_BASE: usize = 1 << 48;
/// The buffer a pointer that addresses nothing resolves to (reads and
/// writes go to scratch storage; the fault is reported for the
/// statement).
pub const BAD_BUF: usize = usize::MAX;

static NEXT_OWNER: AtomicU32 = AtomicU32::new(1);

/// A new owner number for a program's run.
pub fn new_owner() -> u32 {
    NEXT_OWNER.fetch_add(1, Ordering::Relaxed)
}

/// An address in storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ptr {
    pub kind: u8,
    pub owner: u32,
    pub buf: u32,
    pub off: u32,
}

pub const KIND_STORAGE: u8 = 1;
pub const KIND_HEAP: u8 = 2;

impl Ptr {
    pub fn encode(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = self.kind;
        b[4..8].copy_from_slice(&self.owner.to_be_bytes());
        b[8..12].copy_from_slice(&self.buf.to_be_bytes());
        b[12..16].copy_from_slice(&self.off.to_be_bytes());
        b
    }

    /// None for *NULL (or bytes that are not an address).
    pub fn decode(b: &[u8]) -> Option<Ptr> {
        if b.len() < 16 || !matches!(b[0], KIND_STORAGE | KIND_HEAP) {
            return None;
        }
        let u = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Some(Ptr {
            kind: b[0],
            owner: u(4),
            buf: u(8),
            off: u(12),
        })
    }

    /// The address `n` bytes on (pointer arithmetic).
    pub fn add(&self, n: i64) -> Option<Ptr> {
        let off = self.off as i64 + n;
        (0..=u32::MAX as i64).contains(&off).then_some(Ptr {
            off: off as u32,
            ..*self
        })
    }
}

/// Heap storage (ALLOC, %ALLOC, %REALLOC, DEALLOC).
#[derive(Clone, Debug, Default)]
pub struct Heap {
    pub blocks: Vec<Vec<u8>>,
    pub freed: Vec<bool>,
}

impl Heap {
    pub fn alloc(&mut self, size: usize) -> usize {
        self.blocks.push(vec![0; size]);
        self.freed.push(false);
        self.blocks.len() - 1
    }
}

/// A program's storage.
#[derive(Clone, Debug, Default)]
pub struct Mem {
    pub own: Vec<Vec<u8>>,
    pub heap: Heap,
    /// Storage of callers, lent for a bound call: (owner, buffers).
    pub lent: Vec<(u32, Vec<Vec<u8>>)>,
    scratch: Vec<u8>,
}

impl Mem {
    pub fn new(own: Vec<Vec<u8>>) -> Mem {
        Mem {
            own,
            heap: Heap::default(),
            lent: Vec::new(),
            scratch: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.own.len()
    }

    pub fn is_empty(&self) -> bool {
        self.own.is_empty()
    }

    pub fn push(&mut self, b: Vec<u8>) {
        self.own.push(b);
    }

    pub fn truncate(&mut self, n: usize) {
        self.own.truncate(n);
    }

    pub fn extend<I: IntoIterator<Item = Vec<u8>>>(&mut self, it: I) {
        self.own.extend(it);
    }

    pub fn get(&self, i: usize) -> Option<&Vec<u8>> {
        if i == BAD_BUF {
            None
        } else if i >= LENT_BASE {
            let k = (i - LENT_BASE) >> 24;
            self.lent
                .get(k)
                .and_then(|l| l.1.get((i - LENT_BASE) & 0xFF_FFFF))
        } else if i >= HEAP_BASE {
            self.heap.blocks.get(i - HEAP_BASE)
        } else {
            self.own.get(i)
        }
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut Vec<u8>> {
        if i == BAD_BUF {
            None
        } else if i >= LENT_BASE {
            let k = (i - LENT_BASE) >> 24;
            self.lent
                .get_mut(k)
                .and_then(|l| l.1.get_mut((i - LENT_BASE) & 0xFF_FFFF))
        } else if i >= HEAP_BASE {
            self.heap.blocks.get_mut(i - HEAP_BASE)
        } else {
            self.own.get_mut(i)
        }
    }

    /// Scratch storage of at least `n` bytes, for a pointer fault.
    pub fn scratch(&mut self, n: usize) {
        if self.scratch.len() < n {
            self.scratch.resize(n, 0);
        }
    }
}

impl Index<usize> for Mem {
    type Output = Vec<u8>;
    fn index(&self, i: usize) -> &Vec<u8> {
        match self.get(i) {
            Some(b) => b,
            None => &self.scratch,
        }
    }
}

impl IndexMut<usize> for Mem {
    fn index_mut(&mut self, i: usize) -> &mut Vec<u8> {
        if self.get(i).is_some() {
            self.get_mut(i).unwrap()
        } else {
            &mut self.scratch
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_encode_and_null() {
        // ILE RPG Reference: *NULL is a pointer with no address; as4tainr
        // keeps it as 16 zero bytes.
        assert_eq!(Ptr::decode(&[0u8; 16]), None);
        let p = Ptr {
            kind: KIND_HEAP,
            owner: 7,
            buf: 3,
            off: 10,
        };
        assert_eq!(Ptr::decode(&p.encode()), Some(p));
        assert_eq!(p.add(-10).unwrap().off, 0);
        assert!(p.add(-11).is_none());
    }

    #[test]
    fn storage_spaces() {
        let mut m = Mem::new(vec![vec![1, 2], vec![3]]);
        let h = m.heap.alloc(4);
        m.lent.push((9, vec![vec![5, 6, 7]]));
        assert_eq!(m[1], vec![3]);
        m[HEAP_BASE + h][2] = 8;
        assert_eq!(m[HEAP_BASE + h], vec![0, 0, 8, 0]);
        assert_eq!(m[LENT_BASE][1], 6);
        m.scratch(16);
        assert_eq!(m[BAD_BUF].len(), 16);
    }
}
