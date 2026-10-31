//! Save files and saving and restoring objects (CL reference: CRTSAVF,
//! CLRSAVF, DSPSAVF, SAVOBJ, SAVLIB, RSTOBJ, RSTLIB; DLTLIB and CLRLIB).
//!
//! A save file (*FILE, attribute SAVF) holds what a save wrote: the
//! objects' descriptions (their catalog rows), the PostgreSQL objects
//! that implement them (tables with their columns, indexes, constraints,
//! triggers, views, sequences and functions, as SQL), and each table's
//! records (PostgreSQL's binary COPY). The whole is JSON, compressed
//! with gzip: as4tainr's own save format, portable between as4tainr
//! systems (CPYTOSTMF and CPYFRMSTMF copy a save file to and from a
//! stream file; `as4tainr get` and `as4tainr put` move it off and on
//! the system). It is not IBM's save format.

use crate::catalog::{self, ident, pg_name};
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Where a saved character column's collation goes: this system's at
/// restore (EBCDIC order, or byte order without ICU; collate.rs).
const CHAR_COLLATION: &str = " COLLATE {{AS4TAINR_CHAR}}";
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.save_files (
    library text NOT NULL,
    name    text NOT NULL,
    data    bytea,
    saved   timestamptz,
    PRIMARY KEY (library, name)
);
ALTER TABLE as4tainr.objects ADD COLUMN IF NOT EXISTS saved timestamptz;
ALTER TABLE as4tainr.objects ADD COLUMN IF NOT EXISTS restored timestamptz;
-- When the object last changed (SAVCHGOBJ, DSPOBJD): made, restored, its
-- description or contents changed (the triggers below; files' records
-- are looked at by SAVCHGOBJ itself, `changed_since`).
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                  WHERE table_schema = 'as4tainr' AND table_name = 'objects' AND column_name = 'changed') THEN
    ALTER TABLE as4tainr.objects ADD COLUMN changed timestamptz;
    UPDATE as4tainr.objects SET changed = coalesce(restored, created);
    ALTER TABLE as4tainr.objects ALTER COLUMN changed SET DEFAULT now();
  END IF;
END $$;
-- A library's last SAVLIB (with UPDHST(*YES)) and where the database's
-- transactions were then; a file's number of records then.
ALTER TABLE as4tainr.objects ADD COLUMN IF NOT EXISTS savlib timestamptz;
ALTER TABLE as4tainr.objects ADD COLUMN IF NOT EXISTS savlib_xid text;
ALTER TABLE as4tainr.objects ADD COLUMN IF NOT EXISTS savlib_rows bigint;
-- Where the transactions were at a time (saves, catalog initialization):
-- records changed after a date, without commit timestamps.
CREATE TABLE IF NOT EXISTS as4tainr.xid_clock (
    at  timestamptz NOT NULL DEFAULT now(),
    xid text NOT NULL
);
CREATE INDEX IF NOT EXISTS xid_clock_at ON as4tainr.xid_clock (at);
CREATE OR REPLACE FUNCTION as4tainr.object_changed() RETURNS trigger LANGUAGE plpgsql AS $f$
BEGIN
  IF TG_TABLE_NAME = 'objects' THEN
    IF NEW.text IS DISTINCT FROM OLD.text OR NEW.attribute IS DISTINCT FROM OLD.attribute
       OR NEW.owner IS DISTINCT FROM OLD.owner THEN
      NEW.changed := now();
    END IF;
    RETURN NEW;
  END IF;
  IF TG_TABLE_NAME = 'source_members' THEN
    UPDATE as4tainr.objects SET changed = now()
     WHERE library = coalesce(NEW.library, OLD.library) AND name = coalesce(NEW.file, OLD.file)
       AND objtype = '*FILE' AND changed IS DISTINCT FROM now();
  ELSE
    UPDATE as4tainr.objects SET changed = now()
     WHERE library = coalesce(NEW.library, OLD.library) AND name = coalesce(NEW.name, OLD.name)
       AND objtype = TG_ARGV[0] AND changed IS DISTINCT FROM now();
  END IF;
  RETURN NULL;
END $f$;
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'as4tainr_changed'
                  AND tgrelid = 'as4tainr.objects'::regclass) THEN
    CREATE TRIGGER as4tainr_changed BEFORE UPDATE ON as4tainr.objects
      FOR EACH ROW EXECUTE FUNCTION as4tainr.object_changed();
    CREATE TRIGGER as4tainr_changed AFTER INSERT OR UPDATE OR DELETE ON as4tainr.source_members
      FOR EACH ROW EXECUTE FUNCTION as4tainr.object_changed();
    CREATE TRIGGER as4tainr_changed AFTER UPDATE ON as4tainr.data_areas
      FOR EACH ROW EXECUTE FUNCTION as4tainr.object_changed('*DTAARA');
    CREATE TRIGGER as4tainr_changed AFTER UPDATE ON as4tainr.user_spaces
      FOR EACH ROW EXECUTE FUNCTION as4tainr.object_changed('*USRSPC');
  END IF;
END $$;
"#;

const FORMAT: &str = "as4tainr save 1";

/// A catalog table that describes objects: the library column, the
/// object's name column, the object types it is for (none: the library
/// itself, saved by SAVLIB only), and other columns that name its
/// library (changed with it when restored to another library).
struct CatTable {
    table: &'static str,
    lib: &'static str,
    name: &'static str,
    types: &'static [&'static str],
    libs: &'static [&'static str],
}

const CATALOG: &[CatTable] = &[
    // A save file's contents (SAVFDTA(*YES), the default of the save
    // commands: CL reference, SAVOBJ SAVFDTA).
    CatTable {
        table: "save_files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "file_fields",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "file_keys",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "logical_files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &["pf_library"],
    },
    CatTable {
        table: "display_files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "printer_files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "source_files",
        lib: "library",
        name: "name",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "source_members",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "constraints",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "file_triggers",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &["trg_lib", "pgm_lib"],
    },
    CatTable {
        table: "sql_triggers",
        lib: "file_lib",
        name: "file",
        types: &["*FILE"],
        libs: &["library"],
    },
    CatTable {
        table: "journaled_files",
        lib: "library",
        name: "file",
        types: &["*FILE"],
        libs: &[],
    },
    CatTable {
        table: "programs",
        lib: "library",
        name: "name",
        types: &["*PGM"],
        libs: &[],
    },
    CatTable {
        table: "service_programs",
        lib: "library",
        name: "name",
        types: &["*SRVPGM"],
        libs: &[],
    },
    CatTable {
        table: "modules",
        lib: "library",
        name: "name",
        types: &["*MODULE"],
        libs: &[],
    },
    CatTable {
        table: "job_descriptions",
        lib: "library",
        name: "name",
        types: &["*JOBD"],
        libs: &[],
    },
    CatTable {
        table: "commands",
        lib: "library",
        name: "name",
        types: &["*CMD"],
        libs: &[],
    },
    // A data queue's description; its entries are not saved (SAVOBJ's
    // QDTA(*NONE), the default).
    CatTable {
        table: "data_queues",
        lib: "library",
        name: "name",
        types: &["*DTAQ"],
        libs: &[],
    },
    // A user space's description and bytes.
    CatTable {
        table: "user_spaces",
        lib: "library",
        name: "name",
        types: &["*USRSPC"],
        libs: &[],
    },
    CatTable {
        table: "output_queues",
        lib: "library",
        name: "name",
        types: &["*OUTQ"],
        libs: &[],
    },
    CatTable {
        table: "data_areas",
        lib: "library",
        name: "name",
        types: &["*DTAARA"],
        libs: &[],
    },
    CatTable {
        table: "sequences",
        lib: "library",
        name: "name",
        types: &["*DTAARA"],
        libs: &[],
    },
    CatTable {
        table: "message_descriptions",
        lib: "library",
        name: "msgf",
        types: &["*MSGF"],
        libs: &[],
    },
    CatTable {
        table: "bnddir_entries",
        lib: "library",
        name: "bnddir",
        types: &["*BNDDIR"],
        libs: &["obj_lib"],
    },
    CatTable {
        table: "menus",
        lib: "library",
        name: "name",
        types: &["*MENU"],
        libs: &[],
    },
    CatTable {
        table: "routines",
        lib: "library",
        name: "name",
        types: &[],
        libs: &[],
    },
];

