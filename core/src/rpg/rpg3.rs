//! RPG III (RPG/400) source: converted to ILE RPG (RPG IV) fixed form, as
//! IBM's conversion aid CVTRPGSRC does (ILE RPG Programmer's Guide,
//! "Using the RPG III to RPG IV Conversion Aid"), then compiled by the
//! RPG IV engine. CRTRPGPGM uses this; CVTRPGSRC writes the result to a
//! source member.
//!
//! Positions of the RPG III specifications are those of the RPG/400
//! Reference; positions of the RPG IV ones those of the ILE RPG Reference.
//!
//! - H: debug, currency symbol, date format and edit, decimal notation,
//!   alternate collating sequence, forms alignment, file translation →
//!   keywords.
//! - F: positions moved; overflow indicator, key location, external
//!   indicator, SPECIAL routine, line counter (L) → keywords; `K`
//!   continuation lines (RENAME, SFILE, INFDS, INFSR, IGNORE, INCLUD,
//!   PLIST, PRTCTL, ID, SLN, SAVDS, IND, RECNO, COMIT) → keywords.
//! - E: arrays and tables → D specs (DIM, CTDATA, FROMFILE, TOFILE,
//!   PERRCD, ALT, ASCEND/DESCEND); an array that is also a data structure
//!   subfield becomes that subfield.
//! - I: data structures, subfields and named constants → D specs; record
//!   and field lines stay I specs, positions moved.
//! - C: positions moved; several conditioning indicators → AN lines; the
//!   operation codes renamed (SETOF → SETOFF, REDPE → READPE...); position
//!   53 (H, N, P) → operation extenders; `ARR,I` and `*IN,n` → `ARR(I)`,
//!   `*IN(n)`.
//! - O: positions moved, skip entries A0-B2 → 100-112.
//! - Compile-time data: `**` → `**CTDATA name` in the order of the arrays.

use anyhow::Result;

/// Gives the text of a /COPY member by its id (`FILE,MEMBER`).
pub type CopyFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The converted member and what the conversion reported.
#[derive(Debug, Default)]
pub struct Converted {
    pub source: String,
    /// Conversion messages (as in the conversion report), with the line.
    pub messages: Vec<String>,
}

fn pad(line: &str) -> Vec<char> {
    let mut v: Vec<char> = line.trim_end_matches('\r').chars().collect();
    if v.len() < 80 {
        v.resize(80, ' ');
    }
    v
}

/// Positions `from`-`to` (1-based, inclusive) as written.
fn col(cs: &[char], from: usize, to: usize) -> String {
    cs.iter()
        .skip(from - 1)
        .take(to + 1 - from)
        .collect::<String>()
}

fn colt(cs: &[char], from: usize, to: usize) -> String {
    col(cs, from, to).trim().to_string()
}

/// A line under construction: text placed at positions.
struct Out(Vec<char>);

impl Out {
    fn new(seq: &str, spec: char) -> Out {
        let mut v: Vec<char> = format!("{seq:<5}").chars().take(5).collect();
        v.push(spec);
        Out(v)
    }
    fn put(&mut self, pos: usize, text: &str) -> &mut Out {
        let chars: Vec<char> = text.chars().collect();
        let end = pos - 1 + chars.len();
        if self.0.len() < end {
            self.0.resize(end, ' ');
        }
        self.0[pos - 1..end].copy_from_slice(&chars);
        self
    }
    /// Right-adjusted in positions `from`-`to`.
    fn right(&mut self, from: usize, to: usize, text: &str) -> &mut Out {
        let t = text.trim();
        if t.is_empty() {
            return self;
        }
        let w = to + 1 - from;
        let t: String = t.chars().take(w).collect();
        self.put(from, &format!("{t:>w$}"))
    }
    fn text(&self) -> String {
        self.0.iter().collect::<String>().trim_end().to_string()
    }
}

/// RPG III operation codes with other names in RPG IV.
fn rename_op(op: &str) -> &str {
    match op {
        "BITOF" => "BITOFF",
        "CHEKR" => "CHECKR",
        "COMIT" => "COMMIT",
        "DEFN" => "DEFINE",
        "DELET" => "DELETE",
        "EXCPT" => "EXCEPT",
        "LOKUP" => "LOOKUP",
        "OCUR" => "OCCUR",
        "REDPE" => "READPE",
        "RETRN" => "RETURN",
        "SELEC" => "SELECT",
        "SETOF" => "SETOFF",
        "UNLCK" => "UNLOCK",
        "UPDAT" => "UPDATE",
        other => other,
    }
}

/// `ARR,I` → `ARR(I)`, `*IN,nn` → `*IN(nn)` outside literals.
fn convert_index(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('\'') || !t.contains(',') {
        return t.to_string();
    }
    // A numeric literal with a decimal comma stays as written.
    if t.chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '-')
    {
        return t.to_string();
    }
    match t.split_once(',') {
        Some((a, b)) if !a.is_empty() && !b.is_empty() && !b.contains(':') => {
            format!("{a}({b})")
        }
        _ => t.to_string(),
    }
}

