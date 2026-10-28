//! Job descriptions (*JOBD) and workstation customization objects
//! (*WSCST); CHGCMD.
//!
//! Written from IBM's CL reference (CRTJOBD, CHGJOBD, DLTJOBD, DSPJOBD,
//! CRTWSCST, DLTWSCST, CHGCMD, SBMJOB JOBD). A job description keeps
//! what as4tainr's batch jobs use from it: the job queue (QBATCH is the
//! only one), the initial library list (INLLIBL), the user (USER) and the
//! request data (RQSDTA); its other attributes are kept as text and shown
//! by DSPJOBD. SBMJOB JOBD(lib/name) names the job after it (JOB(*JOBD))
//! and starts it with its library list (INLLIBL(*JOBD)). A workstation
//! customization object is an object only (its source is checked to be
//! there; 5250 sessions do not use it).

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.job_descriptions (
    library text NOT NULL,
    name    text NOT NULL,
    jobq    text NOT NULL DEFAULT 'QGPL/QBATCH',
    inllibl text NOT NULL DEFAULT '*SYSVAL',
    usr     text NOT NULL DEFAULT '*RQD',
    rqsdta  text NOT NULL DEFAULT '*NONE',
    other   text NOT NULL DEFAULT '',
    PRIMARY KEY (library, name)
);
-- QGPL/QDFTJOBD, the job description IBM i ships: its values that are
-- not CRTJOBD's defaults as DSPJOBD shows them on IBM i 7.5 (PUB400
-- 2026-10-28, tools/golden/verify12.txt n12_dspjobd).
INSERT INTO as4tainr.job_descriptions (library, name, other)
VALUES ('QGPL', 'QDFTJOBD', 'RTGDTA(QCMDI)' || E'\n' || 'INQMSGRPY(*SYSRPYL)' || E'\n'
                            || 'LOG(4 20 *SECLVL)' || E'\n' || 'LOGCLPGM(*YES)')
ON CONFLICT (library, name) DO UPDATE SET other = EXCLUDED.other
 WHERE job_descriptions.other = '';
INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
SELECT 'QGPL', 'QDFTJOBD', '*JOBD', '', 'Default job description'
 WHERE NOT EXISTS (SELECT 1 FROM as4tainr.objects
                    WHERE library = 'QGPL' AND name = 'QDFTJOBD' AND objtype = '*JOBD');
"#;

/// A job description as SBMJOB uses it.
#[derive(Debug, Clone, Default)]
pub struct JobDescription {
    pub library: String,
    pub name: String,
    pub jobq: String,
    /// The initial library list: None for *SYSVAL (the system's user
    /// part: QGPL QTEMP).
    pub inllibl: Option<Vec<String>>,
    pub user: String,
    pub rqsdta: String,
}

/// A job description through a library list.
pub async fn find(client: &Client, libs: &[String], name: &str) -> Result<Option<JobDescription>> {
    for l in libs {
        if let Some(r) = client
            .query_opt(
                "SELECT library, name, jobq, inllibl, usr, rqsdta FROM as4tainr.job_descriptions
                  WHERE library=$1 AND name=$2",
                &[l, &name.to_ascii_uppercase()],
            )
            .await?
        {
            let inl: String = r.get(3);
            return Ok(Some(JobDescription {
                library: r.get(0),
                name: r.get(1),
                jobq: r.get(2),
                inllibl: (inl != "*SYSVAL")
                    .then(|| inl.split_whitespace().map(str::to_string).collect()),
                user: r.get(4),
                rqsdta: r.get(5),
            }));
        }
    }
    Ok(None)
}

