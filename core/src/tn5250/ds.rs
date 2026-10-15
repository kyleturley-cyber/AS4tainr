//! 5250 data stream: building display screens and parsing the input that
//! comes back. Written from the public 5250 data stream descriptions
//! (RFC 1205 and the 5250 functions reference); only what ibmish needs so far.
//!
//! Screen coordinates are 1-based (row 1..=24, column 1..=80), as in the
//! data stream itself.

use super::ebcdic;

pub const ROWS: u8 = 24;
pub const COLS: u8 = 80;

const ESC: u8 = 0x04;
const CMD_CLEAR_UNIT: u8 = 0x40;
const CMD_WRITE_TO_DISPLAY: u8 = 0x11;
const CMD_READ_MDT_FIELDS: u8 = 0x52;
const ORDER_SBA: u8 = 0x11; // set buffer address
const ORDER_IC: u8 = 0x13; // insert cursor
const ORDER_SF: u8 = 0x1D; // start of field

/// Display attributes (screen attribute bytes 0x20..=0x3F).
pub mod attr {
    pub const NORMAL: u8 = 0x20; // green
    pub const HIGH: u8 = 0x22; // white / high intensity
    pub const UNDERLINE: u8 = 0x24; // green, underlined (input fields)
    pub const NONDISPLAY: u8 = 0x27; // hidden (passwords)
    pub const RED: u8 = 0x28;
    pub const TURQUOISE: u8 = 0x30;
    pub const YELLOW: u8 = 0x32;
    pub const PINK: u8 = 0x38;
    pub const BLUE: u8 = 0x3A;
    /// Bits added to a base attribute.
    pub const REVERSE_BIT: u8 = 0x01;
    pub const UNDERLINE_BIT: u8 = 0x04;
}

/// Field format word (FFW). Byte 1 always has 0x40 added.
pub mod ffw {
    pub const BYPASS: u8 = 0x20; // output only
    pub const ALPHA: u8 = 0x00; // alphanumeric shift
    pub const NUMERIC_ONLY: u8 = 0x03;
    pub const SIGNED_NUMERIC: u8 = 0x07;
    /// Byte 2: fold input to upper case.
    pub const MONOCASE: u8 = 0x20;
}

/// Attention identifier: which key ended the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aid {
    Enter,
    F(u8),
    RollUp,   // Page Down
    RollDown, // Page Up
    Help,
    Clear,
    Other(u8),
}

impl Aid {
    pub fn from_byte(b: u8) -> Aid {
        match b {
            0xF1 => Aid::Enter,
            0x31..=0x3C => Aid::F(b - 0x30),      // F1..F12
            0xB1..=0xBC => Aid::F(b - 0xB0 + 12), // F13..F24
            0xF4 => Aid::RollDown,
            0xF5 => Aid::RollUp,
            0xF3 => Aid::Help,
            0xBD => Aid::Clear,
            other => Aid::Other(other),
        }
    }

