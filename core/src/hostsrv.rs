//! The IBM i host servers that IBM i Access Client Solutions (ACS) and
//! the JDBC driver of the IBM Toolbox for Java (JTOpen, jt400) connect
//! to: the server port mapper (449), the sign-on server (as-signon,
//! 8476) and the database server (as-database, 8471).
//!
//! Their data streams are the ones JTOpen's client side builds and reads
//! (open source, IBM Public License 1.0: ClientAccessDataStream,
//! AS400PortMapDS, SignonExchangeAttributeReq/Rep, SignonInfoReq/Rep,
//! AS400XChgRandSeedDS, AS400StrSvrDS, DBBaseRequestDS, DBBaseReplyDS,
//! DBReplyServerAttributes); the server side here is as4tainr's own.
//!
//! Every data stream starts with a 20-byte header: its length (4), a
//! header ID (2), the server ID (2: 0xE009 sign-on, 0xE004 database), a
//! client/server instance (4), a correlation ID (4, a reply carries its
//! request's), the template length (2) and the request or reply ID (2).
//! Then the template, then parameters as LL (4) CP (2) data.
//!
//! Passwords: the server answers password level 2 (SHA-1 password
//! substitutes; users.rs `host_signon`), whatever QPWDLVL says
//! (docs/fidelity-gaps.md).

use anyhow::{bail, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The well-known ports of the host servers (JTOpen PortMapper's
/// defaults; the port mapper answers them).
pub const SERVICES: &[(&str, u16)] = &[
    ("as-central", 8470),
    ("as-database", 8471),
    ("as-dtaq", 8472),
    ("as-file", 8473),
    ("as-netprt", 8474),
    ("as-rmtcmd", 8475),
    ("as-signon", 8476),
    ("as-svrmap", 449),
];

/// The release the servers say they are: V7R5M0 (as the rest of
/// as4tainr shows: report headers, DSPMOD).
pub const VRM: u32 = 0x0007_0500;

/// The sign-on server's level (JTOpen sends the info request's 0x1128
/// parameter from level 5, additional authentication from 18).
const SIGNON_LEVEL: u16 = 10;

/// The password level the servers announce: SHA-1 substitutes.
const PASSWORD_LEVEL: u8 = 2;

/// Serve the port mapper and the servers that are implemented on `ip`
/// (each on its well-known port) until the process ends. A port that
/// cannot be bound (449 without the privilege) is reported and skipped.
pub async fn run(database_url: String, ip: String) -> Result<()> {
    let client = crate::runtime::connect_db(&database_url).await?;
    crate::catalog::init(&client).await?;
    drop(client);
    let mut tasks = Vec::new();
    for (name, port) in [
        ("as-svrmap", 449u16),
        ("as-signon", 8476),
        ("as-database", 8471),
        ("as-rmtcmd", 8475),
        ("as-dtaq", 8472),
        ("as-file", 8473),
    ] {
        let addr = format!("{ip}:{port}");
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("as4tainr: host server {name} not started: {addr}: {e}");
                continue;
            }
        };
        println!("as4tainr: host server {name} listening on {addr}");
        let url = database_url.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    continue;
                };
                let url = url.clone();
                tokio::spawn(async move {
                    let r = match name {
                        "as-svrmap" => port_mapper(sock).await,
                        "as-signon" => signon(&url, sock).await,
                        "as-rmtcmd" => crate::hostcmd::serve(&url, sock).await,
                        "as-dtaq" => crate::hostcmd::serve_as(&url, sock, 0xE007).await,
                        "as-file" => crate::hostcmd::serve_as(&url, sock, 0xE002).await,
                        _ => crate::hostdb::serve(&url, sock).await,
                    };
                    if let Err(e) = r {
                        eprintln!("as4tainr: {name} connection ended: {e:#}");
                    }
                });
            }
        }));
    }
    if tasks.is_empty() {
        bail!("no host server could be started on {ip}");
    }
    for t in tasks {
        t.await?;
    }
    Ok(())
}

