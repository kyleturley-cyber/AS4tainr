//! DSPPFM's display of a data member: its records as bytes in the record
//! format's layout, as characters, as hexadecimal beside the characters
//! (F10) or as characters over their zone and digit lines (F11).
//!
//! Layout observed on IBM i 7.5 (PUB400 2026-10-27, 5250 screens
//! tools/golden/screens5.txt: 08_dsppfm, 09_dsppfm_f10, 10_dsppfm_f11,
//! 12_dsppfm_wide, 13_dsppfm_wide_f20, 14_dsppfm_wide_f10).

/// The member a DSPPFM display shows.
#[derive(Debug, Clone, Default)]
pub struct MemberView {
    pub file: String,
    pub library: String,
    pub member: String,
    pub reclen: usize,
    /// (relative record number, record bytes).
    pub records: Vec<(i64, Vec<u8>)>,
}

/// How the records are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The characters (bytes below X'40' blank).
    Char,
    /// 20 bytes a line in hexadecimal, then their characters between '*'.
    Hex,
    /// A line of characters, a line of zone digits, a line of low digits,
    /// a blank line.
    Vertical,
}

/// Record lines on the display (rows 7-21).
pub const ROWS: usize = 15;
/// Columns of a character line.
pub const COLS: usize = 78;
/// Bytes of a hexadecimal line.
pub const HEX_BYTES: usize = 20;

/// The note a member without records shows (its place as on IBM i).
pub const NO_RECORDS: &str = "(The member has no records)";

/// How far F19 and F20 move in a mode.
pub fn shift(mode: Mode) -> usize {
    match mode {
        Mode::Hex => HEX_BYTES,
        _ => COLS,
    }
}

fn chars(b: &[u8]) -> String {
    crate::cpyf::printable(b)
}

fn nibbles(b: &[u8], high: bool) -> String {
    b.iter()
        .map(|&x| {
            let n = if high { x >> 4 } else { x & 0x0F };
            char::from_digit(n as u32, 16)
                .unwrap_or('0')
                .to_ascii_uppercase()
        })
        .collect()
}

/// The ruler for record positions `from` to `to` (1-based).
pub fn ruler(from: usize, to: usize) -> String {
    crate::tn5250::session::ruler(from, to)
}

