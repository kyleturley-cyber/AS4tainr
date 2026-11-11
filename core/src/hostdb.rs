//! The database host server (as-database, port 8471): the server JTOpen's
//! JDBC driver (and so ACS's Run SQL Scripts) talks to (hostsrv.rs for
//! the common data stream).
//!
//! A connection starts with an exchange of random seeds (0x7001: the
//! server's seed and password level back) and a start-server request
//! (0x7002: the user and the password substitute; the user and the job
//! back). Then database requests: a header, a 20-byte template (the
//! operation result bitmap at 20, the return ORS handle at 28, the fill
//! ORS handle at 30, the RPB handle at 34, the parameter marker handle at
//! 36, the parameter count at 38) and LL/CP parameters (JTOpen
//! DBBaseRequestDS). Each is answered with reply 0x2800: the template's
//! error class (34) and return code (36), and the results the bitmap asks
//! for (JTOpen DBBaseReplyDS, DBReplyServerAttributes).
//!
//! Done so far: the set and retrieve attributes requests (0x1F80,
//! 0x1F81) with the server attributes; other requests are answered
//! without an error and without results (docs/fidelity-gaps.md).

use crate::hostsrv::{ebcdic, job_name_param, param, read_ds, reply, seed, text37, Ds};
use anyhow::Result;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// The operation result bitmap's bits (JTOpen DBBaseRequestDS).
const ORS_SERVER_ATTRIBUTES: u32 = 0x0100_0000;

/// The database server's functional level: "V7R5M0" and a level the
/// JDBC driver reads as a number (it uses the server job's identifier
/// from level 5).
const FUNCTIONAL_LEVEL: &str = "V7R5M00005";

/// What a connection has agreed on.
#[derive(Debug, Clone)]
struct Attributes {
    date_format: u16,
    date_separator: u16,
    time_format: u16,
    time_separator: u16,
    decimal_separator: u16,
    naming: u16,
    ignore_decimal_errors: u16,
    commit_level: u16,
    package_size: u16,
    translate: u8,
    default_library: String,
}

impl Default for Attributes {
    fn default() -> Self {
        Attributes {
            date_format: 5,
            date_separator: 0,
            time_format: 2,
            time_separator: 0,
            decimal_separator: 0,
            naming: 0,
            ignore_decimal_errors: 0,
            commit_level: 0,
            package_size: 0,
            translate: 0,
            default_library: String::new(),
        }
    }
}

/// Serve one database server connection.
pub async fn serve(url: &str, mut sock: TcpStream) -> Result<()> {
    let client = crate::runtime::connect_db(url).await?;
    let server_seed = seed();
    let mut client_seed: Vec<u8> = Vec::new();
    let mut user: Option<String> = None;
    let mut attrs = Attributes::default();
    while let Some(ds) = read_ds(&mut sock).await? {
        let corr = ds.correlation();
        let out = match ds.req_id() {
            0x7001 => {
                client_seed = ds.0.get(20..28).unwrap_or_default().to_vec();
                let mut t = 0u32.to_be_bytes().to_vec();
                t.extend(server_seed);
                // Byte 5 of the header: the password level (JTOpen
                // AS400XChgRandSeedReplyDS getServerAttributes).
                reply(0, 2, 0xE004, corr, 0xF001, &t, &[])
            }
            0x7002 => {
                let mut name = String::new();
                let mut substitute: Vec<u8> = Vec::new();
                for (cp, d) in ds.params(22) {
                    match cp {
                        0x1104 => name = text37(d),
                        0x1105 => substitute = d.to_vec(),
                        _ => {}
                    }
                }
                let rc = match crate::users::host_signon(
                    &client,
                    &name,
                    &client_seed,
                    &server_seed,
                    &substitute,
                )
                .await
                {
                    Ok(p) => {
                        user = Some(p.name);
                        0u32
                    }
                    Err(rc) => rc,
                };
                let mut p = Vec::new();
                if rc == 0 {
                    let mut u = 37u32.to_be_bytes().to_vec();
                    u.extend(ebcdic(&name.to_ascii_uppercase(), 10));
                    p.extend(param(0x1104, &u));
                    p.extend(job_name_param("QZDASOINIT", "QUSER"));
                }
                reply(0, 0, 0xE004, corr, 0xF002, &rc.to_be_bytes(), &p)
            }
            _ if user.is_none() => {
                // A database request before the start-server request.
                break;
            }
            0x1F80 | 0x1F81 => {
                if ds.req_id() == 0x1F80 {
                    set_attributes(&ds, &mut attrs);
                }
                let mut p = Vec::new();
                if ds.u32(20) & ORS_SERVER_ATTRIBUTES != 0 {
                    p.extend(server_attributes(&attrs, user.as_deref().unwrap_or("")));
                }
                db_reply(&ds, 0, 0, &p)
            }
            // A function the server does not know: error class 7, return
            // code -201 (JTOpen AS400JDBCConnectionImpl isValid sends
            // function 0x0000 and takes that answer as a live connection).
            0x0000 => db_reply(&ds, 7, -201, &[]),
            _ => db_reply(&ds, 0, 0, &[]),
        };
        sock.write_all(&out).await?;
        sock.flush().await?;
    }
    Ok(())
}

