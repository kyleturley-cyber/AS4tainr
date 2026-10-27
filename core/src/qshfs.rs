//! The IFS as a QSH command sees it: the stream files and directories of
//! `as4tainr.ifs` copied to a directory on disk (a mirror kept between
//! commands, so only what changed is copied again), bound at their IFS
//! paths (`/home`, `/tmp`, ...) in a mount namespace that only the
//! command sees, and what the command changed, added or removed written
//! back to the IFS afterwards.
//!
//! The container's own files are not changed, except that a top-level
//! IFS directory with no directory of the same name in the container gets
//! an empty one to be mounted on. Top-level names the container's system
//! uses (/usr, /bin, /etc...) are not mounted over. One QSH command at a
//! time uses the mirror (a PostgreSQL advisory lock). Without mount
//! namespaces (a container started without the privilege) the command
//! runs on the container's file system as before (docs/fidelity-gaps.md).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio_postgres::Client;

/// Advisory lock key for the mirror (one QSH command at a time).
const LOCK: i64 = 0x5153_4846_5300; // "QSHFS"

/// Top-level names the container's system uses: never mounted over.
const SYSTEM_DIRS: &[&str] = &[
    "bin", "boot", "dev", "etc", "lib", "lib32", "lib64", "libx32", "media", "mnt", "opt", "proc",
    "root", "run", "sbin", "srv", "sys", "usr", "var",
];

const MANIFEST: &str = ".as4tainr-manifest.json";

/// What the mirror holds of each IFS entry, to tell later what changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Rec {
    dir: bool,
    /// The IFS entry's last change (microseconds).
    changed_us: i64,
    /// The local file's modification time (ns) and size when written.
    mtime_ns: i128,
    size: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    entries: BTreeMap<String, Rec>,
}

/// How to start a command in a mount namespace of its own: the unshare
/// program and its options, or None when this system cannot.
fn namespace() -> &'static Option<(String, Vec<String>)> {
    static NS: OnceLock<Option<(String, Vec<String>)>> = OnceLock::new();
    NS.get_or_init(|| {
        let unshare = ["/usr/bin/unshare", "/bin/unshare"]
            .into_iter()
            .find(|p| Path::new(p).exists())?;
        let root = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(2).map(|u| u == "0"))
            })
            .unwrap_or(false);
        let mut tries: Vec<Vec<&str>> = Vec::new();
        if root {
            tries.push(vec!["--mount", "--propagation", "private"]);
        }
        tries.push(vec![
            "--user",
            "--map-root-user",
            "--mount",
            "--propagation",
            "private",
        ]);
        for opts in tries {
            let ok = std::process::Command::new(unshare)
                .args(&opts)
                .args(["--", "/bin/true"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                return Some((
                    unshare.to_string(),
                    opts.into_iter().map(String::from).collect(),
                ));
            }
        }
        None
    })
}

/// Is the IFS given to QSH commands (mount namespaces work, and it is
/// not turned off with AS4TAINR_QSH_IFS=0)?
pub fn available() -> bool {
    if std::env::var("AS4TAINR_QSH_IFS").is_ok_and(|v| v == "0") {
        return false;
    }
    if namespace().is_none() {
        static SAID: OnceLock<()> = OnceLock::new();
        SAID.get_or_init(|| {
            eprintln!(
                "as4tainr: QSH commands see the container's files, not the IFS: this system does not allow mount namespaces"
            )
        });
        return false;
    }
    true
}

/// The mirror's directory for a database.
fn mirror_root(database_url: &str) -> PathBuf {
    if let Ok(d) = std::env::var("AS4TAINR_QSH_MIRROR") {
        return PathBuf::from(d);
    }
    // The database's name and host, not its password.
    let tail = database_url.rsplit('@').next().unwrap_or(database_url);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in tail.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    std::env::temp_dir().join(format!("as4tainr-ifs-{h:016x}"))
}

fn local_meta(p: &Path) -> Option<(i128, u64)> {
    let m = std::fs::symlink_metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    Some((t, m.len()))
}

/// Every entry under `root`, as IFS paths ("/a/b"), with whether it is a
/// directory (symbolic links are left out).
fn walk(root: &Path) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if rel.as_os_str().is_empty() && name == MANIFEST {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let r = rel.join(&name);
            let ifs = format!("/{}", r.to_string_lossy());
            if ft.is_dir() {
                out.push((ifs, true));
                stack.push(r);
            } else if ft.is_file() {
                out.push((ifs, false));
            }
        }
    }
    out
}

