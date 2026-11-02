//! The source editor (STRSEU, PDM options 2 and 5): a member's lines with
//! their sequence numbers and dates, and the line commands typed over the
//! sequence numbers.
//!
//! From IBM's public documentation of the Source Entry Utility (ADTS:
//! SEU User's Guide, line commands): `I`/`In` insert blank lines after a
//! line, `D`/`Dn` and a `DD` block delete, `C`/`Cn` and a `CC` block copy,
//! `M`/`Mn` and an `MM` block move, to after an `A` line or before a `B`
//! line, `R`/`Rn` and an `RR` block repeat. Inserted lines left blank go
//! when Enter is pressed again. A line changed or added gets today's date
//! (YYMMDD); new lines get sequence numbers between their neighbors. The
//! display (`tn5250/seu.rs`) is as4tainr's own.
//!
//! This module has no I/O: the session loads a member into an [`Editor`],
//! applies what the operator typed, and saves [`Editor::source`].

/// One line of the member.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// SRCSEQ in hundredths (1.00 is 100).
    pub seq: u32,
    /// SRCDAT, YYMMDD (0 for none).
    pub date: u32,
    pub text: String,
    /// Inserted by I and still blank: it goes at the next Enter.
    pub fresh: bool,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub lines: Vec<Line>,
    /// Whether the member changed since it was loaded or saved.
    pub changed: bool,
    /// The date lines changed now get (YYMMDD).
    pub today: u32,
    /// The member's record data length (SRCDTA): longer text is cut.
    pub width: usize,
}

/// What the operator typed on the display: line commands by line index
/// (the text typed over a sequence number) and changed texts by index.
#[derive(Debug, Clone, Default)]
pub struct Typed {
    pub commands: Vec<(usize, String)>,
    pub texts: Vec<(usize, String)>,
}

/// A line command, parsed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
    Insert(usize),
    Delete(usize),
    DeleteBlock,
    Copy(usize),
    CopyBlock,
    Move(usize),
    MoveBlock,
    Repeat(usize),
    RepeatBlock,
    After(usize),
    Before(usize),
}

fn parse_cmd(s: &str) -> Option<Cmd> {
    let s = s.trim().to_ascii_uppercase();
    let (letters, digits): (String, String) = s.chars().partition(|c| c.is_ascii_alphabetic());
    if !s.starts_with(letters.as_str()) {
        return None;
    }
    let n = if digits.is_empty() {
        1
    } else {
        digits.parse::<usize>().ok()?.max(1)
    };
    Some(match letters.as_str() {
        "I" => Cmd::Insert(n),
        "D" => Cmd::Delete(n),
        "DD" if digits.is_empty() => Cmd::DeleteBlock,
        "C" => Cmd::Copy(n),
        "CC" if digits.is_empty() => Cmd::CopyBlock,
        "M" => Cmd::Move(n),
        "MM" if digits.is_empty() => Cmd::MoveBlock,
        "R" => Cmd::Repeat(n),
        "RR" if digits.is_empty() => Cmd::RepeatBlock,
        "A" => Cmd::After(n),
        "B" => Cmd::Before(n),
        _ => return None,
    })
}

impl Editor {
    /// A member's lines: its source and the kept sequence numbers and
    /// dates (`source::line_records`).
    pub fn new(records: &[(String, String, String)], width: usize, today: u32) -> Editor {
        let lines = records
            .iter()
            .enumerate()
            .map(|(i, (seq, dat, text))| {
                let meta = crate::source::line_meta(seq, dat);
                let seq = meta[..6].parse().unwrap_or((i as u32 + 1) * 100);
                Line {
                    seq,
                    date: meta[6..].parse().unwrap_or(0),
                    text: text.trim_end().to_string(),
                    fresh: false,
                }
            })
            .collect();
        Editor {
            lines,
            changed: false,
            today,
            width,
        }
    }

    /// The member's source and its kept sequence numbers and dates (for
    /// `source::set_member_seqdat`).
    pub fn source(&self) -> (String, String) {
        let mut src = String::new();
        let mut meta = Vec::new();
        for l in self.lines.iter().filter(|l| !l.fresh) {
            src.push_str(l.text.trim_end());
            src.push('\n');
            meta.push(format!("{:06}{:06}", l.seq % 1_000_000, l.date % 1_000_000));
        }
        (src, meta.join("\n"))
    }

