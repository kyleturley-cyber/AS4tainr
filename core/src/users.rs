//! User profiles (*USRPRF) and sign-on checks.
//!
//! Passwords are stored as Argon2id hashes (PHC string format), never in
//! clear text. The first `init` creates QSECOFR. If AS4TAINR_QSECOFR_PASSWORD
//! is set, that is its password; otherwise the password is QSECOFR and is
//! marked expired, so the first sign-on must change it.
//!
//! Password case follows the IBM i system value QPWDLVL, set here with
//! AS4TAINR_QPWDLVL. IBM ships QPWDLVL 0, where passwords are not case
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
CREATE TABLE IF NOT EXISTS as4tainr.user_profiles (
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
-- The host servers' password tokens (hostsrv.rs): SHA-1 of the user ID
-- and the password, as the client computes them for password level 2
-- and up; kept when a password is set or used to sign on.
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS host_tokens bytea[] NOT NULL DEFAULT '{}';
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS usrcls text NOT NULL DEFAULT '*USER';
UPDATE as4tainr.user_profiles SET usrcls = '*SECOFR' WHERE name = 'QSECOFR' AND usrcls = '*USER';
-- QSECOFR is shipped without a current library (*CRTDFT); profiles made
-- before as4tainr modelled that had QGPL.
UPDATE as4tainr.user_profiles SET curlib = '*CRTDFT'
    WHERE name = 'QSECOFR' AND curlib = 'QGPL';
-- Initial program and initial menu (CRTUSRPRF INLPGM, INLMNU): what runs
-- after sign-on. IBM's defaults are *NONE and MAIN.
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS inlpgm text NOT NULL DEFAULT '*NONE';
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS inlmnu text NOT NULL DEFAULT 'MAIN';
-- Sign-on attempts that were not valid since the last valid one
-- (QMAXSIGN), and when the password was last changed (QPWDEXPITV).
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS invalid_signons integer NOT NULL DEFAULT 0;
ALTER TABLE as4tainr.user_profiles ADD COLUMN IF NOT EXISTS pwd_changed timestamptz NOT NULL DEFAULT now();
"#;

/// CRTUSRPRF INLPGM and INLMNU defaults (IBM i CL reference).
pub const DEFAULT_INLPGM: &str = "*NONE";
pub const DEFAULT_INLMNU: &str = "MAIN";

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
    /// Initial program: *NONE or LIB/NAME (*LIBL/NAME).
    pub inlpgm: String,
    /// Initial menu: MAIN, *SIGNOFF or LIB/NAME.
    pub inlmnu: String,
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
    pub inlpgm: String,
    pub inlmnu: String,
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
    pub inlpgm: String,
    pub inlmnu: String,
}

/// The password level (QPWDLVL 0-3) from AS4TAINR_QPWDLVL; 0 when unset,
/// like a new IBM i.
pub fn password_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("AS4TAINR_QPWDLVL")
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
            "SELECT 1 FROM as4tainr.user_profiles WHERE name = 'QSECOFR'",
            &[],
        )
        .await?
        .is_some();
    if !exists {
        let (pw, expired) = match std::env::var("AS4TAINR_QSECOFR_PASSWORD") {
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
                inlpgm: DEFAULT_INLPGM.into(),
                inlmnu: DEFAULT_INLMNU.into(),
            },
        )
        .await?;
    }
    // QUSER, the IBM-supplied profile server jobs run under (Security
    // reference, "IBM-supplied user profiles": user class *USER, no
    // password, so no one signs on with it).
    let quser = client
        .query_opt(
            "SELECT 1 FROM as4tainr.user_profiles WHERE name = 'QUSER'",
            &[],
        )
        .await?
        .is_some();
    if !quser {
        create(
            client,
            NewProfile {
                name: "QUSER".into(),
                password: NewPassword::None,
                expired: false,
                enabled: true,
                usrcls: "*USER".into(),
                special: vec![],
                curlib: "*CRTDFT".into(),
                text: "Work station user".into(),
                inlpgm: DEFAULT_INLPGM.into(),
                inlmnu: DEFAULT_INLMNU.into(),
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
            "INSERT INTO as4tainr.user_profiles
                (name, password_hash, pwd_expired, special_auth, curlib, text, status, usrcls,
                 inlpgm, inlmnu)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING",
            &[
                &name, &hash, &p.expired, &p.special, &p.curlib, &p.text, &status, &p.usrcls,
                &p.inlpgm, &p.inlmnu,
            ],
        )
        .await?;
    if n == 0 {
        bail!("User profile {name} already exists.");
    }
    match &p.password {
        NewPassword::SameAsProfile => store_host_tokens(client, &name, &name).await?,
        NewPassword::Value(pw) => store_host_tokens(client, &name, pw).await?,
        NewPassword::None => {}
    }
    Ok(())
}

/// A host server password token (password level 2 and up): SHA-1 of the
/// user ID (10 characters, blank-padded) and the password (trailing
/// blanks dropped), both as UTF-16 big-endian characters (JTOpen
/// AS400ImplRemote generateShaToken, the client side of the exchange).
pub fn host_token(user: &str, password: &str) -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect() };
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(&utf16(&format!("{:<10}", user.trim().to_ascii_uppercase())));
    ctx.update(&utf16(password.trim_end_matches([' ', '\0', '\u{3000}'])));
    ctx.finish().as_ref().to_vec()
}

