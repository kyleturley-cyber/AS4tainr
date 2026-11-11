//! DSPMOD: a module's attributes, exported and imported symbols, on a
//! display, printed (OUTPUT(*PRINT): IBM i 7.5's report, a page for each
//! panel), or into an output file (OUTPUT(*OUTFILE), DETAIL(*BASIC),
//! (*EXPORT) or (*IMPORT): the formats QBNDMBAS, QBNDMEXP and QBNDMIMP
//! of `outfile.rs`).
//!
//! Observed on PUB400 (2026-11-10, tools/golden/verify74.txt) for a CL
//! module made with CRTCLMOD from a stream file:
//! - u74_exp, u74_bas, u74_imp: the file made gets CPF9861 and CPF9862,
//!   and nothing else is sent (no CPF3030).
//! - u74_exprows: one export, the module's procedure, named as the
//!   module: symbol type '0' (procedure), ARGOPT *NO.
//! - u74_improws: the ILE CL run time's six procedures (`CL_IMPORTS`).
//! - u74_basrows and u74_print: the *BASIC values of a CL module.
//!
//! Code for IBM i lists a module's exports with DETAIL(*EXPORT)
//! OUTPUT(*OUTFILE) (IBMiContent.ts getModuleExports).

use crate::authority::Aut;
use crate::cl::Command;
use crate::commands::{Job, ListDisplay, Outcome};
use crate::report::at;
use anyhow::{bail, Result};

/// The procedures every ILE CL module imports, with their argument
/// optimization (verify74 u74_improws and u74_print, in that order).
const CL_IMPORTS: [(&str, &str); 6] = [
    ("CEEGOTO", "*NO"),
    ("Qcl_CHKBI", "*UNKNOWN"),
    ("Qcl_LkLDA", "*NO"),
    ("Q LE leDefaultEh2", "*UNKNOWN"),
    ("Q LE leBdyCh2", "*UNKNOWN"),
    ("Q LE leBdyEpilog2", "*NO"),
];

/// The release a module is made on and for, and the earliest it can be
/// restored to (u74_basrows BACMCO, BACMCF, BACERT).
const RELEASE: &str = "V7R5M0";

/// The procedures an ILE RPG module without a main procedure imports
/// after its own (PUB400 2026-11-10, verify76 u76_improws and u76_print:
/// a NOMAIN module with one exported procedure and one prototyped call).
const RPG_IMPORTS: [(&str, &str); 6] = [
    ("_QRNX_SUBP_EXCP", "*NO"),
    ("_QRNX_INIT_H", "*UNKNOWN"),
    ("_QRNX_INIT", "*NO"),
    ("_QRNX_GET_ASSOC_CCSID", "*NO"),
    ("_QRNX_SIGNAL_EXCP", "*NO"),
    ("Q LE leDefaultEh2", "*UNKNOWN"),
];

/// The procedures an ILE RPG NOMAIN module has besides its own
/// (verify76 u76_print, *PROCLIST).
const RPG_NOMAIN_PROCS: [&str; 2] = ["_QRNI_NOMAIN", "_QRNI_SOFT_ERR"];

/// The binding directories an ILE RPG module names for binding
/// (verify76 u76_print, *REFSYSOBJ).
const RPG_BNDDIRS: [&str; 3] = ["QRNXLE", "QILE", "QUSAPIBD"];

/// A symbol a module exports or imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// Data, not a procedure.
    pub data: bool,
    /// Argument optimization: *NO, *YES or *UNKNOWN.
    pub argopt: String,
}

/// What DSPMOD shows of a module.
#[derive(Debug, Clone, Default)]
pub struct ModuleFacts {
    pub library: String,
    pub name: String,
    pub attribute: String,
    /// Created and source changed: (yymmdd, hhmmss) in UTC, and the
    /// four-digit year's century digit.
    pub created: (String, String, String),
    pub src_changed: Option<(String, String, String)>,
    pub src_file: String,
    pub src_lib: String,
    pub src_mbr: String,
    pub src_stmf: String,
    pub owner: String,
    pub ccsid: u32,
    pub text: String,
    pub exports: Vec<Symbol>,
    pub imports: Vec<Symbol>,
    /// An RPG module has a main procedure (not NOMAIN).
    pub has_main: bool,
}

impl ModuleFacts {
    fn cl(&self) -> bool {
        self.attribute == "CLLE"
    }

    /// The program entry procedure: a CL module's is _CL_PEP
    /// (u74_basrows BAPEPN), a NOMAIN RPG module has none (u76_basrows
    /// *NONE); an RPG main procedure's is not known here.
    fn pep(&self) -> &'static str {
        if self.cl() {
            "_CL_PEP"
        } else if !self.has_main {
            "*NONE"
        } else {
            ""
        }
    }

    /// The fewest and most parameters (u74_basrows: 0 and 255 for CL;
    /// u76_basrows: -1 and -1 for a module with no entry procedure, whose
    /// report has no line for them).
    fn parms(&self) -> (i32, i32) {
        if self.cl() {
            (0, 255)
        } else if !self.has_main {
            (-1, -1)
        } else {
            (0, 0)
        }
    }

    /// The highest optimization the module can be changed to: *FULL for
    /// RPG (u76_basrows BAOPLX), *NONE for CL (u74_basrows).
    fn max_opt(&self) -> &'static str {
        if self.cl() {
            "*NONE"
        } else {
            "*FULL"
        }
    }

    /// The module's procedures (*PROCLIST): its exported ones, and an
    /// RPG NOMAIN module's run-time ones.
    fn procedures(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .exports
            .iter()
            .filter(|s| !s.data)
            .map(|s| s.name.clone())
            .collect();
        if !self.cl() && !self.has_main {
            v.extend(RPG_NOMAIN_PROCS.iter().map(|p| p.to_string()));
        }
        v
    }

    /// The system objects the module refers to for binding (*REFSYSOBJ).
    fn bind_objects(&self) -> &'static [&'static str] {
        if self.cl() {
            &[]
        } else {
            &RPG_BNDDIRS
        }
    }
}