    /// A sequence number as SEU shows it: 0001.00.
    pub fn seq_text(seq: u32) -> String {
        format!("{:04}.{:02}", seq / 100, seq % 100)
    }

    /// Apply what was typed: changed texts first, then the line commands.
    /// An error leaves the member as the texts left it (the commands are
    /// not done) and says what is wrong.
    pub fn apply(&mut self, typed: &Typed) -> Result<(), String> {
        for (i, t) in &typed.texts {
            if let Some(l) = self.lines.get_mut(*i) {
                let t: String = t.trim_end().chars().take(self.width).collect();
                if t != l.text {
                    l.text = t;
                    l.date = self.today;
                    l.fresh = false;
                    self.changed = true;
                }
            }
        }
        let mut cmds: Vec<(usize, Cmd)> = Vec::new();
        for (i, c) in &typed.commands {
            if c.trim().is_empty() {
                continue;
            }
            match parse_cmd(c) {
                Some(cmd) if *i < self.lines.len() => cmds.push((*i, cmd)),
                _ => return Err(format!("Line command {} is not valid.", c.trim())),
            }
        }
        cmds.sort_by_key(|c| c.0);
        // Inserted lines still blank go at an Enter with no line commands.
        let had_commands = !cmds.is_empty();
        let result = self.run_commands(&cmds);
        if result.is_ok() && !had_commands {
            self.lines.retain(|l| !(l.fresh && l.text.is_empty()));
        }
        result
    }

    fn run_commands(&mut self, cmds: &[(usize, Cmd)]) -> Result<(), String> {
        // Blocks: pairs of DD, CC, MM, RR.
        let mut block: Option<(Cmd, usize)> = None;
        let mut deletes: Vec<std::ops::Range<usize>> = Vec::new();
        let mut source: Option<(std::ops::Range<usize>, bool)> = None; // (lines, move)
        let mut target: Option<usize> = None; // index to insert before
        let mut inserts: Vec<(usize, usize)> = Vec::new(); // (after index, count)
        let mut repeats: Vec<(std::ops::Range<usize>, usize)> = Vec::new();
        let n = self.lines.len();
        let range = |i: usize, k: usize| i..(i + k).min(n);
        for &(i, c) in cmds {
            match c {
                Cmd::DeleteBlock | Cmd::CopyBlock | Cmd::MoveBlock | Cmd::RepeatBlock => {
                    match block.take() {
                        None => block = Some((c, i)),
                        Some((open, start)) if open == c => {
                            let r = start..i + 1;
                            match c {
                                Cmd::DeleteBlock => deletes.push(r),
                                Cmd::RepeatBlock => repeats.push((r, 1)),
                                _ => {
                                    if source.is_some() {
                                        return Err(
                                            "Only one copy or move can be done at a time.".into()
                                        );
                                    }
                                    source = Some((r, c == Cmd::MoveBlock));
                                }
                            }
                        }
                        Some(_) => return Err("A block command has no matching end.".into()),
                    }
                }
                Cmd::Delete(k) => deletes.push(range(i, k)),
                Cmd::Insert(k) => inserts.push((i, k)),
                Cmd::Repeat(k) => repeats.push((i..i + 1, k)),
                Cmd::Copy(k) | Cmd::Move(k) => {
                    if source.is_some() {
                        return Err("Only one copy or move can be done at a time.".into());
                    }
                    source = Some((range(i, k), matches!(c, Cmd::Move(_))));
                }
                Cmd::After(_) | Cmd::Before(_) => {
                    if target.is_some() {
                        return Err("Only one target (A or B) can be given.".into());
                    }
                    target = Some(if matches!(c, Cmd::Before(_)) {
                        i
                    } else {
                        i + 1
                    });
                }
            }
        }
        if block.is_some() {
            return Err("A block command has no matching end.".into());
        }
        if source.is_some() != target.is_some() {
            return Err(if source.is_some() {
                "A copy or move needs a target: A (after) or B (before).".into()
            } else {
                "A target (A or B) needs a copy or move.".into()
            });
        }
        if let (Some((r, _)), Some(t)) = (&source, target) {
            if t > r.start && t < r.end {
                return Err("The target is inside the lines being copied or moved.".into());
            }
        }
        // Build the new list of lines: for each old index, what happens.
        let deleted = |i: usize| deletes.iter().any(|r| r.contains(&i));
        let moved = |i: usize| matches!(&source, Some((r, true)) if r.contains(&i));
        let copied: Vec<Line> = match &source {
            Some((r, _)) => self.lines[r.clone()].to_vec(),
            None => Vec::new(),
        };
        let mut out: Vec<(Line, bool)> = Vec::new(); // (line, new)
        for i in 0..=n {
            if target == Some(i) {
                for l in &copied {
                    out.push((l.clone(), true));
                }
            }
            if i == n {
                break;
            }
            if !deleted(i) && !moved(i) {
                out.push((self.lines[i].clone(), false));
            }
            for (r, k) in &repeats {
                if r.end == i + 1 {
                    for _ in 0..*k {
                        for l in &self.lines[r.clone()] {
                            out.push((l.clone(), true));
                        }
                    }
                }
            }
            for (at, k) in &inserts {
                if *at == i {
                    for _ in 0..*k {
                        out.push((
                            Line {
                                seq: 0,
                                date: self.today,
                                text: String::new(),
                                fresh: true,
                            },
                            true,
                        ));
                    }
                }
            }
        }
        let did =
            !deletes.is_empty() || source.is_some() || !repeats.is_empty() || !inserts.is_empty();
        if !deletes.is_empty() || source.is_some() || !repeats.is_empty() {
            self.changed = true;
        }
        if did {
            self.lines = self.number(out);
        }
        Ok(())
    }

