//! DSPJOB's printed report (printer file QPDSPJOB): the job definition
//! attributes (*DFNA), status attributes (*STSA), run attributes (*RUNA)
//! and call stack (*PGMSTK).
//!
//! The layout is IBM i 7.5's, measured on PUB400's output of a PASE
//! `system` job (2026-10-28, tools/golden/verify19.txt b19_dfna, b19_stsa,
//! b19_runa, b19_pgmstk; the captures are not kept in the repository):
//! a two-line page header (product and level at column 2, "Display Job"
//! at 45, date and time at 97, "Page" at 123 with the number to column
//! 130; the job's name, user and number), the section's title, then one
//! line per attribute: its label from column 4 (6 for a part of the one
//! above) with leaders to the colon (column 49, 58 for the run
//! attributes), the keyword at 53 (62), the value at 66 (75; a part's
//! value at 68 in the definition attributes, at 55 in the status
//! attributes); 56 lines a page; the end-of-listing line last.
//!
//! The values are the job's as as4tainr has them; what it does not keep
//! is in docs/fidelity-gaps.md ("DSPJOB").

use crate::commands::dsp_label;
use crate::report::{at, Stamp};

/// Lines per page.
pub const PAGE_LINES: usize = 56;

/// The job the report is about, and what the sections show of it.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub name: String,
    pub user: String,
    pub number: String,
    /// A PASE `system` job (QP0ZSPWT): its attributes are those of
    /// QGPL/QDFTSVR and of a prestart job in QUSRWRK, as PUB400 showed
    /// them.
    pub system_job: bool,
    /// I (interactive), B (batch), or a `system` job.
    pub job_type: String,
    pub jobd: String,
    pub jobd_lib: String,
    /// The job description's attributes as given (`jobd::given`).
    pub jobd_other: String,
    pub jobq: String,
    pub jobq_lib: String,
    pub current_user: String,
    pub subsystem: String,
    /// Entered the system and started: date and time as the job shows
    /// them.
    pub entered: (String, String),
    pub started: (String, String),
    pub stack: Vec<StackEntry>,
}

/// One call stack entry (*PGMSTK), innermost last.
#[derive(Debug, Clone, Default)]
pub struct StackEntry {
    pub pgm: String,
    pub lib: String,
    pub stmt: String,
    pub inst: String,
    /// Activation group name and number.
    pub actgrp: String,
    pub actgrp_number: u64,
    pub module: String,
    pub module_lib: String,
    pub control_boundary: bool,
    pub procedure: String,
}

/// The two header lines of a page.
fn header(f: &Facts, stamp: &Stamp, page: usize) -> [String; 2] {
    let level = format!("{} {}", crate::report::PRODUCT, crate::report::LEVEL);
    let when = format!("{} {}", stamp.date, stamp.time);
    let page = format!("Page {page:>3}");
    [
        at(&[(2, &level), (45, "Display Job"), (97, &when), (123, &page)]),
        at(&[
            (2, "Job . . . :"),
            (16, &f.name),
            (29, "User . . . :"),
            (44, &f.user),
            (58, "Number . . . . :"),
            (77, &f.number),
        ]),
    ]
}

/// An attribute line: label from `ind` with leaders to the colon at
/// `colon`, then the keyword and the value at their columns.
fn row(ind: usize, label: &str, colon: usize, kw: (usize, &str), val: (usize, &str)) -> String {
    let lead = if label.ends_with(':') {
        label.to_string()
    } else if label == "Output priority (on output queue)" {
        // IBM i's leaders start right after this label's parenthesis
        // (b19_dfna).
        format!("{label}. . . . . . :")
    } else {
        dsp_label(label, colon - ind - 2)
    };
    let mut f: Vec<(usize, &str)> = vec![(ind, &lead)];
    if !kw.1.is_empty() {
        f.push(kw);
    }
    if !val.1.is_empty() {
        f.push(val);
    }
    at(&f)
}

fn sv(name: &str) -> String {
    crate::clpgm::system_value_text(name)
        .and_then(|v| v.ok())
        .unwrap_or_default()
}

