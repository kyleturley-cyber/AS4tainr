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
             DELETE FROM ibmish.file_keys WHERE library='{lib}';",
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

    assert_eq!(resp[0]["version"], 1);
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
