//! IBM i services for SQL: the QSYS2 catalog views and table functions.
//!
//! Written from the Db2 for i SQL Reference (catalog views SYSSCHEMAS,
//! SYSTABLES, SYSCOLUMNS, SYSINDEXES, SYSKEYS, SYSVIEWS) and the IBM i
//! services reference (OBJECT_STATISTICS). Column names, order and code
//! values follow those pages; what as4tainr cannot know (IASPs, journals,
//! save history...) is the null value or the documented "no" value.
//!
//! The views are PostgreSQL views in the QSYS2 schema, owned by the
//! catalog user: the SQL login reads them but never the `as4tainr`
//! catalog itself (views read their tables with their owner's rights).
//! Table functions that need the caller's library list are SQL functions
//! running with the caller's rights; they read the object list through
//! the view `as4tainr_svc.objects`, the only thing in that schema.

use anyhow::{Context, Result};
use tokio_postgres::Client;

use crate::sql::SQL_ROLE;

/// The services' version: change it whenever a definition below changes.
const VERSION: &str = "as4tainr services 3";

/// The buffer length of a field in a record (what an RPG program reads),
/// from its DDS data type, length and decimal positions.
const FIELD_BYTES: &str = r#"
CREATE OR REPLACE FUNCTION as4tainr_svc.field_bytes(t text, len int, scale int, dbl boolean, varlen boolean)
RETURNS int LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE t
    WHEN 'A' THEN len + CASE WHEN varlen THEN 2 ELSE 0 END
    WHEN 'H' THEN len + CASE WHEN varlen THEN 2 ELSE 0 END
    WHEN 'G' THEN 2 * len + CASE WHEN varlen THEN 2 ELSE 0 END
    WHEN 'P' THEN len / 2 + 1
    WHEN 'S' THEN len
    WHEN 'B' THEN CASE WHEN len <= 4 THEN 2 WHEN len <= 9 THEN 4 ELSE 8 END
    WHEN 'F' THEN CASE WHEN dbl OR len > 9 THEN 8 ELSE 4 END
    WHEN 'L' THEN 10
    WHEN 'T' THEN 8
    WHEN 'Z' THEN len
    WHEN 'DECFLOAT' THEN CASE WHEN len = 16 THEN 8 ELSE 16 END
    WHEN 'BOOLEAN' THEN 1
    ELSE len
  END
$$;
"#;

/// The SQL data type name SYSCOLUMNS shows for a field (DATA_TYPE codes
/// of the Db2 for i SQL Reference, SYSCOLUMNS).
const SQL_TYPE: &str = r#"
CREATE OR REPLACE FUNCTION as4tainr_svc.sql_type(t text, len int, scale int, varlen boolean)
RETURNS text LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE t
    WHEN 'A' THEN CASE WHEN varlen THEN 'VARCHAR' ELSE 'CHAR' END
    WHEN 'H' THEN CASE WHEN varlen THEN 'VARBIN' ELSE 'BINARY' END
    WHEN 'G' THEN CASE WHEN varlen THEN 'VARG' ELSE 'GRAPHIC' END
    WHEN 'P' THEN 'DECIMAL'
    WHEN 'S' THEN 'NUMERIC'
    WHEN 'B' THEN CASE WHEN scale > 0 THEN 'DECIMAL' WHEN len <= 4 THEN 'SMALLINT'
                       WHEN len <= 9 THEN 'INTEGER' ELSE 'BIGINT' END
    WHEN 'F' THEN 'FLOAT'
    WHEN 'L' THEN 'DATE'
    WHEN 'T' THEN 'TIME'
    WHEN 'Z' THEN 'TIMESTMP'
    ELSE t
  END
$$;
"#;

