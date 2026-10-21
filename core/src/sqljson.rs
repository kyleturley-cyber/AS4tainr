//! JSON_TABLE (Db2 for i SQL Reference, JSON_TABLE table function) on
//! PostgreSQL 16, which has the SQL/JSON path language but not
//! JSON_TABLE: the rows are `jsonb_path_query` of the row path, each
//! column `jsonb_path_query_first` of its path (silently: NULL ON EMPTY
//! and NULL ON ERROR, the defaults), nested paths left outer joins.

use crate::sql::Tok;
use anyhow::{bail, Result};

/// A column of the COLUMNS clause.
enum Column {
    /// name FOR ORDINALITY
    Ordinality(String),
    /// name type [FORMAT JSON] [PATH 'p'], with the quote and wrapper
    /// options of formatted columns.
    Value {
        name: String,
        ty: String,
        path: String,
        json: bool,
        omit_quotes: bool,
        wrapper: bool,
    },
    /// NESTED [PATH] 'p' COLUMNS (...)
    Nested(String, Vec<Column>),
}

fn word(t: &Tok) -> Option<String> {
    match t {
        Tok::Word(w) => Some(w.to_ascii_uppercase()),
        _ => None,
    }
}

fn is_punct(t: &Tok, p: &str) -> bool {
    matches!(t, Tok::Punct(x) if x == p)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Tokens without spaces, with a cursor.
struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn word(&self) -> Option<String> {
        self.peek().and_then(word)
    }
    fn eat(&mut self, w: &str) -> bool {
        if self.word().as_deref() == Some(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn punct(&mut self, p: &str) -> bool {
        if self.peek().is_some_and(|t| is_punct(t, p)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self, what: &str) -> Result<String> {
        match self.peek() {
            Some(Tok::Str(s)) => {
                let s = s.clone();
                self.i += 1;
                Ok(s)
            }
            _ => bail!("SQL0104: JSON_TABLE: {what} (a string constant) is expected."),
        }
    }
    fn name(&mut self) -> Result<String> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.i += 1;
                Ok(w)
            }
            Some(Tok::Quoted(w)) => {
                let w = w.clone();
                self.i += 1;
                Ok(w)
            }
            _ => bail!("SQL0104: JSON_TABLE: a column name is expected."),
        }
    }
    /// A data type as written: words and a parenthesized length.
    fn data_type(&mut self) -> Result<String> {
        let mut ty = self
            .word()
            .ok_or_else(|| anyhow::anyhow!("SQL0104: JSON_TABLE: a data type is expected."))?;
        self.i += 1;
        while let Some(w) = self.word() {
            if ["VARYING", "PRECISION"].contains(&w.as_str()) {
                ty.push(' ');
                ty.push_str(&w);
                self.i += 1;
            } else {
                break;
            }
        }
        if self.punct("(") {
            ty.push('(');
            loop {
                match self.peek() {
                    Some(t) if is_punct(t, ")") => {
                        self.i += 1;
                        break;
                    }
                    Some(Tok::Number(n)) => ty.push_str(n),
                    Some(t) if is_punct(t, ",") => ty.push(','),
                    Some(Tok::Word(w)) => ty.push_str(w),
                    _ => bail!("SQL0104: JSON_TABLE: a data type is not complete."),
                }
                self.i += 1;
            }
            ty.push(')');
        }
        // CCSID n, FOR BIT DATA: accepted.
        if self.eat("CCSID") {
            self.i += 1;
        }
        Ok(ty)
    }
    /// ON EMPTY / ON ERROR clauses (NULL, ERROR, EMPTY ARRAY...):
    /// accepted; as4tainr gives the null value.
    fn skip_behaviors(&mut self) {
        loop {
            let start = self.i;
            if self.eat("NULL") || self.eat("ERROR") || self.eat("EMPTY") {
                let _ = self.eat("ARRAY") || self.eat("OBJECT");
                if self.eat("ON") && (self.eat("EMPTY") || self.eat("ERROR")) {
                    continue;
                }
            }
            self.i = start;
            break;
        }
    }
    fn columns(&mut self) -> Result<Vec<Column>> {
        if !self.punct("(") {
            bail!("SQL0104: JSON_TABLE: COLUMNS ( is expected.");
        }
        let mut cols = Vec::new();
        loop {
            if self.eat("NESTED") {
                let _ = self.eat("PATH");
                let path = self.string("a nested path")?;
                if self.eat("AS") {
                    self.name()?;
                }
                if !self.eat("COLUMNS") {
                    bail!("SQL0104: JSON_TABLE: COLUMNS is expected after NESTED PATH.");
                }
                cols.push(Column::Nested(path, self.columns()?));
            } else {
                let name = self.name()?;
                if self.eat("FOR") {
                    if !self.eat("ORDINALITY") {
                        bail!("SQL0104: JSON_TABLE: FOR ORDINALITY is expected.");
                    }
                    cols.push(Column::Ordinality(name));
                } else {
                    let ty = self.data_type()?;
                    let json = self.eat("FORMAT") && self.eat("JSON");
                    let mut path = format!("$.{name}");
                    let (mut omit_quotes, mut wrapper) = (false, false);
                    loop {
                        if self.eat("PATH") {
                            path = self.string("a column path")?;
                        } else if matches!(self.word().as_deref(), Some("OMIT" | "KEEP")) {
                            omit_quotes = self.word().as_deref() == Some("OMIT");
                            self.i += 1;
                            let _ = self.eat("QUOTES");
                            if self.eat("ON") {
                                let _ = self.eat("SCALAR") && self.eat("STRING");
                            }
                        } else if self.eat("WITH") {
                            let _ = self.eat("UNCONDITIONAL") || self.eat("CONDITIONAL");
                            let _ = self.eat("ARRAY");
                            let _ = self.eat("WRAPPER");
                            wrapper = true;
                        } else if self.eat("WITHOUT") {
                            let _ = self.eat("ARRAY");
                            let _ = self.eat("WRAPPER");
                        } else {
                            let before = self.i;
                            self.skip_behaviors();
                            if self.i == before {
                                break;
                            }
                        }
                    }
                    cols.push(Column::Value {
                        name,
                        ty,
                        path,
                        json,
                        omit_quotes,
                        wrapper,
                    });
                }
            }
            if self.punct(",") {
                continue;
            }
            if self.punct(")") {
                break;
            }
            bail!("SQL0104: JSON_TABLE: , or ) is expected in COLUMNS.");
        }
        Ok(cols)
    }
}