/// The text label of a report line: `text` at `col`, then leader dots
/// on the columns of `last_dot`'s parity (the first two past the text),
/// then a colon two columns after the last dot (u74_print).
fn label(col: usize, text: &str, last_dot: usize) -> String {
    let mut s = " ".repeat(col - 1);
    s.push_str(text);
    let mut c = col + text.chars().count() + 1;
    if c % 2 != last_dot % 2 {
        c += 1;
    }
    while s.chars().count() < c - 1 {
        s.push(' ');
    }
    while c <= last_dot {
        s.push('.');
        if c < last_dot {
            s.push(' ');
        }
        c += 2;
    }
    s.push_str(" :");
    s
}

/// A detail line: label at column 4 (2 for a group heading's own
/// value-less lines), the value at 65 (u74_print).
fn line(text: &str, value: &str) -> String {
    let l = label(4, text, 59);
    if value.is_empty() {
        l
    } else {
        format!("{l:<64}{value}")
    }
}

/// "yymmdd" and "hhmmss" as a report shows them (the job's date format
/// and separators: `report::Stamp`), date and time two columns apart.
fn shown_when(yymmdd: &str, hhmmss: &str) -> String {
    let n =
        |s: &str, a: usize| -> u32 { s.get(a..a + 2).and_then(|v| v.parse().ok()).unwrap_or(0) };
    let st = crate::report::Stamp::of(
        (2000 + n(yymmdd, 0) as i32, n(yymmdd, 2), n(yymmdd, 4)),
        (n(hhmmss, 0), n(hhmmss, 2), n(hhmmss, 4)),
    );
    format!("{}  {}", st.date, st.time)
}

