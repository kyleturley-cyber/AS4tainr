//! Job schedule entries: ADDJOBSCDE, CHGJOBSCDE, RMVJOBSCDE,
//! HLDJOBSCDE, RLSJOBSCDE, WRKJOBSCDE, and the submitting of their jobs
//! when they are due.
//!
//! Written from the CL reference (ADDJOBSCDE: FRQ *ONCE, *WEEKLY and
//! *MONTHLY; SCDDATE a date, *CURRENT, *MONTHSTR or *MONTHEND, or SCDDAY
//! days of the week; SCDTIME; SAVE; JOBQ, USER, TEXT; ENTRYNBR *ONLY,
//! *ALL or a number). Entries are `as4tainr.job_schedule`; the QBATCH
//! subsystem (`jobs::run_subsystem`) submits the jobs of the entries that
//! are due and works out each one's next time (`next_run`). PUB400's
//! users may not use these commands, so only CPF1628 (an entry that is
//! not there, tools/golden/verify44.txt m44_hldscde) was captured; the
//! other messages have no ID here. Times are the system's (UTC here, as
//! every time in as4tainr: docs/fidelity-gaps.md). Not here: RELDAYMON,
//! OMITDATE, RCYACN other than submitting a missed job when the system
//! is up again (*SBMRLS), JOBD, MSGQ (messages go to the job log).

use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use crate::rpg::dates::{self, Hms, Ymd};
use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.job_schedule (
    name       text NOT NULL,
    number     integer NOT NULL,
    command    text NOT NULL,
    frequency  text NOT NULL,
    scddate    text NOT NULL,   -- *NONE, *MONTHSTR, *MONTHEND or YYYY-MM-DD
    scdday     text NOT NULL,   -- *NONE, *ALL or days (*MON *TUE ...)
    scdtime    text NOT NULL,   -- HH:MM:SS
    jobq       text NOT NULL,
    user_name  text NOT NULL,
    held       boolean NOT NULL DEFAULT false,
    save       boolean NOT NULL DEFAULT false,
    text       text NOT NULL DEFAULT '',
    next_run   text,            -- YYYY-MM-DD HH:MM:SS, NULL: none (saved)
    last_run   text,
    PRIMARY KEY (name, number)
);
"#;

const DAYS: [&str; 7] = ["*MON", "*TUE", "*WED", "*THU", "*FRI", "*SAT", "*SUN"];

/// Monday 0 ... Sunday 6.
fn weekday(d: Ymd) -> usize {
    // 1970-01-01 was a Thursday (3).
    (dates::days(d) - dates::days((1970, 1, 1)) + 3).rem_euclid(7) as usize
}

fn add_days(d: Ymd, n: i64) -> Ymd {
    dates::from_days(dates::days(d) + n)
}

fn stamp(d: Ymd, t: Hms) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        d.0, d.1, d.2, t.0, t.1, t.2
    )
}

/// How an entry is scheduled.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub frequency: String,
    pub date: String,
    pub days: Vec<usize>,
    pub time: Hms,
}