/// A job description's attributes in DSPJOBD's report, IBM i 7.5's
/// layout (PUB400 2026-10-28, tools/golden/verify12.txt n12_dspjobd:
/// labels from column 2 with leaders to column 49, the colon at 51, the
/// value at 55; the initial library list in three columns). Values not
/// given when it was made are CRTJOBD's defaults (IBM i CL reference,
/// CRTJOBD). The initial library list's sequence numbers were not
/// captured: 10, 20... as DSPLIBL numbers.
pub fn report_lines(d: &JobDescription, other: &str, text: &str) -> Vec<String> {
    use crate::report::at;
    let given = |k: &str| -> Option<String> {
        other.lines().find_map(|l| {
            l.strip_prefix(k)
                .and_then(|r| r.strip_prefix('('))
                .and_then(|r| r.strip_suffix(')'))
                .map(|v| v.trim().to_string())
        })
    };
    let val = |k: &str, dft: &str| given(k).unwrap_or_else(|| dft.to_string());
    let line = |label: &str, v: &str| {
        format!("{}   {v}", crate::commands::dsp_label(label, 48))
            .trim_end()
            .to_string()
    };
    let sub = |label: &str, v: &str| {
        format!("{}     {v}", crate::commands::dsp_label(label, 48))
            .trim_end()
            .to_string()
    };
    let split = |q: &str| -> (String, String) {
        match q.split_once('/') {
            Some((l, n)) => (n.to_string(), l.to_string()),
            None => (q.to_string(), String::new()),
        }
    };
    let (jobq, jobq_lib) = split(&d.jobq);
    let jobq_lib = if jobq_lib.is_empty() {
        "*LIBL".to_string()
    } else {
        jobq_lib
    };
    let (outq, outq_lib) = split(&val("OUTQ", "*USRPRF"));
    let log = val("LOG", "4 0 *NOLIST");
    let mut log = log.split_whitespace().map(str::to_string);
    let (level, sev, logtext) = (
        log.next().unwrap_or_else(|| "4".into()),
        log.next().unwrap_or_else(|| "0".into()),
        log.next().unwrap_or_else(|| "*NOLIST".into()),
    );
    let mut out = vec![
        at(&[
            (2, "Job description:"),
            (21, &d.name),
            (36, "Library:"),
            (47, &d.library),
        ]),
        line(" User profile", &d.user),
        line(" CL syntax check", &val("SYNTAX", "*NOCHK")),
        line(" Hold on job queue", &val("HOLD", "*NO")),
        line(" End severity", &val("ENDSEV", "30")),
        line(" Job date", &val("DATE", "*SYSVAL")),
        line(" Job switches", &val("SWS", "00000000")),
        line(" Inquiry message reply", &val("INQMSGRPY", "*RQD")),
        line(" Job priority (on job queue)", &val("JOBPTY", "5")),
        line(" Job queue", &jobq),
        sub("   Library", &jobq_lib),
        line(" Output priority (on output queue)", &val("OUTPTY", "5")),
        line(" Printer device", &val("PRTDEV", "*USRPRF")),
        line(" Output queue", &outq),
        sub("   Library", &outq_lib),
        " Message logging:".to_string(),
        line("   Level", &level),
        line("   Severity", &sev),
        line("   Text", &logtext),
        line(" Log CL program commands", &val("LOGCLPGM", "*NO")),
        line(" Job log output", &val("LOGOUTPUT", "*SYSVAL")),
        line(" Accounting code", &val("ACGCDE", "*USRPRF")),
        line(" Print text", &val("PRTTXT", "*SYSVAL")),
        line(" Routing data", &val("RTGDTA", "QCMDB")),
        line(" Request data", &d.rqsdta),
        line(" DDM conversation", &val("DDMCNV", "*KEEP")),
        line(" Device recovery action", "*SYSVAL"),
        line(" Time slice end pool", "*SYSVAL"),
        line(
            " Job message queue maximum size",
            &val("JOBMSGQMX", "*SYSVAL"),
        ),
        line(
            " Job message queue full action",
            &val("JOBMSGQFL", "*SYSVAL"),
        ),
        line(" Allow multiple threads", &val("ALWMLTTHD", "*NO")),
        line(" Initial ASP group", &val("INLASPGRP", "*NONE")),
        line(" Spooled file action", &val("SPLFACN", "*SYSVAL")),
        line(" Workload group", "*SBSD"),
        line(" Text", text),
        at(&[(56, "Initial library list:")]),
        at(&[(2, "Sequence"), (46, "Sequence"), (90, "Sequence")]),
        at(&[
            (3, "number"),
            (12, "Library"),
            (47, "number"),
            (56, "Library"),
            (91, "number"),
            (100, "Library"),
        ]),
    ];
    match &d.inllibl {
        None => out.push(at(&[(12, "*SYSVAL")])),
        Some(libs) => {
            for row in libs.chunks(3).enumerate() {
                let (r, chunk) = row;
                let mut f: Vec<(usize, String)> = Vec::new();
                for (c, l) in chunk.iter().enumerate() {
                    let n = (r * 3 + c + 1) * 10;
                    let base = 1 + c * 44;
                    f.push((base + 7 - n.to_string().len(), n.to_string()));
                    f.push((base + 11, l.clone()));
                }
                let refs: Vec<(usize, &str)> = f.iter().map(|(c, t)| (*c, t.as_str())).collect();
                out.push(at(&refs));
            }
        }
    }
    out
}

