//! The remote command host server (as-rmtcmd, port 8475, server ID
//! 0xE008): the server the IBM Toolbox for Java's CommandCall (and ACS's
//! commands) run CL commands through (hostsrv.rs for the common data
//! stream).
//!
//! A connection starts as the database server's does: an exchange of
//! random seeds (0x7001) and a start-server request (0x7002, the user and
//! the password substitute). Then (JTOpen RCExchangeAttributesRequest/
//! ReplyDataStream, RCRunCommandRequest/ReplyDataStream,
//! RemoteCommandImplRemote):
//!
//! - exchange attributes (0x1001: the client's CCSID, NLV and version;
//!   reply 0x8001: return code at 20, the job's CCSID at 22, the NLV at
//!   26, the version at 30, the data stream level at 34);
//! - run command (0x1002: the message option at 20, then CP 0x1104 with
//!   the command's CCSID and text, or CP 0x1101 with the text in the
//!   job's CCSID; reply 0x8002: return code at 20, 0x0400 when the
//!   command failed, the number of messages at 22, then the messages
//!   from 24, each CP 0x1106: text and substitution data CCSIDs,
//!   severity, type, ID, message file and library, text, substitution
//!   data, help).
//!
//! The command runs in the connection's job (one for the connection, as
//! the server job QZRCSRVS of IBM i serves one client). Program calls
//! (0x1003) are not done yet: return code 0x0500 (docs/fidelity-gaps.md).

use crate::hostsrv::{ebcdic, job_name_param, param, read_ds, reply, seed, text37, Ds};
use anyhow::Result;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// The data stream level the server answers with: 10, the command in
/// CCSID 1200 (CP 0x1104).
const DS_LEVEL: u16 = 10;

/// The job's CCSID: the messages' text.
const JOB_CCSID: u32 = 37;

/// Serve one remote command connection.
pub async fn serve(url: &str, mut sock: TcpStream) -> Result<()> {
    let client = crate::runtime::connect_db(url).await?;
    let env = crate::commands::Environment {
        database_url: url.to_string(),
    };
    let server_seed = seed();
    let mut client_seed: Vec<u8> = Vec::new();
    let mut server: Option<crate::mapepire::Server> = None;
    while let Some(ds) = read_ds(&mut sock).await? {
        let corr = ds.correlation();
        let out = match ds.req_id() {
            0x7001 => {
                client_seed = ds.0.get(20..28).unwrap_or_default().to_vec();
                let mut t = 0u32.to_be_bytes().to_vec();
                t.extend(server_seed);
                // Byte 5 of the header: the password level.
                reply(0, 2, 0xE008, corr, 0xF001, &t, &[])
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
                        let curlib = (p.curlib != "*CRTDFT").then(|| p.curlib.clone());
                        server = Some(crate::mapepire::Server::new(crate::commands::Job {
                            client: &client,
                            env: &env,
                            user: p.name,
                            curlib,
                            libl: crate::commands::USER_LIBRARY_LIST
                                .iter()
                                .map(|l| l.to_string())
                                .collect(),
                            display: None,
                            id: None,
                            calls: Default::default(),
                        }));
                        0u32
                    }
                    Err(rc) => rc,
                };
                let mut p = Vec::new();
                if rc == 0 {
                    let mut u = 37u32.to_be_bytes().to_vec();
                    u.extend(ebcdic(&name.to_ascii_uppercase(), 10));
                    p.extend(param(0x1104, &u));
                    p.extend(job_name_param("QZRCSRVS", "QUSER"));
                }
                reply(0, 0, 0xE008, corr, 0xF002, &rc.to_be_bytes(), &p)
            }
            _ if server.is_none() => break,
            0x1001 => exchange_attributes(corr),
            0x1002 => {
                let srv = server.as_mut().expect("signed on");
                run_command(srv, &ds).await
            }
            // Call program: not done yet (0x0500, an error resolving the
            // program).
            0x1003 => command_reply(0x8003, corr, 0x0500, &[]),
            // End: no reply.
            0x1004 => break,
            _ => command_reply(0x8002, corr, 0x0205, &[]),
        };
        sock.write_all(&out).await?;
        sock.flush().await?;
    }
    Ok(())
}

/// Reply 0x8001: return code 0, the job's CCSID, its NLV (2924), version
/// 1, the data stream level.
fn exchange_attributes(corr: u32) -> Vec<u8> {
    let mut t = 0u16.to_be_bytes().to_vec();
    t.extend(JOB_CCSID.to_be_bytes());
    t.extend(ebcdic("2924", 4));
    t.extend(1u32.to_be_bytes());
    t.extend(DS_LEVEL.to_be_bytes());
    reply(0, 0, 0xE008, corr, 0x8001, &t, &[])
}

