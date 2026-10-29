//! M7 end to end on real blocks (ADR-0023): a round of blocks 921,080-921,082
//! is settled by the M4 code, the API reads the root from that ledger, and a
//! client gets OKX's row with a proof that verifies against the golden root,
//! pays for fresh data with x402 v2, and gets a settlement payload that the
//! Solidity verifier accepts (fixtures/api/settlement-vector.json, checked by
//! contracts/test/OnyxApiVector.t.sol).
//!
//! Every response also passes the client-side check
//! (`onyx_api::client::verify_response_unchecked`: an off-chain ledger), and its binding equals the one the
//! miners committed to.
//!
//! Regenerate the vector only with a recipe or layout change:
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test api`.

mod common;

// PP6: every answer these tests see conforms to docs/api/openapi.json.
#[path = "../../../crates/onyx-api/src/openapi_check.rs"]
mod openapi_check;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use borsh::BorshDeserialize;
use http_body_util::BodyExt;
use onyx_api::catalog::{Catalog, Market, PricingPolicy, ShardBinding, ShardMeta};
use onyx_api::client::{verify_response_unchecked, Expect, MetricDef, Window};
use onyx_api::roots::LedgerRoots;
use onyx_api::x402::{
    decode_header, encode_header, exact_evm, MockFacilitator, PaymentPayload, PaymentRequired,
    PaymentTerms, SettleResponse, BASE_SEPOLIA, PAYMENT_REQUIRED, PAYMENT_RESPONSE,
    PAYMENT_SIGNATURE,
};
use onyx_api::{router, ApiConfig, ApiState, Payments};
use onyx_btc_schema::EntityFlowRow;
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::recompute::RecomputeEnv;
use onyx_coordinator::round::RoundConfig;
use onyx_coordinator::validate::{
    canary_commit, Ledger, SettlePath, Validation, ValidationConfig, ValidatorSet, Verdict,
};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, MarketPolicy, ProofLevel, Registry, RuntimeProfile, ShardSpec,
    SourceAdapter, SourceId, SourceRef, StaticRegistry,
};
use onyx_miner::Miner;
use onyx_recipe_host::{Host, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::{
    fixture_registry, fixture_registry_v2, label_set_proof_level, load_label_set,
    BtcFixtureAdapter, BtcRules, CHAIN_ID, SOURCE_ID,
};
use onyx_validator::Validator;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

const GOLDEN_ROOT: &str = "0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2";
/// OKX net flow over the three blocks with the full test label set
/// (tests/golden.rs pins the same value).
const OKX_NET_SATS: i128 = 148_783_915;
const MARKET: &str = "okx-net-flow-921080-921082";

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
    now: Arc<AtomicU64>,
    /// The binding the miners committed to (`Commitment::fields().binding_hash()`).
    binding: Hash32,
    /// The binding inputs a client knows from the metric's definition.
    inputs: ShardBinding,
}

/// Settle one round of the fixture shard with the M4 code and stand the API
/// up on the resulting ledger.
/// The demo's test label set is `test.*`, so the server runs with
/// `allow_test_labels` (`dev`); without it the shard is refused.
fn settled_api(paid: bool, dev: bool) -> Served {
    settled_api_with(paid, dev, Labels::V1)
}

/// Which label set the settled fixture shard pins.
#[derive(Clone, Copy, PartialEq)]
enum Labels {
    V1,
    /// Label set v2 (9,850 company labels).
    V2,
}

fn settled_api_with(paid: bool, dev: bool, which: Labels) -> Served {
    let root_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let reg: StaticRegistry = match which {
        Labels::V1 => fixture_registry(),
        Labels::V2 => fixture_registry_v2(),
    };
    let fixtures = root_dir.join("fixtures");
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let labels = load_label_set(
        &fixtures,
        match which {
            Labels::V1 => "labels/test.onyx.entities.btc.v1.json",
            Labels::V2 => "labels/test.onyx.entities.btc.v2.json",
        },
    )
    .expect("labels");
    let wasm = std::fs::read(root_dir.join("recipes/out/btc_entity_balance.wasm")).expect("wasm");
    let cas = Arc::new(MemCas::new());
    cas.put(Kind::Recipe, &wasm).expect("recipe");
    cas.put_label_set(&labels).expect("labels");
    cas.put(Kind::Blob, &[]).expect("params");
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
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: rights.hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    let host = Host::new().expect("host");

    // M4: two honest miners, one validator, unanimous and undisputed.
    let a = Miner::new(key("miner a")).expect("a");
    let b = Miner::new(key("miner b")).expect("b");
    let wa = a
        .work(4, &spec, source.clone(), &reg, &*cas, &host)
        .expect("A");
    let wb = b
        .work(4, &spec, source.clone(), &reg, &*cas, &host)
        .expect("B");
    let v = Validator::new(key("validator honest")).expect("v");
    let mut set = ValidatorSet::new();
    set.add(v.address, 5_000, 1_000).expect("member");
    // M5 (L29-F4): unanimity counts only when a dispute panel could still be
    // drawn, so four bonded validators stand by and give no verdict.
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
    let mut round =
        common::open_round(4, vec![spec.clone()], &reg, &*cas, cfg, 10, &mut ledger).expect("open");
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

    // M7: the API reads the root from the ledger, never from a miner.
    let roots = Arc::new(LedgerRoots::new());
    // M5: still disputable at the settle height, so the API serves nothing yet.
    assert_eq!(roots.sync_round(&ledger, &round, 400, 400, 1_000), 0);
    assert_eq!(roots.sync_round(&ledger, &round, 599, 400, 1_000), 0);
    // Final once the challenge window (200) is over. Settled at height 400,
    // unix time 1,000: the decay clock starts there.
    assert_eq!(roots.sync_round(&ledger, &round, 600, 400, 1_000), 1);
    let mut catalog = Catalog::new();
    let meta =
        ShardMeta::from_spec(4, &spec, "btc.entity-flow", rights).expect("rights match the spec");
    // L2R2-F3: the shard pins a label set, so until its level is supplied the
    // shard is the weakest level (fail closed); the reviewed v1 set is level 4
    // (OKX-signed addresses at 3, Bitwise's published list at 4).
    assert_eq!(meta.proof_level, ProofLevel::TrackRecord);
    let label_level = label_set_proof_level(&fixtures, spec.label_set.as_ref().expect("label set"));
    assert_eq!(label_level, ProofLevel::BondedClaim);
    let mut meta = meta.with_label_set_level(label_level);
    assert_eq!(meta.proof_level, ProofLevel::BondedClaim);
    assert!(meta.test_labels);
    meta.leaf_count = Some(wa.commitment.leaf_count);
    catalog.add_shard(meta);
    catalog.add_market(
        MARKET,
        Market {
            round: 4,
            shard: spec.shard_id(),
            subject: "entity:okx".into(),
        },
    );
    if paid {
        catalog.set_pricing(
            "btc.entity-flow",
            PricingPolicy {
                base_price_micro_usdc: 2_500,
                fresh_window_secs: 86_400,
            },
        );
    }
    let now = Arc::new(AtomicU64::new(2_000));
    let clock = now.clone();
    let state = Arc::new(
        ApiState::new(
            catalog,
            roots,
            cas as Arc<dyn Cas + Send + Sync>,
            Some(Payments {
                terms: PaymentTerms {
                    network: BASE_SEPOLIA.into(),
                    asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
                    pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
                    max_timeout_seconds: 60,
                    extra: Some(json!({"name": "USDC", "version": "2"})),
                },
                facilitator: Arc::new(MockFacilitator::new([payer()])),
            }),
            Arc::new(move || clock.load(Ordering::SeqCst)),
            "http://127.0.0.1:8402",
            ApiConfig {
                allow_test_labels: dev,
                // The mock facilitator judges validity on the wall clock
                // while this server runs a test clock at unix 2,000, so the
                // payment signs a far `validBefore`; the cap on it
                // (OPS-L3-F4) is tested in onyx-api.
                authorization_slack: std::time::Duration::from_secs(10_000_000_000),
                ..ApiConfig::default()
            },
        )
        .expect("config"),
    );
    Served {
        state,
        shard: spec.shard_id(),
        now,
        binding: wa.commitment.fields().binding_hash(),
        inputs: ShardBinding::from_spec(&spec),
    }
}

async fn get(s: &Served, uri: &str, headers: &[(&str, String)]) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().uri(uri);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = router(s.state.clone())
        .oneshot(req.body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    let status = resp.status();
    let h = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    openapi_check::check("GET", uri, status.as_u16(), &body);
    (status, h, body)
}

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().expect("hex").trim_start_matches("0x")).expect("hex")
}

fn h32(v: &Value) -> Hash32 {
    let mut a = [0u8; 32];
    a.copy_from_slice(&bytes(v));
    Hash32(a)
}

fn verifies(root: &Value, row: &Value, index: &Value, n: &Value, sibs: &Value) -> bool {
    let proof = onyx_merkle::Proof {
        index: index.as_u64().expect("index"),
        leaf_count: n.as_u64().expect("n"),
        siblings: sibs.as_array().expect("siblings").iter().map(h32).collect(),
    };
    onyx_merkle::verify(&h32(root), &bytes(row), &proof)
}

fn okx_flow(row: &Value) -> EntityFlowRow {
    let out = OutputRow::try_from_slice(&bytes(row)).expect("output row");
    EntityFlowRow::try_from_slice(&out.value).expect("entity flow row")
}

#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn settled_btc_rows_are_served_with_proofs_against_the_golden_root() {
    let s = settled_api(false, true);
    let (st, _, b) = get(&s, "/v1/metrics/btc.entity-flow?subject=entity:okx", &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["root"], GOLDEN_ROOT);
    // The bundle recomputes the miners' binding from its inputs.
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: "btc.entity-flow",
            def: MetricDef {
                source: SOURCE_ID,
                chain: Some(CHAIN_ID),
                window: Window::Blocks(3),
                recipe_id: s.inputs.recipe_id,
                runtime_hash: s.inputs.runtime_hash,
                schema_id: s.inputs.schema_id,
                params_hash: s.inputs.params_hash,
                label_set: s.inputs.label_set_hash,
                registry_root: s.inputs.registry_root,
            },
            subject: "entity:okx",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    assert_eq!(v.binding, s.binding);
    assert_eq!(v.shard, s.shard);
    assert_eq!(b["verify"]["ledger"], "off-chain");
    assert_eq!(b["asOf"], 921_082);
    assert!(verifies(
        &b["root"],
        &b["row"],
        &b["proof"]["index"],
        &b["proof"]["leafCount"],
        &b["proof"]["siblings"]
    ));
    let okx = okx_flow(&b["row"]);
    assert_eq!(okx.entity, "entity:okx");
    assert_eq!(okx.net_sats, OKX_NET_SATS);

    // The full export hashes to the same root.
    let (st, _, e) = get(&s, &format!("/v1/export/4/{}", s.shard), &[]).await;
    assert_eq!(st, StatusCode::OK);
    verify_response_unchecked(
        &e,
        &Expect::Export {
            round: 4,
            shard: s.shard,
        },
    )
    .expect("export check");
    let rows: Vec<Vec<u8>> = e["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(bytes)
        .collect();
    assert_eq!(
        onyx_merkle::MerkleTree::from_rows(&rows).root().to_string(),
        GOLDEN_ROOT
    );

    // Every row by index verifies too.
    for i in 0..rows.len() {
        let (st, _, r) = get(&s, &format!("/v1/proofs/4/{}/{i}", s.shard), &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert!(verifies(
            &r["root"],
            &r["row"],
            &r["proof"]["index"],
            &r["proof"]["leafCount"],
            &r["proof"]["siblings"]
        ));
    }
}

/// IDX-5: a shard over label set v2 serves every labelled company at
/// `/v1/metrics` with proofs against the settled root; its proof level is the
/// weakest input (published lists: a claim, level 4), so the money-market
/// default refuses it at /v1/settlement.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn every_labelled_company_is_queryable_with_proofs() {
    let s = settled_api_with(false, true, Labels::V2);
    let set = load_label_set(
        &PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")),
        "labels/test.onyx.entities.btc.v2.json",
    )
    .expect("v2");
    let companies: std::collections::BTreeSet<String> =
        set.labels.iter().map(|l| l.entity.clone()).collect();
    assert_eq!(companies.len(), 4);
    for entity in &companies {
        let (st, _, b) = get(
            &s,
            &format!("/v1/metrics/btc.entity-flow?subject={entity}"),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{entity}: {b}");
        let row = okx_flow(&b["row"]);
        assert_eq!(&row.entity, entity);
        assert!(verifies(
            &b["root"],
            &b["row"],
            &b["proof"]["index"],
            &b["proof"]["leafCount"],
            &b["proof"]["siblings"]
        ));
    }
    let meta = s
        .state
        .catalog
        .read()
        .expect("lock")
        .shard(4, &s.shard)
        .expect("meta")
        .clone();
    assert_eq!(meta.proof_level, ProofLevel::BondedClaim);
    let (st, _, b) = get(&s, &format!("/v1/settlement/{MARKET}"), &[]).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"]["code"], "proof_level_not_accepted");
}

#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn test_labels_are_refused_outside_dev_mode() {
    let s = settled_api(false, false);
    let (st, _, b) = get(&s, "/v1/metrics/btc.entity-flow?subject=entity:okx", &[]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(b["error"]["code"], "test_labels");
}

/// The demo payer: a test key that holds nothing.
fn payer_key() -> [u8; 32] {
    key("demo payer (test key, holds nothing)")
}

fn payer() -> String {
    format!(
        "0x{}",
        hex::encode(exact_evm::address(&payer_key()).expect("key"))
    )
}

#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn fresh_btc_data_is_paid_with_x402_and_decays_to_free() {
    let s = settled_api(true, true);
    let uri = "/v1/metrics/btc.entity-flow?subject=entity:okx";
    let (st, h, b) = get(&s, uri, &[]).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    assert!(b.get("row").is_none());
    let pr: PaymentRequired =
        decode_header(h[PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(pr.accepts[0].amount, "2500");
    let accepted = pr.accepts[0].clone();
    let mut to = [0u8; 20];
    to.copy_from_slice(&bytes(&json!(accepted.pay_to)));
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&payer_key()).expect("key"),
        to,
        value: 2_500,
        valid_after: 0,
        valid_before: 9_999_999_999,
        nonce: key("nonce 1"),
    };
    let sig = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&payer_key(), &auth, &accepted).expect("signs"),
        accepted,
    });
    let (st, h, b) = get(&s, uri, &[(PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["root"], GOLDEN_ROOT);
    let settled: SettleResponse =
        decode_header(h[PAYMENT_RESPONSE].to_str().expect("ascii")).expect("settled");
    assert!(settled.success);
    // A day after finalization the same data is free (finalized at unix
    // 1,000; window 86,400).
    s.now.store(87_400, Ordering::SeqCst);
    let (st, h, _) = get(&s, uri, &[]).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!h.contains_key(PAYMENT_RESPONSE));
}

#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn settlement_payload_matches_the_solidity_vector() {
    let s = settled_api(false, true);
    // L2R2-F3: the shard rests on the test label set (level 4), so a market
    // under the money default (levels 1 and 3) refuses to settle on it.
    let (st, _, b) = get(&s, &format!("/v1/settlement/{MARKET}"), &[]).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"]["code"], "proof_level_not_accepted");
    // A dev market that knowingly accepts level-4 label claims settles; a real
    // money market would not (the check is advisory, ADR-0027 amendment 2).
    s.state.catalog.write().expect("catalog").market_policy = MarketPolicy::of(&[
        ProofLevel::Recomputed,
        ProofLevel::SourceProven,
        ProofLevel::BondedClaim,
    ]);
    let (st, _, b) = get(&s, &format!("/v1/settlement/{MARKET}"), &[]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(verifies(
        &b["root"],
        &b["row"],
        &b["index"],
        &b["leafCount"],
        &b["siblings"]
    ));
    assert_eq!(okx_flow(&b["row"]).net_sats, OKX_NET_SATS);
    assert_eq!(
        verify_response_unchecked(
            &b,
            &Expect::Settlement {
                market: MARKET,
                round: 4,
                shard: s.shard,
                subject: "entity:okx",
            },
        )
        .expect("settlement check")
        .binding,
        s.binding
    );
    // The exact arguments OnyxMerkle.verify / OnyxSettlement.verifyRow take.
    let vector = json!({
        "market": MARKET,
        "root": b["root"],
        "row": b["row"],
        "index": b["index"],
        "leafCount": b["leafCount"],
        "siblings": b["siblings"],
        "okxNetSats": OKX_NET_SATS.to_string(),
    });
    let path = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/api/settlement-vector.json"
    ));
    let text = serde_json::to_string_pretty(&vector).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, &text).expect("write vector");
    }
    let on_disk = std::fs::read_to_string(&path)
        .expect("fixtures/api/settlement-vector.json missing; run with ONYX_BLESS=1");
    assert_eq!(
        on_disk, text,
        "settlement vector drifted; rebless only with a recipe change"
    );
}
