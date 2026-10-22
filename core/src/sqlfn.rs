//! Db2 for i scalar functions PostgreSQL has under other rules: CHAR of
//! numbers, dates and times, DIGITS, TIMESTAMP of a Db2 timestamp string,
//! VARCHAR_FORMAT. `sql.rs` rewrites the calls to these
//! `as4tainr_svc` functions; they are made with the services
//! (`services::init`).

/// The functions, as SQL for the services batch.
pub fn sql() -> String {
    format!("{FUNCTIONS}\n{}", grants())
}

fn grants() -> String {
    let role = crate::sql::SQL_ROLE;
    let sigs = [
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
