//! DSPDBR: the files that depend on a database file (logical files, SQL
//! indexes and views built over it), printed or written to an output
//! file in QADSPDBR's format.
//!
//! Report layout and outfile values observed on IBM i 7.5 (PUB400
//! 2026-10-27, tools/golden/verify8.txt c8_dbr_print, c8_dbr_out,
//! c8_dbr_none, c8_dbr_missing, c8_dbr_vals1). The product line is
//! as4tainr's (report.rs).

use crate::authority::{self, Aut};
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

/// One dependent file: name, library, dependency ("D": data).
#[derive(Debug, Clone, PartialEq)]
pub struct Dependent {
    pub name: String,
    pub library: String,
    pub kind: &'static str,
}

/// What the report shows about the file.
#[derive(Debug, Clone)]
pub struct Input {
    pub name: String,
    pub library: String,
    pub physical: bool,
    pub output: String,
    pub dependents: Vec<Dependent>,
}

/// A label with its dots, the colon in column 48 (as captured), then the
/// keyword in column 50 and the value in column 61.
fn line(indent: usize, text: &str, keyword: &str, value: &str) -> String {
    let l = crate::commands::dsp_label(text, 46 - indent);
    format!("{}{l} {keyword:<10} {value}", " ".repeat(indent - 1))
        .trim_end()
        .to_string()
}

/// The printed report (c8_dbr_print).
pub fn report(input: &Input, stamp: &crate::report::Stamp) -> Vec<String> {
    let mut out = vec![
        crate::report::at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (39, "Display Data Base Relations"),
            (97, &stamp.date),
            (107, &stamp.time),
            (122, "Page    1"),
        ]),
        " DSPDBR Command Input".to_string(),
        line(4, "File", "FILE", &input.name),
        line(6, "Library", "", &input.library),
        line(4, "Member", "MBR", "*NONE"),
        line(4, "Record format", "RCDFMT", "*NONE"),
        line(4, "Output", "OUTPUT", &input.output),
        " Specifications".to_string(),
        line(
            4,
            "Type of file",
            "",
            if input.physical {
                "Physical"
            } else {
                "Logical"
            },
        ),
        line(4, "File", "", &input.name),
        line(6, "Library", "", &input.library),
        line(6, "Member", "", "*NONE"),
        line(6, "Record format", "", "*NONE"),
        // The count ends in column 65.
        format!(
            "{}{:>17}",
            line(6, "Number of dependent files", "", ""),
            input.dependents.len()
        ),
        " Files Dependent On Specified File".to_string(),
        "   Dependent File         Library       Dependency   JREF    Constraint".to_string(),
    ];
    for d in &input.dependents {
        let kind = match d.kind {
            "D" => "Data",
            k => k,
        };
        out.push(format!("{:7}{:<19}{:<14}{kind}", "", d.name, d.library));
    }
    out
}

/// The output file's records (QADSPDBR): one per dependent file, or one
/// with no dependent when there is none; WHDTM is the century digit,
/// date and time of the command (c8_dbr_vals1).
pub fn records(input: &Input, now: &str, system: &str) -> Vec<crate::outfile::Record> {
    let one = |d: Option<&Dependent>| -> crate::outfile::Record {
        vec![
            ("WHRTYP", if input.physical { "P" } else { "L" }.to_string()),
            ("WHRFI", input.name.clone()),
            ("WHRLI", input.library.clone()),
            ("WHRMB", "*NONE".into()),
            ("WHRRD", "*NONE".into()),
            ("WHNO", input.dependents.len().to_string()),
            ("WHDTM", now.to_string()),
            ("WHREFI", d.map(|d| d.name.clone()).unwrap_or_default()),
            ("WHRELI", d.map(|d| d.library.clone()).unwrap_or_default()),
            ("WHTYPE", d.map(|d| d.kind.to_string()).unwrap_or_default()),
            ("WHJDIL", "0".into()),
            ("WHJREF", "0".into()),
            ("WHSYSN", system.to_string()),
        ]
    };
    if input.dependents.is_empty() {
        vec![one(None)]
    } else {
        input.dependents.iter().map(|d| one(Some(d))).collect()
    }
}

