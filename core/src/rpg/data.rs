//! RPG data: types, values, and how values are stored in bytes.
//!
//! Every variable lives in bytes in its declared format, as on IBM i:
//! character data in EBCDIC (CCSID 37), zoned decimal (one digit per
//! byte, sign in the zone of the last byte), packed decimal (two digits
//! per byte, sign in the last half byte), binary and integer (big-endian
//! two's complement), indicators ('1'/'0'). Data structure subfields are
//! views of the structure's bytes, so overlays behave as on IBM i.
//! (Formats: ILE RPG Reference, "Data Types and Data Formats".)

use super::dec::Dec;
use crate::tn5250::ebcdic;
use anyhow::{anyhow, bail, Result};

pub const BLANK: u8 = 0x40;

/// Text to EBCDIC.
pub fn e(s: &str) -> Vec<u8> {
    ebcdic::encode(s)
}

/// EBCDIC to text, byte for byte (NUL stays NUL).
pub fn a(b: &[u8]) -> String {
    b.iter()
        .map(|&x| char::from_u32(ebcdic::TO_UNICODE[x as usize] as u32).unwrap_or('?'))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DType {
    /// Fixed-length character (A).
    Char(u32),
    /// Varying-length character (A VARYING / VARCHAR): a 2-byte length,
    /// then up to this many characters.
    Varchar(u32),
    /// Zoned decimal (S): digits, decimal places.
    Zoned(u32, u32),
    /// Packed decimal (P).
    Packed(u32, u32),
    /// Binary decimal (B): digits (1-9), decimal places.
    Binary(u32, u32),
    /// Signed integer (I): 3, 5, 10 or 20 digits.
    Int(u32),
    /// Unsigned integer (U).
    Uns(u32),
    /// Indicator (N).
    Ind,
    /// Date (D) in *ISO form, 10 characters.
    Date,
    /// Time (T) in *ISO form, 8 characters.
    Time,
    /// Timestamp (Z), 26 characters.
    Timestamp,
    /// Pointer (*): 16 bytes, opaque here.
    Pointer,
    /// Float (F): 4 or 8 bytes, IEEE binary floating point.
    Float(u32),
    /// UCS-2 (C) and graphic (G) data: this many characters of 2 bytes
    /// (UTF-16BE, CCSID 13488/1200).
    Ucs2(u32),
    /// Varying UCS-2 or graphic: a 2-byte length in characters, then the
    /// characters.
    VarUcs2(u32),
}

impl DType {
    pub fn size(&self) -> usize {
        match *self {
            DType::Char(n) => n as usize,
            DType::Varchar(n) => n as usize + 2,
            DType::Zoned(d, _) => d as usize,
            DType::Packed(d, _) => d as usize / 2 + 1,
            // ILE RPG Reference, binary format: 1-4 digits in 2 bytes,
            // 5-9 in 4, 10-18 in 8.
            DType::Binary(d, _) => {
                if d <= 4 {
                    2
                } else if d <= 9 {
                    4
                } else {
                    8
                }
            }
            DType::Float(n) => n as usize,
            DType::Ucs2(n) => 2 * n as usize,
            DType::VarUcs2(n) => 2 * n as usize + 2,
            DType::Int(d) | DType::Uns(d) => match d {
                3 => 1,
                5 => 2,
                10 => 4,
                _ => 8,
            },
            DType::Ind => 1,
            DType::Date => 10,
            DType::Time => 8,
            DType::Timestamp => 26,
            DType::Pointer => 16,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            DType::Zoned(..)
                | DType::Packed(..)
                | DType::Binary(..)
                | DType::Int(_)
                | DType::Uns(_)
                | DType::Float(_)
        )
    }

    pub fn is_char(&self) -> bool {
        matches!(
            self,
            DType::Char(_)
                | DType::Varchar(_)
                | DType::Date
                | DType::Time
                | DType::Timestamp
                | DType::Ucs2(_)
                | DType::VarUcs2(_)
        )
    }

    /// Digits and decimal places of a numeric type.
    pub fn precision(&self) -> (u32, u32) {
        match *self {
            DType::Zoned(d, p) | DType::Packed(d, p) | DType::Binary(d, p) => (d, p),
            DType::Int(d) | DType::Uns(d) => (d, 0),
            _ => (0, 0),
        }
    }

    /// Length in characters (or digits) as %LEN gives it.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u32 {
        match *self {
            DType::Char(n) | DType::Varchar(n) | DType::Ucs2(n) | DType::VarUcs2(n) => n,
            DType::Ind => 1,
            DType::Date => 10,
            DType::Time => 8,
            DType::Timestamp => 26,
            DType::Pointer => 16,
            n => n.precision().0,
        }
    }

    /// The type's initial value: blanks, zeros, '0' (ILE RPG reference:
    /// default initialization).
    pub fn initial(&self) -> Vec<u8> {
        let mut b = vec![0u8; self.size()];
        match *self {
            DType::Char(_) => b.fill(BLANK),
            DType::Varchar(_) => {} // length 0
            DType::Zoned(..)
            | DType::Packed(..)
            | DType::Binary(..)
            | DType::Int(_)
            | DType::Uns(_) => {
                encode_num(self, &Dec::ZERO, &mut b).expect("zero fits");
            }
            DType::Ind => b[0] = 0xF0,
            // Float: all zero bytes are 0.0.
            DType::Float(_) => {}
            // UCS-2 blanks (U+0020); a varying field starts empty.
            DType::Ucs2(_) => {
                for pair in b.chunks_mut(2) {
                    pair.copy_from_slice(&[0x00, 0x20]);
                }
            }
            DType::VarUcs2(_) => {}
            DType::Date => b.copy_from_slice(&e("0001-01-01")),
            DType::Time => b.copy_from_slice(&e("00.00.00")),
            DType::Timestamp => b.copy_from_slice(&e("0001-01-01-00.00.00.000000")),
            DType::Pointer => {}
        }
        b
    }
}

