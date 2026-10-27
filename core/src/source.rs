//! Source physical files and their members.
//!
//! On IBM i, program source lives in source physical files (QDDSSRC,
//! QRPGLESRC, QCLSRC...), one member per program or file, each with a
//! source type (PF, LF, DSPF, RPGLE, CLLE...). The create commands take
//! SRCFILE and SRCMBR and copy what they need: changing a member does not
//! change an object until it is created again.
//!
//! as4tainr keeps a source file as a *FILE object (attribute PF-SRC) and its
//! members as text in `as4tainr.source_members`. A program reads and
//! changes a member with record-level access as IBM i's records: SRCSEQ
//! (sequence number, ZONED(6,2)), SRCDAT (date changed, ZONED(6,0)) and
//! SRCDTA (the line, the record length less 12), record format named as
//! the file. When it is opened the member's lines become the records of
//! a table named as the file; when it is closed they are the member's
//! lines again ([`open_member`], [`close_member`]).

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
-- A source file's record length (CRTSRCPF RCDLEN; 92 when not kept).
CREATE TABLE IF NOT EXISTS as4tainr.source_files (
    library  text NOT NULL,
    name     text NOT NULL,
    rcdlen   int NOT NULL,
    PRIMARY KEY (library, name)
);
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
-- When a member was added: *FIRST is the member added first.
ALTER TABLE as4tainr.source_members ADD COLUMN IF NOT EXISTS added timestamptz NOT NULL DEFAULT now();
-- Usage data: when the member was last used and on how many days.
ALTER TABLE as4tainr.source_members ADD COLUMN IF NOT EXISTS last_used timestamptz;
ALTER TABLE as4tainr.source_members ADD COLUMN IF NOT EXISTS days_used integer NOT NULL DEFAULT 0;
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
        "cmd" | "cmdsrc" => "CMD",
        "cbl" | "cblle" => "CBLLE",
        // SQL scripts, and the object-type extensions of IBM's build
        // tools (Bob: .table, .view, .index, .sqlprc, .sqludf, .sqltrg,
        // .sqlseq...).
        "sql" | "table" | "view" | "index" | "sqlprc" | "sqludf" | "sqludt" | "sqltrg"
        | "sqlseq" | "sqlalias" | "sqlvar" | "sqlmask" | "sqlperm" => "SQL",
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
    /// The stream file the source came from (SRCSTMF), or "".
    pub stmf: String,
    /// The CCSID of the source (a stream file's, or 37 for a member).
    pub ccsid: i32,
    /// When the source was last changed (microseconds since 1970, UTC),
    /// for compiler listings.
    pub changed_us: Option<i64>,
}

/// CRTSRCPF. Fails with CPF7302 when the file exists.
pub async fn create_file(client: &Client, library: &str, name: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF2110: Library {lib} does not exist.");
    }
    let n = client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             VALUES ($1, $2, '*FILE', 'PF-SRC', $3) ON CONFLICT DO NOTHING",
            &[&lib, &name, &text],
        )
        .await?;
    if n == 0 {
        return Err(crate::catalog::file_exists_error(&name, &lib));
    }
    Ok(())
}

/// Keep a source file's record length (CRTSRCPF RCDLEN).
pub async fn set_record_length(
    client: &Client,
    library: &str,
    name: &str,
    rcdlen: i32,
) -> Result<()> {
    client
        .execute(
            "INSERT INTO as4tainr.source_files (library, name, rcdlen) VALUES ($1, $2, $3)
             ON CONFLICT (library, name) DO UPDATE SET rcdlen = EXCLUDED.rcdlen",
            &[
                &library.to_ascii_uppercase(),
                &name.to_ascii_uppercase(),
                &rcdlen,
            ],
        )
        .await?;
    Ok(())
}

