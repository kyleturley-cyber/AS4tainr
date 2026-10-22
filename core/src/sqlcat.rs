//! More Db2 for i catalog views and IBM i services, made with the
//! services (`services::init`): QSYS2.SYSSEQUENCES, SYSCST, SYSKEYCST,
//! SYSCHKCST, SYSREFCST, SYSTRIGGERS (Db2 for i SQL Reference, catalog
//! views), and the IBM i services JOB_INFO, OUTPUT_QUEUE_ENTRIES_BASIC
//! (and OUTPUT_QUEUE_ENTRIES), OBJECT_PRIVILEGES (IBM i Services
//! reference). Columns are IBM's names, types and order; values
//! as4tainr does not keep are null or a fixed value (docs/sql.md).

/// The SQL that makes them.
pub fn sql() -> String {
    crate::sqljsonfn::SQL.to_string()
        + &format!(
            r#"
-- PostgreSQL's name for an IBM i name (lower case; a system column name
-- gets a "~" in front), as catalog::pg_name.
CREATE OR REPLACE FUNCTION as4tainr_svc.pg_name(n text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN lower(n) IN ('tableoid', 'xmin', 'cmin', 'xmax', 'cmax', 'ctid', 'oid')
              THEN '~' || lower(n) ELSE lower(n) END
$as4tainr$;

-- The columns of a constraint made by SQL, in key order: (position,
-- PostgreSQL column name).
CREATE OR REPLACE FUNCTION as4tainr_svc.cst_columns(lib text, f text, cname text, kind text,
    OUT pos integer, OUT col text) RETURNS SETOF record
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(k.n AS integer), a.attname::text
    FROM pg_index i
    JOIN pg_class ic ON ic.oid = i.indexrelid
    JOIN pg_namespace ns ON ns.oid = ic.relnamespace
    CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, n)
    JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
   WHERE kind IN ('PRIMARY KEY', 'UNIQUE') AND ns.nspname = lower(lib)
     AND ic.relname = lower(f || '_' || cname)
  UNION ALL
  SELECT CAST(k.n AS integer), a.attname::text
    FROM pg_constraint c
    JOIN pg_namespace ns ON ns.oid = c.connamespace
    CROSS JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum, n)
    JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum
   WHERE kind = 'FOREIGN KEY' AND ns.nspname = lower(lib) AND c.conname = lower(f || '_' || cname)
  ORDER BY 1
$as4tainr$;

-- Constraints made by SQL that are still there.
CREATE OR REPLACE VIEW as4tainr_svc.constraints AS
SELECT c.library, c.file, c.name, c.kind, c.created, coalesce(fl.sql_name, c.file) AS table_name
  FROM as4tainr.constraints c
  LEFT JOIN as4tainr.files fl ON fl.library = c.library AND fl.name = c.file
 WHERE (c.kind IN ('PRIMARY KEY', 'UNIQUE')
        AND to_regclass(format('%I.%I', lower(c.library), lower(c.file || '_' || c.name))) IS NOT NULL)
    OR (c.kind IN ('CHECK', 'FOREIGN KEY')
        AND EXISTS (SELECT 1 FROM pg_constraint pc JOIN pg_namespace ns ON ns.oid = pc.connamespace
                     WHERE ns.nspname = lower(c.library) AND pc.conname = lower(c.file || '_' || c.name)));

CREATE OR REPLACE VIEW qsys2.syscst AS
SELECT CAST(c.library AS varchar(128)) AS constraint_schema,
       CAST(c.name AS varchar(128)) AS constraint_name,
       CAST(c.kind AS varchar(11)) AS constraint_type,
       CAST(c.library AS varchar(128)) AS table_schema,
       CAST(c.table_name AS varchar(128)) AS table_name,
       CAST(c.library AS char(10)) AS system_constraint_schema,
       CAST(c.library AS char(10)) AS system_table_schema,
       CAST(c.file AS char(10)) AS system_table_name,
       CAST('NO' AS varchar(3)) AS is_deferrable,
       CAST('NO' AS varchar(3)) AS initially_deferred,
       CAST((SELECT count(*) FROM as4tainr_svc.cst_columns(c.library, c.file, c.name, c.kind)) AS integer)
         AS constraint_keys,
       CAST('ESTABLISHED' AS varchar(11)) AS constraint_state,
       CAST('YES' AS varchar(3)) AS enabled,
       CAST('NO' AS varchar(3)) AS check_pending,
       CAST(NULL AS varchar(50)) AS constraint_text,
       CAST(NULL AS varchar(2000)) AS long_comment,
       CAST(0 AS smallint) AS iasp_number
  FROM as4tainr_svc.constraints c;

