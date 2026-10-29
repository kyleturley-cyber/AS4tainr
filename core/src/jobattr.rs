//! A job's changeable attributes (CHGJOB): kept in this process by job
//! key (`Job::job_key`, the RPG engine's `io.job_key`), as job-level
//! environment variables are. What a job has not changed comes from its
//! job description or the system values, as before (`dspjob.rs`,
//! RTVJOBA, QUSRJOBI).
//!
//! Written from IBM's CL reference (Change Job (CHGJOB)): the parameters,
//! their special values and ranges; *SYSVAL takes the system value when
//! the command runs; SWS is eight characters of 0 (off), 1 (on) and X
//! (unchanged); DEVRCYACN and TSEPOOL only for interactive jobs
//! (CPF1334); RUNPTY, TIMESLICE, PURGE, DFTWAIT, TSEPOOL, WLCGRP,
//! CPUTIME, MAXTMPSTG and PRCRSCPTY need *JOBCTL special authority
//! (CPF1337). Retrieve Job Attributes (RTVJOBA) returns them.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

type Attrs = BTreeMap<String, String>;

static JOBS: Mutex<Option<HashMap<String, Attrs>>> = Mutex::new(None);

/// An attribute the job changed (keyword as CHGJOB names it), as DSPJOB
/// shows it: `*MDY`, `/`, `00000000`, `4 00 *NOLIST`...
pub fn get(job_key: &str, keyword: &str) -> Option<String> {
    JOBS.lock()
        .ok()?
        .as_ref()?
        .get(job_key)?
        .get(keyword)
        .cloned()
}

/// Every attribute the job changed.
pub fn all(job_key: &str) -> Attrs {
    JOBS.lock()
        .ok()
        .and_then(|g| g.as_ref()?.get(job_key).cloned())
        .unwrap_or_default()
}

fn set(job_key: &str, keyword: &str, value: String) {
    if let Ok(mut g) = JOBS.lock() {
        g.get_or_insert_with(HashMap::new)
            .entry(job_key.to_string())
            .or_default()
            .insert(keyword.to_string(), value);
    }
}

/// The job is gone: forget what it changed.
pub fn forget(job_key: &str) {
    if let Ok(mut g) = JOBS.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(job_key);
        }
    }
}

/// The job switches: eight characters of 0 and 1 (CHGJOB SWS, or the
/// job description's, else 00000000).
pub fn switches(job_key: &str, from_jobd: Option<&str>) -> String {
    get(job_key, "SWS")
        .or_else(|| from_jobd.map(str::to_string))
        .filter(|s| s.len() == 8)
        .unwrap_or_else(|| "00000000".into())
}

/// Set the job switches (an RPG program ending with LR on writes U1-U8
/// back: ILE RPG Reference, External Indicators).
pub fn set_switches(job_key: &str, sws: &str) {
    set(job_key, "SWS", sws.to_string());
}

/// %SWITCH (CL): each of the eight mask characters 0 or 1 must match the
/// job switch, X matches either (CL reference, %SWITCH built-in function).
pub fn switch_matches(switches: &str, mask: &str) -> Result<bool> {
    let mask = mask.trim().trim_matches('\'');
    if mask.len() != 8 || !mask.bytes().all(|b| matches!(b, b'0' | b'1' | b'X' | b'x')) {
        bail!("%SWITCH needs eight characters 0, 1 or X: {mask}");
    }
    Ok(mask
        .bytes()
        .zip(switches.bytes())
        .all(|(m, s)| matches!(m, b'X' | b'x') || m == s))
}

