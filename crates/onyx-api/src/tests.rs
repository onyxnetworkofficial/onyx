//! Router tests on a small synthetic shard (no adapters: core crate). The
//! real Bitcoin shard, settled by the M4 code, is exercised in
//! `adapters/source-btc/tests/api.rs`.
//!
//! Time (ROADMAP E12): every async test here runs on tokio's paused clock
//! (`start_paused = true`). Timers move only when the runtime has nothing
//! else to do (and never while a `spawn_blocking` job runs), so a deadline,
//! a slow facilitator or a re-check wait fires in the order the test sets,
//! however loaded the machine. Clocks in test fakes are tokio's, never
//! `std::time`. Exceptions: the multi-thread test (a std thread holds a
//! lock), whose waits are conditions with caps far above any run, and two
//! tests that only wait past the root-miss memory's std-clock TTL.

use super::*;
use crate::catalog::{DatasetPricing, Market, PricingPolicy, ShardBinding};
use crate::client::{
    allowed_keys, verify_response, verify_response_unchecked, verify_row, ChainAnswer, ClientNode,
    Expect, MetricDef, VerifyError, Window, BOUND_FIELDS, INFORMATIONAL_FIELDS,
};
use crate::contrib::{ContributorPolicy, StaticContributors, CONTRIBUTOR_HEADER};
use crate::roots::{FinalRoot, LedgerRoots, RootError};
use crate::x402::{
    decode_header, encode_header, exact_evm, BoxFuture, FacilitatorError, MockFacilitator,
    PaymentRequired, SettleResponse, VerifyResponse, BASE_SEPOLIA,
};
use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use onyx_coordinator::cas::{CasError, Kind, MemCas};
use onyx_coordinator::observe::SimulatedChain;
use onyx_core::{keccak256, ChainId, DataRights, Snapshot, SourceId, SourceRef};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tower::ServiceExt;

const PAY_TO: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
const CHAIN: u64 = 84_532;
const CONTRACT: [u8; 20] = [0x11; 20];

fn secret(label: &str) -> [u8; 32] {
    keccak256(label.as_bytes()).0
}

/// Address of the test key `label` (holds nothing; tests only).
fn addr(label: &str) -> String {
    hex0x(&exact_evm::address(&secret(label)).expect("key"))
}

fn row(subject: &str, value: &[u8]) -> Vec<u8> {
    borsh::to_vec(&OutputRow {
        key: subject.as_bytes().to_vec(),
        value: value.to_vec(),
    })
    .expect("row")
}

fn rights() -> DataRights {
    DataRights::public_chain(SourceId("btc.node".into()))
}

/// The pinned source of a test shard: a block range ending at `as_of`.
fn source_ref(as_of: u64) -> SourceRef {
    SourceRef {
        source: SourceId("btc.node".into()),
        snapshot: Snapshot::BlockRange {
            chain: ChainId("btc.mainnet".into()),
            from_height: as_of.saturating_sub(2),
            to_height: as_of,
            tip_hash: keccak256(&as_of.to_be_bytes()),
            content_hash: keccak256(b"content"),
        },
    }
}

fn binding(params: &[u8], as_of: u64) -> ShardBinding {
    ShardBinding {
        recipe_id: keccak256(b"recipe"),
        runtime_hash: keccak256(b"runtime"),
        label_set_hash: keccak256(b"labels"),
        source_hash: source_ref(as_of).hash(),
        schema_id: keccak256(b"schema"),
        rights_hash: rights().hash(),
        params_hash: keccak256(params),
        registry_root: keccak256(b"registry"),
    }
}

/// The test metric as a client knows it.
fn flow_def(params: &[u8]) -> MetricDef<'static> {
    MetricDef {
        source: "btc.node",
        chain: Some("btc.mainnet"),
        window: Window::Blocks(3),
        recipe_id: keccak256(b"recipe"),
        runtime_hash: keccak256(b"runtime"),
        schema_id: keccak256(b"schema"),
        params_hash: keccak256(params),
        label_set: keccak256(b"labels"),
        registry_root: keccak256(b"registry"),
    }
}

fn meta(round: u64, as_of: u64, params: &[u8], leaf_count: u64) -> ShardMeta {
    let b = binding(params, as_of);
    ShardMeta {
        round,
        shard: b.shard_id(),
        metric: "test.flow".into(),
        as_of,
        source: source_ref(as_of),
        binding: b,
        leaf_count: Some(leaf_count),
        test_labels: false,
        rights: rights(),
        proof_level: onyx_core::ProofLevel::Recomputed,
        input_floor: onyx_core::ProofLevel::Recomputed,
        label_level: Some(onyx_core::ProofLevel::Recomputed),
        attested_level: None,
        snapshot_unix: None,
        serving_check: None,
    }
}

/// A CAS that counts reads (M7-F3: unpaid requests must not read rows).
struct CountingCas {
    inner: MemCas,
    reads: AtomicU64,
}

impl Cas for CountingCas {
    fn put(&self, kind: Kind, bytes: &[u8]) -> Result<Hash32, CasError> {
        self.inner.put(kind, bytes)
    }
    fn get(&self, kind: Kind, key: &Hash32) -> Option<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(kind, key)
    }
}

/// Ledger roots presented as an on-chain source (for the bundle's chain id
/// and contract).
struct OnChain(Arc<LedgerRoots>);

impl RootSource for OnChain {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        self.0.finalized(round, shard)
    }
    fn kind(&self) -> &'static str {
        "chain"
    }
    fn ledger(&self) -> LedgerKind {
        LedgerKind::Chain {
            chain_id: CHAIN,
            contract: CONTRACT,
        }
    }
}

/// A root source on which `shard` finalizes between the first lookup and
/// the next (M7-L47-F1).
struct Flip {
    inner: Arc<dyn RootSource>,
    shard: Hash32,
    calls: AtomicU64,
}

impl RootSource for Flip {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        if *shard == self.shard && self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Box::pin(async { Ok(None) });
        }
        self.inner.finalized(round, shard)
    }
    fn kind(&self) -> &'static str {
        "ledger"
    }
    fn ledger(&self) -> LedgerKind {
        LedgerKind::OffChain
    }
}

/// Counts facilitator calls.
struct Counting {
    inner: Arc<dyn Facilitator>,
    verifies: AtomicU64,
    settles: AtomicU64,
}

impl Facilitator for Counting {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        self.settles.fetch_add(1, Ordering::SeqCst);
        self.inner.settle(r)
    }
}

fn counting() -> Arc<Counting> {
    Arc::new(Counting {
        inner: Arc::new(MockFacilitator::new([addr("payer a")])),
        verifies: AtomicU64::new(0),
        settles: AtomicU64::new(0),
    })
}

#[derive(Default)]
struct Setup {
    paid: bool,
    facilitator: Option<Arc<dyn Facilitator>>,
    config: ApiConfig,
    chain: bool,
    binding: Option<Hash32>,
    rows: Option<Vec<Vec<u8>>>,
    /// Report this shard as not finalized on its first root lookup only.
    flip: Option<Hash32>,
    /// Contributor pass (ADR-0025).
    contributors: Option<ContributorPolicy>,
    /// Dataset price for exports (ADR-0025).
    dataset: Option<DatasetPricing>,
    /// Keep receipts in this journal (REV-11).
    receipt_log: Option<std::path::PathBuf>,
    /// Keep settle intents in this journal (OPS-L2-F1).
    intent_log: Option<std::path::PathBuf>,
    /// A reporter whose status `/v1/health` shows (REV-L4-F1).
    reporter: Option<Arc<dyn crate::reporter::ReporterHealth>>,
    /// A level-3 post log `/v1/health` shows (ROADMAP C62).
    level3: Option<Arc<dyn crate::Level3Health>>,
    /// Keep the production cap on `validBefore` (OPS-L3-F4). Off by
    /// default: the shared signer [`pay`] signs one fixed far `validBefore`
    /// while tests move the clock; the cap's own tests turn it on.
    strict_valid_before: bool,
    /// Count every lookup that reaches the root source (AUD-1-F4).
    count_roots: Option<Arc<AtomicU64>>,
    /// A serving check (ADR-0034 Amendment 1).
    gate: Option<Arc<dyn gate::ServingGate>>,
    /// The shard names this serving check.
    serving_check: Option<&'static str>,
    /// Root reads of these shards fail (a node error, RESAPI-R1 F3).
    fail_roots: Option<Arc<std::sync::Mutex<std::collections::BTreeSet<Hash32>>>>,
    /// The shard's root is not final at the start (RESAPI-R1 F3).
    unfinal: bool,
    /// List the shard under this metric instead of `test.flow`.
    metric: Option<&'static str>,
    /// The authorization check after an unknown settle (M7-F5 residual).
    settle_check: Option<Arc<dyn crate::settle_check::SettleCheck>>,
    /// The shard pins no label set (`label_set_hash` zero, like a
    /// mining-pool shard; ROADMAP L14).
    no_label_set: bool,
}

/// Fails every root read of the shards in `fail` (RESAPI-R1 F3).
struct FailingRoots {
    inner: Arc<dyn RootSource>,
    fail: Arc<std::sync::Mutex<std::collections::BTreeSet<Hash32>>>,
}

impl RootSource for FailingRoots {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        if self.fail.lock().expect("lock").contains(shard) {
            return Box::pin(async { Err(RootError::Unavailable("node error".into())) });
        }
        self.inner.finalized(round, shard)
    }
    fn kind(&self) -> &'static str {
        "ledger"
    }
    fn ledger(&self) -> LedgerKind {
        LedgerKind::OffChain
    }
}

/// Counts lookups that reach the root source (AUD-1-F4).
struct CountingRoots {
    inner: Arc<dyn RootSource>,
    calls: Arc<AtomicU64>,
}

impl RootSource for CountingRoots {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let inner = self.inner.clone();
        Box::pin(async move {
            // Slow enough that concurrent requests overlap.
            tokio::time::sleep(Duration::from_millis(20)).await;
            inner.finalized(round, shard).await
        })
    }
    fn kind(&self) -> &'static str {
        "ledger"
    }
    fn ledger(&self) -> LedgerKind {
        LedgerKind::OffChain
    }
}

struct Fixture {
    state: Arc<ApiState>,
    shard: Hash32,
    root: Hash32,
    rows: Vec<Vec<u8>>,
    now: Arc<AtomicU64>,
    roots: Arc<LedgerRoots>,
    cas: Arc<CountingCas>,
}

fn build(s: Setup) -> Fixture {
    let cas = Arc::new(CountingCas {
        inner: MemCas::new(),
        reads: AtomicU64::new(0),
    });
    let rows = s.rows.unwrap_or_else(|| {
        vec![
            row("entity:a", b"value a........"),
            row("entity:b", b"value b........"),
            row("entity:c", b"value c........"),
        ]
    });
    let root = cas.put_rows(&rows).expect("rows");
    let mut m = meta(1, 100, b"params one", rows.len() as u64);
    if let Some(g) = s.serving_check {
        m = m.with_serving_check(g);
    }
    if let Some(name) = s.metric {
        m.metric = name.into();
    }
    if s.no_label_set {
        m.binding.label_set_hash = Hash32::ZERO;
        m.shard = m.binding.shard_id();
    }
    let shard = m.shard;
    let roots = Arc::new(LedgerRoots::new());
    if !s.unfinal {
        roots.insert(
            1,
            shard,
            FinalRoot {
                root,
                finalized_at: 400,
                finalized_unix: 1_000,
                binding: s.binding,
                tx: s.chain.then(|| hex0x(&[0x77; 32])),
            },
        );
    }
    let mut catalog = Catalog::new();
    catalog.add_shard(m);
    catalog.add_market(
        "okx-flow",
        Market {
            round: 1,
            shard,
            subject: "entity:b".into(),
        },
    );
    if s.paid {
        catalog.default_pricing = Some(PricingPolicy {
            base_price_micro_usdc: 1_000,
            fresh_window_secs: 3_600,
        });
    }
    catalog.default_dataset_pricing = s.dataset;
    let now = Arc::new(AtomicU64::new(1_500));
    let clock_now = now.clone();
    let payments = s.facilitator.map(|f| Payments {
        terms: PaymentTerms {
            network: BASE_SEPOLIA.into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            pay_to: PAY_TO.into(),
            max_timeout_seconds: 60,
            extra: Some(json!({"name": "USDC", "version": "2"})),
        },
        facilitator: f,
    });
    let source: Arc<dyn RootSource> = if s.chain {
        Arc::new(OnChain(roots.clone()))
    } else {
        roots.clone()
    };
    let source: Arc<dyn RootSource> = match s.flip {
        Some(shard) => Arc::new(Flip {
            inner: source,
            shard,
            calls: AtomicU64::new(0),
        }),
        None => source,
    };
    let source: Arc<dyn RootSource> = match &s.fail_roots {
        Some(fail) => Arc::new(FailingRoots {
            inner: source,
            fail: fail.clone(),
        }),
        None => source,
    };
    let source: Arc<dyn RootSource> = match &s.count_roots {
        Some(calls) => Arc::new(CountingRoots {
            inner: source,
            calls: calls.clone(),
        }),
        None => source,
    };
    let state = Arc::new(
        ApiState::new(
            catalog,
            source,
            cas.clone(),
            payments,
            Arc::new(move || clock_now.load(Ordering::SeqCst)),
            "https://api.onyx.test",
            if s.strict_valid_before {
                s.config
            } else {
                ApiConfig {
                    authorization_slack: Duration::from_secs(10_000_000_000),
                    ..s.config
                }
            },
        )
        .expect("config"),
    );
    let state = match s.contributors {
        Some(p) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_contributors(p),
        ),
        None => state,
    };
    let state = match s.receipt_log {
        Some(path) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_receipt_log(path, &crate::journal::TEST_BINDING)
                .expect("receipt log"),
        ),
        None => state,
    };
    let state = match s.intent_log {
        Some(path) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_intent_log(path, &crate::journal::TEST_BINDING)
                .expect("intent log"),
        ),
        None => state,
    };
    let state = match s.reporter {
        Some(r) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_reporter(r),
        ),
        None => state,
    };
    let state = match s.level3 {
        Some(l) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_level3(l),
        ),
        None => state,
    };
    let state = match s.settle_check {
        Some(c) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_settle_check(c),
        ),
        None => state,
    };
    let state = match s.gate {
        Some(g) => Arc::new(
            Arc::try_unwrap(state)
                .ok()
                .expect("sole owner")
                .with_gate(g)
                .expect("gate"),
        ),
        None => state,
    };
    Fixture {
        state,
        shard,
        root,
        rows,
        now,
        roots,
        cas,
    }
}

fn fixture(paid: bool, facilitator: Option<Arc<dyn Facilitator>>) -> Fixture {
    build(Setup {
        paid,
        facilitator,
        ..Setup::default()
    })
}

fn mock() -> Option<Arc<dyn Facilitator>> {
    Some(Arc::new(MockFacilitator::new([addr("payer a")])))
}

async fn send(
    f: &Fixture,
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
    ip: Option<IpAddr>,
) -> (StatusCode, HeaderMap, Value) {
    let verb = method.to_string();
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut req = req.body(Body::empty()).expect("request");
    if let Some(ip) = ip {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 4000))));
    }
    let resp = router(f.state.clone())
        .oneshot(req)
        .await
        .expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    // PP6: every answer these tests see conforms to docs/api/openapi.json.
    crate::openapi_check::check(&verb, uri, status.as_u16(), &body);
    (status, headers, body)
}

async fn get(f: &Fixture, uri: &str, headers: &[(&str, String)]) -> (StatusCode, HeaderMap, Value) {
    send(f, Method::GET, uri, headers, None).await
}

fn code(b: &Value) -> Option<&str> {
    b["error"]["code"].as_str()
}

fn cc(h: &HeaderMap) -> &str {
    h[header::CACHE_CONTROL].to_str().expect("ascii")
}

fn h32(v: &Value) -> Hash32 {
    let b = hex::decode(v.as_str().expect("hex").trim_start_matches("0x")).expect("hex");
    let mut a = [0u8; 32];
    a.copy_from_slice(&b);
    Hash32(a)
}

/// Check a row body's proof against its root with the network's own verifier.
fn verifies(body: &Value) -> bool {
    let row =
        hex::decode(body["row"].as_str().expect("row").trim_start_matches("0x")).expect("hex");
    let proof = onyx_merkle::Proof {
        index: body["proof"]["index"].as_u64().expect("i"),
        leaf_count: body["proof"]["leafCount"].as_u64().expect("n"),
        siblings: body["proof"]["siblings"]
            .as_array()
            .expect("s")
            .iter()
            .map(h32)
            .collect(),
    };
    onyx_merkle::verify(&h32(&body["root"]), &row, &proof)
}

/// A client with test key `signer` that pays exactly what the 402 challenge
/// asked for, signed (EIP-3009 / EIP-712) with nonce `[nonce; 32]`.
fn pay(challenge: &HeaderMap, signer: &str, nonce: u8) -> String {
    pay_until(challenge, signer, nonce, 9_999_999_999)
}

/// [`pay`] with the authorization's `validBefore`.
fn pay_until(challenge: &HeaderMap, signer: &str, nonce: u8, valid_before: u64) -> String {
    let pr: PaymentRequired = decode_header(
        challenge
            .get(x402::PAYMENT_REQUIRED)
            .expect("challenge header")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    let accepted = pr.accepts[0].clone();
    let mut to = [0u8; 20];
    to.copy_from_slice(&hex::decode(&accepted.pay_to[2..]).expect("hex"));
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&secret(signer)).expect("key"),
        to,
        value: accepted.amount.parse().expect("amount"),
        valid_after: 0,
        valid_before,
        nonce: [nonce; 32],
    };
    encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&secret(signer), &auth, &accepted).expect("signs"),
        accepted,
    })
}

#[tokio::test(start_paused = true)]
async fn health_reports_roots_and_payments() {
    let f = fixture(true, mock());
    let (s, h, b) = get(&f, "/v1/health", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["roots"], "ledger");
    assert_eq!(b["payments"], BASE_SEPOLIA);
    assert_eq!(b["x402Version"], 2);
    assert_eq!(cc(&h), "no-store");
    // L16: shards served per proof level (bounded: five levels at most), so
    // an operator sees a registry floor take effect.
    assert_eq!(b["shardLevels"], json!({"1": 1}));
    {
        let mut c = f.state.catalog.write().expect("lock");
        let mut m = c.shard(1, &f.shard).expect("shard").clone();
        m.proof_level = onyx_core::ProofLevel::BondedClaim;
        c.add_shard(m);
    }
    let (_, _, b) = get(&f, "/v1/health", &[]).await;
    assert_eq!(b["shardLevels"], json!({"4": 1}));
}

#[tokio::test(start_paused = true)]
async fn free_rows_carry_a_proof_that_verifies_against_the_root() {
    let f = fixture(false, None);
    for i in 0..3 {
        let uri = format!("/v1/proofs/1/{}/{i}", f.shard);
        let (s, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::OK);
        assert!(verifies(&b), "row {i}");
        assert_eq!(b["root"], f.root.to_string());
        assert_eq!(b["leafCount"], 3);
        assert_eq!(b["license"], "onyx:public-chain-data");
        // Conditional request: unchanged data, no body.
        let etag = h[header::ETAG].to_str().expect("etag").to_string();
        let (s304, h304, _) = get(&f, &uri, &[("if-none-match", etag)]).await;
        assert_eq!(s304, StatusCode::NOT_MODIFIED);
        assert_eq!(cc(&h304), "public, no-cache");
    }
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{}/3", f.shard), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("row_not_found"))
    );
}

/// M7-F1: `immutable` only on URLs that pin their root; everything else
/// revalidates; errors are never stored.
#[tokio::test(start_paused = true)]
async fn cache_control_per_route() {
    let f = fixture(false, None);
    let (r, s) = (f.root, f.shard);
    let cases = [
        (format!("/v1/proofs/1/{s}/0"), "public, no-cache"),
        (
            format!("/v1/proofs/1/{s}/0?root={r}"),
            "public, max-age=31536000, immutable",
        ),
        (format!("/v1/export/1/{s}"), "public, no-cache"),
        (
            format!("/v1/export/1/{s}?root={r}"),
            "public, max-age=31536000, immutable",
        ),
        (
            "/v1/metrics/test.flow?subject=entity:a".to_string(),
            "public, no-cache",
        ),
        ("/v1/settlement/okx-flow".to_string(), "public, no-cache"),
    ];
    for (uri, want) in cases {
        let (st, h, _) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
        assert_eq!(cc(&h), want, "{uri}");
        assert!(h.contains_key(header::ETAG), "{uri}");
    }
    // A pinned root that is not the finalized one: 409, not stored.
    let other = keccak256(b"other root");
    for uri in [
        format!("/v1/proofs/1/{s}/0?root={other}"),
        format!("/v1/export/1/{s}?root={other}"),
    ] {
        let (st, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::CONFLICT, Some("root_mismatch"))
        );
        assert_eq!(cc(&h), "no-store");
    }
    for uri in [
        format!("/v1/proofs/2/{s}/0"),
        "/v1/settlement/nope".to_string(),
        "/v1/nope".to_string(),
    ] {
        let (st, h, _) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(cc(&h), "no-store", "{uri}");
    }
    // Paid: 402 is not stored; the paid response is private.
    let f = fixture(true, mock());
    let uri = format!("/v1/proofs/1/{s}/0?root={r}");
    let (st, h, _) = get(&f, &uri, &[]).await;
    assert_eq!((st, cc(&h)), (StatusCode::PAYMENT_REQUIRED, "no-store"));
    let (st, h2, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(cc(&h2), "private, max-age=31536000, immutable");
    let uri = "/v1/settlement/okx-flow";
    let (_, h, _) = get(&f, uri, &[]).await;
    let (st, h2, _) = get(&f, uri, &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 2))]).await;
    assert_eq!((st, cc(&h2)), (StatusCode::OK, "private, no-cache"));
}

/// L2R-F8: a market settles only on a proof level the catalog's market policy
/// accepts. Default: levels 1 and 3; a level-2 shard is refused with 409
/// until the operator opts in.
#[tokio::test(start_paused = true)]
async fn markets_settle_only_on_accepted_proof_levels() {
    let f = fixture(false, None);
    let uri = "/v1/settlement/okx-flow";
    let (st, _, _) = get(&f, uri, &[]).await;
    assert_eq!(st, StatusCode::OK);
    {
        let mut c = f.state.catalog.write().expect("lock");
        let mut m = c.shard(1, &f.shard).expect("shard").clone();
        m.proof_level = onyx_core::ProofLevel::ReObserved;
        c.add_shard(m);
    }
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(b["error"]["code"], "proof_level_not_accepted");
    // Rows are still served outside markets; only settlement is refused.
    let (st, _, _) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    assert_eq!(st, StatusCode::OK);
    f.state.catalog.write().expect("lock").market_policy =
        onyx_core::MarketPolicy::of(&onyx_core::ProofLevel::ALL);
    let (st, _, _) = get(&f, uri, &[]).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn bad_requests_get_stable_json_errors() {
    let f = fixture(false, None);
    let s = f.shard;
    let cases = [
        (
            "/v1/proofs/1/zz/0".to_string(),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/proofs/1/0x{s}/0"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/proofs/+1/{s}/0"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/proofs/1/{s}/-1"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/proofs/1/{s}/0?root=0xAB"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/proofs/2/{s}/0"),
            StatusCode::NOT_FOUND,
            "unknown_shard",
        ),
        (
            "/v1/settlement/nope".to_string(),
            StatusCode::NOT_FOUND,
            "unknown_market",
        ),
        (
            "/v1/metrics/nope?subject=entity:a".to_string(),
            StatusCode::NOT_FOUND,
            "unknown_metric",
        ),
        (
            "/v1/metrics/test.flow?subject=entity:zz".to_string(),
            StatusCode::NOT_FOUND,
            "unknown_subject",
        ),
        // M7-F7: extractor rejections and unknown routes are JSON too.
        (
            "/v1/metrics/test.flow".to_string(),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "/v1/metrics/test.flow?subject=entity:a&as_of=x".to_string(),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        ("/v1/nope".to_string(), StatusCode::NOT_FOUND, "not_found"),
        ("/".to_string(), StatusCode::NOT_FOUND, "not_found"),
    ];
    for (uri, status, want) in cases {
        let (st, h, b) = get(&f, &uri, &[]).await;
        assert_eq!((st, code(&b)), (status, Some(want)), "{uri}");
        assert_eq!(h[header::CONTENT_TYPE], "application/json", "{uri}");
    }
    let (st, h, b) = send(&f, Method::POST, "/v1/health", &[], None).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::METHOD_NOT_ALLOWED, Some("method_not_allowed"))
    );
    // The router names the route's own methods (PP6).
    assert_eq!(h[header::ALLOW], "GET,HEAD");
}

/// M7-F8: one spelling per resource; others redirect to it.
#[tokio::test(start_paused = true)]
async fn non_canonical_paths_redirect_to_the_canonical_one() {
    let f = fixture(false, None);
    let s = f.shard.to_string();
    let bare = &s[2..];
    let upper = format!("0x{}", bare.to_uppercase());
    let want = format!("/v1/proofs/1/{s}/2");
    for uri in [
        format!("/v1/proofs/01/{s}/2"),
        format!("/v1/proofs/1/{bare}/2"),
        format!("/v1/proofs/1/{upper}/02"),
        format!("/v1/proofs/1/0X{bare}/2"),
    ] {
        let (st, h, _) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::PERMANENT_REDIRECT, "{uri}");
        assert_eq!(h[header::LOCATION], want.as_str(), "{uri}");
    }
    let r = f.root;
    let (st, h, _) = get(&f, &format!("/v1/export/001/{upper}?root={r}"), &[]).await;
    assert_eq!(st, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        h[header::LOCATION],
        format!("/v1/export/1/{s}?root={r}").as_str()
    );
}

/// M7-L50-F2: HEAD never pays. Without a payment it shows the 402 terms;
/// with one it is refused before verify or settle, so the same payment
/// still buys the row with GET.
#[tokio::test(start_paused = true)]
async fn head_never_settles_a_payment() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (s, h, _) = send(&f, Method::HEAD, &uri, &[], None).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert!(
        h.contains_key(x402::PAYMENT_REQUIRED),
        "HEAD shows the terms"
    );
    let sig = pay(&h, "payer a", 1);
    let (s, h2, _) = send(
        &f,
        Method::HEAD,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, sig.clone())],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(!h2.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 0);
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0);
    // The payment is not spent: GET with it serves the row and settles once.
    let (s, h3, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(verifies(&b));
    assert!(h3.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
}

/// ROADMAP B4: the level-2 limits (REVLOG-1, REVLOG-5) are stated on
/// level-2 answers only: a level-1 or level-3 row, its 402 body and its
/// PAYMENT-REQUIRED offer carry no `limits` and no `extensions`.
#[tokio::test(start_paused = true)]
async fn level_1_and_3_answers_and_offers_state_no_level2_limits() {
    let f = fixture(true, mock());
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    for (level, nonce) in [
        (onyx_core::ProofLevel::Recomputed, 71),
        (onyx_core::ProofLevel::SourceProven, 72),
    ] {
        {
            let mut c = f.state.catalog.write().expect("lock");
            let mut m = c.shard(1, &f.shard).expect("shard").clone();
            m.proof_level = level;
            c.add_shard(m);
        }
        let (s, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{level:?}");
        assert!(b.get("limits").is_none(), "{level:?} 402 body");
        let pr: PaymentRequired =
            decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("offer");
        assert!(pr.extensions.is_none(), "{level:?} offer");
        let (s, _, b) = get(
            &f,
            &uri,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))],
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{level:?}: {b}");
        assert!(b.get("limits").is_none(), "{level:?} answer");
    }
}

#[tokio::test(start_paused = true)]
async fn paid_row_needs_an_exact_settled_payment() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    // No payment: 402 with the challenge, and no data.
    let (s, h, b) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert!(b.get("row").is_none());
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(pr.accepts[0].amount, "1000");
    assert_eq!(pr.accepts[0].network, BASE_SEPOLIA);
    assert_eq!(pr.resource.url, format!("https://api.onyx.test{uri}"));
    // Paid: 200, the proof verifies, and the settlement is reported.
    let sig = pay(&h, "payer a", 1);
    let (s, h2, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig.clone())]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(verifies(&b));
    let settled: SettleResponse =
        decode_header(h2[x402::PAYMENT_RESPONSE].to_str().expect("ascii")).expect("settle header");
    assert!(settled.success);
    assert_eq!(settled.payer, Some(addr("payer a")));
    // M7-F5: the identical retry gets the same response, without a second
    // settlement.
    let (s, h3, b3) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig.clone())]).await;
    assert_eq!((s, &b3), (StatusCode::OK, &b));
    assert_eq!(h3[x402::PAYMENT_RESPONSE], h2[x402::PAYMENT_RESPONSE]);
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
    // The same authorization for another row: refused (URL-bound payment).
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let (s, h4, b) = get(&f, &other, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(b["error"]["message"], "payment is not for this resource");
    assert!(h4.contains_key(x402::PAYMENT_REQUIRED));
    // A payer without funds is refused by verify.
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer without funds", 2))],
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
}

/// OPS-L3-F4: an authorization valid longer than `now + maxTimeoutSeconds`
/// plus the slack (300 s by default) is refused before the facilitator: its
/// settle intent would otherwise stay open until it expired.
#[tokio::test(start_paused = true)]
async fn a_long_lived_authorization_is_refused_before_verify() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        strict_valid_before: true,
        ..Setup::default()
    });
    let now = f.now.load(Ordering::SeqCst);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let late = now + 60 + 300 + 1;
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay_until(&h, "payer a", 1, late))],
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        b["error"]["message"],
        "invalid_exact_evm_payload_authorization_valid_before_too_late"
    );
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 0, "never verified");
    // As an x402 client signs it (`now + maxTimeoutSeconds`), and at the
    // edge of the slack: past our check, on to the facilitator's verify (the
    // mock judges validity on the wall clock, so its answer is not the
    // point here).
    for (nonce, vb) in [(2u8, now + 60), (3, now + 60 + 300)] {
        let before = fac.verifies.load(Ordering::SeqCst);
        let (_, _, b) = get(
            &f,
            &uri,
            &[(x402::PAYMENT_SIGNATURE, pay_until(&h, "payer a", nonce, vb))],
        )
        .await;
        assert_ne!(
            b["error"]["message"],
            "invalid_exact_evm_payload_authorization_valid_before_too_late"
        );
        assert_eq!(fac.verifies.load(Ordering::SeqCst), before + 1, "verified");
    }
}

/// OPS-L4-F1: an authorization that expires before our settle can be mined
/// is refused before verify (a settle mined after `validBefore` fails, and
/// its intent would have to wait for the resolver).
#[tokio::test(start_paused = true)]
async fn a_short_lived_authorization_is_refused_before_verify() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        strict_valid_before: true,
        ..Setup::default()
    });
    let now = f.now.load(Ordering::SeqCst);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, b) = get(
        &f,
        &uri,
        &[(
            x402::PAYMENT_SIGNATURE,
            pay_until(&h, "payer a", 1, now + 2),
        )],
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        b["error"]["message"],
        "invalid_exact_evm_payload_authorization_valid_before"
    );
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 0, "never verified");
}

/// OPS-L4-F4: one authorization sent with two resources (x402 does not sign
/// the resource) gets one settle intent: while the first is open, the
/// second request is refused (409) and nothing is charged.
#[tokio::test(start_paused = true)]
async fn one_authorization_for_two_resources_settles_once() {
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(SettleFails)),
        ..Setup::default()
    });
    let a = format!("/v1/proofs/1/{}/1", f.shard);
    let b = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, ha, _) = get(&f, &a, &[]).await;
    let (_, hb, _) = get(&f, &b, &[]).await;
    // The first settle fails: its intent stays open for the resolver.
    let (s, _, _) = get(&f, &a, &[(x402::PAYMENT_SIGNATURE, pay(&ha, "payer a", 9))]).await;
    assert_ne!(s, StatusCode::OK);
    assert_eq!(f.state.intents.open_count(), 1);
    let (s, _, body) = get(&f, &b, &[(x402::PAYMENT_SIGNATURE, pay(&hb, "payer a", 9))]).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "payment_in_use");
    assert_eq!(f.state.intents.open_count(), 1, "still one intent");
}

/// OPS-L4-F3: while the server's clock disagrees with the chain's by more
/// than the limit, no paid settle starts (503, nothing charged).
#[tokio::test(start_paused = true)]
async fn a_skewed_clock_refuses_paid_requests() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        ..Setup::default()
    });
    f.state
        .intents
        .set_skew(i64::try_from(intents::MAX_CLOCK_SKEW_SECS).expect("fits") + 5);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, body) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "clock_skew");
    assert_eq!(f.state.intents.open_count(), 0);
    f.state.intents.set_skew(0);
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 2))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

/// OPS-L3-F1: the settle succeeds but its receipt cannot be made durable
/// (the receipt journal fails at that write). The paid answer goes out, and
/// the settle intent stays on disk for the resolver instead of being ended.
#[tokio::test(start_paused = true)]
async fn a_receipt_that_is_not_durable_keeps_its_intent() {
    let dir = crate::journal::test_dir("api-receipt-not-durable");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(dir.join("receipts.jsonl")),
        intent_log: Some(dir.join("intents.jsonl")),
        ..Setup::default()
    });
    f.state.receipts.break_journal_for_test();
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(f.state.receipts.write_errors(), 1);
    assert_eq!(f.state.intents.open_count(), 1, "the intent was not ended");
    assert_eq!(f.state.intents.due().len(), 1, "it waits for the resolver");
    drop(f);
    let again =
        crate::intents::IntentLog::open(dir.join("intents.jsonl"), &crate::journal::TEST_BINDING)
            .expect("reopen");
    assert_eq!(again.open_count(), 1, "on disk across a restart");
}

/// M7L3-R3-4: the request above was served, so its intent (kept for the
/// resolver) loses its request key on disk: after a restart, which loses
/// the settled-response cache, its identical retry is not served again from
/// that intent or from a proof of it.
#[tokio::test(start_paused = true)]
async fn m7l3_r3_4_a_served_request_without_receipt_drops_its_retry_key() {
    let dir = crate::journal::test_dir("api-r3-4");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(dir.join("receipts.jsonl")),
        intent_log: Some(dir.join("intents.jsonl")),
        ..Setup::default()
    });
    f.state.receipts.break_journal_for_test();
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(f.state.intents.open_count(), 1, "kept for the resolver");
    drop(f);
    let (_, live) = crate::journal::Journal::open(
        dir.join("intents.jsonl"),
        crate::intents::JOURNAL_KIND,
        &crate::journal::TEST_BINDING,
    )
    .expect("reopen");
    assert_eq!(live.len(), 1, "the intent is on disk");
    assert!(
        live.iter().all(|(_, v)| v.get("request").is_none()),
        "no retry key on disk: {live:?}"
    );
}