/// The report's pages for the details asked (*BASIC, *EXPORT, *IMPORT;
/// *ALL is those): each page starts with the style B header and the
/// module's three lines (u74_print).
pub fn report(m: &ModuleFacts, details: &[String], stamp: &crate::report::Stamp) -> Vec<String> {
    let system = crate::report::system_name();
    let mut pages: Vec<(&str, Vec<String>)> = Vec::new();
    let all = details.iter().any(|d| d == "*ALL");
    let want = |d: &str| all || details.iter().any(|x| x == d);
    let yes_no = |b: bool| if b { "*YES" } else { "*NO" };
    if want("*BASIC") {
        let when = |d: &Option<(String, String, String)>| {
            d.as_ref()
                .map(|d| shown_when(&d.1, &d.2))
                .unwrap_or_default()
        };
        let src_lib = if m.src_file.is_empty() {
            ""
        } else {
            &m.src_lib
        };
        pages.push((
            "*BASIC",
            vec![
                at(&[(2, "Module information:")]),
                line("Module creation date/time", &when(&Some(m.created.clone()))),
                line("Source file", &m.src_file),
                label(6, "Library", 59) + &pad_value(src_lib),
                line("Source member", &m.src_mbr),
                line("Source stream file", &m.src_stmf),
                line("Source file change date/time", &when(&m.src_changed)),
                line("Owner", &m.owner),
                line("Coded character set identifier", &m.ccsid.to_string()),
                line("Text description", &m.text),
                line("Creation data", "*YES"),
                line("Intermediate language data", "*NO"),
                line(
                    "Allow RTVCLSRC (CL module)",
                    if m.cl() { "*YES" } else { "*NO" },
                ),
                line("Sort sequence table", "*HEX"),
                line("Language identifier", "*JOBRUN"),
            ],
        ));
        let (lo, hi) = m.parms();
        let parms = format!("{lo:<8}{hi}");
        let mut page = vec![
            line("Optimization level", "*NONE"),
            line("Maximum optimization level", m.max_opt()),
            line("Debug data", "*YES"),
            line("Compressed", yes_no(false)),
            line("Program entry procedure name", m.pep()),
        ];
        if lo >= 0 {
            page.push(line("Number of parameters", &parms));
        }
        pages.push((
            "*BASIC",
            vec![
                line("Module state", "*USER"),
                line("Module domain", "*SYSTEM"),
                line(
                    "Number of exported defined symbols",
                    &m.exports.len().to_string(),
                ),
                line(
                    "Number of imported (unresolved) symbols",
                    &m.imports.len().to_string(),
                ),
                line("Profiling data", "*NOCOL"),
                line("Enable performance collection", "*PEP"),
                line("Teraspace storage enabled", "*YES"),
                line("Storage model", "*INHERIT"),
            ],
        ));
        // The second page: its first lines, then the rest.
        let n = pages.len() - 1;
        let rest = std::mem::take(&mut pages[n].1);
        page.extend(rest);
        pages[n].1 = page;
        pages.push((
            "*BASIC",
            vec![
                line("Licensed Internal Code options", "*NONE"),
                at(&[(2, "Module compatibility:")]),
                line("Module created on", RELEASE),
                line("Module created for", RELEASE),
                line("Earliest release module can be restored to", RELEASE),
                line("Conversion required", "*NO"),
                line("Conversion details", "*COMMON"),
            ],
        ));
    }
    if want("*SIZE") {
        let size = |label_: &str, cur: &str, max: &str, name: Option<&str>| {
            let mut v = vec![
                at(&[(4, label_)]),
                format!("{:<44}{cur}", label(6, "Current size", 39)),
                format!("{:<44}{max}", label(6, "Maximum size limit", 39)),
            ];
            if let Some(n) = name {
                v.push(
                    format!("{:<44}{n}", label(6, "Procedure name", 39))
                        .trim_end()
                        .to_string(),
                );
            }
            v
        };
        let mut first = vec![at(&[(
            2,
            "Module size (decompressed) and limit information:",
        )])];
        let mut second = Vec::new();
        for (i, (l, _, max)) in SIZES.iter().enumerate() {
            let part = size(l, "0", &max.to_string(), None);
            if i < 17 {
                first.extend(part);
            } else {
                second.extend(part);
            }
        }
        second.push(at(&[(
            2,
            "Procedure size (decompressed) and limit information:",
        )]));
        for (l, _, max) in PROC_SIZES.iter() {
            second.extend(size(l, "0", &max.to_string(), Some("")));
        }
        pages.push(("*SIZE", first));
        pages.push(("*SIZE", second));
    }
    let symbols = |title: (usize, &str), list: &[Symbol]| {
        let mut v = vec![
            at(&[title]),
            at(&[(2, "Symbol"), (51, "Symbol")]),
            at(&[(2, "Name"), (51, "Type"), (70, "ARGOPT")]),
        ];
        for s in list {
            let ty = if s.data { "DATA" } else { "PROCEDURE" };
            v.push(at(&[(2, &s.name), (51, ty), (70, &s.argopt)]));
        }
        v
    };
    if want("*EXPORT") {
        pages.push((
            "*EXPORT",
            symbols((54, "Exported defined symbols:"), &m.exports),
        ));
    }
    if want("*IMPORT") {
        pages.push((
            "*IMPORT",
            symbols((52, "Imported (unresolved) symbols:"), &m.imports),
        ));
    }
    if want("*PROCLIST") {
        let mut v = vec![
            at(&[(59, "Procedure list:")]),
            at(&[(2, "Procedure"), (51, "Procedure")]),
            at(&[(2, "Name"), (51, "Type"), (70, "ARGOPT")]),
        ];
        for p in m.procedures() {
            v.push(at(&[(2, &p), (51, "REGULAR"), (70, "*NO")]));
        }
        pages.push(("*PROCLIST", v));
    }
    if want("*REFSYSOBJ") {
        let mut v = vec![
            at(&[(47, "Reference system objects for bind time:")]),
            at(&[(2, "Object"), (17, "Library"), (32, "Type")]),
        ];
        for o in m.bind_objects() {
            v.push(at(&[(2, o), (17, "*LIBL"), (32, "*BNDDIR")]));
        }
        pages.push(("*REFSYSOBJ", v));
    }
    if want("*COPYRIGHT") {
        pages.push((
            "*COPYRIGHT",
            vec![
                at(&[(56, "Copyright information:")]),
                at(&[(4, "(No copyright information)")]),
            ],
        ));
    }
    let mut out = Vec::new();
    for (i, (detail, body)) in pages.iter().enumerate() {
        let [a, b] = crate::report::header_b(
            46,
            "Display Module Information",
            &system,
            stamp,
            i as u32 + 1,
        );
        out.push(a);
        out.push(b);
        out.push(at(&[
            (1, &label(2, "Module", 31)),
            (37, &m.name),
            (69, label(69, "Library", 98).trim_start()),
            (104, &m.library),
        ]));
        out.push(label(2, "Detail", 30) + &format!("   {detail}"));
        out.push(label(2, "Module Attribute", 30) + &format!("   {}", m.attribute));
        out.extend(body.iter().map(|l| l.trim_end().to_string()));
    }
    out.push(at(&[(
        41,
        "* * * * *   E N D   O F   L I S T I N G   * * * * *",
    )]));
    out
}

fn pad_value(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        format!("   {v}")
    }
}