const DDL: &str = r#"
-- Every object, with the facts the services show.
CREATE OR REPLACE VIEW as4tainr_svc.objects AS
SELECT o.library, o.name, o.objtype, o.attribute, o.text, o.created, o.owner,
       f.sql_name,
       CASE WHEN o.objtype <> '*FILE' THEN NULL
            WHEN f.view IS NOT NULL THEN 'VIEW'
            ELSE f.sql_type END AS sql_object_type,
       CASE WHEN o.objtype = '*FILE' AND o.attribute IN ('PF', 'LF')
            THEN coalesce((SELECT pg_catalog.pg_total_relation_size(c.oid)
                           FROM pg_catalog.pg_class c
                           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                           WHERE n.nspname = lower(o.library) AND c.relname = lower(o.name)), 0)
            WHEN o.objtype = '*FILE' AND o.attribute = 'PF-SRC'
            THEN coalesce((SELECT sum(octet_length(m.source)) FROM as4tainr.source_members m
                           WHERE m.library = o.library AND m.file = o.name), 0)
            WHEN o.objtype = '*FILE' AND o.attribute = 'DSPF'
            THEN coalesce((SELECT octet_length(d.source) FROM as4tainr.display_files d
                           WHERE d.library = o.library AND d.name = o.name), 0)
            WHEN o.objtype = '*PGM'
            THEN coalesce((SELECT octet_length(p.source) FROM as4tainr.programs p
                           WHERE p.library = o.library AND p.name = o.name), 0)
            ELSE 0 END AS size
FROM as4tainr.objects o
LEFT JOIN as4tainr.files f ON o.objtype = '*FILE' AND f.library = o.library AND f.name = o.name;

-- SYSSCHEMAS: one row per library.
CREATE OR REPLACE VIEW qsys2.sysschemas AS
SELECT CAST(o.name AS varchar(128)) AS schema_name,
       CAST(o.owner AS varchar(128)) AS schema_owner,
       CAST(o.owner AS varchar(128)) AS schema_creator,
       CAST(o.created AS timestamp) AS creation_timestamp,
       CAST(coalesce((SELECT sum(s.size) FROM as4tainr_svc.objects s WHERE s.library = o.name), 0)
            AS decimal(15,0)) AS schema_size,
       CAST(nullif(o.text, '') AS varchar(50)) AS schema_text,
       CAST(o.name AS char(10)) AS system_schema_name,
       CAST(0 AS smallint) AS iasp_number
FROM as4tainr_svc.objects o
WHERE o.library = 'QSYS' AND o.objtype = '*LIB';

-- SYSTABLES: tables, physical files, views and logical files (not SQL
-- indexes, which are in SYSINDEXES).
CREATE OR REPLACE VIEW qsys2.systables AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) AS table_name,
       CAST(o.owner AS varchar(128)) AS table_owner,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'V'
                 WHEN f.sql_type = 'TABLE' THEN 'T'
                 WHEN o.attribute = 'LF' THEN 'L'
                 ELSE 'P' END AS char(1)) AS table_type,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 3
                 ELSE (SELECT count(*) FROM as4tainr.file_fields ff
                       WHERE ff.library = o.library AND ff.file = o.name) END AS integer) AS column_count,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 92
                 ELSE coalesce((SELECT sum(as4tainr_svc.field_bytes(ff.data_type, ff.length, ff.decimals, ff.flt_double, ff.varlen))
                                FROM as4tainr.file_fields ff
                                WHERE ff.library = o.library AND ff.file = o.name), 0) END AS integer) AS row_length,
       CAST(nullif(o.text, '') AS varchar(50)) AS table_text,
       CAST(NULL AS varchar(2000)) AS long_comment,
       CAST(o.library AS varchar(128)) AS table_schema,
       CAST(o.created AS timestamp) AS last_altered_timestamp,
       CAST(o.name AS char(10)) AS system_table_name,
       CAST(o.library AS char(10)) AS system_table_schema,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 'S' ELSE 'D' END AS char(1)) AS file_type,
       CAST(NULL AS varchar(18)) AS base_table_catalog,
       CAST(NULL AS varchar(128)) AS base_table_schema,
       CAST(NULL AS varchar(128)) AS base_table_name,
       CAST(NULL AS varchar(10)) AS base_table_member,
       CAST(CASE WHEN o.library IN ('QSYS', 'QSYS2') THEN 'Y' ELSE 'N' END AS char(1)) AS system_table,
       CAST(CASE WHEN o.attribute = 'LF' AND EXISTS (
                   SELECT 1 FROM as4tainr.logical_files lf
                   WHERE lf.library = o.library AND lf.name = o.name AND lf.selection <> 'TRUE')
                 THEN 'Y' ELSE 'N' END AS char(1)) AS select_omit,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'NO' ELSE 'YES' END AS varchar(3)) AS is_insertable_into,
       CAST(0 AS smallint) AS iasp_number,
       CAST(NULL AS varchar(3)) AS enabled,
       CAST(NULL AS varchar(6)) AS maintenance,
       CAST(NULL AS varchar(9)) AS refresh,
       CAST(NULL AS timestamp) AS refresh_time,
       CAST(NULL AS text) AS mqt_definition,
       CAST(NULL AS char(2)) AS isolation,
       CAST('NO' AS varchar(11)) AS partition_table,
       CAST(o.owner AS varchar(128)) AS table_definer,
       CAST(NULL AS char(1)) AS mqt_restore_deferred,
       CAST(NULL AS char(1)) AS rounding_mode,
       CAST(' ' AS char(1)) AS control
