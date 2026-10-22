//! Running CL commands for a job. Shared by the 5250 command line and,
//! later, batch jobs and CL programs.

use anyhow::{anyhow, bail, Result};
use tokio_postgres::Client;

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::{self, Command};
use crate::dtaara;
use crate::jobs;
use crate::runtime;
use crate::users;

/// Settings that come from the server, not from the job.
#[derive(Debug, Clone)]
pub struct Environment {
    pub database_url: String,
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
    pub display: Option<runtime::DisplayHandle>,
    /// The job's qualified name, for jobs the system knows about
    /// (interactive sessions, submitted jobs). None for `as4tainr call`.
    pub id: Option<jobs::JobId>,
    /// The job's call stack as CL sees it: each CL program's message
    /// queue, the escape message being raised, file overrides by call
    /// level.
    pub calls: crate::pgmmsg::CallStack,
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
    /// The report the command prints instead (OUTPUT(*PRINT), batch, the
    /// `system` utility), in IBM's layout (`report.rs`). Empty: the list
    /// is printed as shown.
    pub print: Vec<String>,
    /// The display laid out as IBM i draws it (header lines instead of
    /// `info`, the list's place, the keys); `None`: the plain layout.
    pub ibm: Option<Box<IbmLayout>>,
}

/// How IBM i 7.5 lays out a list display (observed on PUB400: DSPLIB,
/// DSPLIBL, DSPDTAARA, WRKOBJ, DSPOBJD, WRKSPLF, DSPJOBLOG, DSPMSG). The
/// title is on line 1; the keys end on line 23, "More..."/"Bottom" sits
/// just above them, or above "Press Enter to continue." or the command
/// line.
#[derive(Debug, Clone, Default)]
pub struct IbmLayout {
    /// "System:   NAME" on line 2 from this column (62 on most displays).
    pub system_col: Option<u8>,
    /// Text on line 2, ending in column 79 ("Library 1 of 1").
    pub right2: String,
    /// Header lines from line 3, from column 2; a blank line follows them.
    pub lines: Vec<String>,
    /// The line above the list when there are no options ("Type reply
    /// (if required), press Enter.").
    pub instruction: String,
    /// A first heading line above `heading` ("Device or", "ASP", "Value").
    pub heading2: String,
    /// "Parameters for options ... or command" and a command line.
    pub command_line: Option<String>,
    /// "Press Enter to continue." above the keys.
    pub enter_line: bool,
    /// F17=Top and F18=Bottom (the Display displays: DSPLIB, DSPLIBL,
    /// DSPOBJD, DSPJOBLOG).
    pub top_bottom_keys: bool,
    /// A second view of the same rows (F11), as (heading line above,
    /// heading, rows): WRKSPLF's job names.
    pub view2: Option<(String, String, Vec<String>)>,
}

impl IbmLayout {
    /// For `ListDisplay::ibm`.
    pub fn some(self) -> Option<Box<IbmLayout>> {
        Some(Box::new(self))
    }
}

