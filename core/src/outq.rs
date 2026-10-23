//! Output queues (*OUTQ) and the spooled file commands that change
//! spooled files: CRTOUTQ, DLTOUTQ, CLROUTQ, HLDOUTQ, RLSOUTQ, WRKOUTQ,
//! HLDSPLF, RLSSPLF, CHGSPLFA, WRKSPLFA, and the output queue of user
//! profiles (CRTUSRPRF and CHGUSRPRF OUTQ).
//!
//! Written from IBM's CL reference (CHGOUTQ's parameters, which CRTOUTQ
//! shares; HLDOUTQ: held queues keep their files from writers, CPF3426
//! when already held; CPF3357 when a queue is not found; WRKOUTQ and
//! WRKSPLF). QGPL/QPRINT is there from the start, as on a new system;
//! a profile's OUTQ(*WRKSTN), the default, sends its spooled files there
//! (as4tainr's display devices have no output queue of their own).

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::Command;
use crate::commands::{IbmLayout, Job, ListDisplay, Outcome};
use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.output_queues (
    library  text NOT NULL,
    name     text NOT NULL,
    seq      text NOT NULL DEFAULT '*FIFO',
    dspdta   text NOT NULL DEFAULT '*NO',
    oprctl   text NOT NULL DEFAULT '*YES',
    held     boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, name)
);
ALTER TABLE as4tainr.spooled_files ADD COLUMN IF NOT EXISTS outq_lib text NOT NULL DEFAULT 'QGPL';
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS outq text NOT NULL DEFAULT '*WRKSTN';
"#;

/// The tables, and QGPL/QPRINT.
pub async fn init(client: &Client) -> Result<()> {
    client.batch_execute(DDL).await?;
    client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             SELECT 'QGPL', 'QPRINT', '*OUTQ', '', 'Default printer output queue'
              WHERE NOT EXISTS (SELECT 1 FROM as4tainr.objects
                                 WHERE library='QGPL' AND name='QPRINT' AND objtype='*OUTQ')",
            &[],
        )
        .await?;
    client
        .execute(
            "INSERT INTO as4tainr.output_queues (library, name) VALUES ('QGPL', 'QPRINT')
             ON CONFLICT DO NOTHING",
            &[],
        )
        .await?;
    Ok(())
}