FROM as4tainr.objects o
LEFT JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name
WHERE o.objtype = '*FILE' AND o.attribute IN ('PF', 'LF', 'PF-SRC')
  AND coalesce(f.sql_type, '') <> 'INDEX';

-- SYSCOLUMNS: the columns of tables, physical and logical files and views.
CREATE OR REPLACE VIEW qsys2.syscolumns AS
SELECT CAST(coalesce(ff.alias, ff.name) AS varchar(128)) AS column_name,
       CAST(coalesce(f.sql_name, o.name) AS varchar(128)) AS table_name,
       CAST(o.owner AS varchar(128)) AS table_owner,
       CAST(ff.seq + 1 AS integer) AS ordinal_position,
       CAST(as4tainr_svc.sql_type(ff.data_type, ff.length, ff.decimals, ff.varlen) AS varchar(8)) AS data_type,
       CAST(CASE WHEN ff.data_type IN ('P', 'S') OR (ff.data_type = 'B' AND ff.decimals > 0) THEN ff.length
                 WHEN ff.data_type IN ('B', 'F', 'DECFLOAT')
                 THEN as4tainr_svc.field_bytes(ff.data_type, ff.length, ff.decimals, ff.flt_double, ff.varlen)
                 WHEN ff.data_type = 'L' THEN 4
                 WHEN ff.data_type = 'T' THEN 3
                 WHEN ff.data_type = 'Z' THEN 10
                 ELSE ff.length END AS integer) AS length,
       CAST(CASE WHEN ff.data_type IN ('P', 'S', 'B') THEN ff.decimals END AS integer) AS numeric_scale,
       CAST(CASE WHEN ff.null_capable THEN 'Y' ELSE 'N' END AS char(1)) AS is_nullable,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'N' ELSE 'Y' END AS char(1)) AS is_updatable,
       CAST(NULL AS varchar(2000)) AS long_comment,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'N'
                 WHEN ff.identity IS NOT NULL THEN
                   CASE WHEN ff.identity LIKE '%"always":true%' THEN 'I' ELSE 'J' END
                 -- No default only for NOT NULL without DEFAULT; a DDS
                 -- field without DFT gets blanks or zero.
                 WHEN ff.dft = '"Required"' THEN 'N'
                 ELSE 'Y' END AS char(1)) AS has_default,
       CAST(NULL AS varchar(60)) AS column_heading,
       CAST(as4tainr_svc.field_bytes(ff.data_type, ff.length, ff.decimals, ff.flt_double, ff.varlen) AS integer) AS storage,
       CAST(CASE WHEN ff.data_type IN ('P', 'S') THEN ff.length
                 WHEN ff.data_type = 'B' THEN CASE WHEN ff.decimals > 0 THEN ff.length
                                                   WHEN ff.length <= 4 THEN 5
                                                   WHEN ff.length <= 9 THEN 10 ELSE 19 END
                 WHEN ff.data_type = 'F' THEN CASE WHEN ff.flt_double OR ff.length > 9 THEN 53 ELSE 24 END
                 WHEN ff.data_type = 'DECFLOAT' THEN ff.length END AS integer) AS numeric_precision,
       CAST(CASE WHEN ff.data_type IN ('A', 'L', 'T', 'Z', 'CLOB', 'XML') THEN 1208
                 WHEN ff.data_type IN ('G', 'DBCLOB') THEN 1200
                 WHEN ff.data_type IN ('H', 'BLOB') THEN 65535 END AS integer) AS ccsid,
       CAST(o.library AS varchar(128)) AS table_schema,
       CAST(CASE WHEN ff.dft = '"Null"' THEN 'NULL'
                 WHEN ff.dft LIKE '{"Value":%' THEN ff.dft::jsonb ->> 'Value' END AS varchar(2000)) AS column_default,
       CAST(CASE WHEN ff.data_type IN ('A', 'H', 'G', 'CLOB', 'BLOB', 'DBCLOB', 'XML') THEN ff.length END AS integer)
            AS character_maximum_length,
       CAST(CASE WHEN ff.data_type IN ('A', 'H', 'CLOB', 'BLOB', 'XML') THEN ff.length
                 WHEN ff.data_type IN ('G', 'DBCLOB') THEN 2 * ff.length END AS integer) AS character_octet_length,
       CAST(CASE WHEN ff.data_type = 'F' THEN 2
                 WHEN ff.data_type IN ('P', 'S', 'B', 'DECFLOAT') THEN 10 END AS integer) AS numeric_precision_radix,
       CAST(CASE WHEN ff.data_type IN ('L', 'T') THEN 0
                 WHEN ff.data_type = 'Z' THEN greatest(ff.length - 20, 0) END AS integer) AS datetime_precision,
       CAST(nullif(ff.text, '') AS varchar(50)) AS column_text,
       CAST(ff.name AS char(10)) AS system_column_name,
       CAST(o.name AS char(10)) AS system_table_name,
       CAST(o.library AS char(10)) AS system_table_schema,
       CAST(NULL AS varchar(128)) AS user_defined_type_schema,
       CAST(NULL AS varchar(128)) AS user_defined_type_name,
       CAST(CASE WHEN ff.identity IS NOT NULL THEN 'YES' ELSE 'NO' END AS varchar(3)) AS is_identity,
       CAST(CASE WHEN ff.identity IS NULL THEN NULL
                 WHEN ff.identity LIKE '%"always":true%' THEN 'ALWAYS' ELSE 'BY DEFAULT' END AS varchar(10))
            AS identity_generation,
       CAST(NULL AS decimal(31,0)) AS identity_start,
       CAST(NULL AS decimal(31,0)) AS identity_increment,
       CAST(NULL AS decimal(31,0)) AS identity_minimum,
       CAST(NULL AS decimal(31,0)) AS identity_maximum,
       CAST(NULL AS varchar(3)) AS identity_cycle,
       CAST(NULL AS integer) AS identity_cache,
       CAST(NULL AS varchar(3)) AS identity_order,
       CAST(NULL AS text) AS column_expression,
       CAST('N' AS char(1)) AS hidden,
       CAST('N' AS char(1)) AS has_fldproc