/// Object types that are not saved (journals and their receivers, save
/// files' data stays in the save file being saved).
const NOT_SAVED: &[&str] = &["*JRN", "*JRNRCV"];

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SaveImage {
    pub format: String,
    pub saved: String,
    pub command: String,
    pub libraries: Vec<SavedLib>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SavedLib {
    pub name: String,
    pub text: String,
    /// Saved by SAVLIB (the library's own catalog rows, its routines).
    pub whole: bool,
    pub objects: Vec<SavedObj>,
    /// Catalog rows by table, as JSON objects.
    pub catalog: Vec<(String, Vec<serde_json::Value>)>,
    pub pg: Vec<PgItem>,
}

/// OMITOBJ and SELECT of the save and restore commands (CL reference,
/// RSTLIB: OMITOBJ elements object (library/object, generic or *ALL)
/// and object type; SELECT elements *INCLUDE or *OMIT, object
/// (library/object), object type, object attribute (*ALL, *BLANK,
/// generic) and file member): an object an *INCLUDE entry selects is
/// saved or restored unless an *OMIT entry or OMITOBJ leaves it out;
/// with no *INCLUDE entry, every object is.
#[derive(Debug, Default)]
struct Filter {
    omit: Vec<(String, String, String)>,
    select: Vec<(bool, String, String, String, String)>,
}

impl Filter {
    fn from_command(cmd: &Command) -> Result<Filter> {
        let mut f = Filter::default();
        let qual = |q: &str| -> (String, String) {
            match q.split_once('/') {
                Some((l, o)) => (l.to_string(), o.to_string()),
                None => ("*ALL".to_string(), q.to_string()),
            }
        };
        for e in cmd.list("OMITOBJ") {
            let w: Vec<String> = e
                .split_whitespace()
                .map(|x| {
                    x.trim_matches(|c| c == '(' || c == ')')
                        .to_ascii_uppercase()
                })
                .filter(|x| !x.is_empty())
                .collect();
            match w.first().map(String::as_str) {
                None | Some("*NONE") => continue,
                Some(q) => {
                    let (l, o) = qual(q);
                    let t = w.get(1).cloned().unwrap_or_else(|| "*ALL".into());
                    f.omit.push((l, o, t));
                }
            }
        }
        for e in cmd.list("SELECT") {
            let w: Vec<String> = e
                .split_whitespace()
                .map(|x| {
                    x.trim_matches(|c| c == '(' || c == ')')
                        .to_ascii_uppercase()
                })
                .filter(|x| !x.is_empty())
                .collect();
            let include = match w.first().map(String::as_str) {
                None => continue,
                Some("*ALL") if w.len() <= 1 => continue,
                Some("*INCLUDE") => true,
                Some("*OMIT") => false,
                Some(other) => {
                    bail!("CPF9898: SELECT({other} ...): *INCLUDE or *OMIT is expected.")
                }
            };
            let (l, o) = qual(w.get(1).map(String::as_str).unwrap_or("*ALL"));
            let t = w.get(2).cloned().unwrap_or_else(|| "*ALL".into());
            let a = w.get(3).cloned().unwrap_or_else(|| "*ALL".into());
            let m = w.get(4).cloned().unwrap_or_else(|| "*ALL".into());
            if m != "*ALL" {
                bail!("CPF9898: SELECT with file member {m}: as4tainr saves and restores whole files (member *ALL).");
            }
            f.select.push((include, l, o, t, a));
        }
        Ok(f)
    }

    fn allows(&self, lib: &str, name: &str, objtype: &str, attr: &str) -> bool {
        let hit = |l: &str, o: &str, t: &str| {
            catalog::name_matches(l, lib)
                && catalog::name_matches(o, name)
                && (t == "*ALL" || t == objtype)
        };
        let attr_ok = |a: &str| match a {
            "*ALL" => true,
            "*BLANK" => attr.trim().is_empty(),
            a => catalog::name_matches(a, attr),
        };
        if self.omit.iter().any(|(l, o, t)| hit(l, o, t)) {
            return false;
        }
        let matches =
            |(_, l, o, t, a): &&(bool, String, String, String, String)| hit(l, o, t) && attr_ok(a);
        if self.select.iter().filter(|e| !e.0).any(|e| matches(&e)) {
            return false;
        }
        let includes: Vec<_> = self.select.iter().filter(|e| e.0).collect();
        includes.is_empty() || includes.iter().any(matches)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedObj {
    pub name: String,
    pub objtype: String,
    pub attribute: String,
    pub text: String,
    pub owner: String,
    pub public_aut: String,
    pub created: String,
    /// Private authorities: (user, authority).
    pub auths: Vec<(String, String)>,
    /// The rest of the object's description (QLICOBJD, QUSROBJD OBJD0300
    /// and OBJD0400): the user-defined attribute and where its source
    /// was (file, library, member, when it changed). Saves made before
    /// they were kept have none.
    #[serde(default)]
    pub user_attr: String,
    #[serde(default)]
    pub source: (String, String, String, String),
}

/// A PostgreSQL object, as the SQL that makes it (the library's schema
/// written as SCHEMA), and the object it belongs to.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PgItem {
    /// sequence, table, index, constraint, fkey, view, function, trigger
    pub kind: String,
    pub name: String,
    pub sql: String,
    /// ALTER ... OWNER TO, for an owner other than the catalog's user.
    pub owner: Option<String>,
    /// The as4tainr object: (name, type); None for the library's own.
    pub object: Option<(String, String)>,
    /// A table's records: binary COPY, base64.
    pub data: Option<String>,
    /// Sequences: (column or "", last value, is called).
    pub seq: Vec<(String, i64, bool)>,
}

/// The schema in saved SQL.
const SCHEMA: &str = "\u{1}SCHEMA\u{1}";

/// Saved SQL with the schema written as SCHEMA: `lib.` and `"lib".`.
fn unschema(sql: &str, lib: &str) -> String {
    let l = pg_name(lib);
    let quoted = format!("\"{l}\".");
    let mut out = sql.replace(&quoted, &format!("{SCHEMA}."));
    let plain = format!("{l}.");
    let mut res = String::new();
    let mut rest = out.as_str();
    while let Some(k) = rest.find(&plain) {
        let before = res.chars().last().or_else(|| rest[..k].chars().last());
        let prev = if k > 0 {
            rest[..k].chars().last()
        } else {
            before
        };
        let ok = prev
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '"' || c == '$' || c == '.'));
        res.push_str(&rest[..k]);
        if ok {
            res.push_str(SCHEMA);
            res.push('.');
        } else {
            res.push_str(&plain);
        }
        rest = &rest[k + plain.len()..];
    }
    res.push_str(rest);
    out = res;
    out
}

