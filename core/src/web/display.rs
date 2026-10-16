//! The terminal side of a 5250 session: the screen buffer a display station
//! keeps while the server writes to it, and the input record it sends back
//! when the operator presses an AID key.
//!
//! Written from the public 5250 data stream descriptions (RFC 1205 and the
//! 5250 functions reference). It covers what ibmish's server sends (Clear
//! Unit, Write To Display with SBA/IC/SF, Read MDT Fields) plus the common
//! orders a host may add (RA, EA, MC, SOH, TD), so it can be pointed at
//! other 5250 hosts later. Coordinates are 1-based in the data stream and
//! 0-based linear positions here.

use crate::tn5250::ebcdic;
use serde::{Deserialize, Serialize};

pub const ROWS: u16 = 24;
pub const COLS: u16 = 80;
const SIZE: usize = (ROWS * COLS) as usize;

const ESC: u8 = 0x04;

mod cmd {
    pub const CLEAR_UNIT: u8 = 0x40;
    pub const CLEAR_UNIT_ALTERNATE: u8 = 0x20;
    pub const CLEAR_FORMAT_TABLE: u8 = 0x50;
    pub const WRITE_TO_DISPLAY: u8 = 0x11;
    pub const WRITE_ERROR_CODE: u8 = 0x21;
    pub const READ_INPUT_FIELDS: u8 = 0x42;
    pub const READ_MDT_FIELDS: u8 = 0x52;
    pub const READ_MDT_ALTERNATE: u8 = 0x82;
    pub const WRITE_STRUCTURED_FIELD: u8 = 0xF3;
}

mod order {
    pub const SOH: u8 = 0x01;
    pub const RA: u8 = 0x02;
    pub const EA: u8 = 0x03;
    pub const TD: u8 = 0x10;
    pub const SBA: u8 = 0x11;
    pub const WEA: u8 = 0x12;
    pub const IC: u8 = 0x13;
    pub const MC: u8 = 0x14;
    pub const WDSF: u8 = 0x15;
    pub const SF: u8 = 0x1D;
}

/// FFW byte 1: bypass (output only) and modified data tag.
const FFW_BYPASS: u8 = 0x20;
const FFW_MDT: u8 = 0x08;

/// An input or output field from a Start Field order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    /// Linear position of the first data position.
    #[serde(rename = "p")]
    pub start: u16,
    #[serde(rename = "l")]
    pub len: u16,
    /// Field format word, when the field is input capable.
    #[serde(rename = "f")]
    pub ffw: Option<[u8; 2]>,
    #[serde(rename = "a")]
    pub attr: u8,
    #[serde(skip)]
    pub mdt: bool,
}

impl Field {
    pub fn is_input(&self) -> bool {
        matches!(self.ffw, Some([b1, _]) if b1 & FFW_BYPASS == 0)
    }
}

/// What the browser is told after the server writes to the screen.
#[derive(Debug, Serialize)]
pub struct ScreenMsg {
    pub t: &'static str,
    pub rows: u16,
    pub cols: u16,
    /// Screen text, one string per row. Attribute positions are blanks.
    pub lines: Vec<String>,
    /// Attribute bytes as [position, attribute]; each one applies until the next.
    pub attrs: Vec<(u16, u8)>,
    pub fields: Vec<Field>,
    pub cursor: u16,
    /// True while the keyboard is locked (the host has not asked for input).
    pub locked: bool,
    /// Error line text from Write Error Code, if any.
    pub error: Option<String>,
}

/// A key press from the browser: the AID, the cursor and the changed fields.
#[derive(Debug, Deserialize)]
pub struct AidMsg {
    pub aid: String,
    pub cursor: u16,
    #[serde(default)]
    pub fields: Vec<FieldValue>,
}

#[derive(Debug, Deserialize)]
pub struct FieldValue {
    pub p: u16,
    pub v: String,
}

/// AID byte for a key name (5250 functions reference, AID codes).
pub fn aid_byte(name: &str) -> Option<u8> {
    let n = name.to_ascii_uppercase();
    Some(match n.as_str() {
        "ENTER" => 0xF1,
        "HELP" => 0xF3,
        "ROLLDOWN" | "PAGEUP" => 0xF4,
        "ROLLUP" | "PAGEDOWN" => 0xF5,
        "PRINT" => 0xF6,
        "RECBACKSPACE" => 0xF8,
        "CLEAR" => 0xBD,
        f if f.starts_with('F') => {
            let k: u8 = f[1..].parse().ok()?;
            match k {
                1..=12 => 0x30 + k,
                13..=24 => 0xB0 + k - 12,
                _ => return None,
            }
        }
        _ => return None,
    })
}