/// Keep the host server tokens of a password: as given, and at password
/// levels 0 and 1, where passwords are not case-sensitive, in upper and
/// lower case too (the client sends the password as typed).
pub async fn store_host_tokens(client: &Client, user: &str, password: &str) -> Result<()> {
    let user = user.trim().to_ascii_uppercase();
    let mut forms = vec![password.to_string()];
    if password_level() <= 1 {
        forms.push(password.to_uppercase());
        forms.push(password.to_lowercase());
    }
    forms.dedup();
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    for f in forms {
        let t = host_token(&user, &f);
        if !tokens.contains(&t) {
            tokens.push(t);
        }
    }
    client
        .execute(
            "UPDATE as4tainr.user_profiles SET host_tokens = $2 WHERE name = $1",
            &[&user, &tokens],
        )
        .await?;
    Ok(())
}

/// Sign on with a host server's password substitute (password level 2
/// and up): SHA-1 of a kept token, the server's seed, the client's seed,
/// the user ID (UTF-16, blank-padded to 10) and the sequence number 1
/// (JTOpen AS400ImplRemote generateShaSubstitute). The profile, or the
/// host server return code: 0x00020001 user unknown, 0x00020002 disabled,
/// 0x0003000B password not correct, 0x0003000D expired, 0x00030010 no
/// password (JTOpen returnSecurityException).
pub async fn host_signon(
    client: &Client,
    user: &str,
    client_seed: &[u8],
    server_seed: &[u8],
    substitute: &[u8],
) -> std::result::Result<Profile, u32> {
    let user = user.trim().to_ascii_uppercase();
    let row = client
        .query_opt(
            "SELECT host_tokens, password_hash, pwd_expired, special_auth, curlib, text, status, inlpgm, inlmnu
             FROM as4tainr.user_profiles WHERE name = $1",
            &[&user],
        )
        .await
        .map_err(|_| 0x0002_0001u32)?
        .ok_or(0x0002_0001u32)?;
    let status: String = row.get(6);
    if status != "*ENABLED" {
        return Err(0x0002_0002);
    }
    let hash: String = row.get(1);
    if hash == NO_PASSWORD {
        return Err(0x0003_0010);
    }
    let tokens: Vec<Vec<u8>> = row.get(0);
    let uid: Vec<u8> = format!("{:<10}", user)
        .encode_utf16()
        .flat_map(|c| c.to_be_bytes())
        .collect();
    let ok = tokens.iter().any(|t| {
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
        ctx.update(t);
        ctx.update(server_seed);
        ctx.update(client_seed);
        ctx.update(&uid);
        ctx.update(&[0, 0, 0, 0, 0, 0, 0, 1]);
        ctx.finish().as_ref() == substitute
    });
    if !ok {
        let _ = client
            .execute(
                "UPDATE as4tainr.user_profiles SET invalid_signons = invalid_signons + 1 WHERE name = $1",
                &[&user],
            )
            .await;
        return Err(0x0003_000B);
    }
    if row.get::<_, bool>(2) {
        return Err(0x0003_000D);
    }
    let _ = client
        .execute(
            "UPDATE as4tainr.user_profiles SET last_signon = now(), invalid_signons = 0 WHERE name = $1",
            &[&user],
        )
        .await;
    Ok(Profile {
        name: user,
        pwd_expired: false,
        special_auth: row.get(3),
        curlib: row.get(4),
        text: row.get(5),
        inlpgm: row.get(7),
        inlmnu: row.get(8),
    })
}

