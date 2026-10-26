//! Db2 for i scalar functions PostgreSQL has under other rules: CHAR of
//! numbers, dates and times, DIGITS, TIMESTAMP of a Db2 timestamp string,
//! VARCHAR_FORMAT. `sql.rs` rewrites the calls to these
//! `as4tainr_svc` functions; they are made with the services
//! (`services::init`).

/// The functions, as SQL for the services batch.
pub fn sql() -> String {
    format!("{FUNCTIONS}\n{MORE}\n{}\n{}", grants(), implicit_casts())
}

/// Implicit casting between strings and numbers (Db2 for i SQL Reference,
/// "Casting between data types": a character string compared with or
/// used in arithmetic with a number is cast to a number; as DECFLOAT
/// there, NUMERIC here). PostgreSQL has no such casts, so these
/// operators and MOD functions for (text, numeric) and (numeric, text)
/// stand in, in the schema `as4tainr_cast` that ends every SQL search
/// path (`sql::search_path`). CHAR and VARCHAR values reach them through
/// PostgreSQL's own implicit casts to text, integers through theirs to
/// numeric; a string that is not a number fails as a cast does.
fn implicit_casts() -> String {
    let role = crate::sql::SQL_ROLE;
    let mut out = format!(
        "CREATE SCHEMA IF NOT EXISTS {s};\nGRANT USAGE ON SCHEMA {s} TO {role};\n",
        s = crate::sql::CAST_SCHEMA
    );
    let s = crate::sql::CAST_SCHEMA;
    let ops: [(&str, &str, &str, &str); 10] = [
        ("=", "eq", "boolean", "="),
        ("<>", "ne", "boolean", "<>"),
        ("<", "lt", "boolean", "<"),
        (">", "gt", "boolean", ">"),
        ("<=", "le", "boolean", "<="),
        (">=", "ge", "boolean", ">="),
        ("+", "add", "numeric", "+"),
        ("-", "sub", "numeric", "-"),
        ("*", "mul", "numeric", "*"),
        ("/", "div", "numeric", "/"),
    ];
    for (left, right, tag) in [("text", "numeric", "tn"), ("numeric", "text", "nt")] {
        let (a, b) = if left == "text" {
            ("CAST($1 AS numeric)", "$2")
        } else {
            ("$1", "CAST($2 AS numeric)")
        };
        for (op, name, ret, pgop) in ops {
            out.push_str(&format!(
                "CREATE OR REPLACE FUNCTION {s}.{name}_{tag}({left}, {right}) RETURNS {ret}
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT {a} {pgop} {b} $as4tainr$;
DO $as4tainr$ BEGIN
  CREATE OPERATOR {s}.{op} (LEFTARG = {left}, RIGHTARG = {right}, FUNCTION = {s}.{name}_{tag});
EXCEPTION WHEN duplicate_function OR duplicate_object THEN NULL;
END $as4tainr$;
"
            ));
        }
        out.push_str(&format!(
            "CREATE OR REPLACE FUNCTION {s}.mod({left}, {right}) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT mod({a}, {b}) $as4tainr$;
"
        ));
    }
    out
}

