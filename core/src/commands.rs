//! Running CL commands for a job. Shared by the 5250 command line and,
//! later, batch jobs and CL programs.

use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio_postgres::Client;

use crate::bridge;
use crate::catalog;
use crate::cl::{self, Command};

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
    SignOff,
}

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