fn local(root: &Path, ifs: &str) -> PathBuf {
    root.join(ifs.trim_start_matches('/'))
}

/// A mirror of the IFS, made current, for one command.
pub struct Mirror {
    root: PathBuf,
    manifest: Manifest,
}

impl Mirror {
    /// Take the mirror (waits for another QSH command using it) and copy
    /// what changed in the IFS since the last command.
    pub async fn open(client: &Client, database_url: &str) -> Result<Mirror> {
        client
            .execute("SELECT pg_advisory_lock($1)", &[&LOCK])
            .await?;
        match Self::sync_in(client, database_url).await {
            Ok(m) => Ok(m),
            Err(e) => {
                let _ = client
                    .execute("SELECT pg_advisory_unlock($1)", &[&LOCK])
                    .await;
                Err(e)
            }
        }
    }

    async fn sync_in(client: &Client, database_url: &str) -> Result<Mirror> {
        let root = mirror_root(database_url);
        std::fs::create_dir_all(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let mut manifest: Manifest = std::fs::read(root.join(MANIFEST))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let rows = client
            .query(
                "SELECT path, kind = 'D', (extract(epoch FROM changed) * 1000000)::bigint, mode
                   FROM as4tainr.ifs WHERE path <> '/' ORDER BY path",
                &[],
            )
            .await?;
        let ifs: BTreeMap<String, (bool, i64, i32)> = rows
            .iter()
            .map(|r| (r.get::<_, String>(0), (r.get(1), r.get(2), r.get(3))))
            .collect();
        // What is on disk and no longer in the IFS (or not of its kind)
        // goes, deepest first.
        let mut here = walk(&root);
        here.sort_by(|a, b| b.0.cmp(&a.0));
        for (p, dir) in &here {
            let keep = ifs.get(p).is_some_and(|(d, _, _)| d == dir);
            if !keep {
                let l = local(&root, p);
                let _ = if *dir {
                    std::fs::remove_dir_all(&l)
                } else {
                    std::fs::remove_file(&l)
                };
                manifest.entries.remove(p);
            }
        }
        // Directories, then the files that changed.
        let mut fetch: Vec<String> = Vec::new();
        for (p, (dir, changed, mode)) in &ifs {
            let l = local(&root, p);
            if *dir {
                std::fs::create_dir_all(&l)?;
                let _ = std::fs::set_permissions(
                    &l,
                    std::fs::Permissions::from_mode((*mode as u32 & 0o7777) | 0o700),
                );
                manifest.entries.insert(
                    p.clone(),
                    Rec {
                        dir: true,
                        changed_us: *changed,
                        mtime_ns: 0,
                        size: 0,
                    },
                );
                continue;
            }
            let current = manifest.entries.get(p).is_some_and(|r| {
                !r.dir && r.changed_us == *changed && local_meta(&l) == Some((r.mtime_ns, r.size))
            });
            if !current {
                fetch.push(p.clone());
            }
        }
        for chunk in fetch.chunks(200) {
            let rows = client
                .query(
                    "SELECT path, data, (extract(epoch FROM changed) * 1000000)::bigint, mode
                       FROM as4tainr.ifs WHERE path = ANY($1) AND kind = 'F'",
                    &[&chunk],
                )
                .await?;
            for r in rows {
                let p: String = r.get(0);
                let data: Vec<u8> = r.get(1);
                let l = local(&root, &p);
                std::fs::write(&l, &data)?;
                let mode: i32 = r.get(3);
                let _ = std::fs::set_permissions(
                    &l,
                    std::fs::Permissions::from_mode((mode as u32 & 0o7777) | 0o600),
                );
                let (mtime_ns, size) = local_meta(&l).unwrap_or((0, 0));
                manifest.entries.insert(
                    p,
                    Rec {
                        dir: false,
                        changed_us: r.get(2),
                        mtime_ns,
                        size,
                    },
                );
            }
        }
        std::fs::write(root.join(MANIFEST), serde_json::to_vec(&manifest)?)?;
        Ok(Mirror { root, manifest })
    }

    /// The command to run in a namespace where the mirror's top-level
    /// directories are at their IFS paths, starting in `cwd`: the
    /// program and its arguments.
    pub fn command(&self, cwd: &str, line: &str) -> Option<(String, Vec<String>)> {
        let (unshare, opts) = namespace().as_ref()?;
        let mut tops: Vec<String> = std::fs::read_dir(&self.root)
            .ok()?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !SYSTEM_DIRS.contains(&n.as_str()))
            .collect();
        // A directory the mirror itself is in is mounted last, so the
        // mirror can still be reached for the others.
        let inside = |n: &String| self.root.starts_with(format!("/{n}"));
        tops.sort_by_key(|n| (inside(n), n.clone()));
        let mount = ["/usr/bin/mount", "/bin/mount"]
            .into_iter()
            .find(|p| Path::new(p).exists())?;
        let mut script = String::from("exec 2>&1\n");
        // A top-level directory the container does not have is made
        // first; where it cannot be (as4tainr not running as root: the
        // namespace's root cannot write in the container's /), the
        // command does not see that IFS directory (docs/qshell.md).
        for n in &tops {
            let src = self.root.join(n);
            script.push_str(&format!(
                "mkdir -p {t} 2>/dev/null; if [ -d {t} ]; then {mount} --bind {s} {t} || exit 126; fi\n",
                t = sh_quote(&format!("/{n}")),
                s = sh_quote(&src.to_string_lossy()),
            ));
        }
        script.push_str("cd \"$1\" 2>/dev/null || cd /\nexec /bin/sh -c \"$2\"\n");
        let mut args = opts.clone();
        args.extend([
            "--".into(),
            "/bin/sh".into(),
            "-c".into(),
            script,
            "sh".into(),
            cwd.into(),
            line.into(),
        ]);
        Some((unshare.clone(), args))
    }

