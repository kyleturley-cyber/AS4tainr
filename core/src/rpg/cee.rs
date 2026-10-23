//! ILE CEE bindable APIs a program can call through prototypes
//! (EXTPROC('CEEDAYS') and so on): the date and time APIs (CEEDAYS,
//! CEEDATE, CEEDYWK, CEELOCT, CEESECS, CEEDATM, CEEISEC, CEESECI) and the
//! heap storage APIs (CEECRHP, CEEGTST, CEEFRST, CEECZST, CEEDSHP),
//! CEEDOD (an argument's length from its operational descriptor), the
//! bindable national language API QlgConvertCase, and what a program
//! needs to call a service program's procedure it knows by name only:
//! rslvsp (a system pointer to an object), QleActBndPgm (activate a
//! service program) and QleGetExp (a procedure pointer to its export).
//!
//! Written from IBM's ILE CEE API pages ("Convert Date to Lilian Format
//! (CEEDAYS)", "Convert Lilian Date to Character Format (CEEDATE)" and
//! the others). A Lilian date is the number of days since 14 October 1582
//! (16 May 1988 is 148138), Lilian seconds the seconds since 00:00:00 on
//! that day. The feedback code (fc), when passed, gets a 12-byte
//! condition token: all zeros (CEE0000) or the condition; when it is
//! omitted a failure is an escape message (the condition signaled).

use super::data::{self, a, DType, Fig, Fit, Value};
use super::{Arg, Machine};
use crate::rpg::dates::{self, Ymd};
use crate::rpg::dec::Dec;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::Mutex;

/// The CEE APIs as4tainr provides (system procedures: bound without a
/// binding directory, like the C runtime's).
pub const CEE_APIS: &[&str] = &[
    "CEEDAYS",
    "CEEDATE",
    "CEEDYWK",
    "CEELOCT",
    "CEESECS",
    "CEEDATM",
    "CEEISEC",
    "CEESECI",
    "CEECRHP",
    "CEEGTST",
    "CEEFRST",
    "CEECZST",
    "CEEDSHP",
    "CEEDOD",
    "CEEUTCO",
    "CEEUTC",
    "CEEGMT",
    "QLGCONVERTCASE",
    "RSLVSP",
    "QLEACTBNDPGM",
    "QLEGETEXP",
];

/// Service programs activated with QleActBndPgm: activation mark - 1 →
/// (library, name).
static ACTIVATED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Heaps made with CEECRHP: heap ID → its blocks (heap block numbers).
type Heaps = (i32, HashMap<i32, Vec<usize>>);
static HEAPS: Mutex<Option<Heaps>> = Mutex::new(None);

/// A date, a time and microseconds read from a picture string.
type Parsed = (Ymd, (u32, u32, u32), u32);

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
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// The days of 14 October 1582 in `dates::days` terms: Lilian day 0.
fn lilian_zero() -> i64 {
    dates::days((1582, 10, 14))
}

pub fn lilian(d: Ymd) -> i64 {
    dates::days(d) - lilian_zero()
}

pub fn from_lilian(n: i64) -> Ymd {
    dates::from_days(n + lilian_zero())
}

