//! The RPG program cycle and input specifications (ILE RPG Reference,
//! "Program Cycle", "Detailed RPG Program Cycle", "Input
//! Specifications"): primary and secondary files, record identifying
//! indicators, control levels L1-L9, matching fields M1-M9 and MR, field
//! indicators; record images of program-described files.

use super::*;
use crate::rpg::program::InputRec;

/// An input record type and its fields' values: (input record, [(program
/// field, value)]).
type InputValues = (usize, Vec<(String, Value)>);

/// A record's control field values by level, and its matching value.
type ControlValues = (Vec<(u8, String)>, Option<String>);

/// A record read ahead by the cycle and not processed yet.
pub(super) struct Ahead {
    pub record: Record,
}

impl Machine<'_> {
    /// A database record as a program-described file sees it: each field
    /// in its stored form, one after another.
    pub(super) fn record_image(&self, i: usize, rec: &Record) -> Result<Vec<u8>> {
        let fi = &self.prog.files[i];
        let fref = fi.fref.as_ref().unwrap();
        let mut img = vec![data::BLANK; fi.record_len];
        for (fd, (t, off)) in fref.file.fields.iter().zip(&fi.layout) {
            let Some(text) = rec.get(&fd.name) else {
                continue;
            };
            let v = if t.is_numeric() {
                Value::Num(Dec::parse(text)?)
            } else {
                Value::str(text)
            };
            data::store(
                t,
                &v,
                &mut img[*off..*off + t.size()],
                Fit::Truncate { half_adjust: false },
                false,
            )?;
        }
        Ok(img)
    }

    /// A record image back into the database record's fields.
    pub(super) fn image_record(&self, i: usize, img: &[u8]) -> Result<Record> {
        let fi = &self.prog.files[i];
        let fref = fi.fref.as_ref().unwrap();
        let mut rec = Record::new();
        for (fd, (t, off)) in fref.file.fields.iter().zip(&fi.layout) {
            let v = data::load(t, &img[*off..*off + t.size()])?;
            let text = match v {
                Value::Num(d) => d.rescale(fd.decimals as u8, false)?.to_string(),
                other => a(&other.chars()?),
            };
            rec.insert(fd.name.clone(), text);
        }
        Ok(rec)
    }

    /// Which record type of the input specifications a record image is:
    /// the first whose record identification codes it meets.
    fn record_type(&self, i: usize, img: &[u8]) -> Option<usize> {
        let fi = &self.prog.files[i];
        fi.inputs.iter().position(|r| {
            r.codes.is_empty()
                || r.codes.iter().any(|group| {
                    group.iter().all(|c| {
                        let b = img
                            .get(c.position as usize - 1)
                            .copied()
                            .unwrap_or(data::BLANK);
                        let want = data::e(&c.ch.to_string())[0];
                        let ok = match c.part {
                            'Z' => b >> 4 == want >> 4,
                            'D' => b & 0x0F == want & 0x0F,
                            _ => b == want,
                        };
                        ok != c.not
                    })
                })
        })
    }

    /// The input record (type) of a record read from a file, and the
    /// value of each of its fields as the input specifications see it:
    /// (input record, [(program field, raw bytes or text, value)]).
    fn input_of(&self, i: usize, rec: &Record) -> Result<Option<InputValues>> {
        let fi = &self.prog.files[i];
        if fi.program_described {
            let img = self.record_image(i, rec)?;
            let Some(k) = self.record_type(i, &img) else {
                bail!(
                    "RNQ1299: A record of file {} matches no record identification on the input specifications.",
                    fi.decl.name
                );
            };
            let mut vals = Vec::new();
            for f in &fi.inputs[k].fields {
                let t = f.dtype.unwrap();
                if f.from == 0 || f.to > img.len() {
                    bail!(
                        "input field {} (positions {}-{}) is outside the record of {} ({} bytes)",
                        f.name,
                        f.from,
                        f.to,
                        fi.decl.name,
                        img.len()
                    );
                }
                let bytes = &img[f.from - 1..f.to];
                let v = if t.is_numeric() {
                    // Blanks in a zoned field read as zeros.
                    let fixed: Vec<u8> = if matches!(t, DType::Zoned(..)) {
                        bytes
                            .iter()
                            .map(|b| if *b == data::BLANK { 0xF0 } else { *b })
                            .collect()
                    } else {
                        bytes.to_vec()
                    };
                    Value::Num(data::decode_num(&t, &fixed)?)
                } else {
                    data::load(&t, bytes)?
                };
                vals.push((f.name.clone(), v));
            }
            return Ok(Some((k, vals)));
        }
        if fi.inputs.is_empty() {
            return Ok(None);
        }
        // Externally described: the record format's input specifications;
        // the values are the record's fields.
        let fref = fi.fref.as_ref().unwrap();
        let mut vals = Vec::new();
        for f in &fi.inputs[0].fields {
            if let Some(pos) = fi.fields.iter().position(|p| *p == f.name) {
                let fd = &fref.file.fields[pos];
                if let Some(text) = rec.get(&fd.name) {
                    let v = if fd.data_type.is_numeric() {
                        Value::Num(Dec::parse(text)?)
                    } else {
                        Value::str(text)
                    };
                    vals.push((f.name.clone(), v));
                }
            }
        }
        Ok(Some((0, vals)))
    }

    /// Move a record read into the program: its fields, its record
    /// identifying indicator (on), its field indicators.
    pub(super) fn move_input(&mut self, i: usize, rec: &Record) -> Result<()> {
        let input = self.input_of(i, rec)?;
        if !self.prog.files[i].program_described {
            self.fill_fields(i, rec)?;
        }
        let Some((k, vals)) = input else {
            return Ok(());
        };
        let ir: &InputRec = &self.prog.files[i].inputs[k];
        if self.prog.files[i].program_described {
            self.files[i].image = Some(self.record_image(i, rec)?);
            for (name, v) in &vals {
                self.assign(
                    &Expr::name(name),
                    v,
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
            }
        }
        if let Some(ri) = &ir.record_ind {
            self.set_ind(ri, true);
        }
        // Field indicators: plus, minus, zero or blank.
        for f in &ir.fields {
            if f.plus.is_none() && f.minus.is_none() && f.zero.is_none() {
                continue;
            }
            let Some((_, v)) = vals.iter().find(|(n, _)| *n == f.name) else {
                continue;
            };
            let (pos, neg, zero) = match v {
                Value::Num(d) => (d.mantissa() > 0, d.mantissa() < 0, d.is_zero()),
                other => {
                    let c = other.chars()?;
                    (false, false, c.iter().all(|b| *b == data::BLANK))
                }
            };
            for (ind, on) in [(&f.plus, pos), (&f.minus, neg), (&f.zero, zero)] {
                if let Some(ind) = ind {
                    self.set_ind(ind, on);
                }
            }
        }
        Ok(())
    }

    /// The values of a record's control fields, by level (1-9), and of
    /// its matching fields (M1-M9, M9 first), as the cycle compares them.
    fn control_and_match(&self, i: usize, rec: &Record) -> Result<ControlValues> {
        let Some((k, vals)) = self.input_of(i, rec)? else {
            return Ok((vec![], None));
        };
        let ir = &self.prog.files[i].inputs[k];
        let key = |v: &Value| -> Result<String> {
            Ok(match v {
                Value::Num(d) => format!("{:+040}", d.mantissa()),
                other => a(&other.chars()?),
            })
        };
        let mut levels: Vec<(u8, String)> = Vec::new();
        let mut matching: Vec<(u8, String)> = Vec::new();
        for f in &ir.fields {
            let Some((_, v)) = vals.iter().find(|(n, _)| *n == f.name) else {
                continue;
            };
            if let Some(l) = f.level.as_deref().and_then(|l| l[1..].parse::<u8>().ok()) {
                let s = key(v)?;
                match levels.iter_mut().find(|x| x.0 == l) {
                    Some(x) => x.1.push_str(&s),
                    None => levels.push((l, s)),
                }
            }
            if let Some(m) = f
                .matching
                .as_deref()
                .and_then(|l| l[1..].parse::<u8>().ok())
            {
                let s = key(v)?;
                match matching.iter_mut().find(|x| x.0 == m) {
                    Some(x) => x.1.push_str(&s),
                    None => matching.push((m, s)),
                }
            }
        }
        matching.sort_by_key(|x| std::cmp::Reverse(x.0));
        let m = if matching.is_empty() {
            None
        } else {
            Some(matching.into_iter().map(|x| x.1).collect::<String>())
        };
        Ok((levels, m))
    }

    /// The next record of a cycle file (primary or secondary), read ahead.
    fn cycle_read(&mut self, i: usize) -> Result<()> {
        if self.ahead[i].is_some() || self.cycle_eof[i] {
            return Ok(());
        }
        let client = self.io.client;
        let job = self.io.job_number;
        let lock = self.prog.files[i].decl.usage == 'U';
        let rt = self.io.rt.clone();
        let f = self.files[i].open.as_mut().ok_or_else(|| {
            anyhow!(
                "CPF4135: File {} is not open.",
                self.prog.files[i].decl.name
            )
        })?;
        let r = rt.block_on(f.read(client))?;
        let r = rt.block_on(f.lock_after_read(client, r, lock, job))?;
        match r.record {
            Some(rec) => self.ahead[i] = Some(Ahead { record: rec }),
            None => self.cycle_eof[i] = true,
        }
        Ok(())
    }

    /// The RPG cycle (ILE RPG Reference, "Detailed RPG Program Cycle"):
    /// first-cycle (1P) output; then each cycle: heading and detail
    /// output, record identifying and control level indicators off, the
    /// next record (primary first, then secondary files; with matching
    /// fields, the file whose record is lowest, MR on when records of
    /// several files match), control breaks (a change in a control field
    /// sets on its level and those below; the first record sets them on
    /// without total time), total calculations and output, end at LR
    /// (end of file: LR and L1-L9 on), fields moved into the program,
    /// field indicators, detail calculations.
    pub(super) fn cycle(&mut self) -> Result<()> {
        let prog = self.prog;
        let cycle_files: Vec<usize> = prog
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.decl.designation, 'P' | 'S') && f.decl.device == "DISK")
            .map(|(i, _)| i)
            .collect();
        let primary = cycle_files
            .iter()
            .copied()
            .find(|&i| prog.files[i].decl.designation == 'P');
        if let Some(inz) = prog.routines.subroutines.get("*INZSR") {
            self.run_code(inz)?;
        }
        self.set_ind("1P", true);
        self.output(&['H', 'D'])?;
        self.set_ind("1P", false);
        let mut data_seen = false;
        let mut passes = 0u64;
        let uses_matching = cycle_files.iter().any(|&i| {
            prog.files[i]
                .inputs
                .iter()
                .any(|r| r.fields.iter().any(|f| f.matching.is_some()))
        });
        // The match value of the last record taken from each file.
        let mut last_match: Vec<Option<String>> = vec![None; prog.files.len()];
        loop {
            passes += 1;
            // Record identifying and control level indicators go off.
            for &i in &cycle_files {
                for r in &prog.files[i].inputs {
                    if let Some(ri) = &r.record_ind {
                        self.set_ind(ri, false);
                    }
                }
            }
            for l in 1..=9 {
                self.set_ind(&format!("L{l}"), false);
            }
            self.set_ind("MR", false);
            let mut chosen: Option<(usize, Record)> = None;
            if !self.ind("LR") && primary.is_some() {
                for &i in &cycle_files {
                    self.cycle_read(i)?;
                }
                // Which file's record comes next.
                let mut best: Option<(usize, Option<String>)> = None;
                for &i in &cycle_files {
                    let Some(ah) = &self.ahead[i] else { continue };
                    if !uses_matching {
                        // No matching fields: the primary file to its
                        // end, then each secondary file in turn.
                        best = Some((i, None));
                        break;
                    }
                    let (_, m) = self.control_and_match(i, &ah.record)?;
                    let better = match (&best, &m) {
                        (None, _) => true,
                        (Some((_, None)), Some(_)) => false,
                        (Some((_, Some(_))), None) => true,
                        (Some((_, Some(b))), Some(x)) => x < b,
                        (Some((_, None)), None) => false,
                    };
                    if better {
                        best = Some((i, m));
                    }
                }
                match best {
                    Some((i, m)) => {
                        let rec = self.ahead[i].take().unwrap().record;
                        // MR: the record matches a record of another file
                        // (taken before it, or waiting).
                        if let Some(mv) = &m {
                            let mut matched = false;
                            for &j in &cycle_files {
                                if j == i {
                                    continue;
                                }
                                if last_match[j].as_ref() == Some(mv) {
                                    matched = true;
                                }
                                if let Some(ah) = &self.ahead[j] {
                                    let (_, mj) = self.control_and_match(j, &ah.record)?;
                                    if mj.as_ref() == Some(mv) {
                                        matched = true;
                                    }
                                }
                            }
                            self.set_ind("MR", matched);
                        }
                        last_match[i] = m;
                        chosen = Some((i, rec));
                    }
                    None => {
                        // Every file at its end: LR, and all control
                        // levels for the last totals.
                        self.set_ind("LR", true);
                        for l in 1..=9 {
                            self.set_ind(&format!("L{l}"), true);
                        }
                    }
                }
            }
            // Control breaks.
            let mut first_record = false;
            if let Some((i, rec)) = &chosen {
                let (levels, _) = self.control_and_match(*i, rec)?;
                if !levels.is_empty() {
                    let highest_changed = levels
                        .iter()
                        .filter(|(l, v)| self.control.get(l) != Some(v))
                        .map(|(l, _)| *l)
                        .max();
                    if self.control.is_empty() {
                        first_record = true;
                        let top = levels.iter().map(|x| x.0).max().unwrap_or(0);
                        for l in 1..=top {
                            self.set_ind(&format!("L{l}"), true);
                        }
                    } else if let Some(h) = highest_changed {
                        for l in 1..=h {
                            self.set_ind(&format!("L{l}"), true);
                        }
                    }
                    for (l, v) in levels {
                        self.control.insert(l, v);
                    }
                }
            }
            // Total time: not before the first record.
            if data_seen && !first_record || self.ind("LR") {
                self.run_code(&prog.routines.total)?;
                self.output(&['T'])?;
            }
            if self.ind("LR") {
                break;
            }
            if let Some((i, rec)) = &chosen {
                self.move_input(*i, rec)?;
                data_seen = true;
            }
            let flow = self.run_code(&prog.routines.detail)?;
            if matches!(flow, Flow::Return(_)) {
                break;
            }
            self.output(&['H', 'D'])?;
            if self.ind("RT") {
                break;
            }
            // No primary file: the cycle starts over until LR is set. A
            // program that never sets LR runs until it is cancelled, as
            // on IBM i; stop runaway loops in tests at a large bound.
            if passes > 10_000_000 {
                bail!("the program does not set on LR and keeps running");
            }
            if primary.is_none() && !data_seen {
                data_seen = true;
            }
        }
        Ok(())
    }

    /// Output specifications for a database file (ILE RPG Reference,
    /// "Output Specifications"): ADD writes a new record, DEL deletes the
    /// record last read, otherwise the record last read is updated. An
    /// externally described record gets the fields listed (`*ALL`: every
    /// field); a program-described one is a record image with the fields
    /// at their end positions in their data format (position 52).
    pub(super) fn disk_output(&mut self, i: usize, r: &OutRec) -> Result<()> {
        let fi = &self.prog.files[i];
        let fref = fi
            .fref
            .clone()
            .ok_or_else(|| anyhow!("{} is not a database file", fi.decl.name))?;
        let client = self.io.client;
        self.block(crate::authority::check(
            client,
            &self.io.user,
            &fref.library,
            &fref.file.name,
            "*FILE",
            crate::authority::Aut::Change,
        ))?;
        let rec = if r.del {
            Record::new()
        } else if fi.program_described {
            let mut img = match (&self.files[i].image, r.add) {
                (Some(img), false) => img.clone(),
                _ => vec![data::BLANK; fi.record_len],
            };
            let mut prev_end = 0usize;
            for f in &r.fields {
                if !f.cond.is_empty() && !self.conditions_met(&f.cond) {
                    continue;
                }
                let bytes = self.out_bytes(f)?;
                let end = match (f.end, f.relative) {
                    (Some(e), _) => e as usize,
                    (None, Some(n)) => prev_end + n as usize + bytes.len(),
                    (None, None) => prev_end + bytes.len(),
                };
                if end > img.len() || end < bytes.len() {
                    bail!(
                        "output field ending in position {end} is outside the record of {} ({} bytes)",
                        fi.decl.name,
                        img.len()
                    );
                }
                img[end - bytes.len()..end].copy_from_slice(&bytes);
                prev_end = end;
            }
            self.files[i].image = Some(img.clone());
            self.image_record(i, &img)?
        } else {
            let all = r.fields.iter().any(|f| f.name.as_deref() == Some("*ALL"));
            let full = self.record_of(i)?;
            if all || r.fields.is_empty() {
                full
            } else {
                let mut rec = Record::new();
                for f in &r.fields {
                    if !f.cond.is_empty() && !self.conditions_met(&f.cond) {
                        continue;
                    }
                    let Some(n) = &f.name else { continue };
                    let Some(pos) = fi.fields.iter().position(|p| p == n) else {
                        bail!("{n} is not a field of record format {}", fi.format);
                    };
                    let db = &fref.file.fields[pos].name;
                    if let Some(v) = full.get(db) {
                        rec.insert(db.clone(), v.clone());
                    }
                }
                rec
            }
        };
        let rt = self.io.rt.clone();
        let f = self.files[i]
            .open
            .as_mut()
            .ok_or_else(|| anyhow!("CPF4135: File {} is not open.", r.file))?;
        if r.del {
            rt.block_on(async {
                f.delete(client).await?;
                f.release(client).await
            })?;
        } else if r.add {
            rt.block_on(f.write(client, &rec))?;
        } else {
            rt.block_on(async {
                f.update(client, &rec).await?;
                f.release(client).await
            })?;
        }
        Ok(())
    }

    /// The bytes of an output field for a record image: a literal, or the
    /// field in the data format of position 52 (blank: characters, or
    /// zoned digits; P packed; B binary), or edited when it has an edit
    /// code or word.
    fn out_bytes(&mut self, f: &OutField) -> Result<Vec<u8>> {
        if let Some(lit) = &f.literal {
            return Ok(lit.clone());
        }
        if f.edit_code.is_some() || f.edit_word.is_some() {
            return Ok(data::e(&self.out_text(f, 1)?));
        }
        let name = f.name.clone().unwrap_or_default();
        let ex = Expr::name(&name);
        let t = self
            .type_of(&ex)
            .ok_or_else(|| anyhow!("O spec field {name} is not defined"))?;
        let v = self.eval(&ex)?;
        if !t.is_numeric() {
            return v.chars();
        }
        let (n, d) = t.precision();
        let st = match f.format {
            'P' => DType::Packed(n, d),
            'B' => DType::Binary(n.min(9), d),
            _ => DType::Zoned(n, d),
        };
        let mut b = st.initial();
        data::store(&st, &v, &mut b, Fit::Truncate { half_adjust: false }, false)?;
        Ok(b)
    }
}