/// M7-F4: forged, underpaid and malformed payments never reach the facilitator.
#[tokio::test(start_paused = true)]
async fn bad_payments_are_refused_before_the_facilitator() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let good: PaymentPayload = decode_header(&pay(&h, "payer a", 3)).expect("payload");
    // Underpaid.
    let mut under = good.clone();
    under.accepted.amount = "1".into();
    under.payload["authorization"]["value"] = json!("1");
    // Signed by someone else on behalf of the funded payer.
    let mut forged: PaymentPayload = decode_header(&pay(&h, "attacker", 3)).expect("payload");
    forged.payload["authorization"]["from"] = json!(addr("payer a"));
    // Expired (the signature is checked last, so this one is not re-signed).
    let mut expired = good.clone();
    expired.payload["authorization"]["validBefore"] = json!("1000");
    for (p, want) in [
        (&under, "payment does not match the requirements"),
        (&forged, "invalid_exact_evm_payload_signature"),
        (
            &expired,
            "invalid_exact_evm_payload_authorization_valid_before",
        ),
    ] {
        let (s, h2, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, encode_header(p))]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{want}");
        assert!(h2.contains_key(x402::PAYMENT_REQUIRED));
        assert_eq!(b["error"]["message"], want);
    }
    let (s, _, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, "!!!".into())]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, Some("invalid_payment"))
    );
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 0);
}

/// M7-F4: payment attempts are rate limited per client address.
#[tokio::test(start_paused = true)]
async fn payment_attempts_are_rate_limited_per_address() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        config: ApiConfig {
            payment_attempts_per_minute: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let bad = [(x402::PAYMENT_SIGNATURE, "!!!".to_string())];
    let a = Some(IpAddr::from([10, 0, 0, 1]));
    for _ in 0..2 {
        let (s, _, _) = send(&f, Method::GET, &uri, &bad, a).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    let (s, h, b) = send(&f, Method::GET, &uri, &bad, a).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert_eq!(h[header::RETRY_AFTER], "30");
    // Another address is not affected; unpaid requests are not counted.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &bad,
        Some(IpAddr::from([10, 0, 0, 2])),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = send(&f, Method::GET, &uri, &[], a).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
}

/// A facilitator that never answers.
struct Hangs;
impl Facilitator for Hangs {
    fn verify<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(std::future::pending())
    }
    fn settle<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(std::future::pending())
    }
}

/// M7-F4 / F3: a hanging facilitator costs one deadline, not a worker forever.
#[tokio::test(start_paused = true)]
async fn facilitator_and_request_deadlines() {
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Hangs)),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(20),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = pay(&h, "payer a", 4);
    let (s, h2, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig.clone())]).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
    assert!(b.get("row").is_none());
    assert_eq!(cc(&h2), "no-store");
    // A settle may take longer than the request (SETTLE_PATIENCE): the
    // request ends with 504 while the settlement goes on.
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: counting(),
            settle_ms: None,
        })),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(20),
            request_timeout: Duration::from_millis(50),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let (s, _, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::GATEWAY_TIMEOUT, Some("timeout"))
    );
}

/// Verifies at once; settles after `settle_ms` (never, when `None`).
struct Slow {
    inner: Arc<Counting>,
    settle_ms: Option<u64>,
}
impl Facilitator for Slow {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(async move {
            match self.settle_ms {
                Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                None => std::future::pending::<()>().await,
            }
            self.inner.settle(r).await
        })
    }
}

/// M7-L39-F1: the request timeout must leave room for verify and settle.
#[test]
fn config_refuses_a_request_deadline_shorter_than_the_payment() {
    assert!(ApiConfig::default().validate().is_ok());
    for c in [
        ApiConfig {
            request_timeout: Duration::from_millis(20),
            ..ApiConfig::default()
        },
        ApiConfig {
            facilitator_timeout: Duration::from_secs(5),
            ..ApiConfig::default()
        },
        ApiConfig {
            max_facilitator_calls: 0,
            ..ApiConfig::default()
        },
    ] {
        assert!(c.validate().is_err(), "{c:?}");
        assert!(ApiState::new(
            Catalog::new(),
            Arc::new(LedgerRoots::new()),
            Arc::new(MemCas::new()),
            None,
            Arc::new(|| 0),
            "https://api.onyx.test",
            c,
        )
        .is_err());
    }
}

/// OPS-L1-F1: a settle that outlives its request is counted in
/// `settle_tasks` until its receipt is on the log, so a stop that waits for
/// `wait_idle` never drops a payment that landed.
#[tokio::test(start_paused = true)]
async fn a_stop_can_wait_for_a_settle_that_outlives_its_request() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: fac.clone(),
            settle_ms: Some(300),
        })),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(250),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    assert_eq!(f.state.settle_budget(), Duration::from_millis(400));
    assert_eq!(f.state.stop_budget(), Duration::from_millis(650));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 11))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::GATEWAY_TIMEOUT, Some("timeout"))
    );
    // The request is over; its settle still runs and has no receipt yet.
    assert_eq!(f.state.settle_tasks.live(), 1);
    assert_eq!(f.state.receipts.pending(), 0);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(5)).await);
    assert_eq!(f.state.settle_tasks.live(), 0);
    assert_eq!(f.state.receipts.pending(), 1, "the receipt is on the log");
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
    // A wait with nothing running returns at once.
    assert!(f.state.settle_tasks.wait_idle(Duration::ZERO).await);
}

/// M7-L39-F1: a settle that outlives the request deadline still completes
/// and is stored: the identical retry gets the paid answer, settled once.
/// The settle ends 50 ms after the first deadline and 350 ms before the
/// retry's; on the paused clock (E12) that order holds on any machine (the
/// real-time version failed once at load 60, LBL-V2R-F4).
#[tokio::test(start_paused = true)]
async fn a_settle_slower_than_the_request_is_kept_for_the_retry() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: fac.clone(),
            settle_ms: Some(450),
        })),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(150),
            request_timeout: Duration::from_millis(400),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 9))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::GATEWAY_TIMEOUT, Some("timeout"))
    );
    // Retried at once: it waits for the settlement in flight, then replays.
    let (s, h, b) = get(&f, &uri, &sig).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(verifies(&b));
    assert_eq!(b["index"], 2);
    assert!(h.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 1);
}

/// M7-L39-F4: IPv6 clients are limited per /64, not per address.
#[test]
fn client_keys_group_ipv6_by_64() {
    let k = |s: &str| client_key(s.parse().expect("ip"));
    assert_eq!(k("2001:db8:1:2::1"), k("2001:db8:1:2:ffff::9"));
    assert_ne!(k("2001:db8:1:2::1"), k("2001:db8:1:3::1"));
    assert_eq!(k("::ffff:10.0.0.1"), k("10.0.0.1"));
    assert_ne!(k("10.0.0.1"), k("10.0.0.2"));
}

#[tokio::test(start_paused = true)]
async fn one_ipv6_64_shares_one_rate_limit() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        config: ApiConfig {
            payment_attempts_per_minute: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let bad = [(x402::PAYMENT_SIGNATURE, "!!!".to_string())];
    for ip in ["2001:db8::1", "2001:db8::2"] {
        let (s, _, _) = send(&f, Method::GET, &uri, &bad, Some(ip.parse().expect("ip"))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &bad,
        Some("2001:db8::3".parse().expect("ip")),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &bad,
        Some("2001:db8:0:1::3".parse().expect("ip")),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// M7-L39-F4: a global cap on facilitator calls; extra payers get 503
/// `facilitator_busy` at once instead of piling onto the facilitator.
#[tokio::test(start_paused = true)]
async fn facilitator_calls_are_capped_globally() {
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Hangs)),
        config: ApiConfig {
            max_facilitator_calls: 1,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let first = pay(&h, "payer a", 1);
    let state = f.state.clone();
    let u = uri.clone();
    let hanging = tokio::spawn(async move {
        let req = Request::builder()
            .uri(u)
            .header(x402::PAYMENT_SIGNATURE, first)
            .body(Body::empty())
            .expect("request");
        router(state).oneshot(req).await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (s, h2, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 2))],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("facilitator_busy"))
    );
    assert_eq!(h2[header::RETRY_AFTER], "1");
    hanging.abort();
}

/// M7-L39-F4: a signature that is not 65 bytes never reaches the facilitator.
#[tokio::test(start_paused = true)]
async fn odd_length_signatures_are_refused_locally() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let mut p: PaymentPayload = decode_header(&pay(&h, "payer a", 3)).expect("payload");
    let sig = p.payload["signature"].as_str().expect("sig").to_string();
    p.payload["signature"] = json!(format!("{sig}00"));
    let (s, _, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, encode_header(&p))]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{b}");
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 0);
}

/// M7-L39-F5: a settled response is replayed only to the client that paid,
/// and only a few times.
#[tokio::test(start_paused = true)]
async fn replays_are_bound_to_the_client_and_capped() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 11))];
    let a = Some(IpAddr::from([10, 0, 0, 1]));
    let b_ip = Some(IpAddr::from([10, 0, 0, 2]));
    let (s, _, _) = send(&f, Method::GET, &uri, &sig, a).await;
    assert_eq!(s, StatusCode::OK);
    // A copied header from elsewhere is a new (spent) payment, not a replay.
    let (s, _, _) = send(&f, Method::GET, &uri, &sig, b_ip).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    for _ in 0..cache::SETTLED_REPLAYS {
        let (s, _, _) = send(&f, Method::GET, &uri, &sig, a).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _, _) = send(&f, Method::GET, &uri, &sig, a).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
}

/// M7-L39-F3: before payment, a known and an unknown subject look the same
/// (same 402 terms, no row read, no 304); after payment an unknown subject
/// is a 404 and nothing is charged.
#[tokio::test(start_paused = true)]
async fn unpaid_metric_lookups_do_not_reveal_subjects() {
    let fac = counting();
    let f = fixture(true, Some(fac.clone()));
    let terms = |h: &HeaderMap| -> Value {
        let pr: PaymentRequired =
            decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("pr");
        serde_json::to_value(&pr.accepts).expect("json")
    };
    let mut seen = Vec::new();
    for subject in ["entity:a", "entity:zz"] {
        let uri = format!("/v1/metrics/test.flow?subject={subject}");
        let (s, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{subject}");
        seen.push((code(&b).map(str::to_string), terms(&h)));
        let (s, _, _) = get(&f, &uri, &[("if-none-match", "*".to_string())]).await;
        assert_eq!(
            s,
            StatusCode::PAYMENT_REQUIRED,
            "{subject}: no 304 before payment"
        );
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(f.cas.reads.load(Ordering::SeqCst), 0);
    let uri = "/v1/metrics/test.flow?subject=entity:zz";
    let (_, h, _) = get(&f, uri, &[]).await;
    let (s, _, b) = get(
        &f,
        uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 21))],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_subject"))
    );
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0);
}

/// M7-L39-F3: a metric lookup reads at most MAX_METRIC_CANDIDATES shards.
#[tokio::test(start_paused = true)]
async fn metric_lookups_cap_the_shards_they_read() {
    for (newer, want) in [
        (MAX_METRIC_CANDIDATES - 1, StatusCode::OK),
        (MAX_METRIC_CANDIDATES, StatusCode::NOT_FOUND),
    ] {
        let f = fixture(false, None);
        for k in 0..newer as u64 {
            let rows = vec![row("entity:other", b"value x........")];
            let root = f.cas.put_rows(&rows).expect("rows");
            let m = meta(2 + k, 101 + k, format!("params {k}").as_bytes(), 1);
            f.roots.insert(
                m.round,
                m.shard,
                FinalRoot {
                    root,
                    finalized_at: 400,
                    finalized_unix: 1_000,
                    binding: None,
                    tx: None,
                },
            );
            f.state.catalog.write().expect("lock").add_shard(m);
        }
        let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
        assert_eq!(s, want, "{newer} newer shards: {b}");
    }
}

/// M7-L39-F6: one spelling per URL, so one payment `resource` per resource.
#[tokio::test(start_paused = true)]
async fn query_strings_have_one_spelling() {
    let f = fixture(false, None);
    let s = f.shard;
    for (uri, want) in [
        (
            format!("/v1/proofs/1/{s}/%30"),
            format!("/v1/proofs/1/{s}/0"),
        ),
        (
            "/v1/metrics/test.flow?as_of=100&subject=entity:c".to_string(),
            "/v1/metrics/test.flow?subject=entity:c&as_of=100".to_string(),
        ),
        (
            "/v1/metrics/test.flow?subject=entity%3Ac".to_string(),
            "/v1/metrics/test.flow?subject=entity:c".to_string(),
        ),
        (
            "/v1/metrics/test.flow?subject=entity:c&as_of=0100".to_string(),
            "/v1/metrics/test.flow?subject=entity:c&as_of=100".to_string(),
        ),
    ] {
        let (st, h, _) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::PERMANENT_REDIRECT, "{uri}");
        assert_eq!(h[header::LOCATION], want.as_str(), "{uri}");
    }
    for uri in [
        format!("/v1/proofs/1/{s}/0?foo=1"),
        format!("/v1/export/1/{s}?root={}&x=1", f.root),
        "/v1/metrics/test.flow?subject=entity:c&utm=1".to_string(),
        "/v1/metrics/Test.Flow?subject=entity:c".to_string(),
        "/v1/settlement/okx-flow?x=1".to_string(),
        "/v1/settlement/OKX-flow".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri}: {b}");
    }
}

/// M7-L39-F2: a valid proof of something else is refused by the client.
#[tokio::test(start_paused = true)]
async fn client_refuses_valid_answers_to_other_questions() {
    let f = fixture(false, None);
    let s = f.shard;
    let (_, _, proof) = get(&f, &format!("/v1/proofs/1/{s}/1"), &[]).await;
    let (_, _, export) = get(&f, &format!("/v1/export/1/{s}"), &[]).await;
    let (_, _, metric) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    let (_, _, settle) = get(&f, "/v1/settlement/okx-flow", &[]).await;
    let pr = |round, shard, index| Expect::Proof {
        round,
        shard,
        index,
    };
    let me = |metric, subject, as_of| Expect::Metric {
        metric,
        def: flow_def(b"params one"),
        subject,
        as_of,
        min_as_of: 0,
    };
    let se = |market, subject| Expect::Settlement {
        market,
        round: 1,
        shard: s,
        subject,
    };
    let nr = VerifyError::NotRequested;
    let other = keccak256(b"other shard");
    let mut idx = proof.clone();
    idx["index"] = json!(2);
    let mut subj = metric.clone();
    subj["subject"] = json!("entity:b");
    let mut value = settle.clone();
    value["value"] = json!(hex0x(b"forged"));
    let mut ssubj = settle.clone();
    ssubj["subject"] = json!("entity:c");
    let cases: Vec<(&Value, Expect<'_>, VerifyError)> = vec![
        (&proof, pr(1, s, 2), nr("index")),
        (&proof, pr(2, s, 1), nr("round")),
        (&proof, pr(1, other, 1), nr("shard")),
        (
            &idx,
            pr(1, s, 2),
            VerifyError::Malformed("index differs from the proof"),
        ),
        (&export, pr(1, s, 0), nr("an export")),
        (
            &proof,
            Expect::Export { round: 1, shard: s },
            nr("one row, not an export"),
        ),
        (&metric, me("test.other", "entity:c", None), nr("metric")),
        (&metric, me("test.flow", "entity:b", None), nr("subject")),
        (&subj, me("test.flow", "entity:b", None), nr("row key")),
        (&metric, me("test.flow", "entity:c", Some(99)), nr("asOf")),
        (&settle, se("other", "entity:b"), nr("market")),
        (&settle, se("okx-flow", "entity:c"), nr("subject")),
        (&ssubj, se("okx-flow", "entity:c"), nr("row key")),
        (
            &value,
            se("okx-flow", "entity:b"),
            VerifyError::Malformed("value differs from the row"),
        ),
    ];
    for (body, ex, want) in cases {
        assert_eq!(verify_response_unchecked(body, &ex), Err(want), "{ex:?}");
    }
    // The untampered answers pass.
    assert!(verify_response_unchecked(&metric, &me("test.flow", "entity:c", Some(100))).is_ok());
    assert!(verify_response_unchecked(&settle, &se("okx-flow", "entity:b")).is_ok());
}

/// M7-F3: pricing and the 402 need no row: an unpaid request does not read
/// the CAS, and paid ones share one read per root.
#[tokio::test(start_paused = true)]
async fn unpaid_requests_do_not_read_rows() {
    let f = fixture(true, mock());
    let s = f.shard;
    for uri in [
        format!("/v1/proofs/1/{s}/0"),
        format!("/v1/export/1/{s}"),
        "/v1/settlement/okx-flow".to_string(),
    ] {
        for _ in 0..3 {
            let (st, _, _) = get(&f, &uri, &[]).await;
            assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{uri}");
        }
    }
    assert_eq!(f.cas.reads.load(Ordering::SeqCst), 0);
    let uri = format!("/v1/proofs/1/{s}/0");
    let (_, h, _) = get(&f, &uri, &[]).await;
    for n in [5, 6] {
        let (st, _, _) = get(
            &f,
            &uri,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", n))],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    assert_eq!(f.cas.reads.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.caches.trees_cached(), 1);
}

#[tokio::test(start_paused = true)]
async fn export_is_priced_per_row_and_capped() {
    let f = fixture(true, mock());
    let uri = format!("/v1/export/1/{}", f.shard);
    let (s, h, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(pr.accepts[0].amount, "3000");
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 7))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["rows"].as_array().map(Vec::len), Some(3));
    let f = build(Setup {
        config: ApiConfig {
            export_max_rows: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let (s, h, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("export_too_large"))
    );
    assert_eq!(cc(&h), "no-store");
}

#[tokio::test(start_paused = true)]
async fn old_data_decays_to_free_from_its_finalization_time() {
    let f = fixture(true, mock());
    let uri = format!("/v1/export/1/{}", f.shard);
    let (s, _, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    // Finalized at unix 1,000 + window 3,600: free from 4,600 on.
    f.now.store(4_599, Ordering::SeqCst);
    let (s, _, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    f.now.store(4_600, Ordering::SeqCst);
    let (s, h, b) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
    let rows: Vec<Vec<u8>> = b["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| hex::decode(r.as_str().expect("hex").trim_start_matches("0x")).expect("hex"))
        .collect();
    assert_eq!(rows, f.rows);
    assert_eq!(MerkleTree::from_rows(&rows).root().to_string(), b["root"]);
}

/// M7-F10: a poisoned catalog is a 500, never free data.
#[tokio::test(start_paused = true)]
async fn unreadable_price_is_an_error_not_free() {
    let f = fixture(true, mock());
    let state = f.state.clone();
    let _ = std::thread::spawn(move || {
        let _held = state.catalog.write();
        panic!("poison the catalog lock (expected in this test)");
    })
    .join();
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::INTERNAL_SERVER_ERROR, Some("internal"))
    );
    assert!(b.get("row").is_none());
}

#[tokio::test(start_paused = true)]
async fn restricted_and_test_label_data_is_refused_at_serve_time() {
    let f = fixture(false, None);
    {
        let mut c = f.state.catalog.write().expect("lock");
        let mut meta = c.shard(1, &f.shard).expect("meta").clone();
        meta.rights.redistribution = Redistribution::Restricted;
        meta.rights.license = "LicenseRef-vendor".into();
        meta.rights.terms = "vendor.terms.v1".into();
        c.add_shard(meta);
    }
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            Some("restricted_data")
        )
    );
    assert!(b.get("row").is_none());
    // A shard pinned to a `test.*` label set: refused unless in dev mode.
    for dev in [false, true] {
        let f = build(Setup {
            config: ApiConfig {
                allow_test_labels: dev,
                ..ApiConfig::default()
            },
            ..Setup::default()
        });
        {
            let mut c = f.state.catalog.write().expect("lock");
            let mut meta = c.shard(1, &f.shard).expect("meta").clone();
            meta.test_labels = true;
            c.add_shard(meta);
        }
        let (s, _, b) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
        if dev {
            assert_eq!(s, StatusCode::OK);
        } else {
            assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, Some("test_labels")));
        }
    }
}

/// ROADMAP L14, THREATS L8-5: with `rights: Fallback` every route that serves
/// a label-derived shard refuses it with 403 `label_rights_withheld`, before
/// any 402 (no payment header, nothing charged, no row read); `Cleared` (the
/// default) serves the same shard, and a shard without a label set is served
/// in either mode.
#[tokio::test(start_paused = true)]
async fn fallback_rights_withhold_label_derived_shards_before_any_payment() {
    use crate::catalog::LabelRights;
    let uris = |s: Hash32| {
        [
            format!("/v1/proofs/1/{s}/0"),
            format!("/v1/export/1/{s}"),
            "/v1/metrics/test.flow?subject=entity:b".to_string(),
            "/v1/settlement/okx-flow".to_string(),
        ]
    };
    for rights in [LabelRights::Cleared, LabelRights::Fallback] {
        for no_label_set in [false, true] {
            let f = build(Setup {
                paid: true,
                facilitator: mock(),
                config: ApiConfig {
                    rights,
                    ..ApiConfig::default()
                },
                no_label_set,
                ..Setup::default()
            });
            let (_, _, h) = get(&f, "/v1/health", &[]).await;
            assert_eq!(h["rights"], rights.as_str());
            let withheld = rights == LabelRights::Fallback && !no_label_set;
            // Review L16-R6: /v1/health counts a withheld shard apart, never
            // as served at its level.
            if withheld {
                assert_eq!(h["shardLevels"], json!({}), "{rights:?}");
                assert_eq!(h["shardsWithheld"], 1, "{rights:?}");
            } else {
                assert_eq!(h["shardLevels"], json!({"1": 1}), "{rights:?}");
                assert_eq!(h["shardsWithheld"], 0, "{rights:?}");
            }
            for uri in uris(f.shard) {
                let (st, hd, b) = get(&f, &uri, &[]).await;
                let case = format!("{rights:?} no_label_set={no_label_set} {uri}");
                if withheld {
                    assert_eq!(
                        (st, code(&b)),
                        (StatusCode::FORBIDDEN, Some("label_rights_withheld")),
                        "{case}"
                    );
                    assert!(!hd.contains_key("payment-required"), "{case}");
                    let msg = b["error"]["message"].as_str().expect("message");
                    assert!(msg.contains("ONYX_RIGHTS=fallback"), "{case}: {msg}");
                    assert!(b.get("row").is_none(), "{case}");
                } else {
                    // Priced: the first answer is the 402 challenge.
                    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{case}");
                }
            }
            if withheld {
                assert_eq!(f.cas.reads.load(Ordering::SeqCst), 0, "no row read");
            }
        }
    }
    // Free (no pricing): the cleared server answers 200 on the same shard.
    let f = build(Setup::default());
    for uri in uris(f.shard) {
        assert_eq!(get(&f, &uri, &[]).await.0, StatusCode::OK, "{uri}");
    }
}