fn grants() -> String {
    let role = crate::sql::SQL_ROLE;
    let sigs = [
        "job_name()",
        "source_lines(text, text, text)",
        "char(numeric)",
        "char(smallint)",
        "char(integer)",
        "char(bigint)",
        "char(double precision)",
        "char(real)",
        "char(date)",
        "char(date, text)",
        "char(time)",
        "char(time, text)",
        "char(timestamp)",
        "char(timestamp, text)",
        "char(text)",
        "char(text, integer)",
        "digits(numeric)",
        "digits(numeric, integer)",
        "digits(smallint)",
        "digits(integer)",
        "digits(bigint)",
        "timestamp(text)",
        "timestamp(timestamp)",
        "timestamp(date)",
        "timestamp(date, time)",
        "timestamp(text, text)",
        "varchar_format(timestamp, text)",
        "varchar_format(date, text)",
        "ddiv(numeric, numeric, integer)",
        "round(numeric, integer)",
        "truncate(numeric, integer)",
        "locate(text, text)",
        "locate(text, text, integer)",
        "locate_in_string(text, text)",
        "locate_in_string(text, text, integer)",
        "locate_in_string(text, text, integer, integer)",
        "space(integer)",
        "translate(text)",
        "translate(text, text, text)",
        "translate(text, text, text, text)",
        "strip(text)",
        "strip(text, text)",
        "strip(text, text, text)",
        "insert(text, integer, integer, text)",
        "midnight_seconds(time)",
        "midnight_seconds(timestamp)",
        "midnight_seconds(text)",
        "week(date)",
        "week(text)",
        "week(timestamp)",
        "week_iso(date)",
        "week_iso(text)",
        "week_iso(timestamp)",
        "dayofweek_iso(date)",
        "dayofweek_iso(text)",
        "dayofweek_iso(timestamp)",
        "monthname(date)",
        "monthname(text)",
        "monthname(timestamp)",
        "dayname(date)",
        "dayname(text)",
        "dayname(timestamp)",
        "julian_day(date)",
        "julian_day(text)",
        "julian_day(timestamp)",
        "rand()",
        "rand(integer)",
        "date(date)",
        "date(timestamp)",
        "date(timestamptz)",
        "date(text)",
        "date(integer)",
        "date(numeric)",
        "varchar(text)",
        "varchar(text, integer)",
        "varchar(numeric)",
        "varchar(smallint)",
        "varchar(integer)",
        "varchar(bigint)",
        "varchar(double precision)",
        "varchar(date)",
        "varchar(time)",
        "varchar(timestamp)",
        "add_months(date, integer)",
        "add_months(timestamp, integer)",
        "add_months(text, integer)",
        "last_day(date)",
        "last_day(timestamp)",
        "last_day(text)",
        "next_day(timestamp, text)",
        "next_day(date, text)",
        "next_day(text, text)",
        "months_between(timestamp, timestamp)",
        "months_between(text, text)",
        "timestamp_format(text, text)",
        "to_date(text, text)",
        "generate_unique()",
        "hex(smallint)",
        "hex(integer)",
        "hex(bigint)",
        "hex(double precision)",
        "hex(real)",
        "hex(numeric)",
        "char(interval)",
        "timestampdiff(integer, text)",
        "bad_date(text)",
        "max(VARIADIC anycompatiblearray)",
        "min(VARIADIC anycompatiblearray)",
    ];
    sigs.iter()
        .map(|s| format!("GRANT EXECUTE ON FUNCTION as4tainr_svc.{s} TO {role};\n"))
        .collect()
}

const FUNCTIONS: &str = r#"
-- CHAR(decimal) (Db2 for i SQL Reference, CHAR): a minus sign for a
-- negative value, no leading zeros, the decimal point only when the scale
-- is greater than zero, trailing zeros kept.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v numeric) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE
    WHEN v IS NULL THEN NULL
    ELSE (CASE WHEN v < 0 THEN '-' ELSE '' END)
      || regexp_replace(CAST(abs(v) AS text), '^0+(?=[0-9])|^0+(?=\.)', '')
  END $as4tainr$;
-- CHAR(integer): the digits, a minus sign for a negative value.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v smallint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(v AS text) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(v AS text) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(v AS text) $as4tainr$;
-- CHAR(floating point): the value to 17 significant digits, one before the
-- decimal point, then E and the exponent ('-9.8765432100000002E-33' in the
-- Db2 for i SQL Reference, CHAR); zero is 0E0.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v double precision) RETURNS text
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  s text;
BEGIN
  IF v IS NULL THEN RETURN NULL; END IF;
  IF v = 0 THEN RETURN '0E0'; END IF;
  s := trim(to_char(abs(v), '9.9999999999999999EEEE'));
  RETURN (CASE WHEN v < 0 THEN '-' ELSE '' END) || split_part(s, 'e', 1)
    || 'E' || CAST(CAST(split_part(s, 'e', 2) AS integer) AS text);
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v real) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.char(CAST(v AS double precision)) $as4tainr$;
-- CHAR(date, ISO | USA | EUR | JIS): the formats of the Db2 for i SQL
-- Reference (Datetime values: ISO yyyy-mm-dd, USA mm/dd/yyyy, EUR
-- dd.mm.yyyy, JIS yyyy-mm-dd).
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v date, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT to_char(v, CASE upper(f)
    WHEN 'USA' THEN 'MM/DD/YYYY'
    WHEN 'EUR' THEN 'DD.MM.YYYY'
    ELSE 'YYYY-MM-DD' END) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v date) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.char(v, 'ISO') $as4tainr$;
