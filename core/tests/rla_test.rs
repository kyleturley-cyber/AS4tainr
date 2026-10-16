//! Integration tests for the catalog, record-level access and the bridge.
//!
//! Needs a PostgreSQL database. Set IBMISH_TEST_DATABASE_URL, e.g.
//! `postgres://ibmish:ibmish@127.0.0.1/ibmish`. Without it, tests are skipped.
//! Each test creates its own library, so tests can run in parallel.

use ibmish_core::{bridge, catalog, dds, rla};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_postgres::Client;

const CUSTMAST: &str = include_str!("../../samples/qddssrc/CUSTMAST.pf");
const ORDLIN: &str = include_str!("../../samples/qddssrc/ORDLIN.pf");

static SEQ: AtomicU32 = AtomicU32::new(0);

fn db_url() -> Option<String> {
    std::env::var("IBMISH_TEST_DATABASE_URL").ok()
}

macro_rules! require_db {
    () => {
        match db_url() {
            Some(u) => u,
            None => {
                eprintln!("skipped: IBMISH_TEST_DATABASE_URL not set");
                return;
            }
        }
    };
}

/// A fresh library with CUSTMAST and ORDLIN created and loaded.
async fn setup(url: &str) -> (Client, String) {
    let mut client = bridge::connect_db(url).await.unwrap();
    catalog::init(&client).await.unwrap();
    let lib = format!(
        "T{}{:03}",
        std::process::id() % 100000,
        SEQ.fetch_add(1, Ordering::SeqCst)
    );
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {} CASCADE; DELETE FROM ibmish.objects WHERE library='{lib}' OR name='{lib}';
             DELETE FROM ibmish.files WHERE library='{lib}'; DELETE FROM ibmish.file_fields WHERE library='{lib}';
             DELETE FROM ibmish.file_keys WHERE library='{lib}';
             DELETE FROM ibmish.data_areas WHERE library='{lib}';",
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

/// Talk to the bridge the way the JVM does: JSON lines over TCP.
#[tokio::test]
async fn bridge_protocol_round_trip() {
    let url = require_db!();
    let (_client, lib) = setup(&url).await;
    let listener = bridge::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bridge::serve(
        listener,
        bridge::BridgeConfig {
            database_url: url.clone(),
            library_list: vec![lib.clone()],
            display: None,
        },
    ));

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    let requests = [
        json!({"id": 1, "op": "hello"}),
        json!({"id": 2, "op": "metadata", "file": "ORDLIN"}),
        json!({"id": 3, "op": "open", "file": "ORDLIN"}),
        json!({"id": 4, "op": "setll", "handle": 1, "keys": ["1001"]}),
        json!({"id": 5, "op": "reade", "handle": 1}),
        json!({"id": 6, "op": "reade", "handle": 1}),
        json!({"id": 7, "op": "chain", "handle": 9, "keys": ["1"]}),
        json!({"id": 8, "op": "bogus"}),
    ];
    let mut resp = Vec::new();
    for req in requests {
        wr.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        let r: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        resp.push(r);
    }
    server.abort();

    assert_eq!(resp[0]["version"], 3);
    assert_eq!(resp[1]["format"], "ORDLINR");
    assert_eq!(resp[1]["keys"], json!(["ORDNO", "LINNO"]));
    assert_eq!(resp[1]["fields"][4]["decimals"], 2);
    assert_eq!(resp[2]["handle"], 1);
    assert_eq!(resp[3]["equal"], true);
    assert_eq!(resp[4]["record"]["ITEM"], format!("{:<15}", "WIDGET-A"));
    assert_eq!(resp[5]["eof"], true);
    assert_eq!(resp[6]["ok"], false);
    assert_eq!(resp[7]["ok"], false);
    assert_eq!(resp[7]["id"], 8);
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

/// EXFMT over the bridge: values and indicators go to the job's display,
/// the operator's input and response indicators come back.
#[tokio::test]
async fn bridge_exfmt_round_trip_with_a_display() {
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

    // A fake display: checks the request, then "presses F3".
    let (tx, mut rx) = tokio::sync::mpsc::channel::<bridge::DisplayRequest>(1);
    let display = tokio::spawn(async move {
        let req = rx.recv().await.unwrap();
        let shown = req.frame.layers.last().unwrap();
        assert_eq!(shown.format, "PROMPT");
        assert_eq!(shown.values.get("CUSNUM").map(String::as_str), Some("42"));
        assert!(shown.ind[30], "*IN30 is passed to the display");
        assert!(!shown.ind[3]);
        let mut indicators = std::collections::BTreeMap::new();
        indicators.insert(3u8, true);
        indicators.insert(30u8, false);
        req.reply
            .send(Ok(ibmish_core::dspf::Response {
                aid: "F3".into(),
                values: Default::default(),
                indicators,
                changed: Default::default(),
            }))
            .unwrap();
    });

    let listener = bridge::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bridge::serve(
        listener,
        bridge::BridgeConfig {
            database_url: url.clone(),
            library_list: vec![lib.clone()],
            display: Some(tx),
        },
    ));
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    let mut ind = "0".repeat(99);
    ind.replace_range(29..30, "1"); // *IN30
    for req in [
        json!({"id": 1, "op": "dspf", "file": "CUSTINQD"}),
        json!({"id": 2, "op": "exfmt", "file": "CUSTINQD", "format": "PROMPT", "fields": {"CUSNUM": "42"}, "indicators": ind}),
        json!({"id": 3, "op": "exfmt", "file": "CUSTINQD", "format": "NOSUCH", "fields": {}, "indicators": ""}),
    ] {
        wr.write_all(format!("{req}\n").as_bytes()).await.unwrap();
    }
    let mut resp = Vec::new();
    for _ in 0..3 {
        let r: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        resp.push(r);
    }
    display.await.unwrap();
    server.abort();

    assert_eq!(resp[0]["formats"], json!(["PROMPT", "DETAIL"]));
    assert!(resp[0]["source"].as_str().unwrap().contains("ERRMSG"));
    assert_eq!(resp[1]["aid"], "F3");
    assert_eq!(resp[1]["indicators"]["03"], true);
    assert_eq!(resp[1]["indicators"]["30"], false);
    assert_eq!(resp[2]["ok"], false);
}

