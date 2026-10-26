//! Fixed-form operations on strings, arrays and bits (ILE RPG Reference,
//! "Operation Codes"): SCAN, CHECK, CHECKR, SUBST, XLATE, CAT, LOOKUP,
//! MOVEA, XFOOT, SQRT, SORTA, OCCUR, TESTN, TESTZ, TESTB, BITON, BITOFF
//! and the move-zone operations. Factors written `a:b` arrive as the
//! pseudo built-in `*PAIR` (see parse.rs).

use super::*;
use crate::rpg::parse::PAIR;

/// Factor parts: `a:b` → (a, Some(b)); `a` → (a, None).
fn split(e: &Option<Expr>) -> (Option<Expr>, Option<Expr>) {
    match e {
        Some(Expr::Bif(b, args)) if b == PAIR && args.len() == 2 => {
            let first = match &args[0] {
                Expr::Fig(Fig::Blanks) => None,
                x => Some(x.clone()),
            };
            (first, Some(args[1].clone()))
        }
        other => (other.clone(), None),
    }
}

/// The array an expression names (and the index written with it).
pub(super) struct ArrayRef {
    pub name: String,
    pub var: VarInfo,
    pub index: Option<Expr>,
}

impl Machine<'_> {
    fn need(&self, e: &Option<Expr>, code: &str, what: &str) -> Result<Expr> {
        e.clone().ok_or_else(|| anyhow!("{code}: {what} is needed"))
    }

    fn num_of(&mut self, e: &Expr) -> Result<i128> {
        self.eval(e)?.num()?.to_i128()
    }

    /// The bytes of a character value as the operation sees them: a
    /// field with its full length (trailing blanks count).
    fn chars_of(&mut self, e: &Expr) -> Result<Vec<u8>> {
        match self.eval(e)? {
            Value::Fig(f) => {
                let n = self.type_of(e).map(|t| t.size()).unwrap_or(1);
                let mut b = vec![data::BLANK; n];
                data::store(
                    &DType::Char(n as u32),
                    &Value::Fig(f),
                    &mut b,
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
                Ok(b)
            }
            Value::Num(_) => self.move_image(e),
            v => v.chars(),
        }
    }

    pub(super) fn array_ref(&self, e: &Expr) -> Option<ArrayRef> {
        let Expr::Name { parts, args } = e else {
            return None;
        };
        let name = parts.join(".");
        match self.sym(&name) {
            Some(Sym::Var(v)) if v.dim > 0 => Some(ArrayRef {
                name,
                var: v,
                index: args.as_ref().and_then(|a| a.first().cloned()),
            }),
            _ => None,
        }
    }

    /// A result that may be a numeric array: every position found (the
    /// rest set to zero), or the first one.
    fn put_positions(&mut self, result: &Option<Expr>, found: &[usize]) -> Result<()> {
        let Some(r) = result else { return Ok(()) };
        if let Some(arr) = self.array_ref(r).filter(|a| a.index.is_none()) {
            for k in 0..arr.var.dim as usize {
                let v = found.get(k).copied().unwrap_or(0);
                self.store_var(
                    &arr.var,
                    Some(k as i128 + 1),
                    &Value::Num(Dec::from_i64(v as i64)),
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
            }
        } else {
            let v = found.first().copied().unwrap_or(0);
            self.assign(
                r,
                &Value::Num(Dec::from_i64(v as i64)),
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    /// Put a character result left-adjusted: the rest of the result is
    /// kept, or blanked with the P extender.
    fn put_left(&mut self, result: &Expr, text: &[u8], pad: bool) -> Result<()> {
        let mut cur = self.chars_of(result)?;
        if pad {
            cur.fill(data::BLANK);
        }
        let n = text.len().min(cur.len());
        cur[..n].copy_from_slice(&text[..n]);
        self.assign(
            result,
            &Value::Char(cur),
            Fit::Truncate { half_adjust: false },
            false,
        )
    }

    /// An operation that failed: on with the error indicator (positions
    /// 73-74), otherwise the program ends with the message.
    fn op_error(&mut self, ind: &ResultInd, msg: String) -> Result<()> {
        match &ind.lo {
            Some(lo) => {
                self.set_ind(lo, true);
                Ok(())
            }
            None => Err(anyhow!(msg)),
        }
    }

    pub(super) fn string_op(
        &mut self,
        code: &str,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ext: &Ext,
        ind: &ResultInd,
    ) -> Result<bool> {
        let pad = ext.other.contains('P');
        match code {
            "SCAN" => {
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, false);
                }
                let (cmp, cmp_len) = split(f1);
                let cmp = self.chars_of(&self.need(&cmp, code, "a compare string")?)?;
                let len = match cmp_len {
                    Some(l) => self.num_of(&l)? as usize,
                    None => cmp.len(),
                };
                let (base, start) = split(f2);
                let base = self.chars_of(&self.need(&base, code, "a base string")?)?;
                let start = match start {
                    Some(s) => self.num_of(&s)?,
                    None => 1,
                };
                if len == 0 || len > cmp.len() || start < 1 || start as usize > base.len() {
                    self.op_error(
                        ind,
                        "RNQ0100: SCAN: the length or start position is not valid.".into(),
                    )?;
                    return Ok(true);
                }
                let needle = &cmp[..len];
                let mut found = Vec::new();
                let mut at = start as usize - 1;
                while at + len <= base.len() {
                    if &base[at..at + len] == needle {
                        found.push(at + 1);
                        if result.is_none() {
                            break;
                        }
                    }
                    at += 1;
                }
                self.put_positions(result, &found)?;
                if let Some(eq) = &ind.eq {
                    self.set_ind(eq, !found.is_empty());
                }
            }
            "CHECK" | "CHECKR" => {
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, false);
                }
                let comparator = self.chars_of(&self.need(f1, code, "a comparator string")?)?;
                let (base, start) = split(f2);
                let base = self.chars_of(&self.need(&base, code, "a base string")?)?;
                let start = match start {
                    Some(s) => self.num_of(&s)?,
                    None if code == "CHECK" => 1,
                    None => base.len() as i128,
                };
                if start < 1 || start as usize > base.len() {
                    self.op_error(
                        ind,
                        format!("RNQ0100: {code}: the start position is not valid."),
                    )?;
                    return Ok(true);
                }
                let s = start as usize - 1;
                let mut found = Vec::new();
                let multi = result
                    .as_ref()
                    .and_then(|r| self.array_ref(r))
                    .is_some_and(|a| a.index.is_none());
                if code == "CHECK" {
                    for (k, b) in base.iter().enumerate().skip(s) {
                        if !comparator.contains(b) {
                            found.push(k + 1);
                            if !multi {
                                break;
                            }
                        }
                    }
                } else {
                    for k in (0..=s).rev() {
                        if !comparator.contains(&base[k]) {
                            found.push(k + 1);
                            if !multi {
                                break;
                            }
                        }
                    }
                }
                self.put_positions(result, &found)?;
                if let Some(eq) = &ind.eq {
                    self.set_ind(eq, !found.is_empty());
                }
            }
            "SUBST" => {
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, false);
                }
                let (base, start) = split(f2);
                let base = self.chars_of(&self.need(&base, code, "a base string")?)?;
                let start = match start {
                    Some(s) => self.num_of(&s)?,
                    None => 1,
                };
                let len = match f1 {
                    Some(l) => self.num_of(l)?,
                    None => base.len() as i128 - start + 1,
                };
                if start < 1 || len < 1 || start - 1 + len > base.len() as i128 {
                    self.op_error(
                        ind,
                        "RNQ0100: SUBST: the start position or length is outside the string."
                            .into(),
                    )?;
                    return Ok(true);
                }
                let piece = base[start as usize - 1..(start + len - 1) as usize].to_vec();
                let r = self.need(result, code, "a result field")?;
                self.put_left(&r, &piece, pad)?;
            }
            "XLATE" => {
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, false);
                }
                let (from, to) = split(f1);
                let from = self.chars_of(&self.need(&from, code, "the from string")?)?;
                let to = self.chars_of(&self.need(&to, code, "the to string")?)?;
                let (base, start) = split(f2);
                let mut text = self.chars_of(&self.need(&base, code, "a string")?)?;
                let start = match start {
                    Some(s) => self.num_of(&s)?,
                    None => 1,
                };
                if start < 1 || start as usize > text.len() {
                    self.op_error(
                        ind,
                        "RNQ0100: XLATE: the start position is not valid.".into(),
                    )?;
                    return Ok(true);
                }
                for b in text.iter_mut().skip(start as usize - 1) {
                    if let Some(p) = from.iter().position(|f| f == b) {
                        if let Some(t) = to.get(p) {
                            *b = *t;
                        }
                    }
                }
                let r = self.need(result, code, "a result field")?;
                self.put_left(&r, &text, pad)?;
            }
            "CAT" => {
                let r = self.need(result, code, "a result field")?;
                let mut s = match f1 {
                    Some(x) => self.chars_of(x)?,
                    None => self.chars_of(&r)?,
                };
                let (text, blanks) = split(f2);
                let text = self.chars_of(&self.need(&text, code, "factor 2")?)?;
                // With a number of blanks, factor 1's trailing blanks go
                // and that many blanks come between; without it, both
                // factors are joined as they are (ILE RPG Reference, CAT).
                if let Some(n) = blanks {
                    let n = self.num_of(&n)? as usize;
                    while s.last() == Some(&data::BLANK) {
                        s.pop();
                    }
                    s.extend(std::iter::repeat_n(data::BLANK, n));
                }
                s.extend(text);
                self.put_left(&r, &s, pad)?;
            }
            "TEST" => {
                // TEST(D), TEST(T), TEST(Z): is the character or numeric
                // field a valid date, time or timestamp in the format of
                // factor 1 (ILE RPG Reference, TEST). An error sets %ERROR
                // (E) or the indicator in positions 73-74.
                let r = self.need(result, code, "a field")?;
                let fmt = match f1 {
                    Some(Expr::Name { parts, .. }) => parts.join("."),
                    _ => "*ISO".into(),
                };
                let v = self.eval(&r)?;
                let text = match &v {
                    Value::Num(d) => d.trunc().mantissa().to_string(),
                    other => a(&other.chars()?),
                };
                let numeric = matches!(v, Value::Num(_));
                let fmt0 = |f: &str| {
                    if numeric {
                        format!("{}0", f.trim_end_matches('0'))
                    } else {
                        f.to_string()
                    }
                };
                let o = ext.other.as_str();
                let ok = if o.contains('Z') {
                    crate::rpg::dates::parse_ts(&text).is_ok()
                } else if o.contains('T') {
                    crate::rpg::dates::TimeFmt::parse(&fmt0(&fmt))
                        .and_then(|f| crate::rpg::dates::parse_time(&text, &f))
                        .is_ok()
                } else {
                    crate::rpg::dates::DateFmt::parse(&fmt0(&fmt))
                        .and_then(|f| crate::rpg::dates::parse_date(&text, &f))
                        .is_ok()
                };
                if ext.error {
                    self.last = Status {
                        error: !ok,
                        ..Status::default()
                    };
                }
                if let Some(lo) = &ind.lo {
                    self.set_ind(lo, !ok);
                }
                if !ok && !ext.error && ind.lo.is_none() {
                    bail!("RNX0112: {text} is not a valid value for {fmt}.");
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(super) fn array_op(
        &mut self,
        code: &str,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ext: &Ext,
        ind: &ResultInd,
    ) -> Result<bool> {
        match code {
            "LOOKUP" => self.lookup(f1, f2, result, ind)?,
            "XFOOT" => {
                let a = self.need(f2, code, "an array")?;
                let arr = self
                    .array_ref(&a)
                    .ok_or_else(|| anyhow!("XFOOT: factor 2 must be an array"))?;
                let mut sum = Dec::ZERO;
                for k in 1..=arr.var.dim as i128 {
                    sum = sum.add(&self.load_var(&arr.var, Some(k))?.num()?)?;
                }
                let r = self.need(result, code, "a result field")?;
                self.assign(
                    &r,
                    &Value::Num(sum),
                    Fit::Truncate {
                        half_adjust: ext.half_adjust,
                    },
                    false,
                )?;
                let stored = self.eval(&r)?.num()?;
                self.num_inds(ind, &stored);
            }
            "SQRT" => {
                let x = self.eval(&self.need(f2, code, "a value")?)?.num()?;
                if x.is_negative() {
                    bail!("RNQ0101: SQRT of a negative number.");
                }
                let r = self.need(result, code, "a result field")?;
                let (_, decs) = self.type_of(&r).map(|t| t.precision()).unwrap_or((0, 0));
                let v = dec_sqrt(&x, decs as u8 + 1)?;
                self.assign(
                    &r,
                    &Value::Num(v),
                    Fit::Truncate {
                        half_adjust: ext.half_adjust,
                    },
                    false,
                )?;
            }
            "SORTA" => {
                let a = self.need(f2, code, "an array")?;
                self.sorta(&a)?;
            }
            "EVAL-CORR" => {
                let t = self.need(f1, code, "a target data structure")?;
                let s = self.need(f2, code, "a source data structure")?;
                self.eval_corr(&t, &s, ext.half_adjust)?;
            }
            "MOVEA" => {
                let from = self.need(f2, code, "factor 2")?;
                let to = self.need(result, code, "a result field")?;
                self.movea(&from, &to, ext.other.contains('P'))?;
            }
            "OCCUR" => self.occur_op(f1, f2, result, ind)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// EVAL-CORR (ILE RPG Reference, EVAL-CORR): each subfield of the
    /// target with a subfield of the same name in the source gets its
    /// value; subfields that are data structures in both correspond
    /// subfield by subfield; arrays element by element, as many as the
    /// smaller has; subfields whose types do not go together (character
    /// and numeric...) are left as they are.
    pub(super) fn eval_corr(&mut self, t: &Expr, s: &Expr, half_adjust: bool) -> Result<()> {
        let ds = |me: &Self, e: &Expr| -> Option<(String, crate::rpg::program::DsInfo)> {
            let parts = match e {
                Expr::Name { parts, .. } | Expr::Path { parts, .. } => parts,
                _ => return None,
            };
            let key = parts.join(".");
            match me.sym_static(&key) {
                Some(Sym::Ds(d)) => Some((key, d)),
                _ => None,
            }
        };
        let (Some((_, td)), Some((_, sd))) = (ds(self, t), ds(self, s)) else {
            bail!("EVAL-CORR: the target and the source must be data structures");
        };
        // The expression of a subfield of a data structure expression.
        let sub = |e: &Expr, d: &crate::rpg::program::DsInfo, name: &str| -> Expr {
            match e {
                Expr::Name { parts, args } if !d.qualified && args.is_none() && parts.len() == 1 => {
                    Expr::name(name)
                }
                Expr::Name { parts, args } => {
                    let mut p = parts.clone();
                    p.push(name.to_string());
                    let mut idx: Vec<Option<Box<Expr>>> = vec![None; p.len()];
                    if let Some(a) = args.as_ref().and_then(|a| a.first()) {
                        idx[parts.len() - 1] = Some(Box::new(a.clone()));
                    }
                    if args.is_some() {
                        Expr::Path { parts: p, idx, args: None }
                    } else {
                        Expr::Name { parts: p, args: None }
                    }
                }
                Expr::Path { parts, idx, args } => {
                    let mut p = parts.clone();
                    p.push(name.to_string());
                    let mut i = idx.clone();
                    if let Some(a) = args.as_ref().and_then(|a| a.first()) {
                        if let Some(last) = i.last_mut() {
                            *last = Some(Box::new(a.clone()));
                        }
                    }
                    i.push(None);
                    Expr::Path { parts: p, idx: i, args: None }
                }
                other => other.clone(),
            }
        };
        let class = |t: &DType| match t {
            DType::Char(_) | DType::Varchar(_) => 1,
            DType::Ucs2(_) | DType::VarUcs2(_) => 2,
            DType::Date(_) => 3,
            DType::Time(_) => 4,
            DType::Timestamp(_) => 5,
            DType::Ind => 6,
            DType::Pointer => 7,
            _ => 0,
        };
        for name in td.subfields.clone() {
            let short = name.rsplit('.').next().unwrap_or(&name).to_string();
            if !sd
                .subfields
                .iter()
                .any(|x| x.rsplit('.').next().unwrap_or(x) == short)
            {
                continue;
            }
            let te = sub(t, &td, &short);
            let se = sub(s, &sd, &short);
            // Data structures in both: their own corresponding subfields.
            if ds(self, &te).is_some() && ds(self, &se).is_some() {
                self.eval_corr(&te, &se, half_adjust)?;
                continue;
            }
            let (Some(tt), Some(st)) = (self.type_of(&te), self.type_of(&se)) else {
                continue;
            };
            if class(&tt) != class(&st) {
                continue;
            }
            let fit = Fit::Strict { half_adjust };
            let (ta, sa) = (self.array_ref(&te), self.array_ref(&se));
            match (ta, sa) {
                (Some(ta), Some(sa)) if ta.index.is_none() && sa.index.is_none() => {
                    let n = ta.var.dim.min(sa.var.dim) as i128;
                    for k in 1..=n {
                        let v = self.load_var(&sa.var, Some(k))?;
                        let idx = Expr::Num(crate::rpg::dec::Dec::from_i64(k as i64));
                        let with = |e: &Expr| match e {
                            Expr::Name { parts, .. } => Expr::Name {
                                parts: parts.clone(),
                                args: Some(vec![idx.clone()]),
                            },
                            Expr::Path { parts, idx: ix, .. } => Expr::Path {
                                parts: parts.clone(),
                                idx: ix.clone(),
                                args: Some(vec![idx.clone()]),
                            },
                            o => o.clone(),
                        };
                        self.assign(&with(&te), &v, fit, false)?;
                    }
                }
                (Some(_), _) | (_, Some(_)) => {}
                _ => {
                    let v = self.eval(&se)?;
                    self.assign(&te, &v, fit, false)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn sorta(&mut self, a: &Expr) -> Result<()> {
        let arr = self
            .array_ref(a)
            .ok_or_else(|| anyhow!("SORTA: an array is needed"))?;
        let mut vals = Vec::new();
        for k in 1..=self.elems_in_use(&arr.name, &arr.var) as i128 {
            vals.push(self.load_var(&arr.var, Some(k))?);
        }
        let mut err = None;
        vals.sort_by(|x, y| {
            data::compare(x, y).unwrap_or_else(|e| {
                err = Some(e);
                Ordering::Equal
            })
        });
        if let Some(e) = err {
            return Err(e);
        }
        if self.prog.sequence.get(&arr.name) == Some(&'D') {
            vals.reverse();
        }
        // An array overlaying an array of a data structure: the sort
        // rearranges whole elements of the overlaid array, by this key
        // (ILE RPG Reference, SORTA).
        let v = arr.var.clone();
        if v.stride != v.dtype.size() || v.lead > 0 {
            let n = self.elems_in_use(&arr.name, &v) as usize;
            let mut keyed: Vec<(Value, Vec<u8>)> = Vec::new();
            for k in 0..n {
                let key = self.load_var(&v, Some(k as i128 + 1))?;
                let start = v.off + k * v.stride - v.lead;
                keyed.push((key, self.bufs[v.buf][start..start + v.stride].to_vec()));
            }
            let mut err = None;
            keyed.sort_by(|x, y| {
                data::compare(&x.0, &y.0).unwrap_or_else(|e| {
                    err = Some(e);
                    Ordering::Equal
                })
            });
            if let Some(e) = err {
                return Err(e);
            }
            if self.prog.sequence.get(&arr.name) == Some(&'D') {
                keyed.reverse();
            }
            for (k, (_, bytes)) in keyed.into_iter().enumerate() {
                let start = v.off + k * v.stride - v.lead;
                self.bufs[v.buf][start..start + v.stride].copy_from_slice(&bytes);
            }
            return Ok(());
        }
        for (k, v) in vals.iter().enumerate() {
            self.store_var(
                &arr.var,
                Some(k as i128 + 1),
                v,
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    /// LOOKUP (ILE RPG Reference): search an array (from the element its
    /// index names, which is set to the element found, or to 1 when none
    /// is) or a table (whose current element becomes the one found, and
    /// that of the related table in the result field).
    fn lookup(
        &mut self,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ind: &ResultInd,
    ) -> Result<()> {
        let arg = self.eval(&self.need(f1, "LOOKUP", "a search argument")?)?;
        let a = self.need(f2, "LOOKUP", "an array or table")?;
        let arr = self
            .array_ref(&a)
            .ok_or_else(|| anyhow!("LOOKUP: factor 2 must be an array or table"))?;
        let table = arr.name.starts_with("TAB");
        let start = match &arr.index {
            Some(ix) => {
                let s = self.num_of(ix)?;
                if s < 1 || s > arr.var.dim as i128 {
                    bail!(
                        "RNQ0121: An array index is out of range (index {s}, {} elements).",
                        arr.var.dim
                    );
                }
                s as usize
            }
            None => 1,
        };
        let want_eq = ind.eq.is_some();
        let want_hi = ind.hi.is_some();
        let want_lo = ind.lo.is_some();
        if !(want_eq || want_hi || want_lo) {
            bail!("LOOKUP needs a resulting indicator");
        }
        let mut found: Option<(usize, Ordering)> = None;
        let mut best: Option<(usize, Value)> = None;
        for k in start..=arr.var.dim as usize {
            let v = self.load_var(&arr.var, Some(k as i128))?;
            let o = data::compare(&v, &arg)?;
            if o == Ordering::Equal && want_eq {
                found = Some((k, o));
                break;
            }
            // The element nearest above (HI) or below (LO) the argument.
            let fits = (o == Ordering::Greater && want_hi) || (o == Ordering::Less && want_lo);
            if fits {
                let closer = match &best {
                    None => true,
                    Some((_, b)) => {
                        let c = data::compare(&v, b)?;
                        if o == Ordering::Greater {
                            c == Ordering::Less
                        } else {
                            c == Ordering::Greater
                        }
                    }
                };
                if closer {
                    best = Some((k, v));
                }
            }
        }
        if found.is_none() {
            if let Some((k, v)) = best {
                found = Some((k, data::compare(&v, &arg)?));
            }
        }
        for (i, on) in [
            (&ind.hi, matches!(found, Some((_, Ordering::Greater)))),
            (&ind.lo, matches!(found, Some((_, Ordering::Less)))),
            (&ind.eq, matches!(found, Some((_, Ordering::Equal)))),
        ] {
            if let Some(i) = i {
                self.set_ind(i, on);
            }
        }
        if table {
            if let Some((k, _)) = found {
                self.table_cur.insert(arr.name.clone(), k as i128);
                if let Some(r) = result {
                    if let Some(alt) = self.array_ref(r) {
                        self.table_cur.insert(alt.name, k as i128);
                    }
                }
            }
        } else if let Some(ix) = &arr.index {
            let at = found.map(|(k, _)| k).unwrap_or(1);
            self.assign(
                ix,
                &Value::Num(Dec::from_i64(at as i64)),
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    /// The bytes of each element (or of a field) for MOVEA: character
    /// data as is, numbers as zoned digits.
    fn movea_parts(&mut self, e: &Expr) -> Result<(Vec<Vec<u8>>, Option<ArrayRef>, usize)> {
        if let Some(arr) = self.array_ref(e) {
            let first = match &arr.index {
                Some(ix) => self.num_of(ix)? as usize,
                None => 1,
            };
            if first < 1 || first > arr.var.dim as usize {
                bail!(
                    "RNQ0121: An array index is out of range (index {first}, {} elements).",
                    arr.var.dim
                );
            }
            let mut out = Vec::new();
            for k in first..=arr.var.dim as usize {
                let el = Expr::Name {
                    parts: vec![arr.name.clone()],
                    args: Some(vec![Expr::Num(Dec::from_i64(k as i64))]),
                };
                out.push(self.move_image(&el)?);
            }
            Ok((out, Some(arr), first))
        } else {
            Ok((vec![self.move_image(e)?], None, 1))
        }
    }

    /// MOVEA: bytes from factor 2 (an array from its index, or a field)
    /// into the result (the same), left to right, until one runs out; P
    /// blanks (or zeros) the rest of the result (ILE RPG Reference).
    fn movea(&mut self, from: &Expr, to: &Expr, pad: bool) -> Result<()> {
        let (src, _, _) = self.movea_parts(from)?;
        let src: Vec<u8> = src.concat();
        let (dst, darr, first) = self.movea_parts(to)?;
        let numeric = self.type_of(to).is_some_and(|t| t.is_numeric());
        let mut flat: Vec<u8> = dst.concat();
        if pad {
            flat.fill(if numeric { 0xF0 } else { data::BLANK });
        }
        let n = src.len().min(flat.len());
        flat[..n].copy_from_slice(&src[..n]);
        let mut at = 0;
        for (k, part) in dst.iter().enumerate() {
            let bytes = flat[at..at + part.len()].to_vec();
            at += part.len();
            let target = match &darr {
                Some(arr) => Expr::Name {
                    parts: vec![arr.name.clone()],
                    args: Some(vec![Expr::Num(Dec::from_i64((first + k) as i64))]),
                },
                None => to.clone(),
            };
            let val = if numeric {
                let (_, decs) = self
                    .type_of(&target)
                    .map(|t| t.precision())
                    .unwrap_or((0, 0));
                let digits: Vec<u8> = bytes
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let z = if i + 1 == bytes.len() && b >> 4 == 0xD {
                            0xD0
                        } else {
                            0xF0
                        };
                        z | (b & 0x0F).min(9)
                    })
                    .collect();
                Value::Num(data::decode_num(
                    &DType::Zoned(digits.len() as u32, decs),
                    &digits,
                )?)
            } else {
                Value::Char(bytes)
            };
            self.assign(&target, &val, Fit::Truncate { half_adjust: false }, false)?;
        }
        Ok(())
    }

    /// The multiple-occurrence data structure a name refers to.
    fn occurs_of(&self, e: &Expr) -> Result<usize> {
        let name = match e {
            Expr::Name { parts, args: None } => parts.join("."),
            other => bail!("a data structure name is needed, found {other:?}"),
        };
        let Some(Sym::Ds(d)) = self.prog.syms.get(&name) else {
            bail!("{name} is not a data structure");
        };
        if !self.occur.contains_key(&d.buf) {
            bail!("{name} is not a multiple-occurrence data structure");
        }
        Ok(d.buf)
    }

    /// The current occurrence of a multiple-occurrence data structure.
    pub(super) fn occurrence(&self, e: &Expr) -> Result<u32> {
        let buf = self.occurs_of(e)?;
        Ok(self.occur[&buf].2)
    }

    pub(super) fn set_occurrence(&mut self, e: &Expr, n: i128) -> Result<()> {
        let buf = self.occurs_of(e)?;
        let (_, count, cur) = self.occur.get_mut(&buf).unwrap();
        if n < 1 || n > *count as i128 {
            bail!(
                "RNQ0122: The occurrence {n} is outside the data structure ({count} occurrences)."
            );
        }
        *cur = n as u32;
        Ok(())
    }

    /// OCCUR: factor 1 sets the occurrence (a number, or another
    /// multiple-occurrence data structure's current one); the result gets
    /// the current occurrence (ILE RPG Reference, OCCUR).
    fn occur_op(
        &mut self,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ind: &ResultInd,
    ) -> Result<()> {
        let ds = self.need(f2, "OCCUR", "a data structure")?;
        if let Some(v) = f1 {
            let n = match v {
                Expr::Name { parts, args: None }
                    if matches!(self.prog.syms.get(&parts.join(".")), Some(Sym::Ds(_))) =>
                {
                    self.occurrence(v)? as i128
                }
                other => self.num_of(other)?,
            };
            if let Err(e) = self.set_occurrence(&ds, n) {
                return self.op_error(ind, e.to_string());
            }
        }
        if let Some(r) = result {
            let n = self.occurrence(&ds)?;
            self.assign(
                r,
                &Value::Num(Dec::from_i64(n as i64)),
                Fit::Strict { half_adjust: false },
                false,
            )?;
        }
        Ok(())
    }

    pub(super) fn bit_op(
        &mut self,
        code: &str,
        f1: &Option<Expr>,
        f2: &Option<Expr>,
        result: &Option<Expr>,
        ind: &ResultInd,
    ) -> Result<bool> {
        let _ = f1;
        match code {
            "TESTN" => {
                let r = self.need(result, code, "a character field")?;
                let b = self.chars_of(&r)?;
                let digit = |c: u8, last: bool| {
                    (c & 0x0F) <= 9 && (c >> 4 == 0xF || (last && (c >> 4 == 0xC || c >> 4 == 0xD)))
                };
                let lead = b.iter().take_while(|c| **c == data::BLANK).count();
                let all_blank = lead == b.len();
                let rest = &b[lead..];
                let numeric = !rest.is_empty()
                    && rest
                        .iter()
                        .enumerate()
                        .all(|(i, c)| digit(*c, i + 1 == rest.len()));
                self.set_result_inds(ind, numeric && lead == 0, numeric && lead > 0, all_blank);
            }
            "TESTZ" => {
                let r = self.need(result, code, "a character field")?;
                let b = self.chars_of(&r)?;
                let c = b.first().copied().unwrap_or(data::BLANK);
                // '&' and A-I (zone C) high; '-' and J-R (zone D) low.
                let hi = c == 0x50 || c >> 4 == 0xC;
                let lo = c == 0x60 || c >> 4 == 0xD;
                self.set_result_inds(ind, hi, lo, !hi && !lo);
            }
            "TESTB" | "BITON" | "BITOFF" => {
                let r = self.need(result, code, "a one-byte character field")?;
                let mask = self.bit_mask(&self.need(f2, code, "the bits")?)?;
                let mut b = self.chars_of(&r)?;
                if b.len() != 1 {
                    bail!("{code}: the result field must be one byte long");
                }
                match code {
                    "BITON" => b[0] |= mask,
                    "BITOFF" => b[0] &= !mask,
                    _ => {
                        let on = b[0] & mask;
                        self.set_result_inds(
                            ind,
                            on == 0,
                            on != 0 && on != mask,
                            on == mask && mask != 0,
                        );
                        return Ok(true);
                    }
                }
                self.assign(
                    &r,
                    &Value::Char(b),
                    Fit::Strict { half_adjust: false },
                    false,
                )?;
            }
            "MHHZO" | "MHLZO" | "MLHZO" | "MLLZO" => {
                let from = self.move_image(&self.need(f2, code, "factor 2")?)?;
                let r = self.need(result, code, "a result field")?;
                let mut to = self.move_image(&r)?;
                let src = if code.as_bytes()[1] == b'H' {
                    from[0]
                } else {
                    from[from.len() - 1]
                };
                let at = if code.as_bytes()[2] == b'H' {
                    0
                } else {
                    to.len() - 1
                };
                to[at] = (src & 0xF0) | (to[at] & 0x0F);
                let val = match self.type_of(&r) {
                    Some(t) if t.is_numeric() => {
                        let (n, d) = t.precision();
                        Value::Num(data::decode_num(&DType::Zoned(n, d), &to)?)
                    }
                    _ => Value::Char(to),
                };
                self.assign(&r, &val, Fit::Truncate { half_adjust: false }, false)?;
            }
            "SHTDN" => {
                // The system is never shutting down here.
                if let Some(hi) = &ind.hi {
                    self.set_ind(hi, false);
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The bits of TESTB, BITON and BITOFF: a literal of bit numbers
    /// ('0134', 0 the leftmost), or a one-byte field whose bits are on.
    fn bit_mask(&mut self, e: &Expr) -> Result<u8> {
        match e {
            Expr::Str(s) => {
                let mut m = 0u8;
                for c in a(s).chars() {
                    let n = c
                        .to_digit(10)
                        .filter(|n| *n <= 7)
                        .ok_or_else(|| anyhow!("bit number {c} is not 0-7"))?;
                    m |= 0x80 >> n;
                }
                Ok(m)
            }
            other => {
                let b = self.chars_of(other)?;
                Ok(b.first().copied().unwrap_or(0))
            }
        }
    }
}

/// The square root of a non-negative decimal with `scale` decimal places,
/// truncated.
pub(super) fn dec_sqrt(x: &Dec, scale: u8) -> Result<Dec> {
    // sqrt(m / 10^s) = sqrt(m * 10^(2k - s)) / 10^k.
    let s = x.scale() as i32;
    let k = scale as i32;
    let e = 2 * k - s;
    let mut m =
        u128::try_from(x.mantissa()).map_err(|_| anyhow!("SQRT: the value is too large"))?;
    if e >= 0 {
        for _ in 0..e {
            m = m
                .checked_mul(10)
                .ok_or_else(|| anyhow!("SQRT: the value is too large"))?;
        }
    } else {
        for _ in 0..(-e) {
            m /= 10;
        }
    }
    // Integer square root (Newton).
    let r = if m < 2 {
        m
    } else {
        let mut x0 = m;
        let mut x1 = (x0 + m / x0) / 2;
        while x1 < x0 {
            x0 = x1;
            x1 = (x0 + m / x0) / 2;
        }
        x0
    };
    Ok(Dec::new(r as i128, scale))
}