/// What a value must be.
enum Rule {
    /// A number in the range, or one of the special values.
    Range(i64, i64, &'static [&'static str]),
    /// One of the special values.
    Special(&'static [&'static str]),
    /// A name (or LIB/NAME when `qualified`), or one of the special values.
    Name(bool, &'static [&'static str]),
    /// Text up to the length, or one of the special values.
    Text(usize, &'static [&'static str]),
    /// One character of the set, or one of the special values.
    Char(&'static str, &'static [&'static str]),
    Sws,
    Log,
}

/// Parameters needing *JOBCTL special authority.
const JOBCTL: &[&str] = &[
    "RUNPTY",
    "TIMESLICE",
    "PURGE",
    "DFTWAIT",
    "TSEPOOL",
    "WLCGRP",
    "CPUTIME",
    "MAXTMPSTG",
    "PRCRSCPTY",
];

/// Parameters only an interactive job has.
const INTERACTIVE: &[&str] = &["DEVRCYACN", "TSEPOOL"];

fn rule(keyword: &str) -> Option<Rule> {
    use Rule::*;
    Some(match keyword {
        "JOBPTY" => Range(0, 9, &[]),
        "OUTPTY" => Range(1, 9, &[]),
        "RUNPTY" => Range(1, 99, &[]),
        "PRTDEV" => Name(false, &["*USRPRF", "*SYSVAL", "*WRKSTN"]),
        "OUTQ" => Name(true, &["*USRPRF", "*DEV", "*WRKSTN"]),
        "PRTTXT" => Text(30, &["*SYSVAL", "*BLANK"]),
        "LOG" => Log,
        "LOGCLPGM" => Special(&["*NO", "*YES"]),
        "LOGOUTPUT" => Special(&["*SYSVAL", "*JOBLOGSVR", "*JOBEND", "*PND"]),
        "JOBMSGQFL" => Special(&["*SYSVAL", "*NOWRAP", "*WRAP", "*PRTWRAP"]),
        "INQMSGRPY" => Special(&["*RQD", "*DFT", "*SYSRPYL"]),
        "BRKMSG" => Special(&["*NORMAL", "*NOTIFY", "*HOLD"]),
        "STSMSG" => Special(&["*USRPRF", "*SYSVAL", "*NONE", "*NORMAL"]),
        "DDMCNV" => Special(&["*KEEP", "*DROP"]),
        "DATFMT" => Special(&["*SYSVAL", "*YMD", "*MDY", "*DMY", "*JUL"]),
        "DATSEP" => Char("/-., ", &["*SYSVAL", "*BLANK"]),
        "TIMSEP" => Char(":., ", &["*SYSVAL", "*BLANK"]),
        "SWS" => Sws,
        "TIMESLICE" => Range(1, 9_999_999, &[]),
        "PURGE" => Special(&["*YES", "*NO"]),
        "DFTWAIT" => Range(1, 9_999_999, &["*NOMAX"]),
        "DEVRCYACN" => Special(&[
            "*SYSVAL",
            "*MSG",
            "*DSCMSG",
            "*DSCENDRQS",
            "*ENDJOB",
            "*ENDJOBNOLIST",
        ]),
        "TSEPOOL" => Special(&["*SYSVAL", "*NONE", "*BASE"]),
        "PRTKEYFMT" => Special(&["*SYSVAL", "*NONE", "*PRTBDR", "*PRTHDR", "*PRTALL"]),
        "SRTSEQ" => Name(
            true,
            &["*USRPRF", "*SYSVAL", "*HEX", "*LANGIDUNQ", "*LANGIDSHR"],
        ),
        "LANGID" => Name(false, &["*USRPRF", "*SYSVAL"]),
        "CNTRYID" => Name(false, &["*USRPRF", "*SYSVAL"]),
        "CCSID" => Range(1, 65535, &["*USRPRF", "*SYSVAL", "*HEX"]),
        "DECFMT" => Char("IJ", &["*SYSVAL", "*BLANK"]),
        "CHRIDCTL" => Special(&["*USRPRF", "*SYSVAL", "*DEVD", "*JOBCCSID"]),
        "SPLFACN" => Special(&["*SYSVAL", "*KEEP", "*DETACH"]),
        "WLCGRP" => Name(false, &["*NONE"]),
        "CPUTIME" => Range(1, 9_999_999, &["*NOMAX"]),
        "MAXTMPSTG" => Range(1, 2_147_483_647, &["*NOMAX"]),
        "PRCRSCPTY" => Special(&["*SYSCTL", "*NORMAL", "*HIGH", "*LOW"]),
        _ => return None,
    })
}

/// The parameters CHGJOB changes for a running job, in its order.
pub const PARAMS: &[&str] = &[
    "JOBPTY",
    "OUTPTY",
    "PRTDEV",
    "OUTQ",
    "RUNPTY",
    "PRTTXT",
    "LOG",
    "LOGCLPGM",
    "LOGOUTPUT",
    "JOBMSGQFL",
    "INQMSGRPY",
    "BRKMSG",
    "STSMSG",
    "DDMCNV",
    "DATE",
    "DATFMT",
    "DATSEP",
    "TIMSEP",
    "SWS",
    "TIMESLICE",
    "PURGE",
    "DFTWAIT",
    "DEVRCYACN",
    "TSEPOOL",
    "PRTKEYFMT",
    "SRTSEQ",
    "LANGID",
    "CNTRYID",
    "CCSID",
    "DECFMT",
    "CHRIDCTL",
    "SPLFACN",
    "WLCGRP",
    "CPUTIME",
    "MAXTMPSTG",
    "PRCRSCPTY",
];

fn sysval(name: &str) -> String {
    crate::clpgm::system_value_text(name)
        .map(|v| match v {
            Ok(s) => s,
            Err(n) => n.to_string(),
        })
        .unwrap_or_default()
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || "$#@".contains(c))
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "$#@_.".contains(c))
}