/// Lines 1-6 and 22-23 of the display, and the record lines from
/// record `top` (an index into the records) and byte `left` (0-based).
pub fn screen(v: &MemberView, top: usize, left: usize, mode: Mode) -> Vec<String> {
    let mut rows = vec![String::new(); 24];
    rows[0] = format!("{:26}Display Physical File Member", "");
    // A member with no records: no record number, Control or Find, a
    // ruler of 78 columns whatever the record length, a note in
    // parentheses from column 4 of line 8 (as4tainr's own words: the
    // text is IBM's), and F3 and F12 only (PUB400 2026-10-28,
    // tools/golden/screens6.txt 12_dsppfm_empty).
    if v.records.is_empty() {
        rows[1] = format!(
            " File . . . . . . :   {:<20}Library  . . . . :   {}",
            v.file, v.library
        );
        rows[2] = format!(" Member . . . . . :   {}", v.member);
        rows[5] = format!(" {}", ruler(1, COLS));
        rows[7] = format!("   {NO_RECORDS}");
        rows[21] = format!("{:73}Bottom", "");
        rows[22] = " F3=Exit   F12=Cancel".to_string();
        return rows.iter().map(|r| r.trim_end().to_string()).collect();
    }
    let rec_no = v.records.get(top).map(|r| r.0).unwrap_or(0);
    rows[1] = format!(
        " File . . . . . . :   {:<20}Library  . . . . :   {}",
        v.file, v.library
    );
    rows[2] = format!(
        " Member . . . . . :   {:<20}Record . . . . . :   {}",
        v.member, rec_no
    );
    rows[3] = format!(
        " Control  . . . . .{:23}Column . . . . . :   {}",
        "",
        left + 1
    );
    rows[4] = " Find . . . . . . .".to_string();
    let width = |n: usize| v.reclen.saturating_sub(left).min(n);
    let mut body: Vec<String> = Vec::new();
    match mode {
        Mode::Char | Mode::Vertical => {
            let w = width(COLS);
            rows[5] = format!(" {}", ruler(left + 1, left + w));
            for (_, b) in v.records.iter().skip(top) {
                let part = b.get(left..left + w).unwrap_or(&[]);
                body.push(format!(" {}", chars(part)).trim_end().to_string());
                if mode == Mode::Vertical {
                    body.push(format!(" {}", nibbles(part, true)));
                    body.push(format!(" {}", nibbles(part, false)));
                    body.push(String::new());
                }
            }
        }
        Mode::Hex => {
            let w = width(HEX_BYTES);
            let r: Vec<char> = ruler(left + 1, left + w).chars().collect();
            let groups: Vec<String> = r
                .chunks(4)
                .map(|g| g.iter().map(|c| format!("{c} ")).collect())
                .collect();
            rows[5] = format!(" {}    {}", groups.join(" "), r.iter().collect::<String>());
            for (_, b) in v.records.iter().skip(top) {
                let part = b.get(left..left + w).unwrap_or(&[]);
                let hex: Vec<String> = part
                    .chunks(4)
                    .map(|g| {
                        let mut s = nibbles(g, true)
                            .chars()
                            .zip(nibbles(g, false).chars())
                            .flat_map(|(a, b)| [a, b])
                            .collect::<String>();
                        while s.len() < 8 {
                            s.push(' ');
                        }
                        s
                    })
                    .collect();
                body.push(format!(" {}   *{:<w$}*", hex.join(" "), chars(part)));
            }
        }
    }
    // The end of the member shows after its last record when it fits.
    let lines_per = if mode == Mode::Vertical { 4 } else { 1 };
    let remaining = (v.records.len() - top.min(v.records.len())) * lines_per;
    let more = remaining > ROWS;
    if !more {
        body.push(format!("{:27}****** END OF DATA ******", ""));
    }
    for (i, l) in body.into_iter().take(ROWS).enumerate() {
        rows[6 + i] = l;
    }
    rows[21] = if more {
        format!("{:72}More...", "")
    } else {
        format!("{:73}Bottom", "")
    };
    rows[22] = " F3=Exit   F12=Cancel   F19=Left   F20=Right   F24=More keys".to_string();
    rows.iter().map(|r| r.trim_end().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf() -> MemberView {
        let rec = |n: &[u8], name: &str| {
            let mut b = n.to_vec();
            b.extend(crate::tn5250::ebcdic::encode(&format!("{name:<20}")));
            b
        };
        MemberView {
            file: "SCR5PF".into(),
            library: "AS4TRAINR1".into(),
            member: "SCR5PF".into(),
            reclen: 23,
            records: vec![
                (1, rec(&[0x00, 0x00, 0x1F], "Alpha")),
                (2, rec(&[0x00, 0x02, 0x3D], "Beta")),
                (3, rec(&[0x00, 0x45, 0x6F], "Gamma")),
            ],
        }
    }

    // Expected lines: screens5 08_dsppfm (a packed 5,0 and a 20-byte
    // character field; the capture had each record twice).
    #[test]
    fn character_view() {
        let s = screen(&pf(), 0, 0, Mode::Char);
        assert_eq!(
            s[0],
            "                          Display Physical File Member"
        );
        assert_eq!(
            s[1],
            " File . . . . . . :   SCR5PF              Library  . . . . :   AS4TRAINR1"
        );
        assert_eq!(
            s[2],
            " Member . . . . . :   SCR5PF              Record . . . . . :   1"
        );
        assert_eq!(
            s[3],
            " Control  . . . . .                       Column . . . . . :   1"
        );
        assert_eq!(s[4], " Find . . . . . . .");
        assert_eq!(s[5], " *...+....1....+....2...");
        assert_eq!(s[6], "    Alpha");
        assert_eq!(s[7], "    Beta");
        // X'45' is 'á' and X'6F' '?' in CCSID 37 (as on the capture).
        assert_eq!(s[8], "  á?Gamma");
        assert_eq!(s[9], "                           ****** END OF DATA ******");
        assert_eq!(
            s[21],
            "                                                                         Bottom"
        );
        assert_eq!(
            s[22],
            " F3=Exit   F12=Cancel   F19=Left   F20=Right   F24=More keys"
        );
    }

    // 09_dsppfm_f10.
    #[test]
    fn hex_view() {
        let s = screen(&pf(), 0, 0, Mode::Hex);
        assert_eq!(
            s[5],
            " * . . .  + . . .  . 1 . .  . . + .  . . . 2     *...+....1....+....2"
        );
        assert_eq!(
            s[6],
            " 00001FC1 93978881 40404040 40404040 40404040   *   Alpha            *"
        );
        assert_eq!(
            s[8],
            " 00456FC7 81949481 40404040 40404040 40404040   * á?Gamma            *"
        );
    }

    // 10_dsppfm_f11: characters, zone digits, low digits, a blank line.
    #[test]
    fn vertical_view() {
        let s = screen(&pf(), 0, 0, Mode::Vertical);
        assert_eq!(s[5], " *...+....1....+....2...");
        assert_eq!(s[6], "    Alpha");
        assert_eq!(s[7], " 001C9988444444444444444");
        assert_eq!(s[8], " 00F13781000000000000000");
        assert_eq!(s[9], "");
        assert_eq!(s[10], "    Beta");
        assert_eq!(s[11], " 003C8A84444444444444444");
        assert_eq!(s[12], " 02D25310000000000000000");
    }

    // screens6 12_dsppfm_empty: a table of one CHAR(5) column, no rows.
    #[test]
    fn empty_member() {
        let v = MemberView {
            file: "SCR6E".into(),
            library: "AS4TRAINR1".into(),
            member: "SCR6E".into(),
            reclen: 5,
            records: vec![],
        };
        let s = screen(&v, 0, 0, Mode::Char);
        assert_eq!(
            s[1],
            " File . . . . . . :   SCR6E               Library  . . . . :   AS4TRAINR1"
        );
        assert_eq!(s[2], " Member . . . . . :   SCR6E");
        assert_eq!(s[3], "");
        assert_eq!(s[4], "");
        assert_eq!(
            s[5],
            " *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+..."
        );
        assert_eq!(s[6], "");
        assert_eq!(s[7], format!("   {NO_RECORDS}"));
        assert_eq!(s[8], "");
        assert_eq!(
            s[21],
            "                                                                         Bottom"
        );
        assert_eq!(s[22], " F3=Exit   F12=Cancel");
    }

    // screens6 07_dsppfm, 08_dsppfm_f10, 09_dsppfm_f11: a source member
    // of record length 92 (SRCSEQ ZONED(6,2), SRCDAT ZONED(6,0), SRCDTA
    // CHAR(80)) with records 1.00/261027 'line one' and 2.50/0
    // '  line two'.
    #[test]
    fn source_member() {
        let rec = |s: &str| crate::tn5250::ebcdic::encode(&format!("{s:<92}"));
        let v = MemberView {
            file: "SCR6SRC".into(),
            library: "AS4TRAINR1".into(),
            member: "M1".into(),
            reclen: 92,
            records: vec![
                (1, rec("000100261027line one")),
                (2, rec("000250000000  line two")),
            ],
        };
        let s = screen(&v, 0, 0, Mode::Char);
        assert_eq!(
            s[2],
            " Member . . . . . :   M1                  Record . . . . . :   1"
        );
        assert_eq!(
            s[5],
            " *...+....1....+....2....+....3....+....4....+....5....+....6....+....7....+..."
        );
        assert_eq!(s[6], " 000100261027line one");
        assert_eq!(s[7], " 000250000000  line two");
        assert_eq!(s[8], "                           ****** END OF DATA ******");
        let s = screen(&v, 0, 0, Mode::Hex);
        assert_eq!(
            s[6],
            " F0F0F0F1 F0F0F2F6 F1F0F2F7 93899585 40969585   *000100261027line one*"
        );
        assert_eq!(
            s[7],
            " F0F0F0F2 F5F0F0F0 F0F0F0F0 40409389 958540A3   *000250000000  line t*"
        );
        let s = screen(&v, 0, 0, Mode::Vertical);
        assert_eq!(s[6], " 000100261027line one");
        assert_eq!(
            s[7],
            " FFFFFFFFFFFF989849984444444444444444444444444444444444444444444444444444444444"
        );
        assert_eq!(
            s[8],
            " 000100261027395506550000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(s[9], "");
        assert_eq!(
            s[11],
            " FFFFFFFFFFFF4498984AA944444444444444444444444444444444444444444444444444444444"
        );
        assert_eq!(
            s[14],
            "                           ****** END OF DATA ******"
        );
    }

    // 12_dsppfm_wide, 13_dsppfm_wide_f20, 14_dsppfm_wide_f10: a 160-byte
    // record of A CHAR(100) and B CHAR(60).
    #[test]
    fn wide_records() {
        let b = crate::tn5250::ebcdic::encode(&format!("{:<100}{:<60}", "left", "right end"));
        let v = MemberView {
            file: "SCR5W".into(),
            library: "AS4TRAINR1".into(),
            member: "SCR5W".into(),
            reclen: 160,
            records: vec![(1, b)],
        };
        let s = screen(&v, 0, 0, Mode::Char);
        assert_eq!(s[5], format!(" {}", ruler(1, 78)));
        assert!(s[5].ends_with("....+....7....+..."));
        assert_eq!(s[6], " left");
        let s = screen(&v, 0, 78, Mode::Char);
        assert_eq!(
            s[3],
            " Control  . . . . .                       Column . . . . . :   79"
        );
        assert_eq!(
            s[5],
            " .8....+....9....+....0....+....1....+....2....+....3....+....4....+....5....+."
        );
        assert_eq!(s[6], "                       right end");
        let s = screen(&v, 0, 78, Mode::Hex);
        assert_eq!(
            s[5],
            " . 8 . .  . . + .  . . . 9  . . . .  + . . .     .8....+....9....+..."
        );
        assert_eq!(
            s[6],
            " 40404040 40404040 40404040 40404040 40404040   *                    *"
        );
    }
}
