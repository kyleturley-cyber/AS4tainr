//! Object catalog. Libraries map to PostgreSQL schemas; a physical file's
//! data lives in a table of that schema, and its description (fields, keys)
//! lives in the `as4tainr` catalog schema.

use crate::dds::{DataType, FieldDef, LogicalFile, PhysicalFile, Test};
use anyhow::{bail, Context, Result};
use tokio_postgres::Client;

const CATALOG_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS as4tainr;
CREATE TABLE IF NOT EXISTS as4tainr.objects (
    library   text NOT NULL,
    name      text NOT NULL,
    objtype   text NOT NULL,
    attribute text NOT NULL DEFAULT '',
    text      text NOT NULL DEFAULT '',
    created   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, name, objtype)
);
CREATE TABLE IF NOT EXISTS as4tainr.files (
    library   text NOT NULL,
    name      text NOT NULL,
    format    text NOT NULL,
    is_unique boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.file_fields (
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
CREATE TABLE IF NOT EXISTS as4tainr.display_files (
    library text NOT NULL,
    name    text NOT NULL,
    source  text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.file_keys (
    library text NOT NULL,
    file    text NOT NULL,
    seq     int  NOT NULL,
    field   text NOT NULL,
    PRIMARY KEY (library, file, seq)
);
-- Constraints made by SQL (PRIMARY KEY, UNIQUE, CHECK, FOREIGN KEY):
-- the PostgreSQL index or constraint is "{file}_{name}".
CREATE TABLE IF NOT EXISTS as4tainr.constraints (
    library    text NOT NULL,
    file       text NOT NULL,
    name       text NOT NULL,
    kind       text NOT NULL,
    created    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, file, name)
);
-- SQL triggers (CREATE TRIGGER), for QSYS2.SYSTRIGGERS.
CREATE TABLE IF NOT EXISTS as4tainr.sql_triggers (
    library    text NOT NULL,
    name       text NOT NULL,
    file_lib   text NOT NULL,
    file       text NOT NULL,
    timing     text NOT NULL,
    events     text NOT NULL,
    each_row   boolean NOT NULL,
    old_row    text,
    new_row    text,
    condition  text,
    body       text NOT NULL,
    definer    text NOT NULL,
    created    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.sequences (
    library    text NOT NULL,
    name       text NOT NULL,
    data_type  text NOT NULL,
    precision  integer NOT NULL DEFAULT 0,
    cache      bigint,
    ordered    boolean NOT NULL DEFAULT false,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.programs (
    library   text NOT NULL,
    name      text NOT NULL,
    attribute text NOT NULL,
    source    text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.logical_files (
    library    text NOT NULL,
    name       text NOT NULL,
    pf_library text NOT NULL,
    pf_name    text NOT NULL,
    -- the select/omit rules as SQL, for display
    selection  text NOT NULL DEFAULT 'TRUE',
    PRIMARY KEY (library, name)
);
-- Columns added after the first release. ALTER TABLE locks the table even
-- when the column exists, so it only runs when the column is missing
-- (other sessions may be using the catalog).
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='file_keys' AND column_name='descending') THEN
    ALTER TABLE as4tainr.file_keys ADD COLUMN descending boolean NOT NULL DEFAULT false;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='file_fields' AND column_name='flt_double') THEN
    ALTER TABLE as4tainr.file_fields ADD COLUMN IF NOT EXISTS flt_double boolean NOT NULL DEFAULT false;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='file_fields' AND column_name='identity') THEN
    ALTER TABLE as4tainr.file_fields ADD COLUMN IF NOT EXISTS identity text;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='files' AND column_name='view') THEN
    ALTER TABLE as4tainr.files ADD COLUMN IF NOT EXISTS view text;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='files' AND column_name='view_def') THEN
    ALTER TABLE as4tainr.files ADD COLUMN IF NOT EXISTS view_def text;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='file_fields' AND column_name='alias') THEN
    ALTER TABLE as4tainr.file_fields ADD COLUMN IF NOT EXISTS alias text;
    ALTER TABLE as4tainr.files ADD COLUMN IF NOT EXISTS sql_name text;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='programs' AND column_name='options') THEN
    ALTER TABLE as4tainr.programs ADD COLUMN IF NOT EXISTS options text NOT NULL DEFAULT '';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='programs' AND column_name='src_file') THEN
    ALTER TABLE as4tainr.programs ADD COLUMN IF NOT EXISTS src_file text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS src_lib text NOT NULL DEFAULT '',
      ADD COLUMN IF NOT EXISTS src_mbr text NOT NULL DEFAULT '';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='file_fields' AND column_name='dft') THEN
    ALTER TABLE as4tainr.file_fields
      ADD COLUMN IF NOT EXISTS varlen boolean NOT NULL DEFAULT false,
      ADD COLUMN IF NOT EXISTS null_capable boolean NOT NULL DEFAULT false,
      ADD COLUMN IF NOT EXISTS dft text;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='files' AND column_name='sql_type') THEN
    -- TABLE or INDEX for files made by SQL CREATE TABLE / CREATE INDEX
    -- (views are files with `view`); null for files made from DDS.
    ALTER TABLE as4tainr.files ADD COLUMN IF NOT EXISTS sql_type text;
  END IF;