/// A factor that may be `value:value` (SCAN, SUBST, CAT...): each part
/// converted.
fn convert_factor(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('\'') {
        // A literal, possibly followed by :n.
        if let Some(end) = t.strip_prefix('\'').and_then(|r| r.rfind('\'')) {
            let (lit, rest) = t.split_at(end + 2);
            return match rest.strip_prefix(':') {
                Some(r) => format!("{lit}:{}", convert_factor(r)),
                None => t.to_string(),
            };
        }
        return t.to_string();
    }
    match t.split_once(':') {
        Some((a, b)) => format!("{}:{}", convert_index(a), convert_factor(b)),
        None => convert_index(t),
    }
}

/// An array or table from an E spec.
#[derive(Clone, Debug)]
struct Array {
    name: String,
    /// D spec length, type, decimals.
    len: String,
    typ: char,
    decs: String,
    keywords: Vec<String>,
    alt: Option<Box<Array>>,
    line: String,
}

/// The RPG IV length, type and decimals of an RPG III entry: data
/// format (blank, P, B), length in the record, decimal positions.
fn entry_type(fmt: char, len: usize, decs: &str) -> (String, char, String) {
    match fmt {
        'P' => (((len * 2).max(2) - 1).to_string(), 'P', decs_or_zero(decs)),
        'B' => (
            if len <= 2 { "4" } else { "9" }.to_string(),
            'B',
            decs_or_zero(decs),
        ),
        _ if decs.is_empty() => (len.to_string(), ' ', String::new()),
        _ => (len.to_string(), 'S', decs.to_string()),
    }
}

fn decs_or_zero(d: &str) -> String {
    if d.is_empty() {
        "0".into()
    } else {
        d.into()
    }
}