    /// Sequence numbers for new lines: between their neighbors (steps of
    /// 1.00 when there is room, else smaller); when there is no room, the
    /// whole member is numbered again from 1.00 by 1.00.
    fn number(&self, out: Vec<(Line, bool)>) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::with_capacity(out.len());
        let mut i = 0;
        while i < out.len() {
            if !out[i].1 {
                lines.push(out[i].0.clone());
                i += 1;
                continue;
            }
            // A run of new lines between two kept ones.
            let start = i;
            while i < out.len() && out[i].1 {
                i += 1;
            }
            let prev = lines.last().map(|l| l.seq).unwrap_or(0);
            let next = out.get(i).map(|(l, _)| l.seq);
            let count = (i - start) as u32;
            let step = match next {
                None => 100,
                Some(nx) if nx > prev => ((nx - prev) / (count + 1)).min(100),
                Some(_) => 0,
            };
            if step == 0 {
                // No room: number everything again.
                let mut all: Vec<Line> = out.into_iter().map(|(l, _)| l).collect();
                for (k, l) in all.iter_mut().enumerate() {
                    l.seq = (k as u32 + 1) * 100;
                }
                return all;
            }
            for (k, (l, _)) in out[start..i].iter().enumerate() {
                let mut l = l.clone();
                l.seq = prev + step * (k as u32 + 1);
                if !l.fresh {
                    l.date = self.today;
                }
                lines.push(l);
            }
        }
        lines
    }

    /// The index of the next line at or after `from` whose text has
    /// `what` (any case), wrapping to the top; None when no line has it.
    pub fn find(&self, what: &str, from: usize) -> Option<usize> {
        let w = what.to_ascii_uppercase();
        let n = self.lines.len();
        (0..n)
            .map(|k| (from + k) % n.max(1))
            .find(|&i| self.lines[i].text.to_ascii_uppercase().contains(&w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(texts: &[&str]) -> Editor {
        let recs: Vec<(String, String, String)> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("{}.00", i + 1), "261001".into(), t.to_string()))
            .collect();
        Editor::new(&recs, 80, 261102)
    }
    fn texts(e: &Editor) -> Vec<String> {
        e.lines.iter().map(|l| l.text.clone()).collect()
    }
    fn cmds(c: &[(usize, &str)]) -> Typed {
        Typed {
            commands: c.iter().map(|(i, s)| (*i, s.to_string())).collect(),
            texts: vec![],
        }
    }

    // Expected results: SEU User's Guide, line commands (I, D, DD, C, CC,
    // M, MM, A, B, R, RR), and SEU's numbering of new lines between their
    // neighbors.

    #[test]
    fn insert_type_and_blank_inserted_lines_go() {
        let mut e = ed(&["a", "b"]);
        e.apply(&cmds(&[(0, "I2")])).unwrap();
        assert_eq!(texts(&e), ["a", "", "", "b"]);
        // New lines between 1.00 and 2.00: 1.33 and 1.66.
        assert_eq!(e.lines[1].seq, 133);
        assert_eq!(e.lines[2].seq, 166);
        // One is typed on; Enter again: the blank one goes.
        e.apply(&Typed {
            commands: vec![],
            texts: vec![(1, "x".into())],
        })
        .unwrap();
        assert_eq!(texts(&e), ["a", "x", "b"]);
        assert_eq!(e.lines[1].date, 261102);
        assert!(e.changed);
        let (src, meta) = e.source();
        assert_eq!(src, "a\nx\nb\n");
        assert_eq!(meta, "000100261001\n000133261102\n000200261001");
    }

    #[test]
    fn delete_one_several_and_a_block() {
        let mut e = ed(&["a", "b", "c", "d", "e", "f"]);
        e.apply(&cmds(&[(0, "D"), (2, "DD"), (3, "DD")])).unwrap();
        assert_eq!(texts(&e), ["b", "e", "f"]);
        e.apply(&cmds(&[(1, "D5")])).unwrap();
        assert_eq!(texts(&e), ["b"]);
    }

    #[test]
    fn copy_move_after_and_before() {
        let mut e = ed(&["a", "b", "c", "d"]);
        e.apply(&cmds(&[(0, "C"), (3, "A")])).unwrap();
        assert_eq!(texts(&e), ["a", "b", "c", "d", "a"]);
        assert_eq!(e.lines[4].seq, 500);
        let mut e = ed(&["a", "b", "c", "d"]);
        e.apply(&cmds(&[(0, "MM"), (1, "MM"), (3, "B")])).unwrap();
        assert_eq!(texts(&e), ["c", "a", "b", "d"]);
        let mut e = ed(&["a", "b", "c"]);
        e.apply(&cmds(&[(2, "C2"), (0, "B")])).unwrap();
        assert_eq!(texts(&e), ["c", "a", "b", "c"]);
        // Before 1.00: between 0 and 1.00.
        assert_eq!(
            e.lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            [50, 100, 200, 300]
        );
        // No room between 1.00 and 1.01: the member is numbered again.
        let recs = vec![
            ("1.00".to_string(), "0".to_string(), "a".to_string()),
            ("1.01".to_string(), "0".to_string(), "b".to_string()),
        ];
        let mut e = Editor::new(&recs, 80, 261102);
        e.apply(&cmds(&[(0, "I")])).unwrap();
        assert_eq!(
            e.lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            [100, 200, 300]
        );
    }

    #[test]
    fn repeat_lines_and_blocks() {
        let mut e = ed(&["a", "b", "c"]);
        e.apply(&cmds(&[(0, "R2")])).unwrap();
        assert_eq!(texts(&e), ["a", "a", "a", "b", "c"]);
        let mut e = ed(&["a", "b", "c"]);
        e.apply(&cmds(&[(1, "RR"), (2, "RR")])).unwrap();
        assert_eq!(texts(&e), ["a", "b", "c", "b", "c"]);
    }

    #[test]
    fn errors_change_nothing() {
        let mut e = ed(&["a", "b"]);
        assert!(e.apply(&cmds(&[(0, "C")])).is_err());
        assert!(e.apply(&cmds(&[(0, "DD")])).is_err());
        assert!(e.apply(&cmds(&[(0, "Q")])).is_err());
        assert_eq!(texts(&e), ["a", "b"]);
        let mut e = ed(&["a", "b", "c"]);
        // The target inside the block.
        assert!(e.apply(&cmds(&[(0, "CC"), (2, "CC"), (1, "A")])).is_err());
        assert_eq!(texts(&e), ["a", "b", "c"]);
        assert!(!e.changed);
    }

    #[test]
    fn find_wraps() {
        let e = ed(&["dcl-s x", "eval y", "DCL-S z"]);
        assert_eq!(e.find("dcl-s", 1), Some(2));
        assert_eq!(e.find("dcl-s", 0), Some(0));
        assert_eq!(e.find("nothing", 0), None);
    }
}
