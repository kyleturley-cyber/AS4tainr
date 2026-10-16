//! Running CL commands for a job. Shared by the 5250 command line and,
//! later, batch jobs and CL programs.

use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio_postgres::Client;

use crate::bridge;
use crate::catalog;
use crate::cl::{self, Command};
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
    pub curlib: String,
    /// User part of the library list (system libraries are not modelled yet).
    pub libl: Vec<String>,
    /// The job's display, for programs that use display files.
    pub display: Option<bridge::DisplayHandle>,
}

/// A list shown on its own screen (DSPLIB, DSPLIBL, ...).
#[derive(Debug, Clone)]
pub struct ListDisplay {
    pub title: String,
    pub info: Vec<(String, String)>,
    pub heading: String,
    pub rows: Vec<String>,
}

#[derive(Debug)]
pub enum Outcome {
    /// Completion and informational messages, in order.
    Messages(Vec<String>),
    Display(ListDisplay),
    /// CHGPWD: show the Change Password screen.
    ChangePassword,
    SignOff,
}

/// DSPPFM shows at most this many records.
pub const DSPPFM_MAX_RECORDS: i64 = 9999;

impl Job<'_> {
    /// The full library list, current library first, without duplicates.
    pub fn library_list(&self) -> Vec<String> {
        let mut out = vec![self.curlib.clone()];
        for l in &self.libl {
            if !out.contains(l) {
                out.push(l.clone());
            }
        }
        out
    }

    pub async fn run(&mut self, line: &str) -> Result<Outcome> {
        let cmd = cl::parse(line)?;
        match cmd.def.name {
            "CRTLIB" => self.crtlib(&cmd).await,
            "DSPLIB" => self.dsplib(&cmd).await,
            "DSPLIBL" => self.dsplibl().await,
            "ADDLIBLE" => self.addlible(&cmd).await,
            "CALL" => self.call(&cmd).await,
            "CRTUSRPRF" => self.crtusrprf(&cmd).await,
            "DSPUSRPRF" => self.dspusrprf(&cmd).await,
            "DLTUSRPRF" => self.dltusrprf(&cmd).await,
            "CHGPWD" => Ok(Outcome::ChangePassword),
            "WRKOBJ" => self.wrkobj(&cmd).await,
            "DSPPFM" => self.dsppfm(&cmd).await,
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
            self.curlib.clone()
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
        }))
    }

    async fn dsplibl(&mut self) -> Result<Outcome> {
        let mut rows = Vec::new();
        for (i, lib) in self.library_list().iter().enumerate() {
            let kind = if i == 0 { "CUR" } else { "USR" };
            let text = catalog::library_text(self.client, lib)
                .await
                .unwrap_or_default();
            rows.push(format!("{lib:<10}  {kind:<4}  {text}"));
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Library List".into(),
            info: vec![],
            heading: format!("{:<10}  {:<4}  {}", "Library", "Type", "Text"),
            rows,
        }))
    }

    async fn addlible(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = cmd.get("LIB").to_string();
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} not found.");
        }
        if self.library_list().contains(&lib) {
            bail!("CPF2103: Library {lib} already exists in library list.");
        }
        match cmd.get("POSITION") {
            "*LAST" => self.libl.push(lib.clone()),
            _ => self.libl.insert(0, lib.clone()),
        }
        Ok(Outcome::Messages(vec![format!(
            "Library {lib} added to library list."
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
            "*CRTDFT" => "QGPL".to_string(),
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
            "*CURLIB" => vec![self.curlib.clone()],
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
        }))
    }

    async fn dsppfm(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = match cmd.get("FILE").split_once('/') {
            Some(("*LIBL", n)) => n.to_string(),
            Some(("*CURLIB", n)) => format!("{}/{n}", self.curlib),
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
        }))
    }

    /// CALL: run an RPG program on the runner, with file I/O through a
    /// bridge that uses this job's library list. DSPLY lines come back as
    /// messages.
    async fn call(&mut self, cmd: &Command) -> Result<Outcome> {
        let pgm = cmd.get("PGM").to_string();
        let listener = bridge::bind(0).await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(bridge::serve(
            listener,
            bridge::BridgeConfig {
                database_url: self.env.database_url.clone(),
                library_list: self.library_list(),
                display: self.display.clone(),
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
            .args(cmd.list("PARM"))
            .env("IBMISH_LIBL", self.library_list().join(","))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting runner '{}'", self.env.runner))?;
        let out = tokio::time::timeout(self.env.call_timeout, child.wait_with_output()).await;
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
