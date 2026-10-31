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
    /// A completion message the command sends when it prints its report
    /// (DSPMSGD: CPC9802), before the report.
    pub print_msg: Option<String>,
    /// Shown in the Display Spooled File viewer instead (DSPSPLF; DSPFD
    /// and DSPFFD on a display, whose report IBM i shows there).
    pub spool: Option<Box<SpoolView>>,
    /// Shown as DSPPFM shows a data member, its records as bytes
    /// (pfmview.rs).
    pub member: Option<Box<crate::pfmview::MemberView>>,
}

/// What the Display Spooled File viewer shows: the file's name and its
/// lines with their page and line numbers.
#[derive(Debug, Clone, Default)]
pub struct SpoolView {
    pub file: String,
    /// (page, line, text).
    pub lines: Vec<(usize, usize, String)>,
    /// The record width (132 for IBM's printer files QSYSPRT, QPDSPFD...).
    pub width: usize,
}

impl SpoolView {
    /// Lines on pages (as `spool::pages` gives them).
    pub fn of_pages(file: &str, pages: &[Vec<String>]) -> SpoolView {
        let mut lines = Vec::new();
        for (p, page) in pages.iter().enumerate() {
            for (l, t) in page.iter().enumerate() {
                lines.push((p + 1, l + 1, t.clone()));
            }
        }
        let width = lines
            .iter()
            .map(|(_, _, t)| t.chars().count())
            .max()
            .unwrap_or(0)
            .max(132);
        SpoolView {
            file: file.to_string(),
            lines,
            width,
        }
    }
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
    /// Everything is in the header lines: no list, no "Bottom" (DSPDTAARA
    /// of a *DEC or *LGL data area).
    pub no_list: bool,
    /// The system name at the right of line 1 (WRKACTJOB, WRKSBMJOB).
    pub system_row1: bool,
    /// No blank line between the options and the headings (WRKACTJOB).
    pub tight: bool,
    /// The title's column, when IBM i does not center it (PDM).
    pub title_col: Option<u8>,
    /// The system name alone on line 1 from this column (PDM).
    pub system_col1: Option<u8>,
    /// Input fields on the header lines (PDM's file, library and "Position
    /// to"), with their values.
    pub header_fields: Vec<HeaderField>,
    /// The header field whose value positions the list at the first row
    /// whose name (first word) is not lower.
    pub position_field: Option<String>,
    /// The command that shows the list for other header field values:
    /// `{NAME}` is the value of header field NAME.
    pub reload: Option<String>,
    /// The option lines as IBM i lays them out (from column 2), one set
    /// per F23 press; empty: the options joined three blanks apart.
    pub option_text: Vec<Vec<String>>,
    /// The function key lines (from column 2); empty: the usual keys.
    pub key_text: Vec<String>,
    /// Commands of options for one row (by row): (option, command). An
    /// option in `options` that a row's list lacks is not valid for it
    /// when the list is not empty.
    pub row_options: Vec<Vec<(u8, String)>>,
    /// Option 4 asks for a confirmation first (PDM).
    pub confirm_delete: bool,
    /// The cursor starts in the first row's option field.
    pub cursor_on_list: bool,
    /// The row shown first (WRKMSGD MSGID).
    pub start_at: usize,
}

/// An input field on a list display's header lines.
#[derive(Debug, Clone, Default)]
pub struct HeaderField {
    pub name: String,
    pub row: u8,
    pub col: u8,
    pub len: u16,
    pub value: String,
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

/// A library as the job's library list keeps it: the job's own QTEMP
/// library as QTEMP (`Job::library_list` gives the library again).
fn lib_entry(lib: &str) -> String {
    if crate::qtemp::is_qtemp(lib) {
        "QTEMP".to_string()
    } else {
        lib.to_string()
    }
}

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

/// CRTRPGMOD's condition names: DEFINE's and *CRTRPGMOD.
fn module_defines(cmd: &Command) -> String {
    let d = define_option(cmd);
    match d.strip_suffix(')') {
        Some(x) => format!("{x} *CRTRPGMOD)"),
        None => "DEFINE(*CRTRPGMOD)".into(),
    }
}

/// Where an RPG compile searches its /COPY and /INCLUDE files, as a
/// compile option recorded with the object (`rpg::IncludePath`): the
/// current directory, INCDIR (CRTSQLRPGI: INCDIR and COMPILEOPT's
/// INCDIR), the RPGINCDIR environment variable and, for a stream file
/// source, its directory (ILE RPG Programmer's Guide, "Search Path Within
/// The IFS"). None for a member compiled without INCDIR or RPGINCDIR,
/// which searches the library list as before.
async fn include_path_option(
    client: &Client,
    job: &str,
    cmd: &Command,
    m: &crate::source::Member,
) -> String {
    let unquote = |v: &str| v.trim().trim_matches('\'').trim_matches('"').to_string();
    let mut incdir: Vec<String> = Vec::new();
    if cmd.def.params.iter().any(|p| p.keyword == "INCDIR") {
        for v in cmd.list("INCDIR") {
            for part in v.split_whitespace() {
                let d = unquote(part);
                if !d.is_empty() && !d.eq_ignore_ascii_case("*NONE") {
                    incdir.push(d);
                }
            }
        }
    }
    if cmd.def.params.iter().any(|p| p.keyword == "COMPILEOPT") {
        for v in option_values(cmd.get("COMPILEOPT"), "INCDIR") {
            for part in v.split_whitespace() {
                let d = unquote(part);
                if !d.is_empty() && !d.eq_ignore_ascii_case("*NONE") {
                    incdir.push(d);
                }
            }
        }
    }
    let env: Vec<String> = crate::envvar::get(client, job, "RPGINCDIR")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.value)
        .map(|v| {
            v.split(':')
                .filter(|d| !d.trim().is_empty())
                .map(|d| d.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    if m.stmf.is_empty() && incdir.is_empty() && env.is_empty() {
        return String::new();
    }
    let mut dirs = vec![crate::ifs::current_dir()];
    for d in incdir.into_iter().chain(env) {
        if let Ok(n) = crate::ifs::normalize(&d) {
            dirs.push(n);
        }
    }
    if !m.stmf.is_empty() {
        if let Ok(n) = crate::ifs::normalize(&m.stmf) {
            let dir = match n.rfind('/') {
                Some(0) | None => "/".to_string(),
                Some(i) => n[..i].to_string(),
            };
            dirs.push(dir);
        }
    }
    crate::rpg::IncludePath {
        ifs_first: !m.stmf.is_empty(),
        dirs,
    }
    .option()
}

/// DEFINE(names) of a compile command, as compile options ("" for none).
fn define_option(cmd: &Command) -> String {
    let names: Vec<String> = cmd
        .list("DEFINE")
        .iter()
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .map(|n| {
            n.trim_matches(|c| c == '(' || c == ')' || c == '\'')
                .to_string()
        })
        .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("*NONE"))
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!("DEFINE({})", names.join(" "))
    }
}

/// INCDIR of CRTBNDCL/CRTCLMOD: the directories INCLUDE SRCSTMF searches
/// after the source's own.
fn cl_incdir(cmd: &Command) -> Vec<String> {
    crate::cmd::raw_items_text(cmd.get("INCDIR"))
        .into_iter()
        .filter(|d| !d.is_empty() && !d.starts_with('*'))
        .collect()
}

impl Job<'_> {
    /// The library list as searched: system part, current library, user
    /// part.
    pub fn library_list(&self) -> Vec<String> {
        self.library_list_entries()
            .into_iter()
            .map(|(l, _)| crate::qtemp::real(&l))
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
        crate::qtemp::real(self.curlib.as_deref().unwrap_or(DEFAULT_CURRENT_LIBRARY))
    }

    /// Run a command; what it says names the job's QTEMP library QTEMP.
    pub async fn run(&mut self, line: &str) -> Result<Outcome> {
        // Boxed: commands run commands (CALL, CL programs), and the
        // futures would otherwise nest on the stack.
        if crate::qtemp::current().is_none() {
            return Box::pin(self.run_command(line)).await;
        }
        let l = crate::qtemp::logical;
        let r = match Box::pin(self.run_command(line)).await {
            Ok(Outcome::Messages(m)) => Ok(Outcome::Messages(m.iter().map(|x| l(x)).collect())),
            Ok(Outcome::Display(mut d)) => {
                d.title = l(&d.title);
                d.heading = l(&d.heading);
                for (k, v) in d.info.iter_mut() {
                    *k = l(k);
                    *v = l(v);
                }
                for r in d
                    .rows
                    .iter_mut()
                    .chain(d.print.iter_mut())
                    .chain(d.keys.iter_mut())
                {
                    *r = l(r);
                }
                Ok(Outcome::Display(d))
            }
            Ok(o) => Ok(o),
            Err(e) => {
                let t = format!("{e:#}");
                let m = l(&t);
                if m == t {
                    Err(e)
                } else {
                    Err(anyhow!(m))
                }
            }
        };
        for p in self.calls.printed.iter_mut() {
            *p = l(p);
        }
        r
    }

    /// Where a command runs, as ALLOW names it: in a CL program (*IPGM or
    /// *BPGM), else on the job's command line or through QCMDEXC
    /// (*INTERACT or *BATCH, *EXEC); interactive when the job has a
    /// display.
    fn allowed_here(&self, allow: &[String]) -> bool {
        if allow.is_empty() {
            return true;
        }
        let has = |v: &str| allow.iter().any(|a| a == v);
        let interactive = self.display.is_some();
        if self.calls.level() > 0 {
            has(if interactive { "*IPGM" } else { "*BPGM" })
        } else {
            has(if interactive { "*INTERACT" } else { "*BATCH" })
                || has("*EXEC")
                || has("*IREXX")
                || has("*BREXX")
        }
    }

