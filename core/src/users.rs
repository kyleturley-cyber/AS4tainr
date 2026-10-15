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
    curlib        text NOT NULL DEFAULT 'QGPL',
    text          text NOT NULL DEFAULT '',
    status        text NOT NULL DEFAULT '*ENABLED',
    last_signon   timestamptz,
    created       timestamptz NOT NULL DEFAULT now()
);
"#;

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
            "QSECOFR",
            &pw,
            expired,
            &["*ALLOBJ", "*SECADM"],
            "Security officer",
        )
        .await?;
    }
    Ok(())
}

pub async fn create(
    client: &Client,
    name: &str,
    password: &str,
    expired: bool,
    special: &[&str],
    text: &str,
) -> Result<()> {
    let name = validate_name("user profile", name)?;
    check_password_rules(&name, password)?;
    let special: Vec<String> = special.iter().map(|s| s.to_string()).collect();
    let n = client
        .execute(
            "INSERT INTO ibmish.user_profiles (name, password_hash, pwd_expired, special_auth, text)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
            &[&name, &hash_password(&normalized(password))?, &expired, &special, &text],
        )
        .await?;
    if n == 0 {
        bail!("User profile {name} already exists.");
    }
    Ok(())
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
    if !verify_hash(&normalized(password), row.get(0)) {
        bail!(BAD);
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
    fn password_rules() {
        assert!(check_password_rules("BOB", "").is_err());
        assert!(check_password_rules("BOB", "bob").is_err());
        assert!(check_password_rules("BOB", &"x".repeat(29)).is_err());
        assert!(check_password_rules("BOB", "Tr0ub4dor").is_ok());
    }
}
