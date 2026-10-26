//! Externally described printer files (ILE RPG Reference, PRINTER files
//! described by DDS; DDS reference for printer files): WRITE of a record
//! format prints it as `crate::prtf::layout` lays it out, on the same
//! printer (page, line, spooled file) the program's other output to the
//! file uses; the overflow indicator (OFLIND) comes on when a line is
//! printed on or past the overflow line, or spacing or skipping passes it.

use super::*;
use crate::prtf::{Content, PField, PrintOp};

impl Machine<'_> {
    /// The page length and overflow line of a printer file: FORMLEN and
    /// FORMOFL on the file (RPG III line counter specifications), else the
    /// printer file object's PAGESIZE and OVRFLW, else 66 and 60.
    pub(super) fn form_of(&self, i: usize) -> (i32, i32) {
        let f = &self.prog.files[i];
        let kw = |k: &str| -> Option<i32> {
            keyword(&f.decl.keywords, k)
                .and_then(|k| k.args.first())
                .and_then(|a| a.trim().parse().ok())
        };
        let (len, ovf) = match f.page {
            Some((l, _, o)) => (l, o),
            None => (crate::spool::PAGE_LENGTH, crate::spool::OVERFLOW_LINE),
        };
        (kw("FORMLEN").unwrap_or(len), kw("FORMOFL").unwrap_or(ovf))
    }

    /// The printer of a file, opened (its spooled file) on first use.
    pub(super) fn ensure_printer(&mut self, file: &str) -> Result<()> {
        if self.printers.contains_key(file) {
            return Ok(());
        }
        // OVRPRTF SPLFNAME and USRDTA in effect name the spooled file and
        // give its user data (else the file's name and the program's).
        let mut ovr = self
            .io
            .cl
            .as_ref()
            .map(|c| c.overrides.clone())
            .unwrap_or_default();
        crate::ovr::merge_scoped(&self.io.job_key, &mut ovr);
        // Overrides scoped to an activation group are used by that
        // activation group's opens only.
        let group = self.io.actgrp.clone();
        ovr.retain(|o| o.actgrp.as_ref().is_none_or(|g| *g == group));
        let o = crate::ovr::effective(&ovr, crate::ovr::Kind::Printer, file);
        let name = o
            .and_then(|o| o.splfname.clone())
            .unwrap_or_else(|| file.to_string());
        let usrdta = o
            .and_then(|o| o.usrdta.clone())
            .unwrap_or_else(|| self.prog.name.clone());
        let mut echo = false;
        let mut print_job = None;
        let spl = match &self.io.job {
            Some(job) => {
                Some(self.block(crate::spool::open(self.io.client, job, &name, &usrdta))?)
            }
            // `system` and `call`: the user's print job's, while the
            // command runs; its lines also go to standard output.
            None if crate::spool::print_job_mode() => {
                echo = true;
                let client = self.io.client;
                let user = self.io.user.clone();
                let pj = self.block(crate::spool::print_job(client, &user))?;
                let id = self.block(crate::spool::open(client, &pj, &name, &usrdta))?;
                crate::spool::note_temporary(id);
                print_job = Some(pj);
                Some(id)
            }
            None => None,
        };
        // The job's last spooled file, for QSPRILSP: its name, job and
        // number (without a job of as4tainr's, `as4tainr system`, the
        // lines go to standard output and the number counts opens).
        let number = match spl {
            Some(id) => self
                .block(async {
                    Ok::<i32, anyhow::Error>(
                        self.io
                            .client
                            .query_one(
                                "SELECT number FROM as4tainr.spooled_files WHERE id=$1",
                                &[&id],
                            )
                            .await?
                            .get::<_, i32>(0),
                    )
                })
                .unwrap_or(1),
            None => crate::rpg::last_spooled_file(&self.io.job_key).map_or(1, |l| l.4 + 1),
        };
        let (jname, juser, jnbr) = match self.io.job.as_ref().or(print_job.as_ref()) {
            Some(j) => (j.name.clone(), j.user.clone(), format!("{:06}", j.number)),
            None => (String::new(), self.io.user.clone(), String::new()),
        };
        crate::rpg::set_last_spooled_file(
            &self.io.job_key,
            (name.clone(), jname, juser, jnbr, number),
        );
        self.printers.insert(
            file.to_string(),
            Printer {
                spl,
                page: 1,
                line: 1,
                printed_on_page: false,
                buffer: Vec::new(),
                out_pos: (1, 0),
                echo,
            },
        );
        Ok(())
    }

    /// Operations on a PRINTER file in the calculations: WRITE of a record
    /// format (externally described); OPEN, CLOSE and FEOD.
    pub(super) fn printer_op(&mut self, op: FileOp, i: usize, name: &str) -> Result<()> {
        match op {
            FileOp::Write => self.printer_write(i, name),
            FileOp::Open | FileOp::Feod => {
                let file = self.prog.files[i].decl.name.clone();
                self.flush_printer(&file)
            }
            // CLOSE: the spooled file is complete (RDY); an OPEN after it
            // starts a new one.
            FileOp::Close => {
                let file = self.prog.files[i].decl.name.clone();
                self.flush_printer(&file)?;
                if let Some(p) = self.printers.remove(&file) {
                    if let Some(id) = p.spl {
                        let client = self.io.client;
                        self.block(crate::spool::close(client, id))?;
                    }
                }
                Ok(())
            }
            other => bail!(
                "{other:?} {name}: a printer file can only be written (WRITE, EXCEPT, output specifications)"
            ),
        }
    }

    /// WRITE of a printer file's record format.
    fn printer_write(&mut self, i: usize, fmt: &str) -> Result<()> {
        let prog = self.prog;
        let fi = &prog.files[i];
        let Some(dds) = fi.printer.clone() else {
            // WRITE of a program-described printer file with a data
            // structure (ILE RPG Reference, WRITE: "a data structure
            // must be specified for a program-described file"): the
            // structure's bytes are the line, printed as an output
            // specification without spacing would (one line down).
            if let Some(ds) = self.io_ds.clone() {
                let bytes = match self.sym(&ds) {
                    Some(Sym::Ds(d)) => self.bufs[d.buf][d.off..d.off + d.size].to_vec(),
                    Some(Sym::Var(v)) => self.bufs[v.buf][v.off..v.off + v.dtype.size()].to_vec(),
                    _ => bail!("WRITE {fmt}: {ds} is not a data structure"),
                };
                let rec = OutRec {
                    file: fi.decl.name.clone(),
                    kind: 'E',
                    fetch_overflow: false,
                    add: false,
                    del: false,
                    cond: Vec::new(),
                    name: None,
                    space_before: None,
                    space_after: None,
                    skip_before: None,
                    skip_after: None,
                    fields: vec![OutField {
                        cond: Vec::new(),
                        name: None,
                        edit_code: None,
                        blank_after: false,
                        end: Some(bytes.len() as u32),
                        relative: None,
                        edit_word: None,
                        format: ' ',
                        literal: Some(bytes),
                    }],
                    line: 0,
                };
                return self.print_line(&rec);
            }
            bail!(
                "WRITE {fmt}: printer file {} is program-described; it is written with EXCEPT and output specifications",
                fi.decl.name
            );
        };
        let rec = dds.record(fmt).ok_or_else(|| {
            anyhow!(
                "WRITE {fmt}: printer file {} has no record format {fmt}",
                fi.decl.name
            )
        })?;
        let file = fi.decl.name.clone();
        self.ensure_printer(&file)?;
        // Indicators: *IN01-*IN99, or the INDDS data structure (INDARA).
        let mut ind = [false; 100];
        let indds = keyword(&fi.decl.keywords, "INDDS")
            .and_then(|k| k.args.first())
            .map(|a| a.trim().to_ascii_uppercase());
        match indds.as_ref().and_then(|n| self.sym(n)) {
            Some(Sym::Ds(ds)) => {
                let bytes = self.bufs[ds.buf][ds.off..ds.off + ds.size].to_vec();
                for (n, slot) in ind.iter_mut().enumerate().skip(1) {
                    *slot = bytes.get(n - 1) == Some(&0xF1);
                }
            }
            _ => {
                for (n, slot) in ind.iter_mut().enumerate().skip(1) {
                    *slot = self.ind(&format!("{n:02}"));
                }
            }
        }
        // PAGNBR: the page the record prints on, after its SKIPB or
        // SPACEB.
        let page = {
            let p = &self.printers[&file];
            let (page_length, _) = self.form_of(i);
            let first = |v: &[(Option<crate::dspf::Cond>, u32)]| {
                v.iter()
                    .find(|(c, _)| crate::dspf::cond_true(c, &ind))
                    .map(|(_, n)| *n as i32)
            };
            let mut line = p.line;
            let mut page = p.page;
            if let Some(n) = first(&rec.spacing.skip_before) {
                if n < line || n == line && p.printed_on_page {
                    page += 1;
                }
                line = n;
            }
            if let Some(n) = first(&rec.spacing.space_before) {
                if line + n > page_length {
                    page += 1;
                }
            }
            page
        };
        let mut texts: Vec<String> = Vec::new();
        let mut errors: Option<anyhow::Error> = None;
        // The layout asks for each field's text in order; the values are
        // read first (the closure cannot borrow the machine).
        for f in &rec.fields {
            if !f.printed
                || !crate::dspf::cond_true(&f.cond, &ind)
                || matches!(f.content, Content::Literal(_))
            {
                continue;
            }
            match self.printer_text(f, page) {
                Ok(t) => texts.push(t),
                Err(e) => {
                    errors = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = errors {
            return Err(e);
        }
        let mut it = texts.into_iter();
        let ops = crate::prtf::layout(rec, &ind, &mut |_f: &PField| {
            Ok(it.next().unwrap_or_default())
        })?;
        self.print_ops(i, &file, &ops)
    }

    /// The printed text of a field or of DATE, TIME and PAGNBR: numbers
    /// with their edit code or edit word (without one, the digits of the
    /// zoned value, a negative one's sign in the last digit's zone).
    fn printer_text(&mut self, f: &PField, page: i32) -> Result<String> {
        let (value, digits, decs, numeric) = match &f.content {
            Content::Literal(t) => return Ok(t.clone()),
            Content::Date { four_digit } => {
                let (y, m, d) = today();
                let v = if *four_digit {
                    (m as i64) * 1_000_000 + (d as i64) * 10_000 + y as i64
                } else {
                    (m as i64) * 10_000 + (d as i64) * 100 + (y % 100) as i64
                };
                (Dec::from_i64(v), if *four_digit { 8 } else { 6 }, 0, true)
            }
            Content::Time => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    % 86_400;
                let v = (secs / 3600) * 10_000 + (secs / 60 % 60) * 100 + secs % 60;
                (Dec::from_i64(v as i64), 6, 0, true)
            }
            Content::PageNumber => (Dec::from_i64(page as i64), 4, 0, true),
            Content::Field => {
                let v = self.eval(&Expr::name(&f.name))?;
                match f.decimals {
                    Some(d) if f.dtype == 'S' => (v.num()?, f.length as u32, d as u32, true),
                    _ => {
                        let t = a(&v.chars()?);
                        let n = f.length as usize;
                        return Ok(format!("{:<n$}", t.chars().take(n).collect::<String>()));
                    }
                }
            }
        };
        let _ = numeric;
        if let Some(w) = &f.edit_word {
            return edit_word(&value, digits, decs, w);
        }
        Ok(match f.edit_code {
            Some(c) => edit_code(&value, digits, decs, c),
            None => {
                // Zoned digits as they are.
                let t = DType::Zoned(digits, decs);
                let mut b = t.initial();
                data::store(
                    &t,
                    &Value::Num(value),
                    &mut b,
                    Fit::Truncate { half_adjust: false },
                    false,
                )?;
                a(&b)
            }
        })
    }

    /// Carry out a record's printing: skips, spacing, lines; overflow.
    pub(super) fn print_ops(&mut self, i: usize, file: &str, ops: &[PrintOp]) -> Result<()> {
        let (page_length, overflow_line) = self.form_of(i);
        let overflow_ind = self.overflow_ind(file);
        let p = self.printers.get_mut(file).unwrap();
        let new_page = |p: &mut Printer| {
            p.page += 1;
            p.line = 1;
            p.printed_on_page = false;
        };
        let mut overflow = false;
        for op in ops {
            match op {
                PrintOp::Skip(n) | PrintOp::Line(n) => {
                    let n = *n as i32;
                    if n < p.line || n == p.line && p.printed_on_page {
                        new_page(p);
                    } else if p.line <= overflow_line && n > overflow_line {
                        overflow = true;
                    }
                    p.line = n;
                }
                PrintOp::Space(n) => {
                    let before = p.line;
                    p.line += *n as i32;
                    if before <= overflow_line && p.line > overflow_line {
                        overflow = true;
                    }
                    if p.line > page_length {
                        new_page(p);
                    }
                }
                PrintOp::Text(t) => {
                    if p.line > page_length {
                        new_page(p);
                    }
                    p.buffer.push((p.page, p.line, t.clone()));
                    p.printed_on_page = true;
                    if p.line >= overflow_line {
                        overflow = true;
                    }
                }
            }
        }
        let full = p.buffer.len() >= 50;
        if overflow {
            if let Some(of) = overflow_ind {
                self.set_ind(&of, true);
            }
        }
        if full {
            self.flush_printer(file)?;
        }
        Ok(())
    }
}