    async fn run_command(&mut self, line: &str) -> Result<Outcome> {
        if let Some(d) = self.user_command(line).await? {
            // ALLOW: where the command may run (CPD0031 then CPF0006,
            // PUB400 2026-10-26 tools/golden/verify1.txt v_allow: a
            // command with ALLOW(*BPGM *IPGM) run by `system`).
            if !self.allowed_here(&d.allow) {
                return Err(cl::diag_error(
                    "CPD0031",
                    &format!(
                        "Command {} cannot be run from here (see its ALLOW values).",
                        d.name
                    ),
                ));
            }
            return self.run_user_command(&d, line).await;
        }
        // DSPSPLF runs only where it can show the file (IBM i 7.5 refuses
        // it in `system` with CPD0031: PUB400 2026-10-26, tools/golden/
        // spool.txt s_dspsplf). Only there: a job run through the library
        // has no display either, and shows the viewer's rows.
        let first = line
            .trim()
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if first == "DSPSPLF"
            && crate::spool::print_job_mode()
            && self.display.is_none()
            && self.calls.level() == 0
        {
            return Err(cl::diag_error(
                "CPD0031",
                "Command DSPSPLF cannot be run from here (see its ALLOW values).",
            ));
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
        if let Some(o) = self.outq_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.qsh_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.jobd_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.pdm_command(&cmd).await? {
            return Ok(o);
        }
        if let Some(o) = self.objdesc_command(&cmd).await? {
            return Ok(o);
        }
        match cmd.def.name {
            "CRTLIB" => self.crtlib(&cmd).await,
            "CHGLIB" => self.chglib(&cmd).await,
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
            "CHGCMD" => self.chgcmd(&cmd).await,
            "DLTCMD" => self.dltcmd(&cmd).await,
            "CRTPNLGRP" => self.crtpnlgrp(&cmd).await,
            "DLTPNLGRP" => self.dltpnlgrp(&cmd).await,
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
            "RTVBNDSRC" => self.rtvbndsrc(&cmd).await,
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
                crate::rpg::interp::close_ended_spools(self.client).await;
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
            "CRTDTAQ" => self.crtdtaq(&cmd).await,
            "DLTDTAQ" => self.dltdtaq(&cmd).await,
            "CRTMSGQ" => self.crtmsgq(&cmd).await,
            "DLTMSGQ" => self.dltmsgq(&cmd).await,
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
            "CHGOBJD" => self.chgobjd(&cmd).await,
            // ALCOBJ, DLCOBJ: as4tainr has no object locks (database
            // records are locked as PostgreSQL rows): accepted.
            "ALCOBJ" | "DLCOBJ" => Ok(Outcome::Messages(Vec::new())),
            "SBMJOB" => self.sbmjob(&cmd).await,
            "WRKACTJOB" => self.wrkactjob().await,
            "WRKSBMJOB" => self.wrksbmjob().await,
            "DSPJOBLOG" => self.dspjoblog(&cmd).await,
            "DSPJOB" => self.dspjob(&cmd).await,
            "CHGJOB" => self.chgjob(&cmd).await,
            "ENDJOB" => self.endjob(&cmd).await,
            "SNDMSG" => self.sndmsg(&cmd).await,
            "DSPMSG" => self.dspmsg(&cmd).await,
            "SIGNOFF" => {
                // LOG(*LIST): the job log is kept as a spooled file.
                if cmd.get("LOG") == "*LIST" {
                    if let Some(id) = &self.id {
                        let lines = crate::spool::job_log_lines(
                            self.client,
                            id,
                            crate::joblog::Title::Ended,
                        )
                        .await?;
                        crate::spool::print_report(self.client, id, "QPJOBLOG", &id.name, &lines)
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
            "CPYSPLF" => self.cpysplf(&cmd).await,
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

    /// CHGLIB: the library's text and type (*SAME keeps them). The
    /// completion message of a change has no ID here (not captured).
    async fn chglib(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = cmd.get("LIB").to_ascii_uppercase();
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        authority::check(self.client, &self.user, "QSYS", &lib, "*LIB", Aut::Change).await?;
        let text = cmd.get("TEXT");
        let ty = cmd.get("TYPE");
        // Nothing to change: CPC2105 (PUB400 2026-10-23, CHGLIB with
        // TEXT(*SAME), tools/golden/qsh.txt q_chglib).
        if text.eq_ignore_ascii_case("*SAME") && ty.eq_ignore_ascii_case("*SAME") {
            return Ok(Outcome::Messages(vec![format!(
                "CPC2105: No change was made to {lib} in QSYS type *LIB."
            )]));
        }
        let n = self
            .client
            .execute(
                "UPDATE as4tainr.objects SET
                   text = CASE WHEN $2 = '*SAME' THEN text WHEN $2 = '*BLANK' THEN '' ELSE $2 END,
                   attribute = CASE WHEN $3 = '*SAME' THEN attribute ELSE substr($3, 2) END
                 WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB'",
                &[&lib, &text, &ty],
            )
            .await?;
        if n == 0 {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        Ok(Outcome::Messages(vec![format!("Library {lib} changed.")]))
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
            print_msg: None,
            spool: None,
            member: None,
        }))
    }

    async fn rmvlible(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = lib_entry(cmd.get("LIB"));
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
                self.curlib = Some(lib_entry(lib));
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
                    let l = lib_entry(&l);
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
                self.curlib = Some(lib_entry(lib));
            }
        }
        if let Some(v) = new_libl {
            self.libl = v;
        }
        Ok(Outcome::Messages(vec!["Library list changed.".into()]))
    }

    async fn addlible(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = lib_entry(cmd.get("LIB"));
        if !catalog::library_exists(self.client, &crate::qtemp::real(&lib)).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        if self.library_list().contains(&crate::qtemp::real(&lib)) {
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
        // A process with no job of the catalog: its job log for
        // QSYS2.JOBLOG_INFO('*').
        if self.id.is_none() && stmt.to_ascii_uppercase().contains("JOBLOG_INFO") {
            self.mirror_system_job_log().await;
        }
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
        let mut input = crate::sqlstmlist::Input {
            commit: cmd.get("COMMIT").to_ascii_uppercase(),
            naming: cmd.get("NAMING").to_ascii_uppercase(),
            errlvl,
            dftrdbcol: cmd.get("DFTRDBCOL").to_ascii_uppercase(),
            job_ccsid: "37".into(),
            ..Default::default()
        };
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
            let base = stmf.rsplit('/').next().unwrap_or(&stmf);
            let base = base.split('.').next().unwrap_or(base);
            input.name = base
                .chars()
                .take(10)
                .collect::<String>()
                .to_ascii_uppercase();
            input.source = vec![("Source stream file".into(), stmf.clone())];
            input.source_ccsid = ccsid.to_string();
            input.changed = crate::ifs::stat(self.client, &stmf)
                .await?
                .map(|e| stamp_of_us(e.changed_us));
            (text, format!("stream file {stmf}"))
        } else {
            if cmd.get("SRCFILE").is_empty() || cmd.get("SRCMBR").is_empty() {
                bail!("CPD0084: SRCFILE and SRCMBR, or SRCSTMF, are needed.");
            }
            let m = self.source_member(cmd, "").await?;
            let what = format!("member {} in {}/{}", m.name, m.library, m.file);
            input.name = m.name.clone();
            input.source = vec![
                (
                    "Source file".into(),
                    format!("{}/{}", crate::qtemp::logical(&m.library), m.file),
                ),
                ("Member".into(), m.name.clone()),
            ];
            input.source_ccsid = m.ccsid.to_string();
            input.changed = m.changed_us.map(stamp_of_us);
            (m.source, what)
        };
        input.lines = source.lines().map(str::to_string).collect();
        let schema = match cmd.get("DFTRDBCOL") {
            "*NONE" => None,
            s => Some(catalog::validate_name("library", s)?),
        };
        // COMMIT(*NONE) or *NC: no commitment control; else the script's
        // changes are a unit of work of its own.
        let commit = match cmd.get("COMMIT").to_ascii_uppercase().as_str() {
            "*NONE" | "*NC" => ScriptCommit::NoCommit,
            level => ScriptCommit::Level(level.to_string()),
        };
        let run = self
            .run_script(&source, &what, errlvl, schema, true, commit)
            .await?;
        // The listing (OUTPUT(*PRINT)); the statements' messages are in it
        // rather than sent to the caller (PUB400 2026-10-26, verify2
        // w_script, w_rb).
        let printed = crate::sqlstmlist::listing(&input, &run.msgs, &crate::report::Stamp::now());
        if !cmd.get("OUTPUT").eq_ignore_ascii_case("*NONE") {
            let name = input.name.clone();
            self.print_listing(&name, printed).await?;
        }
        if let Some(e) = run.failed {
            return Err(e);
        }
        // No completion message: `system` shows only the listing (PUB400
        // 2026-10-27, tools/golden/verify3.txt x_ok, and x_errors with
        // ERRLVL(30)).
        Ok(Outcome::Messages(vec![]))
    }

    /// Run the statements of an SQL script (RUNSQLSTM). `schema`: the
    /// default schema; with `set_schema` false, SET SCHEMA statements are
    /// skipped (`as4tainr import` keeps every object in its library), but
    /// for SET SCHEMA SESSION, which names the job's QTEMP and its declared
    /// temporary tables.
    pub async fn run_sql_script(
        &mut self,
        source: &str,
        what: &str,
        errlvl: u32,
        schema: Option<String>,
        set_schema: bool,
    ) -> Result<Vec<String>> {
        let run = self
            .run_script(source, what, errlvl, schema, set_schema, ScriptCommit::Auto)
            .await?;
        match run.failed {
            Some(e) => Err(e),
            None => Ok(run.lines),
        }
    }

    /// The statements of a script, run one after the other: a line for
    /// each (and a summary), the listing's messages, and the error that
    /// stopped the run (a statement that failed with a severity above
    /// `errlvl`).
    async fn run_script(
        &mut self,
        source: &str,
        what: &str,
        errlvl: u32,
        mut schema: Option<String>,
        set_schema: bool,
        commit: ScriptCommit,
    ) -> Result<ScriptRun> {
        let no_commit = commit == ScriptCommit::NoCommit;
        // Tables, indexes and views the unit of work made: a ROLLBACK
        // takes them back too (PUB400 2026-10-27, tools/golden/verify4.txt
        // y_rb4 and y_t5: the CREATE TABLE before the ROLLBACK is gone).
        let mut created: Vec<(&'static str, String, String)> = Vec::new();
        // The script's unit of work, under a commit level.
        let unit = match &commit {
            ScriptCommit::Level(l) => Some(
                crate::cmtctl::private(
                    self.client,
                    &self.env.database_url,
                    self.id.as_ref().map(|i| i.to_string()).as_deref(),
                    l,
                )
                .await?,
            ),
            _ => None,
        };
        let mut lines = Vec::new();
        let mut msgs: Vec<crate::sqlstmlist::Msg> = Vec::new();
        let mut failed = None;
        let mut errors = 0;
        let placed = crate::ddl::split_statements_at(source);
        let statements: Vec<String> = placed.iter().map(|p| p.0.clone()).collect();
        // INSERT, UPDATE and DELETE run on one SQL session, made again
        // after data definition (which can add libraries).
        let mut session: Option<(Vec<String>, crate::sql::Session)> = None;
        for (k, stmt) in statements.iter().enumerate() {
            let (record, position) = (placed[k].1, placed[k].2);
            let msg = |id: &str, severity: u32, text: &str| crate::sqlstmlist::Msg {
                id: id.to_string(),
                severity,
                record,
                position,
                text: text.to_string(),
            };
            if !set_schema
                && matches!(
                    crate::ddl::parse(stmt),
                    Ok(Some(crate::ddl::Ddl::SetSchema(ref s))) if !s.eq_ignore_ascii_case("QTEMP")
                )
            {
                lines.push(format!("Statement {}: SET SCHEMA skipped.", k + 1));
                continue;
            }
            let words: Vec<String> = stmt
                .trim()
                .trim_end_matches(';')
                .split_whitespace()
                .map(|w| w.to_ascii_uppercase())
                .collect();
            // Without commitment control (COMMIT(*NONE)), COMMIT, ROLLBACK
            // and SAVEPOINT are not valid: SQL7007, severity 30, which
            // stops the run at the default ERRLVL(10) (PUB400 2026-10-26,
            // verify2 w_rb and w_script).
            if no_commit
                && matches!(
                    words.first().map(String::as_str),
                    Some("COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE")
                )
                && (words[0] != "RELEASE" || words.get(1).map(String::as_str) == Some("SAVEPOINT"))
            {
                let e = anyhow::anyhow!(
                    "SQL7007: {} cannot be used when the statements run without commitment control.",
                    words[0]
                );
                errors += 1;
                lines.push(format!("Statement {}: {e:#}", k + 1));
                msgs.push(msg(
                    "SQL7007",
                    30,
                    crate::jobs::message_text(&format!("{e:#}")),
                ));
                if 30 > errlvl {
                    failed = Some(anyhow::anyhow!(
                        "SQL9010: RUNSQLSTM stopped at statement {} of {what}: {e:#}",
                        k + 1
                    ));
                    break;
                }
                continue;
            }
            // COMMIT and ROLLBACK under the script's commit level: its
            // unit of work (PUB400 2026-10-27, verify3 x_cmt: SQL7961, the
            // row before the ROLLBACK gone; SQL7960).
            let bare = |w: &str| {
                matches!(
                    words.iter().map(String::as_str).collect::<Vec<_>>().as_slice(),
                    [x] | [x, "WORK"] if *x == w
                )
            };
            if let Some(d) = &unit {
                if bare("COMMIT") || bare("ROLLBACK") {
                    let commit = bare("COMMIT");
                    let r = if commit {
                        d.commit().await
                    } else {
                        d.rollback().await
                    };
                    if commit {
                        created.clear();
                    } else {
                        self.undo_created(&mut created).await;
                    }
                    match r {
                        Ok(()) => {
                            let (id, text) = if commit {
                                ("SQL7960", "The changes were committed.")
                            } else {
                                ("SQL7961", "The changes were rolled back.")
                            };
                            lines.push(format!("Statement {}: {text}", k + 1));
                            msgs.push(msg(id, 0, text));
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            // COMMIT [WORK]: each statement here is committed when it
            // runs, so there is nothing left to commit (fidelity-gaps).
            if matches!(
                words
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
                ["COMMIT"] | ["COMMIT", "WORK"]
            ) {
                lines.push(format!("Statement {}: COMMIT done.", k + 1));
                msgs.push(msg("", 0, "COMMIT done."));
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
                            let mut s = match &unit {
                                Some(d) => {
                                    crate::sql::Session::open_in(
                                        self.client,
                                        &self.env.database_url,
                                        &libl,
                                        d.clone(),
                                    )
                                    .await?
                                }
                                None => {
                                    crate::sql::Session::open(
                                        self.client,
                                        &self.env.database_url,
                                        &libl,
                                    )
                                    .await?
                                }
                            };
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
                Ok(text) => {
                    if unit.is_some() {
                        use crate::ddl::Ddl;
                        let made = match crate::ddl::parse(stmt) {
                            Ok(Some(
                                Ddl::CreateTable { name, .. }
                                | Ddl::CreateTableLike { name, .. }
                                | Ddl::CreateTableAs { name, .. },
                            )) => Some(("TABLE", name)),
                            Ok(Some(Ddl::CreateIndex { name, .. })) => Some(("INDEX", name)),
                            Ok(Some(Ddl::CreateView { name, .. })) => Some(("VIEW", name)),
                            _ => None,
                        };
                        if let Some((kind, n)) = made {
                            let lib = n
                                .lib
                                .clone()
                                .or_else(|| schema.clone())
                                .unwrap_or_else(|| self.current_library());
                            created.push((kind, lib, n.name.clone()));
                        }
                    }
                    lines.push(format!("Statement {}: {text}", k + 1));
                    msgs.push(listing_message(&words, &text, &msg));
                }
                Err(e) => {
                    errors += 1;
                    let full = format!("{e:#}");
                    lines.push(format!("Statement {}: {full}", k + 1));
                    // SQL errors have severity 30.
                    let last = full.lines().last().unwrap_or("").to_string();
                    msgs.push(msg(
                        crate::jobs::message_id(&last),
                        30,
                        crate::jobs::message_text(&last),
                    ));
                    if 30 > errlvl {
                        failed = Some(anyhow::anyhow!(
                            "SQL9010: RUNSQLSTM stopped at statement {} of {what}: {full}",
                            k + 1
                        ));
                        break;
                    }
                }
            }
        }
        // The end of a script under a commit level: its changes are
        // committed (SQL7960 with no record: verify3 x_cmt), or rolled
        // back when an error stopped it (CL reference, RUNSQLSTM ERRLVL).
        if let Some(d) = &unit {
            if failed.is_some() {
                // SQL7961 with no record (verify4 y_rb4: the insert
                // before the error is gone).
                d.rollback().await?;
                self.undo_created(&mut created).await;
                msgs.push(crate::sqlstmlist::Msg {
                    id: "SQL7961".into(),
                    severity: 0,
                    record: 0,
                    position: 0,
                    text: "The changes were rolled back.".into(),
                });
            } else {
                d.commit().await?;
                msgs.push(crate::sqlstmlist::Msg {
                    id: "SQL7960".into(),
                    severity: 0,
                    record: 0,
                    position: 0,
                    text: "The changes were committed.".into(),
                });
            }
        }
        lines.push(format!(
            "{} statements run from {what}; {errors} with errors.",
            statements.len()
        ));
        Ok(ScriptRun {
            lines,
            msgs,
            failed,
        })
    }

    /// A ROLLBACK of a script's unit of work: the objects it made are
    /// dropped again, newest first (as4tainr runs data definition outside
    /// the transaction, so this is how it is taken back).
    async fn undo_created(&mut self, created: &mut Vec<(&'static str, String, String)>) {
        while let Some((kind, lib, name)) = created.pop() {
            let mut none = None;
            let _ = crate::ddl::try_run(
                &self.env.database_url,
                &self.user,
                &self.library_list(),
                Some(self.current_library()),
                &mut none,
                &format!("DROP {kind} {lib}.{name}"),
            )
            .await;
        }
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
            } else if let Some(i) = up
                .match_indices("ACTGRP(")
                .map(|(i, _)| i)
                .find(|&i| !up[..i].ends_with("DFT"))
                .filter(|_| up.contains("DFTACTGRP(*NO)"))
            {
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
        // DETAIL(*SRVPGM) printed (PUB400 2026-10-30, verify29 v29_psrv):
        // the bound service programs, *IMMED, their signatures. IBM i's
        // own runtime service programs (QRNXIE, QLEAWI...) are not
        // listed: as4tainr binds none.
        let mut print = Vec::new();
        let detail_srvpgm = cmd
            .list("DETAIL")
            .iter()
            .any(|d| d.trim().eq_ignore_ascii_case("*SRVPGM"));
        if detail_srvpgm {
            use crate::report::at;
            let owner: String = self
                .client
                .query_opt(
                    "SELECT owner FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
                    &[&lib, &name],
                )
                .await?
                .map(|r| r.get(0))
                .unwrap_or_default();
            let [h1, h2] = crate::report::header_b(
                45,
                "Display Program Information",
                &crate::report::system_name(),
                &crate::report::Stamp::now(),
                1,
            );
            print.push(h1);
            print.push(h2);
            print.push(at(&[
                (2, "Program  . . . . . . . :"),
                (29, &name),
                (69, "Library  . . . . . . . :"),
                (96, &lib),
            ]));
            print.push(at(&[(2, "Owner  . . . . . . . . :"), (29, &owner)]));
            print.push(at(&[(2, "Program attribute  . . :"), (29, &p.attribute)]));
            print.push(at(&[(2, "Detail . . . . . . . . :"), (29, "*SRVPGM")]));
            print.push(at(&[(2, "Service")]));
            print.push(at(&[
                (2, "Program"),
                (17, "Library"),
                (32, "Activation"),
                (47, "Signature"),
            ]));
            for (l, n) in &b.srvpgms {
                let sig = b
                    .srv_signatures
                    .get(&format!("{l}/{n}"))
                    .map(|s| crate::ile::signature_hex(s))
                    .unwrap_or_default();
                print.push(at(&[(2, n), (17, l), (32, "*IMMED"), (47, &sig)]));
            }
            print.push(crate::report::END_OBJD.into());
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Program Information".into(),
            print,
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

    /// RTVBNDSRC (CL reference, Retrieve Binder Source): binder language
    /// for a service program's current export block (its signature as
    /// 16 bytes in hexadecimal, its exports in order) or a module's
    /// exports, into a source member (SRCMBR *SRVPGM or *MODULE: named
    /// like the object; MBROPT *REPLACE or *ADD; the member is added
    /// with type BND when it is not there).
    async fn rtvbndsrc(&mut self, cmd: &Command) -> Result<Outcome> {
        let srvpgm = cmd.get("SRVPGM").trim().to_ascii_uppercase();
        let module = cmd.get("MODULE").trim().to_ascii_uppercase();
        let (object, olib, otype, lines) = if !matches!(srvpgm.as_str(), "" | "*NONE") {
            let (lib, name) = self.ile_object(&srvpgm, "*SRVPGM", Aut::Use).await?;
            let (_, b) =
                crate::ile::find_srvpgm(self.client, &self.library_list(), &lib, &name).await?;
            let mut spelled = std::collections::BTreeMap::new();
            for m in &b.modules {
                if let Ok(i) = crate::ile::module_info(self.client, &self.library_list(), m).await {
                    spelled.extend(i.spelled);
                }
            }
            let mut l = vec![format!(
                "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE(X'{}')",
                crate::ile::signature_hex(&b.signature)
            )];
            l.extend(binder_comment("*SRVPGM", &name, &lib));
            l.extend(
                b.exports
                    .iter()
                    .map(|e| format!("  EXPORT SYMBOL(\"{}\")", spelled.get(e).unwrap_or(e))),
            );
            l.push("ENDPGMEXP".into());
            (name, lib, "SRVPGM", l)
        } else if !matches!(module.as_str(), "" | "*NONE") {
            let (lib, name) = self.ile_object(&module, "*MODULE", Aut::Use).await?;
            let m = crate::ile::find_module(self.client, &self.library_list(), &lib, &name).await?;
            let info = crate::ile::module_info(self.client, &self.library_list(), &m).await?;
            let mut l = vec!["STRPGMEXP PGMLVL(*CURRENT)".to_string()];
            l.extend(binder_comment("*MODULE", &name, &lib));
            l.extend(
                info.exports
                    .iter()
                    .map(|e| format!("  EXPORT SYMBOL(\"{}\")", info.spelled.get(e).unwrap_or(e))),
            );
            l.push("ENDPGMEXP".into());
            (name, lib, "MODULE", l)
        } else {
            return Err(crate::cl::diag_error(
                "",
                "RTVBNDSRC needs a module (MODULE) or a service program (SRVPGM).",
            ));
        };
        let file_q = cmd.get("SRCFILE").to_ascii_uppercase();
        let (libs, file) = self.search(&file_q);
        let mut lib = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &file).await? {
                lib = Some(l.clone());
                break;
            }
        }
        let lib = lib.ok_or_else(|| {
            anyhow!(
                "CPF9812: There is no file {file} in library {}.",
                file_q.split_once('/').map(|x| x.0).unwrap_or("*LIBL")
            )
        })?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Change).await?;
        let mbr = match cmd.get("SRCMBR").trim().to_ascii_uppercase().as_str() {
            "*SRVPGM" | "*MODULE" | "" => object.clone(),
            m => m.to_string(),
        };
        let existing =
            crate::source::find_member(self.client, std::slice::from_ref(&lib), &file, &mbr)
                .await
                .ok();
        let mut source = String::new();
        let mut meta: Vec<String> = Vec::new();
        let (srctype, text) = match &existing {
            Some(e) => {
                if cmd.get("MBROPT").eq_ignore_ascii_case("*ADD") {
                    source.push_str(&e.source);
                    let sd = crate::source::member_seqdat(self.client, &lib, &file, &mbr).await?;
                    meta = crate::source::line_records(&e.source, sd.as_deref())
                        .iter()
                        .map(|(q, d, _)| crate::source::line_meta(q, d))
                        .collect();
                }
                (e.srctype.clone(), e.text.clone())
            }
            None => ("BND".to_string(), String::new()),
        };
        // Its records are numbered 0000.01, 0000.02... by their place in
        // the member (verify25 r25_rtvm_hex; verify24 q24_print_rtv2 after
        // MBROPT(*ADD)); their date bytes print as 00 and blanks there,
        // zero here (docs/fidelity-gaps.md).
        for l in &lines {
            source.push_str(l);
            source.push('\n');
            let n = meta.len() + 1;
            meta.push(crate::source::line_meta(
                &format!("{}.{:02}", n / 100, n % 100),
                "0",
            ));
        }
        crate::source::put_member(
            self.client,
            &crate::source::Member {
                library: lib.clone(),
                file: file.clone(),
                name: mbr.clone(),
                srctype,
                text,
                source,
                ..Default::default()
            },
            true,
        )
        .await?;
        crate::source::set_member_seqdat(self.client, &lib, &file, &mbr, &meta.join("\n")).await?;
        // Its messages (PUB400 2026-10-29, tools/golden/verify24.txt
        // q24_rtvbndsrc, q24_rtv_again, q24_rtv_mod): CPI5D12 when the
        // member was added, CPI5D11 naming the object, its library and
        // type, CPC5D01 with the member, file and library and the number
        // of objects included (one) and left out (none).
        let mut msgs = Vec::new();
        if existing.is_none() {
            msgs.push(format!(
                "CPI5D12: Member {mbr} was not there; it was added to the source file."
            ));
        }
        msgs.push(format!(
            "CPI5D11: The exports of {object} in library {}, type {otype}, were retrieved.",
            crate::qtemp::logical(&olib)
        ));
        msgs.push(format!(
            "CPC5D01: Binder language for 1 objects went to member {mbr} of file {file} in library {}; 0 objects were left out.",
            crate::qtemp::logical(&lib)
        ));
        Ok(Outcome::Messages(msgs))
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
        let print = self.srvpgm_print(cmd, &lib, &name, &b).await?;
        Ok(Outcome::Display(ListDisplay {
            title: "Display Service Program Information".into(),
            info,
            heading: String::new(),
            rows,
            print,
            ..Default::default()
        }))
    }

    /// An object's creation date as reports show it (the system's date
    /// format), if the object is there.
    async fn object_date(&self, lib: &str, name: &str, objtype: &str) -> Result<Option<String>> {
        Ok(self
            .client
            .query_opt(
                "SELECT extract(year FROM created)::int, extract(month FROM created)::int,
                        extract(day FROM created)::int
                   FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                &[&lib, &name, &objtype],
            )
            .await?
            .map(|r| {
                crate::report::Stamp::of(
                    (
                        r.get::<_, i32>(0),
                        r.get::<_, i32>(1) as u32,
                        r.get::<_, i32>(2) as u32,
                    ),
                    (0, 0, 0),
                )
                .date
            }))
    }

    /// DSPSRVPGM DETAIL(*SIGNATURE) or DETAIL(*PROCEXP) printed: the
    /// service program's header and its signatures (current first) or
    /// procedure exports. Layout: PUB400 (IBM i 7.5), tools/golden/
    /// verify28.txt u28_sigs, u28_exps (2026-10-30). Other details are
    /// not printed (docs/fidelity-gaps.md).
    async fn srvpgm_print(
        &self,
        cmd: &Command,
        lib: &str,
        name: &str,
        b: &crate::ile::Binding,
    ) -> Result<Vec<String>> {
        let details: Vec<String> = cmd
            .list("DETAIL")
            .iter()
            .map(|d| d.trim().to_ascii_uppercase())
            .collect();
        let wanted: Vec<&str> = details
            .iter()
            .map(String::as_str)
            .filter(|d| matches!(*d, "*MODULE" | "*SIGNATURE" | "*PROCEXP"))
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let (owner, attribute) = match self
            .client
            .query_opt(
                "SELECT owner, attribute FROM as4tainr.objects
                 WHERE library=$1 AND name=$2 AND objtype='*SRVPGM'",
                &[&lib, &name],
            )
            .await?
        {
            Some(r) => (r.get::<_, String>(0), r.get::<_, String>(1)),
            None => (String::new(), String::new()),
        };
        use crate::report::at;
        let stamp = crate::report::Stamp::now();
        let system = crate::report::system_name();
        let mut print = Vec::new();
        for (page, detail) in wanted.iter().enumerate() {
            let [h1, h2] = crate::report::header_b(
                41,
                "Display Service Program Information",
                &system,
                &stamp,
                page as u32 + 1,
            );
            print.push(h1);
            print.push(h2);
            print.push(format!(
                " Service program  . . . . . . . . . . . . :   {name}"
            ));
            print.push(format!(
                "   Library  . . . . . . . . . . . . . . . :     {lib}"
            ));
            print.push(format!(
                " Owner  . . . . . . . . . . . . . . . . . :   {owner}"
            ));
            print.push(format!(
                " Service program attribute  . . . . . . . :   {attribute}"
            ));
            print.push(format!(
                " Detail . . . . . . . . . . . . . . . . . :   {detail}"
            ));
            if *detail == "*MODULE" {
                // PUB400 2026-10-30, verify29 v29_smod: the module, its
                // library, attribute, creation date, *NONE optimization
                // and debug data *YES (as4tainr keeps no other levels).
                print.push(at(&[(47, "Creation"), (60, "Optimization"), (77, "Debug")]));
                print.push(at(&[
                    (2, "Module"),
                    (17, "Library"),
                    (32, "Attribute"),
                    (47, "Date"),
                    (63, "Level"),
                    (77, "Data"),
                ]));
                for m in &b.modules {
                    let date = self
                        .object_date(lib, &m.name, "*MODULE")
                        .await?
                        .or(self.object_date(lib, name, "*SRVPGM").await?)
                        .unwrap_or_default();
                    print.push(at(&[
                        (2, &m.name),
                        (17, lib),
                        (32, &m.attribute),
                        (47, &date),
                        (60, "*NONE"),
                        (77, "*YES"),
                    ]));
                }
            } else if *detail == "*SIGNATURE" {
                print.push(at(&[(61, "Signatures:")]));
                print.push(format!(" {}", crate::ile::signature_hex(&b.signature)));
                for s in &b.prv_signatures {
                    print.push(format!(" {}", crate::ile::signature_hex(s)));
                }
            } else {
                print.push(at(&[(58, "Procedure Exports:")]));
                print.push(at(&[(2, "Procedure Name"), (77, "ARGOPT")]));
                for e in &b.exports {
                    print.push(at(&[(2, e), (77, "*NO")]));
                }
            }
        }
        print.push(crate::report::END_OBJD.into());
        Ok(print)
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
        // Bound again as they were named (*LIBL stays *LIBL).
        let srvpgms: Vec<(String, String)> = old
            .srvpgms
            .iter()
            .map(|(l, n)| {
                let named = old.srv_libraries.get(&format!("{l}/{n}"));
                (named.cloned().unwrap_or_else(|| l.clone()), n.clone())
            })
            .collect();
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
        // An element may be in parentheses of its own: BNDDIR((LIB/DIR)).
        let dirs = cmd
            .list("BNDDIR")
            .into_iter()
            .map(|x| {
                x.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace())
                    .to_string()
            })
            .filter(|x| x != "*NONE" && !x.is_empty())
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
        // DEFINE(*CRTRPGMOD ...) and where the includes are found.
        let module_options = if cl {
            String::new()
        } else {
            let inc = include_path_option(self.client, &self.job_key(), cmd, &m).await;
            let o = format!("{} {inc}", module_defines(cmd)).trim().to_string();
            let snap = crate::rpg::snapshot_option(self.client, &libl, &m.source, &o).await?;
            format!("{o} {snap}").trim().to_string()
        };
        let mut severity = 0u8;
        let mut cl_listing = None;
        let mut m = m;
        if cl {
            let src = crate::source::expand_cl_includes_from(
                self.client,
                &self.library_list(),
                &m.source,
                &m.stmf,
                &cl_incdir(cmd),
            )
            .await?;
            cl_listing = Some(self.check_cl(cmd, &lib, &name, &m, &src, &libl).await?);
            m.source = src;
        } else {
            severity = self
                .compile_rpg(cmd, &lib, &name, &m, &libl, &module_options, true)
                .await?
                .1;
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
            &module_options,
        )
        .await?;
        crate::ile::set_source(self.client, &lib, &name, "*MODULE", &m).await?;
        authority::set_owner(self.client, &lib, &name, "*MODULE", &self.user).await?;
        if let Some((shown, analysis)) = &cl_listing {
            self.cl_listing(cmd, &lib, &name, shown, analysis, true)
                .await?;
            // CPC0816 (observed on IBM i 7.5).
            return Ok(Outcome::Messages(vec![format!(
                "CPC0816: Module {name} was created in library {lib}."
            )]));
        }
        // RNS9305 with the highest severity (observed on IBM i 7.5). Its
        // data starts with the module and its library (iRPGUnit's
        // RUCRTRPG reads them from the job log: CRTTST jobLogCallback);
        // what follows them is not known here (docs/fidelity-gaps.md).
        if !cl {
            let shown = if crate::qtemp::is_qtemp(&lib) {
                "QTEMP".to_string()
            } else {
                lib.clone()
            };
            let mut data = crate::rpg::data::e(&format!("{name:<10}{shown:<10}"));
            data.truncate(20);
            self.calls.message_data = Some(data);
            return Ok(Outcome::Messages(vec![format!(
                "RNS9305: Module {name} was created in library {lib}; highest severity {severity:02}."
            )]));
        }
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
        } else if !cmd.get("SRCSTMF").is_empty() {
            // The binder language in a stream file.
            let src = self.source_member(cmd, &name).await?;
            crate::ile::binder_language(&src.source).map_err(not_created)?
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
        // The completion message CPC5D0B (PUB400 2026-10-23), its data
        // the service program and its library (RPGUnit's RUCRTTST reads
        // them).
        let shown = if crate::qtemp::is_qtemp(&lib) {
            "QTEMP".to_string()
        } else {
            lib.clone()
        };
        let mut data = crate::rpg::data::e(&format!("{name:<10}{shown:<10}"));
        data.truncate(20);
        self.calls.message_data = Some(data);
        Ok(Outcome::Messages(vec![format!(
            "CPC5D0B: Service program {name} is now in library {lib}."
        )]))
    }

    /// CRTPGM: a program from modules; its entry module (ENTMOD, or the
    /// first with a main procedure) is what CALL runs.
    async fn crtpgm_bound(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("PGM"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        // A module not there: CPF3C50, the program not created (PUB400
        // 2026-10-30, verify28 u28_pgmu).
        let mut modules = self.module_list(cmd, &lib, &name).await.map_err(|e| {
            if crate::jobs::escape_id(&format!("{e:#}")) == "CPF5D05" {
                anyhow!("CPF3C50: Program {name} was not created: a module it names is not there.")
            } else {
                e
            }
        })?;
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
        // CPC5D07 (PUB400 2026-10-30, verify29 v29_pgmu).
        Ok(Outcome::Messages(vec![format!(
            "CPC5D07: Program {name} was created in library {lib}."
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
            crate::source::delete_file(self.client, &lib, &name).await?;
            authority::forget(self.client, &lib, &name, "*FILE").await?;
            return Ok(Outcome::Messages(vec![format!(
                "CPC2191: Object {name} of type *FILE in library {lib} was deleted."
            )]));
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
        let outq = cmd.get("OUTQ").to_ascii_uppercase();
        if !outq.is_empty() && outq != "*WRKSTN" {
            crate::outq::find_queue(self.client, &outq, &self.library_list()).await?;
        }
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
        if !outq.is_empty() {
            crate::outq::set_user_outq(self.client, &name, &outq, &self.library_list()).await?;
        }
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
        // IBM i 7.5's DSPUSRPRF display (PUB400): "Display User Profile -
        // Basic", one attribute to a line, labels with their colon in
        // column 45, paged under the "User profile" line.
        let line = |label: &str, value: &str| format!("{}   {value}", dsp_label(label, 41));
        let (inlpgm_lib, inlpgm) = match p.inlpgm.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (String::new(), p.inlpgm.clone()),
        };
        let (inlmnu_lib, inlmnu) = match p.inlmnu.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (String::new(), p.inlmnu.clone()),
        };
        let mut rows = vec![
            line("Previous sign-on", p.last_signon.as_deref().unwrap_or("")),
            line("Status", &p.status),
            line("Password is *NONE", &yes_no(p.no_password)),
            line("Password set expired by command", &yes_no(p.pwd_expired)),
            line("User class", &p.usrcls),
            line("Creation date/time", &p.created),
        ];
        let special = if p.special_auth.is_empty() {
            vec!["*NONE".to_string()]
        } else {
            p.special_auth.clone()
        };
        for (i, a) in special.iter().enumerate() {
            if i == 0 {
                rows.push(line("Special authority", a));
            } else {
                rows.push(format!("{:47}{a}", ""));
            }
        }
        rows.push(line("Current library", &p.curlib));
        rows.push(line("Initial program", &inlpgm));
        if !inlpgm_lib.is_empty() {
            rows.push(format!("{}     {inlpgm_lib}", dsp_label("  Library", 41)));
        }
        rows.push(line("Initial menu", &inlmnu));
        if !inlmnu_lib.is_empty() {
            rows.push(format!("{}     {inlmnu_lib}", dsp_label("  Library", 41)));
        }
        rows.push(line("Text", &p.text));
        Ok(Outcome::Display(ListDisplay {
            title: "Display User Profile - Basic".into(),
            info: vec![],
            heading: String::new(),
            rows,
            ibm: IbmLayout::some(IbmLayout {
                lines: vec![line("User profile", &p.name)],
                enter_line: true,
                ..Default::default()
            }),
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
        let outq = cmd.get("OUTQ").to_ascii_uppercase();
        if !outq.is_empty() && outq != "*SAME" && outq != "*WRKSTN" {
            crate::outq::find_queue(self.client, &outq, &self.library_list()).await?;
        }
        if !users::change(self.client, &name, &changes).await? {
            bail!("CPF2204: User profile {name} not found.");
        }
        if !outq.is_empty() && outq != "*SAME" {
            crate::outq::set_user_outq(self.client, &name, &outq, &self.library_list()).await?;
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
            Some((l, n)) => (vec![crate::qtemp::real(l)], n.to_string()),
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
            let changed_us = crate::ifs::stat(self.client, stmf)
                .await?
                .map(|e| e.changed_us);
            return Ok(crate::source::Member {
                library: String::new(),
                file: String::new(),
                name: object.to_string(),
                srctype: ext,
                text: String::new(),
                source: text,
                stmf: stmf.to_string(),
                ccsid,
                changed_us,
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
                    anyhow::anyhow!("CPF9812: There is no file {file} in library *LIBL.")
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
            bail!("CPF9812: There is no file {file} in library {lib}.");
        }
        Ok(Outcome::Messages(vec![format!(
            "CPC3201: Member {mbr} of file {file} in library {lib} was changed."
        )]))
    }

    async fn crtsrcpf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("FILE"));
        let rcdlen = number(cmd.get("RCDLEN"), "RCDLEN")?;
        crate::source::create_file(self.client, &lib, &name, cmd.get("TEXT")).await?;
        let name = name.to_ascii_uppercase();
        crate::source::set_record_length(self.client, &lib, &name, rcdlen as i32).await?;
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        // MBR: a member added with the file (*FILE: named like it).
        let mbr = match cmd.get("MBR") {
            "*NONE" | "" => None,
            "*FILE" => Some(name.clone()),
            m => Some(m.to_ascii_uppercase()),
        };
        let mut msgs = vec![format!(
            "CPC7301: File {name} was created in library {lib}."
        )];
        if let Some(m) = mbr {
            crate::source::put_member(
                self.client,
                &crate::source::Member {
                    library: lib.clone(),
                    file: name.clone(),
                    name: m.clone(),
                    ccsid: 37,
                    ..Default::default()
                },
                false,
            )
            .await?;
            // The member added: CPC7305 after CPC7301 (PUB400 2026-10-28,
            // tools/golden/verify12.txt n12_src).
            msgs.push(format!(
                "CPC7305: Member {m} was added to file {name} in library {lib}."
            ));
        }
        Ok(Outcome::Messages(msgs))
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
                "CPF9812: There is no file {name} in library {}.",
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
            let source = crate::ddsref::expand_from_catalog(
                self.client,
                &self.library_list(),
                &m.source,
                crate::ddsref::Kind::Printer,
            )
            .await
            .map_err(|e| {
                anyhow!("{e:#}\nCPF7302: File {name} was not created in library {lib}.")
            })?;
            (source, m.text)
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
                ..Default::default()
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
                // Reference fields (R in position 29) take their attributes
                // from the files REF and REFFLD name (ddsref.rs).
                let expanded = crate::ddsref::expand_from_catalog(
                    self.client,
                    &self.library_list(),
                    &m.source,
                    crate::ddsref::Kind::Database,
                )
                .await;
                let parsed = expanded.and_then(|src| crate::dds::parse_pf(&name, &src));
                let mut pf = match parsed {
                    Ok(pf) => pf,
                    Err(e) if !rcdlen => {
                        return Err(self.dds_failed(cmd, &m, &lib, &name, true, e).await);
                    }
                    Err(e) => return Err(e),
                };
                match cmd.get("TEXT") {
                    "" | "*SRCMBRTXT" => {}
                    "*BLANK" => pf.text = String::new(),
                    t => pf.text = t.to_string(),
                }
                if let Err(e) = catalog::create_physical_file(&mut c, &lib, &pf).await {
                    if !rcdlen {
                        self.dds_listing(cmd, &m, &lib, &name, true, &[], &finals_of(&e), false)
                            .await?;
                    }
                    return Err(e);
                }
                if !rcdlen {
                    let done = format!("File {name} was created in library {lib}.");
                    self.dds_listing(
                        cmd,
                        &m,
                        &lib,
                        &name,
                        true,
                        &[],
                        &[("CPC7301".into(), 0, done)],
                        true,
                    )
                    .await?;
                }
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
                let lf = match crate::dds::parse_lf(&name, &m.source) {
                    Ok(lf) => lf,
                    Err(e) => return Err(self.dds_failed(cmd, &m, &lib, &name, false, e).await),
                };
                if let Err(e) =
                    catalog::create_logical_file(&mut c, &self.library_list(), &lib, &lf).await
                {
                    self.dds_listing(cmd, &m, &lib, &name, false, &[], &finals_of(&e), false)
                        .await?;
                    return Err(e);
                }
                let done = format!(
                    "File {name} was created in library {lib} over {}.",
                    lf.based_on
                );
                self.dds_listing(
                    cmd,
                    &m,
                    &lib,
                    &name,
                    false,
                    &[],
                    &[("CPC7301".into(), 0, done)],
                    true,
                )
                .await?;
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
                let source = crate::ddsref::expand_from_catalog(
                    self.client,
                    &self.library_list(),
                    &m.source,
                    crate::ddsref::Kind::Device,
                )
                .await?;
                if cmd.get("REPLACE") == "*NO" {
                    catalog::create_display_file(&mut c, &lib, &name, &source).await?;
                } else {
                    catalog::replace_display_file(&mut c, &lib, &name, &source).await?;
                }
                // CPC7301, as for a physical file (PUB400 2026-10-30,
                // verify29 v29_crtd).
                format!("CPC7301: File {name} was created in library {lib}.")
            }
        };
        authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        Ok(Outcome::Messages(vec![msg]))
    }

    /// CRTPF's and CRTLF's DDS listing (ddslist.rs), printed as IBM i 7.5
    /// prints it for a file created, one that exists already and DDS with
    /// errors (PUB400 2026-10-27, verify5 z_crtpf, z_crtpf_dup, z_crtlf,
    /// z_crtpf_bad).
    #[allow(clippy::too_many_arguments)]
    async fn dds_listing(
        &mut self,
        cmd: &Command,
        m: &crate::source::Member,
        lib: &str,
        name: &str,
        physical: bool,
        msgs: &[crate::ddslist::Msg],
        finals: &[(String, u32, String)],
        created: bool,
    ) -> Result<()> {
        let given: Vec<String> = cmd
            .list("OPTION")
            .iter()
            .flat_map(|v| {
                v.split_whitespace()
                    .map(|x| x.to_ascii_uppercase())
                    .collect::<Vec<_>>()
            })
            .collect();
        let has = |a: &[&str]| given.iter().any(|g| a.contains(&g.as_str()));
        let options = vec![
            if has(&["*NOSRC", "*NOSOURCE"]) {
                "*NOSOURCE"
            } else {
                "*SOURCE"
            }
            .to_string(),
            if has(&["*NOLIST"]) {
                "*NOLIST"
            } else {
                "*LIST"
            }
            .to_string(),
            if has(&["*SECLVL"]) {
                "*SECLVL"
            } else {
                "*NOSECLVL"
            }
            .to_string(),
            if has(&["*EVENTF"]) {
                "*EVENTF"
            } else {
                "*NOEVENTF"
            }
            .to_string(),
        ];
        let text = match cmd.get("TEXT") {
            "" | "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let input = crate::ddslist::Input {
            name: name.to_string(),
            library: lib.to_string(),
            attribute: if physical { "Physical" } else { "Logical" }.into(),
            srcfile: (m.library.clone(), m.file.clone()),
            member: m.name.clone(),
            changed: m.changed_us.map(stamp_of_us),
            options,
            genlvl: "20".into(),
            flag: "00".into(),
            filetype: "*DATA".into(),
            aut: "*LIBCRTAUT".into(),
            replace: "*NO".into(),
            text,
            lines: m.source.lines().map(str::to_string).collect(),
            physical,
        };
        let printed =
            crate::ddslist::listing(&input, msgs, finals, created, &crate::report::Stamp::now());
        if !printed.is_empty() {
            self.print_listing(name, printed).await?;
        }
        Ok(())
    }

    /// DDS with an error: the listing with its message, then CPF7311 and
    /// CPF7302 (PUB400 2026-10-27, verify5 z_crtpf_bad; the DDS message is
    /// only in the listing there, as4tainr also puts it in CPF7311's text).
    async fn dds_failed(
        &mut self,
        cmd: &Command,
        m: &crate::source::Member,
        lib: &str,
        name: &str,
        physical: bool,
        e: anyhow::Error,
    ) -> anyhow::Error {
        let msg = crate::ddslist::message_of(&e);
        let not = format!("File {name} was not created in library {lib}.");
        if let Err(p) = self
            .dds_listing(
                cmd,
                m,
                lib,
                name,
                physical,
                std::slice::from_ref(&msg),
                &[("CPF7302".into(), 40, not.clone())],
                false,
            )
            .await
        {
            return p;
        }
        let at = if msg.line > 0 {
            format!(" (line {}: {})", msg.line, msg.text)
        } else {
            format!(" ({})", msg.text)
        };
        anyhow!(
            "CPF7311: The DDS has errors of severity {} above the generation level 20{at}.\nCPF7302: {not}",
            msg.sev
        )
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
            let inc = include_path_option(self.client, &self.job_key(), cmd, &m).await;
            let options = format!("SQLCOMMIT({}) {inc}", cmd.get("COMMIT"))
                .trim()
                .to_string();
            let snap = crate::rpg::snapshot_option(self.client, &libl, &m.source, &options).await?;
            let options = format!("{options} {snap}").trim().to_string();
            crate::rpg::compile_with(self.client, &libl, &name, &m.source, &options).await?;
            let text = match cmd.get("TEXT") {
                "*SRCMBRTXT" => m.text.clone(),
                "*BLANK" => String::new(),
                t => t.to_string(),
            };
            let mut c = runtime::connect_db(&self.env.database_url).await?;
            crate::ile::store(
                &mut c, &lib, &name, "*MODULE", "SQLRPGLE", &text, &m.source, &options,
            )
            .await?;
            crate::ile::set_source(self.client, &lib, &name, "*MODULE", &m).await?;
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
        let mut rpg3_original: Option<crate::source::Member> = None;
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
            rpg3_original = Some(m.clone());
            m.source = c.source;
        }
        let mut binding: Option<crate::ile::Binding> = None;
        let mut cl_listing: Option<(crate::source::Member, crate::cllist::Analysis)> = None;
        let mut rpg_severity = 0u8;
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
        if cmd.def.name == "CRTBNDRPG" {
            options = format!("{options} {}", define_option(cmd))
                .trim()
                .to_string();
        }
        // CRTBNDRPG DFTACTGRP(*NO) ACTGRP(...): the activation group the
        // program runs in (the source's control keywords win).
        if cmd.def.name == "CRTBNDRPG" && cmd.get("DFTACTGRP") == "*NO" {
            options = format!("{options} DFTACTGRP(*NO) ACTGRP({})", cmd.get("ACTGRP"))
                .trim()
                .to_string();
        }
        // Where its includes are found (RPG).
        if matches!(attribute, "RPGLE" | "SQLRPGLE") {
            let inc = include_path_option(self.client, &self.job_key(), cmd, &m).await;
            options = format!("{options} {inc}").trim().to_string();
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            let snap = crate::rpg::snapshot_option(self.client, &libl, &m.source, &options).await?;
            options = format!("{options} {snap}").trim().to_string();
        }
        // CL: includes are part of the program; then it is checked.
        let source = if attribute == "RPGLE" || attribute == "SQLRPGLE" || attribute == "RPG" {
            // The program is checked now, as the compiler does: syntax,
            // names, types, files (docs/rpg-engine.md).
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            // CRTBNDRPG prints its listing unless OUTPUT(*NONE); CRTSQLRPGI
            // only with OUTPUT(*PRINT) (its default is *NONE).
            let listed = match cmd.def.name {
                "CRTBNDRPG" => true,
                "CRTSQLRPGI" => cmd.get("OUTPUT") == "*PRINT",
                _ => false,
            };
            let (prog, sev) = self
                .compile_rpg(cmd, &lib, &name, &m, &libl, &options, listed)
                .await?;
            rpg_severity = sev;

            // Procedures the program calls that are elsewhere: bound from
            // the binding directories (BNDDIR, ctl-opt BNDDIR).
            if !prog.imports.is_empty() || !prog.data_imports.is_empty() {
                let mut dirs: Vec<String> = cmd
                    .list("BNDDIR")
                    .into_iter()
                    .filter(|d| d != "*NONE")
                    .collect();
                // CRTSQLRPGI passes COMPILEOPT's options to the compiler
                // (BNDDIR, DFTACTGRP...: CL reference, CRTSQLRPGI).
                let compileopt = if cmd.def.name == "CRTSQLRPGI" {
                    cmd.get("COMPILEOPT").to_ascii_uppercase()
                } else {
                    String::new()
                };
                dirs.extend(
                    option_values(&compileopt, "BNDDIR")
                        .into_iter()
                        .filter(|d| d != "*NONE"),
                );
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
                    None if !option_values(&compileopt, "DFTACTGRP").is_empty() => {
                        option_values(&compileopt, "DFTACTGRP")[0] == "*YES"
                    }
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
            let src = crate::source::expand_cl_includes_from(
                self.client,
                &self.library_list(),
                &m.source,
                &m.stmf,
                &cl_incdir(cmd),
            )
            .await?;
            let mut libl = self.library_list();
            if !libl.contains(&lib) {
                libl.insert(0, lib.clone());
            }
            let (shown, analysis) = self.check_cl(cmd, &lib, &name, &m, &src, &libl).await?;
            cl_listing = Some((shown, analysis));
            // ILE CL: DFTACTGRP, ACTGRP, BNDDIR from the command, which
            // DCLPRCOPT in the source overrides, and DCLPRCOPT's
            // BNDSRVPGM (CL reference, DCLPRCOPT). A program not in the
            // default activation group is bound to the procedures it
            // calls (CALLPRC).
            if cmd.def.name == "CRTBNDCL" {
                let o = crate::clpgm::dclprcopt(&src);
                let get = |k: &str| -> String {
                    o.get(k)
                        .map(|v| v.trim().to_ascii_uppercase())
                        .unwrap_or_else(|| cmd.get(k).to_ascii_uppercase())
                };
                if get("DFTACTGRP") == "*NO" {
                    let g = match get("ACTGRP").as_str() {
                        "" | "*STGMDL" => "QILE".to_string(),
                        g => g.to_string(),
                    };
                    options = format!("{options} DFTACTGRP(*NO) ACTGRP({g})")
                        .trim()
                        .to_string();
                    let mut dirs: Vec<String> = cmd
                        .list("BNDDIR")
                        .into_iter()
                        .filter(|d| !d.starts_with('*'))
                        .collect();
                    if let Some(v) = o.get("BNDDIR") {
                        dirs.extend(
                            crate::cmd::raw_items_text(v)
                                .into_iter()
                                .filter(|d| !d.starts_with('*'))
                                .map(|d| d.to_ascii_uppercase()),
                        );
                    }
                    // BNDSRVPGM((LIB/NAME *IMMED) ...) or (NAME).
                    let srv: Vec<(String, String)> = o
                        .get("BNDSRVPGM")
                        .map(|v| crate::cmd::raw_items_text(v))
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|x| !x.starts_with('*'))
                        .map(|x| {
                            let x = x.to_ascii_uppercase();
                            match x.split_once('/') {
                                Some((l, n)) => (l.to_string(), n.to_string()),
                                None => ("*LIBL".to_string(), x),
                            }
                        })
                        .collect();
                    let entry = crate::ile::ModuleCopy {
                        name: name.clone(),
                        source: src.clone(),
                        options: String::new(),
                        attribute: "CLLE".to_string(),
                    };
                    let info = crate::ile::module_info(self.client, &libl, &entry).await?;
                    // A procedure the binder cannot find (one of the
                    // system's that as4tainr does not have, such as
                    // getpwuid_r) leaves the program unbound, as before
                    // ILE CL binding was done: its CALLPRC fails when it
                    // runs (fidelity-gaps, DCLPRCOPT).
                    if !info.imports.is_empty() {
                        if let Ok((mut b, _)) =
                            crate::ile::bind(self.client, &libl, vec![entry], &srv, &dirs).await
                        {
                            b.modules.retain(|x| x.name != name);
                            b.actgrp = g.clone();
                            binding = Some(b);
                        }
                    }
                }
            }
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
        crate::ile::set_source(self.client, &lib, &name, "*PGM", &m).await?;
        crate::ile::set_program_binding(self.client, &lib, &name, binding.as_ref()).await?;
        authority::set_owner(self.client, &lib, &name, "*PGM", &self.user).await?;
        // A CL program: CPC0815; an ILE RPG program: RNS9304 with the
        // highest severity (observed on IBM i 7.5).
        // CRTRPGPGM: the RPG/400 listing and QRG0003 (observed on IBM i
        // 7.5).
        if let Some(orig) = &rpg3_original {
            self.rpg3_listing(cmd, &lib, &name, orig, true).await?;
            return Ok(Outcome::Messages(vec![format!(
                "QRG0003: Program {name} was created in library {lib}; highest severity 00."
            )]));
        }
        if matches!(cmd.def.name, "CRTBNDRPG" | "CRTSQLRPGI") {
            return Ok(Outcome::Messages(vec![format!(
                "RNS9304: Program {name} was created in library {lib}; highest severity {rpg_severity:02}."
            )]));
        }
        if let Some((shown, analysis)) = &cl_listing {
            self.cl_listing(cmd, &lib, &name, shown, analysis, true)
                .await?;
        }
        let id = if matches!(cmd.def.name, "CRTBNDCL" | "CRTCLPGM") {
            "CPC0815: "
        } else {
            ""
        };
        Ok(Outcome::Messages(vec![format!(
            "{id}Program {name} was created in library {lib}."
        )]))
    }

    /// The CL compiler's checks (cllist.rs) on a source (its includes
    /// expanded), then the program's own compile. When a message has
    /// severity 30 or more the listing is printed and the program is not
    /// created: CPF0820 (observed on IBM i 7.5); a module: the message
    /// without an ID (not captured).
    async fn check_cl(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        m: &crate::source::Member,
        src: &str,
        libl: &[String],
    ) -> Result<(crate::source::Member, crate::cllist::Analysis)> {
        let files = crate::clpgm::resolve_files(self.client, libl, src)
            .await
            .unwrap_or_default();
        let user_cmds: Vec<(String, String)> = self
            .client
            .query(
                "SELECT library, name FROM as4tainr.objects WHERE objtype='*CMD'",
                &[],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        // A command: a system one, or one of the library list's (LIB/CMD:
        // of that library).
        let known = |c: &str| {
            let (lib, bare) = match c.rsplit_once('/') {
                Some((l, b)) => (Some(crate::qtemp::real(l)), b),
                None => (None, c),
            };
            match &lib {
                Some(l) if l != "QSYS" && l != "*LIBL" => {
                    user_cmds.iter().any(|(ul, u)| ul == l && u == bare)
                        || l == "*SYSTEM"
                            && (crate::cl::find(bare).is_some() || crate::ibmcmds::is_ibm(bare))
                }
                _ => {
                    crate::cl::find(bare).is_some()
                        || crate::ibmcmds::is_ibm(bare)
                        || user_cmds
                            .iter()
                            .any(|(ul, u)| u == bare && libl.contains(ul))
                }
            }
        };
        let mut analysis =
            crate::cllist::analyze(src, &known, &|st| crate::clpgm::dclf_variables(st, &files));
        if analysis.max_sev() < 30 {
            if let Err(e) = crate::clpgm::compile_with(name, src, &files) {
                let text = format!("{e:#}");
                let line = text
                    .strip_prefix("line ")
                    .and_then(|r| r.split_once(':'))
                    .and_then(|(n, _)| n.trim().parse().ok())
                    .unwrap_or(0);
                let id = crate::jobs::message_id(&text).to_string();
                let text = if id.is_empty() {
                    text
                } else {
                    text[id.len() + 1..].trim().to_string()
                };
                analysis.diags.push(crate::cllist::Diag {
                    line,
                    id,
                    sev: 30,
                    text,
                });
            }
        }
        let mut shown = m.clone();
        shown.source = src.to_string();
        if analysis.max_sev() >= 30 {
            self.cl_listing(cmd, lib, name, &shown, &analysis, false)
                .await?;
            if cmd.def.name == "CRTCLMOD" {
                bail!("Module {name} was not created in library {lib}.");
            }
            bail!("CPF0820: Program {name} was not created.");
        }
        Ok((shown, analysis))
    }

    /// A CL compiler listing (OUTPUT(*PRINT), the default): a spooled
    /// file named after the program, and the printed output `system`
    /// shows.
    async fn cl_listing(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        member: &crate::source::Member,
        analysis: &crate::cllist::Analysis,
        created: bool,
    ) -> Result<()> {
        if cmd.get("OUTPUT") == "*NONE" {
            return Ok(());
        }
        let or = |k: &str, d: &str| -> String {
            match cmd.get(k) {
                "" => d.to_string(),
                v => v.to_string(),
            }
        };
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" | "" => member.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let p = crate::cllist::Prologue {
            program: name.to_string(),
            library: lib.to_string(),
            member: member.clone(),
            dftactgrp: or("DFTACTGRP", "*YES"),
            actgrp: or("ACTGRP", "*ENTMOD"),
            replace: or("REPLACE", "*YES"),
            text,
            dbgview: or("DBGVIEW", "*STMT"),
            module: cmd.def.name == "CRTCLMOD",
        };
        let lines = crate::cllist::listing(&p, analysis, created, &crate::report::Stamp::now());
        self.print_listing(name, lines).await
    }

    /// Compile an ILE RPG source with the compiler's checks
    /// (rpg/check.rs), print its listing when `listed` (rpg/listing.rs),
    /// and fail as the compiler does when a message is more severe than
    /// the generation severity level (GENLVL 10): RNS9308 and RNS9310.
    /// The program and the highest severity.
    #[allow(clippy::too_many_arguments)]
    async fn compile_rpg(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        m: &crate::source::Member,
        libl: &[String],
        options: &str,
        listed: bool,
    ) -> Result<(crate::rpg::program::Program, u8)> {
        let (parsed, compiled) =
            crate::rpg::compile_parts(self.client, libl, name, &m.source, options, &[]).await;
        let mut checked = crate::rpg::check::Checked::default();
        if let (Some(src), Ok(prog)) = (&parsed, &compiled) {
            checked = crate::rpg::check::check(src, prog, &m.source);
        }
        if let Err(e) = &compiled {
            checked.diags.push(crate::rpg::check::Diag {
                line: 0,
                id: crate::jobs::message_id(&format!("{e:#}")).to_string(),
                sev: 30,
                text: format!("{e:#}"),
                at: None,
            });
        }
        if cmd.def.name == "CRTRPGMOD" {
            checked.diags.extend(module_control_keywords(&m.source));
        }
        let max = checked.diags.iter().map(|d| d.sev).max().unwrap_or(0);
        let ok = compiled.is_ok() && max <= 10;
        if listed {
            let copies = crate::rpg::listing_copies(self.client, libl, &m.source, options).await;
            self.rpg_listing(
                cmd,
                lib,
                name,
                m,
                &copies,
                parsed.as_ref(),
                compiled.as_ref().ok(),
                &checked,
                ok,
            )
            .await?;
        }
        if !ok {
            if listed {
                let what = if cmd.def.name == "CRTRPGMOD" {
                    "Module"
                } else {
                    "Program"
                };
                // A module: RNS9309; a program: RNS9310 (PUB400
                // 2026-10-30, verify29 v29_dmod; verify24 q24_rpg).
                let id = if what == "Module" {
                    "RNS9309"
                } else {
                    "RNS9310"
                };
                bail!(
                    "RNS9308: Compilation stopped: errors of severity {max} were found in the program.\n{id}: {what} {name} was not created in library {lib}."
                );
            }
            match compiled {
                Err(e) => return Err(e),
                Ok(_) => {
                    let d = checked
                        .diags
                        .iter()
                        .find(|d| d.sev > 10)
                        .cloned()
                        .unwrap_or_default();
                    bail!("{}: {} (line {})", d.id, d.text, d.line);
                }
            }
        }
        Ok((compiled?, max))
    }

    /// The ILE RPG compiler listing (OUTPUT(*PRINT), the default).
    #[allow(clippy::too_many_arguments)]
    async fn rpg_listing(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        member: &crate::source::Member,
        copies: &[crate::rpg::listing::Copy],
        src: Option<&crate::rpg::ast::Source>,
        prog: Option<&crate::rpg::program::Program>,
        checked: &crate::rpg::check::Checked,
        created: bool,
    ) -> Result<()> {
        if cmd.get("OUTPUT") == "*NONE" {
            return Ok(());
        }
        let or = |k: &str, d: &str| -> String {
            match cmd.get(k) {
                "" => d.to_string(),
                v => v.to_string(),
            }
        };
        let opts = crate::rpg::listing::Opts {
            command: cmd.def.name.to_string(),
            user: self.user.clone(),
            program: name.to_string(),
            library: lib.to_string(),
            text: or("TEXT", "*SRCMBRTXT"),
            dftactgrp: or("DFTACTGRP", "*YES"),
            actgrp: or("ACTGRP", "*STGMDL"),
            replace: or("REPLACE", "*YES"),
            dbgview: or("DBGVIEW", "*STMT"),
            tgtrls: or("TGTRLS", "*CURRENT"),
            alwnull: or("ALWNULL", "*NO"),
            bnddir: match cmd.list("BNDDIR").join(" ") {
                b if b.is_empty() || b == "*NONE" => String::new(),
                b => b,
            },
            genlvl: 10,
            module: cmd.def.name == "CRTRPGMOD",
            srtseq: String::new(),
            langid: String::new(),
        };
        // CRTSQLRPGI: the precompiler's listing first, then the compiler's
        // of the source it hands over (here: the source as written).
        if cmd.def.name == "CRTSQLRPGI" {
            let stamp = crate::report::Stamp::now();
            let sql_opts = crate::rpg::sqllist::Opts {
                library: lib.to_string(),
                object: name.to_string(),
                objtype: or("OBJTYPE", "*PGM"),
                commit: or("COMMIT", "*CHG"),
                text: or("TEXT", "*SRCMBRTXT"),
                replace: or("REPLACE", "*YES"),
                dbgview: or("DBGVIEW", "*NONE"),
            };
            let stmts = crate::rpg::sqllist::statements(&member.source);
            let mut libl = self.library_list();
            if !libl.iter().any(|l| l == lib) {
                libl.insert(0, lib.to_string());
            }
            let mut tables: Vec<crate::rpg::sqllist::Table> = Vec::new();
            for (_, st) in &stmts {
                for t in crate::rpg::sqllist::tables(st) {
                    if tables.iter().any(|x| x.name == t) {
                        continue;
                    }
                    if let Ok(f) = catalog::resolve_file(self.client, &libl, &t).await {
                        tables.push(crate::rpg::sqllist::Table {
                            name: t,
                            columns: f
                                .file
                                .fields
                                .iter()
                                .map(|fd| {
                                    (
                                        fd.name.clone(),
                                        crate::rpg::sqllist::column_type(fd, 37),
                                        fd.null_capable,
                                    )
                                })
                                .collect(),
                        });
                    }
                }
            }
            let mut host: Vec<crate::rpg::sqllist::HostVar> = Vec::new();
            for (_, st) in &stmts {
                for v in crate::rpg::sqllist::host_variables(st) {
                    if host.iter().any(|h| h.name == v) {
                        continue;
                    }
                    let line = src
                        .and_then(|s| {
                            s.defs
                                .iter()
                                .chain(s.procs.iter().flat_map(|p| p.defs.iter()))
                                .find(|d| d.name.eq_ignore_ascii_case(&v))
                                .map(|d| d.line)
                        })
                        .unwrap_or(0);
                    let sym = prog.and_then(|p| {
                        p.syms
                            .get(&v)
                            .or_else(|| p.procs.values().find_map(|x| x.syms.get(&v)))
                    });
                    let sql_type = match sym {
                        Some(crate::rpg::program::Sym::Var(x)) => {
                            crate::rpg::sqllist::sql_type(&x.dtype)
                        }
                        Some(crate::rpg::program::Sym::Ds(d)) => format!("STRUCTURE({})", d.size),
                        _ => String::new(),
                    };
                    host.push(crate::rpg::sqllist::HostVar {
                        name: v,
                        line,
                        sql_type,
                    });
                }
            }
            let mut lines = crate::rpg::sqllist::listing(&sql_opts, member, &tables, &host, &stamp);
            let mut shown = member.clone();
            shown.stmf = format!("/QSYS.LIB/QTEMP.LIB/QSQLTEMP1.FILE/{name}.MBR");
            shown.ccsid = 37;
            shown.changed_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_micros() as i64);
            shown.text = String::new();
            let opts = crate::rpg::listing::Opts {
                command: "CRTBNDRPG".into(),
                dbgview: "*STMT".into(),
                srtseq: "*JOB".into(),
                langid: "*JOB".into(),
                tgtrls: "V7R5M0".into(),
                ..opts
            };
            let input = crate::rpg::listing::Input {
                opts: &opts,
                copies,
                member: &shown,
                src,
                prog,
                diags: &checked.diags,
                undefined: &checked.undefined,
                created,
            };
            lines.extend(crate::rpg::listing::listing(&input, &stamp));
            return self.print_listing(name, lines).await;
        }
        let input = crate::rpg::listing::Input {
            opts: &opts,
            copies,
            member,
            src,
            prog,
            diags: &checked.diags,
            undefined: &checked.undefined,
            created,
        };
        let lines = crate::rpg::listing::listing(&input, &crate::report::Stamp::now());
        self.print_listing(name, lines).await
    }

    /// The RPG/400 compiler listing of CRTRPGPGM (rpg3list.rs).
    async fn rpg3_listing(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        m: &crate::source::Member,
        created: bool,
    ) -> Result<()> {
        if cmd.get("OUTPUT") == "*NONE" {
            return Ok(());
        }
        let mut libl = self.library_list();
        if !libl.iter().any(|l| l == lib) {
            libl.insert(0, lib.to_string());
        }
        let mut files = Vec::new();
        for l in m.source.lines() {
            let cs: Vec<char> = l.chars().collect();
            let is_f = cs.get(5).is_some_and(|c| c.eq_ignore_ascii_case(&'F'))
                && cs.get(6) != Some(&'*')
                && cs.get(18).is_some_and(|c| c.eq_ignore_ascii_case(&'E'));
            if !is_f {
                continue;
            }
            let fname: String = cs
                .iter()
                .skip(6)
                .take(8)
                .collect::<String>()
                .trim()
                .to_ascii_uppercase();
            let Ok(r) = catalog::resolve_file(self.client, &libl, &fname).await else {
                continue;
            };
            let mut at = 0u32;
            let fields = r
                .file
                .fields
                .iter()
                .map(|fd| {
                    let t = crate::rpg::program::program_type(fd, true, false)
                        .unwrap_or(crate::rpg::data::DType::Char(0));
                    let size = t.size() as u32;
                    let from = at + 1;
                    at += size;
                    let (letter, dec) = match t {
                        crate::rpg::data::DType::Packed(_, d) => ('P', Some(d)),
                        crate::rpg::data::DType::Binary(_, d) => ('B', Some(d)),
                        crate::rpg::data::DType::Zoned(_, d) => (' ', Some(d)),
                        _ => (' ', None),
                    };
                    (fd.name.clone(), letter, from, at, dec)
                })
                .collect();
            files.push(crate::rpg::rpg3list::ExtFile {
                file: fname,
                library: r.library.clone(),
                format: r.file.format.clone(),
                fields,
            });
        }
        let opts = crate::rpg::rpg3list::Opts {
            program: name.to_string(),
            library: lib.to_string(),
            text: match cmd.get("TEXT") {
                "" => "*SRCMBRTXT".into(),
                t => t.to_string(),
            },
            replace: match cmd.get("REPLACE") {
                "" => "*YES".into(),
                r => r.to_string(),
            },
        };
        let lines =
            crate::rpg::rpg3list::listing(&opts, m, &files, created, &crate::report::Stamp::now());
        self.print_listing(name, lines).await
    }

    /// A compiler listing: spooled (a file named after the program) when
    /// the job has an identity, and kept for `system` to show.
    async fn print_listing(&mut self, name: &str, lines: Vec<String>) -> Result<()> {
        if let Some(id) = self.id.clone() {
            crate::spool::print_report(self.client, &id, name, "", &lines).await?;
        }
        self.calls.printed = lines;
        Ok(())
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
                bail!("CPF9812: There is no file {tf} in library {tl}.");
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
                        ..Default::default()
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
                    ..Default::default()
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
        if let Some((file, mbr, env)) = &d.rexx {
            // PGM(*REXX): the procedure gets the parameters as one string.
            let arg = crate::cmd::rexx_args(d, line)?;
            let msgs = Box::pin(crate::rexx::strrexprc(self, file, mbr, &arg, env)).await?;
            return Ok(Outcome::Messages(msgs));
        }
        let args = crate::cmd::call_args(crate::cmd::encode(d, line)?);
        if let Some(v) = &d.vldckr {
            // The command analyzer (QSYS/QCATRS) calls the validity
            // checker: its caller's call stack entry.
            let job_key = self.job_key();
            let analyzer =
                crate::pgmmsg::listed_push(&job_key, ("QSYS", "QCATRS"), crate::ile::DEFAULT_GROUP);
            let r = self.call_program_boxed(v, args.clone()).await;
            crate::pgmmsg::listed_pop(&job_key, analyzer);
            // Its messages come before the command's.
            if let Outcome::Messages(mut first) = r? {
                if let Outcome::Messages(m) = self.call_program_boxed(&d.pgm, args).await? {
                    first.extend(m);
                }
                return Ok(Outcome::Messages(first));
            }
        }
        self.call_program_boxed(&d.pgm, args).await
    }

    /// CHGCMD (CL reference): a command's processing program (or REXX
    /// procedure), validity checker and text; the other attributes are
    /// accepted.
    async fn chgcmd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("CMD"));
        let Some(mut d) = crate::cmd::find(self.client, &libs, &name).await? else {
            bail!(
                "CPF9801: Object {} type *CMD not found.",
                name.to_ascii_uppercase()
            );
        };
        authority::check(
            self.client,
            &self.user,
            &d.library,
            &d.name,
            "*CMD",
            Aut::Change,
        )
        .await?;
        let same = |k: &str| cmd.get(k).is_empty() || cmd.get(k).eq_ignore_ascii_case("*SAME");
        let qualified = |q: &str| -> String {
            let q = q.to_ascii_uppercase();
            if q.contains('/') || q.starts_with('*') {
                q
            } else {
                format!("*LIBL/{q}")
            }
        };
        if !same("PGM") {
            let pgm = cmd.get("PGM").to_ascii_uppercase();
            if pgm == "*REXX" {
                let (file, mbr, env) = d
                    .rexx
                    .clone()
                    .unwrap_or_else(|| ("*LIBL/QREXSRC".into(), d.name.clone(), "*COMMAND".into()));
                d.rexx = Some((file, mbr, env));
                d.pgm = "*REXX".into();
            } else {
                d.rexx = None;
                d.pgm = qualified(&pgm);
            }
        }
        if let Some((file, mbr, env)) = d.rexx.as_mut() {
            if !same("REXSRCFILE") {
                *file = qualified(cmd.get("REXSRCFILE"));
            }
            if !same("REXSRCMBR") {
                *mbr = cmd.get("REXSRCMBR").to_ascii_uppercase();
            }
            if !same("REXCMDENV") {
                *env = cmd.get("REXCMDENV").to_ascii_uppercase();
            }
        }
        if !same("VLDCKR") {
            d.vldckr = match cmd.get("VLDCKR").to_ascii_uppercase().as_str() {
                "*NONE" => None,
                v => Some(qualified(v)),
            };
        }
        if !same("TEXT") {
            d.text = match cmd.get("TEXT") {
                "*BLANK" => String::new(),
                t => t.to_string(),
            };
        }
        let text = d.text.clone();
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        crate::cmd::create(&mut c, &d, &text, true).await?;
        Ok(Outcome::Messages(vec![format!(
            "Command {} in library {} changed.",
            d.name, d.library
        )]))
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
        if cmd.get("PGM").eq_ignore_ascii_case("*REXX") {
            let file = cmd.get("REXSRCFILE").to_ascii_uppercase();
            let file = if file.contains('/') {
                file
            } else {
                format!("*LIBL/{file}")
            };
            let mbr = match cmd.get("REXSRCMBR").to_ascii_uppercase() {
                m if m == "*CMD" || m.is_empty() => name.clone(),
                m => m,
            };
            d.rexx = Some((file, mbr, cmd.get("REXCMDENV").to_ascii_uppercase()));
            d.pgm = "*REXX".into();
        }
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        d.text = text.clone();
        // The listing's messages (PUB400 2026-10-26, verify1 v_crtcmd and
        // qsh q_crtcmd): CPD079A, severity 10, for each CRTCMD parameter
        // the CMD statement gives, in the reverse of CRTCMD's order;
        // CPD0250, severity 20, for an optional *CHAR parameter without
        // DFT.
        let lines: Vec<String> = m.source.lines().map(str::to_string).collect();
        let stmts = crate::cmdlist::statements(&lines);
        let mut msgs = Vec::new();
        if let Some(c) = stmts.iter().find(|s| s.command == "CMD") {
            for kw in crate::cmdlist::OVERRIDABLE.iter().rev() {
                if crate::cmdlist::gives(&c.text, kw) {
                    msgs.push(crate::cmdlist::Msg {
                        id: "CPD079A".into(),
                        sev: 10,
                        seq: crate::cmdlist::seq(c.first),
                        text: format!("The CMD statement's {kw} replaces CRTCMD's value."),
                        inline: true,
                    });
                }
            }
        }
        for (s, p) in stmts
            .iter()
            .filter(|s| s.command == "PARM")
            .zip(d.parms.iter())
        {
            if p.ty.eq_ignore_ascii_case("*CHAR")
                && p.min == 0
                && p.dft.is_none()
                && p.constant.is_none()
            {
                msgs.push(crate::cmdlist::Msg {
                    id: "CPD0250".into(),
                    sev: 20,
                    seq: crate::cmdlist::seq(s.first),
                    text: format!("{} has no DFT: blanks are its default.", p.kwd),
                    inline: false,
                });
            }
        }
        let crtcmd_vldckr = cmd.get("VLDCKR").to_ascii_uppercase();
        let crtcmd_allow = cmd.list("ALLOW").join(" ").to_ascii_uppercase();
        // The CMD statement's VLDCKR, MAXPOS, TEXT and ALLOW override
        // CRTCMD's (CL reference, "Command Definition (CMD)").
        let stmt = std::mem::take(&mut d.statement);
        if let Some(v) = stmt.get("VLDCKR") {
            d.vldckr = match v.to_ascii_uppercase().as_str() {
                "*NONE" => None,
                v => Some(qualified(v)),
            };
        }
        if let Some(v) = stmt.get("MAXPOS") {
            d.maxpos = v.parse().ok();
        }
        if let Some(v) = stmt.get("TEXT") {
            d.text = match v.to_ascii_uppercase().as_str() {
                "*BLANK" => String::new(),
                "*SRCMBRTXT" => m.text.clone(),
                "*CMDPMT" => d.prompt.clone(),
                _ => crate::cmd::raw_text(v).unwrap_or_default(),
            };
        }
        // CRTCMD ALLOW is a list: every value counts, not just the first.
        let allow = stmt
            .get("ALLOW")
            .map(String::as_str)
            .unwrap_or(crtcmd_allow.as_str());
        d.allow = allow
            .split_whitespace()
            .map(|x| x.trim_matches(['(', ')']).to_ascii_uppercase())
            .filter(|x| !x.is_empty() && x != "*ALL")
            .collect();
        let text = d.text.clone();
        let mut c = runtime::connect_db(&self.env.database_url).await?;
        crate::cmd::create(&mut c, &d, &text, cmd.get("REPLACE") != "*NO").await?;
        authority::set_owner(self.client, &lib, &name, "*CMD", &self.user).await?;
        // CPC0202 (PUB400 2026-10-23, tools/golden/qsh.txt q_crtcmd).
        let done = format!("Command {name} is now in library {lib}.");
        // The listing (cmdlist.rs): CRTCMD's own values on the first page.
        let split = |q: &str| match q.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), q.to_string()),
        };
        let input = crate::cmdlist::Input {
            name: name.clone(),
            library: lib.clone(),
            pgm: split(&pgm),
            srcfile: (m.library.clone(), m.file.clone()),
            member: m.name.clone(),
            changed: m.changed_us.map(stamp_of_us),
            vldckr: if crtcmd_vldckr.is_empty() {
                "*NONE".into()
            } else {
                crtcmd_vldckr
            },
            mode: vec!["*PROD".into(), "*DEBUG".into(), "*SERVICE".into()],
            allow: match crtcmd_allow.as_str() {
                "" | "*ALL" => crate::cmdlist::ALL_ENVIRONMENTS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                a => a
                    .split_whitespace()
                    .map(|x| x.trim_matches(['(', ')']).to_string())
                    .collect(),
            },
            maxpos: match cmd.get("MAXPOS") {
                "" => "*NOMAX".into(),
                v => v.to_ascii_uppercase(),
            },
            text: match cmd.get("TEXT") {
                "*SRCMBRTXT" => m.text.clone(),
                "*BLANK" | "" => String::new(),
                t => t.to_string(),
            },
            replace: match cmd.get("REPLACE") {
                "" => "*YES".into(),
                v => v.to_ascii_uppercase(),
            },
            lines,
        };
        let printed = crate::cmdlist::listing(&input, &msgs, &done, &crate::report::Stamp::now());
        self.print_listing(&name, printed).await?;
        Ok(Outcome::Messages(vec![format!("CPC0202: {done}")]))
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

    /// CRTPNLGRP: a panel group (UIM help and panels) from its source
    /// member. as4tainr keeps the source with the object (it does not
    /// show UIM help yet: docs/fidelity-gaps.md).
    async fn crtpnlgrp(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("PNLGRP"));
        let (lib, name) = (lib.to_ascii_uppercase(), name.to_ascii_uppercase());
        let (libs, file) = self.search(cmd.get("SRCFILE"));
        let mbr = match cmd.get("SRCMBR").to_ascii_uppercase().as_str() {
            "*PNLGRP" | "" => name.clone(),
            m => m.to_string(),
        };
        let m = crate::source::find_member(self.client, &libs, &file.to_ascii_uppercase(), &mbr)
            .await?;
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: There is no library {lib}.");
        }
        let text = match cmd.get("TEXT") {
            "*SRCMBRTXT" => m.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                 VALUES ($1, $2, '*PNLGRP', 'UIM', $3)
                 ON CONFLICT (library, name, objtype) DO UPDATE SET text = EXCLUDED.text",
                &[&lib, &name, &text],
            )
            .await?;
        authority::set_owner(self.client, &lib, &name, "*PNLGRP", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Panel group {name} created in library {lib}."
        )]))
    }