pub struct Display {
    buf: Vec<u8>,
    fields: Vec<Field>,
    cursor: u16,
    addr: u16,
    locked: bool,
    read_pending: bool,
    error: Option<String>,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            buf: vec![0; SIZE],
            fields: Vec::new(),
            cursor: 0,
            addr: 0,
            locked: true,
            read_pending: false,
            error: None,
        }
    }
}

fn pos(row: u8, col: u8) -> Option<u16> {
    if (1..=ROWS).contains(&(row as u16)) && (1..=COLS).contains(&(col as u16)) {
        Some((row as u16 - 1) * COLS + col as u16 - 1)
    } else {
        None
    }
}

fn is_attr(b: u8) -> bool {
    (0x20..=0x3F).contains(&b)
}

impl Display {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn read_pending(&self) -> bool {
        self.read_pending
    }

    fn put(&mut self, b: u8) {
        self.buf[self.addr as usize] = b;
        self.addr = (self.addr + 1) % SIZE as u16;
    }

    /// Apply the data of one record from the host (header already removed).
    /// Returns a description of anything that was skipped, for the log.
    pub fn apply(&mut self, data: &[u8]) -> Option<String> {
        let mut i = 0;
        while i < data.len() {
            if data[i] != ESC {
                return Some(format!(
                    "expected a command at offset {i}, found {:02X}",
                    data[i]
                ));
            }
            let Some(&c) = data.get(i + 1) else {
                break;
            };
            i += 2;
            match c {
                cmd::CLEAR_UNIT | cmd::CLEAR_UNIT_ALTERNATE => {
                    if c == cmd::CLEAR_UNIT_ALTERNATE {
                        i += 1; // parameter byte
                    }
                    self.buf.fill(0);
                    self.fields.clear();
                    self.cursor = 0;
                    self.addr = 0;
                    self.error = None;
                    self.locked = true;
                    self.read_pending = false;
                }
                cmd::CLEAR_FORMAT_TABLE => {
                    self.fields.clear();
                    self.locked = true;
                }
                cmd::WRITE_TO_DISPLAY => {
                    let cc1 = data.get(i).copied().unwrap_or(0);
                    let cc2 = data.get(i + 1).copied().unwrap_or(0);
                    i += 2;
                    self.locked = true;
                    // CC1 high bits 0x40/0x60/0xA0/0xC0/0xE0 reset modified data tags.
                    if matches!(cc1 & 0xE0, 0x40 | 0x60 | 0xA0 | 0xC0 | 0xE0) {
                        for f in &mut self.fields {
                            f.mdt = false;
                        }
                    }
                    if let Err(e) = self.write_orders(data, &mut i) {
                        return Some(e);
                    }
                    // CC2 0x08: unlock the keyboard.
                    if cc2 & 0x08 != 0 {
                        self.locked = false;
                    }
                }
                cmd::WRITE_ERROR_CODE => {
                    let start = i;
                    while i < data.len() && data[i] != ESC {
                        i += 1;
                    }
                    let text: String = data[start..i]
                        .iter()
                        .filter(|&&b| b >= 0x40)
                        .map(|&b| ebcdic::TO_UNICODE[b as usize] as u8 as char)
                        .collect();
                    self.error = Some(text.trim().to_string());
                }
                cmd::READ_INPUT_FIELDS | cmd::READ_MDT_FIELDS | cmd::READ_MDT_ALTERNATE => {
                    i += 2; // CC1, CC2
                    self.read_pending = true;
                    self.locked = false;
                }
                cmd::WRITE_STRUCTURED_FIELD => {
                    // Length-prefixed; nothing ibmish needs answers to.
                    let len = data
                        .get(i..i + 2)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                        .unwrap_or(0);
                    i += len.max(2);
                }
                other => return Some(format!("command {other:02X} is not supported")),
            }
        }
        None
    }

