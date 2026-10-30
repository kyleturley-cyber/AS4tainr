//! Integration tests: the catalog, record-level access, CL, jobs and the RPG engine.
//!
//! Needs a PostgreSQL database. Set AS4TAINR_TEST_DATABASE_URL, e.g.
//! `postgres://as4tainr:as4tainr@127.0.0.1/as4tainr`. Without it, tests are skipped.
//! Each test creates its own library, so tests can run in parallel.

use as4tainr_core::{catalog, dds, rla, runtime};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio_postgres::Client;

const CUSTMAST: &str = include_str!("../../samples/qddssrc/CUSTMAST.pf");
const ORDLIN: &str = include_str!("../../samples/qddssrc/ORDLIN.pf");

static SEQ: AtomicU32 = AtomicU32::new(0);

fn db_url() -> Option<String> {
    std::env::var("AS4TAINR_TEST_DATABASE_URL").ok()
}

macro_rules! require_db {
    () => {
        match db_url() {
            Some(u) => u,
            None => {
                eprintln!("skipped: AS4TAINR_TEST_DATABASE_URL not set");
                return;
            }
        }
    };
}

/// A fresh library with CUSTMAST and ORDLIN created and loaded.
async fn setup(url: &str) -> (Client, String) {
    let mut client = runtime::connect_db(url).await.unwrap();
    catalog::init(&client).await.unwrap();
    let lib = format!(
        "T{}{:03}",
        std::process::id() % 100000,
        SEQ.fetch_add(1, Ordering::SeqCst)
    );
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {} CASCADE; DELETE FROM as4tainr.objects WHERE library='{lib}' OR name='{lib}';
             DELETE FROM as4tainr.files WHERE library='{lib}'; DELETE FROM as4tainr.file_fields WHERE library='{lib}';
             DELETE FROM as4tainr.file_keys WHERE library='{lib}';
             DELETE FROM as4tainr.data_areas WHERE library='{lib}';
             DELETE FROM as4tainr.message_descriptions WHERE library='{lib}';",
            catalog::ident(&lib)
        ))
        .await
        .unwrap();
    catalog::create_library(&client, &lib, "test")
        .await
        .unwrap();
    for (name, src, csv) in [
        (
            "CUSTMAST",
            CUSTMAST,
            include_str!("../../samples/data/CUSTMAST.csv"),
        ),
        (
            "ORDLIN",
            ORDLIN,
            include_str!("../../samples/data/ORDLIN.csv"),
        ),
    ] {
        let pf = dds::parse_pf(name, src).unwrap();
        catalog::create_physical_file(&mut client, &lib, &pf)
            .await
            .unwrap();
        let f = catalog::resolve_file(&client, std::slice::from_ref(&lib), name)
            .await
            .unwrap();
        let mut of = rla::OpenFile::new(f);
        let mut lines = csv.lines();
        let header: Vec<String> = lines.next().unwrap().split(',').map(String::from).collect();
        for line in lines {
            let vals = split_csv(line);
            let rec: rla::Record = header.iter().cloned().zip(vals).collect();
            of.write(&client, &rec).await.unwrap();
        }
    }
    (client, lib)
}

fn split_csv(line: &str) -> Vec<String> {
    // The sample data has one quoted field containing a comma.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    for c in line.chars() {
        match c {
            '"' => q = !q,
            ',' if !q => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

async fn open(client: &Client, lib: &str, name: &str) -> rla::OpenFile {
    rla::OpenFile::new(
        catalog::resolve_file(client, &[lib.to_string()], name)
            .await
            .unwrap(),
    )
}

fn k(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn field(r: &rla::ReadResult, f: &str) -> String {
    r.record.as_ref().unwrap()[f].clone()
}

#[tokio::test]
async fn read_returns_key_order_not_arrival_order() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "CUSTMAST").await;
    let mut seen = Vec::new();
    loop {
        let r = f.read(&client).await.unwrap();
        if r.eof {
            break;
        }
        seen.push(field(&r, "CUSNO"));
    }
    assert_eq!(seen, ["1", "2", "3", "4", "5"]);
    assert!(f.eof());
    // Reading past the end stays at end.
    assert!(f.read(&client).await.unwrap().eof);
}

#[tokio::test]
async fn chain_found_and_not_found_with_field_formats() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "CUSTMAST").await;

    let r = f.chain(&client, &k(&["1"])).await.unwrap();
    assert!(r.found);
    assert!(!r.eof, "CHAIN never sets %EOF");
    // Character fields keep their full fixed length with trailing blanks.
    assert_eq!(field(&r, "CUSNAM"), format!("{:<30}", "Acme Tooling"));
    // Packed fields keep their declared scale.
    assert_eq!(field(&r, "CUSBAL"), "1250.50");

    let r = f.chain(&client, &k(&["00005"])).await.unwrap();
    assert!(r.found, "numeric keys accept leading zeros");
    assert_eq!(field(&r, "CUSBAL"), "-75.25");
    assert_eq!(field(&r, "CUSNAM").trim_end(), "Lake, Pine & Co");

    let r = f.chain(&client, &k(&["42"])).await.unwrap();
    assert!(!r.found);
    assert!(r.record.is_none());
}

#[tokio::test]
async fn chain_then_read_continues_after_the_record() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "CUSTMAST").await;
    f.chain(&client, &k(&["3"])).await.unwrap();
    assert_eq!(field(&f.read(&client).await.unwrap(), "CUSNO"), "4");
    assert_eq!(
        field(&f.read_previous(&client).await.unwrap(), "CUSNO"),
        "3"
    );
}

#[tokio::test]
async fn setll_partial_key_then_reade_reads_one_order() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "ORDLIN").await;

    assert!(
        f.setll(&client, &k(&["1002"])).await.unwrap(),
        "%EQUAL is on"
    );
    let mut lines = Vec::new();
    loop {
        let r = f.read_equal(&client, None).await.unwrap();
        if r.eof {
            break;
        }
        lines.push((field(&r, "LINNO"), field(&r, "ITEM").trim_end().to_string()));
    }
    assert_eq!(
        lines,
        [
            ("1".to_string(), "NUT-M8".to_string()),
            ("2".to_string(), "BOLT-M8".to_string()),
            ("3".to_string(), "WASHER-M8".to_string())
        ]
    );
    // After READE hits the end of the key range, READ moves on to the next order.
    assert_eq!(field(&f.read(&client).await.unwrap(), "ORDNO"), "1003");

    // SETLL on a missing key: %EQUAL off, READ gets the next higher key.
    assert!(!f.setll(&client, &k(&["1000"])).await.unwrap());
    assert_eq!(field(&f.read(&client).await.unwrap(), "ORDNO"), "1001");
}

#[tokio::test]
async fn setgt_and_readpe_walk_backwards() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "ORDLIN").await;
    f.setgt(&client, &k(&["1002"])).await.unwrap();
    let mut back = Vec::new();
    loop {
        let r = f.read_previous_equal(&client, None).await.unwrap();
        if r.eof {
            break;
        }
        back.push(field(&r, "LINNO"));
    }
    assert_eq!(back, ["3", "2", "1"]);

    // SETGT *HIVAL then READP gives the last record.
    f.setgt(&client, &[]).await.unwrap();
    let r = f.read_previous(&client).await.unwrap();
    assert_eq!(
        (field(&r, "ORDNO"), field(&r, "LINNO")),
        ("1003".into(), "1".into())
    );
}

#[tokio::test]
async fn full_key_chain_on_composite_key() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "ORDLIN").await;
    let r = f.chain(&client, &k(&["1002", "2"])).await.unwrap();
    assert_eq!(field(&r, "ITEM").trim_end(), "BOLT-M8");
    assert_eq!(field(&r, "PRICE"), "0.15");
    assert!(
        f.chain(&client, &k(&["1", "2", "3"])).await.is_err(),
        "too many key values"
    );
}

#[tokio::test]
async fn update_and_delete_act_on_the_last_record_read() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "CUSTMAST").await;

    let err = f
        .update(&client, &rla::Record::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("CPF5013"), "{err}");

    f.chain(&client, &k(&["2"])).await.unwrap();
    let mut rec = rla::Record::new();
    rec.insert("CUSBAL".into(), "0.10".into());
    f.update(&client, &rec).await.unwrap();
    assert_eq!(
        field(&f.chain(&client, &k(&["2"])).await.unwrap(), "CUSBAL"),
        "0.10"
    );

    f.chain(&client, &k(&["3"])).await.unwrap();
    f.delete(&client).await.unwrap();
    // The position survives the delete: READ continues with customer 4.
    assert_eq!(field(&f.read(&client).await.unwrap(), "CUSNO"), "4");
    assert!(!f.chain(&client, &k(&["3"])).await.unwrap().found);
}

#[tokio::test]
async fn unique_file_rejects_duplicate_key_and_numeric_overflow() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "CUSTMAST").await;
    let mut rec = rla::Record::new();
    rec.insert("CUSNO".into(), "1".into());
    let err = f.write(&client, &rec).await.unwrap_err().to_string();
    assert!(err.contains("CPF5026"), "{err}");

    rec.insert("CUSNO".into(), "6".into());
    rec.insert("CUSBAL".into(), "10000000.00".into()); // 9P 2 holds 7 integer digits
    let err = f.write(&client, &rec).await.unwrap_err().to_string();
    assert!(err.contains("RNX0103"), "{err}");
}

#[tokio::test]
async fn duplicate_keys_come_back_in_arrival_order() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "ORDLIN").await;
    for item in ["FIRST", "SECOND", "THIRD"] {
        let mut rec = rla::Record::new();
        rec.insert("ORDNO".into(), "2000".into());
        rec.insert("LINNO".into(), "1".into());
        rec.insert("ITEM".into(), item.into());
        f.write(&client, &rec).await.unwrap();
    }
    f.setll(&client, &k(&["2000", "1"])).await.unwrap();
    let mut items = Vec::new();
    loop {
        let r = f.read_equal(&client, None).await.unwrap();
        if r.eof {
            break;
        }
        items.push(field(&r, "ITEM").trim_end().to_string());
    }
    assert_eq!(items, ["FIRST", "SECOND", "THIRD"]);
}

#[tokio::test]
async fn library_list_resolution() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let err = catalog::resolve_file(&client, &["QGPL".into()], "CUSTMAST").await;
    // QGPL may contain a CUSTMAST from manual runs; qualified names always work.
    let _ = err;
    let f = catalog::resolve_file(&client, &[], &format!("{lib}/custmast"))
        .await
        .unwrap();
    assert_eq!(f.library, lib);
    let err = catalog::resolve_file(&client, std::slice::from_ref(&lib), "NOPE")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("CPF9812"), "{err}");
}

/// Run an RPG program (source text) on the native engine in the test
/// library, as `user`, with an optional display and job.
async fn run_rpg(
    url: &str,
    lib: &str,
    user: &str,
    name: &str,
    source: &str,
    display: Option<runtime::DisplayHandle>,
    job: Option<String>,
) -> as4tainr_core::rpg::CallResult {
    as4tainr_core::rpg::call(
        url,
        vec![lib.to_string()],
        user.to_string(),
        job,
        display,
        None,
        name.into(),
        source.into(),
        vec![],
    )
    .await
    .unwrap()
}

/// What a test looks at on a screen before answering it.
type Check = Box<dyn FnOnce(&as4tainr_core::workstn::Frame) + Send>;

/// A display stand-in: answers each EXFMT with the next response, after
/// letting `check` look at what would be on the screen.
fn operator(
    mut answers: Vec<(as4tainr_core::dspf::Response, Check)>,
) -> (runtime::DisplayHandle, tokio::task::JoinHandle<usize>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<runtime::DisplayRequest>(1);
    answers.reverse();
    let task = tokio::spawn(async move {
        let mut n = 0;
        while let Some(req) = rx.recv().await {
            let (resp, check) = answers.pop().expect("more screens than expected");
            check(&req.frame);
            n += 1;
            let _ = req.reply.send(Ok(resp));
        }
        n
    });
    (tx, task)
}

fn press(aid: &str, values: &[(&str, &str)], ind: &[(u8, bool)]) -> as4tainr_core::dspf::Response {
    as4tainr_core::dspf::Response {
        aid: aid.into(),
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        indicators: ind.iter().copied().collect(),
        changed: Default::default(),
    }
}

/// EXFMT on the native engine (sample CUSTINQ, display file CUSTINQD):
/// the record's fields and indicators go to the job's display; what the
/// operator typed, the response indicators (CA03 sets 03) and *INKx come
/// back. An unknown customer turns on *IN30, which conditions ERRMSG.
#[tokio::test(flavor = "multi_thread")]
async fn rpg_exfmt_with_a_display() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    catalog::create_display_file(
        &mut client,
        &lib,
        "CUSTINQD",
        include_str!("../../samples/qddssrc/CUSTINQD.dspf"),
    )
    .await
    .unwrap();
    let (tx, seen) = operator(vec![
        (
            press("ENTER", &[("CUSNUM", "42")], &[]),
            Box::new(|f| {
                let l = f.layers.last().unwrap();
                assert_eq!(l.format, "PROMPT");
                assert!(!l.ind[30]);
            }),
        ),
        (
            press("F3", &[], &[(3, true)]),
            Box::new(|f| {
                let l = f.layers.last().unwrap();
                assert_eq!(l.format, "PROMPT");
                assert!(l.ind[30], "*IN30 is on after CHAIN found nothing");
                assert_eq!(l.values.get("CUSNUM").map(String::as_str), Some("42"));
            }),
        ),
    ]);
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "CUSTINQ",
        include_str!("../../samples/qrpglesrc/CUSTINQ.rpgle"),
        Some(tx),
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(seen.await.unwrap(), 2);

    // Without a display (batch), EXFMT fails with a clear message.
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "CUSTINQ",
        include_str!("../../samples/qrpglesrc/CUSTINQ.rpgle"),
        None,
        None,
    )
    .await;
    let e = format!("{:#}", r.error.unwrap());
    assert!(e.contains("no display"), "{e}");
}

/// A load-all subfile on the native engine (sample WRKCUST, display file
/// CUSTWRKD): every customer is written to the subfile; the control record
/// is shown with FOOTER kept (OVERLAY); the operator types 5 on line 2,
/// READC returns it and the program shows that customer; UPDATE clears the
/// option; F3 ends.
#[tokio::test(flavor = "multi_thread")]
async fn rpg_subfile_with_a_display() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    catalog::create_display_file(
        &mut client,
        &lib,
        "CUSTWRKD",
        include_str!("../../samples/qddssrc/CUSTWRKD.dspf"),
    )
    .await
    .unwrap();
    let mut opt5 = press("ENTER", &[], &[]);
    opt5.changed
        .insert(2, [("OPT".to_string(), "5".to_string())].into());
    let (tx, seen) = operator(vec![
        (
            opt5,
            Box::new(|f| {
                let names: Vec<&str> = f.layers.iter().map(|l| l.format.as_str()).collect();
                assert_eq!(
                    names,
                    ["FOOTER", "CUSTCTL"],
                    "FOOTER stays: CUSTCTL has OVERLAY"
                );
                let sf = f.subfile.as_ref().unwrap();
                assert!(sf.show);
                assert_eq!(sf.rows.len(), 5);
            }),
        ),
        (
            press("F12", &[], &[(12, true)]),
            Box::new(|f| {
                let l = f.layers.last().unwrap();
                assert_eq!(l.format, "DETAIL");
                assert_eq!(l.values.get("CUSNO").map(|v| v.trim()), Some("2"));
            }),
        ),
        (
            press("F3", &[], &[(3, true)]),
            Box::new(|f| {
                let sf = f.subfile.as_ref().unwrap();
                assert_eq!(sf.rows[1].values.get("OPT").map(|v| v.trim()), Some(""));
            }),
        ),
    ]);
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "WRKCUST",
        include_str!("../../samples/qrpglesrc/WRKCUST.rpgle"),
        Some(tx),
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(seen.await.unwrap(), 3);
}

/// Regression: numeric keys must sort numerically. With unqualified ORDER BY
/// columns PostgreSQL sorted by the text output, so 10 came before 9 and
/// sequential reads stopped early on files with keys of different widths.
#[tokio::test]
async fn numeric_keys_of_different_widths_sort_numerically() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut f = open(&client, &lib, "ORDLIN").await;
    for (ord, lin) in [
        ("100", "1"),
        ("9", "10"),
        ("10", "2"),
        ("9", "9"),
        ("99", "1"),
    ] {
        let mut rec = rla::Record::new();
        rec.insert("ORDNO".into(), ord.into());
        rec.insert("LINNO".into(), lin.into());
        f.write(&client, &rec).await.unwrap();
    }
    f.setll(&client, &[]).await.unwrap();
    let mut seen = Vec::new();
    loop {
        let r = f.read(&client).await.unwrap();
        if r.eof {
            break;
        }
        seen.push(format!("{}/{}", field(&r, "ORDNO"), field(&r, "LINNO")));
    }
    assert_eq!(
        seen,
        [
            "9/9", "9/10", "10/2", "99/1", "100/1", "1001/1", "1002/1", "1002/2", "1002/3",
            "1003/1"
        ]
    );
    // And backwards.
    f.setgt(&client, &[]).await.unwrap();
    let mut back = Vec::new();
    loop {
        let r = f.read_previous(&client).await.unwrap();
        if r.eof {
            break;
        }
        back.push(format!("{}/{}", field(&r, "ORDNO"), field(&r, "LINNO")));
    }
    seen.reverse();
    assert_eq!(back, seen);
}

/// DSPPFM reads a member in arrival order (relative record number), not
/// key order: the CSV loads customer 3 first.
#[tokio::test]
async fn member_records_in_arrival_order() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let f = catalog::resolve_file(&client, std::slice::from_ref(&lib), "CUSTMAST")
        .await
        .unwrap();
    let recs = catalog::member_records(&client, &f, 100).await.unwrap();
    assert_eq!(recs.len(), 5);
    assert_eq!(recs[0].1[0], "3", "first record written comes first");
    assert_eq!(recs[0].1[1], "Bluebird Supply");
    assert_eq!(recs[1].1[3], "1250.50", "numbers keep their scale");
    assert_eq!(
        catalog::member_records(&client, &f, 2).await.unwrap().len(),
        2
    );
}

/// CRTUSRPRF / DSPUSRPRF / DLTUSRPRF at the catalog level.
#[tokio::test]
async fn user_profiles_create_get_delete() {
    use as4tainr_core::users;
    let url = require_db!();
    let client = runtime::connect_db(&url).await.unwrap();
    catalog::init(&client).await.unwrap();
    let name = format!("U{}", std::process::id() % 1_000_000);
    users::delete(&client, &name).await.unwrap();
    let new = |password| users::NewProfile {
        name: name.clone(),
        password,
        expired: false,
        enabled: true,
        usrcls: "*PGMR".into(),
        special: users::class_special_authorities("*PGMR"),
        curlib: "QGPL".into(),
        text: "Test profile".into(),
        inlpgm: users::DEFAULT_INLPGM.into(),
        inlmnu: users::DEFAULT_INLMNU.into(),
    };
    users::create(&client, new(users::NewPassword::SameAsProfile))
        .await
        .unwrap();
    // PASSWORD(*USRPRF): the password is the profile name.
    users::authenticate(&client, &name, &name).await.unwrap();
    assert!(
        users::create(&client, new(users::NewPassword::None))
            .await
            .is_err(),
        "already exists"
    );
    let p = users::get(&client, &name).await.unwrap().unwrap();
    assert_eq!(
        (p.usrcls.as_str(), p.text.as_str()),
        ("*PGMR", "Test profile")
    );
    assert!(p.special_auth.is_empty());
    assert!(!users::has_special(&client, &name, "*SECADM").await.unwrap());
    assert!(users::has_special(&client, "QSECOFR", "*SECADM")
        .await
        .unwrap());
    assert!(users::delete(&client, &name).await.unwrap());
    assert!(users::get(&client, &name).await.unwrap().is_none());
    // PASSWORD(*NONE): the profile cannot sign on.
    users::create(&client, new(users::NewPassword::None))
        .await
        .unwrap();
    assert!(users::authenticate(&client, &name, "").await.is_err());
    assert!(users::authenticate(&client, &name, "*NONE").await.is_err());
    users::delete(&client, &name).await.unwrap();
}

/// A password stored before as4tainr followed QPWDLVL (hashed exactly as
/// typed, mixed case) still signs on, and is stored upper case from then
/// on, so any case works afterwards (QPWDLVL 0). `as4tainr rstpwd` resets
/// a password to the profile name, expired.
#[tokio::test]
async fn passwords_from_before_qpwdlvl_still_work() {
    use as4tainr_core::users;
    let url = require_db!();
    let client = runtime::connect_db(&url).await.unwrap();
    catalog::init(&client).await.unwrap();
    let name = format!("L{}", std::process::id() % 1_000_000);
    users::delete(&client, &name).await.unwrap();
    users::create(
        &client,
        users::NewProfile {
            name: name.clone(),
            password: users::NewPassword::None,
            expired: false,
            enabled: true,
            usrcls: "*USER".into(),
            special: vec![],
            curlib: "QGPL".into(),
            text: String::new(),
            inlpgm: users::DEFAULT_INLPGM.into(),
            inlmnu: users::DEFAULT_INLMNU.into(),
        },
    )
    .await
    .unwrap();
    // The old way: the hash of the password exactly as typed.
    client
        .execute(
            "UPDATE as4tainr.user_profiles SET password_hash = $2 WHERE name = $1",
            &[&name, &users::hash_password("Green5creen!").unwrap()],
        )
        .await
        .unwrap();
    assert!(users::authenticate(&client, &name, "green5creen!")
        .await
        .is_err());
    users::authenticate(&client, &name, "Green5creen!")
        .await
        .unwrap();
    // Re-stored upper case: now any case signs on.
    users::authenticate(&client, &name, "green5creen!")
        .await
        .unwrap();
    users::authenticate(&client, &name, "GREEN5CREEN!")
        .await
        .unwrap();

    assert!(users::reset_password(&client, &name).await.unwrap());
    let p = users::authenticate(&client, &name, &name.to_lowercase())
        .await
        .unwrap();
    assert!(p.pwd_expired, "a reset password must be changed");
    assert!(!users::reset_password(&client, "NOSUCHUSR").await.unwrap());
    users::delete(&client, &name).await.unwrap();
}

/// The library list of a job, as IBM documents it: the system part
/// (QSYS, QSYS2, QHLPSYS, QUSRSYS), then the current library if there is
/// one, then the user part (QGPL, QTEMP as shipped). Only the user part
/// and the current library can be changed.
#[tokio::test]
async fn job_library_list_commands() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec!["QGPL".into(), "QTEMP".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let types = |job: &Job| -> Vec<String> {
        job.library_list_entries()
            .iter()
            .map(|(l, t)| format!("{l} {t}"))
            .collect()
    };
    assert_eq!(
        types(&job),
        [
            "QSYS SYS",
            "QSYS2 SYS",
            "QHLPSYS SYS",
            "QUSRSYS SYS",
            "QGPL USR",
            "QTEMP USR"
        ]
    );
    assert_eq!(job.current_library(), "QGPL", "*CRTDFT creates in QGPL");

    // DSPLIBL: one row per library, option 5 runs DSPLIB on it.
    let Ok(Outcome::Display(d)) = job.run("dsplibl").await else {
        panic!()
    };
    assert_eq!(d.rows.len(), 6);
    assert_eq!(d.keys[4], "QGPL");
    assert_eq!(d.options[0].0, 5);
    assert_eq!(d.options[0].2.replace("{}", &d.keys[4]), "DSPLIB LIB(QGPL)");
    assert!(
        d.rows[4].contains("General purpose library"),
        "{}",
        d.rows[4]
    );

    job.run(&format!("chgcurlib {lib}")).await.unwrap();
    assert_eq!(types(&job)[4], format!("{lib} CUR"));
    assert!(job
        .run(&format!("addlible {lib}"))
        .await
        .unwrap_err()
        .to_string()
        .starts_with("CPF2103"));
    assert!(job
        .run("chgcurlib nosuchlib")
        .await
        .unwrap_err()
        .to_string()
        .starts_with("CPF2110"));
    job.run("chgcurlib *crtdft").await.unwrap();
    assert!(!types(&job).iter().any(|t| t.ends_with("CUR")));

    job.run(&format!("addlible {lib} *last")).await.unwrap();
    assert_eq!(types(&job).last().unwrap(), &format!("{lib} USR"));
    job.run("rmvlible qtemp").await.unwrap();
    assert!(!job.library_list().contains(&"QTEMP".to_string()));
    assert!(
        job.run("rmvlible qsys").await.is_err(),
        "the system part cannot be changed"
    );
    assert!(
        job.run("rmvlible qtemp").await.is_err(),
        "not in the list any more"
    );

    job.run(&format!("chglibl libl(qtemp {lib}) curlib(qgpl)"))
        .await
        .unwrap();
    assert_eq!(
        types(&job)[4..],
        [
            "QGPL CUR".to_string(),
            "QTEMP USR".into(),
            format!("{lib} USR")
        ]
    );
    job.run("chglibl libl(*none) curlib(*crtdft)")
        .await
        .unwrap();
    assert_eq!(job.library_list().len(), 4, "only the system part is left");
}

async fn crtlf(client: &mut Client, lib: &str, name: &str, src: &str) -> rla::OpenFile {
    let lf = dds::parse_lf(name, src).unwrap();
    catalog::create_logical_file(client, &[lib.to_string()], lib, &lf)
        .await
        .unwrap();
    let f = catalog::resolve_file(client, &[lib.to_string()], name)
        .await
        .unwrap();
    rla::OpenFile::new(f)
}

async fn names(client: &Client, f: &mut rla::OpenFile) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let r = f.read(client).await.unwrap();
        if r.eof {
            break;
        }
        out.push(field(&r, "CUSNAM").trim().to_string());
    }
    out
}

/// Logical files (IBM DDS reference): a keyed view of a physical file's
/// records in another order, with only the records select/omit lets
/// through; DESCEND keys read high to low.
#[tokio::test]
async fn logical_files_order_and_select() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    let mut by_name = crtlf(
        &mut client,
        &lib,
        "CUSTNAME",
        include_str!("../../samples/qddssrc/CUSTNAME.lf"),
    )
    .await;
    assert_eq!(
        names(&client, &mut by_name).await,
        [
            "Acme Tooling",
            "Bluebird Supply",
            "Lake, Pine & Co",
            "Midwest Fasteners",
            "Prairie Freight"
        ]
    );
    // SETLL on a partial name, then READ.
    by_name.setll(&client, &["L".into()]).await.unwrap();
    assert_eq!(
        field(&by_name.read(&client).await.unwrap(), "CUSNAM").trim(),
        "Lake, Pine & Co"
    );

    let mut owe = crtlf(
        &mut client,
        &lib,
        "CUSTOWE",
        include_str!("../../samples/qddssrc/CUSTOWE.lf"),
    )
    .await;
    // Balances: 4: 9999999.99, 1: 1250.50, 2: 310.00; 3 (0) and 5 (-75.25)
    // are omitted by COMP(GT 0).
    assert_eq!(
        names(&client, &mut owe).await,
        ["Prairie Freight", "Acme Tooling", "Midwest Fasteners"]
    );
    // Descending key: SETLL 1250.50 positions at the first record whose key
    // is at or after it in access path order, i.e. <= 1250.50.
    owe.setll(&client, &["1250.50".into()]).await.unwrap();
    assert!(owe.equal());
    assert_eq!(
        field(&owe.read(&client).await.unwrap(), "CUSNAM").trim(),
        "Acme Tooling"
    );
    assert_eq!(
        field(&owe.read(&client).await.unwrap(), "CUSNAM").trim(),
        "Midwest Fasteners"
    );
    assert!(owe.read(&client).await.unwrap().eof);
    // And backwards: READP from the end gives the smallest balance first.
    owe.setgt(&client, &[]).await.unwrap();
    assert_eq!(
        field(&owe.read_previous(&client).await.unwrap(), "CUSNAM").trim(),
        "Midwest Fasteners"
    );
    assert_eq!(
        field(&owe.read_previous(&client).await.unwrap(), "CUSNAM").trim(),
        "Acme Tooling"
    );
    // SETGT 1250.50 (descending): after it come the smaller balances.
    owe.setgt(&client, &["1250.50".into()]).await.unwrap();
    assert_eq!(
        field(&owe.read(&client).await.unwrap(), "CUSNAM").trim(),
        "Midwest Fasteners"
    );
    // CHAIN works on the logical file's key.
    let r = owe.chain(&client, &["310".into()]).await.unwrap();
    assert!(r.found);
    assert!(
        !owe.chain(&client, &["0".into()]).await.unwrap().found,
        "omitted record"
    );

    // A record changed through the physical file moves in and out of the
    // logical file: no copy, the same data.
    let mut pf = rla::OpenFile::new(
        catalog::resolve_file(&client, std::slice::from_ref(&lib), "CUSTMAST")
            .await
            .unwrap(),
    );
    let r = pf.chain(&client, &["3".into()]).await.unwrap();
    let mut rec = r.record.unwrap();
    rec.insert("CUSBAL".into(), "500".into());
    pf.update(&client, &rec).await.unwrap();
    owe.setll(&client, &[]).await.unwrap();
    assert_eq!(
        names(&client, &mut owe).await,
        [
            "Prairie Freight",
            "Acme Tooling",
            "Bluebird Supply",
            "Midwest Fasteners"
        ]
    );
}

/// Select/omit rules are tried in order and the first that holds decides;
/// with none holding, the record is omitted after a last select and
/// selected after a last omit (IBM DDS reference). Character constants are
/// quoted; apostrophes and periods in them are data.
#[tokio::test]
async fn logical_file_select_omit_rules_and_unique() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    // Omit Omaha and Lincoln, then select the rest (ALL).
    let mut f = crtlf(
        &mut client,
        &lib,
        "NOTNEB",
        "     A          R CUSTREC                   PFILE(CUSTMAST)\n     A          K CUSNO\n     A          O CUSCTY                    VALUES('Omaha' 'Lincoln')\n     A          S                           ALL\n",
    )
    .await;
    assert_eq!(
        names(&client, &mut f).await,
        ["Acme Tooling", "Midwest Fasteners", "Lake, Pine & Co"]
    );
    // Only an omit rule: everything else is selected.
    let mut f = crtlf(
        &mut client,
        &lib,
        "NOTLOW",
        "     A          R CUSTREC                   PFILE(CUSTMAST)\n     A          K CUSNO\n     A          O CUSBAL                    RANGE(-100 500)\n",
    )
    .await;
    assert_eq!(
        names(&client, &mut f).await,
        ["Acme Tooling", "Prairie Freight"]
    );
    // AND within a rule, and a constant with an apostrophe and a period.
    let mut f = crtlf(
        &mut client,
        &lib,
        "STPAUL",
        "     A          R CUSTREC                   PFILE(CUSTMAST)\n     A          K CUSNO\n     A          S CUSCTY                    COMP(EQ 'St. Paul''s')\n     A            CUSBAL                    COMP(GE 0)\n",
    )
    .await;
    assert!(names(&client, &mut f).await.is_empty());
    let mut pf = rla::OpenFile::new(
        catalog::resolve_file(&client, std::slice::from_ref(&lib), "CUSTMAST")
            .await
            .unwrap(),
    );
    let mut rec = rla::Record::new();
    rec.insert("CUSNO".into(), "9".into());
    rec.insert("CUSNAM".into(), "Saints Supply".into());
    rec.insert("CUSCTY".into(), "St. Paul's".into());
    rec.insert("CUSBAL".into(), "1".into());
    pf.write(&client, &rec).await.unwrap();
    f.setll(&client, &[]).await.unwrap();
    assert_eq!(names(&client, &mut f).await, ["Saints Supply"]);

    // A UNIQUE logical file rejects a second record with the same key
    // among the records it selects; omitted records do not count.
    let mut u = crtlf(
        &mut client,
        &lib,
        "UNAME",
        "     A                                      UNIQUE\n     A          R CUSTREC                   PFILE(CUSTMAST)\n     A          K CUSNAM\n     A          S CUSBAL                    COMP(GT 0)\n",
    )
    .await;
    let mut dup = rla::Record::new();
    dup.insert("CUSNO".into(), "10".into());
    dup.insert("CUSNAM".into(), "Acme Tooling".into());
    dup.insert("CUSBAL".into(), "5".into());
    assert!(
        u.write(&client, &dup).await.is_err(),
        "duplicate key among selected records"
    );
    dup.insert("CUSBAL".into(), "0".into());
    pf.write(&client, &dup).await.unwrap();
    // Writing through a logical file puts the record in the physical file.
    let mut through = rla::Record::new();
    through.insert("CUSNO".into(), "11".into());
    through.insert("CUSNAM".into(), "Zephyr Co".into());
    through.insert("CUSBAL".into(), "7".into());
    u.write(&client, &through).await.unwrap();
    assert!(pf.chain(&client, &["11".into()]).await.unwrap().found);

    // A logical file over a logical file is refused; so are unknown fields.
    let lf = dds::parse_lf(
        "BAD",
        "     A          R CUSTREC                   PFILE(UNAME)\n",
    )
    .unwrap();
    assert!(
        catalog::create_logical_file(&mut client, std::slice::from_ref(&lib), &lib, &lf)
            .await
            .is_err()
    );
    let lf = dds::parse_lf(
        "BAD",
        "     A          R R1                        PFILE(CUSTMAST)\n     A            NOSUCH\n",
    )
    .unwrap();
    assert!(
        catalog::create_logical_file(&mut client, std::slice::from_ref(&lib), &lib, &lf)
            .await
            .is_err()
    );
}

/// Phase 1 gate: a file created from DDS is read with record-level access
/// (the other tests) and with SQL. DB2 for i SQL reference: system naming
/// LIB/FILE, SQL naming LIB.FILE, unqualified names through the library
/// list; the relative record number is not a column unless asked for with
/// RRN(); INSERT without a column list fills the file's fields in order.
#[tokio::test]
async fn sql_reads_and_writes_dds_files() {
    use as4tainr_core::sql::{self, Outcome};
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    crtlf(
        &mut client,
        &lib,
        "CUSTOWE",
        include_str!("../../samples/qddssrc/CUSTOWE.lf"),
    )
    .await;
    let libl = vec![lib.clone()];
    let rows = |o: Outcome| match o {
        Outcome::Rows { columns, rows, .. } => (columns, rows),
        other => panic!("expected rows, got {other:?}"),
    };

    let (cols, r) = rows(
        sql::run(
            &client,
            &url,
            &libl,
            &format!("select * from {lib}/custmast order by cusno"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        cols,
        ["CUSNO", "CUSNAM", "CUSCTY", "CUSBAL"],
        "no RRN column"
    );
    assert_eq!(r.len(), 5);
    assert_eq!(r[0][3], "1250.50", "decimals keep their scale");
    let (_, r) = rows(
        sql::run(
            &client,
            &url,
            &libl,
            &format!("select cusnam from {lib}.custowe where cusbal > 1000 order by cusbal desc"),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        r.iter().map(|x| x[0].trim()).collect::<Vec<_>>(),
        ["Prairie Freight", "Acme Tooling"]
    );
    let (cols, r) = rows(
        sql::run(
            &client,
            &url,
            &libl,
            "select rrn(c), cusno from custmast c where cusno = 3",
        )
        .await
        .unwrap(),
    );
    assert_eq!(cols, ["RRN", "CUSNO"]);
    assert_eq!(r[0][0], "1", "customer 3 was the first record loaded");

    let n = sql::run(
        &client,
        &url,
        &libl,
        "insert into custmast values (8, 'Eighth Co', 'Fargo', 5)",
    )
    .await
    .unwrap();
    assert_eq!(n, Outcome::Count(sql::Kind::Insert, 1));
    // Seen by record-level access too: one database.
    let mut f = rla::OpenFile::new(
        catalog::resolve_file(&client, &libl, "CUSTMAST")
            .await
            .unwrap(),
    );
    assert_eq!(
        field(&f.chain(&client, &["8".into()]).await.unwrap(), "CUSNAM").trim(),
        "Eighth Co"
    );
    let n = sql::run(
        &client,
        &url,
        &libl,
        "update custmast set cusbal = cusbal * 2 where cusno = 8",
    )
    .await
    .unwrap();
    assert_eq!(n, Outcome::Count(sql::Kind::Update, 1));
    assert_eq!(
        field(&f.chain(&client, &["8".into()]).await.unwrap(), "CUSBAL"),
        "10.00"
    );

    // Errors carry DB2-style SQL message IDs; the catalog is out of reach.
    let err = |s: &'static str| {
        let (c, u, l) = (&client, &url, &libl);
        async move { sql::run(c, u, l, s).await.unwrap_err().to_string() }
    };
    assert!(err("select * from nosuch").await.starts_with("SQL0204"));
    assert!(err("insert into custmast values (8, 'Dup', 'X', 0)")
        .await
        .starts_with("SQL0803"));
    assert!(err("select * from \"AS4TAINR\".user_profiles")
        .await
        .starts_with("SQL0551"));
    assert!(err("create table x (a int)").await.starts_with("SQL0084"));
}

async fn cl_job_run(
    client: &Client,
    url: &str,
    lib: &str,
    pgms: &[(&str, &str)],
    call: &str,
) -> anyhow::Result<Vec<String>> {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let mut c = runtime::connect_db(url).await.unwrap();
    for (name, src) in pgms {
        as4tainr_core::clpgm::compile_in(client, &[lib.to_string()], name, src)
            .await
            .unwrap();
        catalog::create_cl_program(&mut c, lib, name, "", src, true)
            .await
            .unwrap();
    }
    let env = Environment {
        database_url: url.to_string(),
    };
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![lib.to_string()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    match job.run(call).await? {
        Outcome::Messages(m) => Ok(m),
        other => panic!("unexpected {other:?}"),
    }
}

/// Message files and program messages (IBM CL reference: CRTMSGF,
/// ADDMSGD, SNDPGMMSG, RCVMSG, RTVMSG, MONMSG). ADDMSGD FMT: *CHAR data
/// without trailing blanks, *DEC with its decimal positions; a second
/// ADDMSGD of the same identifier fails with CPF2412. SNDPGMMSG sends to
/// the caller's queue (TOPGMQ(*PRV)) by default; *SAME stays on the
/// sender's own queue; an *ESCAPE to the caller ends the sender and is an
/// exception the caller's MONMSG handles, then RCVMSG MSGTYPE(*EXCP)
/// receives it with RTNTYPE '15' (escape); a *DIAG has RTNTYPE '02'. With
/// no message, RCVMSG sets the variables to blanks. RMV(*NO) leaves an
/// old message, which *ANY does not receive again but its key does.
#[tokio::test]
async fn cl_message_files_and_program_messages() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let setup_pgm = format!(
        "PGM
  CRTMSGF MSGF({lib}/APPMSG) TEXT('App messages')
  ADDMSGD MSGID(APP0001) MSGF(APPMSG) MSG('Customer &1 owes &2.') SEV(30) +
    FMT((*CHAR 10) (*DEC 7 2))
  ADDMSGD MSGID(APP0002) MSGF(APPMSG) MSG('Order &1 cannot be filled.') +
    SECLVL('Check the stock of order &1.') SEV(40) FMT((*CHAR 5))
  ADDMSGD MSGID(APP0002) MSGF(APPMSG) MSG('again')
  MONMSG CPF2412 EXEC(SNDPGMMSG MSG('duplicate refused'))
ENDPGM"
    );
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("MSGSETUP", &setup_pgm)],
        "CALL MSGSETUP",
    )
    .await
    .unwrap();
    assert_eq!(
        m,
        [
            format!("CPC9801: Object APPMSG of type *MSGF was created in library {lib}."),
            "duplicate refused".to_string()
        ]
    );
    let sender = "PGM
  DCL &DATA *CHAR 14
  DCL &NAME *CHAR 10 STG(*DEFINED) DEFVAR(&DATA 1)
  DCL &AMT *DEC (7 2) STG(*DEFINED) DEFVAR(&DATA 11)
  CHGVAR &NAME 'ACME'
  CHGVAR &AMT 128.5
  SNDPGMMSG MSGID(APP0001) MSGF(APPMSG) MSGDTA(&DATA) MSGTYPE(*DIAG)
  SNDPGMMSG MSG('kept here') TOPGMQ(*SAME)
  SNDPGMMSG MSGID(APP0002) MSGF(APPMSG) MSGDTA('ORD42') MSGTYPE(*ESCAPE)
  SNDPGMMSG MSG('not reached')
ENDPGM";
    let main = "PGM
  DCL &MSG *CHAR 80
  DCL &ID *CHAR 7
  DCL &DTA *CHAR 20
  DCL &F *CHAR 10
  DCL &FL *CHAR 10
  DCL &SEV *DEC (2 0)
  DCL &RT *CHAR 2
  DCL &LEN *DEC (5 0)
  DCL &KEY *CHAR 4
  DCL &SL *CHAR 80
  CALL MSGSEND
  MONMSG APP0000 EXEC(DO)
    RCVMSG MSGTYPE(*EXCP) MSG(&MSG) MSGID(&ID) MSGDTA(&DTA) MSGF(&F) +
      MSGFLIB(&FL) SEV(&SEV) RTNTYPE(&RT) SECLVL(&SL)
    SNDPGMMSG MSG('excp' *BCAT &ID *BCAT &RT *BCAT %CHAR(&SEV) *BCAT &F *BCAT +
      %TRIM(&DTA) *BCAT %TRIM(&MSG) *BCAT %TRIM(&SL))
    IF (&FL *NE ' ') THEN(SNDPGMMSG MSG('library known'))
  ENDDO
  RCVMSG MSGTYPE(*DIAG) MSG(&MSG) MSGLEN(&LEN) SEV(&SEV) RTNTYPE(&RT)
  SNDPGMMSG MSG('diag' *BCAT &RT *BCAT %CHAR(&SEV) *BCAT %CHAR(&LEN) *BCAT %TRIM(&MSG))
  RCVMSG MSGTYPE(*ANY) MSGID(&ID) MSG(&MSG) KEYVAR(&KEY) SEV(&SEV)
  IF ((&KEY *EQ ' ') *AND (&SEV *EQ 0)) THEN(SNDPGMMSG MSG('queue empty'))
  RTVMSG MSGID(APP0002) MSGF(APPMSG) MSGDTA('X1   ') MSG(&MSG) SEV(&SEV)
  SNDPGMMSG MSG(%TRIM(&MSG) *BCAT %CHAR(&SEV))
  SNDPGMMSG MSG('one') TOPGMQ(*SAME) KEYVAR(&KEY)
  RCVMSG MSGTYPE(*ANY) RMV(*NO) MSG(&MSG)
  RCVMSG MSGTYPE(*ANY) MSG(&MSG)
  IF (&MSG *EQ ' ') THEN(SNDPGMMSG MSG('old not received'))
  RCVMSG MSGKEY(&KEY) MSG(&MSG)
  SNDPGMMSG MSG('by key' *BCAT &MSG)
  CHKOBJ OBJ(NOSUCH) OBJTYPE(*FILE)
  MONMSG CPF9801 EXEC(DO)
    RCVMSG MSGTYPE(*EXCP) MSGID(&ID) MSGF(&F)
    SNDPGMMSG MSG('chk' *BCAT &ID *BCAT &F)
  ENDDO
ENDPGM";
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("MSGSEND", sender), ("MSGMAIN", main)],
        "CALL MSGMAIN",
    )
    .await
    .unwrap();
    assert_eq!(
        m,
        [
            "excp APP0002 15 40 APPMSG ORD42 Order ORD42 cannot be filled. Check the stock of order ORD42.",
            "library known",
            "diag 02 30 26 Customer ACME owes 128.50.",
            "queue empty",
            "Order X1 cannot be filled. 40",
            "old not received",
            "by key one",
            "chk CPF9801 QCPFMSG",
        ]
    );
    // An exception received and resent as an escape keeps its text.
    let resend = "PGM
  DCL &ID *CHAR 7
  DCL &D *CHAR 100
  DCL &F *CHAR 10
  DCL &FL *CHAR 10
  CHKOBJ OBJ(NOSUCH) OBJTYPE(*FILE)
  MONMSG CPF0000 EXEC(DO)
    RCVMSG MSGTYPE(*EXCP) MSGID(&ID) MSGDTA(&D) MSGF(&F) MSGFLIB(&FL)
    SNDPGMMSG MSGID(&ID) MSGF(&FL/&F) MSGDTA(&D) MSGTYPE(*ESCAPE)
  ENDDO
ENDPGM";
    let e = cl_job_run(
        &client,
        &url,
        &lib,
        &[("MSGRESND", resend)],
        "CALL MSGRESND",
    )
    .await
    .unwrap_err()
    .to_string();
    // The resent escape message: CPF9801 with as4tainr's text for CHKOBJ
    // (the text is not IBM's; clean room).
    assert!(
        e.starts_with("CPF9801: Object NOSUCH was not found in library *LIBL."),
        "{e}"
    );
    // A message file that does not exist.
    let e = cl_job_run(
        &client,
        &url,
        &lib,
        &[(
            "MSGNOF",
            "PGM\n SNDPGMMSG MSGID(APP0001) MSGF(NOSUCHF) MSGTYPE(*INFO)\nENDPGM",
        )],
        "CALL MSGNOF",
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("CPF2407"), "{e}");
}

/// Files in CL programs (IBM CL reference: DCLF, RCVF, SNDRCVF, CLOSE).
/// DCLF declares a CL variable for each field (with OPNID, named
/// &OPNID_FIELD); RCVF reads the next record in the file's access path
/// (CUSTMAST is keyed by CUSNO); at the end of the file RCVF sends
/// CPF0864 and the variables keep the last record's values; CLOSE lets
/// the next RCVF start again at the first record. A display file's
/// fields and indicators &IN01-&IN99 are variables: SNDRCVF shows a
/// record format with them and takes back what was typed and the
/// response indicators (CA03 sets 03).
#[tokio::test]
async fn cl_files_dclf_rcvf_sndrcvf() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let reader = "PGM
  DCLF FILE(CUSTMAST)
  DCLF FILE(CUSTMAST) OPNID(SECOND)
  DCL &N *DEC (3 0)
  DCL &T *DEC (11 2)
LOOP:
  RCVF
  MONMSG CPF0864 EXEC(GOTO DONE)
  CHGVAR &N (&N + 1)
  CHGVAR &T (&T + &CUSBAL)
  GOTO LOOP
DONE:
  SNDPGMMSG MSG(%CHAR(&N) *BCAT %CHAR(&T) *BCAT %TRIM(&CUSNAM))
  RCVF OPNID(SECOND)
  SNDPGMMSG MSG(%CHAR(&SECOND_CUSNO) *BCAT %TRIM(&SECOND_CUSCTY))
  CLOSE
  RCVF
  SNDPGMMSG MSG('again' *BCAT %CHAR(&CUSNO))
ENDPGM";
    let m = cl_job_run(&client, &url, &lib, &[("CLREAD", reader)], "CALL CLREAD")
        .await
        .unwrap();
    // 0 + 1250.50 - 75.25 + 310.00 + 9999999.99 (samples/data/CUSTMAST.csv).
    assert_eq!(
        m,
        ["5 10001485.24 Lake, Pine & Co", "1 Des Moines", "again 1"]
    );
    // A file that is not there: the program is not created.
    let e = as4tainr_core::clpgm::compile_in(
        &client,
        std::slice::from_ref(&lib),
        "CLNOF",
        "PGM\n DCLF NOSUCHF\nENDPGM",
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("NOSUCHF"), "{e}");

    // A display file.
    let mut c = runtime::connect_db(&url).await.unwrap();
    catalog::create_display_file(
        &mut c,
        &lib,
        "CUSTINQD",
        include_str!("../../samples/qddssrc/CUSTINQD.dspf"),
    )
    .await
    .unwrap();
    let inq = "PGM
  DCLF FILE(CUSTINQD)
  DCLF FILE(CUSTMAST) OPNID(C)
  CHGVAR &IN30 '1'
  SNDRCVF RCDFMT(PROMPT)
  CHGVAR &IN30 '0'
LOOP:
  RCVF OPNID(C)
  IF COND(&C_CUSNO *NE &CUSNUM) THEN(GOTO LOOP)
  CHGVAR &CUSNAM &C_CUSNAM
  CHGVAR &CUSCTY &C_CUSCTY
  CHGVAR &CUSBAL &C_CUSBAL
  SNDRCVF RCDFMT(DETAIL)
  IF COND(&IN03) THEN(SNDPGMMSG MSG('F3 pressed for' *BCAT %TRIM(&CUSNAM)))
ENDPGM";
    catalog::create_cl_program(&mut c, &lib, "CLINQ", "", inq, true)
        .await
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<runtime::DisplayRequest>(1);
    let screens = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(req) = rx.recv().await {
            let top = req.frame.layers.last().unwrap();
            seen.push((
                top.format.clone(),
                top.ind[30],
                top.values.get("CUSNAM").map(|v| v.trim_end().to_string()),
                top.values.get("CUSBAL").cloned(),
            ));
            let mut r = as4tainr_core::dspf::Response {
                aid: "ENTER".into(),
                values: Default::default(),
                indicators: Default::default(),
                changed: Default::default(),
            };
            if top.format == "PROMPT" {
                r.values.insert("CUSNUM".into(), "2".into());
            } else {
                r.aid = "F3".into();
                r.indicators.insert(3, true);
            }
            let _ = req.reply.send(Ok(r));
        }
        seen
    });
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![lib.clone()],
        display: Some(tx),
        id: None,
        calls: Default::default(),
    };
    let out = job.run("CALL CLINQ").await.unwrap();
    drop(job);
    let seen = screens.await.unwrap();
    match out {
        Outcome::Messages(m) => assert_eq!(m, ["F3 pressed for Midwest Fasteners"]),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        seen,
        [
            ("PROMPT".to_string(), true, None, None),
            (
                "DETAIL".to_string(),
                false,
                Some("Midwest Fasteners".to_string()),
                Some("310.00".to_string())
            ),
        ]
    );
}

/// File overrides and small job commands (IBM CL reference: OVRDBF,
/// DLTOVR, CLRPFM, RTVSYSVAL, DLYJOB). OVRDBF TOFILE makes the programs
/// of its call level and those it calls open the other file (an RPG
/// program called after it, a CL RCVF opened after it); DLTOVR removes
/// it; an override ends when the program that made it returns. CLRPFM
/// removes every record. A file of another record format is refused with
/// CPF4131 (level check) unless LVLCHK(*NO). RTVSYSVAL returns the
/// shipped values: QDATFMT MDY, QCCSID 65535; QDAYOFWEEK is *xxx.
#[tokio::test]
async fn cl_overrides_clrpfm_rtvsysval() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let (_, lib2) = setup(&url).await;
    let mut c = runtime::connect_db(&url).await.unwrap();
    let counter = "**FREE
dcl-f CUSTMAST keyed;
dcl-s n packed(5:0);
read CUSTMAST;
dow not %eof(CUSTMAST);
  n += 1;
  read CUSTMAST;
enddo;
dsply ('count ' + %char(n));
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "CNTCUST", "RPGLE", "", counter, true)
        .await
        .unwrap();
    let inner = format!(
        "PGM
  OVRDBF FILE(CUSTMAST) TOFILE({lib2}/CUSTMAST)
  CALL CNTCUST
ENDPGM"
    );
    let main = format!(
        "PGM
  DCLF FILE(CUSTMAST)
  DCL &FMT *CHAR 3
  DCL &CC *DEC (5 0)
  DCL &DOW *CHAR 4
  CLRPFM FILE({lib2}/CUSTMAST)
  CALL OVRINNER
  CALL CNTCUST
  OVRDBF FILE(CUSTMAST) TOFILE({lib2}/CUSTMAST)
  RCVF
  MONMSG CPF0864 EXEC(SNDPGMMSG MSG('overridden file is empty'))
  CALL CNTCUST
  DLTOVR FILE(CUSTMAST)
  CLOSE
  RCVF
  SNDPGMMSG MSG('back to' *BCAT %CHAR(&CUSNO))
  CALL CNTCUST
  OVRDBF FILE(CUSTMAST) TOFILE({lib}/ORDLIN)
  CLOSE
  RCVF
  MONMSG CPF4131 EXEC(SNDPGMMSG MSG('level check'))
  RTVSYSVAL QDATFMT &FMT
  RTVSYSVAL SYSVAL(QCCSID) RTNVAR(&CC)
  RTVSYSVAL QDAYOFWEEK &DOW
  SNDPGMMSG MSG(&FMT *BCAT %CHAR(&CC) *BCAT %SST(&DOW 1 1))
  RTVSYSVAL QNOSUCH &FMT
  MONMSG CPF1028 EXEC(SNDPGMMSG MSG('no such system value'))
  DLYJOB DLY(1)
ENDPGM"
    );
    let started = std::time::Instant::now();
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("OVRINNER", &inner), ("OVRMAIN", &main)],
        "CALL OVRMAIN",
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert_eq!(
        m,
        [
            format!("CPC3101: Member CUSTMAST of file CUSTMAST in library {lib2} was cleared."),
            "DSPLY  count 0".to_string(),
            "DSPLY  count 5".to_string(),
            "overridden file is empty".to_string(),
            "DSPLY  count 0".to_string(),
            "back to 1".to_string(),
            "DSPLY  count 5".to_string(),
            "level check".to_string(),
            "MDY 65535 *".to_string(),
            "no such system value".to_string(),
        ]
    );
}

/// A customer's city, as another job sees it.
async fn cusnos_city(client: &Client, lib: &str, cusno: i32) -> String {
    client
        .query_one(
            &format!(
                "SELECT trim(cuscty) FROM {}.custmast WHERE cusno = $1::int",
                catalog::ident(lib)
            ),
            &[&cusno],
        )
        .await
        .unwrap()
        .get(0)
}

/// The customer numbers above 6, as another job sees them.
async fn cusnos_above_6(client: &Client, lib: &str) -> Vec<String> {
    client
        .query(
            &format!(
                "SELECT cusno::text FROM {}.custmast c WHERE c.cusno > 6 ORDER BY c.cusno",
                catalog::ident(lib)
            ),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect()
}

/// Commitment control (IBM CL reference STRCMTCTL, ENDCMTCTL, COMMIT,
/// ROLLBACK; ILE RPG Reference COMMIT and ROLBK operations and the COMMIT
/// file keyword; Db2 for i SQL Reference COMMIT, ROLLBACK and SET OPTION
/// COMMIT; "Ending commitment control"). Records an RPG program writes to
/// a file opened under commitment control are pending until COMMIT makes
/// them permanent or ROLBK removes them; other jobs do not see pending
/// records. ENDCMTCTL rolls back what is pending (CPF8356 in a batch
/// job). A file with COMMIT cannot be opened without a commitment
/// definition (CPF4326); COMMIT with none is CPF8350 in CL and status
/// 00802 in RPG; a second STRCMTCTL for the same scope is CPF8351. SQL
/// with a commit level other than *NONE starts commitment control itself;
/// the job ending rolls back its pending changes.
#[tokio::test]
async fn commitment_control() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::jobs;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut c = runtime::connect_db(&url).await.unwrap();
    let rpg = "**FREE
dcl-f CUSTMAST usage(*update:*output) keyed commit;
cusno = 7;
cusnam = 'Rolled back';
cuscty = 'X';
cusbal = 0;
write custrec;
rolbk;
cusno = 8;
cusnam = 'Committed';
write custrec;
commit;
cusno = 9;
cusnam = 'Pending';
write custrec;
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "CMTRPG", "RPGLE", "", rpg, true)
        .await
        .unwrap();
    let nocmt = "**FREE
monitor;
  commit;
on-error;
  dsply %char(%status);
endmon;
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "CMTNONE", "RPGLE", "", nocmt, true)
        .await
        .unwrap();
    let sqlpgm = "**FREE
exec sql set option commit = *chg;
exec sql insert into custmast (cusno, cusnam, cuscty, cusbal) values (20, 'Undone', 'Y', 0);
exec sql rollback;
exec sql insert into custmast (cusno, cusnam, cuscty, cusbal) values (21, 'Kept', 'Y', 0);
exec sql commit;
exec sql insert into custmast (cusno, cusnam, cuscty, cusbal) values (22, 'Left pending', 'Y', 0);
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "CMTSQL", "SQLRPGLE", "", sqlpgm, true)
        .await
        .unwrap();
    let cl = "PGM
  COMMIT
  MONMSG CPF8350 EXEC(SNDPGMMSG MSG('no commitment definition'))
  CALL CMTNONE
  STRCMTCTL LCKLVL(*CHG) CMTSCOPE(*JOB)
  STRCMTCTL LCKLVL(*CHG) CMTSCOPE(*JOB)
  MONMSG CPF8351 EXEC(SNDPGMMSG MSG('already started'))
  CALL CMTRPG
ENDPGM";
    catalog::create_cl_program(&mut c, &lib, "CMTCL", "", cl, true)
        .await
        .unwrap();
    let env = Environment {
        database_url: url.clone(),
    };
    let id = jobs::start_interactive(
        &client,
        &format!("C{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![lib.clone()],
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };
    let msgs = |o: Outcome| match o {
        Outcome::Messages(m) => m,
        other => panic!("unexpected {other:?}"),
    };
    let m = msgs(job.run("CALL CMTCL").await.unwrap());
    assert_eq!(
        m,
        ["no commitment definition", "DSPLY  802", "already started"]
    );
    // Another job sees the committed record only.
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8"]);
    let m = msgs(job.run("ENDCMTCTL").await.unwrap());
    assert!(m[0].starts_with("CPF8356"), "{m:?}");
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8"]);
    // No definition now: the file cannot be opened under commitment
    // control.
    let e = job.run("CALL CMTRPG").await.unwrap_err().to_string();
    assert!(e.contains("CPF4326"), "{e}");
    let e = job.run("ROLLBACK").await.unwrap_err().to_string();
    assert!(e.starts_with("CPF8350"), "{e}");

    // SQL starts commitment control by itself.
    job.run("CALL CMTSQL").await.unwrap();
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8", "21"]);
    // The pending insert is there for the job: a COMMIT in CL commits it.
    job.run("COMMIT").await.unwrap();
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8", "21", "22"]);
    // A change left pending when the job ends is rolled back: the record
    // is not there, and its key is free at once (no lock held).
    let pend = "**FREE
exec sql set option commit = *chg;
exec sql insert into custmast (cusno, cusnam, cuscty, cusbal) values (23, 'Never', 'Y', 0);
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "CMTPEND", "SQLRPGLE", "", pend, true)
        .await
        .unwrap();
    job.run("CALL CMTPEND").await.unwrap();
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8", "21", "22"]);
    jobs::end(&client, &id, true).await.unwrap();
    assert_eq!(cusnos_above_6(&client, &lib).await, ["8", "21", "22"]);
    client
        .execute(
            &format!(
                "INSERT INTO {}.custmast (cusno, cusnam, cuscty, cusbal) VALUES (23, 'Free', 'Z', 0)",
                catalog::ident(&lib)
            ),
            &[],
        )
        .await
        .unwrap();
}

/// SQL procedures and functions (Db2 for i SQL Reference: CREATE
/// PROCEDURE (SQL), CREATE FUNCTION (SQL scalar), CALL, compound
/// statements, DECLARE HANDLER, SIGNAL, GET DIAGNOSTICS, FOR, LOOP,
/// LEAVE, ITERATE, cursors). A CONTINUE handler for NOT FOUND lets a
/// FETCH loop end and a SELECT INTO with no row go on; an EXIT handler
/// ends the compound statement that declared it; SIGNAL raises a
/// user-defined SQLSTATE (SQLCODE -438), which a handler for the
/// condition catches; GET DIAGNOSTICS returns MESSAGE_TEXT. OUT
/// parameters come back to the caller (host variables in embedded SQL).
/// RUNSQLSTM keeps a routine's body together although it has semicolons.
/// Expected values from samples/data/CUSTMAST.csv.
#[tokio::test]
async fn sql_procedures_and_functions() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::sql::Outcome as SqlOut;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "CREATE OR REPLACE PROCEDURE {lib}.ADJUST (IN P_CUSNO DECIMAL(5, 0), IN P_AMOUNT DECIMAL(9, 2),
    OUT P_NEWBAL DECIMAL(9, 2), OUT P_STATUS CHAR(10))
  LANGUAGE SQL
  MODIFIES SQL DATA
BEGIN
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
    SET P_STATUS = 'FAILED';
  DECLARE CONTINUE HANDLER FOR NOT FOUND
    SET P_STATUS = 'NOTFOUND';
  SET P_STATUS = 'OK';
  SELECT CUSBAL INTO P_NEWBAL FROM CUSTMAST WHERE CUSNO = P_CUSNO;
  IF P_STATUS = 'OK' THEN
    UPDATE CUSTMAST SET CUSBAL = CUSBAL + P_AMOUNT WHERE CUSNO = P_CUSNO;
    SET P_NEWBAL = P_NEWBAL + P_AMOUNT;
  END IF;
END;

CREATE PROCEDURE {lib}.STATS (OUT P_TOTAL DECIMAL(11, 2), OUT P_NAMES VARCHAR(200),
    OUT P_EVENS INTEGER)
BEGIN
  DECLARE V_BAL DECIMAL(9, 2);
  DECLARE V_NAME CHAR(30);
  DECLARE V_EOF INTEGER DEFAULT 0;
  DECLARE I INTEGER DEFAULT 0;
  DECLARE C1 CURSOR FOR SELECT CUSBAL, CUSNAM FROM CUSTMAST ORDER BY CUSNO;
  DECLARE CONTINUE HANDLER FOR NOT FOUND SET V_EOF = 1;
  SET P_TOTAL = 0;
  OPEN C1;
  FETCH C1 INTO V_BAL, V_NAME;
  WHILE V_EOF = 0 DO
    SET P_TOTAL = P_TOTAL + V_BAL;
    FETCH C1 INTO V_BAL, V_NAME;
  END WHILE;
  CLOSE C1;
  SET P_NAMES = '';
  FOR R AS SELECT CUSNO, CUSCTY FROM CUSTMAST WHERE CUSNO <= 3 ORDER BY CUSNO DO
    SET P_NAMES = P_NAMES CONCAT TRIM(R.CUSCTY) CONCAT ';';
  END FOR;
  SET P_EVENS = 0;
  L1: LOOP
    SET I = I + 1;
    IF I > 10 THEN LEAVE L1; END IF;
    IF MOD(I, 2) = 1 THEN ITERATE L1; END IF;
    SET P_EVENS = P_EVENS + 1;
  END LOOP L1;
END;

CREATE PROCEDURE {lib}.CHECKAMT (IN P_AMT DECIMAL(9, 2))
BEGIN
  IF P_AMT < 0 THEN
    SIGNAL SQLSTATE '75001' SET MESSAGE_TEXT = 'Amount must not be negative';
  END IF;
END;

CREATE PROCEDURE {lib}.TRYAMT (IN P_AMT DECIMAL(9, 2), OUT P_MSG VARCHAR(100))
BEGIN
  DECLARE BAD CONDITION FOR SQLSTATE '75001';
  DECLARE EXIT HANDLER FOR BAD
  BEGIN
    GET DIAGNOSTICS EXCEPTION 1 P_MSG = MESSAGE_TEXT;
  END;
  SET P_MSG = 'accepted';
  CALL CHECKAMT(P_AMT);
END;

CREATE FUNCTION {lib}.DOUBLED (P_AMT DECIMAL(9, 2)) RETURNS DECIMAL(11, 2)
  LANGUAGE SQL DETERMINISTIC
  RETURN P_AMT * 2;

CREATE FUNCTION {lib}.GRADE (P_BAL DECIMAL(9, 2)) RETURNS CHAR(4)
BEGIN
  DECLARE G CHAR(4);
  IF P_BAL > 1000 THEN SET G = 'HIGH';
  ELSEIF P_BAL > 0 THEN SET G = 'LOW';
  ELSE SET G = 'NONE';
  END IF;
  RETURN G;
END;
"
    );
    let lines = job
        .run_sql_script(&script, "test script", 0, None, true)
        .await
        .unwrap();
    assert_eq!(lines.len(), 7, "{lines:?}");
    assert!(lines[6].contains("0 with errors"), "{lines:?}");

    let run = |s: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            let libl = vec![lib.clone()];
            as4tainr_core::sql::run_as(client, &url, &libl, "QSECOFR", Some(lib), None, &s).await
        }
    };
    let msg = |o: SqlOut| match o {
        SqlOut::Message(m) => m,
        other => panic!("unexpected {other:?}"),
    };
    // 310.00 + 10.50 (customer 2).
    assert_eq!(
        msg(run("CALL ADJUST(2, 10.50, ?, ?)".into()).await.unwrap()),
        "Procedure ADJUST ran: P_NEWBAL = 320.50, P_STATUS = OK        ."
    );
    assert_eq!(
        msg(run("CALL ADJUST(99, 1, ?, ?)".into()).await.unwrap()),
        "Procedure ADJUST ran: P_NEWBAL = NULL, P_STATUS = NOTFOUND  ."
    );
    // 0 + 1250.50 + 320.50 - 75.25 + 9999999.99 (after the adjustment).
    assert_eq!(
        msg(run("CALL STATS(?, ?, ?)".into()).await.unwrap()),
        "Procedure STATS ran: P_TOTAL = 10001495.74, P_NAMES = Des Moines;Chicago;Omaha;, P_EVENS = 5."
    );
    assert_eq!(
        msg(run("CALL TRYAMT(-1, ?)".into()).await.unwrap()),
        "Procedure TRYAMT ran: P_MSG = Amount must not be negative."
    );
    assert_eq!(
        msg(run("CALL TRYAMT(5, ?)".into()).await.unwrap()),
        "Procedure TRYAMT ran: P_MSG = accepted."
    );
    let e = run("CALL CHECKAMT(-1)".into())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e, "SQL0438: Amount must not be negative");
    // Functions, in a query.
    match run(format!(
        "SELECT CUSNO, DOUBLED(CUSBAL), GRADE(CUSBAL) FROM {lib}.CUSTMAST ORDER BY CUSNO"
    ))
    .await
    .unwrap()
    {
        SqlOut::Rows { rows, .. } => {
            let got: Vec<String> = rows.iter().map(|r| r.join(" ")).collect();
            assert_eq!(
                got,
                [
                    "1 2501.00 HIGH",
                    // CHAR(4): blank padded.
                    "2 641.00 LOW ",
                    "3 0.00 NONE",
                    "4 19999999.98 HIGH",
                    "5 -150.50 NONE"
                ]
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    // CALL from embedded SQL: OUT parameters into host variables; a
    // signaled SQLSTATE in the SQLCA.
    let rpg = "**FREE
dcl-s newbal packed(9:2);
dcl-s status char(10);
exec sql set option commit = *none;
exec sql call ADJUST(3, 5, :newbal, :status);
dsply (%trim(status) + ' ' + %char(newbal));
exec sql call CHECKAMT(-2);
dsply sqlstate;
dsply %char(sqlcode);
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "CALLSP", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["OK 5.00", "75001", "-438"]);

    // A compound statement by itself (a dynamic compound statement): its
    // handler takes the SQLSTATE 42704 of the DROP of a table that is not
    // there, and the script goes on.
    let lines = job
        .run_sql_script(
            &format!(
                "BEGIN
  DECLARE CONTINUE HANDLER FOR SQLSTATE '42704' BEGIN END;
  DROP TABLE {lib}.NOSUCHTBL;
  UPDATE {lib}.CUSTMAST SET CUSCTY = 'Lincoln NE' WHERE CUSNO = 4;
END;
UPDATE {lib}.CUSTMAST SET CUSBAL = CUSBAL WHERE CUSNO = 4;"
            ),
            "compound script",
            0,
            None,
            true,
        )
        .await
        .unwrap();
    assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    assert_eq!(cusnos_city(&client, &lib, 4).await, "Lincoln NE");

    // A result set: a cursor declared WITH RETURN left open.
    job.run(&format!(
        "RUNSQL SQL('CREATE PROCEDURE {lib}.OWING () DYNAMIC RESULT SETS 1 BEGIN DECLARE C CURSOR WITH RETURN FOR SELECT CUSNO, CUSBAL FROM CUSTMAST WHERE CUSBAL > 1000 ORDER BY CUSNO; OPEN C; END')"
    ))
    .await
    .unwrap();
    match run("CALL OWING()".into()).await.unwrap() {
        SqlOut::Rows { rows, .. } => {
            assert_eq!(rows, [["1", "1250.50"], ["4", "9999999.99"]]);
        }
        other => panic!("unexpected {other:?}"),
    }

    // DROP: the procedure is gone (SQL0440).
    job.run("RUNSQL SQL('DROP PROCEDURE CHECKAMT')")
        .await
        .unwrap();
    let e = run("CALL CHECKAMT(1)".into())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("SQL0440"), "{e}");
    job.run("RUNSQL SQL('DROP FUNCTION GRADE')").await.unwrap();
    let e = run(format!("SELECT GRADE(CUSBAL) FROM {lib}.CUSTMAST"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("SQL"), "{e}");
}

/// Parameters are passed by reference (IBM CL reference, CALL: "the
/// called program can change the value of a CL variable passed as a
/// parameter"; ILE RPG Reference, parameters passed by reference): the
/// caller's variable gets back what the called program left in it,
/// between CL programs, from CL to RPG and from RPG to CL. A called
/// program whose character parameter is shorter than the variable passed
/// changes only its first characters (it shares the variable's storage).
#[tokio::test]
async fn parameters_by_reference() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let mut c = runtime::connect_db(&url).await.unwrap();
    let rpgset = "**FREE
dcl-pi *n;
  s char(20);
  n packed(5:0);
end-pi;
s = 'From RPG';
n = n * 2;
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "RPGSET", "RPGLE", "", rpgset, true)
        .await
        .unwrap();
    let rpgcall = "**FREE
dcl-pr clset2 extpgm('CLSET2');
  s char(10);
  x packed(5:0);
end-pr;
dcl-s v char(10) inz('start');
dcl-s k packed(5:0) inz(3);
clset2(v: k);
dsply (%trim(v) + ' ' + %char(k));
*inlr = *on;
";
    catalog::create_program(&mut c, &lib, "RPGCALLCL", "RPGLE", "", rpgcall, true)
        .await
        .unwrap();
    let clset = "PGM PARM(&S &X &F)
  DCL &S *CHAR 5
  DCL &X *DEC (5 0)
  DCL &F *LGL
  CHGVAR &S 'HELLO'
  CHGVAR &X (&X + 41)
  CHGVAR &F '1'
ENDPGM";
    let clset2 = "PGM PARM(&S &X)
  DCL &S *CHAR 10
  DCL &X *DEC (5 0)
  CHGVAR &S 'done'
  CHGVAR &X (&X * 10)
ENDPGM";
    let caller = "PGM
  DCL &NAME *CHAR 20 VALUE('abcdefghijklmnopqrst')
  DCL &N *DEC (5 0) VALUE(1)
  DCL &FLAG *LGL
  CALL CLSET PARM(&NAME &N &FLAG)
  SNDPGMMSG MSG(&NAME *CAT '|' *CAT %CHAR(&N) *CAT '|' *CAT &FLAG)
  CALL RPGSET PARM(&NAME &N)
  SNDPGMMSG MSG(&NAME *TCAT '|' *CAT %CHAR(&N))
  CALL RPGCALLCL
ENDPGM";
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("CLSET", clset), ("CLSET2", clset2), ("REFCALLER", caller)],
        "CALL REFCALLER",
    )
    .await
    .unwrap();
    // %CHAR of a *DEC (5 0) is padded with blanks to six characters
    // (corrected: PUB400 2026-10-29, tools/golden/verify24.txt q24_rows
    // shows *DEC (5 0) 273 as "273   " inside *CAT; this expected "42"
    // before that capture).
    assert_eq!(
        m,
        [
            "HELLOfghijklmnopqrst|42    |1",
            "From RPG|84",
            "DSPLY  done 30"
        ]
    );
    // An external procedure (Db2 for i SQL Reference, CREATE PROCEDURE
    // (external), PARAMETER STYLE GENERAL): CALL runs the program; INOUT
    // parameters get what it left in them.
    let libl = vec![lib.clone()];
    as4tainr_core::ddl::try_run(
        &url,
        "QSECOFR",
        &libl,
        Some(lib.clone()),
        &mut None,
        &format!(
            "CREATE PROCEDURE {lib}.SETPROC (INOUT S CHAR(20), INOUT N DECIMAL(5, 0)) \
             LANGUAGE RPGLE EXTERNAL NAME {lib}/RPGSET PARAMETER STYLE GENERAL"
        ),
    )
    .await
    .unwrap()
    .unwrap();
    match as4tainr_core::sql::run_as(
        &client,
        &url,
        &libl,
        "QSECOFR",
        Some(lib.clone()),
        None,
        "CALL SETPROC('x', 5)",
    )
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Message(m) => {
            assert_eq!(
                m,
                "Procedure SETPROC ran: S = From RPG            , N = 10."
            )
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// RCVMSG from the caller's program queue (PGMQ(*PRV)) and from a
/// message queue (MSGQ, WAIT); RTVOBJD and RTVMBRD (IBM CL reference).
/// RTVOBJD CRTDATE is CYYMMDDHHMMSS (13 characters, C 1 for 20xx);
/// RTVMBRD NBRCURRCD is the member's number of records (5 in
/// samples/data/CUSTMAST.csv); an object that is not there is CPF9801;
/// with no message and WAIT(1), RCVMSG returns blanks after waiting.
#[tokio::test]
async fn rcvmsg_queues_rtvobjd_rtvmbrd() {
    use as4tainr_core::users;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let user = format!("Q{}", &lib[1..]);
    let _ = users::delete(&client, &user).await;
    let callee = "PGM
  DCL &M *CHAR 40
  RCVMSG PGMQ(*PRV) MSG(&M)
  SNDPGMMSG MSG('callee got' *BCAT &M)
ENDPGM";
    let main = format!(
        "PGM
  DCL &M *CHAR 40
  DCL &L *CHAR 10
  DCL &A *CHAR 10
  DCL &O *CHAR 10
  DCL &D *CHAR 13
  DCL &N *DEC (10 0)
  DCL &FA *CHAR 3
  DCL &FT *CHAR 5
  DCL &MB *CHAR 10
  SNDPGMMSG MSG('kept for the callee') TOPGMQ(*SAME)
  CALL RCVCALLEE
  CRTUSRPRF {user} PASSWORD(*NONE)
  SNDMSG MSG('hello queue') TOUSR({user})
  RCVMSG MSGQ({user}) MSG(&M)
  SNDPGMMSG MSG('queue gave' *BCAT &M)
  RCVMSG MSGQ({user}) MSG(&M) WAIT(1)
  IF COND(&M *EQ ' ') THEN(SNDPGMMSG MSG('queue empty after waiting'))
  RTVOBJD OBJ(CUSTMAST) OBJTYPE(*FILE) RTNLIB(&L) OBJATR(&A) OWNER(&O) CRTDATE(&D)
  SNDPGMMSG MSG(&L *BCAT &A *BCAT &O *BCAT %SST(&D 1 1) *BCAT %CHAR(%LEN(%TRIM(&D))))
  RTVMBRD FILE(CUSTMAST) RTNMBR(&MB) FILEATR(&FA) FILETYPE(&FT) NBRCURRCD(&N)
  SNDPGMMSG MSG(&MB *BCAT &FA *BCAT &FT *BCAT %CHAR(&N))
  RTVOBJD OBJ(NOSUCH) OBJTYPE(*PGM) RTNLIB(&L)
  MONMSG CPF9801 EXEC(SNDPGMMSG MSG('no such object'))
ENDPGM"
    );
    let started = std::time::Instant::now();
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("RCVCALLEE", callee), ("RCVMAIN", &main)],
        "CALL RCVMAIN",
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    // The completion messages of CRTUSRPRF and SNDMSG are not looked at here.
    let m: Vec<&String> = m
        .iter()
        .filter(|l| !l.starts_with("User profile") && !l.starts_with("Message sent"))
        .collect();
    assert_eq!(
        m,
        [
            "callee got kept for the callee",
            "queue gave hello queue",
            "queue empty after waiting",
            &format!("{lib} PF QSECOFR 1 13"),
            "CUSTMAST PF *DATA 5",
            "no such object",
        ]
    );
    users::delete(&client, &user).await.unwrap();

    // Subroutines (IBM CL reference: SUBR, ENDSUBR, CALLSUBR, RTNSUBR):
    // after the program's statements; CALLSUBR RTNVAL gets the value of
    // RTNSUBR or ENDSUBR RTNVAL (0 when none is given).
    let subr = "PGM
  DCL &N *DEC (5 0) VALUE(0)
  DCL &RC *INT 4
  DCL &RC2 *INT 4 VALUE(7)
  CALLSUBR SUBR(ADD5)
  CALLSUBR SUBR(ADD5) RTNVAL(&RC2)
  CALLSUBR SUBR(CHECK) RTNVAL(&RC)
  SNDPGMMSG MSG(%CHAR(&N) *BCAT %CHAR(&RC) *BCAT %CHAR(&RC2))
  CHGVAR &N 0
  CALLSUBR SUBR(CHECK) RTNVAL(&RC)
  SNDPGMMSG MSG(%CHAR(&RC))
  RETURN
SUBR SUBR(ADD5)
  CHGVAR &N (&N + 5)
ENDSUBR
SUBR SUBR(CHECK)
  IF COND(&N *GT 5) THEN(RTNSUBR RTNVAL(1))
ENDSUBR RTNVAL(-1)
ENDPGM";
    let m = cl_job_run(&client, &url, &lib, &[("CLSUBR", subr)], "CALL CLSUBR")
        .await
        .unwrap();
    assert_eq!(m, ["10 1 0", "-1"]);
    assert!(as4tainr_core::clpgm::compile("X", "PGM\n CALLSUBR SUBR(NOPE)\nENDPGM").is_err());
}

/// More SQL data definition (Db2 for i SQL Reference, CREATE TABLE):
/// DECFLOAT(16)/(34) keeps decimal values exactly; BOOLEAN; XML;
/// TIMESTAMP(0) has no fractional seconds; CREATE TABLE ... AS (query)
/// WITH DATA copies the query's rows, WITH NO DATA makes the table
/// empty, with the query's columns. A program's record leaves out
/// DECFLOAT, BOOLEAN and XML columns, as it does large objects. A program
/// created with BNDDIR('QC2LE') (the system's C runtime binding
/// directory) binds and calls Qp0zLprintf.
#[tokio::test]
async fn sql_types_create_table_as_and_qc2le() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "CREATE TABLE {lib}.TYPES (K INTEGER NOT NULL, D16 DECFLOAT(16), D34 DECFLOAT, \
           B BOOLEAN, X XML, T0 TIMESTAMP(0), C CHAR(5));
INSERT INTO {lib}.TYPES VALUES (1, 0.1, 12345678901234567890.123456789, TRUE, '<a/>', '2026-01-02 03:04:05', 'ab');
CREATE TABLE {lib}.OWING AS (SELECT CUSNO, CUSNAM, CUSBAL FROM {lib}.CUSTMAST WHERE CUSBAL > 0) WITH DATA;
CREATE TABLE {lib}.EMPTYCOPY (NO, NAME) AS (SELECT CUSNO, CUSNAM FROM {lib}.CUSTMAST) WITH NO DATA;"
    );
    let lines = job
        .run_sql_script(&script, "types script", 0, None, true)
        .await
        .unwrap();
    assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap()
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => {
                    rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>()
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows(format!("SELECT D16, D34, B, X, T0 FROM {lib}.TYPES")).await,
        // BOOLEAN shows as TRUE; TIMESTAMP(0) without fractional seconds.
        ["0.1|12345678901234567890.123456789|TRUE|<a/>|2026-01-02-03.04.05"]
    );
    // Balances above zero: customers 1, 2 and 4 (samples/data/CUSTMAST.csv).
    assert_eq!(
        rows(format!("SELECT CUSNO FROM {lib}.OWING ORDER BY CUSNO")).await,
        ["1", "2", "4"]
    );
    assert!(rows(format!("SELECT NO, NAME FROM {lib}.EMPTYCOPY"))
        .await
        .is_empty());
    // The program's record has K and C only.
    let rpg = "**FREE
ctl-opt dftactgrp(*no) bnddir('QC2LE');
dcl-pr printf int(10) extproc('Qp0zLprintf');
  fmt pointer value options(*string);
end-pr;
dcl-f TYPES;
read TYPES;
dsply (%char(K) + ' ' + C);
printf('printed' + x'25');
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "TYPESRPG", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    // The job log line has no line end (as in the SYSAPI test).
    assert_eq!(r.messages, ["1 ab", "printed"]);
}

/// IBM i SQL services (Db2 for i SQL Reference, catalog views; IBM i
/// services, OBJECT_STATISTICS) and table functions (CREATE FUNCTION (SQL
/// table), table-reference TABLE(function)). Code values from those pages:
/// SYSTABLES TABLE_TYPE T table, P physical file, V view, L logical file,
/// FILE_TYPE D data; SYSCOLUMNS IS_NULLABLE Y/N, HAS_DEFAULT Y/N, DECIMAL
/// STORAGE (precision/2)+1; SYSINDEXES IS_UNIQUE D duplicates allowed;
/// SYSKEYS ORDERING A/D.
#[tokio::test]
async fn sql_catalog_views_and_table_functions() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "CREATE TABLE {lib}.ITEMS (ID INTEGER NOT NULL, NAME VARCHAR(20), PRICE DECIMAL(7, 2) NOT NULL DEFAULT 0);
CREATE INDEX {lib}.ITEMSIX ON {lib}.ITEMS (PRICE DESC, ID);
CREATE VIEW {lib}.CHEAP AS SELECT ID, NAME FROM {lib}.ITEMS WHERE PRICE < 10;
CREATE FUNCTION {lib}.OWES (MINBAL DECIMAL(9, 2)) RETURNS TABLE (NO DECIMAL(5, 0), NAME CHAR(30))
  LANGUAGE SQL READS SQL DATA RETURN SELECT CUSNO, CUSNAM FROM {lib}.CUSTMAST WHERE CUSBAL > MINBAL;
CREATE TABLE {lib}.OBJS AS (SELECT OBJNAME, OBJTYPE, CURRENT TIMESTAMP AS TS FROM TABLE (QSYS2.OBJECT_STATISTICS(
  OBJECT_SCHEMA => '{lib}', OBJTYPELIST => '*ALL')) AS S) WITH DATA;"
    );
    let lines = job
        .run_sql_script(&script, "services script", 0, None, true)
        .await
        .unwrap();
    assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap()
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => rows
                    .into_iter()
                    .map(|r| r.iter().map(|v| v.trim()).collect::<Vec<_>>().join("|"))
                    .collect::<Vec<_>>(),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows(format!(
            "SELECT TABLE_NAME, TABLE_TYPE, FILE_TYPE, COLUMN_COUNT FROM QSYS2.SYSTABLES
              WHERE TABLE_SCHEMA = '{lib}' AND TABLE_NAME IN ('CHEAP', 'CUSTMAST', 'ITEMS', 'ITEMSIX')
              ORDER BY TABLE_NAME"
        ))
        .await,
        ["CHEAP|V|D|2", "CUSTMAST|P|D|4", "ITEMS|T|D|3"]
    );
    assert_eq!(
        rows(format!(
            "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, HAS_DEFAULT FROM QSYS2.SYSCOLUMNS
              WHERE TABLE_SCHEMA = '{lib}' AND TABLE_NAME = 'ITEMS' ORDER BY ORDINAL_POSITION"
        ))
        .await,
        ["ID|INTEGER|N|N", "NAME|VARCHAR|Y|Y", "PRICE|DECIMAL|N|Y"]
    );
    assert_eq!(
        rows(format!(
            "SELECT LENGTH, NUMERIC_SCALE, STORAGE FROM QSYS2.SYSCOLUMNS
              WHERE TABLE_SCHEMA = '{lib}' AND TABLE_NAME = 'ITEMS' AND COLUMN_NAME = 'PRICE'"
        ))
        .await,
        ["7|2|4"]
    );
    assert_eq!(
        rows(format!(
            "SELECT I.INDEX_NAME, I.TABLE_NAME, I.IS_UNIQUE, I.COLUMN_COUNT, K.COLUMN_NAME, K.ORDERING
               FROM QSYS2.SYSINDEXES I JOIN QSYS2.SYSKEYS K
                 ON K.INDEX_SCHEMA = I.INDEX_SCHEMA AND K.INDEX_NAME = I.INDEX_NAME
              WHERE I.INDEX_SCHEMA = '{lib}' ORDER BY K.ORDINAL_POSITION"
        ))
        .await,
        ["ITEMSIX|ITEMS|D|2|PRICE|D", "ITEMSIX|ITEMS|D|2|ID|A"]
    );
    assert_eq!(
        rows(format!(
            "SELECT TABLE_NAME FROM QSYS2.SYSVIEWS WHERE TABLE_SCHEMA = '{lib}'"
        ))
        .await,
        ["CHEAP"]
    );
    assert_eq!(
        rows(format!(
            "SELECT SCHEMA_NAME FROM QSYS2.SYSSCHEMAS WHERE SCHEMA_NAME = '{lib}'"
        ))
        .await,
        std::slice::from_ref(&lib)
    );
    // OBJECT_STATISTICS: by library and generic name, with named
    // parameters, and through the library list.
    assert_eq!(
        rows(format!(
            "SELECT OBJNAME, OBJTYPE, OBJATTRIBUTE, SQL_OBJECT_TYPE
               FROM TABLE(QSYS2.OBJECT_STATISTICS('{lib}', '*FILE', 'ITEM*')) AS X ORDER BY OBJNAME"
        ))
        .await,
        ["ITEMS|*FILE|PF|TABLE", "ITEMSIX|*FILE|LF|INDEX"]
    );
    assert_eq!(
        rows(
            "SELECT OBJLIB, OBJNAME FROM TABLE(QSYS2.OBJECT_STATISTICS(OBJECT_SCHEMA => '*LIBL',
               OBJTYPELIST => 'FILE', OBJECT_NAME => 'CHEAP')) X"
                .to_string()
        )
        .await,
        [format!("{lib}|CHEAP")]
    );
    // CURRENT TIMESTAMP is a TIMESTAMP (Db2 for i SQL Reference, special
    // registers).
    assert_eq!(
        rows(format!(
            "SELECT DATA_TYPE FROM QSYS2.SYSCOLUMNS
              WHERE TABLE_SCHEMA = '{lib}' AND TABLE_NAME = 'OBJS' AND COLUMN_NAME = 'TS'"
        ))
        .await,
        ["TIMESTMP"]
    );
    assert!(!rows(format!(
        "SELECT * FROM {lib}.OBJS WHERE OBJNAME = 'CUSTMAST'"
    ))
    .await
    .is_empty());
    // The table function: balances above zero are customers 1, 2 and 4
    // (samples/data/CUSTMAST.csv), and it takes a correlated argument.
    assert_eq!(
        rows(format!(
            "SELECT NO FROM TABLE({lib}.OWES(0)) AS O ORDER BY NO"
        ))
        .await,
        ["1", "2", "4"]
    );
    assert_eq!(
        rows(format!(
            "SELECT C.CUSNO, O.NO FROM {lib}.CUSTMAST C, TABLE(OWES(C.CUSBAL - 1)) AS O
              WHERE C.CUSNO = 1 AND O.NO = 1"
        ))
        .await,
        ["1|1"]
    );
}

/// Pointers (ILE RPG Reference: "Basing Pointer Data Type", BASED, %ADDR,
/// pointer arithmetic, %ALLOC, %REALLOC, %STR, DEALLOC(N), and status
/// 00222 "pointer or parameter error" for storage used through *NULL).
#[tokio::test]
async fn rpg_pointers_and_based_storage() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let rpg = "**FREE
ctl-opt dftactgrp(*no);
dcl-s p pointer;
dcl-s q pointer;
dcl-s name char(10) inz('ABCDEFGHIJ');
dcl-s c3 char(3) based(p);
dcl-s c5 char(5) based(ip);
dcl-ds rec qualified based(q);
  a char(2);
  n packed(5:0);
end-ds;
dcl-s buf char(20);
dcl-s s varchar(20) inz('hello');
dcl-s nums int(10) dim(3);
dcl-s one int(10) based(np);

p = %addr(name);
dsply c3;
p += 2;
dsply c3;
c3 = 'xyz';
dsply name;
dsply (%char(p - %addr(name)));
if p > %addr(name);
  dsply 'later';
endif;
ip = %addr(name);
dsply c5;
q = %alloc(10);
rec.a = 'QQ';
rec.n = 123;
dsply (rec.a + %char(rec.n));
q = %realloc(q: 20);
dsply (%char(rec.n));
%str(%addr(buf): 6) = 'abcdefgh';
dsply %str(%addr(buf));
p = %addr(s: *data);
dsply c3;
nums(2) = 42;
np = %addr(nums(2));
dsply (%char(one));
np += 4;
one = 7;
dsply (%char(nums(3)));
dsply (%char(addOne(%addr(nums(3)))));
dsply (%char(nums(3)));
dealloc(n) q;
if q = *null;
  dsply 'null';
endif;
monitor;
  dsply rec.a;
on-error 222;
  dsply 'pointer error';
endmon;
*inlr = *on;

dcl-proc addOne;
  dcl-pi *n int(10);
    at pointer value;
  end-pi;
  dcl-s n int(10) based(at);
  n += 1;
  return n;
end-proc;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "PTRS", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "ABC",
            "CDE",
            "ABxyzFGHIJ",
            "2",
            "later",
            "ABxyz",
            "QQ123",
            "123",
            // At most max - 1 characters, then the null character.
            "abcde",
            // %ADDR(varying: *DATA) addresses the data after the length.
            "hel",
            "42",
            "7",
            "8",
            "8",
            "null",
            "pointer error"
        ]
    );
}

/// The C runtime and the IFS (IBM i open(), read(), write(), close(),
/// unlink(), mkdir() APIs; errno values from "Errno Values for UNIX-Type
/// Functions": EEXIST 3457, ENOENT 3025). A file created with O_CCSID
/// 1208 and O_TEXT_CREAT is UTF-8 and gets the job's EBCDIC text
/// converted; read with O_TEXTDATA converts back; read without it gives
/// the file's bytes ('é' is two bytes in UTF-8).
#[tokio::test]
async fn rpg_c_runtime_and_ifs() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let dir = format!("/t{}", lib.to_lowercase());
    let _ = as4tainr_core::ifs::rmdir(&client, &dir).await;
    let rpg = format!(
        "**FREE
ctl-opt dftactgrp(*no) bnddir('QC2LE');
dcl-pr open int(10) extproc('open');
  path pointer value options(*string);
  oflag int(10) value;
  mode uns(10) value options(*nopass);
  ccsid uns(10) value options(*nopass);
  txtcreatid uns(10) value options(*nopass);
end-pr;
dcl-pr read int(10) extproc('read');
  fd int(10) value;
  buf pointer value;
  n uns(10) value;
end-pr;
dcl-pr write int(10) extproc('write');
  fd int(10) value;
  buf pointer value;
  n uns(10) value;
end-pr;
dcl-pr close int(10) extproc('close');
  fd int(10) value;
end-pr;
dcl-pr unlink int(10) extproc('unlink');
  path pointer value options(*string);
end-pr;
dcl-pr mkdir int(10) extproc('mkdir');
  path pointer value options(*string);
  mode uns(10) value;
end-pr;
dcl-pr getErrno pointer extproc('__errno');
end-pr;
dcl-pr strerror pointer extproc('strerror');
  n int(10) value;
end-pr;
dcl-s errno int(10) based(perr);
dcl-c O_RDONLY 1;
dcl-c O_WRONLY 2;
dcl-c O_CREAT 8;
dcl-c O_CCSID 32;
dcl-c O_TRUNC 64;
dcl-c O_APPEND 256;
dcl-c O_TEXTDATA 16777216;
dcl-c O_TEXT_CREAT 33554432;
dcl-s dir varchar(40) inz('{dir}');
dcl-s fd int(10);
dcl-s line char(7) inz('Hello é');
dcl-s buf char(50);
dcl-s n int(10);

perr = getErrno();
dsply (%char(mkdir(dir: 511)));
dsply (%char(mkdir(dir: 511)) + ' ' + %char(errno));
fd = open(dir + '/a.txt': O_WRONLY + O_CREAT + O_TRUNC + O_CCSID + O_TEXTDATA + O_TEXT_CREAT:
          420: 1208: 0);
n = write(fd: %addr(line): %size(line));
callp close(fd);
fd = open(dir + '/a.txt': O_RDONLY);
n = read(fd: %addr(buf): %size(buf));
callp close(fd);
dsply (%char(n));
fd = open(dir + '/a.txt': O_RDONLY + O_TEXTDATA);
buf = *blanks;
n = read(fd: %addr(buf): %size(buf));
callp close(fd);
dsply (%char(n) + ' ' + %subst(buf: 1: n));
fd = open(dir + '/a.txt': O_WRONLY + O_APPEND + O_TEXTDATA);
n = write(fd: %addr(line): 5);
callp close(fd);
dsply (%char(unlink(dir + '/b.txt')) + ' ' + %char(errno));
dsply %str(strerror(errno));
*inlr = *on;
"
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "CRT", &rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "0",
            "-1 3457",
            "8",
            "7 Hello é",
            "-1 3025",
            "The file or directory does not exist."
        ]
    );
    let (data, ccsid) = as4tainr_core::ifs::read_file(&client, &format!("{dir}/a.txt"))
        .await
        .unwrap();
    assert_eq!(ccsid, 1208);
    assert_eq!(String::from_utf8(data).unwrap(), "Hello éHello");
    as4tainr_core::ifs::unlink(&client, &format!("{dir}/a.txt"))
        .await
        .unwrap();
    as4tainr_core::ifs::rmdir(&client, &dir).await.unwrap();
}

/// IFS commands (IBM CL reference): MKDIR (CPFA0A0 when the directory
/// exists), CPYTOSTMF (trailing blanks removed and *CRLF line ends added;
/// STMFOPT(*NONE) fails when the file exists), CPYFRMSTMF (any CR/LF
/// combination ends a line; TABEXPN(*YES) expands tabs), RNM, WRKLNK,
/// RMVLNK with a generic name (CPFA0A9 when nothing is found), RMVDIR.
#[tokio::test]
async fn ifs_commands() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/c{}", lib.to_lowercase());
    let _ = job.run(&format!("RMVDIR DIR('{dir}') SUBTREE(*ALL)")).await;
    as4tainr_core::source::create_file(&client, &lib, "QSRC", "")
        .await
        .unwrap();
    as4tainr_core::source::put_member(
        &client,
        &as4tainr_core::source::Member {
            library: lib.clone(),
            file: "QSRC".into(),
            name: "M1".into(),
            srctype: "TXT".into(),
            text: String::new(),
            source: "line one   \nline two".into(),
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    let ok = |o: Result<Outcome, anyhow::Error>| match o {
        Ok(Outcome::Messages(m)) => m.join(" "),
        Ok(other) => panic!("unexpected {other:?}"),
        Err(e) => panic!("{e:#}"),
    };
    ok(job.run(&format!("MKDIR DIR('{dir}')")).await);
    let e = job.run(&format!("MKDIR DIR('{dir}')")).await.unwrap_err();
    assert!(e.to_string().starts_with("CPFA0A0"), "{e}");
    let mbr = format!("/QSYS.LIB/{lib}.LIB/QSRC.FILE/M1.MBR");
    ok(job
        .run(&format!(
            "CPYTOSTMF FROMMBR('{mbr}') TOSTMF('{dir}/m1.txt') STMFCCSID(1208)"
        ))
        .await);
    let (data, ccsid) = as4tainr_core::ifs::read_file(&client, &format!("{dir}/m1.txt"))
        .await
        .unwrap();
    assert_eq!(
        (data.as_slice(), ccsid),
        (&b"line one\r\nline two\r\n"[..], 1208)
    );
    let e = job
        .run(&format!(
            "CPYTOSTMF FROMMBR('{mbr}') TOSTMF('{dir}/m1.txt')"
        ))
        .await
        .unwrap_err();
    assert!(e.to_string().starts_with("CPFA097"), "{e}");
    ok(job
        .run(&format!(
            "CPYFRMSTMF FROMSTMF('{dir}/m1.txt') TOMBR('/QSYS.LIB/{lib}.LIB/QSRC.FILE/M2.MBR')"
        ))
        .await);
    let m2 = as4tainr_core::source::find_member(&client, std::slice::from_ref(&lib), "QSRC", "M2")
        .await
        .unwrap();
    assert_eq!(m2.source, "line one\nline two");
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/tabs.txt"),
        b"a\tb\rc\n",
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    ok(job
        .run(&format!(
            "CPYFRMSTMF FROMSTMF('{dir}/tabs.txt') TOMBR('/QSYS.LIB/{lib}.LIB/QSRC.FILE/M2.MBR') MBROPT(*REPLACE)"
        ))
        .await);
    let m2 = as4tainr_core::source::find_member(&client, std::slice::from_ref(&lib), "QSRC", "M2")
        .await
        .unwrap();
    assert_eq!(m2.source, "a       b\nc");
    ok(job
        .run(&format!("RNM OBJ('{dir}/m1.txt') NEWOBJ('m9.txt')"))
        .await);
    match job.run(&format!("WRKLNK OBJ('{dir}')")).await.unwrap() {
        Outcome::Display(d) => {
            let names: Vec<&str> = d
                .rows
                .iter()
                .map(|r| r.split_whitespace().next().unwrap())
                .collect();
            assert_eq!(names, ["m9.txt", "tabs.txt"]);
        }
        other => panic!("unexpected {other:?}"),
    }
    let e = job
        .run(&format!("RMVLNK OBJLNK('{dir}/nothing')"))
        .await
        .unwrap_err();
    assert!(e.to_string().starts_with("CPFA0A9"), "{e}");
    assert!(ok(job.run(&format!("RMVLNK OBJLNK('{dir}/*')")).await).starts_with("2 "));
    ok(job.run(&format!("RMVDIR DIR('{dir}')")).await);
    assert!(as4tainr_core::ifs::stat(&client, &dir)
        .await
        .unwrap()
        .is_none());
}

/// IBM i services (IBM i services reference; Db2 for i SQL Reference):
/// LIBRARY_LIST_INFO (TYPE SYSTEM, CURRENT, USER), JOBLOG_INFO (oldest
/// first; MESSAGE_TYPE INFORMATIONAL, ESCAPE; '*' is the current job),
/// ACTIVE_JOB_INFO (JOB_TYPE INT, JOB_STATUS DSPW), USER_INFO (special
/// authorities padded to 11 characters), SYSTEM_VALUE_INFO (QCCSID is
/// numeric, shipped 65535), SYSROUTINES and SYSPARMS (ROW_TYPE P for a
/// parameter, C for a scalar function's result, as on a real system),
/// IFS_WRITE (CRLF after each line, APPEND by default), IFS_READ,
/// IFS_READ_BINARY, QCMDEXC, and JSON_TABLE with the shapes of IBM's
/// "Using JSON_TABLE" examples.
#[tokio::test]
async fn sql_ibm_i_services() {
    use as4tainr_core::jobs;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let libl: Vec<String> = ["QSYS", "QSYS2", "QHLPSYS", "QUSRSYS"]
        .iter()
        .map(|s| s.to_string())
        .chain([lib.clone()])
        .collect();
    let id = jobs::start_interactive(
        &client,
        &format!("S{}", &lib[1..]),
        "QSECOFR",
        Some(&lib),
        &libl,
    )
    .await
    .unwrap();
    jobs::log(&client, &id, "*INFO", "first message").await;
    jobs::log(&client, &id, "*ESCAPE", "second message").await;
    let run = |sql: String| {
        let client = &client;
        let url = url.clone();
        let libl = libl.clone();
        let lib = lib.clone();
        let job = id.to_string();
        async move {
            as4tainr_core::sql::run_as(client, &url, &libl, "QSECOFR", Some(lib), Some(job), &sql)
                .await
        }
    };
    let rows = |sql: String| {
        let r = run(sql);
        async move {
            match r.await.unwrap() {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => rows
                    .into_iter()
                    .map(|r| r.iter().map(|v| v.trim()).collect::<Vec<_>>().join("|"))
                    .collect::<Vec<_>>(),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows("SELECT ORDINAL_POSITION, SCHEMA_NAME, TYPE FROM QSYS2.LIBRARY_LIST_INFO WHERE ORDINAL_POSITION IN (1, 5)".into()).await,
        ["1|QSYS|SYSTEM".to_string(), format!("5|{lib}|CURRENT")]
    );
    assert_eq!(
        rows("SELECT ORDINAL_POSITION, MESSAGE_TYPE, MESSAGE_TEXT, QUALIFIED_JOB_NAME FROM TABLE(QSYS2.JOBLOG_INFO('*')) AS L".into()).await,
        [
            format!("1|INFORMATIONAL|first message|{id}"),
            format!("2|ESCAPE|second message|{id}")
        ]
    );
    assert!(
        run("SELECT * FROM TABLE(QSYS2.JOBLOG_INFO('999999/NOBODY/NOJOB')) AS L".into())
            .await
            .is_err()
    );
    assert!(rows("SELECT * FROM TABLE(QSYS2.JOBLOG_INFO('999999/NOBODY/NOJOB', IGNORE_ERRORS => 'YES')) AS L".into())
        .await
        .is_empty());
    assert_eq!(
        rows("SELECT JOB_NAME, JOB_TYPE, JOB_STATUS, SUBSYSTEM FROM TABLE(QSYS2.ACTIVE_JOB_INFO(JOB_NAME_FILTER => '*CURRENT')) AS J".into()).await,
        [format!("{id}|INT|DSPW|QINTER")]
    );
    let u = rows("SELECT STATUS, USER_CLASS_NAME, SPECIAL_AUTHORITIES FROM QSYS2.USER_INFO WHERE AUTHORIZATION_NAME = 'QSECOFR'".into()).await;
    assert!(u[0].starts_with("*ENABLED|*SECOFR|*ALLOBJ    *"), "{u:?}");
    assert_eq!(
        rows("SELECT SYSTEM_VALUE_NAME, CURRENT_NUMERIC_VALUE, CURRENT_CHARACTER_VALUE FROM QSYS2.SYSTEM_VALUE_INFO WHERE SYSTEM_VALUE_NAME IN ('QCCSID', 'QDATFMT') ORDER BY 1".into()).await,
        ["QCCSID|65535|-", "QDATFMT|-|MDY"]
    );
    {
        use as4tainr_core::commands::{Environment, Job};
        let env = Environment {
            database_url: url.clone(),
        };
        let mut job = Job {
            client: &client,
            env: &env,
            user: "QSECOFR".into(),
            curlib: Some(lib.clone()),
            libl: vec![lib.clone()],
            display: None,
            id: None,
            calls: Default::default(),
        };
        let script = format!(
            "CREATE PROCEDURE {lib}.P1 (IN A INTEGER, OUT B VARCHAR(10)) LANGUAGE SQL SET B = 'x';
CREATE FUNCTION {lib}.F1 (X DECIMAL(5, 2)) RETURNS INTEGER LANGUAGE SQL RETURN 1;"
        );
        let lines = job
            .run_sql_script(&script, "routines", 0, None, true)
            .await
            .unwrap();
        assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    }
    assert_eq!(
        rows(format!("SELECT ROUTINE_NAME, ROUTINE_TYPE, ROUTINE_BODY, IN_PARMS, OUT_PARMS FROM QSYS2.SYSROUTINES WHERE ROUTINE_SCHEMA = '{lib}' ORDER BY 1")).await,
        ["F1|FUNCTION|SQL|1|0", "P1|PROCEDURE|SQL|1|1"]
    );
    assert_eq!(
        rows(format!("SELECT SPECIFIC_NAME, ORDINAL_POSITION, PARAMETER_MODE, PARAMETER_NAME, NUMERIC_PRECISION, ROW_TYPE FROM QSYS2.SYSPARMS WHERE SPECIFIC_SCHEMA = '{lib}' ORDER BY 1, 2")).await,
        ["F1|1|IN|X|5|P", "F1|2|OUT|-|10|C", "P1|1|IN|A|10|P", "P1|2|OUT|B|-|P"]
    );
    // IFS_WRITE and IFS_READ.
    let dir = format!("/s{}", lib.to_lowercase());
    let _ = as4tainr_core::ifs::mkdir(&client, &dir, 0o755, "QSECOFR").await;
    run(format!("CALL QSYS2.IFS_WRITE(PATH_NAME => '{dir}/f.txt', LINE => 'one', FILE_CCSID => 1208, OVERWRITE => 'REPLACE')")).await.unwrap();
    run(format!("CALL QSYS2.IFS_WRITE('{dir}/f.txt', 'two')"))
        .await
        .unwrap();
    let (data, ccsid) = as4tainr_core::ifs::read_file(&client, &format!("{dir}/f.txt"))
        .await
        .unwrap();
    assert_eq!((data.as_slice(), ccsid), (&b"one\r\ntwo\r\n"[..], 1208));
    assert!(run(format!(
        "CALL QSYS2.IFS_WRITE('{dir}/f.txt', 'three', OVERWRITE => 'NONE')"
    ))
    .await
    .is_err());
    assert_eq!(
        rows(format!(
            "SELECT LINE_NUMBER, LINE FROM TABLE(QSYS2.IFS_READ('{dir}/f.txt')) AS F"
        ))
        .await,
        ["1|one", "2|two"]
    );
    assert_eq!(
        rows(format!("SELECT LINE FROM TABLE(QSYS2.IFS_READ_BINARY(PATH_NAME => '{dir}/f.txt', MAXIMUM_LINE_LENGTH => 5)) AS F")).await,
        ["6F6E650D0A", "74776F0D0A"]
    );
    as4tainr_core::ifs::unlink(&client, &format!("{dir}/f.txt"))
        .await
        .unwrap();
    as4tainr_core::ifs::rmdir(&client, &dir).await.unwrap();
    // QCMDEXC runs a CL command.
    run(format!(
        "CALL QSYS2.QCMDEXC('CRTDTAARA DTAARA({lib}/SVC) TYPE(*CHAR) LEN(3)')"
    ))
    .await
    .unwrap();
    assert_eq!(
        rows(format!(
            "SELECT OBJNAME FROM TABLE(QSYS2.OBJECT_STATISTICS('{lib}', '*DTAARA')) AS O"
        ))
        .await,
        ["SVC"]
    );
    // JSON_TABLE: a row per array element; a missing key is null (lax);
    // a nested path is a left outer join, its ordinality 1 when it finds
    // nothing.
    let doc = r#"{"employees":[{"id":901,"name":{"first":"John","last":"Doe"},"office":"E-334","phones":[{"type":"home","number":"555-3762"},{"type":"work","number":"555-7242"}]},{"id":904,"name":{"first":"Sally","last":"Smith"}}]}"#;
    assert_eq!(
        rows(format!("SELECT U.FIRST, U.OFFICE FROM JSON_TABLE('{doc}', 'lax $.employees[*]' COLUMNS(FIRST VARCHAR(10) PATH 'lax $.name.first', OFFICE CHAR(5) PATH 'lax $.office')) AS U")).await,
        ["John|E-334", "Sally|-"]
    );
    assert_eq!(
        rows(format!("SELECT U.FIRST, U.NESTED_ORD, U.TYPE FROM JSON_TABLE('{doc}', 'lax $.employees[*]' COLUMNS(FIRST VARCHAR(10) PATH 'lax $.name.first', NESTED PATH 'lax $.phones[*]' COLUMNS(NESTED_ORD FOR ORDINALITY, TYPE VARCHAR(10) PATH 'lax $.type'))) AS U")).await,
        ["John|1|home", "John|2|work", "Sally|1|-"]
    );
    assert_eq!(
        rows(format!("SELECT U.OFFICE FROM JSON_TABLE('{doc}', 'lax $.employees[*]' COLUMNS(OFFICE VARCHAR(10) FORMAT JSON PATH 'lax $.office')) AS U")).await,
        ["\"E-334\"", "-"]
    );
    jobs::end(&client, &id, true).await.unwrap();
}

/// CL programs (IBM CL reference): loops with LEAVE and ITERATE, DOUNTIL
/// running its body at least once, nested DOFOR, variables in command
/// parameters, CALL between CL programs with parameters.
#[tokio::test]
async fn cl_programs_control_flow() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let loops = "PGM PARM(&N)
  DCL &N *DEC (3 0)
  DCL &I *DEC (3 0)
  DCL &J *DEC (3 0)
  DCL &S *DEC (5 0)
  DCL &T *CHAR 20
  /* DOWHILE with ITERATE (skip 3) and LEAVE (stop after 5) */
  DOWHILE COND(&I < &N)
    CHGVAR &I (&I + 1)
    IF (&I = 3) THEN(ITERATE)
    IF (&I > 5) THEN(LEAVE)
    CHGVAR &S (&S + &I)
  ENDDO
  SNDPGMMSG MSG('while' *BCAT %CHAR(&S))
  /* DOUNTIL runs once even when the condition is already true */
  CHGVAR &S 0
  DOUNTIL COND(&S > 0)
    CHGVAR &S (&S + 10)
  ENDDO
  SNDPGMMSG MSG('until' *BCAT %CHAR(&S))
  /* Nested DOFOR; ITERATE in the inner loop */
  DOFOR VAR(&I) FROM(1) TO(3)
    DOFOR VAR(&J) FROM(3) TO(1) BY(-1)
      IF (&J = 2) THEN(ITERATE)
      CHGVAR &T (&T *TCAT %CHAR(&I) *TCAT %CHAR(&J))
    ENDDO
  ENDDO
  SNDPGMMSG MSG(%TRIM(&T))
  CALL PGM(CLCHILD) PARM('abc' &N)
ENDPGM";
    let child = "PGM PARM(&A &B)
  DCL &A *CHAR 5
  DCL &B *DEC (3 0)
  SNDPGMMSG MSG('child got' *BCAT %TRIM(&A) *BCAT %CHAR(&B * 2))
ENDPGM";
    // 1+2+4+5 = 12 (3 skipped, stops at 6); until: 10; pairs (i,j) with
    // j in 3,1: 13 11 23 21 33 31; child doubles 9.
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("CLLOOPS", loops), ("CLCHILD", child)],
        "CALL CLLOOPS PARM(9)",
    )
    .await
    .unwrap();
    assert_eq!(
        m,
        ["while 12", "until 10", "131123213331", "child got abc 18"]
    );

    // Variables in command parameters: CRTLIB with &LIB and &TXT.
    let mk = "PGM
  DCL &L *CHAR 10 VALUE('CLMADE')
  DCL &TXT *CHAR 30 VALUE('Made by a CL program')
  CRTLIB LIB(&L) TEXT(&TXT)
  MONMSG MSGID(CPF2111) EXEC(SNDPGMMSG MSG('already there'))
  CHKOBJ OBJ(&L) OBJTYPE(*LIB)
  SNDPGMMSG MSG('found' *BCAT &L)
ENDPGM";
    client
        .batch_execute("DROP SCHEMA IF EXISTS clmade CASCADE; DELETE FROM as4tainr.objects WHERE library='QSYS' AND name='CLMADE'")
        .await
        .unwrap();
    let m = cl_job_run(&client, &url, &lib, &[("CLMK", mk)], "CALL CLMK")
        .await
        .unwrap();
    // CRTLIB's completion message is CPC2102 (message IDs as `system`
    // shows them on PUB400, IBM i 7.5, 2026-10-21).
    assert_eq!(m, ["CPC2102: Library CLMADE was created.", "found CLMADE"]);
    let m = cl_job_run(&client, &url, &lib, &[], "CALL CLMK")
        .await
        .unwrap();
    assert_eq!(
        m,
        ["already there", "found CLMADE"],
        "CPF2111 monitored at command level"
    );
}

/// MONMSG (IBM CL reference): a command-level MONMSG is tried before the
/// program-level one; generic IDs; an *ESCAPE message ends the program
/// and the caller can monitor for it; an unmonitored error ends the
/// program with the error.
#[tokio::test]
async fn cl_programs_monitor_messages() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let thrower = "PGM
  SNDPGMMSG MSGID(CPF9898) MSGF(QCPFMSG) MSGDTA('thrown on purpose') MSGTYPE(*ESCAPE)
  SNDPGMMSG MSG('not reached')
ENDPGM";
    let catcher = "PGM
  DCL &OK *LGL
  MONMSG MSGID(CPF0000) EXEC(GOTO CMDLBL(GLOBAL))
  CALL CLTHROW
  MONMSG MSGID(CPF9898) EXEC(CHGVAR &OK '1')
  IF COND(&OK) THEN(SNDPGMMSG MSG('caught at command level'))
  DSPLIB NOSUCHLIB
  SNDPGMMSG MSG('not reached either')
  RETURN
GLOBAL:
  SNDPGMMSG MSG('caught at program level')
ENDPGM";
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("CLTHROW", thrower), ("CLCATCH", catcher)],
        "CALL CLCATCH",
    )
    .await
    .unwrap();
    assert_eq!(m, ["caught at command level", "caught at program level"]);

    let e = cl_job_run(&client, &url, &lib, &[], "CALL CLTHROW")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPF9898: thrown on purpose"), "{e}");
    let unmonitored = "PGM
  SNDPGMMSG MSG('before')
  DSPLIB NOSUCHLIB
ENDPGM";
    let e = cl_job_run(&client, &url, &lib, &[("CLBAD", unmonitored)], "CALL CLBAD")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("CPF2110") && e.contains("line 3"), "{e}");
    // Wrong number of parameters, decimal overflow.
    let ovf = "PGM PARM(&D)
  DCL &D *DEC (3 0)
  CHGVAR &D (&D * 100)
ENDPGM";
    let e = cl_job_run(
        &client,
        &url,
        &lib,
        &[("CLOVF", ovf)],
        "CALL CLOVF PARM(50)",
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("MCH1210"), "{e}");
    assert!(cl_job_run(&client, &url, &lib, &[], "CALL CLOVF PARM(1 2)")
        .await
        .is_err());
}

/// Run commands in a job whose library list is just `lib`.
async fn job_run(
    client: &Client,
    lib: &str,
    line: &str,
) -> anyhow::Result<as4tainr_core::commands::Outcome> {
    use as4tainr_core::commands::{Environment, Job};
    let env = Environment {
        database_url: String::new(),
    };
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.to_string()),
        libl: vec![lib.to_string()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.run(line).await
}

/// Data areas: CRTDTAARA, CHGDTAARA (all or a substring), DSPDTAARA,
/// DLTDTAARA and RTVDTAARA in a CL program. Expected values follow IBM's
/// command descriptions: a *CHAR value is padded with blanks to its length,
/// a substring change replaces only those positions, *DEC values must fit
/// the length and decimal positions, LEN(*DFT) for *DEC is (15 5).
#[tokio::test]
async fn data_areas_commands_and_rtvdtaara() {
    use as4tainr_core::commands::Outcome;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let run = |line: String| {
        let client = &client;
        let lib = lib.clone();
        async move { job_run(client, &lib, &line).await }
    };
    let msgs = |o: Outcome| match o {
        Outcome::Messages(m) => m,
        other => panic!("unexpected {other:?}"),
    };
    let m = msgs(
        run(format!(
            "CRTDTAARA DTAARA({lib}/NEXTNO) TYPE(*DEC) LEN(7 0) VALUE(1004) TEXT('Next number')"
        ))
        .await
        .unwrap(),
    );
    // CPC0904 is the completion message's ID (observed on IBM i 7.5 with
    // the PASE `system` utility); the text is as4tainr's.
    assert_eq!(
        m,
        [format!(
            "CPC0904: Data area NEXTNO was created in library {lib}."
        )]
    );
    let e = run(format!("CRTDTAARA {lib}/NEXTNO *DEC"))
        .await
        .unwrap_err();
    assert!(e.to_string().starts_with("CPF1023"), "{e}");
    // Unqualified: created in the current library.
    run("CRTDTAARA CO *CHAR LEN(10) VALUE('Acme')".into())
        .await
        .unwrap();
    run("CHGDTAARA DTAARA(CO (6 5)) VALUE('Ltd')".into())
        .await
        .unwrap();
    let Outcome::Display(d) = run("DSPDTAARA CO".into()).await.unwrap() else {
        panic!()
    };
    // The value line of IBM i 7.5's DSPDTAARA display (PUB400 capture):
    // the offset ending in column 6, the quoted value from column 13.
    assert_eq!(d.rows, ["    0      'Acme Ltd  '"]);
    let lines = &d.ibm.as_ref().unwrap().lines;
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Type  . . . . . . . . . :   *CHAR")),
        "{lines:?}"
    );
    run(format!("CRTDTAARA {lib}/DFT TYPE(*DEC)"))
        .await
        .unwrap();
    let Outcome::Display(d) = run("DSPDTAARA DFT".into()).await.unwrap() else {
        panic!()
    };
    // A *DEC value is a header line, not a list (IBM i 7.5's DSPDTAARA of
    // a *DEC data area on PUB400: "Value . . . :   0,00" with QDECFMT J).
    let lines = &d.ibm.as_ref().unwrap().lines;
    assert!(lines.iter().any(|l| l.ends_with(":   15 5")), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Value") && l.ends_with(":   0.00000")),
        "{lines:?}"
    );
    assert!(d.rows.is_empty());
    run("CRTDTAARA FLAG *LGL".into()).await.unwrap();
    // Values that do not fit, and substrings of a *DEC data area.
    assert!(run("CHGDTAARA NEXTNO VALUE(12345678)".into())
        .await
        .is_err());
    assert!(run("CHGDTAARA DTAARA(NEXTNO (1 2)) VALUE(1)".into())
        .await
        .is_err());
    assert!(run("CHGDTAARA DTAARA(CO (8 5)) VALUE(X)".into())
        .await
        .is_err());
    assert!(run("CHGDTAARA FLAG VALUE('2')".into()).await.is_err());

    let pgm = r#"
PGM
  DCL &N *DEC (7 0)
  DCL &C *CHAR 4
  DCL &F *LGL
  RTVDTAARA NEXTNO &N
  CHGVAR &N (&N + 1)
  CHGDTAARA DTAARA(NEXTNO) VALUE(&N)
  RTVDTAARA DTAARA(CO (1 4)) RTNVAR(&C)
  CHGDTAARA FLAG '1'
  RTVDTAARA FLAG &F
  IF &F THEN(SNDPGMMSG MSG(%CHAR(&N) *BCAT &C))
  RTVDTAARA NOSUCH &C
  MONMSG CPF1015 EXEC(SNDPGMMSG MSG('NOSUCH is missing'))
ENDPGM
"#;
    let m = cl_job_run(&client, &url, &lib, &[("DTAPGM", pgm)], "CALL DTAPGM")
        .await
        .unwrap();
    assert!(m.contains(&"1005 Acme".to_string()), "{m:?}");
    assert!(m.contains(&"NOSUCH is missing".to_string()), "{m:?}");
    // A *DEC data area needs a *DEC variable.
    let bad = "PGM\n DCL &C *CHAR 10\n RTVDTAARA NEXTNO &C\nENDPGM\n";
    let e = cl_job_run(&client, &url, &lib, &[("BADPGM", bad)], "CALL BADPGM")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("must be a *DEC variable"), "{e}");

    let m = msgs(run("DLTDTAARA CO".into()).await.unwrap());
    // CPC2191 (observed on IBM i 7.5).
    assert_eq!(
        m,
        [format!(
            "CPC2191: Object CO of type *DTAARA in library {lib} was deleted."
        )]
    );
    let e = run("DSPDTAARA CO".into()).await.unwrap_err();
    assert!(e.to_string().starts_with("CPF1015"), "{e}");
}

/// RPG data areas on the native engine (ILE RPG Reference, IN and OUT):
/// `IN *LOCK` holds the data area until OUT; CHGDTAARA in another job
/// waits for it. IN of a data area that does not exist fails with CPF1015.
#[tokio::test(flavor = "multi_thread")]
async fn rpg_data_area_in_lock_out() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    as4tainr_core::dtaara::create(
        &client,
        &lib,
        "ORDNXT",
        as4tainr_core::dtaara::Type::Dec,
        7,
        0,
        "1004",
        "",
    )
    .await
    .unwrap();
    // While the program waits for the operator (DSPLY with a reply), it
    // holds the lock: another job's CHGDTAARA must wait.
    let (url2, lib2) = (url.clone(), lib.clone());
    let changer: std::sync::Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<String>>>> =
        Default::default();
    let ch = changer.clone();
    let (tx, seen) = operator(vec![(
        press("ENTER", &[("REPLY", "y")], &[]),
        Box::new(move |f| {
            assert_eq!(f.layers.last().unwrap().format, "DSPLYR");
            let h = tokio::spawn(async move {
                let other = runtime::connect_db(&url2).await.unwrap();
                as4tainr_core::dtaara::change(
                    &other,
                    "ORDNXT",
                    std::slice::from_ref(&lib2),
                    &lib2,
                    None,
                    "5000",
                )
                .await
                .unwrap()
                .value
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
            assert!(!h.is_finished(), "CHGDTAARA did not wait for the lock");
            *ch.lock().unwrap() = Some(h);
        }),
    )]);
    let src = "**FREE\ndcl-s nxt packed(7:0) dtaara('ORDNXT');\ndcl-s r char(1);\nin *lock nxt;\ndsply 'Next order?' '' r;\nnxt += 1;\nout nxt;\ndsply %char(nxt);\n*inlr = *on;\n";
    let r = run_rpg(&url, &lib, "QSECOFR", "DTALOCK", src, Some(tx), None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1005"]);
    assert_eq!(seen.await.unwrap(), 1);
    let h = changer.lock().unwrap().take().unwrap();
    assert_eq!(
        h.await.unwrap(),
        "5000",
        "CHGDTAARA ran after OUT released the lock"
    );
    // IN of a data area that is not there.
    let src = "**FREE\ndcl-s x char(10) dtaara('NOSUCH');\nin x;\n*inlr = *on;\n";
    let r = run_rpg(&url, &lib, "QSECOFR", "DTANONE", src, None, None).await;
    let e = format!("{:#}", r.error.unwrap());
    assert!(e.contains("CPF1015"), "{e}");
}

/// Work management: SBMJOB puts a job on QBATCH; the subsystem runs it in
/// a new job with the submitter's library list and keeps its job log;
/// the submitter gets CPF1241 (completed normally) or CPF1240 (ended
/// abnormally) on their message queue, as IBM's work management
/// documentation describes. Also WRKSBMJOB, WRKACTJOB, ENDJOB of a job
/// still on the queue, SNDMSG and DSPMSG.
#[tokio::test]
async fn submitted_jobs_run_in_qbatch() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::jobs;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let device = format!("D{lib}");
    let id = jobs::start_interactive(
        &client,
        &device,
        "QSECOFR",
        Some(&lib),
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };
    let msg = |o: Outcome| match o {
        Outcome::Messages(m) => m.join("\n"),
        other => panic!("unexpected {other:?}"),
    };
    // Runs one submitted job, whether this test or a running server's
    // QBATCH subsystem picks it up.
    let run = |number: i64| {
        let env = env.clone();
        let client = &client;
        async move {
            if let Some(info) = jobs::claim(client, number).await.unwrap() {
                jobs::run_batch_job(&env, &info).await.unwrap();
            }
            for _ in 0..100 {
                if jobs::get(client, number).await.unwrap().status == "OUTQ" {
                    return jobs::get(client, number).await.unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            panic!("job {number} did not end");
        }
    };
    let number_in = |m: &str| -> i64 { m.split_whitespace().nth(2).unwrap()[..6].parse().unwrap() };

    // A job that works: unqualified names resolve through the library
    // list the job inherited (INLLIBL(*CURRENT)).
    let m = msg(job
        .run("SBMJOB CMD(CRTDTAARA DTAARA(D1) TYPE(*CHAR) LEN(5) VALUE('ok')) JOB(MAKEDTA)")
        .await
        .unwrap());
    assert!(m.starts_with("CPC1221: Job "), "{m}");
    assert!(
        m.contains("/QSECOFR/MAKEDTA was put on job queue QBATCH"),
        "{m}"
    );
    let n1 = number_in(&m);
    let done = run(n1).await;
    assert_eq!(done.end_code, "NORMAL");
    let d = as4tainr_core::dtaara::resolve(&client, "D1", std::slice::from_ref(&lib), &lib)
        .await
        .unwrap();
    assert_eq!(d.value, "ok   ");
    let log = jobs::job_log(&client, n1).await.unwrap();
    assert!(log
        .iter()
        .any(|e| e.kind == "*RQS" && e.text.starts_with("CRTDTAARA")));
    assert!(log
        .iter()
        .any(|e| e.msgid == "CPC0904" && e.text.contains("D1")));
    assert!(log.last().unwrap().text.starts_with("CPF1241"));

    // A job that fails ends abnormally.
    let m = msg(job
        .run("SBMJOB CMD(DLTDTAARA NOPE) JOB(FAILS)")
        .await
        .unwrap());
    let n2 = number_in(&m);
    let done = run(n2).await;
    assert_eq!(done.end_code, "ABNORMAL");
    let log = jobs::job_log(&client, n2).await.unwrap();
    // DLTDTAARA of a missing data area: CPF2105 (PUB400, IBM i 7.5).
    assert!(log.iter().any(|e| e.msgid == "CPF2105"), "{log:?}");

    // The completion messages are on QSECOFR's message queue.
    let msgs = jobs::messages(&client, "QSECOFR", false).await.unwrap();
    let job1 = format!("{n1:06}/QSECOFR/MAKEDTA");
    let job2 = format!("{n2:06}/QSECOFR/FAILS");
    assert!(msgs.iter().any(
        |x| x.text == format!("CPF1241: Job {job1} completed normally.") && x.from_user == "QSYS"
    ));
    assert!(msgs
        .iter()
        .any(|x| x.text == format!("CPF1240: Job {job2} ended abnormally.")));

    // A command that does not parse is never submitted.
    assert!(job.run("SBMJOB CMD(NOSUCHCMD X)").await.is_err());

    // WRKSBMJOB and the job log of a submitted job.
    let Outcome::Display(d) = job.run("WRKSBMJOB").await.unwrap() else {
        panic!()
    };
    assert!(d.keys.contains(&job1) && d.keys.contains(&job2));
    let Outcome::Display(d) = job.run(&format!("DSPJOBLOG JOB({job1})")).await.unwrap() else {
        panic!()
    };
    assert!(
        d.rows.iter().any(|r| r.contains("> CRTDTAARA")),
        "{:?}",
        d.rows
    );

    // WRKACTJOB shows this interactive job under QINTER.
    let Outcome::Display(d) = job.run("WRKACTJOB").await.unwrap() else {
        panic!()
    };
    assert!(d.keys.contains(&id.to_string()));

    // ENDJOB of a job that has not started takes it off the queue.
    let m = msg(job.run("SBMJOB CMD(DSPLIBL) JOB(NEVER)").await.unwrap());
    let n3 = number_in(&m);
    let ended = job.run(&format!("ENDJOB JOB({n3:06}/QSECOFR/NEVER)")).await;
    match ended {
        Ok(o) => assert!(msg(o).contains("taken off job queue QBATCH")),
        // A running server's QBATCH may have started it already.
        Err(e) => assert!(
            e.to_string().contains("active") || e.to_string().contains("ended"),
            "{e}"
        ),
    }

    // SNDMSG / DSPMSG.
    msg(job
        .run(&format!(
            "SNDMSG MSG('Lunch at noon, {lib}') TOUSR(QSECOFR)"
        ))
        .await
        .unwrap());
    let Outcome::Display(d) = job.run("DSPMSG").await.unwrap() else {
        panic!()
    };
    assert!(d
        .rows
        .iter()
        .any(|r| r.ends_with(&format!("Lunch at noon, {lib}"))));
    let after = jobs::messages(&client, "QSECOFR", false).await.unwrap();
    assert!(after
        .iter()
        .all(|x| !x.is_new || x.id > after.last().unwrap().id));
    assert!(job.run("SNDMSG MSG(hi) TOUSR(NOBODY99)").await.is_err());

    jobs::end(&client, &id, true).await.unwrap();
    assert_eq!(jobs::get(&client, id.number).await.unwrap().status, "OUTQ");
}

/// Record locks at the record-level access layer the RPG engine uses: a
/// file opened for update locks the record it reads; another job waits
/// WAITRCD, then gets CPF5027 naming the holder. N (no lock) and input
/// files do not lock; a logical file shares the physical file's locks; the
/// next read, UPDATE, UNLOCK and closing release.
#[tokio::test]
async fn record_locks() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    crtlf(
        &mut client,
        &lib,
        "CUSTNAME",
        include_str!("../../samples/qddssrc/CUSTNAME.lf"),
    )
    .await;
    // The record wait: long enough that A's UPDATE, 200 ms after B starts
    // waiting, comes before it ends even on a loaded machine (700 ms
    // timed out once in a full run, 2026-10-29).
    let wait = std::time::Duration::from_millis(2000);
    let holder = format!("000042/QSECOFR/L{lib}");
    let a = runtime::connect_db(&url).await.unwrap();
    runtime::job_session(&a, Some(&holder), wait).await.unwrap();
    let b = runtime::connect_db(&url).await.unwrap();
    runtime::job_session(&b, Some("000043/QSECOFR/OTHER"), wait)
        .await
        .unwrap();
    async fn for_update(c: &Client, lib: &str, name: &str) -> rla::OpenFile {
        let f = catalog::resolve_file(c, &[lib.to_string()], name)
            .await
            .unwrap();
        let (l, pf) = catalog::physical_file_of(c, &f.library, &f.file.name)
            .await
            .unwrap();
        rla::OpenFile::new(f).for_update(format!("{l}/{pf}"))
    }
    async fn chain(
        f: &mut rla::OpenFile,
        c: &Client,
        key: &str,
        lock: bool,
    ) -> anyhow::Result<rla::ReadResult> {
        let r = f.chain(c, &[key.to_string()]).await?;
        f.lock_after_read(c, r, lock, None).await
    }
    let mut fa = for_update(&a, &lib, "CUSTMAST").await;
    let mut fb = for_update(&b, &lib, "CUSTMAST").await;
    let mut fin = open(&b, &lib, "CUSTMAST").await;
    let mut flf = for_update(&b, &lib, "CUSTNAME").await;

    // A locks customer 1; B waits for it, then gets CPF5027 naming A's job.
    assert!(chain(&mut fa, &a, "1", true).await.unwrap().found);
    let t0 = std::time::Instant::now();
    let e = chain(&mut fb, &b, "1", true).await.unwrap_err().to_string();
    assert!(e.starts_with("CPF5027"), "{e}");
    assert!(e.contains(&holder), "{e}");
    assert!(
        t0.elapsed() >= std::time::Duration::from_millis(600),
        "did not wait"
    );
    // N extender, and a file opened for input: no lock, no wait.
    assert!(chain(&mut fb, &b, "1", false).await.unwrap().found);
    assert!(chain(&mut fin, &b, "1", true).await.unwrap().found);
    // The logical file shares the physical file's record locks.
    let e = chain(&mut flf, &b, "Acme Tooling", true)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPF5027"), "{e}");

    // A's next read moves the lock: customer 1 is free, 2 is locked.
    let r = fa.read(&a).await.unwrap();
    let r = fa.lock_after_read(&a, r, true, None).await.unwrap();
    assert_eq!(field(&r, "CUSNO"), "2");
    assert!(chain(&mut fb, &b, "1", true).await.unwrap().found);
    fb.release(&b).await.unwrap();
    assert!(chain(&mut fb, &b, "2", true)
        .await
        .unwrap_err()
        .to_string()
        .starts_with("CPF5027"));
    // UPDATE releases (and B, waiting meanwhile, sees A's change).
    let mut rec = chain(&mut fa, &a, "2", true).await.unwrap().record.unwrap();
    rec.insert("CUSCTY".into(), "Evanston".into());
    let wait_b = tokio::spawn(async move {
        let r = chain(&mut fb, &b, "2", true).await;
        (fb, b, r)
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    fa.update(&a, &rec).await.unwrap();
    fa.release(&a).await.unwrap();
    let (mut fb, b, r) = wait_b.await.unwrap();
    let r = r.unwrap();
    assert!(r.found);
    assert_eq!(field(&r, "CUSCTY").trim(), "Evanston");
    fb.release(&b).await.unwrap();
    // UNLOCK releases too.
    assert!(chain(&mut fa, &a, "3", true).await.unwrap().found);
    fa.release(&a).await.unwrap();
    assert!(chain(&mut fb, &b, "3", true).await.unwrap().found);
}

/// Spooled files: lines kept by page and line, WRKSPLF/DSPSPLF/DLTSPLF,
/// display commands in batch printing to QSYSPRT, and the job log of a
/// batch job that ends abnormally kept as QPJOBLOG (QDFTJOBD has
/// LOG(4 0 *NOLIST): no job log when the job ends normally). Also the
/// bridge's splopen/splwrite/splclose.
#[tokio::test(flavor = "multi_thread")]
async fn spooled_files() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::{jobs, spool};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let user = format!("U{}", &lib[1..]);
    let env = Environment {
        database_url: url.clone(),
    };
    let id = jobs::start_interactive(
        &client,
        "SPLDEV",
        &user,
        Some(&lib),
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: user.clone(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };

    // Pages: lines land where they were printed; gaps are blank lines.
    let spl = spool::open(&client, &id, "QSYSPRT", "MYPGM").await.unwrap();
    spool::write(
        &client,
        spl,
        &[
            (1, 1, "Title   ".into()),
            (1, 3, "Body".into()),
            (2, 1, "Page two".into()),
        ],
    )
    .await
    .unwrap();
    spool::close(&client, spl).await.unwrap();
    let pages = spool::pages(&client, spl).await.unwrap();
    assert_eq!(
        pages,
        vec![
            vec!["Title".to_string(), String::new(), "Body".into()],
            vec!["Page two".to_string()]
        ]
    );
    let f = spool::find(&client, "qsysprt", id.number, None)
        .await
        .unwrap();
    assert_eq!(
        (f.number, f.pages, f.status.as_str(), f.user_data.as_str()),
        (1, 2, "RDY", "MYPGM")
    );

    // A program's printer output becomes a spooled file of its job, with
    // the program's name as user data.
    let custprt = "     FQSYSPRT   O    F  132        PRINTER\n     C                   EXCEPT    LINES\n     C                   SETON                                        LR\n     OQSYSPRT   E            LINES          1\n     O                                            6 'Report'\n     O          E            LINES          1\n     O                                            6 'line 2'\n";
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "CUSTPRT",
        custprt,
        None,
        Some(id.to_string()),
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let f = spool::find(&client, "QSYSPRT", id.number, None)
        .await
        .unwrap();
    assert_eq!((f.number, f.user_data.as_str(), f.pages), (2, "CUSTPRT", 1));

    // WRKSPLF lists them; DSPSPLF shows one; DLTSPLF deletes it.
    let Outcome::Display(d) = job.run("WRKSPLF").await.unwrap() else {
        panic!()
    };
    assert_eq!(d.rows.len(), 2);
    assert!(d.keys[1].starts_with(&format!("FILE(QSYSPRT) JOB({id}) SPLNBR(2)")));
    let Outcome::Display(d) = job
        .run(&format!("DSPSPLF FILE(QSYSPRT) JOB({id}) SPLNBR(1)"))
        .await
        .unwrap()
    else {
        panic!()
    };
    // The viewer shows the lines one after another, as IBM i's does; its
    // Page/Line tells the page (tools/golden/screens4.txt).
    assert_eq!(d.rows, ["Title", "", "Body", "Page two"]);
    let v = d.spool.expect("spooled file viewer");
    assert_eq!(v.lines[2], (1, 3, "Body".to_string()));
    assert_eq!(v.lines[3], (2, 1, "Page two".to_string()));
    job.run("DLTSPLF QSYSPRT SPLNBR(1)").await.unwrap();
    assert!(spool::find(&client, "QSYSPRT", id.number, Some(1))
        .await
        .is_err());

    // DSPJOBLOG OUTPUT(*PRINT): IBM i's layout, "Display Job Log" at
    // column 47 (PUB400 2026-10-28, tools/golden/verify17.txt x17_own).
    job.run("DSPJOBLOG OUTPUT(*PRINT)").await.unwrap();
    let printed = spool::pages(
        &client,
        spool::find(&client, "QPJOBLOG", id.number, None)
            .await
            .unwrap()
            .id,
    )
    .await
    .unwrap();
    assert_eq!(
        printed[0][0].find("Display Job Log"),
        Some(46),
        "{printed:?}"
    );
    assert!(printed[0][3].starts_with("MSGID      TYPE"), "{printed:?}");

    // Batch: a display command prints QSYSPRT; an abnormal end keeps QPJOBLOG.
    let run_batch = |cmd: String| {
        let env = env.clone();
        let client = &client;
        let user = user.clone();
        let lib = lib.clone();
        async move {
            let b = jobs::submit(
                client,
                &jobs::Submit {
                    name: "SPLTEST".into(),
                    user: user.clone(),
                    command: cmd,
                    jobq: "SPLTESTQ".into(), // not QBATCH: a running server must not take it
                    curlib: Some(lib.clone()),
                    libl: vec![lib],
                    submitted_by: user,
                    submitter: None,
                },
            )
            .await
            .unwrap();
            let info = jobs::claim(client, b.number).await.unwrap().unwrap();
            let normal = jobs::run_batch_job(&env, &info).await.unwrap();
            (b, normal)
        }
    };
    let (b1, normal) = run_batch(format!("DSPPFM {lib}/CUSTMAST")).await;
    assert!(normal);
    let pages = spool::pages(
        &client,
        spool::find(&client, "QSYSPRT", b1.number, None)
            .await
            .unwrap()
            .id,
    )
    .await
    .unwrap();
    assert!(
        pages[0].iter().any(|l| l.contains("Bluebird Supply")),
        "{pages:?}"
    );
    assert!(
        spool::find(&client, "QPJOBLOG", b1.number, None)
            .await
            .is_err(),
        "no job log after a normal end"
    );
    let (b2, normal) = run_batch("DLTDTAARA NOPE".into()).await;
    assert!(!normal);
    let log = spool::pages(
        &client,
        spool::find(&client, "QPJOBLOG", b2.number, None)
            .await
            .unwrap()
            .id,
    )
    .await
    .unwrap();
    // DLTDTAARA of a data area that is not there: CPF2105 (PUB400, IBM i
    // 7.5, `system "DLTDTAARA ..."` twice, 2026-10-21).
    assert!(log[0].iter().any(|l| l.contains("CPF2105")), "{log:?}");
    // A job log printed at the end of a job: "Job Log" at column 50, the
    // request after a minus sign (PUB400 2026-10-27,
    // tools/golden/verify8.txt c8_joblog7).
    assert_eq!(log[0][0].find("Job Log"), Some(49), "{log:?}");
    assert!(log[0][1].contains(":   SPLTEST "), "{log:?}");
    assert!(
        log[0]
            .iter()
            .any(|l| l.ends_with("Message . . . . :  -DLTDTAARA NOPE")),
        "{log:?}"
    );
    let Outcome::Display(d) = job.run("WRKSPLF").await.unwrap() else {
        panic!()
    };
    assert!(d
        .keys
        .iter()
        .any(|k| k.contains("QPJOBLOG") && k.contains("/SPLTEST)")));
    jobs::end(&client, &id, true).await.unwrap();
}

/// Object authority (IBM i security reference): the creator owns an
/// object; *PUBLIC gets *CHANGE (QCRTAUT as shipped); a user's authority
/// is *ALLOBJ, else owner (*ALL), else a private authority, else *PUBLIC.
/// *USE reads, *CHANGE changes, *ALL deletes and manages authority;
/// anything less fails with CPF9802.
#[tokio::test(flavor = "multi_thread")]
async fn object_authority() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::{jobs, users};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let bob = format!("B{}", &lib[1..]);
    let _ = users::delete(&client, &bob).await;
    users::create(
        &client,
        users::NewProfile {
            name: bob.clone(),
            password: users::NewPassword::None,
            expired: false,
            enabled: true,
            usrcls: "*USER".into(),
            special: vec![],
            curlib: "*CRTDFT".into(),
            text: String::new(),
            inlpgm: users::DEFAULT_INLPGM.into(),
            inlmnu: users::DEFAULT_INLMNU.into(),
        },
    )
    .await
    .unwrap();
    let env = Environment {
        database_url: url.clone(),
    };
    let job_for = |user: &str| Job {
        client: &client,
        env: &env,
        user: user.to_string(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let mut sec = job_for("QSECOFR");
    let mut b = job_for(&bob);
    let denied = |r: anyhow::Result<Outcome>| match r {
        Err(e) => e.to_string().starts_with("CPF9802"),
        Ok(_) => false,
    };

    sec.run("CRTDTAARA AREA *CHAR LEN(10)").await.unwrap();
    let Outcome::Display(d) = sec
        .run(&format!("DSPOBJAUT {lib}/AREA *DTAARA"))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(d.info.contains(&("Owner".into(), "QSECOFR".into())));
    assert!(
        d.rows
            .iter()
            .any(|r| r.starts_with("*PUBLIC") && r.ends_with("*CHANGE")),
        "{:?}",
        d.rows
    );
    b.run("CHGDTAARA AREA 'by bob'").await.unwrap();

    // *PUBLIC *USE: BOB can read but not change.
    sec.run(&format!(
        "GRTOBJAUT OBJ({lib}/AREA) OBJTYPE(*DTAARA) USER(*PUBLIC) AUT(*USE)"
    ))
    .await
    .unwrap();
    assert!(denied(b.run("CHGDTAARA AREA 'again'").await));
    b.run("DSPDTAARA AREA").await.unwrap();
    // A private authority beats *PUBLIC, both ways.
    sec.run(&format!("GRTOBJAUT AREA *DTAARA USER({bob}) AUT(*CHANGE)"))
        .await
        .unwrap();
    b.run("CHGDTAARA AREA 'again'").await.unwrap();
    sec.run(&format!(
        "GRTOBJAUT AREA *DTAARA USER({bob} *PUBLIC) AUT(*EXCLUDE)"
    ))
    .await
    .unwrap();
    sec.run("GRTOBJAUT AREA *DTAARA USER(*PUBLIC) AUT(*CHANGE)")
        .await
        .unwrap();
    assert!(denied(b.run("DSPDTAARA AREA").await));
    // Only the owner (or *ALL, or *ALLOBJ) manages authority.
    assert!(denied(
        b.run(&format!("GRTOBJAUT AREA *DTAARA USER({bob}) AUT(*ALL)"))
            .await
    ));
    // RVKOBJAUT: the private authority goes; *PUBLIC becomes *EXCLUDE.
    sec.run(&format!("RVKOBJAUT AREA *DTAARA USER({bob})"))
        .await
        .unwrap();
    b.run("DSPDTAARA AREA").await.unwrap();
    sec.run("RVKOBJAUT AREA *DTAARA USER(*PUBLIC) AUT(*ALL)")
        .await
        .unwrap();
    assert!(denied(b.run("DSPDTAARA AREA").await));
    assert!(denied(b.run("DLTDTAARA AREA").await));
    // The creator owns: BOB's own data area is his to manage and delete.
    b.run("CRTDTAARA BOBS *DEC").await.unwrap();
    b.run("GRTOBJAUT BOBS *DTAARA USER(*PUBLIC) AUT(*EXCLUDE)")
        .await
        .unwrap();
    sec.run("DSPDTAARA BOBS").await.unwrap(); // *ALLOBJ
    b.run("DLTDTAARA BOBS").await.unwrap();
    // CHGOBJOWN hands AREA to BOB.
    sec.run(&format!("CHGOBJOWN AREA *DTAARA NEWOWN({bob})"))
        .await
        .unwrap();
    b.run("DLTDTAARA AREA").await.unwrap();

    // Files through the bridge: open for update needs *CHANGE, input *USE.
    let id = jobs::start_interactive(
        &client,
        "AUTDEV",
        &bob,
        Some(&lib),
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    // RPG programs check the files they open: update needs *CHANGE, input
    // *USE; WRITE needs *CHANGE (CPF9802 otherwise).
    let upd = "     FCUSTMAST  UF   E           K DISK\n     C     1             CHAIN     CUSTMAST\n     C                   SETON                                        LR\n";
    let inp = "     FCUSTMAST  IF   E           K DISK\n     C     1             CHAIN     CUSTMAST\n     C                   IF        %FOUND\n     C     CUSNAM        DSPLY\n     C                   ENDIF\n     C                   SETON                                        LR\n";
    let add = "     FCUSTMAST  IF A E           K DISK\n     C                   EVAL      CUSNO = 77\n     C                   WRITE     CUSTREC\n     C                   SETON                                        LR\n";
    let job = Some(id.to_string());
    let denied_rpg = |r: &as4tainr_core::rpg::CallResult| {
        r.error
            .as_ref()
            .is_some_and(|e| format!("{e:#}").contains("CPF9802"))
    };
    sec.run(&format!(
        "GRTOBJAUT {lib}/CUSTMAST *FILE USER(*PUBLIC) AUT(*USE)"
    ))
    .await
    .unwrap();
    let r = run_rpg(&url, &lib, &bob, "UPD", upd, None, job.clone()).await;
    assert!(denied_rpg(&r), "{:?}", r.error);
    let r = run_rpg(&url, &lib, &bob, "INP", inp, None, job.clone()).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["Acme Tooling"]);
    let r = run_rpg(&url, &lib, &bob, "ADD", add, None, job.clone()).await;
    assert!(denied_rpg(&r), "{:?}", r.error);
    sec.run(&format!(
        "GRTOBJAUT {lib}/CUSTMAST *FILE USER({bob}) AUT(*CHANGE)"
    ))
    .await
    .unwrap();
    let r = run_rpg(&url, &lib, &bob, "UPD", upd, None, job.clone()).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    sec.run(&format!(
        "GRTOBJAUT {lib}/CUSTMAST *FILE USER({bob}) AUT(*EXCLUDE)"
    ))
    .await
    .unwrap();
    let r = run_rpg(&url, &lib, &bob, "INP", inp, None, job.clone()).await;
    assert!(denied_rpg(&r), "{:?}", r.error);
    jobs::end(&client, &id, true).await.unwrap();
    users::delete(&client, &bob).await.unwrap();
}

/// Menus and the user profile's initial program and menu. Expected values
/// from IBM's CL reference: CRTUSRPRF INLPGM defaults to *NONE and INLMNU
/// to MAIN; GO needs *USE authority to the menu; CRTMNU PGM(*MENU) is a
/// program with the menu's name; CALL QCMD is the command entry display.
#[tokio::test]
async fn menus_and_initial_menu() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::{menus, users};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let user = format!("M{}", &lib[1..]);
    let _ = users::delete(&client, &user).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let mut sec = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: Some(tx.clone()),
        id: None,
        calls: Default::default(),
    };

    sec.run(&format!("CRTUSRPRF {user} PASSWORD(*NONE)"))
        .await
        .unwrap();
    let p = users::get(&client, &user).await.unwrap().unwrap();
    assert_eq!((p.inlpgm.as_str(), p.inlmnu.as_str()), ("*NONE", "MAIN"));
    sec.run(&format!(
        "CHGUSRPRF {user} INLPGM({lib}/START) INLMNU(*SIGNOFF)"
    ))
    .await
    .unwrap();
    let p = users::get(&client, &user).await.unwrap().unwrap();
    assert_eq!(p.inlpgm, format!("{lib}/START"));
    assert_eq!(p.inlmnu, "*SIGNOFF");
    assert_eq!(p.curlib, "*CRTDFT", "*SAME leaves the rest as it was");

    // GO MAIN: a system menu, from any library list.
    match sec.run("GO MAIN").await.unwrap() {
        Outcome::Menu(menus::Menu::Builtin(m)) => assert_eq!(m.name, "MAIN"),
        _ => panic!("GO MAIN should show the main menu"),
    }
    assert!(matches!(
        sec.run("CALL QCMD").await.unwrap(),
        Outcome::CommandEntry
    ));

    // A *PGM menu.
    let e = sec.run("CRTMNU ORDERS").await.unwrap_err().to_string();
    assert!(e.contains("not supported"), "{e}");
    sec.run("CRTMNU ORDERS TYPE(*PGM)").await.unwrap();
    match sec.run("GO ORDERS").await.unwrap() {
        Outcome::Menu(menus::Menu::Program { program, .. }) => {
            assert_eq!(program, format!("{lib}/ORDERS"))
        }
        _ => panic!("GO of a *PGM menu"),
    }
    // GO needs *USE to the menu.
    sec.run(&format!(
        "GRTOBJAUT ORDERS *MENU USER({user}) AUT(*EXCLUDE)"
    ))
    .await
    .unwrap();
    let mut u = Job {
        client: &client,
        env: &env,
        user: user.clone(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: Some(tx.clone()),
        id: None,
        calls: Default::default(),
    };
    let e = u.run("GO ORDERS").await.unwrap_err().to_string();
    assert!(e.starts_with("CPF9802"), "{e}");
    // A batch job cannot show a menu.
    let mut batch = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    assert!(batch.run("GO MAIN").await.is_err());
    assert!(batch.run("CALL QCMD").await.is_err());

    sec.run("DLTMNU ORDERS").await.unwrap();
    let e = sec.run("GO ORDERS").await.unwrap_err().to_string();
    assert!(e.contains("not found"), "{e}");
    users::delete(&client, &user).await.unwrap();
}

/// `as4tainr import` of the contacts sample: source files and members, then
/// the objects, built with the create commands. Expected values from IBM's
/// CL reference: CRTPF/CRTLF/CRTDSPF take SRCFILE(QDDSSRC) and
/// SRCMBR(*FILE) by default, CRTBNDRPG SRCFILE(QRPGLESRC) and CRTBNDCL
/// SRCFILE(QCLSRC) with SRCMBR(*PGM); the source type comes from the
/// member (PF, DSPF, RPGLE, CLLE); TEXT(*SRCMBRTXT) takes the member's
/// text. Records: the sample's CSV has 5 rows.
#[tokio::test]
async fn import_an_application() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::{import, source};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/apps/contacts");
    let report = import::import(&mut job, &dir, &lib).await.unwrap();
    assert_eq!(report.failed, 0, "{:#?}", report.lines);
    assert!(report
        .lines
        .iter()
        .any(|l| l.contains("CONTACT: 5 records loaded")));

    // The members, with their types and texts (TOBi names).
    let rpg = source::members(&client, &lib, "QRPGLESRC").await.unwrap();
    assert_eq!(
        rpg,
        vec![
            (
                "CONTCNT".to_string(),
                "RPGLE".to_string(),
                "Count contacts".to_string()
            ),
            (
                "CONTLST".to_string(),
                "RPGLE".to_string(),
                "List contacts".to_string()
            ),
        ]
    );
    let dds = source::members(&client, &lib, "QDDSSRC").await.unwrap();
    assert_eq!(
        dds.iter().map(|m| m.1.as_str()).collect::<Vec<_>>(),
        ["PF", "DSPF"]
    );

    // The programs: attribute by create command, text from the member.
    let p = catalog::find_program(&client, std::slice::from_ref(&lib), "CONTCNT")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.attribute, "RPGLE");
    let c = catalog::find_program(&client, std::slice::from_ref(&lib), "CONTACTS")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.attribute, "CLLE");
    // The CL INCLUDE is part of the program.
    assert!(c.source.contains("DCL VAR(&APP)"), "{}", c.source);

    // Create commands with their defaults, from a job.
    job.curlib = Some(lib.clone());
    let msgs = |o: Outcome| match o {
        Outcome::Messages(m) => m.join("\n"),
        _ => String::new(),
    };
    let m = msgs(job.run("CRTBNDRPG PGM(CONTCNT)").await.unwrap());
    // CRTBNDRPG's completion message is RNS9304 (observed on IBM i 7.5,
    // tools/golden/rpg-sql.txt).
    assert_eq!(
        m,
        format!("RNS9304: Program CONTCNT was created in library {lib}; highest severity 00.")
    );
    let e = job
        .run("CRTBNDRPG PGM(CONTCNT) REPLACE(*NO)")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("already exists"), "{e}");
    // SRCMBR(*FILE): the member named like the file.
    let e = job
        .run("CRTPF FILE(CONTACT)")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("CPF7302"), "{e}");
    let e = job.run("CRTPF FILE(NOSUCH)").await.unwrap_err().to_string();
    assert!(e.starts_with("CPF9815"), "{e}");
    // DSPFD *MBRLIST and DSPPFM of a source member.
    let Outcome::Display(d) = job.run("DSPFD QRPGLESRC TYPE(*MBRLIST)").await.unwrap() else {
        panic!()
    };
    assert_eq!(d.rows.len(), 2);
    let Outcome::Display(d) = job.run("DSPPFM QDDSSRC MBR(CONTACT)").await.unwrap() else {
        panic!()
    };
    // The records as bytes: SRCSEQ 1.00 and SRCDAT 0 as zoned digits,
    // then the line (PUB400 2026-10-28, tools/golden/screens6.txt
    // 07_dsppfm; this test expected the line number "   1.00 " before
    // the line, a layout of as4tainr's own).
    assert!(d.rows[0].starts_with("000100000000"), "{:?}", d.rows);
    assert!(d.rows.iter().any(|r| r.contains("K CTID")));

    // A field named like a PostgreSQL system column (CTID) works in SQL.
    let rows = client
        .query(
            &format!(
                "SELECT count(*) FROM {}.{}",
                catalog::ident(&lib),
                catalog::ident("CONTACT")
            ),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get::<_, i64>(0), 5);
}

/// The native RPG engine runs an imported program in its own session:
/// READ until end of file (EOF indicator in positions 75-76), CHAIN found
/// and not found (positions 71-72), a /INCLUDE stream file, DSPLY. The
/// expected output follows from the sample's data (5 contacts; numbers
/// 2 and 4 exist, 9 does not) and the ILE RPG reference semantics.
#[tokio::test(flavor = "multi_thread")]
async fn native_rpg_engine_runs_a_program() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::{import, rpg};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../samples/apps/contacts");
    import::import(&mut job, &dir, &lib).await.unwrap();
    let p = catalog::find_program(&client, std::slice::from_ref(&lib), "CONTCNT")
        .await
        .unwrap()
        .unwrap();
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "CONTCNT".into(),
        p.source,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let trimmed: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_end().to_string())
        .collect();
    assert_eq!(trimmed, ["5", "Grace Hopper", "Barbara Liskov", "2", "1"]);
    // A compile error is reported with its line, before anything runs.
    let e = rpg::compile(
        &client,
        std::slice::from_ref(&lib),
        "BAD",
        "**FREE\nif x = 1;\n",
    )
    .await
    .err()
    .unwrap();
    assert!(e.to_string().contains("IF has no end"), "{e:#}");

    // An interactive program on the native engine (milestone R2): CONTLST
    // fills a load-all subfile and shows it until F3 sets *INKC (ILE RPG
    // Reference: function key indicators; DDS: SFL, SFLCTL). The display
    // here is a stand-in that checks the frame and presses F3.
    let p = catalog::find_program(&client, std::slice::from_ref(&lib), "CONTLST")
        .await
        .unwrap()
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<as4tainr_core::runtime::DisplayRequest>(1);
    let operator = tokio::spawn(async move {
        let mut shown = Vec::new();
        while let Some(req) = rx.recv().await {
            let sfl = req.frame.subfile.clone().expect("CTL01 controls a subfile");
            shown.push((
                req.frame.layers.last().unwrap().format.clone(),
                sfl.rows.len(),
                sfl.show,
            ));
            let _ = req.reply.send(Ok(as4tainr_core::dspf::Response {
                aid: "F3".into(),
                values: Default::default(),
                indicators: Default::default(),
                changed: Default::default(),
            }));
        }
        shown
    });
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        Some(tx),
        None,
        "CONTLST".into(),
        p.source,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let shown = operator.await.unwrap();
    // One EXFMT: the control record over all five contacts (the same
    // count CONTCNT reads), with SFLDSP on (*IN31); F3 ends the DOU.
    assert_eq!(shown, [("CTL01".to_string(), 5, true)]);

    // The RPG cycle with a primary file and cycle output (milestone R3).
    // ILE RPG Reference, "Program Cycle": the first cycle prints the 1P
    // heading before any record is read (an unconditioned detail line
    // would print then too, so it is conditioned N1P); each record read
    // from the primary file is printed at detail time; end of file sets
    // LR, and total output conditioned by LR prints last. Without a job,
    // the lines come back as printed.
    let cycle = "     FCONTACT   IP   E           K DISK\n     FQSYSPRT   O    F  132        PRINTER\n     OQSYSPRT   H    1P                     1\n     O                                            5 'NAMES'\n     O          D   N1P                     1\n     O                       CTID          Z      5\n     O                       CTNAME              37\n     O          T    LR                     1\n     O                                            3 'END'\n";
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "CYCLE".into(),
        cycle.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.printed,
        [
            "NAMES",
            "    1  Ada Lovelace",
            "    2  Grace Hopper",
            "    3  Edsger Dijkstra",
            "    4  Barbara Liskov",
            "    5  Ken Thompson",
            "END"
        ]
    );

    // Milestone R4: procedures and calls (ILE RPG Reference: "Subprocedures",
    // "Passing Parameters", CALL/PARM, EXTPGM, ctl-opt MAIN, STATIC).
    // Expected values follow from those rules: CONST and VALUE pass copies,
    // other parameters are passed by reference (the caller sees changes),
    // STATIC locals keep their value between calls while automatic ones
    // start over, a program called with CALL or a prototype gets its
    // parameters by reference, and error indicator 73-74 on CALL catches a
    // program that cannot be called.
    let mut owner = as4tainr_core::runtime::connect_db(&url).await.unwrap();
    for (name, source) in [
        ("CALLEE", "     C     *ENTRY        PLIST\n     C                   PARM                    NAME             10\n     C                   PARM                    COUNT             5 0\n     C                   EVAL      NAME = 'Changed'\n     C                   EVAL      COUNT = COUNT + 1\n     C                   RETURN\n"),
        ("MAINPGM", "**FREE\nctl-opt main(go);\ndcl-proc go;\n  dcl-pi *n extpgm('MAINPGM');\n    who char(10);\n  end-pi;\n  dsply ('Hi ' + %trim(who));\n  who = 'Back';\nend-proc;\n"),
    ] {
        catalog::create_program(&mut owner, &lib, name, "RPGLE", "", source, true)
            .await
            .unwrap();
    }
    for (name, source, want) in [
        ("PROCS", "**FREE\ndcl-s total packed(9:2);\ndcl-ds cust qualified;\n  id zoned(5);\n  name char(20);\nend-ds;\ndcl-pr addTo;\n  amount packed(9:2) const;\n  acc packed(9:2);\nend-pr;\ntotal = 10;\naddTo(5.25: total);\ndsply %char(total);\ndsply %char(double(21));\ndsply greet('Ada');\nfill(cust);\ndsply cust.name;\ndsply %char(fact(5));\ndsply %char(counter());\ndsply %char(counter());\n*inlr = *on;\n\ndcl-proc addTo;\n  dcl-pi *n;\n    amount packed(9:2) const;\n    acc packed(9:2);\n  end-pi;\n  acc += amount;\nend-proc;\n\ndcl-proc double;\n  dcl-pi *n int(10);\n    n int(10) value;\n  end-pi;\n  return n * 2;\nend-proc;\n\ndcl-proc greet;\n  dcl-pi *n varchar(30);\n    who char(10) const;\n  end-pi;\n  return 'Hello, ' + %trim(who);\nend-proc;\n\ndcl-proc fill;\n  dcl-pi *n;\n    c likeds(cust);\n  end-pi;\n  c.name = 'Filled';\nend-proc;\n\ndcl-proc fact;\n  dcl-pi *n packed(15:0);\n    n packed(15:0) value;\n  end-pi;\n  if n <= 1;\n    return 1;\n  endif;\n  return n * fact(n - 1);\nend-proc;\n\ndcl-proc counter;\n  dcl-pi *n int(10) end-pi;\n  dcl-s count int(10) static;\n  dcl-s temp int(10);\n  count += 1;\n  temp += 1;\n  return count * 10 + temp;\nend-proc;\n", vec!["15.25", "42", "Hello, Ada", "Filled", "120", "11", "21"]),
        ("CALLER", "     D N               S             10    INZ('Orig')\n     D C               S              5  0 INZ(41)\n     C                   CALL      'CALLEE'\n     C                   PARM                    N\n     C                   PARM                    C\n     C     N             DSPLY\n     C     C             DSPLY\n     C                   CALL      'NOSUCH'                             99\n     C   99'missing'     DSPLY\n     C                   SETON                                        LR\n", vec!["Changed", "42", "missing"]),
        // Nested data structures (ILE RPG Reference, LIKEDS on a subfield;
        // "Qualified data structures"): list.users(i).name is element i of
        // the users subfield array; a LIKEDS copy of the whole structure
        // has the same nested subfields; ds(i).x indexes a DS array; a
        // nested element passed by reference is changed by the callee;
        // %SIZE of an array is the size of one element (10 + 3 bytes).
        ("NESTED", "**FREE\ndcl-ds user_t qualified template;\n  name char(10);\n  days zoned(3:0);\nend-ds;\ndcl-ds list qualified;\n  count int(10);\n  users likeds(user_t) dim(3);\nend-ds;\ndcl-ds copy likeds(list);\ndcl-ds pts qualified dim(2);\n  x zoned(3:0);\n  y zoned(3:0);\nend-ds;\ndcl-s i int(10);\nlist.count = 2;\nlist.users(1).name = 'ADA';\nlist.users(1).days = 5;\nlist.users(2).name = 'GRACE';\nlist.users(2).days = 12;\nfor i = 1 to list.count;\n  dsply (%trim(list.users(i).name) + ' ' + %char(list.users(i).days));\nendfor;\ncopy = list;\ndsply copy.users(2).name;\nshow(list.users(2));\ndsply %char(list.users(2).days);\npts(2).x = 7;\npts(1).y = 3;\ndsply %char(pts(2).x + pts(1).y);\ndsply %char(%size(list.users));\n*inlr = *on;\ndcl-proc show;\n  dcl-pi *n;\n    u likeds(user_t);\n  end-pi;\n  dsply ('show ' + %trim(u.name));\n  u.days = 99;\nend-proc;\n", vec!["ADA 5", "GRACE 12", "GRACE", "show GRACE", "99", "10", "13"]),
        // A procedure's local data structure can have a subfield LIKEDS a
        // global template (ILE RPG Reference, LIKEDS: the template may be
        // any data structure in scope); the subfield gets the template's
        // layout and its INZ values.
        ("LOCNEST", "**FREE\ndcl-ds addr_t qualified template;\n  city char(10) inz('Nowhere');\n  zip zoned(5:0);\nend-ds;\ndcl-s keep char(10);\nlocal();\ndsply keep;\n*inlr = *on;\ndcl-proc local;\n  dcl-ds cust qualified;\n    id int(10);\n    home likeds(addr_t);\n  end-ds;\n  dsply cust.home.city;\n  cust.home.city = 'Reno';\n  keep = cust.home.city;\nend-proc;\n", vec!["Nowhere", "Reno"]),
        // %SIZE and %ELEM in definition keywords are resolved when the
        // program is compiled (ILE RPG Reference, %SIZE, %ELEM). Sizes per
        // the reference's data types: int(10) is 4 bytes, packed(7:2) is
        // 4, char(16) is 16, so the structure is 24 bytes.
        ("CONSTBIF", "**FREE\ndcl-ds err qualified;\n  prov int(10);\n  amt packed(7:2);\n  data char(16);\nend-ds;\ndcl-s tbl char(3) dim(12);\ndcl-s a int(10) inz(%size(err.data));\ndcl-s b int(10) inz(%size(err));\ndcl-s c int(10) inz(%elem(tbl) * 2 + 1);\ndsply %char(a);\ndsply %char(b);\ndsply %char(c);\n*inlr = *on;\n", vec!["16", "24", "25"]),
        // ON-EXIT runs when the procedure returns; its indicator is off
        // for a normal end.
        ("ONEXIT", "**FREE\ndsply %char(t());\n*inlr = *on;\ndcl-proc t;\n  dcl-pi *n int(10) end-pi;\n  dcl-s bad ind inz(*on);\n  return 1;\n  on-exit bad;\n    if not bad;\n      dsply 'clean';\n    endif;\nend-proc;\n", vec!["clean", "1"]),
        // LIKE of a data structure (ILE RPG Reference, LIKE: a data
        // structure name gives a character item of the structure's
        // length) as a procedure's return value, the company_system
        // corpus's getDeptDetail: ind 1 + varchar(20) 22 = 23 bytes,
        // assigned to a LIKEDS structure.
        ("LIKEDSRET", "**FREE\ndcl-ds dept_t qualified template;\n  found ind;\n  name varchar(20);\nend-ds;\ndcl-pr getDept like(dept_t);\n  no char(3) const;\nend-pr;\ndcl-ds info likeds(dept_t);\ninfo = getDept('A00');\nif info.found;\n  dsply info.name;\nendif;\ndsply %char(%size(info));\n*inlr = *on;\ndcl-proc getDept;\n  dcl-pi *n like(dept_t);\n    no char(3) const;\n  end-pi;\n  dcl-ds d likeds(dept_t);\n  d.found = *on;\n  d.name = 'Dept ' + no;\n  return d;\nend-proc;\n", vec!["Dept A00", "23"]),
        ("CALLER2", "**FREE\ndcl-pr callee extpgm('CALLEE');\n  name char(10);\n  count packed(5:0);\nend-pr;\ndcl-pr mainpgm extpgm('MAINPGM');\n  who char(10);\nend-pr;\ndcl-s n char(10) inz('Orig');\ndcl-s c packed(5:0) inz(41);\ndcl-s w char(10) inz('Ada');\ncallee(n: c);\ndsply n;\ndsply %char(c);\nmainpgm(w);\ndsply w;\n*inlr = *on;\n", vec!["Changed", "42", "Hi Ada", "Back"]),
    ] {
        let r = rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            source.into(),
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{name}: {:?}", r.error);
        let got: Vec<String> = r.messages.iter().map(|m| m.trim_end().to_string()).collect();
        assert_eq!(got, want, "{name}");
    }

    // System programs and APIs called from RPG: QCMDEXC runs a CL command
    // in the job (the second CRTLIB fails with CPF2111, which MONITOR
    // catches); QDCXLATE with QTCPASC turns EBCDIC 'AB' (X'C1C2', CCSID 37)
    // into ASCII X'4142' and QTCPEBC back; Qp0zLprintf formats %s and %d.
    // Without a job, the printf line comes back with the program's output.
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS \"RPGLIB1\" CASCADE; DELETE FROM as4tainr.objects WHERE library='RPGLIB1' OR name='RPGLIB1';",
        )
        .await
        .unwrap();
    let r = rpg::call(
        &url,
        vec![lib.clone(), "QGPL".into()],
        "QSECOFR".into(),
        None,
        None,
        Some(rpg::ClContext {
            env: Environment {
                database_url: url.clone(),
            },
            curlib: None,
            user_libl: vec![lib.clone()],
            overrides: vec![],
        }),
        "SYSAPI".into(),
        "**FREE\ndcl-pr qcmdexc extpgm('QCMDEXC');\n  cmd char(3000) const;\n  len packed(15:5) const;\nend-pr;\ndcl-pr xlate extpgm('QDCXLATE');\n  len packed(5:0) const;\n  data char(10);\n  table char(10) const;\nend-pr;\ndcl-pr printf int(10) extproc('Qp0zLprintf');\n  fmt pointer value options(*string);\n  a1 pointer value options(*string:*nopass);\n  a2 int(10) value options(*nopass);\nend-pr;\ndcl-s d char(2) inz('AB');\nqcmdexc('CRTLIB LIB(RPGLIB1)': 19);\nmonitor;\n  qcmdexc('CRTLIB LIB(RPGLIB1)': 19);\non-error;\n  dsply 'CRTLIB failed';\nendmon;\nxlate(2: d: 'QTCPASC');\nif d = x'4142';\n  dsply 'ascii';\nendif;\nxlate(2: d: 'QTCPEBC');\ndsply d;\nprintf('%s has %d items' + x'25': 'cart': 3);\n*inlr = *on;\n".into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let got: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_end().to_string())
        .collect();
    assert_eq!(
        got,
        [
            // CPC2102: CRTLIB's completion message (PUB400, IBM i 7.5).
            "CPC2102: Library RPGLIB1 was created.",
            "CRTLIB failed",
            "ascii",
            "AB",
            "cart has 3 items"
        ]
    );
}

/// Milestone R6: embedded SQL in RPG (SQLRPGLE). Expected values follow
/// from the sample data (samples/data/CUSTMAST.csv) and the Db2 for i SQL
/// Reference / Embedded SQL Programming rules: SELECT INTO with no row sets
/// SQLCODE +100 and SQLSTATE '02000'; host variables in a cursor's SELECT
/// are read when the cursor is OPENed; a host structure receives the
/// columns into its subfields in order; SQLERRD(3) is the number of rows
/// changed; an unknown column is SQLCODE -206; a parameter marker of a
/// prepared statement takes the value from USING; a null value sets the
/// indicator variable to -1. %CHAR of a zero packed(9:2) is '.00' (ILE RPG
/// Reference, %CHAR). Fixed-form statements go between C/EXEC SQL and
/// C/END-EXEC with C+ continuation lines.
#[tokio::test]
async fn rpg_embedded_sql() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    for (name, source, want) in [
        (
            "SQL1",
            "**FREE\ndcl-s name char(30);\ndcl-s bal packed(9:2);\ndcl-s cnt int(10);\ndcl-s stmt varchar(200);\ndcl-s nullind int(5);\ndcl-ds cust qualified;\n  no zoned(5:0);\n  nam char(30);\nend-ds;\nexec sql set option commit = *none;\nexec sql select cusnam, cusbal into :name, :bal from custmast where cusno = 3;\ndsply (%trim(name) + ' ' + %char(bal));\nexec sql select count(*) into :cnt from custmast;\ndsply %char(cnt);\nexec sql select cusnam into :name from custmast where cusno = 99;\ndsply %char(sqlcode);\nexec sql declare c1 cursor for\n  select cusno, cusnam from custmast\n   where cusbal > :bal order by cusno;\nbal = 100;\nexec sql open c1;\ndow 1 = 1;\n  exec sql fetch next from c1 into :cust;\n  if sqlcode <> 0;\n    leave;\n  endif;\n  dsply (%char(cust.no) + ' ' + %trim(cust.nam));\nenddo;\nexec sql close c1;\nexec sql update custmast set cuscty = 'Ames' where cusno = 1;\ndsply %char(sqlerrd(3));\nexec sql update custmast set cuscty = 'X' where cusno = 99;\ndsply sqlstate;\nexec sql select nosuchcol into :name from custmast;\ndsply %char(sqlcod);\nstmt = 'select count(*) from custmast where cusbal < ?';\nexec sql prepare s1 from :stmt;\nexec sql declare c2 cursor for s1;\nbal = 0;\nexec sql open c2 using :bal;\nexec sql fetch c2 into :cnt;\ndsply %char(cnt);\nexec sql close c2;\nexec sql values (cast(null as integer)) into :cnt :nullind;\ndsply %char(nullind);\n*inlr = *on;\n",
            vec![
                "Bluebird Supply .00",
                "5",
                "100",
                "1 Acme Tooling",
                "2 Midwest Fasteners",
                "4 Prairie Freight",
                "1",
                "02000",
                "-206",
                "1",
                "-1",
            ],
        ),
        ("SQL2", "     D NAME            S             30\n     D NO              S              5  0 INZ(2)\n     C/EXEC SQL\n     C+ SELECT CUSNAM INTO :NAME\n     C+   FROM CUSTMAST WHERE CUSNO = :NO\n     C/END-EXEC\n     C     NAME          DSPLY\n     C     SQLCOD        DSPLY\n     C                   SETON                                        LR\n", vec!["Midwest Fasteners", "0"]),
    ] {
        let r = rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            source.into(),
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{name}: {:?}", r.error);
        let got: Vec<String> = r.messages.iter().map(|m| m.trim_end().to_string()).collect();
        assert_eq!(got, want, "{name}");
    }

    // Data definition in a program: the table is created, used and
    // dropped; dropping it again is SQLCODE -204 (Db2 for i SQL messages,
    // SQL0204: object not found).
    let src = format!(
        "**FREE\ndcl-s n int(10);\nexec sql create table {lib}.t3 (a int not null);\ndsply %char(sqlcode);\nexec sql insert into t3 values (5);\nexec sql select a into :n from t3;\ndsply %char(n);\nexec sql drop table {lib}.t3;\ndsply %char(sqlcode);\nexec sql drop table {lib}.t3;\ndsply %char(sqlcode);\n*inlr = *on;\n"
    );
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "SQL3".into(),
        src,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let got: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_end().to_string())
        .collect();
    assert_eq!(got, ["0", "5", "0", "-204"]);
}

/// R6: SQL data definition (Db2 for i SQL Reference: CREATE TABLE, CREATE
/// INDEX, DROP, LABEL ON; RUNSQLSTM). Expected values from those rules:
/// a table is a physical file whose record format is named by RCDFMT; an
/// index is a keyed logical file over it (DESC keys read backwards); a
/// column without NOT NULL is null-capable; WITH DEFAULT gives the type's
/// default; LABEL ON TABLE sets the object text and LABEL ON COLUMN ...
/// TEXT IS the field text; PRIMARY KEY rejects a duplicate (SQL0803) and
/// CHECK a row that fails it (SQL0545 from SQL, CPF502F from a WRITE);
/// DROP TABLE drops the indexes over it; DLTF refuses a physical file
/// that a logical file is built over (CPF3219, observed on IBM i 7.5); RUNSQLSTM with ERRLVL(10)
/// stops at the first failing statement (SQL9010). An RPG program sees
/// SQL INTEGER as binary 9,0, DATE/TIME/TIMESTAMP in *ISO form
/// (YYYY-MM-DD, HH.MM.SS, YYYY-MM-DD-HH.MM.SS.ffffff) and VARCHAR as a
/// varying field (ILE RPG Reference, externally described files).
#[tokio::test]
async fn sql_tables_indexes_and_runsqlstm() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::{dds::DataType, rpg, source};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = "
        -- People; the index reads them by name, last first.
        CREATE TABLE PERSON (
          ID INTEGER NOT NULL PRIMARY KEY,
          NAME VARCHAR(30) NOT NULL WITH DEFAULT,
          BAL DECIMAL(9, 2) NOT NULL DEFAULT 0,
          DOB DATE,
          AT TIME NOT NULL DEFAULT '09.00.00',
          JOINED TIMESTAMP NOT NULL DEFAULT '2026-01-01-08.30.00.000000',
          CODE CHAR(1) NOT NULL DEFAULT 'Y' CHECK (CODE IN ('Y', 'N'))
        ) RCDFMT PERSONR;
        LABEL ON TABLE PERSON IS 'People';
        LABEL ON COLUMN PERSON (NAME IS 'Name', NAME TEXT IS 'Full name');
        CREATE INDEX PERSONIX ON PERSON (NAME DESC);
        INSERT INTO PERSON (ID, NAME, BAL, DOB, AT) VALUES (1, 'Ada', 10.50, '1815-12-10', '09:15:00');
        INSERT INTO PERSON (ID, NAME, DOB) VALUES (2, 'Grace', '1906-12-09');
    ";
    let lines = job
        .run_sql_script(script, "test script", 10, None, true)
        .await
        .unwrap();
    assert_eq!(
        lines.last().unwrap(),
        "6 statements run from test script; 0 with errors."
    );

    let f = catalog::resolve_file(&client, std::slice::from_ref(&lib), "PERSON")
        .await
        .unwrap();
    assert_eq!(f.file.format, "PERSONR");
    let field = |n: &str| f.file.fields.iter().find(|x| x.name == n).unwrap().clone();
    assert_eq!(
        (field("ID").data_type, field("ID").length),
        (DataType::Binary, 9)
    );
    assert!(field("NAME").varlen);
    assert_eq!(field("NAME").text, "Full name");
    assert!(field("DOB").null_capable && !field("BAL").null_capable);
    let text: String = client
        .query_one(
            "SELECT text FROM as4tainr.objects WHERE library=$1 AND name='PERSON'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(text, "People");

    // Constraints, from SQL.
    let sql = |s: &'static str| {
        let (url, lib) = (url.clone(), lib.clone());
        async move {
            as4tainr_core::sql::run(&runtime::connect_db(&url).await.unwrap(), &url, &[lib], s)
                .await
                .map_err(|e| e.to_string())
        }
    };
    let e = sql("INSERT INTO PERSON (ID, NAME) VALUES (1, 'Again')")
        .await
        .unwrap_err();
    assert!(e.starts_with("SQL0803"), "{e}");
    let e = sql("INSERT INTO PERSON (ID, CODE) VALUES (3, 'X')")
        .await
        .unwrap_err();
    assert!(e.starts_with("SQL0545"), "{e}");

    // RPG: write through the table (a CHECK failure is CPF502F), read
    // through the index.
    let run = |name: &str, src: &str| {
        rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            src.into(),
            vec![],
        )
    };
    let w = run("DDLW", "**FREE\ndcl-f person usage(*output);\nid = 3;\nname = 'Edsger';\nbal = 7.25;\ndob = d'1930-05-11';\nat = t'13.45.00';\njoined = z'2026-02-03-04.05.06.000007';\ncode = 'N';\nwrite personr;\nid = 4;\ncode = 'X';\nmonitor;\n  write personr;\non-error;\n  dsply 'rejected';\nendmon;\n*inlr = *on;\n").await.unwrap();
    assert!(w.error.is_none(), "{:?}", w.error);
    assert_eq!(w.messages, ["rejected"]);
    let r = run("DDLR", "**FREE\ndcl-f personix keyed;\nread personix;\ndow not %eof(personix);\n  dsply (%trim(name) + ' ' + %char(id) + ' ' + %char(bal) + ' ' + %char(dob) + ' ' + %char(at) + ' ' + %char(joined) + ' ' + code);\n  read personix;\nenddo;\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let got: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_end().to_string())
        .collect();
    assert_eq!(
        got,
        [
            "Grace 2 .00 1906-12-09 09.00.00 2026-01-01-08.30.00.000000 Y",
            "Edsger 3 7.25 1930-05-11 13.45.00 2026-02-03-04.05.06.000007 N",
            "Ada 1 10.50 1815-12-10 09.15.00 2026-01-01-08.30.00.000000 Y",
        ]
    );

    // Null values (ILE RPG Reference, "Database Null Value Support" and
    // the ALWNULL keyword): with ALWNULL(*NO), the default, a record with
    // a null field cannot be read (data mapping error: an error on the
    // operation, which the E extender catches); with *INPUTONLY the field
    // has its default value (a date's is 0001-01-01; to be checked against
    // a real system); with *USRCTL, %NULLIND says the field is null, and
    // setting it writes the null value.
    sql("INSERT INTO PERSON (ID, NAME) VALUES (9, 'Nobody')")
        .await
        .unwrap();
    let r = run(
        "NULLNO",
        "**FREE\ndcl-f personix keyed;\nchain ('Nobody') personix;\n*inlr = *on;\n",
    )
    .await
    .unwrap();
    let e = format!("{:?}", r.error.expect("a data mapping error"));
    assert!(e.contains("CPF5035"), "{e}");
    let r = run("NULLE", "**FREE\ndcl-f personix keyed;\nchain(e) ('Nobody') personix;\ndsply %char(%error());\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1"]);
    let r = run("NULLIN", "**FREE\nctl-opt alwnull(*inputonly);\ndcl-f personix keyed;\nchain ('Nobody') personix;\ndsply %char(dob);\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["0001-01-01"]);
    let r = run("NULLUC", "**FREE\nctl-opt alwnull(*usrctl);\ndcl-f personix keyed usage(*update);\nchain ('Nobody') personix;\ndsply %char(%nullind(dob));\nchain ('Ada') personix;\ndsply %char(%nullind(dob));\n%nullind(dob) = *on;\nupdate personr;\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1", "0"]);
    let rows = match sql("SELECT ID FROM PERSON WHERE DOB IS NULL ORDER BY ID")
        .await
        .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    assert_eq!(rows, vec![vec!["1".to_string()], vec!["9".to_string()]]);
    // CRTBNDRPG ALWNULL(*INPUTONLY) has the effect of the keyword.
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "NULLCMD".into(),
            srctype: "RPGLE".into(),
            text: String::new(),
            source: "**FREE\ndcl-f personix keyed;\nchain ('Nobody') personix;\ndsply %char(dob);\n*inlr = *on;\n".into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    job.run(&format!("CRTBNDRPG PGM({lib}/NULLCMD) ALWNULL(*INPUTONLY)"))
        .await
        .unwrap();
    let out = job.run(&format!("CALL PGM({lib}/NULLCMD)")).await.unwrap();
    assert!(format!("{out:?}").contains("0001-01-01"), "{out:?}");

    // DLTF refuses; DROP TABLE takes the index with it.
    let e = job
        .run(&format!("DLTF FILE({lib}/PERSON)"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPF3219"), "{e}");
    source::create_file(&client, &lib, "QSQLSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QSQLSRC".into(),
            name: "DROPS".into(),
            srctype: "SQL".into(),
            text: String::new(),
            source: "DROP TABLE PERSON; DROP TABLE PERSON; CREATE TABLE AFTER (A INT);".into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let e = job
        .run(&format!("RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(DROPS)"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("SQL9010") && e.contains("statement 2"), "{e}");
    for name in ["PERSON", "PERSONIX", "AFTER"] {
        assert!(
            !catalog::object_exists(&client, std::slice::from_ref(&lib), name, "*FILE")
                .await
                .unwrap(),
            "{name} should not exist"
        );
    }
    // ERRLVL(30): the failing statement is reported and the rest runs.
    let out = job
        .run(&format!(
            "RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(DROPS) ERRLVL(30)"
        ))
        .await
        .unwrap();
    let as4tainr_core::commands::Outcome::Messages(m) = out else {
        panic!("{out:?}")
    };
    // No completion message; the listing reports the errors (PUB400
    // 2026-10-27, tools/golden/verify3.txt x_errors: ERRLVL(30) with
    // severity 30 errors ends normally and sends nothing).
    assert!(m.is_empty(), "{m:?}");
    assert!(
        job.calls
            .printed
            .iter()
            .any(|l| l == "30 level severity errors found in source"),
        "{:?}",
        job.calls.printed
    );
    assert!(
        catalog::object_exists(&client, std::slice::from_ref(&lib), "AFTER", "*FILE")
            .await
            .unwrap()
    );
}

/// SQL names longer than system names (Db2 for i SQL Reference, rules for
/// system names): a table or index named COUNTRY_LIST gets the system name
/// COUNT00001 (first five characters and a five-digit number; the next
/// object with that prefix gets COUNT00002); a column COUNTRY_CODE gets
/// COUNT00001 within the table, unless FOR COLUMN names it; FOR SYSTEM
/// NAME names the table. SQL statements use the SQL names (and the table's
/// system name); CL commands and RPG use the system names, and RPG uses
/// the SQL column names with the ALIAS keyword (ILE RPG Reference, ALIAS).
#[tokio::test]
async fn sql_long_names() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let lines = job
        .run_sql_script(
            "CREATE TABLE COUNTRY_LIST (
               COUNTRY_CODE CHAR(2) NOT NULL PRIMARY KEY,
               COUNTRY_NAME VARCHAR(40) NOT NULL WITH DEFAULT,
               POPULATION_COUNT FOR COLUMN POP BIGINT NOT NULL DEFAULT 0);
             CREATE TABLE ORDER_HISTORY FOR SYSTEM NAME ORDHIST (ORDER_NUMBER INT NOT NULL);
             CREATE INDEX COUNTRY_BY_NAME ON COUNTRY_LIST (COUNTRY_NAME DESC);
             LABEL ON COLUMN COUNTRY_LIST.COUNTRY_NAME TEXT IS 'Name of the country';
             INSERT INTO COUNTRY_LIST (COUNTRY_CODE, COUNTRY_NAME, POPULATION_COUNT) VALUES ('NL', 'Netherlands', 17900000);
             INSERT INTO COUNTRY_LIST VALUES ('IS', 'Iceland', 390000);",
            "test",
            10,
            None,
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        lines.last().unwrap(),
        "6 statements run from test; 0 with errors."
    );
    for (sys, exists) in [
        ("COUNT00001", true),
        ("COUNT00002", true),
        ("ORDHIST", true),
        ("ORDER_HIST", false),
    ] {
        assert_eq!(
            catalog::object_exists(&client, std::slice::from_ref(&lib), sys, "*FILE")
                .await
                .unwrap(),
            exists,
            "{sys}"
        );
    }
    let f = catalog::resolve_file(&client, std::slice::from_ref(&lib), "COUNT00001")
        .await
        .unwrap();
    let cols: Vec<(String, Option<String>)> = f
        .file
        .fields
        .iter()
        .map(|x| (x.name.clone(), x.alias.clone()))
        .collect();
    assert_eq!(
        cols,
        vec![
            ("COUNT00001".into(), Some("COUNTRY_CODE".into())),
            ("COUNT00002".into(), Some("COUNTRY_NAME".into())),
            ("POP".into(), Some("POPULATION_COUNT".into())),
        ]
    );
    assert_eq!(f.file.format, "COUNT00001");
    assert_eq!(f.file.fields[1].text, "Name of the country");

    // SQL by either table name.
    for t in ["COUNTRY_LIST", "COUNT00001"] {
        let rows = match as4tainr_core::sql::run(
            &client,
            &url,
            std::slice::from_ref(&lib),
            &format!("SELECT COUNTRY_CODE, POPULATION_COUNT FROM {t} ORDER BY COUNTRY_CODE"),
        )
        .await
        .unwrap()
        {
            as4tainr_core::sql::Outcome::Rows { rows, .. } => rows,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            rows,
            vec![vec!["IS", "390000"], vec!["NL", "17900000"]],
            "{t}"
        );
    }

    // RPG through the index: system names, or SQL names with ALIAS.
    let run = |name: &str, src: String| {
        rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            src,
            vec![],
        )
    };
    let r = run("LONGA", "**FREE\ndcl-f count00002 keyed alias;\nread count00002;\ndow not %eof(count00002);\n  dsply (country_code + ' ' + %trim(country_name) + ' ' + %char(population_count));\n  read count00002;\nenddo;\n*inlr = *on;\n".into()).await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["NL Netherlands 17900000", "IS Iceland 390000"]);
    let r = run("LONGS", "**FREE\ndcl-f count00002 keyed;\nread count00002;\ndsply (count00001 + ' ' + %trim(count00002) + ' ' + %char(pop));\n*inlr = *on;\n".into()).await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["NL Netherlands 17900000"]);

    // DROP by the SQL name takes the index too.
    job.run_sql_script("DROP TABLE COUNTRY_LIST;", "drop", 10, None, true)
        .await
        .unwrap();
    for sys in ["COUNT00001", "COUNT00002"] {
        assert!(
            !catalog::object_exists(&client, std::slice::from_ref(&lib), sys, "*FILE")
                .await
                .unwrap()
        );
    }
}

/// Views, identity columns and ALTER TABLE (Db2 for i SQL Reference):
/// an identity column GENERATED ALWAYS (START WITH 1000 INCREMENT BY 5)
/// numbers rows 1000, 1005, ...; a record written by a program gets the
/// next value too (the database generates it; the program's value is not
/// used). A view is a logical file a program reads; its rows are the
/// query's. ALTER TABLE ADD COLUMN gives existing rows the default; ADD
/// CHECK constrains later changes (SQL0545); SET DATA TYPE keeps the
/// values; DROP COLUMN RESTRICT is refused while a view uses the column
/// (SQL0478) and CASCADE (the default) drops that view; DROP TABLE drops
/// the views over the table.
#[tokio::test]
async fn sql_views_identity_and_alter() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = "
        CREATE TABLE ORDERS (
          ORDER_ID INTEGER GENERATED ALWAYS AS IDENTITY (START WITH 1000 INCREMENT BY 5),
          CUSTOMER_NAME VARCHAR(30) NOT NULL,
          AMOUNT DECIMAL(9, 2) NOT NULL DEFAULT 0,
          PRIMARY KEY (ORDER_ID));
        INSERT INTO ORDERS (CUSTOMER_NAME, AMOUNT) VALUES ('Ada', 10);
        INSERT INTO ORDERS (CUSTOMER_NAME, AMOUNT) VALUES ('Grace', 250);
        CREATE VIEW BIG_ORDERS AS SELECT ORDER_ID, CUSTOMER_NAME, AMOUNT FROM ORDERS WHERE AMOUNT > 100;
        ALTER TABLE ORDERS ADD COLUMN STATUS CHAR(1) NOT NULL DEFAULT 'O';
        ALTER TABLE ORDERS ADD CONSTRAINT STATUS_OK CHECK (STATUS IN ('O', 'C'));
        ALTER TABLE ORDERS ALTER COLUMN AMOUNT SET DATA TYPE DECIMAL(11, 2);
    ";
    let lines = job
        .run_sql_script(script, "test", 10, None, true)
        .await
        .unwrap();
    assert_eq!(
        lines.last().unwrap(),
        "7 statements run from test; 0 with errors."
    );
    let sql = |s: String| {
        let (url, lib) = (url.clone(), lib.clone());
        async move {
            as4tainr_core::sql::run(&runtime::connect_db(&url).await.unwrap(), &url, &[lib], &s)
                .await
                .map_err(|e| e.to_string())
        }
    };
    let rows = |o: as4tainr_core::sql::Outcome| match o {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    let e = sql("UPDATE ORDERS SET STATUS = 'X'".into())
        .await
        .unwrap_err();
    assert!(e.starts_with("SQL0545"), "{e}");

    // A program writes an order (the identity is generated) and reads the
    // view.
    let run = |name: &str, src: &str| {
        rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            src.into(),
            vec![],
        )
    };
    // ORDERS's record format is ORDERS (the table's system name), so the
    // program renames it (ILE RPG: a record format cannot have the file's
    // name); CUSTOMER_NAME's system name is CUSTO00001.
    let r = run("IDW", "**FREE\ndcl-f orders usage(*output) rename(orders:ordr);\norder_id = 1;\ncusto00001 = 'Edsger';\namount = 500;\nstatus = 'C';\nwrite ordr;\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        rows(
            sql(
                "SELECT ORDER_ID, CUSTOMER_NAME, AMOUNT, STATUS FROM ORDERS ORDER BY ORDER_ID"
                    .into()
            )
            .await
            .unwrap()
        ),
        vec![
            vec!["1000", "Ada", "10.00", "O"],
            vec!["1005", "Grace", "250.00", "O"],
            vec!["1010", "Edsger", "500.00", "C"],
        ]
    );
    let r = run("VIEWR", "**FREE\ndcl-f big_orders alias;\nread big_orders;\ndow not %eof(big_orders);\n  dsply (%char(order_id) + ' ' + %trim(customer_name) + ' ' + %char(amount));\n  read big_orders;\nenddo;\n*inlr = *on;\n").await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let mut got = r.messages.clone();
    got.sort();
    assert_eq!(got, ["1005 Grace 250.00", "1010 Edsger 500.00"]);

    // DROP COLUMN: RESTRICT refused while the view uses the column;
    // CASCADE drops the view.
    let e = job
        .run_sql_script(
            "ALTER TABLE ORDERS DROP COLUMN AMOUNT RESTRICT;",
            "t",
            10,
            None,
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("SQL0478"), "{e}");
    job.run_sql_script("ALTER TABLE ORDERS DROP CONSTRAINT STATUS_OK; ALTER TABLE ORDERS DROP COLUMN STATUS RESTRICT; ALTER TABLE ORDERS DROP COLUMN AMOUNT;", "t", 10, None, true)
        .await
        .unwrap();
    assert!(
        !catalog::object_exists(&client, std::slice::from_ref(&lib), "BIG_O00001", "*FILE")
            .await
            .unwrap()
    );
    assert_eq!(
        rows(
            sql("SELECT * FROM ORDERS ORDER BY ORDER_ID".into())
                .await
                .unwrap()
        ),
        vec![
            vec!["1000", "Ada"],
            vec!["1005", "Grace"],
            vec!["1010", "Edsger"]
        ]
    );
    // DROP TABLE takes the views over it.
    job.run_sql_script(
        "CREATE VIEW ADA AS SELECT * FROM ORDERS WHERE CUSTOMER_NAME = 'Ada'; DROP TABLE ORDERS;",
        "t",
        10,
        None,
        true,
    )
    .await
    .unwrap();
    assert!(
        !catalog::object_exists(&client, std::slice::from_ref(&lib), "ADA", "*FILE")
            .await
            .unwrap()
    );
}

/// More data types (Db2 for i SQL Reference, data types and their DDS
/// equivalents; ILE RPG Reference, data types): DOUBLE and REAL are
/// floats (DDS F); BINARY, VARBINARY and FOR BIT DATA are bytes that are
/// not translated (DDS H), shown by SQL as hex digits; GRAPHIC and
/// VARGRAPHIC are UCS-2 (DDS G); CLOB and BLOB are large objects that an
/// RPG program's record leaves out (RNF7575) while SQL reads them. A float
/// shows in its external display form, +1.230000000000000E+002 for an
/// 8-byte float (IBM support, "Floating point numbers"). With
/// EXTBININT(*YES) an INTEGER column is an int(10) in the program and holds
/// 2147483647.
#[tokio::test]
async fn sql_float_binary_graphic_lob() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = "
        CREATE TABLE MEASURE (
          ID INTEGER NOT NULL,
          RATE DOUBLE NOT NULL WITH DEFAULT,
          SCORE REAL NOT NULL WITH DEFAULT,
          TAG BINARY(4) NOT NULL WITH DEFAULT,
          RAW VARCHAR(8) FOR BIT DATA NOT NULL WITH DEFAULT,
          NAME GRAPHIC(5) NOT NULL WITH DEFAULT,
          NOTE VARGRAPHIC(20) NOT NULL WITH DEFAULT,
          DOC CLOB(1M),
          PIC BLOB(10K)) RCDFMT MEASUREF;
        INSERT INTO MEASURE VALUES (2147483647, 123, 0.5, BX'C1C2C3C4', BX'00FF', 'Grüß', 'Zürich', 'a long text', BX'0102');
    ";
    let lines = job
        .run_sql_script(script, "test", 10, None, true)
        .await
        .unwrap();
    assert_eq!(
        lines.last().unwrap(),
        "2 statements run from test; 0 with errors."
    );
    let rows = match as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "SELECT TAG, RAW, NAME, DOC FROM MEASURE",
    )
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    // GRAPHIC(5) is fixed length: four characters and a blank.
    assert_eq!(rows, vec![vec!["C1C2C3C4", "00FF", "Grüß ", "a long text"]]);
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "TYPES".into(),
        "**FREE\nctl-opt extbinint(*yes);\ndcl-f measure usage(*update);\nread measure;\ndsply %char(id);\ndsply %char(%size(id));\ndsply rate;\ndsply score;\ndsply tag;\ndsply %trim(note);\nrate = rate / 4;\nnote = 'Bern';\nupdate measuref;\n*inlr = *on;\n".into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // C1C2C3C4 is 'ABCD' in EBCDIC; the program gets the bytes as they are.
    assert_eq!(
        r.messages,
        [
            "2147483647",
            "4",
            "+1.230000000000000E+002",
            "+5.000000E-001",
            "ABCD",
            "Zürich"
        ]
    );
    let rows = match as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "SELECT RATE, NOTE, DOC, PIC FROM MEASURE",
    )
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => rows,
        other => panic!("{other:?}"),
    };
    // The update changed RATE and NOTE; the large objects are as they were.
    assert_eq!(rows, vec![vec!["30.75", "Bern", "a long text", "0102"]]);
}

/// ILE: modules, service programs, binding directories (IBM ILE Concepts).
/// A NOMAIN module's EXPORTed procedures are bound into a service program
/// (CRTSRVPGM; EXPORT(*SRCFILE) takes the binder language's EXPORT
/// SYMBOLs); a program created with ctl-opt BNDDIR finds them through the
/// binding directory; its calls run them, parameters by reference come
/// back changed. A service program's global storage lives in its
/// activation for the rest of the job (the call count goes 1, then 2), and
/// RCLACTGRP *ELIGIBLE ends it. CRTPGM binds modules into one program. A
/// symbol nobody exports stops the creation (CPD5D02); an export the
/// modules do not have stops CRTSRVPGM (CPD5D03).
#[tokio::test]
async fn ile_modules_service_programs_bnddir() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGLESRC", "QSRVSRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, text: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let members = [
        put("QRPGLESRC", "MATHMOD", "RPGLE", "**FREE\nctl-opt nomain;\ndcl-s calls int(10);\ndcl-proc addTwo export;\n  dcl-pi *n packed(9:2);\n    a packed(9:2) const;\n    b packed(9:2) const;\n  end-pi;\n  calls += 1;\n  return a + b;\nend-proc;\ndcl-proc callCount export;\n  dcl-pi *n int(10) end-pi;\n  return calls;\nend-proc;\ndcl-proc double export;\n  dcl-pi *n;\n    v packed(9:2);\n  end-pi;\n  v = v * 2;\nend-proc;\n"),
        put("QSRVSRC", "MATHSRV", "BND", "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('MATH V1')\n  EXPORT SYMBOL(ADDTWO)\n  EXPORT SYMBOL(CALLCOUNT)\n  EXPORT SYMBOL(DOUBLE)\nENDPGMEXP\n"),
        put("QSRVSRC", "BADSRV", "BND", "STRPGMEXP PGMLVL(*CURRENT)\n  EXPORT SYMBOL(NOSUCH)\nENDPGMEXP\n"),
        put("QRPGLESRC", "USEMATH", "RPGLE", "**FREE\nctl-opt bnddir('MATHDIR');\ndcl-pr addTwo packed(9:2);\n  a packed(9:2) const;\n  b packed(9:2) const;\nend-pr;\ndcl-pr callCount int(10) end-pr;\ndcl-pr double;\n  v packed(9:2);\nend-pr;\ndcl-s x packed(9:2) inz(5);\ndsply %char(addTwo(1.25: 2));\ndouble(x);\ndsply %char(x);\ndsply %char(callCount());\n*inlr = *on;\n"),
        put("QRPGLESRC", "NOBIND", "RPGLE", "**FREE\nctl-opt dftactgrp(*no);\ndcl-pr missing end-pr;\nmissing();\n*inlr = *on;\n"),
        put("QRPGLESRC", "MAIN1", "RPGLE", "**FREE\ndcl-pr addTwo packed(9:2);\n  a packed(9:2) const;\n  b packed(9:2) const;\nend-pr;\ndsply %char(addTwo(40: 2));\n*inlr = *on;\n"),
    ];
    for m in &members {
        source::put_member(&client, m, false).await.unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/MATHMOD)"),
        format!("CRTSRVPGM SRVPGM({lib}/MATHSRV) MODULE({lib}/MATHMOD) SRCFILE({lib}/QSRVSRC)"),
        format!("CRTBNDDIR BNDDIR({lib}/MATHDIR)"),
        format!("ADDBNDDIRE BNDDIR({lib}/MATHDIR) OBJ(({lib}/MATHSRV *SRVPGM))"),
        format!("CRTBNDRPG PGM({lib}/USEMATH)"),
        format!("CRTRPGMOD MODULE({lib}/MAIN1)"),
        format!("CRTPGM PGM({lib}/BOUND1) MODULE({lib}/MAIN1 {lib}/MATHMOD)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let call = format!("CALL PGM({lib}/USEMATH)");
    assert_eq!(
        cl_cmd(&mut job, call.clone()).await.unwrap(),
        ["DSPLY  3.25", "DSPLY  10.00", "DSPLY  1"]
    );
    assert_eq!(
        cl_cmd(&mut job, call.clone()).await.unwrap(),
        ["DSPLY  3.25", "DSPLY  10.00", "DSPLY  2"]
    );
    cl_cmd(&mut job, "RCLACTGRP ACTGRP(*ELIGIBLE)".into())
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, call).await.unwrap(),
        ["DSPLY  3.25", "DSPLY  10.00", "DSPLY  1"]
    );
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/BOUND1)"))
            .await
            .unwrap(),
        ["DSPLY  42.00"]
    );
    let e = cl_cmd(&mut job, format!("CRTBNDRPG PGM({lib}/NOBIND)"))
        .await
        .unwrap_err();
    assert!(e.contains("CPD5D02") && e.contains("MISSING"), "{e}");
    let e = cl_cmd(
        &mut job,
        format!("CRTSRVPGM SRVPGM({lib}/BADSRV) MODULE({lib}/MATHMOD) SRCFILE({lib}/QSRVSRC)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPD5D03"), "{e}");
    // A module keeps the binding directories its control specification
    // names; CRTPGM binds with them (ILE RPG Reference, BNDDIR keyword),
    // as IBM's Company System test binds EMPDET through ctl-opt BNDDIR.
    for c in [
        format!("CRTRPGMOD MODULE({lib}/USEMATH)"),
        format!("CRTPGM PGM({lib}/USEMATH2) MODULE({lib}/USEMATH)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let r = cl_cmd(&mut job, format!("CALL PGM({lib}/USEMATH2)"))
        .await
        .unwrap();
    assert_eq!(r[..2], ["DSPLY  3.25", "DSPLY  10.00"]);
}

/// Run a CL command; its messages, or the error.
async fn cl_cmd(
    job: &mut as4tainr_core::commands::Job<'_>,
    c: String,
) -> Result<Vec<String>, String> {
    use as4tainr_core::commands::Outcome;
    // The command's messages, then what it printed (without a job, a
    // program's printed lines are the command's printed output).
    // What went to the terminal (QSH) comes first, as `system` shows it.
    job.calls.printed.clear();
    job.calls.terminal.clear();
    let r = job.run(&c).await;
    let printed = std::mem::take(&mut job.calls.printed);
    let mut terminal = std::mem::take(&mut job.calls.terminal);
    match r {
        Ok(Outcome::Messages(m)) => {
            terminal.extend(m);
            terminal.extend(printed);
            Ok(terminal)
        }
        Ok(other) => Ok(vec![format!("{other:?}")]),
        Err(e) => Err(format!("{c}: {e:#}")),
    }
}

#[tokio::test]
async fn rpg_fixed_form_string_array_and_bit_operations() {
    // Expected values from the ILE RPG Reference, operation codes: SCAN
    // (positions relative to the base string; an array result gets every
    // occurrence, 0 after), CHECK/CHECKR (first character not in the
    // comparator), SUBST and XLATE (left-adjusted, P pads), CAT (with
    // :n trailing blanks of factor 1 go), LOOKUP (index set to the
    // element found, or 1; a table's related table follows), XFOOT, SQRT
    // (H rounds), MOVEA, SORTA, OCCUR, TESTN, TESTB/BITON, *LIKE DEFINE,
    // AN/OR conditioning lines, CASxx.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = include_str!("rpg/fixedops.rpgle");
    let r = run_rpg(&url, &lib, "QSECOFR", "FIXEDOPS", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "3",
            "3",
            "8",
            "0",
            "8",
            "0",
            "NOTFOUND",
            "3",
            "2",
            "BCDXX",
            "BCD",
            "AABBxC",
            "aaBBxC",
            "ABC DEF",
            "ABC  DEF",
            "3",
            "FOUND CC",
            "1",
            "NO ZZ",
            "2",
            "6.75",
            "3.16",
            "1.41",
            "EF",
            "AB",
            "AB",
            "EF",
            "XY",
            "TWO",
            "2",
            "NUMERIC",
            "LEADING",
            "BITS ON",
            "BITS MIXED",
            "123456.78",
            "OR YES",
            "SR2",
        ]
    );
}

#[tokio::test]
async fn rpg3_crtrpgpgm_and_cvtrpgsrc() {
    // RPG/400 source (RPG/400 Reference positions): an E-spec table with
    // its alternate table loaded from compile-time data (**), an I-spec
    // data structure and named constant, LOKUP, CAT, an array indexed
    // ARR,I, three conditioning indicators on one line, EXCPT with an edit
    // word. CRTRPGPGM converts it as CVTRPGSRC does (ILE RPG Programmer's
    // Guide, conversion aid) and creates the program.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGSRC", "QRPGLESRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let src = include_str!("rpg/rpg3test.rpg");
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGSRC".into(),
            name: "RPG3TEST".into(),
            srctype: "RPG".into(),
            text: "RPG III test".into(),
            source: src.into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    // CRTBNDRPG does not take RPG III source.
    let e = cl_cmd(
        &mut job,
        format!("CRTBNDRPG PGM({lib}/RPG3TEST) SRCFILE({lib}/QRPGSRC)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CRTRPGPGM"), "{e}");
    cl_cmd(&mut job, format!("CRTRPGPGM PGM({lib}/RPG3TEST)"))
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/RPG3TEST)"))
            .await
            .unwrap(),
        // The printed line: the amount edited by the edit word
        // '  ,   .  -' (a minus for a negative value) ends in position 20.
        [
            "DSPLY  MONTH IS OCTOBER",
            "DSPLY  150",
            "DSPLY  ALL3",
            "           1,234.50-      DONE"
        ]
    );
    // CVTRPGSRC: an RPGLE member; it does not replace one that exists.
    let m = cl_cmd(
        &mut job,
        format!("CVTRPGSRC FROMFILE({lib}/QRPGSRC) FROMMBR(RPG3TEST) TOFILE({lib}/QRPGLESRC)"),
    )
    .await
    .unwrap();
    assert!(m.iter().any(|x| x.contains("converted")), "{m:?}");
    let conv = source::find_member(&client, std::slice::from_ref(&lib), "QRPGLESRC", "RPG3TEST")
        .await
        .unwrap();
    assert_eq!(conv.srctype, "RPGLE");
    assert!(
        conv.source.contains("DIM(3) CTDATA PERRCD(3)"),
        "{}",
        conv.source
    );
    assert!(conv.source.contains("**CTDATA TABM"), "{}", conv.source);
    assert!(cl_cmd(
        &mut job,
        format!("CVTRPGSRC FROMFILE({lib}/QRPGSRC) FROMMBR(RPG3TEST) TOFILE({lib}/QRPGLESRC)"),
    )
    .await
    .is_err());
    // The converted member is RPG IV: CRTBNDRPG creates it.
    cl_cmd(
        &mut job,
        format!("CRTBNDRPG PGM({lib}/RPG4TEST) SRCMBR(RPG3TEST)"),
    )
    .await
    .unwrap();
    // And in free form (CVTRPGFREE), the program does the same.
    let m = cl_cmd(
        &mut job,
        format!("CVTRPGFREE FROMFILE({lib}/QRPGLESRC) FROMMBR(RPG3TEST) TOMBR(RPG3FREE)"),
    )
    .await
    .unwrap();
    assert!(m.iter().any(|x| x.contains("converted")), "{m:?}");
    cl_cmd(&mut job, format!("CRTBNDRPG PGM({lib}/RPG3FREE)"))
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/RPG3FREE)"))
            .await
            .unwrap(),
        [
            "DSPLY  MONTH IS OCTOBER",
            "DSPLY  150",
            "DSPLY  ALL3",
            "           1,234.50-      DONE"
        ]
    );
}

#[tokio::test]
async fn rpg_cycle_control_levels_and_program_described_files() {
    // The RPG cycle (ILE RPG Reference, "Detailed RPG Program Cycle"):
    // a primary file read by the cycle; the L1 control field REGION; on
    // a change (and at end of file) the total calculations and total
    // output run before the new record's fields are moved in, so a total
    // line shows the group that ended; LR at end of file sets L1 on too.
    // Program-described input (record identification codes in position
    // 1, fields by position, zoned with decimals, a field indicator for
    // zero) and output (EXCPT with ADD: a record image).
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    for (name, dds, rows) in [
        (
            "SALES",
            "     A          R SALESR\n     A            REGION         2A\n     A            CUST           5A\n     A            AMOUNT         7P 2\n",
            vec![
                vec![("REGION", "N1"), ("CUST", "C1"), ("AMOUNT", "10.00")],
                vec![("REGION", "N1"), ("CUST", "C2"), ("AMOUNT", "5.50")],
                vec![("REGION", "S1"), ("CUST", "C3"), ("AMOUNT", "7.25")],
                vec![("REGION", "S1"), ("CUST", "C4"), ("AMOUNT", "1.00")],
                vec![("REGION", "W1"), ("CUST", "C5"), ("AMOUNT", "20.00")],
            ],
        ),
        (
            "TRANS",
            "     A          R TRANSR\n     A            TYPE           1A\n     A            DATA          19A\n",
            vec![
                vec![("TYPE", "H"), ("DATA", "ACME")],
                vec![("TYPE", "D"), ("DATA", "00301250")],
                vec![("TYPE", "D"), ("DATA", "01000050")],
                vec![("TYPE", "D"), ("DATA", "00500000")],
            ],
        ),
    ] {
        let pf = dds::parse_pf(name, dds).unwrap();
        catalog::create_physical_file(&mut client, &lib, &pf)
            .await
            .unwrap();
        let mut f = open(&client, &lib, name).await;
        for r in rows {
            let rec: rla::Record = r
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            f.write(&client, &rec).await.unwrap();
        }
    }
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGSRC", "")
        .await
        .unwrap();
    for (name, src) in [
        ("SALESRPT", include_str!("rpg/salesrpt.rpg")),
        ("TRANSPD", include_str!("rpg/transpd.rpg")),
    ] {
        source::put_member(
            &client,
            &source::Member {
                library: lib.clone(),
                file: "QRPGSRC".into(),
                name: name.into(),
                srctype: "RPG".into(),
                text: String::new(),
                source: src.into(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
        cl_cmd(&mut job, format!("CRTRPGPGM PGM({lib}/{name})"))
            .await
            .unwrap();
    }
    let out = cl_cmd(&mut job, format!("CALL PGM({lib}/SALESRPT)"))
        .await
        .unwrap();
    let printed: Vec<&str> = out
        .iter()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        printed,
        [
            "   N1  C1           10.00",
            "   N1  C2            5.50",
            "   N1     TOTAL     15.50",
            "   S1  C3            7.25",
            "   S1  C4            1.00",
            "   S1     TOTAL      8.25",
            "   W1  C5           20.00",
            "   W1     TOTAL     20.00",
            "          GRAND          43.75",
        ],
        "{out:#?}"
    );
    let out = cl_cmd(&mut job, format!("CALL PGM({lib}/TRANSPD)"))
        .await
        .unwrap();
    assert_eq!(
        out,
        [
            "DSPLY  ACME",
            "DSPLY  37.50",
            "DSPLY  5.00",
            "DSPLY  .00",
            "DSPLY  ZERO",
            "DSPLY  42.50"
        ]
    );
    // The record added at LR: 'T' in position 1, the total as 9 zoned
    // digits ending in position 10.
    let mut f = open(&client, &lib, "TRANS").await;
    let mut last = None;
    loop {
        let r = f.read(&client).await.unwrap();
        match r.record {
            Some(rec) => last = Some(rec),
            None => break,
        }
    }
    let last = last.unwrap();
    assert_eq!(last["TYPE"], "T");
    assert_eq!(last["DATA"].trim_end(), "000004250");
}

#[tokio::test]
async fn rpg_cycle_matching_records() {
    // Matching fields (ILE RPG Reference, "Match Fields"): records of the
    // primary and secondary files are processed in ascending order of
    // their M1 fields, a primary record before secondary records with
    // the same value; MR is on for a record that matches a record of the
    // other file.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    for (name, dds, rows) in [
        (
            "CUSTS",
            "     A          R CUSTR\n     A            CUST           2A\n     A            NAME          10A\n",
            vec![
                vec![("CUST", "A1"), ("NAME", "Alpha")],
                vec![("CUST", "B2"), ("NAME", "Beta")],
                vec![("CUST", "C3"), ("NAME", "Gamma")],
            ],
        ),
        (
            "ORDS",
            "     A          R ORDR\n     A            CUST           2A\n     A            AMT            5S 0\n",
            vec![
                vec![("CUST", "A1"), ("AMT", "100")],
                vec![("CUST", "A1"), ("AMT", "50")],
                vec![("CUST", "C3"), ("AMT", "70")],
                vec![("CUST", "D4"), ("AMT", "10")],
            ],
        ),
    ] {
        let pf = dds::parse_pf(name, dds).unwrap();
        catalog::create_physical_file(&mut client, &lib, &pf)
            .await
            .unwrap();
        let mut f = open(&client, &lib, name).await;
        for r in rows {
            let rec: rla::Record = r
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            f.write(&client, &rec).await.unwrap();
        }
    }
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGSRC".into(),
            name: "MATCHREC".into(),
            srctype: "RPG".into(),
            text: String::new(),
            source: include_str!("rpg/matchrec.rpg").into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    cl_cmd(&mut job, format!("CRTRPGPGM PGM({lib}/MATCHREC)"))
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/MATCHREC)"))
            .await
            .unwrap(),
        [
            "DSPLY  Alpha",
            "DSPLY  CUST MR",
            "DSPLY  100",
            "DSPLY  50",
            "DSPLY  Beta",
            "DSPLY  Gamma",
            "DSPLY  CUST MR",
            "DSPLY  70",
            "DSPLY  NO CUST"
        ]
    );
}

#[tokio::test]
async fn rpg_free_form_7x_features_and_builtins() {
    // Expected values from the ILE RPG Reference (7.5): date arithmetic
    // (January 31 plus one month is February 28; %DIFF counts complete
    // units), date formats (*USA mm/dd/yyyy, *MDY two-digit years),
    // %SPLIT (runs of blanks make no empty elements), FOR-EACH, IN with
    // %RANGE, %LIST and an enumeration (DCL-ENUM), DIM(*AUTO) with *NEXT,
    // %XFOOT, %LOOKUP, %MAXARR, %MINARR, %SCANR, %SCANRPL, %LEFT, %RIGHT,
    // %CONCAT, %MAX, %MIN, %EDITW, %SQRT, OVERLOAD, SND-MSG *ESCAPE caught
    // by ON-EXCP.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = include_str!("rpg/freeform.rpgle");
    let r = run_rpg(&url, &lib, "QSECOFR", "FREEFORM", src, None, None).await;
    assert!(r.error.is_none(), "{:?} {:?}", r.error, r.messages);
    assert_eq!(
        r.messages,
        [
            "2026-02-28",
            "02/28/2026",
            "28",
            "2026",
            "00.15.00",
            "2026-10-21-01.45.30.000000",
            "2026-10-20",
            "2026-10-20",
            "20260228",
            "8",
            // %ELEM of a fixed array is its dimension.
            "5 brown",
            "a",
            "b",
            "c",
            "in range",
            "green is a color",
            "X not listed",
            "B",
            "2",
            "AAA",
            "BBB",
            "100",
            "3",
            "3",
            "2",
            "6",
            "bANANa",
            "abef",
            "x, y, z",
            "93",
            " 1,234.50",
            "4",
            "number 12.50",
            "date 31.01.2026",
            "caught CPF9898",
            "not a date",
            "a date",
            "blue or green",
        ]
    );
}

#[tokio::test]
async fn rpg_data_structure_io_kds_and_fields() {
    // ILE RPG Reference: CHAIN with a result data structure fills it, not
    // the program fields; %KDS gives the key from a data structure;
    // UPDATE with %FIELDS changes only the fields listed.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "DSIO",
        include_str!("rpg/dsio.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["Acme Tooling", "0", "Acme Tooling", "1.00"]);
}

#[tokio::test]
async fn rpg_fixed_to_free_conversion_runs_the_same() {
    // The conversion to free form must not change what a program does:
    // each program runs before and after, with the same output. RPG III
    // sources are first converted to RPG IV (as CVTRPGSRC does).
    use as4tainr_core::rpg::{rpg3, tofree};
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let rpg4 = |s: &str| rpg3::convert(s, None).unwrap().source;
    for (name, src, all_free) in [
        ("FIXEDOPS", include_str!("rpg/fixedops.rpgle").to_string(), false),
        ("RPG3TEST", rpg4(include_str!("rpg/rpg3test.rpg")), false),
        ("PSSRFIX", include_str!("rpg/pssrfix.rpgle").to_string(), false),
        ("PSSRCYC", include_str!("rpg/pssrcyc.rpgle").to_string(), false),
        ("OFLCYC", include_str!("rpg/oflcyc.rpgle").to_string(), false),
        ("OFLFETCH", include_str!("rpg/oflfetch.rpgle").to_string(), false),
        ("DSPFREE", "     DX                S              5P 2 INZ(1.5)\n     C     X             ADD       2             X\n     C     X             DSPLY\n     C                   EVAL      *INLR = *ON\n".to_string(), true),
    ] {
        let before = run_rpg(&url, &lib, "QSECOFR", name, &src, None, None).await;
        assert!(before.error.is_none(), "{name}: {:?}", before.error);
        let c = tofree::convert(&src).unwrap();
        assert_eq!(
            c.source.starts_with("**FREE"),
            all_free,
            "{name}:\n{}",
            c.source
        );
        let after = run_rpg(&url, &lib, "QSECOFR", name, &c.source, None, None).await;
        assert!(after.error.is_none(), "{name}: {:?}\n{}", after.error, c.source);
        assert_eq!(before.messages, after.messages, "{name}:\n{}", c.source);
        assert_eq!(before.printed, after.printed, "{name}");
    }
}

#[tokio::test]
async fn ile_activation_groups_and_signatures() {
    // ILE RPG Reference ("Program termination"): a program that returns
    // without LR stays active, its storage kept for the next call, until
    // its activation group ends (RCLACTGRP; RCLRSC for the default
    // group); ACTGRP(*NEW) starts each call afresh. ILE Concepts
    // ("Signature"): a program bound to a service program runs only while
    // the service program has the signature it was bound to, as its
    // current or a *PRV one; UPDPGM binds it again.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGLESRC", "QSRVSRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let counter = |ctl: &str| {
        format!("**FREE\n{ctl}\ndcl-s n packed(5:0) inz(0);\nn += 1;\ndsply %char(n);\nreturn;\n")
    };
    let members = [
        ("NAMEDGRP", "RPGLE", counter("ctl-opt dftactgrp(*no) actgrp('MYGROUP');")),
        ("DFTGRP", "RPGLE", counter("")),
        ("NEWGRP", "RPGLE", counter("ctl-opt dftactgrp(*no) actgrp(*new);")),
        ("SIGMOD", "RPGLE", "**FREE\nctl-opt nomain;\ndcl-proc hello export;\n  dcl-pi *n varchar(20) end-pi;\n  return 'hello v1';\nend-proc;\n".to_string()),
        ("SIGSRV", "BND", "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('V1')\n  EXPORT SYMBOL(HELLO)\nENDPGMEXP\n".to_string()),
        ("SIGSRV2", "BND", "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('V2')\n  EXPORT SYMBOL(HELLO)\nENDPGMEXP\n".to_string()),
        ("SIGSRV3", "BND", "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('V3')\n  EXPORT SYMBOL(HELLO)\nENDPGMEXP\nSTRPGMEXP PGMLVL(*PRV) SIGNATURE('V2')\n  EXPORT SYMBOL(HELLO)\nENDPGMEXP\n".to_string()),
        ("SIGUSER", "RPGLE", "**FREE\nctl-opt dftactgrp(*no);\ndcl-pr hello varchar(20) end-pr;\ndsply hello();\n*inlr = *on;\n".to_string()),
    ];
    for (name, t, text) in &members {
        let file = if *t == "BND" { "QSRVSRC" } else { "QRPGLESRC" };
        source::put_member(
            &client,
            &source::Member {
                library: lib.clone(),
                file: file.into(),
                name: (*name).into(),
                srctype: (*t).into(),
                text: String::new(),
                source: text.clone(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    }
    for p in ["NAMEDGRP", "DFTGRP", "NEWGRP"] {
        cl_cmd(&mut job, format!("CRTBNDRPG PGM({lib}/{p})"))
            .await
            .unwrap();
    }
    let call = |p: &str| format!("CALL PGM({lib}/{p})");
    // A named activation group: the count goes on until RCLACTGRP.
    assert_eq!(
        cl_cmd(&mut job, call("NAMEDGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    assert_eq!(
        cl_cmd(&mut job, call("NAMEDGRP")).await.unwrap(),
        ["DSPLY  2"]
    );
    cl_cmd(&mut job, "RCLACTGRP ACTGRP(MYGROUP)".into())
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, call("NAMEDGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    // The default activation group: until RCLRSC.
    assert_eq!(
        cl_cmd(&mut job, call("DFTGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    assert_eq!(
        cl_cmd(&mut job, call("DFTGRP")).await.unwrap(),
        ["DSPLY  2"]
    );
    cl_cmd(&mut job, "RCLACTGRP ACTGRP(*ELIGIBLE)".into())
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, call("DFTGRP")).await.unwrap(),
        ["DSPLY  3"]
    );
    cl_cmd(&mut job, "RCLRSC".into()).await.unwrap();
    assert_eq!(
        cl_cmd(&mut job, call("DFTGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    // *NEW: a new activation group each call.
    assert_eq!(
        cl_cmd(&mut job, call("NEWGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    assert_eq!(
        cl_cmd(&mut job, call("NEWGRP")).await.unwrap(),
        ["DSPLY  1"]
    );
    // Signatures.
    for c in [
        format!("CRTRPGMOD MODULE({lib}/SIGMOD)"),
        format!("CRTSRVPGM SRVPGM({lib}/SIGSRV) MODULE({lib}/SIGMOD) SRCFILE({lib}/QSRVSRC)"),
        format!("CRTRPGMOD MODULE({lib}/SIGUSER)"),
        format!("CRTPGM PGM({lib}/SIGUSER) MODULE({lib}/SIGUSER) BNDSRVPGM({lib}/SIGSRV)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        cl_cmd(&mut job, call("SIGUSER")).await.unwrap(),
        ["DSPLY  hello v1"]
    );
    let d = cl_cmd(&mut job, format!("DSPPGM PGM({lib}/SIGUSER)"))
        .await
        .unwrap();
    assert!(d.iter().any(|l| l.contains("V1")), "{d:?}");
    // New signature V2: the program bound to V1 no longer runs.
    cl_cmd(
        &mut job,
        format!("CRTSRVPGM SRVPGM({lib}/SIGSRV) MODULE({lib}/SIGMOD) SRCFILE({lib}/QSRVSRC) SRCMBR(SIGSRV2)"),
    )
    .await
    .unwrap();
    let e = cl_cmd(&mut job, call("SIGUSER")).await.unwrap_err();
    assert!(e.contains("MCH4431"), "{e}");
    // UPDPGM binds it again: it runs (bound to V2).
    cl_cmd(&mut job, format!("UPDPGM PGM({lib}/SIGUSER) MODULE(*NONE)"))
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, call("SIGUSER")).await.unwrap(),
        ["DSPLY  hello v1"]
    );
    // V3 keeps V2 as a previous signature: still runs.
    cl_cmd(
        &mut job,
        format!("CRTSRVPGM SRVPGM({lib}/SIGSRV) MODULE({lib}/SIGMOD) SRCFILE({lib}/QSRVSRC) SRCMBR(SIGSRV3)"),
    )
    .await
    .unwrap();
    assert_eq!(
        cl_cmd(&mut job, call("SIGUSER")).await.unwrap(),
        ["DSPLY  hello v1"]
    );
    let d = cl_cmd(&mut job, format!("DSPSRVPGM SRVPGM({lib}/SIGSRV)"))
        .await
        .unwrap();
    assert!(
        d.iter().any(|l| l.contains("V3")) && d.iter().any(|l| l.contains("V2")),
        "{d:?}"
    );
}

#[tokio::test]
async fn ile_exported_data() {
    // ILE RPG Reference, EXPORT and IMPORT keywords: a field exported by
    // one module is the same storage as the field another module of the
    // program (or a service program) imports.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    for (name, text) in [
        ("DATAMOD", "**FREE\nctl-opt nomain;\ndcl-s total packed(7:0) export inz(100);\ndcl-proc addIt export;\n  dcl-pi *n;\n    n packed(7:0) const;\n  end-pi;\n  total += n;\nend-proc;\n"),
        ("DATAMAIN", "**FREE\ndcl-s total packed(7:0) import;\ndcl-pr addIt;\n  n packed(7:0) const;\nend-pr;\ndsply %char(total);\naddIt(5);\ndsply %char(total);\ntotal = 1;\naddIt(1);\ndsply %char(total);\n*inlr = *on;\n"),
    ] {
        source::put_member(
            &client,
            &source::Member {
                library: lib.clone(),
                file: "QRPGLESRC".into(),
                name: name.into(),
                srctype: "RPGLE".into(),
                text: String::new(),
                source: text.into(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/DATAMOD)"),
        format!("CRTRPGMOD MODULE({lib}/DATAMAIN)"),
        format!("CRTPGM PGM({lib}/DATAMAIN) MODULE({lib}/DATAMAIN {lib}/DATAMOD)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/DATAMAIN)"))
            .await
            .unwrap(),
        ["DSPLY  100", "DSPLY  105", "DSPLY  2"]
    );
}

#[tokio::test]
async fn ile_cl_module_callprc() {
    // ILE CL (CL Programming, "Bound calls"): a CL module (CRTCLMOD)
    // bound with an RPG module by CRTPGM calls the RPG procedure with
    // CALLPRC: parameters by reference come back changed, RTNVAL gets the
    // return value.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    source::create_file(&client, &lib, "QCLSRC", "")
        .await
        .unwrap();
    for (file, name, t, text) in [
        ("QRPGLESRC", "MATHPRC", "RPGLE", "**FREE\nctl-opt nomain;\ndcl-proc addTo export;\n  dcl-pi *n packed(9:2);\n    total packed(9:2);\n    amount packed(9:2) value;\n  end-pi;\n  total += amount;\n  return total * 2;\nend-proc;\n"),
        ("QCLSRC", "CLMAIN", "CLLE", "PGM\n  DCL VAR(&TOT) TYPE(*DEC) LEN(9 2) VALUE(10)\n  DCL VAR(&DBL) TYPE(*DEC) LEN(9 2)\n  DCL VAR(&MSG) TYPE(*CHAR) LEN(40)\n  CALLPRC PRC('ADDTO') PARM((&TOT) (5.5 *BYVAL)) RTNVAL(&DBL)\n  CHGVAR VAR(&MSG) VALUE(%CHAR(&TOT) *BCAT %CHAR(&DBL))\n  SNDPGMMSG MSG(&MSG)\nENDPGM\n"),
    ] {
        source::put_member(
            &client,
            &source::Member {
                library: lib.clone(),
                file: file.into(),
                name: name.into(),
                srctype: t.into(),
                text: String::new(),
                source: text.into(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/MATHPRC)"),
        format!("CRTCLMOD MODULE({lib}/CLMAIN)"),
        format!("CRTPGM PGM({lib}/CLMAIN) MODULE({lib}/CLMAIN {lib}/MATHPRC)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let out = cl_cmd(&mut job, format!("CALL PGM({lib}/CLMAIN)"))
        .await
        .unwrap();
    assert!(out.iter().any(|m| m.contains("15.50 31.00")), "{out:?}");
}

#[tokio::test]
async fn rpg_status_data_structures_and_status_codes() {
    // ILE RPG Reference: the program status data structure (*PROC: the
    // program; *PARMS: parameters passed; position 358: the current
    // user), the file information data structure (*STATUS, *OPCODE,
    // *RECORD, open and end-of-file flags in 9 and 10, the relative
    // record number in 397, the record length in 283, the read count in
    // 247), and the status codes of "File Status Codes" and "Program
    // Status Codes": 00012 no record found on CHAIN, 00011 end of file,
    // 01021 duplicate key on WRITE, 00102 divide by zero (exception
    // MCH1211). Customer 2 is the fourth record written (setup); a
    // CUSTMAST record is 5P 0 + 30 + 20 + 9P 2 = 3 + 30 + 20 + 5 bytes.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "PSDS",
        include_str!("rpg/psds.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "PSDS",
            "0",
            "QSECOFR",
            "12 12",
            "CHAIN CUSTREC  1CUSTMAST",
            "0 4 58",
            "11 1 3",
            "1 1021 1021",
            "divide 102 0 MCH1211",
        ]
    );
}

#[tokio::test]
async fn rpg_pssr_infsr_and_return_points() {
    // ILE RPG Reference, "Program Exception/Error Subroutine" and "File
    // Exception/Error Subroutine (INFSR)": an error no operation handles
    // goes to *PSSR (a divide by zero: status 00102; *ROUTINE is the
    // routine the error happened in, *DETC: detail calculations); a
    // GOTO from it goes back into the calculations. A file error goes to
    // the file's INFSR (a duplicate key: 01021). In the cycle, ENDSR's
    // return point *GETIN goes on with the next record, and the return
    // point field is blank after ENDSR. In a subprocedure, RETURN in
    // *PSSR gives the procedure's value; ENDSR without RETURN makes the
    // procedure fail (ILE RPG Programmer's Guide, "Using a Program Error
    // Subroutine"). CALLP(E): %ERROR on, %STATUS the error's status.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let run = |name: &'static str, src: &'static str| {
        let (url, lib) = (url.clone(), lib.clone());
        async move { run_rpg(&url, &lib, "QSECOFR", name, src, None, None).await }
    };
    let r = run("PSSRFIX", include_str!("rpg/pssrfix.rpgle")).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["102", "*DETC", "5"]);
    let r = run("INFSR", include_str!("rpg/infsr.rpgle")).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1021", "Y"]);
    let r = run("PSSRCYC", include_str!("rpg/pssrcyc.rpgle")).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1", "2", "3", "4", "skip", "*GETIN"]);
    let r = run("PSSRPROC", include_str!("rpg/pssrproc.rpgle")).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        ["-1", "2", "in pssr", "fails failed", "in pssr", "1 102"]
    );
    // ENDSR *CANCL ends the program; ENDSR without a return point after an
    // error: the default error handler (the program ends abnormally).
    let cancl = include_str!("rpg/pssrcyc.rpgle").replace("INZ('*GETIN')", "INZ('*CANCL')");
    let r = run_rpg(&url, &lib, "QSECOFR", "PSSRCAN", &cancl, None, None).await;
    assert!(r.error.is_some());
    assert_eq!(r.messages, ["1", "2", "3", "4", "skip", "*CANCL"]);
    let blank = include_str!("rpg/pssrcyc.rpgle").replace("INZ('*GETIN')", "");
    let r = run_rpg(&url, &lib, "QSECOFR", "PSSRBLK", &blank, None, None).await;
    let e = r.error.expect("the error goes on");
    assert!(format!("{e:#}").contains("MCH1211"), "{e:#}");
}

#[tokio::test]
async fn rpg_files_in_procedures() {
    // ILE RPG Reference, "File Considerations": a file declared in a
    // procedure is opened when the procedure is called and closed when
    // it returns, so each call reads from the start; with STATIC it stays
    // open and goes on. Its records are read into data structures.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "LOCALF",
        include_str!("rpg/localf.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["Midwest Fasteners", "?", "1 2", "1 1"]);
}

#[tokio::test]
async fn rpg_overflow_and_fetch_overflow() {
    // ILE RPG Reference, "Overflow Routine" and "Fetch Overflow Routine":
    // printing on or past the overflow line sets the overflow indicator;
    // the lines conditioned by it print once, in the cycle after total
    // output (before the next detail line), or, with fetch overflow (F
    // in position 18), just before the line that fetches them. A skip to
    // a line before the current one starts a new page.
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "OFLCYC",
        include_str!("rpg/oflcyc.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let r2 = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "OFLFETCH",
        include_str!("rpg/oflfetch.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r2.error.is_none(), "{:?}", r2.error);
    assert_eq!(
        r.printed,
        [
            "   HEADING",
            "    1",
            "    2",
            "\u{c}",
            "   HEADING",
            "    3",
            "    4",
            "\u{c}",
            "   HEADING",
            "    5"
        ]
    );
    assert_eq!(
        r2.printed,
        [
            "    1",
            "    2",
            "    3",
            "\u{c}",
            "   HEADING",
            "    4",
            "    5"
        ]
    );
}

#[tokio::test]
async fn rpg_local_data_area() {
    // CL Programming, "Local data area": each job has one of 1024
    // characters; CHGDTAARA, RTVDTAARA and DSPDTAARA take DTAARA(*LDA).
    // ILE RPG Reference: an unnamed data area data structure (U in
    // position 23) is the local data area, read at the start and written
    // back at LR. A submitted job starts with a copy of the submitter's.
    use as4tainr_core::commands::Outcome;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    job_run(&client, &lib, "CHGDTAARA DTAARA(*LDA) VALUE(' ')")
        .await
        .unwrap();
    // A local data area starts as blanks: a packed subfield over blanks
    // is a decimal data error (status 00907) when the program uses it.
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "LDA",
        include_str!("rpg/lda.rpgle"),
        None,
        None,
    )
    .await;
    assert!(format!("{:#}", r.error.unwrap()).contains("RNQ0907"));
    // Positions 6-10 as packed zero (X'000000000F').
    let mut bytes = as4tainr_core::rpg::data::e("FIRST");
    bytes.extend([0, 0, 0, 0, 0x0F]);
    as4tainr_core::dtaara::lda_write(&client, None, &bytes)
        .await
        .unwrap();
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "LDA",
        include_str!("rpg/lda.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["FIRST", ".00"]);
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "LDA",
        include_str!("rpg/lda.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["NEXT", "1.25"]);
    let lda = as4tainr_core::dtaara::lda_area(&client, None)
        .await
        .unwrap();
    assert_eq!(&lda.value[..5], "NEXT ");
    let Outcome::Display(d) = job_run(&client, &lib, "DSPDTAARA *LDA").await.unwrap() else {
        panic!()
    };
    assert!(d.rows[0].contains("NEXT"), "{:?}", d.rows);
    let pgm = "PGM\n DCL &K *CHAR 5\n RTVDTAARA DTAARA(*LDA (1 5)) RTNVAR(&K)\n SNDPGMMSG MSG(&K)\nENDPGM\n";
    let m = cl_job_run(&client, &url, &lib, &[("LDAPGM", pgm)], "CALL LDAPGM")
        .await
        .unwrap();
    assert!(m.contains(&"NEXT".to_string()), "{m:?}");
    // SBMJOB: the new job's local data area is a copy.
    as4tainr_core::dtaara::lda_write(&client, Some(990001), &[0xC1; 4])
        .await
        .unwrap();
    as4tainr_core::dtaara::lda_start(&client, 990002, Some(990001))
        .await
        .unwrap();
    let copy = as4tainr_core::dtaara::lda_area(&client, Some(990002))
        .await
        .unwrap();
    assert_eq!(&copy.value[..5], "AAAA ");
    assert_eq!(copy.value.len(), 1024);
}

#[tokio::test]
async fn cl_defined_and_based_storage() {
    // CL reference, DCL: STG(*DEFINED) DEFVAR(&var start) puts a variable
    // over part of another's storage (a *DEC one packed, digits / 2 + 1
    // bytes); STG(*BASED) BASPTR(&ptr) puts it where the pointer points;
    // ADDRESS and %ADDRESS set a pointer, %OFFSET moves it; CHGVAR %SST
    // changes part of a variable. TYPE(*PTR) STG(*DEFINED) over a
    // character variable is the usual way to keep a copyright text.
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let pgm = r#"
PGM
  DCL &COPYRIGHT *CHAR 64 VALUE('(C) Example')
  DCL &COPYRIGHTP *PTR STG(*DEFINED) DEFVAR(&COPYRIGHT)
  DCL &DATE *CHAR 8 VALUE('20261020')
  DCL &YEAR *CHAR 4 STG(*DEFINED) DEFVAR(&DATE)
  DCL &MONTH *CHAR 2 STG(*DEFINED) DEFVAR(&DATE 5)
  DCL &REC *CHAR 6 VALUE('ABCDEF')
  DCL &NUM *DEC (5 0) STG(*DEFINED) DEFVAR(&REC 4)
  DCL &P *PTR ADDRESS(&DATE 6)
  DCL &B *CHAR 2 STG(*BASED) BASPTR(&P)
  SNDPGMMSG MSG(&YEAR *CAT '/' *CAT &MONTH)
  CHGVAR &MONTH '11'
  SNDPGMMSG MSG(&DATE)
  SNDPGMMSG MSG(&B)
  CHGVAR &B 'XY'
  SNDPGMMSG MSG(&DATE)
  CHGVAR %OFFSET(&P) 0
  SNDPGMMSG MSG(&B)
  CHGVAR &P %ADDRESS(&MONTH)
  SNDPGMMSG MSG(&B *CAT ' ' *CAT %CHAR(%OFFSET(&P)))
  CHGVAR &NUM 123
  SNDPGMMSG MSG(%SST(&REC 1 3))
  SNDPGMMSG MSG(%CHAR(&NUM))
  CHGVAR %SST(&REC 1 2) 'ZZ'
  SNDPGMMSG MSG(%SST(&REC 1 3) *CAT %CHAR(&NUM))
ENDPGM
"#;
    let m = cl_job_run(&client, &url, &lib, &[("DEFPGM", pgm)], "CALL DEFPGM")
        .await
        .unwrap();
    assert_eq!(
        m,
        ["2026/10", "20261120", "20", "202611XY", "20", "11 4", "ABC", "123", "ZZC123"]
    );
    // DEFVAR must fit in its base.
    let bad = "PGM\n DCL &A *CHAR 4\n DCL &B *CHAR 3 STG(*DEFINED) DEFVAR(&A 3)\nENDPGM\n";
    assert!(as4tainr_core::clpgm::compile("BAD", bad).is_err());
}

#[tokio::test]
async fn user_commands_crtcmd() {
    // CL Programming, "Defining commands": CRTCMD makes a command from
    // definition statements; running it checks the values (MIN(1)
    // required, RANGE, RSTD) and calls the command processing program
    // with each parameter in its CPP form: *CHAR padded to LEN, *DEC as a
    // packed number, a qualified name object first (20 characters), a list
    // as a 2-byte count and the values; CASE(*MIXED) keeps a quoted value's
    // case. DLTCMD deletes the command.
    use as4tainr_core::commands::Outcome;
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    source::create_file(&client, &lib, "QCMDSRC", "")
        .await
        .unwrap();
    let cmdsrc = "             CMD        PROMPT('Say hello')
             PARM       KWD(NAME) TYPE(*CHAR) LEN(10) MIN(1) CASE(*MIXED) +
                          PROMPT('Name')
             PARM       KWD(TIMES) TYPE(*DEC) LEN(3 0) DFT(1) RANGE(1 9)
             PARM       KWD(OBJ) TYPE(QO)
QO:          QUAL       TYPE(*NAME) LEN(10) DFT(X)
             QUAL       TYPE(*NAME) LEN(10) DFT(*LIBL) SPCVAL((*LIBL)) +
                          PROMPT('Library')
             PARM       KWD(MODE) TYPE(*CHAR) LEN(4) RSTD(*YES) DFT(*ALL) +
                          VALUES(*ALL *ONE)
";
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QCMDSRC".into(),
            name: "HELLO".into(),
            srctype: "CMD".into(),
            text: "Say hello".into(),
            source: cmdsrc.into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let cpp = "PGM PARM(&NAME &TIMES &OBJ &MODE)
 DCL &NAME *CHAR 10
 DCL &TIMES *DEC (3 0)
 DCL &OBJ *CHAR 20
 DCL &MODE *CHAR 4
 SNDPGMMSG MSG('Hello' *BCAT &NAME *BCAT %CHAR(&TIMES) *BCAT %SST(&OBJ 11 10) *TCAT '/' *CAT %SST(&OBJ 1 10) *BCAT &MODE)
ENDPGM
";
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("HELLOCPP", cpp)],
        &format!("CRTCMD CMD({lib}/HELLO) PGM({lib}/HELLOCPP) SRCFILE({lib}/QCMDSRC)"),
    )
    .await
    .unwrap();
    // CPC0202: PUB400 2026-10-23 (tools/golden/qsh.txt q_crtcmd); the
    // text is as4tainr's own.
    assert_eq!(
        m,
        [format!("CPC0202: Command HELLO is now in library {lib}.")]
    );
    let run = |line: String| {
        let client = &client;
        let lib = lib.clone();
        async move { job_run(client, &lib, &line).await }
    };
    let msgs = |o: Outcome| match o {
        Outcome::Messages(m) => m,
        other => panic!("unexpected {other:?}"),
    };
    let m = msgs(
        run("HELLO NAME('World') TIMES(3) OBJ(QGPL/FOO)".into())
            .await
            .unwrap(),
    );
    assert_eq!(m, ["Hello World 3 QGPL/FOO *ALL"]);
    // Positional, defaults, library qualified command name.
    let m = msgs(run(format!("{lib}/HELLO 'you' MODE(*ONE)")).await.unwrap());
    assert_eq!(m, ["Hello you 1 *LIBL/X *ONE"]);
    for bad in [
        "HELLO",
        "HELLO X TIMES(10)",
        "HELLO X MODE(*TWO)",
        "HELLO X NOPE(1)",
    ] {
        let e = run(bad.into()).await.unwrap_err();
        assert!(e.to_string().starts_with("CPF0006"), "{bad}: {e}");
    }
    // From a CL program, with a variable.
    let caller = "PGM\n DCL &N *CHAR 10 VALUE('Ada')\n HELLO NAME(&N) TIMES(2)\nENDPGM\n";
    let m = cl_job_run(&client, &url, &lib, &[("CALLER", caller)], "CALL CALLER")
        .await
        .unwrap();
    assert!(m.contains(&"Hello Ada 2 *LIBL/X *ALL".to_string()), "{m:?}");
    let Outcome::Display(d) = run("DSPCMD HELLO".into()).await.unwrap() else {
        panic!()
    };
    assert!(d.info.contains(&(
        "Program to process command".into(),
        format!("{lib}/HELLOCPP")
    )));
    let m = msgs(run("DLTCMD HELLO".into()).await.unwrap());
    assert_eq!(m, [format!("Command HELLO deleted from library {lib}.")]);
    assert!(run("HELLO X".into()).await.is_err());
}

#[tokio::test]
async fn printer_files_crtprtf_and_rpg() {
    // CRTPRTF (CL reference): a printer file from DDS, PAGESIZE(12 80),
    // OVRFLW(5). DDS reference for printer files: SKIPB(1) goes to line 1
    // (a new page when the printer is past it), SPACEA after the record,
    // SPACEB before it, positions and '+n' after the field before, PAGNBR
    // the page number, option indicators on fields. ILE RPG Reference:
    // WRITE of the record formats; the overflow indicator comes on when a
    // line is printed on or past the overflow line.
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    source::create_file(&client, &lib, "QDDSSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QDDSSRC".into(),
            name: "CUSTRPT".into(),
            srctype: "PRTF".into(),
            text: "Customer balances".into(),
            source: include_str!("rpg/custrpt.prtf").into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let m = job_run(
        &client,
        &lib,
        &format!("CRTPRTF FILE({lib}/CUSTRPT) SRCFILE({lib}/QDDSSRC) PAGESIZE(12 80) OVRFLW(5)"),
    )
    .await
    .unwrap();
    let as4tainr_core::commands::Outcome::Messages(m) = m else {
        panic!()
    };
    // as4tainr's own text (the completion message's ID is not captured
    // yet).
    assert_eq!(
        m,
        [format!(
            "Printer file CUSTRPT was created in library {lib}."
        )]
    );
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "CUSTRPT",
        include_str!("rpg/custrpt.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let head = |page: &str| format!(" Customer balances{}Page {page:>4}", " ".repeat(11));
    let p = &r.printed;
    // Page 1: heading on line 1, details from line 3; printing on line 5
    // (the overflow line) sets the overflow indicator.
    assert_eq!(p[0], head("1"));
    assert_eq!(p[1], "");
    assert!(p[2].starts_with("     1  Acme Tooling          "), "{p:?}");
    assert!(p[2].contains("1,250.50"), "{p:?}");
    assert!(p[3].starts_with("     2  Midwest Fasteners"), "{p:?}");
    assert!(p[4].starts_with("     3  Bluebird Supply"), "{p:?}");
    // Page 2: the heading again, then the rest; 50 marks a negative
    // balance; TOTAL spaces one line before it.
    assert_eq!(p[5], "\u{c}");
    assert_eq!(p[6], head("2"));
    assert!(p[8].starts_with("     4  Prairie Freight"), "{p:?}");
    assert!(
        p[9].starts_with("     5  Lake, Pine & Co") && p[9].ends_with('*'),
        "{p:?}"
    );
    assert_eq!(p[10], "");
    assert_eq!(p[11], " Records:   5");
    assert_eq!(p.len(), 12, "{p:?}");
    // A program-described printer file takes the object's page size and
    // overflow line.
    job_run(
        &client,
        &lib,
        &format!("CRTPRTF FILE({lib}/SMALLPRT) PAGESIZE(4 80) OVRFLW(3)"),
    )
    .await
    .unwrap();
    // Lines 1-3 fill the page to its overflow line (3): OF on after line
    // 3 and after line 4; line 5 is on page 2 (the page has 4 lines).
    let src = concat!(
        "     FSMALLPRT  OF       80        PRINTER OFLIND(*INOF)\n",
        "     DI                S              3P 0\n",
        "     C                   DO        5             I\n",
        "     C                   EXCEPT    LINE\n",
        "     C   OFI             DSPLY\n",
        "     C   OF              SETOFF                                         OF\n",
        "     C                   ENDDO\n",
        "     C                   SETON                                          LR\n",
        "     OSMALLPRT  E            LINE           1\n",
        "     O                       I             Z      5\n",
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "SMALL", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["3", "4"]);
    assert_eq!(
        r.printed,
        ["    1", "    2", "    3", "    4", "\u{c}", "    5"]
    );
}

/// READ of a display file (ILE RPG Reference, READ; EXFMT is a WRITE
/// then a READ): WRITE puts the record on the display; READ takes the
/// operator's input into its fields, by its format's name or the file's
/// (the record with input fields on the display). READ of a record not on
/// the display fails.
#[tokio::test(flavor = "multi_thread")]
async fn rpg_read_of_a_display_record() {
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    catalog::create_display_file(
        &mut client,
        &lib,
        "CUSTINQD",
        include_str!("../../samples/qddssrc/CUSTINQD.dspf"),
    )
    .await
    .unwrap();
    let (tx, seen) = operator(vec![
        (
            press("ENTER", &[("CUSNUM", "7")], &[]),
            Box::new(|f| assert_eq!(f.layers.last().unwrap().format, "PROMPT")),
        ),
        (
            press("ENTER", &[("CUSNUM", "12")], &[]),
            Box::new(|f| {
                let l = f.layers.last().unwrap();
                assert_eq!(l.format, "PROMPT");
                // The record as written: CUSNUM 8 (7 + 1).
                assert_eq!(l.values.get("CUSNUM").map(|v| v.trim()), Some("8"));
            }),
        ),
    ]);
    let src = "**FREE\ndcl-f custinqd workstn;\nwrite prompt;\nread prompt;\ndsply %char(cusnum);\ncusnum += 1;\nwrite prompt;\nread custinqd;\ndsply %char(cusnum);\nread detail;\n*inlr = *on;\n";
    let r = run_rpg(&url, &lib, "QSECOFR", "READDSP", src, Some(tx), None).await;
    assert_eq!(r.messages, ["7", "12"]);
    let e = format!("{:#}", r.error.expect("DETAIL is not on the display"));
    assert!(e.contains("READ DETAIL"), "{e}");
    assert_eq!(seen.await.unwrap(), 2);
}

/// An optional parameter (IBM CL reference, PGM and MONMSG): a parameter
/// the caller did not pass has no storage, so using it is MCH3601, which a
/// MONMSG after the CHGVAR handles (MONMSG can follow any command,
/// CHGVAR included). The MONMSG after that EXEC's CHGVAR belongs to the
/// first CHGVAR, not to the one in EXEC. Passed, the value is used.
#[tokio::test]
async fn cl_optional_parameter_and_monmsg_after_chgvar() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let pgm = "PGM PARM(&P)
  DCL &P *CHAR 10
  DCL &V *CHAR 10
  CHGVAR &V &P
  MONMSG MCH3601 EXEC(CHGVAR &V 'DEFAULT')
  MONMSG CPF0000 EXEC(CHGVAR &V 'OTHER')
  SNDPGMMSG MSG('V=' *CAT &V)
ENDPGM";
    let m = cl_job_run(&client, &url, &lib, &[("OPTPARM", pgm)], "CALL OPTPARM")
        .await
        .unwrap();
    assert_eq!(m.last().unwrap(), "V=DEFAULT");
    let m = cl_job_run(
        &client,
        &url,
        &lib,
        &[("OPTPARM", pgm)],
        "CALL OPTPARM PARM(GIVEN)",
    )
    .await
    .unwrap();
    assert_eq!(m.last().unwrap(), "V=GIVEN");
    // Not monitored: the program ends with MCH3601.
    let bare = "PGM PARM(&P)
  DCL &P *CHAR 10
  DCL &V *CHAR 10
  CHGVAR &V &P
ENDPGM";
    let e = cl_job_run(&client, &url, &lib, &[("BAREPARM", bare)], "CALL BAREPARM")
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("MCH3601"), "{e:#}");
}

/// Db2 for i scalar functions that PostgreSQL writes differently (Db2 for
/// i SQL Reference, scalar functions). DECIMAL/DEC: precision 15 and scale
/// 0 by default, extra fraction digits cut off (truncated, as assigning
/// to a decimal column does), a string argument read as a number.
/// DAYOFWEEK: 1 is Sunday (2024-03-10 was a Sunday). DAYOFYEAR: March 1 of
/// the leap year 2024 is day 61. DAYS: 0001-01-01 is day 1. QUARTER: 1-4.
#[tokio::test]
async fn sql_db2_scalar_functions() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let libl = vec!["QSYS".to_string(), lib.clone()];
    let run = |sql: &str| {
        let (client, url, libl, sql) = (&client, url.clone(), libl.clone(), sql.to_string());
        async move {
            match as4tainr_core::sql::run_as(client, &url, &libl, "QSECOFR", None, None, &sql)
                .await
                .unwrap()
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => rows[0]
                    .iter()
                    .map(|v| v.trim().to_string())
                    .collect::<Vec<_>>()
                    .join("|"),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        run(
            "SELECT DEC(2.349, 5, 2), DECIMAL(-2.349, 5, 2), DEC(7.9), DEC('12.345', 5, 1), \
             DEC(ROUND(2.345, 2), 19, 4) FROM SYSIBM.SYSDUMMY1"
        )
        .await,
        "2.34|-2.34|7|12.3|2.3500"
    );
    assert_eq!(
        run(
            "SELECT DAYOFWEEK(DATE('2024-03-10')), DAYOFYEAR(DATE('2024-03-01')), \
             DAYS(DATE('0001-01-01')), DAYS(DATE('0001-01-31')), QUARTER(DATE('2024-08-15')), \
             YEAR(DATE('2024-08-15')), MONTH(DATE('2024-08-15')), DAY(DATE('2024-08-15')) \
             FROM SYSIBM.SYSDUMMY1"
        )
        .await,
        "1|61|1|31|3|2024|8|15"
    );
    // CHAR (Db2 for i SQL Reference, CHAR): a decimal with a minus sign, no
    // leading zeros, the decimal point when the scale is not 0 and its
    // trailing zeros; an integer's digits (EDLEVEL 18 is '18'); a floating
    // point number as '-9.8765432100000002E-33' (the reference's example);
    // a date in the format named (CHAR(PRSTDATE, USA) of 1988-12-25 is
    // '12/25/1988').
    assert_eq!(
        run(
            "SELECT CHAR(DEC(-12.5, 5, 2)), CHAR(DEC(0.5, 5, 2)), CHAR(DEC(7, 5, 0)), CHAR(18), \
             CHAR(-9.8765432100000002E-33), CHAR(DATE('1988-12-25'), USA), \
             CHAR(DATE('1988-12-25'), EUR), CHAR(DATE('1988-12-25'), ISO) FROM SYSIBM.SYSDUMMY1"
        )
        .await,
        "-12.50|.50|7|18|-9.8765432100000002E-33|12/25/1988|25.12.1988|1988-12-25"
    );
    // CAST to an integer or a decimal of smaller scale cuts off the
    // fraction digits (Db2 for i SQL Reference, numeric assignments: "the
    // fractional part of the number is eliminated"); DIGITS gives the
    // absolute value's digits to the precision (DIGITS); TIMESTAMP reads
    // Db2's yyyy-mm-dd-hh.mm.ss.nnnnnn form and CHAR writes it back.
    assert_eq!(
        run(
            "SELECT CAST(-1.5 AS INTEGER), CAST(7.9 AS SMALLINT), CAST(7.59 AS DEC(5, 1)), \
             CAST('-3.77' AS DECIMAL(5, 1)), DIGITS(DEC(-12.5, 5, 2)), DIGITS(CAST(42 AS INTEGER)), \
             CHAR(TIMESTAMP('2026-10-22-20.05.09.123456')) FROM SYSIBM.SYSDUMMY1"
        )
        .await,
        "-1|7|7.5|-3.7|01250|0000000042|2026-10-22-20.05.09.123456"
    );
    // Labeled durations (Db2 for i SQL Reference, datetime arithmetic):
    // a month added to January 31 ends on the month's last day.
    assert_eq!(
        run(
            "SELECT CHAR(DATE('2026-01-31') + 1 MONTH, ISO), CHAR(DATE('2024-02-29') + 1 YEAR, ISO), \
             CHAR(DATE('2026-10-22') + 10 DAYS, ISO) FROM SYSIBM.SYSDUMMY1"
        )
        .await,
        "2026-02-28|2025-02-28|2026-11-01"
    );
}

/// A service program whose NOMAIN module holds an SQL cursor (Db2 for i
/// SQL Programming, "Cursor scope", and SET OPTION CLOSQLCSR: *ENDACTGRP
/// is the default, so a cursor stays open until its activation group
/// ends). SET OPTION and DECLARE CURSOR are declarations, allowed in a
/// NOMAIN module's global section; one procedure opens the cursor and
/// another fetches from it, call after call. RCLACTGRP closes it: the
/// next fetch is SQLCODE -501. BNDSRVPGM elements are (service program
/// activation). A CHAR host variable compares with blank padding. GET
/// DIAGNOSTICS reads the previous statement: ROW_COUNT, and condition 1's
/// RETURNED_SQLSTATE, DB2_RETURNED_SQLCODE and DB2_MESSAGE_ID (an unknown
/// column is SQL0206, SQLSTATE 42703); it ends with SQLSTATE 00000.
/// Compile options with nothing to change here (DBGVIEW,
/// RPGPPOPT) are accepted, as is CRTDSPF REPLACE(*YES) over an existing
/// display file.
#[tokio::test]
async fn ile_service_program_sql_cursor_across_calls() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for s in [
        format!("RUNSQL SQL('CREATE TABLE {lib}.ITEMS (ID INTEGER NOT NULL PRIMARY KEY, NAME VARCHAR(20))') COMMIT(*NONE)"),
        format!("RUNSQL SQL('INSERT INTO {lib}.ITEMS VALUES (1, ''one''), (2, ''two''), (3, ''three'')') COMMIT(*NONE)"),
    ] {
        cl_cmd(&mut job, s).await.unwrap();
    }
    for f in ["QRPGLESRC", "QSRVSRC", "QDDSSRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, text: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let members = [
        put("QRPGLESRC", "UTIL", "RPGLE", "**FREE\nctl-opt nomain;\ndcl-proc twice export;\n  dcl-pi *n int(10);\n    n int(10) const;\n  end-pi;\n  return n * 2;\nend-proc;\n"),
        put("QSRVSRC", "UTIL", "BND", "STRPGMEXP PGMLVL(*CURRENT)\n  EXPORT SYMBOL(TWICE)\nENDPGMEXP\n"),
        put("QRPGLESRC", "ITEMSV", "SQLRPGLE", "**FREE\nctl-opt nomain;\nexec sql set option commit = *none, closqlcsr = *endactgrp;\ndcl-s gFrom int(10);\ndcl-s gName varchar(20);\nexec sql declare ITEMLIST cursor for\n  select NAME from ITEMS where ID >= :gFrom order by ID;\ndcl-proc openList export;\n  dcl-pi *n int(10);\n    from int(10) const;\n  end-pi;\n  gFrom = from;\n  exec sql open ITEMLIST;\n  return sqlcode;\nend-proc;\ndcl-proc nextName export;\n  dcl-pi *n varchar(20) end-pi;\n  exec sql fetch next from ITEMLIST into :gName;\n  if sqlcode <> 0;\n    return '*' + %char(sqlcode);\n  endif;\n  return gName;\nend-proc;\n"),
        put("QSRVSRC", "ITEMSV", "BND", "STRPGMEXP PGMLVL(*CURRENT)\n  EXPORT SYMBOL(OPENLIST)\n  EXPORT SYMBOL(NEXTNAME)\nENDPGMEXP\n"),
        put("QRPGLESRC", "LISTA", "RPGLE", "**FREE\nctl-opt dftactgrp(*no) actgrp('ITEMS') bnddir('ITEMDIR');\ndcl-pr openList int(10);\n  from int(10) const;\nend-pr;\ndcl-pr nextName varchar(20) end-pr;\ndsply %char(openList(2));\ndsply nextName();\n*inlr = *on;\n"),
        put("QRPGLESRC", "LISTB", "RPGLE", "**FREE\nctl-opt dftactgrp(*no) actgrp('ITEMS') bnddir('ITEMDIR');\ndcl-pr nextName varchar(20) end-pr;\ndsply nextName();\ndsply nextName();\n*inlr = *on;\n"),
        put("QRPGLESRC", "LISTC", "SQLRPGLE", "**FREE\ndcl-s blank char(5);\ndcl-s key char(10) inz('two');\ndcl-s n int(10);\ndcl-s st char(5);\ndcl-s id varchar(10);\nexec sql set option commit = *none;\nexec sql select count(*) into :n from ITEMS where (:blank = ' ' or ID = 0);\ndsply %char(n);\nexec sql select ID into :n from ITEMS where NAME = :key;\ndsply %char(n);\nexec sql update ITEMS set NAME = NAME where ID >= 2;\nexec sql get diagnostics :n = ROW_COUNT;\ndsply %char(n);\nexec sql select NOSUCH into :n from ITEMS;\nexec sql get diagnostics condition 1 :st = RETURNED_SQLSTATE, :n = DB2_RETURNED_SQLCODE, :id = DB2_MESSAGE_ID;\ndsply (st + ' ' + %char(n) + ' ' + id + ' ' + sqlstate);\n*inlr = *on;\n"),
        put("QDDSSRC", "ITEMD", "DSPF", "     A          R ITEMR\n     A                                  1  2'Items'\n"),
    ];
    for m in &members {
        source::put_member(&client, m, false).await.unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/UTIL) DBGVIEW(*SOURCE) REPLACE(*YES)"),
        format!("CRTSRVPGM SRVPGM({lib}/UTIL) MODULE({lib}/UTIL) SRCFILE({lib}/QSRVSRC)"),
        format!("CRTSQLRPGI OBJ({lib}/ITEMSV) SRCFILE({lib}/QRPGLESRC) OBJTYPE(*MODULE) RPGPPOPT(*LVL2) DBGVIEW(*SOURCE)"),
        format!("CRTSRVPGM SRVPGM({lib}/ITEMSV) MODULE({lib}/ITEMSV) EXPORT(*SRCFILE) SRCFILE({lib}/QSRVSRC) BNDSRVPGM(({lib}/UTIL *IMMED)) ACTGRP(*CALLER)"),
        format!("CRTBNDDIR BNDDIR({lib}/ITEMDIR)"),
        format!("ADDBNDDIRE BNDDIR({lib}/ITEMDIR) OBJ(({lib}/ITEMSV *SRVPGM))"),
        format!("CRTBNDRPG PGM({lib}/LISTA) DBGVIEW(*SOURCE)"),
        format!("CRTBNDRPG PGM({lib}/LISTB)"),
        format!("CRTDSPF FILE({lib}/ITEMD) SRCFILE({lib}/QDDSSRC) RSTDSP(*YES)"),
        format!("CRTDSPF FILE({lib}/ITEMD) SRCFILE({lib}/QDDSSRC) REPLACE(*YES)"),
    ] {
        cl_cmd(&mut job, c.clone()).await.unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    // Fixed-length character host variables compare as CHAR(n): blanks
    // added to the shorter value (Db2 for i SQL Reference, "String
    // comparisons").
    cl_cmd(
        &mut job,
        format!("CRTSQLRPGI OBJ({lib}/LISTC) SRCFILE({lib}/QRPGLESRC)"),
    )
    .await
    .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/LISTC)"))
            .await
            .unwrap(),
        [
            "DSPLY  3",
            "DSPLY  2",
            "DSPLY  2",
            "DSPLY  42703 -206 SQL0206 00000"
        ]
    );
    let e = cl_cmd(
        &mut job,
        format!("CRTDSPF FILE({lib}/ITEMD) SRCFILE({lib}/QDDSSRC) REPLACE(*NO)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF7302"), "{e}");
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/LISTA)"))
            .await
            .unwrap(),
        ["DSPLY  0", "DSPLY  two"]
    );
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/LISTB)"))
            .await
            .unwrap(),
        ["DSPLY  three", "DSPLY  *100"]
    );
    cl_cmd(&mut job, "RCLACTGRP ACTGRP(ITEMS)".into())
        .await
        .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/LISTB)"))
            .await
            .unwrap(),
        ["DSPLY  *-501", "DSPLY  *-501"]
    );
}

/// SQL triggers (Db2 for i SQL Reference, CREATE TRIGGER, DROP TRIGGER):
/// an AFTER UPDATE OF trigger with REFERENCING OLD/NEW and WHEN runs its
/// INSERT for the rows whose price changed, and the USER special register
/// is the job's user;
/// a BEFORE INSERT trigger's SET changes the new row; SIGNAL fails the
/// change with its SQLSTATE. CREATE TRIGGER of an existing name without
/// OR REPLACE is SQL0601; DROP TRIGGER removes it. CREATE OR REPLACE
/// TABLE ... ON REPLACE PRESERVE ROWS keeps the rows (in the columns kept).
#[tokio::test]
async fn sql_triggers_and_preserve_rows() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    for s in [
        format!("CREATE TABLE {lib}.PRICES (ID INTEGER NOT NULL PRIMARY KEY, NAME VARCHAR(20), PRICE DECIMAL(7, 2))"),
        format!("CREATE TABLE {lib}.PRCLOG (ID INTEGER, OLDP DECIMAL(7, 2), NEWP DECIMAL(7, 2), WHO VARCHAR(18))"),
        format!("INSERT INTO {lib}.PRICES VALUES (1, 'one', 10.00), (2, 'two', 20.00)"),
        format!("CREATE TRIGGER {lib}.PRCTRG AFTER UPDATE OF PRICE ON {lib}.PRICES REFERENCING OLD AS O NEW AS N FOR EACH ROW WHEN (O.PRICE <> N.PRICE) INSERT INTO {lib}.PRCLOG VALUES (N.ID, O.PRICE, N.PRICE, USER)"),
        format!("CREATE TRIGGER {lib}.NAMTRG NO CASCADE BEFORE INSERT ON {lib}.PRICES REFERENCING NEW AS N FOR EACH ROW BEGIN ATOMIC SET N.NAME = UPPER(N.NAME); END"),
        format!("CREATE TRIGGER {lib}.NEGTRG BEFORE UPDATE ON {lib}.PRICES REFERENCING NEW AS N FOR EACH ROW WHEN (N.PRICE < 0) SIGNAL SQLSTATE '75001' SET MESSAGE_TEXT = 'Price below zero'"),
        format!("UPDATE {lib}.PRICES SET PRICE = PRICE + 1"),
        format!("UPDATE {lib}.PRICES SET NAME = 'uno' WHERE ID = 1"),
        format!("INSERT INTO {lib}.PRICES VALUES (3, 'three', 30.00)"),
    ] {
        cl_cmd(&mut job, sql(s.clone())).await.unwrap_or_else(|e| panic!("{s}: {e}"));
    }
    let rows = |q: String| {
        let (client, url, lib) = (&client, url.clone(), lib.clone());
        async move {
            match as4tainr_core::sql::run_as(
                client,
                &url,
                std::slice::from_ref(&lib),
                "QSECOFR",
                Some(lib.clone()),
                None,
                &q,
            )
            .await
            .unwrap()
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => rows
                    .into_iter()
                    .map(|r| r.iter().map(|v| v.trim()).collect::<Vec<_>>().join("|"))
                    .collect::<Vec<_>>(),
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows(format!(
            "SELECT ID, OLDP, NEWP, WHO FROM {lib}.PRCLOG ORDER BY ID"
        ))
        .await,
        ["1|10.00|11.00|QSECOFR", "2|20.00|21.00|QSECOFR"]
    );
    assert_eq!(
        rows(format!("SELECT NAME FROM {lib}.PRICES WHERE ID = 3")).await,
        ["THREE"]
    );
    let e = cl_cmd(
        &mut job,
        sql(format!("UPDATE {lib}.PRICES SET PRICE = -1 WHERE ID = 2")),
    )
    .await
    .unwrap_err();
    assert!(e.contains("Price below zero"), "{e}");
    let e = cl_cmd(&mut job, sql(format!("CREATE TRIGGER {lib}.NEGTRG AFTER DELETE ON {lib}.PRICES FOR EACH ROW DELETE FROM {lib}.PRCLOG")))
        .await
        .unwrap_err();
    assert!(e.contains("SQL0601"), "{e}");
    cl_cmd(&mut job, sql(format!("DROP TRIGGER {lib}.NEGTRG")))
        .await
        .unwrap();
    cl_cmd(
        &mut job,
        sql(format!("UPDATE {lib}.PRICES SET PRICE = -1 WHERE ID = 2")),
    )
    .await
    .unwrap();
    let e = cl_cmd(&mut job, sql(format!("DROP TRIGGER {lib}.NEGTRG")))
        .await
        .unwrap_err();
    assert!(e.contains("SQL0204"), "{e}");
    // The table replaced, its rows kept in the columns it still has.
    cl_cmd(
        &mut job,
        sql(format!("CREATE OR REPLACE TABLE {lib}.PRCLOG (ID INTEGER, NEWP DECIMAL(7, 2), NOTE VARCHAR(10) DEFAULT 'none') ON REPLACE PRESERVE ROWS")),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(format!(
            "SELECT ID, NEWP, NOTE FROM {lib}.PRCLOG ORDER BY ID, NEWP"
        ))
        .await,
        ["1|11.00|none", "2|-1.00|none", "2|21.00|none"]
    );
}

/// The program message APIs and other job values in RPG. QMHSNDPM (API
/// reference, Send Program Message): the text is the message
/// description's with &1 replaced by the message data (*CHAR without its
/// trailing blanks), put on the call stack entry named; a 4-byte key comes
/// back; with an error code of 16 bytes provided, an unknown message ID is
/// not an exception: bytes available and the exception ID (CPF2419) are
/// returned. QMHRMVPM removes one message by key, or *ALL. ILE RPG
/// Reference: DTAARA(*VAR : field) names the data area by the field's
/// value when the program runs; INZ(*USER) is the job's user profile.
#[tokio::test]
async fn rpg_message_apis_dtaara_var_inz_user() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let src = format!(
        "**FREE
dcl-pr SndPgmMsg extpgm('QMHSNDPM');
  id char(7) const; f char(20) const; d char(10) const; l int(10) const;
  t char(10) const; e char(10) const; c int(10) const; k char(4); err char(16);
end-pr;
dcl-pr RmvPgmMsg extpgm('QMHRMVPM');
  e char(10) const; c int(10) const; k char(4) const; w char(10) const; err char(16);
end-pr;
dcl-ds apierr len(16) qualified;
  prov int(10) pos(1) inz(16);
  avail int(10) pos(5);
  exid char(7) pos(9);
end-ds;
dcl-s key1 char(4);
dcl-s key2 char(4);
dcl-s me char(10) inz(*USER);
dcl-s daname char(21) inz('{lib}/COUNTER');
dcl-s n packed(5:0) dtaara(*var:daname);
SndPgmMsg('TST0001': 'TMSGF     {lib}': 'WIDGET': 10: '*INFO': 'TMSGQ': 0: key1: apierr);
SndPgmMsg('TST0001': 'TMSGF     {lib}': 'GADGET': 10: '*INFO': 'TMSGQ': 0: key2: apierr);
RmvPgmMsg('TMSGQ': 0: key1: '*BYKEY': apierr);
SndPgmMsg('TST9999': 'TMSGF     {lib}': ' ': 0: '*INFO': 'TMSGQ2': 0: key1: apierr);
dsply (apierr.exid + ' ' + %char(apierr.avail > 0));
dsply me;
in *lock n;
n += 1;
out n;
dsply %char(n);
*inlr = *on;
"
    );
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "MSGAPI".into(),
            srctype: "RPGLE".into(),
            text: String::new(),
            source: src,
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    for c in [
        format!("CRTMSGF MSGF({lib}/TMSGF)"),
        format!(
            "ADDMSGD MSGID(TST0001) MSGF({lib}/TMSGF) MSG('Item &1 is short.') FMT((*CHAR 10))"
        ),
        format!("CRTDTAARA DTAARA({lib}/COUNTER) TYPE(*DEC) LEN(5 0) VALUE(41)"),
        format!("CRTBNDRPG PGM({lib}/MSGAPI)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/MSGAPI)"))
            .await
            .unwrap(),
        ["DSPLY  CPF2419 1", "DSPLY  QSECOFR", "DSPLY  42"]
    );
    let key = format!("PID{}", std::process::id());
    let q: Vec<String> = as4tainr_core::pgmmsg::named_messages(&key, "TMSGQ")
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(q, ["Item GADGET is short."]);
    as4tainr_core::pgmmsg::named_remove(&key, "TMSGQ", "*ALL", None);
    assert!(as4tainr_core::pgmmsg::named_messages(&key, "TMSGQ").is_empty());
}

/// A job for the journal and trigger tests, and its SQL.
async fn sql_rows(client: &tokio_postgres::Client, url: &str, lib: &str, q: &str) -> Vec<String> {
    match as4tainr_core::sql::run_as(
        client,
        url,
        &[lib.to_string()],
        "QSECOFR",
        Some(lib.to_string()),
        None,
        q,
    )
    .await
    .unwrap_or_else(|e| panic!("{q}: {e:#}"))
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => rows
            .into_iter()
            .map(|r| r.iter().map(|v| v.trim()).collect::<Vec<_>>().join("|"))
            .collect(),
        other => panic!("unexpected {other:?}"),
    }
}

/// Journals (CL reference: CRTJRNRCV, CRTJRN, STRJRNPF, CHGJRN, DLTJRNRCV,
/// ENDJRNPF; Db2 for i services: DISPLAY_JOURNAL). With IMAGES(*BOTH) an
/// SQL INSERT is an R PT entry, an UPDATE R UB and R UP, a DELETE R DL;
/// starting journaling is F JM, ending it F EJ. The entry data is the
/// record image in the record format's storage formats: packed decimal
/// 10001 in 7 digits is X'0010001F' (digits, sign F), CHAR in CCSID 37
/// ('W' = X'E6', blank = X'40'), INTEGER -5 as 4-byte two's complement
/// X'FFFFFFFB', zoned 12.50 in NUMERIC(5,2) is X'F0F1F2F5F0'; INTERPRET
/// reads them back. Changes under commitment control are a commit cycle:
/// C SC, the changes, C CM (COMMIT) or C RB (ROLLBACK), the cycle number
/// in COMMIT_CYCLE. CHGJRN JRNRCV(*GEN) attaches RCV0002; the default
/// receiver range is the current receiver, *CURCHAIN all of them.
/// Existing objects: CPF7010; journaling a journaled file again is not an
/// error: CPC7031 says 0 of 1 files started (observed on IBM i 7.5).
#[tokio::test]
async fn journals_and_display_journal() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    for c in [
        sql(format!("CREATE TABLE {lib}.ITEMS (ID DECIMAL(7, 0) NOT NULL, NAME CHAR(10), QTY INTEGER, PRICE NUMERIC(5, 2)) RCDFMT ITEMR")),
        format!("CRTJRNRCV JRNRCV({lib}/RCV0001)"),
        format!("CRTJRN JRN({lib}/JRN) JRNRCV({lib}/RCV0001)"),
        format!("STRJRNPF FILE({lib}/ITEMS) JRN({lib}/JRN) IMAGES(*BOTH)"),
        sql(format!("INSERT INTO {lib}.ITEMS VALUES (10001, 'Widget', -5, 12.50)")),
        sql(format!("UPDATE {lib}.ITEMS SET QTY = 7")),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let e = cl_cmd(
        &mut job,
        format!("CRTJRN JRN({lib}/JRN) JRNRCV({lib}/RCV0001)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF7010"), "{e}");
    let m = cl_cmd(
        &mut job,
        format!("STRJRNPF FILE({lib}/ITEMS) JRN({lib}/JRN)"),
    )
    .await
    .unwrap();
    assert!(
        m.len() == 1 && m[0].starts_with("CPC7031: ") && m[0].contains(" 0 of 1 "),
        "{m:?}"
    );
    let dj = format!("TABLE(QSYS2.DISPLAY_JOURNAL('{lib}', 'JRN')) X");
    assert_eq!(
        sql_rows(&client, &url, &lib, &format!("SELECT JOURNAL_CODE, JOURNAL_ENTRY_TYPE, RTRIM(SUBSTR(\"OBJECT\", 1, 10)), COUNT_OR_RRN FROM {dj} ORDER BY SEQUENCE_NUMBER")).await,
        ["F|JM|ITEMS|0", "R|PT|ITEMS|1", "R|UB|ITEMS|1", "R|UP|ITEMS|1"]
    );
    assert_eq!(
        sql_rows(
            &client,
            &url,
            &lib,
            &format!("SELECT HEX(ENTRY_DATA) FROM {dj} WHERE JOURNAL_ENTRY_TYPE = 'PT'")
        )
        .await,
        ["0010001FE689848785A340404040FFFFFFFBF0F1F2F5F0"]
    );
    assert_eq!(
        sql_rows(&client, &url, &lib, &format!(
            "SELECT INTERPRET(SUBSTR(ENTRY_DATA, 1, 4) AS DECIMAL(7, 0)), INTERPRET(SUBSTR(ENTRY_DATA, 5, 10) AS CHAR(10)), \
             INTERPRET(SUBSTR(ENTRY_DATA, 15, 4) AS INTEGER), INTERPRET(SUBSTR(ENTRY_DATA, 19, 5) AS NUMERIC(5, 2)) \
             FROM {dj} WHERE JOURNAL_ENTRY_TYPE = 'UP'"
        )).await,
        ["10001|Widget|7|12.50"]
    );
    // Commitment control: embedded SQL with COMMIT(*CHG).
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let src = format!(
        "**FREE\nexec sql set option commit = *chg;\nexec sql insert into {lib}.ITEMS values (2, 'Two', 2, 2.00);\nexec sql commit;\nexec sql insert into {lib}.ITEMS values (3, 'Three', 3, 3.00);\nexec sql rollback;\n*inlr = *on;\n"
    );
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "CMTPGM".into(),
            srctype: "SQLRPGLE".into(),
            text: String::new(),
            source: src,
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    cl_cmd(
        &mut job,
        format!("CRTSQLRPGI OBJ({lib}/CMTPGM) SRCFILE({lib}/QRPGLESRC)"),
    )
    .await
    .unwrap();
    cl_cmd(&mut job, format!("CALL PGM({lib}/CMTPGM)"))
        .await
        .unwrap();
    let rows = sql_rows(&client, &url, &lib, &format!(
        "SELECT JOURNAL_CODE, JOURNAL_ENTRY_TYPE, COMMIT_CYCLE, COALESCE(PROGRAM_NAME, '-'), SEQUENCE_NUMBER FROM {dj} \
         WHERE SEQUENCE_NUMBER > 4 ORDER BY SEQUENCE_NUMBER"
    )).await;
    let f: Vec<Vec<&str>> = rows.iter().map(|r| r.split('|').collect()).collect();
    // The first cycle: start, the insert, commit; its number is the start's.
    assert_eq!((f[0][0], f[0][1], f[0][2]), ("C", "SC", f[0][4]));
    assert_eq!(
        (f[1][0], f[1][1], f[1][2], f[1][3]),
        ("R", "PT", f[0][4], "CMTPGM")
    );
    assert_eq!((f[2][0], f[2][1], f[2][2]), ("C", "CM", f[0][4]));
    // The second, rolled back: its start, the insert (written when it
    // was made), the record deleted during the rollback (R DR, IBM i
    // "All journal entries by code and type") and the rollback (C RB).
    assert_eq!((f[3][0], f[3][1]), ("C", "SC"));
    assert_eq!((f[4][0], f[4][1], f[4][2]), ("R", "PT", f[3][4]));
    assert_eq!((f[5][0], f[5][1], f[5][2]), ("R", "DR", f[3][4]));
    assert_eq!((f[6][0], f[6][1], f[6][2]), ("C", "RB", f[3][4]));
    assert_eq!(f.len(), 7, "{rows:?}");
    // A new receiver: only its entries by default; *CURCHAIN all.
    cl_cmd(&mut job, format!("CHGJRN JRN({lib}/JRN) JRNRCV(*GEN)"))
        .await
        .unwrap();
    cl_cmd(
        &mut job,
        sql(format!("DELETE FROM {lib}.ITEMS WHERE ID = 2")),
    )
    .await
    .unwrap();
    assert_eq!(
        sql_rows(
            &client,
            &url,
            &lib,
            &format!("SELECT JOURNAL_ENTRY_TYPE, RECEIVER_NAME FROM {dj}")
        )
        .await,
        ["DL|RCV0002"]
    );
    let all = format!(
        "TABLE(QSYS2.DISPLAY_JOURNAL('{lib}', 'JRN', STARTING_RECEIVER_NAME => '*CURCHAIN')) X"
    );
    assert_eq!(
        sql_rows(&client, &url, &lib, &format!("SELECT COUNT(*) FROM {all}")).await,
        ["13"]
    );
    let e = cl_cmd(&mut job, format!("DLTJRNRCV JRNRCV({lib}/RCV0002)"))
        .await
        .unwrap_err();
    assert!(e.contains("attached"), "{e}");
    cl_cmd(&mut job, format!("DLTJRNRCV JRNRCV({lib}/RCV0001)"))
        .await
        .unwrap();
    assert_eq!(
        sql_rows(&client, &url, &lib, &format!("SELECT COUNT(*) FROM {all}")).await,
        ["1"]
    );
    // Ending journaling: F EJ, and changes are no longer journaled.
    cl_cmd(&mut job, format!("ENDJRNPF FILE({lib}/ITEMS)"))
        .await
        .unwrap();
    cl_cmd(&mut job, sql(format!("DELETE FROM {lib}.ITEMS")))
        .await
        .unwrap();
    assert_eq!(
        sql_rows(
            &client,
            &url,
            &lib,
            &format!(
                "SELECT JOURNAL_CODE || JOURNAL_ENTRY_TYPE FROM {dj} ORDER BY SEQUENCE_NUMBER"
            )
        )
        .await,
        ["RDL", "FEJ"]
    );
    let d = cl_cmd(&mut job, format!("DSPJRN JRN({lib}/JRN)"))
        .await
        .unwrap();
    assert!(d[0].contains("ITEMS"), "{d:?}");
}

/// Trigger programs (CL reference: ADDPFTRG, RMVPFTRG; Db2 for i "Trigger
/// buffer sections"): the program gets the trigger buffer and its length.
/// Event '1' insert, '3' update; time '1' after, '2' before; commit lock
/// level '0' without commitment control; the new record image at the new
/// record offset, in the file's format (an EXTNAME data structure BASED
/// on a pointer to it reads it, with PREFIX names). It runs for SQL and
/// for record-level access, in the job's session: it sees the record the
/// change wrote before it is committed (Db2 for i, "Trigger programs":
/// the trigger program runs in the same job and commitment definition as
/// the application). A *BEFORE program that ends in error stops the
/// change (SQL0443; RPG file status 01023, error in the trigger program
/// before the operation); one with ALWREPCHG(*YES) may change the new
/// record, which is written so. An *AFTER program that ends in error
/// without commitment control: "No rollback occurs" (Db2 for i, "Trigger
/// and application programs that are not under commitment control") —
/// the change and the program's own changes stay; under commitment
/// control both are taken back. RMVPFTRG removes the programs. ILE RPG:
/// OPEN of a USROPN file.
#[tokio::test]
async fn trigger_programs_addpftrg() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    // A job of its own: its commitment definition is not another test's.
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("P{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    for c in [
        sql(format!("CREATE TABLE {lib}.ORD (ORDNO DECIMAL(7, 0) NOT NULL, STS CHAR(1)) RCDFMT ORDR")),
        sql(format!("CREATE TABLE {lib}.AUD (ORDNO DECIMAL(7, 0), ACT CHAR(2), CMT CHAR(1), LEN INTEGER, OLDSTS CHAR(1), NEWSTS CHAR(1), SEEN CHAR(1)) RCDFMT AUDR")),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let put = |name: &str, srctype: &str, src: &str| source::Member {
        library: lib.clone(),
        file: "QRPGLESRC".into(),
        name: name.into(),
        srctype: srctype.into(),
        text: String::new(),
        source: src.into(),
        ..Default::default()
    };
    let msg = "dcl-pr SndPgmMsg extpgm('QMHSNDPM');
  id char(7) const; f char(20) const; d char(40) const; l int(10) const;
  t char(10) const; e char(10) const; c int(10) const; k char(4); err char(8);
end-pr;
dcl-s key char(4);
dcl-s err char(8) inz(*allx'00');
";
    // *AFTER: audits the change (and whether the inserted record can be
    // read), then refuses status Y.
    let trg = format!(
        "**FREE
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-f AUD usage(*output) usropn;
dcl-f ORD usage(*input) usropn prefix(R_);
dcl-ds TrgBufT qualified template;
  File char(10) pos(1);
  Lib char(10) pos(11);
  Event char(1) pos(31);
  Time char(1) pos(32);
  Cmt char(1) pos(33);
  OldOff int(10) pos(49);
  NewOff int(10) pos(65);
end-ds;
dcl-pi *n;
  pBuf likeds(TrgBufT);
  pLen int(10);
end-pi;
dcl-ds NewRec extname('ORD') prefix(N_) based(pNew) end-ds;
dcl-ds OldRec extname('ORD') prefix(O_) based(pOld) end-ds;
{msg}
pNew = %addr(pBuf) + pBuf.NewOff;
ORDNO = N_ORDNO;
ACT = pBuf.Event + pBuf.Time;
CMT = pBuf.Cmt;
LEN = pLen;
OLDSTS = ' ';
NEWSTS = N_STS;
SEEN = 'N';
if pBuf.Event = '3';
  pOld = %addr(pBuf) + pBuf.OldOff;
  OLDSTS = O_STS;
endif;
if pBuf.Event = '1';
  open ORD;
  read ORD;
  dow not %eof(ORD);
    if R_ORDNO = N_ORDNO;
      SEEN = 'Y';
    endif;
    read ORD;
  enddo;
  close ORD;
endif;
open AUD;
write AUDR;
close AUD;
if pBuf.Event = '3' and N_STS = 'Y';
  SndPgmMsg('CPF9898': 'QCPFMSG   *LIBL': 'Y is not allowed': 16: '*ESCAPE': '*PGMBDY': 1: key: err);
endif;
*inlr = *on;
"
    );
    // *BEFORE, ALWREPCHG(*YES): refuses status X, makes U a V.
    let btrg = format!(
        "**FREE
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-ds TrgBufT qualified template;
  Time char(1) pos(32);
  NewOff int(10) pos(65);
end-ds;
dcl-pi *n;
  pBuf likeds(TrgBufT);
  pLen int(10);
end-pi;
dcl-ds NewRec extname('ORD') prefix(N_) based(pNew) end-ds;
{msg}
pNew = %addr(pBuf) + pBuf.NewOff;
if pBuf.Time = '2' and N_STS = 'X';
  SndPgmMsg('CPF9898': 'QCPFMSG   *LIBL': 'X is not allowed': 16: '*ESCAPE': '*PGMBDY': 1: key: err);
endif;
if N_STS = 'U';
  N_STS = 'V';
endif;
*inlr = *on;
"
    );
    let upd = "**FREE
dcl-f ORD usage(*update);
read ORDR;
STS = 'X';
update(e) ORDR;
dsply %char(%status(ORD));
*inlr = *on;
";
    let wrt = "**FREE
dcl-f ORD usage(*output);
ORDNO = 102;
STS = 'U';
write ORDR;
*inlr = *on;
";
    let cmtupd = "**FREE
exec sql set option commit = *chg;
exec sql update ORD set STS = 'Y';
dsply %char(sqlcode);
exec sql commit;
*inlr = *on;
";
    for m in [
        put("TRG", "RPGLE", &trg),
        put("BTRG", "RPGLE", &btrg),
        put("UPD", "RPGLE", upd),
        put("WRT", "RPGLE", wrt),
        put("CMTUPD", "SQLRPGLE", cmtupd),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTBNDRPG PGM({lib}/TRG)"),
        format!("CRTBNDRPG PGM({lib}/BTRG)"),
        format!("CRTBNDRPG PGM({lib}/UPD)"),
        format!("CRTBNDRPG PGM({lib}/WRT)"),
        format!("CRTSQLRPGI OBJ({lib}/CMTUPD) SRCFILE({lib}/QRPGLESRC)"),
        format!("ADDPFTRG FILE({lib}/ORD) TRGTIME(*AFTER) TRGEVENT(*INSERT) PGM({lib}/TRG)"),
        format!("ADDPFTRG FILE({lib}/ORD) TRGTIME(*AFTER) TRGEVENT(*UPDATE) PGM({lib}/TRG)"),
        sql(format!("INSERT INTO {lib}.ORD VALUES (101, 'O')")),
        sql(format!("UPDATE {lib}.ORD SET STS = 'S'")),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let e = cl_cmd(
        &mut job,
        format!("ADDPFTRG FILE({lib}/ORD) TRGTIME(*AFTER) TRGEVENT(*INSERT) PGM({lib}/TRG)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("already"), "{e}");
    let aq =
        format!("SELECT ORDNO, ACT, CMT, OLDSTS, NEWSTS, SEEN FROM {lib}.AUD ORDER BY ACT, NEWSTS");
    let aud = || sql_rows(&client, &url, &lib, &aq);
    // The inserted record was there for the program to read.
    assert_eq!(aud().await, ["101|11|0||O|Y", "101|31|0|O|S|N"]);
    let len: i64 = sql_rows(
        &client,
        &url,
        &lib,
        &format!("SELECT MIN(LEN) FROM {lib}.AUD"),
    )
    .await[0]
        .parse()
        .unwrap();
    assert!(
        len > 96,
        "the buffer is the 96-byte static part and the images: {len}"
    );
    let sq = format!("SELECT STS FROM {lib}.ORD");
    let sts = || sql_rows(&client, &url, &lib, &sq);

    // *BEFORE, refused: the change is not made, the *AFTER program is
    // not called.
    cl_cmd(
        &mut job,
        format!(
            "ADDPFTRG FILE({lib}/ORD) TRGTIME(*BEFORE) TRGEVENT(*UPDATE) PGM({lib}/BTRG) ALWREPCHG(*YES)"
        ),
    )
    .await
    .unwrap();
    let e = cl_cmd(&mut job, sql(format!("UPDATE {lib}.ORD SET STS = 'X'")))
        .await
        .unwrap_err();
    assert!(
        e.contains("SQL0443") && e.contains("X is not allowed"),
        "{e}"
    );
    assert_eq!(sts().await, ["S"]);
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/UPD)"))
            .await
            .unwrap(),
        ["DSPLY  1023"]
    );
    assert_eq!(sts().await, ["S"]);
    assert_eq!(aud().await.len(), 2);

    // ALWREPCHG(*YES): the record as the *BEFORE program left it is
    // written; the *AFTER program sees it.
    cl_cmd(&mut job, sql(format!("UPDATE {lib}.ORD SET STS = 'U'")))
        .await
        .unwrap();
    assert_eq!(sts().await, ["V"]);
    assert_eq!(aud().await[2], "101|31|0|S|V|N");

    // *AFTER, refused under commitment control: the change and the
    // program's audit record are taken back.
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/CMTUPD)"))
            .await
            .unwrap(),
        ["DSPLY  -443"]
    );
    assert_eq!(sts().await, ["V"]);
    assert_eq!(aud().await.len(), 3);

    // *AFTER, refused without commitment control: no rollback; the
    // change and the audit record stay.
    let e = cl_cmd(&mut job, sql(format!("UPDATE {lib}.ORD SET STS = 'Y'")))
        .await
        .unwrap_err();
    assert!(
        e.contains("SQL0443") && e.contains("Y is not allowed"),
        "{e}"
    );
    assert_eq!(sts().await, ["Y"]);
    assert_eq!(aud().await.len(), 4);

    // An RPG WRITE with a *BEFORE insert program that changes the
    // record: it is written as the program left it.
    cl_cmd(
        &mut job,
        format!(
            "ADDPFTRG FILE({lib}/ORD) TRGTIME(*BEFORE) TRGEVENT(*INSERT) PGM({lib}/BTRG) ALWREPCHG(*YES)"
        ),
    )
    .await
    .unwrap();
    cl_cmd(&mut job, format!("CALL PGM({lib}/WRT)"))
        .await
        .unwrap();
    assert_eq!(
        sql_rows(
            &client,
            &url,
            &lib,
            &format!("SELECT STS FROM {lib}.ORD WHERE ORDNO = 102")
        )
        .await,
        ["V"]
    );
    assert_eq!(aud().await[1], "102|11|0||V|Y");

    // Removed: changes no longer call them.
    cl_cmd(&mut job, format!("RMVPFTRG FILE({lib}/ORD)"))
        .await
        .unwrap();
    cl_cmd(&mut job, sql(format!("UPDATE {lib}.ORD SET STS = 'X'")))
        .await
        .unwrap();
    assert_eq!(sts().await, ["X", "X"]);
    assert_eq!(aud().await.len(), 5);
}

/// Rolled-back changes are journaled, and journaled changes are applied
/// and removed (IBM i "All journal entries by code and type"; "Actions
/// of applying or removing journaled changes by journal code"; CL
/// reference APYJRNCHG, RMVJRNCHG, CLRPFM). A ROLLBACK of an update
/// leaves the update's entries in the journal and adds BR (the
/// before-image of the record updated for rollback: the image it had)
/// and UR (the after-image: the record put back), then C RB. RMVJRNCHG
/// works from the newest entry back: DL puts the record back, UB updates
/// it to its before-image, UP is ignored; a commit cycle that was not
/// committed is left out (CMTBDY(*YES), the default). CLRPFM journals F
/// CR. APYJRNCHG works from the oldest: PT adds the record at its
/// relative record number, UP updates it, DL deletes it. The commands
/// journal F SR and RC, F SA and AY; *LASTSAVE needs a save of the file
/// in the journal (F MS); with none, CPF9898.
#[tokio::test]
async fn journal_rollback_apply_and_remove() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("A{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "RBK".into(),
            srctype: "SQLRPGLE".into(),
            text: String::new(),
            source: "**FREE
exec sql set option commit = *chg;
exec sql update ITM set NAME = 'Temp' where ID = 1;
exec sql rollback;
*inlr = *on;
"
            .into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    for c in [
        sql(format!(
            "CREATE TABLE {lib}.ITM (ID DECIMAL(5, 0) NOT NULL, NAME CHAR(10)) RCDFMT ITMR"
        )),
        format!("CRTJRNRCV JRNRCV({lib}/RCV0001)"),
        format!("CRTJRN JRN({lib}/JRN) JRNRCV({lib}/RCV0001)"),
        format!("STRJRNPF FILE({lib}/ITM) JRN({lib}/JRN) IMAGES(*BOTH)"),
        sql(format!(
            "INSERT INTO {lib}.ITM VALUES (1, 'One'), (2, 'Two'), (3, 'Three')"
        )),
        sql(format!("UPDATE {lib}.ITM SET NAME = 'Deux' WHERE ID = 2")),
        sql(format!("DELETE FROM {lib}.ITM WHERE ID = 3")),
        format!("CRTSQLRPGI OBJ({lib}/RBK) SRCFILE({lib}/QRPGLESRC)"),
        format!("CALL PGM({lib}/RBK)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let dj = format!("TABLE(QSYS2.DISPLAY_JOURNAL('{lib}', 'JRN')) X");
    let entries = |w: &str| {
        let q = format!(
            "SELECT JOURNAL_CODE || JOURNAL_ENTRY_TYPE, SEQUENCE_NUMBER, COUNT_OR_RRN, \
             COALESCE(INTERPRET(SUBSTR(ENTRY_DATA, 4, 10) AS CHAR(10)), '') FROM {dj} {w} ORDER BY SEQUENCE_NUMBER"
        );
        let (client, url, lib) = (&client, &url, &lib);
        async move {
            sql_rows(client, url, lib, &q)
                .await
                .into_iter()
                .map(|r| {
                    r.split('|')
                        .map(|x| x.trim().to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        }
    };
    let rows = entries("WHERE JOURNAL_CODE IN ('R', 'C')").await;
    let t: Vec<String> = rows.iter().map(|r| r[0].clone()).collect();
    assert_eq!(
        t,
        ["RPT", "RPT", "RPT", "RUB", "RUP", "RDL", "CSC", "RUB", "RUP", "RBR", "RUR", "CRB"]
    );
    // BR: the image the rollback found (Temp); UR: the record put back.
    assert_eq!((rows[9][2].as_str(), rows[9][3].as_str()), ("1", "Temp"));
    assert_eq!((rows[10][2].as_str(), rows[10][3].as_str()), ("1", "One"));
    let names = || {
        let q = format!("SELECT ID, NAME FROM {lib}.ITM ORDER BY ID");
        let (client, url, lib) = (&client, &url, &lib);
        async move { sql_rows(client, url, lib, &q).await }
    };
    assert_eq!(names().await, ["1|One", "2|Deux"]);

    // Remove back to the update of record 2 (its UB).
    let ub = rows[3][1].clone();
    let m = cl_cmd(
        &mut job,
        format!(
            "RMVJRNCHG JRN({lib}/JRN) FILE({lib}/ITM) RCVRNG(*CURRENT) FROMENT(*LAST) TOENT({ub})"
        ),
    )
    .await
    .unwrap();
    assert!(m[0].starts_with("2 entries removed"), "{m:?}");
    assert_eq!(names().await, ["1|One", "2|Two", "3|Three"]);

    // Cleared, then rebuilt from the journal up to the clear.
    cl_cmd(&mut job, format!("CLRPFM FILE({lib}/ITM)"))
        .await
        .unwrap();
    assert!(names().await.is_empty());
    let cr = entries("WHERE JOURNAL_CODE = 'F' AND JOURNAL_ENTRY_TYPE = 'CR'").await;
    let before_clear = cr[0][1].parse::<i64>().unwrap() - 1;
    let e = cl_cmd(
        &mut job,
        format!("APYJRNCHG JRN({lib}/JRN) FILE({lib}/ITM)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF9898") && e.contains("save"), "{e}");
    let m = cl_cmd(
        &mut job,
        format!(
            "APYJRNCHG JRN({lib}/JRN) FILE({lib}/ITM) RCVRNG(*CURRENT) FROMENT(*FIRST) TOENT({before_clear})"
        ),
    )
    .await
    .unwrap();
    assert!(m[0].starts_with("5 entries applied"), "{m:?}");
    assert_eq!(names().await, ["1|One", "2|Deux"]);
    assert_eq!(
        sql_rows(
            &client,
            &url,
            &lib,
            &format!("SELECT RRN(X), ID FROM {lib}.ITM X ORDER BY 1")
        )
        .await,
        ["1|1", "2|2"]
    );
    // New records get new numbers.
    cl_cmd(
        &mut job,
        sql(format!("INSERT INTO {lib}.ITM VALUES (4, 'Four')")),
    )
    .await
    .unwrap();
    let f: Vec<String> = entries("WHERE JOURNAL_CODE = 'F'")
        .await
        .iter()
        .map(|r| r[0].clone())
        .collect();
    assert_eq!(f, ["FJM", "FSR", "FRC", "FCR", "FSA", "FAY"]);
    // The applied changes are not journaled one by one.
    assert_eq!(
        entries("WHERE JOURNAL_CODE = 'R'").await.len(),
        rows.iter().filter(|r| r[0].starts_with('R')).count() + 1
    );
}

/// The decimal places of quotients, products and sums: the values IBM i
/// 7.5 shows (PUB400, tools/golden/rpg-prec.txt, rows P01-P18).
#[tokio::test]
async fn rpg_precision_rules() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "PRECIS",
        include_str!("rpg/precision.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let m: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_start_matches("DSPLY").trim().to_string())
        .collect();
    assert_eq!(
        m,
        [
            "3.3333333333333333333333333333",
            "33.333333333333333333333333333",
            "3.3333333333333333333333333",
            "33.333333333333333333333333",
            "3.33333333333333333333",
            ".11111111111111111111111111111",
            "100.0000",
            "1.9999999999999999999999999999",
            "176.35714285714285714285714",
            "1.23450",
            "-3.50000000000000000000000000000",
        ]
    );
}

/// %EDITC with *ASTFILL, *CURSYM or a currency symbol; %CHAR(date: *MDY-).
#[tokio::test]
async fn rpg_editc_third_argument() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "EDITC3",
        include_str!("rpg/editc3.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let m: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_start_matches("DSPLY").trim_start().to_string())
        .collect();
    // 'J' edits 1234.56 in 9,2 as '    1,234.56 ' (12 positions and the
    // sign position): the four leading blanks become asterisks; the
    // currency symbol takes the place left of the first digit.
    assert_eq!(m, ["****1,234.56", "$1,234.56", "X1,234.56", "10-22-26",]);
}

/// Date and time formats (ILE RPG Reference, "Date Data Type", "Time
/// Data Type", DATFMT and TIMFMT keywords, "Moving Date-Time Data",
/// %CHAR, %DEC, %DATE): a date is held in its format (the definition's
/// DATFMT, else the control specification's, else *ISO), so a *MDY
/// subfield takes 8 bytes; %CHAR without a format and DSPLY give the
/// field's format, an expression's is the control specification's; %DEC
/// gives the digits in the field's format; MOVE takes factor 1 as the
/// format of the character or numeric side (blank: the date field's) and
/// moves the rightmost (MOVE) or leftmost (MOVEL) characters.
#[tokio::test]
async fn rpg_date_and_time_formats() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "DATES",
        include_str!("rpg/dates.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let m: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_start_matches("DSPLY").trim().to_string())
        .collect();
    assert_eq!(
        m,
        [
            "10/21/2026",
            "10/21/26",
            "26/294",
            "21.10.2026",
            "13:45:00",
            "10/21/26ZZ",
            "2110/21/26",
            "10212026",
            "2039-12-25",
            "2026-01-02",
            "134500",
            "02/01/2026",
            "102126",
            "01/02/2027",
        ]
    );
}

/// XML-INTO (ILE RPG Reference, XML-INTO, "%XML options for the XML-INTO
/// operation code", "Path option", "Program Status Codes"): a data
/// structure from the element of its name, subfields from child elements
/// and attributes (names in lower case by default), character data
/// trimmed (trim=all), a numeric attribute, a date, an indicator, an
/// array subfield with its count (countprefix), a LIKEDS subfield; an
/// array from the elements of its name under the outermost element; a
/// path with case=any; an element no subfield takes is status 00353; a
/// document that is not well formed 00351.
#[tokio::test]
async fn rpg_xml_into() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "XMLINTO",
        include_str!("rpg/xmlinto.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let m: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_start_matches("DSPLY").trim().to_string())
        .collect();
    assert_eq!(
        m,
        [
            "17/Ann Lee/2020-02-29/555-2       /2/Oslo",
            "ac",
            "q",
            "mismatch 353",
            "351"
        ]
    );
}

/// LIKEFILE, TEMPLATE and file parameters, EXTFILE (ILE RPG Reference):
/// a file defined like a template file; a file passed to a procedure is
/// the caller's open file; EXTFILE names the file opened, from a
/// variable at open. Halt indicators (ILE RPG Reference, "Halt Indicators
/// (H1-H9)", "Program Status Codes"): a program that ends with H1-H9 on
/// ends abnormally (00232); its caller gets 00231. PSDS positions 288-333
/// (ILE RPG Reference, "Program Status Data Structure"): the date and
/// time the program was created and its source file, library and member.
#[tokio::test]
async fn rpg_likefile_extfile_halt_and_psds() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    cl_cmd(
        &mut job,
        format!("RUNSQL SQL('CREATE TABLE {lib}.CUSTCOPY AS (SELECT * FROM {lib}.CUSTMAST WHERE CUSNO <= 2) WITH DATA') COMMIT(*NONE)"),
    )
    .await
    .unwrap();
    let r = run_rpg(
        &url,
        &lib,
        "QSECOFR",
        "LIKEF",
        include_str!("rpg/likefile.rpgle"),
        None,
        None,
    )
    .await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let m: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_start_matches("DSPLY").trim().to_string())
        .collect();
    assert_eq!(m, ["count 5", "copy 2", "last 2"]);

    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let put = |name: &str, src: &str| source::Member {
        library: lib.clone(),
        file: "QRPGLESRC".into(),
        name: name.into(),
        srctype: "RPGLE".into(),
        text: String::new(),
        source: src.into(),
        ..Default::default()
    };
    let caller = "**FREE
dcl-pr halt extpgm('HALT') end-pr;
monitor;
  halt();
on-error;
  dsply %char(%status);
endmon;
*inlr = *on;
";
    let psds = "**FREE
dcl-ds psds psds;
  lib char(10) pos(81);
  crtdate char(6) pos(288);
  crttime char(6) pos(294);
  srcfile char(10) pos(304);
  srclib char(10) pos(314);
  srcmbr char(10) pos(324);
end-ds;
dsply (lib + srcfile + srclib + srcmbr);
dsply (%char(%len(%trim(crtdate + crttime))));
*inlr = *on;
";
    for m in [
        put("HALT", include_str!("rpg/halt.rpgle")),
        put("HALTC", caller),
        put("PSDSC", psds),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for n in ["HALT", "HALTC", "PSDSC"] {
        cl_cmd(&mut job, format!("CRTBNDRPG PGM({lib}/{n})"))
            .await
            .unwrap();
    }
    let e = cl_cmd(&mut job, format!("CALL PGM({lib}/HALT)"))
        .await
        .unwrap_err();
    assert!(e.contains("RNQ0232") && e.contains("H3"), "{e}");
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/HALTC)"))
            .await
            .unwrap(),
        ["DSPLY  231"]
    );
    let out = cl_cmd(&mut job, format!("CALL PGM({lib}/PSDSC)"))
        .await
        .unwrap();
    assert_eq!(
        out,
        [
            format!("DSPLY  {lib:<10}QRPGLESRC {lib:<10}PSDSC"),
            "DSPLY  12".to_string()
        ]
    );
}

/// Sequences, the SQL/JSON functions, more catalog views and services,
/// and INSTEAD OF triggers (Db2 for i SQL Reference: CREATE SEQUENCE,
/// ALTER SEQUENCE, NEXT VALUE and PREVIOUS VALUE, JSON_OBJECT,
/// JSON_ARRAY, JSON_ARRAYAGG, JSON_OBJECTAGG, JSON_VALUE, JSON_QUERY,
/// JSON_EXISTS, CREATE TRIGGER, catalog views SYSSEQUENCES, SYSCST,
/// SYSKEYCST, SYSREFCST, SYSCHKCST, SYSTRIGGERS; IBM i Services:
/// OBJECT_PRIVILEGES, JOB_INFO, OUTPUT_QUEUE_ENTRIES_BASIC).
///
/// A sequence starts at START WITH and goes by INCREMENT BY; with CYCLE,
/// past MAXVALUE it starts again at MINVALUE, which for an ascending
/// sequence without MINVALUE is the START WITH value; without CYCLE the
/// values run out (SQL0359); PREVIOUS VALUE is the last value NEXT
/// VALUE gave in the job. JSON_OBJECT pairs keys and values (NULL ON
/// NULL by default), JSON_ARRAY leaves nulls out (ABSENT ON NULL by
/// default), FORMAT JSON values are JSON; JSON_VALUE gives the scalar at
/// a path (cast by RETURNING, a DEFAULT ON EMPTY when there is none),
/// JSON_QUERY JSON (WITH ARRAY WRAPPER for several values).
#[tokio::test]
async fn sql_sequences_json_catalog_services_instead_of() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("Q{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };
    let sql = |s: &str| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    let q = |s: String| {
        let (client, url, lib) = (&client, &url, &lib);
        async move { sql_rows(client, url, lib, &s).await }
    };
    for s in [
        format!("CREATE SEQUENCE {lib}.ORDNO START WITH 10 INCREMENT BY 5 MAXVALUE 20 CYCLE"),
        format!("CREATE SEQUENCE {lib}.ONCE AS SMALLINT START WITH 32766"),
    ] {
        cl_cmd(&mut job, sql(&s)).await.unwrap();
    }
    let mut got = Vec::new();
    for _ in 0..4 {
        got.extend(q("VALUES NEXT VALUE FOR ORDNO".into()).await);
    }
    assert_eq!(got, ["10", "15", "20", "10"]);
    assert_eq!(q(format!("VALUES NEXTVAL FOR {lib}/ONCE")).await, ["32766"]);
    assert_eq!(q("VALUES NEXT VALUE FOR ONCE".into()).await, ["32767"]);
    let e = as4tainr_core::sql::run_as(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "QSECOFR",
        Some(lib.clone()),
        None,
        "VALUES NEXT VALUE FOR ONCE",
    )
    .await
    .unwrap_err();
    assert!(format!("{e}").starts_with("SQL0359"), "{e}");
    cl_cmd(&mut job, sql("ALTER SEQUENCE ONCE RESTART"))
        .await
        .unwrap();
    assert_eq!(q("VALUES NEXT VALUE FOR ONCE".into()).await, ["32766"]);
    assert_eq!(
        q(format!(
            "SELECT SEQUENCE_NAME, START, MINIMUM_VALUE, MAXIMUM_VALUE, INCREMENT, CYCLE_OPTION, DATA_TYPE, NUMERIC_PRECISION \
             FROM QSYS2.SYSSEQUENCES WHERE SEQUENCE_SCHEMA = '{lib}' ORDER BY 1"
        ))
        .await,
        [
            "ONCE|32766|32766|32767|1|NO|SMALLINT|5",
            "ORDNO|10|10|20|5|YES|INTEGER|10"
        ]
    );
    // PREVIOUS VALUE in the job's SQL session (embedded SQL).
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "SEQPGM".into(),
            srctype: "SQLRPGLE".into(),
            text: String::new(),
            source: "**FREE
dcl-s a int(10);
dcl-s b int(10);
exec sql set :a = next value for ordno;
exec sql values previous value for ordno into :b;
dsply (%char(a) + ' ' + %char(b));
*inlr = *on;
"
            .into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    cl_cmd(
        &mut job,
        format!("CRTSQLRPGI OBJ({lib}/SEQPGM) SRCFILE({lib}/QRPGLESRC)"),
    )
    .await
    .unwrap();
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/SEQPGM)"))
            .await
            .unwrap(),
        ["DSPLY  15 15"]
    );

    // JSON.
    for s in [
        format!("CREATE TABLE {lib}.EMP (ID INT NOT NULL, NAME VARCHAR(20), DEPT CHAR(2), CONSTRAINT EMP_PK PRIMARY KEY (ID))"),
        format!("INSERT INTO {lib}.EMP VALUES (1, 'Ann', 'A1'), (2, 'Bo', 'B2'), (3, NULL, 'A1')"),
    ] {
        cl_cmd(&mut job, sql(&s)).await.unwrap();
    }
    assert_eq!(
        q(format!(
            "SELECT JSON_OBJECT(KEY 'id' VALUE ID, 'name' VALUE NAME, 'tags' VALUE JSON_ARRAY(DEPT, NULL)) \
             FROM {lib}.EMP ORDER BY ID"
        ))
        .await,
        [
            r#"{"id":1,"name":"Ann","tags":["A1"]}"#,
            r#"{"id":2,"name":"Bo","tags":["B2"]}"#,
            r#"{"id":3,"name":null,"tags":["A1"]}"#
        ]
    );
    assert_eq!(
        q(format!(
            "SELECT JSON_ARRAYAGG(NAME ORDER BY ID DESC) FROM {lib}.EMP"
        ))
        .await,
        [r#"["Bo","Ann"]"#]
    );
    assert_eq!(
        q(format!(
            "SELECT DEPT, JSON_OBJECTAGG(KEY CAST(ID AS VARCHAR(5)) VALUE NAME ABSENT ON NULL) FROM {lib}.EMP GROUP BY DEPT ORDER BY DEPT"
        ))
        .await,
        [r#"A1|{"1":"Ann"}"#, r#"B2|{"2":"Bo"}"#]
    );
    let doc = r#"'{"order":{"no":17,"lines":[{"item":"A","qty":2},{"item":"B","qty":5}]}}'"#;
    assert_eq!(
        q(format!(
            "VALUES (JSON_VALUE({doc}, '$.order.no' RETURNING INTEGER), \
             JSON_VALUE({doc}, '$.order.lines[1].item'), \
             JSON_VALUE({doc}, '$.order.missing' DEFAULT 'none' ON EMPTY), \
             JSON_QUERY({doc}, '$.order.lines[0]'), \
             JSON_QUERY({doc}, '$.order.lines[*].qty' WITH ARRAY WRAPPER))"
        ))
        .await,
        [r#"17|B|none|{"item":"A","qty":2}|[2,5]"#]
    );
    assert_eq!(
        q(format!(
            "SELECT COUNT(*) FROM {lib}.EMP WHERE JSON_EXISTS({doc}, '$.order.lines') AND NOT JSON_EXISTS({doc}, '$.x')"
        ))
        .await,
        ["3"]
    );

    // Constraints in the catalog.
    for s in [
        format!("CREATE TABLE {lib}.SKILL (EMPID INT NOT NULL, CODE CHAR(4) NOT NULL, LVL INT CHECK (LVL BETWEEN 1 AND 5), \
                 CONSTRAINT SKILL_PK PRIMARY KEY (EMPID, CODE), \
                 CONSTRAINT SKILL_EMP FOREIGN KEY (EMPID) REFERENCES {lib}.EMP (ID) ON DELETE CASCADE)"),
    ] {
        cl_cmd(&mut job, sql(&s)).await.unwrap();
    }
    assert_eq!(
        q(format!(
            "SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE, SYSTEM_TABLE_NAME, CONSTRAINT_KEYS, CONSTRAINT_STATE, ENABLED \
             FROM QSYS2.SYSCST WHERE CONSTRAINT_SCHEMA = '{lib}' AND CONSTRAINT_TYPE <> 'CHECK' ORDER BY 1"
        ))
        .await,
        [
            "EMP_PK|PRIMARY KEY|EMP|1|ESTABLISHED|YES",
            "SKILL_EMP|FOREIGN KEY|SKILL|1|ESTABLISHED|YES",
            "SKILL_PK|PRIMARY KEY|SKILL|2|ESTABLISHED|YES"
        ]
    );
    assert_eq!(
        q(format!(
            "SELECT CONSTRAINT_NAME, COLUMN_NAME, ORDINAL_POSITION FROM QSYS2.SYSKEYCST \
             WHERE CONSTRAINT_SCHEMA = '{lib}' AND CONSTRAINT_NAME LIKE 'SKILL%' ORDER BY 1, 3"
        ))
        .await,
        ["SKILL_EMP|EMPID|1", "SKILL_PK|EMPID|1", "SKILL_PK|CODE|2"]
    );
    assert_eq!(
        q(format!(
            "SELECT UNIQUE_CONSTRAINT_NAME, UPDATE_RULE, DELETE_RULE FROM QSYS2.SYSREFCST WHERE CONSTRAINT_SCHEMA = '{lib}'"
        ))
        .await,
        ["EMP_PK|NO ACTION|CASCADE"]
    );
    assert_eq!(
        q(format!(
            "SELECT CHECK_CLAUSE FROM QSYS2.SYSCHKCST WHERE CONSTRAINT_SCHEMA = '{lib}'"
        ))
        .await,
        ["(LVL >= 1) AND (LVL <= 5)"]
    );

    // INSTEAD OF: an insert through a view.
    for s in [
        format!("CREATE VIEW {lib}.EMPV AS SELECT ID, NAME FROM {lib}.EMP"),
        format!(
            "CREATE TRIGGER {lib}.EMPVI INSTEAD OF INSERT ON {lib}.EMPV REFERENCING NEW AS N FOR EACH ROW \
             INSERT INTO {lib}.EMP VALUES (N.ID, UPPER(N.NAME), 'Z9')"
        ),
        format!("INSERT INTO {lib}.EMPV VALUES (9, 'zed')"),
    ] {
        cl_cmd(&mut job, sql(&s)).await.unwrap_or_else(|e| panic!("{s}: {e}"));
    }
    assert_eq!(
        q(format!("SELECT NAME, DEPT FROM {lib}.EMP WHERE ID = 9")).await,
        ["ZED|Z9"]
    );
    let e = cl_cmd(
        &mut job,
        sql(&format!("CREATE TRIGGER {lib}.EMPVA AFTER INSERT ON {lib}.EMPV FOR EACH ROW INSERT INTO {lib}.EMP VALUES (1, 'X', 'X')")),
    )
    .await
    .unwrap_err();
    assert!(e.contains("SQL0156"), "{e}");
    assert_eq!(
        q(format!(
            "SELECT TRIGGER_NAME, EVENT_MANIPULATION, ACTION_TIMING, ACTION_ORIENTATION, EVENT_OBJECT_TABLE, TRIGGER_MODE \
             FROM QSYS2.SYSTRIGGERS WHERE TRIGGER_SCHEMA = '{lib}'"
        ))
        .await,
        ["EMPVI|INSERT|INSTEAD OF|ROW|EMPV|DB2SQL"]
    );

    // Object privileges: the owner, a private authority, *PUBLIC.
    let user = format!("P{}", &lib[1..]);
    let _ = cl_cmd(&mut job, format!("DLTUSRPRF {user}")).await;
    cl_cmd(&mut job, format!("CRTUSRPRF {user} PASSWORD(*NONE)"))
        .await
        .unwrap();
    cl_cmd(
        &mut job,
        format!("GRTOBJAUT OBJ({lib}/EMP) OBJTYPE(*FILE) USER({user}) AUT(*USE)"),
    )
    .await
    .unwrap();
    assert_eq!(
        q(format!(
            "SELECT AUTHORIZATION_NAME, OBJECT_AUTHORITY, OBJECT_OWNER, DATA_READ, DATA_ADD, OBJECT_EXISTENCE, SQL_OBJECT_TYPE \
             FROM QSYS2.OBJECT_PRIVILEGES WHERE SYSTEM_OBJECT_SCHEMA = '{lib}' AND SYSTEM_OBJECT_NAME = 'EMP' ORDER BY 1"
        ))
        .await,
        [
            "*PUBLIC|*CHANGE|NO|YES|YES|NO|TABLE".to_string(),
            format!("{user}|*USE|NO|YES|NO|NO|TABLE"),
            "QSECOFR|*ALL|YES|YES|YES|YES|TABLE".to_string(),
        ]
    );

    // JOB_INFO: the job, active and interactive.
    assert_eq!(
        q(format!(
            "SELECT JOB_STATUS, JOB_TYPE, JOB_TYPE_ENHANCED FROM TABLE(QSYS2.JOB_INFO(JOB_USER_FILTER => '*ALL')) X \
             WHERE JOB_NAME = '{id}'"
        ))
        .await,
        ["ACTIVE|INT|INTERACTIVE"]
    );
    as4tainr_core::jobs::end(&client, &id, true).await.unwrap();
    cl_cmd(&mut job, format!("DLTUSRPRF {user}")).await.unwrap();
}

/// Save and restore (CL reference: CRTSAVF, SAVLIB, SAVOBJ, RSTLIB,
/// RSTOBJ, DLTLIB, CLRSAVF; IBM i "Journal entry information finder": F
/// MS is written when a journaled object is saved and F MR when it is
/// restored; APYJRNCHG FROMENT(*LASTSAVE) applies the entries after the
/// last save). A library saved to a save file, deleted and restored has
/// its files with their records at the same relative record numbers,
/// logical files, SQL views, sequences (continuing after the last value
/// used), SQL triggers, data areas and programs back. RSTOBJ RSTLIB(...)
/// restores an object into another library; restoring a physical file
/// that exists replaces its data (MBROPT(*MATCH)). A restored file that
/// was journaled is journaled again, and the changes journaled after the
/// save are applied with APYJRNCHG's defaults (RCVRNG(*LASTSAVE)
/// FROMENT(*LASTSAVE)). Save files themselves are not saved; one is
/// copied to and from a stream file by its path
/// (/QSYS.LIB/lib.LIB/name.FILE) with CPYTOSTMF and CPYFRMSTMF.
#[tokio::test]
async fn save_and_restore_libraries_and_objects() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (mut client, lib) = setup(&url).await;
    crtlf(
        &mut client,
        &lib,
        "CUSTNAME",
        include_str!("../../samples/qddssrc/CUSTNAME.lf"),
    )
    .await;
    let env = Environment {
        database_url: url.clone(),
    };
    let jl = format!("{lib}J");
    let rl = format!("{lib}R");
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for l in [&jl, &rl] {
        let _ = cl_cmd(&mut job, format!("DLTLIB {l}")).await;
    }
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    source::create_file(&client, &lib, "QCLSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QCLSRC".into(),
            name: "SETDA".into(),
            srctype: "CLLE".into(),
            text: String::new(),
            source: "PGM\n CHGDTAARA DTAARA(*LIBL/DA1) VALUE('ran')\nENDPGM\n".into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    for c in [
        format!("CRTLIB {jl}"),
        format!("CRTLIB {rl}"),
        format!("CRTJRNRCV JRNRCV({jl}/RCV0001)"),
        format!("CRTJRN JRN({jl}/JRN) JRNRCV({jl}/RCV0001)"),
        format!("STRJRNPF FILE({lib}/CUSTMAST) JRN({jl}/JRN) IMAGES(*BOTH)"),
        sql(format!("CREATE VIEW {lib}.OWING AS SELECT CUSNO FROM {lib}.CUSTMAST WHERE CUSBAL > 0")),
        sql(format!("CREATE SEQUENCE {lib}.ORDSEQ START WITH 100")),
        sql(format!(
            "CREATE TABLE {lib}.NOTES (ID INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, TXT VARCHAR(20) NOT NULL, N DECIMAL(5, 0) DEFAULT 0 CHECK (N >= 0))"
        )),
        sql(format!(
            "CREATE TRIGGER {lib}.NOTEUP BEFORE INSERT ON {lib}.NOTES REFERENCING NEW AS X FOR EACH ROW SET X.N = X.N + 1"
        )),
        sql(format!("INSERT INTO {lib}.NOTES (TXT) VALUES ('a'), ('b')")),
        format!("CRTDTAARA DTAARA({lib}/DA1) TYPE(*CHAR) LEN(10) VALUE('saved')"),
        format!("CRTBNDCL PGM({lib}/SETDA) SRCFILE({lib}/QCLSRC)"),
        format!("CRTSAVF FILE({jl}/SV)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let q = |s: String| {
        let (client, url, lib) = (&client, &url, &lib);
        async move { sql_rows(client, url, lib, &s).await }
    };
    assert_eq!(
        q(format!("VALUES NEXT VALUE FOR {lib}.ORDSEQ")).await,
        ["100"]
    );
    let custs = format!("SELECT RRN(C), CUSNO, CUSNAM FROM {lib}.CUSTMAST C ORDER BY CUSNO");
    let before = q(custs.clone()).await;
    assert_eq!(before.len(), 5);

    let m = cl_cmd(
        &mut job,
        format!("SAVLIB LIB({lib}) DEV(*SAVF) SAVF({jl}/SV)"),
    )
    .await
    .unwrap();
    assert!(
        // CPC3722 (observed on IBM i 7.5 for SAVOBJ).
        m[0].starts_with("CPC3722: ")
            && m[0].ends_with(&format!("objects were saved from library {lib}.")),
        "{m:?}"
    );
    // Saving again needs CLEAR (the save file has data).
    assert!(cl_cmd(
        &mut job,
        format!("SAVLIB LIB({lib}) DEV(*SAVF) SAVF({jl}/SV)")
    )
    .await
    .is_err());
    let dj = format!(
        "SELECT JOURNAL_CODE || JOURNAL_ENTRY_TYPE FROM TABLE(QSYS2.DISPLAY_JOURNAL('{jl}', 'JRN')) X \
         WHERE JOURNAL_CODE = 'F' ORDER BY SEQUENCE_NUMBER"
    );
    assert!(q(dj.clone()).await.contains(&"FMS".to_string()));

    // A change after the save, journaled.
    cl_cmd(
        &mut job,
        sql(format!(
            "UPDATE {lib}.CUSTMAST SET CUSNAM = 'Renamed' WHERE CUSNO = 1"
        )),
    )
    .await
    .unwrap();

    cl_cmd(&mut job, format!("DLTLIB {lib}")).await.unwrap();
    assert!(!catalog::library_exists(&client, &lib).await.unwrap());
    let m = cl_cmd(
        &mut job,
        format!("RSTLIB SAVLIB({lib}) DEV(*SAVF) SAVF({jl}/SV)"),
    )
    .await
    .unwrap();
    assert!(
        m[0].ends_with(&format!("objects were restored from {lib} to {lib}.")),
        "{m:?}"
    );

    // The records as saved, at the same relative record numbers.
    assert_eq!(q(custs.clone()).await, before);
    // The logical file, by name.
    let mut by_name = open(&client, &lib, "CUSTNAME").await;
    assert_eq!(
        names(&client, &mut by_name).await,
        [
            "Acme Tooling",
            "Bluebird Supply",
            "Lake, Pine & Co",
            "Midwest Fasteners",
            "Prairie Freight"
        ]
    );
    // The view: balances 1250.50, 310.00, 9999999.99 are over 0.
    assert_eq!(
        q(format!("SELECT CUSNO FROM {lib}.OWING ORDER BY CUSNO")).await,
        ["1", "2", "4"]
    );
    // The sequence continues; the identity column too; the trigger and
    // the check constraint are there.
    assert_eq!(
        q(format!("VALUES NEXT VALUE FOR {lib}.ORDSEQ")).await,
        ["101"]
    );
    cl_cmd(
        &mut job,
        sql(format!("INSERT INTO {lib}.NOTES (TXT) VALUES ('c')")),
    )
    .await
    .unwrap();
    assert_eq!(
        q(format!("SELECT ID, TXT, N FROM {lib}.NOTES ORDER BY ID")).await,
        ["1|a|1", "2|b|1", "3|c|1"]
    );
    assert!(cl_cmd(
        &mut job,
        sql(format!("INSERT INTO {lib}.NOTES (TXT, N) VALUES ('d', -5)"))
    )
    .await
    .is_err());
    // The program runs and changes the restored data area.
    cl_cmd(&mut job, format!("CALL PGM({lib}/SETDA)"))
        .await
        .unwrap();
    let da = as4tainr_core::dtaara::find(&client, std::slice::from_ref(&lib), "DA1", &lib)
        .await
        .unwrap();
    assert_eq!(da.value.trim(), "ran");

    // Journaled again (F MR), and the change after the save applied.
    let f = q(dj.clone()).await;
    assert_eq!(f.last().map(String::as_str), Some("FMR"), "{f:?}");
    let m = cl_cmd(
        &mut job,
        format!("APYJRNCHG JRN({jl}/JRN) FILE({lib}/CUSTMAST)"),
    )
    .await
    .unwrap();
    assert!(m[0].contains("applied to member CUSTMAST"), "{m:?}");
    assert_eq!(
        q(format!("SELECT CUSNAM FROM {lib}.CUSTMAST WHERE CUSNO = 1")).await,
        ["Renamed"]
    );

    // RSTOBJ over an existing physical file: its data as saved.
    cl_cmd(&mut job, sql(format!("DELETE FROM {lib}.CUSTMAST")))
        .await
        .unwrap();
    cl_cmd(
        &mut job,
        format!("RSTOBJ OBJ(CUSTMAST) SAVLIB({lib}) DEV(*SAVF) SAVF({jl}/SV)"),
    )
    .await
    .unwrap();
    assert_eq!(q(custs.clone()).await, before);

    // RSTOBJ into another library.
    let m = cl_cmd(
        &mut job,
        format!("RSTOBJ OBJ(CUST* NOTES) SAVLIB({lib}) DEV(*SAVF) OBJTYPE(*FILE) SAVF({jl}/SV) RSTLIB({rl})"),
    )
    .await
    .unwrap();
    assert!(m[0].starts_with("3 objects were restored"), "{m:?}");
    assert_eq!(
        q(format!("SELECT COUNT(*) FROM {rl}.CUSTMAST")).await,
        ["5"]
    );
    assert_eq!(q(format!("SELECT COUNT(*) FROM {rl}.NOTES")).await, ["2"]);
    let mut by_name = open(&client, &rl, "CUSTNAME").await;
    assert_eq!(names(&client, &mut by_name).await.len(), 5);

    // A save file copied to a stream file and into another save file
    // (CPYTOSTMF and CPYFRMSTMF with a save file path, as for moving a
    // save to another system), then deleted with DLTF.
    let stmf = format!("/tmp/{lib}.savf");
    for c in [
        format!(
            "CPYTOSTMF FROMMBR('/QSYS.LIB/{jl}.LIB/SV.FILE') TOSTMF('{stmf}') STMFOPT(*REPLACE)"
        ),
        format!("CRTSAVF FILE({jl}/SV2)"),
        format!("CPYFRMSTMF FROMSTMF('{stmf}') TOMBR('/QSYS.LIB/{jl}.LIB/SV2.FILE')"),
        format!("RSTOBJ OBJ(DA1) SAVLIB({lib}) DEV(*SAVF) SAVF({jl}/SV2) RSTLIB({rl})"),
        format!("DLTF FILE({jl}/SV2)"),
        format!("RMVLNK OBJLNK('{stmf}')"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let da = as4tainr_core::dtaara::find(&client, std::slice::from_ref(&rl), "DA1", &rl)
        .await
        .unwrap();
    assert_eq!(da.value.trim(), "saved");
    assert!(
        !catalog::object_exists(&client, std::slice::from_ref(&jl), "SV2", "*FILE")
            .await
            .unwrap()
    );

    // An object not in the save file; an empty save file.
    assert!(cl_cmd(
        &mut job,
        format!("RSTOBJ OBJ(NOSUCH) SAVLIB({lib}) DEV(*SAVF) SAVF({jl}/SV)")
    )
    .await
    .unwrap_err()
    .contains("CPF3770"));
    cl_cmd(&mut job, format!("CLRSAVF FILE({jl}/SV)"))
        .await
        .unwrap();
    assert!(cl_cmd(
        &mut job,
        format!("RSTLIB SAVLIB({lib}) DEV(*SAVF) SAVF({jl}/SV)")
    )
    .await
    .is_err());

    for l in [&rl, &jl] {
        cl_cmd(&mut job, format!("DLTLIB {l}")).await.unwrap();
    }
}

/// The PASE `system` utility (`as4tainr system`) and the printed reports
/// of display commands. Expected values: output of `system` on PUB400
/// (IBM i 7.5) over SSH, 2026-10-21 (target/ibmi/golden; message texts
/// are ours, so only message IDs, streams and exit statuses are compared;
/// report lines are IBM's layout with our values).
#[tokio::test]
async fn pase_system_utility_and_printed_reports() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let ids = |l: &[String]| -> Vec<String> {
        l.iter()
            .map(|m| m.split_once(':').map(|x| x.0).unwrap_or(m).to_string())
            .collect()
    };
    let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    macro_rules! sys {
        ($v:expr) => {
            pase_system::run(&mut job, $v)
        };
    }

    // No command: how to use it, on standard error, status 2 (the text is
    // as4tainr's own).
    let o = sys!(&[]).await;
    assert_eq!(o.stderr, [pase_system::USAGE]);
    assert_eq!(o.status, 2);

    // An escape message: standard error, status 255; -n drops the ID; -v
    // shows the command first.
    let chk = format!("CHKOBJ OBJ({lib}/NOSUCH) OBJTYPE(*FILE)");
    let o = sys!(std::slice::from_ref(&chk)).await;
    assert_eq!((ids(&o.stderr), o.status), (a(&["CPF9801"]), 255));
    assert!(o.stdout.is_empty());
    let o = sys!(&["-n".into(), chk.clone()]).await;
    assert!(!o.stderr[0].starts_with("CPF9801"), "{o:?}");
    // -v: one line with the command on standard output (IBM's line frames
    // the command in its own words; ours says it in ours).
    let o = sys!(&["-v".into(), chk.clone()]).await;
    assert_eq!(o.stdout.len(), 1);
    assert!(o.stdout[0].contains(&chk), "{o:?}");

    // Command checking: the diagnostic, then CPF0006.
    let o = sys!(&["NOSUCHCMD".into()]).await;
    assert_eq!(
        (ids(&o.stderr), o.status),
        (a(&["CPD0030", "CPF0006"]), 255)
    );
    let o = sys!(&[format!("CRTDTAARA DTAARA({lib}/X) TYPE(*BAD)")]).await;
    assert_eq!(ids(&o.stderr), a(&["CPD0084", "CPF0006"]));

    // Completion messages on standard output, status 0.
    let o = sys!(&[format!(
        "CRTDTAARA DTAARA({lib}/GOLDDA) TYPE(*CHAR) LEN(10) VALUE(ABC)"
    )])
    .await;
    assert_eq!((ids(&o.stdout), o.status), (a(&["CPC0904"]), 0));

    // DSPDTAARA's printed report (after its page header).
    let o = sys!(&[format!("DSPDTAARA DTAARA({lib}/GOLDDA)")]).await;
    assert_eq!(o.status, 0);
    assert!(
        o.stdout[0].contains("Display Data Area") && o.stdout[0].contains(&format!("{lib}/GOLDDA"))
    );
    assert_eq!(
        o.stdout[1..],
        [
            " Data area . . . . . . . . . . . :   DTAARA      GOLDDA".to_string(),
            format!("   Library . . . . . . . . . . . :                 {lib}"),
            " Type  . . . . . . . . . . . . . :   TYPE        *CHAR".into(),
            " Length  . . . . . . . . . . . . :   LEN         10".into(),
            " Text description  . . . . . . . :   TEXT".into(),
            "    Offset      Value".into(),
            "                 *...+....1....+....2....+....3....+....4....+....5".into(),
            "     0          'ABC       '".into(),
            "                          * * * * *  E N D  O F  L I S T I N G  * * * * *".into(),
        ]
    );
    // -s: no printed output.
    let o = sys!(&["-s".into(), format!("DSPDTAARA DTAARA({lib}/GOLDDA)")]).await;
    assert!(o.stdout.is_empty() && o.status == 0);

    // DLTDTAARA: CPC2191, then CPF2105 the second time.
    let dlt = format!("DLTDTAARA DTAARA({lib}/GOLDDA)");
    assert_eq!(
        ids(&sys!(std::slice::from_ref(&dlt)).await.stdout),
        a(&["CPC2191"])
    );
    let o = sys!(&[dlt]).await;
    assert_eq!((ids(&o.stderr), o.status), (a(&["CPF2105"]), 255));

    // CRTPF RCDLEN: file and member created; DSPFD TYPE(*ATR).
    let o = sys!(&[format!("CRTPF FILE({lib}/GOLDTEST) RCDLEN(80)")]).await;
    assert_eq!(ids(&o.stdout), a(&["CPC7301", "CPC7305"]));
    let o = sys!(&[format!("DSPFD FILE({lib}/GOLDTEST) TYPE(*ATR)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let file_line = format!(
        " File . : GOLDTEST     Library . : {lib:<10}   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001"
    );
    let lines: Vec<&str> = o.stdout.iter().map(String::as_str).collect();
    let want_page1 = [
        "   File  . . . . . . . . . . . : GOLDTEST",
        &format!("     Library . . . . . . . . . : {lib}"),
        "   Type of information . . . . : *ATR",
        "   File attributes . . . . . . : *ALL",
        "   System  . . . . . . . . . . : *LCL",
    ];
    assert_eq!(lines[1..6], want_page1);
    assert_eq!(lines[7], file_line);
    let want = [
        "                                    Data Base File Attributes",
        "   Externally described file . . . . . . . . . :            No",
    ];
    assert_eq!(lines[8..10], want);
    // From "Text 'description'" to the page break (WAITFILE: CRTPF's
    // shipped default *IMMED; PUB400 shows 30, a changed default).
    let want = [
        "   Text 'description'  . . . . . . . . . . . . : TEXT",
        "   Distributed file  . . . . . . . . . . . . . :            No",
        "   Partitioned SQL Table . . . . . . . . . . . :            No",
        "   DBCS capable  . . . . . . . . . . . . . . . :            No",
        "   Maximum members . . . . . . . . . . . . . . : MAXMBRS        1",
        "   Number of constraints . . . . . . . . . . . :                0",
        "   Number of triggers  . . . . . . . . . . . . :                0",
        "   Number of members . . . . . . . . . . . . . :                1",
        "   Member size                                   SIZE",
        "     Initial number of records . . . . . . . . :                 10000",
        "     Increment number of records . . . . . . . :             1000",
        "     Maximum number of increments  . . . . . . :                3",
        "   Record capacity . . . . . . . . . . . . . . :                 13000",
        "   Allocate storage  . . . . . . . . . . . . . : ALLOCATE   *NO",
        "   Contiguous storage  . . . . . . . . . . . . : CONTIG     *NO",
        "   Preferred storage unit  . . . . . . . . . . : UNIT       *ANY",
        "   Records to force a write  . . . . . . . . . : FRCRATIO   *NONE",
        "   Maximum file wait time  . . . . . . . . . . : WAITFILE   *IMMED",
        "   Maximum record wait time  . . . . . . . . . : WAITRCD       60",
        "   Max % deleted records allowed . . . . . . . : DLTPCT     *NONE",
        "   Reuse deleted records . . . . . . . . . . . : REUSEDLT   *NO",
    ];
    assert_eq!(lines[12..33], want);
    assert!(lines[33].contains("Display File Description") && lines[33].ends_with("Page    2"));
    assert_eq!(lines[34], file_line);
    let want = [
        "                                    Data Base File Attributes",
        "   Sort Sequence . . . . . . . . . . . . . . . : SRTSEQ     *HEX",
        "   Language identifier . . . . . . . . . . . . : LANGID     ENU",
        "   Coded character set identifier  . . . . . . : CCSID      65535",
        "   Allow read operation  . . . . . . . . . . . :            Yes",
        "   Allow write operation . . . . . . . . . . . :            Yes",
        "   Allow update operation  . . . . . . . . . . : ALWUPD     *YES",
        "   Allow delete operation  . . . . . . . . . . : ALWDLT     *YES",
        "   Restrict on drop  . . . . . . . . . . . . . :            No",
        "   Record format level check . . . . . . . . . : LVLCHK     *YES",
        "   Access path . . . . . . . . . . . . . . . . :            Arrival",
        "   Maximum record length . . . . . . . . . . . :               80",
        "   Volatile  . . . . . . . . . . . . . . . . . :            No",
        "   Keep in memory  . . . . . . . . . . . . . . : KEEPINMEM  *NO",
        "   File is currently journaled . . . . . . . . :            No",
    ];
    assert_eq!(lines[35..], want);
    let o = sys!(&[format!("DLTF FILE({lib}/GOLDTEST)")]).await;
    assert_eq!(ids(&o.stdout), a(&["CPC2191"]));

    // CHKOBJ of an object that is there: no message, status 0.
    let o = sys!(&[format!("CHKOBJ OBJ({lib}/CUSTMAST) OBJTYPE(*FILE)")]).await;
    assert_eq!(o, pase_system::Output::default());
    let o = sys!(&["CHKOBJ OBJ(NOSUCHLIB/CUSTMAST) OBJTYPE(*FILE)".to_string()]).await;
    assert_eq!(ids(&o.stderr), a(&["CPF9810"]));
    let o = sys!(&[format!(
        "CHKOBJ OBJ({lib}/CUSTMAST) OBJTYPE(*FILE) MBR(NOPE)"
    )])
    .await;
    assert_eq!(ids(&o.stderr), a(&["CPF9815"]));

    // DSPSYSVAL: the layout's column lines (values are ours: shipped).
    let o = sys!(&["DSPSYSVAL SYSVAL(QCCSID)".into()]).await;
    assert_eq!(
        o.stdout[2],
        "                Current                         Shipped"
    );
    assert_eq!(
        o.stdout[3],
        " Name           value                           value                           Description"
    );
    assert!(o.stdout[4].starts_with(
        " QCCSID         65535                           65535                           "
    ));
    assert_eq!(
        o.stdout[5],
        "                                         * * * * *   E N D  O F  L I S T I N G   * * * * *"
    );
    let o = sys!(&["DSPSYSVAL SYSVAL(QNOSUCH)".into()]).await;
    assert_eq!(ids(&o.stderr), a(&["CPF1028"]));

    // DSPOBJD: the column heading and one object's line.
    let o = sys!(&[format!("DSPOBJD OBJ({lib}/CUSTMAST) OBJTYPE(*FILE)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[2].starts_with(&format!(" Library . . . . . . . :   {lib:<10}")));
    assert_eq!(o.stdout[3], "                                                                    Library ASP group . . :   *SYSBAS");
    assert_eq!(
        o.stdout[4],
        " Object         Type         Attribute                  Size     Text"
    );
    assert!(
        o.stdout[5].starts_with(" CUSTMAST       *FILE        PF  "),
        "{:?}",
        o.stdout[5]
    );
}

/// DSPMSGD's printed reports and the CL and ILE RPG compiler listings,
/// as `system` shows them. Expected layouts: PUB400 (IBM i 7.5) output
/// of tools/golden/listings.txt, 2026-10-22 (target/ibmi/golden: m_*,
/// l_*), with IBM's message texts left out (ours differ) and dates and
/// times masked.
#[tokio::test]
async fn compiler_listings_and_dspmsgd_reports() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // Dates and times masked; the library name replaced by LIB.
    let mask = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let mut s = String::new();
                for c in l.replace(&lib, "LIB").chars() {
                    s.push(if c.is_ascii_digit() { '9' } else { c });
                }
                s
            })
            .collect()
    };
    macro_rules! sys {
        ($c:expr) => {
            pase_system::run(&mut job, &[$c.to_string()]).await
        };
    }

    // ------------------------------------------------------- DSPMSGD
    sys!(format!(
        "CRTMSGF MSGF({lib}/GLDMSGF) TEXT('Golden messages')"
    ));
    for a in [
        format!("ADDMSGD MSGID(GLD0001) MSGF({lib}/GLDMSGF) MSG('Order &1 was not found in file &2.') SECLVL('&N Cause . . . . . :   The order number &1 does not exist in file &2. &N Recovery  . . . :   Enter an existing order number, &B or press F3 to end the program. &P Example: 0001234.') SEV(30) FMT((*CHAR 10) (*CHAR 10))"),
        format!("ADDMSGD MSGID(GLD0002) MSGF({lib}/GLDMSGF) MSG('Amount &1 is over the limit.') SEV(20) FMT((*DEC 9 2))"),
        format!("ADDMSGD MSGID(GLD0003) MSGF({lib}/GLDMSGF) MSG('Continue processing (Y N)?') SEV(99) TYPE(*CHAR) LEN(1) VALUES('Y' 'N') DFT('N')"),
        format!("ADDMSGD MSGID(GLD00A1) MSGF({lib}/GLDMSGF) MSG('Count &1 by user &2 in &3.') FMT((*BIN 4) (*CHAR 10) (*CHAR *VARY 2))"),
    ] {
        assert_eq!(
            pase_system::run(&mut job, std::slice::from_ref(&a)).await.status,
            0,
            "{a}"
        );
    }
    let o = sys!(format!("DSPMSGD RANGE(*ALL) MSGF({lib}/GLDMSGF)"));
    assert_eq!(o.status, 0);
    let m = mask(&o.stdout);
    assert_eq!(m[0], "CPC9999: The printed output was spooled.");
    assert!(
        m[1].contains("Message Description (Formatted Text)") && m[1].ends_with("Page       9")
    );
    // m_full.txt lines 3-38 (page 1), then the page 2 header.
    let want = [
        " Message file  . . . . . . . . . :   GLDMSGF       Library . . . . . . . . . . . . :   LIB",
        " Start message ID  . . . . . . . :   *ALL          Ending message ID . . . . . . . :   GLDMSGF",
        " GLD99A9",
        "   Message . . . . :   Count &9 by user &9 in &9.",
        "   Severity . . . . . . . . . . . . . . . . . :    SEV         9",
        "   Message data field formats . . . . . . . . :    FMT",
        "     Field        Data Type          Length            Vary Bytes Or Dec Pos",
        "      &9            *BIN                  9",
        "      &9            *CHAR                99",
        "      &9            *CHAR             *VARY                     9",
        "   Data to be dumped  . . . . . . . . . . . . :    DMPLST     *JOB",
        "   Message level  . . . . . . . . . . . . . . :    LVL        99/99/99 99",
        "   Alert option . . . . . . . . . . . . . . . :    ALROPT     *NO",
        "   Log problem  . . . . . . . . . . . . . . . :    LOGPRB     *NO",
        "   Coded character set ID . . . . . . . . . . :    CCSID      99999",
        " GLD9999",
        "   Message . . . . :   Order &9 was not found in file &9.",
        "   Cause . . . . . :   The order number &9 does not exist in file &9.",
        "   Recovery  . . . :   Enter an existing order number,",
        "     or press F9 to end the program.",
        "       Example: 9999999.",
        "   Severity . . . . . . . . . . . . . . . . . :    SEV        99",
    ];
    assert_eq!(&m[2..2 + want.len()], &want);
    assert!(
        m[38].contains("Message Description (Formatted Text)") && m[38].ends_with("Page       9")
    );
    assert_eq!(
        m[39],
        "      &9            *DEC                  9                     9"
    );
    let tail = [
        "   Reply specifications:",
        "     Reply type . . . . . . . . . . . . . . . :    TYPE       *CHAR",
        "     Reply length . . . . . . . . . . . . . . :    LEN            9",
        "     Valid reply values . . . . . . . . . . . :    VALUES",
        "        Y                                 N",
        "     Default reply  . . . . . . . . . . . . . :    DFT        N",
    ];
    let k = m.iter().position(|l| l == tail[0]).unwrap();
    assert_eq!(&m[k..k + tail.len()], &tail);
    assert_eq!(
        m.last().unwrap(),
        "                        * * * * *  E N D  O F  L I S T I N G * * * * *"
    );
    // DETAIL(*BASIC) with one identifier: from it to the end, "*ONLY".
    let o = sys!(format!(
        "DSPMSGD RANGE(GLD0002) MSGF({lib}/GLDMSGF) DETAIL(*BASIC)"
    ));
    let m = mask(&o.stdout);
    assert_eq!(
        &m[2..],
        [
            " Message file  . . . . . . . . . :   GLDMSGF       Library . . . . . . . . . . . . :   LIB",
            " Start message ID  . . . . . . . :   GLD9999       Ending message ID . . . . . . . :   *ONLY",
            " MSGID   SEV   MSG",
            " GLD9999  99   Amount &9 is over the limit.",
            " GLD9999  99   Continue processing (Y N)?",
            "                        * * * * *  E N D  O F  L I S T I N G * * * * *",
        ]
    );
    // FMTTXT(*NO): the texts in pieces of 76.
    let o = sys!(format!(
        "DSPMSGD RANGE(GLD0001) MSGF({lib}/GLDMSGF) FMTTXT(*NO)"
    ));
    let m = mask(&o.stdout);
    assert_eq!(m[3], " Message IDs:          GLD9999");
    assert_eq!(
        &m[5..10],
        [
            "   First level message text . . . . . . . . . :    MSG",
            "     Order &9 was not found in file &9.",
            "   Second level message text  . . . . . . . . :    SECLVL",
            "     &N Cause . . . . . :   The order number &9 does not exist in file &9. &N Rec",
            "     overy  . . . :   Enter an existing order number, &B or press F9 to end the p",
        ]
    );
    // A missing identifier: CPF2419, then CPF2519; a missing file: CPF2407.
    let ids = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|l| l.split(':').next().unwrap_or("").to_string())
            .collect()
    };
    let o = sys!(format!("DSPMSGD RANGE(GLD9999) MSGF({lib}/GLDMSGF)"));
    assert_eq!(
        (ids(&o.stderr), o.status),
        (vec!["CPF2419".to_string(), "CPF2519".into()], 255)
    );
    let o = sys!(format!("DSPMSGD RANGE(*ALL) MSGF({lib}/NOSUCHMSGF)"));
    assert_eq!(ids(&o.stderr), ["CPF2407"]);

    // ---------------------------------------------------- CL listing
    let put = |path: &str, text: &str| {
        let (c, p, t) = (&client, path.to_string(), text.to_string());
        async move {
            as4tainr_core::ifs::write_file(c, &p, t.as_bytes(), 819, 0o644, "QSECOFR")
                .await
                .unwrap();
        }
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    put(&format!("{dir}/gldlst.clle"), "/* Golden compiler listing (tools/golden/listings.txt). */\nPGM        PARM(&NAME)\n  DCL        VAR(&NAME) TYPE(*CHAR) LEN(10)\n  DCL        VAR(&COUNT) TYPE(*DEC) LEN(5 0) VALUE(0)\n  DCL        VAR(&TEXT) TYPE(*CHAR) LEN(40)\n  CHGVAR     VAR(&COUNT) VALUE(&COUNT + 1)\n  CHGVAR     VAR(&TEXT) VALUE('Hello ' *CAT &NAME)\n  SNDPGMMSG  MSG(&TEXT)\n  MONMSG     MSGID(CPF0000)\nENDPGM\n").await;
    put(&format!("{dir}/gldlste.clle"), "/* Golden compiler listing with errors (tools/golden/listings.txt). */\nPGM\n  DCL        VAR(&A) TYPE(*CHAR) LEN(5)\n  CHGVAR     VAR(&B) VALUE('X')\n  NOSUCHCMD  PARM(1)\nENDPGM\n").await;
    let o = sys!(format!(
        "CRTBNDCL PGM({lib}/GLDLCL) SRCSTMF('{dir}/gldlst.clle')"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    let m = mask(&o.stdout);
    assert!(m[0].starts_with("CPC9999:"));
    assert!(
        m[1].contains("Control Language")
            && m[1].contains("LIB/GLDLCL")
            && m[1].ends_with("Page    9")
    );
    for l in [
        " Program . . . . . . . . . . . . . . . . . . . :   GLDLCL",
        " Source printing options . . . . . . . . . . . :   *XREF  *NOSECLVL  *NOEVENTF  *NODOSLTLVL",
        " SEQNBR  *...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+... 9 ...+.  DATE",
        "    999-   DCL        VAR(&COUNT) TYPE(*DEC) LEN(9 9) VALUE(9)",
        "   9999- ENDPGM",
        " &COUNT                         999      *DEC                9 9         999    999",
        " &NAME                          999      *CHAR              99           999    999",
        " Total         9-9  99-99  99-99  99-99  99-99  99-99  99-99  99-99  99-99  99-99",
        "      9          9      9      9      9      9      9      9      9      9      9",
    ] {
        assert!(m.iter().any(|x| x == l), "{l}\n{}", m.join("\n"));
    }
    let o = sys!(format!(
        "CRTBNDCL PGM({lib}/GLDLCLE) SRCSTMF('{dir}/gldlste.clle')"
    ));
    assert_eq!(
        (ids(&o.stderr), o.status),
        (vec!["CPF0820".to_string()], 255)
    );
    let ids_in = |v: &[String]| -> Vec<String> {
        v.iter()
            .filter(|l| l.starts_with("* "))
            .map(|l| l[2..9].to_string())
            .collect()
    };
    // Under the statements: CPD0727 (40), CPD0030 (30); in the cross
    // reference: CPD0726 (10), CPD0791 (00).
    assert_eq!(
        ids_in(&o.stdout),
        ["CPD0727", "CPD0030", "CPD0726", "CPD0791"]
    );
    let m = mask(&o.stdout);
    assert!(m.iter().any(|l| l.starts_with("* CPD9999 99  ")));
    assert!(m.contains(
        &"      9          9      9      9      9      9      9      9      9      9      9"
            .to_string()
    ));
    let counts: Vec<&str> = o
        .stdout
        .iter()
        .find(|l| l.starts_with("      4"))
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        counts,
        ["4", "1", "1", "0", "1", "1", "0", "0", "0", "0", "0"]
    );

    // --------------------------------------------------- RPG listing
    let o = sys!(format!("RUNSQL SQL('CREATE TABLE {lib}/GLDLSTF (ID CHAR(10) NOT NULL, AMT DEC(7, 2) NOT NULL) RCDFMT GLDLSTR') COMMIT(*NONE)"));
    assert_eq!(o.status, 0, "{o:?}");
    put(
        &format!("{dir}/gldlst.rpgle"),
        include_str!("../../tools/golden/lst/gldlst.rpgle"),
    )
    .await;
    put(
        &format!("{dir}/gldlste.rpgle"),
        include_str!("../../tools/golden/lst/gldlste.rpgle"),
    )
    .await;
    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLDLST) SRCSTMF('{dir}/gldlst.rpgle')"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].starts_with("RNS9304:") && o.stdout[0].contains("00"));
    let m = mask(&o.stdout);
    for l in [
        "  Compiler options . . . . . . . . :   *XREF      *GEN       *NOSECLVL  *SHOWCPY",
        "                                       *EXPDDS    *EXT       *NOSHOWSKP *NOSRCSTMT",
        "Line   <---------------------- Source Specifications ----------------------------------------------------->  Do  Change Src Seq",
        "    99 for i = 9 to 9;                                                                                      B99             999999",
        "    99   write GLDLSTR;                                                                                      99             999999",
        "    99 endfor;                                                                                              E99             999999",
        "        *  Default activation group . . . . :   *NO                          *",
        "        *  Activation group . . . . . . . . :   *NEW                         *",
        "    99=OGLDLSTR                                                                                                            9999999",
        "    99=O                       ID                  99A CHAR       99                                                       9999999",
        "    99=O                       AMT                 99P PACK      9,9                                                       9999999",
        "    99    99    99 AMT",
        "         GLDLSTF           DISK                    9D",
        "           GLDLSTR                                 9D     99      99",
        "         AMT               P(9,9)                 99M     99      99",
        "         TOTALS            DS(9)                   9D     99M     99M     99",
        "           COUNT           I(99,9)                99D     99      99",
        "         LR                                       99M",
        "    Records . . . . . . . . . . . . :       99",
        "    Specifications  . . . . . . . . :       99",
        "    Comments  . . . . . . . . . . . :        9",
    ] {
        assert!(m.iter().any(|x| x == l), "{l}\n{}", m.join("\n"));
    }
    // The file box ends in column 102.
    assert!(o.stdout.iter().any(|l| l
        == &format!(
            "        * File name. . . . . . . . . :  GLDLSTF          {:<44}*",
            format!("{lib}/GLDLSTF")
        )));
    // RNF7086 (00) for the output-only file at line 7.
    assert!(o
        .stdout
        .iter()
        .any(|l| l.starts_with("*RNF7086 00      7 000007  ")));
    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLDLSTE) SRCSTMF('{dir}/gldlste.rpgle')"
    ));
    assert_eq!(
        (ids(&o.stderr), o.status),
        (vec!["RNS9308".to_string(), "RNS9310".into()], 255)
    );
    let diags: Vec<&str> = o
        .stdout
        .iter()
        .filter(|l| l.starts_with("*RNF") && l.contains("00000"))
        .map(|l| &l[..25])
        .collect();
    assert_eq!(
        diags,
        [
            "*RNF7030 30      7 000007",
            "*RNF7503 30      7 000007",
            "*RNF7416 30      8 000008"
        ]
    );
    assert!(o
        .stdout
        .iter()
        .any(|l| l.starts_with("*RNF7030 NOSUCH            **UNDEF**               7")));
    // OUTPUT(*NONE): no listing.
    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLDLST) SRCSTMF('{dir}/gldlst.rpgle') OUTPUT(*NONE)"
    ));
    assert_eq!(o.stdout.len(), 1, "{o:?}");
}

/// More compiler listings: a module (CRTRPGMOD), /COPY members, an update
/// file's generated input and output specifications, fixed-form RPG IV,
/// the SQL precompiler (CRTSQLRPGI), a CL module (CRTCLMOD) and RPG/400
/// (CRTRPGPGM). Expected layouts: PUB400 (IBM i 7.5) output of
/// tools/golden/listings2.txt, 2026-10-22 (target/ibmi/golden: k_*),
/// with dates, times and IBM's message texts left out.
#[tokio::test]
async fn more_compiler_listings() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let mask = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let mut s = String::new();
                for c in l.replace(&lib, "LIB").chars() {
                    s.push(if c.is_ascii_digit() { '9' } else { c });
                }
                s
            })
            .collect()
    };
    let has = |m: &[String], l: &str| {
        assert!(m.iter().any(|x| x == l), "missing: {l}\n{}", m.join("\n"));
    };
    let dir = format!("/tmp/{lib}x");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, text) in [
        (
            "gldcpy.rpgle",
            include_str!("../../tools/golden/lst2/gldcpy.rpgle"),
        ),
        (
            "gldmod.rpgle",
            include_str!("../../tools/golden/lst2/gldmod.rpgle"),
        ),
        (
            "gldupd.rpgle",
            include_str!("../../tools/golden/lst2/gldupd.rpgle"),
        ),
        (
            "gldfix.rpgle",
            include_str!("../../tools/golden/lst2/gldfix.rpgle"),
        ),
        (
            "gldsql.sqlrpgle",
            include_str!("../../tools/golden/lst2/gldsql.sqlrpgle"),
        ),
        (
            "gldclm.clle",
            include_str!("../../tools/golden/lst2/gldclm.clle"),
        ),
        (
            "gldold.rpg",
            include_str!("../../tools/golden/lst2/gldold.rpg"),
        ),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for c in [
        format!("RUNSQL SQL('CREATE TABLE {lib}/GLDLSTF (ID CHAR(10) NOT NULL, AMT DEC(7, 2) NOT NULL) RCDFMT GLDLSTR') COMMIT(*NONE)"),
        format!("CRTSRCPF FILE({lib}/GLDLSRC) RCDLEN(112)"),
        format!("CPYFRMSTMF FROMSTMF('{dir}/gldcpy.rpgle') TOMBR('/QSYS.LIB/{lib}.LIB/GLDLSRC.FILE/GLDCPY.MBR') MBROPT(*REPLACE) STMFCCSID(819)"),
        format!("CPYFRMSTMF FROMSTMF('{dir}/gldfix.rpgle') TOMBR('/QSYS.LIB/{lib}.LIB/GLDLSRC.FILE/GLDFIX.MBR') MBROPT(*REPLACE) STMFCCSID(819)"),
        format!("CPYFRMSTMF FROMSTMF('{dir}/gldold.rpg') TOMBR('/QSYS.LIB/{lib}.LIB/GLDLSRC.FILE/GLDOLD.MBR') MBROPT(*REPLACE) STMFCCSID(819)"),
        format!("CHGPFM FILE({lib}/GLDLSRC) MBR(GLDOLD) SRCTYPE(RPG)"),
    ] {
        let o = pase_system::run(&mut job, std::slice::from_ref(&c)).await;
        assert_eq!(o.status, 0, "{c}: {o:?}");
    }
    macro_rules! sys {
        ($c:expr) => {
            pase_system::run(&mut job, &[$c.to_string()]).await
        };
    }

    // A module: RNS9305, its prologue, the copy member's box and lines,
    // the /COPY section, a prototype defined in the copy (9D).
    let o = sys!(format!(
        "CRTRPGMOD MODULE({lib}/GLDMOD) SRCSTMF('{dir}/gldmod.rpgle')"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].starts_with("RNS9305:"));
    let m = mask(&o.stdout);
    for l in [
        "  Command  . . . . . . . . . . . . :   CRTRPGMOD",
        "  Module . . . . . . . . . . . . . :   GLDMOD",
        "  Replace module . . . . . . . . . :   *YES",
        "  Storage model . . .  . . . . . . :   *INHERIT",
        "  Binding directory  . . . . . . . :   *NONE",
        "     9 /copy GLDLSRC,GLDCPY                                                                                                 999999",
        "Line   <---------------------- Source Specifications ----------------------------><---- Comments ----> Do  Page  Change Src Seq",
        "        * RPG member name  . . . . . :  GLDCPY                                                       *                     9",
        "     9+**FREE                                                                                                    999999    9999999",
        "     9+dcl-c GREETING 'Hello';                                                                                   999999    9999999",
        "                            / C o p y   M e m b e r s",
        "         GLDMSG            A(99)                   9D     99",
        "         GREETING          CONST                   9D     99",
        "         TEXT              A(99)                  99D     99M     99",
    ] {
        has(&m, l);
    }
    // No compiler options box in a module's listing.
    assert!(!m.iter().any(|l| l.contains("Compiler Options in Effect")));

    // An update file: input specifications before the calculations, output
    // specifications after them, each group with its own source ID; names
    // defined and not used: RNF7031; prototypes bound statically.
    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLDUPD) SRCSTMF('{dir}/gldupd.rpgle')"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    let m = mask(&o.stdout);
    for l in [
        "    99=IGLDLSTR                                                                                                            9999999",
        "    99=I                             A    9   99  ID                                                                       9999999",
        "    99=I                             P   99   99 9AMT                                                                      9999999",
        "    99=OGLDLSTR                                                                                                            9999999",
        "*RNF9999 GLDMSG            A(99)                   9D",
        "*RNF9999 GREETING          CONST                   9D",
        "         ID                A(99)                  99M     99",
        "         GLDMSG                                    9",
        "    Information  (99) . . . . . . . :        9",
    ] {
        has(&m, l);
    }
    assert!(o
        .stdout
        .iter()
        .any(|l| l.starts_with("*RNF7031 00      2 ")));

    // Fixed form: columns 6-100 of the source, resulting indicators with
    // "--" for the ones not used, the Do column; indicators set: D.
    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLDFIX) SRCFILE({lib}/GLDLSRC) SRCMBR(GLDFIX)"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    let m = mask(&o.stdout);
    for l in [
        "     9 C                   READ      GLDLSTR                            ----99                                   999999     999999",
        "    99 C                   DOW       *IN99 = *OFF                                                      B99       999999     999999",
        "    99 C                   SETON                                        LR----                                   999999     999999",
        "         99                                        9D     99      99D",
        "         LR                                       99D",
        "*RNF9999 ID                A(99)                   9D",
        "    Specifications  . . . . . . . . :       99",
    ] {
        has(&m, l);
    }

    // CL module: CPC0816, its prologue, the labels it defines and uses.
    let o = sys!(format!(
        "CRTCLMOD MODULE({lib}/GLDCLM) SRCSTMF('{dir}/gldclm.clle')"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].starts_with("CPC0816:"));
    let m = mask(&o.stdout);
    for l in [
        " Module  . . . . . . . . . . . . . . . . . . . :   GLDCLM",
        " Source printing options . . . . . . . . . . . :   *XREF  *GEN  *NOSECLVL  *NOEVENTF  *NODOSLTLVL",
        " Replace module object . . . . . . . . . . . . :   *YES",
        " Label            Defined     References",
        " LOOP                999         999",
        "     9           9      9      9      9      9      9      9      9      9      9",
    ] {
        has(&m, l);
    }

    // SQL precompiler: its options, the source, the cross reference of
    // host variables, tables and columns; then the compiler's listing.
    let o = sys!(format!("CRTSQLRPGI OBJ({lib}/GLDSQL) SRCSTMF('{dir}/gldsql.sqlrpgle') OBJTYPE(*PGM) COMMIT(*NONE) OUTPUT(*PRINT)"));
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].starts_with("RNS9304:"));
    let m = mask(&o.stdout);
    for l in [
        "Source type...............RPG",
        "To source file............QTEMP/QSQLTEMP9",
        "Commit....................*NONE",
        "Concurrent access",
        "  resolution..............*DFT",
        "    9   exec sql select count(*) into :n from GLDLSTF;",
        "AMT                              9        DECIMAL(9,9) COLUMN (NOT NULL) IN GLDLSTF",
        "GLDLSTF                          ****     TABLE",
        "N                                9        INTEGER PRECISION(9,9)",
        "No errors found in source",
        "    9 Source records processed",
        "  Command  . . . . . . . . . . . . :   CRTBNDRPG",
    ] {
        has(&m, l);
    }

    // RPG/400: QRG0003, its source with indicator usage and DO levels,
    // the input fields, the cross reference.
    let o = sys!(format!(
        "CRTRPGPGM PGM({lib}/GLDOLD) SRCFILE({lib}/GLDLSRC) SRCMBR(GLDOLD)"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].starts_with("QRG0003:"));
    let m = mask(&o.stdout);
    for l in [
        "           H                                                                                              *****",
        "  A999999                                           9  99 ID",
        "  A999999                                       P  99  999AMT",
        "      999  C                     READ GLDLSTR                  99                    9",
        "      999  C           *IN99     DOWEQ'9'                                               B999",
        "      999  C                     SETON                     LR                    9",
        "        TOTAL       P(9,9)      999D     999",
        "* 9999  ID          A(99)   A999999D",
        "        '9'         LITERAL     999",
        "        99             999M     999      999M",
    ] {
        has(&m, l);
    }
}

/// Screens round 4 (IBM i 7.5 on PUB400, tools/golden/screens4.txt and
/// tools/golden/files-and-programs.txt, captures in target/ibmi not kept):
/// DSPFFD's report, printed and in the spooled file viewer; PDM's lists
/// (WRKMBRPDM, WRKOBJPDM) and WRKMSGD laid out at IBM's columns; RMVM;
/// DLTF of a source file.
#[tokio::test]
async fn screens_round4_reports_and_pdm() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}r4");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/gldclm.clle"),
        include_str!("../../tools/golden/lst2/gldclm.clle").as_bytes(),
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    macro_rules! sys {
        ($c:expr) => {
            pase_system::run(&mut job, &[$c.to_string()]).await
        };
    }
    for c in [
        format!("CRTSRCPF FILE({lib}/SCR4SRC) RCDLEN(112)"),
        format!("CPYFRMSTMF FROMSTMF('{dir}/gldclm.clle') TOMBR('/QSYS.LIB/{lib}.LIB/SCR4SRC.FILE/SCR4CL.MBR') MBROPT(*REPLACE) STMFCCSID(819)"),
        format!("CHGPFM FILE({lib}/SCR4SRC) MBR(SCR4CL) SRCTYPE(CLLE) TEXT('Screens round 4')"),
        format!("RUNSQL SQL('CREATE TABLE {lib}/GLDPF (CODE DECIMAL(5, 0) NOT NULL, NAME CHAR(20) NOT NULL) RCDFMT GLDR') COMMIT(*NONE)"),
    ] {
        let o = pase_system::run(&mut job, std::slice::from_ref(&c)).await;
        assert_eq!(o.status, 0, "{c}: {o:?}");
    }
    let has = |m: &[String], l: &str| {
        assert!(m.iter().any(|x| x == l), "missing: {l:?}\n{}", m.join("\n"));
    };

    // DSPFFD of a source file (screens4 24_dspffd_down): SRCSEQ, SRCDAT,
    // SRCDTA in IBM's columns, the record length from CRTSRCPF RCDLEN.
    let o = sys!(format!("DSPFFD FILE({lib}/SCR4SRC)"));
    assert_eq!(o.status, 0, "{o:?}");
    let first = &o.stdout[0];
    assert_eq!(
        first.find("Display File Field Description"),
        Some(29),
        "{first}"
    );
    assert!(first.ends_with("Page    1"), "{first}");
    for l in [
        " Input parameters",
        " Field Level Information",
        "              Data        Field  Buffer    Buffer        Field    Column",
        "   Field      Type       Length  Length  Position        Usage    Heading",
        "   SRCSEQ     ZONED        6  2       6         1        Both",
        "   SRCDAT     ZONED        6  0       6         7        Both",
        "   SRCDTA     CHAR          100     100        13        Both",
        "   Record length . . . . . . . . . . . . . . . :    112",
        "   Number of fields  . . . . . . . . . . . . . :      3",
        "   Externally described  . . . . . . . . . . . :  Yes",
        "   Type of file  . . . . . . . . . . . . . . . :  Physical",
        &format!("     Library . . . . . . . . . . . . . . . . . :  {lib}"),
    ] {
        has(&o.stdout, l);
    }
    // A database file (files-and-programs f_dspffd): the column heading is
    // the field name; a CCSID line follows each character field.
    let o = sys!(format!("DSPFFD FILE({lib}/GLDPF)"));
    assert_eq!(o.status, 0, "{o:?}");
    for l in [
        "   Record format . . . . . . . . . . . . . . . :  GLDR",
        "   Record length . . . . . . . . . . . . . . . :     23",
        "   CODE       PACKED       5  0       3         1        Both     CODE",
        "   NAME       CHAR           20      20         4        Both     NAME",
    ] {
        has(&o.stdout, l);
    }
    let ccsid = o
        .stdout
        .iter()
        .find(|l| l.contains("Coded Character Set Identifier"))
        .unwrap();
    assert_eq!(ccsid.find(':'), Some(47), "{ccsid}");
    assert_eq!(ccsid.len(), 55, "{ccsid}");
    // On a display: the Display Spooled File viewer, file QPDSPFFD, the
    // title in column 30 instead of the page header.
    match job.run(&format!("DSPFFD FILE({lib}/GLDPF)")).await.unwrap() {
        Outcome::Display(d) => {
            let v = d.spool.expect("spooled file viewer");
            assert_eq!(v.file, "QPDSPFFD");
            assert_eq!(
                v.lines[0].2.find("Display File Field Description"),
                Some(29)
            );
            assert_eq!(v.lines[1].2, " Input parameters");
        }
        o => panic!("{o:?}"),
    }

    // WRKMBRPDM (screens4 15_wrkmbrpdm): title in column 28, the system
    // name in 72, header fields at 3,22 4,24 4,70; options at IBM's
    // columns; members in 7, types in 19, texts in 31.
    match job.run(&format!("WRKMBRPDM {lib}/SCR4SRC")).await.unwrap() {
        Outcome::Display(d) => {
            let l = d.ibm.expect("IBM layout");
            assert_eq!(d.title, "Work with Members Using PDM");
            assert_eq!(l.title_col, Some(28));
            assert_eq!(l.system_col1, Some(72));
            let at: Vec<(u8, u8, u16)> = l
                .header_fields
                .iter()
                .map(|f| (f.row, f.col, f.len))
                .collect();
            assert_eq!(at, vec![(3, 22, 10), (4, 24, 10), (4, 70, 10)]);
            // Shown from column 2: "4=Delete" in column 26, "14=Compile" in 50.
            assert_eq!(l.option_text[0][0].find("4=Delete"), Some(24));
            assert_eq!(l.option_text[0][0].find("5=Display"), Some(33));
            assert_eq!(l.option_text[0][1].find("14=Compile"), Some(48));
            assert_eq!(l.option_text[0][1].find("15=Create module"), Some(60));
            assert_eq!(l.key_text[0].find("F5=Refresh"), Some(39));
            assert_eq!(
                d.rows,
                vec!["SCR4CL      CLLE        Screens round 4".to_string()]
            );
            let opts = &l.row_options[0];
            assert!(opts.contains(&(
                14,
                format!("SBMJOB CMD(CRTBNDCL PGM({lib}/SCR4CL) SRCFILE({lib}/SCR4SRC) SRCMBR(SCR4CL)) JOB(SCR4CL)")
            )));
            assert!(opts.contains(&(4, format!("RMVM FILE({lib}/SCR4SRC) MBR(SCR4CL)"))));
        }
        o => panic!("{o:?}"),
    }
    // WRKOBJPDM (screens4 18_wrkobjpdm): programs before files, a source
    // file's attribute PF-SRC, a data file's PF-DTA.
    assert_eq!(
        sys!(format!("CRTBNDCL PGM({lib}/SCR4CL) SRCFILE({lib}/SCR4SRC)")).status,
        0
    );
    match job.run(&format!("WRKOBJPDM {lib}")).await.unwrap() {
        Outcome::Display(d) => {
            let rows: Vec<String> = d
                .rows
                .iter()
                .filter(|r| r.starts_with("SCR4") || r.starts_with("GLDPF"))
                .cloned()
                .collect();
            assert_eq!(
                rows,
                vec![
                    "SCR4CL      *PGM        CLLE        Screens round 4".to_string(),
                    "GLDPF       *FILE       PF-DTA".to_string(),
                    "SCR4SRC     *FILE       PF-SRC".to_string(),
                ]
            );
            let l = d.ibm.unwrap();
            assert_eq!(l.title_col, Some(27));
            assert_eq!(l.option_text[0][0].find("4=Delete"), Some(31));
            assert_eq!(l.option_text[0][0].find("5=Display"), Some(45));
            assert_eq!(l.option_text[0][1].find("8=Display description"), Some(2));
        }
        o => panic!("{o:?}"),
    }

    // WRKMSGD (screens4 28_wrkmsgd): ID in column 8, severity ending in
    // 23, text from 29; the list starts at MSGID.
    for c in [
        format!("CRTMSGF MSGF({lib}/SCR4MSGF)"),
        format!("ADDMSGD MSGID(USR0001) MSGF({lib}/SCR4MSGF) MSG('First') SEV(20)"),
        format!("ADDMSGD MSGID(USR0002) MSGF({lib}/SCR4MSGF) MSG('Second') SEV(40)"),
    ] {
        assert_eq!(
            pase_system::run(&mut job, std::slice::from_ref(&c))
                .await
                .status,
            0,
            "{c}"
        );
    }
    match job
        .run(&format!("WRKMSGD MSGF({lib}/SCR4MSGF) MSGID(USR0002)"))
        .await
        .unwrap()
    {
        Outcome::Display(d) => {
            // Rows are shown from column 7.
            assert_eq!(d.rows[1], " USR0002       40     Second");
            assert_eq!(d.rows[1].find("USR0002").map(|i| i + 7), Some(8));
            assert_eq!(d.rows[1].find("40").map(|i| i + 7), Some(22));
            assert_eq!(d.rows[1].find("Second").map(|i| i + 7), Some(29));
            let l = d.ibm.unwrap();
            assert_eq!(l.start_at, 1);
            assert_eq!(l.system_col, Some(62));
            assert_eq!(l.option_text[0][0].find("4=Delete"), Some(13));
            assert_eq!(l.key_text[0].find("F12=Cancel"), Some(32));
        }
        o => panic!("{o:?}"),
    }

    // RMVM removes a member; DLTF deletes a source file (screens4
    // 31_dltf: "Object SCR4SRC in AS4TRAINR1 type *FILE deleted.").
    let o = sys!(format!("RMVM FILE({lib}/SCR4SRC) MBR(SCR4CL)"));
    assert_eq!(o.status, 0, "{o:?}");
    let o = sys!(format!("DSPPFM FILE({lib}/SCR4SRC) MBR(SCR4CL)"));
    assert_ne!(o.status, 0, "{o:?}");
    let o = sys!(format!("DLTF FILE({lib}/SCR4SRC)"));
    assert_eq!(o.status, 0, "{o:?}");
    // DSPFFD of a file that is not there: CPF3012 (PUB400 2026-10-26,
    // tools/golden/verify2.txt o_atr_ffd; this test expected CPF9812,
    // which no capture showed).
    let o = sys!(format!("DSPFFD FILE({lib}/SCR4SRC)"));
    assert!(
        o.stderr.join("\n").contains("CPF3012") || o.stdout.join("\n").contains("CPF3012"),
        "{o:?}"
    );
}

/// Data queues (IBM documentation: CRTDTAQ; Object APIs, "Send Data
/// Queue (QSNDDTAQ)", "Receive Data Queue (QRCVDTAQ)", "Clear Data Queue
/// (QCLRDTAQ)", "Retrieve Data Queue Description (QMHQRDQD)"). Expected
/// values from those pages: *FIFO and *LIFO order; a keyed receive with
/// LE of entries GGG, XXX, AAA, GGG and key XXX returns AAA, GGG (the
/// first), GGG (the fourth), XXX (QRCVDTAQ's example); a wait of 0 on an
/// empty queue returns length 0; Length of data is the entry's length
/// even when the size of data receiver cuts it; remove *NO leaves the
/// entry; sender information: two PACKED(7,0) byte counts then job name,
/// user, job number, current user (44 bytes); CPF2498 for data longer
/// than MAXLEN, CPF9502 for a key on a queue without keys, CPF9501 for
/// no key on a keyed one (returned in the error code: bytes available
/// 15, then the ID at offset 8); RDQD0100: message length at 8, key
/// length at 12, sequence at 16, sender ID at 17, number of messages at
/// 72.
#[tokio::test]
async fn data_queues_and_their_apis() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let pgm = format!(
        "PGM
  DCL &LEN *DEC (5 0)
  DCL &DATA *CHAR 20
  DCL &KEY *CHAR 3
  DCL &SND *CHAR 44
  DCL &ERR *CHAR 16 VALUE(X'00000010')
  DCL &RCV *CHAR 120
  DCL &RL *CHAR 4
  DCL &N *DEC (5 0)
  DCL &T *CHAR 5
  CRTDTAQ DTAQ({lib}/FIFOQ) MAXLEN(20) TEXT('First in')
  CRTDTAQ DTAQ({lib}/LIFOQ) MAXLEN(20) SEQ(*LIFO)
  CRTDTAQ DTAQ({lib}/KEYQ) MAXLEN(20) SEQ(*KEYED) KEYLEN(3) SENDERID(*YES)
  CALL QSNDDTAQ PARM('FIFOQ' '{lib}' 5 'FIRST')
  CALL QSNDDTAQ PARM('FIFOQ' '*LIBL' 6 'SECOND')
  CALL QRCVDTAQ PARM('FIFOQ' '{lib}' &LEN &DATA 0)
  CHGVAR &T &LEN
  SNDPGMMSG MSG('Q: FIFO ' *CAT &DATA *TCAT ' ' *CAT &T)
  CHGVAR &DATA ' '
  CALL QRCVDTAQ PARM('FIFOQ' '{lib}' &LEN &DATA 0 '  ' 0 ' ' 0 ' ' '*YES' 3 &ERR)
  CHGVAR &T &LEN
  SNDPGMMSG MSG('Q: CUT ' *CAT &DATA *TCAT ' ' *CAT &T)
  CHGVAR &DATA 'UNCHANGED'
  CALL QRCVDTAQ PARM('FIFOQ' '{lib}' &LEN &DATA 0)
  CHGVAR &T &LEN
  SNDPGMMSG MSG('Q: EMPTY ' *CAT &DATA *TCAT ' ' *CAT &T)
  CHGVAR &DATA ' '
  CALL QSNDDTAQ PARM('LIFOQ' '{lib}' 1 'A')
  CALL QSNDDTAQ PARM('LIFOQ' '{lib}' 1 'B')
  CALL QRCVDTAQ PARM('LIFOQ' '{lib}' &LEN &DATA 0 '  ' 0 ' ' 0 ' ' '*NO' 20 &ERR)
  SNDPGMMSG MSG('Q: PEEK ' *CAT &DATA)
  CALL QRCVDTAQ PARM('LIFOQ' '{lib}' &LEN &DATA 0)
  SNDPGMMSG MSG('Q: LIFO ' *CAT &DATA)
  CALL QRCVDTAQ PARM('LIFOQ' '{lib}' &LEN &DATA 0)
  SNDPGMMSG MSG('Q: LIFO ' *CAT &DATA)
  CALL QSNDDTAQ PARM('FIFOQ' '{lib}' 21 'TOO LONG')
  MONMSG CPF2498 EXEC(SNDPGMMSG MSG('Q: CPF2498'))
  CALL QSNDDTAQ PARM('FIFOQ' '{lib}' 1 'X' 3 'AAA')
  MONMSG CPF9502 EXEC(SNDPGMMSG MSG('Q: CPF9502'))
  CALL QSNDDTAQ PARM('KEYQ' '{lib}' 2 'E1' 3 'GGG')
  CALL QSNDDTAQ PARM('KEYQ' '{lib}' 2 'E2' 3 'XXX')
  CALL QSNDDTAQ PARM('KEYQ' '{lib}' 2 'E3' 3 'AAA')
  CALL QSNDDTAQ PARM('KEYQ' '{lib}' 2 'E4' 3 'GGG')
  CALL QRCVDTAQ PARM('KEYQ' '{lib}' &LEN &DATA 0 '  ' 0 ' ' 0 ' ' '*YES' 20 &ERR)
  SNDPGMMSG MSG('Q: ERR ' *CAT %SST(&ERR 9 7))
  CHGVAR &N %BIN(&ERR 5 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('Q: AVAIL ' *CAT &T)
  CALL QMHQRDQD PARM(&RCV X'00000078' 'RDQD0100' 'KEYQ      *LIBL     ')
  CHGVAR &N %BIN(&RCV 73 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('Q: DESC ' *CAT %SST(&RCV 17 2) *CAT ' ' *CAT &T)
  CHGVAR &N %BIN(&RCV 9 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('Q: MAXLEN ' *CAT &T)
  CHGVAR &N %BIN(&RCV 13 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('Q: KEYLEN ' *CAT &T)
  CHGVAR &KEY 'XXX'
  CALL QRCVDTAQ PARM('KEYQ' '{lib}' &LEN &DATA 0 'LE' 3 &KEY 44 &SND)
  SNDPGMMSG MSG('Q: KEY ' *CAT &KEY *CAT ' ' *CAT %SST(&DATA 1 2) *CAT ' ' *CAT %SST(&SND 19 10))
  CHGVAR &KEY 'XXX'
  CALL QRCVDTAQ PARM('KEYQ' '{lib}' &LEN &DATA 0 'LE' 3 &KEY 0 &SND)
  SNDPGMMSG MSG('Q: KEY ' *CAT &KEY *CAT ' ' *CAT %SST(&DATA 1 2))
  CALL QCLRDTAQ PARM('KEYQ' '{lib}' 'EQ' 3 'GGG' &ERR)
  CHGVAR &KEY 'XXX'
  CALL QRCVDTAQ PARM('KEYQ' '{lib}' &LEN &DATA 0 'LE' 3 &KEY 0 &SND)
  SNDPGMMSG MSG('Q: KEY ' *CAT &KEY *CAT ' ' *CAT %SST(&DATA 1 2))
  CALL QRCVDTAQ PARM('KEYQ' '{lib}' &LEN &DATA 0 'XX' 3 &KEY 0 &SND)
  MONMSG CPF9504 EXEC(SNDPGMMSG MSG('Q: CPF9504'))
  CALL QRCVDTAQ PARM('NOQ' '{lib}' &LEN &DATA 0)
  MONMSG CPF9801 EXEC(SNDPGMMSG MSG('Q: CPF9801'))
  DLTDTAQ DTAQ({lib}/LIFOQ)
ENDPGM"
    );
    let m = cl_job_run(&client, &url, &lib, &[("DQTEST", &pgm)], "CALL DQTEST")
        .await
        .unwrap();
    // (CHGVAR of a *DEC (5 0) into a *CHAR 5 gives the digits with
    // leading zeros, CL reference CHGVAR.)
    let q: Vec<&str> = m.iter().filter_map(|x| x.strip_prefix("Q: ")).collect();
    assert_eq!(
        q,
        [
            "FIFO FIRST 00005",
            "CUT SEC 00006",
            "EMPTY UNCHANGED 00000",
            "PEEK B",
            "LIFO B",
            "LIFO A",
            "CPF2498",
            "CPF9502",
            "ERR CPF9501",
            "AVAIL 00015",
            "DESC KY 00004",
            "MAXLEN 00020",
            "KEYLEN 00003",
            "KEY AAA E3 QSECOFR",
            "KEY GGG E1",
            "KEY XXX E2",
            "CPF9504",
            "CPF9801",
        ]
    );
    assert!(m
        .iter()
        .any(|x| x == &format!("Data queue KEYQ was created in library {lib}.")));
    assert!(m.iter().any(
        |x| x == &format!("CPC2191: Object LIFOQ of type *DTAQ in library {lib} was deleted.")
    ));
}

/// An RPG program waits on a data queue (QRCVDTAQ wait time 10: up to
/// ten seconds) for an entry another job sends a second later, and
/// answers on a second queue; QRCVDTAQ with an error code structure
/// gets the exception ID in it (ERRC0100) instead of an escape message.
/// IBM documentation: Object APIs, Receive Data Queue and Send Data
/// Queue; "Error code parameter".
#[tokio::test]
async fn rpg_waits_on_a_data_queue() {
    use as4tainr_core::dtaq::{call_api, Caller, ClArgs};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    for q in ["WAITQ", "REPLYQ"] {
        as4tainr_core::dtaq::create(
            &client,
            &as4tainr_core::dtaq::Create {
                library: &lib,
                name: q,
                maxlen: 20,
                seq: 'F',
                keylen: 0,
                senderid: false,
                force: false,
                autorcl: false,
                size_max: -1,
                size_init: 16,
                text: "",
            },
        )
        .await
        .unwrap();
    }
    let src = format!(
        "**FREE
dcl-pr QSNDDTAQ extpgm('QSNDDTAQ');
  name char(10) const;
  lib char(10) const;
  len packed(5:0) const;
  data char(32767) const options(*varsize);
end-pr;
dcl-pr QRCVDTAQ extpgm('QRCVDTAQ');
  name char(10) const;
  lib char(10) const;
  len packed(5:0);
  data char(32767) options(*varsize);
  wait packed(5:0) const;
  keyord char(2) const options(*nopass);
  keylen packed(3:0) const options(*nopass);
  key char(256) options(*varsize:*nopass);
  sndlen packed(3:0) const options(*nopass);
  sndinf char(44) options(*varsize:*nopass);
  remove char(10) const options(*nopass);
  size packed(5:0) const options(*nopass);
  error likeds(errc) options(*nopass);
end-pr;
dcl-ds errc qualified;
  provided int(10) inz(%size(errc));
  available int(10);
  id char(7);
  *n char(1);
end-ds;
dcl-s len packed(5:0);
dcl-s data char(20);
dcl-s key char(1);
dcl-s snd char(1);
QRCVDTAQ('WAITQ' : '{lib}' : len : data : 10);
dsply ('got ' + %trim(data) + ' ' + %char(len));
QSNDDTAQ('REPLYQ' : '{lib}' : 4 : 'PONG');
QRCVDTAQ('NOSUCHQ' : '{lib}' : len : data : 0 : ' ' : 0 : key : 0 : snd
         : '*YES' : 20 : errc);
dsply ('error ' + errc.id + ' ' + %char(errc.available));
*inlr = *on;
"
    );
    let sender = {
        let url = url.clone();
        let lib = lib.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let c = runtime::connect_db(&url).await.unwrap();
            let caller = Caller {
                user: "QSECOFR".into(),
                libl: vec![lib.clone()],
                ..Default::default()
            };
            let mut a = ClArgs(vec!["WAITQ".into(), lib.clone(), "4".into(), "PING".into()]);
            call_api(&c, &caller, "QSNDDTAQ", &mut a).await.unwrap();
        })
    };
    let start = std::time::Instant::now();
    let r = run_rpg(&url, &lib, "QSECOFR", "DQWAIT", &src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    sender.await.unwrap();
    assert!(start.elapsed() >= std::time::Duration::from_millis(1400));
    assert_eq!(r.messages, ["got PING 4", "error CPF9801 15"]);
    // The answer is on REPLYQ.
    let caller = Caller {
        user: "QSECOFR".into(),
        libl: vec![lib.clone()],
        ..Default::default()
    };
    let mut a = ClArgs(vec![
        "REPLYQ".into(),
        lib.clone(),
        "0".into(),
        " ".repeat(20),
        "0".into(),
    ]);
    call_api(&client, &caller, "QRCVDTAQ", &mut a)
        .await
        .unwrap();
    assert_eq!((a.0[2].as_str(), a.0[3].trim()), ("4", "PONG"));
}

/// User spaces (IBM documentation, Object APIs, "User Space APIs":
/// QUSCRTUS, QUSCHGUS, QUSRTVUS, QUSRUSAT, QUSCUSAT, QUSPTRUS,
/// QUSDLTUS). Expected values from those pages: every byte starts as the
/// initial value; starting positions are 1-based; SPCA0100 has the space
/// size at offset 8, automatic extendibility ('0' or '1') at 12, the
/// initial value at 13 and the library at 14; a second QUSCRTUS with
/// replace *NO fails with CPF9870 (in the error code here); a deleted
/// space is not found (CPF9801). A program changes the space in place
/// through QUSPTRUS's pointer, and QUSRTVUS then sees the change.
#[tokio::test]
async fn user_spaces_and_their_apis() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let pgm = format!(
        "PGM
  DCL &ERR *CHAR 16 VALUE(X'00000010')
  DCL &RCV *CHAR 24
  DCL &D *CHAR 10
  DCL &N *DEC (9 0)
  DCL &T *CHAR 9
  DCL &RL *CHAR 10
  CALL QUSCRTUS PARM('SPACE1    {lib:<10}' 'TEST' X'00000064' '*' '*USE' 'A test space')
  CALL QUSCHGUS PARM('SPACE1    *LIBL     ' X'00000005' X'00000003' 'ABC' '0')
  CALL QUSRTVUS PARM('SPACE1    {lib:<10}' X'00000001' X'0000000A' &D)
  SNDPGMMSG MSG('S: RTV ' *CAT &D)
  CALL QUSRUSAT PARM(&RCV X'00000018' 'SPCA0100' 'SPACE1    *LIBL     ' &ERR)
  CHGVAR &N %BIN(&RCV 9 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('S: SIZE ' *CAT &T *CAT ' ' *CAT %SST(&RCV 13 2) *CAT ' ' *CAT %SST(&RCV 15 10))
  CALL QUSCHGUS PARM('SPACE1    *LIBL     ' X'00000063' X'00000004' 'WXYZ' '0' &ERR)
  SNDPGMMSG MSG('S: PAST END ' *CAT %SST(&ERR 9 7))
  CALL QUSCUSAT PARM(&RL 'SPACE1    *LIBL     ' X'000000010000000300000001F1' &ERR)
  CALL QUSCHGUS PARM('SPACE1    *LIBL     ' X'00000063' X'00000004' 'WXYZ' '0')
  CALL QUSRUSAT PARM(&RCV X'00000018' 'SPCA0100' 'SPACE1    *LIBL     ' &ERR)
  CHGVAR &N %BIN(&RCV 9 4)
  CHGVAR &T &N
  SNDPGMMSG MSG('S: GREW ' *CAT &T *CAT ' ' *CAT %SST(&RCV 13 1) *CAT ' ' *CAT &RL)
  CALL QUSCRTUS PARM('SPACE1    {lib:<10}' 'TEST' X'00000064' ' ' '*USE' 'Again' '*NO' &ERR)
  SNDPGMMSG MSG('S: DUP ' *CAT %SST(&ERR 9 7))
  CALL QUSDLTUS PARM('SPACE1    {lib:<10}' &ERR)
  CALL QUSRTVUS PARM('SPACE1    {lib:<10}' X'00000001' X'0000000A' &D)
  MONMSG CPF9801 EXEC(SNDPGMMSG MSG('S: CPF9801'))
ENDPGM"
    );
    let m = cl_job_run(&client, &url, &lib, &[("USTEST", &pgm)], "CALL USTEST")
        .await
        .unwrap();
    let s: Vec<&str> = m.iter().filter_map(|x| x.strip_prefix("S: ")).collect();
    assert_eq!(
        s,
        [
            "RTV ****ABC***",
            &format!("SIZE 000000100 0* {lib}"),
            "PAST END CPF3CF2",
            &format!("GREW 000000102 1 {lib}"),
            "DUP CPF9870",
            "CPF9801",
        ]
    );

    // QUSPTRUS: an RPG program changes the space through the pointer.
    let src = format!(
        "**FREE
dcl-pr QUSCRTUS extpgm('QUSCRTUS');
  name char(20) const;
  attr char(10) const;
  size int(10) const;
  init char(1) const;
  aut char(10) const;
  text char(50) const;
end-pr;
dcl-pr QUSPTRUS extpgm('QUSPTRUS');
  name char(20) const;
  ptr pointer;
end-pr;
dcl-pr QUSRTVUS extpgm('QUSRTVUS');
  name char(20) const;
  start int(10) const;
  len int(10) const;
  data char(32767) options(*varsize);
end-pr;
dcl-s p pointer;
dcl-ds space based(p);
  head char(4);
  count int(10);
end-ds;
dcl-s back char(8);
QUSCRTUS('SPACE2    {lib}' : 'LIST' : 64 : x'00' : '*ALL' : 'Pointer test');
QUSPTRUS('SPACE2    {lib}' : p);
head = 'LIST';
count = 7;
QUSRTVUS('SPACE2    {lib}' : 1 : 8 : back);
if %subst(back : 5 : 4) = x'00000007';
  dsply (%subst(back : 1 : 4) + ' 7');
endif;
*inlr = *on;
"
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "USPTR", &src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["LIST 7"]);
}

/// The list APIs into a user space (IBM documentation: "List Database
/// File Members (QUSLMBR)", "List Objects (QUSLOBJ)" and the generic
/// header of list APIs): an RPG program lists a source file's members
/// (MBRL0200: name, source type, dates, text) and a library's data
/// queues (OBJL0100: name, library, type), then walks the list through
/// QUSPTRUS's pointer: the list data's offset at 124, the number of
/// entries at 132 and the size of each at 136; information status 'C'
/// (complete) at 103; the user area (0-63) is left as it was; the space
/// (100 bytes) grows to hold the list.
#[tokio::test]
async fn list_apis_into_user_spaces() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    as4tainr_core::source::create_file(&client, &lib, "QLSTSRC", "")
        .await
        .unwrap();
    for (m, t, x) in [("BETA", "CLLE", "Second"), ("ALPHA", "RPGLE", "First")] {
        as4tainr_core::source::put_member(
            &client,
            &as4tainr_core::source::Member {
                library: lib.clone(),
                file: "QLSTSRC".into(),
                name: m.into(),
                srctype: t.into(),
                text: x.into(),
                source: "x".into(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    }
    for q in ["DQ1", "DQ2"] {
        as4tainr_core::dtaq::create(
            &client,
            &as4tainr_core::dtaq::Create {
                library: &lib,
                name: q,
                maxlen: 10,
                seq: 'F',
                keylen: 0,
                senderid: false,
                force: false,
                autorcl: false,
                size_max: -1,
                size_init: 16,
                text: "",
            },
        )
        .await
        .unwrap();
    }
    let src = format!(
        "**FREE
dcl-pr QUSCRTUS extpgm('QUSCRTUS');
  name char(20) const;
  attr char(10) const;
  size int(10) const;
  init char(1) const;
  aut char(10) const;
  text char(50) const;
end-pr;
dcl-pr QUSCHGUS extpgm('QUSCHGUS');
  name char(20) const;
  start int(10) const;
  len int(10) const;
  data char(64) const;
  frc char(1) const;
end-pr;
dcl-pr QUSPTRUS extpgm('QUSPTRUS');
  name char(20) const;
  ptr pointer;
end-pr;
dcl-pr QUSLMBR extpgm('QUSLMBR');
  space char(20) const;
  format char(8) const;
  file char(20) const;
  mbr char(10) const;
  ovr char(1) const;
end-pr;
dcl-pr QUSLOBJ extpgm('QUSLOBJ');
  space char(20) const;
  format char(8) const;
  obj char(20) const;
  type char(10) const;
end-pr;
dcl-s p pointer;
dcl-s e pointer;
dcl-ds hdr based(p);
  user char(64);
  *n char(39);
  status char(1);
  *n char(20);
  listoff int(10) pos(125);
  listsize int(10);
  count int(10);
  entsize int(10);
end-ds;
dcl-ds mbr based(e);
  name char(10);
  type char(10);
  created char(13);
  changed char(13);
  text char(50);
end-ds;
dcl-ds obj based(e);
  oname char(10);
  olib char(10);
  otype char(10);
end-ds;
dcl-s i int(10);
QUSCRTUS('LIST      {lib}' : 'LIST' : 100 : x'00' : '*ALL' : 'Lists');
QUSCHGUS('LIST      {lib}' : 1 : 64 : 'MY USER AREA' : '0');
QUSLMBR('LIST      {lib}' : 'MBRL0200' : 'QLSTSRC   {lib}' : '*ALL' : '0');
QUSPTRUS('LIST      {lib}' : p);
dsply (status + ' ' + %char(count) + ' ' + %char(entsize) + ' ' + %subst(user : 1 : 12));
for i = 0 to count - 1;
  e = p + listoff + i * entsize;
  dsply (%trim(name) + ' ' + %trim(type) + ' ' + %trim(text) + ' ' + %subst(created : 1 : 1));
endfor;
QUSLOBJ('LIST      {lib}' : 'OBJL0100' : '*ALL      {lib}' : '*DTAQ');
dsply (%char(count) + ' ' + %char(entsize));
for i = 0 to count - 1;
  e = p + listoff + i * entsize;
  dsply (%trim(oname) + ' ' + %trim(otype));
endfor;
*inlr = *on;
"
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "LISTS", &src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "C 2 96 MY USER AREA",
            "ALPHA RPGLE First 1",
            "BETA CLLE Second 1",
            "2 30",
            "DQ1 *DTAQ",
            "DQ2 *DTAQ",
        ]
    );
}

/// The retrieve and list APIs (IBM documentation: "Retrieve Object
/// Description (QUSROBJD)" OBJD0200, "Retrieve Member Description
/// (QUSRMBRD)" MBRD0200, "List Fields (QUSLFLD)" FLDL0100, "List Spooled
/// Files (QUSLSPL)" SPLF0100 and "Retrieve Spooled File Attributes
/// (QUSRSPLA)" SPLA0100). Expected values from those pages' formats: the
/// return library (for *LIBL, and for a library named too: iRPGUnit's
/// Object_resolveLibrary relies on it on real systems), domain *S, the owner; the file
/// attribute PF, source file '1', the member's text and records; each
/// field's data type, use B, buffer positions, length, digits and
/// decimals; a spooled file found by its internal identifiers from
/// QUSLSPL, with its name, number, user data, status *READY and pages.
#[tokio::test]
async fn retrieve_and_list_apis() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    as4tainr_core::source::create_file(&client, &lib, "QAPISRC", "API source")
        .await
        .unwrap();
    as4tainr_core::source::put_member(
        &client,
        &as4tainr_core::source::Member {
            library: lib.clone(),
            file: "QAPISRC".into(),
            name: "MEMBER1".into(),
            srctype: "RPGLE".into(),
            text: "Three lines".into(),
            source: "a\nb\nc\n".into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let job = as4tainr_core::jobs::JobId {
        number: 900000 + (std::process::id() as i64 % 99999),
        user: "QSECOFR".into(),
        name: "APITEST".into(),
    };
    as4tainr_core::spool::print_lines(
        &client,
        &job,
        "QSYSPRT",
        "MYDATA",
        &["one".into(), "two".into()],
    )
    .await
    .unwrap();
    let src = format!(
        "**FREE
dcl-pr QUSROBJD extpgm('QUSROBJD');
  rcv char(200) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  obj char(20) const;
  type char(10) const;
end-pr;
dcl-pr QUSRMBRD extpgm('QUSRMBRD');
  rcv char(300) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  file char(20) const;
  mbr char(10) const;
  ovr char(1) const;
end-pr;
dcl-pr QUSCRTUS extpgm('QUSCRTUS');
  name char(20) const;
  attr char(10) const;
  size int(10) const;
  init char(1) const;
  aut char(10) const;
  text char(50) const;
end-pr;
dcl-pr QUSPTRUS extpgm('QUSPTRUS');
  name char(20) const;
  ptr pointer;
end-pr;
dcl-pr QUSLFLD extpgm('QUSLFLD');
  space char(20) const;
  fmt char(8) const;
  file char(20) const;
  rcdfmt char(10) const;
  ovr char(1) const;
end-pr;
dcl-pr QUSLSPL extpgm('QUSLSPL');
  space char(20) const;
  fmt char(8) const;
  user char(10) const;
  outq char(20) const;
  form char(10) const;
  udata char(10) const;
  err char(16);
  job char(26) const;
end-pr;
dcl-pr QUSRSPLA extpgm('QUSRSPLA');
  rcv char(500) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  job char(26) const;
  ijob char(16) const;
  ispl char(16) const;
  splf char(10) const;
  nbr int(10) const;
end-pr;
dcl-ds objd len(200) qualified;
  name char(10) pos(9);
  lib char(10) pos(19);
  type char(10) pos(29);
  rtnlib char(10) pos(39);
  owner char(10) pos(53);
  domain char(2) pos(63);
  attr char(10) pos(91);
  text char(50) pos(101);
end-ds;
dcl-ds mbrd len(300) qualified;
  file char(10) pos(9);
  mbr char(10) pos(29);
  attr char(10) pos(39);
  type char(10) pos(49);
  text char(50) pos(85);
  src char(1) pos(135);
  records int(10) pos(141);
end-ds;
dcl-ds spla len(500) qualified;
  file char(10) pos(67);
  nbr int(10) pos(77);
  udata char(10) pos(91);
  status char(10) pos(101);
  pages int(10) pos(141);
end-ds;
dcl-s p pointer;
dcl-s e pointer;
dcl-ds hdr based(p) qualified;
  listoff int(10) pos(125);
  count int(10) pos(133);
  entsize int(10) pos(137);
end-ds;
dcl-ds fld based(e) qualified;
  name char(10);
  type char(1);
  use char(1);
  outpos int(10);
  inpos int(10);
  len int(10);
  digits int(10);
  dec int(10);
end-ds;
dcl-ds splf based(e) qualified;
  user char(10);
  udata char(10) pos(41);
  ijob char(16) pos(51);
  ispl char(16) pos(67);
end-ds;
dcl-ds s3 based(e) qualified;
  jname char(10);
  juser char(10);
  jnbr char(6);
  file char(10);
  nbr int(10);
  status int(10);
  date char(7);
  time char(6);
  sched char(1);
  sys char(10);
  udata char(10);
  form char(10);
  outq char(10);
end-ds;
dcl-s err char(16) inz(x'00000010');
dcl-s i int(10);
QUSROBJD(objd : %size(objd) : 'OBJD0200' : 'QAPISRC   *LIBL' : '*FILE');
dsply (%trim(objd.name) + ' ' + %trim(objd.type) + ' ' + %trim(objd.rtnlib) + ' '
       + %trim(objd.owner) + ' ' + objd.domain + ' ' + %trim(objd.attr) + ' ' + %trim(objd.text));
QUSROBJD(objd : %size(objd) : 'OBJD0100' : 'QAPISRC   {lib}' : '*FILE');
dsply ('named ' + %trim(objd.rtnlib));
QUSRMBRD(mbrd : %size(mbrd) : 'MBRD0200' : 'QAPISRC   {lib}' : '*FIRST' : '0');
dsply (%trim(mbrd.mbr) + ' ' + %trim(mbrd.attr) + ' ' + %trim(mbrd.type) + ' '
       + %trim(mbrd.text) + ' ' + mbrd.src + ' ' + %char(mbrd.records));
QUSCRTUS('APISPC    {lib}' : 'LIST' : 1000 : x'00' : '*ALL' : '');
QUSLFLD('APISPC    {lib}' : 'FLDL0100' : 'QAPISRC   {lib}' : '*FIRST' : '0');
QUSPTRUS('APISPC    {lib}' : p);
for i = 0 to hdr.count - 1;
  e = p + hdr.listoff + i * hdr.entsize;
  dsply (%trim(fld.name) + ' ' + fld.type + fld.use + ' ' + %char(fld.inpos) + ' '
         + %char(fld.len) + ' ' + %char(fld.digits) + ' ' + %char(fld.dec));
endfor;
QUSLSPL('APISPC    {lib}' : 'SPLF0100' : '*CURRENT' : '*ALL' : '*ALL' : '*ALL' : err
        : 'APITEST   QSECOFR   {nbr:06}');
dsply ('spooled ' + %char(hdr.count) + ' ' + %char(hdr.entsize));
e = p + hdr.listoff;
QUSRSPLA(spla : %size(spla) : 'SPLA0100' : '*INT' : splf.ijob : splf.ispl : '*INT' : -1);
dsply (%trim(spla.file) + ' ' + %char(spla.nbr) + ' ' + %trim(spla.udata) + ' '
       + %trim(spla.status) + ' ' + %char(spla.pages));
QUSLSPL('APISPC    {lib}' : 'SPLF0300' : '*CURRENT' : '*ALL' : '*ALL' : '*ALL' : err
        : 'APITEST   QSECOFR   {nbr:06}');
e = p + hdr.listoff;
dsply ('splf0300 ' + %char(hdr.count) + ' ' + %char(hdr.entsize) + ' ' + %trim(s3.jname)
       + ' ' + %trim(s3.juser) + ' ' + s3.jnbr + ' ' + %trim(s3.file) + ' ' + %char(s3.nbr)
       + ' ' + %char(s3.status) + ' ' + %subst(s3.date:1:1) + ' ' + %trim(s3.udata)
       + ' ' + %trim(s3.form));
*inlr = *on;
",
        nbr = job.number
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "APIS", &src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            &format!("QAPISRC *FILE {lib} QSECOFR *S PF-SRC API source"),
            &format!("named {lib}"),
            "MEMBER1 PF RPGLE Three lines 1 3",
            "SRCSEQ SB 1 6 6 2",
            "SRCDAT SB 7 6 6 0",
            "SRCDTA AB 13 80 0 0",
            "spooled 1 88",
            "QSYSPRT 1 MYDATA *READY 1",
            // SPLF0300 (List Spooled Files API): 136-byte entries; status
            // 1 is RDY; the date opened is CYYMMDD, century 1 for 20xx.
            &format!(
                "splf0300 1 136 APITEST QSECOFR {:06} QSYSPRT 1 1 1 MYDATA *STD",
                job.number
            ),
        ]
    );
}

/// Output queues and spooled file commands (IBM CL reference: CRTOUTQ
/// and CHGOUTQ's parameters, HLDOUTQ (CPF3426 when the queue is already
/// held; CPF3357 when not found), HLDSPLF, RLSSPLF, CHGSPLFA, CLROUTQ,
/// DLTOUTQ, CRTUSRPRF and CHGUSRPRF OUTQ). A user's spooled files go to
/// the profile's output queue; WRKSPLF shows a held file's status as HLD
/// and a ready one as RDY (the Sts column of IBM i 7.5's WRKSPLF, PUB400
/// tools/golden/screens4.txt), with options 3=Hold in column 24,
/// 4=Delete in 33, 5=Display in 44, 6=Release in 56 and 8=Attributes in
/// column 4 of the next line (the same capture).
#[tokio::test]
async fn output_queues_and_spooled_file_commands() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::{jobs, spool};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let user = format!("Q{}", &lib[1..]);
    let env = Environment {
        database_url: url.clone(),
    };
    let mut admin = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for c in [
        format!("CRTOUTQ OUTQ({lib}/MYQ) TEXT('My queue')"),
        format!("CRTUSRPRF USRPRF({user}) PASSWORD(*NONE) OUTQ({lib}/MYQ)"),
    ] {
        admin.run(&c).await.unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let e = admin
        .run(&format!("CRTOUTQ OUTQ({lib}/MYQ)"))
        .await
        .unwrap_err();
    assert!(e.to_string().starts_with("CPF9870"), "{e}");
    let e = admin.run(&format!("HLDOUTQ {lib}/NOQ")).await.unwrap_err();
    assert!(e.to_string().starts_with("CPF3357"), "{e}");
    admin.run(&format!("HLDOUTQ {lib}/MYQ")).await.unwrap();
    let e = admin.run(&format!("HLDOUTQ {lib}/MYQ")).await.unwrap_err();
    assert!(e.to_string().starts_with("CPF3426"), "{e}");
    admin.run(&format!("RLSOUTQ {lib}/MYQ")).await.unwrap();

    let id = jobs::start_interactive(
        &client,
        "OUTQDEV",
        &user,
        Some(&lib),
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: user.clone(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id.clone()),
        calls: Default::default(),
    };
    spool::print_lines(&client, &id, "REPORT", "A", &["one".into()])
        .await
        .unwrap();
    spool::print_lines(&client, &id, "REPORT", "B", &["two".into()])
        .await
        .unwrap();
    let files = spool::for_user(&client, &user).await.unwrap();
    assert!(files.iter().all(|f| f.outq == "MYQ"), "{files:?}");

    job.run("HLDSPLF FILE(REPORT) SPLNBR(1)").await.unwrap();
    let Outcome::Display(d) = job.run("WRKSPLF").await.unwrap() else {
        panic!()
    };
    let sts: Vec<&str> = d.rows.iter().map(|r| &r[48..51]).collect();
    assert_eq!(sts, ["HLD", "RDY"], "{:?}", d.rows);
    let l = d.ibm.unwrap();
    assert_eq!(l.option_text[0][0].find("3=Hold"), Some(22));
    assert_eq!(l.option_text[0][0].find("4=Delete"), Some(31));
    assert_eq!(l.option_text[0][0].find("5=Display"), Some(42));
    assert_eq!(l.option_text[0][0].find("6=Release"), Some(54));
    assert_eq!(l.option_text[0][1].find("8=Attributes"), Some(2));
    job.run("RLSSPLF FILE(REPORT) SPLNBR(1)").await.unwrap();
    assert!(spool::for_user(&client, &user)
        .await
        .unwrap()
        .iter()
        .all(|f| f.status == "RDY"));

    // CHGSPLFA moves a file to QGPL/QPRINT and changes its user data.
    job.run("CHGSPLFA FILE(REPORT) SPLNBR(2) OUTQ(QGPL/QPRINT) USRDTA(MOVED)")
        .await
        .unwrap();
    let f = spool::find(&client, "REPORT", id.number, Some(2))
        .await
        .unwrap();
    assert_eq!((f.outq.as_str(), f.user_data.as_str()), ("QPRINT", "MOVED"));

    // A queue with files is not deleted; CLROUTQ empties it.
    assert!(admin.run(&format!("DLTOUTQ {lib}/MYQ")).await.is_err());
    admin.run(&format!("CLROUTQ {lib}/MYQ")).await.unwrap();
    let left = spool::for_user(&client, &user).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].outq, "QPRINT");
    admin
        .run(&format!("CHGUSRPRF USRPRF({user}) OUTQ(*WRKSTN)"))
        .await
        .unwrap();
    let m = admin.run(&format!("DLTOUTQ {lib}/MYQ")).await.unwrap();
    assert!(
        matches!(m, Outcome::Messages(ref v) if v[0].starts_with("CPC2191")),
        "{m:?}"
    );
    // OUTQ(*WRKSTN): QGPL/QPRINT.
    spool::print_lines(&client, &id, "REPORT", "C", &["three".into()])
        .await
        .unwrap();
    let f = spool::find(&client, "REPORT", id.number, None)
        .await
        .unwrap();
    assert_eq!(f.outq, "QPRINT");
    spool::delete(&client, f.id).await.unwrap();
    spool::delete(&client, left[0].id).await.unwrap();
    admin.run(&format!("DLTUSRPRF {user}")).await.unwrap();
}

/// Referential constraints written the ways IBM i's SQL programming guide
/// writes them: "Department table (DEPARTMENT)" adds them with the
/// constraint's name after FOREIGN KEY (ADD FOREIGN KEY ROD (ADMRDEPT)
/// REFERENCES DEPARTMENT ON DELETE CASCADE: the parent's primary key);
/// "Adding referential constraints" puts one on a column (CONSTRAINT
/// WORKDEPT_EXISTS REFERENCES ... (DEPTNO)). An INSERT whose foreign key
/// has no parent fails with SQL0530 (SQLSTATE 23503).
#[tokio::test]
async fn referential_constraints_as_in_the_sample_tables() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "CREATE TABLE {lib}.DEPARTMENT
      (DEPTNO    CHAR(3)           NOT NULL,
       DEPTNAME  VARCHAR(36)       NOT NULL,
       MGRNO     CHAR(6)                   ,
       ADMRDEPT  CHAR(3)           NOT NULL,
       LOCATION  CHAR(16),
       PRIMARY KEY (DEPTNO));
ALTER TABLE {lib}.DEPARTMENT
      ADD FOREIGN KEY ROD (ADMRDEPT)
          REFERENCES {lib}.DEPARTMENT
          ON DELETE CASCADE;
CREATE TABLE {lib}.EMP (EMPNO CHAR(6) NOT NULL PRIMARY KEY,
       WORKDEPT CHAR(3) CONSTRAINT WORKDEPT_EXISTS REFERENCES {lib}.DEPARTMENT (DEPTNO));
INSERT INTO {lib}.DEPARTMENT VALUES ('A00', 'SPIFFY', NULL, 'A00', NULL);
INSERT INTO {lib}.DEPARTMENT VALUES ('B01', 'PLANNING', NULL, 'A00', NULL);
INSERT INTO {lib}.EMP VALUES ('000010', 'B01');"
    );
    let lines = job
        .run_sql_script(&script, "sample tables", 0, None, true)
        .await
        .unwrap();
    assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    let e = as4tainr_core::sql::run_as(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "QSECOFR",
        None,
        None,
        &format!("INSERT INTO {lib}.EMP VALUES ('000020', 'Z99')"),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().starts_with("SQL0530"), "{e}");
    // SQL0530 names the constraint and its library (Db2 for i SQL
    // messages and codes).
    assert!(
        e.to_string().contains(&format!("WORKDEPT_EXISTS in {lib}")),
        "{e}"
    );
    let e = as4tainr_core::sql::run_as(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "QSECOFR",
        None,
        None,
        &format!("INSERT INTO {lib}.DEPARTMENT VALUES ('C01', 'X', NULL, 'Q99', NULL)"),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().starts_with("SQL0530"), "{e}");
}

/// One value of `VALUES expr` through the SQL interface.
async fn sql_value(client: &tokio_postgres::Client, url: &str, lib: &str, expr: &str) -> String {
    match as4tainr_core::sql::run_as(
        client,
        url,
        &[lib.to_string()],
        "QSECOFR",
        None,
        None,
        &if expr.starts_with("SELECT") {
            expr.to_string()
        } else {
            format!("VALUES {expr}")
        },
    )
    .await
    {
        Ok(as4tainr_core::sql::Outcome::Rows { rows, .. }) => rows[0][0].trim().to_string(),
        Ok(_) => panic!("{expr}: no rows"),
        Err(e) => format!("ERR {e}"),
    }
}

#[tokio::test]
async fn db2_scalar_functions_found_by_the_corpus() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    // Expected values: the examples of the Db2 for i SQL Reference (IBM i
    // 7.5), scalar functions, where it gives one ("ex"); otherwise worked
    // out by hand from the rules it states.
    let cases: &[(&str, &str)] = &[
        ("LOCATE('N', 'DINING')", "3"),                     // ex
        ("LOCATE('N', 'DINING', 4)", "5"),                  // start at 4: D I N I N G
        ("POSSTR('Hello', 'l')", "3"),
        ("LOCATE_IN_STRING('WINNING', 'IN', 1, 2)", "5"),   // second occurrence
        ("LOCATE_IN_STRING('WINNING', 'IN', -1)", "5"),     // backward from the end
        ("INSERT('INSERTING', 4, 2, 'IS')", "INSISTING"),   // ex
        ("INSERT('INSERTING', 4, 0, 'IS')", "INSISERTING"), // ex
        ("INSERT('INSERTING', 4, 2, '')", "INSTING"),       // ex
        ("'[' || SPACE(3) || ']'", "[   ]"),
        ("TRANSLATE('abcdef')", "ABCDEF"),
        // to padded with the pad character to the length of from.
        ("TRANSLATE('abcd', 'A', 'ab', '%')", "A%cd"),
        ("TRANSLATE('abcdef', 'xy', 'bc')", "axydef"),
        ("STRIP('  a  ')", "a"),
        ("STRIP('00100', L, '0')", "100"),
        ("STRIP('00100', TRAILING, '0')", "001"),
        ("IFNULL(NULL, 2)", "2"),
        ("VALUE(NULL, NULL, 3)", "3"),
        ("MIDNIGHT_SECONDS('12:00:00')", "43200"),
        ("MIDNIGHT_SECONDS('24.00.00')", "86400"), // the range ends at 86400
        // Weeks start on Sunday, January 1 is in week 1: 2007-01-01 was a
        // Monday, so Saturday the 6th is in week 1, Sunday the 7th in week 2.
        ("WEEK('2007-01-06')", "1"),
        ("WEEK('2007-01-07')", "2"),
        ("WEEK_ISO('2024-01-01')", "1"),
        ("DAYOFWEEK_ISO('2024-01-07')", "7"), // a Sunday
        ("MONTHNAME('2024-02-01')", "February"),
        ("DAYNAME('2024-01-01')", "Monday"),
        ("JULIAN_DAY('1998-01-01')", "2450815"), // ex
        ("ADD_MONTHS('2024-01-31', 1)", "2024-02-29"), // no 31st: last day
        ("ADD_MONTHS('2024-02-29', 1)", "2024-03-31"), // last day stays last
        ("LAST_DAY('2024-02-10')", "2024-02-29"),
        ("NEXT_DAY('2024-01-01', 'MONDAY')", "2024-01-08-00.00.00.000000"),
        ("NEXT_DAY('2024-01-01', 'FRI')", "2024-01-05-00.00.00.000000"),
        ("MONTHS_BETWEEN('2005-02-02', '2005-01-01')", "1.032258064516129"), // ex
        ("MONTHS_BETWEEN('2005-03-31', '2005-02-28')", "1.000000000000000"), // both last days
        ("JULIAN_DAY(TIMESTAMP_FORMAT('1998-01-01 10:00:00', 'YYYY-MM-DD HH24:MI:SS'))", "2450815"),
        ("TO_DATE('20240105', 'YYYYMMDD')", "2024-01-05-00.00.00.000000"),
        ("LENGTH(GENERATE_UNIQUE())", "13"), // 13 bytes of bit data
        // HEX: the internal form, big-endian binary, IEEE floating point,
        // packed decimal (1.5 is DECIMAL(2,1), X'015D' when negative).
        ("HEX(1)", "00000001"),
        ("HEX(SMALLINT(1))", "0001"),
        ("HEX(DOUBLE(1.5))", "3FF8000000000000"),
        ("HEX(-1.5)", "015D"),
        // INTEGER and the other cast functions cut off the fraction.
        ("INT(1.9)", "1"),
        ("INTEGER('  12 ')", "12"),
        ("SMALLINT(-3.7)", "-3"),
        ("DOUBLE('1.5')", "1.5"),
        ("VARCHAR('abc', 2)", "ab"),
        ("VARCHAR(1.50)", "1.50"),
        ("ZONED(12.345, 5, 2)", "12.34"),
        // DATE of n: n - 1 days after 0001-01-01; yyyynnn; USA and EUR.
        ("DATE(1)", "0001-01-01"),
        ("DATE(DAYS('2021-07-29'))", "2021-07-29"),
        ("DATE('2024032')", "2024-02-01"),
        ("DATE('12/25/2024')", "2024-12-25"),
        ("DATE('25.12.2024')", "2024-12-25"),
        // TIMESTAMPDIFF of CHAR(ts - ts): 10 seconds.
        (
            "TIMESTAMPDIFF(2, CHAR(TIMESTAMP('2024-01-01-00.00.10') - TIMESTAMP('2024-01-01-00.00.00')))",
            "10",
        ),
        ("TIMESTAMPDIFF(16, '00000203000000.000000')", "63"), // 2 months of 30 days + 3
        ("MAX(1, 2)", "2"),
        ("MIN(1, NULL)", "-"), // null when an argument is null
        ("CURDATE() = CURRENT DATE", "TRUE"),
        ("CURRENT USER", "QSECOFR"),
        ("SESSION_USER", "QSECOFR"),
    ];
    for (expr, want) in cases {
        let got = sql_value(&client, &url, &lib, expr).await;
        let got = if got == "1" && *want == "TRUE" {
            "TRUE".to_string()
        } else {
            got
        };
        assert_eq!(got, *want, "{expr}");
    }
    // An invalid date: SQL0181 (Db2 for i SQL messages and codes).
    for bad in ["DATE('2024-02-30')", "DATE(0)"] {
        let got = sql_value(&client, &url, &lib, bad).await;
        assert!(got.starts_with("ERR SQL0181"), "{bad}: {got}");
    }
    // LISTAGG: the values joined in the WITHIN GROUP order; no separator
    // is an empty string.
    let got = sql_value(
        &client,
        &url,
        &lib,
        "SELECT LISTAGG(X, ', ') WITHIN GROUP (ORDER BY X DESC) FROM (VALUES ('a'), ('b'), ('c')) T(X)",
    )
    .await;
    assert_eq!(got, "c, b, a");
    let got = sql_value(
        &client,
        &url,
        &lib,
        "SELECT LISTAGG(X) WITHIN GROUP (ORDER BY X) FROM (VALUES ('b'), ('a')) T(X)",
    )
    .await;
    assert_eq!(got, "ab");
}

#[tokio::test]
async fn procedure_variables_named_like_columns() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // The company_system corpus's POPDEPT: variables named like the
    // columns. The column list of INSERT and the targets of UPDATE ... SET
    // name columns, not variables (Db2 for i SQL Reference, INSERT, UPDATE).
    let script = format!(
        "CREATE TABLE {lib}.DEPT (DEPTNO CHAR(3) NOT NULL, DEPTNAME VARCHAR(36));
CREATE OR REPLACE PROCEDURE {lib}.POPD()
LANGUAGE SQL
BEGIN
  DECLARE DEPTNO CHAR(3);
  DECLARE DEPTNAME VARCHAR(36);
  SET DEPTNO = RIGHT('000' || CAST(RAND() * 1000 AS INT), 3);
  SET DEPTNAME = 'Admin';
  INSERT INTO {lib}.DEPT (DEPTNO, DEPTNAME) VALUES (DEPTNO, DEPTNAME) WITH NC;
  UPDATE {lib}.DEPT SET DEPTNAME = 'IT' WITH NC;
END;
CALL {lib}.POPD();"
    );
    let lines = job
        .run_sql_script(&script, "popd", 0, None, true)
        .await
        .unwrap();
    assert!(lines.last().unwrap().contains("0 with errors"), "{lines:?}");
    let got = sql_value(
        &client,
        &url,
        &lib,
        &format!("SELECT LENGTH(DEPTNO) || DEPTNAME FROM {lib}.DEPT"),
    )
    .await;
    assert_eq!(got, "3IT");
}

#[tokio::test]
async fn rpg_features_found_by_the_ossile_corpus() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // A program-described file the program names does not exist; EXTFILE
    // names the one it opens (ILE RPG Reference, EXTFILE: the file is
    // found when it is opened).
    job.run(&format!("CRTPF FILE({lib}/DATA10) RCDLEN(10)"))
        .await
        .unwrap();
    as4tainr_core::sql::run_as(
        &client,
        &url,
        std::slice::from_ref(&lib),
        "QSECOFR",
        None,
        None,
        &format!("INSERT INTO {lib}.DATA10 VALUES ('HELLO')"),
    )
    .await
    .unwrap();
    let programs: Vec<(&str, String, Vec<&str>)> = vec![
        // A name continued with an ellipsis past position 21, on D and P
        // specifications (ILE RPG Reference: a name may use positions
        // 7-80 when it ends in "..."), as OSSILE's llist_h.rpgle writes
        // list_removeFirst...
        (
            "LONGNAME",
            "     Dr                S             10I 0\n     Dlist_removeFirst...\n     D                 PR            10I 0\n     Dn                              10I 0 value\n     C                   eval      r = list_removeFirst(21)\n     C     r             dsply\n     C                   seton                                        LR\n     Plist_removeFirst...\n     P                 B\n     D                 PI            10I 0\n     Dn                              10I 0 value\n     C                   return    n * 2\n     P                 E\n".to_string(),
            vec!["42"],
        ),
        // ILE RPG Reference: a subfield named like an opcode's prefix
        // (return_value); a subfield LIKEDS a data structure defined
        // later (10 bytes);
        // INZ(*LIKEDS) (the template's INZ values); a local field LIKE a
        // parameter; procedure pointers (%PADDR, EXTPROC(pointer),
        // POINTER(*PROC)); an array overlaying a data structure array,
        // sorted by SORTA (whole elements move); %SIZE of based storage
        // without an address. QlgConvertCase (Convert Case API: request
        // type 1, case 0 to upper case) with the QSYSINC error code
        // structure QUSEC (bytes available 0: no error).
        (
            "OSSFREE",
            "**FREE
ctl-opt dftactgrp(*no);
/copy QSYSINC/QRPGLESRC,QUSEC
dcl-ds msg_t qualified template;
  id char(7);
  sender likeds(sender_t);
end-ds;
dcl-ds sender_t qualified template;
  pgm char(10) inz('CALLER');
end-ds;
dcl-ds iconv_t qualified template;
  return_value int(10) inz(7);
end-ds;
dcl-ds rcb_t qualified template;
  type int(10) inz(1);
  ccsid int(10) inz(0);
  case int(10) inz(0);
  res char(10) inz(*allx'00');
end-ds;
dcl-pr convertCase extproc('QlgConvertCase');
  rcb likeds(rcb_t) const;
  input char(64) const options(*varsize);
  output char(64) options(*varsize);
  length int(10) const;
  err likeds(QUSEC) options(*varsize);
end-pr;
dcl-pr apply int(10) extproc(fn);
  x int(10) value;
end-pr;
dcl-s fn pointer(*proc);
dcl-ds m likeds(msg_t) inz(*likeds);
dcl-ds ic likeds(iconv_t) inz(*likeds);
dcl-ds rcb likeds(rcb_t) inz(*likeds);
dcl-ds err likeds(QUSEC);
dcl-s s char(20) inz('Gipfel strasse 1a');
dcl-ds *n;
  loc char(8) dim(3);
  zip char(3) overlay(loc);
end-ds;
dcl-ds based_t qualified based(p);
  a char(30);
end-ds;
dcl-s p pointer;
dsply %char(%size(m.sender));
dsply %char(ic.return_value);
clear err;
err.QUSBPRV = %size(err);
convertCase(rcb : s : s : %len(s) : err);
dsply s;
dsply %char(err.QUSBAVL);
fn = %paddr(twice);
dsply %char(apply(5));
fn = %paddr('THRICE');
dsply %char(apply(5));
loc(1) = '300Bonn';
loc(2) = '100Linz';
loc(3) = '200Graz';
sorta zip;
dsply (loc(1) + loc(2) + loc(3));
dsply %char(%size(based_t));
*inlr = *on;
dcl-proc twice;
  dcl-pi *n int(10);
    x int(10) value;
  end-pi;
  return x * 2;
end-proc;
dcl-proc thrice;
  dcl-pi *n int(10);
    x int(10) value;
  end-pi;
  dcl-s keep like(x);
  keep = x * 3;
  return keep;
end-proc;
"
            .to_string(),
            vec![
                "10",
                "7",
                "GIPFEL STRASSE 1A",
                "0",
                "10",
                "15",
                "100Linz 200Graz 300Bonn",
                "30",
            ],
        ),
        // A call through a procedure pointer whose prototype has more
        // parameters than the procedure (OSSILE's list_foreach passes its
        // user data to a callback that takes only the value): the
        // procedure gets its parameters; %PARMS is the number of
        // parameters passed (ILE RPG Reference, %PARMS).
        (
            "OSSPARMS",
            "**FREE
ctl-opt dftactgrp(*no);
dcl-pr each int(10) extproc(fn);
  x int(10) value;
  y int(10) value;
end-pr;
dcl-s fn pointer(*proc);
fn = %paddr(one);
dsply %char(each(4 : 9));
*inlr = *on;
dcl-proc one;
  dcl-pi *n int(10);
    x int(10) value;
  end-pi;
  return x * 10 + %parms();
end-proc;
"
            .to_string(),
            vec!["42"],
        ),
        // Arithmetic and comparisons with a float operand are done in
        // float (ILE RPG Reference, expressions: an operand of float type
        // makes the operation a float operation), as OSSILE's ALIST01
        // compares a 19-digit literal with the float(8) it was stored in:
        // in float the difference is 0.
        (
            "OSSFLOAT",
            "**FREE
dcl-s d float(8);
d = 1.234567890123456789;
if 1.234567890123456789 - d < 0.000000000000000001;
  dsply 'equal';
else;
  dsply 'apart';
endif;
*inlr = *on;
"
            .to_string(),
            vec!["equal"],
        ),
        // ILE CEE APIs (the CEEDAYS and CEEDATE pages): 16 May 1988 is
        // Lilian day 148138; 148143 written as "Wwwwwwwwwz, Mmmmmmmmmz
        // DD, YYYY" is "Saturday, May 21, 1988"; CEEDYWK gives 7 for a
        // Saturday (1 is Sunday); an invalid date gives Lilian date 0 and
        // a feedback code that is not all zeros (CEE0000 is). CEEDOD gives
        // the length of the argument passed (its operational descriptor).
        // strtok (ILE C runtime) skips empty tokens; EXTPROC(*DCLCASE).
        // The program-described file opened by EXTFILE.
        (
            "CEEPGM",
            format!(
                "**FREE
ctl-opt dftactgrp(*no) main(main);
dcl-pr CEEDAYS opdesc;
  d char(20) const options(*varsize);
  pic char(20) const options(*varsize);
  lil int(10);
  fc char(12) options(*omit);
end-pr;
dcl-pr CEEDATE opdesc;
  lil int(10) const;
  pic char(40) const options(*varsize);
  result char(40) options(*varsize);
  fc char(12) options(*omit);
end-pr;
dcl-pr CEEDYWK;
  lil int(10) const;
  wday int(10);
  fc char(12) options(*omit);
end-pr;
dcl-pr CEEDOD;
  pos int(10) const;
  a int(10);
  b int(10);
  c int(10);
  d int(10);
  len int(10);
  fc char(12) options(*omit);
end-pr;
dcl-pr strtok pointer extproc('strtok');
  s pointer value options(*string);
  d pointer value options(*string);
end-pr;
dcl-pr lenOf int(10) opdesc extproc(*dclcase);
  v char(100) const options(*varsize);
end-pr;
dcl-pr main extpgm('CEEPGM') end-pr;
dcl-proc main;
  dcl-s lil int(10);
  dcl-s fc char(12);
  dcl-s out char(40);
  dcl-s wday int(10);
  dcl-s t pointer;
  dcl-s fname char(21) inz('{lib}/DATA10');
  dcl-f nosuch disk(10) extfile(fname) usropn;
  dcl-ds rec len(10) end-ds;
  CEEDAYS('19880516' : 'YYYYMMDD' : lil : fc);
  dsply %char(lil);
  dsply %char(%check(x'00' : fc));
  CEEDATE(148143 : 'Wwwwwwwwwz, Mmmmmmmmmz DD, YYYY' : out : *omit);
  dsply out;
  CEEDYWK(148143 : wday : *omit);
  dsply %char(wday);
  CEEDAYS('19880230' : 'YYYYMMDD' : lil : fc);
  dsply %char(lil);
  dsply %char(%check(x'00' : fc) > 0);
  dsply %char(lenOf('abc'));
  t = strtok('a,b,,c' : ',');
  dow t <> *null;
    dsply %str(t);
    t = strtok(*null : ',');
  enddo;
  open nosuch;
  read nosuch rec;
  dsply rec;
  close nosuch;
end-proc;
dcl-proc lenOf;
  dcl-pi *n int(10) opdesc;
    v char(100) const options(*varsize);
  end-pi;
  dcl-s a int(10);
  dcl-s len int(10);
  CEEDOD(1 : a : a : a : a : len : *omit);
  return len;
end-proc;
"
            ),
            vec![
                "148138",
                "0",
                "Saturday, May 21, 1988",
                "7",
                "0",
                "1",
                "3",
                "a",
                "b",
                "c",
                "HELLO",
            ],
        ),
        // QWCRNETA (Retrieve Network Attributes API): the number of
        // attributes, their offsets, then each one's entry (the QSYSINC
        // names QWCRAT, QWCNA, QWCLD); SYSNAME is CHAR(8).
        (
            "NETA",
            "**FREE
/copy QSYSINC/QRPGLESRC,QWCRNETA
dcl-pr qwcrneta extpgm('QWCRNETA');
  rcv char(256) options(*varsize);
  len int(10) const;
  n int(10) const;
  names char(10) const;
  err char(8);
end-pr;
dcl-ds rcv len(256);
  count int(10) pos(1);
  off1 int(10) pos(5);
end-ds;
dcl-s err char(8) inz(*allx'00');
qwcrneta(rcv : 256 : 1 : 'SYSNAME' : err);
dsply %char(count);
qwcrat = %subst(rcv : off1 + 1 : 16);
dsply qwcna;
dsply %char(qwcld);
*inlr = *on;
"
            .to_string(),
            vec!["1", "SYSNAME", "8"],
        ),
    ];
    for (name, source, want) in programs {
        let r = rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            source,
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{name}: {:?}", r.error);
        let got: Vec<String> = r
            .messages
            .iter()
            .map(|m| m.trim_end().to_string())
            .collect();
        assert_eq!(got, want, "{name}");
    }
}

#[tokio::test]
async fn rpg_features_found_by_rpgunit() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    // A job of its own: jobs without one share the process's program
    // message queues, which other tests running at the same time use.
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("R{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    // OVRPRTF's parameters left out are not overridden (CL reference,
    // OVRPRTF): RPGUnit's OVRPRTF names no OUTQ; and CRTSRVPGM takes
    // OPTION(*DUPPROC).
    job.run("OVRPRTF FILE(QSYSPRT) USRDTA(X) SPLFNAME(RPGUNIT)")
        .await
        .unwrap();
    job.run("DLTOVR FILE(QSYSPRT)").await.unwrap();
    as4tainr_core::cl::parse(&format!(
        "CRTSRVPGM SRVPGM({lib}/X) EXPORT(*ALL) OPTION(*DUPPROC)"
    ))
    .unwrap();
    // RPGUnit's own tests (RPGUnit/RPGUnit on GitHub) expect these on IBM
    // i, and they pass there:
    // - QWVRCSTK (Retrieve Call Stack, format CSTK0100): the most recent
    //   entry first; a procedure's name as exported (EXTPROC('inner')
    //   keeps its case); its statement ID the source sequence number with
    //   two decimals, zero-padded to 10 (ASSERTT: '0000008500' for line
    //   85).
    // - QMHSNDPM *ESCAPE to the caller of the caller (counter 2): the
    //   entries between end, their MONITOR groups do not take it
    //   (ASSERTT testFailShouldRiseAboveAnEnclosingMonitorBlock).
    // - An exception message's sender is the procedure where it happened
    //   and its statement '16500' for line 165 (PGMMSGT
    //   test_rcvExcpMsgInfo, RCVM0300 sender information).
    // - QMHRSNEM with a blank key resends the last new escape message to
    //   the caller (PGMMSGT test_resendEscapeMsg).
    // - A procedure returning LIKEDS(ds) DIM(n) returns every element
    //   (ASSERT getCallStk); a whole array is passed to a CONST DIM
    //   parameter (SPLF rtvJobUsrNm).
    // - %INT of blanks is an error (CMDRUNLOG shows '*N' then).
    // - QGYOLJBL lists the job log newest first (*PRV): a handled MCH1211
    //   is there (JOBLOGT).
    // - QWCRTVCA key 305: the current user, CHAR(10) (SPLF).
    // - QLICVTTP *SYMTOHEX of *SRVPGM is x'0203' (MI object types).
    let source = "**FREE
ctl-opt dftactgrp(*no) main(main);
dcl-ds ent_t qualified template;
  pgm char(10);
  proc char(30);
  stmt char(10);
end-ds;
dcl-pr inner likeds(ent_t) dim(2) extproc('inner');
end-pr;
dcl-pr qwvrcstk extpgm('QWVRCSTK');
  rcv char(2000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  jid char(56) const;
  jfmt char(8) const;
  err char(8);
end-pr;
dcl-pr qmhsndpm extpgm('QMHSNDPM');
  id char(7) const;
  msgf char(20) const;
  data char(80) const;
  dlen int(10) const;
  type char(10) const;
  entry char(10) const;
  counter int(10) const;
  key char(4);
  err char(8);
end-pr;
dcl-pr qmhrcvpm extpgm('QMHRCVPM');
  rcv char(2000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  entry char(10) const;
  counter int(10) const;
  type char(10) const;
  key char(4) const;
  wait int(10) const;
  action char(10) const;
  err char(8);
end-pr;
dcl-pr qmhrsnem extpgm('QMHRSNEM');
  key char(4) const;
  err char(8);
end-pr;
dcl-pr qgyoljbl extpgm('QGYOLJBL');
  rcv char(4000) options(*varsize);
  len int(10) const;
  info char(80);
  count int(10) const;
  sel char(80) const;
  sellen int(10) const;
  err char(8);
end-pr;
dcl-pr qwcrtvca extpgm('QWCRTVCA');
  rcv char(100) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  n int(10) const;
  keys int(10) const dim(5) options(*varsize);
  err char(8);
end-pr;
dcl-pr qlicvttp extpgm('QLICVTTP');
  op char(10) const;
  sym char(10) const;
  hex char(2);
  err char(8);
end-pr;
dcl-pr main extpgm('RUNITF') end-pr;
dcl-s err char(8) inz(*allx'00');
dcl-proc main;
  dcl-ds e likeds(ent_t) dim(2);
  dcl-s caught ind;
  dcl-s rcv char(2000);
  dcl-s key char(4);
  dcl-s n int(10);
  dcl-s keys int(10) dim(1);
  dcl-s hex char(2);
  dcl-ds sel len(80) qualified;
    dir char(10) pos(1) inz('*PRV');
    job char(26) pos(11) inz('*');
    start char(4) pos(53) inz(x'FFFFFFFF');
    fldoff int(10) pos(65) inz(0);
    fldcnt int(10) pos(69) inz(0);
  end-ds;
  dcl-ds info len(80) qualified;
    ret int(10) pos(5);
  end-ds;
  e = inner();
  dsply (%trim(e(1).proc) + ' ' + e(1).stmt);
  dsply %trim(e(2).proc);
  monitor;
    caught = passOver();
  on-error;
    dsply 'escape reached main';
  endmon;
  dsply %char(caught);
  monitor;
    n = divide(0);
  on-error;
  endmon;
  qmhrcvpm(rcv : %size(rcv) : 'RCVM0300' : '*' : 0 : '*EXCP' : ' ' : 0 : '*SAME' : err);
  dsply %subst(rcv : 13 : 7);
  dsply %trim(sender(rcv));
  qgyoljbl(rcv : %size(rcv) : info : -1 : sel : %size(sel) : err);
  dsply %subst(rcv : 17 : 7);
  monitor;
    resend();
  on-error;
    dsply 'resent';
  endmon;
  monitor;
    n = %int('   ');
    dsply 'no error';
  on-error 105;
    dsply '*N';
  endmon;
  keys(1) = 305;
  qwcrtvca(rcv : 100 : 'RTVC0100' : 1 : keys : err);
  dsply %subst(rcv : 21 : 10);
  qlicvttp('*SYMTOHEX' : '*SRVPGM' : hex : err);
  dsply %char(hex = x'0203');
end-proc;
dcl-proc inner;
  dcl-pi *n likeds(ent_t) dim(2);
  end-pi;
  dcl-ds res likeds(ent_t) dim(2);
  dcl-s rcv char(2000);
  dcl-s off int(10);
  dcl-s i int(10);
  dcl-s nl int(10);
  qwvrcstk(rcv : %size(rcv) : 'CSTK0100' : '*' : 'JIDF0100' : err); // @STK
  off = bin(%subst(rcv : 13 : 4)) + 1;
  for i = 1 to 2;
    res(i).pgm = %subst(rcv : off + 24 : 10);
    nl = bin(%subst(rcv : off + 16 : 4));
    res(i).proc = %subst(rcv : off + bin(%subst(rcv : off + 12 : 4)) : nl);
    if bin(%subst(rcv : off + 8 : 4)) > 0;
      res(i).stmt = %subst(rcv : off + bin(%subst(rcv : off + 4 : 4)) : 10);
    endif;
    off += bin(%subst(rcv : off : 4));
  endfor;
  return res;
end-proc;
dcl-proc passOver;
  dcl-pi *n ind end-pi;
  monitor;
    sendUp();
  on-error;
    return *on;
  endmon;
  return *off;
end-proc;
dcl-proc sendUp;
  dcl-s key char(4);
  qmhsndpm('CPF9898' : 'QCPFMSG   *LIBL' : 'up' : 2 : '*ESCAPE' : '*' : 2 : key : err);
end-proc;
dcl-proc divide;
  dcl-pi *n int(10);
    d int(10) value;
  end-pi;
  return 1 / d; // @DIV
end-proc;
dcl-proc resend;
  monitor;
    divide(0);
  on-error;
    qmhrsnem(' ' : err);
  endmon;
end-proc;
dcl-proc bin;
  dcl-pi *n int(10);
    b char(4) const;
  end-pi;
  dcl-ds d;
    c char(4);
    i int(10) overlay(c);
  end-ds;
  c = b;
  return i;
end-proc;
dcl-proc sender;
  dcl-pi *n varchar(300);
    rcv char(2000) const;
  end-pi;
  dcl-s at int(10);
  // RCVM0300: lengths of data, text, help and sender information.
  at = 113 + bin(%subst(rcv : 81 : 4)) + bin(%subst(rcv : 89 : 4))
       + bin(%subst(rcv : 97 : 4));
  return %trim(%subst(rcv : at + 63 : 256)) + ' '
       + %trim(%subst(rcv : at + 324 : 10));
end-proc;
";
    let line_of = |marker: &str| {
        source
            .lines()
            .position(|l| l.contains(marker))
            .map(|i| i + 1)
            .unwrap()
    };
    let stk = format!("inner {:010}", line_of("// @STK") * 100);
    let div = format!("DIVIDE {}", line_of("// @DIV") * 100);
    // In the test's own job (its call stack and message queues), not
    // the process-wide ones other tests use at the same time.
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        job.id.as_ref().map(|i| i.to_string()),
        None,
        None,
        "RUNITF".into(),
        source.to_string(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    let got: Vec<String> = r
        .messages
        .iter()
        .map(|m| m.trim_end().to_string())
        .collect();
    let want: Vec<String> = vec![
        stk,
        "MAIN".into(),
        "escape reached main".into(),
        "0".into(),
        "MCH1211".into(),
        div,
        "MCH1211".into(),
        "resent".into(),
        "*N".into(),
        "QSECOFR".into(),
        "1".into(),
    ];
    assert_eq!(got, want);
}

#[tokio::test]
async fn spooled_file_apis_and_printed_output_of_system() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::{jobs, pase_system};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // The golden programs of tools/golden/spool.txt: GLDSPL prints with
    // space before 1 and 2, skip before 10, space after 3 (printed over
    // the line before) and skip before 1 (a new page), under OVRPRTF
    // SPLFNAME(GLDSPL) USRDTA(GLDSPL); GLDSPD makes the spooled file and
    // reads it back with QGYOLSPL, QGYGTLE, QSPOPNSP and QSPGETSP.
    job.run(&format!(
        "RUNSQL SQL('CREATE TABLE {lib}/GLDSPO (ID CHAR(10) NOT NULL, VAL CHAR(100) NOT NULL)') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, src) in [
        (
            "gldspl",
            include_str!("../../tools/golden/spool/gldspl.rpgle"),
        ),
        (
            "gldspd",
            include_str!("../../tools/golden/spool/gldspd.rpgle"),
        ),
    ] {
        let src = src.replace("AS4TRAINR1", &lib);
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}.rpgle"),
            src.as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
        let o = pase_system::run(
            &mut job,
            &[format!(
                "CRTBNDRPG PGM({lib}/{}) SRCSTMF('{dir}/{f}.rpgle')",
                f.to_uppercase()
            )],
        )
        .await;
        assert_eq!(o.status, 0, "{o:?}");
    }

    // PASE `system` writes a program's spooled output to standard output:
    // its lines without the blank lines spacing and skipping make, a line
    // printed over another merged into it (PUB400 2026-10-23, s_print;
    // PUB400's own QPRTTXT page footer left out).
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLDSPL)")]).await;
    assert_eq!(
        o.stdout,
        [
            "LINE ONE",
            "  INDENTED",
            "A    B    C",
            "AFTER A BLANK LINE",
            "SPACEEAFTERL3NE 10",
            "PAGE TWO",
            "LAST"
        ]
    );

    // In a batch job the spooled file stays, named and with user data as
    // OVRPRTF says; the APIs read it. The print data is SCS: a new line
    // (x'15') for each line moved down from line 1, a carriage return
    // (x'0D') to print over a line, a form feed (x'0C') at a new page (SCS
    // control codes); RPGUnit's acceptance tests find each line of its
    // report right after a new line.
    let b = jobs::submit(
        &client,
        &jobs::Submit {
            name: "SPLAPI".into(),
            user: "QSECOFR".into(),
            command: format!("CALL PGM({lib}/GLDSPD)"),
            jobq: "SPLAPIQ".into(),
            curlib: Some(lib.clone()),
            libl: vec![lib.clone()],
            submitted_by: "QSECOFR".into(),
            submitter: None,
        },
    )
    .await
    .unwrap();
    let info = jobs::claim(&client, b.number).await.unwrap().unwrap();
    assert!(jobs::run_batch_job(&env, &info).await.unwrap());
    let rows = client
        .query(
            &format!("SELECT TRIM(ID), TRIM(VAL) FROM {lib}.GLDSPO"),
            &[],
        )
        .await
        .unwrap();
    let mut errs: Vec<(String, String)> = rows
        .iter()
        .filter(|r| r.get::<_, String>(0).starts_with('E'))
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    errs.sort();
    assert!(errs.iter().all(|e| e.1 == "OK"), "{errs:?}");
    assert_eq!(errs.len(), 8);
    let hex = |p: char| -> Vec<u8> {
        let mut parts: Vec<(i64, String)> = rows
            .iter()
            .filter(|r| r.get::<_, String>(0).starts_with(p))
            .map(|r| {
                let id: String = r.get(0);
                (id[1..].parse().unwrap(), r.get(1))
            })
            .collect();
        parts.sort();
        let h: String = parts.into_iter().map(|p| p.1).collect();
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    };
    let e = |s: &str| as4tainr_core::rpg::data::e(s);
    // OSPL0300: job name, user, number, then the spooled file's name.
    let rec = hex('R');
    assert_eq!(&rec[0..10], &e("SPLAPI    ")[..]);
    assert_eq!(&rec[26..36], &e("GLDSPL    ")[..]);
    assert_eq!(&rec[68..78], &e("GLDSPL    ")[..]);
    // SPFR0300 as IBM i 7.5 writes it (PUB400 2026-10-26, tools/golden/
    // spool.txt s_dump, GLDSPD's dump of GLDSPL): header size 64,
    // structure level 0200, spooled file level V6R1M0, complete 'C', the
    // size used, the offset to the first buffer 128 (for SPFR0300 the
    // print data itself), first page 1 at offset 0 of the print data.
    let sp = hex('S');
    let i4 = |o: usize| i32::from_be_bytes(sp[o..o + 4].try_into().unwrap());
    assert_eq!(i4(64), 64);
    assert_eq!(&sp[68..72], &e("0200")[..]);
    assert_eq!(&sp[72..78], &e("V6R1M0")[..]);
    assert_eq!(sp[86], e("C")[0]);
    let used = i4(88) as usize;
    assert_eq!(used, sp.len());
    assert_eq!(i4(92), 128);
    assert_eq!(i4(104) as usize, sp.len() - 128);
    assert_eq!((i4(108), i4(112), i4(116)), (2, 1, 0));
    // The print data: each line's text then a carriage return, new lines
    // before it for each line moved down, the line printed over the one
    // before right after its carriage return, a form feed after each
    // page (the same capture; its NUL bytes, absolute vertical positions
    // for skips and PUB400's page footer are not written here:
    // fidelity-gaps).
    let mut want = Vec::new();
    for (moves, text) in [
        (1, "LINE ONE"),
        (1, "  INDENTED"),
        (1, "A    B    C"),
        (2, "AFTER A BLANK LINE"),
        (4, "SKIPPED TO LINE 10"),
        (0, "SPACE AFTER 3"),
    ] {
        want.extend(std::iter::repeat_n(0x15u8, moves));
        want.extend(e(text));
        want.push(0x0D);
    }
    want.push(0x0C);
    want.extend(e("PAGE TWO"));
    want.push(0x0D);
    want.push(0x15);
    want.extend(e("LAST"));
    want.push(0x0D);
    want.push(0x0C);
    assert_eq!(&sp[128..], &want[..]);
    assert!(
        sp.ends_with(&want),
        "{:02X?}",
        &sp[sp.len().saturating_sub(want.len() + 8)..]
    );
}

#[tokio::test]
async fn environment_variables_and_qshell_from_cl() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // QSH runs commands only on a system set up for it.
    std::env::set_var("AS4TAINR_QSH", "1");
    let dir = format!("/tmp/{lib}q");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    job.run(&format!(
        "RUNSQL SQL('CREATE TABLE {lib}/GLDQSO (ID CHAR(10) NOT NULL, VAL CHAR(150) NOT NULL)') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    // tools/golden/qsh.txt's program.
    for (f, src) in [
        (
            "gldqsh.clle",
            include_str!("../../tools/golden/qsh/gldqsh.clle"),
        ),
        (
            "gldcas.cmd",
            include_str!("../../tools/golden/qsh/gldcas.cmd"),
        ),
        (
            "gldcasc.clle",
            include_str!("../../tools/golden/qsh/gldcasc.clle"),
        ),
    ] {
        let src = src
            .replace("/home/AS4TRAINR/gld", &dir)
            .replace("AS4TRAINR1", &lib);
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            src.as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    // `OBJ(LIB/*ALL)`: the slash qualifies, it does not start a comment.
    cl_cmd(
        &mut job,
        format!("CRTBNDCL PGM({lib}/GLDQSH) SRCSTMF('{dir}/gldqsh.clle')"),
    )
    .await
    .unwrap();
    cl_cmd(&mut job, format!("CALL PGM({lib}/GLDQSH)"))
        .await
        .unwrap();
    let rows = client
        .query(
            &format!("SELECT rtrim(id), rtrim(val) FROM {lib}.gldqso ORDER BY rrn"),
            &[],
        )
        .await
        .unwrap();
    let got: std::collections::BTreeMap<String, String> =
        rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    // Captured on PUB400 2026-10-23 (tools/golden/qsh.txt) unless noted;
    // the IDs of the escape messages are also in the CL reference
    // (ADDENVVAR, CHGENVVAR, RMVENVVAR).
    for (k, v) in [
        ("dupadd", "CPFA980"),
        ("rmvmissing", "CPFA981"),
        ("chgmissing", "CPFA981"),
        ("badname", "CPFA982"),
        // QSH0005 and its exit status in 4 binary bytes; QSH0006 and the
        // signal (Qshell, "Running Qshell commands from CL").
        ("exit3", "QSH0005 4 00000003"),
        ("exit0", "QSH0005 4 00000000"),
        ("signal9", "QSH0006 4 00000009"),
        ("esc0", "QSH0005 4 00000000"),
        // With QIBM_QSH_CMD_ESCAPE_MSG=Y, exit 4 is an escape QSH0005 no
        // MONMSG CPF0000 names: the function check CPF9999, whose data
        // starts with QSH0005 (PUB400: %BIN of its first 4 bytes is
        // x'D8E2C8F0', "QSH0").
        // Data shorter than a *CHAR field: what there is.
        ("shortdata", "<AB>"),
        // *BCAT joins with one blank, *TCAT drops the first operand's
        // trailing blanks (CL reference, expressions).
        ("exprtext", "Gold textx"),
    ] {
        assert_eq!(got.get(k).map(String::as_str), Some(v), "{k}: {got:?}");
    }
    assert!(
        got["esc4"].starts_with("CPF9999 ") && got["esc4"].ends_with("D8E2C8F0F0F0F5"),
        "{got:?}"
    );
    // The completion messages and their data: the name's length, then
    // the name, in apostrophes for ADDENVVAR (also REPLACE(*YES)) and
    // RMVENVVAR (PUB400 2026-10-26, tools/golden/qsh.txt q_out).
    for (k, v) in [
        ("addcomp", "CPCA980 12 000000087DC7D3C4E5C1D97D"),
        ("chgcomp", "CPCA981 10 00000006C7D3C4E5C1D9"),
        ("replcomp", "CPCA980 12 000000087DC7D3C4E5C1D97D"),
        ("rmvcomp", "CPCA988 12 000000087DC7D3C4E5C1D97D"),
    ] {
        assert_eq!(got.get(k).map(String::as_str), Some(v), "{k}: {got:?}");
    }
    // QIBM_QSH_CMD_OUTPUT=FILE=: the output in the stream file, which
    // CPYFRMSTMF (with STMFCODPAG, the old name of STMFCCSID) copies
    // into a program-described file, a record per line.
    cl_cmd(&mut job, format!("CRTPF FILE({lib}/QSHOUT) RCDLEN(20) OPTION(*NOLIST *NOSRC) MAXMBRS(*NOMAX) SIZE(*NOMAX)"))
        .await
        .unwrap();
    cl_cmd(
        &mut job,
        format!(
            "CPYFRMSTMF FROMSTMF('{dir}/qshout.txt') TOMBR('/QSYS.LIB/{lib}.LIB/QSHOUT.FILE/QSHOUT.MBR') MBROPT(*REPLACE) STMFCODPAG(*STMF)"
        ),
    )
    .await
    .unwrap();
    let rows = client
        .query(
            &format!("SELECT qshout FROM {lib}.qshout ORDER BY rrn"),
            &[],
        )
        .await
        .unwrap();
    let lines: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(
        lines,
        [format!("{:<20}", "hello"), format!("{:<20}", "there")]
    );
    // QSH's output by default: standard output (what `system` prints).
    // The job's variables stay after the program ends.
    cl_cmd(&mut job, "RMVENVVAR ENVVAR(QIBM_QSH_CMD_OUTPUT)".into())
        .await
        .unwrap();
    let m = cl_cmd(&mut job, "QSH CMD('echo plain; exit 2')".into())
        .await
        .unwrap();
    // PUB400 (tools/golden/qsh.txt q_stdout): the output, then QSH0005.
    assert_eq!(
        m,
        ["plain", "QSH0005: The command ended with exit status 2."]
    );
    // A command processing program's completion message to its caller
    // is on the caller's queue once: a CL caller receives it, then the
    // next one (QshOni's QSHEXECC loops RCVMSG *COMP for QSH0005).
    cl_cmd(
        &mut job,
        format!("CRTBNDCL PGM({lib}/GLDCASC) SRCSTMF('{dir}/gldcasc.clle')"),
    )
    .await
    .unwrap();
    for c in [
        format!("CRTSRCPF FILE({lib}/GLDQCMD) RCDLEN(92)"),
        format!("CPYFRMSTMF FROMSTMF('{dir}/gldcas.cmd') TOMBR('/QSYS.LIB/{lib}.LIB/GLDQCMD.FILE/GLDCAS.MBR') MBROPT(*REPLACE)"),
        format!("CRTCMD CMD({lib}/GLDCAS) PGM({lib}/GLDCASC) SRCFILE({lib}/GLDQCMD)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let caller = format!(
        "PGM
  DCL &ID *CHAR 7
  DCL &M *CHAR 80
  {lib}/GLDCAS VAL('MiXed') WORD(lower)
  QSH CMD('exit 0')
  RCVMSG MSGTYPE(*COMP) MSG(&M) MSGID(&ID)
  SNDPGMMSG MSG('first' *BCAT &ID *BCAT &M) MSGTYPE(*COMP)
  RCVMSG MSGTYPE(*COMP) MSGID(&ID)
  SNDPGMMSG MSG('second' *BCAT &ID) MSGTYPE(*COMP)
ENDPGM
"
    );
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/gldcall.clle"),
        caller.as_bytes(),
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    cl_cmd(
        &mut job,
        format!("CRTBNDCL PGM({lib}/GLDCALL) SRCSTMF('{dir}/gldcall.clle')"),
    )
    .await
    .unwrap();
    let m = cl_cmd(&mut job, format!("CALL PGM({lib}/GLDCALL)"))
        .await
        .unwrap();
    // PARM CASE(*MONO): a value in apostrophes keeps its case, a word
    // is in upper case (CL reference, PARM CASE).
    assert!(
        m.iter()
            .any(|l| l == "first CPF9898 VAL=<MiXed     > WORD=<LOWER     >."
                || l == "first VAL=<MiXed     > WORD=<LOWER     >"),
        "{m:?}"
    );
    assert!(m.iter().any(|l| l == "second QSH0005"), "{m:?}");
    // CHGLIB: the library's text.
    cl_cmd(&mut job, format!("CHGLIB LIB({lib}) TEXT('Changed text')"))
        .await
        .unwrap();
    let t: String = client
        .query_one(
            "SELECT text FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*LIB'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(t, "Changed text");
    // Nothing changed: CPC2105 (PUB400 2026-10-23, q_chglib).
    let m = cl_cmd(&mut job, format!("CHGLIB LIB({lib}) TEXT(*SAME)"))
        .await
        .unwrap();
    assert!(m[0].starts_with("CPC2105: "), "{m:?}");
    let e = cl_cmd(&mut job, "CHGLIB LIB(NOSUCHLIB9) TEXT(*SAME)".into())
        .await
        .unwrap_err();
    assert!(e.contains("CPF2110"), "{e}");
}

#[tokio::test]
async fn cl_and_rpg_forms_from_qshoni() {
    // Forms QshOni's sources use (built by its own build.sh, docs/import.md).
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}f");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    cl_cmd(
        &mut job,
        format!("RUNSQL SQL('CREATE TABLE {lib}/QFO (N INTEGER)') COMMIT(*NONE)"),
    )
    .await
    .unwrap();
    let src = format!(
        "PGM
  DCL &T *CHAR 10 VALUE('*COMP')
  DCL &C *CHAR 60 VALUE('CRTDTAARA DTAARA({lib}/QFOCMD) TYPE(*CHAR) LEN(5)')
  DCL &L *DEC (15 5) VALUE(60)
  DCL &X *CHAR 10
  DCL &X *CHAR 10
  /* CL programming, selective prompting: ?? and ?* mark parameters  */
  /* the prompt would show; the values given are the command's.      */
  ?CRTDTAARA ??DTAARA({lib}/QFOSEL) ?*TYPE(*CHAR) LEN(3)
  /* QCMDEXC (CL reference, APIs): the command in its first          */
  /* parameter, its length in the second.                            */
  CALL QCMDEXC PARM(&C &L)
  CHKOBJ {lib}/QFOCMD *DTAARA
  CHKOBJ {lib}/QFOSEL *DTAARA
  /* An escape no MONMSG names is a function check, CPF9999, which   */
  /* MONMSG CPF0000 handles (CL programming, unmonitored messages).  */
  RUNSQL SQL('SELEKT 1') COMMIT(*NONE)
  MONMSG CPF0000 EXEC(SNDPGMMSG MSG('function check') MSGTYPE(&T))
  /* QSYS2.NETSTAT_JOB_INFO and its columns.                         */
  RUNSQL SQL('INSERT INTO {lib}/QFO SELECT COUNT(*) FROM +
    QSYS2.NETSTAT_JOB_INFO WHERE LOCAL_PORT > 0 AND +
    JOB_NAME IS NOT NULL OR REMOTE_PORT IS NOT NULL') COMMIT(*NONE)
  SNDPGMMSG MSG('done') MSGTYPE(&T)
ENDPGM
"
    );
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/qfo.clle"),
        src.as_bytes(),
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    // The same declaration twice is taken (QshOni's QSHCPYSRCC); a
    // different one is still an error.
    cl_cmd(
        &mut job,
        format!("CRTBNDCL PGM({lib}/QFO) SRCSTMF('{dir}/qfo.clle')"),
    )
    .await
    .unwrap();
    let m = cl_cmd(&mut job, format!("CALL PGM({lib}/QFO)"))
        .await
        .unwrap();
    assert!(m.iter().any(|l| l == "function check"), "{m:?}");
    assert!(m.iter().any(|l| l == "done"), "{m:?}");
    let n: i64 = client
        .query_one(&format!("SELECT count(*) FROM {lib}.qfo"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/qfo2.clle"),
        b"PGM\n DCL &X *CHAR 10\n DCL &X *CHAR 11\nENDPGM\n",
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    cl_cmd(
        &mut job,
        format!("CRTBNDCL PGM({lib}/QFO2) SRCSTMF('{dir}/qfo2.clle')"),
    )
    .await
    .unwrap_err();

    // A source physical file read and updated by record (CL reference,
    // CRTSRCPF: the fields SRCSEQ, zoned 6,2, SRCDAT, zoned 6,0, and
    // SRCDTA; the record format is named like the file, so RPG renames
    // it).
    for c in [
        format!("CRTSRCPF FILE({lib}/QFOSRC) RCDLEN(92)"),
        format!(
            "CPYFRMSTMF FROMSTMF('{dir}/qfo2.clle') TOMBR('/QSYS.LIB/{lib}.LIB/QFOSRC.FILE/M1.MBR') MBROPT(*REPLACE)"
        ),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let rpg = "**free
dcl-f QFOSRC disk(*ext) usage(*update) rename(QFOSRC:SRCR);
// ILE RPG reference, %LEN: a constant's length at compile time.
dcl-c LEN6 %len('abcdef');
dcl-s n int(10);
dsply %char(LEN6);
read QFOSRC;
dow not %eof(QFOSRC);
  n += 1;
  dsply (%char(SRCSEQ) + ' ' + %char(SRCDAT) + ' ' + %trimr(SRCDTA));
  if n = 2;
    SRCDTA = ' DCL &Y *CHAR 1';
    update SRCR;
  endif;
  read QFOSRC;
enddo;
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "QFORLA", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "6",
            "1.00 0 PGM",
            "2.00 0  DCL &X *CHAR 10",
            "3.00 0  DCL &X *CHAR 11",
            "4.00 0 ENDPGM"
        ]
    );
    let src: String = client
        .query_one(
            "SELECT source FROM as4tainr.source_members WHERE library=$1 AND file='QFOSRC' AND member='M1'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(src, "PGM\n DCL &Y *CHAR 1\n DCL &X *CHAR 11\nENDPGM\n");

    // ILE RPG reference, program-described files: the input
    // specifications define the fields whether or not the file exists
    // when the program is compiled.
    let rpg = "     FNOFILE    IF   F  100        DISK    USROPN
     INOFILE    AA  01
     I                                  1   10  FLD1
     C                   EVAL      FLD1 = 'bound'
     C     FLD1          DSPLY
     C                   EVAL      *INLR = *ON
";
    let r = run_rpg(&url, &lib, "QSECOFR", "QFOISP", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["bound"]);
    // ILE RPG Reference, LIKEDS: the new data structure has the
    // template's length, LEN included (found by tools/golden/spool's
    // GLDSPD, whose list entry DS is LEN(200)).
    let rpg = "**free
dcl-ds rec len(20) qualified;
  a char(3) pos(1);
  b char(4) pos(11);
end-ds;
dcl-ds one likeds(rec);
dsply %char(%size(one));
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "QFOLDS", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["20"]);
}

#[tokio::test]
async fn qsh_commands_work_on_the_ifs() {
    // Qshell's utilities work on the integrated file system (Qshell,
    // "Qshell and the integrated file system"): a command reads the
    // stream files there and what it writes, makes or removes is there
    // after it ends; it starts in the user's home directory.
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    if !as4tainr_core::qshfs::available() {
        eprintln!("skipped: no mount namespaces here");
        return;
    }
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    std::env::set_var("AS4TAINR_QSH", "1");
    let dir = format!("/tmp/{lib}i");
    as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR")
        .await
        .unwrap();
    for (f, data) in [("in.txt", "one\ntwo\nthree\n"), ("old.txt", "x\n")] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            data.as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    cl_cmd(&mut job, "RMVENVVAR ENVVAR(QIBM_QSH_CMD_OUTPUT)".into())
        .await
        .ok();
    let m = cl_cmd(
        &mut job,
        format!(
            "QSH CMD('cd {dir} && wc -l < in.txt && sort in.txt > out.txt && mkdir sub && echo deep > sub/f.txt && rm old.txt && echo end >> in.txt')"
        ),
    )
    .await
    .unwrap();
    assert_eq!(m, ["3", "QSH0005: The command ended with exit status 0."]);
    let read = |p: String| {
        let client = &client;
        async move {
            as4tainr_core::ifs::read_file(client, &p)
                .await
                .map(|(b, _)| String::from_utf8(b).unwrap())
        }
    };
    assert_eq!(
        read(format!("{dir}/out.txt")).await.unwrap(),
        "one\nthree\ntwo\n"
    );
    assert_eq!(read(format!("{dir}/sub/f.txt")).await.unwrap(), "deep\n");
    assert_eq!(
        read(format!("{dir}/in.txt")).await.unwrap(),
        "one\ntwo\nthree\nend\n"
    );
    assert!(read(format!("{dir}/old.txt")).await.is_err());
    // A file that was there keeps its CCSID; a new one is tagged 1208.
    let ccsid = |p: String| {
        let client = &client;
        async move {
            as4tainr_core::ifs::stat(client, &p)
                .await
                .unwrap()
                .unwrap()
                .ccsid
        }
    };
    assert_eq!(ccsid(format!("{dir}/in.txt")).await, 819);
    assert_eq!(ccsid(format!("{dir}/out.txt")).await, 1208);
    // A change made in the IFS after one command is what the next sees.
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/in.txt"),
        b"changed\n",
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    let m = cl_cmd(&mut job, format!("QSH CMD('cat {dir}/in.txt; ls {dir}')"))
        .await
        .unwrap();
    assert_eq!(
        m,
        [
            "changed",
            "in.txt",
            "out.txt",
            "sub",
            "QSH0005: The command ended with exit status 0."
        ]
    );
}

#[tokio::test]
async fn character_keys_are_in_ebcdic_order() {
    // IBM i keeps character fields in CCSID 37 and orders keys, SQL ORDER
    // BY and comparisons by those bytes (sort sequence *HEX): blank x'40'
    // first, then lowercase (x'81'...), uppercase (x'C1'...), digits
    // (x'F0'...) (CCSID 37 code page; CRTPF SRTSEQ(*HEX)).
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::sql::{self, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}e");
    as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR")
        .await
        .unwrap();
    let dds = "     A          R EKEYR\n     A            K              4A\n     A          K K\n";
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/ekey.pf"),
        dds.as_bytes(),
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    for c in [
        format!("CRTSRCPF FILE({lib}/QDDSSRCE) RCDLEN(92)"),
        format!(
            "CPYFRMSTMF FROMSTMF('{dir}/ekey.pf') TOMBR('/QSYS.LIB/{lib}.LIB/QDDSSRCE.FILE/EKEY.MBR') MBROPT(*REPLACE)"
        ),
        format!("CRTPF FILE({lib}/EKEY) SRCFILE({lib}/QDDSSRCE)"),
        format!(
            "RUNSQL SQL('INSERT INTO {lib}/EKEY VALUES (''1''), (''B''), (''a''), (''A''), (''b''), ('' x''), (''Z9''), (''ZZ'')') COMMIT(*NONE)"
        ),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let want = [" x", "a", "b", "A", "B", "ZZ", "Z9", "1"];
    // Record-level access: READ in key order, SETLL then READ.
    let rpg = "**free
dcl-f EKEY keyed;
read EKEY;
dow not %eof(EKEY);
  dsply K;
  read EKEY;
enddo;
setll 'A' EKEY;
read EKEY;
dsply ('from A: ' + K);
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "EKEYRD", rpg, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    let mut expect: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    expect.push("from A: A".into());
    assert_eq!(r.messages, expect);
    // SQL: ORDER BY and a comparison.
    let libl = vec![lib.clone()];
    let col = |o: Outcome| match o {
        Outcome::Rows { rows, .. } => rows
            .into_iter()
            .map(|r| r[0].trim_end().to_string())
            .collect::<Vec<_>>(),
        other => panic!("expected rows, got {other:?}"),
    };
    let r = col(sql::run(
        &client,
        &url,
        &libl,
        &format!("SELECT K FROM {lib}/EKEY ORDER BY K"),
    )
    .await
    .unwrap());
    assert_eq!(r, want);
    let r = col(sql::run(
        &client,
        &url,
        &libl,
        &format!("SELECT K FROM {lib}/EKEY WHERE K > 'ZZ' ORDER BY K"),
    )
    .await
    .unwrap());
    assert_eq!(r, ["Z9", "1"]);
}

#[tokio::test]
async fn object_and_member_lists_are_in_ebcdic_order() {
    // Lists of objects, libraries and members come in the order of their
    // names' CCSID 37 bytes, letters (x'C1'...) before digits (x'F0'...):
    // AB before A1 (sort sequence *HEX; CCSID 37 code page). Byte order
    // of ASCII would put A1 first.
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for c in [
        format!("CRTDTAARA DTAARA({lib}/A1) TYPE(*CHAR) LEN(1)"),
        format!("CRTDTAARA DTAARA({lib}/AB) TYPE(*CHAR) LEN(1)"),
        format!("CRTDTAARA DTAARA({lib}/A) TYPE(*CHAR) LEN(1)"),
        format!("CRTSRCPF FILE({lib}/QSRCO) RCDLEN(92)"),
        format!("ADDPFM FILE({lib}/QSRCO) MBR(M1)"),
        format!("ADDPFM FILE({lib}/QSRCO) MBR(MB)"),
        format!("ADDPFM FILE({lib}/QSRCO) MBR(M)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let objs: Vec<String> = as4tainr_core::catalog::list_objects(&client, &lib)
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.objtype == "*DTAARA")
        .map(|o| o.name)
        .collect();
    assert_eq!(objs, ["A", "AB", "A1"]);
    let mbrs: Vec<String> = as4tainr_core::source::members(&client, &lib, "QSRCO")
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.0)
        .collect();
    assert_eq!(mbrs, ["M", "MB", "M1"]);
    // ADDPFM: the member's type and text; a member that is there already
    // is CPF5812, then the escape CPF7306 (ADDPFM's messages in IBM's
    // command documentation; CL programs monitor for both).
    cl_cmd(
        &mut job,
        format!("ADDPFM FILE({lib}/QSRCO) MBR(MT) TEXT('With a type') SRCTYPE(CLLE)"),
    )
    .await
    .unwrap();
    let mt = as4tainr_core::source::members(&client, &lib, "QSRCO")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.0 == "MT")
        .unwrap();
    assert_eq!((mt.1.as_str(), mt.2.as_str()), ("CLLE", "With a type"));
    let e = cl_cmd(&mut job, format!("ADDPFM FILE({lib}/QSRCO) MBR(M1)"))
        .await
        .unwrap_err();
    assert!(e.contains("CPF5812") && e.contains("CPF7306"), "{e}");
    assert!(e.find("CPF5812") < e.find("CPF7306"), "{e}");
    // What a user sees: DSPLIB's list.
    let out = cl_cmd(&mut job, format!("DSPLIB LIB({lib})"))
        .await
        .unwrap()
        .join("\n");
    let at = |n: &str| {
        out.find(&format!("\"{n:<10}  *DTAARA"))
            .unwrap_or_else(|| panic!("{n} not in {out}"))
    };
    assert!(at("AB") < at("A1"), "{out}");
}

#[tokio::test]
async fn each_job_has_its_own_qtemp() {
    // IBM i work management: QTEMP is a temporary library each job has
    // of its own, empty when the job starts; no other job sees it, and it
    // is deleted when the job ends.
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::qtemp;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let job = |key: &str| Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QTEMP".into()],
        display: None,
        id: Some(as4tainr_core::jobs::JobId {
            number: key.parse().unwrap(),
            user: "QSECOFR".into(),
            name: "QTEMPTEST".into(),
        }),
        calls: Default::default(),
    };
    // Job numbers no real job of the tests has.
    let (ka, kb) = ("987001", "987002");
    let mut a = job(ka);
    let key_a = a.job_key();
    let real_a = qtemp::name_for(&key_a);
    let url2 = url.clone();
    let lib2 = lib.clone();
    qtemp::scope(&client, &key_a, "QSECOFR", async {
        let m = cl_cmd(&mut a, "CRTPF FILE(QTEMP/TMPF) RCDLEN(5)".into())
            .await
            .unwrap();
        // What the job is told names QTEMP.
        assert_eq!(m[0], "CPC7301: File TMPF was created in library QTEMP.");
        cl_cmd(
            &mut a,
            "RUNSQL SQL('INSERT INTO QTEMP/TMPF VALUES(''abc'')') COMMIT(*NONE)".into(),
        )
        .await
        .unwrap();
        cl_cmd(&mut a, "CHKOBJ OBJ(QTEMP/TMPF) OBJTYPE(*FILE)".into())
            .await
            .unwrap();
        // SQL and RPG find it there too (QTEMP.TMPF, the library list).
        let r = as4tainr_core::sql::run(
            &client,
            &url2,
            &a.library_list(),
            "SELECT TMPF FROM QTEMP.TMPF",
        )
        .await
        .unwrap();
        match r {
            as4tainr_core::sql::Outcome::Rows { rows, .. } => assert_eq!(rows[0][0].trim(), "abc"),
            o => panic!("{o:?}"),
        }
        let rpg = "     FTMPF      IF   F    5        DISK
     ITMPF      AA  01
     I                                  1    5  FLD
     C                   READ      TMPF                                   99
     C     FLD           DSPLY
     C                   EVAL      *INLR = *ON
";
        let r = as4tainr_core::rpg::call(
            &url2,
            a.library_list(),
            "QSECOFR".into(),
            None,
            None,
            None,
            "QTMPRD".into(),
            rpg.into(),
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.messages, ["abc"]);
        // The library list keeps QTEMP by that name (RMVLIBLE, ADDLIBLE).
        let m = cl_cmd(&mut a, "RMVLIBLE LIB(QTEMP)".into()).await.unwrap();
        assert_eq!(m, ["Library QTEMP removed from library list."]);
        cl_cmd(&mut a, "ADDLIBLE LIB(QTEMP) POSITION(*LAST)".into())
            .await
            .unwrap();
        // Another job does not see it.
        let mut b = job(kb);
        let key_b = b.job_key();
        qtemp::scope(&client, &key_b, "QSECOFR", async {
            let e = cl_cmd(&mut b, "CHKOBJ OBJ(QTEMP/TMPF) OBJTYPE(*FILE)".into())
                .await
                .unwrap_err();
            assert!(e.contains("CPF9801"), "{e}");
        })
        .await;
        let _ = lib2;
        // CLRLIB QTEMP empties the job's own.
        cl_cmd(&mut a, "CLRLIB LIB(QTEMP)".into()).await.unwrap();
        let e = cl_cmd(&mut a, "CHKOBJ OBJ(QTEMP/TMPF) OBJTYPE(*FILE)".into())
            .await
            .unwrap_err();
        assert!(e.contains("CPF9801"), "{e}");
    })
    .await;
    // The job ended: its QTEMP is gone.
    assert!(!as4tainr_core::catalog::library_exists(&client, &real_a)
        .await
        .unwrap());
}

#[tokio::test]
async fn save_changed_objects() {
    // CL reference (SAVCHGOBJ): the objects changed since the library's
    // last SAVLIB (REFDATE(*SAVLIB)) or since REFDATE/REFTIME; with
    // OBJJRN(*NO) a file journaled since before then is not saved; when
    // nothing is saved, CPF3770 (SAVCHGOBJ's messages).
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    let to = format!("{lib}C");
    for c in [
        format!("CRTLIB {to}"),
        sql(format!("CREATE TABLE {lib}.CHA (C CHAR(5))")),
        sql(format!("CREATE TABLE {lib}.CHB (C CHAR(5))")),
        sql(format!("CREATE TABLE {lib}.CHC (C CHAR(5))")),
        sql(format!("CREATE TABLE {lib}.CHD (C CHAR(5))")),
        sql(format!("CREATE TABLE {lib}.CHJ (C CHAR(5))")),
        sql(format!("INSERT INTO {lib}.CHB VALUES ('b1'), ('b2')")),
        sql(format!("INSERT INTO {lib}.CHC VALUES ('c1')")),
        sql(format!("INSERT INTO {lib}.CHD VALUES ('d1')")),
        format!("CRTDTAARA DTAARA({lib}/DTA) TYPE(*CHAR) LEN(5)"),
        format!("CRTDTAARA DTAARA({lib}/DTB) TYPE(*CHAR) LEN(5)"),
        format!("CRTSRCPF FILE({lib}/QCHSRC) RCDLEN(92)"),
        format!("CRTJRNRCV JRNRCV({lib}/CHR0001)"),
        format!("CRTJRN JRN({lib}/CHJRN) JRNRCV({lib}/CHR0001)"),
        format!("STRJRNPF FILE({lib}/CHJ) JRN({lib}/CHJRN)"),
        format!("CRTSAVF FILE({to}/ALL)"),
        format!("CRTSAVF FILE({to}/CHG)"),
        format!("CRTSAVF FILE({to}/CHGJ)"),
        format!("CRTSAVF FILE({to}/OLD)"),
        format!("SAVLIB LIB({lib}) DEV(*SAVF) SAVF({to}/ALL)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    // Nothing changed since the SAVLIB.
    let e = cl_cmd(
        &mut job,
        format!("SAVCHGOBJ OBJ(*ALL) LIB({lib}) DEV(*SAVF) SAVF({to}/CHG)"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF3770"), "{e}");
    for c in [
        sql(format!("INSERT INTO {lib}.CHA VALUES ('a1')")),
        sql(format!("DELETE FROM {lib}.CHB WHERE C = 'b1'")),
        sql(format!("UPDATE {lib}.CHD SET C = 'd2'")),
        sql(format!("INSERT INTO {lib}.CHJ VALUES ('j1')")),
        format!("CHGDTAARA DTAARA({lib}/DTA) VALUE('new')"),
        format!("ADDPFM FILE({lib}/QCHSRC) MBR(NEWMBR)"),
        format!("CRTDTAARA DTAARA({lib}/DTN) TYPE(*CHAR) LEN(5)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let out = cl_cmd(
        &mut job,
        format!("SAVCHGOBJ OBJ(*ALL) LIB({lib}) DEV(*SAVF) SAVF({to}/CHG)"),
    )
    .await
    .unwrap();
    assert!(out.iter().any(|l| l.starts_with("CPC3722: 6 ")), "{out:?}");
    cl_cmd(
        &mut job,
        format!("RSTOBJ OBJ(*ALL) SAVLIB({lib}) DEV(*SAVF) SAVF({to}/CHG) RSTLIB({to})"),
    )
    .await
    .unwrap();
    let have = |n: &'static str, t: &'static str| {
        let client = &client;
        let to = to.clone();
        async move {
            as4tainr_core::catalog::object_exists(client, &[to], n, t)
                .await
                .unwrap()
        }
    };
    for n in ["CHA", "CHB", "CHD", "QCHSRC"] {
        assert!(have(n, "*FILE").await, "{n} changed");
    }
    assert!(have("DTA", "*DTAARA").await, "DTA changed");
    assert!(have("DTN", "*DTAARA").await, "DTN made after the SAVLIB");
    assert!(!have("CHC", "*FILE").await, "CHC not changed");
    assert!(!have("DTB", "*DTAARA").await, "DTB not changed");
    assert!(!have("CHJ", "*FILE").await, "CHJ journaled: OBJJRN(*NO)");
    // OBJJRN(*YES) saves the journaled file that changed too.
    let out = cl_cmd(
        &mut job,
        format!(
            "SAVCHGOBJ OBJ(CH*) LIB({lib}) DEV(*SAVF) OBJTYPE(*FILE) OBJJRN(*YES) SAVF({to}/CHGJ)"
        ),
    )
    .await
    .unwrap();
    assert!(out.iter().any(|l| l.starts_with("CPC3722: 4 ")), "{out:?}");
    // REFDATE: since January 1, 1980 (MMDDYY, the job's date format),
    // everything; since the end of 2039, nothing.
    let out = cl_cmd(
        &mut job,
        format!(
            "SAVCHGOBJ OBJ(*ALL) LIB({lib}) DEV(*SAVF) OBJTYPE(*DTAARA) REFDATE(010180) REFTIME(000000) SAVF({to}/OLD)"
        ),
    )
    .await
    .unwrap();
    assert!(out.iter().any(|l| l.starts_with("CPC3722: 3 ")), "{out:?}");
    let e = cl_cmd(
        &mut job,
        format!(
            "SAVCHGOBJ OBJ(*ALL) LIB({lib}) DEV(*SAVF) REFDATE('12/31/39') SAVF({to}/OLD) CLEAR(*ALL)"
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF3770"), "{e}");
}

#[tokio::test]
async fn duplicate_objects() {
    // CL reference, CRTDUPOBJ: copies of objects in another library or
    // under a new name (NEWOBJ, one object); DATA(*YES) copies a file's
    // records, *NO (the default) only the file; an object that is there
    // already is not duplicated (CPF2130: n objects duplicated, m not).
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let sql = |s: String| format!("RUNSQL SQL('{}') COMMIT(*NONE)", s.replace('\'', "''"));
    let to = format!("{lib}D");
    for c in [
        format!("CRTLIB {to}"),
        sql(format!(
            "CREATE TABLE {lib}.DUPA (ID INT PRIMARY KEY, N CHAR(5))"
        )),
        sql(format!("INSERT INTO {lib}.DUPA VALUES (1, 'a'), (2, 'b')")),
        format!("CRTDTAARA DTAARA({lib}/DUPDA) TYPE(*CHAR) LEN(5) VALUE('hello')"),
        format!("CRTDUPOBJ OBJ(DUPA) FROMLIB({lib}) OBJTYPE(*FILE) NEWOBJ(DUPB) DATA(*YES)"),
        format!("CRTDUPOBJ OBJ(DUPA) FROMLIB({lib}) OBJTYPE(*FILE) TOLIB({to})"),
        format!("CRTDUPOBJ OBJ(DUP*) FROMLIB({lib}) OBJTYPE(*DTAARA) TOLIB({to})"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        sql_value(
            &client,
            &url,
            &lib,
            &format!("SELECT COUNT(*) FROM {lib}.DUPB")
        )
        .await,
        "2",
        "DATA(*YES)"
    );
    assert_eq!(
        sql_value(
            &client,
            &url,
            &lib,
            &format!("SELECT COUNT(*) FROM {to}.DUPA")
        )
        .await,
        "0",
        "DATA(*NO)"
    );
    let v: String = client
        .query_one(
            "SELECT value FROM as4tainr.data_areas WHERE library=$1 AND name='DUPDA'",
            &[&to],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(v.trim(), "hello");
    let e = cl_cmd(
        &mut job,
        format!("CRTDUPOBJ OBJ(DUPA) FROMLIB({lib}) OBJTYPE(*FILE) TOLIB({to})"),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF2130"), "{e}");
}

#[tokio::test]
async fn job_descriptions() {
    // CL reference: CRTJOBD (INLLIBL, USER, JOBQ, TEXT), CHGJOBD (*SAME
    // keeps a value), DSPJOBD, DLTJOBD; SBMJOB JOBD names the job after
    // the job description (JOB(*JOBD)).
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for c in [
        format!("CRTJOBD JOBD({lib}/TESTJD) TEXT('Test jobs') INLLIBL({lib} QGPL)"),
        format!("CHGJOBD JOBD({lib}/TESTJD) USER(QSECOFR)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let d = as4tainr_core::jobd::find(&client, std::slice::from_ref(&lib), "TESTJD")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.inllibl, Some(vec![lib.clone(), "QGPL".to_string()]));
    assert_eq!(d.user, "QSECOFR");
    let shown = cl_cmd(&mut job, format!("DSPJOBD JOBD({lib}/TESTJD)"))
        .await
        .unwrap()
        .join("\n");
    // The initial library list in DSPJOBD's three columns (IBM i 7.5's
    // layout, PUB400 2026-10-28 verify12 n12_dspjobd; this test looked
    // for "LIB QGPL" on one line, a layout of as4tainr's own).
    // Sequence numbers 10, 20 (not captured; as DSPLIBL numbers them).
    let want = as4tainr_core::report::at(&[(6, "10"), (12, &lib), (50, "20"), (56, "QGPL")]);
    assert!(shown.contains(&want), "{shown}");
    let r = cl_cmd(
        &mut job,
        format!("SBMJOB CMD(DSPLIBL) JOBD({lib}/TESTJD) INLLIBL(*JOBD)"),
    )
    .await
    .unwrap()
    .join("\n");
    assert!(r.contains("/TESTJD"), "{r}");
    cl_cmd(&mut job, format!("DLTJOBD JOBD({lib}/TESTJD)"))
        .await
        .unwrap();
    assert!(
        as4tainr_core::jobd::find(&client, std::slice::from_ref(&lib), "TESTJD")
            .await
            .unwrap()
            .is_none()
    );
    // DSPJOBD of a job description that is not there: CPF1623 (PUB400
    // 2026-10-28, tools/golden/verify11.txt k_dspjobd).
    let e = cl_cmd(&mut job, format!("DSPJOBD JOBD({lib}/TESTJD)"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains(": CPF1623:"), "{e}");
}

#[tokio::test]
async fn save_and_restore_omit_and_select_objects() {
    // CL reference (SAVLIB, RSTOBJ, RSTLIB): OMITOBJ leaves objects out
    // (library/object, generic, *ALL; object type); SELECT *INCLUDE
    // selects and *OMIT leaves out (object, type, attribute, member).
    // QshOni's QSHSAVLIBC and QSHRSTOBJC give these with TGTRLS, PRECHK,
    // SAVACT, SPLFDTA, PVTAUT, STRJRN, FRCOBJCVN... too.
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let to = format!("{lib}R");
    for c in [
        format!("CRTLIB {to}"),
        format!("CRTPF FILE({lib}/SELA) RCDLEN(5)"),
        format!("CRTPF FILE({lib}/SELB) RCDLEN(5)"),
        format!("CRTDTAARA DTAARA({lib}/SELC) TYPE(*CHAR) LEN(1)"),
        format!("CRTDTAARA DTAARA({lib}/SELD) TYPE(*CHAR) LEN(1)"),
        format!("CRTSAVF FILE({to}/SELSF)"),
        format!(
            "SAVLIB LIB({lib}) DEV(*SAVF) SAVF({to}/SELSF) OMITOBJ(({lib}/SELB *FILE)) TGTRLS(*CURRENT) UPDHST(*NO) PRECHK(*NO) SAVACT(*NO) SAVFDTA(*YES) SPLFDTA(*NONE) QDTA(*NONE) DTACPR(*DEV)"
        ),
        format!(
            "RSTOBJ OBJ(*ALL) SAVLIB({lib}) DEV(*SAVF) OBJTYPE(*ALL) SAVF({to}/SELSF) OPTION(*ALL) FILEMBR((*ALL *ALL)) MBROPT(*ALL) SPLFDTA(*NEW) PVTAUT(*NO) STRJRN(*YES) ALWOBJDIF(*ALL) FRCOBJCVN(*SYSVAL) RSTLIB({to}) OMITOBJ((*NONE)) SELECT((*INCLUDE *ALL/SEL* *ALL *ALL *ALL) (*OMIT *ALL/SELD *DTAARA *ALL *ALL))"
        ),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let have = |n: &'static str, t: &'static str| {
        let client = &client;
        let to = to.clone();
        async move {
            as4tainr_core::catalog::object_exists(client, &[to], n, t)
                .await
                .unwrap()
        }
    };
    assert!(have("SELA", "*FILE").await);
    assert!(!have("SELB", "*FILE").await, "omitted when saved");
    assert!(have("SELC", "*DTAARA").await);
    assert!(!have("SELD", "*DTAARA").await, "omitted when restored");
    // DSPATR (CL reference): an IFS object's attributes.
    as4tainr_core::ifs::write_file(
        &client,
        &format!("/tmp/{lib}.txt"),
        b"abc",
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    let m = cl_cmd(&mut job, format!("DSPATR OBJ('/tmp/{lib}.txt')"))
        .await
        .unwrap();
    let text = m.join("\n");
    assert!(
        text.contains("*STMF") && text.contains("819") && text.contains("3"),
        "{text}"
    );
    // A member other than *ALL: whole files only here.
    let e = cl_cmd(
        &mut job,
        format!(
            "RSTOBJ OBJ(SELA) SAVLIB({lib}) DEV(*SAVF) SAVF({to}/SELSF) RSTLIB({to}) FILEMBR((SELA M1))"
        ),
    )
    .await
    .unwrap_err();
    assert!(e.contains("CPF9898"), "{e}");
}

/// QDBRTVFD (IBM documentation, "Retrieve Database File Description
/// (QDBRTVFD) API"): FILD0100's attribute bytes at offset 8 have x'0800'
/// for a source file, the number of members at offset 47 and the maximum
/// record length at 304; FILD0200's flag byte at offset 61 has x'04' when
/// every character field has one CCSID, the CCSID at 45 and the record
/// length at 66; the returned file name is the file and library used.
/// The flags are tested the way iRPGUnit's SrcMbr_getCcsid does it, with
/// hexadecimal constants as numeric values in %BITAND and a comparison
/// (ILE RPG takes hex literals as unsigned numbers there; the code
/// compiles and runs on IBM i).
#[tokio::test]
async fn retrieve_database_file_description() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    as4tainr_core::source::create_file(&client, &lib, "QFDSRC", "Source")
        .await
        .unwrap();
    as4tainr_core::source::put_member(
        &client,
        &as4tainr_core::source::Member {
            library: lib.clone(),
            file: "QFDSRC".into(),
            name: "ONE".into(),
            srctype: "RPGLE".into(),
            source: "x".into(),
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    let src = format!(
        r#"**free
dcl-pr QDBRTVFD extpgm('QDBRTVFD');
  rcv char(65535) options(*varsize);
  len int(10) const;
  ret char(20);
  fmt char(8) const;
  qfile char(20) const;
  rcdfmt char(10) const;
  ovr char(1) const;
  sys char(10) const;
  fmttype char(10) const;
  err char(16);
end-pr;
dcl-ds fh len(400) qualified;
  ret int(10) pos(1);
  flg uns(5) pos(9);
  mnum int(5) pos(48);
  mxrl int(5) pos(305);
end-ds;
dcl-ds ff len(256) qualified;
  rcid int(5) pos(46);
  flgs uns(3) pos(62);
  rlen int(10) pos(67);
end-ds;
dcl-c SRCBIT x'0800';
dcl-c CMNCCSID x'04';
dcl-s rf char(20);
dcl-s err char(16) inz(x'00000010');
QDBRTVFD(fh : %size(fh) : rf : 'FILD0100' : 'QFDSRC    {lib}' : '*FIRST' : '0' : '*LCL' : '*EXT' : err);
dsply (%trim(%subst(rf:1:10)) + ' ' + %trim(%subst(rf:11:10)) + ' ' + %char(fh.ret));
if %bitand(fh.flg : SRCBIT) = SRCBIT;
  dsply ('source ' + %char(fh.mnum) + ' ' + %char(fh.mxrl));
endif;
QDBRTVFD(ff : %size(ff) : rf : 'FILD0200' : 'QFDSRC    *LIBL' : '*FIRST' : '0' : '*LCL' : '*EXT' : err);
if %bitand(ff.flgs : CMNCCSID) = CMNCCSID;
  dsply ('ccsid ' + %char(ff.rcid) + ' ' + %char(ff.rlen));
endif;
*inlr = *on;
"#
    );
    let r = run_rpg(&url, &lib, "QSECOFR", "FDTEST", &src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            format!("QFDSRC {lib} 400"),
            "source 1 92".to_string(),
            "ccsid 37 92".to_string(),
        ]
    );
}

/// Timestamps with 0 to 12 fractional digits (ILE RPG Reference,
/// "Timestamp Data Type" and TIMESTAMP{(fractional-seconds)}): the
/// length is 19 plus a period and the digits; Z literals have 0 to 12
/// digits; assigning a timestamp to one with more digits adds zeros,
/// to one with fewer cuts the fraction; a date plus a time is a
/// timestamp (date and time arithmetic in expressions).
#[tokio::test]
async fn timestamp_precision() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-s t12 timestamp(12);
dcl-s t6 timestamp;
dcl-s t0 timestamp(0);
t12 = Z'2024-09-16-11.22.33.123456789012';
dsply (%char(t12) + ' ' + %char(%size(t12)));
t6 = t12;
t0 = t12;
dsply (%char(t6) + ' ' + %char(t0) + ' ' + %char(%size(t0)));
t6 = %date('2024-09-16': *iso) + %time('11.22.33': *iso);
dsply %char(t6);
t12 = Z'2024-09-16-11.22.33.5';
dsply %char(t12);
if t12 = Z'2024-09-16-11.22.33.500000000000';
  dsply 'equal';
endif;
*inlr = *on;
"#;
    let r = run_rpg(&url, &lib, "QSECOFR", "TSPREC", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(
        r.messages,
        [
            "2024-09-16-11.22.33.123456789012 32",
            "2024-09-16-11.22.33.123456 2024-09-16-11.22.33 19",
            "2024-09-16-11.22.33.000000",
            "2024-09-16-11.22.33.500000000000",
            "equal",
        ]
    );
}

/// CLEAR of a data structure clears every subfield by its type, the
/// subfields of LIKEDS subfields (and of each element of a LIKEDS DIM
/// subfield) too (ILE RPG Reference, CLEAR): numeric subfields become
/// zero, character subfields blanks.
#[tokio::test]
async fn clear_nested_data_structures() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds obj_t qualified template;
  nm char(10);
  n int(10);
end-ds;
dcl-ds list_t qualified template;
  size int(5);
  item likeds(obj_t) dim(3);
end-ds;
dcl-ds outer qualified;
  stamp char(4);
  list likeds(list_t);
end-ds;
outer = *all'x';
clear outer;
dsply (%char(outer.list.size) + ' ' + %char(outer.list.item(3).n) + ' ['
       + outer.list.item(2).nm + '] [' + outer.stamp + ']');
*inlr = *on;
"#;
    let r = run_rpg(&url, &lib, "QSECOFR", "CLRNEST", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["0 0 [          ] [    ]"]);
}

/// A free-form literal continued on the next line (ILE RPG Reference,
/// "Continuation rules"): '+' continues it with the next line's first
/// nonblank character, '-' with the next line's first character; a
/// comment may follow the literal's end.
#[tokio::test]
async fn free_form_literal_continuation() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = "**free
dcl-s s varchar(60);
s = 'https://example.com/a+
     b//c'; // a comment
dsply s;
s = 'one-
 two';
dsply s;
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "LITCONT", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["https://example.com/ab//c", "one two"]);
}

/// A VARYING parameter of a command (PARM VARY(*YES *INT2)) reaches an
/// RPG command processing program as its 2-byte length and the value (CL
/// Programming, "Data passed to the command processing program"): an
/// RPG VARCHAR parameter sees the value's length; a value not given has
/// length 0.
#[tokio::test]
async fn varying_command_parameter_to_rpg() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QCMDSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QCMDSRC".into(),
            name: "VCMD".into(),
            srctype: "CMD".into(),
            source: "             CMD        PROMPT('Varying')
             PARM       KWD(S) TYPE(*CHAR) LEN(64) VARY(*YES *INT2) MIN(0) CASE(*MIXED)
"
            .into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "VCPP".into(),
            srctype: "RPGLE".into(),
            source: "**free\ndcl-pi *n;\n  s varchar(64) const;\nend-pi;\ndsply ('len=' + %char(%len(s)) + ' [' + s + ']');\n*inlr = *on;\n".into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    for c in [
        format!("CRTBNDRPG PGM({lib}/VCPP) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTCMD CMD({lib}/VCMD) PGM({lib}/VCPP) SRCFILE({lib}/QCMDSRC)"),
    ] {
        job.run(&c).await.unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let shown = |o: Outcome| match o {
        Outcome::Messages(m) => m.join(" "),
        other => panic!("unexpected {other:?}"),
    };
    let m = shown(job.run("VCMD S('a/b')").await.unwrap());
    assert!(m.contains("len=3 [a/b]"), "{m}");
    let m = shown(job.run("VCMD").await.unwrap());
    assert!(m.contains("len=0 []"), "{m}");
}

/// RESET sets a field or data structure back to its initial value (ILE
/// RPG Reference, RESET: the value at the end of the initialization
/// step, i.e. INZ), unlike CLEAR, which sets blanks and zeros; a local
/// variable gets its procedure's INZ value. A named constant may be used
/// before its definition (DIM(SIZE) above DCL-C SIZE; iRPGUnit's
/// JOBLOGT2 does so).
#[tokio::test]
async fn reset_restores_initial_values() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-s width int(10) inz(40);
dcl-s arr int(5) dim(SIZE);
dcl-c SIZE 3;
dcl-ds rec qualified;
  name char(5) inz('Bob');
  qty packed(5:0) inz(7);
end-ds;
width = 80;
rec.name = 'Ann';
rec.qty = 9;
reset width;
reset rec;
dsply (%char(width) + ' ' + rec.name + %char(rec.qty) + ' ' + %char(%elem(arr)));
clear width;
dsply %char(width);
local();
*inlr = *on;
dcl-proc local;
  dcl-s n int(10) inz(3);
  n = 5;
  reset n;
  dsply %char(n);
end-proc;
"#;
    let r = run_rpg(&url, &lib, "QSECOFR", "RESETT", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["40 Bob  7 3", "0", "3"]);
}

/// ILE C system() returns 1 when the command fails and puts the
/// exception's message ID into the runtime's exported _EXCP_MSGID (ILE
/// C/C++ Runtime Library Functions, system()); 0 when it runs.
#[tokio::test]
async fn system_function_and_excp_msgid() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-pr system int(10) extproc('system');
  cmd pointer value options(*string);
end-pr;
dcl-s EXCP_MSGID char(7) import('_EXCP_MSGID');
dcl-s rc int(10);
rc = system('CHKOBJ OBJ(QGPL/NOSUCHOBJ) OBJTYPE(*PGM)');
dsply (%char(rc) + ' ' + EXCP_MSGID);
rc = system('CHKOBJ OBJ(QSYS/QGPL) OBJTYPE(*LIB)');
dsply %char(rc);
*inlr = *on;
"#;
    use as4tainr_core::commands::Environment;
    use as4tainr_core::rpg;
    let r = rpg::call(
        &url,
        vec![lib.clone(), "QGPL".into()],
        "QSECOFR".into(),
        None,
        None,
        Some(rpg::ClContext {
            env: Environment {
                database_url: url.clone(),
            },
            curlib: None,
            user_libl: vec![lib.clone()],
            overrides: vec![],
        }),
        "SYSFN".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1 CPF9801", "0"]);
}

/// QLICHGLL (Change Library List API) sets the job's current library and
/// user library list, as CHGLIBL does; QUSRJOBI JOBI0700 (Retrieve Job
/// Information) then returns the system, current and user portions, each
/// library CHAR(11), after the counts at offsets 64-79.
#[tokio::test]
async fn change_and_retrieve_library_list() {
    use as4tainr_core::commands::Environment;
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = format!(
        r#"**free
dcl-pr qlichgll extpgm('QLICHGLL');
  cur char(11) const;
  p1 char(11) const;
  p2 char(11) const;
  libl char(2750) const options(*varsize);
  n int(10) const;
  err char(16);
end-pr;
dcl-pr qusrjobi extpgm('QUSRJOBI');
  rcv char(1000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  job char(26) const;
  ijob char(16) const;
  err char(16);
end-pr;
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(100) const;
  len packed(15:5) const;
end-pr;
dcl-ds j len(1000) qualified;
  nsys int(10) pos(65);
  nprd int(10) pos(69);
  ncur int(10) pos(73);
  nusr int(10) pos(77);
end-ds;
dcl-s err char(16) inz(x'00000010');
dcl-s libs char(11) inz('QGPL');
dcl-s first char(11);
qlichgll('{lib}': '*SAME': '*SAME': libs: 1: err);
qusrjobi(j: %size(j): 'JOBI0700': '*': '': err);
first = %subst(j: 81 + (j.nsys + j.nprd) * 11: 11);
dsply (%char(j.ncur) + ' ' + %char(j.nusr) + ' ' + %trim(first));
qcmdexc('RMVLIBLE LIB(QGPL)': 18);
qusrjobi(j: %size(j): 'JOBI0700': '*': '': err);
dsply (%char(j.ncur) + ' ' + %char(j.nusr));
*inlr = *on;
"#
    );
    let r = rpg::call(
        &url,
        vec![lib.clone(), "QGPL".into()],
        "QSECOFR".into(),
        None,
        None,
        Some(rpg::ClContext {
            env: Environment {
                database_url: url.clone(),
            },
            curlib: None,
            user_libl: vec!["QGPL".into()],
            overrides: vec![],
        }),
        "CHGLL".into(),
        src,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages[0], format!("1 1 {lib}"));
    // A CL command run by the program (QCMDEXC) changes the job's library
    // list too (CL reference, RMVLIBLE): QGPL is no longer in it.
    assert_eq!(r.messages.last().unwrap(), "1 0", "{:?}", r.messages);
}

/// CRTMSGQ, SNDMSG TOMSGQ and QGYOLMSG (Open List of Messages API, IBM
/// i documentation): the queue's messages newest first with *PRV, each
/// with the fields asked for (0302: message text); DLTMSGQ removes the
/// queue, so a second DLTMSGQ fails.
#[tokio::test]
async fn message_queue_and_open_list_of_messages() {
    use as4tainr_core::commands::Environment;
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = format!(
        r#"**free
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(200) const;
  len packed(15:5) const;
end-pr;
dcl-pr qgyolmsg extpgm('QGYOLMSG');
  rcv char(4000) options(*varsize);
  len int(10) const;
  info char(80);
  n int(10) const;
  sort char(1) const;
  sel char(100) const options(*varsize);
  sellen int(10) const;
  uq char(21) const;
  used char(44);
  err char(16);
end-pr;
dcl-pr qgygtle extpgm('QGYGTLE');
  rcv char(4000) options(*varsize);
  len int(10) const;
  handle char(4) const;
  info char(80);
  n int(10) const;
  start int(10) const;
  err char(16);
end-pr;
dcl-ds sel qualified;
  dir char(10) inz('*PRV');
  *n char(2) inz;
  sev int(10) inz(0);
  maxlen int(10) inz(132);
  maxhlp int(10) inz(0);
  selOff int(10) inz(44);
  selCnt int(10) inz(1);
  keyOff int(10) inz(54);
  fldOff int(10) inz(58);
  fldCnt int(10) inz(1);
  crit char(10) inz('*ALL');
  key char(4) inz(x'FFFFFFFF');
  fld int(10) inz(302);
end-ds;
dcl-ds info len(80) qualified;
  total int(10) pos(1);
  ret int(10) pos(5);
end-ds;
dcl-ds rcv len(4000) qualified;
  nxt int(10) pos(1);
  fldOff int(10) pos(5);
  type char(2) pos(24);
  q char(10) pos(50);
end-ds;
dcl-s used char(44);
dcl-s err char(16) inz(x'00000010');
dcl-s c char(200);
qcmdexc('CRTMSGQ MSGQ({lib}/TQ) TEXT(''test'')': 200);
c = 'SNDMSG MSG(''first one'') TOMSGQ({lib}/TQ)';
qcmdexc(c: 200);
c = 'SNDMSG MSG(''second one'') TOMSGQ({lib}/TQ)';
qcmdexc(c: 200);
qgyolmsg(rcv: %size(rcv): info: -1: '0': sel: %size(sel):
  '1TQ        {lib}': used: err);
dsply (%char(info.total) + ' ' + %char(info.ret) + ' ' + rcv.type + ' '
  + %trim(rcv.q) + ' ' + %subst(rcv: rcv.fldOff + 33: 10));
qgygtle(rcv: %size(rcv): %subst(info: 9: 4): info: 1: 2: err);
dsply (%char(info.ret) + ' ' + %subst(rcv: rcv.fldOff + 33: 9));
qcmdexc('DLTMSGQ MSGQ({lib}/TQ)': 200);
monitor;
  qcmdexc('DLTMSGQ MSGQ({lib}/TQ)': 200);
  dsply 'deleted twice';
on-error;
  dsply 'gone';
endmon;
*inlr = *on;
"#
    );
    let r = rpg::call(
        &url,
        vec![lib.clone(), "QGPL".into()],
        "QSECOFR".into(),
        None,
        None,
        Some(rpg::ClContext {
            env: Environment {
                database_url: url.clone(),
            },
            curlib: None,
            user_libl: vec![],
            overrides: vec![],
        }),
        "MSGQL".into(),
        src,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?} {:?}", r.error, r.messages);
    let shown: Vec<&String> = r.messages.iter().filter(|m| !m.starts_with("CP")).collect();
    // QGYGTLE: the list's second entry (the older message).
    assert_eq!(
        shown,
        ["2 2 04 TQ second one", "1 first one", "gone"],
        "{:?}",
        r.messages
    );
}

/// A procedure pointer callback (ILE Concepts, procedure pointer calls)
/// that calls back into the module that called it, with a pointer to that
/// module's local storage: the storage is still there (the caller's
/// frame is active), as iRPGUnit's JobLogReader relies on.
#[tokio::test]
async fn callback_reaches_caller_module_locals() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let put = |name: &str, text: &str| source::Member {
        library: lib.clone(),
        file: "QRPGLESRC".into(),
        name: name.into(),
        srctype: "RPGLE".into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let rdr = "**FREE
ctl-opt nomain;
dcl-ds h_t qualified template;
  p pointer;
end-ds;
dcl-s pCallback pointer(*proc);
dcl-pr callback extproc(pCallback);
  hp pointer const;
end-pr;
dcl-proc rd export;
  dcl-pi *n;
    cb pointer(*proc) const;
  end-pi;
  dcl-s buf char(20);
  dcl-ds h likeds(h_t);
  pCallback = cb;
  buf = 'hello';
  h.p = %addr(buf);
  callback(%addr(h));
end-proc;
dcl-proc info export;
  dcl-pi *n char(5);
    hp pointer const;
  end-pi;
  dcl-ds h likeds(h_t) based(hp);
  dcl-s s char(5) based(h.p);
  return s;
end-proc;
";
    let main = "**FREE
dcl-pr go end-pr;
go();
*inlr = *on;
";
    let cbmod = "**FREE
ctl-opt nomain;
dcl-pr rd;
  cb pointer(*proc) const;
end-pr;
dcl-pr info char(5);
  hp pointer const;
end-pr;
dcl-proc go export;
  rd(%paddr(cb));
end-proc;
dcl-proc cb;
  dcl-pi *n;
    hp pointer const;
  end-pi;
  dsply info(hp);
end-proc;
";
    for m in [put("RDR", rdr), put("CBMAIN", main), put("CBMOD", cbmod)] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/RDR)"),
        format!("CRTRPGMOD MODULE({lib}/CBMAIN)"),
        format!("CRTRPGMOD MODULE({lib}/CBMOD)"),
        format!("CRTPGM PGM({lib}/CBMAIN) MODULE({lib}/CBMAIN {lib}/CBMOD {lib}/RDR)"),
        format!("CRTSRVPGM SRVPGM({lib}/CBSRV) MODULE({lib}/CBMOD {lib}/RDR) EXPORT(*ALL)"),
        format!("CRTPGM PGM({lib}/CBMAIN2) MODULE({lib}/CBMAIN) BNDSRVPGM({lib}/CBSRV)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    // The modules bound into the program, and in a service program (in
    // its activation).
    for p in ["CBMAIN", "CBMAIN2"] {
        assert_eq!(
            cl_cmd(&mut job, format!("CALL PGM({lib}/{p})"))
                .await
                .unwrap(),
            ["DSPLY  hello"],
            "{p}"
        );
    }
}

/// QUSRJOBI JOBI0400 (Retrieve Job Information API, IBM i
/// documentation): date format at offset 219, date and time separators
/// at 218 and 299, decimal format at 456 (blank: a period), 574 bytes up
/// to the job description library's ASP group.
#[tokio::test]
async fn job_information_jobi0400() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-pr qusrjobi extpgm('QUSRJOBI');
  rcv char(1000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  job char(26) const;
  ijob char(16) const;
  err char(16);
end-pr;
dcl-ds j len(1000) qualified;
  avail int(10) pos(5);
  datsep char(1) pos(219);
  datfmt char(4) pos(220);
  timsep char(1) pos(300);
  decfmt char(1) pos(457);
  mode char(8) pos(148);
  devrcy char(13) pos(196);
  ddm char(10) pos(209);
  prtkey char(10) pos(313);
  qfull char(10) pos(358);
  qmax int(10) pos(369);
  splfacn char(10) pos(523);
  logout char(10) pos(555);
  aspgrp char(10) pos(565);
  sched char(8) pos(305);
  rtg char(80) pos(377);
end-ds;
dcl-s err char(16) inz(x'00000010');
dcl-s one int(10) inz(1);
qusrjobi(j: %size(j): 'JOBI0400': '*': '': err);
dsply (%char(j.avail) + ' ' + j.datfmt + j.datsep + j.timsep + '[' + j.decfmt + ']');
dsply (%trim(j.mode) + ' ' + %trim(j.devrcy) + ' ' + %trim(j.ddm) + ' ' + %trim(j.prtkey)
  + ' ' + %trim(j.qfull) + ' ' + %char(j.qmax) + ' ' + %trim(j.splfacn) + ' '
  + %trim(j.logout) + ' ' + %trim(j.aspgrp));
dsply (%char(%scan(x'40': j.sched)) + ' ' + %char(%scan(x'40': j.rtg)));
// %EDITC of an expression edits its own length: %DEC(i: 4: 0) is four
// digits (t13_rows: GLD13J's record IDs J0001, J0026...).
dsply %editc(%dec(one: 4: 0): 'X');
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "JOBI4".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // PUB400 2026-10-28, tools/golden/verify13.txt t13_rows J (a
    // `system` command's job on IBM i 7.5): mode *BLANK, device recovery
    // *MSG, DDM *KEEP, print key *PRTHDR, queue full *WRAP, maximum 4,
    // spooled file action *DETACH, job log output *PND, ASP group
    // *SYSBAS; no scheduled date and no routing data (X'00', no blank).
    assert_eq!(
        r.messages,
        [
            "574 *MDY/:[ ]",
            "*BLANK *MSG *KEEP *PRTHDR *WRAP 4 *DETACH *PND *SYSBAS",
            "0 0",
            "0001"
        ]
    );
}

/// INZ on a data structure initializes every subfield to its type's
/// default, the subfields of a LIKEDS subfield too (ILE RPG Reference,
/// INZ keyword): a nested integer is 0, not blanks (iRPGUnit's
/// CMDRUNLOG g_liblData).
#[tokio::test]
async fn inz_reaches_nested_likeds_subfields() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds libl_t template;
  nume int(5) pos(1);
  lib char(10) dim(3) pos(3);
end-ds;
dcl-ds data_t qualified template;
  curlib char(10) pos(1);
  libl likeds(libl_t);
  amt packed(5:2) dim(2);
end-ds;
dcl-ds g likeds(data_t) inz;
dcl-ds h likeds(data_t);
dsply (%char(g.libl.nume) + ' [' + g.libl.lib(2) + '] ' + %char(g.amt(2)) + ' ' + %char(%size(g)));
dsply %char(%len(%trim(h.curlib)));
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "INZNEST".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["0 [          ] .00 48", "0"]);
}

/// An RPG procedure calls a CL module's procedure bound into the same
/// program (ILE CL: a module's one procedure, named as the module; its
/// parameters by reference): the values the CL procedure leaves come
/// back (iRPGUnit's LIBLT calls its LIBLC module so). RTVJOBA USRLIBL:
/// the user part of the library list, 11 characters a library.
#[tokio::test]
async fn rpg_calls_bound_cl_procedure() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGLESRC", "QCLSRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, text: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let cl = "PGM PARM(&TEXT &N)
DCL VAR(&TEXT) TYPE(*CHAR) LEN(20)
DCL VAR(&N) TYPE(*DEC) LEN(5 0)
DCL VAR(&L) TYPE(*CHAR) LEN(2750)
RTVJOBA USRLIBL(&L)
CHGVAR VAR(&TEXT) VALUE('set by CL')
IF COND(%SST(&L 1 11) *NE ' ') THEN(CHGVAR VAR(&TEXT) VALUE(%SST(&L 1 11) *TCAT ' by CL'))
CHGVAR VAR(&N) VALUE(&N + 1)
ENDPGM
";
    let rpg = "**FREE
dcl-pr clproc extproc('CLPROC');
  text char(20);
  n packed(5:0);
end-pr;
dcl-s t char(20) inz('before');
dcl-s n packed(5:0) inz(41);
clproc(t: n);
dsply (%trim(t) + ' ' + %char(n));
*inlr = *on;
";
    for m in [
        put("QCLSRC", "CLPROC", "CLLE", cl),
        put("QRPGLESRC", "RPGMAIN", "RPGLE", rpg),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTCLMOD MODULE({lib}/CLPROC) SRCFILE({lib}/QCLSRC)"),
        format!("CRTRPGMOD MODULE({lib}/RPGMAIN)"),
        format!("CRTPGM PGM({lib}/RPGMAIN) MODULE({lib}/RPGMAIN {lib}/CLPROC)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/RPGMAIN)"))
            .await
            .unwrap(),
        [format!("DSPLY  {lib} by CL 42")]
    );
}

/// Free-form WRITE of a program-described printer file with a data
/// structure (ILE RPG Reference, WRITE (Create New Records): a data
/// structure is required for a program-described file): its bytes are
/// the printed line (iRPGUnit's LSTSPLFT).
#[tokio::test]
async fn write_program_described_printer_from_ds() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-f QSYSPRT printer(80) usropn;
dcl-ds line len(80);
  text char(20);
end-ds;
open QSYSPRT;
text = 'first line';
write QSYSPRT line;
text = 'second line';
write QSYSPRT line;
close QSYSPRT;
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "WRTPRT".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.printed, ["first line", "second line"]);
}

/// %LEN as the target of an assignment sets a varying-length variable's
/// current length (ILE RPG Reference, %LEN: "%LEN can be used on the
/// left-hand side of an assignment"); beyond its maximum is an error.
#[tokio::test]
async fn len_assignment_sets_current_length() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-s v varchar(10) inz('abcdef');
%len(v) = 3;
dsply (v + '|' + %char(%len(v)));
%len(v) += 2;
dsply (v + '|');
monitor;
  %len(v) = 11;
  dsply 'no error';
on-error;
  dsply 'error';
endmon;
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "LENSET".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["abc|3", "abcde|", "error"]);
}

/// QMHSNDPM to the call stack entry two above the sender, then QMHRCVPM
/// of that message (RCVM0300): the receiving procedure is that entry's,
/// with its name as EXTPROC(*DCLCASE) declares it, also when the sender
/// is a procedure of a service program (iRPGUnit's PGMMSG caller()).
#[tokio::test]
async fn message_to_caller_two_entries_up() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    // A job of its own: jobs without one share the process's call stack,
    // which other tests running at the same time add to.
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("M{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let put = |name: &str, text: &str| source::Member {
        library: lib.clone(),
        file: "QRPGLESRC".into(),
        name: name.into(),
        srctype: "RPGLE".into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let callermod = r#"**FREE
ctl-opt nomain;
dcl-pr qmhsndpm extpgm('QMHSNDPM');
  id char(7) const; msgf char(20) const; data char(100) const options(*varsize);
  len int(10) const; type char(10) const; entry char(10) const; counter int(10) const;
  key char(4); err char(16); entlen int(10) const options(*nopass);
  qual char(20) const options(*nopass); wait int(10) const options(*nopass);
end-pr;
dcl-pr qmhrcvpm extpgm('QMHRCVPM');
  rcv char(2048) options(*varsize); len int(10) const; fmt char(8) const;
  entry char(10) const; counter int(10) const; type char(10) const; key char(4) const;
  wait int(10) const; action char(10) const; err char(16);
  entlen int(10) const options(*nopass); qual char(20) const options(*nopass);
end-pr;
dcl-proc caller export;
  dcl-pi *n varchar(50) end-pi;
  dcl-s key char(4);
  dcl-s err char(16) inz(x'00000010');
  dcl-s buf char(2048);
  dcl-s off int(10);
  dcl-ds h qualified;
    bytRet int(10) pos(1); bytAvl int(10) pos(5);
    rpl int(10) pos(81); msgl int(10) pos(89); hlp int(10) pos(97);
  end-ds;
  qmhsndpm('CPF9897': 'QCPFMSG   *LIBL': 'who': 3: '*INFO': '*': 2: key: err: 1: '*NONE     *NONE': 0);
  qmhrcvpm(buf: 2048: 'RCVM0300': '*': 2: '*INFO': key: 0: '*REMOVE': err: 1: '*NONE     *NONE');
  h = %subst(buf: 1: %size(h));
  off = 113 + h.rpl + h.msgl + h.hlp;
  return %trim(%subst(buf: off + 354: 10)) + '/' + %trim(%subst(buf: off + 364: 10)) + '/' + %trim(%subst(buf: off + 374: 30));
end-proc;
"#;
    let mainmod = r#"**FREE
dcl-pr caller varchar(50) end-pr;
testCaller();
*inlr = *on;
dcl-proc testCaller;
  dcl-pi *n extproc(*dclcase) end-pi;
  dsply ('[' + getCaller() + ']');
end-proc;
dcl-proc getCaller;
  dcl-pi *n varchar(50) extproc(*dclcase) end-pi;
  return caller();
end-proc;
"#;
    for m in [put("CALLERMOD", callermod), put("CALLMAIN", mainmod)] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/CALLERMOD)"),
        format!("CRTSRVPGM SRVPGM({lib}/CALLERSRV) MODULE({lib}/CALLERMOD) EXPORT(*ALL)"),
        format!("CRTRPGMOD MODULE({lib}/CALLMAIN)"),
        format!("CRTPGM PGM({lib}/CALLMAIN) MODULE({lib}/CALLMAIN {lib}/CALLERMOD)"),
        format!("CRTPGM PGM({lib}/CALLMAIN2) MODULE({lib}/CALLMAIN) BNDSRVPGM({lib}/CALLERSRV)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/CALLMAIN)"))
            .await
            .unwrap(),
        ["DSPLY  [CALLMAIN/CALLMAIN/testCaller]"]
    );
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/CALLMAIN2)"))
            .await
            .unwrap(),
        ["DSPLY  [CALLMAIN2/CALLMAIN/testCaller]"]
    );
}

/// QWDRJOBD JOBD0100 (Retrieve Job Description Information API, IBM i
/// documentation): the user at offset 28, the text at 309, the offset to
/// and number of the initial library list's entries at 360 and 364, the
/// entries CHAR(11) each.
#[tokio::test]
async fn retrieve_job_description() {
    use as4tainr_core::commands::Environment;
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = format!(
        r#"**free
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(200) const;
  len packed(15:5) const;
end-pr;
dcl-pr qwdrjobd extpgm('QWDRJOBD');
  rcv char(1000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  jobd char(20) const;
  err char(16);
end-pr;
dcl-ds d len(1000) qualified;
  avail int(10) pos(5);
  text char(50) pos(310);
  ofs int(10) pos(361);
  n int(10) pos(365);
  logsev int(10) pos(157);
end-ds;
dcl-s err char(16) inz(x'00000010');
qcmdexc('CRTJOBD JOBD({lib}/TJOBD) INLLIBL(QGPL {lib}) TEXT(''Test jobd'')': 200);
qwdrjobd(d: %size(d): 'JOBD0100': 'TJOBD     {lib}': err);
dsply (%trim(d.text) + ' ' + %char(d.n) + ' ' + %trim(%subst(d: d.ofs + 1: 11))
  + ' ' + %trim(%subst(d: d.ofs + 12: 11)));
qwdrjobd(d: %size(d): 'JOBD0100': 'NOSUCH    {lib}': err);
dsply %subst(err: 9: 7);
qwdrjobd(d: %size(d): 'JOBD0100': 'QDFTJOBD  QGPL': err);
dsply (%char(d.avail) + ' [' + %subst(d: 75: 2) + '] ' + %char(d.logsev) + ' '
  + %trim(%subst(d: 230: 80)) + ' ' + %char(d.n) + ' ' + %trim(%subst(d: d.ofs + 1: 11))
  + ' ' + %subst(d: 453: 10) + %subst(d: 463: 10) + ' ' + %char(d.ofs));
*inlr = *on;
"#
    );
    let r = rpg::call(
        &url,
        vec![lib.clone(), "QGPL".into()],
        "QSECOFR".into(),
        None,
        None,
        Some(rpg::ClContext {
            env: Environment {
                database_url: url.clone(),
            },
            curlib: None,
            user_libl: vec![],
            overrides: vec![],
        }),
        "RTVJOBD".into(),
        src,
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // CRTJOBD's completion message first.
    let shown = &r.messages[r.messages.len().saturating_sub(3)..];
    // QGPL/QDFTJOBD as IBM i 7.5 returned it (PUB400 2026-10-28,
    // tools/golden/verify13.txt t13_rows D): 489 bytes, job queue
    // priority " 5", logging severity 20, routing data QCMDI, one
    // initial library list entry *SYSVAL at offset 472, then *SYSVAL and
    // *SBSD before it.
    assert_eq!(
        shown,
        [
            format!("Test jobd 2 QGPL {lib}"),
            "CPF9801".to_string(),
            "489 [ 5] 20 QCMDI 1 *SYSVAL *SYSVAL   *SBSD      472".to_string()
        ],
        "{:?}",
        r.messages
    );
}

/// XML-SAX %HANDLER(procedure : communication area) %XML(document)
/// (ILE RPG Reference, XML-SAX and "XML events"): the handler gets each
/// event with the communication area, a pointer to the event's string
/// and its length; it compares the event with the *XML_xxx special
/// words. A handler that returns a value other than 0 ends the parsing.
#[tokio::test]
async fn xml_sax_handler_gets_events() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds info qualified inz;
  path varchar(100);
  attrs varchar(100);
  count int(10);
end-ds;
dcl-s doc varchar(200)
  inz('<order id="7"><item>pen</item><item>ink &amp; nib</item><stop/><item>x</item></order>');
xml-sax %handler(hdl : info) %xml(doc);
dsply info.path;
dsply (info.attrs + ' ' + %char(info.count));
*inlr = *on;
dcl-proc hdl;
  dcl-pi *n int(10);
    c likeds(info);
    event int(10) value;
    pstr pointer value;
    len int(20) value;
    exc int(10) value;
  end-pi;
  dcl-s str char(100) based(pstr);
  dcl-s v varchar(100);
  if len > 0;
    v = %subst(str: 1: len);
  endif;
  c.count += 1;
  select;
  when event = *XML_START_ELEMENT;
    if v = 'stop';
      return 1;
    endif;
    c.path += '<' + v;
  when event = *XML_CHARS or event = *XML_PREDEF_REF;
    c.path += v;
  when event = *XML_ATTR_NAME;
    c.attrs += v + '=';
  when event = *XML_ATTR_CHARS;
    c.attrs += v;
  endsl;
  return 0;
end-proc;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "SAXT".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // The events up to <stop/>: START_DOCUMENT, order, id's three, item,
    // pen, END item, item, ink , &, nib, END item, stop.
    assert_eq!(r.messages, ["<order<itempen<itemink & nib", "id=7 14"]);
}

/// stat() with its buffer declared CONST in the prototype (iRPGUnit's
/// QSYSINC/STAT) still fills the caller's buffer: C passes it by address
/// (IBM i documentation, stat()--Get File Information: struct stat *buf).
#[tokio::test]
async fn stat_fills_const_declared_buffer() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds st_t qualified template;
  mode uns(10);
  rest char(124);
end-ds;
dcl-pr stat int(10) extproc('stat');
  path pointer value options(*string);
  buf const likeds(st_t);
end-pr;
dcl-ds st likeds(st_t);
st.mode = 0;
dsply (%char(stat('/': st)) + ' ' + %char(%bitand(st.mode: x'F000') > 0));
dsply %char(stat('/no/such/dir': st));
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "STATC".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["0 1", "-1"]);
}

/// QMHRCVPM by message key (Receive Program Message API): the key names
/// one message of the job, received also when the call stack entry it
/// was sent to has ended (iRPGUnit's getLatestEscMsg relies on it on IBM
/// i). The receiver gets as many bytes as the length parameter says,
/// also past a shorter variable over larger storage (an RCVM0300 header
/// based on a 2048-byte heap block, as iRPGUnit's caller() does).
#[tokio::test]
async fn receive_message_by_key_after_its_entry_ended() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-pr qmhsndpm extpgm('QMHSNDPM');
  id char(7) const; msgf char(20) const; data char(100) const options(*varsize);
  len int(10) const; type char(10) const; entry char(10) const; counter int(10) const;
  key char(4); err char(16);
end-pr;
dcl-pr qmhrcvpm extpgm('QMHRCVPM');
  rcv char(2048) options(*varsize); len int(10) const; fmt char(8) const;
  entry char(10) const; counter int(10) const; type char(10) const; key char(4) const;
  wait int(10) const; action char(10) const; err char(16);
end-pr;
dcl-ds hdr qualified based(p);
  bytRet int(10);
  bytAvl int(10);
  rplLen int(10) pos(81);
  msgLen int(10) pos(89);
end-ds;
dcl-s p pointer;
dcl-s key char(4);
dcl-s err char(16) inz(x'00000010');
dcl-s txt char(2048) based(p);
key = sendToMyself();
p = %alloc(2048);
qmhrcvpm(hdr: 2048: 'RCVM0300': '*': 0: '*ANY': key: 0: '*REMOVE': err);
dsply (%char(hdr.bytRet > 112) + ' ' + %subst(txt: 113 + hdr.rplLen: hdr.msgLen));
qmhrcvpm(hdr: 2048: 'RCVM0300': '*': 0: '*ANY': key: 0: '*REMOVE': err);
dsply %subst(err: 9: 7);
*inlr = *on;
dcl-proc sendToMyself;
  dcl-pi *n char(4) end-pi;
  dcl-s k char(4);
  qmhsndpm('': '': 'sent by a procedure that ended': 30: '*INFO': '*': 0: k: err);
  return k;
end-proc;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "RCVKEY".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    // Removed with *REMOVE, the key then names no message: CPF2410.
    assert_eq!(r.messages, ["1 sent by a procedure that ended", "CPF2410"]);
}

/// QGYOLJBL (Open List of Job Log Messages API) fields 0605 (sending
/// procedure) and 0705 (receiving procedure): a message a procedure sent
/// to its caller names both.
#[tokio::test]
async fn job_log_list_gives_sender_and_receiver() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-pr qmhsndpm extpgm('QMHSNDPM');
  id char(7) const; msgf char(20) const; data char(100) const options(*varsize);
  len int(10) const; type char(10) const; entry char(10) const; counter int(10) const;
  key char(4); err char(16);
end-pr;
dcl-pr qgyoljbl extpgm('QGYOLJBL');
  rcv char(4000) options(*varsize); len int(10) const; info char(80);
  n int(10) const; sel char(200) const options(*varsize); sellen int(10) const;
  err char(16);
end-pr;
dcl-ds sel qualified;
  dir char(10) pos(1) inz('*PRV');
  job char(26) pos(11) inz('*');
  ijob char(16) pos(37) inz(*blanks);
  key char(4) pos(53) inz(x'FFFFFFFF');
  maxmsg int(10) pos(57) inz(-1);
  maxhlp int(10) pos(61) inz(0);
  fldOff int(10) pos(65) inz(84);
  fldCnt int(10) pos(69) inz(2);
  qOff int(10) pos(73) inz(92);
  qLen int(10) pos(77) inz(1);
  id1 int(10) pos(85) inz(605);
  id2 int(10) pos(89) inz(705);
  q char(1) pos(93) inz('*');
end-ds;
dcl-s buf char(4000);
dcl-ds ent qualified based(pe);
  fldOff int(10) pos(5);
end-ds;
dcl-ds fld qualified based(pf);
  nxt int(10) pos(1);
  dlen int(10) pos(29);
  data char(100) pos(33);
end-ds;
dcl-s pe pointer;
dcl-s pf pointer;
dcl-s err char(16) inz(x'00000010');
dcl-s info char(80);
dcl-s res varchar(200);
dcl-s k char(4);
tellCaller();
qgyoljbl(buf: %size(buf): info: 1: sel: %size(sel): err);
pe = %addr(buf);
pf = %addr(buf) + ent.fldOff;
res = %subst(fld.data: 1: fld.dlen);
pf = %addr(buf) + fld.nxt;
res += ' ' + %subst(fld.data: 1: fld.dlen);
dsply res;
*inlr = *on;
dcl-proc tellCaller;
  qmhsndpm('': '': 'hello caller': 12: '*INFO': '*': 1: k: err);
end-proc;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "JLFLDS".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["TELLCALLER JLFLDS"]);
}

/// CHKOBJ OBJTYPE takes an object type (IBM documentation, CHKOBJ:
/// OBJTYPE is an object type); a value that is none is the command
/// analyzer's CPD0084, then CPF0006, before any object is looked for
/// (iRPGUnit's TESTPGM23 runs `CHKOBJ OBJ(*LIBL/RUTESTCASE) OBJTYPE(*FOO)`
/// and expects the command to be rejected so).
#[tokio::test]
async fn chkobj_rejects_unknown_object_type() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let e = cl_cmd(&mut job, "CHKOBJ OBJ(*LIBL/NOSUCH) OBJTYPE(*FOO)".into())
        .await
        .unwrap_err();
    assert!(e.contains("CPD0084") && e.contains("CPF0006"), "{e}");
    assert!(!e.contains("CPF9801"), "{e}");
    let e = cl_cmd(&mut job, "CHKOBJ OBJ(*LIBL/NOSUCH) OBJTYPE(*PGM)".into())
        .await
        .unwrap_err();
    assert!(e.contains("CPF9801"), "{e}");
}

/// A module bound into a program calls another module, which calls back
/// into the first (ILE Concepts: a module's static storage is one per
/// activation): the call back sees the global values the first module
/// has now, and what it changes the first module sees after (iRPGUnit's
/// XML writer asks CMDRUNSRV, which called it, for its settings).
#[tokio::test]
async fn call_back_into_running_bound_module_shares_globals() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    source::create_file(&client, &lib, "QRPGLESRC", "")
        .await
        .unwrap();
    let put = |name: &str, text: &str| source::Member {
        library: lib.clone(),
        file: "QRPGLESRC".into(),
        name: name.into(),
        srctype: "RPGLE".into(),
        text: String::new(),
        source: text.into(),
        ..Default::default()
    };
    let main = "**FREE
dcl-pr run end-pr;
run();
*inlr = *on;
";
    let first = "**FREE
ctl-opt nomain;
dcl-s mode int(10) inz(1);
dcl-pr report end-pr;
dcl-proc run export;
  mode = 5;
  report();
  dsply ('after ' + %char(mode));
end-proc;
dcl-proc getMode export;
  dcl-pi *n int(10) end-pi;
  return mode;
end-proc;
dcl-proc setMode export;
  dcl-pi *n;
    m int(10) const;
  end-pi;
  mode = m;
end-proc;
";
    let second = "**FREE
ctl-opt nomain;
dcl-pr getMode int(10) end-pr;
dcl-pr setMode;
  m int(10) const;
end-pr;
dcl-proc report export;
  dsply ('mode ' + %char(getMode()));
  setMode(9);
end-proc;
";
    for m in [
        put("CBMAIN3", main),
        put("FIRSTMOD", first),
        put("SECONDMOD", second),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTRPGMOD MODULE({lib}/CBMAIN3)"),
        format!("CRTRPGMOD MODULE({lib}/FIRSTMOD)"),
        format!("CRTRPGMOD MODULE({lib}/SECONDMOD)"),
        format!("CRTPGM PGM({lib}/CBMAIN3) MODULE({lib}/CBMAIN3 {lib}/FIRSTMOD {lib}/SECONDMOD)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    assert_eq!(
        cl_cmd(&mut job, format!("CALL PGM({lib}/CBMAIN3)"))
            .await
            .unwrap(),
        ["DSPLY  mode 5", "DSPLY  after 9"]
    );
}

/// INZ(*LIKEDS) on a LIKEDS subfield: the subfield starts with the
/// template's initial values (ILE RPG Reference, INZ keyword, "*LIKEDS"),
/// also when the data structure is itself a template copied by another
/// INZ(*LIKEDS), in a global data structure, an array of them and a
/// procedure's local one (iRPGUnit's SAX_H templates rely on it).
#[tokio::test]
async fn inz_likeds_reaches_nested_likeds_subfields() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
ctl-opt dftactgrp(*no);
dcl-ds val_t qualified template;
  isNull ind inz(*on);
  text varchar(20) inz;
end-ds;
dcl-ds evt_t qualified template;
  isNull ind inz(*on);
  name varchar(10) inz;
  expected likeds(val_t) inz(*likeds);
end-ds;
dcl-ds list_t qualified template;
  size int(10) inz;
  ev likeds(evt_t) dim(3) inz(*likeds);
end-ds;
dcl-ds plain qualified;
  num packed(3:0) inz(7);
  expected likeds(val_t) inz(*likeds);
end-ds;
dcl-ds e likeds(evt_t) inz(*likeds);
dcl-ds l likeds(list_t) inz(*likeds);
dsply (plain.expected.isNull + %char(plain.num) + e.expected.isNull + e.isNull);
dsply (l.ev(1).expected.isNull + l.ev(3).expected.isNull + l.ev(3).isNull);
local();
*inlr = *on;
dcl-proc local;
  dcl-ds le likeds(evt_t) inz(*likeds);
  dsply (le.expected.isNull + le.isNull);
end-proc;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "INZLDS".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1711", "111", "11"]);
}

/// An indicator is character data of length 1 (ILE RPG Reference,
/// "Indicator Data Type"), so + of two indicators concatenates them.
#[tokio::test]
async fn plus_of_two_indicators_concatenates() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-s a ind inz(*on);
dcl-s b ind inz(*off);
*in05 = *on;
dsply (a + b + *in05);
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "INDCAT".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["101"]);
}

/// A run-time error is signaled as the RPG escape message RNXnnnn (ILE
/// RPG Programmer's Guide, exception handling; iRPGUnit's STRINGT expects
/// RNX0100 from %SUBST out of range as the job's latest escape message):
/// ON-EXCP takes it by that ID and the PSDS shows it at positions 40-46.
#[tokio::test]
async fn runtime_error_is_rnx_escape_message() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds psds psds;
  excpid char(7) pos(40);
end-ds;
dcl-s s varchar(5) inz('abc');
dcl-s r varchar(5);
monitor;
  r = %subst(s: 4: 2);
on-excp 'RNX0100';
  dsply ('excp ' + excpid + ' ' + %char(%status));
on-error;
  dsply ('other ' + excpid);
endmon;
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "RNXID".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["excp RNX0100 100"]);
}

/// OPTIONS(*TRIM) (ILE RPG Reference, OPTIONS keyword): a CONST or VALUE
/// character argument is passed without its leading and trailing blanks,
/// also a variable of the parameter's own type (iRPGUnit's parseNum
/// relies on it for values with trailing blanks).
#[tokio::test]
async fn options_trim_passes_argument_trimmed() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
ctl-opt dftactgrp(*no);
dcl-s v varchar(20);
v = '  ab  ';
dsply (%char(lenOf(v)) + ' ' + %char(lenOf('  xyz ')) + ' ' + %char(valOf(' q ')));
*inlr = *on;
dcl-proc lenOf;
  dcl-pi *n int(10);
    s varchar(20) const options(*trim);
  end-pi;
  return %len(s);
end-proc;
dcl-proc valOf;
  dcl-pi *n int(10);
    s varchar(20) value options(*trim);
  end-pi;
  return %len(s);
end-proc;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "OPTTRIM".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["2 3 1"]);
}

/// localtime_r(time, tm) fills the struct tm it is given (C library
/// reference), also when the RPG prototype declares it CONST (iRPGUnit's
/// QSYSINC/LOCALTIME, used by its STMF module to date a stream file).
#[tokio::test]
async fn localtime_r_fills_const_declared_tm() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
ctl-opt dftactgrp(*no);
dcl-ds tm_t qualified template;
  sec int(10);
  min int(10);
  hour int(10);
  mday int(10);
  mon int(10);
  year int(10);
  wday int(10);
  yday int(10);
  isdst int(10);
end-ds;
dcl-pr localtime_r pointer extproc('localtime_r');
  time int(10) const;
  tm likeds(tm_t) const;
end-pr;
dcl-pr strftime int(10) extproc('strftime');
  dest pointer value;
  maxsize int(10) value;
  format pointer value options(*string);
  timeptr const likeds(tm_t);
end-pr;
dcl-ds t likeds(tm_t) inz;
dcl-s secs int(10) inz(86400);
dcl-s out char(11) inz(*allx'00');
if localtime_r(secs: t) <> *null;
  dsply (%char(t.year) + ' ' + %char(t.mon) + ' ' + %char(t.mday));
  if strftime(%addr(out): %size(out): '%Y-%m-%d': t) = 10;
    dsply %subst(out: 1: 10);
  endif;
endif;
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "LOCALTR".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["70 0 2", "1970-01-02"]);
}

/// mitime builds an MI time interval in the variable it is given (also a
/// CONST-declared one, as iRPGUnit's MILIB prototype has it) and
/// waittime waits that long (iRPGUnit's waitSeconds).
#[tokio::test]
async fn mitime_and_waittime_wait() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
ctl-opt dftactgrp(*no);
dcl-s mi_time_t char(8) inz template;
dcl-pr waitTime extproc('waittime');
  pmi_time const like(mi_time_t);
  option int(5) value;
end-pr;
dcl-pr mitime pointer extproc('mitime');
  mi_time const like(mi_time_t);
  hour int(10) value;
  min int(10) value;
  sec int(10) value;
  hsec int(10) value;
end-pr;
dcl-s t like(mi_time_t) inz(*allx'00');
dcl-s s timestamp;
dcl-s us int(20);
s = %timestamp();
mitime(t: 0: 0: 0: 50);
waitTime(t: 4096);
us = %diff(%timestamp(): s: *mseconds);
dsply %char(us >= 500000 and us < 2000000);
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "MIWAIT".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["1"]);
}

/// A program compiled once is kept for the process (on IBM i it is
/// compiled when created), but a file it describes being created again
/// with other fields is seen at the next activation.
#[tokio::test]
async fn compiled_program_follows_changed_file() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.run(&format!(
        "RUNSQL SQL('create table {lib}.cachef (a char(5))') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let src = format!(
        "**free\ndcl-ds r extname('{lib}/CACHEF') end-ds;\ndsply %char(%size(r));\n*inlr = *on;\n"
    );
    let run = || async {
        rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            "CACHEP".into(),
            src.clone(),
            vec![],
        )
        .await
        .unwrap()
    };
    let r = run().await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["5"]);
    let r = run().await;
    assert_eq!(r.messages, ["5"]);
    job.run(&format!(
        "RUNSQL SQL('drop table {lib}.cachef') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    job.run(&format!(
        "RUNSQL SQL('create table {lib}.cachef (a char(5), b char(3))') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let r = run().await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["8"]);
}

/// Where /COPY and /INCLUDE files are searched (ILE RPG Programmer's
/// Guide, "Search Path Within The IFS"; ILE RPG Reference, "/COPY or
/// /INCLUDE"): for a stream file source the IFS first (current
/// directory, INCDIR, RPGINCDIR, the source's directory; a name without a
/// suffix also as .rpgleinc and .rpgle), then the library list; for a
/// member the library list first (QRPGLESRC), then the IFS.
#[tokio::test]
async fn include_search_order_follows_source_kind() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/inc{}", lib.to_lowercase());
    let main = "**free\n/include proto\n/include proto2\ndsply (who + ' ' + who2);\n*inlr = *on;\n";
    source::put_stream_file(&client, &format!("{dir}/src/main.rpgle"), main)
        .await
        .unwrap();
    source::put_stream_file(
        &client,
        &format!("{dir}/src/proto.rpgleinc"),
        "**free\ndcl-c who 'ifs';\n",
    )
    .await
    .unwrap();
    source::put_stream_file(
        &client,
        &format!("{dir}/other/proto2.rpgle"),
        "**free\ndcl-c who2 'incdir';\n",
    )
    .await
    .unwrap();
    job.run(&format!("CRTSRCPF FILE({lib}/QRPGLESRC) RCDLEN(112)"))
        .await
        .unwrap();
    for (name, text) in [("PROTO", "**free\ndcl-c who 'member';\n"), ("MAINM", main)] {
        source::put_member(
            &client,
            &source::Member {
                library: lib.clone(),
                file: "QRPGLESRC".into(),
                name: name.into(),
                srctype: "RPGLE".into(),
                source: text.into(),
                ccsid: 37,
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    }
    let out = |o: Outcome| format!("{o:?}");
    job.run(&format!(
        "CRTBNDRPG PGM({lib}/INCIFS) SRCSTMF('{dir}/src/main.rpgle') INCDIR('{dir}/other')"
    ))
    .await
    .map(out)
    .unwrap();
    job.run(&format!(
        "CRTBNDRPG PGM({lib}/INCMBR) SRCFILE({lib}/QRPGLESRC) SRCMBR(MAINM) INCDIR('{dir}/other')"
    ))
    .await
    .map(out)
    .unwrap();
    for (pgm, want) in [("INCIFS", "ifs incdir"), ("INCMBR", "member incdir")] {
        let r = job.run(&format!("CALL PGM({lib}/{pgm})")).await.unwrap();
        assert!(format!("{r:?}").contains(want), "{pgm}: {r:?}");
    }
}

/// %LOOKUP of a subfield of every element of an array of data structures,
/// DSARR(*).SUBFIELD (ILE RPG Reference, %LOOKUPxx).
#[tokio::test]
async fn lookup_searches_data_structure_array_subfield() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
dcl-ds item qualified dim(4);
  name char(10);
  qty int(10);
end-ds;
item(1).name = 'APPLE';
item(2).name = 'PEAR';
item(3).name = 'PLUM';
dsply (%char(%lookup('PEAR': item(*).name)) + ' ' + %char(%lookup('KIWI': item(*).name)));
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "LOOKDS".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["2 0"]);
}

/// SQL comments in an embedded SQL statement of free-form RPG (Db2 for i
/// SQL Reference, "Comments": `--` to the end of the line, `/* */`): an
/// apostrophe in one does not start a literal.
#[tokio::test]
async fn embedded_sql_comments_are_not_rpg_literals() {
    use as4tainr_core::rpg::parse;
    let src = "**free\ndcl-s n int(10);\nexec sql\n  select count(*) /* it's a comment */\n    into :n -- and this's one too\n    from sysibm.sysdummy1;\ndsply %char(n);\n";
    let s = parse::parse(src, &|_| None).unwrap();
    let text = format!("{s:?}");
    assert!(!text.contains("comment"), "{text}");
    assert!(text.contains("DSPLY") || text.contains("Dsply"), "{text}");
}

/// EVAL-CORR (ILE RPG Reference, EVAL-CORR): subfields of the same name
/// are assigned (character to varying character, packed to zoned); a
/// nested data structure subfield by subfield; arrays as many elements as
/// the smaller has; subfields of types that do not go together (CODE:
/// character and packed) and those only in the target keep their values.
#[tokio::test]
async fn eval_corr_assigns_corresponding_subfields() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = r#"**free
ctl-opt dftactgrp(*no);
dcl-ds addr_t qualified template;
  city char(10);
  zip packed(5:0);
end-ds;
dcl-ds a qualified;
  name char(10) inz('Alice');
  age packed(3:0) inz(30);
  code char(3) inz('ABC');
  where likeds(addr_t);
  tags char(2) dim(3);
end-ds;
dcl-ds b qualified;
  age zoned(5:0) inz(1);
  name varchar(20) inz('x');
  code packed(3:0) inz(7);
  where likeds(addr_t);
  tags char(2) dim(2);
  oth char(1) inz('O');
end-ds;
a.where.city = 'Paris';
a.where.zip = 75001;
a.tags(1) = 'T1'; a.tags(2) = 'T2'; a.tags(3) = 'T3';
eval-corr b = a;
dsply (%trim(b.name) + ' ' + %char(b.age) + ' ' + %char(b.code) + ' ' + b.where.city + %char(b.where.zip) + ' ' + b.tags(1) + b.tags(2) + ' ' + b.oth);
*inlr = *on;
"#;
    let r = rpg::call(
        &url,
        vec![lib.clone()],
        "QSECOFR".into(),
        None,
        None,
        None,
        "EVALCORR".into(),
        src.into(),
        vec![],
    )
    .await
    .unwrap();
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["Alice 30 7 Paris     75001 T1T2 O"]);
}

/// SQL features of IBM i applications (rpgle-repl's tables and code):
/// - auditing columns GENERATED ALWAYS AS (DATA CHANGE OPERATION),
///   (SESSION_USER), (QSYS2.JOB_NAME) and FOR EACH ROW ON UPDATE AS ROW
///   CHANGE TIMESTAMP, set for SQL changes (Db2 for i SQL Programming,
///   "Creating auditing columns");
/// - RENAME TABLE t TO long_name FOR SYSTEM NAME t: the SQL name (SQL
///   Reference, RENAME);
/// - a column named by its system name (FOR COLUMN) as well as its SQL
///   name;
/// - CREATE ALIAS over a table and over a source file member, INSERT
///   through the member alias adding a record (SQL Reference, CREATE
///   ALIAS); DROP ALIAS.
#[tokio::test]
async fn sql_audit_columns_rename_alias_and_system_column_names() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for st in [
        format!("create table {lib}.audt (item_name for column itemnm varchar(20), op char(1) generated always as (data change operation), who varchar(128) generated always as (session_user), changed timestamp generated always for each row on update as row change timestamp)"),
        format!("rename table {lib}.audt to audited_items for system name audt"),
        format!("insert into {lib}.audited_items (item_name) values (''PEN'')"),
        format!("create alias {lib}.audtal for {lib}.audt"),
    ] {
        job.run(&format!("RUNSQL SQL('{st}') COMMIT(*NONE)"))
            .await
            .unwrap_or_else(|e| panic!("{st}: {e:#}"));
    }
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"))
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => {
                    rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>()
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows(format!(
            "select itemnm, op, changed is not null from {lib}.audited_items"
        ))
        .await,
        ["PEN|I|TRUE"]
    );
    as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("update {lib}.audt set item_name = 'INK'"),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(format!("select item_name, op from {lib}.audtal")).await,
        ["INK|U"]
    );
    // A source member through an alias: an INSERT adds a record.
    job.run(&format!("CRTSRCPF FILE({lib}/SRC) RCDLEN(112) MBR(GEN)"))
        .await
        .unwrap();
    job.run(&format!(
        "RUNSQL SQL('create alias {lib}.genal for {lib}.src (gen)') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    for line in ["       dcl-s a int(10);", "       a = 1;"] {
        as4tainr_core::sql::run(
            &client,
            &url,
            std::slice::from_ref(&lib),
            &format!("insert into {lib}.genal (srcseq, srcdta) values (0, '{line}')"),
        )
        .await
        .unwrap();
    }
    let m = as4tainr_core::source::find_member(&client, std::slice::from_ref(&lib), "SRC", "GEN")
        .await
        .unwrap();
    assert_eq!(m.source, "       dcl-s a int(10);\n       a = 1;");
    // The records keep the SRCSEQ inserted (PUB400 2026-10-28,
    // tools/golden/screens6.txt 07_dsppfm: inserted 1.00 and 2.50 show
    // as 000100 and 000250; this test expected the lines renumbered
    // 1.00, 2.00, which as4tainr did before it kept them).
    assert_eq!(
        rows(format!("select srcseq, trim(srcdta) from {lib}.genal")).await,
        ["0.00|dcl-s a int(10);", "0.00|a = 1;"]
    );
    job.run(&format!(
        "RUNSQL SQL('drop alias {lib}.genal') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    assert!(as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("select * from {lib}.genal")
    )
    .await
    .is_err());
}

/// Embedded SQL as rpgle-repl uses it: data definition prepared and run
/// with EXECUTE (CREATE ALIAS); OVRDBF FILE TOFILE MBR, which SQL
/// statements of the job then use (Db2 for i SQL Programming, overrides);
/// SQL comments with apostrophes; OPTIONS(*CONVERT) (ILE RPG Reference,
/// OPTIONS: a number passed to a character parameter as %CHAR gives it).
#[tokio::test]
async fn embedded_sql_dynamic_ddl_overrides_and_convert() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.run(&format!("CRTSRCPF FILE({lib}/QRPGLESRC) RCDLEN(112)"))
        .await
        .unwrap();
    job.run(&format!("CRTSRCPF FILE({lib}/NOTES) RCDLEN(112) MBR(N1)"))
        .await
        .unwrap();
    let src = format!(
        r#"**free
ctl-opt dftactgrp(*no);
dcl-pr qcmdexc extpgm;
  cmd char(200) const;
  len packed(15:5) const;
end-pr;
dcl-pr show;
  v varchar(30) value options(*convert);
end-pr;
dcl-s stmt varchar(200);
dcl-s n int(10);
dcl-s line varchar(80);
stmt = 'CREATE ALIAS {lib}.NOTEAL FOR {lib}.NOTES (N1)';
exec sql prepare s1 from :stmt;
exec sql execute s1;
exec sql insert into {lib}.noteal (srcseq, srcdta) /* it's the note */
  values (0, 'first note');
qcmdexc('OVRDBF FILE(MYNOTES) TOFILE({lib}/NOTES) MBR(N1)': 200);
exec sql select count(*), max(srcdta) into :n, :line from mynotes;
show(n);
dsply line;
*inlr = *on;
dcl-proc show;
  dcl-pi *n;
    v varchar(30) value options(*convert);
  end-pi;
  dsply ('n=' + v);
end-proc;
"#
    );
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QRPGLESRC".into(),
            name: "SQLOVR".into(),
            srctype: "SQLRPGLE".into(),
            source: src,
            ccsid: 37,
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    job.run(&format!(
        "CRTSQLRPGI OBJ({lib}/SQLOVR) SRCFILE({lib}/QRPGLESRC) COMMIT(*NONE) OUTPUT(*NONE)"
    ))
    .await
    .unwrap_or_else(|e| panic!("{e:#}"));
    let r = job.run(&format!("CALL PGM({lib}/SQLOVR)")).await.unwrap();
    let text = format!("{r:?}");
    assert!(
        text.contains("n=1") && text.contains("first note"),
        "{text}"
    );
}

/// SYSTOOLS.SPOOLED_FILE_DATA (Db2 for i services): the lines of a job's
/// spooled file, the highest-numbered of the name when no number is
/// given, ORDINAL_POSITION from 1.
#[tokio::test]
async fn spooled_file_data_gives_the_lines() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let job: i64 = 900_000 + (std::process::id() % 90_000) as i64;
    for (n, lines) in [(1, vec!["old"]), (2, vec!["first line", "second line"])] {
        let id: i64 = client
            .query_one(
                "INSERT INTO as4tainr.spooled_files (job, job_name, user_name, file, number)
                 VALUES ($1, 'SPLJOB', 'QSECOFR', 'REPORT', $2) RETURNING id",
                &[&job, &n],
            )
            .await
            .unwrap()
            .get(0);
        for (k, l) in lines.iter().enumerate() {
            client
                .execute(
                    "INSERT INTO as4tainr.spool_lines (spl, seq, page, line, text) VALUES ($1, $2, 1, $2, $3)",
                    &[&id, &(k as i32 + 1), &l.to_string()],
                )
                .await
                .unwrap();
        }
    }
    let q = format!(
        "select ordinal_position, trim(spooled_data) from table(systools.spooled_file_data(job_name => '{job:06}/QSECOFR/SPLJOB', spooled_file_name => 'REPORT')) x"
    );
    match as4tainr_core::sql::run(&client, &url, std::slice::from_ref(&lib), &q)
        .await
        .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => assert_eq!(
            rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>(),
            ["1|first line", "2|second line"]
        ),
        other => panic!("{other:?}"),
    }
    client
        .execute(
            "DELETE FROM as4tainr.spooled_files WHERE job = $1 AND job_name = 'SPLJOB'",
            &[&job],
        )
        .await
        .unwrap();
}

/// DECLARE GLOBAL TEMPORARY TABLE (SQL Reference, "DECLARE GLOBAL
/// TEMPORARY TABLE"): the table is made in QTEMP, whose SQL name is
/// SESSION; WITH REPLACE makes it again; SET SCHEMA SESSION makes the
/// unqualified names in the statements after it name QTEMP tables. A
/// column DEFAULT USER takes the user running the statement (SQL
/// Reference, CREATE TABLE, "default-clause": USER).
#[tokio::test]
async fn declared_temporary_table_and_default_user() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "create table {lib}.dgtout (n int, who char(10) default user);
         declare global temporary table dgt1 (a int, b char(3)) with replace;
         insert into session.dgt1 values (1, 'ONE');
         declare global temporary table session.dgt1 (a int, b char(3)) on commit preserve rows not logged with replace;
         insert into session.dgt1 values (2, 'TWO');
         set schema session;
         insert into dgt1 values (3, 'THR');
         insert into {lib}.dgtout (n) select sum(a) from dgt1;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("SQL0") || l.contains("SQL1")),
        "{lines:?}"
    );
    match as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("select n, who from {lib}.dgtout"),
    )
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => {
            assert_eq!(rows, [["5".to_string(), "QSECOFR   ".to_string()]])
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Db2 for i casts a character string compared with a number, or used in
/// arithmetic or MOD with one, to a number (SQL Reference, "Casting
/// between data types"; "Assignments and comparisons"); ALTER TABLE takes
/// several alterations one after another (SQL Reference, ALTER TABLE).
#[tokio::test]
async fn strings_compare_with_numbers_and_alter_table_alterations() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "create table {lib}.cstm (id char(4) not null, active char(1));
         insert into {lib}.cstm values (1, 'N'), (2, 'N'), (10, 'N');
         update {lib}.cstm set active = 'Y' where mod(id, 2) = 0;
         alter table {lib}.cstm
           add column chgtime timestamp not null default current_timestamp
           add column chguser varchar(18) not null default user;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("SQL0") || l.contains("SQL1")),
        "{lines:?}"
    );
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"))
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => {
                    rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>()
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    assert_eq!(
        rows(format!(
            "select trim(id), active, chguser from {lib}.cstm where id > 1 order by id"
        ))
        .await,
        ["10|Y|QSECOFR", "2|Y|QSECOFR"]
    );
    assert_eq!(
        rows(format!(
            "select '5' + 1, 2 * '3', 10 > '9', 10 > 9 from {lib}.cstm where id = 1"
        ))
        .await,
        ["6|6|TRUE|TRUE"]
    );
    // The library list the statement sees is the libraries only.
    assert_eq!(
        rows("select schema_name from qsys2.library_list_info".into()).await,
        std::slice::from_ref(&lib)
    );
}

/// A job running commands as QSECOFR with the library first.
/// `run_in` in an interactive job of its own (its own job key, so its
/// call stack and messages are not shared with other tests' jobs).
async fn run_in_own_job(
    client: &Client,
    url: &str,
    lib: &str,
    line: &str,
) -> anyhow::Result<Vec<String>> {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let env = Environment {
        database_url: url.to_string(),
    };
    let id = as4tainr_core::jobs::start_interactive(
        client,
        &format!("J{}", &lib[1..]),
        "QSECOFR",
        None,
        &[lib.to_string(), "QGPL".into()],
    )
    .await?;
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.to_string()),
        libl: vec![lib.to_string(), "QGPL".into()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    match job.run(line).await? {
        Outcome::Messages(m) => Ok(m),
        other => panic!("unexpected {other:?}"),
    }
}

async fn run_in(client: &Client, url: &str, lib: &str, line: &str) -> anyhow::Result<Vec<String>> {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let env = Environment {
        database_url: url.to_string(),
    };
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.to_string()),
        libl: vec![lib.to_string(), "QGPL".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    match job.run(line).await? {
        Outcome::Messages(m) => Ok(m),
        other => panic!("unexpected {other:?}"),
    }
}

/// Commands as the IBMi-topics corpus (MarcoDeSenas, GPL-3.0) builds
/// them. CL reference, "Command Definition (CMD)": VLDCKR, ALLOW and the
/// other CRTCMD values on the CMD statement override CRTCMD's. PARM
/// RTNVAL(*YES): the command processing program returns a value in the
/// CL variable given. INCLUDE SRCSTMF: a relative path is completed with
/// the source's directory. DCLPRCOPT DFTACTGRP(*NO) ACTGRP BNDSRVPGM:
/// an ILE CL program bound to a service program it calls with CALLPRC.
/// MI MATPGMNM: the bound program's library at offset 18 and name at 50.
/// QWVRCSTK: the most recent entry first; a receiver too small for the
/// entries leaves the information status blank. The validity checker's
/// caller is the command analyzer QSYS/QCATRS (the corpus' validity
/// checkers, written on PUB400, look for it one entry up).
#[tokio::test]
async fn command_statement_validity_checker_and_rtnval() {
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let dir = format!("/t/{}", lib.to_ascii_lowercase());
    let put = |path: String, text: &'static str| {
        let client = &client;
        async move { source::put_stream_file(client, &path, text).await.unwrap() }
    };
    put(
        format!("{dir}/common/includes/inc_decl.clle"),
        "DCL VAR(&DATA) TYPE(*CHAR) LEN(80)\nDCL VAR(&STK) TYPE(*CHAR) LEN(40)\n",
    )
    .await;
    // The service program: the program names on the call stack, most
    // recent first, its own left out; and the information status after
    // a call with a receiver only as long as the header.
    put(
        format!("{dir}/tools/stack.rpgle"),
        "**free
ctl-opt nomain;
dcl-proc Callers export;
  dcl-pi *n;
    res char(40);
  end-pi;
  dcl-pr getStack extpgm('QWVRCSTK');
    rcv char(65535) options(*varsize);
    len int(10) const;
    fmt char(8) const;
    jid char(56) const;
    jfmt char(8) const;
    err char(16);
  end-pr;
  dcl-ds hdr qualified;
    rtn int(10);
    avl int(10);
    ent int(10);
    off int(10);
    nret int(10);
    thread char(8);
    status char(1);
  end-ds;
  dcl-s buf char(4096);
  dcl-s jid char(56);
  dcl-s err char(16);
  dcl-s i int(10);
  dcl-s p pointer;
  dcl-ds e qualified based(p);
    len int(10) pos(1);
    pgm char(10) pos(25);
  end-ds;
  jid = '*';
  %subst(jid:43:2) = x'0000';
  %subst(jid:45:4) = x'00000001';
  %subst(jid:49:8) = *allx'00';
  getStack(hdr:%size(hdr):'CSTK0100':jid:'JIDF0100':err);
  res = 'S' + hdr.status + '|';
  getStack(buf:%size(buf):'CSTK0100':jid:'JIDF0100':err);
  hdr = buf;
  p = %addr(buf) + hdr.off;
  for i = 1 to hdr.nret;
    if e.pgm <> 'STACK';
      res = %trimr(res) + %trimr(e.pgm) + '/';
    endif;
    p += e.len;
  endfor;
end-proc;
",
    )
    .await;
    put(
        format!("{dir}/tools/chk.clle"),
        "PGM PARM(&IN &OUT)
DCLPRCOPT DFTACTGRP(*NO) ACTGRP(TOPICS) BNDSRVPGM((STACK))
DCL VAR(&IN) TYPE(*CHAR) LEN(10)
DCL VAR(&OUT) TYPE(*CHAR) LEN(10)
INCLUDE SRCSTMF('../Common/Includes/inc_decl.clle')
CHGVAR VAR(%BIN(&DATA 1 4)) VALUE(80)
CALLPRC PRC('_MATPGMNM') PARM((&DATA))
SNDPGMMSG MSG('PGM' *BCAT %SST(&DATA 51 10) *TCAT '/' *CAT %SST(&DATA 19 10)) TOPGMQ(*EXT)
CALLPRC PRC('CALLERS') PARM(&STK)
SNDPGMMSG MSG(&STK) TOPGMQ(*EXT)
ENDPGM
",
    )
    .await;
    put(
        format!("{dir}/tools/cpp.clle"),
        "PGM PARM(&IN &OUT)
DCL VAR(&IN) TYPE(*CHAR) LEN(10)
DCL VAR(&OUT) TYPE(*CHAR) LEN(10)
CHGVAR VAR(&OUT) VALUE('got' *BCAT &IN)
ENDPGM
",
    )
    .await;
    put(
        format!("{dir}/tools/caller.clle"),
        "PGM
DCL VAR(&R) TYPE(*CHAR) LEN(10) VALUE('unchanged')
TCMD IN(abc) OUT(&R)
SNDPGMMSG MSG('R=' *CAT &R) TOPGMQ(*EXT)
ENDPGM
",
    )
    .await;
    source::create_file(&client, &lib, "QCMDSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QCMDSRC".into(),
            name: "TCMD".into(),
            srctype: "CMD".into(),
            source: "CMD PROMPT('Test') VLDCKR(CHK) ALLOW(*BPGM *IPGM) TEXT(*CMDPMT)
PARM KWD(IN) TYPE(*CHAR) LEN(10) MIN(1)
PARM KWD(OUT) TYPE(*CHAR) LEN(10) RTNVAL(*YES) MIN(1)
"
            .into(),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    for c in [
        format!("CRTRPGMOD MODULE({lib}/STACK) SRCSTMF('{dir}/tools/stack.rpgle')"),
        format!("CRTSRVPGM SRVPGM({lib}/STACK) MODULE({lib}/STACK) EXPORT(*ALL)"),
        format!("CRTBNDCL PGM({lib}/CHK) SRCSTMF('{dir}/tools/chk.clle')"),
        format!("CRTBNDCL PGM({lib}/CPP) SRCSTMF('{dir}/tools/cpp.clle')"),
        format!("CRTCMD CMD({lib}/TCMD) PGM({lib}/CPP) SRCFILE({lib}/QCMDSRC) VLDCKR(*NONE)"),
        format!("CRTBNDCL PGM({lib}/CALLER) SRCSTMF('{dir}/tools/caller.clle')"),
    ] {
        run_in(&client, &url, &lib, &c)
            .await
            .unwrap_or_else(|e| panic!("{c}: {e:#}"));
    }
    let def = as4tainr_core::cmd::find(&client, std::slice::from_ref(&lib), "TCMD")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(def.vldckr.as_deref(), Some("*LIBL/CHK"));
    assert_eq!(def.allow, ["*BPGM", "*IPGM"]);
    assert_eq!(def.text, "Test");
    // ALLOW(*BPGM *IPGM): not from a command line or `system` (CPD0031,
    // CPF0006: PUB400 2026-10-26, tools/golden/verify1.txt v_allow).
    let e = run_in(&client, &url, &lib, "TCMD IN(x) OUT(y)")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPD0031") && e.contains("CPF0006"), "{e}");
    // In a job of its own: jobs without one share the process's call
    // stack, which other tests running at the same time add to.
    let m = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/CALLER"))
        .await
        .unwrap();
    assert_eq!(
        m,
        [
            format!("PGM CHK/{lib}"),
            // Each ILE CL program is two entries, its entry procedure and
            // its procedure (PUB400 2026-10-26, tools/golden/verify1.txt
            // v_out: "S |GLDVCHK/GLDVCHK/QCATRS/GLDVCALL/GLDVCALL/...").
            "S |CHK/CHK/QCATRS/CALLER/CALLER/".to_string(),
            // An unquoted value is in capitals.
            "R=got ABC".to_string(),
        ],
        "{m:?}"
    );
}

/// IBM i services the IBMi-topics corpus uses. QSYS2.QCMDEXC as a scalar
/// function (rpgpgm.com, "QCMDEXC scalar function added to SQL", on the
/// IBM i 7.4 TR4 service): 1 when the command ran, -1 when it failed.
/// SYSTOOLS.LPRINTF: a procedure writing a line to the job log.
/// QSYS2.ENVIRONMENT_VARIABLE_INFO (system name ENV_VARS; columns
/// ENVIRONMENT_VARIABLE_TYPE VAR_TYPE, _NAME VAR_NAME, _VALUE VAR_VALUE,
/// _CCSID VAR_CCSID...): the job's variables and the system's; an RPG
/// data structure EXTNAME('QSYS2/ENV_VARS') has the system column names.
#[tokio::test]
async fn qcmdexc_function_lprintf_and_environment_variables_view() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "create table {lib}.res (n int, rc int);
         begin
           declare cmd char(100);
           declare issue int;
           set cmd = 'CRTDTAARA DTAARA({lib}/TOPICS) TYPE(*CHAR) LEN(5)';
           select qsys2.qcmdexc(cmd) into issue from sysibm.sysdummy1;
           insert into {lib}.res values (1, issue);
           select qsys2.qcmdexc(cmd) into issue from sysibm.sysdummy1;
           insert into {lib}.res values (2, issue);
           if qsys2.qcmdexc('CHGDTAARA DTAARA({lib}/TOPICS) VALUE(''abc'')') = 1 then
             insert into {lib}.res values (3, 1);
           end if;
           call systools.lprintf('line for the job log');
         end;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("SQL0") || l.contains("SQL1")),
        "{lines:?}"
    );
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"))
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => {
                    rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>()
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    // The second CRTDTAARA fails: the data area exists (CPF1023).
    assert_eq!(
        rows(format!("select n, rc from {lib}.res order by n")).await,
        ["1|1", "2|-1", "3|1"]
    );
    // In a statement of its own (RUNSQL INSERT ... SELECT QSYS2.QCMDEXC:
    // PUB400 2026-10-26, tools/golden/verify1.txt v_qcmdexc1/2 gave 1,
    // then -1 for the same CRTDTAARA).
    for _ in 0..2 {
        as4tainr_core::sql::run(
            &client,
            &url,
            std::slice::from_ref(&lib),
            &format!(
                "insert into {lib}.res select 4, qsys2.qcmdexc('CRTDTAARA DTAARA({lib}/TOPICS2) TYPE(*CHAR)') from sysibm.sysdummy1"
            ),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        rows(format!("select rc from {lib}.res where n = 4")).await,
        ["1", "-1"]
    );
    let v: String = client
        .query_one(
            "SELECT value FROM as4tainr.data_areas WHERE library = $1 AND name = 'TOPICS'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(v.trim_end(), "abc");

    // A job variable and a system variable.
    let jv = format!("TOPICS_{lib}");
    run_in(
        &client,
        &url,
        &lib,
        &format!("ADDENVVAR ENVVAR({jv}) VALUE('job value')"),
    )
    .await
    .unwrap();
    run_in(
        &client,
        &url,
        &lib,
        &format!("ADDENVVAR ENVVAR({jv}) VALUE('sys value') LEVEL(*SYS)"),
    )
    .await
    .unwrap();
    assert_eq!(
        rows(format!(
            "select environment_variable_type, environment_variable_value
               from qsys2.environment_variable_info
              where environment_variable_name = '{jv}'"
        ))
        .await,
        ["JOB|job value", "SYSTEM|sys value"]
    );
    // A job variable with CCSID(*JOB) shows the job's CCSID (PUB400
    // 2026-10-26, tools/golden/verify1.txt v_out: 273, its job CCSID;
    // as4tainr's jobs are CCSID 37).
    assert_eq!(
        rows(format!(
            "select environment_variable_ccsid from qsys2.environment_variable_info
              where environment_variable_name = '{jv}' and environment_variable_type = 'JOB'"
        ))
        .await,
        ["37"]
    );
    assert_eq!(
        rows(format!("select var_type, var_value from qsys2.env_vars where var_name = '{jv}' and var_type = 'SYSTEM'"))
            .await,
        ["SYSTEM|sys value"]
    );
    run_in(
        &client,
        &url,
        &lib,
        &format!("RMVENVVAR ENVVAR({jv}) LEVEL(*SYS)"),
    )
    .await
    .unwrap();
    run_in(&client, &url, &lib, &format!("RMVENVVAR ENVVAR({jv})"))
        .await
        .unwrap();
    assert!(rows(format!(
        "select 1 from qsys2.environment_variable_info where environment_variable_name = '{jv}'"
    ))
    .await
    .is_empty());
    // EXTNAME of the view: its system column names.
    let src = "**free
dcl-ds ev extname('QSYS2/ENV_VARS') qualified end-ds;
ev.var_type = 'JOB';
dsply %trim(ev.var_type);
dsply %char(%size(ev.var_ccsid));
*inlr = *on;
";
    as4tainr_core::source::put_stream_file(&client, &format!("/t/{lib}/ev.rpgle"), src)
        .await
        .unwrap();
    run_in(
        &client,
        &url,
        &lib,
        &format!("CRTBNDRPG PGM({lib}/EV) SRCSTMF('/t/{lib}/ev.rpgle')"),
    )
    .await
    .unwrap();
    let m = run_in(&client, &url, &lib, &format!("CALL {lib}/EV"))
        .await
        .unwrap();
    assert_eq!(m, ["DSPLY  JOB", "DSPLY  4"], "{m:?}");
}

/// ILE RPG Reference, QUALIFIED and LIKE: a qualified data structure's
/// subfields are known only qualified, so a subfield LIKE(JobUser) names
/// the standalone field JobUser even when it is itself declared LIKE
/// another field (the IBMi-topics corpus' QWc_JIDF0100: JobUser
/// like(JobUser), the global JobUser like(UserProfile)).
#[tokio::test]
async fn rpg_like_names_a_field_outside_the_qualified_ds() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let src = "**free
dcl-s UserProfile char(10);
dcl-s JobUser like(UserProfile);
dcl-ds jidf qualified;
  JobUser like(JobUser);
  Rest char(2);
end-ds;
jidf.JobUser = 'ABCDEFGHIJKL';
dsply jidf.JobUser;
dsply %char(%size(jidf));
*inlr = *on;
";
    let r = run_rpg(&url, &lib, "QSECOFR", "LIKEQ", src, None, None).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.messages, ["ABCDEFGHIJ", "12"]);
}

/// DSPOBJD OUTPUT(*OUTFILE) and DSPFD TYPE(*MBRLIST) OUTPUT(*OUTFILE):
/// the output file is made like the model in QSYS (QADSPOBJ, format
/// QLIDOBJD, 116 fields, record length 726; QAFDMBRL, format QWHFDML, 32
/// fields, record length 202: DSPFFD on PUB400 2026-10-26,
/// tools/golden/verify1.txt v_ffd_objd, v_ffd_mbrl), a record per object
/// or member. The field descriptions there give the formats: creation
/// dates MMDDYY for DSPOBJD and YYMMDD for DSPFD, each with a century
/// field (1 = 20xx). OUTMBR(*FIRST *ADD) adds; *REPLACE (the default)
/// replaces.
#[tokio::test]
async fn dspobjd_and_dspfd_outfiles() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    for c in [
        format!("CRTDTAARA DTAARA({lib}/ODA) TYPE(*CHAR) TEXT('outfile test')"),
        format!("CRTSRCPF FILE({lib}/QOSRC)"),
        format!("ADDPFM FILE({lib}/QOSRC) MBR(A1) SRCTYPE(CLLE) TEXT('first')"),
        format!("ADDPFM FILE({lib}/QOSRC) MBR(B2) SRCTYPE(RPGLE)"),
    ] {
        run_in(&client, &url, &lib, &c)
            .await
            .unwrap_or_else(|e| panic!("{c}: {e:#}"));
    }
    // The models: their fields and record lengths.
    for (file, fields, len) in [("QADSPOBJ", 116i64, 726i64), ("QAFDMBRL", 32, 202)] {
        let r = client
            .query_one(
                "SELECT count(*), coalesce(sum(CASE WHEN data_type = 'P' THEN length / 2 + 1 ELSE length END), 0)::bigint
                   FROM as4tainr.file_fields WHERE library = 'QSYS' AND file = $1",
                &[&file],
            )
            .await
            .unwrap();
        assert_eq!(
            (r.get::<_, i64>(0), r.get::<_, i64>(1)),
            (fields, len),
            "{file}"
        );
    }
    run_in(
        &client,
        &url,
        &lib,
        &format!("DSPOBJD OBJ({lib}/ODA) OBJTYPE(*DTAARA) OUTPUT(*OUTFILE) OUTFILE({lib}/OBJS)"),
    )
    .await
    .unwrap();
    let today: (String, String) = {
        let r = client
            .query_one(
                "SELECT to_char(now() AT TIME ZONE 'UTC', 'MMDDYY'), to_char(now() AT TIME ZONE 'UTC', 'YYMMDD')",
                &[],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    let t = format!("{}.objs", lib.to_ascii_lowercase());
    let r = client
        .query_one(
            &format!(
                "SELECT rtrim(odlbnm), rtrim(odobnm), rtrim(odobtp), rtrim(odobtx), rtrim(oddtl),
                        odccen, odcdat, rtrim(odobow) FROM {t}"
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..8).map(|i| r.get(i)).collect();
    assert_eq!(
        got,
        [
            lib.as_str(),
            "ODA",
            "*DTAARA",
            "outfile test",
            "*BASIC",
            "1",
            today.0.as_str(),
            "QSECOFR"
        ]
    );
    run_in(
        &client,
        &url,
        &lib,
        &format!("DSPOBJD OBJ({lib}/ODA) OBJTYPE(*DTAARA) OUTPUT(*OUTFILE) OUTFILE({lib}/OBJS) OUTMBR(*FIRST *ADD)"),
    )
    .await
    .unwrap();
    let n: i64 = client
        .query_one(&format!("SELECT count(*) FROM {t}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 2);
    run_in(
        &client,
        &url,
        &lib,
        &format!("DSPFD FILE({lib}/QOSRC) TYPE(*MBRLIST) OUTPUT(*OUTFILE) OUTFILE({lib}/MBRS)"),
    )
    .await
    .unwrap();
    let rows = client
        .query(
            &format!(
                "SELECT rtrim(mlfile), rtrim(mlname), rtrim(mlseu2), rtrim(mlmtxt), mlnomb::text,
                        mlftyp, rtrim(mlfila), rtrim(mlfatr), mlccen, mlcdat
                   FROM {}.mbrs ORDER BY mlname",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<Vec<String>> = rows
        .iter()
        .map(|r| (0..10).map(|i| r.get(i)).collect())
        .collect();
    let row = |m: &str, t: &str, x: &str| {
        vec![
            "QOSRC".to_string(),
            m.into(),
            t.into(),
            x.into(),
            "2".into(),
            "P".into(),
            "*PHY".into(),
            "PF".into(),
            "1".into(),
            today.1.clone(),
        ]
    };
    assert_eq!(got, [row("A1", "CLLE", "first"), row("B2", "RPGLE", "")]);
    // TYPE(*ATR) to an outfile with FILEATR(*ALL), the default: CPD3006
    // (PUB400 2026-10-26, tools/golden/verify2.txt o_atr).
    let e = run_in(
        &client,
        &url,
        &lib,
        &format!("DSPFD FILE({lib}/QOSRC) TYPE(*ATR) OUTPUT(*OUTFILE) OUTFILE({lib}/ATRS)"),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.starts_with("CPD3006"), "{e}");
    // FILEATR(*PF): a QAFDPHY record (PUB400 2026-10-27,
    // tools/golden/verify3.txt x_fd_phy: CPF9861, CPF9862, CPF3030; a
    // source file's PHDTAT is S, the model's field description).
    let m = run_in(
        &client,
        &url,
        &lib,
        &format!(
            "DSPFD FILE({lib}/QOSRC) TYPE(*ATR) FILEATR(*PF) OUTPUT(*OUTFILE) OUTFILE({lib}/ATRS)"
        ),
    )
    .await
    .unwrap();
    let ids: Vec<&str> = m
        .iter()
        .map(|x| as4tainr_core::jobs::message_id(x))
        .collect();
    assert_eq!(ids, ["CPF9861", "CPF9862", "CPF3030"]);
    let r = client
        .query_one(
            &format!(
                "SELECT rtrim(phfile), phfila, phdtat, phnomb::text, phjrnl FROM {}.atrs",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..5).map(|i| r.get(i)).collect();
    assert_eq!(got, ["QOSRC", "*PHY", "S", "2", "N"]);
    // Other kinds of files do not go to an output file here.
    let e = run_in(
        &client,
        &url,
        &lib,
        &format!(
            "DSPFD FILE({lib}/QOSRC) TYPE(*ATR) FILEATR(*LF) OUTPUT(*OUTFILE) OUTFILE({lib}/ATRS)"
        ),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.starts_with("CPD0084"), "{e}");
}

/// System-period temporal tables (Db2 for i SQL Programming, "Temporal
/// tables"; SQL Reference, CREATE TABLE PERIOD SYSTEM_TIME, ALTER TABLE
/// ADD VERSIONING, table-reference period-specification, DROP TABLE), as
/// IBMi-topics' cmdgrp.sql makes one: ROW BEGIN, ROW END and TRANSACTION
/// START ID columns, a history table made with LIKE, ADD VERSIONING ...
/// ON DELETE ADD EXTRA ROW, columns SET IMPLICITLY HIDDEN in one ALTER.
/// An update moves the old row to the history table, ending when the new
/// version begins; a delete moves it and, with the extra row, adds one
/// for the delete itself (DATA CHANGE OPERATION D); the current row's end
/// is 9999-12-30-00.00.00. FOR SYSTEM_TIME AS OF reads the version of
/// that time from both tables. Dropping the table drops its history.
#[tokio::test]
async fn system_period_temporal_tables() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    // A journal QSQJRN in the library: the tables SQL makes are journaled
    // to it (PUB400 2026-10-27, tools/golden/verify4.txt y_to_jrn), which
    // UPDATE and DELETE of a versioned table need (verify3 x_errors).
    for c in [
        format!("CRTJRNRCV JRNRCV({lib}/QSQJRN0001)"),
        format!("CRTJRN JRN({lib}/QSQJRN) JRNRCV({lib}/QSQJRN0001)"),
    ] {
        job.run(&c).await.unwrap();
    }
    let script = format!(
        "set schema = {lib};
         create table TGRP (
           GRP char(10) not null, SEQ int not null, CMD varchar(100) not null,
           AUDIT_OPER char(1) generated always as (data change operation),
           SYS_START timestamp(12) not null generated always as row begin,
           SYS_END timestamp(12) not null generated always as row end,
           TS_ID timestamp(12) not null generated always as transaction start id,
           period system_time (SYS_START, SYS_END));
         alter table TGRP add primary key (GRP, SEQ);
         create table TGRP_HST like TGRP;
         alter table TGRP add versioning use history table TGRP_HST on delete add extra row;
         alter table TGRP
           alter column SYS_START set implicitly hidden
           alter column SYS_END set implicitly hidden
           alter column TS_ID set implicitly hidden;
         commit;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(!lines.iter().any(|l| l.contains(": SQL")), "{lines:?}");
    let rows = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            match as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"))
            {
                as4tainr_core::sql::Outcome::Rows { rows, .. } => {
                    rows.into_iter().map(|r| r.join("|")).collect::<Vec<_>>()
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let change = |sql: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move {
            as4tainr_core::sql::run(client, &url, &[lib], &sql)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:#}"));
        }
    };
    change(format!(
        "insert into {lib}.tgrp (grp, seq, cmd) values('G1', 1, 'DSPLIBL')"
    ))
    .await;
    // CHAR of the TIMESTAMP(12) row end has 12 fractional digits (PUB400
    // 2026-10-26, tools/golden/verify2.txt w_cur).
    assert_eq!(
        rows(format!("select audit_oper, char(sys_end) from {lib}.tgrp")).await,
        ["I|9999-12-30-00.00.00.000000000000"]
    );
    let first = rows(format!("select char(sys_start) from {lib}.tgrp")).await;
    change(format!("update {lib}.tgrp set cmd = 'WRKACTJOB'")).await;
    let second = rows(format!("select char(sys_start) from {lib}.tgrp")).await;
    // The old version: in the history table, ending when the new began.
    assert_eq!(
        rows(format!(
            "select cmd, audit_oper, char(sys_start), char(sys_end) from {lib}.tgrp_hst"
        ))
        .await,
        [format!("DSPLIBL|I|{}|{}", first[0], second[0])]
    );
    // AS OF the first version's start: that version.
    assert_eq!(
        rows(format!(
            "select cmd from {lib}.tgrp for system_time as of '{}'",
            first[0]
        ))
        .await,
        ["DSPLIBL"]
    );
    assert_eq!(
        rows(format!(
            "select count(*) from {lib}.tgrp for system_time from '2000-01-01-00.00.00' to '9999-12-30-00.00.00' t"
        ))
        .await,
        ["2"]
    );
    change(format!("delete from {lib}.tgrp")).await;
    let hist = rows(format!(
        "select cmd, audit_oper, sys_start = sys_end from {lib}.tgrp_hst order by sys_start, sys_end"
    ))
    .await;
    assert_eq!(hist.len(), 3, "{hist:?}");
    assert_eq!(
        hist[1].split('|').take(2).collect::<Vec<_>>(),
        ["WRKACTJOB", "U"]
    );
    assert_eq!(
        hist[2].split('|').take(2).collect::<Vec<_>>(),
        ["WRKACTJOB", "D"]
    );
    // Without versioning, a delete leaves no history.
    let lines = job
        .run_sql_script(
            &format!(
                "insert into {lib}.tgrp (grp, seq, cmd) values('G2', 1, 'X');
                 alter table {lib}.tgrp drop versioning;
                 delete from {lib}.tgrp;"
            ),
            "test",
            0,
            Some(lib.clone()),
            true,
        )
        .await
        .unwrap();
    assert!(!lines.iter().any(|l| l.contains(": SQL")), "{lines:?}");
    assert_eq!(
        rows(format!("select count(*) from {lib}.tgrp_hst")).await,
        ["3"]
    );
    // Dropping a versioned table drops its history table.
    let lines = job
        .run_sql_script(
            &format!(
                "alter table {lib}.tgrp add versioning use history table {lib}.tgrp_hst;
                 drop table {lib}.tgrp;"
            ),
            "test",
            0,
            Some(lib.clone()),
            true,
        )
        .await
        .unwrap();
    assert!(!lines.iter().any(|l| l.contains(": SQL")), "{lines:?}");
    let left: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.objects WHERE library = $1 AND name IN ('TGRP', 'TGRP_HST')",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
}

/// What PUB400 showed for the verify2 round (IBM i 7.5, 2026-10-26,
/// tools/golden/verify2.txt): RUNSQLSTM COMMIT(*NONE) refuses COMMIT and
/// ROLLBACK with SQL7007 (severity 30), stops there and fails with
/// SQL9010, and its listing shows each statement's message and record
/// (w_rb: SQL7956 for the INSERT at record 1, SQL7007 at record 3; the
/// summary 2 total, 1 info, 1 severe); IMPLICITLY HIDDEN columns are left
/// out of an INSERT without columns (w_ins_nocols: five values for eight
/// columns, three hidden; w_ins_nocols3: three values fail) and of
/// SELECT * (w_star: a table made from it has five columns); FOR
/// SYSTEM_TIME takes no scalar fullselect (w_asof) and needs a temporal
/// table (w_notemporal); DSPOBJD and DSPFD OUTPUT(*OUTFILE) send CPF9861
/// and CPF9862 when they make the file, DSPFD also CPF3030 each time,
/// DSPOBJD without OUTFILE is CPD9861 (o_objd, o_objd_again, o_mbrl,
/// o_mbrl_pf, o_objd_nofile); a DSPOBJD record's bytes per unit is 512,
/// the ASP numbers 1 and change by API allowed (o_objd_vals); the other
/// model files' field counts and record lengths are DSPFFD's (m_*).
#[tokio::test]
async fn verify2_runsqlstm_hidden_columns_and_outfiles() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.run(&format!(
        "RUNSQL SQL('CREATE TABLE {lib}/GLDTO (SEQ INTEGER GENERATED ALWAYS AS IDENTITY, ID CHAR(10), VAL VARCHAR(1000))') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    source::create_file(&client, &lib, "QSQLSRC", "")
        .await
        .unwrap();
    source::put_member(
        &client,
        &source::Member {
            library: lib.clone(),
            file: "QSQLSRC".into(),
            name: "GLDRB".into(),
            srctype: "SQL".into(),
            text: String::new(),
            source: format!(
                "-- COMMIT and ROLLBACK with COMMIT(*NONE).\ninsert into {lib}.GLDTO (ID, VAL) values ('RB1', 'before rollback');\nrollback;\ninsert into {lib}.GLDTO (ID, VAL) values ('RB2', 'after rollback');\ncommit;\n"
            ),
            ..Default::default()
        },
        false,
    )
    .await
    .unwrap();
    let e = job
        .run(&format!(
            "RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(GLDRB) COMMIT(*NONE)"
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("SQL9010") && e.contains("SQL7007"), "{e}");
    let ids = as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("SELECT RTRIM(ID) FROM {lib}.GLDTO ORDER BY SEQ"),
    )
    .await
    .unwrap();
    let as4tainr_core::sql::Outcome::Rows { rows, .. } = ids else {
        panic!("{ids:?}")
    };
    assert_eq!(rows, [["RB1".to_string()]]);
    let l = &job.calls.printed;
    assert!(
        l.iter()
            .any(|x| x.starts_with("SQL7956   0       1  Position 1 ")),
        "{l:#?}"
    );
    assert!(
        l.iter()
            .any(|x| x.starts_with("SQL7007  30       3  Position 1 ")),
        "{l:#?}"
    );
    assert!(
        l.iter()
            .any(|x| x == "      2        1         0          0        1         0"),
        "{l:#?}"
    );
    assert!(l
        .iter()
        .any(|x| x == "30 level severity errors found in source"));
    // The same script under commitment control (COMMIT(*CHG), the
    // default): ROLLBACK takes back the INSERT before it, COMMIT keeps
    // the one after, and the end commits (PUB400 2026-10-27,
    // tools/golden/verify3.txt x_cmt: SQL7961, SQL7960, then SQL7960
    // with no record; CMT1 gone, CMT2 and CMT3 kept).
    job.run(&format!("RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(GLDRB)"))
        .await
        .unwrap();
    let ids = as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("SELECT RTRIM(ID) FROM {lib}.GLDTO ORDER BY SEQ"),
    )
    .await
    .unwrap();
    let as4tainr_core::sql::Outcome::Rows { rows, .. } = ids else {
        panic!("{ids:?}")
    };
    assert_eq!(rows, [["RB1".to_string()], ["RB2".to_string()]]);
    let l = &job.calls.printed;
    assert!(
        l.iter()
            .any(|x| x.starts_with("SQL7961   0       3  Position 1 ")),
        "{l:#?}"
    );
    assert!(
        l.iter()
            .any(|x| x.starts_with("SQL7960   0       5  Position 1 ")),
        "{l:#?}"
    );
    assert!(
        l.iter().any(|x| x.starts_with("SQL7960   0          ")),
        "{l:#?}"
    );
    assert!(l
        .iter()
        .any(|x| x == "00 level severity errors found in source"));
    job.run(&format!(
        "RUNSQL SQL('DELETE FROM {lib}/GLDTO') COMMIT(*NONE)"
    ))
    .await
    .unwrap();

    // A temporal table with hidden columns, as gldtmp.sql makes it.
    let script = format!(
        "set schema = {lib};
         create table GLDTMP (GRP char(10) not null, SEQ int not null, CMD varchar(100) not null,
           AUDIT_USER varchar(128) generated always as (SESSION_USER),
           AUDIT_OPER char(1) generated always as (data change operation),
           SYS_START timestamp(12) not null generated always as row begin,
           SYS_END timestamp(12) not null generated always as row end,
           TS_ID timestamp(12) not null generated always as transaction start id,
           period system_time (SYS_START, SYS_END));
         create table GLDTMP_HST like GLDTMP;
         alter table GLDTMP add versioning use history table GLDTMP_HST on delete add extra row;
         alter table GLDTMP
           alter column SYS_START set implicitly hidden
           alter column SYS_END set implicitly hidden
           alter column TS_ID set implicitly hidden;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(!lines.iter().any(|l| l.contains(": SQL")), "{lines:?}");
    let sql = |s: String| {
        let client = &client;
        let url = url.clone();
        let lib = lib.clone();
        async move { as4tainr_core::sql::run(client, &url, &[lib], &s).await }
    };
    sql(format!(
        "INSERT INTO {lib}.GLDTMP VALUES('G2', 1, 'DSPJOB', DEFAULT, DEFAULT)"
    ))
    .await
    .unwrap();
    // verify3 x_errors: SQL0117.
    let e = sql(format!(
        "INSERT INTO {lib}.GLDTMP VALUES('G3', 1, 'WRKSPLF')"
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("SQL0117"), "{e}");
    match sql(format!("SELECT * FROM {lib}.GLDTMP")).await.unwrap() {
        as4tainr_core::sql::Outcome::Rows { columns, .. } => {
            assert_eq!(columns, ["GRP", "SEQ", "CMD", "AUDIT_USER", "AUDIT_OPER"])
        }
        other => panic!("{other:?}"),
    }
    // Named, a hidden column is there.
    match sql(format!("SELECT T.*, CHAR(SYS_END) FROM {lib}.GLDTMP T"))
        .await
        .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { columns, rows, .. } => {
            assert_eq!(columns.len(), 6, "{columns:?}");
            assert_eq!(rows[0][5], "9999-12-30-00.00.00.000000000000");
        }
        other => panic!("{other:?}"),
    }
    job.run(&format!(
        "RUNSQL SQL('CREATE TABLE {lib}/GLDTSTAR AS (SELECT * FROM {lib}/GLDTMP) WITH NO DATA') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let n: i64 = client
        .query_one(
            "SELECT count(*) FROM as4tainr.file_fields WHERE library = $1 AND file = 'GLDTSTAR'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 5);
    let e = sql(format!(
        "SELECT CMD FROM {lib}.GLDTMP FOR SYSTEM_TIME AS OF (SELECT MIN(SYS_START) FROM {lib}.GLDTMP_HST)"
    ))
    .await
    .unwrap_err()
    .to_string();
    // verify3 x_errors: SQ20524 for a fullselect, a column and a table
    // that is not temporal.
    assert!(e.contains("SQ20524") && e.contains("fullselect"), "{e}");
    for q in [
        format!("SELECT CMD FROM {lib}.GLDTMP FOR SYSTEM_TIME AS OF SYS_START"),
        format!("SELECT VAL FROM {lib}.GLDTO FOR SYSTEM_TIME AS OF CURRENT TIMESTAMP"),
    ] {
        let e = sql(q).await.unwrap_err().to_string();
        assert!(e.contains("SQ20524"), "{e}");
    }
    // FROM ... TO with constants still works (w_fromto: the current row).
    match sql(format!(
        "SELECT COUNT(*) FROM {lib}.GLDTMP FOR SYSTEM_TIME FROM '2000-01-01-00.00.00' TO '9999-12-30-00.00.00' T"
    ))
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => assert_eq!(rows, [["1".to_string()]]),
        other => panic!("{other:?}"),
    }
    // A delete's extra row (begin = end) is in no period: FROM ... TO
    // counts the version that was deleted, not its extra row (verify3
    // x_asof: 2 for an insert, an update and a delete).
    // Not journaled: UPDATE and DELETE of the versioned table are SQL7008
    // naming the history table (PUB400 2026-10-27, verify3 x_errors);
    // journaled, they work (verify3 x_upd, x_del).
    let e = sql(format!("DELETE FROM {lib}.GLDTMP"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("SQL7008") && e.contains("GLDTMP_HST"), "{e}");
    for c in [
        format!("CRTJRNRCV JRNRCV({lib}/GLDRCV)"),
        format!("CRTJRN JRN({lib}/GLDJRN) JRNRCV({lib}/GLDRCV)"),
        format!("STRJRNPF FILE({lib}/GLDTMP {lib}/GLDTMP_HST) JRN({lib}/GLDJRN) IMAGES(*BOTH)"),
    ] {
        job.run(&c).await.unwrap();
    }
    sql(format!("DELETE FROM {lib}.GLDTMP")).await.unwrap();
    match sql(format!(
        "SELECT COUNT(*) FROM {lib}.GLDTMP FOR SYSTEM_TIME FROM '2000-01-01-00.00.00' TO '9999-12-30-00.00.00' T"
    ))
    .await
    .unwrap()
    {
        as4tainr_core::sql::Outcome::Rows { rows, .. } => assert_eq!(rows, [["1".to_string()]]),
        other => panic!("{other:?}"),
    }
    // DROP VERSIONING of a table that is not versioned: SQ20525.
    let drop = format!("RUNSQL SQL('ALTER TABLE {lib}/GLDTMP DROP VERSIONING') COMMIT(*NONE)");
    job.run(&drop).await.unwrap();
    let e = job.run(&drop).await.unwrap_err().to_string();
    assert!(e.contains("SQ20525"), "{e}");

    // Output files: the messages.
    job.run(&format!(
        "CRTDTAARA DTAARA({lib}/GLDODA) TYPE(*CHAR) LEN(10) TEXT('outfile golden')"
    ))
    .await
    .unwrap();
    let ids = |o: Outcome| -> Vec<String> {
        match o {
            Outcome::Messages(m) => m
                .iter()
                .map(|x| as4tainr_core::jobs::message_id(x).to_string())
                .collect(),
            other => panic!("{other:?}"),
        }
    };
    let objd = format!(
        "DSPOBJD OBJ({lib}/GLDODA) OBJTYPE(*DTAARA) OUTPUT(*OUTFILE) OUTFILE({lib}/GLDOBJ)"
    );
    assert_eq!(ids(job.run(&objd).await.unwrap()), ["CPF9861", "CPF9862"]);
    assert!(ids(job.run(&objd).await.unwrap()).is_empty());
    let e = job
        .run(&format!(
            "DSPOBJD OBJ({lib}/GLDODA) OBJTYPE(*DTAARA) OUTPUT(*OUTFILE)"
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPD9861") && e.contains("CPF0006"), "{e}");
    let r = client
        .query_one(
            &format!(
                "SELECT odbpun::text, odaapi, odaspl::text, odlasn::text FROM {}.gldobj",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..4).map(|i| r.get(i)).collect();
    assert_eq!(got, ["512", "1", "1", "1"]);
    source::create_file(&client, &lib, "GLDWSRC", "")
        .await
        .unwrap();
    for m in ["A1", "B2"] {
        job.run(&format!(
            "ADDPFM FILE({lib}/GLDWSRC) MBR({m}) SRCTYPE(CLLE)"
        ))
        .await
        .unwrap();
    }
    // A1 used (copied to a stream file and back), B2 never: the usage
    // fields (verify3 x_mbrl_vals: 1, today, 1 for the member used; blank,
    // blank, 0 for the other).
    let stmf = format!("/tmp/{lib}-a1.txt");
    job.run(&format!(
        "CPYTOSTMF FROMMBR('/QSYS.LIB/{lib}.LIB/GLDWSRC.FILE/A1.MBR') TOSTMF('{stmf}')"
    ))
    .await
    .unwrap();
    job.run(&format!(
        "CPYFRMSTMF FROMSTMF('{stmf}') TOMBR('/QSYS.LIB/{lib}.LIB/GLDWSRC.FILE/A1.MBR') MBROPT(*REPLACE)"
    ))
    .await
    .unwrap();
    // CPYF of a source member (verify3 x_src: printed, no error). A1 has
    // no records here, so the completion message is CPC2957 (PUB400
    // 2026-10-27, verify5 z_cpysplf_2f: no records copied); x_src's member
    // had records (CPC2955).
    match job
        .run(&format!(
            "CPYF FROMFILE({lib}/GLDWSRC) TOFILE(*PRINT) FROMMBR(A1)"
        ))
        .await
        .unwrap()
    {
        Outcome::Display(d) => assert!(d.print[0].starts_with("CPC2957"), "{:?}", d.print),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        ids(job
            .run(&format!(
                "DSPFD FILE({lib}/GLDWSRC) TYPE(*MBRLIST) OUTPUT(*OUTFILE) OUTFILE({lib}/GLDMBR)"
            ))
            .await
            .unwrap()),
        ["CPF9861", "CPF9862", "CPF3030"]
    );
    let today: String = client
        .query_one("SELECT to_char(now() AT TIME ZONE 'UTC', 'YYMMDD')", &[])
        .await
        .unwrap()
        .get(0);
    let rows = client
        .query(
            &format!(
                "SELECT rtrim(mlname), mlucen, mludat, mlucnt::text FROM {}.gldmbr ORDER BY mlname",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<Vec<String>> = rows
        .iter()
        .map(|r| (0..4).map(|i| r.get(i)).collect())
        .collect();
    assert_eq!(
        got,
        [
            vec!["A1".to_string(), "1".into(), today.clone(), "1".into()],
            vec!["B2".to_string(), " ".into(), "      ".into(), "0".into()]
        ]
    );
    match job
        .run(&format!(
            "DSPFD FILE({lib}/GLDTO) TYPE(*MBRLIST) OUTPUT(*OUTFILE) OUTFILE({lib}/GLDMBR) OUTMBR(*FIRST *ADD)"
        ))
        .await
        .unwrap()
    {
        Outcome::Messages(m) => {
            assert_eq!(m.len(), 1, "{m:?}");
            assert!(m[0].starts_with("CPF3030: 1 records"), "{m:?}");
        }
        other => panic!("{other:?}"),
    }
    // The models made at catalog initialization.
    for (file, fields, len) in [
        ("QADSPFFD", 102i64, 1066i64),
        ("QADSPDBR", 16, 372),
        ("QAFDBASI", 48, 199),
        ("QAFDMBR", 155, 906),
        ("QAFDPHY", 98, 3992),
        ("QAFDACCP", 33, 130),
        ("QAFDRFMT", 53, 290),
    ] {
        let r = client
            .query_one(
                "SELECT count(*), coalesce(sum(CASE WHEN data_type = 'P' THEN length / 2 + 1
                                                    WHEN varlen THEN length + 2 ELSE length END), 0)::bigint
                   FROM as4tainr.file_fields WHERE library = 'QSYS' AND file = $1",
                &[&file],
            )
            .await
            .unwrap();
        assert_eq!(
            (r.get::<_, i64>(0), r.get::<_, i64>(1)),
            (fields, len),
            "{file}"
        );
    }
}

/// DSPFFD OUTPUT(*OUTFILE): a QADSPFFD record per field (the model's
/// field descriptions: PUB400 2026-10-26, tools/golden/verify2.txt
/// m_ffd). The values are the printed report's for the same table
/// (verify2 w_hist_ffd, DSPFFD of a table made like gldtmp.sql's: GRP
/// CHAR 10 at 1, SEQ BINARY 9 0 in 4 bytes at 11, CMD a VARCHAR(100) in
/// 102 bytes at 15, AUDIT_USER 130 at 117 and AUDIT_OPER 1 at 247, the
/// auditing columns null-capable); IMPLICITLY HIDDEN columns have
/// WHHDNCOL 1. The file is made with CPF9861 and CPF9862 (outfile.rs).
#[tokio::test]
async fn dspffd_outfile() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let script = format!(
        "create table {lib}.GLDTMP (GRP char(10) not null, SEQ int not null, CMD varchar(100) not null,
           AUDIT_USER varchar(128) generated always as (SESSION_USER),
           AUDIT_OPER char(1) generated always as (data change operation),
           SYS_START timestamp(12) not null generated always as row begin,
           SYS_END timestamp(12) not null generated always as row end,
           TS_ID timestamp(12) not null generated always as transaction start id,
           period system_time (SYS_START, SYS_END));
         alter table {lib}.GLDTMP alter column TS_ID set implicitly hidden;"
    );
    let lines = job
        .run_sql_script(&script, "test", 0, Some(lib.clone()), true)
        .await
        .unwrap();
    assert!(!lines.iter().any(|l| l.contains(": SQL")), "{lines:?}");
    match job
        .run(&format!(
            "DSPFFD FILE({lib}/GLDTMP) OUTPUT(*OUTFILE) OUTFILE({lib}/FFD)"
        ))
        .await
        .unwrap()
    {
        Outcome::Messages(m) => {
            let ids: Vec<&str> = m
                .iter()
                .map(|x| as4tainr_core::jobs::message_id(x))
                .collect();
            // CPF3030 too (PUB400 2026-10-27, verify4.txt y_ffd).
            assert_eq!(ids, ["CPF9861", "CPF9862", "CPF3030"]);
        }
        other => panic!("{other:?}"),
    }
    let rows = client
        .query(
            &format!(
                "SELECT rtrim(whflde) || '|' || whfobo::text || '|' || whfldb::text || '|' || whfldd::text
                        || '|' || whfldp::text || '|' || whfldt || '|' || coalesce(nullif(whvarl, ' '), '_')
                        || '|' || whnull
                        || '|' || whhdncol || '|' || whfldn::text || '|' || rtrim(whname)
                   FROM {}.ffd ORDER BY whfobo",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(
        got[..5],
        [
            "GRP|1|10|0|0|A|N|N|0|8|GLDTMP",
            // Not character: WHVARL blank, shown as _ (verify4 y_ffd_vals).
            "SEQ|11|4|9|0|B|_|N|0|8|GLDTMP",
            "CMD|15|102|0|0|A|Y|N|0|8|GLDTMP",
            "AUDIT_USER|117|130|0|0|A|Y|Y|0|8|GLDTMP",
            "AUDIT_OPER|247|1|0|0|A|N|Y|0|8|GLDTMP",
        ]
    );
    assert!(
        got[7].starts_with("TS_ID|") && got[7].contains("|1|8|"),
        "{got:?}"
    );
    // A non-character field's CCSID: 65535 (verify4 y_ffd_vals).
    let c: String = client
        .query_one(
            &format!(
                "SELECT whcsid::text FROM {}.ffd WHERE whflde = 'SEQ'",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(c, "65535");
}

/// CREATE SCHEMA makes journal QSQJRN and receiver QSQJRN0001 in the
/// schema (Db2 for i SQL Reference, CREATE SCHEMA), and a table SQL makes
/// in a library with QSQJRN is journaled to it with IMAGES(*BOTH) and
/// OMTJRNE(*OPNCLO) (PUB400 2026-10-27, tools/golden/verify4.txt
/// y_to_jrn). RUNSQLSTM's listing then gives CREATE TABLE SQL7950, and
/// SQL7955 for DELETE, SQL7954 for CREATE INDEX, SQL7965 for LABEL ON and
/// COMMENT ON (y_ids); an error that stops a script under COMMIT(*CHG)
/// rolls its changes back, SQL7961 with no record (y_rb4), and a ROLLBACK
/// takes back a table the script made (y_t5).
#[tokio::test]
async fn schema_journal_and_listing_ids() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let sch = format!("S{}", &lib[1..]);
    job.run(&format!("RUNSQL SQL('CREATE SCHEMA {sch}') COMMIT(*NONE)"))
        .await
        .unwrap();
    for (name, ty) in [("QSQJRN", "*JRN"), ("QSQJRN0001", "*JRNRCV")] {
        assert!(
            catalog::object_exists(&client, std::slice::from_ref(&sch), name, ty)
                .await
                .unwrap(),
            "{name}"
        );
    }
    source::create_file(&client, &lib, "QSQLSRC", "")
        .await
        .unwrap();
    let put = |name: &str, text: String| source::Member {
        library: lib.clone(),
        file: "QSQLSRC".into(),
        name: name.into(),
        srctype: "SQL".into(),
        text: String::new(),
        source: text,
        ..Default::default()
    };
    source::put_member(
        &client,
        &put(
            "IDS",
            format!(
                "create table {sch}.T4 (K int not null primary key, V char(10));
insert into {sch}.T4 values (1, 'a');
delete from {sch}.T4 where K = 1;
create index {sch}.T4I on {sch}.T4 (V);
label on table {sch}.T4 is 'golden 4';
"
            ),
        ),
        false,
    )
    .await
    .unwrap();
    job.run(&format!(
        "RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(IDS) COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let l = job.calls.printed.clone();
    for (id, rec) in [
        ("SQL7950", 1),
        ("SQL7956", 2),
        ("SQL7955", 3),
        ("SQL7954", 4),
        ("SQL7965", 5),
    ] {
        assert!(
            l.iter()
                .any(|x| x.starts_with(&format!("{id}   0 {rec:>7}  Position 1 "))),
            "{id}: {l:#?}"
        );
    }
    let j = client
        .query_one(
            "SELECT journal, images, omtjrne FROM as4tainr.journaled_files WHERE library = $1 AND file = 'T4'",
            &[&sch],
        )
        .await
        .unwrap();
    let got: Vec<String> = (0..3).map(|i| j.get(i)).collect();
    assert_eq!(got, ["QSQJRN", "*BOTH", "*OPNCLO"]);
    // COMMIT(*CHG): the error stops the script and its insert is rolled
    // back.
    source::put_member(
        &client,
        &put(
            "RB4",
            format!(
                "insert into {sch}.T4 values (2, 'b');
insert into {sch}.NOSUCHTBL values (1);
"
            ),
        ),
        false,
    )
    .await
    .unwrap();
    let e = job
        .run(&format!("RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(RB4)"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("SQL9010"), "{e}");
    assert!(
        job.calls
            .printed
            .iter()
            .any(|x| x.starts_with("SQL7961   0          ")),
        "{:#?}",
        job.calls.printed
    );
    let n: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM {}.t4", sch.to_ascii_lowercase()),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    // A ROLLBACK takes back a CREATE TABLE of the unit of work too
    // (verify4 y_rb4, y_t5: CPF9801 for the table afterwards).
    source::put_member(
        &client,
        &put(
            "RB5",
            format!("create table {sch}.T5 (K int);\nrollback;\n"),
        ),
        false,
    )
    .await
    .unwrap();
    job.run(&format!("RUNSQLSTM SRCFILE({lib}/QSQLSRC) SRCMBR(RB5)"))
        .await
        .unwrap();
    assert!(
        !catalog::object_exists(&client, std::slice::from_ref(&sch), "T5", "*FILE")
            .await
            .unwrap()
    );
    job.run(&format!("DLTLIB LIB({sch})")).await.unwrap();
}

/// CPYSPLF (CL reference, Copy Spooled File): the spooled file's lines as
/// records of a physical file of one field (CRTPF RCDLEN), replaced or
/// added (MBROPT); CTLCHAR(*FCFC) puts the first-character forms control
/// before each line ('1' starts a page). A job that is not there is
/// CPF3342 (PUB400 2026-10-26, tools/golden/spool-job.txt j_joblog).
#[tokio::test]
async fn cpysplf_copies_lines() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("P{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QGPL".into()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    job.run(&format!("CRTPF FILE({lib}/SPLCPY) RCDLEN(132)"))
        .await
        .unwrap();
    // A spooled file of the job: two pages.
    let printed: Vec<String> = ["first SPLCPY line", "second line", "\x0c", "page two"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    as4tainr_core::spool::print_report(&client, job.id.as_ref().unwrap(), "GLDSPL", "", &printed)
        .await
        .unwrap();
    let spl: String = client
        .query_one(
            "SELECT file FROM as4tainr.spooled_files WHERE job = $1 ORDER BY id DESC LIMIT 1",
            &[&job.id.as_ref().unwrap().number],
        )
        .await
        .unwrap()
        .get(0);
    let copy = |opt: &str| format!("CPYSPLF FILE({spl}) TOFILE({lib}/SPLCPY) SPLNBR(*LAST) {opt}");
    // The completion message is CPF3485 (PUB400 2026-10-27, verify8
    // c8_joblog7).
    match job.run(&copy("")).await.unwrap() {
        Outcome::Messages(m) => assert!(m[0].starts_with("CPF3485: "), "{m:?}"),
        other => panic!("{other:?}"),
    }
    let t = format!("{}.splcpy", lib.to_ascii_lowercase());
    let lines: Vec<String> = client
        .query(&format!("SELECT * FROM {t} ORDER BY rrn"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<_, String>(1))
        .collect();
    assert!(lines.len() > 3, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("SPLCPY")), "{lines:?}");
    // *ADD with *FCFC: the same lines again, each after its control
    // character, the first a '1'.
    job.run(&copy("MBROPT(*ADD) CTLCHAR(*FCFC)")).await.unwrap();
    let n: i64 = client
        .query_one(&format!("SELECT count(*) FROM {t}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n as usize, 2 * lines.len());
    let first: String = client
        .query_one(
            &format!("SELECT * FROM {t} ORDER BY rrn OFFSET $1 LIMIT 1"),
            &[&(lines.len() as i64)],
        )
        .await
        .unwrap()
        .get(1);
    assert!(first.starts_with('1'), "{first:?}");
    let e = job
        .run(&format!(
            "CPYSPLF FILE({spl}) TOFILE({lib}/SPLCPY) JOB(999999/QSECOFR/NOSUCH)"
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPF3342"), "{e}");
    // Into a file of two character fields: the line's bytes fill the
    // record, cut at its length with CPF3370 (PUB400 2026-10-27, verify9
    // d9_spl_two); a zoned field the text cannot go into ends the copy
    // with CPF3485 for what was copied, then CPF9846 (d9_spl_fk).
    for s in [
        format!("CREATE TABLE {lib}/SPLTW (A CHAR(5) NOT NULL, B CHAR(10) NOT NULL)"),
        format!("CREATE TABLE {lib}/SPLZN (N NUMERIC(5, 0) NOT NULL)"),
    ] {
        job.run(&format!("RUNSQL SQL('{s}') COMMIT(*NONE)"))
            .await
            .unwrap();
    }
    let m = match job
        .run(&format!(
            "CPYSPLF FILE({spl}) TOFILE({lib}/SPLTW) SPLNBR(*LAST)"
        ))
        .await
        .unwrap()
    {
        Outcome::Messages(m) => m,
        other => panic!("{other:?}"),
    };
    let ids: Vec<&str> = m.iter().map(|l| &l[..7]).collect();
    assert_eq!(ids, ["CPF3370", "CPF3485"], "{m:?}");
    let (a, b): (String, String) = {
        let r = client
            .query_one(
                &format!(
                    "SELECT a, b FROM {}.spltw ORDER BY rrn LIMIT 1",
                    lib.to_ascii_lowercase()
                ),
                &[],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(
        format!("{a}{b}"),
        format!("{:<15}", &lines[0][..15.min(lines[0].len())])
    );
    let e = job
        .run(&format!(
            "CPYSPLF FILE({spl}) TOFILE({lib}/SPLZN) SPLNBR(*LAST)"
        ))
        .await
        .unwrap_err()
        .to_string();
    let ids: Vec<&str> = e.lines().map(|l| &l[..7]).collect();
    assert_eq!(ids, ["CPF3370", "CPF3485", "CPF9846"], "{e}");
}

/// CRTPF's DDS listing and its DDS errors, CPYF TOFILE(*PRINT) of an
/// empty file (PUB400 2026-10-27, tools/golden/verify5.txt): a file
/// created prints the listing with its expanded source (z_crtpf); a data
/// type that is not valid is CPD7419 at its position in the listing,
/// and the command ends with CPF7311 then CPF7302 (z_crtpf_bad); a file
/// that exists prints the listing too, ending CPF5813 and CPF7302
/// (z_crtpf_dup); copying no records is CPC2957 (z_cpysplf_2f).
#[tokio::test]
async fn crtpf_dds_listing_and_cpyf_print() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let id = as4tainr_core::jobs::start_interactive(
        &client,
        &format!("D{}", &lib[1..]),
        "QSECOFR",
        None,
        std::slice::from_ref(&lib),
    )
    .await
    .unwrap();
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QGPL".into()],
        display: None,
        id: Some(id),
        calls: Default::default(),
    };
    job.run(&format!("CRTSRCPF FILE({lib}/GLD5SRC) RCDLEN(92)"))
        .await
        .unwrap();
    for (m, src) in [
        (
            "GLD5PF",
            "     A          R GLD5R\n     A            CODE           5P 0\n     A            NAME          20A\n     A          K CODE\n",
        ),
        ("GLD5BAD", "     A          R GLD5R\n     A            CODE           5X 0\n"),
    ] {
        as4tainr_core::source::put_member(
            &client,
            &as4tainr_core::source::Member {
                library: lib.clone(),
                file: "GLD5SRC".into(),
                name: m.into(),
                srctype: "PF".into(),
                text: String::new(),
                source: src.into(),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    }
    let ids = |o: Outcome| match o {
        Outcome::Messages(m) => m
            .iter()
            .map(|l| l.split(':').next().unwrap_or("").to_string())
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    job.calls.printed.clear();
    let o = job
        .run(&format!("CRTPF FILE({lib}/GLD5PF) SRCFILE({lib}/GLD5SRC)"))
        .await
        .unwrap();
    assert_eq!(ids(o), ["CPC7301", "CPC7305"]);
    let p = std::mem::take(&mut job.calls.printed);
    assert!(p.iter().any(|l| l.trim() == "Expanded Source"), "{p:#?}");
    assert!(
        p.contains(&"    300                    NAME          20A  B      COLHDG('NAME')                          20            4       4".to_string()),
        "{p:#?}"
    );
    let e = job
        .run(&format!("CRTPF FILE({lib}/GLD5PF) SRCFILE({lib}/GLD5SRC)"))
        .await
        .unwrap_err()
        .to_string();
    let got: Vec<&str> = e.lines().map(|l| &l[..7]).collect();
    assert_eq!(got, ["CPF5813", "CPF7302"]);
    let p = std::mem::take(&mut job.calls.printed);
    assert!(p.iter().any(|l| l.trim() == "Messages"), "{p:#?}");
    assert!(
        p.iter().any(|l| l.starts_with("* CPF5813      40")),
        "{p:#?}"
    );
    let e = job
        .run(&format!("CRTPF FILE({lib}/GLD5BAD) SRCFILE({lib}/GLD5SRC)"))
        .await
        .unwrap_err()
        .to_string();
    let got: Vec<&str> = e.lines().map(|l| &l[..7]).collect();
    assert_eq!(got, ["CPF7311", "CPF7302"]);
    let p = std::mem::take(&mut job.calls.printed);
    assert!(
        p.contains(&"*                                  CPD7419-*".to_string()),
        "{p:#?}"
    );
    assert!(
        p.iter()
            .any(|l| l.starts_with("* CPD7419      30        1")),
        "{p:#?}"
    );
    // OPTION(*NOSRC *NOLIST): nothing printed (z_crtpf_nosrc).
    let _ = job
        .run(&format!(
            "CRTPF FILE({lib}/GLD5PF) SRCFILE({lib}/GLD5SRC) OPTION(*NOSRC *NOLIST)"
        ))
        .await;
    assert!(job.calls.printed.is_empty(), "{:#?}", job.calls.printed);
    // DSPFFD's report in a CL program is spooled file QPDSPFFD, its
    // printer file (verify5 z_cpysplf: CPYSPLF FILE(QPDSPFFD) of a DSPFFD
    // OUTPUT(*PRINT)), which CPYSPLF copies.
    job.run(&format!("CRTPF FILE({lib}/GLD5SP) RCDLEN(133)"))
        .await
        .unwrap();
    job.run(&format!("CRTSRCPF FILE({lib}/QCLSRC)"))
        .await
        .unwrap();
    as4tainr_core::source::put_member(
        &client,
        &as4tainr_core::source::Member {
            library: lib.clone(),
            file: "QCLSRC".into(),
            name: "GLD5CL".into(),
            srctype: "CLLE".into(),
            text: String::new(),
            source: format!(
                "PGM\n  DSPFFD FILE({lib}/GLD5PF) OUTPUT(*PRINT)\n  CPYSPLF FILE(QPDSPFFD) TOFILE({lib}/GLD5SP) SPLNBR(*LAST)\nENDPGM\n"
            ),
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    job.run(&format!("CRTBNDCL PGM({lib}/GLD5CL) SRCFILE({lib}/QCLSRC)"))
        .await
        .unwrap();
    job.run(&format!("CALL PGM({lib}/GLD5CL)")).await.unwrap();
    let n: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM {}.gld5sp", lib.to_ascii_lowercase()),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(n > 10, "{n}");
    match job
        .run(&format!("CPYF FROMFILE({lib}/GLD5PF) TOFILE(*PRINT)"))
        .await
        .unwrap()
    {
        Outcome::Display(d) => {
            assert!(d.print[0].starts_with("CPC2957"), "{:?}", d.print);
            assert!(d
                .print
                .iter()
                .any(|l| l.starts_with(" 0 records copied to member or label *N in file QSYSPRT")));
        }
        other => panic!("{other:?}"),
    }
}

/// DSPDBR and the DSPFD outfiles *BASATR, *MBR, *RCDFMT, *ACCPTH of an
/// SQL table with an index and a view over it (PUB400 2026-10-27,
/// tools/golden/verify8.txt c8_dbr_print, c8_dbr_out, c8_dbr_none,
/// c8_dbr_missing, c8_bas, c8_mbr, c8_fmt, c8_acp and their values).
#[tokio::test]
async fn dspdbr_and_dspfd_outfiles() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for s in [
        format!("CREATE TABLE {lib}/GLD6TB (ID CHAR(10) NOT NULL, VAL CHAR(60) NOT NULL)"),
        format!("CREATE INDEX {lib}/GLD7IX ON {lib}/GLD6TB (ID)"),
        format!("CREATE VIEW {lib}/GLD7VW AS SELECT ID FROM {lib}/GLD6TB"),
    ] {
        job.run(&format!("RUNSQL SQL('{s}') COMMIT(*NONE)"))
            .await
            .unwrap();
    }
    job.run(&format!("CRTPF FILE({lib}/GLD6SP) RCDLEN(133)"))
        .await
        .unwrap();
    let ids = |o: Outcome| match o {
        Outcome::Messages(m) => m
            .iter()
            .map(|l| l.split(':').next().unwrap_or("").to_string())
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    // The printed report's dependents (c8_dbr_print).
    match job
        .run(&format!("DSPDBR FILE({lib}/GLD6TB) OUTPUT(*PRINT)"))
        .await
        .unwrap()
    {
        Outcome::Display(d) => {
            assert!(
                d.print
                    .contains(&format!("       GLD7IX             {lib:<14}Data")),
                "{:#?}",
                d.print
            );
            assert!(d
                .print
                .iter()
                .any(|l| l.ends_with(":                2")
                    && l.contains("Number of dependent files")));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        ids(job
            .run(&format!(
                "DSPDBR FILE({lib}/GLD6TB) OUTPUT(*OUTFILE) OUTFILE({lib}/GLD7DBR)"
            ))
            .await
            .unwrap()),
        ["CPF9861", "CPF9862", "CPF3030"]
    );
    assert_eq!(
        ids(job
            .run(&format!(
                "DSPDBR FILE({lib}/GLD6SP) OUTPUT(*OUTFILE) OUTFILE({lib}/GLD7DBR) OUTMBR(*FIRST *ADD)"
            ))
            .await
            .unwrap()),
        ["CPF3030"]
    );
    let rows: Vec<String> = client
        .query(
            &format!(
                "SELECT rtrim(whrtyp)||'|'||rtrim(whrfi)||'|'||whno::text||'|'||rtrim(whrefi)||'|'||rtrim(whtype)||'|'||rtrim(whrmb)
                   FROM {}.gld7dbr ORDER BY whrfi DESC, whrefi",
                lib.to_ascii_lowercase()
            ),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(
        rows,
        [
            "P|GLD6TB|2|GLD7IX|D|*NONE",
            "P|GLD6TB|2|GLD7VW|D|*NONE",
            "P|GLD6SP|0|||*NONE"
        ]
    );
    let e = job
        .run(&format!("DSPDBR FILE({lib}/NOSUCH) OUTPUT(*PRINT)"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("CPF3012"), "{e}");
    for (ty, file, out) in [
        ("*BASATR", "GLD6TB", "GLD7BAS"),
        ("*MBR", "GLD6TB", "GLD7MBR"),
        ("*RCDFMT", "GLD6TB", "GLD7FMT"),
        ("*ACCPTH", "GLD7IX", "GLD7ACP"),
    ] {
        assert_eq!(
            ids(job
                .run(&format!(
                    "DSPFD FILE({lib}/{file}) TYPE({ty}) OUTPUT(*OUTFILE) OUTFILE({lib}/{out})"
                ))
                .await
                .unwrap()),
            ["CPF9861", "CPF9862", "CPF3030"],
            "{ty}"
        );
    }
    let one = |sql: String| {
        let client = &client;
        async move {
            let r = client.query_one(&sql, &[]).await.unwrap();
            r.get::<_, String>(0)
        }
    };
    let l = lib.to_ascii_lowercase();
    assert_eq!(
        one(format!(
            "SELECT rtrim(atfile)||'|'||atftyp||'|'||rtrim(atfila)||'|'||atdtat||'|'||atwait::text||'|'||atwatr::text||'|'||ataccp||'|'||atsqlt FROM {l}.gld7bas"
        ))
        .await,
        "GLD6TB|P|*PHY|D|30|60|A|T"
    );
    assert_eq!(
        one(format!(
            "SELECT rtrim(mbname)||'|'||mbecen||'|'||rtrim(mbedat)||'|'||mbmxrl::text||'|'||mbnrcd::text||'|'||mbres4 FROM {l}.gld7mbr"
        ))
        .await,
        "GLD6TB|*|NONE|70|0|T"
    );
    assert_eq!(
        one(format!(
            "SELECT rtrim(rfname)||'|'||rffldn::text||'|'||rflen::text||'|'||rfpgrt::text FROM {l}.gld7fmt"
        ))
        .await,
        "GLD6TB|2|70|-1"
    );
    assert_eq!(
        one(format!(
            "SELECT rtrim(apfile)||'|'||apftyp||'|'||apaccp||'|'||rtrim(apbof)||'|'||rtrim(apkeyf)||'|'||apkseq||'|'||apnkyf::text||'|'||apkeyn::text FROM {l}.gld7acp"
        ))
        .await,
        "GLD7IX|L|K|GLD6TB|ID|A|1|1"
    );
}

/// CPYF FMTOPT(*NOCHK) copies the record's bytes (PUB400 2026-10-22,
/// tools/golden/files-and-programs.txt y_fmt_nochk, y_print: CODE 5P 0
/// and NAME 20A into a RCDLEN(30) file print as three blank bytes, then
/// the name), X'00' included: a character field keeps any byte.
#[tokio::test]
async fn cpyf_nochk_copies_record_bytes() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    as4tainr_core::source::create_file(&client, &lib, "QDDSSRC", "")
        .await
        .unwrap();
    as4tainr_core::source::put_member(
        &client,
        &as4tainr_core::source::Member {
            library: lib.clone(),
            file: "QDDSSRC".into(),
            name: "GLDPF".into(),
            srctype: "PF".into(),
            text: String::new(),
            source: "     A          R GLDR\n     A            CODE           5P 0\n     A            NAME          20A\n".into(),
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    for c in [
        format!("CRTPF FILE({lib}/GLDPF) SRCFILE({lib}/QDDSSRC)"),
        format!("RUNSQL SQL('INSERT INTO {lib}/GLDPF VALUES(1, ''Alpha'')') COMMIT(*NONE)"),
        format!("CRTPF FILE({lib}/GLDCPY2) RCDLEN(30)"),
        format!("CPYF FROMFILE({lib}/GLDPF) TOFILE({lib}/GLDCPY2) MBROPT(*ADD) FMTOPT(*NOCHK)"),
    ] {
        job.run(&c).await.unwrap();
    }
    let v: String = client
        .query_one(
            &format!("SELECT gldcpy2 FROM {}.gldcpy2", lib.to_ascii_lowercase()),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    // Packed 1 is X'00001F'; then NAME and the 7 blank bytes of padding.
    let bytes = as4tainr_core::tn5250::ebcdic::encode(&v);
    assert_eq!(&bytes[..3], [0x00, 0x00, 0x1F]);
    assert_eq!(
        as4tainr_core::tn5250::ebcdic::decode(&bytes[3..]),
        format!("{:<27}", "Alpha")
    );
    // Read back through record-level access, the bytes are the same.
    let fr = as4tainr_core::catalog::resolve_file(&client, std::slice::from_ref(&lib), "GLDCPY2")
        .await
        .unwrap();
    let mut f = as4tainr_core::rla::OpenFile::new(fr);
    let r = f.read(&client).await.unwrap();
    let got = r.record.unwrap()["GLDCPY2"].clone();
    assert_eq!(
        as4tainr_core::tn5250::ebcdic::encode(&got)[..3],
        [0x00, 0x00, 0x1F]
    );
}

/// DSPPFM of a data member shows its records as bytes (PUB400
/// 2026-10-27, tools/golden/screens5.txt 08_dsppfm, 09_dsppfm_f10: CODE
/// DEC(5,0) 1 is X'00001F', then NAME's characters).
#[tokio::test]
async fn dsppfm_shows_record_bytes() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for s in [
        format!("CREATE TABLE {lib}/SCR5PF (CODE DEC(5, 0) NOT NULL, NAME CHAR(20) NOT NULL)"),
        format!("INSERT INTO {lib}/SCR5PF VALUES(1, ''Alpha''), (-23, ''Beta'')"),
    ] {
        job.run(&format!("RUNSQL SQL('{s}') COMMIT(*NONE)"))
            .await
            .unwrap();
    }
    let Outcome::Display(d) = job.run(&format!("DSPPFM {lib}/SCR5PF")).await.unwrap() else {
        panic!()
    };
    let m = d.member.expect("a member view");
    assert_eq!(m.reclen, 23);
    assert_eq!(m.records[0].1[..4], [0x00, 0x00, 0x1F, 0xC1]);
    assert_eq!(m.records[1].1[..4], [0x00, 0x02, 0x3D, 0xC2]);
    // The three packed bytes print blank (the screen shows the record
    // from column 2).
    assert_eq!(d.rows[0], "   Alpha");
    assert_eq!(d.heading, "*...+....1....+....2...");
}

/// A source member keeps each record's SRCSEQ and SRCDAT, and DSPPFM
/// shows its records as bytes like a data member's (PUB400 2026-10-28,
/// tools/golden/screens6.txt 06_rows, 07_dsppfm: records inserted with
/// RUNSQL as 1.00/261027 'line one' and 2.50/0 '  line two' show as
/// "000100261027line one" and "000250000000  line two" under a ruler of
/// 78 of the 92 columns). A member with no records shows none (the
/// screen's note: pfmview.rs).
#[tokio::test]
async fn source_member_keeps_sequence_numbers_and_dates() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for c in [
        format!("CRTSRCPF {lib}/SCR6SRC RCDLEN(92)"),
        format!("ADDPFM {lib}/SCR6SRC MBR(M1) SRCTYPE(TXT)"),
        // IBM i's SQL names the source file itself (its first member);
        // here an alias names the member (docs/fidelity-gaps.md).
        format!("RUNSQL SQL('CREATE ALIAS {lib}.SCR6A FOR {lib}.SCR6SRC (M1)') COMMIT(*NONE)"),
    ] {
        job.run(&c).await.unwrap_or_else(|e| panic!("{c}: {e:#}"));
    }
    as4tainr_core::sql::run(
        &client,
        &url,
        std::slice::from_ref(&lib),
        &format!("INSERT INTO {lib}.SCR6A (SRCSEQ, SRCDAT, SRCDTA) VALUES(1.00, 261027, 'line one'), (2.50, 0, '  line two')"),
    )
    .await
    .unwrap();
    let Outcome::Display(d) = job.run(&format!("DSPPFM {lib}/SCR6SRC M1")).await.unwrap() else {
        panic!()
    };
    assert_eq!(d.rows, ["000100261027line one", "000250000000  line two"]);
    let m = d.member.expect("a member view");
    assert_eq!(m.reclen, 92);
    assert_eq!(m.member, "M1");
    let s = as4tainr_core::pfmview::screen(&m, 0, 0, as4tainr_core::pfmview::Mode::Char);
    assert_eq!(s[6], " 000100261027line one");
    // An INSERT without a column list through the alias, and into the
    // source file itself (its first member), with system and SQL naming
    // (PUB400 2026-10-28, tools/golden/verify12.txt n12_ins_nocols,
    // n12_ins_file: records 3 and 4 printed by CPYF as 000300261029three
    // and 000400261030four).
    for st in [
        format!("INSERT INTO {lib}.SCR6A VALUES(3.00, 261029, 'three')"),
        format!("INSERT INTO {lib}/SCR6SRC VALUES(4.00, 261030, 'four')"),
    ] {
        as4tainr_core::sql::run(&client, &url, std::slice::from_ref(&lib), &st)
            .await
            .unwrap_or_else(|e| panic!("{st}: {e:#}"));
    }
    let n = client
        .query_one(
            "SELECT count(*) FROM as4tainr_svc.source_lines($1, 'SCR6SRC', 'M1') WHERE srcseq > 2.5",
            &[&lib],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(n, 2);
    // SQL reads them back as kept.
    let rows = client
        .query(
            "SELECT srcseq::text, srcdat::text, rtrim(srcdta)
               FROM as4tainr_svc.source_lines($1, 'SCR6SRC', 'M1') WHERE srcseq < 3",
            &[&lib],
        )
        .await
        .unwrap();
    let got: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    assert_eq!(
        got,
        [
            (
                "1.00".to_string(),
                "261027".to_string(),
                "line one".to_string()
            ),
            (
                "2.50".to_string(),
                "0".to_string(),
                "  line two".to_string()
            )
        ]
    );
    // CPYF to another member copies them.
    job.run(&format!("ADDPFM {lib}/SCR6SRC MBR(M2)"))
        .await
        .unwrap();
    let o = job
        .run(&format!(
            "CPYF FROMFILE({lib}/SCR6SRC) TOFILE(*PRINT) FROMMBR(M1)"
        ))
        .await;
    let text = format!("{o:?}");
    assert!(text.contains("000250000000  line two"), "{text}");
    // An empty table: the member view has no records.
    job.run(&format!(
        "RUNSQL SQL('CREATE TABLE {lib}/SCR6E (A CHAR(5) NOT NULL)') COMMIT(*NONE)"
    ))
    .await
    .unwrap();
    let Outcome::Display(d) = job.run(&format!("DSPPFM {lib}/SCR6E")).await.unwrap() else {
        panic!()
    };
    let m = d.member.expect("a member view");
    let s = as4tainr_core::pfmview::screen(&m, 0, 0, as4tainr_core::pfmview::Mode::Char);
    assert_eq!(s[7], format!("   {}", as4tainr_core::pfmview::NO_RECORDS));
}

/// verify12's programs (PUB400 2026-10-28, tools/golden/verify12.txt):
/// - RCVMSG and RMVMSG by a message key from another call stack entry
///   (n12_rows: K01 the caller's message received with PGMQ(*SAME), K02
///   RMVMSG by that key removed it, K03 RCVMSG PGMQ(*PRV *) of the key
///   then ends in CPF2410);
/// - ILE RPG: a data structure named as the record format its LIKEREC
///   names (n12_rpgp: RNF3546 20, RNF7030 30 twice, RNF7503 30 twice,
///   RNF7260 30, RNF7066 00, the file and the PSDS RNF7031; the message
///   summary by severity then ID; RNS9308 and RNS9310);
/// - CRTSRCPF MBR: CPC7301 then CPC7305 (n12_src).
#[tokio::test]
async fn verify12_message_keys_likerec_and_crtsrcpf() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let ids = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.split_once(':').map(|x| x.0.to_string()))
            .filter(|i| i.len() == 7)
            .collect()
    };
    macro_rules! sys {
        ($c:expr) => {
            pase_system::run(&mut job, &[$c.to_string()]).await
        };
    }
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    // The capture's sources, in this test's library.
    for (f, text) in [
        (
            "gld12a.clle",
            include_str!("../../tools/golden/verify12/gld12a.clle"),
        ),
        (
            "gld12b.clle",
            include_str!("../../tools/golden/verify12/gld12b.clle"),
        ),
        (
            "gld12p.rpgle",
            include_str!("../../tools/golden/verify12/gld12p.rpgle"),
        ),
    ] {
        let text = text.replace("AS4TRAINR1", &lib);
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    let o = sys!(format!(
        "RUNSQL SQL('CREATE TABLE {lib}/GLD12OUT (ID CHAR(10) NOT NULL, VAL CHAR(200) NOT NULL)') COMMIT(*NONE)"
    ));
    assert_eq!(o.status, 0, "{o:?}");
    for p in ["GLD12B", "GLD12A"] {
        let o = sys!(format!(
            "CRTBNDCL PGM({lib}/{p}) SRCSTMF('{dir}/{}.clle')",
            p.to_ascii_lowercase()
        ));
        assert_eq!(o.status, 0, "{o:?}");
    }
    let o = sys!(format!("CALL PGM({lib}/GLD12A)"));
    assert_eq!(o.status, 0, "{o:?}");
    // Its job log: of GLD12's messages, the one kept and the one to the
    // caller (QCMDEXC) (PUB400 2026-10-28, tools/golden/verify12.txt
    // n12_keys).
    let gld: Vec<(String, String)> = o
        .stdout
        .iter()
        .filter(|l| l.starts_with("*NONE"))
        .map(|l| (l[67..79].trim().to_string(), l[101..113].trim().to_string()))
        .collect();
    assert_eq!(
        gld,
        [
            ("GLD12A".to_string(), "GLD12A".to_string()),
            ("GLD12A".into(), "QCMDEXC".into())
        ],
        "{o:?}"
    );
    let texts: Vec<&str> = o
        .stdout
        .iter()
        .filter_map(|l| l.trim().strip_prefix("Message . . . . :   "))
        .filter(|t| t.starts_with("GLD12"))
        .collect();
    assert_eq!(texts, ["GLD12 kept", "GLD12 to caller"]);
    let rows = client
        .query(
            &format!(
                "SELECT rtrim(id), rtrim(val) FROM {}.{} ORDER BY rrn",
                as4tainr_core::catalog::ident(&lib),
                as4tainr_core::catalog::ident("GLD12OUT")
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<(String, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    assert_eq!(
        got,
        [
            ("K01".to_string(), "GLD12 key test".to_string()),
            ("K02".into(), "removed".into()),
            ("K03".into(), "EXCP CPF2410".into()),
        ]
    );

    let o = sys!(format!(
        "CRTBNDRPG PGM({lib}/GLD12P) SRCSTMF('{dir}/gld12p.rpgle')"
    ));
    assert_eq!(o.status, 255, "{o:?}");
    assert_eq!(ids(&o.stderr), ["RNS9308", "RNS9310"]);
    let msg = |head: &str| o.stdout.iter().filter(|l| l.starts_with(head)).count();
    // RNF3546 under its source line: a marker line with the letter under
    // LIKEREC's argument, then the message with the letter (PUB400
    // 2026-10-28, tools/golden/verify13.txt t13_rpge, the same source).
    let at = o
        .stdout
        .iter()
        .position(|l| l.starts_with("    34   dcl-ds outr likerec(OUTR: *output);"))
        .unwrap_or_else(|| panic!("{o:?}"));
    assert_eq!(o.stdout[at + 1], "======>                      aaaa");
    assert!(
        o.stdout[at + 2].starts_with("*RNF3546 20 a      000031  "),
        "{o:?}"
    );
    for (head, n) in [
        ("*RNF3546 20", 2), // the message and its summary line
        ("*RNF7030 30", 3),
        ("*RNF7503 30", 3),
        ("*RNF7260 30", 2),
        ("*RNF7066 00", 2),
        ("*RNF7031 GLD12OUT          DISK", 1),
        ("*RNF7031 PSDS              DS(46)", 1),
        ("*RNF7031 00      2", 1),
    ] {
        assert_eq!(msg(head), n, "{head}: {o:?}");
    }
    let summary: Vec<&str> = o
        .stdout
        .iter()
        .skip_while(|l| !l.contains("M e s s a g e   S u m m a r y"))
        .take_while(|l| !l.contains("E N D   O F   M E S S A G E"))
        .filter(|l| l.starts_with("*RNF"))
        .map(|l| &l[1..8])
        .collect();
    assert_eq!(
        summary,
        ["RNF7031", "RNF7066", "RNF7086", "RNF3546", "RNF7030", "RNF7260", "RNF7503"],
        "{o:?}"
    );

    let o = sys!(format!("CRTSRCPF FILE({lib}/GLD12SRC) RCDLEN(92) MBR(M1)"));
    assert_eq!(ids(&o.stdout), ["CPC7301", "CPC7305"], "{o:?}");
}

/// SQL UPDATE and DELETE of a source file's records, the file named
/// itself (PUB400 2026-10-28, tools/golden/verify13.txt t13_ins, t13_upd,
/// t13_del, t13_src_print: three records 1.00, 2.00, 3.00 of 261029;
/// UPDATE ... SET SRCDTA = 'TWO', SRCDAT = 261030 WHERE SRCSEQ = 2 and
/// DELETE ... WHERE SRCSEQ = 1 leave 000200261030TWO and
/// 000300261029three). Here a DELETE of two records in one statement
/// too.
#[tokio::test]
async fn sql_update_and_delete_on_a_source_file() {
    use as4tainr_core::commands::{Environment, Job};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    job.run(&format!("CRTSRCPF FILE({lib}/GLD13SRC) RCDLEN(92) MBR(M1)"))
        .await
        .unwrap();
    for st in [
        format!("INSERT INTO {lib}/GLD13SRC VALUES(1.00, 261029, 'one'), (2.00, 261029, 'two'), (3.00, 261029, 'three')"),
        format!("UPDATE {lib}/GLD13SRC SET SRCDTA = 'TWO', SRCDAT = 261030 WHERE SRCSEQ = 2"),
        format!("DELETE FROM {lib}/GLD13SRC WHERE SRCSEQ = 1"),
    ] {
        as4tainr_core::sql::run(&client, &url, std::slice::from_ref(&lib), &st)
            .await
            .unwrap_or_else(|e| panic!("{st}: {e:#}"));
    }
    let lines = || async {
        client
            .query(
                "SELECT srcseq::text || '/' || srcdat::text || '/' || rtrim(srcdta)
                   FROM as4tainr_svc.source_lines($1, 'GLD13SRC', 'M1')",
                &[&lib],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get::<_, String>(0))
            .collect::<Vec<_>>()
    };
    assert_eq!(lines().await, ["2.00/261030/TWO", "3.00/261029/three"]);
    // Two records in one statement: each finds its own line.
    for st in [
        format!("INSERT INTO {lib}/GLD13SRC VALUES(4.00, 0, 'four'), (5.00, 0, 'five')"),
        format!("DELETE FROM {lib}.GLD13SRC WHERE SRCSEQ IN (2, 4)"),
    ] {
        as4tainr_core::sql::run(&client, &url, std::slice::from_ref(&lib), &st)
            .await
            .unwrap_or_else(|e| panic!("{st}: {e:#}"));
    }
    assert_eq!(lines().await, ["3.00/261029/three", "5.00/0/five"]);
}

/// The printed job log of a `system` job whose CL programs send, receive
/// and monitor messages (PUB400 2026-10-28, tools/golden/verify17.txt
/// x17_joblog, the same sources): the CPF9898 completion message GLD17A
/// sent its caller is on standard output first; the job log has the job
/// started message CPF1124, then the messages in the order sent, without
/// the escape message GLD17B sent and RCVMSG RMV(*YES) took, with the
/// CPF9801 escape RCVMSG RMV(*NO) kept; ILE senders and receivers with
/// their module, procedure and statement (source line times 100), system
/// programs (QLICKOBJ, QCMDEXC) with an instruction number instead.
#[tokio::test]
async fn verify17_printed_job_log() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, text) in [
        (
            "gld17a.clle",
            include_str!("../../tools/golden/verify17/gld17a.clle"),
        ),
        (
            "gld17b.clle",
            include_str!("../../tools/golden/verify17/gld17b.clle"),
        ),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.replace("AS4TRAINR1", &lib).as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for p in ["GLD17B", "GLD17A"] {
        let o = pase_system::run(
            &mut job,
            &[format!(
                "CRTBNDCL PGM({lib}/{p}) SRCSTMF('{dir}/{}.clle')",
                p.to_ascii_lowercase()
            )],
        )
        .await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD17A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    assert_eq!(o.stdout[0], "CPF9898: GLD17 comp to caller.", "{o:?}");
    let log = &o.stdout[1..];
    assert!(log[0].contains("Display Job Log"), "{o:?}");
    assert!(log[1].contains(":   QP0ZSPWT        User  . . . . . . :   QUSER"));
    assert!(log[2].ends_with(":   QDFTSVR         Library . . . . . :   QGPL"));
    // Each entry's line as columns: ID, type, severity, from program,
    // library, instruction, to program, library, instruction.
    let cols = |l: &str| -> Vec<String> {
        let at = |a: usize, b: usize| {
            l.get(a - 1..b.min(l.len()))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        vec![
            at(1, 11),
            at(12, 35),
            at(36, 40),
            at(68, 80),
            at(81, 92),
            at(93, 101),
            at(102, 113),
            at(114, 125),
            at(126, 132),
        ]
    };
    let firsts: Vec<Vec<String>> = log
        .iter()
        .filter(|l| !l.starts_with(' ') && !l.starts_with("MSGID"))
        .map(|l| cols(l))
        .collect();
    let l = lib.as_str();
    let want: Vec<Vec<&str>> = vec![
        vec![
            "CPF1124",
            "Information",
            "00",
            "QWTPIIPP",
            "QSYS",
            "04DC",
            "*EXT",
            "",
            "*N",
        ],
        vec![
            "*NONE",
            "Information",
            "",
            "GLD17A",
            l,
            "*STMT",
            "GLD17A",
            l,
            "*STMT",
        ],
        vec![
            "*NONE",
            "Diagnostic",
            "",
            "GLD17A",
            l,
            "*STMT",
            "GLD17A",
            l,
            "*STMT",
        ],
        vec![
            "CPF9898",
            "Completion",
            "40",
            "GLD17A",
            l,
            "*STMT",
            "QCMDEXC",
            "QSYS",
            "012F",
        ],
        vec![
            "CPF9801", "Escape", "40", "QLICKOBJ", "QSYS", "025B", "GLD17A", l, "*STMT",
        ],
        vec![
            "*NONE",
            "Information",
            "",
            "GLD17B",
            l,
            "*STMT",
            "GLD17A",
            l,
            "*STMT",
        ],
        vec![
            "*NONE",
            "Information",
            "",
            "GLD17A",
            l,
            "*STMT",
            "GLD17A",
            l,
            "*STMT",
        ],
    ];
    assert_eq!(firsts, want, "{o:?}");
    let stmts: Vec<&str> = log
        .iter()
        .filter(|l| l.trim_start().starts_with("Statement"))
        .map(|l| l.trim().rsplit(' ').next().unwrap())
        .collect();
    assert_eq!(
        stmts,
        ["700", "700", "800", "800", "900", "1100", "400", "1500", "1900", "1900"],
        "{o:?}"
    );
    let texts: Vec<&str> = log
        .iter()
        .filter_map(|l| l.trim().strip_prefix("Message . . . . :   "))
        .skip(1)
        .collect();
    assert_eq!(
        texts,
        [
            "GLD17 info to self",
            "GLD17 diag to self",
            "GLD17 comp to caller.",
            &format!("Object GLD17NONE was not found in library {lib}."),
            "GLD17B info to caller",
            "GLD17 last",
        ]
    );
    // The sending user (the job's is QUSER) on each entry but CPF1124.
    let users = log
        .iter()
        .filter(|l| l.trim_start().starts_with("From user"))
        .count();
    assert_eq!(users, 6);

    // A `system` command's own job log (a job of its own): only CPF1124,
    // and no message (x17_own).
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &["DSPJOBLOG OUTPUT(*PRINT)".into()]).await;
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout[0].contains("Display Job Log"), "{o:?}");
    assert!(o.stdout[4].starts_with("CPF1124"), "{o:?}");
    assert!(o.stdout[5].trim_start().starts_with("Message . . . . :"));
}

/// Which messages the printed job log keeps (PUB400 2026-10-28,
/// tools/golden/verify18.txt y18_joblog, the same source and message
/// file): every message removed from its queue leaves it, completion
/// messages too (GLD18A's own, received or removed by key, and
/// CRTDTAARA's received with RMV(*YES)); CRTDTAARA's completion message
/// comes from QWCCCRVC (0095), DLTDTAARA's escape CPF2105 (severity 40,
/// monitored, left on the queue) from QLIDLOBJ (0772); a second-level
/// text runs on from column 38 without its format controls; a long text
/// is cut at blanks by column 115.
#[tokio::test]
async fn verify18_removed_messages_leave_the_job_log() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    as4tainr_core::ifs::write_file(
        &client,
        &format!("{dir}/gld18a.clle"),
        include_str!("../../tools/golden/verify18/gld18a.clle")
            .replace("AS4TRAINR1", &lib)
            .as_bytes(),
        819,
        0o644,
        "QSECOFR",
    )
    .await
    .unwrap();
    for c in [
        format!("CRTMSGF MSGF({lib}/GLD18M)"),
        format!("ADDMSGD MSGID(GLD1801) MSGF({lib}/GLD18M) MSG('GLD18 own message &1') SECLVL('&N Cause . . . . . :   GLD18 own cause text. &N Recovery  . . . :   GLD18 own recovery text.') SEV(30) FMT((*CHAR 3))"),
        format!("CRTBNDCL PGM({lib}/GLD18A) SRCSTMF('{dir}/gld18a.clle')"),
    ] {
        let o = pase_system::run(&mut job, &[c]).await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    // The process's QTEMP may still have them from an earlier run of
    // this test with the same process ID.
    for d in ["GLD18R", "GLD18K"] {
        let _ = pase_system::run(&mut job, &[format!("DLTDTAARA DTAARA(QTEMP/{d})")]).await;
    }
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD18A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let log = &o.stdout;
    let col = |l: &str, a: usize, b: usize| {
        l.get(a - 1..b.min(l.len()))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let firsts: Vec<[String; 6]> = log
        .iter()
        .filter(|l| !l.starts_with(' ') && !l.starts_with("MSGID"))
        .map(|l| {
            [
                col(l, 1, 11),
                col(l, 12, 35),
                col(l, 36, 40),
                col(l, 68, 80),
                col(l, 93, 101),
                col(l, 102, 113),
            ]
        })
        .collect();
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
        [a, b, c, d, e, f].map(str::to_string)
    };
    assert_eq!(
        firsts,
        [
            row("CPF1124", "Information", "00", "QWTPIIPP", "04DC", "*EXT"),
            row("CPC0904", "Completion", "00", "QWCCCRVC", "0095", "GLD18A"),
            row("CPF2105", "Escape", "40", "QLIDLOBJ", "0772", "GLD18A"),
            row("GLD1801", "Diagnostic", "30", "GLD18A", "*STMT", "GLD18A"),
            row("*NONE", "Information", "", "GLD18A", "*STMT", "GLD18A"),
            row("*NONE", "Completion", "", "GLD18A", "*STMT", "GLD18A"),
        ],
        "{o:?}"
    );
    let at = log
        .iter()
        .position(|l| l.ends_with("Message . . . . :   GLD18 own message ABC"))
        .unwrap_or_else(|| panic!("{o:?}"));
    let pad = |n: usize, t: &str| format!("{}{t}", " ".repeat(n));
    assert_eq!(
        log[at + 1],
        pad(
            37,
            "Cause . . . . . :   GLD18 own cause text. Recovery  . . . :   GLD18 own"
        )
    );
    assert_eq!(log[at + 2], pad(39, "recovery text."));
    let at = log
        .iter()
        .position(|l| l.contains("Message . . . . :   GLD18 long:"))
        .unwrap();
    assert_eq!(
        log[at..at + 4],
        [
            pad(
                37,
                "Message . . . . :   GLD18 long: one two three four five six seven eight nine"
            ),
            pad(
                39,
                "ten eleven twelve thirteen fourteen fifteen.  After two blanks sixteen"
            ),
            pad(
                39,
                "seventeen eighteen nineteen twenty twenty-one twenty-two twenty-three"
            ),
            pad(39, "twenty-four end."),
        ]
    );
}

/// An RPG program's QMHSNDPM messages in a `system` job's printed job
/// log, and DSPJOB's printed report (PUB400 2026-10-28,
/// tools/golden/verify19.txt b19_joblog, b19_dfna, b19_stsa, b19_runa,
/// b19_pgmstk, the same sources): GLD19P's message to itself has its
/// statement (the source line, 21) on both ends; those to '*' counter 1
/// go to its program entry procedure _QRNP_PEP_GLD19P (statement *N), not
/// to the CL caller; CRTPF's CPC7301 and CPC7305 come from QDDCPF
/// (054C) and QDDCPFM (005B), CLRPFM's from QDBCLRPF (03AE), DLTF's
/// from QLIDLOBJ (0772). DSPJOB's sections have their titles and
/// attribute lines where PUB400 prints them, a `system` job's call stack
/// starts with QP0ZSPWT, and no message is sent.
#[tokio::test]
async fn verify19_rpg_messages_and_dspjob() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, text) in [
        (
            "gld19a.clle",
            include_str!("../../tools/golden/verify19/gld19a.clle"),
        ),
        (
            "gld19p.rpgle",
            include_str!("../../tools/golden/verify19/gld19p.rpgle"),
        ),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.replace("AS4TRAINR1", &lib).as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for c in [
        format!("CRTBNDRPG PGM({lib}/GLD19P) SRCSTMF('{dir}/gld19p.rpgle')"),
        format!("CRTBNDCL PGM({lib}/GLD19A) SRCSTMF('{dir}/gld19a.clle')"),
    ] {
        let o = pase_system::run(&mut job, &[c]).await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD19A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    // Nothing reaches `system` but the job log (b19_joblog).
    assert!(o.stdout[0].contains("Display Job Log"), "{o:?}");
    let log = &o.stdout;
    let col = |l: &str, a: usize, b: usize| {
        l.get(a - 1..b.min(l.len()))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let firsts: Vec<[String; 4]> = log
        .iter()
        .filter(|l| !l.starts_with(' ') && !l.starts_with("MSGID"))
        .map(|l| {
            [
                col(l, 1, 11),
                col(l, 68, 80),
                col(l, 93, 101),
                col(l, 102, 113),
            ]
        })
        .take(8)
        .collect();
    let row = |a: &str, b: &str, c: &str, d: &str| [a, b, c, d].map(str::to_string);
    assert_eq!(
        firsts,
        [
            row("CPF1124", "QWTPIIPP", "04DC", "*EXT"),
            row("*NONE", "GLD19P", "*STMT", "GLD19P"),
            row("*NONE", "GLD19P", "*STMT", "GLD19P"),
            row("*NONE", "GLD19P", "*STMT", "GLD19P"),
            row("*NONE", "GLD19A", "*STMT", "GLD19A"),
            row("CPC7301", "QDDCPF", "054C", "GLD19A"),
            row("CPC7305", "QDDCPFM", "005B", "GLD19A"),
            row("CPC3101", "QDBCLRPF", "03AE", "GLD19A"),
        ],
        "{o:?}"
    );
    let details: Vec<String> = log
        .iter()
        .skip_while(|l| !l.ends_with("GLD19P info to self") && !l.contains("Statement"))
        .filter(|l| {
            l.trim_start().starts_with("Statement") || l.trim_start().starts_with("To procedure")
        })
        .take(6)
        .map(|l| l.trim().rsplit(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(
        details,
        ["21", "GLD19P", "21", "23", "_QRNP_PEP_GLD19P", "*N"],
        "{o:?}"
    );
    assert!(log
        .iter()
        .any(|l| l.contains("QLIDLOBJ     QSYS        0772")));

    // DSPJOB OUTPUT(*PRINT) of a `system` job.
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*DFNA)".into()]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let p = &o.stdout;
    assert!(
        p[0].starts_with(" AS4T001 V0R1M0 261021                      Display Job"),
        "{o:?}"
    );
    assert_eq!(p[2].trim(), "Job Definition Attributes");
    assert_eq!(
        p[3],
        "   Job description  . . . . . . . . . . . . . . :   JOBD         QDFTSVR"
    );
    assert!(p.last().unwrap().contains("E N D  O F  L I S T I N G"));
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*STSA)".into()]).await;
    assert_eq!(o.stdout[2].trim(), "Job Status Attributes");
    assert_eq!(
        o.stdout[3],
        "   Status of job  . . . . . . . . . . . . . . . :   ACTIVE"
    );
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*RUNA)".into()]).await;
    assert_eq!(
        o.stdout[3],
        "   Run priority  . . . . . . . . . . . . . . . . . . . . :   RUNPTY       50"
    );
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*PGMSTK)".into()]).await;
    let pgms: Vec<String> = o
        .stdout
        .iter()
        .filter(|l| l.starts_with("      Q"))
        .map(|l| l[6..17].trim().to_string())
        .collect();
    assert_eq!(
        pgms,
        ["QP0ZSPWT", "QP0ZPCPN", "QP0ZPCPN", "QP2CMD", "QP2CMD", "QP2USER2", "QCMDEXC"],
        "{o:?}"
    );
}

/// Messages from an RPG program another RPG program called (PUB400
/// 2026-10-28, tools/golden/verify20.txt e20_joblog, the same sources):
/// '*' counter 1 from GLD20Q's main procedure goes to its own program
/// entry procedure _QRNP_PEP_GLD20Q (statement *N), *PGMBDY counter 1 to
/// its caller GLD20P at the statement that called it (6). DSPJOB *LIBL,
/// *ACTGRP and *ALL's order of sections (e20_libl, e20_actgrp, e20_all).
#[tokio::test]
async fn verify20_called_rpg_program_messages_and_dspjob() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, text) in [
        (
            "gld20a.clle",
            include_str!("../../tools/golden/verify20/gld20a.clle"),
        ),
        (
            "gld20p.rpgle",
            include_str!("../../tools/golden/verify20/gld20p.rpgle"),
        ),
        (
            "gld20q.rpgle",
            include_str!("../../tools/golden/verify20/gld20q.rpgle"),
        ),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.replace("AS4TRAINR1", &lib).as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for c in [
        format!("CRTBNDRPG PGM({lib}/GLD20Q) SRCSTMF('{dir}/gld20q.rpgle')"),
        format!("CRTBNDRPG PGM({lib}/GLD20P) SRCSTMF('{dir}/gld20p.rpgle')"),
        format!("CRTBNDCL PGM({lib}/GLD20A) SRCSTMF('{dir}/gld20a.clle')"),
    ] {
        let o = pase_system::run(&mut job, &[c]).await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD20A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let detail = |k: &str| -> Vec<String> {
        o.stdout
            .iter()
            .filter(|l| l.trim_start().starts_with(k))
            .map(|l| l.trim().rsplit(' ').next().unwrap().to_string())
            .collect()
    };
    let to: Vec<String> = o
        .stdout
        .iter()
        .filter(|l| l.starts_with("*NONE"))
        .map(|l| l[101..113].trim().to_string())
        .collect();
    assert_eq!(to, ["GLD20Q", "GLD20P"], "{o:?}");
    assert_eq!(detail("To procedure"), ["_QRNP_PEP_GLD20Q", "GLD20P"]);
    assert_eq!(detail("Statement"), ["21", "*N", "23", "6"], "{o:?}");

    job.calls = Default::default();
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*LIBL)".into()]).await;
    assert_eq!(o.status, 0, "{o:?}");
    assert_eq!(o.stdout[2].trim(), "Job Library List");
    assert_eq!(o.stdout[4], "   Library     Type       Device      Text");
    assert!(o.stdout[5].starts_with("   QSYS        SYS "), "{o:?}");
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*ACTGRP)".into()]).await;
    assert_eq!(
        o.stdout[5], " *DFTACTGRP     0000000000000001      Y",
        "{o:?}"
    );
    assert_eq!(o.stdout[6], " *DFTACTGRP     0000000000000002      Y");
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*ALL)".into()]).await;
    let titles: Vec<&str> = o
        .stdout
        .iter()
        .map(|l| l.trim())
        .filter(|l| l.starts_with("Job ") && !l.contains(':'))
        .collect();
    assert_eq!(
        titles,
        [
            "Job Status Attributes",
            "Job Definition Attributes",
            "Job Run Attributes",
            "Job Call Stack",
            "Job Library List",
            "Job Activation Groups"
        ],
        "{o:?}"
    );
}

/// An RPG program's escape message to its own entry procedure, a
/// run-time error MONITOR took, DSPLY and environment variables in a
/// `system` job (PUB400 2026-10-28, tools/golden/verify21.txt
/// h21_joblog, h21_dsply, h21_envvar, the same sources): GLD21Q's CPF9898
/// is in the job log once, to _QRNP_PEP_GLD21Q, and GLD21A's MONMSG gets
/// it as CPF9898; GLD21M's RNX0100 (severity 50) is sent by QRNXIE
/// (QRNXMSG, SignalException, statement 22) to GLD21M at statement 9;
/// GLD21D's DSPLY line is not in `system`'s output; DSPJOB *ENVVAR lists
/// a variable with its value in quotes at column 42.
#[tokio::test]
async fn verify21_escape_to_entry_procedure_rnx_and_dsply() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    for (f, text) in [
        (
            "gld21a.clle",
            include_str!("../../tools/golden/verify21/gld21a.clle"),
        ),
        (
            "gld21q.rpgle",
            include_str!("../../tools/golden/verify21/gld21q.rpgle"),
        ),
        (
            "gld21m.rpgle",
            include_str!("../../tools/golden/verify21/gld21m.rpgle"),
        ),
        (
            "gld21d.rpgle",
            include_str!("../../tools/golden/verify21/gld21d.rpgle"),
        ),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.replace("AS4TRAINR1", &lib).as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for c in [
        format!("CRTBNDRPG PGM({lib}/GLD21Q) SRCSTMF('{dir}/gld21q.rpgle')"),
        format!("CRTBNDRPG PGM({lib}/GLD21M) SRCSTMF('{dir}/gld21m.rpgle')"),
        format!("CRTBNDRPG PGM({lib}/GLD21D) SRCSTMF('{dir}/gld21d.rpgle')"),
        format!("CRTBNDCL PGM({lib}/GLD21A) SRCSTMF('{dir}/gld21a.clle')"),
    ] {
        let o = pase_system::run(&mut job, &[c]).await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD21A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let col = |l: &str, a: usize, b: usize| {
        l.get(a - 1..b.min(l.len()))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let firsts: Vec<[String; 5]> = o
        .stdout
        .iter()
        .filter(|l| !l.starts_with(' ') && !l.starts_with("MSGID"))
        .map(|l| {
            [
                col(l, 1, 11),
                col(l, 36, 40),
                col(l, 68, 80),
                col(l, 93, 101),
                col(l, 102, 113),
            ]
        })
        .collect();
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str| [a, b, c, d, e].map(str::to_string);
    assert_eq!(
        firsts,
        [
            row("CPF1124", "00", "QWTPIIPP", "04DC", "*EXT"),
            row("CPF9898", "40", "GLD21Q", "*STMT", "GLD21Q"),
            row("*NONE", "", "GLD21A", "*STMT", "GLD21A"),
            row("RNX0100", "50", "QRNXIE", "*STMT", "GLD21M"),
        ],
        "{o:?}"
    );
    let detail = |k: &str| -> Vec<String> {
        o.stdout
            .iter()
            .filter(|l| l.trim_start().starts_with(k))
            .map(|l| l.trim().rsplit(' ').next().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        detail("To procedure"),
        ["_QRNP_PEP_GLD21Q", "GLD21A", "GLD21M"]
    );
    assert_eq!(
        detail("From procedure"),
        ["GLD21Q", "GLD21A", "SignalException"]
    );
    assert_eq!(detail("Statement"), ["21", "*N", "900", "900", "22", "9"]);
    assert!(o
        .stdout
        .iter()
        .any(|l| l.ends_with("Message . . . . :   GLD21 caught CPF9898")));

    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD21D)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    assert!(o.stdout.is_empty(), "{o:?}");

    let o = pase_system::run(
        &mut job,
        &["ADDENVVAR ENVVAR(GLD21ENV) VALUE('x21') REPLACE(*YES)".into()],
    )
    .await;
    assert_eq!(o.status, 0, "{o:?}");
    let o = pase_system::run(&mut job, &["DSPJOB OUTPUT(*PRINT) OPTION(*ENVVAR)".into()]).await;
    assert_eq!(o.stdout[2].trim(), "Environment variables", "{o:?}");
    let line = o
        .stdout
        .iter()
        .find(|l| l.starts_with(" GLD21ENV "))
        .unwrap_or_else(|| panic!("{o:?}"));
    assert_eq!(&line[41..46], "'x21'");
    assert_eq!(line.len(), 128);
}

/// QSYS2.JOBLOG_INFO('*') in a `system` job (PUB400 2026-10-28,
/// tools/golden/verify22.txt u22_rows, the same sources): the job start
/// message from QWTPIIPP (04DC) to *EXT (*N), then the messages the
/// printed job log has, with library, program, module, procedure and
/// instruction on both ends: an ILE procedure's statement number (GLD22Q
/// 20, GLD22A 1000 and 1300, QRNXIE 22, GLD22M 9), none for an entry
/// procedure, QCMDEXC's 012F. A completion message RCVMSG RMV(*YES) took
/// is not listed (2026-10-29, tools/golden/verify23.txt n23_rows: of
/// GLD23A's messages only the one kept; the CPC0904 of a CRTDTAARA
/// whose message was received with RMV(*YES) is not there).
#[tokio::test]
async fn verify22_joblog_info_in_a_system_job() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::pase_system;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let dir = format!("/tmp/{lib}");
    let _ = as4tainr_core::ifs::mkdir_all(&client, &dir, "QSECOFR").await;
    let removed = format!(
        "PGM\n  CRTDTAARA  DTAARA(QTEMP/GLD22R) TYPE(*CHAR) TEXT('removed')\n  RCVMSG     MSGTYPE(*COMP) RMV(*YES)\n  CRTDTAARA  DTAARA(QTEMP/GLD22K) TYPE(*CHAR) TEXT('kept')\n  RUNSQL     SQL('INSERT INTO {lib}/GLD22OUT (N, ID) SELECT ORDINAL_POSITION, MESSAGE_ID FROM TABLE(QSYS2.JOBLOG_INFO(''*'')) X WHERE MESSAGE_ID = ''CPC0904''') COMMIT(*NONE)\n  DLTDTAARA  DTAARA(QTEMP/GLD22R)\n  DLTDTAARA  DTAARA(QTEMP/GLD22K)\nENDPGM\n"
    );
    for (f, text) in [
        (
            "gld22a.clle",
            include_str!("../../tools/golden/verify22/gld22a.clle").to_string(),
        ),
        (
            "gld22q.rpgle",
            include_str!("../../tools/golden/verify22/gld22q.rpgle").to_string(),
        ),
        (
            "gld22m.rpgle",
            include_str!("../../tools/golden/verify22/gld22m.rpgle").to_string(),
        ),
        ("gld22v.clle", removed),
    ] {
        as4tainr_core::ifs::write_file(
            &client,
            &format!("{dir}/{f}"),
            text.replace("AS4TRAINR1", &lib).as_bytes(),
            819,
            0o644,
            "QSECOFR",
        )
        .await
        .unwrap();
    }
    for c in [
        format!("RUNSQL SQL('CREATE TABLE {lib}/GLD22OUT (N INT, ID CHAR(7), TY CHAR(13), SEV INT, FP CHAR(10), FL CHAR(10), FM CHAR(10), FPR VARCHAR(40), FI CHAR(8), TP CHAR(10), TL CHAR(10), TM CHAR(10), TPR VARCHAR(40), TI CHAR(8), FU CHAR(10), TX VARCHAR(60))') COMMIT(*NONE)"),
        format!("CRTBNDRPG PGM({lib}/GLD22Q) SRCSTMF('{dir}/gld22q.rpgle')"),
        format!("CRTBNDRPG PGM({lib}/GLD22M) SRCSTMF('{dir}/gld22m.rpgle')"),
        format!("CRTBNDCL PGM({lib}/GLD22A) SRCSTMF('{dir}/gld22a.clle')"),
        format!("CRTBNDCL PGM({lib}/GLD22V) SRCSTMF('{dir}/gld22v.clle')"),
    ] {
        let o = pase_system::run(&mut job, &[c]).await;
        assert_eq!(o.status, 0, "{o:?}");
    }
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD22A)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let table = format!(
        "{}.{}",
        as4tainr_core::catalog::ident(&lib),
        as4tainr_core::catalog::ident("GLD22OUT")
    );
    let rows = client
        .query(
            &format!(
                "SELECT n, rtrim(id), rtrim(ty), coalesce(rtrim(fp), ''), coalesce(rtrim(fl), ''),
                        coalesce(rtrim(fm), ''), coalesce(fpr, ''), coalesce(rtrim(fi), ''),
                        coalesce(rtrim(tp), ''), coalesce(rtrim(tl), ''), coalesce(rtrim(tm), ''),
                        coalesce(tpr, ''), coalesce(rtrim(ti), ''), rtrim(fu)
                   FROM {table} ORDER BY n"
            ),
            &[],
        )
        .await
        .unwrap();
    let got: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut v = vec![r.get::<_, i32>(0).to_string()];
            v.extend((1..14).map(|i| r.get::<_, String>(i)));
            v
        })
        .collect();
    let l = lib.as_str();
    let row = |v: [&str; 14]| v.map(str::to_string).to_vec();
    assert_eq!(
        got,
        [
            row([
                "1",
                "CPF1124",
                "INFORMATIONAL",
                "QWTPIIPP",
                "QSYS",
                "",
                "",
                "04DC",
                "*EXT",
                "",
                "",
                "",
                "*N",
                "QUSER"
            ]),
            row([
                "2",
                "CPF9898",
                "ESCAPE",
                "GLD22Q",
                l,
                "GLD22Q",
                "GLD22Q",
                "20",
                "GLD22Q",
                l,
                "GLD22Q",
                "_QRNP_PEP_GLD22Q",
                "",
                "QSECOFR"
            ]),
            row([
                "3",
                "*NONE",
                "INFORMATIONAL",
                "GLD22A",
                l,
                "GLD22A",
                "GLD22A",
                "1000",
                "GLD22A",
                l,
                "GLD22A",
                "GLD22A",
                "1000",
                "QSECOFR"
            ]),
            row([
                "4",
                "RNX0100",
                "ESCAPE",
                "QRNXIE",
                "QSYS",
                "QRNXMSG",
                "SignalException",
                "22",
                "GLD22M",
                l,
                "GLD22M",
                "GLD22M",
                "9",
                "QSECOFR"
            ]),
            row([
                "5",
                "CPF9898",
                "INFORMATIONAL",
                "GLD22A",
                l,
                "GLD22A",
                "GLD22A",
                "1300",
                "QCMDEXC",
                "QSYS",
                "",
                "",
                "012F",
                "QSECOFR"
            ]),
        ]
    );

    client
        .execute(&format!("DELETE FROM {table}"), &[])
        .await
        .unwrap();
    job.calls = Default::default();
    let o = pase_system::run(&mut job, &[format!("CALL PGM({lib}/GLD22V)")]).await;
    assert_eq!(o.status, 0, "{o:?}");
    let n: i64 = client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}

/// What iMoq (danlong005/IBMiMock, MIT: a mocking framework with its own
/// self-tests) needs of the RPG engine. ILE RPG Reference: %FLOAT of a
/// character expression reads it as a number; a value that does not fit
/// an integer field is error 00103 (status codes); a whole data structure
/// array for a CONST LIKEDS DIM(n) parameter is passed by reference, its
/// elements as they are; Db2 for i SQL Reference, INSERT: `n ROWS
/// VALUES(:host-structure-array)` inserts the first n elements.
#[tokio::test]
async fn rpg_features_found_by_imoq() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    client
        .batch_execute(&format!(
            "CREATE TABLE {lib}.SRCW (srcseq numeric(6,2), srcdat numeric(6,0), srcdta varchar(80))"
        ))
        .await
        .unwrap();
    let programs: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "IMQFLOAT",
            "**FREE
dcl-s t varchar(20) inz('1.5');
dcl-s f8 float(8);
f8 = %float(t);
if f8 = 1.5;
  dsply 'text';
endif;
if %float('2.5E1') = 25;
  dsply 'exponent';
endif;
*inlr = *on;
"
            .to_string(),
            vec!["text", "exponent"],
        ),
        (
            "IMQINT",
            "**FREE
dcl-s i3 int(3);
dcl-s u3 uns(3);
dcl-s h3 int(3);
dcl-s l5 int(5) inz(*loval);
dcl-s hu uns(5) inz(*hival);
monitor;
  i3 = 100 + 100;
  dsply 'no error';
on-error;
  dsply %char(%status);
endmon;
monitor;
  u3 = %uns('300');
  dsply 'no error';
on-error;
  dsply %char(%status);
endmon;
i3 = -128;
u3 = 255;
dsply (%char(i3) + ' ' + %char(u3));
h3 = *hival;
dsply (%char(h3) + ' ' + %char(l5) + ' ' + %char(hu));
*inlr = *on;
"
            .to_string(),
            // *HIVAL and *LOVAL of integer and unsigned fields: the
            // type's largest and smallest values (ILE RPG Reference).
            vec!["103", "103", "-128 255", "127 -32768 65535"],
        ),
        (
            "IMQDSARR",
            "**FREE
ctl-opt dftactgrp(*no);
dcl-ds m_t qualified template;
  parmNo int(10);
  val varchar(100);
end-ds;
dcl-ds arr likeds(m_t) dim(4);
arr(1).parmNo = 1;
arr(2).parmNo = 2;
arr(2).val = 'two';
arr(3).parmNo = 3;
show(arr : 3);
*inlr = *on;
dcl-proc show;
  dcl-pi *n;
    m likeds(m_t) dim(4) const;
    n int(10) const;
  end-pi;
  dcl-s i int(10);
  for i = 1 to n;
    dsply (%char(m(i).parmNo) + m(i).val);
  endfor;
end-proc;
"
            .to_string(),
            vec!["1", "2two", "3"],
        ),
        (
            "IMQROWS",
            format!(
                "**FREE
dcl-ds src qualified dim(5) inz;
  seq packed(6:2);
  dat packed(6:0);
  dta char(80);
end-ds;
dcl-s n int(10) inz(2);
dcl-s c int(10);
src(1).seq = 1;
src(1).dta = 'first';
src(2).seq = 2;
src(2).dta = 'second';
src(3).dta = 'not inserted';
exec sql insert into {lib}.srcw (srcseq, srcdat, srcdta) :n rows values(:src);
dsply %char(sqlcode);
exec sql select count(*) into :c from {lib}.srcw;
dsply %char(c);
*inlr = *on;
"
            ),
            vec!["0", "2"],
        ),
    ];
    for (name, source, want) in programs {
        let r = rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            source,
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{name}: {:?}", r.error);
        let got: Vec<String> = r
            .messages
            .iter()
            .map(|m| m.trim_end().to_string())
            .collect();
        assert_eq!(got, want, "{name}");
    }
    let rows: Vec<String> = client
        .query(
            &format!("SELECT trim(srcdta) FROM {lib}.srcw ORDER BY srcseq"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(rows, ["first", "second"]);
}

/// CHGJOB and RTVJOBA (CL reference: SWS is eight characters 0, 1 or X,
/// X leaving the switch as it is; DATFMT, DATSEP, INQMSGRPY and LOG
/// level, severity and text; RTVJOBA returns them), %SWITCH (CL
/// reference: '1' when the switches match the mask, X matching either),
/// and the RPG external indicators U1-U8 (ILE RPG Reference, External
/// Indicators: set from the job switches when the program starts, the
/// switches set from them when it ends with LR on).
#[tokio::test]
async fn chgjob_rtvjoba_switch_and_external_indicators() {
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    source::create_file(&client, &lib, "QSRC", "")
        .await
        .unwrap();
    let put = |name: &str, srctype: &str, text: &str| source::Member {
        library: lib.clone(),
        file: "QSRC".into(),
        name: name.into(),
        srctype: srctype.into(),
        source: text.into(),
        ..Default::default()
    };
    for m in [
        put(
            "JOBATR",
            "CLLE",
            "PGM
DCL &SWS *CHAR 8
DCL &FMT *CHAR 4
DCL &SEP *CHAR 1
DCL &RPY *CHAR 10
DCL &LVL *CHAR 1
DCL &SEV *DEC (2 0)
DCL &TXT *CHAR 10
CHGJOB SWS(10XXXXX1) DATFMT(*YMD) DATSEP('-') INQMSGRPY(*DFT) LOG(2 10 *SECLVL)
CHGJOB LOG(3 *SAME *MSG)
RTVJOBA SWS(&SWS) DATFMT(&FMT) DATSEP(&SEP) INQMSGRPY(&RPY) LOGLVL(&LVL) LOGSEV(&SEV) LOGTYPE(&TXT)
SNDPGMMSG MSG(&SWS *BCAT &FMT *BCAT &SEP *BCAT &RPY *BCAT &LVL *BCAT %CHAR(&SEV) *BCAT &TXT) TOPGMQ(*EXT)
IF COND(%SWITCH(1XXXXXX1)) THEN(SNDPGMMSG MSG('MATCH') TOPGMQ(*EXT))
IF COND(%SWITCH(0XXXXXXX)) THEN(SNDPGMMSG MSG('NO MATCH') TOPGMQ(*EXT))
CALL JOBSWS
RTVJOBA SWS(&SWS)
SNDPGMMSG MSG(&SWS) TOPGMQ(*EXT)
ENDPGM
",
        ),
        put(
            "JOBSWS",
            "RPGLE",
            "**FREE
if *inu1 and not *inu2 and *inu8;
  dsply 'U1 U8';
endif;
*inu2 = *on;
*inu8 = *off;
*inlr = *on;
",
        ),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTBNDCL PGM({lib}/JOBATR) SRCFILE({lib}/QSRC)"),
        format!("CRTBNDRPG PGM({lib}/JOBSWS) SRCFILE({lib}/QSRC)"),
    ] {
        run_in(&client, &url, &lib, &c)
            .await
            .unwrap_or_else(|e| panic!("{c}: {e:#}"));
    }
    let m = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/JOBATR"))
        .await
        .unwrap();
    let m: Vec<String> = m.iter().map(|x| x.trim_end().to_string()).collect();
    assert_eq!(
        m,
        [
            "10000001 *YMD - *DFT 3 10 *MSG",
            "MATCH",
            "DSPLY  U1 U8",
            "11000000",
        ],
        "{m:?}"
    );
    // Values CHGJOB does not take: diagnostics, then CPF0006.
    let e = run_in(&client, &url, &lib, "CHGJOB SWS(12XXXXXX)")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("CPF0006"), "{e}");
    let e = run_in(&client, &url, &lib, "CHGJOB JOBPTY(10)")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("CPF0006"), "{e}");
}

/// Source and binder commands iMoq's installer and mocks use. CPYSRCF
/// (CL reference, Copy Source File): the member is added to the to-file
/// with the from-member's type and text, MBROPT(*REPLACE) by default,
/// *ADD after the lines there; SRCOPT(*SEQNBR) numbers the lines from
/// SRCSEQ's start by its increment. CLRPFM of a source member leaves it
/// with no records. RTVBNDSRC (CL reference, Retrieve Binder Source): a
/// service program's exports and signature as binder language, which
/// binds a service program with the same signature again. DLTMSGF of a
/// message file that is not there: CPF2105 (CL reference, DLTMSGF error
/// messages). A module's /COPY member in QTEMP: the program still runs
/// in another job (on IBM i the copy is in the object). A procedure
/// interface's EXTPROC('name') is the procedure's exported name (ILE RPG
/// Reference, EXTPROC).
#[tokio::test]
async fn source_and_binder_commands_found_by_imoq() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QTEMP".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["SRCA", "SRCB", "QSRVSRC", "QRPGLESRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, text: &str, src: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        text: text.into(),
        source: src.into(),
        ..Default::default()
    };
    for m in [
        put("SRCA", "M1", "RPGLE", "Three lines", "one\ntwo\nthree\n"),
        put(
            "QRPGLESRC",
            "PRICES",
            "RPGLE",
            "",
            "**FREE
ctl-opt nomain;
dcl-proc mockp1 export;
  dcl-pi *n packed(7:2) extproc('EX_PRICE');
    item char(5) const;
  end-pi;
  return 12.5;
end-proc;
dcl-proc ex_count export;
  dcl-pi *n int(10) end-pi;
  return 3;
end-proc;
",
        ),
        put(
            "QSRVSRC",
            "PRICES",
            "BND",
            "",
            "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('PRICES V1')\n  EXPORT SYMBOL(EX_PRICE)\n  EXPORT SYMBOL(EX_COUNT)\nENDPGMEXP\n",
        ),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    // CPYSRCF: added with type and text; renumbered from 10 by 5.
    cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/SRCA) TOFILE({lib}/SRCB) FROMMBR(M1) SRCOPT(*SEQNBR) SRCSEQ(10 5)"),
    )
    .await
    .unwrap();
    let m = source::find_member(&client, std::slice::from_ref(&lib), "SRCB", "M1")
        .await
        .unwrap();
    assert_eq!(
        (m.srctype.as_str(), m.text.as_str()),
        ("RPGLE", "Three lines")
    );
    let seqdat = source::member_seqdat(&client, &lib, "SRCB", "M1")
        .await
        .unwrap();
    let recs = source::line_records(&m.source, seqdat.as_deref());
    let seqs: Vec<&str> = recs.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(seqs, ["10.00", "15.00", "20.00"]);
    // MBROPT(*ADD): after the lines there.
    cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/SRCA) TOFILE({lib}/SRCB) FROMMBR(M1) MBROPT(*ADD)"),
    )
    .await
    .unwrap();
    let m = source::find_member(&client, std::slice::from_ref(&lib), "SRCB", "M1")
        .await
        .unwrap();
    assert_eq!(m.source.lines().count(), 6);
    // CLRPFM of the source member.
    let r = cl_cmd(&mut job, format!("CLRPFM FILE({lib}/SRCB) MBR(M1)"))
        .await
        .unwrap();
    assert!(r[0].starts_with("CPC3101"), "{r:?}");
    let m = source::find_member(&client, std::slice::from_ref(&lib), "SRCB", "M1")
        .await
        .unwrap();
    assert_eq!(m.source, "");
    // A procedure exported by its interface's EXTPROC name; RTVBNDSRC
    // gives source that binds the same signature again.
    for c in [
        format!("CRTRPGMOD MODULE({lib}/PRICES) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTSRVPGM SRVPGM({lib}/PRICES) MODULE({lib}/PRICES) SRCFILE({lib}/QSRVSRC)"),
        format!("RTVBNDSRC SRVPGM({lib}/PRICES) SRCFILE({lib}/SRCB) SRCMBR(PRICES2)"),
        format!("CRTSRVPGM SRVPGM({lib}/PRICES2) MODULE({lib}/PRICES) SRCFILE({lib}/SRCB)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let sig = |name: &str| {
        let client = &client;
        let lib = lib.clone();
        let name = name.to_string();
        async move {
            let b: String = client
                .query_one(
                    "SELECT binding FROM as4tainr.service_programs WHERE library=$1 AND name=$2",
                    &[&lib, &name],
                )
                .await
                .unwrap()
                .get(0);
            let v: serde_json::Value = serde_json::from_str(&b).unwrap();
            (v["signature"].clone(), v["exports"].clone())
        }
    };
    let (s1, e1) = sig("PRICES").await;
    let (s2, e2) = sig("PRICES2").await;
    assert_eq!(s1, s2);
    assert_eq!(e1, e2);
    assert_eq!(e1, serde_json::json!(["EX_PRICE", "EX_COUNT"]));
    // DLTMSGF of a message file that is not there.
    let e = cl_cmd(&mut job, format!("DLTMSGF MSGF({lib}/NOSUCH)"))
        .await
        .unwrap_err();
    assert!(e.contains("CPF2105"), "{e}");
}

/// More of what iMoq needs. ILE Concepts, procedure pointers: a pointer
/// to a procedure of a program's module calls it from another program or
/// service program, on the module's static storage (one per activation).
/// ILE RPG Reference, OPTIONS(*VARSIZE): the procedure may use all of the
/// storage passed for the parameter, longer than declared (a CL variable
/// passed by CALLPRC). A program whose /COPY member was in its creating
/// job's QTEMP runs in another job (the copy is part of the object). Db2
/// for i SQL Reference, CREATE ALIAS: an alias of a source member takes
/// DELETE. A command's DFT(QTEMP) is the QTEMP of the job that runs it.
#[tokio::test]
async fn pointers_varsize_qtemp_copy_and_aliases_found_by_imoq() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QTEMP".into(), "QGPL".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGLESRC", "QCLSRC", "QCMDSRC", "SRCA"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, src: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        source: src.into(),
        ..Default::default()
    };
    for m in [
        put(
            "QRPGLESRC",
            "CB",
            "RPGLE",
            "**FREE
ctl-opt nomain;
dcl-proc cbcall export;
  dcl-pi *n int(10);
    p pointer(*proc) value;
    n int(10) value;
  end-pi;
  dcl-pr fn int(10) extproc(p);
    x int(10) value;
  end-pr;
  return fn(n);
end-proc;
dcl-proc vsz export;
  dcl-pi *n;
    a char(1) options(*varsize);
  end-pi;
  dcl-s q pointer;
  dcl-s full char(20) based(q);
  q = %addr(a);
  %subst(full : 11 : 10) = 'KLMNOPQRST';
end-proc;
",
        ),
        put(
            "QRPGLESRC",
            "PP",
            "RPGLE",
            "**FREE
ctl-opt dftactgrp(*no) actgrp('PPGRP');
dcl-pr cbcall int(10) extproc('CBCALL');
  p pointer(*proc) value;
  n int(10) value;
end-pr;
dcl-s calls int(10);
dcl-s r int(10);
r = cbcall(%paddr(twice) : 21);
dsply (%char(r) + ' ' + %char(calls));
*inlr = *on;
dcl-proc twice;
  dcl-pi *n int(10);
    n int(10) value;
  end-pi;
  calls += 1;
  return n * 2;
end-proc;
",
        ),
        put(
            "QCLSRC",
            "VSZCL",
            "CLLE",
            "PGM
DCL &A *CHAR 20 VALUE('ABCDEFGHIJ')
CALLPRC PRC('VSZ') PARM((&A))
SNDPGMMSG MSG(&A) TOPGMQ(*EXT)
ENDPGM
",
        ),
        put(
            "QRPGLESRC",
            "USEINC",
            "RPGLE",
            "**FREE
/COPY QTEMP/INC,G
dsply GREETING;
*inlr = *on;
",
        ),
        put(
            "QCMDSRC",
            "TQ",
            "CMD",
            "CMD PROMPT('Test')
PARM KWD(LIB) TYPE(*NAME) LEN(10) DFT(QTEMP)
",
        ),
        put("SRCA", "M1", "TXT", "one\ntwo\nthree\n"),
        put(
            "QCLSRC",
            "MKINC",
            "CLLE",
            &format!(
                "PGM
CRTSRCPF FILE(QTEMP/INC) RCDLEN(112)
CPYFRMSTMF FROMSTMF('/t/{lib}/g.rpgle') TOMBR('/QSYS.LIB/QTEMP.LIB/INC.FILE/G.MBR')
CRTBNDRPG PGM({lib}/USEINC) SRCFILE({lib}/QRPGLESRC)
DLTF FILE(QTEMP/INC)
CALL {lib}/USEINC
ENDPGM
"
            ),
        ),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    source::put_stream_file(
        &client,
        &format!("/t/{lib}/g.rpgle"),
        "**FREE\ndcl-c GREETING 'hello';\n",
    )
    .await
    .unwrap();
    for c in [
        format!("CRTRPGMOD MODULE({lib}/CB) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTSRVPGM SRVPGM({lib}/CB) MODULE({lib}/CB) EXPORT(*ALL)"),
        format!("CRTRPGMOD MODULE({lib}/PP) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTPGM PGM({lib}/PP) MODULE({lib}/PP) BNDSRVPGM({lib}/CB) ACTGRP(PPGRP)"),
        format!("CRTCLMOD MODULE({lib}/VSZCL) SRCFILE({lib}/QCLSRC)"),
        format!("CRTPGM PGM({lib}/VSZCL) MODULE({lib}/VSZCL) BNDSRVPGM({lib}/CB)"),
        format!("CRTBNDCL PGM({lib}/MKINC) SRCFILE({lib}/QCLSRC)"),
        format!("CRTCMD CMD({lib}/TQ) PGM({lib}/PP) SRCFILE({lib}/QCMDSRC)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    // The program's procedure, called from the service program, counted
    // in the program's storage.
    let r = cl_cmd(&mut job, format!("CALL {lib}/PP")).await.unwrap();
    assert_eq!(r, ["DSPLY  42 1"]);
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/VSZCL"))
        .await
        .unwrap();
    assert_eq!(r, ["ABCDEFGHIJKLMNOPQRST"]);
    // Made with a /COPY member in its job's QTEMP, then the member gone
    // (in that job), and in another job: the program has its copy.
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/MKINC"))
        .await
        .unwrap();
    assert_eq!(r.last().map(String::as_str), Some("DSPLY  hello"), "{r:?}");
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/USEINC"))
        .await
        .unwrap();
    assert_eq!(r, ["DSPLY  hello"]);
    // DFT(QTEMP) kept as QTEMP.
    let def: String = client
        .query_one(
            "SELECT definition FROM as4tainr.commands WHERE library=$1 AND name='TQ'",
            &[&lib],
        )
        .await
        .unwrap()
        .get(0);
    assert!(def.contains("\"dft\":\"QTEMP\""), "{def}");
    // DELETE through an alias of a source member.
    for c in [
        format!("RUNSQL SQL('CREATE ALIAS {lib}.A1 FOR {lib}.SRCA (M1)') COMMIT(*NONE)"),
        format!("RUNSQL SQL('DELETE FROM {lib}.A1 WHERE SRCDTA = ''two''') COMMIT(*NONE)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let m = source::find_member(&client, std::slice::from_ref(&lib), "SRCA", "M1")
        .await
        .unwrap();
    assert_eq!(m.source.lines().collect::<Vec<_>>(), ["one", "three"]);
}

/// More of the RPG iMoq's engine and examples use: CLEAR of one element
/// of a data structure array (ILE RPG Reference, CLEAR: each subfield by
/// its type, numeric ones to zero); a data structure array subfield of a
/// data structure array element passed for a CONST LIKEDS DIM parameter;
/// a VARCHAR longer than 65535 (ILE RPG Reference, VARCHAR: a 4-byte
/// length prefix then); %PARMS with an OPTIONS(*NOPASS : *OMIT) parameter
/// not passed, passed as *OMIT and passed (ILE RPG Reference, %PARMS:
/// *OMIT counts as passed).
#[tokio::test]
async fn rpg_engine_features_found_by_imoq_examples() {
    use as4tainr_core::rpg;
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let programs: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "IMQCLR",
            "**FREE
dcl-ds m_t qualified template;
  parmNo int(10);
  val varchar(20);
end-ds;
dcl-ds gv qualified dim(4);
  gen int(10);
  obj char(10);
  nm int(10);
  m likeds(m_t) dim(8);
end-ds;
dcl-s s int(10);
dcl-s g int(10);
for g = 1 to 6;
  s = %rem(g - 1 : 4) + 1;
  clear gv(s);
  gv(s).gen = g;
  gv(s).nm += 1;
  gv(s).m(gv(s).nm).parmNo = g;
endfor;
dsply (%char(gv(2).gen) + ' ' + %char(gv(2).nm) + ' ' + %char(gv(2).m(1).parmNo)
       + ' ' + %char(gv(4).gen) + ' ' + %char(gv(4).nm));
*inlr = *on;
",
            vec!["6 1 6 4 1"],
        ),
        (
            "IMQPATH",
            "**FREE
dcl-ds m_t qualified template;
  parmNo int(10);
  val varchar(20);
end-ds;
dcl-ds gv qualified dim(3);
  nm int(10);
  m likeds(m_t) dim(8);
end-ds;
dcl-s s int(10) inz(3);
gv(s).nm = 3;
gv(s).m(1).parmNo = 1;
gv(s).m(2).parmNo = 2;
gv(s).m(3).parmNo = 3;
gv(s).m(3).val = 'three';
dsply show(gv(s).m : gv(s).nm);
*inlr = *on;
dcl-proc show;
  dcl-pi *n varchar(50);
    m likeds(m_t) dim(8) const;
    n int(10) const;
  end-pi;
  dcl-s i int(10);
  dcl-s o varchar(50);
  for i = 1 to n;
    o += %char(m(i).parmNo) + ' ';
  endfor;
  return o + m(3).val;
end-proc;
",
            vec!["1 2 3 three"],
        ),
        (
            "IMQV4",
            "**FREE
dcl-s big varchar(70000);
dcl-s p pointer;
dcl-s n4 int(10) based(p);
big = 'hello';
p = %addr(big);
dsply (%char(n4) + ' ' + %char(%size(big)));
dsply note('an urgent note');
*inlr = *on;
dcl-proc note;
  dcl-pi *n varchar(40);
    b varchar(70000:4) value;
  end-pi;
  dcl-s q pointer;
  dcl-s len int(10) based(q);
  q = %addr(b);
  return %char(len) + ' ' + b;
end-proc;
",
            vec!["5 70004", "14 an urgent note"],
        ),
        (
            "IMQNOPASS",
            "**FREE
dsply (%char(f(1)) + ' ' + %char(f(1 : *omit)) + ' ' + %char(f(1 : 'x')));
*inlr = *on;
dcl-proc f;
  dcl-pi *n int(10);
    a int(10) const;
    b char(10) const options(*nopass : *omit);
  end-pi;
  return %parms();
end-proc;
",
            vec!["1 2 2"],
        ),
    ];
    for (name, source, want) in programs {
        let r = rpg::call(
            &url,
            vec![lib.clone()],
            "QSECOFR".into(),
            None,
            None,
            None,
            name.into(),
            source.to_string(),
            vec![],
        )
        .await
        .unwrap();
        assert!(r.error.is_none(), "{name}: {:?}", r.error);
        let got: Vec<String> = r
            .messages
            .iter()
            .map(|m| m.trim_end().to_string())
            .collect();
        assert_eq!(got, want, "{name}");
    }
}

/// What iMoq's install and demo need of programs and commands: CRTCMD
/// ALLOW with several values (CL reference, CRTCMD ALLOW: a list of
/// where the command may run) for a command with a RTNVAL(*YES)
/// parameter run in a CL program; QSYS2.BOUND_SRVPGM_INFO (IBM i
/// services: BOUND_SERVICE_PROGRAM_LIBRARY is *LIBL for a service program
/// bound through the library list, else the library named);
/// a program with a linear-main procedure called recursively (ILE RPG
/// Reference: only a cycle-main procedure cannot be called recursively;
/// each call has its own automatic storage); pointers into the storage
/// of programs still active above the one using them; and a CL variable
/// longer than the RPG parameter it is passed for (passed by reference:
/// the program addresses all of the variable).
#[tokio::test]
async fn commands_bindings_and_program_calls_found_by_imoq_demo() {
    use as4tainr_core::commands::{Environment, Job};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QGPL".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    for f in ["QRPGLESRC", "QCLSRC", "QCMDSRC"] {
        source::create_file(&client, &lib, f, "").await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, src: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        source: src.into(),
        ..Default::default()
    };
    let members = [
        put(
            "QCMDSRC",
            "CNT",
            "CMD",
            "CMD PROMPT('Count')
PARM KWD(OBJ) TYPE(*NAME) LEN(10) MIN(1)
PARM KWD(RTNVAL) TYPE(*DEC) LEN(10 0) RTNVAL(*YES) MIN(1)
",
        ),
        put(
            "QCLSRC",
            "CNTC",
            "CLLE",
            "PGM PARM(&OBJ &RTN)
DCL &OBJ *CHAR 10
DCL &RTN *DEC (10 0)
CHGVAR &RTN 7
ENDPGM
",
        ),
        put(
            "QCLSRC",
            "USECNT",
            "CLLE",
            "PGM
DCL &C *DEC (10 0)
CNT OBJ(X) RTNVAL(&C)
IF (&C *EQ 7) THEN(SNDPGMMSG MSG('SEVEN') TOPGMQ(*EXT))
ENDPGM
",
        ),
        put(
            "QRPGLESRC",
            "S1",
            "RPGLE",
            "**FREE
ctl-opt nomain;
dcl-proc one export;
  dcl-pi *n int(10);
  end-pi;
  return 1;
end-proc;
",
        ),
        put(
            "QRPGLESRC",
            "B1",
            "RPGLE",
            "**FREE
dcl-pr one int(10) extproc('ONE');
end-pr;
dsply %char(one());
*inlr = *on;
",
        ),
        put(
            "QRPGLESRC",
            "REC",
            "RPGLE",
            "**FREE
ctl-opt main(main) dftactgrp(*no) actgrp(*caller);
dcl-pr rec extpgm('REC');
  n packed(3:0);
end-pr;
dcl-proc main;
  dcl-pi *n;
    n packed(3:0);
  end-pi;
  dcl-s mine packed(3:0);
  mine = n;
  if n < 3;
    n += 1;
    rec(n);
  endif;
  dsply %char(mine);
end-proc;
",
        ),
        put(
            "QCLSRC",
            "RECDRV",
            "CLLE",
            "PGM
DCL &N *DEC (3 0) VALUE(1)
CALL REC PARM(&N)
ENDPGM
",
        ),
        put(
            "QRPGLESRC",
            "LP1",
            "RPGLE",
            "**FREE
dcl-pr lp2 extpgm('LP2');
  p pointer;
end-pr;
dcl-s v char(10) inz('AAAAAAAAAA');
dcl-s pp pointer;
pp = %addr(v);
lp2(pp);
dsply v;
*inlr = *on;
",
        ),
        put(
            "QRPGLESRC",
            "LP2",
            "RPGLE",
            "**FREE
dcl-pr lp3 extpgm('LP3');
  p pointer;
end-pr;
dcl-pi *n;
  p pointer;
end-pi;
dcl-s t char(10) based(p);
%subst(t : 1 : 3) = 'BBB';
lp3(p);
*inlr = *on;
",
        ),
        put(
            "QRPGLESRC",
            "LP3",
            "RPGLE",
            "**FREE
dcl-pi *n;
  p pointer;
end-pi;
dcl-s t char(10) based(p);
%subst(t : 4 : 3) = 'CCC';
*inlr = *on;
",
        ),
        put(
            "QRPGLESRC",
            "STB",
            "RPGLE",
            "**FREE
dcl-pi *n;
  p1 char(1) options(*nopass);
  p2 char(1) options(*nopass);
end-pi;
dcl-s q pointer;
dcl-s name char(30) based(q);
if %parms() >= 2;
  q = %addr(p2);
  name = 'Grace Hopper';
endif;
return;
",
        ),
        put(
            "QCLSRC",
            "STBCL",
            "CLLE",
            "PGM
DCL &NAME *CHAR 30
CALL STB PARM('X' &NAME)
SNDPGMMSG MSG(&NAME) TOPGMQ(*EXT)
ENDPGM
",
        ),
    ];
    for m in members {
        source::put_member(&client, &m, false).await.unwrap();
    }
    for c in [
        format!("CRTBNDCL PGM({lib}/CNTC) SRCFILE({lib}/QCLSRC)"),
        format!(
            "CRTCMD CMD({lib}/CNT) PGM({lib}/CNTC) SRCFILE({lib}/QCMDSRC) ALLOW(*IPGM *BPGM *IMOD *BMOD)"
        ),
        format!("CRTBNDCL PGM({lib}/USECNT) SRCFILE({lib}/QCLSRC)"),
        format!("CRTRPGMOD MODULE({lib}/S1) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTSRVPGM SRVPGM({lib}/S1) MODULE({lib}/S1) EXPORT(*ALL)"),
        format!("CRTRPGMOD MODULE({lib}/B1) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTPGM PGM({lib}/B1) MODULE({lib}/B1) BNDSRVPGM((*LIBL/S1))"),
        format!("CRTPGM PGM({lib}/B2) MODULE({lib}/B1) BNDSRVPGM(({lib}/S1))"),
        format!("CRTBNDRPG PGM({lib}/REC) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTBNDCL PGM({lib}/RECDRV) SRCFILE({lib}/QCLSRC)"),
        format!("CRTBNDRPG PGM({lib}/LP1) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTBNDRPG PGM({lib}/LP2) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTBNDRPG PGM({lib}/LP3) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTBNDRPG PGM({lib}/STB) SRCFILE({lib}/QRPGLESRC)"),
        format!("CRTBNDCL PGM({lib}/STBCL) SRCFILE({lib}/QCLSRC)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    // Every ALLOW value counts: the command runs in a CL program.
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/USECNT"))
        .await
        .unwrap();
    assert_eq!(r, ["SEVEN"]);
    let rows: Vec<(String, String, String, String)> = client
        .query(
            "SELECT program_name, object_type, bound_service_program_library, bound_service_program
               FROM qsys2.bound_srvpgm_info WHERE program_library = $1 ORDER BY program_name",
            &[&lib],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
    let want = vec![
        (
            "B1".to_string(),
            "*PGM".to_string(),
            "*LIBL".to_string(),
            "S1".to_string(),
        ),
        (
            "B2".to_string(),
            "*PGM".to_string(),
            lib.clone(),
            "S1".to_string(),
        ),
    ];
    assert_eq!(rows, want);
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/RECDRV"))
        .await
        .unwrap();
    assert_eq!(r, ["DSPLY  3", "DSPLY  2", "DSPLY  1"]);
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/LP1"))
        .await
        .unwrap();
    assert_eq!(r, ["DSPLY  BBBCCCAAAA"]);
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/STBCL"))
        .await
        .unwrap();
    assert_eq!(r, ["Grace Hopper"]);
}

/// What PUB400 showed for verify24 (2026-10-29, tools/golden/
/// verify24.txt): CPYSRCF's messages (q24_cpysrcf: CPF4011 from a
/// 92-byte to a 112-byte source file, CPF2889 when the member is added,
/// CPC2955; q24_cpysrcf_add without CPF2889; q24_cpysrcf_nombr CPF2868
/// then CPF2817), CPYF's CPC2955 naming the source member
/// (q24_print_m2), DSPFD TYPE(*MBRLIST)'s printed report (q24_mbrd),
/// RTVBNDSRC's messages and binder source (q24_rtvbndsrc, q24_rtv_again,
/// q24_print_rtv: CPI5D12 when the member is added, CPI5D11, CPC5D01; the
/// comment box; an EXTPROC('Mixed_Case') export as written), CEE9901 for
/// the caller of an ILE CL program that left an escape unmonitored
/// (q24_call) and CL %CHAR of a *DEC with no decimal positions padded to
/// its digits plus one (q24_rows).
#[tokio::test]
async fn verify24_cpysrcf_rtvbndsrc_mbrlist_and_unmonitored_escapes() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QGPL".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let ids = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|l| as4tainr_core::jobs::message_id(l).to_string())
            .filter(|i| !i.is_empty())
            .collect()
    };
    for c in [
        format!("CRTSRCPF FILE({lib}/S92) RCDLEN(92)"),
        format!("CRTSRCPF FILE({lib}/T112) RCDLEN(112)"),
        format!("CRTSRCPF FILE({lib}/QSRC) RCDLEN(112)"),
    ] {
        cl_cmd(&mut job, c).await.unwrap();
    }
    let put = |file: &str, name: &str, srctype: &str, src: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        source: src.into(),
        ..Default::default()
    };
    for m in [
        put("S92", "M1", "TXT", "one\ntwo\nthree\n"),
        put(
            "QSRC",
            "MIX",
            "RPGLE",
            "**FREE
ctl-opt nomain;
dcl-proc mockp1 export;
  dcl-pi *n packed(7:2) extproc('Mixed_Case');
    item char(5) const;
  end-pi;
  return 12.5;
end-proc;
dcl-proc mixcnt export;
  dcl-pi *n int(10) end-pi;
  return 3;
end-proc;
",
        ),
        put(
            "QSRC",
            "MIXB",
            "BND",
            "STRPGMEXP PGMLVL(*CURRENT) SIGNATURE('MIX V1')\n  EXPORT SYMBOL('Mixed_Case')\n  EXPORT SYMBOL(MIXCNT)\nENDPGMEXP\n",
        ),
        put("QSRC", "FAILS", "CLLE", "PGM\nCALL NOSUCH24\nENDPGM\n"),
        put(
            "QSRC",
            "CALLER",
            "CLLE",
            "PGM
DCL &ID *CHAR 7
DCL &S *DEC (2 0) VALUE(10)
DCL &Z *DEC (5 0) VALUE(273)
CALL FAILS
MONMSG MSGID(CPF0001 CEE9901) EXEC(RCVMSG MSGTYPE(*EXCP) RMV(*NO) MSGID(&ID))
SNDPGMMSG MSG('CAUGHT ' *CAT &ID) TOPGMQ(*EXT)
SNDPGMMSG MSG('[' *CAT %CHAR(&S) *CAT '][' *CAT %CHAR(&Z) *CAT ']') TOPGMQ(*EXT)
ENDPGM
",
        ),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    let r = cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/S92) TOFILE({lib}/T112) FROMMBR(M1)"),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPF4011", "CPF2889", "CPC2955"]);
    let r = cl_cmd(
        &mut job,
        format!(
            "CPYSRCF FROMFILE({lib}/S92) TOFILE({lib}/T112) FROMMBR(M1) TOMBR(M2) MBROPT(*ADD)"
        ),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPF4011", "CPF2889", "CPC2955"]);
    let r = cl_cmd(
        &mut job,
        format!(
            "CPYSRCF FROMFILE({lib}/S92) TOFILE({lib}/T112) FROMMBR(M1) TOMBR(M2) MBROPT(*ADD)"
        ),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPF4011", "CPC2955"]);
    let e = cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/S92) TOFILE({lib}/T112) FROMMBR(NOSUCH)"),
    )
    .await
    .unwrap_err();
    // cl_cmd puts the command before the messages.
    let lines: Vec<String> = e
        .split_once("): ")
        .map_or(e.as_str(), |x| x.1)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(ids(&lines), ["CPF2868", "CPF2817"], "{e}");
    let r = match job
        .run(&format!(
            "CPYF FROMFILE({lib}/T112) TOFILE(*PRINT) FROMMBR(M2)"
        ))
        .await
        .unwrap()
    {
        Outcome::Display(d) => d.print,
        other => panic!("{other:?}"),
    };
    assert!(
        r[0].starts_with("CPC2955") && r[0].contains("member M2"),
        "{r:?}"
    );
    // DSPFD TYPE(*MBRLIST) printed: q24_mbrd's columns.
    let print = match job
        .run(&format!(
            "DSPFD FILE({lib}/T112) TYPE(*MBRLIST) OUTPUT(*PRINT)"
        ))
        .await
        .unwrap()
    {
        Outcome::Display(d) => d.print,
        other => panic!("{other:?}"),
    };
    let col = |line: &str, word: &str| line.find(word).map(|i| i + 1);
    let list = print.iter().find(|l| l.contains("Member List")).unwrap();
    assert_eq!(col(list, "Member List"), Some(44));
    assert!(print
        .iter()
        .any(|l| l.contains("Type of file . : Physical *SRC")));
    let m1 = print.iter().find(|l| l.starts_with("  M1 ")).unwrap();
    // Size ends in column 29, the source type starts in 31, records end
    // in 74 and deleted records in 85.
    assert_eq!(&m1[25..29], "8192");
    assert_eq!(&m1[30..33], "TXT");
    assert_eq!(&m1[73..74], "3");
    assert_eq!(&m1[84..85], "0");
    let total = print
        .iter()
        .find(|l| l.starts_with("  Total records  "))
        .unwrap();
    assert_eq!(total.trim_end().len(), 64);
    assert!(total.trim_end().ends_with(" 9"), "{total}");
    // RTVBNDSRC.
    for c in [
        format!("CRTRPGMOD MODULE({lib}/MIX) SRCFILE({lib}/QSRC)"),
        format!("CRTSRVPGM SRVPGM({lib}/MIX) MODULE({lib}/MIX) SRCFILE({lib}/QSRC) SRCMBR(MIXB)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let r = cl_cmd(
        &mut job,
        format!("RTVBNDSRC SRVPGM({lib}/MIX) SRCFILE({lib}/T112) SRCMBR(RTV)"),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPI5D12", "CPI5D11", "CPC5D01"]);
    let r = cl_cmd(
        &mut job,
        format!("RTVBNDSRC SRVPGM({lib}/MIX) SRCFILE({lib}/T112) SRCMBR(RTV) MBROPT(*ADD)"),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPI5D11", "CPC5D01"]);
    let m = source::find_member(&client, std::slice::from_ref(&lib), "T112", "RTV")
        .await
        .unwrap();
    let l: Vec<&str> = m.source.lines().collect();
    assert_eq!(l.len(), 14);
    assert_eq!(l[1], format!("/*{}*/", "*".repeat(66)));
    assert!(
        l[2].starts_with(&format!("/*   *SRVPGM      MIX          {lib:<13}"))
            && l[2].len() == 70
            && l[2].ends_with("*/"),
        "{}",
        l[2]
    );
    assert_eq!(l[4], "  EXPORT SYMBOL(\"Mixed_Case\")");
    assert_eq!(l[5], "  EXPORT SYMBOL(\"MIXCNT\")");
    // CEE9901 for the caller; %CHAR of *DEC (2 0) and (5 0).
    for c in [
        format!("CRTBNDCL PGM({lib}/FAILS) SRCFILE({lib}/QSRC)"),
        format!("CRTBNDCL PGM({lib}/CALLER) SRCFILE({lib}/QSRC)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/CALLER"))
        .await
        .unwrap();
    assert_eq!(r, ["CAUGHT CEE9901", "[10 ][273   ]"]);
}

/// What PUB400 showed for verify25 (2026-10-29, tools/golden/
/// verify25.txt): CL %CHAR of variables padded to a fixed length with no
/// zero before the decimal point (r25_rows: "[12.50    ][-3    ][-.5  ]
/// [42 and 14 blanks][7 and 10 blanks]"); CPYSRCF of the same record
/// length (r25_same: CPF2889, CPC2955) and into a shorter source file
/// (r25_shorter: CPF2889, CPF2973, CPC2955); RTVBNDSRC numbering its
/// records 0000.01, 0000.02... (r25_rtvm_hex); DSPFD TYPE(*MBRLIST) of a
/// data file showing 16384 bytes for a small member (r25_mbrl_data).
#[tokio::test]
async fn verify25_char_padding_cpysrcf_rtvbndsrc_numbers_and_data_mbrlist() {
    use as4tainr_core::commands::{Environment, Job, Outcome};
    use as4tainr_core::source;
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.clone()),
        libl: vec![lib.clone(), "QGPL".into()],
        display: None,
        id: None,
        calls: Default::default(),
    };
    let ids = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|l| as4tainr_core::jobs::message_id(l).to_string())
            .filter(|i| !i.is_empty())
            .collect()
    };
    for c in [
        format!("CRTSRCPF FILE({lib}/S92) RCDLEN(92)"),
        format!("CRTSRCPF FILE({lib}/T112) RCDLEN(112)"),
        format!("CRTSRCPF FILE({lib}/U112) RCDLEN(112)"),
        format!("RUNSQL SQL('CREATE TABLE {lib}/OUT25 (N INT, LINE VARCHAR(200))') COMMIT(*NONE)"),
        format!("RUNSQL SQL('INSERT INTO {lib}/OUT25 VALUES(1, ''a''), (2, ''b''), (3, ''c'')') COMMIT(*NONE)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let put = |file: &str, name: &str, srctype: &str, src: &str| source::Member {
        library: lib.clone(),
        file: file.into(),
        name: name.into(),
        srctype: srctype.into(),
        source: src.into(),
        ..Default::default()
    };
    for m in [
        put("T112", "M1", "TXT", "one\ntwo\nthree\n"),
        put(
            "T112",
            "P",
            "RPGLE",
            "**FREE
ctl-opt nomain;
dcl-proc p25cnt export;
  dcl-pi *n int(10) end-pi;
  return 3;
end-proc;
",
        ),
        put(
            "T112",
            "CHARS",
            "CLLE",
            "PGM
DCL &D72 *DEC (7 2) VALUE(12.5)
DCL &DN *DEC (5 0) VALUE(-3)
DCL &D31 *DEC (3 1) VALUE(-0.5)
DCL &D150 *DEC (15 0) VALUE(42)
DCL &I *INT 4 VALUE(7)
DCL &L *CHAR 200
CHGVAR &L ('[' *CAT %CHAR(&D72) *CAT '][' *CAT %CHAR(&DN) *CAT '][' *CAT +
  %CHAR(&D31) *CAT '][' *CAT %CHAR(&D150) *CAT '][' *CAT %CHAR(&I) *CAT ']')
SNDPGMMSG MSG(&L) TOPGMQ(*EXT)
ENDPGM
",
        ),
    ] {
        source::put_member(&client, &m, false).await.unwrap();
    }
    let r = cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/T112) TOFILE({lib}/U112) FROMMBR(M1)"),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPF2889", "CPC2955"]);
    let r = cl_cmd(
        &mut job,
        format!("CPYSRCF FROMFILE({lib}/T112) TOFILE({lib}/S92) FROMMBR(M1)"),
    )
    .await
    .unwrap();
    assert_eq!(ids(&r), ["CPF2889", "CPF2973", "CPC2955"]);
    for c in [
        format!("CRTRPGMOD MODULE({lib}/P) SRCFILE({lib}/T112)"),
        format!("RTVBNDSRC MODULE({lib}/P) SRCFILE({lib}/U112) SRCMBR(RTVM)"),
        format!("CRTBNDCL PGM({lib}/CHARS) SRCFILE({lib}/T112)"),
    ] {
        cl_cmd(&mut job, c.clone())
            .await
            .unwrap_or_else(|e| panic!("{c}: {e}"));
    }
    let m = source::find_member(&client, std::slice::from_ref(&lib), "U112", "RTVM")
        .await
        .unwrap();
    let sd = source::member_seqdat(&client, &lib, "U112", "RTVM")
        .await
        .unwrap();
    let seqs: Vec<String> = source::line_records(&m.source, sd.as_deref())
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(seqs, ["0.01", "0.02", "0.03", "0.04", "0.05", "0.06"]);
    let print = match job
        .run(&format!(
            "DSPFD FILE({lib}/OUT25) TYPE(*MBRLIST) OUTPUT(*PRINT)"
        ))
        .await
        .unwrap()
    {
        Outcome::Display(d) => d.print,
        other => panic!("{other:?}"),
    };
    let m1 = print.iter().find(|l| l.starts_with("  OUT25 ")).unwrap();
    assert_eq!(&m1[24..29], "16384");
    assert!(print
        .iter()
        .any(|l| l.contains("Type of file . : Physical *DATA")));
    let r = run_in_own_job(&client, &url, &lib, &format!("CALL {lib}/CHARS"))
        .await
        .unwrap();
    assert_eq!(
        r,
        [format!(
            "[12.50    ][-3    ][-.5  ][42{}][7{}]",
            " ".repeat(14),
            " ".repeat(10)
        )]
    );
}
