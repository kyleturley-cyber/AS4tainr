//! CPYF: copy the records of a database file to another file or to a
//! printed listing (TOFILE(*PRINT)).

use crate::catalog;
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use crate::rla;
use anyhow::{anyhow, bail, Result};

impl Job<'_> {
    /// CPYF FROMFILE TOFILE MBROPT CRTFILE FMTOPT (CL reference, CPYF):
    /// the records in the from-file's order (keyed for a keyed logical
    /// file, else arrival), added to the to-file (MBROPT(*ADD)), in place
    /// of its records (*REPLACE), or only into an empty member (*NONE,
    /// the default). CRTFILE(*YES) creates a to-file that is not there
    /// with the from-file's fields. Fields go by name: FMTOPT(*NONE) needs
    /// the same fields in both files, *MAP and *DROP take the fields of the
    /// same name, *NOCHK goes by position. TOFILE(*PRINT) prints the
    /// records. The completion message is CPC2955 with the number of
    /// records copied (observed on IBM i 7.5); a copy that cannot be done
    /// ends with CPF2817.
    pub(crate) async fn cpyf(&mut self, cmd: &Command) -> Result<Outcome> {
        let from_q = cmd.get("FROMFILE").to_ascii_uppercase();
        let (libs, from_name) = self.search(&from_q);
        let from_label = from_q.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        // A source file: its member's records, SRCSEQ, SRCDAT and SRCDTA
        // (PUB400 2026-10-27, tools/golden/verify3.txt x_src: CPYF of a
        // source member to *PRINT).
        let mut source_lib = None;
        for l in &libs {
            if crate::source::is_source_file(self.client, l, &from_name).await? {
                source_lib = Some(l.clone());
                break;
            }
        }
        let mut source_records: Option<Vec<rla::Record>> = None;
        let from = if let Some(l) = source_lib {
            let mbr = match cmd.get("FROMMBR").to_ascii_uppercase().as_str() {
                "" | "*FIRST" => self
                    .client
                    .query_opt(
                        "SELECT member FROM as4tainr.source_members WHERE library=$1 AND file=$2
                          ORDER BY added, member LIMIT 1",
                        &[&l, &from_name],
                    )
                    .await?
                    .map(|r| r.get::<_, String>(0))
                    .ok_or_else(|| {
                        anyhow!(
                            "CPF4102: File {from_name} in library {l} has no members.\nCPF2817: The copy was not done."
                        )
                    })?,
                m => m.to_string(),
            };
            let m =
                crate::source::find_member(self.client, std::slice::from_ref(&l), &from_name, &mbr)
                    .await?;
            // Reading a member is a use of it (source::note_used).
            crate::source::note_used(self.client, &l, &from_name, &mbr).await;
            let rcdlen = crate::source::record_length(self.client, &l, &from_name).await?;
            let pf = crate::source::record_file(&from_name, rcdlen);
            let width = pf.fields[2].length as usize;
            source_records = Some(
                m.source
                    .lines()
                    .enumerate()
                    .map(|(i, line)| {
                        rla::Record::from([
                            ("SRCSEQ".to_string(), format!("{}.00", (i + 1) % 10000)),
                            ("SRCDAT".to_string(), "0".to_string()),
                            ("SRCDTA".to_string(), line.chars().take(width).collect()),
                        ])
                    })
                    .collect(),
            );
            catalog::FileRef {
                library: l,
                file: pf,
            }
        } else {
            catalog::resolve_file(self.client, &libs, &from_name)
                .await
                .map_err(|_| {
                    anyhow!(
                        "CPF2802: From-file {from_name} in library {from_label} does not exist.\nCPF2817: The copy was not done."
                    )
                })?
        };
        crate::authority::check(
            self.client,
            &self.user,
            &from.library,
            &from_name,
            "*FILE",
            crate::authority::Aut::Use,
        )
        .await?;
        let to_q = cmd.get("TOFILE").to_ascii_uppercase();
        let mut records = Vec::new();
        let mut rrns: Vec<i64> = Vec::new();
        if let Some(r) = source_records {
            rrns = (1..=r.len() as i64).collect();
            records = r;
        } else {
            let mut reader = rla::OpenFile::new(from.clone());
            loop {
                let r = reader.read(self.client).await?;
                if r.eof {
                    break;
                }
                if let Some(rec) = r.record {
                    rrns.push(r.rrn.unwrap_or(rrns.len() as i64 + 1));
                    records.push(rec);
                }
            }
        }
        let fields: Vec<String> = from.file.fields.iter().map(|f| f.name.clone()).collect();
        let copied = format!(
            "CPC2955: {} records were copied from member {from_name}.",
            records.len()
        );
        if to_q == "*PRINT" {
            let attr: Option<String> = self
                .client
                .query_opt(
                    "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[&from.library, &from_name],
                )
                .await?
                .map(|r| r.get(0));
            let logical = attr.as_deref().is_some_and(|a| a.starts_with("LF"));
            let member = match cmd.get("FROMMBR").to_ascii_uppercase().as_str() {
                "" | "*FIRST" | "*ALL" => from_name.clone(),
                m => m.to_string(),
            };
            let images: Vec<(i64, Vec<u8>)> = records
                .iter()
                .zip(&rrns)
                .map(|(r, n)| (*n, record_image(&from.file, r)))
                .collect();
            let hex = cmd.get("OUTFMT").eq_ignore_ascii_case("*HEX");
            let print = print_listing(
                &from.library,
                &from_name,
                &member,
                &from.file.format,
                record_length(&from.file),
                logical,
                hex,
                &images,
                &crate::report::Stamp::now(),
            );
            let copied = if records.is_empty() {
                // CPC2957 (PUB400 2026-10-27, verify5 z_cpysplf_2f).
                format!(
                    "CPC2957: No records were copied from file {from_name} in library {}.",
                    from.library
                )
            } else {
                copied
            };
            let mut all = vec![copied];
            all.extend(print);
            let rows: Vec<String> = images
                .iter()
                .map(|(_, l)| printable(l).trim_end().to_string())
                .collect();
            return Ok(Outcome::Display(ListDisplay {
                title: format!("Copy File {}/{from_name}", from.library),
                info: vec![
                    ("From file".into(), format!("{}/{from_name}", from.library)),
                    ("Record format".into(), from.file.format.clone()),
                    ("Records copied".into(), records.len().to_string()),
                ],
                heading: String::new(),
                rows,
                print: all,
                ..Default::default()
            }));
        }
        let (to_libs, to_name) = self.search(&to_q);
        let to_label = to_q
            .split_once('/')
            .map(|x| x.0)
            .unwrap_or("*LIBL")
            .to_string();
        // Messages before CPC2955 (observed on IBM i 7.5: CPF2880 and
        // CPF2889 when CRTFILE(*YES) creates the to-file, CPF4011 when the
        // from-records are shorter).
        let mut infos = Vec::new();
        let mut created = false;
        let to = match catalog::resolve_file(self.client, &to_libs, &to_name).await {
            Ok(t) => t,
            Err(_) if cmd.get("CRTFILE").eq_ignore_ascii_case("*YES") => {
                let lib = if to_label.starts_with('*') {
                    from.library.clone()
                } else {
                    to_label.clone()
                };
                let mut pf = from.file.clone();
                pf.name = to_name.clone();
                pf.view = None;
                let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
                catalog::create_physical_file(&mut c, &lib, &pf).await?;
                crate::authority::set_owner(self.client, &lib, &to_name, "*FILE", &self.user).await?;
                infos.push(format!(
                    "CPF2880: Physical file {to_name} was created in library {lib}."
                ));
                infos.push(format!(
                    "CPF2889: Member {from_name} was added to file {to_name} in library {lib}."
                ));
                created = true;
                catalog::resolve_file(self.client, std::slice::from_ref(&lib), &to_name).await?
            }
            Err(_) => bail!(
                "CPF2861: To-file {to_name} in library {to_label} does not exist, and CRTFILE(*NO) does not create it.\nCPF2817: The copy was not done."
            ),
        };
        crate::authority::check(
            self.client,
            &self.user,
            &to.library,
            &to_name,
            "*FILE",
            crate::authority::Aut::Change,
        )
        .await?;
        let to_fields: Vec<String> = to.file.fields.iter().map(|f| f.name.clone()).collect();
        let fmtopt = cmd.list("FMTOPT").join(" ").to_ascii_uppercase();
        let positional = fmtopt.contains("*NOCHK");
        if !positional && !fmtopt.contains("*MAP") && !fmtopt.contains("*DROP") {
            let mut a = fields.clone();
            let mut b = to_fields.clone();
            a.sort();
            b.sort();
            if a != b {
                bail!(
                    "CPF2963: The record formats of {from_name} and {to_name} differ; FMTOPT(*MAP *DROP) or FMTOPT(*NOCHK) copies them.\nCPF2817: The copy was not done."
                );
            }
        }
        let table = to.table();
        let n_before: i64 = self
            .client
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await?
            .get(0);
        match cmd.get("MBROPT").to_ascii_uppercase().as_str() {
            "*REPLACE" => {
                self.client
                    .batch_execute(&format!("DELETE FROM {table}"))
                    .await?;
            }
            "*ADD" => {}
            // MBROPT(*NONE): only into a to-file this copy creates.
            _ if !created => bail!(
                "CPF2863: File {to_name} exists; MBROPT(*ADD) or MBROPT(*REPLACE) copies into it.\nCPF2817: The copy was not done."
            ),
            _ => {}
        }
        let _ = n_before;
        if positional {
            let len = |f: &crate::dds::PhysicalFile| -> usize {
                f.fields
                    .iter()
                    .filter_map(|x| crate::rpg::program::program_type(x, true, false))
                    .map(|t| t.size())
                    .sum()
            };
            // A from-record shorter than the to-file's record: CPF4011
            // (observed on IBM i 7.5, a 23-byte record into RCDLEN(30)).
            if len(&from.file) < len(&to.file) {
                infos.push(format!(
                    "CPF4011: The records of member {from_name} are shorter than the records of file {to_name}."
                ));
            }
        }
        let mut writer = rla::OpenFile::new(to.clone());
        for r in &records {
            let rec: rla::Record = if positional {
                to_fields
                    .iter()
                    .zip(&fields)
                    .filter_map(|(t, f)| r.get(f).map(|v| (t.clone(), v.clone())))
                    .collect()
            } else {
                r.iter()
                    .filter(|(k, _)| to_fields.contains(k) || k.as_str() == rla::NULLS)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            writer.write(self.client, &rec).await?;
        }
        infos.push(copied);
        Ok(Outcome::Messages(infos))
    }
}

