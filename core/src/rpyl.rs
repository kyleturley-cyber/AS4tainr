//! The system reply list: ADDRPYLE, CHGRPYLE, RMVRPYLE, WRKRPYLE
//! (`as4tainr.reply_list`) and QSYS2.REPLY_LIST_INFO (services.rs).
//!
//! Each entry has a sequence number (1-9999), a message identifier (a
//! generic one ends in 0000: CPA0000 for every CPA message), optional
//! comparison data and its start position, the reply (*DFT, *RQD,
//! *IGNORE, or up to 32 characters) and whether the job is dumped (CL
//! reference, ADDRPYLE). IBM i ships entries 10-40 (PUB400 2026-11-10,
//! tools/golden/verify77.txt u77_rpyl: CPA0700, RPG0000, CBE0000, PLI0000,
//! reply D, dump YES); they are made at catalog initialization. A user
//! without *ALLOBJ special authority may not change the list (u77_addrpyle,
//! u77_rmvrpyle: CPD0032 and CPF0006). Jobs do not consult it yet
//! (docs/fidelity-gaps.md).

use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.reply_list (
    seqnbr  integer PRIMARY KEY,
    msgid   text NOT NULL,
    cmpdta  text,
    start   integer,
    rpy     text NOT NULL DEFAULT '*DFT',
    dump    boolean NOT NULL DEFAULT false
);
CREATE TABLE IF NOT EXISTS as4tainr.reply_list_shipped (done boolean PRIMARY KEY);
INSERT INTO as4tainr.reply_list (seqnbr, msgid, rpy, dump)
SELECT s, m, 'D', true
  FROM (VALUES (10, 'CPA0700'), (20, 'RPG0000'), (30, 'CBE0000'), (40, 'PLI0000')) v(s, m)
 WHERE NOT EXISTS (SELECT 1 FROM as4tainr.reply_list_shipped)
ON CONFLICT DO NOTHING;
INSERT INTO as4tainr.reply_list_shipped VALUES (true) ON CONFLICT DO NOTHING;
"#;

/// An entry as kept: (sequence number, message ID, comparison data and
/// start, reply, dump).
type Entry = (i32, String, Option<String>, Option<i32>, String, bool);

/// A reply as kept: a special value in capitals, else the characters as
/// given (quotes removed: u77_rpyl shows `x` and `NOMAX`).
fn reply(v: &str) -> Result<String> {
    let t = v.trim();
    let up = t.to_ascii_uppercase();
    if matches!(up.as_str(), "*DFT" | "*RQD" | "*IGNORE") {
        return Ok(up);
    }
    let r = t.trim_matches('\'').to_string();
    if r.is_empty() || r.chars().count() > 32 {
        return Err(crate::cl::value_error(&format!(
            "Value '{t}' for parameter RPY is not valid: a reply has 1 to 32 characters."
        )));
    }
    Ok(r)
}

fn seqnbr(v: &str) -> Result<i32> {
    match v.trim().parse::<i32>() {
        Ok(n) if (1..=9999).contains(&n) => Ok(n),
        _ => Err(crate::cl::value_error(&format!(
            "Value '{}' for parameter SEQNBR is not valid: 1 to 9999.",
            v.trim()
        ))),
    }
}

fn msgid(v: &str) -> Result<String> {
    let up = v.trim().to_ascii_uppercase();
    let ok = up == "*ANY"
        || (up.len() == 7
            && up[..3].chars().all(|c| c.is_ascii_alphanumeric())
            && up[3..].chars().all(|c| c.is_ascii_hexdigit()));
    if !ok {
        return Err(crate::cl::value_error(&format!(
            "Value '{}' for parameter MSGID is not valid.",
            v.trim()
        )));
    }
    Ok(up)
}

/// CMPDTA(data [start]): the comparison data and its start position.
fn cmpdta(list: &[String]) -> Result<(Option<String>, Option<i32>)> {
    let Some(d) = list.first() else {
        return Ok((None, None));
    };
    if d.trim().eq_ignore_ascii_case("*NONE") {
        return Ok((None, None));
    }
    let data = d.trim().trim_matches('\'').to_string();
    if data.chars().count() > 28 {
        return Err(crate::cl::value_error(
            "Value for parameter CMPDTA is not valid: 28 characters at most.",
        ));
    }
    let start = match list.get(1) {
        Some(s) => s
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|n| (1..=999).contains(n))
            .ok_or_else(|| crate::cl::value_error("CMPDTA's start position is 1 to 999."))?,
        None => 1,
    };
    Ok((Some(data), Some(start)))
}

/// Does a reply list entry's message identifier take this message (a
/// generic one, ending in 0000 or 00, its prefix: CL reference, ADDRPYLE
/// MSGID)?
pub fn matches(entry: &str, msgid: &str) -> bool {
    if entry == "*ANY" || entry == msgid {
        return true;
    }
    let generic = entry.trim_end_matches('0');
    entry.len() == 7 && entry.ends_with("00") && msgid.starts_with(generic) && generic.len() >= 3
}