/// A source file's record length: CRTSRCPF's RCDLEN, 92 (its default)
/// for files created before it was kept.
pub async fn record_length(client: &Client, library: &str, name: &str) -> Result<u32> {
    Ok(client
        .query_opt(
            "SELECT rcdlen FROM as4tainr.source_files WHERE library=$1 AND name=$2",
            &[&library, &name],
        )
        .await?
        .map(|r| r.get::<_, i32>(0) as u32)
        .unwrap_or(92))
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
                "SELECT srctype, text, source,
                        (extract(epoch FROM changed) * 1000000)::bigint
                   FROM as4tainr.source_members
                 WHERE library=$1 AND file=$2 AND member=$3",
                &[lib, &file, &member],
            )
            .await?
        else {
            bail!("CPF9815: Member {member} of file {file} in library {lib} does not exist.");
        };
        return Ok(Member {
            library: lib.clone(),
            file,
            name: member,
            srctype: r.get(0),
            text: r.get(1),
            source: r.get(2),
            stmf: String::new(),
            ccsid: 37,
            changed_us: r.get(3),
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
    let co = crate::collate::clause(client).await?;
    Ok(client
        .query(
            &format!(
                "SELECT member, srctype, text FROM as4tainr.source_members
                  WHERE library=$1 AND file=$2 ORDER BY member{co}"
            ),
            &[&library, &file],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect())
}

/// DLTF of a source file: the file, its members and its record length.
pub async fn delete_file(client: &Client, library: &str, name: &str) -> Result<()> {
    for sql in [
        "DELETE FROM as4tainr.source_members WHERE library=$1 AND file=$2",
        "DELETE FROM as4tainr.source_files WHERE library=$1 AND name=$2",
        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
    ] {
        client.execute(sql, &[&library, &name]).await?;
    }
    Ok(())
}

/// RMVM: remove a member of a source file.
pub async fn remove_member(client: &Client, library: &str, file: &str, member: &str) -> Result<()> {
    client
        .execute(
            "DELETE FROM as4tainr.source_members WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member],
        )
        .await?;
    Ok(())
}

/// Store a stream file (replacing it) in the IFS, as UTF-8 text.
pub async fn put_stream_file(client: &Client, path: &str, data: &str) -> Result<()> {
    crate::ifs::put_text(client, path, data).await
}

/// The first of these paths that is a stream file (exact paths, the IFS's
/// names compared without case): its path and text.
pub async fn first_stream_file(
    client: &Client,
    paths: &[String],
) -> Result<Option<(String, String)>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let keys: Vec<String> = paths.iter().map(|p| p.to_lowercase()).collect();
    let r = client
        .query_opt(
            "SELECT path, data, ccsid FROM as4tainr.ifs
             WHERE kind = 'F' AND path_key = ANY($1)
             ORDER BY array_position($1, path_key) LIMIT 1",
            &[&keys],
        )
        .await?;
    Ok(r.map(|r| {
        let data: Vec<u8> = r.get(1);
        (r.get(0), crate::ifs::codec::text(r.get(2), &data))
    }))
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

/// A stream file's text by its path (the IFS root is not case-sensitive).
async fn stream_file_text(client: &Client, path: &str) -> Result<Option<String>> {
    let r = client
        .query_opt(
            "SELECT data, ccsid FROM as4tainr.ifs WHERE kind = 'F' AND path_key = lower($1)",
            &[&path],
        )
        .await?;
    Ok(r.map(|r| {
        let data: Vec<u8> = r.get(0);
        crate::ifs::codec::text(r.get(1), &data)
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
    expand_cl_includes_from(client, libs, source, "", &[]).await
}

/// A relative path from a directory: `.` and `..` resolved by name.
pub fn join_relative(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `expand_cl_includes` for a source from the stream file `from` (or ""):
/// a relative SRCSTMF path is looked for from the source's directory,
/// then from each INCDIR directory (CL reference, INCLUDE SRCSTMF: "the
/// path name is completed by pre-appending the searched including
/// directory"; the source directory first, then INCDIR). The IFS root is
/// not case-sensitive.
pub async fn expand_cl_includes_from(
    client: &Client,
    libs: &[String],
    source: &str,
    from: &str,
    incdir: &[String],
) -> Result<String> {
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
                let mut found = None;
                if !path.starts_with('/') {
                    let mut dirs: Vec<String> = Vec::new();
                    if let Some((dir, _)) = from.rsplit_once('/') {
                        dirs.push(dir.to_string());
                    }
                    dirs.extend(incdir.iter().cloned());
                    for d in dirs {
                        let full = join_relative(&d, &path);
                        if let Some(f) = stream_file_text(client, &full).await? {
                            found = Some(f);
                            break;
                        }
                    }
                }
                if found.is_none() {
                    found = find_stream_file(client, &path).await?.map(|x| x.1);
                }
                let Some(data) = found else {
                    bail!("INCLUDE: stream file {path} not found.");
                };
                out.push_str(&data);
            } else if let Some(member) = keyword_value(t, "SRCMBR") {
                let file = keyword_value(t, "SRCFILE").unwrap_or_else(|| "QCLSRC".into());
                let (libs, file) = match file.split_once('/') {
                    Some((l, f)) if l != "*LIBL" => (
                        vec![crate::qtemp::real(&l.to_ascii_uppercase())],
                        f.to_string(),
                    ),
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

/// A source file as a database file: its record format (named as the
/// file) SRCSEQ, SRCDAT, SRCDTA.
pub fn record_file(name: &str, rcdlen: u32) -> crate::dds::PhysicalFile {
    use crate::dds::{DataType, FieldDef};
    let field = |n: &str, t: DataType, len: u32, dec: u32, text: &str| FieldDef {
        name: n.to_string(),
        data_type: t,
        length: len,
        decimals: dec,
        text: text.to_string(),
        varlen: false,
        null_capable: false,
        default: None,
        alias: None,
        identity: None,
        double: false,
        generated: None,
        hidden: false,
    };
    crate::dds::PhysicalFile {
        name: name.to_string(),
        format: name.to_string(),
        text: String::new(),
        unique: false,
        fields: vec![
            field("SRCSEQ", DataType::Zoned, 6, 2, "Sequence number"),
            field("SRCDAT", DataType::Zoned, 6, 0, "Date changed"),
            field(
                "SRCDTA",
                DataType::Char,
                rcdlen.saturating_sub(12).max(1),
                0,
                "Source data",
            ),
        ],
        keys: vec![],
        descending: vec![],
        view: None,
        source: Some(String::new()),
    }
}

/// Open a member of a source file for record-level access: `member`, or
/// the first one added (*FIRST). Its lines become the records of the
/// table LIB.FILE (sequence numbers 1.00, 2.00...; date 0). The member's
/// name.
/// A member was used (opened to read or write it): its last-used date,
/// and one more day in its days-used count when it was not used today
/// yet (DSPFD TYPE(*MBRLIST) MLUDAT and MLUCNT: PUB400 2026-10-27,
/// tools/golden/verify3.txt x_mbrl_vals: a member written by CPYFRMSTMF
/// and read by CPYF the same day, 1; a member never opened, blank and 0).
pub async fn note_used(client: &Client, library: &str, file: &str, member: &str) {
    let _ = client
        .execute(
            "UPDATE as4tainr.source_members
                SET days_used = days_used + CASE WHEN last_used IS NULL
                      OR (last_used AT TIME ZONE 'UTC')::date <> (now() AT TIME ZONE 'UTC')::date
                    THEN 1 ELSE 0 END,
                    last_used = now()
              WHERE library = $1 AND file = $2 AND member = $3",
            &[&library, &file, &member],
        )
        .await;
}

pub async fn open_member(
    client: &Client,
    library: &str,
    file: &str,
    member: Option<&str>,
) -> Result<String> {
    let member = match member.map(|m| m.trim().to_ascii_uppercase()) {
        Some(m) if !m.is_empty() && m != "*FIRST" => m,
        _ => client
            .query_opt(
                "SELECT member FROM as4tainr.source_members WHERE library=$1 AND file=$2
                 ORDER BY added, member LIMIT 1",
                &[&library, &file],
            )
            .await?
            .map(|r| r.get(0))
            .ok_or_else(|| {
                anyhow::anyhow!("CPF4102: File {file} in library {library} has no members.")
            })?,
    };
    let text: String = client
        .query_opt(
            "SELECT source FROM as4tainr.source_members WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member],
        )
        .await?
        .map(|r| r.get(0))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CPF9815: Member {member} of file {file} in library {library} does not exist."
            )
        })?;
    note_used(client, library, file, &member).await;
    let rcdlen = record_length(client, library, file).await?;
    let pf = record_file(file, rcdlen);
    let table = format!(
        "{}.{}",
        crate::catalog::ident(library),
        crate::catalog::ident(file)
    );
    let mut cols = vec!["rrn bigserial PRIMARY KEY".to_string()];
    for f in &pf.fields {
        let coll = if f.data_type == crate::dds::DataType::Char {
            " COLLATE \"C\""
        } else {
            ""
        };
        cols.push(format!(
            "{} {}{} NOT NULL{}",
            f.column(),
            f.sql_type(),
            coll,
            f.default_clause()
        ));
    }
    client
        .batch_execute(&format!(
            "DROP TABLE IF EXISTS {table}; CREATE TABLE {table} ({})",
            cols.join(", ")
        ))
        .await?;
    let width = pf.fields[2].length as usize;
    let lines: Vec<String> = if text.is_empty() {
        vec![]
    } else {
        text.lines()
            .map(|l| l.chars().take(width).collect::<String>())
            .collect()
    };
    let seqs: Vec<String> = (1..=lines.len())
        .map(|n| format!("{}.00", n % 10000))
        .collect();
    client
        .execute(
            &format!(
                "INSERT INTO {table} ({}, {}, {}) SELECT s::numeric, 0, rpad(d, {width})
                   FROM unnest($1::text[], $2::text[]) AS t(s, d)",
                pf.fields[0].column(),
                pf.fields[1].column(),
                pf.fields[2].column()
            ),
            &[&seqs, &lines],
        )
        .await?;
    Ok(member)
}

/// Close a source member opened by [`open_member`]: the records, in
/// arrival order, are its lines again (trailing blanks dropped).
pub async fn close_member(client: &Client, library: &str, file: &str, member: &str) -> Result<()> {
    let pf = record_file(file, 92);
    let table = format!(
        "{}.{}",
        crate::catalog::ident(library),
        crate::catalog::ident(file)
    );
    let rows = client
        .query(
            &format!(
                "SELECT rtrim({}) FROM {table} ORDER BY rrn",
                pf.fields[2].column()
            ),
            &[],
        )
        .await?;
    let mut text: String = rows
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    client
        .execute(
            "UPDATE as4tainr.source_members SET source=$4, changed=now()
             WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member, &text],
        )
        .await?;
    client
        .batch_execute(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // CL reference, INCLUDE SRCSTMF: a relative path is completed with
    // the including directory (here by name: `..` goes up one).
    #[test]
    fn relative_include_paths() {
        assert_eq!(
            join_relative("/p/Tools/UserSpaces", "../../Common/Includes/inc.clle"),
            "/p/Common/Includes/inc.clle"
        );
        assert_eq!(join_relative("/p/a", "./b/c"), "/p/a/b/c");
        assert_eq!(join_relative("/", "../x"), "/x");
    }

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