/// Review L14-R1: a client cannot step around the fallback 403. HEAD, a
/// payment signed against a cleared server's challenge (sent first, with no
/// 402 seen), a junk payment, a contributor proof, pinned roots and every
/// metric query form all get 403 `label_rights_withheld`; no row is read,
/// nothing is settled, and the error body carries no row, proof or amount.
#[tokio::test(start_paused = true)]
async fn the_fallback_403_cannot_be_skipped() {
    use crate::catalog::LabelRights;
    let setup = |rights| Setup {
        paid: true,
        facilitator: mock(),
        config: ApiConfig {
            rights,
            ..ApiConfig::default()
        },
        ..Setup::default()
    };
    // A real payment, signed for the same shard on a cleared server.
    let cleared = build(setup(LabelRights::Cleared));
    let s = cleared.shard;
    let (st, challenge, _) = get(&cleared, &format!("/v1/proofs/1/{s}/0"), &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    let signed = pay(&challenge, "payer a", 7);
    let f = build(setup(LabelRights::Fallback));
    assert_eq!(f.shard, s, "same shard in both modes");
    let r = cleared.root;
    let uris = [
        format!("/v1/proofs/1/{s}/0"),
        format!("/v1/proofs/1/{s}/0?root={r}"),
        format!("/v1/proofs/1/{s}/999999"),
        format!("/v1/export/1/{s}"),
        format!("/v1/export/1/{s}?root={r}"),
        "/v1/metrics/test.flow?subject=entity:b".to_string(),
        "/v1/metrics/test.flow?subject=entity:nobody".to_string(),
        "/v1/metrics/test.flow?subject=entity:b&as_of=100".to_string(),
        // SOAK-5: older than every shard; still the 403, not a 404 naming
        // the oldest shard (E10 review).
        "/v1/metrics/test.flow?subject=entity:b&as_of=99".to_string(),
        "/v1/metrics/test.flow?subject=entity:b&min_as_of=1".to_string(),
        "/v1/settlement/okx-flow".to_string(),
    ];
    let withheld = |st: StatusCode, b: &Value, case: &str| {
        assert_eq!(
            (st, code(b)),
            (StatusCode::FORBIDDEN, Some("label_rights_withheld")),
            "{case}"
        );
        for k in ["row", "proof", "rows", "value"] {
            assert!(b.get(k).is_none(), "{case}: body has {k}");
        }
    };
    for uri in &uris {
        let heads: [(&str, &[(&str, String)]); 3] = [
            (
                "signed payment",
                &[(x402::PAYMENT_SIGNATURE, signed.clone())],
            ),
            ("junk payment", &[(x402::PAYMENT_SIGNATURE, "x".repeat(40))]),
            ("contributor proof", &[(CONTRIBUTOR_HEADER, "y".repeat(40))]),
        ];
        let (st, hd, b) = get(&f, uri, &[]).await;
        withheld(st, &b, &format!("GET {uri}"));
        assert!(
            !hd.contains_key(x402::PAYMENT_REQUIRED),
            "GET {uri}: no 402 terms"
        );
        for (what, h) in heads {
            let (st, hd, b) = get(&f, uri, h).await;
            withheld(st, &b, &format!("GET {uri} with a {what}"));
            assert!(
                !hd.contains_key(x402::PAYMENT_RESPONSE),
                "{uri} {what}: settled"
            );
        }
        // HEAD answers the same status with no body.
        let (st, _, _) = send(&f, Method::HEAD, uri, &[], None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "HEAD {uri}");
    }
    assert_eq!(f.cas.reads.load(Ordering::SeqCst), 0, "no row read");
    // The payment signed above was never taken: the cleared server still
    // accepts it (one use only).
    let (st, _, b) = get(
        &cleared,
        &format!("/v1/proofs/1/{s}/0"),
        &[(x402::PAYMENT_SIGNATURE, signed)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// ROADMAP L14: what counts as label-derived (fail closed).
#[test]
fn label_derived_covers_label_sets_known_gates_and_entity_metrics() {
    let mut m = meta(1, 100, b"params one", 3);
    assert!(m.label_derived(), "pins a label set");
    m.binding.label_set_hash = Hash32::ZERO;
    assert!(!m.label_derived(), "no label set, plain metric name");
    for name in [
        "btc.entity-flow",
        "btc.entity-reserve",
        "crypto.btc.entity_reserve",
        "btc.Entity-balance",
    ] {
        m.metric = name.into();
        assert!(m.label_derived(), "{name}");
    }
    m.metric = "btc.pool-blocks".into();
    assert!(!m.label_derived());
    // A reserve binding under an innocent metric name: the known gate's
    // schema id still marks it.
    m.binding.schema_id = keccak256(b"onyx.btc.entity-reserve-row.v1");
    assert!(m.label_derived(), "known reserve schema");
    assert_eq!(
        crate::catalog::LabelRights::parse("fallback"),
        Ok(crate::catalog::LabelRights::Fallback)
    );
    for bad in ["", "Fallback", "FALLBACK", "true", "1", " cleared"] {
        assert!(crate::catalog::LabelRights::parse(bad).is_err(), "{bad:?}");
    }
}

// Real time: the root-miss memory runs on the std clock; the test only
// waits past it (5 ms over a 1 ms TTL), which no load can shorten (E12).
#[tokio::test]
async fn unfinalized_or_missing_rows_are_not_served() {
    // A miss is remembered for `root_miss_ttl` (AUD-1-F4); keep it short so
    // the shard's later root is read.
    let f = build(Setup {
        config: ApiConfig {
            root_miss_ttl: Duration::from_millis(1),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let m = meta(1, 100, b"params two", 3);
    let other = m.shard;
    f.state.catalog.write().expect("lock").add_shard(m);
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{other}/0"), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("not_finalized"))
    );
    // A root whose rows this server does not hold: 503, never a guess.
    tokio::time::sleep(Duration::from_millis(5)).await;
    f.roots.insert(
        1,
        other,
        FinalRoot {
            root: keccak256(b"elsewhere"),
            finalized_at: 1,
            finalized_unix: 1,
            binding: None,
            tx: None,
        },
    );
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{other}/0"), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("rows_unavailable"))
    );
}

#[tokio::test(start_paused = true)]
async fn metric_and_settlement_find_the_subject_row() {
    let f = fixture(false, None);
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=100", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["index"], 2);
    assert_eq!(b["subject"], "entity:c");
    assert!(verifies(&b));
    // as_of before any shard: nothing to serve (its own code: SOAK-5 below).
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=99", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("no_shard_at_as_of"))
    );
    let (s, _, b) = get(&f, "/v1/settlement/okx-flow", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["index"], 1);
    assert_eq!(b["value"], format!("0x{}", hex::encode(b"value b........")));
    let as_row = json!({"row": b["row"], "root": b["root"],
        "proof": {"index": b["index"], "leafCount": b["leafCount"], "siblings": b["siblings"]}});
    assert!(verifies(&as_row));
}

/// THREATS SOAK-5: an `as_of` older than every shard of a metric that exists
/// answers `no_shard_at_as_of` and names the oldest `as_of`, not
/// `unknown_metric` (read as a wrong metric name). It is refused before the
/// 402, so nothing is quoted or charged. An unknown metric stays
/// `unknown_metric` at any `as_of`, and the oldest shard's own `as_of` serves.
#[tokio::test(start_paused = true)]
async fn an_as_of_older_than_every_shard_has_its_own_code() {
    let f = fixture(true, mock());
    let (s, h, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=99", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("no_shard_at_as_of"))
    );
    assert_eq!(
        b["error"]["message"],
        "every shard of this metric is newer than as_of; the oldest is as_of 100"
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none(), "no quote");
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=0", &[]).await;
    assert_eq!(code(&b), Some("no_shard_at_as_of"), "{s}");
    for uri in [
        "/v1/metrics/nope?subject=entity:c&as_of=99",
        "/v1/metrics/nope?subject=entity:c",
    ] {
        let (s, _, b) = get(&f, uri, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_metric")),
            "{uri}"
        );
    }
    let (s, h, _) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=100", &[]).await;
    assert_eq!(
        s,
        StatusCode::PAYMENT_REQUIRED,
        "the oldest shard is quoted"
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_some());
}

/// M7-F9: a subject missing from the newest shard is served from the newest
/// shard that has it.
#[tokio::test(start_paused = true)]
async fn metric_falls_back_to_older_shards_for_the_subject() {
    let f = fixture(false, None);
    let rows = vec![row("entity:a", b"newer a........")];
    let root = f.cas.put_rows(&rows).expect("rows");
    let m = meta(2, 200, b"params newer", 1);
    f.roots.insert(
        2,
        m.shard,
        FinalRoot {
            root,
            finalized_at: 500,
            finalized_unix: 1_100,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m);
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!((s, b["round"].clone()), (StatusCode::OK, json!(2)));
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!((s, b["round"].clone()), (StatusCode::OK, json!(1)));
    assert!(verifies(&b));
}

/// M7-F2: every row and settlement response carries the bundle a client
/// checks against its own node; tampering with root or binding is caught.
#[tokio::test(start_paused = true)]
async fn responses_verify_client_side_and_tampering_is_rejected() {
    let b0 = binding(b"params one", 100);
    let f0 = fixture(false, None);
    let binding_hash = b0.binding(f0.root, 3);
    let f = build(Setup {
        chain: true,
        binding: Some(binding_hash),
        ..Setup::default()
    });
    let answer = ChainAnswer {
        round: 1,
        shard: f.shard,
        chain_id: CHAIN,
        contract: CONTRACT,
        root: f.root,
        binding: binding_hash,
        finalized_at: 400,
    };
    let s = f.shard;
    let mut bodies = Vec::new();
    let expects = [
        Expect::Proof {
            round: 1,
            shard: s,
            index: 1,
        },
        Expect::Export { round: 1, shard: s },
        Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:c",
            as_of: None,
            min_as_of: 0,
        },
        Expect::Settlement {
            market: "okx-flow",
            round: 1,
            shard: s,
            subject: "entity:b",
        },
    ];
    for (uri, ex) in [
        format!("/v1/proofs/1/{s}/1"),
        format!("/v1/export/1/{s}"),
        "/v1/metrics/test.flow?subject=entity:c".to_string(),
        "/v1/settlement/okx-flow".to_string(),
    ]
    .into_iter()
    .zip(&expects)
    {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
        assert_eq!(b["verify"]["chainId"], CHAIN, "{uri}");
        assert_eq!(b["verify"]["binding"], binding_hash.to_string(), "{uri}");
        assert_eq!(b["verify"]["tx"], hex0x(&[0x77; 32]), "{uri}");
        let v = verify_response(&b, ex, Some(&answer)).expect(&uri);
        assert!(v.chain_checked);
        assert!(v.to_string().starts_with("chain_checked=true: "));
        let unchecked = verify_response_unchecked(&b, ex).expect(&uri);
        assert!(!unchecked.chain_checked);
        // IDX-R5-3: the text leads with the missing chain check.
        assert!(unchecked
            .to_string()
            .starts_with("chain_checked=false (NOT VERIFIED ON CHAIN"));
        assert_eq!(
            unchecked.chain_call(),
            roots::get_finalized_root_calldata(1, &s)
        );
        bodies.push(b);
    }
    let body = &bodies[0];
    let ex = &expects[0];
    // A different root: the binding commits to it; with the binding
    // recomputed too, the proof no longer verifies.
    let fake = keccak256(b"fake root");
    let mut t = body.clone();
    t["root"] = json!(fake.to_string());
    t["verify"]["root"] = json!(fake.to_string());
    assert_eq!(
        verify_response_unchecked(&t, ex),
        Err(VerifyError::BindingMismatch)
    );
    t["verify"]["binding"] = json!(b0.binding(fake, 3).to_string());
    assert_eq!(
        verify_response_unchecked(&t, ex),
        Err(VerifyError::ProofInvalid)
    );
    // A binding that does not match its inputs.
    let mut t = body.clone();
    t["verify"]["binding"] = json!(keccak256(b"fake binding").to_string());
    assert_eq!(
        verify_response_unchecked(&t, ex),
        Err(VerifyError::BindingMismatch)
    );
    // A changed binding input changes the shard id.
    let mut t = body.clone();
    t["verify"]["bindingInputs"]["recipeId"] = json!(keccak256(b"other recipe").to_string());
    assert_eq!(
        verify_response_unchecked(&t, ex),
        Err(VerifyError::ShardMismatch)
    );
    // Rows and inputs all consistent, but not what the chain finalized, or
    // the chain's answer about another (round, shard) (M7-L41-F4).
    for (field, c) in [
        (
            "round",
            ChainAnswer {
                round: 2,
                ..answer.clone()
            },
        ),
        (
            "shard",
            ChainAnswer {
                shard: keccak256(b"another shard"),
                ..answer.clone()
            },
        ),
        (
            "root",
            ChainAnswer {
                root: keccak256(b"x"),
                ..answer.clone()
            },
        ),
        (
            "binding",
            ChainAnswer {
                binding: keccak256(b"x"),
                ..answer.clone()
            },
        ),
        (
            "chainId",
            ChainAnswer {
                chain_id: 1,
                ..answer.clone()
            },
        ),
        (
            "contract",
            ChainAnswer {
                contract: [0x22; 20],
                ..answer.clone()
            },
        ),
        (
            "finalizedAt",
            ChainAnswer {
                finalized_at: 401,
                ..answer.clone()
            },
        ),
    ] {
        assert_eq!(
            verify_response(body, ex, Some(&c)),
            Err(VerifyError::ChainMismatch(field))
        );
    }
    // A tampered export row.
    let mut t = bodies[1].clone();
    t["rows"][0] = json!(hex0x(&row("entity:a", b"forged.........")));
    assert_eq!(
        verify_response_unchecked(&t, &expects[1]),
        Err(VerifyError::ProofInvalid)
    );
    // Raw eth_call return data decodes into the same answer.
    let mut ret = f.root.0.to_vec();
    ret.extend_from_slice(&binding_hash.0);
    ret.extend_from_slice(&[0u8; 24]);
    ret.extend_from_slice(&400u64.to_be_bytes());
    assert_eq!(
        ChainAnswer::from_return(1, s, CHAIN, CONTRACT, &ret),
        Ok(Some(answer))
    );
}

/// The client's own node, as a fake: `eth_chainId`, and `getFinalizedRoot`
/// at the `finalized` block answering `view` (zero when `None`).
struct ClientChain {
    chain_id: u64,
    view: Option<(Hash32, Hash32, u64)>,
    down: bool,
    calls: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
}

impl roots::JsonRpc for ClientChain {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        self.calls
            .lock()
            .expect("lock")
            .push((method.to_string(), params.clone()));
        let r = if self.down {
            Err("connection refused".to_string())
        } else {
            match method {
                "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain_id))),
                "eth_call" => {
                    let mut ret = vec![0u8; 96];
                    if let Some((root, binding, at)) = self.view {
                        ret[..32].copy_from_slice(&root.0);
                        ret[32..64].copy_from_slice(&binding.0);
                        ret[88..].copy_from_slice(&at.to_be_bytes());
                    }
                    Ok(json!(hex0x(&ret)))
                }
                _ => Err(format!("unexpected {method}")),
            }
        };
        Box::pin(async move { r })
    }
}

/// M7 loop 3 (THREATS IDX-R5-3, RES-10 delivery TODO): the client checks the
/// root on its own node by default. `ClientNode::verify` reads
/// `getFinalizedRoot` at the `finalized` block of the chain it trusts and
/// passes with `chain_checked: true`; a root not final there, another root
/// (a lying operator node), another chain, or a node it cannot read is
/// refused. `verify_response` without an answer now fails closed (it used to
/// pass unchecked); the opt-out is explicit and still says NOT VERIFIED ON
/// CHAIN.
#[tokio::test(start_paused = true)]
async fn the_client_checks_roots_on_its_own_node_by_default() {
    let b0 = binding(b"params one", 100);
    let f0 = fixture(false, None);
    let binding_hash = b0.binding(f0.root, 3);
    let f = build(Setup {
        chain: true,
        binding: Some(binding_hash),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (st, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(st, StatusCode::OK);
    let ex = Expect::Proof {
        round: 1,
        shard: f.shard,
        index: 1,
    };
    let node = |chain_id, view, down| {
        ClientNode::new(
            ClientChain {
                chain_id,
                view,
                down,
                calls: Arc::default(),
            },
            CHAIN,
            CONTRACT,
        )
    };
    let honest = node(CHAIN, Some((f.root, binding_hash, 400)), false);
    let v = honest.verify(&b, &ex).await.expect("verified on chain");
    assert!(v.chain_checked);
    assert!(v.to_string().starts_with("chain_checked=true: "));
    // The read is the client's own, at the finalized block, for this shard
    // (M7L3-R1-4): eth_chainId first, then eth_call to the client's own
    // contract with the `finalized` tag.
    let calls: Arc<std::sync::Mutex<Vec<(String, Value)>>> = Arc::default();
    let chain_of = |contract| {
        ClientNode::new(
            ClientChain {
                chain_id: CHAIN,
                view: Some((f.root, binding_hash, 400)),
                down: false,
                calls: calls.clone(),
            },
            CHAIN,
            contract,
        )
    };
    let read = chain_of(CONTRACT);
    read.verify(&b, &ex).await.expect("verified");
    {
        let c = calls.lock().expect("lock");
        let methods: Vec<&str> = c.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(methods, ["eth_chainId", "eth_call"]);
        assert_eq!(c[1].1[1], json!("finalized"), "{:?}", c[1].1);
        assert_eq!(c[1].1[0]["to"], json!(hex0x(&CONTRACT)));
        assert_eq!(
            c[1].1[0]["data"],
            json!(hex0x(&roots::get_finalized_root_calldata(1, &f.shard)))
        );
    }
    let a = read.answer(1, f.shard).await.expect("read").expect("final");
    assert_eq!(a.root, f.root);
    // A client trusting another contract refuses this response.
    assert_eq!(
        chain_of([0x99; 20]).verify(&b, &ex).await,
        Err(VerifyError::ChainMismatch("contract"))
    );
    // Not final on the client's node.
    assert_eq!(
        node(CHAIN, None, false).verify(&b, &ex).await,
        Err(VerifyError::NotFinalOnChain)
    );
    // ... which can be transient: the text says to retry (M7L3-R1-3).
    assert!(VerifyError::NotFinalOnChain
        .to_string()
        .ends_with("retry later"));
    // The operator's node served a root the client's chain does not have.
    assert_eq!(
        node(
            CHAIN,
            Some((keccak256(b"other root"), binding_hash, 400)),
            false
        )
        .verify(&b, &ex)
        .await,
        Err(VerifyError::ChainMismatch("root"))
    );
    // A client node on another chain, or one it cannot read.
    assert_eq!(
        node(1, Some((f.root, binding_hash, 400)), false)
            .verify(&b, &ex)
            .await,
        Err(VerifyError::ChainMismatch("chainId"))
    );
    assert!(matches!(
        node(CHAIN, None, true).verify(&b, &ex).await,
        Err(VerifyError::ChainUnavailable(_))
    ));
    // Backward compatibility: no answer is no longer a pass.
    assert_eq!(
        verify_response(&b, &ex, None),
        Err(VerifyError::NotFinalOnChain)
    );
    // Errors in the response itself still come first.
    let mut t = b.clone();
    t["verify"]["binding"] = json!(keccak256(b"fake binding").to_string());
    assert_eq!(
        verify_response(&t, &ex, None),
        Err(VerifyError::BindingMismatch)
    );
    // The explicit opt-out keeps the loud text.
    let u = verify_response_unchecked(&b, &ex).expect("unchecked");
    assert!(!u.chain_checked);
    assert!(u
        .to_string()
        .starts_with("chain_checked=false (NOT VERIFIED ON CHAIN"));
}

/// ROADMAP D3: `verify_row` is `verify_response` plus the facts a sector's
/// own check builds on (binding inputs, source reference, the proven row),
/// with the same chain read required: no answer is refused, a tampered
/// answer fails as before, an export has no single row.
#[tokio::test(start_paused = true)]
async fn a_proven_row_carries_the_checked_binding_and_row() {
    let b0 = binding(b"params one", 100);
    let f0 = fixture(false, None);
    let binding_hash = b0.binding(f0.root, 3);
    let f = build(Setup {
        chain: true,
        binding: Some(binding_hash),
        ..Setup::default()
    });
    let (st, _, b) = get(&f, &format!("/v1/proofs/1/{}/1", f.shard), &[]).await;
    assert_eq!(st, StatusCode::OK);
    let ex = Expect::Proof {
        round: 1,
        shard: f.shard,
        index: 1,
    };
    let node = ClientNode::new(
        ClientChain {
            chain_id: CHAIN,
            view: Some((f.root, binding_hash, 400)),
            down: false,
            calls: Arc::default(),
        },
        CHAIN,
        CONTRACT,
    );
    let p = node.verify_row(&b, &ex).await.expect("proven");
    assert!(p.verified().chain_checked);
    assert_eq!(*p.verified(), node.verify(&b, &ex).await.expect("verified"));
    assert_eq!(p.inputs().shard_id(), f.shard);
    assert_eq!(p.inputs().binding(f.root, 3), binding_hash);
    assert_eq!(
        keccak256(&borsh::to_vec(p.source()).expect("src")),
        p.inputs().source_hash
    );
    assert_eq!(p.index(), 1);
    assert_eq!(hex0x(p.row()), b["row"].as_str().expect("row"));
    // ADR-0048: the row's level and ladder against the level the client's
    // own `verifyRowLeveled` returned.
    let v = p.verified();
    assert_eq!(b["proofLevel"], 1);
    let l = client::check_row_ladder(&b, v, onyx_core::ProofLevel::Recomputed).expect("ladder");
    assert_eq!(l.level(), Some(onyx_core::ProofLevel::Recomputed));
    assert_eq!(l.steps[0].evidence, f.root);
    // The chain says level 4: the served level 1 is forged.
    assert!(matches!(
        client::check_row_ladder(&b, v, onyx_core::ProofLevel::BondedClaim),
        Err(VerifyError::Ladder(_))
    ));
    let mut lifted = b.clone();
    lifted["ladder"]["steps"][0]["evidence"] = json!(Hash32([3; 32]).to_string());
    assert!(client::check_row_ladder(&lifted, v, onyx_core::ProofLevel::Recomputed).is_err());
    assert_eq!(verify_row(&b, &ex, None), Err(VerifyError::NotFinalOnChain));
    let mut t = b.clone();
    t["row"] = json!(hex0x(b"another row"));
    assert!(verify_row(&t, &ex, None).is_err());
    let (_, _, e) = get(&f, &format!("/v1/export/1/{}", f.shard), &[]).await;
    let exp = Expect::Export {
        round: 1,
        shard: f.shard,
    };
    assert_eq!(
        verify_row(&e, &exp, None),
        Err(VerifyError::NotRequested("one row, not an export"))
    );
}

/// The server refuses to serve a root whose on-chain binding disagrees with
/// the catalog's inputs (wrong recipe, labels, rights or row count).
#[tokio::test(start_paused = true)]
async fn binding_mismatch_with_the_ledger_is_refused() {
    let f = build(Setup {
        chain: true,
        binding: Some(keccak256(b"some other binding")),
        ..Setup::default()
    });
    let (s, _, b) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::INTERNAL_SERVER_ERROR, Some("binding_mismatch"))
    );
    assert!(b.get("row").is_none());
}

/// A facilitator that is down.
struct Down;
impl Facilitator for Down {
    fn verify<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(async { Err(FacilitatorError::Unavailable("timeout".into())) })
    }
    fn settle<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(async { Err(FacilitatorError::Unavailable("timeout".into())) })
    }
}

/// A facilitator that verifies but whose settlement fails.
struct SettleFails;
impl Facilitator for SettleFails {
    fn verify<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(async {
            Ok(VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer: None,
            })
        })
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        let network = r.payment_requirements.network.clone();
        Box::pin(async move {
            Ok(SettleResponse {
                success: false,
                error_reason: Some("insufficient_funds".into()),
                payer: None,
                transaction: String::new(),
                network,
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn no_data_without_a_settled_payment() {
    for fac in [
        Arc::new(Down) as Arc<dyn Facilitator>,
        Arc::new(SettleFails),
    ] {
        let f = fixture(true, Some(fac));
        let uri = format!("/v1/proofs/1/{}/0", f.shard);
        let (_, h, _) = get(&f, &uri, &[]).await;
        let (s, h2, b) = get(
            &f,
            &uri,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 8))],
        )
        .await;
        assert!(
            s == StatusCode::SERVICE_UNAVAILABLE || s == StatusCode::PAYMENT_REQUIRED,
            "{s}"
        );
        assert!(b.get("row").is_none());
        if s == StatusCode::PAYMENT_REQUIRED {
            let r: SettleResponse =
                decode_header(h2[x402::PAYMENT_RESPONSE].to_str().expect("ascii")).expect("hdr");
            assert!(!r.success);
        }
    }
}

/// A multi-leaf settlement vector for the Solidity verifier
/// (contracts/test/OnyxApiVector.t.sol): 13 rows, so proofs take the odd-node
/// promotion path at several levels. Bless with `ONYX_BLESS=1` only with a
/// Merkle or row layout change.
#[tokio::test(start_paused = true)]
async fn multi_leaf_settlement_vector_matches_the_solidity_vector() {
    let rows: Vec<Vec<u8>> = (0..13)
        .map(|i| {
            row(
                &format!("entity:{i}"),
                format!("value {i:02}......").as_bytes(),
            )
        })
        .collect();
    let f = build(Setup {
        rows: Some(rows),
        ..Setup::default()
    });
    let mut cases = Vec::new();
    for i in [0u64, 5, 11, 12] {
        let id = format!("m{i}");
        f.state.catalog.write().expect("lock").add_market(
            id.clone(),
            Market {
                round: 1,
                shard: f.shard,
                subject: format!("entity:{i}"),
            },
        );
        let (st, _, b) = get(&f, &format!("/v1/settlement/{id}"), &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(b["index"], i);
        let subject = format!("entity:{i}");
        verify_response_unchecked(
            &b,
            &Expect::Settlement {
                market: &id,
                round: 1,
                shard: f.shard,
                subject: &subject,
            },
        )
        .expect("verifies");
        cases.push(json!({
            "root": b["root"],
            "row": b["row"],
            "index": b["index"],
            "leafCount": b["leafCount"],
            "siblings": b["siblings"],
        }));
    }
    let vector = json!({ "cases": cases });
    let path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/api/multi-leaf-vector.json"
    ));
    let text = serde_json::to_string_pretty(&vector).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(&path, &text).expect("write vector");
    }
    let on_disk = std::fs::read_to_string(&path)
        .expect("fixtures/api/multi-leaf-vector.json missing; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "multi-leaf vector drifted");
}

/// Add a finalized shard of `test.flow` to a running fixture.
fn add_flow_shard(f: &Fixture, round: u64, as_of: u64, params: &[u8], rows: &[Vec<u8>]) -> Hash32 {
    let root = f.cas.put_rows(rows).expect("rows");
    let m = meta(round, as_of, params, rows.len() as u64);
    let shard = m.shard;
    f.roots.insert(
        round,
        shard,
        FinalRoot {
            root,
            finalized_at: 500,
            finalized_unix: 1_100,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m);
    shard
}

/// M7-L40-F1: a server that answers with a valid, finalized row of another
/// metric's shard (other params) is caught by the metric's definition.
#[tokio::test(start_paused = true)]
async fn client_refuses_rows_of_another_metric_shard() {
    let f = fixture(false, None);
    add_flow_shard(
        &f,
        2,
        100,
        b"params evil",
        &[row("entity:c", b"evil c.........")],
    );
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!((st, b["round"].clone()), (StatusCode::OK, json!(2)));
    let ex = Expect::Metric {
        metric: "test.flow",
        def: flow_def(b"params one"),
        subject: "entity:c",
        as_of: None,
        min_as_of: 0,
    };
    assert_eq!(
        verify_response_unchecked(&b, &ex),
        Err(VerifyError::NotRequested("params"))
    );
    let other_recipe = Expect::Metric {
        metric: "test.flow",
        def: MetricDef {
            recipe_id: keccak256(b"other recipe"),
            ..flow_def(b"params evil")
        },
        subject: "entity:c",
        as_of: None,
        min_as_of: 0,
    };
    assert_eq!(
        verify_response_unchecked(&b, &other_recipe),
        Err(VerifyError::NotRequested("recipe"))
    );
    let other_source = Expect::Metric {
        metric: "test.flow",
        def: MetricDef {
            source: "eth.node",
            ..flow_def(b"params evil")
        },
        subject: "entity:c",
        as_of: None,
        min_as_of: 0,
    };
    assert_eq!(
        verify_response_unchecked(&b, &other_source),
        Err(VerifyError::NotRequested("source"))
    );
}

/// M7-L40-F1: a shard whose snapshot ends after `as_of` is refused, even
/// when the server ignores `as_of`.
#[tokio::test(start_paused = true)]
async fn client_refuses_a_shard_newer_than_as_of() {
    let f = fixture(false, None);
    add_flow_shard(
        &f,
        2,
        200,
        b"params one",
        &[row("entity:c", b"newer c........")],
    );
    let (_, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!(b["asOf"], 200);
    let ex = |as_of| Expect::Metric {
        metric: "test.flow",
        def: flow_def(b"params one"),
        subject: "entity:c",
        as_of,
        min_as_of: 0,
    };
    assert_eq!(
        verify_response_unchecked(&b, &ex(Some(150))),
        Err(VerifyError::NotRequested("asOf"))
    );
    assert!(verify_response_unchecked(&b, &ex(Some(200))).is_ok());
    // The server's own as_of answer is the older shard, and it verifies.
    let (_, _, old) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=150", &[]).await;
    assert_eq!(old["asOf"], 100);
    assert!(verify_response_unchecked(&old, &ex(Some(150))).is_ok());
}

/// PLAN q14 (a), M7-L41-F1: a lying server answering "latest" with an older
/// finalized shard is refused once the client sets its freshness floor
/// (`min_as_of`); recency beyond that floor is not provable from one answer.
#[tokio::test(start_paused = true)]
async fn client_refuses_a_shard_older_than_min_as_of() {
    let f = fixture(false, None);
    add_flow_shard(
        &f,
        2,
        200,
        b"params one",
        &[row("entity:c", b"newer c........")],
    );
    let ex = |min_as_of| Expect::Metric {
        metric: "test.flow",
        def: flow_def(b"params one"),
        subject: "entity:c",
        as_of: None,
        min_as_of,
    };
    // Honest newest answer passes a floor below it.
    let (_, _, newest) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!(newest["asOf"], 200);
    assert!(verify_response_unchecked(&newest, &ex(150)).is_ok());
    // A stale answer (the older shard, served as if it were "latest") is a
    // valid finalized row, yet the floor refuses it.
    let (_, _, stale) = get(&f, "/v1/metrics/test.flow?subject=entity:c&as_of=150", &[]).await;
    assert_eq!(stale["asOf"], 100);
    assert_eq!(
        verify_response_unchecked(&stale, &ex(150)),
        Err(VerifyError::NotRequested("asOf older than min_as_of"))
    );
    // At or below the stale shard's end, the floor lets it through: the
    // client chose that tolerance.
    assert!(verify_response_unchecked(&stale, &ex(100)).is_ok());
}

/// M7-L40-F1: a market answered from another shard than the one the client
/// knows it settles on is refused.
#[tokio::test(start_paused = true)]
async fn client_refuses_settlement_from_another_shard() {
    let f = fixture(false, None);
    let s = f.shard;
    let other = add_flow_shard(
        &f,
        2,
        101,
        b"params one",
        &[row("entity:b", b"other b........")],
    );
    assert_ne!(other, s, "a later snapshot is another shard");
    f.state.catalog.write().expect("lock").add_market(
        "okx-flow",
        Market {
            round: 2,
            shard: other,
            subject: "entity:b".into(),
        },
    );
    let (st, _, b) = get(&f, "/v1/settlement/okx-flow", &[]).await;
    assert_eq!(st, StatusCode::OK);
    let ex = |round, shard| Expect::Settlement {
        market: "okx-flow",
        round,
        shard,
        subject: "entity:b",
    };
    assert_eq!(
        verify_response_unchecked(&b, &ex(1, s)),
        Err(VerifyError::NotRequested("round"))
    );
    assert_eq!(
        verify_response_unchecked(&b, &ex(2, s)),
        Err(VerifyError::NotRequested("shard"))
    );
    assert!(verify_response_unchecked(&b, &ex(2, other)).is_ok());
}

/// M7-L40-F1: echoed fields a client reads (asOf, license, recipeId) and the
/// pinned source are bound; editing any of them is caught.
#[tokio::test(start_paused = true)]
async fn client_catches_edited_echo_fields() {
    let f = fixture(false, None);
    let (_, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    let ex = Expect::Metric {
        metric: "test.flow",
        def: flow_def(b"params one"),
        subject: "entity:c",
        as_of: None,
        min_as_of: 0,
    };
    assert!(verify_response_unchecked(&b, &ex).is_ok());
    let edit = |k: &str, v: Value| {
        let mut x = b.clone();
        x[k] = v;
        verify_response_unchecked(&x, &ex)
    };
    let m = VerifyError::Malformed;
    assert_eq!(
        edit("asOf", json!(99)),
        Err(m("asOf differs from the snapshot"))
    );
    assert_eq!(
        edit("license", json!("forged")),
        Err(m("license differs from the rights record"))
    );
    assert_eq!(
        edit("recipeId", json!(Hash32::ZERO.to_string())),
        Err(m("recipeId differs from the binding"))
    );
    let forged_source = hex0x(&borsh::to_vec(&source_ref(99)).expect("borsh"));
    let mut x = b.clone();
    x["verify"]["sourceRef"] = json!(forged_source);
    assert_eq!(
        verify_response_unchecked(&x, &ex),
        Err(VerifyError::BindingMismatch)
    );
}

/// Every key of an object, as `prefix.key` for nested ones.
fn keys(v: &Value, prefix: &str, out: &mut Vec<String>) {
    let Some(o) = v.as_object() else { return };
    for (k, inner) in o {
        out.push(k.clone());
        // Bundles are bound as a whole by the binding recompute.
        if matches!(k.as_str(), "proof" | "verify") {
            keys(inner, &format!("{prefix}{k}."), out);
        }
    }
}

/// M7-L40-F1: completeness by construction. Every field of every response
/// is either checked by `verify_response` or listed as informational, and
/// no field is both.
#[tokio::test(start_paused = true)]
async fn every_response_field_is_bound_or_informational() {
    for name in BOUND_FIELDS {
        assert!(
            !INFORMATIONAL_FIELDS.contains(name),
            "{name} is in both lists"
        );
    }
    let f0 = fixture(false, None);
    let bh = binding(b"params one", 100).binding(f0.root, 3);
    let f = build(Setup {
        chain: true,
        binding: Some(bh),
        ..Setup::default()
    });
    let s = f.shard;
    for uri in [
        format!("/v1/proofs/1/{s}/1"),
        format!("/v1/export/1/{s}"),
        "/v1/metrics/test.flow?subject=entity:c".to_string(),
        "/v1/settlement/okx-flow".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
        let mut all = Vec::new();
        keys(&b, "", &mut all);
        assert!(all.len() > 5, "{uri}");
        for k in all {
            assert!(
                BOUND_FIELDS.contains(&k.as_str()) || INFORMATIONAL_FIELDS.contains(&k.as_str()),
                "{uri}: field {k} is neither checked nor listed as informational"
            );
        }
    }
}

/// M7-L40-F2: the /56 and /48 around a /64 get their own wider buckets.
#[test]
fn rate_scopes_cover_the_56_and_48() {
    let k = client_key("2001:db8:7:1234::1".parse().expect("ip"));
    assert_eq!(
        rate_scopes(&k, 2),
        vec![
            ("2001:db8:7:1234::/64".to_string(), 2),
            ("2001:db8:7:1200::/56".to_string(), 8),
            ("2001:db8:7::/48".to_string(), 32),
        ]
    );
    assert_eq!(
        rate_scopes("10.0.0.1", 2),
        vec![("10.0.0.1".to_string(), 2)]
    );
}

/// M7-L40-F2: rotating /64s inside one /56, then /56s inside one /48,
/// runs out of attempts; another /48 is not affected.
#[tokio::test(start_paused = true)]
async fn rotating_ipv6_prefixes_share_the_wider_buckets() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        config: ApiConfig {
            payment_attempts_per_minute: 1,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let bad = [(x402::PAYMENT_SIGNATURE, "!!!".to_string())];
    let try_ip = |ip: String| {
        let (f, uri, bad) = (&f, &uri, &bad);
        async move {
            let (s, _, b) = send(f, Method::GET, uri, bad, Some(ip.parse().expect("ip"))).await;
            (s, code(&b).map(str::to_string))
        }
    };
    // One /56: 4 attempts across 4 different /64s, then 429.
    for n in 0..4 {
        assert_eq!(
            try_ip(format!("2001:db8:0:{n:x}::1")).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        try_ip("2001:db8:0:4::1".into()).await,
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited".into()))
    );
    // One /48: 16 attempts in all; 4 are spent, 12 more across new /56s.
    for n in 1..=12u32 {
        let ip = format!("2001:db8:0:{:x}::1", n << 8);
        assert_eq!(
            try_ip(ip).await.0,
            StatusCode::BAD_REQUEST,
            "/56 number {n}"
        );
    }
    assert_eq!(
        try_ip(format!("2001:db8:0:{:x}::1", 13 << 8)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        try_ip("2001:db8:1::1".into()).await.0,
        StatusCode::BAD_REQUEST
    );
}

/// M7-L40-F2/F3: a payment that is verified but never settled (here a 404
/// for an unknown subject) is charged to the payer's unsettled budget; when
/// it is empty the payer is refused before the facilitator.
#[tokio::test(start_paused = true)]
async fn verified_but_unsettled_payments_use_up_the_payer_budget() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            unsettled_per_hour: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let unknown = "/v1/metrics/test.flow?subject=entity:zz";
    let (_, h, _) = get(&f, unknown, &[]).await;
    for nonce in [31, 32] {
        let (s, _, b) = get(
            &f,
            unknown,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))],
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_subject"))
        );
    }
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 2);
    let known = "/v1/metrics/test.flow?subject=entity:c";
    let (_, h2, _) = get(&f, known, &[]).await;
    let (s, hh, b) = get(
        &f,
        known,
        &[(x402::PAYMENT_SIGNATURE, pay(&h2, "payer a", 33))],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("payer_limited"))
    );
    assert_eq!(hh[header::RETRY_AFTER], "600");
    assert_eq!(
        fac.verifies.load(Ordering::SeqCst),
        2,
        "refused before verify"
    );
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0);
}

/// M7-L40-F4: two identical paid requests at the same time settle once and
/// both get the row.
#[tokio::test(start_paused = true)]
async fn concurrent_identical_payments_settle_once() {
    let fac = counting();
    let slow = Arc::new(Slow {
        inner: fac.clone(),
        settle_ms: Some(50),
    });
    let f = fixture(true, Some(slow));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 41))];
    let ((s1, _, b1), (s2, _, b2)) = tokio::join!(get(&f, &uri, &sig), get(&f, &uri, &sig));
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK), "{b1} {b2}");
    assert_eq!(b1, b2);
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 1);
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
}

/// A test shard like [`meta`] with one change, its source hash and shard id
/// recomputed so the binding stays internally consistent.
fn variant(round: u64, as_of: u64, change: impl FnOnce(&mut ShardMeta)) -> ShardMeta {
    let mut m = meta(round, as_of, b"params one", 1);
    change(&mut m);
    m.binding.source_hash = m.source.hash();
    m.shard = m.binding.shard_id();
    m
}

fn add_meta_shard(f: &Fixture, m: ShardMeta, rows: &[Vec<u8>]) -> Hash32 {
    let root = f.cas.put_rows(rows).expect("rows");
    let shard = m.shard;
    f.roots.insert(
        m.round,
        shard,
        FinalRoot {
            root,
            finalized_at: 500,
            finalized_unix: 1_100,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m);
    shard
}

/// M7-L41-F2: a valid, finalized row of the same recipe and params is still
/// refused when its shard uses another label set (a `test.*` one included),
/// chain, window, runtime or registry than the metric the client means.
#[tokio::test(start_paused = true)]
async fn client_refuses_another_label_set_chain_window_runtime_or_registry() {
    let set_chain = |chain: &str| {
        let chain = chain.to_string();
        move |m: &mut ShardMeta| {
            if let Snapshot::BlockRange { chain: c, .. } = &mut m.source.snapshot {
                *c = ChainId(chain);
            }
        }
    };
    let cases: Vec<(&str, ShardMeta)> = vec![
        (
            "label set",
            variant(2, 300, |m| {
                m.binding.label_set_hash = keccak256(b"test.onyx.entities.btc");
                m.test_labels = true;
            }),
        ),
        ("chain", variant(2, 300, set_chain("eth.mainnet"))),
        (
            "window",
            variant(2, 300, |m| {
                if let Snapshot::BlockRange { from_height, .. } = &mut m.source.snapshot {
                    *from_height = 300; // one block, not three
                }
            }),
        ),
        (
            "runtime",
            variant(2, 300, |m| {
                m.binding.runtime_hash = keccak256(b"other runtime")
            }),
        ),
        (
            "registry",
            variant(2, 300, |m| {
                m.binding.registry_root = keccak256(b"other registry")
            }),
        ),
    ];
    for (what, m) in cases {
        // A server that serves anything, test label sets included.
        let f = build(Setup {
            config: ApiConfig {
                allow_test_labels: true,
                ..ApiConfig::default()
            },
            ..Setup::default()
        });
        add_meta_shard(&f, m, &[row("entity:c", b"other c........")]);
        let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
        assert_eq!(
            (st, b["round"].clone()),
            (StatusCode::OK, json!(2)),
            "{what}"
        );
        let ex = Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:c",
            as_of: None,
            min_as_of: 0,
        };
        assert_eq!(
            verify_response_unchecked(&b, &ex),
            Err(VerifyError::NotRequested(what)),
            "{what}"
        );
    }
}

/// M7-L41-F4: each endpoint's answer may carry only its own keys; an extra
/// field (a `value` on a proof or metric answer, an `asOf` on a settlement,
/// anything inside the bundle or the proof) is refused, not ignored.
#[tokio::test(start_paused = true)]
async fn client_refuses_fields_an_endpoint_never_sends() {
    let f = fixture(false, None);
    let s = f.shard;
    let proof_ex = Expect::Proof {
        round: 1,
        shard: s,
        index: 1,
    };
    let (_, _, proof) = get(&f, &format!("/v1/proofs/1/{s}/1"), &[]).await;
    assert!(verify_response_unchecked(&proof, &proof_ex).is_ok());
    let metric_ex = Expect::Metric {
        metric: "test.flow",
        def: flow_def(b"params one"),
        subject: "entity:c",
        as_of: None,
        min_as_of: 0,
    };
    let (_, _, metric) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert!(verify_response_unchecked(&metric, &metric_ex).is_ok());
    let settle_ex = Expect::Settlement {
        market: "okx-flow",
        round: 1,
        shard: s,
        subject: "entity:b",
    };
    let (_, _, settle) = get(&f, "/v1/settlement/okx-flow", &[]).await;
    assert!(verify_response_unchecked(&settle, &settle_ex).is_ok());
    let cases: Vec<(&Value, &Expect<'_>, Vec<&str>, &str)> = vec![
        (&proof, &proof_ex, vec!["value"], "body.value"),
        (&proof, &proof_ex, vec!["asOf"], "body.asOf"),
        (&metric, &metric_ex, vec!["value"], "body.value"),
        (&settle, &settle_ex, vec!["asOf"], "body.asOf"),
        (&proof, &proof_ex, vec!["verify", "extra"], "verify.extra"),
        (&proof, &proof_ex, vec!["proof", "extra"], "proof.extra"),
        (
            &metric,
            &metric_ex,
            vec!["verify", "bindingInputs", "extra"],
            "verify.bindingInputs.extra",
        ),
    ];
    for (body, ex, path, want) in cases {
        let mut t = body.clone();
        let mut at = &mut t;
        for k in &path[..path.len() - 1] {
            at = &mut at[*k];
        }
        at[path[path.len() - 1]] = json!("0x00");
        assert_eq!(
            verify_response_unchecked(&t, ex),
            Err(VerifyError::UnexpectedField(want.to_string())),
            "{want}"
        );
    }
    // Every key an endpoint may send is either bound or informational.
    for ex in [
        &proof_ex,
        &metric_ex,
        &settle_ex,
        &Expect::Export { round: 1, shard: s },
    ] {
        for k in allowed_keys(ex) {
            assert!(
                BOUND_FIELDS.contains(&k) || INFORMATIONAL_FIELDS.contains(&k),
                "{k}"
            );
        }
    }
}

/// The 20-byte address of a test signer.
fn addr_bytes(label: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    a.copy_from_slice(&hex::decode(addr(label).trim_start_matches("0x")).expect("address hex"));
    a
}

/// M7-L42-F2: a settled header replayed from other addresses is refused by
/// the facilitator (spent nonce) and does not spend the real payer's budget.
#[tokio::test(start_paused = true)]
async fn replays_of_a_settled_header_do_not_drain_the_payers_budget() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            unsettled_per_hour: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 51))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    for i in 0..25u8 {
        let (s, _, b) = send(
            &f,
            Method::GET,
            &uri,
            &sig,
            Some(IpAddr::from([10, 9, 0, i])),
        )
        .await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "replay {i}: {b}");
        assert!(b.get("row").is_none());
    }
    assert_eq!(fac.verifies.load(Ordering::SeqCst), 26);
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
    let payer = addr_bytes("payer a");
    assert!(f.state.caches.payer_allowed(&payer, 2), "budget intact");
    assert_eq!(
        f.state.caches.payers_tracked(),
        0,
        "full budgets are not stored"
    );
}

/// Verifies through `inner`; settle reports a facilitator error.
struct SettleErrors {
    inner: Arc<Counting>,
}
impl Facilitator for SettleErrors {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        _: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(async { Err(FacilitatorError::Unavailable("settle failed".into())) })
    }
}

