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
-- Each line's SRCSEQ and SRCDAT as twelve digits (ssssssdddddd), the
-- lines' separated by newlines; null: the lines are numbered 1.00,
-- 2.00... with date 0 ([`line_meta`]).
ALTER TABLE as4tainr.source_members ADD COLUMN IF NOT EXISTS seqdat text;
-- Stream files (a small IFS): include files that source refers to by
-- path, such as RPG `/INCLUDE 'header.rpgleinc'`.
-- The /COPY and /INCLUDE files an RPG object was created with (by a
-- digest of them all, `rpg::snapshot_option`): a program or module whose
-- include is gone (a QTEMP member of the job that created it) still
-- compiles from what it was created with.
CREATE TABLE IF NOT EXISTS as4tainr.include_snapshots (
    snap  text NOT NULL,
    id    text NOT NULL,
    text  text NOT NULL,
    PRIMARY KEY (snap, id)
);
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
         SET srctype=EXCLUDED.srctype, text=EXCLUDED.text, source=EXCLUDED.source, changed=now(),
             seqdat=CASE WHEN source_members.source = EXCLUDED.source THEN source_members.seqdat END"
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
    bail!("CPF9812: There is no file {file} in library *LIBL.");
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

/// The trigger function that adds a record at the end of a source member
/// for an INSERT through a view of the member (an alias, or SQL naming
/// the source file): the trigger's arguments are the library, file and
/// member; the record's SRCSEQ and SRCDAT are kept (`seqdat`).
pub fn insert_function(name: &str) -> String {
    format!(
        r#"CREATE OR REPLACE FUNCTION {name}() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $f$
DECLARE
  -- The record's SRCSEQ and SRCDAT as twelve digits (seqdat).
  meta text := lpad(CAST(CAST(abs(round(coalesce(NEW.srcseq, 0) * 100)) AS bigint) % 1000000 AS text), 6, '0')
            || lpad(CAST(CAST(abs(coalesce(NEW.srcdat, 0)) AS bigint) % 1000000 AS text), 6, '0');
BEGIN
  -- The member's lines without the newline that ends the last.
  UPDATE as4tainr.source_members
     SET source = CASE WHEN source = '' THEN rtrim(coalesce(NEW.srcdta, ''))
                       ELSE regexp_replace(source, E'\n$', '') || E'\n' || rtrim(coalesce(NEW.srcdta, '')) END,
         seqdat = CASE WHEN source = '' THEN meta
                       ELSE coalesce(seqdat,
                              (SELECT string_agg(lpad(CAST((l.n * 100) % 1000000 AS text), 6, '0') || '000000',
                                                 E'\n' ORDER BY l.n)
                                 FROM regexp_split_to_table(regexp_replace(source, E'\n$', ''), E'\n')
                                      WITH ORDINALITY AS l(t, n)))
                            || E'\n' || meta END,
         changed = now()
   WHERE library = TG_ARGV[0] AND file = TG_ARGV[1] AND member = TG_ARGV[2];
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Member % of file % in library % not found.', TG_ARGV[2], TG_ARGV[1], TG_ARGV[0]
      USING ERRCODE = '42704';
  END IF;
  RETURN NEW;
END $f$;
"#
    )
}

