//! The order of character data: EBCDIC, as on IBM i.
//!
//! IBM i keeps character fields in their CCSID (37 here) and orders keys,
//! ORDER BY and comparisons of character columns by those bytes (the
//! sort sequence *HEX, the default of CRTPF SRTSEQ and of SQL): blank
//! and special characters, then lowercase letters, then uppercase, then
//! digits ("a" < "A" < "1"). as4tainr stores text, so character columns
//! get a PostgreSQL ICU collation, `as4tainr.ebcdic37`, whose rules list
//! the 255 characters of CCSID 37 (all but x'00') in the order of their
//! EBCDIC bytes; characters CCSID 37 does not have come after them.
//!
//! ICU tailoring rules need PostgreSQL 16 with ICU. Without them, keys
//! stay in byte order ("C"), as before, and catalog initialization says
//! so. Columns made by earlier versions (COLLATE "C") are changed at
//! catalog initialization, all in one transaction with the views over
//! them made again.

use anyhow::Result;
use tokio_postgres::Client;

/// The collation character columns are made with.
pub const COLLATION: &str = "as4tainr.ebcdic37";

/// The `COLLATE` clause of a character column: EBCDIC order when the
/// database has the collation, byte order ("C") when this PostgreSQL
/// could not make it (or the character columns of an earlier version
/// could not be changed to it).
pub async fn clause<C: tokio_postgres::GenericClient>(c: &C) -> Result<String> {
    Ok(if exists(c).await? {
        format!(" COLLATE {COLLATION}")
    } else {
        " COLLATE \"C\"".to_string()
    })
}

async fn exists<C: tokio_postgres::GenericClient>(c: &C) -> Result<bool> {
    Ok(c.query_opt(
        "SELECT 1 FROM pg_collation c JOIN pg_namespace n ON n.oid = c.collnamespace
              WHERE n.nspname = 'as4tainr' AND c.collname = 'ebcdic37'",
        &[],
    )
    .await?
    .is_some())
}

/// ICU rules: every CCSID 37 character but x'00' (no PostgreSQL text
/// holds it), in EBCDIC order, before the first regular character.
pub fn rules() -> String {
    let mut r = String::from("&[before 1][first regular]");
    for &u in &crate::tn5250::ebcdic::TO_UNICODE {
        if u == 0 {
            continue;
        }
        let c = char::from_u32(u as u32).unwrap_or('\u{1a}');
        r.push('<');
        if c == '\'' {
            r.push_str("''");
        } else {
            r.push('\'');
            r.push(c);
            r.push('\'');
        }
    }
    r
}

/// Make the collation the first time, and change the character columns
/// an earlier version made ("C") to it, in one transaction: a database
/// never has both orders, which PostgreSQL could not compare. When that
/// fails, everything stays in byte order and as4tainr says why.
pub async fn init(client: &Client) -> Result<()> {
    if exists(client).await? {
        return Ok(());
    }
    client.batch_execute("BEGIN").await?;
    let r = async {
        client
            .batch_execute(&format!(
                "CREATE COLLATION {COLLATION} (provider = icu, locale = 'und', rules = $R${}$R$)",
                rules()
            ))
            .await?;
        migrate(client).await
    }
    .await;
    match r {
        Ok(()) => client.batch_execute("COMMIT").await?,
        Err(e) => {
            client.batch_execute("ROLLBACK").await?;
            eprintln!("as4tainr: character keys stay in byte order, not EBCDIC order: {e:#}");
        }
    }
    Ok(())
}

