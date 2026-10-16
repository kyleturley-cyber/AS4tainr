//! Object catalog. Libraries map to PostgreSQL schemas; a physical file's
//! data lives in a table of that schema, and its description (fields, keys)
//! lives in the `ibmish` catalog schema.

use crate::dds::{DataType, FieldDef, LogicalFile, PhysicalFile, Test};
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
ALTER TABLE ibmish.file_keys ADD COLUMN IF NOT EXISTS descending boolean NOT NULL DEFAULT false;
CREATE TABLE IF NOT EXISTS ibmish.programs (
    library   text NOT NULL,
    name      text NOT NULL,
    attribute text NOT NULL,
    source    text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS ibmish.logical_files (
    library    text NOT NULL,
    name       text NOT NULL,
    pf_library text NOT NULL,
    pf_name    text NOT NULL,
    -- the select/omit rules as SQL, for display
    selection  text NOT NULL DEFAULT 'TRUE',
    PRIMARY KEY (library, name)
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

/// Libraries every system has, with our descriptions.
pub const SYSTEM_LIBRARIES: &[(&str, &str)] = &[
    ("QSYS", "System library"),
    ("QSYS2", "System library for SQL objects"),
    ("QHLPSYS", "Library for help text"),
    ("QUSRSYS", "System library for user objects"),
    ("QGPL", "General purpose library"),
    ("QTEMP", "Temporary library"),
];

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
        client
            .batch_execute(crate::dtaara::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::jobs::DDL)
            .await
            .context("creating catalog")?;
        for (lib, text) in SYSTEM_LIBRARIES {
            create_library(client, lib, text).await?;
            // Catalogs made by older versions gave every one the same text.
            client
                .execute(
                    "UPDATE ibmish.objects SET text = $2
                     WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB' AND text = 'System library'",
                    &[lib, text],
                )
                .await?;
        }
        crate::users::init(client).await?;
        // SQL runs as a restricted role. A database user who may not
        // create roles still gets a working system, without SQL.
        if let Err(e) = crate::sql::init(client).await {
            eprintln!("ibmish: SQL is not available: {e:#}");
        }
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

/// CRTBNDCL: keep a checked CL program as a *PGM object (attribute CLLE).
/// With `replace`, an existing program of that name is replaced.
pub async fn create_cl_program(
    client: &mut Client,
    library: &str,
    name: &str,
    text: &str,
    source: &str,
    replace: bool,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("program", name)?;
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
    let exists = tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
            &[&lib, &name],
        )
        .await?
        .is_some();
    if exists && !replace {
        bail!("Program {name} in library {lib} already exists.");
    }
    tx.execute(
        "DELETE FROM ibmish.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "DELETE FROM ibmish.programs WHERE library=$1 AND name=$2",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "INSERT INTO ibmish.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*PGM','CLLE',$3)",
        &[&lib, &name, &text],
    )
    .await?;
    tx.execute(
        "INSERT INTO ibmish.programs VALUES ($1,$2,'CLLE',$3)",
        &[&lib, &name, &source],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A CL program found through a list of libraries: (library, source).
pub async fn find_cl_program(
    client: &Client,
    libs: &[String],
    name: &str,
) -> Result<Option<(String, String)>> {
    for lib in libs {
        if let Some(r) = client
            .query_opt(
                "SELECT source FROM ibmish.programs WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            return Ok(Some((lib.clone(), r.get(0))));
        }
    }
    Ok(None)
}

/// Is there an object of this type in one of these libraries (CHKOBJ)?
/// Libraries themselves are *LIB objects in QSYS.
pub async fn object_exists(
    client: &Client,
    libs: &[String],
    name: &str,
    objtype: &str,
) -> Result<bool> {
    let libs: Vec<String> = if objtype == "*LIB" {
        vec!["QSYS".into()]
    } else {
        libs.to_vec()
    };
    Ok(client
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library = ANY($1) AND name=$2 AND objtype=$3",
            &[&libs, &name, &objtype],
        )
        .await?
        .is_some())
}

/// All libraries, by name.
pub async fn list_libraries(client: &Client) -> Result<Vec<String>> {
    Ok(client
        .query(
            "SELECT name FROM ibmish.objects WHERE library='QSYS' AND objtype='*LIB' ORDER BY name",
            &[],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
}

/// Does an object name match a name, a generic name (`CUST*`) or *ALL?
pub fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern {
        "*ALL" => true,
        p => match p.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == p,
        },
    }
}

/// The records of a physical file in arrival order (relative record
/// number), each field as text (numbers as decimal strings). At most
/// `limit` records.
pub async fn member_records(
    client: &Client,
    f: &FileRef,
    limit: i64,
) -> Result<Vec<(i64, Vec<String>)>> {
    let cols: Vec<String> = f
        .file
        .fields
        .iter()
        .map(|fd| format!("COALESCE(t.{}::text, '')", ident(&fd.name)))
        .collect();
    let sql = format!(
        "SELECT t.rrn, {} FROM {} AS t ORDER BY t.rrn LIMIT $1",
        if cols.is_empty() {
            "''".to_string()
        } else {
            cols.join(", ")
        },
        f.table()
    );
    Ok(client
        .query(&sql, &[&limit])
        .await?
        .into_iter()
        .map(|r| {
            let vals = (1..=f.file.fields.len())
                .map(|i| r.get::<_, String>(i))
                .collect();
            (r.get::<_, i64>(0), vals)
        })
        .collect())
}

pub async fn create_library(client: &Client, library: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    client
        .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&lib)))
        .await?;
    // The SQL role may use the library (when SQL is set up).
    let _ = crate::sql::grant_library(client, &lib).await;
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
        let desc = pf.is_descending(k);
        tx.execute(
            "INSERT INTO ibmish.file_keys (library, file, seq, field, descending) VALUES ($1,$2,$3,$4,$5)",
            &[&lib, &name, &seq, k, &desc],
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
            access_path(pf)
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

/// Index columns for a file's access path: its keys (DESC where the key
/// is descending), then arrival order.
fn access_path(f: &PhysicalFile) -> String {
    f.keys
        .iter()
        .map(|k| {
            if f.is_descending(k) {
                format!("{} DESC", ident(k))
            } else {
                ident(k)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A select/omit constant as an SQL literal for a field. Character fields
/// take quoted constants; numeric fields take numbers.
fn so_literal(field: &FieldDef, v: &str) -> Result<String> {
    if field.data_type.is_numeric() {
        let ok = {
            let t = v.trim_start_matches(['+', '-']);
            let (i, f) = t.split_once('.').unwrap_or((t, ""));
            !t.is_empty()
                && i.chars().all(|c| c.is_ascii_digit())
                && f.chars().all(|c| c.is_ascii_digit())
        };
        if !ok {
            bail!(
                "select/omit value {v} for numeric field {} is not a number",
                field.name
            );
        }
        Ok(format!("{v}::numeric"))
    } else {
        let Some(inner) = v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) else {
            bail!(
                "select/omit value {v} for character field {} must be in apostrophes",
                field.name
            );
        };
        // DDS doubles apostrophes inside constants, as SQL does.
        Ok(format!("'{inner}'"))
    }
}

/// The select/omit rules as an SQL condition: the first rule whose
/// conditions all hold decides; when none does, the record is omitted if
/// the last rule is a select and selected if it is an omit.
/// Columns are written as `{prefix}"name"` (`t.` in the view, nothing in
/// a partial index).
pub fn selection_sql(lf: &LogicalFile, fields: &[FieldDef], prefix: &str) -> Result<String> {
    if lf.select_omit.is_empty() {
        return Ok("TRUE".into());
    }
    let mut out = String::from("CASE");
    for rule in &lf.select_omit {
        let mut conds = Vec::new();
        for (name, test) in &rule.conds {
            let f = fields.iter().find(|f| &f.name == name).ok_or_else(|| {
                anyhow::anyhow!(
                    "select/omit field {name} is not in physical file {}",
                    lf.based_on
                )
            })?;
            let col = format!("{prefix}{}", ident(name));
            conds.push(match test {
                Test::Comp(op, v) => {
                    let op = match op.as_str() {
                        "EQ" => "=",
                        "NE" => "<>",
                        "LT" => "<",
                        "NL" | "GE" => ">=",
                        "GT" => ">",
                        "NG" | "LE" => "<=",
                        other => bail!("COMP operator {other} is not valid"),
                    };
                    format!("{col} {op} {}", so_literal(f, v)?)
                }
                Test::Values(vs) => format!(
                    "{col} IN ({})",
                    vs.iter()
                        .map(|v| so_literal(f, v))
                        .collect::<Result<Vec<_>>>()?
                        .join(", ")
                ),
                Test::Range(lo, hi) => format!(
                    "{col} BETWEEN {} AND {}",
                    so_literal(f, lo)?,
                    so_literal(f, hi)?
                ),
            });
        }
        let when = if conds.is_empty() {
            "TRUE".to_string()
        } else {
            conds.join(" AND ")
        };
        out.push_str(&format!(" WHEN {when} THEN {}", rule.select));
    }
    let last_select = lf.select_omit.last().map(|r| r.select).unwrap_or(true);
    out.push_str(&format!(" ELSE {} END", !last_select));
    Ok(out)
}

/// CRTLF: a logical file is a view over its physical file's table (the
/// listed fields and rrn, filtered by select/omit), with its own index on
/// the physical file for its key order, and a partial unique index when it
/// is UNIQUE.
pub async fn create_logical_file(
    client: &mut Client,
    libl: &[String],
    library: &str,
    lf: &LogicalFile,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", &lf.name)?;
    let pf = resolve_file(client, libl, &lf.based_on).await?;
    if client
        .query_opt(
            "SELECT 1 FROM ibmish.logical_files WHERE library=$1 AND name=$2",
            &[&pf.library, &pf.file.name],
        )
        .await?
        .is_some()
    {
        bail!(
            "PFILE({}) is a logical file; a logical file must be built over a physical file.",
            lf.based_on
        );
    }
    let fields: Vec<FieldDef> = if lf.fields.is_empty() {
        pf.file.fields.clone()
    } else {
        lf.fields
            .iter()
            .map(|n| {
                pf.file
                    .fields
                    .iter()
                    .find(|f| &f.name == n)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("field {n} is not in physical file {}", pf.file.name)
                    })
            })
            .collect::<Result<_>>()?
    };
    for k in &lf.keys {
        if !fields.iter().any(|f| &f.name == k) {
            bail!(
                "key field {k} is not a field of record format {}",
                lf.format
            );
        }
    }
    let selection = selection_sql(lf, &pf.file.fields, "t.")?;
    let index_selection = selection_sql(lf, &pf.file.fields, "")?;
    let as_pf = PhysicalFile {
        name: name.clone(),
        format: lf.format.clone(),
        text: lf.text.clone(),
        unique: lf.unique,
        fields: fields.clone(),
        keys: lf.keys.clone(),
        descending: lf.descending.clone(),
    };

    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?
        .is_some()
    {
        bail!("CPF7302: file {name} already exists in library {lib}");
    }
    if tx
        .query_opt(
            "SELECT 1 FROM ibmish.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: library {lib} not found");
    }
    tx.execute(
        "INSERT INTO ibmish.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','LF',$3)",
        &[&lib, &name, &lf.text],
    )
    .await?;
    tx.execute(
        "INSERT INTO ibmish.files (library, name, format, is_unique) VALUES ($1,$2,$3,$4)",
        &[&lib, &name, &lf.format, &lf.unique],
    )
    .await?;
    for (i, f) in fields.iter().enumerate() {
        let (seq, len, dec) = (i as i32, f.length as i32, f.decimals as i32);
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
    for (i, k) in lf.keys.iter().enumerate() {
        let (seq, desc) = (i as i32, lf.descending.contains(k));
        tx.execute(
            "INSERT INTO ibmish.file_keys (library, file, seq, field, descending) VALUES ($1,$2,$3,$4,$5)",
            &[&lib, &name, &seq, k, &desc],
        )
        .await?;
    }
    tx.execute(
        "INSERT INTO ibmish.logical_files (library, name, pf_library, pf_name, selection) VALUES ($1,$2,$3,$4,$5)",
        &[&lib, &name, &pf.library, &pf.file.name, &index_selection],
    )
    .await?;
    let cols: Vec<String> = fields
        .iter()
        .map(|f| format!("t.{}", ident(&f.name)))
        .collect();
    let view = format!("{}.{}", ident(&lib), ident(&name));
    tx.batch_execute(&format!(
        "CREATE VIEW {view} AS SELECT t.rrn, {} FROM {} AS t WHERE {selection}",
        cols.join(", "),
        pf.table()
    ))
    .await?;
    if !lf.keys.is_empty() {
        // The index lives with the physical file's table; its name says
        // which logical file it serves.
        let base = pf.table();
        let where_ = if index_selection == "TRUE" {
            String::new()
        } else {
            format!(" WHERE {index_selection}")
        };
        tx.batch_execute(&format!(
            "CREATE INDEX {} ON {base} ({}, rrn){where_}",
            ident(&format!("{lib}_{name}_key")),
            access_path(&as_pf)
        ))
        .await?;
        if lf.unique {
            let keycols: Vec<String> = lf.keys.iter().map(|k| ident(k)).collect();
            tx.batch_execute(&format!(
                "CREATE UNIQUE INDEX {} ON {base} ({}){where_}",
                ident(&format!("{lib}_{name}_ukey")),
                keycols.join(", ")
            ))
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
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
        let key_rows = client
            .query(
                "SELECT field, descending FROM ibmish.file_keys WHERE library=$1 AND file=$2 ORDER BY seq",
                &[lib, &name],
            )
            .await?;
        let keys = key_rows.iter().map(|r| r.get::<_, String>(0)).collect();
        let descending = key_rows
            .iter()
            .filter(|r| r.get::<_, bool>(1))
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
                descending,
            },
        });
    }
    bail!(
        "CPF9812: file {name} not found in library list {}",
        search.join(" ")
    )
}
