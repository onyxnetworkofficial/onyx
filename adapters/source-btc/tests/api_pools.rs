//! `btc.pool-blocks` at `/v1/metrics` (ADR-0032): a round of the committed
//! coinbase proofs (blocks 970,019-970,029) is settled by the M4 code, and the
//! API serves each allowlisted pool's row by its company name with a proof
//! against the golden root. The shard pins no label set, so it is level 1
//! (rerun) and the client's check accepts it with a zero label-set hash. A
//! name that is not a row (a free-text coinbase handle, or "unknown" when
//! every block matched) is a 404.

mod common;

// PP6: every answer these tests see conforms to docs/api/openapi.json.
#[path = "../../../crates/onyx-api/src/openapi_check.rs"]
mod openapi_check;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use borsh::BorshDeserialize;
use http_body_util::BodyExt;
use onyx_api::catalog::{Catalog, ShardBinding, ShardMeta};
use onyx_api::client::{verify_response_unchecked, Expect, MetricDef, Window};
use onyx_api::roots::LedgerRoots;
use onyx_api::{router, ApiConfig, ApiState};
use onyx_coordinator::cas::{Cas, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::recompute::RecomputeEnv;
use onyx_coordinator::round::RoundConfig;
use onyx_coordinator::validate::{
    canary_commit, Ledger, SettlePath, Validation, ValidationConfig, ValidatorSet, Verdict,
};
use onyx_core::{
    keccak256, Hash32, ProofLevel, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef,
};
use onyx_miner::Miner;
use onyx_pool_schema::PoolBlocksRow;
use onyx_recipe_host::{Host, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::node::{NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::pools::{load_coinbase_proofs, pool_table, CoinbaseProofs};
use onyx_source_btc::{
    fixture_registry_pools, verify_checksum, BtcRules, CHAIN_ID, SCHEMA_POOL_BLOCKS, SOURCE_ID,
};
use onyx_validator::Validator;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const METRIC: &str = "btc.pool-blocks";
const PROOFS: &str = "btc/coinbase-970019-970029.json";
const LIVE_TIP: &str = "0000000000000000000200491e37ff370d8f79bd056051ddc6e9123da05ba446";
/// `fixtures/golden/btc-pool-blocks-970019-970029.json`.
const GOLDEN_ROOT: &str = "0x7dee13a241aacf28fe2bc6f83bd26806d965a86ceeb1a6fd3fb8bb141ff6debd";

const CFG: RoundConfig = RoundConfig {
    commit_close: 100,
    reveal_close: 200,
    min_bond: 1_000,
    validator_set_root: Hash32([0; 32]),
    canary_commit: Hash32([0; 32]),
    beacon_height: 201,
    validation: ValidationConfig {
        verdict_commit_close: 300,
        verdict_reveal_close: 400,
        canary_open_close: 450,
        pending_close: 600,
        rate_ppm: 1_000_000,
        miner_reward: 500,
        validator_fee: 10,
        validator_slash: 2_000,
        missed_slash: 100,
        min_canary_ppm: 50_000,
    },
    dispute: DisputeConfig {
        challenge_window: 200,
        min_challenge_bond: 1_000,
        panel_size: 3,
        panel_commit_window: 50,
        panel_reveal_window: 50,
        appeal_window: 50,
        max_appeals: 1,
        panel_fee: 10,
        panel_slash: 500,
    },
};

fn key(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes()).0
}

struct Served {
    state: Arc<ApiState>,
    shard: Hash32,
    binding: Hash32,
    inputs: ShardBinding,
}

/// Settle one round of the pool shard and serve it (production config: no
/// test labels, nothing paid).
fn settled_pools() -> Served {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    verify_checksum(&fixtures, PROOFS).expect("checksum");
    let proofs: CoinbaseProofs =
        serde_json::from_slice(&std::fs::read(fixtures.join(PROOFS)).expect("read"))
            .expect("proofs");
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: Some((970_029, LIVE_TIP.to_string())),
    };
    let (source, snapshot) = load_coinbase_proofs(&proofs, &pin).expect("proofs load");
    let (table, _) = pool_table(&fixtures).expect("table");
    let params = borsh::to_vec(&table).expect("params");
    let wasm = std::fs::read(root.join("recipes/out/btc_pool_blocks.wasm")).expect("wasm");
    let reg = fixture_registry_pools();
    let cas = Arc::new(MemCas::new());
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put(Kind::Blob, &params).expect("params");
    let rights = reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .expect("rights")
        .clone();
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: HOST_ID.into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 256 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 512,
        }),
        label_set: None,
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(SCHEMA_POOL_BLOCKS),
        rights_hash: rights.hash(),
        params,
        registry_root: reg.root(),
    };
    let host = Host::new().expect("host");
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let wa = a
        .work(7, &spec, source.clone(), &reg, &*cas, &host)
        .expect("A");
    let wb = b
        .work(7, &spec, source.clone(), &reg, &*cas, &host)
        .expect("B");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut set = ValidatorSet::new();
    set.add(v.address, 5_000, 1_000).expect("member");
    for i in 0..4 {
        let spare = Validator::new(key(&format!("spare validator {i}"))).expect("spare");
        set.add(spare.address, 5_000, 1_000).expect("spare member");
    }
    let salt = keccak256(b"s");
    let mut ledger = Ledger::<BtcRules>::default();
    let cfg = RoundConfig {
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).expect("empty"),
        ..CFG
    };
    let mut round = common::open_round_with(
        7,
        vec![spec.clone()],
        &reg,
        &*cas,
        std::slice::from_ref(&source),
        cfg,
        10,
        &mut ledger,
    )
    .expect("open");
    let tag = round.tag();
    for (n, (m, wk)) in [(&a, &wa), (&b, &wb)].into_iter().enumerate() {
        round
            .commit(50 + n as u64, &m.sign_commit(wk, 1_000, &tag), &mut ledger)
            .expect("commit");
    }
    for (m, wk) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(wk, &*cas).expect("rows");
        round
            .reveal(
                150,
                m.address,
                spec.shard_id(),
                wk.commitment.clone(),
                wk.salt,
                &*cas,
            )
            .expect("reveal");
    }
    round.close(200).expect("close");
    let mut val = Validation::<BtcRules>::new(&round, set, keccak256(b"beacon"), 201, &mut ledger)
        .expect("validation");
    let seed = val.seed();
    let mut pending = Vec::new();
    for (s, m, _) in val.tasks_for(&v.address) {
        val.commit_verdict(250, &v.commit(&seed, &s, &m, &Verdict::Agree))
            .expect("commit verdict");
        pending.push((s, m));
    }
    for (s, m) in pending {
        val.reveal_verdict(350, v.address, s, m, Verdict::Agree, v.salt(&seed, &s, &m))
            .expect("reveal verdict");
    }
    let never = |_: &ShardSpec| -> Option<Vec<u8>> { None };
    let env = RecomputeEnv {
        reg: &reg,
        cas: &*cas,
        host: &host,
        source: &never,
    };
    let settled = val.settle(400, Some((&[], &salt)), &env).expect("settle");
    assert_eq!(
        settled.references()[&spec.shard_id()].1,
        SettlePath::Unanimous
    );
    assert!(ledger.apply(&settled));
    let roots = Arc::new(LedgerRoots::new());
    assert_eq!(roots.sync_round(&ledger, &round, 600, 400, 1_000), 1);
    let mut catalog = Catalog::new();
    let meta = ShardMeta::from_spec(7, &spec, METRIC, rights).expect("rights match");
    // No label set, but the pinned pool table is a third-party mapping the
    // rerun does not check: the shard is floored at level 4 (AUD-2-F6).
    assert_eq!(meta.proof_level, ProofLevel::Recomputed);
    // The level comes from the spec (its schema), not from config (AUD-2R-F3).
    let input = onyx_source_btc::input_level(&spec).expect("a pool shard has an input level");
    assert_eq!(input, onyx_source_btc::POOL_TABLE_LEVEL);
    let mut meta = meta.with_input_level(input);
    assert_eq!(meta.proof_level, ProofLevel::BondedClaim);
    assert!(!onyx_core::MarketPolicy::default().accepts(meta.proof_level));
    assert!(!meta.test_labels);
    meta.leaf_count = Some(wa.commitment.leaf_count);
    catalog.add_shard(meta);
    let state = Arc::new(
        ApiState::new(
            catalog,
            roots,
            cas as Arc<dyn Cas + Send + Sync>,
            None,
            Arc::new(|| 2_000),
            "http://127.0.0.1:8402",
            ApiConfig::default(),
        )
        .expect("config"),
    );
    Served {
        state,
        shard: spec.shard_id(),
        binding: wa.commitment.fields().binding_hash(),
        inputs: ShardBinding::from_spec(&spec),
    }
}

