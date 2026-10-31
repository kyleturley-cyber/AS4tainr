//! SQL global variables (Db2 for i SQL Reference, CREATE VARIABLE): a
//! variable is an object (a *SRVPGM, attribute CLE, as PUB400 showed:
//! tools/golden/verify30.txt w30_obj) described in
//! `as4tainr.sql_variables`; each job has its own value, the default until
//! the job sets one (w30_rows: 'N' in a new job, 'Y' after SET in the same
//! RUNSQLSTM script, 'N' again in the next job).
//!
//! The values are kept in this process by job (its QTEMP library names
//! the job; else the process). A statement that names a variable has the
//! name replaced by its value, cast to the variable's type, before it is
//! translated (`substitute`); a column of a table the statement names
//! wins over a variable of the same name, as Db2 resolves names.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{bail, Result};
use tokio_postgres::Client;

/// (job, library, variable) → the value set (None: NULL).
type Values = HashMap<(String, String, String), Option<String>>;

static VALUES: Mutex<Option<Values>> = Mutex::new(None);

/// The job whose values are meant.
pub fn job() -> String {
    crate::qtemp::current().unwrap_or_else(|| format!("PID{}", std::process::id()))
}

/// SET: the job's value of a variable.
pub fn set(lib: &str, name: &str, value: Option<String>) {
    if let Ok(mut g) = VALUES.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert((job(), lib.to_string(), name.to_string()), value);
    }
}

/// The job's value forgotten: the default again (as in a new job).
pub fn clear(lib: &str, name: &str) {
    if let Ok(mut g) = VALUES.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(&(job(), lib.to_string(), name.to_string()));
        }
    }
}

fn get(lib: &str, name: &str) -> Option<Option<String>> {
    VALUES.lock().ok().and_then(|g| {
        g.as_ref()
            .and_then(|m| m.get(&(job(), lib.to_string(), name.to_string())).cloned())
    })
}

/// A variable's definition: (library, type, default expression).
pub async fn find(
    client: &Client,
    libl: &[String],
    lib: Option<&str>,
    name: &str,
) -> Result<Option<(String, String, Option<String>)>> {
    let libs: Vec<String> = match lib {
        Some(l) => vec![crate::qtemp::real(l)],
        None => libl.to_vec(),
    };
    for l in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT data_type, dft FROM as4tainr.sql_variables WHERE library=$1 AND name=$2",
                &[l, &name],
            )
            .await?
        {
            return Ok(Some((l.clone(), r.get(0), r.get(1))));
        }
    }
    Ok(None)
}

/// The words of a statement outside literals and quoted names, as
/// (start, end) byte offsets.
fn words(sql: &str) -> Vec<(usize, usize)> {
    let cs: Vec<(usize, char)> = sql.char_indices().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || "_#@$".contains(c);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i].1;
        if c == '\'' || c == '"' {
            i += 1;
            while i < cs.len() && cs[i].1 != c {
                i += 1;
            }
            i += 1;
            continue;
        }
        if is_word(c) && !c.is_ascii_digit() && (i == 0 || !is_word(cs[i - 1].1)) {
            let start = cs[i].0;
            while i < cs.len() && is_word(cs[i].1) {
                i += 1;
            }
            let end = if i < cs.len() { cs[i].0 } else { sql.len() };
            out.push((start, end));
            continue;
        }
        i += 1;
    }
    out
}