/// The parser options a set attributes request asks for.
fn set_attributes(ds: &Ds, a: &mut Attributes) {
    let tl = ds.template_len();
    for (cp, d) in ds.params(20 + tl) {
        let v16 = || d.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        match cp {
            0x3807 => a.date_format = v16().unwrap_or(a.date_format),
            0x3808 => a.date_separator = v16().unwrap_or(a.date_separator),
            0x3809 => a.time_format = v16().unwrap_or(a.time_format),
            0x380A => a.time_separator = v16().unwrap_or(a.time_separator),
            0x380B => a.decimal_separator = v16().unwrap_or(a.decimal_separator),
            0x380C => a.naming = v16().unwrap_or(a.naming),
            0x380D => a.ignore_decimal_errors = v16().unwrap_or(a.ignore_decimal_errors),
            0x380E => a.commit_level = v16().unwrap_or(a.commit_level),
            0x3806 => a.package_size = v16().unwrap_or(a.package_size),
            0x3805 => a.translate = d.first().copied().unwrap_or(a.translate),
            // Default library: CCSID (2), length (2), the name.
            0x380F if d.len() > 4 => a.default_library = text37(&d[4..]),
            _ => {}
        }
    }
}

/// A database reply (0x2800) to request `ds`: its template with the
/// error class and return code, then the parameters.
fn db_reply(ds: &Ds, error_class: u16, rc: i32, params: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 20];
    // The return data function ID: the request's.
    t[10..12].copy_from_slice(&ds.req_id().to_be_bytes());
    t[14..16].copy_from_slice(&error_class.to_be_bytes());
    t[16..20].copy_from_slice(&rc.to_be_bytes());
    reply(0, 0, 0xE004, ds.correlation(), 0x2800, &t, params)
}

/// The server attributes (CP 0x3804): CCSID 37 for its text, then the
/// layout of JTOpen DBReplyServerAttributes (offsets from the data).
fn server_attributes(a: &Attributes, user: &str) -> Vec<u8> {
    let mut d = Vec::with_capacity(130);
    for v in [
        a.date_format,
        a.date_separator,
        a.time_format,
        a.time_separator,
        a.decimal_separator,
        a.naming,
        a.ignore_decimal_errors,
        a.commit_level,
        a.package_size,
    ] {
        d.extend(v.to_be_bytes());
    }
    d.push(a.translate); // 18
    d.extend(37u16.to_be_bytes()); // 19: server CCSID
    d.extend(0u16.to_be_bytes()); // 21: NLSS value
    d.extend(ebcdic("ENU", 3)); // 23: language ID
    d.extend(ebcdic("*HEX", 10)); // 26: language table
    d.extend(ebcdic("", 10)); // 36: its library
    d.extend(ebcdic("2924", 4)); // 46: language feature code
    d.extend(ebcdic(FUNCTIONAL_LEVEL, 10)); // 50
    d.extend(ebcdic(&crate::report::system_name(), 18)); // 60: RDB name
    let lib = if a.default_library.is_empty() {
        "QGPL"
    } else {
        &a.default_library
    };
    d.extend(ebcdic(lib, 10)); // 78: default SQL library
    let number = format!("{:06}", std::process::id() % 1_000_000);
    let job = format!("{:<10}{:<10}{number}", "QZDASOINIT", "QUSER");
    d.extend(ebcdic(&job, 26)); // 88: job identifier
    d.extend((lib.len() as u16).to_be_bytes()); // 114: default schema length
    d.extend(ebcdic(lib, lib.len())); // 116
    let _ = user;
    let mut v = 37u16.to_be_bytes().to_vec();
    v.extend(d);
    param(0x3804, &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_attributes_where_jtopen_reads_them() {
        // DBReplyServerAttributes: the data starts at the parameter's
        // offset + 8; server CCSID at 19, functional level at 50 (its
        // VRM digits at 51, 53, 55), default library at 78.
        let p = server_attributes(&Attributes::default(), "SSHU");
        let d = &p[8..];
        assert_eq!(u16::from_be_bytes([d[19], d[20]]), 37);
        assert_eq!(text37(&d[50..60]), "V7R5M00005");
        assert_eq!(d[51] & 0x0F, 7);
        assert_eq!(d[53] & 0x0F, 5);
        assert_eq!(text37(&d[78..88]), "QGPL");
        assert_eq!(u16::from_be_bytes([d[114], d[115]]), 4);
    }
}
