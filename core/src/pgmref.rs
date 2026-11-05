//! DSPPGMREF (the objects a program refers to) and CHGPGM.
//!
//! A CL program's references, as IBM i 7.5 lists those of a program made
//! with CRTBNDCL (PUB400, tools/golden/verify44.txt m44_pgmref): the
//! programs it CALLs by name (library as written, *LIBL when it is not
//! qualified), in the order of the source, then the ILE CL run time's
//! service programs QLECWI, QCLSRV and QLEAWI in QSYS. A DCLF file is not
//! listed (it was not in the capture). An RPG program's references are
//! its files (F specifications, DCL-F) and the programs named by EXTPGM
//! or CALL, from its source: not captured from IBM i
//! (docs/fidelity-gaps.md). Messages: CHGPGM CPC0540, CPF9811 for a
//! program that is not there; DSPPGMREF CPF3033.

use crate::cl::Command;
use crate::commands::{dsp_label, Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

/// A referenced object: (name, library, type).
pub type Reference = (String, String, String);

/// The service programs every ILE CL program refers to (m44_pgmref).
const ILE_CL_RUNTIME: [&str; 3] = ["QLECWI", "QCLSRV", "QLEAWI"];

fn push(out: &mut Vec<Reference>, r: Reference) {
    if !out.contains(&r) {
        out.push(r);
    }
}

/// The objects a CL program's source refers to.
pub fn cl_references(source: &str, ile: bool) -> Result<Vec<Reference>> {
    fn walk(st: &crate::clpgm::Statement, out: &mut Vec<Reference>) -> Result<()> {
        let mut positional = 0;
        for p in &st.params {
            let kw = match &p.keyword {
                Some(k) => k.to_ascii_uppercase(),
                None => {
                    positional += 1;
                    if positional == 1 {
                        "PGM".to_string()
                    } else {
                        String::new()
                    }
                }
            };
            let value = p.value.trim();
            if matches!(kw.as_str(), "THEN" | "CMD" | "EXEC") && !value.is_empty() {
                let inner = crate::clpgm::parse_statement(st.line, value)?;
                walk(&inner, out)?;
            } else if st.name.eq_ignore_ascii_case("CALL") && kw == "PGM" {
                let v = value.trim_matches(|c| c == '(' || c == ')').trim();
                if v.is_empty() || v.starts_with('&') || v.starts_with('\'') {
                    continue;
                }
                let v = v.to_ascii_uppercase();
                let (lib, name) = match v.split_once('/') {
                    Some((l, n)) => (l.to_string(), n.to_string()),
                    None => ("*LIBL".to_string(), v.clone()),
                };
                push(out, (name, lib, "*PGM".into()));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for (line, text) in crate::clpgm::logical_lines(source)? {
        if let Ok(st) = crate::clpgm::parse_statement(line, &text) {
            walk(&st, &mut out)?;
        }
    }
    if ile {
        for s in ILE_CL_RUNTIME {
            push(&mut out, (s.into(), "QSYS".into(), "*SRVPGM".into()));
        }
    }
    Ok(out)
}

/// DSPPGMREF OUTPUT(*OUTFILE): a record per referenced object in
/// QADSPPGM's format (n45_ffdpgm: the program, its text, the number of
/// objects, the retrieval date and time as century, date and time, the
/// object, its library and type). The one-letter object type and the
/// file usage follow the model's field descriptions as as4tainr reads
/// them (F file, P program, S service program; usage 1 for a file read);
/// record format counts and level identifiers are not kept (zero and
/// blank). Values not captured from IBM i (docs/fidelity-gaps.md).
pub fn records(
    pgm: &str,
    lib: &str,
    text: &str,
    refs: &[Reference],
    files: &[FileUse],
    now: &str,
    system: &str,
) -> Vec<crate::outfile::Record> {
    let mut out = Vec::new();
    let mut fi = files.iter();
    for (n, l, t) in refs {
        // The one-letter type: F for a file, P for a program, blank for
        // a service program (o46_refg, o46_refcv).
        let letter = match t.as_str() {
            "*FILE" => "F",
            "*SRVPGM" => "",
            _ => "P",
        };
        let base = |r: &mut crate::outfile::Record| {
            r.extend([
                ("WHLIB", lib.to_string()),
                ("WHPNAM", pgm.to_string()),
                ("WHTEXT", text.to_string()),
                ("WHFNUM", refs.len().to_string()),
                ("WHDTTM", now.to_string()),
                ("WHFNAM", n.clone()),
                ("WHLNAM", l.clone()),
            ]);
        };
        let tail = |r: &mut crate::outfile::Record, formats: usize| {
            r.extend([
                ("WHOBJT", letter.to_string()),
                ("WHOTYP", t.clone()),
                ("WHSYSN", system.to_string()),
                ("WHSPKG", "P".into()),
                ("WHRFNB", formats.to_string()),
            ]);
        };
        let use_ = if t == "*FILE" { fi.next() } else { None };
        match use_ {
            // A file: a record per record format, with the file's name
            // in the program, its use and the format's level identifier
            // and field count (o46_refg record 3).
            Some(u) if !u.formats.is_empty() => {
                for (f, id, count) in &u.formats {
                    let mut r = Vec::new();
                    base(&mut r);
                    r.extend([
                        ("WHSNAM", u.in_program.clone()),
                        ("WHRFNO", u.formats.len().to_string()),
                        ("WHFUSG", u.usage.0.to_string()),
                        ("WHRFNM", f.clone()),
                        ("WHRFSN", id.clone()),
                        ("WHRFFN", count.to_string()),
                    ]);
                    tail(&mut r, u.formats.len());
                    out.push(r);
                }
            }
            _ => {
                let mut r = Vec::new();
                base(&mut r);
                r.extend([
                    ("WHSNAM", String::new()),
                    ("WHRFNO", "0".into()),
                    ("WHFUSG", "0".into()),
                    ("WHRFNM", String::new()),
                    ("WHRFSN", String::new()),
                    ("WHRFFN", "0".into()),
                ]);
                tail(&mut r, 0);
                out.push(r);
            }
        }
    }
    out
}

/// The ILE RPG run time's service programs an RPG program refers to,
/// after its own references (PUB400 2026-11-04, verify46 o46_pgmref and
/// o46_refg: a program with a file: QRNXIE, QRNXIO, QRNXUTIL, QLEAWI).
/// QRNXIO, the file support, is listed here only for a program with
/// files (one without was not captured).
fn rpg_runtime(files: bool) -> Vec<Reference> {
    let mut out = vec!["QRNXIE"];
    if files {
        out.push("QRNXIO");
    }
    out.extend(["QRNXUTIL", "QLEAWI"]);
    out.into_iter()
        .map(|n| (n.to_string(), "QSYS".to_string(), "*SRVPGM".to_string()))
        .collect()
}

/// An RPG program's references in the order IBM i lists them (o46_pgmref):
/// the called programs (GLD46C before QCMDEXC there: by name, as
/// as4tainr takes it), the files, then the run time's service programs.
pub fn rpg_ordered(source: &str) -> Vec<Reference> {
    let all = rpg_references(source);
    let mut pgms: Vec<Reference> = all.iter().filter(|r| r.2 == "*PGM").cloned().collect();
    pgms.sort_by(|a, b| a.0.cmp(&b.0));
    let files: Vec<Reference> = all.iter().filter(|r| r.2 == "*FILE").cloned().collect();
    let has_files = !files.is_empty();
    pgms.into_iter()
        .chain(files)
        .chain(rpg_runtime(has_files))
        .collect()
}

/// A file's use in an RPG program as DSPPGMREF gives it: (code, words).
/// Input is as captured (o46_pgmref "Input", o46_refg WHFUSG 01); the
/// others follow the model's numbering as as4tainr reads it (2 output,
/// 3 input and output, 4 update with input), not captured.
pub fn rpg_file_usage(source: &str, file: &str) -> (u8, &'static str) {
    let file = file.to_ascii_uppercase();
    for raw in source.lines() {
        let upper = raw.to_ascii_uppercase();
        let fixed: Vec<char> = upper.chars().collect();
        if fixed.len() > 16 && fixed[5] == 'F' && fixed[6] != '*' {
            let name: String = fixed[6..16].iter().collect::<String>().trim().to_string();
            if name == file {
                return match fixed.get(16) {
                    Some('O') => (2, "Output"),
                    Some('U') => (4, "Update"),
                    Some('C') => (3, "Input/Output"),
                    _ => (1, "Input"),
                };
            }
        }
        let free = upper.trim_start();
        if let Some(rest) = free.strip_prefix("DCL-F ") {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || "#$@_".contains(*c))
                .collect();
            if name == file {
                let usage = rest
                    .find("USAGE(")
                    .map(|i| rest[i..].split(')').next().unwrap_or("").to_string())
                    .unwrap_or_default();
                return if usage.contains("*UPDATE") {
                    (4, "Update")
                } else if usage.contains("*OUTPUT") && usage.contains("*INPUT") {
                    (3, "Input/Output")
                } else if usage.contains("*OUTPUT") {
                    (2, "Output")
                } else {
                    (1, "Input")
                };
            }
        }
    }
    (1, "Input")
}

/// A file a program uses: its name in the program, its use, and its
/// record formats (name, level identifier, field count).
#[derive(Clone, Debug, Default)]
pub struct FileUse {
    pub in_program: String,
    pub usage: (u8, &'static str),
    pub formats: Vec<(String, String, usize)>,
}

/// The objects an RPG program's source names: files and called programs.
pub fn rpg_references(source: &str) -> Vec<Reference> {
    let mut out = Vec::new();
    let quoted = |s: &str| -> Option<String> {
        let a = s.find('\'')?;
        let b = s[a + 1..].find('\'')?;
        Some(s[a + 1..a + 1 + b].trim().to_ascii_uppercase())
    };
    for raw in source.lines() {
        let line: String = raw.chars().collect();
        let upper = line.to_ascii_uppercase();
        let fixed: Vec<char> = upper.chars().collect();
        // Fixed form: F in column 6, the file name in columns 7-16.
        if fixed.len() > 16 && fixed[5] == 'F' && fixed[6] != '*' && fixed[6] != ' ' {
            let name: String = fixed[6..16].iter().collect::<String>().trim().to_string();
            if !name.is_empty() {
                push(&mut out, (name, "*LIBL".into(), "*FILE".into()));
            }
        }
        let free = upper.trim_start();
        if let Some(rest) = free.strip_prefix("DCL-F ") {
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || "#$@_".contains(*c))
                .collect();
            if !name.is_empty() {
                push(&mut out, (name, "*LIBL".into(), "*FILE".into()));
            }
        }
        if let Some(i) = upper.find("EXTPGM(") {
            if let Some(n) = quoted(&upper[i..]) {
                let (lib, name) = match n.split_once('/') {
                    Some((l, p)) => (l.to_string(), p.to_string()),
                    None => ("*LIBL".to_string(), n),
                };
                push(&mut out, (name, lib, "*PGM".into()));
            }
        }
    }
    out
}

/// DSPPGMREF OUTPUT(*PRINT) in IBM i 7.5's layout (PUB400, verify44
/// m44_pgmref): the command's input, the program, the count and each
/// object with its library and type. The product line is as4tainr's.
pub fn report(
    program: &str,
    library: &str,
    text: &str,
    refs: &[Reference],
    files: &[FileUse],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    use crate::report::at;
    let page = "Page    1";
    let count = refs.len().to_string();
    let mut out = vec![
        at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (39, "Display Program References"),
            (97, &stamp.date),
            (107, &stamp.time),
            (122, page),
        ]),
        at(&[(2, "DSPPGMREF Command Input")]),
        at(&[(4, &dsp_label("Program", 42)), (52, program)]),
        at(&[(6, &dsp_label("Library", 40)), (54, library)]),
        at(&[(4, &dsp_label("Output", 42)), (52, "*PRINT")]),
        at(&[(4, &dsp_label("Object types", 42)), (52, "*PGM")]),
        at(&[(2, &dsp_label("Program", 44)), (52, program)]),
        at(&[(4, &dsp_label("Library", 42)), (54, library)]),
        at(&[
            (4, "Text 'description'. . . . . . . . . . . . . :"),
            (52, text),
        ]),
        at(&[
            (4, &dsp_label("Number of objects referenced", 42)),
            (59 - count.len(), &count),
        ]),
    ];
    let mut fi = files.iter();
    for (name, lib, ty) in refs {
        out.push(at(&[(4, &dsp_label("Object", 42)), (54, name)]));
        out.push(at(&[(6, &dsp_label("Library", 40)), (56, lib)]));
        out.push(at(&[(6, &dsp_label("Object type", 40)), (56, ty)]));
        // A file's name in the program, use and record formats
        // (o46_pgmref).
        if ty == "*FILE" {
            if let Some(u) = fi.next() {
                if !u.formats.is_empty() {
                    let n = u.formats.len().to_string();
                    out.push(at(&[
                        (6, &dsp_label("File name in program", 40)),
                        (56, &u.in_program),
                    ]));
                    out.push(at(&[(6, &dsp_label("File usage", 40)), (56, u.usage.1)]));
                    out.push(at(&[
                        (6, &dsp_label("Number of record formats", 40)),
                        (61 - n.len(), &n),
                    ]));
                    out.push(at(&[
                        (8, "Record Format"),
                        (27, "Format Level Identifier"),
                        (55, "Field Count"),
                    ]));
                    for (f, id, count) in &u.formats {
                        let c = count.to_string();
                        out.push(at(&[(10, f), (32, id), (63 - c.len(), &c)]));
                    }
                }
            }
        }
    }
    out
}

impl Job<'_> {
    pub(crate) async fn pgmref_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "DSPPGMREF" => self.dsppgmref(cmd).await?,
            "CHGPGM" => self.chgpgm(cmd).await?,
            _ => return Ok(None),
        }))
    }

    /// The program PGM names, or the message for one that is not there.
    async fn named_program(
        &self,
        spec: &str,
        missing: impl Fn(&str, &str) -> String,
    ) -> Result<crate::catalog::Program> {
        let spec = spec.trim().to_ascii_uppercase();
        let (libs, name) = self.search(&spec);
        let shown = spec
            .split_once('/')
            .map(|x| x.0.to_string())
            .unwrap_or_else(|| "*LIBL".into());
        match crate::catalog::find_program(self.client, &libs, &name).await? {
            Some(p) => Ok(p),
            None => bail!(missing(&name, &shown)),
        }
    }

    async fn dsppgmref(&mut self, cmd: &Command) -> Result<Outcome> {
        let p = self
            .named_program(cmd.get("PGM"), |n, l| {
                format!("CPF3033: There is no program {n} in library {l}.")
            })
            .await?;
        let refs = match p.attribute.as_str() {
            "CLLE" => cl_references(&p.source, true)?,
            "CLP" => cl_references(&p.source, false)?,
            _ => rpg_ordered(&p.source),
        };
        // An RPG program's files: the library the file is found in, its
        // record format (o46_pgmref: AS4TRAINR1, not *LIBL).
        let mut refs = refs;
        let mut files = Vec::new();
        if !matches!(p.attribute.as_str(), "CLLE" | "CLP") {
            for r in refs.iter_mut().filter(|r| r.2 == "*FILE") {
                let mut u = FileUse {
                    in_program: r.0.clone(),
                    usage: rpg_file_usage(&p.source, &r.0),
                    formats: vec![],
                };
                if let Ok(f) =
                    crate::catalog::resolve_file(self.client, &self.library_list(), &r.0).await
                {
                    r.1 = crate::qtemp::logical(&f.library);
                    u.formats.push((
                        f.file.format.clone(),
                        crate::objdesc::level_id(&f.file.format, &f.file.fields),
                        f.file.fields.len(),
                    ));
                }
                files.push(u);
            }
        }
        let lib = crate::qtemp::logical(&p.library);
        let text: String = self
            .client
            .query_opt(
                "SELECT text FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
                &[&p.library, &p.name],
            )
            .await?
            .map(|r| r.get(0))
            .unwrap_or_default();
        if cmd.get("OUTPUT").eq_ignore_ascii_case("*OUTFILE") {
            let outfile = cmd.get("OUTFILE").trim().to_string();
            if outfile.is_empty() {
                bail!(
                    "CPD9861: OUTPUT(*OUTFILE) was given without OUTFILE, the file to receive the records.\n{}",
                    crate::cl::CPF0006
                );
            }
            let option = cmd
                .list("OUTMBR")
                .get(1)
                .cloned()
                .unwrap_or_else(|| "*REPLACE".into());
            let now: String = self
                .client
                .query_one(
                    "SELECT CASE WHEN t < '2000-01-01' THEN '0' ELSE '1' END || to_char(t, 'YYMMDDHH24MISS')
                       FROM (SELECT now() AT TIME ZONE 'UTC' AS t) x",
                    &[],
                )
                .await?
                .get(0);
            let system: String = crate::report::system_name().chars().take(8).collect();
            let recs = records(&p.name, &lib, &text, &refs, &files, &now, &system);
            let w =
                crate::outfile::write(self, &outfile, &option, &crate::outfile::QADSPPGM, &recs)
                    .await?;
            let mut messages = w.messages.clone();
            messages.push(crate::outfile::records_added(&w, recs.len()));
            return Ok(Outcome::Messages(messages));
        }
        let print = report(
            &p.name,
            &lib,
            &text,
            &refs,
            &files,
            &crate::report::Stamp::now(),
        );
        let fmt = |n: &str, l: &str, t: &str| format!("{n:<10}  {l:<10}  {t}");
        Ok(Outcome::Display(ListDisplay {
            title: "Display Program References".into(),
            info: vec![
                ("Program".into(), p.name.clone()),
                ("Library".into(), lib),
                (
                    "Number of objects referenced".into(),
                    refs.len().to_string(),
                ),
            ],
            heading: fmt("Object", "Library", "Type"),
            rows: refs.iter().map(|(n, l, t)| fmt(n, l, t)).collect(),
            print,
            ..Default::default()
        }))
    }

    /// CHGPGM: TEXT is kept; the other attributes (optimization, user
    /// profile, performance collection...) are accepted and not used.
    async fn chgpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let p = self
            .named_program(cmd.get("PGM"), |n, l| {
                format!("CPF9811: There is no program {n} in library {l}.")
            })
            .await?;
        crate::authority::check(
            self.client,
            &self.user,
            &p.library,
            &p.name,
            "*PGM",
            crate::authority::Aut::Change,
        )
        .await?;
        let text = cmd.get("TEXT");
        if text != "*SAME" {
            let text = if text == "*BLANK" { "" } else { text };
            self.client
                .execute(
                    "UPDATE as4tainr.objects SET text=$3 WHERE library=$1 AND name=$2 AND objtype='*PGM'",
                    &[&p.library, &p.name, &text],
                )
                .await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "CPC0540: Program {} in {} was changed.",
            p.name,
            crate::qtemp::logical(&p.library)
        )]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_of_a_cl_program() {
        // PUB400 (IBM i 7.5), verify44 m44_pgmref: GLD44C (DCLF of
        // QSYS/QADSPOBJ, CALL PGM(QSYS/QCMDEXC)) refers to QCMDEXC, then
        // QLECWI, QCLSRV and QLEAWI; the DCLF file is not listed.
        let src = "/* verify44 */\n PGM\n DCLF FILE(QSYS/QADSPOBJ)\n DCL VAR(&A) TYPE(*CHAR) LEN(10) VALUE('x')\n CALL PGM(QSYS/QCMDEXC) PARM('DLYJOB DLY(1)' 13)\n ENDPGM\n";
        let r = cl_references(src, true).unwrap();
        let names: Vec<(&str, &str, &str)> = r
            .iter()
            .map(|(n, l, t)| (n.as_str(), l.as_str(), t.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("QCMDEXC", "QSYS", "*PGM"),
                ("QLECWI", "QSYS", "*SRVPGM"),
                ("QCLSRV", "QSYS", "*SRVPGM"),
                ("QLEAWI", "QSYS", "*SRVPGM"),
            ]
        );
    }

    #[test]
    fn dsppgmref_report_as_ibm_i_prints_it() {
        // PUB400 (IBM i 7.5), verify44 m44_pgmref, below the header line (two of its four
        // objects, so the count is 2).
        let refs = vec![
            (
                "QCMDEXC".to_string(),
                "QSYS".to_string(),
                "*PGM".to_string(),
            ),
            (
                "QLECWI".to_string(),
                "QSYS".to_string(),
                "*SRVPGM".to_string(),
            ),
        ];
        let r = report(
            "GLD44C",
            "AS4TRAINR1",
            "",
            &refs,
            &[],
            &crate::report::Stamp::default(),
        );
        let want = [
            " DSPPGMREF Command Input",
            "   Program . . . . . . . . . . . . . . . . . . :   GLD44C",
            "     Library . . . . . . . . . . . . . . . . . :     AS4TRAINR1",
            "   Output  . . . . . . . . . . . . . . . . . . :   *PRINT",
            "   Object types  . . . . . . . . . . . . . . . :   *PGM",
            " Program . . . . . . . . . . . . . . . . . . . :   GLD44C",
            "   Library . . . . . . . . . . . . . . . . . . :     AS4TRAINR1",
            "   Text 'description'. . . . . . . . . . . . . :",
            "   Number of objects referenced  . . . . . . . :         2",
            "   Object  . . . . . . . . . . . . . . . . . . :     QCMDEXC",
            "     Library . . . . . . . . . . . . . . . . . :       QSYS",
            "     Object type . . . . . . . . . . . . . . . :       *PGM",
            "   Object  . . . . . . . . . . . . . . . . . . :     QLECWI",
            "     Library . . . . . . . . . . . . . . . . . :       QSYS",
            "     Object type . . . . . . . . . . . . . . . :       *SRVPGM",
        ];
        assert_eq!(r[1..], want);
    }
}