END $$;
"#;

/// Quote an SQL identifier. Object names are validated before use, but we
/// still quote everything that reaches dynamic SQL.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", pg_name(s).replace('"', "\"\""))
}

/// PostgreSQL's system column names. A field can have one of these names
/// on IBM i (CTID: a contact ID), so in PostgreSQL it is stored with a `~`
/// in front, a character IBM i names cannot contain.
const PG_SYSTEM_COLUMNS: &[&str] = &["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid", "oid"];

/// The name as4tainr uses in PostgreSQL for an IBM i name.
pub fn pg_name(s: &str) -> String {
    let l = s.to_ascii_lowercase();
    if PG_SYSTEM_COLUMNS.contains(&l.as_str()) {
        format!("~{l}")
    } else {
        l
    }
}

/// The IBM i name of a PostgreSQL column (upper case).
pub fn ibm_name(pg: &str) -> String {
    pg.trim_start_matches('~').to_ascii_uppercase()
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
        match self.file.view {
            // An SQL view whose rows have no record number of their own:
            // they are numbered in the order the view returns them.
            Some(false) => format!(
                "(SELECT row_number() OVER () AS rrn, v.* FROM {}.{} AS v)",
                ident(&self.library),
                ident(&self.file.name)
            ),
            _ => format!("{}.{}", ident(&self.library), ident(&self.file.name)),
        }
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

/// Advisory lock key that serializes catalog creation ("IBMISH" in ASCII: the
/// project's earlier name; the value must not change).
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
            .batch_execute(crate::authority::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::ile::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::dtaara::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::cmd::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::prtf::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::msgf::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::sqlpl::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::menus::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::source::DDL)
            .await
            .context("creating catalog")?;
        crate::ifs::init(client)
            .await
            .context("creating the integrated file system")?;
        client
            .batch_execute(crate::jobs::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::spool::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::journal::DDL)
            .await
            .context("creating catalog")?;
        client
            .batch_execute(crate::pftrg::DDL)
            .await
            .context("creating catalog")?;
        for (lib, text) in SYSTEM_LIBRARIES {
            create_library(client, lib, text).await?;
            // Catalogs made by older versions gave every one the same text.
            client
                .execute(
                    "UPDATE as4tainr.objects SET text = $2
                     WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB' AND text = 'System library'",
                    &[lib, text],
                )
                .await?;
        }
        crate::users::init(client).await?;
        // SQL runs as a restricted role. A database user who may not
        // create roles still gets a working system, without SQL.
        if let Err(e) = crate::sql::init(client).await {
            eprintln!("as4tainr: SQL is not available: {e:#}");
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
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
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
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_some())
}

pub async fn library_text(client: &Client, library: &str) -> Result<String> {
    Ok(client
        .query_opt(
            "SELECT text FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
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
            "SELECT name, objtype, attribute, text FROM as4tainr.objects WHERE library=$1 ORDER BY name, objtype",
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
    create_program(client, library, name, "CLLE", text, source, replace).await
}

/// Keep a program's source as a *PGM object with this attribute (CLLE,
/// CLP, RPGLE): what CRTBNDCL, CRTCLPGM and CRTBNDRPG create.
pub async fn create_program(
    client: &mut Client,
    library: &str,
    name: &str,
    attribute: &str,
    text: &str,
    source: &str,
    replace: bool,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("program", name)?;
    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} not found.");
    }
    let exists = tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
            &[&lib, &name],
        )
        .await?
        .is_some();
    if exists && !replace {
        bail!("Program {name} in library {lib} already exists.");
    }
    tx.execute(
        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*PGM'",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "DELETE FROM as4tainr.programs WHERE library=$1 AND name=$2",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*PGM',$4,$3)",
        &[&lib, &name, &text, &attribute],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.programs (library, name, attribute, source) VALUES ($1,$2,$4,$3)",
        &[&lib, &name, &source, &attribute],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Record a program's compile options (see `Program::options`).
/// The source member a program was created from.
pub async fn set_program_source(
    client: &Client,
    library: &str,
    name: &str,
    member: (&str, &str, &str),
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.programs SET src_file=$3, src_lib=$4, src_mbr=$5 WHERE library=$1 AND name=$2",
            &[&library, &name, &member.0, &member.1, &member.2],
        )
        .await?;
    Ok(())
}