fn reschema(sql: &str, lib: &str) -> String {
    sql.replace(SCHEMA, &ident(lib))
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The names of things a save takes: None for all of the library.
struct Pick {
    objects: Option<BTreeSet<(String, String)>>,
}

impl Pick {
    fn has(&self, name: &str, objtype: &str) -> bool {
        match &self.objects {
            None => true,
            Some(s) => s.contains(&(name.to_string(), objtype.to_string())),
        }
    }
}

/// Save a library's objects (all, or those picked).
async fn save_library(
    client: &mut Client,
    lib: &str,
    pick: &Pick,
    whole: bool,
) -> Result<SavedLib> {
    let text = catalog::library_text(client, lib).await.unwrap_or_default();
    let tx = client
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    tx.batch_execute("SET LOCAL search_path = pg_catalog")
        .await?;
    let rows = tx
        .query(
            "SELECT o.name, o.objtype, o.attribute, o.text, o.owner, o.public_aut, o.created::text,
                    o.user_attr, o.src_file, o.src_lib, o.src_mbr, o.src_changed
               FROM as4tainr.objects o WHERE o.library = $1 ORDER BY o.created, o.name",
            &[&lib],
        )
        .await?;
    let mut objects = Vec::new();
    for r in &rows {
        let (name, objtype, attribute): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        if NOT_SAVED.contains(&objtype.as_str()) || !pick.has(&name, &objtype) {
            continue;
        }
        let auths = tx
            .query(
                "SELECT user_name, aut FROM as4tainr.object_auths WHERE library=$1 AND name=$2 AND objtype=$3 ORDER BY 1",
                &[&lib, &name, &objtype],
            )
            .await?
            .iter()
            .map(|a| (a.get(0), a.get(1)))
            .collect();
        objects.push(SavedObj {
            name,
            objtype,
            attribute,
            text: r.get(3),
            owner: r.get(4),
            public_aut: r.get(5),
            created: r.get(6),
            auths,
            user_attr: r.get(7),
            source: (r.get(8), r.get(9), r.get(10), r.get(11)),
        });
    }
    let names_of = |t: &str| -> Vec<String> {
        objects
            .iter()
            .filter(|o| o.objtype == t)
            .map(|o| o.name.clone())
            .collect()
    };
    // Catalog rows.
    let mut cat = Vec::new();
    for c in CATALOG {
        if c.types.is_empty() && !whole {
            continue;
        }
        let names: Vec<String> = c.types.iter().flat_map(|t| names_of(t)).collect();
        let sql = format!(
            "SELECT to_jsonb(t)::text FROM as4tainr.{} t WHERE {} = $1{}",
            c.table,
            c.lib,
            if c.types.is_empty() {
                String::new()
            } else {
                format!(" AND {} = ANY($2)", c.name)
            }
        );
        let rows = if c.types.is_empty() {
            tx.query(&sql, &[&lib]).await?
        } else {
            tx.query(&sql, &[&lib, &names]).await?
        };
        let vals: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| serde_json::from_str(r.get::<_, &str>(0)))
            .collect::<std::result::Result<_, _>>()?;
        if !vals.is_empty() {
            cat.push((c.table.to_string(), vals));
        }
    }
    // PostgreSQL relations and which object each is part of.
    let mut owner_of: BTreeMap<String, (String, String)> = BTreeMap::new();
    for o in &objects {
        match o.objtype.as_str() {
            "*FILE" | "*DTAARA" => {
                owner_of.insert(pg_name(&o.name), (o.name.clone(), o.objtype.clone()));
            }
            _ => {}
        }
    }
    // A table's SQL name is a view of that name.
    for r in tx
        .query(
            "SELECT name, sql_name FROM as4tainr.files WHERE library=$1 AND sql_name IS NOT NULL",
            &[&lib],
        )
        .await?
    {
        let (n, s): (String, String) = (r.get(0), r.get(1));
        if let Some(o) = owner_of.get(&pg_name(&n)).cloned() {
            owner_of.insert(pg_name(&s), o);
        }
    }
    let schema = pg_name(lib);
    let me: String = tx.query_one("SELECT current_user::text", &[]).await?.get(0);
    let owner = |o: String| if o == me { None } else { Some(o) };
    let belongs = |rel: &str| -> Option<Option<(String, String)>> {
        match owner_of.get(rel) {
            Some(o) => Some(Some(o.clone())),
            None if pick.objects.is_none() => Some(None),
            None => None,
        }
    };
    let mut pg = Vec::new();
    let rels = tx
        .query(
            "SELECT c.oid, c.relname::text, c.relkind::text, pg_get_userbyid(c.relowner)::text,
                    coalesce(array_to_string(c.reloptions, ','), '')
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname = $1 AND c.relkind IN ('r', 'v', 'S')
              ORDER BY c.oid",
            &[&schema],
        )
        .await?;
    let mut tables: Vec<(u32, String, ObjectRef)> = Vec::new();
    for r in &rels {
        let (oid, rel, kind, own, opts): (u32, String, String, String, String) =
            (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
        match kind.as_str() {
            "S" => {
                // Sequences a column owns go with their table.
                let owned: bool = tx
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_depend WHERE classid = 'pg_class'::regclass AND objid = $1
                                          AND deptype IN ('a', 'i') AND refclassid = 'pg_class'::regclass)",
                        &[&oid],
                    )
                    .await?
                    .get(0);
                if owned {
                    continue;
                }
                let Some(object) = belongs(&rel) else {
                    continue;
                };
                let s = tx
                    .query_one(
                        "SELECT format_type(s.seqtypid, NULL), s.seqincrement, s.seqmin, s.seqmax, s.seqstart, s.seqcycle,
                                ps.last_value, ps.last_value IS NOT NULL
                           FROM pg_sequence s JOIN pg_sequences ps ON ps.schemaname = $2 AND ps.sequencename = $3
                          WHERE s.seqrelid = $1",
                        &[&oid, &schema, &rel],
                    )
                    .await?;
                let (ty, inc, min, max, start, cycle): (String, i64, i64, i64, i64, bool) =
                    (s.get(0), s.get(1), s.get(2), s.get(3), s.get(4), s.get(5));
                let last: Option<i64> = s.get(6);
                pg.push(PgItem {
                    kind: "sequence".into(),
                    name: rel.clone(),
                    sql: format!(
                        "CREATE SEQUENCE {SCHEMA}.{} AS {ty} INCREMENT BY {inc} MINVALUE {min} MAXVALUE {max} START WITH {start} {}",
                        q(&rel),
                        if cycle { "CYCLE" } else { "NO CYCLE" }
                    ),
                    owner: owner(own),
                    object,
                    data: None,
                    seq: last.map(|v| vec![(String::new(), v, true)]).unwrap_or_default(),
                });
            }
            "r" => {
                let Some(object) = belongs(&rel) else {
                    continue;
                };
                let mut cols = Vec::new();
                let mut seqs = Vec::new();
                for c in tx
                    .query(
                        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                                pg_get_expr(d.adbin, d.adrelid), a.attidentity::text,
                                CASE WHEN a.attcollation <> t.typcollation AND a.attcollation <> 0
                                     THEN (SELECT collname::text FROM pg_collation WHERE oid = a.attcollation) END,
                                pg_get_serial_sequence(format('%I.%I', $2::text, $3::text), a.attname)
                           FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid
                           LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                          WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped
                          ORDER BY a.attnum",
                        &[&oid, &schema, &rel],
                    )
                    .await?
                {
                    let (name, ty, notnull): (String, String, bool) = (c.get(0), c.get(1), c.get(2));
                    let dft: Option<String> = c.get(3);
                    let identity: String = c.get(4);
                    let coll: Option<String> = c.get(5);
                    let serial: Option<String> = c.get(6);
                    let mut def = format!("{} ", q(&name));
                    match (&serial, identity.as_str()) {
                        (Some(sq), "a" | "d") => {
                            let s = tx
                                .query_one(
                                    "SELECT s.seqincrement, s.seqmin, s.seqmax, s.seqstart, s.seqcycle
                                       FROM pg_sequence s WHERE s.seqrelid = $1::text::regclass",
                                    &[&sq],
                                )
                                .await?;
                            let (inc, min, max, start, cycle): (i64, i64, i64, i64, bool) =
                                (s.get(0), s.get(1), s.get(2), s.get(3), s.get(4));
                            def.push_str(&format!(
                                "{ty} GENERATED {} AS IDENTITY (START WITH {start} INCREMENT BY {inc} MINVALUE {min} MAXVALUE {max}{})",
                                if identity == "a" { "ALWAYS" } else { "BY DEFAULT" },
                                if cycle { " CYCLE" } else { "" }
                            ));
                            seqs.push(name.clone());
                        }
                        (Some(_), _) if dft.as_deref().is_some_and(|d| d.starts_with("nextval(")) => {
                            def.push_str(match ty.as_str() {
                                "bigint" => "bigserial",
                                "smallint" => "smallserial",
                                _ => "serial",
                            });
                            seqs.push(name.clone());
                        }
                        _ => {
                            def.push_str(&ty);
                            // Character columns saved by earlier
                            // versions ("C") come back in EBCDIC order too.
                            match coll.as_deref() {
                                Some("C" | "ebcdic37") => def.push_str(CHAR_COLLATION),
                                Some(cl) => def.push_str(&format!(" COLLATE {}", q(cl))),
                                None => {}
                            }
                            if notnull {
                                def.push_str(" NOT NULL");
                            }
                            if let Some(d) = &dft {
                                def.push_str(&format!(" DEFAULT {d}"));
                            }
                        }
                    }
                    cols.push(def);
                }
                // The records.
                let stream = tx
                    .copy_out(&format!(
                        "COPY {}.{} TO STDOUT (FORMAT binary)",
                        q(&schema),
                        q(&rel)
                    ))
                    .await?;
                futures_util::pin_mut!(stream);
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                }
                let mut seq = Vec::new();
                for col in &seqs {
                    let r = tx
                        .query_one(
                            &format!(
                                "SELECT last_value, is_called FROM {}",
                                tx.query_one(
                                    "SELECT pg_get_serial_sequence(format('%I.%I', $1::text, $2::text), $3)",
                                    &[&schema, &rel, col],
                                )
                                .await?
                                .get::<_, String>(0)
                            ),
                            &[],
                        )
                        .await?;
                    seq.push((col.clone(), r.get(0), r.get(1)));
                }
                pg.push(PgItem {
                    kind: "table".into(),
                    name: rel.clone(),
                    sql: format!("CREATE TABLE {SCHEMA}.{} ({})", q(&rel), cols.join(", ")),
                    owner: owner(own),
                    object: object.clone(),
                    data: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                    seq,
                });
                tables.push((oid, rel, object));
            }
            _ => {
                // Views (SQL views, logical files, a table's SQL name).
                let Some(object) = belongs(&rel) else {
                    continue;
                };
                let def: String = tx
                    .query_one("SELECT pg_get_viewdef($1::oid, true)", &[&oid])
                    .await?
                    .get(0);
                let check = if opts.contains("check_option=local") {
                    " WITH LOCAL CHECK OPTION"
                } else if opts.contains("check_option=cascaded") {
                    " WITH CASCADED CHECK OPTION"
                } else {
                    ""
                };
                pg.push(PgItem {
                    kind: "view".into(),
                    name: rel.clone(),
                    sql: unschema(
                        &format!(
                            "CREATE VIEW {SCHEMA}.{} AS {}{check}",
                            q(&rel),
                            def.trim().trim_end_matches(';')
                        ),
                        lib,
                    ),
                    owner: owner(own),
                    object,
                    data: None,
                    seq: vec![],
                });
                tables.push((oid, rel, None));
            }
        }
    }
    // Indexes not made by a constraint: on the tables saved, and a
    // logical file's (named library_file_key) on its physical file.
    let lf_names: BTreeSet<String> = objects
        .iter()
        .filter(|o| o.objtype == "*FILE")
        .flat_map(|o| {
            let b = pg_name(&format!("{lib}_{}", o.name));
            [format!("{b}_key"), format!("{b}_ukey")]
        })
        .collect();
    for r in tx
        .query(
            "SELECT ic.relname::text, tc.relname::text, pg_get_indexdef(i.indexrelid), pg_get_userbyid(ic.relowner)::text
               FROM pg_index i
               JOIN pg_class ic ON ic.oid = i.indexrelid
               JOIN pg_class tc ON tc.oid = i.indrelid
               JOIN pg_namespace n ON n.oid = ic.relnamespace
              WHERE n.nspname = $1
                AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid AND c.contype IN ('p', 'u', 'x'))
              ORDER BY ic.oid",
            &[&schema],
        )
        .await?
    {
        let (iname, tname, def, own): (String, String, String, String) = (r.get(0), r.get(1), r.get(2), r.get(3));
        let lf_owner = lf_names
            .contains(&iname)
            .then(|| {
                let base = iname.trim_end_matches("_ukey").trim_end_matches("_key");
                let file = &base[schema.len() + 1..];
                objects
                    .iter()
                    .find(|o| o.objtype == "*FILE" && pg_name(&o.name) == file)
                    .map(|o| (o.name.clone(), o.objtype.clone()))
            })
            .flatten();
        let object = match (lf_owner, belongs(&tname)) {
            (Some(o), _) => Some(o),
            (None, Some(o)) => o,
            (None, None) => continue,
        };
        pg.push(PgItem {
            kind: "index".into(),
            name: iname.clone(),
            sql: unschema(&def, lib),
            owner: owner(own),
            object,
            data: None,
            seq: vec![],
        });
    }
    // Constraints, triggers and their functions, of the tables saved.
    let saved_rels: BTreeMap<String, Option<(String, String)>> = tables
        .iter()
        .map(|(_, r, o)| (r.clone(), o.clone().or_else(|| owner_of.get(r).cloned())))
        .collect();
    for r in tx
        .query(
            "SELECT c.conname::text, c.contype::text, t.relname::text, pg_get_constraintdef(c.oid)
               FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
               JOIN pg_namespace n ON n.oid = t.relnamespace
              WHERE n.nspname = $1 AND c.contype IN ('p', 'u', 'c', 'f', 'x')
              ORDER BY c.oid",
            &[&schema],
        )
        .await?
    {
        let (cname, ctype, tname, def): (String, String, String, String) =
            (r.get(0), r.get(1), r.get(2), r.get(3));
        let Some(object) = saved_rels.get(&tname) else {
            continue;
        };
        pg.push(PgItem {
            kind: if ctype == "f" {
                "fkey".into()
            } else {
                "constraint".into()
            },
            name: cname.clone(),
            sql: unschema(
                &format!(
                    "ALTER TABLE {SCHEMA}.{} ADD CONSTRAINT {} {def}",
                    q(&tname),
                    q(&cname)
                ),
                lib,
            ),
            owner: None,
            object: object.clone(),
            data: None,
            seq: vec![],
        });
    }
    let mut funcs_done = BTreeSet::new();
    for r in tx
        .query(
            "SELECT tg.tgname::text, t.relname::text, pg_get_triggerdef(tg.oid), tg.tgfoid,
                    fn.nspname::text = $1
               FROM pg_trigger tg JOIN pg_class t ON t.oid = tg.tgrelid
               JOIN pg_namespace n ON n.oid = t.relnamespace
               JOIN pg_proc f ON f.oid = tg.tgfoid JOIN pg_namespace fn ON fn.oid = f.pronamespace
              WHERE n.nspname = $1 AND NOT tg.tgisinternal AND tg.tgname NOT LIKE 'as4tainr~%'
              ORDER BY tg.oid",
            &[&schema],
        )
        .await?
    {
        let (tgname, tname, def, foid, local): (String, String, String, u32, bool) =
            (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
        let Some(object) = saved_rels.get(&tname) else {
            continue;
        };
        if local && funcs_done.insert(foid) {
            pg.push(function_item(&tx, foid, lib, object.clone(), &me).await?);
        }
        pg.push(PgItem {
            kind: "trigger".into(),
            name: tgname,
            sql: unschema(&def, lib),
            owner: None,
            object: object.clone(),
            data: None,
            seq: vec![],
        });
    }
    // The library's other functions (SQL routines): SAVLIB.
    if whole {
        for r in tx
            .query(
                "SELECT p.oid FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                  WHERE n.nspname = $1 ORDER BY p.oid",
                &[&schema],
            )
            .await?
        {
            let oid: u32 = r.get(0);
            if funcs_done.insert(oid) {
                pg.push(function_item(&tx, oid, lib, None, &me).await?);
            }
        }
    }
    tx.commit().await?;
    Ok(SavedLib {
        name: lib.to_string(),
        text,
        whole,
        objects,
        catalog: cat,
        pg,
    })
}

async fn function_item(
    tx: &tokio_postgres::Transaction<'_>,
    oid: u32,
    lib: &str,
    object: Option<(String, String)>,
    me: &str,
) -> Result<PgItem> {
    let r = tx
        .query_one(
            "SELECT p.proname::text, pg_get_functiondef(p.oid), pg_get_userbyid(p.proowner)::text,
                    pg_get_function_identity_arguments(p.oid), p.prokind::text
               FROM pg_proc p WHERE p.oid = $1",
            &[&oid],
        )
        .await?;
    let (name, def, own, args, kind): (String, String, String, String, String) =
        (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
    let what = if kind == "p" { "PROCEDURE" } else { "FUNCTION" };
    Ok(PgItem {
        kind: "function".into(),
        name: name.clone(),
        sql: unschema(&def, lib),
        owner: (own != me).then(|| {
            format!(
                "ALTER {what} {SCHEMA}.{}({args}) OWNER TO {}",
                q(&name),
                q(&own)
            )
        }),
        object,
        data: None,
        seq: vec![],
    })
}

/// A save image as the bytes of a save file (gzip of its JSON).
fn pack(img: &SaveImage) -> Result<Vec<u8>> {
    use std::io::Write;
    let json = serde_json::to_vec(img)?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&json)?;
    Ok(gz.finish()?)
}

