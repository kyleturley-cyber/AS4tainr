//! Built-in functions beyond the basic ones in interp.rs (ILE RPG
//! Reference, "Built-in Functions"): dates, times and timestamps
//! (%DATE, %TIME, %TIMESTAMP, %DIFF, %SUBDT, the durations, %CHAR and
//! %DEC of dates), arrays and lists (%LOOKUPxx, %TLOOKUPxx, %XFOOT,
//! %SUBARR, %LIST, %SPLIT, %MAXARR, %MINARR, IN), strings (%SCANR,
//! %SCANRPL, %LEFT, %RIGHT, %CONCAT, %CONCATARR, %CHARCOUNT, %EDITW),
//! numbers (%SQRT, %UNS, %UNSH, %DECPOS, %MAX, %MIN, %BITAND, %BITOR,
//! %BITXOR, %BITNOT), program and file state (%STATUS, %OPEN, %OCCUR,
//! %PARMNUM, %PASSED, %OMITTED, %SHTDN, %PROC).

use super::*;
use crate::rpg::data::DurUnit;
use crate::rpg::dates::{self, DateFmt, TimeFmt};

/// A date, time or timestamp value as the program holds it.
#[derive(Clone, Copy, Debug)]
pub(super) enum Temporal {
    Date(dates::Ymd),
    Time(dates::Hms),
    Ts(dates::Ts),
}

/// Recognize a date (`yyyy-mm-dd`), time (`hh.mm.ss`) or timestamp in
/// the forms the program holds them in.
pub(super) fn temporal(v: &Value) -> Option<Temporal> {
    let Value::Char(c) = v else { return None };
    let t = a(c);
    let b = t.as_bytes();
    match t.len() {
        10 if b[4] == b'-' && b[7] == b'-' => dates::parse_date(&t, &DateFmt::iso())
            .ok()
            .map(Temporal::Date),
        8 if b[2] == b'.' && b[5] == b'.' => dates::parse_time(&t, &TimeFmt::iso())
            .ok()
            .map(Temporal::Time),
        19..=32 if b[4] == b'-' && b[10] == b'-' => dates::parse_ts(&t).ok().map(Temporal::Ts),
        _ => None,
    }
}

fn date_text(d: dates::Ymd) -> Value {
    Value::str(&dates::format_date(d, &DateFmt::iso()))
}

fn time_text(t: dates::Hms) -> Value {
    Value::str(&dates::format_time(t, &TimeFmt::iso()))
}

fn ts_text(t: &dates::Ts) -> Value {
    Value::str(&dates::format_ts(t, false))
}

fn as_ts(t: Temporal) -> dates::Ts {
    match t {
        Temporal::Date(d) => (d, (0, 0, 0), 0),
        Temporal::Time(h) => ((1, 1, 1), h, 0),
        Temporal::Ts(t) => t,
    }
}

fn num(n: i128) -> Value {
    Value::Num(Dec::new(n, 0))
}