/// A reply with its return code and messages.
fn command_reply(id: u16, corr: u32, rc: u16, messages: &[Vec<u8>]) -> Vec<u8> {
    let mut t = rc.to_be_bytes().to_vec();
    t.extend((messages.len() as u16).to_be_bytes());
    let p: Vec<u8> = messages.concat();
    reply(0, 0, 0xE008, corr, id, &t, &p)
}

/// The command of a run command request: CP 0x1104 (CCSID, text) or
/// 0x1101 (text in the job's CCSID), after the 1-byte template.
fn command_text(ds: &Ds) -> Option<String> {
    for (cp, d) in ds.params(21) {
        match cp {
            0x1104 if d.len() >= 4 => {
                let ccsid = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
                return Some(text_in(ccsid, &d[4..]));
            }
            0x1101 => return Some(text_in(JOB_CCSID, d)),
            _ => {}
        }
    }
    None
}

fn text_in(ccsid: u32, b: &[u8]) -> String {
    match ccsid {
        1200 | 13488 => {
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        1208 => String::from_utf8_lossy(b).into_owned(),
        c => crate::ifs::codec::text(c as i32, b),
    }
}

/// Run a command: return code 0 with its completion messages, or
/// 0x0400 with the messages it sent (the escape message last).
async fn run_command(srv: &mut crate::mapepire::Server<'_>, ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let Some(cmd) = command_text(ds) else {
        return command_reply(0x8002, corr, 0x0301, &[]);
    };
    let (ok, messages) = srv.run_command(cmd.trim()).await;
    let list: Vec<Vec<u8>> = messages.iter().map(message).collect();
    command_reply(0x8002, corr, if ok { 0 } else { 0x0400 }, &list)
}

/// A message (CP 0x1106, JTOpen AS400ImplRemote parseMessage): the text
/// CCSID and the substitution data CCSID (4 each), the severity (2), then
/// each as a 4-byte length and its bytes: the type (two digits: 01
/// completion, 02 diagnostic, 04 informational, 14 notify, 15 escape),
/// the ID, the message file, its library, the text, the substitution
/// data (none) and the help (none).
fn message(m: &crate::mapepire::ClMessage) -> Vec<u8> {
    let field = |d: &mut Vec<u8>, b: &[u8]| {
        d.extend((b.len() as u32).to_be_bytes());
        d.extend(b);
    };
    let mut d = JOB_CCSID.to_be_bytes().to_vec();
    d.extend(JOB_CCSID.to_be_bytes());
    d.extend((m.severity as u16).to_be_bytes());
    let ty = match m.kind.as_str() {
        "DIAGNOSTIC" => "02",
        "INFORMATIONAL" => "04",
        "NOTIFY" => "14",
        "ESCAPE" => "15",
        _ => "01",
    };
    field(&mut d, &ebcdic(ty, 2));
    field(&mut d, &ebcdic(&m.id, 7));
    field(&mut d, &ebcdic(m.file.unwrap_or(""), 10));
    field(&mut d, &ebcdic(m.library.unwrap_or(""), 10));
    field(&mut d, &crate::ifs::codec::bytes(JOB_CCSID as i32, &m.text));
    field(&mut d, &[]);
    field(&mut d, &[]);
    param(0x1106, &d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_attributes_where_jtopen_reads_them() {
        // RCExchangeAttributesReplyDataStream: return code at 20, CCSID
        // at 22, data stream level at 34.
        let r = exchange_attributes(5);
        assert_eq!(u16::from_be_bytes([r[18], r[19]]), 0x8001);
        assert_eq!(u16::from_be_bytes([r[20], r[21]]), 0);
        assert_eq!(u32::from_be_bytes([r[22], r[23], r[24], r[25]]), 37);
        assert_eq!(u16::from_be_bytes([r[34], r[35]]), 10);
    }

    #[test]
    fn a_message_as_jtopen_parses_it() {
        let m = crate::mapepire::ClMessage {
            id: "CPF2105".into(),
            text: "Object X in Y type *FILE not found.".into(),
            kind: "ESCAPE".into(),
            severity: 40,
            file: Some("QCPFMSG"),
            library: Some("QSYS"),
        };
        let p = message(&m);
        // LL, CP 0x1106 (byte 5 is 0x06), then the fields.
        assert_eq!(&p[4..6], &[0x11, 0x06]);
        let d = &p[6..];
        assert_eq!(u16::from_be_bytes([d[8], d[9]]), 40);
        assert_eq!(u32::from_be_bytes([d[10], d[11], d[12], d[13]]), 2);
        assert_eq!(&d[14..16], &ebcdic("15", 2)[..]);
        assert_eq!(u32::from_be_bytes([d[16], d[17], d[18], d[19]]), 7);
        assert_eq!(text37(&d[20..27]), "CPF2105");
        let r = command_reply(0x8002, 1, 0x0400, std::slice::from_ref(&p));
        assert_eq!(u16::from_be_bytes([r[20], r[21]]), 0x0400);
        assert_eq!(u16::from_be_bytes([r[22], r[23]]), 1);
        assert_eq!(&r[24..], &p[..]);
    }
}
