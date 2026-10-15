//! Object catalog. Libraries map to PostgreSQL schemas; a physical file's
//! data lives in a table of that schema, and its description (fields, keys)
//! lives in the `ibmish` catalog schema.

use crate::dds::{DataType, FieldDef, PhysicalFile};
use anyhow::{bail, Context, Result};
use tokio_postgres::Client;

const CATALOG_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS ibmish;
CREATE TABLE IF NOT EXISTS ibmish.objects (
    library   text NOT NULL,
    name      text NOT NULL,
    objtype   text NOT NULL,
    attribute text NOT NULL DEFAULT '',
    text      text NOT NULL DEFAULT '',
    created   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, name, objtype)
);
CREATE TABLE IF NOT EXISTS ibmish.files (
    library   text NOT NULL,
    name      text NOT NULL,
    format    text NOT NULL,
    is_unique boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS ibmish.file_fields (
    library   text NOT NULL,
    file      text NOT NULL,
    seq       int  NOT NULL,
    name      text NOT NULL,
    data_type text NOT NULL,
    length    int  NOT NULL,
    decimals  int  NOT NULL,
    text      text NOT NULL DEFAULT '',
    PRIMARY KEY (library, file, seq)
);
CREATE TABLE IF NOT EXISTS ibmish.display_files (
    library text NOT NULL,
    name    text NOT NULL,
    source  text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS ibmish.file_keys (
    library text NOT NULL,
    file    text NOT NULL,
    seq     int  NOT NULL,
    field   text NOT NULL,
    PRIMARY KEY (library, file, seq)
);
"#;

/// Quote an SQL identifier. Object names are validated before use, but we
/// still quote everything that reaches dynamic SQL.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.to_ascii_lowercase().replace('"', "\"\""))
}

/// IBM i object names: 1-10 chars, start with A-Z, $, # or @; then also
/// digits, `_` and `.`. Non-ASCII national characters are a known gap.
pub fn validate_name(kind: &str, name: &str) -> Result<String> {
    let up = name.trim().to_ascii_uppercase();
    let ok_first = |c: char| c.is_ascii_uppercase() || "$#@".contains(c);
    let ok_rest = |c: char| ok_first(c) || c.is_ascii_digit() || "_.".contains(c);
    let mut chars = up.chars();
    match chars.next() {
        Some(c) if ok_first(c) && up.len() <= 10 && chars.all(ok_rest) => Ok(up),
        _ => bail!("'{name}' is not a valid {kind} name"),
    }
}

/// A physical file as resolved through the library list.
#[derive(Debug, Clone)]
pub struct FileRef {
    pub library: String,
    pub file: PhysicalFile,
}

impl FileRef {
    pub fn table(&self) -> String {
        format!("{}.{}", ident(&self.library), ident(&self.file.name))
    }
}

/// Advisory lock key that serializes catalog creation ("IBMISH" in ASCII).
const INIT_LOCK_KEY: i64 = 0x49424D495348;

pub async fn init(client: &Client) -> Result<()> {
    // `CREATE SCHEMA IF NOT EXISTS` is not safe against concurrent callers:
    // two sessions can both see "missing" and one fails on a duplicate key.
    // Several processes may initialize at once (containers starting
    // together, parallel tests), so take a session-wide advisory lock.
    client
        .execute("SELECT pg_advisory_lock($1)", &[&INIT_LOCK_KEY])
        .await?;
    let result = async {
        client
            .batch_execute(CATALOG_DDL)
            .await
            .context("creating catalog")?;
        for lib in ["QSYS", "QGPL", "QTEMP"] {
            create_library(client, lib, "System library").await?;
        }
        crate::users::init(client).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    client
        .execute("SELECT pg_advisory_unlock($1)", &[&INIT_LOCK_KEY])
        .await?;
    result
}

/// CRTLIB: like `create_library`, but fails with CPF2111 when the library exists.
pub async fn create_library_new(client: &Client, library: &str, text: &str) -> Result<String> {
    let lib = validate_name("library", library)?;
    let exists = client
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?;
    if exists.is_some() {
        bail!("CPF2111: Library {lib} already exists.");
    }
    create_library(client, &lib, text).await?;
    Ok(lib)
}

/// One object in a library, for DSPLIB.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub name: String,
    pub objtype: String,
    pub attribute: String,
    pub text: String,
}

pub async fn library_exists(client: &Client, library: &str) -> Result<bool> {
    let lib = validate_name("library", library)?;
    Ok(client
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_some())
}

pub async fn library_text(client: &Client, library: &str) -> Result<String> {
    Ok(client
        .query_opt(
            "SELECT text FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&library],
        )
        .await?
        .map(|r| r.get(0))
        .unwrap_or_default())
}

/// Objects in a library, sorted by name then type.
pub async fn list_objects(client: &Client, library: &str) -> Result<Vec<ObjectEntry>> {
    let lib = validate_name("library", library)?;
    if !library_exists(client, &lib).await? {
        bail!("CPF2110: Library {lib} not found.");
    }
    Ok(client
        .query(
            "SELECT name, objtype, attribute, text FROM ibmish.objects WHERE library=$1 ORDER BY name, objtype",
            &[&lib],
        )
        .await?
        .into_iter()
        .map(|r| ObjectEntry { name: r.get(0), objtype: r.get(1), attribute: r.get(2), text: r.get(3) })
        .collect())
}

pub async fn create_library(client: &Client, library: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    client
        .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&lib)))
        .await?;
    client
        .execute(
            "INSERT INTO ibmish.objects (library, name, objtype, text) VALUES ('QSYS', $1, '*LIB', $2)
             ON CONFLICT DO NOTHING",
            &[&lib, &text],
        )
        .await?;
    Ok(())
}

