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
        let from = catalog::resolve_file(self.client, &libs, &from_name)
            .await
            .map_err(|_| {
                anyhow!(
                    "CPF2802: From-file {from_name} in library {from_label} does not exist.\nCPF2817: The copy was not done."
                )
            })?;
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
        let mut reader = rla::OpenFile::new(from.clone());
        let mut records = Vec::new();
        loop {
            let r = reader.read(self.client).await?;
            if r.eof {
                break;
            }
            if let Some(rec) = r.record {
                records.push(rec);
            }
        }
        let fields: Vec<String> = from.file.fields.iter().map(|f| f.name.clone()).collect();
        let copied = format!(
            "CPC2955: {} records were copied from member {from_name}.",
            records.len()
        );
        if to_q == "*PRINT" {
            let widths: Vec<usize> = from
                .file
                .fields
                .iter()
                .map(|f| {
                    records
                        .iter()
                        .map(|r| r.get(&f.name).map(|v| v.chars().count()).unwrap_or(0))
                        .max()
                        .unwrap_or(0)
                        .max(f.name.len())
                })
                .collect();
            let line = |vals: Vec<String>| -> String {
                vals.iter()
                    .zip(&widths)
                    .map(|(v, w)| format!("{v:<w$}"))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_end()
                    .to_string()
            };
            let rows: Vec<String> = records
                .iter()
                .map(|r| {
                    line(
                        fields
                            .iter()
                            .map(|f| r.get(f).cloned().unwrap_or_default())
                            .collect(),
                    )
                })
                .collect();
            // Printed: the completion message, then the records (observed
            // on IBM i 7.5: CPC2955 first, then the listing).
            let heading = line(fields.clone());
            let mut print = vec![
                copied,
                format!("Copy File {}/{from_name}", from.library),
                format!("Record format: {}", from.file.format),
                heading.clone(),
            ];
            print.extend(rows.iter().cloned());
            return Ok(Outcome::Display(ListDisplay {
                title: format!("Copy File {}/{from_name}", from.library),
                info: vec![
                    ("From file".into(), format!("{}/{from_name}", from.library)),
                    ("Record format".into(), from.file.format.clone()),
                    ("Records copied".into(), records.len().to_string()),
                ],
                heading,
                rows,
                print,
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