/// A label as IBM i's displays write it: the text, dots in every other
/// column up to `last_dot` (an offset from the label's first column), a
/// blank and a colon. The parity of `last_dot` decides the dots' columns:
/// "Library  . . . . . . :" (DSPLIB, last dot at 19), "Data area . . . . . .
/// . :" (DSPDTAARA, 22), observed on IBM i 7.5.
pub fn dsp_label(text: &str, last_dot: usize) -> String {
    let mut out = text.to_string();
    let len = out.chars().count();
    let mut col = len + 1;
    if col % 2 != last_dot % 2 {
        col += 1;
    }
    while out.chars().count() < col {
        out.push(' ');
    }
    while col <= last_dot {
        out.push('.');
        if col < last_dot {
            out.push(' ');
        }
        col += 2;
    }
    while out.chars().count() < last_dot + 2 {
        out.push(' ');
    }
    out.push(':');
    out
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
    /// GO: show a menu.
    Menu(crate::menus::Menu),
    /// CALL QCMD: show the Command Entry display.
    CommandEntry,
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
        if let Some(d) = self.user_command(line).await? {
            return self.run_user_command(&d, line).await;
        }
        let cmd = cl::parse(line)?;
        if let Some(o) = self.ifs_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.journal_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.pftrg_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.save_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.objdesc_command(&cmd).await? {
            return Ok(o);
        }
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
            "CHGUSRPRF" => self.chgusrprf(&cmd).await,
            "GO" => self.go(&cmd).await,
            "CRTSRCPF" => self.crtsrcpf(&cmd).await,
            "CHGPFM" => self.chgpfm(&cmd).await,
            "CRTPF" | "CRTLF" | "CRTDSPF" => self.crtfile(&cmd).await,
            "CRTPRTF" => self.crtprtf(&cmd).await,
            "CRTBNDCL" | "CRTCLPGM" | "CRTBNDRPG" | "CRTSQLRPGI" | "CRTRPGPGM" => {
                self.crtpgm(&cmd).await
            }
            "CVTRPGSRC" => self.cvtrpgsrc(&cmd).await,
            "CVTRPGFREE" => self.cvtrpgfree(&cmd).await,
            "CRTMNU" => self.crtmnu(&cmd).await,
            "CRTCMD" => self.crtcmd(&cmd).await,
            "DLTCMD" => self.dltcmd(&cmd).await,
            "DSPCMD" => self.dspcmd(&cmd).await,
            "CRTMSGF" => self.crtmsgf(&cmd).await,
            "OVRDBF" => self.override_file(&cmd, crate::ovr::Kind::Db),
            "STRCMTCTL" => self.strcmtctl(&cmd).await,
            "ENDCMTCTL" => self.endcmtctl().await,
            "COMMIT" | "ROLLBACK" => self.commit(cmd.def.name == "COMMIT").await,
            "OVRPRTF" => self.override_file(&cmd, crate::ovr::Kind::Printer),
            "DLTOVR" => self.dltovr(&cmd),
            "CLRPFM" => self.clrpfm(&cmd).await,
            "DLYJOB" => self.dlyjob(&cmd).await,
            "DLTMSGF" => self.dltmsgf(&cmd).await,
            "ADDMSGD" => self.addmsgd(&cmd, false).await,
            "CHGMSGD" => self.addmsgd(&cmd, true).await,
            "RMVMSGD" => self.rmvmsgd(&cmd).await,
            "DSPMSGD" => self.dspmsgd(&cmd).await,
            "DLTMNU" => self.dltmnu(&cmd).await,
            "DLTUSRPRF" => self.dltusrprf(&cmd).await,
            "CHGPWD" => Ok(Outcome::ChangePassword),
            "STRSQL" => Ok(Outcome::StartSql),
            // A statement RUNSQL cannot run: one escape message, SQL9010,
            // with the SQL error in its text (observed on IBM i 7.5).
            "RUNSQL" => self.runsql(&cmd).await.map_err(|e| {
                let t = format!("{e:#}").replace('\n', " ");
                anyhow::anyhow!("SQL9010: RUNSQL failed: {t}")
            }),
            "RUNSQLSTM" => self.runsqlstm(&cmd).await,
            "CRTRPGMOD" | "CRTCLMOD" => self.crtrpgmod(&cmd).await,
            "CALLPRC" => bail!("CALLPRC can only be used in a CL program or procedure."),
            "CRTSRVPGM" => self.crtsrvpgm(&cmd).await,
            "CRTPGM" => self.crtpgm_bound(&cmd).await,
            "CRTBNDDIR" => self.crtbnddir(&cmd).await,
            "ADDBNDDIRE" | "RMVBNDDIRE" => self.bnddire(&cmd).await,
            "DSPBNDDIR" => self.dspbnddir(&cmd).await,
            "DLTPGM" | "DLTMOD" | "DLTSRVPGM" | "DLTBNDDIR" => self.dlt_ile(&cmd).await,
            "DSPPGM" => self.dsppgm(&cmd).await,
            "DSPSRVPGM" => self.dspsrvpgm(&cmd).await,
            "UPDPGM" | "UPDSRVPGM" => self.updpgm(&cmd).await,
            "RCLACTGRP" | "RCLRSC" => {
                let key = self
                    .id
                    .as_ref()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| format!("PID{}", std::process::id()));
                // RCLRSC: the programs of the default activation group
                // that ended without LR; RCLACTGRP: an activation group
                // by name, or *ELIGIBLE (all but the default one).
                let what = if cmd.def.name == "RCLRSC" {
                    crate::ile::DEFAULT_GROUP.to_string()
                } else {
                    cmd.get("ACTGRP").to_ascii_uppercase()
                };
                let groups = match what.as_str() {
                    "*ELIGIBLE" => crate::ile::Groups::Eligible,
                    g => crate::ile::Groups::Named(g),
                };
                let n = crate::ile::end_activations(&key, &groups);
                if cmd.def.name == "RCLACTGRP" && n == 0 && what != "*ELIGIBLE" {
                    bail!("CPF1653: Activation group {what} not found.");
                }
                Ok(Outcome::Messages(vec![if cmd.def.name == "RCLRSC" {
                    "Resources reclaimed.".into()
                } else {
                    "Activation groups reclaimed.".into()
                }]))
            }
            "DLTF" => self.dltf(&cmd).await,
            "WRKOBJ" => self.wrkobj(&cmd).await,
            "DSPPFM" => self.dsppfm(&cmd).await,
            "CRTDTAARA" => self.crtdtaara(&cmd).await,
            "CHGDTAARA" => self.chgdtaara(&cmd).await,
            "DSPDTAARA" => self.dspdtaara(&cmd).await,
            "DLTDTAARA" => self.dltdtaara(&cmd).await,
            "CHKOBJ" => {
                self.chkobj(
                    cmd.get("OBJ"),
                    cmd.get("OBJTYPE"),
                    cmd.get("MBR"),
                    cmd.get("AUT"),
                )
                .await?;
                Ok(Outcome::Messages(vec![]))
            }
            "DSPSYSVAL" => self.dspsysval(&cmd),
            "DSPOBJD" => self.dspobjd(&cmd).await,
            "SBMJOB" => self.sbmjob(&cmd).await,
            "WRKACTJOB" => self.wrkactjob().await,
            "WRKSBMJOB" => self.wrksbmjob().await,
            "DSPJOBLOG" => self.dspjoblog(&cmd).await,
            "ENDJOB" => self.endjob(&cmd).await,
            "SNDMSG" => self.sndmsg(&cmd).await,
            "DSPMSG" => self.dspmsg(&cmd).await,
            "SIGNOFF" => {
                // LOG(*LIST): the job log is kept as a spooled file.
                if cmd.get("LOG") == "*LIST" {
                    if let Some(id) = &self.id {
                        let lines = crate::spool::job_log_lines(self.client, id).await?;
                        crate::spool::print_lines(self.client, id, "QPJOBLOG", &id.name, &lines)
                            .await?;
                    }
                }
                Ok(Outcome::SignOff)
            }
            "GRTOBJAUT" | "RVKOBJAUT" => self.objaut(&cmd).await,
            "DSPOBJAUT" => self.dspobjaut(&cmd).await,
            "CHGOBJOWN" => self.chgobjown(&cmd).await,
            "WRKSPLF" => self.wrksplf(&cmd).await,
            "DSPSPLF" => self.dspsplf(&cmd).await,
            "DLTSPLF" => self.dltsplf(&cmd).await,
            other => bail!("CPD0030: Command {other} is not implemented yet."),
        }
    }

    async fn crtlib(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = catalog::create_library_new(self.client, cmd.get("LIB"), cmd.get("TEXT")).await?;
        authority::set_owner(self.client, "QSYS", &lib, "*LIB", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2102: Library {lib} was created."
        )]))
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
        let kind: String = self
            .client
            .query_opt(
                "SELECT attribute FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
                &[&lib],
            )
            .await?
            .map(|r| r.get(0))
            .unwrap_or_default();
        let kind = if kind.trim().is_empty() {
            "PROD".to_string()
        } else {
            kind
        };
        // IBM i 7.5's DSPLIB display (PUB400): the library's attributes in
        // two columns, then the objects with their size (what this system
        // stores for them, fidelity-gaps.md) and text.
        let mut rows: Vec<String> = Vec::new();
        for o in &objects {
            let size = catalog::object_size(self.client, &lib, &o.name, &o.objtype)
                .await
                .unwrap_or(0);
            let text: String = o.text.chars().take(26).collect();
            rows.push(format!(
                "{:<10}  {:<8}  {:<10}  {:>11}  {}",
                o.name, o.objtype, o.attribute, size, text
            ));
        }
        let keys = objects
            .iter()
            .map(|o| format!("OBJ({lib}/{}) OBJTYPE({})", o.name, o.objtype))
            .collect();
        let pair = |l: &str, v: &str, r: &str, rv: &str| {
            let left = if l.is_empty() {
                String::new()
            } else {
                format!("{}   {v}", dsp_label(l, 19))
            };
            format!("{left:<41}{}   {rv}", dsp_label(r, 19))
        };
        Ok(Outcome::Display(ListDisplay {
            title: "Display Library".into(),
            info: vec![],
            heading: format!(
                "{:<10}  {:<8}  {:<10}  {:>11}  {}",
                "Object", "Type", "Attribute", "Size", "Text"
            ),
            rows,
            options: vec![(5, "Display description".into(), "DSPOBJD {}".into())],
            keys,
            ibm: IbmLayout::some(IbmLayout {
                top_bottom_keys: true,
                lines: vec![
                    pair(
                        "Library",
                        &lib,
                        "Number of objects",
                        &objects.len().to_string(),
                    ),
                    pair("Type", &kind, "Library ASP number", "1"),
                    pair(
                        "Create authority",
                        "*SYSVAL",
                        "Library ASP device",
                        "*SYSBAS",
                    ),
                    pair("", "", "Library ASP group", "*SYSBAS"),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    async fn dsplibl(&mut self) -> Result<Outcome> {
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        // Printed layout: PUB400 (IBM i 7.5) `system "DSPLIBL"`, 2026-10-21.
        let stamp = crate::report::Stamp::now();
        let mut print = vec![
            crate::report::header_a(18, 44, "Library List", 0, "", &stamp, 1),
            crate::report::at(&[(27, "ASP")]),
            crate::report::at(&[
                (4, "Library"),
                (16, "Type"),
                (27, "Device"),
                (39, "Text Description"),
            ]),
        ];
        for (lib, kind) in self.library_list_entries() {
            let text = catalog::library_text(self.client, &lib)
                .await
                .unwrap_or_default();
            rows.push(format!("{lib:<10}  {kind:<8}  {:<10}  {text}", ""));
            print.push(crate::report::at(&[(4, &lib), (16, kind), (39, &text)]));
            keys.push(lib);
        }
        print.push(crate::report::END_A.into());
        Ok(Outcome::Display(ListDisplay {
            title: "Display Library List".into(),
            info: vec![],
            // IBM i 7.5's DSPLIBL display (PUB400): an ASP device column.
            heading: format!(
                "{:<10}  {:<8}  {:<10}  {}",
                "Library", "Type", "Device", "Text"
            ),
            rows,
            options: vec![(
                5,
                "Display objects in library".into(),
                "DSPLIB LIB({})".into(),
            )],
            keys,
            refresh: None,
            print,
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(62),
                top_bottom_keys: true,
                lines: vec![String::new()],
                heading2: format!("{:22}ASP", ""),
                ..Default::default()
            }),
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
            // A library that does not exist: CPF2110 (observed on IBM i 7.5).
            if !catalog::library_exists(self.client, &lib).await? {
                bail!("CPF2110: Library {lib} does not exist.");
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
                    bail!("CPF2110: Library {lib} does not exist.");
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
                        bail!("CPF2110: Library {l} does not exist.");
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
                    bail!("CPF2110: Library {lib} does not exist.");
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
            bail!("CPF2110: Library {lib} does not exist.");
        }
        if self.library_list().contains(&lib) {
            bail!("CPF2103: Library {lib} is already in the library list.");
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
        let mut schema = None;
        if let Some(msg) = crate::ddl::try_run(
            &self.env.database_url,
            &self.user,
            &self.library_list(),
            Some(self.current_library()),
            &mut schema,
            stmt,
        )
        .await?
        {
            let _ = msg;
            return Ok(Outcome::Messages(vec![]));
        }
        if crate::sqlpl::parse_call(stmt)?.is_none() && !crate::sqlpl::is_compound(stmt) {
            let t = crate::sql::translate(stmt, &|_| false)?;
            if t.kind == crate::sql::Kind::Query {
                bail!("SQL0084: RUNSQL cannot run a SELECT; use STRSQL to see rows.");
            }
        }
        match crate::sql::run_as(
            self.client,
            &self.env.database_url,
            &self.library_list(),
            &self.user,
            Some(self.current_library()),
            self.id.as_ref().map(|i| i.to_string()),
            stmt,
        )
        .await?
        {
            // RUNSQL sends no completion message (observed on IBM i 7.5).
            crate::sql::Outcome::Count(..)
            | crate::sql::Outcome::Message(_)
            | crate::sql::Outcome::Rows { .. } => Ok(Outcome::Messages(vec![])),
        }
    }

    /// RUNSQLSTM: the SQL statements in a source member, one after the
    /// other. A statement that fails with a severity above ERRLVL stops the
    /// run (SQL9010); SQL errors have severity 30, so ERRLVL(30) runs
    /// every statement and reports the ones that failed.
    async fn runsqlstm(&mut self, cmd: &Command) -> Result<Outcome> {
        let errlvl: u32 = cmd
            .get("ERRLVL")
            .parse()
            .ok()
            .filter(|n| *n <= 40)
            .ok_or_else(|| anyhow::anyhow!("CPD0084: ERRLVL must be a number from 0 to 40."))?;
        // SRCSTMF, or SRCFILE and SRCMBR (one or the other).
        let stmf = cmd.get("SRCSTMF").to_string();
        let (source, what) = if !stmf.is_empty() {
            if !cmd.get("SRCFILE").is_empty() || !cmd.get("SRCMBR").is_empty() {
                bail!("CPD0084: SRCSTMF cannot be given with SRCFILE or SRCMBR.");
            }
            let (bytes, ccsid) = crate::ifs::read_file(self.client, &stmf)
                .await
                .map_err(|e| anyhow::anyhow!("CPFA0A9: Object {stmf} not found: {e}"))?;
            let text = crate::ifs::codec::text(ccsid, &bytes);
            // A byte order mark is not a statement.
            let text = text.trim_start_matches('\u{FEFF}').to_string();
            (text, format!("stream file {stmf}"))
        } else {
            if cmd.get("SRCFILE").is_empty() || cmd.get("SRCMBR").is_empty() {
                bail!("CPD0084: SRCFILE and SRCMBR, or SRCSTMF, are needed.");
            }
            let m = self.source_member(cmd, "").await?;
            let what = format!("member {} in {}/{}", m.name, m.library, m.file);
            (m.source, what)
        };
        let schema = match cmd.get("DFTRDBCOL") {
            "*NONE" => None,
            s => Some(catalog::validate_name("library", s)?),
        };
        let lines = self
            .run_sql_script(&source, &what, errlvl, schema, true)
            .await?;
        Ok(Outcome::Messages(lines))
    }

    /// Run the statements of an SQL script (RUNSQLSTM). `schema`: the
    /// default schema; with `set_schema` false, SET SCHEMA statements are
    /// skipped (`as4tainr import` keeps every object in its library).
    pub async fn run_sql_script(
        &mut self,
        source: &str,
        what: &str,
        errlvl: u32,
        mut schema: Option<String>,
        set_schema: bool,
    ) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        let mut errors = 0;
        let statements = crate::ddl::split_statements(source);
        // INSERT, UPDATE and DELETE run on one SQL session, made again
        // after data definition (which can add libraries).
        let mut session: Option<(Vec<String>, crate::sql::Session)> = None;
        for (k, stmt) in statements.iter().enumerate() {
            if !set_schema
                && matches!(
                    crate::ddl::parse(stmt),
                    Ok(Some(crate::ddl::Ddl::SetSchema(_)))
                )
            {
                lines.push(format!("Statement {}: SET SCHEMA skipped.", k + 1));
                continue;
            }
            let libl = match &schema {
                Some(s) => vec![s.clone()],
                None => self.library_list(),
            };
            let r = match crate::ddl::try_run(
                &self.env.database_url,
                &self.user,
                &self.library_list(),
                Some(self.current_library()),
                &mut schema,
                stmt,
            )
            .await
            {
                Ok(Some(msg)) => {
                    session = None;
                    Ok(msg)
                }
                Ok(None) => {
                    let t = crate::sql::translate(stmt, &|_| false);
                    let plain = crate::sqlpl::parse_call(stmt).ok().flatten().is_none()
                        && !crate::sqlpl::is_compound(stmt);
                    if matches!(&t, Ok(t) if t.kind == crate::sql::Kind::Query) {
                        Err(anyhow::anyhow!(
                            "SQL0084: SELECT is not valid in RUNSQLSTM."
                        ))
                    } else if plain && t.is_ok() {
                        if session.as_ref().map(|(l, _)| l != &libl).unwrap_or(true) {
                            let mut s = crate::sql::Session::open(
                                self.client,
                                &self.env.database_url,
                                &libl,
                            )
                            .await?;
                            s.set_context(
                                &self.user,
                                Some(self.current_library()),
                                self.id.as_ref().map(|i| i.to_string()),
                            )
                            .await?;
                            session = Some((libl.clone(), s));
                        }
                        let (_, s) = session.as_ref().unwrap();
                        match s.exec(self.client, stmt).await {
                            Ok(crate::sql::Exec::Count(kind, n)) => {
                                Ok(crate::sql::count_message(kind, n))
                            }
                            Ok(crate::sql::Exec::Rows { .. }) => Ok(String::new()),
                            Err(e) => Err(e),
                        }
                    } else {
                        match crate::sql::run_as(
                            self.client,
                            &self.env.database_url,
                            &libl,
                            &self.user,
                            Some(self.current_library()),
                            self.id.as_ref().map(|i| i.to_string()),
                            stmt,
                        )
                        .await
                        {
                            Ok(crate::sql::Outcome::Count(kind, n)) => {
                                Ok(crate::sql::count_message(kind, n))
                            }
                            Ok(crate::sql::Outcome::Message(m)) => Ok(m),
                            Ok(crate::sql::Outcome::Rows { .. }) => Ok(String::new()),
                            Err(e) => Err(e),
                        }
                    }
                }
                Err(e) => Err(e),
            };
            match r {
                Ok(msg) => lines.push(format!("Statement {}: {msg}", k + 1)),
                Err(e) => {
                    errors += 1;
                    lines.push(format!("Statement {}: {e:#}", k + 1));
                    // SQL errors have severity 30.
                    if 30 > errlvl {
                        bail!(
                            "SQL9010: RUNSQLSTM stopped at statement {} of {what}: {e:#}",
                            k + 1
                        );
                    }
                }
            }
        }
        lines.push(format!(
            "{} statements run from {what}; {errors} with errors.",
            statements.len()
        ));
        Ok(lines)
    }

    /// Module copies named on CRTPGM/CRTSRVPGM MODULE (`*PGM`/`*SRVPGM`:
    /// the module named like the object, in its library).
    async fn module_list(
        &self,
        cmd: &Command,
        lib: &str,
        name: &str,
    ) -> Result<Vec<crate::ile::ModuleCopy>> {
        let mut out = Vec::new();
        for m in cmd.list("MODULE") {
            let (l, n) = match m.as_str() {
                "*PGM" | "*SRVPGM" => (lib.to_string(), name.to_string()),
                other => match other.split_once('/') {
                    Some(("*LIBL", n)) => ("*LIBL".to_string(), n.to_string()),
                    Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
                    Some((l, n)) => (l.to_string(), n.to_string()),
                    None => ("*LIBL".to_string(), other.to_string()),
                },
            };
            let n = n.to_ascii_uppercase();
            let copy = crate::ile::find_module(
                self.client,
                &self.library_list(),
                &l.to_ascii_uppercase(),
                &n,
            )
            .await?;
            let found_lib = if l == "*LIBL" {
                let mut found = String::new();
                for x in self.library_list() {
                    if catalog::object_exists(self.client, std::slice::from_ref(&x), &n, "*MODULE")
                        .await?
                    {
                        found = x;
                        break;
                    }
                }
                found
            } else {
                l.to_ascii_uppercase()
            };
            authority::check(self.client, &self.user, &found_lib, &n, "*MODULE", Aut::Use).await?;
            out.push(copy);
        }
        Ok(out)
    }

    /// (library, name) pairs of BNDSRVPGM, and BNDDIR names.
    /// A program or service program named on a command: (library, name).
    async fn ile_object(&self, q: &str, objtype: &str, aut: Aut) -> Result<(String, String)> {
        let (libs, name) = self.search(q);
        let name = name.to_ascii_uppercase();
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, objtype).await? {
                authority::check(self.client, &self.user, l, &name, objtype, aut).await?;
                return Ok((l.clone(), name));
            }
        }
        bail!("CPF9801: Object {name} type {objtype} not found.")
    }

    /// DSPPGM: a program's attributes, modules and bound service
    /// programs (with the signatures they had when bound).
    async fn dsppgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.ile_object(cmd.get("PGM"), "*PGM", Aut::Use).await?;
        let p = catalog::find_program(self.client, std::slice::from_ref(&lib), &name)
            .await?
            .ok_or_else(|| anyhow!("CPF9801: Program {name} not found."))?;
        let b = p.binding.clone().unwrap_or_default();
        let ilerpg = matches!(p.attribute.as_str(), "RPGLE" | "SQLRPGLE" | "CLLE");
        let group = {
            let up = p.options.to_ascii_uppercase();
            if !ilerpg {
                "*DFTACTGRP (OPM program)".to_string()
            } else if let Some(i) = up.find("ACTGRP(").filter(|_| up.contains("DFTACTGRP(*NO)")) {
                up[i + 7..].split(')').next().unwrap_or("").to_string()
            } else if up.contains("DFTACTGRP(*NO)") {
                "QILE".to_string()
            } else {
                "*DFTACTGRP".to_string()
            }
        };
        let mut rows = vec![format!(
            "{:<10}  {:<10}  {:<10}  {}",
            "Module", "Library", "Attribute", "Kind"
        )];
        let entry = if b.entry_module.is_empty() {
            name.clone()
        } else {
            b.entry_module.clone()
        };
        rows.push(format!(
            "{entry:<10}  {lib:<10}  {:<10}  entry module",
            p.attribute
        ));
        for m in &b.modules {
            rows.push(format!(
                "{:<10}  {:<10}  {:<10}  bound module",
                m.name, lib, "RPGLE"
            ));
        }
        if !b.srvpgms.is_empty() {
            rows.push(String::new());
            rows.push(format!(
                "{:<10}  {:<10}  Signature",
                "Service program", "Library"
            ));
            for (l, n) in &b.srvpgms {
                let sig = b
                    .srv_signatures
                    .get(&format!("{l}/{n}"))
                    .cloned()
                    .unwrap_or_default();
                rows.push(format!("{n:<10}  {l:<10}  {sig}"));
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Program Information".into(),
            info: vec![
                ("Program".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Program attribute".into(), p.attribute.clone()),
                (
                    "Type of program".into(),
                    if ilerpg { "ILE" } else { "OPM" }.into(),
                ),
                ("Activation group attribute".into(), group),
            ],
            heading: String::new(),
            rows,
            ..Default::default()
        }))
    }

    /// DSPSRVPGM: a service program's activation group, signatures,
    /// modules and exports.
    async fn dspsrvpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self
            .ile_object(cmd.get("SRVPGM"), "*SRVPGM", Aut::Use)
            .await?;
        let (_, b) =
            crate::ile::find_srvpgm(self.client, &self.library_list(), &lib, &name).await?;
        let mut rows = vec![format!("{:<10}  Kind", "Name")];
        for m in &b.modules {
            rows.push(format!("{:<10}  module", m.name));
        }
        for e in &b.exports {
            rows.push(format!("{e:<10}  exported procedure"));
        }
        for (l, n) in &b.srvpgms {
            rows.push(format!("{n:<10}  bound service program ({l})"));
        }
        let mut info = vec![
            ("Service program".into(), name.clone()),
            ("Library".into(), lib.clone()),
            (
                "Activation group attribute".into(),
                if b.actgrp.is_empty() {
                    "*CALLER".into()
                } else {
                    b.actgrp.clone()
                },
            ),
            ("Current signature".into(), b.signature.clone()),
        ];
        for s in &b.prv_signatures {
            info.push(("Previous signature".into(), s.clone()));
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Service Program Information".into(),
            info,
            heading: String::new(),
            rows,
            ..Default::default()
        }))
    }

    /// UPDPGM, UPDSRVPGM: replace modules of a program or service program
    /// with the module objects as they are now, and bind it again (its
    /// service programs' current signatures are taken). UPDSRVPGM
    /// EXPORT(*SAME) keeps the exports and signature.
    async fn updpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let srv = cmd.def.name == "UPDSRVPGM";
        let (objtype, param) = if srv {
            ("*SRVPGM", "SRVPGM")
        } else {
            ("*PGM", "PGM")
        };
        let (lib, name) = self
            .ile_object(cmd.get(param), objtype, Aut::Change)
            .await?;
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        // MODULE(*NONE): no module replaced; the object is bound again.
        let replace = if cmd.list("MODULE").iter().any(|m| m == "*NONE") {
            Vec::new()
        } else {
            self.module_list(cmd, &lib, &name).await?
        };
        let (old, entry_source, options) = if srv {
            let (_, b) = crate::ile::find_srvpgm(self.client, &libl, &lib, &name).await?;
            (b, None, String::new())
        } else {
            let p = catalog::find_program(self.client, std::slice::from_ref(&lib), &name)
                .await?
                .ok_or_else(|| anyhow!("CPF9801: Program {name} not found."))?;
            (
                p.binding.clone().unwrap_or_default(),
                Some(p.source.clone()),
                p.options.clone(),
            )
        };
        // The modules: the entry module (a program), then the others,
        // replaced where named.
        let mut modules: Vec<crate::ile::ModuleCopy> = Vec::new();
        let entry_name = if old.entry_module.is_empty() {
            name.clone()
        } else {
            old.entry_module.clone()
        };
        if let Some(src) = &entry_source {
            let p = catalog::find_program(self.client, std::slice::from_ref(&lib), &name)
                .await?
                .ok_or_else(|| anyhow!("CPF9801: Program {name} not found."))?;
            modules.push(crate::ile::ModuleCopy {
                name: entry_name.clone(),
                source: src.clone(),
                options: String::new(),
                attribute: if p.attribute == "CLLE" {
                    "CLLE".into()
                } else {
                    "RPGLE".into()
                },
            });
        }
        modules.extend(old.modules.iter().cloned());
        let mut updated = Vec::new();
        for r in replace {
            match modules.iter_mut().find(|m| m.name == r.name) {
                Some(m) => {
                    *m = r.clone();
                    updated.push(r.name.clone());
                }
                None => bail!(
                    "Module {} is not in {objtype} {name}; it cannot be replaced.",
                    r.name
                ),
            }
        }
        let srvpgms = old.srvpgms.clone();
        let (mut binding, infos) =
            crate::ile::bind(self.client, &libl, modules.clone(), &srvpgms, &[])
                .await
                .map_err(|e| anyhow!("{objtype} {name} not updated. {e:#}"))?;
        if srv {
            let all: Vec<String> = infos.iter().flat_map(|i| i.exports.clone()).collect();
            match cmd.get("EXPORT") {
                "*ALL" => {
                    binding.signature = crate::ile::generated_signature(&all);
                    binding.exports = all;
                    binding.prv_signatures = old.prv_signatures.clone();
                }
                "*SRCFILE" => {
                    let (libs, file) = self.search(cmd.get("SRCFILE"));
                    let member = match cmd.get("SRCMBR") {
                        "*SRVPGM" => name.clone(),
                        m => m.to_string(),
                    };
                    let src =
                        crate::source::find_member(self.client, &libs, &file, &member).await?;
                    let bs = crate::ile::binder_language(&src.source)?;
                    for e in &bs.exports {
                        if !all.contains(e) {
                            bail!("CPD5D03: Symbol {e} named for export is not exported by any of the modules.");
                        }
                    }
                    binding.signature = bs
                        .signature
                        .unwrap_or_else(|| crate::ile::generated_signature(&bs.exports));
                    binding.exports = bs.exports;
                    binding.prv_signatures = bs.previous;
                }
                _ => {
                    for e in &old.exports {
                        if !all.contains(e) {
                            bail!("CPD5D03: Symbol {e} is exported by {name} but no module defines it any more.");
                        }
                    }
                    binding.exports = old.exports.clone();
                    binding.signature = old.signature.clone();
                    binding.prv_signatures = old.prv_signatures.clone();
                }
            }
            binding.actgrp = old.actgrp.clone();
            let json = serde_json::to_string(&binding)?;
            client_update_srvpgm(self.client, &lib, &name, &json).await?;
        } else {
            let entry = binding.modules.remove(0);
            binding.entry_module = entry.name.clone();
            let mut c = runtime::connect_db(&self.env.database_url).await?;
            let p = catalog::find_program(self.client, std::slice::from_ref(&lib), &name)
                .await?
                .unwrap();
            catalog::create_program(&mut c, &lib, &name, &p.attribute, "", &entry.source, true)
                .await?;
            catalog::set_program_options(self.client, &lib, &name, &options).await?;
            crate::ile::set_program_binding(self.client, &lib, &name, Some(&binding)).await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "{} {name} in library {lib} updated{}.",
            if srv { "Service program" } else { "Program" },
            if updated.is_empty() {
                String::new()
            } else {
                format!(" (modules {})", updated.join(", "))
            }
        )]))
    }

    fn bind_lists(cmd: &Command) -> (Vec<(String, String)>, Vec<String>) {
        let srv = cmd
            .list("BNDSRVPGM")
            .into_iter()
            .filter(|x| x != "*NONE")
            // An element is (service program activation), e.g.
            // ((MYLIB/UTIL *IMMED)): the activation is not kept.
            .map(|x| {
                x.trim_start_matches('(')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(')')
                    .to_string()
            })
            .map(|x| match x.split_once('/') {
                Some((l, n)) => (l.to_ascii_uppercase(), n.to_ascii_uppercase()),
                None => ("*LIBL".to_string(), x.to_ascii_uppercase()),
            })
            .collect();
        let dirs = cmd
            .list("BNDDIR")
            .into_iter()
            .filter(|x| x != "*NONE")
            .collect();
        (srv, dirs)
    }

    /// CRTRPGMOD: a module from a source member, checked as the compiler
    /// does.
    async fn crtrpgmod(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("MODULE"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let m = self.source_member(cmd, &name).await?;
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        let cl = cmd.def.name == "CRTCLMOD";
        if cl {
            crate::clpgm::compile_in(self.client, &libl, &name, &m.source).await?;
        } else {
            crate::rpg::compile(self.client, &libl, &name, &m.source).await?;
        }
        if cmd.get("REPLACE") == "*NO"
            && catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*MODULE")
                .await?
        {
            bail!("CPF5D0B: Module {name} in library {lib} already exists (REPLACE(*NO)).");
        }
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        crate::ile::store(
            &mut c,
            &lib,
            &name,
            "*MODULE",
            if cl { "CLLE" } else { "RPGLE" },
            &text,
            &m.source,
            "",
        )
        .await?;
        authority::set_owner(self.client, &lib, &name, "*MODULE", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Module {name} created in library {lib}."
        )]))
    }

    /// CRTSRVPGM: modules bound into a service program; EXPORT(*ALL) exports
    /// every exported procedure, EXPORT(*SRCFILE) the binder language's.
    async fn crtsrvpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("SRVPGM"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let modules = self.module_list(cmd, &lib, &name).await?;
        let (srv, dirs) = Self::bind_lists(cmd);
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        let not_created = |e: anyhow::Error| {
            anyhow::anyhow!("CPF5D11: Service program {name} not created. {e:#}")
        };
        let (mut binding, infos) = crate::ile::bind(self.client, &libl, modules, &srv, &dirs)
            .await
            .map_err(not_created)?;
        let all: Vec<String> = infos.iter().flat_map(|i| i.exports.clone()).collect();
        let src = if cmd.get("EXPORT") == "*ALL" {
            crate::ile::BinderSource {
                exports: all.clone(),
                ..Default::default()
            }
        } else {
            let (libs, file) = self.search(cmd.get("SRCFILE"));
            let member = match cmd.get("SRCMBR") {
                "*SRVPGM" => name.clone(),
                m => m.to_string(),
            };
            let src = crate::source::find_member(self.client, &libs, &file, &member).await?;
            crate::ile::binder_language(&src.source).map_err(not_created)?
        };
        let crate::ile::BinderSource {
            exports,
            signature,
            previous,
        } = src;
        binding.prv_signatures = previous;
        binding.actgrp = cmd.get("ACTGRP").to_ascii_uppercase();
        for e in &exports {
            if !all.contains(e) {
                return Err(not_created(anyhow::anyhow!(
                    "CPD5D03: Symbol {e} named for export is not exported by any of the modules."
                )));
            }
        }
        binding.signature = signature.unwrap_or_else(|| crate::ile::generated_signature(&exports));
        binding.exports = exports;
        if cmd.get("REPLACE") == "*NO"
            && catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*SRVPGM")
                .await?
        {
            bail!(
                "CPF5D0B: Service program {name} in library {lib} already exists (REPLACE(*NO))."
            );
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let json = serde_json::to_string(&binding)?;
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        crate::ile::store(&mut c, &lib, &name, "*SRVPGM", "RPGLE", &text, &json, "").await?;
        authority::set_owner(self.client, &lib, &name, "*SRVPGM", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Service program {name} created in library {lib}."
        )]))
    }

    /// CRTPGM: a program from modules; its entry module (ENTMOD, or the
    /// first with a main procedure) is what CALL runs.
    async fn crtpgm_bound(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("PGM"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let mut modules = self.module_list(cmd, &lib, &name).await?;
        let (srv, dirs) = Self::bind_lists(cmd);
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        let not_created =
            |e: anyhow::Error| anyhow::anyhow!("CPF5D04: Program {name} not created. {e:#}");
        // The entry module first.
        let mut infos = Vec::new();
        for m in &modules {
            infos.push(
                crate::ile::module_info(self.client, &libl, m)
                    .await
                    .map_err(not_created)?,
            );
        }
        let entry = match cmd.get("ENTMOD") {
            "*FIRST" => infos.iter().position(|i| i.has_main),
            e => {
                let e = e.rsplit('/').next().unwrap_or(e).to_ascii_uppercase();
                infos.iter().position(|i| i.name == e)
            }
        };
        let Some(entry) = entry.filter(|k| infos[*k].has_main) else {
            return Err(not_created(anyhow::anyhow!(
                "CPD5D11: No module has a program entry procedure (all are NOMAIN)."
            )));
        };
        let first = modules.remove(entry);
        modules.insert(0, first);
        let (mut binding, _) = crate::ile::bind(self.client, &libl, modules.clone(), &srv, &dirs)
            .await
            .map_err(not_created)?;
        let entry = modules[0].clone();
        binding.modules.retain(|m| m.name != entry.name);
        binding.entry_module = entry.name.clone();
        if cmd.get("REPLACE") == "*NO"
            && catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*PGM")
                .await?
        {
            bail!("CPF5D0B: Program {name} in library {lib} already exists (REPLACE(*NO)).");
        }
        let text = match cmd.get("TEXT") {
            "*ENTMODTXT" | "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        // A CL entry module makes a CL program.
        let attr = if entry.attribute == "CLLE" {
            "CLLE"
        } else {
            "RPGLE"
        };
        catalog::create_program(&mut c, &lib, &name, attr, &text, &entry.source, true).await?;
        // ACTGRP(*ENTMOD): QILE for an RPG entry module (ILE Concepts,
        // CRTPGM).
        let actgrp = match cmd.get("ACTGRP").to_ascii_uppercase().as_str() {
            "*ENTMOD" => "QILE".to_string(),
            g => g.to_string(),
        };
        let options = format!("{} DFTACTGRP(*NO) ACTGRP({actgrp})", entry.options)
            .trim()
            .to_string();
        catalog::set_program_options(self.client, &lib, &name, &options).await?;
        crate::ile::set_program_binding(self.client, &lib, &name, Some(&binding)).await?;
        authority::set_owner(self.client, &lib, &name, "*PGM", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Program {name} was created in library {lib}."
        )]))
    }

    async fn crtbnddir(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("BNDDIR"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        crate::ile::create_bnddir(self.client, &lib, &name, cmd.get("TEXT")).await?;
        authority::set_owner(self.client, &lib, &name, "*BNDDIR", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Binding directory {name} created in library {lib}."
        )]))
    }

    /// The binding directory a command names (found through the library
    /// list), after checking the user may change it.
    async fn bnddir_of(&self, q: &str, need: Aut) -> Result<(String, String)> {
        let (libs, name) = self.search(q);
        let name = name.to_ascii_uppercase();
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, "*BNDDIR")
                .await?
            {
                authority::check(self.client, &self.user, l, &name, "*BNDDIR", need).await?;
                return Ok((l.clone(), name));
            }
        }
        bail!("CPF5D07: Binding directory {name} not found.")
    }

    /// ADDBNDDIRE and RMVBNDDIRE: OBJ((LIB/NAME *SRVPGM) (NAME *MODULE) ...).
    async fn bnddire(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, dir) = self.bnddir_of(cmd.get("BNDDIR"), Aut::Change).await?;
        let raw = cmd.get("OBJ");
        let groups: Vec<Vec<String>> = if raw.trim_start().starts_with('(') {
            raw.split(')')
                .map(|g| g.trim().trim_start_matches('(').trim())
                .filter(|g| !g.is_empty())
                .map(|g| {
                    g.split_whitespace()
                        .map(|w| w.to_ascii_uppercase())
                        .collect()
                })
                .collect()
        } else {
            vec![raw
                .split_whitespace()
                .map(|w| w.to_ascii_uppercase())
                .collect()]
        };
        let mut entries = Vec::new();
        for g in groups {
            let obj = g.first().cloned().unwrap_or_default();
            let t = g.get(1).cloned().unwrap_or_else(|| "*SRVPGM".into());
            if cmd.def.name == "ADDBNDDIRE" && !matches!(t.as_str(), "*SRVPGM" | "*MODULE") {
                bail!("CPF5D10: Object type {t} is not valid in a binding directory (*SRVPGM or *MODULE).");
            }
            entries.push((obj, t));
        }
        if cmd.def.name == "ADDBNDDIRE" {
            let rows: Vec<(String, String, String)> = entries
                .iter()
                .map(|(o, t)| match o.split_once('/') {
                    Some((l, n)) => (l.to_string(), n.to_string(), t.clone()),
                    None => ("*LIBL".to_string(), o.clone(), t.clone()),
                })
                .collect();
            for (l, n, _) in &rows {
                catalog::validate_name("object", n)?;
                if l != "*LIBL" {
                    catalog::validate_name("library", l)?;
                }
            }
            let n = crate::ile::add_bnddir_entries(self.client, &lib, &dir, &rows).await?;
            Ok(Outcome::Messages(vec![format!(
                "{n} entries added to binding directory {dir} in {lib}."
            )]))
        } else {
            let n = crate::ile::remove_bnddir_entries(self.client, &lib, &dir, &entries).await?;
            if n == 0 {
                bail!("CPF5D08: Object not found in binding directory {dir}.");
            }
            Ok(Outcome::Messages(vec![format!(
                "{n} entries removed from binding directory {dir} in {lib}."
            )]))
        }
    }

    async fn dspbnddir(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, dir) = self.bnddir_of(cmd.get("BNDDIR"), Aut::Use).await?;
        let rows = crate::ile::bnddir_entries(
            self.client,
            std::slice::from_ref(&lib),
            &format!("{lib}/{dir}"),
        )
        .await?;
        Ok(Outcome::Display(ListDisplay {
            title: "Display Binding Directory".into(),
            info: vec![
                ("Binding directory".into(), dir.clone()),
                ("Library".into(), lib.clone()),
            ],
            heading: format!("{:<10}  {:<10}  {:<8}", "Object", "Library", "Type"),
            rows: rows
                .iter()
                .map(|(l, n, t)| format!("{n:<10}  {l:<10}  {t:<8}"))
                .collect(),
            ..Default::default()
        }))
    }

    /// DLTMOD, DLTSRVPGM, DLTBNDDIR.
    async fn dlt_ile(&mut self, cmd: &Command) -> Result<Outcome> {
        let (kw, objtype, what) = match cmd.def.name {
            "DLTPGM" => ("PGM", "*PGM", "Program"),
            "DLTMOD" => ("MODULE", "*MODULE", "Module"),
            "DLTSRVPGM" => ("SRVPGM", "*SRVPGM", "Service program"),
            _ => ("BNDDIR", "*BNDDIR", "Binding directory"),
        };
        let (libs, name) = self.search(cmd.get(kw));
        let name = name.to_ascii_uppercase();
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, objtype).await? {
                if authority::effective(self.client, &self.user, l, &name, objtype).await?
                    < Aut::All
                {
                    bail!("CPF2189: Not authorized to delete {name} in {l} type {objtype}.");
                }
                crate::ile::delete_object(self.client, l, &name, objtype).await?;
                // CPC2191; not found, CPF2105 (observed on IBM i 7.5).
                let _ = what;
                return Ok(Outcome::Messages(vec![format!(
                    "CPC2191: Object {name} of type {objtype} in library {l} was deleted."
                )]));
            }
        }
        let label = cmd.get(kw).split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        bail!("CPF2105: Object {name} of type {objtype} in library {label} not found.")
    }

    /// DLTF: delete a file (not a physical file that logical files are
    /// built over: CPF3219, observed on IBM i 7.5).
    async fn dltf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("FILE"));
        let name = name.to_ascii_uppercase();
        let mut found = None;
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, "*FILE").await? {
                found = Some(l.clone());
                break;
            }
        }
        let Some(lib) = found else {
            bail!(
                "CPF2105: File {name} in library {} not found.",
                libs.join(" ")
            );
        };
        if authority::effective(self.client, &self.user, &lib, &name, "*FILE").await? < Aut::All {
            bail!("CPF2189: Not authorized to delete file {name} in library {lib}.");
        }
        if crate::source::is_source_file(self.client, &lib, &name).await? {
            bail!("CPF2105: File {name} in library {lib} is a source file; deleting source files is not supported yet.");
        }
        if crate::save::is_save_file(self.client, &lib, &name).await? {
            crate::save::delete_save_file(self.client, &lib, &name).await?;
            return Ok(Outcome::Messages(vec![format!(
                "CPC2191: Object {name} of type *FILE in library {lib} was deleted."
            )]));
        }
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        catalog::delete_file(&mut c, &lib, &name, false).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2191: Object {name} of type *FILE in library {lib} was deleted."
        )]))
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
                    bail!("CPF2110: Library {lib} does not exist.");
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
                inlpgm: cmd.get("INLPGM").to_string(),
                inlmnu: cmd.get("INLMNU").to_string(),
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
                ("Initial program to call".into(), p.inlpgm),
                ("Initial menu".into(), p.inlmnu),
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

    async fn chgusrprf(&mut self, cmd: &Command) -> Result<Outcome> {
        self.require_secadm("change user profiles").await?;
        let name = cmd.get("USRPRF").to_string();
        let same = |k: &str| match cmd.get(k) {
            "*SAME" => None,
            v => Some(v.to_string()),
        };
        let curlib = match same("CURLIB") {
            Some(lib) if lib != "*CRTDFT" => {
                if !catalog::library_exists(self.client, &lib).await? {
                    bail!("CPF2110: Library {lib} does not exist.");
                }
                Some(lib)
            }
            other => other,
        };
        let changes = users::ProfileChanges {
            expired: same("PWDEXP").map(|v| v == "*YES"),
            enabled: same("STATUS").map(|v| v != "*DISABLED"),
            curlib,
            text: same("TEXT"),
            inlpgm: same("INLPGM"),
            inlmnu: same("INLMNU"),
        };
        if !users::change(self.client, &name, &changes).await? {
            bail!("CPF2204: User profile {name} not found.");
        }
        Ok(Outcome::Messages(vec![format!(
            "User profile {name} changed."
        )]))
    }

    /// A qualified name to create: LIB/NAME, *CURLIB/NAME or NAME (the
    /// current library).
    pub(crate) fn create_target(&self, q: &str) -> (String, String) {
        match q.split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (self.current_library(), q.to_string()),
        }
    }

    /// The libraries a qualified name is looked for in, and the name.
    pub(crate) fn search(&self, q: &str) -> (Vec<String>, String) {
        match q.split_once('/') {
            Some(("*LIBL", n)) => (self.library_list(), n.to_string()),
            Some(("*CURLIB", n)) => (vec![self.current_library()], n.to_string()),
            Some((l, n)) => (vec![l.to_string()], n.to_string()),
            None => (self.library_list(), q.to_string()),
        }
    }

    /// SRCFILE and SRCMBR of a create command (SRCMBR(*FILE), (*PGM): the
    /// member named like the object).
    async fn source_member(&self, cmd: &Command, object: &str) -> Result<crate::source::Member> {
        // SRCSTMF: the source in a stream file instead of a member (the
        // compile commands; SRCFILE and SRCMBR are then not used).
        let stmf = cmd.get("SRCSTMF");
        if !stmf.is_empty() && cmd.def.params.iter().any(|p| p.keyword == "SRCSTMF") {
            let (bytes, ccsid) = crate::ifs::read_file(self.client, stmf)
                .await
                .map_err(|e| anyhow!("CPFA0A9: Stream file {stmf} cannot be read: {e}"))?;
            let text = crate::ifs::codec::text(ccsid, &bytes);
            let text = text.trim_start_matches('\u{FEFF}').replace("\r\n", "\n");
            let ext = stmf.rsplit('.').next().unwrap_or("").to_ascii_uppercase();
            return Ok(crate::source::Member {
                library: String::new(),
                file: String::new(),
                name: object.to_string(),
                srctype: ext,
                text: String::new(),
                source: text,
            });
        }
        let (libs, file) = self.search(cmd.get("SRCFILE"));
        let member = match cmd.get("SRCMBR") {
            "*FILE" | "*PGM" | "*OBJ" | "*MODULE" | "*CMD" => object.to_string(),
            m => m.to_string(),
        };
        let m = crate::source::find_member(self.client, &libs, &file, &member).await?;
        authority::check(
            self.client,
            &self.user,
            &m.library,
            &m.file,
            "*FILE",
            Aut::Use,
        )
        .await?;
        Ok(m)
    }

    /// CHGPFM: a member's source type and text (source files; other
    /// attributes are accepted).
    async fn chgpfm(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("FILE").to_ascii_uppercase();
        let (lib, file) = match q.split_once('/') {
            Some((l, f)) => (l.to_string(), f.to_string()),
            None => ("*LIBL".to_string(), q.clone()),
        };
        let lib = match lib.as_str() {
            "*LIBL" => {
                let mut found = None;
                for l in self.library_list() {
                    if crate::source::is_source_file(self.client, &l, &file).await?
                        || catalog::object_exists(
                            self.client,
                            std::slice::from_ref(&l),
                            &file,
                            "*FILE",
                        )
                        .await?
                    {
                        found = Some(l);
                        break;
                    }
                }
                found.ok_or_else(|| {
                    anyhow::anyhow!("CPF9812: File {file} in library *LIBL not found.")
                })?
            }
            "*CURLIB" => self.current_library(),
            l => l.to_string(),
        };
        let mbr = cmd.get("MBR").to_ascii_uppercase();
        if crate::source::is_source_file(self.client, &lib, &file).await? {
            let n = self
                .client
                .execute(
                    "UPDATE as4tainr.source_members
                        SET srctype = CASE WHEN $4 = '*SAME' THEN srctype WHEN $4 = '*NONE' THEN '' ELSE $4 END,
                            text = CASE WHEN $5 = '*SAME' THEN text ELSE $5 END
                      WHERE library = $1 AND file = $2 AND member = $3",
                    &[&lib, &file, &mbr, &cmd.get("SRCTYPE").to_ascii_uppercase(), &cmd.get("TEXT")],
                )
                .await?;
            if n == 0 {
                bail!("CPF3141: Member {mbr} not found in file {lib}/{file}.");
            }
        } else if !catalog::object_exists(self.client, std::slice::from_ref(&lib), &file, "*FILE")
            .await?
        {
            bail!("CPF9812: File {file} in library {lib} not found.");
        }
        Ok(Outcome::Messages(vec![format!(
            "CPC3201: Member {mbr} of file {file} in library {lib} was changed."
        )]))
    }

    async fn crtsrcpf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        number(cmd.get("RCDLEN"), "RCDLEN")?;
        crate::source::create_file(self.client, &lib, &name, cmd.get("TEXT")).await?;
        let name = name.to_ascii_uppercase();
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC7301: File {name} was created in library {lib}."
        )]))
    }

    /// DSPFD TYPE(*ATR) of a database physical file: its printed report
    /// (layout from PUB400, IBM i 7.5, `system "DSPFD
    /// FILE(AS4TRAINR1/GOLDTEST) TYPE(*ATR)"` of a CRTPF RCDLEN(80) file,
    /// 2026-10-21). Attributes this system does not keep show CRTPF's
    /// shipped defaults (SIZE, ALLOCATE, WAITFILE ...; fidelity-gaps.md).
    pub(crate) async fn dspfd_atr(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("FILE"));
        let name = name.to_ascii_uppercase();
        let mut found = None;
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, "*FILE").await? {
                found = Some(l.clone());
                break;
            }
        }
        let Some(lib) = found else {
            bail!(
                "CPF9812: File {name} in library {} not found.",
                cmd.get("FILE")
                    .split_once('/')
                    .map(|x| x.0)
                    .unwrap_or("*LIBL")
            );
        };
        authority::check(self.client, &self.user, &lib, &name, "*FILE", Aut::Use).await?;
        let row = self
            .client
            .query_one(
                "SELECT attribute, text, extract(year FROM created)::int,
                        to_char(created AT TIME ZONE 'UTC', 'YYMMDDHH24MISS'),
                        extract(month FROM created)::int, extract(day FROM created)::int
                   FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                &[&lib, &name],
            )
            .await?;
        let attr: String = row.get(0);
        if attr != "PF" {
            bail!(
                "CPF9898: DSPFD TYPE(*ATR) prints physical files only for now; {name} is {attr}."
            );
        }
        let text: String = row.get(1);
        let year: i32 = row.get(2);
        let level_id = format!(
            "{}{}",
            if year >= 2000 { 1 } else { 0 },
            row.get::<_, String>(3)
        );
        let created = crate::report::Stamp::of(
            (
                year,
                row.get::<_, i32>(4) as u32,
                row.get::<_, i32>(5) as u32,
            ),
            (0, 0, 0),
        )
        .date;
        let fr = catalog::resolve_file(self.client, std::slice::from_ref(&lib), &name).await?;
        let pf = &fr.file;
        // CRTPF RCDLEN(n): one field and format named like the file.
        let external = !(pf.fields.len() == 1 && pf.fields[0].name == name && pf.format == name);
        let reclen: u32 = pf.fields.iter().map(|f| f.buffer_bytes()).sum();
        let mut counts = Vec::new();
        for sql in [
            "SELECT count(*) FROM as4tainr.file_triggers WHERE library=$1 AND file=$2",
            "SELECT count(*) FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
        ] {
            counts.push(
                self.client
                    .query_one(sql, &[&lib, &name])
                    .await
                    .map(|r| r.get::<_, i64>(0))
                    .unwrap_or(0),
            );
        }
        let (triggers, journaled) = (counts[0], counts[1] > 0);
        let yn = |b: bool| if b { "Yes" } else { "No" };
        // "label . . . : KEYWORD", value from column 61 (numbers right
        // aligned to column 65, record counts to column 70).
        let kv = |label: &str, kw: &str, value: &str| {
            let head = if kw.is_empty() {
                label.to_string()
            } else {
                format!("{label} {kw}")
            };
            format!("{head:<60}{value}").trim_end().to_string()
        };
        let num = |label: &str, kw: &str, n: i64, end: usize| {
            let head = if kw.is_empty() {
                label.to_string()
            } else {
                format!("{label} {kw}")
            };
            format!("{head:<60}{:>w$}", n, w = end - 60)
        };
        let stamp = crate::report::Stamp::now();
        let header = |page: u32| {
            let when = format!("{}  {}", stamp.date, stamp.time);
            crate::report::at(&[
                (2, crate::report::PRODUCT),
                (18, crate::report::LEVEL),
                (39, "Display File Description"),
                (97, &when),
                (122, &format!("Page {page:>4}")),
            ])
        };
        let file_line = format!(
            " File . : {name:<10}   Library . : {lib:<10}   Type of file . : {:<26}Auxiliary Storage Pool ID . : 00001",
            "Physical *DATA"
        );
        let attrs = "                                    Data Base File Attributes".to_string();
        let mut print = vec![
            header(1),
            format!("   File  . . . . . . . . . . . : {name}"),
            format!("     Library . . . . . . . . . : {lib}"),
            "   Type of information . . . . : *ATR".into(),
            "   File attributes . . . . . . : *ALL".into(),
            "   System  . . . . . . . . . . : *LCL".into(),
            "   Processor . . . . . . . . . : AS4TAINR Display File Description Processor".into(),
            file_line.clone(),
            attrs.clone(),
            kv(
                "   Externally described file . . . . . . . . . :",
                "",
                yn(external),
            ),
            kv(
                "   File level identifier . . . . . . . . . . . :",
                "",
                &level_id,
            ),
            kv(
                "   Creation date . . . . . . . . . . . . . . . :",
                "",
                &created,
            ),
            kv(
                "   Text 'description'  . . . . . . . . . . . . :",
                "TEXT",
                &text,
            ),
            kv("   Distributed file  . . . . . . . . . . . . . :", "", "No"),
            kv("   Partitioned SQL Table . . . . . . . . . . . :", "", "No"),
            kv("   DBCS capable  . . . . . . . . . . . . . . . :", "", "No"),
            num(
                "   Maximum members . . . . . . . . . . . . . . :",
                "MAXMBRS",
                1,
                65,
            ),
            num(
                "   Number of constraints . . . . . . . . . . . :",
                "",
                if pf.unique { 1 } else { 0 },
                65,
            ),
            num(
                "   Number of triggers  . . . . . . . . . . . . :",
                "",
                triggers,
                65,
            ),
            num(
                "   Number of members . . . . . . . . . . . . . :",
                "",
                1,
                65,
            ),
            "   Member size                                   SIZE".into(),
            num(
                "     Initial number of records . . . . . . . . :",
                "",
                10000,
                70,
            ),
            num(
                "     Increment number of records . . . . . . . :",
                "",
                1000,
                65,
            ),
            num(
                "     Maximum number of increments  . . . . . . :",
                "",
                3,
                65,
            ),
            num(
                "   Record capacity . . . . . . . . . . . . . . :",
                "",
                13000,
                70,
            ),
            kv(
                "   Allocate storage  . . . . . . . . . . . . . :",
                "ALLOCATE",
                "*NO",
            ),
            kv(
                "   Contiguous storage  . . . . . . . . . . . . :",
                "CONTIG",
                "*NO",
            ),
            kv(
                "   Preferred storage unit  . . . . . . . . . . :",
                "UNIT",
                "*ANY",
            ),
            kv(
                "   Records to force a write  . . . . . . . . . :",
                "FRCRATIO",
                "*NONE",
            ),
            kv(
                "   Maximum file wait time  . . . . . . . . . . :",
                "WAITFILE",
                "*IMMED",
            ),
            num(
                "   Maximum record wait time  . . . . . . . . . :",
                "WAITRCD",
                60,
                65,
            ),
            kv(
                "   Max % deleted records allowed . . . . . . . :",
                "DLTPCT",
                "*NONE",
            ),
            kv(
                "   Reuse deleted records . . . . . . . . . . . :",
                "REUSEDLT",
                "*NO",
            ),
            header(2),
            file_line,
            attrs,
            kv(
                "   Sort Sequence . . . . . . . . . . . . . . . :",
                "SRTSEQ",
                "*HEX",
            ),
            kv(
                "   Language identifier . . . . . . . . . . . . :",
                "LANGID",
                "ENU",
            ),
        ];
        let ccsid = crate::clpgm::system_value_text("QCCSID")
            .map(|v| match v {
                Ok(s) => s,
                Err(n) => n.to_string(),
            })
            .unwrap_or_else(|| "65535".into());
        print.extend([
            kv(
                "   Coded character set identifier  . . . . . . :",
                "CCSID",
                &ccsid,
            ),
            kv(
                "   Allow read operation  . . . . . . . . . . . :",
                "",
                "Yes",
            ),
            kv(
                "   Allow write operation . . . . . . . . . . . :",
                "",
                "Yes",
            ),
            kv(
                "   Allow update operation  . . . . . . . . . . :",
                "ALWUPD",
                "*YES",
            ),
            kv(
                "   Allow delete operation  . . . . . . . . . . :",
                "ALWDLT",
                "*YES",
            ),
            kv("   Restrict on drop  . . . . . . . . . . . . . :", "", "No"),
            kv(
                "   Record format level check . . . . . . . . . :",
                "LVLCHK",
                "*YES",
            ),
            kv(
                "   Access path . . . . . . . . . . . . . . . . :",
                "",
                if pf.keys.is_empty() {
                    "Arrival"
                } else {
                    "Keyed"
                },
            ),
            num(
                "   Maximum record length . . . . . . . . . . . :",
                "",
                reclen as i64,
                65,
            ),
            kv("   Volatile  . . . . . . . . . . . . . . . . . :", "", "No"),
            kv(
                "   Keep in memory  . . . . . . . . . . . . . . :",
                "KEEPINMEM",
                "*NO",
            ),
            kv(
                "   File is currently journaled . . . . . . . . :",
                "",
                yn(journaled),
            ),
        ]);
        Ok(Outcome::Display(ListDisplay {
            title: "Display File Description".into(),
            info: vec![
                ("File".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Type of information".into(), "*ATR".into()),
            ],
            rows: print[1..].to_vec(),
            print,
            ..Default::default()
        }))
    }

    /// CRTPRTF: a printer file, described by DDS or not (SRCFILE(*NONE):
    /// program-described), with its page size and overflow line.
    async fn crtprtf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let (source, mbr_text) = if cmd.get("SRCFILE") == "*NONE" {
            (String::new(), String::new())
        } else {
            let m = self.source_member(cmd, &name).await?;
            (m.source, m.text)
        };
        let size: Vec<String> = cmd
            .list("PAGESIZE")
            .iter()
            .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .collect();
        let len: i32 = number(size.first().map(String::as_str).unwrap_or("66"), "PAGESIZE")? as i32;
        let width: i32 =
            number(size.get(1).map(String::as_str).unwrap_or("132"), "PAGESIZE")? as i32;
        let ovf: i32 = number(cmd.get("OVRFLW"), "OVRFLW")? as i32;
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => mbr_text,
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        crate::prtf::create(
            self.client,
            &lib,
            &name,
            &source,
            (len, width, ovf),
            &text,
            cmd.get("REPLACE") != "*NO",
        )
        .await
        .map_err(|e| {
            if crate::jobs::escape_id(&e.to_string()) == "CPF7302" {
                e
            } else {
                anyhow!("{e:#}\nCPF7302: File {name} was not created in library {lib}.")
            }
        })?;
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Printer file {name} was created in library {lib}."
        )]))
    }

    /// CRTPF, CRTLF, CRTDSPF from a source member.
    async fn crtfile(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        let lib = lib.to_ascii_uppercase();
        let name = name.to_ascii_uppercase();
        // CRTPF RCDLEN(n): a program-described file, without DDS: one
        // record format and one character field of that length, both
        // named for the file (CL reference, CRTPF RCDLEN).
        let rcdlen = cmd.def.name == "CRTPF" && !matches!(cmd.get("RCDLEN"), "" | "*NONE");
        let m = if rcdlen {
            let len: u32 = cmd
                .get("RCDLEN")
                .parse()
                .ok()
                .filter(|n| (1..=32766).contains(n))
                .ok_or_else(|| {
                    anyhow!(
                        "CPD0084: RCDLEN({}) must be a record length from 1 to 32766.\n{}",
                        cmd.get("RCDLEN"),
                        crate::cl::CPF0006
                    )
                })?;
            crate::source::Member {
                library: lib.clone(),
                file: String::new(),
                name: name.clone(),
                srctype: "PF".into(),
                text: String::new(),
                source: format!(
                    "{:5}A{:10}R {name:<10}\n{:5}A{:12}{name:<10} {len:>5}A\n",
                    "", "", "", ""
                ),
            }
        } else {
            // No source member: CPF9815, then the file is not created,
            // CPF7302 (observed on IBM i 7.5).
            self.source_member(cmd, &name).await.map_err(|e| {
                if crate::jobs::escape_id(&e.to_string()) == "CPF9815" {
                    anyhow!("{e:#}\nCPF7302: File {name} was not created in library {lib}.")
                } else {
                    e
                }
            })?
        };
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        let msg = match cmd.def.name {
            "CRTPF" => {
                let mut pf = crate::dds::parse_pf(&name, &m.source)?;
                match cmd.get("TEXT") {
                    "" | "*SRCMBRTXT" => {}
                    "*BLANK" => pf.text = String::new(),
                    t => pf.text = t.to_string(),
                }
                catalog::create_physical_file(&mut c, &lib, &pf).await?;
                if rcdlen {
                    self.client
                        .execute(
                            "UPDATE as4tainr.files SET described = false WHERE library=$1 AND name=$2",
                            &[&lib, &name],
                        )
                        .await?;
                }
                // A physical file gets its member (MBR(*FILE)), named for
                // the file: CPC7301 then CPC7305 (observed on IBM i 7.5).
                return {
                    authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
                    Ok(Outcome::Messages(vec![
                        format!("CPC7301: File {name} was created in library {lib}."),
                        format!(
                            "CPC7305: Member {name} was added to file {name} in library {lib}."
                        ),
                    ]))
                };
            }
            "CRTLF" => {
                let lf = crate::dds::parse_lf(&name, &m.source)?;
                catalog::create_logical_file(&mut c, &self.library_list(), &lib, &lf).await?;
                // CPC7301 and CPC7305, as for a physical file (observed on
                // IBM i 7.5).
                authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
                return Ok(Outcome::Messages(vec![
                    format!(
                        "CPC7301: File {name} was created in library {lib} over {}.",
                        lf.based_on
                    ),
                    format!("CPC7305: Member {name} was added to file {name} in library {lib}."),
                ]));
            }
            _ => {
                if cmd.get("REPLACE") == "*NO" {
                    catalog::create_display_file(&mut c, &lib, &name, &m.source).await?;
                } else {
                    catalog::replace_display_file(&mut c, &lib, &name, &m.source).await?;
                }
                format!("Display file {name} created in library {lib}.")
            }
        };
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![msg]))
    }

    /// CRTBNDCL, CRTCLPGM, CRTBNDRPG, CRTSQLRPGI from a source member: the
    /// program is checked (compiled) first.
    async fn crtpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let sql = cmd.def.name == "CRTSQLRPGI";
        if sql && cmd.get("OBJTYPE") == "*MODULE" {
            // A module with embedded SQL, for CRTPGM or CRTSRVPGM.
            let (lib, name) = self.create_target(cmd.get("OBJ"));
            let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
            let m = self.source_member(cmd, &name).await?;
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            crate::rpg::compile(self.client, &libl, &name, &m.source).await?;
            let text = match cmd.get("TEXT") {
                "*SRCMBRTXT" => m.text.clone(),
                "*BLANK" => String::new(),
                t => t.to_string(),
            };
            let mut c = runtime::connect_db(&self.env.database_url).await?;
            crate::ile::store(
                &mut c,
                &lib,
                &name,
                "*MODULE",
                "SQLRPGLE",
                &text,
                &m.source,
                &format!("SQLCOMMIT({})", cmd.get("COMMIT")),
            )
            .await?;
            authority::set_owner(self.client, &lib, &name, "*MODULE", &self.user).await?;
            return Ok(Outcome::Messages(vec![format!(
                "Module {name} created in library {lib}."
            )]));
        }
        if sql && cmd.get("OBJTYPE") != "*PGM" {
            bail!(
                "CRTSQLRPGI OBJTYPE({}): service programs are created from modules with CRTSRVPGM.",
                cmd.get("OBJTYPE")
            );
        }
        let (lib, name) = self.create_target(cmd.get(if sql { "OBJ" } else { "PGM" }));
        let lib = lib.to_ascii_uppercase();
        let name = name.to_ascii_uppercase();
        let m = self.source_member(cmd, &name).await?;
        let attribute = match cmd.def.name {
            "CRTBNDCL" => "CLLE",
            "CRTCLPGM" => "CLP",
            "CRTSQLRPGI" => "SQLRPGLE",
            "CRTRPGPGM" => "RPG",
            _ => "RPGLE",
        };
        if attribute == "RPGLE" && m.srctype == "RPG" {
            bail!(
                "Member {} is RPG/400 (RPG III) source: create it with CRTRPGPGM, or convert it to RPG IV with CVTRPGSRC.",
                m.name
            );
        }
        // RPG/400: converted to RPG IV as CVTRPGSRC does, with its /COPY
        // members; the program keeps the converted source.
        let mut m = m;
        if attribute == "RPG" {
            if m.srctype != "RPG" && m.srctype != "RPG38" {
                bail!(
                    "Member {} has source type {}; CRTRPGPGM creates programs from RPG source.",
                    m.name,
                    m.srctype
                );
            }
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            let c = crate::rpg::convert_rpg3(self.client, &libl, &m.source, true).await?;
            m.source = c.source;
        }
        let mut binding: Option<crate::ile::Binding> = None;
        // Compile options from the command (CRTBNDRPG ALWNULL).
        let mut options = match cmd.def.name {
            "CRTBNDRPG" if cmd.get("ALWNULL") != "*NO" => {
                format!("ALWNULL({})", cmd.get("ALWNULL"))
            }
            // CRTSQLRPGI COMMIT: the commit level of its SQL statements
            // (SET OPTION COMMIT in the source wins).
            "CRTSQLRPGI" => format!("SQLCOMMIT({})", cmd.get("COMMIT")),
            _ => String::new(),
        };
        // CRTBNDRPG DFTACTGRP(*NO) ACTGRP(...): the activation group the
        // program runs in (the source's control keywords win).
        if cmd.def.name == "CRTBNDRPG" && cmd.get("DFTACTGRP") == "*NO" {
            options = format!("{options} DFTACTGRP(*NO) ACTGRP({})", cmd.get("ACTGRP"))
                .trim()
                .to_string();
        }
        // CL: includes are part of the program; then it is checked.
        let source = if attribute == "RPGLE" || attribute == "SQLRPGLE" || attribute == "RPG" {
            // The program is checked now, as the compiler does: syntax,
            // names, types, files (docs/rpg-engine.md).
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            let prog =
                crate::rpg::compile_with(self.client, &libl, &name, &m.source, &options).await?;
            // Procedures the program calls that are elsewhere: bound from
            // the binding directories (BNDDIR, ctl-opt BNDDIR).
            if !prog.imports.is_empty() || !prog.data_imports.is_empty() {
                let mut dirs: Vec<String> = cmd
                    .list("BNDDIR")
                    .into_iter()
                    .filter(|d| d != "*NONE")
                    .collect();
                if let Some(k) = crate::rpg::ast::keyword(&prog.control, "BNDDIR") {
                    dirs.extend(
                        k.args
                            .iter()
                            .map(|a| a.trim().trim_matches('\'').to_ascii_uppercase()),
                    );
                }
                // DFTACTGRP(*YES) programs cannot bind; ctl-opt BNDDIR or
                // ACTGRP implies DFTACTGRP(*NO) (ILE RPG Reference).
                let ctl = |k: &str| crate::rpg::ast::keyword(&prog.control, k);
                let dft = match ctl("DFTACTGRP").and_then(|k| k.args.first()) {
                    Some(a) => a.trim().eq_ignore_ascii_case("*YES"),
                    None if ctl("BNDDIR").is_some() || ctl("ACTGRP").is_some() => false,
                    None => cmd.def.name == "CRTBNDRPG" && cmd.get("DFTACTGRP") == "*YES",
                };
                if dft {
                    bail!(
                        "CPF5D04: Program {name} not created: it calls procedures that are not in it ({}), and a program in the default activation group (DFTACTGRP(*YES)) cannot be bound to them; specify DFTACTGRP(*NO) and BNDDIR.",
                        prog.imports.join(", ")
                    );
                }
                let entry = crate::ile::ModuleCopy {
                    name: name.clone(),
                    source: m.source.clone(),
                    options: options.clone(),
                    attribute: attribute.to_string(),
                };
                let (mut b, _) = crate::ile::bind(self.client, &libl, vec![entry], &[], &dirs)
                    .await
                    .map_err(|e| anyhow::anyhow!("CPF5D04: Program {name} not created. {e:#}"))?;
                b.modules.retain(|x| x.name != name);
                binding = Some(b);
            }
            m.source.clone()
        } else {
            let src =
                crate::source::expand_cl_includes(self.client, &self.library_list(), &m.source)
                    .await?;
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            crate::clpgm::compile_in(self.client, &libl, &name, &src).await?;
            src
        };
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let replace = cmd.get("REPLACE") != "*NO";
        if let Some(p) =
            catalog::find_program(self.client, std::slice::from_ref(&lib), &name).await?
        {
            if !replace {
                bail!("Program {name} in library {lib} already exists (REPLACE(*NO)).");
            }
            authority::check(self.client, &self.user, &p.library, &name, "*PGM", Aut::All).await?;
        }
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        catalog::create_program(&mut c, &lib, &name, attribute, &text, &source, replace).await?;
        catalog::set_program_options(self.client, &lib, &name, &options).await?;
        catalog::set_program_source(self.client, &lib, &name, (&m.file, &m.library, &m.name))
            .await?;
        crate::ile::set_program_binding(self.client, &lib, &name, binding.as_ref()).await?;
        authority::set_owner(self.client, &lib, &name, "*PGM", &self.user).await?;
        // A CL program: CPC0815 (observed on IBM i 7.5).
        let id = if matches!(cmd.def.name, "CRTBNDCL" | "CRTCLPGM") {
            "CPC0815: "
        } else {
            ""
        };
        Ok(Outcome::Messages(vec![format!(
            "{id}Program {name} was created in library {lib}."
        )]))
    }

    /// CVTRPGSRC: RPG III members to RPG IV members (ILE RPG
    /// Programmer's Guide, "Using the RPG III to RPG IV Conversion Aid").
    /// Members of type RPG become RPGLE, SQLRPG become SQLRPGLE; others
    /// are not converted.
    async fn cvtrpgsrc(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, file) = self.search(cmd.get("FROMFILE"));
        let file = file.to_ascii_uppercase();
        let mut lib = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &file).await? {
                lib = Some(l.clone());
                break;
            }
        }
        let lib = lib.ok_or_else(|| anyhow!("CPF9812: File {file} not found."))?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Use).await?;
        let pattern = cmd.get("FROMMBR").to_ascii_uppercase();
        let all = crate::source::members(self.client, &lib, &file).await?;
        let chosen: Vec<_> = all
            .into_iter()
            .filter(|(m, _, _)| match pattern.as_str() {
                "*ALL" => true,
                p if p.ends_with('*') => m.starts_with(p.trim_end_matches('*')),
                p => m == p,
            })
            .collect();
        if chosen.is_empty() {
            bail!("CPF9815: Member {pattern} of file {file} in library {lib} does not exist.");
        }
        let to = cmd.get("TOFILE").to_ascii_uppercase();
        let to_target = if to == "*NONE" {
            None
        } else {
            let (tl, tf) = self.create_target(&to);
            let tl = tl.to_ascii_uppercase();
            let tf = tf.to_ascii_uppercase();
            if !crate::source::is_source_file(self.client, &tl, &tf).await? {
                bail!("CPF9812: File {tf} in library {tl} not found.");
            }
            Some((tl, tf))
        };
        if chosen.len() > 1 && cmd.get("TOMBR") != "*FROMMBR" {
            bail!("TOMBR must be *FROMMBR when more than one member is converted.");
        }
        let expand = cmd.get("EXPCPY") == "*YES";
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        let mut out = Vec::new();
        let (mut done, mut skipped) = (0, 0);
        for (name, srctype, text) in chosen {
            let new_type = match srctype.as_str() {
                "RPG" | "RPG38" => "RPGLE",
                "SQLRPG" => "SQLRPGLE",
                _ => {
                    out.push(format!(
                        "Member {name} not converted: source type {srctype} is not RPG or SQLRPG."
                    ));
                    skipped += 1;
                    continue;
                }
            };
            let m =
                crate::source::find_member(self.client, std::slice::from_ref(&lib), &file, &name)
                    .await?;
            let c = crate::rpg::convert_rpg3(self.client, &libl, &m.source, expand).await?;
            if cmd.get("CVTRPT") != "*NO" {
                for msg in &c.messages {
                    out.push(format!("{name}: {msg}"));
                }
            }
            if let Some((tl, tf)) = &to_target {
                let tm = match cmd.get("TOMBR") {
                    "*FROMMBR" => name.clone(),
                    t => t.to_ascii_uppercase(),
                };
                crate::source::put_member(
                    self.client,
                    &crate::source::Member {
                        library: tl.clone(),
                        file: tf.clone(),
                        name: tm.clone(),
                        srctype: new_type.into(),
                        text: text.clone(),
                        source: c.source,
                    },
                    false,
                )
                .await
                .map_err(|e| anyhow!("Member {tm} not converted: {e:#}"))?;
                out.push(format!(
                    "Member {name} converted to {tl}/{tf}({tm}), type {new_type}."
                ));
            } else {
                out.push(format!("Member {name} checked (TOFILE(*NONE))."));
            }
            done += 1;
        }
        out.push(format!(
            "{done} members converted, {skipped} not converted."
        ));
        Ok(Outcome::Messages(out))
    }

    /// CVTRPGFREE (as4tainr's own command; IBM i has none): fixed-form
    /// RPGLE and SQLRPGLE members to free form (`rpg::tofree`), written as
    /// new members; what has no free form stays fixed.
    async fn cvtrpgfree(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, file) = self.search(cmd.get("FROMFILE"));
        let file = file.to_ascii_uppercase();
        let pattern = cmd.get("FROMMBR").to_ascii_uppercase();
        let mut lib = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &file).await? {
                lib = Some(l.clone());
                break;
            }
        }
        let lib = lib.ok_or_else(|| anyhow!("CPF9812: File {file} not found."))?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Use).await?;
        let (tl, tf) = self.create_target(cmd.get("TOFILE"));
        let (tl, tf) = (tl.to_ascii_uppercase(), tf.to_ascii_uppercase());
        let chosen: Vec<_> = crate::source::members(self.client, &lib, &file)
            .await?
            .into_iter()
            .filter(|(m, _, _)| match pattern.as_str() {
                "*ALL" => true,
                p if p.ends_with('*') => m.starts_with(p.trim_end_matches('*')),
                p => m == p,
            })
            .collect();
        if chosen.is_empty() {
            bail!("CPF9815: Member {pattern} of file {file} in library {lib} does not exist.");
        }
        let to_mbr = cmd.get("TOMBR").to_ascii_uppercase();
        if chosen.len() > 1 && to_mbr != "*FROMMBR" {
            bail!("TOMBR must be *FROMMBR when more than one member is converted.");
        }
        let mut out = Vec::new();
        for (name, srctype, text) in chosen {
            if !matches!(srctype.as_str(), "RPGLE" | "SQLRPGLE") {
                out.push(format!(
                    "Member {name} not converted: source type {srctype} is not RPGLE or SQLRPGLE."
                ));
                continue;
            }
            let tm = if to_mbr == "*FROMMBR" {
                name.clone()
            } else {
                to_mbr.clone()
            };
            if tl == lib && tf == file && tm == name {
                bail!("Member {name} would replace itself: name another member (TOMBR) or file (TOFILE).");
            }
            let m =
                crate::source::find_member(self.client, std::slice::from_ref(&lib), &file, &name)
                    .await?;
            let c = crate::rpg::tofree::convert(&m.source)?;
            crate::source::put_member(
                self.client,
                &crate::source::Member {
                    library: tl.clone(),
                    file: tf.clone(),
                    name: tm.clone(),
                    srctype: srctype.clone(),
                    text,
                    source: c.source,
                },
                false,
            )
            .await
            .map_err(|e| anyhow!("Member {tm} not converted: {e:#}"))?;
            for msg in &c.messages {
                out.push(format!("{name}: {msg}"));
            }
            out.push(if c.fixed_lines == 0 {
                format!("Member {name} converted to {tl}/{tf}({tm}): all free form (**FREE).")
            } else {
                format!(
                    "Member {name} converted to {tl}/{tf}({tm}); {} lines stay fixed.",
                    c.fixed_lines
                )
            });
        }
        Ok(Outcome::Messages(out))
    }

    /// GO: find the menu; the display shows it.
    async fn go(&mut self, cmd: &Command) -> Result<Outcome> {
        let menu = crate::menus::find(self.client, cmd.get("MENU"), &self.library_list()).await?;
        if let crate::menus::Menu::Program { library, name, .. } = &menu {
            authority::check(self.client, &self.user, library, name, "*MENU", Aut::Use).await?;
        }
        if self.display.is_none() {
            bail!("Menus can only be shown in an interactive job.");
        }
        Ok(Outcome::Menu(menu))
    }

    /// CRTMNU: only TYPE(*PGM) for now.
    async fn crtmnu(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = match cmd.get("MENU").split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (self.current_library(), cmd.get("MENU").to_string()),
        };
        match cmd.get("TYPE") {
            "*PGM" => {}
            "*DSPF" | "*UIM" => bail!(
                "Menu type {} is not supported yet; use TYPE(*PGM) with a program that shows the menu.",
                cmd.get("TYPE")
            ),
            other => bail!("Menu type {other} is not valid; use *DSPF, *PGM or *UIM."),
        }
        // PGM(*MENU): the program has the menu's name, in the menu's library.
        let program = match cmd.get("PGM") {
            "*MENU" => format!("{lib}/{name}"),
            p if p.contains('/') => p.to_string(),
            p => format!("*LIBL/{p}"),
        };
        crate::menus::create(self.client, &lib, &name, &program, cmd.get("TEXT")).await?;
        let name = name.to_ascii_uppercase();
        authority::set_owner(self.client, &lib, &name, "*MENU", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Menu {name} created in library {lib}."
        )]))
    }

    /// A command made with CRTCMD, when the line runs one: a qualified
    /// name, or a name the system commands do not have, found in the
    /// library list.
    pub async fn user_command(&self, line: &str) -> Result<Option<crate::cmd::Definition>> {
        let first = line
            .trim()
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let (libs, name) = match first.split_once('/') {
            Some(("QSYS", n)) if cl::is_system(n) => return Ok(None),
            Some(_) => self.search(&first),
            None if cl::is_system(&first) => return Ok(None),
            None => (self.library_list(), first.clone()),
        };
        if name.is_empty() || name.starts_with('*') {
            return Ok(None);
        }
        crate::cmd::find(self.client, &libs, &name).await
    }

    /// Run a command made with CRTCMD: its parameters checked against its
    /// definition, then its validity checking program and its command
    /// processing program called with them.
    async fn run_user_command(
        &mut self,
        d: &crate::cmd::Definition,
        line: &str,
    ) -> Result<Outcome> {
        authority::check(
            self.client,
            &self.user,
            &d.library,
            &d.name,
            "*CMD",
            Aut::Use,
        )
        .await?;
        let args = crate::cmd::call_args(crate::cmd::encode(d, line)?);
        if let Some(v) = &d.vldckr {
            self.call_program_boxed(v, args.clone()).await?;
        }
        self.call_program_boxed(&d.pgm, args).await
    }

    /// CRTCMD: a command from command definition source.
    async fn crtcmd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("CMD"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        crate::catalog::validate_name("command", &name)?;
        let m = self.source_member(cmd, &name).await?;
        let qualified = |q: &str| -> String {
            let q = q.to_ascii_uppercase();
            if q.contains('/') {
                q
            } else {
                format!("*LIBL/{q}")
            }
        };
        let pgm = qualified(cmd.get("PGM"));
        let mut d = crate::cmd::compile(&lib, &name, &pgm, &m.source)
            .map_err(|e| anyhow!("CPF0201: Command {name} not created in library {lib}. {e:#}"))?;
        d.vldckr = match cmd.get("VLDCKR") {
            "*NONE" | "" => None,
            v => Some(qualified(v)),
        };
        d.maxpos = cmd.get("MAXPOS").parse().ok();
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        d.text = text.clone();
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        crate::cmd::create(&mut c, &d, &text, cmd.get("REPLACE") != "*NO").await?;
        authority::set_owner(self.client, &lib, &name, "*CMD", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Command {name} created in library {lib}."
        )]))
    }

    async fn dltcmd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("CMD"));
        let name = name.to_ascii_uppercase();
        let Some(d) = crate::cmd::find(self.client, &libs, &name).await? else {
            bail!("CPF2105: Object {name} of type *CMD in library *LIBL not found.");
        };
        authority::check(self.client, &self.user, &d.library, &name, "*CMD", Aut::All).await?;
        crate::cmd::delete(self.client, &d.library, &name).await?;
        authority::forget(self.client, &d.library, &name, "*CMD").await?;
        Ok(Outcome::Messages(vec![format!(
            "Command {name} deleted from library {}.",
            d.library
        )]))
    }

    /// DSPCMD: what a command runs and its parameters.
    async fn dspcmd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("CMD"));
        let name = name.to_ascii_uppercase();
        let d = match crate::cmd::find(self.client, &libs, &name).await? {
            Some(d) => d,
            None if cl::is_system(&name) => crate::cmd::Definition {
                library: "QSYS".into(),
                name: name.clone(),
                pgm: format!("QSYS/{name}"),
                prompt: cl::find(&name)
                    .map(|c| c.description.to_string())
                    .unwrap_or_default(),
                ..Default::default()
            },
            None => bail!("CPF2105: Object {name} of type *CMD in library *LIBL not found."),
        };
        authority::check(self.client, &self.user, &d.library, &name, "*CMD", Aut::Use).await?;
        Ok(Outcome::Display(ListDisplay {
            title: "Display Command".into(),
            info: vec![
                ("Command".into(), d.name.clone()),
                ("Library".into(), d.library.clone()),
                ("Program to process command".into(), d.pgm.clone()),
                (
                    "Validity checker".into(),
                    d.vldckr.clone().unwrap_or_else(|| "*NONE".into()),
                ),
                ("Prompt".into(), d.prompt.clone()),
                ("Text".into(), d.text.clone()),
            ],
            heading: format!("{:<10}  {:<10}  {:>6}  Prompt", "Keyword", "Type", "Length"),
            rows: d
                .parms
                .iter()
                .map(|p| format!("{:<10}  {:<10}  {:>6}  {}", p.kwd, p.ty, p.len, p.prompt))
                .collect(),
            ..Default::default()
        }))
    }

    /// The job's name for its commitment definitions (and record locks):
    /// `000123/USER/NAME`, or the process for a job the system does not
    /// know.
    pub fn job_key(&self) -> String {
        self.id
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or_else(|| format!("PID{}", std::process::id()))
    }

    /// STRCMTCTL. A CL program and the command line run in the default
    /// activation group.
    async fn strcmtctl(&mut self, cmd: &Command) -> Result<Outcome> {
        let lcklvl = cmd.get("LCKLVL").to_ascii_uppercase();
        if !matches!(lcklvl.as_str(), "*CHG" | "*CS" | "*ALL") {
            bail!(
                "CPF0006: The command has errors and was not run. LCKLVL({lcklvl}) is not *CHG, *CS or *ALL."
            );
        }
        crate::cmtctl::start(
            self.client,
            &self.env.database_url,
            &self.job_key(),
            crate::ile::DEFAULT_GROUP,
            &lcklvl,
            cmd.get("CMTSCOPE") == "*JOB",
        )
        .await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// ENDCMTCTL: pending changes are rolled back (as in a batch job; an
    /// interactive job is not asked first).
    async fn endcmtctl(&mut self) -> Result<Outcome> {
        let n = crate::cmtctl::end(&self.job_key(), crate::ile::DEFAULT_GROUP).await?;
        Ok(Outcome::Messages(if n > 0 {
            vec![format!(
                "CPF8356: Commitment control ended; {n} pending changes were rolled back."
            )]
        } else {
            vec![]
        }))
    }

    /// COMMIT and ROLLBACK (CL): CPF8350 without a commitment definition.
    async fn commit(&mut self, commit: bool) -> Result<Outcome> {
        let Some(d) = crate::cmtctl::find(&self.job_key(), crate::ile::DEFAULT_GROUP) else {
            bail!("CPF8350: Commitment definition not found.");
        };
        if commit {
            d.commit().await?;
        } else {
            d.rollback().await?;
        }
        Ok(Outcome::Messages(vec![]))
    }

    /// OVRDBF and OVRPRTF: at the current call level (OVRSCOPE(*JOB):
    /// for the whole job, as the command line's are).
    fn override_file(&mut self, cmd: &Command, kind: crate::ovr::Kind) -> Result<Outcome> {
        let file = crate::catalog::validate_name("file", cmd.get("FILE")).or_else(|e| {
            if cmd.get("FILE") == "*PRTF" {
                Ok("*PRTF".to_string())
            } else {
                Err(e)
            }
        })?;
        let tofile = match cmd.get("TOFILE") {
            "*FILE" | "" => None,
            t => Some(t.to_ascii_uppercase()),
        };
        let num = |k: &str| -> Result<Option<i32>> {
            match cmd.get(k) {
                "*NONE" | "" => Ok(None),
                v => v.trim().parse().map(Some).map_err(|_| {
                    anyhow!("CPF0006: The command has errors and was not run. {k}({v}) is not a number.")
                }),
            }
        };
        let pagesize = match cmd.list("PAGESIZE").as_slice() {
            [l, w, ..] => Some((
                l.parse().map_err(|_| {
                    anyhow!(
                        "CPF0006: The command has errors and was not run. PAGESIZE length {l} is not a number."
                    )
                })?,
                w.parse().map_err(|_| {
                    anyhow!(
                        "CPF0006: The command has errors and was not run. PAGESIZE width {w} is not a number."
                    )
                })?,
            )),
            _ => None,
        };
        let level = if cmd.get("OVRSCOPE") == "*JOB" {
            0
        } else {
            self.calls.level()
        };
        crate::ovr::add(
            &mut self.calls.overrides,
            crate::ovr::Override {
                level,
                kind,
                file,
                tofile,
                pagesize,
                ovrflw: num("OVRFLW")?,
                no_lvlchk: cmd.get("LVLCHK") == "*NO",
            },
        );
        Ok(Outcome::Messages(vec![]))
    }

    /// DLTOVR: overrides of the current call level (LVL(*JOB): those for
    /// the whole job).
    fn dltovr(&mut self, cmd: &Command) -> Result<Outcome> {
        let level = if cmd.get("LVL") == "*JOB" {
            0
        } else {
            self.calls.level()
        };
        let files = cmd.list("FILE");
        let before = self.calls.overrides.len();
        self.calls.overrides.retain(|o| {
            o.level != level
                || !files.iter().any(|f| {
                    f == "*ALL"
                        || (f == "*PRTF" && o.kind == crate::ovr::Kind::Printer)
                        || f.eq_ignore_ascii_case(&o.file)
                })
        });
        // A file with no override at the level: CPF9841 (CL reference,
        // DLTOVR; observed on IBM i 7.5).
        if self.calls.overrides.len() == before
            && !files.iter().any(|f| f == "*ALL" || f == "*PRTF")
        {
            bail!(
                "CPF9841: There is no override of file {} at this call level.",
                files.join(" ")
            );
        }
        // FILE(*ALL) with no overrides: CPF5809, an informational message
        // (observed on IBM i 7.5).
        if self.calls.overrides.len() == before && files.iter().any(|f| f == "*ALL") {
            return Ok(Outcome::Messages(vec![
                "CPF5809: There are no overrides at this call level.".into(),
            ]));
        }
        Ok(Outcome::Messages(vec![]))
    }

    /// CLRPFM: every record removed; the next record written is record 1.
    async fn clrpfm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("FILE"));
        let lib_label = cmd
            .get("FILE")
            .split_once('/')
            .map(|(l, _)| l)
            .unwrap_or("*LIBL");
        let name = name.to_ascii_uppercase();
        let q = match libs.as_slice() {
            [one] if cmd.get("FILE").contains('/') => format!("{one}/{name}"),
            _ => name.clone(),
        };
        let fr = crate::catalog::resolve_file(self.client, &self.library_list(), &q)
            .await
            .map_err(|_| anyhow!("CPF3142: File {name} in library {lib_label} not found."))?;
        if fr.file.view.is_some()
            || !catalog::object_exists(
                self.client,
                std::slice::from_ref(&fr.library),
                &name,
                "*FILE",
            )
            .await?
        {
            bail!("CPF3142: File {name} in library {lib_label} not found.");
        }
        let lf: bool = self
            .client
            .query_opt(
                "SELECT 1 FROM as4tainr.logical_files WHERE library=$1 AND name=$2",
                &[&fr.library, &name],
            )
            .await?
            .is_some();
        if lf {
            bail!(
                "File {name} in library {} is not a physical file; CLRPFM clears physical files.",
                fr.library
            );
        }
        authority::check(
            self.client,
            &self.user,
            &fr.library,
            &name,
            "*FILE",
            Aut::Change,
        )
        .await?;
        self.client
            .batch_execute(&format!(
                "TRUNCATE TABLE {}.{} RESTART IDENTITY",
                catalog::ident(&fr.library),
                catalog::ident(&name)
            ))
            .await?;
        // A journaled file: F CR, member cleared.
        let job = self.id.as_ref().map(|i| i.to_string()).unwrap_or_default();
        crate::journal::file_cleared(self.client, &fr.library, &name, (&job, &self.user)).await?;
        let mbr = match cmd.get("MBR") {
            "*FIRST" | "*LAST" | "" => name.clone(),
            m => m.to_ascii_uppercase(),
        };
        Ok(Outcome::Messages(vec![format!(
            "CPC3101: Member {mbr} of file {name} in library {} was cleared.",
            fr.library
        )]))
    }

    /// DLYJOB: wait a number of seconds, or until a time of day (RSMTIME
    /// hhmmss, today or, when that time is past, tomorrow; times in UTC).
    async fn dlyjob(&mut self, cmd: &Command) -> Result<Outcome> {
        let secs: u64 = match (cmd.get("DLY"), cmd.get("RSMTIME")) {
            (d, _) if d != "*NONE" && !d.is_empty() => d.trim().parse().map_err(|_| {
                anyhow!("CPF0006: The command has errors and was not run. DLY({d}) is not a number of seconds.")
            })?,
            (_, t) if t != "*NONE" && !t.is_empty() => {
                let t: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
                if t.len() != 6 {
                    bail!(
                        "CPF0006: The command has errors and was not run. RSMTIME({t}) is not a time hhmmss."
                    );
                }
                let target = t[0..2].parse::<u64>()? * 3600
                    + t[2..4].parse::<u64>()? * 60
                    + t[4..6].parse::<u64>()?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() % 86400)
                    .unwrap_or(0);
                (target + 86400 - now) % 86400
            }
            _ => bail!("CPF0006: The command has errors and was not run. DLY or RSMTIME is needed."),
        };
        if let Some(id) = &self.id {
            jobs::set_function(self.client, id, "DLYJOB", "DLY").await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        Ok(Outcome::Messages(vec![]))
    }

    /// CRTMSGF.
    async fn crtmsgf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("MSGF"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        crate::msgf::create(self.client, &lib, &name, text).await?;
        authority::set_owner(self.client, &lib, &name, "*MSGF", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC9801: Object {name} of type *MSGF was created in library {lib}."
        )]))
    }

    /// The library of a message file named on a command (CPF2407 when
    /// there is none).
    async fn message_file(&self, q: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(q);
        let name = name.to_ascii_uppercase();
        match crate::msgf::locate(self.client, &libs, &name).await? {
            Some(lib) => Ok((lib, name)),
            None => {
                let lib = q.split_once('/').map(|(l, _)| l).unwrap_or("*LIBL");
                bail!("CPF2407: Message file {name} in {lib} not found.")
            }
        }
    }

    /// DLTMSGF.
    async fn dltmsgf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::All).await?;
        crate::msgf::delete(self.client, &lib, &name).await?;
        authority::forget(self.client, &lib, &name, "*MSGF").await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2191: Object {name} of type *MSGF in library {lib} was deleted."
        )]))
    }

    /// ADDMSGD, and CHGMSGD (`change`: *SAME keeps a value).
    async fn addmsgd(&mut self, cmd: &Command, change: bool) -> Result<Outcome> {
        let msgid = crate::msgf::check_msgid(cmd.get("MSGID"))?;
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Change).await?;
        let old = if change {
            let all = crate::msgf::list(self.client, &lib, &name).await?;
            Some(all.into_iter().find(|d| d.msgid == msgid).ok_or_else(|| {
                anyhow!("CPF2419: Message {msgid} is not in message file {name} in library {lib}.")
            })?)
        } else {
            None
        };
        let same = |v: &str| change && v == "*SAME";
        let msg = match cmd.get("MSG") {
            v if same(v) => old.as_ref().map(|d| d.msg.clone()).unwrap_or_default(),
            v => v.to_string(),
        };
        let seclvl = match cmd.get("SECLVL") {
            v if same(v) => old.as_ref().map(|d| d.seclvl.clone()).unwrap_or_default(),
            "*NONE" => String::new(),
            v => v.to_string(),
        };
        let sev = match cmd.get("SEV") {
            v if same(v) => old.as_ref().map(|d| d.sev).unwrap_or(0),
            v => match v.trim().parse::<i32>() {
                Ok(n) if (0..=99).contains(&n) => n,
                _ => {
                    bail!("CPF0006: The command has errors and was not run. SEV({v}) is not 0-99.")
                }
            },
        };
        let fmt_vals = cmd.list("FMT");
        let fmt = if fmt_vals.first().is_some_and(|v| same(v)) {
            old.as_ref().map(|d| d.fmt.clone()).unwrap_or_default()
        } else if fmt_vals.iter().any(|v| v.starts_with('(')) {
            crate::msgf::parse_fmt(&fmt_vals)?
        } else {
            // FMT(*CHAR 10): one field written without its parentheses.
            crate::msgf::parse_fmt(&[fmt_vals.join(" ")])?
        };
        let d = crate::msgf::Description {
            library: lib.clone(),
            msgf: name.clone(),
            msgid: msgid.clone(),
            msg,
            seclvl,
            sev,
            fmt,
        };
        crate::msgf::add(self.client, &d, change).await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// RMVMSGD.
    async fn rmvmsgd(&mut self, cmd: &Command) -> Result<Outcome> {
        let msgid = crate::msgf::check_msgid(cmd.get("MSGID"))?;
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Change).await?;
        crate::msgf::remove(self.client, &lib, &name, &msgid).await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// DSPMSGD: the descriptions in a range of identifiers.
    async fn dspmsgd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Use).await?;
        let range = cmd.list("RANGE");
        let (from, to) = match range.as_slice() {
            [] => ("*FIRST".to_string(), "*LAST".to_string()),
            [a] if a == "*ALL" || a == "*FIRST" => ("*FIRST".into(), "*LAST".into()),
            [a] => (a.clone(), a.clone()),
            [a, b, ..] => (a.clone(), b.clone()),
        };
        let ds: Vec<_> = crate::msgf::list(self.client, &lib, &name)
            .await?
            .into_iter()
            .filter(|d| from == "*FIRST" || d.msgid >= from)
            .filter(|d| to == "*LAST" || d.msgid <= to)
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Display Message Descriptions".into(),
            info: vec![
                ("Message file".into(), name.clone()),
                ("Library".into(), lib.clone()),
            ],
            heading: format!("{:<7}  {:>3}  Message text", "Msg ID", "Sev"),
            rows: ds
                .iter()
                .map(|d| format!("{:<7}  {:>3}  {}", d.msgid, format!("{:02}", d.sev), d.msg))
                .collect(),
            ..Default::default()
        }))
    }

    async fn dltmnu(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = match cmd.get("MENU").split_once('/') {
            Some(("*CURLIB", n)) => (Some(self.current_library()), n.to_string()),
            Some(("*LIBL", n)) => (None, n.to_string()),
            Some((l, n)) => (Some(l.to_string()), n.to_string()),
            None => (None, cmd.get("MENU").to_string()),
        };
        let lib = match lib {
            Some(l) => l,
            None => crate::menus::library_of(self.client, &name, &self.library_list())
                .await?
                .ok_or_else(|| anyhow!("Menu {name} in library *LIBL not found."))?,
        };
        if lib == "QSYS" {
            bail!("Menu {name} is supplied with the system and cannot be deleted.");
        }
        authority::check(self.client, &self.user, &lib, &name, "*MENU", Aut::All).await?;
        if !crate::menus::delete(self.client, &lib, &name).await? {
            bail!("Menu {name} in library {lib} not found.");
        }
        authority::forget(self.client, &lib, &name, "*MENU").await?;
        Ok(Outcome::Messages(vec![format!(
            "Menu {name} deleted from library {lib}."
        )]))
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
                    bail!("CPF2110: Library {l} does not exist.");
                }
                vec![l.to_string()]
            }
        };
        let mut rows = Vec::new();
        let mut keys = Vec::new();
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
                    keys.push(format!("OBJ({l}/{}) OBJTYPE({})", o.name, o.objtype));
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
            // IBM i 7.5's WRKOBJ (PUB400): options, the list, a command line.
            options: vec![
                (5, "Display authority".into(), "DSPOBJAUT {}".into()),
                (8, "Display description".into(), "DSPOBJD {}".into()),
            ],
            keys,
            refresh: Some(format!(
                "WRKOBJ OBJ({}) OBJTYPE({objtype})",
                if lib == "*LIBL" {
                    obj.clone()
                } else {
                    format!("{lib}/{obj}")
                }
            )),
            ibm: IbmLayout::some(IbmLayout {
                command_line: Some("Parameters or command".into()),
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    async fn dsppfm(&mut self, cmd: &Command) -> Result<Outcome> {
        // A source file: the lines of a member.
        {
            let (libs, file) = self.search(cmd.get("FILE"));
            let file = file.to_ascii_uppercase();
            for l in &libs {
                if !crate::source::is_source_file(self.client, l, &file).await? {
                    continue;
                }
                authority::check(self.client, &self.user, l, &file, "*FILE", Aut::Use).await?;
                let member = match cmd.get("MBR") {
                    "*FIRST" => crate::source::members(self.client, l, &file)
                        .await?
                        .first()
                        .map(|m| m.0.clone())
                        .ok_or_else(|| anyhow!("File {file} in library {l} has no members."))?,
                    m => m.to_string(),
                };
                let m = crate::source::find_member(
                    self.client,
                    std::slice::from_ref(l),
                    &file,
                    &member,
                )
                .await?;
                return Ok(Outcome::Display(ListDisplay {
                    title: "Display Physical File Member".into(),
                    info: vec![
                        ("File".into(), file.clone()),
                        ("Library".into(), l.clone()),
                        ("Member".into(), m.name.clone()),
                        ("Type".into(), m.srctype.clone()),
                    ],
                    heading: "SEQNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7".into(),
                    rows: m
                        .source
                        .lines()
                        .enumerate()
                        .map(|(i, line)| format!("{:>4}.00 {}", i + 1, line))
                        .collect(),
                    ..Default::default()
                }));
            }
        }
        let name = match cmd.get("FILE").split_once('/') {
            Some(("*LIBL", n)) => n.to_string(),
            Some(("*CURLIB", n)) => format!("{}/{n}", self.current_library()),
            _ => cmd.get("FILE").to_string(),
        };
        let f = catalog::resolve_file(self.client, &self.library_list(), &name).await?;
        authority::check(
            self.client,
            &self.user,
            &f.library,
            &f.file.name,
            "*FILE",
            Aut::Use,
        )
        .await?;
        let recs = catalog::member_records(self.client, &f, DSPPFM_MAX_RECORDS + 1).await?;
        let widths: Vec<usize> = f
            .file
            .fields
            .iter()
            .map(|fd| {
                let w = if fd.data_type.is_numeric() {
                    // digits, decimal point, sign
                    fd.length as usize + usize::from(fd.decimals > 0) + 1
                } else if fd.varlen {
                    // Variable length: as wide as the longest value shown.
                    0
                } else {
                    fd.length as usize
                };
                w.max(fd.name.len())
            })
            .collect();
        let widths: Vec<usize> = widths
            .iter()
            .enumerate()
            .map(|(i, w)| {
                if f.file.fields[i].varlen {
                    recs.iter()
                        .take(DSPPFM_MAX_RECORDS as usize)
                        .map(|(_, v)| v.get(i).map(|x| x.chars().count()).unwrap_or(0))
                        .max()
                        .unwrap_or(0)
                        .max(*w)
                } else {
                    *w
                }
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
        // A system command is checked now; a command made with CRTCMD
        // when the job runs it.
        if self.user_command(&command).await?.is_none() {
            cl::parse(&command)?;
        }
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
                    bail!("CPF2110: Library {l} does not exist.");
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
            "CPC1221: Job {id} was put on job queue {jobq} in library QGPL."
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
            print: vec![],
            ibm: None,
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
            print: vec![],
            ibm: None,
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
        if cmd.get("OUTPUT") == "*PRINT" {
            let me = self
                .id
                .clone()
                .ok_or_else(|| anyhow!("OUTPUT(*PRINT) needs a job to own the spooled file."))?;
            let lines = crate::spool::job_log_lines(self.client, &job.id).await?;
            crate::spool::print_lines(self.client, &me, "QPJOBLOG", &job.id.name, &lines).await?;
            return Ok(Outcome::Messages(vec![format!(
                "Job log of {} printed to spooled file QPJOBLOG.",
                job.id
            )]));
        }
        let entries = jobs::job_log(self.client, job.id.number).await?;
        // IBM i 7.5's DSPJOBLOG display (PUB400): requests after '>',
        // messages under them as texts (F1 on IBM i shows the message ID).
        let rows = entries
            .iter()
            .flat_map(|e| {
                let first = if e.kind == "*RQS" {
                    format!("> {}", e.text)
                } else {
                    format!("    {}", jobs::shown_text(&e.text))
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
                    let indent = "      ";
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
            info: vec![],
            heading: String::new(),
            rows,
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(62),
                lines: vec![format!(
                    "Job . . :   {:<10}    User . . :   {:<10}    Number . . . :   {:06}",
                    job.id.name, job.id.user, job.id.number
                )],
                enter_line: true,
                top_bottom_keys: true,
                ..Default::default()
            }),
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
        // IBM i 7.5's DSPMSG display (PUB400): the queue's attributes, then
        // each message, oldest first, under a "From" line with the sender,
        // date and time; texts without message IDs.
        let mut rows = Vec::new();
        for m in &msgs {
            let (date, time) = m.sent.split_once(' ').unwrap_or((m.sent.as_str(), ""));
            rows.push(format!(
                "  {}   {:<10}     {date}   {time}",
                dsp_label("From", 10),
                m.from_user
            ));
            let text = jobs::shown_text(&m.text);
            let mut line = String::new();
            for w in text.split(' ') {
                if !line.is_empty() && 2 + line.len() + 1 + w.len() > 77 {
                    rows.push(format!("  {line}"));
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(w);
            }
            rows.push(format!("  {line}"));
        }
        let left = |l: &str, v: &str| format!("{:<44}", format!("{}   {v}", dsp_label(l, 14)));
        Ok(Outcome::Display(ListDisplay {
            title: "Display Messages".into(),
            info: vec![],
            heading: String::new(),
            rows,
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(56),
                lines: vec![
                    format!(
                        "{}{}   *DSPMSG",
                        left("Queue", &q),
                        dsp_label("Program", 14)
                    ),
                    format!(
                        "{}{}",
                        left("  Library", "  QUSRSYS"),
                        dsp_label("  Library", 14)
                    ),
                    format!(
                        "{}{}   *NOTIFY",
                        left("Severity", "00"),
                        dsp_label("Delivery", 14)
                    ),
                ],
                instruction: "Type reply (if required), press Enter.".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    /// The library and name of an existing object (OBJ and OBJTYPE).
    async fn object_named(&self, cmd: &Command) -> Result<(String, String, String)> {
        let objtype = cmd.get("OBJTYPE").to_string();
        if !["*FILE", "*PGM", "*DTAARA", "*LIB", "*MENU"].contains(&objtype.as_str()) {
            bail!(
                "Object type {objtype} is not supported; use *FILE, *PGM, *DTAARA, *MENU or *LIB."
            );
        }
        let (lib, name) = match cmd.get("OBJ").split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some(("*LIBL", n)) => ("*LIBL".to_string(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), cmd.get("OBJ").to_string()),
        };
        let libs = if objtype == "*LIB" {
            vec!["QSYS".to_string()]
        } else if lib == "*LIBL" {
            self.library_list()
        } else {
            vec![lib.clone()]
        };
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, &objtype).await?
            {
                return Ok((l.clone(), name, objtype));
            }
        }
        bail!("CPF9801: Object {name} in library {lib} type {objtype} not found.")
    }

    /// GRTOBJAUT and RVKOBJAUT.
    async fn objaut(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, objtype) = self.object_named(cmd).await?;
        authority::check_manage(self.client, &self.user, &lib, &name, &objtype).await?;
        let grant = cmd.def.name == "GRTOBJAUT";
        let aut = Aut::parse(cmd.get("AUT"))?;
        let mut done = Vec::new();
        for user in cmd.list("USER") {
            if user != "*PUBLIC" && users::get(self.client, &user).await?.is_none() {
                bail!("CPF2204: User profile {user} not found.");
            }
            if grant {
                authority::grant(self.client, &lib, &name, &objtype, &user, aut).await?;
                done.push(format!(
                    "{user} has authority {} to {lib}/{name} {objtype}.",
                    aut.code()
                ));
            } else {
                authority::revoke(self.client, &lib, &name, &objtype, &user).await?;
                done.push(if user == "*PUBLIC" {
                    format!("*PUBLIC authority to {lib}/{name} {objtype} is now *EXCLUDE.")
                } else {
                    format!("Private authority of {user} to {lib}/{name} {objtype} revoked.")
                });
            }
        }
        Ok(Outcome::Messages(done))
    }

    async fn dspobjaut(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, objtype) = self.object_named(cmd).await?;
        let a = authority::get(self.client, &lib, &name, &objtype)
            .await?
            .ok_or_else(|| anyhow!("CPF9801: Object {name} was not found in library {lib}."))?;
        let mut rows = vec![format!("{:<10}  {}", a.owner, "*ALL (owner)")];
        for (u, aut) in &a.private {
            rows.push(format!("{u:<10}  {}", aut.code()));
        }
        rows.push(format!("{:<10}  {}", "*PUBLIC", a.public.code()));
        Ok(Outcome::Display(ListDisplay {
            title: "Display Object Authority".into(),
            info: vec![
                ("Object".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Object type".into(), objtype.clone()),
                ("Owner".into(), a.owner.clone()),
            ],
            heading: format!("{:<10}  {}", "User", "Authority"),
            rows,
            ..Default::default()
        }))
    }

    async fn chgobjown(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, objtype) = self.object_named(cmd).await?;
        authority::check_manage(self.client, &self.user, &lib, &name, &objtype).await?;
        let new = cmd.get("NEWOWN").to_string();
        if users::get(self.client, &new).await?.is_none() {
            bail!("CPF2204: User profile {new} not found.");
        }
        authority::set_owner(self.client, &lib, &name, &objtype, &new).await?;
        Ok(Outcome::Messages(vec![format!(
            "Owner of {lib}/{name} {objtype} changed to {new}."
        )]))
    }

    async fn wrksplf(&mut self, cmd: &Command) -> Result<Outcome> {
        let user = match cmd.get("SELECT") {
            "*CURRENT" => self.user.clone(),
            u => {
                if u != self.user {
                    self.require_secadm("work with other users' spooled files")
                        .await?;
                }
                u.to_string()
            }
        };
        let files = crate::spool::for_user(self.client, &user).await?;
        // IBM i 7.5's WRKSPLF display (PUB400): file, user, output queue,
        // user data, status, total pages, current page, copies.
        let fmt = |file: &str,
                   user: &str,
                   queue: &str,
                   data: &str,
                   sts: &str,
                   pages: &str,
                   cur: &str,
                   copy: &str| {
            format!("{file:<10}  {user:<10}  {queue:<10}  {data:<10}  {sts:<4}  {pages:>5}  {cur:>6}  {copy:>4}")
        };
        let rows = files
            .iter()
            .map(|f| {
                fmt(
                    &f.file,
                    &f.user,
                    &f.outq,
                    &f.user_data,
                    &f.status,
                    &f.pages.to_string(),
                    "",
                    "1",
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with All Spooled Files".into(),
            info: vec![],
            heading: fmt(
                "File",
                "User",
                "Queue",
                "User Data",
                "Sts",
                "Pages",
                "Page",
                "Copy",
            ),
            rows,
            options: vec![
                (4, "Delete".into(), "DLTSPLF {}".into()),
                (5, "Display".into(), "DSPSPLF {}".into()),
            ],
            keys: files
                .iter()
                .map(|f| format!("FILE({}) JOB({}) SPLNBR({})", f.file, f.job_name, f.number))
                .collect(),
            refresh: Some(format!("WRKSPLF SELECT({user})")),
            print: vec![],
            ibm: IbmLayout::some(IbmLayout {
                heading2: format!("{:24}{:<30}{:>5}{:>8}", "", "Device or", "Total", "Cur"),
                command_line: Some("Parameters or command".into()),
                // F11: the job each file belongs to (as4tainr's columns).
                view2: Some((
                    String::new(),
                    format!(
                        "{:<10}  {:<10}  {:<10}  {:>6}  {:>8}",
                        "File", "Job", "User", "Number", "File Nbr"
                    ),
                    files
                        .iter()
                        .map(|f| {
                            let (number, rest) =
                                f.job_name.split_once('/').unwrap_or(("", &f.job_name));
                            let job = rest.rsplit('/').next().unwrap_or(rest);
                            format!(
                                "{:<10}  {:<10}  {:<10}  {:>6}  {:>8}",
                                f.file, job, f.user, number, f.number
                            )
                        })
                        .collect(),
                )),
                ..Default::default()
            }),
        }))
    }

    /// The spooled file a FILE/JOB/SPLNBR command names.
    async fn spooled_file(&self, cmd: &Command) -> Result<crate::spool::SpooledFile> {
        let job = self.job_named(cmd.get("JOB")).await?;
        let number = match cmd.get("SPLNBR") {
            "*LAST" | "" => None,
            n => Some(
                n.parse::<i32>()
                    .map_err(|_| anyhow!("SPLNBR({n}) is not a number."))?,
            ),
        };
        let f = crate::spool::find(self.client, cmd.get("FILE"), job.id.number, number).await?;
        if f.user != self.user {
            self.require_secadm("use other users' spooled files")
                .await?;
        }
        Ok(f)
    }

    async fn dspsplf(&mut self, cmd: &Command) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        let pages = crate::spool::pages(self.client, f.id).await?;
        let mut rows = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            if i > 0 {
                rows.push(format!("{:-^40}", format!(" Page {} ", i + 1)));
            }
            rows.extend(page.iter().cloned());
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Spooled File".into(),
            info: vec![
                ("File".into(), f.file.clone()),
                ("Job".into(), format!("{}   Number {}", f.job_name, f.number)),
                ("Pages".into(), f.pages.to_string()),
            ],
            heading: "*...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+....8....+....9....+....0....+....1....+....2....+....3..".into(),
            rows,
            ..Default::default()
        }))
    }

    async fn dltsplf(&mut self, cmd: &Command) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        crate::spool::delete(self.client, f.id).await?;
        Ok(Outcome::Messages(vec![format!(
            "Spooled file {} number {} deleted.",
            f.file, f.number
        )]))
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
        authority::set_owner(self.client, &d.library, &d.name, "*DTAARA", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC0904: Data area {} was created in library {}.",
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
        if name.eq_ignore_ascii_case("*LDA") {
            let job = self.id.as_ref().map(|j| j.number);
            dtaara::lda_change(self.client, job, part, cmd.get("VALUE")).await?;
            return Ok(Outcome::Messages(vec!["Local data area changed.".into()]));
        }
        let found = dtaara::resolve(
            self.client,
            &name,
            &self.library_list(),
            &self.current_library(),
        )
        .await?;
        authority::check(
            self.client,
            &self.user,
            &found.library,
            &found.name,
            "*DTAARA",
            Aut::Change,
        )
        .await?;
        let d = dtaara::change(
            self.client,
            &name,
            &self.library_list(),
            &self.current_library(),
            part,
            cmd.get("VALUE"),
        )
        .await?;
        // CHGDTAARA sends no completion message (observed on IBM i 7.5).
        let _ = d;
        Ok(Outcome::Messages(vec![]))
    }

    async fn dspdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        let d = if cmd.get("DTAARA").eq_ignore_ascii_case("*LDA") {
            dtaara::lda_area(self.client, self.id.as_ref().map(|j| j.number)).await?
        } else {
            let d = dtaara::resolve(
                self.client,
                cmd.get("DTAARA"),
                &self.library_list(),
                &self.current_library(),
            )
            .await?;
            authority::check(
                self.client,
                &self.user,
                &d.library,
                &d.name,
                "*DTAARA",
                Aut::Use,
            )
            .await?;
            d
        };
        // Character values a line of 50 at a time, with their offsets.
        let rows: Vec<String> = match d.ty {
            dtaara::Type::Char => d
                .value
                .chars()
                .collect::<Vec<_>>()
                .chunks(50)
                .enumerate()
                .map(|(i, c)| format!("{:>5}      '{}'", i * 50, c.iter().collect::<String>()))
                .collect(),
            _ => vec![format!("{:>5}      {}", 0, d.value)],
        };
        // Printed layout: PUB400 (IBM i 7.5) `system "DSPDTAARA
        // DTAARA(AS4TRAINR1/GOLDDA)"` (*CHAR, LEN 10), 2026-10-21. *DEC
        // and *LGL values use the same lines (not yet compared).
        use crate::report::at;
        let qual = format!("{}/{}", d.library, d.name);
        let mut print = vec![
            crate::report::header_a(
                17,
                51,
                "Display Data Area",
                73,
                &qual,
                &crate::report::Stamp::now(),
                1,
            ),
            at(&[
                (2, "Data area . . . . . . . . . . . :"),
                (38, "DTAARA"),
                (50, &d.name),
            ]),
            at(&[(4, "Library . . . . . . . . . . . :"), (52, &d.library)]),
            at(&[
                (2, "Type  . . . . . . . . . . . . . :"),
                (38, "TYPE"),
                (50, d.ty.code()),
            ]),
            at(&[
                (2, "Length  . . . . . . . . . . . . :"),
                (38, "LEN"),
                (50, &d.length_text()),
            ]),
            at(&[
                (2, "Text description  . . . . . . . :"),
                (38, "TEXT"),
                (50, &d.text),
            ]),
            at(&[(5, "Offset"), (17, "Value")]),
            at(&[(18, "*...+....1....+....2....+....3....+....4....+....5")]),
        ];
        let chunks: Vec<(usize, String)> = match d.ty {
            dtaara::Type::Char => d
                .value
                .chars()
                .collect::<Vec<_>>()
                .chunks(50)
                .enumerate()
                .map(|(i, c)| (i * 50, format!("'{}'", c.iter().collect::<String>())))
                .collect(),
            _ => vec![(0, d.value.clone())],
        };
        for (off, v) in &chunks {
            print.push(at(&[(2, &format!("{off:>5}")), (17, v)]));
        }
        print.push(crate::report::END_A.into());
        Ok(Outcome::Display(ListDisplay {
            print,
            title: "Display Data Area".into(),
            info: vec![],
            // IBM i 7.5's DSPDTAARA display (PUB400): the value under a
            // ruler, 50 positions to a line, then "Press Enter to continue."
            heading: format!(
                "{:<12}{}",
                "Offset", "*...+....1....+....2....+....3....+....4....+....5"
            ),
            rows,
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(62),
                lines: vec![
                    format!("{}   {}", dsp_label("Data area", 22), d.name),
                    format!("{}     {}", dsp_label("  Library", 22), d.library),
                    format!("{}   {}", dsp_label("Type", 22), d.ty.code()),
                    format!("{}   {}", dsp_label("Length", 22), d.length_text()),
                    format!("{}   {}", dsp_label("Text", 22), d.text),
                ],
                heading2: format!("{:11}Value", ""),
                enter_line: true,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    /// CHKOBJ: does the object exist (and the member, MBR), and has the
    /// user the authority AUT to it? No message when it does; escape
    /// messages CPF9810 (library), CPF9801 (object), CPF9815 (member),
    /// CPF9802 (authority) as IBM's CHKOBJ documentation lists them.
    pub async fn chkobj(&mut self, obj: &str, objtype: &str, mbr: &str, aut: &str) -> Result<()> {
        let obj = obj.trim().to_ascii_uppercase();
        let objtype = objtype.trim().to_ascii_uppercase();
        let (label, name) = match obj.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), obj.clone()),
        };
        let libs = match label.as_str() {
            "*LIBL" => self.library_list(),
            "*CURLIB" => vec![self.current_library()],
            l => {
                if !catalog::library_exists(self.client, l).await? {
                    bail!("CPF9810: Library {l} does not exist.");
                }
                vec![l.to_string()]
            }
        };
        let libs = if objtype == "*LIB" {
            vec!["QSYS".to_string()]
        } else {
            libs
        };
        let mut found = None;
        for lib in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(lib), &name, &objtype)
                .await?
            {
                found = Some(lib.clone());
                break;
            }
        }
        let Some(lib) = found else {
            bail!("CPF9801: Object {name} was not found in library {label}.");
        };
        let mbr = mbr.trim().to_ascii_uppercase();
        if !mbr.is_empty() && mbr != "*NONE" && objtype == "*FILE" {
            let source = crate::source::members(self.client, &lib, &name).await.ok();
            let ok = match &source {
                Some(ms) if !ms.is_empty() => {
                    mbr == "*FIRST" || ms.iter().any(|(m, _, _)| m.eq_ignore_ascii_case(&mbr))
                }
                _ => mbr == "*FIRST" || mbr == name,
            };
            if !ok {
                bail!("CPF9815: Member {mbr} of file {name} in library {lib} does not exist.");
            }
        }
        let aut = aut.trim().to_ascii_uppercase();
        if !aut.is_empty() && aut != "*NONE" {
            let need = Aut::parse(&aut)?;
            authority::check(self.client, &self.user, &lib, &name, &objtype, need).await?;
        }
        Ok(())
    }

    /// DSPSYSVAL: a system value, its shipped value and what it is for.
    fn dspsysval(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = cmd.get("SYSVAL").trim().to_ascii_uppercase();
        let Some(desc) = crate::clpgm::system_value_description(&name) else {
            bail!("CPF1028: System value {name} not found.");
        };
        let show = |v: Result<String, i64>| match v {
            Ok(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
            Err(n) => n.to_string(),
        };
        let current = crate::clpgm::system_value_text(&name)
            .map(show)
            .unwrap_or_default();
        let shipped = crate::clpgm::shipped_system_value_text(&name)
            .map(show)
            .unwrap_or_default();
        // Printed layout: PUB400 (IBM i 7.5) `system "DSPSYSVAL
        // SYSVAL(QCCSID)"`, 2026-10-21.
        use crate::report::at;
        let [h1, h2] = crate::report::header_b(
            52,
            "System Values",
            &crate::report::system_name(),
            &crate::report::Stamp::now(),
            1,
        );
        let differs = current != shipped;
        let mut print = vec![
            h1,
            h2,
            at(&[(17, "Current"), (49, "Shipped")]),
            at(&[
                (2, "Name"),
                (17, "value"),
                (49, "value"),
                (81, "Description"),
            ]),
            at(&[
                (2, &name),
                (14, if differs { ">" } else { "" }),
                (17, &current),
                (49, &shipped),
                (81, desc),
            ]),
        ];
        if differs {
            print.push(at(&[
                (6, "Note:"),
                (
                    13,
                    "> means current value is different from the shipped value",
                ),
            ]));
        }
        print.push(crate::report::END_SYSVAL.into());
        Ok(Outcome::Display(ListDisplay {
            title: "Display System Value".into(),
            info: vec![
                ("System value".into(), name.clone()),
                ("Description".into(), desc.into()),
                ("Value".into(), current),
                ("Shipped value".into(), shipped),
            ],
            print,
            ..Default::default()
        }))
    }

    /// DSPOBJD DETAIL(*BASIC): objects by name (generic, *ALL) and type
    /// in a library or the library list.
    async fn dspobjd(&mut self, cmd: &Command) -> Result<Outcome> {
        let obj = cmd.get("OBJ").trim().to_ascii_uppercase();
        let objtype = cmd.get("OBJTYPE").trim().to_ascii_uppercase();
        let objtype = if objtype.is_empty() {
            "*ALL".to_string()
        } else {
            objtype
        };
        let detail = cmd.get("DETAIL").trim().to_ascii_uppercase();
        if !detail.is_empty() && detail != "*BASIC" {
            return Err(crate::cl::diag_error(
                "CPD0084",
                &format!("DETAIL({detail}) is not supported yet; *BASIC is."),
            ));
        }
        let (label, name) = match obj.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), obj.clone()),
        };
        let libs = match label.as_str() {
            "*LIBL" => self.library_list(),
            "*CURLIB" => vec![self.current_library()],
            "*ALL" | "*USRLIBL" | "*ALLUSR" => catalog::list_libraries(self.client).await?,
            l => {
                if !catalog::library_exists(self.client, l).await? {
                    bail!("CPF9810: Library {l} does not exist.");
                }
                vec![l.to_string()]
            }
        };
        let mut types: Vec<String> = cmd
            .list("OBJTYPE")
            .iter()
            .map(|t| t.trim().to_ascii_uppercase())
            .collect();
        if types.is_empty() {
            types.push(objtype.clone());
        }
        let matches_name = |n: &str| {
            if name == "*ALL" {
                true
            } else if let Some(prefix) = name.strip_suffix('*') {
                n.starts_with(prefix)
            } else {
                n == name
            }
        };
        let mut groups: Vec<(String, Vec<catalog::ObjectEntry>)> = Vec::new();
        for lib in &libs {
            let list = if types.iter().any(|t| t == "*LIB") && lib != "QSYS" {
                vec![]
            } else {
                catalog::list_objects(self.client, lib)
                    .await
                    .unwrap_or_default()
            };
            let mut hits = Vec::new();
            for o in list {
                if !matches_name(&o.name) {
                    continue;
                }
                if !(types.iter().any(|t| t == "*ALL") || types.contains(&o.objtype)) {
                    continue;
                }
                if authority::check(self.client, &self.user, lib, &o.name, &o.objtype, Aut::Use)
                    .await
                    .is_err()
                {
                    continue;
                }
                hits.push(o);
            }
            if !hits.is_empty() {
                groups.push((lib.clone(), hits));
                if name != "*ALL" && !name.ends_with('*') && label == "*LIBL" {
                    // A specific name in the library list: the first found.
                    break;
                }
            }
        }
        if groups.is_empty() {
            // DSPOBJD of an object that is not there: CPF2105 (observed on IBM
            // i 7.5).
            bail!("CPF2105: Object {name} of type {objtype} in library {label} not found.");
        }
        // Printed layout: PUB400 (IBM i 7.5) `system "DSPOBJD
        // OBJ(QSYS/QCMD) OBJTYPE(*PGM)"`, 2026-10-21. Size: what this
        // system stores for the object (fidelity-gaps.md).
        use crate::report::at;
        let [h1, h2] = crate::report::header_b(
            42,
            "Display Object Description - Basic",
            &crate::report::system_name(),
            &crate::report::Stamp::now(),
            1,
        );
        let mut print = vec![h1, h2];
        let mut rows = Vec::new();
        for (lib, objs) in &groups {
            print.push(at(&[
                (2, "Library . . . . . . . :"),
                (28, lib),
                (69, "Library ASP device  . :"),
                (95, "*SYSBAS"),
            ]));
            print.push(at(&[(69, "Library ASP group . . :"), (95, "*SYSBAS")]));
            print.push(at(&[
                (2, "Object"),
                (17, "Type"),
                (30, "Attribute"),
                (57, "Size"),
                (66, "Text"),
            ]));
            for o in objs {
                let size = catalog::object_size(self.client, lib, &o.name, &o.objtype)
                    .await
                    .unwrap_or(0);
                print.push(at(&[
                    (2, &o.name),
                    (17, &o.objtype),
                    (30, &o.attribute),
                    (49, &format!("{size:>12}")),
                    (66, &o.text),
                ]));
                let text: String = o.text.chars().take(26).collect();
                if groups.len() == 1 {
                    rows.push(format!(
                        "{:<10}  {:<8}  {:<10}  {:>11}  {}",
                        o.name, o.objtype, o.attribute, size, text
                    ));
                } else {
                    rows.push(format!(
                        "{:<10}  {:<10}  {:<8}  {:<10}  {}",
                        o.name, lib, o.objtype, o.attribute, text
                    ));
                }
            }
        }
        print.push(crate::report::END_OBJD.into());
        // IBM i 7.5's display (PUB400) shows one library at a time ("Library
        // 1 of 1"); objects from several libraries are one list here.
        let ibm = if groups.len() == 1 {
            let lib = &groups[0].0;
            IbmLayout::some(IbmLayout {
                right2: "Library 1 of 1".into(),
                lines: vec![
                    format!(
                        "{:<41}{}   *SYSBAS",
                        format!("{}   {lib}", dsp_label("Library", 20)),
                        dsp_label("Library ASP device", 20)
                    ),
                    format!("{:<41}{}   *SYSBAS", "", dsp_label("Library ASP group", 20)),
                ],
                top_bottom_keys: true,
                ..Default::default()
            })
        } else {
            None
        };
        Ok(Outcome::Display(ListDisplay {
            title: "Display Object Description - Basic".into(),
            heading: if groups.len() == 1 {
                format!(
                    "{:<10}  {:<8}  {:<10}  {:>11}  {}",
                    "Object", "Type", "Attribute", "Size", "Text"
                )
            } else {
                format!(
                    "{:<10}  {:<10}  {:<8}  {:<10}  {}",
                    "Object", "Library", "Type", "Attribute", "Text"
                )
            },
            rows,
            print,
            ibm,
            ..Default::default()
        }))
    }

    async fn dltdtaara(&mut self, cmd: &Command) -> Result<Outcome> {
        // Not found: CPF2105, as for other objects DLTxxx deletes
        // (observed on IBM i 7.5).
        let found = match dtaara::resolve(
            self.client,
            cmd.get("DTAARA"),
            &self.library_list(),
            &self.current_library(),
        )
        .await
        {
            Ok(f) => f,
            Err(e) if crate::jobs::escape_id(&e.to_string()) == "CPF1015" => {
                let q = cmd.get("DTAARA").to_ascii_uppercase();
                let (lib, name) = q.split_once('/').unwrap_or(("*LIBL", &q));
                bail!("CPF2105: Object {name} of type *DTAARA in library {lib} not found.");
            }
            Err(e) => return Err(e),
        };
        authority::check(
            self.client,
            &self.user,
            &found.library,
            &found.name,
            "*DTAARA",
            Aut::All,
        )
        .await?;
        authority::forget(self.client, &found.library, &found.name, "*DTAARA").await?;
        let d = dtaara::delete(
            self.client,
            cmd.get("DTAARA"),
            &self.library_list(),
            &self.current_library(),
        )
        .await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2191: Object {} of type *DTAARA in library {} was deleted.",
            d.name, d.library
        )]))
    }

    /// CALL: run a program with the job's library list. DSPLY lines come
    /// back as messages.
    async fn call(&mut self, cmd: &Command) -> Result<Outcome> {
        let pgm = cmd.get("PGM").to_string();
        let args = cmd
            .list("PARM")
            .into_iter()
            .map(|a| typed_parm(&a))
            .collect();
        self.call_program(&pgm, args).await
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

    /// CALL: a CL program found through the library list runs here; an
    /// RPG program runs on the RPG engine.
    pub async fn call_program(&mut self, pgm: &str, args: Vec<String>) -> Result<Outcome> {
        let pgm = pgm.to_ascii_uppercase();
        let (libs, name) = match pgm.split_once('/') {
            Some(("*LIBL", n)) => (self.library_list(), n.to_string()),
            Some(("*CURLIB", n)) => (vec![self.current_library()], n.to_string()),
            Some((l, n)) => (vec![l.to_string()], n.to_string()),
            None => (self.library_list(), pgm.clone()),
        };
        // QCMD, the command processor: interactively, the Command Entry
        // display.
        if name == "QCMD" {
            if self.display.is_some() {
                return Ok(Outcome::CommandEntry);
            }
            bail!("QCMD shows the Command Entry display; it can only be called in an interactive job.");
        }
        if let Some(id) = &self.id {
            jobs::set_function(self.client, id, &format!("PGM-{name}"), "RUN").await;
        }
        match catalog::find_program(self.client, &libs, &name).await? {
            Some(p) => {
                authority::check(self.client, &self.user, &p.library, &name, "*PGM", Aut::Use)
                    .await?;
                match p.attribute.as_str() {
                    "CLLE" | "CLP" => {
                        // The files DCLF names were found when the program
                        // was created; IBM i opens them by the library
                        // list at the first I/O, which the program may
                        // change first (ADDLIBLE). Their descriptions are
                        // taken from the library list, else from the
                        // program's own library.
                        let mut libl = self.library_list();
                        if !libl.contains(&p.library) {
                            libl.push(p.library.clone());
                        }
                        let mut program =
                            crate::clpgm::compile_in(self.client, &libl, &name, &p.source).await?;
                        // An ILE CL program bound to modules and service
                        // programs: for CALLPRC.
                        program.object = format!("{}/{}", p.library, name);
                        program.binding = p.binding.clone();
                        let up = p.options.to_ascii_uppercase();
                        if let (true, Some(i)) = (
                            up.contains("DFTACTGRP(*NO)"),
                            up.find(" ACTGRP(")
                                .or_else(|| up.find("ACTGRP(").filter(|i| *i == 0)),
                        ) {
                            let rest = &up[i..];
                            let start = rest.find('(').unwrap_or(0) + 1;
                            let g = rest[start..]
                                .split(')')
                                .next()
                                .unwrap_or("QILE")
                                .to_string();
                            program.actgrp = if g == "*NEW" {
                                format!("*NEW{}", std::process::id())
                            } else {
                                g
                            };
                        }
                        crate::clpgm::run(self, &program, &args).await
                    }
                    _ => self.call_rpg(&name, Some(&p.library), args).await,
                }
            }
            // Not in a library named on the command: CPD0170, CPF0006.
            None if matches!(pgm.split_once('/'), Some((l, _)) if !l.starts_with('*')) => {
                Err(crate::cl::diag_error(
                    "CPD0170",
                    &format!("Program {name} was not found in library {}.", libs[0]),
                ))
            }
            // Not a program object: the source in AS4TAINR_PGM_PATH (the
            // samples).
            None => self.call_rpg(&name, None, args).await,
        }
    }

    /// Run an RPG program (docs/rpg-engine.md). `library`: where the
    /// program object is; None: PGM.rpgle in AS4TAINR_PGM_PATH.
    async fn call_rpg(
        &mut self,
        pgm: &str,
        library: Option<&str>,
        args: Vec<String>,
    ) -> Result<Outcome> {
        let source = match library {
            Some(lib) => catalog::find_program(self.client, &[lib.to_string()], pgm)
                .await?
                .map(|p| crate::rpg::ProgramCode {
                    library: Some(p.library.clone()),
                    source: p.source,
                    options: p.options,
                    binding: p.binding,
                }),
            None => crate::rpg::source_from_path(pgm).map(|s| crate::rpg::ProgramCode {
                source: s,
                ..Default::default()
            }),
        }
        // A program that is not there: the command analyzer's CPD0170, then
        // CPF0006 (observed on IBM i 7.5 for CALL on a command line).
        .ok_or_else(|| {
            crate::cl::diag_error(
                "CPD0170",
                &format!(
                    "Program {pgm} was not found in library {}.",
                    library.unwrap_or("*LIBL")
                ),
            )
        })?;
        let r = crate::rpg::call_with(
            &self.env.database_url,
            self.library_list(),
            self.user.clone(),
            self.id.as_ref().map(|i| i.to_string()),
            self.display.clone(),
            Some(crate::rpg::ClContext {
                env: self.env.clone(),
                curlib: self.curlib.clone(),
                user_libl: self.libl.clone(),
                overrides: self.calls.overrides.clone(),
            }),
            pgm.to_string(),
            source,
            args,
        )
        .await?;
        self.calls.returned = Some(r.params.clone());
        let mut msgs: Vec<String> = r.messages.iter().map(|m| format!("DSPLY  {m}")).collect();
        // Without a job there are no spooled files: printed lines come back
        // as they would be on paper.
        msgs.extend(r.printed.iter().cloned());
        if let Some(e) = r.error {
            msgs.push(format!("{e:#}"));
            msgs.push(format!("Program {pgm} ended abnormally."));
            return Err(anyhow!(msgs.join("\n")));
        }
        Ok(Outcome::Messages(msgs))
    }
}
/// A whole number in a command parameter.
fn number(s: &str, keyword: &str) -> Result<u32> {
    s.trim().parse().map_err(|_| {
        anyhow!("CPF0006: The command has errors and was not run. {keyword} value '{s}' is not a number.")
    })
}