/// The first time after `now` the schedule says, or None (a *ONCE entry
/// whose time has passed).
pub fn next_run(s: &Schedule, now: (Ymd, Hms)) -> Option<(Ymd, Hms)> {
    let (today, t_now) = now;
    let later = |d: Ymd| d > today || (d == today && s.time > t_now);
    let month_day = |y: i32, m: u32, want: u32| (y, m, want.min(dates::month_days(y, m)));
    let next_month = |(y, m): (i32, u32)| if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    if !s.days.is_empty() {
        // The next of the days of the week, today included when its
        // time is still to come (*ONCE and *WEEKLY alike).
        return (0..8)
            .map(|k| add_days(today, k))
            .find(|d| s.days.contains(&weekday(*d)) && later(*d))
            .map(|d| (d, s.time));
    }
    match (s.frequency.as_str(), s.date.as_str()) {
        (_, "*MONTHSTR") | (_, "*MONTHEND") => {
            let end = s.date == "*MONTHEND";
            let mut ym = (today.0, today.1);
            for _ in 0..3 {
                let d = if end {
                    month_day(ym.0, ym.1, 31)
                } else {
                    (ym.0, ym.1, 1)
                };
                if later(d) {
                    return Some((d, s.time));
                }
                ym = next_month(ym);
            }
            None
        }
        (f, date) => {
            let d = parse_iso(date)?;
            match f {
                "*ONCE" => later(d).then_some((d, s.time)),
                "*WEEKLY" => {
                    let mut d = d;
                    if !later(d) {
                        let behind = dates::days(today) - dates::days(d);
                        d = add_days(d, behind.div_euclid(7) * 7);
                        while !later(d) {
                            d = add_days(d, 7);
                        }
                    }
                    Some((d, s.time))
                }
                _ => {
                    // *MONTHLY: the date's day of the month, each month.
                    if later(d) {
                        return Some((d, s.time));
                    }
                    let mut ym = (today.0, today.1);
                    for _ in 0..3 {
                        let c = month_day(ym.0, ym.1, d.2);
                        if later(c) {
                            return Some((c, s.time));
                        }
                        ym = next_month(ym);
                    }
                    None
                }
            }
        }
    }
}

fn parse_iso(s: &str) -> Option<Ymd> {
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    Some((p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?))
}

/// SCDDATE in the job's date format (QDATFMT: MDY, YMD, DMY), with or
/// without separators, 2- or 4-digit year.
pub fn parse_date(v: &str, datfmt: &str) -> Result<Ymd> {
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    let bad = || anyhow::anyhow!("SCDDATE {v} is not a date in the job's date format ({datfmt}).");
    let (a, b, c) = match digits.len() {
        6 => (&digits[0..2], &digits[2..4], &digits[4..6]),
        8 if datfmt == "YMD" => (&digits[0..4], &digits[4..6], &digits[6..8]),
        8 => (&digits[0..2], &digits[2..4], &digits[4..8]),
        _ => return Err(bad()),
    };
    let year = |y: &str| -> Result<i32> {
        let n: i32 = y.parse()?;
        Ok(if y.len() == 2 {
            if n < 40 {
                2000 + n
            } else {
                1900 + n
            }
        } else {
            n
        })
    };
    let (y, m, d) = match datfmt {
        "YMD" => (year(a)?, b.parse()?, c.parse()?),
        "DMY" => (year(c)?, b.parse()?, a.parse()?),
        _ => (year(c)?, a.parse()?, b.parse()?),
    };
    if !dates::valid((y, m, d)) {
        return Err(bad());
    }
    Ok((y, m, d))
}

/// SCDTIME: HHMM or HHMMSS, separators allowed.
pub fn parse_time(v: &str) -> Result<Hms> {
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    let p = |s: &str| s.parse::<u32>().unwrap_or(99);
    let t = match digits.len() {
        4 => (p(&digits[0..2]), p(&digits[2..4]), 0),
        6 => (p(&digits[0..2]), p(&digits[2..4]), p(&digits[4..6])),
        _ => bail!("SCDTIME {v} is not a time (HHMMSS)."),
    };
    if t.0 > 23 || t.1 > 59 || t.2 > 59 {
        bail!("SCDTIME {v} is not a time (HHMMSS).");
    }
    Ok(t)
}

fn now() -> (Ymd, Hms) {
    let (d, t, _) = dates::now();
    (d, t)
}

fn now_text() -> String {
    let (d, t) = now();
    stamp(d, t)
}

/// A row of `as4tainr.job_schedule`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub number: i32,
    pub command: String,
    pub schedule: Schedule,
    pub jobq: String,
    pub user: String,
    pub held: bool,
    pub save: bool,
    pub text: String,
    pub next_run: Option<String>,
}

