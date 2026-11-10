//! The QSYS.LIB file system as a shell command sees it (an SSH or QSH
//! command run on the IFS mirror, qshfs.rs): `/QSYS.LIB/LIB.LIB` for each
//! library, and in the libraries the command names (its line or its
//! starting directory) their objects as `NAME.TYPE`; a file is a
//! directory of its members (`MBR.MBR`). A source member reads as text,
//! a line for each record without its sequence number and date and
//! without trailing blanks (IBM i documentation, "QSYS.LIB file system":
//! source physical file members as text). What the command changed in a
//! source member, or a member it made in a source file, is written back
//! after it (as CPYFRMSTMF would: each line a record).
//!
//! Code for IBM i reaches members this way: `cd` into a source file to
//! search its members (SearchTools.ts), `ls /QSYS.LIB/LIB.LIB/OBJ.*` to
//! find an object (IBMiContent.ts objectResolve).

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tokio_postgres::Client;

/// A source member written to the tree, to tell afterwards whether the
/// command changed it.
#[derive(Debug, Clone)]
struct Written {
    library: String,
    file: String,
    member: String,
    mtime_ns: i128,
    size: u64,
}

/// The tree made for one command.
#[derive(Debug, Default)]
pub struct Tree {
    /// Its directory: `root/QSYS.LIB` is mounted at /QSYS.LIB.
    pub root: PathBuf,
    members: BTreeMap<PathBuf, Written>,
    /// Source files' directories: (library, file).
    source_dirs: BTreeMap<PathBuf, (String, String)>,
}

/// The libraries a command names: `/QSYS.LIB/NAME.LIB` in its line or its
/// starting directory (upper case).
pub fn named_libraries(cwd: &str, line: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for text in [cwd, line] {
        let up = text.to_ascii_uppercase();
        let mut from = 0;
        while let Some(i) = up[from..].find("/QSYS.LIB/") {
            let start = from + i + "/QSYS.LIB/".len();
            let rest = &up[start..];
            let end = rest
                .find(|c: char| c == '/' || c.is_whitespace() || "'\";&|),<>".contains(c))
                .unwrap_or(rest.len());
            if let Some(lib) = rest[..end].strip_suffix(".LIB") {
                if !lib.is_empty() && !lib.contains('*') {
                    out.insert(lib.to_string());
                }
            }
            from = start;
        }
    }
    out
}

fn meta(p: &Path) -> Option<(i128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    Some((t, m.len()))
}

/// A source member's text: its lines, each ending in a newline.
fn member_text(source: &str) -> String {
    let mut s = String::new();
    for l in source.lines() {
        s.push_str(l.trim_end());
        s.push('\n');
    }
    s
}

/// Whether a command reaches /QSYS.LIB (names it in its line or starts
/// there): only then is the tree made and mounted.
pub fn wanted(cwd: &str, line: &str) -> bool {
    cwd.to_ascii_uppercase().starts_with("/QSYS.LIB")
        || line.to_ascii_uppercase().contains("/QSYS.LIB")
}

