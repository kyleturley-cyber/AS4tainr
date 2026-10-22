//! Source physical files and their members.
//!
//! On IBM i, program source lives in source physical files (QDDSSRC,
//! QRPGLESRC, QCLSRC...), one member per program or file, each with a
//! source type (PF, LF, DSPF, RPGLE, CLLE...). The create commands take
//! SRCFILE and SRCMBR and copy what they need: changing a member does not
//! change an object until it is created again.
//!
//! as4tainr keeps a source file as a *FILE object (attribute PF-SRC) and its
//! members as text in `as4tainr.source_members`. Source files cannot be read
//! with record-level access yet (no SRCSEQ/SRCDAT/SRCDTA records).

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.source_members (
    library  text NOT NULL,
    file     text NOT NULL,
    member   text NOT NULL,
    srctype  text NOT NULL DEFAULT '',
    text     text NOT NULL DEFAULT '',
    source   text NOT NULL,
    changed  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, file, member)
);
-- Stream files (a small IFS): include files that source refers to by
-- path, such as RPG `/INCLUDE 'header.rpgleinc'`.
CREATE TABLE IF NOT EXISTS as4tainr.stream_files (
    path    text PRIMARY KEY,
    data    text NOT NULL,
    changed timestamptz NOT NULL DEFAULT now()
);
"#;

/// The source type of a member, from a stream file's extension as the
/// usual IBM i tooling (and git repositories of IBM i source) name them.
pub fn type_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "pf" => "PF",
        "lf" => "LF",
        "dspf" => "DSPF",
        "prtf" => "PRTF",
        "rpgle" => "RPGLE",
        "sqlrpgle" => "SQLRPGLE",
        "rpg" => "RPG",
        "clle" => "CLLE",
        "clp" => "CLP",
        "cmd" => "CMD",
        "cbl" | "cblle" => "CBLLE",
        "sql" | "table" | "view" | "index" => "SQL",
        "bnd" => "BND",
        "bnddir" => "BNDDIR",
        "txt" => "TXT",
        _ => return None,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Member {
    pub library: String,
    pub file: String,
    pub name: String,
    pub srctype: String,
    pub text: String,
    pub source: String,
}

/// CRTSRCPF. Fails with CPF7302 when the file exists.
pub async fn create_file(client: &Client, library: &str, name: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF2110: Library {lib} not found.");
    }
    let n = client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             VALUES ($1, $2, '*FILE', 'PF-SRC', $3) ON CONFLICT DO NOTHING",
            &[&lib, &name, &text],
        )
        .await?;
    if n == 0 {
        bail!("CPF7302: File {name} not created in library {lib}: it already exists.");
    }
    Ok(())
}

/// Is LIB/NAME a source file?
pub async fn is_source_file(client: &Client, library: &str, name: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE' AND attribute='PF-SRC'",
            &[&library, &name],
        )
        .await?
        .is_some())
}

/// Add a member, or replace it (`replace`; otherwise an existing member
/// is an error).
pub async fn put_member(client: &Client, m: &Member, replace: bool) -> Result<()> {
    let member = validate_name("member", &m.name)?;
    if !is_source_file(client, &m.library, &m.file).await? {
        bail!(
            "CPF9812: File {} in library {} not found, or it is not a source file.",
            m.file,
            m.library
        );
    }
    let sql = if replace {
        "INSERT INTO as4tainr.source_members VALUES ($1,$2,$3,$4,$5,$6, now())
         ON CONFLICT (library, file, member) DO UPDATE
         SET srctype=EXCLUDED.srctype, text=EXCLUDED.text, source=EXCLUDED.source, changed=now()"
    } else {
        "INSERT INTO as4tainr.source_members VALUES ($1,$2,$3,$4,$5,$6, now()) ON CONFLICT DO NOTHING"
    };
    let n = client
        .execute(
            sql,
            &[&m.library, &m.file, &member, &m.srctype, &m.text, &m.source],
        )
        .await?;
    if n == 0 {
        bail!(
            "Member {member} already exists in file {} in library {}.",
            m.file,
            m.library
        );
    }
    Ok(())
}