/// M7-L47-F3 (superseding M7-L42-F1's refund): a settle that fails with a
/// facilitator error is as unknown as one that times out (the transfer may
/// have been sent before the error): the unit stays spent, nothing is stored
/// as a success, and the next payment from that payer is limited.
#[tokio::test(start_paused = true)]
async fn a_failed_or_timed_out_settle_keeps_the_unit_and_stores_no_success() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(SettleErrors { inner: fac.clone() })),
        config: ApiConfig {
            unsettled_per_hour: 1,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, hs, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 61))],
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(b.get("row").is_none(), "no row without a settle");
    assert!(hs.get(x402::PAYMENT_RESPONSE).is_none());
    assert_eq!(f.state.caches.settled_kept(), 0, "not stored as a success");
    let payer = addr_bytes("payer a");
    assert!(
        !f.state.caches.payer_allowed(&payer, 1),
        "a failed settle keeps the unit"
    );
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 62))],
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(b["error"]["code"], "payer_limited");

    let slow = counting();
    let g = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: slow.clone(),
            settle_ms: None,
        })),
        config: ApiConfig {
            unsettled_per_hour: 1,
            facilitator_timeout: Duration::from_millis(30),
            request_timeout: Duration::from_millis(80),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let (_, h, _) = get(&g, &uri, &[]).await;
    let (s, _, _) = get(
        &g,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 67))],
    )
    .await;
    assert_eq!(s, StatusCode::GATEWAY_TIMEOUT);
    // The settle task gives up after 4 facilitator deadlines; then the unit
    // stays spent because the payment may have landed.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !g.state.caches.payer_allowed(&payer, 1),
        "timeout keeps the unit"
    );
}

/// Adds a newer shard (round 2, `as_of` 200) holding `entity:a`, finalized
/// at unix `finalized_unix`.
fn add_newer_shard(f: &Fixture, finalized_unix: u64) -> Hash32 {
    let rows = vec![row("entity:a", b"newer a........")];
    let root = f.cas.put_rows(&rows).expect("rows");
    let m = meta(2, 200, b"params newer", 1);
    let shard = m.shard;
    f.roots.insert(
        2,
        shard,
        FinalRoot {
            root,
            finalized_at: 500,
            finalized_unix,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m);
    shard
}

/// M7-L47-F1: a newer shard that finalizes between the quote and the row
/// lookup is not served at the older shard's price: the lookup starts at the
/// quoted shard. Here the old shard is free and the new one would be paid.
// Real time: the root-miss memory runs on the std clock; the test only
// waits past it (5 ms over a 1 ms TTL), which no load can shorten (E12).
#[tokio::test]
async fn a_shard_finalizing_after_the_quote_is_not_served_at_the_stale_price() {
    let newer = meta(2, 200, b"params newer", 1).shard;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        flip: Some(newer),
        // A miss is remembered for `root_miss_ttl` (AUD-1-F4); keep it short
        // so the second request sees the shard finalize.
        config: ApiConfig {
            root_miss_ttl: Duration::from_millis(1),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    // Round 1 (finalized at unix 1,000) is long past its fresh window; the
    // newer shard finalizes now and would cost the full price.
    let now = 1_000 + 10 * 3_600;
    f.now.store(now, Ordering::SeqCst);
    add_newer_shard(&f, now);
    let (s, h, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    assert_eq!(b["round"], json!(1), "served the priced (free) shard only");
    assert_eq!(b["row"], json!(hex0x(&f.rows[0])));
    // Once finalized (and the miss expired), the newer shard is quoted, and
    // paid for.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (s, _, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
}

/// M7-L47-F5: `min_as_of` is part of the canonical URL, and a floor above
/// the newest finalized shard is a 404 before any 402 (nothing charged).
#[tokio::test(start_paused = true)]
async fn min_as_of_floor_is_canonical_and_404s_before_any_charge() {
    let f = fixture(true, mock());
    let (st, h, _) = get(
        &f,
        "/v1/metrics/test.flow?min_as_of=100&subject=entity:c",
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        h[header::LOCATION],
        "/v1/metrics/test.flow?subject=entity:c&min_as_of=100"
    );
    let (st, h, b) = get(
        &f,
        "/v1/metrics/test.flow?subject=entity:c&min_as_of=101",
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none(), "no 402, no charge");
    let (st, _, _) = get(
        &f,
        "/v1/metrics/test.flow?subject=entity:c&min_as_of=100",
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "the floor is met");
    // A newer shard that is not finalized yet does not meet the floor.
    let newer = meta(2, 200, b"params newer", 1);
    f.state.catalog.write().expect("lock").add_shard(newer);
    let (st, h, b) = get(
        &f,
        "/v1/metrics/test.flow?subject=entity:a&min_as_of=150",
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(code(&b), Some("not_finalized"));
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
}

/// M7-L47-F2: exports are capped in bytes as well as rows. Free exports and
/// paid exports whose rows are already in memory get 413 before any 402; a
/// paid export found too large only after verify settles nothing and gives
/// the payer's unit back.
#[tokio::test(start_paused = true)]
async fn exports_are_capped_in_bytes() {
    let small = ApiConfig {
        export_max_bytes: 1_024,
        ..ApiConfig::default()
    };
    let free = build(Setup {
        config: small.clone(),
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", free.shard);
    let (s, _, b) = get(&free, &uri, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("export_too_large"))
    );
    let (s, _, _) = get(
        &fixture(false, None),
        &format!("/v1/export/1/{}", free.shard),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "the default cap fits a small shard");

    let fac = counting();
    let paid = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            unsettled_per_hour: 1,
            ..small
        },
        ..Setup::default()
    });
    let reads = paid.cas.reads.load(Ordering::SeqCst);
    let (s, h, _) = get(&paid, &uri, &[]).await;
    assert_eq!(
        s,
        StatusCode::PAYMENT_REQUIRED,
        "rows not read before payment"
    );
    assert_eq!(paid.cas.reads.load(Ordering::SeqCst), reads);
    let (s, _, b) = get(
        &paid,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 81))],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("export_too_large"))
    );
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0, "nothing settled");
    assert!(
        paid.state.caches.payer_allowed(&addr_bytes("payer a"), 1),
        "unit given back"
    );
    // The rows are in memory now: the next request is refused before the 402.
    let (s, h, _) = get(&paid, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
}

/// M7-L47-F2: free exports have their own per-client budget.
#[tokio::test(start_paused = true)]
async fn free_exports_are_rate_limited_per_client() {
    let f = build(Setup {
        config: ApiConfig {
            free_exports_per_minute: 2,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", f.shard);
    let a: IpAddr = "203.0.113.7".parse().expect("ip");
    let b: IpAddr = "203.0.113.8".parse().expect("ip");
    for _ in 0..2 {
        let (s, _, _) = send(&f, Method::GET, &uri, &[], Some(a)).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, h, body) = send(&f, Method::GET, &uri, &[], Some(a)).await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.get("retry-after").is_some());
    let (s, _, _) = send(&f, Method::GET, &uri, &[], Some(b)).await;
    assert_eq!(s, StatusCode::OK, "another client has its own budget");
    // Single rows are not limited by the export budget.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &format!("/v1/proofs/1/{}/0", f.shard),
        &[],
        Some(a),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

async fn raw(f: &Fixture, uri: &str) -> Response {
    let req = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    router(f.state.clone())
        .oneshot(req)
        .await
        .expect("response")
}

/// M7-L47-F2: a response holds its in-flight permit until its body is
/// written (or dropped), and at most `send_timeout`; a body asked for after
/// the send timeout fails, which closes the connection.
///
/// E10: the clock is paused, so time moves only when this test sleeps (and
/// then straight to the next timer). Under load the real clock could run the
/// 100 ms send timeout out between two steps; here the permit must still be
/// held 1 ms before the deadline and be back 1 ms after it.
#[tokio::test(start_paused = true)]
async fn the_permit_lasts_until_the_body_is_written_or_the_send_timeout() {
    let f = build(Setup {
        config: ApiConfig {
            max_concurrency: 1,
            send_timeout: Duration::from_millis(100),
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", f.shard);
    let unread = raw(&f, &uri).await;
    assert_eq!(unread.status(), StatusCode::OK);
    let (s, _, b) = get(&f, "/v1/health", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("overloaded"))
    );
    drop(unread);
    let (s, _, _) = get(&f, "/v1/health", &[]).await;
    assert_eq!(s, StatusCode::OK, "a dropped body gives the permit back");

    let stalled = raw(&f, &uri).await;
    assert_eq!(
        stalled.status(),
        StatusCode::OK,
        "a written body gave its permit back"
    );
    tokio::time::sleep(Duration::from_millis(99)).await;
    let (s, _, b) = get(&f, "/v1/health", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("overloaded")),
        "a stalled body holds its permit until the send timeout"
    );
    tokio::time::sleep(Duration::from_millis(2)).await;
    let (s, _, _) = get(&f, "/v1/health", &[]).await;
    assert_eq!(s, StatusCode::OK, "the send timeout gives the permit back");
    let got = stalled.into_body().collect().await;
    assert!(got.is_err(), "a body written after the send timeout fails");
}

/// M7-L47-F4: a server refuses a payment network outside the testnet
/// allowlist unless `allow_mainnet` is set.
#[test]
fn a_mainnet_payment_network_is_refused_without_the_switch() {
    let state = |allow_mainnet: bool| {
        let payments = Payments {
            terms: PaymentTerms {
                network: "eip155:8453".into(),
                asset: "0x0000000000000000000000000000000000000001".into(),
                pay_to: PAY_TO.into(),
                max_timeout_seconds: 60,
                extra: None,
            },
            facilitator: Arc::new(MockFacilitator::new([addr("payer a")])),
        };
        ApiState::new(
            Catalog::new(),
            Arc::new(LedgerRoots::new()),
            Arc::new(MemCas::new()),
            Some(payments),
            Arc::new(|| 0),
            "https://api.onyx.test",
            ApiConfig {
                allow_mainnet,
                ..ApiConfig::default()
            },
        )
    };
    let err = state(false).err().expect("refused");
    assert!(err.0.contains("--allow-mainnet"), "{err}");
    assert!(state(true).is_ok());
}

/// Adds a `test.flow` shard with `rows`, finalized at unix `finalized_unix`
/// (`None`: in the catalog but not finalized).
fn add_shard_at(
    f: &Fixture,
    round: u64,
    as_of: u64,
    params: &[u8],
    finalized_unix: Option<u64>,
    rows: &[Vec<u8>],
) -> ShardMeta {
    let root = f.cas.put_rows(rows).expect("rows");
    let m = meta(round, as_of, params, rows.len() as u64);
    if let Some(finalized_unix) = finalized_unix {
        f.roots.insert(
            round,
            m.shard,
            FinalRoot {
                root,
                finalized_at: 500,
                finalized_unix,
                binding: None,
                tx: None,
            },
        );
    }
    f.state.catalog.write().expect("lock").add_shard(m.clone());
    m
}

fn amount(challenge: &HeaderMap) -> u64 {
    let pr: PaymentRequired =
        decode_header(challenge[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("decodes");
    pr.accepts[0].amount.parse().expect("amount")
}

/// M7-L48-F1: shard A (newest `as_of`, finalized long ago, free) lacks the
/// subject; shard B (older `as_of`, finalized after A, still fresh) has it.
/// The quote comes from A (free), so B is never served for it: 404, no
/// charge. B's row is reached, and paid for, with B's own `as_of`.
#[tokio::test(start_paused = true)]
async fn a_dearer_older_shard_is_not_served_at_a_free_quote() {
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        ..Setup::default()
    });
    let now = 1_000 + 10 * 3_600;
    f.now.store(now, Ordering::SeqCst);
    add_shard_at(
        &f,
        2,
        200,
        b"shard a",
        Some(1_000),
        &[row("entity:y", b"a y............")],
    );
    add_shard_at(
        &f,
        3,
        150,
        b"shard b",
        Some(now - 60),
        &[row("entity:x", b"b x............")],
    );
    let (s, h, b) = get(&f, "/v1/metrics/test.flow?subject=entity:x", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_subject")),
        "{b}"
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:y", &[]).await;
    assert_eq!(
        (s, b["round"].clone()),
        (StatusCode::OK, json!(2)),
        "A is free"
    );
    let uri = "/v1/metrics/test.flow?subject=entity:x&as_of=150";
    let (s, h, _) = get(&f, uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(amount(&h), 1_000);
    let (s, _, b) = get(&f, uri, &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))]).await;
    assert_eq!((s, b["round"].clone()), (StatusCode::OK, json!(3)), "{b}");
    assert_eq!(fac.settles.load(Ordering::SeqCst), 1);
}

/// Seeded xorshift64 (tests only; no `rand` dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// (round, as_of, params, finalized_unix, has the subject)
type RandomShard = (u64, u64, Vec<u8>, Option<u64>, bool);

/// M7-L48-F1 invariant: whatever the shards (any `as_of` and finalization
/// order, subjects present or missing, a shard finalizing between the quote
/// and the lookup), a served metric row costs, at serve time, exactly what
/// was paid for it (0 when served without payment), never more.
///
/// M7-L50-F1: the clock also moves between the 402 and the paid retry,
/// often past the fresh window (3,600 s), and every status is its own
/// outcome: an unexpected one fails the test instead of counting as
/// "missing".
#[tokio::test(start_paused = true)]
async fn a_served_metric_row_never_costs_more_than_was_paid() {
    let mut rng = Rng(0x0e1c_5eed_4848_0050);
    // Outcomes: served free; 404 before paying; paid and served; paid
    // retry served free because the data decayed meanwhile; 404 after the
    // payment (the price says nothing about the subject).
    let (mut free, mut absent, mut paid, mut decayed, mut absent_paid) = (0, 0, 0, 0, 0);
    for case in 0..400u64 {
        let now = 1_000 + rng.below(20_000);
        let extra = 1 + rng.below(5);
        let specs: Vec<RandomShard> = (0..extra)
            .map(|k| {
                let round = 2 + k;
                let as_of = 50 + rng.below(250);
                let params = format!("case {case} round {round}").into_bytes();
                let fin = (rng.below(5) != 0).then(|| rng.below(now + 1));
                (round, as_of, params, fin, rng.below(2) == 0)
            })
            .collect();
        let flip = (rng.below(2) == 0).then(|| {
            let (_, as_of, p, _, _) = &specs[rng.below(extra) as usize];
            meta(0, *as_of, p, 1).shard
        });
        // A counting facilitator: a settle must show up as a payment, never
        // hide behind a free row or a 404 (M7-L51-F1).
        let fac = counting();
        let f = build(Setup {
            paid: true,
            facilitator: Some(fac.clone()),
            flip,
            ..Setup::default()
        });
        f.now.store(now, Ordering::SeqCst);
        // Round 1 (`as_of` 100, finalized at unix 1,000) holds entity:a..c.
        let mut fin_of = vec![(1u64, 100u64, b"params one".to_vec(), Some(1_000u64))];
        for (round, as_of, params, fin, has) in &specs {
            let mut rows = vec![row("entity:y", b"filler.........")];
            if *has {
                rows.push(row("entity:x", b"subject........"));
            }
            add_shard_at(&f, *round, *as_of, params, *fin, &rows);
            fin_of.push((*round, *as_of, params.clone(), *fin));
        }
        let subject = if rng.below(4) == 0 {
            "entity:a"
        } else {
            "entity:x"
        };
        let uri = match rng.below(3) {
            0 => format!(
                "/v1/metrics/test.flow?subject={subject}&as_of={}",
                50 + rng.below(250)
            ),
            _ => format!("/v1/metrics/test.flow?subject={subject}"),
        };
        let cost = |b: &Value, at: u64| -> u64 {
            let round = b["round"].as_u64().expect("round");
            let (_, as_of, params, fin) =
                fin_of.iter().find(|s| s.0 == round).expect("served round");
            let m = meta(round, *as_of, params, 1);
            f.state.catalog.read().expect("lock").price(
                &m,
                fin.expect("served shards are finalized"),
                at,
                1,
            )
        };
        let (s, h, b) = get(&f, &uri, &[]).await;
        match s {
            StatusCode::OK => {
                assert_eq!(
                    cost(&b, now),
                    0,
                    "case {case}: served free but priced: {uri}"
                );
                free += 1;
            }
            StatusCode::NOT_FOUND => absent += 1,
            StatusCode::PAYMENT_REQUIRED => {
                // Half the time the payer comes back later, up to two
                // fresh windows later.
                let later = now + (rng.below(2) * rng.below(7_200));
                f.now.store(later, Ordering::SeqCst);
                let (s, h2, b) = get(
                    &f,
                    &uri,
                    &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
                )
                .await;
                let settled = h2.contains_key(x402::PAYMENT_RESPONSE);
                let settles = fac.settles.load(Ordering::SeqCst);
                assert_eq!(
                    settles,
                    u64::from(settled),
                    "case {case}: settled {settles} time(s), header says {settled}: {uri}"
                );
                match s {
                    StatusCode::OK => {
                        let c = cost(&b, later);
                        if settled {
                            // Paid exactly what the row costs at serve time.
                            assert_eq!(
                                c,
                                amount(&h),
                                "case {case}: served at another price: {uri}"
                            );
                            paid += 1;
                        } else {
                            // The quote became free (decay, or a newer free
                            // shard): the header is ignored, nothing paid.
                            assert_eq!(c, 0, "case {case}: served unpaid but priced: {uri}");
                            decayed += 1;
                        }
                    }
                    StatusCode::NOT_FOUND => {
                        assert!(!settled, "case {case}: settled a 404: {uri}");
                        absent_paid += 1;
                    }
                    other => panic!("case {case}: paid retry gave {other}: {uri}"),
                }
            }
            other => panic!("case {case}: first request gave {other}: {uri}"),
        }
    }
    // The cases reach every outcome (the property is not vacuous).
    assert!(
        free > 20 && absent > 20 && paid > 20 && decayed > 5 && absent_paid > 5,
        "{free} {absent} {paid} {decayed} {absent_paid}"
    );
}

/// M7-L48-F2: `X-Forwarded-For` is believed only from a trusted proxy, and
/// then the right-most address outside the trusted ranges is the client.
#[test]
fn client_ip_takes_the_right_most_untrusted_forwarded_address() {
    let ip = |s: &str| -> IpAddr { s.parse().expect("ip") };
    let trusted: Vec<Cidr> = ["10.0.0.0/8", "2001:db8:f::/48", "192.0.2.1"]
        .iter()
        .map(|c| c.parse().expect("cidr"))
        .collect();
    // An untrusted peer: the header is ignored.
    assert_eq!(
        client_ip(ip("203.0.113.9"), &["198.51.100.1"], &trusted),
        ip("203.0.113.9")
    );
    // A trusted peer: right-most untrusted entry, skipping trusted hops.
    assert_eq!(
        client_ip(
            ip("10.1.2.3"),
            &["6.6.6.6, 198.51.100.1, 10.9.9.9"],
            &trusted
        ),
        ip("198.51.100.1")
    );
    assert_eq!(
        client_ip(
            ip("::ffff:10.1.2.3"),
            &["6.6.6.6", "198.51.100.2"],
            &trusted
        ),
        ip("198.51.100.2"),
        "several headers in order; IPv4-mapped peers count as IPv4"
    );
    assert_eq!(
        client_ip(ip("2001:db8:f::1"), &["2001:db8:1::5"], &trusted),
        ip("2001:db8:1::5")
    );
    // No header, only trusted hops, or junk: the peer.
    assert_eq!(client_ip(ip("10.1.2.3"), &[], &trusted), ip("10.1.2.3"));
    assert_eq!(
        client_ip(ip("192.0.2.1"), &["10.0.0.1"], &trusted),
        ip("192.0.2.1")
    );
    assert_eq!(
        client_ip(ip("10.1.2.3"), &["6.6.6.6, junk"], &trusted),
        ip("10.1.2.3")
    );
    // No trusted proxies (the default): never the header.
    assert_eq!(client_ip(ip("10.1.2.3"), &["6.6.6.6"], &[]), ip("10.1.2.3"));
    // AUD-2-F8: a /0 range would trust every peer, so it is refused too.
    for bad in [
        "10.0.0.0/33",
        "2001:db8::/129",
        "nope",
        "10.0.0.0/x",
        "0.0.0.0/0",
        "::/0",
    ] {
        assert!(bad.parse::<Cidr>().is_err(), "{bad}");
    }
    let wide: Cidr = "0.0.0.0/1".parse().expect("cidr");
    assert!(wide.contains(ip("8.8.8.8")) && !wide.contains(ip("::1")));
}

/// M7-L50-F4: proxies that append the peer's socket address (`ip:port`,
/// `[v6]:port`) still give each client its own key, not the proxy's.
#[test]
fn forwarded_entries_with_ports_name_the_client() {
    let ip = |s: &str| -> IpAddr { s.parse().expect("ip") };
    let trusted: Vec<Cidr> = ["10.0.0.0/8"]
        .iter()
        .map(|c| c.parse().expect("cidr"))
        .collect();
    let proxy = ip("10.1.2.3");
    for (hop, want) in [
        ("198.51.100.7:51234", "198.51.100.7"),
        ("[2001:db8:1::5]:443", "2001:db8:1::5"),
        ("[2001:db8:1::6]", "2001:db8:1::6"),
        ("198.51.100.8", "198.51.100.8"),
    ] {
        assert_eq!(client_ip(proxy, &[hop], &trusted), ip(want), "{hop}");
    }
    // A trusted hop with a port is still skipped.
    assert_eq!(
        client_ip(proxy, &["198.51.100.9, 10.9.9.9:8080"], &trusted),
        ip("198.51.100.9")
    );
    // Malformed entries still fall back to the peer.
    for bad in [
        "198.51.100.7:",
        "198.51.100.7:99999",
        "[2001:db8::1",
        "2001:db8::1]:443",
        "[198.51.100.7]",
        "host.example:80",
    ] {
        assert_eq!(client_ip(proxy, &[bad], &trusted), proxy, "{bad}");
    }
}

/// M7-L48-F2 end to end: a spoofed `X-Forwarded-For` from an untrusted peer
/// does not buy fresh rate-limit buckets; behind a trusted proxy each
/// forwarded client has its own.
#[tokio::test(start_paused = true)]
async fn forwarded_for_is_believed_only_from_trusted_proxies() {
    let f = build(Setup {
        config: ApiConfig {
            free_exports_per_minute: 2,
            trusted_proxies: vec!["10.0.0.0/8".parse().expect("cidr")],
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", f.shard);
    let xff = |a: &str| vec![("x-forwarded-for", a.to_string())];
    let direct: IpAddr = "203.0.113.7".parse().expect("ip");
    for spoof in ["198.51.100.1", "198.51.100.2"] {
        let (s, _, _) = send(&f, Method::GET, &uri, &xff(spoof), Some(direct)).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _, _) = send(&f, Method::GET, &uri, &xff("198.51.100.3"), Some(direct)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "spoofed header ignored");
    let proxy: IpAddr = "10.0.0.2".parse().expect("ip");
    for _ in 0..2 {
        let (s, _, _) = send(
            &f,
            Method::GET,
            &uri,
            &xff("6.6.6.6, 198.51.100.4"),
            Some(proxy),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &xff("7.7.7.7, 198.51.100.4"),
        Some(proxy),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "keyed by the right-most untrusted hop"
    );
    let (s, _, _) = send(&f, Method::GET, &uri, &xff("198.51.100.5"), Some(proxy)).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "another forwarded client has its own budget"
    );
}

// ------------------------------------------------------------------ money loop (ADR-0025)

fn sk(label: &str) -> secp256k1::SecretKey {
    secp256k1::SecretKey::from_slice(&secret(label)).expect("key")
}

fn eth(label: &str) -> [u8; 20] {
    exact_evm::address(&secret(label)).expect("key")
}

/// A server where "miner m" did accepted work at unix 1,400 (30-day window).
fn contributor_fixture(fac: Option<Arc<dyn Facilitator>>, per_minute: u32) -> Fixture {
    let src = Arc::new(StaticContributors::new(30 * 86_400));
    src.insert(eth("miner m"), 1_400);
    let mut policy = ContributorPolicy::new(src);
    policy.reads_per_minute = per_minute;
    build(Setup {
        paid: true,
        facilitator: fac,
        contributors: Some(policy),
        ..Setup::default()
    })
}

/// ADR-0025 item 3: a contributor reads fresh data free with a one-time proof
/// for that exact URL; the answer is private and nothing is settled.
#[tokio::test(start_paused = true)]
async fn contributors_read_fresh_data_free_with_a_one_time_proof() {
    let fac = counting();
    let f = contributor_fixture(Some(fac.clone()), 60);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let proof = crate::contrib::sign(&sk("miner m"), &resource, 1_500, "off-chain");
    let (s, h, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, proof.clone())]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(verifies(&b));
    assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(cc(&h), "private, no-cache", "never cached for others");
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.state.receipts.pending(),
        0,
        "a free read earns no revenue"
    );
    // The same proof again: refused (one use).
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, proof)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, Some("invalid_contributor_proof"))
    );
    // HEAD never uses a proof (its body would be stripped).
    let fresh = crate::contrib::sign(&sk("miner m"), &resource, 1_499, "off-chain");
    let (s, _, b) = send(
        &f,
        Method::HEAD,
        &uri,
        &[(CONTRIBUTOR_HEADER, fresh.clone())],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b, Value::Null, "HEAD has no body");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, fresh)]).await;
    assert_eq!(s, StatusCode::OK, "the proof was not used by the HEAD");
    // A proof for another row is refused here.
    let other = crate::contrib::sign(&sk("miner m"), &format!("{resource}x"), 1_500, "off-chain");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, other)]).await;
    assert_eq!(code(&b), Some("invalid_contributor_proof"));
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Someone with no accepted work: refused (they pay instead).
    let stranger = crate::contrib::sign(&sk("stranger"), &resource, 1_500, "off-chain");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, stranger)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_a_contributor"))
    );
    // (The pass lapsing after its window: `contrib::tests`.)
}

/// ADR-0025 item 3: free readers cannot drain the service; each address has
/// its own read budget, whatever IP it comes from.
#[tokio::test(start_paused = true)]
async fn contributor_reads_are_rate_limited_per_address() {
    let f = contributor_fixture(mock(), 1);
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let p1 = crate::contrib::sign(&sk("miner m"), &resource, 1_500, "off-chain");
    let p2 = crate::contrib::sign(&sk("miner m"), &resource, 1_501, "off-chain");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p1)]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p2)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.contains_key("retry-after"));
}

/// Counts how often the contributor source is asked (REV-L1-F1).
struct CountingSource {
    inner: StaticContributors,
    asked: AtomicU64,
}

impl crate::contrib::ContributorSource for CountingSource {
    fn is_contributor<'a>(
        &'a self,
        who: [u8; 20],
        now_unix: u64,
    ) -> x402::BoxFuture<'a, Result<bool, crate::roots::RootError>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.is_contributor(who, now_unix)
    }
}

/// REV-L1-F1: requests signed by throwaway keys (free to make, no accepted
/// work) are charged to the sending client's own proof-check budget before the
/// source is asked; they never enter the contributor budget table, so a
/// contributor's spent budget is never evicted and refilled by churn. A
/// confirmed contributor is not looked up again on every read.
#[tokio::test(start_paused = true)]
async fn throwaway_keys_never_refill_a_contributor_budget() {
    let inner = StaticContributors::new(30 * 86_400);
    inner.insert(eth("miner m"), 1_400);
    let src = Arc::new(CountingSource {
        inner,
        asked: AtomicU64::new(0),
    });
    let mut policy = ContributorPolicy::new(src.clone());
    policy.reads_per_minute = 2;
    policy.checks_per_minute = 8;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let miner = |t: u64| crate::contrib::sign(&sk("miner m"), &resource, t, "off-chain");
    let ip: Option<IpAddr> = Some("198.51.100.7".parse().expect("ip"));
    let other: Option<IpAddr> = Some("203.0.113.9".parse().expect("ip"));
    for t in [1_500, 1_501] {
        let (s, _, _) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, miner(t))], ip).await;
        assert_eq!(s, StatusCode::OK);
    }
    assert_eq!(
        src.asked.load(Ordering::SeqCst),
        1,
        "status cached after the first read"
    );
    let (s, _, b) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, miner(1_502))],
        ip,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    // Another client floods throwaway keys: 403 until its own check budget
    // is spent, then 429; the source is asked at most that budget.
    let mut forbidden = 0;
    for i in 0..50u64 {
        let junk = crate::contrib::sign(&sk(&format!("junk {i}")), &resource, 1_500, "off-chain");
        let (s, _, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, junk)], other).await;
        match (s, code(&b)) {
            (StatusCode::FORBIDDEN, Some("not_a_contributor")) => forbidden += 1,
            (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")) => {}
            x => panic!("junk {i}: {x:?}"),
        }
    }
    assert_eq!(forbidden, 8, "the flooder's own budget");
    assert!(src.asked.load(Ordering::SeqCst) <= 9);
    assert_eq!(
        f.state.caches.contributors_tracked(),
        1,
        "junk never enters the table"
    );
    // The miner's budget is still spent.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, miner(1_503))],
        ip,
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// REV-L2-F1: a used proof is refused before any budget is charged, so
/// whoever holds one (it is a bearer token) cannot replay it from other IPs to
/// spend the contributor's reads and lock them out.
#[tokio::test(start_paused = true)]
async fn replaying_a_used_proof_costs_its_signer_nothing() {
    let f = contributor_fixture(mock(), 2);
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let p = |t: u64| crate::contrib::sign(&sk("miner m"), &resource, t, "off-chain");
    let miner: Option<IpAddr> = Some("198.51.100.7".parse().expect("ip"));
    let used = p(1_500);
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, used.clone())],
        miner,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for i in 0..5u8 {
        let ip: Option<IpAddr> = Some(format!("203.0.113.{i}").parse().expect("ip"));
        let (s, _, b) = send(
            &f,
            Method::GET,
            &uri,
            &[(CONTRIBUTOR_HEADER, used.clone())],
            ip,
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, Some("invalid_contributor_proof")),
            "replay {i}"
        );
    }
    // The miner's second read of the minute is still there.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, p(1_501))],
        miner,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "replays spent none of the miner's reads");
}

/// REV-L2-F2: contributor-source load has a ceiling. A "not a contributor"
/// answer is reused for a minute (one junk key re-signed from many IPs asks
/// the source once), and lookups for new addresses share one server-wide
/// budget (503 `contributor_checks_busy` beyond), however many IPs send them.
#[tokio::test(start_paused = true)]
async fn non_contributors_are_remembered_and_source_lookups_are_capped() {
    let src = Arc::new(CountingSource {
        inner: StaticContributors::new(30 * 86_400),
        asked: AtomicU64::new(0),
    });
    let mut policy = ContributorPolicy::new(src.clone());
    policy.source_calls_per_minute = 5;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let junk = sk("junk");
    for ip_i in 0..10u8 {
        let ip: Option<IpAddr> = Some(format!("192.0.2.{ip_i}").parse().expect("ip"));
        for t in 0..20u64 {
            let h = crate::contrib::sign(&junk, &resource, 1_400 + t, "off-chain");
            let (s, _, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, h)], ip).await;
            assert_eq!(
                (s, code(&b)),
                (StatusCode::FORBIDDEN, Some("not_a_contributor"))
            );
        }
    }
    assert_eq!(src.asked.load(Ordering::SeqCst), 1, "answer reused");
    // New keys from new IPs: 4 more lookups fit the budget, then 503.
    let (mut forbidden, mut busy) = (0, 0);
    for i in 0..20u8 {
        let ip: Option<IpAddr> = Some(format!("198.18.0.{i}").parse().expect("ip"));
        let h = crate::contrib::sign(&sk(&format!("key {i}")), &resource, 1_500, "off-chain");
        let (s, h, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, h)], ip).await;
        match (s, code(&b)) {
            (StatusCode::FORBIDDEN, Some("not_a_contributor")) => forbidden += 1,
            (StatusCode::SERVICE_UNAVAILABLE, Some("contributor_checks_busy")) => {
                assert!(h.contains_key("retry-after"));
                busy += 1;
            }
            x => panic!("key {i}: {x:?}"),
        }
    }
    assert_eq!((forbidden, busy), (4, 16));
    assert_eq!(
        src.asked.load(Ordering::SeqCst),
        5,
        "the server-wide budget"
    );
}

/// REV-L1-F5: a contributor proof is a bearer token, so it never buys a whole
/// shard (the export is paid; the proof is not used), and an address's free
/// reads are capped in rows per day as well as per minute.
#[tokio::test(start_paused = true)]
async fn contributor_proofs_never_buy_an_export_and_rows_per_day_are_capped() {
    let src = Arc::new(StaticContributors::new(30 * 86_400));
    src.insert(eth("miner m"), 1_400);
    let mut policy = ContributorPolicy::new(src);
    policy.rows_per_day = 1;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let export = format!("/v1/export/1/{}", f.shard);
    let res = format!("https://api.onyx.test{export}");
    let proof = crate::contrib::sign(&sk("miner m"), &res, 1_500, "off-chain");
    let (s, h, _) = get(&f, &export, &[(CONTRIBUTOR_HEADER, proof)]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "exports are always paid");
    assert!(h.contains_key(x402::PAYMENT_REQUIRED));
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let res = format!("https://api.onyx.test{uri}");
    let p = |t: u64| crate::contrib::sign(&sk("miner m"), &res, t, "off-chain");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p(1_500))]).await;
    assert_eq!(s, StatusCode::OK, "the export did not use the day's row");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p(1_501))]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

/// ADR-0025 item 2: each settled payment leaves one receipt naming the shard
/// served, the payer and the EIP-3009 nonce; a replayed answer leaves none.
#[tokio::test(start_paused = true)]
async fn settled_payments_leave_one_receipt_for_the_served_shard() {
    let f = fixture(true, mock());
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = pay(&h, "payer a", 9);
    let (s, _, _) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig.clone())]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(s, StatusCode::OK);
    let got = f.state.receipts.drain(10);
    assert_eq!(
        got,
        vec![crate::receipts::Receipt {
            round: 1,
            shard: f.shard,
            amount: 1_000,
            payer: eth("payer a"),
            nonce: [9; 32],
        }]
    );
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["payTo"], PAY_TO);
    assert_eq!(health["receiptsPending"], 0);
    assert_eq!(health["receiptLog"], "memory");
    assert_eq!(health["receiptWriteErrors"], 0);
}

/// REV-11: with a receipt journal, the receipt of a paid answer is on disk
/// when the answer returns; a restarted server (a new state on the same file)
/// still holds it for the reporter.
#[tokio::test(start_paused = true)]
async fn a_paid_answers_receipt_is_on_disk_before_the_answer_returns() {
    let d = crate::journal::test_dir("api-receipts");
    let path = d.join("receipts.jsonl");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(path.clone()),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 9))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["receiptLog"], "file");
    assert_eq!(health["receiptsPending"], 1);
    // While this server runs, nobody else may open its journal (REV-L3-F4).
    let busy = crate::receipts::ReceiptLog::open(&path, &crate::journal::TEST_BINDING);
    assert!(busy.is_err(), "one server per journal");
    // Stopped: read the file as a restarted server would.
    drop(f);
    let reopened =
        crate::receipts::ReceiptLog::open(&path, &crate::journal::TEST_BINDING).expect("reopen");
    assert_eq!(reopened.peek(9).len(), 1);
    assert_eq!(reopened.peek(9)[0].nonce, [9; 32]);
    std::fs::remove_dir_all(d).ok();
}