/// The port mapper: the client sends a service name (ASCII, no length)
/// and reads 5 bytes: '+' and the port (4 bytes), or '-' (JTOpen
/// AS400PortMapDS, AS400PortMapReplyDS). A name ending in "-s" asks for
/// the TLS port, which as4tainr does not serve.
pub async fn port_mapper(mut sock: TcpStream) -> Result<()> {
    let mut buf = [0u8; 64];
    let n = sock.read(&mut buf).await?;
    let name = String::from_utf8_lossy(&buf[..n])
        .trim()
        .to_ascii_lowercase();
    let mut reply = vec![b'-', 0, 0, 0, 0];
    if let Some((_, port)) = SERVICES.iter().find(|(s, _)| *s == name) {
        reply[0] = b'+';
        reply[1..].copy_from_slice(&u32::from(*port).to_be_bytes());
    }
    sock.write_all(&reply).await?;
    sock.flush().await?;
    Ok(())
}

/// A data stream as read: the whole of it, header first.
pub struct Ds(pub Vec<u8>);

impl Ds {
    pub fn u16(&self, at: usize) -> u16 {
        self.0
            .get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .unwrap_or(0)
    }
    pub fn u32(&self, at: usize) -> u32 {
        self.0
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    }
    pub fn server_id(&self) -> u16 {
        self.u16(6)
    }
    pub fn correlation(&self) -> u32 {
        self.u32(12)
    }
    pub fn template_len(&self) -> usize {
        self.u16(16) as usize
    }
    pub fn req_id(&self) -> u16 {
        self.u16(18)
    }
    /// The LL/CP parameters from `from`: (code point, data).
    pub fn params(&self, from: usize) -> Vec<(u16, &[u8])> {
        let mut out = Vec::new();
        let mut at = from;
        while at + 6 <= self.0.len() {
            let ll = self.u32(at) as usize;
            if ll < 6 || at + ll > self.0.len() {
                break;
            }
            out.push((self.u16(at + 4), &self.0[at + 6..at + ll]));
            at += ll;
        }
        out
    }
}

/// Read one data stream (None at the end of the connection).
pub async fn read_ds(sock: &mut TcpStream) -> Result<Option<Ds>> {
    let mut len = [0u8; 4];
    match sock.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    if !(20..=64 << 20).contains(&n) {
        bail!("a data stream of {n} bytes is not valid");
    }
    let mut data = vec![0u8; n];
    data[..4].copy_from_slice(&len);
    sock.read_exact(&mut data[4..]).await?;
    Ok(Some(Ds(data)))
}

/// A reply data stream: header (with the request's correlation ID),
/// template, parameters.
pub fn reply(
    header_byte4: u8,
    header_byte5: u8,
    server_id: u16,
    correlation: u32,
    reply_id: u16,
    template: &[u8],
    params: &[u8],
) -> Vec<u8> {
    let len = 20 + template.len() + params.len();
    let mut v = Vec::with_capacity(len);
    v.extend((len as u32).to_be_bytes());
    v.push(header_byte4);
    v.push(header_byte5);
    v.extend(server_id.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(correlation.to_be_bytes());
    v.extend((template.len() as u16).to_be_bytes());
    v.extend(reply_id.to_be_bytes());
    v.extend(template);
    v.extend(params);
    v
}

/// An LL/CP parameter.
pub fn param(cp: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + data.len());
    v.extend(((6 + data.len()) as u32).to_be_bytes());
    v.extend(cp.to_be_bytes());
    v.extend(data);
    v
}

/// Text in CCSID 37 (EBCDIC), blank-padded to `n` bytes.
pub fn ebcdic(s: &str, n: usize) -> Vec<u8> {
    let mut b = crate::ifs::codec::bytes(37, s);
    b.resize(n, 0x40);
    b
}

/// EBCDIC (CCSID 37) bytes as text, trailing blanks dropped.
pub fn text37(b: &[u8]) -> String {
    crate::ifs::codec::text(37, b).trim_end().to_string()
}

/// A random 8-byte seed.
pub fn seed() -> [u8; 8] {
    let mut s = [0u8; 8];
    let r: u64 = rand::random();
    s.copy_from_slice(&r.to_be_bytes());
    s
}

/// The job a server connection says it is (CP 0x111F): its CCSID, then
/// "number/user/name" in EBCDIC.
pub fn job_name_param(name: &str, user: &str) -> Vec<u8> {
    // The qualified job name as number/user/name (JTOpen
    // RemoteCommandImplRemote getJobInfo splits the job string at the
    // slashes: number, user, name).
    let number = format!("{:06}", std::process::id() % 1_000_000);
    let mut d = 37u32.to_be_bytes().to_vec();
    d.extend(crate::ifs::codec::bytes(
        37,
        &format!("{number}/{user}/{name}"),
    ));
    param(0x111F, &d)
}