/// A member of a source file found through a list of libraries (the first
/// library that has the source file).
pub async fn find_member(
    client: &Client,
    libs: &[String],
    file: &str,
    member: &str,
) -> Result<Member> {
    let file = file.to_ascii_uppercase();
    let member = member.to_ascii_uppercase();
    for lib in libs {
        if !is_source_file(client, lib, &file).await? {
            continue;
        }
        let Some(r) = client
            .query_opt(
                "SELECT srctype, text, source FROM as4tainr.source_members
                 WHERE library=$1 AND file=$2 AND member=$3",
                &[lib, &file, &member],
            )
            .await?
        else {
            bail!("CPF9815: Member {member} file {file} in library {lib} not found.");
        };
        return Ok(Member {
            library: lib.clone(),
            file,
            name: member,
            srctype: r.get(0),
            text: r.get(1),
            source: r.get(2),
        });
    }
    bail!("CPF9812: File {file} in library *LIBL not found.");
}

/// The members of a source file: (member, type, text), by name.
pub async fn members(
    client: &Client,
    library: &str,
    file: &str,
) -> Result<Vec<(String, String, String)>> {
    Ok(client
        .query(
            "SELECT member, srctype, text FROM as4tainr.source_members
             WHERE library=$1 AND file=$2 ORDER BY member",
            &[&library, &file],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect())
}

/// Store a stream file (replacing it) in the IFS, as UTF-8 text.
pub async fn put_stream_file(client: &Client, path: &str, data: &str) -> Result<()> {
    crate::ifs::put_text(client, path, data).await
}

/// A stream file by path: an absolute path as is; a relative one (an
/// include named relative to an include directory) is the shortest path
/// that ends with it. Case is ignored, as in the IFS root file system.
/// The text in its CCSID.
pub async fn find_stream_file(client: &Client, path: &str) -> Result<Option<(String, String)>> {
    let want = path.trim().trim_start_matches("./");
    let suffix = format!(
        "%/{}",
        want.trim_start_matches('/')
            .to_lowercase()
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let r = client
        .query_opt(
            "SELECT path, data, ccsid FROM as4tainr.ifs
             WHERE kind = 'F' AND (path_key = lower($1) OR path_key LIKE $2)
             ORDER BY path_key = lower($1) DESC, length(path) LIMIT 1",
            &[&want, &suffix],
        )
        .await?;
    Ok(r.map(|r| {
        let data: Vec<u8> = r.get(1);
        (r.get(0), crate::ifs::codec::text(r.get(2), &data))
    }))
}

/// The value of KEYWORD(...) in a command, quotes removed.
fn keyword_value(text: &str, keyword: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let at = upper.find(&format!("{keyword}("))? + keyword.len() + 1;
    let rest = &text[at..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().trim_matches('\'').to_string())
}

/// CL `INCLUDE SRCSTMF('path')` or `INCLUDE SRCMBR(name) SRCFILE(lib/file)`:
/// the include's text replaces the statement before the program is
/// checked, as the CL compiler does. Includes may include (10 levels).
pub async fn expand_cl_includes(client: &Client, libs: &[String], source: &str) -> Result<String> {
    let mut text = source.to_string();
    for _ in 0..10 {
        let mut changed = false;
        let mut out = String::new();
        for line in text.lines() {
            let t = line.trim_start();
            if !t.to_ascii_uppercase().starts_with("INCLUDE ") {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            changed = true;
            if let Some(path) = keyword_value(t, "SRCSTMF") {
                let Some((_, data)) = find_stream_file(client, &path).await? else {
                    bail!("INCLUDE: stream file {path} not found.");
                };
                out.push_str(&data);
            } else if let Some(member) = keyword_value(t, "SRCMBR") {
                let file = keyword_value(t, "SRCFILE").unwrap_or_else(|| "QCLSRC".into());
                let (libs, file) = match file.split_once('/') {
                    Some((l, f)) if l != "*LIBL" => (vec![l.to_ascii_uppercase()], f.to_string()),
                    Some((_, f)) => (libs.to_vec(), f.to_string()),
                    None => (libs.to_vec(), file),
                };
                out.push_str(&find_member(client, &libs, &file, &member).await?.source);
            } else {
                bail!("INCLUDE needs SRCSTMF or SRCMBR.");
            }
            out.push('\n');
        }
        text = out;
        if !changed {
            return Ok(text);
        }
    }
    bail!("INCLUDE: includes nested more than 10 deep.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_types_from_extensions() {
        // The source types are those of IBM's SEU/PDM (CL reference,
        // SRCTYPE values); the extensions are the usual stream-file names.
        assert_eq!(type_for_extension("RPGLE"), Some("RPGLE"));
        assert_eq!(type_for_extension("clp"), Some("CLP"));
        assert_eq!(type_for_extension("dspf"), Some("DSPF"));
        assert_eq!(type_for_extension("csv"), None);
    }
}