    async fn dltpnlgrp(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("PNLGRP"));
        let name = name.to_ascii_uppercase();
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, "*PNLGRP")
                .await?
            {
                authority::check(self.client, &self.user, l, &name, "*PNLGRP", Aut::All).await?;
                self.client
                    .execute(
                        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*PNLGRP'",
                        &[l, &name],
                    )
                    .await?;
                authority::forget(self.client, l, &name, "*PNLGRP").await?;
                return Ok(Outcome::Messages(vec![format!(
                    "CPC2191: Object {name} of type *PNLGRP in library {l} was deleted."
                )]));
            }
        }
        bail!("CPF2105: Object {name} of type *PNLGRP in library *LIBL not found.")
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
        let actgrp = match cmd.get("OVRSCOPE") {
            "*ACTGRPDFN" | "" => self.named_actgrp(),
            _ => None,
        };
        crate::ovr::merge_scoped(&self.job_key(), &mut self.calls.overrides);
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
                splfname: match cmd.get("SPLFNAME") {
                    "" | "*NONE" | "*FILE" => None,
                    n => Some(n.to_ascii_uppercase()),
                },
                usrdta: match cmd.get("USRDTA") {
                    "" | "*NONE" => None,
                    d => Some(d.to_string()),
                },
                actgrp,
                mbr: match cmd.get("MBR") {
                    "" | "*FIRST" => None,
                    m => Some(m.to_ascii_uppercase()),
                },
            },
        );
        crate::ovr::save_scoped(&self.job_key(), &self.calls.overrides);
        Ok(Outcome::Messages(vec![]))
    }

    /// The non-default activation group of the RPG procedure running the
    /// command (through QCMDEXC), if any.
    fn named_actgrp(&self) -> Option<String> {
        crate::pgmmsg::stack(&self.job_key())
            .iter()
            .rev()
            .find(|e| !(e.lib == "QSYS" && e.pgm == "QCMDEXC"))
            .map(|e| e.actgrp.clone())
            .filter(|g| !g.is_empty() && g != crate::ile::DEFAULT_GROUP)
    }

    /// DLTOVR: overrides of the current call level (LVL(*JOB): those for
    /// the whole job).
    fn dltovr(&mut self, cmd: &Command) -> Result<Outcome> {
        let level = if cmd.get("LVL") == "*JOB" {
            0
        } else {
            self.calls.level()
        };
        // LVL(*) or *ACTGRPDFN in a non-default activation group: the
        // overrides scoped to that activation group (DLTOVR, LVL).
        let actgrp = match cmd.get("LVL") {
            "*" | "*ACTGRPDFN" | "" => self.named_actgrp(),
            _ => None,
        };
        let files = cmd.list("FILE");
        crate::ovr::merge_scoped(&self.job_key(), &mut self.calls.overrides);
        let before = self.calls.overrides.len();
        self.calls.overrides.retain(|o| {
            let here = match &actgrp {
                Some(g) => o.actgrp.as_ref() == Some(g),
                None => o.level == level && o.actgrp.is_none(),
            };
            !here
                || !files.iter().any(|f| {
                    f == "*ALL"
                        || (f == "*PRTF" && o.kind == crate::ovr::Kind::Printer)
                        || f.eq_ignore_ascii_case(&o.file)
                })
        });
        crate::ovr::save_scoped(&self.job_key(), &self.calls.overrides);
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
        // A source file: the member's records go (CLRPFM of a source
        // physical file member).
        for l in &libs {
            if !crate::source::is_source_file(self.client, l, &name).await? {
                continue;
            }
            authority::check(self.client, &self.user, l, &name, "*FILE", Aut::Change).await?;
            let mbr = match cmd.get("MBR").to_ascii_uppercase().as_str() {
                "*FIRST" | "" => self
                    .client
                    .query_opt(
                        "SELECT member FROM as4tainr.source_members WHERE library=$1 AND file=$2
                          ORDER BY added, member LIMIT 1",
                        &[l, &name],
                    )
                    .await?
                    .map(|r| r.get::<_, String>(0))
                    .ok_or_else(|| {
                        anyhow!("CPF3141: Member *FIRST not found in file {name} in library {lib_label}.")
                    })?,
                "*LAST" => self
                    .client
                    .query_opt(
                        "SELECT member FROM as4tainr.source_members WHERE library=$1 AND file=$2
                          ORDER BY added DESC, member DESC LIMIT 1",
                        &[l, &name],
                    )
                    .await?
                    .map(|r| r.get::<_, String>(0))
                    .ok_or_else(|| {
                        anyhow!("CPF3141: Member *LAST not found in file {name} in library {lib_label}.")
                    })?,
                m => m.to_string(),
            };
            let n = self
                .client
                .execute(
                    "UPDATE as4tainr.source_members SET source='', seqdat=NULL, changed=now()
                      WHERE library=$1 AND file=$2 AND member=$3",
                    &[l, &name, &mbr],
                )
                .await?;
            if n == 0 {
                bail!("CPF3141: Member {mbr} not found in file {name} in library {lib_label}.");
            }
            return Ok(Outcome::Messages(vec![format!(
                "CPC3101: Member {mbr} of file {name} in library {} was cleared.",
                crate::qtemp::logical(l)
            )]));
        }
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
    pub(crate) async fn message_file(&self, q: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(q);
        let name = name.to_ascii_uppercase();
        match crate::msgf::locate(self.client, &libs, &name).await? {
            Some(lib) => Ok((lib, name)),
            None => {
                let lib = q.split_once('/').map(|(l, _)| l).unwrap_or("*LIBL");
                bail!("CPF2407: There is no message file {name} in library {lib}.")
            }
        }
    }

    /// DLTMSGF.
    async fn dltmsgf(&mut self, cmd: &Command) -> Result<Outcome> {
        // A message file that is not there: CPF2105, as for the other
        // delete commands (iMoq's installer monitors DLTMSGF for it).
        let q = cmd.get("MSGF");
        let (lib, name) = match self.message_file(q).await {
            Ok(x) => x,
            Err(_) => {
                let (_, n) = self.search(q);
                let label = q.split_once('/').map(|(l, _)| l).unwrap_or("*LIBL");
                bail!(
                    "CPF2105: Object {} of type *MSGF in library {} not found.",
                    n.to_ascii_uppercase(),
                    label.to_ascii_uppercase()
                )
            }
        };
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
            attrs: self.msgd_attrs(cmd, old.as_ref().map(|d| &d.attrs))?,
        };
        crate::msgf::add(self.client, &d, change).await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// The reply and other attributes ADDMSGD and CHGMSGD set (`old`:
    /// CHGMSGD's description, whose values *SAME keeps). The message
    /// level is today's date and 01, or the next number when the
    /// description was already changed today.
    fn msgd_attrs(
        &self,
        cmd: &Command,
        old: Option<&crate::msgf::Attrs>,
    ) -> Result<crate::msgf::Attrs> {
        let mut a = old.cloned().unwrap_or_default();
        let set = |v: &str| v != "*SAME";
        let ty = cmd.get("TYPE").to_ascii_uppercase();
        if set(&ty) {
            a.reply_type = ty.clone();
        }
        let len = cmd.list("LEN");
        let len_given = len.first().is_some_and(|v| set(v) && v != "*TYPE");
        if len_given {
            let n = |s: Option<&String>| -> Result<u32> {
                match s {
                    None => Ok(0),
                    Some(v) => v.trim().parse().map_err(|_| {
                        anyhow!("CPF0006: The command has errors and was not run. LEN({v}) is not valid.")
                    }),
                }
            };
            a.reply_len = (n(len.first())?, n(len.get(1))?);
        } else if set(&ty) || len.first().is_some_and(|v| v == "*TYPE") {
            // LEN(*TYPE): the reply type's own length (ADDMSGD).
            a.reply_len = match a.reply_type.as_str() {
                "*DEC" => (15, 5),
                "*NAME" => (10, 0),
                "*NONE" => (0, 0),
                _ => (32, 0),
            };
        }
        let values = cmd.list("VALUES");
        if values.first().is_some_and(|v| set(v)) {
            a.values = if values.first().is_some_and(|v| v == "*NONE") {
                vec![]
            } else {
                values
                    .iter()
                    .map(|v| v.trim().trim_matches('\'').to_string())
                    .collect()
            };
        }
        let dft = cmd.get("DFT");
        if set(dft) {
            a.dft = if dft == "*NONE" {
                String::new()
            } else {
                dft.trim().trim_matches('\'').to_string()
            };
        }
        let alr = cmd.list("ALROPT");
        if let Some(v) = alr.first().filter(|v| set(v)) {
            a.alropt = v.to_ascii_uppercase();
        }
        let logprb = cmd.get("LOGPRB");
        if set(logprb) {
            a.logprb = logprb.to_ascii_uppercase();
        }
        let ccsid = cmd.get("CCSID");
        if set(ccsid) {
            // *JOB and *HEX: the text is kept as it is (shown as 65535,
            // observed on IBM i 7.5).
            a.ccsid = match ccsid.to_ascii_uppercase().as_str() {
                "*JOB" | "*HEX" => "65535".into(),
                v => v.to_string(),
            };
        }
        let today = crate::msgf::today();
        if old.is_some() && a.changed == today {
            a.level += 1;
        } else {
            a.level = 1;
        }
        a.changed = today;
        Ok(a)
    }

    /// RMVMSGD.
    async fn rmvmsgd(&mut self, cmd: &Command) -> Result<Outcome> {
        let msgid = crate::msgf::check_msgid(cmd.get("MSGID"))?;
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Change).await?;
        crate::msgf::remove(self.client, &lib, &name, &msgid).await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// DSPMSGD: the descriptions in a range of identifiers, shown as a
    /// list or printed in IBM's layout (`msgf::report`).
    async fn dspmsgd(&mut self, cmd: &Command) -> Result<Outcome> {
        use crate::msgf::{ReportIds, ReportKind};
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Use).await?;
        let kind = if cmd.get("DETAIL") == "*BASIC" {
            ReportKind::Basic
        } else if cmd.get("FMTTXT") == "*NO" {
            ReportKind::Unformatted
        } else {
            ReportKind::Formatted
        };
        let range: Vec<String> = cmd
            .list("RANGE")
            .iter()
            .map(|v| v.to_ascii_uppercase())
            .collect();
        let all = crate::msgf::list(self.client, &lib, &name).await?;
        let key = |id: &str| crate::rpg::data::e(id);
        // Observed on IBM i 7.5: RANGE(*ALL) heads the report "*ALL" to
        // the message file's name; one identifier is the only one shown,
        // except DETAIL(*BASIC), which lists from it to the end (heading
        // "*ONLY"); two are a range.
        let (from, to, ids): (Option<String>, Option<String>, ReportIds) = match range.as_slice() {
            [] => (
                None,
                None,
                ReportIds::Range("*FIRST".into(), "*LAST".into()),
            ),
            [a] | [a, _] if a == "*ALL" => {
                (None, None, ReportIds::Range("*ALL".into(), name.clone()))
            }
            [a] if a == "*FIRST" => (
                None,
                None,
                ReportIds::Range("*FIRST".into(), "*LAST".into()),
            ),
            [a] | [a, _] if range.get(1).is_none_or(|b| b == "*ONLY") => {
                let a = crate::msgf::check_msgid(a)?;
                if !all.iter().any(|d| d.msgid == a) {
                    bail!(
                            "CPF2419: Message {a} is not in message file {name} in library {lib}.\nCPF2519: Errors occurred while processing the list of message identifiers."
                        );
                }
                if kind == ReportKind::Basic {
                    (Some(a.clone()), None, ReportIds::Range(a, "*ONLY".into()))
                } else {
                    (Some(a.clone()), Some(a.clone()), ReportIds::One(a))
                }
            }
            [a] => (Some(a.clone()), Some(a.clone()), ReportIds::One(a.clone())),
            [a, b, ..] => {
                let f = (a != "*FIRST").then(|| a.clone());
                let t = (b != "*LAST").then(|| b.clone());
                (f, t, ReportIds::Range(a.clone(), b.clone()))
            }
        };
        let ds: Vec<_> = all
            .into_iter()
            .filter(|d| from.as_ref().is_none_or(|f| key(&d.msgid) >= key(f)))
            .filter(|d| to.as_ref().is_none_or(|t| key(&d.msgid) <= key(t)))
            .collect();
        let stamp = crate::report::Stamp::now();
        let print = crate::msgf::report(
            kind,
            &lib,
            &name,
            &ids,
            &ds,
            &stamp,
            &crate::report::system_name(),
        );
        // CPC9802 comes with the printed report (observed with `system`).
        let printed = "CPC9802: The printed output was spooled.".to_string();
        if cmd.get("OUTPUT") == "*PRINT" {
            if let Some(me) = self.id.clone() {
                crate::spool::print_lines(self.client, &me, "QPMSGD", &name, &print).await?;
                return Ok(Outcome::Messages(vec![printed]));
            }
        }
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
            print,
            print_msg: Some(printed),
            spool: None,
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
                // The member's records as their bytes, as a data
                // member's: SRCSEQ ZONED(6,2), SRCDAT ZONED(6,0), SRCDTA
                // (PUB400 2026-10-28, tools/golden/screens6.txt 07_dsppfm,
                // 08_dsppfm_f10, 09_dsppfm_f11).
                let seqdat = crate::source::member_seqdat(self.client, l, &file, &m.name).await?;
                let rcdlen = crate::source::record_length(self.client, l, &file).await?;
                let pf = crate::source::record_file(&file, rcdlen);
                let width = pf.fields[2].length as usize;
                let images: Vec<(i64, Vec<u8>)> =
                    crate::source::line_records(&m.source, seqdat.as_deref())
                        .into_iter()
                        .take(DSPPFM_MAX_RECORDS as usize)
                        .enumerate()
                        .map(|(i, (seq, dat, line))| {
                            let rec = crate::rla::Record::from([
                                ("SRCSEQ".to_string(), seq),
                                ("SRCDAT".to_string(), dat),
                                ("SRCDTA".to_string(), line.chars().take(width).collect()),
                            ]);
                            (i as i64 + 1, crate::cpyf::record_image(&pf, &rec))
                        })
                        .collect();
                let reclen = crate::cpyf::record_length(&pf);
                return Ok(Outcome::Display(ListDisplay {
                    title: "Display Physical File Member".into(),
                    info: vec![
                        ("File".into(), file.clone()),
                        ("Library".into(), l.clone()),
                        ("Member".into(), m.name.clone()),
                        ("Type".into(), m.srctype.clone()),
                    ],
                    heading: crate::pfmview::ruler(1, reclen.max(1)),
                    rows: images
                        .iter()
                        .map(|(_, b)| crate::cpyf::printable(b).trim_end().to_string())
                        .collect(),
                    member: Some(Box::new(crate::pfmview::MemberView {
                        file: file.clone(),
                        library: crate::qtemp::logical(l),
                        member: m.name.clone(),
                        reclen,
                        records: images,
                    })),
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
        // The records as their bytes (pfmview.rs; PUB400 2026-10-27,
        // screens5 08_dsppfm): what a 5250 display and a printed DSPPFM
        // show.
        let reclen = crate::cpyf::record_length(&f.file);
        let images: Vec<(i64, Vec<u8>)> = recs
            .iter()
            .take(DSPPFM_MAX_RECORDS as usize)
            .map(|(rrn, vals)| {
                let rec: crate::rla::Record = f
                    .file
                    .fields
                    .iter()
                    .zip(vals)
                    .map(|(fd, v)| (fd.name.clone(), v.clone()))
                    .collect();
                (*rrn, crate::cpyf::record_image(&f.file, &rec))
            })
            .collect();
        let member_view = crate::pfmview::MemberView {
            file: f.file.name.clone(),
            library: crate::qtemp::logical(&f.library),
            member: f.file.name.clone(),
            reclen,
            records: images.clone(),
        };
        let truncated = recs.len() as i64 > DSPPFM_MAX_RECORDS;
        let rows: Vec<String> = images
            .iter()
            .map(|(_, b)| crate::cpyf::printable(b).trim_end().to_string())
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
        // Printed and listed: the record bytes as characters under a
        // ruler of the record length.
        Ok(Outcome::Display(ListDisplay {
            title: "Display Physical File Member".into(),
            info,
            heading: crate::pfmview::ruler(1, reclen.max(1)),
            rows,
            member: Some(Box::new(member_view)),
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
        // JOBD: *USRPRF (as4tainr's profiles name none: QDFTJOBD's
        // defaults), or a job description: the job's name (JOB(*JOBD))
        // and its initial library list (INLLIBL(*JOBD)).
        let jobd = match cmd.get("JOBD").to_ascii_uppercase().as_str() {
            "" | "*USRPRF" => None,
            q => {
                let (libs, n) = self.search(q);
                match crate::jobd::find(self.client, &libs, &n).await? {
                    Some(d) => Some(d),
                    None => bail!(
                        "CPF1338: Errors on SBMJOB command. Job description {} not found.",
                        n.to_ascii_uppercase()
                    ),
                }
            }
        };
        let name = match cmd.get("JOB") {
            "*JOBD" => jobd
                .as_ref()
                .map(|d| d.name.clone())
                .unwrap_or_else(|| jobs::DEFAULT_BATCH_JOB_NAME.to_string()),
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
        let inllibl: Vec<String> = cmd
            .list("INLLIBL")
            .iter()
            .map(|l| l.to_ascii_uppercase())
            .collect();
        let libl = if inllibl.is_empty() || inllibl == ["*CURRENT"] {
            self.libl.clone()
        } else if inllibl == ["*JOBD"] {
            match jobd.as_ref().and_then(|d| d.inllibl.clone()) {
                Some(l) => l,
                None => vec!["QGPL".into(), "QTEMP".into()],
            }
        } else if inllibl == ["*SYSVAL"] {
            vec!["QGPL".into(), "QTEMP".into()]
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
        // IBM i 7.5's WRKACTJOB (PUB400): subsystem/job, user, type, CPU %
        // (not measured here: blank), function, status.
        let fmt = |job: &str, user: &str, ty: &str, func: &str, status: &str| {
            format!(
                "{job:<13}  {user:<10}  {ty:<4}  {:>5}  {func:<14}  {status}",
                ""
            )
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
            info: vec![],
            heading: format!(
                "{:<13}  {:<10}  {:<4}  {:>5}  {:<14}  {}",
                "Subsystem/Job", "User", "Type", "CPU %", "Function", "Status"
            ),
            rows,
            options: vec![(5, "Display job log".into(), "DSPJOBLOG JOB({})".into())],
            keys,
            refresh: Some("WRKACTJOB".into()),
            print: vec![],
            ibm: IbmLayout::some(IbmLayout {
                system_row1: true,
                right2: {
                    let st = crate::report::Stamp::now();
                    format!("{}  {} {}", st.date, st.time, st.zone)
                        .trim_end()
                        .to_string()
                },
                lines: vec![format!("{:48}Active jobs:   {}", "", active.len())],
                heading2: format!("{:15}Current", ""),
                command_line: Some("Parameters or command".into()),
                tight: true,
                ..Default::default()
            }),
            print_msg: None,
            spool: None,
            member: None,
        }))
    }

    async fn wrksbmjob(&mut self) -> Result<Outcome> {
        let list = jobs::submitted_by(self.client, &self.user).await?;
        // IBM i 7.5's WRKSBMJOB (PUB400): job, user, type, status, function.
        let fmt = |job: &str, user: &str, ty: &str, status: &str, func: &str| {
            format!("{job:<10}  {user:<10}  {ty:<7}  {status:<16}  {func}")
        };
        let rows = list
            .iter()
            .map(|j| {
                let status = if j.status == "OUTQ" && j.end_code == "ABNORMAL" {
                    "OUTQ  (ended abnormally)".to_string()
                } else {
                    j.status.clone()
                };
                fmt(&j.id.name, &j.id.user, "BATCH", &status, &j.function)
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Submitted Jobs".into(),
            info: vec![],
            heading: fmt("Job", "User", "Type", "-----Status-----", "Function"),
            rows,
            options: vec![
                (4, "End".into(), "ENDJOB JOB({})".into()),
                (5, "Display job log".into(), "DSPJOBLOG JOB({})".into()),
            ],
            keys: list.iter().map(|j| j.id.to_string()).collect(),
            refresh: Some("WRKSBMJOB".into()),
            print: vec![],
            ibm: IbmLayout::some(IbmLayout {
                system_row1: true,
                right2: {
                    let st = crate::report::Stamp::now();
                    format!("{}  {} {}", st.date, st.time, st.zone)
                        .trim_end()
                        .to_string()
                },
                lines: vec![format!("{}   *USER", dsp_label("Submitted from", 29))],
                command_line: Some("Parameters or command".into()),
                top_bottom_keys: true,
                ..Default::default()
            }),
            print_msg: None,
            spool: None,
            member: None,
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

    /// CHGJOB: the attributes of this job (`jobattr.rs`). Another job
    /// is found (CPF1321 when it is not there) but not changed: its
    /// attributes live in its own process (docs/fidelity-gaps.md).
    async fn chgjob(&mut self, cmd: &Command) -> Result<Outcome> {
        let spec = cmd.get("JOB").trim().to_string();
        if !matches!(spec.as_str(), "*" | "") {
            let job = self.job_named(&spec).await?;
            if self.id.as_ref().map(|i| i.number) != Some(job.id.number) {
                bail!(
                    "Changing another job's attributes is not supported here.\nCPF1336: CHGJOB of job {} did not change it.",
                    job.id
                );
            }
        }
        for k in ["JOBQ", "SCDDATE", "SCDTIME"] {
            if !matches!(
                cmd.get(k).trim().to_ascii_uppercase().as_str(),
                "*SAME" | ""
            ) {
                bail!("CPF1343: {k} can be changed only for a job on a job queue.");
            }
        }
        let asked: Vec<(String, String)> = crate::jobattr::PARAMS
            .iter()
            .map(|k| {
                let v = if *k == "LOG" {
                    cmd.list(k).join(" ")
                } else {
                    cmd.get(k).to_string()
                };
                (k.to_string(), v)
            })
            .collect();
        let key = self.job_key();
        let jobd = match crate::jobd::find(self.client, &["QGPL".to_string()], "QDFTJOBD").await? {
            Some(d) if self.id.is_some() => crate::jobd::other_attributes(self.client, &d).await?,
            _ => String::new(),
        };
        let current = |k: &str| -> String {
            crate::jobattr::get(&key, k)
                .or_else(|| crate::jobd::given(&jobd, k))
                .unwrap_or_else(|| match k {
                    "SWS" => "00000000".into(),
                    "LOG" => "4 0 *NOLIST".into(),
                    _ => String::new(),
                })
        };
        let jobctl = crate::users::has_special(self.client, &self.user, "*JOBCTL").await?;
        crate::jobattr::change(&key, &asked, &current, self.display.is_some(), jobctl)?;
        Ok(Outcome::Messages(vec![]))
    }

    async fn dspjoblog(&mut self, cmd: &Command) -> Result<Outcome> {
        let this = matches!(cmd.get("JOB").trim(), "*" | "");
        if this && self.id.is_none() && cmd.get("OUTPUT") == "*PRINT" {
            // `system`'s own job: its job log printed (QPJOBLOG), which
            // `system` writes out; no message (PUB400 2026-10-28,
            // tools/golden/verify17.txt x17_own).
            let lines = self.system_job_log();
            self.calls.printed.extend(lines);
            return Ok(Outcome::Messages(vec![]));
        }
        let job = self.job_named(cmd.get("JOB")).await?;
        if cmd.get("OUTPUT") == "*PRINT" {
            let me = self
                .id
                .clone()
                .ok_or_else(|| anyhow!("OUTPUT(*PRINT) needs a job to own the spooled file."))?;
            let lines =
                crate::spool::job_log_lines(self.client, &job.id, crate::joblog::Title::Display)
                    .await?;
            crate::spool::print_report(self.client, &me, "QPJOBLOG", &job.id.name, &lines).await?;
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

    /// The printed job log of a job that is not as4tainr's (`system`,
    /// `as4tainr call`): as on IBM i, where `system` runs in a QP0ZSPWT
    /// job of user QUSER with job description QGPL/QDFTSVR (PUB400
    /// 2026-10-28, tools/golden/verify17.txt x17_own; the number here is
    /// the process's). The job started message CPF1124 first, then the
    /// messages its programs sent that are still on their queues
    /// (crate::joblog).
    /// A `system` job's log as QSYS2.JOBLOG_INFO('*') lists it to the job
    /// (PUB400 2026-10-28, tools/golden/verify22.txt u22_rows: the job
    /// start message from QWTPIIPP, then the messages the printed job log
    /// has, with each program's library, module, procedure and statement
    /// or instruction; removed messages are not listed, 2026-10-29,
    /// verify23.txt n23_rows), copied for SQL before a statement runs.
    async fn mirror_system_job_log(&self) {
        use crate::joblog::{info_end, InfoRow};
        let ((y, mo, d), (h, mi, sec), us) = crate::pgmmsg::job_started();
        let ts = |cyymmdd: &str, hms: &str, usec: &str| -> String {
            let g = |v: &str, r: std::ops::Range<usize>| v.get(r).unwrap_or("00").to_string();
            let century = if cyymmdd.starts_with('1') { "20" } else { "19" };
            format!(
                "{century}{}-{}-{} {}:{}:{}.{}",
                g(cyymmdd, 1..3),
                g(cyymmdd, 3..5),
                g(cyymmdd, 5..7),
                g(hms, 0..2),
                g(hms, 2..4),
                g(hms, 4..6),
                if usec.is_empty() { "000000" } else { usec }
            )
        };
        let mut rows = vec![InfoRow {
            msgid: Some("CPF1124".into()),
            kind: "INFORMATIONAL".into(),
            sev: 0,
            sent: format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{sec:02}.{us:06}"),
            from: [
                Some("QSYS".into()),
                Some("QWTPIIPP".into()),
                None,
                None,
                Some("04DC".into()),
            ],
            to: [None, Some("*EXT".into()), None, None, Some("*N".into())],
            from_user: "QUSER".into(),
            text: self.system_job_started_text(),
            seclvl: String::new(),
            msgf: Some(("QCPFMSG".into(), "QSYS".into())),
        }];
        for m in self.calls.logged() {
            rows.push(InfoRow {
                msgid: (!m.msgid.is_empty()).then(|| m.msgid.clone()),
                kind: match m.ty.as_str() {
                    "*COMP" => "COMPLETION",
                    "*DIAG" => "DIAGNOSTIC",
                    "*ESCAPE" => "ESCAPE",
                    "*NOTIFY" => "NOTIFY",
                    "*INQ" => "INQUIRY",
                    "*RPY" => "REPLY",
                    "*RQS" => "REQUEST",
                    _ => "INFORMATIONAL",
                }
                .into(),
                sev: m.sev,
                sent: ts(&m.sent.0, &m.sent.1, &m.usec),
                from: info_end(m.logged_sender.as_ref().unwrap_or(&m.sender)),
                to: info_end(&m.receiver),
                from_user: self.user.clone(),
                text: m.text.clone(),
                seclvl: m.seclvl.clone(),
                msgf: (!m.msgid.is_empty() && !m.msgf.is_empty()).then(|| {
                    let lib = if m.msgflib.is_empty() {
                        "QSYS".to_string()
                    } else {
                        m.msgflib.clone()
                    };
                    (m.msgf.clone(), lib)
                }),
            });
        }
        crate::joblog::mirror(self.client, &self.job_key(), &rows).await;
    }

    /// The text of a `system` job's start message CPF1124 (as4tainr's
    /// own words).
    fn system_job_started_text(&self) -> String {
        let number = format!("{:06}", std::process::id() % 1_000_000);
        let ((y, mo, d), (h, mi, sec), _) = crate::pgmmsg::job_started();
        let started = crate::report::Stamp::of((y, mo, d), (h, mi, sec));
        format!(
            "Job {number}/QUSER/QP0ZSPWT began running in subsystem QUSRWRK in QSYS on {} at {}; it came into the system on {} at {}.",
            started.date, started.time, started.date, started.time
        )
    }

    fn system_job_log(&self) -> Vec<String> {
        use crate::joblog::{date_text, time_text, End, Entry};
        let number = format!("{:06}", std::process::id() % 1_000_000);
        let ((y, mo, d), (h, mi, sec), us) = crate::pgmmsg::job_started();
        let started = crate::report::Stamp::of((y, mo, d), (h, mi, sec));
        let hms = format!("{h:02}{mi:02}{sec:02}");
        let mut entries = vec![Entry {
            msgid: "CPF1124".into(),
            ty: "*INFO".into(),
            date: started.date.clone(),
            time: time_text(&hms, &format!("{us:06}")),
            user: "QUSER".into(),
            from: End {
                pgm: "QWTPIIPP".into(),
                lib: "QSYS".into(),
                inst: "04DC".into(),
                ..Default::default()
            },
            to: End {
                pgm: "*EXT".into(),
                inst: "*N".into(),
                ..Default::default()
            },
            text: self.system_job_started_text(),
            ..Default::default()
        }];
        // What the CL programs put on their queues (the message APIs'
        // job log, crate::pgmmsg::job_log, is the process's, not this
        // job's: not here, docs/fidelity-gaps.md).
        let logged = self.calls.logged();
        let end = |s: &crate::pgmmsg::Sender| End {
            pgm: s.pgm.clone(),
            lib: s.lib.clone(),
            inst: if !s.inst.is_empty() {
                s.inst.clone()
            } else if !s.module.is_empty() {
                "*STMT".into()
            } else {
                "*N".into()
            },
            module: s.module.clone(),
            proc: s.proc.clone(),
            stmt: if s.line > 0 {
                s.line.to_string()
            } else {
                "*N".into()
            },
        };
        entries.extend(logged.iter().map(|m| Entry {
            msgid: m.msgid.clone(),
            ty: m.ty.clone(),
            sev: m.sev,
            date: date_text(&m.sent.0),
            time: time_text(&m.sent.1, &m.usec),
            user: self.user.clone(),
            from: end(m.logged_sender.as_ref().unwrap_or(&m.sender)),
            to: end(&m.receiver),
            text: m.text.clone(),
            seclvl: m.seclvl.clone(),
        }));
        let job = crate::joblog::Job {
            name: "QP0ZSPWT".into(),
            user: "QUSER".into(),
            number,
            jobd: "QDFTSVR".into(),
            jobd_lib: "QGPL".into(),
        };
        crate::joblog::print(
            &job,
            crate::joblog::Title::Display,
            &entries,
            &crate::report::Stamp::now(),
            &crate::report::system_name(),
        )
    }

    /// DSPJOB: the job's definition, status and run attributes and its
    /// call stack in IBM i 7.5's printed layout (crate::dspjob), printed
    /// (OUTPUT(*PRINT), `system`) or shown as a list. OPTION *DFNA,
    /// *STSA, *RUNA, *PGMSTK, or *SELECT/*ALL for the four.
    /// The facts DSPJOB shows of a job (`dspjob::Facts`): this job's
    /// (`system`'s own when it has no ID) or the one named; this job's
    /// attributes changed by CHGJOB (`jobattr`).
    pub(crate) async fn job_facts(&self, spec: &str) -> Result<crate::dspjob::Facts> {
        use crate::dspjob::Facts;
        let this = matches!(spec.trim(), "*" | "");
        let ((y, mo, d), (h, mi, s), _) = crate::pgmmsg::job_started();
        let st = crate::report::Stamp::of((y, mo, d), (h, mi, s));
        let f = if this && self.id.is_none() {
            // `system`'s own job (PUB400 2026-10-28,
            // tools/golden/verify19.txt b19_dfna, b19_stsa).
            Facts {
                name: "QP0ZSPWT".into(),
                user: "QUSER".into(),
                number: format!("{:06}", std::process::id() % 1_000_000),
                system_job: true,
                jobd: "QDFTSVR".into(),
                jobd_lib: "QGPL".into(),
                current_user: self.user.clone(),
                subsystem: "QUSRWRK".into(),
                entered: (st.date.clone(), st.time.clone()),
                started: (st.date.clone(), st.time.clone()),
                stack: crate::dspjob::system_job_base(),
                ..Default::default()
            }
        } else {
            let job = self.job_named(spec).await?;
            let d = crate::jobd::find(self.client, &["QGPL".to_string()], "QDFTJOBD").await?;
            let other = match &d {
                Some(d) => crate::jobd::other_attributes(self.client, d).await?,
                None => String::new(),
            };
            // The catalog's entered time is MM/DD/YY HH:MM:SS.
            let e = &job.entered;
            let n = |r: std::ops::Range<usize>| -> u32 {
                e.get(r).and_then(|v| v.parse().ok()).unwrap_or(0)
            };
            let es = crate::report::Stamp::of(
                (2000 + n(6..8) as i32, n(0..2), n(3..5)),
                (n(9..11), n(12..14), n(15..17)),
            );
            let batch = job.job_type.starts_with('B');
            Facts {
                name: job.id.name.clone(),
                user: job.id.user.clone(),
                number: format!("{:06}", job.id.number),
                job_type: if batch { "B" } else { "I" }.into(),
                jobd: "QDFTJOBD".into(),
                jobd_lib: "QGPL".into(),
                jobd_other: other,
                jobq: if batch {
                    job.jobq.clone()
                } else {
                    String::new()
                },
                jobq_lib: if batch { "QGPL".into() } else { String::new() },
                current_user: job.id.user.clone(),
                subsystem: job.subsystem.clone(),
                entered: (es.date.clone(), es.time.clone()),
                started: (es.date.clone(), es.time.clone()),
                ..Default::default()
            }
        };
        let mut f = f;
        let own = this
            || self
                .id
                .as_ref()
                .is_some_and(|i| f.number == format!("{:06}", i.number));
        if own {
            f.changed = crate::jobattr::all(&self.job_key());
        }
        Ok(f)
    }

    async fn dspjob(&mut self, cmd: &Command) -> Result<Outcome> {
        use crate::dspjob::StackEntry;
        let option = cmd.get("OPTION").trim().to_ascii_uppercase();
        let mut f = self.job_facts(cmd.get("JOB")).await?;
        // The call stack as this process keeps it (QWVRCSTK's entries).
        let mut groups: Vec<String> = Vec::new();
        let mut in_use: Vec<String> = Vec::new();
        let mut last_ile = String::new();
        for e in crate::pgmmsg::stack_listed(&self.job_key()) {
            if f.system_job && e.lib == "QSYS" && e.pgm == "QP2USER2" {
                continue;
            }
            if e.lib == "QSYS" && e.pgm == "QCMDEXC" {
                f.stack.push(crate::dspjob::qcmdexc_entry());
                continue;
            }
            // Activation group numbers: the default one's is 2 (the
            // user state default group, b19_pgmstk); named groups are
            // numbered from 3 as they appear (docs/fidelity-gaps.md).
            let number = if e.actgrp == crate::ile::DEFAULT_GROUP || e.actgrp.is_empty() {
                2
            } else {
                match groups.iter().position(|g| *g == e.actgrp) {
                    Some(i) => i as u64 + 3,
                    None => {
                        groups.push(e.actgrp.clone());
                        groups.len() as u64 + 2
                    }
                }
            };
            let actgrp = if e.actgrp.is_empty() {
                crate::ile::DEFAULT_GROUP.to_string()
            } else {
                e.actgrp.clone()
            };
            in_use.push(actgrp.clone());
            let ile = !e.module.is_empty();
            let boundary = ile && last_ile != actgrp;
            if ile {
                last_ile = actgrp.clone();
            }
            f.stack.push(StackEntry {
                pgm: e.pgm.clone(),
                lib: crate::qtemp::logical(&e.lib),
                stmt: if e.line > 0 {
                    e.line.to_string()
                } else {
                    String::new()
                },
                actgrp,
                actgrp_number: number,
                module: e.module.clone(),
                // CRTBNDRPG and CRTBNDCL make the module in QTEMP.
                module_lib: if ile { "QTEMP".into() } else { String::new() },
                control_boundary: boundary,
                procedure: e.proc.clone(),
                ..Default::default()
            });
        }
        // The library list, and the activation groups: the two default
        // ones (b19_pgmstk: QCMDEXC's 1, the user programs' 2), then the
        // named ones on the call stack and those with service programs.
        for (lib, kind) in self.library_list_entries() {
            let text = catalog::library_text(self.client, &lib)
                .await
                .unwrap_or_default();
            f.libl.push((lib, kind.to_string(), text));
        }
        // The job's environment variables; CCSID 0 is the job's (37 here).
        for (name, v) in crate::envvar::job_vars(self.client, &self.job_key())
            .await
            .unwrap_or_default()
        {
            let ccsid = if v.ccsid == 0 { 37 } else { v.ccsid };
            f.envvars.push((name, v.value, ccsid));
        }
        for g in crate::ile::job_groups(&self.job_key()) {
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
        f.groups = vec![
            (crate::ile::DEFAULT_GROUP.to_string(), 1, true),
            (crate::ile::DEFAULT_GROUP.to_string(), 2, true),
        ];
        for (i, g) in groups.iter().enumerate() {
            f.groups.push((g.clone(), i as u64 + 3, in_use.contains(g)));
        }
        let sections = match option.as_str() {
            "*DFNA" => vec![crate::dspjob::definition(&f)],
            "*LIBL" => vec![crate::dspjob::library_list(&f)],
            "*ACTGRP" => vec![crate::dspjob::activation_groups(&f)],
            "*ENVVAR" => vec![crate::dspjob::environment_variables(&f)],
            "*STSA" => vec![crate::dspjob::status(&f)],
            "*RUNA" => vec![crate::dspjob::run(&f)],
            "*PGMSTK" => vec![crate::dspjob::stack(&f)],
            // In the order IBM i prints them (PUB400 2026-10-28,
            // tools/golden/verify20.txt e20_all), the sections not done
            // here left out.
            "*SELECT" | "*ALL" | "" => vec![
                crate::dspjob::status(&f),
                crate::dspjob::definition(&f),
                crate::dspjob::run(&f),
                crate::dspjob::stack(&f),
                crate::dspjob::library_list(&f),
                crate::dspjob::activation_groups(&f),
                crate::dspjob::environment_variables(&f),
            ],
            other => {
                bail!("DSPJOB OPTION({other}) is not supported yet (*DFNA, *STSA, *RUNA, *PGMSTK, *LIBL, *ACTGRP, *ENVVAR).")
            }
        };
        let rows: Vec<String> = sections.iter().flatten().cloned().collect();
        let print = crate::dspjob::print(&f, &sections, &crate::report::Stamp::now());
        if self.id.is_none() && cmd.get("OUTPUT") == "*PRINT" {
            // `system` writes the report out; no message (b19_dfna).
            self.calls.printed.extend(print);
            return Ok(Outcome::Messages(vec![]));
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Job".into(),
            print,
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

    /// CRTMSGQ: a *MSGQ object; its messages are kept under "LIB/NAME"
    /// (jobs::msgq_key). Completion message CPC9801.
    async fn crtmsgq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("MSGQ"));
        let lib = crate::qtemp::real(&lib.to_ascii_uppercase());
        let name = name.trim().to_ascii_uppercase();
        if !catalog::library_exists(self.client, &lib).await? {
            bail!(
                "CPF2110: There is no library {}.",
                crate::qtemp::logical(&lib)
            );
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        let n = self
            .client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                 VALUES ($1, $2, '*MSGQ', '', $3) ON CONFLICT DO NOTHING",
                &[&lib, &name, &text],
            )
            .await?;
        if n == 0 {
            bail!(
                "CPF2112: Object {name} in {} type *MSGQ already exists.",
                crate::qtemp::logical(&lib)
            );
        }
        // Messages left by an earlier queue of that name (a QTEMP that
        // went with its job) are not this queue's.
        self.client
            .execute(
                "DELETE FROM as4tainr.messages WHERE msgq=$1",
                &[&format!("{lib}/{name}")],
            )
            .await?;
        authority::set_owner(self.client, &lib, &name, "*MSGQ", &self.user).await?;
        // CPC9801 (PUB400 2026-10-26, tools/golden/verify1.txt v_crtmsgq).
        Ok(Outcome::Messages(vec![format!(
            "CPC9801: Message queue {name} was made in library {}.",
            crate::qtemp::logical(&lib)
        )]))
    }

    /// DLTMSGQ: the queue and its messages; CPC2191 / CPF2105 as for
    /// DLTDTAQ.
    async fn dltmsgq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("MSGQ"));
        let name = name.to_ascii_uppercase();
        let label = cmd
            .get("MSGQ")
            .to_ascii_uppercase()
            .split_once('/')
            .map(|x| x.0.to_string())
            .unwrap_or_else(|| "*LIBL".into());
        let mut msgs = Vec::new();
        for l in &libs {
            let objs = catalog::list_objects(self.client, l)
                .await
                .unwrap_or_default();
            for o in objs
                .iter()
                .filter(|o| o.objtype == "*MSGQ" && catalog::name_matches(&name, &o.name))
            {
                authority::check(self.client, &self.user, l, &o.name, "*MSGQ", Aut::All).await?;
                self.client
                    .execute(
                        "DELETE FROM as4tainr.messages WHERE msgq=$1",
                        &[&format!("{l}/{}", o.name)],
                    )
                    .await?;
                authority::forget(self.client, l, &o.name, "*MSGQ").await?;
                self.client
                    .execute(
                        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*MSGQ'",
                        &[l, &o.name],
                    )
                    .await?;
                msgs.push(format!(
                    "CPC2191: Object {} of type *MSGQ in library {} was deleted.",
                    o.name,
                    crate::qtemp::logical(l)
                ));
            }
            if !msgs.is_empty() && !name.ends_with('*') {
                break;
            }
        }
        if msgs.is_empty() {
            bail!("CPF2105: Object {name} of type *MSGQ in library {label} not found.");
        }
        Ok(Outcome::Messages(msgs))
    }

    async fn sndmsg(&mut self, cmd: &Command) -> Result<Outcome> {
        let queues: Vec<String> = cmd
            .list("TOMSGQ")
            .iter()
            .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .filter(|q| q != "*NONE")
            .collect();
        if !queues.is_empty() {
            let kind = match cmd.get("MSGTYPE") {
                "*INQ" => "*INQ",
                _ => "*INFO",
            };
            let from_job = self.id.as_ref().map(|i| i.to_string()).unwrap_or_default();
            let mut keys = Vec::new();
            for q in &queues {
                let q = q.to_ascii_uppercase();
                let (libs, name) = match q.split_once('/') {
                    Some(_) => self.search(&q),
                    None => (self.library_list(), q.clone()),
                };
                let mut libs = libs;
                if q.starts_with("*LIBL/") || !q.contains('/') {
                    libs.push("QUSRSYS".into());
                }
                match crate::jobs::msgq_key(self.client, &libs, &name).await? {
                    Some(k) => keys.push(k),
                    None => {
                        let lib = q.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
                        bail!("CPF2403: Message queue {name} in {lib} not found.");
                    }
                }
            }
            for k in &keys {
                crate::jobs::send_message(
                    self.client,
                    k,
                    &self.user,
                    &from_job,
                    kind,
                    cmd.get("MSG"),
                )
                .await?;
            }
            return Ok(Outcome::Messages(vec![]));
        }
        let to = cmd.get("TOUSR").to_string();
        if to.is_empty() {
            bail!("CPF0006: The command has errors and was not run. SNDMSG needs TOUSR or TOMSGQ.");
        }
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
        let objtype = cmd.get("OBJTYPE").trim().to_ascii_uppercase();
        if !objtype.starts_with('*') || objtype.len() < 2 {
            bail!("CPD0084: Object type {objtype} is not valid.");
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

    /// GRTOBJAUT and RVKOBJAUT. GRTOBJAUT (CL reference): REPLACE(*NO)
    /// adds the authority to what the user has (as4tainr keeps the
    /// system-defined authorities, so the greater of the two; *EXCLUDE
    /// replaces), REPLACE(*YES) replaces it; REFOBJ gives the object the
    /// reference object's *PUBLIC and private authorities.
    async fn objaut(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, objtype) = self.object_named(cmd).await?;
        authority::check_manage(self.client, &self.user, &lib, &name, &objtype).await?;
        let grant = cmd.def.name == "GRTOBJAUT";
        let replace = cmd.get("REPLACE").eq_ignore_ascii_case("*YES");
        let refobj = cmd.get("REFOBJ").to_ascii_uppercase();
        let users_given = cmd.list("USER");
        if grant && !refobj.is_empty() && refobj != "*NONE" {
            let reftype = match cmd.get("REFOBJTYPE").to_ascii_uppercase().as_str() {
                "" | "*OBJTYPE" => objtype.clone(),
                t => t.to_string(),
            };
            let (rlib, rname) = match refobj.split_once('/') {
                Some(("*LIBL", n)) | Some(("", n)) => ("*LIBL".to_string(), n.to_string()),
                Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
                Some((l, n)) => (crate::qtemp::real(l), n.to_string()),
                None => ("*LIBL".to_string(), refobj.clone()),
            };
            let libs = if rlib == "*LIBL" {
                self.library_list()
            } else {
                vec![rlib.clone()]
            };
            let mut found = None;
            for l in &libs {
                if let Some(a) = authority::get(self.client, l, &rname, &reftype).await? {
                    found = Some((l.clone(), a));
                    break;
                }
            }
            let Some((rlib, a)) = found else {
                bail!("CPF9801: Object {rname} in library {rlib} type {reftype} not found.");
            };
            let mut give: Vec<(String, Aut)> = a.private.clone();
            give.push(("*PUBLIC".to_string(), a.public));
            for (u, aut) in give {
                let aut = if replace || aut == Aut::Exclude {
                    aut
                } else {
                    aut.max(
                        self.held_authority(&lib, &name, &objtype, &u)
                            .await?
                            .unwrap_or(aut),
                    )
                };
                authority::grant(self.client, &lib, &name, &objtype, &u, aut).await?;
            }
            return Ok(Outcome::Messages(vec![format!(
                "Authorities of {}/{rname} {reftype} given to {}/{name} {objtype}.",
                crate::qtemp::logical(&rlib),
                crate::qtemp::logical(&lib)
            )]));
        }
        if users_given.is_empty() {
            return Err(crate::cl::diag_error(
                "CPD0071",
                "Parameter USER needs a value.",
            ));
        }
        let aut = Aut::parse(cmd.get("AUT"))?;
        let mut done = Vec::new();
        for user in users_given {
            if user != "*PUBLIC" && users::get(self.client, &user).await?.is_none() {
                bail!("CPF2204: User profile {user} not found.");
            }
            if grant {
                let aut = if replace || aut == Aut::Exclude {
                    aut
                } else {
                    aut.max(
                        self.held_authority(&lib, &name, &objtype, &user)
                            .await?
                            .unwrap_or(aut),
                    )
                };
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

    /// The authority a user holds to an object of their own (a private
    /// authority; *PUBLIC's for *PUBLIC), if any.
    async fn held_authority(
        &self,
        lib: &str,
        name: &str,
        objtype: &str,
        user: &str,
    ) -> Result<Option<Aut>> {
        let Some(a) = authority::get(self.client, lib, name, objtype).await? else {
            return Ok(None);
        };
        if user == "*PUBLIC" {
            return Ok(Some(a.public));
        }
        Ok(a.private.iter().find(|(u, _)| u == user).map(|(_, x)| *x))
    }

    /// CHGOBJD: an object's text (TEXT *SAME, *BLANK or a text). DAYS
    /// (*RESET of the days used count) is accepted; as4tainr keeps no
    /// usage counts.
    async fn chgobjd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, objtype) = self.object_named(cmd).await?;
        authority::check(self.client, &self.user, &lib, &name, &objtype, Aut::Change).await?;
        match cmd.get("TEXT") {
            "*SAME" | "" => {}
            t => {
                let text = if t == "*BLANK" { "" } else { t };
                self.client
                    .execute(
                        "UPDATE as4tainr.objects SET text=$4 WHERE library=$1 AND name=$2 AND objtype=$3",
                        &[&lib, &name, &objtype, &text],
                    )
                    .await?;
            }
        }
        Ok(Outcome::Messages(vec![format!(
            "Description of object {name} in {lib} type {objtype} changed."
        )]))
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
        let old = authority::get(self.client, &lib, &name, &objtype)
            .await?
            .map(|a| a.owner)
            .unwrap_or_default();
        authority::set_owner(self.client, &lib, &name, &objtype, &new).await?;
        // CUROWNAUT(*SAME): the old owner keeps *ALL as a private
        // authority; *REVOKE (the default): it has none.
        if cmd.get("CUROWNAUT").eq_ignore_ascii_case("*SAME") && !old.is_empty() && old != new {
            authority::grant(self.client, &lib, &name, &objtype, &old, Aut::All).await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "Owner of {lib}/{name} {objtype} changed to {new}."
        )]))
    }

    async fn wrksplf(&mut self, cmd: &Command) -> Result<Outcome> {
        // SELECT(user print-device form-type user-data ASP) (CL reference,
        // WRKSPLF): the user and the user data select here.
        let select = cmd.list("SELECT");
        let udata = select
            .get(3)
            .map(|d| d.to_ascii_uppercase())
            .filter(|d| d != "*ALL");
        let user = match select.first().map(String::as_str).unwrap_or("*CURRENT") {
            "*CURRENT" | "" => self.user.clone(),
            u => {
                if u != self.user {
                    self.require_secadm("work with other users' spooled files")
                        .await?;
                }
                u.to_string()
            }
        };
        let mut files = crate::spool::for_user(self.client, &user).await?;
        if let Some(d) = &udata {
            files.retain(|f| f.user_data.eq_ignore_ascii_case(d));
        }
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
                    crate::outq::short_status(&f.status),
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
                (3, "Hold".into(), "HLDSPLF {}".into()),
                (4, "Delete".into(), "DLTSPLF {}".into()),
                (5, "Display".into(), "DSPSPLF {}".into()),
                (6, "Release".into(), "RLSSPLF {}".into()),
                (8, "Attributes".into(), "WRKSPLFA {}".into()),
            ],
            keys: files
                .iter()
                .map(|f| format!("FILE({}) JOB({}) SPLNBR({})", f.file, f.job_name, f.number))
                .collect(),
            refresh: Some(format!("WRKSPLF SELECT({user})")),
            print: vec![],
            ibm: IbmLayout::some(IbmLayout {
                // IBM i 7.5's option and key lines (PUB400,
                // tools/golden/screens4.txt 08_wrksplf): the options done
                // here at IBM's columns, a blank line, the keys on line 22.
                option_text: vec![vec![
                    wrk_cols(&[
                        (24, "3=Hold"),
                        (33, "4=Delete"),
                        (44, "5=Display"),
                        (56, "6=Release"),
                    ]),
                    wrk_cols(&[(4, "8=Attributes")]),
                    String::new(),
                ]],
                key_text: vec![
                    wrk_cols(&[(2, "F3=Exit"), (25, "F11=View 2"), (38, "F12=Cancel")]),
                    String::new(),
                ],
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
            print_msg: None,
            spool: None,
            member: None,
        }))
    }

    /// The spooled file a FILE/JOB/SPLNBR command names.
    pub(crate) async fn spooled_file(&self, cmd: &Command) -> Result<crate::spool::SpooledFile> {
        // `system` (no job of as4tainr's): its own job has no spooled
        // files (they are its print job's), so JOB(*) finds none: CPF3303
        // (PUB400 2026-10-26, tools/golden/spool.txt s_dump, GLDSPD's
        // DLTSPLF FILE(GLDSPL) SPLNBR(*LAST)).
        let spec = cmd.get("JOB").trim();
        if (spec == "*" || spec.is_empty()) && self.id.is_none() {
            bail!(
                "CPF3303: Spooled file {} is not in the job running the command.",
                cmd.get("FILE").to_ascii_uppercase()
            );
        }
        let job = self.job_named(cmd.get("JOB")).await?;
        let number = match cmd.get("SPLNBR") {
            "*LAST" | "*ONLY" | "" => None,
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
        let view = SpoolView::of_pages(&f.file, &pages);
        Ok(Outcome::Display(ListDisplay {
            title: "Display Spooled File".into(),
            rows: view.lines.iter().map(|(_, _, t)| t.clone()).collect(),
            spool: Some(Box::new(view)),
            ..Default::default()
        }))
    }

    /// CPYSPLF (CL reference): the spooled file's lines as records of a
    /// physical file with one character field (CRTPF RCDLEN) or of a
    /// source member; MBROPT *REPLACE or *ADD; CTLCHAR(*FCFC) puts the
    /// first-character forms control in front ('1' for a page's first
    /// line, blank for the others). The lines are as the pages print,
    /// blank lines included (fidelity-gaps).
    async fn cpysplf(&mut self, cmd: &Command) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        let pages = crate::spool::pages(self.client, f.id).await?;
        let fcfc = cmd.get("CTLCHAR").eq_ignore_ascii_case("*FCFC");
        let mut lines = Vec::new();
        for page in &pages {
            for (k, l) in page.iter().enumerate() {
                lines.push(if fcfc {
                    format!("{}{l}", if k == 0 { '1' } else { ' ' })
                } else {
                    l.clone()
                });
            }
        }
        let (libs, name) = self.search(cmd.get("TOFILE"));
        let name = name.to_ascii_uppercase();
        let add = cmd.get("MBROPT").eq_ignore_ascii_case("*ADD");
        let mut source_lib = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &name).await? {
                source_lib = Some(l.clone());
                break;
            }
        }
        let target_lib: String;
        if let Some(lib) = source_lib {
            target_lib = lib.clone();
            let mbr = match cmd.get("TOMBR").to_ascii_uppercase().as_str() {
                "" | "*FIRST" => name.clone(),
                m => m.to_string(),
            };
            let old =
                crate::source::find_member(self.client, std::slice::from_ref(&lib), &name, &mbr)
                    .await
                    .ok();
            let mut text = match (&old, add) {
                (Some(o), true) if !o.source.is_empty() => format!("{}\n", o.source),
                _ => String::new(),
            };
            text.push_str(&lines.join("\n"));
            crate::source::put_member(
                self.client,
                &crate::source::Member {
                    library: lib.clone(),
                    file: name.clone(),
                    name: mbr,
                    srctype: old.as_ref().map(|o| o.srctype.clone()).unwrap_or_default(),
                    text: old.as_ref().map(|o| o.text.clone()).unwrap_or_default(),
                    source: text,
                    ..Default::default()
                },
                true,
            )
            .await?;
        } else {
            let to = catalog::resolve_file(self.client, &libs, &name)
                .await
                .map_err(|_| {
                    anyhow!(
                        "CPF9812: There is no file {name} to copy spooled file {} into.",
                        f.file
                    )
                })?;
            authority::check(
                self.client,
                &self.user,
                &to.library,
                &name,
                "*FILE",
                Aut::Change,
            )
            .await?;
            target_lib = to.library.clone();
            // The lines go into the record as its bytes, whatever its
            // fields (PUB400 2026-10-27, verify9 d9_spl_two, d9_spl_short:
            // two character fields, one short field); data cut off at the
            // record length is CPF3370, once; bytes a numeric field cannot
            // take end the copy with CPF9846 after CPF3485 for the records
            // copied (d9_spl_fk).
            let reclen = crate::cpyf::record_length(&to.file);
            let table = to.table();
            if !add {
                self.client
                    .batch_execute(&format!("DELETE FROM {table}"))
                    .await?;
            }
            let shown = crate::qtemp::logical(&to.library);
            let mut diags = Vec::new();
            if lines.iter().any(|l| l.trim_end().chars().count() > reclen) {
                diags.push(format!(
                    "CPF3370: Not all of the data fitted into the records of file {name} in {shown}."
                ));
            }
            let mut writer = crate::rla::OpenFile::new(to.clone());
            for (done, l) in lines.iter().enumerate() {
                let mut img = crate::tn5250::ebcdic::encode(&format!("{l:<reclen$}"));
                img.truncate(reclen);
                let rec = match crate::cpyf::image_record(&to.file, &img) {
                    Ok(r) => r,
                    Err(_) => {
                        diags.push(format!(
                            "CPF3485: {done} records were copied to file {name} in {shown}."
                        ));
                        diags.push(format!(
                            "CPF9846: Record {} of file {name} in library {shown} could not be written: its data does not fit the fields.",
                            done + 1
                        ));
                        bail!("{}", diags.join("\n"));
                    }
                };
                writer.write(self.client, &rec).await?;
            }
            if !diags.is_empty() {
                diags.push(format!(
                    "CPF3485: {} records were copied to file {name} in {shown}.",
                    lines.len()
                ));
                return Ok(Outcome::Messages(diags));
            }
        }
        // CPF3485, the completion message (PUB400 2026-10-27, verify8
        // c8_joblog7: CPYSPLF of a job log into a RCDLEN(132) file).
        Ok(Outcome::Messages(vec![format!(
            "CPF3485: {} records were copied to file {name} in {}.",
            lines.len(),
            crate::qtemp::logical(&target_lib)
        )]))
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
            match cmd.get("TEXT") {
                "*BLANK" => "",
                t => t,
            },
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
        let is_char = matches!(d.ty, dtaara::Type::Char);
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
            heading: if is_char {
                format!(
                    "{:<12}{}",
                    "Offset", "*...+....1....+....2....+....3....+....4....+....5"
                )
            } else {
                String::new()
            },
            rows: if is_char { rows } else { vec![] },
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(62),
                lines: {
                    let mut l = vec![
                        format!("{}   {}", dsp_label("Data area", 22), d.name),
                        format!("{}     {}", dsp_label("  Library", 22), d.library),
                        format!("{}   {}", dsp_label("Type", 22), d.ty.code()),
                        format!("{}   {}", dsp_label("Length", 22), d.length_text()),
                        format!("{}   {}", dsp_label("Text", 22), d.text),
                    ];
                    // A *DEC or *LGL value is one more header line (IBM i
                    // 7.5, PUB400: "Value . . . :   0,00" with QDECFMT J).
                    if !is_char {
                        l.push(format!("{}   {}", dsp_label("Value", 22), d.value));
                    }
                    l
                },
                heading2: if is_char {
                    format!("{:11}Value", "")
                } else {
                    String::new()
                },
                enter_line: true,
                no_list: !is_char,
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
        // OBJTYPE takes an object type only (the command analyzer's
        // check comes before the object is looked for: CPD0084, CPF0006).
        if !crate::cl::OBJECT_TYPES.contains(&objtype.as_str()) {
            return Err(crate::cl::value_error(&format!(
                "Value '{objtype}' for parameter OBJTYPE is not an object type."
            )));
        }
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
        if cmd.get("OUTPUT").eq_ignore_ascii_case("*OUTFILE") {
            return self.dspobjd_outfile(cmd, &groups).await;
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

    /// DSPOBJD OUTPUT(*OUTFILE): a record per object in the format of
    /// QSYS/QADSPOBJ (QLIDOBJD; outfile.rs), DETAIL(*BASIC)'s fields:
    /// dates are MMDDYY (the creation, change and display dates) or
    /// YYMMDD (the source's change date), each with its century field
    /// (0 = 19xx, 1 = 20xx), times HHMMSS (the model's field
    /// descriptions, PUB400 2026-10-26, tools/golden/verify1.txt
    /// v_ffd_objd). Fields as4tainr has no value for are blank or zero
    /// (fidelity-gaps).
    async fn dspobjd_outfile(
        &mut self,
        cmd: &Command,
        groups: &[(String, Vec<catalog::ObjectEntry>)],
    ) -> Result<Outcome> {
        let outfile = cmd.get("OUTFILE").trim().to_string();
        // PUB400 2026-10-26, verify2 o_objd_nofile: CPD9861.
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
        let now = self
            .client
            .query_one(
                "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY'),
                        to_char(now() AT TIME ZONE 'UTC', 'YYMMDD'),
                        to_char(now() AT TIME ZONE 'UTC', 'HH24MISS')",
                &[],
            )
            .await?;
        let (ny, nd, nt): (String, String, String) = (now.get(0), now.get(1), now.get(2));
        let system = crate::report::system_name();
        let mut records = Vec::new();
        for (lib, objs) in groups {
            for o in objs {
                let size = catalog::object_size(self.client, lib, &o.name, &o.objtype)
                    .await
                    .unwrap_or(0)
                    .max(0) as u64;
                let journaled = o.objtype == "*FILE"
                    && self
                        .client
                        .query_opt(
                            "SELECT 1 FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
                            &[lib, &o.name],
                        )
                        .await?
                        .is_some();
                let r = self
                    .client
                    .query_opt(
                        "SELECT to_char(created AT TIME ZONE 'UTC', 'YYYY'),
                                to_char(created AT TIME ZONE 'UTC', 'MMDDYY'),
                                to_char(created AT TIME ZONE 'UTC', 'HH24MISS'),
                                to_char(coalesce(changed, created) AT TIME ZONE 'UTC', 'YYYY'),
                                to_char(coalesce(changed, created) AT TIME ZONE 'UTC', 'MMDDYY'),
                                to_char(coalesce(changed, created) AT TIME ZONE 'UTC', 'HH24MISS'),
                                coalesce(owner, ''), coalesce(src_file, ''), coalesce(src_lib, ''),
                                coalesce(src_mbr, ''),
                                coalesce(to_char(nullif(src_changed, '')::timestamptz AT TIME ZONE 'UTC', 'YYYY'), ''),
                                coalesce(to_char(nullif(src_changed, '')::timestamptz AT TIME ZONE 'UTC', 'YYMMDD'), ''),
                                coalesce(to_char(nullif(src_changed, '')::timestamptz AT TIME ZONE 'UTC', 'HH24MISS'), ''),
                                coalesce(user_attr, '')
                           FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                        &[lib, &o.name, &o.objtype],
                    )
                    .await?;
                let g = |i: usize| -> String {
                    r.as_ref()
                        .and_then(|r| r.try_get::<_, Option<String>>(i).ok().flatten())
                        .unwrap_or_default()
                };
                let mut rec: crate::outfile::Record = vec![
                    ("ODDCEN", crate::outfile::century(&ny)),
                    ("ODDDAT", crate::outfile::job_date(&nd)),
                    ("ODDTIM", nt.clone()),
                    ("ODLBNM", crate::qtemp::logical(lib)),
                    ("ODOBNM", o.name.clone()),
                    ("ODOBTP", o.objtype.clone()),
                    ("ODOBAT", o.attribute.clone()),
                    ("ODOBFR", "0".into()),
                    ("ODOBSZ", size.to_string()),
                    ("ODOBTX", o.text.clone()),
                    ("ODOBLK", "0".into()),
                    ("ODOBDM", "0".into()),
                    ("ODOBOW", g(6)),
                    ("ODSRCF", g(7)),
                    ("ODSRCL", g(8)),
                    ("ODSRCM", g(9)),
                    ("ODOBSY", system.clone()),
                    ("ODCRTU", g(6)),
                    ("ODCRTS", system.clone()),
                    ("ODASP", "1".into()),
                    ("ODUATR", g(13)),
                    // Sizes in units of 512 bytes, the bytes per unit a
                    // data area's record shows (PUB400 2026-10-26, verify2
                    // o_objd_vals: 16 units of 512 for 8192 bytes).
                    ("ODSIZU", size.div_ceil(512).to_string()),
                    ("ODBPUN", "512".into()),
                    ("ODDTL", "*BASIC".into()),
                    // Allow change by API, and the system ASP for the
                    // object and its library (o_objd_vals: 1, 1, 1).
                    ("ODAAPI", "1".into()),
                    ("ODASPL", "1".into()),
                    ("ODLASN", "1".into()),
                    ("ODJRST", if journaled { "1" } else { "0" }.into()),
                ];
                if !g(0).is_empty() {
                    rec.extend([
                        ("ODCCEN", crate::outfile::century(&g(0))),
                        ("ODCDAT", g(1)),
                        ("ODCTIM", g(2)),
                        ("ODLCEN", crate::outfile::century(&g(3))),
                        ("ODLDAT", g(4)),
                        ("ODLTIM", g(5)),
                    ]);
                }
                if !g(10).is_empty() {
                    rec.extend([
                        ("ODSRCC", crate::outfile::century(&g(10))),
                        ("ODSRCD", g(11)),
                        ("ODSRCT", g(12)),
                    ]);
                }
                records.push(rec);
            }
        }
        let w = crate::outfile::write(self, &outfile, &option, &crate::outfile::QADSPOBJ, &records)
            .await?;
        Ok(Outcome::Messages(w.messages))
    }

    /// CRTDTAQ (CL reference). TYPE(*DDM) is not done; MAXLEN 1-64512,
    /// KEYLEN 1-256 with SEQ(*KEYED).
    async fn crtdtaq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("DTAQ"));
        let lib = lib.to_ascii_uppercase();
        if cmd.get("TYPE") == "*DDM" {
            bail!("CPF9898: as4tainr has no DDM data queues (TYPE(*DDM)).");
        }
        let maxlen = number(cmd.get("MAXLEN"), "MAXLEN")? as i32;
        if !(1..=crate::dtaq::MAX_ENTRY).contains(&maxlen) {
            bail!("CPF0006: The command has errors and was not run. MAXLEN must be 1 to 64512.");
        }
        let seq = match cmd.get("SEQ") {
            "*LIFO" => 'L',
            "*KEYED" => 'K',
            _ => 'F',
        };
        let keylen = if seq == 'K' {
            let k = match cmd.get("KEYLEN") {
                "" => bail!(
                    "CPF0006: The command has errors and was not run. SEQ(*KEYED) needs KEYLEN."
                ),
                k => number(k, "KEYLEN")? as i32,
            };
            if !(1..=crate::dtaq::MAX_KEY).contains(&k) {
                bail!("CPF0006: The command has errors and was not run. KEYLEN must be 1 to 256.");
            }
            k
        } else {
            0
        };
        let size: Vec<String> = cmd
            .list("SIZE")
            .iter()
            .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .collect();
        let size_max = match size.first().map(String::as_str) {
            Some("*MAX2GB") => -2,
            Some("*MAX16MB") | None => -1,
            Some(n) => number(n, "SIZE")? as i32,
        };
        let size_init = match size.get(1).map(String::as_str) {
            Some("*DEFAULT") | None => 16,
            Some(n) => number(n, "SIZE")? as i32,
        };
        let yes = |k: &str| cmd.get(k) == "*YES";
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        crate::dtaq::create(
            self.client,
            &crate::dtaq::Create {
                library: &lib,
                name: &name,
                maxlen,
                seq,
                keylen,
                senderid: yes("SENDERID"),
                force: yes("FORCE"),
                autorcl: yes("AUTORCL"),
                size_max,
                size_init,
                text,
            },
        )
        .await?;
        let name = name.to_ascii_uppercase();
        authority::set_owner(self.client, &lib, &name, "*DTAQ", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Data queue {name} was created in library {lib}."
        )]))
    }

    /// DLTDTAQ: CPC2191 as for the other DLTxxx commands; CPF2105 when the
    /// queue is not found (as IBM i 7.5 does for DLTDTAARA).
    async fn dltdtaq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("DTAQ"));
        let name = name.to_ascii_uppercase();
        let label = cmd
            .get("DTAQ")
            .to_ascii_uppercase()
            .split_once('/')
            .map(|x| x.0.to_string())
            .unwrap_or_else(|| "*LIBL".into());
        let mut msgs = Vec::new();
        for l in &libs {
            let objs = catalog::list_objects(self.client, l)
                .await
                .unwrap_or_default();
            let hits: Vec<_> = objs
                .iter()
                .filter(|o| o.objtype == "*DTAQ" && catalog::name_matches(&name, &o.name))
                .collect();
            for o in &hits {
                authority::check(self.client, &self.user, l, &o.name, "*DTAQ", Aut::All).await?;
                crate::dtaq::delete(self.client, l, &o.name).await?;
                msgs.push(format!(
                    "CPC2191: Object {} of type *DTAQ in library {l} was deleted.",
                    o.name
                ));
            }
            if !hits.is_empty() && !name.ends_with('*') {
                break;
            }
        }
        if msgs.is_empty() {
            bail!("CPF2105: Object {name} of type *DTAQ in library {label} not found.");
        }
        Ok(Outcome::Messages(msgs))
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
            Some((l, n)) => (vec![crate::qtemp::real(l)], n.to_string()),
            None => (self.library_list(), pgm.clone()),
        };
        // System APIs done here (in QSYS on IBM i): data queues, user
        // spaces, list and retrieve APIs (sysapi.rs).
        if crate::sysapi::is_api(&name)
            && matches!(pgm.split_once('/'), None | Some(("*LIBL" | "QSYS", _)))
        {
            let caller = self.dtaq_caller();
            let mut a = crate::dtaq::ClArgs(args);
            crate::sysapi::call_api(self.client, &caller, &name, &mut a).await?;
            self.calls.returned = Some(a.0);
            return Ok(Outcome::Messages(vec![]));
        }
        // QCMDEXC (command, its length): the command runs here, its
        // messages to the caller (Execute Command API).
        if name == "QCMDEXC" && matches!(pgm.split_once('/'), None | Some(("*LIBL" | "QSYS", _))) {
            let cmd = args.first().cloned().unwrap_or_default();
            let len = args
                .get(1)
                .and_then(|l| l.trim().parse::<f64>().ok())
                .map(|l| l.max(0.0) as usize);
            let cmd: String = match len {
                Some(n) => cmd.chars().take(n).collect(),
                None => cmd,
            };
            let r = self.run_boxed(cmd.trim()).await;
            self.calls.returned = Some(args.clone());
            return r;
        }
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
                        program.pep = p.attribute == "CLLE";
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
                                crate::rpg::interp::new_group()
                            } else {
                                g
                            };
                        }
                        let ile = program.pep;
                        let r = match crate::clpgm::run(self, &program, &args).await {
                            Err(e) if ile => Err(unmonitored_in_ile_cl(&name, e)),
                            Err(e) => Err(unmonitored_in_opm_cl(&name, e)),
                            r => r,
                        };
                        // ACTGRP(*NEW): the group ends with the program,
                        // and the programs that ran in it (*CALLER) with
                        // it, their storage and files (ILE Concepts,
                        // "Activation group deletion").
                        if program.actgrp.starts_with("*NEW") {
                            crate::ile::end_activations(
                                &self.job_key(),
                                &crate::ile::Groups::Named(&program.actgrp),
                            );
                            crate::rpg::interp::close_ended_spools(self.client).await;
                        }
                        r
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

    /// The job as the data queue APIs see it.
    fn dtaq_caller(&self) -> crate::dtaq::Caller {
        crate::dtaq::Caller {
            user: self.user.clone(),
            job: self.id.clone(),
            libl: self.library_list(),
            curlib: self.current_library(),
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
                caller_group: crate::pgmmsg::stack_listed(&self.job_key())
                    .last()
                    .map(|e| e.actgrp.clone()),
            }),
            pgm.to_string(),
            source,
            args,
        )
        .await?;
        self.calls.returned = Some(r.params.clone());
        let mut msgs: Vec<String> = r.messages.iter().map(|m| format!("DSPLY  {m}")).collect();
        // Without a job there are no spooled files: printed lines come back
        // as the command's printed output (PASE `system` writes a
        // program's spooled output to standard output).
        self.calls.printed.extend(r.printed.iter().cloned());
        // Its messages for this job's job log.
        self.calls.log_from_program(r.logged.clone());
        if let Some(e) = r.error {
            msgs.push(format!("{e:#}"));
            msgs.push(format!("Program {pgm} ended abnormally."));
            return Err(anyhow!(msgs.join("\n")));
        }
        Ok(Outcome::Messages(msgs))
    }
}
/// A line of texts at screen columns, shown from column 2.
fn wrk_cols(fields: &[(usize, &str)]) -> String {
    let shifted: Vec<(usize, &str)> = fields.iter().map(|(c, t)| (c - 1, *t)).collect();
    crate::report::at(&shifted)
}