/// A value checked against its rule and put in the form the job keeps
/// (system values resolved). `current`: the job's value now (for SWS's X
/// and LOG's *SAME elements).
fn normalize(keyword: &str, value: &str, current: &str) -> Result<String> {
    let v = value.trim();
    let unq = v.trim_matches('\'');
    let upper = unq.to_ascii_uppercase();
    let bad = || -> anyhow::Error {
        crate::cl::diag_error(
            "",
            &format!("Value '{unq}' for parameter {keyword} is not valid."),
        )
    };
    let Some(rule) = rule(keyword) else {
        return Ok(unq.to_string());
    };
    let resolved = |s: &str| -> String {
        match (keyword, s) {
            ("DATFMT", "*SYSVAL") => format!("*{}", sysval("QDATFMT")),
            ("DATSEP", "*SYSVAL") => sysval("QDATSEP"),
            ("TIMSEP", "*SYSVAL") => sysval("QTIMSEP"),
            ("DECFMT", "*SYSVAL") => sysval("QDECFMT"),
            ("DATSEP" | "TIMSEP" | "DECFMT", "*BLANK") => " ".into(),
            ("PRTTXT", "*SYSVAL") => sysval("QPRTTXT"),
            ("PRTTXT", "*BLANK") => String::new(),
            ("CCSID", "*SYSVAL" | "*USRPRF") => sysval("QCCSID"),
            ("CCSID", "*HEX") => "65535".into(),
            ("LANGID", "*SYSVAL" | "*USRPRF") => sysval("QLANGID"),
            ("CNTRYID", "*SYSVAL" | "*USRPRF") => sysval("QCNTRYID"),
            ("PRTDEV", "*SYSVAL") => sysval("QPRTDEV"),
            (_, s) => s.to_string(),
        }
    };
    Ok(match rule {
        Rule::Range(lo, hi, specials) => {
            if specials.contains(&upper.as_str()) {
                resolved(&upper)
            } else {
                match upper.parse::<i64>() {
                    Ok(n) if (lo..=hi).contains(&n) => n.to_string(),
                    _ => return Err(bad()),
                }
            }
        }
        Rule::Special(specials) => {
            if !specials.contains(&upper.as_str()) {
                return Err(bad());
            }
            resolved(&upper)
        }
        Rule::Name(qualified, specials) => {
            if specials.contains(&upper.as_str()) {
                resolved(&upper)
            } else {
                let (lib, name) = match upper.split_once('/') {
                    Some((l, n)) if qualified => (Some(l), n),
                    Some(_) => return Err(bad()),
                    None => (None, upper.as_str()),
                };
                let lib_ok = lib.is_none_or(|l| matches!(l, "*LIBL" | "*CURLIB") || valid_name(l));
                if !valid_name(name) || !lib_ok {
                    return Err(bad());
                }
                match lib {
                    Some(l) => format!("{l}/{name}"),
                    None if qualified => format!("*LIBL/{name}"),
                    None => name.to_string(),
                }
            }
        }
        Rule::Text(max, specials) => {
            if specials.contains(&upper.as_str()) {
                resolved(&upper)
            } else if unq.chars().count() > max {
                return Err(bad());
            } else {
                unq.to_string()
            }
        }
        Rule::Char(set, specials) => {
            if specials.contains(&upper.as_str()) {
                resolved(&upper)
            } else if unq.chars().count() == 1 && set.contains(unq) {
                unq.to_string()
            } else {
                return Err(bad());
            }
        }
        Rule::Sws => {
            if upper.len() != 8 || !upper.bytes().all(|b| matches!(b, b'0' | b'1' | b'X')) {
                return Err(bad());
            }
            let cur: Vec<u8> = format!("{current:0<8}").bytes().collect();
            upper
                .bytes()
                .enumerate()
                .map(|(i, b)| if b == b'X' { cur[i] as char } else { b as char })
                .collect()
        }
        Rule::Log => {
            // Level 0-4, severity 0-99, text *MSG/*SECLVL/*NOLIST; each
            // element may be *SAME. Kept as DSPJOB shows it.
            let parts: Vec<&str> = upper.split_whitespace().collect();
            let cur: Vec<&str> = current.split_whitespace().collect();
            let pick = |i: usize| -> String {
                match parts.get(i) {
                    None | Some(&"*SAME") => cur.get(i).copied().unwrap_or("").to_string(),
                    Some(p) => p.to_string(),
                }
            };
            if parts.len() > 3 {
                return Err(bad());
            }
            let (l, s, t) = (pick(0), pick(1), pick(2));
            if !matches!(l.as_str(), "0" | "1" | "2" | "3" | "4") {
                return Err(bad());
            }
            let sev = s.parse::<u8>().ok().filter(|n| *n <= 99).ok_or_else(bad)?;
            if !matches!(t.as_str(), "*MSG" | "*SECLVL" | "*NOLIST") {
                return Err(bad());
            }
            format!("{l} {sev} {t}")
        }
    })
}