/// CRTPF: create the catalog entries and the backing table in one transaction.
pub async fn create_physical_file(
    client: &mut Client,
    library: &str,
    pf: &PhysicalFile,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", &pf.name)?;
    for f in &pf.fields {
        validate_name("field", &f.name)?;
    }
    let tx = client.transaction().await?;
    let exists = tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?;
    if exists.is_some() {
        bail!("CPF7302: file {name} already exists in library {lib}");
    }
    let lib_exists = tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?;
    if lib_exists.is_none() {
        bail!("CPF2110: library {lib} not found");
    }

    tx.execute(
        "INSERT INTO ibmish.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','PF',$3)",
        &[&lib, &name, &pf.text],
    )
    .await?;
    tx.execute(
        "INSERT INTO ibmish.files (library, name, format, is_unique) VALUES ($1,$2,$3,$4)",
        &[&lib, &name, &pf.format, &pf.unique],
    )
    .await?;
    for (i, f) in pf.fields.iter().enumerate() {
        let seq = i as i32;
        let len = f.length as i32;
        let dec = f.decimals as i32;
        tx.execute(
            "INSERT INTO ibmish.file_fields VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            &[
                &lib,
                &name,
                &seq,
                &f.name,
                &f.data_type.code(),
                &len,
                &dec,
                &f.text,
            ],
        )
        .await?;
    }
    for (i, k) in pf.keys.iter().enumerate() {
        let seq = i as i32;
        tx.execute(
            "INSERT INTO ibmish.file_keys VALUES ($1,$2,$3,$4)",
            &[&lib, &name, &seq, k],
        )
        .await?;
    }

    // Character columns use the "C" collation so key order is byte order.
    // NOTE: IBM i orders keys by EBCDIC; see docs/fidelity-gaps.md.
    let mut cols: Vec<String> = vec!["rrn bigserial PRIMARY KEY".into()];
    for f in &pf.fields {
        let coll = if f.data_type == DataType::Char {
            " COLLATE \"C\""
        } else {
            ""
        };
        let default = if f.data_type == DataType::Char {
            "' '"
        } else {
            "0"
        };
        cols.push(format!(
            "{} {}{} NOT NULL DEFAULT {}",
            ident(&f.name),
            f.sql_type(),
            coll,
            default
        ));
    }
    let table = format!("{}.{}", ident(&lib), ident(&name));
    tx.batch_execute(&format!("CREATE TABLE {table} ({})", cols.join(", ")))
        .await?;
    if !pf.keys.is_empty() {
        let keycols: Vec<String> = pf.keys.iter().map(|k| ident(k)).collect();
        // Access path: key order, then arrival order (rrn) for duplicate keys.
        tx.batch_execute(&format!(
            "CREATE INDEX {} ON {table} ({}, rrn)",
            ident(&format!("{name}_key")),
            keycols.join(", ")
        ))
        .await?;
        if pf.unique {
            tx.batch_execute(&format!(
                "CREATE UNIQUE INDEX {} ON {table} ({})",
                ident(&format!("{name}_ukey")),
                keycols.join(", ")
            ))
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// CRTDSPF: store a display file's DDS source after checking that it parses.
pub async fn create_display_file(
    client: &mut Client,
    library: &str,
    name: &str,
    source: &str,
) -> Result<crate::dspf::DisplayFile> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    let parsed = crate::dspf::parse(&name, source)?;
    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} not found.");
    }
    if tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?
        .is_some()
    {
        bail!("CPF7302: File {name} already exists in library {lib}.");
    }
    tx.execute(
        "INSERT INTO ibmish.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','DSPF','')",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "INSERT INTO ibmish.display_files VALUES ($1,$2,$3)",
        &[&lib, &name, &source],
    )
    .await?;
    tx.commit().await?;
    Ok(parsed)
}

