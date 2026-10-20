//! Dates, times and timestamps (ILE RPG Reference, "Date Data Type",
//! "Time Data Type", "Timestamp Data Type", "Date and time arithmetic"):
//! the external formats (*ISO, *USA, *EUR, *JIS, *MDY, *DMY, *YMD, *JUL,
//! *CYMD, *CMDY, *CDMY, *LONGJUL; *HMS and the time formats), validity,
//! adding durations and the difference between two values.
//!
//! In a program a date is held in *ISO form (`yyyy-mm-dd`), a time as
//! `hh.mm.ss` and a timestamp as `yyyy-mm-dd-hh.mm.ss.ffffff`.

use super::data::DurUnit;
use anyhow::{anyhow, bail, Result};

/// A date: year, month, day.
pub type Ymd = (i32, u32, u32);
/// A time: hour, minute, second.
pub type Hms = (u32, u32, u32);

pub fn leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn month_days(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if leap(y) {
                29
            } else {
                28
            }
        }
    }
}

pub fn valid(d: Ymd) -> bool {
    (1..=9999).contains(&d.0) && (1..=12).contains(&d.1) && d.2 >= 1 && d.2 <= month_days(d.0, d.1)
}

/// Days since 0000-03-01 style epoch (proleptic Gregorian), for
/// differences.
pub fn days(d: Ymd) -> i64 {
    let (y, m, dd) = (d.0 as i64, d.1 as i64, d.2 as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe
}

pub fn from_days(z: i64) -> Ymd {
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

fn day_of_year(d: Ymd) -> u32 {
    (days(d) - days((d.0, 1, 1)) + 1) as u32
}

/// A two-digit year: 40-99 are 1940-1999, 00-39 are 2000-2039.
fn century_year(yy: i32) -> i32 {
    if yy >= 40 {
        1900 + yy
    } else {
        2000 + yy
    }
}

/// A date format and its separator: `*MDY/`, `*ISO`, `*ISO0`, `*YMD-`...
#[derive(Clone, Debug, PartialEq)]
pub struct DateFmt {
    pub name: String,
    /// None: no separator (the `0` forms).
    pub sep: Option<char>,
}

impl DateFmt {
    pub fn parse(s: &str) -> Result<DateFmt> {
        let s = s.trim().to_ascii_uppercase();
        let names = [
            "*LONGJUL", "*CYMD", "*CMDY", "*CDMY", "*ISO", "*USA", "*EUR", "*JIS", "*MDY", "*DMY",
            "*YMD", "*JUL",
        ];
        let name = names
            .iter()
            .find(|n| s.starts_with(*n))
            .ok_or_else(|| anyhow!("date format {s} is not valid"))?;
        let rest = &s[name.len()..];
        let default = match *name {
            "*ISO" | "*JIS" => '-',
            "*EUR" => '.',
            _ => '/',
        };
        let sep = match rest {
            "" => Some(default),
            "0" => None,
            "&" => Some(' '),
            r if r.len() == 1 && "/-.,".contains(r) => {
                if matches!(*name, "*ISO" | "*USA" | "*EUR" | "*JIS") {
                    bail!("date format {s}: only 0 can follow {name}");
                }
                r.chars().next()
            }
            _ => bail!("date format {s} is not valid"),
        };
        Ok(DateFmt {
            name: name.to_string(),
            sep,
        })
    }

    pub fn iso() -> DateFmt {
        DateFmt {
            name: "*ISO".into(),
            sep: Some('-'),
        }
    }

    /// The parts in order: Y4 (4-digit year), Y2, C (century), M, D, J
    /// (day of year).
    fn parts(&self) -> &'static [&'static str] {
        match self.name.as_str() {
            "*ISO" | "*JIS" => &["Y4", "M", "D"],
            "*USA" => &["M", "D", "Y4"],
            "*EUR" => &["D", "M", "Y4"],
            "*MDY" => &["M", "D", "Y2"],
            "*DMY" => &["D", "M", "Y2"],
            "*YMD" => &["Y2", "M", "D"],
            "*JUL" => &["Y2", "J"],
            "*LONGJUL" => &["Y4", "J"],
            "*CYMD" => &["CY", "M", "D"],
            "*CMDY" => &["CM", "D", "Y2"],
            _ => &["CD", "M", "Y2"],
        }
    }

    /// Characters of the external form.
    pub fn len(&self) -> usize {
        let digits: usize = self
            .parts()
            .iter()
            .map(|p| match *p {
                "Y4" => 4,
                "J" => 3,
                "CY" | "CM" | "CD" => 3,
                _ => 2,
            })
            .sum();
        digits
            + if self.sep.is_some() {
                self.parts().len() - 1
            } else {
                0
            }
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// A date in a format.
pub fn format_date(d: Ymd, f: &DateFmt) -> String {
    let c = (d.0 / 100 - 19).clamp(0, 9);
    let pieces: Vec<String> = f
        .parts()
        .iter()
        .map(|p| match *p {
            "Y4" => format!("{:04}", d.0),
            "Y2" => format!("{:02}", d.0 % 100),
            "M" => format!("{:02}", d.1),
            "D" => format!("{:02}", d.2),
            "J" => format!("{:03}", day_of_year(d)),
            "CY" => format!("{c}{:02}", d.0 % 100),
            "CM" => format!("{c}{:02}", d.1),
            _ => format!("{c}{:02}", d.2),
        })
        .collect();
    match f.sep {
        Some(s) => pieces.join(&s.to_string()),
        None => pieces.concat(),
    }
}

/// A date in a format, checked (RNX0112 when not valid, as the date
/// built-ins and TEST(D) report).
pub fn parse_date(text: &str, f: &DateFmt) -> Result<Ymd> {
    let t = text.trim();
    let bad = || {
        anyhow!(
            "RNX0112: Date value {t} is not valid for format {}.",
            f.name
        )
    };
    let digits: Vec<char> = t.chars().filter(|c| c.is_ascii_digit()).collect();
    // The external form: with separators, exactly as long as the format;
    // without, digits only (a number may lack its leading zeros).
    match f.sep {
        Some(_) => {
            if t.chars().count() != f.len() || digits.len() + f.parts().len() - 1 != t.len() {
                return Err(bad());
            }
        }
        None => {
            if digits.len() != t.len() {
                return Err(bad());
            }
        }
    }
    let want: usize = f
        .parts()
        .iter()
        .map(|p| match *p {
            "Y4" => 4,
            "J" | "CY" | "CM" | "CD" => 3,
            _ => 2,
        })
        .sum();
    if digits.len() > want {
        return Err(bad());
    }
    let padded: String = std::iter::repeat_n('0', want - digits.len())
        .chain(digits)
        .collect();
    let mut at = 0;
    let mut take = |n: usize| -> i32 {
        let v = padded[at..at + n].parse::<i32>().unwrap_or(-1);
        at += n;
        v
    };
    let (mut y, mut m, mut d, mut j, mut cent) = (0i32, 0i32, 0i32, -1i32, None);
    for p in f.parts() {
        match *p {
            "Y4" => y = take(4),
            "Y2" => y = take(2),
            "M" => m = take(2),
            "D" => d = take(2),
            "J" => j = take(3),
            "CY" => {
                cent = Some(take(1));
                y = take(2);
            }
            "CM" => {
                cent = Some(take(1));
                m = take(2);
            }
            _ => {
                cent = Some(take(1));
                d = take(2);
            }
        }
    }
    let year = match (f.parts().contains(&"Y4"), cent) {
        (true, _) => y,
        (false, Some(c)) => 1900 + 100 * c + y,
        (false, None) => century_year(y),
    };
    let ymd = if j >= 0 {
        if j < 1 || j > if leap(year) { 366 } else { 365 } {
            return Err(bad());
        }
        from_days(days((year, 1, 1)) + j as i64 - 1)
    } else {
        if m < 1 || d < 1 {
            return Err(bad());
        }
        (year, m as u32, d as u32)
    };
    // Two-digit year formats hold 1940-2039 only.
    if !valid(ymd) {
        return Err(bad());
    }
    Ok(ymd)
}

/// A time format: *HMS (with a separator), *ISO, *USA, *EUR, *JIS.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeFmt {
    pub name: String,
    pub sep: Option<char>,
}

impl TimeFmt {
    pub fn parse(s: &str) -> Result<TimeFmt> {
        let s = s.trim().to_ascii_uppercase();
        let name = ["*HMS", "*ISO", "*USA", "*EUR", "*JIS"]
            .into_iter()
            .find(|n| s.starts_with(n))
            .ok_or_else(|| anyhow!("time format {s} is not valid"))?;
        let rest = &s[name.len()..];
        let default = match name {
            "*ISO" | "*EUR" => '.',
            _ => ':',
        };
        let sep = match rest {
            "" => Some(default),
            "0" => None,
            "&" => Some(' '),
            r if r.len() == 1 && ":.,".contains(r) && name == "*HMS" => r.chars().next(),
            _ => bail!("time format {s} is not valid"),
        };
        Ok(TimeFmt {
            name: name.into(),
            sep,
        })
    }

    pub fn iso() -> TimeFmt {
        TimeFmt {
            name: "*ISO".into(),
            sep: Some('.'),
        }
    }
}

pub fn format_time(t: Hms, f: &TimeFmt) -> String {
    if f.name == "*USA" {
        let (h12, ap) = match t.0 {
            0 => (12, "AM"),
            1..=11 => (t.0, "AM"),
            12 => (12, "PM"),
            _ => (t.0 - 12, "PM"),
        };
        return format!("{h12:02}:{:02} {ap}", t.1);
    }
    match f.sep {
        Some(s) => format!("{:02}{s}{:02}{s}{:02}", t.0, t.1, t.2),
        None => format!("{:02}{:02}{:02}", t.0, t.1, t.2),
    }
}

pub fn parse_time(text: &str, f: &TimeFmt) -> Result<Hms> {
    let t = text.trim();
    let bad = || {
        anyhow!(
            "RNX0113: Time value {t} is not valid for format {}.",
            f.name
        )
    };
    if f.name == "*USA" {
        let u = t.to_ascii_uppercase();
        let (hm, ap) = u.split_at(u.len().saturating_sub(2));
        let hm = hm.trim();
        let (h, m) = hm.split_once(':').ok_or_else(bad)?;
        let h: u32 = h.parse().map_err(|_| bad())?;
        let m: u32 = m.parse().map_err(|_| bad())?;
        if !(1..=12).contains(&h) || m > 59 {
            return Err(bad());
        }
        let h = match (ap, h) {
            ("AM", 12) => 0,
            ("AM", h) => h,
            ("PM", 12) => 12,
            ("PM", h) => h + 12,
            _ => return Err(bad()),
        };
        return Ok((h, m, 0));
    }
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() > 6 || (f.sep.is_some() && t.len() != 8) {
        return Err(bad());
    }
    let d = format!("{digits:0>6}");
    let h: u32 = d[0..2].parse().map_err(|_| bad())?;
    let m: u32 = d[2..4].parse().map_err(|_| bad())?;
    let s: u32 = d[4..6].parse().map_err(|_| bad())?;
    // 24.00.00 is valid and means midnight at the end of the day.
    if !(h < 24 && m < 60 && s < 60 || h == 24 && m == 0 && s == 0) {
        return Err(bad());
    }
    Ok((h, m, s))
}

/// A timestamp: date, time, microseconds.
pub type Ts = (Ymd, Hms, u32);

pub fn format_ts(t: &Ts, iso0: bool) -> String {
    let (d, h, us) = *t;
    if iso0 {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}{us:06}",
            d.0, d.1, d.2, h.0, h.1, h.2
        )
    } else {
        format!(
            "{:04}-{:02}-{:02}-{:02}.{:02}.{:02}.{us:06}",
            d.0, d.1, d.2, h.0, h.1, h.2
        )
    }
}