/// A subfile over the bridge, as the JVM uses it: the records are written
/// on one connection and the control record is shown on another (the
/// runner has one connection for EXFMT, one per display record format).
/// The operator types an option on record 2; READC returns it, then eof.
#[tokio::test]
async fn bridge_subfile_write_exfmt_readc() {
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

    let (tx, mut rx) = tokio::sync::mpsc::channel::<bridge::DisplayRequest>(1);
    let display = tokio::spawn(async move {
        let req = rx.recv().await.unwrap();
        let names: Vec<&str> = req.frame.layers.iter().map(|l| l.format.as_str()).collect();
        assert_eq!(
            names,
            ["FOOTER", "CUSTCTL"],
            "FOOTER stays: CUSTCTL has OVERLAY"
        );
        let sf = req.frame.subfile.as_ref().unwrap();
        assert!(sf.show);
        assert_eq!(sf.rows.iter().map(|r| r.rrn).collect::<Vec<_>>(), [1, 2, 3]);
        let mut changed = std::collections::BTreeMap::new();
        changed.insert(2u16, [("OPT".to_string(), "5".to_string())].into());
        req.reply
            .send(Ok(ibmish_core::dspf::Response {
                aid: "ENTER".into(),
                values: Default::default(),
                indicators: Default::default(),
                changed,
            }))
            .unwrap();
    });

    let listener = bridge::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bridge::serve(
        listener,
        bridge::BridgeConfig {
            database_url: url.clone(),
            library_list: vec![lib.clone()],
            display: Some(tx),
        },
    ));
    async fn conn(
        addr: std::net::SocketAddr,
    ) -> (
        tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
        tokio::net::tcp::OwnedWriteHalf,
    ) {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (rd, wr) = stream.into_split();
        (BufReader::new(rd).lines(), wr)
    }
    async fn call(
        c: &mut (
            tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
            tokio::net::tcp::OwnedWriteHalf,
        ),
        req: Value,
    ) -> Value {
        c.1.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        serde_json::from_str(&c.0.next_line().await.unwrap().unwrap()).unwrap()
    }
    let mut writer = conn(addr).await;
    let mut runner = conn(addr).await;
    let ind = |on: &[usize]| {
        let mut s = vec![b'0'; 99];
        for &i in on {
            s[i - 1] = b'1';
        }
        String::from_utf8(s).unwrap()
    };
    let r = call(&mut writer, json!({"op": "dspwrite", "file": "CUSTWRKD", "format": "CUSTCTL", "fields": {}, "indicators": ind(&[32])})).await;
    assert_eq!(r["ok"], true, "{r}");
    for n in 1..=3 {
        let r = call(&mut writer, json!({"op": "sflwrite", "file": "CUSTWRKD", "format": "CUSTSFL", "rrn": n,
            "fields": {"OPT": "", "CUSNO": n.to_string(), "CUSNAM": format!("Customer {n}"), "CUSCTY": ""}, "indicators": ind(&[])})).await;
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = call(&mut writer, json!({"op": "sflwrite", "file": "CUSTWRKD", "format": "CUSTSFL", "rrn": 2, "fields": {}, "indicators": ind(&[])})).await;
    assert_eq!(r["ok"], false, "record 2 exists already");
    call(&mut writer, json!({"op": "dspwrite", "file": "CUSTWRKD", "format": "FOOTER", "fields": {}, "indicators": ind(&[])})).await;
    let r = call(&mut runner, json!({"op": "exfmt", "file": "CUSTWRKD", "format": "CUSTCTL", "fields": {}, "indicators": ind(&[31, 33])})).await;
    assert_eq!(r["aid"], "ENTER", "{r}");
    display.await.unwrap();
    let r = call(
        &mut writer,
        json!({"op": "sflreadc", "file": "CUSTWRKD", "format": "CUSTSFL"}),
    )
    .await;
    assert_eq!(
        (r["found"].clone(), r["rrn"].clone()),
        (json!(true), json!(2)),
        "{r}"
    );
    assert_eq!(r["record"]["OPT"], "5");
    assert_eq!(r["record"]["CUSNAM"], "Customer 2");
    let r = call(&mut writer, json!({"op": "sflupdate", "file": "CUSTWRKD", "format": "CUSTSFL",
        "fields": {"OPT": "", "CUSNO": "2", "CUSNAM": "Customer 2", "CUSCTY": ""}, "indicators": ind(&[])})).await;
    assert_eq!(r["ok"], true, "{r}");
    let r = call(
        &mut writer,
        json!({"op": "sflreadc", "file": "CUSTWRKD", "format": "CUSTSFL"}),
    )
    .await;
    assert_eq!(r["eof"], true, "{r}");
    let r = call(
        &mut writer,
        json!({"op": "sflchain", "file": "CUSTWRKD", "format": "CUSTSFL", "rrn": 3}),
    )
    .await;
    assert_eq!(r["record"]["CUSNAM"], "Customer 3");
    server.abort();
}