pub async fn set_program_options(
    client: &Client,
    library: &str,
    name: &str,
    options: &str,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.programs SET options=$3 WHERE library=$1 AND name=$2",
            &[&library, &name, &options],
        )
        .await?;
    Ok(())
}

/// A CL program found through a list of libraries: (library, source).
pub async fn find_cl_program(
    client: &Client,
    libs: &[String],
    name: &str,
) -> Result<Option<(String, String)>> {
    Ok(find_program(client, libs, name)
        .await?
        .filter(|p| p.attribute == "CLLE" || p.attribute == "CLP")
        .map(|p| (p.library, p.source)))
}

/// A program object (*PGM) kept by as4tainr.
#[derive(Debug, Clone)]
pub struct Program {
    pub library: String,
    pub name: String,
    /// CLLE, CLP or RPGLE.
    pub attribute: String,
    pub source: String,
    /// Compile options given on the create command, as control keywords
    /// (e.g. `ALWNULL(*USRCTL)`); the source's own control keywords win.
    pub options: String,
    /// How its modules are bound (CRTPGM, binding directories).
    pub binding: Option<crate::ile::Binding>,
}

/// The first program of this name in a list of libraries.
pub async fn find_program(client: &Client, libs: &[String], name: &str) -> Result<Option<Program>> {
    let name = name.to_ascii_uppercase();
    for lib in libs {
        if let Some(r) = client
            .query_opt(
                "SELECT attribute, source, options, binding FROM as4tainr.programs WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            return Ok(Some(Program {
                library: lib.clone(),
                name,
                attribute: r.get(0),
                source: r.get(1),
                options: r.get(2),
                binding: r
                    .get::<_, Option<&str>>(3)
                    .and_then(|b| serde_json::from_str(b).ok()),
            }));
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
            "SELECT 1 FROM as4tainr.objects WHERE library = ANY($1) AND name=$2 AND objtype=$3",
            &[&libs, &name, &objtype],
        )
        .await?
        .is_some())
}

/// All libraries, by name.
pub async fn list_libraries(client: &Client) -> Result<Vec<String>> {
    Ok(client
        .query(
            "SELECT name FROM as4tainr.objects WHERE library='QSYS' AND objtype='*LIB' ORDER BY name",
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
        .map(|fd| {
            let col = format!("t.{}", fd.column());
            // Character values without their trailing blanks; dates and
            // times in their external form; null shown as empty.
            if fd.data_type.is_datetime() {
                let mut plain = fd.clone();
                plain.null_capable = false;
                format!("COALESCE({}, '')", plain.read_expr(&col))
            } else {
                format!("COALESCE({col}::text, '')")
            }
        })
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
            "INSERT INTO as4tainr.objects (library, name, objtype, text) VALUES ('QSYS', $1, '*LIB', $2)
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
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?;
    if exists.is_some() {
        bail!("CPF7302: file {name} already exists in library {lib}");
    }
    let lib_exists = tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?;
    if lib_exists.is_none() {
        bail!("CPF2110: library {lib} not found");
    }

    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','PF',$3)",
        &[&lib, &name, &pf.text],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.files (library, name, format, is_unique) VALUES ($1,$2,$3,$4)",
        &[&lib, &name, &pf.format, &pf.unique],
    )
    .await?;
    for (i, f) in pf.fields.iter().enumerate() {
        insert_field(&tx, &lib, &name, i as i32, f).await?;
    }
    for (i, k) in pf.keys.iter().enumerate() {
        let seq = i as i32;
        let desc = pf.is_descending(k);
        tx.execute(
            "INSERT INTO as4tainr.file_keys (library, file, seq, field, descending) VALUES ($1,$2,$3,$4,$5)",
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
        cols.push(format!(
            "{} {}{}{}{}",
            f.column(),
            f.sql_type(),
            coll,
            if f.null_capable { "" } else { " NOT NULL" },
            match &f.identity {
                Some(i) => i.sql(),
                None => f.default_clause(),
            }
        ));
    }
    let table = format!("{}.{}", ident(&lib), ident(&name));
    tx.batch_execute(&format!("CREATE TABLE {table} ({})", cols.join(", ")))
        .await?;
    if !pf.keys.is_empty() {
        let keycols: Vec<String> = pf.keys.iter().map(|k| key_column(&pf.fields, k)).collect();
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
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} not found.");
    }
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?
        .is_some()
    {
        bail!("CPF7302: File {name} already exists in library {lib}.");
    }
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','DSPF','')",
        &[&lib, &name],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.display_files VALUES ($1,$2,$3)",
        &[&lib, &name, &source],
    )
    .await?;
    tx.commit().await?;
    Ok(parsed)
}