async fn get(s: &Served, uri: &str) -> (StatusCode, Value) {
    let resp = router(s.state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("resp");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    openapi_check::check("GET", uri, status.as_u16(), &body);
    (status, body)
}

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().expect("hex").trim_start_matches("0x")).expect("hex")
}

fn h32(v: &Value) -> Hash32 {
    let mut a = [0u8; 32];
    a.copy_from_slice(&bytes(v));
    Hash32(a)
}

#[tokio::test]
async fn pool_rows_are_served_by_company_name_with_proofs() {
    let s = settled_pools();
    let def = MetricDef {
        source: SOURCE_ID,
        chain: Some(CHAIN_ID),
        window: Window::Blocks(11),
        recipe_id: s.inputs.recipe_id,
        runtime_hash: s.inputs.runtime_hash,
        schema_id: s.inputs.schema_id,
        params_hash: s.inputs.params_hash,
        label_set: Hash32::ZERO,
        registry_root: s.inputs.registry_root,
    };
    for (pool, query, want) in [
        ("F2Pool", "F2Pool", 2),
        ("Foundry USA", "Foundry%20USA", 6),
        ("NiceHash", "NiceHash", 1),
    ] {
        let (st, b) = get(&s, &format!("/v1/metrics/{METRIC}?subject={query}")).await;
        assert_eq!(st, StatusCode::OK, "{pool}: {b}");
        assert_eq!(b["root"], GOLDEN_ROOT);
        assert_eq!(b["asOf"], 970_029);
        assert_eq!(b["labelSet"], Value::Null, "no label set");
        let v = verify_response_unchecked(
            &b,
            &Expect::Metric {
                metric: METRIC,
                def: def.clone(),
                subject: pool,
                as_of: None,
                min_as_of: 0,
            },
        )
        .expect("client check");
        assert_eq!(v.binding, s.binding);
        assert_eq!(v.shard, s.shard);
        let proof = onyx_merkle::Proof {
            index: b["proof"]["index"].as_u64().expect("index"),
            leaf_count: b["proof"]["leafCount"].as_u64().expect("n"),
            siblings: b["proof"]["siblings"]
                .as_array()
                .expect("siblings")
                .iter()
                .map(h32)
                .collect(),
        };
        assert!(onyx_merkle::verify(
            &h32(&b["root"]),
            &bytes(&b["row"]),
            &proof
        ));
        let out = OutputRow::try_from_slice(&bytes(&b["row"])).expect("output row");
        let row = PoolBlocksRow::try_from_slice(&out.value).expect("pool row");
        assert_eq!(row.pool, pool);
        assert_eq!(row.blocks, want);
        assert_eq!((row.from_height, row.to_height), (970_019, 970_029));
    }
    // Not rows: a free-text coinbase handle, and "unknown" (every block of
    // this range matched a listed pool).
    for subject in ["Mined%20by%20example-user", "unknown"] {
        let (st, b) = get(&s, &format!("/v1/metrics/{METRIC}?subject={subject}")).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{subject}: {b}");
        assert_eq!(b["error"]["code"], "unknown_subject");
    }
}
