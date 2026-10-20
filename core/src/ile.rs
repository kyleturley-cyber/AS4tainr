//! ILE program objects built from modules: modules (*MODULE, CRTRPGMOD),
//! service programs (*SRVPGM, CRTSRVPGM), programs bound from modules
//! (CRTPGM, and CRTBNDRPG with binding directories), binding directories
//! (*BNDDIR) and binder language (STRPGMEXP / EXPORT / ENDPGMEXP).
//!
//! Written from IBM's ILE Concepts manual. Binding copies modules into the
//! object being created, and resolves each procedure a module imports (a
//! prototype it calls that no module of its own defines) to a module
//! bound in or to a service program that exports it; a symbol that is not
//! found stops the creation (CPD5D02). Service programs are bound by
//! reference: the program records which one, and calls its procedures in
//! it when it runs. A service program's global storage lives in its
//! activation: it is kept for the rest of the job.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use tokio_postgres::Client;

use crate::catalog::validate_name;

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS as4tainr.modules (
    library   text NOT NULL,
    name      text NOT NULL,
    attribute text NOT NULL,
    source    text NOT NULL,
    options   text NOT NULL DEFAULT '',
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.service_programs (
    library text NOT NULL,
    name    text NOT NULL,
    binding text NOT NULL,
    PRIMARY KEY (library, name)
);
CREATE TABLE IF NOT EXISTS as4tainr.bnddir_entries (
    library  text NOT NULL,
    bnddir   text NOT NULL,
    seq      int  NOT NULL,
    obj_lib  text NOT NULL,
    obj_name text NOT NULL,
    obj_type text NOT NULL,
    PRIMARY KEY (library, bnddir, seq)
);
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                 WHERE table_schema='as4tainr' AND table_name='programs' AND column_name='binding') THEN
    ALTER TABLE as4tainr.programs ADD COLUMN IF NOT EXISTS binding text;
  END IF;
END $$;
"#;

/// A module's code as bound into an object (a copy: changing the module
/// afterwards does not change the object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleCopy {
    pub name: String,
    pub source: String,
    pub options: String,
}

/// Where an imported procedure is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// A module bound into the same object.
    Module(String),
    /// A service program, bound by reference.
    SrvPgm { library: String, name: String },
}

/// How an object's modules are bound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The modules (for a program, besides its entry module).
    pub modules: Vec<ModuleCopy>,
    /// Each imported procedure, and where it is.
    pub imports: BTreeMap<String, Target>,
    /// A service program's exported procedures.
    pub exports: Vec<String>,
    /// Bound service programs (library, name).
    pub srvpgms: Vec<(String, String)>,
    /// A service program's signature (binder language, or generated).
    pub signature: String,
}

/// What a module defines and needs, from its compiled form.
pub struct ModuleInfo {
    pub name: String,
    /// Procedures it exports (EXPORT).
    pub exports: Vec<String>,
    /// Procedures it calls that it does not define.
    pub imports: Vec<String>,
    /// It has a main procedure (cycle or ctl-opt MAIN): it can be a
    /// program's entry module.
    pub has_main: bool,
}

/// Compile a module and say what it exports and imports.
pub async fn module_info(client: &Client, libl: &[String], m: &ModuleCopy) -> Result<ModuleInfo> {
    let prog = crate::rpg::compile_with(client, libl, &m.name, &m.source, &m.options)
        .await
        .map_err(|e| anyhow!("module {}: {e:#}", m.name))?;
    Ok(ModuleInfo {
        name: m.name.clone(),
        exports: prog.exports(),
        imports: prog.imports.clone(),
        has_main: !prog.nomain,
    })
}

/// A binding directory's entries (library, name, type), in order.
pub async fn bnddir_entries(
    client: &Client,
    libl: &[String],
    bnddir: &str,
) -> Result<Vec<(String, String, String)>> {
    let (libs, name) = match bnddir.split_once('/') {
        Some((l, n)) if l != "*LIBL" => (vec![l.to_ascii_uppercase()], n.to_ascii_uppercase()),
        Some((_, n)) => (libl.to_vec(), n.to_ascii_uppercase()),
        None => (libl.to_vec(), bnddir.to_ascii_uppercase()),
    };
    for lib in &libs {
        if crate::catalog::object_exists(client, std::slice::from_ref(lib), &name, "*BNDDIR")
            .await?
        {
            let rows = client
                .query(
                    "SELECT obj_lib, obj_name, obj_type FROM as4tainr.bnddir_entries
                      WHERE library=$1 AND bnddir=$2 ORDER BY seq",
                    &[lib, &name],
                )
                .await?;
            return Ok(rows
                .iter()
                .map(|r| (r.get(0), r.get(1), r.get(2)))
                .collect());
        }
    }
    bail!("CPF5D07: Binding directory {name} not found.")
}

