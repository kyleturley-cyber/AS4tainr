//! The network print server (as-netprt, port 8474, server ID 0xE003):
//! what IBM i Access Client Solutions' printer output and the IBM Toolbox
//! for Java's SpooledFile and PrintObject input streams ask of it. A
//! connection starts as the remote command server's does (hostcmd.rs
//! `serve_as`).
//!
//! Written from the client side of the protocol in JTOpen (NPDataStream,
//! NPCodePoint, NPCPAttributeValue, NPCPAttributeIDList, NPConversation,
//! PrintObjectImplRemote, PrintObjectInputStreamImplRemote,
//! PrintObjectTransformedInputStreamImplRemote):
//!
//! - a request is the header, a 12-byte template (action ID at 20, flags
//!   at 22, return code at 26, error object at 28) and code points (a
//!   4-byte length counting its 6-byte header, a 2-byte ID, the data);
//!   the request ID is the object type (1 spooled file, 8 the server),
//!   the reply's the same with bit 0x8000;
//! - an attribute value code point (0x0008, and the IDs of objects, the
//!   spooled file ID 0x0001) has a count and an entry length (12), then
//!   for each attribute its ID, type (3 four-byte binary, 5 string, 7
//!   packed 15,5), length and the offset of its value from the start of
//!   the code point; strings are in the server's CCSID with a closing
//!   null; an attribute ID list (0x0007) has a count, a length (2) and
//!   the IDs;
//! - the server's attributes (its CCSID, level, job; the NLV is changed);
//! - a spooled file's attributes (retrieve attributes, by its ID or an
//!   open handle); open (0x0002: the SCS data the spooled file APIs give,
//!   spoolapi.rs `scs`) and open modified (0x001A: the text of its pages,
//!   ASCII lines ending in CR LF and a form feed after each page, whatever
//!   transform is asked for); read (0x0003: up to NUMBYTES bytes in a data
//!   code point 0x0009, return code 0x0013 at the end), tell (0x0018: the
//!   bytes left), seek (0x0017), close (0x0005). Other actions answer
//!   return code 0x0015, function not supported (docs/fidelity-gaps.md).

use crate::hostsrv::{reply, Ds};
use std::collections::HashMap;
use tokio_postgres::Client;

/// The server's CCSID (strings in attribute values).
const CCSID: u16 = 37;

// Object types (request IDs).
const SPOOLED_FILE: u16 = 0x0001;
const NP_SERVER: u16 = 0x0008;

// Actions.
const OPEN: u16 = 0x0002;
const READ: u16 = 0x0003;
const CLOSE: u16 = 0x0005;
const CHANGE_ATTRIBUTES: u16 = 0x000E;
const RETRIEVE_ATTRIBUTES: u16 = 0x000F;
const SEEK: u16 = 0x0017;
const TELL: u16 = 0x0018;
const OPEN_MODIFIED: u16 = 0x001A;

// Code points.
const CP_SPOOLED_FILE_ID: u16 = 0x0001;
const CP_ATTRIBUTE_LIST: u16 = 0x0007;
const CP_ATTRIBUTE_VALUE: u16 = 0x0008;
const CP_DATA: u16 = 0x0009;
const CP_HANDLE: u16 = 0x000C;

// Return codes.
const RET_INV_REQ_ACT: u16 = 0x0004;
const RET_INV_SPLF_HANDLE: u16 = 0x000A;
const RET_READ_EOF: u16 = 0x0013;
const RET_FUNCTION_NOT_SUP: u16 = 0x0015;
const RET_SPLF_NOT_FOUND: u16 = 0x001E;

