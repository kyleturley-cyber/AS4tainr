//! Running CL commands for a job. Shared by the 5250 command line and,
//! later, batch jobs and CL programs.

use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio_postgres::Client;

use crate::bridge;
use crate::catalog;
use crate::cl::{self, Command};
use crate::dtaara;
use crate::jobs;
use crate::users;

/// Settings that come from the server, not from the job.
#[derive(Debug, Clone)]
pub struct Environment {
    pub database_url: String,
    /// Command that runs RPG programs (the Jariko runner).
    pub runner: String,
    pub call_timeout: Duration,
}

/// The parts of a job that commands read and change.
pub struct Job<'a> {
    pub client: &'a Client,
    pub env: &'a Environment,
    pub user: String,
    /// The current library, or None (*CRTDFT: objects are created in QGPL).
    pub curlib: Option<String>,
    /// User part of the library list.
    pub libl: Vec<String>,
    /// The job's display, for programs that use display files.
    pub display: Option<bridge::DisplayHandle>,
    /// The job's qualified name, for jobs the system knows about
    /// (interactive sessions, submitted jobs). None for `ibmish call`.
    pub id: Option<jobs::JobId>,
}

/// A list shown on its own screen (DSPLIB, DSPLIBL, ...).
#[derive(Debug, Clone, Default)]
pub struct ListDisplay {
    pub title: String,
    pub info: Vec<(String, String)>,
    pub heading: String,
    pub rows: Vec<String>,
    /// Options the operator can type next to a row: (number, text,
    /// command with `{}` for the row's key). Empty: a plain list.
    pub options: Vec<(u8, String, String)>,
    /// One key per row, for the options' commands. A row with an empty
    /// key takes no option.
    pub keys: Vec<String>,
    /// The command that shows this list again with fresh data (F5), if
    /// the list can change while it is shown.
    pub refresh: Option<String>,
}

/// The system part of the library list (system value QSYSLIBL as shipped).
pub const SYSTEM_LIBRARY_LIST: &[&str] = &["QSYS", "QSYS2", "QHLPSYS", "QUSRSYS"];

/// The user part of a new job's library list (system value QUSRLIBL as
/// shipped).
pub const USER_LIBRARY_LIST: &[&str] = &["QGPL", "QTEMP"];

/// The library objects are created in when there is no current library.
pub const DEFAULT_CURRENT_LIBRARY: &str = "QGPL";

#[derive(Debug)]
pub enum Outcome {
    /// Completion and informational messages, in order.
    Messages(Vec<String>),
    Display(ListDisplay),
    /// CHGPWD: show the Change Password screen.
    ChangePassword,
    /// STRSQL: the interactive SQL screen.
    StartSql,
    SignOff,
}

/// DSPPFM shows at most this many records.
pub const DSPPFM_MAX_RECORDS: i64 = 9999;

