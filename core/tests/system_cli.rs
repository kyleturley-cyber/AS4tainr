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
    assert_eq!(
        (r.rc, ids(&r.err)),
        (255, vec!["CPD0084", "CPF0006"]),
        "{:?}",
        r.err
    );

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
    assert_eq!((r.rc, r.out.len(), r.err.len()), (255, 0, 0), "{:?}", r.err);

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
    assert_eq!(
        (r.rc, ids(&r.out)),
        (0, vec!["CPF2880", "CPF2889", "CPC2955"]),
        "{:?}",
        r.err
    );

    // Round 4 captures (IBM i 7.5, tools/golden/files-and-programs.txt):
    // CPYF CRTFILE(*YES) sends CPF2880 and CPF2889 before CPC2955;
    // MBROPT(*NONE) into a file that exists is CPF2863, CPF2817; a from-
    // file that is not there CPF2802, CPF2817; a to-file CPF2861, CPF2817;
    // different formats without FMTOPT CPF2963, CPF2817; FMTOPT(*NOCHK)
    // into longer records CPF4011, CPC2955. SAVOBJ completes with CPC3722,
    // CLRSAVF with CPC3725, CRTSAVF with CPC7301; DLTOVR FILE(*ALL) with no
    // overrides is CPF5809 (informational, status 0); DSPOBJD of a missing
    // object CPF2105; a CALL of a missing program inside a CL program is
    // CPF0001 there (MONMSG CPF0001 catches it).
    let r = system(
        &url,
        &[&format!(
            "CPYF FROMFILE({lib}/T1) TOFILE({lib}/T3) CRTFILE(*YES)"
        )],
    );
    assert_eq!(ids(&r.out), ["CPF2880", "CPF2889", "CPC2955"]);
    let r = system(
        &url,
        &[&format!("CPYF FROMFILE({lib}/T1) TOFILE({lib}/T3)")],
    );
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPF2863", "CPF2817"]));
    let r = system(
        &url,
        &[&format!("CPYF FROMFILE({lib}/NOSUCH) TOFILE({lib}/T3)")],
    );
    assert_eq!(ids(&r.err), ["CPF2802", "CPF2817"]);
    let r = system(
        &url,
        &[&format!(
            "CPYF FROMFILE({lib}/T1) TOFILE({lib}/NOSUCH) MBROPT(*ADD)"
        )],
    );
    assert_eq!(ids(&r.err), ["CPF2861", "CPF2817"]);
    assert_eq!(
        system(&url, &[&format!("CRTPF FILE({lib}/P30) RCDLEN(30)")]).rc,
        0
    );
    let r = system(
        &url,
        &[&format!(
            "CPYF FROMFILE({lib}/T1) TOFILE({lib}/P30) MBROPT(*ADD)"
        )],
    );
    assert_eq!(ids(&r.err), ["CPF2963", "CPF2817"]);
    let r = system(
        &url,
        &[&format!(
            "CPYF FROMFILE({lib}/T1) TOFILE({lib}/P30) MBROPT(*ADD) FMTOPT(*NOCHK)"
        )],
    );
    assert_eq!(
        (r.rc, ids(&r.out)),
        (0, vec!["CPF4011", "CPC2955"]),
        "{:?}",
        r.err
    );
    assert_eq!(
        ids(&system(&url, &[&format!("CRTSAVF FILE({lib}/SV)")]).out),
        ["CPC7301"]
    );
    let r = system(
        &url,
        &[&format!(
            "SAVOBJ OBJ(T1) LIB({lib}) DEV(*SAVF) SAVF({lib}/SV)"
        )],
    );
    assert_eq!(ids(&r.out), ["CPC3722"], "{:?}", r.err);
    assert_eq!(
        ids(&system(&url, &[&format!("CLRSAVF FILE({lib}/SV)")]).out),
        ["CPC3725"]
    );
    let r = system(&url, &["DLTOVR FILE(*ALL)"]);
    assert_eq!((r.rc, ids(&r.out)), (0, vec!["CPF5809"]));
    let r = system(
        &url,
        &[&format!("DSPOBJD OBJ({lib}/NOSUCH) OBJTYPE(*FILE)")],
    );
    assert_eq!((r.rc, ids(&r.err)), (255, vec!["CPF2105"]));
    // CL program: CALL of a missing program, MONMSG CPF0001.
    let src = format!(
        "PGM\n CALL PGM({lib}/NOSUCH)\n MONMSG MSGID(CPF0001) EXEC(SNDPGMMSG MSG(GOT0001) MSGTYPE(*COMP))\n MONMSG MSGID(CPF0006) EXEC(SNDPGMMSG MSG(GOT0006) MSGTYPE(*COMP))\nENDPGM\n"
    );
    let dir = std::env::temp_dir().join(format!("syscli{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.clle"), src).unwrap();
    let put = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .args(["--database-url", &url, "put"])
        .arg(dir.join("t.clle"))
        .arg(format!("/tmp/{lib}.clle"))
        .output()
        .unwrap();
    assert!(put.status.success());
    let r = system(
        &url,
        &[&format!(
            "CRTBNDCL PGM({lib}/MON1) SRCSTMF('/tmp/{lib}.clle')"
        )],
    );
    assert_eq!(r.rc, 0, "{:?}", r.err);
    let r = system(&url, &[&format!("CALL PGM({lib}/MON1)")]);
    assert_eq!(
        (r.rc, r.out.clone()),
        (0, vec!["GOT0001".to_string()]),
        "{:?}",
        r.err
    );

    assert_eq!(system(&url, &[&format!("DLTLIB {lib}")]).rc, 0);
}

