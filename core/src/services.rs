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
// 9: character columns of the catalog views in the "C" collation, so
// ORDER BY sorts as Db2 for i's *HEX sort sequence does ('*PUBLIC' before
// names), whatever the database's default collation is.
// 10-12: Db2 scalar functions CHAR, DIGITS, TIMESTAMP, VARCHAR_FORMAT (sqlfn.rs).
const VERSION: &str = "as4tainr services 33";

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
            ELSE 0 END AS size,
       o.changed
FROM as4tainr.objects o
LEFT JOIN as4tainr.files f ON o.objtype = '*FILE' AND f.library = o.library AND f.name = o.name;

-- SYSSCHEMAS: one row per library.
CREATE OR REPLACE VIEW qsys2.sysschemas AS
SELECT CAST(o.name AS varchar(128)) COLLATE "C" AS schema_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS schema_owner,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS schema_creator,
       CAST(o.created AS timestamp) AS creation_timestamp,
       CAST(coalesce((SELECT sum(s.size) FROM as4tainr_svc.objects s WHERE s.library = o.name), 0)
            AS decimal(15,0)) AS schema_size,
       CAST(nullif(o.text, '') AS varchar(50)) COLLATE "C" AS schema_text,
       CAST(o.name AS char(10)) COLLATE "C" AS system_schema_name,
       CAST(0 AS smallint) AS iasp_number
FROM as4tainr_svc.objects o
WHERE o.library = 'QSYS' AND o.objtype = '*LIB';

-- SYSTABLES: tables, physical files, views and logical files (not SQL
-- indexes, which are in SYSINDEXES).
CREATE OR REPLACE VIEW qsys2.systables AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) COLLATE "C" AS table_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS table_owner,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'V'
                 WHEN f.sql_type = 'TABLE' THEN 'T'
                 WHEN o.attribute = 'LF' THEN 'L'
                 ELSE 'P' END AS char(1)) COLLATE "C" AS table_type,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 3
                 ELSE (SELECT count(*) FROM as4tainr.file_fields ff
                       WHERE ff.library = o.library AND ff.file = o.name) END AS integer) AS column_count,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 92
                 ELSE coalesce((SELECT sum(as4tainr_svc.field_bytes(ff.data_type, ff.length, ff.decimals, ff.flt_double, ff.varlen))
                                FROM as4tainr.file_fields ff
                                WHERE ff.library = o.library AND ff.file = o.name), 0) END AS integer) AS row_length,
       CAST(nullif(o.text, '') AS varchar(50)) COLLATE "C" AS table_text,
       CAST(NULL AS varchar(2000)) COLLATE "C" AS long_comment,
       CAST(o.library AS varchar(128)) COLLATE "C" AS table_schema,
       CAST(o.created AS timestamp) AS last_altered_timestamp,
       CAST(o.name AS char(10)) COLLATE "C" AS system_table_name,
       CAST(o.library AS char(10)) COLLATE "C" AS system_table_schema,
       CAST(CASE WHEN o.attribute = 'PF-SRC' THEN 'S' ELSE 'D' END AS char(1)) COLLATE "C" AS file_type,
       CAST(NULL AS varchar(18)) COLLATE "C" AS base_table_catalog,
       CAST(NULL AS varchar(128)) COLLATE "C" AS base_table_schema,
       CAST(NULL AS varchar(128)) COLLATE "C" AS base_table_name,
       CAST(NULL AS varchar(10)) COLLATE "C" AS base_table_member,
       CAST(CASE WHEN o.library IN ('QSYS', 'QSYS2') THEN 'Y' ELSE 'N' END AS char(1)) COLLATE "C" AS system_table,
       CAST(CASE WHEN o.attribute = 'LF' AND EXISTS (
                   SELECT 1 FROM as4tainr.logical_files lf
                   WHERE lf.library = o.library AND lf.name = o.name AND lf.selection <> 'TRUE')
                 THEN 'Y' ELSE 'N' END AS char(1)) COLLATE "C" AS select_omit,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'NO' ELSE 'YES' END AS varchar(3)) COLLATE "C" AS is_insertable_into,
       CAST(0 AS smallint) AS iasp_number,
       CAST(NULL AS varchar(3)) COLLATE "C" AS enabled,
       CAST(NULL AS varchar(6)) COLLATE "C" AS maintenance,
       CAST(NULL AS varchar(9)) COLLATE "C" AS refresh,
       CAST(NULL AS timestamp) AS refresh_time,
       CAST(NULL AS text) AS mqt_definition,
       CAST(NULL AS char(2)) COLLATE "C" AS isolation,
       CAST('NO' AS varchar(11)) COLLATE "C" AS partition_table,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS table_definer,
       CAST(NULL AS char(1)) COLLATE "C" AS mqt_restore_deferred,
       CAST(NULL AS char(1)) COLLATE "C" AS rounding_mode,
       CAST(' ' AS char(1)) COLLATE "C" AS control
FROM as4tainr.objects o
LEFT JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name
WHERE o.objtype = '*FILE' AND o.attribute IN ('PF', 'LF', 'PF-SRC')
  AND coalesce(f.sql_type, '') <> 'INDEX';