/// The trigger function for UPDATE and DELETE of a source member's
/// records through a view of them with their relative record numbers
/// (`rrn`): the line is changed or taken out (with its SRCSEQ and SRCDAT,
/// `seqdat`). Records deleted earlier in the same statement are counted
/// so that later ones still find their lines (PUB400 2026-10-28,
/// tools/golden/verify13.txt t13_upd, t13_del: UPDATE ... WHERE SRCSEQ
/// = 2 and DELETE ... WHERE SRCSEQ = 1 on the source file).
pub fn change_function(name: &str) -> String {
    format!(
        r#"CREATE OR REPLACE FUNCTION {name}() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $f$
DECLARE
  src text;
  meta text;
  lines text[];
  metas text[];
  n int;
  i int;
  stmt text := CAST(statement_timestamp() AS text) || '|' || TG_ARGV[0] || '/' || TG_ARGV[1] || '/' || TG_ARGV[2];
  done text := coalesce(current_setting('as4tainr.srcdel', true), '');
  gone int[] := '{{}}';
BEGIN
  SELECT source, seqdat INTO src, meta FROM as4tainr.source_members
   WHERE library = TG_ARGV[0] AND file = TG_ARGV[1] AND member = TG_ARGV[2] FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Member % of file % in library % not found.', TG_ARGV[2], TG_ARGV[1], TG_ARGV[0]
      USING ERRCODE = '42704';
  END IF;
  src := regexp_replace(src, E'\n$', '');
  lines := CASE WHEN src = '' THEN '{{}}'::text[] ELSE string_to_array(src, E'\n') END;
  metas := coalesce(string_to_array(meta, E'\n'), '{{}}'::text[]);
  FOR i IN 1 .. coalesce(array_length(lines, 1), 0) LOOP
    IF i > coalesce(array_length(metas, 1), 0) OR length(metas[i]) <> 12 THEN
      metas[i] := lpad(CAST((i * 100) % 1000000 AS text), 6, '0') || '000000';
    END IF;
  END LOOP;
  IF split_part(done, '#', 1) = stmt AND split_part(done, '#', 2) <> '' THEN
    gone := string_to_array(split_part(done, '#', 2), ',')::int[];
  END IF;
  n := OLD.rrn - (SELECT count(*) FROM unnest(gone) g WHERE g < OLD.rrn);
  IF n < 1 OR n > coalesce(array_length(lines, 1), 0) THEN
    RETURN NULL;
  END IF;
  IF TG_OP = 'DELETE' THEN
    lines := lines[1 : n - 1] || lines[n + 1 :];
    metas := metas[1 : n - 1] || metas[n + 1 :];
    gone := gone || CAST(OLD.rrn AS int);
    PERFORM set_config('as4tainr.srcdel', stmt || '#' || array_to_string(gone, ','), true);
  ELSE
    lines[n] := rtrim(coalesce(NEW.srcdta, ''));
    metas[n] := lpad(CAST(CAST(abs(round(coalesce(NEW.srcseq, 0) * 100)) AS bigint) % 1000000 AS text), 6, '0')
             || lpad(CAST(CAST(abs(coalesce(NEW.srcdat, 0)) AS bigint) % 1000000 AS text), 6, '0');
  END IF;
  UPDATE as4tainr.source_members
     SET source = array_to_string(lines, E'\n'),
         seqdat = CASE WHEN coalesce(array_length(lines, 1), 0) = 0 THEN NULL
                       ELSE array_to_string(metas[1 : array_length(lines, 1)], E'\n') END,
         changed = now()
   WHERE library = TG_ARGV[0] AND file = TG_ARGV[1] AND member = TG_ARGV[2];
  RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END $f$;
"#
    )
}

/// The trigger function for UPDATE and DELETE through an alias over a
/// source member (its view has no relative record numbers): the first
/// line with the old row's SRCSEQ, SRCDAT and SRCDTA is changed or taken
/// out (identical lines are the same record to the statement).
pub fn alias_change_function(name: &str) -> String {
    format!(
        r#"CREATE OR REPLACE FUNCTION {name}() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $f$
DECLARE
  src text;
  meta text;
  lines text[];
  metas text[];
  old_meta text := lpad(CAST(CAST(abs(round(coalesce(OLD.srcseq, 0) * 100)) AS bigint) % 1000000 AS text), 6, '0')
                || lpad(CAST(CAST(abs(coalesce(OLD.srcdat, 0)) AS bigint) % 1000000 AS text), 6, '0');
  n int := 0;
  i int;
BEGIN
  SELECT source, seqdat INTO src, meta FROM as4tainr.source_members
   WHERE library = TG_ARGV[0] AND file = TG_ARGV[1] AND member = TG_ARGV[2] FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'Member % of file % in library % not found.', TG_ARGV[2], TG_ARGV[1], TG_ARGV[0]
      USING ERRCODE = '42704';
  END IF;
  src := regexp_replace(src, E'\n$', '');
  lines := CASE WHEN src = '' THEN '{{}}'::text[] ELSE string_to_array(src, E'\n') END;
  metas := coalesce(string_to_array(meta, E'\n'), '{{}}'::text[]);
  FOR i IN 1 .. coalesce(array_length(lines, 1), 0) LOOP
    IF i > coalesce(array_length(metas, 1), 0) OR length(metas[i]) <> 12 THEN
      metas[i] := lpad(CAST((i * 100) % 1000000 AS text), 6, '0') || '000000';
    END IF;
    IF n = 0 AND metas[i] = old_meta AND rtrim(lines[i]) = rtrim(coalesce(OLD.srcdta, '')) THEN
      n := i;
    END IF;
  END LOOP;
  IF n = 0 THEN
    RETURN NULL;
  END IF;
  IF TG_OP = 'DELETE' THEN
    lines := lines[1 : n - 1] || lines[n + 1 :];
    metas := metas[1 : n - 1] || metas[n + 1 :];
  ELSE
    lines[n] := rtrim(coalesce(NEW.srcdta, ''));
    metas[n] := lpad(CAST(CAST(abs(round(coalesce(NEW.srcseq, 0) * 100)) AS bigint) % 1000000 AS text), 6, '0')
             || lpad(CAST(CAST(abs(coalesce(NEW.srcdat, 0)) AS bigint) % 1000000 AS text), 6, '0');
  END IF;
  UPDATE as4tainr.source_members
     SET source = array_to_string(lines, E'\n'),
         seqdat = CASE WHEN coalesce(array_length(lines, 1), 0) = 0 THEN NULL
                       ELSE array_to_string(metas[1 : array_length(lines, 1)], E'\n') END,
         changed = now()
   WHERE library = TG_ARGV[0] AND file = TG_ARGV[1] AND member = TG_ARGV[2];
  RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END $f$;
"#
    )
}