/// The SELECT list and joins of the columns at one nesting level.
fn build(
    cols: &[Column],
    alias: &str,
    level: &mut usize,
    select: &mut Vec<String>,
    joins: &mut Vec<String>,
) -> Result<()> {
    let mut nested_seen = false;
    for c in cols {
        match c {
            Column::Ordinality(n) => {
                // A nested path that finds nothing still numbers its row 1
                // (Db2 for i SQL Programming, "Using JSON_TABLE").
                select.push(format!(
                    "CAST(coalesce({alias}.o, 1) AS integer) AS {}",
                    crate::catalog::ident(n)
                ))
            }
            Column::Value {
                name,
                ty,
                path,
                json,
                omit_quotes,
                wrapper,
            } => {
                let pg = crate::ddl::parse_type(ty)?.sql_type();
                let first = format!(
                    "jsonb_path_query_first({alias}.v, {}, '{{}}', true)",
                    quote(path)
                );
                let e = if *wrapper {
                    format!(
                        "jsonb_path_query_array({alias}.v, {}, '{{}}', true)::text",
                        quote(path)
                    )
                } else if *json && *omit_quotes {
                    format!(
                        "CASE WHEN jsonb_typeof({first}) = 'string' THEN jsonb_build_array({first}) ->> 0 ELSE ({first})::text END"
                    )
                } else if *json {
                    format!("({first})::text")
                } else {
                    // A scalar column: an object or array is an error, so
                    // the null value.
                    format!(
                        "CASE WHEN jsonb_typeof({first}) IN ('object', 'array') THEN NULL ELSE jsonb_build_array({first}) ->> 0 END"
                    )
                };
                select.push(format!(
                    "CAST({e} AS {pg}) AS {}",
                    crate::catalog::ident(name)
                ));
            }
            Column::Nested(path, inner) => {
                if nested_seen {
                    bail!(
                        "SQL0084: JSON_TABLE: sibling NESTED PATH clauses are not supported yet."
                    );
                }
                nested_seen = true;
                *level += 1;
                let a = format!("as4tainr_jt{level}");
                joins.push(format!(
                    "LEFT JOIN LATERAL jsonb_path_query({alias}.v, {}) WITH ORDINALITY AS {a}(v, o) ON true",
                    quote(path)
                ));
                build(inner, &a, level, select, joins)?;
            }
        }
    }
    Ok(())
}