/// Figurative constants (ILE RPG reference, "Figurative Constants").
#[derive(Clone, Debug, PartialEq)]
pub enum Fig {
    Blanks,
    Zeros,
    HiVal,
    LoVal,
    On,
    Off,
    Null,
    /// *ALL'x...': the pattern repeated.
    All(Vec<u8>),
}

/// A value while an expression is evaluated.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Character data, EBCDIC.
    Char(Vec<u8>),
    Num(Dec),
    Ind(bool),
    Fig(Fig),
    /// A duration (%DAYS, %MONTHS, %YEARS, %HOURS, %MINUTES, %SECONDS,
    /// %MSECONDS) for date, time and timestamp arithmetic.
    Dur(DurUnit, i64),
    /// A list of values: an array, %LIST, %SPLIT, %SUBARR, an enum.
    List(Vec<Value>),
}

/// The unit of a duration (ILE RPG Reference, %DIFF and the duration
/// built-in functions).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DurUnit {
    Years,
    Months,
    Days,
    Hours,
    Minutes,
    Seconds,
    MSeconds,
}

impl DurUnit {
    /// *YEARS or *Y, *MONTHS or *M, *DAYS or *D, *HOURS or *H, *MINUTES
    /// or *MN, *SECONDS or *S, *MSECONDS or *MS.
    pub fn parse(s: &str) -> Option<DurUnit> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "*YEARS" | "*Y" => DurUnit::Years,
            "*MONTHS" | "*M" => DurUnit::Months,
            "*DAYS" | "*D" => DurUnit::Days,
            "*HOURS" | "*H" => DurUnit::Hours,
            "*MINUTES" | "*MN" => DurUnit::Minutes,
            "*SECONDS" | "*S" => DurUnit::Seconds,
            "*MSECONDS" | "*MS" => DurUnit::MSeconds,
            _ => return None,
        })
    }
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Char(e(s))
    }

    pub fn num(&self) -> Result<Dec> {
        match self {
            Value::Num(d) => Ok(*d),
            Value::Fig(Fig::Zeros) | Value::Fig(Fig::LoVal) => Ok(Dec::ZERO),
            Value::Ind(b) => Ok(Dec::from_i64(*b as i64)),
            other => bail!("expected a number, got {}", other.describe()),
        }
    }

    pub fn chars(&self) -> Result<Vec<u8>> {
        match self {
            Value::Char(c) => Ok(c.clone()),
            Value::Ind(b) => Ok(vec![if *b { 0xF1 } else { 0xF0 }]),
            Value::Fig(Fig::Blanks) => Ok(vec![]),
            other => bail!("expected character data, got {}", other.describe()),
        }
    }

    pub fn truth(&self) -> Result<bool> {
        match self {
            Value::Ind(b) => Ok(*b),
            Value::Fig(Fig::On) => Ok(true),
            Value::Fig(Fig::Off) => Ok(false),
            Value::Char(c) if c.len() == 1 => Ok(c[0] == 0xF1),
            other => bail!(
                "expected an indicator or a condition, got {}",
                other.describe()
            ),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Value::Char(c) => format!("'{}'", a(c)),
            Value::Num(d) => d.to_string(),
            Value::Ind(b) => if *b { "*ON" } else { "*OFF" }.into(),
            Value::Fig(f) => format!("{f:?}"),
            Value::Dur(u, n) => format!("{n} {u:?}"),
            Value::List(v) => format!("a list of {} values", v.len()),
        }
    }
}

