//! Commands that look at files without changing them: DSPFD and DSPFFD
//! (and the dispatch of CPYF, in `cpyf.rs`). CHKOBJ, DSPOBJD and DSPSYSVAL
//! are in `commands.rs`.

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

impl Job<'_> {
    pub(crate) async fn objdesc_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "DSPFD" => self.dspfd(cmd).await?,
            "DSPFFD" => self.dspffd(cmd).await?,
            "CPYF" => self.cpyf(cmd).await?,
            _ => return Ok(None),
        }))
    }

    /// DSPFD (CL reference): TYPE(*ALL), the default, gives the
    /// attributes then the members; *ATR the attributes; *MBRLIST the
    /// members; *ACCPTH the key fields. Values as4tainr does not keep are
    /// CRTPF's defaults (SIZE(10000 1000 3), MAXMBRS(1), WAITFILE(*IMMED),
    /// WAITRCD(60), REUSEDLT(*NO), LVLCHK(*YES), SRTSEQ(*HEX)).
    async fn dspfd(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("FILE").to_ascii_uppercase();
        let (libs, name) = self.search(&q);
        let label = q
            .split_once('/')
            .map(|x| x.0)
            .unwrap_or("*LIBL")
            .to_string();
        let mut found = None;
        for l in &libs {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute, text, to_char(created, 'YYYY-MM-DD') FROM as4tainr.objects
                      WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[l, &name],
                )
                .await?
            {
                found = Some((
                    l.clone(),
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                ));
                break;
            }
        }
        let Some((lib, attr, text, created)) = found else {
            bail!("CPF9812: File {name} in library {label} not found.");
        };
        authority::check(self.client, &self.user, &lib, &name, "*FILE", Aut::Use).await?;
        let ty = cmd.get("TYPE").to_ascii_uppercase();
        if cmd.get("OUTPUT").eq_ignore_ascii_case("*OUTFILE") {
            if ty != "*MBRLIST" {
                return Err(crate::cl::value_error(&format!(
                    "OUTPUT(*OUTFILE) is done for TYPE(*MBRLIST) only, not TYPE({ty})."
                )));
            }
            return self.dspfd_mbrlist_outfile(cmd, &lib, &name, &attr).await;
        }
        // A data physical file's attributes: the printed report.
        if ty == "*ATR"
            && attr == "PF"
            && !crate::source::is_source_file(self.client, &lib, &name).await?
        {
            return self.dspfd_atr(cmd).await;
        }
        let source = crate::source::is_source_file(self.client, &lib, &name).await?;
        let members: Vec<(String, String, String)> = if source {
            crate::source::members(self.client, &lib, &name).await?
        } else if attr == "PF" || attr == "LF" {
            vec![(name.clone(), String::new(), text.clone())]
        } else {
            vec![]
        };
        let mut rows = Vec::new();
        let kv = |rows: &mut Vec<String>, k: &str, v: &str| rows.push(format!("{k:<36} {v}"));
        if ty == "*ALL" || ty == "*ATR" {
            let client = self.client;
            let count = |sql: &'static str| {
                let (lib, name) = (lib.clone(), name.clone());
                async move {
                    let n: i64 = client.query_one(sql, &[&lib, &name]).await?.get(0);
                    Ok::<i64, anyhow::Error>(n)
                }
            };
            let constraints =
                count("SELECT count(*) FROM as4tainr.constraints WHERE library=$1 AND file=$2")
                    .await?;
            let triggers =
                count("SELECT count(*) FROM as4tainr.file_triggers WHERE library=$1 AND file=$2")
                    .await?
                    + count(
                        "SELECT count(*) FROM as4tainr.sql_triggers WHERE file_lib=$1 AND file=$2",
                    )
                    .await?;
            let journaled =
                count("SELECT count(*) FROM as4tainr.journaled_files WHERE library=$1 AND file=$2")
                    .await?
                    > 0;
            let described: bool = self
                .client
                .query_opt(
                    "SELECT described FROM as4tainr.files WHERE library=$1 AND name=$2",
                    &[&lib, &name],
                )
                .await?
                .map(|r| r.get(0))
                .unwrap_or(true);
            let kind = match attr.as_str() {
                "PF" => "Physical, data",
                "PF-SRC" => "Physical, source",
                "LF" => "Logical",
                "SAVF" => "Save file",
                "DSPF" => "Display file",
                "PRTF" => "Printer file",
                _ => attr.as_str(),
            };
            let yn = |b: bool| if b { "Yes" } else { "No" };
            kv(&mut rows, "File", &name);
            kv(&mut rows, "Library", &lib);
            kv(&mut rows, "Kind of file", kind);
            kv(
                &mut rows,
                "Described by DDS or SQL",
                yn(described && !source),
            );
            kv(&mut rows, "Created", &created);
            kv(&mut rows, "Text", &text);
            if attr == "PF" || attr == "LF" || source {
                let (reclen, keys) =
                    match catalog::resolve_file(self.client, std::slice::from_ref(&lib), &name)
                        .await
                    {
                        Ok(f) => (
                            f.file
                                .fields
                                .iter()
                                .filter_map(|x| crate::rpg::program::program_type(x, true, false))
                                .map(|t| t.size())
                                .sum::<usize>(),
                            f.file.keys.clone(),
                        ),
                        Err(_) => (if source { 112 } else { 0 }, vec![]),
                    };
                kv(&mut rows, "Members", &members.len().to_string());
                kv(
                    &mut rows,
                    "Maximum members (MAXMBRS)",
                    if source { "*NOMAX" } else { "1" },
                );
                kv(&mut rows, "Constraints", &constraints.to_string());
                kv(&mut rows, "Triggers", &triggers.to_string());
                kv(&mut rows, "Records at first (SIZE)", "10000");
                kv(&mut rows, "Records added each time (SIZE)", "1000");
                kv(&mut rows, "Times records are added (SIZE)", "3");
                kv(&mut rows, "Reuse deleted records (REUSEDLT)", "*NO");
                kv(&mut rows, "Wait for the file (WAITFILE)", "*IMMED");
                kv(&mut rows, "Wait for a record (WAITRCD)", "60");
                kv(&mut rows, "Sort sequence (SRTSEQ)", "*HEX");
                kv(
                    &mut rows,
                    "Character set (CCSID)",
                    if described { "37" } else { "65535" },
                );
                kv(&mut rows, "Level check (LVLCHK)", "*YES");
                kv(
                    &mut rows,
                    "Access path",
                    if keys.is_empty() { "Arrival" } else { "Keyed" },
                );
                kv(&mut rows, "Record length", &reclen.to_string());
                kv(&mut rows, "Journaled", yn(journaled));
            }
        }
        if ty == "*ACCPTH" {
            let f = catalog::resolve_file(self.client, std::slice::from_ref(&lib), &name).await?;
            kv(
                &mut rows,
                "Access path",
                if f.file.keys.is_empty() {
                    "Arrival"
                } else {
                    "Keyed"
                },
            );
            kv(
                &mut rows,
                "Unique keys",
                if f.file.unique { "Yes" } else { "No" },
            );
            for k in &f.file.keys {
                let order = if f.file.is_descending(k) {
                    "Descending"
                } else {
                    "Ascending"
                };
                kv(&mut rows, "Key field", &format!("{k:<10} {order}"));
            }
        }
        let mut heading = String::new();
        if ty == "*ALL" || ty == "*MBRLIST" {
            let line = format!("{:<10}  {:<10}  Text", "Member", "Type");
            if ty == "*MBRLIST" {
                heading = line;
            } else {
                rows.push(String::new());
                rows.push(line);
            }
            rows.extend(
                members
                    .iter()
                    .map(|(m, t, x)| format!("{m:<10}  {t:<10}  {x}")),
            );
        }
        // On a display, IBM i shows the report in the Display Spooled File
        // viewer (file QPDSPFD), headed by the date and the title
        // (tools/golden/screens4.txt); as4tainr shows its own lines there.
        let date = crate::report::Stamp::now().date;
        let mut shown = vec![crate::report::at(&[
            (4, &date),
            (28, "Display File Description"),
        ])];
        if !heading.is_empty() {
            shown.push(format!("  {heading}"));
        }
        shown.extend(rows.iter().map(|r| format!("  {r}").trim_end().to_string()));
        let view = crate::commands::SpoolView {
            file: "QPDSPFD".into(),
            lines: shown
                .iter()
                .enumerate()
                .map(|(i, t)| (1, i + 1, t.clone()))
                .collect(),
            width: 132,
        };
        Ok(Outcome::Display(ListDisplay {
            title: "Display File Description".into(),
            info: vec![
                ("File".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Members".into(), members.len().to_string()),
            ],
            heading,
            rows,
            spool: Some(Box::new(view)),
            ..Default::default()
        }))
    }

    /// DSPFD TYPE(*MBRLIST) OUTPUT(*OUTFILE): a record per member in the
    /// format of QSYS/QAFDMBRL (QWHFDML; outfile.rs). Dates are YYMMDD
    /// with a century field (0 = 19xx, 1 = 20xx), times HHMMSS (the
    /// model's field descriptions, PUB400 2026-10-26,
    /// tools/golden/verify1.txt v_ffd_mbrl). A source member's records
    /// are its lines; a data member's its rows. Fields as4tainr has no
    /// value for are blank or zero (fidelity-gaps).
    async fn dspfd_mbrlist_outfile(
        &mut self,
        cmd: &Command,
        lib: &str,
        name: &str,
        attr: &str,
    ) -> Result<Outcome> {
        let outfile = cmd.get("OUTFILE").trim().to_string();
        if outfile.is_empty() {
            return Err(crate::cl::value_error(
                "OUTPUT(*OUTFILE) needs OUTFILE, the file to receive the records.",
            ));
        }
        let option = cmd
            .list("OUTMBR")
            .get(1)
            .cloned()
            .unwrap_or_else(|| "*REPLACE".into());
        let logical = attr == "LF";
        let source = crate::source::is_source_file(self.client, lib, name).await?;
        let size = catalog::object_size(self.client, lib, name, "*FILE")
            .await
            .unwrap_or(0);
        let stamp =
            "SELECT to_char(t AT TIME ZONE 'UTC', 'YYYY'), to_char(t AT TIME ZONE 'UTC', 'YYMMDD'),
                            to_char(t AT TIME ZONE 'UTC', 'HH24MISS')
                       FROM (SELECT CAST($1::text AS timestamptz) AS t) x";
        let now = self.client.query_one(stamp, &[&"now"]).await?;
        let (ny, nd, nt): (String, String, String) = (now.get(0), now.get(1), now.get(2));
        // (member, source type, text, records, created, changed)
        type Member = (String, String, String, i64, String, String);
        let members: Vec<Member> = if source {
            self.client
                .query(
                    "SELECT member, srctype, text,
                            CASE WHEN source = '' THEN 0
                                 ELSE array_length(string_to_array(rtrim(source, E'\n'), E'\n'), 1) END::bigint,
                            coalesce(added, now())::text, coalesce(changed, added, now())::text
                       FROM as4tainr.source_members WHERE library=$1 AND file=$2 ORDER BY member",
                    &[&lib, &name],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
                .collect()
        } else if attr == "PF" || logical {
            let o = self
                .client
                .query_one(
                    "SELECT text, created::text, coalesce(changed, created)::text FROM as4tainr.objects
                      WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[&lib, &name],
                )
                .await?;
            let rows: i64 = self
                .client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM {}.{}",
                        catalog::ident(lib),
                        catalog::ident(name)
                    ),
                    &[],
                )
                .await
                .map(|r| r.get(0))
                .unwrap_or(0);
            vec![(
                name.to_string(),
                String::new(),
                o.get(0),
                rows,
                o.get(1),
                o.get(2),
            )]
        } else {
            return Err(crate::cl::value_error(&format!(
                "File {name} in library {lib} has no members to list."
            )));
        };
        let system: String = crate::report::system_name().chars().take(8).collect();
        let mut records = Vec::new();
        for (m, srctype, text, nrec, created, changed) in &members {
            let c = self.client.query_one(stamp, &[created]).await?;
            let h = self.client.query_one(stamp, &[changed]).await?;
            let (cy, cd): (String, String) = (c.get(0), c.get(1));
            let (hy, hd, ht): (String, String, String) = (h.get(0), h.get(1), h.get(2));
            records.push(vec![
                ("MLRCEN", crate::outfile::century(&ny)),
                ("MLRDAT", nd.clone()),
                ("MLRTIM", nt.clone()),
                ("MLFILE", name.to_string()),
                ("MLLIB", crate::qtemp::logical(lib)),
                ("MLFTYP", if logical { "L" } else { "P" }.to_string()),
                ("MLFILA", if logical { "*LGL" } else { "*PHY" }.to_string()),
                ("MLFATR", if logical { "LF" } else { "PF" }.to_string()),
                ("MLSYSN", system.clone()),
                ("MLASP", "1".to_string()),
                ("MLNOMB", members.len().to_string()),
                ("MLNAME", m.clone()),
                ("MLNRCD", nrec.to_string()),
                ("MLNDTR", "0".to_string()),
                ("MLSIZE", size.to_string()),
                ("MLSEU", srctype.clone()),
                ("MLCCEN", crate::outfile::century(&cy)),
                ("MLCDAT", cd),
                ("MLCHGC", crate::outfile::century(&hy)),
                ("MLCHGD", hd),
                ("MLCHGT", ht),
                ("MLMTXT", text.clone()),
                ("MLSEU2", srctype.clone()),
                ("MLSIZ2", size.to_string()),
            ]);
        }
        crate::outfile::write(self, &outfile, &option, &crate::outfile::QAFDMBRL, &records).await?;
        Ok(Outcome::Messages(vec![]))
    }

    /// DSPFFD: a file's record format and its fields, in the layout of
    /// IBM i 7.5's report (PUB400, `system "DSPFFD FILE(AS4TRAINR1/GLDPF)"`
    /// in tools/golden/files-and-programs.txt), printed (OUTPUT(*PRINT),
    /// batch, `system`) or, on a display, shown in the Display Spooled File
    /// viewer as file QPDSPFFD (tools/golden/screens4.txt).
    async fn dspffd(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("FILE").to_ascii_uppercase();
        let (libs, name) = self.search(&q);
        let label = q
            .split_once('/')
            .map(|x| x.0)
            .unwrap_or("*LIBL")
            .to_string();
        let mut found = None;
        for l in &libs {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute, extract(year FROM created)::int, extract(month FROM created)::int,
                            extract(day FROM created)::int
                       FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[l, &name],
                )
                .await?
            {
                found = Some((l.clone(), r));
                break;
            }
        }
        let Some((lib, row)) = found else {
            bail!("CPF9812: File {name} in library {label} not found.");
        };
        authority::check(self.client, &self.user, &lib, &name, "*FILE", Aut::Use).await?;
        let attr: String = row.get(0);
        let created = crate::report::Stamp::of(
            (
                row.get::<_, i32>(1),
                row.get::<_, i32>(2) as u32,
                row.get::<_, i32>(3) as u32,
            ),
            (0, 0, 0),
        )
        .date;
        use crate::dds::DataType;
        let (format, fields, external) = record_format(self.client, &lib, &name, &attr)
            .await
            .map_err(|_| anyhow::anyhow!("CPF9812: File {name} in library {label} not found."))?;
        // Source files' fields have no column headings.
        let heading = attr != "PF-SRC";
        // A character field's CCSID: the system's (QCCSID), or the job's
        // default, 37, when QCCSID is 65535 (as4tainr's jobs are CCSID 37).
        let ccsid = crate::clpgm::system_value_text("QCCSID")
            .map(|v| match v {
                Ok(s) => s,
                Err(n) => n.to_string(),
            })
            .filter(|v| v.trim() != "65535")
            .unwrap_or_else(|| "37".into());
        let at = crate::report::at;
        let kv = |col: usize, label: &str, value: &str| {
            let l = crate::commands::dsp_label(label, 48 - col - 2);
            format!("{}{l}  {value}", " ".repeat(col - 1))
                .trim_end()
                .to_string()
        };
        let num = |col: usize, label: &str, n: usize| {
            let l = crate::commands::dsp_label(label, 48 - col - 2);
            format!("{}{l}{n:>7}", " ".repeat(col - 1))
        };
        let yn = |b: bool| if b { "Yes" } else { "No" };
        let reclen: u32 = fields.iter().map(|f| f.buffer_bytes()).sum();
        let mut body = vec![
            at(&[(2, "Input parameters")]),
            kv(4, "File", &name),
            kv(6, "Library", &lib),
            at(&[(2, "File Information")]),
            kv(4, "File", &name),
            kv(6, "Library", &lib),
            kv(4, "File location", "*LCL"),
            kv(4, "Externally described", yn(external)),
            num(4, "Number of record formats", 1),
            kv(
                4,
                "Type of file",
                if attr.starts_with("LF") {
                    "Logical"
                } else {
                    "Physical"
                },
            ),
            kv(4, "File creation date", &created),
            at(&[(2, "Record Format Information")]),
            kv(4, "Record format", &format),
            kv(4, "Format level identifier", &level_id(&format, &fields)),
            num(4, "Number of fields", fields.len()),
            num(4, "Record length", reclen as usize),
            at(&[(2, "Field Level Information")]),
            at(&[
                (15, "Data"),
                (27, "Field"),
                (34, "Buffer"),
                (44, "Buffer"),
                (58, "Field"),
                (67, "Column"),
            ]),
            at(&[
                (4, "Field"),
                (15, "Type"),
                (26, "Length"),
                (34, "Length"),
                (42, "Position"),
                (58, "Usage"),
                (67, "Heading"),
            ]),
        ];
        let mut pos = 1u32;
        for x in &fields {
            let ty = match x.data_type {
                DataType::Char => "CHAR".to_string(),
                t => format!("{t:?}").to_ascii_uppercase(),
            };
            let size = x.buffer_bytes();
            // Numbers: digits ending in column 28, decimal positions in
            // 31; other lengths end in column 31.
            let length = if x.data_type.is_numeric() && x.data_type != DataType::Float {
                format!("{:>7}{:>3}", x.length, x.decimals)
            } else {
                format!("{:>10}", x.length)
            };
            let mut line = format!(
                "   {:<10} {:<7}{length}{:>8}{:>10}        Both",
                x.name, ty, size, pos
            );
            if heading {
                line = format!("{line:<66}{}", x.name);
            }
            body.push(line);
            if x.data_type == DataType::Char {
                body.push(format!(
                    "     {}{ccsid:>7}",
                    crate::commands::dsp_label("Coded Character Set Identifier", 48 - 6 - 2)
                ));
            }
            pos += size;
        }
        let title = "Display File Field Description";
        let stamp = crate::report::Stamp::now();
        let when = format!("{}  {}", stamp.date, stamp.time);
        let mut print = vec![at(&[
            (2, crate::report::PRODUCT),
            (18, crate::report::LEVEL),
            (30, title),
            (97, &when),
            (122, "Page    1"),
        ])];
        print.extend(body.iter().cloned());
        let mut shown = vec![at(&[(30, title)])];
        shown.extend(body);
        let view = crate::commands::SpoolView {
            file: "QPDSPFFD".into(),
            lines: shown
                .iter()
                .enumerate()
                .map(|(i, t)| (1, i + 1, t.clone()))
                .collect(),
            width: 132,
        };
        Ok(Outcome::Display(ListDisplay {
            title: "Display Spooled File".into(),
            rows: shown,
            print,
            spool: Some(Box::new(view)),
            ..Default::default()
        }))
    }
}