/// `yyyy-mm-dd-hh.mm.ss[.ffffff]`, or the *ISO0 digits; fewer fraction
/// digits are filled with zeros.
pub fn parse_ts(text: &str) -> Result<Ts> {
    let t = text.trim();
    let bad = || anyhow!("RNX0114: Timestamp value {t} is not valid.");
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if t.contains('-') {
        // yyyy-mm-dd-hh.mm.ss.ffffff (the fraction may be shorter).
        if t.len() < 19 {
            return Err(bad());
        }
        let d = parse_date(&t[..10], &DateFmt::iso()).map_err(|_| bad())?;
        if &t[10..11] != "-" {
            return Err(bad());
        }
        let h = parse_time(&t[11..19], &TimeFmt::iso()).map_err(|_| bad())?;
        let frac = t.get(20..).unwrap_or("");
        if !frac.chars().all(|c| c.is_ascii_digit()) || frac.len() > 12 {
            return Err(bad());
        }
        let us: u32 = format!("{:0<6}", &frac[..frac.len().min(6)])
            .parse()
            .unwrap_or(0);
        return Ok((d, h, us));
    }
    if digits.len() < 14 {
        return Err(bad());
    }
    let d = parse_date(&digits[..8], &DateFmt::parse("*ISO0")?).map_err(|_| bad())?;
    let h = parse_time(&digits[8..14], &TimeFmt::parse("*ISO0")?).map_err(|_| bad())?;
    let frac = &digits[14..];
    let us: u32 = format!("{:0<6}", &frac[..frac.len().min(6)])
        .parse()
        .unwrap_or(0);
    Ok((d, h, us))
}