FROM as4tainr.file_fields ff
JOIN as4tainr.objects o ON o.library = ff.library AND o.name = ff.file AND o.objtype = '*FILE'
LEFT JOIN as4tainr.files f ON f.library = ff.library AND f.name = ff.file
WHERE coalesce(f.sql_type, '') <> 'INDEX';

-- SYSINDEXES: indexes made by SQL CREATE INDEX.
CREATE OR REPLACE VIEW qsys2.sysindexes AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) AS index_name,
       CAST(o.owner AS varchar(128)) AS index_owner,
       CAST(coalesce(pf.sql_name, lf.pf_name) AS varchar(128)) AS table_name,
       CAST(coalesce(po.owner, o.owner) AS varchar(128)) AS table_owner,
       CAST(lf.pf_library AS varchar(128)) AS table_schema,
       CAST(CASE WHEN f.is_unique THEN 'U' ELSE 'D' END AS char(1)) AS is_unique,
       CAST((SELECT count(*) FROM as4tainr.file_keys k
             WHERE k.library = o.library AND k.file = o.name) AS integer) AS column_count,
       CAST(o.library AS varchar(128)) AS index_schema,
       CAST(o.name AS char(10)) AS system_index_name,
       CAST(o.library AS char(10)) AS system_index_schema,
       CAST(lf.pf_name AS char(10)) AS system_table_name,
       CAST(lf.pf_library AS char(10)) AS system_table_schema,
       CAST(NULL AS varchar(2000)) AS long_comment,
       CAST(0 AS smallint) AS iasp_number,
       CAST(nullif(o.text, '') AS varchar(50)) AS index_text,
       CAST(NULL AS varchar(3)) AS is_spanning_index,
       CAST(o.owner AS varchar(128)) AS index_definer,
       CAST(NULL AS char(1)) AS rounding_mode,
       CAST('N' AS char(1)) AS index_has_search_condition,
       CAST('N' AS char(1)) AS search_condition_has_udf,
       CAST(NULL AS text) AS search_condition,
       CAST('N' AS char(1)) AS index_has_include_expression,
       CAST(NULL AS text) AS include_expression,
       CAST(o.created AS timestamp) AS created