/// Replace each JSON_TABLE(...) in a statement's tokens by a derived
/// table that PostgreSQL runs. The document expression keeps its tokens
/// (the translation that follows handles them).
pub(crate) fn rewrite(toks: Vec<Tok>) -> Result<Vec<Tok>> {
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        let is_jt = word(&toks[i]).as_deref() == Some("JSON_TABLE")
            && toks[i + 1..]
                .iter()
                .find(|t| !matches!(t, Tok::Space(_)))
                .is_some_and(|t| is_punct(t, "("));
        if !is_jt {
            out.push(toks[i].clone());
            i += 1;
            continue;
        }
        // The arguments: up to the matching parenthesis.
        let open = (i + 1..toks.len())
            .find(|&j| is_punct(&toks[j], "("))
            .unwrap();
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
            bail!("SQL0104: JSON_TABLE: ) is missing.");
        };
        // The document: up to the first comma outside parentheses.
        let mut depth = 0;
        let comma = (open + 1..close).find(|&j| {
            if is_punct(&toks[j], "(") {
                depth += 1;
            } else if is_punct(&toks[j], ")") {
                depth -= 1;
            }
            depth == 0 && is_punct(&toks[j], ",")
        });
        let Some(comma) = comma else {
            bail!("SQL0104: JSON_TABLE needs a JSON document, a path and COLUMNS.");
        };
        let mut doc: Vec<Tok> = toks[open + 1..comma].to_vec();
        // FORMAT JSON after the document: accepted.
        let words: Vec<usize> = (0..doc.len())
            .filter(|&k| !matches!(doc[k], Tok::Space(_)))
            .collect();
        if words.len() >= 3 {
            let (a, b) = (words[words.len() - 2], words[words.len() - 1]);
            if word(&doc[a]).as_deref() == Some("FORMAT")
                && word(&doc[b]).as_deref() == Some("JSON")
            {
                doc.truncate(a);
            }
        }
        let mut p = P {
            t: toks[comma + 1..close]
                .iter()
                .filter(|t| !matches!(t, Tok::Space(_)))
                .cloned()
                .collect(),
            i: 0,
        };
        let path = p.string("the row path")?;
        if p.eat("AS") {
            p.name()?;
        }
        let mut error_on_error = false;
        if p.word().as_deref() == Some("ERROR") {
            error_on_error = true;
        }
        let _ = (p.eat("EMPTY") || p.eat("ERROR")) && p.eat("ON") && p.eat("ERROR");
        if !p.eat("COLUMNS") {
            bail!("SQL0104: JSON_TABLE: COLUMNS is expected.");
        }
        let cols = p.columns()?;
        if p.word().as_deref() == Some("ERROR") {
            error_on_error = true;
        }
        let _ = (p.eat("EMPTY") || p.eat("ERROR")) && p.eat("ON") && p.eat("ERROR");
        if p.i < p.t.len() {
            bail!("SQL0104: JSON_TABLE: a token after COLUMNS is not valid.");
        }
        let mut select = Vec::new();
        let mut joins = Vec::new();
        let mut level = 0;
        build(&cols, "as4tainr_jt0", &mut level, &mut select, &mut joins)?;
        // EMPTY ON ERROR (the default): a document that is not JSON gives
        // no rows.
        let (to_json, end) = if error_on_error {
            ("CAST((", ") AS jsonb)")
        } else {
            ("as4tainr_svc.try_jsonb(CAST((", ") AS text))")
        };
        out.push(Tok::Punct(format!(
            "(SELECT {} FROM jsonb_path_query({to_json}",
            select.join(", ")
        )));
        out.extend(doc);
        out.push(Tok::Punct(format!(
            "{end}, {}) WITH ORDINALITY AS as4tainr_jt0(v, o) {})",
            quote(&path),
            joins.join(" ")
        )));
        i = close + 1;
    }
    Ok(out)
}