/// *DFNA: the job definition attributes.
pub fn definition(f: &Facts) -> Vec<String> {
    let given = |k: &str| crate::jobd::given(&f.jobd_other, k);
    let val = |k: &str, sys: &str, dft: &str| -> String {
        if f.system_job {
            sys.to_string()
        } else {
            given(k).unwrap_or_else(|| dft.to_string())
        }
    };
    let ((y, mo, d), (h, mi, s), _) = crate::rpg::dates::now();
    let iso = format!("{y:04}-{mo:02}-{d:02}");
    let tsep = sv("QTIMSEP");
    let time = format!("{h:02}{tsep}{mi:02}{tsep}{s:02}");
    let log = val("LOG", "4 0 *NOLIST", "4 0 *NOLIST");
    let mut log = log.split_whitespace().map(str::to_string);
    let (level, sev, text) = (
        log.next().unwrap_or_default(),
        log.next().unwrap_or_default(),
        log.next().unwrap_or_default(),
    );
    // *SYSVAL and *USRPRF resolved: the system values as shipped
    // (QLOGOUTPUT *PND, QPRTDEV), the user profile's (*SYS accounting
    // code, the printer device of the system).
    let resolved = |v: String, sysval: &str| -> String {
        match v.as_str() {
            "*SYSVAL" | "*USRPRF" => sysval.to_string(),
            _ => v,
        }
    };
    let logoutput = resolved(val("LOGOUTPUT", "*PND", "*SYSVAL"), "*PND");
    let prtdev = resolved(val("PRTDEV", "*USRPRF", "*USRPRF"), &sv("QPRTDEV"));
    let acgcde = resolved(val("ACGCDE", "*SYS", "*USRPRF"), "*SYS");
    let (outq, outq_lib) = if f.system_job {
        ("*DEV".to_string(), String::new())
    } else {
        ("QPRINT".to_string(), "QGPL".to_string())
    };
    let utc = sv("QUTCOFFSET");
    let offset = format!(
        "{}:{}",
        utc.get(0..3).unwrap_or("+00"),
        utc.get(3..5).unwrap_or("00")
    );
    let zone = sv("QTIMZON");
    let (full, abbr) = match zone.as_str() {
        "Q0000UTC" => ("Coordinated Universal Time", "UTC"),
        _ => ("", ""),
    };
    let decfmt = match sv("QDECFMT").trim() {
        "" => "*BLANK".to_string(),
        v => v.to_string(),
    };
    let prttxt = format!("'{:<30}'", sv("QPRTTXT"));
    let r = |ind: usize, l: &str, kw: &str, v: &str| {
        let vc = if ind == 4 { 66 } else { 68 };
        row(ind, l, 49, (53, kw), (vc, v))
    };
    let part = |l: &str, v: &str| row(6, l, 49, (53, ""), (66, v));
    let jobq_asp = if f.jobq.is_empty() { "" } else { "*SYSBAS" };
    vec![
        at(&[(37, "Job Definition Attributes")]),
        r(4, "Job description", "JOBD", &f.jobd),
        r(6, "Library", "", &f.jobd_lib),
        r(6, "ASP device", "", "*SYSBAS"),
        r(4, "Job queue", "JOBQ", &f.jobq),
        r(6, "Library", "", &f.jobq_lib),
        r(6, "ASP device", "", jobq_asp),
        r(
            4,
            "Job priority (on job queue)",
            "JOBPTY",
            &val("JOBPTY", "5", "5"),
        ),
        r(
            4,
            "Output priority (on output queue)",
            "OUTPTY",
            &val("OUTPTY", "5", "5"),
        ),
        r(4, "End severity", "ENDSEV", &val("ENDSEV", "30", "30")),
        r(4, "Message logging:", "LOG", ""),
        part("Level", &level),
        part("Severity", &sev),
        part("Text", &text),
        r(
            4,
            "Log CL program commands",
            "LOGCLPGM",
            &val("LOGCLPGM", "*NO", "*NO"),
        ),
        r(4, "Job log output", "LOGOUTPUT", &logoutput),
        r(4, "Printer device", "PRTDEV", &prtdev),
        r(4, "Default output queue", "OUTQ", &outq),
        r(6, "Library", "", &outq_lib),
        r(4, "Job date", "DATE", &iso),
        r(4, "Date format", "DATFMT", &format!("*{}", sv("QDATFMT"))),
        r(4, "Date separator", "DATSEP", &sv("QDATSEP")),
        r(4, "Time separator", "TIMSEP", &tsep),
        r(4, "Job time:", "DATETIME", ""),
        part("Current date", &iso),
        part("Current time", &time),
        r(4, "Day of week", "DAYOFWEEK", &sv("QDAYOFWEEK")),
        r(4, "Time zone", "TIMZON", &zone),
        r(6, "Offset", "TIMOFFSET", &offset),
        r(6, "Full name", "TIMZONFULL", full),
        r(6, "Abbreviated name", "TIMZONABBR", abbr),
        r(4, "Year offset", "YEAROFS", "0"),
        r(
            4,
            "Job switches",
            "SWS",
            &val("SWS", "00000000", "00000000"),
        ),
        r(
            4,
            "Inquiry message reply",
            "INQMSGRPY",
            &val("INQMSGRPY", "*RQD", "*RQD"),
        ),
        r(4, "Accounting code", "ACGCDE", &acgcde),
        r(4, "Print text", "PRTTXT", &prttxt),
        r(
            4,
            "DDM conversation",
            "DDMCNV",
            &val("DDMCNV", "*KEEP", "*KEEP"),
        ),
        r(4, "Break message handling", "BRKMSG", "*NORMAL"),
        r(4, "Status message", "STSMSG", "*NORMAL"),
        r(4, "Device recovery action", "DEVRCYACN", "*MSG"),
        r(4, "Time slice end pool", "TSEPOOL", "*NONE"),
        r(4, "Print key format", "PRTKEYFMT", "*PRTHDR"),
        r(4, "Sort sequence", "SRTSEQ", "*HEX"),
        r(6, "Library", "", ""),
        r(4, "Language identifier", "LANGID", &sv("QLANGID")),
        r(
            4,
            "Country or region identifier",
            "CNTRYID",
            &sv("QCNTRYID"),
        ),
        // The job's CCSIDs as QUSRJOBI JOBI0400 gives them here.
        r(4, "Coded character set ID", "CCSID", "65535"),
        r(4, "Default coded character set ID", "DFTCCSID", "37"),
        r(4, "Job message queue maximum size", "JOBMSGQMX", "4"),
        r(4, "Job message queue full action", "JOBMSGQFL", "*WRAP"),
        r(
            4,
            "Allow multiple threads",
            "ALWMLTTHD",
            &val("ALWMLTTHD", "*YES", "*NO"),
        ),
        r(4, "Decimal format", "DECFMT", &decfmt),
        r(4, "Character identifier control", "CHRIDCTL", "*JOBCCSID"),
        r(4, "Auxiliary storage pool group", "ASPGRP", "*NONE"),
        r(4, "Spooled file action", "SPLFACN", "*DETACH"),
        r(4, "Workload group", "WLCGRP", "*NONE"),
        r(4, "Processor resources priority", "PRCRSCPTY", "*SYSCTL"),
    ]
}

