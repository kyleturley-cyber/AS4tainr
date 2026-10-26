//! Qshell from CL (QSH and STRQSH with CMD) and the environment variable
//! commands (ADDENVVAR, CHGENVVAR, RMVENVVAR, WRKENVVAR).
//!
//! Written from IBM's Qshell documentation, "Running Qshell commands from
//! CL": QSH CMD runs one command and ends; QIBM_QSH_CMD_OUTPUT sends its
//! output to STDOUT (the default), NONE, FILE=path (truncated first) or
//! FILEAPPEND=path; QSH0005 is sent when the command ends normally, with
//! its exit status, QSH0006 when it ends by a signal, with the signal
//! number, as completion messages, or as escape messages when
//! QIBM_QSH_CMD_ESCAPE_MSG is Y (QSH0005 only for an exit status above
//! 0). The exit status is the first 4 bytes of QSH0005's data, a binary
//! number (QshOni's QSHEXECC reads it with %BIN(&MSGDTA 1 4)).
//!
//! The command runs in the container, with /bin/sh, as the operating
//! system user as4tainr runs as: it is not limited by as4tainr's object
//! authorities, so QSH runs only when the system is set up for it
//! (AS4TAINR_QSH=1). The command sees the job's environment variables
//! (only those: not as4tainr's own), and the IFS at its paths in a mount
//! namespace of its own (`qshfs.rs`), or the container's file system
//! where mount namespaces are not allowed (docs/fidelity-gaps.md).

use crate::cl::{Command, Value};
use crate::commands::{Job, ListDisplay, Outcome};
use crate::envvar::{self, Level, Var};
use anyhow::{bail, Result};

/// Is Qshell set up on this system?
pub fn enabled() -> bool {
    std::env::var("AS4TAINR_QSH").is_ok_and(|v| matches!(v.as_str(), "1" | "yes" | "YES" | "Y"))
}

/// The value as typed: a quoted value keeps its case, a word is in
/// upper case (as the command parser leaves it).
fn raw(cmd: &Command, kw: &str) -> String {
    cmd.params
        .get(kw)
        .and_then(|v| v.first())
        .map(|v| match v {
            Value::Word(s) | Value::Quoted(s) => s.clone(),
        })
        .unwrap_or_default()
}

fn level(cmd: &Command) -> Level {
    if cmd.get("LEVEL").eq_ignore_ascii_case("*SYS") {
        Level::System
    } else {
        Level::Job
    }
}

/// CCSID: a number, *JOB (0 here: the job's), *HEX (65535).
fn ccsid(text: &str) -> Result<Option<i32>> {
    Ok(match text.to_ascii_uppercase().as_str() {
        "*SAME" => None,
        "*JOB" | "" => Some(0),
        "*HEX" => Some(65535),
        n => match n.parse::<i32>() {
            Ok(c) if (1..=65535).contains(&c) => Some(c),
            _ => bail!("CPF3BCA: CCSID {text} is not supported."),
        },
    })
}

/// Exit status and signal of a finished command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum End {
    Exit(i32),
    Signal(i32),
}

/// What QSH sends when the command ends: the message, its data, and
/// whether it is an escape message.
pub fn end_message(end: End, escape_msg: bool) -> (String, Vec<u8>, bool) {
    match end {
        End::Exit(n) => (
            format!("QSH0005: The command ended with exit status {n}."),
            n.to_be_bytes().to_vec(),
            escape_msg && n > 0,
        ),
        End::Signal(s) => (
            format!("QSH0006: Command ended because of signal {s}."),
            s.to_be_bytes().to_vec(),
            escape_msg,
        ),
    }
}

/// Where QIBM_QSH_CMD_OUTPUT sends the output.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Stdout,
    None,
    File(String),
    Append(String),
}

pub fn output(setting: Option<&str>) -> Output {
    let Some(s) = setting else {
        return Output::Stdout;
    };
    let up = s.to_ascii_uppercase();
    if let Some(p) = up.strip_prefix("FILEAPPEND=") {
        Output::Append(s[s.len() - p.len()..].to_string())
    } else if let Some(p) = up.strip_prefix("FILE=") {
        Output::File(s[s.len() - p.len()..].to_string())
    } else if up == "NONE" {
        Output::None
    } else {
        Output::Stdout
    }
}

/// The message data of the environment variable commands' completion
/// messages: the length of what follows (4-byte binary), then the name
/// (EBCDIC), in apostrophes for CPCA980 and CPCA988.
fn envvar_data(name: &str, quoted: bool) -> Vec<u8> {
    let shown = if quoted {
        format!("'{name}'")
    } else {
        name.to_string()
    };
    let text = crate::rpg::data::e(&shown);
    let mut d = (text.len() as i32).to_be_bytes().to_vec();
    d.extend(text);
    d
}