-- SYSCOLUMNS: the columns of tables, physical and logical files and views.
CREATE OR REPLACE VIEW qsys2.syscolumns AS
SELECT CAST(coalesce(ff.alias, ff.name) AS varchar(128)) COLLATE "C" AS column_name,
       CAST(coalesce(f.sql_name, o.name) AS varchar(128)) COLLATE "C" AS table_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS table_owner,
       CAST(ff.seq + 1 AS integer) AS ordinal_position,
       CAST(as4tainr_svc.sql_type(ff.data_type, ff.length, ff.decimals, ff.varlen) AS varchar(8)) COLLATE "C" AS data_type,
       CAST(CASE WHEN ff.data_type IN ('P', 'S') OR (ff.data_type = 'B' AND ff.decimals > 0) THEN ff.length
                 WHEN ff.data_type IN ('B', 'F', 'DECFLOAT')
                 THEN as4tainr_svc.field_bytes(ff.data_type, ff.length, ff.decimals, ff.flt_double, ff.varlen)
                 WHEN ff.data_type = 'L' THEN 4
                 WHEN ff.data_type = 'T' THEN 3
                 WHEN ff.data_type = 'Z' THEN 10
                 ELSE ff.length END AS integer) AS length,
       CAST(CASE WHEN ff.data_type IN ('P', 'S', 'B') THEN ff.decimals END AS integer) AS numeric_scale,
       CAST(CASE WHEN ff.null_capable THEN 'Y' ELSE 'N' END AS char(1)) COLLATE "C" AS is_nullable,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'N' ELSE 'Y' END AS char(1)) COLLATE "C" AS is_updatable,
       CAST(NULL AS varchar(2000)) COLLATE "C" AS long_comment,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'N'
                 WHEN ff.identity IS NOT NULL THEN
                   CASE WHEN ff.identity LIKE '%"always":true%' THEN 'I' ELSE 'J' END
                 -- No default only for NOT NULL without DEFAULT; a DDS
                 -- field without DFT gets blanks or zero.
                 WHEN ff.dft = '"Required"' THEN 'N'
                 ELSE 'Y' END AS char(1)) COLLATE "C" AS has_default,
       CAST(NULL AS varchar(60)) COLLATE "C" AS column_heading,
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
       CAST(o.library AS varchar(128)) COLLATE "C" AS table_schema,
       CAST(CASE WHEN ff.dft = '"Null"' THEN 'NULL'
                 WHEN ff.dft = '"User"' THEN 'USER'
                 WHEN ff.dft LIKE '{"Value":%' THEN ff.dft::jsonb ->> 'Value' END AS varchar(2000)) COLLATE "C" AS column_default,
       CAST(CASE WHEN ff.data_type IN ('A', 'H', 'G', 'CLOB', 'BLOB', 'DBCLOB', 'XML') THEN ff.length END AS integer)
            AS character_maximum_length,
       CAST(CASE WHEN ff.data_type IN ('A', 'H', 'CLOB', 'BLOB', 'XML') THEN ff.length
                 WHEN ff.data_type IN ('G', 'DBCLOB') THEN 2 * ff.length END AS integer) AS character_octet_length,
       CAST(CASE WHEN ff.data_type = 'F' THEN 2
                 WHEN ff.data_type IN ('P', 'S', 'B', 'DECFLOAT') THEN 10 END AS integer) AS numeric_precision_radix,
       CAST(CASE WHEN ff.data_type IN ('L', 'T') THEN 0
                 WHEN ff.data_type = 'Z' THEN greatest(ff.length - 20, 0) END AS integer) AS datetime_precision,
       CAST(nullif(ff.text, '') AS varchar(50)) COLLATE "C" AS column_text,
       CAST(ff.name AS char(10)) COLLATE "C" AS system_column_name,
       CAST(o.name AS char(10)) COLLATE "C" AS system_table_name,
       CAST(o.library AS char(10)) COLLATE "C" AS system_table_schema,
       CAST(NULL AS varchar(128)) COLLATE "C" AS user_defined_type_schema,
       CAST(NULL AS varchar(128)) COLLATE "C" AS user_defined_type_name,
       CAST(CASE WHEN ff.identity IS NOT NULL THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS is_identity,
       CAST(CASE WHEN ff.identity IS NULL THEN NULL
                 WHEN ff.identity LIKE '%"always":true%' THEN 'ALWAYS' ELSE 'BY DEFAULT' END AS varchar(10))
            AS identity_generation,
       CAST(NULL AS decimal(31,0)) AS identity_start,
       CAST(NULL AS decimal(31,0)) AS identity_increment,
       CAST(NULL AS decimal(31,0)) AS identity_minimum,
       CAST(NULL AS decimal(31,0)) AS identity_maximum,
       CAST(NULL AS varchar(3)) COLLATE "C" AS identity_cycle,
       CAST(NULL AS integer) AS identity_cache,
       CAST(NULL AS varchar(3)) COLLATE "C" AS identity_order,
       CAST(NULL AS text) AS column_expression,
       CAST('N' AS char(1)) COLLATE "C" AS hidden,
       CAST('N' AS char(1)) COLLATE "C" AS has_fldproc
FROM as4tainr.file_fields ff
JOIN as4tainr.objects o ON o.library = ff.library AND o.name = ff.file AND o.objtype = '*FILE'
LEFT JOIN as4tainr.files f ON f.library = ff.library AND f.name = ff.file
WHERE coalesce(f.sql_type, '') <> 'INDEX';

-- SYSINDEXES: indexes made by SQL CREATE INDEX.
CREATE OR REPLACE VIEW qsys2.sysindexes AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) COLLATE "C" AS index_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS index_owner,
       CAST(coalesce(pf.sql_name, lf.pf_name) AS varchar(128)) COLLATE "C" AS table_name,
       CAST(coalesce(po.owner, o.owner) AS varchar(128)) COLLATE "C" AS table_owner,
       CAST(lf.pf_library AS varchar(128)) COLLATE "C" AS table_schema,
       CAST(CASE WHEN f.is_unique THEN 'U' ELSE 'D' END AS char(1)) COLLATE "C" AS is_unique,
       CAST((SELECT count(*) FROM as4tainr.file_keys k
             WHERE k.library = o.library AND k.file = o.name) AS integer) AS column_count,
       CAST(o.library AS varchar(128)) COLLATE "C" AS index_schema,
       CAST(o.name AS char(10)) COLLATE "C" AS system_index_name,
       CAST(o.library AS char(10)) COLLATE "C" AS system_index_schema,
       CAST(lf.pf_name AS char(10)) COLLATE "C" AS system_table_name,
       CAST(lf.pf_library AS char(10)) COLLATE "C" AS system_table_schema,
       CAST(NULL AS varchar(2000)) COLLATE "C" AS long_comment,
       CAST(0 AS smallint) AS iasp_number,
       CAST(nullif(o.text, '') AS varchar(50)) COLLATE "C" AS index_text,
       CAST(NULL AS varchar(3)) COLLATE "C" AS is_spanning_index,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS index_definer,
       CAST(NULL AS char(1)) COLLATE "C" AS rounding_mode,
       CAST('N' AS char(1)) COLLATE "C" AS index_has_search_condition,
       CAST('N' AS char(1)) COLLATE "C" AS search_condition_has_udf,
       CAST(NULL AS text) AS search_condition,
       CAST('N' AS char(1)) COLLATE "C" AS index_has_include_expression,
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
SELECT CAST(coalesce(f.sql_name, k.file) AS varchar(128)) COLLATE "C" AS index_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS index_owner,
       CAST(coalesce(pff.alias, k.field) AS varchar(128)) COLLATE "C" AS column_name,
       CAST(pff.seq + 1 AS integer) AS column_position,
       CAST(k.seq + 1 AS integer) AS ordinal_position,
       CAST(CASE WHEN k.descending THEN 'D' ELSE 'A' END AS char(1)) COLLATE "C" AS ordering,
       CAST(k.library AS varchar(128)) COLLATE "C" AS index_schema,
       CAST(k.field AS char(10)) COLLATE "C" AS system_column_name,
       CAST(k.file AS char(10)) COLLATE "C" AS system_index_name,
       CAST(k.library AS char(10)) COLLATE "C" AS system_index_schema,
       CAST('N' AS char(1)) COLLATE "C" AS column_is_expression,
       CAST(NULL AS char(1)) COLLATE "C" AS expression_has_udf,
       CAST(NULL AS text) AS key_expression
FROM as4tainr.file_keys k
JOIN as4tainr.files f ON f.library = k.library AND f.name = k.file AND f.sql_type = 'INDEX'
JOIN as4tainr.objects o ON o.library = k.library AND o.name = k.file AND o.objtype = '*FILE'
JOIN as4tainr.logical_files lf ON lf.library = k.library AND lf.name = k.file
LEFT JOIN as4tainr.file_fields pff ON pff.library = lf.pf_library AND pff.file = lf.pf_name AND pff.name = k.field;

-- SYSVIEWS: SQL views and their queries.
CREATE OR REPLACE VIEW qsys2.sysviews AS
SELECT CAST(coalesce(f.sql_name, o.name) AS varchar(128)) COLLATE "C" AS table_name,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS view_owner,
       CAST(1 AS integer) AS seqno,
       CAST(CASE WHEN f.view_def::jsonb ->> 'check_option' = 'true' THEN 'C' ELSE 'N' END AS char(1)) COLLATE "C" AS check_option,
       CAST(f.view_def::jsonb ->> 'query' AS varchar(5000)) COLLATE "C" AS view_definition,
       CAST('N' AS char(1)) COLLATE "C" AS is_updatable,
       CAST(o.library AS varchar(128)) COLLATE "C" AS table_schema,
       CAST(o.name AS char(10)) COLLATE "C" AS system_view_name,
       CAST(o.library AS char(10)) COLLATE "C" AS system_view_schema,
       CAST('NO' AS varchar(3)) COLLATE "C" AS is_insertable_into,
       CAST(0 AS smallint) AS iasp_number,
       CAST('N' AS char(1)) COLLATE "C" AS is_deletable,
       CAST(o.owner AS varchar(128)) COLLATE "C" AS view_definer,
       CAST(NULL AS char(1)) COLLATE "C" AS rounding_mode
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
    ("change_timestamp", "timestamp", "o.changed"),
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

/// Drop the views of an earlier version: CREATE OR REPLACE VIEW cannot
/// change a column's collation or type. A view that others depend on
/// (a user's view over a catalog view) is dropped once they are gone, or
/// kept and replaced as before.
async fn drop_views(client: &Client) {
    let Ok(rows) = client
        .query(
            "SELECT schemaname, viewname FROM pg_views WHERE schemaname IN ('qsys2', 'as4tainr_svc')",
            &[],
        )
        .await
    else {
        return;
    };
    let mut left: Vec<(String, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    // Views over views: drop the ones nothing depends on, then again.
    loop {
        let before = left.len();
        let mut still = Vec::new();
        for (schema, view) in left {
            let sql = format!(
                "DROP VIEW IF EXISTS {}.{}",
                crate::catalog::ident(&schema),
                crate::catalog::ident(&view)
            );
            if client.batch_execute(&sql).await.is_err() {
                still.push((schema, view));
            }
        }
        left = still;
        if left.is_empty() || left.len() == before {
            break;
        }
    }
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
    if have.is_some() {
        drop_views(client).await;
    }
    // The views' character columns are in the order of the files'
    // (EBCDIC: collate.rs), so they compare with them.
    let coll = crate::collate::clause(client).await?;
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
             {SPOOLED_FILE_DATA}
             GRANT USAGE ON SCHEMA systools TO {SQL_ROLE};
             GRANT EXECUTE ON FUNCTION systools.spooled_file_data(varchar, varchar, integer) TO {SQL_ROLE};
             GRANT SELECT ON qsys2.sysschemas, qsys2.systables, qsys2.syscolumns,
                   qsys2.sysindexes, qsys2.syskeys, qsys2.sysviews TO {SQL_ROLE};
             GRANT EXECUTE ON FUNCTION qsys2.object_statistics(varchar, varchar, varchar) TO {SQL_ROLE};
             {}
             {}
             COMMENT ON SCHEMA as4tainr_svc IS '{VERSION}';",
            object_statistics(),
            work_services(),
            crate::journal::sql_functions() + &crate::pftrg::sql_functions() + &crate::sqlcat::sql() + &crate::sqlfn::sql()
        ).replace(" COLLATE \"C\"", &coll))
        .await
        .context("creating the QSYS2 services")?;
    Ok(())
}

