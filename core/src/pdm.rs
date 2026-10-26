//! PDM's list displays (WRKMBRPDM, WRKOBJPDM), WRKMSGD and RMVM.
//!
//! Layouts observed on IBM i 7.5 (PUB400, tools/golden/screens4.txt, the
//! capture not kept in the repository): titles, header fields, option and
//! key lines at IBM's columns. as4tainr shows only the options and keys
//! it carries out, each where IBM i shows it; the others leave their place
//! blank (docs/fidelity-gaps.md, "PDM").

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::Command;
use crate::commands::{HeaderField, IbmLayout, Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

/// A line of texts at screen columns, as a string shown from column 2.
fn cols(fields: &[(usize, &str)]) -> String {
    let shifted: Vec<(usize, &str)> = fields.iter().map(|(c, t)| (c - 1, *t)).collect();
    crate::report::at(&shifted)
}

/// The command that compiles a member of this source type into a program
/// (PDM option 14) and into a module (option 15), as PDM chooses them.
fn compile_commands(
    lib: &str,
    file: &str,
    mbr: &str,
    ty: &str,
) -> (Option<String>, Option<String>) {
    let src = format!("SRCFILE({lib}/{file}) SRCMBR({mbr})");
    let obj = format!("{lib}/{mbr}");
    match ty {
        "CLLE" => (
            Some(format!("CRTBNDCL PGM({obj}) {src}")),
            Some(format!("CRTCLMOD MODULE({obj}) {src}")),
        ),
        "CLP" | "CL" => (Some(format!("CRTCLPGM PGM({obj}) {src}")), None),
        "RPGLE" => (
            Some(format!("CRTBNDRPG PGM({obj}) {src}")),
            Some(format!("CRTRPGMOD MODULE({obj}) {src}")),
        ),
        "SQLRPGLE" => (
            Some(format!("CRTSQLRPGI OBJ({obj}) {src} OBJTYPE(*PGM)")),
            Some(format!("CRTSQLRPGI OBJ({obj}) {src} OBJTYPE(*MODULE)")),
        ),
        "RPG" => (Some(format!("CRTRPGPGM PGM({obj}) {src}")), None),
        "PF" => (Some(format!("CRTPF FILE({obj}) {src}")), None),
        "LF" => (Some(format!("CRTLF FILE({obj}) {src}")), None),
        "DSPF" => (Some(format!("CRTDSPF FILE({obj}) {src}")), None),
        "PRTF" => (Some(format!("CRTPRTF FILE({obj}) {src}")), None),
        _ => (None, None),
    }
}

/// PDM runs compiles in batch (its default "Run in batch: Y").
fn submitted(cmd: &str, job: &str) -> String {
    format!("SBMJOB CMD({cmd}) JOB({job})")
}

/// The commands that delete and display an object of a type (PDM options
/// 4 and 5).
fn object_commands(
    lib: &str,
    name: &str,
    ty: &str,
    attr: &str,
) -> (Option<String>, Option<String>) {
    let q = format!("{lib}/{name}");
    let delete = match ty {
        "*PGM" => Some(format!("DLTPGM PGM({q})")),
        "*FILE" => Some(format!("DLTF FILE({q})")),
        "*DTAARA" => Some(format!("DLTDTAARA DTAARA({q})")),
        "*MODULE" => Some(format!("DLTMOD MODULE({q})")),
        "*SRVPGM" => Some(format!("DLTSRVPGM SRVPGM({q})")),
        "*CMD" => Some(format!("DLTCMD CMD({q})")),
        "*MSGF" => Some(format!("DLTMSGF MSGF({q})")),
        "*BNDDIR" => Some(format!("DLTBNDDIR BNDDIR({q})")),
        "*MENU" => Some(format!("DLTMNU MENU({q})")),
        "*DTAQ" => Some(format!("DLTDTAQ DTAQ({q})")),
        "*OUTQ" => Some(format!("DLTOUTQ OUTQ({q})")),
        _ => None,
    };
    let display = match ty {
        "*PGM" => Some(format!("DSPPGM PGM({q})")),
        "*FILE" if attr == "PF-SRC" => Some(format!("WRKMBRPDM FILE({q})")),
        "*FILE" if attr == "PF-DTA" => Some(format!("DSPPFM FILE({q})")),
        "*DTAARA" => Some(format!("DSPDTAARA DTAARA({q})")),
        "*SRVPGM" => Some(format!("DSPSRVPGM SRVPGM({q})")),
        "*CMD" => Some(format!("DSPCMD CMD({q})")),
        "*MSGF" => Some(format!("WRKMSGD MSGF({q})")),
        "*BNDDIR" => Some(format!("DSPBNDDIR BNDDIR({q})")),
        "*OUTQ" => Some(format!("WRKOUTQ OUTQ({q})")),
        _ => None,
    };
    (delete, display)
}

/// WRKOBJPDM's order: programs, then files, then the other types, each by
/// name (PUB400 shows a *PGM before the library's files).
fn type_rank(ty: &str) -> u8 {
    match ty {
        "*PGM" => 0,
        "*FILE" => 1,
        _ => 2,
    }
}

impl Job<'_> {
    pub(crate) async fn pdm_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "WRKMBRPDM" => self.wrkmbrpdm(cmd).await?,
            "WRKOBJPDM" => self.wrkobjpdm(cmd).await?,
            "WRKMSGD" => self.wrkmsgd(cmd).await?,
            "RMVM" => self.rmvm(cmd).await?,
            "ADDPFM" => self.addpfm(cmd).await?,
            _ => return Ok(None),
        }))
    }

    /// The library of a file named FILE(lib/name), and the name.
    async fn pdm_file(&self, q: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(q);
        let name = name.to_ascii_uppercase();
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, "*FILE").await? {
                return Ok((l.clone(), name));
            }
        }
        let lib = q.split_once('/').map(|(l, _)| l).unwrap_or("*LIBL");
        bail!("CPF9812: File {name} in library {lib} not found.")
    }

    /// ADDPFM: add a member to a source file. A member that is there
    /// already is CPF5812, then the escape CPF7306 (the messages a CL
    /// program monitors for after ADDPFM; IBM's ADDPFM command
    /// documentation). Data files keep one member in as4tainr.
    async fn addpfm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, file) = self.pdm_file(cmd.get("FILE")).await?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Change).await?;
        let mbr = catalog::validate_name("member", cmd.get("MBR"))?;
        if !crate::source::is_source_file(self.client, &lib, &file).await? {
            bail!(
                "CPF7306: Member {mbr} not added to file {file} in library {lib}: as4tainr keeps one member in a data file."
            );
        }
        let text = match cmd.get("TEXT") {
            t if t.eq_ignore_ascii_case("*BLANK") => String::new(),
            t => t.to_string(),
        };
        let srctype = match cmd.get("SRCTYPE").to_ascii_uppercase() {
            t if t == "*NONE" => String::new(),
            t => t,
        };
        let m = crate::source::Member {
            library: lib.clone(),
            file: file.clone(),
            name: mbr.clone(),
            srctype,
            text,
            ccsid: 37,
            ..Default::default()
        };
        if crate::source::put_member(self.client, &m, false)
            .await
            .is_err()
        {
            bail!(
                "CPF5812: Member {mbr} already exists in file {file} in library {lib}.\nCPF7306: Member {mbr} not added to file {file} in library {lib}."
            );
        }
        Ok(Outcome::Messages(vec![format!(
            "CPC7305: Member {mbr} was added to file {file} in library {lib}."
        )]))
    }

    /// RMVM: remove a member from a source file.
    async fn rmvm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, file) = self.pdm_file(cmd.get("FILE")).await?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Change).await?;
        let mbr = cmd.get("MBR").to_ascii_uppercase();
        if !crate::source::is_source_file(self.client, &lib, &file).await? {
            bail!("as4tainr keeps one member in a data file; delete file {file} instead.");
        }
        let all = crate::source::members(self.client, &lib, &file).await?;
        let chosen: Vec<String> = all
            .into_iter()
            .map(|(m, _, _)| m)
            .filter(|m| catalog::name_matches(&mbr, m))
            .collect();
        if chosen.is_empty() {
            bail!("CPF9815: Member {mbr} of file {file} in library {lib} does not exist.");
        }
        let mut done = Vec::new();
        for m in chosen {
            crate::source::remove_member(self.client, &lib, &file, &m).await?;
            done.push(format!(
                "Member {m} of file {file} in library {lib} was removed."
            ));
        }
        Ok(Outcome::Messages(done))
    }

    /// WRKMBRPDM: the members of a file, with PDM's options.
    async fn wrkmbrpdm(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, file) = self.pdm_file(cmd.get("FILE")).await?;
        authority::check(self.client, &self.user, &lib, &file, "*FILE", Aut::Use).await?;
        let pattern = cmd.get("MBR").to_ascii_uppercase();
        let mbrtype = cmd.get("MBRTYPE").to_ascii_uppercase();
        let source = crate::source::is_source_file(self.client, &lib, &file).await?;
        let members = if source {
            crate::source::members(self.client, &lib, &file).await?
        } else {
            let text: String = self
                .client
                .query_opt(
                    "SELECT text FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[&lib, &file],
                )
                .await?
                .map(|r| r.get(0))
                .unwrap_or_default();
            vec![(file.clone(), String::new(), text)]
        };
        let members: Vec<_> = members
            .into_iter()
            .filter(|(m, t, _)| {
                catalog::name_matches(&pattern, m)
                    && (mbrtype == "*ALL" || catalog::name_matches(&mbrtype, t))
            })
            .collect();
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        let mut row_options = Vec::new();
        for (m, t, text) in &members {
            rows.push(format!("{m:<10}  {t:<10}  {text}").trim_end().to_string());
            keys.push(m.clone());
            let mut o = Vec::new();
            if source {
                o.push((4, format!("RMVM FILE({lib}/{file}) MBR({m})")));
            }
            o.push((5, format!("DSPPFM FILE({lib}/{file}) MBR({m})")));
            let (pgm, module) = compile_commands(&lib, &file, m, t);
            if let Some(c) = pgm {
                o.push((14, submitted(&c, m)));
            }
            if let Some(c) = module {
                o.push((15, submitted(&c, m)));
            }
            row_options.push(o);
        }
        let mut options = vec![
            (5u8, "Display".to_string(), String::new()),
            (14, "Compile".into(), String::new()),
            (15, "Create module".into(), String::new()),
        ];
        let mut first = vec![(35, "5=Display")];
        if source {
            options.insert(0, (4, "Delete".into(), String::new()));
            first.insert(0, (26, "4=Delete"));
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Members Using PDM".into(),
            heading: format!("{:<10}  {:<10}  {}", "Member", "Type", "Text"),
            rows,
            options,
            keys,
            refresh: Some(format!(
                "WRKMBRPDM FILE({lib}/{file}) MBR({pattern}) MBRTYPE({mbrtype})"
            )),
            ibm: IbmLayout::some(IbmLayout {
                title_col: Some(28),
                system_col1: Some(72),
                lines: vec![
                    cols(&[(2, "File  . . . . . .")]),
                    cols(&[(4, "Library . . . ."), (45, "Position to  . . . . .")]),
                ],
                header_fields: vec![
                    HeaderField {
                        name: "FILE".into(),
                        row: 3,
                        col: 22,
                        len: 10,
                        value: file.clone(),
                    },
                    HeaderField {
                        name: "LIB".into(),
                        row: 4,
                        col: 24,
                        len: 10,
                        value: lib.clone(),
                    },
                    HeaderField {
                        name: "POS".into(),
                        row: 4,
                        col: 70,
                        len: 10,
                        value: String::new(),
                    },
                ],
                position_field: Some("POS".into()),
                reload: Some("WRKMBRPDM FILE({LIB}/{FILE})".into()),
                option_text: vec![vec![
                    cols(&first),
                    cols(&[(50, "14=Compile"), (62, "15=Create module")]),
                ]],
                key_text: vec![cols(&[(2, "F3=Exit"), (41, "F5=Refresh")]), String::new()],
                command_line: Some("Parameters or command".into()),
                row_options,
                confirm_delete: true,
                cursor_on_list: true,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    /// WRKOBJPDM: a library's objects, with PDM's options.
    async fn wrkobjpdm(&mut self, cmd: &Command) -> Result<Outcome> {
        let lib = match cmd.get("LIB").to_ascii_uppercase().as_str() {
            "*CURLIB" => self.current_library(),
            l => l.to_string(),
        };
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        let pattern = cmd.get("OBJ").to_ascii_uppercase();
        let objtype = cmd.get("OBJTYPE").to_ascii_uppercase();
        let objatr = cmd.get("OBJATR").to_ascii_uppercase();
        let mut objects = Vec::new();
        for o in catalog::list_objects(self.client, &lib).await? {
            let attr = match o.attribute.as_str() {
                "PF" if o.objtype == "*FILE" => {
                    if crate::source::is_source_file(self.client, &lib, &o.name).await? {
                        "PF-SRC".to_string()
                    } else {
                        "PF-DTA".to_string()
                    }
                }
                a => a.to_string(),
            };
            if catalog::name_matches(&pattern, &o.name)
                && (objtype == "*ALL" || objtype == o.objtype)
                && (objatr == "*ALL" || catalog::name_matches(&objatr, &attr))
            {
                objects.push((o.name, o.objtype, attr, o.text));
            }
        }
        objects.sort_by(|a, b| (type_rank(&a.1), &a.1, &a.0).cmp(&(type_rank(&b.1), &b.1, &b.0)));
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        let mut row_options = Vec::new();
        for (name, ty, attr, text) in &objects {
            let text: String = text.chars().take(37).collect();
            rows.push(
                format!("{name:<10}  {ty:<10}  {attr:<10}  {text}")
                    .trim_end()
                    .to_string(),
            );
            keys.push(name.clone());
            let (delete, display) = object_commands(&lib, name, ty, attr);
            let mut o = Vec::new();
            if let Some(c) = delete {
                o.push((4, c));
            }
            if let Some(c) = display {
                o.push((5, c));
            }
            o.push((8, format!("DSPOBJD OBJ({lib}/{name}) OBJTYPE({ty})")));
            if ty == "*FILE" && attr.starts_with("PF") {
                o.push((12, format!("WRKMBRPDM FILE({lib}/{name})")));
            }
            row_options.push(o);
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Objects Using PDM".into(),
            heading: format!(
                "{:<10}  {:<10}  {:<10}  {}",
                "Object", "Type", "Attribute", "Text"
            ),
            rows,
            options: vec![
                (4, "Delete".into(), String::new()),
                (5, "Display".into(), String::new()),
                (8, "Display description".into(), String::new()),
                (12, "Work with".into(), String::new()),
            ],
            keys,
            refresh: Some(format!(
                "WRKOBJPDM LIB({lib}) OBJ({pattern}) OBJTYPE({objtype}) OBJATR({objatr})"
            )),
            ibm: IbmLayout::some(IbmLayout {
                title_col: Some(27),
                system_col1: Some(72),
                lines: vec![
                    cols(&[
                        (2, "Library . . . . ."),
                        (39, "Position to . . . . . . . ."),
                    ]),
                    String::new(),
                ],
                header_fields: vec![
                    HeaderField {
                        name: "LIB".into(),
                        row: 3,
                        col: 22,
                        len: 10,
                        value: lib.clone(),
                    },
                    HeaderField {
                        name: "POS".into(),
                        row: 3,
                        col: 69,
                        len: 10,
                        value: String::new(),
                    },
                ],
                position_field: Some("POS".into()),
                reload: Some("WRKOBJPDM LIB({LIB})".into()),
                option_text: vec![
                    vec![
                        cols(&[(33, "4=Delete"), (47, "5=Display")]),
                        cols(&[(4, "8=Display description")]),
                    ],
                    vec![cols(&[(3, "12=Work with")]), String::new()],
                ],
                key_text: vec![
                    cols(&[(2, "F3=Exit"), (40, "F5=Refresh")]),
                    cols(&[(40, "F23=More options")]),
                ],
                command_line: Some("Parameters or command".into()),
                row_options,
                confirm_delete: true,
                cursor_on_list: true,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }

    /// WRKMSGD: a message file's descriptions, from MSGID on.
    async fn wrkmsgd(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.message_file(cmd.get("MSGF")).await?;
        authority::check(self.client, &self.user, &lib, &name, "*MSGF", Aut::Use).await?;
        let all = crate::msgf::list(self.client, &lib, &name).await?;
        let key = |id: &str| crate::rpg::data::e(id);
        let start = match cmd.get("MSGID").to_ascii_uppercase().as_str() {
            "*FIRST" | "" => 0,
            id => all
                .iter()
                .position(|d| key(&d.msgid) >= key(id))
                .unwrap_or(all.len().saturating_sub(1)),
        };
        let q = format!("{lib}/{name}");
        let rows: Vec<String> = all
            .iter()
            .map(|d| {
                let r = format!(" {:<7}{:>9}     {}", d.msgid, d.sev, d.msg);
                r.chars().take(73).collect()
            })
            .collect();
        let keys = all.iter().map(|d| d.msgid.clone()).collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Message Descriptions".into(),
            heading: "Message ID  Severity  Message Text".into(),
            rows,
            options: vec![
                (4, "Delete".into(), format!("RMVMSGD MSGID({{}}) MSGF({q})")),
                (
                    5,
                    "Display details".into(),
                    format!("DSPMSGD RANGE({{}}) MSGF({q})"),
                ),
                (
                    6,
                    "Print".into(),
                    format!("DSPMSGD RANGE({{}}) MSGF({q}) OUTPUT(*PRINT)"),
                ),
            ],
            keys,
            refresh: Some(format!("WRKMSGD MSGF({q})")),
            ibm: IbmLayout::some(IbmLayout {
                system_col: Some(62),
                lines: vec![
                    cols(&[
                        (2, "Message file:"),
                        (18, &name),
                        (33, "Library:"),
                        (44, &lib),
                    ]),
                    String::new(),
                    cols(&[(2, "Position to . . . . . . ."), (40, "Message ID")]),
                ],
                header_fields: vec![HeaderField {
                    name: "POS".into(),
                    row: 5,
                    col: 30,
                    len: 7,
                    value: String::new(),
                }],
                position_field: Some("POS".into()),
                option_text: vec![vec![cols(&[
                    (15, "4=Delete"),
                    (26, "5=Display details"),
                    (46, "6=Print"),
                ])]],
                key_text: vec![
                    cols(&[(2, "F3=Exit"), (12, "F5=Refresh"), (34, "F12=Cancel")]),
                    String::new(),
                ],
                command_line: Some("Parameters or command".into()),
                confirm_delete: true,
                cursor_on_list: true,
                start_at: start,
                ..Default::default()
            }),
            ..Default::default()
        }))
    }
}
