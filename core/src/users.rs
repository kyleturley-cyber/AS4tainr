//! User profiles (*USRPRF) and sign-on checks.
//!
//! Passwords are stored as Argon2id hashes (PHC string format), never in
//! clear text. The first `init` creates QSECOFR. If IBMISH_QSECOFR_PASSWORD
//! is set, that is its password; otherwise the password is QSECOFR and is
//! marked expired, so the first sign-on must change it.
//!
//! Password case follows the IBM i system value QPWDLVL, set here with
//! IBMISH_QPWDLVL. IBM ships QPWDLVL 0, where passwords are not case
//! sensitive: the system changes them to upper case. At levels 2 and 3
//! they are case sensitive.

use anyhow::{anyhow, bail, Result};
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS ibmish.user_profiles (
    name          text PRIMARY KEY,
    password_hash text NOT NULL,
    pwd_expired   boolean NOT NULL DEFAULT false,
    special_auth  text[] NOT NULL DEFAULT '{}',
    curlib        text NOT NULL DEFAULT '*CRTDFT',
    text          text NOT NULL DEFAULT '',
    status        text NOT NULL DEFAULT '*ENABLED',
    last_signon   timestamptz,
    created       timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE ibmish.user_profiles ADD COLUMN IF NOT EXISTS usrcls text NOT NULL DEFAULT '*USER';
UPDATE ibmish.user_profiles SET usrcls = '*SECOFR' WHERE name = 'QSECOFR' AND usrcls = '*USER';
-- QSECOFR is shipped without a current library (*CRTDFT); profiles made
-- before ibmish modelled that had QGPL.
UPDATE ibmish.user_profiles SET curlib = '*CRTDFT'
    WHERE name = 'QSECOFR' AND curlib = 'QGPL';
"#;

/// User classes (CRTUSRPRF USRCLS).
pub const USER_CLASSES: &[&str] = &["*USER", "*SYSOPR", "*PGMR", "*SECADM", "*SECOFR"];

/// Special authorities a profile can have (CRTUSRPRF SPCAUT).
pub const SPECIAL_AUTHORITIES: &[&str] = &[
    "*ALLOBJ",
    "*AUDIT",
    "*IOSYSCFG",
    "*JOBCTL",
    "*SAVSYS",
    "*SECADM",
    "*SERVICE",
    "*SPLCTL",
];

/// SPCAUT(*USRCLS): the special authorities that come with a user class.
/// IBM i Security reference, "Special authority" default values by user
/// class at security level 40 (the shipped QSECURITY).
pub fn class_special_authorities(usrcls: &str) -> Vec<String> {
    let v: &[&str] = match usrcls {
        "*SECOFR" => SPECIAL_AUTHORITIES,
        "*SECADM" => &["*SECADM"],
        "*SYSOPR" => &["*JOBCTL", "*SAVSYS"],
        _ => &[],
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// How a new profile's password is set (CRTUSRPRF PASSWORD).
pub enum NewPassword {
    /// *USRPRF: the same as the profile name.
    SameAsProfile,
    /// *NONE: the profile cannot sign on.
    None,
    Value(String),
}

/// A new user profile (CRTUSRPRF).
pub struct NewProfile {
    pub name: String,
    pub password: NewPassword,
    pub expired: bool,
    pub enabled: bool,
    pub usrcls: String,
    pub special: Vec<String>,
    pub curlib: String,
    pub text: String,
}

/// A user profile as DSPUSRPRF shows it.
#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub name: String,
    pub usrcls: String,
    pub status: String,
    pub pwd_expired: bool,
    pub no_password: bool,
    pub special_auth: Vec<String>,
    pub curlib: String,
    pub text: String,
    pub last_signon: Option<String>,
    pub created: String,
}

/// Stored instead of a hash for PASSWORD(*NONE): never matches.
const NO_PASSWORD: &str = "*NONE";

pub const MAX_PASSWORD: usize = 28;

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub pwd_expired: bool,
    pub special_auth: Vec<String>,
    pub curlib: String,
    pub text: String,
}

/// The password level (QPWDLVL 0-3) from IBMISH_QPWDLVL; 0 when unset,
/// like a new IBM i.
pub fn password_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("IBMISH_QPWDLVL")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|l| *l <= 3)
            .unwrap_or(0)
    })
}

/// A password as the system checks it at a password level: upper case at
/// levels 0 and 1, as typed at 2 and 3.
pub fn normalize_password(level: u8, pw: &str) -> String {
    if level <= 1 {
        pw.to_uppercase()
    } else {
        pw.to_string()
    }
}

fn normalized(pw: &str) -> String {
    normalize_password(password_level(), pw)
}

pub fn hash_password(pw: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("hashing password: {e}"))
}