/// The records of DETAIL(*EXPORT) or (*IMPORT): `now` is the display
/// date and time (century digit, yymmdd, hhmmss).
pub fn symbol_records(
    m: &ModuleFacts,
    prefix: &'static str,
    list: &[Symbol],
    now: &(String, String, String),
    system: &str,
) -> Vec<crate::outfile::Record> {
    let f = |s: &str| -> &'static str {
        // The field names are the prefix and the suffix.
        match (prefix, s) {
            ("EX", "DCEN") => "EXDCEN",
            ("EX", "DDAT") => "EXDDAT",
            ("EX", "DTIM") => "EXDTIM",
            ("EX", "LBNM") => "EXLBNM",
            ("EX", "MONM") => "EXMONM",
            ("EX", "MOAT") => "EXMOAT",
            ("EX", "MOSY") => "EXMOSY",
            ("EX", "SYNM") => "EXSYNM",
            ("EX", "SYTY") => "EXSYTY",
            ("EX", "OPPP") => "EXOPPP",
            (_, "DCEN") => "IMDCEN",
            (_, "DDAT") => "IMDDAT",
            (_, "DTIM") => "IMDTIM",
            (_, "LBNM") => "IMLBNM",
            (_, "MONM") => "IMMONM",
            (_, "MOAT") => "IMMOAT",
            (_, "MOSY") => "IMMOSY",
            (_, "SYNM") => "IMSYNM",
            (_, "SYTY") => "IMSYTY",
            _ => "IMOPPP",
        }
    };
    list.iter()
        .map(|s| {
            vec![
                (f("DCEN"), now.0.clone()),
                (f("DDAT"), crate::outfile::job_date(&now.1)),
                (f("DTIM"), now.2.clone()),
                (f("LBNM"), m.library.clone()),
                (f("MONM"), m.name.clone()),
                (f("MOAT"), m.attribute.clone()),
                (f("MOSY"), system.to_string()),
                (f("SYNM"), s.name.clone()),
                (f("SYTY"), if s.data { "1" } else { "0" }.to_string()),
                (f("OPPP"), s.argopt.clone()),
            ]
        })
        .collect()
}

/// The record of DETAIL(*BASIC) (u74_basrows).
pub fn basic_record(
    m: &ModuleFacts,
    now: &(String, String, String),
    system: &str,
) -> crate::outfile::Record {
    let (lo, hi) = m.parms();
    let (scen, sdat, stim) = m.src_changed.clone().unwrap_or_default();
    let rel = RELEASE.to_string();
    let s = |v: &str| v.to_string();
    vec![
        ("BADCEN", now.0.clone()),
        ("BADDAT", crate::outfile::job_date(&now.1)),
        ("BADTIM", now.2.clone()),
        ("BALBNM", m.library.clone()),
        ("BAMONM", m.name.clone()),
        ("BAMOAT", m.attribute.clone()),
        ("BAMOSY", system.to_string()),
        ("BACCEN", m.created.0.clone()),
        ("BACDAT", crate::outfile::job_date(&m.created.1)),
        ("BACTIM", m.created.2.clone()),
        ("BASRCF", m.src_file.clone()),
        (
            "BASRCL",
            if m.src_file.is_empty() {
                String::new()
            } else {
                m.src_lib.clone()
            },
        ),
        ("BASRCM", m.src_mbr.clone()),
        ("BASRCC", scen),
        (
            "BASRCD",
            if sdat.is_empty() {
                sdat
            } else {
                crate::outfile::job_date(&sdat)
            },
        ),
        ("BASRCT", stim),
        ("BAMOWN", m.owner.clone()),
        ("BACCSI", m.ccsid.to_string()),
        ("BATEXT", m.text.clone()),
        ("BAMIT", s("*YES")),
        ("BASRTT", s("*HEX")),
        ("BALGID", s("*JOBRUN")),
        ("BAOPLV", s("*NONE")),
        ("BAOPLX", s(m.max_opt())),
        ("BADBLV", s("*YES")),
        ("BACOMP", s("*NO")),
        ("BAPEPN", s(m.pep())),
        ("BAMINP", lo.to_string()),
        ("BAMAXP", hi.to_string()),
        ("BASTAT", s("*USER")),
        ("BADOMN", s("*SYSTEM")),
        ("BANEXS", m.exports.len().to_string()),
        ("BANIMS", m.imports.len().to_string()),
        ("BACMCO", rel.clone()),
        ("BACMCF", rel.clone()),
        ("BACERT", rel),
        ("BASQLN", s("0")),
        ("BASEPL", s("*PEP")),
        ("BASCNV", s("*NO")),
        ("BAPRFD", s("*NOCOL")),
        ("BAILD", s("*NO")),
        ("BATERA", s("*YES")),
        ("BALICO", s("*NONE")),
        ("BASTGM", s("*INHERIT")),
        ("BASQSZ", s("0")),
        ("BACNVD", s("*COMMON")),
        ("BAARCS", s(if m.cl() { "*YES" } else { "*NO" })),
        ("BASCCD", m.ccsid.to_string()),
        ("BASLEN", m.src_stmf.chars().count().to_string()),
        ("BASTMF", m.src_stmf.clone()),
    ]
}

/// The record of DETAIL(*SIZE): the module's names; as4tainr has no
/// sizes of compiled code and data to give, so they are zero
/// (u75_sizeffd's format; values not captured).
pub fn size_record(
    m: &ModuleFacts,
    now: &(String, String, String),
    system: &str,
) -> crate::outfile::Record {
    vec![
        ("SIDCEN", now.0.clone()),
        ("SIDDAT", crate::outfile::job_date(&now.1)),
        ("SIDTIM", now.2.clone()),
        ("SILBNM", m.library.clone()),
        ("SIMONM", m.name.clone()),
        ("SIMOAT", m.attribute.clone()),
        ("SIMOSY", system.to_string()),
    ]
    .into_iter()
    .chain(SIZES.iter().map(|(_, f, max)| (*f, max.to_string())))
    .chain(PROC_SIZES.iter().map(|(_, f, max)| (*f, max.to_string())))
    .chain([("SISTSL8", "2147483648".to_string())])
    .collect()
}

