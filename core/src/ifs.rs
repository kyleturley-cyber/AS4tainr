//! The integrated file system (IFS): stream files and directories in the
//! "root" (/) file system.
//!
//! Written from IBM's integrated file system and UNIX-type API
//! documentation. Each object is a row of `as4tainr.ifs`: its path (case
//! preserved; names are found ignoring case, as in the root file system),
//! whether it is a stream file or a directory, its bytes, the CCSID it is
//! tagged with, its permission bits and owner. Text conversion between
//! CCSIDs (O_TEXTDATA) is done here for the CCSIDs listed in `codec`.

use anyhow::Result;
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.ifs (
    path_key text PRIMARY KEY,
    path     text NOT NULL,
    kind     char(1) NOT NULL,
    data     bytea NOT NULL DEFAULT '',
    ccsid    int NOT NULL DEFAULT 37,
    mode     int NOT NULL DEFAULT 0,
    owner    text NOT NULL DEFAULT 'QSECOFR',
    created  timestamptz NOT NULL DEFAULT now(),
    changed  timestamptz NOT NULL DEFAULT now()
);
INSERT INTO as4tainr.ifs (path_key, path, kind, mode) VALUES ('/', '/', 'D', 493)
  ON CONFLICT DO NOTHING;
-- The directories an IBM i system has from the start: /home (users'
-- home directories) and /tmp (everyone's; restricted rename and unlink,
-- mode 1777).
INSERT INTO as4tainr.ifs (path_key, path, kind, mode) VALUES ('/home', '/home', 'D', 493), ('/tmp', '/tmp', 'D', 1023)
  ON CONFLICT DO NOTHING;
"#;

/// Create the IFS table; include files kept by earlier versions (in
/// `as4tainr.stream_files`) move into it.
pub async fn init(client: &Client) -> Result<()> {
    client.batch_execute(DDL).await?;
    let old = client
        .query(
            "SELECT s.path, s.data FROM as4tainr.stream_files s
              WHERE NOT EXISTS (SELECT 1 FROM as4tainr.ifs i WHERE i.path_key = lower(s.path))",
            &[],
        )
        .await?;
    for r in old {
        let path: String = r.get(0);
        let data: String = r.get(1);
        put_text(client, &path, &data).await?;
    }
    Ok(())
}

/// Store a text file (UTF-8, CCSID 1208), with the directories above it.
pub async fn put_text(client: &Client, path: &str, text: &str) -> Result<()> {
    let path = normalize(path)?;
    mkdir_all(client, parent(&path), "QSECOFR").await?;
    write_file(client, &path, text.as_bytes(), 1208, 0o644, "QSECOFR").await
}

/// errno values (IBM i "Errno Values for UNIX-Type Functions").
pub mod errno {
    pub const EINVAL: i32 = 3021;
    pub const ENOENT: i32 = 3025;
    pub const EPERM: i32 = 3027;
    pub const EIO: i32 = 3006;
    pub const EBUSY: i32 = 3029;
    pub const EACCES: i32 = 3401;
    pub const ENOTDIR: i32 = 3403;
    pub const EBADF: i32 = 3450;
    pub const EMFILE: i32 = 3452;
    pub const EEXIST: i32 = 3457;
    pub const ESPIPE: i32 = 3469;
    pub const ENOSYS: i32 = 3470;
    pub const EISDIR: i32 = 3471;
    pub const ENAMETOOLONG: i32 = 3486;
    pub const ENOTEMPTY: i32 = 3488;
    pub const ECONVERT: i32 = 3490;
    pub const EFAULT: i32 = 3408;
}

/// An IFS error: an errno value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

impl std::fmt::Display for Errno {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", errno_text(self.0))
    }
}

impl std::error::Error for Errno {}

/// The errno value of an error (EIO for anything that is not one).
pub fn errno_of(e: &anyhow::Error) -> i32 {
    e.downcast_ref::<Errno>().map(|x| x.0).unwrap_or(errno::EIO)
}

