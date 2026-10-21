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
            format!("Message file APPMSG created in library {lib}."),
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
    assert!(
        e.starts_with("CPF9801: Object NOSUCH in library *LIBL not found."),
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
            format!("Member CUSTMAST of file CUSTMAST in library {lib2} cleared."),
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
            as4tainr_core::sql::run_as(client, &url, &libl, "QSECOFR", Some(lib), &s).await
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
    assert_eq!(
        m,
        ["HELLOfghijklmnopqrst|42|1", "From RPG|84", "DSPLY  done 30"]
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
    assert_eq!(m, ["Library CLMADE created.", "found CLMADE"]);
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
    assert_eq!(m, [format!("Data area NEXTNO created in library {lib}.")]);
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
    assert_eq!(d.rows, ["     0   'Acme Ltd  '"]);
    assert!(d.info.contains(&("Type".into(), "*CHAR".into())));
    run(format!("CRTDTAARA {lib}/DFT TYPE(*DEC)"))
        .await
        .unwrap();
    let Outcome::Display(d) = run("DSPDTAARA DFT".into()).await.unwrap() else {
        panic!()
    };
    assert!(d.info.contains(&("Length".into(), "15 5".into())));
    assert_eq!(d.rows, ["     0   0.00000"]);
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
    assert_eq!(m, [format!("Data area CO deleted from library {lib}.")]);
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
        m.contains("/QSECOFR/MAKEDTA submitted to job queue QBATCH"),
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
        .any(|e| e.text == format!("Data area D1 created in library {lib}.")));
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
    assert!(log.iter().any(|e| e.msgid == "CPF1015"), "{log:?}");

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
    let wait = std::time::Duration::from_millis(700);
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
    assert_eq!(
        d.rows,
        [
            "Title",
            "",
            "Body",
            "---------------- Page 2 ----------------",
            "Page two"
        ]
    );
    job.run("DLTSPLF QSYSPRT SPLNBR(1)").await.unwrap();
    assert!(spool::find(&client, "QSYSPRT", id.number, Some(1))
        .await
        .is_err());

    // DSPJOBLOG OUTPUT(*PRINT).
    job.run("DSPJOBLOG OUTPUT(*PRINT)").await.unwrap();
    assert!(spool::find(&client, "QPJOBLOG", id.number, None)
        .await
        .is_ok());

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
    assert!(log[0].iter().any(|l| l.contains("CPF1015")), "{log:?}");
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
    assert_eq!(m, format!("Program CONTCNT created in library {lib}."));
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
    let Outcome::Display(d) = job.run("DSPFD QRPGLESRC").await.unwrap() else {
        panic!()
    };
    assert_eq!(d.rows.len(), 2);
    let Outcome::Display(d) = job.run("DSPPFM QDDSSRC MBR(CONTACT)").await.unwrap() else {
        panic!()
    };
    assert!(d.rows[0].starts_with("   1.00 "), "{:?}", d.rows);
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
            "Library RPGLIB1 created.",
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
/// that a logical file is built over (CPF3203); RUNSQLSTM with ERRLVL(10)
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
    assert!(e.starts_with("CPF3203"), "{e}");
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
    assert_eq!(
        m.last().unwrap(),
        &format!("3 statements run from member DROPS in {lib}/QSQLSRC; 2 with errors.")
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
}

/// Run a CL command; its messages, or the error.
async fn cl_cmd(
    job: &mut as4tainr_core::commands::Job<'_>,
    c: String,
) -> Result<Vec<String>, String> {
    use as4tainr_core::commands::Outcome;
    match job.run(&c).await {
        Ok(Outcome::Messages(m)) => Ok(m),
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
    assert_eq!(m, [format!("Command HELLO created in library {lib}.")]);
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
    assert_eq!(m, [format!("File CUSTRPT created in library {lib}.")]);
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
