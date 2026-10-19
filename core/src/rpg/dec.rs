//! Exact decimal numbers for RPG: a signed integer mantissa and a number
//! of decimal places. No floating point anywhere.
//!
//! RPG fields hold at most 63 digits; this type holds 38 (an i128), which
//! covers every field in practice. Results that need more digits fail with
//! an error rather than lose digits silently.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::fmt;

/// The most digits a value can have here.
pub const MAX_DIGITS: u32 = 38;

/// Decimal places kept in the quotient of a division before it is
/// assigned (the assignment then truncates or rounds it to the target).
pub const DIV_SCALE: u8 = 18;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dec {
    /// value = m / 10^s
    m: i128,
    s: u8,
}

fn pow10(n: u32) -> Result<i128> {
    10i128
        .checked_pow(n)
        .ok_or_else(|| anyhow!("a number has more than {MAX_DIGITS} digits"))
}

impl Dec {
    pub const ZERO: Dec = Dec { m: 0, s: 0 };

    pub fn new(m: i128, s: u8) -> Dec {
        Dec { m, s }
    }

    pub fn from_i64(v: i64) -> Dec {
        Dec { m: v as i128, s: 0 }
    }

    pub fn mantissa(&self) -> i128 {
        self.m
    }

    pub fn scale(&self) -> u8 {
        self.s
    }

    pub fn is_zero(&self) -> bool {
        self.m == 0
    }

    pub fn is_negative(&self) -> bool {
        self.m < 0
    }

    pub fn neg(&self) -> Dec {
        Dec {
            m: -self.m,
            s: self.s,
        }
    }

    pub fn abs(&self) -> Dec {
        Dec {
            m: self.m.abs(),
            s: self.s,
        }
    }

    /// Parse decimal text: optional sign (leading or trailing), digits,
    /// optional point. Blanks around are ignored; empty text is zero.
    pub fn parse(text: &str) -> Result<Dec> {
        let t = text.trim();
        if t.is_empty() {
            return Ok(Dec::ZERO);
        }
        let (neg, body) = if let Some(r) = t.strip_prefix('-') {
            (true, r.trim_start())
        } else if let Some(r) = t.strip_prefix('+') {
            (false, r.trim_start())
        } else if let Some(r) = t.strip_suffix('-') {
            (true, r.trim_end())
        } else if let Some(r) = t.strip_suffix('+') {
            (false, r.trim_end())
        } else {
            (false, t)
        };
        let body = body.replace(',', ".");
        let (int, frac) = match body.split_once('.') {
            Some((i, f)) => (i, f),
            None => (body.as_str(), ""),
        };
        if int.is_empty() && frac.is_empty()
            || !int.chars().all(|c| c.is_ascii_digit())
            || !frac.chars().all(|c| c.is_ascii_digit())
        {
            bail!("'{text}' is not a valid number");
        }
        let digits = format!("{int}{frac}");
        let digits = digits.trim_start_matches('0');
        if digits.len() as u32 > MAX_DIGITS {
            bail!("'{text}' has more than {MAX_DIGITS} digits");
        }
        let m: i128 = if digits.is_empty() {
            0
        } else {
            digits.parse()?
        };
        Ok(Dec {
            m: if neg { -m } else { m },
            s: frac.len() as u8,
        })
    }

    /// The value with exactly `scale` decimal places: extra places are
    /// dropped (truncated) or, with `half_adjust`, rounded half away from
    /// zero, as RPG's (H) extender does.
    pub fn rescale(&self, scale: u8, half_adjust: bool) -> Result<Dec> {
        match scale.cmp(&self.s) {
            Ordering::Equal => Ok(*self),
            Ordering::Greater => {
                let f = pow10((scale - self.s) as u32)?;
                let m = self
                    .m
                    .checked_mul(f)
                    .ok_or_else(|| anyhow!("a number has more than {MAX_DIGITS} digits"))?;
                Ok(Dec { m, s: scale })
            }
            Ordering::Less => {
                let f = pow10((self.s - scale) as u32)?;
                let mut q = self.m / f;
                if half_adjust {
                    let r = (self.m % f).abs();
                    if r * 2 >= f {
                        q += if self.m < 0 { -1 } else { 1 };
                    }
                }
                Ok(Dec { m: q, s: scale })
            }
        }
    }