/// DETAIL(*SIZE)'s sizes: the report's label, the outfile's limit field
/// and the limit (PUB400 2026-11-10, verify76 u76_print and u76_sizerows:
/// IBM i 7.5's limits; the current sizes are as4tainr's, 0, as it has no
/// compiled module to measure).
const SIZES: [(&str, &str, u64); 23] = [
    ("Total module size (KB):", "SITMSL", 268435456),
    ("Procedures and constants (KB):", "SIPCOL", 268435387),
    ("Debug space (bytes):", "SIDBSL", 16777216),
    ("Associated space (bytes):", "SIASPL", 16777216),
    ("Module constants (bytes):", "SIMOCL", 16772608),
    ("Static storage (bytes):", "SISTSL", 2147483648),
    ("Dictionary mapping table (bytes):", "SIDMTL", 16777184),
    ("Exception mapping table (bytes):", "SIEHTL", 16777184),
    (
        "Exception mapping table list area (bytes):",
        "SIEMLL",
        16777184,
    ),
    (
        "Binding specifications component (bytes):",
        "SIBNSL",
        16777184,
    ),
    ("String directory component (bytes):", "SISTDL", 16777184),
    ("Dictionary component (bytes):", "SIDICL", 16777184),
    ("Instructions component (bytes):", "SIINCL", 16777184),
    ("Initialization component (bytes):", "SIIZCL", 16777184),
    ("Alias component (bytes):", "SIALIL", 16777184),
    ("Type information component (bytes):", "SITYPL", 16777184),
    ("Literal pool component (bytes):", "SILITL", 16777184),
    ("Static storage work area (bytes):", "SISWAL", 16777184),
    ("Binding work area (bytes):", "SIBWAL", 16777184),
    ("Number of auxiliary storage segments:", "SIAXSL", 16384),
    ("Number of static storage allocations:", "SISSFL", 190649),
    ("Number of procedures:", "SIPROL", 116508),
    ("Number of copyrights:", "SICOPL", 1024),
];

/// The procedure sizes of DETAIL(*SIZE) (u76_print, u76_sizerows).
const PROC_SIZES: [(&str, &str, u64); 6] = [
    ("Automatic storage allocation:", "SIPASL", 16776704),
    (
        "Licensed Internal Code stack allocation:",
        "SIPVSL",
        16777184,
    ),
    ("Debug statement mapping table:", "SIPDSL", 16777184),
    ("Exception statement mapping table:", "SIPESL", 16777184),
    ("Machine instruction range mapping:", "SIPMIL", 16777184),
    ("Largest procedure:", "SIPLPL", 16711616),
];