CREATE OR REPLACE VIEW qsys2.syskeycst AS
SELECT CAST(c.library AS varchar(128)) AS constraint_schema,
       CAST(c.name AS varchar(128)) AS constraint_name,
       CAST(coalesce(ff.alias, ff.name, upper(k.col)) AS varchar(128)) AS column_name,
       k.pos AS ordinal_position,
       CAST(ff.seq + 1 AS integer) AS column_position,
       CAST(c.library AS varchar(128)) AS table_schema,
       CAST(c.table_name AS varchar(128)) AS table_name,
       CAST(coalesce(ff.name, upper(k.col)) AS char(10)) AS system_column_name,
       CAST(c.library AS char(10)) AS system_constraint_schema,
       CAST(c.library AS char(10)) AS system_table_schema,
       CAST(c.file AS char(10)) AS system_table_name,
       CAST(0 AS smallint) AS iasp_number
  FROM as4tainr_svc.constraints c
 CROSS JOIN LATERAL as4tainr_svc.cst_columns(c.library, c.file, c.name, c.kind) k
  LEFT JOIN as4tainr.file_fields ff ON ff.library = c.library AND ff.file = c.file
        AND as4tainr_svc.pg_name(coalesce(ff.alias, ff.name)) = k.col
 WHERE c.kind <> 'CHECK';

CREATE OR REPLACE VIEW qsys2.syschkcst AS
SELECT CAST(c.library AS varchar(128)) AS constraint_schema,
       CAST(c.name AS varchar(128)) AS constraint_name,
       CAST(CASE WHEN d.cond LIKE '%''%' THEN d.cond ELSE upper(d.cond) END AS varchar(2000)) AS check_clause,
       CAST(NULL AS char(1)) AS rounding_mode,
       CAST(c.library AS char(10)) AS system_constraint_schema,
       CAST(NULL AS varchar(500)) AS insert_action,
       CAST(NULL AS varchar(500)) AS update_action
  FROM as4tainr_svc.constraints c
  JOIN pg_namespace ns ON ns.nspname = lower(c.library)
  JOIN pg_constraint pc ON pc.connamespace = ns.oid AND pc.conname = lower(c.file || '_' || c.name)
 CROSS JOIN LATERAL (SELECT regexp_replace(pg_get_constraintdef(pc.oid), '^CHECK \(\((.*)\)\)$|^CHECK \((.*)\)$', '\1\2')
                        AS cond) d
 WHERE c.kind = 'CHECK';

-- A foreign key's parent key: the unique constraint (or keyed file) its
-- PostgreSQL index is.
CREATE OR REPLACE VIEW qsys2.sysrefcst AS
SELECT CAST(c.library AS varchar(128)) AS constraint_schema,
       CAST(c.name AS varchar(128)) AS constraint_name,
       CAST(upper(pns.nspname) AS varchar(128)) AS unique_constraint_schema,
       CAST(coalesce(pk.name, upper(ic.relname)) AS varchar(128)) AS unique_constraint_name,
       CAST(CASE pc.confmatchtype WHEN 'f' THEN 'FULL' WHEN 'p' THEN 'PARTIAL' ELSE 'NONE' END AS varchar(7)) AS match_option,
       CAST(CASE pc.confupdtype WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL'
                                WHEN 'd' THEN 'SET DEFAULT' ELSE 'NO ACTION' END AS varchar(11)) AS update_rule,
       CAST(CASE pc.confdeltype WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL'
                                WHEN 'd' THEN 'SET DEFAULT' ELSE 'NO ACTION' END AS varchar(11)) AS delete_rule,
       CAST(cardinality(pc.conkey) AS integer) AS column_count,
       CAST(c.library AS char(10)) AS system_constraint_schema,
       CAST(0 AS smallint) AS iasp_number
  FROM as4tainr_svc.constraints c
  JOIN pg_namespace ns ON ns.nspname = lower(c.library)
  JOIN pg_constraint pc ON pc.connamespace = ns.oid AND pc.conname = lower(c.file || '_' || c.name)
  JOIN pg_class pt ON pt.oid = pc.confrelid
  JOIN pg_namespace pns ON pns.oid = pt.relnamespace
  LEFT JOIN pg_class ic ON ic.oid = pc.conindid
  LEFT JOIN as4tainr.constraints pk ON lower(pk.library) = pns.nspname AND lower(pk.file) = pt.relname
        AND lower(pk.file || '_' || pk.name) = ic.relname
 WHERE c.kind = 'FOREIGN KEY';