/// A failure: the CEE message ID and our own text.
struct Cond(u16, &'static str);

/// One element of a picture string.
#[derive(Debug, Clone, PartialEq)]
enum Pic {
    Year4,
    Year2,
    Month,
    MonthZ,
    Day,
    DayZ,
    DayOfYear,
    /// A month name: (upper case, length 3 or 0 for the full name, padded
    /// to 10 unless z).
    MonthName(bool, usize, bool),
    WeekdayName(bool, usize, bool),
    Hour,
    HourZ,
    Minute,
    Second,
    /// Fractions of a second: n digits.
    Fraction(usize),
    AmPm,
    Lit(char),
}

/// The elements of a picture string (CEEDATE's examples: YYYY, YY, MM,
/// ZM, DD, ZD, DDD, MMM, Mmm, Mmmmmmmmmm, Mmmmmmmmmz, WWW, Www,
/// Wwwwwwwwww, Wwwwwwwwwz, HH, ZH, MI, SS, 9..., AP).
fn picture(p: &str) -> Vec<Pic> {
    let cs: Vec<char> = p.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let at = |i: usize, s: &str| -> bool {
        let n = s.chars().count();
        i + n <= cs.len() && cs[i..i + n].iter().copied().eq(s.chars())
    };
    while i < cs.len() {
        // Names: M or W then m/w letters, ending in z or not.
        if (cs[i] == 'M' || cs[i] == 'W') && i + 1 < cs.len() {
            let lower = cs[i].to_ascii_lowercase();
            let upper_c = cs[i];
            let mut j = i + 1;
            while j < cs.len() && (cs[j] == lower || cs[j] == upper_c) {
                j += 1;
            }
            let z = j < cs.len() && (cs[j] == 'z' || cs[j] == 'Z') && j - i == 9;
            let len = j - i + usize::from(z);
            let upper = cs[i + 1] == upper_c;
            if len >= 3 && !(upper_c == 'M' && cs[i + 1] == 'I') && (len == 3 || len == 10) {
                let full = len == 10;
                let p = if upper_c == 'M' {
                    Pic::MonthName(upper, if full { 0 } else { 3 }, z)
                } else {
                    Pic::WeekdayName(upper, if full { 0 } else { 3 }, z)
                };
                out.push(p);
                i += len;
                continue;
            }
        }
        let (p, n) = if at(i, "YYYY") {
            (Pic::Year4, 4)
        } else if at(i, "YY") {
            (Pic::Year2, 2)
        } else if at(i, "ZM") {
            (Pic::MonthZ, 2)
        } else if at(i, "ZD") {
            (Pic::DayZ, 2)
        } else if at(i, "ZH") {
            (Pic::HourZ, 2)
        } else if at(i, "DDD") {
            (Pic::DayOfYear, 3)
        } else if at(i, "DD") {
            (Pic::Day, 2)
        } else if at(i, "MM") {
            (Pic::Month, 2)
        } else if at(i, "MI") {
            (Pic::Minute, 2)
        } else if at(i, "HH") {
            (Pic::Hour, 2)
        } else if at(i, "SS") {
            (Pic::Second, 2)
        } else if at(i, "AP") {
            (Pic::AmPm, 2)
        } else if cs[i] == '9' {
            let n = cs[i..].iter().take_while(|&&c| c == '9').count();
            (Pic::Fraction(n), n)
        } else {
            (Pic::Lit(cs[i]), 1)
        };
        out.push(p);
        i += n;
    }
    out
}

fn name(n: &str, upper: bool, len: usize, z: bool) -> String {
    let s: String = if len == 3 {
        n.chars().take(3).collect()
    } else {
        n.to_string()
    };
    let s = if upper { s.to_ascii_uppercase() } else { s };
    if len == 0 && !z {
        format!("{s:<10}")
    } else {
        s
    }
}

/// A date and time in a picture string.
fn format(pic: &str, d: Ymd, t: (u32, u32, u32), us: u32) -> String {
    let mut out = String::new();
    let doy = lilian(d) - lilian((d.0, 1, 1)) + 1;
    let dow = (lilian(d) + 4).rem_euclid(7) as usize;
    for p in picture(pic) {
        match p {
            Pic::Year4 => out.push_str(&format!("{:04}", d.0)),
            Pic::Year2 => out.push_str(&format!("{:02}", d.0 % 100)),
            Pic::Month => out.push_str(&format!("{:02}", d.1)),
            Pic::MonthZ => out.push_str(&d.1.to_string()),
            Pic::Day => out.push_str(&format!("{:02}", d.2)),
            Pic::DayZ => out.push_str(&d.2.to_string()),
            Pic::DayOfYear => out.push_str(&format!("{doy:03}")),
            Pic::MonthName(u, l, z) => out.push_str(&name(MONTHS[d.1 as usize - 1], u, l, z)),
            Pic::WeekdayName(u, l, z) => out.push_str(&name(DAYS[dow], u, l, z)),
            Pic::Hour => out.push_str(&format!("{:02}", t.0)),
            Pic::HourZ => out.push_str(&t.0.to_string()),
            Pic::Minute => out.push_str(&format!("{:02}", t.1)),
            Pic::Second => out.push_str(&format!("{:02}", t.2)),
            Pic::Fraction(n) => {
                let f = format!("{us:06}000000");
                out.push_str(&f[..n.min(12)]);
            }
            Pic::AmPm => out.push_str(if t.0 < 12 { "AM" } else { "PM" }),
            Pic::Lit(c) => out.push(c),
        }
    }
    out
}

/// A two-digit year in the century window: the 100 years starting 80
/// years before this one (the century start value's default, CEESCEN).
fn window(yy: i32) -> i32 {
    let this = dates::now().0 .0;
    let start = this - 80;
    let mut y = start - start.rem_euclid(100) + yy;
    if y < start {
        y += 100;
    }
    y
}

/// Read a date (and time) written in a picture string (CEEDAYS, CEESECS):
/// numbers take their width, or up to the next separator when the input
/// has it ("6/2/88" with MM/DD/YY).
fn parse(input: &str, pic: &str) -> std::result::Result<Parsed, Cond> {
    let ps = picture(pic);
    if ps.is_empty() {
        return Err(Cond(2518, "the picture string is not valid"));
    }
    let cs: Vec<char> = input.trim_end().chars().collect();
    let mut i = 0;
    let (mut y, mut m, mut d, mut doy) = (None, None, None, None);
    let (mut h, mut mi, mut s, mut us, mut pm) = (0u32, 0u32, 0u32, 0u32, None);
    for (k, p) in ps.iter().enumerate() {
        let next_lit = match ps.get(k + 1) {
            Some(Pic::Lit(c)) => Some(*c),
            _ => None,
        };
        let mut num = |w: usize, flexible: bool| -> std::result::Result<u32, Cond> {
            let mut j = i;
            while j < cs.len() && j - i < w && cs[j].is_ascii_digit() {
                j += 1;
            }
            let short_ok = flexible || next_lit.is_some_and(|c| cs.get(j) == Some(&c));
            if j - i < w && !(short_ok && j > i) {
                return Err(Cond(
                    2507,
                    "the date has too few digits for its picture string",
                ));
            }
            let v: u32 = cs[i..j].iter().collect::<String>().parse().unwrap_or(0);
            i = j;
            Ok(v)
        };
        match p {
            Pic::Year4 => y = Some(num(4, false)? as i32),
            Pic::Year2 => y = Some(window(num(2, false)? as i32)),
            Pic::Month => m = Some(num(2, false)?),
            Pic::MonthZ => m = Some(num(2, true)?),
            Pic::Day => d = Some(num(2, false)?),
            Pic::DayZ => d = Some(num(2, true)?),
            Pic::DayOfYear => doy = Some(num(3, false)?),
            Pic::Hour => h = num(2, false)?,
            Pic::HourZ => h = num(2, true)?,
            Pic::Minute => mi = num(2, false)?,
            Pic::Second => s = num(2, false)?,
            Pic::Fraction(n) => {
                let v = num(*n, false)? as u64;
                let n = *n as u32;
                us = if n <= 6 {
                    v * 10u64.pow(6 - n)
                } else {
                    v / 10u64.pow(n - 6)
                } as u32;
            }
            Pic::AmPm => {
                let t: String = cs
                    .get(i..i + 2)
                    .map(|x| x.iter().collect())
                    .unwrap_or_default();
                pm = Some(t.eq_ignore_ascii_case("PM"));
                i += 2;
            }
            Pic::MonthName(_, len, _) => {
                let rest: String = cs[i..].iter().collect::<String>().to_ascii_uppercase();
                let k = MONTHS
                    .iter()
                    .position(|n| rest.starts_with(&n.to_ascii_uppercase()[..3]))
                    .ok_or(Cond(2517, "the month is not valid"))?;
                m = Some(k as u32 + 1);
                let full = MONTHS[k].len();
                i += if *len == 3 || !rest.starts_with(&MONTHS[k].to_ascii_uppercase()) {
                    3
                } else {
                    full
                };
                while *len == 0 && i < cs.len() && cs[i] == ' ' {
                    i += 1;
                }
            }
            Pic::WeekdayName(_, len, _) => {
                let rest: String = cs[i..].iter().collect::<String>().to_ascii_uppercase();
                let k = DAYS
                    .iter()
                    .position(|n| rest.starts_with(&n.to_ascii_uppercase()[..3]))
                    .ok_or(Cond(2508, "the day is not valid"))?;
                i += if *len == 3 || !rest.starts_with(&DAYS[k].to_ascii_uppercase()) {
                    3
                } else {
                    DAYS[k].len()
                };
            }
            Pic::Lit(c) => {
                if cs.get(i) == Some(c) {
                    i += 1;
                } else if cs.get(i).is_some_and(|x| !x.is_ascii_alphanumeric()) {
                    // Another separator is taken for this one.
                    i += 1;
                }
            }
        }
    }
    if let Some(true) = pm {
        if h < 12 {
            h += 12;
        }
    } else if let Some(false) = pm {
        if h == 12 {
            h = 0;
        }
    }
    let y = y.ok_or(Cond(2507, "the picture string has no year"))?;
    if !(1582..=9999).contains(&y) {
        return Err(Cond(2521, "the year is not valid"));
    }
    let date = match doy {
        Some(n) => {
            let last = if dates::leap(y) { 366 } else { 365 };
            if !(1..=last).contains(&n) {
                return Err(Cond(2508, "the day is not valid"));
            }
            dates::from_days(dates::days((y, 1, 1)) + n as i64 - 1)
        }
        None => {
            let m = m.unwrap_or(1);
            if !(1..=12).contains(&m) {
                return Err(Cond(2517, "the month is not valid"));
            }
            let d = d.unwrap_or(1);
            if d < 1 || d > dates::month_days(y, m) {
                return Err(Cond(2508, "the day is not valid"));
            }
            (y, m, d)
        }
    };
    if lilian(date) < 1 {
        return Err(Cond(2513, "the date is before 15 October 1582"));
    }
    if h > 24 || mi > 59 || s > 59 {
        return Err(Cond(2507, "the time is not valid"));
    }
    Ok((date, (h, mi, s), us))
}

/// The 12-byte condition token: CEE0000 is all zeros; otherwise severity
/// 3 and the message number, case 1, the facility CEE (as4tainr fills
/// the control bits and instance information with zeros).
fn token(c: &Option<Cond>) -> Vec<u8> {
    let Some(Cond(n, _)) = c else {
        return vec![0; 12];
    };
    let mut t = vec![0u8; 12];
    t[0..2].copy_from_slice(&3u16.to_be_bytes());
    t[2..4].copy_from_slice(&n.to_be_bytes());
    t[4] = 0b0101_1000;
    t[5..8].copy_from_slice(&data::e("CEE"));
    t
}

impl<'a> Machine<'a> {
    fn cee_value(&self, arg: Option<&Arg>) -> Result<Option<Value>> {
        Ok(match arg {
            Some(Arg::Val(v)) => Some(v.clone()),
            Some(Arg::Ref(l)) => Some(data::load(
                &l.dtype,
                &self.bufs[l.buf][l.off..l.off + l.size],
            )?),
            _ => None,
        })
    }

    fn cee_text(&self, arg: Option<&Arg>) -> Result<String> {
        Ok(match self.cee_value(arg)? {
            Some(Value::Char(c)) => a(&c),
            Some(v @ Value::Num(_)) => v.num()?.to_string(),
            Some(_) | None => String::new(),
        })
    }

    fn cee_num(&self, arg: Option<&Arg>) -> Result<Dec> {
        match self.cee_value(arg)? {
            Some(v) => v.num(),
            None => Ok(Dec::from_i64(0)),
        }
    }

    fn cee_set(&mut self, arg: Option<&Arg>, v: Value) -> Result<()> {
        if let Some(Arg::Ref(l)) = arg {
            let l = *l;
            let v = match (&l.dtype, v) {
                // Character output: cut or padded to the variable.
                (DType::Char(n), Value::Char(mut c)) => {
                    c.resize(*n as usize, 0x40);
                    Value::Char(c)
                }
                (_, v) => v,
            };
            data::store(
                &l.dtype,
                &v,
                &mut self.bufs[l.buf][l.off..l.off + l.size],
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    /// Finish a CEE call: the condition into fc when it was passed, or
    /// signaled.
    fn cee_end(&mut self, api: &str, fc: Option<&Arg>, c: Option<Cond>) -> Result<Value> {
        match fc {
            Some(Arg::Ref(l)) => {
                let l = *l;
                let t = token(&c);
                let n = l.size.min(12);
                self.bufs[l.buf][l.off..l.off + n].copy_from_slice(&t[..n]);
            }
            _ => {
                if let Some(Cond(n, text)) = c {
                    bail!("CEE{n:04}: {api}: {text}.");
                }
            }
        }
        Ok(Value::Fig(Fig::Null))
    }

    /// A CEE API. None when `name` is not one.
    pub(super) fn cee_call(&mut self, name: &str, args: &[Arg]) -> Option<Result<Value>> {
        if !CEE_APIS.contains(&name) {
            return None;
        }
        Some(self.cee_call_inner(name, args))
    }

    fn cee_call_inner(&mut self, name: &str, args: &[Arg]) -> Result<Value> {
        let g = |i: usize| args.get(i);
        let char_out = |s: &str| Value::Char(data::e(s));
        match name {
            // CEEDAYS(date, picture, Lilian date, fc).
            "CEEDAYS" => {
                let input = self.cee_text(g(0))?;
                let pic = self.cee_text(g(1))?;
                let pic = if pic.trim().is_empty() {
                    "YYYY-MM-DD".to_string()
                } else {
                    pic.trim_end().to_string()
                };
                match parse(&input, &pic) {
                    Ok((d, _, _)) => {
                        self.cee_set(g(2), Value::Num(Dec::from_i64(lilian(d))))?;
                        self.cee_end(name, g(3), None)
                    }
                    Err(c) => {
                        self.cee_set(g(2), Value::Num(Dec::from_i64(0)))?;
                        self.cee_end(name, g(3), Some(c))
                    }
                }
            }
            // CEEDATE(Lilian date, picture, character date, fc).
            "CEEDATE" => {
                let n = self.cee_num(g(0))?.to_i128()? as i64;
                let pic = self.cee_text(g(1))?;
                let pic = if pic.trim().is_empty() {
                    "YYYY-MM-DD".to_string()
                } else {
                    pic.trim_end().to_string()
                };
                if !(1..=3_074_324).contains(&n) {
                    self.cee_set(g(2), char_out(""))?;
                    return self.cee_end(
                        name,
                        g(3),
                        Some(Cond(2512, "the Lilian date is not valid")),
                    );
                }
                let s = format(&pic, from_lilian(n), (0, 0, 0), 0);
                self.cee_set(g(2), char_out(&s))?;
                self.cee_end(name, g(3), None)
            }
            // CEEDYWK(Lilian date, day of the week 1 = Sunday ... 7, fc).
            "CEEDYWK" => {
                let n = self.cee_num(g(0))?.to_i128()? as i64;
                if !(1..=3_074_324).contains(&n) {
                    self.cee_set(g(1), Value::Num(Dec::from_i64(0)))?;
                    return self.cee_end(
                        name,
                        g(2),
                        Some(Cond(2512, "the Lilian date is not valid")),
                    );
                }
                // Lilian day 1, 15 October 1582, was a Friday (6).
                let w = (n + 4).rem_euclid(7) + 1;
                self.cee_set(g(1), Value::Num(Dec::from_i64(w)))?;
                self.cee_end(name, g(2), None)
            }
            // CEELOCT(Lilian date, Lilian seconds, Gregorian character
            // YYYYMMDDHHMISS999, fc): now, local time.
            "CEELOCT" => {
                let (d, t, us) = dates::now();
                let l = lilian(d);
                let secs = Dec::parse(&format!(
                    "{}.{:03}",
                    l * 86400 + (t.0 * 3600 + t.1 * 60 + t.2) as i64,
                    us / 1000
                ))?;
                self.cee_set(g(0), Value::Num(Dec::from_i64(l)))?;
                self.cee_set(g(1), Value::Num(secs))?;
                self.cee_set(g(2), char_out(&format("YYYYMMDDHHMISS999", d, t, us)))?;
                self.cee_end(name, g(3), None)
            }
            // CEEUTCO(hours, minutes, seconds, fc): the offset from UTC to
            // local time. The job's time zone is UTC here
            // (docs/fidelity-gaps.md): 0.
            "CEEUTCO" => {
                for k in 0..3 {
                    self.cee_set(g(k), Value::Num(Dec::from_i64(0)))?;
                }
                self.cee_end(name, g(3), None)
            }
            // CEEUTC and CEEGMT(Lilian date, Lilian seconds, fc): now, UTC.
            "CEEUTC" | "CEEGMT" => {
                let (d, t, us) = dates::now();
                let l = lilian(d);
                let secs = Dec::parse(&format!(
                    "{}.{:03}",
                    l * 86400 + (t.0 * 3600 + t.1 * 60 + t.2) as i64,
                    us / 1000
                ))?;
                self.cee_set(g(0), Value::Num(Dec::from_i64(l)))?;
                self.cee_set(g(1), Value::Num(secs))?;
                self.cee_end(name, g(2), None)
            }
            // CEESECS(timestamp, picture, Lilian seconds, fc).
            "CEESECS" => {
                let input = self.cee_text(g(0))?;
                let pic = self.cee_text(g(1))?;
                let pic = if pic.trim().is_empty() {
                    "YYYY-MM-DD HH:MI:SS.999".to_string()
                } else {
                    pic.trim_end().to_string()
                };
                match parse(&input, &pic) {
                    Ok((d, t, us)) => {
                        let secs = Dec::parse(&format!(
                            "{}.{:06}",
                            lilian(d) * 86400 + (t.0 * 3600 + t.1 * 60 + t.2) as i64,
                            us
                        ))?;
                        self.cee_set(g(2), Value::Num(secs))?;
                        self.cee_end(name, g(3), None)
                    }
                    Err(c) => {
                        self.cee_set(g(2), Value::Num(Dec::from_i64(0)))?;
                        self.cee_end(name, g(3), Some(c))
                    }
                }
            }
            // CEEDATM(Lilian seconds, picture, character timestamp, fc).
            "CEEDATM" => {
                let s = self.cee_num(g(0))?;
                let whole = s.trunc().to_i128()? as i64;
                let us = (s
                    .sub(&s.trunc())?
                    .mul(&Dec::from_i64(1_000_000))?
                    .trunc()
                    .to_i128()?) as u32;
                if whole < 86400 {
                    self.cee_set(g(2), char_out(""))?;
                    return self.cee_end(
                        name,
                        g(3),
                        Some(Cond(2512, "the number of seconds is not valid")),
                    );
                }
                let d = from_lilian(whole / 86400);
                let r = (whole % 86400) as u32;
                let pic = self.cee_text(g(1))?;
                let out = format(pic.trim_end(), d, (r / 3600, r / 60 % 60, r % 60), us);
                self.cee_set(g(2), char_out(&out))?;
                self.cee_end(name, g(3), None)
            }
            // CEEISEC(year, month, day, hours, minutes, seconds,
            // milliseconds, Lilian seconds, fc).
            "CEEISEC" => {
                let mut v = [0i64; 7];
                for (k, x) in v.iter_mut().enumerate() {
                    *x = self.cee_num(g(k))?.to_i128()? as i64;
                }
                let d = (v[0] as i32, v[1] as u32, v[2] as u32);
                if !dates::valid(d)
                    || lilian(d) < 1
                    || v[3] > 23
                    || v[4] > 59
                    || v[5] > 59
                    || v[6] > 999
                {
                    return self.cee_end(name, g(8), Some(Cond(2507, "a value is not valid")));
                }
                let secs = Dec::parse(&format!(
                    "{}.{:03}",
                    lilian(d) * 86400 + v[3] * 3600 + v[4] * 60 + v[5],
                    v[6]
                ))?;
                self.cee_set(g(7), Value::Num(secs))?;
                self.cee_end(name, g(8), None)
            }
            // CEESECI(Lilian seconds, year, month, day, hours, minutes,
            // seconds, milliseconds, fc).
            "CEESECI" => {
                let s = self.cee_num(g(0))?;
                let whole = s.trunc().to_i128()? as i64;
                let ms = s
                    .sub(&s.trunc())?
                    .mul(&Dec::from_i64(1000))?
                    .trunc()
                    .to_i128()? as i64;
                if whole < 86400 {
                    return self.cee_end(
                        name,
                        g(8),
                        Some(Cond(2512, "the number of seconds is not valid")),
                    );
                }
                let d = from_lilian(whole / 86400);
                let r = whole % 86400;
                for (k, x) in [
                    d.0 as i64,
                    d.1 as i64,
                    d.2 as i64,
                    r / 3600,
                    r / 60 % 60,
                    r % 60,
                    ms,
                ]
                .into_iter()
                .enumerate()
                {
                    self.cee_set(g(k + 1), Value::Num(Dec::from_i64(x)))?;
                }
                self.cee_end(name, g(8), None)
            }
            // CEECRHP(heap ID, initial size, increment, options, fc): a
            // heap of its own.
            "CEECRHP" => {
                let id = {
                    let mut h = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
                    let (next, map) = h.get_or_insert_with(|| (0, HashMap::new()));
                    *next += 1;
                    map.insert(*next, Vec::new());
                    *next
                };
                self.cee_set(g(0), Value::Num(Dec::from_i64(id as i64)))?;
                self.cee_end(name, g(4), None)
            }
            // CEEGTST(heap ID, size, address, fc): storage from a heap (0,
            // the default heap).
            "CEEGTST" => {
                let id = self.cee_num(g(0))?.to_i128()? as i32;
                let size = self.cee_num(g(1))?;
                if id != 0 && !heap_exists(id) {
                    return self.cee_end(name, g(3), Some(Cond(2502, "the heap ID is not valid")));
                }
                let p = self.alloc(&Value::Num(size))?;
                if let (Value::Ptr(ptr), true) = (&p, id != 0) {
                    let mut h = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some((_, map)) = h.as_mut() {
                        map.entry(id).or_default().push(ptr.buf as usize);
                    }
                }
                self.cee_set(g(2), p)?;
                self.cee_end(name, g(3), None)
            }
            // CEEFRST(address, fc).
            "CEEFRST" => {
                let p = self.cee_value(g(0))?.unwrap_or(Value::Fig(Fig::Null));
                self.dealloc(&p)?;
                self.cee_end(name, g(1), None)
            }
            // CEECZST(address, new size, fc): the storage made that size,
            // its contents kept.
            "CEECZST" => {
                let p = self.cee_value(g(0))?.unwrap_or(Value::Fig(Fig::Null));
                let n = self.cee_num(g(1))?;
                let q = self.realloc(&p, &Value::Num(n))?;
                self.cee_set(g(0), q)?;
                self.cee_end(name, g(2), None)
            }
            // CEEDSHP(heap ID, fc): the heap and all its storage freed.
            "CEEDSHP" => {
                let id = self.cee_num(g(0))?.to_i128()? as i32;
                let blocks = {
                    let mut h = HEAPS.lock().unwrap_or_else(|e| e.into_inner());
                    h.as_mut().and_then(|(_, map)| map.remove(&id))
                };
                let Some(blocks) = blocks else {
                    return self.cee_end(name, g(1), Some(Cond(2502, "the heap ID is not valid")));
                };
                for b in blocks {
                    if let (Some(f), Some(x)) = (
                        self.bufs.heap.freed.get_mut(b),
                        self.bufs.heap.blocks.get_mut(b),
                    ) {
                        *f = true;
                        *x = Vec::new();
                    }
                }
                self.cee_end(name, g(1), None)
            }
            // CEEDOD(parameter number, descriptor type, data type,
            // descriptor information 1 and 2, length, fc): the length of
            // the argument the current procedure got for that parameter
            // (as4tainr gives the length only: the other values are 0).
            "CEEDOD" => {
                let n = self.cee_num(g(0))?.to_i128()? as usize;
                let len = self
                    .frames
                    .last()
                    .and_then(|f| f.lengths.get(n.wrapping_sub(1)).copied().flatten());
                let Some(len) = len else {
                    return self.cee_end(
                        name,
                        g(6),
                        Some(Cond(
                            502,
                            "there is no operational descriptor for the parameter",
                        )),
                    );
                };
                for k in 1..=4 {
                    self.cee_set(g(k), Value::Num(Dec::from_i64(0)))?;
                }
                self.cee_set(g(5), Value::Num(Dec::from_i64(len as i64)))?;
                self.cee_end(name, g(6), None)
            }
            // QlgConvertCase(request control block, input, output,
            // length, error code) (Convert Case API): request type 1, a
            // CCSID (0, the job's; as4tainr converts in CCSID 37 whatever
            // the CCSID); case 0 to upper case, 1 to lower case.
            // A character whose other case is not one character stays.
            "QLGCONVERTCASE" => {
                let rcb = match self.cee_value(g(0))? {
                    Some(Value::Char(c)) => c,
                    _ => vec![],
                };
                let int_at = |i: usize| {
                    rcb.get(i..i + 4)
                        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(0)
                };
                let (kind, case) = (int_at(0), int_at(8));
                let n = self.cee_num(g(3))?.to_i128()?.max(0) as usize;
                let input = match self.cee_value(g(1))? {
                    Some(Value::Char(c)) => c,
                    _ => vec![],
                };
                // The error code: bytes available 0, or the exception.
                let err = |m: &mut Self, id: Option<&str>| -> Result<()> {
                    if let Some(Arg::Ref(l)) = g(4) {
                        let l = *l;
                        let b = &mut m.bufs[l.buf][l.off..l.off + l.size];
                        let provided = b
                            .get(0..4)
                            .map(|x| i32::from_be_bytes([x[0], x[1], x[2], x[3]]))
                            .unwrap_or(0);
                        if provided < 8 {
                            // Bytes provided 0: the error is an escape
                            // message (API error code parameter rules).
                            if let Some(id) = id {
                                bail!("{id}: QlgConvertCase: the request is not valid.");
                            }
                        } else {
                            let avail: i32 = if id.is_some() { 16 } else { 0 };
                            b[4..8].copy_from_slice(&avail.to_be_bytes());
                            if let (Some(id), true) = (id, provided >= 15) {
                                b[8..15].copy_from_slice(&data::e(id));
                            }
                        }
                    } else if let Some(id) = id {
                        bail!("{id}: QlgConvertCase: the request is not valid.");
                        // (No error code parameter: an escape message.)
                    }
                    Ok(())
                };
                // The Convert Case API's messages: CPF3BEB request type,
                // CPF3BE5 case request, CPF3C12 length of data.
                let bad = if kind != 1 {
                    Some("CPF3BEB")
                } else if !(0..=1).contains(&case) {
                    Some("CPF3BE5")
                } else if n == 0 || n >= 16_773_104 {
                    Some("CPF3C12")
                } else {
                    None
                };
                if bad.is_some() {
                    err(self, bad)?;
                    return Ok(Value::Fig(Fig::Null));
                }
                let mut bytes = input.clone();
                bytes.resize(n, 0x40);
                let out: Vec<u8> = bytes
                    .iter()
                    .map(|&b| {
                        let c = a(&[b]);
                        let t: String = if case == 0 {
                            c.to_uppercase()
                        } else {
                            c.to_lowercase()
                        };
                        let e = data::e(&t);
                        if t.chars().count() == 1 && e.len() == 1 {
                            e[0]
                        } else {
                            b
                        }
                    })
                    .collect();
                if let Some(Arg::Ref(l)) = g(2) {
                    let l = *l;
                    let k = n.min(l.size);
                    self.bufs[l.buf][l.off..l.off + k].copy_from_slice(&out[..k]);
                }
                err(self, None)?;
                Ok(Value::Fig(Fig::Null))
            }
            // rslvsp(hex object type, object, library, authority) (the MI
            // builtin): a system pointer to the object; MCH3401 when it
            // cannot be found.
            "RSLVSP" => {
                let hex = match self.cee_value(g(0))? {
                    Some(Value::Char(c)) => c,
                    _ => vec![],
                };
                let h = u16::from_be_bytes([
                    hex.first().copied().unwrap_or(0),
                    hex.get(1).copied().unwrap_or(0),
                ]);
                let objtype = crate::sysapi::MI_TYPES
                    .iter()
                    .find(|(_, x)| *x == h)
                    .map(|(n, _)| n.to_string())
                    .ok_or_else(|| {
                        anyhow!("MCH3401: rslvsp: object type {h:04X} is not one as4tainr knows.")
                    })?;
                let text_of = |m: &Self, i: usize| -> Result<String> {
                    Ok(match m.cee_value(g(i))? {
                        Some(v @ Value::Ptr(_)) => a(&m.str_at(&v, None)?.chars()?),
                        Some(Value::Char(c)) => a(&c),
                        _ => String::new(),
                    }
                    .trim()
                    .to_ascii_uppercase())
                };
                let obj = text_of(self, 1)?;
                let lib = text_of(self, 2)?;
                let libs = if lib.is_empty() || lib == "*LIBL" {
                    self.io.libl.clone()
                } else {
                    vec![lib.clone()]
                };
                let client = self.io.client;
                let mut found = None;
                for l in libs {
                    if self.block(crate::catalog::object_exists(
                        client,
                        std::slice::from_ref(&l),
                        &obj,
                        &objtype,
                    ))? {
                        found = Some(l);
                        break;
                    }
                }
                let Some(l) = found else {
                    bail!(
                        "MCH3401: Object {obj} type {objtype} cannot be found in library {}.",
                        if lib.is_empty() { "*LIBL" } else { &lib }
                    );
                };
                Ok(Value::Ptr(super::super::mem::Ptr {
                    kind: super::super::mem::KIND_SYS,
                    owner: 0,
                    buf: super::super::mem::sys_id(&(objtype, l, obj)),
                    off: 0,
                }))
            }
            // QleActBndPgm(service program system pointer, activation mark,
            // activation information, its length, error code) (Activate
            // Bound Program API): the activation mark, also returned.
            "QLEACTBNDPGM" => {
                let target = match self.cee_value(g(0))? {
                    Some(Value::Ptr(p)) if p.kind == super::super::mem::KIND_SYS => {
                        super::super::mem::sys_ref(p.buf)
                    }
                    _ => None,
                };
                let Some((t, lib, name)) = target.filter(|(t, _, _)| t == "*SRVPGM" || t == "*PGM")
                else {
                    bail!("MCH3601: QleActBndPgm needs a system pointer to a service program.");
                };
                let _ = t;
                let client = self.io.client;
                let libl = self.io.libl.clone();
                self.block(crate::ile::find_srvpgm(client, &libl, &lib, &name))?;
                let mark = {
                    let mut g = ACTIVATED.lock().unwrap_or_else(|e| e.into_inner());
                    let k = match g.iter().position(|x| *x == (lib.clone(), name.clone())) {
                        Some(k) => k,
                        None => {
                            g.push((lib, name));
                            g.len() - 1
                        }
                    };
                    k as i64 + 1
                };
                self.cee_set(g(1), Value::Num(Dec::from_i64(mark)))?;
                Ok(Value::Num(Dec::from_i64(mark)))
            }
            // QleGetExp(activation mark, export number, name length, name,
            // export pointer, export type, error code) (Get Export API):
            // a procedure pointer to the export (by number, or by name
            // when the number is 0); export type 1 for a procedure, 0
            // when there is no such export.
            "QLEGETEXP" => {
                let mark = self.cee_num(g(0))?.to_i128()? as usize;
                let number = self.cee_num(g(1))?.to_i128()? as usize;
                let len = self.cee_num(g(2))?.to_i128()?.max(0) as usize;
                let nm: String = self.cee_text(g(3))?.chars().take(len).collect();
                let act = ACTIVATED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(mark.wrapping_sub(1))
                    .cloned();
                let Some((lib, sp)) = act else {
                    bail!("MCH3601: QleGetExp: activation mark {mark} is not valid.");
                };
                let client = self.io.client;
                let libl = self.io.libl.clone();
                let (lib, b) = self.block(crate::ile::find_srvpgm(client, &libl, &lib, &sp))?;
                let want = nm.trim().to_ascii_uppercase();
                let export = if number > 0 {
                    b.exports.get(number - 1).cloned()
                } else {
                    b.exports
                        .iter()
                        .find(|e| e.eq_ignore_ascii_case(&want))
                        .cloned()
                };
                let (ptr, kind) = match export {
                    Some(e) => (
                        Value::Ptr(super::super::mem::Ptr {
                            kind: super::super::mem::KIND_PROC,
                            owner: 0,
                            buf: super::super::mem::proc_id(
                                &e.to_ascii_uppercase(),
                                Some((lib, sp)),
                            ),
                            off: 0,
                        }),
                        1,
                    ),
                    None => (Value::Fig(Fig::Null), 0),
                };
                self.cee_set(g(4), ptr.clone())?;
                self.cee_set(g(5), Value::Num(Dec::from_i64(kind)))?;
                Ok(ptr)
            }
            other => Err(anyhow!("{other} is not a CEE API")),
        }
    }
}

fn heap_exists(id: i32) -> bool {
    HEAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|(_, m)| m.contains_key(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lilian_dates_as_the_cee_pages_show_them() {
        // CEEDAYS and CEEDATE: 16 May 1988 is Lilian day 148138.
        assert_eq!(lilian((1988, 5, 16)), 148138);
        assert_eq!(from_lilian(148143), (1988, 5, 21));
        let p = |i: &str, pic: &str| parse(i, pic).ok().map(|x| lilian(x.0));
        assert_eq!(p("19880516", "YYYYMMDD"), Some(148138));
        assert_eq!(p("1988-05-16", "YYYY-MM-DD"), Some(148138));
        // "6/2/88" (MM/DD/YY), "060288" (MMDDYY) and "88154" (YYDDD) are
        // the same day, 2 June 1988.
        assert_eq!(p("6/2/88", "MM/DD/YY"), Some(148155));
        assert_eq!(p("060288", "MMDDYY"), Some(148155));
        assert_eq!(p("88154", "YYDDD"), Some(148155));
        assert!(matches!(parse("19880230", "YYYYMMDD"), Err(Cond(2508, _))));
        assert!(matches!(parse("19881316", "YYYYMMDD"), Err(Cond(2517, _))));
        // CEEDATE's examples.
        let f = |n: i64, pic: &str| format(pic, from_lilian(n), (0, 0, 0), 0);
        assert_eq!(f(148138, "YYYY-ZM-ZD"), "1988-5-16");
        assert_eq!(f(148139, "ZM/DD/YYYY"), "5/17/1988");
        assert_eq!(f(148140, "DD Mmm YYYY"), "18 May 1988");
        assert_eq!(f(148141, "YY.DDD"), "88.140");
        assert_eq!(f(148142, "YY/MM/DD HH:MI:SS.99"), "88/05/20 00:00:00.00");
        assert_eq!(f(148143, "WWW., MMM DD, YYYY"), "SAT., MAY 21, 1988");
        assert_eq!(f(148143, "Www., Mmm DD, YYYY"), "Sat., May 21, 1988");
        assert_eq!(
            f(148143, "Wwwwwwwwww, Mmmmmmmmmm DD, YYYY"),
            "Saturday  , May        21, 1988"
        );
        assert_eq!(
            f(148143, "Wwwwwwwwwz, Mmmmmmmmmz DD, YYYY"),
            "Saturday, May 21, 1988"
        );
    }
}