/// CRTDSPF REPLACE(*YES): a display file that already exists gets the new
/// source (IBM i moves the old object to QRPLOBJ; as4tainr keeps no copy).
/// Any other kind of file with the name is still CPF7302.
pub async fn replace_display_file(
    client: &mut Client,
    lib: &str,
    name: &str,
    source: &str,
) -> Result<crate::dspf::DisplayFile> {
    let lib = validate_name("library", lib)?;
    let name = validate_name("file", name)?;
    let parsed = crate::dspf::parse(&name, source)?;
    let n = client
        .execute(
            "UPDATE as4tainr.display_files SET source=$3 WHERE library=$1 AND name=$2",
            &[&lib, &name, &source],
        )
        .await?;
    if n == 0 {
        return create_display_file(client, &lib, &name, source).await;
    }
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
                "SELECT source FROM as4tainr.display_files WHERE library=$1 AND name=$2",
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

/// The column of a key field (by system name).
fn key_column(fields: &[FieldDef], k: &str) -> String {
    fields
        .iter()
        .find(|f| f.name == k)
        .map(|f| f.column())
        .unwrap_or_else(|| ident(k))
}

/// Index columns for a file's access path: its keys (DESC where the key
/// is descending), then arrival order.
fn access_path(f: &PhysicalFile) -> String {
    f.keys
        .iter()
        .map(|k| {
            if f.is_descending(k) {
                format!("{} DESC", key_column(&f.fields, k))
            } else {
                key_column(&f.fields, k)
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
            let col = format!("{prefix}{}", f.column());
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
            "SELECT 1 FROM as4tainr.logical_files WHERE library=$1 AND name=$2",
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
        view: None,
    };

    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?
        .is_some()
    {
        bail!("CPF7302: file {name} already exists in library {lib}");
    }
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: library {lib} not found");
    }
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','LF',$3)",
        &[&lib, &name, &lf.text],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.files (library, name, format, is_unique) VALUES ($1,$2,$3,$4)",
        &[&lib, &name, &lf.format, &lf.unique],
    )
    .await?;
    for (i, f) in fields.iter().enumerate() {
        insert_field(&tx, &lib, &name, i as i32, f).await?;
    }
    for (i, k) in lf.keys.iter().enumerate() {
        let (seq, desc) = (i as i32, lf.descending.contains(k));
        tx.execute(
            "INSERT INTO as4tainr.file_keys (library, file, seq, field, descending) VALUES ($1,$2,$3,$4,$5)",
            &[&lib, &name, &seq, k, &desc],
        )
        .await?;
    }
    tx.execute(
        "INSERT INTO as4tainr.logical_files (library, name, pf_library, pf_name, selection) VALUES ($1,$2,$3,$4,$5)",
        &[&lib, &name, &pf.library, &pf.file.name, &index_selection],
    )
    .await?;
    let cols: Vec<String> = fields.iter().map(|f| format!("t.{}", f.column())).collect();
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
            let keycols: Vec<String> = lf.keys.iter().map(|k| key_column(&fields, k)).collect();
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

/// The physical file whose records a file holds: the file itself, or for
/// a logical file the physical file it is built over.
pub async fn physical_file_of(
    client: &Client,
    library: &str,
    name: &str,
) -> Result<(String, String)> {
    Ok(
        match client
            .query_opt(
                "SELECT pf_library, pf_name FROM as4tainr.logical_files WHERE library=$1 AND name=$2",
                &[&library, &name],
            )
            .await?
        {
            Some(r) => (r.get(0), r.get(1)),
            None => (library.to_string(), name.to_string()),
        },
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
                "SELECT format, is_unique, (SELECT text FROM as4tainr.objects o
                    WHERE o.library=f.library AND o.name=f.name AND o.objtype='*FILE'), view
                 FROM as4tainr.files f WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?;
        let Some(row) = row else { continue };
        let fields = client
            .query(
                "SELECT name, data_type, length, decimals, text, varlen, null_capable, dft, alias, identity, flt_double FROM as4tainr.file_fields
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
                    varlen: r.get(5),
                    null_capable: r.get(6),
                    default: r
                        .get::<_, Option<&str>>(7)
                        .and_then(|d| serde_json::from_str(d).ok()),
                    alias: r.get(8),
                    identity: r
                        .get::<_, Option<&str>>(9)
                        .and_then(|d| serde_json::from_str(d).ok()),
                    double: r.get(10),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let key_rows = client
            .query(
                "SELECT field, descending FROM as4tainr.file_keys WHERE library=$1 AND file=$2 ORDER BY seq",
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
                view: row.get::<_, Option<String>>(3).map(|v| v == "RRN"),
            },
        });
    }
    bail!(
        "CPF9812: file {name} not found in library list {}",
        search.join(" ")
    )
}

