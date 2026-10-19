//! Object authority: who may use which object.
//!
//! Written from IBM's public security reference. Each object has an
//! owner (the user who created it; QSECOFR for system objects and objects
//! created with the `as4tainer` CLI), a *PUBLIC authority, and private
//! authorities for named users. The authorities are *ALL, *CHANGE, *USE
//! and *EXCLUDE.
//!
//! A user's authority to an object is found in this order (stopping at
//! the first that applies): the *ALLOBJ special authority (all objects);
//! being the owner (*ALL); a private authority; *PUBLIC. So a private
//! *EXCLUDE keeps a user out even when *PUBLIC is *CHANGE.
//!
//! New objects get *PUBLIC *CHANGE: the system value QCRTAUT is shipped
//! as *CHANGE, and libraries' CRTAUT(*SYSVAL) uses it.
//!
//! What needs what: reading a file, calling a program, reading a data
//! area or using a display file needs *USE; writing, updating or deleting
//! records and changing a data area need *CHANGE; deleting an object or
//! changing its authorities needs *ALL (or being the owner).

use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
ALTER TABLE as4tainer.objects ADD COLUMN IF NOT EXISTS owner text NOT NULL DEFAULT 'QSECOFR';
ALTER TABLE as4tainer.objects ADD COLUMN IF NOT EXISTS public_aut text NOT NULL DEFAULT '*CHANGE';
CREATE TABLE IF NOT EXISTS as4tainer.object_auths (
    library   text NOT NULL,
    name      text NOT NULL,
    objtype   text NOT NULL,
    user_name text NOT NULL,
    aut       text NOT NULL,
    PRIMARY KEY (library, name, objtype, user_name)
);
"#;

/// The authority given to *PUBLIC on new objects (QCRTAUT as shipped).
pub const DEFAULT_PUBLIC: Aut = Aut::Change;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Aut {
    Exclude,
    Use,
    Change,
    All,
}

