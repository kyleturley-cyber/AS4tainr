//! The data queue host server (as-dtaq, port 8472, server ID 0xE007): the
//! server the IBM Toolbox for Java's DataQueue and KeyedDataQueue talk
//! to. A connection starts as the remote command server's does
//! (hostcmd.rs `serve_as`: seeds, start server, a job for the user).
//!
//! Requests (JTOpen DQ*DataStream, BaseDataQueueImplRemote): a header,
//! then the queue's name at 20 and its library at 30 (10 bytes each in
//! CCSID 37), then
//!
//! - exchange attributes (0x0000: the client's version at 20; reply
//!   0x8000);
//! - attributes (0x0001; reply 0x8001: the largest entry at 22, whether
//!   sender information is kept at 26, the type at 27 (F0 FIFO, F1 LIFO,
//!   F2 keyed), the key length at 28, force at 30, the text at 31, 50
//!   bytes);
//! - read (0x0002: keyed at 40, the key search order at 41 (2 bytes:
//!   EQ, NE, LT, LE, GT, GE), the wait in seconds at 43 (-1 for ever),
//!   peek at 47, then CP 0x5002 the key; reply 0x8003: the sender
//!   information at 22 (36 bytes: job, user, job number, current user;
//!   blanks when the queue keeps none), then CP 0x5001 the entry and CP
//!   0x5002 its key, or the common reply with 0xF006 when there was none);
//! - create (0x0003: the largest entry at 40, authority at 44, sender
//!   information at 45, sequence at 46 (F0 FIFO, F1 LIFO, F2 keyed), key
//!   length at 47, force at 49, text at 50);
//! - delete (0x0004), write (0x0005: keyed at 40, reply wanted at 41,
//!   from 42 CP 0x5001 the entry, CP 0x5002 the key), clear (0x0006: keyed at
//!   40, CP 0x5002 the key).
//!
//! Other replies are the common reply 0x8002: the return code at 20
//! (0xF000 done, 0xF001 a command check with its message: LL, CP, then
//! "CPFnnnn: text" in CCSID 37 from 28).
//!
//! The work is done by the data queue APIs (dtaq.rs), with their
//! messages.

use crate::hostsrv::{ebcdic, param, reply, text37, Ds};

/// Answer one data queue request: None when the request wants no reply
/// (a write without a reply wanted).
pub async fn request(srv: &mut crate::mapepire::Server<'_>, ds: &Ds) -> Option<Vec<u8>> {
    let corr = ds.correlation();
    let v = &ds.0;
    let name = text37(v.get(20..30).unwrap_or_default()).trim().to_string();
    let lib = text37(v.get(30..40).unwrap_or_default()).trim().to_string();
    let job = &mut srv.job;
    let caller = job.dtaq_caller();
    let client = job.client;
    let r = match ds.req_id() {
        0x0000 => {
            return Some(reply(
                0,
                0,
                0xE007,
                corr,
                0x8000,
                &0xF000u16.to_be_bytes(),
                &[],
            ))
        }
        0x0001 => match crate::dtaq::find(client, &lib, &name, &caller).await {
            Ok(q) => return Some(attributes(corr, &q)),
            Err(e) => Err(e),
        },
        0x0002 => return Some(read(client, &caller, ds, &name, &lib).await),
        0x0003 => create(job, ds, &name, &lib).await,
        0x0004 => job
            .run(&format!("DLTDTAQ DTAQ({lib}/{name})"))
            .await
            .map(|_| ()),
        0x0005 => {
            let entry = cp_data(ds, 42, 0x5001).unwrap_or_default();
            let key = cp_data(ds, 42, 0x5002);
            let mut args = api_args(&name, &lib, &[(3, 5)]);
            args.0.push((packed5(entry.len() as i64), packed_type(5)));
            args.0.push((entry.clone(), char_type(entry.len())));
            if let Some(k) = &key {
                args.0.push((packed3(k.len() as i64), packed_type(3)));
                args.0.push((k.clone(), char_type(k.len())));
            }
            let r = crate::sysapi::call_api(client, &caller, "QSNDDTAQ", &mut args).await;
            if v.get(41) != Some(&0xF1) && r.is_ok() {
                return None;
            }
            r
        }
        0x0006 => {
            let mut args = api_args(&name, &lib, &[]);
            if let Some(k) = cp_data(ds, 41, 0x5002) {
                args.0.push((ebcdic("EQ", 2), char_type(2)));
                args.0.push((packed3(k.len() as i64), packed_type(3)));
                args.0.push((k.clone(), char_type(k.len())));
                args.0.push((vec![0; 8], char_type(8)));
            }
            crate::sysapi::call_api(client, &caller, "QCLRDTAQ", &mut args).await
        }
        _ => return Some(common(corr, 0xF003, None)),
    };
    Some(match r {
        Ok(()) => common(corr, 0xF000, None),
        Err(e) => common(corr, 0xF001, Some(&e)),
    })
}

type Args = crate::dtaq::RpgArgs;