const COLS: &str = "name, number, command, frequency, scddate, scdday, scdtime, jobq, user_name, held, save, text, next_run";

fn entry(r: &tokio_postgres::Row) -> Entry {
    let days: String = r.get(5);
    let time: String = r.get(6);
    Entry {
        name: r.get(0),
        number: r.get(1),
        command: r.get(2),
        schedule: Schedule {
            frequency: r.get(3),
            date: r.get(4),
            days: day_numbers(&days),
            time: parse_time(&time).unwrap_or((0, 0, 0)),
        },
        jobq: r.get(7),
        user: r.get(8),
        held: r.get(9),
        save: r.get(10),
        text: r.get(11),
        next_run: r.get(12),
    }
}

fn day_numbers(days: &str) -> Vec<usize> {
    if days.trim() == "*ALL" {
        return (0..7).collect();
    }
    days.split_whitespace()
        .filter_map(|d| DAYS.iter().position(|x| x.eq_ignore_ascii_case(d)))
        .collect()
}

/// Submit the jobs of the entries that are due (the QBATCH subsystem
/// calls this as it polls); each entry then gets its next time, or is
/// removed (a *ONCE entry with SAVE(*NO)).
pub async fn submit_due(client: &Client) -> Result<usize> {
    let at = now_text();
    let due: Vec<Entry> = client
        .query(
            &format!(
                "SELECT {COLS} FROM as4tainr.job_schedule
                  WHERE NOT held AND next_run IS NOT NULL AND next_run <= $1 ORDER BY next_run, name, number"
            ),
            &[&at],
        )
        .await?
        .iter()
        .map(entry)
        .collect();
    for e in &due {
        let p = crate::users::get(client, &e.user).await?;
        let curlib = p
            .as_ref()
            .map(|p| p.curlib.clone())
            .filter(|c| c != "*CRTDFT" && !c.is_empty());
        crate::jobs::submit(
            client,
            &crate::jobs::Submit {
                name: e.name.clone(),
                user: e.user.clone(),
                command: e.command.clone(),
                jobq: e.jobq.clone(),
                curlib,
                libl: crate::commands::USER_LIBRARY_LIST
                    .iter()
                    .map(|l| l.to_string())
                    .collect(),
                submitted_by: e.user.clone(),
                submitter: None,
            },
        )
        .await?;
        let next = next_run(&e.schedule, now()).map(|(d, t)| stamp(d, t));
        if next.is_none() && !e.save {
            client
                .execute(
                    "DELETE FROM as4tainr.job_schedule WHERE name=$1 AND number=$2",
                    &[&e.name, &e.number],
                )
                .await?;
        } else {
            client
                .execute(
                    "UPDATE as4tainr.job_schedule SET next_run=$3, last_run=$4 WHERE name=$1 AND number=$2",
                    &[&e.name, &e.number, &next, &at],
                )
                .await?;
        }
    }
    Ok(due.len())
}

/// The entries WRKJOBSCDE shows and the other commands act on.
async fn entries(client: &Client, job: &str) -> Result<Vec<Entry>> {
    let rows = client
        .query(
            &format!("SELECT {COLS} FROM as4tainr.job_schedule ORDER BY name, number"),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(entry)
        .filter(|e| job == "*ALL" || crate::catalog::name_matches(job, &e.name))
        .collect())
}

