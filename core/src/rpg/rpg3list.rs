//! The RPG/400 compiler listing (CRTRPGPGM): the command's options, the
//! source with indicator usage and DO nesting, the input fields of
//! externally described files, the messages, a cross reference of files,
//! fields, literals and indicators, a message summary and a final summary.
//!
//! Layout observed on IBM i 7.5 (PUB400, 2026-10-22, tools/golden/
//! listings2.txt). Messages are as4tainr's own texts: QRG7031 (a field
//! not used), QRG7086 (records blocked).

use std::collections::BTreeMap;

use crate::report::{at, Stamp};

/// The command's values the listing shows.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub program: String,
    pub library: String,
    pub text: String,
    pub replace: String,
}

/// An externally described file's record: its fields as stored (name,
/// data type letter P/S/B or blank, from, to, decimals).
#[derive(Debug, Clone, Default)]
pub struct ExtFile {
    pub file: String,
    pub library: String,
    pub format: String,
    pub fields: Vec<(String, char, u32, u32, Option<u32>)>,
}

const PAGE: usize = 52;

/// Columns `from`-`to` (1-based) of a source line, trimmed.
fn col(cs: &[char], from: usize, to: usize) -> String {
    cs.iter()
        .skip(from - 1)
        .take(to + 1 - from)
        .collect::<String>()
        .trim()
        .to_string()
}

/// References: (reference, flag).
type Refs = Vec<(String, char)>;
/// A file's references: name, device, its references, and each record
/// format's.
type FileRefs = (String, String, Refs, Vec<(String, Refs)>);

/// A row of the source section.
enum Row {
    Src(usize),
    Text(String),
    /// An input field line.
    Input(String),
}