FROM as4tainr.objects o
JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name AND f.sql_type = 'INDEX'
JOIN as4tainr.logical_files lf ON lf.library = o.library AND lf.name = o.name
LEFT JOIN as4tainr.files pf ON pf.library = lf.pf_library AND pf.name = lf.pf_name
LEFT JOIN as4tainr.objects po ON po.library = lf.pf_library AND po.name = lf.pf_name AND po.objtype = '*FILE'
WHERE o.objtype = '*FILE';

-- SYSKEYS: the key columns of the indexes in SYSINDEXES.
CREATE OR REPLACE VIEW qsys2.syskeys AS
SELECT CAST(coalesce(f.sql_name, k.file) AS varchar(128)) AS index_name,
       CAST(o.owner AS varchar(128)) AS index_owner,
       CAST(coalesce(pff.alias, k.field) AS varchar(128)) AS column_name,
       CAST(pff.seq + 1 AS integer) AS column_position,
       CAST(k.seq + 1 AS integer) AS ordinal_position,
       CAST(CASE WHEN k.descending THEN 'D' ELSE 'A' END AS char(1)) AS ordering,
       CAST(k.library AS varchar(128)) AS index_schema,
       CAST(k.field AS char(10)) AS system_column_name,
       CAST(k.file AS char(10)) AS system_index_name,
       CAST(k.library AS char(10)) AS system_index_schema,
       CAST('N' AS char(1)) AS column_is_expression,
       CAST(NULL AS char(1)) AS expression_has_udf,
       CAST(NULL AS text) AS key_expression
FROM as4tainr.file_keys k
JOIN as4tainr.files f ON f.library = k.library AND f.name = k.file AND f.sql_type = 'INDEX'
JOIN as4tainr.objects o ON o.library = k.library AND o.name = k.file AND o.objtype = '*FILE'
JOIN as4tainr.logical_files lf ON lf.library = k.library AND lf.name = k.file
LEFT JOIN as4tainr.file_fields pff ON pff.library = lf.pf_library AND pff.file = lf.pf_name AND pff.name = k.field;

-- SYSVIEWS: SQL views and their queries.
CREATE OR REPLACE VIEW qsys2.sysviews AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) AS table_name,
       CAST(o.owner AS varchar(128)) AS view_owner,
       CAST(1 AS integer) AS seqno,
       CAST(CASE WHEN f.view_def::jsonb ->> 'check_option' = 'true' THEN 'C' ELSE 'N' END AS char(1)) AS check_option,
       CAST(f.view_def::jsonb ->> 'query' AS varchar(5000)) AS view_definition,
       CAST('N' AS char(1)) AS is_updatable,
       CAST(o.library AS varchar(128)) AS table_schema,
       CAST(o.name AS char(10)) AS system_view_name,
       CAST(o.library AS char(10)) AS system_view_schema,
       CAST('NO' AS varchar(3)) AS is_insertable_into,
       CAST(0 AS smallint) AS iasp_number,
       CAST('N' AS char(1)) AS is_deletable,
       CAST(o.owner AS varchar(128)) AS view_definer,
       CAST(NULL AS char(1)) AS rounding_mode
FROM as4tainr.objects o
JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name AND f.view IS NOT NULL
WHERE o.objtype = '*FILE';
"#;