// ---------------------------------------------------------------------
// Work management, security, system values, routines and the IFS.

/// One result column of a service: (name, type, value).
type Col = (&'static str, &'static str, &'static str);

/// `name type, ...` and `CAST(value AS type), ...` for a column list.
fn col_lists(cols: &[Col]) -> (String, String) {
    let defs: Vec<String> = cols.iter().map(|(n, t, _)| format!("{n} {t}")).collect();
    let vals: Vec<String> = cols
        .iter()
        .map(|(n, t, e)| format!("CAST({e} AS {t}) AS {n}"))
        .collect();
    (defs.join(", "), vals.join(", "))
}

/// QSYS2.LIBRARY_LIST_INFO (IBM i services: LIBRARY_LIST_INFO view): the
/// statement's library list, from the SQL session's search path.
fn library_list_info() -> String {
    let system: Vec<String> = crate::commands::SYSTEM_LIBRARY_LIST
        .iter()
        .map(|l| format!("'{l}'"))
        .collect();
    format!(
        "CREATE OR REPLACE VIEW qsys2.library_list_info AS
         SELECT CAST(x.n AS integer) AS ordinal_position,
                CAST(upper(x.s) AS varchar(128)) COLLATE \"C\" AS schema_name,
                CAST(upper(x.s) AS varchar(10)) COLLATE \"C\" AS system_schema_name,
                CAST(CASE WHEN upper(x.s) IN ({sys}) THEN 'SYSTEM'
                          WHEN upper(x.s) = current_setting('as4tainr.curlib', true) THEN 'CURRENT'
                          ELSE 'USER' END AS varchar(15)) COLLATE \"C\" AS type,
                CAST(0 AS smallint) AS iasp_number,
                CAST(nullif(o.text, '') AS varchar(50)) COLLATE \"C\" AS text_description
         FROM unnest(current_schemas(false)) WITH ORDINALITY AS x(s, n)
         LEFT JOIN as4tainr.objects o
           ON o.library = 'QSYS' AND o.objtype = '*LIB' AND o.name = upper(x.s)
         WHERE x.s <> 'as4tainr_cast';",
        sys = system.join(", ")
    )
}

/// QSYS2.USER_INFO (IBM i services: USER_INFO view) over the user
/// profiles. Values as4tainr does not keep are the null value.
const USER_INFO: &[Col] = &[
    ("authorization_name", "varchar(10)", "u.name"),
    ("previous_signon", "timestamp", "u.last_signon"),
    ("sign_on_attempts_not_valid", "integer", "0"),
    ("status", "varchar(10)", "u.status"),
    ("netserver_disabled", "varchar(3)", "'NO'"),
    ("password_change_date", "timestamp", "NULL"),
    ("no_password_indicator", "varchar(3)", "CASE WHEN u.password_hash = '*NONE' THEN 'YES' ELSE 'NO' END"),
    ("password_level_0_1", "varchar(3)", "'NO'"),
    ("password_level_2_3", "varchar(3)", "'YES'"),
    ("password_expiration_interval", "smallint", "0"),
    ("date_password_expires", "timestamp", "NULL"),
    ("days_until_password_expires", "integer", "NULL"),
    ("set_password_to_expire", "varchar(3)", "CASE WHEN u.pwd_expired THEN 'YES' ELSE 'NO' END"),
    ("user_class_name", "varchar(10)", "u.usrcls"),
    ("special_authorities", "varchar(88)", "nullif(rtrim(array_to_string(ARRAY(SELECT rpad(a, 11) FROM unnest(u.special_auth) AS a), '')), '')"),
    ("group_profile_name", "varchar(10)", "'*NONE'"),
    ("supplemental_group_count", "smallint", "0"),
    ("supplemental_group_list", "varchar(150)", "NULL"),
    ("owner", "varchar(10)", "'*USRPRF'"),
    ("group_authority", "varchar(10)", "'*NONE'"),
    ("assistance_level", "varchar(10)", "'*SYSVAL'"),
    ("current_library_name", "varchar(10)", "u.curlib"),
    ("initial_menu_name", "varchar(10)", "CASE WHEN position('/' IN u.inlmnu) > 0 THEN split_part(u.inlmnu, '/', 2) ELSE u.inlmnu END"),
    ("initial_menu_library_name", "varchar(10)", "CASE WHEN position('/' IN u.inlmnu) > 0 THEN split_part(u.inlmnu, '/', 1) WHEN u.inlmnu = '*SIGNOFF' THEN NULL ELSE '*LIBL' END"),
    ("initial_program_name", "varchar(10)", "CASE WHEN position('/' IN u.inlpgm) > 0 THEN split_part(u.inlpgm, '/', 2) ELSE u.inlpgm END"),
    ("initial_program_library_name", "varchar(10)", "CASE WHEN position('/' IN u.inlpgm) > 0 THEN split_part(u.inlpgm, '/', 1) WHEN u.inlpgm = '*NONE' THEN NULL ELSE '*LIBL' END"),
    ("limit_capabilities", "varchar(10)", "'*NO'"),
    ("text_description", "varchar(50)", "nullif(u.text, '')"),
    ("display_signon_information", "varchar(10)", "'*SYSVAL'"),
    ("limit_device_sessions", "varchar(10)", "'*SYSVAL'"),
    ("keyboard_buffering", "varchar(10)", "'*SYSVAL'"),
    ("maximum_allowed_storage", "bigint", "-1"),
    ("storage_used", "bigint", "0"),
    ("highest_scheduling_priority", "char(1)", "'3'"),
    ("job_description_name", "varchar(10)", "'QDFTJOBD'"),
    ("job_description_library_name", "varchar(10)", "'QGPL'"),
    ("accounting_code", "varchar(15)", "NULL"),
    ("message_queue_name", "varchar(10)", "u.name"),
    ("message_queue_library_name", "varchar(10)", "'QUSRSYS'"),
    ("message_queue_delivery_method", "varchar(10)", "'*NOTIFY'"),
    ("message_queue_severity", "smallint", "0"),
    ("output_queue_name", "varchar(10)", "'*WRKSTN'"),
    ("output_queue_library_name", "varchar(10)", "NULL"),
    ("print_device", "varchar(10)", "'*WRKSTN'"),
    ("special_environment", "varchar(10)", "'*SYSVAL'"),
    ("attention_key_handling_program_name", "varchar(10)", "'*SYSVAL'"),
    ("attention_key_handling_program_library_name", "varchar(10)", "NULL"),
    ("language_id", "varchar(10)", "'*SYSVAL'"),
    ("country_or_region_id", "varchar(10)", "'*SYSVAL'"),
    ("character_code_set_id", "varchar(6)", "'*SYSVAL'"),
    ("user_options", "varchar(77)", "NULL"),
    ("sort_sequence_table_name", "varchar(10)", "'*SYSVAL'"),
    ("sort_sequence_table_library_name", "varchar(10)", "NULL"),
    ("object_auditing_value", "varchar(10)", "'*NONE'"),
    ("user_action_audit_level", "varchar(363)", "NULL"),
    ("group_authority_type", "varchar(10)", "'*PRIVATE'"),
    ("user_id_number", "bigint", "NULL"),
    ("group_id_number", "bigint", "0"),
    ("locale_job_attributes", "varchar(88)", "NULL"),
    ("group_member_indicator", "varchar(3)", "'NO'"),
    ("digital_certificate_indicator", "varchar(3)", "'NO'"),
    ("character_identifier_control", "varchar(10)", "'*SYSVAL'"),
    ("local_password_management", "varchar(3)", "'YES'"),
    ("block_password_change", "varchar(10)", "'*SYSVAL'"),
    ("user_entitlement_required", "varchar(3)", "'YES'"),
    ("user_expiration_interval", "smallint", "0"),
    ("user_expiration_date", "timestamp", "NULL"),
    ("user_expiration_action", "varchar(8)", "NULL"),
    ("home_directory", "varchar(1024)", "'/home/' || u.name"),
    ("locale_path_name", "varchar(1024)", "'*SYSVAL'"),
    ("user_default_password", "varchar(3)", "'NO'"),
    ("user_owner", "varchar(10)", "'QSECOFR'"),
    ("user_creator", "varchar(10)", "'QSECOFR'"),
    ("size", "decimal(15,0)", "NULL"),
    ("creation_timestamp", "timestamp", "u.created"),
    ("last_used_timestamp", "timestamp", "u.last_signon"),
    ("days_used_count", "integer", "NULL"),
    ("last_reset_timestamp", "timestamp", "NULL"),
    ("authority_collection_active", "varchar(3)", "'NO'"),
    ("authority_collection_repository_exists", "varchar(3)", "'NO'"),
    ("pase_shell_path", "varchar(1024)", "NULL"),
];

fn user_info() -> String {
    let (_, vals) = col_lists(USER_INFO);
    format!(
        "CREATE OR REPLACE VIEW qsys2.user_info AS SELECT {vals} FROM as4tainr.user_profiles u;"
    )
}

/// QSYS2.SYSTEM_VALUE_INFO (IBM i services): the system values RTVSYSVAL
/// knows; the date and time ones from the clock when read.
fn system_value_info() -> String {
    let clock = |name: &str| -> Option<&'static str> {
        Some(match name {
            "QDATE" => "to_char(localtimestamp, 'MMDDYY')",
            "QYEAR" => "to_char(localtimestamp, 'YY')",
            "QMONTH" => "to_char(localtimestamp, 'MM')",
            "QDAY" => "to_char(localtimestamp, 'DD')",
            "QCENTURY" => {
                "CASE WHEN extract(year FROM localtimestamp) >= 2000 THEN '1' ELSE '0' END"
            }
            "QTIME" => "to_char(localtimestamp, 'HH24MISSMS')",
            "QHOUR" => "to_char(localtimestamp, 'HH24')",
            "QMINUTE" => "to_char(localtimestamp, 'MI')",
            "QSECOND" => "to_char(localtimestamp, 'SS')",
            "QDAYOFWEEK" => "'*' || upper(to_char(localtimestamp, 'DY'))",
            _ => return None,
        })
    };
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let rows: Vec<String> = crate::clpgm::SYSTEM_VALUE_NAMES
        .iter()
        .filter_map(|n| {
            let (num, ch) = match clock(n) {
                Some(e) => ("NULL".to_string(), e.to_string()),
                None => match crate::clpgm::system_value_text(n)? {
                    Ok(t) => ("NULL".to_string(), q(&t)),
                    Err(v) => (v.to_string(), "NULL".to_string()),
                },
            };
            Some(format!("({}, {num}, {ch})", q(n)))
        })
        .collect();
    format!(
        "CREATE OR REPLACE VIEW qsys2.system_value_info AS
         SELECT CAST(v.n AS varchar(10)) COLLATE \"C\" AS system_value_name,
                CAST(v.num AS bigint) AS current_numeric_value,
                CAST(v.ch AS varchar(4096)) COLLATE \"C\" AS current_character_value
         FROM (VALUES {}) AS v(n, num, ch);",
        rows.join(", ")
    )
}