/// as4tainr's text for an errno value (strerror).
pub fn errno_text(n: i32) -> String {
    use errno::*;
    match n {
        0 => "No error.".into(),
        EINVAL => "A parameter value is not valid.".into(),
        ENOENT => "The file or directory does not exist.".into(),
        EPERM => "The operation is not permitted.".into(),
        EIO => "An input or output error occurred.".into(),
        EBUSY => "The object is in use.".into(),
        EACCES => "The user is not authorized to the object.".into(),
        ENOTDIR => "A part of the path is not a directory.".into(),
        EBADF => "The file descriptor is not open, or not open for this use.".into(),
        EMFILE => "Too many files are open in the job.".into(),
        EEXIST => "The file or directory already exists.".into(),
        ESPIPE => "The offset cannot be set for this file.".into(),
        ENOSYS => "The function is not available.".into(),
        EISDIR => "The object is a directory.".into(),
        ENAMETOOLONG => "The path name is too long.".into(),
        ENOTEMPTY => "The directory is not empty.".into(),
        ECONVERT => "The data cannot be converted between the CCSIDs.".into(),
        EFAULT => "An address is not valid.".into(),
        other => format!("Error number {other}."),
    }
}

fn err<T>(n: i32) -> Result<T> {
    Err(Errno(n).into())
}

/// An absolute path without `.`, `..`, empty parts or a trailing `/`.
/// Relative paths are taken from the root directory (the job's current
/// directory is always /).
pub fn normalize(path: &str) -> Result<String> {
    let path = path.trim_end_matches('\0').trim();
    if path.is_empty() {
        return err(errno::ENOENT);
    }
    if path.len() > 16 * 1024 {
        return err(errno::ENAMETOOLONG);
    }
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// An object's description.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub dir: bool,
    pub ccsid: i32,
    pub mode: i32,
    pub owner: String,
    pub size: i64,
}

pub async fn stat(client: &Client, path: &str) -> Result<Option<Entry>> {
    let path = normalize(path)?;
    Ok(client
        .query_opt(
            "SELECT path, kind, ccsid, mode, owner, octet_length(data)::bigint
               FROM as4tainr.ifs WHERE path_key = $1",
            &[&key(&path)],
        )
        .await?
        .map(|r| Entry {
            path: r.get(0),
            dir: r.get::<_, String>(1) == "D",
            ccsid: r.get(2),
            mode: r.get(3),
            owner: r.get(4),
            size: r.get(5),
        }))
}

/// A stream file's bytes and CCSID.
pub async fn read_file(client: &Client, path: &str) -> Result<(Vec<u8>, i32)> {
    let path = normalize(path)?;
    let Some(r) = client
        .query_opt(
            "SELECT kind, data, ccsid FROM as4tainr.ifs WHERE path_key = $1",
            &[&key(&path)],
        )
        .await?
    else {
        return err(errno::ENOENT);
    };
    if r.get::<_, String>(0) == "D" {
        return err(errno::EISDIR);
    }
    Ok((r.get(1), r.get(2)))
}

async fn check_parent(client: &Client, path: &str) -> Result<()> {
    match stat(client, parent(path)).await? {
        None => err(errno::ENOENT),
        Some(e) if !e.dir => err(errno::ENOTDIR),
        Some(_) => Ok(()),
    }
}

/// Create a stream file or replace its bytes (the CCSID and mode are
/// those of a new file; an existing file keeps its own).
pub async fn write_file(
    client: &Client,
    path: &str,
    data: &[u8],
    ccsid: i32,
    mode: i32,
    owner: &str,
) -> Result<()> {
    let path = normalize(path)?;
    if path == "/" {
        return err(errno::EISDIR);
    }
    if let Some(e) = stat(client, &path).await? {
        if e.dir {
            return err(errno::EISDIR);
        }
        client
            .execute(
                "UPDATE as4tainr.ifs SET data = $2, changed = now() WHERE path_key = $1",
                &[&key(&path), &data],
            )
            .await?;
        return Ok(());
    }
    check_parent(client, &path).await?;
    client
        .execute(
            "INSERT INTO as4tainr.ifs (path_key, path, kind, data, ccsid, mode, owner)
             VALUES ($1, $2, 'F', $3, $4, $5, $6)",
            &[&key(&path), &path, &data, &ccsid, &mode, &owner],
        )
        .await?;
    Ok(())
}