/// How a value goes into a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// EVAL: decimal places truncated (or rounded with H); too many
    /// integer digits is an error (RNQ0103).
    Strict { half_adjust: bool },
    /// Fixed-form arithmetic (ADD, Z-ADD, MULT...): high-order digits
    /// that do not fit are lost, without an error.
    Truncate { half_adjust: bool },
}

/// Store a number in a numeric field's bytes.
pub fn encode_num(t: &DType, v: &Dec, out: &mut [u8]) -> Result<()> {
    if let DType::Float(n) = *t {
        // A float field holds a binary approximation of the value.
        let x: f64 = v
            .to_string()
            .parse()
            .map_err(|_| anyhow!("{v} is not a number"))?;
        if n == 4 {
            out.copy_from_slice(&(x as f32).to_be_bytes());
        } else {
            out.copy_from_slice(&x.to_be_bytes());
        }
        return Ok(());
    }
    let (digits, decimals) = t.precision();
    let d = v.rescale(decimals as u8, false)?;
    match *t {
        DType::Zoned(..) => {
            let s = d.digits(digits, decimals)?;
            for (i, c) in s.bytes().enumerate() {
                out[i] = 0xF0 | (c - b'0');
            }
            if d.is_negative() {
                let last = out.len() - 1;
                out[last] = 0xD0 | (out[last] & 0x0F);
            }
        }
        DType::Packed(..) => {
            let s = d.digits(digits, decimals)?;
            // An odd number of digits: pad with a leading zero.
            let s = if s.len() % 2 == 0 { format!("0{s}") } else { s };
            let mut nibbles: Vec<u8> = s.bytes().map(|c| c - b'0').collect();
            nibbles.push(if d.is_negative() { 0x0D } else { 0x0F });
            for (i, pair) in nibbles.chunks(2).enumerate() {
                out[i] = (pair[0] << 4) | pair[1];
            }
        }
        DType::Binary(..) | DType::Int(_) | DType::Uns(_) => {
            let m = d.mantissa();
            if matches!(t, DType::Uns(_)) && m < 0 {
                bail!(
                    "RNQ0103: The target for a numeric operation is too small to hold the result."
                );
            }
            let bytes = m.to_be_bytes();
            let n = out.len();
            out.copy_from_slice(&bytes[16 - n..]);
        }
        _ => bail!("not a numeric type"),
    }
    Ok(())
}