/// A date plus a duration: months and years keep the day, or take the
/// last day of the month when the day is past it (ILE RPG Reference,
/// "Adding or Subtracting Dates").
pub fn add_to_date(d: Ymd, unit: DurUnit, n: i64) -> Result<Ymd> {
    let r = match unit {
        DurUnit::Days => from_days(days(d) + n),
        DurUnit::Months | DurUnit::Years => {
            let months = if unit == DurUnit::Years { n * 12 } else { n };
            let total = d.0 as i64 * 12 + (d.1 as i64 - 1) + months;
            let y = total.div_euclid(12) as i32;
            let m = (total.rem_euclid(12) + 1) as u32;
            (y, m, d.2.min(month_days(y, m)))
        }
        _ => bail!("a date can only be changed by years, months or days"),
    };
    if !valid(r) {
        bail!("RNX0112: The date is outside the range 0001-01-01 to 9999-12-31.");
    }
    Ok(r)
}

/// Microseconds since 0001-01-01 of a timestamp.
fn ts_us(t: &Ts) -> i128 {
    let (d, h, us) = *t;
    ((days(d) as i128 * 86400 + h.0 as i128 * 3600 + h.1 as i128 * 60 + h.2 as i128) * 1_000_000)
        + us as i128
}

fn from_ts_us(v: i128) -> Ts {
    let us = v.rem_euclid(1_000_000) as u32;
    let secs = v.div_euclid(1_000_000);
    let day = secs.div_euclid(86400) as i64;
    let s = secs.rem_euclid(86400) as u32;
    (from_days(day), (s / 3600, s / 60 % 60, s % 60), us)
}