/// A record's bytes in its record format (EBCDIC characters, zoned,
/// packed and binary numbers), as a program or CPYF sees the buffer.
pub fn record_image(f: &crate::dds::PhysicalFile, rec: &rla::Record) -> Vec<u8> {
    use crate::rpg::data::{self, Fit, Value};
    let mut img = Vec::new();
    for fd in &f.fields {
        let Some(t) = crate::rpg::program::program_type(fd, true, false) else {
            continue;
        };
        let mut b = vec![data::BLANK; t.size()];
        if let Some(text) = rec.get(&fd.name) {
            let v = if t.is_numeric() {
                crate::rpg::dec::Dec::parse(text.trim())
                    .map(Value::Num)
                    .ok()
            } else {
                Some(Value::str(text))
            };
            if let Some(v) = v {
                let _ = data::store(&t, &v, &mut b, Fit::Truncate { half_adjust: false }, false);
            }
        }
        img.extend(b);
    }
    img
}

/// The record length of a record format (the bytes of its fields).
pub fn record_length(f: &crate::dds::PhysicalFile) -> usize {
    f.fields
        .iter()
        .filter_map(|x| crate::rpg::program::program_type(x, true, false))
        .map(|t| t.size())
        .sum()
}

/// A record image as CPYF OUTFMT(*CHAR) prints it: its EBCDIC characters,
/// the bytes that are not printable characters (below X'40', X'FF') as
/// blanks (PUB400 2026-10-22, files-and-programs f_cpyf_print: a packed
/// key printed as blanks).
pub fn printable(img: &[u8]) -> String {
    img.iter()
        .map(|&b| {
            if b < 0x40 || b == 0xFF {
                ' '
            } else {
                crate::tn5250::ebcdic::decode(&[b])
                    .chars()
                    .next()
                    .unwrap_or(' ')
            }
        })
        .collect()
}