/// Data definition that changes what the SQL role owns or may use (its
/// views and functions, grants to it, dropping them) takes this lock:
/// PostgreSQL records those dependencies in shared catalog rows, and two
/// such statements at once can fail with "tuple concurrently updated".
pub const ROLE_DDL_XACT_LOCK: &str = "SELECT pg_advisory_xact_lock(1095979828, 1)";
pub const ROLE_DDL_LOCK: &str = "SELECT pg_advisory_lock(1095979828, 1)";
pub const ROLE_DDL_UNLOCK: &str = "SELECT pg_advisory_unlock(1095979828, 1)";

/// Delete a database or display file: its description, its authorities
/// and its data. A physical file with logical files over it is deleted
/// only with `dependents` (SQL DROP TABLE drops the indexes and views
/// over a table; DLTF refuses, CPF3203).
pub async fn delete_file(
    client: &mut Client,
    library: &str,
    name: &str,
    dependents: bool,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    let tx = client.transaction().await?;
    tx.batch_execute(ROLE_DDL_XACT_LOCK).await?;
    let Some(row) = tx
        .query_opt(
            "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&lib, &name],
        )
        .await?
    else {
        bail!("CPF2105: File {name} in library {lib} not found.");
    };
    let attribute: String = row.get(0);
    let is_view = tx
        .query_opt(
            "SELECT 1 FROM as4tainr.files WHERE library=$1 AND name=$2 AND view IS NOT NULL",
            &[&lib, &name],
        )
        .await?
        .is_some();
    let mut over: Vec<(String, String)> = tx
        .query(
            "SELECT library, name FROM as4tainr.logical_files WHERE pf_library=$1 AND pf_name=$2 ORDER BY 1, 2",
            &[&lib, &name],
        )
        .await?
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    // SQL views built over this file (any view in PostgreSQL that uses
    // it, other than the view giving a table its SQL name).
    over.extend(sql_views_over(&tx, &lib, &name).await?);
    if !over.is_empty() && !dependents {
        bail!(
            "CPF3203: File {name} in library {lib} cannot be deleted: logical file {}/{} is built over it.",
            over[0].0,
            over[0].1
        );
    }
    let _ = is_view;
    let mut drop = over;
    drop.push((lib.clone(), name.clone()));
    for (l, n) in &drop {
        let lf = tx
            .query_opt(
                "SELECT pf_library FROM as4tainr.logical_files WHERE library=$1 AND name=$2",
                &[l, n],
            )
            .await?;
        let qualified = format!("{}.{}", ident(l), ident(n));
        let view = tx
            .query_opt(
                "SELECT 1 FROM as4tainr.files WHERE library=$1 AND name=$2 AND view IS NOT NULL",
                &[l, n],
            )
            .await?
            .is_some();
        match (&lf, attribute.as_str()) {
            _ if view => {
                tx.batch_execute(&format!("DROP VIEW IF EXISTS {qualified} CASCADE"))
                    .await?;
            }
            (Some(r), _) => {
                // The view, and the indexes on the physical file's table.
                let pf_lib: String = r.get(0);
                tx.batch_execute(&format!(
                    "DROP VIEW IF EXISTS {qualified};
                     DROP INDEX IF EXISTS {}.{};
                     DROP INDEX IF EXISTS {}.{};",
                    ident(&pf_lib),
                    ident(&format!("{l}_{n}_key")),
                    ident(&pf_lib),
                    ident(&format!("{l}_{n}_ukey")),
                ))
                .await?;
            }
            (None, "DSPF") | (None, "PRTF") => {}
            (None, _) => {
                tx.batch_execute(&format!("DROP TABLE IF EXISTS {qualified} CASCADE"))
                    .await?;
                // A journaled file: journaling ends with it (F EJ).
                if let Some(j) = tx
                    .query_opt(
                        "DELETE FROM as4tainr.journaled_files WHERE library=$1 AND file=$2 RETURNING journal_lib, journal",
                        &[l, n],
                    )
                    .await?
                {
                    let (jl, jn): (String, String) = (j.get(0), j.get(1));
                    let obj = format!("{n:<10}{l:<10}{n:<10}");
                    tx.execute(
                        "SELECT as4tainr_svc.journal_entry($1, $2, 'F', 'EJ', 0, NULL, NULL, $3, '*QDDS')",
                        &[&jl, &jn, &obj],
                    )
                    .await?;
                }
            }
        }
        for table in ["files", "logical_files", "display_files", "printer_files"] {
            tx.execute(
                &format!("DELETE FROM as4tainr.{table} WHERE library=$1 AND name=$2"),
                &[l, n],
            )
            .await?;
        }
        for table in ["file_fields", "file_keys", "constraints"] {
            tx.execute(
                &format!("DELETE FROM as4tainr.{table} WHERE library=$1 AND file=$2"),
                &[l, n],
            )
            .await?;
        }
        tx.execute(
            "DELETE FROM as4tainr.object_auths WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[l, n],
        )
        .await?;
        tx.execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[l, n],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Change a file's text description.
pub async fn set_file_text(client: &Client, library: &str, name: &str, text: &str) -> Result<()> {
    let n = client
        .execute(
            "UPDATE as4tainr.objects SET text=$3 WHERE library=$1 AND name=$2 AND objtype='*FILE'",
            &[&library, &name, &text],
        )
        .await?;
    if n == 0 {
        bail!("SQL0204: {library}/{name} not found.");
    }
    Ok(())
}

/// Change a field's text (in the file and in the logical files that
/// share the field).
pub async fn set_field_text(
    client: &Client,
    library: &str,
    file: &str,
    field: &str,
    text: &str,
) -> Result<()> {
    let n = client
        .execute(
            "UPDATE as4tainr.file_fields SET text=$4 WHERE library=$1 AND file=$2 AND (name=$3 OR alias=$3)",
            &[&library, &file, &field, &text],
        )
        .await?;
    if n == 0 {
        bail!("SQL0205: Column {field} not in table {file} in {library}.");
    }
    Ok(())
}

/// A file's SQL name, when it is not its system name. For a table, SQL
/// statements reach it by that name too: a view of the same records
/// (PostgreSQL views of one table can be changed through).
pub async fn set_sql_name(
    client: &Client,
    library: &str,
    name: &str,
    sql_name: &str,
    table: bool,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.files SET sql_name=$3 WHERE library=$1 AND name=$2",
            &[&library, &name, &sql_name],
        )
        .await?;
    if table {
        client
            .batch_execute(&format!(
                "CREATE VIEW {}.{} AS SELECT * FROM {}.{}",
                ident(library),
                ident(sql_name),
                ident(library),
                ident(name)
            ))
            .await?;
    }
    Ok(())
}

/// Mark a file as made by SQL (`TABLE` or `INDEX`), for the catalog
/// views (QSYS2.SYSTABLES: table type T rather than P).
pub async fn set_sql_type(
    client: &Client,
    library: &str,
    name: &str,
    sql_type: &str,
) -> Result<()> {
    client
        .execute(
            "UPDATE as4tainr.files SET sql_type=$3 WHERE library=$1 AND name=$2",
            &[&library, &name, &sql_type],
        )
        .await?;
    Ok(())
}

/// A file found by its system name or its SQL name (SQL statements).
pub async fn resolve_sql_file(client: &Client, libl: &[String], name: &str) -> Result<FileRef> {
    let (libs, n) = match name.split_once('/') {
        Some((l, n)) => (vec![l.to_ascii_uppercase()], n.to_ascii_uppercase()),
        None => (libl.to_vec(), name.to_ascii_uppercase()),
    };
    for lib in &libs {
        if validate_name("file", &n).is_ok() {
            if let Ok(f) = resolve_file(client, std::slice::from_ref(lib), &n).await {
                return Ok(f);
            }
        }
        if let Some(r) = client
            .query_opt(
                "SELECT name FROM as4tainr.files WHERE library=$1 AND sql_name=$2",
                &[lib, &n],
            )
            .await?
        {
            let sys: String = r.get(0);
            return resolve_file(client, std::slice::from_ref(lib), &sys).await;
        }
    }
    bail!("SQL0204: {n} not found.")
}

/// A unique constraint (SQL PRIMARY KEY or UNIQUE) on a physical file.
/// A constraint made by SQL, for the catalog views (SYSCST...).
pub async fn record_constraint(
    client: &Client,
    library: &str,
    file: &str,
    name: &str,
    kind: &str,
) -> Result<()> {
    client
        .execute(
            "INSERT INTO as4tainr.constraints (library, file, name, kind) VALUES ($1, $2, $3, $4)
             ON CONFLICT (library, file, name) DO UPDATE SET kind = EXCLUDED.kind, created = now()",
            &[&library, &file, &name, &kind],
        )
        .await?;
    Ok(())
}

pub async fn add_unique_constraint(
    client: &Client,
    library: &str,
    file: &str,
    constraint: &str,
    columns: &[String],
) -> Result<()> {
    let fields = client
        .query(
            "SELECT name, alias FROM as4tainr.file_fields WHERE library=$1 AND file=$2",
            &[&library, &file],
        )
        .await?;
    let cols: Vec<String> = columns
        .iter()
        .map(|c| {
            let alias = fields
                .iter()
                .find(|r| r.get::<_, String>(0) == *c)
                .and_then(|r| r.get::<_, Option<String>>(1));
            ident(alias.as_deref().unwrap_or(c))
        })
        .collect();
    client
        .batch_execute(&format!(
            "CREATE UNIQUE INDEX {} ON {}.{} ({})",
            ident(&format!("{file}_{constraint}")),
            ident(library),
            ident(file),
            cols.join(", ")
        ))
        .await?;
    Ok(())
}

/// The SQL views (catalog files with a query) that use a file, found in
/// PostgreSQL's dependencies; views of views are found too, also through
/// the view that gives a table its SQL name.
async fn sql_views_over(
    tx: &tokio_postgres::Transaction<'_>,
    library: &str,
    name: &str,
) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    // PostgreSQL schema and relation names (lower case).
    let mut todo = vec![(library.to_ascii_lowercase(), name.to_ascii_lowercase())];
    while let Some((s, r)) = todo.pop() {
        let rows = tx
            .query(
                "SELECT DISTINCT n.nspname::text, v.relname::text, f.library, f.name
                   FROM pg_depend d
                   JOIN pg_rewrite w ON w.oid = d.objid
                   JOIN pg_class v ON v.oid = w.ev_class
                   JOIN pg_namespace n ON n.oid = v.relnamespace
                   LEFT JOIN as4tainr.files f
                     ON lower(f.library) = n.nspname AND lower(f.name) = v.relname AND f.view IS NOT NULL
                  WHERE d.refobjid = to_regclass(format('%I.%I', $1::text, $2::text))
                    AND v.oid <> d.refobjid",
                &[&s, &r],
            )
            .await?;
        for row in rows {
            let k: (String, String) = (row.get(0), row.get(1));
            if seen.contains(&k) {
                continue;
            }
            seen.push(k.clone());
            todo.push(k);
            if let (Some(l), Some(n)) = (
                row.get::<_, Option<String>>(2),
                row.get::<_, Option<String>>(3),
            ) {
                out.push((l, n));
            }
        }
    }
    Ok(out)
}