/// A service program's binding, found through the library list (or in
/// the library named).
pub async fn find_srvpgm(
    client: &Client,
    libl: &[String],
    library: &str,
    name: &str,
) -> Result<(String, Binding)> {
    let libs = if library == "*LIBL" || library.is_empty() {
        libl.to_vec()
    } else {
        vec![library.to_string()]
    };
    for lib in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT binding FROM as4tainr.service_programs WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            let b: String = r.get(0);
            return Ok((lib.clone(), serde_json::from_str(&b)?));
        }
    }
    bail!("CPF5D06: Service program {name} not found in library {library}.")
}

/// A module object (CRTRPGMOD): its source and options.
pub async fn find_module(
    client: &Client,
    libl: &[String],
    library: &str,
    name: &str,
) -> Result<ModuleCopy> {
    let libs = if library == "*LIBL" || library.is_empty() {
        libl.to_vec()
    } else {
        vec![library.to_string()]
    };
    for lib in &libs {
        if let Some(r) = client
            .query_opt(
                "SELECT source, options FROM as4tainr.modules WHERE library=$1 AND name=$2",
                &[lib, &name],
            )
            .await?
        {
            return Ok(ModuleCopy {
                name: name.to_string(),
                source: r.get(0),
                options: r.get(1),
            });
        }
    }
    bail!("CPF5D05: Module {name} not found in library {library}.")
}

