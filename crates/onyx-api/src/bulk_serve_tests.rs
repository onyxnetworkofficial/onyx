//! Bulk datasets on a node (CL5, ADR-0059; THREATS CL5-1..8). The chain is a
//! fake node holding the signed book's `report`, `factView` and `keyState`,
//! answering reads only at its finalized block's hash.

use super::*;
use crate::x402::BoxFuture;
use axum::http::Request;
use http_body_util::BodyExt;
use onyx_bulk::manifest::NewVersion;
use onyx_bulk::row::{encode_row, Cell};
use onyx_bulk::shard::encode_shard;
use onyx_bulk::Schema;
use onyx_core::{DataRights, Redistribution, SourceId};
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;

const BOOK: [u8; 20] = [0xB0; 20];
const CHAIN: u64 = 31_337;
const SECRET: [u8; 32] = [0x42; 32];
/// The publisher's next key, after a theft of the first (THREATS CAT-5).
const SECRET2: [u8; 32] = [0x43; 32];
const FIN: [u8; 32] = [0xF1; 32];
const T0: u64 = 1_800_000_000;

fn pin() -> BulkBook {
    BulkBook {
        chain_id: CHAIN,
        book: BOOK,
    }
}

#[derive(Default)]
struct Book {
    /// digest -> (key, fact, report hash, recorded at)
    reports: BTreeMap<Hash32, (Hash32, Hash32, Hash32, u64)>,
    /// fact -> (state, accepted report hash, accepted at)
    facts: BTreeMap<Hash32, (u8, Hash32, u64)>,
    /// key -> (life, suspended, signer)
    keys: BTreeMap<Hash32, (u8, bool, [u8; 20])>,
    /// (key, sequence) -> digest (`sequenceOf`)
    seqs: BTreeMap<(Hash32, u64), Hash32>,
    down: bool,
    reads: u64,
    /// The finalized block's number when not 0x64 (a lagging read).
    fin: Option<u64>,
    /// digest -> value hash (`report(digest).valueHash`).
    values: BTreeMap<Hash32, Hash32>,
    /// key -> keccak of its source (`keyState(key).source`).
    sources: BTreeMap<Hash32, Hash32>,
    /// `KeyRegistered` logs: (block, key).
    logs: Vec<(u64, Hash32)>,
    /// The finalized block's time when not `T0`.
    ts: Option<u64>,
    /// Block number -> time (`T0` when not set).
    times: BTreeMap<u64, u64>,
}

pub(crate) struct Node(Mutex<Book>);

fn w(b: &[u8]) -> Vec<u8> {
    let mut x = vec![0u8; 32 - b.len()];
    x.extend_from_slice(b);
    x
}

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = (|| {
            let mut c = self.0.lock().map_err(|_| "lock")?;
            if c.down {
                return Err("connection refused".to_string());
            }
            c.reads += 1;
            Ok(match method {
                "eth_chainId" => json!(format!("0x{CHAIN:x}")),
                "eth_getBlockByNumber" if p[0] == "finalized" => {
                    let n = c.fin.unwrap_or(0x64);
                    let ts = c.ts.unwrap_or(T0);
                    json!({"number": format!("0x{n:x}"), "hash": hex0x(&FIN), "timestamp": format!("0x{ts:x}")})
                }
                "eth_getBlockByNumber" => {
                    let n = u64::from_str_radix(
                        p[0].as_str().unwrap_or("").trim_start_matches("0x"),
                        16,
                    )
                    .map_err(|_| "block number")?;
                    let ts = c.times.get(&n).copied().unwrap_or(T0);
                    json!({"number": format!("0x{n:x}"), "hash": hex0x(&FIN), "timestamp": format!("0x{ts:x}")})
                }
                "eth_call" => {
                    assert_eq!(
                        p[1]["blockHash"],
                        hex0x(&FIN),
                        "reads only at the finalized block"
                    );
                    let data = hex_of(&p[0]["data"])?;
                    let arg = Hash32(data[4..36].try_into().map_err(|_| "arg")?);
                    let sel: [u8; 4] = data[..4].try_into().map_err(|_| "sel")?;
                    let mut out = Vec::new();
                    if sel == selector("report(bytes32)") {
                        let (k, f, rh, at) = c.reports.get(&arg).copied().unwrap_or((
                            Hash32::ZERO,
                            Hash32::ZERO,
                            Hash32::ZERO,
                            0,
                        ));
                        let vh = c.values.get(&arg).copied().unwrap_or(Hash32::ZERO);
                        for x in [k.0.to_vec(), f.0.to_vec(), vh.0.to_vec(), rh.0.to_vec()] {
                            out.extend(w(&x));
                        }
                        out.extend(w(&at.to_be_bytes()));
                        out.extend(w(&[0]));
                        out.extend(w(&at.to_be_bytes()));
                    } else if sel == selector("factView(bytes32)") {
                        let (s, a, at) = c.facts.get(&arg).copied().unwrap_or((0, Hash32::ZERO, 0));
                        out.extend(w(&[s]));
                        out.extend(w(&a.0));
                        out.extend(w(&at.to_be_bytes()));
                        out.extend(w(&[0x80]));
                        out.extend(w(&[0]));
                    } else if sel == selector("sequenceOf(bytes32,uint64)") {
                        let s: [u8; 8] = data[60..68].try_into().map_err(|_| "seq")?;
                        let d = c
                            .seqs
                            .get(&(arg, u64::from_be_bytes(s)))
                            .copied()
                            .unwrap_or(Hash32::ZERO);
                        out.extend(w(&d.0));
                    } else if sel == selector("keyState(bytes32)") {
                        let (l, s, signer) = c.keys.get(&arg).copied().unwrap_or_default();
                        out.extend(w(&[l]));
                        out.extend(w(&[u8::from(s)]));
                        out.extend(w(&signer));
                        out.extend(w(&c.sources.get(&arg).copied().unwrap_or(Hash32::ZERO).0));
                        out.extend(w(&[0]));
                        out.extend(w(&[0]));
                    } else {
                        return Err("unknown call".into());
                    }
                    json!(hex0x(&out))
                }
                "eth_getLogs" => {
                    let n = |k: &str| {
                        u64::from_str_radix(
                            p[0][k].as_str().unwrap_or("0x0").trim_start_matches("0x"),
                            16,
                        )
                        .unwrap_or(0)
                    };
                    let (lo, hi) = (n("fromBlock"), n("toBlock"));
                    assert!(hi - lo < 200, "the public RPC's range limit");
                    json!(c
                        .logs
                        .iter()
                        .filter(|(b, _)| (lo..=hi).contains(b))
                        .map(|(_, k)| json!({"topics": [Hash32::ZERO.to_string(), k.to_string()]}))
                        .collect::<Vec<_>>())
                }
                _ => return Err(format!("unexpected {method}")),
            })
        })();
        Box::pin(async move { r })
    }
}

impl Node {
    /// The book withdraws `r` (its key compromised from before it): the
    /// fact no longer holds a value.
    pub(crate) fn withdraw(&self, r: &SignedReport) {
        let mut c = self.0.lock().expect("ok");
        let fact = keccak256(&r.payload.digest().0);
        c.facts.insert(fact, (0, Hash32::ZERO, 0));
    }

    /// The fact of `r` is disputed (open: it may be resolved back).
    fn dispute(&self, r: &SignedReport) {
        let mut c = self.0.lock().expect("ok");
        let fact = keccak256(&r.payload.digest().0);
        c.facts.insert(fact, (2, Hash32::ZERO, 0));
    }

    /// The key of `r` is suspended (or reinstated).
    fn suspend(&self, r: &SignedReport, on: bool) {
        let mut c = self.0.lock().expect("ok");
        if let Some(k) = c.keys.get_mut(&r.payload.key) {
            k.1 = on;
        }
    }

    /// The life of `r`'s key (1 active, 2 retired, 3 compromised).
    fn life(&self, r: &SignedReport, l: u8) {
        let mut c = self.0.lock().expect("ok");
        if let Some(k) = c.keys.get_mut(&r.payload.key) {
            k.0 = l;
        }
    }

    fn reads(&self) -> u64 {
        self.0.lock().expect("ok").reads
    }

    /// Record and accept `r`, signed with `secret`'s key.
    pub(crate) fn anchor_by(&self, r: &SignedReport, secret: &[u8; 32]) {
        let signer = onyx_bulk::anchor::address_of(secret).expect("ok");
        self.0
            .lock()
            .expect("ok")
            .keys
            .entry(r.payload.key)
            .or_insert((1, false, signer));
        self.anchor(r);
    }

    /// Record and accept `r` in the book (a posted, final report).
    pub(crate) fn anchor(&self, r: &SignedReport) {
        let mut c = self.0.lock().expect("ok");
        let fact = keccak256(&r.payload.digest().0);
        c.reports
            .insert(r.payload.digest(), (r.payload.key, fact, r.hash(), T0));
        c.values.insert(
            r.payload.digest(),
            keccak256(&borsh::to_vec(&r.payload.value).expect("value")),
        );
        c.facts.insert(fact, (1, r.hash(), T0));
        c.seqs
            .entry((r.payload.key, r.payload.sequence))
            .or_insert(r.payload.digest());
        let signer = onyx_bulk::anchor::address_of(&SECRET).expect("ok");
        c.keys.entry(r.payload.key).or_insert((1, false, signer));
    }
}

fn schema() -> Schema {
    Schema::from_json(&json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":2}],"key":"t"})).expect("ok")
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.publisher".into()),
        attribution: None,
        terms: String::new(),
    }
}

/// Shard files of `per` rows each over keys `keys`.
pub(crate) fn shard_files(keys: std::ops::Range<i128>, per: usize) -> Vec<Vec<u8>> {
    let rows: Vec<Vec<u8>> = keys
        .map(|t| {
            encode_row(&schema(), &[Cell::Int(t), Cell::Decimal(format!("{t}.50"))]).expect("ok")
        })
        .collect();
    rows.chunks(per).map(encode_shard).collect()
}

pub(crate) fn manifest(files: &[Vec<u8>], prev: Option<&Manifest>, created: u64) -> Manifest {
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    let base = entries.len();
    for (i, f) in files.iter().enumerate() {
        let c = check_shard(&schema(), f).expect("ok");
        entries.push(ShardEntry::new((base + i) as u32, &c.summary));
    }
    Manifest::build(
        NewVersion {
            dataset: "synthetic.metrics".into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: created,
            schema: schema(),
            rights: rights(),
        },
        prev,
        entries,
    )
    .expect("ok")
}

pub(crate) fn sign(m: &Manifest) -> SignedReport {
    onyx_bulk::anchor::sign(&SECRET, &m.header).expect("ok")
}

pub(crate) struct World {
    pub(crate) node: Arc<Node>,
    pub(crate) store: Arc<BulkStore>,
    clock: Arc<AtomicU64>,
    pub(crate) dir: PathBuf,
}

static N: AtomicU64 = AtomicU64::new(0);

/// `store` with its links index worker started, as the node starts it.
pub(crate) fn started(store: BulkStore) -> Arc<BulkStore> {
    let s = Arc::new(store);
    s.succ_start();
    s
}

fn world_with(limits: BulkLimits) -> World {
    let dir = std::env::temp_dir().join(format!(
        "onyx-bulk-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let node = Arc::new(Node(Mutex::new(Book::default())));
    let clock = Arc::new(AtomicU64::new(T0));
    let c = clock.clone();
    let store = BulkStore::open(
        pin(),
        node.clone(),
        Arc::new(move || c.load(Ordering::Relaxed)),
        &dir,
        &dir.join("warm"),
        limits,
    )
    .expect("ok")
    .with_free_per_minute(10_000);
    World {
        node,
        store: started(store),
        clock,
        dir,
    }
}

pub(crate) fn world() -> World {
    world_with(BulkLimits::default())
}

impl World {
    async fn post(&self, m: &Manifest, r: &SignedReport) -> Result<(StatusCode, Value), ApiError> {
        self.store
            .accept_manifest(&m.to_bytes(), &borsh::to_vec(r).expect("ok"))
            .await
    }

    async fn put(&self, root: &Hash32, file: Vec<u8>) -> Result<(StatusCode, Value), ApiError> {
        self.store
            .accept_shard(
                "example.publisher",
                "synthetic.metrics",
                root,
                &HeaderMap::new(),
                Body::from(file),
            )
            .await
    }

    async fn rows(&self, q: &str) -> Result<Value, ApiError> {
        self.store
            .rows(
                "example.publisher",
                "synthetic.metrics",
                &parse_rows_query(Some(q))?,
            )
            .await
    }
}

fn code(e: ApiError) -> &'static str {
    e.code
}

/// Publish 3 shards (keys 0..30, 10 rows each) and upload them all.
pub(crate) async fn published(w: &World) -> (Manifest, Vec<Vec<u8>>) {
    published_keys(w, 0..30).await
}

/// Publish the rows of `keys`, 10 rows a shard, and upload every shard.
pub(crate) async fn published_keys(
    w: &World,
    keys: std::ops::Range<i128>,
) -> (Manifest, Vec<Vec<u8>>) {
    let files = shard_files(keys, 10);
    let m = manifest(&files, None, T0);
    let r = sign(&m);
    w.node.anchor(&r);
    let (s, v) = w.post(&m, &r).await.expect("ok");
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(v["missing"].as_array().expect("ok").len(), files.len());
    for (f, e) in files.iter().zip(&m.shards) {
        assert_eq!(
            w.put(&e.root, f.clone()).await.expect("ok").0,
            StatusCode::CREATED
        );
    }
    (m, files)
}

/// `order=desc` (newcomer finding A15): the newest rows first, the same
/// proofs, pages that meet the ascending ones row for row.
#[tokio::test]
async fn rows_newest_first_check_and_page_like_the_ascending_ones() {
    let w = world();
    published(&w).await;
    // The newest 3 rows, across no shard edge, then across the edge at 20.
    let a = w.rows("order=desc&limit=3").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, None, None).expect("ok");
    assert!(c.descending);
    assert_eq!(
        c.rows.iter().map(|r| r.2).collect::<Vec<_>>(),
        vec![29, 28, 27]
    );
    assert_eq!(a["query"]["order"], json!("desc"));
    assert_eq!(a["next_page"], json!(1));
    assert_eq!(a["shards"].as_array().expect("ok").len(), 1);
    // The whole request checked (review NEWC-R1 M1): proven to start at the
    // newest row; the second page is a plain newest-first page.
    let newest_first = onyx_bulk::wire::RowsAsked {
        desc: true,
        ..Default::default()
    };
    assert!(
        onyx_bulk::wire::check_rows_asked(&a, &newest_first)
            .expect("ok")
            .newest_proven
    );
    let second = w.rows("order=desc&limit=3&page=1").await.expect("ok");
    assert!(onyx_bulk::wire::check_rows_asked(&second, &newest_first).is_err());
    let asked = onyx_bulk::wire::RowsAsked {
        page: 1,
        ..newest_first
    };
    assert!(
        !onyx_bulk::wire::check_rows_asked(&second, &asked)
            .expect("ok")
            .newest_proven
    );
    // Pages of 4 from the top: 8 pages, each checks, every row once.
    let mut keys = Vec::new();
    for page in 0..8 {
        let a = w
            .rows(&format!("order=desc&limit=4&page={page}"))
            .await
            .expect("ok");
        let c = onyx_bulk::wire::check_rows_answer(&a, None, None).expect("ok");
        keys.extend(c.rows.iter().map(|r| r.2));
        assert_eq!(a["next_page"].is_null(), page == 7, "page {page}");
    }
    assert_eq!(keys, (0..30).rev().collect::<Vec<i128>>());
    // With a range: the range's own newest rows.
    let a = w.rows("from=8&to=12&order=desc&limit=4").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(8), Some(12)).expect("ok");
    assert_eq!(
        c.rows.iter().map(|r| r.2).collect::<Vec<_>>(),
        vec![12, 11, 10, 9]
    );
    assert_eq!(a["next_page"], json!(1));
    let a = w
        .rows("from=8&to=12&order=desc&limit=4&page=1")
        .await
        .expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(8), Some(12)).expect("ok");
    assert_eq!(c.rows.iter().map(|r| r.2).collect::<Vec<_>>(), vec![8]);
    assert_eq!(a["next_page"], Value::Null);
    // `asc` is the order without the parameter; anything else is refused.
    let a = w.rows("order=asc&limit=2").await.expect("ok");
    assert_eq!(a["query"]["order"], json!("asc"));
    assert_eq!(a["rows"][0]["key"], json!("0"));
    assert_eq!(
        code(w.rows("order=newest").await.expect_err("bad")),
        "bad_request"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Dates in `from` and `to` (A15): unix seconds, UTC, a date alone is the
/// whole day; whole numbers as before.
#[test]
fn rows_query_takes_dates_and_whole_numbers() {
    let q = |s: &str| parse_rows_query(Some(s));
    let ok = |s: &str| q(s).expect(s);
    assert_eq!(ok("from=1767225600").from, Some(1_767_225_600));
    assert!(!ok("from=-5&to=1767225600").dated);
    let d = ok("from=2026-01-01&to=2026-01-01");
    assert_eq!((d.from, d.to), (Some(1_767_225_600), Some(1_767_311_999)));
    assert!(d.dated);
    assert_eq!(ok("from=2026-01-01T00:00:01Z").from, Some(1_767_225_601));
    assert_eq!(ok("to=2026-01-01T12%3A30%3A00").to, Some(1_767_270_600));
    assert_eq!(ok("from=1970-01-01").from, Some(0));
    assert_eq!(ok("from=1969-12-31").from, Some(-86_400));
    assert_eq!(ok("from=2024-02-29").from, Some(1_709_164_800));
    assert_eq!(ok("to=2000-12-31T23:59:59Z").to, Some(978_307_199));
    for bad in [
        "from=2026-1-1",
        "from=2026-02-30",
        "from=2025-02-29",
        "from=2026-13-01",
        "from=2026-01-01T24:00:00Z",
        "from=2026-01-01T00:00",
        "from=2026-01-01+01:00",
        "from=20260101x",
        "from=yesterday",
        "from=",
        "from=2026-01-02&to=2026-01-01",
        "order=desc&order=asc",
    ] {
        assert_eq!(code(q(bad).expect_err(bad)), "bad_request", "{bad}");
    }
    assert!(q("from=x").expect_err("x").message.contains("2026-01-31"));
}

/// A date is taken, and the unit named, only for a key that reads as unix
/// seconds (review NEWC-R1 M3): named as a time and with its whole signed
/// range between 1973 and 2100; never from the column's name alone.
#[tokio::test]
async fn a_date_needs_a_key_that_reads_as_unix_seconds() {
    // Keys 1,700,000,000..+30 in a column named `t`: seconds of 2023-11-14.
    let w = world();
    published_keys(&w, 1_700_000_000..1_700_000_030).await;
    let a = w
        .rows("from=2023-11-14&to=2023-11-14&limit=2")
        .await
        .expect("ok");
    assert_eq!(a["key"], json!({"column": "t", "unit": "unix_seconds"}));
    assert_eq!(a["query"]["from"], json!("1699920000"));
    assert_eq!(a["query"]["to"], json!("1700006399"));
    assert_eq!(a["rows"].as_array().expect("rows").len(), 2);
    onyx_bulk::wire::check_rows_answer(&a, Some(1_699_920_000), Some(1_700_006_399)).expect("ok");
    let a = w.rows("from=2023-11-15").await.expect("ok");
    assert_eq!(a["rows"], json!([]));
    let _ = std::fs::remove_dir_all(&w.dir);
    // Keys 0..30 in the same column: not unix seconds by their range. No
    // unit is claimed and a date is refused; whole numbers work as before.
    let w = world();
    published(&w).await;
    let a = w.rows("from=0&to=5").await.expect("ok");
    assert_eq!(a["key"], json!({"column": "t", "unit": null}));
    let e = w.rows("from=1970-01-01").await.expect_err("refused");
    assert_eq!(e.code, "bad_request");
    assert!(
        e.message.contains("whole numbers") && e.message.contains("1973-03-03"),
        "{}",
        e.message
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[test]
fn only_a_named_time_key_in_the_unix_range_reads_as_unix_seconds() {
    let day = 86_400i128;
    // The live shape: a `day` column of days' first seconds.
    assert!(key_is_unix_seconds("day", 1_279_324_800, 1_794_528_000));
    assert!(key_is_unix_seconds("t", UNIX_KEY_MIN, UNIX_KEY_END - 1));
    for (name, lo, hi, why) in [
        (
            "height",
            1_279_324_800,
            1_794_528_000,
            "not named as a time",
        ),
        ("ts", 1_279_324_800_000, 1_794_528_000_000, "milliseconds"),
        (
            "ts",
            1_279_324_800,
            1_794_528_000_000,
            "milliseconds at the end",
        ),
        ("day", 20_100_717, 20_261_113, "YYYYMMDD"),
        (
            "date",
            2_010_071_700,
            2_026_111_300,
            "YYYYMMDDHH: in range, not whole days",
        ),
        (
            "day",
            1_279_324_801,
            1_794_528_000,
            "a day key off the day's first second",
        ),
        ("t", 0, 30, "small counters"),
        ("t", -5, 1_794_528_000, "before 1973"),
        ("t", UNIX_KEY_MIN - 1, UNIX_KEY_MIN, "just under the range"),
        ("t", UNIX_KEY_END - 1, UNIX_KEY_END, "2100 and later"),
        ("day", 800_000, 820_000, "a block height named day"),
    ] {
        assert!(!key_is_unix_seconds(name, lo, hi), "{why}");
    }
    assert_eq!(UNIX_KEY_END % day, 0);
    let dated = parse_rows_query(Some("from=2026-01-01")).expect("q");
    let plain = parse_rows_query(Some("from=5")).expect("q");
    assert!(dates_fit(&dated, Some("day"), true).is_ok());
    assert!(dates_fit(&plain, Some("height"), false).is_ok());
    for col in ["height", "day"] {
        let e = dates_fit(&dated, Some(col), false).expect_err("refused");
        assert_eq!(e.code, "bad_request");
        assert!(e.message.contains(col) && e.message.contains("whole numbers"));
    }
}

#[tokio::test]
async fn publish_upload_and_query_with_client_check() {
    let w = world();
    let (m, _) = published(&w).await;
    // A range across the shard boundary at key 10: rows 8..=12.
    let a = w.rows("from=8&to=12").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(8), Some(12)).expect("ok");
    assert_eq!(
        c.rows.iter().map(|r| r.2).collect::<Vec<_>>(),
        vec![8, 9, 10, 11, 12]
    );
    assert_eq!(c.manifest_root, m.header.root());
    assert_eq!(a["anchor"]["digest"], json!(c.digest.to_string()));
    assert_eq!(a["anchor"]["checked_at"]["number"], json!(100));
    assert_eq!(a["shards"].as_array().expect("ok").len(), 2);
    assert_eq!(a["next_page"], Value::Null);
    // Pages of 4 over 0..=29: 8 pages, each checks, all rows once.
    let mut keys = Vec::new();
    for page in 0..8 {
        let a = w.rows(&format!("limit=4&page={page}")).await.expect("ok");
        let c = onyx_bulk::wire::check_rows_answer(&a, None, None).expect("ok");
        keys.extend(c.rows.iter().map(|r| r.2));
        assert_eq!(a["next_page"].is_null(), page == 7, "page {page}");
    }
    assert_eq!(keys, (0..30).collect::<Vec<i128>>());
    // Re-posting the same manifest is idempotent (nothing missing now).
    let (s, v) = w.post(&m, &sign(&m)).await.expect("ok");
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["missing"], json!([]));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn routes_answer_through_the_router() {
    let w = world();
    let (m, _) = published(&w).await;
    let app = routes(w.store.clone()).merge(upload_routes(w.store.clone()));
    let get = |uri: &str| Request::get(uri).body(Body::empty()).expect("ok");
    let r = app.clone().oneshot(get("/v1/bulk")).await.expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&r.into_body().collect().await.expect("ok").to_bytes()).expect("ok");
    assert_eq!(v["datasets"][0]["rows"], json!(30));
    assert_eq!(v["datasets"][0]["stored_shards"], json!(3));
    let r = app
        .clone()
        .oneshot(get("/v1/bulk/example.publisher/synthetic.metrics"))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let r = app
        .clone()
        .oneshot(get(
            "/v1/bulk/example.publisher/synthetic.metrics/rows?from=3&to=4",
        ))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    for bad in [
        "?limit=0",
        "?limit=1001",
        "?from=5&to=4",
        "?from=1.5",
        "?x=1",
        "?page=1&page=2",
    ] {
        let r = app
            .clone()
            .oneshot(get(&format!(
                "/v1/bulk/example.publisher/synthetic.metrics/rows{bad}"
            )))
            .await
            .expect("ok");
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = app
        .clone()
        .oneshot(get("/v1/bulk/example.publisher/other.set/rows"))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    // The manifest route through the router.
    let body = json!({"manifest": hex0x(&m.to_bytes()), "report": hex0x(&borsh::to_vec(&sign(&m)).expect("ok"))});
    let r = app
        .clone()
        .oneshot(
            Request::post("/v1/bulk/manifests")
                .body(Body::from(body.to_string()))
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// ADR-0060: the storage network's peer routes give a held manifest and a
/// held shard file by root, and refuse an unknown or malformed root.
#[tokio::test]
async fn peer_routes_give_manifests_and_shard_files_by_root() {
    let w = world();
    let (m, files) = published(&w).await;
    let app = routes(w.store.clone())
        .merge(upload_routes(w.store.clone()))
        .merge(peer_routes(w.store.clone()));
    let get = |uri: String| Request::get(uri).body(Body::empty()).expect("ok");
    let root = m.header.root();
    let r = app
        .clone()
        .oneshot(get(format!("/v1/bulk/manifests/{root}")))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&r.into_body().collect().await.expect("ok").to_bytes()).expect("ok");
    crate::openapi_check::check("GET", &format!("/v1/bulk/manifests/{root}"), 200, &v);
    assert_eq!(v["manifest"], json!(hex0x(&m.to_bytes())));
    let rep = onyx_bulk::wire::hex_field(&v["report"], "report").expect("hex");
    assert_eq!(rep, borsh::to_vec(&sign(&m)).expect("ok"));
    let e = m.shards[1];
    let r = app
        .clone()
        .oneshot(get(format!("/v1/bulk/shards/{}", e.root)))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers().get(header::CONTENT_TYPE).expect("type"),
        "application/octet-stream"
    );
    // Streamed from the checked file with its exact length (SERVE-WRITE-1).
    assert_eq!(
        http_body::Body::size_hint(r.body()).exact(),
        Some(files[1].len() as u64)
    );
    let body = r.into_body().collect().await.expect("ok").to_bytes();
    assert_eq!(body.to_vec(), files[1]);
    // An unknown manifest root, an unknown shard root, a malformed root.
    let other = Hash32([0xab; 32]);
    for (uri, status, code) in [
        (
            format!("/v1/bulk/manifests/{other}"),
            404,
            "bulk_unknown_manifest",
        ),
        (
            format!("/v1/bulk/shards/{other}"),
            409,
            "bulk_shard_missing",
        ),
        ("/v1/bulk/shards/0xAB".to_string(), 400, "bad_request"),
    ] {
        let r = app.clone().oneshot(get(uri.clone())).await.expect("ok");
        assert_eq!(r.status().as_u16(), status, "{uri}");
        let v: Value =
            serde_json::from_slice(&r.into_body().collect().await.expect("ok").to_bytes())
                .expect("ok");
        crate::openapi_check::check("GET", &uri, status, &v);
        assert_eq!(v["error"]["code"], json!(code), "{uri}: {v}");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS SERVE-WRITE-1: a shard answer is sent from its checked open file
/// in frames of at most [`FILE_FRAME`] bytes, never held whole in memory; the
/// handle keeps the checked bytes even when the path is removed (a move
/// between tiers), and a file shorter than its checked length fails the body
/// instead of ending it short.
#[tokio::test]
async fn serve_write_1_a_shard_streams_from_its_checked_file_in_frames() {
    let dir = std::env::temp_dir().join(format!(
        "onyx-file-body-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("shard");
    let data: Vec<u8> = (0..(FILE_FRAME * 3 + 10))
        .map(|i| (i % 251) as u8)
        .collect();
    std::fs::write(&path, &data).expect("write");
    let f = std::fs::File::open(&path).expect("open");
    std::fs::remove_file(&path).expect("moved away");
    let mut body = FileBody::new(f, data.len() as u64);
    assert_eq!(
        http_body::Body::size_hint(&body).exact(),
        Some(data.len() as u64)
    );
    let mut got = Vec::new();
    while let Some(fr) = body.frame().await {
        let fr = fr.expect("frame").into_data().expect("data");
        assert!(
            !fr.is_empty() && fr.len() <= FILE_FRAME,
            "{} bytes",
            fr.len()
        );
        got.extend_from_slice(&fr);
    }
    assert_eq!(got, data);
    assert!(http_body::Body::is_end_stream(&body));
    // A file that ends early: an error, not a short answer.
    std::fs::write(&path, &data[..100]).expect("write");
    let f = std::fs::File::open(&path).expect("open");
    let mut body = FileBody::new(f, 200);
    let first = body.frame().await.expect("frame").expect("ok");
    assert_eq!(first.into_data().expect("data").len(), 100);
    let err = body.frame().await.expect("frame").expect_err("cut");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review SERVE-WRITE-R2 finding 3: the route itself streams a shard larger
/// than a frame in frames of at most [`FILE_FRAME`] bytes (a buffered answer
/// would come as one frame).
#[tokio::test]
async fn serve_write_1_the_shard_route_answers_in_frames() {
    let w = world();
    let files = shard_files(0..8_000, 8_000);
    assert!(files[0].len() > 2 * FILE_FRAME, "{} bytes", files[0].len());
    let m = manifest(&files, None, T0);
    let r = sign(&m);
    w.node.anchor(&r);
    assert_eq!(w.post(&m, &r).await.expect("ok").0, StatusCode::CREATED);
    assert_eq!(
        w.put(&m.shards[0].root, files[0].clone())
            .await
            .expect("ok")
            .0,
        StatusCode::CREATED
    );
    let resp = peer_routes(w.store.clone())
        .oneshot(
            Request::get(format!("/v1/bulk/shards/{}", m.shards[0].root))
                .body(Body::empty())
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    let (mut got, mut frames) = (Vec::new(), 0);
    while let Some(f) = body.frame().await {
        let f = f.expect("frame").into_data().expect("data");
        assert!(f.len() <= FILE_FRAME, "{} bytes in one frame", f.len());
        got.extend_from_slice(&f);
        frames += 1;
    }
    assert!(frames > 2, "{frames} frames");
    assert_eq!(got, files[0]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-1: a shard whose root is in no anchored manifest, a file sent
/// under another shard's root, a wrong size and an encoded body are refused,
/// and nothing is stored.
#[tokio::test]
async fn forged_shards_are_refused() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let r = sign(&m);
    w.node.anchor(&r);
    w.post(&m, &r).await.expect("ok");
    let forged = shard_files(100..110, 10).remove(0);
    let forged_root = check_shard(&schema(), &forged).expect("ok").summary.root;
    assert_eq!(
        code(
            w.put(&forged_root, forged.clone())
                .await
                .expect_err("refused")
        ),
        "bulk_unknown_shard"
    );
    // Shard 0 with one value changed (same size, still a valid shard):
    // content mismatch.
    let mut changed = files[0].clone();
    let at = changed.windows(3).rposition(|x| x == b".50").expect("ok");
    changed[at + 1] = b'4';
    assert!(check_shard(&schema(), &changed).is_ok());
    assert_eq!(
        code(
            w.put(&m.shards[0].root, changed)
                .await
                .expect_err("refused")
        ),
        "bulk_shard_mismatch"
    );
    // A file of the right root plus one byte: size refused.
    let mut longer = files[0].clone();
    longer.push(0);
    assert_eq!(
        code(w.put(&m.shards[0].root, longer).await.expect_err("refused")),
        "bulk_shard_size"
    );
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    let e = w
        .store
        .accept_shard(
            "example.publisher",
            "synthetic.metrics",
            &m.shards[0].root,
            &h,
            Body::from(files[0].clone()),
        )
        .await
        .expect_err("refused");
    assert_eq!(code(e), "bulk_encoded_body");
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_static("999999999"),
    );
    let e = w
        .store
        .accept_shard(
            "example.publisher",
            "synthetic.metrics",
            &m.shards[0].root,
            &h,
            Body::from(files[0].clone()),
        )
        .await
        .expect_err("refused");
    assert_eq!(code(e), "bulk_shard_size");
    assert_eq!(w.store.tier(&m.shards[0].root), None);
    // A query needing an unstored shard says so instead of answering short.
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("refused")),
        "bulk_shard_missing"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-2: a manifest not in the book (or not final) is refused; a
/// second anchored manifest for a held version forks the dataset and stops
/// serving it; a version out of order is refused.
#[tokio::test]
async fn manifest_swap_and_unanchored_manifests() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let r = sign(&m);
    let e = w.post(&m, &r).await.expect_err("refused");
    assert_eq!(code(e), "bulk_not_anchored");
    w.node.0.lock().expect("ok").down = true;
    assert_eq!(
        code(w.post(&m, &r).await.expect_err("refused")),
        "bulk_chain_unavailable"
    );
    w.node.0.lock().expect("ok").down = false;
    // Signed by the key but the payload's key swapped: refused before the chain.
    let mut rekey = r.clone();
    rekey.payload.key = Hash32([1; 32]);
    assert_eq!(
        code(w.post(&m, &rekey).await.expect_err("refused")),
        "bulk_bad_manifest"
    );
    w.node.anchor(&r);
    w.post(&m, &r).await.expect("ok");
    for (f, e) in files.iter().zip(&m.shards) {
        w.put(&e.root, f.clone()).await.expect("ok");
    }
    // Version 3 before 2.
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 10);
    let v3 = manifest(&shard_files(40..50, 10), Some(&v2), T0 + 20);
    let r3 = sign(&v3);
    w.node.anchor(&r3);
    assert_eq!(
        code(w.post(&v3, &r3).await.expect_err("refused")),
        "bulk_not_successor"
    );
    // A swapped v1 (other rows), unanchored: refused, dataset still served.
    let swap = manifest(&shard_files(0..30, 15), None, T0 + 1);
    let rs = sign(&swap);
    assert_eq!(
        code(w.post(&swap, &rs).await.expect_err("refused")),
        "bulk_not_anchored"
    );
    assert!(w.rows("from=0&to=1").await.is_ok());
    // The same swap anchored: the publisher's key signed two v1s. Forked.
    w.node.anchor(&rs);
    assert_eq!(
        code(w.post(&swap, &rs).await.expect_err("refused")),
        "bulk_dataset_forked"
    );
    assert_eq!(
        code(w.rows("from=0&to=1").await.expect_err("refused")),
        "bulk_dataset_forked"
    );
    // It survives a restart.
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("ok");
    assert_eq!(
        code(
            again
                .info("example.publisher", "synthetic.metrics")
                .expect_err("refused")
        ),
        "bulk_dataset_forked"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-3: versions append; an old report re-sent changes nothing; the
/// same root under another report that is not anchored is refused.
#[tokio::test]
async fn versions_append_and_replays_change_nothing() {
    let w = world();
    let (m, _) = published(&w).await;
    let add = shard_files(30..45, 10);
    let v2 = manifest(&add, Some(&m), T0 + 60);
    let r2 = sign(&v2);
    w.node.anchor(&r2);
    let (s, v) = w.post(&v2, &r2).await.expect("ok");
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        v["missing"].as_array().expect("ok").len(),
        2,
        "only the new shards are missing"
    );
    for (f, e) in add.iter().zip(&v2.shards[3..]) {
        w.put(&e.root, f.clone()).await.expect("ok");
    }
    let a = w.rows("from=28&to=31").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(28), Some(31)).expect("ok");
    assert_eq!(c.header.version, 2);
    assert_eq!(c.rows.len(), 4);
    // Replays of v1 (same report, or the same root under another sequence).
    assert_eq!(w.post(&m, &sign(&m)).await.expect("ok").0, StatusCode::OK);
    // The sequence is fixed by dataset and version and the signature is
    // deterministic: signing v1 again gives the very same report.
    assert_eq!(sign(&m), sign(&m));
    assert_eq!(
        w.rows("from=0&to=0")
            .await
            .map(|a| a["manifest"]["version"].clone())
            .expect("ok"),
        json!(2)
    );
    // Restart: both versions reload and serve.
    let again = Arc::new(
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0),
            &w.dir,
            &w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("ok"),
    );
    let a = again
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=40&to=44")).expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(
        onyx_bulk::wire::check_rows_answer(&a, Some(40), Some(44))
            .expect("ok")
            .rows
            .len(),
        5
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-4: an oversized manifest body and malformed manifests are
/// refused before any chain read.
#[tokio::test]
async fn oversize_and_malformed_manifests() {
    let w = world();
    let app = upload_routes(w.store.clone());
    let big = vec![b'x'; MAX_MANIFEST_BODY + 1];
    let r = app
        .clone()
        .oneshot(
            Request::post("/v1/bulk/manifests")
                .body(Body::from(big))
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let e = w
        .store
        .accept_manifest(b"junk", b"junk")
        .await
        .expect_err("refused");
    assert_eq!(code(e), "bulk_bad_manifest");
    assert_eq!(w.node.0.lock().expect("ok").reads, 0);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-5: per-publisher bytes, per-publisher datasets and the node
/// total are enforced when the manifest is taken.
#[tokio::test]
async fn quotas() {
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let bytes = m.header.byte_count;
    let w = world_with(BulkLimits {
        quota_bytes: bytes - 1,
        ..BulkLimits::default()
    });
    let r = sign(&m);
    w.node.anchor(&r);
    assert_eq!(
        code(w.post(&m, &r).await.expect_err("refused")),
        "bulk_quota_exceeded"
    );
    let w2 = world_with(BulkLimits {
        max_total_bytes: bytes - 1,
        ..BulkLimits::default()
    });
    w2.node.anchor(&r);
    assert_eq!(
        code(w2.post(&m, &r).await.expect_err("refused")),
        "bulk_node_full"
    );
    let w3 = world_with(BulkLimits {
        quota_bytes: bytes,
        ..BulkLimits::default()
    });
    w3.node.anchor(&r);
    assert_eq!(w3.post(&m, &r).await.expect("ok").0, StatusCode::CREATED);
    // A second version adding bytes over the quota.
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 5);
    let r2 = sign(&v2);
    w3.node.anchor(&r2);
    assert_eq!(
        code(w3.post(&v2, &r2).await.expect_err("refused")),
        "bulk_quota_exceeded"
    );
    for d in [w.dir, w2.dir, w3.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// THREATS CL5-6: a key marked compromised (or the fact disputed) stops
/// serving at the next anchor recheck.
#[tokio::test]
async fn compromised_key_stops_serving() {
    let w = world();
    let (m, _) = published(&w).await;
    assert!(w.rows("from=0&to=1").await.is_ok());
    let key = sign(&m).payload.key;
    // A retired key (a rotation) keeps what it signed before.
    w.node
        .0
        .lock()
        .expect("ok")
        .keys
        .get_mut(&key)
        .expect("ok")
        .0 = 2;
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert!(w.rows("from=0&to=1").await.is_ok());
    w.node
        .0
        .lock()
        .expect("ok")
        .keys
        .get_mut(&key)
        .expect("ok")
        .0 = 3;
    // The book withdraws what the compromised key stated from `since` on.
    w.node.withdraw(&sign(&m));
    // Inside the recheck window the cached read stands; past it, refused.
    assert!(w.rows("from=0&to=1").await.is_ok());
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let e = w.rows("from=0&to=1").await.expect_err("refused");
    assert_eq!(code(e), "bulk_not_servable");
    // Recovery: the reviewer registers a new key for the publisher, which
    // signs the same manifest again; the node re-anchors the held version.
    let new_secret = [0x43; 32];
    let r2 = onyx_bulk::anchor::sign(&new_secret, &m.header).expect("ok");
    {
        let signer = onyx_bulk::anchor::address_of(&new_secret).expect("ok");
        let mut c = w.node.0.lock().expect("ok");
        c.keys.insert(r2.payload.key, (1, false, signer));
    }
    w.node.anchor(&r2);
    let (s, v) = w.post(&m, &r2).await.expect("ok");
    assert_eq!((s, v["reanchored"].clone()), (StatusCode::OK, json!(true)));
    let a = w.rows("from=0&to=1").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(1)).expect("ok");
    assert_eq!(
        c.signer,
        onyx_bulk::anchor::address_of(&new_secret).expect("ok")
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-7: a manifest whose rights forbid redistribution, are
/// restricted, name another source, or lack the statement is refused.
#[tokio::test]
async fn rights_are_checked() {
    let w = world();
    let files = shard_files(0..10, 10);
    let m = manifest(&files, None, T0);
    let changes: [fn(&mut Manifest); 3] = [
        |m: &mut Manifest| m.header.rights.redistribution = Redistribution::Forbidden,
        |m: &mut Manifest| m.header.rights_statement = "ok".into(),
        |m: &mut Manifest| m.header.rights.source = SourceId("other.source".into()),
    ];
    for f in changes {
        let mut bad = m.clone();
        f(&mut bad);
        let r = sign(&bad);
        w.node.anchor(&r);
        let e = w.post(&bad, &r).await.expect_err("refused");
        assert_eq!(code(e), "bulk_bad_manifest");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS CL5-8 (ADR-0052): quiet shards go warm, a read brings them back
/// hot, and they still serve proven rows.
#[tokio::test]
async fn tiers_warm_and_back() {
    let w = world_with(BulkLimits {
        warm_after_secs: 100,
        ..BulkLimits::default()
    });
    let (m, _) = published(&w).await;
    assert_eq!(w.store.sweep(), 0);
    w.clock.fetch_add(100, Ordering::Relaxed);
    assert_eq!(w.store.sweep(), 3);
    assert_eq!(w.store.tier(&m.shards[2].root), Some(Tier::Warm));
    let a = w.rows("from=25&to=26").await.expect("ok");
    onyx_bulk::wire::check_rows_answer(&a, Some(25), Some(26)).expect("ok");
    assert_eq!(w.store.tier(&m.shards[2].root), Some(Tier::Hot));
    assert_eq!(w.store.tier(&m.shards[0].root), Some(Tier::Warm));
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review CL5-R4: a thief's version withdrawn by the book (the key marked
/// compromised from before it) stops being served, the honest earlier
/// version serves again, and the publisher's own next version (under a new
/// key) replaces the withdrawn one; a version still anchored is a fork.
#[tokio::test]
async fn a_withdrawn_latest_version_falls_back_and_is_replaced() {
    let w = world();
    let (m, _) = published(&w).await;
    let thief = manifest(&shard_files(30..40, 10), Some(&m), T0 + 10);
    let rt = sign(&thief);
    w.node.anchor(&rt);
    w.post(&thief, &rt).await.expect("ok");
    assert_eq!(
        w.rows("from=0&to=1").await.expect("ok")["manifest"]["version"],
        json!(2)
    );
    w.node.withdraw(&rt);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let a = w.rows("from=0&to=1").await.expect("ok");
    assert_eq!(a["manifest"]["version"], json!(1));
    onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(1)).expect("ok");
    // The publisher's own version 2 under a new key replaces the thief's.
    let new_secret = [0x44; 32];
    let honest = manifest(&shard_files(30..50, 10), Some(&m), T0 + 20);
    let rh = onyx_bulk::anchor::sign(&new_secret, &honest.header).expect("ok");
    {
        let signer = onyx_bulk::anchor::address_of(&new_secret).expect("ok");
        w.node
            .0
            .lock()
            .expect("ok")
            .keys
            .insert(rh.payload.key, (1, false, signer));
    }
    w.node.anchor(&rh);
    let (s, _) = w.post(&honest, &rh).await.expect("ok");
    assert_eq!(s, StatusCode::CREATED);
    assert!(w
        .dir
        .join("manifests/example.publisher/synthetic.metrics/withdrawn")
        .join(format!("v2-{}.bin", thief.header.root().to_hex()))
        .exists());
    let a = w.rows("from=0&to=1").await.expect("ok");
    assert_eq!(
        a["manifest"]["root"],
        json!(honest.header.root().to_string())
    );
    // A third manifest for version 2 while the honest one holds: a fork.
    let other = manifest(&shard_files(30..60, 10), Some(&m), T0 + 30);
    let ro = onyx_bulk::anchor::sign(&new_secret, &other.header).expect("ok");
    w.node.anchor(&ro);
    assert_eq!(
        code(w.post(&other, &ro).await.expect_err("refused")),
        "bulk_dataset_forked"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review CL5-R3: a second dataset whose manifest names another dataset's
/// stored shard root under another schema never changes the first one's
/// answers (loads are keyed by root and schema, and checked against each
/// dataset's entry).
#[tokio::test]
async fn a_shared_root_under_another_schema_does_not_disturb_the_owner() {
    let w = world();
    let (m, files) = published(&w).await;
    // Dataset B lists A's first shard with a wrong entry (other key range).
    let mut e = m.shards[0];
    e.key_min = 0;
    e.key_max = 1_000;
    let b = Manifest::build(
        NewVersion {
            dataset: "synthetic.copy".into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: T0,
            schema: schema(),
            rights: rights(),
        },
        None,
        vec![e],
    )
    .expect("ok");
    let rb = sign(&b);
    w.node.anchor(&rb);
    w.post(&b, &rb).await.expect("ok");
    let e2 = w
        .store
        .accept_shard(
            "example.publisher",
            "synthetic.copy",
            &e.root,
            &HeaderMap::new(),
            Body::from(files[0].clone()),
        )
        .await
        .expect_err("refused");
    assert_eq!(code(e2), "bulk_shard_mismatch");
    let eb = w
        .store
        .rows(
            "example.publisher",
            "synthetic.copy",
            &parse_rows_query(Some("from=0&to=5")).expect("ok"),
        )
        .await
        .expect_err("refused");
    assert_eq!(code(eb), "bulk_shard_missing");
    let a = w.rows("from=0&to=9").await.expect("ok");
    assert_eq!(
        onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(9))
            .expect("ok")
            .rows
            .len(),
        10
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Catalogs (ADR-0059 amendment 1; THREATS CAT-R1) -------------------------------------------

use onyx_bulk::catalog::{self as cat, Catalog, CatalogLeaf};

fn publisher() -> SourceId {
    SourceId("example.publisher".into())
}

fn other_leaf(dataset: &str) -> CatalogLeaf {
    CatalogLeaf {
        publisher: publisher(),
        dataset: dataset.into(),
        version: 1,
        prev: Hash32::ZERO,
        root: Hash32([5; 32]),
    }
}

fn catalog_of(prev: Option<&Catalog>, created: u64, leaves: Vec<CatalogLeaf>) -> Catalog {
    Catalog::build(publisher(), prev, created, leaves).expect("catalog")
}

fn sign_catalog(c: &Catalog) -> SignedReport {
    cat::sign(&SECRET, &c.header).expect("ok")
}

impl World {
    async fn post_catalog(
        &self,
        c: &Catalog,
        r: &SignedReport,
    ) -> Result<(StatusCode, Value), ApiError> {
        self.store
            .accept_catalog(
                &borsh::to_vec(c).expect("ok"),
                &borsh::to_vec(r).expect("ok"),
            )
            .await
    }

    async fn post_in(&self, m: &Manifest, n: u64) -> Result<(StatusCode, Value), ApiError> {
        self.store
            .accept_manifest_in_catalog(&m.to_bytes(), n)
            .await
    }

    async fn upload_all(&self, m: &Manifest, files: &[Vec<u8>]) {
        for (f, e) in files.iter().zip(&m.shards) {
            if !self.store.holds_shard(&e.root) {
                assert_eq!(
                    self.put(&e.root, f.clone()).await.expect("ok").0,
                    StatusCode::CREATED
                );
            }
        }
    }
}

#[tokio::test]
async fn cat_r1_one_catalog_report_anchors_its_manifests() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), other_leaf("synthetic.other")],
    );
    let r0 = sign_catalog(&c0);
    // Not in the book yet.
    assert_eq!(
        code(w.post_catalog(&c0, &r0).await.expect_err("refused")),
        "bulk_not_anchored"
    );
    w.node.anchor(&r0);
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("ok").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("ok").0,
        StatusCode::OK
    );
    // The manifest, with no report of its own.
    assert_eq!(w.post_in(&m, 0).await.expect("ok").0, StatusCode::CREATED);
    w.upload_all(&m, &files).await;
    let a = w.rows("from=8&to=12").await.expect("ok");
    let ck = onyx_bulk::wire::check_rows_answer(&a, Some(8), Some(12)).expect("client check");
    assert_eq!(
        ck.catalog.as_ref().map(cat::CatalogHeader::root),
        Some(c0.header.root())
    );
    assert_eq!(ck.digest, r0.payload.digest());
    // Its own report was never posted: the catalog's report is the only one.
    let info = w
        .store
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    assert_eq!(info["anchor"]["catalog"]["number"], json!(0));
    // A version the catalog does not list, a catalog this node does not hold.
    let m2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 20);
    assert_eq!(
        code(w.post_in(&m2, 0).await.expect_err("refused")),
        "bulk_not_in_catalog"
    );
    assert_eq!(
        code(w.post_in(&m2, 4).await.expect_err("refused")),
        "bulk_catalog_unknown"
    );
    // Both survive a restart, with the catalog served back.
    let again = Arc::new(
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0),
            &w.dir,
            &w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("reopen"),
    );
    assert_eq!(
        again
            .catalog_record("example.publisher", 0)
            .expect("rebuilt")
            .map(|(c, _)| c.header.root()),
        Some(c0.header.root())
    );
    let a = again
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=0&to=3")).expect("ok"),
        )
        .await
        .expect("ok");
    assert!(onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(3)).is_ok());
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_r1_catalogs_are_taken_in_order_as_append_only_successors() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let m2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 20);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 30,
        vec![CatalogLeaf::of(&m2.header), other_leaf("synthetic.new")],
    );
    let c2 = catalog_of(Some(&c1), T0 + 40, c1.leaves.clone());
    for c in [&c0, &c1, &c2] {
        w.node.anchor(&sign_catalog(c));
    }
    // Number 2 before 0 and 1: refused, naming the one needed.
    let e = w
        .post_catalog(&c2, &sign_catalog(&c2))
        .await
        .expect_err("gap");
    assert_eq!(code(e), "bulk_catalog_gap");
    w.post_catalog(&c0, &sign_catalog(&c0)).await.expect("c0");
    w.post_catalog(&c1, &sign_catalog(&c1)).await.expect("c1");
    // Version 2 is taken through catalog 1 once version 1 is held.
    assert_eq!(
        code(w.post_in(&m2, 1).await.expect_err("order")),
        "bulk_not_successor"
    );
    w.post_in(&m, 0).await.expect("v1");
    assert_eq!(w.post_in(&m2, 1).await.expect("v2").0, StatusCode::CREATED);
    // Another catalog 1: refused. A catalog 2 that drops a dataset: refused,
    // though signed and in the book (built by hand: `Catalog::build` refuses it).
    // (Under one key the book takes no second catalog 1: an equivocation
    // disputes both; two anchored ones fork, THREATS CAT-6.)
    let other1 = catalog_of(Some(&c0), T0 + 31, vec![CatalogLeaf::of(&m2.header)]);
    assert_eq!(
        code(
            w.post_catalog(&other1, &sign_catalog(&other1))
                .await
                .expect_err("conflict")
        ),
        "bulk_not_anchored"
    );
    let leaves = vec![CatalogLeaf::of(&m2.header)];
    let dropped = Catalog {
        header: cat::CatalogHeader {
            format: cat::FORMAT,
            publisher: publisher(),
            number: 2,
            prev: c1.header.root(),
            created_unix: T0 + 50,
            leaf_count: 1,
            leaves_root: cat::leaves_tree(&leaves).root(),
        },
        leaves,
    };
    // Not anchored: refused as it is (anchored, it forks: THREATS CAT-6).
    assert_eq!(
        code(
            w.post_catalog(&dropped, &sign_catalog(&dropped))
                .await
                .expect_err("dropped")
        ),
        "bulk_catalog_not_successor"
    );
    assert_eq!(
        w.post_catalog(&c2, &sign_catalog(&c2)).await.expect("c2").0,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_r1_m1_no_second_root_for_one_version() {
    // The book holds the manifest's own report for version 1 with root A; a
    // catalog lists root B for version 1: the node refuses B's manifest.
    let w = world();
    let a = manifest(&shard_files(0..30, 10), None, T0);
    w.node.anchor(&sign(&a));
    let b = manifest(&shard_files(100..130, 10), None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&b.header)]);
    w.node.anchor(&sign_catalog(&c0));
    w.post_catalog(&c0, &sign_catalog(&c0))
        .await
        .expect("catalog");
    assert_eq!(
        code(w.post_in(&b, 0).await.expect_err("refused")),
        "bulk_conflicting_anchor"
    );
    // A node that holds A refuses the catalog itself.
    let w2 = world();
    published(&w2).await;
    let held = w2
        .store
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    assert_eq!(held["versions"][0]["version"], json!(1));
    let c = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&b.header)]);
    w2.node.anchor(&sign_catalog(&c));
    assert_eq!(
        code(
            w2.post_catalog(&c, &sign_catalog(&c))
                .await
                .expect_err("refused")
        ),
        "bulk_conflicting_anchor"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
    let _ = std::fs::remove_dir_all(&w2.dir);
}

#[tokio::test]
async fn cat_r1_l2_a_dataset_enters_after_its_own_versions() {
    // The refresh path of the first publisher: version 1 has its own report;
    // catalog 0 lists version 2, naming version 1's root.
    let w = world();
    let (m1, _) = published(&w).await;
    let files2 = shard_files(30..40, 10);
    let m2 = manifest(&files2, Some(&m1), T0 + 20);
    let c0 = catalog_of(None, T0 + 30, vec![CatalogLeaf::of(&m2.header)]);
    w.node.anchor(&sign_catalog(&c0));
    w.post_catalog(&c0, &sign_catalog(&c0))
        .await
        .expect("catalog");
    assert_eq!(w.post_in(&m2, 0).await.expect("v2").0, StatusCode::CREATED);
    for (f, e) in files2.iter().zip(&m2.shards[m1.shards.len()..]) {
        assert_eq!(
            w.put(&e.root, f.clone()).await.expect("ok").0,
            StatusCode::CREATED
        );
    }
    let a = w.rows("from=28&to=33").await.expect("ok");
    let ck = onyx_bulk::wire::check_rows_answer(&a, Some(28), Some(33)).expect("client check");
    assert_eq!(ck.header.version, 2);
    assert!(ck.catalog.is_some());
    // A dataset entering at version 2 that this node does not hold version 1 of.
    let w2 = world();
    let c = catalog_of(None, T0 + 30, vec![CatalogLeaf::of(&m2.header)]);
    w2.node.anchor(&sign_catalog(&c));
    assert_eq!(
        code(
            w2.post_catalog(&c, &sign_catalog(&c))
                .await
                .expect_err("refused")
        ),
        "bulk_catalog_entry"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
    let _ = std::fs::remove_dir_all(&w2.dir);
}

/// A dataset named `name` with the rows of `keys` (its manifest alone).
async fn post_named(w: &World, name: &str, keys: std::ops::Range<i128>) {
    let files = shard_files(keys, 10);
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| ShardEntry::new(i as u32, &check_shard(&schema(), f).expect("ok").summary))
        .collect();
    let b = Manifest::build(
        NewVersion {
            dataset: name.into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: T0,
            schema: schema(),
            rights: rights(),
        },
        None,
        entries,
    )
    .expect("ok");
    let rb = sign(&b);
    w.node.anchor(&rb);
    w.post(&b, &rb).await.expect("ok");
}

/// READINESS A20: the list and the info say how fresh a dataset is. A key
/// that reads as unix seconds gives `newest` (its UTC day) and
/// `newest_age_days` against the node's clock at the answer; `fresh=30d`
/// keeps a dataset to the last second of its 30th day and `sort=newest`
/// orders by the newest key. A key that is not a time has no `newest`, is
/// never in a `fresh` list and sorts last. Nothing is built or read from the
/// chain for it.
#[tokio::test]
async fn the_list_says_how_fresh_each_dataset_is() {
    const DAY: i128 = 86_400;
    let w = world();
    let t0 = i128::from(T0);
    // Newest keys: 2 days back; 31 days less a second back; 80 days back;
    // and keys 0..30, which are no time.
    let edge = t0 - 31 * DAY + 1;
    post_named(&w, "synthetic.fresh", t0 - 2 * DAY..t0 - 2 * DAY + 3).await;
    post_named(&w, "synthetic.edge", edge - 2..edge + 1).await;
    post_named(&w, "synthetic.old", t0 - 80 * DAY - 2..t0 - 80 * DAY + 1).await;
    post_named(&w, "synthetic.heights", 0..30).await;
    let (reads, jobs) = (w.node.reads(), w.store.succ_jobs());
    let list = |q: &str| {
        w.store
            .list(&parse_list_query(Some(q)).expect(q))
            .expect("ok")
    };
    let ids = |v: &Value| -> Vec<String> {
        v["datasets"]
            .as_array()
            .expect("ok")
            .iter()
            .map(|d| {
                let d = d["dataset"].as_str().expect("ok");
                d.trim_start_matches("synthetic.").to_string()
            })
            .collect()
    };
    let entry = |v: &Value, d: &str| {
        v["datasets"]
            .as_array()
            .expect("list")
            .iter()
            .find(|e| e["dataset"] == format!("synthetic.{d}"))
            .cloned()
            .expect("listed")
    };
    let all = list("");
    assert_eq!(ids(&all), ["edge", "fresh", "heights", "old"], "id order");
    assert_eq!(all["now_unix"], json!(T0));
    let f = entry(&all, "fresh");
    assert_eq!(
        (f["newest"].clone(), f["newest_age_days"].clone()),
        (json!("2027-01-13"), json!(1))
    );
    let e = entry(&all, "edge");
    assert_eq!(
        (e["newest"].clone(), e["newest_age_days"].clone()),
        (json!("2026-12-15"), json!(30))
    );
    let o = entry(&all, "old");
    assert_eq!(
        (o["newest"].clone(), o["newest_age_days"].clone()),
        (json!("2026-10-27"), json!(80))
    );
    // Not a time: no field at all, so nothing can call it stale.
    let h = entry(&all, "heights");
    assert!(h.get("newest").is_none() && h.get("newest_age_days").is_none());
    // fresh: the edge is in at 30 days and 86,399 seconds.
    let fresh = list("fresh=30d");
    assert_eq!(ids(&fresh), ["edge", "fresh"]);
    assert_eq!(fresh["total"], json!(2));
    assert_eq!(ids(&list("fresh=1d")), ["fresh"]);
    assert_eq!(ids(&list("fresh=3650d")), ["edge", "fresh", "old"]);
    assert_eq!(ids(&list("fresh=30d&q=old")).len(), 0);
    // sort=newest: the newest key first, no time last; pages follow it.
    assert_eq!(
        ids(&list("sort=newest")),
        ["fresh", "edge", "old", "heights"]
    );
    assert_eq!(ids(&list("sort=id")), ids(&all));
    let p1 = list("sort=newest&limit=2");
    assert_eq!(ids(&p1), ["fresh", "edge"]);
    assert_eq!(p1["total"], json!(4));
    assert_eq!(p1["next"], json!("example.publisher/synthetic.edge"));
    let p2 = list("sort=newest&limit=2&after=example.publisher%2Fsynthetic.edge");
    assert_eq!(ids(&p2), ["old", "heights"]);
    assert_eq!(p2["next"], Value::Null);
    let p3 = list("sort=newest&limit=1&after=example.publisher%2Fsynthetic.old");
    assert_eq!(ids(&p3), ["heights"]);
    assert_eq!(
        ids(&list(
            "sort=newest&fresh=30d&limit=1&after=example.publisher%2Fsynthetic.fresh"
        )),
        ["edge"]
    );
    let gone = w
        .store
        .list(
            &parse_list_query(Some("sort=newest&after=example.publisher%2Fnot.held")).expect("ok"),
        )
        .expect_err("no such cursor");
    assert_eq!(gone.status, StatusCode::BAD_REQUEST);
    // The info says the same.
    let info = w
        .store
        .info("example.publisher", "synthetic.edge")
        .expect("ok");
    assert_eq!(
        (info["newest"].clone(), info["newest_age_days"].clone()),
        (json!("2026-12-15"), json!(30))
    );
    let info = w
        .store
        .info("example.publisher", "synthetic.heights")
        .expect("ok");
    assert!(info.get("newest").is_none() && info.get("newest_age_days").is_none());
    // One second on the node's clock: the edge is 31 days behind. The
    // answer follows the clock at the answer, nothing is kept from before.
    w.clock.fetch_add(1, Ordering::Relaxed);
    let later = list("fresh=30d");
    assert_eq!(ids(&later), ["fresh"]);
    assert_eq!(later["now_unix"], json!(T0 + 1));
    assert_eq!(
        entry(&list(""), "edge")["newest_age_days"],
        json!(31),
        "the day and the age, not a kept answer"
    );
    assert_eq!(
        w.store
            .info("example.publisher", "synthetic.edge")
            .expect("ok")["newest_age_days"],
        json!(31)
    );
    assert_eq!(ids(&list("fresh=31d")), ["edge", "fresh"]);
    // A newest key up to a day ahead of the clock: age 0, never negative.
    w.clock.store(T0 - 3 * 86_400 + 2, Ordering::Relaxed);
    let ahead = entry(&list(""), "fresh");
    assert_eq!(
        (ahead["newest"].clone(), ahead["newest_age_days"].clone()),
        (json!("2027-01-13"), json!(0))
    );
    assert!(ahead.get("newest_in_future").is_none());
    assert_eq!(ids(&list("fresh=1d")), ["fresh"]);
    // Review FRESH-R1 M1: one second more and the key is dated in the
    // future. A publisher signs its own key range, so such a key is no
    // news: no day, no age, in no fresh list, sorted with the keys that are
    // no time, and marked.
    w.clock.store(T0 - 3 * 86_400 + 1, Ordering::Relaxed);
    let all = list("");
    let e = entry(&all, "fresh");
    assert_eq!(e["newest_in_future"], json!(true));
    assert!(e.get("newest").is_none() && e.get("newest_age_days").is_none());
    for d in ["edge", "old", "heights"] {
        assert!(entry(&all, d).get("newest_in_future").is_none(), "{d}");
    }
    assert_eq!(list("fresh=1d")["total"], json!(0));
    assert_eq!(ids(&list("fresh=3650d")), ["edge", "old"]);
    assert_eq!(ids(&list("sort=newest&fresh=3650d")), ["edge", "old"]);
    assert_eq!(
        ids(&list("sort=newest")),
        ["edge", "old", "fresh", "heights"],
        "a key of the future never tops the newest"
    );
    let info = w
        .store
        .info("example.publisher", "synthetic.fresh")
        .expect("ok");
    assert_eq!(info["newest_in_future"], json!(true));
    assert!(info.get("newest").is_none() && info.get("newest_age_days").is_none());
    // Between `old` (80 days back) and `edge`: `old` is the only dated one,
    // so it leads; the future ones follow in id order with `heights`.
    w.clock.store(T0 - 60 * 86_400, Ordering::Relaxed);
    assert_eq!(
        ids(&list("sort=newest")),
        ["old", "edge", "fresh", "heights"]
    );
    assert_eq!(ids(&list("fresh=3650d")), ["old"]);
    let p2 = list("sort=newest&limit=2&after=example.publisher%2Fsynthetic.edge");
    assert_eq!(ids(&p2), ["fresh", "heights"]);
    // Every listing above came from the held signed headers: no chain
    // read, no index build.
    assert_eq!((w.node.reads(), w.store.succ_jobs()), (reads, jobs));
    for bad in [
        "fresh=30",
        "fresh=d",
        "fresh=0d",
        "fresh=3651d",
        "fresh=-1d",
        "fresh=1.5d",
        "fresh=%2B5d",
        "fresh=30d&fresh=7d",
        "sort=",
        "sort=rows",
        "sort=NEWEST",
    ] {
        let e = parse_list_query(Some(bad)).expect_err(bad);
        assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// The day of a key and its age in whole days (READINESS A20).
#[test]
fn a_newest_key_reads_as_a_day_and_an_age() {
    assert_eq!(iso_day(0), "1970-01-01");
    assert_eq!(iso_day(86_399), "1970-01-01");
    assert_eq!(iso_day(951_782_400), "2000-02-29");
    assert_eq!(iso_day(1_794_528_000), "2026-11-13");
    assert_eq!(iso_day(4_102_444_799), "2099-12-31");
    for t in [100_000_000i128, 1_709_164_800, 1_767_225_600, 1_798_761_599] {
        assert_eq!(parse_time(&iso_day(t), false), Some(t - t % 86_400));
    }
    assert_eq!(age_days(1_000_000, 1_000_000), 0);
    assert_eq!(age_days(1_000_000, 1_000_000 + 86_399), 0);
    assert_eq!(age_days(1_000_000, 1_000_000 + 86_400), 1);
    assert_eq!(age_days(2_000_000, 1_000_000), 0);
}

/// `GET /v1/bulk` search and pages: every word must be in
/// `publisher/dataset` (any case), pages follow id order through `next`,
/// `total` counts every match, and a bad query is a 400.
#[tokio::test]
async fn the_dataset_list_searches_and_pages() {
    let w = world();
    let (m, _) = published(&w).await;
    for name in ["synthetic.alpha", "synthetic.copy2"] {
        let b = Manifest::build(
            NewVersion {
                dataset: name.into(),
                publisher: SourceId("example.publisher".into()),
                created_unix: T0,
                schema: schema(),
                rights: rights(),
            },
            None,
            m.shards.clone(),
        )
        .expect("ok");
        let rb = sign(&b);
        w.node.anchor(&rb);
        w.post(&b, &rb).await.expect("ok");
    }
    let list = |q: &str| {
        w.store
            .list(&parse_list_query(Some(q)).expect("ok"))
            .expect("ok")
    };
    let ids = |v: &Value| -> Vec<String> {
        v["datasets"]
            .as_array()
            .expect("ok")
            .iter()
            .map(|d| d["dataset"].as_str().expect("ok").to_string())
            .collect()
    };
    let all = list("");
    assert_eq!(
        ids(&all),
        ["synthetic.alpha", "synthetic.copy2", "synthetic.metrics"]
    );
    assert_eq!(
        (all["total"].clone(), all["next"].clone()),
        (json!(3), Value::Null)
    );
    assert_eq!(all["datasets"][2]["stored_shards"], json!(3));
    assert_eq!(all["datasets"][2]["created_unix"], json!(T0));
    assert_eq!(ids(&list("q=alpha")), ["synthetic.alpha"]);
    assert_eq!(ids(&list("q=SYNTHETIC+met")), ["synthetic.metrics"]);
    assert_eq!(
        ids(&list("q=example.publisher%2Fsynthetic.c")),
        ["synthetic.copy2"]
    );
    assert_eq!(list("q=nothing")["total"], json!(0));
    let p1 = list("limit=2");
    assert_eq!(ids(&p1), ["synthetic.alpha", "synthetic.copy2"]);
    assert_eq!(p1["total"], json!(3));
    assert_eq!(p1["next"], json!("example.publisher/synthetic.copy2"));
    let p2 = list("limit=2&after=example.publisher%2Fsynthetic.copy2");
    assert_eq!(ids(&p2), ["synthetic.metrics"]);
    assert_eq!(
        (p2["total"].clone(), p2["next"].clone()),
        (json!(3), Value::Null)
    );
    assert_eq!(list("q=synthetic&limit=3")["next"], Value::Null);
    // any: whole words, a group needs one of its terms, every group must match.
    assert_eq!(ids(&list("any=alpha")), ["synthetic.alpha"]);
    assert_eq!(ids(&list("any=alph*")), ["synthetic.alpha"]);
    assert_eq!(
        list("any=alph")["total"],
        json!(0),
        "a part of a word is not a word"
    );
    assert_eq!(
        ids(&list("any=alpha%2Cmetrics")),
        ["synthetic.alpha", "synthetic.metrics"]
    );
    assert_eq!(
        ids(&list("any=synthetic%3Balpha,copy2")),
        ["synthetic.alpha", "synthetic.copy2"]
    );
    assert_eq!(
        list("any=example-publisher")["total"],
        json!(3),
        "words next to each other"
    );
    assert_eq!(
        list("any=publisher-example")["total"],
        json!(0),
        "in their order"
    );
    assert_eq!(ids(&list("q=synth&any=METRICS")), ["synthetic.metrics"]);
    let too_long = format!("q={}", "a".repeat(MAX_SEARCH_CHARS + 1));
    for bad in [
        "q=a<b",
        too_long.as_str(),
        "limit=0",
        "limit=501",
        "after=nope",
        "after=a/b/c",
        "q=a&q=b",
        "sort=rows",
        "q=%zz",
        "q=%ff",
        "any=",
        "any=a;;b",
        "any=a+b",
        "any=a*b",
        "any=-a",
        "any=a;b;c;d;e;f;g;h;i",
    ] {
        let e = parse_list_query(Some(bad)).expect_err(bad);
        assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let app = routes(w.store.clone());
    let r = app
        .oneshot(
            Request::get("/v1/bulk?q=alpha&limit=1")
                .body(Body::empty())
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&r.into_body().collect().await.expect("ok").to_bytes()).expect("ok");
    assert_eq!(ids(&v), ["synthetic.alpha"]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// SCALE-3: only the newest catalog of a publisher is held whole; an older
/// one is read from its file when asked (a manifest uploaded against it, the
/// peer route), checked against the header taken at upload, and a restart
/// keeps the same view.
#[tokio::test]
async fn scale_3_older_catalogs_are_read_from_their_files() {
    // Every catalog whole (the files these checks change by hand).
    let w = world_every(1);
    let m = manifest(&shard_files(0..30, 10), None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![CatalogLeaf::of(&m.header), other_leaf("synthetic.new")],
    );
    for c in [&c0, &c1] {
        w.node.anchor(&sign_catalog(c));
        w.post_catalog(c, &sign_catalog(c)).await.expect("catalog");
    }
    let whole = |s: &BulkStore| -> Vec<u64> {
        let cs = s.catalogs.read().expect("lock");
        cs.values()
            .flat_map(|m| m.iter().filter(|(_, h)| h.body.is_some()).map(|(n, _)| *n))
            .collect()
    };
    assert_eq!(
        whole(&w.store),
        vec![1],
        "only the newest catalog is held whole"
    );
    // A manifest uploaded against the older catalog 0 is taken (catalog read from its file).
    assert_eq!(w.post_in(&m, 0).await.expect("in 0").0, StatusCode::CREATED);
    assert_eq!(
        w.store
            .catalog_record(&publisher().0, 0)
            .expect("rebuilt")
            .map(|(c, _)| c),
        Some(c0.clone())
    );
    assert_eq!(
        w.store
            .catalog_record(&publisher().0, 1)
            .expect("rebuilt")
            .map(|(c, _)| c),
        Some(c1.clone())
    );
    // After a restart: the same view, and still one catalog whole.
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0 + 30),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("reopen");
    assert_eq!(whole(&again), vec![1]);
    assert_eq!(
        again
            .catalog_record(&publisher().0, 0)
            .expect("rebuilt")
            .map(|(c, _)| c),
        Some(c0.clone())
    );
    // A file changed after it was taken is never served as that catalog.
    let f = w.dir.join("catalogs").join(&publisher().0).join("0.bin");
    let mut bytes = std::fs::read(&f).expect("read");
    let (c, r, a): (Vec<u8>, Vec<u8>, Vec<u8>) =
        BorshDeserialize::try_from_slice(&bytes).expect("rec");
    let mut other = Catalog::try_from_slice(&c).expect("catalog");
    other.header.created_unix += 1;
    bytes = borsh::to_vec(&(borsh::to_vec(&other).expect("enc"), r, a)).expect("enc");
    std::fs::write(&f, bytes).expect("write");
    // The body read before stays served (it was checked); read afresh, the
    // changed file is refused, and a restart refuses it (its signature).
    assert_eq!(
        again
            .catalog_record(&publisher().0, 0)
            .expect("rebuilt")
            .map(|(c, _)| c),
        Some(c0.clone())
    );
    again.old_body.lock().expect("lock").clear();
    assert!(again.catalog_record(&publisher().0, 0).is_err());
    assert!(BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0 + 30),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default()
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// SCALE-5: at start the two newest catalogs are checked whole; an older one
/// by its signed header and its link to the one before. Changed leaves under
/// an intact header are refused when that catalog is read; a changed header
/// or a broken link stops the start.
#[tokio::test]
async fn scale_5_older_catalogs_are_checked_by_header_at_start() {
    // Every catalog whole (the files these checks change by hand).
    let w = world_every(1);
    let m = manifest(&shard_files(0..30, 10), None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![CatalogLeaf::of(&m.header), other_leaf("synthetic.b")],
    );
    let c2 = catalog_of(Some(&c1), T0 + 30, c1.leaves.clone());
    let c3 = catalog_of(Some(&c2), T0 + 40, c2.leaves.clone());
    for c in [&c0, &c1, &c2, &c3] {
        w.node.anchor(&sign_catalog(c));
        w.post_catalog(c, &sign_catalog(c)).await.expect("catalog");
    }
    let reopen = || {
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0 + 50),
            &w.dir,
            &w.dir.join("warm"),
            BulkLimits::default(),
        )
    };
    let file = |n: u64| {
        w.dir
            .join("catalogs")
            .join(&publisher().0)
            .join(format!("{n}.bin"))
    };
    let record = |n: u64| -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        BorshDeserialize::try_from_slice(&std::fs::read(file(n)).expect("read")).expect("rec")
    };
    let write = |n: u64, c: &Catalog, r: Vec<u8>, a: Vec<u8>| {
        std::fs::write(
            file(n),
            borsh::to_vec(&(borsh::to_vec(c).expect("enc"), r, a)).expect("enc"),
        )
        .expect("write")
    };
    let s = reopen().expect("start");
    assert_eq!(
        s.catalog_record(&publisher().0, 1)
            .expect("rebuilt")
            .map(|(c, _)| c),
        Some(c1.clone())
    );
    // Catalog 1 (older): one leaf changed, header intact. The start passes;
    // reading it refuses it.
    let (_, r1, a1) = record(1);
    let mut bad = c1.clone();
    bad.leaves[1].version = 9;
    write(1, &bad, r1.clone(), a1.clone());
    let s = reopen().expect("start: the header still holds");
    assert!(s.catalog_record(&publisher().0, 1).is_err());
    // Its header changed: the signature fails, the start stops.
    let mut bad = c1.clone();
    bad.header.created_unix += 1;
    write(1, &bad, r1.clone(), a1.clone());
    assert!(reopen().is_err());
    // A validly signed catalog 1 that does not follow catalog 0: refused.
    let mut unlinked = c1.clone();
    unlinked.header.prev = Hash32([7; 32]);
    let ru = sign_catalog(&unlinked);
    write(1, &unlinked, borsh::to_vec(&ru).expect("enc"), a1.clone());
    let e = reopen().err().expect("refused");
    assert!(e.contains("does not follow catalog 0"), "{e}");
    write(1, &c1, r1, a1);
    assert!(reopen().is_ok());
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- A stolen key's catalogs (THREATS CAT-5, rules of review CAT-FIX-R1) -----------------------

/// Catalog 0 (version 1 of `synthetic.metrics`, uploaded) and a thief's
/// catalog 1 under the same key (its own version 2, uploaded, and a new
/// dataset), both anchored and held.
struct Theft {
    w: World,
    m: Manifest,
    c0: Catalog,
    c1: Catalog,
    r1: SignedReport,
    bad: Manifest,
}

async fn theft() -> Theft {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    w.upload_all(&m, &files).await;
    let bad_files = shard_files(30..40, 10);
    let bad = manifest(&bad_files, Some(&m), T0 + 20);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 30,
        vec![CatalogLeaf::of(&bad.header), other_leaf("synthetic.thief")],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    w.post_in(&bad, 1).await.expect("the thief's version 2");
    w.upload_all(&bad, &bad_files).await;
    Theft {
        w,
        m,
        c0,
        c1,
        r1,
        bad,
    }
}

/// The publisher's replacement catalog 1 under its new key, anchored, with
/// its own version 2.
fn replacement(t: &Theft) -> (Catalog, SignedReport, Manifest, Vec<Vec<u8>>) {
    let files = shard_files(30..45, 10);
    let good = manifest(&files, Some(&t.m), T0 + 40);
    let c = catalog_of(Some(&t.c0), T0 + 50, vec![CatalogLeaf::of(&good.header)]);
    let r = cat::sign(&SECRET2, &c.header).expect("ok");
    t.w.node.anchor_by(&r, &SECRET2);
    (c, r, good, files)
}

fn held_root(w: &World, n: u64) -> Option<Hash32> {
    w.store
        .catalog_record("example.publisher", n)
        .expect("rebuilt")
        .map(|(c, _)| c.header.root())
}

fn served_versions(w: &World) -> Vec<u64> {
    let info = w
        .store
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    info["versions"]
        .as_array()
        .expect("ok")
        .iter()
        .map(|v| v["version"].as_u64().expect("ok"))
        .collect()
}

/// ADR-0059 amendment 4 (THREATS CAT-3): a catalog with a tombstone removes
/// its dataset from the rows, info and listing, and every upload path
/// refuses it (an old manifest through an older catalog, an own report, a
/// shard), with 410 naming the catalog; the bytes stay held and the peer
/// routes by root still answer (storage deals, the history check); the other
/// datasets serve on; a restart keeps the removal.
#[tokio::test]
async fn cat_3_a_tombstone_removes_a_dataset_but_keeps_its_bytes() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let keep = other_leaf("synthetic.other");
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), keep.clone()],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    w.upload_all(&m, &files).await;
    w.rows("from=0&to=3").await.expect("served");
    let gone = CatalogLeaf::of(&m.header).tombstone().expect("tombstone");
    let c1 = catalog_of(Some(&c0), T0 + 20, vec![gone, keep]);
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1).await.expect("c1").0,
        StatusCode::CREATED
    );
    let removed = |e: ApiError| {
        assert_eq!(e.status, StatusCode::GONE);
        assert_eq!(e.code, "bulk_dataset_removed");
    };
    removed(w.rows("from=0&to=3").await.expect_err("removed"));
    removed(
        w.store
            .info_checked("example.publisher", "synthetic.metrics")
            .await
            .expect_err("removed"),
    );
    removed(w.post_in(&m, 0).await.expect_err("not again"));
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        Some(1)
    );
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.other")
            .expect("read"),
        None
    );
    // Through the routes.
    let read = routes(w.store.clone());
    let up = upload_routes(w.store.clone());
    let peer = peer_routes(w.store.clone());
    let send = |app: Router, req: Request<Body>| async move {
        let r = app.oneshot(req).await.expect("ok");
        let status = r.status();
        let b = r.into_body().collect().await.expect("ok").to_bytes();
        (
            status,
            serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
        )
    };
    let get = |u: String| Request::get(u).body(Body::empty()).expect("ok");
    for uri in [
        "/v1/bulk/example.publisher/synthetic.metrics".to_string(),
        "/v1/bulk/example.publisher/synthetic.metrics/rows?from=0&to=3".to_string(),
    ] {
        let (s, v) = send(read.clone(), get(uri.clone())).await;
        assert_eq!(s, StatusCode::GONE, "{uri}");
        assert_eq!(v["error"]["code"], json!("bulk_dataset_removed"));
        assert_eq!(v["catalog"], json!(1));
        crate::openapi_check::check("GET", &uri, 410, &v);
    }
    let (s, v) = send(read.clone(), get("/v1/bulk".into())).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !v.to_string().contains("synthetic.metrics"),
        "left out of the list"
    );
    let post = |body: Value| {
        Request::post("/v1/bulk/manifests")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("ok")
    };
    for body in [
        json!({"manifest": hex0x(&m.to_bytes()), "catalog": 0}),
        json!({"manifest": hex0x(&m.to_bytes()), "report": hex0x(&borsh::to_vec(&sign(&m)).expect("ok"))}),
    ] {
        let (s, v) = send(up.clone(), post(body)).await;
        assert_eq!(s, StatusCode::GONE);
        assert_eq!(v["error"]["code"], json!("bulk_dataset_removed"));
        crate::openapi_check::check("POST", "/v1/bulk/manifests", 410, &v);
    }
    let shard_uri = format!(
        "/v1/bulk/example.publisher/synthetic.metrics/shards/{}",
        m.shards[0].root
    );
    let (s, v) = send(
        up.clone(),
        Request::put(shard_uri.clone())
            .body(Body::from(files[0].clone()))
            .expect("ok"),
    )
    .await;
    assert_eq!(s, StatusCode::GONE);
    crate::openapi_check::check("PUT", &shard_uri, 410, &v);
    // The bytes stay, and the peer routes by root answer.
    assert!(w.store.holds_shard(&m.shards[0].root));
    let (s, _) = send(
        peer.clone(),
        get(format!("/v1/bulk/manifests/{}", m.header.root())),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let r = peer
        .clone()
        .oneshot(get(format!("/v1/bulk/shards/{}", m.shards[0].root)))
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    // A storage node's own fetch of a deal's manifest (an own-report
    // manifest, ADR-0060) is not refused for the removal.
    let own = sign(&m);
    w.node.anchor(&own);
    if let Err(e) = w.post(&m, &own).await {
        assert_ne!(
            e.code, "bulk_dataset_removed",
            "the storage path: {}",
            e.message
        );
    }
    // A restart keeps the removal.
    let again = w.reopen().expect("starts");
    assert_eq!(
        again
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        Some(1)
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Own-report versions 1 and 2 of `synthetic.metrics`, held and anchored.
async fn own_two(w: &World) -> (Manifest, Manifest) {
    let f1 = shard_files(0..30, 10);
    let m1 = manifest(&f1, None, T0);
    let f2 = shard_files(30..40, 10);
    let m2 = manifest(&f2, Some(&m1), T0 + 5);
    for (m, f) in [(&m1, &f1), (&m2, &f2)] {
        let r = sign(m);
        w.node.anchor(&r);
        assert_eq!(w.post(m, &r).await.expect("own").0, StatusCode::CREATED);
        w.upload_all(m, f).await;
    }
    (m1, m2)
}

/// Review of the CAT-3 build, finding 1 (amendment 4 items 1 and 4): a
/// dataset with own-report versions enters a catalog as a tombstone over the
/// version 2 the node holds with another root: taken (not
/// `bulk_conflicting_anchor`), and the dataset is removed.
#[tokio::test]
async fn cat_3_a_tombstone_over_a_held_own_version_is_taken() {
    let w = world();
    let (m1, _m2) = own_two(&w).await;
    let t = CatalogLeaf::of(&m1.header).tombstone().expect("t");
    assert_eq!(t.version, 2);
    let c0 = catalog_of(None, T0 + 10, vec![t]);
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("taken").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        Some(0)
    );
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("removed")),
        "bulk_dataset_removed"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review of the CAT-3 build, finding 1 (the set-aside path, `plan_cut`):
/// after a key theft, the publisher's replacement catalog removes the
/// dataset with a tombstone over an own-report version the book has not
/// withdrawn: taken, not refused as a contradicted version.
#[tokio::test]
async fn cat_3_a_replacement_may_remove_over_a_live_own_version() {
    let w = world();
    let (m1, m2) = own_two(&w).await;
    // Catalog 0 lists version 1; version 2 is held by its own report.
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m1.header)]);
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    let c1 = catalog_of(
        Some(&c0),
        T0 + 30,
        vec![CatalogLeaf::of(&m2.header), other_leaf("synthetic.thief")],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("the thief's c1");
    // The replacement catalog 1 under the new key ends the dataset at
    // version 2, over the held own version 2 (live, not withdrawn).
    let t = CatalogLeaf::of(&m1.header).tombstone().expect("t");
    assert_eq!(t.version, 2);
    let c1b = catalog_of(Some(&c0), T0 + 50, vec![t]);
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    w.node.anchor_by(&r1b, &SECRET2);
    w.node.withdraw(&r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        Some(1)
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Launch checklist LC-17: the listing names each publisher's newest held
/// catalog (number, root, creation time, leaves, tombstones), whatever the
/// query, so a watch sees a refresh that stopped.
#[tokio::test]
async fn lc_17_the_listing_names_each_publishers_newest_catalog() {
    let w = world();
    let (cs, _) = walked(&w, 2).await;
    let (c2, _) = cs.last().expect("c2");
    // `moved` comes from a built links index (null before: ADR-0059
    // amendment 6, review SUCC-B2 L4).
    w.settle().await;
    let r = routes(w.store.clone())
        .oneshot(
            Request::get("/v1/bulk?q=nothing.matches&limit=1")
                .body(Body::empty())
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&r.into_body().collect().await.expect("ok").to_bytes()).expect("ok");
    crate::openapi_check::check("GET", "/v1/bulk", 200, &v);
    assert_eq!(v["total"], json!(0));
    assert_eq!(
        v["catalogs"],
        json!([{
            "publisher": "example.publisher",
            "number": 2,
            "root": c2.header.root().to_string(),
            "createdUnix": c2.header.created_unix,
            "leaves": c2.header.leaf_count,
            "removed": 0,
            "moved": 0,
            "erase": null,
        }])
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// ADR-0059 amendment 4 item 4 (review CAT-TOMB-R1 M5): a tombstone is taken
/// even when the node holds another root at its version; and item 2: a
/// set-aside of the tombstone's catalog brings the dataset back.
#[tokio::test]
async fn cat_3_a_tombstone_is_always_taken_and_a_set_aside_undoes_it() {
    let t = theft().await;
    let w = &t.w;
    // The thief's catalog 1 lists version 2 (held); the next catalog
    // removes the dataset at version 3, over nothing held there.
    let c2 = catalog_of(
        Some(&t.c1),
        T0 + 40,
        vec![
            CatalogLeaf::of(&t.bad.header).tombstone().expect("t"),
            other_leaf("synthetic.thief"),
        ],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    assert_eq!(
        w.post_catalog(&c2, &r2).await.expect("c2").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        Some(2)
    );
    // The book withdraws the thief's catalogs 1 and 2; the publisher's
    // replacement catalog 1 brings the dataset back with its own version 2.
    let (c1b, r1b, good, files) = replacement(&t);
    w.node.withdraw(&t.r1);
    w.node.withdraw(&r2);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        None
    );
    w.post_in(&good, 1).await.expect("v2");
    w.upload_all(&good, &files).await;
    w.rows("from=0&to=3").await.expect("served again");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_a_withdrawn_catalog_is_set_aside_for_the_publishers_replacement() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, good, files) = replacement(&t);
    // While the book still holds the thief's catalog, two anchored catalogs
    // 1 are a fork (THREATS CAT-6): the publisher's catalog datasets stop.
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("held")),
        "bulk_publisher_forked"
    );
    assert_eq!(held_root(w, 1), Some(t.c1.header.root()));
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("forked")),
        "bulk_dataset_forked"
    );
    // The reviewer marks the stolen key compromised from before catalog 1:
    // the book withdraws it, the fork clears at the next read, and the
    // replacement takes its place.
    w.node.withdraw(&t.r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(held_root(w, 1), Some(c1b.header.root()));
    assert_eq!(held_root(w, 0), Some(t.c0.header.root()));
    // The thief's version 2 is gone (and its files kept aside); version 1 serves.
    assert_eq!(served_versions(w), vec![1]);
    let ds = w.dir.join("manifests/example.publisher/synthetic.metrics");
    let aside = |f: String| ds.join("withdrawn").join(f);
    assert!(aside(format!("v2-{}.bin", t.bad.header.root().to_hex())).exists());
    assert!(aside(format!("v2-{}.catalog.bin", t.bad.header.root().to_hex())).exists());
    assert!(!ds.join("v2.bin").exists() && !ds.join("v2.catalog.bin").exists());
    assert!(w
        .dir
        .join(format!(
            "catalogs/example.publisher/withdrawn/1-{}.diff",
            t.c1.header.root().to_hex()
        ))
        .exists());
    let a = w.rows("from=0&to=3").await.expect("v1 serves");
    assert_eq!(
        onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(3))
            .expect("client check")
            .catalog
            .map(|h| h.number),
        Some(0)
    );
    // The thief's version can no longer come back through the replacement;
    // the publisher's own version 2 enters through it.
    assert_eq!(
        code(w.post_in(&t.bad, 1).await.expect_err("not listed")),
        "bulk_not_in_catalog"
    );
    assert_eq!(
        w.post_in(&good, 1).await.expect("v2").0,
        StatusCode::CREATED
    );
    w.upload_all(&good, &files).await;
    assert_eq!(served_versions(w), vec![1, 2]);
    // The thief's catalog again: the book no longer holds it, known
    // without a chain read (review of CAT-5, L1).
    let before = w.node.reads();
    assert_eq!(
        code(w.post_catalog(&t.c1, &t.r1).await.expect_err("withdrawn")),
        "bulk_not_anchored"
    );
    assert_eq!(w.node.reads(), before);
    // A restart loads the same state.
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("reopen");
    assert_eq!(
        again
            .catalog_record("example.publisher", 1)
            .expect("rebuilt")
            .map(|(c, _)| c.header.root()),
        Some(c1b.header.root())
    );
    let info = again
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    assert_eq!(
        info["manifest"]["root"],
        json!(good.header.root().to_string())
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_temporary_refusals_set_nothing_aside() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, _, _) = replacement(&t);
    let still_held = |w: &World| {
        assert_eq!(held_root(w, 1), Some(t.c1.header.root()));
        assert_eq!(served_versions(w), vec![1, 2]);
    };
    // An open dispute can be resolved back (review CAT-FIX-R1 H1).
    w.node.dispute(&t.r1);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("disputed")),
        "bulk_catalog_conflict"
    );
    still_held(w);
    // A suspended key can be reinstated.
    w.node.anchor(&t.r1);
    w.node.suspend(&t.r1, true);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("suspended")),
        "bulk_catalog_conflict"
    );
    still_held(w);
    w.node.suspend(&t.r1, false);
    // A finalized block behind the one catalog 1 was taken at: try again.
    w.node.withdraw(&t.r1);
    w.node.0.lock().expect("ok").fin = Some(0x63);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("lagging")),
        "bulk_chain_unavailable"
    );
    still_held(w);
    w.node.0.lock().expect("ok").fin = None;
    // A replacement the book does not hold costs one read of its own report
    // (chain id, finalized block, `report`), none of the held catalog.
    let files = shard_files(30..41, 10);
    let other = manifest(&files, Some(&t.m), T0 + 60);
    let c1c = catalog_of(Some(&t.c0), T0 + 70, vec![CatalogLeaf::of(&other.header)]);
    let r1c = cat::sign(&SECRET2, &c1c.header).expect("ok");
    let before = w.node.reads();
    assert_eq!(
        code(w.post_catalog(&c1c, &r1c).await.expect_err("unanchored")),
        "bulk_not_anchored"
    );
    assert_eq!(w.node.reads() - before, 3);
    still_held(w);
    // The book's final word: set aside.
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(served_versions(w), vec![1]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_every_catalog_from_the_number_must_be_withdrawn() {
    let t = theft().await;
    let w = &t.w;
    // The thief also took catalog 2.
    let c2 = catalog_of(Some(&t.c1), T0 + 35, t.c1.leaves.clone());
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    w.post_catalog(&c2, &r2).await.expect("c2");
    let (c1b, r1b, _, _) = replacement(&t);
    // Catalog 1 withdrawn, catalog 2 only disputed: nothing moves.
    w.node.withdraw(&t.r1);
    w.node.dispute(&r2);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("2 holds")),
        "bulk_catalog_conflict"
    );
    assert_eq!(held_root(w, 2), Some(c2.header.root()));
    assert_eq!(served_versions(w), vec![1, 2]);
    // Both withdrawn: both set aside, newest first, and the replacement is
    // catalog 1.
    w.node.withdraw(&r2);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(held_root(w, 1), Some(c1b.header.root()));
    assert_eq!(held_root(w, 2), None);
    assert_eq!(served_versions(w), vec![1]);
    // The publisher's next catalog takes number 2 again.
    let c3 = catalog_of(Some(&c1b), T0 + 90, c1b.leaves.clone());
    let r3 = cat::sign(&SECRET2, &c3.header).expect("ok");
    w.node.anchor_by(&r3, &SECRET2);
    assert_eq!(
        w.post_catalog(&c3, &r3).await.expect("next").0,
        StatusCode::CREATED
    );
    assert_eq!(held_root(w, 2), Some(c3.header.root()));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_only_the_books_final_word_counts_as_withdrawn() {
    let w = world();
    let c = catalog_of(None, T0 + 10, vec![other_leaf("synthetic.a")]);
    let r = sign_catalog(&c);
    let signer = onyx_bulk::anchor::address_of(&SECRET).expect("ok");
    let read = || async {
        let at = decision_block(w.node.as_ref(), CHAIN).await.expect("block");
        let x = read_anchor_at(w.node.as_ref(), CHAIN, &BOOK, &r, &signer, at)
            .await
            .expect("read");
        let why = match x.state {
            AnchorState::Anchored(_) => "anchored",
            AnchorState::Not(why) => why,
        };
        (why, x.withdrawn)
    };
    // Not recorded yet (or a lagging node): not final.
    assert_eq!(read().await, (reason::NOT_FINAL, false));
    w.node.anchor(&r);
    assert_eq!(read().await, ("anchored", false));
    let fact = keccak256(&r.payload.digest().0);
    let set_fact = |state: u8, accepted: Hash32| {
        w.node
            .0
            .lock()
            .expect("ok")
            .facts
            .insert(fact, (state, accepted, T0));
    };
    // Withdrawn (key retired or compromised) or a dispute cleared: final.
    set_fact(0, Hash32::ZERO);
    assert_eq!(read().await, (reason::NOT_ACCEPTED, true));
    // Accepted on another report: final.
    set_fact(1, Hash32([7; 32]));
    assert_eq!(read().await, (reason::NOT_ACCEPTED, true));
    // An open dispute: not final, unless the key is compromised (the book
    // never keeps a compromised key's report, review of CAT-5 M1).
    set_fact(2, Hash32::ZERO);
    assert_eq!(read().await, (reason::NOT_ACCEPTED, false));
    w.node.life(&r, 3);
    assert_eq!(read().await, (reason::NOT_ACCEPTED, true));
    w.node.life(&r, 1);
    // A suspended key: not final.
    set_fact(1, r.hash());
    w.node.suspend(&r, true);
    assert_eq!(read().await, (reason::KEY, false));
    w.node.suspend(&r, false);
    // Another signature over the same payload: final.
    w.node
        .0
        .lock()
        .expect("ok")
        .reports
        .get_mut(&r.payload.digest())
        .expect("recorded")
        .2 = Hash32([9; 32]);
    assert_eq!(read().await, (reason::OTHER_SIGNATURE, true));
    let _ = std::fs::remove_dir_all(&w.dir);
}

fn manifest_of(dataset: &str, files: &[Vec<u8>], created: u64) -> Manifest {
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| ShardEntry::new(i as u32, &check_shard(&schema(), f).expect("ok").summary))
        .collect();
    Manifest::build(
        NewVersion {
            dataset: dataset.into(),
            publisher: publisher(),
            created_unix: created,
            schema: schema(),
            rights: rights(),
        },
        None,
        entries,
    )
    .expect("ok")
}

#[tokio::test]
async fn cat_5_review_m1_an_equivocation_then_a_compromise_is_final() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, _, _) = replacement(&t);
    // The publisher, unaware of the theft, signs its own catalog 1 under the
    // same key: the book disputes the fact and suspends the key.
    w.node.dispute(&t.r1);
    w.node.suspend(&t.r1, true);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("open")),
        "bulk_catalog_conflict"
    );
    // The reviewer marks the key compromised: no dispute can keep its report.
    w.node.life(&t.r1, 3);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(served_versions(w), vec![1]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_review_m2_a_failed_set_aside_leaves_versions_one_to_k() {
    let t = theft().await;
    let w = &t.w;
    // The thief's own report for version 3, on its catalog's version 2.
    let bad3 = manifest(&shard_files(40..50, 10), Some(&t.bad), T0 + 36);
    let r3 = sign(&bad3);
    w.node.anchor(&r3);
    w.post(&bad3, &r3).await.expect("v3");
    assert_eq!(served_versions(w), vec![1, 2, 3]);
    let (c1b, r1b, _, _) = replacement(&t);
    w.node.withdraw(&t.r1);
    w.node.withdraw(&r3);
    // Version 3 cannot move (a directory where its file goes): the upload
    // fails with nothing moved, never a gap before version 3.
    let ds = w.dir.join("manifests/example.publisher/synthetic.metrics");
    let block = ds
        .join("withdrawn")
        .join(format!("v3-{}.bin", bad3.header.root().to_hex()));
    std::fs::create_dir_all(&block).expect("ok");
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("blocked")),
        "internal"
    );
    for v in 1..=3 {
        assert!(ds.join(format!("v{v}.bin")).exists(), "v{v}");
    }
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("starts");
    assert_eq!(
        again
            .info("example.publisher", "synthetic.metrics")
            .expect("ok")["versions"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    drop(again);
    // Unblocked: the upload again finishes the set-aside.
    std::fs::remove_dir(&block).expect("ok");
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(served_versions(w), vec![1]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_review_m3_a_thiefs_own_report_version_goes_only_when_withdrawn() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    // The thief's version 2 under its own report, then its catalog 1.
    let bad = manifest(&shard_files(30..40, 10), Some(&m), T0 + 20);
    let rb = sign(&bad);
    w.node.anchor(&rb);
    w.post(&bad, &rb).await.expect("v2");
    let c1 = catalog_of(Some(&c0), T0 + 30, vec![CatalogLeaf::of(&bad.header)]);
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    let good = manifest(&shard_files(30..45, 10), Some(&m), T0 + 40);
    let c1b = catalog_of(Some(&c0), T0 + 50, vec![CatalogLeaf::of(&good.header)]);
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    w.node.anchor_by(&r1b, &SECRET2);
    // Only the catalog withdrawn: version 2's own report still holds, so
    // the replacement is refused before anything moves.
    w.node.withdraw(&r1);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("v2 holds")),
        "bulk_conflicting_anchor"
    );
    assert_eq!(held_root(&w, 1), Some(c1.header.root()));
    assert_eq!(served_versions(&w), vec![1, 2]);
    // Both withdrawn: both go, and the replacement fits.
    w.node.withdraw(&rb);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(served_versions(&w), vec![1]);
    assert_eq!(
        w.post_in(&good, 1).await.expect("v2").0,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_5_review_m4_versions_a_catalog_lists_unchanged_keep_an_anchor() {
    let w = world();
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let e = manifest_of("synthetic.even", &shard_files(100..110, 10), T0);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), CatalogLeaf::of(&e.header)],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("m v1");
    w.post_in(&e, 0).await.expect("e v1");
    let anchored_by = |w: &World| {
        w.store
            .info("example.publisher", "synthetic.even")
            .expect("ok")["anchor"]["catalog"]["number"]
            .as_u64()
    };
    // The thief's catalog 1 must list `e` unchanged (append only). Anyone may
    // post `e` under it: while catalog 0's report holds, nothing moves.
    let bad = manifest(&shard_files(30..40, 10), Some(&m), T0 + 20);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 30,
        vec![CatalogLeaf::of(&bad.header), CatalogLeaf::of(&e.header)],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    w.post_in(&bad, 1).await.expect("the thief's v2");
    assert_eq!(w.post_in(&e, 1).await.expect("same").0, StatusCode::OK);
    assert_eq!(anchored_by(&w), Some(0));
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(w.post_in(&e, 1).await.expect("same").0, StatusCode::OK);
    assert_eq!(anchored_by(&w), Some(0));
    // The publisher re-signs catalog 1 unchanged under its new key (the
    // reviewer's compromise point fell before it): what catalog 1 anchors
    // stays, now under the new report, and survives a restart.
    let r1b = cat::sign(&SECRET2, &c1.header).expect("ok");
    w.node.anchor_by(&r1b, &SECRET2);
    w.node.withdraw(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1b).await.expect("re-signed").0,
        StatusCode::CREATED
    );
    assert_eq!(served_versions(&w), vec![1, 2]);
    assert_eq!(anchored_by(&w), Some(0));
    let new_signer = hex0x(&onyx_bulk::anchor::address_of(&SECRET2).expect("ok"));
    let info = w
        .store
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    assert_eq!(info["anchor"]["signer"], json!(new_signer));
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("starts");
    let info = again
        .info("example.publisher", "synthetic.metrics")
        .expect("ok");
    assert_eq!(info["anchor"]["signer"], json!(new_signer));
    assert_eq!(info["versions"].as_array().map(Vec::len), Some(2));
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// CL5-6 right after a compromise: the node's cached read of the old report
/// still says it holds, yet the publisher's re-signed manifest re-anchors
/// the version (a manifest's own new report always reads the chain).
#[tokio::test]
async fn a_resigned_manifest_reanchors_inside_the_recheck_window() {
    let w = world();
    let (m, _) = published(&w).await;
    assert!(w.rows("from=0&to=1").await.is_ok());
    w.node.life(&sign(&m), 3);
    w.node.withdraw(&sign(&m));
    let r2 = onyx_bulk::anchor::sign(&SECRET2, &m.header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    let (s, v) = w.post(&m, &r2).await.expect("ok");
    assert_eq!((s, v["reanchored"].clone()), (StatusCode::OK, json!(true)));
    // While the held report holds, a second report changes nothing.
    let r3 = onyx_bulk::anchor::sign(&[0x44; 32], &m.header).expect("ok");
    w.node.anchor_by(&r3, &[0x44; 32]);
    let (s, v) = w.post(&m, &r3).await.expect("ok");
    assert_eq!((s, v.get("reanchored").cloned()), (StatusCode::OK, None));
    let a = w.rows("from=0&to=1").await.expect("ok");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(1)).expect("ok");
    assert_eq!(
        c.signer,
        onyx_bulk::anchor::address_of(&SECRET2).expect("ok")
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Catalog forks (THREATS CAT-6) --------------------------------------------------------------

fn fork_file(w: &World) -> PathBuf {
    w.dir.join("catalogs/example.publisher").join(FORK_FILE)
}

#[tokio::test]
async fn cat_6_two_anchored_catalogs_with_one_number_fork_until_one_is_withdrawn() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, good, _) = replacement(&t);
    // The same catalog 1 re-signed under the new key while the old report
    // holds: nothing to change.
    let r1s = cat::sign(&SECRET2, &t.c1.header).expect("ok");
    w.node.anchor_by(&r1s, &SECRET2);
    assert_eq!(
        w.post_catalog(&t.c1, &r1s).await.expect("same").0,
        StatusCode::OK
    );
    assert!(!fork_file(w).exists());
    // Another root under catalog 1, both anchored: a fork.
    let e = w.post_catalog(&c1b, &r1b).await.expect_err("fork");
    assert_eq!(code(e), "bulk_publisher_forked");
    assert!(fork_file(w).exists());
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("forked")),
        "bulk_dataset_forked"
    );
    assert_eq!(
        code(
            w.store
                .info_checked("example.publisher", "synthetic.metrics")
                .await
                .expect_err("forked")
        ),
        "bulk_dataset_forked"
    );
    let list = w
        .store
        .list(&parse_list_query(None).expect("ok"))
        .expect("ok");
    assert_eq!(list["datasets"][0]["forked"], json!(true));
    // Uploads of the publisher are refused, catalogs and manifests.
    assert_eq!(
        code(w.post_in(&good, 1).await.expect_err("forked")),
        "bulk_publisher_forked"
    );
    assert_eq!(
        code(w.post_catalog(&t.c1, &t.r1).await.expect_err("forked")),
        "bulk_publisher_forked"
    );
    // A restart keeps the mark (its record is checked again).
    let again = Arc::new(
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0),
            &w.dir,
            &w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("starts"),
    );
    assert_eq!(
        code(
            again
                .rows(
                    "example.publisher",
                    "synthetic.metrics",
                    &parse_rows_query(Some("from=0&to=3")).expect("ok")
                )
                .await
                .expect_err("forked")
        ),
        "bulk_dataset_forked"
    );
    drop(again);
    // An open dispute on one side changes nothing; a withdrawal clears it at
    // the next read past the recheck time.
    w.node.dispute(&r1b);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("still")),
        "bulk_dataset_forked"
    );
    w.node.withdraw(&r1b);
    // Within the recheck time the last read stands.
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("cached")),
        "bulk_dataset_forked"
    );
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert!(w.rows("from=0&to=3").await.is_ok());
    assert!(!fork_file(w).exists());
    assert!(w
        .dir
        .join("catalogs/example.publisher/withdrawn")
        .join(format!(
            "fork-{}-{}.bin",
            t.c1.header.root().to_hex(),
            c1b.header.root().to_hex()
        ))
        .exists());
    assert_eq!(served_versions(w), vec![1, 2]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_6_an_anchored_catalog_that_breaks_the_chain_forks() {
    let t = theft().await;
    let w = &t.w;
    // Catalog 2 that drops `synthetic.metrics` (built by hand).
    let leaves = vec![other_leaf("synthetic.thief")];
    let dropped = Catalog {
        header: cat::CatalogHeader {
            format: cat::FORMAT,
            publisher: publisher(),
            number: 2,
            prev: t.c1.header.root(),
            created_unix: T0 + 60,
            leaf_count: 1,
            leaves_root: cat::leaves_tree(&leaves).root(),
        },
        leaves,
    };
    let rd = sign_catalog(&dropped);
    // Not in the book: refused as it is, one read of its own report.
    let before = w.node.reads();
    assert_eq!(
        code(w.post_catalog(&dropped, &rd).await.expect_err("broken")),
        "bulk_catalog_not_successor"
    );
    assert_eq!(w.node.reads() - before, 3);
    assert!(!fork_file(w).exists());
    // Anchored, with catalog 1 anchored: a fork.
    w.node.anchor(&rd);
    let e = w.post_catalog(&dropped, &rd).await.expect_err("fork");
    assert_eq!(code(e), "bulk_publisher_forked");
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("forked")),
        "bulk_dataset_forked"
    );
    // The held side withdrawn (the reviewer's compromise point fell before
    // catalog 1): the mark clears, and catalog 1 can then be replaced.
    w.node.withdraw(&t.r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert!(w.rows("from=0&to=3").await.is_ok());
    let (c1b, r1b, _, _) = replacement(&t);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_6_review_one_read_per_window_and_an_unreadable_mark_stops_the_start() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, _, _) = replacement(&t);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    // Past the window with the chain down: the first read fails (503) and
    // claims the window; the reads meanwhile keep the mark with no chain read.
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    w.node.0.lock().expect("ok").down = true;
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("down")),
        "bulk_chain_unavailable"
    );
    w.node.0.lock().expect("ok").down = false;
    let before = w.node.reads();
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("marked")),
        "bulk_dataset_forked"
    );
    assert_eq!(w.node.reads(), before);
    // A fork file that cannot be read stops the start (never taken as no fork).
    std::fs::remove_file(fork_file(w)).expect("ok");
    std::fs::create_dir(fork_file(w)).expect("ok");
    assert!(BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .is_err());
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Catalogs stored as diffs (ADR-0059 amendment 2 draft 4; THREATS CAT-DIFF-R1) --------------

fn world_every(k: u64) -> World {
    world_with(BulkLimits {
        catalog_checkpoint: k,
        ..BulkLimits::default()
    })
}

/// Catalogs 0..=n of 40 datasets: each moves `synthetic.d00` by one version
/// and every second one adds a dataset.
fn chain(n: u64) -> Vec<Catalog> {
    let mut out: Vec<Catalog> = Vec::new();
    for i in 0..=n {
        let leaves = match out.last() {
            None => (0..40)
                .map(|d| other_leaf(&format!("synthetic.d{d:02}")))
                .collect(),
            Some(prev) => {
                let mut l = prev.leaves.clone();
                if let Some(a) = l.iter_mut().find(|x| x.dataset == "synthetic.d00") {
                    a.prev = a.root;
                    a.version += 1;
                    a.root = Hash32([10 + i as u8; 32]);
                }
                if i % 2 == 0 {
                    l.push(other_leaf(&format!("synthetic.e{i:02}")));
                }
                l.sort_by(|x, y| x.dataset.cmp(&y.dataset));
                l
            }
        };
        let c = catalog_of(out.last(), T0 + 10 * (i + 1), leaves);
        out.push(c);
    }
    out
}

impl World {
    async fn take_all(&self, cs: &[Catalog]) {
        for c in cs {
            let r = sign_catalog(c);
            self.node.anchor(&r);
            assert_eq!(
                self.post_catalog(c, &r).await.expect("taken").0,
                StatusCode::CREATED
            );
        }
    }

    fn cat_dir(&self) -> PathBuf {
        self.dir.join("catalogs").join(&publisher().0)
    }

    fn reopen(&self) -> Result<BulkStore, String> {
        BulkStore::open(
            pin(),
            self.node.clone(),
            Arc::new(|| T0 + 500),
            &self.dir,
            &self.dir.join("warm"),
            BulkLimits::default(),
        )
    }
}

fn rebuilt(s: &BulkStore, n: u64) -> Result<Option<Catalog>, String> {
    Ok(s.catalog_record(&publisher().0, n)?.map(|(c, _)| c))
}

#[tokio::test]
async fn cat_diff_catalogs_between_checkpoints_are_diffs_rebuilt_byte_for_byte() {
    let w = world_every(4);
    let cs = chain(9);
    w.take_all(&cs).await;
    let dir = w.cat_dir();
    for n in 0..=9u64 {
        let whole = n % 4 == 0;
        assert_eq!(dir.join(format!("{n}.bin")).exists(), whole, "{n}.bin");
        assert_eq!(dir.join(format!("{n}.diff")).exists(), !whole, "{n}.diff");
    }
    let size = |f: &str| std::fs::metadata(dir.join(f)).expect("file").len();
    assert!(
        size("1.diff") * 5 < size("0.bin"),
        "a diff holds only what changed"
    );
    // Every catalog, in any order (the cache starts a rebuild only from
    // below), equal to the one taken: the same Borsh bytes the route serves.
    for n in [9u64, 3, 0, 5, 6, 7, 1, 2, 8, 4] {
        let c = rebuilt(&w.store, n).expect("rebuilt").expect("held");
        assert_eq!(
            borsh::to_vec(&c).expect("enc"),
            borsh::to_vec(&cs[n as usize]).expect("enc")
        );
    }
    // After a restart (the newest rebuilt whole at start).
    let again = w.reopen().expect("starts");
    for n in 0..=9u64 {
        assert_eq!(
            rebuilt(&again, n).expect("rebuilt"),
            Some(cs[n as usize].clone())
        );
    }
    // The next catalog follows the rebuilt newest.
    let more = chain(10);
    let r = sign_catalog(&more[10]);
    w.node.anchor(&r);
    assert_eq!(
        w.post_catalog(&more[10], &r).await.expect("10").0,
        StatusCode::CREATED
    );
    assert!(dir.join("10.diff").exists());
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_diff_a_damaged_diff_is_an_error_never_not_held() {
    let w = world_every(4);
    let cs = chain(9);
    w.take_all(&cs).await;
    let dir = w.cat_dir();
    let damage = |n: u64| {
        let f = dir.join(format!("{n}.diff"));
        let mut d = CatalogDiff::try_from_slice(&std::fs::read(&f).expect("read")).expect("diff");
        // The last leaf: in 2.diff the dataset it adds, which every later
        // catalog keeps (a leaf a later diff moves again would be replaced).
        if let Some(l) = d.leaves.last_mut() {
            l.root = Hash32([0xEE; 32]);
        }
        std::fs::write(&f, borsh::to_vec(&d).expect("enc")).expect("write");
    };
    // An older diff, header intact: the start passes; it and the diffs built
    // on it answer an error (500), the next whole catalog and above serve.
    damage(2);
    let again = w.reopen().expect("starts: the header still holds");
    assert!(rebuilt(&again, 2).is_err());
    assert!(rebuilt(&again, 3).is_err());
    assert_eq!(rebuilt(&again, 1).expect("ok"), Some(cs[1].clone()));
    assert_eq!(rebuilt(&again, 5).expect("ok"), Some(cs[5].clone()));
    assert_eq!(rebuilt(&again, 42).expect("ok"), None);
    // The newest diff damaged: the start stops (the newest is rebuilt whole).
    damage(9);
    assert!(w.reopen().is_err());
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_diff_a_set_aside_leaves_a_prefix_and_the_replacement_follows_it() {
    let w = world_every(4);
    let cs = chain(5);
    w.take_all(&cs).await;
    // Catalogs 3 (a diff), 4 (whole) and 5 (a diff) withdrawn; the
    // publisher's replacement 3 under its new key follows catalog 2.
    for c in &cs[3..] {
        w.node.withdraw(&sign_catalog(c));
    }
    let mut leaves = cs[2].leaves.clone();
    leaves.push(other_leaf("synthetic.f00"));
    leaves.sort_by(|x, y| x.dataset.cmp(&y.dataset));
    let c3 = catalog_of(Some(&cs[2]), T0 + 100, leaves);
    let r3 = cat::sign(&SECRET2, &c3.header).expect("ok");
    w.node.anchor_by(&r3, &SECRET2);
    assert_eq!(
        w.post_catalog(&c3, &r3).await.expect("replaced").0,
        StatusCode::CREATED
    );
    let dir = w.cat_dir();
    let aside = dir.join("withdrawn");
    assert!(aside
        .join(format!("3-{}.diff", cs[3].header.root().to_hex()))
        .exists());
    assert!(aside
        .join(format!("4-{}.bin", cs[4].header.root().to_hex()))
        .exists());
    assert!(aside
        .join(format!("5-{}.diff", cs[5].header.root().to_hex()))
        .exists());
    assert!(
        dir.join("3.diff").exists() && !dir.join("4.bin").exists() && !dir.join("5.diff").exists()
    );
    assert_eq!(rebuilt(&w.store, 3).expect("ok"), Some(c3.clone()));
    let again = w.reopen().expect("starts");
    for (n, c) in [(0u64, &cs[0]), (2, &cs[2]), (3, &c3)] {
        assert_eq!(rebuilt(&again, n).expect("ok").as_ref(), Some(c));
    }
    assert_eq!(rebuilt(&again, 4).expect("ok"), None);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_diff_catalog_bytes_count_in_the_quota() {
    let cs = chain(0);
    let w = world_with(BulkLimits {
        quota_bytes: 1_000,
        ..BulkLimits::default()
    });
    let r = sign_catalog(&cs[0]);
    w.node.anchor(&r);
    assert_eq!(
        code(w.post_catalog(&cs[0], &r).await.expect_err("quota")),
        "bulk_quota_exceeded"
    );
    assert!(!w.cat_dir().join("0.bin").exists());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// The hosted node's layout: catalogs taken before diffs existed are whole
/// (`0.bin`, `1.bin`); after the change the next ones are diffs on them.
#[tokio::test]
async fn cat_diff_the_live_layout_whole_files_then_diffs() {
    let w = world_every(1);
    let cs = chain(3);
    w.take_all(&cs[..2]).await;
    let s = Arc::new(w.reopen().expect("starts with the default checkpoint"));
    for c in &cs[2..] {
        let r = sign_catalog(c);
        w.node.anchor(&r);
        let (st, _) = s
            .accept_catalog(
                &borsh::to_vec(c).expect("enc"),
                &borsh::to_vec(&r).expect("enc"),
            )
            .await
            .expect("taken");
        assert_eq!(st, StatusCode::CREATED);
    }
    let dir = w.cat_dir();
    for f in ["0.bin", "1.bin", "2.diff", "3.diff"] {
        assert!(dir.join(f).exists(), "{f}");
    }
    drop(s);
    let again = Arc::new(w.reopen().expect("starts"));
    for n in 0..=3u64 {
        assert_eq!(
            rebuilt(&again, n).expect("ok"),
            Some(cs[n as usize].clone())
        );
    }
    // Through the route: the newest at once, an older one rebuilt, and a
    // damaged diff a 500, never "no such catalog".
    let app = peer_routes(again.clone());
    let get = |n: u64| {
        Request::get(format!("/v1/bulk/catalogs/example.publisher/{n}"))
            .body(Body::empty())
            .expect("ok")
    };
    for n in [3u64, 2] {
        let r = app.clone().oneshot(get(n)).await.expect("ok");
        assert_eq!(r.status(), StatusCode::OK, "catalog {n}");
    }
    let f = dir.join("2.diff");
    let mut d = CatalogDiff::try_from_slice(&std::fs::read(&f).expect("read")).expect("diff");
    if let Some(l) = d.leaves.last_mut() {
        l.root = Hash32([0xEE; 32]);
    }
    std::fs::write(&f, borsh::to_vec(&d).expect("enc")).expect("write");
    again.old_body.lock().expect("lock").clear();
    // The answer kept from the good file would be served (SERVE-WRITE-2):
    // drop it, so the route rebuilds from the damaged one.
    *again.answers.lock().expect("lock") = AnswerBook::default();
    let r = app.clone().oneshot(get(2)).await.expect("ok");
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let r = app.clone().oneshot(get(9)).await.expect("ok");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS SERVE-WRITE-2: the catalog route answers from the kept answer
/// when it was made for the held root and report (a seeded answer comes back
/// as is), builds again when it was not, and every reader shares one copy.
#[tokio::test]
async fn serve_write_2_catalog_answers_are_shared_and_checked() {
    let w = world();
    walked(&w, 3).await;
    let app = peer_routes(w.store.clone());
    let get = |n: u64| {
        Request::get(format!("/v1/bulk/catalogs/example.publisher/{n}"))
            .body(Body::empty())
            .expect("ok")
    };
    let read = |r: Response| async move {
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).expect("type"),
            "application/json"
        );
        assert_eq!(
            r.headers().get(header::CACHE_CONTROL).expect("cc"),
            "no-store"
        );
        r.into_body().collect().await.expect("ok").to_bytes()
    };
    let first = read(app.clone().oneshot(get(2)).await.expect("ok")).await;
    let v: Value = serde_json::from_slice(&first).expect("json");
    crate::openapi_check::check("GET", "/v1/bulk/catalogs/example.publisher/2", 200, &v);
    assert_eq!(v["number"], json!(2));
    let key = "catalog/example.publisher/2";
    let check = w.store.catalog_check("example.publisher", 2).expect("held");
    let a = w.store.answer(key, check).expect("kept");
    let b = w.store.answer(key, check).expect("kept");
    assert_eq!(*a, first);
    assert!(Arc::ptr_eq(&a, &b), "one shared copy");
    let builds = w.store.answers.lock().expect("lock").builds;
    assert_eq!(
        read(app.clone().oneshot(get(2)).await.expect("ok")).await,
        first
    );
    assert_eq!(
        w.store.answers.lock().expect("lock").builds,
        builds,
        "no rebuild"
    );
    // The route reads the kept answer: one seeded under the held check is
    // what it sends.
    let seeded = axum::body::Bytes::from_static(b"{\"seeded\":true}");
    // (A valid copy alive is kept over a new one: drop it first.)
    drop((a, b));
    w.store.answers.lock().expect("lock").forget(key);
    w.store.keep_answer(key.into(), check, seeded.clone());
    assert_eq!(
        read(app.clone().oneshot(get(2)).await.expect("ok")).await,
        seeded
    );
    // Made for another report: stale, dropped, the real answer built again.
    w.store
        .keep_answer(key.into(), (check.0, Hash32([0x5a; 32])), seeded.clone());
    assert_eq!(
        read(app.clone().oneshot(get(2)).await.expect("ok")).await,
        first
    );
    assert!(w.store.answer(key, (check.0, Hash32([0x5a; 32]))).is_none());
    // Not held: a 404, nothing kept.
    let r = app.clone().oneshot(get(9)).await.expect("ok");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert!(w.store.answers.lock().expect("lock").building.is_empty());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review SERVE-WRITE-R3 findings 1 and 2: readers that arrive together for
/// an answer nobody holds get one build, and an answer pushed out of the
/// kept set while a reader still holds it is handed out again, not rebuilt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_write_2_one_build_for_a_crowd_and_no_second_copy_after_eviction() {
    let w = world();
    walked(&w, 3).await;
    w.store.old_body.lock().expect("lock").clear();
    w.store.answers.lock().expect("lock").kept.clear();
    let app = peer_routes(w.store.clone());
    let before = w.store.answers.lock().expect("lock").builds;
    let mut readers = Vec::new();
    for _ in 0..32 {
        let app = app.clone();
        readers.push(tokio::spawn(async move {
            let r = app
                .oneshot(
                    Request::get("/v1/bulk/catalogs/example.publisher/0")
                        .body(Body::empty())
                        .expect("ok"),
                )
                .await
                .expect("ok");
            assert_eq!(r.status(), StatusCode::OK);
            r.into_body().collect().await.expect("ok").to_bytes()
        }));
    }
    let mut bodies = Vec::new();
    for h in readers {
        bodies.push(h.await.expect("reader"));
    }
    assert!(bodies.windows(2).all(|p| p[0] == p[1]));
    assert_eq!(
        w.store.answers.lock().expect("lock").builds - before,
        1,
        "one build for 32 readers"
    );
    // A reader still holds the answer; others push it out of the kept set.
    let key = "catalog/example.publisher/0";
    let check = w.store.catalog_check("example.publisher", 0).expect("held");
    let held = w.store.answer(key, check).expect("live");
    let big = || axum::body::Bytes::from(vec![0u8; 12 << 20]);
    for n in 0..3u64 {
        w.store.keep_answer(
            format!("catalog/x/{n}"),
            (Hash32([1; 32]), Hash32([2; 32])),
            big(),
        );
    }
    {
        let b = w.store.answers.lock().expect("lock");
        assert!(!b.kept.contains_key(key), "pushed out of the kept set");
        assert!(b.kept_bytes <= ANSWER_BYTES, "{}", b.kept_bytes);
    }
    let builds = w.store.answers.lock().expect("lock").builds;
    let again = w.store.answer(key, check).expect("still live");
    assert!(
        Arc::ptr_eq(&held, &again),
        "the held copy, not a second one"
    );
    assert_eq!(w.store.answers.lock().expect("lock").builds, builds);
    // Nobody holds it any more, and it is not kept: built again next time.
    drop((held, again));
    for n in 3..6u64 {
        w.store.keep_answer(
            format!("catalog/x/{n}"),
            (Hash32([1; 32]), Hash32([2; 32])),
            big(),
        );
    }
    assert!(w.store.answer(key, check).is_none());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// SERVE-WRITE-2: the manifest route answers from the kept answer in the
/// same way, without copying the manifest to check it.
#[tokio::test]
async fn serve_write_2_manifest_answers_are_shared_and_checked() {
    let w = world();
    let (m, _) = published(&w).await;
    let root = m.header.root();
    let uri = format!("/v1/bulk/manifests/{root}");
    let app = peer_routes(w.store.clone());
    let fetch = || async {
        let r = app
            .clone()
            .oneshot(Request::get(uri.clone()).body(Body::empty()).expect("ok"))
            .await
            .expect("ok");
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).expect("type"),
            "application/json"
        );
        r.into_body().collect().await.expect("ok").to_bytes()
    };
    let first = fetch().await;
    assert_eq!(fetch().await, first);
    let v: Value = serde_json::from_slice(&first).expect("json");
    crate::openapi_check::check("GET", &uri, 200, &v);
    let key = format!("manifest/{root}");
    let check = w.store.manifest_check(&root).expect("held");
    let (_, rep) = w.store.manifest_record(&root).expect("held");
    assert_eq!(check, (root, rep.hash()));
    let a = w.store.answer(&key, check).expect("kept");
    let b = w.store.answer(&key, check).expect("kept");
    assert_eq!(*a, first);
    assert!(Arc::ptr_eq(&a, &b), "one shared copy");
    let seeded = axum::body::Bytes::from_static(b"{\"seeded\":true}");
    drop((a, b));
    w.store.answers.lock().expect("lock").forget(&key);
    w.store.keep_answer(key.clone(), check, seeded.clone());
    assert_eq!(fetch().await, seeded);
    w.store
        .keep_answer(key.clone(), (root, Hash32([9; 32])), seeded.clone());
    assert_eq!(fetch().await, first);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review SERVE-WRITE-R4 finding 1: a look with an old check does not throw
/// away the valid answer others hold; the next reader gets that same copy.
#[tokio::test]
async fn serve_write_2_a_stale_look_keeps_the_live_copy() {
    let w = world();
    walked(&w, 3).await;
    let app = peer_routes(w.store.clone());
    let get = || {
        Request::get("/v1/bulk/catalogs/example.publisher/2")
            .body(Body::empty())
            .expect("ok")
    };
    let r = app.clone().oneshot(get()).await.expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    let key = "catalog/example.publisher/2";
    let check = w.store.catalog_check("example.publisher", 2).expect("held");
    let held = w.store.answer(key, check).expect("kept");
    assert!(w.store.answer(key, (check.0, Hash32([7; 32]))).is_none());
    let before = w.store.answers.lock().expect("lock").builds;
    let r = app.clone().oneshot(get()).await.expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        w.store.answers.lock().expect("lock").builds,
        before,
        "no new copy"
    );
    let again = w.store.answer(key, check).expect("kept");
    assert!(Arc::ptr_eq(&held, &again), "the same copy");
    // A build that finishes while a valid copy is alive hands out that copy.
    let other = w
        .store
        .keep_answer(key.into(), check, axum::body::Bytes::from_static(b"{}"));
    assert!(Arc::ptr_eq(&held, &other));
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review SERVE-WRITE-R4 finding 2: a turn that ends without an answer (a
/// cancelled or failed holder) leaves its gate to the reader waiting on it,
/// so a later arrival waits too: one copy for both, and no gate left after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_write_2_a_turn_that_ends_without_an_answer_keeps_one_copy() {
    let w = world();
    walked(&w, 3).await;
    w.store.old_body.lock().expect("lock").clear();
    w.store.answers.lock().expect("lock").kept.clear();
    let app = peer_routes(w.store.clone());
    let key = "catalog/example.publisher/0";
    let req = || {
        Request::get("/v1/bulk/catalogs/example.publisher/0")
            .body(Body::empty())
            .expect("ok")
    };
    let waiters = |n: usize| {
        let s = w.store.clone();
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                loop {
                    let c = s
                        .answers
                        .lock()
                        .expect("lock")
                        .building
                        .get(key)
                        .map_or(0, Arc::strong_count);
                    if c >= n {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the readers queued");
        }
    };
    // The rebuilds are busy, and a holder takes the turn and gives up.
    let permits = w
        .store
        .rebuilds
        .clone()
        .acquire_many_owned(MAX_REBUILDS as u32)
        .await
        .expect("permits");
    let holder = w.store.answer_turn(key).await;
    let first = tokio::spawn(app.clone().oneshot(req()));
    // The book, the holder's gate and guard, and the waiter.
    waiters(4).await;
    drop(holder);
    let second = tokio::spawn(app.clone().oneshot(req()));
    // The book, the first reader's gate and guard, and the second waiting.
    waiters(4).await;
    drop(permits);
    let (r1, r2) = (
        first.await.expect("task").expect("ok"),
        second.await.expect("task").expect("ok"),
    );
    assert_eq!((r1.status(), r2.status()), (StatusCode::OK, StatusCode::OK));
    let (mut b1, mut b2) = (r1.into_body(), r2.into_body());
    let f1 = b1
        .frame()
        .await
        .expect("f")
        .expect("f")
        .into_data()
        .expect("d");
    let f2 = b2
        .frame()
        .await
        .expect("f")
        .expect("f")
        .into_data()
        .expect("d");
    assert_eq!(f1.as_ptr(), f2.as_ptr(), "one copy for both readers");
    drop((b1, b2));
    assert!(
        w.store.answers.lock().expect("lock").building.is_empty(),
        "no gate left"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn serve_write_2_a_shared_answer_goes_out_in_slices_of_one_copy() {
    let data = axum::body::Bytes::from(
        (0..(FILE_FRAME * 2 + 7))
            .map(|i| i as u8)
            .collect::<Vec<_>>(),
    );
    let mut body = SharedBody {
        bytes: Arc::new(data.clone()),
        at: 0,
    };
    let range = data.as_ptr() as usize..data.as_ptr() as usize + data.len();
    let (mut got, mut frames) = (Vec::new(), 0);
    while let Some(f) = body.frame().await {
        let f = f.expect("frame").into_data().expect("data");
        assert!(f.len() <= FILE_FRAME);
        assert!(
            range.contains(&(f.as_ptr() as usize)),
            "a slice, not a copy"
        );
        got.extend_from_slice(&f);
        frames += 1;
    }
    assert_eq!(frames, 3);
    assert_eq!(got, data);
}

// ---------------------------------------------------------------- history (CAT-1)

/// A node reached through its peer routes (the history check's GETs).
struct RouterGet(Router);

impl crate::storage::HttpGet for RouterGet {
    fn get<'a>(
        &'a self,
        url: &'a str,
        max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            let path = url.strip_prefix("http://node").ok_or("another host")?;
            let r = self
                .0
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).expect("ok"))
                .await
                .map_err(|e| e.to_string())?;
            let status = r.status().as_u16();
            let b = r
                .into_body()
                .collect()
                .await
                .map_err(|e| e.to_string())?
                .to_bytes();
            assert!(b.len() <= max, "answer over the client's cap");
            Ok((status, b.to_vec()))
        })
    }
}

/// Catalogs 0..=n of a world: the 40 and more datasets of `chain(n)`, and
/// `synthetic.metrics`, which moves a version at every third catalog; each
/// catalog anchored and taken, each version posted in the catalog that first
/// lists it, with its shards. The manifests by version.
async fn walked(w: &World, n: u64) -> (Vec<(Catalog, SignedReport)>, Vec<Manifest>) {
    let others = chain(n);
    let files = shard_files(0..30, 10);
    let mut ms = vec![(manifest(&files, None, T0), files)];
    let mut out: Vec<(Catalog, SignedReport)> = Vec::new();
    for i in 0..=n {
        let moved = i > 0 && i % 3 == 0;
        if moved {
            let k = 30 + 10 * i as i128;
            let f = shard_files(k..k + 10, 10);
            let m = manifest(&f, Some(&ms.last().expect("v").0), T0 + 10 * i + 5);
            ms.push((m, f));
        }
        let mut leaves = others[i as usize].leaves.clone();
        leaves.push(CatalogLeaf::of(&ms.last().expect("v").0.header));
        leaves.sort_by(|a, b| a.dataset.cmp(&b.dataset));
        let c = catalog_of(out.last().map(|(c, _)| c), T0 + 10 * (i + 1), leaves);
        let r = sign_catalog(&c);
        w.node.anchor(&r);
        assert_eq!(
            w.post_catalog(&c, &r).await.expect("taken").0,
            StatusCode::CREATED
        );
        if i == 0 || moved {
            let (m, f) = ms.last().expect("v");
            assert_eq!(
                w.post_in(m, i).await.expect("version").0,
                StatusCode::CREATED
            );
            w.upload_all(m, f).await;
        }
        out.push((c, r));
    }
    (out, ms.into_iter().map(|(m, _)| m).collect())
}

fn opts(keys: Vec<[u8; 20]>, scan_from: Option<u64>) -> crate::bulk_history::HistoryOpts {
    crate::bulk_history::HistoryOpts {
        node: "http://node".into(),
        keys,
        scan_from,
        logs_range: 50,
    }
}

async fn history(
    w: &World,
    o: &crate::bulk_history::HistoryOpts,
) -> crate::bulk_history::HistoryOutcome {
    let a = w.rows("from=0&to=3").await.expect("rows");
    let ck = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(3)).expect("client check");
    let get = RouterGet(peer_routes(w.store.clone()));
    crate::bulk_history::check_history(&get, w.node.as_ref(), CHAIN, BOOK, &ck, o).await
}

fn verified(o: crate::bulk_history::HistoryOutcome) -> Value {
    match o {
        crate::bulk_history::HistoryOutcome::Verified(v) => v,
        other => panic!("not verified: {}", other.report()),
    }
}

fn refused(o: crate::bulk_history::HistoryOutcome) -> (&'static str, String) {
    match o {
        crate::bulk_history::HistoryOutcome::Refused(f, w) => (f, w),
        other => panic!("not refused: {}", other.report()),
    }
}

fn undecided(o: crate::bulk_history::HistoryOutcome) -> String {
    match o {
        crate::bulk_history::HistoryOutcome::Undecided(w) => w,
        other => panic!("not undecided: {}", other.report()),
    }
}

#[tokio::test]
async fn cat_1_the_files_route_answers_stored_catalogs_a_block_at_a_time() {
    let w = world_every(4);
    let (cs, _) = walked(&w, 9).await;
    let app = peer_routes(w.store.clone());
    let files = |q: &str| {
        let app = app.clone();
        let uri = format!("/v1/bulk/catalogs/example.publisher/files{q}");
        async move {
            let r = app
                .oneshot(Request::get(uri).body(Body::empty()).expect("ok"))
                .await
                .expect("ok");
            let s = r.status();
            (
                s,
                r.into_body()
                    .collect()
                    .await
                    .expect("ok")
                    .to_bytes()
                    .to_vec(),
            )
        }
    };
    let (s, b) = files("?from=0").await;
    assert_eq!(s, StatusCode::OK);
    let f = cat::CatalogFiles::try_from_slice(&b).expect("decodes");
    assert_eq!(f.newest, 9);
    let numbers =
        |f: &cat::CatalogFiles| f.files.iter().map(|x| x.header.number).collect::<Vec<_>>();
    assert_eq!(numbers(&f), vec![0, 1, 2, 3]);
    assert!(matches!(f.files[0].leaves, cat::FileLeaves::Whole(_)));
    assert!(matches!(f.files[1].leaves, cat::FileLeaves::Diff(_)));
    assert_eq!(f.files[2].header, cs[2].0.header);
    assert_eq!(f.files[2].report, cs[2].1);
    let f = cat::CatalogFiles::try_from_slice(&files("?from=5").await.1).expect("decodes");
    assert_eq!(numbers(&f), vec![5, 6, 7]);
    let f = cat::CatalogFiles::try_from_slice(&files("?from=8").await.1).expect("decodes");
    assert_eq!(numbers(&f), vec![8, 9]);
    assert!(matches!(f.files[0].leaves, cat::FileLeaves::Whole(_)));
    assert_eq!(files("?from=10").await.0, StatusCode::NOT_FOUND);
    // The byte cap: always at least one catalog.
    let one = w
        .store
        .catalog_files(&publisher().0, 0, 1)
        .expect("ok")
        .expect("held");
    let f = cat::CatalogFiles::try_from_slice(&one).expect("decodes");
    assert_eq!(numbers(&f), vec![0]);
    for bad in ["", "?from=x", "?from=1&from=2", "?to=3"] {
        assert_eq!(files(bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_an_honest_history_with_diffs_verifies() {
    let w = world_every(4);
    let (cs, ms) = walked(&w, 9).await;
    let v = verified(history(&w, &opts(vec![], None)).await);
    assert_eq!(v["catalogs"], json!(10));
    assert_eq!(v["newest"], json!(9));
    assert_eq!(v["entered"], json!(0));
    assert_eq!(v["versions"].as_array().expect("v").len(), ms.len());
    assert_eq!(v["versions"][3]["firstCatalog"], json!(9));
    assert_eq!(v["keys"].as_array().expect("k").len(), 1);
    assert_eq!(
        v["keys"][0]["catalogs"],
        json!({"count": 10, "first": 0, "last": 9})
    );
    assert_eq!(v["keys"][0]["found"], json!(["answer", "walk"]));
    assert_eq!(v["created"]["last"], json!(cs[9].0.header.created_unix));
    assert!(v["limit"].is_string(), "no scan: the limit is stated");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_a_node_that_serves_wrong_leaves_is_at_fault() {
    let w = world_every(4);
    walked(&w, 5).await;
    // A leaf changed in a stored diff (a damaged or forged file).
    let f = w.cat_dir().join("2.diff");
    let mut d = CatalogDiff::try_from_slice(&std::fs::read(&f).expect("read")).expect("decodes");
    d.leaves[0].root = Hash32([0xEE; 32]);
    std::fs::write(&f, borsh::to_vec(&d).expect("ok")).expect("write");
    let (fault, why) = refused(history(&w, &opts(vec![], None)).await);
    assert_eq!(fault, "node");
    assert!(why.contains("catalog 2"), "{why}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Catalog `n` with one leaf's root changed: another catalog `n` the
/// publisher's other key signs (built by hand: no successor of anything).
fn rewritten(c: &Catalog) -> Catalog {
    let mut leaves = c.leaves.clone();
    leaves[1].root = Hash32([0xAB; 32]);
    let header = cat::CatalogHeader {
        leaves_root: cat::leaves_tree(&leaves).root(),
        created_unix: c.header.created_unix + 1,
        ..c.header.clone()
    };
    Catalog { header, leaves }
}

#[tokio::test]
async fn cat_1_review_f2_a_second_catalog_under_another_key_is_caught_once_the_key_is_known() {
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    let alt = rewritten(&cs[2].0);
    let r = cat::sign(&SECRET2, &alt.header).expect("ok");
    w.node.anchor_by(&r, &SECRET2);
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    // Unknown to the walk: the stated limit.
    let v = verified(history(&w, &opts(vec![], None)).await);
    assert_eq!(v["limit"], json!(crate::bulk_history::HISTORY_LIMIT));
    // Named by the caller: two live catalogs 2.
    let (fault, why) = refused(history(&w, &opts(vec![signer2], None)).await);
    assert_eq!(fault, "publisher");
    assert!(why.contains("catalog 2"), "{why}");
    // Found by a key scan (200 blocks a query at most, as the public RPC).
    {
        let mut b = w.node.0.lock().expect("ok");
        b.sources
            .insert(r.payload.key, keccak256(publisher().0.as_bytes()));
        b.logs.push((0x31, r.payload.key));
        b.logs.push((0x32, Hash32([0x99; 32])));
    }
    let (fault, _) = refused(history(&w, &opts(vec![], Some(0))).await);
    assert_eq!(fault, "publisher");
    // While its fact is disputed, nothing is decided.
    w.node.dispute(&r);
    assert!(undecided(history(&w, &opts(vec![signer2], None)).await).contains("catalog 2"));
    // Withdrawn for good, it is listed as set aside.
    w.node.withdraw(&r);
    let v = verified(history(&w, &opts(vec![signer2], None)).await);
    assert_eq!(v["setAside"].as_array().expect("s").len(), 1);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_review_f1_a_stolen_key_recovery_verifies_with_the_thiefs_catalog_set_aside() {
    let t = theft().await;
    let w = &t.w;
    let (c1b, r1b, good, files) = replacement(&t);
    w.node.withdraw(&t.r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    w.post_catalog(&c1b, &r1b).await.expect("replaced");
    w.post_in(&good, 1).await.expect("v2");
    w.upload_all(&good, &files).await;
    let v = verified(history(w, &opts(vec![], None)).await);
    assert_eq!(v["catalogs"], json!(2));
    assert_eq!(v["keys"].as_array().expect("k").len(), 2);
    let aside = v["setAside"].as_array().expect("s");
    assert_eq!(aside.len(), 1);
    assert_eq!(aside[0]["digest"], json!(t.r1.payload.digest().to_string()));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_review_f4_a_catalog_the_book_holds_but_the_node_did_not_serve_is_undecided() {
    let w = world();
    let (cs, _) = walked(&w, 2).await;
    let next = catalog_of(Some(&cs[2].0), T0 + 100, cs[2].0.leaves.clone());
    w.node.anchor(&sign_catalog(&next));
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("catalog 3"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_a_withdrawn_or_unrecorded_catalog_on_the_chain_is_undecided() {
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    w.node.withdraw(&cs[1].1);
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("catalog 1"));
    // Not yet recorded at the block read (the caller's node lags): undecided.
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    w.node
        .0
        .lock()
        .expect("ok")
        .reports
        .remove(&cs[3].1.payload.digest());
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("catalog 3"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_review_f5_a_second_root_for_a_version_is_compared_by_value() {
    let w = world();
    let (_, ms) = walked(&w, 3).await;
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    // The other key's own report of version 2 with the same root: fine.
    let same = onyx_bulk::anchor::sign(&SECRET2, &ms[1].header).expect("ok");
    w.node.anchor_by(&same, &SECRET2);
    verified(history(&w, &opts(vec![signer2], None)).await);
    // Another root for version 2 under that key: a rewrite.
    let w = world();
    let (_, ms) = walked(&w, 3).await;
    let other = manifest(&shard_files(500..510, 10), Some(&ms[0]), T0 + 33);
    let r = onyx_bulk::anchor::sign(&SECRET2, &other.header).expect("ok");
    w.node.anchor_by(&r, &SECRET2);
    let (fault, why) = refused(history(&w, &opts(vec![signer2], None)).await);
    assert_eq!(fault, "publisher");
    assert!(why.contains("version 2"), "{why}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_a_dataset_that_entered_after_its_own_reports_is_followed_to_version_1() {
    let w = world();
    // Version 1 with its own report, then catalogs from version 2 on.
    let (m1, _) = published(&w).await;
    let f2 = shard_files(30..40, 10);
    let m2 = manifest(&f2, Some(&m1), T0 + 5);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m2.header), other_leaf("synthetic.other")],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m2, 0).await.expect("v2");
    w.upload_all(&m2, &f2).await;
    let v = verified(history(&w, &opts(vec![], None)).await);
    assert_eq!(v["versions"][0]["version"], json!(1));
    assert_eq!(v["versions"][0]["firstCatalog"], Value::Null);
    assert_eq!(v["versions"][1]["firstCatalog"], json!(0));
    assert!(v["keys"][0]["found"]
        .as_array()
        .expect("f")
        .contains(&json!("manifest")));
    // Version 1's own report withdrawn for good, and still served: undecided;
    // restated under the publisher's other key with the same root, it holds
    // (review of the build, finding 8).
    w.node.withdraw(&sign(&m1));
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("version 1"));
    let again = onyx_bulk::anchor::sign(&SECRET2, &m1.header).expect("ok");
    w.node.anchor_by(&again, &SECRET2);
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    verified(history(&w, &opts(vec![signer2], None)).await);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// A node whose answers a test may replace (`hook` gives the answer, or
/// `None` for the node's own).
struct HookGet<F: Fn(&str) -> Option<(u16, Vec<u8>)> + Send + Sync> {
    inner: RouterGet,
    hook: F,
}

impl<F: Fn(&str) -> Option<(u16, Vec<u8>)> + Send + Sync> crate::storage::HttpGet for HookGet<F> {
    fn get<'a>(
        &'a self,
        url: &'a str,
        max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        match (self.hook)(url) {
            Some(r) => Box::pin(async move { Ok(r) }),
            None => self.inner.get(url, max),
        }
    }
}

/// ADR-0059 amendment 4 item 6 (THREATS CAT-3): a rows answer checked
/// before the publisher removed its dataset comes out `Removed` (exit 4),
/// naming the tombstone's catalog; a live own report at the tombstone's
/// version is the publisher's fault, ranked above the removal.
#[tokio::test]
async fn cat_3_the_history_check_reports_a_removal_and_a_report_after_it() {
    let w = world();
    let (cs, ms) = walked(&w, 3).await;
    let ck = checked_rows(&w).await;
    let last = ms.last().expect("v").clone();
    let (prev, _) = cs.last().expect("c");
    let mut leaves = prev.leaves.clone();
    let i = prev.find("synthetic.metrics").expect("listed");
    leaves[i] = leaves[i].tombstone().expect("t");
    let c4 = catalog_of(Some(prev), T0 + 1_000, leaves);
    let r4 = sign_catalog(&c4);
    w.node.anchor(&r4);
    assert_eq!(
        w.post_catalog(&c4, &r4).await.expect("c4").0,
        StatusCode::CREATED
    );
    let get = RouterGet(peer_routes(w.store.clone()));
    let o = history_with(&w, &ck, &get).await;
    assert_eq!(o.code(), 4, "{}", o.report());
    let v = o.report();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["removed"]["catalog"], json!(4));
    assert_eq!(v["removed"]["version"], json!(last.header.version + 1));
    // A live own report at the version after the tombstone: the
    // publisher's (review of the CAT-3 build, finding 1).
    let files = shard_files(500..510, 10);
    let at_t = manifest(&files, Some(&last), T0 + 2_000);
    let beyond = manifest(&shard_files(510..520, 10), Some(&at_t), T0 + 2_001);
    assert_eq!(beyond.header.version, last.header.version + 2);
    w.node.anchor(&sign(&beyond));
    let (f, why) = refused(history_with(&w, &ck, &get).await);
    assert_eq!(f, "publisher", "{why}");
    assert!(why.contains("after the dataset's tombstone"), "{why}");
    // And one at the tombstone's own version.
    w.node.anchor(&sign(&at_t));
    let o = history_with(&w, &ck, &get).await;
    assert_eq!(refused(o).0, "publisher");
    let _ = std::fs::remove_dir_all(&w.dir);
}

async fn checked_rows(w: &World) -> onyx_bulk::wire::Checked {
    let a = w.rows("from=0&to=3").await.expect("rows");
    onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(3)).expect("client check")
}

async fn history_with(
    w: &World,
    ck: &onyx_bulk::wire::Checked,
    get: &dyn crate::storage::HttpGet,
) -> crate::bulk_history::HistoryOutcome {
    crate::bulk_history::check_history(get, w.node.as_ref(), CHAIN, BOOK, ck, &opts(vec![], None))
        .await
}

fn files_of(cs: &[(Catalog, SignedReport)], newest: u64) -> Vec<u8> {
    borsh::to_vec(&cat::CatalogFiles {
        newest,
        files: cs
            .iter()
            .map(|(c, r)| cat::CatalogFile {
                header: c.header.clone(),
                report: r.clone(),
                leaves: cat::FileLeaves::Whole(c.leaves.clone()),
            })
            .collect(),
    })
    .expect("ok")
}

#[tokio::test]
async fn cat_1_build_f1_with_no_catalog_served_the_book_is_asked_for_catalog_0() {
    let w = world();
    published(&w).await;
    let v = verified(history(&w, &opts(vec![], None)).await);
    assert_eq!(v["catalogs"], json!(0));
    assert_eq!(v["versions"][0]["version"], json!(1));
    // The book holds a catalog 0 under the same key that this node does not.
    let c0 = catalog_of(None, T0 + 10, vec![other_leaf("synthetic.other")]);
    w.node.anchor(&sign_catalog(&c0));
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("catalog 0"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_build_f2_an_own_report_answer_is_tied_to_the_catalogs() {
    // Honest: an own-report version 3 after catalogs listing 1 and 2.
    let w = world();
    let (_, ms) = walked(&w, 3).await;
    let f3 = shard_files(900..910, 10);
    let m3 = manifest(&f3, Some(&ms[1]), T0 + 200);
    let r3 = sign(&m3);
    w.node.anchor(&r3);
    w.post(&m3, &r3).await.expect("v3");
    w.upload_all(&m3, &f3).await;
    let ck = checked_rows(&w).await;
    assert!(ck.catalog.is_none() && ck.header.version == 3);
    let v = verified(history(&w, &opts(vec![], None)).await);
    assert_eq!(v["versions"].as_array().expect("v").len(), 3);
    // A rows answer with another version 2, under a key that signed nothing
    // in the walk: refused, from the answer's own key.
    let b = world();
    let (m1, _) = published(&b).await;
    let f2 = shard_files(700..710, 10);
    let other = manifest(&f2, Some(&m1), T0 + 7);
    let r2 = onyx_bulk::anchor::sign(&SECRET2, &other.header).expect("ok");
    b.node.anchor_by(&r2, &SECRET2);
    b.post(&other, &r2).await.expect("v2 under the other key");
    b.upload_all(&other, &f2).await;
    let ck = checked_rows(&b).await;
    let a = world();
    walked(&a, 3).await;
    a.node.anchor_by(&r2, &SECRET2);
    let get = RouterGet(peer_routes(a.store.clone()));
    let (fault, why) = refused(history_with(&a, &ck, &get).await);
    assert_eq!(fault, "publisher");
    assert!(why.contains("version 2"), "{why}");
    for d in [&w.dir, &a.dir, &b.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn cat_1_build_f3_the_rows_answers_catalog_lagging_or_forked() {
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    let ck = checked_rows(&w).await;
    assert_eq!(ck.catalog.as_ref().map(|h| h.number), Some(3));
    // A history node that has catalogs 0..=2 only: it lags (undecided).
    let short = files_of(&cs[..3], 2);
    let get = HookGet {
        inner: RouterGet(peer_routes(w.store.clone())),
        hook: move |u: &str| u.contains("/files?from=0").then(|| (200, short.clone())),
    };
    assert!(undecided(history_with(&w, &ck, &get).await).contains("past the newest"));
    // Another catalog 3, anchored under the publisher's other key: a fork.
    let alt = catalog_of(
        Some(&cs[2].0),
        cs[3].0.header.created_unix + 1,
        cs[3].0.leaves.clone(),
    );
    let ra = cat::sign(&SECRET2, &alt.header).expect("ok");
    w.node.anchor_by(&ra, &SECRET2);
    let mut forked = cs[..3].to_vec();
    forked.push((alt, ra.clone()));
    let forked = files_of(&forked, 3);
    let get = HookGet {
        inner: RouterGet(peer_routes(w.store.clone())),
        hook: move |u: &str| u.contains("/files?from=0").then(|| (200, forked.clone())),
    };
    assert_eq!(refused(history_with(&w, &ck, &get).await).0, "publisher");
    // Not anchored: undecided.
    w.node.withdraw(&ra);
    assert!(undecided(history_with(&w, &ck, &get).await).contains("catalog 3"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_build_f4_a_withdrawn_catalog_still_served_is_undecided_not_refused() {
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    w.node.withdraw(&cs[1].1);
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("not set it aside"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_build_f6_a_signed_rule_break_is_the_publishers_only_when_anchored() {
    let w = world();
    let (cs, _) = walked(&w, 1).await;
    let ck = checked_rows(&w).await;
    // A signed catalog 1 that drops every dataset but one.
    let leaves = vec![cs[1].0.leaves[0].clone()];
    let header = cat::CatalogHeader {
        leaf_count: 1,
        leaves_root: cat::leaves_tree(&leaves).root(),
        ..cs[1].0.header.clone()
    };
    let bad = Catalog { header, leaves };
    let rb = sign_catalog(&bad);
    let served = files_of(&[cs[0].clone(), (bad, rb.clone())], 1);
    let get = HookGet {
        inner: RouterGet(peer_routes(w.store.clone())),
        hook: move |u: &str| u.contains("/files?from=0").then(|| (200, served.clone())),
    };
    assert_eq!(refused(history_with(&w, &ck, &get).await).0, "node");
    w.node.anchor(&rb);
    assert_eq!(refused(history_with(&w, &ck, &get).await).0, "publisher");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test(start_paused = true)]
async fn cat_1_build_f6_a_busy_node_is_tried_again_and_a_set_aside_mid_walk_is_undecided() {
    let w = world_every(4);
    walked(&w, 9).await;
    let ck = checked_rows(&w).await;
    let busy = AtomicU64::new(0);
    let get = HookGet {
        inner: RouterGet(peer_routes(w.store.clone())),
        hook: move |u: &str| {
            (u.contains("/files?from=0") && busy.fetch_add(1, Ordering::Relaxed) < 2)
                .then(|| (429, Vec::new()))
        },
    };
    verified(history_with(&w, &ck, &get).await);
    let get = HookGet {
        inner: RouterGet(peer_routes(w.store.clone())),
        hook: |u: &str| u.contains("/files?from=4").then(|| (404, Vec::new())),
    };
    assert!(undecided(history_with(&w, &ck, &get).await).contains("catalog 4"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_build_f6_a_scanned_key_counts_only_when_the_book_says_it_is_the_publishers() {
    const SECRET3: [u8; 32] = [0x44; 32];
    let w = world();
    let (cs, _) = walked(&w, 3).await;
    let alt = rewritten(&cs[1].0);
    let r = cat::sign(&SECRET3, &alt.header).expect("ok");
    w.node.anchor_by(&r, &SECRET3);
    {
        let mut b = w.node.0.lock().expect("ok");
        b.sources
            .insert(r.payload.key, keccak256(b"other.publisher"));
        b.logs.push((0x40, r.payload.key));
    }
    let v = verified(history(&w, &opts(vec![], Some(0))).await);
    assert_eq!(v["keysScanned"]["from"], json!(0));
    assert!(v["limit"].is_null());
    w.node
        .0
        .lock()
        .expect("ok")
        .sources
        .insert(r.payload.key, keccak256(publisher().0.as_bytes()));
    assert_eq!(
        refused(history(&w, &opts(vec![], Some(0))).await).0,
        "publisher"
    );
    // A scan that starts past the block read scans nothing and says so.
    let v = verified(history(&w, &opts(vec![], Some(0x1000))).await);
    assert!(v["keysScanned"].is_null() && v["limit"].is_string());
    let _ = std::fs::remove_dir_all(&w.dir);
}

async fn files_req(app: &Router) -> Response {
    app.clone()
        .oneshot(
            Request::get("/v1/bulk/catalogs/example.publisher/files?from=0")
                .body(Body::empty())
                .expect("ok"),
        )
        .await
        .expect("ok")
}

#[tokio::test]
async fn cat_1_fix_f1_two_files_answers_at_a_time_each_held_until_its_last_frame() {
    let w = world();
    walked(&w, 3).await;
    let app = peer_routes(w.store.clone());
    let req = || files_req(&app);
    let a = req().await;
    let b = req().await;
    assert_eq!((a.status(), b.status()), (StatusCode::OK, StatusCode::OK));
    // Both unread: a third is told to come back.
    let c = req().await;
    assert_eq!(c.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(c.headers()["retry-after"], "10");
    assert_eq!(w.store.file_reads.available_permits(), 0);
    // One read to its end lets one more in; the bytes are the whole answer.
    let bytes = a.into_body().collect().await.expect("sent").to_bytes();
    assert_eq!(w.store.file_reads.available_permits(), 1);
    let f = cat::CatalogFiles::try_from_slice(&bytes).expect("decodes");
    assert_eq!(f.files.len(), 4);
    assert_eq!(req().await.status(), StatusCode::OK);
    drop(b);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test(start_paused = true)]
async fn cat_1_fix_f1_a_reader_that_stalls_lets_go_of_the_permit_after_the_deadline() {
    let w = world();
    walked(&w, 3).await;
    let app = peer_routes(w.store.clone());
    let req = || files_req(&app);
    let a = req().await;
    let b = req().await;
    assert_eq!(req().await.status(), StatusCode::SERVICE_UNAVAILABLE);
    tokio::time::sleep(std::time::Duration::from_secs(FILES_SEND_SECS + 1)).await;
    // The permits are free while both stalled answers still exist, and a
    // stalled answer ends with an error when asked for more.
    assert_eq!(w.store.file_reads.available_permits(), 2);
    assert_eq!(req().await.status(), StatusCode::OK);
    assert!(a.into_body().collect().await.is_err());
    drop(b);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// A world with catalogs 0..=3 (versions 1 and 2 listed) and own-report
/// versions after them, one manifest per extra version; their reports.
async fn with_own_versions(extra: u32) -> (World, Vec<Manifest>, Vec<SignedReport>) {
    let w = world();
    let (_, mut ms) = walked(&w, 3).await;
    let mut rs = Vec::new();
    for i in 0..extra {
        let k = 900 + 10 * i128::from(i);
        let f = shard_files(k..k + 10, 10);
        let m = manifest(&f, ms.last(), T0 + 200 + u64::from(i));
        let r = sign(&m);
        w.node.anchor(&r);
        w.post(&m, &r).await.expect("own version");
        w.upload_all(&m, &f).await;
        ms.push(m);
        rs.push(r);
    }
    (w, ms, rs)
}

#[tokio::test]
async fn cat_1_fix_f2_a_history_node_without_the_answers_own_versions_is_undecided() {
    let (b, _, rs) = with_own_versions(2).await;
    let ck = checked_rows(&b).await;
    assert_eq!(ck.header.version, 4);
    // Another node with the same catalogs that does not hold versions 3, 4.
    let a = world();
    walked(&a, 3).await;
    for r in &rs {
        a.node.anchor(r);
    }
    let get = RouterGet(peer_routes(a.store.clone()));
    assert!(undecided(history_with(&a, &ck, &get).await).contains("may lag"));
    // On the node that holds them, it verifies.
    verified(history(&b, &opts(vec![], None)).await);
    for d in [&a.dir, &b.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn cat_1_fix_f3_the_answers_key_is_read_for_every_catalog_number() {
    // An own-report version 3 under a key that signed nothing in the walk,
    // whose key also signed another catalog 1: caught only through the
    // answer's key.
    let b = world();
    let (cs, ms) = walked(&b, 3).await;
    let f = shard_files(900..910, 10);
    let m3 = manifest(&f, Some(&ms[1]), T0 + 200);
    let r3 = onyx_bulk::anchor::sign(&SECRET2, &m3.header).expect("ok");
    b.node.anchor_by(&r3, &SECRET2);
    b.post(&m3, &r3).await.expect("v3 under the other key");
    b.upload_all(&m3, &f).await;
    verified(history(&b, &opts(vec![], None)).await);
    let alt = rewritten(&cs[1].0);
    let ra = cat::sign(&SECRET2, &alt.header).expect("ok");
    b.node.anchor_by(&ra, &SECRET2);
    let (fault, why) = refused(history(&b, &opts(vec![], None)).await);
    assert_eq!(fault, "publisher");
    assert!(why.contains("catalog 1"), "{why}");
    let _ = std::fs::remove_dir_all(&b.dir);
}

#[tokio::test]
async fn cat_1_fix_f3_an_own_report_answer_whose_chain_leaves_the_catalogs_is_the_publishers() {
    // Version 3 names a version 2 the catalogs never listed (its own report
    // only in the other node's book): the chain meets the catalogs' version
    // 2 with another root.
    let b = world();
    let (m1, _) = published(&b).await;
    let f2 = shard_files(800..810, 10);
    let v2 = manifest(&f2, Some(&m1), T0 + 6);
    let r2 = sign(&v2);
    b.node.anchor(&r2);
    b.post(&v2, &r2).await.expect("v2");
    b.upload_all(&v2, &f2).await;
    let f3 = shard_files(810..820, 10);
    let v3 = manifest(&f3, Some(&v2), T0 + 300);
    let r3 = sign(&v3);
    b.node.anchor(&r3);
    b.post(&v3, &r3).await.expect("v3");
    b.upload_all(&v3, &f3).await;
    let ck = checked_rows(&b).await;
    let a = world();
    walked(&a, 3).await;
    a.node.anchor(&r3);
    let get = RouterGet(peer_routes(a.store.clone()));
    let (fault, why) = refused(history_with(&a, &ck, &get).await);
    assert_eq!(fault, "publisher");
    assert!(why.contains("version 2"), "{why}");
    // Its report not anchored where the history is read: undecided.
    a.node.withdraw(&r3);
    a.node.dispute(&r3);
    assert!(undecided(history_with(&a, &ck, &get).await).contains("version 2"));
    for d in [&a.dir, &b.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn cat_1_fix_f3_a_version_the_book_holds_after_the_newest_seen_is_undecided() {
    let w = world();
    let (_, ms) = walked(&w, 3).await;
    let m3 = manifest(&shard_files(900..910, 10), Some(&ms[1]), T0 + 200);
    w.node.anchor(&sign(&m3));
    assert!(undecided(history(&w, &opts(vec![], None)).await).contains("version 3"));
    let _ = std::fs::remove_dir_all(&w.dir);
}

struct EmptyCalls;

impl JsonRpc for EmptyCalls {
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async { Ok(json!("0x")) })
    }
}

#[tokio::test]
async fn cat_1_fix_f3_an_empty_sequence_answer_is_no_answer() {
    let at = (0x64, Hash32(FIN));
    assert!(
        read_sequence_at(&EmptyCalls, &BOOK, &Hash32([1; 32]), 7, at)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cat_1_fix_f1_the_permit_goes_with_the_last_frame_not_the_first() {
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = sem.clone().try_acquire_owned().expect("free");
    let data: Vec<u8> = (0..(FILES_FRAME * 2 + 10)).map(|i| i as u8).collect();
    let mut body = FilesBody::new(data.clone(), permit);
    let mut got = Vec::new();
    let mut frames = 0;
    while let Some(f) = body.frame().await {
        let f = f.expect("frame").into_data().expect("data");
        assert!(f.len() <= FILES_FRAME);
        got.extend_from_slice(&f);
        frames += 1;
        // As hyper does: a body that says it has ended is dropped.
        if http_body::Body::is_end_stream(&body) {
            break;
        }
        assert_eq!(sem.available_permits(), 0, "held while frames remain");
    }
    assert_eq!(frames, 3);
    assert_eq!(got, data);
    assert_eq!(sem.available_permits(), 1, "let go with the last frame");
}

#[tokio::test]
async fn cat_1_fix_f4_an_answer_key_alone_is_read_even_when_its_version_is_listed() {
    // A rows answer for version 2 (listed in the catalogs, same root) under
    // the publisher's other key, which signed nothing in the walk and also
    // signed another catalog 1: only the answer's own key finds it.
    let w = world();
    let (cs, ms) = walked(&w, 3).await;
    let mut ck = checked_rows(&w).await;
    assert_eq!(ck.header.version, 2);
    let r2 = onyx_bulk::anchor::sign(&SECRET2, &ms[1].header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    ck.catalog = None;
    ck.digest = r2.payload.digest();
    ck.report_hash = r2.hash();
    ck.signer = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    ck.report = r2;
    let get = RouterGet(peer_routes(w.store.clone()));
    verified(history_with(&w, &ck, &get).await);
    let alt = rewritten(&cs[1].0);
    let ra = cat::sign(&SECRET2, &alt.header).expect("ok");
    w.node.anchor_by(&ra, &SECRET2);
    assert_eq!(refused(history_with(&w, &ck, &get).await).0, "publisher");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_1_fix_f4_the_answers_own_version_is_never_fetched_from_the_history_node() {
    // The history node does not hold the answer's own version 3, but holds
    // every version below it: the check needs nothing more from it.
    let (b, _, rs) = with_own_versions(1).await;
    let ck = checked_rows(&b).await;
    assert_eq!(ck.header.version, 3);
    let a = world();
    walked(&a, 3).await;
    a.node.anchor(&rs[0]);
    let get = RouterGet(peer_routes(a.store.clone()));
    verified(history_with(&a, &ck, &get).await);
    for d in [&a.dir, &b.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn cat_1_fix_f5_a_large_answer_gets_time_for_a_slow_honest_reader() {
    assert_eq!(files_deadline(1 << 20).as_secs(), FILES_SEND_SECS);
    let big = files_deadline(cat::MAX_FILES_BYTES + cat::MAX_CATALOG_BYTES);
    assert!(big.as_secs() >= 190, "24 MiB at 128 KiB/s: {big:?}");
}

// ADR-0059 amendment 4 item 5a: erasing a removed dataset's shards after
// the grace (THREATS CAT-3, design review CAT-ERASE-R1).

const DAY: u64 = 86_400;

impl Node {
    /// The finalized block is `block`, at time `ts`.
    fn at(&self, block: u64, ts: u64) {
        let mut c = self.0.lock().expect("ok");
        c.fin = Some(block);
        c.ts = Some(ts);
        c.times.insert(block, ts);
    }
}

fn erase_cfg(mode: EraseMode) -> EraseConfig {
    EraseConfig {
        mode,
        ..EraseConfig::default()
    }
}

fn world_erase(cfg: EraseConfig) -> World {
    let w = world();
    let c = w.clock.clone();
    let store = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(move || c.load(Ordering::Relaxed)),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("ok")
    .with_free_per_minute(10_000)
    .with_erase(cfg);
    World {
        node: w.node,
        store: started(store),
        clock: w.clock,
        dir: w.dir,
    }
}

struct Erase {
    w: World,
    m: Manifest,
    files: Vec<Vec<u8>>,
    other: Manifest,
    c1: Catalog,
    r1: SignedReport,
}

/// `synthetic.metrics` (3 shards) and `synthetic.other` (its first shard)
/// in catalog 0 (block 100, time `T0`); catalog 1 removes `synthetic.metrics`.
async fn erase_world(cfg: EraseConfig) -> Erase {
    let w = world_erase(cfg);
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let other = manifest_of("synthetic.other", &files[..1], T0);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), CatalogLeaf::of(&other.header)],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    w.post_in(&other, 0).await.expect("other");
    w.upload_all(&m, &files).await;
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&m.header).tombstone().expect("t"),
            CatalogLeaf::of(&other.header),
        ],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1).await.expect("c1").0,
        StatusCode::CREATED
    );
    Erase {
        w,
        m,
        files,
        other,
        c1,
        r1,
    }
}

impl Erase {
    fn roots(&self) -> Vec<Hash32> {
        self.m.shards.iter().map(|e| e.root).collect()
    }

    fn due(&self) -> Option<u64> {
        self.w
            .store
            .erase_json("example.publisher", "synthetic.metrics")
            .and_then(|v| v["due"].as_u64())
    }

    async fn pass(&self, block: u64, ts: u64) -> ErasePass {
        self.w.node.at(block, ts);
        self.w.store.erase_pass().await.expect("pass")
    }

    /// Two passes from block `from`, both after `due`: nothing planned or erased.
    async fn nothing(&self, from: u64, due: u64, what: &str) {
        for b in from..from + 2 {
            let p = self.pass(b, due + 10 * b).await;
            assert_eq!((p.planned, p.erased), (0, 0), "{what}");
        }
    }

    async fn other_rows(&self) -> Result<Value, ApiError> {
        self.w
            .store
            .rows(
                "example.publisher",
                "synthetic.other",
                &parse_rows_query(Some("from=0&to=3"))?,
            )
            .await
    }
}

#[tokio::test]
async fn cat_3_erase_waits_out_the_grace_then_erases_shards_and_keeps_manifests() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    let p = e.pass(100, T0).await;
    assert_eq!((p.waiting, p.planned, p.erased), (1, 0, 0));
    assert_eq!(e.due(), Some(due), "from the take of catalog 1, chain time");
    let gone = w.rows("from=0&to=3").await.expect_err("removed");
    assert_eq!(gone.code, "bulk_dataset_removed");
    assert!(gone
        .fields
        .iter()
        .any(|(k, v)| *k == "erase" && v == &json!({"phase": "waiting", "due": due})));
    assert_eq!(e.pass(101, due - 1).await.planned, 0, "not due yet");
    let p = e.pass(102, due).await;
    assert_eq!(
        (p.planned, p.erased),
        (1, 0),
        "a plan is logged once before it is carried out"
    );
    assert!(roots.iter().all(|r| s.holds_shard(r)));
    let p = e.pass(103, due + 3_600).await;
    assert_eq!((p.erased, p.shards, p.kept), (1, 2, 1));
    assert!(s.holds_shard(&roots[0]), "synthetic.other names it");
    for r in &roots[1..] {
        assert!(!s.holds_shard(r));
        assert!(s.shard_file(r).await.is_none());
        assert!(!w
            .dir
            .join("hot")
            .join("bulk")
            .join(hex::encode(r.0))
            .exists());
    }
    e.other_rows().await.expect("the other dataset is served");
    assert!(
        s.manifest_record(&e.m.header.root()).is_some(),
        "manifests stay"
    );
    let peer = peer_routes(s.clone());
    let read = routes(s.clone());
    let get = |u: String| Request::get(u).body(Body::empty()).expect("ok");
    let send = |app: Router, req: Request<Body>| async move {
        let r = app.oneshot(req).await.expect("ok");
        let status = r.status();
        let b = r.into_body().collect().await.expect("ok").to_bytes();
        (
            status,
            serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null),
        )
    };
    let uri = format!("/v1/bulk/shards/{}", roots[1]);
    let (st, v) = send(peer.clone(), get(uri.clone())).await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(v["error"]["code"], json!("bulk_shard_removed"));
    crate::openapi_check::check("GET", &uri, 410, &v);
    let (st, _) = send(peer.clone(), get(format!("/v1/bulk/shards/{}", roots[0]))).await;
    assert_eq!(st, StatusCode::OK, "a shared shard is still served");
    let (st, _) = send(
        peer.clone(),
        get(format!("/v1/bulk/manifests/{}", e.m.header.root())),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "the history check reads it");
    let (st, _) = send(
        peer.clone(),
        get("/v1/bulk/catalogs/example.publisher/1".into()),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = send(read.clone(), get("/v1/bulk".into())).await;
    assert_eq!(st, StatusCode::OK);
    crate::openapi_check::check("GET", "/v1/bulk", 200, &v);
    assert_eq!(
        v["catalogs"][0]["erase"],
        json!({"waiting": 0, "erased": 1})
    );
    let rows_uri = "/v1/bulk/example.publisher/synthetic.metrics/rows?from=0&to=3".to_string();
    let (st, v) = send(read.clone(), get(rows_uri.clone())).await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(v["erase"]["phase"], json!("erased"));
    crate::openapi_check::check("GET", &rows_uri, 410, &v);
    // A start keeps it, and nothing more goes.
    let again = w
        .reopen()
        .expect("starts")
        .with_erase(erase_cfg(EraseMode::On));
    assert!(again.erased_root(&roots[1]) && !again.holds_shard(&roots[1]));
    assert!(again.holds_shard(&roots[0]));
    w.node.at(104, due + 7_200);
    assert_eq!(again.erase_pass().await.expect("pass").erased, 0);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_reads_chain_time_never_the_host_clock() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    e.pass(100, T0 + 100).await;
    e.w.clock.store(T0 + 400 * DAY, Ordering::Relaxed);
    for b in 101..104 {
        let p = e.pass(b, T0 + 200).await;
        assert_eq!((p.planned, p.erased), (0, 0), "the chain says day 0");
    }
    assert!(e.roots().iter().all(|r| e.w.store.holds_shard(r)));
    let _ = std::fs::remove_dir_all(&e.w.dir);
}

#[tokio::test]
async fn cat_3_erase_a_compromised_retired_or_suspended_key_or_a_withdrawn_catalog_stops_it() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s) = (&e.w, &e.w.store);
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    w.node.life(&e.r1, 3);
    e.nothing(101, due, "a compromised key").await;
    w.node.life(&e.r1, 2);
    e.nothing(103, due, "a retired key").await;
    w.node.life(&e.r1, 1);
    w.node.suspend(&e.r1, true);
    e.nothing(105, due, "a suspended key").await;
    w.node.suspend(&e.r1, false);
    w.node.withdraw(&e.r1);
    e.nothing(107, due, "a withdrawn catalog").await;
    assert!(e.roots().iter().all(|r| s.holds_shard(r)));
    // Read anchored with an active key again: planned, then erased.
    w.node.anchor(&e.r1);
    assert_eq!(e.pass(200, due + 5_000).await.planned, 1);
    assert_eq!(e.pass(201, due + 9_000).await.erased, 1);
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_a_new_key_restarts_the_grace_and_two_keys_taking_turns_do_not() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let w = &e.w;
    e.pass(100, T0).await;
    assert_eq!(e.due(), Some(T0 + 30 * DAY));
    let leaves = || {
        vec![
            CatalogLeaf::of(&e.m.header).tombstone().expect("t"),
            CatalogLeaf::of(&e.other.header),
        ]
    };
    // Catalog 2 under the publisher's other key, taken at block 101.
    w.node.at(101, T0 + 5 * DAY);
    let c2 = catalog_of(Some(&e.c1), T0 + 30, leaves());
    let r2 = cat::sign(&SECRET2, &c2.header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    assert_eq!(
        w.post_catalog(&c2, &r2).await.expect("c2").0,
        StatusCode::CREATED
    );
    e.pass(102, T0 + 5 * DAY + 60).await;
    assert_eq!(
        e.due(),
        Some(T0 + 35 * DAY),
        "a new key starts the grace again"
    );
    // Catalog 3 under the first key: its run's first catalog under that key
    // is catalog 1, so the first grace stands (no restart by taking turns).
    w.node.at(103, T0 + 6 * DAY);
    let c3 = catalog_of(Some(&c2), T0 + 40, leaves());
    let r3 = sign_catalog(&c3);
    w.node.anchor(&r3);
    assert_eq!(
        w.post_catalog(&c3, &r3).await.expect("c3").0,
        StatusCode::CREATED
    );
    e.pass(104, T0 + 6 * DAY + 60).await;
    assert_eq!(e.due(), Some(T0 + 30 * DAY));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_a_root_named_meanwhile_is_kept_and_the_plan_waits_again() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    assert_eq!(e.pass(101, due).await.planned, 1);
    // Before the plan is carried out, catalog 2 adds a dataset naming the
    // second shard: the plan changes, so it is logged again, not carried out.
    w.node.at(102, due + 60);
    let third = manifest_of("synthetic.third", &e.files[1..2], T0);
    let c2 = catalog_of(
        Some(&e.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&e.m.header).tombstone().expect("t"),
            CatalogLeaf::of(&e.other.header),
            CatalogLeaf::of(&third.header),
        ],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    w.post_catalog(&c2, &r2).await.expect("c2");
    w.post_in(&third, 2).await.expect("third");
    let p = e.pass(103, due + 120).await;
    assert_eq!((p.planned, p.erased), (1, 0), "a changed plan waits again");
    let p = e.pass(104, due + 3_600).await;
    assert_eq!((p.erased, p.shards, p.kept), (1, 1, 2));
    assert!(s.holds_shard(&roots[0]) && s.holds_shard(&roots[1]));
    assert!(!s.holds_shard(&roots[2]));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_a_tombstone_withdrawn_after_erasure_takes_the_shards_again() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    e.pass(101, due).await;
    assert_eq!(e.pass(102, due + 3_600).await.erased, 1);
    assert!(!s.holds_shard(&roots[2]));
    // The book withdraws catalog 1 (its key was stolen); the publisher's
    // replacement catalog 1 under its new key lists the dataset again.
    w.node.withdraw(&e.r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    w.node.at(103, due + 4_000);
    let c1b = catalog_of(
        Some(
            &s.catalog_record(&publisher().0, 0)
                .expect("read")
                .expect("c0")
                .0,
        ),
        T0 + 50,
        vec![
            CatalogLeaf::of(&e.m.header),
            CatalogLeaf::of(&e.other.header),
        ],
    );
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    w.node.anchor_by(&r1b, &SECRET2);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(
        s.removed("example.publisher", "synthetic.metrics")
            .expect("read"),
        None
    );
    let p = e.pass(104, due + 5_000).await;
    assert_eq!(p.erased, 0);
    assert_eq!(e.due(), None, "the record is set aside");
    assert!(!s.erased_root(&roots[2]));
    // The publisher sends the shards again; rows are served.
    w.upload_all(&e.m, &e.files).await;
    w.rows("from=0&to=3").await.expect("served again");
    assert!(w.dir.join("removals/example.publisher/withdrawn").is_dir());
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_cut_short_resumes_after_a_restart() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    e.pass(101, due).await;
    *s.erase_stop.lock().expect("lock") = Some(1);
    w.node.at(102, due + 3_600);
    assert!(s.erase_pass().await.is_err(), "stopped after one file");
    // A dry run finishes nothing, and serves none of its roots meanwhile.
    let dry = w
        .reopen()
        .expect("starts")
        .with_erase(erase_cfg(EraseMode::DryRun));
    w.node.at(103, due + 5_000);
    assert_eq!(dry.erase_pass().await.expect("pass").erased, 0);
    assert!(dry.erased_root(&roots[1]) && dry.erased_root(&roots[2]));
    // The start finds no tracked shard without its file, and serves none of
    // the roots it began on.
    let again = w
        .reopen()
        .expect("starts")
        .with_erase(erase_cfg(EraseMode::On));
    assert!(again.erased_root(&roots[1]) && again.erased_root(&roots[2]));
    assert!(again.shard_file(&roots[2]).await.is_none());
    w.node.at(104, due + 7_200);
    let p = again.erase_pass().await.expect("finished");
    assert_eq!(p.erased, 1);
    for r in &roots[1..] {
        assert!(!w
            .dir
            .join("hot")
            .join("bulk")
            .join(hex::encode(r.0))
            .exists());
    }
    assert!(again.holds_shard(&roots[0]));
    w.reopen().expect("starts again");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_off_dry_run_and_hold() {
    let due = T0 + 30 * DAY;
    // Off: no record at all.
    let e = erase_world(erase_cfg(EraseMode::Off)).await;
    for b in 100..104 {
        assert_eq!(e.pass(b, due + b).await, ErasePass::default());
    }
    assert!(!e.w.dir.join("removals").exists());
    let _ = std::fs::remove_dir_all(&e.w.dir);
    // Dry run: records and one logged plan, never a deletion.
    let e = erase_world(erase_cfg(EraseMode::DryRun)).await;
    e.pass(100, T0).await;
    assert_eq!(e.pass(101, due).await.planned, 1);
    for b in 102..106 {
        let p = e.pass(b, due + b).await;
        assert_eq!((p.planned, p.erased), (0, 0));
    }
    assert!(e.roots().iter().all(|r| e.w.store.holds_shard(r)));
    assert!(
        e.roots().iter().all(|r| !e.w.store.erased_root(r)),
        "nothing marked"
    );
    // Switched on, the plan the dry run logged is carried out.
    let on =
        e.w.reopen()
            .expect("starts")
            .with_erase(erase_cfg(EraseMode::On));
    e.w.node.at(110, due + 9_000);
    assert_eq!(on.erase_pass().await.expect("pass").erased, 1);
    let _ = std::fs::remove_dir_all(&e.w.dir);
    // Hold, by publisher or by dataset.
    for hold in ["example.publisher", "example.publisher/synthetic.metrics"] {
        let mut cfg = erase_cfg(EraseMode::On);
        cfg.hold.insert(hold.into());
        let e = erase_world(cfg).await;
        e.pass(100, T0).await;
        for b in 101..104 {
            let p = e.pass(b, due + b).await;
            assert_eq!((p.planned, p.erased), (0, 0), "{hold}");
        }
        assert!(e.roots().iter().all(|r| e.w.store.holds_shard(r)));
        let _ = std::fs::remove_dir_all(&e.w.dir);
    }
}

#[tokio::test]
async fn cat_3_erase_a_lagging_finalized_block_erases_nothing() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    // Catalog 1 was read at block 100: a finalized block before it is behind.
    for b in [98, 99] {
        let p = e.pass(b, due + b).await;
        assert_eq!((p.planned, p.erased), (0, 0));
    }
    assert!(e.roots().iter().all(|r| e.w.store.holds_shard(r)));
    let _ = std::fs::remove_dir_all(&e.w.dir);
}

#[tokio::test]
async fn cat_3_erase_two_removed_datasets_sharing_a_root_erase_it_with_the_last() {
    let mut cfg = erase_cfg(EraseMode::On);
    cfg.max_shards = 1;
    let e = erase_world(cfg).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    // Catalog 2 removes synthetic.other too: both name the first shard.
    let c2 = catalog_of(
        Some(&e.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&e.m.header).tombstone().expect("t"),
            CatalogLeaf::of(&e.other.header).tombstone().expect("t"),
        ],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    w.post_catalog(&c2, &r2).await.expect("c2");
    e.pass(100, T0).await;
    assert_eq!(e.pass(101, due).await.planned, 2);
    // Each keeps the shared root while the other still holds it; the first
    // erase of a pass goes even over the budget of 1.
    // In the same pass synthetic.other's plan changes (the shared root is
    // its own now): logged again, carried out a pass later.
    let p = e.pass(102, due + 3_600).await;
    assert_eq!((p.erased, p.shards, p.kept, p.planned), (1, 2, 1, 1));
    assert!(s.holds_shard(&roots[0]));
    let p = e.pass(103, due + 7_200).await;
    assert_eq!((p.erased, p.shards), (1, 1));
    assert!(roots.iter().all(|r| !s.holds_shard(r)));
    assert!(s.manifest_record(&e.other.header.root()).is_some());
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_spends_its_budget_then_waits_for_the_next_pass() {
    let mut cfg = erase_cfg(EraseMode::On);
    cfg.max_shards = 1;
    let w = world_erase(cfg);
    let fx = shard_files(100..110, 10);
    let fy = shard_files(110..120, 10);
    let x = manifest_of("synthetic.x", &fx, T0);
    let y = manifest_of("synthetic.y", &fy, T0);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&x.header), CatalogLeaf::of(&y.header)],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&x, 0).await.expect("x");
    w.post_in(&y, 0).await.expect("y");
    for (m, f, d) in [(&x, &fx, "synthetic.x"), (&y, &fy, "synthetic.y")] {
        w.store
            .accept_shard(
                "example.publisher",
                d,
                &m.shards[0].root,
                &HeaderMap::new(),
                Body::from(f[0].clone()),
            )
            .await
            .expect("stored");
    }
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&x.header).tombstone().expect("t"),
            CatalogLeaf::of(&y.header).tombstone().expect("t"),
        ],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    let due = T0 + 30 * DAY;
    for (b, ts, want) in [
        (100, T0, (0, 0, 0)),
        (101, due, (2, 0, 0)),
        (102, due + 3_600, (0, 1, 1)),
        (103, due + 7_200, (0, 1, 1)),
    ] {
        w.node.at(b, ts);
        let p = w.store.erase_pass().await.expect("pass");
        assert_eq!((p.planned, p.erased, p.shards), want, "block {b}");
    }
    assert!(!w.store.holds_shard(&x.shards[0].root));
    assert!(!w.store.holds_shard(&y.shards[0].root));
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn cat_3_erase_a_root_stored_again_is_served_and_erased_bytes_leave_the_quota() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s, roots) = (&e.w, &e.w.store, e.roots());
    let due = T0 + 30 * DAY;
    // Quota: the removed dataset's bytes count until it is erased.
    let others = e.other.header.byte_count + s.catalog_bytes_of("example.publisher");
    let room = s.limits.quota_bytes - others;
    assert!(s.check_catalog_quota("example.publisher", room).is_err());
    e.pass(100, T0).await;
    e.pass(101, due).await;
    assert_eq!(e.pass(102, due + 3_600).await.erased, 1);
    s.check_catalog_quota("example.publisher", room)
        .expect("the erased dataset's bytes are gone");
    // Catalog 2 adds a dataset naming an erased root; its upload stores it
    // again, and it is served and no longer listed as erased.
    w.node.at(103, due + 4_000);
    let fourth = manifest_of("synthetic.fourth", &e.files[2..3], T0);
    let c2 = catalog_of(
        Some(&e.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&e.m.header).tombstone().expect("t"),
            CatalogLeaf::of(&e.other.header),
            CatalogLeaf::of(&fourth.header),
        ],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    w.post_catalog(&c2, &r2).await.expect("c2");
    let (_, v) = w.post_in(&fourth, 2).await.expect("fourth");
    assert_eq!(
        v["missing"].as_array().map(Vec::len),
        Some(1),
        "told to send it"
    );
    let (st, _) = s
        .accept_shard(
            "example.publisher",
            "synthetic.fourth",
            &roots[2],
            &HeaderMap::new(),
            Body::from(e.files[2].clone()),
        )
        .await
        .expect("stored again");
    assert_eq!(st, StatusCode::CREATED);
    assert!(!s.erased_root(&roots[2]) && s.erased_root(&roots[1]));
    assert!(s.shard_open(&roots[2]).await.is_some());
    let again = w
        .reopen()
        .expect("starts")
        .with_erase(erase_cfg(EraseMode::On));
    assert!(
        !again.erased_root(&roots[2]),
        "its record no longer lists it"
    );
    assert!(again.holds_shard(&roots[2]));
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Storage nodes learn of a removal (ADR-0059 amendment 4 item 5a slice 2a; review CAT-LEARN-R1)

const LP: &str = "example.publisher";
const LD: &str = "synthetic.metrics";

/// A leaf answer as `GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}` gives
/// it, for `dataset`'s leaf of `c` signed by `r`.
pub(crate) fn leaf_answer_of(c: &Catalog, r: &SignedReport, dataset: &str) -> Vec<u8> {
    let lp = c
        .leaf_proof(c.find(dataset).expect("listed"))
        .expect("proof");
    leaf_body_of(&c.header, &lp, r)
}

fn leaf_body_of(h: &CatalogHeader, lp: &LeafProof, r: &SignedReport) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "publisher": h.publisher.0, "dataset": lp.leaf.dataset, "number": h.number,
        "root": h.root().to_string(), "removed": lp.leaf.is_tombstone(),
        "header": hex0x(&borsh::to_vec(h).expect("ok")),
        "leaf": hex0x(&borsh::to_vec(lp).expect("ok")),
        "report": hex0x(&borsh::to_vec(r).expect("ok")),
        "from": "catalog",
    }))
    .expect("ok")
}

/// Catalog 0 of the publisher removing `m`'s dataset (beside the live
/// `synthetic.other`), anchored in `w`'s book and taken by `w` (which holds
/// `m` by its own report).
pub(crate) async fn removed_in_catalog(w: &World, m: &Manifest) -> (Catalog, SignedReport) {
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![
            CatalogLeaf::of(&m.header).tombstone().expect("t"),
            other_leaf("synthetic.other"),
        ],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("c0").0,
        StatusCode::CREATED
    );
    (c0, r0)
}

/// Catalog 0 listing `m` live, then catalog 1 removing its dataset, both
/// anchored in `w`'s book and taken by `w` (which holds `m` by its own report).
pub(crate) async fn live_then_removed(
    w: &World,
    m: &Manifest,
) -> ((Catalog, SignedReport), (Catalog, SignedReport)) {
    let (c0, r0) = live_catalog(m, T0 + 10);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![CatalogLeaf::of(&m.header).tombstone().expect("t")],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1).await.expect("c1").0,
        StatusCode::CREATED
    );
    ((c0, r0), (c1, r1))
}

/// A catalog listing `m` live, signed (not anchored anywhere).
pub(crate) fn live_catalog(m: &Manifest, created: u64) -> (Catalog, SignedReport) {
    let c = catalog_of(None, created, vec![CatalogLeaf::of(&m.header)]);
    let r = sign_catalog(&c);
    (c, r)
}

/// A catalog removing `m`'s dataset, signed (not anchored anywhere).
pub(crate) fn tombstone_catalog(m: &Manifest, created: u64) -> (Catalog, SignedReport) {
    let c = catalog_of(
        None,
        created,
        vec![CatalogLeaf::of(&m.header).tombstone().expect("t")],
    );
    let r = sign_catalog(&c);
    (c, r)
}

/// A catalog node `cat` (own-report version 1 of `synthetic.metrics`, then
/// catalog 0 removing it) and a storage node `sto` holding version 1 and its
/// shards by its own report and no catalog; `sto`'s book anchors catalog 0.
struct Learning {
    cat: World,
    sto: World,
    m: Manifest,
    files: Vec<Vec<u8>>,
    c0: Catalog,
    r0: SignedReport,
}

async fn learning() -> Learning {
    let cat = world();
    let (m, files) = published(&cat).await;
    let sto = world();
    let r = sign(&m);
    sto.node.anchor(&r);
    sto.post(&m, &r).await.expect("v1");
    sto.upload_all(&m, &files).await;
    let (c0, r0) = removed_in_catalog(&cat, &m).await;
    sto.node.anchor(&r0);
    Learning {
        cat,
        sto,
        m,
        files,
        c0,
        r0,
    }
}

impl Learning {
    fn clean(&self) {
        let _ = std::fs::remove_dir_all(&self.cat.dir);
        let _ = std::fs::remove_dir_all(&self.sto.dir);
    }
}

/// THREATS FUZZ-8 (review FUZZ-8-R1): the pure check the fuzz target calls is
/// the one `learn_from` composes: a good tombstone and a good live leaf pass
/// with the catalog key as signer; another publisher, another dataset, a
/// changed leaf and a body that is not JSON are refused.
#[tokio::test]
async fn fuzz_8_the_leaf_answer_check_is_the_storage_nodes() {
    let l = learning().await;
    let body = leaf_answer_of(&l.c0, &l.r0, LD);
    let (signer, removed) = check_leaf_answer(LP, LD, &body).expect("a good tombstone");
    assert!(removed);
    let (live, rl) = live_catalog(&l.m, T0 + 30);
    let (signer_live, removed_live) =
        check_leaf_answer(LP, LD, &leaf_answer_of(&live, &rl, LD)).expect("a good live leaf");
    assert!(!removed_live);
    assert_eq!(signer, signer_live);
    assert!(check_leaf_answer("other.publisher", LD, &body).is_err());
    assert!(check_leaf_answer(LP, "synthetic.other", &body).is_err());
    let other = leaf_answer_of(&l.c0, &l.r0, "synthetic.other");
    assert!(check_leaf_answer(LP, LD, &other).is_err());
    let mut v: Value = serde_json::from_slice(&body).expect("json");
    v["leaf"] = json!(hex0x(
        &borsh::to_vec(&l.c0.leaf_proof(1).expect("p")).expect("ok")
    ));
    assert!(check_leaf_answer(LP, LD, &serde_json::to_vec(&v).expect("ok")).is_err());
    assert!(check_leaf_answer(LP, LD, b"not json").is_err());
    l.clean();
}

/// `GET /v1/bulk/catalogs/{p}/leaf/{d}` on `w`, checked against the spec.
async fn leaf_get(w: &World, p: &str, d: &str) -> (StatusCode, Value, Vec<u8>) {
    let uri = format!("/v1/bulk/catalogs/{p}/leaf/{d}");
    let r = peer_routes(w.store.clone())
        .oneshot(Request::get(&uri).body(Body::empty()).expect("ok"))
        .await
        .expect("ok");
    let status = r.status();
    let b = r
        .into_body()
        .collect()
        .await
        .expect("ok")
        .to_bytes()
        .to_vec();
    let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
    crate::openapi_check::check("GET", &uri, status.as_u16(), &v);
    (status, v, b)
}

impl World {
    /// One answer for `synthetic.metrics` in a pass of its own.
    async fn learn(&self, body: &[u8]) -> Learn {
        let mut pass = self.store.learn_pass_start().await.expect("pass");
        self.store
            .learn_from(&mut pass, LP, LD, body, "test")
            .await
            .expect("read")
    }

    /// A pass that only reads the kept proofs again: (what the recheck did,
    /// records dropped at its end).
    async fn recheck(&self) -> Result<(Recheck, usize), String> {
        let mut pass = self.store.learn_pass_start().await?;
        let re = self.store.learn_recheck(&mut pass).await?;
        Ok((re, self.store.learn_finish(&mut pass)))
    }

    fn learned(&self) -> Option<learn::Learned> {
        self.store
            .learned_record(&(LP.to_string(), LD.to_string()))
            .expect("lock")
    }

    /// This world's store opened again on its folder with `mode`.
    pub(crate) fn with_mode(self, mode: RemovalsMode) -> World {
        let c = self.clock.clone();
        let store = BulkStore::open(
            pin(),
            self.node.clone(),
            Arc::new(move || c.load(Ordering::Relaxed)),
            &self.dir,
            &self.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("ok")
        .with_free_per_minute(10_000)
        .with_removals(mode);
        World {
            node: self.node,
            store: started(store),
            clock: self.clock,
            dir: self.dir,
        }
    }
}

fn is_gone(e: &ApiError) -> bool {
    e.status == StatusCode::GONE && e.code == "bulk_dataset_removed"
}

/// Slice 2a's route: the newest held catalog's leaf with its proof, from
/// memory (no chain read); removed true and false; 404 `bulk_no_leaf` for a
/// dataset or publisher it does not list; a node with no catalog answers
/// from a learned proof only; 409 under a fork mark; 500 when the newest
/// catalog is not held whole. Every answer checks against the spec.
#[tokio::test]
async fn cat_3_learn_the_route_answers_the_newest_leaf_with_its_proof() {
    let l = learning().await;
    let reads = l.cat.node.reads();
    let (s, v, _) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["removed"], json!(true));
    assert_eq!(v["from"], json!("catalog"));
    assert_eq!(v["number"], json!(0));
    assert_eq!(v["root"], json!(l.c0.header.root().to_string()));
    let h = CatalogHeader::try_from_slice(
        &onyx_bulk::wire::hex_field(&v["header"], "header").expect("hex"),
    )
    .expect("header");
    let lp =
        LeafProof::try_from_slice(&onyx_bulk::wire::hex_field(&v["leaf"], "leaf").expect("hex"))
            .expect("leaf");
    let rep = SignedReport::try_from_slice(
        &onyx_bulk::wire::hex_field(&v["report"], "report").expect("hex"),
    )
    .expect("report");
    assert_eq!(h, l.c0.header);
    h.proves(&lp).expect("proves");
    assert!(lp.leaf.is_tombstone() && lp.leaf.dataset == LD);
    assert_eq!(rep, l.r0);
    let (s, v, _) = leaf_get(&l.cat, LP, "synthetic.other").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["removed"], json!(false));
    for (p, d) in [(LP, "synthetic.none"), ("other.publisher", LD)] {
        let (s, v, _) = leaf_get(&l.cat, p, d).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{p}/{d}");
        assert_eq!(v["error"]["code"], json!("bulk_no_leaf"));
    }
    let (s, _, _) = leaf_get(&l.cat, LP, "bad..id").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(l.cat.node.reads(), reads, "no chain read on a request");
    // A storage node with no catalog: nothing, then its learned proof.
    let (s, v, _) = leaf_get(&l.sto, LP, LD).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], json!("bulk_no_leaf"));
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(l.sto.learn(&body).await, Learn::Kept(0));
    let (s, v, b2) = leaf_get(&l.sto, LP, LD).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["from"], json!("learned"));
    assert_eq!(v["removed"], json!(true));
    // A third node takes the relayed proof as it is.
    let third = world();
    let r = sign(&l.m);
    third.node.anchor(&r);
    third.post(&l.m, &r).await.expect("v1");
    third.node.anchor(&l.r0);
    assert_eq!(third.learn(&b2).await, Learn::Kept(0));
    let _ = std::fs::remove_dir_all(&third.dir);
    // A node holding the catalogs never answers from a learned proof: the
    // catalog node's live leaf of another dataset stays its own.
    assert!(l.cat.learned().is_none());
    // Under a fork mark: 409.
    let (cx, rx) = live_catalog(&l.m, T0 + 11);
    l.cat
        .store
        .mark_fork(&Fork {
            held: (l.c0.header.clone(), l.r0.clone()),
            other: (cx.header.clone(), rx),
            why: "test".into(),
        })
        .expect("marked");
    let (s, v, _) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], json!("bulk_publisher_forked"));
    l.cat.store.forks.write().expect("ok").clear();
    // The newest catalog not held whole: 500, never "no leaf".
    l.cat
        .store
        .catalogs
        .write()
        .expect("ok")
        .get_mut(LP)
        .and_then(|m| m.values_mut().next_back())
        .expect("held")
        .body = None;
    let (s, _, _) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    l.clean();
}

/// The leaf route and the shard `PUT` share the node's router (the read,
/// upload and peer routes merged as the node does): the leaf answers, a shard
/// `PUT` still reaches its route (here its 410 with `learned`, and for a
/// publisher named `catalogs` its 404), and a `PUT` on the leaf path is 405.
#[tokio::test]
async fn cat_3_learn_the_leaf_route_and_the_shard_put_share_the_router() {
    let l = learning().await;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(l.sto.learn(&body).await, Learn::Kept(0));
    let s = l.sto.store.clone();
    let app = routes(s.clone())
        .merge(upload_routes(s.clone()))
        .merge(peer_routes(s));
    let send = |req: Request<Body>| {
        let app = app.clone();
        async move {
            let (m, u) = (req.method().to_string(), req.uri().to_string());
            let r = app.oneshot(req).await.expect("ok");
            let status = r.status();
            let b = r.into_body().collect().await.expect("ok").to_bytes();
            let v = serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null);
            crate::openapi_check::check(&m, &u, status.as_u16(), &v);
            (status, v)
        }
    };
    let leaf = format!("/v1/bulk/catalogs/{LP}/leaf/{LD}");
    let (st, v) = send(Request::get(&leaf).body(Body::empty()).expect("ok")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["from"], json!("learned"));
    let root = l.m.shards[0].root;
    let (st, v) = send(
        Request::put(format!("/v1/bulk/{LP}/{LD}/shards/{root}"))
            .body(Body::from(l.files[0].clone()))
            .expect("ok"),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(v["learned"]["number"], json!(0));
    let (st, v) = send(
        Request::put(format!("/v1/bulk/catalogs/leaf/shards/{root}"))
            .body(Body::from(l.files[0].clone()))
            .expect("ok"),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], json!("bulk_unknown_dataset"));
    let (st, _) = send(Request::put(&leaf).body(Body::empty()).expect("ok")).await;
    assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED);
    l.clean();
}

/// A verified tombstone stops rows, info and the listing with 410 and
/// `learned`, and the public uploads; `removed()` stays catalog-only; the
/// storage node's own fetch paths (a manifest, a shard), the shard and
/// manifest routes by root still answer.
#[tokio::test]
async fn cat_3_learn_a_verified_tombstone_stops_rows_info_and_listing_not_the_storage_paths() {
    let l = learning().await;
    let w = &l.sto;
    w.rows("from=0&to=3").await.expect("served before");
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    assert_eq!(w.store.removed(LP, LD).expect("read"), None, "catalog-only");
    let e = w.rows("from=0&to=3").await.expect_err("removed");
    assert!(is_gone(&e));
    let read = routes(w.store.clone());
    let up = upload_routes(w.store.clone());
    let peer = peer_routes(w.store.clone());
    let send = |app: Router, req: Request<Body>| async move {
        let (m, u) = (req.method().to_string(), req.uri().to_string());
        let r = app.oneshot(req).await.expect("ok");
        let status = r.status();
        let b = r.into_body().collect().await.expect("ok").to_bytes();
        let v = serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null);
        if v != Value::Null {
            crate::openapi_check::check(&m, &u, status.as_u16(), &v);
        }
        (status, v)
    };
    let get = |u: String| Request::get(u).body(Body::empty()).expect("ok");
    for uri in [
        format!("/v1/bulk/{LP}/{LD}"),
        format!("/v1/bulk/{LP}/{LD}/rows?from=0&to=3"),
    ] {
        let (s, v) = send(read.clone(), get(uri.clone())).await;
        assert_eq!(s, StatusCode::GONE, "{uri}");
        assert_eq!(v["error"]["code"], json!("bulk_dataset_removed"));
        assert_eq!(v["catalog"], json!(0));
        assert_eq!(
            v["learned"],
            json!({
                "number": 0,
                "root": l.c0.header.root().to_string(),
                "checked_at": {"number": 100, "hash": hex0x(&FIN)},
            })
        );
    }
    let (s, v) = send(read.clone(), get("/v1/bulk".into())).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!v.to_string().contains(LD), "left out of the list");
    // The public uploads.
    let own = json!({"manifest": hex0x(&l.m.to_bytes()), "report": hex0x(&borsh::to_vec(&sign(&l.m)).expect("ok"))});
    for body in [
        own,
        json!({"manifest": hex0x(&l.m.to_bytes()), "catalog": 0}),
    ] {
        let (s, v) = send(
            up.clone(),
            Request::post("/v1/bulk/manifests")
                .body(Body::from(body.to_string()))
                .expect("ok"),
        )
        .await;
        assert_eq!(s, StatusCode::GONE);
        assert_eq!(v["learned"]["number"], json!(0));
    }
    assert!(is_gone(&w.post_in(&l.m, 0).await.expect_err("removed")));
    let (s, _) = send(
        up.clone(),
        Request::put(format!("/v1/bulk/{LP}/{LD}/shards/{}", l.m.shards[0].root))
            .body(Body::from(l.files[0].clone()))
            .expect("ok"),
    )
    .await;
    assert_eq!(s, StatusCode::GONE);
    // The storage node's own fetches and the peer routes by root.
    let r = sign(&l.m);
    w.post(&l.m, &r)
        .await
        .expect("its own fetch of a deal's manifest");
    w.store
        .put_shard(LP, LD, &l.m.shards[0].root, l.files[0].clone())
        .await
        .expect("its own fetch of a deal's shard");
    let (s, _) = send(
        peer.clone(),
        get(format!("/v1/bulk/manifests/{}", l.m.header.root())),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        peer.clone(),
        get(format!("/v1/bulk/shards/{}", l.m.shards[0].root)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // The erase pass reads catalogs only: nothing recorded.
    w.store.erase_pass().await.expect("pass");
    assert!(w.store.erase_json(LP, LD).is_none());
    l.clean();
}

/// A restart keeps a learned removal (loaded before any route serves); a
/// stray temporary file is no record.
#[tokio::test]
async fn cat_3_learn_a_restart_keeps_it() {
    let l = learning().await;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(l.sto.learn(&body).await, Learn::Kept(0));
    let dir = l.sto.dir.join("learned").join(LP);
    assert!(dir.join(format!("{LD}.bin")).exists());
    std::fs::write(dir.join(format!("{LD}.tmp")), b"cut short").expect("ok");
    let again = Arc::new(l.sto.reopen().expect("starts"));
    assert!(again.learned_removal(LP, LD).expect("ok").is_some());
    let e = again
        .rows(LP, LD, &parse_rows_query(Some("from=0&to=3")).expect("q"))
        .await
        .expect_err("removed");
    assert!(is_gone(&e));
    l.clean();
}

/// Review CAT-LEARN-R2 2: a damaged learned record, a record at another
/// dataset's path, a stray file beside the publishers' folders and a file
/// that is no `.bin` never stop the start, whatever the mode: a bad record
/// moves to `learned/<publisher>/damaged/` and the good ones load.
#[tokio::test]
async fn cat_3_learn_a_damaged_record_is_set_aside_never_stopping_the_start() {
    let l = learning().await;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(l.sto.learn(&body).await, Learn::Kept(0));
    let top = l.sto.dir.join("learned");
    let dir = top.join(LP);
    std::fs::write(dir.join("synthetic.junk.bin"), b"junk").expect("ok");
    std::fs::copy(
        dir.join(format!("{LD}.bin")),
        dir.join("synthetic.copy.bin"),
    )
    .expect("ok");
    std::fs::write(top.join("stray.bin"), b"not a folder").expect("ok");
    std::fs::write(dir.join("notes.txt"), b"not a record").expect("ok");
    for mode in [RemovalsMode::Off, RemovalsMode::On] {
        let s = l
            .sto
            .reopen()
            .expect("a learned record never stops the start");
        let s = s.with_removals(mode);
        assert_eq!(s.learned_count(), 1, "{mode:?}: the good record loads");
        assert_eq!(
            s.learned_removal(LP, LD).expect("ok").is_some(),
            mode == RemovalsMode::On
        );
    }
    let damaged = dir.join("damaged");
    assert!(damaged.join("synthetic.junk.bin").exists());
    assert!(damaged.join("synthetic.copy.bin").exists());
    assert!(!dir.join("synthetic.junk.bin").exists());
    assert!(top.join("stray.bin").exists() && dir.join("notes.txt").exists());
    l.clean();
}

/// Review CAT-LEARN-R2 3: records are written outside the state's lock; a
/// write that fails keeps the removal in memory (fail closed) and is written
/// at the next recheck; `checked` moves in memory only; and a write that
/// fails never stops a drop: the dataset serves again, and the file left
/// behind is read at start and dropped again.
#[tokio::test]
async fn cat_3_learn_a_write_failure_never_stops_a_drop_or_a_removal() {
    let l = learning().await;
    let w = &l.sto;
    let file = w.dir.join("learned").join(LP).join(format!("{LD}.bin"));
    let fault = |on: bool| w.store.learn_fault.store(on, Ordering::Relaxed);
    fault(true);
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    assert!(!file.exists());
    assert!(is_gone(
        &w.rows("from=0&to=3").await.expect_err("kept in memory")
    ));
    assert_eq!(w.recheck().await.expect("read").0.kept, 1);
    fault(false);
    w.recheck().await.expect("read");
    assert!(file.exists(), "written at the next recheck");
    let bytes = std::fs::read(&file).expect("ok");
    w.node.at(200, T0 + 100);
    w.recheck().await.expect("read");
    assert_eq!(
        std::fs::read(&file).expect("ok"),
        bytes,
        "not written for `checked`"
    );
    let (_, v) = w.store.learned_removal(LP, LD).expect("ok").expect("kept");
    assert_eq!(v["checked_at"]["number"], json!(200));
    fault(true);
    w.node.withdraw(&l.r0);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!((re.withdrawn, dropped), (1, 1));
    w.rows("from=0&to=3").await.expect("served again");
    assert!(file.exists(), "the move failed");
    fault(false);
    let again = Arc::new(w.reopen().expect("starts"));
    assert_eq!(again.learned_count(), 1, "read again at start");
    let mut pass = again.learn_pass_start().await.expect("pass");
    again.learn_recheck(&mut pass).await.expect("read");
    assert_eq!(again.learn_finish(&mut pass), 1, "and dropped again");
    assert!(!file.exists());
    again
        .rows(LP, LD, &parse_rows_query(Some("from=0&to=3")).expect("q"))
        .await
        .expect("served");
    l.clean();
}

/// Review CAT-LEARN-R2 4: every kept proof is read again each pass, each
/// key's first included, and goes only on its own withdrawal: a first that
/// goes leaves the newest; a newest that goes leaves the older proof that
/// holds (served as removed, asked again).
#[tokio::test]
async fn cat_3_learn_each_proof_is_read_again_and_goes_only_on_its_own_withdrawal() {
    for gone in ["first", "newest"] {
        let l = learning().await;
        let w = &l.sto;
        let (_, _, body0) = leaf_get(&l.cat, LP, LD).await;
        assert_eq!(w.learn(&body0).await, Learn::Kept(0));
        let c1 = catalog_of(
            Some(&l.c0),
            T0 + 20,
            vec![
                CatalogLeaf::of(&l.m.header).tombstone().expect("t"),
                other_leaf("synthetic.other"),
            ],
        );
        let r1 = sign_catalog(&c1);
        w.node.anchor(&r1);
        assert_eq!(w.learn(&leaf_answer_of(&c1, &r1, LD)).await, Learn::Kept(1));
        assert_eq!(w.learned().expect("kept").first().number(), 0);
        w.node.withdraw(if gone == "first" { &l.r0 } else { &r1 });
        let mut pass = w.store.learn_pass_start().await.expect("pass");
        let re = w.store.learn_recheck(&mut pass).await.expect("read");
        let reask = pass.reask();
        assert_eq!(w.store.learn_finish(&mut pass), 0, "{gone}");
        drop(pass);
        let r = w.learned().expect("kept");
        if gone == "first" {
            assert_eq!((re.firsts_gone, re.moved), (1, 0));
            assert_eq!((r.newest.number(), r.first().number()), (1, 1));
            assert!(reask.is_empty());
        } else {
            assert_eq!((re.firsts_gone, re.moved), (0, 1));
            assert_eq!((r.newest.number(), r.first().number()), (0, 0));
            assert_eq!(reask, vec![(LP.to_string(), LD.to_string())]);
        }
        assert!(r.firsts.is_empty());
        assert!(is_gone(&w.rows("from=0&to=3").await.expect_err("removed")));
        l.clean();
    }
}

/// Review CAT-LEARN-R2 4: when the book withdraws the only kept proof, the
/// dataset stays removed while the pass asks about it again; a proof that
/// holds (here the publisher's replacement catalog 0, with the withdrawn
/// one's number) replaces it, the withdrawn record is kept aside, and the
/// dataset never served in between. Without one, the end of the pass drops it.
#[tokio::test]
async fn cat_3_learn_a_withdrawn_newest_is_asked_again_before_serving_resumes() {
    let l = learning().await;
    let w = &l.sto;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    // The publisher's replacement under its next key, anchored.
    let (cb, _) = tombstone_catalog(&l.m, T0 + 15);
    let rb = cat::sign(&SECRET2, &cb.header).expect("ok");
    w.node.anchor_by(&rb, &SECRET2);
    w.node.withdraw(&l.r0);
    let mut pass = w.store.learn_pass_start().await.expect("pass");
    let re = w.store.learn_recheck(&mut pass).await.expect("read");
    assert_eq!(re.withdrawn, 1);
    assert_eq!(pass.reask(), vec![(LP.to_string(), LD.to_string())]);
    assert!(is_gone(
        &w.rows("from=0&to=3").await.expect_err("still removed")
    ));
    assert_eq!(
        w.store
            .learn_from(&mut pass, LP, LD, &body, "test")
            .await
            .expect("read"),
        Learn::Refused(
            "its catalog does not read anchored (fact_not_accepted_on_this_report)".into()
        ),
        "the withdrawn proof again"
    );
    assert_eq!(
        w.store
            .learn_from(&mut pass, LP, LD, &leaf_answer_of(&cb, &rb, LD), "test")
            .await
            .expect("read"),
        Learn::Kept(0)
    );
    assert_eq!(w.store.learn_finish(&mut pass), 0);
    drop(pass);
    let r = w.learned().expect("kept");
    assert_eq!(r.newest.header.root(), cb.header.root());
    assert_eq!(r.first().key(), rb.payload.key);
    assert!(is_gone(&w.rows("from=0&to=3").await.expect_err("removed")));
    assert!(w
        .dir
        .join("learned")
        .join(LP)
        .join("withdrawn")
        .read_dir()
        .expect("kept aside")
        .next()
        .is_some());
    // Withdrawn too, and nobody gives another: dropped at the pass's end.
    w.node.withdraw(&rb);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!((re.withdrawn, dropped), (1, 1));
    w.rows("from=0&to=3").await.expect("served again");
    l.clean();
}

/// Review CAT-LEARN-R2 6: a catalog with the kept proof's number and another
/// root is not kept (one number, two catalogs: the kept one stays).
#[tokio::test]
async fn cat_3_learn_another_catalog_with_the_kept_number_is_not_kept() {
    let l = learning().await;
    let w = &l.sto;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    let (cb, rb) = tombstone_catalog(&l.m, T0 + 15);
    w.node.anchor(&rb);
    assert_eq!(
        w.learn(&leaf_answer_of(&cb, &rb, LD)).await,
        Learn::Refused("another catalog with the kept one's number".into())
    );
    assert_eq!(
        w.learned().expect("kept").newest.header.root(),
        l.c0.header.root()
    );
    l.clean();
}

/// Each forgery changes nothing: another dataset's or another publisher's
/// tombstone, a live leaf, a bad path, a bad signature, an unanchored report,
/// a `prev` that is not the held version 1's root, an answer that is no JSON.
#[tokio::test]
async fn cat_3_learn_each_forgery_changes_nothing() {
    let l = learning().await;
    let w = &l.sto;
    let gone_leaf = |publisher: SourceId, dataset: &str, prev: Hash32| CatalogLeaf {
        publisher,
        dataset: dataset.into(),
        version: 2,
        prev,
        root: Hash32::ZERO,
    };
    let anchored = |c: &Catalog| {
        let r = cat::sign(&SECRET, &c.header).expect("ok");
        w.node.anchor(&r);
        r
    };
    let mroot = l.m.header.root();
    // (what, the answer, the reason it is refused for)
    let mut forged: Vec<(&str, Vec<u8>, &str)> = Vec::new();
    // Another dataset's tombstone, anchored.
    let ca = catalog_of(
        None,
        T0 + 30,
        vec![gone_leaf(publisher(), "synthetic.gone", Hash32([5; 32]))],
    );
    let ra = anchored(&ca);
    forged.push((
        "another dataset",
        leaf_answer_of(&ca, &ra, "synthetic.gone"),
        "the leaf names another dataset",
    ));
    // Another publisher's tombstone of this dataset id, anchored.
    let other = SourceId("other.publisher".into());
    let cb = Catalog::build(
        other.clone(),
        None,
        T0 + 30,
        vec![gone_leaf(other, LD, mroot)],
    )
    .expect("catalog");
    let rb = anchored(&cb);
    forged.push((
        "another publisher",
        leaf_answer_of(&cb, &rb, LD),
        "another publisher's",
    ));
    // A live leaf, anchored.
    let (cl, rl) = live_catalog(&l.m, T0 + 30);
    w.node.anchor(&rl);
    forged.push(("a live leaf", leaf_answer_of(&cl, &rl, LD), "live"));
    // The real tombstone with a bad path, a bad index, a bad signature.
    let real = l.c0.leaf_proof(l.c0.find(LD).expect("listed")).expect("p");
    let mut bad = real.clone();
    bad.proof.siblings[0].0[0] ^= 1;
    forged.push((
        "a bad path",
        leaf_body_of(&l.c0.header, &bad, &l.r0),
        "catalog proof",
    ));
    let mut bad = real.clone();
    bad.proof.index += 1;
    forged.push((
        "a bad index",
        leaf_body_of(&l.c0.header, &bad, &l.r0),
        "catalog proof",
    ));
    let mut sig = l.r0.clone();
    sig.signature[5] ^= 1;
    forged.push((
        "a bad signature",
        leaf_body_of(&l.c0.header, &real, &sig),
        "the report:",
    ));
    // A tombstone whose report the book does not hold.
    let (cu, ru) = tombstone_catalog(&l.m, T0 + 40);
    forged.push((
        "an unanchored report",
        leaf_answer_of(&cu, &ru, LD),
        "does not read anchored",
    ));
    // A tombstone at version 2 naming another version 1 than the held one.
    let cf = catalog_of(
        None,
        T0 + 50,
        vec![gone_leaf(publisher(), LD, Hash32([7; 32]))],
    );
    let rf = anchored(&cf);
    forged.push(("a fork", leaf_answer_of(&cf, &rf, LD), "a fork"));
    // Tombstones at version 1 or with no previous root, in hand-made
    // catalogs (`Catalog::build` refuses them), anchored.
    for (version, what) in [
        (1, "a tombstone at version 1"),
        (2, "a tombstone with no previous root"),
    ] {
        let leaves = vec![CatalogLeaf {
            publisher: publisher(),
            dataset: LD.into(),
            version,
            prev: Hash32::ZERO,
            root: Hash32::ZERO,
        }];
        let c = Catalog {
            header: CatalogHeader {
                format: cat::FORMAT,
                publisher: publisher(),
                number: 0,
                prev: Hash32::ZERO,
                created_unix: T0 + 60 + u64::from(version),
                leaf_count: 1,
                leaves_root: cat::leaves_tree(&leaves).root(),
            },
            leaves,
        };
        let r = anchored(&c);
        forged.push((what, leaf_answer_of(&c, &r, LD), "a tombstone at version 1"));
    }
    forged.push(("no JSON", b"<html>".to_vec(), "not JSON"));
    for (what, body, why) in &forged {
        match w.learn(body).await {
            Learn::Refused(r) => assert!(r.contains(why), "{what}: refused for {r:?}"),
            Learn::Live(n) => assert_eq!((*why, n), ("live", 0), "{what}"),
            got => panic!("{what}: {got:?}"),
        }
        assert!(w.learned().is_none(), "{what}: kept");
        assert_eq!(w.store.learned_count(), 0, "{what}");
        w.rows("from=0&to=3").await.expect(what);
    }
    assert!(!w
        .dir
        .join("learned")
        .join(LP)
        .join(format!("{LD}.bin"))
        .exists());
    l.clean();
}

/// The book's final word on the kept proof's catalog drops it (moved aside)
/// and the dataset serves again; the same answer is not kept again.
#[tokio::test]
async fn cat_3_learn_a_withdrawn_catalog_drops_it_and_serving_resumes() {
    let l = learning().await;
    let w = &l.sto;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    assert!(is_gone(&w.rows("from=0&to=3").await.expect_err("removed")));
    w.node.withdraw(&l.r0);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!((re.kept, re.withdrawn, dropped), (0, 1, 1));
    w.rows("from=0&to=3").await.expect("served again");
    assert!(w.learned().is_none());
    assert!(w
        .dir
        .join("learned")
        .join(LP)
        .join("withdrawn")
        .read_dir()
        .expect("moved aside")
        .next()
        .is_some());
    assert!(matches!(w.learn(&body).await, Learn::Refused(_)));
    let (s, _, _) = leaf_get(w, LP, LD).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Withdrawn for good: a restart does not bring it back.
    assert!(w
        .reopen()
        .expect("starts")
        .learned_removal(LP, LD)
        .expect("ok")
        .is_none());
    l.clean();
}

/// A suspended key, an open dispute, a retired key and a chain that cannot
/// be read keep the removal (review CAT-LEARN-R1 M2: only the book's final
/// word drops it).
#[tokio::test]
async fn cat_3_learn_a_suspension_a_dispute_a_retired_key_or_a_failed_read_keeps_it() {
    let l = learning().await;
    let w = &l.sto;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body).await, Learn::Kept(0));
    w.node.suspend(&l.r0, true);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!((re.kept, re.open, dropped), (1, 1, 0));
    assert!(is_gone(&w.rows("from=0&to=3").await.expect_err("kept")));
    w.node.suspend(&l.r0, false);
    w.node.life(&l.r0, 2);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!(
        (re.kept, dropped),
        (1, 0),
        "a retired key's past catalog stays"
    );
    w.node.life(&l.r0, 1);
    w.node.dispute(&l.r0);
    let (re, dropped) = w.recheck().await.expect("read");
    assert_eq!((re.kept, re.open, dropped), (1, 1, 0));
    w.node.anchor(&l.r0);
    w.node.0.lock().expect("ok").down = true;
    assert!(w.recheck().await.is_err());
    w.node.0.lock().expect("ok").down = false;
    assert!(w.learned().is_some());
    assert!(is_gone(&w.rows("from=0&to=3").await.expect_err("kept")));
    l.clean();
}

/// Held versions at or after the tombstone's do not stop it (the publisher's
/// fault, logged; review CAT-LEARN-R1 M1), and a node that does not hold the
/// version before it takes it too (dataset ids are final, item 2).
#[tokio::test]
async fn cat_3_learn_held_versions_at_or_after_the_tombstone_do_not_stop_it() {
    let cat = world();
    let (m1, m2) = own_two(&cat).await;
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m1.header).tombstone().expect("t")],
    );
    let r0 = sign_catalog(&c0);
    cat.node.anchor(&r0);
    cat.post_catalog(&c0, &r0)
        .await
        .expect("taken over version 2");
    let sto = world();
    own_two(&sto).await;
    sto.node.anchor(&r0);
    let (_, _, body) = leaf_get(&cat, LP, LD).await;
    assert_eq!(sto.learn(&body).await, Learn::Kept(0));
    assert!(is_gone(
        &sto.rows("from=0&to=3").await.expect_err("removed")
    ));
    // Version 3 removed while this node holds version 1 only.
    let one = world();
    let r1 = sign(&m1);
    one.node.anchor(&r1);
    one.post(&m1, &r1).await.expect("v1");
    let c = catalog_of(
        None,
        T0 + 20,
        vec![CatalogLeaf::of(&m2.header).tombstone().expect("t")],
    );
    let r = sign_catalog(&c);
    one.node.anchor(&r);
    assert_eq!(one.learn(&leaf_answer_of(&c, &r, LD)).await, Learn::Kept(0));
    for w in [&cat, &sto, &one] {
        let _ = std::fs::remove_dir_all(&w.dir);
    }
}

/// `first` stays while `newest` moves to later catalogs under the same key
/// (review CAT-LEARN-R1 M3); an older proof changes nothing; a newer one
/// under another key starts that key's `first`, and keys taking turns (A, B,
/// A) never restart it: slice 1's rule, the first catalog under the newest
/// key (review CAT-LEARN-R2 4).
#[tokio::test]
async fn cat_3_learn_first_stays_while_newest_moves() {
    let l = learning().await;
    let w = &l.sto;
    let (_, _, body0) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(w.learn(&body0).await, Learn::Kept(0));
    let r = w.learned().expect("kept");
    let f = r.first();
    assert_eq!((f.number(), f.seen_at, r.newest.number()), (0, T0, 0));
    assert_eq!(f.key(), l.r0.payload.key);
    // Later, catalog 1 keeps the tombstone.
    w.node.at(200, T0 + 1_000);
    let c1 = catalog_of(
        Some(&l.c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&l.m.header).tombstone().expect("t"),
            other_leaf("synthetic.more"),
            other_leaf("synthetic.other"),
        ],
    );
    let r1 = sign_catalog(&c1);
    l.cat.node.anchor(&r1);
    l.cat.post_catalog(&c1, &r1).await.expect("c1");
    w.node.anchor(&r1);
    let (_, v, body1) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(v["number"], json!(1));
    assert_eq!(w.learn(&body1).await, Learn::Kept(1));
    let r = w.learned().expect("kept");
    let f = r.first();
    assert_eq!((f.number(), f.seen_at, r.newest.number()), (0, T0, 1));
    assert_eq!(r.checked.0, 200);
    assert_eq!(w.learn(&body0).await, Learn::Older(0));
    assert_eq!(w.learn(&body1).await, Learn::Same(1));
    assert_eq!(w.learned().expect("kept").newest.number(), 1);
    // Catalog 2 under the publisher's next key B: B's `first` starts.
    w.node.at(300, T0 + 2_000);
    let c2 = catalog_of(Some(&c1), T0 + 30, c1.leaves.clone());
    let r2 = cat::sign(&SECRET2, &c2.header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    assert_eq!(w.learn(&leaf_answer_of(&c2, &r2, LD)).await, Learn::Kept(2));
    let r = w.learned().expect("kept");
    assert_eq!((r.first().number(), r.first().seen_at), (2, T0 + 2_000));
    assert_eq!(r.first().key(), r2.payload.key);
    // Catalog 3 under key A again: A's first (catalog 0) is the first again.
    w.node.at(400, T0 + 3_000);
    let c3 = catalog_of(Some(&c2), T0 + 40, c1.leaves.clone());
    let r3 = sign_catalog(&c3);
    w.node.anchor(&r3);
    assert_eq!(w.learn(&leaf_answer_of(&c3, &r3, LD)).await, Learn::Kept(3));
    let r = w.learned().expect("kept");
    assert_eq!((r.first().number(), r.first().seen_at), (0, T0));
    assert_eq!(
        r.firsts.iter().map(|p| p.number()).collect::<Vec<_>>(),
        vec![0, 2],
        "each key's first, nothing else"
    );
    // A restart keeps every first with its report.
    let again = w.reopen().expect("starts");
    let back = again
        .learned_record(&(LP.to_string(), LD.to_string()))
        .expect("lock")
        .expect("kept");
    assert_eq!((back.first().number(), back.newest.number()), (0, 3));
    l.clean();
}

/// `log` keeps records and logs, nothing served changes; `off` uses no
/// record (rows serve, the route has no learned leaf); `on` again stops rows.
#[tokio::test]
async fn cat_3_learn_log_and_off_modes() {
    let l = learning().await;
    let Learning { cat, sto, c0, .. } = l;
    let sto = sto.with_mode(RemovalsMode::Log);
    let (_, _, body) = leaf_get(&cat, LP, LD).await;
    assert_eq!(sto.learn(&body).await, Learn::Kept(0));
    assert_eq!(sto.store.learned_count(), 1);
    sto.rows("from=0&to=3").await.expect("log: served");
    assert!(sto.store.learned_removal(LP, LD).expect("ok").is_none());
    let list = sto
        .store
        .list(&parse_list_query(None).expect("q"))
        .expect("ok");
    assert!(list.to_string().contains(LD), "log: listed");
    let (s, v, _) = leaf_get(&sto, LP, LD).await;
    assert_eq!((s, v["from"].clone()), (StatusCode::OK, json!("learned")));
    let sto = sto.with_mode(RemovalsMode::Off);
    assert_eq!(sto.store.learned_count(), 1, "loaded, not used");
    sto.rows("from=0&to=3").await.expect("off: served");
    let (s, _, _) = leaf_get(&sto, LP, LD).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let sto = sto.with_mode(RemovalsMode::On);
    let e = sto.rows("from=0&to=3").await.expect_err("on");
    assert!(is_gone(&e));
    assert_eq!(e.status, StatusCode::GONE);
    assert_eq!(c0.header.number, 0);
    let _ = std::fs::remove_dir_all(&cat.dir);
    let _ = std::fs::remove_dir_all(&sto.dir);
}

// --- Widening successors (ADR-0059 amendment 5; THREATS BULK-WIDEN-1..6) ---------------------------

/// A store that takes format-2 manifests (`ONYX_BULK_FORMAT2=on`).
pub(crate) fn world_format2() -> World {
    world_with(BulkLimits {
        format2: true,
        ..BulkLimits::default()
    })
}

/// [`schema`] widened: `v` from 2 to 4 places, and a nullable text column
/// `note` added after it.
pub(crate) fn wide_schema() -> Schema {
    Schema::from_json(&json!({"columns":[
        {"name":"t","kind":"int"},
        {"name":"v","kind":"decimal","scale":4},
        {"name":"note","kind":"text","nullable":true}
    ],"key":"t"}))
    .expect("ok")
}

/// Shard files under [`wide_schema`] of `per` rows each over `keys`.
pub(crate) fn wide_files(keys: std::ops::Range<i128>, per: usize) -> Vec<Vec<u8>> {
    let rows: Vec<Vec<u8>> = keys
        .map(|t| {
            let note = if t % 2 == 0 {
                Cell::Text(format!("n{t}"))
            } else {
                Cell::Null
            };
            encode_row(
                &wide_schema(),
                &[Cell::Int(t), Cell::Decimal(format!("{t}.1250")), note],
            )
            .expect("ok")
        })
        .collect();
    rows.chunks(per).map(encode_shard).collect()
}

/// The successor of `prev` under `schema` adding `files` (built under it).
pub(crate) fn manifest_under(
    schema: &Schema,
    files: &[Vec<u8>],
    prev: &Manifest,
    created: u64,
) -> Manifest {
    let mut entries = prev.shards.clone();
    let base = entries.len();
    for (i, f) in files.iter().enumerate() {
        let c = check_shard(schema, f).expect("fits its schema");
        entries.push(ShardEntry::new((base + i) as u32, &c.summary));
    }
    Manifest::build(
        NewVersion {
            dataset: "synthetic.metrics".into(),
            publisher: SourceId("example.publisher".into()),
            created_unix: created,
            schema: schema.clone(),
            rights: rights(),
        },
        Some(prev),
        entries,
    )
    .expect("a successor")
}

/// Post `m` with its own anchored report and upload `files` (its new shards).
pub(crate) async fn post_and_upload(w: &World, m: &Manifest, files: &[Vec<u8>]) {
    let r = sign(m);
    w.node.anchor(&r);
    assert_eq!(w.post(m, &r).await.expect("taken").0, StatusCode::CREATED);
    let new = &m.shards[m.shards.len() - files.len()..];
    for (f, e) in files.iter().zip(new) {
        assert_eq!(
            w.put(&e.root, f.clone()).await.expect("stored").0,
            StatusCode::CREATED
        );
    }
}

/// Version 2 of [`published`]: `v` widened to 4 places and `note` added,
/// one new shard (keys 30..40).
pub(crate) async fn widened(w: &World, v1: &Manifest) -> (Manifest, Vec<Vec<u8>>) {
    let files = wide_files(30..40, 10);
    let v2 = manifest_under(&wide_schema(), &files, v1, T0 + 60);
    assert_eq!(v2.header.format, onyx_bulk::manifest::FORMAT_2);
    post_and_upload(w, &v2, &files).await;
    (v2, files)
}

/// ADR-0059 amendment 5 item 4: after a widening, old shards are loaded and
/// checked under their own schema, so the loads made before it (keyed by
/// root and schema hash) are used as they are; rows of both schemas check
/// and show under the newest.
#[tokio::test]
async fn widen_old_shards_load_under_their_own_schema_without_reload() {
    let w = world_format2();
    let (v1, _) = published(&w).await;
    let a = w.rows("from=0&to=29").await.expect("v1 rows");
    onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(29)).expect("checks");
    let before = w.store.loaded.lock().expect("lock").clone();
    assert_eq!(before.len(), 3);
    let old_hash = keccak256(&borsh::to_vec(&schema()).expect("ok"));
    assert!(before.iter().all(|(k, _)| k.1 == old_hash));
    let (v2, _) = widened(&w, &v1).await;
    let a = w.rows("from=0&to=39").await.expect("v2 rows");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(39)).expect("checks");
    assert_eq!(c.header.root(), v2.header.root());
    assert_eq!(c.rows.len(), 40);
    // Old rows shown padded, with the added column null; new rows as stored.
    assert_eq!(
        a["rows"][3]["values"],
        json!({"t":"3","v":"3.5000","note":null})
    );
    assert_eq!(a["rows"][3]["schema"], json!(0));
    assert_eq!(
        a["rows"][30]["values"],
        json!({"t":"30","v":"30.1250","note":"n30"})
    );
    assert_eq!(a["rows"][30]["schema"], json!(1));
    assert_eq!(a["manifest"]["format"], json!(2));
    assert_eq!(a["manifest"]["schema"], wide_schema().to_json());
    assert_eq!(
        a["manifest"]["schemas"],
        json!([{"until": 3, "schema": schema().to_json()}, {"until": 4, "schema": wide_schema().to_json()}])
    );
    // The three old loads are the very same ones; one new load, under the
    // new schema's hash.
    let after = w.store.loaded.lock().expect("lock").clone();
    assert_eq!(after.len(), 4);
    for (k, l) in &before {
        assert!(
            after.iter().any(|(k2, l2)| k2 == k && Arc::ptr_eq(l, l2)),
            "shard {} was loaded again",
            k.0
        );
    }
    let new_hash = keccak256(&borsh::to_vec(&wide_schema()).expect("ok"));
    assert!(after
        .iter()
        .any(|(k, _)| k.0 == v2.shards[3].root && k.1 == new_hash));
    // The info route names every schema too.
    let info = w
        .store
        .info("example.publisher", "synthetic.metrics")
        .expect("info");
    assert_eq!(info["manifest"]["schemas"], a["manifest"]["schemas"]);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Item 4: every path that stores a shard by root (the publisher's `PUT`, a
/// peer's `put_shard`, the restore after a withdrawn tombstone, amendment 4
/// item 5a) checks an old root under the schema its index was built under.
#[tokio::test]
async fn widen_shard_put_and_restore_check_an_old_root_under_its_schema() {
    let w = world_format2();
    let (v1, files) = published(&w).await;
    let (v2, wide) = widened(&w, &v1).await;
    // Sent again by the publisher and by a peer: each checked under schema 0
    // (a held root answers 200, after the same checks).
    assert_eq!(
        w.put(&v1.shards[0].root, files[0].clone())
            .await
            .expect("put")
            .0,
        StatusCode::OK
    );
    w.store
        .put_shard(
            "example.publisher",
            "synthetic.metrics",
            &v1.shards[1].root,
            files[1].clone(),
        )
        .await
        .expect("peer put");
    w.store
        .put_shard(
            "example.publisher",
            "synthetic.metrics",
            &v2.shards[3].root,
            wide[0].clone(),
        )
        .await
        .expect("peer put of the new shard");
    // Under the newest schema the old file does not check: that is what the
    // node would have refused.
    assert!(check_shard(&wide_schema(), &files[0]).is_err());
    let _ = std::fs::remove_dir_all(&w.dir);

    // The restore: the widened dataset in catalogs, removed by a tombstone,
    // erased, the tombstone's catalog withdrawn and replaced; the publisher
    // then sends the erased shards again (old and new schema).
    let w = {
        let base = world_format2();
        let c = base.clock.clone();
        let store = BulkStore::open(
            pin(),
            base.node.clone(),
            Arc::new(move || c.load(Ordering::Relaxed)),
            &base.dir,
            &base.dir.join("warm"),
            BulkLimits {
                format2: true,
                ..BulkLimits::default()
            },
        )
        .expect("ok")
        .with_free_per_minute(10_000)
        .with_erase(erase_cfg(EraseMode::On));
        World {
            node: base.node,
            store: started(store),
            clock: base.clock,
            dir: base.dir,
        }
    };
    let s = &w.store;
    let files = shard_files(0..30, 10);
    let v1 = manifest(&files, None, T0);
    let wide = wide_files(30..40, 10);
    let v2 = manifest_under(&wide_schema(), &wide, &v1, T0 + 5);
    let other = manifest_of("synthetic.other", &files[..1], T0);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&v1.header), CatalogLeaf::of(&other.header)],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&v1, 0).await.expect("v1");
    w.post_in(&other, 0).await.expect("other");
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![CatalogLeaf::of(&v2.header), CatalogLeaf::of(&other.header)],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    w.post_in(&v2, 1).await.expect("v2 (format 2)");
    let all: Vec<Vec<u8>> = files.iter().chain(&wide).cloned().collect();
    w.upload_all(&v2, &all).await;
    w.rows("from=0&to=39").await.expect("served");
    let c2 = catalog_of(
        Some(&c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&v2.header).tombstone().expect("t"),
            CatalogLeaf::of(&other.header),
        ],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    assert_eq!(
        w.post_catalog(&c2, &r2).await.expect("c2").0,
        StatusCode::CREATED
    );
    let due = T0 + 30 * DAY;
    let pass = |b: u64, ts: u64| {
        w.node.at(b, ts);
        s.erase_pass()
    };
    pass(100, T0).await.expect("pass");
    pass(101, due).await.expect("pass");
    assert_eq!(pass(102, due + 3_600).await.expect("pass").erased, 1);
    let roots: Vec<Hash32> = v2.shards.iter().map(|e| e.root).collect();
    // Shard 0 is the other dataset's too: kept; the rest erased.
    assert!(s.holds_shard(&roots[0]));
    assert!(roots[1..].iter().all(|r| !s.holds_shard(r)));
    w.node.withdraw(&r2);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    w.node.at(103, due + 4_000);
    let c2b = catalog_of(
        Some(&c1),
        T0 + 50,
        vec![CatalogLeaf::of(&v2.header), CatalogLeaf::of(&other.header)],
    );
    let r2b = cat::sign(&SECRET2, &c2b.header).expect("ok");
    w.node.anchor_by(&r2b, &SECRET2);
    assert_eq!(
        w.post_catalog(&c2b, &r2b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    assert_eq!(pass(104, due + 5_000).await.expect("pass").erased, 0);
    assert!(roots.iter().all(|r| !s.erased_root(r)));
    // The restore: shards 1 and 2 under schema 0, shard 3 under schema 1.
    for (i, f) in all.iter().enumerate().skip(1) {
        assert_eq!(
            w.put(&roots[i], f.clone()).await.expect("restored").0,
            StatusCode::CREATED,
            "shard {i}"
        );
    }
    let a = w.rows("from=0&to=39").await.expect("served again");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(39)).expect("checks");
    assert_eq!(c.rows.len(), 40);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Item 3 (THREATS BULK-WIDEN-1): a successor that moves a boundary,
/// relabels an earlier shard's schema, or drops the earlier schemas is not a
/// successor (409), and the node keeps serving the version it holds.
#[tokio::test]
async fn widen_a_relabelling_successor_is_refused_409() {
    let w = world_format2();
    let (v1, _) = published(&w).await;
    let (v2, _) = widened(&w, &v1).await;
    let more = wide_files(40..50, 10);
    let v3 = manifest_under(&wide_schema(), &more, &v2, T0 + 120);
    let mut moved = v3.clone();
    moved.header.earlier[0].until = 2;
    let mut relabelled = v3.clone();
    relabelled.header.earlier[0].schema = Schema::from_json(&json!({"columns":[
        {"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":3}
    ],"key":"t"}))
    .expect("ok");
    let mut dropped = v3.clone();
    dropped.header.earlier.clear();
    dropped.header.format = onyx_bulk::manifest::FORMAT;
    for (name, m) in [
        ("moved", &moved),
        ("relabelled", &relabelled),
        ("dropped", &dropped),
    ] {
        m.validate().expect("valid on its own");
        let r = sign(m);
        w.node.anchor(&r);
        let e = w.post(m, &r).await.expect_err("refused");
        assert_eq!(
            (e.status, e.code),
            (StatusCode::CONFLICT, "bulk_not_successor"),
            "{name}"
        );
    }
    let a = w.rows("from=0&to=39").await.expect("still served");
    assert_eq!(a["manifest"]["version"], json!(2));
    // The honest version 3 is taken.
    post_and_upload(&w, &v3, &more).await;
    let a = w.rows("from=35&to=45").await.expect("v3");
    onyx_bulk::wire::check_rows_answer(&a, Some(35), Some(45)).expect("checks");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Item 8 (review BULK-WIDEN-R1 finding 2): a node refuses a format-2
/// upload unless `ONYX_BULK_FORMAT2` is on, through its own report or a
/// catalog, and keeps serving the earlier version.
#[tokio::test]
async fn widen_a_format_2_upload_is_refused_until_the_flag_is_on() {
    let w = world();
    let (v1, _) = published(&w).await;
    let files = wide_files(30..40, 10);
    let v2 = manifest_under(&wide_schema(), &files, &v1, T0 + 60);
    let r = sign(&v2);
    w.node.anchor(&r);
    let e = w.post(&v2, &r).await.expect_err("refused");
    assert_eq!(
        (e.status, e.code),
        (StatusCode::UNPROCESSABLE_ENTITY, "bulk_format_not_taken")
    );
    // Through a catalog: the catalog (leaves only) is taken, the manifest not.
    let c0 = catalog_of(None, T0 + 100, vec![CatalogLeaf::of(&v2.header)]);
    let rc = sign_catalog(&c0);
    w.node.anchor(&rc);
    w.post_catalog(&c0, &rc)
        .await
        .expect("the catalog is taken");
    let e = w.post_in(&v2, 0).await.expect_err("refused");
    assert_eq!(e.code, "bulk_format_not_taken");
    let a = w.rows("from=0&to=29").await.expect("v1 still served");
    assert_eq!(a["manifest"]["version"], json!(1));
    assert_eq!(a["manifest"]["format"], json!(1));
    assert!(a["rows"][0].get("schema").is_none());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Item 3 and 8: a restart decodes and checks a held format-2 chain again
/// (successor rule included), with `ONYX_BULK_FORMAT2` off too, and serves
/// it; a held version that relabels the one before stops the start.
#[tokio::test]
async fn widen_a_restart_rechecks_a_format_2_chain() {
    let w = world_format2();
    let (v1, _) = published(&w).await;
    let (v2, _) = widened(&w, &v1).await;
    let more = wide_files(40..50, 10);
    let v3 = manifest_under(&wide_schema(), &more, &v2, T0 + 120);
    post_and_upload(&w, &v3, &more).await;
    let open = |limits: BulkLimits| {
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0 + 200),
            &w.dir,
            &w.dir.join("warm"),
            limits,
        )
    };
    let again = Arc::new(open(BulkLimits::default()).expect("reads format 2 with the flag off"));
    let a = again
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=25&to=45")).expect("ok"),
        )
        .await
        .expect("served");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(25), Some(45)).expect("checks");
    assert_eq!((c.header.version, c.rows.len()), (3, 21));
    // The restarted node with the flag off takes no new format-2 version.
    let more4 = wide_files(50..60, 10);
    let v4 = manifest_under(&wide_schema(), &more4, &v3, T0 + 180);
    let r4 = sign(&v4);
    w.node.anchor(&r4);
    let e = again
        .accept_manifest(&v4.to_bytes(), &borsh::to_vec(&r4).expect("ok"))
        .await
        .expect_err("refused");
    assert_eq!(e.code, "bulk_format_not_taken");
    // A held version 3 that moves version 2's boundary (signed: only the
    // successor rule can tell) stops the start.
    let mut bad = v3.clone();
    bad.header.earlier[0].until = 2;
    bad.validate().expect("valid on its own");
    let rb = sign(&bad);
    let anchor = w
        .store
        .datasets
        .read()
        .expect("lock")
        .get(&(
            "example.publisher".to_string(),
            "synthetic.metrics".to_string(),
        ))
        .and_then(|d| d.versions.get(2).map(|h| h.anchor.clone()))
        .expect("held v3");
    let dir = w
        .store
        .dataset_dir("example.publisher", "synthetic.metrics");
    write_version(&dir, &bad, &rb, &anchor, None).expect("written");
    let e = open(BulkLimits::default()).err().expect("the start stops");
    assert!(e.contains("not a successor"), "{e}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Item 8 (reviews BULK-WIDEN-R1 finding 7, BULK-WIDEN-R2 finding 2): the
/// history walk meets an own-report manifest in a header format it does not
/// read. A manifest answer carries its header's own bytes; bytes that are not
/// the root's (a flipped format byte, a header of another root, no header)
/// are the node's fault even under the publisher's own report. The honest
/// case, a header that hashes to the root, is undecided:
/// `bulk_history::tests::widen_an_unknown_format_is_undecided_only_when_its_header_is_the_root`.
#[tokio::test]
async fn widen_the_history_walk_blames_unknown_format_bytes_that_are_not_the_root() {
    let w = world();
    let (m1, _) = published(&w).await;
    let f2 = shard_files(30..40, 10);
    let m2 = manifest(&f2, Some(&m1), T0 + 5);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m2.header), other_leaf("synthetic.other")],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m2, 0).await.expect("v2");
    w.upload_all(&m2, &f2).await;
    let ck = checked_rows(&w).await;
    let root1 = m1.header.root();
    let path = format!("/v1/bulk/manifests/{root1}");
    // The node's own answer names the header, and it hashes to the root.
    let app = peer_routes(w.store.clone());
    let r = app
        .oneshot(
            Request::get(path.as_str())
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("answer");
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&r.into_body().collect().await.expect("body").to_bytes())
        .expect("json");
    let hb = hex_of(&v["header"]).expect("header");
    assert_eq!(hb, m1.header.to_bytes());
    assert_eq!(onyx_bulk::manifest::root_of_header_bytes(&hb), root1);
    // Flipped bytes under the publisher's own report.
    let mut newer = m1.to_bytes();
    newer[0] = 3;
    let hlen = hb.len();
    let real = hb.clone();
    let answer = |header: Option<&[u8]>| {
        let mut a = json!({"root": root1.to_string(), "manifest": hex0x(&newer), "report": hex0x(&borsh::to_vec(&sign(&m1)).expect("ok"))});
        if let Some(h) = header {
            a["header"] = json!(hex0x(h));
        }
        a.to_string().into_bytes()
    };
    for (name, body) in [
        ("no header", answer(None)),
        ("the flipped header", answer(Some(&newer[..hlen]))),
        ("the root's header, other bytes", answer(Some(&real))),
    ] {
        let p = path.clone();
        let get = HookGet {
            inner: RouterGet(peer_routes(w.store.clone())),
            hook: move |u: &str| u.ends_with(&p).then(|| (200, body.clone())),
        };
        let (fault, why) = refused(history_with(&w, &ck, &get).await);
        assert_eq!(fault, "node", "{name}: {why}");
        assert!(why.contains("format 3"), "{name}: {why}");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review BULK-WIDEN-R2 finding 3: with `ONYX_BULK_FORMAT2` off, the gate
/// stops only a format-2 manifest that would be stored (a new version, or one
/// in a withdrawn version's place). A held format-2 manifest posted again is
/// still the idempotent 200, and a second anchored manifest for a held
/// format-2 version still marks the dataset forked, writing only the
/// `forked.txt` note, never that manifest.
#[tokio::test]
async fn widen_with_the_flag_off_a_held_format_2_version_answers_200_and_its_fork_is_recorded() {
    let off = |w: &World| {
        Arc::new(
            BulkStore::open(
                pin(),
                w.node.clone(),
                Arc::new(|| T0 + 200),
                &w.dir,
                &w.dir.join("warm"),
                BulkLimits::default(),
            )
            .expect("opens with the flag off"),
        )
    };
    let post = |s: &Arc<BulkStore>, m: &Manifest, r: &SignedReport| {
        let (m, r) = (m.to_bytes(), borsh::to_vec(r).expect("ok"));
        let s = s.clone();
        async move { s.accept_manifest(&m, &r).await }
    };
    // The idempotent 200, then a fork of version 2.
    let w = world_format2();
    let (v1, _) = published(&w).await;
    let (v2, wide) = widened(&w, &v1).await;
    let s = off(&w);
    let (st, body) = post(&s, &v2, &sign(&v2)).await.expect("held");
    assert_eq!((st, body["missing"].clone()), (StatusCode::OK, json!([])));
    let v3 = manifest_under(&wide_schema(), &wide_files(40..50, 10), &v2, T0 + 120);
    let r3 = sign(&v3);
    w.node.anchor(&r3);
    assert_eq!(
        post(&s, &v3, &r3).await.expect_err("new").code,
        "bulk_format_not_taken"
    );
    let dir = s.dataset_dir("example.publisher", "synthetic.metrics");
    let held_v2 = std::fs::read(dir.join("v2.bin")).expect("v2");
    let fork = manifest_under(&wide_schema(), &wide, &v1, T0 + 61);
    assert_ne!(fork.header.root(), v2.header.root());
    let rf = sign(&fork);
    w.node.anchor(&rf);
    let e = post(&s, &fork, &rf).await.expect_err("forked");
    assert_eq!(e.code, "bulk_dataset_forked");
    assert!(dir.join("forked.txt").is_file());
    assert_eq!(std::fs::read(dir.join("v2.bin")).expect("v2"), held_v2);
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, vec!["forked.txt", "v1.bin", "v2.bin"]);
    let e = s
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=0&to=5")).expect("ok"),
        )
        .await
        .expect_err("not served");
    assert_eq!(e.code, "bulk_dataset_forked");
    let _ = std::fs::remove_dir_all(&w.dir);
    // A withdrawn format-2 version is not replaced by another format-2
    // manifest while the flag is off: the held one stays as it was.
    let w = world_format2();
    let (v1, _) = published(&w).await;
    let (v2, wide) = widened(&w, &v1).await;
    let s = off(&w);
    w.node.withdraw(&sign(&v2));
    let other = manifest_under(&wide_schema(), &wide, &v1, T0 + 61);
    let ro = sign(&other);
    w.node.anchor(&ro);
    assert_eq!(
        post(&s, &other, &ro)
            .await
            .expect_err("would be stored")
            .code,
        "bulk_format_not_taken"
    );
    let dir = s.dataset_dir("example.publisher", "synthetic.metrics");
    let kept = Manifest::from_bytes(
        &<(Vec<u8>, Vec<u8>, Vec<u8>)>::try_from_slice(
            &std::fs::read(dir.join("v2.bin")).expect("v2"),
        )
        .expect("record")
        .0,
    )
    .expect("manifest");
    assert_eq!(kept.header.root(), v2.header.root());
    assert!(!dir.join("withdrawn").exists());
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Successor links (ADR-0059 amendment 6, slice 2; THREATS CAT-SUCC-*) ---

const LINKS: &str = "onyx.successors";

/// Field `k` of an error answer.
fn field_of(e: &ApiError, k: &str) -> Value {
    e.fields
        .iter()
        .find(|(n, _)| *n == k)
        .map_or(Value::Null, |(_, v)| v.clone())
}

/// A links row `(n, from, from_version, from_prev, to)`.
fn link_row(n: i128, from: &str, v: u32, prev: &Hash32, to: Option<&str>) -> Vec<u8> {
    let cells = vec![
        Cell::Int(n),
        Cell::Text(from.into()),
        Cell::Int(i128::from(v)),
        Cell::Text(prev.to_string()),
        to.map_or(Cell::Null, |t| Cell::Text(t.into())),
    ];
    encode_row(&onyx_bulk::successor::links_schema(), &cells).expect("row")
}

/// The next links version after `prev` (version 1 without), one new shard
/// of `rows`.
fn links_version(rows: &[Vec<u8>], prev: Option<&Manifest>, created: u64) -> (Manifest, Vec<u8>) {
    let (m, mut files) = links_version_of(&[rows.to_vec()], prev, created);
    (m, files.remove(0))
}

/// The next links version after `prev`, one new shard per element of
/// `shards`; the new shard files.
fn links_version_of(
    shards: &[Vec<Vec<u8>>],
    prev: Option<&Manifest>,
    created: u64,
) -> (Manifest, Vec<Vec<u8>>) {
    let s = onyx_bulk::successor::links_schema();
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    let mut files = Vec::new();
    for rows in shards {
        let file = encode_shard(rows);
        let c = check_shard(&s, &file).expect("shard");
        entries.push(ShardEntry::new(entries.len() as u32, &c.summary));
        files.push(file);
    }
    let m = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: publisher(),
            created_unix: created,
            schema: s,
            rights: rights(),
        },
        prev,
        entries,
    )
    .expect("links manifest");
    (m, files)
}

impl World {
    async fn put_in(&self, dataset: &str, root: &Hash32, file: Vec<u8>) -> StatusCode {
        self.store
            .accept_shard(
                "example.publisher",
                dataset,
                root,
                &HeaderMap::new(),
                Body::from(file),
            )
            .await
            .expect("shard")
            .0
    }

    /// Post and anchor catalog `c`.
    async fn take(&self, c: &Catalog) {
        let r = sign_catalog(c);
        self.node.anchor(&r);
        self.post_catalog(c, &r).await.expect("catalog taken");
    }

    /// Wait until the publisher's links index is built for its newest
    /// catalog (a read never waits for it: it answers `pending`).
    async fn settle(&self) {
        self.store.succ_settle("example.publisher").await;
    }

    /// Wait (up to a cap far above any run) until the publisher's fork mark
    /// is cleared by its background read.
    async fn fork_cleared(&self) {
        let wait = async {
            while self.store.fork_marked("example.publisher").is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(60), wait)
            .await
            .expect("the fork mark clears");
    }

    /// A held shard that can no longer be read (as after a loss on disk):
    /// an index that reads it again stays pending.
    fn forget(&self, root: &Hash32) {
        self.store.loaded.lock().expect("lock").clear();
        assert!(self.store.tiers.forget_bulk(root));
    }

    async fn successor_of(&self, dataset: &str) -> Value {
        self.settle().await;
        let e = self
            .store
            .rows(
                "example.publisher",
                dataset,
                &parse_rows_query(Some("")).expect("q"),
            )
            .await
            .expect_err("removed");
        assert_eq!(e.code, "bulk_dataset_removed");
        field_of(&e, "successor")
    }
}

/// A move: `synthetic.metrics` (the old id, held) to `synthetic.new` (held),
/// in catalog 1, with links version 1 of one row. The links version is
/// posted but its shard is not uploaded yet.
struct Move {
    w: World,
    old: Manifest,
    new: Manifest,
    links: Manifest,
    links_file: Vec<u8>,
    c0: Catalog,
    c1: Catalog,
}

async fn a_move() -> Move {
    let w = world();
    let files = shard_files(0..30, 10);
    let old = manifest(&files, None, T0);
    let new_files = shard_files(0..40, 10);
    let new = manifest_of("synthetic.new", &new_files, T0 + 1);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&old.header), CatalogLeaf::of(&new.header)],
    );
    w.take(&c0).await;
    w.post_in(&old, 0).await.expect("old");
    w.upload_all(&old, &files).await;
    w.post_in(&new, 0).await.expect("new");
    for (f, e) in new_files.iter().zip(&new.shards) {
        assert!(w
            .put_in("synthetic.new", &e.root, f.clone())
            .await
            .is_success());
    }
    let tomb = CatalogLeaf::of(&old.header).tombstone().expect("tombstone");
    let (links, links_file) = links_version(
        &[link_row(
            1,
            "synthetic.metrics",
            2,
            &old.header.root(),
            Some("synthetic.new"),
        )],
        None,
        T0 + 15,
    );
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            tomb,
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    w.take(&c1).await;
    Move {
        w,
        old,
        new,
        links,
        links_file,
        c0,
        c1,
    }
}

#[tokio::test]
async fn cat_succ_a_link_is_pending_until_its_links_version_is_held() {
    let m = a_move().await;
    // The catalog is taken, the links version is not here yet.
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "pending"
    );
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "pending"
    );
    let root = m.links.shards[0].root;
    assert_eq!(
        m.w.put_in(LINKS, &root, m.links_file.clone()).await,
        StatusCode::CREATED
    );
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(s["status"], "live");
    assert_eq!(s["dataset"], "synthetic.new");
}

#[tokio::test]
async fn cat_succ_the_410_info_and_listing_show_the_link() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        s,
        json!({"dataset": "synthetic.new", "final": "synthetic.new", "status": "live", "hops": 1,
               "row": "1", "generation": 1,
               "proof": "/v1/bulk/example.publisher/synthetic.metrics/successor"})
    );
    // Info of the old id is the same 410; the new id names what it replaces.
    let e =
        m.w.store
            .info_checked("example.publisher", "synthetic.metrics")
            .await
            .expect_err("removed");
    assert_eq!(field_of(&e, "successor")["dataset"], "synthetic.new");
    let info =
        m.w.store
            .info_checked("example.publisher", "synthetic.new")
            .await
            .expect("live");
    assert_eq!(
        info["replaces"],
        json!({"count": 1, "first": ["synthetic.metrics"]})
    );
    // The listing: a count per entry, `moved` per catalog, the links role.
    let r = list(State(m.w.store.clone()), None, RawQuery(None)).await;
    let body = r.into_body().collect().await.expect("body").to_bytes();
    let v: Value = serde_json::from_slice(&body).expect("json");
    let entry = |d: &str| {
        v["datasets"]
            .as_array()
            .expect("list")
            .iter()
            .find(|e| e["dataset"] == d)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(entry("synthetic.new")["replaces"], 1);
    assert_eq!(entry(LINKS)["role"], "successors");
    assert_eq!(entry(LINKS)["generation"], 1);
    assert_eq!(
        entry("synthetic.metrics"),
        Value::Null,
        "removed: not listed"
    );
    assert_eq!(v["catalogs"][0]["moved"], 1);
    let _ = (&m.old, &m.new, &m.c0, &m.c1);
}

#[tokio::test]
async fn cat_succ_the_successor_answer_proves_the_chain() {
    use onyx_bulk::successor::{follow, read_links_pages, End, Listing};
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    // Everything a reader needs, checked here with the onyx-bulk rules.
    let hex = |v: &Value| onyx_bulk::wire::hex_field(v, "field").expect("hex");
    let h = CatalogHeader::try_from_slice(&hex(&a["catalog"]["header"])).expect("header");
    let report = SignedReport::try_from_slice(&hex(&a["catalog"]["report"])).expect("report");
    assert_eq!(h, m.c1.header);
    cat::verify(&h, &report).expect("signed");
    let mut leaves = BTreeMap::new();
    for p in a["chain"].as_array().expect("chain") {
        let lp = LeafProof::try_from_slice(&hex(&p["leaf"])).expect("leaf");
        h.proves(&lp).expect("proven");
        leaves.insert(lp.leaf.dataset.clone(), lp.leaf);
    }
    let g = &a["generations"][0];
    let glp = LeafProof::try_from_slice(&hex(&g["leaf"])).expect("links leaf");
    h.proves(&glp).expect("proven");
    let (_, links) = read_links_pages(&glp.leaf.root, std::slice::from_ref(&a["links"]))
        .expect("every links row");
    let chain = follow("synthetic.metrics", Some(&links), |id| {
        Ok::<_, ()>(
            leaves
                .get(id)
                .cloned()
                .map_or(Listing::Absent, Listing::Leaf),
        )
    })
    .expect("walk");
    assert_eq!(
        chain.end,
        End::Live {
            dataset: "synthetic.new".into(),
            version: 1
        }
    );
    assert_eq!(a["end"]["status"], "live");
    // A live dataset has no successor; an unknown one is not here.
    let live =
        m.w.store
            .successor_answer("example.publisher", "synthetic.new")
            .await
            .expect_err("live");
    assert_eq!(
        (live.status, live.code),
        (StatusCode::CONFLICT, "bulk_dataset_live")
    );
    let none =
        m.w.store
            .successor_answer("example.publisher", "synthetic.none")
            .await
            .expect_err("unknown");
    assert_eq!(none.code, "bulk_unknown_dataset");
    // Through the route.
    let app = routes(m.w.store.clone());
    let r = app
        .oneshot(
            Request::get("/v1/bulk/example.publisher/synthetic.metrics/successor")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("ok");
    assert_eq!(r.status(), StatusCode::OK);
}

/// Review SUCC-R2 L1 and SUCC-B2 L2: a fork of the links dataset found
/// after the index was built, and a publisher fork mark, each make the 410
/// name no successor; once the book withdraws one side of the publisher
/// fork, the 410's own read of the chain clears the mark. Nothing of the
/// index is cleared by hand.
#[tokio::test]
async fn cat_succ_under_a_fork_mark_or_a_forked_links_dataset_the_410_names_no_successor() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    let set_forked = |why: Option<&str>| {
        m.w.store
            .datasets
            .write()
            .expect("lock")
            .get_mut(&("example.publisher".into(), LINKS.into()))
            .expect("held")
            .forked = why.map(str::to_string);
    };
    // The links dataset forks on this node (as `accept_held` marks it).
    set_forked(Some("two manifests for version 1"));
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await,
        json!({"status": "forked"})
    );
    let info =
        m.w.store
            .info_checked("example.publisher", "synthetic.new")
            .await
            .expect("live");
    assert!(info.get("replaces").is_none(), "no link under a fork");
    set_forked(None);
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    // A second anchored catalog 1: a publisher fork mark, read before the
    // removal; the successor route refuses.
    let c1b = catalog_of(
        Some(&m.c0),
        T0 + 21,
        vec![
            CatalogLeaf::of(&m.old.header),
            CatalogLeaf::of(&m.new.header),
        ],
    );
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    m.w.node.anchor_by(&r1b, &SECRET2);
    assert_eq!(
        code(m.w.post_catalog(&c1b, &r1b).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    let e =
        m.w.store
            .info_checked("example.publisher", "synthetic.metrics")
            .await
            .expect_err("not served");
    assert_eq!(e.code, "bulk_dataset_removed");
    assert_eq!(field_of(&e, "successor"), json!({"status": "forked"}));
    assert_eq!(
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect_err("forked")
            .code,
        "bulk_publisher_forked"
    );
    // The book withdraws the other side. After the recheck window the 410
    // still answers at once, `forked`, with no chain read of its own
    // (review SUCC-B3 L2); the mark is read again in the background, and
    // once it clears the 410 names the successor.
    m.w.node.withdraw(&r1b);
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let before = m.w.node.reads();
    let e =
        m.w.store
            .rows(
                "example.publisher",
                "synthetic.metrics",
                &parse_rows_query(Some("")).expect("q"),
            )
            .await
            .expect_err("removed");
    assert_eq!(field_of(&e, "successor"), json!({"status": "forked"}));
    assert_eq!(m.w.node.reads(), before, "the 410 reads no chain");
    m.w.fork_cleared().await;
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone()),
        (json!("live"), json!("synthetic.new"))
    );
    assert!(m.w.store.fork_marked("example.publisher").is_none());
}

/// Review SUCC-R2 L2 and SUCC-B4: a links dataset forked on this node while
/// its index waits for a shard makes the 410 say `forked`, not `pending`.
#[tokio::test]
async fn cat_succ_a_pending_index_of_a_forked_links_dataset_is_forked() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.settle().await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await,
        json!({"status": "pending"})
    );
    m.w.store
        .datasets
        .write()
        .expect("lock")
        .get_mut(&("example.publisher".into(), LINKS.into()))
        .expect("held")
        .forked = Some("two manifests for version 1".into());
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await,
        json!({"status": "forked"})
    );
}

/// THREATS CAT-SUCC-2 and CAT-SUCC-6 (reviews SUCC-R2 M4, SUCC-B2 L5,
/// SUCC-B4): a later row fixes a wrong link with no key at stake. Each
/// catalog reads its links version whole once: a read between the catalog
/// and the links upload is pending and reads nothing, and a start builds
/// the index before any read.
#[tokio::test]
async fn cat_succ_a_later_row_fixes_a_wrong_link_and_a_start_builds_the_index() {
    let m = a_move().await;
    // Version 1 names a typo.
    let (bad, bad_file) = links_version(&[move_row(1, &m.old, "synthetic.typo")], None, T0 + 15);
    let w = world();
    let _ = (&m.links, &m.links_file);
    // A fresh world with the typo version in catalog 1.
    let files = shard_files(0..30, 10);
    w.take(&m.c0).await;
    w.post_in(&m.old, 0).await.expect("old");
    w.upload_all(&m.old, &files).await;
    w.post_in(&m.new, 0).await.expect("new");
    let tomb = CatalogLeaf::of(&m.old.header).tombstone().expect("t");
    let c1 = catalog_of(
        Some(&m.c0),
        T0 + 20,
        vec![
            tomb.clone(),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&bad.header),
        ],
    );
    w.take(&c1).await;
    w.post_in(&bad, 1).await.expect("links v1");
    w.put_in(LINKS, &bad.shards[0].root, bad_file).await;
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone()),
        (json!("not_listed"), json!("synthetic.typo"))
    );
    assert_eq!(w.store.succ_reads(), 1);
    // Version 2 adds the right row; no key is at stake (review SUCC-R1 H1).
    let (good, good_file) =
        links_version(&[move_row(2, &m.old, "synthetic.new")], Some(&bad), T0 + 25);
    let c2 = catalog_of(
        Some(&c1),
        T0 + 30,
        vec![
            tomb.clone(),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&good.header),
        ],
    );
    w.take(&c2).await;
    // Read between the catalog and the links upload: pending, nothing read.
    assert_eq!(
        w.successor_of("synthetic.metrics").await["status"],
        "pending"
    );
    assert_eq!(w.store.succ_reads(), 1);
    w.post_in(&good, 2).await.expect("links v2");
    w.put_in(LINKS, &good.shards[1].root, good_file).await;
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone(), s["row"].clone()),
        (json!("live"), json!("synthetic.new"), json!("2"))
    );
    assert_eq!(w.store.succ_reads(), 3, "version 2 read whole, once");
    // The next catalog lists the same links version: read whole once more.
    let c3 = catalog_of(
        Some(&c2),
        T0 + 40,
        vec![
            tomb,
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&good.header),
            other_leaf("synthetic.extra"),
        ],
    );
    w.take(&c3).await;
    assert_eq!(w.successor_of("synthetic.metrics").await["row"], "2");
    assert_eq!(
        w.store.succ_reads(),
        5,
        "the same version, read for catalog 3"
    );
    // A start builds every publisher's index before any read.
    let again = started(
        BulkStore::open(
            pin(),
            w.node.clone(),
            Arc::new(|| T0),
            &w.dir,
            &w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("reopen"),
    );
    again.succ_settle("example.publisher").await;
    assert!(again.succ_built("example.publisher"), "built at start");
    assert_eq!(again.succ_reads(), 2);
    let e = again
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("")).expect("q"),
        )
        .await
        .expect_err("removed");
    assert_eq!(field_of(&e, "successor")["row"], "2");
    assert_eq!(again.succ_reads(), 2, "the read built nothing");
    // Queued again with nothing new: the job finds it built and reads none.
    again.succ_start();
    again.succ_settle("example.publisher").await;
    assert_eq!(again.succ_reads(), 2, "built already: nothing read");
    let keys = w.node.0.lock().expect("lock").keys.clone();
    assert!(keys.values().all(|k| !k.1), "no key suspended");
}

#[tokio::test]
async fn cat_succ_rows_at_a_root_serve_that_version() {
    let w = world();
    let (v1, _) = published(&w).await;
    let files2 = shard_files(30..40, 10);
    let v2 = manifest(&files2, Some(&v1), T0 + 5);
    let r2 = sign(&v2);
    w.node.anchor(&r2);
    w.post(&v2, &r2).await.expect("v2");
    w.upload_all(&v2, &[vec![], vec![], vec![], files2[0].clone()])
        .await;
    let newest = w.rows("from=0&to=3").await.expect("rows");
    assert_eq!(newest["manifest"]["root"], v2.header.root().to_string());
    let at = w
        .rows(&format!("from=0&to=3&root={}", v1.header.root()))
        .await
        .expect("rows at v1");
    assert_eq!(at["manifest"]["root"], v1.header.root().to_string());
    assert_eq!(at["query"]["root"], v1.header.root().to_string());
    let e = w
        .rows(&format!("root={}", Hash32([9; 32])))
        .await
        .expect_err("not held");
    assert_eq!(
        (e.status, e.code),
        (StatusCode::CONFLICT, "bulk_version_not_held")
    );
    assert_eq!(
        code(w.rows("root=0x12").await.expect_err("bad")),
        "bad_request"
    );
}

#[tokio::test]
async fn cat_succ_an_own_report_links_version_is_refused() {
    let w = world();
    let (links, _) = links_version(
        &[link_row(
            1,
            "synthetic.metrics",
            2,
            &Hash32([1; 32]),
            Some("synthetic.new"),
        )],
        None,
        T0,
    );
    let r = sign(&links);
    w.node.anchor(&r);
    let e = w.post(&links, &r).await.expect_err("refused");
    assert_eq!(
        (e.status, e.code),
        (StatusCode::UNPROCESSABLE_ENTITY, "bulk_reserved_dataset")
    );
}

/// A tombstone of `old` (version 1) and its row `n` to `to`.
fn move_row(n: i128, old: &Manifest, to: &str) -> Vec<u8> {
    link_row(n, &old.header.dataset, 2, &old.header.root(), Some(to))
}

/// Review SUCC-B2 H1 (1) and SUCC-B4: a links version whose last shard is
/// withheld leaves the index pending and reads nothing, however often it
/// is read or a held shard is sent again; the shard that comes reads the
/// version whole, once.
#[tokio::test]
async fn cat_succ_a_withheld_shard_leaves_the_index_pending_until_it_comes() {
    let m = a_move().await;
    // Version 2: seven one-row shards of inert rows, then the move's row.
    let mut shards: Vec<Vec<Vec<u8>>> = (0..7)
        .map(|i| {
            vec![link_row(
                i + 2,
                &format!("synthetic.gone{i}"),
                2,
                &Hash32([7; 32]),
                Some("synthetic.new"),
            )]
        })
        .collect();
    shards.push(vec![move_row(9, &m.old, "synthetic.new")]);
    let (v2, files) = links_version_of(&shards, Some(&m.links), T0 + 25);
    let tomb = CatalogLeaf::of(&m.old.header).tombstone().expect("t");
    let c2 = catalog_of(
        Some(&m.c1),
        T0 + 30,
        vec![
            tomb,
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&v2.header),
        ],
    );
    m.w.take(&c2).await;
    m.w.post_in(&m.links, 1).await.expect("links v1");
    m.w.post_in(&v2, 2).await.expect("links v2");
    m.w.put_in(LINKS, &v2.shards[0].root, m.links_file.clone())
        .await;
    for (f, e) in files.iter().zip(&v2.shards[1..8]) {
        m.w.put_in(LINKS, &e.root, f.clone()).await;
    }
    for _ in 0..3 {
        assert_eq!(
            m.w.successor_of("synthetic.metrics").await["status"],
            "pending"
        );
    }
    assert_eq!(
        m.w.store.succ_reads(),
        0,
        "nothing read while one is missing"
    );
    let jobs = m.w.store.succ_jobs().len();
    m.w.put_in(LINKS, &v2.shards[1].root, files[0].clone())
        .await;
    m.w.settle().await;
    assert_eq!(m.w.store.succ_jobs().len(), jobs, "a held shard: no job");
    m.w.put_in(LINKS, &v2.shards[8].root, files[7].clone())
        .await;
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["row"].clone()),
        (json!("live"), json!("9"))
    );
    assert_eq!(m.w.store.succ_reads(), 9, "the version read whole, once");
}

/// Review SUCC-B2 L4 (a, b): the successor route answers a dataset live in
/// the newest catalog with 409 `bulk_dataset_live` even when this node does
/// not hold it, and a storage node's learned removal with its 410.
#[tokio::test]
async fn cat_succ_the_route_answers_live_for_a_listed_dataset_and_410_for_a_learned_removal() {
    let m = a_move().await;
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    let c2 = catalog_of(Some(&m.c1), T0 + 30, leaves);
    m.w.take(&c2).await;
    let e =
        m.w.store
            .successor_answer("example.publisher", "synthetic.extra")
            .await
            .expect_err("live");
    assert_eq!(
        (e.status, e.code),
        (StatusCode::CONFLICT, "bulk_dataset_live")
    );
    let l = learning().await;
    let (_, _, body) = leaf_get(&l.cat, LP, LD).await;
    assert_eq!(l.sto.learn(&body).await, Learn::Kept(0));
    let e = l
        .sto
        .store
        .successor_answer(LP, LD)
        .await
        .expect_err("removed");
    assert_eq!(
        (e.status, e.code),
        (StatusCode::GONE, "bulk_dataset_removed")
    );
    assert!(e.fields.iter().any(|(k, _)| *k == "learned"));
    l.clean();
}

/// Review SUCC-B2 L4 (c, d): the listing shows `moved` and `replaces` only
/// from a settled index: null while the links are pending, and nothing
/// under a publisher fork mark.
#[tokio::test]
async fn cat_succ_the_listing_shows_no_count_while_pending_or_forked() {
    let m = a_move().await;
    let listing = |s: Arc<BulkStore>| async move {
        let r = list(State(s), None, RawQuery(None)).await;
        let body = r.into_body().collect().await.expect("body").to_bytes();
        serde_json::from_slice::<Value>(&body).expect("json")
    };
    m.w.settle().await;
    let v = listing(m.w.store.clone()).await;
    assert_eq!(v["catalogs"][0]["moved"], Value::Null, "pending");
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let v = listing(m.w.store.clone()).await;
    assert_eq!(v["catalogs"][0]["moved"], 1);
    // A second anchored catalog 1: a fork mark.
    let c1b = catalog_of(
        Some(&m.c0),
        T0 + 21,
        vec![
            CatalogLeaf::of(&m.old.header),
            CatalogLeaf::of(&m.new.header),
        ],
    );
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    m.w.node.anchor_by(&r1b, &SECRET2);
    assert_eq!(
        code(m.w.post_catalog(&c1b, &r1b).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    let v = listing(m.w.store.clone()).await;
    assert_eq!(v["catalogs"][0]["moved"], Value::Null, "forked");
    assert!(v["datasets"]
        .as_array()
        .expect("list")
        .iter()
        .all(|e| e.get("replaces").is_none()));
}

/// Review SUCC-B2 M2 and H1 (2, 3), SUCC-B4 M1: no read waits for a links
/// index build. While the worker is stuck before another publisher's job
/// (as on a hung disk, accepted), the listing, the 410 and info answer from
/// the index built before.
#[tokio::test]
async fn cat_succ_the_410_answers_while_another_publishers_build_waits() {
    let cap = || std::time::Duration::from_secs(60);
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    // The worker is stuck (as on a hung disk) before the next job.
    m.w.store.succ_hold();
    let other = SourceId("example.second".into());
    let leaf = CatalogLeaf {
        publisher: other.clone(),
        dataset: "synthetic.metrics".into(),
        version: 1,
        prev: Hash32::ZERO,
        root: Hash32([5; 32]),
    };
    let c = Catalog::build(other, None, T0 + 10, vec![leaf]).expect("catalog");
    m.w.take(&c).await;
    let r = tokio::time::timeout(cap(), list(State(m.w.store.clone()), None, RawQuery(None)))
        .await
        .expect("the listing answers while a build is held");
    assert_eq!(r.status(), StatusCode::OK);
    let s = tokio::time::timeout(cap(), m.w.successor_of("synthetic.metrics"))
        .await
        .expect("the 410 answers while another publisher's build is held");
    assert_eq!(s["status"], "live");
    let info = tokio::time::timeout(
        cap(),
        m.w.store.info_checked("example.publisher", "synthetic.new"),
    )
    .await
    .expect("info answers")
    .expect("live");
    assert_eq!(info["replaces"]["count"], 1);
    assert_eq!(m.w.store.succ_queued(), vec!["example.second".to_string()]);
    m.w.store.succ_release();
    m.w.store.succ_settle("example.second").await;
    assert!(m.w.store.succ_built("example.second"));
}

/// Review SUCC-B2 L1 and M2: the successor answer takes the catalog's
/// number, header and report in one read, and uses the index only when it
/// was built for that catalog: while the build for a new catalog waits, the
/// answer comes at once with the new catalog's header and report and the
/// end `pending`, never the older catalog's chain.
#[tokio::test]
async fn cat_succ_the_answer_names_one_catalog_and_waits_for_no_build() {
    let cap = || std::time::Duration::from_secs(60);
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let hex = |v: &Value| onyx_bulk::wire::hex_field(v, "field").expect("hex");
    let one_catalog = |a: &Value, want: &Catalog| {
        let h = CatalogHeader::try_from_slice(&hex(&a["catalog"]["header"])).expect("header");
        let r = SignedReport::try_from_slice(&hex(&a["catalog"]["report"])).expect("report");
        assert_eq!(h, want.header);
        assert_eq!(a["catalog"]["number"], want.header.number);
        cat::verify(&h, &r).expect("the report signs this header");
    };
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    one_catalog(&a, &m.c1);
    assert_eq!(a["end"]["status"], "live");
    m.w.store.succ_hold();
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    let c2 = catalog_of(Some(&m.c1), T0 + 30, leaves);
    m.w.take(&c2).await;
    let a = tokio::time::timeout(
        cap(),
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics"),
    )
    .await
    .expect("answered while the build waits")
    .expect("answer");
    one_catalog(&a, &c2);
    assert_eq!(a["end"], json!({"status": "pending"}));
    assert_eq!(a["chain"], json!([]));
    assert_eq!(a["links"], Value::Null);
    m.w.store.succ_release();
    m.w.settle().await;
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    one_catalog(&a, &c2);
    assert_eq!(a["end"]["status"], "live");
}

/// The thief's catalog 1 (`SECRET`): it removes `synthetic.metrics` and
/// links it to `to`; its links version is uploaded.
async fn a_thiefs_move(
    to: &str,
) -> (
    World,
    Manifest,
    Manifest,
    Catalog,
    Catalog,
    SignedReport,
    Manifest,
) {
    let w = world();
    let files = shard_files(0..30, 10);
    let old = manifest(&files, None, T0);
    let new_files = shard_files(0..40, 10);
    let new = manifest_of("synthetic.new", &new_files, T0 + 1);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&old.header), CatalogLeaf::of(&new.header)],
    );
    w.take(&c0).await;
    w.post_in(&old, 0).await.expect("old");
    w.upload_all(&old, &files).await;
    w.post_in(&new, 0).await.expect("new");
    let (links, links_file) = links_version(&[move_row(1, &old, to)], None, T0 + 15);
    let mut leaves = vec![
        CatalogLeaf::of(&old.header).tombstone().expect("t"),
        CatalogLeaf::of(&new.header),
        CatalogLeaf::of(&links.header),
    ];
    if to != "synthetic.new" {
        leaves.push(other_leaf(to));
    }
    let c1 = catalog_of(Some(&c0), T0 + 20, leaves);
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    w.post_catalog(&c1, &r1).await.expect("c1");
    w.post_in(&links, 1).await.expect("links");
    w.put_in(LINKS, &links.shards[0].root, links_file).await;
    (w, old, new, c0, c1, r1, links)
}

impl World {
    /// The publisher's replacement of catalog 1 under `SECRET2`, taken once
    /// the book withdrew `r1` (THREATS CAT-5).
    async fn replace_1(
        &self,
        c0: &Catalog,
        r1: &SignedReport,
        leaves: Vec<CatalogLeaf>,
    ) -> Catalog {
        let c1b = catalog_of(Some(c0), T0 + 50, leaves);
        let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
        self.node.anchor_by(&r1b, &SECRET2);
        self.node.withdraw(r1);
        self.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
        assert_eq!(
            self.post_catalog(&c1b, &r1b).await.expect("replaced").0,
            StatusCode::CREATED
        );
        c1b
    }

    async fn listed(&self) -> Value {
        self.settle().await;
        let r = list(State(self.store.clone()), None, RawQuery(None)).await;
        let body = r.into_body().collect().await.expect("body").to_bytes();
        serde_json::from_slice(&body).expect("json")
    }
}

/// THREATS CAT-SUCC-1 and CAT-SUCC-8 (review SUCC-B2 L5): a thief's
/// catalog that removes a dataset and links it to the thief's id is set
/// aside with its links version; the publisher's replacement lists its own
/// links version and the index follows it.
#[tokio::test]
async fn cat_succ_a_set_aside_catalog_takes_its_rows_with_it() {
    let (w, old, new, c0, _c1, r1, thiefs) = a_thiefs_move("synthetic.thief").await;
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(s["dataset"], "synthetic.thief");
    let (own, own_file) = links_version(&[move_row(1, &old, "synthetic.new")], None, T0 + 45);
    let c1b = w
        .replace_1(
            &c0,
            &r1,
            vec![
                CatalogLeaf::of(&old.header).tombstone().expect("t"),
                CatalogLeaf::of(&new.header),
                CatalogLeaf::of(&own.header),
            ],
        )
        .await;
    // The thief's links version went with its catalog.
    let e = w
        .store
        .rows(
            "example.publisher",
            LINKS,
            &parse_rows_query(Some(&format!("root={}", thiefs.header.root()))).expect("q"),
        )
        .await
        .expect_err("set aside");
    assert_eq!(e.code, "bulk_unknown_dataset");
    assert_eq!(
        w.successor_of("synthetic.metrics").await["status"],
        "pending"
    );
    w.post_in(&own, 1).await.expect("own links");
    w.put_in(LINKS, &own.shards[0].root, own_file).await;
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone()),
        (json!("live"), json!("synthetic.new"))
    );
    let v = w.listed().await;
    assert_eq!(v["catalogs"][0]["number"], c1b.header.number);
    assert_eq!(v["catalogs"][0]["moved"], 1);
}

/// THREATS CAT-SUCC-8 (review SUCC-B2 L5): when the tombstone's catalog is
/// set aside, the old id is back and its row is inert (the replacement
/// lists the same links version); the same tombstone again makes it act.
#[tokio::test]
async fn cat_succ_a_set_aside_tombstone_makes_the_link_inert() {
    let (w, old, new, c0, _c1, r1, links) = a_thiefs_move("synthetic.new").await;
    assert_eq!(w.successor_of("synthetic.metrics").await["status"], "live");
    let reads = w.store.succ_reads();
    let c1b = w
        .replace_1(
            &c0,
            &r1,
            vec![
                CatalogLeaf::of(&old.header),
                CatalogLeaf::of(&new.header),
                CatalogLeaf::of(&links.header),
            ],
        )
        .await;
    w.settle().await;
    w.rows("from=0&to=3")
        .await
        .expect("the old id serves again");
    let info = w
        .store
        .info_checked("example.publisher", "synthetic.new")
        .await
        .expect("live");
    assert!(info.get("replaces").is_none(), "the row acts on nothing");
    assert_eq!(w.listed().await["catalogs"][0]["moved"], 0);
    // The same tombstone (version 2 after the same root) again.
    let c2 = catalog_of(
        Some(&c1b),
        T0 + 60,
        vec![
            CatalogLeaf::of(&old.header).tombstone().expect("t"),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    let r2 = cat::sign(&SECRET2, &c2.header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    w.post_catalog(&c2, &r2).await.expect("c2");
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone(), s["row"].clone()),
        (json!("live"), json!("synthetic.new"), json!("1"))
    );
    assert_eq!(
        w.store.succ_reads(),
        reads + 1,
        "the same links version: read for catalog 2 only (catalog 1b removes nothing)"
    );
}

/// THREATS CAT-SUCC-8 (review SUCC-B2 L5): erasing the old id's shards
/// after the grace keeps its link (the links are another dataset).
#[tokio::test]
async fn cat_succ_erasure_keeps_the_link() {
    let e = erase_world(erase_cfg(EraseMode::On)).await;
    let (w, s) = (&e.w, &e.w.store);
    let (links, file) = links_version(&[move_row(1, &e.m, "synthetic.other")], None, T0 + 22);
    let c2 = catalog_of(
        Some(&e.c1),
        T0 + 25,
        vec![
            CatalogLeaf::of(&e.m.header).tombstone().expect("t"),
            CatalogLeaf::of(&e.other.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    w.take(&c2).await;
    w.post_in(&links, 2).await.expect("links");
    w.put_in(LINKS, &links.shards[0].root, file).await;
    let due = T0 + 30 * DAY;
    e.pass(100, T0).await;
    e.pass(102, due).await;
    let p = e.pass(103, due + 3_600).await;
    assert_eq!(p.erased, 1);
    assert!(!s.holds_shard(&e.m.shards[1].root), "erased");
    assert!(s.holds_shard(&links.shards[0].root), "the links stay");
    let gone = w.rows("from=0&to=3").await.expect_err("removed");
    assert_eq!(field_of(&gone, "erase")["phase"], "erased");
    let succ = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (succ["status"].clone(), succ["dataset"].clone()),
        (json!("live"), json!("synthetic.other"))
    );
    let _ = (&e.files, &e.r1);
}

/// THREATS CAT-SUCC-4 (review SUCC-B2 L5): a row written before its
/// tombstone is inert until that exact tombstone (version and previous
/// root); a row for another tombstone of the same id never acts.
#[tokio::test]
async fn cat_succ_a_row_written_before_its_tombstone_acts_only_on_that_tombstone() {
    let m = a_move().await;
    let w = world();
    let files = shard_files(0..30, 10);
    w.take(&m.c0).await;
    w.post_in(&m.old, 0).await.expect("old");
    w.upload_all(&m.old, &files).await;
    w.post_in(&m.new, 0).await.expect("new");
    // Catalog 1: the old id still live, the links already name its future
    // tombstone (row 1) and another one after a root it never had (row 2).
    let (links, file) = links_version(
        &[
            move_row(1, &m.old, "synthetic.new"),
            link_row(
                2,
                "synthetic.metrics",
                2,
                &Hash32([9; 32]),
                Some("synthetic.typo"),
            ),
        ],
        None,
        T0 + 15,
    );
    let c1 = catalog_of(
        Some(&m.c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&m.old.header),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    w.take(&c1).await;
    w.post_in(&links, 1).await.expect("links");
    w.put_in(LINKS, &links.shards[0].root, file).await;
    w.settle().await;
    w.rows("from=0&to=3").await.expect("live: the row waits");
    assert_eq!(w.listed().await["catalogs"][0]["moved"], 0);
    let c2 = catalog_of(
        Some(&c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    w.take(&c2).await;
    let s = w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["dataset"].clone(), s["row"].clone()),
        (json!("live"), json!("synthetic.new"), json!("1"))
    );
    let _ = (&m.links, &m.links_file, &m.c1, &m.w);
}

/// THREATS CAT-SUCC-10 (review SUCC-B2 L5): the links dataset is an
/// ordinary dataset on every read route: info, rows (checked as any rows
/// answer), the listing, its manifest and shard by root, and the successor
/// route answers it live.
#[tokio::test]
async fn cat_succ_the_links_dataset_is_an_ordinary_dataset_on_every_route() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let send = |app: Router, u: String| async move {
        let r = app
            .oneshot(Request::get(u).body(Body::empty()).expect("req"))
            .await
            .expect("ok");
        let status = r.status();
        let body = r.into_body().collect().await.expect("body").to_bytes();
        (status, body)
    };
    let read = routes(m.w.store.clone());
    let peer = peer_routes(m.w.store.clone());
    let base = format!("/v1/bulk/example.publisher/{LINKS}");
    let (s, b) = send(read.clone(), base.clone()).await;
    assert_eq!(s, StatusCode::OK);
    let info: Value = serde_json::from_slice(&b).expect("json");
    assert_eq!(info["manifest"]["root"], m.links.header.root().to_string());
    let (s, b) = send(read.clone(), format!("{base}/rows")).await;
    assert_eq!(s, StatusCode::OK);
    let rows: Value = serde_json::from_slice(&b).expect("json");
    onyx_bulk::wire::check_rows_answer(&rows, None, None).expect("an ordinary rows answer");
    assert_eq!(rows["rows"].as_array().map(Vec::len), Some(1));
    let (s, b) = send(read.clone(), "/v1/bulk".into()).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&b).contains(LINKS));
    let (s, _) = send(
        peer.clone(),
        format!("/v1/bulk/manifests/{}", m.links.header.root()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, b) = send(peer, format!("/v1/bulk/shards/{}", m.links.shards[0].root)).await;
    assert_eq!((s, b.to_vec()), (StatusCode::OK, m.links_file.clone()));
    let (s, b) = send(read, format!("{base}/successor")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(String::from_utf8_lossy(&b).contains("bulk_dataset_live"));
}

// --- Build reviews SUCC-B3 and SUCC-B4 ---

/// `q`'s catalog 0 (its made-up dataset `synthetic.old` live) and catalog
/// 1 (`leaves` and that dataset's tombstone), both taken: catalog 1 removes
/// something, so its links are read (review SUCC-B5 M2). Catalog 1.
async fn a_removing_catalog(w: &World, q: &str, leaves: Vec<CatalogLeaf>) -> Catalog {
    let old = CatalogLeaf {
        publisher: SourceId(q.into()),
        dataset: "synthetic.old".into(),
        version: 1,
        prev: Hash32::ZERO,
        root: Hash32([3; 32]),
    };
    let c0 = Catalog::build(SourceId(q.into()), None, T0 + 8, vec![old.clone()]).expect("c0");
    w.take(&c0).await;
    let mut leaves = leaves;
    leaves.push(old.tombstone().expect("live"));
    let c1 = Catalog::build(SourceId(q.into()), Some(&c0), T0 + 10, leaves).expect("c1");
    w.take(&c1).await;
    c1
}

/// Publisher `q`'s catalog 0: a dataset of its own (one shard of `rows`
/// rows), a tombstone, and a links version whose one entry is that shard,
/// which does not read under the links schema: its index stays pending
/// (review SUCC-B3 H1). The big shard's root, and the catalog.
async fn a_pending_links_publisher(w: &World, q: &str, rows: i128) -> (Hash32, Catalog) {
    let mut r = rights();
    r.source = SourceId(q.into());
    let files = shard_files(0..rows, rows as usize);
    let big_entry = ShardEntry::new(0, &check_shard(&schema(), &files[0]).expect("ok").summary);
    let big = Manifest::build(
        NewVersion {
            dataset: "synthetic.big".into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 4,
            schema: schema(),
            rights: r.clone(),
        },
        None,
        vec![big_entry],
    )
    .expect("big manifest");
    let m = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: onyx_bulk::successor::links_schema(),
            rights: r,
        },
        None,
        vec![big_entry],
    )
    .expect("links manifest");
    let c = a_removing_catalog(
        w,
        q,
        vec![CatalogLeaf::of(&big.header), CatalogLeaf::of(&m.header)],
    )
    .await;
    let n = c.header.number;
    w.post_in(&big, n).await.expect("big manifest taken");
    w.store
        .accept_shard(
            q,
            "synthetic.big",
            &big_entry.root,
            &HeaderMap::new(),
            Body::from(files[0].clone()),
        )
        .await
        .expect("big shard");
    w.post_in(&m, n).await.expect("links manifest taken");
    (big_entry.root, c)
}

/// Publisher `q`'s catalog 0 with a links version of one shard per element
/// of `shards`, all uploaded, and with a tombstone when it `removes`: the
/// catalog, the manifest and the files.
async fn a_links_publisher_of(
    w: &World,
    q: &str,
    shards: &[Vec<Vec<u8>>],
    removes: bool,
) -> (Catalog, Manifest, Vec<Vec<u8>>) {
    let mut r = rights();
    r.source = SourceId(q.into());
    let s = onyx_bulk::successor::links_schema();
    let files: Vec<Vec<u8>> = shards.iter().map(|rows| encode_shard(rows)).collect();
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| ShardEntry::new(i as u32, &check_shard(&s, f).expect("shard").summary))
        .collect();
    let m = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: s,
            rights: r,
        },
        None,
        entries,
    )
    .expect("links manifest");
    let leaves = vec![CatalogLeaf::of(&m.header)];
    let c = if removes {
        a_removing_catalog(w, q, leaves).await
    } else {
        let c = Catalog::build(SourceId(q.into()), None, T0 + 10, leaves).expect("catalog");
        w.take(&c).await;
        c
    };
    w.post_in(&m, c.header.number)
        .await
        .expect("links manifest taken");
    for (f, e) in files.iter().zip(&m.shards) {
        w.store
            .accept_shard(q, LINKS, &e.root, &HeaderMap::new(), Body::from(f.clone()))
            .await
            .expect("links shard");
    }
    (c, m, files)
}

/// Inert links row `i` (a tombstone no catalog lists).
fn inert_row(i: i128) -> Vec<u8> {
    link_row(
        i,
        &format!("synthetic.gone{i}"),
        2,
        &Hash32([7; 32]),
        Some("synthetic.next"),
    )
}

/// Publisher `q`'s catalog 0 with a tombstone and a links version of `n`
/// inert rows, all uploaded.
async fn a_links_publisher(w: &World, q: &str, n: i128) -> Catalog {
    let rows: Vec<Vec<u8>> = (1..=n).map(inert_row).collect();
    a_links_publisher_of(w, q, &[rows], true).await.0
}

/// Publisher `q`'s next catalog after `c`, the same leaves, taken.
async fn next_catalog(w: &World, q: &str, c: &Catalog, created: u64) -> Catalog {
    let n = Catalog::build(SourceId(q.into()), Some(c), created, c.leaves.clone()).expect("next");
    w.take(&n).await;
    n
}

fn cap() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

impl World {
    /// The 410's `successor` of `dataset` as it stands now (no wait).
    async fn successor_now(&self, dataset: &str) -> Value {
        let e = self
            .store
            .rows(
                "example.publisher",
                dataset,
                &parse_rows_query(Some("")).expect("q"),
            )
            .await
            .expect_err("removed");
        field_of(&e, "successor")
    }

    /// The jobs of `q` so far.
    fn jobs_of(&self, q: &str) -> usize {
        self.store.succ_jobs().iter().filter(|j| *j == q).count()
    }
}

/// Every read route, for the publisher and `other`.
async fn read_everything(s: &Arc<BulkStore>, other: &str) {
    let q = parse_rows_query(Some("")).expect("q");
    let _ = s.rows("example.publisher", "synthetic.metrics", &q).await;
    let _ = s
        .info_checked("example.publisher", "synthetic.metrics")
        .await;
    let _ = s.info_checked("example.publisher", "synthetic.new").await;
    let _ = s
        .successor_answer("example.publisher", "synthetic.metrics")
        .await;
    let _ = s.replaces_of(other, "synthetic.next");
    let _ = s.replaces_all();
    let _ = list(State(s.clone()), None, RawQuery(None)).await;
    let app = routes(s.clone());
    for u in [
        "/v1/bulk".to_string(),
        "/v1/bulk/example.publisher/synthetic.metrics".into(),
        "/v1/bulk/example.publisher/synthetic.metrics/rows".into(),
        "/v1/bulk/example.publisher/synthetic.metrics/successor".into(),
        "/v1/bulk/example.publisher/synthetic.new".into(),
        format!("/v1/bulk/{other}/{LINKS}"),
    ] {
        let r = app
            .clone()
            .oneshot(Request::get(u).body(Body::empty()).expect("req"))
            .await
            .expect("ok");
        let _ = r.into_body().collect().await;
    }
}

/// Review SUCC-B3 H1 and M1 (the reviewer's attack), after the SUCC-B4
/// redesign: two publishers whose links never read (their leaf names their
/// own 60,000-row data shard), whose answers are read all the time and
/// whose held shards are sent again, never hold up another publisher's
/// build: none of that queues a job, and each big shard is loaded once.
#[tokio::test]
async fn cat_succ_two_pending_publishers_starve_no_other_build() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    let links_root = m.links.shards[0].root;
    m.w.put_in(LINKS, &links_root, m.links_file.clone()).await;
    m.w.settle().await;
    let hostile = ["example.third", "example.fourth"];
    for q in hostile {
        a_pending_links_publisher(&m.w, q, 60_000).await;
        m.w.store.succ_settle(q).await;
    }
    assert_eq!(
        m.w.store.succ_reads(),
        3,
        "the honest links shard and each big shard, once"
    );
    let jobs = m.w.store.succ_jobs().len();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s, st, file) = (m.w.store.clone(), stop.clone(), m.links_file.clone());
    let attacker = tokio::spawn(async move {
        let mut n = 0u64;
        while !st.load(Ordering::Relaxed) {
            // A held shard sent again (any client may), and reads.
            if n.is_multiple_of(50) {
                let _ = s
                    .accept_shard(
                        "example.publisher",
                        LINKS,
                        &links_root,
                        &HeaderMap::new(),
                        Body::from(file.clone()),
                    )
                    .await;
            }
            for q in hostile {
                let _ = s.replaces_of(q, "synthetic.x");
            }
            let _ = s.replaces_all();
            n += 1;
            tokio::task::yield_now().await;
        }
        n
    });
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    let c2 = catalog_of(Some(&m.c1), T0 + 30, leaves);
    m.w.take(&c2).await;
    let built = tokio::time::timeout(cap(), m.w.settle()).await;
    stop.store(true, Ordering::Relaxed);
    assert!(attacker.await.expect("attacker") > 0);
    assert!(built.is_ok(), "the index for catalog 2 is built");
    assert_eq!(
        m.w.store.succ_jobs()[jobs..].to_vec(),
        vec!["example.publisher".to_string()],
        "one job: the new catalog's"
    );
    assert_eq!(m.w.store.succ_reads(), 4, "no big shard loaded again");
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
}

/// Review SUCC-B3 M1 and SUCC-B4: a pending index is built again only for
/// the shard it waits for: a held shard sent again, or another publisher's
/// new links shard, queues nothing for it.
#[tokio::test]
async fn cat_succ_only_the_publishers_own_change_builds_a_pending_index_again() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.settle().await;
    let q = "example.publisher";
    let before = m.w.jobs_of(q);
    // A held shard of this publisher sent again.
    let files = shard_files(0..40, 10);
    assert_eq!(
        m.w.put_in("synthetic.new", &m.new.shards[0].root, files[0].clone())
            .await,
        StatusCode::OK
    );
    m.w.settle().await;
    assert_eq!(m.w.jobs_of(q), before, "held again: no job");
    // Another publisher's new links shard.
    a_links_publisher(&m.w, "example.second", 2).await;
    m.w.store.succ_settle("example.second").await;
    m.w.settle().await;
    assert_eq!(m.w.jobs_of(q), before, "another publisher's change");
    // The shard the index waits for.
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    assert_eq!(m.w.jobs_of(q), before + 1, "its own shard: one job");
}

/// Review SUCC-B4 (redesign): a held links shard that does not read under
/// the links schema is read again only for the publisher's next catalog
/// (or links manifest): no read, no time and no shard sent again does it.
#[tokio::test]
async fn cat_succ_an_unreadable_links_version_is_read_again_only_for_a_new_catalog() {
    let w = world();
    let q = "example.second";
    let (big, c) = a_pending_links_publisher(&w, q, 100).await;
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 1);
    w.clock.fetch_add(3_600, Ordering::Relaxed);
    let files = shard_files(0..100, 100);
    assert_eq!(
        w.store
            .accept_shard(
                q,
                "synthetic.big",
                &big,
                &HeaderMap::new(),
                Body::from(files[0].clone())
            )
            .await
            .expect("sent again")
            .0,
        StatusCode::OK
    );
    for _ in 0..3 {
        let _ = w.store.replaces_of(q, "synthetic.x");
        let _ = w.store.replaces_all();
    }
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 1, "nothing read again");
    next_catalog(&w, q, &c, T0 + 20).await;
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 2, "its next catalog: read once more");
}

/// Review SUCC-B3 L6: a catalog, a links manifest and a links shard taken
/// through the upload routes each queue one job, before any read; the read
/// after them builds nothing.
#[tokio::test]
async fn cat_succ_an_upload_starts_the_index_build_before_any_read() {
    let w = world();
    let files = shard_files(0..30, 10);
    let old = manifest(&files, None, T0);
    let new_files = shard_files(0..40, 10);
    let new = manifest_of("synthetic.new", &new_files, T0 + 1);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&old.header), CatalogLeaf::of(&new.header)],
    );
    w.take(&c0).await;
    w.post_in(&old, 0).await.expect("old");
    w.post_in(&new, 0).await.expect("new");
    w.settle().await;
    let (links, links_file) = links_version(&[move_row(1, &old, "synthetic.new")], None, T0 + 15);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&old.header).tombstone().expect("t"),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    let up = upload_routes(w.store.clone());
    let send = |req: Request<Body>| {
        let up = up.clone();
        async move { up.oneshot(req).await.expect("ok").status() }
    };
    let q = "example.publisher";
    let wr = &w;
    let one_more = move |n: usize| async move {
        wr.settle().await;
        wr.jobs_of(q) == n + 1 && wr.store.succ_built(q)
    };
    let n = w.jobs_of(q);
    let body = json!({"catalog": hex0x(&borsh::to_vec(&c1).expect("ok")), "report": hex0x(&borsh::to_vec(&r1).expect("ok"))});
    let s = send(
        Request::post("/v1/bulk/catalogs")
            .body(Body::from(body.to_string()))
            .expect("req"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(one_more(n).await, "the catalog upload built the index");
    let n = w.jobs_of(q);
    let body = json!({"manifest": hex0x(&links.to_bytes()), "catalog": 1});
    let s = send(
        Request::post("/v1/bulk/manifests")
            .body(Body::from(body.to_string()))
            .expect("req"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(
        one_more(n).await,
        "the links manifest upload built it again"
    );
    let n = w.jobs_of(q);
    let s = send(
        Request::put(format!(
            "/v1/bulk/example.publisher/{LINKS}/shards/{}",
            links.shards[0].root
        ))
        .body(Body::from(links_file))
        .expect("req"),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert!(one_more(n).await, "the links shard upload built it again");
    assert_eq!(w.store.succ_reads(), 1, "the links row read by that job");
    let n = w.jobs_of(q);
    assert_eq!(w.successor_now("synthetic.metrics").await["status"], "live");
    assert_eq!(
        (w.jobs_of(q), w.store.succ_reads()),
        (n, 1),
        "the read built nothing"
    );
}

/// Review SUCC-B3 L6 and SUCC-B4: a job that panics leaves the index as it
/// was and stops no other build; the publisher's next catalog builds it.
#[tokio::test]
async fn cat_succ_a_build_that_panics_stops_no_other_build() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.settle().await;
    m.w.store.succ_panic_next("example.publisher");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    assert_eq!(
        m.w.successor_now("synthetic.metrics").await["status"],
        "pending"
    );
    a_links_publisher(&m.w, "example.second", 1).await;
    tokio::time::timeout(cap(), m.w.store.succ_settle("example.second"))
        .await
        .expect("the worker goes on");
    assert_eq!(m.w.store.succ_kept("example.second"), 0);
    assert!(m.w.store.succ_built("example.second"));
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    m.w.take(&catalog_of(Some(&m.c1), T0 + 30, leaves)).await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
}

/// Review SUCC-B4 H1: the index keeps only the links that act in its
/// catalog (one per tombstone at most), never every row: 52 rows, one kept.
#[tokio::test]
async fn cat_succ_the_index_keeps_only_the_links_that_act_in_its_catalog() {
    let m = a_move().await;
    let mut rows: Vec<Vec<u8>> = (2..52).map(inert_row).collect();
    rows.push(move_row(52, &m.old, "synthetic.new"));
    let (v2, f2) = links_version(&rows, Some(&m.links), T0 + 25);
    let c2 = catalog_of(
        Some(&m.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&v2.header),
        ],
    );
    m.w.take(&c2).await;
    m.w.post_in(&m.links, 1).await.expect("links v1");
    m.w.post_in(&v2, 2).await.expect("links v2");
    m.w.put_in(LINKS, &v2.shards[0].root, m.links_file.clone())
        .await;
    m.w.put_in(LINKS, &v2.shards[1].root, f2).await;
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["row"].clone()),
        (json!("live"), json!("52"))
    );
    assert_eq!(m.w.store.succ_kept("example.publisher"), 1);
}

/// Review SUCC-B3 L4: once no generation is used, nothing of the links is
/// kept.
#[tokio::test]
async fn cat_succ_rows_of_a_generation_no_longer_used_are_let_go() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    assert_eq!(m.w.store.succ_kept("example.publisher"), 1);
    let c2 = catalog_of(
        Some(&m.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&m.links.header).tombstone().expect("t"),
        ],
    );
    m.w.take(&c2).await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "no_links"
    );
    assert_eq!(m.w.store.succ_kept("example.publisher"), 0);
}

/// Review SUCC-B3 L1: the 410 names the successor from the catalog the
/// removal was read from, matched by root, not number: an index built for
/// another catalog with the same number does not answer.
#[tokio::test]
async fn cat_succ_the_410_matches_its_catalog_by_root() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    let other = catalog_of(
        Some(&m.c0),
        T0 + 21,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&m.links.header),
        ],
    );
    assert_eq!(other.header.number, m.c1.header.number);
    let body = CatalogBody::new(other);
    assert_eq!(
        m.w.store
            .successor_json("example.publisher", "synthetic.metrics", 1, &body),
        json!({"status": "pending"})
    );
}

/// Review SUCC-B3 L3: when page 0 of the links cannot be read, the answer
/// is still given, with `end` pending and no links, not an error.
#[tokio::test]
async fn cat_succ_the_answer_without_its_links_page_is_pending() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    m.w.forget(&m.links.shards[0].root);
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("an answer");
    assert_eq!(a["end"], json!({"status": "pending"}));
    assert_eq!(a["links"], Value::Null);
    assert_eq!(a["chain"], json!([]));
}

/// Review SUCC-B3 M1: index builds take none of the readers' load permits:
/// with every one of them held (readers busy), a build still reads its
/// shards.
#[tokio::test]
async fn cat_succ_index_builds_take_no_reader_load_permits() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    let _busy =
        m.w.store
            .loads
            .acquire_many(MAX_LOADS as u32)
            .await
            .expect("every reader permit");
    let s = tokio::time::timeout(cap(), m.w.successor_of("synthetic.metrics"))
        .await
        .expect("built with every reader permit held");
    assert_eq!(s["status"], "live");
    assert_eq!(m.w.store.succ_reads(), 1);
}

/// Review SUCC-B4 (redesign): no read queues a build. With a settled index,
/// an index that waits for a shard, and one not built for the newest
/// catalog, every read route answers and the queue, the jobs and the reads
/// stay as they are.
#[tokio::test]
async fn cat_succ_a_read_never_queues_a_build() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.settle().await;
    a_links_publisher(&m.w, "example.second", 2).await;
    m.w.store.succ_settle("example.second").await;
    assert_eq!(
        m.w.successor_now("synthetic.metrics").await["status"],
        "pending",
        "waits for the links shard"
    );
    let (jobs, reads) = (m.w.store.succ_jobs(), m.w.store.succ_reads());
    for _ in 0..3 {
        read_everything(&m.w.store, "example.second").await;
    }
    assert_eq!(m.w.store.succ_queued(), Vec::<String>::new());
    m.w.settle().await;
    assert_eq!(
        (m.w.store.succ_jobs(), m.w.store.succ_reads()),
        (jobs.clone(), reads)
    );
    // Not built for the newest catalog: reads answer pending, queue nothing.
    m.w.store.succ_hold();
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    m.w.take(&catalog_of(Some(&m.c1), T0 + 30, leaves)).await;
    let queued = vec!["example.publisher".to_string()];
    assert_eq!(m.w.store.succ_queued(), queued);
    for _ in 0..3 {
        read_everything(&m.w.store, "example.second").await;
    }
    assert_eq!(m.w.store.succ_queued(), queued);
    assert_eq!(m.w.store.succ_jobs(), jobs);
    m.w.store.succ_release();
}

/// Review SUCC-B4 H1 (the reviewer's attack, kept as a test): publishers
/// with links of their own, read all the time and posting new catalogs,
/// used to evict the honest index at every cycle. Nothing is evicted now:
/// the honest 410 stays live at every moment and its index is never built
/// again.
#[tokio::test]
async fn cat_succ_other_publishers_churn_never_unbuilds_an_honest_index() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let hostile: Vec<String> = (0..17).map(|i| format!("example.hostile{i}")).collect();
    let mut cats = Vec::new();
    for q in &hostile {
        cats.push(a_links_publisher(&m.w, q, 2).await);
    }
    let q = "example.publisher";
    let built = m.w.jobs_of(q);
    let body = CatalogBody::new(m.c1.clone());
    let honest = || m.w.store.successor_json(q, "synthetic.metrics", 1, &body)["status"].clone();
    for round in 0..5u64 {
        for (h, c) in hostile.iter().zip(cats.iter_mut()) {
            let _ = m.w.store.replaces_of(h, "synthetic.next");
            *c = next_catalog(&m.w, h, c, T0 + 100 + round).await;
            assert_eq!(honest(), "live");
        }
        let _ = list(State(m.w.store.clone()), None, RawQuery(None)).await;
        assert_eq!(honest(), "live");
        for h in &hostile {
            m.w.store.succ_settle(h).await;
        }
        assert_eq!(honest(), "live");
    }
    assert_eq!(
        m.w.jobs_of(q),
        built,
        "the honest index was never built again"
    );
}

/// Review SUCC-B4 H1 and M1: churn of many hostile publishers delays the
/// honest index only by its place in the queue. With the worker held, each
/// hostile publisher posts catalogs again and again and is read; each is
/// queued once, the honest publisher once after them, and it is built
/// after exactly those jobs.
#[tokio::test]
async fn cat_succ_hostile_churn_delays_an_honest_build_only_by_its_place_in_the_queue() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    let hostile: Vec<String> = (0..8).map(|i| format!("example.hostile{i}")).collect();
    let mut cats = Vec::new();
    for h in &hostile {
        cats.push(a_links_publisher(&m.w, h, 2).await);
        m.w.store.succ_settle(h).await;
    }
    m.w.settle().await;
    let start = m.w.store.succ_jobs().len();
    m.w.store.succ_hold();
    for (k, (h, c)) in hostile.iter().zip(cats.iter_mut()).enumerate() {
        *c = next_catalog(&m.w, h, c, T0 + 100 + k as u64).await;
    }
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    for round in 0..3u64 {
        for (h, c) in hostile.iter().zip(cats.iter_mut()) {
            *c = next_catalog(&m.w, h, c, T0 + 200 + round).await;
            let _ = m.w.store.replaces_of(h, "synthetic.next");
        }
        let _ = list(State(m.w.store.clone()), None, RawQuery(None)).await;
    }
    let mut want = hostile.clone();
    want.push("example.publisher".into());
    assert_eq!(m.w.store.succ_queued(), want);
    m.w.store.succ_release();
    m.w.settle().await;
    assert_eq!(
        m.w.store.succ_jobs()[start..].to_vec(),
        want,
        "built after exactly the jobs queued before it"
    );
    assert_eq!(
        m.w.successor_now("synthetic.metrics").await["status"],
        "live"
    );
}

/// The publisher's links version 2 of 4 shards (version 1's and 3 more),
/// every shard held, in catalog 2 (review SUCC-B4).
async fn four_shard_links(m: &Move) -> (Manifest, Catalog) {
    let shards: Vec<Vec<Vec<u8>>> = (0..3).map(|i| vec![inert_row(i + 2)]).collect();
    let (v2, files) = links_version_of(&shards, Some(&m.links), T0 + 25);
    let c2 = catalog_of(
        Some(&m.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&v2.header),
        ],
    );
    m.w.take(&c2).await;
    m.w.post_in(&m.links, 1).await.expect("links v1");
    m.w.post_in(&v2, 2).await.expect("links v2");
    m.w.put_in(LINKS, &v2.shards[0].root, m.links_file.clone())
        .await;
    for (f, e) in files.iter().zip(&v2.shards[1..]) {
        m.w.put_in(LINKS, &e.root, f.clone()).await;
    }
    (v2, c2)
}

/// Review SUCC-B4 M2: a links version that only appends to the last one is
/// read whole, once, like any other (no part read to keep or to drop), and
/// versions taken while the worker is busy come to one read of the newest.
#[tokio::test]
async fn cat_succ_each_catalog_reads_its_links_version_whole_once() {
    let m = a_move().await;
    let (v2, c2) = four_shard_links(&m).await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    assert_eq!(m.w.store.succ_reads(), 4, "version 2 read whole, once");
    m.w.store.succ_hold();
    let leaves = |v: &Manifest| {
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&v.header),
        ]
    };
    let (v3, f3) = links_version(&[inert_row(9)], Some(&v2), T0 + 35);
    let c3 = catalog_of(Some(&c2), T0 + 40, leaves(&v3));
    m.w.take(&c3).await;
    m.w.post_in(&v3, 3).await.expect("links v3");
    m.w.put_in(LINKS, &v3.shards[4].root, f3).await;
    let (v4, f4) = links_version(&[move_row(10, &m.old, "synthetic.new")], Some(&v3), T0 + 45);
    m.w.take(&catalog_of(Some(&c3), T0 + 50, leaves(&v4))).await;
    m.w.post_in(&v4, 4).await.expect("links v4");
    m.w.put_in(LINKS, &v4.shards[5].root, f4).await;
    assert_eq!(
        m.w.store.succ_queued(),
        vec!["example.publisher".to_string()]
    );
    m.w.store.succ_release();
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["row"].clone()),
        (json!("live"), json!("10"))
    );
    assert_eq!(
        m.w.store.succ_reads(),
        4 + 6,
        "version 4 read whole, once; version 3 not at all"
    );
}

/// Review SUCC-B4 M1 and the redesign: one publisher's big but usable
/// links version (a 60,000-row shard between two small ones) is read in one
/// job, with no pass or time limit to cut it short, and costs one read per
/// catalog it posts: never one per request or per shard sent again.
#[tokio::test]
async fn cat_succ_a_big_links_version_costs_one_read_per_catalog() {
    let w = world();
    let q = "example.second";
    let shards = vec![
        vec![inert_row(1)],
        (2..60_002).map(inert_row).collect::<Vec<_>>(),
        vec![inert_row(60_002)],
    ];
    let (c, m, files) = a_links_publisher_of(&w, q, &shards, true).await;
    assert!(m.header.byte_count <= onyx_bulk::successor::LINKS_MAX_BYTES);
    w.store.succ_settle(q).await;
    assert!(w.store.succ_built(q));
    assert_eq!(w.store.succ_reads(), 3, "every shard read once");
    let jobs = w.store.succ_jobs().len();
    for _ in 0..50 {
        let _ = w.store.replaces_of(q, "synthetic.next");
        let _ = w.store.replaces_all();
        let _ = w.store.info_checked(q, LINKS).await;
    }
    for (f, e) in files.iter().zip(&m.shards) {
        w.store
            .accept_shard(q, LINKS, &e.root, &HeaderMap::new(), Body::from(f.clone()))
            .await
            .expect("sent again");
    }
    w.store.succ_settle(q).await;
    assert_eq!((w.store.succ_jobs().len(), w.store.succ_reads()), (jobs, 3));
    next_catalog(&w, q, &c, T0 + 20).await;
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 6, "its next catalog: read once more");
}

/// Review SUCC-B4 L1: the successor route reads page 0 of the links from
/// the held version: no fork read and no anchor read of the chain, even
/// after the recheck window (the rows route reads both then).
#[tokio::test]
async fn cat_succ_the_successor_route_reads_no_chain() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let before = m.w.node.reads();
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    assert_eq!(a["end"]["status"], "live");
    assert_eq!(
        a["links"]["manifest"]["root"],
        m.links.header.root().to_string()
    );
    assert_eq!(m.w.node.reads(), before, "no chain read");
    let root = m.links.header.root();
    m.w.store
        .rows(
            "example.publisher",
            LINKS,
            &parse_rows_query(Some(&format!("root={root}"))).expect("q"),
        )
        .await
        .expect("rows");
    assert!(
        m.w.node.reads() > before,
        "control: the rows route reads it"
    );
}

/// Review SUCC-B4 L2: an index build reads a warm links shard where it is:
/// no reader asked for it, so it stays warm.
#[tokio::test]
async fn cat_succ_an_index_build_leaves_shard_tiers_as_they_are() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    let root = m.links.shards[0].root;
    m.w.put_in(LINKS, &root, m.links_file.clone()).await;
    m.w.settle().await;
    m.w.store.tiers.to_warm(&root).expect("to warm");
    m.w.store.loaded.lock().expect("lock").clear();
    let reads = m.w.store.succ_reads();
    let mut leaves = m.c1.leaves.clone();
    leaves.push(other_leaf("synthetic.extra"));
    m.w.take(&catalog_of(Some(&m.c1), T0 + 30, leaves)).await;
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
    assert_eq!(m.w.store.succ_reads(), reads + 1, "read for catalog 2");
    assert_eq!(m.w.store.tier(&root), Some(Tier::Warm));
}

// --- Build review SUCC-B5 (the reviewer's attack tests, each with a
// tombstone in the attacker's catalog: since SUCC-B5 M2 a catalog without
// one reads nothing) ---

/// Wait (up to `secs`) until `publisher` is neither queued nor being built.
async fn succ_b5_settle(s: &BulkStore, publisher: &str, secs: u64) {
    let wait = async {
        while s.succ_busy(publisher) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(secs), wait)
        .await
        .expect("the build ends");
}

/// Review SUCC-B5 H1 (the reviewer's attack): `LINKS_MAX_BYTES` bounded only
/// the header's claim. Links entries that name one held shard (stored under
/// the publisher's own dataset, of 768,916 bytes) with the fewest bytes its
/// rows allow made a 4,352-byte header for a 49 MB job (65,536 rows of 48 KB
/// would be 3 GiB). Now a load refuses a file whose size is not its
/// entry's, before it reads it: the index stays pending.
#[tokio::test]
async fn cat_succ_a_job_reads_no_more_than_the_byte_cap() {
    let w = world();
    let q = "example.second";
    let s = onyx_bulk::successor::links_schema();
    let mut r = rights();
    r.source = SourceId(q.into());
    // 16 inert rows of about 48 KB (three 16,000-byte texts), all with key 1.
    let fat: Vec<Vec<u8>> = (0..16)
        .map(|_| {
            encode_row(
                &s,
                &[
                    Cell::Int(1),
                    Cell::Text("x".repeat(16_000)),
                    Cell::Int(2),
                    Cell::Text("y".repeat(16_000)),
                    Cell::Text("z".repeat(16_000)),
                ],
            )
            .expect("row")
        })
        .collect();
    let file = encode_shard(&fat);
    let sum = check_shard(&s, &file).expect("shard").summary;
    let data = Manifest::build(
        NewVersion {
            dataset: "synthetic.fat".into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 4,
            schema: s.clone(),
            rights: r.clone(),
        },
        None,
        vec![ShardEntry::new(0, &sum)],
    )
    .expect("data manifest");
    // The links version: 64 entries naming that one shard, each declaring
    // the fewest bytes its row count allows (equal one-key ranges may follow
    // each other).
    const K: u32 = 64;
    let entries: Vec<ShardEntry> = (0..K)
        .map(|i| {
            let mut e = ShardEntry::new(i, &sum);
            e.bytes = 4 + 4 * e.rows;
            e
        })
        .collect();
    let links = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: s,
            rights: r,
        },
        None,
        entries,
    )
    .expect("links manifest");
    assert!(onyx_bulk::successor::usable(&links.header).is_ok());
    let real = u64::from(K) * file.len() as u64;
    let c = a_removing_catalog(
        &w,
        q,
        vec![
            CatalogLeaf::of(&data.header),
            CatalogLeaf::of(&links.header),
        ],
    )
    .await;
    let n = c.header.number;
    w.post_in(&data, n).await.expect("data manifest taken");
    w.store
        .accept_shard(
            q,
            "synthetic.fat",
            &sum.root,
            &HeaderMap::new(),
            Body::from(file.clone()),
        )
        .await
        .expect("data shard");
    w.post_in(&links, n).await.expect("links manifest taken");
    succ_b5_settle(&w.store, q, 600).await;
    let settled = w.store.replaces_all().contains_key(q);
    let reads = w.store.succ_reads();
    assert!(
        !settled || real <= onyx_bulk::successor::LINKS_MAX_BYTES,
        "the index was built from {reads} loads, {real} real bytes, over the {} cap (header: {})",
        onyx_bulk::successor::LINKS_MAX_BYTES,
        links.header.byte_count
    );
    assert_eq!(reads, 1, "the first load is refused: the job stops there");
    // A reader of entry 0's rows is refused the same way: the file is in no
    // cache (an index load fills none), so its size on disk refuses it.
    let entry_0 = parse_rows_query(Some("limit=16")).expect("q");
    let e = w.store.rows(q, LINKS, &entry_0).await.expect_err("refused");
    assert_eq!(e.code, "bulk_shard_missing");
    // A reader of the data dataset loads that file under the links schema
    // into the cache: a cache hit is checked against the entry too.
    w.store
        .rows(q, "synthetic.fat", &parse_rows_query(Some("")).expect("q"))
        .await
        .expect("the data rows");
    let e = w.store.rows(q, LINKS, &entry_0).await.expect_err("refused");
    assert_eq!(e.code, "bulk_shard_missing");
}

/// Review SUCC-B5 H1: an entry that names a file the job loaded for an
/// earlier entry is checked against it too. Here the first entry is true
/// and the next three name the same file with the fewest bytes its row
/// allows: the job reads the file once and refuses the second entry, so the
/// file's rows are not taken again and again under a small header.
#[tokio::test]
async fn cat_succ_an_entry_naming_a_loaded_file_with_fewer_bytes_is_refused() {
    let w = world();
    let q = "example.second";
    let s = onyx_bulk::successor::links_schema();
    let mut r = rights();
    r.source = SourceId(q.into());
    let file = encode_shard(&[inert_row(1)]);
    let sum = check_shard(&s, &file).expect("shard").summary;
    let entries: Vec<ShardEntry> = (0..4)
        .map(|i| {
            let mut e = ShardEntry::new(i, &sum);
            if i > 0 {
                e.bytes = 4 + 4 * e.rows;
            }
            e
        })
        .collect();
    let links = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: s,
            rights: r,
        },
        None,
        entries,
    )
    .expect("links manifest");
    assert!(onyx_bulk::successor::usable(&links.header).is_ok());
    let c = a_removing_catalog(&w, q, vec![CatalogLeaf::of(&links.header)]).await;
    w.post_in(&links, c.header.number)
        .await
        .expect("links manifest taken");
    // Taken for the first entry, the true one.
    w.store
        .accept_shard(q, LINKS, &sum.root, &HeaderMap::new(), Body::from(file))
        .await
        .expect("the shard");
    w.store.succ_settle(q).await;
    assert!(w.store.succ_built(q));
    assert_eq!(w.store.succ_reads(), 1, "one load");
    assert!(
        !w.store.replaces_all().contains_key(q),
        "pending: entry 1 is false"
    );
}

/// Review SUCC-B5 M1 (the reviewer's attack): a job's cost was its
/// entries, not its bytes: one held one-row shard named by 65,536 entries
/// made a usable version (10 MB by its header, one 80-byte upload), and
/// each job of it loaded 65,537 shard files while an honest build waited
/// behind it (11.6 s). Now a version of more than `MAX_LINK_SHARDS` shards
/// is unusable, and a job loads each distinct shard once.
#[tokio::test]
async fn cat_succ_an_honest_build_never_waits_behind_a_job_of_65536_loads() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.settle().await;
    let w = &m.w;
    let q = "example.second";
    let s = onyx_bulk::successor::links_schema();
    let mut r = rights();
    r.source = SourceId(q.into());
    let file = encode_shard(&[inert_row(1)]);
    let sum = check_shard(&s, &file).expect("shard").summary;
    const K: u32 = 65_536;
    let entries: Vec<ShardEntry> = (0..K).map(|i| ShardEntry::new(i, &sum)).collect();
    let links = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: s,
            rights: r,
        },
        None,
        entries,
    )
    .expect("links manifest");
    let c = a_removing_catalog(w, q, vec![CatalogLeaf::of(&links.header)]).await;
    w.post_in(&links, c.header.number)
        .await
        .expect("links manifest taken");
    succ_b5_settle(&w.store, q, 600).await;
    let before = w.store.succ_reads();
    // The one upload: the attacker's job starts.
    w.store
        .accept_shard(q, LINKS, &sum.root, &HeaderMap::new(), Body::from(file))
        .await
        .expect("the one shard");
    // The honest links shard comes now: its build is queued behind.
    w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    succ_b5_settle(&w.store, "example.publisher", 900).await;
    let loads = w.store.succ_reads() - before;
    assert_eq!(w.successor_now("synthetic.metrics").await["status"], "live");
    assert!(loads < 1_024, "one job of {loads} shard loads");
    assert_eq!(
        loads, 1,
        "the attacker's version is unusable: only the honest shard"
    );
}

/// Review SUCC-B5 M1: entries that name one shard again load it once a
/// job; its rows still count at each entry (readers read them all).
#[tokio::test]
async fn cat_succ_a_shard_named_again_is_loaded_once_a_job() {
    let w = world();
    let q = "example.second";
    let row = inert_row(1);
    let shards = vec![vec![row.clone()], vec![row.clone()], vec![row]];
    let (_, m, _) = a_links_publisher_of(&w, q, &shards, true).await;
    assert_eq!(
        m.shards[0].root, m.shards[2].root,
        "one file named three times"
    );
    w.store.succ_settle(q).await;
    assert!(w.store.replaces_all().contains_key(q), "built, not pending");
    assert_eq!(w.store.succ_reads(), 1, "one load for three entries");
}

/// Review SUCC-B5 L1 (the reviewer's attack): a links manifest of a
/// generation the catalog lists but does not use queued a job that read
/// the used (unreadable) version whole again: up to 8 reads a catalog. Now
/// a pending index waits for exactly one header or shard.
#[tokio::test]
async fn cat_succ_an_unused_generation_reads_nothing_again() {
    let w = world();
    let q = "example.second";
    let mut r = rights();
    r.source = SourceId(q.into());
    let files = shard_files(0..100, 100);
    let big_entry = ShardEntry::new(0, &check_shard(&schema(), &files[0]).expect("ok").summary);
    let big = Manifest::build(
        NewVersion {
            dataset: "synthetic.big".into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 4,
            schema: schema(),
            rights: r.clone(),
        },
        None,
        vec![big_entry],
    )
    .expect("big manifest");
    // Generation 1 names the data shard: it never reads under the links schema.
    let g1 = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: onyx_bulk::successor::links_schema(),
            rights: r.clone(),
        },
        None,
        vec![big_entry],
    )
    .expect("links manifest");
    let s = onyx_bulk::successor::links_schema();
    let f2 = encode_shard(&[inert_row(1)]);
    let g2 = Manifest::build(
        NewVersion {
            dataset: onyx_bulk::successor::generation_id(2).expect("in range"),
            publisher: SourceId(q.into()),
            created_unix: T0 + 6,
            schema: s.clone(),
            rights: r,
        },
        None,
        vec![ShardEntry::new(
            0,
            &check_shard(&s, &f2).expect("ok").summary,
        )],
    )
    .expect("generation 2");
    let c = a_removing_catalog(
        &w,
        q,
        vec![
            CatalogLeaf::of(&big.header),
            CatalogLeaf::of(&g1.header),
            CatalogLeaf::of(&g2.header),
        ],
    )
    .await;
    let n = c.header.number;
    w.post_in(&big, n).await.expect("big manifest taken");
    w.store
        .accept_shard(
            q,
            "synthetic.big",
            &big_entry.root,
            &HeaderMap::new(),
            Body::from(files[0].clone()),
        )
        .await
        .expect("big shard");
    w.post_in(&g1, n).await.expect("generation 1 taken");
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 1, "generation 1 read once");
    w.post_in(&g2, n).await.expect("generation 2 taken");
    w.store.succ_settle(q).await;
    assert_eq!(
        w.store.succ_reads(),
        1,
        "a manifest of an unused generation read generation 1 again"
    );
}

/// Review SUCC-B5 L2: a shard stored after a job found it missing but
/// before the job stored that it waits for it is not lost: the job checks
/// again once its wait is stored, and queues itself.
#[tokio::test]
async fn cat_succ_a_shard_stored_while_a_job_stores_its_wait_is_not_lost() {
    let m = a_move().await;
    m.w.settle().await;
    m.w.store.succ_hold_store();
    // It waits for this header: the job runs, finds the shard missing and
    // stops before it stores that.
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    let at = async {
        while !m.w.store.succ_at_store_gate() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(cap(), at).await.expect("at the gate");
    // The index still waits for the header: this upload queues nothing.
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.store.succ_release_store();
    assert_eq!(
        m.w.successor_of("synthetic.metrics").await["status"],
        "live"
    );
}

/// Review SUCC-B5 L2: the same for a header: a manifest stored after a job
/// found it missing but before the job stored its wait is not lost.
#[tokio::test]
async fn cat_succ_a_header_stored_while_a_job_stores_its_wait_is_not_lost() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let (v2, f2) = links_version(
        &[move_row(2, &m.old, "synthetic.new")],
        Some(&m.links),
        T0 + 25,
    );
    let c2 = catalog_of(
        Some(&m.c1),
        T0 + 30,
        vec![
            CatalogLeaf::of(&m.old.header).tombstone().expect("t"),
            CatalogLeaf::of(&m.new.header),
            CatalogLeaf::of(&v2.header),
        ],
    );
    m.w.store.succ_hold_store();
    m.w.take(&c2).await;
    let at = async {
        while !m.w.store.succ_at_store_gate() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(cap(), at).await.expect("at the gate");
    // The index for catalog 1 waits for nothing: this queues nothing.
    m.w.post_in(&v2, 2).await.expect("links v2");
    m.w.store.succ_release_store();
    m.w.settle().await;
    m.w.put_in(LINKS, &v2.shards[1].root, f2).await;
    let s = m.w.successor_of("synthetic.metrics").await;
    assert_eq!(
        (s["status"].clone(), s["row"].clone()),
        (json!("live"), json!("2"))
    );
}

/// Review SUCC-B5 L3: page 0 of the links is refused, as `serving` refuses
/// the version, when the last anchor read found its report no longer held;
/// still with no chain read.
#[tokio::test]
async fn cat_succ_page_0_is_refused_when_its_version_was_found_not_held() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let digest =
        m.w.store
            .datasets
            .read()
            .expect("lock")
            .get(&("example.publisher".into(), LINKS.into()))
            .expect("held")
            .versions[0]
            .report
            .payload
            .digest();
    m.w.store.mark(digest, false);
    let before = m.w.node.reads();
    let a =
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    assert_eq!(a["end"], json!({"status": "pending"}));
    assert_eq!(a["links"], Value::Null);
    assert_eq!(m.w.node.reads(), before, "no chain read");
    let root = m.links.header.root();
    m.w.store
        .rows(
            "example.publisher",
            LINKS,
            &parse_rows_query(Some(&format!("root={root}"))).expect("q"),
        )
        .await
        .expect_err("the rows route refuses it too");
}

/// Review SUCC-B5 L3: a "not held" verdict older than `ANCHOR_RECHECK_SECS`
/// still refuses page 0 (the answer reads no chain), and is read again in
/// the background as the rows route would read it: a report held again
/// serves page 0 again.
#[tokio::test]
async fn cat_succ_a_stale_not_held_verdict_is_read_again_in_the_background() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let digest =
        m.w.store
            .datasets
            .read()
            .expect("lock")
            .get(&("example.publisher".into(), LINKS.into()))
            .expect("held")
            .versions[0]
            .report
            .payload
            .digest();
    m.w.store.mark(digest, false);
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let answer = || {
        m.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
    };
    let a = answer().await.expect("answer");
    assert_eq!(a["links"], Value::Null, "refused until it is read again");
    let held = async {
        loop {
            let c =
                m.w.store
                    .rechecked
                    .lock()
                    .expect("lock")
                    .get(&digest)
                    .copied();
            if c.is_some_and(|(_, held)| held) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    };
    tokio::time::timeout(cap(), held)
        .await
        .expect("read again in the background");
    let a = answer().await.expect("answer");
    assert_eq!(a["end"]["status"], "live");
    assert_ne!(a["links"], Value::Null);
}

/// Review SUCC-B5 M2: a start queues the publishers whose newest catalog
/// removes a dataset first and reads nothing for the others (nothing can
/// act in their catalogs), so an honest publisher with a move is not left
/// pending behind every other publisher's read.
#[tokio::test]
async fn cat_succ_a_start_reads_only_catalogs_that_remove_and_queues_them_first() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    for q in ["example.aaa", "example.bbb"] {
        a_links_publisher_of(&m.w, q, &[vec![inert_row(1)]], false).await;
    }
    // Bigger than the publisher's version: the smaller goes first (SUCC-B6
    // L1).
    a_links_publisher(&m.w, "example.zzz", 3).await;
    for q in [
        "example.publisher",
        "example.aaa",
        "example.bbb",
        "example.zzz",
    ] {
        m.w.store.succ_settle(q).await;
    }
    let again = Arc::new(
        BulkStore::open(
            pin(),
            m.w.node.clone(),
            Arc::new(|| T0),
            &m.w.dir,
            &m.w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("reopen"),
    );
    again.succ_hold();
    again.succ_start();
    let order = [
        "example.publisher",
        "example.zzz",
        "example.aaa",
        "example.bbb",
    ];
    assert_eq!(again.succ_queued(), order.map(String::from).to_vec());
    again.succ_release();
    for q in order {
        again.succ_settle(q).await;
        assert!(again.succ_built(q), "{q} built");
    }
    assert_eq!(again.succ_reads(), 2, "only the catalogs that remove read");
    assert_eq!(
        again.replaces_all().get("example.aaa").map(|r| r.1),
        Some(0)
    );
}

// --- Build review SUCC-B6 ---

/// Review SUCC-B6 (the reviewer's guard check): a shard the index waited
/// for, sent again after the job it woke failed its read, queues nothing.
/// Only `State::set_wait` dropping the old waiter stops it; without that,
/// every re-send of the shard is one more full read of the version.
#[tokio::test]
async fn cat_succ_a_waited_shard_sent_again_after_a_failed_read_reads_nothing() {
    let w = world();
    let q = "example.second";
    let s = onyx_bulk::successor::links_schema();
    let mut r = rights();
    r.source = SourceId(q.into());
    let fx = encode_shard(&[inert_row(1)]);
    let sx = check_shard(&s, &fx).expect("x").summary;
    let fr = encode_shard(&[inert_row(2)]);
    let sr = check_shard(&s, &fr).expect("r").summary;
    // R is held under a data dataset (links schema, its true entry there).
    let data = Manifest::build(
        NewVersion {
            dataset: "synthetic.fat".into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 4,
            schema: s.clone(),
            rights: r.clone(),
        },
        None,
        vec![ShardEntry::new(0, &sr)],
    )
    .expect("data");
    // The links version: X (true), then R with a false size.
    let mut er = ShardEntry::new(1, &sr);
    er.bytes += 1;
    let links = Manifest::build(
        NewVersion {
            dataset: LINKS.into(),
            publisher: SourceId(q.into()),
            created_unix: T0 + 5,
            schema: s,
            rights: r,
        },
        None,
        vec![ShardEntry::new(0, &sx), er],
    )
    .expect("links");
    let c = a_removing_catalog(
        &w,
        q,
        vec![
            CatalogLeaf::of(&data.header),
            CatalogLeaf::of(&links.header),
        ],
    )
    .await;
    let n = c.header.number;
    w.post_in(&data, n).await.expect("data manifest");
    w.store
        .accept_shard(
            q,
            "synthetic.fat",
            &sr.root,
            &HeaderMap::new(),
            Body::from(fr),
        )
        .await
        .expect("R");
    w.post_in(&links, n).await.expect("links manifest");
    w.store.succ_settle(q).await;
    assert_eq!(w.store.succ_reads(), 0, "waits for X");
    let hm = HeaderMap::new();
    let send_x = || {
        w.store
            .accept_shard(q, LINKS, &sx.root, &hm, Body::from(fx.clone()))
    };
    send_x().await.expect("X");
    w.store.succ_settle(q).await;
    let reads = w.store.succ_reads();
    assert_eq!(reads, 2, "X loaded, R refused by its size");
    assert!(!w.store.replaces_all().contains_key(q), "pending");
    let jobs = w.jobs_of(q);
    for _ in 0..3 {
        send_x().await.expect("X again");
        w.store.succ_settle(q).await;
    }
    assert_eq!(w.jobs_of(q), jobs, "X sent again queues no job");
    assert_eq!(w.store.succ_reads(), reads, "and reads nothing");
}

/// A JSON-RPC that counts calls and, like a real one, is not ready at once.
struct SlowRpc {
    inner: Arc<Node>,
    calls: AtomicU64,
}

impl JsonRpc for SlowRpc {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.inner.call(method, p).await
        })
    }
}

/// Review SUCC-B6 M1 (the reviewer's attack): in a stale "not held" window
/// every successor answer spawned its own background chain read (250 calls
/// for 50 answers), and a failed read left the verdict stale, so each
/// answer after it read again (50 with the RPC down). Now the first answer
/// claims the window's one read, and a failed read writes "not held" anew,
/// as `fork_stands` does (review CAT-6 L3).
#[tokio::test]
async fn cat_succ_a_stale_verdict_is_read_once_however_many_answers() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m.w.settle().await;
    let rpc = Arc::new(SlowRpc {
        inner: m.w.node.clone(),
        calls: AtomicU64::new(0),
    });
    let c = m.w.clock.clone();
    let s = started(
        BulkStore::open(
            pin(),
            rpc.clone(),
            Arc::new(move || c.load(Ordering::Relaxed)),
            &m.w.dir,
            &m.w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("reopen"),
    );
    s.succ_settle("example.publisher").await;
    let digest = s
        .datasets
        .read()
        .expect("lock")
        .get(&("example.publisher".into(), LINKS.into()))
        .expect("held")
        .versions[0]
        .report
        .payload
        .digest();
    s.mark(digest, false);
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    // One answer alone: how many calls one background read makes.
    let before = rpc.calls.load(Ordering::SeqCst);
    let a = s
        .successor_answer("example.publisher", "synthetic.metrics")
        .await
        .expect("answer");
    assert_eq!(a["links"], Value::Null);
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    let one = rpc.calls.load(Ordering::SeqCst) - before;
    assert!(one > 0, "a background read");
    // Stale again; fifty answers in the same window.
    s.mark(digest, false);
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let before = rpc.calls.load(Ordering::SeqCst);
    for _ in 0..50 {
        let _ = s
            .successor_answer("example.publisher", "synthetic.metrics")
            .await;
    }
    for _ in 0..2_000 {
        tokio::task::yield_now().await;
    }
    let fifty = rpc.calls.load(Ordering::SeqCst) - before;
    // The RPC down: a failed read writes "not held" anew, so the answers
    // after it read nothing in this window.
    s.mark(digest, false);
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    m.w.node.0.lock().expect("lock").down = true;
    let before = rpc.calls.load(Ordering::SeqCst);
    for _ in 0..50 {
        let _ = s
            .successor_answer("example.publisher", "synthetic.metrics")
            .await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    let down = rpc.calls.load(Ordering::SeqCst) - before;
    assert!(
        fifty <= 2 * one && down <= 2,
        "one read is {one} chain calls; 50 answers in one stale window made {fifty}; \
         with the RPC down, 50 answers one at a time made {down}"
    );
    // The claim is let go after each read: a later stale window reads.
    m.w.node.0.lock().expect("lock").down = false;
    m.w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let before = rpc.calls.load(Ordering::SeqCst);
    let _ = s
        .successor_answer("example.publisher", "synthetic.metrics")
        .await;
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert!(rpc.calls.load(Ordering::SeqCst) > before, "read again");
}

/// Review SUCC-B6 L1: at start, among the publishers whose catalog removes
/// a dataset, the one with the smallest links version is built first, not
/// the first by name: publishers named to sort first with big versions do
/// not hold back an honest 410 at every restart.
#[tokio::test]
async fn cat_succ_a_start_builds_the_smallest_links_version_first() {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    for (q, n) in [("example.aaa", 2_000), ("example.aab", 500)] {
        a_links_publisher(&m.w, q, n).await;
    }
    for q in ["example.publisher", "example.aaa", "example.aab"] {
        m.w.store.succ_settle(q).await;
    }
    let again = Arc::new(
        BulkStore::open(
            pin(),
            m.w.node.clone(),
            Arc::new(|| T0),
            &m.w.dir,
            &m.w.dir.join("warm"),
            BulkLimits::default(),
        )
        .expect("reopen"),
    );
    again.succ_hold();
    again.succ_start();
    let order = ["example.publisher", "example.aab", "example.aaa"];
    assert_eq!(again.succ_queued(), order.map(String::from).to_vec());
    again.succ_release();
    for q in order {
        again.succ_settle(q).await;
        assert!(again.replaces_all().contains_key(q), "{q} built");
    }
}

// --- The reader's successor check (ADR-0059 amendment 6 item 7) ---

use crate::bulk_successor::{
    check_successor, check_successor_unchecked, SuccessorOpts, SuccessorOutcome,
};

fn sopts(keys: Vec<[u8; 20]>, scan_from: Option<u64>) -> SuccessorOpts {
    SuccessorOpts {
        node: Some("http://node".into()),
        keys,
        scan_from,
        logs_range: 50,
    }
}

/// The node's successor answer for `dataset`, once its index is built.
async fn answer_of(w: &World, dataset: &str) -> Value {
    w.settle().await;
    w.store
        .successor_answer("example.publisher", dataset)
        .await
        .expect("answer")
}

/// The reader's check of `answer` against the world's node and book.
async fn succ_check_with(
    w: &World,
    get: &dyn crate::storage::HttpGet,
    dataset: &str,
    answer: &Value,
    o: &SuccessorOpts,
) -> SuccessorOutcome {
    check_successor(
        get,
        w.node.as_ref(),
        CHAIN,
        BOOK,
        answer,
        "example.publisher",
        dataset,
        o,
    )
    .await
}

async fn succ_check(
    w: &World,
    dataset: &str,
    answer: &Value,
    o: &SuccessorOpts,
) -> SuccessorOutcome {
    let get = RouterGet(routes(w.store.clone()));
    succ_check_with(w, &get, dataset, answer, o).await
}

fn succ_refused(o: SuccessorOutcome) -> (&'static str, String) {
    match o {
        SuccessorOutcome::Refused(f, w) => (f, w),
        other => panic!("not refused: {}", other.report()),
    }
}

fn succ_undecided(o: SuccessorOutcome) -> String {
    match o {
        SuccessorOutcome::Undecided(w) => w,
        other => panic!("not undecided: {}", other.report()),
    }
}

/// [`a_move`] with its links version held: the old id's successor is live.
async fn a_held_move() -> Move {
    let m = a_move().await;
    m.w.post_in(&m.links, 1).await.expect("links manifest");
    m.w.put_in(LINKS, &m.links.shards[0].root, m.links_file.clone())
        .await;
    m
}

#[tokio::test]
async fn bulk_successor_a_move_verifies_on_chain() {
    let m = a_held_move().await;
    let a = answer_of(&m.w, "synthetic.metrics").await;
    let o = succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    let v = o.report();
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["chainChecked"], json!(true));
    assert_eq!(
        v["hops"],
        json!([{"from": "synthetic.metrics", "version": 2, "row": "1", "position": 0, "to": "synthetic.new"}])
    );
    assert_eq!(
        v["end"],
        json!({"status": "live", "dataset": "synthetic.new", "version": 1})
    );
    assert_eq!(v["catalog"]["number"], json!(1));
    assert_eq!(v["generation"], json!(1));
    assert_eq!(v["links"]["pages"], json!(1));
    assert_eq!(v["checkedAt"]["number"], json!(100));
    assert!(v["limit"].is_string(), "no scan: the limit is stated");
    assert_eq!(v["note"], json!(crate::bulk_successor::SUCCESSOR_NOTE));
    // Off chain only: never verified.
    let get = RouterGet(routes(m.w.store.clone()));
    let u =
        check_successor_unchecked(&get, &a, "example.publisher", "synthetic.metrics", None).await;
    assert_eq!(u.code(), 3);
    assert_eq!(u.report()["chainChecked"], json!(false));
    assert_eq!(u.report()["end"]["status"], json!("live"));
    // Another dataset than the one asked, or an end the proofs do not give:
    // the node's.
    let (f, why) = succ_refused(succ_check(&m.w, "synthetic.new", &a, &sopts(vec![], None)).await);
    assert_eq!(f, "node", "{why}");
    let mut claimed = a.clone();
    claimed["end"]["status"] = json!("removed");
    let (f, why) =
        succ_refused(succ_check(&m.w, "synthetic.metrics", &claimed, &sopts(vec![], None)).await);
    assert_eq!(f, "node");
    assert!(why.contains("not what its proofs give"), "{why}");
    // A leaf proof of another catalog: the node's.
    let mut other = a.clone();
    other["chain"][1] = a["chain"][0].clone();
    assert_eq!(
        succ_refused(succ_check(&m.w, "synthetic.metrics", &other, &sopts(vec![], None)).await).0,
        "node"
    );
    // The catalog's report not anchored at the caller's block: undecided.
    m.w.node
        .0
        .lock()
        .expect("ok")
        .reports
        .remove(&sign_catalog(&m.c1).payload.digest());
    let why = succ_undecided(succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], None)).await);
    assert!(why.contains("catalog 1"), "{why}");
    let _ = (&m.old, &m.new, &m.c0);
    let _ = std::fs::remove_dir_all(&m.w.dir);
}

/// A move whose links version holds `fillers` rows for made-up tombstones,
/// then the row that moves `synthetic.metrics` to `synthetic.new`, `per`
/// rows a shard; catalog 1 lists it and the node holds it all.
async fn a_big_move(fillers: usize, per: usize) -> (World, Manifest) {
    let w = world();
    let files = shard_files(0..30, 10);
    let old = manifest(&files, None, T0);
    let new = manifest_of("synthetic.new", &shard_files(0..10, 10), T0 + 1);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&old.header), CatalogLeaf::of(&new.header)],
    );
    w.take(&c0).await;
    let mut rows: Vec<Vec<u8>> = (0..fillers)
        .map(|i| {
            link_row(
                i as i128 + 1,
                &format!("synthetic.x{i}"),
                2,
                &Hash32([9; 32]),
                Some("synthetic.new"),
            )
        })
        .collect();
    rows.push(link_row(
        fillers as i128 + 1,
        "synthetic.metrics",
        2,
        &old.header.root(),
        Some("synthetic.new"),
    ));
    let shards: Vec<Vec<Vec<u8>>> = rows.chunks(per).map(<[Vec<u8>]>::to_vec).collect();
    let (links, links_files) = links_version_of(&shards, None, T0 + 15);
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&old.header).tombstone().expect("t"),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    w.take(&c1).await;
    w.post_in(&links, 1).await.expect("links manifest");
    for (f, e) in links_files.iter().zip(&links.shards) {
        assert!(w.put_in(LINKS, &e.root, f.clone()).await.is_success());
    }
    (w, links)
}

#[tokio::test]
async fn bulk_successor_a_withheld_row_is_the_nodes_fault() {
    // 1,001 rows: page 0 in the answer, page 1 from the node.
    let (w, links) = a_big_move(1_000, 600).await;
    let a = answer_of(&w, "synthetic.metrics").await;
    let o = succ_check(&w, "synthetic.metrics", &a, &sopts(vec![], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(o.report()["links"]["pages"], json!(2));
    assert_eq!(o.report()["hops"][0]["position"], json!(1_000));
    // Without --node the second page cannot be read: nothing decided.
    let no_node = SuccessorOpts {
        node: None,
        ..sopts(vec![], None)
    };
    let why = succ_undecided(succ_check(&w, "synthetic.metrics", &a, &no_node).await);
    assert!(why.contains("--node"), "{why}");
    // Page 1 answered 404, 409 or 410: an honest node does that after a
    // set-aside or when `--node` is another node that lags, so nothing is
    // decided (review SUCC-B7 L1).
    let page1 = format!("root={}&page=1", links.header.root());
    for status in [404, 409, 410] {
        let get = HookGet {
            inner: RouterGet(routes(w.store.clone())),
            hook: |u: &str| u.contains(&page1).then(|| (status, b"{}".to_vec())),
        };
        let why = succ_undecided(
            succ_check_with(&w, &get, "synthetic.metrics", &a, &sopts(vec![], None)).await,
        );
        assert!(why.contains("links page 1"), "{status}: {why}");
    }
    // Page 1 sent empty, or page 0 sent again: the node's.
    let page0 = serde_json::to_vec(&a["links"]).expect("json");
    let mut empty = a["links"].clone();
    empty["rows"] = json!([]);
    let empty = serde_json::to_vec(&empty).expect("json");
    for (name, answer) in [("empty", (200, empty)), ("page 0 again", (200, page0))] {
        let get = HookGet {
            inner: RouterGet(routes(w.store.clone())),
            hook: |u: &str| u.contains(&page1).then(|| answer.clone()),
        };
        let (f, why) = succ_refused(
            succ_check_with(&w, &get, "synthetic.metrics", &a, &sopts(vec![], None)).await,
        );
        assert_eq!(f, "node", "{name}: {why}");
    }
    // A row taken out of page 0: the run does not join.
    let mut short = a.clone();
    short["links"]["rows"].as_array_mut().expect("rows").pop();
    let (f, why) =
        succ_refused(succ_check(&w, "synthetic.metrics", &short, &sopts(vec![], None)).await);
    assert_eq!(f, "node");
    assert!(why.contains("row"), "{why}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn bulk_successor_an_old_catalog_is_undecided_when_a_scanned_key_holds_the_next() {
    let m = a_held_move().await;
    let a = answer_of(&m.w, "synthetic.metrics").await;
    // Catalog 2 under the publisher's other key, which this node never got.
    let c2 = catalog_of(Some(&m.c1), T0 + 30, m.c1.leaves.clone());
    let r2 = cat::sign(&SECRET2, &c2.header).expect("ok");
    m.w.node.anchor_by(&r2, &SECRET2);
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    // Unknown to the check: the stated limit.
    let o = succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(
        o.report()["limit"],
        json!(crate::bulk_successor::SUCCESSOR_LIMIT)
    );
    // Named by the caller, or found by a key scan: an old catalog.
    let why = succ_undecided(
        succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![signer2], None)).await,
    );
    assert!(why.contains("catalog 2"), "{why}");
    {
        let mut b = m.w.node.0.lock().expect("ok");
        b.sources
            .insert(r2.payload.key, keccak256(publisher().0.as_bytes()));
        b.logs.push((0x31, r2.payload.key));
    }
    let why =
        succ_undecided(succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], Some(0))).await);
    assert!(why.contains("catalog 2"), "{why}");
    // Withdrawn for good, it no longer counts.
    m.w.node.withdraw(&r2);
    let o = succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], Some(0))).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(o.report()["keysScanned"]["from"], json!(0));
    // Under the catalog's own key, no option is needed.
    let c2 = catalog_of(Some(&m.c1), T0 + 31, m.c1.leaves.clone());
    m.w.node.anchor(&sign_catalog(&c2));
    let why = succ_undecided(succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![], None)).await);
    assert!(why.contains("catalog 2"), "{why}");
    let _ = std::fs::remove_dir_all(&m.w.dir);
}

#[tokio::test]
async fn bulk_successor_every_end_but_live_is_exit_4() {
    let w = world();
    let p = Hash32([5; 32]);
    let mut ids: Vec<String> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|s| format!("synthetic.{s}"))
        .collect();
    ids.extend((0..=9).map(|i| format!("synthetic.t{i}")));
    let c0 = catalog_of(None, T0 + 10, ids.iter().map(|d| other_leaf(d)).collect());
    w.take(&c0).await;
    let row = |n: i128, from: &str, to: Option<&str>| link_row(n, from, 2, &p, to);
    let mut rows = vec![
        row(1, "synthetic.b", Some("synthetic.c")),
        row(2, "synthetic.b", None),
        row(3, "synthetic.c", Some("synthetic.gone")),
        row(4, "synthetic.d", Some("synthetic.e")),
        row(5, "synthetic.e", Some("synthetic.d")),
    ];
    for i in 0..9 {
        rows.push(row(
            6 + i,
            &format!("synthetic.t{i}"),
            Some(&format!("synthetic.t{}", i + 1)),
        ));
    }
    let (links, file) = links_version(&rows, None, T0 + 15);
    let mut leaves: Vec<CatalogLeaf> = c0
        .leaves
        .iter()
        .map(|l| {
            if l.dataset == "synthetic.t9" {
                l.clone()
            } else {
                l.tombstone().expect("t")
            }
        })
        .collect();
    leaves.push(CatalogLeaf::of(&links.header));
    let c1 = catalog_of(Some(&c0), T0 + 20, leaves);
    w.take(&c1).await;
    w.post_in(&links, 1).await.expect("links manifest");
    w.put_in(LINKS, &links.shards[0].root, file).await;
    for (start, status, last) in [
        ("synthetic.a", "removed", "synthetic.a"),
        ("synthetic.b", "retracted", "synthetic.b"),
        ("synthetic.c", "not_listed", "synthetic.gone"),
        ("synthetic.d", "cycle", "synthetic.d"),
        ("synthetic.t0", "too_long", "synthetic.t8"),
    ] {
        let a = answer_of(&w, start).await;
        let o = succ_check(&w, start, &a, &sopts(vec![], None)).await;
        assert_eq!(o.code(), 4, "{start}: {}", o.report());
        assert_eq!(o.report()["end"]["status"], json!(status), "{start}");
        assert_eq!(o.report()["end"]["dataset"], json!(last), "{start}");
        assert_eq!(o.report()["ok"], json!(false));
    }
    // No links dataset at all: no_links.
    let n = world();
    let c0 = catalog_of(None, T0 + 10, vec![other_leaf("synthetic.a")]);
    n.take(&c0).await;
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![other_leaf("synthetic.a").tombstone().expect("t")],
    );
    n.take(&c1).await;
    let a = answer_of(&n, "synthetic.a").await;
    let o = succ_check(&n, "synthetic.a", &a, &sopts(vec![], None)).await;
    assert_eq!(o.code(), 4, "{}", o.report());
    assert_eq!(o.report()["end"]["status"], json!("no_links"));
    assert_eq!(o.report()["generation"], Value::Null);
    for d in [&w.dir, &n.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn bulk_successor_an_old_nodes_404_is_exit_3() {
    let m = a_held_move().await;
    // An old node: 404 on the route, or its 410 with no successor.
    let gone =
        m.w.store
            .rows(
                "example.publisher",
                "synthetic.metrics",
                &parse_rows_query(Some("")).expect("q"),
            )
            .await
            .expect_err("removed");
    for answer in [
        json!({"error": {"code": "not_found", "message": "no route"}}),
        json!({"error": {"code": gone.code, "message": gone.message}}),
    ] {
        let o = succ_check(&m.w, "synthetic.metrics", &answer, &sopts(vec![], None)).await;
        assert_eq!(o.code(), 3, "{}", o.report());
        assert!(succ_undecided(o).contains("not a successor answer"));
    }
    // A pending answer decides nothing either.
    let p = a_move().await;
    let a =
        p.w.store
            .successor_answer("example.publisher", "synthetic.metrics")
            .await
            .expect("answer");
    assert_eq!(a["end"]["status"], json!("pending"));
    assert_eq!(
        succ_check(&p.w, "synthetic.metrics", &a, &sopts(vec![], None))
            .await
            .code(),
        3
    );
    for d in [&m.w.dir, &p.w.dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The node's read routes and peer routes together: what a history check
/// with a successor reads.
fn reader_get(w: &World) -> RouterGet {
    RouterGet(routes(w.store.clone()).merge(peer_routes(w.store.clone())))
}

/// A history to follow: `synthetic.metrics` (rows checked in catalog 0)
/// moves to `synthetic.new` in catalog 1, whose links version also holds a
/// row, written early, that moves `synthetic.new` on to `synthetic.newer`;
/// catalog 2 removes `synthetic.new`. The successor answer at catalog 1,
/// and the catalogs with their reports.
async fn a_followed_history() -> (
    World,
    onyx_bulk::wire::Checked,
    Value,
    Vec<(Catalog, SignedReport)>,
) {
    let w = world();
    let files = shard_files(0..30, 10);
    let old = manifest(&files, None, T0);
    let new = manifest_of("synthetic.new", &shard_files(0..10, 10), T0 + 1);
    let newer = manifest_of("synthetic.newer", &shard_files(0..10, 10), T0 + 2);
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![
            CatalogLeaf::of(&old.header),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&newer.header),
        ],
    );
    let r0 = sign_catalog(&c0);
    w.take(&c0).await;
    w.post_in(&old, 0).await.expect("old");
    w.upload_all(&old, &files).await;
    let ck = checked_rows(&w).await;
    let (links, file) = links_version(
        &[
            link_row(
                1,
                "synthetic.metrics",
                2,
                &old.header.root(),
                Some("synthetic.new"),
            ),
            link_row(
                2,
                "synthetic.new",
                2,
                &new.header.root(),
                Some("synthetic.newer"),
            ),
        ],
        None,
        T0 + 15,
    );
    let c1 = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&old.header).tombstone().expect("t"),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&newer.header),
            CatalogLeaf::of(&links.header),
        ],
    );
    let r1 = sign_catalog(&c1);
    w.take(&c1).await;
    w.post_in(&links, 1).await.expect("links manifest");
    w.put_in(LINKS, &links.shards[0].root, file).await;
    let a1 = answer_of(&w, "synthetic.metrics").await;
    let mut leaves = c1.leaves.clone();
    let i = c1.find("synthetic.new").expect("listed");
    leaves[i] = leaves[i].tombstone().expect("t");
    let c2 = catalog_of(Some(&c1), T0 + 30, leaves);
    let r2 = sign_catalog(&c2);
    w.take(&c2).await;
    w.settle().await;
    (w, ck, a1, vec![(c0, r0), (c1, r1), (c2, r2)])
}

/// ADR-0059 amendment 6 item 7 (`--history`): a removed dataset's report
/// gains `successor`, checked at the walk's block; the exit stays 4. Each
/// hop names the catalog that first listed its row and its tombstone, and a
/// row written before its tombstone is marked `early`.
#[tokio::test]
async fn cat_succ_the_history_check_follows_a_link_and_still_reports_removed() {
    let (w, ck, _, _) = a_followed_history().await;
    let get = reader_get(&w);
    let o = history_with(&w, &ck, &get).await;
    assert_eq!(o.code(), 4, "{}", o.report());
    let v = o.report();
    assert_eq!(v["removed"]["catalog"], json!(1));
    let s = &v["successor"];
    assert_eq!(
        s["end"],
        json!({"status": "live", "dataset": "synthetic.newer", "version": 1})
    );
    assert_eq!(s["catalog"]["number"], json!(2));
    let hop = |i: usize, k: &str| s["hops"][i][k].clone();
    assert_eq!(
        (
            hop(0, "to"),
            hop(0, "firstCatalog"),
            hop(0, "tombstoneCatalog"),
            hop(0, "early")
        ),
        (json!("synthetic.new"), json!(1), json!(1), json!(false))
    );
    assert_eq!(
        (
            hop(1, "to"),
            hop(1, "firstCatalog"),
            hop(1, "tombstoneCatalog"),
            hop(1, "early")
        ),
        (json!("synthetic.newer"), json!(1), json!(2), json!(true))
    );
    // An old node (no successor route): the removal stands, with a note.
    let old = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| u.ends_with("/successor").then(|| (404, b"{}".to_vec())),
    };
    let o = history_with(&w, &ck, &old).await;
    assert_eq!(o.code(), 4, "{}", o.report());
    assert_eq!(o.report()["successor"]["status"], json!("unavailable"));
    // A successor answer whose proofs fail: the node's.
    let mut bad = w
        .store
        .successor_answer("example.publisher", "synthetic.metrics")
        .await
        .expect("answer");
    bad["end"]["status"] = json!("removed");
    let bad = serde_json::to_vec(&bad).expect("json");
    let lying = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| u.ends_with("/successor").then(|| (200, bad.clone())),
    };
    assert_eq!(refused(history_with(&w, &ck, &lying).await).0, "node");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review SUCC-R2 L3: the successor answer's catalog must be the walk's
/// newest. A newer one extends the walk once; one the walk cannot reach,
/// or an older one, leaves the check undecided (exit 3).
#[tokio::test]
async fn cat_succ_the_history_check_needs_the_successor_catalog_in_its_walk() {
    let (w, ck, a1, cs) = a_followed_history().await;
    // The walk stops at catalog 1; the answer is from catalog 2: extended.
    let first = files_of(&cs[..2], 1);
    let short = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| u.ends_with("files?from=0").then(|| (200, first.clone())),
    };
    let o = history_with(&w, &ck, &short).await;
    assert_eq!(o.code(), 4, "{}", o.report());
    assert_eq!(o.report()["newest"], json!(2));
    assert_eq!(o.report()["successor"]["catalog"]["number"], json!(2));
    // The walk cannot reach catalog 2: undecided.
    let gone = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| {
            if u.ends_with("files?from=0") {
                Some((200, first.clone()))
            } else {
                u.ends_with("files?from=2").then(|| (404, b"{}".to_vec()))
            }
        },
    };
    let why = undecided(history_with(&w, &ck, &gone).await);
    assert!(why.contains("catalog 2"), "{why}");
    // An answer from catalog 1 while the walk reached catalog 2: undecided.
    let a1 = serde_json::to_vec(&a1).expect("json");
    let stale = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| u.ends_with("/successor").then(|| (200, a1.clone())),
    };
    let why = undecided(history_with(&w, &ck, &stale).await);
    assert!(why.contains("older"), "{why}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

// --- Build review SUCC-B7 (THREATS SUCC-B7) ---

/// SUCC-B7 M1: a history node that answers the walk's extension (`files?from=2`,
/// toward the successor answer's catalog 2) with a catalog numbered past it
/// moves the walk nowhere. Asked once, it is the node's fault; before the
/// fix the check asked again without end.
#[tokio::test]
async fn succ_b7_a_files_answer_past_the_successor_catalog_is_the_nodes_fault() {
    let (w, ck, _, cs) = a_followed_history().await;
    let first = files_of(&cs[..2], 1);
    let (c2, _) = &cs[2];
    let c3 = catalog_of(Some(c2), T0 + 40, c2.leaves.clone());
    let r3 = sign_catalog(&c3);
    let past = files_of(&[(c3, r3)], 3);
    let asked = AtomicU64::new(0);
    let hostile = HookGet {
        inner: reader_get(&w),
        hook: |u: &str| {
            if u.ends_with("files?from=0") {
                Some((200, first.clone()))
            } else if u.ends_with("files?from=2") {
                // Cut the loop so a regression fails instead of hanging.
                if asked.fetch_add(1, Ordering::SeqCst) >= 100 {
                    Some((500, b"{}".to_vec()))
                } else {
                    Some((200, past.clone()))
                }
            } else {
                None
            }
        },
    };
    let (fault, why) = refused(history_with(&w, &ck, &hostile).await);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "{why}");
    assert_eq!(fault, "node", "{why}");
    assert!(why.contains("from 2"), "{why}");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// SUCC-B7 M2: a second live catalog of the answer's number under another
/// key of the publisher the check knows (`--keys`, a scan) is the
/// publisher's fault, as `--history` rules; before the fix only the next
/// number was read and the check exited 0 on the branch the node served.
#[tokio::test]
async fn succ_b7_a_second_catalog_of_the_same_number_under_a_known_key_is_refused() {
    let m = a_held_move().await;
    let a = answer_of(&m.w, "synthetic.metrics").await;
    // The publisher's other key signs another catalog 1, with no move.
    let c1b = catalog_of(Some(&m.c0), T0 + 21, m.c0.leaves.clone());
    let r1b = cat::sign(&SECRET2, &c1b.header).expect("ok");
    m.w.node.anchor_by(&r1b, &SECRET2);
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    let (fault, why) =
        succ_refused(succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![signer2], None)).await);
    assert_eq!(fault, "publisher", "{why}");
    assert!(
        why.contains("catalog 1") && why.contains("two live reports"),
        "{why}"
    );
    // Disputed, nothing is decided; withdrawn for good, it no longer counts.
    m.w.node.dispute(&r1b);
    let why = succ_undecided(
        succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![signer2], None)).await,
    );
    assert!(why.contains("catalog 1"), "{why}");
    m.w.node.withdraw(&r1b);
    let o = succ_check(&m.w, "synthetic.metrics", &a, &sopts(vec![signer2], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(o.report()["keys"].as_array().map(Vec::len), Some(2));
    let _ = std::fs::remove_dir_all(&m.w.dir);
}

/// SUCC-B7 L2: a part under the catalog's signature that breaks its own
/// rules is the publisher's fault only when that catalog's report reads
/// anchored at the caller's block; any key can sign a catalog naming a
/// publisher, so otherwise nothing is decided.
#[tokio::test]
async fn succ_b7_a_signed_fault_is_the_publishers_only_once_its_catalog_reads_anchored() {
    use crate::bulk_history::Stop;
    use crate::bulk_successor::{settle, Off};
    let m = a_held_move().await;
    let signed = |secret: &[u8; 32]| Off::Signed {
        why: "the signed links version breaks its own header".into(),
        report: Box::new(cat::sign(secret, &m.c1.header).expect("ok")),
        signer: onyx_bulk::anchor::address_of(secret).expect("ok"),
        number: 1,
    };
    let at = decision_block(m.w.node.as_ref(), CHAIN)
        .await
        .expect("block");
    let verdict = |off| settle(m.w.node.as_ref(), CHAIN, &BOOK, at, off);
    // The catalog the book holds: the publisher's.
    assert!(matches!(
        verdict(signed(&SECRET)).await,
        Stop::Refused("publisher", _)
    ));
    // The same catalog signed by a key the book never saw: undecided.
    match verdict(signed(&[0x77; 32])).await {
        Stop::Undecided(why) => assert!(why.contains("not known to be the publisher's"), "{why}"),
        Stop::Refused(f, why) => panic!("refused ({f}): {why}"),
    }
    // Off chain only (`--unchecked`), a signed fault never names the publisher.
    let _ = std::fs::remove_dir_all(&m.w.dir);
}

/// THREATS CAT-SUCC-8 and CAT-SUCC-1 (review SUCC-B7 L4): the reader's check
/// around a set-aside and a key rotation. A thief's catalog links the old id
/// to the thief's dataset; once the book withdraws it, the answer taken from
/// it is undecided, and its links version, set aside, is no longer served
/// (undecided, not the node's fault). The publisher's replacement under its
/// next key checks to the publisher's own target, with the old key named.
#[tokio::test]
async fn succ_b7_the_reader_follows_a_set_aside_and_a_key_rotation() {
    let (w, old, new, c0, _c1, r1, thiefs) = a_thiefs_move("synthetic.thief").await;
    let stolen = answer_of(&w, "synthetic.metrics").await;
    let o = succ_check(&w, "synthetic.metrics", &stolen, &sopts(vec![], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    assert_eq!(o.report()["end"]["dataset"], json!("synthetic.thief"));
    let (own, own_file) = links_version(&[move_row(1, &old, "synthetic.new")], None, T0 + 45);
    w.replace_1(
        &c0,
        &r1,
        vec![
            CatalogLeaf::of(&old.header).tombstone().expect("t"),
            CatalogLeaf::of(&new.header),
            CatalogLeaf::of(&own.header),
        ],
    )
    .await;
    // The thief's answer after the book withdrew its catalog.
    let why =
        succ_undecided(succ_check(&w, "synthetic.metrics", &stolen, &sopts(vec![], None)).await);
    assert!(why.contains("withdrew"), "{why}");
    // Its links version is set aside: a page of it is not served.
    let e = w
        .store
        .rows(
            "example.publisher",
            LINKS,
            &parse_rows_query(Some(&format!("root={}&page=1", thiefs.header.root()))).expect("q"),
        )
        .await
        .expect_err("set aside");
    assert_eq!(e.status, StatusCode::NOT_FOUND);
    // The replacement, signed by the publisher's next key.
    w.post_in(&own, 1).await.expect("own links");
    w.put_in(LINKS, &own.shards[0].root, own_file).await;
    let a = answer_of(&w, "synthetic.metrics").await;
    let signer1 = onyx_bulk::anchor::address_of(&SECRET).expect("ok");
    let signer2 = onyx_bulk::anchor::address_of(&SECRET2).expect("ok");
    let o = succ_check(&w, "synthetic.metrics", &a, &sopts(vec![signer1], None)).await;
    assert_eq!(o.code(), 0, "{}", o.report());
    let v = o.report();
    assert_eq!(v["end"]["dataset"], json!("synthetic.new"));
    assert_eq!(v["signer"], json!(hex0x(&signer2)));
    assert_eq!(v["keys"].as_array().map(Vec::len), Some(2));
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review NEWC-R1 (its property test): for every range, limit and page, the
/// newest-first walk is the oldest-first walk reversed, and every page passes
/// the check of the whole request (order and page asked).
#[tokio::test]
async fn newest_first_pages_equal_the_reversed_oldest_first_ones_for_every_range() {
    let w = world();
    published(&w).await;
    for from in -1i128..=31 {
        for to in from..=31 {
            for limit in [1u64, 2, 3, 7, 10, 11, 30, 31] {
                let mut walk = Vec::new();
                for desc in [false, true] {
                    let mut keys = Vec::new();
                    let mut page = 0u64;
                    loop {
                        let o = if desc { "desc" } else { "asc" };
                        let a = w
                            .rows(&format!(
                                "from={from}&to={to}&limit={limit}&page={page}&order={o}"
                            ))
                            .await
                            .expect("ok");
                        let asked = onyx_bulk::wire::RowsAsked {
                            from: Some(from),
                            to: Some(to),
                            desc,
                            page,
                        };
                        let c = onyx_bulk::wire::check_rows_asked(&a, &asked).unwrap_or_else(|e| {
                            panic!("check {from} {to} {limit} {page} {o}: {e:?}")
                        });
                        let n = c.rows.len();
                        assert_eq!(c.descending, desc, "{from} {to} {limit} {page}");
                        keys.extend(c.rows.iter().map(|r| r.2));
                        if a["next_page"].is_null() {
                            break;
                        }
                        assert!(
                            n > 0,
                            "empty page with a next: {from} {to} {limit} {page} {o}"
                        );
                        page += 1;
                        assert!(page < 100);
                    }
                    walk.push(keys);
                }
                let want: Vec<i128> = (from.max(0)..=to.min(29)).collect();
                assert_eq!(walk[0], want, "asc {from} {to} {limit}");
                walk[1].reverse();
                assert_eq!(walk[1], want, "desc {from} {to} {limit}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

// ---- Limits per publisher (ADR-0066 item 10; THREATS PUBON-6, PUBON-18) ----

/// A limits file's entries: `(id, bytes, datasets, paused)`.
fn listed(entries: &[(&str, u64, usize, bool)]) -> PublisherLimits {
    PublisherLimits {
        reserve_bytes: 0,
        grants: entries
            .iter()
            .map(|(id, quota_bytes, max_datasets, paused)| {
                (
                    (*id).to_string(),
                    PublisherGrant {
                        quota_bytes: *quota_bytes,
                        max_datasets: *max_datasets,
                        paused: *paused,
                    },
                )
            })
            .collect(),
    }
}

/// A world whose node has a limits file.
fn world_listed(entries: &[(&str, u64, usize, bool)]) -> World {
    let w = world();
    let dir = w.dir.clone();
    drop(w.store);
    relisted(w.node, w.clock, dir, entries)
}

/// The node of a world started again on the same directory with `entries`
/// as its limits file (the file is read at start).
fn relisted(
    node: Arc<Node>,
    clock: Arc<AtomicU64>,
    dir: PathBuf,
    entries: &[(&str, u64, usize, bool)],
) -> World {
    let c = clock.clone();
    let store = BulkStore::open(
        pin(),
        node.clone(),
        Arc::new(move || c.load(Ordering::Relaxed)),
        &dir,
        &dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("ok")
    .with_free_per_minute(10_000)
    .with_publishers(listed(entries))
    .expect("the limits fit what is held");
    World {
        node,
        store: started(store),
        clock,
        dir,
    }
}

impl World {
    /// This world's node restarted with `entries` as its limits file.
    fn restarted_with(self, entries: &[(&str, u64, usize, bool)]) -> World {
        let World {
            node,
            store,
            clock,
            dir,
        } = self;
        drop(store);
        relisted(node, clock, dir, entries)
    }
}

/// Version 1 of `dataset` by `publisher`.
fn manifest_by(publisher: &str, dataset: &str, files: &[Vec<u8>]) -> Manifest {
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| ShardEntry::new(i as u32, &check_shard(&schema(), f).expect("ok").summary))
        .collect();
    Manifest::build(
        NewVersion {
            dataset: dataset.into(),
            publisher: SourceId(publisher.into()),
            created_unix: T0,
            schema: schema(),
            rights: DataRights {
                source: SourceId(publisher.into()),
                ..rights()
            },
        },
        None,
        entries,
    )
    .expect("ok")
}

const ME: &str = "example.publisher";
const OTHER: &str = "second.publisher";

/// ADR-0066 item 10 (THREATS PUBON-6): with a limits file a publisher has
/// its entry's bytes and datasets, not the node's one quota, and a
/// registered publisher without an entry is refused before any chain read.
#[tokio::test]
async fn a_publisher_gets_its_entry_and_one_without_an_entry_is_refused() {
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let bytes = m.header.byte_count;
    // The node's own quota (16 GiB, 64 datasets) would take everything below.
    let w = world_listed(&[(ME, bytes, 1, false)]);
    let r = sign(&m);
    w.node.anchor(&r);
    assert_eq!(w.post(&m, &r).await.expect("ok").0, StatusCode::CREATED);
    w.upload_all(&m, &files).await;
    w.rows("from=0&to=3").await.expect("served");
    // Its entry's bytes: a second version over them is refused.
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 5);
    let r2 = sign(&v2);
    w.node.anchor(&r2);
    let e = w.post(&v2, &r2).await.expect_err("over its entry");
    assert_eq!(e.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(e.code, "bulk_quota_exceeded");
    // Its entry's datasets: a second dataset and a catalog of two leaves.
    let d2 = manifest_by(ME, "synthetic.second", &shard_files(0..1, 1));
    let rd = sign(&d2);
    w.node.anchor(&rd);
    assert_eq!(
        code(w.post(&d2, &rd).await.expect_err("one dataset")),
        "bulk_quota_exceeded"
    );
    let two = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), other_leaf("synthetic.other")],
    );
    let rc = sign_catalog(&two);
    w.node.anchor(&rc);
    assert_eq!(
        code(w.post_catalog(&two, &rc).await.expect_err("two leaves")),
        "bulk_quota_exceeded"
    );
    // A registered publisher the file does not list: refused, no chain read.
    let om = manifest_by(OTHER, "synthetic.metrics", &files);
    let or = sign(&om);
    w.node.anchor(&or);
    let reads = w.node.reads();
    let e = w.post(&om, &or).await.expect_err("not listed");
    assert_eq!(e.status, StatusCode::FORBIDDEN);
    assert_eq!(e.code, "bulk_publisher_not_listed");
    let oc = Catalog::build(
        SourceId(OTHER.into()),
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&om.header)],
    )
    .expect("catalog");
    let ocr = sign_catalog(&oc);
    w.node.anchor(&ocr);
    assert_eq!(
        code(w.post_catalog(&oc, &ocr).await.expect_err("not listed")),
        "bulk_publisher_not_listed"
    );
    assert_eq!(w.node.reads(), reads, "refused before the chain is read");
    // Through the upload route.
    let app = upload_routes(w.store.clone());
    let body = json!({
        "manifest": hex0x(&om.to_bytes()),
        "report": hex0x(&borsh::to_vec(&or).expect("ok")),
    });
    let res = app
        .oneshot(
            Request::post("/v1/bulk/manifests")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let b = res.into_body().collect().await.expect("ok").to_bytes();
    let v: Value = serde_json::from_slice(&b).expect("json");
    assert_eq!(v["error"]["code"], "bulk_publisher_not_listed");
    // The listed publisher is untouched.
    w.rows("from=0&to=3").await.expect("served");
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS PUBON-6: a publisher at its own grant is refused alone; another
/// publisher's upload is taken as before (the node's total has room, since
/// the grants are summed at start).
#[tokio::test]
async fn a_publisher_at_its_quota_does_not_affect_another() {
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let bytes = m.header.byte_count;
    let w = world_listed(&[(ME, bytes, 4, false), (OTHER, 2 * bytes, 4, false)]);
    let r = sign(&m);
    w.node.anchor(&r);
    assert_eq!(w.post(&m, &r).await.expect("ok").0, StatusCode::CREATED);
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 5);
    let r2 = sign(&v2);
    w.node.anchor(&r2);
    assert_eq!(
        code(w.post(&v2, &r2).await.expect_err("full")),
        "bulk_quota_exceeded"
    );
    // The other publisher still has all of its own grant: two datasets of
    // the size that filled the first one's.
    for d in ["synthetic.metrics", "synthetic.more"] {
        let om = manifest_by(OTHER, d, &files);
        let or = sign(&om);
        w.node.anchor(&or);
        assert_eq!(
            w.post(&om, &or).await.expect("other").0,
            StatusCode::CREATED,
            "{d}"
        );
    }
    let third = manifest_by(OTHER, "synthetic.third", &files);
    let rt = sign(&third);
    w.node.anchor(&rt);
    assert_eq!(
        code(w.post(&third, &rt).await.expect_err("its own grant")),
        "bulk_quota_exceeded"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Without a limits file nothing changes: every registered publisher has
/// the node's one quota and cap, and none needs an entry.
#[tokio::test]
async fn without_a_limits_file_every_publisher_has_the_nodes_one_quota() {
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let bytes = m.header.byte_count;
    let w = world_with(BulkLimits {
        quota_bytes: bytes,
        ..BulkLimits::default()
    });
    for p in [ME, OTHER] {
        let m = manifest_by(p, "synthetic.metrics", &files);
        let r = sign(&m);
        w.node.anchor(&r);
        assert_eq!(w.post(&m, &r).await.expect(p).0, StatusCode::CREATED);
        let over = manifest_by(p, "synthetic.more", &files);
        let ro = sign(&over);
        w.node.anchor(&ro);
        assert_eq!(
            code(w.post(&over, &ro).await.expect_err("the one quota")),
            "bulk_quota_exceeded"
        );
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// ADR-0066 item 10: a paused publisher keeps being served and takes no
/// manifest, no shard and no catalog that brings data, each refused before
/// a chain read.
#[tokio::test]
async fn a_paused_publisher_is_served_and_takes_no_upload() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let (m, _files) = published(&w).await;
    // A second dataset whose shard is still missing when the pause starts
    // (other keys: a shard is stored by its root).
    let f2 = shard_files(100..110, 10);
    let d2 = manifest_by(ME, "synthetic.second", &f2);
    let rd = sign(&d2);
    w.node.anchor(&rd);
    assert_eq!(w.post(&d2, &rd).await.expect("ok").0, StatusCode::CREATED);
    let w = w.restarted_with(&[(ME, 1 << 30, 16, true)]);
    // Served as before.
    let rows = w.rows("from=0&to=3").await.expect("served");
    assert_eq!(rows["rows"].as_array().expect("rows").len(), 4);
    w.store
        .info_checked(ME, "synthetic.metrics")
        .await
        .expect("info");
    let reads = w.node.reads();
    let paused = |e: ApiError| {
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert_eq!(e.code, "bulk_publisher_paused");
    };
    // No manifest: a next version, a new dataset.
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 5);
    let r2 = sign(&v2);
    w.node.anchor(&r2);
    paused(w.post(&v2, &r2).await.expect_err("a version"));
    let d3 = manifest_by(ME, "synthetic.third", &f2);
    let r3 = sign(&d3);
    w.node.anchor(&r3);
    paused(w.post(&d3, &r3).await.expect_err("a dataset"));
    // No shard, also of a manifest held before the pause.
    paused(
        w.store
            .accept_shard(
                ME,
                "synthetic.second",
                &d2.shards[0].root,
                &HeaderMap::new(),
                Body::from(f2[0].clone()),
            )
            .await
            .expect_err("a shard"),
    );
    assert!(!w.store.holds_shard(&d2.shards[0].root));
    // No catalog that lists data (a first catalog: every leaf is new).
    let c0 = catalog_of(None, T0 + 10, vec![CatalogLeaf::of(&m.header)]);
    let rc = sign_catalog(&c0);
    w.node.anchor(&rc);
    paused(w.post_catalog(&c0, &rc).await.expect_err("a catalog"));
    paused(w.post_in(&v2, 0).await.expect_err("through a catalog"));
    assert_eq!(w.node.reads(), reads, "refused before the chain is read");
    // Opened again, the same uploads are taken.
    let w = w.restarted_with(&[(ME, 1 << 30, 16, false)]);
    assert_eq!(w.post(&v2, &r2).await.expect("open").0, StatusCode::CREATED);
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS PUBON-18 (review PUBON-R1 M6): a paused publisher can take data
/// down. A catalog in which every leaf is unchanged or a tombstone is taken;
/// one that changes or adds a dataset is not.
#[tokio::test]
async fn a_paused_publisher_may_still_post_a_tombstone_catalog() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let keep = other_leaf("synthetic.other");
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), keep.clone()],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    w.upload_all(&m, &files).await;
    let w = w.restarted_with(&[(ME, 1 << 30, 16, true)]);
    w.rows("from=0&to=3").await.expect("served while paused");
    let paused = |e: ApiError| assert_eq!(e.code, "bulk_publisher_paused");
    // The held catalog sent again changes nothing: answered as ever.
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("same").0,
        StatusCode::OK
    );
    // A catalog that adds a dataset, or moves one to a next version: refused.
    let adds = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&m.header),
            keep.clone(),
            other_leaf("synthetic.new"),
        ],
    );
    let ra = sign_catalog(&adds);
    w.node.anchor(&ra);
    paused(w.post_catalog(&adds, &ra).await.expect_err("adds"));
    let v2 = manifest(&shard_files(30..40, 10), Some(&m), T0 + 5);
    let moves = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![CatalogLeaf::of(&v2.header), keep.clone()],
    );
    let rm = sign_catalog(&moves);
    w.node.anchor(&rm);
    paused(w.post_catalog(&moves, &rm).await.expect_err("a version"));
    // A tombstone with data beside it: refused whole.
    let gone = CatalogLeaf::of(&m.header).tombstone().expect("tombstone");
    let mixed = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![gone.clone(), keep.clone(), other_leaf("synthetic.new")],
    );
    let rx = sign_catalog(&mixed);
    w.node.anchor(&rx);
    paused(w.post_catalog(&mixed, &rx).await.expect_err("mixed"));
    assert_eq!(
        w.store.removed(ME, "synthetic.metrics").expect("read"),
        None
    );
    // The tombstone alone, the other leaf unchanged: taken.
    let c1 = catalog_of(Some(&c0), T0 + 20, vec![gone, keep]);
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1).await.expect("the takedown").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store.removed(ME, "synthetic.metrics").expect("read"),
        Some(1)
    );
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("removed")),
        "bulk_dataset_removed"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// THREATS PUBON-18: a paused publisher that never posted a catalog removes
/// a dataset it published under its own reports: a first catalog of
/// tombstones only is taken.
#[tokio::test]
async fn a_paused_publisher_removes_an_own_report_dataset_with_a_first_catalog() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let (m1, _m2) = own_two(&w).await;
    let w = w.restarted_with(&[(ME, 1 << 30, 16, true)]);
    let t = CatalogLeaf::of(&m1.header).tombstone().expect("t");
    let c0 = catalog_of(None, T0 + 10, vec![t]);
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    assert_eq!(
        w.post_catalog(&c0, &r0).await.expect("taken").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store.removed(ME, "synthetic.metrics").expect("read"),
        Some(0)
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// The limits file's format (ADR-0066 item 10): every field required, no
/// other taken, one entry per id, numbers in range, and the grants plus the
/// reserve within the node's total.
#[test]
fn a_limits_file_with_unknown_fields_duplicates_or_bad_numbers_is_refused() {
    let entry = |id: &str, q: &str, d: &str, s: &str| {
        format!(r#"{{"id":"{id}","quota_gib":{q},"datasets":{d},"state":"{s}"}}"#)
    };
    let file = |reserve: &str, entries: &[String]| {
        format!(
            r#"{{"reserve_gib":{reserve},"publishers":[{}]}}"#,
            entries.join(",")
        )
    };
    let good = file(
        "2",
        &[
            entry("first.publisher", "16", "50000", "open"),
            entry("second.publisher", "1", "16", "paused"),
        ],
    );
    let l = PublisherLimits::parse(&good).expect("good");
    assert_eq!(l.reserve_bytes, 2 << 30);
    assert_eq!(
        l.grants.get("first.publisher"),
        Some(&PublisherGrant {
            quota_bytes: 16 << 30,
            max_datasets: 50_000,
            paused: false
        })
    );
    assert_eq!(
        l.grants.get("second.publisher"),
        Some(&PublisherGrant {
            quota_bytes: 1 << 30,
            max_datasets: 16,
            paused: true
        })
    );
    // 16 + 1 + 2 GiB: fits 19, not 18 (thirteen grants of 1 GiB on a node
    // of 24 with 16 held: review PUBON-R1 M5).
    l.fits(19 << 30).expect("fits");
    let e = l.fits(18 << 30).expect_err("over");
    assert!(
        e.contains("exceed") && e.contains("ONYX_BULK_MAX_GIB"),
        "{e}"
    );
    let many: Vec<String> = std::iter::once(entry("first.publisher", "16", "50000", "open"))
        .chain((0..13).map(|i| entry(&format!("small-{i}.publisher"), "1", "16", "open")))
        .collect();
    assert!(PublisherLimits::parse(&file("0", &many))
        .expect("parsed")
        .fits(24 << 30)
        .is_err());
    // An empty list is a node that takes no upload.
    assert!(PublisherLimits::parse(&file("0", &[]))
        .expect("empty")
        .grants
        .is_empty());
    let one = |q: &str, d: &str, s: &str| file("0", &[entry("a.publisher", q, d, s)]);
    let bad: Vec<(String, &str)> = vec![
        ("".into(), "not valid"),
        ("[]".into(), "not valid"),
        (r#"{"publishers":[]}"#.into(), "reserve_gib"),
        (r#"{"reserve_gib":1}"#.into(), "publishers"),
        (
            r#"{"reserve_gib":1,"publishers":[],"note":"x"}"#.into(),
            "unknown field",
        ),
        (
            r#"{"reserve_gib":1,"reserve_gib":2,"publishers":[]}"#.into(),
            "duplicate field",
        ),
        (
            r#"{"reserve_gib":0,"publishers":[{"id":"a.publisher","quota_gib":1,"datasets":1,"state":"open","contact":"x"}]}"#.into(),
            "unknown field",
        ),
        (
            r#"{"reserve_gib":0,"publishers":[{"id":"a.publisher","quota_gib":1,"datasets":1}]}"#.into(),
            "state",
        ),
        (
            file(
                "0",
                &[
                    entry("a.publisher", "1", "1", "open"),
                    entry("a.publisher", "2", "1", "open"),
                ],
            ),
            "two entries",
        ),
        (one("0", "1", "open"), "quota_gib must be"),
        (one("1048577", "1", "open"), "quota_gib must be"),
        (one("18446744073709551615", "1", "open"), "quota_gib must be"),
        (one("99999999999999999999999", "1", "open"), "not valid"),
        (one("-1", "1", "open"), "not valid"),
        (one("1.5", "1", "open"), "not valid"),
        (one("\"1\"", "1", "open"), "not valid"),
        (one("1", "0", "open"), "datasets must be"),
        (one("1", "50001", "open"), "datasets must be"),
        (one("1", "1", "closed"), "state must be open or paused"),
        (one("1", "1", "Paused"), "state must be open or paused"),
        (
            file("1048577", &[]),
            "reserve_gib must be",
        ),
        (
            file("0", &[entry("Not An Id", "1", "1", "open")]),
            "not a publisher id",
        ),
        (file("0", &[entry("", "1", "1", "open")]), "not a publisher id"),
    ];
    for (text, want) in bad {
        let e = PublisherLimits::parse(&text).expect_err(&text);
        assert!(e.contains(want), "{text}: {e}");
    }
    let huge = format!("{}{}", good, " ".repeat(MAX_PUBLISHERS_FILE_BYTES));
    assert!(PublisherLimits::parse(&huge)
        .expect_err("too long")
        .contains("over"));
}

// ---- Review LIM-R1 of the limits per publisher ----

/// Review LIM-R1 M1: a takedown is not held to the publisher's own limits.
/// A grant lowered below what is held, or a dataset cap below the
/// catalog's leaves, refuses a catalog that adds data and takes one that
/// only removes a dataset, open or paused.
#[tokio::test]
async fn a_takedown_is_taken_over_a_lowered_grant_and_a_small_dataset_cap() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let files = shard_files(0..30, 10);
    let m = manifest(&files, None, T0);
    let keep = other_leaf("synthetic.other");
    let c0 = catalog_of(
        None,
        T0 + 10,
        vec![CatalogLeaf::of(&m.header), keep.clone()],
    );
    let r0 = sign_catalog(&c0);
    w.node.anchor(&r0);
    w.post_catalog(&c0, &r0).await.expect("c0");
    w.post_in(&m, 0).await.expect("v1");
    w.upload_all(&m, &files).await;
    // One byte and one dataset: far below the two leaves and the bytes held.
    let w = w.restarted_with(&[(ME, 1, 1, false)]);
    let adds = catalog_of(
        Some(&c0),
        T0 + 20,
        vec![
            CatalogLeaf::of(&m.header),
            keep.clone(),
            other_leaf("synthetic.new"),
        ],
    );
    let ra = sign_catalog(&adds);
    w.node.anchor(&ra);
    assert_eq!(
        code(w.post_catalog(&adds, &ra).await.expect_err("adds")),
        "bulk_quota_exceeded"
    );
    let gone = CatalogLeaf::of(&m.header).tombstone().expect("tombstone");
    let c1 = catalog_of(Some(&c0), T0 + 20, vec![gone.clone(), keep.clone()]);
    let r1 = sign_catalog(&c1);
    w.node.anchor(&r1);
    assert_eq!(
        w.post_catalog(&c1, &r1).await.expect("the takedown").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store.removed(ME, "synthetic.metrics").expect("read"),
        Some(1)
    );
    // Paused as well, the second dataset goes the same way.
    let w = w.restarted_with(&[(ME, 1, 1, true)]);
    let c2 = catalog_of(
        Some(&c1),
        T0 + 30,
        vec![gone, keep.tombstone().expect("tombstone")],
    );
    let r2 = sign_catalog(&c2);
    w.node.anchor(&r2);
    assert_eq!(
        w.post_catalog(&c2, &r2).await.expect("paused takedown").0,
        StatusCode::CREATED
    );
    assert_eq!(
        w.store.removed(ME, "synthetic.other").expect("read"),
        Some(2)
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review LIM-R1 M2: the grants alone do not bound the node. A publisher
/// counts with the larger of its grant and what it holds, one without an
/// entry with what it holds; a file that does not fit beside that stops
/// the start and names the publishers, and what fits is said in the log.
#[tokio::test]
async fn what_a_node_holds_must_fit_beside_the_grants_at_start() {
    let w = world();
    let files = shard_files(0..30, 10);
    let mut b = 0;
    for p in [ME, OTHER] {
        let m = manifest_by(p, "synthetic.metrics", &files);
        b = m.header.byte_count;
        let r = sign(&m);
        w.node.anchor(&r);
        assert_eq!(w.post(&m, &r).await.expect(p).0, StatusCode::CREATED);
    }
    let World {
        node,
        store,
        clock,
        dir,
    } = w;
    drop(store);
    let open = |total: u64, entries: &[(&str, u64, usize, bool)]| {
        let c = clock.clone();
        BulkStore::open(
            pin(),
            node.clone(),
            Arc::new(move || c.load(Ordering::Relaxed)),
            &dir,
            &dir.join("warm"),
            BulkLimits {
                max_total_bytes: total,
                ..BulkLimits::default()
            },
        )
        .expect("ok")
        .with_publishers(listed(entries))
    };
    // A grant lowered to one byte and a publisher without an entry: what
    // both hold fits the total exactly, and the start says who holds what.
    let lowered: &[(&str, u64, usize, bool)] = &[(ME, 1, 4, false)];
    assert!(
        listed(lowered).fits(2 * b - 1).is_ok(),
        "the grants alone fit"
    );
    let s = open(2 * b, lowered).expect("fits");
    let notes = s.publisher_notes().join("\n");
    assert!(
        notes.contains(&format!(
            "publisher {ME} holds {b} bytes, over its grant of 1"
        )),
        "{notes}"
    );
    assert!(
        notes.contains(&format!(
            "publisher {OTHER} holds {b} bytes and has no entry"
        )),
        "{notes}"
    );
    drop(s);
    // One byte less: refused, naming both.
    let e = open(2 * b - 1, lowered).err().expect("does not fit");
    assert!(
        e.contains(&format!("{ME} holds {b} bytes over its grant of 1"))
            && e.contains(&format!("{OTHER} holds {b} bytes and has no entry"))
            && e.contains("the node's total"),
        "{e}"
    );
    // A grant larger than what is held counts whole: the unused part is
    // promised too.
    let roomy: &[(&str, u64, usize, bool)] = &[(ME, 3 * b, 4, false), (OTHER, b, 4, false)];
    assert!(open(4 * b - 1, roomy).is_err());
    let s = open(4 * b, roomy).expect("fits");
    assert!(s.publisher_notes().is_empty());
    drop(s);
    // The reserve counts, and a file that lists no publisher says so.
    let mut with_reserve = listed(roomy);
    with_reserve.reserve_bytes = 1;
    let c = clock.clone();
    let reopened = BulkStore::open(
        pin(),
        node.clone(),
        Arc::new(move || c.load(Ordering::Relaxed)),
        &dir,
        &dir.join("warm"),
        BulkLimits {
            max_total_bytes: 4 * b,
            ..BulkLimits::default()
        },
    )
    .expect("ok");
    assert!(reopened.with_publishers(with_reserve).is_err());
    let s = open(2 * b, &[]).expect("holdings fit");
    assert!(
        s.publisher_notes()
            .iter()
            .any(|n| n.contains("lists no publisher: every bulk upload is refused")),
        "{:?}",
        s.publisher_notes()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review LIM-R1 M3: a pause does not hide an equivocation. A second
/// anchored manifest for a version a paused publisher holds marks the
/// dataset forked, also when its grant is below what it holds.
#[tokio::test]
async fn a_paused_publisher_still_forks_on_a_second_anchored_manifest() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    published(&w).await;
    let w = w.restarted_with(&[(ME, 1, 16, true)]);
    let other = manifest(&shard_files(0..20, 10), None, T0 + 1);
    let ro = sign(&other);
    w.node.anchor(&ro);
    assert_eq!(
        code(w.post(&other, &ro).await.expect_err("a fork")),
        "bulk_dataset_forked"
    );
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("not served")),
        "bulk_dataset_forked"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review LIM-R1 M3 and L2: what a paused publisher holds is answered as
/// for any publisher. The same manifest and a stored shard sent again are
/// 200, and a held manifest signed again by a new key after a compromise is
/// re-anchored, so the dataset keeps being served.
#[tokio::test]
async fn a_paused_publishers_held_manifest_is_reanchored_and_a_resend_is_200() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let (m, files) = published(&w).await;
    let w = w.restarted_with(&[(ME, 1 << 30, 16, true)]);
    let r = sign(&m);
    assert_eq!(
        w.post(&m, &r).await.expect("the same again").0,
        StatusCode::OK
    );
    assert_eq!(
        w.put(&m.shards[0].root, files[0].clone())
            .await
            .expect("a stored shard again")
            .0,
        StatusCode::OK
    );
    w.node.life(&r, 3);
    w.node.withdraw(&r);
    let r2 = onyx_bulk::anchor::sign(&SECRET2, &m.header).expect("ok");
    w.node.anchor_by(&r2, &SECRET2);
    let (s, v) = w.post(&m, &r2).await.expect("re-anchored");
    assert_eq!((s, v["reanchored"].clone()), (StatusCode::OK, json!(true)));
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    let a = w
        .rows("from=0&to=1")
        .await
        .expect("served under the new key");
    let c = onyx_bulk::wire::check_rows_answer(&a, Some(0), Some(1)).expect("ok");
    assert_eq!(
        c.signer,
        onyx_bulk::anchor::address_of(&SECRET2).expect("ok")
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review LIM-R1 M3: a replacement for a withdrawn version is new data. A
/// paused publisher's is refused and nothing is moved aside; the node falls
/// back to the version before, as the book says.
#[tokio::test]
async fn a_paused_publisher_gets_no_replacement_for_a_withdrawn_version() {
    let w = world_listed(&[(ME, 1 << 30, 16, false)]);
    let (m, _) = published(&w).await;
    let thief = manifest(&shard_files(30..40, 10), Some(&m), T0 + 10);
    let rt = sign(&thief);
    w.node.anchor(&rt);
    w.post(&thief, &rt).await.expect("ok");
    let w = w.restarted_with(&[(ME, 1 << 30, 16, true)]);
    w.node.withdraw(&rt);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        w.rows("from=0&to=1").await.expect("ok")["manifest"]["version"],
        json!(1)
    );
    let new_secret = [0x44; 32];
    let honest = manifest(&shard_files(30..50, 10), Some(&m), T0 + 20);
    let rh = onyx_bulk::anchor::sign(&new_secret, &honest.header).expect("ok");
    w.node.anchor_by(&rh, &new_secret);
    assert_eq!(
        code(w.post(&honest, &rh).await.expect_err("new data")),
        "bulk_publisher_paused"
    );
    assert!(!w
        .dir
        .join("manifests/example.publisher/synthetic.metrics/withdrawn")
        .exists());
    // Open again, the replacement is taken.
    let w = w.restarted_with(&[(ME, 1 << 30, 16, false)]);
    assert_eq!(
        w.post(&honest, &rh).await.expect("replaced").0,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// [`theft`] with the node restarted on a limits file.
async fn theft_listed(entries: &[(&str, u64, usize, bool)]) -> Theft {
    let Theft {
        w,
        m,
        c0,
        c1,
        r1,
        bad,
    } = theft().await;
    Theft {
        w: w.restarted_with(entries),
        m,
        c0,
        c1,
        r1,
        bad,
    }
}

/// Review LIM-R1 M3 (THREATS CAT-6 under a pause): a paused publisher's
/// catalog whose number is held, or one that breaks the chain, is read
/// against the book like any other. The same catalog under a new report is
/// 200; two anchored catalogs that cannot both stand mark the fork.
#[tokio::test]
async fn a_paused_publishers_catalogs_still_mark_a_fork() {
    let t = theft_listed(&[(ME, 1 << 30, 16, true)]).await;
    let w = &t.w;
    let (c1b, r1b, _, _) = replacement(&t);
    let r1s = cat::sign(&SECRET2, &t.c1.header).expect("ok");
    w.node.anchor_by(&r1s, &SECRET2);
    assert_eq!(
        w.post_catalog(&t.c1, &r1s).await.expect("same").0,
        StatusCode::OK
    );
    assert!(!fork_file(w).exists());
    // An already-taken number with another root, both anchored.
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    assert!(fork_file(w).exists());
    assert_eq!(
        code(w.rows("from=0&to=3").await.expect_err("forked")),
        "bulk_dataset_forked"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
    // An anchored catalog that is not the successor of the held newest.
    let t = theft_listed(&[(ME, 1 << 30, 16, true)]).await;
    let w = &t.w;
    let leaves = vec![other_leaf("synthetic.thief")];
    let dropped = Catalog {
        header: cat::CatalogHeader {
            format: cat::FORMAT,
            publisher: publisher(),
            number: 2,
            prev: t.c1.header.root(),
            created_unix: T0 + 60,
            leaf_count: 1,
            leaves_root: cat::leaves_tree(&leaves).root(),
        },
        leaves,
    };
    let rd = sign_catalog(&dropped);
    w.node.anchor(&rd);
    assert_eq!(
        code(w.post_catalog(&dropped, &rd).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    assert!(fork_file(w).exists());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review LIM-R1 M3: when the book withdrew a paused publisher's catalogs
/// (a stolen key), they are set aside as the book says, but the replacement
/// that brings data is not stored until the publisher is open again.
#[tokio::test]
async fn a_paused_publishers_withdrawn_catalog_is_set_aside_without_a_replacement() {
    let t = theft_listed(&[(ME, 1 << 30, 16, true)]).await;
    let w = &t.w;
    let (c1b, r1b, _, _) = replacement(&t);
    w.node.withdraw(&t.r1);
    w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
    assert_eq!(
        code(w.post_catalog(&c1b, &r1b).await.expect_err("new data")),
        "bulk_publisher_paused"
    );
    assert_eq!(held_root(w, 1), None, "the thief's catalog is set aside");
    assert_eq!(held_root(w, 0), Some(t.c0.header.root()));
    assert_eq!(served_versions(w), vec![1]);
    let Theft { w, .. } = t;
    let w = w.restarted_with(&[(ME, 1 << 30, 16, false)]);
    assert_eq!(
        w.post_catalog(&c1b, &r1b).await.expect("replaced").0,
        StatusCode::CREATED
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review LIM-R1 L1: the refusals that need no chain come first, also
/// while a fork of the publisher is marked and due for its next read: a
/// publisher without an entry, and a paused publisher's new data.
#[tokio::test]
async fn limit_refusals_come_before_a_marked_forks_chain_read() {
    let t = theft().await;
    let (c1b, r1b, _, _) = replacement(&t);
    assert_eq!(
        code(t.w.post_catalog(&c1b, &r1b).await.expect_err("fork")),
        "bulk_publisher_forked"
    );
    let Theft { w, c1, bad, .. } = t;
    let next = manifest(&shard_files(40..50, 10), Some(&bad), T0 + 70);
    let rn = sign(&next);
    let more = catalog_of(
        Some(&c1),
        T0 + 80,
        vec![
            CatalogLeaf::of(&bad.header),
            other_leaf("synthetic.thief"),
            other_leaf("synthetic.new"),
        ],
    );
    let rm = sign_catalog(&more);
    for (entries, want) in [
        (
            &[(OTHER, 1u64 << 30, 16usize, false)][..],
            "bulk_publisher_not_listed",
        ),
        (&[(ME, 1 << 30, 16, true)][..], "bulk_publisher_paused"),
    ] {
        let w = relisted(w.node.clone(), w.clock.clone(), w.dir.clone(), entries);
        assert!(fork_file(&w).exists());
        w.clock.fetch_add(ANCHOR_RECHECK_SECS, Ordering::Relaxed);
        let reads = w.node.reads();
        assert_eq!(code(w.post(&next, &rn).await.expect_err(want)), want);
        assert_eq!(code(w.post_in(&next, 1).await.expect_err(want)), want);
        assert_eq!(
            code(w.post_catalog(&more, &rm).await.expect_err(want)),
            want
        );
        assert_eq!(w.node.reads(), reads, "{want}: no chain read");
    }
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review S3-R1 M3: the reviewer's tool asks the node, before `registerKey`,
/// whether limits per publisher are on and the publisher has its entry. The
/// answer names the asked id only: no other entry and no number.
#[tokio::test]
async fn the_listing_says_whether_a_publisher_has_its_limit() {
    let ask = |w: &World, q: &str| {
        let v = w
            .store
            .list(&parse_list_query(Some(q)).expect("query"))
            .expect("list");
        crate::openapi_check::check("GET", "/v1/bulk", 200, &v);
        v
    };
    // A node without a limits file: every registered publisher would get
    // the node's one quota.
    let open = world();
    assert_eq!(
        ask(&open, "listed=example.publisher")["limits"],
        json!({"on": false, "listed": false, "publisher": "example.publisher"})
    );
    assert!(ask(&open, "limit=5")["limits"].is_null(), "only when asked");
    let w = world_listed(&[
        (ME, 1 << 30, 16, false),
        ("other.publisher", 7 << 30, 9, true),
    ]);
    assert_eq!(
        ask(&w, &format!("listed={ME}"))["limits"],
        json!({"on": true, "listed": true, "publisher": ME})
    );
    let other = ask(&w, "listed=new.publisher");
    assert_eq!(
        other["limits"],
        json!({"on": true, "listed": false, "publisher": "new.publisher"})
    );
    let text = other.to_string();
    assert!(
        !text.contains("other.publisher") && !text.contains("quota"),
        "{text}"
    );
    assert!(parse_list_query(Some("listed=Not%20An%20Id")).is_err());
}
