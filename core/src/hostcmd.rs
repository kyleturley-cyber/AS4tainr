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
//! - call program (0x1003, JTOpen RCCallProgramRequest/ReplyDataStream:
//!   the program at 20 and its library at 30, 10 bytes each in the job's
//!   CCSID, the message option at 40, the number of parameters at 41,
//!   then from 43 each parameter as CP 0x1103: its length (4), usage
//!   (2) and data; reply 0x8003: return code at 20, then (0) the output
//!   and input-output parameters in order, each CP 0x1103 with its
//!   length, usage and data, or the messages (0x0500 the program was not
//!   found, 0x0501 it failed).
//!
//! Commands and programs run in the connection's job (one for the
//! connection, as the server job QZRCSRVS of IBM i serves one client).

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
pub async fn serve(url: &str, sock: TcpStream) -> Result<()> {
    serve_as(url, sock, 0xE008).await
}

/// Serve one connection of the remote command server (0xE008) or the
/// data queue server (0xE007, hostdtaq.rs): the start of a connection is
/// the same, the job the requests run in too.
pub async fn serve_as(url: &str, mut sock: TcpStream, server_id: u16) -> Result<()> {
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
                reply(0, 2, server_id, corr, 0xF001, &t, &[])
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
                    let job = if server_id == 0xE007 {
                        "QZHQSSRV"
                    } else {
                        "QZRCSRVS"
                    };
                    p.extend(job_name_param(job, "QUSER"));
                }
                reply(0, 0, server_id, corr, 0xF002, &rc.to_be_bytes(), &p)
            }
            _ if server.is_none() => break,
            _ if server_id == 0xE007 => {
                let srv = server.as_mut().expect("signed on");
                match crate::hostdtaq::request(srv, &ds).await {
                    Some(out) => out,
                    None => continue,
                }
            }
            0x1001 => exchange_attributes(corr),
            0x1002 => {
                let srv = server.as_mut().expect("signed on");
                run_command(srv, &ds).await
            }
            0x1003 => {
                let srv = server.as_mut().expect("signed on");
                call_program(srv, &ds).await
            }
            // End: no reply.
            0x1004 => break,
            _ => command_reply(0x8002, corr, 0x0205, &[]),
        };
        sock.write_all(&out).await?;
        sock.flush().await?;
    }
    Ok(())
}

/// A parameter of a program call: its length, whether the program's
/// value goes back, its bytes.
struct Parm {
    max: usize,
    out: bool,
    data: Vec<u8>,
}

/// The parameters of a call program request. Usage (JTOpen
/// RCCallProgramRequestDataStream): 11 input, 12 output, 13 and 33 input
/// and output (trailing zeros left off, the data is the start of the
/// parameter), 21, 22 and 23 the same with the data run-length
/// compressed (22: output, no data), 255 null.
fn call_parms(ds: &Ds) -> Vec<Parm> {
    let v = &ds.0;
    let n = ds.u16(41) as usize;
    let mut out = Vec::with_capacity(n);
    let mut at = 43;
    for _ in 0..n {
        let Some(h) = v.get(at..at + 12) else { break };
        let ll = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let max = u32::from_be_bytes([h[6], h[7], h[8], h[9]]) as usize;
        let usage = u16::from_be_bytes([h[10], h[11]]);
        let body = v.get(at + 12..at + ll.max(12)).unwrap_or_default();
        let mut data = match usage {
            21 | 23 => rle_expand(body, max),
            22 | 0xFF => Vec::new(),
            _ => body.to_vec(),
        };
        data.resize(max, 0);
        out.push(Parm {
            max,
            out: matches!(usage % 10, 2 | 3),
            data,
        });
        at += ll.max(12);
    }
    out
}

/// Run-length compressed bytes expanded (JTOpen DataStreamCompression:
/// 0x1B 0x1B is 0x1B; 0x1B, two bytes and a 2-byte count repeats them).
fn rle_expand(b: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(max);
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x1B {
            out.push(b[i]);
            i += 1;
        } else if b.get(i + 1) == Some(&0x1B) {
            out.push(0x1B);
            i += 2;
        } else if i + 4 < b.len() {
            let count = u16::from_be_bytes([b[i + 3], b[i + 4]]) as usize;
            for _ in 0..count {
                out.extend([b[i + 1], b[i + 2]]);
            }
            i += 5;
        } else {
            break;
        }
    }
    out
}

