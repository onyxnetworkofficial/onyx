//! Successor links end to end (ADR-0059 amendment 6): `onyx-publish` builds,
//! signs and uploads every catalog and links version to an in-process node
//! over HTTP; the node serves the 410 with its successor and the successor
//! answer; the reader's check (`onyx_api::bulk_successor`) follows it. A
//! fake chain node stands for the book (anchoring is not this test's: the
//! fake records each signed catalog as posted and final).

use super::links::tests::T;
use super::{catalog_sign_cmd, catalog_upload_cmd, now_unix, parse_opts, upload_cmd};
use onyx_api::bulk_serve::{peer_routes, routes, upload_routes, BulkBook, BulkLimits, BulkStore};
use onyx_api::bulk_successor::{check_successor, SuccessorOpts};
use onyx_api::roots::JsonRpc;
use onyx_api::storage::{HttpGet, HyperGet};
use onyx_api::x402::BoxFuture;
use onyx_core::{keccak256, Hash32, SignedReport};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOOK: [u8; 20] = [0xB0; 20];
const CHAIN: u64 = 31_337;
/// A made-up test key (never a real one).
const SECRET: [u8; 32] = [0x42; 32];
const FIN: [u8; 32] = [0xF1; 32];

/// The signed book at its finalized block: recorded reports, accepted
/// facts, registered keys and `sequenceOf`.
#[derive(Default)]
struct Book {
    reports: BTreeMap<Hash32, (Hash32, Hash32, Hash32, Hash32)>,
    facts: BTreeMap<Hash32, Hash32>,
    keys: BTreeMap<Hash32, [u8; 20]>,
    seqs: BTreeMap<(Hash32, u64), Hash32>,
}

#[derive(Default)]
struct FakeChain(Mutex<Book>);

fn word(b: &[u8]) -> Vec<u8> {
    let mut x = vec![0u8; 32 - b.len()];
    x.extend_from_slice(b);
    x
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

impl FakeChain {
    /// `r` posted, recorded and accepted, its key registered.
    fn anchor(&self, r: &SignedReport) {
        let mut b = self.0.lock().expect("lock");
        let fact = keccak256(&r.payload.digest().0);
        let value = keccak256(&borsh::to_vec(&r.payload.value).expect("value"));
        b.reports
            .insert(r.payload.digest(), (r.payload.key, fact, value, r.hash()));
        b.facts.insert(fact, r.hash());
        b.seqs
            .insert((r.payload.key, r.payload.sequence), r.payload.digest());
        b.keys.insert(
            r.payload.key,
            onyx_bulk::anchor::address_of(&SECRET).expect("key"),
        );
    }

    fn eth_call(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let b = self.0.lock().map_err(|_| "lock")?;
        let arg = Hash32(
            data.get(4..36)
                .ok_or("arg")?
                .try_into()
                .map_err(|_| "arg")?,
        );
        let sel = &data[..4];
        let mut out = Vec::new();
        if sel == selector("report(bytes32)") {
            let z = Hash32::ZERO;
            let (k, f, v, h) = b.reports.get(&arg).copied().unwrap_or((z, z, z, z));
            for x in [k, f, v, h] {
                out.extend(word(&x.0));
            }
            let at = word(&1u64.to_be_bytes());
            out.extend(at.clone());
            out.extend(word(&[0]));
            out.extend(at);
        } else if sel == selector("factView(bytes32)") {
            match b.facts.get(&arg) {
                Some(h) => {
                    out.extend(word(&[1]));
                    out.extend(word(&h.0));
                }
                None => {
                    out.extend(word(&[0]));
                    out.extend(word(&[0]));
                }
            }
            out.extend(word(&1u64.to_be_bytes()));
            out.extend(word(&[0x80]));
            out.extend(word(&[0]));
        } else if sel == selector("sequenceOf(bytes32,uint64)") {
            let s: [u8; 8] = data
                .get(60..68)
                .ok_or("seq")?
                .try_into()
                .map_err(|_| "seq")?;
            let d = b
                .seqs
                .get(&(arg, u64::from_be_bytes(s)))
                .copied()
                .unwrap_or(Hash32::ZERO);
            out.extend(word(&d.0));
        } else if sel == selector("keyState(bytes32)") {
            let signer = b.keys.get(&arg);
            out.extend(word(&[u8::from(signer.is_some())]));
            out.extend(word(&[0]));
            out.extend(word(signer.map_or(&[0u8; 20][..], |s| &s[..])));
            out.extend(word(&[0]));
            out.extend(word(&[0]));
            out.extend(word(&[0]));
        } else {
            return Err("unknown call".into());
        }
        Ok(out)
    }
}

impl JsonRpc for FakeChain {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let hx = |b: &[u8]| format!("0x{}", hex::encode(b));
        let r = match method {
            "eth_chainId" => Ok(json!(format!("0x{CHAIN:x}"))),
            "eth_getBlockByNumber" => Ok(json!({
                "number": "0x64", "hash": hx(&FIN), "timestamp": format!("0x{:x}", now_unix()),
            })),
            "eth_call" => {
                let data = p[0]["data"]
                    .as_str()
                    .and_then(|d| hex::decode(d.trim_start_matches("0x")).ok())
                    .unwrap_or_default();
                self.eth_call(&data).map(|o| json!(hx(&o)))
            }
            m => Err(format!("unexpected {m}")),
        };
        Box::pin(async move { r })
    }
}