    /// Write back what the command changed, added or removed, and let
    /// the next QSH command have the mirror.
    pub async fn close(mut self, client: &Client, user: &str) -> Result<()> {
        let r = self.sync_out(client, user).await;
        let _ = std::fs::write(
            self.root.join(MANIFEST),
            serde_json::to_vec(&self.manifest).unwrap_or_default(),
        );
        client
            .execute("SELECT pg_advisory_unlock($1)", &[&LOCK])
            .await?;
        r
    }

    async fn sync_out(&mut self, client: &Client, user: &str) -> Result<()> {
        let mut here = walk(&self.root);
        here.sort();
        let seen: BTreeSet<String> = here.iter().map(|(p, _)| p.clone()).collect();
        for (p, dir) in &here {
            let known = self.manifest.entries.get(p).cloned();
            if *dir {
                if known.is_none() {
                    match crate::ifs::mkdir(client, p, 0o755, user).await {
                        Ok(()) => {}
                        Err(e) if crate::ifs::errno_of(&e) == crate::ifs::errno::EEXIST => {}
                        Err(e) => return Err(e),
                    }
                }
                continue;
            }
            let l = local(&self.root, p);
            let meta = local_meta(&l);
            let changed = match &known {
                None => true,
                Some(r) => r.dir || meta != Some((r.mtime_ns, r.size)),
            };
            if changed {
                let data = std::fs::read(&l)?;
                let mode = std::fs::metadata(&l)
                    .map(|m| (m.permissions().mode() & 0o7777) as i32)
                    .unwrap_or(0o644);
                // A new file is UTF-8 text as far as the IFS knows; a
                // file that was there keeps its CCSID.
                crate::ifs::write_file(client, p, &data, 1208, mode, user).await?;
                // Fetched again by the next command.
                self.manifest.entries.remove(p);
            }
        }
        // What the command removed, deepest first.
        let gone: Vec<(String, bool)> = self
            .manifest
            .entries
            .iter()
            .filter(|(p, _)| !seen.contains(*p))
            .map(|(p, r)| (p.clone(), r.dir))
            .rev()
            .collect();
        for (p, dir) in gone {
            let r = if dir {
                crate::ifs::rmdir(client, &p).await
            } else {
                crate::ifs::unlink(client, &p).await
            };
            if r.is_ok() {
                self.manifest.entries.remove(&p);
            }
        }
        Ok(())
    }
}

/// `s` in single quotes for /bin/sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_for_the_shell() {
        assert_eq!(sh_quote("/a b"), "'/a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn the_mirror_is_per_database_without_the_password() {
        let a = mirror_root("postgres://u:secret@h/db1");
        let b = mirror_root("postgres://u:other@h/db1");
        let c = mirror_root("postgres://u:secret@h/db2");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(!a.to_string_lossy().contains("secret"));
    }
}
