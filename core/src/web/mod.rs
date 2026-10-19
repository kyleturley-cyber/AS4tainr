//! Browser 5250 terminal: `as4tainr tn5250 --web-listen ADDR` serves a web
//! page that is a 5250 display station, with a row of clickable function
//! keys above the screen.
//!
//! Each browser tab opens a WebSocket. The server side of that socket is a
//! 5250 terminal ([`display::Display`]) connected to this process's own
//! TN5250 listener, so a browser session is a normal 5250 session (its own
//! QPADEVnnnn device and job) and goes through exactly the same sign-on and
//! screens as tn5250 or ACS. The browser only draws the screen and edits
//! fields; the 5250 data stream stays on the server.
//!
//! Protocol (JSON text messages):
//! - server -> browser: `{"t":"screen", ...}` ([`display::ScreenMsg`]),
//!   `{"t":"closed","reason":...}` when the 5250 session ends.
//! - browser -> server: `{"t":"aid","aid":"F3","cursor":N,"fields":[{"p":N,"v":"..."}]}`
//!   with only the fields the operator changed.

pub mod display;
pub mod ws;

use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::tn5250::telnet::{self, Event, Parser};

const INDEX_HTML: &str = include_str!("static/index.html");
const TERMINAL_JS: &str = include_str!("static/terminal.js");
const TERMINAL_CSS: &str = include_str!("static/terminal.css");

/// The terminal type the browser terminal reports: a 24x80 color display.
pub const TERMINAL_TYPE: &str = "IBM-3179-2";

const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// Serve the web terminal on `listener`. Every WebSocket connects to the
/// TN5250 server at `backend` (normally this process's own listener).
pub async fn serve(listener: TcpListener, backend: SocketAddr) -> Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, backend).await {
                eprintln!("as4tainr: web terminal {peer}: {e:#}");
            }
        });
    }
}

