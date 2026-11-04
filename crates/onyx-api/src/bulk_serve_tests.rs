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
                "eth_getBlockByNumber" => {
                    assert_eq!(p[0], "finalized");
                    let n = c.fin.unwrap_or(0x64);
                    json!({"number": format!("0x{n:x}"), "hash": hex0x(&FIN), "timestamp": format!("0x{T0:x}")})
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
                        for x in [k.0.to_vec(), f.0.to_vec(), vec![0; 32], rh.0.to_vec()] {
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
                        out.extend(w(&[0]));
                        out.extend(w(&[0]));
                        out.extend(w(&[0]));
                    } else {
                        return Err("unknown call".into());
                    }
                    json!(hex0x(&out))
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
    let r = app.clone().oneshot(get(2)).await.expect("ok");
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let r = app.clone().oneshot(get(9)).await.expect("ok");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&w.dir);
}
