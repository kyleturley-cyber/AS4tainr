//! DSPFD OUTPUT(*OUTFILE) for TYPE(*BASATR), *MBR, *RCDFMT and *ACCPTH:
//! records in the formats of QAFDBASI, QAFDMBR, QAFDRFMT and QAFDACCP
//! (outfile.rs).
//!
//! Values observed on IBM i 7.5 for an SQL table and an SQL index over
//! it (PUB400 2026-10-27, tools/golden/verify8.txt c8_bas, c8_mbr,
//! c8_fmt, c8_acp and their values c8_*_vals). Values as4tainr does not
//! keep (wait times, sizes, operation counts...) are the captured ones
//! or zero (docs/fidelity-gaps.md).

use crate::catalog;
use crate::commands::{Job, Outcome};
use crate::outfile::Record;
use anyhow::Result;

/// The file a DSPFD outfile describes.
pub struct FileInfo {
    pub library: String,
    pub name: String,
    pub logical: bool,
    pub source: bool,
    pub sql_type: String,
    pub text: String,
    pub file: crate::dds::PhysicalFile,
    /// The physical file a logical file is over.
    pub based_on: Option<(String, String)>,
    pub select_omit: bool,
    /// (century, YYMMDD, HHMMSS): now, the file's creation, the
    /// member's last change.
    pub now: (String, String, String),
    pub created: (String, String, String),
    pub changed: (String, String, String),
    pub records: i64,
    /// Deleted records (record numbers no longer used).
    pub deleted: i64,
    pub journal: Option<(String, String, String)>,
    pub system: String,
}

/// The fields every one of these models begins with, under its prefix
/// (ATRCEN... ATASP for QAFDBASI).
fn head(names: [&'static str; 10], f: &FileInfo) -> Record {
    vec![
        (names[0], f.now.0.clone()),
        (names[1], f.now.1.clone()),
        (names[2], f.now.2.clone()),
        (names[3], f.name.clone()),
        (names[4], crate::qtemp::logical(&f.library)),
        (names[5], if f.logical { "L" } else { "P" }.into()),
        (names[6], if f.logical { "*LGL" } else { "*PHY" }.into()),
        (names[7], if f.logical { "LF" } else { "PF" }.into()),
        (names[8], f.system.clone()),
        (names[9], "1".into()),
    ]
}

fn keyed(f: &FileInfo) -> bool {
    !f.file.keys.is_empty()
}

fn reclen(f: &FileInfo) -> u32 {
    f.file.fields.iter().map(|x| x.buffer_bytes()).sum()
}

/// The SQL file type: T table, I index, V view; '0' for a file not made
/// by SQL (verify9 d9_bas_vals1, d9_blf_vals1).
fn sql_code(f: &FileInfo) -> &'static str {
    if f.file.view.is_some() {
        return "V";
    }
    match f.sql_type.as_str() {
        "TABLE" => "T",
        "INDEX" => "I",
        _ => "0",
    }
}

fn sql_made(f: &FileInfo) -> bool {
    sql_code(f) != "0"
}

/// The bytes of the key fields (MBMXKL; verify9 d9_mbr_vals: 3 for a
/// 5,0 packed key).
fn key_bytes(f: &FileInfo) -> u32 {
    f.file
        .keys
        .iter()
        .filter_map(|k| f.file.fields.iter().find(|x| &x.name == k))
        .map(|x| x.buffer_bytes())
        .sum()
}

/// QAFDBASI (c8_bas_vals1).
pub fn basatr(f: &FileInfo) -> Vec<Record> {
    let mut r = head(
        [
            "ATRCEN", "ATRDAT", "ATRTIM", "ATFILE", "ATLIB", "ATFTYP", "ATFILA", "ATFATR",
            "ATSYSN", "ATASP",
        ],
        f,
    );
    r.extend([
        ("ATDTAT", if f.source { "S" } else { "D" }.to_string()),
        ("ATWAIT", "30".into()),
        ("ATWATR", "60".into()),
        ("ATLVLC", "Y".into()),
        ("ATTXT", f.text.clone()),
        ("ATNOFM", "1".into()),
        ("ATFCCN", f.created.0.clone()),
        ("ATFCDT", f.created.1.clone()),
        ("ATFCTM", f.created.2.clone()),
        ("ATFLS", "Y".into()),
        ("ATICAP", "N".into()),
        ("ATACCP", if keyed(f) { "K" } else { "A" }.into()),
        ("ATSELO", if f.select_omit { "Y" } else { "N" }.into()),
        ("ATCSEQ", "N".into()),
        ("ATNOMB", "1".into()),
        ("ATJOIN", "N".into()),
        ("ATSQLT", sql_code(f).into()),
    ]);
    vec![r]
}