/// SYSTOOLS.SPOOLED_FILE_DATA(job, spooled file name, number) (Db2 for i
/// services, SPOOLED_FILE_DATA): the lines of a job's spooled file, the
/// highest-numbered of that name when no number is given; QPJOBLOG by
/// default; '*' is the statement's own job. None found: no rows (IBM i's
/// CPYSPLF-based function ends in error).
const SPOOLED_FILE_DATA: &str = r#"
CREATE SCHEMA IF NOT EXISTS systools;
DROP FUNCTION IF EXISTS systools.spooled_file_data(varchar, varchar, integer);
CREATE FUNCTION systools.spooled_file_data(job_name varchar,
    spooled_file_name varchar DEFAULT 'QPJOBLOG', spooled_file_number integer DEFAULT NULL)
RETURNS TABLE (ordinal_position integer, spooled_data varchar(200))
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  q text := upper(trim(job_name));
  n bigint;
  f bigint;
BEGIN
  IF q = '*' THEN
    q := upper(coalesce(current_setting('as4tainr.job', true), ''));
    -- Work without a job of as4tainr's (`system`): no job log is kept.
    IF q = '' THEN
      RETURN;
    END IF;
  END IF;
  IF split_part(q, '/', 1) ~ '^[0-9]{1,6}$' THEN
    n := split_part(q, '/', 1)::bigint;
  END IF;
  SELECT s.id INTO f FROM as4tainr.spooled_files s
   WHERE s.job = n AND s.user_name = split_part(q, '/', 2) AND s.job_name = split_part(q, '/', 3)
     AND s.file = upper(trim(spooled_file_name))
     AND (spooled_file_number IS NULL OR s.number = spooled_file_number)
   ORDER BY s.number DESC LIMIT 1;
  IF f IS NULL THEN
    RETURN;
  END IF;
  RETURN QUERY
  SELECT CAST(row_number() OVER (ORDER BY l.seq) AS integer), CAST(left(l.text, 200) AS varchar(200))
    FROM as4tainr.spool_lines l WHERE l.spl = f ORDER BY l.seq;
END $$;
"#;

/// QSYS2.JOBLOG_INFO(job [, ignore errors]): a job's log, oldest first
/// ('*' is the statement's own job).
const JOBLOG_INFO: &str = r#"
DROP FUNCTION IF EXISTS qsys2.joblog_info(varchar, varchar);
CREATE FUNCTION qsys2.joblog_info(job_name varchar, ignore_errors varchar DEFAULT 'NO')
RETURNS TABLE (
  ordinal_position integer, message_id varchar(7), message_type varchar(13),
  message_subtype varchar(22), severity smallint, message_timestamp timestamp,
  from_library varchar(10), from_program varchar(256), from_module varchar(10),
  from_procedure varchar(4096), from_instruction varchar(10), to_library varchar(10),
  to_program varchar(10), to_module varchar(10), to_procedure varchar(4096),
  to_instruction varchar(10), from_user varchar(10), message_file varchar(10),
  message_library varchar(10), message_token_length smallint, message_tokens bytea,
  message_text varchar(1024), message_second_level_text varchar(4096),
  message_key bytea, qualified_job_name varchar(28))
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  q text := upper(trim(job_name));
  n bigint;
  j record;
BEGIN
  IF q = '*' THEN
    q := upper(coalesce(current_setting('as4tainr.job', true), ''));
    -- A process with no as4tainr job (PASE `system`, `call`): the job log
    -- it copied before the statement (as4tainr.process_job_log, by job
    -- key; joblog.rs, commands.rs mirror_system_job_log).
    IF q = '' THEN
      RETURN QUERY
      SELECT CAST(row_number() OVER (ORDER BY l.seq) AS integer),
             CAST(l.msgid AS varchar(7)), CAST(l.kind AS varchar(13)),
             CAST(NULL AS varchar(22)), CAST(l.sev AS smallint), l.sent,
             CAST(l.from_library AS varchar(10)), CAST(l.from_program AS varchar(256)),
             CAST(l.from_module AS varchar(10)), CAST(l.from_procedure AS varchar(4096)),
             CAST(l.from_instruction AS varchar(10)), CAST(l.to_library AS varchar(10)),
             CAST(l.to_program AS varchar(10)), CAST(l.to_module AS varchar(10)),
             CAST(l.to_procedure AS varchar(4096)), CAST(l.to_instruction AS varchar(10)),
             CAST(l.from_user AS varchar(10)), CAST(l.msgf AS varchar(10)),
             CAST(l.msgf_library AS varchar(10)), CAST(NULL AS smallint), CAST(NULL AS bytea),
             CAST(l.text AS varchar(1024)), CAST(nullif(l.seclvl, '') AS varchar(4096)),
             CAST(int4send(l.seq) AS bytea),
             CAST(lpad((substr(l.job, 4)::bigint % 1000000)::text, 6, '0') || '/QUSER/QP0ZSPWT' AS varchar(28))
        FROM as4tainr.process_job_log l
       WHERE l.job = current_setting('as4tainr.job_key', true)
       ORDER BY l.seq;
      RETURN;
    END IF;
  END IF;
  IF split_part(q, '/', 1) ~ '^[0-9]{1,6}$' THEN
    n := split_part(q, '/', 1)::bigint;
  END IF;
  SELECT * INTO j FROM as4tainr.jobs
   WHERE number = n AND user_name = split_part(q, '/', 2) AND name = split_part(q, '/', 3);
  IF NOT FOUND THEN
    IF upper(coalesce(ignore_errors, 'NO')) = 'YES' THEN
      RETURN;
    END IF;
    RAISE EXCEPTION 'Job % not found.', job_name USING ERRCODE = '42704';
  END IF;
  RETURN QUERY
  SELECT CAST(row_number() OVER (ORDER BY l.seq) AS integer),
         CAST(nullif(l.msgid, '') AS varchar(7)),
         CAST(CASE l.kind WHEN '*RQS' THEN 'REQUEST' WHEN '*COMP' THEN 'COMPLETION'
                          WHEN '*ESCAPE' THEN 'ESCAPE' WHEN '*DIAG' THEN 'DIAGNOSTIC'
                          WHEN '*NOTIFY' THEN 'NOTIFY' WHEN '*INQ' THEN 'INQUIRY'
                          WHEN '*RPY' THEN 'REPLY' ELSE 'INFORMATIONAL' END AS varchar(13)),
         CAST(NULL AS varchar(22)), CAST(NULL AS smallint), CAST(l.sent AS timestamp),
         CAST(NULL AS varchar(10)), CAST(NULL AS varchar(256)), CAST(NULL AS varchar(10)),
         CAST(NULL AS varchar(4096)), CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10)),
         CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10)), CAST(NULL AS varchar(4096)),
         CAST(NULL AS varchar(10)), CAST(j.user_name AS varchar(10)), CAST(NULL AS varchar(10)),
         CAST(NULL AS varchar(10)), CAST(NULL AS smallint), CAST(NULL AS bytea),
         CAST(l.text AS varchar(1024)), CAST(NULL AS varchar(4096)),
         CAST(int4send(CAST(l.seq AS integer)) AS bytea),
         CAST(lpad(j.number::text, 6, '0') || '/' || j.user_name || '/' || j.name AS varchar(28))
    FROM as4tainr.job_log l WHERE l.job = j.number ORDER BY l.seq;