/// Bind: resolve every import of `modules` (the entry module first for a
/// program) against the modules themselves, the service programs named,
/// then the binding directories' entries in order (a module found there is
/// copied in; a service program is bound by reference).
pub async fn bind(
    client: &Client,
    libl: &[String],
    mut modules: Vec<ModuleCopy>,
    bndsrvpgm: &[(String, String)],
    bnddir: &[String],
) -> Result<(Binding, Vec<ModuleInfo>)> {
    let mut infos = Vec::new();
    for m in &modules {
        infos.push(module_info(client, libl, m).await?);
    }
    let mut binding = Binding::default();
    let mut srvpgms: Vec<(String, String, Binding)> = Vec::new();
    for (l, n) in bndsrvpgm {
        let (lib, b) = find_srvpgm(client, libl, l, n).await?;
        srvpgms.push((lib, n.clone(), b));
    }
    let mut dir: Vec<(String, String, String)> = Vec::new();
    for d in bnddir {
        dir.extend(bnddir_entries(client, libl, d).await?);
    }
    // Resolve until nothing new is bound in (a module from a binding
    // directory can import procedures of its own).
    let mut k = 0;
    while k < infos.len() {
        let imports = infos[k].imports.clone();
        for sym in imports {
            if binding.imports.contains_key(&sym) {
                continue;
            }
            if let Some(m) = infos.iter().find(|i| i.exports.contains(&sym)) {
                binding
                    .imports
                    .insert(sym.clone(), Target::Module(m.name.clone()));
                continue;
            }
            if let Some((lib, name, _)) = srvpgms.iter().find(|(_, _, b)| b.exports.contains(&sym))
            {
                binding.imports.insert(
                    sym.clone(),
                    Target::SrvPgm {
                        library: lib.clone(),
                        name: name.clone(),
                    },
                );
                continue;
            }
            let mut found = false;
            for (l, n, t) in &dir {
                match t.as_str() {
                    "*SRVPGM" => {
                        let Ok((lib, b)) = find_srvpgm(client, libl, l, n).await else {
                            continue;
                        };
                        if b.exports.contains(&sym) {
                            binding.imports.insert(
                                sym.clone(),
                                Target::SrvPgm {
                                    library: lib.clone(),
                                    name: n.clone(),
                                },
                            );
                            if !srvpgms.iter().any(|(sl, sn, _)| *sl == lib && sn == n) {
                                srvpgms.push((lib, n.clone(), b));
                            }
                            found = true;
                            break;
                        }
                    }
                    "*MODULE" => {
                        let Ok(m) = find_module(client, libl, l, n).await else {
                            continue;
                        };
                        let info = module_info(client, libl, &m).await?;
                        if info.exports.contains(&sym) {
                            binding
                                .imports
                                .insert(sym.clone(), Target::Module(m.name.clone()));
                            if !modules.iter().any(|x| x.name == m.name) {
                                modules.push(m);
                                infos.push(info);
                            }
                            found = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if !found {
                bail!(
                    "CPD5D02: Definition for symbol {sym} not found (imported by module {}).",
                    infos[k].name
                );
            }
        }
        k += 1;
    }
    binding.modules = modules;
    binding.srvpgms = srvpgms.into_iter().map(|(l, n, _)| (l, n)).collect();
    Ok((binding, infos))
}

/// Binder language (ILE Concepts, binder language): the exports of a
/// service program and its signature.
///
/// ```text
/// STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('MYSIG')
///   EXPORT SYMBOL(PROC1)
///   EXPORT SYMBOL('Proc2')
/// ENDPGMEXP
/// ```
///
/// Only the *CURRENT block's exports count (*PRV blocks keep older
/// signatures valid; as4tainr does not check signatures yet).
pub fn binder_language(source: &str) -> Result<(Vec<String>, Option<String>)> {
    let mut exports = Vec::new();
    let mut signature = None;
    let mut current = false;
    let mut seen_current = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("/*") {
            continue;
        }
        let cmd = crate::cl::parse(line).map_err(|e| anyhow!("binder language: {e}"))?;
        match cmd.def.name {
            "STRPGMEXP" => {
                current = cmd.get("PGMLVL") != "*PRV";
                if current {
                    if seen_current {
                        bail!("CPD5D17: Only one PGMLVL(*CURRENT) export block is allowed.");
                    }
                    seen_current = true;
                    let s = cmd.get("SIGNATURE");
                    if s != "*GEN" {
                        signature = Some(s.to_string());
                    }
                }
            }
            "EXPORT" => {
                if current {
                    let sym = cmd.get("SYMBOL").to_string();
                    exports.push(sym.to_ascii_uppercase());
                }
            }
            "ENDPGMEXP" => current = false,
            other => bail!("binder language: {other} is not a binder language statement"),
        }
    }
    if !seen_current {
        bail!("CPD5D16: The binder source has no PGMLVL(*CURRENT) export block.");
    }
    Ok((exports, signature))
}

/// A generated signature: a digest of the exports in order (ILE: the
/// binder generates one from the export list).
pub fn generated_signature(exports: &[String]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for e in exports {
        for b in e.bytes().chain(std::iter::once(0)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016X}")
}

/// Global storage of the service programs active in each job (their
/// activation; kept until the job ends): (job, LIB/SRVPGM/MODULE).
/// (job, unit) -> the unit's global storage.
type Activations = HashMap<(String, String), Vec<Vec<u8>>>;

static ACTIVATIONS: Mutex<Option<Activations>> = Mutex::new(None);

/// A module's storage in a service program activation, if it was
/// activated before in the job.
pub fn activation(job: &str, unit: &str) -> Option<Vec<Vec<u8>>> {
    ACTIVATIONS
        .lock()
        .ok()?
        .as_ref()?
        .get(&(job.to_string(), unit.to_string()))
        .cloned()
}

/// Keep a module's storage in the job's activation.
pub fn keep_activation(job: &str, unit: &str, storage: Vec<Vec<u8>>) {
    if let Ok(mut g) = ACTIVATIONS.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert((job.to_string(), unit.to_string()), storage);
    }
}

/// RCLACTGRP / the end of a job: its activations go.
pub fn end_activations(job: &str) {
    if let Ok(mut g) = ACTIVATIONS.lock() {
        if let Some(m) = g.as_mut() {
            m.retain(|(j, _), _| j != job);
        }
    }
}

/// CRTBNDDIR.
pub async fn create_bnddir(client: &Client, library: &str, name: &str, text: &str) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("binding directory", name)?;
    if !crate::catalog::library_exists(client, &lib).await? {
        bail!("CPF2110: Library {lib} not found.");
    }
    let n = client
        .execute(
            "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text)
             VALUES ($1,$2,'*BNDDIR','',$3) ON CONFLICT DO NOTHING",
            &[&lib, &name, &text],
        )
        .await?;
    if n == 0 {
        bail!("CPF5D0E: Binding directory {name} in library {lib} already exists.");
    }
    Ok(())
}

/// ADDBNDDIRE: entries at the end.
pub async fn add_bnddir_entries(
    client: &Client,
    library: &str,
    bnddir: &str,
    entries: &[(String, String, String)],
) -> Result<usize> {
    let next: i32 = client
        .query_one(
            "SELECT coalesce(max(seq), 0) + 1 FROM as4tainr.bnddir_entries WHERE library=$1 AND bnddir=$2",
            &[&library, &bnddir],
        )
        .await?
        .get(0);
    let mut added = 0;
    for (i, (l, n, t)) in entries.iter().enumerate() {
        let exists = client
            .query_opt(
                "SELECT 1 FROM as4tainr.bnddir_entries WHERE library=$1 AND bnddir=$2
                   AND obj_lib=$3 AND obj_name=$4 AND obj_type=$5",
                &[&library, &bnddir, l, n, t],
            )
            .await?
            .is_some();
        if exists {
            continue;
        }
        client
            .execute(
                "INSERT INTO as4tainr.bnddir_entries VALUES ($1,$2,$3,$4,$5,$6)",
                &[&library, &bnddir, &(next + i as i32), l, n, t],
            )
            .await?;
        added += 1;
    }
    Ok(added)
}

/// RMVBNDDIRE.
pub async fn remove_bnddir_entries(
    client: &Client,
    library: &str,
    bnddir: &str,
    entries: &[(String, String)],
) -> Result<u64> {
    let mut n = 0;
    for (obj, t) in entries {
        let (l, name) = match obj.split_once('/') {
            Some((l, n)) => (l.to_string(), n.to_string()),
            None => ("*LIBL".to_string(), obj.clone()),
        };
        n += client
            .execute(
                "DELETE FROM as4tainr.bnddir_entries WHERE library=$1 AND bnddir=$2
                   AND obj_name=$3 AND ($4 = '*LIBL' OR obj_lib=$4) AND ($5 = '*ALL' OR obj_type=$5)",
                &[&library, &bnddir, &name, &l, t],
            )
            .await?;
    }
    Ok(n)
}

/// Delete a module, service program or binding directory object.
pub async fn delete_object(
    client: &Client,
    library: &str,
    name: &str,
    objtype: &str,
) -> Result<()> {
    let n = client
        .execute(
            "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&library, &name, &objtype],
        )
        .await?;
    if n == 0 {
        bail!("CPF2105: Object {name} in {library} type {objtype} not found.");
    }
    let table = match objtype {
        "*MODULE" => "DELETE FROM as4tainr.modules WHERE library=$1 AND name=$2",
        "*SRVPGM" => "DELETE FROM as4tainr.service_programs WHERE library=$1 AND name=$2",
        _ => "DELETE FROM as4tainr.bnddir_entries WHERE library=$1 AND bnddir=$2",
    };
    client.execute(table, &[&library, &name]).await?;
    client
        .execute(
            "DELETE FROM as4tainr.object_auths WHERE library=$1 AND name=$2 AND objtype=$3",
            &[&library, &name, &objtype],
        )
        .await?;
    Ok(())
}

/// Store a module (CRTRPGMOD) or a service program (CRTSRVPGM), replacing
/// one of the same name.
#[allow(clippy::too_many_arguments)]
pub async fn store(
    client: &mut Client,
    library: &str,
    name: &str,
    objtype: &str,
    attribute: &str,
    text: &str,
    content: &str,
    options: &str,
) -> Result<()> {
    let lib = validate_name("library", library)?;
    let name = validate_name("object", name)?;
    let tx = client.transaction().await?;
    if tx
        .query_opt(
            "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await?
        .is_none()
    {
        bail!("CPF2110: Library {lib} not found.");
    }
    tx.execute(
        "DELETE FROM as4tainr.objects WHERE library=$1 AND name=$2 AND objtype=$3",
        &[&lib, &name, &objtype],
    )
    .await?;
    tx.execute(
        "INSERT INTO as4tainr.objects (library, name, objtype, attribute, text) VALUES ($1,$2,$3,$4,$5)",
        &[&lib, &name, &objtype, &attribute, &text],
    )
    .await?;
    if objtype == "*MODULE" {
        tx.execute(
            "DELETE FROM as4tainr.modules WHERE library=$1 AND name=$2",
            &[&lib, &name],
        )
        .await?;
        tx.execute(
            "INSERT INTO as4tainr.modules (library, name, attribute, source, options) VALUES ($1,$2,$3,$4,$5)",
            &[&lib, &name, &attribute, &content, &options],
        )
        .await?;
    } else {
        tx.execute(
            "DELETE FROM as4tainr.service_programs WHERE library=$1 AND name=$2",
            &[&lib, &name],
        )
        .await?;
        tx.execute(
            "INSERT INTO as4tainr.service_programs (library, name, binding) VALUES ($1,$2,$3)",
            &[&lib, &name, &content],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A program's binding (CRTPGM, or CRTBNDRPG with imports).
pub async fn set_program_binding(
    client: &Client,
    library: &str,
    name: &str,
    binding: Option<&Binding>,
) -> Result<()> {
    let b = binding.map(serde_json::to_string).transpose()?;
    client
        .execute(
            "UPDATE as4tainr.programs SET binding=$3 WHERE library=$1 AND name=$2",
            &[&library, &name, &b],
        )
        .await?;
    Ok(())
}
