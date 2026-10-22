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
//! - `-n`: messages without their IDs; `-q`: no messages; `-v`: the
//!   command first, on standard output; `-s`: printed output not shown;
//!   `-h`: how to use it, on standard output;
//! - no command: how to use it on standard error, exit status 2.
//!
//! The texts (usage, the -v line, messages) are as4tainr's own.

use crate::commands::{Job, Outcome};

pub const USAGE: &str = "usage: as4tainr system [-beEhiIkKnOpqsv] CL-command [CL-parameters ...]";

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
    quiet: bool,
    help: bool,
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
                'q' => opts.quiet = true,
                'h' => opts.help = true,
                // Accepted, no effect here: CCSID conversion, job log,
                // spooled file and process options (b e E i I k K O p).
                'b' | 'e' | 'E' | 'i' | 'I' | 'k' | 'K' | 'O' | 'p' => {}
                _ => return usage(),
            }
        }
        rest = &rest[1..];
    }
    let command = rest.join(" ");
    let mut out = Output::default();
    if opts.help {
        out.stdout.push(USAGE.into());
        if command.trim().is_empty() {
            return out;
        }
    }
    if command.trim().is_empty() {
        return usage();
    }
    if opts.verbose {
        out.stdout.push(format!("Command: {command}"));
    }
    let msg = |m: &str| -> String {
        if opts.no_ids {
            strip_id(m).to_string()
        } else {
            m.to_string()
        }
    };
    job.calls.printed.clear();
    let result = job.run(&command).await;
    // A compiler listing: after the completion message, or before the
    // failure's messages (observed on IBM i 7.5).
    let printed = std::mem::take(&mut job.calls.printed);
    let printed = if opts.no_spool { vec![] } else { printed };
    if result.is_err() {
        out.stdout.extend(printed.iter().cloned());
    }
    match result {
        Ok(Outcome::Messages(m)) if !opts.quiet => {
            out.stdout.extend(m.iter().map(|l| msg(l)));
            out.stdout.extend(printed);
        }
        Ok(Outcome::Messages(_)) => out.stdout.extend(printed),
        Ok(Outcome::Display(d)) => {
            if let Some(m) = d.print_msg.as_ref().filter(|_| !opts.quiet) {
                out.stdout.push(msg(m));
            }
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
            if !opts.quiet {
                out.stderr
                    .extend(failure_messages(&format!("{e:#}")).iter().map(|l| msg(l)));
            }
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

/// The messages of a failed command: its diagnostics, one per line, then
/// the escape message. A command analyzer diagnostic (CPDxxxx) that is
/// not followed by an escape message gets CPF0006.
fn failure_messages(err: &str) -> Vec<String> {
    let mut lines: Vec<String> = err
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    if lines
        .last()
        .is_some_and(|l| crate::jobs::message_id(l).starts_with("CPD"))
    {
        lines.push(crate::cl::CPF0006.into());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values: PUB400 (IBM i 7.5) `system` output, 2026-10-21:
    // the analyzer's diagnostic, then CPF0006; an escape message alone.
    #[test]
    fn diagnostics_come_before_cpf0006() {
        let ids = |v: Vec<String>| -> Vec<String> {
            v.iter()
                .map(|l| crate::jobs::message_id(l).to_string())
                .collect()
        };
        assert_eq!(
            ids(failure_messages("CPD0030: Command X was not found.")),
            ["CPD0030", "CPF0006"]
        );
        assert_eq!(
            ids(failure_messages(
                "CPD0071: Parameter LIB needs a value.\nCPF0006: Not run."
            )),
            ["CPD0071", "CPF0006"]
        );
        assert_eq!(
            ids(failure_messages("CPF9801: Object X was not found.")),
            ["CPF9801"]
        );
    }

    #[test]
    fn no_ids() {
        assert_eq!(
            strip_id("CPF9801: Object X was not found."),
            "Object X was not found."
        );
        assert_eq!(strip_id("just text: more"), "just text: more");
    }
}