/// Read a number from a numeric field's bytes. Invalid digits are an
/// error (RNQ0907, decimal data error).
pub fn decode_num(t: &DType, b: &[u8]) -> Result<Dec> {
    if let DType::Float(n) = *t {
        return float_dec(float_of(n, b));
    }
    let (_, decimals) = t.precision();
    let bad = || anyhow!("RNQ0907: Decimal-data error occurred (field contains invalid digits).");
    let m: i128 = match *t {
        DType::Zoned(..) => {
            let mut m: i128 = 0;
            for &x in b {
                let d = x & 0x0F;
                if d > 9 {
                    return Err(bad());
                }
                m = m * 10 + d as i128;
            }
            let zone = b.last().map(|x| x >> 4).unwrap_or(0xF);
            if zone == 0xD || zone == 0xB {
                -m
            } else {
                m
            }
        }
        DType::Packed(..) => {
            let mut m: i128 = 0;
            let n = b.len();
            for (i, &x) in b.iter().enumerate() {
                let hi = x >> 4;
                if hi > 9 {
                    return Err(bad());
                }
                m = m * 10 + hi as i128;
                if i + 1 < n {
                    let lo = x & 0x0F;
                    if lo > 9 {
                        return Err(bad());
                    }
                    m = m * 10 + lo as i128;
                }
            }
            let sign = b.last().map(|x| x & 0x0F).unwrap_or(0xF);
            match sign {
                0x0D | 0x0B => -m,
                0x0A | 0x0C | 0x0E | 0x0F => m,
                _ => return Err(bad()),
            }
        }
        DType::Binary(..) | DType::Int(_) => {
            let mut buf = if b.first().is_some_and(|x| x & 0x80 != 0) {
                [0xFFu8; 16]
            } else {
                [0u8; 16]
            };
            buf[16 - b.len()..].copy_from_slice(b);
            i128::from_be_bytes(buf)
        }
        DType::Uns(_) => {
            let mut buf = [0u8; 16];
            buf[16 - b.len()..].copy_from_slice(b);
            i128::from_be_bytes(buf)
        }
        _ => bail!("not a numeric type"),
    };
    Ok(Dec::new(m, decimals as u8))
}

/// Store a value in a field of type `t` (bytes `out`), converting as RPG
/// assignment does. `right`: right-adjust character data (EVALR).
pub fn store(t: &DType, v: &Value, out: &mut [u8], fit: Fit, right: bool) -> Result<()> {
    match t {
        DType::Char(_) | DType::Date | DType::Time | DType::Timestamp => {
            let src = match v {
                Value::Fig(f) => return fill_fig(t, f, out),
                Value::Num(_) => bail!("A numeric value cannot be assigned to a character field."),
                other => other.chars()?,
            };
            put_chars(&src, out, right);
        }
        DType::Varchar(max) => {
            let src = match v {
                Value::Fig(Fig::Blanks) | Value::Fig(Fig::Null) => vec![],
                Value::Fig(f) => {
                    let mut tmp = vec![0u8; *max as usize];
                    fill_fig(&DType::Char(*max), f, &mut tmp)?;
                    tmp
                }
                Value::Num(_) => bail!("A numeric value cannot be assigned to a character field."),
                other => other.chars()?,
            };
            let n = src.len().min(*max as usize);
            out[0..2].copy_from_slice(&(n as u16).to_be_bytes());
            out[2..2 + n].copy_from_slice(&src[..n]);
        }
        DType::Ind => {
            out[0] = if v.truth()? { 0xF1 } else { 0xF0 };
        }
        DType::Ucs2(_) | DType::VarUcs2(_) => {
            let text: String = match v {
                Value::Fig(Fig::Blanks) | Value::Fig(Fig::Null) => String::new(),
                Value::Fig(f) => {
                    let n = t.len() as usize;
                    let mut tmp = vec![0u8; n];
                    fill_fig(&DType::Char(n as u32), f, &mut tmp)?;
                    a(&tmp)
                }
                Value::Num(_) => bail!("A numeric value cannot be assigned to a character field."),
                other => a(&other.chars()?),
            };
            let units: Vec<u16> = text.encode_utf16().collect();
            match t {
                DType::Ucs2(n) => {
                    let n = *n as usize;
                    let mut u: Vec<u16> = if right && units.len() < n {
                        let mut v = vec![0x20u16; n - units.len()];
                        v.extend(&units);
                        v
                    } else if right {
                        units[units.len() - n..].to_vec()
                    } else {
                        units.iter().copied().take(n).collect()
                    };
                    u.resize(n, 0x20);
                    for (i, x) in u.iter().enumerate() {
                        out[2 * i..2 * i + 2].copy_from_slice(&x.to_be_bytes());
                    }
                }
                _ => {
                    let n = units.len().min(t.len() as usize);
                    out[0..2].copy_from_slice(&(n as u16).to_be_bytes());
                    for (i, x) in units.iter().take(n).enumerate() {
                        out[2 + 2 * i..4 + 2 * i].copy_from_slice(&x.to_be_bytes());
                    }
                }
            }
        }
        DType::Pointer => {
            if !matches!(v, Value::Fig(Fig::Null)) {
                bail!("pointers can only be set to *NULL here");
            }
            out.fill(0);
        }
        DType::Float(n) => {
            let x = match v {
                Value::Fig(Fig::Zeros) => Dec::ZERO,
                Value::Fig(Fig::HiVal) | Value::Fig(Fig::LoVal) => {
                    // The largest float (ILE RPG: *HIVAL/*LOVAL for float).
                    let m = if *n == 4 { f32::MAX as f64 } else { f64::MAX };
                    out.copy_from_slice(&if *n == 4 {
                        let x = if matches!(v, Value::Fig(Fig::HiVal)) {
                            m
                        } else {
                            -m
                        } as f32;
                        x.to_be_bytes().to_vec()
                    } else {
                        let x = if matches!(v, Value::Fig(Fig::HiVal)) {
                            m
                        } else {
                            -m
                        };
                        x.to_be_bytes().to_vec()
                    });
                    return Ok(());
                }
                Value::Char(_) | Value::Fig(Fig::Blanks) => {
                    bail!("Character data cannot be assigned to a numeric field.")
                }
                other => other.num()?,
            };
            encode_num(t, &x, out)?;
        }
        _ => {
            let n = match v {
                Value::Fig(Fig::HiVal) | Value::Fig(Fig::LoVal) => {
                    let (d, p) = t.precision();
                    let nines = Dec::parse(&"9".repeat(d as usize))?
                        .div_scale(&Dec::from_i64(10i64.pow(p)), p as u8)?;
                    if matches!(v, Value::Fig(Fig::HiVal)) {
                        nines
                    } else if matches!(t, DType::Uns(_)) {
                        Dec::ZERO
                    } else {
                        nines.neg()
                    }
                }
                Value::Fig(Fig::Blanks) => bail!("*BLANKS cannot be assigned to a numeric field."),
                Value::Char(_) => bail!("Character data cannot be assigned to a numeric field."),
                other => other.num()?,
            };
            let (digits, decimals) = t.precision();
            let n = match fit {
                Fit::Strict { half_adjust } => {
                    let r = n.rescale(decimals as u8, half_adjust)?;
                    if !r.fits(digits, decimals) {
                        bail!("RNQ0103: The target for a numeric operation is too small to hold the result.");
                    }
                    r
                }
                Fit::Truncate { half_adjust } => n.fit_truncating(digits, decimals, half_adjust)?,
            };
            encode_num(t, &n, out)?;
        }
    }
    Ok(())
}