pub fn add_to_ts(t: &Ts, unit: DurUnit, n: i64) -> Result<Ts> {
    let n = n as i128;
    Ok(match unit {
        DurUnit::Years | DurUnit::Months | DurUnit::Days => {
            (add_to_date(t.0, unit, n as i64)?, t.1, t.2)
        }
        DurUnit::Hours => from_ts_us(ts_us(t) + n * 3_600_000_000),
        DurUnit::Minutes => from_ts_us(ts_us(t) + n * 60_000_000),
        DurUnit::Seconds => from_ts_us(ts_us(t) + n * 1_000_000),
        DurUnit::MSeconds => from_ts_us(ts_us(t) + n),
    })
}

/// A time plus hours, minutes or seconds, around the clock.
pub fn add_to_time(t: Hms, unit: DurUnit, n: i64) -> Result<Hms> {
    let secs = match unit {
        DurUnit::Hours => n * 3600,
        DurUnit::Minutes => n * 60,
        DurUnit::Seconds => n,
        _ => bail!("a time can only be changed by hours, minutes or seconds"),
    };
    let s = (t.0 as i64 * 3600 + t.1 as i64 * 60 + t.2 as i64 + secs).rem_euclid(86400) as u32;
    Ok((s / 3600, s / 60 % 60, s % 60))
}

/// Whole months from b to a (negative when a is earlier).
fn months_between(a: Ymd, b: Ymd) -> i64 {
    let mut m = (a.0 as i64 - b.0 as i64) * 12 + (a.1 as i64 - b.1 as i64);
    if m > 0 && a.2 < b.2 {
        m -= 1;
    } else if m < 0 && a.2 > b.2 {
        m += 1;
    }
    m
}

/// %DIFF of two timestamps (dates and times are timestamps at midnight or
/// on 0001-01-01) in a unit: complete units, towards zero.
pub fn diff(a: &Ts, b: &Ts, unit: DurUnit) -> i128 {
    let us = ts_us(a) - ts_us(b);
    match unit {
        DurUnit::Years | DurUnit::Months => {
            let mut m = months_between(a.0, b.0) as i128;
            // A partial day at the end does not complete the month.
            let a_rest = ts_us(a) - ts_us(&(a.0, (0, 0, 0), 0));
            let b_rest = ts_us(b) - ts_us(&(b.0, (0, 0, 0), 0));
            if m > 0 && a.0 .2 == b.0 .2 && a_rest < b_rest {
                m -= 1;
            } else if m < 0 && a.0 .2 == b.0 .2 && a_rest > b_rest {
                m += 1;
            }
            if unit == DurUnit::Years {
                m / 12
            } else {
                m
            }
        }
        DurUnit::Days => us / 86_400_000_000,
        DurUnit::Hours => us / 3_600_000_000,
        DurUnit::Minutes => us / 60_000_000,
        DurUnit::Seconds => us / 1_000_000,
        DurUnit::MSeconds => us,
    }
}