END $$;
"#;

/// QSYS2.ACTIVE_JOB_INFO's result columns (IBM i services), in IBM's
/// order: (name, type, value). DETAILED_INFO NONE gives the first 30;
/// WORK and ALL the work management ones as4tainr knows.
const ACTIVE_JOB_INFO: &[Col] = &[
    ("ordinal_position", "integer", "row_number() OVER (ORDER BY j.number)"),
    ("job_name", "varchar(28)", "lpad(j.number::text, 6, '0') || '/' || j.user_name || '/' || j.name"),
    ("job_name_short", "varchar(10)", "j.name"),
    ("job_user", "varchar(10)", "j.user_name"),
    ("job_number", "varchar(6)", "lpad(j.number::text, 6, '0')"),
    ("internal_job_id", "bytea", "NULL"),
    ("subsystem", "varchar(10)", "nullif(j.subsystem, '')"),
    ("subsystem_library_name", "varchar(10)", "CASE WHEN j.subsystem <> '' THEN 'QSYS' END"),
    ("authorization_name", "varchar(10)", "j.user_name"),
    ("job_type", "varchar(3)", "j.type"),
    ("function_type", "varchar(3)", "CASE WHEN j.function LIKE '___-%' THEN left(j.function, 3) END"),
    ("function", "varchar(10)", "CASE WHEN j.function LIKE '___-%' THEN substr(j.function, 5) ELSE nullif(j.function, '') END"),
    ("job_status", "varchar(4)", "coalesce(nullif(j.activity, ''), 'RUN')"),
    ("memory_pool", "varchar(9)", "CASE j.type WHEN 'INT' THEN 'INTERACT' ELSE 'BASE' END"),
    ("run_priority", "integer", "CASE j.type WHEN 'INT' THEN 20 ELSE 50 END"),
    ("thread_count", "integer", "1"),
    ("temporary_storage", "integer", "NULL"),
    ("cpu_time", "decimal(20,0)", "NULL"),
    ("total_disk_io_count", "decimal(20,0)", "NULL"),
    ("elapsed_interaction_count", "integer", "NULL"),
    ("elapsed_total_response_time", "integer", "NULL"),
    ("elapsed_total_disk_io_count", "decimal(20,0)", "NULL"),
    ("elapsed_async_disk_io_count", "decimal(20,0)", "NULL"),
    ("elapsed_sync_disk_io_count", "decimal(20,0)", "NULL"),
    ("elapsed_cpu_percentage", "decimal(10,2)", "NULL"),
    ("elapsed_cpu_time", "decimal(20,0)", "NULL"),
    ("elapsed_page_fault_count", "decimal(20,0)", "NULL"),
    ("job_end_reason", "varchar(60)", "NULL"),
    ("server_type", "varchar(30)", "NULL"),
    ("elapsed_time", "decimal(20,3)", "NULL"),
    ("job_description_library", "varchar(10)", "CASE WHEN work THEN 'QGPL' END"),
    ("job_description", "varchar(10)", "CASE WHEN work THEN 'QDFTJOBD' END"),
    ("job_queue_library", "varchar(10)", "CASE WHEN work AND j.jobq <> '' THEN 'QGPL' END"),
    ("job_queue", "varchar(10)", "CASE WHEN work THEN nullif(j.jobq, '') END"),
    ("output_queue_library", "varchar(10)", "CASE WHEN work THEN 'QGPL' END"),
    ("output_queue", "varchar(10)", "CASE WHEN work THEN 'QPRINT' END"),
    ("workload_group", "varchar(10)", "NULL"),
    ("ccsid", "integer", "CASE WHEN work THEN 37 END"),
    ("default_ccsid", "integer", "CASE WHEN work THEN 37 END"),
    ("sort_sequence_library", "varchar(10)", "NULL"),
    ("sort_sequence", "varchar(10)", "CASE WHEN work THEN '*HEX' END"),
    ("language_id", "char(3)", "CASE WHEN work THEN 'ENU' END"),
    ("date_format", "char(4)", "CASE WHEN work THEN '*MDY' END"),
    ("date_separator", "char(1)", "CASE WHEN work THEN '/' END"),
    ("time_separator", "char(1)", "CASE WHEN work THEN ':' END"),
    ("decimal_format", "varchar(6)", "CASE WHEN work THEN '*BLANK' END"),
    ("timezone_description", "varchar(10)", "NULL"),
    ("timezone_current_offset", "integer", "NULL"),
    ("timezone_full_name", "varchar(50)", "NULL"),
    ("timezone_abbreviated_name", "varchar(10)", "NULL"),
    ("job_type_enhanced", "varchar(28)", "CASE WHEN work THEN CASE j.type WHEN 'INT' THEN 'INTERACTIVE' ELSE 'BATCH' END END"),
    ("job_entered_system_time", "timestamp(0)", "CASE WHEN work THEN j.entered END"),
    ("job_active_time", "timestamp(0)", "CASE WHEN work THEN j.started END"),
    ("client_ip_address", "varchar(45)", "NULL"),
    ("job_user_identity_setting", "varchar(11)", "NULL"),
    ("job_user_identity", "varchar(10)", "NULL"),
    ("dbcs_capable", "varchar(3)", "NULL"),
    ("signal_status", "varchar(3)", "NULL"),
    ("message_reply", "varchar(3)", "NULL"),
    ("end_status", "varchar(3)", "NULL"),
    ("cancel_key", "varchar(3)", "NULL"),
    ("exit_key", "varchar(3)", "NULL"),
    ("maximum_active_threads", "integer", "NULL"),
    ("system_pool_id", "integer", "NULL"),
    ("pool_name", "varchar(10)", "NULL"),
    ("qtemp_size", "integer", "NULL"),
    ("peak_temporary_storage", "integer", "NULL"),
    ("default_wait", "integer", "NULL"),
    ("maximum_processing_time_allowed", "integer", "NULL"),
    ("maximum_temporary_storage_allowed", "integer", "NULL"),
    ("time_slice", "integer", "NULL"),
    ("page_faults", "bigint", "NULL"),
    ("total_response_time", "bigint", "NULL"),
    ("interactive_transactions", "integer", "NULL"),
    ("database_lock_waits", "integer", "NULL"),
    ("non_database_lock_waits", "integer", "NULL"),
    ("internal_machine_lock_waits", "integer", "NULL"),
    ("database_lock_wait_time", "integer", "NULL"),
    ("non_database_lock_wait_time", "integer", "NULL"),
    ("internal_machine_lock_wait_time", "integer", "NULL"),
    ("sql_statement_text", "varchar(10000)", "NULL"),
    ("sql_statement_status", "varchar(8)", "NULL"),
    ("sql_statement_start_timestamp", "timestamp", "NULL"),
    ("sql_statement_name", "varchar(128)", "NULL"),
    ("sql_statement_library_name", "varchar(10)", "NULL"),
    ("sql_statement_object_name", "varchar(10)", "NULL"),
    ("sql_statement_object_type", "varchar(7)", "NULL"),
    ("query_options_library_name", "varchar(10)", "NULL"),
    ("sql_activation_group_count", "integer", "NULL"),
    ("sql_descriptor_count", "bigint", "NULL"),
    ("sql_lob_locator_count", "integer", "NULL"),
    ("cli_handle_count", "bigint", "NULL"),
    ("sql_server_mode", "varchar(3)", "NULL"),
    ("client_acctng", "varchar(255)", "NULL"),
    ("client_applname", "varchar(255)", "NULL"),
    ("client_programid", "varchar(255)", "NULL"),
    ("client_userid", "varchar(255)", "NULL"),
    ("client_wrkstnname", "varchar(255)", "NULL"),
    ("routine_type", "char(1)", "NULL"),
    ("routine_schema", "varchar(128)", "NULL"),
    ("routine_specific_name", "varchar(128)", "NULL"),
    ("client_port", "integer", "NULL"),
    ("client_host", "varchar(255)", "NULL"),
    ("interface_name", "varchar(127)", "NULL"),
    ("interface_type", "varchar(63)", "NULL"),
    ("interface_level", "varchar(63)", "NULL"),
    ("server_mode_connecting_job", "varchar(28)", "NULL"),
    ("server_mode_connecting_thread", "bigint", "NULL"),
    ("prestart_job_reuse_count", "integer", "NULL"),
    ("prestart_job_max_use_count", "integer", "NULL"),
    ("available_result_sets", "integer", "NULL"),
    ("unconsumed_result_sets", "integer", "NULL"),
    ("open_cursor_count", "integer", "NULL"),
    ("full_open_cursor_count", "bigint", "NULL"),
    ("pseudo_open_cursor_count", "bigint", "NULL"),
    ("pseudo_closed_cursor_count", "integer", "NULL"),
    ("cqe_cursor_count", "integer", "NULL"),
    ("cqe_cursor_storage", "integer", "NULL"),
    ("sqe_cursor_count", "integer", "NULL"),
    ("sqe_cursor_storage", "integer", "NULL"),
    ("largest_query_size", "integer", "NULL"),
    ("qro_hash", "varchar(8)", "NULL"),
    ("open_files", "integer", "NULL"),
];

