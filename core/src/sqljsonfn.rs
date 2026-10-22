//! The SQL/JSON functions of Db2 for i (SQL Reference: JSON_OBJECT,
//! JSON_ARRAY, JSON_ARRAYAGG, JSON_OBJECTAGG, JSON_VALUE, JSON_QUERY,
//! JSON_EXISTS) on PostgreSQL. The constructors build PostgreSQL `json`
//! (which keeps the order of keys as written) and give it in Db2's form,
//! without blanks between tokens (`{"id":1,"name":"Ann"}`); a value is a
//! JSON string, number, boolean or null by its type, or the JSON it is
//! (FORMAT JSON, or another constructor's result). Defaults as Db2:
//! JSON_OBJECT and JSON_OBJECTAGG NULL ON NULL, JSON_ARRAY and
//! JSON_ARRAYAGG ABSENT ON NULL; JSON_VALUE, JSON_QUERY and JSON_EXISTS
//! NULL (FALSE) ON EMPTY and ON ERROR, the path language PostgreSQL's
//! SQL/JSON path (lax by default).

use crate::sql::Tok;
use anyhow::{bail, Result};

fn word(t: &Tok) -> Option<String> {
    match t {
        Tok::Word(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

fn is_punct(t: &Tok, p: &str) -> bool {
    matches!(t, Tok::Punct(x) if x == p)
}

/// How a token changes the parenthesis depth (rewritten calls are
/// punctuation with parentheses in them).
fn delta(t: &Tok) -> i32 {
    match t {
        Tok::Punct(x) => x.matches('(').count() as i32 - x.matches(')').count() as i32,
        _ => 0,
    }
}

fn p(s: &str) -> Tok {
    Tok::Punct(s.to_string())
}

const FUNCS: &[&str] = &[
    "JSON_OBJECT",
    "JSON_ARRAY",
    "JSON_ARRAYAGG",
    "JSON_OBJECTAGG",
    "JSON_VALUE",
    "JSON_QUERY",
    "JSON_EXISTS",
];

/// Replace the SQL/JSON function calls in a statement's tokens.
pub(crate) fn rewrite(toks: Vec<Tok>) -> Result<Vec<Tok>> {
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let name = word(&toks[i]).filter(|w| FUNCS.contains(&w.as_str()));
        let open = name
            .as_ref()
            .and_then(|_| (i + 1..toks.len()).find(|&j| !matches!(toks[j], Tok::Space(_))));
        let (Some(name), Some(open)) = (name, open) else {
            out.push(toks[i].clone());
            i += 1;
            continue;
        };
        if !is_punct(&toks[open], "(") {
            out.push(toks[i].clone());
            i += 1;
            continue;
        }
        let mut depth = 0;
        let mut close = None;
        for (j, t) in toks.iter().enumerate().skip(open) {
            if is_punct(t, "(") {
                depth += 1;
            } else if is_punct(t, ")") {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
        }
        let Some(close) = close else {
            bail!("SQL0104: {name} needs a closing parenthesis.");
        };
        // Nested calls first.
        let inner = rewrite(toks[open + 1..close].to_vec())?;
        out.extend(call(&name, inner)?);
        i = close + 1;
    }
    Ok(out)
}

/// The arguments at the top level, split at commas; blanks kept inside.
fn split_args(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut args = vec![Vec::new()];
    let mut depth = 0;
    for t in toks {
        if depth == 0 && is_punct(t, ",") {
            args.push(Vec::new());
            continue;
        }
        depth += delta(t);
        args.last_mut().unwrap().push(t.clone());
    }
    args
}

/// Words at the top level of an argument (not in parentheses), with
/// their positions.
fn top_words(toks: &[Tok]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0;
    for (k, t) in toks.iter().enumerate() {
        if depth == 0 {
            if let Some(w) = word(t) {
                out.push((k, w));
            }
        }
        depth += delta(t);
    }
    out
}

/// Trailing clauses of the last argument: [NULL | ABSENT] ON NULL,
/// RETURNING type, FORMAT JSON. Returns the argument before them and
/// (null on null, returning type tokens).
struct Tail {
    null_on_null: Option<bool>,
    returning: Option<Vec<Tok>>,
}

fn take_tail(arg: &mut Vec<Tok>) -> Tail {
    let mut tail = Tail {
        null_on_null: None,
        returning: None,
    };
    let words = top_words(arg);
    // RETURNING type ...: the rest.
    if let Some((k, _)) = words.iter().find(|(_, w)| w == "RETURNING") {
        let mut ret: Vec<Tok> = arg[k + 1..].to_vec();
        // FORMAT JSON after the type is the type's form.
        let rw = top_words(&ret);
        if let Some((f, _)) = rw.iter().find(|(_, w)| w == "FORMAT") {
            ret.truncate(*f);
        }
        tail.returning = Some(ret);
        arg.truncate(*k);
    }
    let words = top_words(arg);
    let n = words.len();
    if n >= 3 && words[n - 2].1 == "ON" && words[n - 1].1 == "NULL" {
        match words[n - 3].1.as_str() {
            "NULL" => tail.null_on_null = Some(true),
            "ABSENT" => tail.null_on_null = Some(false),
            _ => return tail,
        }
        arg.truncate(words[n - 3].0);
    }
    tail
}

/// A value as JSON: FORMAT JSON values are JSON text, others JSON by
/// their type.
fn json_value_of(arg: &[Tok]) -> Vec<Tok> {
    let words = top_words(arg);
    let n = words.len();
    let mut v = arg.to_vec();
    if n >= 2 && words[n - 2].1 == "FORMAT" && words[n - 1].1 == "JSON" {
        v.truncate(words[n - 2].0);
        let mut out = vec![p("CAST(CAST(")];
        out.extend(v);
        out.push(p(" AS text) AS json)"));
        return out;
    }
    // A constant without a type: a string, or NULL.
    let solid: Vec<&Tok> = v.iter().filter(|t| !matches!(t, Tok::Space(_))).collect();
    match solid.as_slice() {
        [Tok::Str(_)] => {
            let mut out = vec![p("to_json(CAST(")];
            out.extend(v);
            out.push(p(" AS text))"));
            return out;
        }
        [Tok::Word(w)] if w.eq_ignore_ascii_case("NULL") => return vec![p("CAST(NULL AS json)")],
        _ => {}
    }
    let mut out = vec![p("to_json(")];
    out.extend(v);
    out.push(p(")"));
    out
}

fn finish(mut body: Vec<Tok>, tail: &Tail) -> Vec<Tok> {
    let mut out = vec![p("as4tainr_svc.json_compact(")];
    out.append(&mut body);
    out.push(p(")"));
    if let Some(t) = &tail.returning {
        let mut r = vec![p("CAST(CAST(")];
        r.extend(out);
        r.push(p(" AS text) AS "));
        r.extend(t.iter().cloned());
        r.push(p(")"));
        return r;
    }
    out
}

fn call(name: &str, inner: Vec<Tok>) -> Result<Vec<Tok>> {
    let mut args = split_args(&inner);
    let empty = |a: &Vec<Tok>| a.iter().all(|t| matches!(t, Tok::Space(_)));
    if args.len() == 1 && empty(&args[0]) {
        args.clear();
    }
    match name {
        "JSON_OBJECT" => {
            let mut tail = Tail {
                null_on_null: None,
                returning: None,
            };
            if let Some(last) = args.last_mut() {
                tail = take_tail(last);
            }
            let mut body = vec![p("json_build_object(")];
            let mut first = true;
            for a in &args {
                if empty(a) {
                    continue;
                }
                let (k, v) = key_value(a, name)?;
                if !first {
                    body.push(p(", "));
                }
                first = false;
                body.push(p("CAST("));
                body.extend(k);
                body.push(p(" AS text), "));
                body.extend(json_value_of(&v));
            }
            body.push(p(")"));
            if tail.null_on_null == Some(false) {
                body.insert(0, p("as4tainr_svc.json_absent("));
                body.push(p(")"));
            }
            Ok(finish(body, &tail))
        }
        "JSON_ARRAY" => {
            let mut tail = Tail {
                null_on_null: None,
                returning: None,
            };
            if let Some(last) = args.last_mut() {
                tail = take_tail(last);
            }
            // JSON_ARRAY(fullselect): the values of its column.
            let first_word = args
                .first()
                .and_then(|a| a.iter().find(|t| !matches!(t, Tok::Space(_))))
                .and_then(word);
            if args.len() == 1
                && matches!(first_word.as_deref(), Some("SELECT" | "WITH" | "VALUES"))
            {
                let absent = tail.null_on_null != Some(true);
                let mut body = vec![p(
                    "(SELECT coalesce(json_agg(to_json(as4tainr_q.v) ORDER BY as4tainr_q.n), '[]'::json) FROM (SELECT as4tainr_q0.*, row_number() OVER () AS n FROM (",
                )];
                body.extend(args[0].clone());
                body.push(p(&format!(
                    ") AS as4tainr_q0) AS as4tainr_q(v){})",
                    if absent {
                        " WHERE as4tainr_q.v IS NOT NULL"
                    } else {
                        ""
                    }
                )));
                return Ok(finish(body, &tail));
            }
            let mut body = vec![p("json_build_array(")];
            let mut first = true;
            for a in &args {
                if empty(a) {
                    continue;
                }
                if !first {
                    body.push(p(", "));
                }
                first = false;
                body.extend(json_value_of(a));
            }
            body.push(p(")"));
            if tail.null_on_null != Some(true) {
                body.insert(0, p("as4tainr_svc.json_absent("));
                body.push(p(")"));
            }
            Ok(finish(body, &tail))
        }
        "JSON_ARRAYAGG" => {
            let [arg] = args.as_mut_slice() else {
                bail!("SQL0104: JSON_ARRAYAGG takes one value.");
            };
            let tail = take_tail(arg);
            // ORDER BY ... at the top level.
            let words = top_words(arg);
            let order = words
                .windows(2)
                .find(|w| w[0].1 == "ORDER" && w[1].1 == "BY")
                .map(|w| w[0].0);
            let (val, ord) = match order {
                Some(k) => (arg[..k].to_vec(), arg[k..].to_vec()),
                None => (arg.clone(), Vec::new()),
            };
            let absent = tail.null_on_null != Some(true);
            let mut body = vec![p("json_agg(")];
            body.extend(json_value_of(&val));
            body.push(p(" "));
            body.extend(ord);
            body.push(p(")"));
            if absent {
                let words = top_words(&val);
                let n = words.len();
                let mut plain = val.clone();
                if n >= 2 && words[n - 2].1 == "FORMAT" && words[n - 1].1 == "JSON" {
                    plain.truncate(words[n - 2].0);
                }
                body.push(p(" FILTER (WHERE ("));
                body.extend(plain);
                body.push(p(") IS NOT NULL)"));
            }
            Ok(finish(body, &tail))
        }
        "JSON_OBJECTAGG" => {
            let [arg] = args.as_mut_slice() else {
                bail!("SQL0104: JSON_OBJECTAGG takes one key and value.");
            };
            let tail = take_tail(arg);
            let (k, v) = key_value(arg, name)?;
            let mut body = vec![p("json_object_agg(CAST(")];
            body.extend(k);
            body.push(p(" AS text), "));
            body.extend(json_value_of(&v));
            body.push(p(")"));
            if tail.null_on_null == Some(false) {
                let mut plain = v.clone();
                let words = top_words(&v);
                let n = words.len();
                if n >= 2 && words[n - 2].1 == "FORMAT" && words[n - 1].1 == "JSON" {
                    plain.truncate(words[n - 2].0);
                }
                body.push(p(" FILTER (WHERE ("));
                body.extend(plain);
                body.push(p(") IS NOT NULL)"));
            }
            Ok(finish(body, &tail))
        }
        "JSON_VALUE" | "JSON_QUERY" | "JSON_EXISTS" => query_fn(name, args),
        _ => unreachable!(),
    }
}

/// `[KEY] k VALUE v` or `k : v`.
fn key_value(a: &[Tok], name: &str) -> Result<(Vec<Tok>, Vec<Tok>)> {
    let words = top_words(a);
    let mut start = 0;
    if let Some((k, w)) = words.first() {
        if w == "KEY" {
            start = k + 1;
        }
    }
    let sep = words
        .iter()
        .find(|(k, w)| *k >= start && w == "VALUE")
        .map(|(k, _)| (*k, *k + 1))
        .or_else(|| {
            let mut depth = 0;
            a.iter().enumerate().skip(start).find_map(|(k, t)| {
                if depth == 0 && is_punct(t, ":") {
                    return Some((k, k + 1));
                }
                depth += delta(t);
                None
            })
        });
    let Some((s, v)) = sep else {
        bail!("SQL0104: {name}: KEY name VALUE value is expected.");
    };
    Ok((a[start..s].to_vec(), a[v..].to_vec()))
}

/// JSON_VALUE, JSON_QUERY, JSON_EXISTS: (document, path, clauses).
fn query_fn(name: &str, args: Vec<Vec<Tok>>) -> Result<Vec<Tok>> {
    if args.len() < 2 {
        bail!("SQL0104: {name} needs a document and a path.");
    }
    let doc = args[0].clone();
    // The path, then the clauses (in the same argument).
    let mut rest = args[1].clone();
    let path_end = rest
        .iter()
        .position(|t| matches!(t, Tok::Str(_)))
        .map(|k| k + 1)
        .unwrap_or(rest.len());
    let clauses: Vec<Tok> = rest.split_off(path_end);
    let path = rest;
    let words: Vec<String> = top_words(&clauses).into_iter().map(|(_, w)| w).collect();
    let text = words.join(" ");
    let on = |what: &str| -> String {
        // NULL | ERROR | EMPTY ARRAY | EMPTY OBJECT | TRUE | FALSE |
        // UNKNOWN | DEFAULT x ON what
        let key = format!(" ON {what}");
        let Some(at) = text.find(&key) else {
            return String::new();
        };
        let before = &text[..at];
        let last: Vec<&str> = before.split_whitespace().collect();
        match last.as_slice() {
            [.., "EMPTY", "ARRAY"] => "EMPTY ARRAY".into(),
            [.., "EMPTY", "OBJECT"] => "EMPTY OBJECT".into(),
            [.., w] => w.to_string(),
            [] => String::new(),
        }
    };
    let on_empty = on("EMPTY");
    let on_error = on("ERROR");
    let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut out = Vec::new();
    match name {
        "JSON_EXISTS" => {
            out.push(p("as4tainr_svc.json_exists(CAST("));
            out.extend(doc);
            out.push(p(" AS text), "));
            out.extend(path);
            out.push(p(&format!(", {})", lit(&on_error))));
        }
        "JSON_VALUE" => {
            // RETURNING type: the value cast to it (VARCHAR(2000) when
            // not given).
            let ret: Vec<Tok> = match top_words(&clauses).iter().find(|(_, w)| w == "RETURNING") {
                Some((k, _)) => {
                    let after = &clauses[k + 1..];
                    let stop = top_words(after)
                        .iter()
                        .find(|(_, w)| matches!(w.as_str(), "NULL" | "ERROR" | "DEFAULT"))
                        .map(|(j, _)| *j)
                        .unwrap_or(after.len());
                    after[..stop].to_vec()
                }
                None => vec![p("varchar(2000)")],
            };
            out.push(p("CAST(as4tainr_svc.json_value(CAST("));
            out.extend(doc);
            out.push(p(" AS text), "));
            out.extend(path);
            out.push(p(&format!(", {}, {}) AS ", lit(&on_empty), lit(&on_error))));
            out.extend(ret);
            out.push(p(")"));
            // DEFAULT x ON EMPTY: the default for a value not there.
            if on_empty == "DEFAULT" || on_error == "DEFAULT" {
                let cw = top_words(&clauses);
                if let Some((k, _)) = cw.iter().find(|(_, w)| w == "DEFAULT") {
                    let after = &clauses[k + 1..];
                    let stop = top_words(after)
                        .iter()
                        .find(|(_, w)| w == "ON")
                        .map(|(j, _)| *j)
                        .unwrap_or(after.len());
                    let mut wrapped = vec![p("COALESCE(")];
                    wrapped.extend(out);
                    wrapped.push(p(", "));
                    wrapped.extend(after[..stop].iter().cloned());
                    wrapped.push(p(")"));
                    out = wrapped;
                }
            }
        }
        _ => {
            let wrapper = if text.contains("WITHOUT") {
                "NONE"
            } else if text.contains("CONDITIONAL") && !text.contains("UNCONDITIONAL") {
                "CONDITIONAL"
            } else if text.contains("WRAPPER") {
                "UNCONDITIONAL"
            } else {
                "NONE"
            };
            let omit = text.contains("OMIT QUOTES");
            out.push(p("as4tainr_svc.json_query(CAST("));
            out.extend(doc);
            out.push(p(" AS text), "));
            out.extend(path);
            out.push(p(&format!(
                ", {}, {}, {}, {})",
                lit(wrapper),
                if omit { "true" } else { "false" },
                lit(&on_empty),
                lit(&on_error)
            )));
        }
    }
    Ok(out)
}

/// The PostgreSQL functions the rewritten calls use.
pub const SQL: &str = r#"
-- JSON without blanks between tokens, keys in the order written.
CREATE OR REPLACE FUNCTION as4tainr_svc.json_compact(j json) RETURNS json
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  r text;
BEGIN
  IF j IS NULL THEN RETURN NULL; END IF;
  CASE json_typeof(j)
    WHEN 'object' THEN
      SELECT '{' || coalesce(string_agg(to_json(e.key)::text || ':' || as4tainr_svc.json_compact(e.value)::text,
                                        ',' ORDER BY e.n), '') || '}'
        INTO r FROM json_each(j) WITH ORDINALITY AS e(key, value, n);
    WHEN 'array' THEN
      SELECT '[' || coalesce(string_agg(as4tainr_svc.json_compact(e.value)::text, ',' ORDER BY e.n), '') || ']'
        INTO r FROM json_array_elements(j) WITH ORDINALITY AS e(value, n);
    ELSE
      r := btrim(j::text);
  END CASE;
  RETURN r::json;
END $as4tainr$;

-- ABSENT ON NULL: an object's members or an array's elements that are
-- null are left out (not those of the JSON inside them).
CREATE OR REPLACE FUNCTION as4tainr_svc.json_absent(j json) RETURNS json
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  r json;
BEGIN
  IF j IS NULL THEN RETURN NULL; END IF;
  IF json_typeof(j) = 'object' THEN
    SELECT coalesce(json_object_agg(e.key, e.value ORDER BY e.n), '{}'::json) INTO r
      FROM json_each(j) WITH ORDINALITY AS e(key, value, n) WHERE json_typeof(e.value) <> 'null';
  ELSIF json_typeof(j) = 'array' THEN
    SELECT coalesce(json_agg(e.value ORDER BY e.n), '[]'::json) INTO r
      FROM json_array_elements(j) WITH ORDINALITY AS e(value, n) WHERE json_typeof(e.value) <> 'null';
  ELSE
    r := j;
  END IF;
  RETURN r;
END $as4tainr$;

-- JSON_VALUE: an SQL/JSON scalar as text. on_empty and on_error: '' or
-- NULL (null), ERROR (an error), DEFAULT (null here; the call gives the
-- default).
CREATE OR REPLACE FUNCTION as4tainr_svc.json_value(doc text, path text, on_empty text, on_error text)
RETURNS text LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  v jsonb;
BEGIN
  IF doc IS NULL OR path IS NULL THEN RETURN NULL; END IF;
  BEGIN
    v := jsonb_path_query_first(doc::jsonb, path::jsonpath);
  EXCEPTION WHEN OTHERS THEN
    IF on_error = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22032', MESSAGE = 'SQL16410: JSON_VALUE: ' || SQLERRM;
    END IF;
    RETURN NULL;
  END;
  IF v IS NULL THEN
    IF on_empty = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22035', MESSAGE = 'SQL16410: JSON_VALUE: no value for path ' || path;
    END IF;
    RETURN NULL;
  END IF;
  IF jsonb_typeof(v) IN ('object', 'array') THEN
    IF on_error = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22036', MESSAGE = 'SQL16410: JSON_VALUE: the value for path ' || path || ' is not a scalar';
    END IF;
    RETURN NULL;
  END IF;
  IF jsonb_typeof(v) = 'null' THEN RETURN NULL; END IF;
  RETURN v #>> '{}';
END $as4tainr$;

-- The values a simple SQL/JSON path selects ($, .name, ."name", .*, [n],
-- [*], lax or strict), from json so that objects keep their keys' order;
-- NULL (not a set) for paths with more than these (filters, ranges,
-- last, methods): the caller then uses jsonb_path_query.
CREATE OR REPLACE FUNCTION as4tainr_svc.json_path_simple(doc json, path text) RETURNS json[]
LANGUAGE plpgsql IMMUTABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  p text := btrim(path);
  cur json[];
  nxt json[];
  v json;
  m text[];
  lax boolean := true;
  e json;
BEGIN
  IF lower(p) LIKE 'strict %' THEN lax := false; p := btrim(substr(p, 7));
  ELSIF lower(p) LIKE 'lax %' THEN p := btrim(substr(p, 4)); END IF;
  IF left(p, 1) <> '$' THEN RETURN NULL; END IF;
  p := substr(p, 2);
  cur := ARRAY[doc];
  WHILE p <> '' LOOP
    nxt := '{}';
    IF p ~ '^\.[A-Za-z_][A-Za-z0-9_]*' THEN
      m := regexp_match(p, '^\.([A-Za-z_][A-Za-z0-9_]*)');
      p := substr(p, length(m[1]) + 2);
    ELSIF p ~ '^\."[^"]*"' THEN
      m := regexp_match(p, '^\."([^"]*)"');
      p := substr(p, length(m[1]) + 4);
    ELSIF p ~ '^\.\*' THEN
      m := ARRAY['*'::text, 'members'];
      p := substr(p, 3);
    ELSIF p ~ '^\[\s*\*\s*\]' THEN
      m := ARRAY['*'::text, 'elements'];
      p := regexp_replace(p, '^\[\s*\*\s*\]', '');
    ELSIF p ~ '^\[\s*[0-9]+\s*\]' THEN
      m := ARRAY[(regexp_match(p, '^\[\s*([0-9]+)\s*\]'))[1], 'index'];
      p := regexp_replace(p, '^\[\s*[0-9]+\s*\]', '');
    ELSE
      RETURN NULL;
    END IF;
    FOREACH v IN ARRAY cur LOOP
      IF m[2] = 'index' OR m[2] = 'elements' THEN
        IF json_typeof(v) = 'array' THEN
          IF m[2] = 'elements' THEN
            FOR e IN SELECT x FROM json_array_elements(v) AS x LOOP nxt := nxt || e; END LOOP;
          ELSIF (m[1])::int < json_array_length(v) THEN
            nxt := nxt || (v -> (m[1])::int);
          ELSIF NOT lax THEN RETURN NULL;
          END IF;
        ELSIF lax AND (m[2] = 'elements' OR m[1] = '0') THEN
          nxt := nxt || v;
        ELSIF NOT lax THEN RETURN NULL;
        END IF;
      ELSE
        -- Members: of an object; lax, of each object in an array.
        FOR e IN SELECT x FROM (
            SELECT v AS x WHERE json_typeof(v) = 'object'
            UNION ALL
            SELECT a FROM json_array_elements(CASE WHEN lax AND json_typeof(v) = 'array' THEN v ELSE '[]'::json END) AS a
             WHERE json_typeof(a) = 'object') AS objs LOOP
          IF m[2] = 'members' THEN
            nxt := nxt || ARRAY(SELECT x.value FROM json_each(e) AS x);
          ELSIF json_typeof(e -> m[1]) IS NOT NULL THEN
            nxt := nxt || (e -> m[1]);
          END IF;
        END LOOP;
      END IF;
    END LOOP;
    cur := nxt;
  END LOOP;
  RETURN cur;
END $as4tainr$;

-- JSON_QUERY: the SQL/JSON value (or values, in an array wrapper).
CREATE OR REPLACE FUNCTION as4tainr_svc.json_query(doc text, path text, wrapper text, omit_quotes boolean,
    on_empty text, on_error text)
RETURNS json LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
DECLARE
  js json[];
  r json;
BEGIN
  IF doc IS NULL OR path IS NULL THEN RETURN NULL; END IF;
  BEGIN
    js := as4tainr_svc.json_path_simple(doc::json, path);
    IF js IS NULL THEN
      SELECT array_agg(x::json) INTO js FROM jsonb_path_query(doc::jsonb, path::jsonpath) AS x;
    END IF;
  EXCEPTION WHEN OTHERS THEN
    IF on_error = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22032', MESSAGE = 'SQL16410: JSON_QUERY: ' || SQLERRM;
    END IF;
    RETURN CASE on_error WHEN 'EMPTY ARRAY' THEN '[]'::json WHEN 'EMPTY OBJECT' THEN '{}'::json END;
  END;
  IF js IS NULL OR cardinality(js) = 0 THEN
    IF on_empty = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22035', MESSAGE = 'SQL16410: JSON_QUERY: no value for path ' || path;
    END IF;
    RETURN CASE on_empty WHEN 'EMPTY ARRAY' THEN '[]'::json WHEN 'EMPTY OBJECT' THEN '{}'::json END;
  END IF;
  IF wrapper = 'UNCONDITIONAL'
     OR (wrapper = 'CONDITIONAL' AND (cardinality(js) > 1 OR json_typeof(js[1]) NOT IN ('object', 'array'))) THEN
    r := to_json(js);
    RETURN as4tainr_svc.json_compact(r);
  END IF;
  IF cardinality(js) > 1 THEN
    IF on_error = 'ERROR' THEN
      RAISE EXCEPTION USING ERRCODE = '22034', MESSAGE = 'SQL16410: JSON_QUERY: more than one value for path ' || path;
    END IF;
    RETURN NULL;
  END IF;
  IF omit_quotes AND json_typeof(js[1]) = 'string' THEN
    RETURN to_json(js[1] #>> '{}');
  END IF;
  RETURN as4tainr_svc.json_compact(js[1]);
END $as4tainr$;

-- JSON_EXISTS: is there a value for the path? on_error: FALSE (the
-- default), TRUE, UNKNOWN (null) or ERROR.
CREATE OR REPLACE FUNCTION as4tainr_svc.json_exists(doc text, path text, on_error text)
RETURNS boolean LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp AS $as4tainr$
BEGIN
  IF doc IS NULL OR path IS NULL THEN RETURN NULL; END IF;
  RETURN jsonb_path_exists(doc::jsonb, path::jsonpath);
EXCEPTION WHEN OTHERS THEN
  CASE on_error
    WHEN 'TRUE' THEN RETURN true;
    WHEN 'UNKNOWN' THEN RETURN NULL;
    WHEN 'ERROR' THEN RAISE EXCEPTION USING ERRCODE = '22032', MESSAGE = 'SQL16410: JSON_EXISTS: ' || SQLERRM;
    ELSE RETURN false;
  END CASE;
END $as4tainr$;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(sql: &str) -> String {
        let toks = rewrite(crate::sql::tokenize(sql).unwrap()).unwrap();
        toks.iter()
            .map(|t| match t {
                Tok::Space(s) | Tok::Word(s) | Tok::Number(s) | Tok::Punct(s) => s.clone(),
                Tok::Str(s) => format!("'{s}'"),
                Tok::Quoted(s) => format!("\"{s}\""),
            })
            .collect()
    }

    #[test]
    fn constructors() {
        // Db2 for i SQL Reference, JSON_OBJECT: KEY k VALUE v pairs,
        // NULL ON NULL by default; JSON_ARRAY: ABSENT ON NULL by default.
        let s = tr("SELECT JSON_OBJECT(KEY 'id' VALUE ID, 'n' VALUE JSON_ARRAY(1, NULL) ABSENT ON NULL) FROM T");
        assert_eq!(
            s,
            "SELECT as4tainr_svc.json_compact(as4tainr_svc.json_absent(json_build_object(CAST( 'id'  AS text), \
             to_json( ID), CAST( 'n'  AS text), to_json( as4tainr_svc.json_compact(as4tainr_svc.json_absent(\
             json_build_array(to_json(1), CAST(NULL AS json)))) )))) FROM T"
        );
    }
}