/// *STSA: the job status attributes.
pub fn status(f: &Facts) -> Vec<String> {
    let r = |ind: usize, l: &str, v: &str| {
        let vc = if l == "Set by" { 55 } else { 53 };
        row(ind, l, 49, (53, ""), (vc, v))
    };
    let job_type = match f.job_type.as_str() {
        "I" => "INT",
        "B" => "BCH",
        _ => "PJ",
    };
    vec![
        at(&[(39, "Job Status Attributes")]),
        r(4, "Status of job", "ACTIVE"),
        r(4, "Current user profile", &f.current_user),
        r(4, "Job user identity", &f.current_user),
        r(6, "Set by", "*DEFAULT"),
        r(4, "Entered system:", ""),
        r(6, "Date", &f.entered.0),
        r(6, "Time", &f.entered.1),
        r(4, "Started:", ""),
        r(6, "Date", &f.started.0),
        r(6, "Time", &f.started.1),
        r(4, "Subsystem", &f.subsystem),
        r(6, "Subsystem pool ID", "1"),
        r(4, "Type of job", job_type),
        r(4, "Special environment", "*NONE"),
        r(4, "Program return code", "0"),
        r(4, "Controlled end requested", "NO"),
        r(4, "System", &crate::report::system_name()),
    ]
}

/// What the process has used: CPU milliseconds, and the resident and
/// peak memory in megabytes (for the run attributes).
fn usage() -> (u64, u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, r)| r.split_whitespace().collect())
        .unwrap_or_default();
    // utime and stime (fields 14 and 15), in clock ticks of 10 ms.
    let ticks = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let cpu = (ticks(11) + ticks(12)) * 10;
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|r| r.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (cpu, kb("VmRSS:") / 1024, kb("VmHWM:") / 1024)
}

