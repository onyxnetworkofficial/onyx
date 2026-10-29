//! Shared fixtures for the fuzz targets (`fuzz/fuzz_targets`) and the seed
//! corpus generator (`seedgen`). Test-only keys and values: nothing here
//! holds funds or is deployed.

use onyx_api::catalog::{Catalog, Market, PricingPolicy, ShardBinding, ShardMeta};
use onyx_api::client::{Expect, MetricDef, Window};
use onyx_api::journal::Binding;
use onyx_api::roots::{FinalRoot, JsonRpc, LedgerKind, LedgerRoots, RootError, RootSource};
use onyx_api::x402::{exact_evm, BoxFuture, MockFacilitator, PaymentRequirements, PaymentTerms};
use onyx_api::{ApiConfig, ApiState, Payments};
use onyx_coordinator::cas::{CasExt, MemCas};
use onyx_core::{keccak256, ChainId, DataRights, Hash32, Snapshot, SourceId, SourceRef};
use onyx_recipe_sdk::OutputRow;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Recipient of the test payments.
pub const PAY_TO: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
/// Testnet USDC on Base Sepolia (configuration, as in the API tests).
pub const ASSET: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
/// Base Sepolia.
pub const CHAIN: u64 = 84_532;
/// The settlement contract of the fixture.
pub const CONTRACT: [u8; 20] = [0x11; 20];
/// The fixture server's clock (unix seconds).
pub const API_NOW: u64 = 1_500;
/// The fixture shard's params.
pub const PARAMS: &[u8] = b"params one";
/// The binding every journal fixture is opened with.
pub const BIND: Binding = Binding {
    chain_id: CHAIN,
    contract: [0x22; 20],
};

/// A test key (holds nothing).
pub fn secret(label: &str) -> [u8; 32] {
    keccak256(label.as_bytes()).0
}

/// `0x` hex.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Address of the test key `label`.
pub fn addr(label: &str) -> String {
    hex0x(&exact_evm::address(&secret(label)).unwrap_or([0; 20]))
}

/// A Borsh `OutputRow`.
pub fn row(subject: &str, value: &[u8]) -> Vec<u8> {
    borsh::to_vec(&OutputRow {
        key: subject.as_bytes().to_vec(),
        value: value.to_vec(),
    })
    .unwrap_or_default()
}

/// Rights of the fixture source.
pub fn rights() -> DataRights {
    DataRights::public_chain(SourceId("btc.node".into()))
}

