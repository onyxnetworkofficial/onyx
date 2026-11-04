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
                    json!({"number": "0x64", "hash": hex0x(&FIN), "timestamp": format!("0x{T0:x}")})
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
    let other1 = catalog_of(Some(&c0), T0 + 31, vec![CatalogLeaf::of(&m2.header)]);
    w.node.anchor(&sign_catalog(&other1));
    assert_eq!(
        code(
            w.post_catalog(&other1, &sign_catalog(&other1))
                .await
                .expect_err("conflict")
        ),
        "bulk_catalog_conflict"
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
    w.node.anchor(&sign_catalog(&dropped));
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
    let w = world();
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
        w.store.catalog_record(&publisher().0, 0).map(|(c, _)| c),
        Some(c0.clone())
    );
    assert_eq!(
        w.store.catalog_record(&publisher().0, 1).map(|(c, _)| c),
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
        again.catalog_record(&publisher().0, 0).map(|(c, _)| c),
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
        again.catalog_record(&publisher().0, 0).map(|(c, _)| c),
        Some(c0.clone())
    );
    *again.old_body.lock().expect("lock") = None;
    assert_eq!(again.catalog_record(&publisher().0, 0), None);
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