pub fn unpack(bytes: &[u8]) -> Result<SaveImage> {
    use std::io::Read;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut json)
        .map_err(|_| anyhow!("CPF9898: The save file does not hold an as4tainr save."))?;
    let img: SaveImage = serde_json::from_slice(&json)
        .map_err(|_| anyhow!("CPF9898: The save file does not hold an as4tainr save."))?;
    if img.format != FORMAT {
        bail!(
            "CPF9898: The save file holds a save in format {}; as4tainr reads {FORMAT}.",
            img.format
        );
    }
    Ok(img)
}

/// Restore objects of a saved library into `to` (created when it does
/// not exist): those picked by name and type (None: all). Returns the
/// names restored and messages for those that were not.
async fn restore_library(
    client: &mut Client,
    saved: &SavedLib,
    to: &str,
    pick: &(dyn Fn(&SavedObj) -> bool + Sync),
    user: &str,
) -> Result<(Vec<SavedObj>, Vec<String>)> {
    let from = saved.name.as_str();
    if !catalog::library_exists(client, to).await? {
        catalog::create_library(client, to, &saved.text).await?;
    }
    let mut msgs = Vec::new();
    let mut done: Vec<SavedObj> = Vec::new();
    // Objects already there: a physical file gets the saved records
    // (MBROPT(*MATCH)) when its columns are the same; others are
    // replaced.
    let mut skip_pg: BTreeSet<(String, String)> = BTreeSet::new();
    let mut data_only: BTreeSet<String> = BTreeSet::new();
    let mut todo: Vec<&SavedObj> = Vec::new();
    for o in saved.objects.iter().filter(|o| pick(o)) {
        let exists = catalog::object_exists(client, &[to.to_string()], &o.name, &o.objtype).await?;
        if exists {
            if o.objtype == "*FILE" && o.attribute != "SAVF" {
                skip_pg.insert((o.name.clone(), o.objtype.clone()));
                if o.attribute == "PF" {
                    data_only.insert(pg_name(&o.name));
                }
            } else {
                delete_object(client, to, &o.name, &o.objtype).await?;
            }
        }
        todo.push(o);
    }
    let wanted = |obj: &Option<(String, String)>| -> bool {
        match obj {
            None => saved.whole && todo.len() == saved.objects.len(),
            Some(k) => todo.iter().any(|o| o.name == k.0 && o.objtype == k.1),
        }
    };
    let tx = client.transaction().await?;
    // PostgreSQL objects, in order.
    let order = [
        "sequence",
        "table",
        "index",
        "constraint",
        "view",
        "function",
        "trigger",
        "fkey",
    ];
    for kind in order {
        let items: Vec<&PgItem> = saved
            .pg
            .iter()
            .filter(|i| i.kind == kind && wanted(&i.object))
            .collect();
        let mut pending: Vec<&PgItem> = Vec::new();
        for it in items {
            let existing = it.object.as_ref().is_some_and(|o| skip_pg.contains(o));
            if existing {
                // Records into the physical file that is there.
                if it.kind == "table" && data_only.contains(&it.name) {
                    let t = format!("{}.{}", ident(to), q(&it.name));
                    tx.batch_execute(&format!("TRUNCATE TABLE {t}")).await?;
                    load_data(&tx, &t, it).await.with_context(|| {
                        format!("CPF3282: File {} in {to} not restored: its record format differs from the saved file's", it.name)
                    })?;
                }
                continue;
            }
            pending.push(it);
        }
        // Views may use other views: made in rounds.
        let mut round = 0;
        while !pending.is_empty() {
            let mut left = Vec::new();
            let mut last_err = None;
            for it in pending {
                // Character columns in this system's order (saves made
                // before EBCDIC order have COLLATE "C").
                let coll = crate::collate::clause(&tx).await?;
                let sql = reschema(&it.sql, to)
                    .replace(CHAR_COLLATION, &coll)
                    .replace(" COLLATE \"C\"", &coll);
                let sql = if it.kind == "index" {
                    rename_index(&sql, &it.name, from, to)
                } else {
                    sql
                };
                tx.batch_execute("SAVEPOINT as4tainr_rst").await?;
                match tx.batch_execute(&sql).await {
                    Ok(()) => {
                        tx.batch_execute("RELEASE SAVEPOINT as4tainr_rst").await?;
                        if let Some(own) = &it.owner {
                            let alter = if it.kind == "function" {
                                reschema(own, to)
                            } else {
                                let what = match it.kind.as_str() {
                                    "sequence" => "SEQUENCE",
                                    "view" => "VIEW",
                                    _ => "TABLE",
                                };
                                format!(
                                    "ALTER {what} {}.{} OWNER TO {}",
                                    ident(to),
                                    q(&it.name),
                                    q(own)
                                )
                            };
                            tx.batch_execute(&alter).await?;
                        }
                        if it.kind == "table" {
                            load_data(&tx, &format!("{}.{}", ident(to), q(&it.name)), it).await?;
                        }
                        if it.kind == "sequence" {
                            if let Some((_, v, called)) = it.seq.first() {
                                tx.execute(
                                    "SELECT setval(format('%I.%I', $1::text, $2::text), $3, $4)",
                                    &[&pg_name(to), &it.name, v, called],
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        tx.batch_execute("ROLLBACK TO SAVEPOINT as4tainr_rst")
                            .await?;
                        last_err = Some(format!(
                            "{} {}: {}",
                            it.kind,
                            it.name,
                            e.as_db_error()
                                .map(|d| d.message().to_string())
                                .unwrap_or(e.to_string())
                        ));
                        left.push(it);
                    }
                }
            }
            round += 1;
            if left.is_empty() {
                break;
            }
            if round > 20 || kind != "view" {
                // A foreign key whose parent is not on the system, and
                // the like: the object is restored without it.
                if kind == "fkey" {
                    for it in &left {
                        msgs.push(format!(
                            "Referential constraint {} not restored: {}.",
                            it.name,
                            last_err.clone().unwrap_or_default()
                        ));
                    }
                    break;
                }
                bail!(
                    "CPF3773: Objects not restored to {to}: {}",
                    last_err.unwrap_or_default()
                );
            }
            pending = left;
        }
    }
    // Catalog rows, the library names in them changed.
    for (table, rows) in &saved.catalog {
        let Some(c) = CATALOG.iter().find(|c| c.table == table) else {
            continue;
        };
        for row in rows {
            let mut row = row.clone();
            let key_name = row
                .get(c.name)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let fits = if c.types.is_empty() {
                saved.whole && todo.len() == saved.objects.len()
            } else {
                todo.iter()
                    .any(|o| o.name == key_name && c.types.contains(&o.objtype.as_str()))
                    && !c
                        .types
                        .iter()
                        .any(|t| skip_pg.contains(&(key_name.clone(), t.to_string())))
            };
            if !fits {
                continue;
            }
            if let Some(obj) = row.as_object_mut() {
                obj.insert(c.lib.to_string(), serde_json::Value::String(to.to_string()));
                for l in c.libs {
                    if obj.get(*l).and_then(|v| v.as_str()) == Some(from) {
                        obj.insert(l.to_string(), serde_json::Value::String(to.to_string()));
                    }
                }
            }
            if table == "journaled_files" {
                // Journaling goes on when the journal is there.
                let (jl, jn) = (
                    row["journal_lib"].as_str().unwrap_or("").to_string(),
                    row["journal"].as_str().unwrap_or("").to_string(),
                );
                let there = tx
                    .query_opt(
                        "SELECT 1 FROM as4tainr.journals WHERE library=$1 AND name=$2",
                        &[&jl, &jn],
                    )
                    .await?
                    .is_some();
                if !there {
                    msgs.push(format!(
                        "File {key_name} restored without journaling: journal {jl}/{jn} not found."
                    ));
                    continue;
                }
            }
            tx.execute(
                &format!(
                    "INSERT INTO as4tainr.{table} SELECT * FROM jsonb_populate_record(NULL::as4tainr.{table}, $1::text::jsonb)
                     ON CONFLICT DO NOTHING"
                ),
                &[&row.to_string()],
            )
            .await
            .with_context(|| format!("restoring the description of {key_name}"))?;
        }
    }
    // The objects.
    for o in &todo {
        if skip_pg.contains(&(o.name.clone(), o.objtype.clone())) {
            tx.execute(
                "UPDATE as4tainr.objects SET restored = now(), changed = now(), user_attr = $4
                  WHERE library=$1 AND name=$2 AND objtype=$3",
                &[&to, &o.name, &o.objtype, &o.user_attr],
            )
            .await?;
            done.push((*o).clone());
            continue;
        }
        let owner = match tx
            .query_opt(
                "SELECT 1 FROM as4tainr.user_profiles WHERE name=$1",
                &[&o.owner],
            )
            .await
        {
            Ok(Some(_)) => o.owner.clone(),
            _ if o.owner == "QSECOFR" => o.owner.clone(),
            _ => user.to_string(),
        };
        tx.execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text, owner, public_aut, created, restored,
                                           user_attr, src_file, src_lib, src_mbr, src_changed)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::timestamptz, now(), $9, $10, $11, $12, $13)",
            &[
                &to,
                &o.name,
                &o.objtype,
                &o.attribute,
                &o.text,
                &owner,
                &o.public_aut,
                &o.created,
                &o.user_attr,
                &o.source.0,
                &o.source.1,
                &o.source.2,
                &o.source.3,
            ],
        )
        .await?;
        for (u, a) in &o.auths {
            tx.execute(
                "INSERT INTO as4tainr.object_auths VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[&to, &o.name, &o.objtype, u, a],
            )
            .await?;
        }
        done.push((*o).clone());
    }
    tx.commit().await?;
    // What as4tainr adds to tables: journaling, trigger programs.
    for o in done
        .iter()
        .filter(|o| o.objtype == "*FILE" && o.attribute == "PF")
    {
        if skip_pg.contains(&(o.name.clone(), o.objtype.clone())) {
            continue;
        }
        if let Some((jl, jn)) = crate::journal::journal_of(client, to, &o.name).await? {
            let images: String = client
                .query_one(
                    "SELECT images FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
                    &[&to, &o.name],
                )
                .await?
                .get(0);
            let lit = crate::dds::quote;
            client
                .batch_execute(&format!(
                    "DROP TRIGGER IF EXISTS \"as4tainr~journal\" ON {t};
                     CREATE TRIGGER \"as4tainr~journal\" AFTER INSERT OR UPDATE OR DELETE ON {t}
                     FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.journal_trigger({}, {}, {}, {}, {})",
                    lit(&jl),
                    lit(&jn),
                    lit(&images),
                    lit(to),
                    lit(&o.name),
                    t = format!("{}.{}", ident(to), ident(&o.name))
                ))
                .await?;
            client
                .execute(
                    "SELECT as4tainr_svc.journal_entry($1, $2, 'F', 'MR', 0, NULL, NULL, $3, '*QDDS')",
                    &[&jl, &jn, &crate::journal::member_object(to, &o.name)],
                )
                .await?;
        }
        crate::pftrg::sync_capture(client, to, &o.name).await?;
    }
    crate::sql::grant_library(client, to).await.ok();
    client
        .batch_execute(&format!(
            "GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA {} TO {}",
            ident(to),
            crate::sql::SQL_ROLE
        ))
        .await
        .ok();
    Ok((done, msgs))
}

