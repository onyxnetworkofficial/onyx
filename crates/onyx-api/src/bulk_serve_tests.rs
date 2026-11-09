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
    fn withdraw(&self, r: &SignedReport) {
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
    fn anchor_by(&self, r: &SignedReport, secret: &[u8; 32]) {
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
        store: Arc::new(store),
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
    let again = BulkStore::open(
        pin(),
        w.node.clone(),
        Arc::new(|| T0),
        &w.dir,
        &w.dir.join("warm"),
        BulkLimits::default(),
    )
    .expect("ok");
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
        store: Arc::new(store),
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