/// QSYS2.OBJECT_STATISTICS's result columns, in IBM's order: (name, type,
/// value). Values as4tainr does not keep are the null value or the
/// documented "no" value.
const OBJECT_STATISTICS_COLUMNS: &[(&str, &str, &str)] = &[
    ("objname", "varchar(10)", "o.name"),
    ("objtype", "varchar(8)", "o.objtype"),
    ("objowner", "varchar(10)", "o.owner"),
    ("objdefiner", "varchar(10)", "o.owner"),
    ("objcreated", "timestamp", "o.created"),
    ("objsize", "decimal(15,0)", "o.size"),
    ("objtext", "varchar(50)", "o.text"),
    (
        "objlongname",
        "varchar(128)",
        "coalesce(o.sql_name, o.name)",
    ),
    ("last_used_timestamp", "timestamp", "NULL"),
    ("last_used_object", "varchar(4)", "'NO'"),
    ("days_used_count", "integer", "0"),
    ("last_reset_timestamp", "timestamp", "NULL"),
    ("iasp_number", "smallint", "0"),
    ("iasp_name", "varchar(10)", "'*SYSBAS'"),
    ("objattribute", "varchar(10)", "nullif(o.attribute, '')"),
    ("objlongschema", "varchar(128)", "o.library"),
    ("text", "varchar(50)", "o.text"),
    ("sql_object_type", "varchar(9)", "o.sql_object_type"),
    ("objlib", "varchar(10)", "o.library"),
    ("change_timestamp", "timestamp", "o.created"),
    ("user_changed", "varchar(3)", "'NO'"),
    ("source_file", "varchar(10)", "NULL"),
    ("source_library", "varchar(10)", "NULL"),
    ("source_member", "varchar(10)", "NULL"),
    ("source_timestamp", "timestamp", "NULL"),
    ("created_system", "varchar(8)", "NULL"),
    ("created_system_version", "varchar(9)", "NULL"),
    ("licensed_program", "varchar(7)", "NULL"),
    ("licensed_program_version", "varchar(9)", "NULL"),
    ("compiler", "varchar(7)", "NULL"),
    ("compiler_version", "varchar(9)", "NULL"),
    ("object_control_level", "char(8)", "NULL"),
    ("ptf_number", "char(7)", "NULL"),
    ("apar_id", "char(6)", "NULL"),
    ("user_defined_attribute", "varchar(10)", "NULL"),
    ("allow_change_by_program", "varchar(3)", "'NO'"),
    ("changed_by_program", "varchar(3)", "'NO'"),
    ("compressed", "varchar(4)", "'NO'"),
    ("primary_group", "varchar(10)", "'*NONE'"),
    ("storage_freed", "varchar(3)", "'NO'"),
    ("associated_space_size", "integer", "NULL"),
    ("optimum_space_alignment", "varchar(3)", "NULL"),
    ("overflow_storage", "varchar(3)", "'NO'"),
    ("object_domain", "varchar(7)", "'*SYSTEM'"),
    ("object_audit", "varchar(10)", "'*NONE'"),
    ("object_signed", "varchar(3)", "'NO'"),
    ("system_trusted_source", "varchar(3)", "'NO'"),
    ("multiple_signatures", "varchar(3)", "'NO'"),
    ("save_timestamp", "timestamp", "NULL"),
    ("restore_timestamp", "timestamp", "NULL"),
    ("save_while_active_timestamp", "timestamp", "NULL"),
    ("save_command", "varchar(10)", "NULL"),
    ("save_device", "varchar(5)", "NULL"),
    ("save_file_name", "varchar(10)", "NULL"),
    ("save_file_library", "varchar(10)", "NULL"),
    ("save_volume", "varchar(71)", "NULL"),
    ("save_label", "varchar(17)", "NULL"),
    ("save_sequence_number", "decimal(10,0)", "NULL"),
    ("last_save_size", "decimal(15,0)", "NULL"),
    ("journaled", "varchar(3)", "'NO'"),
    ("journal_name", "varchar(10)", "NULL"),
    ("journal_library", "varchar(10)", "NULL"),
    ("journal_images", "varchar(6)", "NULL"),
    ("omit_journal_entry", "varchar(7)", "NULL"),
    ("remote_journal_filter", "varchar(3)", "NULL"),
    ("journal_start_timestamp", "timestamp", "NULL"),
    ("apply_starting_receiver", "varchar(10)", "NULL"),
    ("apply_starting_receiver_library", "varchar(10)", "NULL"),
    ("authority_collection_value", "varchar(10)", "'*NONE'"),
];