/// A PASE shell's job, as a makefile uses it (`liblist -c LIB; system
/// "CRTBNDRPG ... SRCSTMF('qrpglesrc/x.rpgle')"`): `system` runs in the
/// shell's current directory, so relative path names start there (IBM's
/// integrated file system documentation: relative path names start in the
/// job's current directory, which PASE shares with the job); the current
/// library is where unqualified SQL objects are created with system naming
/// (Db2 for i SQL reference, "Unqualified names"); CHGCURDIR changes the
/// current directory (CL reference); printf writes the program's standard
/// output (ILE C runtime), which `system` writes out.
#[test]
fn pase_shell_job() {
    let Some(url) = db_url() else {
        eprintln!("skipped: AS4TAINR_TEST_DATABASE_URL not set");
        return;
    };
    let lib = format!("P{}", std::process::id() % 1_000_000);
    let run = |dir: &std::path::Path, curlib: Option<&str>, cmd: &str| -> Run {
        let mut c = Command::new(env!("CARGO_BIN_EXE_as4tainr"));
        c.current_dir(dir).args(["--database-url", &url]);
        if let Some(l) = curlib {
            c.args(["--curlib", l]);
        }
        let o = c.args(["system", cmd]).output().unwrap();
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
    };
    let dir = std::env::temp_dir().join(format!("pasejob{}", std::process::id()));
    std::fs::create_dir_all(dir.join("qrpglesrc")).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(
        dir.join("qrpglesrc/hello.rpgle"),
        "**free\nctl-opt dftactgrp(*no);\ndcl-pr printf int(10) extproc('printf');\n  f pointer value options(*string);\nend-pr;\nprintf('Hello ');\nprintf('world' + x'25');\nprintf('next' + x'25');\nreturn;\n",
    )
    .unwrap();
    std::fs::write(dir.join("sub/note.txt"), "x\n").unwrap();
    let d = dir.to_str().unwrap();
    let put = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .args(["--database-url", &url, "put", d, d])
        .output()
        .unwrap();
    assert!(put.status.success());
    assert_eq!(run(&dir, None, &format!("CRTLIB {lib}")).rc, 0);
    let r = run(
        &dir,
        Some(&lib),
        &format!("CRTBNDRPG PGM({lib}/HELLO) SRCSTMF('qrpglesrc/hello.rpgle')"),
    );
    assert_eq!(r.rc, 0, "{:?} {:?}", r.out, r.err);
    let r = run(&dir, Some(&lib), &format!("CALL {lib}/HELLO"));
    assert_eq!(r.out, ["Hello world", "next"], "{:?}", r.err);
    // The current library: CREATE TABLE without a library.
    let r = run(
        &dir,
        Some(&lib),
        "RUNSQL SQL('CREATE TABLE PASET1 (C INT)') COMMIT(*NONE) NAMING(*SYS)",
    );
    assert_eq!(r.rc, 0, "{:?} {:?}", r.out, r.err);
    assert_eq!(
        run(
            &dir,
            None,
            &format!("CHKOBJ OBJ({lib}/PASET1) OBJTYPE(*FILE)")
        )
        .rc,
        0
    );
    // CHGCURDIR, then a relative path, in one job (a CL program).
    std::fs::write(
        dir.join("cd.clle"),
        "PGM\n CHGCURDIR DIR('sub')\n MKDIR DIR('made')\nENDPGM\n",
    )
    .unwrap();
    let put = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .args(["--database-url", &url, "put"])
        .arg(dir.join("cd.clle"))
        .arg(format!("{d}/cd.clle"))
        .output()
        .unwrap();
    assert!(put.status.success());
    assert_eq!(
        run(
            &dir,
            None,
            &format!("CRTBNDCL PGM({lib}/CD) SRCSTMF('cd.clle')")
        )
        .rc,
        0
    );
    assert_eq!(run(&dir, None, &format!("CALL {lib}/CD")).rc, 0);
    let r = run(
        std::path::Path::new("/"),
        None,
        &format!("CHGCURDIR DIR('{d}/sub/made')"),
    );
    assert_eq!(r.rc, 0, "{:?} {:?}", r.out, r.err);
    let r = run(&dir, None, "CHGCURDIR DIR('nosuch')");
    assert_eq!(r.rc, 255);
    // A program's printed output under `system`: its spooled file is
    // the user's print job's (QPRTJOB), not the command's own job, so
    // DLTSPLF of JOB(*) does not find it (CPF3303), and its lines go to
    // standard output (PUB400 2026-10-26, tools/golden/spool.txt s_dump:
    // GLDSPD lists GLDSPL under QPRTJOB, its DLTSPLF ends with CPF3303).
    std::fs::write(
        dir.join("prt.rpgle"),
        "**free\nctl-opt dftactgrp(*no);\ndcl-f qsysprt printer(80) usropn;\ndcl-ds line len(80) end-ds;\ndcl-pr qcmdexc extpgm('QCMDEXC');\n  c char(200) const;\n  l packed(15:5) const;\nend-pr;\ndcl-ds psds psds;\n  excid char(7) pos(40);\nend-ds;\nopen qsysprt;\nline = 'PRINTED';\nwrite qsysprt line;\nclose qsysprt;\nmonitor;\n  qcmdexc('DLTSPLF FILE(QSYSPRT) SPLNBR(*LAST)' : 35);\n  dsply 'deleted';\non-error;\n  dsply excid;\nendmon;\nreturn;\n",
    )
    .unwrap();
    let put = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .args(["--database-url", &url, "put"])
        .arg(dir.join("prt.rpgle"))
        .arg(format!("{d}/prt.rpgle"))
        .output()
        .unwrap();
    assert!(put.status.success());
    let r = run(
        &dir,
        Some(&lib),
        &format!("CRTBNDRPG PGM({lib}/PRT) SRCSTMF('prt.rpgle')"),
    );
    assert_eq!(r.rc, 0, "{:?} {:?}", r.out, r.err);
    let r = run(&dir, Some(&lib), &format!("CALL {lib}/PRT"));
    assert!(
        r.out.iter().any(|l| l.contains("CPF3303")),
        "{:?} {:?}",
        r.out,
        r.err
    );
    assert!(r.out.iter().any(|l| l == "PRINTED"), "{:?}", r.out);
    // DSPSPLF does not run under `system` (CPD0031 then CPF0006: the
    // same capture, s_dspsplf). CRTMSGQ's completion message is CPC9801,
    // SNDMSG sends none, DLTMSGQ's is CPC2191 (PUB400 2026-10-26,
    // tools/golden/verify1.txt v_crtmsgq, v_sndmsg, v_dltmsgq).
    let r = run(&dir, None, "DSPSPLF FILE(QSYSPRT) SPLNBR(*LAST)");
    assert_eq!((ids(&r.err), r.rc), (vec!["CPD0031", "CPF0006"], 255));
    let r = run(
        &dir,
        None,
        &format!("CRTMSGQ MSGQ({lib}/MQ) TEXT('golden')"),
    );
    assert_eq!((ids(&r.out), r.rc), (vec!["CPC9801"], 0), "{:?}", r.err);
    let r = run(&dir, None, &format!("SNDMSG MSG('hello') TOMSGQ({lib}/MQ)"));
    assert_eq!((r.out.len(), r.rc), (0, 0), "{:?}", r.out);
    let r = run(&dir, None, &format!("DLTMSGQ MSGQ({lib}/MQ)"));
    assert_eq!((ids(&r.out), r.rc), (vec!["CPC2191"], 0));
    assert_eq!(run(&dir, None, &format!("DLTLIB {lib}")).rc, 0);
}

