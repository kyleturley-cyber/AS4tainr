//! The source editor's display (STRSEU, PDM option 2): as4tainr's own
//! layout. Line 1 the title, line 2 the member, line 3 a command line
//! (TOP, BOTTOM, FIND text, SAVE), line 4 a column ruler, lines 5-21 the
//! member's lines: the sequence number (type line commands over it) and
//! 71 columns of text (F19/F20 move the window over longer records).
//! Roll keys page; F3 ends, asking whether to save a changed member.
//! The editing itself is `crate::seu`.

use anyhow::Result;

use super::super::ds::{attr, Aid, FieldKind, Screen};
use super::{centered, keys, Term};
use crate::commands::{EditTarget, Job};
use crate::seu::{Editor, Typed};

/// Lines of the member on the display.
const PAGE: usize = 17;
/// Columns of text shown.
const COLS: usize = 71;
/// The longest line EDTF keeps (a stream file's lines have no record
/// length; longer ones are cut).
const STREAM_WIDTH: usize = 32767;

/// What is being edited, as loaded: a member, or a stream file (EDTF).
enum Source {
    Member {
        lib: String,
        member: crate::source::Member,
    },
    Stream {
        path: String,
        /// The file's CCSID (a new file: 1208) and whether its lines end
        /// with CR LF (kept when it is written back).
        ccsid: i32,
        crlf: bool,
        exists: bool,
    },
}