/// An SQL view as a file: its description from the PostgreSQL view's
/// columns (types mapped to their DDS equivalents), not keyed.
pub async fn register_view(
    client: &mut Client,
    library: &str,
    name: &str,
    format: &str,
    def: &crate::ddl::ViewDef,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("file", name)?;
    let cols = client
        .query(
            "SELECT column_name::text, data_type::text, character_maximum_length, numeric_precision, numeric_scale
               FROM information_schema.columns
              WHERE table_schema = lower($1) AND table_name = lower($2)
              ORDER BY ordinal_position",
            &[&lib, &name],
        )
        .await?;
    let (fields, has_rrn) = fields_of_columns(&cols)?;
    let tx = client.transaction().await?;
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,'*FILE','LF','')",
        &[&lib, &name],
    )
    .await?;
    let view = if has_rrn { "RRN" } else { "NORRN" };
    tx.execute(
        "INSERT INTO as4tainr.files (library, name, format, is_unique, view, view_def) VALUES ($1,$2,$3,false,$4,$5)",
        &[&lib, &name, &format, &view, &serde_json::to_string(def)?],
    )
    .await?;
    for (i, f) in fields.iter().enumerate() {
        insert_field(&tx, &lib, &name, i as i32, f).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Fields for the columns information_schema describes (a view's, or a
/// query's): PostgreSQL types as the SQL types they stand for. Also
/// whether there is a record number column.
pub fn fields_of_columns(cols: &[tokio_postgres::Row]) -> Result<(Vec<FieldDef>, bool)> {
    let mut fields: Vec<FieldDef> = Vec::new();
    let mut has_rrn = false;
    for c in cols {
        let col: String = c.get(0);
        if col == "rrn" {
            has_rrn = true;
            continue;
        }
        let ty: String = c.get(1);
        let len: Option<i32> = c.get(2);
        let prec: Option<i32> = c.get(3);
        let scale: Option<i32> = c.get(4);
        let (t, l, d, varlen) = match ty.as_str() {
            "character" => (DataType::Char, len.unwrap_or(1) as u32, 0, false),
            "character varying" => (DataType::Char, len.unwrap_or(32740) as u32, 0, true),
            // Text without a length (expressions): the longest VARCHAR.
            "text" => (DataType::Char, 32740, 0, true),
            "smallint" => (DataType::Binary, 4, 0, false),
            "integer" => (DataType::Binary, 9, 0, false),
            "bigint" => (DataType::Binary, 18, 0, false),
            "numeric" => (
                DataType::Packed,
                prec.unwrap_or(31) as u32,
                scale.unwrap_or(0) as u32,
                false,
            ),
            "real" => (DataType::Float, 9, 0, false),
            "double precision" => (DataType::Float, 17, 0, false),
            "bytea" => (DataType::Hex, 32740, 0, true),
            "date" => (DataType::Date, 10, 0, false),
            // Db2 has no time zone types: CURRENT TIME and CURRENT
            // TIMESTAMP are TIME and TIMESTAMP (PostgreSQL's are with time
            // zone).
            "time without time zone" | "time with time zone" => (DataType::Time, 8, 0, false),
            "timestamp without time zone" | "timestamp with time zone" => {
                (DataType::Timestamp, 26, 0, false)
            }
            "boolean" => (DataType::Boolean, 1, 0, false),
            other => bail!("SQL0084: a column of type {other} ({col}) is not supported yet."),
        };
        // A column name that is not a valid system name gets a generated
        // one (Db2 for i: first five characters and a number).
        let upper = col.to_ascii_uppercase();
        let (sys, alias) = if validate_name("column", &upper).is_ok() {
            (upper.clone(), None)
        } else {
            let sys = crate::ddl::generated_name(&upper, &|c| {
                fields.iter().any(|f| f.is_named(c))
                    || cols
                        .iter()
                        .any(|o| o.get::<_, String>(0).eq_ignore_ascii_case(c))
            })?;
            (sys, Some(upper.clone()))
        };
        let mut f = FieldDef::new(&sys, t, l, d);
        f.varlen = varlen;
        f.double = t == DataType::Float && l > 9;
        f.null_capable = true;
        f.alias = alias;
        fields.push(f);
    }
    Ok((fields, has_rrn))
}

/// The SQL views (catalog files) that use a column of a table, also
/// through the view that gives the table its SQL name.
pub async fn views_using_column(
    client: &Client,
    library: &str,
    table: &str,
    field: &FieldDef,
) -> Result<Vec<(String, String)>> {
    let col = crate::catalog::pg_name(field.alias.as_deref().unwrap_or(&field.name));
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut todo = vec![(library.to_ascii_lowercase(), table.to_ascii_lowercase())];
    while let Some((s, r)) = todo.pop() {
        let rows = client
            .query(
                "SELECT DISTINCT n.nspname::text, v.relname::text, f.library, f.name
                   FROM pg_depend d
                   JOIN pg_rewrite w ON w.oid = d.objid
                   JOIN pg_class v ON v.oid = w.ev_class
                   JOIN pg_namespace n ON n.oid = v.relnamespace
                   JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
                   LEFT JOIN as4tainr.files f
                     ON lower(f.library) = n.nspname AND lower(f.name) = v.relname AND f.view IS NOT NULL
                  WHERE d.refobjid = to_regclass(format('%I.%I', $1::text, $2::text))
                    AND a.attname = $3 AND v.oid <> d.refobjid",
                &[&s, &r, &col],
            )
            .await?;
        for row in rows {
            let k: (String, String) = (row.get(0), row.get(1));
            if seen.contains(&k) {
                continue;
            }
            seen.push(k.clone());
            todo.push(k);
            if let (Some(l), Some(n)) = (
                row.get::<_, Option<String>>(2),
                row.get::<_, Option<String>>(3),
            ) {
                out.push((l, n));
            }
        }
    }
    Ok(out)
}

/// The SQL views (catalog files) built over a file, directly or not.
pub async fn views_over(
    client: &mut Client,
    library: &str,
    name: &str,
) -> Result<Vec<(String, String)>> {
    let tx = client.transaction().await?;
    let v = sql_views_over(&tx, library, name).await?;
    tx.commit().await?;
    Ok(v)
}

/// Describe one field of a file in the catalog.
pub async fn insert_field(
    c: &impl tokio_postgres::GenericClient,
    library: &str,
    file: &str,
    seq: i32,
    f: &FieldDef,
) -> Result<()> {
    c.execute(
        "INSERT INTO as4tainr.file_fields (library, file, seq, name, data_type, length, decimals, text, varlen, null_capable, dft, alias, identity, flt_double)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        &[
            &library,
            &file,
            &seq,
            &f.name,
            &f.data_type.code(),
            &(f.length as i32),
            &(f.decimals as i32),
            &f.text,
            &f.varlen,
            &f.null_capable,
            &f.default.as_ref().map(|d| serde_json::to_string(d).unwrap_or_default()),
            &f.alias,
            &f.identity.as_ref().map(|d| serde_json::to_string(d).unwrap_or_default()),
            &f.double,
        ],
    )
    .await?;
    Ok(())
}