impl Job<'_> {
    /// DSPDBR FILE OUTPUT(* | *PRINT | *OUTFILE) OUTFILE OUTMBR (CL
    /// reference): a file that is not there is CPF3012 (c8_dbr_missing);
    /// an output file gets CPF9861/CPF9862 when it is made and CPF3030
    /// each time (c8_dbr_out, c8_dbr_none).
    pub(crate) async fn dspdbr(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("FILE").to_ascii_uppercase();
        let (libs, name) = self.search(&q);
        let label = q
            .split_once('/')
            .map(|x| x.0)
            .unwrap_or("*LIBL")
            .to_string();
        let mut found = None;
        for l in &libs {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[l, &name],
                )
                .await?
            {
                found = Some((l.clone(), r.get::<_, String>(0)));
                break;
            }
        }
        let Some((lib, attr)) = found else {
            let shown = if label.starts_with('*') {
                self.current_library()
            } else {
                label
            };
            bail!("CPF3012: There is no file {name} in library {shown}.");
        };
        authority::check(self.client, &self.user, &lib, &name, "*FILE", Aut::Use).await?;
        let dependents = crate::catalog::dependent_files(self.client, &lib, &name)
            .await?
            .into_iter()
            .map(|(l, n)| Dependent {
                name: n,
                library: crate::qtemp::logical(&l),
                kind: "D",
            })
            .collect();
        let output = match cmd.get("OUTPUT").to_ascii_uppercase().as_str() {
            "" => "*".to_string(),
            o => o.to_string(),
        };
        let input = Input {
            name: name.clone(),
            library: crate::qtemp::logical(&lib),
            physical: !attr.starts_with("LF"),
            output: output.clone(),
            dependents,
        };
        if output == "*OUTFILE" {
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
            let recs = records(&input, &now, &system);
            let w =
                crate::outfile::write(self, &outfile, &option, &crate::outfile::QADSPDBR, &recs)
                    .await?;
            let mut messages = w.messages.clone();
            messages.push(crate::outfile::records_added(&w, recs.len()));
            return Ok(Outcome::Messages(messages));
        }
        let print = report(&input, &crate::report::Stamp::now());
        Ok(Outcome::Display(ListDisplay {
            title: "Display Data Base Relations".into(),
            rows: print[1..].to_vec(),
            print,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            name: "GLD6TB".into(),
            library: "AS4TRAINR1".into(),
            physical: true,
            output: "*PRINT".into(),
            dependents: vec![
                Dependent {
                    name: "GLD7IX".into(),
                    library: "AS4TRAINR1".into(),
                    kind: "D",
                },
                Dependent {
                    name: "GLD7VW".into(),
                    library: "AS4TRAINR1".into(),
                    kind: "D",
                },
            ],
        }
    }

    // Expected lines: PUB400 (IBM i 7.5) 2026-10-27, verify8 c8_dbr_print
    // (the product line is as4tainr's).
    #[test]
    fn report_layout() {
        let stamp = crate::report::Stamp {
            date: "26-10-27".into(),
            time: "20:28:22".into(),
            zone: String::new(),
        };
        let l = report(&input(), &stamp);
        assert_eq!(
            l[0],
            format!(
                " {}  {}               Display Data Base Relations                               26-10-27  20:28:22       Page    1",
                crate::report::PRODUCT,
                crate::report::LEVEL
            )
        );
        let want = [
            " DSPDBR Command Input",
            "   File  . . . . . . . . . . . . . . . . . . . : FILE       GLD6TB",
            "     Library . . . . . . . . . . . . . . . . . :            AS4TRAINR1",
            "   Member  . . . . . . . . . . . . . . . . . . : MBR        *NONE",
            "   Record format . . . . . . . . . . . . . . . : RCDFMT     *NONE",
            "   Output  . . . . . . . . . . . . . . . . . . : OUTPUT     *PRINT",
            " Specifications",
            "   Type of file  . . . . . . . . . . . . . . . :            Physical",
            "   File  . . . . . . . . . . . . . . . . . . . :            GLD6TB",
            "     Library . . . . . . . . . . . . . . . . . :            AS4TRAINR1",
            "     Member  . . . . . . . . . . . . . . . . . :            *NONE",
            "     Record format . . . . . . . . . . . . . . :            *NONE",
            "     Number of dependent files . . . . . . . . :                2",
            " Files Dependent On Specified File",
            "   Dependent File         Library       Dependency   JREF    Constraint",
            "       GLD7IX             AS4TRAINR1    Data",
            "       GLD7VW             AS4TRAINR1    Data",
        ];
        assert_eq!(&l[1..], want);
    }

    // c8_dbr_vals1: two dependents, then a file with none (one record).
    #[test]
    fn outfile_records() {
        let r = records(&input(), "1261027202822", "PUB400");
        assert_eq!(r.len(), 2);
        let get = |r: &crate::outfile::Record, k: &str| {
            r.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get(&r[0], "WHRTYP"), "P");
        assert_eq!(get(&r[0], "WHNO"), "2");
        assert_eq!(get(&r[1], "WHREFI"), "GLD7VW");
        assert_eq!(get(&r[1], "WHTYPE"), "D");
        assert_eq!(get(&r[1], "WHRMB"), "*NONE");
        let mut none = input();
        none.dependents.clear();
        let r = records(&none, "1261027202822", "PUB400");
        assert_eq!(r.len(), 1);
        assert_eq!(get(&r[0], "WHNO"), "0");
        assert_eq!(get(&r[0], "WHREFI"), "");
        assert_eq!(get(&r[0], "WHTYPE"), "");
    }
}