/// QSYS2.OBJECT_STATISTICS(OBJECT_SCHEMA, OBJTYPELIST [, OBJECT_NAME]).
/// The library list special values use the caller's search path (the
/// job's library list), so this runs with the caller's rights.
fn object_statistics() -> String {
    let returns: Vec<String> = OBJECT_STATISTICS_COLUMNS
        .iter()
        .map(|(n, t, _)| format!("{n} {t}"))
        .collect();
    let values: Vec<String> = OBJECT_STATISTICS_COLUMNS
        .iter()
        .map(|(_, t, e)| format!("CAST({e} AS {t})"))
        .collect();
    format!(
        "DROP FUNCTION IF EXISTS qsys2.object_statistics(varchar, varchar, varchar);
         CREATE FUNCTION qsys2.object_statistics(object_schema varchar, objtypelist varchar,
                                                 object_name varchar DEFAULT '*ALL')
         RETURNS TABLE ({})
         LANGUAGE sql STABLE AS $as4tainr$
         WITH arg AS (
           SELECT upper(trim(object_schema)) AS sch,
                  upper(trim(coalesce(object_name, '*ALL'))) AS nam,
                  ARRAY(SELECT '*' || ltrim(t, '*')
                        FROM regexp_split_to_table(upper(trim(objtypelist)), '[ ,]+') AS t
                        WHERE t <> '') AS types
         ),
         libl AS (
           SELECT upper(s) AS lib, n
           FROM unnest(current_schemas(false)) WITH ORDINALITY AS x(s, n)
         )
         SELECT {}
         FROM as4tainr_svc.objects o
         CROSS JOIN arg
         LEFT JOIN libl ON libl.lib = o.library
         WHERE ('*ALL' = ANY (arg.types) OR o.objtype = ANY (arg.types))
           AND CASE
                 WHEN arg.sch IN ('*ALL', '*ALLAVL', '*ALLSIMPLE') THEN true
                 WHEN arg.sch IN ('*ALLUSR', '*ALLUSRAVL') THEN
                   o.library NOT LIKE 'Q%' OR o.library IN ('QGPL', 'QUSRSYS')
                 WHEN arg.sch IN ('*LIBL', '*USRLIBL', '*CURLIB') THEN libl.lib IS NOT NULL
                 ELSE o.library = arg.sch
               END
           AND (arg.nam IN ('*ALL', '*ALLSIMPLE')
                OR (right(arg.nam, 1) = '*'
                    AND o.name LIKE replace(replace(rtrim(arg.nam, '*'), '%', '\\%'), '_', '\\_') || '%')
                OR o.name = arg.nam OR o.sql_name = arg.nam)
         ORDER BY libl.n, o.library, o.name, o.objtype
         $as4tainr$;",
        returns.join(", "),
        values.join(", ")
    )
}

/// Create (or replace) the services. Called from `sql::init`, after the
/// SQL login exists.
pub async fn init(client: &Client) -> Result<()> {
    // Made again only when this version's definitions are not there yet
    // (replacing views locks them against the jobs reading them).
    let have: Option<String> = client
        .query_opt(
            "SELECT obj_description(oid, 'pg_namespace') FROM pg_namespace WHERE nspname = 'as4tainr_svc'",
            &[],
        )
        .await?
        .and_then(|r| r.get(0));
    if have.as_deref() == Some(VERSION) {
        return Ok(());
    }
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS as4tainr_svc;
             REVOKE ALL ON SCHEMA as4tainr_svc FROM PUBLIC;
             {FIELD_BYTES}
             {SQL_TYPE}
             {DDL}
             {}
             GRANT USAGE ON SCHEMA as4tainr_svc TO {SQL_ROLE};
             GRANT SELECT ON as4tainr_svc.objects TO {SQL_ROLE};
             GRANT USAGE ON SCHEMA qsys2 TO {SQL_ROLE};
             GRANT SELECT ON qsys2.sysschemas, qsys2.systables, qsys2.syscolumns,
                   qsys2.sysindexes, qsys2.syskeys, qsys2.sysviews TO {SQL_ROLE};
             GRANT EXECUTE ON FUNCTION qsys2.object_statistics(varchar, varchar, varchar) TO {SQL_ROLE};
             COMMENT ON SCHEMA as4tainr_svc IS '{VERSION}';",
            object_statistics()
        ))
        .await
        .context("creating the QSYS2 services")?;
    Ok(())
}
