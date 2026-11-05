//! Authorization lists (*AUTL): CRTAUTL, DLTAUTL, ADDAUTLE, CHGAUTLE,
//! RMVAUTLE, DSPAUTL, WRKAUTL.
//!
//! Written from IBM's security reference and CL reference. A list is an
//! object in QSYS with an owner, a *PUBLIC authority and users' private
//! authorities, kept as any object's are (`authority.rs`: the owner and
//! `public_aut` in `as4tainr.objects`, the entries in
//! `as4tainr.object_auths`). The owner has *ALL and manages the list.
//! Messages as captured on IBM i 7.5 (PUB400, tools/golden/verify44.txt):
//! CRTAUTL CPC2207, ADDAUTLE CPI2211 per user then CPC2208, RMVAUTLE
//! CPI2213 then CPC2210 (CPD2276 and CPF2285 for a user not on it),
//! DLTAUTL CPC2191, a list that is not there CPF2283.
//!
//! Not here (docs/fidelity-gaps.md): objects secured by a list
//! (GRTOBJAUT AUTL), *AUTLMGT, EDTAUTL.

use crate::authority::{self, Aut};
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};

const LIB: &str = "QSYS";
const TYPE: &str = "*AUTL";

fn name_of(cmd: &Command) -> String {
    cmd.get("AUTL").trim().to_ascii_uppercase()
}

fn missing(name: &str) -> anyhow::Error {
    anyhow::anyhow!("CPF2283: There is no authorization list {name}.")
}