-- SQL triggers (CREATE TRIGGER) and trigger programs (ADDPFTRG).
CREATE OR REPLACE VIEW qsys2.systriggers AS
SELECT CAST(t.library AS varchar(128)) AS trigger_schema,
       CAST(t.name AS varchar(128)) AS trigger_name,
       CAST(CASE WHEN t.events LIKE '% %' THEN 'MULTI' ELSE t.events END AS varchar(6)) AS event_manipulation,
       CAST(1 AS bigint) AS action_order,
       CAST(t.condition AS text) AS action_condition,
       CAST(t.body AS text) AS action_statement,
       CAST(CASE WHEN t.each_row THEN 'ROW' ELSE 'STATEMENT' END AS varchar(9)) AS action_orientation,
       CAST(t.timing AS varchar(10)) AS action_timing,
       CAST(t.file_lib AS varchar(128)) AS event_object_schema,
       CAST(coalesce(f.sql_name, t.file) AS varchar(128)) AS event_object_table,
       CAST(NULL AS varchar(128)) AS action_reference_old_table,
       CAST(NULL AS varchar(128)) AS action_reference_new_table,
       CAST(t.old_row AS varchar(128)) AS action_reference_old_row,
       CAST(t.new_row AS varchar(128)) AS action_reference_new_row,
       CAST(t.created AS timestamp) AS created,
       CAST(t.name AS varchar(10)) AS trigger_program_name,
       CAST(t.library AS varchar(10)) AS trigger_program_library,
       CAST('Y' AS char(1)) AS operative,
       CAST('DB2SQL' AS varchar(6)) AS trigger_mode,
       CAST(NULL AS varchar(50)) AS trigger_text,
       CAST(t.created AS timestamp) AS last_altered,
       CAST(t.definer AS varchar(128)) AS trigger_definer,
       CAST(t.library AS char(10)) AS system_trigger_schema,
       CAST(t.file_lib AS char(10)) AS system_event_object_schema,
       CAST(t.file AS char(10)) AS system_event_object_table
  FROM as4tainr.sql_triggers t
  LEFT JOIN as4tainr.files f ON f.library = t.file_lib AND f.name = t.file
 WHERE EXISTS (SELECT 1 FROM pg_trigger pt JOIN pg_class pc ON pc.oid = pt.tgrelid
                 JOIN pg_namespace ns ON ns.oid = pc.relnamespace
                WHERE ns.nspname = lower(t.file_lib) AND pt.tgname = lower(t.name))
UNION ALL
SELECT CAST(x.trg_lib AS varchar(128)), CAST(x.trg AS varchar(128)),
       CAST(ltrim(x.event, '*') AS varchar(6)), CAST(1 AS bigint), CAST(NULL AS text), CAST(NULL AS text),
       CAST('ROW' AS varchar(9)), CAST(ltrim(x.time, '*') AS varchar(10)),
       CAST(x.library AS varchar(128)), CAST(coalesce(f.sql_name, x.file) AS varchar(128)),
       CAST(NULL AS varchar(128)), CAST(NULL AS varchar(128)), CAST(NULL AS varchar(128)), CAST(NULL AS varchar(128)),
       CAST(x.added AS timestamp), CAST(x.pgm AS varchar(10)), CAST(x.pgm_lib AS varchar(10)),
       CAST('Y' AS char(1)), CAST('DB2ROW' AS varchar(6)), CAST(NULL AS varchar(50)),
       CAST(x.added AS timestamp), CAST(NULL AS varchar(128)),
       CAST(x.trg_lib AS char(10)), CAST(x.library AS char(10)), CAST(x.file AS char(10))
  FROM as4tainr.file_triggers x
  LEFT JOIN as4tainr.files f ON f.library = x.library AND f.name = x.file;