/// A logical file's index is named for its library: in another library,
/// the new library's name.
fn rename_index(sql: &str, name: &str, from: &str, to: &str) -> String {
    let old_prefix = format!("{}_", pg_name(from));
    if from == to || !name.starts_with(&old_prefix) {
        return sql.to_string();
    }
    let new = format!("{}_{}", pg_name(to), &name[old_prefix.len()..]);
    let quoted_old = q(name);
    if sql.contains(&format!("INDEX {quoted_old} ON")) {
        sql.replacen(
            &format!("INDEX {quoted_old} ON"),
            &format!("INDEX {} ON", q(&new)),
            1,
        )
    } else {
        sql.replacen(
            &format!("INDEX {name} ON"),
            &format!("INDEX {} ON", q(&new)),
            1,
        )
    }
}

async fn load_data(tx: &tokio_postgres::Transaction<'_>, table: &str, it: &PgItem) -> Result<()> {
    if let Some(d) = &it.data {
        let bytes = base64::engine::general_purpose::STANDARD.decode(d)?;
        let sink = tx
            .copy_in(&format!("COPY {table} FROM STDIN (FORMAT binary)"))
            .await?;
        futures_util::pin_mut!(sink);
        sink.send(bytes::Bytes::from(bytes)).await?;
        sink.finish().await?;
    }
    for (col, v, called) in &it.seq {
        if col.is_empty() {
            continue;
        }
        tx.execute(
            "SELECT setval(pg_get_serial_sequence($1, $2), $3, $4)",
            &[&table, col, v, called],
        )
        .await?;
    }
    Ok(())
}

/// Delete an object that is not a file (its catalog rows and PostgreSQL
/// object).
async fn delete_object(client: &mut Client, lib: &str, name: &str, objtype: &str) -> Result<()> {
    for c in CATALOG.iter().filter(|c| c.types.contains(&objtype)) {
        client
            .execute(
                &format!(
                    "DELETE FROM as4tainr.{} WHERE {} = $1 AND {} = $2",
                    c.table, c.lib, c.name
                ),
                &[&lib, &name],
            )
            .await?;
    }
    if objtype == "*DTAARA" {
        client
            .batch_execute(&format!(
                "DROP SEQUENCE IF EXISTS {}.{}",
                ident(lib),
                ident(name)
            ))
            .await?;
    }
    client
        .execute(
            "DELETE FROM as4tainr.object_auths WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&lib, &name, &objtype],
        )
        .await?;
    client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&lib, &name, &objtype],
        )
        .await?;
    Ok(())
}

/// DLTLIB and CLRLIB: the library's objects (its schema, and every
/// catalog row naming it as their library).
pub async fn clear_library(client: &mut Client, lib: &str, delete: bool) -> Result<usize> {
    let n: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.objects WHERE library=$1",
            &[&lib],
        )
        .await?
        .get(0);
    let text = catalog::library_text(client, lib).await.unwrap_or_default();
    let tables = client
        .query(
            "SELECT table_name::text, column_name::text FROM information_schema.columns
              WHERE table_schema = 'as4tainr' AND column_name IN ('library', 'journal_lib')
                AND table_name NOT IN ('objects')",
            &[],
        )
        .await?;
    let tx = client.transaction().await?;
    tx.batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", ident(lib)))
        .await?;
    for r in &tables {
        let (t, c): (String, String) = (r.get(0), r.get(1));
        tx.execute(&format!("DELETE FROM as4tainr.{t} WHERE {c} = $1"), &[&lib])
            .await?;
    }
    tx.execute("DELETE FROM as4tainr.objects WHERE library=$1", &[&lib])
        .await?;
    if delete {
        tx.execute(
            "DELETE FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?;
        tx.execute(
            "DELETE FROM as4tainr.object_auths WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?;
    }
    tx.commit().await?;
    if !delete {
        catalog::create_library(client, lib, &text).await?;
    }
    Ok(n as usize)
}

/// Whether a *FILE is a save file.
pub async fn is_save_file(client: &Client, lib: &str, name: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE' AND attribute='SAVF'",
            &[&lib, &name],
        )
        .await?
        .is_some())
}

/// DLTF of a save file: the object, its authorities and its data.
pub async fn delete_save_file(client: &Client, lib: &str, name: &str) -> Result<()> {
    for t in ["save_files", "object_auths", "objects"] {
        let extra = if t == "save_files" {
            ""
        } else {
            " AND objtype='*FILE'"
        };
        client
            .execute(
                &format!("DELETE FROM as4tainr.{t} WHERE library=$1 AND name=$2{extra}"),
                &[&lib, &name],
            )
            .await?;
    }
    Ok(())
}

/// The object (name, type) a PostgreSQL item belongs to, if any.
type ObjectRef = Option<(String, String)>;

/// What SAVCHGOBJ compares objects with: a time (None: the library has
/// no SAVLIB, so everything is changed) and, for REFDATE(*SAVLIB), where
/// the transactions were when that save began.
struct Since {
    time: Option<String>,
    savlib_xid: Option<String>,
}

/// REFDATE and REFTIME as a timestamp (UTC, the system's time zone):
/// the date in the job's date format, QDATFMT MDY (MMDDYY or MMDDYYYY,
/// separators allowed; YY 40-99 is 19YY, 00-39 20YY); the time HHMMSS or
/// HHMM, separators allowed; REFTIME(*NONE) is the start of the day.
fn reference_time(date: &str, time: &str) -> Result<String> {
    let digits = |s: &str| -> String { s.chars().filter(|c| c.is_ascii_digit()).collect() };
    let bad = || {
        anyhow::anyhow!("CPF9898: REFDATE({date}) is not a date in the job's date format (MMDDYY).")
    };
    let d = digits(date);
    let (m, dd, y): (&str, &str, u32) = match d.len() {
        6 => {
            let yy: u32 = d[4..6].parse()?;
            (
                &d[0..2],
                &d[2..4],
                if yy >= 40 { 1900 + yy } else { 2000 + yy },
            )
        }
        8 => (&d[0..2], &d[2..4], d[4..8].parse()?),
        _ => return Err(bad()),
    };
    let (m, dd): (u32, u32) = (m.parse()?, dd.parse()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&m) || dd == 0 || dd > days[m as usize - 1] {
        return Err(bad());
    }
    let t = if time.eq_ignore_ascii_case("*NONE") || time.is_empty() {
        "000000".to_string()
    } else {
        let t = digits(time);
        match t.len() {
            4 => format!("{t}00"),
            6 => t,
            _ => bail!("CPF9898: REFTIME({time}) is not a time (HHMMSS)."),
        }
    };
    let (h, mi, se): (u32, u32, u32) = (t[0..2].parse()?, t[2..4].parse()?, t[4..6].parse()?);
    if h > 23 || mi > 59 || se > 59 {
        bail!("CPF9898: REFTIME({time}) is not a time (HHMMSS).");
    }
    Ok(format!("{y:04}-{m:02}-{dd:02} {h:02}:{mi:02}:{se:02}+00"))
}