/// Read a field's value.
pub fn load(t: &DType, b: &[u8]) -> Result<Value> {
    Ok(match t {
        DType::Char(_) | DType::Date | DType::Time | DType::Timestamp => Value::Char(b.to_vec()),
        DType::Varchar(max) => {
            let n = (u16::from_be_bytes([b[0], b[1]]) as usize).min(*max as usize);
            Value::Char(b[2..2 + n].to_vec())
        }
        // UCS-2 and graphic data are converted to the job's character set
        // (CCSID 37) when used with character data.
        DType::Ucs2(_) => Value::Char(e(&utf16_text(b))),
        DType::VarUcs2(max) => {
            let n = (u16::from_be_bytes([b[0], b[1]]) as usize).min(*max as usize);
            Value::Char(e(&utf16_text(&b[2..2 + 2 * n])))
        }
        DType::Ind => Value::Ind(b[0] == 0xF1),
        DType::Pointer => Value::Fig(Fig::Null),
        _ => Value::Num(decode_num(t, b)?),
    })
}

fn put_chars(src: &[u8], out: &mut [u8], right: bool) {
    let n = out.len();
    if right {
        if src.len() >= n {
            out.copy_from_slice(&src[src.len() - n..]);
        } else {
            let pad = n - src.len();
            out[..pad].fill(BLANK);
            out[pad..].copy_from_slice(src);
        }
    } else if src.len() >= n {
        out.copy_from_slice(&src[..n]);
    } else {
        out[..src.len()].copy_from_slice(src);
        out[src.len()..].fill(BLANK);
    }
}