/// The values of a list parameter as one blank-separated text.
fn joined(cmd: &Command, k: &str) -> String {
    cmd.list(k)
        .iter()
        .flat_map(|v| {
            v.split_whitespace()
                .map(|w| {
                    w.trim_matches(|c| c == '(' || c == ')')
                        .to_ascii_uppercase()
                })
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Attributes kept only to be shown: (keyword, value).
const OTHER: &[&str] = &[
    "JOBPTY",
    "OUTPTY",
    "PRTDEV",
    "OUTQ",
    "PRTTXT",
    "ACGCDE",
    "RTGDTA",
    "SYNTAX",
    "INQMSGRPY",
    "HOLD",
    "DATE",
    "SWS",
    "JOBMSGQMX",
    "JOBMSGQFL",
    "LOG",
    "LOGCLPGM",
    "LOGOUTPUT",
    "ENDSEV",
    "ALWMLTTHD",
    "SPLFACN",
    "DDMCNV",
    "INLASPGRP",
];

impl Job<'_> {
    pub(crate) async fn jobd_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "CRTJOBD" | "CHGJOBD" => self.crtjobd(cmd).await?,
            "DLTJOBD" | "DLTWSCST" => {
                let (kw, ty) = if cmd.def.name == "DLTJOBD" {
                    ("JOBD", "*JOBD")
                } else {
                    ("WSCST", "*WSCST")
                };
                let (libs, name) = self.search(cmd.get(kw));
                let name = name.to_ascii_uppercase();
                for l in &libs {
                    if catalog::object_exists(self.client, std::slice::from_ref(l), &name, ty)
                        .await?
                    {
                        if authority::effective(self.client, &self.user, l, &name, ty).await?
                            < Aut::All
                        {
                            bail!("CPF2189: Not authorized to delete {name} in {l} type {ty}.");
                        }
                        for sql in [
                            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                            "DELETE FROM as4tainr.object_auths WHERE library=$1 AND name=$2 AND objtype=$3",
                        ] {
                            self.client.execute(sql, &[l, &name, &ty]).await?;
                        }
                        self.client
                            .execute(
                                "DELETE FROM as4tainr.job_descriptions WHERE library=$1 AND name=$2",
                                &[l, &name],
                            )
                            .await?;
                        return Ok(Some(Outcome::Messages(vec![format!(
                            "CPC2191: Object {name} of type {ty} in library {l} was deleted."
                        )])));
                    }
                }
                let label = cmd.get(kw).split_once('/').map(|x| x.0).unwrap_or("*LIBL");
                bail!("CPF2105: Object {name} of type {ty} in library {label} not found.")
            }
            "DSPJOBD" => {
                let (libs, name) = self.search(cmd.get("JOBD"));
                // A job description that is not there: CPF1623 alone
                // (PUB400 2026-10-28, tools/golden/verify11.txt k_dspjobd).
                let Some(d) = find(self.client, &libs, &name).await? else {
                    bail!(
                        "CPF1623: Job description {} cannot be shown.",
                        name.to_ascii_uppercase()
                    );
                };
                let other: String = self
                    .client
                    .query_one(
                        "SELECT other FROM as4tainr.job_descriptions WHERE library=$1 AND name=$2",
                        &[&d.library, &d.name],
                    )
                    .await?
                    .get(0);
                let text: String = self
                    .client
                    .query_opt(
                        "SELECT text FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*JOBD'",
                        &[&d.library, &d.name],
                    )
                    .await?
                    .map(|r| r.get(0))
                    .unwrap_or_default();
                let rows = report_lines(&d, &other, &text);
                let [h1, h2] = crate::report::header_b(
                    45,
                    "Job Description Information",
                    &crate::report::system_name(),
                    &crate::report::Stamp::now(),
                    1,
                );
                let mut print = vec![h1, h2];
                print.extend(rows.iter().cloned());
                print.push(crate::report::END_SYSVAL.into());
                Outcome::Display(ListDisplay {
                    title: "Display Job Description".into(),
                    print,
                    rows,
                    ..Default::default()
                })
            }
            "CRTWSCST" => {
                let (lib, name) = self.create_target(cmd.get("WSCST"));
                let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
                let m = self.source_member_of(cmd, &name).await?;
                if catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*WSCST")
                    .await?
                {
                    bail!("Object {name} in {lib} type *WSCST already exists.");
                }
                self.client
                    .execute(
                        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*WSCST','',$3)",
                        &[&lib, &name, &m],
                    )
                    .await?;
                authority::set_owner(self.client, &lib, &name, "*WSCST", &self.user).await?;
                Outcome::Messages(vec![format!(
                    "Workstation customization object {name} created in library {lib}."
                )])
            }
            _ => return Ok(None),
        }))
    }

    /// The source member a create command names exists: its text.
    async fn source_member_of(&self, cmd: &Command, name: &str) -> Result<String> {
        let (libs, file) = self.search(cmd.get("SRCFILE"));
        let mbr = match cmd.get("SRCMBR").to_ascii_uppercase().as_str() {
            "" | "*WSCST" | "*OBJ" => name.to_string(),
            m => m.to_string(),
        };
        let m = crate::source::find_member(self.client, &libs, &file.to_ascii_uppercase(), &mbr)
            .await?;
        Ok(match cmd.get("TEXT") {
            "*SRCMBRTXT" | "" => m.text,
            "*BLANK" => String::new(),
            t => t.to_string(),
        })
    }

    /// CRTJOBD and CHGJOBD (*SAME keeps a value).
    async fn crtjobd(&mut self, cmd: &Command) -> Result<Outcome> {
        let change = cmd.def.name == "CHGJOBD";
        let (lib, name) = if change {
            let (libs, name) = self.search(cmd.get("JOBD"));
            match find(self.client, &libs, &name).await? {
                Some(d) => (d.library, d.name),
                None => bail!(
                    "CPF9801: Object {} is not in library {}.",
                    name.to_ascii_uppercase(),
                    cmd.get("JOBD")
                        .split_once('/')
                        .map(|x| x.0)
                        .unwrap_or("*LIBL")
                ),
            }
        } else {
            let (lib, name) = self.create_target(cmd.get("JOBD"));
            (
                lib.to_ascii_uppercase(),
                catalog::validate_name("object", &name)?,
            )
        };
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        let get = |k: &str| -> Option<String> {
            let v = cmd.get(k);
            (!v.is_empty() && !v.eq_ignore_ascii_case("*SAME")).then(|| v.to_string())
        };
        let inllibl = match joined(cmd, "INLLIBL") {
            s if s.is_empty() || s == "*SAME" => None,
            s if s == "*SYSVAL" => Some("*SYSVAL".to_string()),
            s => {
                for l in s.split_whitespace() {
                    if !catalog::library_exists(self.client, l).await? {
                        bail!("CPF2110: Library {l} does not exist.");
                    }
                }
                Some(s)
            }
        };
        let other: String = OTHER
            .iter()
            .filter_map(|k| get(k).map(|v| format!("{k}({v})")))
            .collect::<Vec<_>>()
            .join("\n");
        if change {
            self.client
                .execute(
                    "UPDATE as4tainr.job_descriptions SET
                        jobq = coalesce($3, jobq), inllibl = coalesce($4, inllibl),
                        usr = coalesce($5, usr), rqsdta = coalesce($6, rqsdta),
                        other = CASE WHEN $7 = '' THEN other ELSE $7 END
                      WHERE library=$1 AND name=$2",
                    &[
                        &lib,
                        &name,
                        &get("JOBQ").map(|v| v.to_ascii_uppercase()),
                        &inllibl,
                        &get("USER").map(|v| v.to_ascii_uppercase()),
                        &get("RQSDTA"),
                        &other,
                    ],
                )
                .await?;
            if let Some(t) = get("TEXT") {
                self.client
                    .execute(
                        "UPDATE as4tainr.objects SET text=$3 WHERE library=$1 AND name=$2 AND objtype='*JOBD'",
                        &[&lib, &name, &if t == "*BLANK" { String::new() } else { t }],
                    )
                    .await?;
            }
            return Ok(Outcome::Messages(vec![format!(
                "Job description {name} in library {lib} changed."
            )]));
        }
        if catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*JOBD").await? {
            bail!("Job description {name} in library {lib} already exists.");
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" | "" => String::new(),
            t => t.to_string(),
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*JOBD','',$3)",
                &[&lib, &name, &text],
            )
            .await?;
        self.client
            .execute(
                "INSERT INTO as4tainr.job_descriptions (library, name, jobq, inllibl, usr, rqsdta, other)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &lib,
                    &name,
                    &get("JOBQ").unwrap_or_else(|| "QGPL/QBATCH".into()).to_ascii_uppercase(),
                    &inllibl.unwrap_or_else(|| "*SYSVAL".into()),
                    &get("USER").unwrap_or_else(|| "*RQD".into()).to_ascii_uppercase(),
                    &get("RQSDTA").unwrap_or_else(|| "*NONE".into()),
                    &other,
                ],
            )
            .await?;
        authority::set_owner(self.client, &lib, &name, "*JOBD", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Job description {name} created in library {lib}."
        )]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PUB400 2026-10-28, tools/golden/verify12.txt n12_dspjobd: DSPJOBD
    // QGPL/QDFTJOBD OUTPUT(*PRINT), the lines after the page header.
    #[test]
    fn qdftjobd_report() {
        let d = JobDescription {
            library: "QGPL".into(),
            name: "QDFTJOBD".into(),
            jobq: "QGPL/QBATCH".into(),
            inllibl: None,
            user: "*RQD".into(),
            rqsdta: "*NONE".into(),
        };
        let other = "RTGDTA(QCMDI)\nINQMSGRPY(*SYSRPYL)\nLOG(4 20 *SECLVL)\nLOGCLPGM(*YES)";
        let got = report_lines(&d, other, "Default job description");
        let want = [
            " Job description:   QDFTJOBD       Library:   QGPL",
            " User profile . . . . . . . . . . . . . . . . . . :   *RQD",
            " CL syntax check  . . . . . . . . . . . . . . . . :   *NOCHK",
            " Hold on job queue  . . . . . . . . . . . . . . . :   *NO",
            " End severity . . . . . . . . . . . . . . . . . . :   30",
            " Job date . . . . . . . . . . . . . . . . . . . . :   *SYSVAL",
            " Job switches . . . . . . . . . . . . . . . . . . :   00000000",
            " Inquiry message reply  . . . . . . . . . . . . . :   *SYSRPYL",
            " Job priority (on job queue)  . . . . . . . . . . :   5",
            " Job queue  . . . . . . . . . . . . . . . . . . . :   QBATCH",
            "   Library  . . . . . . . . . . . . . . . . . . . :     QGPL",
            " Output priority (on output queue)  . . . . . . . :   5",
            " Printer device . . . . . . . . . . . . . . . . . :   *USRPRF",
            " Output queue . . . . . . . . . . . . . . . . . . :   *USRPRF",
            "   Library  . . . . . . . . . . . . . . . . . . . :",
            " Message logging:",
            "   Level  . . . . . . . . . . . . . . . . . . . . :   4",
            "   Severity . . . . . . . . . . . . . . . . . . . :   20",
            "   Text . . . . . . . . . . . . . . . . . . . . . :   *SECLVL",
            " Log CL program commands  . . . . . . . . . . . . :   *YES",
            " Job log output . . . . . . . . . . . . . . . . . :   *SYSVAL",
            " Accounting code  . . . . . . . . . . . . . . . . :   *USRPRF",
            " Print text . . . . . . . . . . . . . . . . . . . :   *SYSVAL",
            " Routing data . . . . . . . . . . . . . . . . . . :   QCMDI",
            " Request data . . . . . . . . . . . . . . . . . . :   *NONE",
            " DDM conversation . . . . . . . . . . . . . . . . :   *KEEP",
            " Device recovery action . . . . . . . . . . . . . :   *SYSVAL",
            " Time slice end pool  . . . . . . . . . . . . . . :   *SYSVAL",
            " Job message queue maximum size . . . . . . . . . :   *SYSVAL",
            " Job message queue full action  . . . . . . . . . :   *SYSVAL",
            " Allow multiple threads . . . . . . . . . . . . . :   *NO",
            " Initial ASP group  . . . . . . . . . . . . . . . :   *NONE",
            " Spooled file action  . . . . . . . . . . . . . . :   *SYSVAL",
            " Workload group . . . . . . . . . . . . . . . . . :   *SBSD",
            " Text . . . . . . . . . . . . . . . . . . . . . . :   Default job description",
            "                                                       Initial library list:",
            " Sequence                                    Sequence                                    Sequence",
            "  number   Library                            number   Library                            number   Library",
            "           *SYSVAL",
        ];
        assert_eq!(got, want);
    }
}
