//! A program's display (WORKSTN) files while it runs: the record formats on
//! the screen, the subfiles, and one EXFMT's interaction with the operator,
//! including paging through a subfile.
//!
//! Written from IBM's public DDS and RPG references:
//!
//! - WRITE of a record format puts it on the screen. Without OVERLAY the
//!   screen is cleared first; with OVERLAY only records that share a line
//!   with the new one are removed.
//! - A subfile is a list of records numbered 1-9999 (relative record
//!   numbers) that the program fills with WRITE and that the system shows a
//!   page (SFLPAG records) at a time below its control record (SFLCTL).
//!   Page Down/Page Up move through the records already in the subfile
//!   without the program; past either end, PAGEDOWN/PAGEUP hand the key to
//!   the program, if the control record has them.
//! - After the control record is read, READC returns the subfile records
//!   the operator changed, in record number order, plus records written or
//!   updated with SFLNXTCHG on.
//!
//! Not supported yet (see docs/fidelity-gaps.md): SFLRCDNBR, SFLINZ,
//! SFLFOLD/SFLDROP, message subfiles, READ of a display file.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::dspf::{self, Context, DisplayFile, DrawOpts, Keyword, Record, Response};
use crate::tn5250::ds::{self, Aid, Screen};

/// A record format as last written to the screen.
#[derive(Debug, Clone)]
pub struct Layer {
    pub file: Arc<DisplayFile>,
    pub format: String,
    pub values: BTreeMap<String, String>,
    pub ind: [bool; 100],
}

impl Layer {
    fn record(&self) -> &Record {
        self.file
            .record(&self.format)
            .expect("layer formats are checked when written")
    }
}

#[derive(Debug, Clone)]
struct SflRecord {
    values: BTreeMap<String, String>,
    ind: [bool; 100],
    /// SFLNXTCHG was on when the program wrote or updated the record.
    next_change: bool,
    /// Returned by READC.
    changed: bool,
}

#[derive(Debug, Default)]
struct Subfile {
    records: BTreeMap<u16, SflRecord>,
    /// READC continues with records numbered from here.
    readc_from: u16,
    /// Record last read by READC or CHAIN (for UPDATE).
    current: Option<u16>,
}

/// One subfile record as an EXFMT shows it.
#[derive(Debug, Clone)]
pub struct SflRow {
    pub rrn: u16,
    pub values: BTreeMap<String, String>,
    pub ind: [bool; 100],
}

/// The subfile part of an EXFMT of a control record.
#[derive(Debug, Clone)]
pub struct SflFrame {
    pub format: String,
    /// SFLDSP is on: show the records.
    pub show: bool,
    pub rows: Vec<SflRow>,
}

/// Everything one EXFMT shows: the records on the screen, the last one
/// being the record read, and its subfile.
#[derive(Debug, Clone)]
pub struct Frame {
    pub layers: Vec<Layer>,
    pub subfile: Option<SflFrame>,
}

/// The display files of one program call.
#[derive(Debug, Default)]
pub struct Workstation {
    screen: Vec<Layer>,
    subfiles: HashMap<(String, String), Subfile>,
}

fn record<'a>(file: &'a DisplayFile, format: &str) -> Result<&'a Record> {
    file.record(format).ok_or_else(|| {
        anyhow!(
            "Record format {} not found in display file {}.",
            format.to_ascii_uppercase(),
            file.name
        )
    })
}

/// The subfile record format a control record controls, and its geometry:
/// (record, first line, lines per record).
fn subfile_of<'a>(file: &'a DisplayFile, ctl: &Record) -> Option<(&'a Record, u8, u8)> {
    let sfl = file.record(ctl.subfile_control()?)?;
    let rows = sfl.rows();
    let first = *rows.first()?;
    Some((sfl, first, rows[rows.len() - 1] - first + 1))
}

/// Screen lines a record occupies, for the OVERLAY rules. A control record
/// also owns its subfile's lines and the SFLEND line.
fn lines(file: &DisplayFile, rec: &Record) -> Vec<u8> {
    let mut rows = rec.rows();
    if let Some((_, first, height)) = subfile_of(file, rec) {
        let mut last = first as u16 + height as u16 * rec.sflpag() - 1;
        if rec.has(|k| matches!(k, Keyword::SflEnd(_))) {
            last += 1;
        }
        rows.extend(first..=(last.min(24) as u8));
    }
    rows.sort_unstable();
    rows.dedup();
    rows
}