/// A file's record format: its name, its fields, and whether the file
/// is externally described. A source file's record is a sequence number,
/// a date and the data (CRTSRCPF RCDLEN).
pub async fn record_format(
    client: &tokio_postgres::Client,
    lib: &str,
    name: &str,
    attr: &str,
) -> Result<(String, Vec<crate::dds::FieldDef>, bool)> {
    use crate::dds::{DataType, FieldDef};
    if attr == "PF-SRC" {
        let rcdlen = crate::source::record_length(client, lib, name).await?;
        let f = |n: &str, t: DataType, l: u32, d: u32| FieldDef {
            name: n.into(),
            data_type: t,
            length: l,
            decimals: d,
            text: String::new(),
            varlen: false,
            null_capable: false,
            default: None,
            alias: None,
            identity: None,
            double: false,
            generated: None,
        };
        return Ok((
            name.to_string(),
            vec![
                f("SRCSEQ", DataType::Zoned, 6, 2),
                f("SRCDAT", DataType::Zoned, 6, 0),
                f("SRCDTA", DataType::Char, rcdlen.saturating_sub(12), 0),
            ],
            true,
        ));
    }
    let fr = catalog::resolve_file(client, &[lib.to_string()], name).await?;
    let pf = fr.file;
    // CRTPF RCDLEN(n): one field and format named like the file.
    let external = !(pf.fields.len() == 1 && pf.fields[0].name == name && pf.format == name);
    Ok((pf.format, pf.fields, external))
}

/// A record format's level identifier: 13 hexadecimal digits that change
/// when the format's fields change. IBM i computes its own from the
/// format; as4tainr's is a hash of the fields' names, types and lengths
/// (docs/fidelity-gaps.md).
pub fn level_id(format: &str, fields: &[crate::dds::FieldDef]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |s: &str| {
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    eat(format);
    for f in fields {
        eat(&format!(
            ";{}:{}:{}:{}",
            f.name,
            f.data_type.code(),
            f.length,
            f.decimals
        ));
    }
    format!("{:013X}", h & 0xF_FFFF_FFFF_FFFF)
}