/// Replace the global variables a statement names by their values.
pub async fn substitute(client: &Client, libl: &[String], sql: &str) -> Result<String> {
    let ws = words(sql);
    let names: Vec<String> = ws
        .iter()
        .map(|(a, b)| sql[*a..*b].to_ascii_uppercase())
        .collect();
    if names.is_empty() {
        return Ok(sql.to_string());
    }
    let rows = client
        .query(
            "SELECT library, name, data_type, dft FROM as4tainr.sql_variables WHERE name = ANY($1)",
            &[&names],
        )
        .await?;
    if rows.is_empty() {
        return Ok(sql.to_string());
    }
    let defs: Vec<(String, String, String, Option<String>)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
    // Columns of the files the statement names win over variables.
    let columns: Vec<String> = client
        .query(
            "SELECT DISTINCT upper(coalesce(ff.alias, ff.name)) FROM as4tainr.file_fields ff
              WHERE ff.library = ANY($1) AND ff.file IN (
                    SELECT f.name FROM as4tainr.files f
                     WHERE f.library = ANY($1) AND (f.name = ANY($2) OR upper(f.sql_name) = ANY($2)))
              UNION SELECT DISTINCT ff.name FROM as4tainr.file_fields ff
              WHERE ff.library = ANY($1) AND ff.file IN (
                    SELECT f.name FROM as4tainr.files f
                     WHERE f.library = ANY($1) AND (f.name = ANY($2) OR upper(f.sql_name) = ANY($2)))",
            &[&libl, &names],
        )
        .await?
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut out = String::new();
    let mut last = 0;
    for (k, (a, b)) in ws.iter().enumerate() {
        let w = &names[k];
        // Qualified: LIB.NAME or LIB/NAME right before it.
        let before = sql[..*a].trim_end();
        let qualifier = if (before.ends_with('.') || before.ends_with('/')) && k > 0 {
            let (qa, qb) = ws[k - 1];
            (sql[qb..*a].trim() == "." || sql[qb..*a].trim() == "/")
                .then(|| (qa, sql[qa..qb].to_ascii_uppercase()))
        } else {
            None
        };
        // A qualified name whose qualifier is itself qualified (a column
        // of LIB.TABLE) is not a variable.
        let def = match &qualifier {
            Some((_, lib)) => defs
                .iter()
                .find(|d| &d.1 == w && crate::qtemp::real(lib) == d.0),
            None if columns.contains(w) => None,
            None => libl
                .iter()
                .find_map(|l| defs.iter().find(|d| &d.1 == w && &d.0 == l)),
        };
        let Some((lib, name, ty, dft)) = def else {
            continue;
        };
        let start = qualifier.as_ref().map(|q| q.0).unwrap_or(*a);
        if start < last {
            continue;
        }
        let value = match get(lib, name) {
            Some(Some(v)) => format!("CAST('{}' AS {ty})", v.replace('\'', "''")),
            Some(None) => format!("CAST(NULL AS {ty})"),
            None => match dft {
                Some(d) => format!("CAST(({d}) AS {ty})"),
                None => format!("CAST(NULL AS {ty})"),
            },
        };
        out.push_str(&sql[last..start]);
        out.push_str(&value);
        last = *b;
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

/// CREATE [OR REPLACE] VARIABLE: the object and its description.
pub async fn create(
    client: &Client,
    lib: &str,
    name: &str,
    data_type: &str,
    default: Option<&str>,
    replace: bool,
) -> Result<String> {
    let exists = client
        .query_opt(
            "SELECT 1 FROM as4tainr.sql_variables WHERE library=$1 AND name=$2",
            &[&lib, &name],
        )
        .await?
        .is_some();
    if exists && !replace {
        bail!("SQL0601: {name} in {lib} type *SRVPGM already exists.");
    }
    if !exists && crate::catalog::object_exists(client, &[lib.to_string()], name, "*SRVPGM").await?
    {
        bail!("SQL0601: {name} in {lib} type *SRVPGM already exists.");
    }
    client
        .execute(
            "INSERT INTO as4tainr.sql_variables (library, name, data_type, dft) VALUES ($1,$2,$3,$4)
             ON CONFLICT (library, name) DO UPDATE SET data_type = EXCLUDED.data_type, dft = EXCLUDED.dft",
            &[&lib, &name, &data_type, &default],
        )
        .await?;
    if !exists {
        client
            .execute(
                "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
                 VALUES ($1, $2, '*SRVPGM', 'CLE', '')",
                &[&lib, &name],
            )
            .await?;
    }
    Ok(format!("Variable {name} created in {lib}."))
}

/// DROP VARIABLE.
pub async fn drop(client: &Client, lib: &str, name: &str) -> Result<bool> {
    let n = client
        .execute(
            "DELETE FROM as4tainr.sql_variables WHERE library=$1 AND name=$2",
            &[&lib, &name],
        )
        .await?;
    if n > 0 {
        client
            .execute(
                "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype='*SRVPGM'",
                &[&lib, &name],
            )
            .await?;
    }
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_outside_literals() {
        let s = "INSERT INTO T VALUES (1, 'GLD30V', LIB.GLD30V)";
        let w: Vec<&str> = words(s).iter().map(|(a, b)| &s[*a..*b]).collect();
        assert_eq!(w, ["INSERT", "INTO", "T", "VALUES", "LIB", "GLD30V"]);
    }
}