    pub fn name(self) -> String {
        match self {
            Aid::Enter => "ENTER".into(),
            Aid::F(n) => format!("F{n}"),
            Aid::RollUp => "ROLLUP".into(),
            Aid::RollDown => "ROLLDOWN".into(),
            Aid::Help => "HELP".into(),
            Aid::Clear => "CLEAR".into(),
            Aid::Other(b) => format!("AID{b:02X}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Any characters.
    Text,
    /// Any characters, folded to upper case by the terminal.
    Upper,
    /// Not displayed (passwords).
    Hidden,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub row: u8,
    pub col: u8,
    pub len: u16,
    pub kind: FieldKind,
    pub value: String,
}

/// A full display screen, written with Clear Unit + Write To Display and
/// followed by Read MDT Fields.
#[derive(Debug, Default, Clone)]
pub struct Screen {
    orders: Vec<u8>,
    fields: Vec<Field>,
    cursor: Option<(u8, u8)>,
}

/// 0-based linear screen offset of (row, col).
fn linear(row: u8, col: u8) -> u16 {
    (row as u16 - 1) * COLS as u16 + col as u16 - 1
}

fn from_linear(p: u16) -> (u8, u8) {
    ((p / COLS as u16) as u8 + 1, (p % COLS as u16) as u8 + 1)
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    fn sba(&mut self, row: u8, col: u8) {
        self.orders.extend_from_slice(&[ORDER_SBA, row, col]);
    }

    /// Position the address on the attribute byte of an item starting at
    /// (row, col): the position before it, or the item itself at 1,1.
    fn sba_attribute(&mut self, row: u8, col: u8) -> bool {
        let lin = linear(row, col);
        if lin == 0 {
            self.sba(row, col);
            false
        } else {
            let (r, c) = from_linear(lin - 1);
            self.sba(r, c);
            true
        }
    }

    /// Normal attribute right after an item, so its attribute stops there.
    fn end_attribute(&mut self, row: u8, col: u8, len: u16) {
        let end = linear(row, col) + len;
        if end < ROWS as u16 * COLS as u16 {
            let (r, c) = from_linear(end);
            self.sba(r, c);
            self.orders.push(attr::NORMAL);
        }
    }

    /// Text at (row, col) with a display attribute in the position before it.
    pub fn text(&mut self, row: u8, col: u8, attribute: u8, s: &str) -> &mut Self {
        debug_assert!((1..=COLS).contains(&col) && (1..=ROWS).contains(&row));
        let max = (COLS - col + 1) as usize;
        let s: String = s.chars().take(max).collect();
        if self.sba_attribute(row, col) {
            self.orders.push(attribute);
        }
        self.orders.extend(ebcdic::encode(&s));
        if attribute != attr::NORMAL {
            self.end_attribute(row, col, s.chars().count() as u16);
        }
        self
    }

    /// Plain (green) text.
    pub fn put(&mut self, row: u8, col: u8, s: &str) -> &mut Self {
        self.text(row, col, attr::NORMAL, s)
    }

    /// An input field: underlined, or hidden for passwords.
    pub fn field(
        &mut self,
        name: &str,
        row: u8,
        col: u8,
        len: u16,
        kind: FieldKind,
        value: &str,
    ) -> &mut Self {
        let ffw2 = if kind == FieldKind::Upper {
            ffw::MONOCASE
        } else {
            0
        };
        let attribute = if kind == FieldKind::Hidden {
            attr::NONDISPLAY
        } else {
            attr::UNDERLINE
        };
        self.input_field(name, row, col, len, ffw::ALPHA, ffw2, attribute, value)
    }

    /// An input field with an explicit field format word and attribute.
    #[allow(clippy::too_many_arguments)]
    pub fn input_field(
        &mut self,
        name: &str,
        row: u8,
        col: u8,
        len: u16,
        ffw1: u8,
        ffw2: u8,
        attribute: u8,
        value: &str,
    ) -> &mut Self {
        self.sba_attribute(row, col);
        self.orders.extend_from_slice(&[
            ORDER_SF,
            0x40 | ffw1,
            ffw2,
            attribute,
            (len >> 8) as u8,
            len as u8,
        ]);
        let value: String = value.chars().take(len as usize).collect();
        if !value.is_empty() {
            self.orders.extend(ebcdic::encode(&value));
        }
        self.end_attribute(row, col, len);
        if self.cursor.is_none() {
            self.cursor = Some((row, col));
        }
        let kind = if attribute == attr::NONDISPLAY {
            FieldKind::Hidden
        } else if ffw2 & ffw::MONOCASE != 0 {
            FieldKind::Upper
        } else {
            FieldKind::Text
        };
        self.fields.push(Field {
            name: name.to_string(),
            row,
            col,
            len,
            kind,
            value,
        });
        self
    }

    /// Put the cursor at a position (default: the first input field).
    pub fn cursor_at(&mut self, row: u8, col: u8) -> &mut Self {
        self.cursor = Some((row, col));
        self
    }

    /// Message on line 24, high intensity.
    pub fn message(&mut self, msg: &str) -> &mut Self {
        if !msg.is_empty() {
            self.text(ROWS, 2, attr::HIGH, msg);
        }
        self
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The data stream for this screen: clear, write, then read modified fields.
    pub fn to_stream(&self) -> Vec<u8> {
        let mut out = vec![ESC, CMD_CLEAR_UNIT, ESC, CMD_WRITE_TO_DISPLAY, 0x00, 0x08];
        // CC1 0x00: no reset; CC2 0x08: unlock the keyboard.
        out.extend_from_slice(&self.orders);
        let (r, c) = self.cursor.unwrap_or((1, 2));
        out.extend_from_slice(&[ORDER_IC, r, c]);
        out.extend_from_slice(&[ESC, CMD_READ_MDT_FIELDS, 0x00, 0x00]);
        out
    }
}

/// What the terminal sent back after Read MDT Fields.
#[derive(Debug, Clone)]
pub struct Input {
    pub aid: Aid,
    pub cursor: (u8, u8),
    /// Field values by name: modified fields as typed, others as written.
    pub values: Vec<(String, String)>,
    /// Names of the fields the user changed.
    pub modified: Vec<String>,
}

impl Input {
    pub fn get(&self, name: &str) -> &str {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }
}

/// Parse the data of an input record against the screen that was shown.
pub fn parse_input(data: &[u8], screen: &Screen) -> Input {
    let cursor = (
        data.first().copied().unwrap_or(1),
        data.get(1).copied().unwrap_or(1),
    );
    let aid = Aid::from_byte(data.get(2).copied().unwrap_or(0xF1));
    let mut values: Vec<(String, String)> = screen
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect();
    let mut modified = Vec::new();
    let mut i = 3;
    while i + 2 < data.len() && data[i] == ORDER_SBA {
        let (row, col) = (data[i + 1], data[i + 2]);
        let start = i + 3;
        let mut end = start;
        while end < data.len() && data[end] != ORDER_SBA {
            end += 1;
        }
        let text = ebcdic::decode(&data[start..end]).trim_end().to_string();
        if let Some(pos) = screen
            .fields
            .iter()
            .position(|f| f.row == row && f.col == col)
        {
            values[pos].1 = text;
            modified.push(screen.fields[pos].name.clone());
        }
        i = end;
    }
    Input {
        aid,
        cursor,
        values,
        modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_stream_layout() {
        let mut s = Screen::new();
        s.text(1, 33, attr::HIGH, "Hi");
        s.field("USER", 6, 53, 10, FieldKind::Upper, "");
        let st = s.to_stream();
        // Clear unit, WTD with keyboard unlock.
        assert_eq!(&st[..6], &[0x04, 0x40, 0x04, 0x11, 0x00, 0x08]);
        // SBA 1,32 + high attribute + "Hi" (EBCDIC), then normal attribute at 1,35.
        assert_eq!(
            &st[6..16],
            &[0x11, 1, 32, 0x22, 0xC8, 0x89, 0x11, 1, 35, 0x20]
        );
        // SBA 6,52 + SF: FFW 40 20 (monocase), underline attribute, length 10.
        assert_eq!(
            &st[16..25],
            &[0x11, 6, 52, 0x1D, 0x40, 0x20, 0x24, 0x00, 0x0A]
        );
        // End attribute after the field at 6,63.
        assert_eq!(&st[25..29], &[0x11, 6, 63, 0x20]);
        // Insert cursor in the field, then Read MDT Fields.
        assert_eq!(&st[29..], &[0x13, 6, 53, 0x04, 0x52, 0x00, 0x00]);
    }

    #[test]
    fn column_one_puts_the_attribute_at_the_end_of_the_previous_row() {
        let mut s = Screen::new();
        s.text(3, 1, attr::HIGH, "X");
        let st = s.to_stream();
        assert_eq!(&st[6..10], &[0x11, 2, 80, 0x22]);
    }

    #[test]
    fn parse_modified_fields_and_aid() {
        let mut s = Screen::new();
        s.field("USER", 6, 53, 10, FieldKind::Upper, "");
        s.field("PASSWORD", 7, 53, 10, FieldKind::Hidden, "");
        s.field("LIB", 8, 53, 10, FieldKind::Upper, "QGPL");
        // Cursor 7,58; Enter; USER = "QSECOFR"; PASSWORD = "secret" (+ nulls).
        let mut data = vec![7, 58, 0xF1, 0x11, 6, 53];
        data.extend(ebcdic::encode("QSECOFR"));
        data.extend_from_slice(&[0x11, 7, 53]);
        data.extend(ebcdic::encode("secret"));
        data.extend_from_slice(&[0x00, 0x00]);
        let inp = parse_input(&data, &s);
        assert_eq!(inp.aid, Aid::Enter);
        assert_eq!(inp.cursor, (7, 58));
        assert_eq!(inp.get("USER"), "QSECOFR");
        assert_eq!(inp.get("PASSWORD"), "secret");
        assert_eq!(inp.get("LIB"), "QGPL", "unmodified fields keep their value");
        assert_eq!(inp.modified, vec!["USER", "PASSWORD"]);
    }

    #[test]
    fn aid_codes() {
        assert_eq!(Aid::from_byte(0x33), Aid::F(3));
        assert_eq!(Aid::from_byte(0x3C), Aid::F(12));
        assert_eq!(Aid::from_byte(0xB1), Aid::F(13));
        assert_eq!(Aid::from_byte(0xF5), Aid::RollUp);
        assert_eq!(Aid::F(3).name(), "F3");
    }
}