/// A source file an SQL statement names: Db2 for i reads and adds to its
/// first member (PUB400 2026-10-28, tools/golden/verify12.txt
/// n12_ins_file: INSERT INTO the source file added record 4 to M1).
#[derive(Debug, Clone, PartialEq)]
pub struct SqlSource {
    /// Where the statement names it qualified (`"lib"."file"`,
    /// `lib.file`): byte ranges replaced by the view. A bare name is
    /// left as it is: the temporary view of that name is found first.
    pub spans: Vec<(usize, usize)>,
    /// The temporary view put in its place.
    pub view: String,
    pub library: String,
    pub file: String,
    pub member: String,
}

/// The source files a translated SQL statement names, qualified
/// (`"lib"."file"`) or after FROM, JOIN, INTO, UPDATE or TABLE and
/// found first through the library list, with the views that stand for
/// them ([`sql_source_views`]). Text in quotes is not looked at.
pub async fn sql_sources(client: &Client, libl: &[String], sql: &str) -> Result<Vec<SqlSource>> {
    // Tokens outside string literals: quoted names keep their quotes.
    let b = sql.as_bytes();
    let mut toks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                i += 1;
            }
            i += 1;
        } else if c == b'"' {
            let st = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            i += 1;
            toks.push((st, i.min(b.len())));
        } else if c.is_ascii_alphanumeric() || b"_#@$".contains(&c) {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b"_#@$".contains(&b[i])) {
                i += 1;
            }
            toks.push((st, i));
        } else {
            if c == b'.' {
                toks.push((i, i + 1));
            }
            i += 1;
        }
    }
    let word = |k: usize| sql[toks[k].0..toks[k].1].to_string();
    let unq = |w: &str| w.trim_matches('"').to_ascii_uppercase();
    let adjacent = |k: usize| {
        k + 2 < toks.len()
            && word(k + 1) == "."
            && toks[k].1 == toks[k + 1].0
            && toks[k + 1].1 == toks[k + 2].0
            && word(k + 2) != "."
    };
    // (span, library if qualified, file).
    type Candidate = (Option<(usize, usize)>, Option<String>, String);
    let mut cands: Vec<Candidate> = Vec::new();
    for k in 0..toks.len() {
        let w = word(k);
        if w == "." || (k > 0 && word(k - 1) == "." && toks[k - 1].1 == toks[k].0) {
            continue;
        }
        if adjacent(k) {
            // Three parts (a column of lib.file) are not a table.
            if k + 4 < toks.len() && word(k + 3) == "." && toks[k + 2].1 == toks[k + 3].0 {
                continue;
            }
            cands.push((
                Some((toks[k].0, toks[k + 2].1)),
                Some(unq(&w)),
                unq(&word(k + 2)),
            ));
        } else if k > 0
            && !w.starts_with('"')
            && matches!(
                word(k - 1).to_ascii_uppercase().as_str(),
                "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE"
            )
        {
            cands.push((None, None, w.to_ascii_uppercase()));
        }
    }
    let mut out: Vec<SqlSource> = Vec::new();
    if cands.is_empty() {
        return Ok(out);
    }
    // The files of those names in the libraries concerned, in one query
    // (this runs before every SQL statement).
    let names: Vec<String> = cands.iter().map(|c| c.2.clone()).collect();
    let mut libs_all: Vec<String> = libl.to_vec();
    libs_all.extend(cands.iter().filter_map(|c| c.1.clone()));
    let files: Vec<(String, String, String)> = client
        .query(
            "SELECT library, name, attribute FROM as4tainr.objects
              WHERE objtype = '*FILE' AND name = ANY($1) AND library = ANY($2)",
            &[&names, &libs_all],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    if !files.iter().any(|f| f.2 == "PF-SRC") {
        return Ok(out);
    }
    for (span, lib, file) in cands {
        let libs = match &lib {
            Some(l) => vec![l.clone()],
            None => libl.to_vec(),
        };
        // The first library with a file of that name decides.
        let found = libs
            .iter()
            .find_map(|l| files.iter().find(|f| &f.0 == l && f.1 == file))
            .filter(|f| f.2 == "PF-SRC")
            .map(|f| f.0.clone());
        let Some(l) = found else { continue };
        let view = match lib {
            Some(_) => format!("as4src_{l}_{file}").to_ascii_lowercase(),
            None => file.to_ascii_lowercase(),
        };
        if let Some(o) = out.iter_mut().find(|o| o.view == view) {
            o.spans.extend(span);
            continue;
        }
        let member: String = client
            .query_opt(
                "SELECT member FROM as4tainr.source_members WHERE library=$1 AND file=$2
                  ORDER BY added, member LIMIT 1",
                &[&l, &file],
            )
            .await?
            .map(|r| r.get(0))
            .unwrap_or_default();
        out.push(SqlSource {
            spans: span.into_iter().collect(),
            view,
            library: l,
            file,
            member,
        });
    }
    Ok(out)
}