-- CHAR(time, ISO | USA | EUR | JIS): ISO and EUR hh.mm.ss, USA hh:mm AM,
-- JIS hh:mm:ss.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v time, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT to_char(CAST('2000-01-01' AS date) + v, CASE upper(f)
    WHEN 'USA' THEN 'HH12:MI AM'
    WHEN 'JIS' THEN 'HH24:MI:SS'
    ELSE 'HH24.MI.SS' END) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v time) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.char(v, 'ISO') $as4tainr$;
-- CHAR(date + labeled duration, format): PostgreSQL's date plus interval
-- is a timestamp; with a date format it is the date.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v timestamp, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.char(CAST(v AS date), f) $as4tainr$;
-- CHAR(timestamp): yyyy-mm-dd-hh.mm.ss.nnnnnn.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v timestamp) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT to_char(v, 'YYYY-MM-DD-HH24.MI.SS.US') $as4tainr$;
-- CHAR(string [, length]): the string, cut or padded to the length.
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT v $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v text, n integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT rpad(v, n) $as4tainr$;
-- Decimal division with Db2's scale (31 - p + s - s', worked out by
-- sql.rs): the quotient to that many decimals, the rest cut off.
CREATE OR REPLACE FUNCTION as4tainr_svc.ddiv(a numeric, b numeric, s integer) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT trunc((a + CAST(0 AS numeric(100, 80))) / b, s) $as4tainr$;
-- ROUND and TRUNCATE of a decimal keep its scale (ROUND(2.675, 2) is
-- 2.680): the sum with 0 times the argument has the argument's scale.
CREATE OR REPLACE FUNCTION as4tainr_svc.round(a numeric, n integer) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT round(a, n) + 0 * a $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.truncate(a numeric, n integer) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT trunc(a, n) + 0 * a $as4tainr$;
-- DIGITS (Db2 for i SQL Reference): the digits of the absolute value,
-- without sign or decimal point, with leading zeros to the precision
-- (SMALLINT 5, INTEGER 10, BIGINT 19 digits).
CREATE OR REPLACE FUNCTION as4tainr_svc.digits(v numeric, p integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT lpad(replace(CAST(abs(v) AS text), '.', ''), p, '0') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.digits(v numeric) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT replace(CAST(abs(v) AS text), '.', '') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.digits(v smallint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT lpad(CAST(abs(CAST(v AS integer)) AS text), 5, '0') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.digits(v integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT lpad(CAST(abs(CAST(v AS bigint)) AS text), 10, '0') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.digits(v bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT lpad(CAST(abs(CAST(v AS numeric)) AS text), 19, '0') $as4tainr$;
-- TIMESTAMP (Db2 for i SQL Reference): of a string in the Db2 form
-- yyyy-mm-dd-hh.mm.ss[.nnnnnn] (or any form PostgreSQL reads), of a date
-- (midnight), of a date and a time.
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp(v text) RETURNS timestamp
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  t text := trim(v);
BEGIN
  IF t IS NULL THEN RETURN NULL; END IF;
  IF t ~ '^\d{4}-\d{2}-\d{2}-\d{2}\.\d{2}\.\d{2}(\.\d{1,12})?$' THEN
    t := substr(t, 1, 10) || ' ' || replace(substr(t, 12, 8), '.', ':') || substr(t, 20);
  END IF;
  RETURN CAST(t AS timestamp);
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp(v timestamp) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT v $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp(v date) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(v AS timestamp) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp(d date, t time) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT d + t $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp(d text, t text) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(d AS date) + CAST(replace(t, '.', ':') AS time) $as4tainr$;
-- VARCHAR_FORMAT(timestamp, format) (Db2 for i SQL Reference): the format
-- elements YYYY, MM, DD, HH24, HH12, HH, MI, SS, FF (fractions) and AM/PM;
-- the ones PostgreSQL's to_char spells the same way are passed on.
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar_format(v timestamp, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT to_char(v, regexp_replace(regexp_replace(f, 'FF[1-6]?', 'US', 'g'), 'HH(?!12|24)', 'HH12', 'g')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar_format(v date, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.varchar_format(CAST(v AS timestamp), f) $as4tainr$;
"#;

// Functions found missing by the company_system corpus (docs/import.md).
// The rules and examples are those of the Db2 for i SQL Reference, scalar
// functions (IBM i 7.5).
const MORE: &str = r#"
-- A source member's records, for an alias or an override naming it
-- (SRCSEQ numbers the lines 1.00, 2.00..., SRCDAT 0, SRCDTA the line).
CREATE OR REPLACE FUNCTION as4tainr_svc.source_lines(lib text, fil text, mbr text)
RETURNS TABLE (srcseq numeric(6,2), srcdat numeric(6,0), srcdta varchar(32754))
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  RETURN QUERY
  SELECT CAST(l.n AS numeric(6,2)), CAST(0 AS numeric(6,0)), CAST(l.t AS varchar(32754))
    FROM as4tainr.source_members m,
         LATERAL regexp_split_to_table(m.source, E'\n') WITH ORDINALITY AS l(t, n)
   WHERE m.library = lib AND m.file = fil AND m.member = mbr AND m.source <> '';
END $as4tainr$;
-- QSYS2.JOB_NAME (Db2 for i SQL Reference, built-in global variables):
-- the job's qualified name, number/user/name.
CREATE OR REPLACE FUNCTION as4tainr_svc.job_name() RETURNS varchar(28)
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(COALESCE(NULLIF(current_setting('as4tainr.job', true), ''),
                       NULLIF(current_setting('application_name'), '')) AS varchar(28)) $as4tainr$;
-- DATE: of a date or timestamp; of a number n, the date n - 1 days after
-- January 1, 0001; of a string, the date it represents (ISO, USA, EUR,
-- JIS, a timestamp, or yyyynnn, a year and a day of the year).
CREATE OR REPLACE FUNCTION as4tainr_svc.date(d date) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT d $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.date(d timestamp) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(d AS date) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.date(d timestamptz) RETURNS date
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(d AS date) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.bad_date(v text) RETURNS integer
LANGUAGE plpgsql VOLATILE SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  RAISE EXCEPTION 'date value % out of range', v USING ERRCODE = '22008';
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.date(n integer) RETURNS date
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  IF n < 1 OR n > 3652059 THEN PERFORM as4tainr_svc.bad_date(CAST(n AS text)); END IF;
  RETURN DATE '0001-01-01' + (n - 1);
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.date(n numeric) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.date(CAST(trunc(n) AS integer)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.date(v text) RETURNS date
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  t text := trim(v);
BEGIN
  IF t IS NULL THEN RETURN NULL; END IF;
  IF t ~ '^\d{7}$' THEN
    IF substr(t, 5)::int < 1 OR substr(t, 5)::int > 366 THEN
      PERFORM as4tainr_svc.bad_date(t);
    END IF;
    RETURN make_date(substr(t, 1, 4)::int, 1, 1) + (substr(t, 5)::int - 1);
  ELSIF t ~ '^\d{1,2}/\d{1,2}/\d{4}$' THEN
    RETURN make_date(split_part(t, '/', 3)::int, split_part(t, '/', 1)::int, split_part(t, '/', 2)::int);
  ELSIF t ~ '^\d{1,2}\.\d{1,2}\.\d{4}$' THEN
    RETURN make_date(split_part(t, '.', 3)::int, split_part(t, '.', 2)::int, split_part(t, '.', 1)::int);
  ELSIF length(t) > 10 THEN
    RETURN CAST(as4tainr_svc.timestamp(t) AS date);
  END IF;
  RETURN CAST(t AS date);
END $as4tainr$;
-- LOCATE(search, source [, start]): the position of the first occurrence
-- of search at or after start, 0 when there is none ("LOCATE('N',
-- 'DINING')" is 3).
CREATE OR REPLACE FUNCTION as4tainr_svc.locate(f text, s text, b integer) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN b < 1 OR b > length(s) + 1 THEN 0
    WHEN strpos(substr(s, b), f) = 0 THEN 0
    ELSE strpos(substr(s, b), f) + b - 1 END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.locate(f text, s text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.locate(f, s, 1) $as4tainr$;
-- LOCATE_IN_STRING(source, search [, start [, instance]]): the position of
-- the instance-th occurrence; a negative start searches backward from that
-- many characters before the end.
CREATE OR REPLACE FUNCTION as4tainr_svc.locate_in_string(s text, f text, b integer, n integer) RETURNS integer
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  pos integer;
  found integer := 0;
  lf integer := length(f);
BEGIN
  IF s IS NULL OR f IS NULL OR b IS NULL OR n IS NULL THEN RETURN NULL; END IF;
  IF n < 1 OR b = 0 THEN
    RAISE EXCEPTION 'SQL0138: the start or instance argument of LOCATE_IN_STRING is not valid' USING ERRCODE = '22011';
  END IF;
  IF b > 0 THEN
    pos := b;
    WHILE pos + lf - 1 <= length(s) LOOP
      IF substr(s, pos, lf) = f THEN
        found := found + 1;
        IF found = n THEN RETURN pos; END IF;
      END IF;
      pos := pos + 1;
    END LOOP;
  ELSE
    pos := length(s) + b + 1;
    IF pos + lf - 1 > length(s) THEN pos := length(s) - lf + 1; END IF;
    WHILE pos >= 1 LOOP
      IF substr(s, pos, lf) = f THEN
        found := found + 1;
        IF found = n THEN RETURN pos; END IF;
      END IF;
      pos := pos - 1;
    END LOOP;
  END IF;
  RETURN 0;
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.locate_in_string(s text, f text, b integer) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.locate_in_string(s, f, b, 1) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.locate_in_string(s text, f text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.locate_in_string(s, f, 1, 1) $as4tainr$;
-- SPACE(n): n blanks.
CREATE OR REPLACE FUNCTION as4tainr_svc.space(n integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT repeat(' ', n) $as4tainr$;
-- TRANSLATE(s): upper case. TRANSLATE(s, to, from [, pad]): each character
-- of from is replaced by the character in the same position of to, to
-- padded with pad (a blank by default) to the length of from; the first
-- occurrence of a character in from counts.
CREATE OR REPLACE FUNCTION as4tainr_svc.translate(s text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(s) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.translate(s text, t text, f text, p text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT translate(s, f, rpad(t, length(f), substr(p, 1, 1))) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.translate(s text, t text, f text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.translate(s, t, f, ' ') $as4tainr$;
-- STRIP(s [, B | L | T [, character]]): blanks (or the character) taken
-- off both ends, the start or the end (sql.rs passes the keyword as 'B',
-- 'L' or 'T').
CREATE OR REPLACE FUNCTION as4tainr_svc.strip(s text, m text, c text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE upper(substr(m, 1, 1))
    WHEN 'L' THEN ltrim(s, c)
    WHEN 'T' THEN rtrim(s, c)
    ELSE btrim(s, c) END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.strip(s text, m text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.strip(s, m, ' ') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.strip(s text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.strip(s, 'B', ' ') $as4tainr$;
-- INSERT(source, start, length, insert): length characters from start
-- deleted, insert put there.
CREATE OR REPLACE FUNCTION as4tainr_svc.insert(s text, b integer, n integer, i text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT overlay(s PLACING i FROM b FOR n) $as4tainr$;
-- MIDNIGHT_SECONDS: the seconds since midnight, 0 to 86400.
CREATE OR REPLACE FUNCTION as4tainr_svc.midnight_seconds(t time) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(floor(extract(epoch FROM t)) AS integer) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.midnight_seconds(t timestamp) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.midnight_seconds(CAST(t AS time)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.midnight_seconds(t text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN length(trim(t)) > 8
    THEN as4tainr_svc.midnight_seconds(as4tainr_svc.timestamp(t))
    ELSE as4tainr_svc.midnight_seconds(CAST(replace(trim(t), '.', ':') AS time)) END $as4tainr$;
-- WEEK: the week of the year, 1 to 54; weeks start on Sunday and January 1
-- is in week 1. WEEK_ISO: ISO 8601 weeks (Monday, the week with the year's
-- first Thursday is week 1). DAYOFWEEK_ISO: 1 for Monday to 7 for Sunday.
CREATE OR REPLACE FUNCTION as4tainr_svc.week(d date) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST((extract(doy FROM d) - 1
    + extract(dow FROM CAST(date_trunc('year', d) AS date))) AS integer) / 7 + 1 $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.week(d text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.week(as4tainr_svc.date(d)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.week_iso(d date) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(extract(week FROM d) AS integer) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.week_iso(d text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.week_iso(as4tainr_svc.date(d)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayofweek_iso(d date) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(extract(isodow FROM d) AS integer) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayofweek_iso(d text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.dayofweek_iso(as4tainr_svc.date(d)) $as4tainr$;
-- MONTHNAME and DAYNAME: the English names, first letter upper case.
CREATE OR REPLACE FUNCTION as4tainr_svc.monthname(d date) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT to_char(d, 'FMMonth') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.monthname(d text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.monthname(as4tainr_svc.date(d)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayname(d date) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT to_char(d, 'FMDay') $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayname(d text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.dayname(as4tainr_svc.date(d)) $as4tainr$;
-- JULIAN_DAY: the days since January 1, 4713 B.C. ("JULIAN_DAY('1998-01-01')"
-- is 2450815).
CREATE OR REPLACE FUNCTION as4tainr_svc.julian_day(d date) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT CAST(to_char(d, 'J') AS integer) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.julian_day(d text) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.julian_day(as4tainr_svc.date(d)) $as4tainr$;
-- The same of a timestamp: of its date.
CREATE OR REPLACE FUNCTION as4tainr_svc.week(d timestamp) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.week(CAST(d AS date)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.week_iso(d timestamp) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.week_iso(CAST(d AS date)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayofweek_iso(d timestamp) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.dayofweek_iso(CAST(d AS date)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.monthname(d timestamp) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.monthname(CAST(d AS date)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.dayname(d timestamp) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.dayname(CAST(d AS date)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.julian_day(d timestamp) RETURNS integer
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.julian_day(CAST(d AS date)) $as4tainr$;
-- RAND([seed]): a floating point value from 0 to 1; a seed starts the
-- series again.
CREATE OR REPLACE FUNCTION as4tainr_svc.rand() RETURNS double precision
LANGUAGE sql VOLATILE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT random() $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.rand(seed integer) RETURNS double precision
LANGUAGE sql VOLATILE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT setseed(CAST(seed AS double precision) / 2147483648.0); SELECT random() $as4tainr$;
-- VARCHAR: a string cut to the length; numbers, dates, times and
-- timestamps as CHAR gives them, without padding.
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v text) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT v $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v text, n integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT left(v, n) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v numeric) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v smallint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v double precision) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v date) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v time) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.varchar(v timestamp) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.char(v) $as4tainr$;
-- ADD_MONTHS(d, n): n months later; when d is the last day of its month,
-- or the day does not exist in the result's month, the last day of the
-- result's month.
CREATE OR REPLACE FUNCTION as4tainr_svc.last_day(d date) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(date_trunc('month', d) + interval '1 month' - interval '1 day' AS date) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.last_day(d timestamp) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.last_day(CAST(d AS date)) + CAST(d AS time) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.last_day(d text) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.last_day(as4tainr_svc.date(d)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.add_months(d date, n integer) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN d = as4tainr_svc.last_day(d)
    THEN as4tainr_svc.last_day(CAST(d + make_interval(months => n) AS date))
    ELSE CAST(d + make_interval(months => n) AS date) END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.add_months(d timestamp, n integer) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.add_months(CAST(d AS date), n) + CAST(d AS time) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.add_months(d text, n integer) RETURNS date
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.add_months(as4tainr_svc.date(d), n) $as4tainr$;
-- NEXT_DAY(d, day): the timestamp of the first such day of the week after
-- d (MON, TUE... or the full English name), at d's time.
CREATE OR REPLACE FUNCTION as4tainr_svc.next_day(d timestamp, w text) RETURNS timestamp
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  k integer := array_position(ARRAY['SUN','MON','TUE','WED','THU','FRI','SAT'], upper(substr(trim(w), 1, 3))) - 1;
  have integer := extract(dow FROM d);
BEGIN
  IF d IS NULL OR w IS NULL THEN RETURN NULL; END IF;
  IF k IS NULL OR (length(trim(w)) > 3 AND upper(trim(w)) <> upper(to_char(DATE '2023-01-01' + k, 'FMDay'))) THEN
    RAISE EXCEPTION 'SQL0171: % is not a day of the week', w USING ERRCODE = '22546';
  END IF;
  RETURN d + make_interval(days => CASE WHEN k > have THEN k - have ELSE k - have + 7 END);
END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.next_day(d date, w text) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.next_day(CAST(d AS timestamp), w) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.next_day(d text, w text) RETURNS timestamp
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.next_day(as4tainr_svc.timestamp(d), w) $as4tainr$;
-- MONTHS_BETWEEN(a, b): whole months when the days of the month are the
-- same or both are the last day of their month; otherwise the months plus
-- the difference in days (and time) over 31, DECIMAL(31,15)
-- ("MONTHS_BETWEEN('2005-02-02', '2005-01-01')" is 1.032258064516129).
CREATE OR REPLACE FUNCTION as4tainr_svc.months_between(a timestamp, b timestamp) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(trunc(CASE
    WHEN extract(day FROM a) = extract(day FROM b)
      OR (CAST(a AS date) = as4tainr_svc.last_day(CAST(a AS date))
          AND CAST(b AS date) = as4tainr_svc.last_day(CAST(b AS date)))
    THEN CAST((extract(year FROM a) - extract(year FROM b)) * 12
      + extract(month FROM a) - extract(month FROM b) AS numeric)
    ELSE CAST((extract(year FROM a) - extract(year FROM b)) * 12
      + extract(month FROM a) - extract(month FROM b) AS numeric)
      + (CAST(extract(day FROM a) - extract(day FROM b) AS numeric)
         + CAST(extract(epoch FROM CAST(a AS time)) - extract(epoch FROM CAST(b AS time)) AS numeric) / 86400)
        / 31
    END, 15) AS numeric(31,15)) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.months_between(a text, b text) RETURNS numeric
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.months_between(as4tainr_svc.timestamp(a), as4tainr_svc.timestamp(b)) $as4tainr$;
-- TIMESTAMP_FORMAT(string, format) and its synonym TO_DATE: the elements
-- YYYY, MM, DD, HH24, HH12 (HH), MI, SS, NNNNNN and FF1-FF12 (fractions).
CREATE OR REPLACE FUNCTION as4tainr_svc.timestamp_format(v text, f text) RETURNS timestamp
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CAST(to_timestamp(v, regexp_replace(regexp_replace(f, 'FF([7-9]|1[0-2])|NNNNNN', 'US', 'g'),
    'HH(?!12|24)', 'HH12', 'g')) AS timestamp) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.to_date(v text, f text) RETURNS timestamp
LANGUAGE sql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT as4tainr_svc.timestamp_format(v, f) $as4tainr$;
-- GENERATE_UNIQUE(): 13 bytes, the current time (microseconds) and a
-- random part, unique and growing in a job.
CREATE OR REPLACE FUNCTION as4tainr_svc.generate_unique() RETURNS bytea
LANGUAGE sql VOLATILE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT decode(lpad(to_hex(CAST(extract(epoch FROM clock_timestamp()) * 1000000 AS bigint)), 16, '0')
    || lpad(to_hex(CAST(floor(random() * 1099511627776) AS bigint)), 10, '0'), 'hex') $as4tainr$;
-- HEX of numbers: the internal form on IBM i, big-endian binary integers,
-- IEEE floating point, packed decimal with sign F or D... (C for
-- positive, D for negative).
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v smallint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(int2send(v), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v integer) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(int4send(v), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v bigint) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(int8send(v), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v double precision) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(float8send(v), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v real) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$ SELECT upper(encode(float4send(v), 'hex')) $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.hex(v numeric) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN length(d) % 2 = 0 THEN '0' || d ELSE d END
    || CASE WHEN v < 0 THEN 'D' ELSE 'C' END
  FROM (SELECT replace(CAST(abs(v) AS text), '.', '') AS d) x $as4tainr$;
-- CHAR of a timestamp duration (a timestamp minus a timestamp):
-- yyyymmddhhmmss.nnnnnn without leading zeros, the days of PostgreSQL's
-- interval counted as months of 30 days (docs/fidelity-gaps.md).
CREATE OR REPLACE FUNCTION as4tainr_svc.char(v interval) RETURNS text
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT as4tainr_svc.char(CAST(
    (CASE WHEN v < interval '0' THEN -1 ELSE 1 END) * (
      CAST(extract(year FROM j) AS numeric) * 10000000000
      + CAST(extract(month FROM j) AS numeric) * 100000000
      + CAST(extract(day FROM j) AS numeric) * 1000000
      + CAST(extract(hour FROM j) AS numeric) * 10000
      + CAST(extract(minute FROM j) AS numeric) * 100
      + CAST(extract(microseconds FROM j) AS numeric(20,6)) / 1000000) AS numeric(20,6)))
  FROM (SELECT justify_interval(CASE WHEN v < interval '0' THEN -v ELSE v END) AS j) x $as4tainr$;
-- TIMESTAMPDIFF(unit, duration): the duration (CHAR of a timestamp
-- duration) in microseconds (1), seconds (2), minutes (4), hours (8), days
-- (16), weeks (32), months (64), quarters (128) or years (256), estimated
-- with 365 days a year and 30 days a month, the rest cut off.
CREATE OR REPLACE FUNCTION as4tainr_svc.timestampdiff(u integer, d text) RETURNS integer
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  n numeric := CAST(trim(d) AS numeric(22,6));
  sg integer := CASE WHEN n < 0 THEN -1 ELSE 1 END;
  t text := to_char(abs(n), 'FM00000000000000.000000');
  y numeric := substr(t, 1, 4)::numeric;
  mo numeric := substr(t, 5, 2)::numeric;
  dd numeric := substr(t, 7, 2)::numeric;
  h numeric := substr(t, 9, 2)::numeric;
  mi numeric := substr(t, 11, 2)::numeric;
  s numeric := substr(t, 13, 2)::numeric;
  us numeric := substr(t, 16, 6)::numeric;
  days numeric := y * 365 + mo * 30 + dd;
  secs numeric := ((days * 24 + h) * 60 + mi) * 60 + s;
  r numeric;
BEGIN
  IF d IS NULL THEN RETURN NULL; END IF;
  r := CASE u
    WHEN 1 THEN secs * 1000000 + us
    WHEN 2 THEN secs
    WHEN 4 THEN trunc(secs / 60)
    WHEN 8 THEN trunc(secs / 3600)
    WHEN 16 THEN trunc(secs / 86400)
    WHEN 32 THEN trunc(secs / 604800)
    WHEN 64 THEN y * 12 + mo + trunc(dd / 30)
    WHEN 128 THEN trunc((y * 12 + mo + trunc(dd / 30)) / 3)
    WHEN 256 THEN y + trunc((mo + trunc(dd / 30)) / 12)
  END;
  IF r IS NULL THEN
    RAISE EXCEPTION 'SQL0171: % is not a TIMESTAMPDIFF interval', u USING ERRCODE = '22546';
  END IF;
  RETURN CAST(sg * r AS integer);
END $as4tainr$;
-- MAX and MIN with two or more arguments: the largest or smallest, null
-- when any argument is null.
CREATE OR REPLACE FUNCTION as4tainr_svc.max(VARIADIC v anycompatiblearray) RETURNS anycompatible
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN array_position(v, NULL) IS NOT NULL THEN NULL ELSE (SELECT max(x) FROM unnest(v) x) END $as4tainr$;
CREATE OR REPLACE FUNCTION as4tainr_svc.min(VARIADIC v anycompatiblearray) RETURNS anycompatible
LANGUAGE sql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
  SELECT CASE WHEN array_position(v, NULL) IS NOT NULL THEN NULL ELSE (SELECT min(x) FROM unnest(v) x) END $as4tainr$;
"#;