impl Job<'_> {
    pub(crate) async fn dspmod_command(&mut self, cmd: &Command) -> Result<Option<Outcome>> {
        Ok(Some(match cmd.def.name {
            "DSPMOD" => self.dspmod(cmd).await?,
            // The reply list's commands, dispatched here: one more arm in
            // Job::run's chain made its future overflow the stack of
            // deeply nested commands (rla verify12_message_keys...).
            "ADDRPYLE" | "CHGRPYLE" | "RMVRPYLE" | "WRKRPYLE" => {
                return Box::pin(self.rpyl_command(cmd)).await
            }
            _ => return Ok(None),
        }))
    }

    /// What DSPMOD shows of the module MODULE names.
    async fn module_facts(&self, spec: &str) -> Result<ModuleFacts> {
        let (lib, name) = self.ile_object(spec, "*MODULE", Aut::Use).await?;
        let copy = crate::ile::find_module(self.client, &self.library_list(), &lib, &name).await?;
        let mut libl = self.library_list();
        if !libl.contains(&lib) {
            libl.insert(0, lib.clone());
        }
        let info = crate::ile::module_info(self.client, &libl, &copy).await?;
        let cl = copy.attribute == "CLLE";
        let exports = info
            .exports
            .iter()
            .map(|e| Symbol {
                name: info.spelled.get(e).cloned().unwrap_or_else(|| e.clone()),
                data: info.data.contains(e),
                argopt: "*NO".into(),
            })
            .collect();
        // The module's own imports (ARGOPT *NO: verify76 u76_improws for
        // an RPG prototype, u76_climp for a CL CALLPRC) among the run
        // time's: a CL module's after the first three of CL_IMPORTS
        // (u76_climp), an RPG module's before RPG_IMPORTS (u76_improws).
        let own: Vec<Symbol> = info
            .imports
            .iter()
            .map(|i| Symbol {
                name: i.clone(),
                data: info.data.contains(i),
                argopt: "*NO".into(),
            })
            .collect();
        let rt = |l: &[(&str, &str)]| -> Vec<Symbol> {
            l.iter()
                .map(|(n, a)| Symbol {
                    name: n.to_string(),
                    data: false,
                    argopt: a.to_string(),
                })
                .collect()
        };
        let imports: Vec<Symbol> = if cl {
            let mut v = rt(&CL_IMPORTS[..3]);
            v.extend(own);
            v.extend(rt(&CL_IMPORTS[3..]));
            v
        } else {
            let mut v = own;
            v.extend(rt(&RPG_IMPORTS));
            v
        };
        let when = "CASE WHEN t < '2000-01-01' THEN '0' ELSE '1' END, to_char(t, 'YYMMDD'), to_char(t, 'HH24MISS')";
        let r = self
            .client
            .query_one(
                &format!(
                    "SELECT o.owner, o.text, m.src_file, m.src_lib, m.src_mbr, m.src_stmf,
                            c1, c2, c3, s1, s2, s3
                       FROM as4tainr.objects o
                       JOIN as4tainr.modules m ON m.library = o.library AND m.name = o.name
                       CROSS JOIN LATERAL (SELECT {when} FROM (SELECT o.created AT TIME ZONE 'UTC' AS t) x) AS c(c1, c2, c3)
                       LEFT JOIN LATERAL (SELECT {when} FROM (SELECT m.src_changed AT TIME ZONE 'UTC' AS t) x
                                           WHERE t IS NOT NULL) AS s(s1, s2, s3) ON true
                      WHERE o.library = $1 AND o.name = $2 AND o.objtype = '*MODULE'"
                ),
                &[&lib, &name],
            )
            .await?;
        let s1: Option<String> = r.get(9);
        let ccsid = crate::jobattr::get(&self.job_key(), "CCSID")
            .and_then(|c| c.parse::<u32>().ok())
            .filter(|&c| c != 65535)
            .unwrap_or(37);
        let owner: Option<String> = r.get(0);
        Ok(ModuleFacts {
            library: crate::qtemp::logical(&lib),
            name,
            attribute: copy.attribute.clone(),
            created: (r.get(6), r.get(7), r.get(8)),
            src_changed: s1.map(|c| (c, r.get(10), r.get(11))),
            src_file: r.get(2),
            src_lib: r.get(3),
            src_mbr: r.get(4),
            src_stmf: r.get(5),
            owner: owner.unwrap_or_default(),
            ccsid,
            text: r.get(1),
            exports,
            imports,
            has_main: info.has_main,
        })
    }

    async fn dspmod(&mut self, cmd: &Command) -> Result<Outcome> {
        let details: Vec<String> = cmd
            .list("DETAIL")
            .iter()
            .map(|d| d.trim().to_ascii_uppercase())
            .filter(|d| !d.is_empty())
            .collect();
        let details = if details.is_empty() {
            vec!["*BASIC".to_string()]
        } else {
            details
        };
        if cmd.get("OUTPUT").eq_ignore_ascii_case("*OUTFILE") {
            let outfile = cmd.get("OUTFILE").trim().to_string();
            if outfile.is_empty() {
                bail!(
                    "CPD9861: OUTPUT(*OUTFILE) was given without OUTFILE, the file to receive the records.\n{}",
                    crate::cl::CPF0006
                );
            }
            if details.len() != 1 {
                return Err(crate::cl::value_error(
                    "OUTPUT(*OUTFILE) takes one DETAIL value.",
                ));
            }
            let option = cmd
                .list("OUTMBR")
                .get(1)
                .cloned()
                .unwrap_or_else(|| "*REPLACE".into());
            let r = self
                .client
                .query_one(
                    "SELECT CASE WHEN t < '2000-01-01' THEN '0' ELSE '1' END, to_char(t, 'YYMMDD'), to_char(t, 'HH24MISS')
                       FROM (SELECT now() AT TIME ZONE 'UTC' AS t) x",
                    &[],
                )
                .await?;
            let now = (r.get(0), r.get(1), r.get(2));
            let system: String = crate::report::system_name().chars().take(8).collect();
            let model = match details[0].as_str() {
                "*EXPORT" => &crate::outfile::DSPMOD_EXPORT,
                "*IMPORT" => &crate::outfile::DSPMOD_IMPORT,
                "*BASIC" => &crate::outfile::DSPMOD_BASIC,
                "*SIZE" => &crate::outfile::DSPMOD_SIZE,
                d => {
                    return Err(crate::cl::value_error(&format!(
                        "DETAIL({d}) with OUTPUT(*OUTFILE) is not available here."
                    )))
                }
            };
            // The file is made (or its member cleared) before the module
            // is looked for: a module that is not there leaves an empty
            // file, its CPF9861 and CPF9862 before the CPF9801 (PUB400
            // 2026-11-10, verify75 u75_exp).
            let m = match self.module_facts(cmd.get("MODULE")).await {
                Ok(m) => m,
                Err(e) => {
                    let w = crate::outfile::write(self, &outfile, &option, model, &[]).await?;
                    let mut lines = w.messages;
                    lines.push(format!("{e:#}"));
                    bail!(lines.join("\n"));
                }
            };
            let recs = match details[0].as_str() {
                "*EXPORT" => symbol_records(&m, "EX", &m.exports, &now, &system),
                "*IMPORT" => symbol_records(&m, "IM", &m.imports, &now, &system),
                "*SIZE" => vec![size_record(&m, &now, &system)],
                _ => vec![basic_record(&m, &now, &system)],
            };
            let w = crate::outfile::write(self, &outfile, &option, model, &recs).await?;
            return Ok(Outcome::Messages(w.messages));
        }
        let m = self.module_facts(cmd.get("MODULE")).await?;
        let print = report(&m, &details, &crate::report::Stamp::now());
        let fmt = |n: &str, t: &str, a: &str| format!("{n:<48}  {t:<10}  {a}");
        let mut rows = Vec::new();
        for (kind, list) in [("export", &m.exports), ("import", &m.imports)] {
            for s in list.iter() {
                let ty = if s.data { "DATA" } else { "PROCEDURE" };
                rows.push(fmt(&s.name, ty, &format!("{} {kind}", s.argopt)));
            }
        }
        Ok(Outcome::Display(ListDisplay {
            title: "Display Module Information".into(),
            info: vec![
                ("Module".into(), m.name.clone()),
                ("Library".into(), m.library.clone()),
                ("Module attribute".into(), m.attribute.clone()),
                ("Owner".into(), m.owner.clone()),
                (
                    "Exported defined symbols".into(),
                    m.exports.len().to_string(),
                ),
                ("Imported symbols".into(), m.imports.len().to_string()),
            ],
            heading: fmt("Symbol", "Type", "ARGOPT"),
            rows,
            print,
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cl_module() -> ModuleFacts {
        ModuleFacts {
            library: "AS4TRAINR1".into(),
            name: "GLD74M".into(),
            attribute: "CLLE".into(),
            created: ("1".into(), "261110".into(), "185128".into()),
            src_changed: Some(("1".into(), "261110".into(), "185128".into())),
            src_stmf: "/tmp/gld74.clle".into(),
            owner: "AS4TRAINR".into(),
            ccsid: 273,
            exports: vec![Symbol {
                name: "GLD74M".into(),
                data: false,
                argopt: "*NO".into(),
            }],
            imports: CL_IMPORTS
                .iter()
                .map(|(n, a)| Symbol {
                    name: n.to_string(),
                    data: false,
                    argopt: a.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn labels_with_leader_dots() {
        // u74_print, lines as printed.
        assert_eq!(label(2, "Module", 31), " Module . . . . . . . . . . . . :");
        assert_eq!(label(2, "Detail", 30), " Detail  . . . . . . . . . . . :");
        assert_eq!(
            label(2, "Module Attribute", 30),
            " Module Attribute  . . . . . . :"
        );
        assert_eq!(
            line("Owner", "AS4TRAINR"),
            "   Owner  . . . . . . . . . . . . . . . . . . . . . . . . . :   AS4TRAINR"
        );
        assert_eq!(
            line("Source stream file", "/tmp/gld74.clle"),
            "   Source stream file . . . . . . . . . . . . . . . . . . . :   /tmp/gld74.clle"
        );
        assert_eq!(
            line("Number of imported (unresolved) symbols", "6"),
            "   Number of imported (unresolved) symbols  . . . . . . . . :   6"
        );
    }

    #[test]
    fn printed_report_of_a_cl_module() {
        // verify74 u74_print (PUB400 2026-11-10): DETAIL(*BASIC *EXPORT
        // *IMPORT), five pages; the lines after each page's header.
        let stamp = crate::report::Stamp {
            date: "26-11-10".into(),
            time: "18:51:30".into(),
            zone: "UTC".into(),
        };
        let r = report(
            &cl_module(),
            &["*BASIC".into(), "*EXPORT".into(), "*IMPORT".into()],
            &stamp,
        );
        let module = " Module . . . . . . . . . . . . :   GLD74M                          Library  . . . . . . . . . . . :   AS4TRAINR1";
        assert_eq!(
            r.iter().filter(|l| l.as_str() == module).count(),
            5,
            "{r:#?}"
        );
        let has = |l: &str| r.iter().any(|x| x == l);
        assert!(has(" Detail  . . . . . . . . . . . :   *EXPORT"));
        assert!(has(" Module Attribute  . . . . . . :   CLLE"));
        assert!(has(
            "   Program entry procedure name . . . . . . . . . . . . . . :   _CL_PEP"
        ));
        assert!(has(
            "   Number of parameters . . . . . . . . . . . . . . . . . . :   0       255"
        ));
        assert!(has(
            "   Coded character set identifier . . . . . . . . . . . . . :   273"
        ));
        assert!(has(
            "   Earliest release module can be restored to . . . . . . . :   V7R5M0"
        ));
        assert!(has(
            " GLD74M                                           PROCEDURE          *NO"
        ));
        assert!(has(
            " Q LE leDefaultEh2                                PROCEDURE          *UNKNOWN"
        ));
        assert!(has(
            "                                                   Imported (unresolved) symbols:"
        ));
        assert_eq!(
            r.last().unwrap(),
            "                                        * * * * *   E N D   O F   L I S T I N G   * * * * *"
        );
        // Pages: 3 for *BASIC, one each for *EXPORT and *IMPORT.
        assert_eq!(
            r.iter()
                .filter(|l| l.contains("Display Module Information"))
                .count(),
            5
        );
    }

    #[test]
    fn basic_record_of_a_cl_module() {
        // verify74 u74_basrows.
        let rec = basic_record(
            &cl_module(),
            &("1".into(), "261110".into(), "185129".into()),
            "PUB400",
        );
        let v = |k: &str| rec.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        assert_eq!(v("BAPEPN"), "_CL_PEP");
        assert_eq!(v("BAMAXP"), "255");
        assert_eq!(v("BANEXS"), "1");
        assert_eq!(v("BANIMS"), "6");
        assert_eq!(v("BASLEN"), "15");
        assert_eq!(v("BACNVD"), "*COMMON");
        assert_eq!(v("BAARCS"), "*YES");
        assert_eq!(v("BACMCO"), "V7R5M0");
    }

    fn rpg_nomain() -> ModuleFacts {
        let sym = |n: &str, a: &str| Symbol {
            name: n.into(),
            data: false,
            argopt: a.into(),
        };
        let mut imports = vec![sym("OTHER76", "*NO")];
        imports.extend(RPG_IMPORTS.iter().map(|(n, a)| sym(n, a)));
        ModuleFacts {
            library: "AS4TRAINR1".into(),
            name: "GLD76R".into(),
            attribute: "RPGLE".into(),
            created: ("1".into(), "261110".into(), "225649".into()),
            src_stmf: "/tmp/gld76.rpgle".into(),
            owner: "AS4TRAINR".into(),
            ccsid: 273,
            exports: vec![sym("HELLO76", "*NO")],
            imports,
            has_main: false,
            ..Default::default()
        }
    }

    #[test]
    fn printed_report_of_an_rpg_nomain_module() {
        // verify76 u76_print (PUB400 2026-11-10): DETAIL(*ALL) of a NOMAIN
        // RPG module: *BASIC (no line for the parameters), *SIZE (two
        // pages), *EXPORT, *IMPORT, *PROCLIST, *REFSYSOBJ, *COPYRIGHT.
        let stamp = crate::report::Stamp {
            date: "26-11-10".into(),
            time: "22:56:50".into(),
            zone: "UTC".into(),
        };
        let r = report(&rpg_nomain(), &["*ALL".into()], &stamp);
        let has = |l: &str| r.iter().any(|x| x == l);
        assert_eq!(
            r.iter()
                .filter(|l| l.contains("Display Module Information"))
                .count(),
            10
        );
        assert!(has(
            "   Program entry procedure name . . . . . . . . . . . . . . :   *NONE"
        ));
        assert!(has(
            "   Maximum optimization level . . . . . . . . . . . . . . . :   *FULL"
        ));
        assert!(has(
            "   Allow RTVCLSRC (CL module) . . . . . . . . . . . . . . . :   *NO"
        ));
        assert!(!r.iter().any(|l| l.contains("Number of parameters")));
        assert!(has(
            "   Number of imported (unresolved) symbols  . . . . . . . . :   7"
        ));
        assert!(has(" Module size (decompressed) and limit information:"));
        assert!(has("   Exception mapping table list area (bytes):"));
        assert!(has("     Maximum size limit . . . . . . . . :   268435456"));
        assert!(has("     Maximum size limit . . . . . . . . :   190649"));
        assert!(has(" Procedure size (decompressed) and limit information:"));
        assert!(has(
            " OTHER76                                          PROCEDURE          *NO"
        ));
        assert!(has(
            " _QRNX_INIT_H                                     PROCEDURE          *UNKNOWN"
        ));
        assert!(has(
            "                                                          Procedure list:"
        ));
        assert!(has(
            " _QRNI_SOFT_ERR                                   REGULAR            *NO"
        ));
        assert!(has(
            "                                              Reference system objects for bind time:"
        ));
        assert!(has(" Object         Library        Type"));
        assert!(has(" QUSAPIBD       *LIBL          *BNDDIR"));
        assert!(has(
            "                                                       Copyright information:"
        ));
        assert!(has("   (No copyright information)"));
        // The *SIZE pages: 17 sizes on the first (u76_print page 4).
        let p4 = r.iter().position(|l| l.ends_with("Page     4")).unwrap();
        let p5 = r.iter().position(|l| l.ends_with("Page     5")).unwrap();
        assert!(r[p4..p5]
            .iter()
            .any(|l| l == "   Literal pool component (bytes):"));
        assert!(r[p5..]
            .iter()
            .any(|l| l == "   Static storage work area (bytes):"));
    }

    #[test]
    fn basic_and_size_records_of_an_rpg_nomain_module() {
        // verify76 u76_basrows, u76_sizerows.
        let now = ("1".into(), "261110".into(), "225649".into());
        let b = basic_record(&rpg_nomain(), &now, "PUB400");
        let v = |r: &crate::outfile::Record, k: &str| {
            r.iter().find(|(n, _)| *n == k).unwrap().1.clone()
        };
        assert_eq!(v(&b, "BAPEPN"), "*NONE");
        assert_eq!(v(&b, "BAMINP"), "-1");
        assert_eq!(v(&b, "BAMAXP"), "-1");
        assert_eq!(v(&b, "BAOPLX"), "*FULL");
        assert_eq!(v(&b, "BAARCS"), "*NO");
        assert_eq!(v(&b, "BANIMS"), "7");
        let z = size_record(&rpg_nomain(), &now, "PUB400");
        assert_eq!(v(&z, "SITMSL"), "268435456");
        assert_eq!(v(&z, "SIPLPL"), "16711616");
        assert_eq!(v(&z, "SISTSL8"), "2147483648");
    }
}