// Attribute IDs (JTOpen PrintObject).
const ATTR_CPI: u16 = 0x0017;
const ATTR_CODEPAGE: u16 = 0x0019;
const ATTR_COPIES: u16 = 0x001C;
const ATTR_COPIESLEFT: u16 = 0x001D;
const ATTR_CURPAGE: u16 = 0x001E;
const ATTR_DATE: u16 = 0x0022;
const ATTR_FORMTYPE: u16 = 0x0030;
const ATTR_CHAR_ID: u16 = 0x0037;
const ATTR_JOBNAME: u16 = 0x003B;
const ATTR_JOBNUMBER: u16 = 0x003C;
const ATTR_JOBUSER: u16 = 0x003E;
const ATTR_LPI: u16 = 0x0040;
const ATTR_OUTQUELIB: u16 = 0x0049;
const ATTR_OUTQUE: u16 = 0x004A;
const ATTR_PAGELEN: u16 = 0x004E;
const ATTR_PAGEWIDTH: u16 = 0x0051;
const ATTR_PRTDEVTYPE: u16 = 0x005A;
const ATTR_SPOOLFILE: u16 = 0x0068;
const ATTR_SPLFNUM: u16 = 0x0069;
const ATTR_SPLFSTATUS: u16 = 0x006A;
const ATTR_SCHEDULE: u16 = 0x006B;
const ATTR_TIME: u16 = 0x006E;
const ATTR_PAGES: u16 = 0x006F;
const ATTR_USERDATA: u16 = 0x0074;
const ATTR_NUMBYTES: u16 = 0x007D;
const ATTR_SEEKOFF: u16 = 0x007E;
const ATTR_SEEKORG: u16 = 0x007F;
const ATTR_NPSCCSID: u16 = 0x008A;
const ATTR_NPSLEVEL: u16 = 0x008D;
const ATTR_NLV_ID: u16 = 0x00B4;
const ATTR_PRTASSIGNED: u16 = 0x00BA;
const ATTR_JOBSYSTEM: u16 = 0x00FB;

/// An attribute's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Str(String),
    /// Packed decimal (15,5), as hundred-thousandths.
    Dec(i64),
}

/// An open spooled file: its bytes and where reading is.
struct Open {
    data: Vec<u8>,
    pos: usize,
}

/// What a connection keeps.
#[derive(Default)]
pub struct Session {
    open: HashMap<u32, Open>,
    next: u32,
}

/// The code points of a request (from 32): (ID, data).
fn code_points(b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut at = 32;
    while at + 6 <= b.len() {
        let ll = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let id = u16::from_be_bytes([b[at + 4], b[at + 5]]);
        if ll < 6 || at + ll > b.len() {
            break;
        }
        out.push((id, &b[at + 6..at + ll]));
        at += ll;
    }
    out
}

/// The attributes of an attribute value code point's data.
pub fn parse_values(d: &[u8]) -> HashMap<u16, Vec<u8>> {
    let mut out = HashMap::new();
    if d.len() < 4 {
        return out;
    }
    let n = u16::from_be_bytes([d[0], d[1]]) as usize;
    let size = (u16::from_be_bytes([d[2], d[3]]) as usize).max(12);
    for k in 0..n {
        let e = 4 + k * size;
        let Some(en) = d.get(e..e + 12) else { break };
        let id = u16::from_be_bytes([en[0], en[1]]);
        let len = u32::from_be_bytes(en[4..8].try_into().unwrap()) as usize;
        let off = (u32::from_be_bytes(en[8..12].try_into().unwrap()) as usize).saturating_sub(6);
        if let Some(v) = d.get(off..off + len) {
            out.insert(id, v.to_vec());
        }
    }
    out
}

/// A string attribute's text (EBCDIC, up to its null).
fn text(v: &[u8]) -> String {
    let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
    crate::rpg::data::a(&v[..end]).trim().to_string()
}

fn int(v: &[u8]) -> i64 {
    v.get(..4)
        .map(|x| i32::from_be_bytes(x.try_into().unwrap()) as i64)
        .unwrap_or(0)
}

/// The IDs of an attribute ID list code point.
pub fn parse_ids(d: &[u8]) -> Vec<u16> {
    if d.len() < 4 {
        return Vec::new();
    }
    let n = u16::from_be_bytes([d[0], d[1]]) as usize;
    (0..n)
        .filter_map(|k| d.get(4 + 2 * k..6 + 2 * k))
        .map(|x| u16::from_be_bytes([x[0], x[1]]))
        .collect()
}

/// Packed decimal (15,5): 8 bytes, sign F or D.
fn packed15(hundred_thousandths: i64) -> Vec<u8> {
    let digits = format!("{:015}", hundred_thousandths.unsigned_abs());
    let mut nibbles: Vec<u8> = digits.bytes().map(|c| c - b'0').collect();
    nibbles.push(if hundred_thousandths < 0 { 0x0D } else { 0x0F });
    nibbles.chunks(2).map(|p| (p[0] << 4) | p[1]).collect()
}