impl Workstation {
    pub fn new() -> Self {
        Self::default()
    }

    fn place(&mut self, layer: Layer) {
        let rec = layer.record();
        if !rec.active(&layer.ind, |k| *k == Keyword::Overlay) {
            self.screen.clear();
        }
        let mine = lines(&layer.file, rec);
        self.screen.retain(|l| {
            let theirs = lines(&l.file, l.record());
            !(l.file.name == layer.file.name && l.format == layer.format)
                && !theirs.iter().any(|r| mine.contains(r))
        });
        self.screen.push(layer);
    }

    fn subfile(&mut self, file: &DisplayFile, format: &str) -> &mut Subfile {
        self.subfiles
            .entry((file.name.clone(), format.to_ascii_uppercase()))
            .or_default()
    }

    /// WRITE of a record format that is not a subfile record.
    pub fn write(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
        values: BTreeMap<String, String>,
        ind: [bool; 100],
    ) -> Result<()> {
        let rec = record(file, format)?;
        if rec.is_subfile() {
            bail!(
                "WRITE {}: subfile records are written with the relative record number named in SFILE.",
                rec.name
            );
        }
        if let Some(sfl) = rec.subfile_control() {
            if rec.active(&ind, |k| matches!(k, Keyword::SflClr | Keyword::SflDlt)) {
                let sub = self.subfile(file, sfl);
                sub.records.clear();
                sub.readc_from = 0;
                sub.current = None;
            }
            let shows = rec.active(&ind, |k| matches!(k, Keyword::SflDspCtl | Keyword::SflDsp));
            if !shows {
                return Ok(());
            }
        }
        self.place(Layer {
            file: file.clone(),
            format: rec.name.clone(),
            values,
            ind,
        });
        Ok(())
    }

