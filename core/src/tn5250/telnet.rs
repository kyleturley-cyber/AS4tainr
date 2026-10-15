//! Telnet layer of TN5250 (RFC 854/885/1091/856, RFC 1205, RFC 2877).
//!
//! The server negotiates TERMINAL-TYPE, END-OF-RECORD and BINARY, then
//! exchanges 5250 records: a 10-byte header plus a 5250 data stream,
//! with 0xFF doubled and IAC EOR marking the end of each record.

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const IAC: u8 = 255;
pub const DONT: u8 = 254;
pub const DO: u8 = 253;
pub const WONT: u8 = 252;
pub const WILL: u8 = 251;
pub const SB: u8 = 250;
pub const SE: u8 = 240;
pub const EOR: u8 = 239;

pub const OPT_BINARY: u8 = 0;
pub const OPT_TTYPE: u8 = 24;
pub const OPT_EOR: u8 = 25;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// 5250 record opcodes (RFC 1205 header, byte 9).
pub mod opcode {
    pub const NO_OP: u8 = 0x00;
    pub const INVITE: u8 = 0x01;
    pub const OUTPUT_ONLY: u8 = 0x02;
    pub const PUT_GET: u8 = 0x03;
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// IAC <cmd> <option>
    Command(u8, u8),
    /// IAC SB <option> <data> IAC SE
    Sub(u8, Vec<u8>),
    /// A complete record (header and data), IAC EOR removed and FF undoubled.
    Record(Vec<u8>),
}

#[derive(Default)]
enum State {
    #[default]
    Data,
    Iac,
    Cmd(u8),
    Sub,
    SubIac,
}

/// Incremental parser for the telnet byte stream.
#[derive(Default)]
pub struct Parser {
    state: State,
    record: Vec<u8>,
    sub: Vec<u8>,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Event>) {
        for &b in bytes {
            self.state = match std::mem::take(&mut self.state) {
                State::Data => {
                    if b == IAC {
                        State::Iac
                    } else {
                        self.record.push(b);
                        State::Data
                    }
                }
                State::Iac => match b {
                    IAC => {
                        self.record.push(IAC);
                        State::Data
                    }
                    EOR => {
                        out.push(Event::Record(std::mem::take(&mut self.record)));
                        State::Data
                    }
                    DO | DONT | WILL | WONT => State::Cmd(b),
                    SB => {
                        self.sub.clear();
                        State::Sub
                    }
                    // NOP, GA and others carry no option byte.
                    _ => State::Data,
                },
                State::Cmd(cmd) => {
                    out.push(Event::Command(cmd, b));
                    State::Data
                }
                State::Sub => {
                    if b == IAC {
                        State::SubIac
                    } else {
                        self.sub.push(b);
                        State::Sub
                    }
                }
                State::SubIac => match b {
                    SE => {
                        let mut data = std::mem::take(&mut self.sub);
                        let opt = if data.is_empty() { 0 } else { data.remove(0) };
                        out.push(Event::Sub(opt, data));
                        State::Data
                    }
                    IAC => {
                        self.sub.push(IAC);
                        State::Sub
                    }
                    _ => State::Sub,
                },
            };
        }
    }
}

/// Build one record: header + data, FF doubled, IAC EOR appended.
pub fn frame_record(op: u8, data: &[u8]) -> Vec<u8> {
    let len = data.len() + 10;
    let mut rec = Vec::with_capacity(len + 2);
    rec.extend_from_slice(&[(len >> 8) as u8, len as u8]); // logical record length
    rec.extend_from_slice(&[0x12, 0xA0]); // record type: GDS
    rec.extend_from_slice(&[0x00, 0x00]); // reserved
    rec.push(0x04); // variable header length
    rec.extend_from_slice(&[0x00, 0x00]); // flags
    rec.push(op);
    rec.extend_from_slice(data);
    let mut out = Vec::with_capacity(rec.len() + 8);
    for b in rec {
        out.push(b);
        if b == IAC {
            out.push(IAC);
        }
    }
    out.extend_from_slice(&[IAC, EOR]);
    out
}

/// The 5250 data portion of a received record (header stripped).
pub fn record_data(rec: &[u8]) -> Result<&[u8]> {
    if rec.len() < 10 || rec[2] != 0x12 || rec[3] != 0xA0 {
        bail!("not a 5250 GDS record");
    }
    let start = 6 + rec[6] as usize;
    Ok(rec.get(start..).unwrap_or(&[]))
}

