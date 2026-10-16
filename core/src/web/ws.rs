//! A small WebSocket server side (RFC 6455): the opening handshake and
//! data frames. Dependency-free on purpose, with SHA-1 (RFC 3174) and
//! base64 (RFC 4648) for the handshake only.

use anyhow::{bail, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message accepted from a browser. Input records are tiny.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// RFC 6455 section 1.3: the GUID appended to Sec-WebSocket-Key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The Sec-WebSocket-Accept value for a client's Sec-WebSocket-Key.
pub fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{}{GUID}", key.trim()).as_bytes()))
}

#[derive(Debug, PartialEq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Read one message from a client, joining fragments. Client frames must be
/// masked (RFC 6455 section 5.1).
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Message> {
    let mut data = Vec::new();
    let mut first_op = None;
    loop {
        let mut head = [0u8; 2];
        r.read_exact(&mut head).await?;
        let fin = head[0] & 0x80 != 0;
        let op = head[0] & 0x0F;
        if head[1] & 0x80 == 0 {
            bail!("unmasked frame from the browser");
        }
        let len = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).await?;
                u16::from_be_bytes(b) as u64
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).await?;
                u64::from_be_bytes(b)
            }
            n => n as u64,
        };
        if len as usize > MAX_MESSAGE || data.len() + len as usize > MAX_MESSAGE {
            bail!("message from the browser is too large");
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask).await?;
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).await?;
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        match op {
            OP_CLOSE => return Ok(Message::Close),
            OP_PING => return Ok(Message::Ping(payload)),
            OP_PONG => return Ok(Message::Pong),
            OP_TEXT | OP_BINARY if first_op.is_none() => first_op = Some(op),
            OP_CONT if first_op.is_some() => {}
            _ => bail!("unexpected WebSocket opcode {op}"),
        }
        data.extend_from_slice(&payload);
        if fin {
            return Ok(match first_op {
                Some(OP_TEXT) => Message::Text(String::from_utf8(data)?),
                _ => Message::Binary(data),
            });
        }
    }
}

/// One unmasked server frame.
pub fn frame(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | op);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= 0xFFFF => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

pub async fn send_text<W: AsyncWrite + Unpin>(w: &mut W, text: &str) -> Result<()> {
    w.write_all(&frame(OP_TEXT, text.as_bytes())).await?;
    Ok(())
}

pub async fn send_pong<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> Result<()> {
    w.write_all(&frame(OP_PONG, payload)).await?;
    Ok(())
}

pub async fn send_close<W: AsyncWrite + Unpin>(w: &mut W) -> Result<()> {
    w.write_all(&frame(OP_CLOSE, &[])).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_test_vectors() {
        // RFC 3174 section 7.3, test 1 and test 2.
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn base64_test_vectors() {
        // RFC 4648 section 10.
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn handshake_accept_key() {
        // RFC 6455 section 1.3 example.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[tokio::test]
    async fn masked_and_fragmented_frames() {
        // RFC 6455 section 5.7 examples: a masked single-frame "Hello" and
        // an unmasked fragmented "Hel" + "lo" (masked here, as a client must).
        let mut bytes = vec![
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let mask = [1u8, 2, 3, 4];
        bytes.extend_from_slice(&[0x01, 0x83]);
        bytes.extend_from_slice(&mask);
        bytes.extend(b"Hel".iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        bytes.extend_from_slice(&[0x80, 0x82]);
        bytes.extend_from_slice(&mask);
        bytes.extend(b"lo".iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let mut r = &bytes[..];
        assert_eq!(
            read_message(&mut r).await.unwrap(),
            Message::Text("Hello".into())
        );
        assert_eq!(
            read_message(&mut r).await.unwrap(),
            Message::Text("Hello".into())
        );
    }

    #[tokio::test]
    async fn unmasked_client_frames_are_refused() {
        // RFC 6455 section 5.7: unmasked "Hello" is valid only from a server.
        let bytes = [0x81u8, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        assert!(read_message(&mut &bytes[..]).await.is_err());
    }

    #[test]
    fn server_frames_use_extended_lengths() {
        assert_eq!(frame(OP_TEXT, b"Hello")[..2], [0x81, 0x05]);
        let f = frame(OP_TEXT, &[b'x'; 300]);
        assert_eq!(f[..4], [0x81, 126, 0x01, 0x2C]);
        assert_eq!(f.len(), 304);
    }
}