impl Job<'_> {
    pub(crate) async fn save_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        let msgs = match cmd.def.name {
            "CRTSAVF" => self.crtsavf(cmd).await?,
            "CLRSAVF" => {
                let (lib, name) = self.save_file(cmd.get("FILE")).await?;
                self.client
                    .execute(
                        "UPDATE as4tainr.save_files SET data = NULL, saved = NULL WHERE library=$1 AND name=$2",
                        &[&lib, &name],
                    )
                    .await?;
                vec![format!(
                    "CPC3725: Save file {name} in library {lib} was cleared."
                )]
            }
            "DSPSAVF" => return self.dspsavf(cmd).await.map(Some),
            "SAVOBJ" | "SAVLIB" | "SAVCHGOBJ" => self.save(cmd).await?,
            "RSTOBJ" | "RSTLIB" => self.restore(cmd).await?,
            "DLTLIB" | "CLRLIB" => self.dltlib(cmd).await?,
            "CRTDUPOBJ" => self.crtdupobj(cmd).await?,
            "RNMOBJ" => self.rnmobj(cmd).await?,
            "DLTOBJ" => self.dltobj(cmd).await?,
            _ => return Ok(None),
        };
        Ok(Some(Outcome::Messages(msgs)))
    }

    async fn crtsavf(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name) = self.create_target(&cmd.get("FILE").to_ascii_uppercase());
        let name = catalog::validate_name("file", &name)?;
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        if catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*FILE").await? {
            return Err(crate::catalog::file_exists_error(&name, &lib));
        }
        let text = match cmd.get("TEXT") {
            "*BLANK" | "" => String::new(),
            t => t.to_string(),
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1, $2, '*FILE', 'SAVF', $3)",
                &[&lib, &name, &text],
            )
            .await?;
        self.client
            .execute(
                "INSERT INTO as4tainr.save_files (library, name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                &[&lib, &name],
            )
            .await?;
        crate::authority::set_owner(self.client, &lib, &name, "*FILE", &self.user).await?;
        // AUT: the public authority (*LIBCRTAUT: the library's default,
        // *CHANGE here).
        let aut = cmd.get("AUT").trim().to_ascii_uppercase();
        if matches!(aut.as_str(), "*ALL" | "*CHANGE" | "*USE" | "*EXCLUDE") {
            crate::authority::grant(
                self.client,
                &lib,
                &name,
                "*FILE",
                "*PUBLIC",
                crate::authority::Aut::parse(&aut)?,
            )
            .await?;
        }
        Ok(vec![format!(
            "CPC7301: File {name} was created in library {lib}."
        )])
    }

    /// A save file by qualified name.
    async fn save_file(&self, q: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(&q.to_ascii_uppercase());
        for l in &libs {
            if let Some(r) = self
                .client
                .query_opt(
                    "SELECT attribute FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*FILE'",
                    &[l, &name],
                )
                .await?
            {
                if r.get::<_, String>(0) != "SAVF" {
                    bail!("CPF3782: File {name} in {l} is not a save file.");
                }
                return Ok((l.clone(), name));
            }
        }
        let lib = q.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        bail!("CPF9812: There is no file {name} in library {lib}.");
    }

    /// A save file's contents as bytes (CPYTOSTMF). An empty save file
    /// has none.
    pub(crate) async fn save_file_data(&self, lib: &str, name: &str) -> Result<Vec<u8>> {
        let (lib, name) = self.save_file(&format!("{lib}/{name}")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &lib,
            &name,
            "*FILE",
            crate::authority::Aut::Use,
        )
        .await?;
        let data: Option<Vec<u8>> = self
            .client
            .query_one(
                "SELECT data FROM as4tainr.save_files WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        data.ok_or_else(|| anyhow!("CPF3780: Save file {name} in {lib} contains no data."))
    }

    /// A save file's contents from bytes (CPYFRMSTMF): the save file must
    /// exist; one with data is replaced only with MBROPT(*REPLACE).
    pub(crate) async fn put_save_file_data(
        &self,
        lib: &str,
        name: &str,
        bytes: &[u8],
        replace: bool,
    ) -> Result<()> {
        let (lib, name) = self.save_file(&format!("{lib}/{name}")).await?;
        crate::authority::check(
            self.client,
            &self.user,
            &lib,
            &name,
            "*FILE",
            crate::authority::Aut::Change,
        )
        .await?;
        unpack(bytes)
            .map_err(|_| anyhow!("CPF9898: The stream file is not an as4tainr save file."))?;
        if self.savf_image(&lib, &name).await?.is_some() && !replace {
            bail!(
                "CPF9898: Save file {name} in {lib} contains data; MBROPT(*REPLACE) replaces it."
            );
        }
        self.client
            .execute(
                "UPDATE as4tainr.save_files SET data = $3, saved = now() WHERE library=$1 AND name=$2",
                &[&lib, &name, &bytes],
            )
            .await?;
        Ok(())
    }

    async fn savf_image(&self, lib: &str, name: &str) -> Result<Option<SaveImage>> {
        let data: Option<Vec<u8>> = self
            .client
            .query_one(
                "SELECT data FROM as4tainr.save_files WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        data.map(|d| unpack(&d)).transpose()
    }

    async fn dspsavf(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name) = self.save_file(cmd.get("FILE")).await?;
        let img = self.savf_image(&lib, &name).await?;
        let mut lines = Vec::new();
        let mut info = vec![
            ("Save file".to_string(), name.clone()),
            ("Library".to_string(), lib.clone()),
        ];
        if let Some(img) = &img {
            info.push(("Save command".into(), img.command.clone()));
            info.push(("Saved".into(), img.saved.clone()));
            for l in &img.libraries {
                for o in &l.objects {
                    lines.push(format!(
                        "{:<10} {:<10} {:<8} {:<10} {:<10} {}",
                        o.name, l.name, o.objtype, o.attribute, o.owner, o.text
                    ));
                }
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Saved Objects - Save File".into(),
            info,
            heading: format!(
                "{:<10} {:<10} {:<8} {:<10} {:<10} {}",
                "Object", "Library", "Type", "Attribute", "Owner", "Text"
            ),
            rows: lines,
            ..Default::default()
        }))
    }

    /// SAVOBJ OBJ(...) LIB(...) DEV(*SAVF) OBJTYPE(...) SAVF(...), SAVLIB
    /// LIB(...) DEV(*SAVF) SAVF(...), with CLEAR and UPDHST.
    async fn save(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let dev = cmd.get("DEV").to_ascii_uppercase();
        if dev != "*SAVF" {
            bail!("CPF9898: DEV({dev}): as4tainr saves to save files (DEV(*SAVF)).");
        }
        let (slib, sname) = self.save_file(cmd.get("SAVF")).await?;
        let clear = cmd.get("CLEAR").to_ascii_uppercase();
        if self.savf_image(&slib, &sname).await?.is_some() && clear != "*ALL" && clear != "*REPLACE"
        {
            bail!("CPF9898: Save file {sname} in {slib} contains data; CLEAR(*REPLACE) or CLEAR(*ALL) replaces it.");
        }
        let whole = cmd.def.name == "SAVLIB";
        let libs: Vec<String> = cmd
            .list("LIB")
            .iter()
            .map(|l| l.to_ascii_uppercase())
            .collect();
        let objs: Vec<String> = if whole {
            vec!["*ALL".into()]
        } else {
            cmd.list("OBJ")
                .iter()
                .map(|l| l.to_ascii_uppercase())
                .collect()
        };
        let types: Vec<String> = if whole {
            vec!["*ALL".into()]
        } else {
            cmd.list("OBJTYPE")
                .iter()
                .map(|l| l.to_ascii_uppercase())
                .collect()
        };
        let mut img = SaveImage {
            format: FORMAT.into(),
            saved: self
                .client
                .query_one("SELECT now()::text", &[])
                .await?
                .get(0),
            command: cmd.def.name.to_string(),
            libraries: Vec::new(),
        };
        // Where the transactions are before the save: what changes after
        // this is newer than the save (SAVCHGOBJ).
        let xid: String = self
            .client
            .query_one(
                "INSERT INTO as4tainr.xid_clock (xid)
                 VALUES (pg_snapshot_xmin(pg_current_snapshot())::text) RETURNING xid",
                &[],
            )
            .await?
            .get(0);
        let changed_only = cmd.def.name == "SAVCHGOBJ";
        let filter = Filter::from_command(cmd)?;
        let mut msgs = Vec::new();
        let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
        for lib in &libs {
            if !catalog::library_exists(self.client, lib).await? {
                bail!("CPF2110: Library {lib} does not exist.");
            }
            // The objects: by name (generic, *ALL) and type.
            let rows = self
                .client
                .query(
                    &format!(
                        "SELECT name, objtype, attribute FROM as4tainr.objects WHERE library=$1 ORDER BY name{}",
                        crate::collate::clause(self.client).await?
                    ),
                    &[lib],
                )
                .await?;
            let since = if changed_only {
                Some(self.change_reference(cmd, lib).await?)
            } else {
                None
            };
            let mut chosen = BTreeSet::new();
            let mut not_saved = Vec::new();
            let mut omitted = false;
            for r in &rows {
                let (n, t, a): (String, String, String) = (r.get(0), r.get(1), r.get(2));
                let name_ok = objs
                    .iter()
                    .any(|o| o == "*ALL" || catalog::name_matches(o, &n));
                let type_ok = types.iter().any(|x| x == "*ALL" || *x == t);
                if !(name_ok && type_ok && filter.allows(lib, &n, &t, &a)) {
                    if name_ok && type_ok {
                        omitted = true;
                    }
                    continue;
                }
                if let Some(since) = &since {
                    if !self
                        .changed_since(lib, &n, &t, since, cmd.get("OBJJRN"))
                        .await?
                    {
                        continue;
                    }
                }
                if NOT_SAVED.contains(&t.as_str()) {
                    not_saved.push(format!("{n} {t}"));
                    continue;
                }
                // The save file being written is not in the save.
                if a == "SAVF" && *lib == slib && n == sname {
                    omitted = true;
                    continue;
                }
                crate::authority::check(
                    self.client,
                    &self.user,
                    lib,
                    &n,
                    &t,
                    crate::authority::Aut::Use,
                )
                .await?;
                chosen.insert((n, t));
            }
            if chosen.is_empty() && !whole {
                // SAVCHGOBJ of a library with nothing changed: CPF3770 as
                // SAVOBJ (to be checked against a real system).
                bail!("CPF3770: No objects were saved or restored for library {lib}.");
            }
            let pick = Pick {
                objects: if whole && !omitted {
                    None
                } else {
                    Some(chosen)
                },
            };
            let mut saved = save_library(&mut c, lib, &pick, whole).await?;
            // SAVFDTA(*NO): save files without their contents.
            if cmd.get("SAVFDTA").eq_ignore_ascii_case("*NO") {
                for (table, rows) in saved.catalog.iter_mut() {
                    if table == "save_files" {
                        for r in rows.iter_mut() {
                            if let Some(o) = r.as_object_mut() {
                                o.insert("data".into(), serde_json::Value::Null);
                                o.insert("saved".into(), serde_json::Value::Null);
                            }
                        }
                    }
                }
            }
            msgs.push(format!(
                "CPC3722: {} objects were saved from library {lib}.",
                saved.objects.len()
            ));
            for s in not_saved {
                msgs.push(format!(
                    "Object {s} in {lib} not saved (as4tainr does not save this type)."
                ));
            }
            img.libraries.push(saved);
        }
        let bytes = pack(&img)?;
        self.client
            .execute(
                "UPDATE as4tainr.save_files SET data = $3, saved = now() WHERE library=$1 AND name=$2",
                &[&slib, &sname, &bytes],
            )
            .await?;
        // Save history (UPDHST(*YES)); a journaled file's save is in its
        // journal (F MS).
        if !cmd.get("UPDHST").eq_ignore_ascii_case("*NO") {
            for l in img.libraries.iter().filter(|_| whole) {
                // The library's last SAVLIB, the reference of SAVCHGOBJ
                // REFDATE(*SAVLIB), and its files' records then.
                self.client
                    .execute(
                        "UPDATE as4tainr.objects SET savlib = $2::text::timestamptz, savlib_xid = $3
                          WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB'",
                        &[&l.name, &img.saved, &xid],
                    )
                    .await?;
                for o in l.objects.iter().filter(|o| o.objtype == "*FILE") {
                    let rows = self.records(&l.name, &o.name, None).await?.map(|(n, _)| n);
                    self.client
                        .execute(
                            "UPDATE as4tainr.objects SET savlib_rows = $3
                              WHERE library = $1 AND name = $2 AND objtype = '*FILE'",
                            &[&l.name, &o.name, &rows],
                        )
                        .await?;
                }
            }
            for l in &img.libraries {
                for o in &l.objects {
                    self.client
                        .execute(
                            "UPDATE as4tainr.objects SET saved = now() WHERE library=$1 AND name=$2 AND objtype=$3",
                            &[&l.name, &o.name, &o.objtype],
                        )
                        .await?;
                    if o.objtype == "*FILE" {
                        if let Some((jl, jn)) =
                            crate::journal::journal_of(self.client, &l.name, &o.name).await?
                        {
                            let job = self.job_name();
                            crate::journal::entry(
                                self.client,
                                &jl,
                                &jn,
                                "F",
                                "MS",
                                Some(crate::journal::member_object(&l.name, &o.name)),
                                Some("*QDDS"),
                                (&job, &self.user),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        Ok(msgs)
    }

    /// What SAVCHGOBJ compares with: REFDATE and REFTIME, or the
    /// library's last SAVLIB (none: every object counts as changed).
    async fn change_reference(&self, cmd: &Command, lib: &str) -> Result<Since> {
        let date = cmd.get("REFDATE").to_ascii_uppercase();
        if date == "*SAVLIB" {
            let r = self
                .client
                .query_opt(
                    "SELECT savlib::text, savlib_xid FROM as4tainr.objects
                      WHERE library = 'QSYS' AND name = $1 AND objtype = '*LIB'",
                    &[&lib],
                )
                .await?;
            let (time, xid): (Option<String>, Option<String>) = match r {
                Some(r) => (r.get(0), r.get(1)),
                None => (None, None),
            };
            return Ok(Since {
                time,
                savlib_xid: xid,
            });
        }
        Ok(Since {
            time: Some(reference_time(&date, cmd.get("REFTIME"))?),
            savlib_xid: None,
        })
    }

    /// The number of records of a physical file (None: not a table), and
    /// whether one was written after the transactions were at `xid`.
    async fn records(
        &self,
        lib: &str,
        name: &str,
        xid: Option<&str>,
    ) -> Result<Option<(i64, bool)>> {
        let table = format!("{}.{}", catalog::ident(lib), catalog::ident(name));
        let is_table: bool = self
            .client
            .query_one(
                "SELECT coalesce((SELECT relkind = 'r' FROM pg_class WHERE oid = to_regclass($1)), false)",
                &[&table],
            )
            .await?
            .get(0);
        if !is_table {
            return Ok(None);
        }
        // A record written after `xid` has a younger xmin (frozen records
        // are the oldest); one transaction more is counted as newer.
        let r = self
            .client
            .query_one(
                &format!(
                    "SELECT count(*)::bigint,
                            coalesce(bool_or(age(t.xmin)::numeric <=
                              pg_snapshot_xmax(pg_current_snapshot())::text::numeric - $1::text::numeric + 1), false)
                       FROM {table} t"
                ),
                &[&xid.unwrap_or("0")],
            )
            .await?;
        Ok(Some((r.get(0), xid.is_some() && r.get::<_, bool>(1))))
    }

    /// Has the object changed since the reference? Its description
    /// (`objects.changed`: made, restored, text, owner, a data area's
    /// value, a source member...), and a physical file's records.
    /// OBJJRN(*NO): a file journaled since before the reference is not
    /// saved (SAVCHGOBJ's OBJJRN parameter).
    async fn changed_since(
        &self,
        lib: &str,
        name: &str,
        objtype: &str,
        since: &Since,
        objjrn: &str,
    ) -> Result<bool> {
        let Some(time) = &since.time else {
            return Ok(true);
        };
        if objtype == "*FILE" && !objjrn.eq_ignore_ascii_case("*YES") {
            let journaled = self
                .client
                .query_opt(
                    "SELECT 1 FROM as4tainr.journaled_files
                      WHERE library = $1 AND file = $2 AND started < $3::text::timestamptz",
                    &[&lib, &name, time],
                )
                .await?
                .is_some();
            if journaled {
                return Ok(false);
            }
        }
        let described: bool = self
            .client
            .query_opt(
                "SELECT changed > $4::text::timestamptz FROM as4tainr.objects
                  WHERE library = $1 AND name = $2 AND objtype = $3",
                &[&lib, &name, &objtype, time],
            )
            .await?
            .and_then(|r| r.get(0))
            .unwrap_or(true);
        if described || objtype != "*FILE" {
            return Ok(described);
        }
        if let Some(xid) = &since.savlib_xid {
            // Since the last SAVLIB: records written after it, or fewer
            // or more records than it saved.
            let then: Option<i64> = self
                .client
                .query_opt(
                    "SELECT savlib_rows FROM as4tainr.objects
                      WHERE library = $1 AND name = $2 AND objtype = '*FILE'",
                    &[&lib, &name],
                )
                .await?
                .and_then(|r| r.get(0));
            return Ok(match self.records(lib, name, Some(xid)).await? {
                Some((n, newer)) => newer || then != Some(n),
                None => false,
            });
        }
        // Since a date: with commit timestamps (track_commit_timestamp),
        // the records committed after it; without them, the records
        // written after the transactions were where they were at the
        // last save or initialization before the date (or every file
        // when there is none).
        let stamps: bool = self
            .client
            .query_one(
                "SELECT current_setting('track_commit_timestamp') = 'on'",
                &[],
            )
            .await?
            .get(0);
        let table = format!("{}.{}", catalog::ident(lib), catalog::ident(name));
        if stamps {
            if self.records(lib, name, None).await?.is_none() {
                return Ok(false);
            }
            return Ok(self
                .client
                .query_one(
                    &format!(
                        "SELECT coalesce(bool_or(pg_xact_commit_timestamp(t.xmin) > $1::text::timestamptz), false)
                           FROM {table} t"
                    ),
                    &[time],
                )
                .await?
                .get(0));
        }
        let clock: Option<String> = self
            .client
            .query_opt(
                "SELECT xid FROM as4tainr.xid_clock WHERE at <= $1::text::timestamptz ORDER BY at DESC LIMIT 1",
                &[time],
            )
            .await?
            .map(|r| r.get(0));
        Ok(match clock {
            None => self.records(lib, name, None).await?.is_some(),
            Some(x) => self
                .records(lib, name, Some(&x))
                .await?
                .is_some_and(|(_, newer)| newer),
        })
    }

    /// CRTDUPOBJ (CL reference): copies of objects of a library in
    /// another library, or one object under a new name (NEWOBJ). Done as
    /// a save of the objects (the catalog rows, the PostgreSQL objects,
    /// with records only for DATA(*YES)) restored into TOLIB, renamed
    /// first for NEWOBJ.
    async fn crtdupobj(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let up = |k: &str| cmd.get(k).to_ascii_uppercase();
        let pattern = up("OBJ");
        let types: Vec<String> = cmd
            .list("OBJTYPE")
            .iter()
            .map(|t| t.to_ascii_uppercase())
            .collect();
        let type_ok = |t: &str| types.iter().any(|x| x == "*ALL" || x == t);
        // FROMLIB: a library, or the first of the library list or the
        // current library with a matching object.
        let from_libs = match up("FROMLIB").as_str() {
            "*LIBL" => self.library_list(),
            "*CURLIB" => vec![self.current_library()],
            l => vec![catalog::validate_name("library", l)?],
        };
        let mut from = None;
        let mut chosen: Vec<(String, String)> = Vec::new();
        for l in &from_libs {
            let rows = self
                .client
                .query(
                    "SELECT name, objtype FROM as4tainr.objects WHERE library=$1 ORDER BY name, objtype",
                    &[l],
                )
                .await?;
            let found: Vec<(String, String)> = rows
                .iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
                .filter(|(n, t)| {
                    (pattern == "*ALL" || catalog::name_matches(&pattern, n)) && type_ok(t)
                })
                .collect();
            if !found.is_empty() {
                from = Some(l.clone());
                chosen = found;
                break;
            }
        }
        let Some(from) = from else {
            bail!(
                "CPF2105: There is no object {pattern} of type {} in library {}.",
                up("FROMLIB"),
                types.join(" ")
            );
        };
        let to = match up("TOLIB").as_str() {
            "*FROMLIB" | "*SAME" => from.clone(),
            "*CURLIB" => self.current_library(),
            l => catalog::validate_name("library", l)?,
        };
        if !catalog::library_exists(self.client, &to).await? {
            bail!("CPF2110: Library {to} does not exist.");
        }
        let newobj = match up("NEWOBJ").as_str() {
            "*OBJ" | "*SAME" => None,
            n => Some(catalog::validate_name("object", n)?),
        };
        if newobj.is_some() && chosen.len() > 1 {
            bail!(
                "CPF9898: NEWOBJ names one object; {} objects match {pattern}.",
                chosen.len()
            );
        }
        let mut msgs = Vec::new();
        chosen.retain(|(n, t)| {
            if NOT_SAVED.contains(&t.as_str()) {
                msgs.push(format!(
                    "Object {n} type {t} not duplicated (as4tainr does not copy this type)."
                ));
                false
            } else {
                true
            }
        });
        for (n, t) in &chosen {
            crate::authority::check(
                self.client,
                &self.user,
                &from,
                n,
                t,
                crate::authority::Aut::Use,
            )
            .await?;
            let target = newobj.clone().unwrap_or_else(|| n.clone());
            if catalog::object_exists(self.client, std::slice::from_ref(&to), &target, t).await? {
                bail!("Object {target} type {t} already exists in library {to}.\nCPF2130: 0 objects duplicated. 1 objects not duplicated.");
            }
        }
        if chosen.is_empty() {
            bail!(
                "CPF2105: There is no object {pattern} of type {} in library {from}.",
                types.join(" ")
            );
        }
        let pick = Pick {
            objects: Some(chosen.iter().cloned().collect()),
        };
        let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
        let mut saved = save_library(&mut c, &from, &pick, false).await?;
        if !up("DATA").eq("*YES") {
            for it in saved.pg.iter_mut() {
                if it.kind == "table" {
                    it.data = None;
                }
            }
        }
        if let Some(new) = &newobj {
            let (old, objtype) = chosen[0].clone();
            rename_saved(&mut saved, &old, new, &objtype);
        }
        let (done, mut notes) = restore_library(&mut c, &saved, &to, &|_| true, &self.user).await?;
        msgs.append(&mut notes);
        msgs.push(format!(
            "{} objects duplicated from library {from} to library {to}.",
            done.len()
        ));
        Ok(msgs)
    }

    /// RNMOBJ (CL reference): the object gets the new name in its
    /// library and keeps everything else (owner, authorities, dates,
    /// description, contents). Done as a duplicate under the new name
    /// (as CRTDUPOBJ NEWOBJ with DATA(*YES), which keeps the creation
    /// date) and the deletion of the old one; when the old one cannot
    /// be deleted (a physical file with logical files or views over it),
    /// the duplicate is deleted again and the rename fails. A library is
    /// not renamed here.
    async fn rnmobj(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let objtype = cmd.get("OBJTYPE").trim().to_ascii_uppercase();
        let new = catalog::validate_name("object", &cmd.get("NEWOBJ").to_ascii_uppercase())?;
        if objtype == "*LIB" {
            bail!("CPF9898: as4tainr does not rename libraries.");
        }
        let (lib, name) = match cmd.get("OBJ").to_ascii_uppercase().split_once('/') {
            Some(("*CURLIB", n)) => (self.current_library(), n.to_string()),
            Some(("*LIBL", n)) => ("*LIBL".to_string(), n.to_string()),
            Some((l, n)) => (crate::qtemp::real(l), n.to_string()),
            None => ("*LIBL".to_string(), cmd.get("OBJ").to_ascii_uppercase()),
        };
        let libs = if lib == "*LIBL" {
            self.library_list()
        } else {
            vec![lib.clone()]
        };
        let mut found = None;
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, &objtype).await?
            {
                found = Some(l.clone());
                break;
            }
        }
        let Some(lib) = found else {
            bail!(
                "CPF2105: Object {name} of type {objtype} in library {} not found.",
                crate::qtemp::logical(&lib)
            );
        };
        crate::authority::check(
            self.client,
            &self.user,
            &lib,
            &name,
            &objtype,
            crate::authority::Aut::All,
        )
        .await?;
        if catalog::object_exists(self.client, std::slice::from_ref(&lib), &new, &objtype).await? {
            bail!(
                "CPF2112: There is already an object {new} of type {objtype} in library {}.",
                crate::qtemp::logical(&lib)
            );
        }
        let pick = Pick {
            objects: Some([(name.clone(), objtype.clone())].into_iter().collect()),
        };
        let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
        let mut saved = save_library(&mut c, &lib, &pick, false).await?;
        rename_saved(&mut saved, &name, &new, &objtype);
        restore_library(&mut c, &saved, &lib, &|_| true, &self.user).await?;
        let shown = crate::qtemp::logical(&lib);
        let delete = format!("DLTOBJ OBJ({lib}/{name}) OBJTYPE({objtype})");
        if let Err(e) = Box::pin(self.run(&delete)).await {
            let undo = format!("DLTOBJ OBJ({lib}/{new}) OBJTYPE({objtype})");
            let _ = Box::pin(self.run(&undo)).await;
            return Err(e.context(format!(
                "Object {name} in {shown} type {objtype} not renamed: it could not be replaced."
            )));
        }
        Ok(vec![format!(
            "Object {name} in {shown} type {objtype} renamed {new}."
        )])
    }

    /// DLTOBJ (CL reference): an object, or the objects a generic name
    /// matches, of one type, deleted by the type's own delete command
    /// (DLTF, DLTPGM...), whose messages it sends.
    async fn dltobj(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let objtype = cmd.get("OBJTYPE").trim().to_ascii_uppercase();
        let (kw, verb) = match objtype.as_str() {
            "*FILE" => ("FILE", "DLTF"),
            "*PGM" => ("PGM", "DLTPGM"),
            "*SRVPGM" => ("SRVPGM", "DLTSRVPGM"),
            "*MODULE" => ("MODULE", "DLTMOD"),
            "*BNDDIR" => ("BNDDIR", "DLTBNDDIR"),
            "*DTAARA" => ("DTAARA", "DLTDTAARA"),
            "*DTAQ" => ("DTAQ", "DLTDTAQ"),
            "*MSGF" => ("MSGF", "DLTMSGF"),
            "*MSGQ" => ("MSGQ", "DLTMSGQ"),
            "*CMD" => ("CMD", "DLTCMD"),
            "*MENU" => ("MENU", "DLTMNU"),
            "*PNLGRP" => ("PNLGRP", "DLTPNLGRP"),
            "*JOBD" => ("JOBD", "DLTJOBD"),
            "*OUTQ" => ("OUTQ", "DLTOUTQ"),
            "*JRN" => ("JRN", "DLTJRN"),
            "*JRNRCV" => ("JRNRCV", "DLTJRNRCV"),
            "*LIB" => ("LIB", "DLTLIB"),
            t if t.starts_with('*') && t.len() > 1 => {
                bail!("CPF9898: DLTOBJ of type {t} is not done by as4tainr.")
            }
            t => bail!("CPD0084: Object type {t} is not valid."),
        };
        let obj = cmd.get("OBJ").to_ascii_uppercase();
        let (lib, name) = match obj.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), obj.clone()),
        };
        let line = if objtype == "*LIB" {
            format!("DLTLIB LIB({name})")
        } else {
            format!("{verb} {kw}({lib}/{name})")
        };
        match Box::pin(self.run(&line)).await? {
            Outcome::Messages(m) => Ok(m),
            _ => Ok(Vec::new()),
        }
    }

    /// RSTOBJ OBJ(...) SAVLIB(lib) DEV(*SAVF) OBJTYPE(...) SAVF(...)
    /// RSTLIB(...), RSTLIB SAVLIB(lib) DEV(*SAVF) SAVF(...) RSTLIB(...).
    async fn restore(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let dev = cmd.get("DEV").to_ascii_uppercase();
        if dev != "*SAVF" {
            bail!("CPF9898: DEV({dev}): as4tainr restores from save files (DEV(*SAVF)).");
        }
        let (slib, sname) = self.save_file(cmd.get("SAVF")).await?;
        let Some(img) = self.savf_image(&slib, &sname).await? else {
            bail!("CPF3780: Save file {sname} in {slib} contains no data.");
        };
        let savlib = cmd
            .list("SAVLIB")
            .first()
            .map(|l| l.to_ascii_uppercase())
            .unwrap_or_default();
        let saved = img
            .libraries
            .iter()
            .find(|l| savlib == "*ANY" || l.name == savlib)
            .ok_or_else(|| {
                anyhow!("CPF3780: Library {savlib} is not in save file {slib}/{sname}.")
            })?
            .clone();
        let to = match cmd.get("RSTLIB").to_ascii_uppercase().as_str() {
            "" | "*SAVLIB" => saved.name.clone(),
            l => catalog::validate_name("library", l)?,
        };
        if cmd.def.name == "RSTOBJ" && !catalog::library_exists(self.client, &to).await? {
            bail!("CPF2110: Library {to} does not exist.");
        }
        let all = cmd.def.name == "RSTLIB";
        let objs: Vec<String> = if all {
            vec!["*ALL".into()]
        } else {
            cmd.list("OBJ")
                .iter()
                .map(|l| l.to_ascii_uppercase())
                .collect()
        };
        let types: Vec<String> = if all {
            vec!["*ALL".into()]
        } else {
            cmd.list("OBJTYPE")
                .iter()
                .map(|l| l.to_ascii_uppercase())
                .collect()
        };
        let filter = Filter::from_command(cmd)?;
        // FILEMBR: whole files only (*ALL members).
        for e in cmd.list("FILEMBR") {
            let w: Vec<String> = e
                .split_whitespace()
                .map(|x| {
                    x.trim_matches(|c| c == '(' || c == ')')
                        .to_ascii_uppercase()
                })
                .filter(|x| !x.is_empty())
                .collect();
            if w.iter().skip(1).any(|m| m != "*ALL") {
                bail!("CPF9898: FILEMBR({e}): as4tainr restores whole files (members *ALL).");
            }
        }
        if !matches!(cmd.get("OPTION").to_ascii_uppercase().as_str(), "" | "*ALL") {
            bail!(
                "CPF9898: OPTION({}): as4tainr restores every object (OPTION(*ALL)).",
                cmd.get("OPTION")
            );
        }
        let pick = |o: &SavedObj| {
            objs.iter()
                .any(|x| x == "*ALL" || catalog::name_matches(x, &o.name))
                && types.iter().any(|x| x == "*ALL" || *x == o.objtype)
                && filter.allows(&saved.name, &o.name, &o.objtype, &o.attribute)
        };
        if !saved.objects.iter().any(pick) {
            bail!(
                "CPF3770: No objects were saved or restored for library {}.",
                saved.name
            );
        }
        let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
        let user = self.user.clone();
        let (done, mut notes) = restore_library(&mut c, &saved, &to, &pick, &user).await?;
        let mut msgs = vec![format!(
            "{} objects were restored from {} to {to}.",
            done.len(),
            saved.name
        )];
        msgs.append(&mut notes);
        Ok(msgs)
    }

    async fn dltlib(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let lib = cmd.get("LIB").to_ascii_uppercase();
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} does not exist.");
        }
        let delete = cmd.def.name == "DLTLIB";
        // CLRLIB QTEMP clears the job's own; it cannot be deleted.
        let qtemp = crate::qtemp::is_qtemp(&lib);
        if matches!(
            lib.as_str(),
            "QSYS" | "QSYS2" | "QGPL" | "QUSRSYS" | "QHLPSYS"
        ) || (qtemp && (delete || crate::qtemp::current().is_none()))
        {
            let lib = if qtemp { "QTEMP" } else { lib.as_str() };
            bail!("CPF9898: Library {lib} is a system library and cannot be deleted or cleared.");
        }
        // The job's own QTEMP: the job may clear it.
        if !qtemp {
            crate::authority::check(
                self.client,
                &self.user,
                "QSYS",
                &lib,
                "*LIB",
                crate::authority::Aut::All,
            )
            .await?;
        }
        let mut c = crate::runtime::connect_db(&self.env.database_url).await?;
        let n = clear_library(&mut c, &lib, delete).await?;
        Ok(vec![if delete {
            format!("Library {lib} deleted.")
        } else {
            format!("Library {lib} cleared, {n} objects deleted.")
        }])
    }
}