/// The values of KEYWORD(...) in a string of options (CRTSQLRPGI
/// COMPILEOPT), split at blanks: empty when it is not there.
fn option_values(opts: &str, keyword: &str) -> Vec<String> {
    let u = opts.to_ascii_uppercase();
    let pat = format!("{keyword}(");
    let Some(i) = u
        .find(&pat)
        .filter(|i| *i == 0 || !u.as_bytes()[i - 1].is_ascii_alphanumeric())
    else {
        return Vec::new();
    };
    let rest = &u[i + pat.len()..];
    let mut depth = 1;
    let mut end = rest.len();
    for (j, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = j;
                    break;
                }
            }
            _ => {}
        }
    }
    rest[..end]
        .split_whitespace()
        .map(|v| v.trim_matches(|c| c == '(' || c == ')').to_string())
        .filter(|v| !v.is_empty())
        .collect()
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

/// How a script's statements are committed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptCommit {
    /// Each statement when it runs; COMMIT does nothing (`as4tainr
    /// import`, `run_sql_script`).
    Auto,
    /// COMMIT(*NONE) or *NC: COMMIT and ROLLBACK are SQL7007.
    NoCommit,
    /// A commit level: a unit of work of the script's own.
    Level(String),
}

/// A script's run (`Job::run_script`).
struct ScriptRun {
    lines: Vec<String>,
    msgs: Vec<crate::sqlstmlist::Msg>,
    failed: Option<anyhow::Error>,
}

