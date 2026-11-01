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

mod money_loop;

// ------------------------------------------------------------ level 2

mod level2;

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

mod serving_gates;

// ------------------------------------------------------------ after an unknown settle

mod settle_retry;

// ------------------------------------------------------------ routes and prepaid vouchers
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

mod paid_observe;

/// The estimate layer (ADR-0050, docs/spec/estimates.md): estimates are served
/// apart, with method and error, and never through a checked route.
mod estimates;