/// The address to reach a listener bound to `bound` from this process.
pub fn loopback_for(bound: SocketAddr) -> SocketAddr {
    let mut a = bound;
    if a.ip().is_unspecified() {
        a.set_ip(match a {
            SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    a
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn parse_request(head: &str) -> Result<Request> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target)) = (first.next(), first.next()) else {
        bail!("bad request line");
    };
    let path = target.split('?').next().unwrap_or("/").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(Request {
        method: method.to_string(),
        path,
        headers,
    })
}

async fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST_HEAD {
            bail!("request header too large");
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            bail!("connection closed before the request was complete");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Host header value safe to echo into a header (host, port, IPv6 brackets).
fn clean_host(h: Option<&str>) -> Option<String> {
    let h = h?.trim();
    (!h.is_empty()
        && h.len() < 256
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:[]_".contains(c)))
    .then(|| h.to_string())
}

/// A WebSocket may only be opened by the page this server served: the
/// Origin's host must be the Host the browser connected to. This stops other
/// web sites from opening 5250 sessions through the operator's browser.
fn origin_allowed(origin: Option<&str>, host: Option<&str>) -> bool {
    match (origin, host) {
        (None, _) => true, // not a browser (curl, test tools)
        (Some(o), Some(h)) => o
            .split_once("://")
            .map(|(_, rest)| rest.trim_end_matches('/'))
            .is_some_and(|oh| oh.eq_ignore_ascii_case(h.trim())),
        (Some(_), None) => false,
    }
}

async fn respond(
    stream: &mut TcpStream,
    head_only: bool,
    status: &str,
    ctype: &str,
    extra: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    if !head_only {
        stream.write_all(body).await?;
    }
    stream.shutdown().await.ok();
    Ok(())
}

async fn handle(mut stream: TcpStream, peer: SocketAddr, backend: SocketAddr) -> Result<()> {
    let head = tokio::time::timeout(Duration::from_secs(10), read_head(&mut stream))
        .await
        .context("timed out reading the request")??;
    let req = parse_request(&head)?;
    // HEAD is GET without the body (health checks use it).
    let head_only = req.method == "HEAD";
    if req.method != "GET" && !head_only {
        return respond(
            &mut stream,
            head_only,
            "405 Method Not Allowed",
            "text/plain",
            "Allow: GET, HEAD\r\n",
            b"GET only\n",
        )
        .await;
    }
    let host = clean_host(req.header("Host"));
    match req.path.as_str() {
        "/" | "/index.html" => {
            // Scripts and styles come only from this server; the socket only
            // to the host the page was loaded from.
            let csp = match &host {
                Some(h) => format!("default-src 'self'; connect-src 'self' ws://{h} wss://{h}; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
                None => "default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".to_string(),
            };
            respond(
                &mut stream,
                head_only,
                "200 OK",
                "text/html; charset=utf-8",
                &format!("Content-Security-Policy: {csp}\r\nX-Frame-Options: DENY\r\n"),
                INDEX_HTML.as_bytes(),
            )
            .await
        }
        "/terminal.js" => {
            respond(
                &mut stream,
                head_only,
                "200 OK",
                "text/javascript; charset=utf-8",
                "",
                TERMINAL_JS.as_bytes(),
            )
            .await
        }
        "/terminal.css" => {
            respond(
                &mut stream,
                head_only,
                "200 OK",
                "text/css; charset=utf-8",
                "",
                TERMINAL_CSS.as_bytes(),
            )
            .await
        }
        "/healthz" => respond(&mut stream, head_only, "200 OK", "text/plain", "", b"ok\n").await,
        "/ws" => {
            let upgrade = req
                .header("Upgrade")
                .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
            let Some(key) = req.header("Sec-WebSocket-Key").filter(|_| upgrade) else {
                return respond(
                    &mut stream,
                    head_only,
                    "400 Bad Request",
                    "text/plain",
                    "",
                    b"WebSocket upgrade expected\n",
                )
                .await;
            };
            if !origin_allowed(req.header("Origin"), req.header("Host")) {
                eprintln!(
                    "as4tainr: web terminal {peer}: refused a WebSocket from origin {:?}",
                    req.header("Origin")
                );
                return respond(
                    &mut stream,
                    head_only,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    b"cross-origin WebSocket refused\n",
                )
                .await;
            }
            let accept = ws::accept_key(key);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await?;
            session(stream, peer, backend).await
        }
        _ => {
            respond(
                &mut stream,
                head_only,
                "404 Not Found",
                "text/plain",
                "",
                b"not found\n",
            )
            .await
        }
    }
}

/// Answer the TN5250 server's telnet negotiation as a 5250 display
/// (RFC 1205 section 2 and RFC 2877): terminal type, binary and end of record.
fn negotiate_reply(cmd: u8, opt: u8) -> Vec<u8> {
    use telnet::*;
    match (cmd, opt) {
        (DO, OPT_TTYPE | OPT_EOR | OPT_BINARY) => vec![IAC, WILL, opt],
        (WILL, OPT_EOR | OPT_BINARY) => vec![IAC, DO, opt],
        (DO, _) => vec![IAC, WONT, opt],
        (WILL, _) => vec![IAC, DONT, opt],
        _ => vec![],
    }
}

fn terminal_type_reply() -> Vec<u8> {
    use telnet::*;
    let mut v = vec![IAC, SB, OPT_TTYPE, 0]; // IS
    v.extend_from_slice(TERMINAL_TYPE.as_bytes());
    v.extend_from_slice(&[IAC, SE]);
    v
}

enum FromBrowser {
    Text(String),
    Ping(Vec<u8>),
    Closed,
}

/// One browser terminal: the WebSocket on one side, a TN5250 connection to
/// the server on the other.
async fn session(stream: TcpStream, peer: SocketAddr, backend: SocketAddr) -> Result<()> {
    let (mut ws_r, mut ws_w) = stream.into_split();
    let host = TcpStream::connect(backend)
        .await
        .with_context(|| format!("connecting to the 5250 server at {backend}"))?;
    host.set_nodelay(true).ok();
    let (mut host_r, mut host_w) = host.into_split();
    eprintln!("as4tainr: web terminal {peer} connected");

    // WebSocket reads are not cancel safe, so they run in their own task.
    let (tx, mut rx) = mpsc::channel::<FromBrowser>(16);
    let reader = tokio::spawn(async move {
        loop {
            let m = match ws::read_message(&mut ws_r).await {
                Ok(ws::Message::Text(t)) => FromBrowser::Text(t),
                Ok(ws::Message::Ping(p)) => FromBrowser::Ping(p),
                Ok(ws::Message::Pong | ws::Message::Binary(_)) => continue,
                Ok(ws::Message::Close) | Err(_) => FromBrowser::Closed,
            };
            let closed = matches!(m, FromBrowser::Closed);
            if tx.send(m).await.is_err() || closed {
                return;
            }
        }
    });

    let mut display = display::Display::new();
    let mut parser = Parser::default();
    let mut buf = vec![0u8; 8192];
    let reason = loop {
        tokio::select! {
            n = host_r.read(&mut buf) => {
                let n = n.unwrap_or(0);
                if n == 0 {
                    break "The 5250 session ended.".to_string();
                }
                let mut events = Vec::new();
                parser.feed(&buf[..n], &mut events);
                let mut redraw = false;
                for ev in events {
                    match ev {
                        Event::Command(cmd, opt) => {
                            let r = negotiate_reply(cmd, opt);
                            if !r.is_empty() {
                                host_w.write_all(&r).await?;
                            }
                        }
                        Event::Sub(telnet::OPT_TTYPE, d) if d.first() == Some(&1) => {
                            host_w.write_all(&terminal_type_reply()).await?;
                        }
                        Event::Sub(..) => {}
                        Event::Record(r) if r.is_empty() => {}
                        Event::Record(r) => {
                            let data = telnet::record_data(&r)?;
                            if let Some(skipped) = display.apply(data) {
                                eprintln!("as4tainr: web terminal {peer}: {skipped}");
                            }
                            redraw = true;
                        }
                    }
                }
                if redraw {
                    let msg = serde_json::to_string(&display.screen_msg())?;
                    ws::send_text(&mut ws_w, &msg).await?;
                }
            }
            m = rx.recv() => match m {
                None | Some(FromBrowser::Closed) => break String::new(),
                Some(FromBrowser::Ping(p)) => ws::send_pong(&mut ws_w, &p).await?,
                Some(FromBrowser::Text(t)) => {
                    let v: serde_json::Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if v.get("t").and_then(|x| x.as_str()) != Some("aid") {
                        continue;
                    }
                    let Ok(aid) = serde_json::from_value::<display::AidMsg>(v) else {
                        continue;
                    };
                    match display.input(&aid) {
                        Some(rec) => {
                            host_w
                                .write_all(&telnet::frame_record(telnet::opcode::PUT_GET, &rec))
                                .await?;
                        }
                        // Keyboard locked: tell the browser the screen as it is.
                        None => {
                            let msg = serde_json::to_string(&display.screen_msg())?;
                            ws::send_text(&mut ws_w, &msg).await?;
                        }
                    }
                }
            }
        }
    };
    reader.abort();
    if !reason.is_empty() {
        let msg = serde_json::json!({"t": "closed", "reason": reason}).to_string();
        ws::send_text(&mut ws_w, &msg).await.ok();
        ws::send_close(&mut ws_w).await.ok();
    }
    eprintln!("as4tainr: web terminal {peer} disconnected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_must_match_host() {
        assert!(origin_allowed(
            Some("http://localhost:5250"),
            Some("localhost:5250")
        ));
        assert!(origin_allowed(
            Some("https://as4tainr.example.com"),
            Some("AS4TAINR.example.com")
        ));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("localhost:5250")
        ));
        assert!(!origin_allowed(
            Some("http://localhost:9999"),
            Some("localhost:5250")
        ));
        assert!(!origin_allowed(Some("null"), Some("localhost:5250")));
        assert!(origin_allowed(None, Some("localhost:5250")));
    }

    #[test]
    fn host_header_is_only_echoed_when_plain() {
        assert_eq!(
            clean_host(Some("localhost:5250")).as_deref(),
            Some("localhost:5250")
        );
        assert_eq!(
            clean_host(Some("[::1]:5250")).as_deref(),
            Some("[::1]:5250")
        );
        assert_eq!(clean_host(Some("x; script-src *")), None);
        assert_eq!(clean_host(Some("a\r\nSet-Cookie: x")), None);
    }

    #[test]
    fn loopback_for_wildcard_listeners() {
        assert_eq!(
            loopback_for("0.0.0.0:23".parse().unwrap()),
            "127.0.0.1:23".parse().unwrap()
        );
        assert_eq!(
            loopback_for("10.1.2.3:2323".parse().unwrap()),
            "10.1.2.3:2323".parse().unwrap()
        );
    }

    #[test]
    fn telnet_answers_as_a_5250_display() {
        use telnet::*;
        // RFC 1205: the client agrees to TERMINAL-TYPE, EOR and BINARY ...
        assert_eq!(negotiate_reply(DO, OPT_TTYPE), vec![IAC, WILL, OPT_TTYPE]);
        assert_eq!(negotiate_reply(WILL, OPT_EOR), vec![IAC, DO, OPT_EOR]);
        assert_eq!(negotiate_reply(DO, OPT_BINARY), vec![IAC, WILL, OPT_BINARY]);
        // ... and refuses what it does not do (ECHO = 1).
        assert_eq!(negotiate_reply(DO, 1), vec![IAC, WONT, 1]);
        // IAC SB TERMINAL-TYPE IS "IBM-3179-2" IAC SE.
        let tt = terminal_type_reply();
        assert_eq!(&tt[..4], &[IAC, SB, OPT_TTYPE, 0]);
        assert_eq!(&tt[4..14], b"IBM-3179-2");
        assert_eq!(&tt[14..], &[IAC, SE]);
    }

    #[test]
    fn request_parsing() {
        let r = parse_request("GET /ws?x=1 HTTP/1.1\r\nHost: a:1\r\nupgrade: WebSocket").unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/ws");
        assert_eq!(r.header("Upgrade"), Some("WebSocket"));
        assert_eq!(r.header("host"), Some("a:1"));
    }
}