CREATE OR REPLACE VIEW qsys2.syssequences AS
SELECT CAST(s.library AS varchar(128)) AS sequence_schema,
       CAST(s.name AS varchar(128)) AS sequence_name,
       CAST(ps.max_value AS decimal(63, 0)) AS maximum_value,
       CAST(ps.min_value AS decimal(63, 0)) AS minimum_value,
       CAST(ps.increment_by AS integer) AS increment,
       CAST(CASE WHEN ps.cycle THEN 'YES' ELSE 'NO' END AS varchar(3)) AS cycle_option,
       CAST(coalesce(s.cache, 0) AS integer) AS cache,
       CAST(CASE WHEN s.ordered THEN 'YES' ELSE 'NO' END AS varchar(3)) AS "order",
       CAST(s.data_type AS varchar(128)) AS data_type,
       CAST(CASE s.data_type WHEN 'SMALLINT' THEN 5 WHEN 'INTEGER' THEN 10 WHEN 'BIGINT' THEN 19
                             ELSE s.precision END AS integer) AS numeric_precision,
       CAST(NULL AS varchar(128)) AS user_defined_type_schema,
       CAST(NULL AS varchar(128)) AS user_defined_type_name,
       CAST(ps.start_value AS decimal(63, 0)) AS start,
       CAST(ps.last_value AS decimal(63, 0)) AS maxassignedval,
       CAST(o.owner AS varchar(128)) AS sequence_definer,
       CAST(o.created AS timestamp) AS sequence_created,
       CAST(o.created AS timestamp) AS last_altered_timestamp,
       CAST(nullif(o.text, '') AS varchar(50)) AS sequence_text,
       CAST(NULL AS varchar(2000)) AS long_comment,
       CAST(s.library AS char(10)) AS system_seq_schema,
       CAST(s.name AS char(10)) AS system_seq_name,
       CAST(0 AS smallint) AS iasp_number
  FROM as4tainr.sequences s
  JOIN as4tainr.objects o ON o.library = s.library AND o.name = s.name AND o.objtype = '*DTAARA'
  JOIN pg_sequences ps ON ps.schemaname = lower(s.library) AND ps.sequencename = lower(s.name);

-- OUTPUT_QUEUE_ENTRIES_BASIC (IBM i Services): the spooled files. Sizes
-- are the lines' bytes in kilobytes; the output queue's library is QGPL.
CREATE OR REPLACE VIEW qsys2.output_queue_entries_basic AS
SELECT CAST(s.outq AS varchar(10)) AS output_queue_name,
       CAST('QGPL' AS varchar(10)) AS output_queue_library_name,
       CAST(s.created AS timestamp) AS create_timestamp,
       CAST(s.file AS varchar(10)) AS spooled_file_name,
       CAST(s.user_name AS varchar(10)) AS user_name,
       CAST(nullif(s.user_data, '') AS varchar(10)) AS user_data,
       CAST(CASE s.status WHEN 'OPN' THEN 'OPEN' WHEN 'HLD' THEN 'HELD' WHEN 'SAV' THEN 'SAVED'
                          WHEN 'CLO' THEN 'CLOSED' ELSE 'READY' END AS varchar(15)) AS status,
       CAST(ceil(coalesce((SELECT sum(length(l.text) + 1) FROM as4tainr.spool_lines l WHERE l.spl = s.id), 0)
                 / 1024.0) AS integer) AS size,
       CAST(s.pages AS integer) AS total_pages,
       CAST(1 AS smallint) AS copies,
       CAST('*STD' AS varchar(10)) AS form_type,
       CAST(s.job_name AS varchar(28)) AS job_name,
       CAST('*SCS' AS varchar(10)) AS device_type,
       CAST(5 AS smallint) AS output_priority,
       CAST(s.number AS integer) AS file_number,
       CAST('AS4TAINR' AS varchar(8)) AS system
  FROM as4tainr.spooled_files s;
CREATE OR REPLACE VIEW qsys2.output_queue_entries AS
SELECT * FROM qsys2.output_queue_entries_basic;