/// REV-L4-F1: `/v1/health` shows the reporter's credits in flight and since
/// when one is stuck, with its last error; `null` without a reporter.
#[tokio::test(start_paused = true)]
async fn health_shows_a_stuck_reporter() {
    struct Stuck;
    impl crate::reporter::ReporterHealth for Stuck {
        fn reporter_status(&self) -> crate::reporter::ReporterStatus {
            crate::reporter::ReporterStatus {
                posts: 7,
                applied: 40,
                in_flight: 2,
                stuck_since: Some(1_700_000_000),
                write_errors: 0,
                last_error: Some("credit at nonce 9 unmined".into()),
            }
        }
    }
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        reporter: Some(Arc::new(Stuck)),
        ..Setup::default()
    });
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    let r = &health["reporter"];
    assert_eq!(r["inFlight"], 2);
    assert_eq!(r["stuckSince"], 1_700_000_000u64);
    assert_eq!(r["posts"], 7);
    assert_eq!(r["lastError"], "credit at nonce 9 unmined");
    let (_, _, health) = get(&fixture(true, mock()), "/v1/health", &[]).await;
    assert!(health["reporter"].is_null());
}

/// ROADMAP C62: `/v1/health` carries the level-3 store's `level3` object
/// as it reports it, and `null` without a level-3 store.
#[tokio::test(start_paused = true)]
async fn c62_health_shows_the_level_3_post_log() {
    struct Posts;
    impl crate::Level3Health for Posts {
        fn level3_health(&self) -> Value {
            json!({"postsPending": 2, "postsRefused": 1, "oldestPendingSecs": 299, "stopped": null})
        }
    }
    let f = build(Setup {
        level3: Some(Arc::new(Posts)),
        ..Setup::default()
    });
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(
        health["level3"],
        json!({"postsPending": 2, "postsRefused": 1, "oldestPendingSecs": 299, "stopped": null})
    );
    let (_, _, health) = get(&fixture(false, None), "/v1/health", &[]).await;
    assert!(health["level3"].is_null(), "{health}");
    assert!(health.as_object().expect("object").contains_key("level3"));
}

/// REV-L3-F5: once a receipt write failed, the server refuses new paid
/// requests (503, before anything is verified or charged) until it is
/// drained and restarted; `/v1/health` says it is not accepting payments.
#[tokio::test(start_paused = true)]
async fn a_failed_receipt_write_refuses_new_payments() {
    let d = crate::journal::test_dir("api-receipts-fail");
    let path = d.join("receipts.jsonl");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(path.clone()),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["acceptingPayments"], true);
    // The disk fails on the next write.
    f.state.receipts.break_journal_for_test();
    f.state.receipts.record(crate::receipts::Receipt {
        round: 1,
        shard: f.shard,
        amount: 1,
        payer: [1; 20],
        nonce: [1; 32],
    });
    assert_eq!(f.state.receipts.write_errors(), 1);
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 2))],
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(b["error"]["code"], "receipt_log_unavailable");
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["acceptingPayments"], false);
    assert_eq!(health["receiptWriteErrors"], 1);
    std::fs::remove_dir_all(d).ok();
}

/// ADR-0025 item 4: an agent buys a whole fresh dataset at a flat price,
/// never more than every row and never less than the floor; old data is free.
#[tokio::test(start_paused = true)]
async fn datasets_have_a_flat_price_between_the_floor_and_every_row() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        dataset: Some(DatasetPricing {
            flat_micro_usdc: 2_000,
            floor_rows: 1,
        }),
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", f.shard);
    let (s, h, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(
        pr.accepts[0].amount, "2000",
        "3 rows at 1,000 each, flat 2,000"
    );
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 3))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["rows"].as_array().map(Vec::len), Some(3));
    // One row still costs one row.
    let (_, h, _) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(pr.accepts[0].amount, "1000");
    // Past the fresh window the dataset is free.
    f.now.store(4_600, Ordering::SeqCst);
    let (s, _, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// OPS-L2-F1, F3: a settle whose outcome is unknown (here, a facilitator
/// slower than the whole settle patience) stays a settle intent on disk for
/// the resolver, is counted, and shows on `/v1/health`; no receipt is
/// guessed.
#[tokio::test(start_paused = true)]
async fn an_unknown_settle_stays_an_intent_and_is_counted() {
    let dir = crate::journal::test_dir("api-intent-unknown");
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: fac.clone(),
            settle_ms: Some(400),
        })),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1_000),
            ..ApiConfig::default()
        },
        intent_log: Some(dir.join("settle-intents.jsonl")),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 21))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    assert_eq!(f.state.intents.due().len(), 1, "released to the resolver");
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 1);
    let (_, _, hb) = get(&f, "/v1/health", &[]).await;
    assert_eq!(hb["settles"]["unknownOutcomes"], 1);
    assert_eq!(hb["settles"]["intentsOpen"], 1);
    assert_eq!(hb["settles"]["intentLog"], "file");
    // The intent survives a restart (it is on disk).
    drop(f);
    let again = crate::intents::IntentLog::open(
        dir.join("settle-intents.jsonl"),
        &crate::journal::TEST_BINDING,
    )
    .expect("reopen");
    assert_eq!(again.due().len(), 1);
}

/// OPS-L2-F1: a settled payment ends its intent once its receipt is on disk.
#[tokio::test(start_paused = true)]
async fn a_settled_payment_ends_its_intent() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 22))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(f.state.receipts.pending(), 1);
    assert_eq!(f.state.intents.open_count(), 0);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 0);
}

/// OPS-L2-F1: an intent that cannot be made durable means no settle at all
/// (fail closed): nothing is charged and paid requests get 503.
#[tokio::test(start_paused = true)]
async fn a_broken_intent_log_refuses_before_settling() {
    let dir = crate::journal::test_dir("api-intent-broken");
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        intent_log: Some(dir.join("settle-intents.jsonl")),
        ..Setup::default()
    });
    f.state.intents.break_journal_for_test();
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 23))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("settle_intent_unavailable")
        )
    );
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0, "never settled");
    // The failed write closes the log: later payments are refused before
    // verify, like a receipt log write error (REV-L3-F5).
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 24))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("receipt_log_unavailable")
        )
    );
    let (_, _, hb) = get(&f, "/v1/health", &[]).await;
    assert_eq!(hb["acceptingPayments"], false);
}

/// Counts settles running at once (OPS-L2-F2).
struct Concurrent {
    inner: MockFacilitator,
    running: AtomicU64,
    max_running: AtomicU64,
}

impl Facilitator for Concurrent {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(async move {
            let n = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.inner.settle(r).await
        })
    }
}

/// OPS-L2-F2 (review 2's slot-queue case): the slot taken at verify is held
/// through the settle, so settle tasks never queue for a slot outside their
/// timeout: at most `max_facilitator_calls` exist at once, and after the
/// last request every one ends within the settle budget.
///
/// E10: the clock is paused (time moves only when every task waits on a
/// timer), so the 80 ms settle always fits its 100 ms timeout. On the real
/// clock a loaded machine timed every settle out (2 of 10 suite runs at load
/// ~40 on 2026-10-16 had no payment settled).
///
/// E10 review: a payer told `503` retries 10 ms later (it used to spend all
/// six nonces at once, so only two payers ever contended and a settle that
/// queued for a slot passed), and a sampler counts settle tasks every 1 ms,
/// not only when a request returns. With the slot dropped after verify and
/// taken again by the settle task, `max_live` reaches 8.
#[tokio::test(start_paused = true)]
async fn settles_never_queue_for_a_facilitator_slot() {
    let payers: Vec<String> = (0..8).map(|i| format!("slot payer {i}")).collect();
    let fac = Arc::new(Concurrent {
        inner: MockFacilitator::new(payers.iter().map(|p| addr(p))),
        running: AtomicU64::new(0),
        max_running: AtomicU64::new(0),
    });
    let slots = 2;
    let f = Arc::new(build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(500),
            max_facilitator_calls: slots,
            payment_attempts_per_minute: 1_000_000,
            unsettled_per_hour: 1_000_000,
            ..ApiConfig::default()
        },
        ..Setup::default()
    }));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let max_live = Arc::new(AtomicU64::new(0));
    let sampler = {
        let (f, max_live) = (f.clone(), max_live.clone());
        tokio::spawn(async move {
            loop {
                max_live.fetch_max(f.state.settle_tasks.live() as u64, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };
    let mut hs = Vec::new();
    for (i, p) in payers.iter().enumerate() {
        let (f, h, p, uri, max_live) = (
            f.clone(),
            h.clone(),
            p.clone(),
            uri.clone(),
            max_live.clone(),
        );
        hs.push(tokio::spawn(async move {
            let mut statuses = Vec::new();
            for nonce in 0..6u8 {
                // The mock's nonces are global: one per payment.
                let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, &p, i as u8 * 6 + nonce))];
                let ip: IpAddr = format!("10.9.{i}.{nonce}").parse().expect("ip");
                let mut s = StatusCode::SERVICE_UNAVAILABLE;
                for _ in 0..10_000 {
                    s = send(&f, Method::GET, &uri, &sig, Some(ip)).await.0;
                    max_live.fetch_max(f.state.settle_tasks.live() as u64, Ordering::SeqCst);
                    if s != StatusCode::SERVICE_UNAVAILABLE {
                        break;
                    }
                    // Retries spread over every millisecond, so some
                    // payer is always about to ask for a slot.
                    tokio::time::sleep(Duration::from_millis(1 + i as u64)).await;
                }
                statuses.push(s);
            }
            statuses
        }));
    }
    let mut ok = 0;
    for h in hs {
        ok += h
            .await
            .expect("task")
            .iter()
            .filter(|s| **s == StatusCode::OK)
            .count();
    }
    sampler.abort();
    assert_eq!(ok, 8 * 6, "every payment settled once it had a slot");
    assert!(
        fac.max_running.load(Ordering::SeqCst) <= slots as u64,
        "settling at once: {}",
        fac.max_running.load(Ordering::SeqCst)
    );
    assert!(
        max_live.load(Ordering::SeqCst) <= slots as u64,
        "settle tasks at once: {}",
        max_live.load(Ordering::SeqCst)
    );
    let budget = f.state.settle_budget();
    assert!(f.state.settle_tasks.wait_idle(budget).await);
}

// ------------------------------------------------------------ level 2

mod level2 {
    //! `/v1/observations` (ADR-0027 amendment 4): a synthetic fixed feed,
    //! settled by the coordinator's own round code, served with its
    //! transcript; the client rebuilds the value from its own template.
    use super::*;
    use crate::client::{
        draw_budget_from_return, draw_matches_from_return, honest_sealed_returns,
        observation_level_from_return, verify_observation, verify_observation_onchain,
        ObservationExpect, SealedRets, VerifiedObservation, VerifyError,
    };

    /// ROADMAP C11: what the registry answers for a round of a draw without a
    /// reveal key (no sealed post): `revealLogComplete` true.
    fn unsealed(v: &VerifiedObservation) -> SealedRets {
        honest_sealed_returns(&v.checked.reveal_log, None, true, false)
    }
    use crate::observations;
    use crate::observations::{ObservationFeed, ObservationStore, Transcript, OBSERVATION_ROUND};
    use onyx_coordinator::observe::{
        sign_commit, sign_reveal, test_keys, ObservationResults, ObserveConfig, Observer,
    };
    use onyx_core::claim::MetricId;
    use onyx_core::{
        observation_commit, Answer, FeedTemplate, Observation, ObservationKind, Redistribution,
        Subject, SubjectRule,
    };

    const START: u64 = 1_500;
    const METRIC: &str = "test.index";
    /// The checker's clock: one minute after the test round's reveal close
    /// (1,500 + 300 + 180).
    const LOAD_NOW: u64 = 2_040;

    fn rights(r: Redistribution) -> DataRights {
        DataRights {
            license: "CC0-1.0".into(),
            redistribution: r,
            source: SourceId("test.page".into()),
            attribution: Some("Source: test page".into()),
            terms: if r == Redistribution::Restricted {
                "test.terms".into()
            } else {
                String::new()
            },
        }
    }

    fn subject() -> Subject {
        Subject::Aggregate("test.index".into())
    }

    fn template(r: Redistribution) -> FeedTemplate {
        FeedTemplate {
            feed: "test.index.v1".into(),
            metric: MetricId(METRIC.into()),
            fetcher: keccak256(b"test fetcher"),
            url_template: "https://example.test/index.json".into(),
            selector: "/value".into(),
            kind: ObservationKind::Numeric {
                scale: 2,
                tolerance: 0,
            },
            window_secs: 300,
            min_observers: 3,
            rights: rights(r),
            subject: SubjectRule::Fixed(subject()),
        }
    }

    fn obs_addr(i: u8) -> [u8; 20] {
        test_keys::ordered(i).1
    }

    /// Settle one round of `template` from `START` where observers answer
    /// `answers` (mantissas), and return its transcript.
    fn settle(template: &FeedTemplate, answers: &[i128]) -> Transcript {
        settle_from(template, START, answers)
    }

    /// Settle one round of `template` from `start`.
    fn settle_from(template: &FeedTemplate, start: u64, answers: &[i128]) -> Transcript {
        let (mut store, admin) = ObservationResults::new();
        store
            .register_feed(&admin, template.clone(), "test", 0)
            .expect("registers");
        let spec = template.instantiate(&subject(), start).expect("spec");
        let cfg = ObserveConfig {
            commit_close: spec.window_end + 60,
            reveal_close: spec.window_end + 180,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        };
        let observers: Vec<Observer> = (1..=answers.len() as u8)
            .map(|i| Observer {
                address: obs_addr(i),
                bond: 1_000,
            })
            .collect();
        let snapshot = store
            .announce_draw(&spec, &cfg, &observers, 10, &SimulatedChain::at(0))
            .expect("announces");
        let mut round = store
            .open_round(
                spec.clone(),
                cfg,
                snapshot,
                &SimulatedChain::with_beacon(Hash32([3; 32])),
            )
            .expect("opens");
        let observers = round.observers().to_vec();
        let rid = round.round_id();
        let attempt = round.draw().attempt;
        let mut reveals = Vec::new();
        for (i, n) in (1..=answers.len() as u8).zip(answers) {
            let (sk, a) = test_keys::ordered(i);
            let o = Observation {
                answer: Answer::Number(*n),
                observed_at: start + 10,
                page_hash: keccak256(&n.to_be_bytes()),
            };
            let salt = keccak256(&[i]);
            let c = observation_commit(&rid, &a, &o, &salt);
            let sig = sign_commit(&sk, &rid, attempt, &c).expect("sign");
            round.commit(start + 10, a, c, sig).expect("commit");
            reveals.push((sk, a, o, salt));
        }
        for (sk, a, o, s) in &reveals {
            let sig = sign_reveal(sk, &rid, attempt, o, s).expect("sign");
            round
                .reveal(cfg.commit_close + 1, *a, o.clone(), *s, sig)
                .expect("reveal");
        }
        let commits = round.signed_commits();
        let reveals = round.signed_reveals();
        let settled = store.settle(&round, cfg.reveal_close + 1).expect("settles");
        let onyx_coordinator::observe::ObservationOutcome::Value(answer) =
            settled.outcome().clone()
        else {
            panic!("no value")
        };
        Transcript {
            subject: subject(),
            window_start: start,
            cfg,
            pool: round.pool().to_vec(),
            draw: round.draw(),
            observers,
            commits,
            reveals,
            sealed: Default::default(),
            answer,
        }
    }

    /// The operator's pin for `t` (L2API-R4-F1).
    fn pins(t: &FeedTemplate) -> observations::TemplatePins {
        observations::TemplatePins::from([(t.metric.0.clone(), t.id())])
    }

    fn feed(r: Redistribution) -> ObservationFeed {
        let t = template(r);
        let text = ObservationStore::to_json(&t, &[settle(&t, &[12_345, 12_345, 12_345])], None)
            .to_string();
        ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("store loads")
    }

    /// The shard fixture plus the level-2 feed.
    fn served(paid: bool, facilitator: Option<Arc<dyn Facilitator>>, r: Redistribution) -> Fixture {
        let f = build(Setup {
            paid,
            facilitator,
            ..Setup::default()
        });
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_observations(feed(r))
            .expect("adds");
        f
    }

    const URI: &str = "/v1/observations/test.index?subject=test.index";