    fn aligned(a: &Dec, b: &Dec) -> Result<(i128, i128, u8)> {
        let s = a.s.max(b.s);
        Ok((a.rescale(s, false)?.m, b.rescale(s, false)?.m, s))
    }

    pub fn add(&self, o: &Dec) -> Result<Dec> {
        let (a, b, s) = Dec::aligned(self, o)?;
        Ok(Dec {
            m: a.checked_add(b).ok_or_else(overflow)?,
            s,
        })
    }

    pub fn sub(&self, o: &Dec) -> Result<Dec> {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Dec) -> Result<Dec> {
        match self.m.checked_mul(o.m) {
            Some(m) => Ok(Dec { m, s: self.s + o.s }.normalized()),
            None => {
                // Drop decimal places of the factors until it fits.
                let (a, b) = (self.normalized(), o.normalized());
                if (a.s, b.s) != (self.s, o.s) {
                    return a.mul(&b);
                }
                Err(overflow())
            }
        }
    }

    /// Division, with DIV_SCALE decimal places (truncated). Dividing by
    /// zero is an error (RPG: MCH1211).
    pub fn div(&self, o: &Dec) -> Result<Dec> {
        self.div_scale(o, DIV_SCALE)
    }

    pub fn div_scale(&self, o: &Dec, scale: u8) -> Result<Dec> {
        if o.m == 0 {
            bail!("MCH1211: Attempt made to divide by zero.");
        }
        // a/10^sa / (b/10^sb) = (a * 10^(scale + sb - sa)) / b / 10^scale
        let shift = scale as i32 + o.s as i32 - self.s as i32;
        let (num, den) = if shift >= 0 {
            (
                self.m
                    .checked_mul(pow10(shift as u32)?)
                    .ok_or_else(overflow)?,
                o.m,
            )
        } else {
            (
                self.m,
                o.m.checked_mul(pow10((-shift) as u32)?)
                    .ok_or_else(overflow)?,
            )
        };
        Ok(Dec {
            m: num / den,
            s: scale,
        })
    }

    /// The remainder of the integer division (RPG %REM, MVR): same sign
    /// as the dividend.
    pub fn rem(&self, o: &Dec) -> Result<Dec> {
        if o.m == 0 {
            bail!("MCH1211: Attempt made to divide by zero.");
        }
        let (a, b, s) = Dec::aligned(self, o)?;
        Ok(Dec { m: a % b, s })
    }

    /// Integer part (truncated toward zero).
    pub fn trunc(&self) -> Dec {
        Dec {
            m: self.rescale(0, false).map(|d| d.m).unwrap_or(0),
            s: 0,
        }
    }

    /// Trailing zero decimal places removed.
    pub fn normalized(&self) -> Dec {
        let mut d = *self;
        while d.s > 0 && d.m % 10 == 0 {
            d.m /= 10;
            d.s -= 1;
        }
        d
    }

    /// Digits before the decimal point (0 for a value below 1).
    pub fn int_digits(&self) -> u32 {
        let mut i = (self.m / 10i128.pow(self.s as u32)).unsigned_abs();
        let mut n = 0;
        while i > 0 {
            i /= 10;
            n += 1;
        }
        n
    }

    /// Does the value fit a field of `digits` digits with `decimals`
    /// decimal places, after truncating extra decimal places?
    pub fn fits(&self, digits: u32, decimals: u32) -> bool {
        self.int_digits() + decimals <= digits
    }