fn char_type(n: usize) -> crate::rpg::data::DType {
    crate::rpg::data::DType::Char(n as u32)
}

fn packed_type(digits: u32) -> crate::rpg::data::DType {
    crate::rpg::data::DType::Packed(digits, 0)
}

/// A packed decimal of 5 digits (3 bytes).
fn packed5(n: i64) -> Vec<u8> {
    packed_n(n, 5)
}

/// A packed decimal of 3 digits (2 bytes).
fn packed3(n: i64) -> Vec<u8> {
    packed_n(n, 3)
}

fn packed_n(n: i64, digits: u32) -> Vec<u8> {
    let t = packed_type(digits);
    let mut b = vec![0u8; t.size()];
    let v = crate::rpg::data::Value::Num(
        crate::rpg::dec::Dec::parse(&n.to_string()).unwrap_or_default(),
    );
    let _ = crate::rpg::data::store(
        &t,
        &v,
        &mut b,
        crate::rpg::data::Fit::Truncate { half_adjust: false },
        false,
    );
    b
}

/// The queue's name and library as an API's first two parameters.
fn api_args(name: &str, lib: &str, _packed: &[(usize, u32)]) -> Args {
    crate::dtaq::RpgArgs(vec![
        (ebcdic(name, 10), char_type(10)),
        (ebcdic(lib, 10), char_type(10)),
    ])
}

/// The data of CP `want` among the parameters from `from`.
fn cp_data(ds: &Ds, from: usize, want: u16) -> Option<Vec<u8>> {
    ds.params(from)
        .into_iter()
        .find(|(cp, _)| *cp == want)
        .map(|(_, d)| d.to_vec())
}

/// The common reply: its return code, and for 0xF001 the message
/// ("CPFnnnn: text", CCSID 37) of the error.
fn common(corr: u32, rc: u16, e: Option<&anyhow::Error>) -> Vec<u8> {
    let mut p = Vec::new();
    if let Some(e) = e {
        let all = format!("{e:#}");
        let last = all
            .lines()
            .rev()
            .find(|l| crate::jobs::message_id(l).len() == 7)
            .unwrap_or(all.lines().last().unwrap_or(""));
        let id = crate::jobs::message_id(last);
        let text = crate::jobs::message_text(last).trim().to_string();
        // A keyed queue used without a key: CPF9506, the key length not
        // the queue's (what JTOpen's BaseDataQueueImplRemote takes for a
        // keyed queue opened as DataQueue: IllegalObjectTypeException).
        let (id, text) = if id == "CPF9501" {
            (
                "CPF9506",
                format!("{text} The key length 0 is not the queue's."),
            )
        } else {
            (id, text)
        };
        let msg = if id.len() == 7 {
            format!("{id}: {text}")
        } else {
            format!("CPF9898: {}", last.trim())
        };
        p = param(0x0001, &crate::ifs::codec::bytes(37, &msg));
    }
    reply(0, 0, 0xE007, corr, 0x8002, &rc.to_be_bytes(), &p)
}

/// The attributes reply (0x8001).
fn attributes(corr: u32, q: &crate::dtaq::DataQueue) -> Vec<u8> {
    let mut t = 0xF000u16.to_be_bytes().to_vec();
    t.extend((q.maxlen as u32).to_be_bytes());
    t.push(if q.senderid { 0xF1 } else { 0xF0 });
    t.push(match q.seq {
        'L' => 0xF1,
        'K' => 0xF2,
        _ => 0xF0,
    });
    t.extend((q.keylen as u16).to_be_bytes());
    t.push(if q.force { 0xF1 } else { 0xF0 });
    t.extend(ebcdic(&q.text, 50));
    reply(0, 0, 0xE007, corr, 0x8001, &t, &[])
}