/// The fixture source: a block range ending at `as_of`.
pub fn source_ref(as_of: u64) -> SourceRef {
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

/// The fixture shard's binding inputs.
pub fn binding(params: &[u8], as_of: u64) -> ShardBinding {
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

/// The fixture metric as a client knows it.
pub fn flow_def(params: &[u8]) -> MetricDef<'static> {
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

/// Ledger roots presented as an on-chain source.
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

/// The API server fixture: one three-row shard in round 1, finalized on a
/// chain with its binding, and one market on its second row.
pub struct ApiFixture {
    /// The server state.
    pub state: Arc<ApiState>,
    /// The shard id.
    pub shard: Hash32,
    /// Its root.
    pub root: Hash32,
    /// Its binding hash.
    pub binding: Hash32,
    /// Its rows.
    pub rows: Vec<Vec<u8>>,
}

/// The fixture rows.
pub fn fixture_rows() -> Vec<Vec<u8>> {
    vec![
        row("entity:a", b"value a........"),
        row("entity:b", b"value b........"),
        row("entity:c", b"value c........"),
    ]
}

/// Build the API fixture; `paid` prices rows and settles through a mock
/// facilitator that accepts payments signed by the test key `payer a`.
pub fn api_fixture(paid: bool) -> Option<ApiFixture> {
    let cas = Arc::new(MemCas::new());
    let rows = fixture_rows();
    let root = cas.put_rows(&rows).ok()?;
    let m = meta(1, 100, PARAMS, rows.len() as u64);
    let binding_hash = m.binding.binding(root, rows.len() as u64);
    let shard = m.shard;
    let roots = Arc::new(LedgerRoots::new());
    roots.insert(
        1,
        shard,
        FinalRoot {
            root,
            finalized_at: 400,
            finalized_unix: 1_000,
            binding: Some(binding_hash),
            tx: Some(hex0x(&[0x77; 32])),
        },
    );
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
    let payments = if paid {
        catalog.default_pricing = Some(PricingPolicy {
            base_price_micro_usdc: 1_000,
            fresh_window_secs: 3_600,
        });
        Some(Payments {
            terms: terms(),
            facilitator: Arc::new(MockFacilitator::new([addr("payer a")])),
        })
    } else {
        None
    };
    let state = ApiState::new(
        catalog,
        Arc::new(OnChain(roots)),
        cas,
        payments,
        Arc::new(|| API_NOW),
        "https://api.onyx.test",
        ApiConfig {
            authorization_slack: Duration::from_secs(10_000_000_000),
            ..ApiConfig::default()
        },
    )
    .ok()?;
    Some(ApiFixture {
        state: Arc::new(state),
        shard,
        root,
        binding: binding_hash,
        rows,
    })
}

/// The payment terms of the paid fixture.
pub fn terms() -> PaymentTerms {
    PaymentTerms {
        network: onyx_api::x402::BASE_SEPOLIA.into(),
        asset: ASSET.into(),
        pay_to: PAY_TO.into(),
        max_timeout_seconds: 60,
        extra: Some(json!({"name": "USDC", "version": "2"})),
    }
}

/// The requirement for one 1,000-unit row.
pub fn requirements() -> PaymentRequirements {
    terms().requirements(1_000)
}

/// What a client asks of the fixture, one per response route.
pub fn expects(shard: Hash32) -> [Expect<'static>; 4] {
    [
        Expect::Proof {
            round: 1,
            shard,
            index: 1,
        },
        Expect::Export { round: 1, shard },
        Expect::Metric {
            metric: "test.flow",
            def: flow_def(PARAMS),
            subject: "entity:c",
            as_of: None,
            min_as_of: 0,
        },
        Expect::Settlement {
            market: "okx-flow",
            round: 1,
            shard,
            subject: "entity:b",
        },
    ]
}

/// The URI of each route in [`expects`] order.
pub fn expect_uris(shard: Hash32) -> [String; 4] {
    [
        format!("/v1/proofs/1/{shard}/1"),
        format!("/v1/export/1/{shard}"),
        "/v1/metrics/test.flow?subject=entity:c".to_string(),
        "/v1/settlement/okx-flow".to_string(),
    ]
}

/// The header line of a journal of `kind` bound to [`BIND`].
pub fn journal_header(kind: &str) -> Value {
    json!({
        "onyxJournal": 2,
        "kind": kind,
        "chainId": BIND.chain_id,
        "contract": hex0x(&BIND.contract),
    })
}

/// `<sum> <body>\n`, the journal's line framing (its checksum is the first
/// 16 bytes of keccak-256 of the body, in hex).
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = hex::encode(&keccak256(body).0[..16]).into_bytes();
    out.push(b' ');
    out.extend_from_slice(body);
    out.push(b'\n');
    out
}

/// A JSON-RPC node that answers from a script, then errors.
#[derive(Default)]
pub struct ScriptedRpc {
    answers: Mutex<VecDeque<Result<Value, String>>>,
}

impl ScriptedRpc {
    /// A node answering `answers` in order.
    pub fn new(answers: impl IntoIterator<Item = Result<Value, String>>) -> Self {
        ScriptedRpc {
            answers: Mutex::new(answers.into_iter().collect()),
        }
    }
}

impl JsonRpc for ScriptedRpc {
    fn call<'a>(
        &'a self,
        _method: &'a str,
        _params: Value,
    ) -> BoxFuture<'a, Result<Value, String>> {
        let next = self
            .answers
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
            .unwrap_or_else(|| Err("script ended".into()));
        Box::pin(async move { next })
    }
}

thread_local! {
    static RT: Option<tokio::runtime::Runtime> = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .ok();
}

/// Run `f` to completion on this thread's runtime (paused clock, so timers
/// fire at once); `None` only if no runtime could be built.
pub fn block_on<F: std::future::Future>(f: F) -> Option<F::Output> {
    RT.with(|rt| rt.as_ref().map(|rt| rt.block_on(f)))
}

/// The level-2 fixture: the recorded Debt to the Penny round (fiscaldata).
pub mod level2 {
    use onyx_core::{FeedTemplate, Subject};

    /// A checker's clock one day after the recording.
    pub const NOW: u64 = onyx_source_fiscaldata::RECORDED_WINDOW_START + 86_400;

    /// The client's template.
    pub fn template() -> FeedTemplate {
        onyx_source_fiscaldata::template()
    }

    /// The recording's record date.
    pub fn record_date() -> String {
        onyx_source_fiscaldata::recorded().record_date
    }

    /// The subject asked for.
    pub fn subject() -> Subject {
        Subject::Aggregate(onyx_source_fiscaldata::subject(&record_date()))
    }