impl Job<'_> {
    pub(crate) async fn jobscde_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "ADDJOBSCDE" => self.addjobscde(cmd).await?,
            "CHGJOBSCDE" => self.chgjobscde(cmd).await?,
            "RMVJOBSCDE" | "HLDJOBSCDE" | "RLSJOBSCDE" => self.act_on_entries(cmd).await?,
            "WRKJOBSCDE" => self.wrkjobscde(cmd).await?,
            _ => return Ok(None),
        }))
    }

    /// The schedule ADDJOBSCDE or CHGJOBSCDE gives (`old`: CHGJOBSCDE's
    /// entry, whose values *SAME keeps).
    fn schedule_of(&self, cmd: &Command, old: Option<&Schedule>) -> Result<Schedule> {
        let up = |k: &str| cmd.get(k).trim().to_ascii_uppercase();
        let same = |k: &str| old.is_some() && up(k) == "*SAME";
        let frequency = if same("FRQ") {
            old.map(|o| o.frequency.clone()).unwrap_or_default()
        } else {
            let f = up("FRQ");
            if !matches!(f.as_str(), "*ONCE" | "*WEEKLY" | "*MONTHLY") {
                return Err(crate::cl::diag_error(
                    "CPD0084",
                    &format!("FRQ cannot be {f}: use *ONCE, *WEEKLY or *MONTHLY."),
                ));
            }
            f
        };
        let datfmt = crate::clpgm::system_value_text("QDATFMT")
            .and_then(|v| v.ok())
            .unwrap_or_else(|| "MDY".into());
        let (today, t_now) = now();
        let date = if same("SCDDATE") {
            old.map(|o| o.date.clone()).unwrap_or_default()
        } else {
            match up("SCDDATE").as_str() {
                "*CURRENT" => format!("{:04}-{:02}-{:02}", today.0, today.1, today.2),
                v @ ("*NONE" | "*MONTHSTR" | "*MONTHEND") => v.to_string(),
                v => {
                    let d = parse_date(v, &datfmt)?;
                    format!("{:04}-{:02}-{:02}", d.0, d.1, d.2)
                }
            }
        };
        let days: Vec<usize> = if same("SCDDAY") {
            old.map(|o| o.days.clone()).unwrap_or_default()
        } else {
            let words: Vec<String> = cmd
                .list("SCDDAY")
                .iter()
                .flat_map(|v| {
                    v.split_whitespace()
                        .map(str::to_ascii_uppercase)
                        .collect::<Vec<_>>()
                })
                .collect();
            if words.iter().any(|w| w == "*NONE") || words.is_empty() {
                vec![]
            } else {
                let mut d = Vec::new();
                for w in &words {
                    if w == "*ALL" {
                        d.extend(0..7);
                    } else if let Some(i) = DAYS.iter().position(|x| x == w) {
                        d.push(i);
                    } else {
                        return Err(crate::cl::diag_error(
                            "CPD0084",
                            &format!("SCDDAY cannot be {w}: use *NONE, *ALL or *MON through *SUN."),
                        ));
                    }
                }
                d.sort_unstable();
                d.dedup();
                d
            }
        };
        let time = if same("SCDTIME") {
            old.map(|o| o.time).unwrap_or((0, 0, 0))
        } else {
            match up("SCDTIME").as_str() {
                "*CURRENT" => t_now,
                v => parse_time(v)?,
            }
        };
        // The CL reference: a date or days of the week, not both, and one
        // of them.
        let dated = date != "*NONE";
        if dated == !days.is_empty() {
            bail!("Give either SCDDATE or SCDDAY (the other *NONE).");
        }
        if frequency == "*MONTHLY" && !days.is_empty() {
            bail!("FRQ(*MONTHLY) with SCDDAY needs RELDAYMON, which as4tainr does not have: give SCDDATE.");
        }
        Ok(Schedule {
            frequency,
            date,
            days,
            time,
        })
    }

    async fn addjobscde(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = crate::catalog::validate_name("job", cmd.get("JOB"))?;
        let command = cmd.get("CMD").trim().to_string();
        crate::cl::parse(&command)?;
        let s = self.schedule_of(cmd, None)?;
        // SCDDATE(*CURRENT) with a time that has passed: tomorrow.
        let mut s = s;
        let mut next = next_run(&s, now());
        if next.is_none()
            && s.frequency == "*ONCE"
            && cmd.get("SCDDATE").trim().eq_ignore_ascii_case("*CURRENT")
        {
            let (today, _) = now();
            let d = add_days(today, 1);
            s.date = format!("{:04}-{:02}-{:02}", d.0, d.1, d.2);
            next = next_run(&s, now());
        }
        let Some((nd, nt)) = next else {
            bail!("Job schedule entry {name} was not added: its date and time have passed.");
        };
        let jobq = match cmd.get("JOBQ") {
            "*JOBD" | "" => crate::jobs::BATCH_QUEUE.to_string(),
            q => match self.find_job_queue(q).await? {
                Some((_, _, key)) => key,
                None => bail!("Job queue {q} not found."),
            },
        };
        let user = match cmd.get("USER").trim().to_ascii_uppercase().as_str() {
            "*CURRENT" | "" => self.user.clone(),
            "*JOBD" => self.user.clone(),
            u => {
                if crate::users::get(self.client, u).await?.is_none() {
                    bail!("CPF2204: There is no user profile {u}.");
                }
                if u != self.user && self.require_secadm("").await.is_err() {
                    bail!("Not authorized to schedule jobs for user {u}.");
                }
                u.to_string()
            }
        };
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        let save = cmd.get("SAVE").eq_ignore_ascii_case("*YES");
        let number: i32 = self
            .client
            .query_one(
                "SELECT COALESCE(max(number), 0) + 1 FROM as4tainr.job_schedule WHERE name=$1",
                &[&name],
            )
            .await?
            .get(0);
        let days = s
            .days
            .iter()
            .map(|d| DAYS[*d])
            .collect::<Vec<_>>()
            .join(" ");
        let days = if days.is_empty() {
            "*NONE".into()
        } else {
            days
        };
        let time = format!("{:02}:{:02}:{:02}", s.time.0, s.time.1, s.time.2);
        self.client
            .execute(
                "INSERT INTO as4tainr.job_schedule
                   (name, number, command, frequency, scddate, scdday, scdtime, jobq, user_name, save, text, next_run)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
                &[
                    &name,
                    &number,
                    &command,
                    &s.frequency,
                    &s.date,
                    &days,
                    &time,
                    &jobq,
                    &user,
                    &save,
                    &text,
                    &stamp(nd, nt),
                ],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "Job schedule entry {name} number {number:06} was added; its job is next submitted {}.",
            stamp(nd, nt)
        )]))
    }

    /// The entries JOB and ENTRYNBR name (*ONLY: the one entry of that
    /// name; *ALL: all of them, RMVJOBSCDE). CPF1628 when there is none.
    async fn named_entries(&self, cmd: &Command) -> Result<Vec<Entry>> {
        let job = cmd.get("JOB").trim().to_ascii_uppercase();
        let nbr = cmd.get("ENTRYNBR").trim().to_ascii_uppercase();
        let all = entries(self.client, &job).await?;
        let chosen: Vec<Entry> = match nbr.as_str() {
            "*ONLY" => {
                if all.len() > 1 {
                    bail!(
                        "There are {} job schedule entries {job}: give ENTRYNBR.",
                        all.len()
                    );
                }
                all
            }
            "*ALL" => all,
            n => {
                let n: i32 = n.parse().unwrap_or(-1);
                all.into_iter().filter(|e| e.number == n).collect()
            }
        };
        if chosen.is_empty() {
            bail!("CPF1628: There is no job schedule entry {job} number {nbr}.");
        }
        for e in &chosen {
            if e.user != self.user && self.require_secadm("").await.is_err() {
                bail!(
                    "Not authorized to job schedule entry {} number {:06}.",
                    e.name,
                    e.number
                );
            }
        }
        Ok(chosen)
    }

    async fn act_on_entries(&mut self, cmd: &Command) -> Result<Outcome> {
        let chosen = self.named_entries(cmd).await?;
        let mut msgs = Vec::new();
        for e in &chosen {
            let (sql, done) = match cmd.def.name {
                "RMVJOBSCDE" => (
                    "DELETE FROM as4tainr.job_schedule WHERE name=$1 AND number=$2",
                    "removed",
                ),
                "HLDJOBSCDE" => (
                    "UPDATE as4tainr.job_schedule SET held=true WHERE name=$1 AND number=$2",
                    "held",
                ),
                _ => (
                    "UPDATE as4tainr.job_schedule SET held=false WHERE name=$1 AND number=$2",
                    "released",
                ),
            };
            self.client.execute(sql, &[&e.name, &e.number]).await?;
            msgs.push(format!(
                "Job schedule entry {} number {:06} was {done}.",
                e.name, e.number
            ));
        }
        Ok(Outcome::Messages(msgs))
    }

    async fn chgjobscde(&mut self, cmd: &Command) -> Result<Outcome> {
        let chosen = self.named_entries(cmd).await?;
        let e = &chosen[0];
        let s = self.schedule_of(cmd, Some(&e.schedule))?;
        let Some((nd, nt)) = next_run(&s, now()) else {
            bail!(
                "Job schedule entry {} was not changed: its date and time have passed.",
                e.name
            );
        };
        let command = match cmd.get("CMD").trim() {
            "*SAME" | "" => e.command.clone(),
            c => {
                crate::cl::parse(c)?;
                c.to_string()
            }
        };
        let text = match cmd.get("TEXT") {
            "*SAME" => e.text.clone(),
            "*BLANK" => String::new(),
            t => t.to_string(),
        };
        let days = s
            .days
            .iter()
            .map(|d| DAYS[*d])
            .collect::<Vec<_>>()
            .join(" ");
        let days = if days.is_empty() {
            "*NONE".into()
        } else {
            days
        };
        let time = format!("{:02}:{:02}:{:02}", s.time.0, s.time.1, s.time.2);
        self.client
            .execute(
                "UPDATE as4tainr.job_schedule SET command=$3, frequency=$4, scddate=$5, scdday=$6,
                        scdtime=$7, text=$8, next_run=$9 WHERE name=$1 AND number=$2",
                &[
                    &e.name,
                    &e.number,
                    &command,
                    &s.frequency,
                    &s.date,
                    &days,
                    &time,
                    &text,
                    &stamp(nd, nt),
                ],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "Job schedule entry {} number {:06} was changed; its job is next submitted {}.",
            e.name,
            e.number,
            stamp(nd, nt)
        )]))
    }

    async fn wrkjobscde(&mut self, cmd: &Command) -> Result<Outcome> {
        let job = cmd.get("JOB").trim().to_ascii_uppercase();
        let list = entries(self.client, &job).await?;
        let fmt = |n: &str, nbr: &str, st: &str, sched: &str, frq: &str, next: &str, text: &str| {
            format!("{n:<10}  {nbr:<6}  {st:<3}  {sched:<20}  {frq:<8}  {next:<19}  {text}")
                .trim_end()
                .to_string()
        };
        let rows: Vec<String> = list
            .iter()
            .map(|e| {
                let s = &e.schedule;
                let when = if s.days.is_empty() {
                    s.date.clone()
                } else {
                    s.days
                        .iter()
                        .map(|d| DAYS[*d])
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                let sched = format!("{when} {:02}:{:02}:{:02}", s.time.0, s.time.1, s.time.2);
                let status = if e.held {
                    "HLD"
                } else if e.next_run.is_none() {
                    "SAV"
                } else {
                    "SCD"
                };
                fmt(
                    &e.name,
                    &format!("{:06}", e.number),
                    status,
                    &sched,
                    &s.frequency,
                    e.next_run.as_deref().unwrap_or(""),
                    &e.text,
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Job Schedule Entries".into(),
            heading: fmt(
                "Job",
                "Number",
                "Sts",
                "Schedule",
                "Frequency",
                "Next submit",
                "Text",
            ),
            keys: list
                .iter()
                .map(|e| format!("JOB({}) ENTRYNBR({:06})", e.name, e.number))
                .collect(),
            rows,
            options: vec![
                (3, "Hold".into(), "HLDJOBSCDE {}".into()),
                (4, "Remove".into(), "RMVJOBSCDE {}".into()),
                (6, "Release".into(), "RLSJOBSCDE {}".into()),
            ],
            refresh: Some(format!("WRKJOBSCDE JOB({job})")),
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(f: &str, date: &str, days: &[usize], time: Hms) -> Schedule {
        Schedule {
            frequency: f.into(),
            date: date.into(),
            days: days.to_vec(),
            time,
        }
    }

    #[test]
    fn weekdays() {
        // 2026-11-04 is a Wednesday, 2026-11-08 a Sunday (calendar).
        assert_eq!(weekday((2026, 11, 4)), 2);
        assert_eq!(weekday((2026, 11, 8)), 6);
    }

    #[test]
    fn next_times_as_the_cl_reference_describes_them() {
        // ADDJOBSCDE (CL reference): FRQ(*WEEKLY) SCDDAY(*SUN) at 23:59
        // from Wednesday 2026-11-04 is Sunday 2026-11-08.
        let now = ((2026, 11, 4), (17, 0, 0));
        assert_eq!(
            next_run(&sched("*WEEKLY", "*NONE", &[6], (23, 59, 0)), now),
            Some(((2026, 11, 8), (23, 59, 0)))
        );
        // Today's day, time still to come: today.
        assert_eq!(
            next_run(&sched("*WEEKLY", "*NONE", &[2], (18, 0, 0)), now),
            Some(((2026, 11, 4), (18, 0, 0)))
        );
        // Today's day, time passed: a week later.
        assert_eq!(
            next_run(&sched("*WEEKLY", "*NONE", &[2], (9, 0, 0)), now),
            Some(((2026, 11, 11), (9, 0, 0)))
        );
        // *ONCE on a date that has passed: none.
        assert_eq!(
            next_run(&sched("*ONCE", "2026-11-03", &[], (9, 0, 0)), now),
            None
        );
        // *WEEKLY from a date: every seven days after it.
        assert_eq!(
            next_run(&sched("*WEEKLY", "2026-10-01", &[], (9, 0, 0)), now),
            Some(((2026, 11, 5), (9, 0, 0)))
        );
        // *MONTHLY on the 31st: the month's last day when it is shorter.
        assert_eq!(
            next_run(&sched("*MONTHLY", "2026-10-31", &[], (9, 0, 0)), now),
            Some(((2026, 11, 30), (9, 0, 0)))
        );
        // *MONTHEND and *MONTHSTR.
        assert_eq!(
            next_run(&sched("*MONTHLY", "*MONTHEND", &[], (9, 0, 0)), now),
            Some(((2026, 11, 30), (9, 0, 0)))
        );
        assert_eq!(
            next_run(&sched("*MONTHLY", "*MONTHSTR", &[], (9, 0, 0)), now),
            Some(((2026, 12, 1), (9, 0, 0)))
        );
    }

    #[test]
    fn dates_in_the_job_format() {
        assert_eq!(parse_date("110826", "MDY").unwrap(), (2026, 11, 8));
        assert_eq!(parse_date("11/08/26", "MDY").unwrap(), (2026, 11, 8));
        assert_eq!(parse_date("261108", "YMD").unwrap(), (2026, 11, 8));
        assert_eq!(parse_date("08112026", "DMY").unwrap(), (2026, 11, 8));
        assert!(parse_date("133126", "MDY").is_err());
        assert_eq!(parse_time("235900").unwrap(), (23, 59, 0));
        assert_eq!(parse_time("23:59").unwrap(), (23, 59, 0));
        assert!(parse_time("2460").is_err());
    }
}