/// Read (receive or peek): Receive Data Queue with the key search order,
/// the wait, 44 bytes of sender information and "remove" *NO to peek.
async fn read(
    client: &tokio_postgres::Client,
    caller: &crate::dtaq::Caller,
    ds: &Ds,
    name: &str,
    lib: &str,
) -> Vec<u8> {
    let corr = ds.correlation();
    let v = &ds.0;
    let q = match crate::dtaq::find(client, lib, name, caller).await {
        Ok(q) => q,
        Err(e) => return common(corr, 0xF001, Some(&e)),
    };
    let order = text37(v.get(41..43).unwrap_or_default());
    let wait = v
        .get(43..47)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0);
    let peek = v.get(47) == Some(&0xF1);
    let key = cp_data(ds, 48, 0x5002);
    let max = q.maxlen.max(1) as usize;
    let mut args = api_args(name, lib, &[]);
    args.0.push((packed5(0), packed_type(5)));
    args.0.push((vec![0x40; max], char_type(max)));
    args.0.push((packed5(wait as i64), packed_type(5)));
    let k = key.clone().unwrap_or_default();
    args.0.push((
        ebcdic(
            if order.trim().is_empty() {
                "EQ"
            } else {
                order.trim()
            },
            2,
        ),
        char_type(2),
    ));
    args.0.push((packed3(k.len() as i64), packed_type(3)));
    args.0.push((k.clone(), char_type(k.len())));
    args.0.push((packed3(44), packed_type(3)));
    args.0.push((vec![0x40; 44], char_type(44)));
    args.0
        .push((ebcdic(if peek { "*NO" } else { "*YES" }, 10), char_type(10)));
    args.0.push((max_size(max), char_type(4)));
    args.0.push((vec![0; 8], char_type(8)));
    // Remove message and size of data receiver: the 13-parameter form.
    if let Err(e) = crate::sysapi::call_api(client, caller, "QRCVDTAQ", &mut args).await {
        return common(corr, 0xF001, Some(&e));
    }
    let len = crate::dtaq::ApiArgs::num(&args, 2).unwrap_or(0).max(0) as usize;
    if len == 0 {
        return common(corr, 0xF006, None);
    }
    let mut t = 0xF000u16.to_be_bytes().to_vec();
    let info = &args.0[9].0;
    let sender = if q.senderid && info.len() >= 44 {
        info[8..44].to_vec()
    } else {
        vec![0x40; 36]
    };
    t.extend(sender);
    let data = &args.0[3].0[..len.min(max)];
    let mut p = param(0x5001, data);
    if key.is_some() {
        p.extend(param(0x5002, &args.0[7].0));
    }
    reply(0, 0, 0xE007, corr, 0x8003, &t, &p)
}

/// A binary(4) size, as the receive API's size of data receiver.
fn max_size(n: usize) -> Vec<u8> {
    (n as i32).to_be_bytes().to_vec()
}

/// Create: CRTDTAQ's values from the request (the authority is not
/// used: the queue gets the library's).
async fn create(
    job: &mut crate::commands::Job<'_>,
    ds: &Ds,
    name: &str,
    lib: &str,
) -> anyhow::Result<()> {
    let v = &ds.0;
    let b = |i: usize| v.get(i).copied().unwrap_or(0xF0);
    let maxlen = v
        .get(40..44)
        .map(|x| i32::from_be_bytes([x[0], x[1], x[2], x[3]]))
        .unwrap_or(0);
    let keylen = v
        .get(47..49)
        .map(|x| u16::from_be_bytes([x[0], x[1]]) as i32)
        .unwrap_or(0);
    let text = text37(v.get(50..100).unwrap_or_default())
        .trim_end()
        .to_string();
    let lib = match lib {
        "*CURLIB" | "" => job.current_library(),
        l => crate::qtemp::real(l),
    };
    if !(1..=crate::dtaq::MAX_ENTRY).contains(&maxlen) {
        anyhow::bail!("CPF2498: Length of data {maxlen} is not valid for data queue {name}.");
    }
    crate::dtaq::create(
        job.client,
        &crate::dtaq::Create {
            library: &lib,
            name,
            maxlen,
            seq: match b(46) {
                0xF1 => 'L',
                0xF2 => 'K',
                _ => 'F',
            },
            keylen,
            senderid: b(45) == 0xF1,
            force: b(49) == 0xF1,
            autorcl: false,
            size_max: -1,
            size_init: 16,
            text: &text,
        },
    )
    .await?;
    crate::authority::set_owner(job.client, &lib, name, "*DTAQ", &job.user).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_where_jtopen_reads_them() {
        // DQRequestAttributesNormalReplyDataStream: largest entry at 22,
        // sender information 26, type 27 (low four bits), key length 28,
        // force 30, text 31.
        let q = crate::dtaq::DataQueue {
            library: "L".into(),
            name: "Q".into(),
            maxlen: 512,
            seq: 'K',
            keylen: 8,
            senderid: true,
            force: false,
            autorcl: false,
            size_max: -1,
            size_init: 16,
            text: "orders".into(),
        };
        let r = attributes(1, &q);
        assert_eq!(u16::from_be_bytes([r[18], r[19]]), 0x8001);
        assert_eq!(u32::from_be_bytes([r[22], r[23], r[24], r[25]]), 512);
        assert_eq!((r[26], r[27] & 0x0F, r[30]), (0xF1, 2, 0xF0));
        assert_eq!(u16::from_be_bytes([r[28], r[29]]), 8);
        assert_eq!(text37(&r[31..81]).trim(), "orders");
        // DQCommonReplyDataStream: return code at 20, the message from
        // 28 (its ID the first 7 characters, the text after 9).
        let e = anyhow::anyhow!("CPF9801: Object Q is not in library L.");
        let r = common(1, 0xF001, Some(&e));
        assert_eq!(u16::from_be_bytes([r[20], r[21]]), 0xF001);
        let len = u32::from_be_bytes([r[22], r[23], r[24], r[25]]) as usize - 6;
        let m = text37(&r[28..28 + len]);
        assert_eq!(&m[..7], "CPF9801");
        assert_eq!(&m[9..], "Object Q is not in library L.");
        assert_eq!(packed5(11), vec![0x00, 0x01, 0x1F]);
    }
}