/// QAFDMBR (c8_mbr_vals1-3 for an SQL table, verify9 d9_mbr_vals1-3 for
/// a keyed DDS file): the member, named like the file.
pub fn member(f: &FileInfo) -> Vec<Record> {
    let mut r = head(
        [
            "MBRCEN", "MBRDAT", "MBRTIM", "MBFILE", "MBLIB", "MBFTYP", "MBFILA", "MBFATR",
            "MBSYSN", "MBASP",
        ],
        f,
    );
    r.extend([
        ("MBDTAT", if f.source { "S" } else { "D" }.to_string()),
        ("MBWAIT", "30".into()),
        ("MBWATR", "60".into()),
        ("MBLVLC", "Y".into()),
        ("MBTXT", f.text.clone()),
        ("MBNOFM", "1".into()),
        ("MBFCCN", f.created.0.clone()),
        ("MBFCDT", f.created.1.clone()),
        ("MBFCTM", f.created.2.clone()),
        ("MBFLS", "Y".into()),
        ("MBICAP", "N".into()),
        ("MBACCP", if keyed(f) { "K" } else { "A" }.into()),
        ("MBSELO", if f.select_omit { "Y" } else { "N" }.into()),
        ("MBCSEQ", "N".into()),
        ("MBNOMB", "1".into()),
        // The SQL file type is in MBRES4 in this model (as captured).
        ("MBRES4", sql_code(f).into()),
        ("MBNAME", f.name.clone()),
        ("MBCCEN", f.created.0.clone()),
        ("MBCDAT", f.created.1.clone()),
        ("MBCTIM", f.created.2.clone()),
        // EXPDATE(*NONE): '*' and 'NONE' in the century and date fields.
        ("MBECEN", "*".into()),
        ("MBEDAT", "NONE".into()),
        ("MBMAXM", "1".into()),
        ("MBMXRL", reclen(f).to_string()),
        ("MBJRNL", if f.journal.is_some() { "Y" } else { "N" }.into()),
        // SIZE: *NOMAX for an SQL table, the DDS default 10000 1000 3
        // (record capacity 13000) for a DDS file.
        ("MBSIZ", if sql_made(f) { "0" } else { "10000" }.into()),
        ("MBSIZI", if sql_made(f) { "0" } else { "1000" }.into()),
        ("MBSIZM", if sql_made(f) { "0" } else { "3" }.into()),
        (
            "MBRCDC",
            if sql_made(f) { "4294967288" } else { "13000" }.into(),
        ),
        ("MBNRCD", f.records.to_string()),
        ("MBNDTR", f.deleted.to_string()),
        ("MBALLO", "N".into()),
        ("MBCONT", "N".into()),
        ("MBRSHR", "N".into()),
        ("MBDSSZ", if sql_made(f) { "12288" } else { "8192" }.into()),
        ("MBDSZ2", if sql_made(f) { "12288" } else { "8192" }.into()),
        ("MBCHGC", f.changed.0.clone()),
        ("MBCHGD", f.changed.1.clone()),
        ("MBCHGT", f.changed.2.clone()),
        ("MBALRD", "Y".into()),
        ("MBALWT", "Y".into()),
        ("MBALUP", "Y".into()),
        ("MBALDT", "Y".into()),
        // Usage: the last change, used once (both captures: 1, today, 1).
        ("MBUCEN", f.changed.0.clone()),
        ("MBUDAT", f.changed.1.clone()),
        ("MBUCNT", "1".into()),
        ("MBAPBL", if keyed(f) { "2" } else { "1" }.into()),
        ("MBINTR", "0".into()),
        ("MBRBCK", "0".into()),
    ]);
    if keyed(f) {
        // A keyed access path (d9_mbr_vals): maintenance immediate,
        // recovery and force *NO, the key's bytes, its size (90112 as
        // captured), an entry and a key value per record.
        let k = key_bytes(f).to_string();
        r.extend([
            ("MBMANT", "I".to_string()),
            ("MBRECV", "N".into()),
            ("MBFKAP", "N".into()),
            ("MBMXKL", k.clone()),
            ("MBMXK2", k),
            ("MBINDX", "Y".into()),
            ("MBACPJ", "N".into()),
            ("MBISIZ", "90112".into()),
            ("MBISZ2", "90112".into()),
            ("MBIXNT", f.records.to_string()),
            ("MBUKV1", f.records.to_string()),
        ]);
    }
    if let Some((j, l, images)) = &f.journal {
        r.extend([
            ("MBJRNM", j.clone()),
            ("MBJRLB", l.clone()),
            ("MBJRIM", images.clone()),
        ]);
    }
    vec![r]
}