/// mkdir(): a new directory.
pub async fn mkdir(client: &Client, path: &str, mode: i32, owner: &str) -> Result<()> {
    let path = normalize(path)?;
    if stat(client, &path).await?.is_some() {
        return err(errno::EEXIST);
    }
    check_parent(client, &path).await?;
    client
        .execute(
            "INSERT INTO as4tainr.ifs (path_key, path, kind, mode, owner) VALUES ($1, $2, 'D', $3, $4)",
            &[&key(&path), &path, &mode, &owner],
        )
        .await?;
    Ok(())
}

/// A directory and the directories above it (for imports).
pub async fn mkdir_all(client: &Client, path: &str, owner: &str) -> Result<()> {
    let path = normalize(path)?;
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        match stat(client, &at).await? {
            Some(e) if e.dir => {}
            Some(_) => return err(errno::ENOTDIR),
            None => {
                client
                    .execute(
                        "INSERT INTO as4tainr.ifs (path_key, path, kind, mode, owner)
                         VALUES ($1, $2, 'D', 493, $3) ON CONFLICT DO NOTHING",
                        &[&key(&at), &at, &owner],
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// rmdir(): an empty directory removed.
pub async fn rmdir(client: &Client, path: &str) -> Result<()> {
    let path = normalize(path)?;
    if path == "/" {
        return err(errno::EBUSY);
    }
    match stat(client, &path).await? {
        None => return err(errno::ENOENT),
        Some(e) if !e.dir => return err(errno::ENOTDIR),
        Some(_) => {}
    }
    let children: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.ifs WHERE path_key LIKE $1",
            &[&format!("{}/%", like_escape(&key(&path)))],
        )
        .await?
        .get(0);
    if children > 0 {
        return err(errno::ENOTEMPTY);
    }
    client
        .execute(
            "DELETE FROM as4tainr.ifs WHERE path_key = $1",
            &[&key(&path)],
        )
        .await?;
    Ok(())
}

/// unlink(): a stream file removed (a directory cannot be: EPERM).
pub async fn unlink(client: &Client, path: &str) -> Result<()> {
    let path = normalize(path)?;
    match stat(client, &path).await? {
        None => err(errno::ENOENT),
        Some(e) if e.dir => err(errno::EPERM),
        Some(_) => {
            client
                .execute(
                    "DELETE FROM as4tainr.ifs WHERE path_key = $1",
                    &[&key(&path)],
                )
                .await?;
            Ok(())
        }
    }
}

/// Rename an object, keeping an existing target (Qp0lRenameKeep: EEXIST).
/// A directory moves with everything in it.
pub async fn rename(client: &Client, old: &str, new: &str) -> Result<()> {
    let old = normalize(old)?;
    let new = normalize(new)?;
    if stat(client, &old).await?.is_none() {
        return err(errno::ENOENT);
    }
    if key(&old) != key(&new) && stat(client, &new).await?.is_some() {
        return err(errno::EEXIST);
    }
    if key(&new).starts_with(&format!("{}/", key(&old))) {
        return err(errno::EINVAL);
    }
    check_parent(client, &new).await?;
    client
        .execute(
            "UPDATE as4tainr.ifs
                SET path = $2 || substr(path, length($1) + 1),
                    path_key = lower($2 || substr(path, length($1) + 1)),
                    changed = now()
              WHERE path_key = lower($1) OR path_key LIKE $3",
            &[&old, &new, &format!("{}/%", like_escape(&key(&old)))],
        )
        .await?;
    Ok(())
}

/// The objects in a directory: (name, is a directory), by name.
pub async fn list(client: &Client, path: &str) -> Result<Vec<(String, bool)>> {
    let path = normalize(path)?;
    match stat(client, &path).await? {
        None => return err(errno::ENOENT),
        Some(e) if !e.dir => return err(errno::ENOTDIR),
        Some(_) => {}
    }
    let prefix = if path == "/" {
        "/".to_string()
    } else {
        format!("{}/", key(&path))
    };
    let rows = client
        .query(
            "SELECT path, kind FROM as4tainr.ifs
              WHERE path_key LIKE $1 AND path_key <> '/' AND position('/' IN substr(path_key, $2)) = 0
              ORDER BY path_key",
            &[&format!("{}%", like_escape(&prefix)), &(prefix.len() as i32 + 1)],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let p: String = r.get(0);
            let name = p.rsplit('/').next().unwrap_or("").to_string();
            (name, r.get::<_, String>(1) == "D")
        })
        .collect())
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Text conversion between CCSIDs: 37 (EBCDIC), 1208 (UTF-8), 819, 367
/// and 1252 (ASCII and Latin-1; 1252's extra characters at 0x80-0x9F are
/// taken as Latin-1's), 1200 and 13488 (UTF-16).
pub mod codec {
    use crate::tn5250::ebcdic;

    pub fn supported(ccsid: i32) -> bool {
        matches!(ccsid, 37 | 1208 | 819 | 367 | 1252 | 1200 | 13488)
    }

    /// The next character at the start of `b` and its length in bytes
    /// (None when `b` ends inside a character).
    pub fn decode_char(ccsid: i32, b: &[u8]) -> Option<(char, usize)> {
        match ccsid {
            37 => b.first().map(|&x| {
                (
                    char::from_u32(ebcdic::TO_UNICODE[x as usize] as u32).unwrap_or('?'),
                    1,
                )
            }),
            1208 => {
                let n = match b.first()? {
                    x if *x < 0x80 => 1,
                    x if *x >= 0xF0 => 4,
                    x if *x >= 0xE0 => 3,
                    x if *x >= 0xC0 => 2,
                    _ => return Some(('\u{FFFD}', 1)),
                };
                if b.len() < n {
                    return None;
                }
                match std::str::from_utf8(&b[..n]) {
                    Ok(s) => s.chars().next().map(|c| (c, n)),
                    Err(_) => Some(('\u{FFFD}', 1)),
                }
            }
            1200 | 13488 => {
                if b.len() < 2 {
                    return None;
                }
                let u = u16::from_be_bytes([b[0], b[1]]);
                if (0xD800..0xDC00).contains(&u) {
                    if b.len() < 4 {
                        return None;
                    }
                    let l = u16::from_be_bytes([b[2], b[3]]);
                    let c = char::decode_utf16([u, l]).next()?.unwrap_or('\u{FFFD}');
                    return Some((c, 4));
                }
                Some((char::from_u32(u as u32).unwrap_or('\u{FFFD}'), 2))
            }
            _ => b.first().map(|&x| (x as char, 1)),
        }
    }

    pub fn encode_char(ccsid: i32, c: char, out: &mut Vec<u8>) {
        match ccsid {
            37 => out.extend(ebcdic::encode(&c.to_string())),
            1208 => {
                let mut buf = [0u8; 4];
                out.extend(c.encode_utf8(&mut buf).as_bytes());
            }
            1200 | 13488 => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.extend(u.to_be_bytes());
                }
            }
            367 if (c as u32) > 0x7F => out.push(b'?'),
            _ => out.push(if (c as u32) < 256 { c as u8 } else { b'?' }),
        }
    }

    pub fn convert(from: i32, to: i32, b: &[u8]) -> Vec<u8> {
        if from == to {
            return b.to_vec();
        }
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match decode_char(from, &b[i..]) {
                Some((c, n)) => {
                    encode_char(to, c, &mut out);
                    i += n;
                }
                None => break,
            }
        }
        out
    }

    /// Text in a CCSID as a Rust string.
    pub fn text(ccsid: i32, b: &[u8]) -> String {
        String::from_utf8_lossy(&convert(ccsid, 1208, b)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(
            normalize("/home//bob/./x/../a.txt").unwrap(),
            "/home/bob/a.txt"
        );
        assert_eq!(normalize("tmp/f").unwrap(), "/tmp/f");
        assert_eq!(normalize("/").unwrap(), "/");
        assert_eq!(normalize("/..").unwrap(), "/");
        assert_eq!(parent("/a/b"), "/a");
        assert_eq!(parent("/a"), "/");
    }

    #[test]
    fn conversions() {
        // 'A' is 0xC1 in CCSID 37, 0x41 in ASCII and UTF-8; 'é' is two
        // bytes in UTF-8 (0xC3 0xA9) and 0x51 in CCSID 37.
        assert_eq!(codec::convert(37, 1208, &[0xC1, 0x51]), "Aé".as_bytes());
        assert_eq!(codec::convert(1208, 37, "Aé".as_bytes()), [0xC1, 0x51]);
        assert_eq!(codec::convert(1208, 1200, b"A"), [0x00, 0x41]);
        assert_eq!(codec::decode_char(1208, &[0xC3]), None);
    }
}