/// Edit or browse a member, or edit a stream file; the message to show
/// afterwards.
pub(super) async fn edit(term: &mut Term, job: &mut Job<'_>, t: &EditTarget) -> Result<String> {
    let ((y, mo, d), _, _) = crate::rpg::dates::now();
    let today = (y.rem_euclid(100) as u32) * 10000 + mo * 100 + d;
    let (src, mut ed) = match &t.stmf {
        Some(path) => {
            let (text, ccsid, crlf, exists) = match crate::ifs::read_file(job.client, path).await {
                Ok((bytes, ccsid)) => {
                    let text = crate::ifs::codec::text(ccsid, &bytes);
                    let crlf = text.contains("\r\n");
                    (text, ccsid, crlf, true)
                }
                Err(e) if crate::ifs::errno_of(&e) == crate::ifs::errno::ENOENT => {
                    (String::new(), 1208, false, false)
                }
                Err(e) => return Err(e),
            };
            let mut ed = Editor::new(&[], STREAM_WIDTH, today);
            let body = text.strip_suffix('\n').unwrap_or(&text);
            if !body.is_empty() {
                ed.lines = body
                    .split('\n')
                    .enumerate()
                    .map(|(i, l)| crate::seu::Line {
                        seq: (i as u32 + 1) * 100,
                        date: 0,
                        text: l.trim_end_matches('\r').to_string(),
                        fresh: false,
                    })
                    .collect();
            }
            (
                Source::Stream {
                    path: path.clone(),
                    ccsid,
                    crlf,
                    exists,
                },
                ed,
            )
        }
        None => {
            let lib = crate::qtemp::real(&t.library);
            let m = crate::source::find_member(
                job.client,
                std::slice::from_ref(&lib),
                &t.file,
                &t.member,
            )
            .await?;
            let seqdat = crate::source::member_seqdat(job.client, &lib, &t.file, &t.member).await?;
            let records = crate::source::line_records(&m.source, seqdat.as_deref());
            let width = crate::source::record_length(job.client, &lib, &t.file)
                .await
                .unwrap_or(92)
                .saturating_sub(12)
                .max(1) as usize;
            let ed = Editor::new(&records, width, today);
            (Source::Member { lib, member: m }, ed)
        }
    };
    let width = ed.width;
    if ed.lines.is_empty() && !t.browse {
        // A new or empty member or file: lines ready to type on.
        ed.lines = (0..PAGE)
            .map(|i| crate::seu::Line {
                seq: (i as u32 + 1) * 100,
                date: today,
                text: String::new(),
                fresh: true,
            })
            .collect();
    }
    // `top` counts the "Beginning of data" line as 0.
    let mut top = 0usize;
    let mut left = 0usize;
    let mut message = String::new();
    let mut command = String::new();
    loop {
        let mut s = Screen::new();
        let mode = if t.browse { "Browse" } else { "Edit" };
        let changed = if ed.changed { "  (changed)" } else { "" };
        match &src {
            Source::Member { member: m, .. } => {
                centered(&mut s, 1, &format!("Source Entry: {mode}"));
                s.put(
                    2,
                    2,
                    &format!(
                        "{}/{}  {}  {}{changed}",
                        t.library, t.file, t.member, m.srctype
                    ),
                );
            }
            Source::Stream { path, exists, .. } => {
                centered(
                    &mut s,
                    1,
                    if t.browse {
                        "Display File"
                    } else {
                        "Edit File"
                    },
                );
                let new = if *exists { "" } else { "  (new file)" };
                let shown: String = path.chars().take(58).collect();
                s.put(2, 2, &format!("{shown}{new}{changed}"));
            }
        }
        s.put(2, 62, &format!("Columns {}-{}", left + 1, left + COLS));
        s.text(3, 2, attr::HIGH, "Cmd ===>");
        s.field("CMD", 3, 11, 69, FieldKind::Text, &command);
        command.clear();
        s.put(4, 2, &format!("{:<8}{}", "", ruler(left)));
        // The virtual list: 0 = beginning, 1..=n lines, n+1 = end.
        let n = ed.lines.len();
        for row in 0..PAGE {
            let v = top + row;
            let r = 5 + row as u8;
            if v == 0 {
                s.text(
                    r,
                    2,
                    attr::BLUE,
                    &format!("{:*<36} Beginning of data {:*<23}", "", ""),
                );
            } else if v == n + 1 {
                s.text(
                    r,
                    2,
                    attr::BLUE,
                    &format!("{:*<36} End of data {:*<29}", "", ""),
                );
                break;
            } else if v <= n {
                let i = v - 1;
                let l = &ed.lines[i];
                let shown: String = l.text.chars().skip(left).take(COLS).collect();
                if t.browse {
                    s.put(r, 2, &Editor::seq_text(l.seq));
                    s.put(r, 10, &shown);
                } else {
                    s.field(
                        &format!("S{i}"),
                        r,
                        2,
                        7,
                        FieldKind::Upper,
                        &Editor::seq_text(l.seq),
                    );
                    s.field(
                        &format!("D{i}"),
                        r,
                        10,
                        COLS as u16,
                        FieldKind::Text,
                        &shown,
                    );
                }
            }
        }
        keys(
            &mut s,
            23,
            "F3=Exit  F4=Prompt  F5=Refresh  F19=Left  F20=Right  Roll=Page",
        );
        s.message(&message);
        message.clear();
        if !t.browse {
            s.cursor_at(3, 11);
        }
        let inp = term.show(&s).await?;
        // What was typed on the lines on the display.
        let mut typed = Typed::default();
        for name in &inp.modified {
            if let Some(i) = name.strip_prefix('S').and_then(|x| x.parse::<usize>().ok()) {
                if let Some(l) = ed.lines.get(i) {
                    let c = line_command(inp.get(name), &Editor::seq_text(l.seq));
                    if !c.is_empty() {
                        typed.commands.push((i, c));
                    }
                }
            } else if let Some(i) = name.strip_prefix('D').and_then(|x| x.parse::<usize>().ok()) {
                if let Some(l) = ed.lines.get(i) {
                    let typed_text = inp.get(name).trim_end_matches(['\0', ' ']);
                    let head: String = l.text.chars().take(left).collect();
                    let tail: String = l.text.chars().skip(left + COLS).collect();
                    let mut text = format!("{head:<left$}{typed_text}");
                    if !tail.is_empty() {
                        text = format!("{text:<w$}{tail}", w = left + COLS);
                    }
                    typed.texts.push((i, text));
                }
            }
        }
        if !t.browse {
            let changed: Vec<usize> = typed.texts.iter().map(|(i, _)| *i).collect();
            if let Err(e) = ed.apply(&typed) {
                message = e;
            } else if let Source::Member { member: m, .. } = &src {
                if let Some(m2) = syntax_check(job, &m.srctype, &ed, &changed).await? {
                    message = m2;
                }
            }
        }
        let n = ed.lines.len();
        // The highest first line: the end of data on the last line.
        let max_top = (n + 2).saturating_sub(PAGE);
        match inp.aid {
            Aid::F(3) | Aid::F(12) => {
                if t.browse || !ed.changed {
                    return Ok(String::new());
                }
                match exit_choice(term).await? {
                    Some(true) => {
                        save(job, &src, t, &ed).await?;
                        return Ok(match &src {
                            Source::Stream { path, .. } => format!("Stream file {path} was saved."),
                            Source::Member { .. } => format!(
                                "Member {} in file {}/{} was changed.",
                                t.member, t.library, t.file
                            ),
                        });
                    }
                    Some(false) => {
                        return Ok(match &src {
                            Source::Stream { path, .. } => {
                                format!("Stream file {path} was not changed.")
                            }
                            Source::Member { .. } => {
                                format!("Member {} was not changed.", t.member)
                            }
                        })
                    }
                    None => continue,
                }
            }
            Aid::RollUp => top = (top + PAGE - 1).min(max_top),
            Aid::RollDown => top = top.saturating_sub(PAGE - 1),
            Aid::F(19) => left = left.saturating_sub(40),
            Aid::F(20) => {
                if left + COLS < width {
                    left = (left + 40).min(width.saturating_sub(COLS));
                }
            }
            Aid::F(4) if !t.browse => {
                // F4 on a CL statement: the command prompted, and the
                // statement written back from what the prompt gives (SEU
                // User's Guide: prompting a source statement).
                let cl = matches!(&src, Source::Member { member: m, .. }
                    if matches!(m.srctype.to_ascii_uppercase().as_str(), "CLP" | "CLLE" | "CL"));
                let row = inp.cursor.0 as usize;
                let v = (5..5 + PAGE).contains(&row).then(|| top + row - 5);
                match v {
                    _ if !cl => message = "F4 prompts statements of CL members here.".into(),
                    Some(v) if v >= 1 && v <= n => {
                        let (first, last) = crate::seu::cl_statement(&ed.lines, v - 1);
                        let joined = crate::seu::cl_joined(&ed.lines[first..=last]);
                        // The label and indentation stay as they were.
                        let body = joined.trim_start();
                        let (label, cmd) = match body.split_once(':') {
                            Some((l, c)) if !l.contains(' ') && !l.contains('(') => {
                                (format!("{l}:"), c.trim_start())
                            }
                            _ => (String::new(), body),
                        };
                        let lead = &joined[..joined.len() - body.len()];
                        let prefix = if label.is_empty() {
                            lead.to_string()
                        } else {
                            format!("{lead}{label} ")
                        };
                        match crate::cl::parse_partial(cmd) {
                            Err(e) => message = crate::jobs::display_text(&e.to_string()),
                            Ok((def, vals)) => {
                                if let super::PromptResult::Run(line) =
                                    super::prompt(term, def, &vals, "").await?
                                {
                                    let lines = crate::seu::cl_wrapped(&prefix, &line, ed.width);
                                    ed.replace_lines(first, last, &lines);
                                }
                            }
                        }
                    }
                    _ => message = "Put the cursor on a statement to prompt it.".into(),
                }
            }
            Aid::F(5) | Aid::Enter => {}
            _ => message = "That key is not valid on this display.".into(),
        }
        // The command line.
        let cmd = inp.get("CMD").trim().to_string();
        let upper = cmd.to_ascii_uppercase();
        if upper.is_empty() {
            continue;
        }
        if upper == "TOP" || upper == "T" {
            top = 0;
        } else if upper == "BOTTOM" || upper == "BOT" || upper == "B" {
            top = max_top;
        } else if upper == "SAVE" {
            if t.browse {
                message = "A member being browsed cannot be saved.".into();
            } else {
                save(job, &src, t, &ed).await?;
                ed.changed = false;
                message = match &src {
                    Source::Stream { path, .. } => format!("Stream file {path} saved."),
                    Source::Member { .. } => format!("Member {} saved.", t.member),
                };
            }
        } else if let Some(what) = upper
            .strip_prefix("FIND ")
            .or_else(|| upper.strip_prefix("F "))
        {
            // Search from the line after the first one shown (`top` is
            // the line number of the first one shown, 1-based).
            let what = what.trim().trim_matches('\'');
            match ed.find(what, if n == 0 { 0 } else { top % n }) {
                Some(i) => {
                    top = i + 1;
                    message = format!("'{what}' found.");
                }
                None => message = format!("'{what}' was not found."),
            }
        } else {
            message = format!("{cmd} is not a command here: use TOP, BOTTOM, FIND text or SAVE.");
            command = cmd;
        }
    }
}