/// A service program's binding replaced (UPDSRVPGM).
async fn client_update_srvpgm(
    client: &tokio_postgres::Client,
    lib: &str,
    name: &str,
    json: &str,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.service_programs SET binding=$3 WHERE library=$1 AND name=$2",
            &[&lib, &name, &json],
        )
        .await?;
    crate::ile::binding_changed();
    Ok(())
}

/// A CALL parameter written with its type and length, `(value (*CHAR
/// 128))` (IBM CL reference, CALL PARM): the value, a character value
/// padded to the length. Other parameters as they are.
fn typed_parm(a: &str) -> String {
    let t = a.trim();
    let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) else {
        return a.to_string();
    };
    let inner = inner.trim();
    // The value: a quoted string or a word, then the (type length) group.
    let (value, rest) = if let Some(q) = inner.strip_prefix('\'') {
        let mut v = String::new();
        let mut chars = q.char_indices().peekable();
        let mut end = q.len();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().map(|x| x.1) == Some('\'') {
                    v.push('\'');
                    chars.next();
                    continue;
                }
                end = i + 1;
                break;
            }
            v.push(c);
        }
        (v, q[end..].trim())
    } else {
        match inner.split_once(char::is_whitespace) {
            Some((v, r)) => (v.to_string(), r.trim()),
            None => (inner.to_string(), ""),
        }
    };
    let ty: Vec<&str> = rest
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_whitespace()
        .collect();
    match ty.as_slice() {
        [t, len, ..] if t.eq_ignore_ascii_case("*CHAR") => {
            let n: usize = len.parse().unwrap_or(value.len());
            format!("{value:<n$}")
        }
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::dsp_label;

    #[test]
    fn display_labels_as_ibm_i_writes_them() {
        // Observed on IBM i 7.5 (PUB400 screens): DSPLIB's labels have their
        // colon in column 23 (offset 21 from column 2), DSPDTAARA's in 26.
        assert_eq!(dsp_label("Library", 19), "Library  . . . . . . :");
        assert_eq!(dsp_label("Type", 19), "Type . . . . . . . . :");
        assert_eq!(dsp_label("Create authority", 19), "Create authority . . :");
        assert_eq!(dsp_label("Number of objects", 19), "Number of objects  . :");
        assert_eq!(dsp_label("Data area", 22), "Data area . . . . . . . :");
        assert_eq!(dsp_label("  Library", 22), "  Library . . . . . . . :");
        assert_eq!(dsp_label("Type", 22), "Type  . . . . . . . . . :");
        assert_eq!(dsp_label("From", 10), "From  . . . :");
    }
}