/// The output queue a user's spooled files go to: the profile's OUTQ,
/// or QGPL/QPRINT for *WRKSTN (and a queue that no longer exists).
pub async fn for_user(client: &Client, user: &str) -> Result<(String, String)> {
    let q: Option<String> = client
        .query_opt(
            "SELECT outq FROM as4tainr.user_profiles WHERE name=$1",
            &[&user],
        )
        .await?
        .map(|r| r.get(0));
    if let Some((lib, name)) = q.as_deref().and_then(|q| q.split_once('/')) {
        if client
            .query_opt(
                "SELECT 1 FROM as4tainr.output_queues WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?
            .is_some()
        {
            return Ok((lib.to_string(), name.to_string()));
        }
    }
    Ok(("QGPL".into(), "QPRINT".into()))
}

/// A user profile's OUTQ: *WRKSTN or LIB/NAME of an output queue.
pub async fn set_user_outq(
    client: &Client,
    user: &str,
    value: &str,
    libl: &[String],
) -> Result<()> {
    let v = value.trim().to_ascii_uppercase();
    let stored = if v == "*WRKSTN" || v == "*DEV" {
        "*WRKSTN".to_string()
    } else {
        let (lib, name) = find_queue(client, &v, libl).await?;
        format!("{lib}/{name}")
    };
    client
        .execute(
            "UPDATE as4tainr.user_profiles SET outq=$2 WHERE name=$1",
            &[&user.to_ascii_uppercase(), &stored],
        )
        .await?;
    Ok(())
}

/// Find an output queue (LIB/NAME, *LIBL/NAME, NAME): CPF3357 if it is
/// not there.
pub async fn find_queue(client: &Client, q: &str, libl: &[String]) -> Result<(String, String)> {
    let q = q.trim().to_ascii_uppercase();
    let (libs, name, label) = match q.split_once('/') {
        Some(("*LIBL", n)) => (libl.to_vec(), n.to_string(), "*LIBL".to_string()),
        Some((l, n)) => (vec![l.to_string()], n.to_string(), l.to_string()),
        None => (libl.to_vec(), q.clone(), "*LIBL".to_string()),
    };
    for l in &libs {
        if client
            .query_opt(
                "SELECT 1 FROM as4tainr.output_queues WHERE library=$1 AND name=$2",
                &[l, &name],
            )
            .await?
            .is_some()
        {
            return Ok((l.clone(), name));
        }
    }
    bail!("CPF3357: Output queue {name} in library {label} not found.")
}

/// IBM's short status of a spooled file on lists (WRKSPLF's Sts).
pub fn short_status(s: &str) -> &str {
    match s {
        "OPN" | "HLD" | "SAV" | "WTR" => s,
        _ => "RDY",
    }
}

impl Job<'_> {
    pub(crate) async fn outq_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "CRTOUTQ" => self.crtoutq(cmd).await?,
            "DLTOUTQ" => self.dltoutq(cmd).await?,
            "CLROUTQ" => self.clroutq(cmd).await?,
            "HLDOUTQ" => self.hold_outq(cmd, true).await?,
            "RLSOUTQ" => self.hold_outq(cmd, false).await?,
            "WRKOUTQ" => self.wrkoutq(cmd).await?,
            "HLDSPLF" => self.hold_splf(cmd, true).await?,
            "RLSSPLF" => self.hold_splf(cmd, false).await?,
            "CHGSPLFA" => self.chgsplfa(cmd).await?,
            "WRKSPLFA" => self.wrksplfa(cmd).await?,
            _ => return Ok(None),
        }))
    }

    async fn crtoutq(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("OUTQ").to_ascii_uppercase();
        let (lib, name) = match q.split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => (self.current_library(), q.clone()),
        };
        let lib = catalog::validate_name("library", &lib)?;
        let name = catalog::validate_name("output queue", &name)?;
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF9810: Library {lib} not found.");
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        let n = self
            .client
            .execute(
                "WITH o AS (
                    INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                    VALUES ($1, $2, '*OUTQ', '', $3) ON CONFLICT DO NOTHING RETURNING 1)
                 INSERT INTO as4tainr.output_queues (library, name, seq, dspdta, oprctl)
                 SELECT $1, $2, $4, $5, $6 FROM o",
                &[
                    &lib,
                    &name,
                    &text,
                    &cmd.get("SEQ").to_string(),
                    &cmd.get("DSPDTA").to_string(),
                    &cmd.get("OPRCTL").to_string(),
                ],
            )
            .await?;
        if n == 0 {
            bail!("CPF9870: Object {name} type *OUTQ already exists in library {lib}.");
        }
        authority::set_owner(self.client, &lib, &name, "*OUTQ", &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "Output queue {name} was created in library {lib}."
        )]))
    }

    /// DLTOUTQ: only an output queue without spooled files.
    async fn dltoutq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = find_queue(self.client, cmd.get("OUTQ"), &self.library_list()).await?;
        authority::check(self.client, &self.user, &lib, &name, "*OUTQ", Aut::All).await?;
        let files: i64 = self
            .client
            .query_one(
                "SELECT count(*) FROM as4tainr.spooled_files WHERE outq_lib=$1 AND outq=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        if files > 0 {
            bail!("Output queue {name} in {lib} not deleted: it has {files} spooled files.");
        }
        self.client
            .execute(
                "DELETE FROM as4tainr.output_queues WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        authority::forget(self.client, &lib, &name, "*OUTQ").await?;
        self.client
            .execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*OUTQ'",
                &[&lib, &name],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2191: Object {name} of type *OUTQ in library {lib} was deleted."
        )]))
    }

    /// CLROUTQ: every spooled file on the queue is deleted (files being
    /// written stay).
    async fn clroutq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = find_queue(self.client, cmd.get("OUTQ"), &self.library_list()).await?;
        authority::check(self.client, &self.user, &lib, &name, "*OUTQ", Aut::Change).await?;
        let ids: Vec<i64> = self
            .client
            .query(
                "SELECT id FROM as4tainr.spooled_files
                  WHERE outq_lib=$1 AND outq=$2 AND status <> 'OPN'",
                &[&lib, &name],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        for id in &ids {
            crate::spool::delete(self.client, *id).await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "{} spooled files deleted from output queue {name} in library {lib}.",
            ids.len()
        )]))
    }

    /// HLDOUTQ and RLSOUTQ.
    async fn hold_outq(&mut self, cmd: &Command, hold: bool) -> Result<Outcome> {
        let (lib, name) = find_queue(self.client, cmd.get("OUTQ"), &self.library_list()).await?;
        authority::check(self.client, &self.user, &lib, &name, "*OUTQ", Aut::Change).await?;
        let held: bool = self
            .client
            .query_one(
                "SELECT held FROM as4tainr.output_queues WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        if hold && held {
            bail!("CPF3426: Output queue {name} in library {lib} is already held.");
        }
        self.client
            .execute(
                "UPDATE as4tainr.output_queues SET held=$3 WHERE library=$1 AND name=$2",
                &[&lib, &name, &hold],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "Output queue {name} in library {lib} {}.",
            if hold { "held" } else { "released" }
        )]))
    }

    /// HLDSPLF and RLSSPLF: a held file stays on its queue, status HLD,
    /// until it is released (RDY again).
    async fn hold_splf(&mut self, cmd: &Command, hold: bool) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        let (from, to) = if hold { ("RDY", "HLD") } else { ("HLD", "RDY") };
        if f.status != from {
            return Ok(Outcome::Messages(vec![format!(
                "Spooled file {} number {} is not {}; nothing changed.",
                f.file,
                f.number,
                if hold { "ready" } else { "held" }
            )]));
        }
        self.client
            .execute(
                "UPDATE as4tainr.spooled_files SET status=$2 WHERE id=$1",
                &[&f.id, &to],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "Spooled file {} number {} {}.",
            f.file,
            f.number,
            if hold { "held" } else { "released" }
        )]))
    }

    /// CHGSPLFA: the output queue and user data (the other attributes
    /// are accepted and not kept).
    async fn chgsplfa(&mut self, cmd: &Command) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        let outq = cmd.get("OUTQ").to_ascii_uppercase();
        if outq != "*SAME" && !outq.is_empty() {
            let (lib, name) = find_queue(self.client, &outq, &self.library_list()).await?;
            authority::check(self.client, &self.user, &lib, &name, "*OUTQ", Aut::Use).await?;
            self.client
                .execute(
                    "UPDATE as4tainr.spooled_files SET outq_lib=$2, outq=$3 WHERE id=$1",
                    &[&f.id, &lib, &name],
                )
                .await?;
        }
        let usrdta = cmd.get("USRDTA");
        if usrdta != "*SAME" && !usrdta.is_empty() {
            let v: String = usrdta.chars().take(10).collect();
            self.client
                .execute(
                    "UPDATE as4tainr.spooled_files SET user_data=$2 WHERE id=$1",
                    &[&f.id, &v],
                )
                .await?;
        }
        Ok(Outcome::Messages(vec![format!(
            "Attributes of spooled file {} number {} changed.",
            f.file, f.number
        )]))
    }

    /// WRKSPLFA: a spooled file's attributes (as4tainr's own layout).
    async fn wrksplfa(&mut self, cmd: &Command) -> Result<Outcome> {
        let f = self.spooled_file(cmd).await?;
        let lib: String = self
            .client
            .query_one(
                "SELECT outq_lib FROM as4tainr.spooled_files WHERE id=$1",
                &[&f.id],
            )
            .await?
            .get(0);
        let rows = vec![
            format!("{:<32}{}", "Job", f.job_name),
            format!("{:<32}{}", "Spooled file", f.file),
            format!("{:<32}{}", "File number", f.number),
            format!("{:<32}{}", "Status", crate::sysapi::status_name(&f.status)),
            format!("{:<32}{}", "Output queue", f.outq),
            format!("{:<32}{}", "  Library", lib),
            format!("{:<32}{}", "User data", f.user_data),
            format!("{:<32}{}", "Total pages", f.pages),
            format!("{:<32}{}", "Created", f.created),
            format!("{:<32}{}", "Form type", "*STD"),
            format!("{:<32}{}", "Copies", 1),
        ];
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Spooled File Attributes".into(),
            rows,
            ..Default::default()
        }))
    }

    /// WRKOUTQ: all output queues (*ALL, generic names) or one queue's
    /// spooled files.
    async fn wrkoutq(&mut self, cmd: &Command) -> Result<Outcome> {
        let q = cmd.get("OUTQ").to_ascii_uppercase();
        let (lib, name) = match q.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), q.clone()),
        };
        let generic = name == "*ALL" || name.ends_with('*');
        if !generic {
            let (lib, name) = find_queue(self.client, &q, &self.library_list()).await?;
            authority::check(self.client, &self.user, &lib, &name, "*OUTQ", Aut::Use).await?;
            let held: bool = self
                .client
                .query_one(
                    "SELECT held FROM as4tainr.output_queues WHERE library=$1 AND name=$2",
                    &[&lib, &name],
                )
                .await?
                .get(0);
            let rows = self
                .client
                .query(
                    "SELECT file, user_name, user_data, status, pages, job_name, number
                       FROM as4tainr.spooled_files WHERE outq_lib=$1 AND outq=$2 ORDER BY id",
                    &[&lib, &name],
                )
                .await?;
            let fmt = |f: &str, u: &str, d: &str, s: &str, p: &str| {
                format!("{f:<10}  {u:<10}  {d:<10}  {s:<4}  {p:>5}")
            };
            return Ok(Outcome::Display(ListDisplay {
                title: "Work with Output Queue".into(),
                heading: fmt("File", "User", "User Data", "Sts", "Pages"),
                rows: rows
                    .iter()
                    .map(|r| {
                        fmt(
                            &r.get::<_, String>(0),
                            &r.get::<_, String>(1),
                            &r.get::<_, String>(2),
                            short_status(&r.get::<_, String>(3)),
                            &r.get::<_, i32>(4).to_string(),
                        )
                    })
                    .collect(),
                options: vec![
                    (3, "Hold".into(), "HLDSPLF {}".into()),
                    (4, "Delete".into(), "DLTSPLF {}".into()),
                    (5, "Display".into(), "DSPSPLF {}".into()),
                    (6, "Release".into(), "RLSSPLF {}".into()),
                ],
                keys: rows
                    .iter()
                    .map(|r| {
                        format!(
                            "FILE({}) JOB({}) SPLNBR({})",
                            r.get::<_, String>(0),
                            r.get::<_, String>(5),
                            r.get::<_, i32>(6)
                        )
                    })
                    .collect(),
                refresh: Some(format!("WRKOUTQ OUTQ({lib}/{name})")),
                ibm: IbmLayout::some(IbmLayout {
                    lines: vec![format!(
                        "Queue:   {name:<10}  Library:   {lib:<10}  Status:   {}",
                        if held { "HLD" } else { "RLS" }
                    )],
                    command_line: Some("Parameters or command".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
        let libs: Vec<String> = match lib.as_str() {
            "*LIBL" => self.library_list(),
            "*CURLIB" => vec![self.current_library()],
            "*ALL" => catalog::list_libraries(self.client).await?,
            l => vec![l.to_string()],
        };
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for l in &libs {
            for r in self
                .client
                .query(
                    "SELECT q.name, q.held,
                            (SELECT count(*) FROM as4tainr.spooled_files s
                              WHERE s.outq_lib=q.library AND s.outq=q.name)
                       FROM as4tainr.output_queues q WHERE q.library=$1 ORDER BY q.name",
                    &[l],
                )
                .await?
            {
                let n: String = r.get(0);
                if !catalog::name_matches(&name, &n) {
                    continue;
                }
                let held: bool = r.get(1);
                let files: i64 = r.get(2);
                rows.push(format!(
                    "{n:<10}  {l:<10}  {files:>5}  {:<10}  {}",
                    "",
                    if held { "HLD" } else { "RLS" }
                ));
                keys.push(format!("{l}/{n}"));
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with All Output Queues".into(),
            heading: format!(
                "{:<10}  {:<10}  {:>5}  {:<10}  {}",
                "Queue", "Library", "Files", "Writer", "Status"
            ),
            rows,
            options: vec![
                (3, "Hold".into(), "HLDOUTQ {}".into()),
                (4, "Delete".into(), "DLTOUTQ {}".into()),
                (5, "Work with".into(), "WRKOUTQ {}".into()),
                (6, "Release".into(), "RLSOUTQ {}".into()),
            ],
            keys,
            refresh: Some(format!("WRKOUTQ OUTQ({q})")),
            ibm: IbmLayout::some(IbmLayout {
                command_line: Some("Parameters or command".into()),
                ..Default::default()
            }),
            ..Default::default()
        }))
    }
}