/// *RUNA: the job run attributes. A `system` job's (PUB400: run
/// priority 50, time slice 2000, default wait 30); an interactive job's
/// and a batch job's are those of IBM i's QINTER and QBATCH classes
/// (run priority 20 and 50, time slice 2000 and 5000, default wait 30 and
/// 120, IBM i work management documentation).
pub fn run(f: &Facts) -> Vec<String> {
    let (pty, slice, wait) = match (f.system_job, f.job_type.as_str()) {
        (true, _) => ("50", "2000", "30"),
        (false, "B") => ("50", "5000", "120"),
        _ => ("20", "2000", "30"),
    };
    let (cpu, tmp, peak) = usage();
    let r = |ind: usize, l: &str, kw: &str, v: &str| row(ind, l, 58, (62, kw), (75, v));
    vec![
        at(&[(41, "Job Run Attributes")]),
        r(4, "Run priority", "RUNPTY", pty),
        r(4, "Time slice in milliseconds", "TIMESLICE", slice),
        r(4, "Eligible for purge", "PURGE", "*YES"),
        r(4, "Default wait time in seconds", "DFTWAIT", wait),
        r(4, "Maximum CPU time in milliseconds", "CPUTIME", "*NOMAX"),
        r(6, "CPU time used", "", &cpu.to_string()),
        r(
            4,
            "Maximum temporary storage in megabytes",
            "MAXTMPSTG",
            "*NOMAX",
        ),
        r(6, "Temporary storage used", "", &tmp.to_string()),
        r(6, "Peak temporary storage used", "", &peak.to_string()),
        r(4, "Maximum threads", "MAXTHD", "*NOMAX"),
        r(6, "Threads", "", "1"),
        r(4, "Thread resources affinity:", "THDRSCAFN", ""),
        r(6, "Group", "", "*NOGROUP"),
        r(6, "Level", "", "*NORMAL"),
        r(4, "Resources affinity group", "RSCAFNGRP", "*NO"),
    ]
}

/// *PGMSTK: the call stack, outermost first.
pub fn stack(f: &Facts) -> Vec<String> {
    let mut out = vec![
        at(&[(48, "Job Call Stack")]),
        at(&[(2, "Thread"), (10, ". :"), (16, "00000001")]),
        at(&[(101, "Module or"), (124, "Ctl")]),
        at(&[
            (2, "Type"),
            (7, "Program"),
            (31, "Statement"),
            (41, "Identifiers"),
            (56, "Instruction"),
            (73, "Activation"),
            (84, "Group"),
            (101, "Expanded"),
            (110, "Type"),
            (124, "Bdy"),
        ]),
    ];
    for e in &f.stack {
        let number = format!("{:016}", e.actgrp_number);
        let inst = format!("{:>4}", e.inst);
        let mut cols: Vec<(usize, &str)> = vec![(7, &e.pgm), (18, &e.lib)];
        if !e.stmt.is_empty() {
            cols.push((31, &e.stmt));
        }
        if !e.inst.is_empty() {
            cols.push((68, &inst));
        }
        cols.push((73, &e.actgrp));
        cols.push((84, &number));
        if !e.module.is_empty() {
            cols.push((101, &e.module));
            cols.push((112, &e.module_lib));
        }
        cols.push((125, if e.control_boundary { "Y" } else { "N" }));
        out.push(at(&cols));
        if !e.procedure.is_empty() {
            out.push(at(&[(10, &format!("Procedure: {}", e.procedure))]));
        }
    }
    out
}

/// The report: the sections asked for, in pages of [`PAGE_LINES`] lines
/// with the header on each, then the end-of-listing line.
pub fn print(f: &Facts, sections: &[Vec<String>], stamp: &Stamp) -> Vec<String> {
    let body: Vec<String> = sections.iter().flatten().cloned().collect();
    let per_page = PAGE_LINES - 2;
    let mut out = Vec::new();
    let pages = body.len().div_ceil(per_page).max(1);
    for p in 0..pages {
        out.extend(header(f, stamp, p + 1));
        out.extend(body.iter().skip(p * per_page).take(per_page).cloned());
    }
    out.push(crate::report::END_A.to_string());
    out
}

/// The entries below the command of a PASE `system` job, outermost
/// first, as PUB400's showed them (b19_pgmstk): the job's own program
/// QP0ZSPWT, QP0ZPCPN making the process, then QP2CMD, QP2USER2 and
/// QCMDEXC running the command.
pub fn system_job_base() -> Vec<StackEntry> {
    let e = |pgm: &str, stmt: &str, n: u64, module: &str, bdy: bool, proc: &str| StackEntry {
        pgm: pgm.into(),
        lib: "QSYS".into(),
        stmt: stmt.into(),
        inst: String::new(),
        actgrp: "*DFTACTGRP".into(),
        actgrp_number: n,
        module: module.into(),
        module_lib: "QBUILDSS1".into(),
        control_boundary: bdy,
        procedure: proc.into(),
    };
    vec![
        e("QP0ZSPWT", "", 1, "QP0ZSPWT", true, "_CXX_PEP__Fv"),
        e("QP0ZPCPN", "132", 1, "QP0ZPCPN", false, "Qp0zPJNewProcess"),
        e("QP0ZPCPN", "306", 1, "QP0ZPCPN", false, "Qp0zNewProcess"),
        e("QP2CMD", "", 2, "QP2CMD", true, "_CXX_PEP__Fv"),
        e("QP2CMD", "35", 2, "QP2CMD", false, "main"),
        e("QP2USER2", "93", 2, "QP2CL", false, "__Qp2RunCL"),
    ]
}