    /// A round of `t` from `start` at draw 0 whose three observers disagree
    /// (no result), built directly: the pool is the draw size.
    fn no_result(t: &FeedTemplate, start: u64) -> observations::Attempt {
        let spec = t.instantiate(&subject(), start).expect("spec");
        let cfg = ObserveConfig {
            commit_close: spec.window_end + 60,
            reveal_close: spec.window_end + 180,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        };
        let observers: Vec<Observer> = (1..=3u8)
            .map(|i| Observer {
                address: obs_addr(i),
                bond: 1_000,
            })
            .collect();
        let draw =
            onyx_coordinator::observe::DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
        let rid = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &draw);
        let mut commits = Vec::new();
        let mut reveals = Vec::new();
        for (i, o) in (1i128..).zip(&observers) {
            let ob = Observation {
                answer: Answer::Number(i),
                observed_at: start + 10,
                page_hash: keccak256(&i.to_be_bytes()),
            };
            let salt = keccak256(&o.address);
            let (c, r) = test_keys::signed(
                &test_keys::ordered_secret(&o.address).expect("test key"),
                &rid,
                draw.attempt,
                &ob,
                &salt,
            );
            commits.push(c);
            reveals.push(r);
        }
        observations::Attempt {
            subject: subject(),
            window_start: start,
            cfg,
            pool: observers.clone(),
            draw,
            observers,
            commits,
            reveals,
            sealed: Default::default(),
        }
    }

    /// SEC-R2-F1: the value fact at `START` and, for the window before it, a
    /// fact whose only round settled no value.
    fn served_with_attempt() -> (Fixture, observations::Attempt) {
        served_with_attempt_under(ApiConfig::default())
    }

    fn served_with_attempt_under(config: ApiConfig) -> (Fixture, observations::Attempt) {
        let t = template(Redistribution::Allowed);
        let a = no_result(&t, START - 600);
        let text = ObservationStore::to_json_with_attempts(
            &t,
            &[settle(&t, &[12_345, 12_345, 12_345])],
            std::slice::from_ref(&a),
            None,
        )
        .to_string();
        let store = ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("loads");
        let f = build(Setup {
            paid: false,
            config,
            ..Setup::default()
        });
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_observations(store)
            .expect("adds");
        (f, a)
    }

    /// ATT-R1-F5: free `/attempts` answers have their own per-client budget.
    #[tokio::test]
    async fn free_attempts_are_rate_limited_per_client() {
        let (f, a) = served_with_attempt_under(ApiConfig {
            free_attempts_per_minute: 2,
            ..ApiConfig::default()
        });
        let (t, subj) = expect();
        let window_end = t
            .instantiate(&subj, a.window_start)
            .expect("spec")
            .window_end;
        let uri = format!(
            "/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}"
        );
        let x: IpAddr = "203.0.113.7".parse().expect("ip");
        let y: IpAddr = "203.0.113.8".parse().expect("ip");
        for _ in 0..2 {
            let (s, _, b) = send(&f, Method::GET, &uri, &[], Some(x)).await;
            assert_eq!(s, StatusCode::OK, "{b}");
        }
        let (s, h, body) = send(&f, Method::GET, &uri, &[], Some(x)).await;
        assert_eq!(
            (s, code(&body)),
            (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
        );
        assert!(h.get("retry-after").is_some());
        let (s, _, _) = send(&f, Method::GET, &uri, &[], Some(y)).await;
        assert_eq!(s, StatusCode::OK, "another client has its own budget");
    }

    #[tokio::test]
    async fn a_fact_without_a_value_serves_its_rounds_free_and_a_valued_fact_refers_to_outcome_all()
    {
        let (f, a) = served_with_attempt();
        let (t, subj) = expect();
        let window_end = t
            .instantiate(&subj, a.window_start)
            .expect("spec")
            .window_end;
        let ex = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: Some(window_end),
            min_window_end: 0,
            now: LOAD_NOW,
        };
        let uri = format!(
            "/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}"
        );
        let (s, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["attempts"][0]["outcome"], "noResult");
        let v = crate::client::verify_attempts(&b, &ex).expect("verifies");
        assert_eq!((v.attempts.len(), v.value_attempt), (1, None));
        // The valued fact's rounds come only with its value.
        let valued = t.instantiate(&subj, START).expect("spec").window_end;
        let (s, _, b) = get(
            &f,
            &format!("/v1/observations/test.index/attempts?subject=test.index&window_end={valued}"),
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
        let (s, _, _) = get(&f, "/v1/observations/test.index/attempts?subject=nope", &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = get(
            &f,
            "/v1/observations/test.index/attempts?subject=test.index&as_of=1",
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn outcome_all_adds_the_facts_rounds_without_a_value() {
        let (f, _) = served_with_attempt();
        let (t, subj) = expect();
        let ex = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        };
        let (s, h, b) = get(&f, &format!("{URI}&outcome=all"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        // The valued fact had no round without a value: an empty list, which
        // the client checks against the chain's drawCount (1).
        assert_eq!(b["attempts"], json!([]));
        let v = verify_observation(&b, &ex).expect("verifies");
        let all = v.attempts.expect("asked for all");
        assert_eq!(all.value_attempt, Some(0));
        let mut one = [0u8; 32];
        one[31] = 1;
        let mut budget = [0u8; 64];
        budget[63] = 3;
        let tally = crate::client::verify_attempts_onchain(&all, &one, &[], &budget, &[], &[])
            .expect("one draw, the value's");
        assert_eq!((tally.draws, tally.without_value), (1, 0));
        // Its ETag differs from the plain answer's.
        let (_, h2, b2) = get(&f, URI, &[]).await;
        assert!(b2.get("attempts").is_none());
        assert_ne!(h.get("etag"), h2.get("etag"));
        // `value` is the default: one spelling, without the parameter.
        let (s, _, _) = get(&f, &format!("{URI}&outcome=value"), &[]).await;
        assert!(s.is_redirection(), "{s}");
        let (s, _, _) = get(&f, &format!("{URI}&outcome=some"), &[]).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    fn expect() -> (FeedTemplate, Subject) {
        (template(Redistribution::Allowed), subject())
    }

    fn limit_ids(v: &Value) -> Vec<&str> {
        v.as_array()
            .expect("limits")
            .iter()
            .map(|l| l["id"].as_str().expect("id"))
            .collect()
    }

    /// ROADMAP B4 (THREATS REVLOG-1, REVLOG-5): the 402 offer (its body and
    /// the PAYMENT-REQUIRED `extensions`) and every answer state both known
    /// limits, so a buyer sees them before paying. A value's fact is closed,
    /// so REVLOG-5 does not apply to the rounds of a value answer.
    #[tokio::test]
    async fn every_level2_answer_and_offer_states_revlog_1_and_revlog_5() {
        let want = observations::limits_json(false);
        assert_eq!(limit_ids(&want), ["REVLOG-1", "REVLOG-5"]);
        assert_eq!(want[0]["applies"], true);
        assert_eq!(want[1]["applies"], false);
        for l in want.as_array().expect("limits") {
            assert_eq!(l["docs"], observations::LIMITS_DOCS);
            assert!(!l["text"].as_str().expect("text").is_empty());
        }
        let (t, subj) = expect();
        let ex = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        };
        let f = served(true, mock(), Redistribution::Allowed);
        for (uri, nonce) in [(URI.to_string(), 61), (format!("{URI}&outcome=all"), 62)] {
            for method in [Method::GET, Method::HEAD] {
                let (s, h, b) = send(&f, method.clone(), &uri, &[], None).await;
                assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{method} {uri}");
                if method == Method::GET {
                    assert_eq!(b["limits"], want, "402 body of {uri}");
                }
                let pr: PaymentRequired =
                    decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii"))
                        .expect("offer");
                // x402 v2 §5.1.2: each extension is exactly `{info, schema}`.
                let ext = pr.extensions.expect("extensions");
                let keys = |v: &Value| -> Vec<String> {
                    v.as_object().expect("object").keys().cloned().collect()
                };
                assert_eq!(keys(&ext), ["limits"], "402 offer of {method} {uri}");
                let l = &ext["limits"];
                assert_eq!(keys(l), ["info", "schema"], "{method} {uri}");
                assert_eq!(l["info"], json!({ "limits": want }), "{method} {uri}");
                assert_eq!(l["schema"]["required"], json!(["limits"]));
                let item = &l["schema"]["properties"]["limits"]["items"];
                assert_eq!(item["required"], json!(["id", "applies", "text", "docs"]));
                for x in want.as_array().expect("limits") {
                    assert_eq!(keys(x), keys(&item["properties"]), "schema fits info");
                }
            }
            let (_, h, _) = get(&f, &uri, &[]).await;
            let (s, _, b) = get(
                &f,
                &uri,
                &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))],
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{b}");
            assert_eq!(b["limits"], want, "paid answer of {uri}");
            verify_observation(&b, &ex).expect("the client accepts the stated limits");
        }
        // A free (decayed or unpriced) answer states them too.
        let free = served(false, None, Redistribution::Allowed);
        let (s, _, b) = get(&free, URI, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["limits"], want);
    }

    /// B4: the free `/attempts` answer of a closed (fixed) fact states both
    /// limits, REVLOG-5 not applying: its rounds come with their reveals.
    #[tokio::test]
    async fn a_closed_facts_attempts_state_revlog_5_as_not_applying() {
        let (f, a) = served_with_attempt();
        let (t, subj) = expect();
        let window_end = t
            .instantiate(&subj, a.window_start)
            .expect("spec")
            .window_end;
        let uri = format!(
            "/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}"
        );
        let (s, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(b["attempts"][0].get("reveals").is_some(), "closed: open");
        assert_eq!(b["limits"], observations::limits_json(false));
    }

    /// x402 v2 §5.1.2: clients echo the offer's `extensions` in their
    /// `PaymentPayload` (the official SDKs do). A level-2 payment that echoes
    /// `limits` is accepted and served like one that does not.
    #[tokio::test]
    async fn a_payment_echoing_the_limits_extension_is_accepted() {
        let f = served(true, mock(), Redistribution::Allowed);
        let (s, h, _) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
        let pr: Value =
            decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("offer");
        let mut p: Value = decode_header(&pay(&h, "payer a", 63)).expect("payload");
        p["extensions"] = pr["extensions"].clone();
        assert!(p["extensions"]["limits"]["info"]["limits"].is_array());
        let (s, _, b) = get(&f, URI, &[(x402::PAYMENT_SIGNATURE, encode_header(&p))]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["limits"], observations::limits_json(false));
    }

    /// B4, backward compatibility: an answer without `limits` (a server
    /// from before B4) still verifies; a misstated one is refused.
    #[tokio::test]
    async fn the_client_takes_no_limits_from_an_older_server_and_refuses_misstated_ones() {
        let f = served(false, None, Redistribution::Allowed);
        let (_, _, b) = get(&f, URI, &[]).await;
        let (t, subj) = expect();
        let ex = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        };
        let mut old = b.clone();
        old.as_object_mut().expect("obj").remove("limits");
        verify_observation(&old, &ex).expect("an older server's answer verifies");
        type Edit = (&'static str, fn(&mut Value));
        let edits: [Edit; 5] = [
            ("REVLOG-5 left out", |b| {
                b["limits"].as_array_mut().expect("arr").pop();
            }),
            ("REVLOG-1 said not to apply", |b| {
                b["limits"][0]["applies"] = json!(false)
            }),
            ("REVLOG-5 said to apply to an open round", |b| {
                b["limits"][1]["applies"] = json!(true)
            }),
            ("a softened text", |b| {
                b["limits"][0]["text"] = json!("none")
            }),
            ("not a list", |b| b["limits"] = json!({})),
        ];
        for (what, edit) in edits {
            let mut x = b.clone();
            edit(&mut x);
            assert!(verify_observation(&x, &ex).is_err(), "{what}");
        }
    }

    /// L2RND-R3-F3, REVLOG LOW-1: the served `onchain` hint names all four
    /// registry views a client must ask, and the client accepts it.
    #[tokio::test]
    async fn the_onchain_hint_names_all_four_registry_calls() {
        let t = template(Redistribution::Allowed);
        let pointer = observations::OnchainPointer {
            chain_id: 84_532,
            registry: [7; 20],
        };
        let text =
            ObservationStore::to_json(&t, &[settle(&t, &[12_345, 12_345, 12_345])], Some(pointer))
                .to_string();
        let store = ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("store loads");
        let f = build(Setup {
            paid: false,
            ..Setup::default()
        });
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_observations(store)
            .expect("adds");
        let (s, _, b) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let o = &b["onchain"];
        assert_eq!(o["call"], observations::OBSERVATION_LEVEL_CALL);
        assert_eq!(o["drawMatches"], observations::DRAW_MATCHES_CALL);
        assert_eq!(o["drawBudget"], observations::DRAW_BUDGET_CALL);
        // Review REVLOG LOW-1: the transcript check is named too.
        assert_eq!(
            o["transcriptComplete"],
            observations::TRANSCRIPT_COMPLETE_CALL
        );
        let (t, subj) = expect();
        let e = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        };
        verify_observation(&b, &e).expect("the client accepts the hint");
        // An older server's hint without the key still verifies (the client
        // asks transcriptComplete anyway); a hint naming another call does not.
        let mut old = b.clone();
        old["onchain"]
            .as_object_mut()
            .expect("hint")
            .remove("transcriptComplete");
        verify_observation(&old, &e).expect("a hint without the key verifies");
        let mut wrong = b.clone();
        wrong["onchain"]["transcriptComplete"] = "OnyxRegistry.somethingElse()".into();
        assert!(matches!(
            verify_observation(&wrong, &e),
            Err(VerifyError::Malformed("onchain.transcriptComplete"))
        ));
    }

    #[tokio::test]
    async fn a_free_observation_verifies_from_the_clients_own_template() {
        let f = served(false, None, Redistribution::Allowed);
        let (s, h, b) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["value"], "123.45");
        assert_eq!(b["proofLevel"], 2);
        assert_eq!(cc(&h), "public, no-cache");
        let (t, subj) = expect();
        let v = verify_observation(
            &b,
            &ObservationExpect {
                template: &t,
                subject: &subj,
                window_end: None,
                min_window_end: 0,
                now: LOAD_NOW,
            },
        )
        .expect("verifies");
        assert_eq!(v.answer, Answer::Number(12_345));
        assert_eq!(v.value, "123.45");
        // The on-chain check the client can make with its own node.
        let call = v.observation_level_calldata();
        assert_eq!(call.len(), 4 + 5 * 32);
        let mut two = [0u8; 32];
        two[31] = 2;
        assert_eq!(observation_level_from_return(&two), Ok(2));
        assert!(observation_level_from_return(&[1u8; 32]).is_err());
        // L2RND-R1: the draw check against the client's own node.
        let call = v.draw_matches_calldata();
        assert_eq!(call.len(), 4 + 7 * 32);
        assert_eq!(
            call[..4],
            keccak256(b"drawMatches(bytes32,bytes32,uint64,uint32,bytes32,uint64,bytes32)").0[..4]
        );
        assert_eq!(
            call[4 + 5 * 32..4 + 6 * 32][24..],
            v.checked.draw.beacon_height.to_be_bytes()
        );
        assert_eq!(call[4 + 6 * 32..], v.checked.draw.beacon.0);
        // L2RND-R3: the commitment word is the client's own recomputation.
        assert_eq!(
            call[4 + 4 * 32..4 + 5 * 32],
            v.checked.onchain.draw_commit().0
        );
        let mut yes = [0u8; 32];
        yes[31] = 1;
        assert_eq!(draw_matches_from_return(&yes), Ok(true));
        assert_eq!(draw_matches_from_return(&[0u8; 32]), Ok(false));
        yes[31] = 2;
        assert!(draw_matches_from_return(&yes).is_err());
        // L2RND-R2-F6: level 2, drawMatches and the draw bound, together.
        let [_, _, budget, complete] = v.onchain_calls();
        // ADR-0027 amendment 8: the transcript log query of the served round.
        assert_eq!(complete, v.checked.log.calldata());
        assert_eq!(v.checked.log.round_id, v.checked.round_id);
        assert_eq!(v.checked.log.registry_fact, v.checked.registry_fact);
        assert!(v.checked.log.entries.iter().all(|e| e.revealed));
        assert_eq!(
            budget[..4],
            keccak256(b"drawBudget(bytes32,bytes32)").0[..4]
        );
        assert_eq!(budget[4..36], v.checked.onchain.metric_key.0);
        let words = |a: u32, b: u32| {
            let mut w = [0u8; 64];
            w[28..32].copy_from_slice(&a.to_be_bytes());
            w[60..64].copy_from_slice(&b.to_be_bytes());
            w
        };
        assert_eq!(draw_budget_from_return(&words(1, 3)), Ok((1, 3)));
        assert!(draw_budget_from_return(&[0u8; 32]).is_err());
        yes[31] = 1;
        let ok = verify_observation_onchain(&v, &two, &yes, &words(0, 3), &yes, &unsealed(&v));
        assert_eq!(ok, Ok(()));
        let chain = |e: &'static str| Err(VerifyError::ChainMismatch(e));
        let mut one = [0u8; 32];
        one[31] = 1;
        assert_eq!(
            verify_observation_onchain(&v, &one, &yes, &words(0, 3), &yes, &unsealed(&v)),
            chain("observationLevel")
        );
        assert_eq!(
            verify_observation_onchain(&v, &two, &[0u8; 32], &words(0, 3), &yes, &unsealed(&v)),
            chain("drawMatches")
        );
        // More draws open than the reopens allow, or an attempt past the bound.
        assert_eq!(
            verify_observation_onchain(&v, &two, &yes, &words(4, 3), &yes, &unsealed(&v)),
            chain("drawBudget")
        );
        let mut late = v.clone();
        late.checked.draw.attempt = 3;
        assert_eq!(
            verify_observation_onchain(&late, &two, &yes, &words(0, 3), &yes, &unsealed(&late)),
            chain("drawBudget")
        );
        assert_eq!(
            verify_observation_onchain(&late, &two, &yes, &words(0, 6), &yes, &unsealed(&late)),
            Ok(())
        );
        // SIG-R1-F1: the chain logged a commit or receipt the round leaves out.
        assert_eq!(
            verify_observation_onchain(&v, &two, &yes, &words(0, 3), &[0u8; 32], &unsealed(&v)),
            chain("transcriptComplete: a logged commit or receipted reveal is left out")
        );
        assert!(
            verify_observation_onchain(&v, &two, &yes, &words(0, 3), &two, &unsealed(&v)).is_err()
        );
        // Conditional request on the result hash.
        let etag = h[header::ETAG].to_str().expect("etag").to_string();
        let (s304, _, _) = get(&f, URI, &[("if-none-match", etag)]).await;
        assert_eq!(s304, StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn a_fresh_observation_is_paid_billed_to_its_fact_and_free_after_decay() {
        let f = served(true, mock(), Redistribution::Allowed);
        let (s, h, b) = get(&f, URI, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::PAYMENT_REQUIRED, Some("payment_required"))
        );
        let (s, h2, b) = get(
            &f,
            URI,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 41))],
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(h2.contains_key(x402::PAYMENT_RESPONSE));
        assert_eq!(cc(&h2), "private, no-cache");
        // Billed to the observation revenue book of its on-chain fact key.
        let r = f.state.receipts.peek(10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].round, OBSERVATION_ROUND);
        assert_eq!(
            r[0].shard.to_string(),
            b["registryFactKey"].as_str().expect("key")
        );
        // The settled time is the window end the template fixes (1,500 +
        // 300, L2API-R3-F1); the test policy keeps a value paid for an hour.
        f.now.store(1_800 + 3_599, Ordering::SeqCst);
        let (s, _, _) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "still fresh");
        f.now.store(1_800 + 3_600, Ordering::SeqCst);
        let (s, h, _) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::OK, "decayed to free");
        assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
    }

    /// L2API-R3-F3: a fixed feed's older, now-free windows are reachable
    /// with `as_of` (newest window ending at or before it), like a shard.
    #[tokio::test]
    async fn as_of_picks_the_newest_window_ending_at_or_before_it() {
        let f = build(Setup::default());
        let t = template(Redistribution::Allowed);
        let rounds = [
            settle_from(&t, START, &[12_345, 12_345, 12_345]),
            settle_from(&t, 2_400, &[22_222, 22_222, 22_222]),
        ];
        let text = ObservationStore::to_json(&t, &rounds, None).to_string();
        let feed = ObservationStore::from_json(&text, 2_940, &pins(&t)).expect("store loads");
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_observations(feed)
            .expect("adds");
        let (s, _, b) = get(&f, URI, &[]).await;
        assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(2_700)));
        let (s, _, b) = get(&f, &format!("{URI}&as_of=2699"), &[]).await;
        assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(1_800)));
        assert_eq!(b["value"], "123.45");
        let (s, _, b) = get(&f, &format!("{URI}&as_of=2700"), &[]).await;
        assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(2_700)));
        let (s, _, b) = get(&f, &format!("{URI}&as_of=1799"), &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_subject"))
        );
        let (s, _, _) = get(&f, &format!("{URI}&window_end=1800&as_of=2700"), &[]).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "window_end and as_of together");
    }

    /// L2API-R4-F2: a round older than the client's `min_window_end` is a
    /// 404 before any 402, so a client is never charged for a round its own
    /// freshness floor refuses (as a shard's `min_as_of`, M7-L47-F5).
    #[tokio::test]
    async fn min_window_end_refuses_an_old_round_before_payment() {
        let f = served(true, mock(), Redistribution::Allowed);
        f.now.store(1_800 + 10, Ordering::SeqCst);
        let (s, h, b) = get(&f, &format!("{URI}&min_window_end=1801"), &[]).await;
        assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, Some("too_old")));
        assert!(
            !h.contains_key(x402::PAYMENT_REQUIRED),
            "no payment terms for a refused round"
        );
        let (s, _, _) = get(&f, &format!("{URI}&min_window_end=1800"), &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "the round meets the floor");
        // The canonical order is subject, window_end, as_of, min_window_end.
        let (s, _, _) = get(
            &f,
            "/v1/observations/test.index?min_window_end=1800&subject=test.index",
            &[],
        )
        .await;
        assert!(s.is_redirection(), "{s}");
    }

    #[tokio::test]
    async fn restricted_rights_are_not_served_and_unknown_names_are_404() {
        let f = served(false, None, Redistribution::Restricted);
        let (s, _, b) = get(&f, URI, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (
                StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
                Some("restricted_data")
            )
        );
        let f = served(false, None, Redistribution::Allowed);
        let (s, _, b) = get(&f, "/v1/observations/test.other?subject=test.index", &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_metric"))
        );
        let (s, _, b) = get(&f, "/v1/observations/test.index?subject=test.nope", &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_subject"))
        );
        // One canonical URL per resource; unknown parameters are refused.
        let (s, _, _) = get(
            &f,
            "/v1/observations/test.index?subject=test.index&x=1",
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_forbidden_source_never_loads_and_a_bad_transcript_never_loads() {
        // Forbidden rights: refused at load (a forbidden feed cannot even be
        // registered, so build the store text by hand from an allowed one).
        let t = template(Redistribution::Allowed);
        let mut store = ObservationStore::to_json(&t, &[settle(&t, &[7, 7, 7])], None);
        let mut forbidden = template(Redistribution::Forbidden);
        forbidden.rights.terms = String::new();
        store["template"] = json!(hex0x(&borsh::to_vec(&forbidden).expect("borsh")));
        let e = ObservationStore::from_json(&store.to_string(), LOAD_NOW, &pins(&forbidden))
            .expect_err("forbidden");
        assert!(e.to_string().contains("forbid"), "{e}");
        // A served answer that is not the value the reveals settle.
        let mut tr = settle(&t, &[7, 7, 7]);
        tr.answer = Answer::Number(8);
        let text = ObservationStore::to_json(&t, &[tr], None).to_string();
        assert!(ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).is_err());
        // Unknown store fields are refused.
        let mut v = ObservationStore::to_json(&t, &[settle(&t, &[7, 7, 7])], None);
        v["surprise"] = json!(1);
        assert!(ObservationStore::from_json(&v.to_string(), LOAD_NOW, &pins(&t)).is_err());
    }

    #[tokio::test]
    async fn the_client_refuses_forged_evidence() {
        let f = served(false, None, Redistribution::Allowed);
        let (_, _, b) = get(&f, URI, &[]).await;
        let (t, subj) = expect();
        let ex = ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        };
        assert!(verify_observation(&b, &ex).is_ok());
        let forged = |edit: &dyn Fn(&mut Value)| {
            let mut x = b.clone();
            edit(&mut x);
            verify_observation(&x, &ex)
        };
        // Another value, with or without its text.
        let other = hex0x(&borsh::to_vec(&Answer::Number(99_999)).expect("borsh"));
        assert!(forged(&|x| x["answer"] = json!(other)).is_err());
        assert!(forged(&|x| x["value"] = json!("999.99")).is_err());
        // A reveal that does not open its commit.
        assert!(forged(&|x| x["reveals"][0]["salt"] = json!(Hash32([9; 32]).to_string())).is_err());
        // A changed configuration (another round id).
        assert!(forged(&|x| x["config"]["reward"] = json!("11")).is_err());
        // Derived fields must be what the transcript yields.
        assert!(forged(&|x| x["resultHash"] = json!(Hash32([1; 32]).to_string())).is_err());
        assert!(forged(&|x| x["registryFactKey"] = json!(Hash32([1; 32]).to_string())).is_err());
        assert!(forged(&|x| x["settledUnix"] = json!(1)).is_err());
        assert!(forged(&|x| x["proofLevel"] = json!(1)).is_err());
        // ADR-0048: the ladder is the round's own level-2 step; a lifted
        // level, a step on other evidence, or a rise nothing checks is refused.
        let ok = verify_observation(&b, &ex).expect("verifies");
        assert_eq!(ok.ladder.level(), Some(onyx_core::ProofLevel::ReObserved));
        assert_eq!(ok.ladder.steps[0].evidence, ok.checked.result);
        assert_eq!(b["ladder"]["level"], 2);
        assert!(forged(&|x| x["ladder"]["level"] = json!(1)).is_err());
        assert!(forged(&|x| {
            x["ladder"]["level"] = json!(1);
            x["ladder"]["steps"][0]["level"] = json!(1);
            x["ladder"]["steps"][0]["name"] = json!("recomputed");
        })
        .is_err());
        assert!(forged(
            &|x| x["ladder"]["steps"][0]["evidence"] = json!(Hash32([1; 32]).to_string())
        )
        .is_err());
        assert!(forged(&|x| x["ladder"]["value"] = json!(Hash32([1; 32]).to_string())).is_err());
        assert!(forged(&|x| {
            let mut s = x["ladder"]["steps"][0].clone();
            s["level"] = json!(1);
            s["name"] = json!("recomputed");
            s["evidence"] = json!(Hash32([7; 32]).to_string());
            x["ladder"]["steps"] = json!([x["ladder"]["steps"][0].clone(), s]);
            x["ladder"]["level"] = json!(1);
        })
        .is_err());
        // An older node without the field: the client's own step.
        assert_eq!(
            forged(&|x| {
                x.as_object_mut().expect("object").remove("ladder");
            })
            .expect("verifies")
            .ladder,
            ok.ladder
        );
        assert!(forged(&|x| x["license"] = json!("MIT")).is_err());
        // Unknown fields, and a shard answer, are refused.
        assert!(forged(&|x| x["surprise"] = json!(1)).is_err());
        assert!(forged(&|x| x["verify"] = json!({})).is_err());
        // Another client template (another fetcher) yields another spec: the
        // served transcript does not verify under it.
        let mut other_t = template(Redistribution::Allowed);
        other_t.fetcher = keccak256(b"another fetcher");
        assert!(verify_observation(
            &b,
            &ObservationExpect {
                template: &other_t,
                subject: &subj,
                window_end: None,
                min_window_end: 0,
                now: LOAD_NOW,
            }
        )
        .is_err());
        // Another subject than the one asked for.
        let elsewhere = Subject::Aggregate("test.elsewhere".into());
        assert_eq!(
            verify_observation(
                &b,
                &ObservationExpect {
                    template: &t,
                    subject: &elsewhere,
                    window_end: None,
                    min_window_end: 0,
                    now: LOAD_NOW,
                }
            ),
            Err(VerifyError::NotRequested("another subject"))
        );
    }

    #[test]
    fn observation_metrics_are_never_markets_and_never_share_a_shard_name() {
        let f = build(Setup::default());
        let mut c = f.state.catalog.write().expect("lock");
        // A level-2 metric cannot take the name of a shard metric.
        let mut t = template(Redistribution::Allowed);
        t.metric = MetricId("test.flow".into());
        assert!(c
            .add_observations(ObservationFeed::new(t).expect("feed"))
            .is_err());
        assert!(c.add_observations(feed(Redistribution::Allowed)).is_ok());
        assert!(
            c.add_observations(feed(Redistribution::Allowed)).is_err(),
            "one feed per metric"
        );
        // Markets bind shard rows only: there is no market on an observation.
        assert!(c.market(METRIC).is_none());
    }

    /// R-EST-2 (ADR-0050): a level-2 feed in the estimate namespace is refused.
    #[test]
    fn a_level2_feed_under_an_estimate_metric_is_refused() {
        let f = build(Setup::default());
        let mut c = f.state.catalog.write().expect("lock");
        let mut t = template(Redistribution::Allowed);
        t.metric = MetricId("estimate.test.daily".into());
        let err = c
            .add_observations(ObservationFeed::new(t).expect("feed"))
            .expect_err("estimate metric");
        assert!(err.contains("estimate"), "{err}");
    }
    /// A self-consistent transcript for `observers` under `cfg` (every commit
    /// opens, every reveal agrees on `value`): what an operator could write
    /// into a store by hand, without any coordinator opening the round.
    fn hand_made(
        window_start: u64,
        cfg: ObserveConfig,
        observers: Vec<Observer>,
        value: i128,
    ) -> Transcript {
        let t = template(Redistribution::Allowed);
        let spec = t.instantiate(&subject(), window_start).expect("spec");
        // The pool is exactly the draw size, so the draw is the whole pool.
        let draw =
            onyx_coordinator::observe::DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
        let rid = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &draw);
        let mut commits = Vec::new();
        let mut reveals = Vec::new();
        for (i, o) in observers.iter().enumerate() {
            let ob = Observation {
                answer: Answer::Number(value),
                observed_at: window_start + 10,
                page_hash: keccak256(b"page"),
            };
            let salt = keccak256(&[i as u8, 7]);
            let (c, r) = test_keys::signed(
                &test_keys::ordered_secret(&o.address).expect("test key"),
                &rid,
                draw.attempt,
                &ob,
                &salt,
            );
            commits.push(c);
            reveals.push(r);
        }
        Transcript {
            subject: subject(),
            window_start,
            cfg,
            pool: observers.clone(),
            draw,
            observers,
            commits,
            reveals,
            sealed: Default::default(),
            answer: Answer::Number(value),
        }
    }

    fn good_cfg(window_start: u64) -> ObserveConfig {
        let end = window_start + 300;
        ObserveConfig {
            commit_close: end + 60,
            reveal_close: end + 180,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        }
    }

    fn observers(n: u8, bond: u128) -> Vec<Observer> {
        (1..=n)
            .map(|i| Observer {
                address: obs_addr(i),
                bond,
            })
            .collect()
    }

    /// L2API-R1-F1: a round no coordinator could open (one observer under
    /// `min_observers` 3, no bond, no slashes, commits closing at 0) neither
    /// loads on the server nor verifies in a client, even though every commit
    /// opens and the answer is the value the reveals settle.
    #[test]
    fn a_round_no_coordinator_could_open_never_loads_and_never_verifies() {
        let t = template(Redistribution::Allowed);
        let loads = |tr: Transcript| {
            ObservationStore::from_json(
                &ObservationStore::to_json(&t, &[tr], None).to_string(),
                LOAD_NOW,
                &pins(&t),
            )
        };
        // The reviewer's round: one observer, zero bond, zero slashes.
        let one = hand_made(
            START,
            ObserveConfig {
                commit_close: 0,
                reveal_close: 0,
                reward: 0,
                outlier_slash: 0,
                miss_slash: 0,
            },
            vec![Observer {
                address: obs_addr(0x42),
                bond: 0,
            }],
            99_999_999,
        );
        assert!(observations::check(&t, &one, LOAD_NOW).is_err());
        let e = loads(one).expect_err("refused at load");
        assert!(e.to_string().contains("coordinator could open"), "{e}");
        // Good config, but too few observers.
        assert!(loads(hand_made(START, good_cfg(START), observers(2, 1_000), 7)).is_err());
        // Bonds that do not cover the outlier slash.
        assert!(loads(hand_made(START, good_cfg(START), observers(3, 99), 7)).is_err());
        // Observers out of order (or repeated).
        let mut swapped = observers(3, 1_000);
        swapped.swap(0, 1);
        assert!(loads(hand_made(START, good_cfg(START), swapped, 7)).is_err());
        let mut twice = observers(3, 1_000);
        twice[1] = twice[0];
        assert!(loads(hand_made(START, good_cfg(START), twice, 7)).is_err());
        // The same rules, honestly met, load.
        assert!(loads(hand_made(START, good_cfg(START), observers(3, 1_000), 7)).is_ok());
    }

    /// L2API-R1-F2: a round's reveal close is bounded: never after the
    /// checker's clock (plus skew), never more than a day after the commit
    /// close, so a round "settling in the future" is never served. The reveal
    /// close is not the price clock (L2API-R2-F2, R4-F3): the price decays
    /// from the template-fixed decay start, here the round's window end.
    #[test]
    fn a_round_settling_in_the_future_is_refused_and_the_price_decays() {
        let t = template(Redistribution::Allowed);
        // A valid config whose reveal close is a day after LOAD_NOW.
        let late_start = (LOAD_NOW + 86_400) / 300 * 300;
        let tr = hand_made(late_start, good_cfg(late_start), observers(3, 1_000), 7);
        assert!(matches!(
            observations::check(&t, &tr, LOAD_NOW),
            Err(observations::ObservationError::NotSettledYet { .. })
        ));
        let text = ObservationStore::to_json(&t, std::slice::from_ref(&tr), None).to_string();
        assert!(ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).is_err());
        // Once the clock passes the reveal close, the same round loads.
        let after = tr.cfg.reveal_close;
        assert!(ObservationStore::from_json(&text, after, &pins(&t)).is_ok());
        // A reveal phase longer than MAX_REVEAL_SECS never loads.
        let mut long = good_cfg(START);
        long.reveal_close = long.commit_close + onyx_coordinator::observe::MAX_REVEAL_SECS + 1;
        let tr = hand_made(START, long, observers(3, 1_000), 7);
        assert!(observations::check(&t, &tr, u64::MAX).is_err());
        // And the honest round's price decays from its template-fixed decay
        // start (a fixed feed: its own window end), not from its reveal close.
        let mut c = Catalog::default();
        c.set_pricing(
            METRIC,
            PricingPolicy {
                base_price_micro_usdc: 2_500,
                fresh_window_secs: 3_600,
            },
        );
        let f = feed(Redistribution::Allowed);
        let settled = f
            .record("test.index", None, None)
            .expect("record")
            .checked
            .settled_unix;
        assert_eq!(c.observation_price(METRIC, settled, settled + 3_599), 2_500);
        assert_eq!(c.observation_price(METRIC, settled, settled + 3_600), 0);
    }

    /// L2API-R1-F3: a client asking for "newest" sets a freshness floor; an
    /// older round is refused even though it verifies.
    #[tokio::test]
    async fn a_round_older_than_the_clients_floor_is_refused() {
        let f = served(false, None, Redistribution::Allowed);
        let (_, _, b) = get(&f, URI, &[]).await;
        let (t, subj) = expect();
        let ask = |floor: u64| ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: floor,
            now: LOAD_NOW,
        };
        // The served round's window ends at 1,300.
        assert!(verify_observation(&b, &ask(1_800)).is_ok());
        assert_eq!(
            verify_observation(&b, &ask(1_801)),
            Err(VerifyError::NotRequested(
                "windowEnd older than min_window_end"
            ))
        );
        // A client clock before the round's reveal close (minus skew) refuses it.
        assert!(verify_observation(
            &b,
            &ObservationExpect {
                now: 1_980 - observations::MAX_CLOCK_SKEW_SECS - 1,
                ..ask(0)
            }
        )
        .is_err());
    }

    /// L2API-R1-F5: a paid observation answers no conditional request before
    /// payment, and no ETag names the result hash (a guessed value could be
    /// confirmed against it).
    #[tokio::test]
    async fn a_paid_observation_answers_no_304_before_payment() {
        let f = served(true, mock(), Redistribution::Allowed);
        let (s, h, _) = get(&f, URI, &[]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
        // Even the right ETag (and `*`) gets a 402, not a 304.
        let free = served(false, None, Redistribution::Allowed);
        let (s, fh, b) = get(&free, URI, &[]).await;
        assert_eq!(s, StatusCode::OK);
        let etag = fh[header::ETAG].to_str().expect("etag").to_string();
        assert!(!etag.contains(
            b["resultHash"]
                .as_str()
                .expect("hash")
                .trim_start_matches("0x")
        ));
        for tag in [etag.clone(), "*".to_string()] {
            let (s, _, _) = get(&f, URI, &[("if-none-match", tag)]).await;
            assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
        }
        assert!(!h.contains_key(header::ETAG) || h[header::ETAG] != etag.as_str());
        // Free data still revalidates.
        let (s, _, _) = get(&free, URI, &[("if-none-match", etag)]).await;
        assert_eq!(s, StatusCode::NOT_MODIFIED);
    }
}

/// AUD-1-F4: unauthenticated lookups of shards with no final root do not
/// reach the node once per request: a miss is cached, concurrent lookups of
/// one shard share one node call, and each client has a budget of uncached
/// lookups per minute.
#[tokio::test(start_paused = true)]
async fn unfinalized_lookups_are_bounded() {
    let calls = Arc::new(AtomicU64::new(0));
    let f = build(Setup {
        count_roots: Some(calls.clone()),
        config: ApiConfig {
            root_lookups_per_minute: 10,
            ..ApiConfig::default()
        },
        ..Setup::default()
    });
    // 15 newer shards of the metric that never finalize (the 16th
    // candidate is the finalized shard).
    {
        let mut c = f.state.catalog.write().expect("lock");
        for k in 0..15u64 {
            c.add_shard(meta(
                10 + k,
                200 + k,
                format!("never final {k}").as_bytes(),
                1,
            ));
        }
    }
    // The first request walks the candidates until its 10-lookup budget ends.
    let (s, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(code(&b), Some("rate_limited"));
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    // Repeating it within the miss TTL reaches the node no more for those 10.
    let before = calls.load(Ordering::SeqCst);
    for _ in 0..20 {
        let _ = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), before, "misses are cached");
    // Another client: 5 more candidates, then the final root: served.
    let other = IpAddr::from([203, 0, 113, 9]);
    let (s, _, b) = send(
        &f,
        Method::GET,
        "/v1/metrics/test.flow?subject=entity:a",
        &[],
        Some(other),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(calls.load(Ordering::SeqCst), before + 6);
}

/// AUD-1-F4: concurrent requests for one shard with no final root share one
/// node lookup.
#[tokio::test(start_paused = true)]
async fn concurrent_lookups_of_one_shard_share_one_node_call() {
    let calls = Arc::new(AtomicU64::new(0));
    let f = Arc::new(build(Setup {
        count_roots: Some(calls.clone()),
        ..Setup::default()
    }));
    let m = meta(9, 300, b"never final", 1);
    f.state.catalog.write().expect("lock").add_shard(m.clone());
    let uri = format!("/v1/proofs/9/{}/0", m.shard);
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let f = f.clone();
        let uri = uri.clone();
        tasks.push(tokio::spawn(async move { get(&f, &uri, &[]).await.0 }));
    }
    for t in tasks {
        assert_eq!(t.await.expect("task"), StatusCode::NOT_FOUND);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.caches.root_flights(), 0, "no lock left behind");
}

// ------------------------------------------------------------ serving checks

/// A gate that clears `entity:a` and `entity:b` and refuses `entity:c`, or
/// refuses the whole shard; it records the params it was handed. It claims
/// every shard with the test schema. With `chain` = n it judges like a
/// chain: fewer than n shards refuse them all (a missing link).
struct TestGate {
    refuse_shard: bool,
    seen: std::sync::Mutex<Vec<Option<Vec<u8>>>>,
    metric: &'static str,
    chain: AtomicU64,
    /// Leave out the refused row's clearance (a gate answering short, L9A-4).
    short: AtomicBool,
}

impl gate::ServingGate for TestGate {
    fn name(&self) -> &str {
        "test-gate"
    }
    fn metric(&self) -> &str {
        self.metric
    }
    fn claims(&self, meta: &ShardMeta) -> bool {
        meta.binding.schema_id == keccak256(b"schema")
    }
    fn judge(&self, shards: &[gate::GateShard]) -> Vec<gate::Judgement> {
        if (shards.len() as u64) < self.chain.load(Ordering::SeqCst) {
            return vec![Err("the chain has a gap".into()); shards.len()];
        }
        shards
            .iter()
            .map(|s| {
                if let Ok(mut seen) = self.seen.lock() {
                    seen.push(s.params.clone());
                }
                if self.refuse_shard {
                    return Err("forged base".into());
                }
                let short = self.short.load(Ordering::SeqCst);
                Ok(s.leaves
                    .iter()
                    .filter(|l| {
                        !short || OutputRow::try_from_slice(l).expect("row").key != b"entity:c"
                    })
                    .map(|l| {
                        let o = OutputRow::try_from_slice(l).expect("row");
                        let ok = o.key != b"entity:c";
                        gate::RowClearance {
                            key: o.key,
                            cross_check: if ok {
                                Ok(gate::CrossCheck {
                                    basis: "issuer-wallet-list".into(),
                                    source: "test list".into(),
                                })
                            } else {
                                Err("entity:c is not independently cross-checked".into())
                            },
                        }
                    })
                    .collect())
            })
            .collect()
    }
}

fn gated(refuse_shard: bool, paid: bool) -> (Fixture, Arc<TestGate>) {
    let g = Arc::new(TestGate {
        refuse_shard,
        seen: std::sync::Mutex::new(Vec::new()),
        metric: "test.flow",
        chain: AtomicU64::new(0),
        short: AtomicBool::new(false),
    });
    let f = build(Setup {
        paid,
        facilitator: paid.then(mock).flatten(),
        gate: Some(g.clone()),
        ..Setup::default()
    });
    (f, g)
}

/// R-GATE-1: a gated shard is not served before its serving check ran (503
/// pending, before any 402); after it, cleared rows carry `servingCheck`
/// (and the client reads it), an uncleared row is 403 `not_cross_checked`
/// before any 402, and an export with such a row is refused.
#[tokio::test(start_paused = true)]
async fn a_gated_metric_serves_only_cleared_rows() {
    let (f, g) = gated(false, true);
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("serving_check_pending")
        )
    );
    // The params come from the store by the params hash.
    f.cas.put(Kind::Blob, b"params one").expect("params");
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
    assert_eq!(
        *g.seen.lock().expect("lock"),
        vec![Some(b"params one".to_vec())]
    );
    // Refused row: 403 before the 402, nothing charged.
    let (st, h, b) = get(&f, "/v1/metrics/test.flow?subject=entity:c", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    let (st, h, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    let pay = pay(&h, "payer a", 1);
    let (st, _, b) = get(
        &f,
        "/v1/metrics/test.flow?subject=entity:a",
        &[(x402::PAYMENT_SIGNATURE, pay)],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["servingCheck"]["gate"], "test-gate");
    assert_eq!(b["servingCheck"]["crossChecked"], true);
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    let claim = v.require_cross_checked().expect("claimed");
    assert_eq!(
        claim.bases,
        vec![("issuer-wallet-list".to_string(), "test list".to_string())]
    );
    assert!(v.to_string().contains("cross_checked=true"));
    // A forged claim shape is refused by the client.
    let mut bad = b.clone();
    bad["servingCheck"]["crossChecked"] = json!(false);
    assert!(verify_response_unchecked(
        &bad,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .is_err());
    // Proof by index: row 2 (entity:c) is refused before any 402.
    let uri = format!("/v1/proofs/1/{}/2", f.shard);
    let (st, h, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    assert!(h.get(x402::PAYMENT_REQUIRED).is_none());
    // An export would carry entity:c: refused.
    let uri = format!("/v1/export/1/{}", f.shard);
    let (st, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    // Health shows the run.
    let (_, _, h) = get(&f, "/v1/health", &[]).await;
    assert_eq!(h["servingChecks"][0]["cleared"], 1);
}

/// THREATS L9A-4: a gate whose clearance list is shorter than the shard
/// (here: the refused row left out) clears nothing, so an export, which walks
/// the clearance list, can never carry the uncleared row.
#[tokio::test(start_paused = true)]
async fn a_gate_answering_fewer_rows_than_leaves_clears_nothing() {
    let (f, g) = gated(false, false);
    f.cas.put(Kind::Blob, b"params one").expect("params");
    g.short.store(true, Ordering::SeqCst);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 0));
    assert!(runs[0]
        .first_refusal
        .as_deref()
        .is_some_and(|m| m.contains("cleared 2 of 3 rows")));
    for uri in [
        format!("/v1/export/1/{}", f.shard),
        "/v1/metrics/test.flow?subject=entity:a".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_failed")),
            "{uri}"
        );
    }
    // A full answer clears the shard again.
    g.short.store(false, Ordering::SeqCst);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
}

/// R-GATE-2: a shard the serving check refused is never served, whatever
/// the endpoint; a shard naming a gate the server lacks is refused (fail
/// closed); a client that requires a cross-check refuses an ungated answer.
#[tokio::test(start_paused = true)]
async fn a_refused_or_unchecked_shard_is_not_served() {
    let (f, _) = gated(true, false);
    gate::refresh(&f.state).await;
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(b["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("forged base")));
    for uri in [
        format!("/v1/proofs/1/{}/0", f.shard),
        format!("/v1/export/1/{}", f.shard),
        "/v1/settlement/okx-flow".to_string(),
    ] {
        let (st, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_failed")),
            "{uri}"
        );
    }
    // No gate on this server, but the shard names one: refused.
    let f = build(Setup {
        serving_check: Some("btc-entity-reserve-v1"),
        ..Setup::default()
    });
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_unavailable"))
    );
    // Ungated: served without a claim, and a client requiring one refuses it.
    let f = fixture(false, None);
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK);
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: "test.flow",
            def: flow_def(b"params one"),
            subject: "entity:a",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    assert_eq!(v.serving, None);
    assert!(v.require_cross_checked().is_err());
}

/// R-GATE-3: an answer is tied to the root it judged: when the chain's root
/// for the shard changes (the catalog row stays), the old answer no longer
/// serves it.
#[tokio::test(start_paused = true)]
async fn a_serving_answer_is_bound_to_the_root_it_judged() {
    let (f, _) = gated(false, false);
    gate::refresh(&f.state).await;
    let (st, _, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK);
    let other = f
        .cas
        .put_rows(&[row("entity:a", b"other value....")])
        .expect("rows");
    let gs = &f.state.gates;
    let m = f
        .state
        .catalog
        .read()
        .expect("lock")
        .shard(1, &f.shard)
        .cloned()
        .expect("meta");
    assert!(matches!(gate::cleared(gs, &m, &f.root), Ok(Some(_))));
    assert!(matches!(
        gate::cleared(gs, &m, &other),
        Err(gate::Refusal::Pending)
    ));
}

fn gate_with(metric: &'static str, chain: u64) -> Arc<TestGate> {
    Arc::new(TestGate {
        refuse_shard: false,
        seen: std::sync::Mutex::new(Vec::new()),
        metric,
        chain: AtomicU64::new(chain),
        short: AtomicBool::new(false),
    })
}

fn shard_meta(f: &Fixture, round: u64, shard: &Hash32) -> ShardMeta {
    f.state
        .catalog
        .read()
        .expect("lock")
        .shard(round, shard)
        .cloned()
        .expect("meta")
}

/// R-GATE-4 (review RESAPI-R1 F1): a gate claims shards by their binding,
/// not only by the catalog's metric name or `servingCheck`: a shard listed
/// under another metric, naming no check, is still judged before any row is
/// served; a shard two different gates apply to is refused.
#[tokio::test(start_paused = true)]
async fn a_gate_claims_a_shard_by_its_binding_not_its_catalog_name() {
    let f = build(Setup {
        gate: Some(gate_with("another.metric", 0)),
        ..Setup::default()
    });
    let uri = "/v1/metrics/test.flow?subject=entity:a";
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("serving_check_pending")
        ),
        "{b}"
    );
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (1, 1));
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["servingCheck"]["gate"], "test-gate");
    // The catalog names another check than the gate claiming the binding.
    let f = build(Setup {
        gate: Some(gate_with("another.metric", 0)),
        serving_check: Some("some-other-check"),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    let (st, _, b) = get(&f, uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(b["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("several serving checks")));
}

/// R-GATE-5 (review RESAPI-R1 F1): the known gated bindings are compiled
/// into every server, so one without the gate still knows a reserve shard
/// by its schema id or recipe id.
#[test]
fn known_gates_are_well_formed_and_claim_by_binding() {
    let mut names = std::collections::BTreeSet::new();
    for k in gate::KNOWN_GATES {
        assert!(names.insert(k.name), "duplicate {}", k.name);
        assert!(!k.schemas.is_empty());
        for r in k.recipe_ids {
            let mut b = [0u8; 32];
            hex::decode_to_slice(r.trim_start_matches("0x"), &mut b).expect("32-byte hex");
            assert_ne!(b, [0u8; 32]);
        }
    }
    let mut m = meta(1, 100, b"params one", 3);
    assert_eq!(gate::known_claim(&m), None);
    m.binding.schema_id = keccak256(b"onyx.btc.entity-reserve-row.v1");
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    let mut m = meta(1, 100, b"params one", 3);
    let mut r = [0u8; 32];
    hex::decode_to_slice(
        "d0c1331f82f9a743548ca68de3693a2754ae35e6faef7e047a739ba939b49e34",
        &mut r,
    )
    .expect("hex");
    m.binding.recipe_id = Hash32(r);
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    // A server with no gate refuses it (fail closed), whatever the metric.
    let gs = gate::Gates::default();
    assert_eq!(
        gate::cleared(&gs, &m, &Hash32::ZERO).err(),
        Some(gate::Refusal::Unavailable("btc-entity-reserve-v1".into()))
    );
}

/// R-GATE-7 (review RESAPI-R2-1, the review's scenario): a later reserve
/// build that `KNOWN_GATES` does not list (another recipe id, another schema
/// id) listed under `btc.entity-reserve` on a server without the gate (plain
/// `onyx-node`) was served unchecked. The known gate now also claims its
/// metric names, so every endpoint refuses it (403
/// `serving_check_unavailable`, before any 402); the same shard under
/// another metric is not a reserve shard to this server.
#[tokio::test(start_paused = true)]
async fn a_gated_metric_name_fails_closed_on_any_server() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        metric: Some("btc.entity-reserve"),
        ..Setup::default()
    });
    let m = shard_meta(&f, 1, &f.shard);
    assert_ne!(
        m.binding.schema_id,
        keccak256(b"onyx.btc.entity-reserve-row.v1")
    );
    assert_eq!(gate::known_claim(&m), Some("btc-entity-reserve-v1"));
    for uri in [
        "/v1/metrics/btc.entity-reserve?subject=entity:a".to_string(),
        format!("/v1/proofs/1/{}/0", f.shard),
        format!("/v1/export/1/{}", f.shard),
    ] {
        let (st, h, b) = get(&f, &uri, &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::FORBIDDEN, Some("serving_check_unavailable")),
            "{uri}: {b}"
        );
        assert!(h.get(x402::PAYMENT_REQUIRED).is_none(), "{uri}: no 402");
    }
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        ..Setup::default()
    });
    let (st, _, _) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
}