impl Job<'_> {
    /// The library list as searched: system part, current library, user
    /// part.
    pub fn library_list(&self) -> Vec<String> {
        self.library_list_entries()
            .into_iter()
            .map(|(l, _)| l)
            .collect()
    }

    /// The library list with each library's type (SYS, CUR, USR), as
    /// DSPLIBL shows it.
    pub fn library_list_entries(&self) -> Vec<(String, &'static str)> {
        let mut out: Vec<(String, &'static str)> = SYSTEM_LIBRARY_LIST
            .iter()
            .map(|l| (l.to_string(), "SYS"))
            .collect();
        if let Some(c) = &self.curlib {
            out.push((c.clone(), "CUR"));
        }
        for l in &self.libl {
            if !out.iter().any(|(x, _)| x == l) {
                out.push((l.clone(), "USR"));
            }
        }
        out
    }

    /// *CURLIB: the current library, or QGPL when there is none.
    pub fn current_library(&self) -> String {
        self.curlib
            .clone()
            .unwrap_or_else(|| DEFAULT_CURRENT_LIBRARY.to_string())
    }

    pub async fn run(&mut self, line: &str) -> Result<Outcome> {
        let cmd = cl::parse(line)?;
        match cmd.def.name {
            "CRTLIB" => self.crtlib(&cmd).await,
            "DSPLIB" => self.dsplib(&cmd).await,
            "DSPLIBL" => self.dsplibl().await,
            "ADDLIBLE" => self.addlible(&cmd).await,
            "RMVLIBLE" => self.rmvlible(&cmd).await,
            "CHGCURLIB" => self.chgcurlib(&cmd).await,
            "CHGLIBL" => self.chglibl(&cmd).await,
            "CALL" => self.call(&cmd).await,
            "CRTUSRPRF" => self.crtusrprf(&cmd).await,
            "DSPUSRPRF" => self.dspusrprf(&cmd).await,
            "DLTUSRPRF" => self.dltusrprf(&cmd).await,
            "CHGPWD" => Ok(Outcome::ChangePassword),
            "STRSQL" => Ok(Outcome::StartSql),
            "RUNSQL" => self.runsql(&cmd).await,
            "WRKOBJ" => self.wrkobj(&cmd).await,
            "DSPPFM" => self.dsppfm(&cmd).await,
            "CRTDTAARA" => self.crtdtaara(&cmd).await,
            "CHGDTAARA" => self.chgdtaara(&cmd).await,
            "DSPDTAARA" => self.dspdtaara(&cmd).await,
            "DLTDTAARA" => self.dltdtaara(&cmd).await,
            "SBMJOB" => self.sbmjob(&cmd).await,
            "WRKACTJOB" => self.wrkactjob().await,
            "WRKSBMJOB" => self.wrksbmjob().await,
            "DSPJOBLOG" => self.dspjoblog(&cmd).await,
            "ENDJOB" => self.endjob(&cmd).await,
            "SNDMSG" => self.sndmsg(&cmd).await,
            "DSPMSG" => self.dspmsg(&cmd).await,
            "SIGNOFF" => Ok(Outcome::SignOff),
            other => bail!("CPD0030: Command {other} is not implemented yet."),
        }
    }

    async fn crtlib(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = catalog::create_library_new(self.client, cmd.get("LIB"), cmd.get("TEXT")).await?;
        Ok(Outcome::Messages(vec![format!("Library {lib} created.")]))
    }

    async fn dsplib(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = cmd.get("LIB");
        if lib == "*LIBL" {
            return self.dsplibl().await;
        }
        let lib = if lib == "*CURLIB" {
            self.current_library()
        } else {
            lib.to_string()
        };
        let objects = catalog::list_objects(self.client, &lib).await?;
        let text = catalog::library_text(self.client, &lib).await?;
        let rows = objects
            .iter()
            .map(|o| {
                format!(
                    "{:<10}  {:<10}  {:<10}  {}",
                    o.name, o.objtype, o.attribute, o.text
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Display Library".into(),
            info: vec![
                ("Library".into(), lib.clone()),
                ("Number of objects".into(), objects.len().to_string()),
                ("Text".into(), text),
            ],
            heading: format!(
                "{:<10}  {:<10}  {:<10}  {}",
                "Object", "Type", "Attribute", "Text"
            ),
            rows,
            ..Default::default()
        }))
    }

    async fn dsplibl(&mut self) -> Result<Outcome> {
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for (lib, kind) in self.library_list_entries() {
            let text = catalog::library_text(self.client, &lib)
                .await
                .unwrap_or_default();
            rows.push(format!("{lib:<10}  {kind:<4}  {text}"));
            keys.push(lib);
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Library List".into(),
            info: vec![],
            heading: format!("{:<10}  {:<4}  {}", "Library", "Type", "Text"),
            rows,
            options: vec![(
                5,
                "Display objects in library".into(),
                "DSPLIB LIB({})".into(),
            )],
            keys,
            refresh: None,
        }))
    }

    async fn rmvlible(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = cmd.get("LIB").to_string();
        let before = self.libl.len();
        self.libl.retain(|l| *l != lib);
        if self.libl.len() == before {
            if self.curlib.as_deref() == Some(lib.as_str())
                || SYSTEM_LIBRARY_LIST.contains(&lib.as_str())
            {
                bail!("Library {lib} is not in the user part of the library list and was not removed.");
            }
            bail!("Library {lib} is not in the library list.");
        }
        Ok(Outcome::Messages(vec![format!(
            "Library {lib} removed from library list."
        )]))
    }

    async fn chgcurlib(&mut self, cmd: &Command) -> Result<Outcome> {
        match cmd.get("CURLIB") {
            "*CRTDFT" => {
                self.curlib = None;
                Ok(Outcome::Messages(vec![
                    "Current library removed; objects are created in QGPL.".into(),
                ]))
            }
            lib => {
                if !catalog::library_exists(self.client, lib).await? {
                    bail!("CPF2110: Library {lib} not found.");
                }
                self.curlib = Some(lib.to_string());
                Ok(Outcome::Messages(vec![format!(
                    "Current library changed to {lib}."
                )]))
            }
        }
    }

    async fn chglibl(&mut self, cmd: &Command) -> Result<Outcome> {
        let list = cmd.list("LIBL");
        let new_libl: Option<Vec<String>> = match list.as_slice() {
            [one] if one == "*SAME" => None,
            [one] if one == "*NONE" => Some(vec![]),
            libs => {
                let mut v: Vec<String> = Vec::new();
                for l in libs {
                    let l = catalog::validate_name("library", l)?;
                    if !catalog::library_exists(self.client, &l).await? {
                        bail!("CPF2110: Library {l} not found.");
                    }
                    if v.contains(&l) {
                        bail!("Library {l} is in the list more than once.");
                    }
                    v.push(l);
                }
                Some(v)
            }
        };
        match cmd.get("CURLIB") {
            "*SAME" => {}
            "*CRTDFT" => self.curlib = None,
            lib => {
                if !catalog::library_exists(self.client, lib).await? {
                    bail!("CPF2110: Library {lib} not found.");
                }
                self.curlib = Some(lib.to_string());
            }
        }
        if let Some(v) = new_libl {
            self.libl = v;
        }
        Ok(Outcome::Messages(vec!["Library list changed.".into()]))
    }

    async fn addlible(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = cmd.get("LIB").to_string();
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} not found.");
        }
        if self.library_list().contains(&lib) {
            bail!("CPF2103: Library {lib} already exists in library list.");
        }
        // Only the user part of the library list can be changed.
        match cmd.get("POSITION") {
            "*LAST" => self.libl.push(lib.clone()),
            _ => self.libl.insert(0, lib.clone()),
        }
        Ok(Outcome::Messages(vec![format!(
            "Library {lib} added to library list."
        )]))
    }

    /// RUNSQL: one SQL statement that does not return rows.
    async fn runsql(&mut self, cmd: &Command) -> Result<Outcome> {
        let stmt = cmd.get("SQL");
        let t = crate::sql::translate(stmt, &|_| false)?;
        if t.kind == crate::sql::Kind::Query {
            bail!("SQL0084: RUNSQL cannot run a SELECT; use STRSQL to see rows.");
        }
        match crate::sql::run(
            self.client,
            &self.env.database_url,
            &self.library_list(),
            stmt,
        )
        .await?
        {
            crate::sql::Outcome::Count(kind, n) => {
                Ok(Outcome::Messages(vec![crate::sql::count_message(kind, n)]))
            }
            crate::sql::Outcome::Rows { .. } => Ok(Outcome::Messages(vec![])),
        }
    }

    async fn require_secadm(&self, what: &str) -> Result<()> {
        if !users::has_special(self.client, &self.user, "*SECADM").await? {
            bail!("Not authorized to {what}: *SECADM special authority is needed.");
        }
        Ok(())
    }

    async fn crtusrprf(&mut self, cmd: &Command) -> Result<Outcome> {
        self.require_secadm("create user profiles").await?;
        let name = cmd.get("USRPRF").to_string();
        let password = match cmd.get("PASSWORD") {
            "*USRPRF" => users::NewPassword::SameAsProfile,
            "*NONE" => users::NewPassword::None,
            pw => users::NewPassword::Value(pw.to_string()),
        };
        let usrcls = cmd.get("USRCLS").to_string();
        let special = match cmd.list("SPCAUT").as_slice() {
            [one] if one == "*USRCLS" => users::class_special_authorities(&usrcls),
            [one] if one == "*NONE" => vec![],
            list => list.to_vec(),
        };
        let curlib = match cmd.get("CURLIB") {
            "*CRTDFT" => "*CRTDFT".to_string(),
            lib => {
                if !catalog::library_exists(self.client, lib).await? {
                    bail!("CPF2110: Library {lib} not found.");
                }
                lib.to_string()
            }
        };
        users::create(
            self.client,
            users::NewProfile {
                name: name.clone(),
                password,
                expired: cmd.get("PWDEXP") == "*YES",
                enabled: cmd.get("STATUS") != "*DISABLED",
                usrcls,
                special,
                curlib,
                text: cmd.get("TEXT").to_string(),
            },
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "User profile {name} created."
        )]))
    }

    async fn dspusrprf(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = cmd.get("USRPRF").to_string();
        if name != self.user {
            self.require_secadm("display other user profiles").await?;
        }
        let Some(p) = users::get(self.client, &name).await? else {
            bail!("CPF2204: User profile {name} not found.");
        };
        let yes_no = |b: bool| if b { "*YES" } else { "*NO" }.to_string();
        Ok(Outcome::Display(ListDisplay {
            title: "Display User Profile".into(),
            info: vec![
                ("User profile".into(), p.name),
                ("User class".into(), p.usrcls),
                ("Status".into(), p.status),
                ("Password is *NONE".into(), yes_no(p.no_password)),
                ("Set password to expired".into(), yes_no(p.pwd_expired)),
                ("Current library".into(), p.curlib),
                ("Text".into(), p.text),
                (
                    "Previous sign-on".into(),
                    p.last_signon.unwrap_or_else(|| "*NONE".into()),
                ),
                ("Created".into(), p.created),
            ],
            heading: "Special authority".into(),
            rows: if p.special_auth.is_empty() {
                vec!["*NONE".into()]
            } else {
                p.special_auth
            },
            ..Default::default()
        }))
    }

    async fn dltusrprf(&mut self, cmd: &Command) -> Result<Outcome> {
        self.require_secadm("delete user profiles").await?;
        let name = cmd.get("USRPRF").to_string();
        if name == "QSECOFR" {
            bail!("User profile QSECOFR is supplied with the system and cannot be deleted.");
        }
        if name == self.user {
            bail!("User profile {name} is signed on in this job and cannot be deleted.");
        }
        if !users::delete(self.client, &name).await? {
            bail!("CPF2204: User profile {name} not found.");
        }
        Ok(Outcome::Messages(vec![format!(
            "User profile {name} deleted."
        )]))
    }

    async fn wrkobj(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, obj) = match cmd.get("OBJ").split_once('/') {
            Some((l, o)) => (l.to_string(), o.to_string()),
            None => ("*LIBL".to_string(), cmd.get("OBJ").to_string()),
        };
        let objtype = cmd.get("OBJTYPE").to_string();
        let libs: Vec<String> = match lib.as_str() {
            "*LIBL" => self.library_list(),
            "*CURLIB" => vec![self.current_library()],
            "*USRLIBL" => self.libl.clone(),
            "*ALL" => catalog::list_libraries(self.client).await?,
            "*ALLUSR" => catalog::list_libraries(self.client)
                .await?
                .into_iter()
                .filter(|l| !l.starts_with('Q'))
                .collect(),
            l => {
                if !catalog::library_exists(self.client, l).await? {
                    bail!("CPF2110: Library {l} not found.");
                }
                vec![l.to_string()]
            }
        };
        let mut rows = Vec::new();
        for l in &libs {
            let Ok(objects) = catalog::list_objects(self.client, l).await else {
                continue; // a library in the list that no longer exists
            };
            for o in objects {
                if catalog::name_matches(&obj, &o.name)
                    && (objtype == "*ALL" || objtype == o.objtype)
                {
                    rows.push(format!(
                        "{:<10}  {:<8}  {:<10}  {:<10}  {}",
                        o.name, o.objtype, l, o.attribute, o.text
                    ));
                }
            }
        }
        if rows.is_empty() {
            bail!("No objects match {lib}/{obj} type {objtype}.");
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Objects".into(),
            info: vec![],
            heading: format!(
                "{:<10}  {:<8}  {:<10}  {:<10}  {}",
                "Object", "Type", "Library", "Attribute", "Text"
            ),
            rows,
            ..Default::default()
        }))
    }

    async fn dsppfm(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = match cmd.get("FILE").split_once('/') {
            Some(("*LIBL", n)) => n.to_string(),
            Some(("*CURLIB", n)) => format!("{}/{n}", self.current_library()),
            _ => cmd.get("FILE").to_string(),
        };
        let f = catalog::resolve_file(self.client, &self.library_list(), &name).await?;
        let recs = catalog::member_records(self.client, &f, DSPPFM_MAX_RECORDS + 1).await?;
        let widths: Vec<usize> = f
            .file
            .fields
            .iter()
            .map(|fd| {
                let w = if fd.data_type.is_numeric() {
                    // digits, decimal point, sign
                    fd.length as usize + usize::from(fd.decimals > 0) + 1
                } else {
                    fd.length as usize
                };
                w.max(fd.name.len())
            })
            .collect();
        // Numbers (and their headings) are right-aligned, text left-aligned.
        let line = |vals: &[String], numeric_right: bool| -> String {
            vals.iter()
                .zip(&widths)
                .zip(&f.file.fields)
                .map(|((v, w), fd)| {
                    if numeric_right && fd.data_type.is_numeric() {
                        format!("{v:>w$}")
                    } else {
                        format!("{v:<w$}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end()
                .to_string()
        };
        let names: Vec<String> = f.file.fields.iter().map(|fd| fd.name.clone()).collect();
        let truncated = recs.len() as i64 > DSPPFM_MAX_RECORDS;
        let rows: Vec<String> = recs
            .iter()
            .take(DSPPFM_MAX_RECORDS as usize)
            .map(|(_, v)| line(v, true))
            .collect();
        let mut info = vec![
            ("File".to_string(), f.file.name.clone()),
            ("Library".to_string(), f.library.clone()),
            ("Records shown".to_string(), rows.len().to_string()),
        ];
        if truncated {
            info.push((
                "Note".into(),
                format!("only the first {DSPPFM_MAX_RECORDS} records are shown"),
            ));
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Physical File Member".into(),
            info,
            heading: line(&names, true),
            rows,
            ..Default::default()
        }))
    }

    /// SBMJOB: put the command on job queue QBATCH, to run in a new batch
    /// job with this job's user, current library and library list.
    async fn sbmjob(&mut self, cmd: &Command) -> Result<Outcome> {
        let command = cmd.get("CMD").trim().to_string();
        // Check it now, as SBMJOB does: a command that does not parse is
        // never submitted.
        cl::parse(&command)?;
        let name = match cmd.get("JOB") {
            "*JOBD" => jobs::DEFAULT_BATCH_JOB_NAME.to_string(),
            n => n.to_string(),
        };
        let jobq = match cmd.get("JOBQ") {
            "*JOBD" | "QBATCH" | "*LIBL/QBATCH" | "QGPL/QBATCH" | "QSYS/QBATCH" => {
                jobs::BATCH_QUEUE
            }
            other => bail!("Job queue {other} not found. The job queue is QBATCH (JOBQ(*JOBD))."),
        };
        let curlib = match cmd.get("CURLIB") {
            "*CURRENT" => self.curlib.clone(),
            "*CRTDFT" => None,
            l => {
                if !catalog::library_exists(self.client, l).await? {
                    bail!("CPF2110: Library {l} not found.");
                }
                Some(l.to_string())
            }
        };
        let inllibl = cmd.list("INLLIBL");
        let libl = if inllibl.is_empty() || inllibl == ["*CURRENT"] {
            self.libl.clone()
        } else {
            inllibl
        };
        let id = jobs::submit(
            self.client,
            &jobs::Submit {
                name,
                user: self.user.clone(),
                command,
                jobq: jobq.to_string(),
                curlib,
                libl,
                submitted_by: self.user.clone(),
                submitter: self.id.as_ref().map(|i| i.number),
            },
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC1221: Job {id} submitted to job queue {jobq} in library QGPL."
        )]))
    }

    async fn wrkactjob(&mut self) -> Result<Outcome> {
        let active = jobs::active(self.client).await?;
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        let fmt = |job: &str, user: &str, ty: &str, func: &str, status: &str| {
            format!("{job:<12}  {user:<10}  {ty:<4}  {func:<16}  {status}")
        };
        for sbs in [jobs::BATCH_QUEUE, jobs::INTERACTIVE_SUBSYSTEM] {
            rows.push(fmt(sbs, "QSYS", "SBS", "", "DEQW"));
            keys.push(String::new());
            for j in active.iter().filter(|j| j.subsystem == sbs) {
                rows.push(fmt(
                    &format!("  {}", j.id.name),
                    &j.id.user,
                    &j.job_type,
                    &j.function,
                    &j.activity,
                ));
                keys.push(j.id.to_string());
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Active Jobs".into(),
            info: vec![("Active jobs".into(), active.len().to_string())],
            heading: fmt("Subsystem/Job", "User", "Type", "Function", "Status"),
            rows,
            options: vec![(5, "Display job log".into(), "DSPJOBLOG JOB({})".into())],
            keys,
            refresh: Some("WRKACTJOB".into()),
        }))
    }

    async fn wrksbmjob(&mut self) -> Result<Outcome> {
        let list = jobs::submitted_by(self.client, &self.user).await?;
        let fmt = |job: &str, user: &str, number: &str, status: &str, func: &str| {
            format!("{job:<10}  {user:<10}  {number:<6}  {status:<6}  {func}")
        };
        let rows = list
            .iter()
            .map(|j| {
                let status = if j.status == "OUTQ" && j.end_code == "ABNORMAL" {
                    "OUTQ  (ended abnormally)".to_string()
                } else {
                    j.status.clone()
                };
                fmt(
                    &j.id.name,
                    &j.id.user,
                    &format!("{:06}", j.id.number),
                    &status,
                    &j.function,
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Submitted Jobs".into(),
            info: vec![("Submitted from".into(), format!("*USER ({})", self.user))],
            heading: fmt("Job", "User", "Number", "Status", "Function"),
            rows,
            options: vec![
                (4, "End".into(), "ENDJOB JOB({})".into()),
                (5, "Display job log".into(), "DSPJOBLOG JOB({})".into()),
            ],
            keys: list.iter().map(|j| j.id.to_string()).collect(),
            refresh: Some("WRKSBMJOB".into()),
        }))
    }

    /// The job a JOB parameter names; `*` is this job.
    async fn job_named(&self, spec: &str) -> Result<jobs::JobInfo> {
        if spec.trim() == "*" || spec.trim().is_empty() {
            let id = self
                .id
                .as_ref()
                .ok_or_else(|| anyhow!("This job has no job log (it is not a system job)."))?;
            return jobs::get(self.client, id.number).await;
        }
        jobs::find(self.client, &jobs::parse_spec(spec)?).await
    }

    async fn dspjoblog(&mut self, cmd: &Command) -> Result<Outcome> {
        let job = self.job_named(cmd.get("JOB")).await?;
        let entries = jobs::job_log(self.client, job.id.number).await?;
        let rows = entries
            .iter()
            .flat_map(|e| {
                let first = if e.kind == "*RQS" {
                    format!("{}  > {}", e.sent, e.text)
                } else {
                    format!("{}    {}", e.sent, e.text)
                };
                let mut out = Vec::new();
                // Long messages continue on the next lines.
                let mut rest = first.as_str();
                while rest.chars().count() > 76 {
                    let cut = rest
                        .char_indices()
                        .nth(76)
                        .map(|(i, _)| i)
                        .unwrap_or(rest.len());
                    let at = rest[..cut].rfind(' ').filter(|&i| i > 20).unwrap_or(cut);
                    out.push(rest[..at].to_string());
                    rest = rest[at..].trim_start();
                    let indent = "              ";
                    if rest.chars().count() <= 76 - indent.len() {
                        out.push(format!("{indent}{rest}"));
                        rest = "";
                        break;
                    }
                }
                if !rest.is_empty() {
                    out.push(rest.to_string());
                }
                out
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Display Job Log".into(),
            info: vec![
                ("Job".into(), job.id.name.clone()),
                ("User".into(), job.id.user.clone()),
                ("Number".into(), format!("{:06}", job.id.number)),
            ],
            heading: "Time      Message".into(),
            rows,
            ..Default::default()
        }))
    }

    async fn endjob(&mut self, cmd: &Command) -> Result<Outcome> {
        let job = self.job_named(cmd.get("JOB")).await?;
        if job.status == "JOBQ" {
            if job.id.user != self.user {
                self.require_secadm("end other users' jobs").await?;
            }
            jobs::end_queued(self.client, &job.id).await?;
            return Ok(Outcome::Messages(vec![format!(
                "Job {} ended; it was taken off job queue {}.",
                job.id, job.jobq
            )]));
        }
        if job.status == "OUTQ" {
            bail!("Job {} has already ended.", job.id);
        }
        bail!(
            "Job {} is active. Ending active jobs is not supported yet.",
            job.id
        )
    }

    async fn sndmsg(&mut self, cmd: &Command) -> Result<Outcome> {
        let to = cmd.get("TOUSR").to_string();
        let to = if to == "*SYSOPR" {
            "QSYSOPR".to_string()
        } else {
            to
        };
        if to != "QSYSOPR" && users::get(self.client, &to).await?.is_none() {
            bail!("CPF2204: User profile {to} not found.");
        }
        let from_job = self.id.as_ref().map(|i| i.to_string()).unwrap_or_default();
        jobs::send_message(
            self.client,
            &to,
            &self.user,
            &from_job,
            "*INFO",
            cmd.get("MSG"),
        )
        .await?;
        Ok(Outcome::Messages(vec![format!("Message sent to {to}.")]))
    }

    async fn dspmsg(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = match cmd.get("MSGQ") {
            "*USRPRF" => self.user.clone(),
            q => {
                if q != self.user {
                    self.require_secadm("display other users' messages").await?;
                }
                q.to_string()
            }
        };
        let msgs = jobs::messages(self.client, &q, q == self.user).await?;
        let mut rows = Vec::new();
        for m in msgs.iter().rev() {
            rows.push(format!("{}{}", if m.is_new { "* " } else { "  " }, m.text));
            rows.push(format!("    From {} at {}", m.from_user, m.sent));
        }
        if rows.is_empty() {
            rows.push("(no messages)".into());
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Messages".into(),
            info: vec![
                ("Queue".into(), q.clone()),
                ("Messages".into(), msgs.len().to_string()),
            ],
            heading: "Newest first; * = new".into(),
            rows,
            ..Default::default()
        }))
    }

    async fn crtdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = match cmd.get("DTAARA").split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (self.current_library(), cmd.get("DTAARA").to_string()),
        };
        let ty = dtaara::Type::parse(cmd.get("TYPE"))?;
        let len = cmd.list("LEN");
        let (length, decimals) = match len.as_slice() {
            [d] if d == "*DFT" => ty.default_length(),
            [l] => (number(l, "LEN")?, 0),
            [l, d] => (number(l, "LEN")?, number(d, "LEN")?),
            _ => bail!("LEN takes a length, and for *DEC a number of decimal positions."),
        };
        let value = match cmd.get("VALUE") {
            "*DFT" => "",
            v => v,
        };
        let d = dtaara::create(
            self.client,
            &lib,
            &name,
            ty,
            length,
            decimals,
            value,
            cmd.get("TEXT"),
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "Data area {} created in library {}.",
            d.name, d.library
        )]))
    }

    async fn chgdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        let vals = cmd.list("DTAARA");
        let (name, part) = match vals.as_slice() {
            [n] => (n.clone(), None),
            [n, p] => {
                let inner = p.trim_start_matches('(').trim_end_matches(')');
                (n.clone(), dtaara::parse_substring(inner)?)
            }
            _ => bail!("DTAARA takes a data area name and, optionally, (start length)."),
        };
        let d = dtaara::change(
            self.client,
            &name,
            &self.library_list(),
            &self.current_library(),
            part,
            cmd.get("VALUE"),
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "Data area {} in {} changed.",
            d.name, d.library
        )]))
    }

    async fn dspdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        let d = dtaara::resolve(
            self.client,
            cmd.get("DTAARA"),
            &self.library_list(),
            &self.current_library(),
        )
        .await?;
        // Character values a line of 50 at a time, with their offsets.
        let rows: Vec<String> = match d.ty {
            dtaara::Type::Char => d
                .value
                .chars()
                .collect::<Vec<_>>()
                .chunks(50)
                .enumerate()
                .map(|(i, c)| format!("{:>6}   '{}'", i * 50, c.iter().collect::<String>()))
                .collect(),
            _ => vec![format!("{:>6}   {}", 0, d.value)],
        };
        Ok(Outcome::Display(ListDisplay {
            title: "Display Data Area".into(),
            info: vec![
                ("Data area".into(), d.name.clone()),
                ("Library".into(), d.library.clone()),
                ("Type".into(), d.ty.code().into()),
                ("Length".into(), d.length_text()),
                ("Text".into(), d.text.clone()),
            ],
            heading: format!("{:>6}   {}", "Offset", "Value"),
            rows,
            ..Default::default()
        }))
    }

    async fn dltdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        let d = dtaara::delete(
            self.client,
            cmd.get("DTAARA"),
            &self.library_list(),
            &self.current_library(),
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "Data area {} deleted from library {}.",
            d.name, d.library
        )]))
    }

    /// CALL: run an RPG program on the runner, with file I/O through a
    /// bridge that uses this job's library list. DSPLY lines come back as
    /// messages.
    async fn call(&mut self, cmd: &Command) -> Result<Outcome> {
        let pgm = cmd.get("PGM").to_string();
        self.call_program(&pgm, cmd.list("PARM")).await
    }

    /// Run a command line, boxed: CL programs run commands, and commands
    /// run CL programs, so the futures nest.
    pub fn run_boxed<'b>(
        &'b mut self,
        line: &'b str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Outcome>> + Send + 'b>> {
        Box::pin(self.run(line))
    }

    /// CALL from a CL program (boxed for the same reason).
    pub fn call_program_boxed<'b>(
        &'b mut self,
        pgm: &'b str,
        args: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Outcome>> + Send + 'b>> {
        Box::pin(self.call_program(pgm, args))
    }

    /// CALL: a CL program found through the library list runs here;
    /// anything else goes to the RPG runner.
    pub async fn call_program(&mut self, pgm: &str, args: Vec<String>) -> Result<Outcome> {
        let pgm = pgm.to_ascii_uppercase();
        let (libs, name) = match pgm.split_once('/') {
            Some(("*LIBL", n)) => (self.library_list(), n.to_string()),
            Some(("*CURLIB", n)) => (vec![self.current_library()], n.to_string()),
            Some((l, n)) => (vec![l.to_string()], n.to_string()),
            None => (self.library_list(), pgm.clone()),
        };
        if let Some(id) = &self.id {
            jobs::set_function(self.client, id, &format!("PGM-{name}"), "RUN").await;
        }
        if let Some((_, source)) = catalog::find_cl_program(self.client, &libs, &name).await? {
            let program = crate::clpgm::compile(&name, &source)?;
            return crate::clpgm::run(self, &program, &args).await;
        }
        self.call_rpg(&name, args).await
    }

    async fn call_rpg(&mut self, pgm: &str, args: Vec<String>) -> Result<Outcome> {
        let pgm = pgm.to_string();
        let listener = bridge::bind(0).await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(bridge::serve(
            listener,
            bridge::BridgeConfig {
                database_url: self.env.database_url.clone(),
                library_list: self.library_list(),
                display: self.display.clone(),
                job: self.id.as_ref().map(|i| i.to_string()),
                record_wait: None,
            },
        ));
        let mut parts = self.env.runner.split_whitespace();
        let exe = parts
            .next()
            .ok_or_else(|| anyhow!("no runner configured (IBMISH_RUNNER)"))?;
        let child = tokio::process::Command::new(exe)
            .args(parts)
            .arg("--bridge")
            .arg(addr.to_string())
            .arg(&pgm)
            .args(&args)
            .env("IBMISH_LIBL", self.library_list().join(","))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting runner '{}'", self.env.runner))?;
        // A batch call has a time limit; an interactive one waits for the
        // operator as long as it takes.
        let out = if self.display.is_some() {
            Ok(child.wait_with_output().await)
        } else {
            tokio::time::timeout(self.env.call_timeout, child.wait_with_output()).await
        };
        server.abort();
        let out = match out {
            Ok(r) => r?,
            Err(_) => bail!(
                "Program {pgm} did not end within {}s and was cancelled.",
                self.env.call_timeout.as_secs()
            ),
        };
        let mut msgs: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with("Picked up JAVA_TOOL_OPTIONS"))
            .map(|l| format!("DSPLY  {}", l.trim_end()))
            .collect();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let reason = stderr
                .lines()
                .rev()
                .find(|l| l.contains("RNQ") || l.contains("CPF") || l.contains("not found"))
                .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
                .unwrap_or("no details")
                .trim()
                .to_string();
            msgs.push(reason);
            msgs.push(format!("Program {pgm} ended abnormally."));
            return Err(anyhow!(msgs.join("\n")));
        }
        Ok(Outcome::Messages(msgs))
    }
}

/// A whole number in a command parameter.
fn number(s: &str, keyword: &str) -> Result<u32> {
    s.trim().parse().map_err(|_| {
        anyhow!("CPF0006: Errors occurred in command. {keyword} value '{s}' is not a number.")
    })
}