/// The sign-on server: exchange of attributes (0x7003: server version,
/// level, seed, password level) and sign-on information (0x7004: the
/// password substitute checked, dates and the server's CCSID back).
pub async fn signon(url: &str, mut sock: TcpStream) -> Result<()> {
    let client = crate::runtime::connect_db(url).await?;
    let server_seed = seed();
    let mut client_seed: Vec<u8> = Vec::new();
    while let Some(ds) = read_ds(&mut sock).await? {
        let corr = ds.correlation();
        match ds.req_id() {
            0x7003 => {
                for (cp, d) in ds.params(20) {
                    if cp == 0x1103 {
                        client_seed = d.to_vec();
                    }
                }
                let mut p = param(0x1101, &VRM.to_be_bytes());
                p.extend(param(0x1102, &SIGNON_LEVEL.to_be_bytes()));
                p.extend(param(0x1103, &server_seed));
                p.extend(param(0x1119, &[PASSWORD_LEVEL]));
                p.extend(job_name_param("QZSOSIGN", "QUSER"));
                let r = reply(0, 0, 0xE009, corr, 0xF003, &0u32.to_be_bytes(), &p);
                sock.write_all(&r).await?;
            }
            0x7004 => {
                let tl = ds.template_len();
                let mut user = String::new();
                let mut substitute: Vec<u8> = Vec::new();
                for (cp, d) in ds.params(20 + tl) {
                    match cp {
                        0x1104 => user = text37(d),
                        0x1105 => substitute = d.to_vec(),
                        _ => {}
                    }
                }
                let rc = match crate::users::host_signon(
                    &client,
                    &user,
                    &client_seed,
                    &server_seed,
                    &substitute,
                )
                .await
                {
                    Ok(_) => 0u32,
                    Err(rc) => rc,
                };
                let mut p = Vec::new();
                if rc == 0 {
                    let now = signon_date();
                    p.extend(param(0x1106, &now));
                    p.extend(param(0x1107, &now));
                    p.extend(param(0x1108, &date(2099, 12, 31)));
                    p.extend(param(0x1114, &37u32.to_be_bytes()));
                }
                let r = reply(0, 0, 0xE009, corr, 0xF004, &rc.to_be_bytes(), &p);
                sock.write_all(&r).await?;
            }
            other => {
                // Not done here (change password, tokens): "request not
                // valid" (0x0001000B, JTOpen SIGNON_REQUEST_NOT_VALID).
                let r = reply(
                    0,
                    0,
                    0xE009,
                    corr,
                    other | 0x8000,
                    &0x0001_000Bu32.to_be_bytes(),
                    &[],
                );
                sock.write_all(&r).await?;
            }
        }
        sock.flush().await?;
    }
    Ok(())
}

/// A date and time as the sign-on server sends them: year (2), month,
/// day, hour, minute, second, and a reserved byte.
fn date(y: u16, mo: u8, d: u8) -> Vec<u8> {
    let mut v = y.to_be_bytes().to_vec();
    v.extend([mo, d, 0, 0, 0, 0]);
    v
}

fn signon_date() -> Vec<u8> {
    let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::now();
    let mut v = (y as u16).to_be_bytes().to_vec();
    v.extend([mo as u8, d as u8, h as u8, mi as u8, s as u8, 0]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_as_jtopen_reads_them() {
        // SignonExchangeAttributeRep: return code at 20, server version at
        // 30 (CP 0x1101 at 24), level at 40 (CP 0x1102 at 34).
        let mut p = param(0x1101, &VRM.to_be_bytes());
        p.extend(param(0x1102, &10u16.to_be_bytes()));
        let r = Ds(reply(0, 0, 0xE009, 7, 0xF003, &0u32.to_be_bytes(), &p));
        assert_eq!(r.u32(0) as usize, r.0.len());
        assert_eq!(r.u32(12), 7);
        assert_eq!(r.u32(20), 0);
        assert_eq!(r.u32(30), 0x0007_0500);
        assert_eq!(r.u16(40), 10);
        assert_eq!(r.params(24).len(), 2);
    }

    #[test]
    fn sha_password_token() {
        // JTOpen generateShaToken: SHA-1 of the user ID (blank-padded to
        // 10) and the password, both UTF-16BE; 20 bytes.
        let t = crate::users::host_token("sshu", "PW123");
        assert_eq!(t.len(), 20);
        assert_eq!(t, crate::users::host_token("SSHU      ", "PW123  "));
        assert_ne!(t, crate::users::host_token("SSHU", "pw123"));
    }
}