/// A profile's description, or None if it does not exist.
pub async fn get(client: &Client, name: &str) -> Result<Option<ProfileInfo>> {
    let name = name.trim().to_ascii_uppercase();
    let row = client
        .query_opt(
            "SELECT name, usrcls, status, pwd_expired, password_hash = $2, special_auth, curlib, text,
                    to_char(last_signon AT TIME ZONE 'UTC', 'MM/DD/YY HH24:MI:SS'),
                    to_char(created AT TIME ZONE 'UTC', 'MM/DD/YY HH24:MI:SS'),
                    inlpgm, inlmnu
             FROM as4tainr.user_profiles WHERE name = $1",
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
        inlpgm: r.get(10),
        inlmnu: r.get(11),
    }))
}

/// Reset a profile's password to its own name, expired, and enable it:
/// the recovery for a forgotten QSECOFR password (on IBM i, done from
/// dedicated service tools). Returns false if the profile does not exist.
pub async fn reset_password(client: &Client, name: &str) -> Result<bool> {
    let name = name.trim().to_ascii_uppercase();
    Ok(client
        .execute(
            "UPDATE as4tainr.user_profiles
             SET password_hash = $2, pwd_expired = true, status = '*ENABLED',
                 invalid_signons = 0, pwd_changed = now()
             WHERE name = $1",
            &[&name, &hash_password(&normalized(&name))?],
        )
        .await?
        > 0
        && store_host_tokens(client, &name, &name).await.is_ok())
}

/// CHGUSRPRF PASSWORD: a new password (a value, *USRPRF or *NONE) set
/// by a security officer, without the current one. Returns false if the
/// profile does not exist.
pub async fn set_password(client: &Client, name: &str, pw: &NewPassword) -> Result<bool> {
    let name = name.trim().to_ascii_uppercase();
    let (hash, token) = match pw {
        NewPassword::None => (NO_PASSWORD.to_string(), None),
        NewPassword::SameAsProfile => (hash_password(&normalized(&name))?, Some(name.clone())),
        NewPassword::Value(v) => {
            check_password_rules(&name, v)?;
            (hash_password(&normalized(v))?, Some(v.clone()))
        }
    };
    let n = client
        .execute(
            "UPDATE as4tainr.user_profiles SET password_hash = $2, pwd_changed = now() WHERE name = $1",
            &[&name, &hash],
        )
        .await?;
    if n == 0 {
        return Ok(false);
    }
    match token {
        Some(t) => store_host_tokens(client, &name, &t).await?,
        None => {
            client
                .execute(
                    "UPDATE as4tainr.user_profiles SET host_tokens = '{}' WHERE name = $1",
                    &[&name],
                )
                .await?;
        }
    }
    Ok(true)
}

/// CHGUSRPRF: the attributes to change (None = leave as is).
#[derive(Debug, Default)]
pub struct ProfileChanges {
    pub expired: Option<bool>,
    pub enabled: Option<bool>,
    pub curlib: Option<String>,
    pub text: Option<String>,
    pub inlpgm: Option<String>,
    pub inlmnu: Option<String>,
}