/// Find a display file through the library list; returns (library, source).
pub async fn resolve_display_file(
    client: &Client,
    libl: &[String],
    name: &str,
) -> Result<(String, String)> {
    let (lib_q, name) = match name.split_once('/') {
        Some((l, n)) => (
            Some(validate_name("library", l)?),
            validate_name("file", n)?,
        ),
        None => (None, validate_name("file", name)?),
    };
    let search: Vec<String> = lib_q.map(|l| vec![l]).unwrap_or_else(|| libl.to_vec());
    for lib in &search {
        if let Some(row) = client
            .query_opt(
                "SELECT source FROM ibmish.display_files WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            return Ok((lib.clone(), row.get(0)));
        }
    }
    bail!(
        "CPF9812: File {name} not found in library list {}",
        search.join(" ")
    )
}

/// Resolve a file name through the library list (first match wins).
pub async fn resolve_file(client: &Client, libl: &[String], name: &str) -> Result<FileRef> {
    let (lib_q, name) = match name.split_once('/') {
        Some((l, n)) => (
            Some(validate_name("library", l)?),
            validate_name("file", n)?,
        ),
        None => (None, validate_name("file", name)?),
    };
    let search: Vec<String> = match lib_q {
        Some(l) => vec![l],
        None => libl.to_vec(),
    };
    for lib in &search {
        let row = client
            .query_opt(
                "SELECT format, is_unique, (SELECT text FROM ibmish.objects o
                    WHERE o.library=f.library AND o.name=f.name AND o.objtype='*FILE')
                 FROM ibmish.files f WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?;
        let Some(row) = row else { continue };
        let fields = client
            .query(
                "SELECT name, data_type, length, decimals, text FROM ibmish.file_fields
                 WHERE library=$1 AND file=$2 ORDER BY seq",
                &[lib, &name],
            )
            .await?
            .into_iter()
            .map(|r| {
                Ok(FieldDef {
                    name: r.get(0),
                    data_type: DataType::from_code(r.get::<_, &str>(1))?,
                    length: r.get::<_, i32>(2) as u32,
                    decimals: r.get::<_, i32>(3) as u32,
                    text: r.get(4),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let keys = client
            .query(
                "SELECT field FROM ibmish.file_keys WHERE library=$1 AND file=$2 ORDER BY seq",
                &[lib, &name],
            )
            .await?
            .into_iter()
            .map(|r| r.get::<_, String>(0))
            .collect();
        return Ok(FileRef {
            library: lib.clone(),
            file: PhysicalFile {
                name: name.clone(),
                format: row.get(0),
                unique: row.get(1),
                text: row.get::<_, Option<String>>(2).unwrap_or_default(),
                fields,
                keys,
            },
        });
    }
    bail!(
        "CPF9812: file {name} not found in library list {}",
        search.join(" ")
    )
}
