//! Output files (OUTPUT(*OUTFILE)): the system's model files in QSYS,
//! made at catalog initialization, and writing a command's records into
//! the file OUTFILE names (made like the model when it is not there).
//!
//! The models' record formats, field names, types and lengths are IBM
//! i 7.5's, from DSPFFD of QSYS/QADSPOBJ and QSYS/QAFDMBRL on PUB400
//! (2026-10-26, tools/golden/verify1.txt v_ffd_objd, v_ffd_mbrl); field
//! texts and column headings are not copied (clean room).

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::{self, ident};

/// A field: name, DDS data type (A, P, S), length, decimal positions.
type Field = (&'static str, char, u32, u32);

/// A model file in QSYS.
pub struct Model {
    pub file: &'static str,
    pub format: &'static str,
    pub text: &'static str,
    pub fields: &'static [Field],
}

/// DSPOBJD OUTPUT(*OUTFILE).
pub const QADSPOBJ: Model = Model {
    file: "QADSPOBJ",
    format: "QLIDOBJD",
    text: "Model output file for DSPOBJD",
    fields: QLIDOBJD,
};

/// DSPFD TYPE(*MBRLIST) OUTPUT(*OUTFILE).
pub const QAFDMBRL: Model = Model {
    file: "QAFDMBRL",
    format: "QWHFDML",
    text: "Model output file for DSPFD TYPE(*MBRLIST)",
    fields: QWHFDML,
};

const QWHFDML: &[Field] = &[
    ("MLRCEN", 'A', 1, 0),
    ("MLRDAT", 'A', 6, 0),
    ("MLRTIM", 'A', 6, 0),
    ("MLFILE", 'A', 10, 0),
    ("MLLIB", 'A', 10, 0),
    ("MLFTYP", 'A', 1, 0),
    ("MLFILA", 'A', 4, 0),
    ("MLMXD", 'A', 3, 0),
    ("MLFATR", 'A', 6, 0),
    ("MLSYSN", 'A', 8, 0),
    ("MLASP", 'P', 3, 0),
    ("MLRES", 'A', 4, 0),
    ("MLNOMB", 'P', 5, 0),
    ("MLNAME", 'A', 10, 0),
    ("MLNRCD", 'P', 10, 0),
    ("MLNDTR", 'P', 10, 0),
    ("MLSIZE", 'P', 10, 0),
    ("MLSEU", 'A', 4, 0),
    ("MLCCEN", 'A', 1, 0),
    ("MLCDAT", 'A', 6, 0),
    ("MLCHGC", 'A', 1, 0),
    ("MLCHGD", 'A', 6, 0),
    ("MLCHGT", 'A', 6, 0),
    ("MLMTXT", 'A', 50, 0),
    ("MLSEU2", 'A', 10, 0),
    ("MLUCEN", 'A', 1, 0),
    ("MLUDAT", 'A', 6, 0),
    ("MLUCNT", 'P', 5, 0),
    ("MLTCEN", 'A', 1, 0),
    ("MLTDAT", 'A', 6, 0),
    ("MLSIZ2", 'P', 15, 0),
    ("MLSTFR", 'A', 1, 0),
];
const QLIDOBJD: &[Field] = &[
    ("ODDCEN", 'A', 1, 0),
    ("ODDDAT", 'A', 6, 0),
    ("ODDTIM", 'A', 6, 0),
    ("ODLBNM", 'A', 10, 0),
    ("ODOBNM", 'A', 10, 0),
    ("ODOBTP", 'A', 8, 0),
    ("ODOBAT", 'A', 10, 0),
    ("ODOBFR", 'A', 1, 0),
    ("ODOBSZ", 'P', 10, 0),
    ("ODOBTX", 'A', 50, 0),
    ("ODOBLK", 'A', 1, 0),
    ("ODOBDM", 'A', 1, 0),
    ("ODCCEN", 'A', 1, 0),
    ("ODCDAT", 'A', 6, 0),
    ("ODCTIM", 'A', 6, 0),
    ("ODOBOW", 'A', 10, 0),
    ("ODSCEN", 'A', 1, 0),
    ("ODSDAT", 'A', 6, 0),
    ("ODSTIM", 'A', 6, 0),
    ("ODSCMD", 'A', 10, 0),
    ("ODSSZE", 'P', 10, 0),
    ("ODSSLT", 'S', 2, 0),
    ("ODSDEV", 'A', 10, 0),
    ("ODSV01", 'A', 6, 0),
    ("ODSV02", 'A', 6, 0),
    ("ODSV03", 'A', 6, 0),
    ("ODSV04", 'A', 6, 0),
    ("ODSV05", 'A', 6, 0),
    ("ODSV06", 'A', 6, 0),
    ("ODSV07", 'A', 6, 0),
    ("ODSV08", 'A', 6, 0),
    ("ODSV09", 'A', 6, 0),
    ("ODSV10", 'A', 6, 0),
    ("ODSVMR", 'A', 1, 0),
    ("ODRCEN", 'A', 1, 0),
    ("ODRDAT", 'A', 6, 0),
    ("ODRTIM", 'A', 6, 0),
    ("ODCPFL", 'A', 6, 0),
    ("ODSRCF", 'A', 10, 0),
    ("ODSRCL", 'A', 10, 0),
    ("ODSRCM", 'A', 10, 0),
    ("ODSRCC", 'A', 1, 0),
    ("ODSRCD", 'A', 6, 0),
    ("ODSRCT", 'A', 6, 0),
    ("ODCMNM", 'A', 7, 0),
    ("ODCMVR", 'A', 6, 0),
    ("ODOBLV", 'A', 8, 0),
    ("ODUMOD", 'A', 1, 0),
    ("ODPPNM", 'A', 7, 0),
    ("ODPPVR", 'A', 6, 0),
    ("ODPCNR", 'A', 5, 0),
    ("ODAPAR", 'A', 6, 0),
    ("ODSSQN", 'S', 4, 0),
    ("ODLCEN", 'A', 1, 0),
    ("ODLDAT", 'A', 6, 0),
    ("ODLTIM", 'A', 6, 0),
    ("ODSFIL", 'A', 10, 0),
    ("ODSFLB", 'A', 10, 0),
    ("ODASP", 'S', 2, 0),
    ("ODLBL", 'A', 17, 0),
    ("ODPTFN", 'A', 7, 0),
    ("ODOBSY", 'A', 8, 0),
    ("ODCRTU", 'A', 10, 0),
    ("ODCRTS", 'A', 8, 0),
    ("ODUUPD", 'A', 1, 0),
    ("ODUCEN", 'A', 1, 0),
    ("ODUDAT", 'A', 6, 0),
    ("ODUCNT", 'P', 5, 0),
    ("ODTCEN", 'A', 1, 0),
    ("ODTDAT", 'A', 6, 0),
    ("ODODMN", 'A', 2, 0),
    ("ODCPVR", 'A', 6, 0),
    ("ODCVRM", 'A', 6, 0),
    ("ODPVRM", 'A', 6, 0),
    ("ODCPRS", 'A', 1, 0),
    ("ODOASP", 'A', 1, 0),
    ("ODAAPI", 'A', 1, 0),
    ("ODAPIC", 'A', 1, 0),
    ("ODUATR", 'A', 10, 0),
    ("ODACEN", 'A', 1, 0),
    ("ODADAT", 'A', 6, 0),
    ("ODATIM", 'A', 6, 0),
    ("ODAUDT", 'A', 10, 0),
    ("ODSIZU", 'P', 10, 0),
    ("ODBPUN", 'P', 10, 0),
    ("ODPGP", 'A', 10, 0),
    ("ODSSQL", 'S', 10, 0),
    ("ODOSIG", 'A', 1, 0),
    ("ODJRST", 'A', 1, 0),
    ("ODJRNM", 'A', 10, 0),
    ("ODJRLB", 'A', 10, 0),
    ("ODJRIM", 'A', 1, 0),
    ("ODJREN", 'A', 1, 0),
    ("ODJRCN", 'A', 1, 0),
    ("ODJRDT", 'A', 6, 0),
    ("ODJRTI", 'A', 6, 0),
    ("ODSSZU", 'P', 10, 0),
    ("ODSBPU", 'P', 10, 0),
    ("ODASPL", 'S', 5, 0),
    ("ODLASN", 'S', 5, 0),
    ("ODOADN", 'A', 10, 0),
    ("ODLADN", 'A', 10, 0),
    ("ODSSIG", 'A', 1, 0),
    ("ODMSIG", 'A', 1, 0),
    ("ODASSZ", 'P', 15, 0),
    ("ODALGN", 'A', 1, 0),
    ("ODOADG", 'A', 10, 0),
    ("ODLADG", 'A', 10, 0),
    ("ODSJRA", 'A', 10, 0),
    ("ODSJRL", 'A', 10, 0),
    ("ODSJRD", 'A', 10, 0),
    ("ODSJRG", 'A', 10, 0),
    ("ODJRJF", 'A', 1, 0),
    ("ODDTL", 'A', 10, 0),
    ("ODAUTC", 'A', 10, 0),
    ("ODBDID", 'A', 16, 0),
];

/// The model's DDS source (fixed form: name at column 19, length ending
/// at column 34, type at 35, decimal positions at 36-37).
fn dds(m: &Model) -> String {
    let mut s = format!("     A          R {:<10}\n", m.format);
    for (name, ty, len, dec) in m.fields {
        let decs = if *ty == 'A' {
            "  ".to_string()
        } else {
            format!("{dec:>2}")
        };
        s.push_str(&format!("     A            {name:<10}{len:>6}{ty}{decs}\n"));
    }
    s
}

/// The model's file description, named `name`.
fn description(m: &Model, name: &str) -> Result<crate::dds::PhysicalFile> {
    let mut pf = crate::dds::parse_pf(name, &dds(m))?;
    pf.text = m.text.to_string();
    Ok(pf)
}

/// At catalog initialization: the model files in QSYS, when missing.
pub async fn init(client: &Client) -> Result<()> {
    for m in [&QADSPOBJ, &QAFDMBRL] {
        let there = client
            .query_opt(
                "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*FILE'",
                &[&m.file],
            )
            .await?
            .is_some();
        if there {
            continue;
        }
        let pf = description(m, m.file)?;
        client.batch_execute("BEGIN").await?;
        match catalog::create_physical_file_in(client, "QSYS", m.file, &pf).await {
            Ok(()) => client.batch_execute("COMMIT").await?,
            Err(e) => {
                client.batch_execute("ROLLBACK").await?;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A record for an output file: values by field name (fields not given
/// are blank or zero).
pub type Record = Vec<(&'static str, String)>;

/// The models' century digit for a four-digit year: 0 for 19xx, 1 for
/// 20xx.
pub fn century(year: &str) -> String {
    if year.starts_with("19") {
        "0".into()
    } else {
        "1".into()
    }
}

/// Write `records` into the output file `outfile` (LIB/FILE, the library
/// *LIBL or *CURLIB too), member option `option` (*REPLACE clears it
/// first, *ADD adds). A file that is not there is made like the model,
/// with the model's record format.
pub async fn write(
    job: &crate::commands::Job<'_>,
    outfile: &str,
    option: &str,
    model: &Model,
    records: &[Record],
) -> Result<()> {
    let q = outfile.trim().to_ascii_uppercase();
    let (label, name) = match q.split_once('/') {
        Some((l, n)) => (l.to_string(), n.to_string()),
        None => ("*LIBL".to_string(), q.clone()),
    };
    if name.is_empty() || name.starts_with('*') {
        return Err(crate::cl::value_error(
            "OUTFILE needs the name of a database file.",
        ));
    }
    let client = job.client;
    let libs: Vec<String> = match label.as_str() {
        "*LIBL" => job.library_list(),
        "*CURLIB" => vec![job.current_library()],
        l => vec![l.to_string()],
    };
    let found = catalog::resolve_file(client, &libs, &name).await.ok();
    let (lib, file) = match found {
        Some(f) => (f.library.clone(), f.file),
        None => {
            let lib = match label.as_str() {
                "*LIBL" | "*CURLIB" => job.current_library(),
                l => l.to_string(),
            };
            if !catalog::library_exists(client, &lib).await? {
                bail!("CPF9810: There is no library {lib}.");
            }
            let pf = description(model, &name)?;
            let mut c = crate::runtime::connect_db(&job.env.database_url).await?;
            catalog::create_physical_file(&mut c, &lib, &pf).await?;
            crate::authority::set_owner(client, &lib, &name, "*FILE", &job.user).await?;
            (lib, pf)
        }
    };
    let table = format!("{}.{}", ident(&lib), ident(&name));
    if !option.trim().eq_ignore_ascii_case("*ADD") {
        client
            .batch_execute(&format!("DELETE FROM {table}"))
            .await?;
    }
    let cols: Vec<String> = file.fields.iter().map(|f| f.column()).collect();
    let casts: Vec<String> = file
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| format!("CAST(${}::text AS {})", i + 1, f.sql_type()))
        .collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.join(", "),
        casts.join(", ")
    );
    for r in records {
        let values: Vec<String> = file
            .fields
            .iter()
            .map(|f| {
                let given = r
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&f.name))
                    .map(|(_, v)| v.clone());
                if f.data_type == crate::dds::DataType::Char {
                    let v = given.unwrap_or_default();
                    let n = f.length as usize;
                    let mut v: String = v.chars().take(n).collect();
                    while v.chars().count() < n {
                        v.push(' ');
                    }
                    v
                } else {
                    given
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| "0".into())
                }
            })
            .collect();
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        client.execute(sql.as_str(), &params).await?;
    }
    Ok(())
}