    /// The operator's pin of the template.
    pub fn pins() -> onyx_api::observations::TemplatePins {
        let t = template();
        onyx_api::observations::TemplatePins::from([(t.metric.0.clone(), t.id())])
    }
}

/// A JSON-RPC node scripted per method: `{"eth_chainId": ["0x14a34"],
/// "eth_getLogs": [[...], ...]}`; each call takes the method's next answer,
/// the string `"__err"` is a node error, and a method with no answer left
/// errs. At most 256 calls, so no caller loops forever.
pub struct MethodRpc {
    answers: Mutex<std::collections::BTreeMap<String, VecDeque<Value>>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl MethodRpc {
    /// The node scripted by `v` (an object of method to answers).
    pub fn new(v: &Value) -> Self {
        let mut map = std::collections::BTreeMap::new();
        if let Some(o) = v.as_object() {
            for (k, a) in o {
                let q: VecDeque<Value> = match a {
                    Value::Array(items) => items.iter().cloned().collect(),
                    other => std::iter::once(other.clone()).collect(),
                };
                map.insert(k.clone(), q);
            }
        }
        MethodRpc {
            answers: Mutex::new(map),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl JsonRpc for MethodRpc {
    fn call<'a>(&'a self, method: &'a str, _params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let n = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let next = if n >= 256 {
            Err("call budget spent".to_string())
        } else {
            self.answers
                .lock()
                .ok()
                .and_then(|mut m| m.get_mut(method).and_then(VecDeque::pop_front))
                .map_or_else(
                    || Err(format!("{method}: no answer")),
                    |v| {
                        if v == json!("__err") {
                            Err(format!("{method}: node error"))
                        } else {
                            Ok(v)
                        }
                    },
                )
        };
        Box::pin(async move { next })
    }
}

/// An HTTP transport answering every POST with one fixed status and body.
pub struct FixedPost(pub u16, pub Vec<u8>);

impl onyx_api::x402::HttpPost for FixedPost {
    fn post<'a>(
        &'a self,
        _url: &'a str,
        _body: Vec<u8>,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        let r = Ok((self.0, self.1.clone()));
        Box::pin(async move { r })
    }
}

/// A small registry: a few metrics, an entity, an aggregate and sources.
pub fn registry() -> onyx_core::StaticRegistry {
    use onyx_core::{EntityKind, MetricInfo, StaticRegistry, ValueKind};
    let mut r = StaticRegistry::new();
    let m = |value_kind, needs_label_set| MetricInfo {
        value_kind,
        needs_label_set,
    };
    let _ = r
        .add_metric("test.metric", m(ValueKind::UInt, false))
        .and_then(|r| r.add_metric("test.flag", m(ValueKind::Bool, false)))
        .and_then(|r| r.add_metric("test.labelled", m(ValueKind::UInt, true)))
        .and_then(|r| r.add_metric("test.flow", m(ValueKind::Bytes, true)));
    let _ = r
        .add_entity("entity:example-exchange", EntityKind::Exchange)
        .and_then(|r| r.add_aggregate("btc.mainnet"))
        .and_then(|r| r.add_instrument_namespace("ticker"));
    for id in ["chain.test", "btc.node"] {
        let _ = r.add_source(DataRights::public_chain(SourceId(id.into())));
    }
    r
}

/// A shard spec over the fixture rows' source (no recipe), with its rights.
pub fn held_spec() -> (onyx_core::ShardSpec, DataRights) {
    let rights = rights();
    let spec = onyx_core::ShardSpec {
        recipe: None,
        runtime: None,
        label_set: None,
        source: source_ref(100),
        schema: keccak256(b"schema"),
        rights_hash: rights.hash(),
        params: PARAMS.to_vec(),
        registry_root: keccak256(b"registry"),
    };
    (spec, rights)
}

/// The fixture shard's (id, root, binding) without building a server.
pub fn fixture_ids() -> (Hash32, Hash32, Hash32) {
    let rows = fixture_rows();
    let root = onyx_merkle::MerkleTree::from_rows(&rows).root();
    let b = binding(PARAMS, 100);
    (b.shard_id(), root, b.binding(root, rows.len() as u64))
}

/// The client's own-node answer for the fixture shard.
pub fn chain_answer() -> onyx_api::client::ChainAnswer {
    let (shard, root, binding) = fixture_ids();
    onyx_api::client::ChainAnswer {
        round: 1,
        shard,
        chain_id: CHAIN,
        contract: CONTRACT,
        root,
        binding,
        finalized_at: 400,
    }
}