    fn subfile_record<'a>(file: &'a DisplayFile, format: &str) -> Result<(&'a Record, &'a Record)> {
        let rec = record(file, format)?;
        if !rec.is_subfile() {
            bail!(
                "{} is not a subfile record format (no SFL keyword).",
                rec.name
            );
        }
        let ctl = file
            .records
            .iter()
            .find(|r| r.subfile_control() == Some(rec.name.as_str()))
            .ok_or_else(|| anyhow!("subfile {} has no control record", rec.name))?;
        Ok((rec, ctl))
    }

    /// WRITE of a subfile record.
    pub fn sfl_write(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
        rrn: i64,
        values: BTreeMap<String, String>,
        ind: [bool; 100],
    ) -> Result<()> {
        let (rec, ctl) = Self::subfile_record(file, format)?;
        if !(1..=9999).contains(&rrn) {
            bail!(
                "WRITE {}: relative record number {rrn} is not valid; subfile records are numbered 1 to 9999.",
                rec.name
            );
        }
        let rrn = rrn as u16;
        // SFLSIZ = SFLPAG: the subfile cannot grow past one page.
        if ctl.sflsiz() == ctl.sflpag() && rrn > ctl.sflsiz() {
            bail!(
                "WRITE {}: record {rrn} does not fit; the subfile holds {} records (SFLSIZ equals SFLPAG).",
                rec.name,
                ctl.sflsiz()
            );
        }
        let next_change = rec.active(&ind, |k| *k == Keyword::SflNxtChg);
        let name = rec.name.clone();
        let sub = self.subfile(file, &name);
        if sub.records.contains_key(&rrn) {
            bail!("WRITE {name}: the subfile already has record {rrn}.");
        }
        sub.records.insert(
            rrn,
            SflRecord {
                values,
                ind,
                next_change,
                changed: false,
            },
        );
        Ok(())
    }

    /// CHAIN to a subfile record by relative record number.
    pub fn sfl_chain(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
        rrn: i64,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let (rec, _) = Self::subfile_record(file, format)?;
        let name = rec.name.clone();
        let sub = self.subfile(file, &name);
        let key = u16::try_from(rrn).ok();
        match key.and_then(|k| sub.records.get(&k).map(|r| (k, r.values.clone()))) {
            Some((k, v)) => {
                sub.current = Some(k);
                Ok(Some(v))
            }
            None => {
                sub.current = None;
                Ok(None)
            }
        }
    }

    /// READC: the next changed subfile record, with its number.
    pub fn sfl_readc(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
    ) -> Result<Option<(u16, BTreeMap<String, String>)>> {
        let (rec, _) = Self::subfile_record(file, format)?;
        let name = rec.name.clone();
        let sub = self.subfile(file, &name);
        let next = sub
            .records
            .range(sub.readc_from..)
            .find(|(_, r)| r.changed)
            .map(|(k, r)| (*k, r.values.clone()));
        match next {
            Some((k, v)) => {
                sub.readc_from = k + 1;
                sub.current = Some(k);
                Ok(Some((k, v)))
            }
            None => {
                sub.readc_from = u16::MAX;
                sub.current = None;
                Ok(None)
            }
        }
    }

    /// UPDATE of the subfile record last read by READC or CHAIN.
    pub fn sfl_update(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
        values: BTreeMap<String, String>,
        ind: [bool; 100],
    ) -> Result<()> {
        let (rec, _) = Self::subfile_record(file, format)?;
        let next_change = rec.active(&ind, |k| *k == Keyword::SflNxtChg);
        let name = rec.name.clone();
        let sub = self.subfile(file, &name);
        let Some(cur) = sub.current else {
            bail!("UPDATE {name}: no subfile record was read first (READC or CHAIN).");
        };
        let r = sub.records.get_mut(&cur).expect("current record exists");
        r.values = values;
        r.ind = ind;
        r.next_change = next_change;
        r.changed = false;
        Ok(())
    }

    /// EXFMT, first half: put the record on the screen and describe what
    /// the operator will see.
    pub fn frame(
        &mut self,
        file: &Arc<DisplayFile>,
        format: &str,
        values: BTreeMap<String, String>,
        ind: [bool; 100],
    ) -> Result<Frame> {
        let rec = record(file, format)?;
        if rec.is_subfile() {
            bail!(
                "EXFMT {}: a subfile record is not shown by itself; EXFMT its control record.",
                rec.name
            );
        }
        let mut subfile = None;
        if let Some(sfl) = rec.subfile_control() {
            if rec.has(|k| *k == Keyword::SflDspCtl)
                && !rec.active(&ind, |k| *k == Keyword::SflDspCtl)
            {
                bail!(
                    "EXFMT {}: the control record is not displayed (SFLDSPCTL is off).",
                    rec.name
                );
            }
            let show = rec.active(&ind, |k| *k == Keyword::SflDsp);
            let sfl = sfl.to_string();
            let sub = self.subfile(file, &sfl);
            if show && sub.records.is_empty() {
                bail!(
                    "EXFMT {}: subfile {sfl} has no records to display (SFLDSP is on).",
                    rec.name
                );
            }
            let rows = sub
                .records
                .iter()
                .map(|(k, r)| SflRow {
                    rrn: *k,
                    values: r.values.clone(),
                    ind: r.ind,
                })
                .collect();
            subfile = Some(SflFrame {
                format: sfl,
                show,
                rows,
            });
        }
        self.place(Layer {
            file: file.clone(),
            format: rec.name.clone(),
            values,
            ind,
        });
        Ok(Frame {
            layers: self.screen.clone(),
            subfile,
        })
    }

    /// EXFMT, second half: take in what the operator entered.
    pub fn apply(&mut self, file: &DisplayFile, format: &str, resp: &Response) {
        if let Some(l) = self
            .screen
            .iter_mut()
            .rev()
            .find(|l| l.file.name == file.name && l.format.eq_ignore_ascii_case(format))
        {
            for (k, v) in &resp.values {
                l.values.insert(k.clone(), v.clone());
            }
        }
        let Some(sfl) = file.record(format).and_then(|r| r.subfile_control()) else {
            return;
        };
        let sub = self.subfile(file, sfl);
        for (rrn, r) in sub.records.iter_mut() {
            let typed = resp.changed.get(rrn);
            if let Some(t) = typed {
                for (k, v) in t {
                    r.values.insert(k.clone(), v.clone());
                }
            }
            r.changed = r.next_change || typed.is_some();
            r.next_change = false;
        }
        sub.readc_from = 0;
        sub.current = None;
    }
}

