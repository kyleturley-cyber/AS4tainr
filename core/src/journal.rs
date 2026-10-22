//! Journals: journal receivers (*JRNRCV), journals (*JRN), physical files
//! journaled to them, and the entries.
//!
//! Written from IBM's CL command reference (CRTJRNRCV, CRTJRN, CHGJRN,
//! DLTJRN, DLTJRNRCV, STRJRNPF, ENDJRNPF, DSPJRN) and the Db2 for i
//! services reference (QSYS2.DISPLAY_JOURNAL). A journaled file has a
//! PostgreSQL trigger that writes an entry for every record added (R PT),
//! changed (R UB with IMAGES(*BOTH), R UP) or deleted (R DL), whichever
//! interface changed it: record-level access, SQL or CL. An entry's data
//! is the record image in the file's record format (EBCDIC character
//! fields, packed and zoned decimals, binary). Changes made under
//! commitment control belong to a commit cycle: its start (C SC) is
//! written with the first change, its end with COMMIT (C CM) or ROLLBACK
//! (C RB). The entries live in `as4tainr.journal_entries`, numbered by a
//! sequence per journal.

use crate::catalog::{self, ident};
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use anyhow::{bail, Result};
use tokio_postgres::Client;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.journals (
    library      text NOT NULL,
    name         text NOT NULL,
    jid          serial UNIQUE,
    receiver_lib text NOT NULL,
    receiver     text NOT NULL,
    images       text NOT NULL DEFAULT '*AFTER',
    text         text NOT NULL DEFAULT '',
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.journal_receivers (
    library     text NOT NULL,
    name        text NOT NULL,
    journal_lib text,
    journal     text,
    attached    timestamptz,
    detached    timestamptz,
    text        text NOT NULL DEFAULT '',
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.journaled_files (
    library     text NOT NULL,
    file        text NOT NULL,
    journal_lib text NOT NULL,
    journal     text NOT NULL,
    images      text NOT NULL,
    omtjrne     text NOT NULL,
    started     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (library, file)
);
CREATE TABLE IF NOT EXISTS as4tainr.journal_entries (
    journal_lib  text NOT NULL,
    journal      text NOT NULL,
    seq          bigint NOT NULL,
    ts           timestamptz NOT NULL DEFAULT clock_timestamp(),
    code         char(1) NOT NULL,
    type         char(2) NOT NULL,
    count_rrn    bigint,
    entry_data   bytea,
    null_ind     text,
    object       text,
    object_type  text,
    user_name    text,
    job_name     text,
    job_user     text,
    job_number   text,
    program_name text,
    program_lib  text,
    commit_cycle numeric NOT NULL DEFAULT 0,
    receiver_lib text,
    receiver     text,
    PRIMARY KEY (journal_lib, journal, seq)
);
"#;

/// The name of a journal's sequence (its entries' numbers).
fn seq_name(jid: i32) -> String {
    format!("as4tainr.jrnseq_{jid}")
}

/// PostgreSQL functions the journals use, made with the IBM i services
/// (`services::init`): EBCDIC and record images, writing an entry, the
/// trigger on journaled files, and the ends of commit cycles.
pub fn sql_functions() -> String {
    let table: String = crate::tn5250::ebcdic::FROM_UNICODE
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        r#"
CREATE OR REPLACE FUNCTION as4tainr_svc.e37(s text) RETURNS bytea
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  tbl bytea := '\x{table}'::bytea;
  out bytea := '\x'::bytea;
  c int;
BEGIN
  IF s IS NULL THEN RETURN NULL; END IF;
  FOR i IN 1..length(s) LOOP
    c := ascii(substr(s, i, 1));
    out := out || set_byte('\x00'::bytea, 0, CASE WHEN c < 256 THEN get_byte(tbl, c) ELSE 111 END);
  END LOOP;
  RETURN out;
END $as4tainr$;

-- A field's bytes in a record image (DDS data types; ILE RPG and DDS
-- references for the storage formats): v is the value as PostgreSQL text,
-- NULL for the null value (stored as the type's default).
CREATE OR REPLACE FUNCTION as4tainr_svc.field_image(v text, t text, len int, decs int, varlen boolean, dbl boolean)
RETURNS bytea LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  n numeric;
  d text;
  h text;
  neg boolean;
  s text;
BEGIN
  IF t IN ('A') THEN
    s := coalesce(v, '');
    IF varlen THEN
      s := substr(s, 1, len);
      RETURN int2send(length(s)::int2) || as4tainr_svc.e37(rpad(s, len));
    END IF;
    RETURN as4tainr_svc.e37(rpad(s, len));
  ELSIF t IN ('P', 'S', 'B') THEN
    n := coalesce(v::numeric, 0);
    neg := n < 0;
    n := trunc(abs(n) * power(10::numeric, decs));
    IF t = 'B' THEN
      IF neg THEN n := -n; END IF;
      IF len <= 4 THEN RETURN int2send(n::int2);
      ELSIF len <= 9 THEN RETURN int4send(n::int4);
      ELSE RETURN int8send(n::int8); END IF;
    END IF;
    d := lpad(n::text, len, '0');
    d := substr(d, length(d) - len + 1);
    IF t = 'P' THEN
      h := d || CASE WHEN neg THEN 'd' ELSE 'f' END;
      IF length(h) % 2 = 1 THEN h := '0' || h; END IF;
      RETURN decode(h, 'hex');
    END IF;
    h := '';
    FOR i IN 1..len LOOP
      h := h || CASE WHEN i = len AND neg THEN 'd' ELSE 'f' END || substr(d, i, 1);
    END LOOP;
    RETURN decode(h, 'hex');
  ELSIF t = 'F' THEN
    IF dbl OR len > 9 THEN RETURN float8send(coalesce(v::float8, 0)); END IF;
    RETURN float4send(coalesce(v::float4, 0));
  ELSIF t = 'L' THEN
    RETURN as4tainr_svc.e37(coalesce(v, '0001-01-01'));
  ELSIF t = 'T' THEN
    RETURN as4tainr_svc.e37(replace(coalesce(v, '00:00:00'), ':', '.'));
  ELSIF t = 'Z' THEN
    RETURN as4tainr_svc.e37(to_char(coalesce(v::timestamp, '0001-01-01'::timestamp), 'YYYY-MM-DD-HH24.MI.SS.US'));
  ELSIF t = 'H' THEN
    h := coalesce(substr(v, 3), '');
    IF varlen THEN RETURN int2send((length(h) / 2)::int2) || decode(rpad(h, len * 2, '0'), 'hex'); END IF;
    RETURN decode(rpad(h, len * 2, '0'), 'hex');
  ELSIF t = 'G' THEN
    s := coalesce(v, '');
    h := '';
    FOR i IN 1..len LOOP
      h := h || lpad(to_hex(CASE WHEN i <= length(s) THEN least(ascii(substr(s, i, 1)), 65533) ELSE 32 END), 4, '0');
    END LOOP;
    IF varlen THEN RETURN int2send(least(length(s), len)::int2) || decode(h, 'hex'); END IF;
    RETURN decode(h, 'hex');
  END IF;
  RETURN '\x'::bytea;
END $as4tainr$;

-- A row (as JSON) as the file's record image and null byte map.
CREATE OR REPLACE FUNCTION as4tainr_svc.record_image(lib text, f text, r jsonb, OUT image bytea, OUT nulls text)
LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  fd record;
  k text;
  v text;
BEGIN
  image := '\x'::bytea;
  nulls := '';
  IF r IS NULL THEN image := NULL; nulls := NULL; RETURN; END IF;
  FOR fd IN SELECT name, alias, data_type, length, decimals, varlen, flt_double
              FROM as4tainr.file_fields
             WHERE library = lib AND file = f
               AND data_type NOT IN ('CLOB', 'BLOB', 'DBCLOB', 'DECFLOAT', 'BOOLEAN', 'XML')
             ORDER BY seq LOOP
    k := lower(coalesce(fd.alias, fd.name));
    IF k IN ('tableoid', 'xmin', 'cmin', 'xmax', 'cmax', 'ctid', 'oid') THEN k := '~' || k; END IF;
    v := r ->> k;
    nulls := nulls || CASE WHEN v IS NULL THEN '1' ELSE '0' END;
    image := image || as4tainr_svc.field_image(v, fd.data_type, fd.length, fd.decimals, fd.varlen, fd.flt_double);
  END LOOP;
END $as4tainr$;

-- Write one journal entry; its sequence number. Under commitment control
-- (as4tainr.cmt), a change opens the commit cycle of its journal (C SC)
-- the first time.
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_entry(jlib text, jname text, jcode text, jtype text,
    rrn bigint, data bytea, nulls text, obj text, objtype text)
RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  j record;
  s bigint;
  cc numeric := 0;
  job text := coalesce(nullif(current_setting('as4tainr.job', true), ''), current_setting('application_name'));
  usr text;
BEGIN
  SELECT jid, receiver_lib, receiver INTO j FROM as4tainr.journals WHERE library = jlib AND name = jname;
  IF NOT FOUND THEN RETURN NULL; END IF;
  usr := coalesce(nullif(current_setting('as4tainr.user', true), ''), nullif(split_part(job, '/', 2), ''), upper(session_user));
  IF jcode NOT IN ('C', 'F', 'J') AND current_setting('as4tainr.cmt', true) = '1' THEN
    cc := nullif(current_setting('as4tainr.cc_' || j.jid, true), '')::numeric;
    IF cc IS NULL THEN
      cc := nextval('as4tainr.jrnseq_' || j.jid);
      INSERT INTO as4tainr.journal_entries (journal_lib, journal, seq, code, type, count_rrn,
          user_name, job_name, job_user, job_number, program_name, program_lib, commit_cycle, receiver_lib, receiver)
      VALUES (jlib, jname, cc, 'C', 'SC', 0, usr, split_part(job, '/', 3), split_part(job, '/', 2), split_part(job, '/', 1),
          nullif(current_setting('as4tainr.program', true), ''), nullif(current_setting('as4tainr.program_lib', true), ''),
          cc, j.receiver_lib, j.receiver);
      PERFORM set_config('as4tainr.cc_' || j.jid, cc::text, true);
      PERFORM set_config('as4tainr.cycles',
          coalesce(current_setting('as4tainr.cycles', true), '') || j.jid || ':' || cc || ',', true);
    END IF;
  END IF;
  s := nextval('as4tainr.jrnseq_' || j.jid);
  INSERT INTO as4tainr.journal_entries (journal_lib, journal, seq, code, type, count_rrn, entry_data, null_ind,
      object, object_type, user_name, job_name, job_user, job_number, program_name, program_lib, commit_cycle,
      receiver_lib, receiver)
  VALUES (jlib, jname, s, jcode, jtype, rrn, data, nulls, obj, objtype, usr,
      split_part(job, '/', 3), split_part(job, '/', 2), split_part(job, '/', 1),
      nullif(current_setting('as4tainr.program', true), ''), nullif(current_setting('as4tainr.program_lib', true), ''),
      cc, j.receiver_lib, j.receiver);
  RETURN s;
END $as4tainr$;

-- The trigger on a journaled file: TG_ARGV = journal library and name,
-- IMAGES, file library and name.
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_trigger() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  obj text := rpad(TG_ARGV[4], 10) || rpad(TG_ARGV[3], 10) || rpad(TG_ARGV[4], 10);
  img record;
BEGIN
  -- A change made on trial, for *BEFORE trigger programs, is not one;
  -- changes APYJRNCHG and RMVJRNCHG make are journaled as F AY and RC.
  IF current_setting('as4tainr.pftrg_trial', true) = '1'
     OR current_setting('as4tainr.apply', true) = '1' THEN RETURN NULL; END IF;
  IF TG_OP = 'INSERT' THEN
    SELECT * INTO img FROM as4tainr_svc.record_image(TG_ARGV[3], TG_ARGV[4], to_jsonb(NEW));
    PERFORM as4tainr_svc.journal_entry(TG_ARGV[0], TG_ARGV[1], 'R', 'PT', (to_jsonb(NEW) ->> 'rrn')::bigint,
        img.image, img.nulls, obj, '*QDDS');
  ELSIF TG_OP = 'UPDATE' THEN
    IF TG_ARGV[2] = '*BOTH' THEN
      SELECT * INTO img FROM as4tainr_svc.record_image(TG_ARGV[3], TG_ARGV[4], to_jsonb(OLD));
      PERFORM as4tainr_svc.journal_entry(TG_ARGV[0], TG_ARGV[1], 'R', 'UB', (to_jsonb(OLD) ->> 'rrn')::bigint,
          img.image, img.nulls, obj, '*QDDS');
    END IF;
    SELECT * INTO img FROM as4tainr_svc.record_image(TG_ARGV[3], TG_ARGV[4], to_jsonb(NEW));
    PERFORM as4tainr_svc.journal_entry(TG_ARGV[0], TG_ARGV[1], 'R', 'UP', (to_jsonb(NEW) ->> 'rrn')::bigint,
        img.image, img.nulls, obj, '*QDDS');
  ELSE
    SELECT * INTO img FROM as4tainr_svc.record_image(TG_ARGV[3], TG_ARGV[4], to_jsonb(OLD));
    PERFORM as4tainr_svc.journal_entry(TG_ARGV[0], TG_ARGV[1], 'R', 'DL', (to_jsonb(OLD) ->> 'rrn')::bigint,
        img.image, img.nulls, obj, '*QDDS');
  END IF;
  RETURN NULL;
END $as4tainr$;

-- The commit cycles open in this transaction: (jid, cycle).
CREATE OR REPLACE FUNCTION as4tainr_svc.open_cycles(OUT jid int, OUT cycle numeric) RETURNS SETOF record
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT split_part(x, ':', 1)::int, split_part(x, ':', 2)::numeric
    FROM unnest(string_to_array(coalesce(current_setting('as4tainr.cycles', true), ''), ',')) AS x
   WHERE x <> ''
$as4tainr$;

-- COMMIT: a C CM entry for each commit cycle the transaction opened.
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_commit() RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  c record;
  j record;
BEGIN
  FOR c IN SELECT * FROM as4tainr_svc.open_cycles() LOOP
    SELECT library, name INTO j FROM as4tainr.journals WHERE jid = c.jid;
    IF FOUND THEN
      PERFORM set_config('as4tainr.cmt', '0', true);
      PERFORM as4tainr_svc.cycle_end(j.library, j.name, 'CM', c.cycle);
    END IF;
  END LOOP;
END $as4tainr$;

-- The end of a commit cycle (CM, or RB written after the ROLLBACK, when
-- the cycle's start was rolled back too: it is written again).
CREATE OR REPLACE FUNCTION as4tainr_svc.cycle_end(jlib text, jname text, jtype text, cycle numeric) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  s bigint;
BEGIN
  IF jtype = 'RB' AND NOT EXISTS (SELECT 1 FROM as4tainr.journal_entries
      WHERE journal_lib = jlib AND journal = jname AND seq = cycle) THEN
    s := as4tainr_svc.journal_entry(jlib, jname, 'C', 'SC', 0, NULL, NULL, NULL, NULL);
    UPDATE as4tainr.journal_entries SET seq = cycle, commit_cycle = cycle
     WHERE journal_lib = jlib AND journal = jname AND seq = s;
  END IF;
  s := as4tainr_svc.journal_entry(jlib, jname, 'C', jtype, 0, NULL, NULL, NULL, NULL);
  UPDATE as4tainr.journal_entries SET commit_cycle = cycle
   WHERE journal_lib = jlib AND journal = jname AND seq = s;
END $as4tainr$;

-- ROLLBACK: C RB entries, by journal id, for the cycles open before it.
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_rolled_back(ids int[], cycles numeric[]) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  j record;
BEGIN
  FOR i IN 1..coalesce(array_length(ids, 1), 0) LOOP
    SELECT library, name INTO j FROM as4tainr.journals WHERE jid = ids[i];
    IF FOUND THEN
      PERFORM as4tainr_svc.cycle_end(j.library, j.name, 'RB', cycles[i]);
    END IF;
  END LOOP;
END $as4tainr$;

-- The entries of the commit cycles open in this transaction (before a
-- rollback): JSON for journal_restore.
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_hold() RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  res jsonb := '[]'::jsonb;
  c record;
  j record;
BEGIN
  FOR c IN SELECT * FROM as4tainr_svc.open_cycles() LOOP
    SELECT library, name INTO j FROM as4tainr.journals WHERE jid = c.jid;
    IF FOUND THEN
      res := res || coalesce((SELECT jsonb_agg(to_jsonb(e) ORDER BY e.seq) FROM as4tainr.journal_entries e
          WHERE e.journal_lib = j.library AND e.journal = j.name AND e.commit_cycle = c.cycle), '[]'::jsonb);
    END IF;
  END LOOP;
  IF res = '[]'::jsonb THEN RETURN NULL; END IF;
  RETURN jsonb_build_object('cycles', current_setting('as4tainr.cycles', true), 'entries', res);
END $as4tainr$;

-- After a rollback (whole: the transaction; otherwise to a savepoint):
-- the entries it took back are in the journal again, as on IBM i (an
-- entry is written when the change is made), and each record change it
-- undid is journaled, newest first (Journal code R: DR, record deleted
-- for rollback, for an added record; BR and UR, before- and after-image
-- of a record updated for rollback, for an updated record (BR when the
-- journal has before-images); UR for a deleted record put back).
CREATE OR REPLACE FUNCTION as4tainr_svc.journal_restore(held jsonb, whole boolean) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  e as4tainr.journal_entries;
  prev as4tainr.journal_entries;
  lost as4tainr.journal_entries[] := '{{}}';
  all_e as4tainr.journal_entries[];
  img record;
  id int;
  r jsonb;
  l text;
  f text;
  d bytea;
  nl text;
BEGIN
  IF held IS NULL THEN RETURN; END IF;
  SELECT array_agg(x ORDER BY x.seq) INTO all_e
    FROM jsonb_populate_recordset(NULL::as4tainr.journal_entries, held -> 'entries') x;
  FOREACH e IN ARRAY coalesce(all_e, '{{}}') LOOP
    IF NOT EXISTS (SELECT 1 FROM as4tainr.journal_entries
                    WHERE journal_lib = e.journal_lib AND journal = e.journal AND seq = e.seq) THEN
      INSERT INTO as4tainr.journal_entries VALUES (e.*);
      lost := lost || e;
    END IF;
  END LOOP;
  IF cardinality(lost) = 0 THEN RETURN; END IF;
  IF NOT whole THEN
    PERFORM set_config('as4tainr.cycles', coalesce(held ->> 'cycles', ''), true);
  END IF;
  PERFORM set_config('as4tainr.cmt', '1', true);
  FOR i IN REVERSE cardinality(lost)..1 LOOP
    e := lost[i];
    SELECT jid INTO id FROM as4tainr.journals WHERE library = e.journal_lib AND name = e.journal;
    PERFORM set_config('as4tainr.cc_' || id, e.commit_cycle::text, true);
    CONTINUE WHEN e.code <> 'R' OR e.type NOT IN ('PT', 'PX', 'UP', 'DL');
    IF e.type IN ('PT', 'PX') THEN
      PERFORM as4tainr_svc.journal_entry(e.journal_lib, e.journal, 'R', 'DR', e.count_rrn,
          e.entry_data, e.null_ind, e.object, e.object_type);
    ELSIF e.type = 'DL' THEN
      PERFORM as4tainr_svc.journal_entry(e.journal_lib, e.journal, 'R', 'UR', e.count_rrn,
          e.entry_data, e.null_ind, e.object, e.object_type);
    ELSE
      -- The record as it was before the update: its before-image, the
      -- entry before for it, or (no such entry) the record now.
      SELECT * INTO prev FROM unnest(all_e) x
       WHERE x.object = e.object AND x.count_rrn = e.count_rrn AND x.seq < e.seq AND x.code = 'R'
         AND x.type IN ('UB', 'PT', 'PX', 'UP')
       ORDER BY x.seq DESC LIMIT 1;
      IF FOUND THEN
        d := prev.entry_data;
        nl := prev.null_ind;
        IF prev.type = 'UB' THEN
          PERFORM as4tainr_svc.journal_entry(e.journal_lib, e.journal, 'R', 'BR', e.count_rrn,
              e.entry_data, e.null_ind, e.object, e.object_type);
        END IF;
      ELSE
        f := rtrim(substr(e.object, 1, 10));
        l := rtrim(substr(e.object, 11, 10));
        EXECUTE format('SELECT to_jsonb(t) FROM %I.%I t WHERE rrn = $1', lower(l), lower(f)) INTO r USING e.count_rrn;
        SELECT * INTO img FROM as4tainr_svc.record_image(l, f, r);
        d := img.image;
        nl := img.nulls;
      END IF;
      PERFORM as4tainr_svc.journal_entry(e.journal_lib, e.journal, 'R', 'UR', e.count_rrn,
          d, nl, e.object, e.object_type);
    END IF;
  END LOOP;
END $as4tainr$;

{display_journal}

{interpret}

GRANT EXECUTE ON FUNCTION as4tainr_svc.journal_commit(), as4tainr_svc.open_cycles(),
    as4tainr_svc.journal_rolled_back(int[], numeric[]), as4tainr_svc.e37(text),
    as4tainr_svc.journal_hold(), as4tainr_svc.journal_restore(jsonb, boolean),
    as4tainr_svc.interpret(bytea, text, int, int) TO {role};
GRANT EXECUTE ON FUNCTION qsys2.display_journal(varchar, varchar, varchar, varchar, timestamp, numeric,
    varchar, varchar, varchar, varchar, varchar, varchar, varchar, varchar, varchar, varchar, varchar,
    timestamp, numeric, varchar, integer, varchar, varchar) TO {role};
"#,
        display_journal = DISPLAY_JOURNAL,
        interpret = INTERPRET,
        role = crate::sql::SQL_ROLE,
    )
}

/// INTERPRET (Db2 for i SQL Reference): bytes read as a data type's
/// internal form, as text for the CAST the translation adds: 'P' packed,
/// 'S' zoned, 'B' big-endian binary integer, 'A' EBCDIC (CCSID 37)
/// characters, 'V' a 2-byte length and characters.
const INTERPRET: &str = r#"
-- SUBSTR (Db2 for i SQL Reference): of a fixed-length character string,
-- a fixed-length string (so it compares with blank padding, as Db2
-- compares strings); of other strings and binary strings, as PostgreSQL.
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s bpchar, b bigint, n bigint) RETURNS bpchar
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(substr(rpad(CAST(s AS text), (b + n - 1)::int), b::int, n::int) AS bpchar) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s bpchar, b bigint) RETURNS bpchar
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(substr(CAST(s AS text), b::int) AS bpchar) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s text, b bigint, n bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT substr(s, b::int, n::int) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s text, b bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT substr(s, b::int) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s bytea, b bigint, n bigint) RETURNS bytea
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT substr(s, b::int, n::int) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.substr(s bytea, b bigint) RETURNS bytea
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT substr(s, b::int) $as4tainr$;
-- HEX (Db2 for i SQL Reference): the hexadecimal digits of a value's
-- internal form (characters in CCSID 37).
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(b bytea) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(b, 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(t text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(as4tainr_svc.e37(t), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.interpret(b bytea, t text, p int, s int) RETURNS text
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  h text := encode(b, 'hex');
  d text := '';
  sign text;
  n numeric := 0;
BEGIN
  IF b IS NULL THEN RETURN NULL; END IF;
  IF t = 'P' THEN
    IF length(b) <> p / 2 + 1 THEN
      RAISE EXCEPTION 'SQL0171: the bytes are not a packed decimal of % digits', p USING ERRCODE = '22018';
    END IF;
    sign := substr(h, length(h), 1);
    d := substr(h, 1, length(h) - 1);
    IF d !~ '^[0-9]*$' THEN RAISE EXCEPTION 'SQL0802: not a packed decimal' USING ERRCODE = '22018'; END IF;
    n := d::numeric / power(10::numeric, s);
    IF sign IN ('b', 'd') THEN n := -n; END IF;
    RETURN n::text;
  ELSIF t = 'S' THEN
    FOR i IN 1..length(b) LOOP
      d := d || substr(h, i * 2, 1);
    END LOOP;
    IF d !~ '^[0-9]*$' THEN RAISE EXCEPTION 'SQL0802: not a zoned decimal' USING ERRCODE = '22018'; END IF;
    n := d::numeric / power(10::numeric, s);
    IF substr(h, length(h) - 1, 1) IN ('b', 'd') THEN n := -n; END IF;
    RETURN n::text;
  ELSIF t = 'B' THEN
    FOR i IN 0..length(b) - 1 LOOP
      n := n * 256 + get_byte(b, i);
    END LOOP;
    IF get_byte(b, 0) >= 128 THEN n := n - power(2::numeric, 8 * length(b)); END IF;
    RETURN trunc(n)::text;
  ELSIF t = 'A' THEN
    RETURN as4tainr_svc.decode(b, 37);
  ELSIF t = 'V' THEN
    RETURN as4tainr_svc.decode(substr(b, 3, get_byte(b, 0) * 256 + get_byte(b, 1)), 37);
  END IF;
  RETURN NULL;
END $as4tainr$;
"#;

/// QSYS2.DISPLAY_JOURNAL (Db2 for i services, DISPLAY_JOURNAL table
/// function): its 23 parameters and 54 result columns, in IBM's order.
const DISPLAY_JOURNAL: &str = r#"
DO $as4tainr$ BEGIN
IF NOT EXISTS (SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
               WHERE n.nspname = 'qsys2' AND t.typname = 'display_journal_row') THEN
CREATE TYPE qsys2.display_journal_row AS (
    entry_timestamp timestamp, sequence_number numeric(21, 0), journal_code char(1),
    journal_entry_type char(2), count_or_rrn bigint, entry_data bytea, null_value_indicators varchar(8000),
    "object" char(30), object_type varchar(10), object_type_indicator char(1), file_type_indicator char(1),
    journal_identifier varchar(10), user_name varchar(10), job_name varchar(10), job_user varchar(10),
    job_number varchar(6), thread bigint, program_name varchar(10), program_library varchar(10),
    program_library_asp_device varchar(10), program_library_asp_number integer, commit_cycle numeric(21, 0),
    nested_commit_level bigint, xid varchar(140), luw varchar(39), remote_port integer,
    remote_address varchar(46), system_name varchar(8), system_sequence_number numeric(21, 0),
    referential_constraint char(1), "trigger" char(1), ignore_on_apply char(1), minimized_entry_data char(1),
    minimized_on_field_boundary char(1), indicator_flag char(1), receiver_name varchar(10),
    receiver_library varchar(10), receiver_asp_device varchar(10), receiver_asp_number integer,
    arm_number integer, object_asp_device varchar(10), object_asp_number integer, parent_file_id bytea,
    object_file_id bytea, relative_directory_file_id bytea, object_file_name varchar(2002),
    path_name text, dlo_name varchar(12), folder_path varchar(63), "current_user" varchar(10),
    syslog_event varchar(2048), syslog_facility integer, syslog_severity integer, syslog_priority integer);
END IF;
END $as4tainr$;
-- An older OBJECT attribute is changed (only then: a user's view over
-- DISPLAY_JOURNAL depends on the type).
DO $as4tainr$ BEGIN
IF EXISTS (SELECT 1 FROM pg_attribute a JOIN pg_type t ON t.typrelid = a.attrelid
             JOIN pg_namespace n ON n.oid = t.typnamespace
            WHERE n.nspname = 'qsys2' AND t.typname = 'display_journal_row' AND a.attname = 'object'
              AND format_type(a.atttypid, a.atttypmod) <> 'character(30)') THEN
  ALTER TYPE qsys2.display_journal_row ALTER ATTRIBUTE "object" TYPE char(30);
END IF;
END $as4tainr$;
CREATE OR REPLACE FUNCTION qsys2.display_journal(
    journal_library varchar, journal_name varchar,
    starting_receiver_library varchar DEFAULT NULL, starting_receiver_name varchar DEFAULT '*CURRENT',
    starting_timestamp timestamp DEFAULT NULL, starting_sequence numeric DEFAULT NULL,
    journal_codes varchar DEFAULT '*ALL', journal_entry_types varchar DEFAULT '*ALL',
    object_library varchar DEFAULT NULL, object_name varchar DEFAULT NULL,
    object_objtype varchar DEFAULT NULL, object_member varchar DEFAULT NULL,
    "user" varchar DEFAULT '*ALL', job varchar DEFAULT '*ALL', program varchar DEFAULT '*ALL',
    ending_receiver_library varchar DEFAULT NULL, ending_receiver_name varchar DEFAULT '*CURRENT',
    ending_timestamp timestamp DEFAULT NULL, ending_sequence numeric DEFAULT NULL,
    generate_syslog varchar DEFAULT 'NO', eof_delay integer DEFAULT 0,
    commit_cycle varchar DEFAULT '*ALL', include_internal varchar DEFAULT 'NO')
RETURNS SETOF qsys2.display_journal_row
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  jl text := upper(trim(journal_library));
  jn text := upper(trim(journal_name));
  srl text := upper(trim(coalesce(starting_receiver_library, '*LIBL')));
  srn text := upper(trim(coalesce(starting_receiver_name, '*CURRENT')));
  erl text := upper(trim(coalesce(ending_receiver_library, '*LIBL')));
  ern text := upper(trim(coalesce(ending_receiver_name, '*CURRENT')));
  j record;
  rcvs text[];
  first_at timestamptz;
  last_at timestamptz;
  codes text[];
  types text[];
  objs text[];
BEGIN
  IF jl IN ('*LIBL', '*CURLIB') THEN
    RAISE EXCEPTION 'SQL0171: JOURNAL_LIBRARY cannot be %', jl USING ERRCODE = '22546';
  END IF;
  SELECT * INTO j FROM as4tainr.journals WHERE library = jl AND name = jn;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'CPF9801: Object % in library % not found.', jn, jl USING ERRCODE = '42704';
  END IF;
  -- The receivers in the range, by the time each was attached.
  IF srn = '*CURRENT' THEN
    first_at := (SELECT attached FROM as4tainr.journal_receivers WHERE library = j.receiver_lib AND name = j.receiver);
  ELSIF srn IN ('*CURCHAIN', '*CURAVLCHN', '*CURSEQCHN') THEN
    first_at := '-infinity';
  ELSE
    first_at := (SELECT attached FROM as4tainr.journal_receivers
                  WHERE name = srn AND (srl IN ('*LIBL', '*CURLIB') OR library = srl)
                    AND journal_lib = jl AND journal = jn);
    IF first_at IS NULL THEN
      RAISE EXCEPTION 'CPF9801: Object % in library % not found.', srn, srl USING ERRCODE = '42704';
    END IF;
  END IF;
  IF ern = '*CURRENT' THEN
    last_at := 'infinity';
  ELSE
    last_at := (SELECT attached FROM as4tainr.journal_receivers
                 WHERE name = ern AND (erl IN ('*LIBL', '*CURLIB') OR library = erl)
                   AND journal_lib = jl AND journal = jn);
  END IF;
  rcvs := ARRAY(SELECT library || '/' || name FROM as4tainr.journal_receivers
                 WHERE journal_lib = jl AND journal = jn AND attached >= first_at AND attached <= coalesce(last_at, 'infinity'));
  codes := regexp_split_to_array(upper(trim(coalesce(journal_codes, '*ALL'))), '[ ,]+');
  types := regexp_split_to_array(upper(trim(coalesce(journal_entry_types, '*ALL'))), '[ ,]+');
  objs := regexp_split_to_array(upper(trim(coalesce(object_name, '*ALL'))), '[ ,]+');
  RETURN QUERY
  SELECT e.ts::timestamp, e.seq::numeric(21, 0), e.code::char(1), e.type::char(2), e.count_rrn,
         e.entry_data, e.null_ind::varchar(8000), e.object::char(30), e.object_type::varchar(10),
         CASE WHEN e.object IS NULL THEN NULL ELSE '1' END::char(1),
         CASE WHEN e.object IS NULL THEN NULL ELSE '0' END::char(1),
         NULL::varchar(10), e.user_name::varchar(10), e.job_name::varchar(10), e.job_user::varchar(10),
         e.job_number::varchar(6), 0::bigint, e.program_name::varchar(10), e.program_lib::varchar(10),
         NULL::varchar(10), CASE WHEN e.program_name IS NULL THEN NULL ELSE 1 END::integer,
         e.commit_cycle::numeric(21, 0), 0::bigint, NULL::varchar(140), NULL::varchar(39), NULL::integer,
         NULL::varchar(46), 'AS4TAINR'::varchar(8), e.seq::numeric(21, 0), '0'::char(1), '0'::char(1),
         '0'::char(1), '0'::char(1), '0'::char(1), '0'::char(1), e.receiver::varchar(10),
         e.receiver_lib::varchar(10), NULL::varchar(10), 1, NULL::integer, NULL::varchar(10),
         CASE WHEN e.object IS NULL THEN NULL ELSE 1 END::integer, NULL::bytea, NULL::bytea, NULL::bytea,
         NULL::varchar(2002), NULL::text, NULL::varchar(12), NULL::varchar(63), e.user_name::varchar(10),
         NULL::varchar(2048), NULL::integer, NULL::integer, NULL::integer
    FROM as4tainr.journal_entries e
   WHERE e.journal_lib = jl AND e.journal = jn
     AND (e.receiver_lib || '/' || e.receiver) = ANY (rcvs)
     AND (starting_timestamp IS NULL OR e.ts >= starting_timestamp)
     AND (ending_timestamp IS NULL OR e.ts <= ending_timestamp)
     AND (starting_sequence IS NULL OR e.seq >= starting_sequence)
     AND (ending_sequence IS NULL OR e.seq <= ending_sequence)
     AND ('*ALL' = ANY (codes) OR e.code = ANY (codes)
          OR ('*CTL' = ANY (codes) AND e.code IN ('J', 'F')))
     AND ('*ALL' = ANY (types) OR e.type = ANY (types)
          OR ('*RCD' = ANY (types) AND e.code = 'R'))
     AND (object_name IS NULL OR '*ALL' = ANY (objs) OR rtrim(substr(e.object, 1, 10)) = ANY (objs))
     AND (object_library IS NULL OR upper(trim(object_library)) IN ('*LIBL', '*CURLIB')
          OR rtrim(substr(e.object, 11, 10)) = upper(trim(object_library)))
     AND (upper(trim(coalesce("user", '*ALL'))) = '*ALL' OR e.user_name = upper(trim("user")))
     AND (upper(trim(coalesce(program, '*ALL'))) = '*ALL' OR e.program_name = upper(trim(program)))
     AND (upper(trim(coalesce(job, '*ALL'))) = '*ALL'
          OR e.job_number || '/' || e.job_user || '/' || e.job_name = upper(trim(job)))
     AND (upper(trim(coalesce(display_journal.commit_cycle, '*ALL'))) = '*ALL'
          OR e.commit_cycle::text = trim(display_journal.commit_cycle))
   ORDER BY e.seq;
END $as4tainr$;
"#;

/// A journal or receiver name for messages.
fn qualified(lib: &str, name: &str) -> String {
    format!("{lib}/{name}")
}

/// The next name in a series (CHGJRN JRNRCV(*GEN)): the digits at the end
/// counted up, keeping their number (COMJRN0001 -> COMJRN0002); without
/// digits, 0001 added (up to the 10 characters a name has).
pub fn next_receiver_name(name: &str) -> String {
    let digits = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 {
        let mut n: String = name.chars().take(6).collect();
        n.push_str("0001");
        return n;
    }
    let (stem, num) = name.split_at(name.len() - digits);
    let next = num.parse::<u64>().unwrap_or(0) + 1;
    let s = format!("{next:0digits$}");
    if s.len() > digits {
        format!("{stem}{}", "0".repeat(digits))
    } else {
        format!("{stem}{s}")
    }
}

/// Write a journal entry from as4tainr (F JM, F EJ, J PR...) for a job
/// (its name "number/user/name") and user.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn entry(
    client: &Client,
    jlib: &str,
    jname: &str,
    code: &str,
    ty: &str,
    object: Option<String>,
    object_type: Option<&str>,
    who: (&str, &str),
) -> Result<()> {
    let (job, user) = who;
    client
        .execute(
            "SELECT set_config('as4tainr.job', $7, true), set_config('as4tainr.user', $8, true),
                    as4tainr_svc.journal_entry($1, $2, $3, $4, 0, NULL, NULL, $5, $6)",
            &[
                &jlib,
                &jname,
                &code,
                &ty,
                &object,
                &object_type,
                &job,
                &user,
            ],
        )
        .await?;
    Ok(())
}

pub(crate) fn member_object(lib: &str, file: &str) -> String {
    format!("{file:<10}{lib:<10}{file:<10}")
}

/// The journal a file is journaled to, if it is.
pub async fn journal_of(
    client: &Client,
    lib: &str,
    file: &str,
) -> Result<Option<(String, String)>> {
    Ok(client
        .query_opt(
            "SELECT journal_lib, journal FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
            &[&lib, &file],
        )
        .await?
        .map(|r| (r.get(0), r.get(1))))
}

/// Journaling ends for the file.
async fn file_deleted(client: &Client, lib: &str, file: &str, who: (&str, &str)) -> Result<()> {
    if let Some((jl, jn)) = journal_of(client, lib, file).await? {
        client
            .execute(
                "DELETE FROM as4tainr.journaled_files WHERE library=$1 AND file=$2",
                &[&lib, &file],
            )
            .await?;
        entry(
            client,
            &jl,
            &jn,
            "F",
            "EJ",
            Some(member_object(lib, file)),
            Some("*QDDS"),
            who,
        )
        .await?;
    }
    Ok(())
}

impl Job<'_> {
    /// The job's name, number/user/name (empty outside a job).
    pub(crate) fn job_name(&self) -> String {
        self.id.as_ref().map(|i| i.to_string()).unwrap_or_default()
    }

    /// An existing object by qualified name: (library, name), or CPF9801.
    async fn find_object(&self, q: &str, objtype: &str) -> Result<(String, String)> {
        let (libs, name) = self.search(&q.to_ascii_uppercase());
        for l in &libs {
            if catalog::object_exists(self.client, std::slice::from_ref(l), &name, objtype).await? {
                return Ok((l.clone(), name));
            }
        }
        let lib = q.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
        bail!("CPF9801: Object {name} in library {lib} not found.");
    }

    pub(crate) async fn journal_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        let msg = match cmd.def.name {
            "CRTJRNRCV" => self.crtjrnrcv(cmd).await?,
            "CRTJRN" => self.crtjrn(cmd).await?,
            "CHGJRN" => self.chgjrn(cmd).await?,
            "DLTJRN" => self.dltjrn(cmd).await?,
            "DLTJRNRCV" => self.dltjrnrcv(cmd).await?,
            "STRJRNPF" => self.strjrnpf(cmd).await?,
            "ENDJRNPF" => self.endjrnpf(cmd).await?,
            "DSPJRN" => return self.dspjrn(cmd).await.map(Some),
            "APYJRNCHG" => self.apply_journal(cmd, false).await?,
            "RMVJRNCHG" => self.apply_journal(cmd, true).await?,
            _ => return Ok(None),
        };
        Ok(Some(Outcome::Messages(msg)))
    }

    async fn crtjrnrcv(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name) = self.create_target(&cmd.get("JRNRCV").to_ascii_uppercase());
        let name = catalog::validate_name("journal receiver", &name)?;
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} not found.");
        }
        if catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*JRNRCV").await?
        {
            bail!("CPF7010: Object {name} in {lib} type *JRNRCV already exists.");
        }
        let text = cmd.get("TEXT").to_string();
        let text = if text.starts_with('*') {
            String::new()
        } else {
            text
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, text) VALUES ($1, $2, '*JRNRCV', $3)",
                &[&lib, &name, &text],
            )
            .await?;
        self.client
            .execute(
                "INSERT INTO as4tainr.journal_receivers (library, name, text) VALUES ($1, $2, $3)",
                &[&lib, &name, &text],
            )
            .await?;
        crate::authority::set_owner(self.client, &lib, &name, "*JRNRCV", &self.user).await?;
        Ok(vec![format!(
            "Journal receiver {name} created in library {lib}."
        )])
    }

    /// A receiver to attach: it exists and has never been attached.
    async fn receiver_to_attach(&self, q: &str) -> Result<(String, String)> {
        let (lib, name) = self.find_object(q, "*JRNRCV").await?;
        let used: Option<String> = self
            .client
            .query_one(
                "SELECT journal FROM as4tainr.journal_receivers WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        if used.is_some() {
            bail!("CPF9898: Journal receiver {name} in {lib} was attached to a journal before.");
        }
        Ok((lib, name))
    }

    async fn attach(&self, jlib: &str, jname: &str, rlib: &str, rname: &str) -> Result<()> {
        self.client
            .execute(
                "UPDATE as4tainr.journal_receivers SET journal_lib=$3, journal=$4, attached=clock_timestamp()
                 WHERE library=$1 AND name=$2",
                &[&rlib, &rname, &jlib, &jname],
            )
            .await?;
        Ok(())
    }

    async fn crtjrn(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name) = self.create_target(&cmd.get("JRN").to_ascii_uppercase());
        let name = catalog::validate_name("journal", &name)?;
        if !catalog::library_exists(self.client, &lib).await? {
            bail!("CPF2110: Library {lib} not found.");
        }
        if catalog::object_exists(self.client, std::slice::from_ref(&lib), &name, "*JRN").await? {
            bail!("CPF7010: Object {name} in {lib} type *JRN already exists.");
        }
        let (rlib, rname) = self.receiver_to_attach(cmd.get("JRNRCV")).await?;
        let text = cmd.get("TEXT").to_string();
        let text = if text.starts_with('*') {
            String::new()
        } else {
            text
        };
        self.client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, text) VALUES ($1, $2, '*JRN', $3)",
                &[&lib, &name, &text],
            )
            .await?;
        let jid: i32 = self
            .client
            .query_one(
                "INSERT INTO as4tainr.journals (library, name, receiver_lib, receiver, text)
                 VALUES ($1, $2, $3, $4, $5) RETURNING jid",
                &[&lib, &name, &rlib, &rname, &text],
            )
            .await?
            .get(0);
        self.client
            .batch_execute(&format!("CREATE SEQUENCE IF NOT EXISTS {}", seq_name(jid)))
            .await?;
        self.attach(&lib, &name, &rlib, &rname).await?;
        crate::authority::set_owner(self.client, &lib, &name, "*JRN", &self.user).await?;
        Ok(vec![format!("Journal {name} created in library {lib}.")])
    }

    async fn journal(&self, q: &str) -> Result<(String, String, i32, String, String)> {
        let (lib, name) = self.find_object(q, "*JRN").await?;
        let r = self
            .client
            .query_one(
                "SELECT jid, receiver_lib, receiver FROM as4tainr.journals WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        Ok((lib, name, r.get(0), r.get(1), r.get(2)))
    }

    /// CHGJRN JRNRCV(*GEN | name) [SEQOPT(*CONT | *RESET)]: a new
    /// receiver attached, the old one detached.
    async fn chgjrn(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name, jid, rlib, rname) = self.journal(cmd.get("JRN")).await?;
        let want = cmd.get("JRNRCV").to_ascii_uppercase();
        let (nlib, nname) = match want.as_str() {
            "" | "*SAME" => return Ok(vec![format!("Journal {name} in {lib} not changed.")]),
            "*GEN" => {
                let n = next_receiver_name(&rname);
                if !catalog::object_exists(self.client, std::slice::from_ref(&rlib), &n, "*JRNRCV")
                    .await?
                {
                    self.client
                        .execute(
                            "INSERT INTO as4tainr.objects (library, name, objtype) VALUES ($1, $2, '*JRNRCV')",
                            &[&rlib, &n],
                        )
                        .await?;
                    self.client
                        .execute(
                            "INSERT INTO as4tainr.journal_receivers (library, name) VALUES ($1, $2)",
                            &[&rlib, &n],
                        )
                        .await?;
                    crate::authority::set_owner(self.client, &rlib, &n, "*JRNRCV", &self.user)
                        .await?;
                }
                self.receiver_to_attach(&format!("{rlib}/{n}")).await?
            }
            q => self.receiver_to_attach(q).await?,
        };
        let job = self.job_name();
        entry(
            self.client,
            &lib,
            &name,
            "J",
            "PR",
            None,
            None,
            (&job, &self.user),
        )
        .await?;
        self.client
            .execute(
                "UPDATE as4tainr.journal_receivers SET detached=clock_timestamp() WHERE library=$1 AND name=$2",
                &[&rlib, &rname],
            )
            .await?;
        self.client
            .execute(
                "UPDATE as4tainr.journals SET receiver_lib=$3, receiver=$4 WHERE library=$1 AND name=$2",
                &[&lib, &name, &nlib, &nname],
            )
            .await?;
        self.attach(&lib, &name, &nlib, &nname).await?;
        if cmd.get("SEQOPT").eq_ignore_ascii_case("*RESET") {
            self.client
                .batch_execute(&format!("ALTER SEQUENCE {} RESTART", seq_name(jid)))
                .await?;
        }
        Ok(vec![format!(
            "Journal receiver {} attached to journal {}.",
            qualified(&nlib, &nname),
            qualified(&lib, &name)
        )])
    }

    async fn dltjrn(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name, jid, _, _) = self.journal(cmd.get("JRN")).await?;
        let files: i64 = self
            .client
            .query_one(
                "SELECT count(*) FROM as4tainr.journaled_files WHERE journal_lib=$1 AND journal=$2",
                &[&lib, &name],
            )
            .await?
            .get(0);
        if files > 0 {
            bail!("CPF9898: Journal {name} in {lib} cannot be deleted: {files} files are journaled to it (ENDJRNPF first).");
        }
        self.client
            .execute(
                "UPDATE as4tainr.journal_receivers SET detached=coalesce(detached, clock_timestamp())
                 WHERE journal_lib=$1 AND journal=$2",
                &[&lib, &name],
            )
            .await?;
        self.client
            .execute(
                "DELETE FROM as4tainr.journals WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        self.client
            .execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*JRN'",
                &[&lib, &name],
            )
            .await?;
        self.client
            .batch_execute(&format!("DROP SEQUENCE IF EXISTS {}", seq_name(jid)))
            .await?;
        Ok(vec![format!("Journal {name} in {lib} deleted.")])
    }

    async fn dltjrnrcv(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (lib, name) = self.find_object(cmd.get("JRNRCV"), "*JRNRCV").await?;
        let r = self
            .client
            .query_one(
                "SELECT journal_lib, journal, detached IS NULL AND journal IS NOT NULL
                   FROM as4tainr.journal_receivers WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        let attached: bool = r.get(2);
        if attached {
            bail!("CPF9898: Journal receiver {name} in {lib} is attached to its journal; change the journal (CHGJRN) first.");
        }
        let (jl, jn): (Option<String>, Option<String>) = (r.get(0), r.get(1));
        if let (Some(jl), Some(jn)) = (jl, jn) {
            self.client
                .execute(
                    "DELETE FROM as4tainr.journal_entries WHERE journal_lib=$1 AND journal=$2
                       AND receiver_lib=$3 AND receiver=$4",
                    &[&jl, &jn, &lib, &name],
                )
                .await?;
        }
        self.client
            .execute(
                "DELETE FROM as4tainr.journal_receivers WHERE library=$1 AND name=$2",
                &[&lib, &name],
            )
            .await?;
        self.client
            .execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*JRNRCV'",
                &[&lib, &name],
            )
            .await?;
        Ok(vec![format!("Journal receiver {name} in {lib} deleted.")])
    }

    /// The physical files a FILE list names (generic names and *ALL).
    async fn physical_files(&self, items: &[String]) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for it in items {
            let it = it.trim().to_ascii_uppercase();
            let (libs, name) = self.search(&it);
            let generic = name == "*ALL" || name.ends_with('*');
            let mut found = false;
            for l in &libs {
                let rows = self
                    .client
                    .query(
                        "SELECT name FROM as4tainr.objects WHERE library=$1 AND objtype='*FILE' AND attribute='PF'
                           AND (name=$2 OR ($3 AND ($2 = '*ALL' OR name LIKE rtrim($2, '*') || '%')))
                         ORDER BY name",
                        &[l, &name, &generic],
                    )
                    .await?;
                for r in rows {
                    out.push((l.clone(), r.get::<_, String>(0)));
                    found = true;
                }
                if found && !generic {
                    break;
                }
            }
            if !found {
                if !generic && catalog::object_exists(self.client, &libs, &name, "*FILE").await? {
                    bail!("CPF9898: File {name} is not a physical file.");
                }
                let lib = it.split_once('/').map(|x| x.0).unwrap_or("*LIBL");
                bail!("CPF9812: File {name} in library {lib} not found.");
            }
        }
        Ok(out)
    }

    /// STRJRNPF FILE(...) JRN(...) [IMAGES(*AFTER|*BOTH)]
    /// [OMTJRNE(*NONE|*OPNCLO)]: every change to the files is journaled.
    async fn strjrnpf(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let (jlib, jname, _, _, _) = self.journal(cmd.get("JRN")).await?;
        let files = self.physical_files(&cmd.list("FILE")).await?;
        let images = match cmd.get("IMAGES").to_ascii_uppercase().as_str() {
            "*BOTH" => "*BOTH",
            _ => "*AFTER",
        };
        let omit = match cmd.get("OMTJRNE").to_ascii_uppercase().as_str() {
            "*OPNCLO" => "*OPNCLO",
            _ => "*NONE",
        };
        let mut msgs = Vec::new();
        for (lib, file) in &files {
            crate::authority::check(
                self.client,
                &self.user,
                lib,
                file,
                "*FILE",
                crate::authority::Aut::All,
            )
            .await?;
            if let Some((l, j)) = journal_of(self.client, lib, file).await? {
                bail!("CPF7030: Object {file} in {lib} of type *FILE is already being journaled (to {l}/{j}).");
            }
            self.client
                .execute(
                    "INSERT INTO as4tainr.journaled_files (library, file, journal_lib, journal, images, omtjrne)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[lib, file, &jlib, &jname, &images, &omit],
                )
                .await?;
            let q = |s: &str| crate::dds::quote(s);
            self.client
                .batch_execute(&format!(
                    "CREATE TRIGGER \"as4tainr~journal\" AFTER INSERT OR UPDATE OR DELETE ON {}.{}
                     FOR EACH ROW EXECUTE FUNCTION as4tainr_svc.journal_trigger({}, {}, {}, {}, {})",
                    ident(lib),
                    ident(file),
                    q(&jlib),
                    q(&jname),
                    q(images),
                    q(lib),
                    q(file)
                ))
                .await?;
            let job = self.job_name();
            entry(
                self.client,
                &jlib,
                &jname,
                "F",
                "JM",
                Some(member_object(lib, file)),
                Some("*QDDS"),
                (&job, &self.user),
            )
            .await?;
            msgs.push(format!(
                "Journaling started for file {lib}/{file} (journal {jlib}/{jname})."
            ));
        }
        msgs.push(format!(
            "{} of {} objects have started journaling.",
            files.len(),
            files.len()
        ));
        Ok(msgs)
    }

    /// ENDJRNPF FILE(... | *ALL) JRN(*FILE | journal).
    async fn endjrnpf(&mut self, cmd: &Command) -> Result<Vec<String>> {
        let items = cmd.list("FILE");
        let jrn = cmd.get("JRN").to_ascii_uppercase();
        let files: Vec<(String, String)> = if items.len() == 1
            && items[0].eq_ignore_ascii_case("*ALL")
        {
            if jrn.is_empty() || jrn == "*FILE" {
                bail!("CPF9898: FILE(*ALL) needs the journal (JRN).");
            }
            let (jl, jn, _, _, _) = self.journal(&jrn).await?;
            self.client
                .query(
                    "SELECT library, file FROM as4tainr.journaled_files WHERE journal_lib=$1 AND journal=$2 ORDER BY 1, 2",
                    &[&jl, &jn],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect()
        } else {
            self.physical_files(&items).await?
        };
        let mut msgs = Vec::new();
        for (lib, file) in &files {
            let Some((jl, jn)) = journal_of(self.client, lib, file).await? else {
                bail!("CPF9898: File {file} in library {lib} is not journaled.");
            };
            self.client
                .batch_execute(&format!(
                    "DROP TRIGGER IF EXISTS \"as4tainr~journal\" ON {}.{}",
                    ident(lib),
                    ident(file)
                ))
                .await?;
            let job = self.job_name();
            file_deleted(self.client, lib, file, (&job, &self.user)).await?;
            msgs.push(format!(
                "Journaling ended for file {lib}/{file} (journal {jl}/{jn})."
            ));
        }
        Ok(msgs)
    }

    /// DSPJRN JRN(...) [FILE(...)] [RCVRNG(*CURRENT | *CURCHAIN)]
    /// [JRNCDE(...)] [ENTTYP(...)]: the entries, one line each.
    async fn dspjrn(&mut self, cmd: &Command) -> Result<Outcome> {
        let (lib, name, _, _, _) = self.journal(cmd.get("JRN")).await?;
        let rcvrng = cmd.get("RCVRNG").to_ascii_uppercase();
        let start = if rcvrng.contains("*CURCHAIN") {
            "*CURCHAIN"
        } else {
            "*CURRENT"
        };
        let file = cmd
            .list("FILE")
            .first()
            .map(|f| f.to_ascii_uppercase())
            .filter(|f| !f.starts_with("*ALL") && !f.is_empty());
        let codes = cmd
            .list("JRNCDE")
            .iter()
            .map(|c| {
                c.trim_start_matches('(')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(')')
                    .to_ascii_uppercase()
            })
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let codes = if codes.is_empty() {
            "*ALL".to_string()
        } else {
            codes
        };
        let types = cmd.list("ENTTYP").join(" ").to_ascii_uppercase();
        let types = if types.is_empty() {
            "*ALL".to_string()
        } else {
            types
        };
        let (flib, fname) = match &file {
            Some(f) => match f.split_once('/') {
                Some((l, n)) => (Some(l.to_string()), Some(n.to_string())),
                None => (None, Some(f.clone())),
            },
            None => (None, None),
        };
        let rows = self
            .client
            .query(
                "SELECT sequence_number::text, journal_code, journal_entry_type, coalesce(rtrim(substr(\"object\", 1, 10)), ''),
                        coalesce(rtrim(substr(\"object\", 11, 10)), ''), coalesce(job_number || '/' || job_user || '/' || job_name, ''),
                        coalesce(program_name, ''), to_char(entry_timestamp, 'YYYY-MM-DD HH24:MI:SS'), commit_cycle::text
                   FROM qsys2.display_journal($1, $2, starting_receiver_name => $3, journal_codes => $4,
                        journal_entry_types => $5, object_library => $6, object_name => $7)",
                &[&lib, &name, &start, &codes, &types, &flib, &fname],
            )
            .await?;
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                let g = |i: usize| r.get::<_, String>(i);
                format!(
                    "{:>10} {} {} {:<10} {:<10} {:<28} {:<10} {} {}",
                    g(0),
                    g(1),
                    g(2),
                    g(3),
                    g(4),
                    g(5),
                    g(6),
                    g(7),
                    g(8)
                )
            })
            .collect();
        Ok(Outcome::Display(ListDisplay {
            title: "Display Journal Entries".into(),
            info: vec![
                ("Journal".into(), name.clone()),
                ("Library".into(), lib.clone()),
            ],
            heading: format!(
                "{:>10} C Ty {:<10} {:<10} {:<28} {:<10} {:<19} {}",
                "Sequence", "Object", "Library", "Job", "Program", "Time", "Cycle"
            ),
            rows: lines,
            ..Default::default()
        }))
    }
}

/// The entries of the open commit cycles, held over a rollback (None:
/// there are none).
pub async fn hold(client: &Client) -> Option<String> {
    client
        .query_one("SELECT as4tainr_svc.journal_hold()::text", &[])
        .await
        .ok()
        .and_then(|r| r.get(0))
}

/// After a rollback: its entries back in the journal, and the record
/// changes it undid journaled (`as4tainr_svc.journal_restore`).
pub async fn restore(client: &Client, held: Option<String>, whole: bool) -> Result<()> {
    if held.is_some() {
        client
            .execute(
                "SELECT as4tainr_svc.journal_restore($1::text::jsonb, $2)",
                &[&held, &whole],
            )
            .await?;
    }
    Ok(())
}

/// ROLLBACK TO SAVEPOINT under commitment control, journaled.
pub async fn rollback_to(client: &Client, sp: &str) -> Result<()> {
    let held = hold(client).await;
    client
        .batch_execute(&format!("ROLLBACK TO SAVEPOINT {sp}"))
        .await?;
    restore(client, held, false).await
}

/// A record image (and null byte map) of a file as a JSON object of its
/// column values, for `jsonb_populate_record`: the inverse of
/// `as4tainr_svc.field_image`.
pub async fn image_json(
    catalog: &Client,
    lib: &str,
    file: &str,
    img: &[u8],
    nulls: &str,
) -> Result<String> {
    let rows = catalog
        .query(
            "SELECT name, alias, data_type, length, decimals, varlen, flt_double
               FROM as4tainr.file_fields
              WHERE library = $1 AND file = $2
                AND data_type NOT IN ('CLOB', 'BLOB', 'DBCLOB', 'DECFLOAT', 'BOOLEAN', 'XML')
              ORDER BY seq",
            &[&lib, &file],
        )
        .await?;
    let mut obj = serde_json::Map::new();
    let mut off = 0usize;
    let nulls: Vec<char> = nulls.chars().collect();
    for (i, r) in rows.iter().enumerate() {
        let name: String = r.get(0);
        let alias: Option<String> = r.get(1);
        let t: String = r.get(2);
        let len = r.get::<_, i32>(3).max(0) as usize;
        let decs = r.get::<_, i32>(4).max(0) as u32;
        let varlen: bool = r.get(5);
        let dbl: bool = r.get(6);
        let key = catalog::pg_name(alias.as_deref().unwrap_or(&name));
        let (v, n) = field_value(&t, len, decs, varlen, dbl, &img[off.min(img.len())..])?;
        off += n;
        let v = if nulls.get(i) == Some(&'1') {
            serde_json::Value::Null
        } else {
            v
        };
        obj.insert(key, v);
    }
    Ok(serde_json::Value::Object(obj).to_string())
}

/// One field's value from its bytes in a record image, and its size.
fn field_value(
    t: &str,
    len: usize,
    decs: u32,
    varlen: bool,
    dbl: bool,
    b: &[u8],
) -> Result<(serde_json::Value, usize)> {
    use serde_json::Value;
    let need = |n: usize| -> Result<&[u8]> {
        b.get(..n)
            .ok_or_else(|| anyhow::anyhow!("the record image is shorter than the record format"))
    };
    let vlen = |b: &[u8]| -> usize { u16::from_be_bytes([b[0], b[1]]) as usize };
    let num = |digits: String, neg: bool| -> Value {
        let d = format!("{:0>w$}", digits, w = decs as usize + 1);
        let (i, f) = d.split_at(d.len() - decs as usize);
        let s = if f.is_empty() {
            i.to_string()
        } else {
            format!("{i}.{f}")
        };
        Value::String(if neg { format!("-{s}") } else { s })
    };
    Ok(match t {
        "P" => {
            let n = len / 2 + 1;
            let x = need(n)?;
            let h: String = x.iter().map(|c| format!("{c:02x}")).collect();
            let (digits, sign) = h.split_at(h.len() - 1);
            (num(digits.to_string(), matches!(sign, "d" | "b")), n)
        }
        "S" => {
            let x = need(len)?;
            let digits: String = x.iter().map(|c| char::from(b'0' + (c & 0x0F))).collect();
            let neg = x.last().is_some_and(|c| matches!(c >> 4, 0xD | 0xB));
            (num(digits, neg), len)
        }
        "B" => {
            let n = if len <= 4 {
                2
            } else if len <= 9 {
                4
            } else {
                8
            };
            let x = need(n)?;
            let v: i64 = match n {
                2 => i16::from_be_bytes([x[0], x[1]]) as i64,
                4 => i32::from_be_bytes(x.try_into()?) as i64,
                _ => i64::from_be_bytes(x.try_into()?),
            };
            (num(v.unsigned_abs().to_string(), v < 0), n)
        }
        "F" => {
            if dbl || len > 9 {
                let x = need(8)?;
                (
                    Value::String(f64::from_be_bytes(x.try_into()?).to_string()),
                    8,
                )
            } else {
                let x = need(4)?;
                (
                    Value::String(f32::from_be_bytes(x.try_into()?).to_string()),
                    4,
                )
            }
        }
        "L" => {
            let x = need(10)?;
            (Value::String(crate::tn5250::ebcdic::decode(x)), 10)
        }
        "T" => {
            let x = need(8)?;
            (
                Value::String(crate::tn5250::ebcdic::decode(x).replace('.', ":")),
                8,
            )
        }
        "Z" => {
            let x = need(26)?;
            let s = crate::tn5250::ebcdic::decode(x);
            // YYYY-MM-DD-HH.MM.SS.ffffff
            let s = format!("{} {}{}", &s[..10], s[11..19].replace('.', ":"), &s[19..]);
            (Value::String(s), 26)
        }
        "H" => {
            let (x, n) = if varlen {
                let x = need(2 + len)?;
                (&x[2..2 + vlen(x).min(len)], 2 + len)
            } else {
                (need(len)?, len)
            };
            let h: String = x.iter().map(|c| format!("{c:02x}")).collect();
            (Value::String(format!("\\x{h}")), n)
        }
        "G" => {
            let (x, n) = if varlen {
                let x = need(2 + len * 2)?;
                (&x[2..2 + vlen(x).min(len) * 2], 2 + len * 2)
            } else {
                (need(len * 2)?, len * 2)
            };
            let units: Vec<u16> = x
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            let s = String::from_utf16_lossy(&units);
            (
                Value::String(if varlen { s } else { s.trim_end().to_string() }),
                n,
            )
        }
        _ => {
            if varlen {
                let x = need(2 + len)?;
                let s = crate::tn5250::ebcdic::decode(&x[2..2 + vlen(x).min(len)]);
                (Value::String(s), 2 + len)
            } else {
                let x = need(len)?;
                (Value::String(crate::tn5250::ebcdic::decode(x)), len)
            }
        }
    })
}

/// A journaled file was cleared (CLRPFM): its F CR entry.
pub async fn file_cleared(client: &Client, lib: &str, file: &str, who: (&str, &str)) -> Result<()> {
    if let Some((jl, jn)) = journal_of(client, lib, file).await? {
        entry(
            client,
            &jl,
            &jn,
            "F",
            "CR",
            Some(member_object(lib, file)),
            Some("*QDDS"),
            who,
        )
        .await?;
    }
    Ok(())
}

/// An entry limit of APYJRNCHG and RMVJRNCHG (FROMENT, TOENT).
fn entry_limit(v: &str, first: i64, last: i64) -> Result<i64> {
    match v.trim().to_ascii_uppercase().as_str() {
        "*FIRST" => Ok(first),
        "*LAST" | "*LASTRST" | "*COMPLETE" => Ok(last),
        "*LASTSAVE" => bail!("CPF0006: Errors occurred in command. *LASTSAVE is not valid here."),
        n => n.parse().map_err(|_| {
            anyhow::anyhow!("CPF0006: Errors occurred in command. {v} is not a sequence number.")
        }),
    }
}

impl Job<'_> {
    /// APYJRNCHG and RMVJRNCHG (CL reference; IBM i "Actions of applying
    /// or removing journaled changes by journal code"): the record
    /// changes journaled for files, applied oldest first (PT and PX add
    /// the record at its relative record number, UP and UR update it to
    /// the after-image, DL and DR delete it, F CR clears the member; UB
    /// and BR are ignored) or removed newest first (PT and PX delete the
    /// record, UB and BR update it to the before-image, DL and DR put it
    /// back; UP and UR are ignored; RMVJRNCHG needs IMAGES(*BOTH)). With
    /// CMTBDY(*YES) a change made under commitment control is applied or
    /// removed only when its commit cycle was committed within the range.
    /// Trigger programs are not called and the changes are not journaled
    /// one by one: the journal gets F SA (SR) when the command starts and
    /// F AY (RC) for each file.
    pub(crate) async fn apply_journal(
        &mut self,
        cmd: &Command,
        remove: bool,
    ) -> Result<Vec<String>> {
        let (jlib, jname, _, rlib, rname) = self.journal(cmd.get("JRN")).await?;
        let files = self.physical_files(&cmd.list("FILE")).await?;
        let rcvrng = cmd.list("RCVRNG").join(" ").to_ascii_uppercase();
        // RCVRNG(*LASTSAVE): the receivers from the one attached when the
        // file was saved to the one attached now. Every receiver keeps its
        // entries here, so that is the chain (*CURCHAIN).
        let current = !(rcvrng.contains("*CURCHAIN") || rcvrng.contains("*LASTSAVE"));
        let pick = |a: &str, b: &str| -> String {
            match cmd.get(b) {
                "" => cmd.get(a).to_string(),
                l => l.to_string(),
            }
        };
        let from = pick("FROMENT", "FROMENTLRG");
        let to = pick("TOENT", "TOENTLRG");
        // Apply: from the older to the newer entry; remove: from the
        // newer back to the older.
        let limits = |from: &str, to: &str| -> Result<(i64, i64)> {
            Ok(if remove {
                (
                    entry_limit(to, 0, 0)?,
                    entry_limit(from, i64::MAX, i64::MAX)?,
                )
            } else {
                (
                    entry_limit(from, 0, 0)?,
                    entry_limit(to, i64::MAX, i64::MAX)?,
                )
            })
        };
        // FROMENT(*LASTSAVE) (APYJRNCHG): from the entry after the file's
        // last save, its F MS entry (written by SAVOBJ/SAVLIB). RMVJRNCHG
        // has no *LASTSAVE.
        let lastsave = !remove && from.trim().eq_ignore_ascii_case("*LASTSAVE");
        if !lastsave {
            limits(&from, &to)?;
        }
        let cmtbdy = !cmd.get("CMTBDY").eq_ignore_ascii_case("*NO");
        let job = self.job_name();
        let who = (job.as_str(), self.user.as_str());
        let mut msgs = Vec::new();
        let tx = self.client;
        tx.batch_execute("BEGIN; SELECT set_config('as4tainr.apply', '1', true)")
            .await?;
        let r: Result<()> = async {
            entry(
                tx,
                &jlib,
                &jname,
                "F",
                if remove { "SR" } else { "SA" },
                None,
                None,
                who,
            )
            .await?;
            for (lib, file) in &files {
                let (lo, hi) = if lastsave {
                    let saved: Option<i64> = tx
                        .query_one(
                            "SELECT max(seq) FROM as4tainr.journal_entries
                              WHERE journal_lib=$1 AND journal=$2 AND object=$3 AND code='F' AND type='MS'",
                            &[&jlib, &jname, &member_object(lib, file)],
                        )
                        .await?
                        .get(0);
                    let Some(saved) = saved else {
                        bail!(
                            "CPF9898: No save of file {file} in {lib} is in journal {jname}; give FROMENT(*FIRST) or a sequence number."
                        );
                    };
                    (saved + 1, limits("*FIRST", &to)?.1)
                } else {
                    limits(&from, &to)?
                };
                let n = self
                    .apply_file(
                        &jlib,
                        &jname,
                        lib,
                        file,
                        remove,
                        (lo, hi),
                        current.then_some((&rlib, &rname)),
                        cmtbdy,
                    )
                    .await?;
                entry(
                    tx,
                    &jlib,
                    &jname,
                    "F",
                    if remove { "RC" } else { "AY" },
                    Some(member_object(lib, file)),
                    Some("*QDDS"),
                    who,
                )
                .await?;
                msgs.push(format!(
                    "{n} entries {} member {file} file {file} in {lib}.",
                    if remove { "removed from" } else { "applied to" }
                ));
            }
            Ok(())
        }
        .await;
        match r {
            Ok(()) => tx.batch_execute("COMMIT").await?,
            Err(e) => {
                tx.batch_execute("ROLLBACK").await?;
                return Err(e);
            }
        }
        Ok(msgs)
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_file(
        &self,
        jlib: &str,
        jname: &str,
        lib: &str,
        file: &str,
        remove: bool,
        (lo, hi): (i64, i64),
        receiver: Option<(&String, &String)>,
        cmtbdy: bool,
    ) -> Result<usize> {
        let tx = self.client;
        let images = tx
            .query_opt(
                "SELECT images FROM as4tainr.journaled_files WHERE library=$1 AND file=$2 AND journal_lib=$3 AND journal=$4",
                &[&lib, &file, &jlib, &jname],
            )
            .await?;
        // The file may no longer be journaled; its entries are still in
        // the journal.
        if remove && images.as_ref().map(|r| r.get::<_, String>(0)).as_deref() == Some("*AFTER") {
            bail!(
                "CPF9898: File {file} in {lib} is journaled with IMAGES(*AFTER); RMVJRNCHG needs before-images (IMAGES(*BOTH))."
            );
        }
        let obj = member_object(lib, file);
        let (rl, rn) = match receiver {
            Some((l, n)) => (Some(l.as_str()), Some(n.as_str())),
            None => (None, None),
        };
        let rows = tx
            .query(
                "SELECT e.seq, e.code::text, e.type::text, coalesce(e.count_rrn, 0), e.entry_data, coalesce(e.null_ind, ''),
                        e.commit_cycle::text,
                        e.commit_cycle = 0 OR EXISTS (SELECT 1 FROM as4tainr.journal_entries c
                            WHERE c.journal_lib = e.journal_lib AND c.journal = e.journal AND c.code = 'C'
                              AND c.type = 'CM' AND c.commit_cycle = e.commit_cycle AND c.seq BETWEEN $4 AND $5)
                   FROM as4tainr.journal_entries e
                  WHERE e.journal_lib = $1 AND e.journal = $2 AND e.object = $3 AND e.code IN ('R', 'F')
                    AND e.seq BETWEEN $4 AND $5
                    AND ($6::text IS NULL OR (e.receiver_lib = $6 AND e.receiver = $7))
                  ORDER BY e.seq",
                &[&jlib, &jname, &obj, &lo, &hi, &rl, &rn],
            )
            .await?;
        let table = format!("{}.{}", ident(lib), ident(file));
        let mut n = 0usize;
        let mut rows: Vec<_> = rows.iter().collect();
        if remove {
            rows.reverse();
        }
        for r in rows {
            let seq: i64 = r.get(0);
            let code: String = r.get(1);
            let ty: String = r.get(2);
            let rrn: i64 = r.get(3);
            let img: Option<Vec<u8>> = r.get(4);
            let nulls: String = r.get(5);
            let committed: bool = r.get(7);
            if cmtbdy && !committed {
                continue;
            }
            #[derive(PartialEq)]
            enum Act {
                Add,
                Put,
                Delete,
                Clear,
                Skip,
            }
            let act = match (code.as_str(), ty.as_str(), remove) {
                ("R", "PT" | "PX", false) => Act::Add,
                ("R", "UP" | "UR", false) => Act::Put,
                ("R", "DL" | "DR", false) => Act::Delete,
                ("F", "CR", false) => Act::Clear,
                ("R", "PT" | "PX", true) => Act::Delete,
                ("R", "UB" | "BR" | "DL" | "DR", true) => Act::Put,
                ("F", "CR", true) => bail!(
                    "CPF9898: Entry {seq} (F CR, member cleared) cannot be removed; {n} entries were removed from file {file} in {lib} before it."
                ),
                _ => Act::Skip,
            };
            if act == Act::Skip {
                continue;
            }
            let row = match (&act, &img) {
                (Act::Add | Act::Put, Some(img)) => {
                    Some(image_json(tx, lib, file, img, &nulls).await?)
                }
                (Act::Add | Act::Put, None) => {
                    bail!("CPF9898: Entry {seq} has no record image.")
                }
                _ => None,
            };
            match act {
                Act::Delete => {
                    tx.execute(&format!("DELETE FROM {table} WHERE rrn = $1"), &[&rrn])
                        .await?;
                }
                Act::Clear => {
                    tx.batch_execute(&format!("TRUNCATE TABLE {table} RESTART IDENTITY"))
                        .await?;
                }
                Act::Add | Act::Put => {
                    let row = row.unwrap_or_default();
                    let cols: Vec<String> =
                        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&row)?
                            .keys()
                            .map(|k| format!("\"{}\"", k.replace('"', "\"\"")))
                            .collect();
                    let exists = tx
                        .query_opt(&format!("SELECT 1 FROM {table} WHERE rrn = $1"), &[&rrn])
                        .await?
                        .is_some();
                    if exists && act == Act::Add {
                        bail!(
                            "CPF9898: Entry {seq}: record {rrn} of file {file} in {lib} already exists; {n} entries were applied before it."
                        );
                    }
                    if exists {
                        let sets: Vec<String> =
                            cols.iter().map(|c| format!("{c} = r.{c}")).collect();
                        tx.execute(
                            &format!(
                                "UPDATE {table} t SET {} FROM jsonb_populate_record(NULL::{table}, $2::text::jsonb) r WHERE t.rrn = $1",
                                sets.join(", ")
                            ),
                            &[&rrn, &row],
                        )
                        .await?;
                    } else {
                        tx.execute(
                            &format!(
                                "INSERT INTO {table} (rrn, {c}) SELECT $1, {c} FROM jsonb_populate_record(NULL::{table}, $2::text::jsonb)",
                                c = cols.join(", ")
                            ),
                            &[&rrn, &row],
                        )
                        .await?;
                    }
                }
                Act::Skip => {}
            }
            n += 1;
        }
        // New records get numbers after those put back.
        let t = table.replace('\'', "''");
        tx.batch_execute(&format!(
            "SELECT setval(pg_get_serial_sequence('{t}', 'rrn'),
                    greatest(m, nextval(pg_get_serial_sequence('{t}', 'rrn')) - 1, 1))
               FROM (SELECT max(rrn) AS m FROM {table}) x WHERE m IS NOT NULL"
        ))
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_receiver_names() {
        // CHGJRN JRNRCV(*GEN): the next receiver's name counts up the
        // digits at the end (CL reference, CHGJRN).
        assert_eq!(next_receiver_name("COMJRN0001"), "COMJRN0002");
        assert_eq!(next_receiver_name("RCV0099"), "RCV0100");
        assert_eq!(next_receiver_name("RCV9"), "RCV0");
        assert_eq!(next_receiver_name("MYRCV"), "MYRCV0001");
    }

    #[test]
    fn record_image_fields_as_values() {
        // Storage formats (ILE RPG Reference, "Data types and data
        // formats"): packed 12345- in P(5,2) is X'12345D'; zoned 0042 in
        // S(4,0) is X'F0F0F4F2'; binary B(4,0) -7 is X'FFF9'; an ISO date
        // is its 10 characters.
        let (v, n) = field_value("P", 5, 2, false, false, &[0x12, 0x34, 0x5D]).unwrap();
        assert_eq!((v.as_str().unwrap(), n), ("-123.45", 3));
        let (v, n) = field_value("S", 4, 0, false, false, &[0xF0, 0xF0, 0xF4, 0xF2]).unwrap();
        assert_eq!((v.as_str().unwrap(), n), ("0042", 4));
        let (v, n) = field_value("B", 4, 0, false, false, &[0xFF, 0xF9]).unwrap();
        assert_eq!((v.as_str().unwrap(), n), ("-7", 2));
        let d = crate::tn5250::ebcdic::encode("2026-10-21");
        let (v, _) = field_value("L", 10, 0, false, false, &d).unwrap();
        assert_eq!(v.as_str().unwrap(), "2026-10-21");
        let z = crate::tn5250::ebcdic::encode("2026-10-21-13.45.10.000001");
        let (v, _) = field_value("Z", 26, 0, false, false, &z).unwrap();
        assert_eq!(v.as_str().unwrap(), "2026-10-21 13:45:10.000001");
        let a = [&[0u8, 2][..], &crate::tn5250::ebcdic::encode("OKxx")].concat();
        let (v, n) = field_value("A", 4, 0, true, false, &a).unwrap();
        assert_eq!((v.as_str().unwrap(), n), ("OK", 6));
    }
}