    fn write_orders(&mut self, data: &[u8], i: &mut usize) -> Result<(), String> {
        let rc = |data: &[u8], at: usize| -> Result<u16, String> {
            match (data.get(at), data.get(at + 1)) {
                (Some(&r), Some(&c)) => {
                    pos(r, c).ok_or(format!("address {r},{c} is off the screen"))
                }
                _ => Err("truncated order".into()),
            }
        };
        while *i < data.len() && data[*i] != ESC {
            let b = data[*i];
            match b {
                order::SBA => {
                    self.addr = rc(data, *i + 1)?;
                    *i += 3;
                }
                order::IC | order::MC => {
                    self.cursor = rc(data, *i + 1)?;
                    *i += 3;
                }
                order::RA => {
                    let to = rc(data, *i + 1)?;
                    let ch = *data.get(*i + 3).ok_or("truncated RA")?;
                    *i += 4;
                    while self.addr <= to {
                        let at = self.addr;
                        self.put(ch);
                        if at == to || self.addr == 0 {
                            break;
                        }
                    }
                }
                order::EA => {
                    let to = rc(data, *i + 1)?;
                    let n = *data.get(*i + 3).ok_or("truncated EA")? as usize;
                    *i += 3 + n.max(1);
                    while self.addr <= to {
                        let at = self.addr;
                        self.put(0);
                        if at == to || self.addr == 0 {
                            break;
                        }
                    }
                }
                order::SOH => {
                    // The length byte counts itself.
                    let n = *data.get(*i + 1).ok_or("truncated order")? as usize;
                    *i += 1 + n.max(1);
                }
                order::WEA => *i += 3, // attribute type + attribute
                order::TD | order::WDSF => {
                    let n = data
                        .get(*i + 1..*i + 3)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                        .ok_or("truncated order")?;
                    if b == order::TD {
                        for k in 0..n {
                            let v = *data.get(*i + 3 + k).ok_or("truncated TD")?;
                            self.put(v);
                        }
                        *i += 3 + n;
                    } else {
                        *i += 1 + n.max(2);
                    }
                }
                order::SF => {
                    *i += 1;
                    let mut ffw = None;
                    if data.get(*i).is_some_and(|&x| x & 0xC0 == 0x40) {
                        ffw = Some([data[*i], *data.get(*i + 1).ok_or("truncated SF")?]);
                        *i += 2;
                        // Field control words: two bytes each, first byte 0x8x..0xBx.
                        while data.get(*i).is_some_and(|&x| x & 0xC0 == 0x80) {
                            *i += 2;
                        }
                    }
                    let attr = *data.get(*i).ok_or("truncated SF")?;
                    if !is_attr(attr) {
                        return Err(format!("SF without an attribute ({attr:02X})"));
                    }
                    let len = data
                        .get(*i + 1..*i + 3)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]))
                        .ok_or("truncated SF")?;
                    *i += 3;
                    self.put(attr);
                    let start = self.addr;
                    let len = len.min(SIZE as u16 - start);
                    let field = Field {
                        start,
                        len,
                        ffw,
                        attr,
                        mdt: ffw.is_some_and(|f| f[0] & FFW_MDT != 0),
                    };
                    match self.fields.iter().position(|f| f.start == start) {
                        Some(k) => self.fields[k] = field,
                        None => {
                            let at = self.fields.partition_point(|f| f.start < start);
                            self.fields.insert(at, field);
                        }
                    }
                }
                0x00 | 0x1C | 0x0E | 0x0F | 0x20..=0xFF => {
                    self.put(b);
                    *i += 1;
                }
                other => return Err(format!("order {other:02X} is not supported")),
            }
        }
        Ok(())
    }

    pub fn screen_msg(&self) -> ScreenMsg {
        let mut lines = Vec::with_capacity(ROWS as usize);
        let mut attrs = Vec::new();
        for r in 0..ROWS as usize {
            let row = &self.buf[r * COLS as usize..(r + 1) * COLS as usize];
            let mut s = String::with_capacity(COLS as usize);
            for (c, &b) in row.iter().enumerate() {
                if is_attr(b) {
                    attrs.push(((r * COLS as usize + c) as u16, b));
                    s.push(' ');
                } else if b < 0x40 {
                    s.push(' ');
                } else {
                    s.push(char::from_u32(ebcdic::TO_UNICODE[b as usize] as u32).unwrap_or(' '));
                }
            }
            lines.push(s);
        }
        ScreenMsg {
            t: "screen",
            rows: ROWS,
            cols: COLS,
            lines,
            attrs,
            fields: self.fields.clone(),
            cursor: self.cursor,
            locked: self.locked,
            error: self.error.clone(),
        }
    }

    /// Build the input record for an AID key, after putting the browser's
    /// field values into the buffer. Returns None while the keyboard is
    /// locked (the host has not asked for input) or for an unknown key.
    pub fn input(&mut self, msg: &AidMsg) -> Option<Vec<u8>> {
        if self.locked || !self.read_pending {
            return None;
        }
        let aid = aid_byte(&msg.aid)?;
        for fv in &msg.fields {
            let Some(k) = self
                .fields
                .iter()
                .position(|f| f.start == fv.p && f.is_input())
            else {
                continue;
            };
            let (start, len) = (self.fields[k].start as usize, self.fields[k].len as usize);
            let mut bytes: Vec<u8> = ebcdic::encode(&fv.v.chars().take(len).collect::<String>())
                .into_iter()
                // Control characters would break the record; send them as blanks.
                .map(|b| if b < 0x40 { 0x40 } else { b })
                .collect();
            bytes.resize(len, 0);
            self.buf[start..start + len].copy_from_slice(&bytes);
            self.fields[k].mdt = true;
        }
        let cursor = msg.cursor.min(SIZE as u16 - 1);
        self.cursor = cursor;
        let mut out = vec![(cursor / COLS + 1) as u8, (cursor % COLS + 1) as u8, aid];
        // Read MDT Fields: each changed input field as SBA + its data, with
        // trailing nulls left off.
        for f in self.fields.iter().filter(|f| f.mdt && f.is_input()) {
            let (s, l) = (f.start as usize, f.len as usize);
            let data = &self.buf[s..s + l];
            let used = data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
            out.extend_from_slice(&[0x11, (f.start / COLS + 1) as u8, (f.start % COLS + 1) as u8]);
            out.extend(data[..used].iter().map(|&b| if b == 0 { 0x40 } else { b }));
        }
        self.locked = true;
        self.read_pending = false;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tn5250::ds::{self, attr, Aid, FieldKind, Screen};

    fn at(row: u16, col: u16) -> u16 {
        (row - 1) * COLS + col - 1
    }

    /// The screens come from ibmish's own encoder (ds.rs), whose byte layout
    /// is checked against the 5250 data stream description in its tests.
    fn signon() -> Screen {
        let mut s = Screen::new();
        s.text(1, 36, attr::HIGH, "Sign On");
        s.put(6, 17, "User");
        s.field("USER", 6, 53, 10, FieldKind::Upper, "");
        s.field("PASSWORD", 7, 53, 10, FieldKind::Hidden, "");
        s.field("CURLIB", 8, 53, 10, FieldKind::Upper, "QGPL");
        s
    }

    #[test]
    fn draws_text_attributes_fields_and_cursor() {
        let mut d = Display::new();
        assert_eq!(d.apply(&signon().to_stream()), None);
        let m = d.screen_msg();
        assert_eq!(&m.lines[0][35..42], "Sign On");
        assert_eq!(&m.lines[5][16..20], "User");
        assert_eq!(&m.lines[7][52..56], "QGPL");
        // High intensity before "Sign On", normal after it.
        assert!(m.attrs.contains(&(at(1, 35), attr::HIGH)));
        assert!(m.attrs.contains(&(at(1, 43), attr::NORMAL)));
        assert_eq!(m.fields.len(), 3);
        assert_eq!(m.fields[0].start, at(6, 53));
        assert_eq!(m.fields[0].len, 10);
        // FFW 0x40 0x20: input, monocase.
        assert_eq!(m.fields[0].ffw, Some([0x40, 0x20]));
        assert_eq!(m.fields[1].attr, attr::NONDISPLAY);
        // Cursor in the first field; Read MDT Fields unlocks the keyboard.
        assert_eq!(m.cursor, at(6, 53));
        assert!(!m.locked);
        assert!(d.read_pending());
    }

    #[test]
    fn input_record_round_trips_through_the_server_parser() {
        let screen = signon();
        let mut d = Display::new();
        d.apply(&screen.to_stream());
        let rec = d
            .input(&AidMsg {
                aid: "ENTER".into(),
                cursor: at(7, 58),
                fields: vec![
                    FieldValue {
                        p: at(6, 53),
                        v: "QSECOFR".into(),
                    },
                    FieldValue {
                        p: at(7, 53),
                        v: "secret".into(),
                    },
                ],
            })
            .unwrap();
        let inp = ds::parse_input(&rec, &screen);
        assert_eq!(inp.aid, Aid::Enter);
        assert_eq!(inp.cursor, (7, 58));
        assert_eq!(inp.get("USER"), "QSECOFR");
        assert_eq!(inp.get("PASSWORD"), "secret");
        assert_eq!(inp.get("CURLIB"), "QGPL", "an unchanged field is not sent");
        assert_eq!(inp.modified, vec!["USER", "PASSWORD"]);
        // Locked until the host writes again.
        assert!(d
            .input(&AidMsg {
                aid: "ENTER".into(),
                cursor: 0,
                fields: vec![]
            })
            .is_none());
    }

    #[test]
    fn clearing_a_field_sends_it_empty() {
        let screen = signon();
        let mut d = Display::new();
        d.apply(&screen.to_stream());
        let rec = d
            .input(&AidMsg {
                aid: "F3".into(),
                cursor: 0,
                fields: vec![FieldValue {
                    p: at(8, 53),
                    v: "".into(),
                }],
            })
            .unwrap();
        let inp = ds::parse_input(&rec, &screen);
        assert_eq!(inp.aid, Aid::F(3));
        assert_eq!(inp.get("CURLIB"), "");
    }

    #[test]
    fn control_characters_cannot_forge_orders() {
        // A 0x11 (SBA) byte typed into a field would start a new field in
        // the record; it is sent as a blank instead.
        let screen = signon();
        let mut d = Display::new();
        d.apply(&screen.to_stream());
        let rec = d
            .input(&AidMsg {
                aid: "ENTER".into(),
                cursor: 0,
                fields: vec![FieldValue {
                    p: at(6, 53),
                    v: "A\u{11}\u{8}\u{1}B".into(),
                }],
            })
            .unwrap();
        assert_eq!(rec.iter().filter(|&&b| b == 0x11).count(), 1);
        assert_eq!(ds::parse_input(&rec, &screen).get("USER"), "A   B");
    }

    #[test]
    fn values_are_cut_to_the_field_length_and_unknown_fields_ignored() {
        let screen = signon();
        let mut d = Display::new();
        d.apply(&screen.to_stream());
        let rec = d
            .input(&AidMsg {
                aid: "ENTER".into(),
                cursor: 0,
                fields: vec![
                    FieldValue {
                        p: at(6, 53),
                        v: "ABCDEFGHIJKLMNOP".into(),
                    },
                    FieldValue {
                        p: at(1, 1),
                        v: "X".into(),
                    },
                ],
            })
            .unwrap();
        let inp = ds::parse_input(&rec, &screen);
        assert_eq!(inp.get("USER"), "ABCDEFGHIJ");
        assert_eq!(inp.modified, vec!["USER"]);
    }

    #[test]
    fn aid_codes_match_the_server() {
        for (name, aid) in [
            ("ENTER", Aid::Enter),
            ("F1", Aid::F(1)),
            ("F12", Aid::F(12)),
            ("F13", Aid::F(13)),
            ("F24", Aid::F(24)),
            ("ROLLUP", Aid::RollUp),
            ("ROLLDOWN", Aid::RollDown),
            ("HELP", Aid::Help),
            ("CLEAR", Aid::Clear),
        ] {
            assert_eq!(Aid::from_byte(aid_byte(name).unwrap()), aid, "{name}");
        }
        assert_eq!(aid_byte("F25"), None);
        assert_eq!(aid_byte("ATTN"), None);
    }

    #[test]
    fn repeat_to_address_and_overlay_fields() {
        // RA: SBA 2,1, RA to 2,10 with '-' (0x60): ten dashes.
        let mut d = Display::new();
        let data = [
            0x04, 0x40, 0x04, 0x11, 0x00, 0x08, 0x11, 2, 1, 0x02, 2, 10, 0x60,
        ];
        assert_eq!(d.apply(&data), None);
        assert_eq!(&d.screen_msg().lines[1][..11], "---------- ");
        // A second SF at the same position replaces the field.
        let mut d = Display::new();
        let sf = |len: u8| [0x11, 3, 9, 0x1D, 0x40, 0x00, 0x24, 0x00, len];
        let mut data = vec![0x04, 0x11, 0x00, 0x08];
        data.extend(sf(5));
        data.extend(sf(7));
        d.apply(&data);
        assert_eq!(d.fields().len(), 1);
        assert_eq!(d.fields()[0].len, 7);
        assert_eq!(d.fields()[0].start, at(3, 10));
    }

    #[test]
    fn output_only_and_bypass_fields_are_not_input() {
        let mut d = Display::new();
        // SF with no FFW (attribute first): output only. SF with bypass FFW.
        let data = [
            0x04, 0x11, 0x00, 0x08, 0x11, 4, 1, 0x1D, 0x22, 0x00, 0x05, 0x11, 5, 1, 0x1D, 0x60,
            0x00, 0x20, 0x00, 0x05,
        ];
        assert_eq!(d.apply(&data), None);
        assert_eq!(d.fields().len(), 2);
        assert!(d.fields().iter().all(|f| !f.is_input()));
    }

    #[test]
    fn unknown_orders_are_reported_not_misdrawn() {
        let mut d = Display::new();
        let err = d.apply(&[0x04, 0x11, 0x00, 0x08, 0x19, 0x40]);
        assert!(err.unwrap().contains("19"));
    }
}