/// A D spec.
#[allow(clippy::too_many_arguments)]
fn d_line(
    seq: &str,
    name: &str,
    ext: bool,
    ds_type: char,
    def: &str,
    from: &str,
    to: &str,
    typ: char,
    decs: &str,
    kws: &[String],
) -> Vec<String> {
    let mut o = Out::new(seq, 'D');
    o.put(7, name);
    if ext {
        o.put(22, "E");
    }
    if ds_type != ' ' {
        o.put(23, &ds_type.to_string());
    }
    o.put(24, def);
    // A special keyword (*STATUS...) takes the from and to positions.
    if from.starts_with('*') {
        o.put(26, from);
    } else {
        o.right(26, 32, from);
    }
    o.right(33, 39, to);
    if typ != ' ' {
        o.put(40, &typ.to_string());
    }
    o.right(41, 42, decs);
    // Keywords: positions 44-80, more on continuation lines.
    let mut lines = Vec::new();
    let mut cur = o;
    let mut at = 44;
    for k in kws {
        if at > 44 && at + k.len() > 80 {
            lines.push(cur.text());
            cur = Out::new("", 'D');
            at = 44;
        }
        cur.put(at, k);
        at += k.len() + 1;
    }
    lines.push(cur.text());
    lines
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

struct FileOut {
    name: String,
    before: Vec<String>,
    line: String,
    kws: Vec<String>,
    after: Vec<String>,
}

#[derive(Default)]
struct Conv {
    messages: Vec<String>,
    h: Vec<String>,
    /// F specs by file name: comments before, the F line, keywords (its
    /// own, from L specs and continuation lines), comments after.
    f: Vec<FileOut>,
    arrays: Vec<Array>,
    /// D specs from I specs (data structures, constants).
    d: Vec<String>,
    i: Vec<String>,
    c: Vec<String>,
    o: Vec<String>,
    /// Comments and directives waiting for the next specification.
    pending: Vec<String>,
    /// Inside a data structure (I specs).
    in_ds: bool,
    /// Array names used as data structure subfields.
    in_ds_arrays: Vec<String>,
    ctdata: Vec<String>,
    altseq: bool,
    ftrans: bool,
}

impl Conv {
    fn warn(&mut self, line: usize, msg: &str) {
        self.messages.push(format!("line {line}: {msg}"));
    }

    fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }

    fn h_spec(&mut self, cs: &[char], seq: &str) {
        let ch = |p: usize| cs[p - 1];
        let mut k = Vec::new();
        if ch(15) == '1' {
            k.push("DEBUG".to_string());
        }
        if ch(18) != ' ' {
            k.push(format!("CURSYM({})", quote(&ch(18).to_string())));
        }
        let fmt = match ch(19) {
            'D' => Some("*DMY"),
            'Y' => Some("*YMD"),
            'M' => Some("*MDY"),
            _ => None,
        };
        if fmt.is_some() || ch(20) != ' ' {
            let sep = if ch(20) == ' ' {
                String::new()
            } else {
                ch(20).to_string()
            };
            k.push(format!("DATEDIT({}{sep})", fmt.unwrap_or("*MDY")));
        }
        match ch(21) {
            'I' => k.push("DECEDIT(',')".into()),
            'J' => k.push("DECEDIT('0,')".into()),
            'D' => k.push("DECEDIT('0.')".into()),
            _ => {}
        }
        if ch(26) == 'S' {
            self.altseq = true;
            k.push("ALTSEQ(*SRC)".into());
        }
        if ch(41) == '1' {
            k.push("FORMSALIGN(*YES)".into());
        }
        if ch(43) == 'F' {
            self.ftrans = true;
            k.push("FTRANS(*SRC)".into());
        }
        let mut lines = self.take_pending();
        let mut o = Out::new(seq, 'H');
        o.put(7, &k.join(" "));
        lines.push(o.text());
        self.h.extend(lines);
    }

    fn f_spec(&mut self, cs: &[char], seq: &str, lineno: usize) {
        let ch = |p: usize| cs[p - 1];
        let name = colt(cs, 7, 14);
        // A continuation line: K in position 53.
        if ch(53) == 'K' && name.is_empty() {
            let option = colt(cs, 54, 59).to_ascii_uppercase();
            let entry = colt(cs, 60, 67);
            let kw = match option.as_str() {
                "RENAME" => Some(format!("RENAME({}:{entry})", colt(cs, 19, 28))),
                "SFILE" => Some(format!("SFILE({entry}:{})", colt(cs, 47, 52))),
                "INFDS" => Some(format!("INFDS({entry})")),
                "INFSR" => Some(format!("INFSR({entry})")),
                "IGNORE" => Some(format!("IGNORE({entry})")),
                "INCLUD" => Some(format!("INCLUDE({entry})")),
                "PLIST" => Some(format!("PLIST({entry})")),
                "PRTCTL" => Some(format!("PRTCTL({entry})")),
                "ID" => Some(format!("DEVID({entry})")),
                "SLN" => Some(format!("SLN({entry})")),
                "SAVDS" => Some(format!("SAVEDS({entry})")),
                "IND" => Some(format!("SAVEIND({entry})")),
                "RECNO" => Some(format!("RECNO({entry})")),
                "COMIT" if entry.is_empty() => Some("COMMIT".into()),
                "COMIT" => Some(format!("COMMIT(*IN{entry})")),
                "FMTS" => None,
                "NUM" => Some(format!("MAXDEV({entry})")),
                "PASS" => Some("PASS(*NOIND)".into()),
                _ => None,
            };
            match (kw, self.f.last_mut()) {
                (Some(k), Some(f)) => f.kws.push(k),
                _ => self.warn(
                    lineno,
                    &format!("file continuation option {option} is not converted"),
                ),
            }
            let pend = self.take_pending();
            if let Some(f) = self.f.last_mut() {
                f.after.extend(pend);
            }
            return;
        }
        let mut o = Out::new(seq, 'F');
        o.put(7, &name);
        o.put(17, &ch(15).to_string());
        o.put(18, &ch(16).to_string());
        o.put(19, &ch(17).to_string());
        if ch(66) == 'A' {
            o.put(20, "A");
        }
        o.put(21, &ch(18).to_string());
        o.put(22, &ch(19).to_string());
        o.right(23, 27, &col(cs, 24, 27));
        o.put(28, &ch(28).to_string());
        o.right(29, 33, &col(cs, 29, 30));
        o.put(34, &ch(31).to_string());
        o.put(35, &ch(32).to_string());
        let device = colt(cs, 40, 46).to_ascii_uppercase();
        o.put(36, &device);
        let mut kws = Vec::new();
        let ofl = colt(cs, 33, 34).to_ascii_uppercase();
        if !ofl.is_empty() {
            kws.push(format!("OFLIND(*IN{ofl})"));
        }
        let keyloc = colt(cs, 35, 38);
        if !keyloc.is_empty() {
            kws.push(format!("KEYLOC({keyloc})"));
        }
        let ext = colt(cs, 71, 72).to_ascii_uppercase();
        if !ext.is_empty() {
            kws.push(format!("EXTIND(*IN{ext})"));
        }
        if device == "SPECIAL" {
            let pgm = colt(cs, 54, 59);
            if !pgm.is_empty() {
                kws.push(format!("PGMNAME({})", quote(&pgm)));
            }
        }
        let before = self.take_pending();
        self.f.push(FileOut {
            name: name.to_ascii_uppercase(),
            before,
            line: o.text(),
            kws,
            after: vec![],
        });
    }

    fn e_spec(&mut self, cs: &[char], seq: &str, lineno: usize) {
        let ch = |p: usize| cs[p - 1];
        let name = colt(cs, 27, 32).to_ascii_uppercase();
        if name.is_empty() {
            // Record address files and the like.
            self.warn(
                lineno,
                "an extension specification without an array name is not converted",
            );
            return;
        }
        let from = colt(cs, 11, 18);
        let to = colt(cs, 19, 26);
        let perrcd = colt(cs, 33, 35);
        let entries = colt(cs, 36, 39);
        let len: usize = colt(cs, 40, 42).parse().unwrap_or(0);
        let (dlen, typ, decs) = entry_type(ch(43), len, &colt(cs, 44, 44));
        let mut kws = vec![format!("DIM({entries})")];
        if !from.is_empty() {
            kws.push(format!("FROMFILE({from})"));
        } else if !perrcd.is_empty() {
            kws.push("CTDATA".into());
        }
        if !perrcd.is_empty() {
            kws.push(format!("PERRCD({perrcd})"));
        }
        if !to.is_empty() {
            kws.push(format!("TOFILE({to})"));
        }
        match ch(45) {
            'A' => kws.push("ASCEND".into()),
            'D' => kws.push("DESCEND".into()),
            _ => {}
        }
        let alt_name = colt(cs, 46, 51).to_ascii_uppercase();
        let alt = if alt_name.is_empty() {
            None
        } else {
            let alen: usize = colt(cs, 52, 54).parse().unwrap_or(0);
            let (l, t, d) = entry_type(ch(55), alen, &colt(cs, 56, 56));
            let mut ak = vec![format!("DIM({entries})"), format!("ALT({name})")];
            match ch(57) {
                'A' => ak.push("ASCEND".into()),
                'D' => ak.push("DESCEND".into()),
                _ => {}
            }
            Some(Box::new(Array {
                name: alt_name,
                len: l,
                typ: t,
                decs: d,
                keywords: ak,
                alt: None,
                line: seq.to_string(),
            }))
        };
        let pend = self.take_pending();
        self.d.extend(pend);
        self.arrays.push(Array {
            name,
            len: dlen,
            typ,
            decs,
            keywords: kws,
            alt,
            line: seq.to_string(),
        });
    }

    fn l_spec(&mut self, cs: &[char], lineno: usize) {
        let file = colt(cs, 7, 14).to_ascii_uppercase();
        let formlen = colt(cs, 15, 17);
        let ofl = colt(cs, 20, 22);
        match self.f.iter_mut().find(|f| f.name == file) {
            Some(f) => {
                if !formlen.is_empty() {
                    f.kws.push(format!("FORMLEN({formlen})"));
                }
                if !ofl.is_empty() {
                    f.kws.push(format!("FORMOFL({ofl})"));
                }
            }
            None => self.warn(lineno, &format!("line counter for {file}: no such file")),
        }
    }

    fn i_spec(&mut self, cs: &[char], seq: &str, lineno: usize) {
        let ch = |p: usize| cs[p - 1];
        let pend = self.take_pending();
        // A data structure.
        if col(cs, 19, 20).eq_ignore_ascii_case("DS") {
            self.in_ds = true;
            self.d.extend(pend);
            let name = colt(cs, 7, 12).to_ascii_uppercase();
            let ext = ch(17) == 'E';
            let mut kws = Vec::new();
            if ext {
                let f = colt(cs, 21, 30);
                kws.push(format!(
                    "EXTNAME({})",
                    if f.is_empty() { &name } else { &f }
                ));
            }
            let ds_type = match ch(18) {
                'U' => 'U',
                'S' => 'S',
                _ => ' ',
            };
            if ch(18) == 'I' {
                // Initialized data structure.
                kws.push("INZ".into());
            }
            let occurs = colt(cs, 44, 47);
            if !occurs.is_empty() {
                kws.push(format!("OCCURS({occurs})"));
            }
            let len = colt(cs, 48, 51);
            self.d.extend(d_line(
                seq, &name, ext, ds_type, "DS", "", &len, ' ', "", &kws,
            ));
            return;
        }
        // A named constant.
        if ch(43) == 'C' && !colt(cs, 53, 58).is_empty() {
            self.d.extend(pend);
            let name = colt(cs, 53, 58).to_ascii_uppercase();
            let value = colt(cs, 21, 42);
            self.d.extend(d_line(
                seq,
                &name,
                false,
                ' ',
                "C",
                "",
                "",
                ' ',
                "",
                &[format!("CONST({value})")],
            ));
            return;
        }
        let name7 = colt(cs, 7, 14);
        let and_or = colt(cs, 14, 16).to_ascii_uppercase();
        let record_line = !name7.is_empty() && !(and_or == "AND" || and_or == "OR")
            || and_or == "AND"
            || and_or == "OR";
        if self.in_ds && !record_line {
            self.d.extend(pend);
            self.ds_subfield(cs, seq);
            return;
        }
        self.in_ds = false;
        self.i.extend(pend);
        let mut o = Out::new(seq, 'I');
        if record_line {
            if and_or == "AND" || and_or == "OR" {
                o.put(16, &and_or);
            } else {
                o.put(7, &name7);
                o.put(17, &col(cs, 15, 16));
            }
            o.put(19, &ch(17).to_string());
            o.put(20, &ch(18).to_string());
            o.put(21, &col(cs, 19, 20));
            // Record identification codes: three groups.
            for (g3, g4) in [(21, 23), (28, 31), (35, 39)] {
                let pos = colt(cs, g3, g3 + 3);
                if pos.is_empty() {
                    continue;
                }
                o.right(g4, g4 + 4, &pos);
                o.put(g4 + 5, &ch(g3 + 4).to_string());
                o.put(g4 + 6, &ch(g3 + 5).to_string());
                o.put(g4 + 7, &ch(g3 + 6).to_string());
            }
            if !colt(cs, 43, 74).is_empty() {
                self.warn(
                    lineno,
                    "input record line: positions 43-74 are not converted",
                );
            }
        } else {
            // A field line: program-described (from, to) or externally
            // described (external name in 21-30).
            o.put(21, &col(cs, 21, 30));
            let f = ch(43);
            if f != ' ' {
                o.put(36, &f.to_string());
            }
            o.right(37, 41, &col(cs, 44, 47));
            o.right(42, 46, &col(cs, 48, 51));
            o.right(47, 48, &col(cs, 52, 52));
            o.put(49, &convert_index(&colt(cs, 53, 58)));
            o.put(63, &col(cs, 59, 60));
            o.put(65, &col(cs, 61, 62));
            o.put(67, &col(cs, 63, 64));
            o.put(69, &col(cs, 65, 70));
        }
        self.i.push(o.text());
    }

    fn ds_subfield(&mut self, cs: &[char], seq: &str) {
        let ch = |p: usize| cs[p - 1];
        let name = colt(cs, 53, 58).to_ascii_uppercase();
        let ext_name = colt(cs, 21, 30);
        let from = colt(cs, 44, 47);
        let to = colt(cs, 48, 51);
        let mut kws = Vec::new();
        // Special keywords of the program status and file information
        // data structures: *STATUS, *ROUTINE, *PARMS...
        let special = colt(cs, 44, 51);
        if special.starts_with('*') {
            self.d.extend(d_line(
                seq,
                &name,
                false,
                ' ',
                "",
                &special,
                "",
                ' ',
                "",
                &[],
            ));
            return;
        }
        if ch(8) == 'I' {
            let v = colt(cs, 21, 42);
            kws.push(if v.is_empty() {
                "INZ".to_string()
            } else {
                format!("INZ({v})")
            });
        }
        // A renamed field of an external data structure.
        if !ext_name.is_empty() && ch(8) != 'I' {
            kws.push(format!("EXTFLD({ext_name})"));
            self.d
                .extend(d_line(seq, &name, true, ' ', "", "", "", ' ', "", &kws));
            return;
        }
        let decs = colt(cs, 52, 52);
        let typ = match ch(43) {
            'P' => 'P',
            'B' => 'B',
            _ if !decs.is_empty() => 'S',
            _ => ' ',
        };
        // An array of the E specs placed in the data structure.
        if let Some(a) = self.arrays.iter().find(|a| a.name == name).cloned() {
            self.in_ds_arrays.push(name.clone());
            let mut k = a.keywords.clone();
            k.extend(kws);
            self.d.extend(d_line(
                seq, &name, false, ' ', "", &from, &to, typ, &decs, &k,
            ));
            if let Some(alt) = &a.alt {
                self.d.extend(d_line(
                    &alt.line,
                    &alt.name,
                    false,
                    ' ',
                    "S",
                    "",
                    &alt.len,
                    alt.typ,
                    &alt.decs,
                    &alt.keywords,
                ));
            }
            return;
        }
        self.d.extend(d_line(
            seq,
            &convert_index(&name),
            false,
            ' ',
            "",
            &from,
            &to,
            typ,
            &decs,
            &kws,
        ));
    }

    fn c_spec(&mut self, cs: &[char], seq: &str, lineno: usize) {
        let ch = |p: usize| cs[p - 1];
        let pend = self.take_pending();
        self.c.extend(pend);
        let level = col(cs, 7, 8);
        let inds: Vec<String> = [9, 12, 15]
            .iter()
            .map(|&p| col(cs, p, p + 2))
            .filter(|s| !s.trim().is_empty())
            .collect();
        let op3 = colt(cs, 28, 32).to_ascii_uppercase();
        let mut op = rename_op(&op3).to_string();
        // Position 53: half adjust, no lock, pad.
        let ext = match (ch(53).to_ascii_uppercase(), op.as_str()) {
            ('H', _) => "H",
            ('N', "CHAIN" | "READ" | "READE" | "READP" | "READPE" | "DELETE") => "N",
            ('P', _) => "P",
            (' ', _) => "",
            (c, _) => {
                self.warn(lineno, &format!("position 53 '{c}' is not converted"));
                ""
            }
        };
        match op.as_str() {
            "DEBUG" | "FREE" => {
                self.warn(
                    lineno,
                    &format!("operation {op} does not exist in RPG IV: removed"),
                );
                self.c
                    .push(format!("{seq:<5}C*  {}", col(cs, 7, 74).trim_end()));
                return;
            }
            _ => {}
        }
        if !ext.is_empty() {
            op = format!("{op}({ext})");
        }
        // Conditioning indicators beyond the first go on lines before the
        // operation, joined with AN.
        let mut lines = Vec::new();
        for (k, ind) in inds.iter().enumerate() {
            if k + 1 == inds.len() {
                break;
            }
            let mut o = Out::new(seq, 'C');
            o.put(7, if k == 0 { &level } else { "AN" });
            o.put(9, ind);
            lines.push(o.text());
        }
        let mut o = Out::new(seq, 'C');
        o.put(7, if inds.len() > 1 { "AN" } else { &level });
        if let Some(last) = inds.last() {
            o.put(9, last);
        }
        let f1 = colt(cs, 18, 27);
        let f2 = colt(cs, 33, 42);
        let res = colt(cs, 43, 48);
        o.put(12, &convert_factor(&f1));
        o.put(26, &op);
        o.put(36, &convert_factor(&f2));
        o.put(50, &convert_index(&res));
        o.right(64, 68, &col(cs, 49, 51));
        o.right(69, 70, &col(cs, 52, 52));
        o.put(71, &col(cs, 54, 59));
        let comment = colt(cs, 60, 74);
        if !comment.is_empty() {
            o.put(81, &comment);
        }
        lines.push(o.text());
        self.c.extend(lines);
    }

    fn o_spec(&mut self, cs: &[char], seq: &str) {
        let ch = |p: usize| cs[p - 1];
        let pend = self.take_pending();
        self.o.extend(pend);
        let name = colt(cs, 7, 14);
        let and_or = colt(cs, 14, 16).to_ascii_uppercase();
        let is_and_or = and_or == "AND" || and_or == "OR";
        let record = is_and_or || !name.is_empty() || !colt(cs, 15, 22).is_empty();
        let mut o = Out::new(seq, 'O');
        if record {
            if is_and_or {
                o.put(16, &and_or);
            } else {
                o.put(7, &name);
                o.put(17, &ch(15).to_string());
                let addel = colt(cs, 16, 18).to_ascii_uppercase();
                if addel == "ADD" || addel == "DEL" {
                    o.put(18, &addel);
                } else {
                    o.put(18, &ch(16).to_string());
                    o.right(40, 42, &col(cs, 17, 17));
                    o.right(43, 45, &col(cs, 18, 18));
                    o.right(46, 48, &skip(&colt(cs, 19, 20)));
                    o.right(49, 51, &skip(&colt(cs, 21, 22)));
                }
            }
            o.put(21, &col(cs, 23, 31));
            o.put(30, &colt(cs, 32, 37));
        } else {
            o.put(21, &col(cs, 23, 31));
            o.put(30, &convert_index(&colt(cs, 32, 37)));
            o.put(44, &ch(38).to_string());
            o.put(45, &ch(39).to_string());
            o.right(47, 51, &col(cs, 40, 43));
            o.put(52, &ch(44).to_string());
            o.put(53, col(cs, 45, 70).trim_end());
        }
        self.o.push(o.text());
    }
}

