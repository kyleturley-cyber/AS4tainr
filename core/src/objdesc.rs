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
        Ok(Outcome::Display(ListDisplay {
            title: "Display File Description".into(),
            info: vec![
                ("File".into(), name.clone()),
                ("Library".into(), lib.clone()),
                ("Members".into(), members.len().to_string()),
            ],
            heading,
            rows,
            ..Default::default()
        }))
    }

    /// DSPFFD: a file's record format and its fields, with their types,
    /// lengths, positions in the record and texts.
    async fn dspffd(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("FILE").to_ascii_uppercase();
        let (libs, name) = self.search(&q);
        let label = q
            .split_once('/')
            .map(|x| x.0)
            .unwrap_or("*LIBL")
            .to_string();
        let f = catalog::resolve_file(self.client, &libs, &name)
            .await
            .map_err(|_| anyhow::anyhow!("CPF9812: File {name} in library {label} not found."))?;
        authority::check(
            self.client,
            &self.user,
            &f.library,
            &name,
            "*FILE",
            Aut::Use,
        )
        .await?;
        let mut rows = Vec::new();
        let mut pos = 1usize;
        for x in &f.file.fields {
            let ty = format!("{:?}", x.data_type).to_ascii_uppercase();
            let len = if x.decimals > 0 || matches!(ty.as_str(), "PACKED" | "ZONED" | "BINARY") {
                format!("{},{}", x.length, x.decimals)
            } else {
                x.length.to_string()
            };
            let size = crate::rpg::program::program_type(x, true, false)
                .map(|t| t.size())
                .unwrap_or(0);
            rows.push(format!(
                "{:<10} {:<10} {:>7} {:>7} {:>7}  {}",
                x.name, ty, len, size, pos, x.text
            ));
            pos += size;
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display File Field Description".into(),
            info: vec![
                ("File".into(), name.clone()),
                ("Library".into(), f.library.clone()),
                ("Record format".into(), f.file.format.clone()),
                ("Fields".into(), f.file.fields.len().to_string()),
                ("Record length".into(), (pos - 1).to_string()),
            ],
            heading: format!(
                "{:<10} {:<10} {:>7} {:>7} {:>7}  {}",
                "Field", "Type", "Length", "Bytes", "From", "Text"
            ),
            rows,
            ..Default::default()
        }))
    }
}