fn fill_fig(t: &DType, f: &Fig, out: &mut [u8]) -> Result<()> {
    match f {
        Fig::Blanks => out.fill(BLANK),
        Fig::Zeros => out.fill(0xF0),
        Fig::HiVal => out.fill(0xFF),
        Fig::LoVal => out.fill(0x00),
        Fig::On => out.fill(0xF1),
        Fig::Off => out.fill(0xF0),
        Fig::Null => out.fill(BLANK),
        Fig::All(p) => {
            if p.is_empty() {
                bail!("*ALL needs a value");
            }
            for (i, b) in out.iter_mut().enumerate() {
                *b = p[i % p.len()];
            }
        }
    }
    let _ = t;
    Ok(())
}

/// Compare two values as RPG does: numbers by value; character data
/// byte by byte in EBCDIC order, the shorter padded with blanks.
pub fn compare(x: &Value, y: &Value) -> Result<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let fig_char = |f: &Fig, len: usize| -> Vec<u8> {
        let mut v = vec![0u8; len.max(1)];
        let _ = fill_fig(&DType::Char(len as u32), f, &mut v);
        v
    };
    Ok(match (x, y) {
        (Value::Num(a), b) => a.cmp(&fig_num(b)?),
        (a, Value::Num(b)) => fig_num(a)?.cmp(b),
        (Value::Ind(a), b) => a.cmp(&b.truth()?),
        (a, Value::Ind(b)) => a.truth()?.cmp(b),
        (Value::Char(a), Value::Fig(f)) => cmp_chars(a, &fig_char(f, a.len())),
        (Value::Fig(f), Value::Char(b)) => cmp_chars(&fig_char(f, b.len()), b),
        (Value::Char(a), Value::Char(b)) => cmp_chars(a, b),
        (Value::Fig(a), Value::Fig(b)) => {
            if a == b {
                Ordering::Equal
            } else {
                cmp_chars(&fig_char(a, 1), &fig_char(b, 1))
            }
        }
        (a, b) => bail!("{} and {} cannot be compared", a.describe(), b.describe()),
    })
}

fn fig_num(v: &Value) -> Result<Dec> {
    match v {
        Value::Fig(Fig::HiVal) => Ok(Dec::new(i128::MAX / 10, 0)),
        Value::Fig(Fig::LoVal) => Ok(Dec::new(-(i128::MAX / 10), 0)),
        other => other.num(),
    }
}

fn cmp_chars(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(BLANK);
        let y = b.get(i).copied().unwrap_or(BLANK);
        if x != y {
            return x.cmp(&y);
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(t: DType, v: &str) -> Vec<u8> {
        let mut b = t.initial();
        store(
            &t,
            &Value::Num(Dec::parse(v).unwrap()),
            &mut b,
            Fit::Strict { half_adjust: false },
            false,
        )
        .unwrap();
        b
    }

    #[test]
    fn storage_formats() {
        // ILE RPG reference, data formats: zoned -123 in 5 digits is
        // F0 F0 F1 F2 D3; packed 5,2 value 12.34 is 01 23 4F; binary 4
        // digits (2 bytes) -2 is FF FE; integer 10 digits is 4 bytes.
        assert_eq!(
            bytes(DType::Zoned(5, 0), "-123"),
            vec![0xF0, 0xF0, 0xF1, 0xF2, 0xD3]
        );
        assert_eq!(bytes(DType::Packed(5, 2), "12.34"), vec![0x01, 0x23, 0x4F]);
        assert_eq!(bytes(DType::Packed(4, 0), "-7"), vec![0x00, 0x00, 0x7D]);
        assert_eq!(bytes(DType::Binary(4, 0), "-2"), vec![0xFF, 0xFE]);
        assert_eq!(DType::Int(10).size(), 4);
        for (t, v) in [
            (DType::Zoned(7, 2), "-12345.67"),
            (DType::Packed(9, 3), "123456.789"),
            (DType::Int(5), "-32768"),
            (DType::Uns(10), "4294967295"),
        ] {
            let b = bytes(t, v);
            assert_eq!(load(&t, &b).unwrap(), Value::Num(Dec::parse(v).unwrap()));
        }
    }

    #[test]
    fn assignment_rules() {
        let t = DType::Packed(3, 0);
        let mut b = t.initial();
        // EVAL: too many digits is an error (RNQ0103).
        let e = store(
            &t,
            &Value::Num(Dec::from_i64(1234)),
            &mut b,
            Fit::Strict { half_adjust: false },
            false,
        );
        assert!(e.unwrap_err().to_string().starts_with("RNQ0103"));
        // Z-ADD, ADD...: the high-order digits are lost.
        store(
            &t,
            &Value::Num(Dec::from_i64(1234)),
            &mut b,
            Fit::Truncate { half_adjust: false },
            false,
        )
        .unwrap();
        assert_eq!(load(&t, &b).unwrap(), Value::Num(Dec::from_i64(234)));
        // Character: left-adjusted and padded; EVALR right-adjusts.
        let c = DType::Char(5);
        let mut b = c.initial();
        store(
            &c,
            &Value::str("AB"),
            &mut b,
            Fit::Strict { half_adjust: false },
            false,
        )
        .unwrap();
        assert_eq!(a(&b), "AB   ");
        store(
            &c,
            &Value::str("AB"),
            &mut b,
            Fit::Strict { half_adjust: false },
            true,
        )
        .unwrap();
        assert_eq!(a(&b), "   AB");
        store(
            &c,
            &Value::str("ABCDEFG"),
            &mut b,
            Fit::Strict { half_adjust: false },
            true,
        )
        .unwrap();
        assert_eq!(a(&b), "CDEFG");
    }

    #[test]
    fn ebcdic_order() {
        // CCSID 37: lower case sorts before upper case, letters before digits.
        assert_eq!(
            compare(&Value::str("a"), &Value::str("A")).unwrap(),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare(&Value::str("Z"), &Value::str("1")).unwrap(),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare(&Value::str("AB"), &Value::str("AB  ")).unwrap(),
            std::cmp::Ordering::Equal
        );
    }
}

/// The value of a float field's bytes.
pub fn float_of(n: u32, b: &[u8]) -> f64 {
    if n == 4 {
        f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64
    } else {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8]);
        f64::from_be_bytes(a)
    }
}