impl Job<'_> {
    pub(crate) async fn rpyl_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "ADDRPYLE" => self.addrpyle(cmd, false).await?,
            "CHGRPYLE" => self.addrpyle(cmd, true).await?,
            "RMVRPYLE" => self.rmvrpyle(cmd).await?,
            "WRKRPYLE" => self.wrkrpyle(cmd).await?,
            _ => return Ok(None),
        }))
    }

    /// Changing the list needs *ALLOBJ (u77_addrpyle: CPD0032, CPF0006).
    async fn may_change_reply_list(&self, cmd: &str) -> Result<()> {
        if !crate::users::has_special(self.client, &self.user, "*ALLOBJ").await? {
            bail!(
                "CPD0032: {} may not run {cmd}: it changes the system reply list.\n{}",
                self.user,
                crate::cl::CPF0006
            );
        }
        Ok(())
    }

    async fn entry(&self, n: i32) -> Result<Option<Entry>> {
        Ok(self
            .client
            .query_opt(
                "SELECT seqnbr, msgid, cmpdta, start, rpy, dump FROM as4tainr.reply_list WHERE seqnbr = $1",
                &[&n],
            )
            .await?
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5))))
    }

    async fn addrpyle(&mut self, cmd: &Command, change: bool) -> Result<Outcome> {
        let name = if change { "CHGRPYLE" } else { "ADDRPYLE" };
        self.may_change_reply_list(name).await?;
        let n = seqnbr(cmd.get("SEQNBR"))?;
        let old = self.entry(n).await?;
        let same = |k: &str| change && cmd.get(k).trim().eq_ignore_ascii_case("*SAME");
        let (msg, data, start, rpy, dump) = match (change, old) {
            (false, Some(_)) => bail!("Reply list entry {n} is there already."),
            (true, None) => bail!("There is no reply list entry {n}."),
            (_, old) => {
                let old = old.unwrap_or((n, String::new(), None, None, "*DFT".into(), false));
                let msg = if same("MSGID") {
                    old.1
                } else {
                    msgid(cmd.get("MSGID"))?
                };
                let (data, start) = if same("CMPDTA") {
                    (old.2, old.3)
                } else {
                    cmpdta(&cmd.list("CMPDTA"))?
                };
                let rpy = if same("RPY") {
                    old.4
                } else {
                    reply(cmd.get("RPY"))?
                };
                let dump = if same("DUMP") {
                    old.5
                } else {
                    cmd.get("DUMP").trim().eq_ignore_ascii_case("*YES")
                };
                (msg, data, start, rpy, dump)
            }
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.reply_list (seqnbr, msgid, cmpdta, start, rpy, dump)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (seqnbr) DO UPDATE SET msgid = $2, cmpdta = $3, start = $4, rpy = $5, dump = $6",
                &[&n, &msg, &data, &start, &rpy, &dump],
            )
            .await?;
        Ok(Outcome::Messages(vec![if change {
            format!("Reply list entry {n} changed.")
        } else {
            format!("Reply list entry {n} added.")
        }]))
    }

    async fn rmvrpyle(&mut self, cmd: &Command) -> Result<Outcome> {
        self.may_change_reply_list("RMVRPYLE").await?;
        let v = cmd.get("SEQNBR").trim().to_ascii_uppercase();
        if v == "*ALL" {
            let k = self
                .client
                .execute("DELETE FROM as4tainr.reply_list", &[])
                .await?;
            return Ok(Outcome::Messages(vec![format!(
                "{k} reply list entries removed."
            )]));
        }
        let n = seqnbr(&v)?;
        let k = self
            .client
            .execute("DELETE FROM as4tainr.reply_list WHERE seqnbr = $1", &[&n])
            .await?;
        if k == 0 {
            bail!("There is no reply list entry {n}.");
        }
        Ok(Outcome::Messages(vec![format!(
            "Reply list entry {n} removed."
        )]))
    }

    async fn wrkrpyle(&mut self, cmd: &Command) -> Result<Outcome> {
        let only = cmd.get("SEQNBR").trim().to_ascii_uppercase();
        let rows = self
            .client
            .query(
                "SELECT seqnbr, msgid, coalesce(cmpdta, '*NONE'), rpy, dump FROM as4tainr.reply_list ORDER BY seqnbr",
                &[],
            )
            .await?;
        let fmt = |s: &str, m: &str, c: &str, r: &str, d: &str| {
            format!("{s:>8}  {m:<7}  {c:<28}  {r:<32}  {d}")
        };
        let rows: Vec<String> = rows
            .iter()
            .filter(|r| only.is_empty() || only == "*ALL" || r.get::<_, i32>(0).to_string() == only)
            .map(|r| {
                let dump: bool = r.get(4);
                fmt(
                    &r.get::<_, i32>(0).to_string(),
                    r.get::<_, &str>(1),
                    r.get::<_, &str>(2),
                    r.get::<_, &str>(3),
                    if dump { "*YES" } else { "*NO" },
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Work with System Reply List Entries".into(),
            heading: fmt("Sequence", "Message", "Compare value", "Reply", "Dump"),
            rows,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_message_identifiers() {
        // CL reference, ADDRPYLE MSGID: CPA0000 for every CPA message,
        // CPA0700 for CPA0701-CPA07FF (u77_rpyl's entry 10).
        assert!(matches("CPA0700", "CPA0702"));
        assert!(matches("RPG0000", "RPG1216"));
        assert!(!matches("CPA0700", "CPA5305"));
        assert!(matches("CPA7025", "CPA7025"));
        assert!(!matches("CPA7025", "CPA7026"));
        assert!(matches("*ANY", "CPF1234"));
    }

    #[test]
    fn replies_as_kept() {
        // u77_rpyl: replies D, x, NOMAX as given.
        assert_eq!(reply("'x'").unwrap(), "x");
        assert_eq!(reply("NOMAX").unwrap(), "NOMAX");
        assert_eq!(reply("*dft").unwrap(), "*DFT");
        assert!(reply(&"y".repeat(33)).is_err());
    }
}