/// A saved object under another name (CRTDUPOBJ NEWOBJ): its object
/// entry, its catalog rows, and for a file its PostgreSQL objects (the
/// table and what is named after it).
fn rename_saved(saved: &mut SavedLib, old: &str, new: &str, objtype: &str) {
    for o in saved.objects.iter_mut() {
        if o.name == old && o.objtype == objtype {
            o.name = new.to_string();
        }
    }
    for (table, rows) in saved.catalog.iter_mut() {
        let Some(c) = CATALOG.iter().find(|c| c.table == table) else {
            continue;
        };
        if !c.types.contains(&objtype) {
            continue;
        }
        for row in rows.iter_mut() {
            let Some(obj) = row.as_object_mut() else {
                continue;
            };
            if obj.get(c.name).and_then(|v| v.as_str()) != Some(old) {
                continue;
            }
            obj.insert(
                c.name.to_string(),
                serde_json::Value::String(new.to_string()),
            );
            // A command's definition names itself.
            if table == "commands" {
                if let Some(d) = obj.get("definition").and_then(|v| v.as_str()) {
                    if let Ok(mut j) = serde_json::from_str::<serde_json::Value>(d) {
                        j["name"] = serde_json::Value::String(new.to_string());
                        obj.insert(
                            "definition".into(),
                            serde_json::Value::String(j.to_string()),
                        );
                    }
                }
            }
        }
    }
    let (po, pn) = (pg_name(old), pg_name(new));
    for it in saved.pg.iter_mut() {
        if it
            .object
            .as_ref()
            .is_some_and(|(n, t)| n == old && t == objtype)
        {
            it.object = Some((new.to_string(), objtype.to_string()));
            if it.name == po {
                it.name = pn.clone();
            } else if let Some(rest) = it.name.strip_prefix(&format!("{po}_")) {
                it.name = format!("{pn}_{rest}");
            }
            let sql = it
                .sql
                .replace(&q(&po), &q(&pn))
                .replace(&format!("\"{po}_"), &format!("\"{pn}_"));
            // Names made from the table's (ta_pk), unquoted.
            let mut out = String::new();
            let cs: Vec<char> = sql.chars().collect();
            let mut i = 0;
            let ident =
                |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#' || c == '@';
            while i < cs.len() {
                let starts = (i == 0 || !ident(cs[i - 1]))
                    && cs[i..].iter().take(po.len() + 1).collect::<String>() == format!("{po}_");
                if starts {
                    // The whole name, quoted when the new one is not a
                    // plain identifier (#U00000001_pk).
                    let mut j = i + po.len();
                    while j < cs.len() && ident(cs[j]) {
                        j += 1;
                    }
                    let renamed = format!("{pn}{}", cs[i + po.len()..j].iter().collect::<String>());
                    let plain = renamed
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                        && renamed
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
                    if plain {
                        out.push_str(&renamed);
                    } else {
                        out.push_str(&q(&renamed));
                    }
                    i = j;
                } else {
                    out.push(cs[i]);
                    i += 1;
                }
            }
            it.sql = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names() {
        let s = unschema(
            "CREATE VIEW t1.v AS SELECT a FROM t1.x JOIN \"t1\".y ON t1.x.k = y.k, xt1.z",
            "T1",
        );
        assert_eq!(
            reschema(&s, "NEW"),
            "CREATE VIEW \"new\".v AS SELECT a FROM \"new\".x JOIN \"new\".y ON \"new\".x.k = y.k, xt1.z"
        );
        assert_eq!(
            rename_index(
                "CREATE INDEX t1_lf_key ON \"new\".pf USING btree (k)",
                "t1_lf_key",
                "T1",
                "NEW"
            ),
            "CREATE INDEX \"new_lf_key\" ON \"new\".pf USING btree (k)"
        );
    }
}