    /// Keep only the low-order digits that fit `digits` digits with
    /// `decimals` places: what the fixed-form arithmetic operations do
    /// (Z-ADD, ADD...: the high-order digits are lost, no error).
    pub fn fit_truncating(&self, digits: u32, decimals: u32, half_adjust: bool) -> Result<Dec> {
        let d = self.rescale(decimals as u8, half_adjust)?;
        let limit = pow10(digits)?;
        Ok(Dec {
            m: d.m % limit,
            s: d.s,
        })
    }

    /// Exact integer value, if there are no decimal places left.
    pub fn to_i128(&self) -> Result<i128> {
        let n = self.normalized();
        if n.s != 0 {
            bail!("{} is not a whole number", self);
        }
        Ok(n.m)
    }

    /// The absolute value's digits, `digits` of them with `decimals`
    /// decimal places, leading zeros kept (zoned/packed contents).
    pub fn digits(&self, digits: u32, decimals: u32) -> Result<String> {
        let d = self.rescale(decimals as u8, false)?;
        let s = d.m.unsigned_abs().to_string();
        if s.len() as u32 > digits {
            bail!("{self} does not fit {digits} digits");
        }
        Ok(format!("{:0>width$}", s, width = digits as usize))
    }
}

fn overflow() -> anyhow::Error {
    anyhow!("a number has more than {MAX_DIGITS} digits")
}

impl PartialOrd for Dec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Dec {
    fn cmp(&self, other: &Self) -> Ordering {
        match Dec::aligned(self, other) {
            Ok((a, b, _)) => a.cmp(&b),
            // Too many digits to align: compare signs, then lengths.
            Err(_) => self
                .m
                .signum()
                .cmp(&other.m.signum())
                .then(self.int_digits().cmp(&other.int_digits())),
        }
    }
}

/// Plain text: optional '-', digits, '.' and the decimal places
/// (`-1.50`, `0.05`, `12`).
impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.m.unsigned_abs().to_string();
        let s = self.s as usize;
        let sign = if self.m < 0 { "-" } else { "" };
        if s == 0 {
            return write!(f, "{sign}{digits}");
        }
        let padded = format!("{:0>width$}", digits, width = s + 1);
        let (i, d) = padded.split_at(padded.len() - s);
        write!(f, "{sign}{i}.{d}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Dec {
        Dec::parse(s).unwrap()
    }

    #[test]
    fn arithmetic_is_exact() {
        assert_eq!(d("0.1").add(&d("0.2")).unwrap(), d("0.3"));
        assert_eq!(d("12.99").mul(&d("3")).unwrap().to_string(), "38.97");
        assert_eq!(
            d("10")
                .div(&d("4"))
                .unwrap()
                .rescale(2, false)
                .unwrap()
                .to_string(),
            "2.50"
        );
        assert_eq!(d("-7").rem(&d("2")).unwrap().to_string(), "-1");
        assert!(d("1")
            .div(&d("0"))
            .unwrap_err()
            .to_string()
            .starts_with("MCH1211"));
    }

    #[test]
    fn truncation_and_half_adjust() {
        // ILE RPG reference: without (H) extra decimal places are
        // truncated; with (H) the value is rounded (half away from zero).
        assert_eq!(d("2.675").rescale(2, false).unwrap().to_string(), "2.67");
        assert_eq!(d("2.675").rescale(2, true).unwrap().to_string(), "2.68");
        assert_eq!(d("-2.675").rescale(2, true).unwrap().to_string(), "-2.68");
        // Fixed-form Z-ADD into a 3-digit field keeps the low-order digits.
        assert_eq!(
            d("12345").fit_truncating(3, 0, false).unwrap().to_string(),
            "345"
        );
        assert!(!d("1000").fits(3, 0));
        assert!(d("999.99").fits(5, 2));
    }

    #[test]
    fn text() {
        assert_eq!(d("-0.05").to_string(), "-0.05");
        assert_eq!(d("12-").to_string(), "-12");
        assert_eq!(d(" ").to_string(), "0");
        assert_eq!(d("42").digits(5, 2).unwrap(), "04200");
        assert!(Dec::parse("1x").is_err());
    }
}