/// R-GATE-8 (review RESAPI-R2-2): when the indexer takes a shard out of the
/// catalog (two reads refuted its final view), the gates run again at once,
/// so the later links of a chain judged with it lose their pass; before,
/// they kept it until the next timed run.
#[tokio::test(start_paused = true)]
async fn an_indexer_removal_runs_the_gates_again() {
    let (f, g) = gated(false, false);
    // A chain of two shards: the gate refuses a chain with a missing link.
    g.chain.store(2, Ordering::SeqCst);
    let rows = vec![row("entity:a", b"value a2.......")];
    let root2 = f.cas.put_rows(&rows).expect("rows");
    let m2 = meta(2, 200, b"params two", 1);
    let shard2 = m2.shard;
    f.roots.insert(
        2,
        shard2,
        FinalRoot {
            root: root2,
            finalized_at: 500,
            finalized_unix: 1_100,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2);
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (2, 2), "{runs:?}");
    let uri = format!("/v1/proofs/2/{shard2}/0");
    assert_eq!(get(&f, &uri, &[]).await.0, StatusCode::OK);
    // The indexer took the first link out; a poll that changed nothing
    // else and is not a 20th poll.
    let m1 = shard_meta(&f, 1, &f.shard);
    f.state
        .catalog
        .write()
        .expect("lock")
        .remove_shard(1, &f.shard);
    let quiet = crate::indexer::PollReport::default();
    assert!(gate::after_poll(&f.state, &quiet, 3).await.is_none());
    let report = crate::indexer::PollReport {
        removed: vec![(1, f.shard)],
        ..Default::default()
    };
    let runs = gate::after_poll(&f.state, &report, 3)
        .await
        .expect("a removal runs the gates");
    assert_eq!(runs[0].cleared, 0, "{runs:?}");
    // The removed link's own answer is gone too (mutation L4).
    assert_eq!(
        gate::cleared(&f.state.gates, &m1, &f.root).err(),
        Some(gate::Refusal::Pending)
    );
    let (st, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("serving_check_failed")),
        "{b}"
    );
}

/// R-GATE-6 (review RESAPI-R1 F3): a root read that fails during a refresh
/// is not "not final": the shard is judged at its last judged root, so one
/// node error does not refuse a chain.
#[tokio::test(start_paused = true)]
async fn a_failed_root_read_keeps_the_shard_at_its_last_judged_root() {
    let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 1)),
        fail_roots: Some(fail.clone()),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    let m = shard_meta(&f, 1, &f.shard);
    assert!(matches!(
        gate::cleared(&f.state.gates, &m, &f.root),
        Ok(Some(_))
    ));
    fail.lock().expect("lock").insert(f.shard);
    let runs = gate::refresh(&f.state).await;
    fail.lock().expect("lock").clear();
    let (st, _, b) = get(&f, "/v1/metrics/test.flow?subject=entity:a", &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (runs[0].read_errors, runs[0].judged, runs[0].cleared),
        (1, 1, 1)
    );
}

/// R-GATE-7 (review RESAPI-R1 F3): when a shard never judged cannot be read
/// (its link is missing, so a chain check would refuse everything), the run
/// stores no new verdicts: unchanged shards keep their answers, the new one
/// is pending, and the next good run judges both.
#[tokio::test(start_paused = true)]
async fn a_failed_read_of_a_new_shard_leaves_it_pending_and_keeps_the_rest() {
    let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let g = gate_with("test.flow", 1);
    let f = build(Setup {
        gate: Some(g.clone()),
        fail_roots: Some(fail.clone()),
        ..Setup::default()
    });
    gate::refresh(&f.state).await;
    // A second shard of the chain is listed; its root read fails.
    let rows2 = vec![row("entity:a", b"value a two....")];
    let root2 = f.cas.put_rows(&rows2).expect("rows");
    let m2 = meta(2, 101, b"params two", 1);
    f.roots.insert(
        2,
        m2.shard,
        FinalRoot {
            root: root2,
            finalized_at: 401,
            finalized_unix: 1_001,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2.clone());
    g.chain.store(2, Ordering::SeqCst);
    fail.lock().expect("lock").insert(m2.shard);
    let runs = gate::refresh(&f.state).await;
    let m1 = shard_meta(&f, 1, &f.shard);
    assert!(
        matches!(gate::cleared(&f.state.gates, &m1, &f.root), Ok(Some(_))),
        "the unchanged shard keeps its clearance"
    );
    assert_eq!(
        gate::cleared(&f.state.gates, &m2, &root2).err(),
        Some(gate::Refusal::Pending)
    );
    assert_eq!((runs[0].read_errors, runs[0].judged), (1, 0), "{runs:?}");
    // The unread shard is counted as skipped, and the kept clearance in
    // the run's note (mutation L4).
    assert_eq!(runs[0].skipped, 1, "{runs:?}");
    let note = runs[0].first_refusal.clone().unwrap_or_default();
    assert!(
        note.starts_with("1 root read(s) failed")
            && note.contains(": 1 previous clearance(s) kept"),
        "{note}"
    );
    fail.lock().expect("lock").clear();
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (2, 2));
    assert!(matches!(
        gate::cleared(&f.state.gates, &m2, &root2),
        Ok(Some(_))
    ));
}

/// Mutation L4: in a run left incomplete by an unread new shard, a judged
/// shard whose root changed, or that left the catalog, loses its old
/// answer (and is not counted as kept).
#[tokio::test(start_paused = true)]
async fn an_incomplete_run_drops_answers_that_no_longer_hold() {
    for removed in [false, true] {
        let fail = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let g = gate_with("test.flow", 1);
        let f = build(Setup {
            gate: Some(g.clone()),
            fail_roots: Some(fail.clone()),
            ..Setup::default()
        });
        gate::refresh(&f.state).await;
        let m1 = shard_meta(&f, 1, &f.shard);
        assert!(matches!(
            gate::cleared(&f.state.gates, &m1, &f.root),
            Ok(Some(_))
        ));
        if removed {
            f.state
                .catalog
                .write()
                .expect("lock")
                .remove_shard(1, &f.shard);
        } else {
            let other = f
                .cas
                .put_rows(&[row("entity:a", b"another value..")])
                .expect("rows");
            f.roots.insert(
                1,
                f.shard,
                FinalRoot {
                    root: other,
                    finalized_at: 402,
                    finalized_unix: 1_002,
                    binding: None,
                    tx: None,
                },
            );
        }
        let rows2 = vec![row("entity:a", b"value a two....")];
        let root2 = f.cas.put_rows(&rows2).expect("rows");
        let m2 = meta(2, 101, b"params two", 1);
        f.roots.insert(
            2,
            m2.shard,
            FinalRoot {
                root: root2,
                finalized_at: 401,
                finalized_unix: 1_001,
                binding: None,
                tx: None,
            },
        );
        f.state.catalog.write().expect("lock").add_shard(m2.clone());
        fail.lock().expect("lock").insert(m2.shard);
        let runs = gate::refresh(&f.state).await;
        assert_eq!(runs[0].judged, 0, "{runs:?}");
        let note = runs[0].first_refusal.clone().unwrap_or_default();
        assert!(note.contains(": 0 previous clearance(s) kept"), "{note}");
        assert_eq!(
            gate::cleared(&f.state.gates, &m1, &f.root).err(),
            Some(gate::Refusal::Pending),
            "removed: {removed}"
        );
    }
}

/// Mutation L4: a final shard whose rows are not in the store is skipped
/// (counted, never judged or cleared).
#[tokio::test(start_paused = true)]
async fn a_final_shard_without_its_rows_is_skipped() {
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 0)),
        ..Setup::default()
    });
    let rows2 = vec![row("entity:a", b"value a two....")];
    let root2 = onyx_merkle::MerkleTree::from_rows(&rows2).root();
    let m2 = meta(2, 101, b"params two", 1);
    f.roots.insert(
        2,
        m2.shard,
        FinalRoot {
            root: root2,
            finalized_at: 401,
            finalized_unix: 1_001,
            binding: None,
            tx: None,
        },
    );
    f.state.catalog.write().expect("lock").add_shard(m2.clone());
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].skipped), (1, 1), "{runs:?}");
    assert_eq!(
        gate::cleared(&f.state.gates, &m2, &root2).err(),
        Some(gate::Refusal::Pending)
    );
}

/// Mutation L4: a gate's name and its metric are each registered once.
#[test]
fn a_gate_name_or_metric_is_registered_once() {
    let mut g = gate::Gates::default();
    assert!(g.is_empty());
    g.add(gate_with("test.flow", 0)).expect("first");
    assert!(!g.is_empty());
    // Same name ("test-gate"), another metric.
    assert!(g.add(gate_with("test.other", 0)).is_err());
    // Same metric under another name.
    struct Renamed(Arc<TestGate>);
    impl gate::ServingGate for Renamed {
        fn name(&self) -> &str {
            "another-gate"
        }
        fn metric(&self) -> &str {
            self.0.metric
        }
        fn claims(&self, meta: &ShardMeta) -> bool {
            self.0.claims(meta)
        }
        fn judge(&self, shards: &[gate::GateShard]) -> Vec<gate::Judgement> {
            self.0.judge(shards)
        }
    }
    assert!(g.add(Arc::new(Renamed(gate_with("test.flow", 0)))).is_err());
    g.add(Arc::new(Renamed(gate_with("test.third", 0))))
        .expect("new name and metric");
}

/// R-GATE-8 (review RESAPI-R1 F3): a listed shard that finalizes after the
/// start is judged by the periodic refresh, without a restart or an indexer.
#[tokio::test(start_paused = true)]
async fn a_listed_shard_that_finalizes_later_is_judged_without_a_restart() {
    let f = build(Setup {
        gate: Some(gate_with("test.flow", 0)),
        unfinal: true,
        ..Setup::default()
    });
    let runs = gate::refresh(&f.state).await;
    assert_eq!((runs[0].judged, runs[0].skipped), (0, 1));
    let task = gate::spawn_refresh(f.state.clone(), Duration::from_millis(10), |_| {});
    f.roots.insert(
        1,
        f.shard,
        FinalRoot {
            root: f.root,
            finalized_at: 400,
            finalized_unix: 1_000,
            binding: None,
            tx: None,
        },
    );
    let uri = "/v1/metrics/test.flow?subject=entity:a";
    let mut st = StatusCode::SERVICE_UNAVAILABLE;
    for _ in 0..200 {
        st = get(&f, uri, &[]).await.0;
        if st != StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();
    assert_eq!(st, StatusCode::OK);
}

/// The token as the API's node sees it, for the authorization check after
/// an unknown settle (M7-F5 residual): block `n` at time `1_000 + 2 n`, head
/// 1,000; the authorization (if used) in block 500, moving `value` to
/// `paid_to`. EIP-3009 lets a nonce move money once: `land` refuses a
/// second use.
struct FakeToken {
    token: [u8; 20],
    from: [u8; 20],
    nonce: [u8; 32],
    value: AtomicU64,
    paid_to: std::sync::Mutex<Option<[u8; 20]>>,
    transfers: AtomicU64,
    down: std::sync::atomic::AtomicBool,
    state_reads: AtomicU64,
}

const FAKE_TOKEN: [u8; 20] = [0x36; 20];
const PAID_TX: &str = "0x7a11000000000000000000000000000000000000000000000000000000000001";

impl FakeToken {
    fn new(nonce: u8) -> Arc<Self> {
        Arc::new(FakeToken {
            token: FAKE_TOKEN,
            from: addr_bytes("payer a"),
            nonce: [nonce; 32],
            value: AtomicU64::new(0),
            paid_to: std::sync::Mutex::new(None),
            transfers: AtomicU64::new(0),
            down: std::sync::atomic::AtomicBool::new(false),
            state_reads: AtomicU64::new(0),
        })
    }

    /// The authorization moves its value to `to`, unless it was used.
    fn land(&self, to: [u8; 20]) -> bool {
        let mut p = self.paid_to.lock().expect("lock");
        if p.is_some() {
            return false;
        }
        *p = Some(to);
        self.transfers.fetch_add(1, Ordering::SeqCst);
        true
    }
}

impl roots::JsonRpc for FakeToken {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let q = |v: &Value| {
            u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
                .unwrap_or(0)
        };
        let paid = *self.paid_to.lock().expect("lock");
        let r = if self.down.load(Ordering::SeqCst) {
            Err("node: connection refused".to_string())
        } else {
            match method {
                "eth_call" => {
                    self.state_reads.fetch_add(1, Ordering::SeqCst);
                    Ok(json!(format!("0x{:064x}", u8::from(paid.is_some()))))
                }
                "eth_blockNumber" => Ok(json!("0x3e8")),
                "eth_getBlockByNumber" => {
                    let n = q(&params[0]);
                    Ok(
                        json!({ "number": format!("0x{n:x}"), "timestamp": format!("0x{:x}", 1_000 + 2 * n) }),
                    )
                }
                "eth_getLogs" => {
                    let (f, t) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                    Ok(if paid.is_some() && (f..=t).contains(&500) {
                        json!([{ "address": hex0x(&self.token), "transactionHash": PAID_TX }])
                    } else {
                        json!([])
                    })
                }
                "eth_getTransactionReceipt" => {
                    let to = paid.unwrap_or_default();
                    Ok(json!({"status": "0x1", "logs": [{
                        "address": hex0x(&self.token),
                        "topics": [hex0x(&crate::arrival::authorization_used_topic()), hex0x(&crate::arrival::word_addr(&self.from)), hex0x(&self.nonce)],
                        "data": "0x",
                    }, {
                        "address": hex0x(&self.token),
                        "topics": [hex0x(&crate::arrival::transfer_topic()), hex0x(&crate::arrival::word_addr(&self.from)), hex0x(&crate::arrival::word_addr(&to))],
                        "data": hex0x(&crate::arrival::word_u128(u128::from(self.value.load(Ordering::SeqCst)))),
                    }]}))
                }
                _ => Err(format!("unexpected {method}")),
            }
        };
        Box::pin(async move { r })
    }
}

impl roots::JsonRpc for Arc<FakeToken> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        self.as_ref().call(method, params)
    }
}

/// What one facilitator settle does, in call order (the last repeats).
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Never answers; nothing lands.
    Hang,
    /// The transfer lands in OnyxRevenue, then the answer is lost.
    LandThenHang,
    /// The authorization pays someone else, then the answer is lost.
    ElsewhereThenHang,
    /// The transfer lands and the facilitator says so.
    Settle,
    /// The earlier settle, still in a mempool, lands first; this one is
    /// refused by the token (the nonce is used).
    EarlierLandsThenRefused,
    /// Refused for a reason that is not a used nonce; nothing lands.
    RefusedOther,
}

struct Scripted {
    inner: Arc<Counting>,
    chain: Arc<FakeToken>,
    revenue: [u8; 20],
    steps: std::sync::Mutex<std::collections::VecDeque<Step>>,
    calls: AtomicU64,
}

impl Facilitator for Scripted {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = {
            let mut q = self.steps.lock().expect("lock");
            if q.len() > 1 {
                q.pop_front().expect("step")
            } else {
                *q.front().expect("step")
            }
        };
        Box::pin(async move {
            let refused = |reason: &str| SettleResponse {
                success: false,
                error_reason: Some(reason.into()),
                payer: Some(addr("payer a")),
                transaction: String::new(),
                network: BASE_SEPOLIA.into(),
            };
            match step {
                Step::Hang => std::future::pending::<()>().await,
                Step::LandThenHang => {
                    self.chain.land(self.revenue);
                    std::future::pending::<()>().await
                }
                Step::ElsewhereThenHang => {
                    self.chain.land([0x99; 20]);
                    std::future::pending::<()>().await
                }
                Step::Settle => {
                    if !self.chain.land(self.revenue) {
                        return Ok(refused("invalid_transaction_state"));
                    }
                }
                Step::EarlierLandsThenRefused => {
                    self.chain.land(self.revenue);
                    return Ok(refused("invalid_transaction_state"));
                }
                Step::RefusedOther => return Ok(refused("unexpected_settle_error")),
            }
            self.inner.settle(r).await
        })
    }
}

/// A paid server whose facilitator follows `steps`, with the authorization
/// check over `chain` (or none).
fn scripted(steps: &[Step], chain: &Arc<FakeToken>, check: bool) -> (Fixture, Arc<Scripted>) {
    let revenue = addr_bytes_of(PAY_TO);
    let fac = Arc::new(Scripted {
        inner: counting(),
        chain: chain.clone(),
        revenue,
        steps: std::sync::Mutex::new(steps.iter().copied().collect()),
        calls: AtomicU64::new(0),
    });
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_millis(2_000),
            ..ApiConfig::default()
        },
        settle_check: check.then(|| {
            Arc::new(crate::settle_check::ChainSettleCheck::new(
                chain.clone(),
                FAKE_TOKEN,
                revenue,
            )) as Arc<dyn crate::settle_check::SettleCheck>
        }),
        ..Setup::default()
    });
    (f, fac)
}

fn addr_bytes_of(hex_addr: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    a.copy_from_slice(&hex::decode(hex_addr.trim_start_matches("0x")).expect("hex"));
    a
}

/// One paid request with nonce `nonce`, the fake token told its value.
async fn paid_once(f: &Fixture, chain: &FakeToken, nonce: u8) -> (StatusCode, HeaderMap, Value) {
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    chain.value.store(
        pr.accepts[0].amount.parse().expect("amount"),
        Ordering::SeqCst,
    );
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))];
    get(f, &uri, &sig).await
}

/// M7-F5 residual (R-SETTLE-CHECK-1): a settle that landed but whose answer
/// was lost (a timeout) is not left unknown: `authorizationState` says
/// used, the token's logs show the transfer into OnyxRevenue, and the
/// request is served with that transaction; the receipt is written and the
/// intent ends. Nothing is re-sent. Without the check the payer was charged
/// and got 503.
#[tokio::test(start_paused = true)]
async fn a_timed_out_settle_that_landed_is_served_from_the_token() {
    let chain = FakeToken::new(41);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 41).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert!(s.success);
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never re-sent");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "the receipt is written");
    assert_eq!(f.state.intents.open_count(), 0, "the intent ended");
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 0);
    // Without the check the same settle stays unknown (the M7-F5 residual).
    let chain = FakeToken::new(42);
    let (f, _) = scripted(&[Step::LandThenHang], &chain, false);
    let (st, _, b) = paid_once(&f, &chain, 42).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
}

/// M7-F5 residual (R-SETTLE-CHECK-2): a timed-out settle whose
/// authorization is still unused is sent once more (the same signed
/// authorization: its nonce moves money once); success serves the request.
/// The payer is charged once.
#[tokio::test(start_paused = true)]
async fn an_unused_authorization_is_re_sent_once() {
    let chain = FakeToken::new(43);
    let (f, fac) = scripted(&[Step::Hang, Step::Settle], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 43).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
    assert_eq!(f.state.intents.open_count(), 0);
}

/// M7-F5 residual (R-SETTLE-CHECK-3, never charged twice): the first settle
/// was still in a mempool when the server re-sent it. The earlier one lands,
/// the re-send is refused by the token, and the second check finds the
/// payment: served, one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn a_re_send_racing_the_first_settle_charges_once_and_serves() {
    let chain = FakeToken::new(44);
    let (f, fac) = scripted(&[Step::Hang, Step::EarlierLandsThenRefused], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 44).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send only");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert_eq!(
        chain.state_reads.load(Ordering::SeqCst),
        2,
        "checked before and after"
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7-F5 residual (R-SETTLE-CHECK-4, never served unpaid): an authorization
/// used to pay someone else is the payer's refusal (402, nothing served, no
/// receipt); one that stays unused after the one re-send, or a node that
/// cannot be read, stays unknown (503, the unit spent, the intent left to
/// the resolver).
#[tokio::test(start_paused = true)]
async fn a_used_elsewhere_or_unread_authorization_is_never_served() {
    let chain = FakeToken::new(45);
    let (f, fac) = scripted(&[Step::ElsewhereThenHang], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 45).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("payment_failed")),
        "{b}"
    );
    assert!(b.get("row").is_none());
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    // Still unused after the re-send.
    let chain = FakeToken::new(46);
    let (f, fac) = scripted(&[Step::Hang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 46).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        ),
        "{b}"
    );
    assert!(
        b["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("still unused")),
        "{b}"
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send, not more");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 0);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.intents.open_count(), 1, "left to the resolver");
    // A node error decides nothing: no re-send, unknown.
    let chain = FakeToken::new(47);
    chain.down.store(true, Ordering::SeqCst);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 47).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        ),
        "{b}"
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.intents.open_count(), 1);
}

/// A node behind the one that mined the settle (M7L3-R1-1): its
/// `authorizationState` reads "unused" for the next `lag` reads, or for
/// `window` after its first read, even after the transfer landed (a
/// load-balanced backend a block behind). Its logs are current.
struct Lagging {
    chain: Arc<FakeToken>,
    lag: AtomicU64,
    window: Option<(Duration, std::sync::Mutex<Option<tokio::time::Instant>>)>,
}

impl roots::JsonRpc for Lagging {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        if method == "eth_call" {
            let behind = match &self.window {
                Some((d, start)) => {
                    let mut s = start.lock().expect("lock");
                    let t0 = *s.get_or_insert_with(tokio::time::Instant::now);
                    t0.elapsed() < *d
                }
                None => self
                    .lag
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok(),
            };
            if behind {
                return Box::pin(async { Ok(json!(format!("0x{:064x}", 0))) });
            }
        }
        self.chain.call(method, params)
    }
}

/// A paid server whose facilitator follows `steps`, checking through a node
/// that lags as `node` says; the wait before a re-check is `wait`.
fn lagging(
    steps: &[Step],
    chain: &Arc<FakeToken>,
    node: Lagging,
    wait: Duration,
) -> (Fixture, Arc<Scripted>) {
    lagging_until(steps, chain, node, wait, Duration::from_millis(2_000))
}

/// [`lagging`] with the request deadline `request_timeout`.
fn lagging_until(
    steps: &[Step],
    chain: &Arc<FakeToken>,
    node: Lagging,
    wait: Duration,
    request_timeout: Duration,
) -> (Fixture, Arc<Scripted>) {
    let revenue = addr_bytes_of(PAY_TO);
    let fac = Arc::new(Scripted {
        inner: counting(),
        chain: chain.clone(),
        revenue,
        steps: std::sync::Mutex::new(steps.iter().copied().collect()),
        calls: AtomicU64::new(0),
    });
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout,
            settle_recheck_wait: wait,
            ..ApiConfig::default()
        },
        settle_check: Some(Arc::new(crate::settle_check::ChainSettleCheck::new(
            node, FAKE_TOKEN, revenue,
        ))),
        ..Setup::default()
    });
    (f, fac)
}

fn behind_by(chain: &Arc<FakeToken>, reads: u64) -> Lagging {
    Lagging {
        chain: chain.clone(),
        lag: AtomicU64::new(reads),
        window: None,
    }
}

/// M7L3-R1-1 (b) (review PoC `lagging_state_read_after_landed_settle`):
/// the first settle landed, its answer was lost, and the node read
/// "unused" on both checks. The re-send was refused (nonce used), the
/// second check said "still unused", the payer was charged and got 503, and
/// the identical retry got 409 "nothing was charged". A re-send the token
/// refused as used now has the second check read the token's logs: the
/// transfer into OnyxRevenue is found and the request is served, charged
/// once, one receipt.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_a_refused_re_send_reads_the_logs_past_a_lagging_node() {
    let chain = FakeToken::new(61);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang, Step::Settle],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, h, b) = paid_once(&f, &chain, 61).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
}

/// M7L3-R1-1 (c): a payment whose settle ended unknown (the node lagged
/// through both checks, the re-send's answer was lost) is served on its
/// identical retry once the token proves the transfer: the retry runs the
/// authorization check instead of getting 409, never settles again, and is
/// charged once with one receipt. Another request with the same
/// authorization still gets 409.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_the_identical_retry_of_an_unknown_settle_is_checked_and_served() {
    let chain = FakeToken::new(64);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 64).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.intents.open_count(), 1);
    // Another resource with the same authorization: still one settle.
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, ho, _) = get(&f, &other, &[]).await;
    let (st, _, b) = get(
        &f,
        &other,
        &[(x402::PAYMENT_SIGNATURE, pay(&ho, "payer a", 64))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::CONFLICT, Some("payment_in_use")),
        "{b}"
    );
    // The identical retry (same header, resource and client).
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 64))];
    let (st, h, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "never settled again");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
    // A third identical request replays the settled answer.
    let (st, _, _) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7L3-R1-1 (a): the check after a re-send waits about one block first.
/// The node reads "unused" for 40 ms after its first read; the re-send is
/// refused for another reason (so the logs are not read regardless). With
/// the wait the second check sees the landed transfer and serves; an
/// immediate second check read "still unused" (503, charged).
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_the_second_check_waits_one_block() {
    let chain = FakeToken::new(65);
    let node = Lagging {
        chain: chain.clone(),
        lag: AtomicU64::new(0),
        window: Some((Duration::from_millis(40), std::sync::Mutex::new(None))),
    };
    let (f, fac) = lagging(
        &[Step::LandThenHang, Step::RefusedOther],
        &chain,
        node,
        Duration::from_millis(90),
    );
    assert_eq!(f.state.settle_recheck_wait(), Duration::from_millis(90));
    let (st, _, b) = paid_once(&f, &chain, 65).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7L3-R1-1 (review PoC `wrong_value_is_not_served`): a transfer into
/// OnyxRevenue for another value is not proof of this payment: never
/// served, no receipt, also not on the identical retry.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_a_transfer_of_another_value_is_never_served() {
    let chain = FakeToken::new(63);
    let (f, _) = scripted(&[Step::LandThenHang], &chain, true);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    let v: u64 = pr.accepts[0].amount.parse().expect("amount");
    chain.value.store(v + 1, Ordering::SeqCst);
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 63))];
    for _ in 0..2 {
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_ne!(st, StatusCode::OK, "{b}");
        assert!(b.get("row").is_none());
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.receipts.pending(), 0);
    }
}

/// M7L3-R2-1 (review PoC A): the resolver (every 30 s by default) recovered
/// the payment of a request whose settle ended unknown before the payer's
/// identical retry came. The intent is gone, so the retry used to take the
/// fresh path: a new verify and settle (with a real facilitator, 402 on the
/// used nonce) although the payer was charged. The resolver now keeps a
/// proof for that request: the retry is served from it, with the
/// transaction that paid, and no new settle; one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn m7l3_r2_1_the_identical_retry_after_the_resolver_is_served_from_its_proof() {
    let chain = FakeToken::new(68);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 68).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    let out = r.resolve_once().await;
    assert_eq!(out.recovered, 1, "{out:?}");
    assert_eq!(f.state.intents.open_count(), 0);
    assert_eq!(f.state.receipts.pending(), 1);
    // The fake chain's clock is not the fixture's: as if they agreed.
    f.state.intents.set_skew(0);
    let calls = fac.calls.load(Ordering::SeqCst);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 68))];
    let (st, h, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert!(s.success);
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
    // Further identical retries replay the stored answer; the proof is
    // used once.
    let (st, _, _) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// SOAK-1 (L6 soak, found with an RPC proxy that drops answers): without
/// the authorization check (a self-settling facilitator, ADR-0026), a
/// settle that landed but whose answer was lost left its intent open, and
/// the payer's identical retry took the fresh path and got 409
/// `payment_in_use` "already being settled for another request; this
/// request added no charge", though it is the same request and the payer
/// was charged: a client trusting it signs a new authorization and pays
/// twice. The identical retry now gets 503 `payment_pending` with
/// `Retry-After`, never settles again, and once the resolver proved the
/// transfer it is served from that proof: one settle, one transfer, one
/// receipt. Another request with the same authorization still gets 409.
#[tokio::test(start_paused = true)]
async fn soak_1_the_identical_retry_of_an_unknown_self_settle_is_told_to_retry() {
    let chain = FakeToken::new(44);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, false);
    let (st, _, b) = paid_once(&f, &chain, 44).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "the payer paid");
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 44))];
    let (st, hs, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("payment_pending")),
        "{b}"
    );
    assert_eq!(
        hs.get("retry-after").and_then(|v| v.to_str().ok()),
        Some(PAYMENT_PENDING_RETRY_SECS.to_string().as_str())
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never settled again");
    // Another resource with the same authorization: 409, as before.
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, ho, _) = get(&f, &other, &[]).await;
    let (st, _, b) = get(
        &f,
        &other,
        &[(x402::PAYMENT_SIGNATURE, pay(&ho, "payer a", 44))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::CONFLICT, Some("payment_in_use")),
        "{b}"
    );
    // The resolver proves the transfer; the identical retry is served.
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    assert_eq!(r.resolve_once().await.recovered, 1);
    f.state.intents.set_skew(0);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
}

/// A scripted facilitator that credits the shard in its own settle, as
/// `self_settle::SelfSettle` does (ADR-0026: `credits_on_settle` true).
struct SelfCrediting(Arc<Scripted>);

impl Facilitator for SelfCrediting {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.0.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        self.0.settle(r)
    }
    fn credits_on_settle(&self) -> bool {
        true
    }
}

/// SOAK-1, review NB1: the same with a facilitator that credits on settle
/// (self-settle). Such a server skips the authorization check even when one
/// is configured (`checks_unknown_settles`), so the identical retry of an
/// unknown settle takes the `payment_pending` path, with or without a
/// check: 503 with `Retry-After`, no second settle, then served from the
/// resolver's proof; one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn soak_1_a_self_crediting_facilitator_tells_the_identical_retry_to_wait() {
    for (nonce, check) in [(45u8, false), (46, true)] {
        let chain = FakeToken::new(nonce);
        let revenue = addr_bytes_of(PAY_TO);
        let fac = Arc::new(Scripted {
            inner: counting(),
            chain: chain.clone(),
            revenue,
            steps: std::sync::Mutex::new([Step::LandThenHang].into_iter().collect()),
            calls: AtomicU64::new(0),
        });
        let f = build(Setup {
            paid: true,
            facilitator: Some(Arc::new(SelfCrediting(fac.clone()))),
            config: ApiConfig {
                facilitator_timeout: Duration::from_millis(50),
                request_timeout: Duration::from_millis(2_000),
                ..ApiConfig::default()
            },
            settle_check: check.then(|| {
                Arc::new(crate::settle_check::ChainSettleCheck::new(
                    chain.clone(),
                    FAKE_TOKEN,
                    revenue,
                )) as Arc<dyn crate::settle_check::SettleCheck>
            }),
            ..Setup::default()
        });
        assert!(
            !f.state.checks_unknown_settles(),
            "self-settle runs no check"
        );
        let (st, _, b) = paid_once(&f, &chain, nonce).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.intents.open_count(), 1);
        assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "the payer paid");
        let uri = format!("/v1/proofs/1/{}/1", f.shard);
        let (_, h, _) = get(&f, &uri, &[]).await;
        let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))];
        let (st, hs, b) = get(&f, &uri, &sig).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::SERVICE_UNAVAILABLE, Some("payment_pending")),
            "check {check}: {b}"
        );
        assert!(hs.get("retry-after").is_some());
        assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never settled again");
        let r = crate::intents::IntentResolver::new(
            chain.clone(),
            FAKE_TOKEN,
            revenue,
            f.state.intents.clone(),
            f.state.receipts.clone(),
        )
        .with_clock(f.state.clock.clone());
        assert_eq!(r.resolve_once().await.recovered, 1);
        f.state.intents.set_skew(0);
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_eq!(st, StatusCode::OK, "check {check}: {b}");
        assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "no new settle");
        assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.receipts.pending(), 1, "one receipt");
        assert_eq!(f.state.intents.open_count(), 0);
    }
}

/// Raise the default price of the fixture's rows to `micro_usdc`.
fn set_price(f: &Fixture, micro_usdc: u64) {
    f.state.catalog.write().expect("lock").default_pricing = Some(PricingPolicy {
        base_price_micro_usdc: micro_usdc,
        fresh_window_secs: 3_600,
    });
}

/// M7L3-R3-5: the proof path compares the payment with the requirements
/// offered now, like a fresh payment: after a price change the identical
/// retry is not served from the proof at the old price (402 with the new
/// terms, the proof untouched); at the old price again it is.
#[tokio::test(start_paused = true)]
async fn m7l3_r3_5_the_proof_path_checks_the_current_price() {
    let chain = FakeToken::new(71);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 71).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    assert_eq!(r.resolve_once().await.recovered, 1);
    f.state.intents.set_skew(0);
    assert_eq!(f.state.intents.proven_count(), 1);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 71))];
    let calls = fac.calls.load(Ordering::SeqCst);
    set_price(&f, 2_000);
    let (st, h2, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{b}");
    assert_eq!(
        b["error"]["message"],
        "payment does not match the requirements"
    );
    assert!(b.get("row").is_none());
    let pr: PaymentRequired = decode_header(
        h2.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert_eq!(pr.accepts[0].amount, "2000", "the new terms");
    assert_eq!(f.state.intents.proven_count(), 1, "the proof is untouched");
    set_price(&f, 1_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R3-5: the retry path (an open intent, the authorization check)
/// also compares the payment with the requirements offered now: after a
/// price change the identical retry gets 402 and does not take the intent.
#[tokio::test(start_paused = true)]
async fn m7l3_r3_5_the_retry_path_checks_the_current_price() {
    let chain = FakeToken::new(72);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 72).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 72))];
    let calls = fac.calls.load(Ordering::SeqCst);
    set_price(&f, 2_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{b}");
    assert_eq!(
        b["error"]["message"],
        "payment does not match the requirements"
    );
    assert!(b.get("row").is_none());
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(
        f.state.receipts.pending(),
        0,
        "not served, nothing recorded"
    );
    assert_eq!(f.state.intents.due().len(), 1, "still the resolver's");
    set_price(&f, 1_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R2-2 (review drop-order PoC): when a settle task ends without a
/// success, its intent goes back to the resolver BEFORE its claim is
/// released, so a waiting identical retry woken by the claim finds the
/// intent (`awaiting`) instead of one still marked as the task's. The test
/// holds the intent log's lock while the task ends: the task must block on
/// the intent's release with its claim still held. Released first, the
/// claim was gone while the intent was still the task's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m7l3_r2_2_the_intent_is_released_before_the_claim() {
    // Real time (a std thread holds the lock), so no deadline here may race
    // the machine (E12): the request deadline, which this test does not
    // check, is far above the time the test holds the lock (a 2 s one gave
    // 504 at load 67-73), and every wait is a condition with a 60 s cap.
    const CAP: Duration = Duration::from_secs(60);
    let chain = FakeToken::new(69);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging_until(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
        CAP,
    );
    let f = Arc::new(f);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    chain.value.store(
        pr.accepts[0].amount.parse().expect("amount"),
        Ordering::SeqCst,
    );
    let sig = pay(&h, "payer a", 69);
    let (f1, u1, s1) = (f.clone(), uri.clone(), sig.clone());
    let first = tokio::spawn(async move { get(&f1, &u1, &[(x402::PAYMENT_SIGNATURE, s1)]).await });
    // Past the re-send's intent access (`take_resend`): the re-send started.
    let deadline = std::time::Instant::now() + CAP;
    while fac.calls.load(Ordering::SeqCst) < 2 {
        assert!(std::time::Instant::now() < deadline, "no re-send");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (free_tx, free_rx) = std::sync::mpsc::channel::<()>();
    let log = f.state.intents.clone();
    let holder = std::thread::spawn(move || {
        log.hold_lock_for_test(|| {
            let _ = locked_tx.send(());
            let _ = free_rx.recv();
        })
    });
    locked_rx.recv().expect("locked");
    // The task reached its end (the outcome is unknown) ... The runtime's
    // timers may stall while a worker blocks on the lock: this thread
    // sleeps without them.
    while f.state.unknown_settles.load(Ordering::SeqCst) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the settle never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(100));
    // ... and blocks on releasing its intent, its claim still held.
    let claimed = f.state.caches.claimed();
    free_tx.send(()).expect("free");
    holder.join().expect("holder");
    assert_eq!(claimed, 1, "the claim was released before the intent");
    let (st, _, b) = first.await.expect("join");
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(CAP).await);
    assert_eq!(f.state.caches.claimed(), 0);
    // The identical retry takes the intent over and is served.
    let (st, _, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R2-5: identical retries of a request whose authorization stays
/// unused take no new payer unit, so each re-sending the settle is capped:
/// at most `MAX_RESENDS` re-sends per authorization, the first request's
/// included. Past it the check still runs, nothing is sent.
#[tokio::test(start_paused = true)]
async fn m7l3_r2_5_re_sends_of_one_authorization_are_capped() {
    let chain = FakeToken::new(70);
    let (f, fac) = scripted(&[Step::Hang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 70).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(
        fac.calls.load(Ordering::SeqCst),
        2,
        "the settle and one re-send"
    );
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 70))];
    let max = u64::from(crate::intents::MAX_RESENDS);
    for n in 1..=max + 1 {
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        let sent = fac.calls.load(Ordering::SeqCst) - 1;
        assert_eq!(sent, (n + 1).min(max), "retry {n}: re-sends so far");
        if n + 1 > max {
            assert!(
                b["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("not re-sent")),
                "{b}"
            );
        }
    }
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 0);
    assert_eq!(f.state.intents.open_count(), 1, "left to the resolver");
}