// ------------------------------------------------------------ interaction

/// One EXFMT on the operator's screen: draws the frame, pages through the
/// subfile, and validates what was typed.
pub struct Interaction<'a> {
    frame: &'a Frame,
    /// Index (into the subfile rows) of the first record on the page.
    top: usize,
    ctl_values: BTreeMap<String, String>,
    /// What the operator typed into subfile records, by record number.
    edits: BTreeMap<u16, BTreeMap<String, String>>,
    message: Option<String>,
    /// Field in error: (subfile record number or None for the control
    /// record, field name).
    error: Option<(Option<u16>, String)>,
}

pub enum Step {
    Done(Response),
    /// Show the screen again (another page, or an error).
    Again,
}

const SFL_FIELD_SEP: char = '.';

impl<'a> Interaction<'a> {
    pub fn new(frame: &'a Frame) -> Self {
        let active = frame
            .layers
            .last()
            .expect("a frame has the record being read");
        Self {
            frame,
            top: 0,
            ctl_values: active.values.clone(),
            edits: BTreeMap::new(),
            message: None,
            error: None,
        }
    }

    fn active(&self) -> &'a Layer {
        self.frame
            .layers
            .last()
            .expect("a frame has the record being read")
    }

    fn page_size(&self) -> usize {
        self.active().record().sflpag() as usize
    }

    /// Index of the first record now shown (for tests and the cursor).
    pub fn top(&self) -> usize {
        self.top
    }

    pub fn screen(&self, ctx: &Context) -> Screen {
        let mut s = Screen::new();
        let (active, others) = self.frame.layers.split_last().expect("non-empty frame");
        for l in others {
            dspf::draw(
                &mut s,
                l.record(),
                &l.values,
                &l.ind,
                ctx,
                &DrawOpts::default(),
            );
        }
        let rec = active.record();
        let mut cursor = None;
        let mut message = None;
        let ctl_error = match &self.error {
            Some((None, f)) => Some(f.as_str()),
            _ => None,
        };
        let show_ctl = self.frame.subfile.is_none()
            || !rec.has(|k| *k == Keyword::SflDspCtl)
            || rec.active(&active.ind, |k| *k == Keyword::SflDspCtl);
        if show_ctl {
            let d = dspf::draw(
                &mut s,
                rec,
                &self.ctl_values,
                &active.ind,
                ctx,
                &DrawOpts {
                    input: true,
                    error_field: ctl_error,
                    ..Default::default()
                },
            );
            cursor = d.cursor;
            message = d.message;
        }
        if let (Some(sf), Some((sflrec, first, height))) =
            (&self.frame.subfile, subfile_of(&active.file, rec))
        {
            let pag = self.page_size();
            if sf.show {
                for (i, row) in sf.rows.iter().skip(self.top).take(pag).enumerate() {
                    let mut v = row.values.clone();
                    if let Some(e) = self.edits.get(&row.rrn) {
                        v.extend(e.iter().map(|(k, x)| (k.clone(), x.clone())));
                    }
                    let prefix = format!("{}{SFL_FIELD_SEP}", row.rrn);
                    let err = match &self.error {
                        Some((Some(r), f)) if *r == row.rrn => Some(f.as_str()),
                        _ => None,
                    };
                    let d = dspf::draw(
                        &mut s,
                        sflrec,
                        &v,
                        &row.ind,
                        ctx,
                        &DrawOpts {
                            input: true,
                            row_offset: i as u8 * height,
                            prefix: &prefix,
                            error_field: err,
                        },
                    );
                    if cursor.is_none() {
                        cursor = d.cursor;
                    }
                    if message.is_none() {
                        message = d.message;
                    }
                }
                // SFLEND: tell the operator whether there is more.
                let end_kind = rec.keywords.iter().find_map(|k| match &k.kw {
                    Keyword::SflEnd(kind) => Some(kind.as_str()),
                    _ => None,
                });
                if let Some(kind) = end_kind {
                    let at_end = self.top + pag >= sf.rows.len();
                    let end_on = rec.active(&active.ind, |k| matches!(k, Keyword::SflEnd(_)));
                    let last_line = first as usize + pag * height as usize - 1;
                    if kind == "*PLUS" {
                        if !(at_end && end_on) {
                            s.put(last_line as u8, 80, "+");
                        }
                    } else {
                        let text = if at_end && end_on {
                            "Bottom"
                        } else {
                            "More..."
                        };
                        let row = (last_line + 1).min(24) as u8;
                        s.put(row, 80 - text.len() as u8, text);
                    }
                }
            }
        }
        if let Some(c) = cursor {
            s.cursor_at(c.0, c.1);
        }
        if let Some(m) = self.message.clone().or(message) {
            s.message(&m);
        }
        s
    }

    fn sfl_row_count(&self) -> usize {
        match &self.frame.subfile {
            Some(sf) if sf.show => sf.rows.len(),
            _ => 0,
        }
    }

    fn page_key(&self, down: bool) -> Option<Option<u8>> {
        let active = self.active();
        active.record().keywords.iter().find_map(|k| match k.kw {
            Keyword::PageDown(i) if down && dspf::cond_true(&k.cond, &active.ind) => Some(i),
            Keyword::PageUp(i) if !down && dspf::cond_true(&k.cond, &active.ind) => Some(i),
            _ => None,
        })
    }

    pub fn input(&mut self, inp: &ds::Input) -> Step {
        self.message = None;
        self.error = None;
        for name in &inp.modified {
            let v = inp.get(name).to_string();
            match name.split_once(SFL_FIELD_SEP) {
                Some((rrn, field)) => {
                    if let Ok(rrn) = rrn.parse::<u16>() {
                        self.edits
                            .entry(rrn)
                            .or_default()
                            .insert(field.to_string(), v);
                    }
                }
                None => {
                    self.ctl_values.insert(name.clone(), v);
                }
            }
        }
        let n = self.sfl_row_count();
        let pag = self.page_size();
        let has_sfl = self.frame.subfile.is_some();
        let page_ind = match inp.aid {
            Aid::RollUp if has_sfl => {
                if self.top + pag < n {
                    self.top += pag;
                    return Step::Again;
                }
                match self.page_key(true) {
                    Some(i) => Some(i),
                    None => {
                        self.message = Some("You are already at the bottom of the list.".into());
                        return Step::Again;
                    }
                }
            }
            Aid::RollDown if has_sfl => {
                if self.top > 0 {
                    self.top = self.top.saturating_sub(pag);
                    return Step::Again;
                }
                match self.page_key(false) {
                    Some(i) => Some(i),
                    None => {
                        self.message = Some("You are already at the top of the list.".into());
                        return Step::Again;
                    }
                }
            }
            Aid::RollUp | Aid::RollDown => self.page_key(inp.aid == Aid::RollUp),
            _ => None,
        };
        self.finish(inp, page_ind)
    }

    fn finish(&mut self, inp: &ds::Input, page_ind: Option<Option<u8>>) -> Step {
        let active = self.active();
        let rec = active.record();
        // The control record's fields, as the control record sees them.
        let ctl_input = ds::Input {
            aid: if page_ind.is_some() {
                Aid::Enter
            } else {
                inp.aid
            },
            cursor: inp.cursor,
            values: self
                .ctl_values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            modified: vec![],
        };
        let mut resp = match dspf::handle_input(&active.file, rec, &active.ind, &ctl_input) {
            dspf::Outcome::Retry { message, field } => {
                self.message = Some(message);
                self.error = field.map(|f| (None, f));
                return Step::Again;
            }
            dspf::Outcome::Done(r) => r,
        };
        if let Some(i) = page_ind {
            resp.aid = inp.aid.name();
            if let Some(i) = i {
                resp.indicators.insert(i, true);
            }
        }
        let returns_data =
            page_ind.is_some() || dspf::returns_data(&active.file, rec, &active.ind, inp.aid);
        if returns_data {
            if let Some((sflrec, _, _)) = subfile_of(&active.file, rec) {
                let rows: Vec<u16> = self
                    .frame
                    .subfile
                    .as_ref()
                    .map(|s| s.rows.iter().map(|r| r.rrn).collect())
                    .unwrap_or_default();
                for (rrn, e) in &self.edits {
                    let mut out = BTreeMap::new();
                    for (name, text) in e {
                        let Some(f) = sflrec.fields.iter().find(|f| &f.name == name) else {
                            continue;
                        };
                        let v = if f.is_numeric() {
                            match dspf::parse_numeric_input(text, f.length, f.decimals.unwrap_or(0))
                            {
                                Ok(v) => v,
                                Err(_) => {
                                    let idx = rows.iter().position(|r| r == rrn).unwrap_or(0);
                                    let pag = self.page_size().max(1);
                                    self.top = idx / pag * pag;
                                    self.message = Some(format!(
                                        "Field {} needs a number with at most {} digits ({} decimals).",
                                        f.name,
                                        f.length,
                                        f.decimals.unwrap_or(0)
                                    ));
                                    self.error = Some((Some(*rrn), f.name.clone()));
                                    return Step::Again;
                                }
                            }
                        } else {
                            text.clone()
                        };
                        out.insert(name.clone(), v);
                    }
                    resp.changed.insert(*rrn, out);
                }
            }
        }
        Step::Done(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUSTWRKD: &str = include_str!("../../samples/qddssrc/CUSTWRKD.dspf");

    fn file() -> Arc<DisplayFile> {
        Arc::new(dspf::parse("CUSTWRKD", CUSTWRKD).unwrap())
    }

    fn ind(on: &[usize]) -> [bool; 100] {
        let mut i = [false; 100];
        for &n in on {
            i[n] = true;
        }
        i
    }

    fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn ctx() -> Context<'static> {
        Context {
            user: "QSECOFR",
            system: "AS4TAINR",
            date: "10/15/26".into(),
            time: "12:00:00".into(),
        }
    }

    fn text(s: &Screen) -> String {
        crate::tn5250::ebcdic::decode(&s.to_stream())
    }

    fn load(ws: &mut Workstation, f: &Arc<DisplayFile>, n: u16) {
        ws.write(f, "CUSTCTL", BTreeMap::new(), ind(&[32])).unwrap();
        for i in 1..=n {
            ws.sfl_write(
                f,
                "CUSTSFL",
                i as i64,
                vals(&[
                    ("OPT", ""),
                    ("CUSNO", &i.to_string()),
                    ("CUSNAM", &format!("Customer {i}")),
                ]),
                ind(&[]),
            )
            .unwrap();
        }
    }

    fn input(aid: Aid, typed: &[(&str, &str)]) -> ds::Input {
        ds::Input {
            aid,
            cursor: (1, 1),
            values: typed
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            modified: typed.iter().map(|(a, _)| a.to_string()).collect(),
        }
    }

    #[test]
    fn dds_subfile_keywords() {
        let f = file();
        let sfl = f.record("CUSTSFL").unwrap();
        let ctl = f.record("CUSTCTL").unwrap();
        assert!(sfl.is_subfile());
        assert_eq!(ctl.subfile_control(), Some("CUSTSFL"));
        assert_eq!((ctl.sflsiz(), ctl.sflpag()), (50, 4));
        assert!(ctl.active(&ind(&[31]), |k| *k == Keyword::SflDsp));
        assert!(!ctl.active(&ind(&[]), |k| *k == Keyword::SflDsp));
        assert!(ctl.has(|k| *k == Keyword::Overlay));
        // SFLMSG text continued with '+' on the next line.
        assert!(ctl.has(|k| *k
            == Keyword::SflMsg {
                text: "Type option 5 or leave the option blank.".into(),
                indicator: Some(40)
            }));
        // The control record owns the subfile lines 7-10 and the SFLEND line 11.
        assert_eq!(lines(&f, ctl), vec![1, 3, 4, 6, 7, 8, 9, 10, 11]);
    }

    #[test]
    fn dds_rules_for_subfiles() {
        // IBM DDS: SFLCTL must immediately follow the SFL record it names.
        let bad = "     A          R S1                        SFL\n     A            F1             5A  O  5  2\n     A          R OTHER\n     A                                  1  2'x'\n     A          R C1                        SFLCTL(S1)\n     A                                      SFLSIZ(10)\n     A                                      SFLPAG(5)\n";
        assert!(dspf::parse("X", bad).is_err());
        // SFLSIZ and SFLPAG are required on the control record.
        let nosiz = "     A          R S1                        SFL\n     A            F1             5A  O  5  2\n     A          R C1                        SFLCTL(S1)\n     A                                      SFLPAG(5)\n";
        assert!(dspf::parse("X", nosiz).is_err());
        // A page must fit on the 24-line display.
        let toobig = "     A          R S1                        SFL\n     A            F1             5A  O 20  2\n     A          R C1                        SFLCTL(S1)\n     A                                      SFLSIZ(10)\n     A                                      SFLPAG(10)\n";
        assert!(dspf::parse("X", toobig).is_err());
    }

    #[test]
    fn sflclr_and_relative_record_numbers() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 5);
        // The same record number twice is an error.
        assert!(ws.sfl_write(&f, "CUSTSFL", 3, vals(&[]), ind(&[])).is_err());
        // Subfile records are numbered 1-9999 (IBM RPG reference, SFILE).
        assert!(ws.sfl_write(&f, "CUSTSFL", 0, vals(&[]), ind(&[])).is_err());
        assert!(ws
            .sfl_write(&f, "CUSTSFL", 10000, vals(&[]), ind(&[]))
            .is_err());
        assert_eq!(
            ws.sfl_chain(&f, "CUSTSFL", 2).unwrap().unwrap()["CUSNAM"],
            "Customer 2"
        );
        assert!(ws.sfl_chain(&f, "CUSTSFL", 9).unwrap().is_none());
        // WRITE of the control record with SFLCLR on empties the subfile.
        ws.write(&f, "CUSTCTL", BTreeMap::new(), ind(&[32]))
            .unwrap();
        assert!(ws.sfl_chain(&f, "CUSTSFL", 2).unwrap().is_none());
        // A subfile record is not written like a plain record, and vice versa.
        assert!(ws.write(&f, "CUSTSFL", vals(&[]), ind(&[])).is_err());
        assert!(ws.sfl_write(&f, "FOOTER", 1, vals(&[]), ind(&[])).is_err());
    }

    // IBM DDS reference (SFLDSP): displaying a subfile with no records is an
    // error; with SFLDSP off only the control record is shown.
    #[test]
    fn exfmt_with_sfldsp_needs_records() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 0);
        assert!(ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31]))
            .is_err());
        // SFLDSP off: only the control record is shown.
        let fr = ws.frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[])).unwrap();
        assert!(!fr.subfile.unwrap().show);
    }

    // IBM DDS reference (OVERLAY): without OVERLAY the display is erased
    // before the record is shown; with it, only records that overlap it are.
    #[test]
    fn overlay_keeps_records_on_other_lines() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 5);
        ws.write(&f, "FOOTER", BTreeMap::new(), ind(&[])).unwrap();
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31, 33]))
            .unwrap();
        let names: Vec<&str> = fr.layers.iter().map(|l| l.format.as_str()).collect();
        assert_eq!(names, ["FOOTER", "CUSTCTL"]);
        // DETAIL has no OVERLAY: it clears the screen.
        let fr = ws.frame(&f, "DETAIL", BTreeMap::new(), ind(&[])).unwrap();
        let names: Vec<&str> = fr.layers.iter().map(|l| l.format.as_str()).collect();
        assert_eq!(names, ["DETAIL"]);
    }

    // IBM DDS reference (SFLPAG, SFLSIZ > SFLPAG, SFLEND(*MORE)): the system
    // pages through the records already in the subfile; More... unless the
    // SFLEND indicator is on and the last page is shown, then Bottom.
    #[test]
    fn paging_within_the_subfile_and_sflend() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 5);
        ws.write(&f, "FOOTER", BTreeMap::new(), ind(&[])).unwrap();
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31, 33]))
            .unwrap();
        let mut it = Interaction::new(&fr);
        let t = text(&it.screen(&ctx()));
        assert!(t.contains("Customer 1") && t.contains("Customer 4"));
        assert!(!t.contains("Customer 5"));
        assert!(t.contains("More..."), "{t}");
        assert!(t.contains("F3=Exit"), "the footer stays (OVERLAY)");
        // Page Down shows the second page without the program.
        assert!(matches!(it.input(&input(Aid::RollUp, &[])), Step::Again));
        let t = text(&it.screen(&ctx()));
        assert!(t.contains("Customer 5") && !t.contains("Customer 4"));
        // SFLEND(*MORE) with its indicator on: last page says Bottom.
        assert!(t.contains("Bottom"), "{t}");
        // Past the end without PAGEDOWN: a message, still the program waits.
        assert!(matches!(it.input(&input(Aid::RollUp, &[])), Step::Again));
        assert!(text(&it.screen(&ctx())).contains("bottom of the list"));
        assert!(matches!(it.input(&input(Aid::RollDown, &[])), Step::Again));
        assert_eq!(it.top(), 0);
    }

    // IBM RPG reference (READC) and DDS reference (SFLNXTCHG): READC reads
    // the next changed subfile record; SFLNXTCHG on WRITE/UPDATE marks a
    // record changed for the next READC after the next output operation.
    #[test]
    fn readc_returns_changed_records_in_order() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 5);
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31, 33]))
            .unwrap();
        let mut it = Interaction::new(&fr);
        // Option on record 3 (page 1), then page down and an option on record 5.
        assert!(matches!(
            it.input(&input(Aid::RollUp, &[("3.OPT", "5")])),
            Step::Again
        ));
        let Step::Done(resp) = it.input(&input(Aid::Enter, &[("5.OPT", "X")])) else {
            panic!("Enter ends the EXFMT")
        };
        assert_eq!(resp.changed.keys().copied().collect::<Vec<_>>(), [3, 5]);
        ws.apply(&f, "CUSTCTL", &resp);
        let (r, v) = ws.sfl_readc(&f, "CUSTSFL").unwrap().unwrap();
        assert_eq!((r, v["OPT"].as_str()), (3, "5"));
        // UPDATE with SFLNXTCHG (*IN50) keeps the record for the next READC.
        ws.sfl_update(&f, "CUSTSFL", v, ind(&[50])).unwrap();
        let (r, _) = ws.sfl_readc(&f, "CUSTSFL").unwrap().unwrap();
        assert_eq!(r, 5);
        assert!(ws.sfl_readc(&f, "CUSTSFL").unwrap().is_none());
        // Next EXFMT: nothing typed, only the SFLNXTCHG record is changed.
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31, 33]))
            .unwrap();
        let Step::Done(resp) = Interaction::new(&fr).input(&input(Aid::Enter, &[])) else {
            panic!()
        };
        ws.apply(&f, "CUSTCTL", &resp);
        assert_eq!(ws.sfl_readc(&f, "CUSTSFL").unwrap().unwrap().0, 3);
        assert!(ws.sfl_readc(&f, "CUSTSFL").unwrap().is_none());
    }

    // IBM DDS reference (CAnn): command attention keys return no input data.
    #[test]
    fn ca_keys_return_no_subfile_data() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 2);
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31]))
            .unwrap();
        let Step::Done(resp) = Interaction::new(&fr).input(&input(Aid::F(12), &[("1.OPT", "5")]))
        else {
            panic!()
        };
        assert_eq!(resp.indicators.get(&12), Some(&true));
        assert!(resp.changed.is_empty());
    }

    // IBM DDS reference (SFLMSG): the message shows when the control record is
    // displayed with its condition on; its response indicator is set off on input.
    #[test]
    fn sflmsg_shows_and_its_indicator_is_reset() {
        let f = file();
        let mut ws = Workstation::new();
        load(&mut ws, &f, 2);
        let fr = ws
            .frame(&f, "CUSTCTL", BTreeMap::new(), ind(&[31, 40]))
            .unwrap();
        let mut it = Interaction::new(&fr);
        assert!(text(&it.screen(&ctx())).contains("Type option 5 or leave the option blank."));
        let Step::Done(resp) = it.input(&input(Aid::Enter, &[])) else {
            panic!()
        };
        assert_eq!(resp.indicators.get(&40), Some(&false));
    }
}