/// Skip entries 01-99, A0-A9 (100-109), B0-B2 (110-112).
fn skip(s: &str) -> String {
    let u = s.to_ascii_uppercase();
    let mut c = u.chars();
    match (c.next(), c.next()) {
        (Some('A'), Some(d)) if d.is_ascii_digit() => format!("1{}{d}", 0),
        (Some('B'), Some(d)) if d.is_ascii_digit() => format!("11{d}"),
        _ => u.trim_start_matches('0').to_string(),
    }
}

/// Convert an RPG III member. `copy` gives the text of `/COPY` members
/// (QRPGSRC by default) when they are to be expanded (EXPCPY(*YES), and
/// always when CRTRPGPGM compiles).
pub fn convert(source: &str, copy: Option<CopyFn>) -> Result<Converted> {
    let mut cv = Conv::default();
    let mut lines: Vec<(usize, String)> = Vec::new();
    expand(source, copy, &mut lines, 0, &mut cv)?;
    let mut in_data = false;
    for (lineno, raw) in &lines {
        let lineno = *lineno;
        if in_data {
            cv.ctdata.push(raw.clone());
            continue;
        }
        if raw.starts_with("**") {
            in_data = true;
            cv.ctdata.push(raw.clone());
            continue;
        }
        let cs = pad(raw);
        let seq = col(&cs, 1, 5);
        let spec = cs[5].to_ascii_uppercase();
        // Comments and directives keep their place.
        if cs[6] == '*' || cs[6] == '/' || raw.trim().is_empty() || spec == ' ' {
            let text = if cs[6] == '*' {
                let mapped = match spec {
                    'E' => 'D',
                    'L' => 'F',
                    ' ' => ' ',
                    other => other,
                };
                let mut v = cs.clone();
                v[5] = mapped;
                v.iter().take(80).collect::<String>().trim_end().to_string()
            } else {
                raw.trim_end().to_string()
            };
            cv.pending.push(text);
            continue;
        }
        match spec {
            'H' => cv.h_spec(&cs, &seq),
            'F' => cv.f_spec(&cs, &seq, lineno),
            'E' => cv.e_spec(&cs, &seq, lineno),
            'L' => cv.l_spec(&cs, lineno),
            'I' => cv.i_spec(&cs, &seq, lineno),
            'C' => cv.c_spec(&cs, &seq, lineno),
            'O' => cv.o_spec(&cs, &seq),
            other => cv.warn(lineno, &format!("specification type {other} is not valid")),
        }
    }
    // Assemble in RPG IV order: H, F, D, I, C, O, compile-time data.
    let mut out: Vec<String> = Vec::new();
    out.append(&mut cv.h);
    for f in &cv.f {
        out.extend(f.before.iter().cloned());
        // Keywords from position 44, more on continuation lines.
        let mut cur = Out(pad(&f.line).into_iter().take(43).collect());
        let mut at = 44;
        for k in &f.kws {
            if at > 44 && at + k.len() > 81 {
                out.push(cur.text());
                cur = Out::new("", 'F');
                at = 44;
            }
            cur.put(at, k);
            at += k.len() + 1;
        }
        out.push(cur.text());
        out.extend(f.after.iter().cloned());
    }
    // Arrays not placed in a data structure.
    for a in &cv.arrays {
        if cv.in_ds_arrays.contains(&a.name) {
            continue;
        }
        out.extend(d_line(
            &a.line,
            &a.name,
            false,
            ' ',
            "S",
            "",
            &a.len,
            a.typ,
            &a.decs,
            &a.keywords,
        ));
        if let Some(alt) = &a.alt {
            out.extend(d_line(
                &alt.line,
                &alt.name,
                false,
                ' ',
                "S",
                "",
                &alt.len,
                alt.typ,
                &alt.decs,
                &alt.keywords,
            ));
        }
    }
    out.append(&mut cv.d);
    out.append(&mut cv.i);
    out.append(&mut cv.c);
    out.append(&mut cv.o);
    out.append(&mut cv.pending);
    // Compile-time data: ALTSEQ and FTRANS records first when the control
    // specification asks for them, then the arrays in the order of their
    // definitions.
    if !cv.ctdata.is_empty() {
        let mut names: Vec<String> = Vec::new();
        if cv.altseq {
            names.push("**ALTSEQ".into());
        }
        if cv.ftrans {
            names.push("**FTRANS".into());
        }
        for a in &cv.arrays {
            if a.keywords.iter().any(|k| k == "CTDATA") {
                names.push(format!("**CTDATA {}", a.name));
            }
        }
        let mut k = 0;
        for l in &cv.ctdata {
            if l.starts_with("**") {
                match names.get(k) {
                    Some(n) => out.push(n.clone()),
                    None => {
                        cv.messages
                            .push("compile-time data with no array for it".to_string());
                        out.push(l.clone());
                    }
                }
                k += 1;
            } else {
                out.push(l.clone());
            }
        }
    }
    let mut source = out.join("\n");
    source.push('\n');
    Ok(Converted {
        source,
        messages: cv.messages,
    })
}