/// SEU's syntax checking of a CL member (SEU User's Guide: the lines
/// changed are checked when Enter is pressed and an error is shown
/// with the line): the statements that hold a changed line, checked as
/// the CL compiler checks a statement (a command that is not there, a
/// value not valid, a statement that cannot be parsed). Checks that need
/// the whole program (variables declared, labels) wait for the compile.
/// The first error, with its line's sequence number; None when there is
/// none or the member is not CL.
async fn syntax_check(
    job: &mut Job<'_>,
    srctype: &str,
    ed: &Editor,
    changed: &[usize],
) -> Result<Option<String>> {
    if changed.is_empty() {
        return Ok(None);
    }
    if !matches!(srctype.to_ascii_uppercase().as_str(), "CLP" | "CLLE" | "CL") {
        let source: Vec<&str> = ed.lines.iter().map(|l| l.text.as_str()).collect();
        let lines: Vec<usize> = changed.iter().map(|i| i + 1).collect();
        return Ok(dds_check(srctype, &source, &lines).map(|(line, text)| {
            let seq = ed
                .lines
                .get(line.saturating_sub(1))
                .map(|l| Editor::seq_text(l.seq))
                .unwrap_or_default();
            format!("Line {seq}: {text}")
        }));
    }
    // The statements the changed lines are in: a line continues the one
    // before when that one ends in + or -.
    let mut starts = std::collections::BTreeSet::new();
    for &i in changed {
        let mut s = i.min(ed.lines.len().saturating_sub(1));
        while s > 0 {
            let prev = ed.lines[s - 1].text.trim_end();
            if prev.ends_with('+') || prev.ends_with('-') {
                s -= 1;
            } else {
                break;
            }
        }
        starts.insert(s + 1);
    }
    let source: String = ed
        .lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let user_cmds: Vec<String> = job
        .client
        .query(
            "SELECT name FROM as4tainr.objects WHERE objtype='*CMD'",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let known = |c: &str| {
        let bare = c.rsplit('/').next().unwrap_or(c);
        crate::cl::find(bare).is_some()
            || crate::ibmcmds::is_ibm(bare)
            || user_cmds.iter().any(|u| u == bare)
    };
    let a = crate::cllist::analyze(&source, &known, &|_| Vec::new());
    // CPD0727, CPD0726, CPD0791, CPD0792: variables and labels, which
    // the compile checks over the whole program.
    let found = a.diags.iter().find(|d| {
        starts.contains(&d.line)
            && d.sev >= 30
            && !matches!(d.id.as_str(), "CPD0727" | "CPD0726" | "CPD0791" | "CPD0792")
    });
    Ok(found.map(|d| {
        let seq = ed
            .lines
            .get(d.line.saturating_sub(1))
            .map(|l| Editor::seq_text(l.seq))
            .unwrap_or_default();
        format!("Line {seq}: {}", crate::jobs::message_text(&d.text).trim())
    }))
}

/// SEU's syntax checking of a DDS or ILE RPG member (PF, LF, DSPF, PRTF,
/// RPGLE, SQLRPGLE; SEU
/// User's Guide: DDS source is checked as it is typed): the member parsed
/// as its compiler parses it, the first error on one of the lines typed
/// (1-based) with its message (an ID where the DDS listing gives one,
/// CPD7419 for a data type that is not valid: PUB400, verify5
/// z_crtpf_bad). A physical file with reference fields is left to the
/// compile (its fields come from other files).
pub fn dds_check(srctype: &str, source: &[&str], lines: &[usize]) -> Option<(usize, String)> {
    let text: String = source
        .iter()
        .map(|l| l.chars().take(80).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let r: anyhow::Result<()> = match srctype.to_ascii_uppercase().as_str() {
        // ILE RPG: the member parsed as the compiler parses it, /COPY
        // members taken as empty (their lines are not this member's);
        // names are not checked (a program being typed uses names it
        // declares further down), as SEU's own check is of the syntax.
        "RPGLE" | "SQLRPGLE" => {
            let src = source.join("\n") + "\n";
            crate::rpg::parse::parse(&src, &|_| Some(String::new())).map(|_| ())
        }
        "PF" => {
            let refs = source.iter().any(|l| {
                let u = l.to_ascii_uppercase();
                u.contains("REF(") || u.contains("REFFLD(") || l.chars().nth(28) == Some('R')
            });
            if refs {
                return None;
            }
            crate::dds::parse_pf("SEUCHECK", &text).map(|_| ())
        }
        "LF" => crate::dds::parse_lf("SEUCHECK", &text).map(|_| ()),
        "DSPF" => crate::dspf::parse("SEUCHECK", &text).map(|_| ()),
        "PRTF" => crate::prtf::parse("SEUCHECK", &text).map(|_| ()),
        _ => return None,
    };
    let e = r.err()?;
    let m = crate::ddslist::message_of(&e);
    if !lines.contains(&m.line) {
        return None;
    }
    let t = crate::jobs::message_text(&m.text).trim().to_string();
    Some((
        m.line,
        match m.id {
            Some(id) => format!("{id}: {t}"),
            None => t,
        },
    ))
}

/// A column ruler from column `left` + 1.
fn ruler(left: usize) -> String {
    (left + 1..=left + COLS)
        .map(|c| {
            if c % 10 == 0 {
                char::from_digit(((c / 10) % 10) as u32, 10).unwrap_or('+')
            } else if c % 5 == 0 {
                '+'
            } else {
                '.'
            }
        })
        .collect()
}

/// The line command typed over a sequence number: what is left once the
/// part of the number still showing at the end is taken off ("D" typed
/// over "0012.00" comes back as "D012.00"; with Field Exit, as "D").
fn line_command(value: &str, shown: &str) -> String {
    let v: Vec<char> = value.trim_end_matches(['\0', ' ']).chars().collect();
    let o: Vec<char> = shown.chars().collect();
    let mut k = 0;
    while k < v.len() && k < o.len() && v[v.len() - 1 - k] == o[o.len() - 1 - k] {
        k += 1;
    }
    if k == v.len() {
        // Nothing typed (the number as it was).
        return String::new();
    }
    let cmd: String = v[..v.len() - k].iter().collect();
    cmd.trim().to_string()
}

/// F3 on a changed member: save it (Y) or not (N), F12 back to editing.
async fn exit_choice(term: &mut Term) -> Result<Option<bool>> {
    let mut message = String::new();
    loop {
        let mut s = Screen::new();
        centered(&mut s, 1, "Exit");
        s.put(3, 2, "Type choice, press Enter.");
        s.put(5, 4, "Save the changed member . . .");
        s.field("SAVE", 5, 35, 1, FieldKind::Upper, "Y");
        s.put(5, 40, "Y=Yes, N=No");
        keys(&mut s, 23, "F12=Cancel");
        s.message(&message);
        let inp = term.show(&s).await?;
        match inp.aid {
            Aid::F(12) => return Ok(None),
            Aid::Enter => match inp.get("SAVE").trim().to_ascii_uppercase().as_str() {
                "Y" | "" => return Ok(Some(true)),
                "N" => return Ok(Some(false)),
                _ => message = "Type Y or N.".into(),
            },
            _ => message = "That key is not valid on this display.".into(),
        }
    }
}

/// Write the member back with its sequence numbers and dates, or the
/// stream file in its CCSID with its line ends.
async fn save(job: &mut Job<'_>, src: &Source, t: &EditTarget, ed: &Editor) -> Result<()> {
    match src {
        Source::Stream {
            path, ccsid, crlf, ..
        } => {
            let text = stream_text(ed, *crlf);
            let bytes = crate::ifs::codec::convert(1208, *ccsid, text.as_bytes());
            crate::ifs::write_file(job.client, path, &bytes, *ccsid, 0o644, &job.user).await
        }
        Source::Member { lib, member: m } => {
            let (source, seqdat) = ed.source();
            crate::source::put_member(
                job.client,
                &crate::source::Member {
                    library: lib.to_string(),
                    file: t.file.clone(),
                    name: t.member.clone(),
                    srctype: m.srctype.clone(),
                    text: m.text.clone(),
                    source,
                    ..Default::default()
                },
                true,
            )
            .await?;
            crate::source::set_member_seqdat(job.client, lib, &t.file, &t.member, &seqdat).await?;
            Ok(())
        }
    }
}

/// A stream file's text from the editor's lines: each line (trailing
/// blanks taken off) ended by LF, or CR LF when the file had them.
fn stream_text(ed: &Editor, crlf: bool) -> String {
    let end = if crlf { "\r\n" } else { "\n" };
    let mut out = String::new();
    for l in ed.lines.iter().filter(|l| !l.fresh) {
        out.push_str(l.text.trim_end());
        out.push_str(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A data type that is not valid is CPD7419 (PUB400, verify5
    // z_crtpf_bad), shown when its line was typed; a line not typed is
    // left to the compile, and a PF with reference fields is not checked.
    #[test]
    fn dds_members_checked_as_typed() {
        let src = [
            "     A          R REC",
            "     A            CODE           5P 0",
            "     A            NAME          20X",
        ];
        let (line, text) = dds_check("PF", &src, &[3]).unwrap();
        assert_eq!(line, 3);
        assert!(text.starts_with("CPD7419"), "{text}");
        assert!(dds_check("PF", &src, &[2]).is_none());
        let good = [
            "     A          R REC",
            "     A            CODE           5P 0",
        ];
        assert!(dds_check("PF", &good, &[2]).is_none());
        let refs = [
            "     A                                      REF(OTHER)",
            "     A          R REC",
            "     A            NAME          20X",
        ];
        assert!(dds_check("PF", &refs, &[3]).is_none());
        assert!(dds_check("TXT", &src, &[3]).is_none());
        // ILE RPG: a syntax error on a line typed is shown with its line
        // (the compiler's parse error); a name not defined is left to the
        // compile.
        let rpg = [
            "     C                   EVAL      X = 1 +",
            "     C                   EVAL      Y = UNDEFINED",
        ];
        let (line, _) = dds_check("RPGLE", &rpg, &[1]).unwrap();
        assert_eq!(line, 1);
        assert!(dds_check("RPGLE", &rpg, &[2]).is_none());
    }

    #[test]
    fn line_commands_typed_over_numbers() {
        assert_eq!(line_command("D012.00", "0012.00"), "D");
        assert_eq!(line_command("I3", "0012.00"), "I3");
        assert_eq!(line_command("CC12.00", "0012.00"), "CC");
        assert_eq!(line_command("0012.00", "0012.00"), "");
        assert_eq!(line_command("a", "0001.00"), "a");
    }

    #[test]
    fn ruler_marks_tens() {
        let r = ruler(0);
        assert_eq!(&r[..10], "....+....1");
        assert_eq!(r.len(), COLS);
        assert_eq!(&ruler(40)[..10], "....+....5");
    }

    #[test]
    fn stream_file_lines() {
        let mut ed = Editor::new(&[], STREAM_WIDTH, 0);
        for (i, t) in ["one  ", "", "three"].iter().enumerate() {
            ed.lines.push(crate::seu::Line {
                seq: (i as u32 + 1) * 100,
                date: 0,
                text: t.to_string(),
                fresh: false,
            });
        }
        assert_eq!(stream_text(&ed, false), "one\n\nthree\n");
        assert_eq!(stream_text(&ed, true), "one\r\n\r\nthree\r\n");
    }
}