-- OBJECT_PRIVILEGES (IBM i Services): the owner (*ALL), each private
-- authority, and *PUBLIC, for each object; the specific authorities of
-- *ALL, *CHANGE, *USE and *EXCLUDE (Security reference, "Object
-- authority").
CREATE OR REPLACE VIEW qsys2.object_privileges AS
WITH a AS (
  SELECT o.library, o.name, o.objtype, o.owner, o.text, o.owner AS who, '*ALL' AS aut FROM as4tainr.objects o
  UNION ALL
  SELECT o.library, o.name, o.objtype, o.owner, o.text, x.user_name, x.aut
    FROM as4tainr.object_auths x
    JOIN as4tainr.objects o ON o.library = x.library AND o.name = x.name AND o.objtype = x.objtype
   WHERE x.user_name <> o.owner
  UNION ALL
  SELECT o.library, o.name, o.objtype, o.owner, o.text, '*PUBLIC', o.public_aut FROM as4tainr.objects o
)
SELECT CAST(a.library AS varchar(128)) AS object_schema,
       CAST(coalesce(f.sql_name, a.name) AS varchar(128)) AS object_name,
       CAST(a.library AS varchar(10)) AS system_object_schema,
       CAST(a.name AS varchar(10)) AS system_object_name,
       CAST(a.objtype AS varchar(8)) AS object_type,
       CAST(CASE WHEN a.objtype = '*FILE' THEN
              CASE f.sql_type WHEN 'TABLE' THEN 'TABLE' WHEN 'VIEW' THEN 'VIEW' WHEN 'INDEX' THEN 'INDEX' END
            WHEN a.objtype = '*DTAARA' AND EXISTS (SELECT 1 FROM as4tainr.sequences s
                 WHERE s.library = a.library AND s.name = a.name) THEN 'SEQUENCE' END AS varchar(9)) AS sql_object_type,
       CAST(a.who AS varchar(10)) AS authorization_name,
       CAST(a.aut AS varchar(12)) AS object_authority,
       CAST(a.owner AS varchar(10)) AS owner,
       CAST(NULL AS varchar(10)) AS authorization_list,
       CAST(NULL AS varchar(10)) AS primary_group,
       CAST('NO' AS varchar(3)) AS authorization_list_management,
       CAST(CASE WHEN a.who = a.owner THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_owner,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE', '*USE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_operational,
       CAST(CASE WHEN a.aut = '*ALL' THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_management,
       CAST(CASE WHEN a.aut = '*ALL' THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_existence,
       CAST(CASE WHEN a.aut = '*ALL' THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_alter,
       CAST(CASE WHEN a.aut = '*ALL' THEN 'YES' ELSE 'NO' END AS varchar(3)) AS object_reference,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE', '*USE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS data_read,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS data_add,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS data_update,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS data_delete,
       CAST(CASE WHEN a.aut IN ('*ALL', '*CHANGE', '*USE') THEN 'YES' ELSE 'NO' END AS varchar(3)) AS data_execute,
       CAST(nullif(a.text, '') AS varchar(50)) AS text_description
  FROM a
  LEFT JOIN as4tainr.files f ON a.objtype = '*FILE' AND f.library = a.library AND f.name = a.name;

-- JOB_INFO (IBM i Services): the jobs, active, on a job queue or ended
-- with output (the first 30 of IBM's columns). Without JOB_USER_FILTER,
-- the jobs of the statement's user.
DROP FUNCTION IF EXISTS qsys2.job_info(varchar, varchar, varchar, varchar, varchar);
CREATE FUNCTION qsys2.job_info(job_status_filter varchar DEFAULT '*ALL', job_type_filter varchar DEFAULT '*ALL',
    job_subsystem_filter varchar DEFAULT '*ALL', job_user_filter varchar DEFAULT NULL,
    job_submitter_filter varchar DEFAULT '*ALL')
RETURNS TABLE (job_name varchar(28), job_information varchar(12), job_status varchar(6), job_type varchar(3),
    job_type_enhanced varchar(28), job_subsystem varchar(10), job_date varchar(10),
    job_description_library varchar(10), job_description varchar(10), job_accounting_code varchar(15),
    submitter_job_name varchar(28), submitter_message_queue_library varchar(10),
    submitter_message_queue varchar(10), server_type varchar(30), job_entered_system_time timestamp(0),
    job_scheduled_time timestamp(0), job_active_time timestamp(0), job_end_time timestamp(0),
    job_end_severity smallint, completion_status varchar(8), job_end_reason varchar(60),
    job_queue_library varchar(10), job_queue_name varchar(10), job_queue_status varchar(9),
    job_queue_priority smallint, job_queue_time timestamp(0), job_message_queue_maximum_size smallint,
    job_message_queue_full_action varchar(8), allow_multiple_threads varchar(3),
    peak_temporary_storage integer)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
SELECT CAST(lpad(j.number::text, 6, '0') || '/' || j.user_name || '/' || j.name AS varchar(28)),
       CAST('YES' AS varchar(12)), CAST(j.status AS varchar(6)), CAST(j.type AS varchar(3)),
       CAST(CASE j.type WHEN 'INT' THEN 'INTERACTIVE' ELSE 'BATCH' END AS varchar(28)),
       CAST(nullif(j.subsystem, '') AS varchar(10)),
       CAST(CASE WHEN j.status <> 'OUTQ' THEN 'SYSVAL' END AS varchar(10)),
       CAST(NULL AS varchar(10)), CAST(NULL AS varchar(10)), CAST(NULL AS varchar(15)),
       CAST(sj.job_name AS varchar(28)),
       CAST(CASE WHEN sj.job_name IS NOT NULL THEN 'QUSRSYS' END AS varchar(10)),
       CAST(CASE WHEN sj.job_name IS NOT NULL THEN sj.user_name END AS varchar(10)),
       CAST(NULL AS varchar(30)),
       CAST(j.entered AS timestamp(0)), CAST(NULL AS timestamp(0)), CAST(j.started AS timestamp(0)),
       CAST(j.ended AS timestamp(0)), CAST(30 AS smallint),
       CAST(nullif(j.end_code, '') AS varchar(8)), CAST(NULL AS varchar(60)),
       CAST(CASE WHEN j.status = 'JOBQ' THEN 'QGPL' END AS varchar(10)),
       CAST(nullif(j.jobq, '') AS varchar(10)),
       CAST(CASE WHEN j.status = 'JOBQ' THEN 'RELEASED' END AS varchar(9)),
       CAST(CASE WHEN j.status = 'JOBQ' THEN 5 END AS smallint),
       CAST(CASE WHEN j.status = 'JOBQ' THEN j.entered END AS timestamp(0)),
       CAST(NULL AS smallint), CAST(NULL AS varchar(8)), CAST('NO' AS varchar(3)), CAST(NULL AS integer)
  FROM as4tainr.jobs j
  LEFT JOIN LATERAL (SELECT lpad(s.number::text, 6, '0') || '/' || s.user_name || '/' || s.name AS job_name,
                            s.user_name
                       FROM as4tainr.jobs s WHERE s.number = j.submitter) sj ON true
 WHERE (upper(job_status_filter) = '*ALL' OR j.status = ltrim(upper(job_status_filter), '*'))
   AND (upper(job_type_filter) = '*ALL'
        OR (upper(job_type_filter) = '*BATCH' AND j.type = 'BCH')
        OR (upper(job_type_filter) = '*INTERACT' AND j.type = 'INT'))
   AND (upper(job_subsystem_filter) = '*ALL' OR j.subsystem = upper(job_subsystem_filter))
   AND (upper(coalesce(job_user_filter, '')) = '*ALL'
        OR j.user_name = CASE WHEN job_user_filter IS NULL OR upper(job_user_filter) = '*USER'
            THEN coalesce(nullif(current_setting('as4tainr.user', true), ''),
                          nullif(split_part(current_setting('application_name'), '/', 2), ''), upper(session_user))
            ELSE upper(job_user_filter) END)
   AND (upper(job_submitter_filter) = '*ALL'
        OR (upper(job_submitter_filter) = '*JOB' AND sj.job_name IS NOT NULL
            AND sj.job_name = upper(current_setting('as4tainr.job', true)))
        OR (upper(job_submitter_filter) = '*USER' AND sj.user_name =
            coalesce(nullif(current_setting('as4tainr.user', true), ''), upper(session_user))))
 ORDER BY j.number
$as4tainr$;

GRANT SELECT ON qsys2.syscst, qsys2.syskeycst, qsys2.syschkcst, qsys2.sysrefcst, qsys2.systriggers,
    qsys2.syssequences, qsys2.output_queue_entries_basic, qsys2.output_queue_entries,
    qsys2.object_privileges TO {role};
GRANT EXECUTE ON FUNCTION qsys2.job_info(varchar, varchar, varchar, varchar, varchar) TO {role};
"#,
            role = crate::sql::SQL_ROLE
        )
}