/// REXX/400: STRREXPRC runs a procedure from a source member (SRCFILE,
/// SRCMBR, PARM): SAY writes to standard output; PARSE (words: the last
/// variable keeps all but one separating blank, IBM's REXX parsing
/// documentation); DO loops; compound variables; SELECT; internal
/// routines with PROCEDURE and RETURN (RESULT); built-in functions;
/// decimal arithmetic to 9 digits (1/3 = 0.333333333); CL commands: RC is
/// 0 or the escape message identifier, `&name` is a REXX variable that a
/// retrieve command fills; SIGNAL ON ERROR with SIGL; a non-zero EXIT is
/// the escape message CPF7CFF.
#[test]
fn rexx_procedures() {
    let Some(url) = db_url() else {
        eprintln!("skipped: AS4TAINR_TEST_DATABASE_URL not set");
        return;
    };
    let lib = format!("X{}", std::process::id() % 1_000_000);
    let src = "/* a test procedure */
parse arg a b
say 'args:' a '/' b
total = 0
do i = 1 to 5
  total = total + i
end
say 'total' total
say 'div' 1/3 7%2 7//2 2**10 1.50+1
s.1 = 'x'; s.2 = 'y'; s.0 = 2
do k = 1 to s.0; say 'stem' k s.k; end
say substr('abcdef', 2, 3) strip('  hi  ') words('a b c') c2x('A') x2c('C1') left('ab', 4, '.')'|'
select
  when a = 'one' then say 'select one'
  otherwise say 'select other'
end
call twice 3
say 'result' result twice(5)
'CHKOBJ OBJ(QSYS/NOSUCHOBJ) OBJTYPE(*PGM)'
say 'rc' rc
user = '          '
'RTVJOBA USER(&user)'
say 'user' strip(user)
parse value 'More  words    in data' with v1 v2 v3
say '['v3']'
signal on error
'CHKOBJ OBJ(QSYS/NOSUCHOBJ) OBJTYPE(*PGM)'
say 'not reached'
exit 0
twice: procedure
  parse arg n
  return n * 2
error:
  say 'trapped' rc 'line' sigl
  exit 3
";
    let dir = std::env::temp_dir().join(format!("rexx{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.rexx"), src).unwrap();
    let put = Command::new(env!("CARGO_BIN_EXE_as4tainr"))
        .args(["--database-url", &url, "put"])
        .arg(dir.join("t.rexx"))
        .arg(format!("/tmp/{lib}.rexx"))
        .output()
        .unwrap();
    assert!(put.status.success());
    for c in [
        format!("CRTLIB {lib}"),
        format!("CRTSRCPF FILE({lib}/QREXSRC) RCDLEN(112)"),
        format!(
            "CPYFRMSTMF FROMSTMF('/tmp/{lib}.rexx') TOMBR('/QSYS.LIB/{lib}.LIB/QREXSRC.FILE/T.MBR')"
        ),
    ] {
        let r = system(&url, &[&c]);
        assert_eq!(r.rc, 0, "{c}: {:?}", r.err);
    }
    let r = system(
        &url,
        &[&format!(
            "STRREXPRC SRCMBR(T) SRCFILE({lib}/QREXSRC) PARM('one two')"
        )],
    );
    assert_eq!(
        r.out[..r.out.len().min(15)],
        [
            "args: one / two",
            "total 15",
            "div 0.333333333 3 1 1024 2.50",
            "stem 1 x",
            "stem 2 y",
            "bcd hi 3 C1 A ab..|",
            "select one",
            "result 6 10",
            "rc CPF9801",
            "user QSECOFR",
            "[   in data]",
            "trapped CPF9801 line 27",
        ][..],
        "{:?} {:?}",
        r.out,
        r.err
    );
    // EXIT 3: CPF7CFF.
    assert_eq!(r.rc, 255);
    assert_eq!(ids(&r.err).last(), Some(&"CPF7CFF"), "{:?}", r.err);
    assert_eq!(system(&url, &[&format!("DLTLIB {lib}")]).rc, 0);
}