/// Character columns of library tables still in byte order ("C"). A
/// column a view uses cannot be changed, so the views over the tables
/// (SQL views, long SQL names, logical files) are dropped first and made
/// again after, as they were: definition, options, owner, privileges,
/// comment and INSTEAD OF triggers.
async fn migrate(client: &Client) -> Result<()> {
    let rows = client
        .query(
            "SELECT c.oid, n.nspname::text, c.relname::text,
                    array_agg(a.attname::text ORDER BY a.attnum),
                    array_agg(format_type(a.atttypid, a.atttypmod) ORDER BY a.attnum)
               FROM pg_attribute a
               JOIN pg_class c ON c.oid = a.attrelid AND c.relkind = 'r'
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE a.attnum > 0 AND NOT a.attisdropped
                AND a.attcollation = (SELECT oid FROM pg_collation
                                       WHERE collname = 'C' AND collnamespace = 'pg_catalog'::regnamespace)
                AND n.nspname NOT IN ('as4tainr', 'as4tainr_svc', 'qsys2', 'pg_catalog', 'information_schema')
                AND EXISTS (SELECT 1 FROM as4tainr.objects o
                             WHERE o.library = 'QSYS' AND o.objtype = '*LIB' AND lower(o.name) = n.nspname)
              GROUP BY c.oid, n.nspname, c.relname",
            &[],
        )
        .await?;
    let oids: Vec<u32> = rows.iter().map(|r| r.get(0)).collect();
    let views = saved_views(client, &oids).await?;
    for v in views.iter().rev() {
        client.batch_execute(&v.drop).await?;
    }
    for r in &rows {
        let (schema, table): (String, String) = (r.get(1), r.get(2));
        let cols: Vec<String> = r.get(3);
        let types: Vec<String> = r.get(4);
        let alters: Vec<String> = cols
            .iter()
            .zip(&types)
            .map(|(c, t)| format!("ALTER COLUMN {} TYPE {t} COLLATE {COLLATION}", quote(c)))
            .collect();
        client
            .batch_execute(&format!(
                "ALTER TABLE {}.{} {}",
                quote(&schema),
                quote(&table),
                alters.join(", ")
            ))
            .await?;
    }
    for v in &views {
        for s in &v.create {
            client.batch_execute(s).await?;
        }
    }
    Ok(())
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A view to make again: its statements, in order.
struct SavedView {
    drop: String,
    create: Vec<String>,
}

/// The views over the tables, and over those views (the deepest last),
/// with what makes them again.
async fn saved_views(client: &Client, oids: &[u32]) -> Result<Vec<SavedView>> {
    let views = client
        .query(
            "WITH RECURSIVE v(oid, depth) AS (
                 SELECT DISTINCT r.ev_class, 1 FROM pg_depend d JOIN pg_rewrite r ON r.oid = d.objid
                  WHERE d.refobjid = ANY($1) AND NOT r.ev_class = ANY($1)
                 UNION
                 SELECT DISTINCT r.ev_class, v.depth + 1 FROM v
                   JOIN pg_depend d ON d.refobjid = v.oid
                   JOIN pg_rewrite r ON r.oid = d.objid
                  WHERE r.ev_class <> v.oid AND v.depth < 20)
             SELECT c.oid, n.nspname::text, c.relname::text, pg_get_viewdef(c.oid, true),
                    pg_get_userbyid(c.relowner)::text, coalesce(array_to_string(c.reloptions, ', '), ''),
                    obj_description(c.oid, 'pg_class'), max(v.depth)
               FROM v JOIN pg_class c ON c.oid = v.oid AND c.relkind = 'v'
               JOIN pg_namespace n ON n.oid = c.relnamespace
              GROUP BY c.oid, n.nspname, c.relname, c.relowner, c.reloptions
              ORDER BY max(v.depth), c.oid",
            &[&oids],
        )
        .await?;
    let mut saved: Vec<SavedView> = Vec::new();
    for v in &views {
        let voi: u32 = v.get(0);
        let (vs, vn, def, owner, opts): (String, String, String, String, String) =
            (v.get(1), v.get(2), v.get(3), v.get(4), v.get(5));
        let comment: Option<String> = v.get(6);
        let name = format!("{}.{}", quote(&vs), quote(&vn));
        let mut create = vec![format!(
            "CREATE VIEW {name}{} AS {}",
            if opts.is_empty() {
                String::new()
            } else {
                format!(" WITH ({opts})")
            },
            def.trim_end().trim_end_matches(';')
        )];
        create.push(format!("ALTER VIEW {name} OWNER TO {}", quote(&owner)));
        let grants = client
            .query(
                "SELECT CASE WHEN a.grantee = 0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(a.grantee)) END,
                        a.privilege_type, a.is_grantable
                   FROM pg_class c, aclexplode(c.relacl) a WHERE c.oid = $1",
                &[&voi],
            )
            .await?;
        for g in grants {
            let (who, what, opt): (String, String, bool) = (g.get(0), g.get(1), g.get(2));
            create.push(format!(
                "GRANT {what} ON {name} TO {who}{}",
                if opt { " WITH GRANT OPTION" } else { "" }
            ));
        }
        if let Some(c) = comment {
            create.push(format!(
                "COMMENT ON VIEW {name} IS '{}'",
                c.replace('\'', "''")
            ));
        }
        let trg = client
            .query(
                "SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgrelid = $1 AND NOT tgisinternal",
                &[&voi],
            )
            .await?;
        create.extend(trg.iter().map(|t| t.get::<_, String>(0)));
        saved.push(SavedView {
            drop: format!("DROP VIEW {name}"),
            create,
        });
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    // CCSID 37 (the EBCDIC table): blank x'40', 'a' x'81', 'A' x'C1',
    // '0' x'F0'.
    #[test]
    fn rules_list_ccsid_37_in_byte_order() {
        let r = rules();
        let at = |s: &str| r.find(s).unwrap();
        assert!(at("' '") < at("'a'"));
        assert!(at("'a'") < at("'z'"));
        assert!(at("'z'") < at("'A'"));
        assert!(at("'Z'") < at("'0'"));
        assert!(r.contains("<''"), "the apostrophe is quoted");
        // 255 relations, and '<' is one of the characters.
        assert_eq!(r.matches('<').count(), 256);
    }
}