fn verify_hash(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// Called from catalog::init. Creates QSECOFR if it does not exist.
pub async fn init(client: &Client) -> Result<()> {
    client.batch_execute(DDL).await?;
    let exists = client
        .query_opt(
            "SELECT 1 FROM ibmish.user_profiles WHERE name = 'QSECOFR'",
            &[],
        )
        .await?
        .is_some();
    if !exists {
        let (pw, expired) = match std::env::var("IBMISH_QSECOFR_PASSWORD") {
            Ok(p) if !p.is_empty() => (p, false),
            _ => ("QSECOFR".to_string(), true),
        };
        create(
            client,
            NewProfile {
                name: "QSECOFR".into(),
                password: NewPassword::Value(pw),
                expired,
                enabled: true,
                usrcls: "*SECOFR".into(),
                special: class_special_authorities("*SECOFR"),
                curlib: "*CRTDFT".into(),
                text: "Security officer".into(),
            },
        )
        .await?;
    }
    Ok(())
}

pub async fn create(client: &Client, p: NewProfile) -> Result<()> {
    let name = validate_name("user profile", &p.name)?;
    let hash = match &p.password {
        NewPassword::None => NO_PASSWORD.to_string(),
        NewPassword::SameAsProfile => hash_password(&normalized(&name))?,
        NewPassword::Value(pw) => {
            check_password_rules(&name, pw)?;
            hash_password(&normalized(pw))?
        }
    };
    if !USER_CLASSES.contains(&p.usrcls.as_str()) {
        bail!(
            "User class {} is not valid; use one of {}.",
            p.usrcls,
            USER_CLASSES.join(", ")
        );
    }
    for a in &p.special {
        if !SPECIAL_AUTHORITIES.contains(&a.as_str()) {
            bail!("Special authority {a} is not valid.");
        }
    }
    let status = if p.enabled { "*ENABLED" } else { "*DISABLED" };
    let n = client
        .execute(
            "INSERT INTO ibmish.user_profiles
                (name, password_hash, pwd_expired, special_auth, curlib, text, status, usrcls)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
            &[
                &name, &hash, &p.expired, &p.special, &p.curlib, &p.text, &status, &p.usrcls,
            ],
        )
        .await?;
    if n == 0 {
        bail!("User profile {name} already exists.");
    }
    Ok(())
}

/// A profile's description, or None if it does not exist.
pub async fn get(client: &Client, name: &str) -> Result<Option<ProfileInfo>> {
    let name = name.trim().to_ascii_uppercase();
    let row = client
        .query_opt(
            "SELECT name, usrcls, status, pwd_expired, password_hash = $2, special_auth, curlib, text,
                    to_char(last_signon AT TIME ZONE 'UTC', 'MM/DD/YY HH24:MI:SS'),
                    to_char(created AT TIME ZONE 'UTC', 'MM/DD/YY HH24:MI:SS')
             FROM ibmish.user_profiles WHERE name = $1",
            &[&name, &NO_PASSWORD],
        )
        .await?;
    Ok(row.map(|r| ProfileInfo {
        name: r.get(0),
        usrcls: r.get(1),
        status: r.get(2),
        pwd_expired: r.get(3),
        no_password: r.get(4),
        special_auth: r.get(5),
        curlib: r.get(6),
        text: r.get(7),
        last_signon: r.get(8),
        created: r.get(9),
    }))
}

/// Reset a profile's password to its own name, expired, and enable it:
/// the recovery for a forgotten QSECOFR password (on IBM i, done from
/// dedicated service tools). Returns false if the profile does not exist.
pub async fn reset_password(client: &Client, name: &str) -> Result<bool> {
    let name = name.trim().to_ascii_uppercase();
    Ok(client
        .execute(
            "UPDATE ibmish.user_profiles
             SET password_hash = $2, pwd_expired = true, status = '*ENABLED'
             WHERE name = $1",
            &[&name, &hash_password(&normalized(&name))?],
        )
        .await?
        > 0)
}

/// Does the profile have a special authority?
pub async fn has_special(client: &Client, name: &str, authority: &str) -> Result<bool> {
    Ok(get(client, name)
        .await?
        .map(|p| p.special_auth.iter().any(|a| a == authority))
        .unwrap_or(false))
}

/// DLTUSRPRF. Returns false if the profile did not exist.
pub async fn delete(client: &Client, name: &str) -> Result<bool> {
    let name = name.trim().to_ascii_uppercase();
    Ok(client
        .execute("DELETE FROM ibmish.user_profiles WHERE name = $1", &[&name])
        .await?
        > 0)
}

pub fn check_password_rules(user: &str, pw: &str) -> Result<()> {
    if pw.is_empty() {
        bail!("The password cannot be blank.");
    }
    if pw.starts_with(' ') || pw.ends_with(' ') {
        bail!("The password cannot start or end with a blank.");
    }
    if pw.chars().count() > MAX_PASSWORD {
        bail!("The password can be at most {MAX_PASSWORD} characters.");
    }
    if pw.eq_ignore_ascii_case(user) && user != "QSECOFR" {
        bail!("The password cannot be the same as the user profile name.");
    }
    Ok(())
}

/// Check a sign-on. Unknown users and wrong passwords give the same error,
/// so the sign-on screen does not reveal which profiles exist.
pub async fn authenticate(client: &Client, user: &str, password: &str) -> Result<Profile> {
    const BAD: &str = "CPF1107: Password or user profile not correct.";
    let user = user.trim().to_ascii_uppercase();
    let row = client
        .query_opt(
            "SELECT password_hash, pwd_expired, special_auth, curlib, text, status
             FROM ibmish.user_profiles WHERE name = $1",
            &[&user],
        )
        .await?;
    let Some(row) = row else {
        // Spend the same time as a real check, so response time does not
        // reveal whether the profile exists.
        static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let dummy = DUMMY.get_or_init(|| hash_password("not-a-password").unwrap_or_default());
        let _ = verify_hash(password, dummy);
        bail!(BAD);
    };
    let hash: String = row.get(0);
    if !verify_hash(&normalized(password), &hash) {
        // Passwords set before ibmish followed QPWDLVL were stored as typed.
        // Accept those as typed, and store them the current way from now on.
        if normalized(password) != password && verify_hash(password, &hash) {
            client
                .execute(
                    "UPDATE ibmish.user_profiles SET password_hash = $2 WHERE name = $1",
                    &[&user, &hash_password(&normalized(password))?],
                )
                .await?;
        } else {
            bail!(BAD);
        }
    }
    let status: String = row.get(5);
    if status != "*ENABLED" {
        bail!("User profile {user} cannot sign on.");
    }
    client
        .execute(
            "UPDATE ibmish.user_profiles SET last_signon = now() WHERE name = $1",
            &[&user],
        )
        .await?;
    Ok(Profile {
        name: user,
        pwd_expired: row.get(1),
        special_auth: row.get(2),
        curlib: row.get(3),
        text: row.get(4),
    })
}

pub async fn change_password(client: &Client, user: &str, current: &str, new: &str) -> Result<()> {
    authenticate(client, user, current)
        .await
        .map_err(|_| anyhow!("Current password not correct."))?;
    check_password_rules(user, new)?;
    if normalized(new) == normalized(current) {
        bail!("The new password must be different from the current password.");
    }
    client
        .execute(
            "UPDATE ibmish.user_profiles SET password_hash = $2, pwd_expired = false WHERE name = $1",
            &[&user, &hash_password(&normalized(new))?],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_and_differ() {
        let a = hash_password("Secret1").unwrap();
        let b = hash_password("Secret1").unwrap();
        assert_ne!(a, b, "salted");
        assert!(a.starts_with("$argon2id$"));
        assert!(verify_hash("Secret1", &a));
        assert!(!verify_hash("secret1", &a));
    }

    // IBM i Security reference, QPWDLVL: at levels 0 and 1 passwords are
    // changed to upper case (so not case sensitive); at 2 and 3 they are
    // case sensitive.
    #[test]
    fn password_level_decides_case() {
        let h = hash_password(&normalize_password(0, "QSECOFR")).unwrap();
        assert!(verify_hash(&normalize_password(0, "qsecofr"), &h));
        assert!(verify_hash(&normalize_password(1, "QsEcOfR"), &h));
        let h = hash_password(&normalize_password(2, "Secret1")).unwrap();
        assert!(!verify_hash(&normalize_password(2, "secret1"), &h));
        assert!(verify_hash(&normalize_password(3, "Secret1"), &h));
    }

    #[test]
    fn user_class_special_authorities() {
        // IBM i Security reference: default special authorities by user
        // class at security level 40.
        assert_eq!(class_special_authorities("*USER"), Vec::<String>::new());
        assert_eq!(class_special_authorities("*PGMR"), Vec::<String>::new());
        assert_eq!(class_special_authorities("*SYSOPR"), ["*JOBCTL", "*SAVSYS"]);
        assert_eq!(class_special_authorities("*SECADM"), ["*SECADM"]);
        assert_eq!(class_special_authorities("*SECOFR").len(), 8);
        assert!(!verify_hash("anything", NO_PASSWORD), "*NONE never matches");
    }

    #[test]
    fn password_rules() {
        assert!(check_password_rules("BOB", "").is_err());
        assert!(check_password_rules("BOB", "bob").is_err());
        assert!(check_password_rules("BOB", &"x".repeat(29)).is_err());
        assert!(check_password_rules("BOB", "Tr0ub4dor").is_ok());
    }
}