/// An attribute value code point (with its header).
pub fn values_cp(id: u16, values: &[(u16, Value)]) -> Vec<u8> {
    let n = values.len();
    let mut entries = Vec::new();
    let mut data = Vec::new();
    let base = 6 + 4 + 12 * n;
    for (attr, v) in values {
        let (ty, bytes): (u16, Vec<u8>) = match v {
            Value::Int(i) => (3, i.to_be_bytes().to_vec()),
            Value::Str(s) => {
                let mut b = crate::rpg::data::e(s);
                b.push(0);
                (5, b)
            }
            Value::Dec(d) => (7, packed15(*d)),
        };
        entries.extend(attr.to_be_bytes());
        entries.extend(ty.to_be_bytes());
        entries.extend((bytes.len() as u32).to_be_bytes());
        entries.extend(((base + data.len()) as u32).to_be_bytes());
        data.extend(bytes);
    }
    let mut body = (n as u16).to_be_bytes().to_vec();
    body.extend(12u16.to_be_bytes());
    body.extend(entries);
    body.extend(data);
    cp(id, &body)
}

fn cp(id: u16, data: &[u8]) -> Vec<u8> {
    let mut v = ((data.len() + 6) as u32).to_be_bytes().to_vec();
    v.extend(id.to_be_bytes());
    v.extend(data);
    v
}

/// A reply: its ID the request's with bit 0x8000 (JTOpen NPDataStream
/// hashCode), the template (the action, flags 0: the last reply, the
/// return code) and the code points.
fn np_reply(ds: &Ds, rc: u16, cps: &[u8]) -> Vec<u8> {
    let action = ds.u16(20);
    let mut t = action.to_be_bytes().to_vec();
    t.extend(0u32.to_be_bytes());
    t.extend(rc.to_be_bytes());
    t.extend(0u32.to_be_bytes());
    reply(
        0,
        0,
        0xE003,
        ds.correlation(),
        ds.req_id() | 0x8000,
        &t,
        cps,
    )
}

