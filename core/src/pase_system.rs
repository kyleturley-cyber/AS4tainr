//! The PASE `system` utility: run one CL command and show its messages
//! and printed output, as `as4tainr system` does.
//!
//! Behavior observed on PUB400 (IBM i 7.5), 2026-10-21, with
//! `system [options] "command"` from an SSH shell (target/ibmi/golden):
//!
//! - completion and informational messages go to standard output as
//!   `MSGID: text`, exit status 0;
//! - a command that fails writes its diagnostic messages and then the
//!   escape message to standard error, exit status 255 (a syntax error:
//!   the diagnostic, e.g. CPD0084, then CPF0006);
//! - a display command's printed report goes to standard output;
//! - `-n`: messages without their IDs; `-v`: `Command is > command  <`
//!   first; `-s`: printed output not shown;
//! - no command: `Usage: system [-beEhiIkKnOpqsv] command` on standard
//!   error, exit status 2.

use crate::commands::{Job, Outcome};

pub const USAGE: &str = "Usage: system [-beEhiIkKnOpqsv] command";

/// What `system` writes and its exit status.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub status: u8,
}

#[derive(Debug, Default, Clone, Copy)]
struct Options {
    no_ids: bool,
    verbose: bool,
    no_spool: bool,
}

/// Run `system` with its arguments (options, then the command; words
/// after the options are joined with blanks).
pub async fn run(job: &mut Job<'_>, args: &[String]) -> Output {
    let mut opts = Options::default();
    let mut rest = args;
    while let Some(a) = rest.first() {
        let Some(flags) = a.strip_prefix('-') else {
            break;
        };
        if flags.is_empty() {
            break;
        }
        for c in flags.chars() {
            match c {
                'n' => opts.no_ids = true,
                'v' => opts.verbose = true,
                's' => opts.no_spool = true,
                // Accepted, no effect here: job log, spooled file and
                // process options (b e E h i I k K O p q).
                'b' | 'e' | 'E' | 'h' | 'i' | 'I' | 'k' | 'K' | 'O' | 'p' | 'q' => {}
                _ => return usage(),
            }
        }
        rest = &rest[1..];
    }
    let command = rest.join(" ");
    if command.trim().is_empty() {
        return usage();
    }
    let mut out = Output::default();
    if opts.verbose {
        out.stdout.push(format!("Command is > {command}  <"));
    }
    let msg = |m: &str| -> String {
        if opts.no_ids {
            strip_id(m).to_string()
        } else {
            m.to_string()
        }
    };
    match job.run(&command).await {
        Ok(Outcome::Messages(m)) => out.stdout.extend(m.iter().map(|l| msg(l))),
        Ok(Outcome::Display(d)) => {
            if !opts.no_spool {
                if d.print.is_empty() {
                    out.stdout.push(d.title.clone());
                    out.stdout
                        .extend(d.info.iter().map(|(k, v)| format!("{k}: {v}")));
                    out.stdout.push(d.heading.clone());
                    out.stdout.extend(d.rows.iter().cloned());
                } else {
                    out.stdout.extend(d.print.iter().cloned());
                }
            }
        }
        Ok(_) => {}
        Err(e) => {
            out.stderr
                .extend(failure_messages(&format!("{e:#}")).iter().map(|l| msg(l)));
            out.status = 255;
        }
    }
    out
}

fn usage() -> Output {
    Output {
        stderr: vec![USAGE.into()],
        status: 2,
        ..Default::default()
    }
}

/// A message without its ID: `CPF9801: text` → `text`.
fn strip_id(m: &str) -> &str {
    match m.split_once(": ") {
        Some((id, text)) if !id.is_empty() && crate::jobs::message_id(m) == id => text,
        _ => m,
    }
}

/// The messages of a failed command, diagnostics first. A command that
/// could not be checked ends with CPF0006 after its diagnostic.
fn failure_messages(err: &str) -> Vec<String> {
    const CPF0006: &str = "CPF0006: Errors occurred in command.";
    if let Some(detail) = err.strip_prefix(CPF0006) {
        let detail = detail.trim();
        return if detail.is_empty() {
            vec![CPF0006.into()]
        } else {
            vec![detail.into(), CPF0006.into()]
        };
    }
    if err.starts_with("CPD") {
        return vec![err.into(), CPF0006.into()];
    }
    vec![err.into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values: PUB400 (IBM i 7.5) `system` output, 2026-10-21.
    #[test]
    fn diagnostics_come_before_cpf0006() {
        assert_eq!(
            failure_messages("CPD0030: Command NOSUCHCMD in library *LIBL not found."),
            vec![
                "CPD0030: Command NOSUCHCMD in library *LIBL not found.".to_string(),
                "CPF0006: Errors occurred in command.".to_string()
            ]
        );
        assert_eq!(
            failure_messages("CPF9801: Object NOSUCH in library AS4TRAINR1 not found."),
            vec!["CPF9801: Object NOSUCH in library AS4TRAINR1 not found.".to_string()]
        );
    }

    #[test]
    fn no_ids() {
        assert_eq!(
            strip_id("CPF9801: Object X in library Y not found."),
            "Object X in library Y not found."
        );
        assert_eq!(strip_id("just text: more"), "just text: more");
    }
}