/// The parts %SUBDT takes: years, months, days, hours, minutes, seconds,
/// microseconds.
pub fn subdt(t: &Ts, unit: DurUnit) -> i64 {
    let (d, h, us) = *t;
    match unit {
        DurUnit::Years => d.0 as i64,
        DurUnit::Months => d.1 as i64,
        DurUnit::Days => d.2 as i64,
        DurUnit::Hours => h.0 as i64,
        DurUnit::Minutes => h.1 as i64,
        DurUnit::Seconds => h.2 as i64,
        DurUnit::MSeconds => us as i64,
    }
}

/// The current local date and time of the system.
pub fn now() -> Ts {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // The job's time zone: UTC here (docs/fidelity-gaps.md).
    let us = now.as_micros() as i128 + (days((1970, 1, 1)) as i128) * 86_400_000_000;
    from_ts_us(us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        // ILE RPG Reference, "Date Data Type" table of external formats.
        let d = (2026, 10, 20);
        let f = |s: &str| DateFmt::parse(s).unwrap();
        assert_eq!(format_date(d, &f("*ISO")), "2026-10-20");
        assert_eq!(format_date(d, &f("*USA")), "10/20/2026");
        assert_eq!(format_date(d, &f("*EUR")), "20.10.2026");
        assert_eq!(format_date(d, &f("*MDY")), "10/20/26");
        assert_eq!(format_date(d, &f("*DMY-")), "20-10-26");
        assert_eq!(format_date(d, &f("*YMD0")), "261020");
        assert_eq!(format_date(d, &f("*JUL")), "26/293");
        assert_eq!(format_date(d, &f("*LONGJUL")), "2026/293");
        assert_eq!(format_date(d, &f("*CYMD")), "126/10/20");
        assert_eq!(format_date(d, &f("*ISO0")), "20261020");
        assert_eq!(parse_date("10/20/26", &f("*MDY")).unwrap(), d);
        // Two-digit years: 40-99 are 19xx.
        assert_eq!(parse_date("01/01/40", &f("*MDY")).unwrap(), (1940, 1, 1));
        assert_eq!(parse_date("26/293", &f("*JUL")).unwrap(), d);
        assert_eq!(parse_date("1261020", &f("*CYMD0")).unwrap(), d);
        assert!(parse_date("2026-02-29", &f("*ISO")).is_err());
        assert!(parse_date("2024-02-29", &f("*ISO")).is_ok());
        let t = TimeFmt::parse("*USA").unwrap();
        assert_eq!(format_time((13, 5, 0), &t), "01:05 PM");
        assert_eq!(parse_time("12:30 AM", &t).unwrap(), (0, 30, 0));
        assert_eq!(
            format_time((13, 5, 9), &TimeFmt::parse("*HMS").unwrap()),
            "13:05:09"
        );
    }

    #[test]
    fn arithmetic() {
        // ILE RPG Reference, "Adding or Subtracting Dates": adding a
        // month to January 31 gives the last day of February.
        assert_eq!(
            add_to_date((2026, 1, 31), DurUnit::Months, 1).unwrap(),
            (2026, 2, 28)
        );
        assert_eq!(
            add_to_date((2024, 2, 29), DurUnit::Years, 1).unwrap(),
            (2025, 2, 28)
        );
        assert_eq!(
            add_to_date((2026, 12, 31), DurUnit::Days, 1).unwrap(),
            (2027, 1, 1)
        );
        assert_eq!(
            add_to_time((23, 30, 0), DurUnit::Minutes, 45).unwrap(),
            (0, 15, 0)
        );
        let a: Ts = ((2026, 10, 20), (0, 0, 0), 0);
        let b: Ts = ((2026, 1, 1), (0, 0, 0), 0);
        assert_eq!(diff(&a, &b, DurUnit::Days), 292);
        assert_eq!(diff(&a, &b, DurUnit::Months), 9);
        assert_eq!(diff(&b, &a, DurUnit::Months), -9);
        assert_eq!(diff(&a, &b, DurUnit::Years), 0);
        let c: Ts = ((2026, 2, 19), (0, 0, 0), 0);
        assert_eq!(diff(&c, &b, DurUnit::Months), 1);
        assert_eq!(
            diff(
                &((2026, 2, 1), (0, 0, 0), 0),
                &((2026, 1, 31), (0, 0, 0), 0),
                DurUnit::Months
            ),
            0
        );
    }
}