/// QSYS2.ACTIVE_JOB_INFO(...): the active jobs, filtered by subsystems,
/// job names (generic with *; *CURRENT for the statement's job) and
/// users, given as comma-separated lists.
fn active_job_info() -> String {
    let (defs, vals) = col_lists(ACTIVE_JOB_INFO);
    format!(
        "DROP FUNCTION IF EXISTS qsys2.active_job_info(varchar, varchar, varchar, varchar, varchar);
         CREATE FUNCTION qsys2.active_job_info(reset_statistics varchar DEFAULT 'NO',
             subsystem_list_filter varchar DEFAULT NULL, job_name_filter varchar DEFAULT NULL,
             current_user_list_filter varchar DEFAULT NULL, detailed_info varchar DEFAULT 'NONE')
         RETURNS TABLE ({defs})
         LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
         SELECT {vals}
           FROM as4tainr.jobs j
          CROSS JOIN (SELECT upper(coalesce(detailed_info, 'NONE')) IN ('WORK', 'ALL') AS work) AS d
          WHERE j.status = 'ACTIVE'
            AND (subsystem_list_filter IS NULL OR j.subsystem = ANY (
                   SELECT upper(trim(s)) FROM regexp_split_to_table(subsystem_list_filter, ',') AS s))
            AND (current_user_list_filter IS NULL OR j.user_name = ANY (
                   SELECT upper(trim(s)) FROM regexp_split_to_table(current_user_list_filter, ',') AS s))
            AND (job_name_filter IS NULL OR upper(trim(job_name_filter)) = '*ALL'
                 OR (upper(trim(job_name_filter)) = '*CURRENT'
                     AND lpad(j.number::text, 6, '0') || '/' || j.user_name || '/' || j.name
                         = upper(current_setting('as4tainr.job', true)))
                 OR j.name LIKE replace(replace(upper(trim(job_name_filter)), '_', '\\_'), '*', '%'))
          ORDER BY j.number
         $as4tainr$;"
    )
}

/// QSYS2.SYSROUTINES and QSYS2.SYSPARMS (Db2 for i catalog views) over
/// the SQL procedures and functions.
const ROUTINE_VIEWS: &str = r#"
CREATE OR REPLACE VIEW as4tainr_svc.routines AS
SELECT r.library, r.name, r.kind, r.created, d.def
  FROM as4tainr.routines r CROSS JOIN LATERAL (SELECT r.definition::jsonb AS def) d;
CREATE OR REPLACE VIEW qsys2.sysroutines AS
SELECT CAST(r.library AS varchar(128)) COLLATE "C" AS specific_schema,
       CAST(r.name AS varchar(128)) COLLATE "C" AS specific_name,
       CAST(r.library AS varchar(128)) COLLATE "C" AS routine_schema,
       CAST(r.name AS varchar(128)) COLLATE "C" AS routine_name,
       CAST(r.kind AS varchar(9)) COLLATE "C" AS routine_type,
       CAST(r.created AS timestamp) AS routine_created,
       CAST(NULL AS varchar(128)) COLLATE "C" AS routine_definer,
       CAST(CASE WHEN r.def ->> 'external' IS NOT NULL THEN 'EXTERNAL' ELSE 'SQL' END AS varchar(8)) COLLATE "C" AS routine_body,
       CAST(r.def ->> 'external' AS varchar(279)) COLLATE "C" AS external_name,
       CAST(NULL AS varchar(8)) COLLATE "C" AS external_language,
       CAST(CASE WHEN r.def ->> 'external' IS NOT NULL THEN 'GENERAL' END AS varchar(7)) COLLATE "C" AS parameter_style,
       CAST('NO' AS varchar(3)) COLLATE "C" AS is_deterministic,
       CAST(CASE r.kind WHEN 'PROCEDURE' THEN 'MODIFIES' ELSE 'READS' END AS varchar(8)) COLLATE "C" AS sql_data_access,
       CAST(NULL AS varchar(3483)) COLLATE "C" AS sql_path,
       CAST(NULL AS varchar(2048)) COLLATE "C" AS parm_signature,
       CAST(CASE WHEN r.kind = 'FUNCTION' THEN 1 ELSE 0 END AS smallint) AS number_of_results,
       CAST(coalesce((r.def ->> 'result_sets')::int, 0) AS smallint) AS max_dynamic_result_sets,
       CAST((SELECT count(*) FROM jsonb_array_elements(r.def -> 'parms') p WHERE p ->> 'mode' = 'IN') AS smallint) AS in_parms,
       CAST((SELECT count(*) FROM jsonb_array_elements(r.def -> 'parms') p WHERE p ->> 'mode' = 'OUT') AS smallint) AS out_parms,
       CAST((SELECT count(*) FROM jsonb_array_elements(r.def -> 'parms') p WHERE p ->> 'mode' = 'INOUT') AS smallint) AS inout_parms,
       CAST(NULL AS bytea) AS parse_tree,
       CAST(NULL AS bytea) AS parm_array,
       CAST(NULL AS varchar(2000)) COLLATE "C" AS long_comment,
       CAST(nullif(r.def ->> 'body', '') AS text) AS routine_definition,
       CAST(CASE WHEN r.kind = 'PROCEDURE' THEN ' ' END AS char(1)) COLLATE "C" AS function_origin,
       CAST(CASE WHEN r.kind = 'PROCEDURE' THEN ' '
                 WHEN r.def ->> 'returns' LIKE 'TABLE(%' THEN 'T' ELSE 'S' END AS char(1)) COLLATE "C" AS function_type,
       CAST(NULL AS char(1)) COLLATE "C" AS external_action,
       CAST(NULL AS varchar(3)) COLLATE "C" AS is_null_call,
       CAST(NULL AS integer) AS scratch_pad,
       CAST(NULL AS varchar(3)) COLLATE "C" AS final_call,
       CAST(NULL AS varchar(3)) COLLATE "C" AS parallelizable,
       CAST(NULL AS varchar(3)) COLLATE "C" AS dbinfo,
       CAST(NULL AS varchar(128)) COLLATE "C" AS source_specific_schema,
       CAST(NULL AS varchar(128)) COLLATE "C" AS source_specific_name,
       CAST(NULL AS varchar(3)) COLLATE "C" AS is_user_defined_cast,
       CAST(NULL AS bigint) AS cardinality,
       CAST(NULL AS varchar(3)) COLLATE "C" AS fenced,
       CAST(NULL AS varchar(3)) COLLATE "C" AS commit_on_return,
       CAST(0 AS smallint) AS iasp_number,
       CAST(NULL AS varchar(3)) COLLATE "C" AS new_savepoint_level,
       CAST(r.created AS timestamp) AS last_altered,
       CAST(NULL AS char(1)) COLLATE "C" AS debug_mode,
       CAST(NULL AS text) AS debug_data,
       CAST(NULL AS char(1)) COLLATE "C" AS rounding_mode,
       CAST(NULL AS varchar(50)) COLLATE "C" AS routine_text,
       CAST(NULL AS char(1)) COLLATE "C" AS secure,
       CAST(NULL AS bytea) AS routine_environment,
       CAST(NULL AS bytea) AS routine_default_qdt
  FROM as4tainr_svc.routines r;