/// Call a program: return code 0 with the output parameters, 0x0500
/// with the messages when it was not found (or not authorized), 0x0501
/// when it failed.
async fn call_program(srv: &mut crate::mapepire::Server<'_>, ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let name = text37(ds.0.get(20..30).unwrap_or_default())
        .trim()
        .to_string();
    let lib = text37(ds.0.get(30..40).unwrap_or_default())
        .trim()
        .to_string();
    let parms = call_parms(ds);
    let bytes: Vec<Vec<u8>> = parms.iter().map(|p| p.data.clone()).collect();
    match srv.job.call_program_bytes(&lib, &name, bytes).await {
        Ok((_, back)) => {
            let mut t = 0u16.to_be_bytes().to_vec();
            t.extend(0u16.to_be_bytes());
            let mut p = Vec::new();
            for (parm, b) in parms.iter().zip(back) {
                if !parm.out {
                    continue;
                }
                let mut d = (parm.max as u32).to_be_bytes().to_vec();
                d.extend(12u16.to_be_bytes());
                let mut b = b;
                b.resize(parm.max, 0);
                d.extend(b);
                p.extend(param(0x1103, &d));
            }
            reply(0, 0, 0xE008, corr, 0x8003, &t, &p)
        }
        Err(e) => {
            let text = format!("{e:#}");
            let lines: Vec<&str> = text
                .lines()
                .filter(|l| crate::jobs::message_id(l).len() == 7)
                .collect();
            // The program (or its library) not there: MCH3401, its
            // substitution data the object's type (02 01 a program, 04 01
            // a library) and its name in 30 bytes, as JTOpen's
            // RemoteCommandImplRemote runProgramOffThread reads it to
            // throw ObjectDoesNotExistException.
            if lines
                .iter()
                .any(|l| crate::jobs::message_id(l) == "CPD0170")
            {
                let lib_missing = !matches!(lib.as_str(), "" | "*LIBL" | "*CURLIB")
                    && !crate::catalog::library_exists(srv.job.client, &lib)
                        .await
                        .unwrap_or(true);
                let (code, object) = if lib_missing {
                    ([0x04, 0x01], lib.clone())
                } else {
                    ([0x02, 0x01], name.clone())
                };
                let mut subst = code.to_vec();
                subst.extend(ebcdic(&object, 30));
                let m = crate::mapepire::ClMessage {
                    id: "MCH3401".into(),
                    text: format!("Object {object} cannot be found to be used."),
                    kind: "ESCAPE".into(),
                    severity: crate::pgmmsg::system_severity("MCH3401") as i64,
                    file: Some("QCPFMSG"),
                    library: Some("QSYS"),
                };
                return command_reply(0x8003, corr, 0x0500, &[message(&m, &subst)]);
            }
            let n = lines.len();
            let messages: Vec<Vec<u8>> = lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let id = crate::jobs::message_id(l).to_string();
                    let (library, file) = crate::mapepire::message_file_of(&id);
                    let m = crate::mapepire::ClMessage {
                        severity: crate::pgmmsg::system_severity(&id) as i64,
                        text: crate::jobs::message_text(l).trim().to_string(),
                        kind: if i + 1 == n { "ESCAPE" } else { "DIAGNOSTIC" }.into(),
                        id,
                        file,
                        library,
                    };
                    message(&m, &[])
                })
                .collect();
            let refused = lines
                .iter()
                .any(|l| matches!(crate::jobs::message_id(l), "CPF9801" | "CPF9802"));
            command_reply(
                0x8003,
                corr,
                if refused { 0x0500 } else { 0x0501 },
                &messages,
            )
        }
    }
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
    let list: Vec<Vec<u8>> = messages.iter().map(|m| message(m, &[])).collect();
    command_reply(0x8002, corr, if ok { 0 } else { 0x0400 }, &list)
}

/// A message (CP 0x1106, JTOpen AS400ImplRemote parseMessage): the text
/// CCSID and the substitution data CCSID (4 each), the severity (2), then
/// each as a 4-byte length and its bytes: the type (two digits: 01
/// completion, 02 diagnostic, 04 informational, 14 notify, 15 escape),
/// the ID, the message file, its library, the text, the substitution
/// data (none) and the help (none).
fn message(m: &crate::mapepire::ClMessage, subst: &[u8]) -> Vec<u8> {
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
    field(&mut d, subst);
    field(&mut d, &[]);
    param(0x1106, &d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_call_parameters_as_jtopen_sends_them() {
        // RCCallProgramRequestDataStream: the template's program (20),
        // library (30), message option (40), count (41); parameters
        // from 43: LL, CP 0x1103, length, usage, data (input with its
        // trailing zeros left off; 22 output with none; 21 compressed).
        let mut v = vec![0u8; 43];
        v[41..43].copy_from_slice(&3u16.to_be_bytes());
        for (max, usage, data) in [
            (6u32, 11u16, vec![0xC1, 0xC2]),
            (4, 22, vec![]),
            (8, 21, vec![0x1B, 0x40, 0x40, 0x00, 0x04]),
        ] {
            let mut d = max.to_be_bytes().to_vec();
            d.extend(usage.to_be_bytes());
            d.extend(data);
            v.extend(param(0x1103, &d));
        }
        let p = call_parms(&Ds(v));
        assert_eq!(p.len(), 3);
        assert_eq!(
            (p[0].data.as_slice(), p[0].out),
            (&[0xC1, 0xC2, 0, 0, 0, 0][..], false)
        );
        assert_eq!((p[1].data.as_slice(), p[1].out), (&[0u8; 4][..], true));
        assert_eq!(p[2].data, vec![0x40; 8]);
    }

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
        let p = message(&m, &[]);
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
