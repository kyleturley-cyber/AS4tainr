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
                centered(&mut s, 1, "Edit File");
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
            "F3=Exit  F5=Refresh  F19=Left  F20=Right  Roll=Page",
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
            if let Err(e) = ed.apply(&typed) {
                message = e;
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
