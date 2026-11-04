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
    for (name, lib, ty) in refs {
        out.push(at(&[(4, &dsp_label("Object", 42)), (54, name)]));
        out.push(at(&[(6, &dsp_label("Library", 40)), (56, lib)]));
        out.push(at(&[(6, &dsp_label("Object type", 40)), (56, ty)]));
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
        if cmd.get("OUTPUT").eq_ignore_ascii_case("*OUTFILE") {
            bail!("DSPPGMREF OUTPUT(*OUTFILE) is not available here: use OUTPUT(*PRINT).");
        }
        let refs = match p.attribute.as_str() {
            "CLLE" => cl_references(&p.source, true)?,
            "CLP" => cl_references(&p.source, false)?,
            _ => rpg_references(&p.source),
        };
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
        let print = report(&p.name, &lib, &text, &refs, &crate::report::Stamp::now());
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