/// The listing's message for a statement that ran: IBM's identifier for
/// the kinds of statements a capture shows (PUB400 2026-10-26,
/// tools/golden/verify2.txt w_script and w_rb: SQL790F for SET SCHEMA,
/// SQL7905 with severity 20 for CREATE TABLE in a library without a
/// journal, which as4tainr's libraries never have, SQL7988 for ALTER
/// TABLE, SQL7956 for INSERT; 2026-10-27, verify3.txt x_ok: SQL7957 for
/// UPDATE, SQL0100 for a change of no rows, SQL7951 for CREATE VIEW,
/// SQL7953 for DROP; verify4.txt y_ids: SQL7955 for DELETE, SQL7950 for
/// CREATE TABLE journaled to QSQJRN, SQL7954 for CREATE INDEX, SQL7965
/// for LABEL ON and COMMENT ON), none for the others.
fn listing_message(
    words: &[String],
    text: &str,
    msg: &dyn Fn(&str, u32, &str) -> crate::sqlstmlist::Msg,
) -> crate::sqlstmlist::Msg {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["SET", "SCHEMA", ..] | ["SET", "CURRENT", "SCHEMA", ..] => msg("SQL790F", 0, text),
        // SET of a global variable: SQL7948 (PUB400 2026-10-30, verify30
        // w30_script).
        ["SET", ..] if text.starts_with("Variable ") => msg("SQL7948", 0, text),
        // Journaled to the library's QSQJRN: SQL7950 (verify4 y_ids);
        // else SQL7905, severity 20.
        ["CREATE", "TABLE", ..] | ["CREATE", "OR", "REPLACE", "TABLE", ..]
            if text.ends_with(crate::ddl::JOURNALED) =>
        {
            msg("SQL7950", 0, text)
        }
        ["CREATE", "TABLE", ..] | ["CREATE", "OR", "REPLACE", "TABLE", ..] => {
            msg("SQL7905", 20, &format!("{text} It is not journaled."))
        }
        ["CREATE", "INDEX", ..] | ["CREATE", "UNIQUE", "INDEX", ..] => msg("SQL7954", 0, text),
        ["LABEL", "ON", ..] | ["COMMENT", "ON", ..] => msg("SQL7965", 0, text),
        ["ALTER", "TABLE", ..] => msg("SQL7988", 0, text),
        // A change that found no rows: SQL0100 (verify3 x_ok, x_errors).
        ["INSERT" | "UPDATE" | "DELETE", ..] if text.starts_with("0 rows") => {
            msg("SQL0100", 0, text)
        }
        ["INSERT", ..] => msg("SQL7956", 0, text),
        ["UPDATE", ..] => msg("SQL7957", 0, text),
        ["DELETE", ..] => msg("SQL7955", 0, text),
        ["CREATE", "VIEW", ..] | ["CREATE", "OR", "REPLACE", "VIEW", ..] => msg("SQL7951", 0, text),
        ["DROP", ..] => msg("SQL7953", 0, text),
        _ => msg("", 0, text),
    }
}