/// Answer one print server request.
pub async fn request(client: &Client, user: &str, s: &mut Session, ds: &Ds) -> Vec<u8> {
    let cps = code_points(&ds.0);
    let get = |id: u16| cps.iter().find(|(c, _)| *c == id).map(|(_, d)| *d);
    let wanted: Vec<u16> = get(CP_ATTRIBUTE_LIST).map(parse_ids).unwrap_or_default();
    let given = get(CP_ATTRIBUTE_VALUE)
        .map(parse_values)
        .unwrap_or_default();
    let action = ds.u16(20);
    match (ds.req_id(), action) {
        (NP_SERVER, RETRIEVE_ATTRIBUTES | CHANGE_ATTRIBUTES) => {
            let mut vals = Vec::new();
            for id in &wanted {
                let v = match *id {
                    ATTR_NPSCCSID => Value::Int(CCSID as i32),
                    ATTR_NPSLEVEL => Value::Str("V7R5M0".into()),
                    ATTR_NLV_ID => Value::Str(
                        given
                            .get(&ATTR_NLV_ID)
                            .map(|v| text(v))
                            .unwrap_or_else(|| "2924".into()),
                    ),
                    ATTR_JOBNAME => Value::Str("QNPSERVS".into()),
                    ATTR_JOBUSER => Value::Str("QUSER".into()),
                    ATTR_JOBNUMBER => Value::Str(format!("{:06}", std::process::id() % 1_000_000)),
                    _ => continue,
                };
                vals.push((*id, v));
            }
            np_reply(ds, 0, &values_cp(CP_ATTRIBUTE_VALUE, &vals))
        }
        (SPOOLED_FILE, RETRIEVE_ATTRIBUTES) => {
            // By an open handle: the bytes of what is open.
            if let Some(h) = get(CP_HANDLE) {
                let Some(o) = s.open.get(&(int(h) as u32)) else {
                    return np_reply(ds, RET_INV_SPLF_HANDLE, &[]);
                };
                let vals = vec![(ATTR_NUMBYTES, Value::Int(o.data.len() as i32))];
                return np_reply(ds, 0, &values_cp(CP_ATTRIBUTE_VALUE, &vals));
            }
            let Some(splf) = find(client, user, get(CP_SPOOLED_FILE_ID)).await else {
                return np_reply(ds, RET_SPLF_NOT_FOUND, &[]);
            };
            let mut vals = Vec::new();
            for id in &wanted {
                if *id == ATTR_NUMBYTES {
                    let n = crate::spoolapi::scs(client, splf.id)
                        .await
                        .map(|(d, _)| d.len())
                        .unwrap_or(0);
                    vals.push((*id, Value::Int(n as i32)));
                } else if let Some(v) = attribute(&splf, *id) {
                    vals.push((*id, v));
                }
            }
            np_reply(ds, 0, &values_cp(CP_ATTRIBUTE_VALUE, &vals))
        }
        (SPOOLED_FILE, OPEN | OPEN_MODIFIED) => {
            let Some(splf) = find(client, user, get(CP_SPOOLED_FILE_ID)).await else {
                return np_reply(ds, RET_SPLF_NOT_FOUND, &[]);
            };
            let data = if action == OPEN {
                crate::spoolapi::scs(client, splf.id)
                    .await
                    .map(|(d, _)| d)
                    .unwrap_or_default()
            } else {
                plain_text(client, splf.id).await
            };
            s.next += 1;
            let h = s.next;
            s.open.insert(h, Open { data, pos: 0 });
            np_reply(ds, 0, &cp(CP_HANDLE, &h.to_be_bytes()))
        }
        (SPOOLED_FILE, READ) => {
            let Some(o) = get(CP_HANDLE).and_then(|h| s.open.get_mut(&(int(h) as u32))) else {
                return np_reply(ds, RET_INV_SPLF_HANDLE, &[]);
            };
            let want = given
                .get(&ATTR_NUMBYTES)
                .map(|v| int(v))
                .unwrap_or(4096)
                .max(0) as usize;
            if o.pos >= o.data.len() {
                return np_reply(ds, RET_READ_EOF, &[]);
            }
            let end = (o.pos + want).min(o.data.len());
            let chunk = o.data[o.pos..end].to_vec();
            o.pos = end;
            np_reply(ds, 0, &cp(CP_DATA, &chunk))
        }
        (SPOOLED_FILE, TELL) => {
            let Some(o) = get(CP_HANDLE).and_then(|h| s.open.get(&(int(h) as u32))) else {
                return np_reply(ds, RET_INV_SPLF_HANDLE, &[]);
            };
            let left = o.data.len().saturating_sub(o.pos);
            let vals = vec![(ATTR_NUMBYTES, Value::Int(left as i32))];
            np_reply(ds, 0, &values_cp(CP_ATTRIBUTE_VALUE, &vals))
        }
        (SPOOLED_FILE, SEEK) => {
            let Some(o) = get(CP_HANDLE).and_then(|h| s.open.get_mut(&(int(h) as u32))) else {
                return np_reply(ds, RET_INV_SPLF_HANDLE, &[]);
            };
            let off = given.get(&ATTR_SEEKOFF).map(|v| int(v)).unwrap_or(0);
            let from = match given.get(&ATTR_SEEKORG).map(|v| int(v)).unwrap_or(1) {
                2 => o.pos as i64,
                3 => o.data.len() as i64,
                _ => 0,
            };
            o.pos = (from + off).clamp(0, o.data.len() as i64) as usize;
            np_reply(ds, 0, &[])
        }
        (SPOOLED_FILE, CLOSE) => {
            if let Some(h) = get(CP_HANDLE) {
                s.open.remove(&(int(h) as u32));
            }
            np_reply(ds, 0, &[])
        }
        (SPOOLED_FILE, _) => np_reply(ds, RET_FUNCTION_NOT_SUP, &[]),
        _ => np_reply(ds, RET_INV_REQ_ACT, &[]),
    }
}

/// The spooled file a spooled file ID code point names: its name and
/// number, the job's name, user and number.
async fn find(client: &Client, user: &str, id: Option<&[u8]>) -> Option<crate::spoolapi::Splf> {
    let a = parse_values(id?);
    let name = a
        .get(&ATTR_SPOOLFILE)
        .map(|v| text(v))?
        .to_ascii_uppercase();
    let number = a.get(&ATTR_SPLFNUM).map(|v| int(v) as i32);
    let job_name = a.get(&ATTR_JOBNAME).map(|v| text(v).to_ascii_uppercase());
    let job_user = a.get(&ATTR_JOBUSER).map(|v| text(v).to_ascii_uppercase());
    let job_number = a
        .get(&ATTR_JOBNUMBER)
        .and_then(|v| text(v).parse::<i64>().ok());
    let all = crate::spoolapi::select(client, &Default::default(), user, None)
        .await
        .ok()?;
    let mut matching: Vec<_> = all
        .into_iter()
        .filter(|s| {
            s.file == name
                && job_name.as_ref().is_none_or(|j| *j == s.job_name)
                && job_user.as_ref().is_none_or(|u| *u == s.job_user)
                && job_number.is_none_or(|n| n == s.job_number)
                && number.is_none_or(|n| n <= 0 || n == s.number)
        })
        .collect();
    matching.pop()
}