impl Job<'_> {
    pub(crate) async fn qsh_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "ADDENVVAR" => {
                let name = raw(cmd, "ENVVAR");
                let value = match raw(cmd, "VALUE").as_str() {
                    "*NULL" => None,
                    v => Some(v.to_string()),
                };
                let ccsid = ccsid(cmd.get("CCSID"))?.unwrap_or(0);
                self.check_sys_level(cmd).await?;
                envvar::add(
                    self.client,
                    &self.job_key(),
                    level(cmd),
                    &name,
                    Var { value, ccsid },
                    cmd.get("REPLACE").eq_ignore_ascii_case("*YES"),
                )
                .await?;
                // CPCA980, also when REPLACE(*YES) replaced one; its data
                // the name's length (4-byte binary) then the name in
                // apostrophes (PUB400 2026-10-26, tools/golden/qsh.txt
                // q_out addcomp, replcomp: 00000008 'GLDVAR').
                self.calls.message_data = Some(envvar_data(&name, true));
                Outcome::Messages(vec![format!(
                    "CPCA980: Environment variable {name} was added."
                )])
            }
            "CHGENVVAR" => {
                let name = raw(cmd, "ENVVAR");
                let value = match raw(cmd, "VALUE").as_str() {
                    "*SAME" => None,
                    "*NULL" => Some(None),
                    v => Some(Some(v.to_string())),
                };
                self.check_sys_level(cmd).await?;
                envvar::change(
                    self.client,
                    &self.job_key(),
                    level(cmd),
                    &name,
                    value,
                    ccsid(cmd.get("CCSID"))?,
                )
                .await?;
                // CPCA981, its data the name without apostrophes (PUB400
                // 2026-10-26, tools/golden/qsh.txt q_out chgcomp:
                // 00000006 GLDVAR).
                self.calls.message_data = Some(envvar_data(&name, false));
                Outcome::Messages(vec![format!(
                    "CPCA981: Environment variable {name} was changed."
                )])
            }
            "RMVENVVAR" => {
                let name = raw(cmd, "ENVVAR");
                self.check_sys_level(cmd).await?;
                envvar::remove(self.client, &self.job_key(), level(cmd), &name).await?;
                // CPCA988, data as CPCA980's (PUB400 2026-10-26,
                // tools/golden/qsh.txt q_out rmvcomp).
                self.calls.message_data = Some(envvar_data(&name, true));
                Outcome::Messages(vec![format!(
                    "CPCA988: Environment variable {name} was removed."
                )])
            }
            "WRKENVVAR" => {
                let vars = match level(cmd) {
                    Level::Job => envvar::job_vars(self.client, &self.job_key()).await?,
                    Level::System => envvar::system_vars(self.client).await?,
                };
                let rows: Vec<String> = vars
                    .iter()
                    .map(|(k, v)| format!("{:<30} {}", k, v.value.as_deref().unwrap_or("*NULL")))
                    .collect();
                Outcome::Display(ListDisplay {
                    title: "Work with Environment Variables".into(),
                    heading: format!("{:<30} {}", "Variable", "Value"),
                    print: rows.clone(),
                    rows,
                    ..Default::default()
                })
            }
            "QSH" | "STRQSH" => self.qsh(cmd).await?,
            "STRREXPRC" => {
                let parm = match cmd.get("PARM") {
                    p if p.eq_ignore_ascii_case("*NONE") => "",
                    p => p,
                };
                let (file, mbr, env) = (
                    cmd.get("SRCFILE").to_string(),
                    cmd.get("SRCMBR").to_string(),
                    cmd.get("CMDENV").to_string(),
                );
                let msgs = Box::pin(crate::rexx::strrexprc(self, &file, &mbr, parm, &env)).await?;
                Outcome::Messages(msgs)
            }
            _ => return Ok(None),
        }))
    }

    /// LEVEL(*SYS) needs *JOBCTL special authority.
    async fn check_sys_level(&self, cmd: &Command) -> Result<()> {
        if level(cmd) == Level::System
            && !crate::users::has_special(self.client, &self.user, "*JOBCTL").await?
            && !crate::users::has_special(self.client, &self.user, "*ALLOBJ").await?
        {
            bail!("CPFA98E: Not authorized to change system-level environment variables.");
        }
        Ok(())
    }

    async fn qsh(&mut self, cmd: &Command) -> Result<Outcome> {
        let line = raw(cmd, "CMD");
        if line.is_empty() || line.eq_ignore_ascii_case("*NONE") {
            // No interactive Qshell session here; in a batch job IBM's
            // does nothing either.
            return Ok(Outcome::Messages(vec![]));
        }
        if !enabled() {
            bail!(
                "CPF9899: Qshell is not set up on this system (AS4TAINR_QSH): the command was not run."
            );
        }
        let job = self.job_key();
        let vars = envvar::for_process(self.client, &job).await?;
        let setting = |n: &str| vars.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        let out = output(setting("QIBM_QSH_CMD_OUTPUT").as_deref());
        let escape_msg = setting("QIBM_QSH_CMD_ESCAPE_MSG").is_some_and(|v| v == "Y");
        // The IFS at its paths, when this system can (qshfs.rs); the
        // command starts in the user's home directory if there is one.
        let mirror = if crate::qshfs::available() {
            Some(crate::qshfs::Mirror::open(self.client, &self.env.database_url).await?)
        } else {
            None
        };
        let home = format!("/home/{}", self.user);
        let home = match crate::ifs::stat(self.client, &home).await {
            Ok(Some(e)) if e.dir => e.path,
            _ => "/".to_string(),
        };
        // The job's current directory (Qshell starts in it).
        let start = match &mirror {
            Some(_) => crate::ifs::current_dir(),
            None => std::env::temp_dir().to_string_lossy().to_string(),
        };
        let mut p = match mirror.as_ref().and_then(|m| m.command(&start, &line)) {
            Some((prog, args)) => {
                let mut c = tokio::process::Command::new(prog);
                c.args(args);
                c
            }
            None => {
                let mut c = tokio::process::Command::new("/bin/sh");
                c.arg("-c")
                    .arg(format!("exec 2>&1\n{line}"))
                    .current_dir(std::env::temp_dir());
                c
            }
        };
        p.env_clear()
            .envs(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped());
        if setting("PATH").is_none() {
            p.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        }
        if setting("HOME").is_none() {
            p.env(
                "HOME",
                if mirror.is_some() {
                    home.as_str()
                } else {
                    start.as_str()
                },
            );
        }
        p.env("LOGNAME", &self.user);
        let r = p.output().await;
        // What the command changed goes back to the IFS.
        if let Some(m) = mirror {
            m.close(self.client, &self.user).await?;
        }
        let r = r?;
        let text = String::from_utf8_lossy(&r.stdout).to_string();
        match &out {
            Output::Stdout => self
                .calls
                .terminal
                .extend(text.lines().map(|l| l.to_string())),
            Output::None => {}
            Output::File(path) | Output::Append(path) => {
                let old = if matches!(out, Output::Append(_)) {
                    crate::ifs::read_file(self.client, path).await.ok()
                } else {
                    None
                };
                let (bytes, ccsid) = match old {
                    Some((b, c)) if crate::ifs::codec::supported(c) => {
                        let mut all = crate::ifs::codec::text(c, &b);
                        all.push_str(&text);
                        let mut v = Vec::new();
                        for ch in all.chars() {
                            crate::ifs::codec::encode_char(c, ch, &mut v);
                        }
                        (v, c)
                    }
                    // A new file: in the job's CCSID (PUB400 2026-10-23,
                    // tools/golden/qsh.txt q_file: CCSID 273, the job's
                    // there; as4tainr's jobs are CCSID 37).
                    _ => {
                        let mut v = Vec::new();
                        for ch in text.chars() {
                            crate::ifs::codec::encode_char(37, ch, &mut v);
                        }
                        (v, 37)
                    }
                };
                crate::ifs::write_file(self.client, path, &bytes, ccsid, 0o644, &self.user)
                    .await
                    .map_err(|e| anyhow::anyhow!("CPF9899: Qshell output file {path}: {e}"))?;
            }
        }
        use std::os::unix::process::ExitStatusExt;
        let end = match (r.status.code(), r.status.signal()) {
            (Some(c), _) => End::Exit(c),
            (None, Some(s)) => End::Signal(s),
            _ => End::Exit(0),
        };
        let (msg, data, escape) = end_message(end, escape_msg);
        let id = &msg[..7];
        if escape {
            let mut m = crate::pgmmsg::from_error(&anyhow::anyhow!("{msg}"), id);
            m.data = data;
            self.calls.escape = Some(m);
            bail!("{msg}");
        }
        self.calls.message_data = Some(data);
        Ok(Outcome::Messages(vec![msg]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Qshell, "Running Qshell commands from CL": QIBM_QSH_CMD_OUTPUT.
    #[test]
    fn output_setting() {
        assert_eq!(output(None), Output::Stdout);
        assert_eq!(output(Some("STDOUT")), Output::Stdout);
        assert_eq!(output(Some("NONE")), Output::None);
        assert_eq!(
            output(Some("FILE=/tmp/Out.txt")),
            Output::File("/tmp/Out.txt".into())
        );
        assert_eq!(
            output(Some("FILEAPPEND=/tmp/a")),
            Output::Append("/tmp/a".into())
        );
    }

    // QSH0005 with the exit status (escape only above 0 with
    // QIBM_QSH_CMD_ESCAPE_MSG=Y), QSH0006 with the signal number.
    #[test]
    fn end_messages() {
        let (m, d, e) = end_message(End::Exit(0), true);
        assert!(m.starts_with("QSH0005") && d == [0, 0, 0, 0] && !e);
        let (_, d, e) = end_message(End::Exit(2), true);
        assert!(d == [0, 0, 0, 2] && e);
        let (_, _, e) = end_message(End::Exit(2), false);
        assert!(!e);
        let (m, d, e) = end_message(End::Signal(9), true);
        assert!(m.starts_with("QSH0006") && d == [0, 0, 0, 9] && e);
    }
}