/// The users given in USER, upper case, blanks between them split.
fn users(cmd: &Command) -> Vec<String> {
    cmd.list("USER")
        .iter()
        .flat_map(|v| {
            v.split_whitespace()
                .map(str::to_ascii_uppercase)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// An authority as DSPAUTL's columns show it: (object operational,
/// object management, existence, alter, reference, read, add, update,
/// delete, execute), from the security reference's table of what *ALL,
/// *CHANGE, *USE and *EXCLUDE hold.
fn detail(a: Aut) -> [bool; 10] {
    match a {
        Aut::All => [true; 10],
        Aut::Change => [
            true, false, false, false, false, true, true, true, true, true,
        ],
        Aut::Use => [
            true, false, false, false, false, true, false, false, false, true,
        ],
        Aut::Exclude => [false; 10],
    }
}

/// DSPAUTL's printed report in IBM i 7.5's layout (PUB400, verify44
/// m44_dspautl): rows (user, authority, list management).
pub fn report(
    name: &str,
    owner: &str,
    rows: &[(String, Aut, bool)],
    stamp: &crate::report::Stamp,
) -> Vec<String> {
    use crate::commands::dsp_label;
    use crate::report::at;
    let [h1, h2] = crate::report::header_b(
        46,
        "Display Authorization List",
        &crate::report::system_name(),
        stamp,
        1,
    );
    let mut out = vec![
        h1,
        h2,
        at(&[
            (2, &dsp_label("Object", 19)),
            (27, name),
            (69, &dsp_label("Owner", 19)),
            (94, owner),
        ]),
        at(&[
            (4, &dsp_label("Library", 17)),
            (29, LIB),
            (69, &dsp_label("Primary group", 19)),
            (94, "*NONE"),
        ]),
        at(&[
            (18, "Object"),
            (31, "List"),
            (40, "----------------Object-----------------"),
            (84, "---------------------Data---------------------"),
        ]),
        at(&[
            (2, "User"),
            (17, "Authority"),
            (31, "Mgt"),
            (40, "Opr"),
            (48, "Mgt"),
            (56, "Exist"),
            (66, "Alter"),
            (76, "Ref"),
            (84, "Read"),
            (93, "Add"),
            (101, "Update"),
            (112, "Delete"),
            (123, "Execute"),
        ]),
    ];
    const MARKS: [usize; 10] = [41, 49, 58, 68, 77, 85, 94, 103, 114, 126];
    for (user, aut, mgt) in rows {
        let mut f: Vec<(usize, &str)> = vec![(2, user), (17, aut.code())];
        if *mgt {
            f.push((32, "X"));
        }
        for (col, on) in MARKS.iter().zip(detail(*aut)) {
            if on {
                f.push((*col, "X"));
            }
        }
        out.push(at(&f));
    }
    out.push(crate::report::END_OBJD.into());
    out
}

impl Job<'_> {
    pub(crate) async fn autl_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "CRTAUTL" => self.crtautl(cmd).await?,
            "DLTAUTL" => self.dltautl(cmd).await?,
            "ADDAUTLE" | "CHGAUTLE" => self.addautle(cmd).await?,
            "RMVAUTLE" => self.rmvautle(cmd).await?,
            "DSPAUTL" => self.dspautl(cmd).await?,
            "WRKAUTL" => self.wrkautl(cmd).await?,
            _ => return Ok(None),
        }))
    }

    async fn autl_exists(&self, name: &str) -> Result<bool> {
        crate::catalog::object_exists(self.client, &[LIB.into()], name, TYPE).await
    }

    async fn crtautl(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = name_of(cmd);
        crate::catalog::validate_name("authorization list", &name)?;
        let text = match cmd.get("TEXT") {
            "*BLANK" => "",
            t => t,
        };
        // *LIBCRTAUT: QSYS's CRTAUT(*SYSVAL), QCRTAUT *CHANGE as shipped
        // (PUB400 verify44 m44_dspautl: *PUBLIC *CHANGE).
        let public = match cmd.get("AUT") {
            "" | "*LIBCRTAUT" => authority::DEFAULT_PUBLIC,
            a => Aut::parse(a)?,
        };
        let n = self
            .client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                 VALUES ($1, $2, $3, '', $4) ON CONFLICT DO NOTHING",
                &[&LIB, &name, &TYPE, &text],
            )
            .await?;
        if n == 0 {
            // CPF2278 (PUB400 2026-11-04, verify45 n45_crtautl2).
            bail!("CPF2278: CRTAUTL found an authorization list named {name} in place already.");
        }
        authority::set_owner(self.client, LIB, &name, TYPE, &self.user).await?;
        authority::grant(self.client, LIB, &name, TYPE, "*PUBLIC", public).await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2207: Authorization list {name} was made."
        )]))
    }

    async fn dltautl(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = name_of(cmd);
        if !self.autl_exists(&name).await? {
            return Err(missing(&name));
        }
        authority::check(self.client, &self.user, LIB, &name, TYPE, Aut::All).await?;
        authority::forget(self.client, LIB, &name, TYPE).await?;
        self.client
            .execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
                &[&LIB, &name, &TYPE],
            )
            .await?;
        Ok(Outcome::Messages(vec![format!(
            "CPC2191: Object {name} of type *AUTL in library QSYS was deleted."
        )]))
    }

    /// ADDAUTLE (and CHGAUTLE, which changes entries that are there):
    /// each user gets the authority; CPI2211 for each, then CPC2208.
    async fn addautle(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = name_of(cmd);
        if !self.autl_exists(&name).await? {
            return Err(missing(&name));
        }
        authority::check_manage(self.client, &self.user, LIB, &name, TYPE).await?;
        // AUT is a list (*USE, *CHANGE, *ALL, *EXCLUDE): the first one
        // counts (*AUTLMGT is not kept, docs/fidelity-gaps.md).
        let aut = cmd
            .list("AUT")
            .iter()
            .flat_map(|v| {
                v.split_whitespace()
                    .map(str::to_ascii_uppercase)
                    .collect::<Vec<_>>()
            })
            .find(|a| a != "*AUTLMGT")
            .unwrap_or_else(|| "*USE".into());
        let aut = Aut::parse(&aut)?;
        let change = cmd.def.name == "CHGAUTLE";
        let mut msgs = Vec::new();
        for u in users(cmd) {
            if u != "*PUBLIC" && crate::users::get(self.client, &u).await?.is_none() {
                bail!("CPF2204: There is no user profile {u}.");
            }
            let on = u == "*PUBLIC" || self.on_list(&name, &u).await?;
            if change && !on {
                bail!("User {u} is not on authorization list {name}.");
            }
            if !change && on {
                // CPD2272 then CPF2282 with the counts (PUB400
                // 2026-11-04, verify45 n45_addautle2).
                bail!(
                    "CPD2272: {u} has an entry on authorization list {name} already.\nCPF2282: ADDAUTLE: users not added 1; authorization lists done 1."
                );
            }
            authority::grant(self.client, LIB, &name, TYPE, &u, aut).await?;
            if !change {
                msgs.push(format!(
                    "CPI2211: Authorization list {name} now has user {u}."
                ));
            }
        }
        msgs.push(if change {
            format!("Authorization list {name}: the users' authorities were changed.")
        } else {
            "CPC2208: The users were added; 1 authorization list done.".into()
        });
        Ok(Outcome::Messages(msgs))
    }

    async fn on_list(&self, name: &str, user: &str) -> Result<bool> {
        Ok(self
            .client
            .query_opt(
                "SELECT 1 FROM as4tainr.object_auths
                 WHERE library=$1 AND name=$2 AND objtype=$3 AND user_name=$4",
                &[&LIB, &name, &TYPE, &user],
            )
            .await?
            .is_some())
    }

    async fn rmvautle(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = name_of(cmd);
        if !self.autl_exists(&name).await? {
            return Err(missing(&name));
        }
        authority::check_manage(self.client, &self.user, LIB, &name, TYPE).await?;
        let mut msgs = Vec::new();
        let mut errors = 0;
        for u in users(cmd) {
            if u != "*PUBLIC" && authority::revoke(self.client, LIB, &name, TYPE, &u).await? {
                msgs.push(format!(
                    "CPI2213: User {u} is no longer on authorization list {name}."
                ));
            } else {
                errors += 1;
                msgs.push(format!("CPD2276: User {u} was not taken off the list."));
            }
        }
        if errors > 0 {
            msgs.push(format!(
                "CPF2285: {errors} users could not be removed; 1 authorization list done."
            ));
            bail!(msgs.join("\n"));
        }
        msgs.push("CPC2210: The users were removed; 1 authorization list done.".into());
        Ok(Outcome::Messages(msgs))
    }

    /// The list's rows as DSPAUTL shows them: *PUBLIC, the owner (with
    /// list management), the other users by name.
    async fn autl_rows(&self, name: &str) -> Result<(String, Vec<(String, Aut, bool)>)> {
        let Some(a) = authority::get(self.client, LIB, name, TYPE).await? else {
            return Err(missing(name));
        };
        let mut rows = vec![
            ("*PUBLIC".to_string(), a.public, false),
            (a.owner.clone(), Aut::All, true),
        ];
        rows.extend(
            a.private
                .iter()
                .filter(|(u, _)| *u != a.owner)
                .map(|(u, x)| (u.clone(), *x, false)),
        );
        Ok((a.owner, rows))
    }

    async fn dspautl(&mut self, cmd: &Command) -> Result<Outcome> {
        let name = name_of(cmd);
        let (owner, rows) = self.autl_rows(&name).await?;
        let print = report(&name, &owner, &rows, &crate::report::Stamp::now());
        let fmt = |u: &str, a: &str, m: &str| format!("{u:<10}  {a:<10}  {m}");
        Ok(Outcome::Display(ListDisplay {
            title: "Display Authorization List".into(),
            info: vec![
                ("Object".into(), name.clone()),
                ("Library".into(), LIB.into()),
                ("Owner".into(), owner),
            ],
            heading: fmt("User", "Authority", "List mgt"),
            rows: rows
                .iter()
                .map(|(u, a, m)| {
                    fmt(u, a.code(), if *m { "X" } else { "" })
                        .trim_end()
                        .to_string()
                })
                .collect(),
            print,
            ..Default::default()
        }))
    }

    async fn wrkautl(&mut self, cmd: &Command) -> Result<Outcome> {
        let spec = name_of(cmd);
        let lists = self
            .client
            .query(
                "SELECT name, text FROM as4tainr.objects WHERE library=$1 AND objtype=$2 ORDER BY name",
                &[&LIB, &TYPE],
            )
            .await?;
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        for r in lists {
            let n: String = r.get(0);
            if crate::catalog::name_matches(&spec, &n) || spec == "*ALL" {
                rows.push(format!("{n:<10}  {}", r.get::<_, String>(1)));
                keys.push(n);
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Work with Authorization Lists".into(),
            heading: format!("{:<10}  Text", "List"),
            rows,
            options: vec![
                (4, "Delete".into(), "DLTAUTL AUTL({})".into()),
                (5, "Display".into(), "DSPAUTL AUTL({})".into()),
            ],
            keys,
            refresh: Some(format!("WRKAUTL AUTL({spec})")),
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dspautl_report_as_ibm_i_prints_it() {
        // PUB400 (IBM i 7.5), verify44 m44_dspautl: list GLD44A owned by
        // AS4TRAINR, *PUBLIC *CHANGE, QUSER *USE (ADDAUTLE AUT(*USE)).
        let rows = vec![
            ("*PUBLIC".to_string(), Aut::Change, false),
            ("AS4TRAINR".to_string(), Aut::All, true),
            ("QUSER".to_string(), Aut::Use, false),
        ];
        let r = report(
            "GLD44A",
            "AS4TRAINR",
            &rows,
            &crate::report::Stamp::default(),
        );
        let want = [
            " Object . . . . . . . :   GLD44A                                    Owner  . . . . . . . :   AS4TRAINR",
            "   Library  . . . . . :     QSYS                                    Primary group  . . . :   *NONE",
            "                 Object       List     ----------------Object-----------------     ---------------------Data---------------------",
            " User           Authority     Mgt      Opr     Mgt     Exist     Alter     Ref     Read     Add     Update     Delete     Execute",
            " *PUBLIC        *CHANGE                 X                                           X        X        X          X           X",
            " AS4TRAINR      *ALL           X        X       X        X         X        X       X        X        X          X           X",
            " QUSER          *USE                    X                                           X                                        X",
            "                                        * * * * *   E N D   O F   L I S T I N G   * * * * *",
        ];
        assert_eq!(r[2..], want);
    }
}