fn opts(a: &[&str]) -> super::Opts {
    let v: Vec<String> = a.iter().map(|s| s.to_string()).collect();
    parse_opts(&v).expect("opts")
}

/// Sign catalog `n` of `t` with the test key, record it in the book, upload
/// it, then upload each version folder of `dirs` with `--catalog n`.
async fn publish(t: &T, chain: &FakeChain, base: &str, n: u64, dirs: &[&str]) {
    let key = t.p("publisher.key");
    let cat = t.p(&format!("c{n}.bin"));
    catalog_sign_cmd(&opts(&["--key", &key, "--catalog", &cat])).expect("signed");
    let r: SignedReport = borsh::BorshDeserialize::try_from_slice(
        &std::fs::read(format!("{cat}.report")).expect("report"),
    )
    .expect("report");
    chain.anchor(&r);
    catalog_upload_cmd(&opts(&["--catalog", &cat, "--node", base]))
        .await
        .expect("catalog uploaded");
    for d in dirs {
        upload_cmd(&opts(&[
            "--dir",
            &t.p(d),
            "--node",
            base,
            "--catalog",
            &n.to_string(),
        ]))
        .await
        .expect("version uploaded");
    }
}

/// The 410 of `synthetic.a`, once its `successor` is `status` at `last`
/// (the node builds its links index in the background; a cap far above
/// any run).
async fn gone_until(get: &HyperGet, base: &str, status: &str, last: &str) -> Value {
    let url = format!("{base}/v1/bulk/example.publisher/synthetic.a");
    let wait = async {
        loop {
            let (s, b) = get.get(&url, 1 << 20).await.expect("node");
            let v: Value = serde_json::from_slice(&b).expect("json");
            if s == 410 && v["successor"]["status"] == status && v["successor"]["final"] == last {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .expect("the 410 names the successor")
}

/// The reader's check of the node's successor answer for `synthetic.a`.
async fn reader(get: &HyperGet, chain: &FakeChain, base: &str) -> (i32, Value) {
    let url = format!("{base}/v1/bulk/example.publisher/synthetic.a/successor");
    let (s, b) = get.get(&url, 16 << 20).await.expect("node");
    assert_eq!(s, 200);
    let answer: Value = serde_json::from_slice(&b).expect("json");
    let o = check_successor(
        get,
        chain,
        CHAIN,
        BOOK,
        &answer,
        "example.publisher",
        "synthetic.a",
        &SuccessorOpts {
            node: Some(base.to_string()),
            keys: Vec::new(),
            scan_from: None,
            logs_range: 50,
        },
    )
    .await;
    (o.code(), o.report())
}

/// An in-process node on a local port for `t`, with the fake chain behind
/// it and the test key written for `catalog-sign`.
async fn local_node(t: &T) -> (Arc<FakeChain>, String, HyperGet) {
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(t.dir.join("publisher.key"))
            .expect("key file");
        writeln!(f, "0x{}", hex::encode(SECRET)).expect("key");
    }
    let chain = Arc::new(FakeChain::default());
    let store = BulkStore::open(
        BulkBook {
            chain_id: CHAIN,
            book: BOOK,
        },
        chain.clone(),
        Arc::new(now_unix),
        &t.dir.join("node"),
        &t.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("store")
    .with_free_per_minute(10_000);
    let store = Arc::new(store);
    store.succ_start();
    let app = routes(store.clone())
        .merge(peer_routes(store.clone()))
        .merge(upload_routes(store.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(async move { axum::serve(listener, app).await });
    let get = HyperGet::new(Duration::from_secs(60));
    (chain, base, get)
}

#[tokio::test]
async fn succ_e2e_a_move_a_relink_an_unlink_and_two_hops_on_a_local_node() {
    let t = T::new("e2e");
    let (chain, base, get) = local_node(&t).await;

    // Catalog 0: a, b and c, each uploaded.
    publish(&t, &chain, &base, 0, &["a", "b", "c"]).await;
    // Catalog 1: a moves to b; the links version goes up first.
    t.cat(&[
        "--prev",
        &t.p("c0.bin"),
        "--move",
        "synthetic.a=synthetic.b",
        "--links-out",
        &t.p("l1"),
        "--out",
        &t.p("c1.bin"),
    ])
    .expect("c1");
    publish(&t, &chain, &base, 1, &["l1"]).await;
    let g = gone_until(&get, &base, "live", "synthetic.b").await;
    assert_eq!(g["error"]["code"], "bulk_dataset_removed");
    assert_eq!(g["successor"]["dataset"], "synthetic.b");
    assert_eq!(g["successor"]["generation"], 1);
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["end"]["dataset"], "synthetic.b");
    assert_eq!(r["chainChecked"], true);
    // Catalog 2: a relinked to c (a fix: the last row wins).
    t.cat(&[
        "--prev",
        &t.p("c1.bin"),
        "--links",
        &t.p("l1"),
        "--relink",
        "synthetic.a=synthetic.c",
        "--links-out",
        &t.p("l2"),
        "--out",
        &t.p("c2.bin"),
    ])
    .expect("c2");
    publish(&t, &chain, &base, 2, &["l2"]).await;
    gone_until(&get, &base, "live", "synthetic.c").await;
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(
        (code, r["end"]["dataset"].clone()),
        (0, json!("synthetic.c")),
        "{r}"
    );
    // Catalog 3: the link retracted.
    t.cat(&[
        "--prev",
        &t.p("c2.bin"),
        "--links",
        &t.p("l2"),
        "--unlink",
        "synthetic.a",
        "--links-out",
        &t.p("l3"),
        "--out",
        &t.p("c3.bin"),
    ])
    .expect("c3");
    publish(&t, &chain, &base, 3, &["l3"]).await;
    gone_until(&get, &base, "retracted", "synthetic.a").await;
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(
        (code, r["end"]["status"].clone()),
        (4, json!("retracted")),
        "{r}"
    );
    // Catalog 4: a linked to b again; catalog 5: b moves on to c. A link
    // names a dataset live in its own catalog, so a move on is the next
    // catalog's: two hops.
    t.cat(&[
        "--prev",
        &t.p("c3.bin"),
        "--links",
        &t.p("l3"),
        "--relink",
        "synthetic.a=synthetic.b",
        "--links-out",
        &t.p("l4"),
        "--out",
        &t.p("c4.bin"),
    ])
    .expect("c4");
    publish(&t, &chain, &base, 4, &["l4"]).await;
    gone_until(&get, &base, "live", "synthetic.b").await;
    t.cat(&[
        "--prev",
        &t.p("c4.bin"),
        "--links",
        &t.p("l4"),
        "--move",
        "synthetic.b=synthetic.c",
        "--links-out",
        &t.p("l5"),
        "--out",
        &t.p("c5.bin"),
    ])
    .expect("c5");
    publish(&t, &chain, &base, 5, &["l5"]).await;
    gone_until(&get, &base, "live", "synthetic.c").await;
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 0, "{r}");
    let hops: Vec<Value> = r["hops"].as_array().expect("hops").clone();
    assert_eq!(
        hops.iter().map(|h| h["to"].clone()).collect::<Vec<_>>(),
        vec![json!("synthetic.b"), json!("synthetic.c")]
    );
    assert_eq!(r["links"]["version"], 5);
}

/// Review SUCC-B7 L4 (the reviewer's test): `--links-next-generation`
/// through the tool, the node and the reader (generation 2 used, generation
/// 1 a tombstone), then a 2-hop chain whose middle hop is retracted.
#[tokio::test]
async fn succ_b7_e2e_next_generation_and_a_retracted_middle_hop() {
    let t = T::new("e2e-b7");
    let (chain, base, get) = local_node(&t).await;
    publish(&t, &chain, &base, 0, &["a", "b", "c"]).await;
    // c1: a -> b.
    t.cat(&[
        "--prev",
        &t.p("c0.bin"),
        "--move",
        "synthetic.a=synthetic.b",
        "--links-out",
        &t.p("l1"),
        "--out",
        &t.p("c1.bin"),
    ])
    .expect("c1");
    publish(&t, &chain, &base, 1, &["l1"]).await;
    gone_until(&get, &base, "live", "synthetic.b").await;
    // c2: b -> c: two hops.
    t.cat(&[
        "--prev",
        &t.p("c1.bin"),
        "--links",
        &t.p("l1"),
        "--move",
        "synthetic.b=synthetic.c",
        "--links-out",
        &t.p("l2"),
        "--out",
        &t.p("c2.bin"),
    ])
    .expect("c2");
    publish(&t, &chain, &base, 2, &["l2"]).await;
    gone_until(&get, &base, "live", "synthetic.c").await;
    // c3: the next generation, carrying both links, no new row.
    t.cat(&[
        "--prev",
        &t.p("c2.bin"),
        "--links",
        &t.p("l2"),
        "--links-next-generation",
        "--links-out",
        &t.p("l3"),
        "--out",
        &t.p("c3.bin"),
    ])
    .expect("c3");
    publish(&t, &chain, &base, 3, &["l3"]).await;
    let url = format!("{base}/v1/bulk/example.publisher/synthetic.a");
    let wait = async {
        loop {
            let (_, b) = get.get(&url, 1 << 20).await.expect("node");
            let v: Value = serde_json::from_slice(&b).expect("json");
            if v["successor"]["generation"] == 2 && v["successor"]["status"] == "live" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let g = tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .expect("generation 2");
    assert_eq!(g["successor"]["final"], "synthetic.c");
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["generation"], 2, "{r}");
    assert_eq!(r["skipped"][0]["why"], "removed", "{r}");
    assert_eq!(r["hops"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(r["end"]["dataset"], "synthetic.c", "{r}");
    // c4: the middle hop (b) retracted, in generation 2.
    t.cat(&[
        "--prev",
        &t.p("c3.bin"),
        "--links",
        &t.p("l3"),
        "--unlink",
        "synthetic.b",
        "--links-out",
        &t.p("l4"),
        "--out",
        &t.p("c4.bin"),
    ])
    .expect("c4");
    publish(&t, &chain, &base, 4, &["l4"]).await;
    gone_until(&get, &base, "retracted", "synthetic.b").await;
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 4, "{r}");
    assert_eq!(r["end"]["status"], "retracted", "{r}");
    assert_eq!(r["end"]["dataset"], "synthetic.b", "{r}");
    assert_eq!(r["hops"].as_array().map(Vec::len), Some(1), "{r}");
}

/// Review SUCC-B7 L4: a links version of more than one page. The answer
/// carries page 0; the reader fetches page 1 from the node over HTTP
/// (`--node`, `rows?root=`) and joins them; the tool then appends to that
/// version and the reader reads both pages of the next one.
#[tokio::test]
async fn succ_b7_e2e_links_of_two_pages_are_read_through_the_node() {
    let t = T::new("e2e-pages");
    let (chain, base, get) = local_node(&t).await;
    publish(&t, &chain, &base, 0, &["a", "b", "c"]).await;
    // Catalog 1 by hand: 1,500 links rows, the first moves a to b.
    t.hand_built_move(1_500);
    publish(&t, &chain, &base, 1, &["l1"]).await;
    gone_until(&get, &base, "live", "synthetic.b").await;
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        (r["links"]["rows"].clone(), r["links"]["pages"].clone()),
        (json!(1_500), json!(2))
    );
    assert_eq!(r["hops"][0]["position"], 0);
    // Catalog 2 by the tool: one more row, on page 1.
    t.cat(&[
        "--prev",
        &t.p("c1.bin"),
        "--links",
        &t.p("l1"),
        "--move",
        "synthetic.c=synthetic.b",
        "--links-out",
        &t.p("l2"),
        "--out",
        &t.p("c2.bin"),
    ])
    .expect("c2");
    publish(&t, &chain, &base, 2, &["l2"]).await;
    let url = format!("{base}/v1/bulk/example.publisher/synthetic.c");
    let wait = async {
        loop {
            let (_, b) = get.get(&url, 1 << 20).await.expect("node");
            let v: Value = serde_json::from_slice(&b).expect("json");
            if v["successor"]["status"] == "live" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let g = tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .expect("c moved");
    assert_eq!(g["successor"]["row"], "1501");
    let (code, r) = reader(&get, &chain, &base).await;
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        (r["links"]["rows"].clone(), r["links"]["pages"].clone()),
        (json!(1_501), json!(2))
    );
}
