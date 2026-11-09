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
const VERSION: &str = "as4tainr services 56";

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
       o.changed, o.user_attr, o.src_file, o.src_lib, o.src_mbr
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
    (
        "objlongschema",
        "varchar(128)",
        "CASE WHEN o.library = arg.qtemp THEN 'QTEMP' ELSE o.library END",
    ),
    ("text", "varchar(50)", "o.text"),
    ("sql_object_type", "varchar(9)", "o.sql_object_type"),
    (
        "objlib",
        "varchar(10)",
        "CASE WHEN o.library = arg.qtemp THEN 'QTEMP' ELSE o.library END",
    ),
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
    (
        "user_defined_attribute",
        "varchar(10)",
        "nullif(o.user_attr, '')",
    ),
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
         WITH job AS (
           SELECT coalesce(nullif(current_setting('as4tainr.qtemp', true), ''), 'QTEMP') AS qtemp
         ),
         arg AS (
           SELECT CASE WHEN upper(trim(object_schema)) = 'QTEMP' THEN job.qtemp
                       ELSE upper(trim(object_schema)) END AS sch,
                  job.qtemp,
                  upper(trim(coalesce(object_name, '*ALL'))) AS nam,
                  ARRAY(SELECT '*' || ltrim(t, '*')
                        FROM regexp_split_to_table(upper(trim(objtypelist)), '[ ,]+') AS t
                        WHERE t <> '') AS types
           FROM job
         ),
         libl AS (
           SELECT upper(btrim(btrim(s), chr(34))) AS lib, n
           FROM regexp_split_to_table(current_setting('search_path'), ',') WITH ORDINALITY AS x(s, n)
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
                CAST(x.l AS varchar(128)) COLLATE \"C\" AS schema_name,
                CAST(x.l AS varchar(10)) COLLATE \"C\" AS system_schema_name,
                CAST(CASE WHEN x.l IN ({sys}) THEN 'SYSTEM'
                          WHEN x.l = current_setting('as4tainr.curlib', true) THEN 'CURRENT'
                          ELSE 'USER' END AS varchar(15)) COLLATE \"C\" AS type,
                CAST(0 AS smallint) AS iasp_number,
                CAST(nullif(o.text, '') AS varchar(50)) COLLATE \"C\" AS text_description
         FROM (SELECT s, n,
                      -- The job's QTEMP (qtemp.rs: #QTnnnnnn, #QPnnnnnnn) by its name.
                      CASE WHEN upper(s) ~ '^#Q[TP][0-9]+$' THEN 'QTEMP' ELSE upper(s) END AS l
                 FROM unnest(current_schemas(false)) WITH ORDINALITY AS u(s, n)) AS x
         LEFT JOIN as4tainr.objects o
           ON o.library = 'QSYS' AND o.objtype = '*LIB' AND o.name = x.l
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
            "QDATETIME" => "to_char(localtimestamp, 'YYYYMMDDHH24MISSUS')",
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
   WHERE s.job = n
     -- job_name is the qualified name (spool.rs open); a bare name with
     -- the user in user_name is taken too.
     AND ((split_part(s.job_name, '/', 2) = split_part(q, '/', 2)
           AND split_part(s.job_name, '/', 3) = split_part(q, '/', 3))
          OR (s.job_name = split_part(q, '/', 3) AND s.user_name = split_part(q, '/', 2)))
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

-- QSYS2.STACK_INFO(job [, thread]): the call stack of the statement's
-- own job ('*') as the program running the statement left it in
-- as4tainr.stack, the newest entry first, numbered from the oldest
-- (PUB400 2026-10-31, verify32 y32_rows); a control boundary where an
-- ILE entry's caller is in another activation group or is not ILE;
-- other jobs: no rows.
DROP FUNCTION IF EXISTS qsys2.stack_info(varchar, varchar);
CREATE FUNCTION qsys2.stack_info(job_name varchar DEFAULT '*', thread varchar DEFAULT 'ALL')
RETURNS TABLE (thread_id bigint, ordinal_position integer, entry_type varchar(10),
    program_name varchar(10), program_library_name varchar(10),
    statement_identifiers varchar(109), control_boundary varchar(3),
    module_name varchar(10), module_library_name varchar(10),
    procedure_name varchar(4096), activation_group_name varchar(10))
LANGUAGE sql STABLE AS $as4tainr$
  SELECT CAST(1 AS bigint), CAST(e.n AS integer),
         CAST(CASE WHEN coalesce(e.v ->> 'module', '') = '' THEN 'OPM' ELSE 'ILE' END AS varchar(10)),
         CAST(e.v ->> 'pgm' AS varchar(10)), CAST(e.v ->> 'lib' AS varchar(10)),
         CAST(nullif(e.v ->> 'line', '0') AS varchar(109)),
         CAST(CASE WHEN e.v ->> 'actgrp' IS DISTINCT FROM lag(e.v ->> 'actgrp') OVER (ORDER BY e.n)
                     OR coalesce(lag(e.v ->> 'module') OVER (ORDER BY e.n), '') = ''
                   THEN 'YES' ELSE 'NO' END AS varchar(3)),
         CAST(nullif(e.v ->> 'module', '') AS varchar(10)),
         CAST(CASE WHEN coalesce(e.v ->> 'module', '') = '' THEN NULL ELSE e.v ->> 'lib' END AS varchar(10)),
         CAST(nullif(e.v ->> 'proc', '') AS varchar(4096)),
         CAST(e.v ->> 'actgrp' AS varchar(10))
    FROM jsonb_array_elements(
           CASE WHEN upper(trim(coalesce(job_name, '*'))) = '*'
                THEN coalesce(nullif(current_setting('as4tainr.stack', true), ''), '[]')::jsonb
                ELSE '[]'::jsonb END) WITH ORDINALITY AS e(v, n)
   ORDER BY e.n DESC
$as4tainr$;

DROP FUNCTION IF EXISTS qsys2.ifs_object_statistics(varchar, varchar, varchar, varchar, varchar);
CREATE FUNCTION qsys2.ifs_object_statistics(start_path_name varchar,
    subtree_directories varchar DEFAULT 'YES', object_type_list varchar DEFAULT NULL,
    omit_list varchar DEFAULT NULL, ignore_errors varchar DEFAULT 'YES')
RETURNS TABLE (path_name varchar(16000), object_type varchar(10), symbolic_link varchar(16000),
    object_owner varchar(10), create_timestamp timestamp, data_change_timestamp timestamp,
    object_change_timestamp timestamp, data_size bigint, ccsid integer)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
  WITH arg AS (
    SELECT lower(as4tainr_svc.ifs_path(start_path_name)) AS k,
           upper(coalesce(subtree_directories, 'YES')) <> 'NO' AS subtree,
           ARRAY(SELECT t FROM regexp_split_to_table(upper(trim(coalesce(object_type_list, ''))), '[ ,]+') AS t
                  WHERE t <> '') AS types,
           ARRAY(SELECT lower(as4tainr_svc.ifs_path(t))
                   FROM regexp_split_to_table(trim(coalesce(omit_list, '')), ' +') AS t
                  WHERE t <> '') AS omit
  ),
  objs AS (
    SELECT f.*, CASE f.kind WHEN 'D' THEN '*DIR' WHEN 'F' THEN '*STMF' ELSE '*SYMLNK' END AS t
      FROM as4tainr.ifs f, arg
     WHERE f.path_key = arg.k
        OR (f.path_key LIKE CASE WHEN arg.k = '/' THEN '/' ELSE arg.k || '/' END || '%'
            AND (arg.subtree
                 OR position('/' IN substr(f.path_key,
                      length(CASE WHEN arg.k = '/' THEN '/' ELSE arg.k || '/' END) + 1)) = 0))
  )
  SELECT CAST(o.path AS varchar(16000)), CAST(o.t AS varchar(10)), CAST(NULL AS varchar(16000)),
         CAST(o.owner AS varchar(10)), o.created::timestamp, o.changed::timestamp,
         o.changed::timestamp, octet_length(o.data)::bigint,
         CASE WHEN o.kind = 'F' THEN o.ccsid END
    FROM objs o, arg
   WHERE (cardinality(arg.types) = 0 OR '*ALLOBJ' = ANY (arg.types) OR o.t = ANY (arg.types)
          OR ('*ALLDIR' = ANY (arg.types) AND o.t = '*DIR')
          OR ('*ALLSTMF' = ANY (arg.types) AND o.t = '*STMF'))
     AND NOT EXISTS (SELECT 1 FROM unnest(arg.omit) AS m
                      WHERE o.path_key = m OR o.path_key LIKE m || '/%')
   ORDER BY o.path_key
$as4tainr$;
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
        keywords: String::new(),
        reference: None,
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

/// QSYS2.QSQLIBL() (the table function under LIBRARY_LIST_INFO, with its
/// columns), QSYS2.QSYUSRINFO(USERNAME) and QSYS2.PROGRAM_EXPORT_IMPORT_INFO
/// with the columns and types PUB400's give (2026-11-09, verify66
/// n66_mapepire "usr", "pei": QSYUSRINFO has no AUTHORIZATION_NAME, flags
/// as Y/N, CHARACTER_CODE_SET_ID -2 for *SYSVAL). Values as4tainr does not
/// keep are null or the system default; SPECIAL_AUTHORITIES is a Y/N flag
/// per special authority in the order *ALLOBJ *SECADM *SAVSYS *JOBCTL
/// *SPLCTL *SERVICE *AUDIT *IOSYSCFG (as4tainr's reading of the QSYRUSRI
/// format). A service program's exports are *PROCEXP, its and a program's
/// imports *PROCIMP.
const VIEW_FUNCTIONS: &str = r#"
DROP FUNCTION IF EXISTS qsys2.qsyusrinfo(varchar);
CREATE FUNCTION qsys2.qsyusrinfo(username varchar)
RETURNS TABLE (
  previous_signon timestamp,
  sign_on_attempts_not_valid integer,
  maximum_sign_on_attempts varchar(7),
  status varchar(10),
  netserver_disabled varchar(3),
  password_change_date timestamp,
  no_password_indicator char(1),
  password_level_0_1 char(1),
  password_level_2_3 char(1),
  password_level_4 char(1),
  password_expiration_interval smallint,
  date_password_expires timestamp,
  days_until_password_expires smallint,
  set_password_to_expire char(1),
  user_class_name varchar(10),
  special_authorities varchar(15),
  group_profile_name varchar(10),
  owner varchar(10),
  group_authority varchar(10),
  assistance_level varchar(10),
  current_library_name varchar(10),
  initial_menu_name varchar(10),
  initial_menu_library_name varchar(10),
  initial_program_name varchar(10),
  initial_program_library_name varchar(10),
  limit_capabilities varchar(10),
  text_description varchar(50),
  display_signon_information varchar(10),
  limit_device_sessions varchar(10),
  keyboard_buffering varchar(10),
  maximum_allowed_storage bigint,
  storage_used bigint,
  highest_scheduling_priority char(1),
  job_description_name varchar(10),
  job_description_library_name varchar(10),
  accounting_code varchar(15),
  message_queue_name varchar(10),
  message_queue_library_name varchar(10),
  message_queue_delivery_method varchar(10),
  message_queue_severity smallint,
  output_queue_name varchar(10),
  output_queue_library_name varchar(10),
  print_device varchar(10),
  special_environment varchar(10),
  attention_key_handling_program_name varchar(10),
  attention_key_handling_program_library_name varchar(10),
  language_id varchar(10),
  country_or_region_id varchar(10),
  character_code_set_id integer,
  user_options varchar(36),
  sort_sequence_table_name varchar(10),
  sort_sequence_table_library_name varchar(10),
  object_auditing_value varchar(10),
  user_action_audit_level varchar(64),
  group_authority_type varchar(10),
  user_id_number bigint,
  group_id_number bigint,
  locale_job_attributes varchar(16),
  group_member_indicator char(1),
  digital_certificate_indicator char(1),
  character_identifier_control varchar(10),
  local_password_management char(1),
  block_password_change varchar(10),
  user_entitlement_required char(1),
  user_expiration_interval smallint,
  user_expiration_date timestamp,
  user_expiration_action varchar(8),
  home_directory varchar(1024),
  locale_path_name varchar(1024),
  number_supplemental_groups smallint,
  supplemental_groups varchar(150),
  number_iasp_storage_usage_descriptors smallint,
  iasp_storage_usage_descriptors text,
  user_default_password char(1),
  authority_collection_active_indicator char(1),
  authority_collection_repository_exists_indicator char(1),
  pase_shell varchar(1024),
  available_profile_entries bigint,
  total_profile_entries bigint,
  owner_entries bigint,
  private_authority_entries bigint,
  authorized_user_entries bigint,
  primary_group_entries bigint,
  temp_available_profile_entries bigint,
  temp_total_profile_entries bigint,
  temp_owner_entries bigint,
  temp_private_authority_entries bigint,
  temp_authorized_user_entries bigint,
  temp_primary_group_entries bigint)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
  SELECT CAST(u.last_signon AS timestamp),
         CAST(u.invalid_signons AS integer),
         CAST('*SYSVAL' AS varchar(7)),
         CAST(u.status AS varchar(10)),
         CAST('NO' AS varchar(3)),
         CAST(u.pwd_changed AS timestamp),
         CAST(CASE WHEN u.password_hash = '*NONE' THEN 'Y' ELSE 'N' END AS char(1)),
         CAST('N' AS char(1)),
         CAST('Y' AS char(1)),
         CAST('Y' AS char(1)),
         CAST(0 AS smallint),
         CAST(NULL AS timestamp),
         CAST(-1 AS smallint),
         CAST(CASE WHEN u.pwd_expired THEN 'Y' ELSE 'N' END AS char(1)),
         CAST(u.usrcls AS varchar(10)),
         CAST((SELECT string_agg(CASE WHEN a = ANY(u.special_auth) THEN 'Y' ELSE 'N' END, '' ORDER BY i) FROM unnest(ARRAY['*ALLOBJ','*SECADM','*SAVSYS','*JOBCTL','*SPLCTL','*SERVICE','*AUDIT','*IOSYSCFG']) WITH ORDINALITY AS x(a, i)) AS varchar(15)),
         CAST('*NONE' AS varchar(10)),
         CAST('*USRPRF' AS varchar(10)),
         CAST('*NONE' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST(u.curlib AS varchar(10)),
         CAST(CASE WHEN position('/' IN u.inlmnu) > 0 THEN split_part(u.inlmnu, '/', 2) ELSE u.inlmnu END AS varchar(10)),
         CAST(CASE WHEN position('/' IN u.inlmnu) > 0 THEN split_part(u.inlmnu, '/', 1) WHEN u.inlmnu = '*SIGNOFF' THEN NULL ELSE '*LIBL' END AS varchar(10)),
         CAST(CASE WHEN position('/' IN u.inlpgm) > 0 THEN split_part(u.inlpgm, '/', 2) ELSE u.inlpgm END AS varchar(10)),
         CAST(CASE WHEN position('/' IN u.inlpgm) > 0 THEN split_part(u.inlpgm, '/', 1) WHEN u.inlpgm = '*NONE' THEN NULL ELSE '*LIBL' END AS varchar(10)),
         CAST('*NO' AS varchar(10)),
         CAST(nullif(u.text, '') AS varchar(50)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST(-1 AS bigint),
         CAST(0 AS bigint),
         CAST('3' AS char(1)),
         CAST('QDFTJOBD' AS varchar(10)),
         CAST('QGPL' AS varchar(10)),
         CAST(NULL AS varchar(15)),
         CAST(u.name AS varchar(10)),
         CAST('QUSRSYS' AS varchar(10)),
         CAST('*NOTIFY' AS varchar(10)),
         CAST(0 AS smallint),
         CAST(u.outq AS varchar(10)),
         CAST(NULL AS varchar(10)),
         CAST('*WRKSTN' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST(NULL AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST(-2 AS integer),
         CAST(NULL AS varchar(36)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST(NULL AS varchar(10)),
         CAST('*NONE' AS varchar(10)),
         CAST(NULL AS varchar(64)),
         CAST('*PRIVATE' AS varchar(10)),
         CAST(NULL AS bigint),
         CAST(0 AS bigint),
         CAST(NULL AS varchar(16)),
         CAST('0' AS char(1)),
         CAST('0' AS char(1)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('Y' AS char(1)),
         CAST('*SYSVAL' AS varchar(10)),
         CAST('1' AS char(1)),
         CAST(0 AS smallint),
         CAST(NULL AS timestamp),
         CAST('*NONE' AS varchar(8)),
         CAST('/home/' || u.name AS varchar(1024)),
         CAST('*SYSVAL' AS varchar(1024)),
         CAST(0 AS smallint),
         CAST(NULL AS varchar(150)),
         CAST(0 AS smallint),
         CAST(NULL AS text),
         CAST(NULL AS char(1)),
         CAST('0' AS char(1)),
         CAST('0' AS char(1)),
         CAST(NULL AS varchar(1024)),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint)
    FROM as4tainr.user_profiles u
   WHERE u.name = upper(rtrim($1))
$$;
DROP FUNCTION IF EXISTS qsys2.program_export_import_info(varchar, varchar, varchar);
CREATE FUNCTION qsys2.program_export_import_info(
  program_library varchar, program_name varchar, object_type varchar DEFAULT '*ALL')
RETURNS TABLE (
  program_library varchar(10), program_name varchar(10), object_type varchar(7),
  symbol_name varchar(8192), symbol_usage varchar(10), argument_optimization varchar(4),
  data_item_size integer)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
  WITH lib AS (
    -- QTEMP: the statement's job's own (qtemp.rs name_for: #QTnnnnnn, #QPnnnnnnn).
    SELECT CASE WHEN upper(rtrim($1)) <> 'QTEMP' THEN upper(rtrim($1))
                WHEN coalesce(current_setting('as4tainr.job_key', true), '') LIKE 'PID%'
                THEN '#QP' || lpad((substr(current_setting('as4tainr.job_key', true), 4)::bigint % 10000000)::text, 7, '0')
                ELSE '#QT' || lpad((split_part(coalesce(current_setting('as4tainr.job_key', true), '0'), '/', 1)::bigint % 1000000)::text, 6, '0')
                END AS l),
  objs AS (
    SELECT p.library, p.name, '*SRVPGM' AS t, p.binding::jsonb AS b
      FROM as4tainr.service_programs p, lib WHERE p.library = lib.l AND p.name = upper(rtrim($2))
       AND upper($3) IN ('*SRVPGM', '*ALL')
    UNION ALL
    SELECT p.library, p.name, '*PGM', p.binding::jsonb
      FROM as4tainr.programs p, lib WHERE p.library = lib.l AND p.name = upper(rtrim($2))
       AND p.binding IS NOT NULL AND upper($3) IN ('*PGM', '*ALL'))
  SELECT CAST(CASE WHEN upper(rtrim($1)) = 'QTEMP' THEN 'QTEMP' ELSE o.library END AS varchar(10)),
         CAST(o.name AS varchar(10)), CAST(o.t AS varchar(7)),
         CAST(e.value AS varchar(8192)), CAST('*PROCEXP' AS varchar(10)), CAST('*NO' AS varchar(4)),
         CAST(NULL AS integer)
    FROM objs o, jsonb_array_elements_text(coalesce(o.b->'exports', '[]'::jsonb)) AS e(value)
  UNION ALL
  SELECT CAST(CASE WHEN upper(rtrim($1)) = 'QTEMP' THEN 'QTEMP' ELSE o.library END AS varchar(10)),
         CAST(o.name AS varchar(10)), CAST(o.t AS varchar(7)),
         CAST(i.key AS varchar(8192)), CAST('*PROCIMP' AS varchar(10)), CAST('*NO' AS varchar(4)),
         CAST(NULL AS integer)
    FROM objs o, jsonb_each(coalesce(o.b->'imports', '{}'::jsonb)) AS i
$$;

CREATE OR REPLACE FUNCTION qsys2.qsqlibl() RETURNS SETOF qsys2.library_list_info
LANGUAGE sql STABLE AS $$ SELECT * FROM qsys2.library_list_info $$;
"#;

/// The QSYS2.QCMDEXC scalar function (IBM i services: 1 when the command
/// ran, -1 when it failed) in a query that calls it for its rows, as
/// Mapepire runs one (mapepire.rs): the statement runs once to gather the
/// commands (each new one is recorded, its value null), as4tainr runs
/// them, and the statement runs again with their results. A statement
/// that is not run this way (no gathering row for its job) gets SQL0901.
const QCMDEXC_FUNCTION: &str = r#"
CREATE UNLOGGED TABLE IF NOT EXISTS as4tainr.qcmdexc_calls (
    job  text NOT NULL,
    seq  bigserial,
    cmd  text NOT NULL,
    rc   integer,
    PRIMARY KEY (job, cmd)
);
CREATE OR REPLACE FUNCTION qsys2.qcmdexc(cmd varchar) RETURNS integer
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  j text := coalesce(current_setting('as4tainr.job_key', true), '');
  r integer;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM as4tainr.qcmdexc_calls c WHERE c.job = j AND c.cmd = '') THEN
    RAISE EXCEPTION 'SQL0901: QSYS2.QCMDEXC is called for the rows of this statement: that is not supported here.'
      USING ERRCODE = '58004';
  END IF;
  SELECT c.rc INTO r FROM as4tainr.qcmdexc_calls c WHERE c.job = j AND c.cmd = qcmdexc.cmd;
  IF FOUND THEN
    RETURN r;
  END IF;
  INSERT INTO as4tainr.qcmdexc_calls (job, cmd) VALUES (j, qcmdexc.cmd) ON CONFLICT DO NOTHING;
  RETURN NULL;
END $$;
"#;

/// QSYS2.SPOOLED_FILE_INFO (IBM i services): the spooled files of a user
/// (*CURRENT: the statement's job's user; *ALL), created from a moment
/// on, with a status (*READY, *HELD, *OPEN... or *ALL), on an output
/// queue, with user data. The columns, their order and types as PUB400's
/// function gives them (2026-11-09, verify64 m64_mapepire "sfi"); size,
/// system and IPP job id are null here, priority 5, copies 1, form *STD.
const SPOOLED_FILE_INFO: &str = r#"
DROP FUNCTION IF EXISTS qsys2.spooled_file_info(varchar, timestamp, timestamp, varchar, varchar, varchar, varchar, varchar);
CREATE FUNCTION qsys2.spooled_file_info(
  user_name varchar DEFAULT '*CURRENT', starting_timestamp timestamp DEFAULT NULL,
  ending_timestamp timestamp DEFAULT NULL, status varchar DEFAULT '*ALL',
  output_queue varchar DEFAULT '*ALL', user_data varchar DEFAULT '*ALL',
  form_type varchar DEFAULT '*ALL', system_name varchar DEFAULT '*ALL')
RETURNS TABLE (
  spooled_file_name varchar(10), spooled_file_number integer, status varchar(15),
  output_priority integer, creation_timestamp timestamp(0), user_data varchar(10),
  size bigint, total_pages integer, copies integer, qualified_job_name varchar(28),
  job_name varchar(10), job_user varchar(10), job_number varchar(6),
  file_available varchar(8), form_type varchar(10), output_queue_library varchar(10),
  output_queue varchar(10), asp_number integer, system varchar(8),
  internet_print_protocol_job_id integer)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
  SELECT CAST(s.file AS varchar(10)), s.number,
         CAST(CASE s.status WHEN 'RDY' THEN 'READY' WHEN 'HLD' THEN 'HELD' WHEN 'OPN' THEN 'OPEN'
                            WHEN 'SAV' THEN 'SAVED' WHEN 'WTR' THEN 'WRITING' ELSE s.status END AS varchar(15)),
         5, CAST(s.created AS timestamp(0)), CAST(nullif(s.user_data, '') AS varchar(10)),
         CAST(NULL AS bigint), s.pages, 1, CAST(s.job_name AS varchar(28)),
         CAST(split_part(s.job_name, '/', 3) AS varchar(10)),
         CAST(split_part(s.job_name, '/', 2) AS varchar(10)),
         CAST(split_part(s.job_name, '/', 1) AS varchar(6)),
         CAST(CASE WHEN s.status = 'OPN' THEN '*FILEEND' ELSE '*FILEEND' END AS varchar(8)),
         CAST('*STD' AS varchar(10)),
         CAST(s.outq_lib AS varchar(10)), CAST(s.outq AS varchar(10)), 1,
         CAST(NULL AS varchar(8)), CAST(NULL AS integer)
    FROM as4tainr.spooled_files s
   WHERE (upper($1) = '*ALL'
          OR s.user_name = CASE WHEN upper($1) = '*CURRENT'
                                THEN current_setting('as4tainr.user', true) ELSE upper($1) END)
     AND ($2 IS NULL OR s.created::timestamp >= $2)
     AND ($3 IS NULL OR s.created::timestamp <= $3)
     AND (upper($4) = '*ALL'
          OR s.status = CASE upper($4) WHEN '*READY' THEN 'RDY' WHEN '*HELD' THEN 'HLD'
                                        WHEN '*OPEN' THEN 'OPN' WHEN '*SAVED' THEN 'SAV'
                                        WHEN '*WRITING' THEN 'WTR' ELSE upper($4) END)
     AND (upper($5) = '*ALL' OR s.outq = upper(split_part($5, '/', 2)) OR s.outq = upper($5))
     AND (upper($6) = '*ALL' OR rtrim(s.user_data) = rtrim($6))
   ORDER BY s.created, s.id
$$;
"#;

/// QSYS2.LIBRARY_INFO(LIBRARY_NAME [, IGNORE_ERRORS] [, DETAILED_INFO])
/// (IBM i services): a library's description. as4tainr has one ASP: the
/// IASP name is *SYSBAS (Code for IBM i takes *SYSBAS as "not in an
/// independent ASP"). The columns and their types as PUB400's function
/// gives them (2026-11-09, verify64 m64_mapepire "li"); journaling and
/// create authority values are as4tainr's (*SYSVAL, NO).
const LIBRARY_INFO: &str = r#"
DROP FUNCTION IF EXISTS qsys2.library_info(varchar, varchar, varchar);
CREATE FUNCTION qsys2.library_info(
  library_name varchar, ignore_errors varchar DEFAULT 'NO', detailed_info varchar DEFAULT 'NO')
RETURNS TABLE (
  object_count integer, library_size bigint, library_size_complete varchar(3),
  library_type char(4), text_description varchar(50), iasp_name varchar(10),
  iasp_number integer, create_authority varchar(10), object_audit_create varchar(7),
  journaled varchar(3), journal_library varchar(10), journal_name varchar(10),
  inherit_journaling varchar(3), journal_inherit_rules integer,
  journal_start_timestamp timestamp, apply_starting_receiver_library varchar(10),
  apply_starting_receiver varchar(10), apply_starting_receiver_asp varchar(10))
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  lib text := upper(trim(library_name));
BEGIN
  IF NOT EXISTS (SELECT 1 FROM as4tainr.objects o
                  WHERE o.library = 'QSYS' AND o.objtype = '*LIB' AND o.name = lib) THEN
    IF upper(coalesce(ignore_errors, 'NO')) = 'YES' THEN
      RETURN;
    END IF;
    RAISE EXCEPTION 'SQL0204: % in QSYS type *LIB not found.', lib USING ERRCODE = '42704';
  END IF;
  -- DETAILED_INFO NO: the size is not worked out (null), as on PUB400.
  RETURN QUERY
  SELECT CAST((SELECT count(*) FROM as4tainr.objects x WHERE x.library = lib) AS integer),
         CAST(NULL AS bigint), CAST(NULL AS varchar(3)),
         CAST(CASE WHEN o.attribute = 'TEST' THEN 'TEST' ELSE 'PROD' END AS char(4)),
         CAST(nullif(o.text, '') AS varchar(50)),
         CAST('*SYSBAS' AS varchar(10)), 1,
         CAST('*SYSVAL' AS varchar(10)), CAST(NULL AS varchar(7)),
         CAST('NO' AS varchar(3)), CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10)),
         CAST('NO' AS varchar(3)), 0, CAST(NULL AS timestamp),
         CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10))
    FROM as4tainr.objects o
   WHERE o.library = 'QSYS' AND o.objtype = '*LIB' AND o.name = lib;
END $$;
"#;

/// QSYS2.PROGRAM_INFO: a program's or service program's facts, in the
/// 107 columns and types of IBM i 7.5's view (PUB400 2026-11-09, verify66
/// n66_mapepire and verify68 o68_mapepire "pgmi": QSYS/QCMDEXC). What
/// as4tainr keeps (library, name, type, attribute, owner, text, created,
/// source member) is filled in; the rest, which describes the compiled
/// object IBM's compilers make, is null.
const PROGRAM_INFO: &str = r#"
DROP VIEW IF EXISTS qsys2.program_info CASCADE;
CREATE VIEW qsys2.program_info AS
SELECT
       CAST(o.library AS varchar(10)) COLLATE "C" AS program_library,
       CAST(o.name AS varchar(10)) COLLATE "C" AS program_name,
       CAST(CASE WHEN (o.objtype = '*SRVPGM' OR o.attribute IN ('RPGLE', 'SQLRPGLE', 'CLLE', 'CBLLE', 'SQLCBLLE', 'CLE', 'CPPLE')) THEN 'ILE' ELSE 'OPM' END AS varchar(3)) COLLATE "C" AS program_type,
       CAST(o.objtype AS varchar(7)) COLLATE "C" AS object_type,
       CAST(o.created AS timestamp(0)) AS create_timestamp,
       CAST(nullif(o.text, '') AS varchar(50)) COLLATE "C" AS text_description,
       CAST(o.owner AS varchar(10)) COLLATE "C" AS program_owner,
       CAST(nullif(o.attribute, '') AS varchar(10)) COLLATE "C" AS program_attribute,
       CAST('*USER' AS varchar(6)) COLLATE "C" AS user_profile,
       CAST('*YES' AS varchar(4)) COLLATE "C" AS use_adopted_authority,
       CAST('*USER' AS varchar(8)) COLLATE "C" AS program_state,
       CAST('*USER' AS varchar(7)) COLLATE "C" AS program_domain,
       CAST('V7R5M0' AS char(6)) COLLATE "C" AS earliest_possible_release,
       CAST(NULL AS char(6)) COLLATE "C" AS release_created_on,
       CAST(NULL AS char(6)) COLLATE "C" AS target_release,
       CAST(NULL AS integer) AS minimum_number_parms,
       CAST(NULL AS integer) AS maximum_number_parms,
       CAST(NULL AS integer) AS associated_space_size,
       CAST('*USER' AS varchar(8)) COLLATE "C" AS paging_pool,
       CAST('*BLOCK' AS varchar(8)) COLLATE "C" AS paging_amount,
       CAST(NULL AS varchar(4)) COLLATE "C" AS allow_rtvclsrc,
       CAST('*NO' AS varchar(4)) COLLATE "C" AS conversion_required,
       CAST(NULL AS varchar(8)) COLLATE "C" AS conversion_detail,
       CAST(NULL AS varchar(10)) COLLATE "C" AS program_entry_procedure_module_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS program_entry_procedure_module,
       CAST(NULL AS varchar(10)) COLLATE "C" AS activation_group,
       CAST(NULL AS varchar(4)) COLLATE "C" AS shared_activation_group,
       CAST(NULL AS varchar(4)) COLLATE "C" AS observable_info_compressed,
       CAST(NULL AS varchar(4)) COLLATE "C" AS runtime_info_compressed,
       CAST(NULL AS varchar(4)) COLLATE "C" AS allow_update,
       CAST(NULL AS varchar(4)) COLLATE "C" AS allow_bound_srvpgm_library_update,
       CAST(NULL AS varchar(6)) COLLATE "C" AS all_creation_data,
       CAST(NULL AS varchar(10)) COLLATE "C" AS profiling_data,
       CAST(NULL AS varchar(5)) COLLATE "C" AS teraspace_storage_enabled_modules,
       CAST(NULL AS varchar(4)) COLLATE "C" AS teraspace_storage_enabled_pep,
       CAST(NULL AS varchar(10)) COLLATE "C" AS storage_model,
       CAST(NULL AS varchar(4)) COLLATE "C" AS argument_optimization,
       CAST(NULL AS integer) AS number_of_unresolved_references,
       CAST(NULL AS varchar(4)) COLLATE "C" AS allow_static_storage_reinit,
       CAST(NULL AS bigint) AS minimum_static_storage_size,
       CAST(NULL AS bigint) AS maximum_static_storage_size,
       CAST(NULL AS integer) AS auxiliary_storage_segments,
       CAST(NULL AS integer) AS maximum_auxiliary_storage_segments,
       CAST(NULL AS integer) AS program_size,
       CAST(NULL AS integer) AS maximum_program_size,
       CAST(NULL AS integer) AS modules,
       CAST(NULL AS integer) AS maximum_modules,
       CAST(NULL AS integer) AS service_programs,
       CAST(NULL AS integer) AS maximum_service_programs,
       CAST(NULL AS integer) AS string_directory_size,
       CAST(NULL AS integer) AS maximum_string_directory_size,
       CAST(NULL AS integer) AS copyrights,
       CAST(NULL AS text) AS copyright_strings,
       CAST(NULL AS integer) AS copyright_string_size,
       CAST(NULL AS integer) AS maximum_copyright_string_size,
       CAST(NULL AS varchar(10)) COLLATE "C" AS export_source_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS export_source_file,
       CAST(NULL AS varchar(10)) COLLATE "C" AS export_source_file_member,
       CAST(NULL AS varchar(5000)) COLLATE "C" AS export_source_stream_file,
       CAST(NULL AS integer) AS procedure_exports,
       CAST(NULL AS integer) AS maximum_procedure_exports,
       CAST(NULL AS integer) AS data_exports,
       CAST(NULL AS integer) AS maximum_data_exports,
       CAST(NULL AS integer) AS signatures,
       CAST(NULL AS bytea) AS export_signatures,
       CAST(NULL AS integer) AS maximum_signatures,
       CAST(nullif(coalesce(nullif(p.src_lib, ''), o.src_lib), '') AS varchar(10)) COLLATE "C" AS source_file_library,
       CAST(nullif(coalesce(nullif(p.src_file, ''), o.src_file), '') AS varchar(10)) COLLATE "C" AS source_file,
       CAST(nullif(coalesce(nullif(p.src_mbr, ''), o.src_mbr), '') AS varchar(10)) COLLATE "C" AS source_file_member,
       CAST(p.src_changed AS timestamp(0)) AS source_file_change_timestamp,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sort_sequence_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sort_sequence,
       CAST(NULL AS varchar(7)) COLLATE "C" AS language_id,
       CAST('*NONE' AS varchar(6)) COLLATE "C" AS observable,
       CAST(NULL AS varchar(11)) COLLATE "C" AS optimization,
       CAST(NULL AS varchar(4)) COLLATE "C" AS log_commands,
       CAST(NULL AS varchar(4)) COLLATE "C" AS fix_decimal_data,
       CAST(NULL AS varchar(10)) COLLATE "C" AS update_pasa,
       CAST(NULL AS varchar(10)) COLLATE "C" AS clear_pasa,
       CAST(NULL AS varchar(14)) COLLATE "C" AS compiler_id,
       CAST(NULL AS varchar(4)) COLLATE "C" AS teraspace_storage_enabled_program,
       CAST(NULL AS integer) AS opm_program_size,
       CAST(NULL AS integer) AS static_storage_size,
       CAST(NULL AS integer) AS automatic_storage_size,
       CAST(NULL AS integer) AS number_mi_instructions,
       CAST(NULL AS integer) AS number_mi_odt_entries,
       CAST(NULL AS integer) AS sql_statement_count,
       CAST(NULL AS varchar(18)) COLLATE "C" AS sql_relational_database,
       CAST(NULL AS varchar(5)) COLLATE "C" AS sql_commitment_control,
       CAST(NULL AS varchar(4)) COLLATE "C" AS sql_naming,
       CAST(NULL AS varchar(4)) COLLATE "C" AS sql_date_format,
       CAST(NULL AS char(1)) COLLATE "C" AS sql_date_separator,
       CAST(NULL AS varchar(4)) COLLATE "C" AS sql_time_format,
       CAST(NULL AS char(1)) COLLATE "C" AS sql_time_separator,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_sort_sequence_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_sort_sequence,
       CAST(NULL AS varchar(7)) COLLATE "C" AS sql_language_id,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_default_schema,
       CAST(NULL AS varchar(3483)) COLLATE "C" AS sql_path,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_dynamic_user_profile,
       CAST(NULL AS varchar(9)) COLLATE "C" AS sql_allow_copy_data,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_close_sql_cursor,
       CAST(NULL AS varchar(4)) COLLATE "C" AS sql_delay_prepare,
       CAST(NULL AS varchar(8)) COLLATE "C" AS sql_allow_block,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_package_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sql_package,
       CAST(NULL AS varchar(4)) COLLATE "C" AS sql_rdb_connection_method
  FROM as4tainr.objects o
  LEFT JOIN as4tainr.programs p ON p.library = o.library AND p.name = o.name
 WHERE o.objtype IN ('*PGM', '*SRVPGM');
"#;

/// QSYS2.SYSFILES (Db2 for i catalog: the database files) over the
/// physical and logical files, with the 104 columns and types of PUB400's
/// (2026-11-09, verify66 n66_mapepire "sysf"): FILE_TYPE SOURCE or DATA,
/// record length (a source file's RCDLEN), members, key fields, fields,
/// CCSID 37, the CONTAINS_ columns from the fields, release V7R5M0 and
/// language ENU (verify68 o68_mapepire "sysf"); what as4tainr does not
/// keep is null. Also as the table
/// function SYSFILES(schema, file) Code for IBM i calls (objectBrowser.ts).
///
/// QSYS2.PARTITION_STATISTICS(schema, table) (the table function under
/// SYSPARTITIONSTAT): one row per member: a source file's members, with
/// their type, text, lines and dates; a data file's one member, named as
/// the file.
const FILE_CATALOG: &str = r#"
DROP VIEW IF EXISTS qsys2.sysfiles CASCADE;
CREATE VIEW qsys2.sysfiles AS
SELECT
       CAST(o.library AS varchar(10)) COLLATE "C" AS system_table_schema,
       CAST(o.name AS varchar(10)) COLLATE "C" AS system_table_name,
       CAST(o.library AS varchar(128)) COLLATE "C" AS table_schema,
       CAST(coalesce(f.sql_name, o.name) AS varchar(128)) COLLATE "C" AS table_name,
       CAST(1 AS integer) AS iasp_number,
       CAST(nullif(o.text, '') AS varchar(50)) COLLATE "C" AS text_description,
       CAST(CASE WHEN o.attribute LIKE 'LF%' THEN 'LOGICAL' ELSE 'PHYSICAL' END AS varchar(8)) COLLATE "C" AS native_type,
       CAST(CASE WHEN sf.name IS NOT NULL OR o.attribute = 'PF-SRC' THEN 'SOURCE' ELSE 'DATA' END AS varchar(6)) COLLATE "C" AS file_type,
       CAST(CASE WHEN f.view IS NOT NULL THEN 'VIEW' WHEN f.sql_type = 'TABLE' THEN 'TABLE' END AS varchar(5)) COLLATE "C" AS sql_object_type,
       CAST(o.created AS timestamp(0)) AS last_altered_timestamp,
       CAST(NULL AS char(13)) COLLATE "C" AS file_level_id,
       CAST('YES' AS varchar(3)) COLLATE "C" AS level_check,
       CAST(o.owner AS varchar(10)) COLLATE "C" AS file_owner,
       CAST(NULL AS varchar(10)) COLLATE "C" AS create_public_authority,
       CAST(CASE WHEN sf.name IS NOT NULL THEN (SELECT count(*) FROM as4tainr.source_members m WHERE m.library = o.library AND m.file = o.name) ELSE 1 END AS integer) AS number_members,
       CAST(CASE WHEN sf.name IS NOT NULL THEN NULL ELSE 1 END AS integer) AS maximum_members,
       CAST(coalesce(sf.rcdlen, t.row_length) AS integer) AS maximum_record_length,
       CAST((SELECT count(*) FROM as4tainr.file_keys k WHERE k.library = o.library AND k.file = o.name) AS integer) AS number_key_fields,
       CAST(CASE WHEN sf.name IS NULL AND NOT EXISTS (SELECT 1 FROM as4tainr.file_keys k WHERE k.library = o.library AND k.file = o.name) THEN 0 END AS integer) AS maximum_key_length,
       CAST(0 AS integer) AS trigger_count,
       CAST(0 AS integer) AS constraint_count,
       CAST(NULL AS integer) AS number_based_on_files,
       CAST(NULL AS text) AS based_on_files,
       CAST('YES' AS varchar(3)) COLLATE "C" AS allow_read,
       CAST('YES' AS varchar(3)) COLLATE "C" AS allow_write,
       CAST('YES' AS varchar(3)) COLLATE "C" AS allow_update,
       CAST('YES' AS varchar(3)) COLLATE "C" AS allow_delete,
       CAST(30 AS integer) AS maximum_file_wait_time,
       CAST(60 AS integer) AS maximum_record_wait_time,
       CAST(NULL AS integer) AS force_write_ratio,
       CAST(CASE WHEN t.select_omit = 'Y' THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS select_omit,
       CAST('NO' AS varchar(3)) COLLATE "C" AS program_described,
       CAST('NO' AS varchar(3)) COLLATE "C" AS distributed,
       CAST('V7R5M0' AS char(6)) COLLATE "C" AS file_vrm,
       CAST(NULL AS char(6)) COLLATE "C" AS earliest_possible_release,
       CAST(NULL AS varchar(3)) COLLATE "C" AS allow_null_keys,
       CAST('YES' AS varchar(3)) COLLATE "C" AS allow_null_data,
       CAST(NULL AS varchar(3)) COLLATE "C" AS primary_key,
       CAST(NULL AS varchar(3)) COLLATE "C" AS unique_constraint,
       CAST('NO' AS varchar(3)) COLLATE "C" AS restrict_on_drop,
       CAST('NO' AS varchar(3)) COLLATE "C" AS volatile,
       CAST('NO' AS varchar(3)) COLLATE "C" AS keep_in_memory,
       CAST('*ANY' AS varchar(4)) COLLATE "C" AS media_preference,
       CAST(NULL AS varchar(10)) COLLATE "C" AS source_file_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS source_file,
       CAST(NULL AS varchar(10)) COLLATE "C" AS source_file_member,
       CAST(CASE WHEN EXISTS (SELECT 1 FROM as4tainr.file_keys k WHERE k.library = o.library AND k.file = o.name) THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS access_path_keyed,
       CAST(NULL AS varchar(14)) COLLATE "C" AS access_path_type,
       CAST(NULL AS varchar(6)) COLLATE "C" AS access_path_maintenance,
       CAST(NULL AS varchar(7)) COLLATE "C" AS access_path_size,
       CAST(NULL AS integer) AS logical_page_size,
       CAST(NULL AS varchar(3)) COLLATE "C" AS force_keyed_access_path,
       CAST(NULL AS varchar(3)) COLLATE "C" AS access_path_journaled,
       CAST(NULL AS varchar(7)) COLLATE "C" AS access_path_recovery,
       CAST(NULL AS integer) AS srtseq_ind,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sort_sequence_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS sort_sequence,
       CAST('ENU' AS char(3)) COLLATE "C" AS language_identifier,
       CAST(NULL AS varchar(8)) COLLATE "C" AS rounding_mode,
       CAST(NULL AS varchar(3)) COLLATE "C" AS decfloat_warnings,
       CAST(1 AS integer) AS number_record_formats,
       CAST(NULL AS char(13)) COLLATE "C" AS format_level_id,
       CAST(coalesce(f.format, o.name) AS varchar(10)) COLLATE "C" AS format_name,
       CAST(coalesce(sf.rcdlen, t.row_length) AS integer) AS record_length,
       CAST(CASE WHEN sf.name IS NOT NULL THEN 3 ELSE (SELECT count(*) FROM as4tainr.file_fields ff WHERE ff.library = o.library AND ff.file = o.name) END AS integer) AS number_fields,
       CAST(37 AS integer) AS common_ccsid,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_explicit_ccsid,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_multiple_ccsids,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_unicode,
       CAST(CASE WHEN fx.varying THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_varying_length,
       CAST(CASE WHEN fx.datetime THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_datetime,
       CAST(CASE WHEN fx.graphic THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_graphic,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_lob,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_rowid,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_udt,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_datalink,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_datalink_file_link_control,
       CAST(CASE WHEN fx.nulls THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_null,
       CAST(CASE WHEN fx.defaults THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_default,
       CAST(CASE WHEN fx.identities THEN 'YES' ELSE 'NO' END AS varchar(3)) COLLATE "C" AS contains_identity,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_row_change_timestamp,
       CAST('NO' AS varchar(3)) COLLATE "C" AS contains_user_defined_function,
       CAST('NO' AS varchar(3)) COLLATE "C" AS allocate_storage,
       CAST(NULL AS varchar(3)) COLLATE "C" AS contiguous_storage,
       CAST(NULL AS integer) AS maximum_deleted_percentage,
       CAST(NULL AS integer) AS initial_records,
       CAST(NULL AS integer) AS increment_records,
       CAST(NULL AS integer) AS maximum_increments,
       CAST('YES' AS varchar(3)) COLLATE "C" AS reuse_deleted_records,
       CAST('NO' AS varchar(3)) COLLATE "C" AS materialized_query_table,
       CAST('NO' AS varchar(3)) COLLATE "C" AS partitioned_table,
       CAST('NO' AS varchar(3)) COLLATE "C" AS row_and_column_access_control,
       CAST(NULL AS integer) AS total_select_omit,
       CAST(NULL AS varchar(10)) COLLATE "C" AS fmtslr_library,
       CAST(NULL AS varchar(10)) COLLATE "C" AS fmtslr_program,
       CAST(NULL AS varchar(3)) COLLATE "C" AS is_join_logical,
       CAST(NULL AS varchar(3)) COLLATE "C" AS dynamic_selection,
       CAST(NULL AS varchar(8)) COLLATE "C" AS with_check_option,
       CAST(NULL AS varchar(3)) COLLATE "C" AS physical_lob,
       CAST(NULL AS varchar(3)) COLLATE "C" AS physical_datalink,
       CAST(NULL AS varchar(3)) COLLATE "C" AS index_column_is_expression,
       CAST(NULL AS varchar(3)) COLLATE "C" AS index_expression_has_udf,
       CAST(NULL AS varchar(3)) COLLATE "C" AS index_has_search_condition,
       CAST(NULL AS varchar(3)) COLLATE "C" AS index_search_condition_has_udf
  FROM as4tainr.objects o
  LEFT JOIN as4tainr.files f ON f.library = o.library AND f.name = o.name
  LEFT JOIN as4tainr.source_files sf ON sf.library = o.library AND sf.name = o.name
  LEFT JOIN qsys2.systables t ON t.system_table_schema = o.library AND t.system_table_name = o.name
  LEFT JOIN LATERAL (
       SELECT coalesce(bool_or(ff.varlen), false) AS varying,
              coalesce(bool_or(ff.data_type IN ('L', 'T', 'Z')), false) AS datetime,
              coalesce(bool_or(ff.data_type = 'G'), false) AS graphic,
              coalesce(bool_or(ff.null_capable), false) AS nulls,
              coalesce(bool_or(ff.dft IS NOT NULL), false) AS defaults,
              coalesce(bool_or(ff.identity IS NOT NULL), false) AS identities
         FROM as4tainr.file_fields ff
        WHERE ff.library = o.library AND ff.file = o.name) fx ON true
 WHERE o.objtype = '*FILE' AND o.attribute IN ('PF', 'LF', 'PF-SRC', 'PF-DTA', 'LF-DTA');
DROP FUNCTION IF EXISTS qsys2.sysfiles(varchar, varchar);
CREATE FUNCTION qsys2.sysfiles(table_schema varchar, table_name varchar)
RETURNS SETOF qsys2.sysfiles LANGUAGE sql STABLE AS $$
  SELECT * FROM qsys2.sysfiles s
   WHERE s.system_table_schema = upper(rtrim($1)) AND s.system_table_name = upper(rtrim($2))
$$;

DROP FUNCTION IF EXISTS qsys2.partition_statistics(varchar, varchar);
CREATE FUNCTION qsys2.partition_statistics(table_schema varchar, table_name varchar)
RETURNS TABLE (
  table_partition varchar(128),
  system_table_member char(10),
  number_rows bigint,
  number_row_pages bigint,
  number_pages bigint,
  overflow bigint,
  clustered char(1),
  active_blocks bigint,
  avgrowsize bigint,
  partition_type char(1),
  partition_number integer,
  number_distributed_partitions integer,
  number_deleted_rows bigint,
  data_size bigint,
  variable_length_size bigint,
  fixed_length_extents bigint,
  variable_length_extents bigint,
  column_stats_size bigint,
  maintained_temporary_index_size bigint,
  out_of_sync_data_space_entries bigint,
  variable_length_segments bigint,
  number_distinct_indexes smallint,
  open_operations bigint,
  close_operations bigint,
  insert_operations bigint,
  update_operations bigint,
  delete_operations bigint,
  logical_reads bigint,
  physical_reads bigint,
  sequential_reads bigint,
  random_reads bigint,
  blocked_insert_operations bigint,
  blocked_insert_rows bigint,
  clear_operations bigint,
  copy_operations bigint,
  reorganize_operations bigint,
  index_builds bigint,
  last_change_timestamp timestamp,
  last_save_timestamp timestamp,
  last_restore_timestamp timestamp,
  last_used_timestamp timestamp,
  days_used_count integer,
  last_reset_timestamp timestamp,
  file_cst_type char(11),
  next_identity_value decimal(31,0),
  low_inclusive char(1),
  low_value varchar(1024),
  high_inclusive char(1),
  high_value varchar(1024),
  number_partitioning_keys integer,
  partitioning_keys varchar(2880),
  keep_in_memory char(1),
  media_preference smallint,
  create_timestamp timestamp,
  last_source_update_timestamp timestamp,
  source_type varchar(10),
  volatile char(1),
  text varchar(50),
  partial_transaction char(1),
  apply_starting_receiver_library varchar(10),
  apply_starting_receiver varchar(10))
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$
  SELECT CAST(m.member AS varchar(128)),
         CAST(m.member AS char(10)),
         CAST(CASE WHEN m.source = '' THEN 0 ELSE length(m.source) - length(replace(m.source, chr(10), '')) + CASE WHEN right(m.source, 1) = chr(10) THEN 0 ELSE 1 END END AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS char(1)),
         CAST(NULL AS bigint),
         CAST(coalesce(sf.rcdlen, 92) AS bigint),
         CAST(' ' AS char(1)),
         CAST(0 AS integer),
         CAST(0 AS integer),
         CAST(0 AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS smallint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(m.changed AS timestamp),
         CAST(NULL AS timestamp),
         CAST(NULL AS timestamp),
         CAST(m.last_used AS timestamp),
         CAST(m.days_used AS integer),
         CAST(NULL AS timestamp),
         CAST(NULL AS char(11)),
         CAST(NULL AS decimal(31,0)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(1024)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(1024)),
         CAST(NULL AS integer),
         CAST(NULL AS varchar(2880)),
         CAST('0' AS char(1)),
         CAST(NULL AS smallint),
         CAST(m.added AS timestamp),
         CAST(m.changed AS timestamp),
         CAST(nullif(m.srctype, '') AS varchar(10)),
         CAST('0' AS char(1)),
         CAST(nullif(m.text, '') AS varchar(50)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(10)),
         CAST(NULL AS varchar(10))
    FROM as4tainr.source_members m
    LEFT JOIN as4tainr.source_files sf ON sf.library = m.library AND sf.name = m.file
   WHERE m.library = upper(rtrim($1)) AND m.file = upper(rtrim($2))
  UNION ALL
  SELECT CAST(o.name AS varchar(128)),
         CAST(o.name AS char(10)),
         CAST(0 AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS char(1)),
         CAST(NULL AS bigint),
         CAST(f.record_length AS bigint),
         CAST(' ' AS char(1)),
         CAST(0 AS integer),
         CAST(0 AS integer),
         CAST(0 AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS smallint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(NULL AS bigint),
         CAST(o.created AS timestamp),
         CAST(NULL AS timestamp),
         CAST(NULL AS timestamp),
         CAST(NULL AS timestamp),
         CAST(0 AS integer),
         CAST(NULL AS timestamp),
         CAST(NULL AS char(11)),
         CAST(NULL AS decimal(31,0)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(1024)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(1024)),
         CAST(NULL AS integer),
         CAST(NULL AS varchar(2880)),
         CAST('0' AS char(1)),
         CAST(NULL AS smallint),
         CAST(o.created AS timestamp),
         CAST(NULL AS timestamp),
         CAST(NULL AS varchar(10)),
         CAST('0' AS char(1)),
         CAST(nullif(o.text, '') AS varchar(50)),
         CAST(NULL AS char(1)),
         CAST(NULL AS varchar(10)),
         CAST(NULL AS varchar(10))
    FROM as4tainr.objects o
    JOIN qsys2.sysfiles f ON f.system_table_schema = o.library AND f.system_table_name = o.name
   WHERE o.objtype = '*FILE' AND f.file_type = 'DATA'
     AND o.library = upper(rtrim($1)) AND o.name = upper(rtrim($2))
$$;
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
         {VIEW_FUNCTIONS}
         {QCMDEXC_FUNCTION}
         {SPOOLED_FILE_INFO}
         {LIBRARY_INFO}
         {FILE_CATALOG}
         {PROGRAM_INFO}
         GRANT SELECT ON qsys2.sysfiles, qsys2.program_info TO {SQL_ROLE};
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