CREATE OR REPLACE VIEW qsys2.sysparms AS
WITH p AS (
  SELECT r.library, r.name, CAST(x.n AS integer) AS pos, x.p ->> 'mode' AS mode,
         x.p ->> 'name' AS pname, x.p ->> 'ty' AS ty, x.p ->> 'default' AS dft, 'P' AS row_type
    FROM as4tainr_svc.routines r,
         jsonb_array_elements(r.def -> 'parms') WITH ORDINALITY AS x(p, n)
  UNION ALL
  -- A scalar function's result.
  SELECT r.library, r.name, CAST(jsonb_array_length(r.def -> 'parms') + 1 AS integer), 'OUT',
         NULL, r.def ->> 'returns', NULL, 'C'
    FROM as4tainr_svc.routines r
   WHERE r.kind = 'FUNCTION' AND r.def ->> 'returns' NOT LIKE 'TABLE(%'
)
SELECT CAST(p.library AS varchar(128)) COLLATE "C" AS specific_schema,
       CAST(p.name AS varchar(128)) COLLATE "C" AS specific_name,
       p.pos AS ordinal_position,
       CAST(p.mode AS varchar(5)) COLLATE "C" AS parameter_mode,
       CAST(p.pname AS varchar(128)) COLLATE "C" AS parameter_name,
       CAST(upper(trim(regexp_replace(p.ty, '\s*\(.*$', ''))) AS varchar(128)) COLLATE "C" AS data_type,
       CAST(CASE WHEN upper(p.ty) ~ '^(DEC|DECIMAL|NUMERIC|NUM)\y'
                 THEN coalesce((regexp_match(p.ty, '\(\s*\d+\s*,\s*(\d+)'))[1]::int, 0) END AS integer) AS numeric_scale,
       CAST(CASE WHEN upper(p.ty) ~ '^(DEC|DECIMAL|NUMERIC|NUM)\y'
                 THEN coalesce((regexp_match(p.ty, '\(\s*(\d+)'))[1]::int, 5)
                 WHEN upper(p.ty) ~ '^SMALLINT' THEN 5
                 WHEN upper(p.ty) ~ '^(INT|INTEGER)\y' THEN 10
                 WHEN upper(p.ty) ~ '^BIGINT' THEN 19 END AS integer) AS numeric_precision,
       CAST(CASE WHEN upper(p.ty) ~ '^(CHAR|CHARACTER|VARCHAR|CLOB)' THEN 37 END AS integer) AS ccsid,
       CAST(CASE WHEN upper(p.ty) ~ '^(CHAR|CHARACTER|VARCHAR|GRAPHIC|VARGRAPHIC|BINARY|VARBINARY)'
                 THEN coalesce((regexp_match(p.ty, '\(\s*(\d+)'))[1]::int, 1) END AS integer) AS character_maximum_length,
       CAST(NULL AS integer) AS character_octet_length,
       CAST(CASE WHEN upper(p.ty) ~ '^(DEC|DECIMAL|NUMERIC|NUM|SMALLINT|INT|INTEGER|BIGINT)\y' THEN 10 END AS integer) AS numeric_precision_radix,
       CAST(NULL AS integer) AS datetime_precision,
       CAST('YES' AS varchar(3)) COLLATE "C" AS is_nullable,
       CAST(NULL AS varchar(2000)) COLLATE "C" AS long_comment,
       CAST(p.row_type AS char(1)) COLLATE "C" AS row_type,
       CAST(NULL AS varchar(128)) COLLATE "C" AS data_type_schema,
       CAST(NULL AS varchar(128)) COLLATE "C" AS data_type_name,
       CAST('NO' AS varchar(3)) COLLATE "C" AS as_locator,
       CAST(0 AS smallint) AS iasp_number,
       CAST('NO' AS varchar(3)) COLLATE "C" AS normalize_data,
       CAST(p.dft AS text) AS "default"
  FROM p;
"#;

/// Text from bytes in a CCSID (IFS_READ).
fn decode_function() -> String {
    // CCSID 37 as a 256-character string (NUL as a blank: PostgreSQL text
    // cannot hold it).
    let map: String = crate::tn5250::ebcdic::TO_UNICODE
        .iter()
        .map(|&u| format!("\\{:04X}", if u == 0 { 0x20 } else { u }))
        .collect();
    format!(
        r#"CREATE OR REPLACE FUNCTION as4tainr_svc.decode(b bytea, ccsid integer)
RETURNS text LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  m text := U&'{map}';
  out text := '';
  i integer := 0;
  u integer;
BEGIN
  IF ccsid = 1208 THEN RETURN convert_from(b, 'UTF8'); END IF;
  IF ccsid IN (819, 367) THEN RETURN convert_from(b, 'LATIN1'); END IF;
  IF ccsid = 1252 THEN RETURN convert_from(b, 'WIN1252'); END IF;
  IF ccsid = 37 THEN
    RETURN (SELECT coalesce(string_agg(substr(m, get_byte(b, k) + 1, 1), '' ORDER BY k), '')
              FROM generate_series(0, length(b) - 1) AS k);
  END IF;
  IF ccsid IN (1200, 13488) THEN
    WHILE i + 1 < length(b) LOOP
      u := get_byte(b, i) * 256 + get_byte(b, i + 1);
      out := out || CASE WHEN u BETWEEN 55296 AND 57343 OR u = 0 THEN '?' ELSE chr(u) END;
      i := i + 2;
    END LOOP;
    RETURN out;
  END IF;
  RAISE EXCEPTION 'CCSID % cannot be converted.', ccsid USING ERRCODE = '22021';
END $as4tainr$;"#
    )
}

/// QSYS2.IFS_READ, IFS_READ_UTF8 (lines as text) and IFS_READ_BINARY
/// (pieces as bytes): a stream file, split at its line ends.
const IFS_READ: &str = r#"
CREATE OR REPLACE FUNCTION as4tainr_svc.ifs_path(p text)
RETURNS text LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  parts text[] := '{}';
  x text;
BEGIN
  FOREACH x IN ARRAY string_to_array(coalesce(p, ''), '/') LOOP
    IF x = '' OR x = '.' THEN CONTINUE; END IF;
    IF x = '..' THEN parts := parts[1:cardinality(parts) - 1]; CONTINUE; END IF;
    parts := parts || x;
  END LOOP;
  RETURN '/' || array_to_string(parts, '/');
END $as4tainr$;

CREATE OR REPLACE FUNCTION as4tainr_svc.ifs_file(path_name text, ignore_errors text)
RETURNS TABLE (data bytea, ccsid integer)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  RETURN QUERY SELECT f.data, f.ccsid FROM as4tainr.ifs f
   WHERE f.path_key = lower(as4tainr_svc.ifs_path(path_name)) AND f.kind = 'F';
  IF NOT FOUND AND upper(coalesce(ignore_errors, 'YES')) = 'NO' THEN
    RAISE EXCEPTION 'Stream file % not found.', path_name USING ERRCODE = '42704';
  END IF;
END $as4tainr$;

CREATE OR REPLACE FUNCTION as4tainr_svc.ifs_lines(t text, max_len integer, eol text)
RETURNS TABLE (line_number integer, line text)
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  WITH lines AS (
    SELECT l, n FROM regexp_split_to_table(t,
             CASE upper(coalesce(eol, 'ANY'))
               WHEN 'ANY' THEN E'\r\n|\n\r|\r|\n' WHEN 'CR' THEN E'\r' WHEN 'LF' THEN E'\n'
               WHEN 'CRLF' THEN E'\r\n' WHEN 'LFCR' THEN E'\n\r' ELSE E'\\A(?!)' END)
         WITH ORDINALITY AS x(l, n)
  ),
  kept AS (
    -- The line ends end lines: nothing after the last one is no line.
    SELECT l, n FROM lines
     WHERE NOT (l = '' AND n = (SELECT max(n) FROM lines))
  ),
  pieces AS (
    SELECT n, k, substr(l, k * max_len + 1, max_len) AS piece
      FROM kept, generate_series(0, greatest(ceil(length(l)::numeric / max_len)::int - 1, 0)) AS k
  )
  SELECT CAST(row_number() OVER (ORDER BY n, k) AS integer), piece FROM pieces ORDER BY n, k
$as4tainr$;

DROP FUNCTION IF EXISTS qsys2.ifs_read(varchar, integer, varchar, varchar);
CREATE FUNCTION qsys2.ifs_read(path_name varchar, maximum_line_length integer DEFAULT 2147483647,
    end_of_line varchar DEFAULT 'ANY', ignore_errors varchar DEFAULT 'YES')
RETURNS TABLE (line_number integer, line text)
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT l.line_number, l.line
    FROM as4tainr_svc.ifs_file(path_name, ignore_errors) f,
         as4tainr_svc.ifs_lines(as4tainr_svc.decode(f.data, f.ccsid), maximum_line_length, end_of_line) l
$as4tainr$;

DROP FUNCTION IF EXISTS qsys2.ifs_read_utf8(varchar, integer, varchar, varchar);
CREATE FUNCTION qsys2.ifs_read_utf8(path_name varchar, maximum_line_length integer DEFAULT 2147483647,
    end_of_line varchar DEFAULT 'ANY', ignore_errors varchar DEFAULT 'YES')