/// The listing; `created`: whether the program was created.
pub fn listing(
    o: &Opts,
    m: &crate::source::Member,
    files: &[ExtFile],
    created: bool,
    stamp: &Stamp,
) -> Vec<String> {
    let header = |page: usize| {
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (40, "RPG/400"),
            (70, &format!("{}/{}", o.library, o.program)),
            (95, &format!("{}  {}", stamp.date, stamp.time)),
            (119, &format!("Page {:>7}", page)),
        ])
    };
    let label = |indent: usize, t: &str, v: &str| {
        at(&[
            (indent, &crate::commands::dsp_label(t, 35 - indent)),
            (41, v),
        ])
    };
    let changed = m
        .changed_us
        .map(|us| {
            let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::from_epoch_us(us);
            let st = Stamp::of((y, mo, d), (h, mi, s));
            format!("{}  {}", st.date, st.time)
        })
        .unwrap_or_default();
    let mut pro = vec![
        label(2, "Compiler", "as4tainr RPG/400"),
        " Command Options:".to_string(),
        label(4, "Program", &format!("{}/{}", o.library, o.program)),
        label(4, "Source file", &format!("{}/{}", m.library, m.file)),
        label(4, "Source member", &m.name),
        label(
            4,
            "Source listing options",
            "*SOURCE     *XREF       *GEN        *NODUMP     *NOSECLVL   *NOSRCDBG   *NOLSTDBG",
        ),
        label(
            4,
            "Generation options",
            "*NOLIST     *NOXREF     *NOATR      *NODUMP     *NOOPTIMIZE",
        ),
    ];
    for (t, v) in [
        ("Source listing indentation", "*NONE"),
        ("Type conversion options", "*NONE"),
        ("Sort sequence", "*HEX"),
        ("Language identifier", "*JOBRUN"),
        ("SAA flagging", "*NOFLAG"),
        ("Generation severity level", "9"),
        ("Print file", "*LIBL/QSYSPRT"),
        ("Replace program", &o.replace),
        ("Target release", "*CURRENT"),
        ("User profile", "*USER"),
        ("Authority", "*LIBCRTAUT"),
        ("Text", &o.text),
        ("Phase trace", "*NO"),
        ("Intermediate text dump", "*NONE"),
        ("Snap dump", "*NONE"),
        ("Codelist", "*NONE"),
        ("Ignore decimal data error", "*NO"),
        ("Allow null values", "*NO"),
    ] {
        pro.push(label(4, t, v));
    }
    pro.push(" Actual Program Source:".into());
    for (t, v) in [
        ("Member", m.name.as_str()),
        ("File", m.file.as_str()),
        ("Library", m.library.as_str()),
        ("Last Change", changed.as_str()),
        ("Description", m.text.as_str()),
    ] {
        pro.push(label(4, t, v));
    }

    // ------------------------------------------------------------ source
    let lines: Vec<Vec<char>> = m.source.lines().map(|l| l.chars().collect()).collect();
    let spec = |cs: &Vec<char>| cs.get(5).map(|c| c.to_ascii_uppercase()).unwrap_or(' ');
    let comment = |cs: &Vec<char>| cs.get(6) == Some(&'*') || cs.iter().all(|c| *c == ' ');
    let has_h = lines.iter().any(|cs| spec(cs) == 'H' && !comment(cs));
    let first_other = lines
        .iter()
        .position(|cs| !comment(cs) && spec(cs) != 'H')
        .unwrap_or(lines.len());
    let first_calc = lines
        .iter()
        .position(|cs| !comment(cs) && spec(cs) == 'C')
        .unwrap_or(lines.len());
    let mut rows: Vec<Row> = Vec::new();
    for (i, cs) in lines.iter().enumerate() {
        if i == first_other && !has_h {
            // The control specification the compiler uses when there is
            // none.
            rows.push(Row::Text(at(&[(12, "H"), (107, "*****")])));
        }
        if i == first_calc {
            for (k, f) in files.iter().enumerate() {
                let letter = (b'A' + k as u8) as char;
                rows.push(Row::Input(format!(
                    "  {letter}000000   INPUT  FIELDS FOR RECORD {} FILE {} FORMAT {}.",
                    f.format, f.file, f.format
                )));
                for (j, (name, t, from, to, dec)) in f.fields.iter().enumerate() {
                    let id = format!("{letter}{:06}", j + 1);
                    let d = dec.map(|d| d.to_string()).unwrap_or_default();
                    let text = format!("  {id}{:39}{t}{from:>4}{to:>4}{d:1}{name}", "");
                    rows.push(Row::Input(text));
                }
            }
        }
        rows.push(Row::Src(i));
        if spec(cs) == 'F'
            && !comment(cs)
            && cs.get(18).is_some_and(|c| c.eq_ignore_ascii_case(&'E'))
        {
            let name = col(cs, 7, 14).to_ascii_uppercase();
            if let Some(f) = files.iter().find(|f| f.file == name) {
                rows.push(Row::Text(format!(
                    "            RECORD FORMAT(S):  LIBRARY {} FILE {}.",
                    f.library, f.file
                )));
                rows.push(Row::Text(format!(
                    "                     EXTERNAL FORMAT {} RPG NAME {}",
                    f.format, f.format
                )));
            }
        }
    }
    // Indicator usage (resulting indicators: 1 high, 2 low, 3 equal) and
    // DO nesting of each calculation.
    let mut level = 0u32;
    let mut out_src = vec![
        " SEQUENCE                                                                         IND    DO    LAST       PAGE   PROGRAM".to_string(),
        " NUMBER    *...1....+....2....+....3....+....4....+....5....+....6....+....7...*  USE    NUM   UPDATE     LINE   ID".into(),
        at(&[(27, "S o u r c e   L i s t i n g")]),
    ];
    let seq = |i: usize| format!("{:>9}", (i + 1) * 100);
    for r in &rows {
        match r {
            Row::Text(t) | Row::Input(t) => out_src.push(t.clone()),
            Row::Src(i) => {
                let cs = &lines[*i];
                let text: String = cs.iter().skip(5).take(75).collect();
                let mut line = format!("{}  {text}", seq(*i));
                if spec(cs) == 'C' && !comment(cs) {
                    let op = col(cs, 28, 32).to_ascii_uppercase();
                    // Columns 82, 84 and 86: 1, 2, 3 for the high, low
                    // and equal indicators used (observed).
                    let use_: String = [(54, '1'), (56, '2'), (58, '3')]
                        .iter()
                        .map(|(c, d)| {
                            if col(cs, *c, c + 1).is_empty() {
                                "  ".to_string()
                            } else {
                                format!("{d} ")
                            }
                        })
                        .collect();
                    let begins = op.starts_with("DO")
                        || (op.starts_with("IF") && op.len() == 4)
                        || op == "SELEC"
                        || op.starts_with("CAS");
                    let ends = matches!(op.as_str(), "END" | "ENDDO" | "ENDIF" | "ENDSL" | "ENDCS");
                    let do_ = if begins {
                        level += 1;
                        format!("B{level:03}")
                    } else if ends {
                        let s = format!("E{:03}", level.max(1));
                        level = level.saturating_sub(1);
                        s
                    } else if level > 0 {
                        format!(" {level:03}")
                    } else {
                        String::new()
                    };
                    if !use_.trim().is_empty() || !do_.is_empty() {
                        line = format!("{line:<81}{use_} {do_}");
                    }
                }
                out_src.push(line.trim_end().to_string());
            }
        }
    }
    out_src.push("           * * * * *   E N D   O F   S O U R C E   * * * * *".into());

    // ------------------------------------------------------- messages
    let mut msgs: Vec<(String, usize, String)> = Vec::new();
    for (k, f) in files.iter().enumerate() {
        // Blocked: an input-only file read with READ only.
        let _ = k;
        let decl = lines.iter().enumerate().find(|(_, cs)| {
            spec(cs) == 'F' && !comment(cs) && col(cs, 7, 14).eq_ignore_ascii_case(&f.file)
        });
        if let Some((i, cs)) = decl {
            let input_only = cs.get(14).is_some_and(|c| c.eq_ignore_ascii_case(&'I'));
            let random = lines.iter().any(|x| {
                spec(x) == 'C'
                    && !comment(x)
                    && matches!(
                        col(x, 28, 32).to_ascii_uppercase().as_str(),
                        "CHAIN"
                            | "SETLL"
                            | "SETGT"
                            | "READE"
                            | "READP"
                            | "REDPE"
                            | "UPDAT"
                            | "DELET"
                    )
                    && (col(x, 33, 42).eq_ignore_ascii_case(&f.file)
                        || col(x, 33, 42).eq_ignore_ascii_case(&f.format))
            });
            if input_only && !random {
                msgs.push((
                    "7086".into(),
                    i,
                    format!("RPG BLOCKS THE RECORDS OF FILE {}.", f.file),
                ));
            }
        }
    }
    let mut diag = vec![at(&[(
        11,
        "A d d i t i o n a l   D i a g n o s t i c   M e s s a g e s",
    )])];
    for (id, i, text) in &msgs {
        diag.push(format!("* {id}{:>10}   {text}", (i + 1) * 100));
    }

    // -------------------------------------------------- cross reference
    // References of fields, literals and indicators: name → [(ref, flag)].
    let mut refs: BTreeMap<String, Vec<(String, char)>> = BTreeMap::new();
    let mut inds: BTreeMap<String, Vec<(String, char)>> = BTreeMap::new();
    let mut attrs: BTreeMap<String, String> = BTreeMap::new();
    for f in files {
        for (j, (name, t, from, to, dec)) in f.fields.iter().enumerate() {
            let k = files.iter().position(|x| x.file == f.file).unwrap_or(0);
            let id = format!("{}{:06}", (b'A' + k as u8) as char, j + 1);
            refs.entry(name.clone()).or_default().push((id, 'D'));
            let a = match (t, dec) {
                ('P', Some(d)) => format!("P({},{d})", (to + 1 - from) * 2 - 1),
                ('S', Some(d)) | (' ', Some(d)) => format!("S({},{d})", to + 1 - from),
                ('B', Some(d)) => format!("B({},{d})", if to + 1 - from <= 2 { 4 } else { 9 }),
                _ => format!("A({})", to + 1 - from),
            };
            attrs.insert(name.clone(), a);
        }
    }
    let mut file_refs: Vec<FileRefs> = Vec::new();
    for (i, cs) in lines.iter().enumerate() {
        if comment(cs) {
            continue;
        }
        let r = ((i + 1) * 100).to_string();
        match spec(cs) {
            'F' => {
                let name = col(cs, 7, 14).to_ascii_uppercase();
                let dev = col(cs, 40, 46).to_ascii_uppercase();
                let fmt = files
                    .iter()
                    .find(|f| f.file == name)
                    .map(|f| f.format.clone())
                    .unwrap_or_default();
                let k = files.iter().position(|f| f.file == name).unwrap_or(0);
                let rec_refs = vec![
                    (r.clone(), 'D'),
                    (format!("{}000000", (b'A' + k as u8) as char), ' '),
                ];
                file_refs.push((name, dev, vec![(r, 'D')], vec![(fmt, rec_refs)]));
            }
            'C' => {
                let f1 = col(cs, 18, 27).to_ascii_uppercase();
                let op = col(cs, 28, 32).to_ascii_uppercase();
                let f2 = col(cs, 33, 42).to_ascii_uppercase();
                let res = col(cs, 43, 48).to_ascii_uppercase();
                let len = col(cs, 49, 51);
                let dec = col(cs, 52, 52);
                for x in [&f1, &f2] {
                    if x.is_empty() {
                        continue;
                    }
                    if let Some(i) = x.strip_prefix("*IN") {
                        refs.entry(x.clone()).or_default().push((r.clone(), ' '));
                        attrs.insert(x.clone(), "A(1)".into());
                        inds.entry("*IN".into()).or_default().push((r.clone(), ' '));
                        let _ = i;
                    } else if x.starts_with('\'') {
                        refs.entry(x.clone()).or_default().push((r.clone(), ' '));
                        attrs.insert(x.clone(), "LITERAL".into());
                    } else if file_refs
                        .iter()
                        .any(|(_, _, _, recs)| recs.iter().any(|(n, _)| n == x))
                    {
                        for (_, _, _, recs) in file_refs.iter_mut() {
                            for (n, rr) in recs.iter_mut() {
                                if n == x {
                                    rr.push((r.clone(), ' '));
                                }
                            }
                        }
                    } else if !x.starts_with('*')
                        && !x.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        refs.entry(x.clone()).or_default().push((r.clone(), ' '));
                    }
                }
                if !res.is_empty() {
                    let defines = !len.is_empty();
                    refs.entry(res.clone())
                        .or_default()
                        .push((r.clone(), if defines { 'D' } else { 'M' }));
                    if defines {
                        let d = if dec.is_empty() {
                            None
                        } else {
                            dec.parse::<u32>().ok()
                        };
                        let n: u32 = len.parse().unwrap_or(0);
                        attrs.insert(
                            res.clone(),
                            match d {
                                Some(d) => format!("P({n},{d})"),
                                None => format!("A({n})"),
                            },
                        );
                    }
                }
                let _ = op;
                for c in [54usize, 56, 58] {
                    let ind = col(cs, c, c + 1).to_ascii_uppercase();
                    if !ind.is_empty() {
                        inds.entry(ind).or_default().push((r.clone(), 'M'));
                    }
                }
                for c in [10usize, 13, 16] {
                    let ind = col(cs, c, c + 1).to_ascii_uppercase();
                    if !ind.is_empty() {
                        inds.entry(ind).or_default().push((r.clone(), ' '));
                    }
                }
            }
            _ => {}
        }
    }
    // *INxx in a factor names indicator xx as well.
    for k in refs.keys().cloned().collect::<Vec<_>>() {
        if let Some(i) = k.strip_prefix("*IN") {
            if i.len() == 2 {
                let r = refs[&k].clone();
                inds.entry(i.to_string()).or_default().extend(r);
            }
        }
    }
    let slots = |r: &[(String, char)]| -> String {
        r.iter()
            .map(|(n, f)| format!("{n:>8}{f}"))
            .collect::<String>()
    };
    let ebc = |s: &str| crate::rpg::data::e(s);
    let mut xref = vec![
        at(&[(26, "C r o s s   R e f e r e n c e")]),
        " File and Record References:".to_string(),
        "        FILE/RCD    DEV/RCD     REFERENCES (D=DEFINED)".into(),
    ];
    for (k, (name, dev, r, recs)) in file_refs.iter().enumerate() {
        xref.push(
            format!(
                "{:<31}{}",
                at(&[(5, &format!("{:02}", k + 1)), (9, name), (21, dev)]),
                slots(r)
            )
            .trim_end()
            .to_string(),
        );
        for (n, rr) in recs {
            xref.push(
                format!("{:<31}{}", at(&[(11, n)]), slots(rr))
                    .trim_end()
                    .to_string(),
            );
        }
    }
    xref.push(" Field References:".into());
    xref.push("        FIELD       ATTR    REFERENCES (M=MODIFIED D=DEFINED)".into());
    let mut names: Vec<&String> = refs.keys().collect();
    // Literals after the names.
    names.sort_by_cached_key(|n| (n.starts_with('\''), ebc(n)));
    let mut unused = 0usize;
    for n in names {
        let r = &refs[n];
        let prefix = if r.iter().all(|x| x.1 == 'D') {
            unused += 1;
            "* 7031"
        } else {
            ""
        };
        let a = attrs.get(n).cloned().unwrap_or_default();
        xref.push(
            format!("{:<27}{}", at(&[(1, prefix), (9, n), (21, &a)]), slots(r))
                .trim_end()
                .to_string(),
        );
    }
    xref.push(" Indicator References:".into());
    xref.push("        INDICATOR  REFERENCES (M=MODIFIED D=DEFINED)".into());
    let mut il: Vec<&String> = inds.keys().collect();
    il.sort_by_cached_key(|n| ebc(n));
    for i in il {
        let mut r = inds[i].clone();
        r.sort_by_key(|x| x.0.parse::<usize>().unwrap_or(0));
        r.dedup();
        xref.push(
            format!("{:<18}{}", at(&[(9, i)]), slots(&r))
                .trim_end()
                .to_string(),
        );
    }
    xref.push("      * * * * *   E N D   O F   C R O S S   R E F E R E N C E   * * * * *".into());

    // --------------------------------------------------- message summary
    let mut summary = vec![at(&[(26, "M e s s a g e   S u m m a r y")])];
    let mut all: Vec<(String, usize, String)> = Vec::new();
    if unused > 0 {
        all.push((
            "QRG7031".into(),
            unused,
            "A name or indicator is not used.".into(),
        ));
    }
    if !msgs.is_empty() {
        all.push((
            "QRG7086".into(),
            msgs.len(),
            "RPG blocks the records of the file; its INFDS changes only when a block is read or written.".into(),
        ));
    }
    for (id, n, text) in &all {
        summary.push(format!("* {id} Severity:  00   Number: {n:>4}"));
        let mut cur = "          Message . . . . :   ".to_string();
        for w in text.split(' ') {
            if cur.len() + w.len() + 1 > 76 {
                summary.push(cur.trim_end().to_string());
                cur = "            ".into();
            }
            cur.push_str(w);
            cur.push(' ');
        }
        summary.push(cur.trim_end().to_string());
    }
    summary
        .push("      * * * * *   E N D   O F   M E S S A G E   S U M M A R Y   * * * * *".into());

    // ------------------------------------------------------ final summary
    let total: usize = all.iter().map(|x| x.1).sum();
    let comments = lines.iter().filter(|cs| comment(cs)).count();
    let tsum = |t: &str, v: usize| {
        at(&[
            (5, &crate::commands::dsp_label(t, 26)),
            (37, &v.to_string()),
        ])
    };
    let mut fin = vec![
        at(&[(28, "F i n a l   S u m m a r y")]),
        " Message Count:  (by Severity Number)".to_string(),
        "            TOTAL    00     10     20     30     40     50".into(),
        format!(
            "{total:>15}{total:>8}{:>7}{:>7}{:>7}{:>7}{:>7}",
            0, 0, 0, 0, 0
        ),
        " Program Source Totals:".into(),
        tsum("Records", lines.len()),
        tsum("Specifications", lines.len() - comments),
        tsum("Table Records", 0),
        tsum("Comments", comments),
    ];
    fin.push(if created {
        format!(
            " Program {} was created in library {}; highest severity 00. Created on {} at {}.",
            o.program, o.library, stamp.date, stamp.time
        )
    } else {
        format!(
            " Program {} was not created in library {}.",
            o.program, o.library
        )
    });
    fin.push("          * * * * *   E N D   O F   C O M P I L A T I O N   * * * * *".into());

    let mut out = Vec::new();
    let mut page = 0;
    let mut src_and_diag = out_src;
    src_and_diag.extend(diag);
    for section in [pro, src_and_diag, xref, summary, fin] {
        for chunk in section.chunks(PAGE - 1) {
            page += 1;
            out.push(header(page));
            out.extend(chunk.iter().cloned());
        }
    }
    out
}