/// QAFDRFMT (c8_fmt_vals1): the record format.
pub fn rcdfmt(f: &FileInfo) -> Vec<Record> {
    let mut r = head(
        [
            "RFRCEN", "RFRDAT", "RFRTIM", "RFFILE", "RFLIB", "RFFTYP", "RFFILA", "RFFATR",
            "RFSYSN", "RFASP",
        ],
        f,
    );
    r.extend([
        ("RFTOTF", "1".to_string()),
        ("RFNAME", f.file.format.clone()),
        ("RFFLDN", f.file.fields.len().to_string()),
        ("RFLEN", reclen(f).to_string()),
        (
            "RFID",
            crate::objdesc::level_id(&f.file.format, &f.file.fields),
        ),
        ("RFTYPE", "1".into()),
        ("RFPGRT", "-1".into()),
    ]);
    vec![r]
}

/// QAFDACCP (c8_acp_vals1 for an SQL index; verify9 d9_acp_vals1 for
/// a keyed DDS physical file, a DDS logical file and a table in arrival
/// sequence): a record per key field, numbered from 1, or one without a
/// key; the based-on file only for a logical file.
pub fn accpth(f: &FileInfo) -> Vec<Record> {
    let base = |r: &mut Record| {
        r.extend([
            ("APSELO", if f.select_omit { "Y" } else { "N" }.to_string()),
            ("APACCP", if keyed(f) { "K" } else { "A" }.into()),
            ("APJOIN", "N".into()),
            ("APNKYF", f.file.keys.len().to_string()),
        ]);
        if keyed(f) {
            r.extend([
                ("APMANT", "I".to_string()),
                ("APUNIQ", if f.file.unique { "Y" } else { "N" }.into()),
                ("APKEYO", "N".into()),
                ("APACPJ", "N".into()),
            ]);
        }
        match &f.based_on {
            Some((bl, bf)) => r.extend([
                ("APNSCO", "1".to_string()),
                ("APNSC2", "1".into()),
                ("APBOF", bf.clone()),
                ("APBOL", crate::qtemp::logical(bl)),
                ("APBOLF", f.file.format.clone()),
            ]),
            None => r.extend([("APNSCO", "0".to_string()), ("APNSC2", "0".into())]),
        }
    };
    let names = [
        "APRCEN", "APRDAT", "APRTIM", "APFILE", "APLIB", "APFTYP", "APFILA", "APFATR", "APSYSN",
        "APASP",
    ];
    if f.file.keys.is_empty() {
        let mut r = head(names, f);
        base(&mut r);
        return vec![r];
    }
    f.file
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let mut r = head(names, f);
            base(&mut r);
            r.extend([
                ("APKEYF", k.clone()),
                (
                    "APKSEQ",
                    if f.file.descending.contains(k) {
                        "D"
                    } else {
                        "A"
                    }
                    .to_string(),
                ),
                // A numeric key is signed (S; d9_acp_vals1), a character
                // key N.
                (
                    "APKSIN",
                    if f.file
                        .fields
                        .iter()
                        .find(|x| &x.name == k)
                        .is_some_and(|x| x.data_type.is_numeric())
                    {
                        "S"
                    } else {
                        "N"
                    }
                    .into(),
                ),
                ("APKZD", "N".into()),
                ("APKASQ", "N".into()),
                ("APKEYN", (i + 1).to_string()),
            ]);
            r
        })
        .collect()
}