RETURNS TABLE (line_number integer, line text)
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT * FROM qsys2.ifs_read(path_name, maximum_line_length, end_of_line, ignore_errors)
$as4tainr$;

DROP FUNCTION IF EXISTS qsys2.ifs_read_binary(varchar, integer, varchar, varchar);
CREATE FUNCTION qsys2.ifs_read_binary(path_name varchar, maximum_line_length integer DEFAULT 2147483647,
    end_of_line varchar DEFAULT 'NONE', ignore_errors varchar DEFAULT 'YES')
RETURNS TABLE (line_number integer, line bytea)
LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  f record;
  k integer := 0;
BEGIN
  IF upper(coalesce(end_of_line, 'NONE')) <> 'NONE' THEN
    RAISE EXCEPTION 'END_OF_LINE must be NONE for IFS_READ_BINARY.' USING ERRCODE = '22023';
  END IF;
  FOR f IN SELECT * FROM as4tainr_svc.ifs_file(path_name, ignore_errors) LOOP
    WHILE k * maximum_line_length < length(f.data) LOOP
      line_number := k + 1;
      line := substring(f.data FROM k * maximum_line_length + 1 FOR maximum_line_length);
      RETURN NEXT;
      k := k + 1;
    END LOOP;
  END LOOP;
END $as4tainr$;
"#;

/// JSON text as jsonb, or the null value when it is not JSON (JSON_TABLE
/// EMPTY ON ERROR).
const TRY_JSONB: &str = r#"
CREATE OR REPLACE FUNCTION as4tainr_svc.try_jsonb(t text)
RETURNS jsonb LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  RETURN t::jsonb;
EXCEPTION WHEN others THEN
  RETURN NULL;
END $as4tainr$;
GRANT EXECUTE ON FUNCTION as4tainr_svc.try_jsonb(text) TO PUBLIC;
"#;

/// The record format of a QSYS2 view an RPG program names as an
/// externally described file or data structure (EXTNAME(QSYS2/ENV_VARS)):
/// its system column names and types.
pub fn view_record(name: &str) -> Option<crate::dds::PhysicalFile> {
    use crate::dds::{DataType, FieldDef, PhysicalFile};
    let field = |n: &str, alias: &str, t: DataType, len: u32, varlen: bool, null: bool| FieldDef {
        name: n.to_string(),
        data_type: t,
        length: len,
        decimals: 0,
        text: String::new(),
        varlen,
        null_capable: null,
        default: None,
        alias: Some(alias.to_string()),
        identity: None,
        double: false,
        generated: None,
        hidden: false,
    };
    let fields = match name.to_ascii_uppercase().as_str() {
        "ENV_VARS" | "ENVIRONMENT_VARIABLE_INFO" => vec![
            field(
                "VAR_TYPE",
                "ENVIRONMENT_VARIABLE_TYPE",
                DataType::Char,
                6,
                true,
                false,
            ),
            field(
                "VAR_NAME",
                "ENVIRONMENT_VARIABLE_NAME",
                DataType::Graphic,
                128,
                true,
                false,
            ),
            field(
                "VAR_VALUE",
                "ENVIRONMENT_VARIABLE_VALUE",
                DataType::Graphic,
                1024,
                true,
                true,
            ),
            field(
                "VAR_BNAME",
                "ENVIRONMENT_VARIABLE_BINARY_NAME",
                DataType::Hex,
                128,
                true,
                false,
            ),
            field(
                "VAR_BVALUE",
                "ENVIRONMENT_VARIABLE_BINARY_VALUE",
                DataType::Hex,
                1024,
                true,
                true,
            ),
            field(
                "VAR_CCSID",
                "ENVIRONMENT_VARIABLE_CCSID",
                DataType::Binary,
                9,
                false,
                false,
            ),
        ],
        _ => return None,
    };
    Some(PhysicalFile {
        name: "ENV_VARS".into(),
        format: "ENV_VARS".into(),
        text: String::new(),
        unique: false,
        fields,
        keys: vec![],
        descending: vec![],
        view: None,
        source: None,
    })
}

/// QSYS2.ENVIRONMENT_VARIABLE_INFO (IBM i services: ENVIRONMENT_VARIABLE_INFO
/// view, system name ENV_VARS): the job's environment variables (JOB),
/// then the system-level ones (SYSTEM). Columns and their system names
/// from that page; the binary forms are the UTF-8 bytes here. A job
/// variable added with CCSID(*JOB) shows the job's CCSID (273 on PUB400,
/// 2026-10-26, tools/golden/verify1.txt v_out; as4tainr's jobs are 37).
const ENV_VARS: &str = r#"
CREATE OR REPLACE VIEW qsys2.environment_variable_info AS
SELECT CAST(t AS varchar(6)) COLLATE "C" AS environment_variable_type,
       CAST(name AS varchar(128)) COLLATE "C" AS environment_variable_name,
       CAST(value AS varchar(1024)) COLLATE "C" AS environment_variable_value,
       convert_to(name, 'UTF8') AS environment_variable_binary_name,
       convert_to(value, 'UTF8') AS environment_variable_binary_value,
       CAST(CASE WHEN ccsid <> 0 THEN ccsid WHEN t = 'JOB' THEN 37 ELSE 65535 END AS integer)
         AS environment_variable_ccsid
FROM (SELECT 'JOB' AS t, 1 AS o, name, value, ccsid FROM as4tainr.job_environment_variables
      WHERE job = current_setting('as4tainr.job_key', true)
      UNION ALL
      SELECT 'SYSTEM', 2, name, value, ccsid FROM as4tainr.environment_variables) v
ORDER BY o, name;
CREATE OR REPLACE VIEW qsys2.env_vars AS
SELECT environment_variable_type AS var_type,
       environment_variable_name AS var_name,
       environment_variable_value AS var_value,
       environment_variable_binary_name AS var_bname,
       environment_variable_binary_value AS var_bvalue,
       environment_variable_ccsid AS var_ccsid
FROM qsys2.environment_variable_info;
"#;

/// QSYS2.BOUND_SRVPGM_INFO (IBM i services, BOUND_SRVPGM_INFO view): the
/// service programs each program and service program is bound to, with
/// the library they were named with when bound (*LIBL, or a library).
/// The signature is shown for generated signatures; one given as text in
/// binder language is null (docs/fidelity-gaps.md).
const BOUND_SRVPGM_INFO: &str = r#"
CREATE OR REPLACE VIEW qsys2.bound_srvpgm_info AS
SELECT CAST(o.library AS varchar(10)) COLLATE "C" AS program_library,
       CAST(o.name AS varchar(10)) COLLATE "C" AS program_name,
       CAST(o.objtype AS varchar(7)) COLLATE "C" AS object_type,
       CAST(COALESCE(o.b->'srv_libraries'->>((s->>0) || '/' || (s->>1)), '*LIBL')
            AS varchar(10)) COLLATE "C" AS bound_service_program_library,
       CAST(s->>1 AS varchar(10)) COLLATE "C" AS bound_service_program,
       CASE WHEN (o.b->'srv_signatures'->>((s->>0) || '/' || (s->>1))) ~ '^[0-9A-Fa-f]{16}$'
            THEN decode('0000000000000000' || (o.b->'srv_signatures'->>((s->>0) || '/' || (s->>1))), 'hex')
       END AS bound_service_program_signature,
       CAST('*IMMED' AS varchar(6)) COLLATE "C" AS bound_service_program_activation
FROM (SELECT library, name, '*PGM' AS objtype, binding::jsonb AS b
        FROM as4tainr.programs WHERE binding IS NOT NULL
      UNION ALL
      SELECT library, name, '*SRVPGM', binding::jsonb FROM as4tainr.service_programs) o
CROSS JOIN LATERAL jsonb_array_elements(COALESCE(o.b->'srvpgms', '[]'::jsonb)) s;
"#;

/// The services added with this version: definitions and grants.
fn work_services() -> String {
    format!(
        "ALTER TABLE as4tainr.routines ADD COLUMN IF NOT EXISTS created timestamptz NOT NULL DEFAULT now();
         {TRY_JSONB}
         {}
         {}
         {}
         {JOBLOG_INFO}
         {}
         {ROUTINE_VIEWS}
         {}
         {IFS_READ}
         {ENV_VARS}
         {BOUND_SRVPGM_INFO}
         GRANT SELECT ON qsys2.bound_srvpgm_info TO {SQL_ROLE};
         GRANT SELECT ON qsys2.library_list_info, qsys2.user_info, qsys2.system_value_info,
               qsys2.sysroutines, qsys2.sysparms, qsys2.environment_variable_info,
               qsys2.env_vars TO {SQL_ROLE};",
        library_list_info(),
        user_info(),
        system_value_info(),
        active_job_info(),
        decode_function(),
    )
}
