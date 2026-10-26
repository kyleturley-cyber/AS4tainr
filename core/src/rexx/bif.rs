//! REXX built-in functions (REXX/400 Reference, "Functions"). Strings
//! are text; functions on bytes (C2X, X2C, C2D, D2C, BITAND...) take them
//! as CCSID 37.

use super::{number, rounded, show, with_queue, Rx};
use anyhow::{anyhow, bail, Result};
use rust_decimal::prelude::*;

fn e(s: &str) -> Vec<u8> {
    crate::tn5250::ebcdic::encode(s)
}

fn a(b: &[u8]) -> String {
    crate::rpg::data::a(b)
}

const MONTHS: [&str; 12] = [
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
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// Days from 0001-01-01 (day 0), the proleptic Gregorian calendar.
fn base_days(y: i64, m: i64, d: i64) -> i64 {
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + d - 307
}

fn day_of_year(y: i64, m: i64, d: i64) -> i64 {
    base_days(y, m, d) - base_days(y, 1, 1) + 1
}

/// The built-in function `name`, or None when there is none of that name.
pub(super) fn call(
    rx: &mut Rx<'_, '_>,
    name: &str,
    args: &[Option<String>],
) -> Result<Option<String>> {
    let arg = |i: usize| -> Option<&str> { args.get(i).and_then(|a| a.as_deref()) };
    let req = |i: usize| -> Result<&str> {
        arg(i).ok_or_else(|| anyhow!("REX0040: {name}: argument {} is missing.", i + 1))
    };
    let whole = |i: usize| -> Result<Option<i64>> {
        match arg(i) {
            None => Ok(None),
            Some(v) => {
                let d = number(v).filter(|d| d.fract().is_zero()).ok_or_else(|| {
                    anyhow!(
                        "REX0040: {name}: argument {} '{v}' is not a whole number.",
                        i + 1
                    )
                })?;
                Ok(Some(d.to_i64().unwrap_or(i64::MAX)))
            }
        }
    };
    let pos_arg = |i: usize, dflt: i64| -> Result<usize> {
        let n = whole(i)?.unwrap_or(dflt);
        if n < 1 {
            bail!("REX0040: {name}: argument {} must be positive.", i + 1);
        }
        Ok(n as usize)
    };
    let len_arg = |i: usize| -> Result<Option<usize>> {
        match whole(i)? {
            Some(n) if n < 0 => bail!("REX0040: {name}: argument {} must not be negative.", i + 1),
            Some(n) => Ok(Some(n as usize)),
            None => Ok(None),
        }
    };
    let pad = |i: usize| -> Result<char> {
        match arg(i) {
            None => Ok(' '),
            Some(p) if p.chars().count() == 1 => Ok(p.chars().next().unwrap()),
            Some(p) => bail!("REX0040: {name}: the pad '{p}' is not one character."),
        }
    };
    let num = |i: usize| -> Result<Decimal> {
        let v = req(i)?;
        number(v).ok_or_else(|| anyhow!("REX0040: {name}: '{v}' is not a number."))
    };
    let chars = |s: &str| -> Vec<char> { s.chars().collect() };
    let tf = |b: bool| Some(if b { "1" } else { "0" }.to_string());
    let words = |s: &str| -> Vec<(usize, String)> {
        // (character position, word)
        let c = chars(s);
        let mut out = Vec::new();
        let mut i = 0;
        while i < c.len() {
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            if i >= c.len() {
                break;
            }
            let st = i;
            while i < c.len() && !c[i].is_whitespace() {
                i += 1;
            }
            out.push((st, c[st..i].iter().collect()));
        }
        out
    };
    let digits = rx.digits;
    Ok(match name {
        "ABBREV" => {
            let (info, ab) = (req(0)?, req(1)?);
            let min = len_arg(2)?.unwrap_or(ab.chars().count());
            tf(info.starts_with(ab) && ab.chars().count() >= min)
        }
        "ABS" => Some(show(rounded(num(0)?.abs(), digits))),
        "ADDRESS" => Some(rx.address.clone()),
        "ARG" => {
            let f = rx.frames.last().expect("a frame");
            let n = f
                .args
                .iter()
                .rposition(|a| a.is_some())
                .map_or(0, |i| i + 1);
            match whole(0)? {
                None => Some(n.to_string()),
                Some(i) if i < 1 => bail!("REX0040: ARG: argument 1 must be positive."),
                Some(i) => {
                    let v = f.args.get(i as usize - 1).cloned().flatten();
                    match arg(1).map(|o| o.to_ascii_uppercase()) {
                        None => Some(v.unwrap_or_default()),
                        Some(o) if o.starts_with('E') => tf(v.is_some()),
                        Some(o) if o.starts_with('O') => tf(v.is_none()),
                        Some(o) => bail!("REX0040: ARG: option {o} is not E or O."),
                    }
                }
            }
        }
        "B2X" => {
            let b: String = req(0)?.chars().filter(|c| !c.is_whitespace()).collect();
            if !b.chars().all(|c| c == '0' || c == '1') {
                bail!("REX0040: B2X: '{b}' is not binary.");
            }
            let pad = (4 - b.len() % 4) % 4;
            let b = format!("{}{b}", "0".repeat(pad));
            Some(
                (0..b.len())
                    .step_by(4)
                    .map(|i| format!("{:X}", u8::from_str_radix(&b[i..i + 4], 2).unwrap_or(0)))
                    .collect(),
            )
        }
        "X2B" => {
            let h: String = req(0)?.chars().filter(|c| !c.is_whitespace()).collect();
            let mut out = String::new();
            for c in h.chars() {
                let v = c
                    .to_digit(16)
                    .ok_or_else(|| anyhow!("REX0040: X2B: '{h}' is not hexadecimal."))?;
                out.push_str(&format!("{v:04b}"));
            }
            Some(out)
        }
        "C2X" => Some(e(req(0)?).iter().map(|b| format!("{b:02X}")).collect()),
        "X2C" => {
            let h: String = req(0)?.chars().filter(|c| !c.is_whitespace()).collect();
            let h = if h.len() % 2 == 1 { format!("0{h}") } else { h };
            let bytes = (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16))
                .collect::<std::result::Result<Vec<u8>, _>>()
                .map_err(|_| anyhow!("REX0040: X2C: '{h}' is not hexadecimal."))?;
            Some(a(&bytes))
        }
        "C2D" => {
            let b = e(req(0)?);
            let n = len_arg(1)?;
            let b: Vec<u8> = match n {
                Some(n) => {
                    let mut v = b.clone();
                    while v.len() < n {
                        v.insert(0, 0);
                    }
                    v[v.len() - n..].to_vec()
                }
                None => b,
            };
            if b.len() > 16 {
                bail!("REX0040: C2D: the string is too long.");
            }
            let mut v: i128 = 0;
            for x in &b {
                v = (v << 8) | *x as i128;
            }
            if n.is_some() && !b.is_empty() && b[0] & 0x80 != 0 {
                v -= 1i128 << (8 * b.len());
            }
            Some(v.to_string())
        }
        "D2C" | "D2X" => {
            let d = num(0)?;
            if !d.fract().is_zero() {
                bail!("REX0040: {name}: '{}' is not a whole number.", req(0)?);
            }
            let v = d.to_i128().unwrap_or(0);
            let n = len_arg(1)?;
            if v < 0 && n.is_none() {
                bail!("REX0040: {name}: a negative number needs a length.");
            }
            let bytes_needed = |v: i128| -> usize {
                let mut k = 1;
                while k < 16 && (v >> (8 * k)) != 0 {
                    k += 1;
                }
                k
            };
            if name == "D2C" {
                let k = n.unwrap_or_else(|| bytes_needed(v));
                let all = v.to_be_bytes();
                let mut b: Vec<u8> = all[16 - k.min(16)..].to_vec();
                while b.len() < k {
                    b.insert(0, if v < 0 { 0xFF } else { 0 });
                }
                Some(a(&b))
            } else {
                let hex = if v < 0 {
                    format!("{:032X}", v as u128)
                } else {
                    format!("{v:X}")
                };
                Some(match n {
                    Some(n) if hex.len() >= n => hex[hex.len() - n..].to_string(),
                    Some(n) => format!(
                        "{}{hex}",
                        if v < 0 { "F" } else { "0" }.repeat(n - hex.len())
                    ),
                    None => hex,
                })
            }
        }
        "X2D" => {
            let h: String = req(0)?.chars().filter(|c| !c.is_whitespace()).collect();
            let n = len_arg(1)?;
            let h = match n {
                Some(n) if h.len() > n => h[h.len() - n..].to_string(),
                Some(n) => format!("{}{h}", "0".repeat(n - h.len())),
                None => h,
            };
            if h.len() > 30 {
                bail!("REX0040: X2D: the string is too long.");
            }
            let mut v = if h.is_empty() {
                0
            } else {
                i128::from_str_radix(&h, 16)
                    .map_err(|_| anyhow!("REX0040: X2D: '{h}' is not hexadecimal."))?
            };
            if n.is_some_and(|n| n > 0)
                && h.chars()
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .is_some_and(|d| d >= 8)
            {
                v -= 1i128 << (4 * h.len());
            }
            Some(v.to_string())
        }
        "BITAND" | "BITOR" | "BITXOR" => {
            let x = e(req(0)?);
            let y = e(arg(1).unwrap_or(""));
            let p = arg(2).map(|p| e(p).first().copied().unwrap_or(0));
            let n = x.len().max(y.len());
            let mut out = Vec::new();
            for i in 0..n {
                let (bx, by) = (x.get(i).copied(), y.get(i).copied().or(p));
                out.push(match (bx, by) {
                    (Some(bx), Some(by)) => match name {
                        "BITAND" => bx & by,
                        "BITOR" => bx | by,
                        _ => bx ^ by,
                    },
                    (Some(b), None) | (None, Some(b)) => b,
                    (None, None) => 0,
                });
            }
            Some(a(&out))
        }
        "CENTER" | "CENTRE" => {
            let s = chars(req(0)?);
            let n =
                len_arg(1)?.ok_or_else(|| anyhow!("REX0040: {name}: the length is missing."))?;
            let p = pad(2)?;
            if s.len() >= n {
                let drop = s.len() - n;
                Some(s[drop / 2..drop / 2 + n].iter().collect())
            } else {
                let add = n - s.len();
                let l = add / 2;
                Some(format!(
                    "{}{}{}",
                    p.to_string().repeat(l),
                    s.iter().collect::<String>(),
                    p.to_string().repeat(add - l)
                ))
            }
        }
        "COMPARE" => {
            let (x, y) = (chars(req(0)?), chars(req(1)?));
            let p = pad(2)?;
            let n = x.len().max(y.len());
            let at = (0..n).find(|&i| x.get(i).unwrap_or(&p) != y.get(i).unwrap_or(&p));
            Some(at.map_or(0, |i| i + 1).to_string())
        }
        "CONDITION" => {
            let c = rx.condition.clone().unwrap_or_default();
            let o = arg(0)
                .map(|o| o.to_ascii_uppercase())
                .unwrap_or_else(|| "I".into());
            Some(match o.chars().next().unwrap_or('I') {
                'C' => c.name,
                'D' => c.desc,
                'I' => c.instruction,
                'S' => {
                    if c.name.is_empty() {
                        String::new()
                    } else if rx.trap(&c.name).is_some() {
                        "ON".into()
                    } else {
                        "OFF".into()
                    }
                }
                _ => bail!("REX0040: CONDITION: option {o} is not C, D, I or S."),
            })
        }
        "COPIES" => {
            let n = len_arg(1)?.ok_or_else(|| anyhow!("REX0040: COPIES: the count is missing."))?;
            Some(req(0)?.repeat(n))
        }
        "DATATYPE" => {
            let s = req(0)?;
            match arg(1) {
                None => Some(if number(s).is_some() { "NUM" } else { "CHAR" }.into()),
                Some(t) => {
                    let t = t.to_ascii_uppercase().chars().next().unwrap_or(' ');
                    let all = |f: &dyn Fn(char) -> bool| !s.is_empty() && s.chars().all(f);
                    tf(match t {
                        'A' => all(&|c| c.is_ascii_alphanumeric()),
                        'B' => all(&|c| c == '0' || c == '1' || c == ' '),
                        'L' => all(&|c| c.is_ascii_lowercase()),
                        'M' => all(&|c| c.is_ascii_alphabetic()),
                        'N' => number(s).is_some(),
                        'S' => {
                            !s.is_empty()
                                && s.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || "._!?@#$".contains(c))
                        }
                        'U' => all(&|c| c.is_ascii_uppercase()),
                        'W' => number(s).is_some_and(|d| d.fract().is_zero()),
                        'X' => s.chars().all(|c| c.is_ascii_hexdigit() || c == ' '),
                        o => bail!("REX0040: DATATYPE: type {o} is not known."),
                    })
                }
            }
        }
        "DATE" => {
            let ((y, mo, d), _, _) = crate::rpg::dates::now();
            let (y, mo, d) = (y as i64, mo as i64, d as i64);
            let o = arg(0)
                .map(|o| o.to_ascii_uppercase())
                .unwrap_or_else(|| "N".into());
            let b = base_days(y, mo, d);
            Some(match o.chars().next().unwrap_or('N') {
                'N' => format!("{d} {} {y}", &MONTHS[mo as usize - 1][..3]),
                'S' => format!("{y:04}{mo:02}{d:02}"),
                'U' => format!("{mo:02}/{d:02}/{:02}", y % 100),
                'E' => format!("{d:02}/{mo:02}/{:02}", y % 100),
                'O' => format!("{:02}/{mo:02}/{d:02}", y % 100),
                'J' => format!("{:02}{:03}", y % 100, day_of_year(y, mo, d)),
                'D' => day_of_year(y, mo, d).to_string(),
                'M' => MONTHS[mo as usize - 1].to_string(),
                'W' => DAYS[(b % 7) as usize].to_string(),
                'B' => b.to_string(),
                _ => bail!("REX0040: DATE: option {o} is not known."),
            })
        }
        "TIME" => {
            let (_, (h, mi, s), us) = crate::rpg::dates::now();
            let (h, mi, s) = (h as i64, mi as i64, s as i64);
            let o = arg(0)
                .map(|o| o.to_ascii_uppercase())
                .unwrap_or_else(|| "N".into());
            Some(match o.chars().next().unwrap_or('N') {
                'N' => format!("{h:02}:{mi:02}:{s:02}"),
                'L' => format!("{h:02}:{mi:02}:{s:02}.{us:06}"),
                'H' => h.to_string(),
                'M' => (h * 60 + mi).to_string(),
                'S' => (h * 3600 + mi * 60 + s).to_string(),
                'C' => format!(
                    "{}:{mi:02}{}",
                    if h % 12 == 0 { 12 } else { h % 12 },
                    if h < 12 { "am" } else { "pm" }
                ),
                _ => bail!("REX0040: TIME: option {o} is not available here."),
            })
        }
        "DELSTR" => {
            let s = chars(req(0)?);
            let n = pos_arg(1, 1)?;
            let len = len_arg(2)?;
            let st = (n - 1).min(s.len());
            let en = len.map_or(s.len(), |l| (st + l).min(s.len()));
            Some(s[..st].iter().chain(&s[en..]).collect())
        }
        "DELWORD" => {
            let s = req(0)?;
            let n = pos_arg(1, 1)?;
            let len = len_arg(2)?;
            let w = words(s);
            let c = chars(s);
            if n > w.len() {
                Some(s.to_string())
            } else {
                let st = w[n - 1].0;
                let last = len.map_or(w.len(), |l| (n - 1 + l).min(w.len()));
                let en = if last >= w.len() { c.len() } else { w[last].0 };
                Some(c[..st].iter().chain(&c[en..]).collect())
            }
        }
        "DIGITS" => Some(digits.to_string()),
        "FUZZ" => Some("0".into()),
        "FORM" => Some("SCIENTIFIC".into()),
        "ERRORTEXT" => Some(format!("REXX error {}", req(0)?)),
        "FORMAT" => {
            let d = num(0)?;
            let d = rounded(d, digits);
            let after = whole(2)?;
            let d = match after {
                Some(a) => d.round_dp_with_strategy(
                    a.max(0) as u32,
                    RoundingStrategy::MidpointAwayFromZero,
                ),
                None => d,
            };
            let mut t = show(d);
            if let Some(a) = after {
                let a = a.max(0) as usize;
                let cur = t.find('.').map_or(0, |i| t.len() - i - 1);
                if a > 0 && cur == 0 {
                    t.push('.');
                }
                t.push_str(&"0".repeat(a.saturating_sub(cur)));
            }
            if let Some(b) = whole(1)? {
                let int_len = t.find('.').unwrap_or(t.len());
                let b = b.max(0) as usize;
                if int_len > b {
                    bail!(
                        "REX0040: FORMAT: {} needs more than {b} places before the point.",
                        req(0)?
                    );
                }
                t = format!("{}{t}", " ".repeat(b - int_len));
            }
            Some(t)
        }
        "INSERT" | "OVERLAY" => {
            let new = chars(req(0)?);
            let target = chars(req(1)?);
            let p = pad(4)?;
            let n = if name == "INSERT" {
                len_arg(2)?.unwrap_or(0)
            } else {
                pos_arg(2, 1)? - 1
            };
            let len = len_arg(3)?.unwrap_or(new.len());
            let mut piece: Vec<char> = new.iter().copied().take(len).collect();
            while piece.len() < len {
                piece.push(p);
            }
            let mut t = target.clone();
            while t.len() < n {
                t.push(p);
            }
            if name == "INSERT" {
                t.splice(n..n, piece);
            } else {
                let end = (n + len).min(t.len());
                t.splice(n..end, piece);
            }
            Some(t.into_iter().collect())
        }
        "LASTPOS" | "POS" | "INDEX" => {
            let (needle, hay) = if name == "INDEX" {
                (chars(req(1)?), chars(req(0)?))
            } else {
                (chars(req(0)?), chars(req(1)?))
            };
            if needle.is_empty() || needle.len() > hay.len() {
                return Ok(Some("0".into()));
            }
            let found = if name == "LASTPOS" {
                let start = whole(2)?
                    .map_or(hay.len(), |s| s.max(1) as usize)
                    .min(hay.len());
                let last = start.saturating_sub(needle.len());
                (0..=last).rev().find(|&i| {
                    i + needle.len() <= hay.len() && hay[i..i + needle.len()] == needle[..]
                })
            } else {
                let start = pos_arg(2, 1)? - 1;
                (start..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
            };
            Some(found.map_or(0, |i| i + 1).to_string())
        }
        "LEFT" | "RIGHT" => {
            let s = chars(req(0)?);
            let n =
                len_arg(1)?.ok_or_else(|| anyhow!("REX0040: {name}: the length is missing."))?;
            let p = pad(2)?;
            Some(if name == "LEFT" {
                let mut v: Vec<char> = s.iter().copied().take(n).collect();
                while v.len() < n {
                    v.push(p);
                }
                v.into_iter().collect()
            } else if s.len() >= n {
                s[s.len() - n..].iter().collect()
            } else {
                format!(
                    "{}{}",
                    p.to_string().repeat(n - s.len()),
                    s.iter().collect::<String>()
                )
            })
        }
        "LENGTH" => Some(req(0)?.chars().count().to_string()),
        "MAX" | "MIN" => {
            let mut best: Option<Decimal> = None;
            for (i, v) in args.iter().enumerate() {
                let v = v
                    .as_deref()
                    .ok_or_else(|| anyhow!("REX0040: {name}: argument {} is missing.", i + 1))?;
                let d =
                    number(v).ok_or_else(|| anyhow!("REX0040: {name}: '{v}' is not a number."))?;
                best = Some(match best {
                    None => d,
                    Some(b) if (name == "MAX") == (d > b) => d,
                    Some(b) => b,
                });
            }
            Some(show(rounded(
                best.ok_or_else(|| anyhow!("REX0040: {name} needs a number."))?,
                digits,
            )))
        }
        "QUEUED" => Some(with_queue(&rx.queue_key, |q| q.len()).to_string()),
        "RANDOM" => {
            let lo = whole(0)?.unwrap_or(0);
            let hi = whole(1)?.unwrap_or(if args.len() == 1 && arg(0).is_some() {
                lo
            } else {
                999
            });
            let (lo, hi) = if args.len() == 1 && arg(0).is_some() {
                (0, lo)
            } else {
                (lo, hi)
            };
            if hi < lo {
                bail!("REX0040: RANDOM: the maximum is less than the minimum.");
            }
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0) as i64;
            Some((lo + t.rem_euclid(hi - lo + 1)).to_string())
        }
        "REVERSE" => Some(req(0)?.chars().rev().collect()),
        "SIGN" => {
            let d = num(0)?;
            Some(
                if d.is_zero() {
                    "0"
                } else if d.is_sign_negative() {
                    "-1"
                } else {
                    "1"
                }
                .into(),
            )
        }
        "SOURCELINE" => match whole(0)? {
            None => Some(rx.prog.source.len().to_string()),
            Some(n) if n >= 1 && (n as usize) <= rx.prog.source.len() => {
                Some(rx.prog.source[n as usize - 1].clone())
            }
            Some(n) => bail!("REX0040: SOURCELINE: there is no line {n}."),
        },
        "SPACE" => {
            let n = len_arg(1)?.unwrap_or(1);
            let p = pad(2)?;
            let w: Vec<String> = words(req(0)?).into_iter().map(|(_, w)| w).collect();
            Some(w.join(&p.to_string().repeat(n)))
        }
        "STRIP" => {
            let s = req(0)?;
            let o = arg(1)
                .map(|o| o.to_ascii_uppercase())
                .unwrap_or_else(|| "B".into());
            let c = match arg(2) {
                None => ' ',
                Some(x) if x.chars().count() == 1 => x.chars().next().unwrap(),
                Some(x) => bail!("REX0040: STRIP: '{x}' is not one character."),
            };
            Some(match o.chars().next().unwrap_or('B') {
                'B' => s.trim_matches(c).to_string(),
                'L' => s.trim_start_matches(c).to_string(),
                'T' => s.trim_end_matches(c).to_string(),
                _ => bail!("REX0040: STRIP: option {o} is not B, L or T."),
            })
        }
        "SUBSTR" => {
            let s = chars(req(0)?);
            let n = pos_arg(1, 1)?;
            let p = pad(3)?;
            let len = len_arg(2)?.unwrap_or_else(|| s.len().saturating_sub(n - 1));
            let mut out: Vec<char> = s.iter().copied().skip(n - 1).take(len).collect();
            while out.len() < len {
                out.push(p);
            }
            Some(out.into_iter().collect())
        }
        "SUBWORD" => {
            let s = req(0)?;
            let n = pos_arg(1, 1)?;
            let len = len_arg(2)?;
            let w = words(s);
            let c = chars(s);
            if n > w.len() || len == Some(0) {
                Some(String::new())
            } else {
                let last = len.map_or(w.len(), |l| (n - 1 + l).min(w.len()));
                let (st, (lst, lw)) = (w[n - 1].0, &w[last - 1]);
                Some(c[st..lst + lw.chars().count()].iter().collect())
            }
        }
        "WORD" => {
            let n = pos_arg(1, 1)?;
            Some(
                words(req(0)?)
                    .get(n - 1)
                    .map(|w| w.1.clone())
                    .unwrap_or_default(),
            )
        }
        "WORDINDEX" => {
            let n = pos_arg(1, 1)?;
            Some(words(req(0)?).get(n - 1).map_or(0, |w| w.0 + 1).to_string())
        }
        "WORDLENGTH" => {
            let n = pos_arg(1, 1)?;
            Some(
                words(req(0)?)
                    .get(n - 1)
                    .map_or(0, |w| w.1.chars().count())
                    .to_string(),
            )
        }
        "WORDS" => Some(words(req(0)?).len().to_string()),
        "WORDPOS" => {
            let phrase: Vec<String> = words(req(0)?).into_iter().map(|(_, w)| w).collect();
            let w: Vec<String> = words(req(1)?).into_iter().map(|(_, w)| w).collect();
            let start = pos_arg(2, 1)?;
            if phrase.is_empty() || phrase.len() > w.len() {
                return Ok(Some("0".into()));
            }
            let at = (start - 1..=w.len() - phrase.len())
                .find(|&i| w[i..i + phrase.len()] == phrase[..]);
            Some(at.map_or(0, |i| i + 1).to_string())
        }
        "SYMBOL" => {
            let s = req(0)?.to_ascii_uppercase();
            let valid = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._!?@#$".contains(c));
            Some(
                if !valid {
                    "BAD"
                } else if s.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
                    "LIT"
                } else if rx.get(&s).is_some() {
                    "VAR"
                } else {
                    "LIT"
                }
                .into(),
            )
        }
        "TRANSLATE" => {
            let s = req(0)?;
            if args.len() <= 1 {
                return Ok(Some(s.to_ascii_uppercase()));
            }
            let out = chars(arg(1).unwrap_or(""));
            let input: Vec<char> = match arg(2) {
                Some(i) => chars(i),
                None => (0u8..=255)
                    .map(|b| crate::rpg::data::a(&[b]).chars().next().unwrap_or('\0'))
                    .collect(),
            };
            let p = pad(3)?;
            Some(
                s.chars()
                    .map(|c| match input.iter().position(|&x| x == c) {
                        Some(i) => *out.get(i).unwrap_or(&p),
                        None => c,
                    })
                    .collect(),
            )
        }
        "TRUNC" => {
            let d = rounded(num(0)?, digits);
            let n = whole(1)?.unwrap_or(0).max(0) as u32;
            let t = d.trunc_with_scale(n);
            let mut s = show(t);
            let cur = s.find('.').map_or(0, |i| s.len() - i - 1);
            if n > 0 && cur == 0 {
                s.push('.');
            }
            s.push_str(&"0".repeat((n as usize).saturating_sub(cur)));
            Some(s)
        }
        "VALUE" => {
            let n = req(0)?.to_ascii_uppercase();
            let old = rx.get(&n).unwrap_or_else(|| rx.derived(&n));
            if let Some(v) = arg(1) {
                rx.set(&n, v.to_string());
            }
            Some(old)
        }
        "VERIFY" => {
            let s = chars(req(0)?);
            let r = chars(req(1)?);
            let m = arg(2)
                .map(|o| o.to_ascii_uppercase())
                .unwrap_or_else(|| "N".into());
            let start = pos_arg(3, 1)?;
            let want_match = m.starts_with('M');
            let at = (start - 1..s.len()).find(|&i| r.contains(&s[i]) == want_match);
            Some(at.map_or(0, |i| i + 1).to_string())
        }
        "USERID" => Some(rx.job.user.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // REXX/400 Reference, DATE('B'): 1 January 0001 is day 0; a Monday.
    #[test]
    fn base_dates() {
        assert_eq!(base_days(1, 1, 1), 0);
        assert_eq!(base_days(1970, 1, 1), 719162);
        // 14 October 2026 is a Wednesday.
        assert_eq!(DAYS[(base_days(2026, 10, 14) % 7) as usize], "Wednesday");
        assert_eq!(day_of_year(2024, 12, 31), 366);
    }
}
