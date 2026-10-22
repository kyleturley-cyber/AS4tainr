//! `as4tainr system`, which behaves as IBM i's PASE `system` utility.
//!
//! Expected values: IBM's documentation of the PASE for i system utility
//! (options -h, -n, -q, -s, -v; exit status 255 when the command sends an
//! escape message; messages to standard output, or standard error for a
//! command that failed) and captures made on IBM i 7.5 (PUB400, 2026-10-21,
//! tools/golden.py): a usage error exits with 2; the command analyzer's
//! diagnostic (CPD0030 for an unknown command, CPD0084 for a value not
//! valid) comes before the escape message CPF0006; CHKOBJ of a missing
//! object sends CPF9801; CRTDTAARA completes with CPC0904, DLTDTAARA and
//! DLTF with CPC2191, and DLTDTAARA of a missing data area fails with
//! CPF2105; CRTPF RCDLEN(80) sends CPC7301 then CPC7305 (its member).
//! Only message IDs and exit codes are compared: the texts are as4tainr's.

use std::process::Command;

fn db_url() -> Option<String> {
    std::env::var("AS4TAINR_TEST_DATABASE_URL").ok()
}

struct Run {
    out: Vec<String>,
    err: Vec<String>,
    rc: i32,
}

fn system(url: &str, args: &[&str]) -> Run {
    let o = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .arg("--database-url")
        .arg(url)
        .arg("system")
        .args(args)
        .output()
        .unwrap();
    let lines = |b: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::to_string)
            .collect()
    };
    Run {
        out: lines(&o.stdout),
        err: lines(&o.stderr),
        rc: o.status.code().unwrap_or(-1),
    }
}

fn ids(lines: &[String]) -> Vec<&str> {
    lines
        .iter()
        .map(|l| l.split(':').next().unwrap_or(""))
        .collect()
}

#[test]
fn pase_system_utility() {
    let Some(url) = db_url() else {
        eprintln!("skipped: AS4TAINR_TEST_DATABASE_URL not set");
        return;
    };
    let lib = format!("S{}", std::process::id() % 1_000_000_000);
    let _ = system(&url, &[&format!("DLTLIB {lib}")]);
    assert_eq!(system(&url, &[&format!("CRTLIB {lib}")]).rc, 0);

    // No command: a usage error.
    let r = system(&url, &[]);
    assert_eq!(r.rc, 2);
    assert!(r.out.is_empty() && !r.err.is_empty());

    // An unknown command: CPD0030, then CPF0006, on standard error.
    let r = system(&url, &["NOSUCHCMD"]);
    assert_eq!(r.rc, 255);
    assert_eq!(ids(&r.err), ["CPD0030", "CPF0006"]);
    assert!(r.out.is_empty());

    // A value not valid: CPD0084, then CPF0006.
    let r = system(&url, &[&format!("CRTDTAARA DTAARA({lib}/X) TYPE(*BAD)")]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPD0084", "CPF0006"]));

    // CHKOBJ of an object that is not there: CPF9801; -n leaves the ID
    // out; -v writes the command first, on standard output.
    let chk = format!("CHKOBJ OBJ({lib}/NOSUCH) OBJTYPE(*FILE)");
    let r = system(&url, &[&chk]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPF9801"]));
    let r = system(&url, &["-n", &chk]);
    assert_eq!(r.rc, 255);
    assert_eq!(r.err.len(), 1);
    assert!(!r.err[0].starts_with("CPF9801"), "{:?}", r.err);
    let r = system(&url, &["-v", &chk]);
    assert_eq!(r.out.len(), 1);
    assert!(r.out[0].contains(&chk), "{:?}", r.out);
    // -q: no messages at all, the exit status still tells.
    let r = system(&url, &["-q", &chk]);
    assert_eq!((r.rc, r.out.len(), r.err.len()), (255, 0, 0));

    // Completion messages on standard output, exit status 0. The command
    // may be given in pieces (joined with blanks).
    let r = system(
        &url,
        &[
            "CRTDTAARA",
            &format!("DTAARA({lib}/GOLDDA) TYPE(*CHAR) LEN(10) VALUE(ABC)"),
        ],
    );
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPC0904"]));
    assert!(r.err.is_empty());

    // Printed output on standard output; -s leaves it out.
    let dsp = format!("DSPDTAARA DTAARA({lib}/GOLDDA)");
    let r = system(&url, &[&dsp]);
    assert_eq!(r.rc, 0);
    assert!(r.out.iter().any(|l| l.contains("ABC")), "{:?}", r.out);
    let r = system(&url, &["-s", &dsp]);
    assert_eq!((r.rc, r.out.len()), (0, 0));

    let dlt = format!("DLTDTAARA DTAARA({lib}/GOLDDA)");
    let r = system(&url, &[&dlt]);
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPC2191"]));
    let r = system(&url, &[&dlt]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPF2105"]));

    // CRTPF RCDLEN: the file and its member.
    let r = system(&url, &[&format!("CRTPF FILE({lib}/GOLDTEST) RCDLEN(80)")]);
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPC7301", "CPC7305"]));
    let r = system(&url, &[&format!("DSPFD FILE({lib}/GOLDTEST) TYPE(*ATR)")]);
    assert_eq!(r.rc, 0);
    assert!(
        r.out
            .iter()
            .any(|l| l.to_ascii_lowercase().contains("record length")
                && l.trim_end().ends_with(" 80")),
        "{:?}",
        r.out
    );
    let r = system(&url, &[&format!("DLTF FILE({lib}/GOLDTEST)")]);
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPC2191"]));

    // Round 3 captures (IBM i 7.5): RUNSQL sends no completion message
    // and fails with SQL9010 only; CALL of a missing program is CPD0170,
    // CPF0006; DLTPGM of a missing program CPF2105; CPYF completes with
    // CPC2955.
    let r = system(
        &url,
        &[&format!(
            "RUNSQL SQL('CREATE TABLE {lib}.T1 (ID INT PRIMARY KEY)') COMMIT(*NONE)"
        )],
    );
    assert_eq!((r.rc, r.out.len()), (0, 0), "{:?}", r.err);
    let ins = format!("RUNSQL SQL('INSERT INTO {lib}.T1 VALUES(1)') COMMIT(*NONE)");
    let r = system(&url, &[&ins]);
    assert_eq!((r.rc, r.out.len(), r.err.len()), (0, 0, 0));
    // The same key again: a duplicate key error, which RUNSQL reports as
    // SQL9010 alone.
    let r = system(&url, &[&ins]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["SQL9010"]));
    let r = system(&url, &[&format!("CALL PGM({lib}/NOSUCH)")]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPD0170", "CPF0006"]));
    let r = system(&url, &[&format!("DLTPGM PGM({lib}/NOSUCH)")]);
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPF2105"]));
    let r = system(
        &url,
        &[&format!(
            "CPYF FROMFILE({lib}/T1) TOFILE({lib}/T2) CRTFILE(*YES)"
        )],
    );
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPC2955"]), "{:?}", r.err);

    assert_eq!(system(&url, &[&format!("DLTLIB {lib}")]).rc, 0);
}