/// The members the /COPY directives of a source name, as `convert` asks
/// for them.
pub fn copy_ids(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|raw| {
            let cs = pad(raw);
            if col(&cs, 7, 11).eq_ignore_ascii_case("/COPY") {
                Some(copy_id(&cs))
            } else {
                None
            }
        })
        .collect()
}

fn copy_id(cs: &[char]) -> String {
    let id = colt(cs, 13, 44).to_ascii_uppercase();
    let id = id.split_whitespace().next().unwrap_or("").to_string();
    if id.contains(',') {
        id
    } else {
        format!("QRPGSRC,{id}")
    }
}

/// The source with /COPY members expanded (RPG III: `/COPY
/// LIB/FILE,MEMBER`, `/COPY FILE,MEMBER`, `/COPY MEMBER` from QRPGSRC).
fn expand(
    source: &str,
    copy: Option<CopyFn>,
    out: &mut Vec<(usize, String)>,
    depth: usize,
    cv: &mut Conv,
) -> Result<()> {
    for (i, raw) in source.lines().enumerate() {
        let raw = raw.trim_end_matches('\r');
        let cs = pad(raw);
        let directive = col(&cs, 7, 11).to_ascii_uppercase();
        if directive == "/COPY" && !raw.starts_with("**") {
            if let Some(f) = copy {
                let id = copy_id(&cs);
                match f(&id) {
                    Some(text) if depth < 16 => {
                        expand(&text, copy, out, depth + 1, cv)?;
                        continue;
                    }
                    _ => cv.warn(i + 1, &format!("/COPY member {id} was not found")),
                }
            }
        }
        out.push((i + 1, raw.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(parts: &[(usize, &str)]) -> String {
        let mut o = Out(Vec::new());
        for (p, t) in parts {
            o.put(*p, t);
        }
        o.text()
    }

    #[test]
    fn calculation_positions_and_renamed_operations() {
        // RPG/400 Reference, calculation specifications: factor 1 in
        // 18-27, operation 28-32, factor 2 33-42, result 43-48, length
        // 49-51, decimals 52, half adjust 53, indicators 54-59. ILE RPG
        // Reference: factor 1 12-25, operation 26-35, factor 2 36-49,
        // result 50-63, length 64-68, decimals 69-70, indicators 71-76
        // (each expected line was checked position by position).
        let src = [
            line(&[
                (6, "C"),
                (9, "N01"),
                (18, "QTY"),
                (28, "MULT"),
                (33, "PRICE"),
                (43, "AMT"),
                (49, "  9"),
                (52, "2"),
                (53, "H"),
            ]),
            line(&[(6, "C"), (28, "SETOF"), (54, "  50  ")]),
            line(&[
                (6, "C"),
                (9, " 10"),
                (12, " 11"),
                (15, "N12"),
                (18, "KEY"),
                (28, "CHAIN"),
                (33, "CUSTMAS"),
                (54, "  90"),
            ]),
            line(&[
                (6, "C"),
                (18, "'A'"),
                (28, "LOKUP"),
                (33, "ARR,I"),
                (58, "20"),
            ]),
        ]
        .join("\n");
        let c = convert(&src, None).unwrap();
        let out: Vec<&str> = c.source.lines().collect();
        assert_eq!(
            out,
            [
                "     C  N01QTY           MULT(H)   PRICE         AMT               9 2",
                "     C                   SETOFF                                         50",
                "     C   10",
                "     CAN 11",
                "     CANN12KEY           CHAIN     CUSTMAS                              90",
                "     C     'A'           LOOKUP    ARR(I)                                 20",
            ]
        );
    }

    #[test]
    fn arrays_and_compile_time_data() {
        // RPG/400 Reference, extension specifications: array name 27-32,
        // entries per record 33-35, entries 36-39, length 40-42, decimal
        // positions 44, sequence 45, alternate 46-57.
        let e = line(&[
            (6, "E"),
            (27, "TABA"),
            (33, "  3"),
            (36, "   3"),
            (40, "  1"),
            (46, "TABB"),
            (52, "  2"),
            (56, "0"),
        ]);
        let src = format!("{e}\n**\nA01B02C03\n");
        let c = convert(&src, None).unwrap();
        let out: Vec<&str> = c.source.lines().collect();
        assert_eq!(
            out,
            [
                "     DTABA             S              1    DIM(3) CTDATA PERRCD(3)",
                "     DTABB             S              2S 0 DIM(3) ALT(TABA)",
                "**CTDATA TABA",
                "A01B02C03",
            ]
        );
    }

    #[test]
    fn data_structures_and_constants() {
        // RPG/400 Reference, input specifications for data structures: DS
        // in 19-20, occurrences 44-47, length 48-51; subfields: data format
        // 43, from 44-47, to 48-51, decimals 52, name 53-58; named
        // constants: value 21-42, C in 43, name 53-58.
        let src = [
            line(&[(6, "I"), (7, "DATE"), (19, "DS")]),
            line(&[(6, "I"), (44, "   1"), (48, "   2"), (52, "0"), (53, "MM")]),
            line(&[
                (6, "I"),
                (43, "P"),
                (44, "   3"),
                (48, "   5"),
                (52, "0"),
                (53, "YY"),
            ]),
            line(&[(6, "I"), (21, "'HELLO'"), (43, "C"), (53, "GREET")]),
        ]
        .join("\n");
        let c = convert(&src, None).unwrap();
        let out: Vec<&str> = c.source.lines().collect();
        assert_eq!(
            out,
            [
                "     DDATE             DS",
                "     DMM                       1      2S 0",
                "     DYY                       3      5P 0",
                "     DGREET            C                   CONST('HELLO')",
            ]
        );
    }

    #[test]
    fn output_positions() {
        // RPG/400 Reference, output specifications: type 15, space 17-18,
        // skip 19-22, indicators 23-31, field 32-37, edit code 38, end
        // position 40-43, constant 45-70.
        let src = [
            line(&[(6, "O"), (7, "QPRINT"), (15, "H"), (18, "2"), (19, "01")]),
            line(&[(6, "O"), (23, "N50"), (32, "AMT"), (38, "1"), (40, "  20")]),
            line(&[(6, "O"), (40, "  40"), (45, "'TOTAL'")]),
        ]
        .join("\n");
        let c = convert(&src, None).unwrap();
        let out: Vec<&str> = c.source.lines().collect();
        assert_eq!(
            out,
            [
                "     OQPRINT    H                           2  1",
                "     O              N50      AMT           1     20",
                "     O                                           40 'TOTAL'",
            ]
        );
    }
}
