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
            "CHGUSRPRF" => self.chgusrprf(&cmd).await,
            "GO" => self.go(&cmd).await,
            "CRTSRCPF" => self.crtsrcpf(&cmd).await,
            "DSPFD" => self.dspfd(&cmd).await,
            "CRTPF" | "CRTLF" | "CRTDSPF" => self.crtfile(&cmd).await,
            "CRTBNDCL" | "CRTCLPGM" | "CRTBNDRPG" => self.crtpgm(&cmd).await,
            "CRTMNU" => self.crtmnu(&cmd).await,
            "DLTMNU" => self.dltmnu(&cmd).await,
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
                    bail!("CPF2110: Library {lib} not found.");
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
    fn create_target(&self, q: &str) -> (String, String) {
        match q.split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (self.current_library(), q.to_string()),
        }
    }

    /// The libraries a qualified name is looked for in, and the name.
    fn search(&self, q: &str) -> (Vec<String>, String) {
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
        let (libs, file) = self.search(cmd.get("SRCFILE"));
        let member = match cmd.get("SRCMBR") {
            "*FILE" | "*PGM" => object.to_string(),
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

    async fn crtsrcpf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        number(cmd.get("RCDLEN"), "RCDLEN")?;
        crate::source::create_file(self.client, &lib, &name, cmd.get("TEXT")).await?;
        let name = name.to_ascii_uppercase();
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Source file {name} created in library {lib}."
        )]))
    }

    /// DSPFD TYPE(*MBRLIST) of a source file.
    async fn dspfd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("FILE"));
        let name = name.to_ascii_uppercase();
        let mut found = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &name).await? {
                found = Some(l.clone());
                break;
            }
        }
        let Some(lib) = found else {
            bail!("CPF9812: File {name} in library *LIBL not found, or it is not a source file (DSPFD shows the members of source files only).");
        };
        authority::check(self.client, &self.user, &lib, &name, "*FILE", Aut::Use).await?;
        let members = crate::source::members(self.client, &lib, &name).await?;
        Ok(Outcome::Display(ListDisplay {
            title: "Display File Description".into(),
            info: vec![
                ("File".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Members".into(), members.len().to_string()),
            ],
            heading: format!("{:<10}  {:<10}  Text", "Member", "Type"),
            rows: members
                .iter()
                .map(|(m, t, x)| format!("{m:<10}  {t:<10}  {x}"))
                .collect(),
            ..Default::default()
        }))
    }

    /// CRTPF, CRTLF, CRTDSPF from a source member.
    async fn crtfile(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        let lib = lib.to_ascii_uppercase();
        let name = name.to_ascii_uppercase();
        let m = self.source_member(cmd, &name).await?;
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        let msg = match cmd.def.name {
            "CRTPF" => {
                let pf = crate::dds::parse_pf(&name, &m.source)?;
                catalog::create_physical_file(&mut c, &lib, &pf).await?;
                format!("File {name} created in library {lib}.")
            }
            "CRTLF" => {
                let lf = crate::dds::parse_lf(&name, &m.source)?;
                catalog::create_logical_file(&mut c, &self.library_list(), &lib, &lf).await?;
                format!("File {name} created in library {lib} over {}.", lf.based_on)
            }
            _ => {
                catalog::create_display_file(&mut c, &lib, &name, &m.source).await?;
                format!("Display file {name} created in library {lib}.")
            }
        };
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![msg]))
    }

    /// CRTBNDCL, CRTCLPGM, CRTBNDRPG from a source member. CL is checked
    /// here; RPG is checked by the runner when the program is called.
    async fn crtpgm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("PGM"));
        let lib = lib.to_ascii_uppercase();
        let name = name.to_ascii_uppercase();
        let m = self.source_member(cmd, &name).await?;
        let attribute = match cmd.def.name {
            "CRTBNDCL" => "CLLE",
            "CRTCLPGM" => "CLP",
            _ => "RPGLE",
        };
        if attribute == "RPGLE" && m.srctype == "RPG" {
            bail!(
                "Member {} is RPG/400 (RPG III) source; RPG III is not supported yet.",
                m.name
            );
        }
        // CL: includes are part of the program; then it is checked.
        let source = if attribute == "RPGLE" {
            // The program is checked now, as the compiler does: syntax,
            // names, types, files (docs/rpg-engine.md).
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            crate::rpg::compile(self.client, &libl, &name, &m.source).await?;
            m.source.clone()
        } else {
            let src =
                crate::source::expand_cl_includes(self.client, &self.library_list(), &m.source)
                    .await?;
            crate::clpgm::compile(&name, &src)?;
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
        authority::set_owner(self.client, &lib, &name, "*PGM", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Program {name} created in library {lib}."
        )]))
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
            .ok_or_else(|| anyhow!("CPF9801: Object {name} in library {lib} not found."))?;
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
        let fmt = |file: &str,
                   user: &str,
                   data: &str,
                   status: &str,
                   pages: &str,
                   job: &str,
                   nbr: &str| {
            format!(
                "{file:<10}  {user:<10}  {data:<10}  {status:<4}  {pages:>5}  {job:<24}  {nbr:>4}"
            )
        };
        let rows = files
            .iter()
            .map(|f| {
                fmt(
                    &f.file,
                    &f.user,
                    &f.user_data,
                    &f.status,
                    &f.pages.to_string(),
                    &f.job_name,
                    &f.number.to_string(),
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with All Spooled Files".into(),
            info: vec![("User".into(), user.clone())],
            heading: fmt("File", "User", "User data", "Sts", "Pages", "Job", "Nbr"),
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
        authority::check(
            self.client,
            &self.user,
            &d.library,
            &d.name,
            "*DTAARA",
            Aut::Use,
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
        let found = dtaara::resolve(
            self.client,
            cmd.get("DTAARA"),
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
            "Data area {} deleted from library {}.",
            d.name, d.library
        )]))
    }

    /// CALL: run a program with the job's library list. DSPLY lines come
    /// back as messages.
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
                        let program = crate::clpgm::compile(&name, &p.source)?;
                        crate::clpgm::run(self, &program, &args).await
                    }
                    _ => self.call_rpg(&name, Some(&p.library), args).await,
                }
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
                .map(|p| p.source),
            None => crate::rpg::source_from_path(pgm),
        }
        .ok_or_else(|| anyhow!("CPF4101: Program {pgm} not found."))?;
        let r = crate::rpg::call(
            &self.env.database_url,
            self.library_list(),
            self.user.clone(),
            self.id.as_ref().map(|i| i.to_string()),
            self.display.clone(),
            Some(crate::rpg::ClContext {
                env: self.env.clone(),
                curlib: self.curlib.clone(),
                user_libl: self.libl.clone(),
            }),
            pgm.to_string(),
            source,
            args,
        )
        .await?;
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
        anyhow!("CPF0006: Errors occurred in command. {keyword} value '{s}' is not a number.")
    })
}