/// QCMDEXC's entry (b19_pgmstk: instruction 012F, the system's default
/// activation group, no module).
pub fn qcmdexc_entry() -> StackEntry {
    StackEntry {
        pgm: "QCMDEXC".into(),
        lib: "QSYS".into(),
        inst: "012F".into(),
        actgrp: "*DFTACTGRP".into(),
        actgrp_number: 1,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(l: &str) -> Vec<usize> {
        let b = l.as_bytes();
        let mut v = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b' ' {
                i += 1;
                continue;
            }
            v.push(i + 1);
            while i < b.len() && !(b[i] == b' ' && (i + 1 >= b.len() || b[i + 1] == b' ')) {
                i += 1;
            }
        }
        v
    }

    fn facts() -> Facts {
        Facts {
            name: "QP0ZSPWT".into(),
            user: "QUSER".into(),
            number: "757588".into(),
            system_job: true,
            jobd: "QDFTSVR".into(),
            jobd_lib: "QGPL".into(),
            current_user: "AS4TRAINR".into(),
            subsystem: "QUSRWRK".into(),
            ..Default::default()
        }
    }

    // The header, title and attribute columns of PUB400's report
    // (2026-10-28, tools/golden/verify19.txt b19_dfna, b19_stsa,
    // b19_runa).
    #[test]
    fn columns_as_captured() {
        let stamp = Stamp {
            date: "26-10-28".into(),
            time: "17:38:18".into(),
            zone: "UTC".into(),
        };
        let p = print(&facts(), &[definition(&facts())], &stamp);
        assert_eq!(cols(&p[0]), [2, 45, 97, 123, 130]);
        assert_eq!(cols(&p[1]), [2, 16, 29, 44, 58, 77]);
        assert_eq!(cols(&p[2]), [37]);
        assert_eq!(
            p[3],
            "   Job description  . . . . . . . . . . . . . . :   JOBD         QDFTSVR"
        );
        assert_eq!(
            p[4],
            "     Library  . . . . . . . . . . . . . . . . . :                  QGPL"
        );
        assert_eq!(
            p[10],
            "   Output priority (on output queue). . . . . . :   OUTPTY       5"
        );
        assert_eq!(
            p[12],
            "   Message logging:                                 LOG"
        );
        assert_eq!(
            p[13],
            "     Level  . . . . . . . . . . . . . . . . . . :                4"
        );
        // 56 lines a page: page 2 starts with the header, then the
        // last three attributes, then the end of the listing.
        assert!(p[56].ends_with("Page   2"), "{p:?}");
        assert!(p[58].trim_start().starts_with("Spooled file action"));
        assert_eq!(p.len(), 62);
        assert!(p[61].contains("E N D  O F  L I S T I N G"));
        let s = status(&facts());
        assert_eq!(
            s[4],
            "     Set by . . . . . . . . . . . . . . . . . . :     *DEFAULT"
        );
        assert_eq!(
            s[13],
            "   Type of job  . . . . . . . . . . . . . . . . :   PJ"
        );
        let r = run(&facts());
        assert_eq!(
            r[1],
            "   Run priority  . . . . . . . . . . . . . . . . . . . . :   RUNPTY       50"
        );
        assert_eq!(cols(&r[6]), [6, 75]);
    }

    // The call stack's lines (b19_pgmstk: QP2USER2 and QCMDEXC).
    #[test]
    fn stack_lines() {
        let mut f = facts();
        f.stack = system_job_base();
        f.stack.push(qcmdexc_entry());
        let l = stack(&f);
        assert_eq!(
            l[3],
            " Type Program                 Statement Identifiers    Instruction      Activation Group            Expanded Type          Bdy"
        );
        assert_eq!(
            l[14],
            "      QP2USER2   QSYS         93                                        *DFTACTGRP 0000000000000002 QP2CL      QBUILDSS1    N"
        );
        assert_eq!(l[15], "         Procedure: __Qp2RunCL");
        assert_eq!(
            l[16],
            "      QCMDEXC    QSYS                                              012F *DFTACTGRP 0000000000000001                         N"
        );
    }
}