impl Aut {
    pub fn parse(s: &str) -> Result<Aut> {
        Ok(match s.trim().to_ascii_uppercase().as_str() {
            "*EXCLUDE" => Aut::Exclude,
            "*USE" => Aut::Use,
            "*CHANGE" => Aut::Change,
            "*ALL" => Aut::All,
            other => bail!("Authority {other} is not valid; use *ALL, *CHANGE, *USE or *EXCLUDE."),
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            Aut::Exclude => "*EXCLUDE",
            Aut::Use => "*USE",
            Aut::Change => "*CHANGE",
            Aut::All => "*ALL",
        }
    }
}

/// An object's owner and authorities (DSPOBJAUT).
#[derive(Debug, Clone)]
pub struct ObjectAuthority {
    pub owner: String,
    pub public: Aut,
    /// Private authorities, by user name.
    pub private: Vec<(String, Aut)>,
}

/// The owner and authorities of an object, or None when there is no such
/// object.
pub async fn get(
    client: &Client,
    library: &str,
    name: &str,
    objtype: &str,
) -> Result<Option<ObjectAuthority>> {
    let Some(row) = client
        .query_opt(
            "SELECT owner, public_aut FROM as4tainer.objects WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&library, &name, &objtype],
        )
        .await?
    else {
        return Ok(None);
    };
    let private = client
        .query(
            "SELECT user_name, aut FROM as4tainer.object_auths
             WHERE library=$1 AND name=$2 AND objtype=$3 ORDER BY user_name",
            &[&library, &name, &objtype],
        )
        .await?
        .iter()
        .map(|r| Ok((r.get::<_, String>(0), Aut::parse(r.get::<_, &str>(1))?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(ObjectAuthority {
        owner: row.get(0),
        public: Aut::parse(row.get::<_, &str>(1))?,
        private,
    }))
}

/// A user's authority to an object (see the module comment for the
/// order). Objects as4tainer does not catalog (RPG programs found as source)
/// are not checked: *ALL.
pub async fn effective(
    client: &Client,
    user: &str,
    library: &str,
    name: &str,
    objtype: &str,
) -> Result<Aut> {
    if crate::users::has_special(client, user, "*ALLOBJ").await? {
        return Ok(Aut::All);
    }
    let Some(a) = get(client, library, name, objtype).await? else {
        return Ok(Aut::All);
    };
    if a.owner == user {
        return Ok(Aut::All);
    }
    if let Some((_, aut)) = a.private.iter().find(|(u, _)| u == user) {
        return Ok(*aut);
    }
    Ok(a.public)
}

/// Fail with CPF9802 unless the user has at least `need` to the object.
pub async fn check(
    client: &Client,
    user: &str,
    library: &str,
    name: &str,
    objtype: &str,
    need: Aut,
) -> Result<()> {
    if effective(client, user, library, name, objtype).await? < need {
        bail!("CPF9802: Not authorized to object {name} in {library} type {objtype}.");
    }
    Ok(())
}

/// The user who created an object owns it.
pub async fn set_owner(
    client: &Client,
    library: &str,
    name: &str,
    objtype: &str,
    owner: &str,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainer.objects SET owner=$4 WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&library, &name, &objtype, &owner],
        )
        .await?;
    Ok(())
}

/// May this user change the object's authorities (GRTOBJAUT, RVKOBJAUT,
/// CHGOBJOWN)? The owner, a user with *ALL to it, or *ALLOBJ.
pub async fn check_manage(
    client: &Client,
    user: &str,
    library: &str,
    name: &str,
    objtype: &str,
) -> Result<()> {
    if effective(client, user, library, name, objtype).await? < Aut::All {
        bail!("CPF9802: Not authorized to object {name} in {library} type {objtype}: only its owner, or a user with *ALL authority to it, can change its authorities.");
    }
    Ok(())
}

/// GRTOBJAUT: give a user (or *PUBLIC) an authority to the object.
pub async fn grant(
    client: &Client,
    library: &str,
    name: &str,
    objtype: &str,
    user: &str,
    aut: Aut,
) -> Result<()> {
    if user == "*PUBLIC" {
        client
            .execute(
                "UPDATE as4tainer.objects SET public_aut=$4 WHERE library=$1 AND name=$2 AND objtype=$3",
                &[&library, &name, &objtype, &aut.code()],
            )
            .await?;
    } else {
        client
            .execute(
                "INSERT INTO as4tainer.object_auths VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (library, name, objtype, user_name) DO UPDATE SET aut = EXCLUDED.aut",
                &[&library, &name, &objtype, &user, &aut.code()],
            )
            .await?;
    }
    Ok(())
}

/// RVKOBJAUT: a user's private authority is removed (they fall back to
/// *PUBLIC); for *PUBLIC, the public authority becomes *EXCLUDE.
pub async fn revoke(
    client: &Client,
    library: &str,
    name: &str,
    objtype: &str,
    user: &str,
) -> Result<bool> {
    if user == "*PUBLIC" {
        grant(client, library, name, objtype, "*PUBLIC", Aut::Exclude).await?;
        return Ok(true);
    }
    Ok(client
        .execute(
            "DELETE FROM as4tainer.object_auths WHERE library=$1 AND name=$2 AND objtype=$3 AND user_name=$4",
            &[&library, &name, &objtype, &user],
        )
        .await?
        > 0)
}

/// Private authorities of a deleted object go with it.
pub async fn forget(client: &Client, library: &str, name: &str, objtype: &str) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainer.object_auths WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&library, &name, &objtype],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorities_are_ordered() {
        // *ALL includes *CHANGE, which includes *USE; *EXCLUDE is nothing.
        assert!(Aut::All > Aut::Change && Aut::Change > Aut::Use && Aut::Use > Aut::Exclude);
        assert_eq!(Aut::parse("*use").unwrap(), Aut::Use);
        assert!(Aut::parse("*OBJOPR").is_err());
        assert_eq!(DEFAULT_PUBLIC.code(), "*CHANGE");
    }
}