/// CPYF TOFILE(*PRINT)'s listing in IBM i 7.5's layout (PUB400
/// 2026-10-22 files-and-programs f_cpyf_print, rpg-sql v_print,
/// 2026-10-27 verify5 z_cpysplf_2f): a page header, the from-file, its
/// member, record format and record length, the to-file; a ruler; each
/// record's number (with the member number for a logical file) and its
/// characters; the count line and the end line. 30 lines a page, the
/// header and ruler repeated (v_print). The product line is as4tainr's.
/// OUTFMT(*HEX): under each record's characters, a line of each byte's
/// zone (high) digit and a line of its low digit (PUB400 2026-10-27,
/// verify7 c7_hex).
#[allow(clippy::too_many_arguments)]
pub fn print_listing(
    lib: &str,
    file: &str,
    member: &str,
    format: &str,
    reclen: usize,
    logical: bool,
    hex: bool,
    images: &[(i64, Vec<u8>)],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    const PAGE: usize = 30;
    let qualified = format!("{lib}/{file}");
    let header = |page: usize| {
        format!(
            " {}  {}{:9}COPY FILE{:>34}  {member:<10}{:11}{} {}  Page{:>10}",
            crate::report::PRODUCT,
            crate::report::LEVEL,
            "",
            qualified,
            "",
            stamp.date,
            stamp.time,
            page
        )
    };
    let ruler = format!(
        " {}    RCDNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+... 0",
        if logical { "MBRNBR" } else { "      " }
    );
    let mut out = vec![
        header(1),
        format!(
            " From file . . . . . :  {qualified:<26}Member  . . :  {member:<20}Record format . . . . . :  {format}"
        ),
        format!(" Record length . . . :  {reclen}"),
        " To file . . . . . . :  *PRINT".to_string(),
        ruler.clone(),
    ];
    let mut page = 1;
    let mut on_page = out.len();
    let need = if hex { 3 } else { 1 };
    for (n, img) in images {
        if on_page + need > PAGE {
            page += 1;
            out.push(header(page));
            out.push(ruler.clone());
            on_page = 2;
        }
        let mbr = if logical { "1" } else { "" };
        let text = printable(img);
        out.push(format!("{mbr:>5}{n:>12}  {text}").trim_end().to_string());
        if hex {
            let nib = |f: fn(u8) -> u8| -> String {
                img.iter()
                    .map(|&b| char::from_digit(f(b) as u32, 16).unwrap_or('0'))
                    .collect::<String>()
                    .to_ascii_uppercase()
            };
            out.push(format!("{:19}{}", "", nib(|b| b >> 4)));
            out.push(format!("{:19}{}", "", nib(|b| b & 0x0F)));
        }
        on_page += need;
    }
    out.push(format!(
        " {} records copied to member or label *N in file QSYSPRT in library QSYS. 0 records excluded.",
        images.len()
    ));
    out.push(format!(
        "{:29}* * * * *   E N D   O F   C O M P U T E R   P R I N T O U T   * * * * *",
        ""
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected lines: PUB400 (IBM i 7.5) 2026-10-22, files-and-programs
    // f_cpyf_print (a keyed logical file over CODE 5P 0, NAME 20A; the
    // product line is as4tainr's) and rpg-sql v_print (30 lines a page).
    #[test]
    fn print_layout() {
        let stamp = crate::report::Stamp {
            date: "26-10-22".into(),
            time: "01:29:12".into(),
            zone: String::new(),
        };
        let img = |s: &str| crate::tn5250::ebcdic::encode(&format!("   {s:<20}"));
        let l = print_listing(
            "AS4TRAINR1",
            "GOLDLF",
            "GOLDLF",
            "GOLDR",
            23,
            true,
            false,
            &[(2, img("Alpha")), (1, img("Beta"))],
            &stamp,
        );
        assert_eq!(
            l[0],
            format!(
                " {}  {}         COPY FILE                 AS4TRAINR1/GOLDLF  GOLDLF               26-10-22 01:29:12  Page         1",
                crate::report::PRODUCT,
                crate::report::LEVEL
            )
        );
        assert_eq!(l[1], " From file . . . . . :  AS4TRAINR1/GOLDLF         Member  . . :  GOLDLF              Record format . . . . . :  GOLDR");
        assert_eq!(l[2], " Record length . . . :  23");
        assert_eq!(l[3], " To file . . . . . . :  *PRINT");
        assert_eq!(l[4], " MBRNBR    RCDNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+... 0");
        assert_eq!(l[5], "    1           2     Alpha");
        assert_eq!(l[7], " 2 records copied to member or label *N in file QSYSPRT in library QSYS. 0 records excluded.");
        assert_eq!(l[8], "                             * * * * *   E N D   O F   C O M P U T E R   P R I N T O U T   * * * * *");
        let many: Vec<(i64, Vec<u8>)> = (1..=30)
            .map(|n| (n, crate::tn5250::ebcdic::encode("x")))
            .collect();
        let l = print_listing("L", "F", "F", "F", 1, false, false, &many, &stamp);
        // Page 1: header, 3 lines, ruler, 25 records; page 2: header,
        // ruler, the next records (v_print).
        assert_eq!(l[4], "           RCDNBR  *...+... 1 ...+... 2 ...+... 3 ...+... 4 ...+... 5 ...+... 6 ...+... 7 ...+... 8 ...+... 9 ...+... 0");
        assert_eq!(l[5], "                1  x");
        assert_eq!(l[29], "               25  x");
        assert!(l[30].ends_with("Page         2"));
        assert_eq!(l[32], "               26  x");
    }

    // OUTFMT(*HEX): PUB400 2026-10-27, verify7 c7_hex (ID CHAR(4), N
    // DECIMAL(5,0): 'AB', 123 and 'Z9', -45).
    #[test]
    fn hex_lines() {
        let pf = crate::dds::parse_pf(
            "GLD7HX",
            "     A          R GLD7HX\n     A            ID             4A\n     A            N              5P 0\n",
        )
        .unwrap();
        let rec = |id: &str, n: &str| {
            rla::Record::from([
                ("ID".to_string(), id.to_string()),
                ("N".to_string(), n.to_string()),
            ])
        };
        let stamp = crate::report::Stamp::default();
        let l = print_listing(
            "AS4TRAINR1",
            "GLD7HX",
            "GLD7HX",
            "GLD7HX",
            7,
            false,
            true,
            &[
                (1, record_image(&pf, &rec("AB", "123"))),
                (2, record_image(&pf, &rec("Z9", "-45"))),
            ],
            &stamp,
        );
        assert_eq!(
            &l[5..11],
            [
                "                1  AB",
                "                   CC44013",
                "                   120002F",
                "                2  Z9    )",
                "                   EF44005",
                "                   990004D",
            ]
        );
    }

    // A packed field's bytes print as blanks, characters as themselves
    // (f_cpyf_print: CODE 5P 0 then NAME).
    #[test]
    fn packed_bytes_print_as_blanks() {
        let pf = crate::dds::parse_pf(
            "GOLDPF",
            "     A          R GOLDR\n     A            CODE           5P 0\n     A            NAME          20A\n",
        )
        .unwrap();
        let rec = rla::Record::from([
            ("CODE".to_string(), "2".to_string()),
            ("NAME".to_string(), "Alpha".to_string()),
        ]);
        let img = record_image(&pf, &rec);
        assert_eq!(img.len(), 23);
        assert_eq!(printable(&img).trim_end(), "   Alpha");
        assert_eq!(record_length(&pf), 23);
    }
}