pub struct Connection {
    stream: TcpStream,
    parser: Parser,
    events: std::collections::VecDeque<Event>,
    pub terminal_type: String,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Connection {
            stream,
            parser: Parser::default(),
            events: Default::default(),
            terminal_type: String::new(),
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).await?;
        Ok(())
    }

    async fn next_event(&mut self) -> Result<Event> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Ok(e);
            }
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).await?;
            if n == 0 {
                bail!("client disconnected");
            }
            let mut evs = Vec::new();
            self.parser.feed(&buf[..n], &mut evs);
            self.events.extend(evs);
        }
    }

    /// Refuse options we did not ask for.
    async fn refuse(&mut self, cmd: u8, opt: u8) -> Result<()> {
        match cmd {
            DO => self.send(&[IAC, WONT, opt]).await,
            WILL => self.send(&[IAC, DONT, opt]).await,
            _ => Ok(()),
        }
    }

    /// Server-side negotiation. Returns the client's terminal type.
    pub async fn negotiate(&mut self) -> Result<String> {
        self.send(&[IAC, DO, OPT_TTYPE]).await?;
        // Terminal type.
        loop {
            match self.next_event().await? {
                Event::Command(WILL, OPT_TTYPE) => {
                    self.send(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE])
                        .await?;
                }
                Event::Command(WONT, OPT_TTYPE) => {
                    bail!("client refused TERMINAL-TYPE: not a 5250 client")
                }
                Event::Sub(OPT_TTYPE, data) if data.first() == Some(&TTYPE_IS) => {
                    self.terminal_type = String::from_utf8_lossy(&data[1..]).trim().to_string();
                    break;
                }
                Event::Command(cmd, opt) => self.refuse(cmd, opt).await?,
                _ => {}
            }
        }
        if !self.terminal_type.to_ascii_uppercase().starts_with("IBM-") {
            bail!(
                "terminal type '{}' is not a 5250 display",
                self.terminal_type
            );
        }
        self.send(&[
            IAC, DO, OPT_EOR, IAC, WILL, OPT_EOR, IAC, DO, OPT_BINARY, IAC, WILL, OPT_BINARY,
        ])
        .await?;
        // Wait for the four answers (in any order).
        let mut need = [
            (WILL, OPT_EOR),
            (DO, OPT_EOR),
            (WILL, OPT_BINARY),
            (DO, OPT_BINARY),
        ]
        .to_vec();
        while !need.is_empty() {
            match self.next_event().await? {
                Event::Command(cmd, opt) => {
                    if let Some(i) = need.iter().position(|&x| x == (cmd, opt)) {
                        need.remove(i);
                    } else if cmd == WONT || cmd == DONT {
                        if opt == OPT_EOR || opt == OPT_BINARY {
                            bail!("client refused option {opt}");
                        }
                    } else {
                        self.refuse(cmd, opt).await?;
                    }
                }
                Event::Record(r) if r.is_empty() => {}
                _ => {}
            }
        }
        Ok(self.terminal_type.clone())
    }

    pub async fn send_record(&mut self, op: u8, data: &[u8]) -> Result<()> {
        let framed = frame_record(op, data);
        self.send(&framed).await
    }

    /// Wait for the next 5250 record from the client; returns its data portion.
    pub async fn read_record(&mut self) -> Result<Vec<u8>> {
        loop {
            match self
                .next_event()
                .await
                .context("reading from the 5250 client")?
            {
                Event::Record(r) if !r.is_empty() => return Ok(record_data(&r)?.to_vec()),
                Event::Command(cmd, opt) => self.refuse(cmd, opt).await?,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_splits_commands_subs_and_records() {
        let mut p = Parser::default();
        let mut ev = Vec::new();
        // WILL TTYPE, SB TTYPE IS "IBM-3179-2" SE, a record containing a doubled FF.
        let mut input = vec![IAC, WILL, OPT_TTYPE, IAC, SB, OPT_TTYPE, 0];
        input.extend_from_slice(b"IBM-3179-2");
        input.extend_from_slice(&[IAC, SE, 1, 2, IAC, IAC, 3, IAC, EOR]);
        // Feed in two pieces to check that state survives a split.
        p.feed(&input[..9], &mut ev);
        p.feed(&input[9..], &mut ev);
        assert_eq!(ev[0], Event::Command(WILL, OPT_TTYPE));
        let mut tt = vec![0];
        tt.extend_from_slice(b"IBM-3179-2");
        assert_eq!(ev[1], Event::Sub(OPT_TTYPE, tt));
        assert_eq!(ev[2], Event::Record(vec![1, 2, IAC, 3]));
    }

    #[test]
    fn frame_has_header_doubles_ff_and_ends_with_eor() {
        let f = frame_record(opcode::PUT_GET, &[0x04, 0xFF]);
        assert_eq!(&f[..10], &[0x00, 0x0C, 0x12, 0xA0, 0, 0, 0x04, 0, 0, 0x03]);
        assert_eq!(&f[10..], &[0x04, 0xFF, 0xFF, IAC, EOR]);
        // Round trip through the parser.
        let mut p = Parser::default();
        let mut ev = Vec::new();
        p.feed(&f, &mut ev);
        let Event::Record(r) = &ev[0] else { panic!() };
        assert_eq!(record_data(r).unwrap(), &[0x04, 0xFF]);
    }
}