/// A time in microseconds since 1970 (UTC) as a listing shows it.
/// The messages of a failed create, for a listing's summary: each
/// `ID: text` line, severity 40 (verify5 z_crtpf_dup: CPF5813 and CPF7302
/// both 40).
fn finals_of(e: &anyhow::Error) -> Vec<(String, u32, String)> {
    format!("{e:#}")
        .lines()
        .filter_map(|l| {
            let (id, text) = l.split_once(": ")?;
            (id.len() == 7 && id.chars().all(|c| c.is_ascii_alphanumeric()))
                .then(|| (id.to_string(), 40, text.to_string()))
        })
        .collect()
}

fn stamp_of_us(us: i64) -> crate::report::Stamp {
    let ((y, m, d), (h, mi, sec), _) = crate::rpg::dates::from_epoch_us(us);
    crate::report::Stamp::of((y, m, d), (h, mi, sec))
}

/// An escape message an ILE CL program did not monitor ends the program,
/// and its caller gets CEE9901 naming that message, the program and the
/// statement (PUB400 2026-10-29, tools/golden/verify24.txt q24_call: a
/// CALL of a program that was not there, CPF0001 unmonitored at
/// statement 4700 of a stream file source, line 47; `system` shows only
/// CEE9901). An escape the program sent to its caller (SNDPGMMSG
/// MSGTYPE(*ESCAPE)) is not one: it arrives as it was sent.
fn unmonitored_in_ile_cl(name: &str, e: anyhow::Error) -> anyhow::Error {
    let Some((line, before)) = unmonitored(name, &e) else {
        return e;
    };
    let id = crate::jobs::escape_id(&before);
    // From an OPM program it called (CPF9999): CEE9901 names that
    // program, its message and its statement (PUB400 2026-10-30,
    // tools/golden/verify26.txt s26_call_opm).
    if id == "CPF9999" {
        if let Some(l) = before
            .lines()
            .rev()
            .find(|l| l.starts_with("CPF9999: Program "))
        {
            return anyhow!("CEE9901: {}", &l["CPF9999: ".len()..]);
        }
    }
    anyhow!(
        "CEE9901: Program {name} ended: escape message {id} was not monitored at statement {:010} (line {line}).",
        line * 100
    )
}