impl Machine<'_> {
    /// A special value written as an argument: *ISO, *DAYS, *ALLSEP...
    fn special_arg(&self, args: &[Expr], i: usize) -> Option<String> {
        match args.get(i) {
            Some(Expr::Name { parts, args: None })
                if parts.len() == 1 && parts[0].starts_with('*') =>
            {
                Some(parts[0].clone())
            }
            _ => None,
        }
    }

    fn type_is_temporal(&self, e: &Expr) -> bool {
        matches!(
            self.temporal_type(e),
            Some(DType::Date(_)) | Some(DType::Time(_)) | Some(DType::Timestamp)
        )
    }

    /// The type of a date, time or timestamp expression: a field's own;
    /// a built-in's result or date arithmetic in the control
    /// specification's format.
    pub(super) fn temporal_type(&self, e: &Expr) -> Option<DType> {
        let dft_d = DType::Date(self.prog.datfmt().ok()?);
        let dft_t = DType::Time(self.prog.timfmt().ok()?);
        match e {
            Expr::Bif(b, _) => match b.as_str() {
                "DATE" => Some(dft_d),
                "TIME" => Some(dft_t),
                "TIMESTAMP" => Some(DType::Timestamp),
                _ => self.type_of(e),
            },
            Expr::Bin(BinOp::Add | BinOp::Sub, l, r) => {
                match (self.temporal_type(l), self.temporal_type(r)) {
                    (Some(DType::Date(_)), None) | (None, Some(DType::Date(_))) => Some(dft_d),
                    (Some(DType::Time(_)), None) | (None, Some(DType::Time(_))) => Some(dft_t),
                    (Some(DType::Timestamp), None) | (None, Some(DType::Timestamp)) => {
                        Some(DType::Timestamp)
                    }
                    _ => None,
                }
            }
            _ => self.type_of(e),
        }
    }

    /// The elements of an array (or the values of a list): those in use
    /// of a varying-dimension array.
    pub(super) fn list_of(&mut self, e: &Expr) -> Result<Vec<Value>> {
        if let Some(arr) = self.array_ref(e) {
            if arr.index.is_none() {
                let n = self.elems_in_use(&arr.name, &arr.var);
                let mut out = Vec::with_capacity(n as usize);
                for k in 1..=n as i128 {
                    out.push(self.load_var(&arr.var, Some(k))?);
                }
                return Ok(out);
            }
        }
        match self.eval(e)? {
            Value::List(v) => Ok(v),
            other => Ok(vec![other]),
        }
    }

    /// `x IN %RANGE(a:b)`: a <= x <= b; `x IN list` (an array, %LIST, an
    /// enumeration): x equals one of the values (ILE RPG Reference, IN).
    pub(super) fn eval_in(&mut self, l: &Expr, r: &Expr) -> Result<Value> {
        let x = self.eval(l)?;
        if let Expr::Bif(b, args) = r {
            if b == "RANGE" {
                if args.len() != 2 {
                    bail!("%RANGE needs a low and a high value");
                }
                let lo = self.eval(&args[0])?;
                let hi = self.eval(&args[1])?;
                return Ok(Value::Ind(
                    data::compare(&x, &lo)? != Ordering::Less
                        && data::compare(&x, &hi)? != Ordering::Greater,
                ));
            }
        }
        for v in self.list_of(r)? {
            if data::compare(&x, &v)? == Ordering::Equal {
                return Ok(Value::Ind(true));
            }
        }
        Ok(Value::Ind(false))
    }

    /// A date, time or timestamp plus or minus a duration.
    pub(super) fn date_arith(&mut self, op: BinOp, x: Value, y: Value) -> Result<Value> {
        let (base, unit, n) = match (&x, &y, op) {
            (b, Value::Dur(u, n), BinOp::Add) => (b.clone(), *u, *n),
            (b, Value::Dur(u, n), _) => (b.clone(), *u, -*n),
            (Value::Dur(u, n), b, BinOp::Add) => (b.clone(), *u, *n),
            _ => bail!("a duration cannot be subtracted from {}", y.describe()),
        };
        let t = temporal(&base)
            .ok_or_else(|| anyhow!("{} is not a date, time or timestamp", base.describe()))?;
        Ok(match t {
            Temporal::Date(d) => date_text(dates::add_to_date(d, unit, n)?),
            Temporal::Time(h) => time_text(dates::add_to_time(h, unit, n)?),
            Temporal::Ts(ts) => ts_text(&dates::add_to_ts(&ts, unit, n)?),
        })
    }

    /// A value for %DATE, %TIME, %TIMESTAMP: character data in a format,
    /// a number with the format's digits, or another date/time value.
    fn temporal_arg(&mut self, args: &[Expr]) -> Result<(Value, Option<String>)> {
        let v = self.eval(&args[0])?;
        Ok((v, self.special_arg(args, 1)))
    }

    /// Built-ins whose form here differs from interp.rs's version: %CHAR
    /// and %DEC of dates, %ELEM of varying arrays.
    pub(super) fn bif_override(&mut self, b: &str, args: &[Expr]) -> Result<Option<Value>> {
        match b {
            "CHAR" if !args.is_empty() && (args.len() == 2 || self.type_is_temporal(&args[0])) => {
                let v = self.eval(&args[0])?;
                let Some(t) = temporal(&v) else {
                    return Ok(None);
                };
                let fmt = self.special_arg(args, 1);
                // Without a format: the date's (time's) own format; an
                // expression's is the control specification's.
                let ty = self.temporal_type(&args[0]);
                Ok(Some(Value::str(&match t {
                    Temporal::Date(d) => {
                        let f = match (fmt.as_deref(), ty) {
                            (Some(f), _) => DateFmt::parse(f)?,
                            (None, Some(DType::Date(f))) => f.or(self.prog.datfmt()?),
                            (None, _) => self.prog.datfmt()?,
                        };
                        dates::format_date(d, &f)
                    }
                    Temporal::Time(h) => {
                        let f = match (fmt.as_deref(), ty) {
                            (Some(f), _) => TimeFmt::parse(f)?,
                            (None, Some(DType::Time(f))) => f.or(self.prog.timfmt()?),
                            (None, _) => self.prog.timfmt()?,
                        };
                        dates::format_time(h, &f)
                    }
                    Temporal::Ts(ts) => dates::format_ts(&ts, fmt.as_deref() == Some("*ISO0")),
                })))
            }
            "DEC" if !args.is_empty() && args.len() <= 2 => {
                let typed = self.type_is_temporal(&args[0]);
                if !typed && args.len() != 2 {
                    return Ok(None);
                }
                if args.len() == 2 && self.special_arg(args, 1).is_none() {
                    return Ok(None);
                }
                let v = self.eval(&args[0])?;
                let Some(t) = temporal(&v) else {
                    return Ok(None);
                };
                let fmt = self.special_arg(args, 1);
                let ty = self.temporal_type(&args[0]);
                // The value's digits in the format (else the date's own),
                // without separators.
                let text = match t {
                    Temporal::Date(d) => {
                        let mut f = match (fmt.as_deref(), ty) {
                            (Some(x), _) => DateFmt::parse(x)?,
                            (None, Some(DType::Date(f))) => f.or(self.prog.datfmt()?),
                            _ => self.prog.datfmt()?,
                        };
                        f.sep = None;
                        dates::format_date(d, &f)
                    }
                    Temporal::Time(h) => {
                        let mut f = match (fmt.as_deref(), ty) {
                            (Some(x), _) => TimeFmt::parse(x)?,
                            (None, Some(DType::Time(f))) => f.or(self.prog.timfmt()?),
                            _ => self.prog.timfmt()?,
                        };
                        f.sep = None;
                        dates::format_time(h, &f)
                    }
                    Temporal::Ts(ts) => dates::format_ts(&ts, true),
                };
                Ok(Some(Value::Num(Dec::parse(&text)?)))
            }
            "ELEM" => {
                let Some(e) = args.first() else {
                    return Ok(None);
                };
                match self.array_ref(e) {
                    Some(arr) if self.prog.varying.contains_key(&arr.name) => {
                        Ok(Some(num(self.elems_in_use(&arr.name, &arr.var) as i128)))
                    }
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    pub(super) fn bif_more(&mut self, b: &str, args: &[Expr]) -> Result<Option<Value>> {
        let arg = |m: &mut Self, i: usize| -> Result<Value> {
            m.eval(
                args.get(i)
                    .ok_or_else(|| anyhow!("%{b} needs more arguments"))?,
            )
        };
        let int_arg = |m: &mut Self, i: usize| -> Result<i128> { arg(m, i)?.num()?.to_i128() };
        Ok(Some(match b {
            // ---- dates and times ---------------------------------------
            "DATE" => {
                if args.is_empty()
                    || matches!(self.special_arg(args, 0).as_deref(), Some("*SYS" | "*JOB"))
                {
                    return Ok(Some(date_text(dates::now().0)));
                }
                let (v, fmt) = self.temporal_arg(args)?;
                if let Some(t) = temporal(&v) {
                    if fmt.is_none() {
                        return Ok(Some(match t {
                            Temporal::Date(d) => date_text(d),
                            Temporal::Ts(ts) => date_text(ts.0),
                            Temporal::Time(_) => bail!("%DATE of a time is not valid"),
                        }));
                    }
                }
                // Without a format: the control specification's DATFMT.
                let dft = self.prog.datfmt()?;
                let f = fmt.unwrap_or_else(|| dft.code());
                let d = match v {
                    Value::Num(n) => {
                        let f = DateFmt::parse(&format!("{}0", f.trim_end_matches('0')))?;
                        dates::parse_date(&n.trunc().mantissa().to_string(), &f)?
                    }
                    other => dates::parse_date(&a(&other.chars()?), &DateFmt::parse(&f)?)?,
                };
                date_text(d)
            }
            "TIME" => {
                if args.is_empty() || self.special_arg(args, 0).as_deref() == Some("*SYS") {
                    return Ok(Some(time_text(dates::now().1)));
                }
                let (v, fmt) = self.temporal_arg(args)?;
                if let Some(t) = temporal(&v) {
                    if fmt.is_none() {
                        return Ok(Some(match t {
                            Temporal::Time(h) => time_text(h),
                            Temporal::Ts(ts) => time_text(ts.1),
                            Temporal::Date(_) => bail!("%TIME of a date is not valid"),
                        }));
                    }
                }
                let dft = self.prog.timfmt()?;
                let f = fmt.unwrap_or_else(|| dft.code());
                let h = match v {
                    Value::Num(n) => dates::parse_time(
                        &n.trunc().mantissa().to_string(),
                        &TimeFmt::parse(&format!("{}0", f.trim_end_matches('0')))?,
                    )?,
                    other => dates::parse_time(&a(&other.chars()?), &TimeFmt::parse(&f)?)?,
                };
                time_text(h)
            }
            "TIMESTAMP" => {
                if args.is_empty() || self.special_arg(args, 0).as_deref() == Some("*SYS") {
                    return Ok(Some(ts_text(&dates::now())));
                }
                let (v, _fmt) = self.temporal_arg(args)?;
                let ts = match temporal(&v) {
                    Some(Temporal::Date(d)) => (d, (0, 0, 0), 0),
                    Some(Temporal::Ts(t)) => t,
                    Some(Temporal::Time(_)) => bail!("%TIMESTAMP of a time is not valid"),
                    None => match v {
                        Value::Num(n) => dates::parse_ts(&n.trunc().mantissa().to_string())?,
                        other => dates::parse_ts(&a(&other.chars()?))?,
                    },
                };
                ts_text(&ts)
            }
            "YEARS" | "MONTHS" | "DAYS" | "HOURS" | "MINUTES" | "SECONDS" | "MSECONDS" => {
                let u = DurUnit::parse(&format!("*{b}")).unwrap();
                Value::Dur(u, int_arg(self, 0)? as i64)
            }
            "DIFF" => {
                let x = arg(self, 0)?;
                let y = arg(self, 1)?;
                let unit = self
                    .special_arg(args, 2)
                    .and_then(|s| DurUnit::parse(&s))
                    .ok_or_else(|| anyhow!("%DIFF needs a unit: *YEARS, *MONTHS, *DAYS..."))?;
                let tx = temporal(&x).ok_or_else(|| {
                    anyhow!("%DIFF: {} is not a date, time or timestamp", x.describe())
                })?;
                let ty = temporal(&y).ok_or_else(|| {
                    anyhow!("%DIFF: {} is not a date, time or timestamp", y.describe())
                })?;
                num(dates::diff(&as_ts(tx), &as_ts(ty), unit))
            }
            "SUBDT" => {
                let x = arg(self, 0)?;
                let unit = self
                    .special_arg(args, 1)
                    .and_then(|s| DurUnit::parse(&s))
                    .ok_or_else(|| anyhow!("%SUBDT needs a unit"))?;
                let t = temporal(&x).ok_or_else(|| {
                    anyhow!("%SUBDT: {} is not a date, time or timestamp", x.describe())
                })?;
                num(dates::subdt(&as_ts(t), unit) as i128)
            }
            // ---- arrays and lists --------------------------------------
            "LIST" => {
                let mut v = Vec::new();
                for e in args {
                    v.push(self.eval(e)?);
                }
                Value::List(v)
            }
            "SPLIT" => {
                // %SPLIT(string : separators): the pieces between
                // separators (default a blank); runs of separators and
                // separators at the ends make no empty pieces, unless
                // *ALLSEP (ILE RPG Reference, %SPLIT).
                let s = a(&arg(self, 0)?.chars()?);
                let seps: Vec<char> = match args.get(1) {
                    Some(e) if self.special_arg(args, 1).is_none() => {
                        a(&self.eval(e)?.chars()?).chars().collect()
                    }
                    _ => vec![' '],
                };
                let allsep = (1..args.len())
                    .any(|i| self.special_arg(args, i).as_deref() == Some("*ALLSEP"));
                let pieces: Vec<Value> = s
                    .split(|c| seps.contains(&c))
                    .filter(|p| allsep || !p.is_empty())
                    .map(Value::str)
                    .collect();
                Value::List(pieces)
            }
            "SUBARR" => {
                let all = self.list_of(&args[0])?;
                let start = int_arg(self, 1)? as usize;
                let n = match args.get(2) {
                    Some(_) => int_arg(self, 2)? as usize,
                    None => all.len() + 1 - start.max(1),
                };
                if start < 1 || start - 1 + n > all.len() {
                    bail!("RNQ0121: %SUBARR start {start} or number {n} is outside the array.");
                }
                Value::List(all[start - 1..start - 1 + n].to_vec())
            }
            "XFOOT" => {
                let mut sum = Dec::ZERO;
                for v in self.list_of(&args[0])? {
                    sum = sum.add(&v.num()?)?;
                }
                Value::Num(sum)
            }
            "MAXARR" | "MINARR" => {
                let all = self.list_of(&args[0])?;
                let start = if args.len() > 1 {
                    int_arg(self, 1)? as usize
                } else {
                    1
                };
                let n = if args.len() > 2 {
                    int_arg(self, 2)? as usize
                } else {
                    all.len() + 1 - start
                };
                let mut best: Option<(usize, Value)> = None;
                for (k, v) in all.iter().enumerate().skip(start - 1).take(n) {
                    let better = match &best {
                        None => true,
                        Some((_, bv)) => {
                            let o = data::compare(v, bv)?;
                            if b == "MAXARR" {
                                o == Ordering::Greater
                            } else {
                                o == Ordering::Less
                            }
                        }
                    };
                    if better {
                        best = Some((k + 1, v.clone()));
                    }
                }
                num(best.map(|x| x.0).unwrap_or(0) as i128)
            }
            "LOOKUP" | "LOOKUPLT" | "LOOKUPLE" | "LOOKUPGE" | "LOOKUPGT" => {
                // The index of the element found, 0 when none (ILE RPG
                // Reference, %LOOKUPxx): LT and LE the nearest below,
                // GE and GT the nearest above.
                let x = arg(self, 0)?;
                let all = self.list_of(&args[1])?;
                let start = if args.len() > 2 {
                    int_arg(self, 2)? as usize
                } else {
                    1
                };
                let n = if args.len() > 3 {
                    int_arg(self, 3)? as usize
                } else {
                    all.len() + 1 - start.max(1)
                };
                num(self.search(&x, &all, start, n, &b[6..])? as i128)
            }
            "TLOOKUP" | "TLOOKUPLT" | "TLOOKUPLE" | "TLOOKUPGE" | "TLOOKUPGT" => {
                let x = arg(self, 0)?;
                let tab = self
                    .array_ref(&args[1])
                    .ok_or_else(|| anyhow!("%{b} needs a table"))?;
                let all = self.list_of(&args[1])?;
                let k = self.search(&x, &all, 1, all.len(), &b[7..])?;
                if k > 0 {
                    self.table_cur.insert(tab.name.clone(), k as i128);
                    if let Some(alt) = args.get(2).and_then(|e| self.array_ref(e)) {
                        self.table_cur.insert(alt.name, k as i128);
                    }
                }
                Value::Ind(k > 0)
            }
            // ---- strings -------------------------------------------------
            "SCANR" => {
                let pat = arg(self, 0)?.chars()?;
                let s = arg(self, 1)?.chars()?;
                let start = if args.len() > 2 {
                    int_arg(self, 2)? as usize
                } else {
                    1
                };
                let len = if args.len() > 3 {
                    int_arg(self, 3)? as usize
                } else {
                    s.len() + 1 - start.max(1)
                };
                if start < 1 || start - 1 + len > s.len() {
                    bail!("RNQ0100: %SCANR start or length is outside the string.");
                }
                let hay = &s[start - 1..start - 1 + len];
                let found = if pat.is_empty() || pat.len() > hay.len() {
                    0
                } else {
                    hay.windows(pat.len())
                        .rposition(|w| w == pat.as_slice())
                        .map(|p| p + start)
                        .unwrap_or(0)
                };
                num(found as i128)
            }
            "SCANRPL" => {
                let pat = arg(self, 0)?.chars()?;
                let rep = arg(self, 1)?.chars()?;
                let s = arg(self, 2)?.chars()?;
                let start = if args.len() > 3 {
                    int_arg(self, 3)? as usize
                } else {
                    1
                };
                let len = if args.len() > 4 {
                    int_arg(self, 4)? as usize
                } else {
                    s.len() + 1 - start.max(1)
                };
                if start < 1 || start - 1 + len > s.len() {
                    bail!("RNQ0100: %SCANRPL start or length is outside the string.");
                }
                let mut out = s[..start - 1].to_vec();
                let region = &s[start - 1..start - 1 + len];
                let mut i = 0;
                while i < region.len() {
                    if !pat.is_empty() && region[i..].starts_with(&pat) {
                        out.extend(&rep);
                        i += pat.len();
                    } else {
                        out.push(region[i]);
                        i += 1;
                    }
                }
                out.extend(&s[start - 1 + len..]);
                Value::Char(out)
            }
            "LEFT" | "RIGHT" => {
                let s = arg(self, 0)?.chars()?;
                let n = int_arg(self, 1)? as usize;
                if n > s.len() {
                    bail!("RNQ0100: %{b} length {n} is longer than the string.");
                }
                Value::Char(if b == "LEFT" {
                    s[..n].to_vec()
                } else {
                    s[s.len() - n..].to_vec()
                })
            }
            "CONCAT" => {
                // %CONCAT(separator : a : b ...): the values joined with
                // the separator (*NONE: nothing between them).
                let sep = match self.special_arg(args, 0).as_deref() {
                    Some("*NONE") => vec![],
                    Some("*BLANK") | Some("*BLANKS") => vec![data::BLANK],
                    _ => arg(self, 0)?.chars()?,
                };
                let mut out = Vec::new();
                for (k, e) in args.iter().enumerate().skip(1) {
                    if k > 1 {
                        out.extend(&sep);
                    }
                    let v = self.eval(e)?;
                    out.extend(match v {
                        Value::Num(d) => e_of(&char_of_num(&d)),
                        o => o.chars()?,
                    });
                }
                Value::Char(out)
            }
            "CONCATARR" => {
                let sep = match self.special_arg(args, 0).as_deref() {
                    Some("*NONE") => vec![],
                    _ => arg(self, 0)?.chars()?,
                };
                let all = self.list_of(&args[1])?;
                let mut out = Vec::new();
                for (k, v) in all.iter().enumerate() {
                    if k > 0 {
                        out.extend(&sep);
                    }
                    out.extend(match v {
                        Value::Num(d) => e_of(&char_of_num(d)),
                        o => o.chars()?,
                    });
                }
                Value::Char(out)
            }
            "CHARCOUNT" => num(arg(self, 0)?.chars()?.len() as i128),
            "EDITW" => {
                let x = arg(self, 0)?.num()?;
                let (digits, decs) = self
                    .type_of(&args[0])
                    .filter(|t| t.is_numeric())
                    .map(|t| t.precision())
                    .unwrap_or(((x.int_digits() + x.scale() as u32).max(1), x.scale() as u32));
                let w = a(&arg(self, 1)?.chars()?);
                Value::str(&edit_word(&x, digits, decs, &w)?)
            }
            // ---- numbers ------------------------------------------------
            "SQRT" => {
                let x = arg(self, 0)?.num()?;
                if x.is_negative() {
                    bail!("RNQ0101: %SQRT of a negative number.");
                }
                Value::Num(ops::dec_sqrt(&x, 15)?.normalized())
            }
            "UNS" | "UNSH" => {
                let v = arg(self, 0)?;
                let d = match v {
                    Value::Char(c) => Dec::parse(a(&c).trim())?,
                    o => o.num()?,
                };
                let r = d.rescale(0, b == "UNSH")?;
                if r.is_negative() {
                    bail!("RNQ0103: %{b} of a negative value.");
                }
                Value::Num(r)
            }
            "DECPOS" => {
                let d = self
                    .type_of(&args[0])
                    .filter(|t| t.is_numeric())
                    .map(|t| t.precision().1)
                    .unwrap_or_else(|| 0);
                num(d as i128)
            }
            "MAX" | "MIN" => {
                let mut best = arg(self, 0)?;
                for i in 1..args.len() {
                    let v = arg(self, i)?;
                    let o = data::compare(&v, &best)?;
                    if (b == "MAX" && o == Ordering::Greater) || (b == "MIN" && o == Ordering::Less)
                    {
                        best = v;
                    }
                }
                best
            }
            "BITAND" | "BITOR" | "BITXOR" | "BITNOT" => {
                let first = arg(self, 0)?;
                match first {
                    Value::Num(_) => {
                        let mut acc = first.num()?.to_i128()?;
                        if b == "BITNOT" {
                            acc = !acc;
                        }
                        for i in 1..args.len() {
                            let x = int_arg(self, i)?;
                            acc = match b {
                                "BITAND" => acc & x,
                                "BITOR" => acc | x,
                                _ => acc ^ x,
                            };
                        }
                        num(acc)
                    }
                    other => {
                        let mut acc = other.chars()?;
                        if b == "BITNOT" {
                            acc.iter_mut().for_each(|x| *x = !*x);
                        }
                        for i in 1..args.len() {
                            let x = arg(self, i)?.chars()?;
                            for (k, byte) in acc.iter_mut().enumerate() {
                                let y = x.get(k).copied().unwrap_or(0);
                                *byte = match b {
                                    "BITAND" => *byte & y,
                                    "BITOR" => *byte | y,
                                    _ => *byte ^ y,
                                };
                            }
                        }
                        Value::Char(acc)
                    }
                }
            }
            // ---- program and file state ------------------------------------
            "OPEN" => {
                let name = match args.first() {
                    Some(Expr::Name { parts, .. }) => parts.join("."),
                    _ => bail!("%OPEN needs a file name"),
                };
                let i = self
                    .file_idx(&name)
                    .ok_or_else(|| anyhow!("{name} is not a file"))?;
                Value::Ind(self.files[i].open.is_some() || self.files[i].ws_open)
            }
            "STATUS" => {
                // The most recent program or file status code; with a
                // file, that file's (ILE RPG Reference, %STATUS).
                let st = match args.first() {
                    None => self.last.clone(),
                    Some(_) => self.file_status(args)?,
                };
                num(st.code as i128)
            }
            "OCCUR" => num(self.occurrence(&args[0])? as i128),
            "SHTDN" => Value::Ind(false),
            // %MSG(id : message file : data), for SND-MSG.
            "MSG" => {
                let mut v = Vec::new();
                for e in args {
                    v.push(self.eval(e)?);
                }
                Value::List(v)
            }
            "PROC" => Value::str(
                &self
                    .frames
                    .last()
                    .map(|f| f.proc.name.clone())
                    .unwrap_or_else(|| self.prog.name.clone()),
            ),
            "PARMNUM" | "PASSED" | "OMITTED" => {
                let name = match args.first() {
                    Some(Expr::Name { parts, .. }) => parts.join("."),
                    _ => bail!("%{b} needs a parameter name"),
                };
                let (pos, passed) = match self.frames.last() {
                    Some(f) => (
                        f.proc.params.iter().position(|p| p.name == name),
                        f.passed,
                    ),
                    None => (
                        self.prog.entry.iter().position(|p| {
                            matches!(&p.target, Expr::Name { parts, .. } if parts.join(".") == name)
                        }),
                        self.parms_passed,
                    ),
                };
                let pos = pos.ok_or_else(|| anyhow!("%{b}: {name} is not a parameter"))? + 1;
                match b {
                    "PARMNUM" => num(pos as i128),
                    "PASSED" => Value::Ind(pos <= passed && !self.omitted(&name)),
                    _ => Value::Ind(self.omitted(&name)),
                }
            }
            _ => return Ok(None),
        }))
    }

    /// The kind of value an argument is, for choosing an OVERLOAD
    /// candidate: N numeric, C character, D date, T time, Z timestamp,
    /// I indicator.
    fn arg_kind(&mut self, e: &Expr) -> Result<Vec<char>> {
        let of_type = |t: DType| -> char {
            match t {
                DType::Date(_) => 'D',
                DType::Time(_) => 'T',
                DType::Timestamp => 'Z',
                DType::Ind => 'I',
                t if t.is_numeric() => 'N',
                _ => 'C',
            }
        };
        if let Some(t) = self.type_of(e) {
            return Ok(vec![of_type(t)]);
        }
        Ok(match self.eval(e)? {
            Value::Num(_) => vec!['N'],
            Value::Ind(_) | Value::Fig(Fig::On) | Value::Fig(Fig::Off) => vec!['I'],
            v @ Value::Char(_) => match temporal(&v) {
                // A literal in date form may be meant as a date.
                Some(Temporal::Date(_)) => vec!['D', 'C'],
                Some(Temporal::Time(_)) => vec!['T', 'C'],
                Some(Temporal::Ts(_)) => vec!['Z', 'C'],
                None => vec!['C'],
            },
            _ => vec!['C', 'N'],
        })
    }

    /// The OVERLOAD candidate a call means: the first whose parameters
    /// take the arguments, by number and kind (ILE RPG Reference,
    /// OVERLOAD keyword).
    pub(super) fn overload_target(
        &mut self,
        pr: &crate::rpg::program::Proto,
        args: &[Expr],
    ) -> Result<String> {
        let mut kinds = Vec::new();
        for a in args {
            kinds.push(self.arg_kind(a)?);
        }
        let prog = self.prog;
        for cand in &pr.overload {
            let params = match (prog.protos.get(cand), prog.procs.get(cand)) {
                (Some(p), _) => &p.params,
                (None, Some(p)) => &p.params,
                _ => bail!("OVERLOAD({cand}): {cand} is not a prototype or procedure"),
            };
            let required = params.iter().filter(|p| !p.nopass).count();
            if args.len() < required || args.len() > params.len() {
                continue;
            }
            let fits = params.iter().zip(&kinds).all(|(p, k)| {
                let want = match p.dtype {
                    DType::Date(_) => 'D',
                    DType::Time(_) => 'T',
                    DType::Timestamp => 'Z',
                    DType::Ind => 'I',
                    t if t.is_numeric() => 'N',
                    _ => 'C',
                };
                k.contains(&want)
            });
            if fits {
                return Ok(cand.clone());
            }
        }
        bail!(
            "No OVERLOAD candidate of {} takes these arguments.",
            pr.name
        )
    }

    /// The subfields of a data structure: (subfield, name to use).
    pub(super) fn ds_members(&self, ds: &str) -> Result<Vec<(String, String)>> {
        let Some(Sym::Ds(d)) = self.sym(ds) else {
            bail!("{ds} is not a data structure");
        };
        Ok(d.subfields
            .iter()
            .map(|sf| {
                let key = if d.qualified {
                    format!("{ds}.{sf}")
                } else {
                    sf.clone()
                };
                (sf.clone(), key)
            })
            .collect())
    }

    /// A record read into a result data structure: each field into the
    /// subfield of its name.
    pub(super) fn record_to_ds(&mut self, i: usize, rec: &Record, ds: &str) -> Result<()> {
        // A program-described file: the record image into the data
        // structure, byte for byte (ILE RPG Reference, result data
        // structure for a program-described file).
        if self.prog.files[i].program_described {
            let img = self.record_image(i, rec)?;
            let Some(Sym::Ds(d)) = self.sym(ds) else {
                bail!("{ds} is not a data structure");
            };
            let n = img.len().min(d.size);
            self.bufs[d.buf][d.off..d.off + n].copy_from_slice(&img[..n]);
            self.files[i].image = Some(img);
            return Ok(());
        }
        let members = self.ds_members(ds)?;
        let fi = self.prog.files[i].clone();
        let fref = fi
            .fref
            .as_ref()
            .ok_or_else(|| anyhow!("{} is not a database file", fi.decl.name))?;
        for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
            let Some((_, key)) = members.iter().find(|(sf, _)| sf == pname || *sf == fd.name)
            else {
                continue;
            };
            let Some(text) = rec.get(&fd.name) else {
                continue;
            };
            let v = if fd.data_type.is_numeric() {
                Value::Num(Dec::parse(text)?)
            } else {
                Value::str(text)
            };
            self.assign(
                &Expr::name(key),
                &v,
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    /// A record to write or update from a data structure's subfields.
    pub(super) fn ds_to_record(&mut self, i: usize, ds: &str) -> Result<Record> {
        if self.prog.files[i].program_described {
            let Some(Sym::Ds(d)) = self.sym(ds) else {
                bail!("{ds} is not a data structure");
            };
            let mut img = vec![data::BLANK; self.prog.files[i].record_len];
            let n = img.len().min(d.size);
            img[..n].copy_from_slice(&self.bufs[d.buf][d.off..d.off + n]);
            return self.image_record(i, &img);
        }
        let members = self.ds_members(ds)?;
        let fi = self.prog.files[i].clone();
        let fref = fi
            .fref
            .as_ref()
            .ok_or_else(|| anyhow!("{} is not a database file", fi.decl.name))?;
        let mut rec = Record::new();
        for (fd, pname) in fref.file.fields.iter().zip(&fi.fields) {
            let Some((_, key)) = members.iter().find(|(sf, _)| sf == pname || *sf == fd.name)
            else {
                continue;
            };
            let v = self.eval(&Expr::name(key))?;
            let text = match v {
                Value::Num(d) => d.rescale(fd.decimals as u8, false)?.to_string(),
                other => a(&other.chars()?),
            };
            rec.insert(fd.name.clone(), text);
        }
        Ok(rec)
    }

    /// Where each exported or imported definition is: (symbol, buffer,
    /// offset, length, exported).
    fn shared_places(&self) -> Vec<(String, usize, usize, usize, bool)> {
        let prog = self.prog;
        let mut out = Vec::new();
        for (list, exported) in [(&prog.data_exports, true), (&prog.data_imports, false)] {
            for (sym, name) in list {
                match prog.syms.get(name) {
                    Some(Sym::Var(v)) => out.push((
                        sym.clone(),
                        v.buf,
                        v.off,
                        v.stride * v.dim.max(1) as usize,
                        exported,
                    )),
                    Some(Sym::Ds(d)) => out.push((
                        sym.clone(),
                        d.buf,
                        d.off,
                        d.size * d.dim.max(1) as usize,
                        exported,
                    )),
                    _ => {}
                }
            }
        }
        out
    }

    /// Exported data: this module's values to the shared copy.
    pub(super) fn share_out(&mut self) {
        let places = self.shared_places();
        if places.is_empty() {
            return;
        }
        if let Ok(mut s) = self.io.shared.lock() {
            for (sym, buf, off, len, _) in places {
                s.insert(sym, self.bufs[buf][off..off + len].to_vec());
            }
        }
    }

    /// Exported data: the shared copy into this module's definitions.
    pub(super) fn share_in(&mut self) {
        let places = self.shared_places();
        if places.is_empty() {
            return;
        }
        let s = self.io.shared.lock().map(|s| s.clone()).unwrap_or_default();
        for (sym, buf, off, len, _) in places {
            if let Some(b) = s.get(&sym) {
                let n = len.min(b.len());
                self.bufs[buf][off..off + n].copy_from_slice(&b[..n]);
            }
        }
    }

    /// Exported data: this module's exports with their initial values,
    /// where nothing has set them yet.
    pub(super) fn share_seed(&mut self) {
        let places = self.shared_places();
        if let Ok(mut s) = self.io.shared.lock() {
            for (sym, buf, off, len, exported) in places {
                if exported && !s.contains_key(&sym) {
                    s.insert(sym, self.bufs[buf][off..off + len].to_vec());
                }
            }
        }
    }

    /// Exported data of the modules bound into the program: their initial
    /// values (the program's activation initializes all its modules).
    pub(super) fn seed_bound_modules(&mut self) -> Result<()> {
        let Some(b) = self.io.binding.clone() else {
            return Ok(());
        };
        let client = self.io.client;
        let libl = self.io.libl.clone();
        for m in &b.modules {
            let p = self.block(super::super::compile_with(
                client, &libl, &m.name, &m.source, &m.options,
            ))?;
            if p.data_exports.is_empty() {
                continue;
            }
            let storage = self
                .io
                .modules
                .lock()
                .ok()
                .and_then(|x| x.get(&format!("MODULE {}", m.name)).cloned())
                .unwrap_or_else(|| p.globals.clone());
            if let Ok(mut s) = self.io.shared.lock() {
                for (sym, name) in &p.data_exports {
                    let place = match p.syms.get(name) {
                        Some(Sym::Var(v)) => Some((v.buf, v.off, v.stride * v.dim.max(1) as usize)),
                        Some(Sym::Ds(d)) => Some((d.buf, d.off, d.size * d.dim.max(1) as usize)),
                        _ => None,
                    };
                    if let Some((buf, off, len)) = place {
                        s.entry(sym.clone())
                            .or_insert_with(|| storage[buf][off..off + len].to_vec());
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether a parameter was passed as *OMIT.
    fn omitted(&self, name: &str) -> bool {
        self.frames
            .last()
            .is_some_and(|f| f.omitted.iter().any(|n| n == name))
    }

    /// The elements in use of an array: all of them, or of a varying
    /// dimension array (DIM(*AUTO), DIM(*VAR)) its current number.
    pub(super) fn elems_in_use(&self, name: &str, v: &VarInfo) -> u32 {
        if self.prog.varying.contains_key(name) {
            self.var_elems.get(name).copied().unwrap_or(0)
        } else {
            v.dim
        }
    }

    /// Search a list from `start` for `n` elements: "" equal, LT/LE the
    /// nearest below (LE: equal first), GE/GT the nearest above (GE: equal
    /// first). The 1-based index, 0 when none.
    fn search(&self, x: &Value, all: &[Value], start: usize, n: usize, how: &str) -> Result<usize> {
        if start < 1 {
            bail!("RNQ0121: the start index {start} is not valid");
        }
        let mut best: Option<(usize, &Value)> = None;
        for (k, v) in all.iter().enumerate().skip(start - 1).take(n) {
            let o = data::compare(v, x)?;
            let ok = match how {
                "" => o == Ordering::Equal,
                "LT" => o == Ordering::Less,
                "LE" => o != Ordering::Greater,
                "GE" => o != Ordering::Less,
                _ => o == Ordering::Greater,
            };
            if !ok {
                continue;
            }
            if how.is_empty() || o == Ordering::Equal && (how == "LE" || how == "GE") {
                return Ok(k + 1);
            }
            let closer = match best {
                None => true,
                Some((_, bv)) => {
                    let c = data::compare(v, bv)?;
                    if matches!(how, "LT" | "LE") {
                        c == Ordering::Greater
                    } else {
                        c == Ordering::Less
                    }
                }
            };
            if closer {
                best = Some((k + 1, v));
            }
        }
        Ok(best.map(|b| b.0).unwrap_or(0))
    }
}

fn e_of(s: &str) -> Vec<u8> {
    data::e(s)
}