/// A float as an exact decimal (its shortest decimal form); values that do
/// not fit 38 digits are an error.
pub fn float_dec(x: f64) -> Result<Dec> {
    if !x.is_finite() {
        bail!("RNQ0103: The float value is not a finite number.");
    }
    let text = format!("{x}");
    // Shortest form, without an exponent (Rust's Display): very small
    // values are rounded to 31 decimal places.
    Dec::parse(&text).or_else(|_| {
        let short = format!("{x:.31}");
        Dec::parse(short.trim_end_matches('0'))
    })
}

/// The external display representation of a float (ILE RPG Reference):
/// a sign, one digit, a decimal point, 15 digits (8-byte) or 6 digits
/// (4-byte), E, and a signed three-digit exponent: +1.230000000000000E+002.
pub fn float_text(x: f64, bytes: u32) -> String {
    let digits = if bytes == 4 { 6 } else { 15 };
    let s = format!("{:.*e}", digits, x.abs());
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = exp.parse().unwrap_or(0);
    format!(
        "{}{mant}E{}{:03}",
        if x.is_sign_negative() && x != 0.0 {
            '-'
        } else {
            '+'
        },
        if e < 0 { '-' } else { '+' },
        e.abs()
    )
}

/// UTF-16BE bytes as text.
pub fn utf16_text(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod float_tests {
    use super::*;

    #[test]
    fn float_display_form() {
        // External display representation (IBM support, "Floating point
        // numbers"): +1.230000000000000E+002 for 123 in an 8-byte float.
        assert_eq!(float_text(123.0, 8), "+1.230000000000000E+002");
        assert_eq!(float_text(-0.00125, 8), "-1.250000000000000E-003");
        assert_eq!(float_text(0.5, 4), "+5.000000E-001");
        assert_eq!(float_text(0.0, 8), "+0.000000000000000E+000");
        // Stored and read back through the field's bytes.
        let mut b = vec![0u8; 8];
        encode_num(&DType::Float(8), &Dec::parse("1234.5678").unwrap(), &mut b).unwrap();
        assert_eq!(
            decode_num(&DType::Float(8), &b).unwrap().to_string(),
            "1234.5678"
        );
        // BINDEC of 10-18 digits takes 8 bytes (ILE RPG Reference).
        assert_eq!(DType::Binary(18, 0).size(), 8);
        // UCS-2: two bytes a character, blanks are U+0020.
        assert_eq!(DType::Ucs2(2).initial(), vec![0, 0x20, 0, 0x20]);
    }
}
