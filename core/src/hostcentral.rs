//! The central host server (as-central, port 8470, server ID 0xE000):
//! what the IBM Toolbox for Java asks it for, the system's CCSID and the
//! conversion tables of single-byte CCSIDs it has no table of its own
//! for (JTOpen NLSExchangeAttrRequest/Reply, NLSGetTableRequest/Reply,
//! NLSImplRemote). A connection starts as the remote command server's
//! does (hostcmd.rs `serve_as`).
//!
//! - exchange attributes (0x1301: the client's CCSID at 24; reply 0x1301:
//!   the return codes at 22 and 24, the data stream level 26, the
//!   system's CCSID 28, the server's version 32, function levels 36, 38
//!   and 40);
//! - get table (0x1201: the CCSIDs from and to at 22 and 26, then CP 4
//!   with the 256 characters to convert; reply 0x1201: the return codes
//!   at 22 and 24, then from 28 an LL and CP and the table: each
//!   character in the to-CCSID, 2 bytes, for 1200, 13488 and 61952);
//! - the double-byte and mixed tables (0x1202, 0x1203) and the license
//!   requests are answered with a return code that they are not done.

use crate::hostsrv::{reply, Ds};

/// The system's CCSID.
const SYSTEM_CCSID: u32 = 37;

/// Answer one central server request.
pub fn request(ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let id = ds.req_id();
    match id {
        0x1301 => {
            let mut t = vec![0u8; 2]; // chain
            t.extend(0u16.to_be_bytes()); // 22
            t.extend(0u16.to_be_bytes()); // 24
            t.extend(0u16.to_be_bytes()); // 26 level
            t.extend(SYSTEM_CCSID.to_be_bytes()); // 28
            t.extend(0x0007_0500u32.to_be_bytes()); // 32
            t.extend([0u8; 6]); // 36, 38, 40
            reply(0, 0, 0xE000, corr, 0x1301, &t, &[])
        }
        0x1201 => table(ds),
        // Not done: primary return code 1.
        _ => {
            let mut t = vec![0u8; 2];
            t.extend(1u16.to_be_bytes());
            t.extend(0u16.to_be_bytes());
            t.extend([0u8; 2]);
            reply(0, 0, 0xE000, corr, id, &t, &[])
        }
    }
}

/// A single-byte CCSID's characters in UTF-16 (1200, 13488, 61952).
fn table(ds: &Ds) -> Vec<u8> {
    let corr = ds.correlation();
    let from = ds.u32(22) as i32;
    let to = ds.u32(26);
    let fail = |rc: u16| {
        let mut t = vec![0u8; 2];
        t.extend(rc.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        t.extend([0u8; 2]);
        reply(0, 0, 0xE000, corr, 0x1201, &t, &[])
    };
    if !matches!(to, 1200 | 13488 | 0xF200) || !crate::ifs::codec::supported(from) {
        return fail(2);
    }
    let chars: Vec<u8> = ds
        .params(34)
        .into_iter()
        .find(|(cp, _)| *cp == 4)
        .map(|(_, d)| d.to_vec())
        .unwrap_or_else(|| (0..=255u8).collect());
    let mut data = Vec::with_capacity(chars.len() * 2);
    for b in chars {
        let c = crate::ifs::codec::text(from, &[b])
            .encode_utf16()
            .next()
            .unwrap_or(0xFFFD);
        data.extend(c.to_be_bytes());
    }
    let mut t = vec![0u8; 2];
    t.extend(0u16.to_be_bytes());
    t.extend(0u16.to_be_bytes());
    t.extend([0u8; 2]);
    reply(
        0,
        0,
        0xE000,
        corr,
        0x1201,
        &t,
        &crate::hostsrv::param(4, &data),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(id: u16, t: &[u8], p: &[u8]) -> Ds {
        let mut v = ((20 + t.len() + p.len()) as u32).to_be_bytes().to_vec();
        v.extend([0, 0, 0xE0, 0x00, 0, 0, 0, 0, 0, 0, 0, 1]);
        v.extend((t.len() as u16).to_be_bytes());
        v.extend(id.to_be_bytes());
        v.extend(t);
        v.extend(p);
        Ds(v)
    }

    #[test]
    fn replies_where_jtopen_reads_them() {
        // NLSExchangeAttrReply: return code at 22, CCSID at 28.
        let r = request(&ds(0x1301, &[0; 18], &[]));
        assert_eq!(u16::from_be_bytes([r[22], r[23]]), 0);
        assert_eq!(u32::from_be_bytes([r[28], r[29], r[30], r[31]]), 37);
        // NLSGetTableReply: the table from 34 (LL at 28), CCSID 37's
        // 0xC1 is 'A' and 0x40 a blank (the EBCDIC code page 37 chart).
        let mut t = vec![0u8; 14];
        t[2..6].copy_from_slice(&37u32.to_be_bytes());
        t[6..10].copy_from_slice(&13488u32.to_be_bytes());
        let p = crate::hostsrv::param(4, &(0..=255u8).collect::<Vec<_>>());
        let r = request(&ds(0x1201, &t, &p));
        assert_eq!(u16::from_be_bytes([r[22], r[23]]), 0);
        assert_eq!(u32::from_be_bytes([r[28], r[29], r[30], r[31]]), 6 + 512);
        assert_eq!(
            u16::from_be_bytes([r[34 + 0xC1 * 2], r[35 + 0xC1 * 2]]),
            'A' as u16
        );
        assert_eq!(u16::from_be_bytes([r[34 + 0x80], r[35 + 0x80]]), ' ' as u16);
    }
}