/// Make the temporary views (on the connection that runs the statement)
/// for the source files a statement names, and put them in its text: the
/// member's records SRCSEQ, SRCDAT, SRCDTA (and their relative record
/// numbers, RRN, which SELECT * leaves out as for any file); an INSERT
/// adds a record at the member's end, UPDATE and DELETE change and take
/// out records ([`change_function`]).
pub async fn sql_source_views(
    sql_client: &Client,
    sources: &[SqlSource],
    text: &str,
) -> Result<String> {
    let q = |v: &str| format!("'{}'", v.replace('\'', "''"));
    let mut text = text.to_string();
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for s in sources {
        let view = format!("pg_temp.{}", crate::catalog::ident(&s.view));
        sql_client
            .batch_execute(&format!(
                "DROP VIEW IF EXISTS {view};
                 CREATE TEMP VIEW {} AS SELECT rrn, srcseq, srcdat, srcdta
                   FROM as4tainr_svc.source_records({a});
                 CREATE TRIGGER as4tainr_source INSTEAD OF INSERT ON {view}
                   FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.source_insert({a});
                 CREATE TRIGGER as4tainr_source_change INSTEAD OF UPDATE OR DELETE ON {view}
                   FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.source_change({a})",
                crate::catalog::ident(&s.view),
                a = format!("{}, {}, {}", q(&s.library), q(&s.file), q(&s.member)),
            ))
            .await?;
        spans.extend(s.spans.iter().map(|&(a, b)| (a, b, view.clone())));
    }
    spans.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (a, b, v) in spans {
        text.replace_range(a..b, &v);
    }
    Ok(text)
}

/// A member's lines as records: (SRCSEQ, SRCDAT, SRCDTA), the numbers
/// as decimal text ("2.50", "261027"). `seqdat` is the member's kept
/// SRCSEQ and SRCDAT (twelve digits a line); lines it has none for are
/// numbered 1.00, 2.00... with date 0, as CPYFRMSTMF numbers them.
pub fn line_records(source: &str, seqdat: Option<&str>) -> Vec<(String, String, String)> {
    let meta: Vec<&str> = seqdat.map(|m| m.split('\n').collect()).unwrap_or_default();
    source
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let (seq, dat) = match meta.get(i) {
                Some(m) if m.len() == 12 && m.bytes().all(|b| b.is_ascii_digit()) => (
                    format!("{}.{}", m[..4].parse::<u32>().unwrap_or(0), &m[4..6]),
                    m[6..].parse::<u32>().unwrap_or(0).to_string(),
                ),
                _ => (format!("{}.00", (i + 1) % 10000), "0".to_string()),
            };
            (seq, dat, line.to_string())
        })
        .collect()
}