/// PP6: a wrong method gets the route's own methods in `Allow` (the intake routes take POST only;
/// the answer used to say "only GET is supported" with `Allow: GET, HEAD` everywhere), and the
/// answer conforms to the OpenAPI file.
#[tokio::test]
async fn method_errors_name_the_routes_own_methods() {
    let f = fixture(false, None);
    let dir = crate::journal::test_dir("method-405");
    let store = Arc::new(
        crate::submit::SubmitStore::open(
            dir.join("submissions.journal"),
            None,
            crate::submit::SubmitLimits::default(),
        )
        .expect("intake"),
    );
    let app = router_with(f.state.clone(), crate::submit::routes(store));
    let receipt = format!("/v1/submit/sub_{}", "0".repeat(32));
    for (method, uri, allow) in [
        (Method::GET, "/v1/submit/check", "POST"),
        (Method::GET, "/v1/submit", "POST"),
        (Method::POST, "/v1/health", "GET,HEAD"),
        (Method::DELETE, receipt.as_str(), "GET,HEAD"),
    ] {
        let verb = method.to_string();
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let resp = app.clone().oneshot(req).await.expect("response");
        let status = resp.status();
        let got = resp
            .headers()
            .get(header::ALLOW)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .replace(' ', "");
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        crate::openapi_check::check(&verb, uri, status.as_u16(), &body);
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{verb} {uri}");
        assert_eq!(got, allow, "{verb} {uri}: Allow");
        assert_eq!(code(&body), Some("method_not_allowed"));
        let msg = body["error"]["message"].as_str().unwrap_or_default();
        assert!(!msg.contains("only GET"), "{verb} {uri}: {msg}");
    }
}

/// PP6: the node serves its own OpenAPI description, the same bytes as docs/api/openapi.json,
/// with an ETag that revalidates to 304.
#[tokio::test]
async fn the_node_serves_its_openapi_description() {
    let f = fixture(false, None);
    let (st, h, body) = get(&f, "/v1/openapi.json", &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    assert_eq!(h[header::CACHE_CONTROL], "public, no-cache");
    let file: Value =
        serde_json::from_str(include_str!("../../../docs/api/openapi.json")).expect("spec");
    assert_eq!(body, file);
    assert_eq!(body["openapi"], "3.1.0");
    let etag = h[header::ETAG].to_str().expect("etag").to_string();
    let (st, _, _) = get(&f, "/v1/openapi.json", &[("if-none-match", etag)]).await;
    assert_eq!(st, StatusCode::NOT_MODIFIED);
}

/// A fixed account source for the prepaid tests.
struct FixedAccount(crate::prepaid::Account);

impl crate::prepaid_gate::AccountSource for FixedAccount {
    fn account<'a>(
        &'a self,
        _: [u8; 20],
    ) -> BoxFuture<'a, Result<crate::prepaid::Account, String>> {
        let a = self.0;
        Box::pin(async move { Ok(a) })
    }
}

/// ROADMAP S7 (ADR-0046 amendment 1): a paid row paid with a prepaid voucher. The voucher must
/// add exactly the price; the answer leaves only after the journal line is on disk; the
/// identical retry is served again with no new charge; a server without prepaid says so.
#[tokio::test(start_paused = true)]
async fn a_prepaid_voucher_pays_a_row_once_and_its_retry_is_free() {
    use crate::prepaid::{domain_separator, Account, Voucher};
    use crate::prepaid_gate::{voucher_header, PrepaidState, PREPAID_RESPONSE, PREPAID_VOUCHER};
    let f = fixture(true, mock());
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let domain = domain_separator(CHAIN, &CONTRACT);
    let buyer = secret("prepaid buyer");
    let v1 = voucher_header(&Voucher::sign(&buyer, &domain, 1000).expect("sign"));
    // No prepaid on this server: the header is refused, not ignored.
    let (s, _, b) = get(&f, &uri, &[(PREPAID_VOUCHER, v1.clone())]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, Some("prepaid_not_offered"))
    );
    let dir = crate::journal::test_dir("prepaid-http");
    let journal = dir.join("prepaid.journal");
    let src = Arc::new(FixedAccount(Account {
        deposited: 2500,
        redeemed: 0,
        exiting: false,
    }));
    let pp = Arc::new(PrepaidState::open(&journal, domain, src, 60).expect("open"));
    let f = Fixture {
        state: Arc::new(
            Arc::try_unwrap(f.state)
                .ok()
                .expect("sole owner")
                .with_prepaid(pp),
        ),
        ..f
    };
    // Paid with the voucher: 200, the proof verifies, the new total is reported.
    let (s, h, b) = get(&f, &uri, &[(PREPAID_VOUCHER, v1.clone())]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(verifies(&b));
    assert_eq!(h[PREPAID_RESPONSE], "total=1000; spendable=1500");
    assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
    let lines = || {
        std::fs::read_to_string(&journal)
            .expect("journal")
            .lines()
            .count()
    };
    assert_eq!(lines(), 1);
    // The identical retry: served again, nothing new taken.
    let (s, h2, b2) = get(&f, &uri, &[(PREPAID_VOUCHER, v1.clone())]).await;
    assert_eq!((s, &b2), (StatusCode::OK, &b));
    assert_eq!(h2[PREPAID_RESPONSE], "total=1000; spendable=1500");
    assert_eq!(lines(), 1);
    // The same voucher for another row: refused, with the total to sign next.
    let (s, _, b) = get(&f, &other, &[(PREPAID_VOUCHER, v1)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert_eq!(
        (b["last"].as_str(), b["price"].as_str()),
        (Some("1000"), Some("1000"))
    );
    // The next total pays the other row; one beyond the deposit is refused.
    let v2 = voucher_header(&Voucher::sign(&buyer, &domain, 2000).expect("sign"));
    let (s, h, _) = get(&f, &other, &[(PREPAID_VOUCHER, v2)]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[PREPAID_RESPONSE], "total=2000; spendable=500");
    let v3 = voucher_header(&Voucher::sign(&buyer, &domain, 3000).expect("sign"));
    let (s, _, b) = get(&f, &uri, &[(PREPAID_VOUCHER, v3)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert_eq!(lines(), 2);
    // Malformed, or signed by someone else: refused before anything is read.
    let (s, _, b) = get(&f, &uri, &[(PREPAID_VOUCHER, "0x00:1:0x00".into())]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, Some("invalid_prepaid"))
    );
    let mut forged = Voucher::sign(&secret("someone else"), &domain, 3000).expect("sign");
    forged.buyer = Voucher::sign(&buyer, &domain, 1).expect("sign").buyer;
    let (s, _, b) = get(&f, &uri, &[(PREPAID_VOUCHER, voucher_header(&forged))]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
    );
    assert_eq!(lines(), 2);
}

// ------------------------------------------------- paid observation (NL8)

mod paid_observe {
    //! ADR-0049 (NORTH-STAR NS10, north-star loop NL8): a level-2 subject no round settled yet is
    //! observed only after a prepaid hold; the hold is charged after the round is served and
    //! released in full otherwise. A synthetic daily feed, settled by the coordinator's own round
    //! code with the shared test keys.
    use super::*;
    use crate::observations::{ObservationStore, TemplatePins, Transcript};
    use crate::observe::{
        step, ticket_hash, ObserveOffer, ObserveState, RoundRunner, TicketState, BECOMES_PUBLIC,
        REFUND_RULE,
    };
    use crate::prepaid::{domain_separator, Account, PrepaidError, Voucher};
    use crate::prepaid_gate::{voucher_header, ChargeError, Held, PrepaidState, PREPAID_VOUCHER};
    use onyx_coordinator::observe::{
        sign_commit, sign_reveal, test_keys, ObservationOutcome, ObservationResults, ObserveConfig,
        Observer,
    };
    use onyx_core::claim::MetricId;
    use onyx_core::{
        day_number, observation_commit, Answer, FeedTemplate, Observation, ObservationKind,
        Redistribution, Subject, SubjectRule,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const METRIC: &str = "test.daily";
    const PRICE: u64 = 500;
    const DEADLINE: u64 = 1_800;
    /// The day the tests run on: 2026-10-21, one hour in.
    fn today() -> u64 {
        u64::try_from(day_number("2026-10-21").expect("day")).expect("day") * 86_400 + 3_600
    }
    fn subj(date: &str) -> String {
        format!("{METRIC}:{date}")
    }
    fn uri(date: &str) -> String {
        format!("/v1/observations/{METRIC}?subject={}", subj(date))
    }

    fn template() -> FeedTemplate {
        FeedTemplate {
            feed: "test.daily.v1".into(),
            metric: MetricId(METRIC.into()),
            fetcher: keccak256(b"test daily fetcher"),
            url_template: "https://example.test/daily/{date}.json".into(),
            selector: "/value".into(),
            kind: ObservationKind::Numeric {
                scale: 2,
                tolerance: 0,
            },
            window_secs: 300,
            min_observers: 3,
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("test.page".into()),
                attribution: Some("Source: test page".into()),
                terms: String::new(),
            },
            subject: SubjectRule::Daily {
                prefix: METRIC.into(),
                min_days: 1,
                max_days: 30,
            },
        }
    }

    /// One settled round of `t` for `subject` from `start`, observers answering `answers`.
    fn settle(t: &FeedTemplate, subject: &Subject, start: u64, answers: &[i128]) -> Transcript {
        let (mut store, admin) = ObservationResults::new();
        store
            .register_feed(&admin, t.clone(), "test", 0)
            .expect("registers");
        let spec = t.instantiate(subject, start).expect("spec");
        let cfg = ObserveConfig {
            commit_close: spec.window_end + 60,
            reveal_close: spec.window_end + 180,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        };
        let observers: Vec<Observer> = (1..=answers.len() as u8)
            .map(|i| Observer {
                address: test_keys::ordered(i).1,
                bond: 1_000,
            })
            .collect();
        let snapshot = store
            .announce_draw(&spec, &cfg, &observers, 10, &SimulatedChain::at(0))
            .expect("announces");
        let mut round = store
            .open_round(
                spec.clone(),
                cfg,
                snapshot,
                &SimulatedChain::with_beacon(Hash32([3; 32])),
            )
            .expect("opens");
        let observers = round.observers().to_vec();
        let rid = round.round_id();
        let attempt = round.draw().attempt;
        let mut reveals = Vec::new();
        for (i, n) in (1..=answers.len() as u8).zip(answers) {
            let (sk, a) = test_keys::ordered(i);
            let o = Observation {
                answer: Answer::Number(*n),
                observed_at: start + 10,
                page_hash: keccak256(&n.to_be_bytes()),
            };
            let salt = keccak256(&[i]);
            let c = observation_commit(&rid, &a, &o, &salt);
            let sig = sign_commit(&sk, &rid, attempt, &c).expect("sign");
            round.commit(start + 10, a, c, sig).expect("commit");
            reveals.push((sk, a, o, salt));
        }
        for (sk, a, o, s) in &reveals {
            let sig = sign_reveal(sk, &rid, attempt, o, s).expect("sign");
            round
                .reveal(cfg.commit_close + 1, *a, o.clone(), *s, sig)
                .expect("reveal");
        }
        let commits = round.signed_commits();
        let reveals = round.signed_reveals();
        let settled = store.settle(&round, cfg.reveal_close + 1).expect("settles");
        let ObservationOutcome::Value(answer) = settled.outcome().clone() else {
            panic!("no value")
        };
        Transcript {
            subject: subject.clone(),
            window_start: start,
            cfg,
            pool: round.pool().to_vec(),
            draw: round.draw(),
            observers,
            commits,
            reveals,
            sealed: Default::default(),
            answer,
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Honest,
        Fails,
        OtherDay,
        Forged,
        /// Finishes after the hold's deadline.
        Late,
    }

    /// The operator's coordinator, faked: settles a round now, then moves the clock past it.
    struct Runner {
        calls: AtomicU64,
        mode: Mutex<Mode>,
        now: Arc<AtomicU64>,
    }

    impl RoundRunner for Runner {
        fn run<'a>(
            &'a self,
            template: &'a FeedTemplate,
            subject: &'a Subject,
            now: u64,
            deadline: u64,
        ) -> BoxFuture<'a, Result<Transcript, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mode = *self.mode.lock().expect("mode");
            Box::pin(async move {
                let start = now - now % template.window_secs;
                let t = match mode {
                    Mode::Fails => return Err("the source did not answer".into()),
                    Mode::Honest => settle(template, subject, start, &[7, 7, 7]),
                    Mode::OtherDay => settle(
                        template,
                        &Subject::Aggregate(subj("2026-10-18")),
                        start,
                        &[7, 7, 7],
                    ),
                    Mode::Forged => {
                        let mut t = settle(template, subject, start, &[7, 7, 7]);
                        t.answer = Answer::Number(8);
                        t
                    }
                    Mode::Late => {
                        let t = settle(template, subject, start, &[7, 7, 7]);
                        self.now.store(deadline + 1, Ordering::SeqCst);
                        return Ok(t);
                    }
                };
                self.now.store(t.cfg.reveal_close + 60, Ordering::SeqCst);
                Ok(t)
            })
        }
    }

    struct Setup {
        f: Fixture,
        pp: Arc<PrepaidState>,
        runner: Arc<Runner>,
        journal: std::path::PathBuf,
        store: std::path::PathBuf,
        domain: [u8; 32],
    }

    fn account(deposited: u128, exiting: bool) -> Arc<FixedAccount> {
        Arc::new(FixedAccount(Account {
            deposited,
            redeemed: 0,
            exiting,
        }))
    }

    fn setup(exiting: bool) -> Setup {
        let f = fixture(true, mock());
        f.now.store(today(), Ordering::SeqCst);
        let t = template();
        let dir = crate::journal::test_dir("observe");
        let store = dir.join("daily.json");
        let text = ObservationStore::to_json(&t, &[], None);
        std::fs::write(&store, text.to_string()).expect("store");
        let pins = TemplatePins::from([(METRIC.to_owned(), t.id())]);
        let feed = ObservationStore::from_json(&text.to_string(), today(), &pins).expect("loads");
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_observations(feed)
            .expect("adds");
        let domain = domain_separator(CHAIN, &CONTRACT);
        let journal = dir.join("prepaid.journal");
        let pp = Arc::new(
            PrepaidState::open(&journal, domain, account(10_000, exiting), 60)
                .expect("open")
                .with_hold_limit(DEADLINE),
        );
        let runner = Arc::new(Runner {
            calls: AtomicU64::new(0),
            mode: Mutex::new(Mode::Honest),
            now: f.now.clone(),
        });
        let offers = BTreeMap::from([(
            METRIC.to_owned(),
            ObserveOffer {
                price: PRICE,
                deadline_secs: DEADLINE,
                store: store.clone(),
            },
        )]);
        let obs = Arc::new(ObserveState::new(offers, runner.clone(), &pp).expect("observe"));
        let state = Arc::try_unwrap(f.state)
            .ok()
            .expect("sole owner")
            .with_prepaid(pp.clone())
            .with_observe(obs)
            .expect("observe on");
        Setup {
            f: Fixture {
                state: Arc::new(state),
                ..f
            },
            pp,
            runner,
            journal,
            store,
            domain,
        }
    }

    impl Setup {
        fn voucher(&self, who: &str, total: u128) -> String {
            voucher_header(&Voucher::sign(&secret(who), &self.domain, total).expect("sign"))
        }
        fn lines(&self) -> usize {
            std::fs::read_to_string(&self.journal)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        }
        fn mode(&self, m: Mode) {
            *self.runner.mode.lock().expect("mode") = m;
        }
        fn calls(&self) -> u64 {
            self.runner.calls.load(Ordering::SeqCst)
        }
        async fn hold(&self, date: &str, who: &str, total: u128) -> String {
            let (s, _, b) = get(
                &self.f,
                &uri(date),
                &[(PREPAID_VOUCHER, self.voucher(who, total))],
            )
            .await;
            assert_eq!(s, StatusCode::ACCEPTED, "{b}");
            b["ticket"].as_str().expect("ticket").to_owned()
        }
        async fn ticket(&self, t: &str) -> Value {
            let (s, h, b) = get(&self.f, &format!("/v1/tickets/{t}"), &[]).await;
            assert_eq!(s, StatusCode::OK, "{b}");
            assert_eq!(cc(&h), "no-store");
            b
        }
    }

    /// R-OBS-1, R-OBS-2, R-OBS-3: an unpaid request starts no round; a paid one holds the
    /// voucher (never redeemable while held), runs one round, serves the value through the
    /// ticket, and only then charges it; a second buyer of the queued fact pays nothing.
    #[tokio::test(start_paused = true)]
    async fn an_unpaid_subject_runs_no_round_and_a_paid_one_is_served_then_charged() {
        let s = setup(false);
        let (st, _, b) = get(&s.f, &uri("2026-10-20"), &[]).await;
        assert_eq!(
            (st, code(&b)),
            (
                StatusCode::PAYMENT_REQUIRED,
                Some("observe_payment_required")
            )
        );
        assert_eq!(b["observe"]["price"], "500");
        assert_eq!(b["observe"]["pays"], PREPAID_VOUCHER);
        assert_eq!(step(&s.f.state).await, Default::default());
        assert_eq!((s.calls(), s.lines()), (0, 0), "no money, no round");
        let ticket = s.hold("2026-10-20", "buyer a", 500).await;
        assert_eq!(s.lines(), 1);
        assert!(
            s.pp.to_redeem().is_empty(),
            "a held voucher is never redeemed"
        );
        // Another buyer of the same fact: told it is queued, nothing taken, no ticket.
        let (st, h, b) = get(
            &s.f,
            &uri("2026-10-20"),
            &[(PREPAID_VOUCHER, s.voucher("buyer b", 500))],
        )
        .await;
        assert_eq!(st, StatusCode::ACCEPTED);
        assert_eq!(b["state"], "queued");
        assert!(b.get("ticket").is_none());
        assert_eq!(h[header::RETRY_AFTER], "30");
        assert_eq!(s.lines(), 1);
        assert_eq!(s.ticket(&ticket).await["state"], "queued");
        let (_, _, health) = get(&s.f, "/v1/health", &[]).await;
        assert_eq!(health["observe"]["queued"], 1);
        // The worker runs the round: served, then charged.
        let done = step(&s.f.state).await;
        assert_eq!(s.calls(), 1);
        assert!(done.observed.is_some() && done.released.is_empty());
        assert_eq!(s.lines(), 2);
        assert_eq!(
            s.pp.to_redeem()
                .iter()
                .map(|v| v.cumulative)
                .collect::<Vec<_>>(),
            vec![500]
        );
        let b = s.ticket(&ticket).await;
        assert_eq!(b["state"], "observed");
        assert_eq!(b["observation"]["value"], "0.07");
        // The round is in the results store and loads with every check.
        let pins = TemplatePins::from([(METRIC.to_owned(), template().id())]);
        let stored = std::fs::read_to_string(&s.store).expect("store");
        let feed = ObservationStore::from_json(&stored, s.f.now.load(Ordering::SeqCst), &pins)
            .expect("loads");
        assert!(feed.record(&subj("2026-10-20"), None, None).is_some());
        // Later buyers of the fact pay the ordinary way; no second round.
        let (st, _, b) = get(&s.f, &uri("2026-10-20"), &[]).await;
        assert!(
            st == StatusCode::OK || code(&b) == Some("payment_required"),
            "{st} {b}"
        );
        assert_eq!(step(&s.f.state).await, Default::default());
        assert_eq!(s.calls(), 1);
    }

    /// R-OBS-2, R-OBS-6: a failed round and a passed deadline each release the hold in full; the
    /// buyer's next voucher starts from the earlier total.
    #[tokio::test(start_paused = true)]
    async fn a_failed_round_or_a_passed_deadline_releases_the_hold_in_full() {
        let s = setup(false);
        s.mode(Mode::Fails);
        let first = s.hold("2026-10-20", "buyer a", 500).await;
        let done = step(&s.f.state).await;
        assert_eq!(done.released, vec![ticket_hash(&hex32(&first))]);
        let b = s.ticket(&first).await;
        assert_eq!(
            (b["state"].as_str(), b["refund"].as_str()),
            (Some("released"), Some(REFUND_RULE))
        );
        assert!(s.pp.to_redeem().is_empty());
        // The same total again: accepted, so nothing was charged.
        let second = s.hold("2026-10-20", "buyer a", 500).await;
        s.mode(Mode::Honest);
        s.f.now.fetch_add(DEADLINE, Ordering::SeqCst);
        let done = step(&s.f.state).await;
        assert_eq!(done.released, vec![ticket_hash(&hex32(&second))]);
        assert_eq!(s.calls(), 1, "a hold past its deadline runs no round");
        assert_eq!(s.ticket(&second).await["state"], "released");
        assert!(s.pp.to_redeem().is_empty());
        let (_, _, health) = get(&s.f, "/v1/health", &[]).await;
        assert_eq!(health["observe"]["released"], 2);
    }

    /// R-OBS-4: a transcript for another day, one that does not check against the pinned
    /// template, or one that arrives after the deadline is never served and never charged.
    #[tokio::test(start_paused = true)]
    async fn a_round_for_another_day_a_forged_or_a_late_one_is_never_served() {
        for mode in [Mode::OtherDay, Mode::Forged, Mode::Late] {
            let s = setup(false);
            s.mode(mode);
            let t = s.hold("2026-10-20", "buyer a", 500).await;
            let done = step(&s.f.state).await;
            assert_eq!(done.released.len(), 1);
            assert_eq!(s.ticket(&t).await["state"], "released");
            let c = s.f.state.catalog.read().expect("lock");
            let feed = c.observations(METRIC).expect("feed");
            assert!(feed.records.is_empty(), "nothing served");
            drop(c);
            assert!(s.pp.to_redeem().is_empty());
            let stored: Value =
                serde_json::from_str(&std::fs::read_to_string(&s.store).expect("store"))
                    .expect("json");
            assert_eq!(stored["rounds"], json!([]));
        }
    }

    /// NL8-1, NL8-3 (R-OBS-7, R-OBS-9): one hold per buyer, and no other voucher while it is
    /// pending; buyers' facts run oldest first.
    #[tokio::test(start_paused = true)]
    async fn one_hold_per_buyer_and_facts_run_in_turn() {
        let s = setup(false);
        let a = s.hold("2026-10-20", "buyer a", 500).await;
        let (st, _, b) = get(
            &s.f,
            &uri("2026-10-19"),
            &[(PREPAID_VOUCHER, s.voucher("buyer a", 1000))],
        )
        .await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
        );
        assert!(b["error"]["message"].as_str().expect("m").contains("hold"));
        // Its ordinary answers wait too (the voucher API, as the paid routes call it).
        let now = today();
        let paid = |total| Voucher::sign(&secret("buyer a"), &s.domain, total).expect("sign");
        assert_eq!(
            s.pp.charge(paid(1000), 1000, (1, Hash32([1; 32])), Hash32([2; 32]), now),
            Err(ChargeError::Refused(PrepaidError::HoldPending))
        );
        assert_eq!(s.lines(), 1);
        let b2 = s.hold("2026-10-19", "buyer b", 500).await;
        assert_eq!(
            step(&s.f.state).await.observed,
            Some(ticket_hash(&hex32(&a)))
        );
        assert_eq!(
            step(&s.f.state).await.observed,
            Some(ticket_hash(&hex32(&b2)))
        );
        // Charged: buyer a pays on as usual, from the held total.
        assert!(s
            .pp
            .charge(paid(1500), 1000, (1, Hash32([1; 32])), Hash32([3; 32]), now)
            .is_ok());
    }

    /// NL8-2 (R-OBS-8): no hold during an exit, none longer than the hold limit, and an offer
    /// whose deadline outlasts it is refused at start.
    #[tokio::test(start_paused = true)]
    async fn holds_respect_the_exit_and_the_hold_limit() {
        let s = setup(true);
        let (st, _, b) = get(
            &s.f,
            &uri("2026-10-20"),
            &[(PREPAID_VOUCHER, s.voucher("buyer a", 500))],
        )
        .await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::PAYMENT_REQUIRED, Some("prepaid_refused"))
        );
        assert_eq!(s.lines(), 0);
        let dir = crate::journal::test_dir("observe-limit");
        let pp = PrepaidState::open(dir.join("j"), s.domain, account(10_000, false), 60)
            .expect("open")
            .with_hold_limit(DEADLINE);
        let long = BTreeMap::from([(
            METRIC.to_owned(),
            ObserveOffer {
                price: PRICE,
                deadline_secs: DEADLINE + 1,
                store: s.store.clone(),
            },
        )]);
        assert!(ObserveState::new(long, s.runner.clone(), &pp).is_err());
        let v = Voucher::sign(&secret("buyer c"), &s.domain, 500).expect("sign");
        pp.refresh(v.buyer, today()).await.expect("read");
        let held = |deadline| Held {
            ticket: Hash32([9; 32]),
            metric: METRIC.into(),
            subject: subj("2026-10-20"),
            deadline,
            created: today(),
        };
        assert_eq!(
            pp.hold(v.clone(), 500, held(today() + DEADLINE + 1), today()),
            Err(ChargeError::Refused(PrepaidError::HoldTooLong {
                limit: DEADLINE
            }))
        );
        assert_eq!(pp.hold(v, 500, held(today() + DEADLINE), today()), Ok(()));
    }

    /// NL8-4, NL8-5 (R-OBS-10, R-OBS-11): the offer says the value becomes public and is the same
    /// for every subject the template allows now; a day it does not allow, or a metric with no
    /// offer, is the usual 404 and nothing is held.
    #[tokio::test(start_paused = true)]
    async fn the_offer_is_the_same_for_every_allowed_day_and_says_the_value_becomes_public() {
        let s = setup(false);
        let (_, _, one) = get(&s.f, &uri("2026-10-20"), &[]).await;
        let (_, _, two) = get(&s.f, &uri("2026-09-25"), &[]).await;
        assert_eq!(one["observe"], two["observe"]);
        assert_eq!(one["observe"]["becomesPublic"], BECOMES_PUBLIC);
        assert_eq!(one["observe"]["refund"], REFUND_RULE);
        for date in ["2026-10-21", "2026-08-01", "2026-10-99"] {
            let (st, _, b) = get(
                &s.f,
                &uri(date),
                &[(PREPAID_VOUCHER, s.voucher("buyer a", 500))],
            )
            .await;
            assert_eq!(
                (st, code(&b)),
                (StatusCode::NOT_FOUND, Some("unknown_subject")),
                "{date}"
            );
        }
        // A query naming a window is not an observe request either.
        let (st, _, _) = get(&s.f, &format!("{}&window_end=1", uri("2026-10-20")), &[]).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(s.lines(), 0);
        let (st, _, b) = get(&s.f, "/v1/tickets/00", &[]).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::NOT_FOUND, Some("unknown_ticket"))
        );
    }

    /// A restart: a pending hold comes back queued and runs; an ended one still answers its
    /// ticket.
    #[tokio::test(start_paused = true)]
    async fn a_restart_resumes_pending_holds_and_keeps_ended_tickets() {
        let s = setup(false);
        let t = s.hold("2026-10-20", "buyer a", 500).await;
        let h = ticket_hash(&hex32(&t));
        let reopen = || {
            let pp = PrepaidState::open(&s.journal, s.domain, account(10_000, false), 60)
                .expect("reopen")
                .with_hold_limit(DEADLINE);
            let offers = BTreeMap::from([(
                METRIC.to_owned(),
                ObserveOffer {
                    price: PRICE,
                    deadline_secs: DEADLINE,
                    store: s.store.clone(),
                },
            )]);
            let o = ObserveState::new(offers, s.runner.clone(), &pp).expect("observe");
            (pp, o)
        };
        let (pp, o) = reopen();
        assert_eq!(pp.holds().len(), 1);
        assert_eq!(o.ticket(&h).map(|t| t.state), Some(TicketState::Queued));
        assert!(pp.to_redeem().is_empty());
        step(&s.f.state).await;
        let (pp, o) = reopen();
        assert!(pp.holds().is_empty());
        assert_eq!(o.ticket(&h).map(|t| t.state), Some(TicketState::Observed));
        assert_eq!(pp.to_redeem().len(), 1);
    }

    fn hex32(s: &str) -> [u8; 32] {
        hex::decode(s).expect("hex").try_into().expect("32 bytes")
    }
}

/// The estimate layer (ADR-0050, docs/spec/estimates.md): estimates are served
/// apart, with method and error, and never through a checked route.
mod estimates {
    use super::*;
    use onyx_core::{ErrorBand, Estimate, MetricId, Subject, Value};

    const EST: &str = "estimate.test.daily_mean";

    fn est(subject: &str, as_of: u64, error: ErrorBand) -> Estimate {
        Estimate {
            metric: MetricId(EST.into()),
            subject: Subject::Aggregate(subject.into()),
            as_of,
            value: Value::Fixed {
                mantissa: 1_234,
                scale: 2,
            },
            method: "onyx.mean-ci95.v1".into(),
            method_hash: keccak256(b"the method's text"),
            error,
            inputs: vec![keccak256(b"input")],
        }
    }

    fn band() -> ErrorBand {
        ErrorBand::Interval {
            lo: Value::Fixed {
                mantissa: 1_000,
                scale: 2,
            },
            hi: Value::Fixed {
                mantissa: 1_500,
                scale: 2,
            },
            coverage_ppm: 950_000,
        }
    }

    #[test]
    fn a_checked_catalog_refuses_an_estimate_metric() {
        let f = build(Setup::default());
        let mut c = f.state.catalog.write().expect("lock");
        // A shard spec cannot be listed under an estimate metric...
        let m = meta(9, 100, b"params est", 1);
        let mut hand = m.clone();
        hand.metric = EST.into();
        c.add_shard(hand.clone());
        assert!(c.shard(hand.round, &hand.shard).is_none(), "never added");
        assert!(c.for_metric(EST, None).is_empty());
        // ...and an estimate cannot be filed under a checked metric.
        let mut wrong = est("test.net", 1, band());
        wrong.metric = MetricId("test.flow".into());
        assert!(c.add_estimates(vec![wrong]).is_err());
        // A malformed estimate is refused whole.
        let mut bad = est("test.net", 1, band());
        bad.method_hash = Hash32::ZERO;
        assert!(c
            .add_estimates(vec![est("test.other", 1, band()), bad])
            .is_err());
        assert!(!c.has_estimates(EST), "nothing added on a refusal");
    }

    #[test]
    fn from_spec_refuses_an_estimate_metric() {
        let spec = onyx_core::ShardSpec {
            recipe: None,
            runtime: None,
            label_set: None,
            source: source_ref(100),
            schema: keccak256(b"schema"),
            rights_hash: rights().hash(),
            params: b"p".to_vec(),
            registry_root: keccak256(b"registry"),
        };
        assert!(ShardMeta::from_spec(1, &spec, "test.flow", rights()).is_some());
        assert!(ShardMeta::from_spec(1, &spec, EST, rights()).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn estimates_route_answers_method_and_error() {
        let f = fixture(false, None);
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_estimates(vec![
                est("test.net", 10, band()),
                // An older estimate of the same subject never replaces a newer one.
                est("test.net", 5, ErrorBand::Unknown),
                est("test.unknown", 10, ErrorBand::Unknown),
            ])
            .expect("estimates");
        let (s, h, b) = get(&f, &format!("/v1/estimates/{EST}?subject=test.net"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["value"], "12.34");
        assert_eq!(b["level"], "E");
        assert!(b["proofLevel"].is_null() && b["ladder"].is_null());
        assert_eq!(b["checked"], false);
        assert_eq!(b["method"], "onyx.mean-ci95.v1");
        assert_eq!(b["error"]["kind"], "interval");
        assert_eq!(b["error"]["lo"], "10.00");
        assert_eq!(b["error"]["hi"], "15.00");
        assert_eq!(b["error"]["coveragePpm"], 950_000);
        assert_eq!(b["asOf"], 10);
        assert!(h.get(header::ETAG).is_some());
        // R-EST-3: an unknown error is stated, never left out.
        let (s, _, b) = get(
            &f,
            &format!("/v1/estimates/{EST}?subject=test.unknown"),
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["error"]["kind"], "unknown");
        // A subject with no estimate.
        let (s, _, b) = get(&f, &format!("/v1/estimates/{EST}?subject=test.none"), &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(code(&b), Some("unknown_subject"));
    }

    /// The "API refuses to mix them" test (NL3 Done-when): an estimate never
    /// answers on a checked route, and a checked metric never on the
    /// estimates route.
    #[tokio::test(start_paused = true)]
    async fn a_value_route_refuses_an_estimate_metric() {
        let f = fixture(false, None);
        f.state
            .catalog
            .write()
            .expect("lock")
            .add_estimates(vec![est("test.net", 10, band())])
            .expect("estimates");
        for uri in [
            format!("/v1/metrics/{EST}?subject=test.net"),
            format!("/v1/observations/{EST}?subject=test.net"),
        ] {
            let (s, _, b) = get(&f, &uri, &[]).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{uri}: {b}");
            assert_eq!(code(&b), Some("unknown_metric"), "{uri}");
        }
        let (s, _, b) = get(&f, "/v1/estimates/test.flow?subject=entity:a", &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
        assert_eq!(code(&b), Some("unknown_metric"));
    }
}