impl Job<'_> {
    /// What the outfiles need to know about a file.
    async fn file_info(&self, lib: &str, name: &str, attr: &str) -> Result<FileInfo> {
        let stamp = |col: &str| {
            format!(
                "SELECT CASE WHEN t < '2000-01-01' THEN '0' ELSE '1' END, to_char(t, 'YYMMDD'), to_char(t, 'HH24MISS')
                   FROM (SELECT ({col}) AT TIME ZONE 'UTC' AS t FROM as4tainr.objects
                          WHERE library=$1 AND name=$2 AND objtype='*FILE') x"
            )
        };
        let three = |r: tokio_postgres::Row| (r.get(0), r.get(1), r.get(2));
        let now = three(
            self.client
                .query_one(&stamp("now()"), &[&lib, &name])
                .await?,
        );
        let created = three(
            self.client
                .query_one(&stamp("created"), &[&lib, &name])
                .await?,
        );
        let changed = three(
            self.client
                .query_one(&stamp("coalesce(changed, created)"), &[&lib, &name])
                .await?,
        );
        let o = self
            .client
            .query_one(
                "SELECT o.text, coalesce(f.sql_type, '') FROM as4tainr.objects o
                   LEFT JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name
                  WHERE o.library=$1 AND o.name=$2 AND o.objtype='*FILE'",
                &[&lib, &name],
            )
            .await?;
        let source = crate::source::is_source_file(self.client, lib, name).await?;
        let fr = if source {
            let len = crate::source::record_length(self.client, lib, name).await?;
            catalog::FileRef {
                library: lib.to_string(),
                file: crate::source::record_file(name, len),
            }
        } else {
            catalog::resolve_file(self.client, &[lib.to_string()], name).await?
        };
        let lf = self
            .client
            .query_opt(
                "SELECT pf_library, pf_name, selection FROM as4tainr.logical_files WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        let (records, deleted): (i64, i64) = if source {
            (0, 0)
        } else {
            self.client
                .query_one(
                    &format!(
                        "SELECT count(*), coalesce(max(rrn), 0) - count(*) FROM {}",
                        fr.table()
                    ),
                    &[],
                )
                .await
                .map(|r| (r.get(0), r.get(1)))
                .unwrap_or((0, 0))
        };
        let journal = self
            .client
            .query_opt(
                "SELECT journal, journal_lib, images FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
                &[&lib, &name],
            )
            .await?
            .map(|j| {
                let images: String = j.get(2);
                (
                    j.get::<_, String>(0),
                    j.get::<_, String>(1),
                    if images.eq_ignore_ascii_case("*BOTH") {
                        "B"
                    } else {
                        "A"
                    }
                    .to_string(),
                )
            });
        Ok(FileInfo {
            library: lib.to_string(),
            name: name.to_string(),
            logical: attr.starts_with("LF"),
            source,
            sql_type: o.get(1),
            text: o.get(0),
            select_omit: lf
                .as_ref()
                .is_some_and(|r| r.get::<_, String>(2).trim() != "TRUE"),
            based_on: lf.map(|r| (r.get(0), r.get(1))),
            file: fr.file,
            now,
            created,
            changed,
            records,
            deleted,
            journal,
            system: crate::report::system_name().chars().take(8).collect(),
        })
    }

    /// DSPFD TYPE(*BASATR | *MBR | *RCDFMT | *ACCPTH) OUTPUT(*OUTFILE):
    /// CPF9861/CPF9862 when the file is made, CPF3030 each time
    /// (c8_bas, c8_mbr, c8_fmt, c8_acp).
    pub(crate) async fn dspfd_type_outfile(
        &mut self,
        ty: &str,
        outfile: &str,
        option: &str,
        lib: &str,
        name: &str,
        attr: &str,
    ) -> Result<Outcome> {
        let f = self.file_info(lib, name, attr).await?;
        let (model, records) = match ty {
            "*BASATR" => (&crate::outfile::QAFDBASI, basatr(&f)),
            "*MBR" => (&crate::outfile::QAFDMBR, member(&f)),
            "*RCDFMT" => (&crate::outfile::QAFDRFMT, rcdfmt(&f)),
            _ => (&crate::outfile::QAFDACCP, accpth(&f)),
        };
        let w = crate::outfile::write(self, outfile, option, model, &records).await?;
        let mut messages = w.messages.clone();
        messages.push(crate::outfile::records_added(&w, records.len()));
        Ok(Outcome::Messages(messages))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> FileInfo {
        let pf = crate::dds::parse_pf(
            "GLD6TB",
            "     A          R GLD6TB\n     A            ID            10A\n     A            VAL           60A\n",
        )
        .unwrap();
        FileInfo {
            library: "AS4TRAINR1".into(),
            name: "GLD6TB".into(),
            logical: false,
            source: false,
            sql_type: "TABLE".into(),
            text: String::new(),
            file: pf,
            based_on: None,
            select_omit: false,
            now: ("1".into(), "261027".into(), "202822".into()),
            created: ("1".into(), "261027".into(), "202716".into()),
            changed: ("1".into(), "261027".into(), "202716".into()),
            records: 0,
            deleted: 0,
            journal: None,
            system: "PUB400".into(),
        }
    }

    fn get(r: &Record, k: &str) -> String {
        r.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    // Expected values: PUB400 (IBM i 7.5) 2026-10-27, verify8
    // c8_bas_vals1, c8_mbr_vals1-3, c8_fmt_vals1 (an SQL table of two
    // character fields, 70 bytes).
    #[test]
    fn table_values() {
        let f = info();
        let b = &basatr(&f)[0];
        for (k, v) in [
            ("ATFILE", "GLD6TB"),
            ("ATFTYP", "P"),
            ("ATFILA", "*PHY"),
            ("ATFATR", "PF"),
            ("ATSYSN", "PUB400"),
            ("ATASP", "1"),
            ("ATDTAT", "D"),
            ("ATWAIT", "30"),
            ("ATWATR", "60"),
            ("ATLVLC", "Y"),
            ("ATFCDT", "261027"),
            ("ATFCTM", "202716"),
            ("ATACCP", "A"),
            ("ATSELO", "N"),
            ("ATNOMB", "1"),
            ("ATJOIN", "N"),
            ("ATSQLT", "T"),
        ] {
            assert_eq!(get(b, k), v, "{k}");
        }
        let m = &member(&f)[0];
        for (k, v) in [
            ("MBNAME", "GLD6TB"),
            ("MBRES4", "T"),
            ("MBECEN", "*"),
            ("MBEDAT", "NONE"),
            ("MBMAXM", "1"),
            ("MBMXRL", "70"),
            ("MBJRNL", "N"),
            ("MBRCDC", "4294967288"),
            ("MBNRCD", "0"),
            ("MBDSSZ", "12288"),
            ("MBCHGD", "261027"),
            ("MBALRD", "Y"),
            ("MBAPBL", "1"),
        ] {
            assert_eq!(get(m, k), v, "{k}");
        }
        let r = &rcdfmt(&f)[0];
        for (k, v) in [
            ("RFNAME", "GLD6TB"),
            ("RFFLDN", "2"),
            ("RFLEN", "70"),
            ("RFTYPE", "1"),
            ("RFPGRT", "-1"),
            ("RFTOTF", "1"),
        ] {
            assert_eq!(get(r, k), v, "{k}");
        }
    }

    // Expected values: PUB400 2026-10-27, verify9 d9_bas_vals1,
    // d9_mbr_vals1-3, d9_acp_vals1: a DDS physical file keyed on CODE
    // 5P 0 with two records (one more deleted), and an SQL table in
    // arrival sequence.
    #[test]
    fn dds_values() {
        let mut f = info();
        f.name = "GLD9PF".into();
        f.sql_type = String::new();
        f.file = crate::dds::parse_pf(
            "GLD9PF",
            "     A          R GLD9R\n     A            CODE           5P 0\n     A            NAME          20A\n     A          K CODE\n",
        )
        .unwrap();
        f.records = 2;
        f.deleted = 1;
        assert_eq!(get(&basatr(&f)[0], "ATSQLT"), "0");
        assert_eq!(get(&basatr(&f)[0], "ATACCP"), "K");
        let m = &member(&f)[0];
        for (k, v) in [
            ("MBRES4", "0"),
            ("MBMANT", "I"),
            ("MBRECV", "N"),
            ("MBFKAP", "N"),
            ("MBMXKL", "3"),
            ("MBSIZ", "10000"),
            ("MBSIZI", "1000"),
            ("MBSIZM", "3"),
            ("MBRCDC", "13000"),
            ("MBNRCD", "2"),
            ("MBNDTR", "1"),
            ("MBDSSZ", "8192"),
            ("MBISIZ", "90112"),
            ("MBIXNT", "2"),
            ("MBACPJ", "N"),
            ("MBUCNT", "1"),
            ("MBINDX", "Y"),
            ("MBDSZ2", "8192"),
            ("MBMXK2", "3"),
            ("MBAPBL", "2"),
            ("MBISZ2", "90112"),
            ("MBUKV1", "2"),
            ("MBINTR", "0"),
            ("MBRBCK", "0"),
        ] {
            assert_eq!(get(m, k), v, "{k}");
        }
        let a = &accpth(&f)[0];
        for (k, v) in [
            ("APMANT", "I"),
            ("APNSCO", "0"),
            ("APBOF", ""),
            ("APBOLF", ""),
            ("APKEYF", "CODE"),
            ("APKSIN", "S"),
            ("APACPJ", "N"),
            ("APNSC2", "0"),
        ] {
            assert_eq!(get(a, k), v, "{k}");
        }
        let t = info();
        let a = &accpth(&t)[0];
        for (k, v) in [
            ("APMANT", ""),
            ("APUNIQ", ""),
            ("APKEYO", ""),
            ("APACCP", "A"),
            ("APNSCO", "0"),
            ("APNKYF", "0"),
            ("APKEYN", ""),
            ("APJOIN", "N"),
            ("APACPJ", ""),
            ("APNSC2", "0"),
        ] {
            assert_eq!(get(a, k), v, "{k}");
        }
    }

    // c8_acp_vals1: an SQL index GLD7IX over GLD6TB keyed on ID.
    #[test]
    fn index_access_path() {
        let mut f = info();
        f.name = "GLD7IX".into();
        f.logical = true;
        f.sql_type = "INDEX".into();
        f.file.keys = vec!["ID".into()];
        f.based_on = Some(("AS4TRAINR1".into(), "GLD6TB".into()));
        let a = accpth(&f);
        assert_eq!(a.len(), 1);
        for (k, v) in [
            ("APFILE", "GLD7IX"),
            ("APFTYP", "L"),
            ("APFILA", "*LGL"),
            ("APFATR", "LF"),
            ("APMANT", "I"),
            ("APUNIQ", "N"),
            ("APKEYO", "N"),
            ("APSELO", "N"),
            ("APACCP", "K"),
            ("APNSCO", "1"),
            ("APBOF", "GLD6TB"),
            ("APBOL", "AS4TRAINR1"),
            ("APBOLF", "GLD6TB"),
            ("APNKYF", "1"),
            ("APKEYF", "ID"),
            ("APKSEQ", "A"),
            ("APKSIN", "N"),
            ("APKZD", "N"),
            ("APKASQ", "N"),
            ("APKEYN", "1"),
            ("APJOIN", "N"),
            ("APACPJ", "N"),
            ("APNSC2", "1"),
        ] {
            assert_eq!(get(&a[0], k), v, "{k}");
        }
    }
}