/// SRCSEQ and SRCDAT (decimal text) as the twelve digits kept for a line.
pub fn line_meta(seq: &str, dat: &str) -> String {
    let seq = seq.trim().trim_start_matches(['-', '+']);
    let (int, frac) = seq.split_once('.').unwrap_or((seq, ""));
    let digits = |s: &str| -> u64 {
        s.bytes()
            .filter(u8::is_ascii_digit)
            .fold(0u64, |n, b| (n * 10 + u64::from(b - b'0')) % 1_000_000_000)
    };
    let frac2: String = frac.chars().chain("00".chars()).take(2).collect();
    let s = (digits(int) % 10_000) * 100 + digits(&frac2);
    let d = dat.trim().trim_start_matches(['-', '+']);
    let d = digits(d.split_once('.').map(|x| x.0).unwrap_or(d)) % 1_000_000;
    format!("{s:06}{d:06}")
}

/// A member's kept SRCSEQ and SRCDAT (see [`line_records`]).
pub async fn member_seqdat(
    client: &Client,
    library: &str,
    file: &str,
    member: &str,
) -> Result<Option<String>> {
    Ok(client
        .query_opt(
            "SELECT seqdat FROM as4tainr.source_members WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member],
        )
        .await?
        .and_then(|r| r.get(0)))
}

/// Keep a member's SRCSEQ and SRCDAT (twelve digits a line, `line_meta`).
pub async fn set_member_seqdat(
    client: &Client,
    library: &str,
    file: &str,
    member: &str,
    seqdat: &str,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.source_members SET seqdat=$4 WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member, &seqdat],
        )
        .await?;
    Ok(())
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
        keywords: String::new(),
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
/// table LIB.FILE (with the SRCSEQ and SRCDAT kept for them, else
/// sequence numbers 1.00, 2.00... and date 0). The member's name.
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
    let (text, seqdat): (String, Option<String>) = client
        .query_opt(
            "SELECT source, seqdat FROM as4tainr.source_members WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member],
        )
        .await?
        .map(|r| (r.get(0), r.get(1)))
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
    let recs = line_records(&text, seqdat.as_deref());
    let seqs: Vec<&str> = recs.iter().map(|r| r.0.as_str()).collect();
    let dats: Vec<&str> = recs.iter().map(|r| r.1.as_str()).collect();
    let lines: Vec<String> = recs
        .iter()
        .map(|r| r.2.chars().take(width).collect())
        .collect();
    client
        .execute(
            &format!(
                "INSERT INTO {table} ({}, {}, {}) SELECT s::numeric, a::numeric, rpad(d, {width})
                   FROM unnest($1::text[], $2::text[], $3::text[]) WITH ORDINALITY AS t(s, a, d, n)
                  ORDER BY n",
                pf.fields[0].column(),
                pf.fields[1].column(),
                pf.fields[2].column()
            ),
            &[&seqs, &dats, &lines],
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
                "SELECT rtrim({}), {}::text, {}::text FROM {table} ORDER BY rrn",
                pf.fields[2].column(),
                pf.fields[0].column(),
                pf.fields[1].column()
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
    // The records' SRCSEQ and SRCDAT are kept (none when they are the
    // numbering a member without them gets).
    let meta: Vec<String> = rows
        .iter()
        .map(|r| line_meta(&r.get::<_, String>(1), &r.get::<_, String>(2)))
        .collect();
    let numbered = meta
        .iter()
        .enumerate()
        .all(|(i, m)| *m == line_meta(&format!("{}.00", (i + 1) % 10000), "0"));
    let seqdat = (!numbered).then(|| meta.join("\n"));
    client
        .execute(
            "UPDATE as4tainr.source_members SET source=$4, seqdat=$5, changed=now()
             WHERE library=$1 AND file=$2 AND member=$3",
            &[&library, &file, &member, &text, &seqdat],
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

    // SRCSEQ ZONED(6,2) and SRCDAT ZONED(6,0) kept for each line
    // (PUB400 2026-10-28, tools/golden/screens6.txt 07_dsppfm: the
    // records inserted with 1.00/261027 and 2.50/0 show as 000100261027
    // and 000250000000).
    #[test]
    fn sequence_numbers_and_dates() {
        assert_eq!(line_meta("1.00", "261027"), "000100261027");
        assert_eq!(line_meta("2.5", "0"), "000250000000");
        assert_eq!(line_meta("9999.99", "991231"), "999999991231");
        let r = line_records("a\nb\nc\n", Some("000100261027\n000250000000"));
        assert_eq!(r[0], ("1.00".into(), "261027".into(), "a".into()));
        assert_eq!(r[1], ("2.50".into(), "0".into(), "b".into()));
        // A line with none kept: its line number.
        assert_eq!(r[2], ("3.00".into(), "0".into(), "c".into()));
        assert_eq!(line_records("x", None)[0].0, "1.00");
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