/// An escape message an OPM CL program (CRTCLPGM) did not monitor: a
/// function check, and with the default reply the caller gets CPF9999
/// (PUB400 2026-10-30, tools/golden/verify26.txt s26_rows: a calling
/// program monitoring CPF0001, CEE9901 and CPF9999 received CPF9999).
fn unmonitored_in_opm_cl(name: &str, e: anyhow::Error) -> anyhow::Error {
    let Some((line, before)) = unmonitored(name, &e) else {
        return e;
    };
    let id = crate::jobs::escape_id(&before);
    anyhow!(
        "CPF9999: Program {name} ended: escape message {id} was not monitored at statement {} (line {line}).",
        line * 100
    )
}

/// The line where CL program `name` stopped on a message it did not
/// monitor, and the messages before (None: it did not stop that way).
fn unmonitored(name: &str, e: &anyhow::Error) -> Option<(u64, String)> {
    let text = format!("{e:#}");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let rest = lines
        .last()?
        .strip_prefix(&format!("Error in CL program {name} "))?;
    let line: u64 = rest
        .strip_prefix("at line ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let before = lines[..lines.len() - 1].join("\n");
    if crate::jobs::escape_id(&before).is_empty() {
        return None;
    }
    Some((line, before))
}

/// The comment RTVBNDSRC writes after STRPGMEXP: a box naming the object's
/// type, name and library and when the source was retrieved (yy/mm/dd
/// and hh:mm:ss), in the columns PUB400 wrote them (2026-10-29,
/// tools/golden/verify24.txt q24_print_rtv, q24_print_rtvm).
fn binder_comment(objtype: &str, name: &str, lib: &str) -> Vec<String> {
    let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::now();
    let line = format!(
        "/*   {objtype:<13}{name:<13}{:<13}{:02}/{mo:02}/{d:02}  {h:02}:{mi:02}:{s:02}",
        crate::qtemp::logical(lib),
        y.rem_euclid(100)
    );
    let rule = format!("/*{}*/", "*".repeat(66));
    vec![rule.clone(), format!("{line:<68}*/"), rule]
}

/// RNF1324 (severity 20): a module's control specification names
/// DFTACTGRP, ACTGRP or USRPRF, which only a program (CRTBNDRPG) takes;
/// CRTRPGMOD then stops (PUB400 2026-10-30, tools/golden/verify29.txt
/// v29_vlst, v29_dlst: the message points at the keyword, marker a).
pub(crate) fn module_control_keywords(source: &str) -> Vec<crate::rpg::check::Diag> {
    let mut out = Vec::new();
    let mut in_ctl = false;
    let free = source
        .lines()
        .next()
        .is_some_and(|l| l.trim_start().to_ascii_uppercase().starts_with("**FREE"));
    for (i, line) in source.lines().enumerate() {
        let u = line.to_ascii_uppercase();
        let fixed_h = !free
            && u.len() > 6
            && u.as_bytes()[5] == b'H'
            && !matches!(u.as_bytes()[6], b'*' | b'/');
        let code = u.split("//").next().unwrap_or("");
        let starts = code.trim_start().starts_with("CTL-OPT");
        if !(fixed_h || starts || in_ctl) {
            continue;
        }
        if starts {
            in_ctl = true;
        }
        let area = if fixed_h { &u[6.min(u.len())..] } else { code };
        let base = if fixed_h { 6 } else { 0 };
        let chars: Vec<char> = area.chars().collect();
        let mut k = 0;
        while k < chars.len() {
            if chars[k].is_ascii_alphabetic() && (k == 0 || !chars[k - 1].is_ascii_alphanumeric()) {
                let start = k;
                while k < chars.len() && (chars[k].is_ascii_alphanumeric() || chars[k] == '-') {
                    k += 1;
                }
                let word: String = chars[start..k].iter().collect();
                if matches!(word.as_str(), "DFTACTGRP" | "ACTGRP" | "USRPRF") {
                    out.push(crate::rpg::check::Diag {
                        line: i + 1,
                        id: "RNF1324".into(),
                        sev: 20,
                        text: format!(
                            "{word} is a program's keyword: DFTACTGRP, ACTGRP and USRPRF are not valid for a module."
                        ),
                        at: Some((base + start, word.len())),
                    });
                }
            } else {
                k += 1;
            }
        }
        if in_ctl && code.contains(';') {
            in_ctl = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::dsp_label;

    #[test]
    fn compile_options_of_crtsqlrpgi() {
        // COMPILEOPT passes options to the compiler (Db2 for i embedded
        // SQL programming, CRTSQLRPGI): the company_system corpus uses
        // COMPILEOPT('BNDDIR(APP) DFTACTGRP(*NO)').
        let opts = "BNDDIR(APP QC2LE) DFTACTGRP(*NO)";
        assert_eq!(super::option_values(opts, "BNDDIR"), ["APP", "QC2LE"]);
        assert_eq!(super::option_values(opts, "DFTACTGRP"), ["*NO"]);
        assert!(super::option_values(opts, "ACTGRP").is_empty());
        assert!(super::option_values("XBNDDIR(A)", "BNDDIR").is_empty());
    }

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