/// What CHGJOB asks for: (keyword, value) pairs not *SAME. `current`
/// gives the job's value of an attribute now. `interactive`: the job has a
/// display; `jobctl`: the user has *JOBCTL special authority.
pub fn change(
    job_key: &str,
    asked: &[(String, String)],
    current: &dyn Fn(&str) -> String,
    interactive: bool,
    jobctl: bool,
) -> Result<()> {
    // All are checked before any is changed.
    let mut new = Vec::new();
    for (k, v) in asked {
        if v.trim().eq_ignore_ascii_case("*SAME") || v.trim().is_empty() {
            continue;
        }
        if INTERACTIVE.contains(&k.as_str()) && !interactive {
            bail!("CPF1334: {k} can be changed only for an interactive job.");
        }
        if JOBCTL.contains(&k.as_str()) && !jobctl {
            bail!("CPF1337: Changing {k} needs *JOBCTL special authority.");
        }
        new.push((k.clone(), normalize(k, v, &current(k))?));
    }
    for (k, v) in new {
        set(job_key, &k, v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values and rules from IBM's CL reference, Change Job (CHGJOB).
    #[test]
    fn values_are_checked_and_kept_as_the_job_shows_them() {
        // X keeps the switch as it is: 1 X 0 X X X X 1 over 0 1 0 1 0 1 0 1.
        assert_eq!(
            normalize("SWS", "1X0XXXX1", "01010101").unwrap(),
            "11010101"
        );
        assert!(normalize("SWS", "1X0", "00000000").is_err());
        assert_eq!(
            normalize("LOG", "4 *SAME *SECLVL", "2 10 *NOLIST").unwrap(),
            "4 10 *SECLVL"
        );
        assert!(normalize("LOG", "5 0 *MSG", "4 0 *NOLIST").is_err());
        assert_eq!(normalize("DATSEP", "'-'", "/").unwrap(), "-");
        assert_eq!(normalize("DATSEP", "*BLANK", "/").unwrap(), " ");
        assert!(normalize("DATSEP", "':'", "/").is_err());
        assert_eq!(normalize("DATFMT", "*YMD", "*MDY").unwrap(), "*YMD");
        assert_eq!(normalize("DATFMT", "*SYSVAL", "*YMD").unwrap(), "*MDY");
        assert!(normalize("JOBPTY", "10", "5").is_err());
        assert!(normalize("OUTPTY", "0", "5").is_err());
        assert_eq!(normalize("RUNPTY", "99", "20").unwrap(), "99");
        assert_eq!(normalize("OUTQ", "QGPL/QPRINT", "").unwrap(), "QGPL/QPRINT");
        assert_eq!(normalize("CCSID", "*HEX", "37").unwrap(), "65535");
        assert_eq!(normalize("INQMSGRPY", "*dft", "*RQD").unwrap(), "*DFT");
        assert!(normalize("INQMSGRPY", "*NO", "*RQD").is_err());
        assert!(switch_matches("10000001", "1XXXXXX1").unwrap());
        assert!(!switch_matches("10000001", "0XXXXXXX").unwrap());
    }

    #[test]
    fn interactive_only_and_jobctl_parameters() {
        let cur = |_: &str| String::new();
        let e = change(
            "T1",
            &[("DEVRCYACN".into(), "*MSG".into())],
            &cur,
            false,
            true,
        );
        assert!(e.unwrap_err().to_string().starts_with("CPF1334"));
        let e = change("T1", &[("RUNPTY".into(), "30".into())], &cur, false, false);
        assert!(e.unwrap_err().to_string().starts_with("CPF1337"));
        // Nothing is changed when one value is wrong.
        let e = change(
            "T2",
            &[("JOBPTY".into(), "3".into()), ("OUTPTY".into(), "0".into())],
            &cur,
            false,
            true,
        );
        assert!(e.is_err());
        assert_eq!(get("T2", "JOBPTY"), None);
        change("T2", &[("JOBPTY".into(), "3".into())], &cur, false, true).unwrap();
        assert_eq!(get("T2", "JOBPTY").as_deref(), Some("3"));
        forget("T2");
        assert_eq!(get("T2", "JOBPTY"), None);
    }
}