/// A spooled file's attribute, for the IDs as4tainr knows.
fn attribute(s: &crate::spoolapi::Splf, id: u16) -> Option<Value> {
    let status = match s.status.as_str() {
        "RDY" => "*READY",
        "OPN" => "*OPEN",
        "CLO" => "*CLOSED",
        "SAV" => "*SAVED",
        "WTR" => "*WRITING",
        "HLD" => "*HELD",
        other => other,
    };
    Some(match id {
        ATTR_SPOOLFILE => Value::Str(s.file.clone()),
        ATTR_SPLFNUM => Value::Int(s.number),
        ATTR_JOBNAME => Value::Str(s.job_name.clone()),
        ATTR_JOBUSER => Value::Str(s.job_user.clone()),
        ATTR_JOBNUMBER => Value::Str(format!("{:06}", s.job_number)),
        ATTR_JOBSYSTEM => Value::Str(crate::report::system_name()),
        ATTR_DATE => Value::Str(s.opened.0.clone()),
        ATTR_TIME => Value::Str(s.opened.1.clone()),
        ATTR_PAGES => Value::Int(s.pages),
        ATTR_CURPAGE => Value::Int(0),
        ATTR_COPIES | ATTR_COPIESLEFT => Value::Int(1),
        ATTR_SPLFSTATUS => Value::Str(status.into()),
        ATTR_OUTQUE => Value::Str(s.outq.clone()),
        ATTR_OUTQUELIB => Value::Str(s.outq_lib.clone()),
        ATTR_USERDATA => Value::Str(s.user_data.clone()),
        ATTR_FORMTYPE => Value::Str("*STD".into()),
        ATTR_PRTDEVTYPE => Value::Str("*SCS".into()),
        ATTR_SCHEDULE => Value::Str("*FILEEND".into()),
        ATTR_PRTASSIGNED => Value::Str("3".into()),
        ATTR_CODEPAGE => Value::Str(CCSID.to_string()),
        ATTR_CHAR_ID => Value::Str("*DEVD".into()),
        // A printed page as QSYSPRT has it: 66 lines of 132 characters,
        // 6 lines and 10 characters per inch.
        ATTR_PAGELEN => Value::Dec(66 * 100_000),
        ATTR_PAGEWIDTH => Value::Dec(132 * 100_000),
        ATTR_LPI => Value::Dec(6 * 100_000),
        ATTR_CPI => Value::Dec(10 * 100_000),
        _ => return None,
    })
}

/// A spooled file's pages as ASCII text: each line ending in CR LF, a
/// form feed after each page.
async fn plain_text(client: &Client, id: i64) -> Vec<u8> {
    let pages = crate::spool::pages(client, id).await.unwrap_or_default();
    let mut out = Vec::new();
    for page in pages {
        for line in page {
            out.extend(
                line.trim_end()
                    .chars()
                    .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }),
            );
            out.extend(b"\r\n");
        }
        out.push(0x0C);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // JTOpen NPCPAttributeValue: count, entry length 12, entries of ID,
    // type, length and offset from the code point's start (its 6-byte
    // header included), then the values; strings end in a null.
    #[test]
    fn attribute_values_round_trip() {
        let cp = values_cp(
            CP_ATTRIBUTE_VALUE,
            &[
                (ATTR_SPOOLFILE, Value::Str("QPDSPJOB".into())),
                (ATTR_SPLFNUM, Value::Int(3)),
            ],
        );
        assert_eq!(u16::from_be_bytes([cp[4], cp[5]]), CP_ATTRIBUTE_VALUE);
        let v = parse_values(&cp[6..]);
        assert_eq!(text(&v[&ATTR_SPOOLFILE]), "QPDSPJOB");
        assert_eq!(*v[&ATTR_SPOOLFILE].last().unwrap(), 0);
        assert_eq!(int(&v[&ATTR_SPLFNUM]), 3);
        // The type of a string is 5, of a four-byte binary 3.
        assert_eq!(u16::from_be_bytes([cp[12], cp[13]]), 5);
        assert_eq!(u16::from_be_bytes([cp[24], cp[25]]), 3);
    }

    // JTOpen NPAttrFloat: packed decimal (15,5), positive sign F: 6.00000
    // is the digits 000000000600000 and the sign.
    #[test]
    fn packed_15_5() {
        assert_eq!(
            packed15(6 * 100_000),
            [0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x0F]
        );
    }
}