/// CHGUSRPRF. Returns false if the profile does not exist.
pub async fn change(client: &Client, name: &str, c: &ProfileChanges) -> Result<bool> {
    let name = name.trim().to_ascii_uppercase();
    let status = c
        .enabled
        .map(|e| if e { "*ENABLED" } else { "*DISABLED" }.to_string());
    Ok(client
        .execute(
            "UPDATE as4tainr.user_profiles SET
                pwd_expired = COALESCE($2, pwd_expired),
                status = COALESCE($3, status),
                invalid_signons = CASE WHEN $3 = '*ENABLED' THEN 0 ELSE invalid_signons END,
                curlib = COALESCE($4, curlib),
                text = COALESCE($5, text),
                inlpgm = COALESCE($6, inlpgm),
                inlmnu = COALESCE($7, inlmnu)
             WHERE name = $1",
            &[
                &name, &c.expired, &status, &c.curlib, &c.text, &c.inlpgm, &c.inlmnu,
            ],
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
        .execute(
            "DELETE FROM as4tainr.user_profiles WHERE name = $1",
            &[&name],
        )
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
            "SELECT password_hash, pwd_expired, special_auth, curlib, text, status, inlpgm, inlmnu
             FROM as4tainr.user_profiles WHERE name = $1",
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
        // Passwords set before as4tainr followed QPWDLVL were stored as typed.
        // Accept those as typed, and store them the current way from now on.
        if normalized(password) != password && verify_hash(password, &hash) {
            client
                .execute(
                    "UPDATE as4tainr.user_profiles SET password_hash = $2 WHERE name = $1",
                    &[&user, &hash_password(&normalized(password))?],
                )
                .await?;
        } else {
            // QMAXSIGN (IBM i system values: sign-on attempts not valid
            // before the profile is disabled; 3 as shipped). QSECOFR is
            // not disabled: as4tainr has no console to recover from
            // (`as4tainr rstpwd` resets it).
            let n: i32 = client
                .query_one(
                    "UPDATE as4tainr.user_profiles SET invalid_signons = invalid_signons + 1
                     WHERE name = $1 RETURNING invalid_signons",
                    &[&user],
                )
                .await?
                .get(0);
            let max = crate::clpgm::system_value_text("QMAXSIGN")
                .and_then(|v| v.ok())
                .and_then(|v| v.trim().parse::<i32>().ok());
            if let Some(max) = max {
                if n >= max && user != "QSECOFR" {
                    client
                        .execute(
                            "UPDATE as4tainr.user_profiles SET status = '*DISABLED' WHERE name = $1",
                            &[&user],
                        )
                        .await?;
                    bail!("{BAD}\nUser profile {user} was disabled: {n} sign-on attempts were not valid (QMAXSIGN {max}).");
                }
            }
            bail!(BAD);
        }
    }
    let status: String = row.get(5);
    if status != "*ENABLED" {
        bail!("User profile {user} cannot sign on.");
    }
    client
        .execute(
            "UPDATE as4tainr.user_profiles SET last_signon = now(), invalid_signons = 0 WHERE name = $1",
            &[&user],
        )
        .await?;
    // The host servers' tokens of the password just used (profiles made
    // before they were kept get them at their next sign-on).
    let _ = store_host_tokens(client, &user, password).await;
    // QPWDEXPITV: a password older than that many days must be changed.
    let days = crate::clpgm::system_value_text("QPWDEXPITV")
        .and_then(|v| v.ok())
        .and_then(|v| v.trim().parse::<i32>().ok());
    let too_old = match days {
        Some(d) => client
            .query_one(
                "SELECT pwd_changed < now() - make_interval(days => $2) FROM as4tainr.user_profiles WHERE name = $1",
                &[&user, &d],
            )
            .await?
            .get(0),
        None => false,
    };
    Ok(Profile {
        name: user,
        pwd_expired: row.get::<_, bool>(1) || too_old,
        special_auth: row.get(2),
        curlib: row.get(3),
        text: row.get(4),
        inlpgm: row.get(6),
        inlmnu: row.get(7),
    })
}

pub async fn change_password(client: &Client, user: &str, current: &str, new: &str) -> Result<()> {
    // The current password checked (not a sign-on attempt).
    let user = user.trim().to_ascii_uppercase();
    let user = user.as_str();
    let hash: Option<String> = client
        .query_opt(
            "SELECT password_hash FROM as4tainr.user_profiles WHERE name = $1",
            &[&user],
        )
        .await?
        .map(|r| r.get(0));
    let ok =
        hash.is_some_and(|h| verify_hash(&normalized(current), &h) || verify_hash(current, &h));
    if !ok {
        bail!("Current password not correct.");
    }
    check_password_rules(user, new)?;
    if normalized(new) == normalized(current) {
        bail!("The new password must be different from the current password.");
    }
    client
        .execute(
            "UPDATE as4tainr.user_profiles SET password_hash = $2, pwd_expired = false, pwd_changed = now() WHERE name = $1",
            &[&user, &hash_password(&normalized(new))?],
        )
        .await?;
    store_host_tokens(client, user, new).await?;
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
