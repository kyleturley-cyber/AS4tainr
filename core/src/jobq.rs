//! Job queues as objects (*JOBQ): CRTJOBQ, DLTJOBQ, and WRKJOBQ's
//! printed report of one queue.
//!
//! QBATCH is the queue the QBATCH subsystem takes jobs from (`jobs.rs`;
//! its jobs have `jobq` 'QBATCH'). A queue made with CRTJOBQ is an object
//! in its library; SBMJOB can put jobs on it (`jobq` 'LIB/NAME'), where
//! they wait: no subsystem here takes jobs from other queues, as on IBM i
//! when no subsystem has a job queue entry for it (docs/fidelity-gaps.md).
//! Messages as captured on IBM i 7.5 (PUB400, tools/golden/verify44.txt):
//! CRTJOBQ CPC9801, CPF3323 when it is there already; DLTJOBQ CPC2191,
//! CPF2105.

use crate::authority::{self, Aut};
use crate::catalog;
use crate::cl::Command;
use crate::commands::{Job, Outcome};
use anyhow::{bail, Result};

const TYPE: &str = "*JOBQ";

/// A job on a queue as WRKJOBQ's report lists it.
#[derive(Debug, Clone)]
pub struct QueuedJob {
    pub name: String,
    pub user: String,
    pub number: i64,
    pub held: bool,
}

/// WRKJOBQ JOBQ(lib/name) OUTPUT(*PRINT) in IBM i 7.5's layout (PUB400,
/// verify44 m44_wrkjobq: the header with the queue and library, the
/// column headings, the end line). A queue's job lines are not captured:
/// they are written under the headings (priority 5, SBMJOB's default).
pub fn report(
    queue: &str,
    library: &str,
    jobs: &[QueuedJob],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    use crate::report::at;
    let when = format!("{} {} {}", stamp.date, stamp.time, stamp.zone);
    let mut out = vec![
        at(&[
            (2, crate::report::PRODUCT),
            (17, crate::report::LEVEL),
            (33, "Work With Job Queue"),
            (61, queue),
            (73, "in"),
            (77, library),
            (89, when.trim_end()),
            (122, "Page    1"),
        ]),
        at(&[
            (2, "Job Name"),
            (15, "User"),
            (28, "Number"),
            (37, "Priority"),
            (48, "Status"),
        ]),
    ];
    if jobs.is_empty() {
        out.push(at(&[(4, "(The job queue is empty)")]));
    }
    for j in jobs {
        let number = format!("{:06}", j.number);
        out.push(at(&[
            (2, &j.name),
            (15, &j.user),
            (28, &number),
            (44, "5"),
            (48, if j.held { "HLD" } else { "RLS" }),
        ]));
    }
    out.push("         * * * * *   E N D   O F   L I S T I N G   * * * * *".into());
    out
}

impl Job<'_> {
    pub(crate) async fn jobq_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "CRTJOBQ" => self.crtjobq(cmd).await?,
            "DLTJOBQ" => self.dltjobq(cmd).await?,
            _ => return Ok(None),
        }))
    }

    async fn crtjobq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.create_target(cmd.get("JOBQ"));
        let lib = crate::qtemp::real(&lib.to_ascii_uppercase());
        let name = catalog::validate_name("job queue", &name)?;
        let shown = crate::qtemp::logical(&lib);
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: There is no library {shown}.");
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        let n = self
            .client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                 VALUES ($1, $2, $3, '', $4) ON CONFLICT DO NOTHING",
                &[&lib, &name, &TYPE, &text],
            )
            .await?;
        if n == 0 {
            bail!("CPF3323: There is already a job queue {name} in {shown}.");
        }
        authority::set_owner(self.client, &lib, &name, TYPE, &self.user).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC9801: Job queue {name} was made in library {shown}."
        )]))
    }

    async fn dltjobq(&mut self, cmd: &Command) -> Result<Outcome> {
        let (libs, name) = self.search(cmd.get("JOBQ"));
        let name = name.to_ascii_uppercase();
        let label = cmd
            .get("JOBQ")
            .to_ascii_uppercase()
            .split_once('/')
            .map(|x| x.0.to_string())
            .unwrap_or_else(|| "*LIBL".into());
        for l in &libs {
            if !catalog::object_exists(self.client, std::slice::from_ref(l), &name, TYPE).await? {
                continue;
            }
            authority::check(self.client, &self.user, l, &name, TYPE, Aut::All).await?;
            let key = format!("{l}/{name}");
            let waiting = self
                .client
                .query_opt(
                    "SELECT 1 FROM as4tainr.jobs WHERE jobq=$1 AND status='JOBQ' LIMIT 1",
                    &[&key],
                )
                .await?
                .is_some();
            if waiting {
                bail!(
                    "Job queue {name} in {} has jobs waiting on it and was not deleted.",
                    crate::qtemp::logical(l)
                );
            }
            authority::forget(self.client, l, &name, TYPE).await?;
            self.client
                .execute(
                    "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                    &[l, &name, &TYPE],
                )
                .await?;
            return Ok(Outcome::Messages(vec![format!(
                "CPC2191: Object {name} of type *JOBQ in library {} was deleted.",
                crate::qtemp::logical(l)
            )]));
        }
        bail!("CPF2105: Object {name} of type *JOBQ in library {label} not found.");
    }

    /// A job queue named by SBMJOB or WRKJOBQ: Some((library, name, key))
    /// for QBATCH (key 'QBATCH') or a queue made with CRTJOBQ (key
    /// 'LIB/NAME'), None when there is no such queue.
    pub(crate) async fn find_job_queue(
        &self,
        spec: &str,
    ) -> Result<Option<(String, String, String)>> {
        let spec = spec.trim().to_ascii_uppercase();
        let (libs, name) = self.search(&spec);
        let qualified = spec.contains('/');
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, TYPE).await? {
                return Ok(Some((
                    crate::qtemp::logical(l),
                    name.clone(),
                    format!("{l}/{name}"),
                )));
            }
        }
        let lib = spec.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        // A queue jobs were put on by name (`jobs::submit`), as HLDJOBQ
        // and RLSJOBQ know it.
        let named = name != crate::jobs::BATCH_QUEUE
            && self
                .client
                .query_opt(
                    "SELECT 1 FROM as4tainr.jobs WHERE jobq=$1 LIMIT 1",
                    &[&name],
                )
                .await?
                .is_some();
        if named && (!qualified || matches!(lib, "*LIBL" | "QGPL" | "QSYS")) {
            return Ok(Some(("QGPL".into(), name.clone(), name)));
        }
        if name == crate::jobs::BATCH_QUEUE
            && (!qualified || matches!(lib, "*LIBL" | "QGPL" | "QSYS"))
        {
            return Ok(Some(("QGPL".into(), name, crate::jobs::BATCH_QUEUE.into())));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrkjobq_report_of_an_empty_queue() {
        // PUB400 (IBM i 7.5), verify44 m44_wrkjobq: the column headings
        // and the end line under the header (whose product, date and time
        // are as4tainr's).
        let r = report(
            "GLD44Q",
            "AS4TRAINR1",
            &[],
            &crate::report::Stamp::default(),
        );
        assert_eq!(r[0].find("Work With Job Queue"), Some(32));
        assert_eq!(r[0].find("GLD44Q"), Some(60));
        assert_eq!(r[0].find("AS4TRAINR1"), Some(76));
        assert_eq!(
            r[1],
            " Job Name     User         Number   Priority   Status"
        );
        assert_eq!(
            r[3],
            "         * * * * *   E N D   O F   L I S T I N G   * * * * *"
        );
    }
}