/// Empty a directory, keeping it.
fn clear(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if e.file_type().is_ok_and(|t| t.is_dir()) {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

/// Make the tree for a command in `root`, kept between commands: a
/// directory for each library (those made or deleted since the last
/// command added or removed), the objects of the libraries this command
/// names (what an earlier command was given of others removed).
pub async fn build(client: &Client, root: &Path, cwd: &str, line: &str) -> Result<Tree> {
    let top = root.join("QSYS.LIB");
    std::fs::create_dir_all(&top)?;
    let mut tree = Tree {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let libs: Vec<String> = client
        .query(
            "SELECT name FROM as4tainr.objects WHERE library = 'QSYS' AND objtype = '*LIB'
              AND name NOT LIKE '#Q%' ORDER BY name",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let want: BTreeSet<String> = libs.iter().map(|l| format!("{l}.LIB")).collect();
    let mut have: BTreeSet<String> = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(&top) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if want.contains(&n) {
                have.insert(n);
            } else {
                let _ = std::fs::remove_dir_all(e.path());
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    for d in want.difference(&have) {
        std::fs::create_dir_all(top.join(d))?;
    }
    // What earlier commands were given of libraries: gone.
    let filled = root.join(".filled");
    if let Ok(t) = std::fs::read_to_string(&filled) {
        for l in t.lines() {
            clear(&top.join(format!("{l}.LIB")));
        }
    }
    let named = named_libraries(cwd, line);
    std::fs::write(
        &filled,
        named.iter().cloned().collect::<Vec<_>>().join("\n"),
    )?;
    for lib in named {
        let lib = crate::qtemp::real(&lib);
        if !libs.contains(&lib) {
            continue;
        }
        let dir = top.join(format!("{}.LIB", crate::qtemp::logical(&lib)));
        clear(&dir);
        std::fs::create_dir_all(&dir)?;
        let objects = client
            .query(
                "SELECT o.name, o.objtype, (sf.name IS NOT NULL)
                   FROM as4tainr.objects o
                   LEFT JOIN as4tainr.source_files sf ON sf.library = o.library AND sf.name = o.name
                  WHERE o.library = $1 ORDER BY o.name, o.objtype",
                &[&lib],
            )
            .await?;
        for o in objects {
            let name: String = o.get(0);
            let ty: String = o.get(1);
            let source: bool = o.get(2);
            let ty = ty.trim_start_matches('*');
            let p = dir.join(format!("{name}.{ty}"));
            if ty != "FILE" {
                std::fs::write(&p, b"")?;
                continue;
            }
            std::fs::create_dir_all(&p)?;
            if source {
                tree.source_dirs
                    .insert(p.clone(), (lib.clone(), name.clone()));
                let members = client
                    .query(
                        "SELECT member, source FROM as4tainr.source_members
                          WHERE library = $1 AND file = $2",
                        &[&lib, &name],
                    )
                    .await?;
                for m in members {
                    let member: String = m.get(0);
                    let text: String = m.get(1);
                    let f = p.join(format!("{member}.MBR"));
                    std::fs::write(&f, member_text(&text))?;
                    let (mtime_ns, size) = meta(&f).unwrap_or((0, 0));
                    tree.members.insert(
                        f,
                        Written {
                            library: lib.clone(),
                            file: name.clone(),
                            member,
                            mtime_ns,
                            size,
                        },
                    );
                }
            } else {
                // A data file: its member, named as the file.
                std::fs::write(p.join(format!("{name}.MBR")), b"")?;
            }
        }
    }
    Ok(tree)
}

/// Write back what the command changed in source members, and the
/// members it made in source files.
pub async fn write_back(client: &Client, tree: &Tree) -> Result<()> {
    for (dir, (lib, file)) in &tree.source_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let Some(member) = name
                .strip_suffix(".MBR")
                .or_else(|| name.strip_suffix(".mbr"))
            else {
                continue;
            };
            if !e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let now = meta(&p);
            let known = tree.members.get(&p);
            if known.is_some_and(|w| now == Some((w.mtime_ns, w.size))) {
                continue;
            }
            let data = std::fs::read(&p)?;
            let text = String::from_utf8_lossy(&data).replace("\r\n", "\n");
            let source = text.strip_suffix('\n').unwrap_or(&text).to_string();
            let member = member.to_ascii_uppercase();
            let (srctype, mtext) = match known {
                Some(w) => client
                    .query_opt(
                        "SELECT srctype, text FROM as4tainr.source_members
                          WHERE library = $1 AND file = $2 AND member = $3",
                        &[&w.library, &w.file, &w.member],
                    )
                    .await?
                    .map(|r| (r.get(0), r.get(1)))
                    .unwrap_or_default(),
                None => (String::new(), String::new()),
            };
            crate::source::put_member(
                client,
                &crate::source::Member {
                    library: lib.clone(),
                    file: file.clone(),
                    name: member,
                    srctype,
                    text: mtext,
                    source,
                    stmf: String::new(),
                    ccsid: 37,
                    changed_us: None,
                },
                true,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_a_command_names() {
        let l = named_libraries(
            "/QSYS.LIB/ILEDITOR.LIB/QRPGLESRC.FILE",
            "for f in '/QSYS.LIB/MYLIB.LIB/OBJ.*' /qsys.lib/other.lib; do echo; done",
        );
        assert_eq!(
            l.into_iter().collect::<Vec<_>>(),
            ["ILEDITOR", "MYLIB", "OTHER"]
        );
        assert!(named_libraries("/home/me", "ls /QSYS.LIB").is_empty());
    }

    #[test]
    fn a_member_as_text() {
        assert_eq!(member_text("a  \n  b\n"), "a\n  b\n");
        assert_eq!(member_text(""), "");
    }
}