/// Without a display (batch), EXFMT fails with a clear message.
#[tokio::test]
async fn exfmt_without_a_display_fails_clearly() {
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
    let listener = bridge::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bridge::serve(
        listener,
        bridge::BridgeConfig {
            database_url: url.clone(),
            library_list: vec![lib.clone()],
            display: None,
        },
    ));
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    wr.write_all(b"{\"id\":1,\"op\":\"exfmt\",\"file\":\"CUSTINQD\",\"format\":\"PROMPT\"}\n")
        .await
        .unwrap();
    let r: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    server.abort();
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("no display"), "{r}");
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
    use ibmish_core::users;
    let url = require_db!();
    let client = bridge::connect_db(&url).await.unwrap();
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

/// A password stored before ibmish followed QPWDLVL (hashed exactly as
/// typed, mixed case) still signs on, and is stored upper case from then
/// on, so any case works afterwards (QPWDLVL 0). `ibmish rstpwd` resets
/// a password to the profile name, expired.
#[tokio::test]
async fn passwords_from_before_qpwdlvl_still_work() {
    use ibmish_core::users;
    let url = require_db!();
    let client = bridge::connect_db(&url).await.unwrap();
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
        },
    )
    .await
    .unwrap();
    // The old way: the hash of the password exactly as typed.
    client
        .execute(
            "UPDATE ibmish.user_profiles SET password_hash = $2 WHERE name = $1",
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
    use ibmish_core::commands::{Environment, Job, Outcome};
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    let env = Environment {
        database_url: url.clone(),
        runner: String::new(),
        call_timeout: std::time::Duration::from_secs(5),
    };
    let mut job = Job {
        client: &client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec!["QGPL".into(), "QTEMP".into()],
        display: None,
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
    use ibmish_core::sql::{self, Outcome};
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
    assert!(err("select * from \"IBMISH\".user_profiles")
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
    use ibmish_core::commands::{Environment, Job, Outcome};
    let mut c = bridge::connect_db(url).await.unwrap();
    for (name, src) in pgms {
        ibmish_core::clpgm::compile(name, src).unwrap();
        catalog::create_cl_program(&mut c, lib, name, "", src, true)
            .await
            .unwrap();
    }
    let env = Environment {
        database_url: url.to_string(),
        runner: "false".into(),
        call_timeout: std::time::Duration::from_secs(5),
    };
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: None,
        libl: vec![lib.to_string()],
        display: None,
    };
    match job.run(call).await? {
        Outcome::Messages(m) => Ok(m),
        other => panic!("unexpected {other:?}"),
    }
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
        .batch_execute("DROP SCHEMA IF EXISTS clmade CASCADE; DELETE FROM ibmish.objects WHERE library='QSYS' AND name='CLMADE'")
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
) -> anyhow::Result<ibmish_core::commands::Outcome> {
    use ibmish_core::commands::{Environment, Job};
    let env = Environment {
        database_url: String::new(),
        runner: "false".into(),
        call_timeout: std::time::Duration::from_secs(5),
    };
    let mut job = Job {
        client,
        env: &env,
        user: "QSECOFR".into(),
        curlib: Some(lib.to_string()),
        libl: vec![lib.to_string()],
        display: None,
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
    use ibmish_core::commands::Outcome;
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

/// RPG IN *LOCK / OUT through the bridge: while one program holds the
/// data area, CHGDTAARA from another job waits, so a counter is never
/// changed between a program's IN and its OUT (IBM i: IN *LOCK keeps an
/// exclusive lock until OUT, UNLOCK or the end of the program).
#[tokio::test]
async fn bridge_data_area_in_lock_out() {
    let url = require_db!();
    let (client, lib) = setup(&url).await;
    ibmish_core::dtaara::create(
        &client,
        &lib,
        "ORDNXT",
        ibmish_core::dtaara::Type::Dec,
        7,
        0,
        "1004",
        "",
    )
    .await
    .unwrap();
    let listener = bridge::bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(bridge::serve(
        listener,
        bridge::BridgeConfig {
            database_url: url.clone(),
            library_list: vec![lib.clone()],
            display: None,
        },
    ));
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    let mut ask = async |req: Value| -> Value {
        wr.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap()
    };
    let r = ask(json!({"id": 1, "op": "dtaara_in", "name": "ORDNXT", "lock": true})).await;
    assert_eq!(r["value"], "1004");
    assert_eq!(r["type"], "*DEC");

    // Another job changes the data area: it must wait for OUT.
    let other = bridge::connect_db(&url).await.unwrap();
    let libl = vec![lib.clone()];
    let lib2 = lib.clone();
    let change = tokio::spawn(async move {
        ibmish_core::dtaara::change(&other, "ORDNXT", &libl, &lib2, None, "5000")
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!change.is_finished(), "CHGDTAARA did not wait for the lock");

    let r = ask(json!({"id": 2, "op": "dtaara_out", "name": "ORDNXT", "value": "1005"})).await;
    assert_eq!(r["ok"], true, "{r}");
    let after = change.await.unwrap();
    assert_eq!(after.value, "5000");

    // Without a lock, IN just reads; a value that does not fit is refused.
    let r = ask(json!({"id": 3, "op": "dtaara_in", "name": "ORDNXT"})).await;
    assert_eq!(r["value"], "5000");
    let r = ask(json!({"id": 4, "op": "dtaara_out", "name": "ORDNXT", "value": "123456789"})).await;
    assert_eq!(r["ok"], false);
    let r = ask(json!({"id": 5, "op": "dtaara_in", "name": "NOSUCH"})).await;
    assert!(r["error"].as_str().unwrap().starts_with("CPF1015"), "{r}");
    server.abort();
}
