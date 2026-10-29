//! Demo query server (M7, ADR-0023; docs/ops/run-api.md).
//!
//! Settles one round of the fixture shard (blocks 921,080-921,082, full test
//! label set) with the M4 code, then serves it on 127.0.0.1 with onyx-api:
//!
//! ```text
//! cargo run --release -p onyx-source-btc --example serve-api -- --dev         # everything free
//! cargo run --release -p onyx-source-btc --example serve-api -- --dev --paid  # x402 v2, mock facilitator
//! ```
//!
//! `--dev` is required: the fixture shard is pinned to the `test.*` label
//! set, which a server refuses without it. `--paid` charges 2,500 micro-USDC
//! per row on Base Sepolia terms, checked by a local mock facilitator that
//! verifies the EIP-3009 signature and accepts the demo payer only (a test
//! key derived from a public string; it holds nothing). No network, no real
//! keys, no real payments.
//!
//! `--paid --live-facilitator` sends verify and settle to a real facilitator
//! over HTTPS instead: the URL in `ONYX_X402_FACILITATOR`, else the public
//! x402.org facilitator for Base Sepolia. The demo payer holds nothing, so
//! its payments are refused there; a funded testnet payer is needed.
//! Testnet only: a network outside `x402::TESTNET_NETWORKS` is refused at
//! startup unless `--allow-mainnet` is given, and this project never gives it.

use onyx_api::catalog::{Catalog, Market, PricingPolicy, ShardMeta};
use onyx_api::roots::LedgerRoots;
use onyx_api::x402::{
    exact_evm, facilitator_url_from_env, Facilitator, HttpFacilitator, HyperPost, MockFacilitator,
    PaymentTerms, BASE_SEPOLIA,
};
use onyx_api::{router, ApiConfig, ApiState, Payments};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::recompute::{FailureAllowList, RecomputeEnv};
use onyx_coordinator::round::{Round, RoundConfig};
use onyx_coordinator::validate::{
    canary_commit, Ledger, Validation, ValidationConfig, ValidatorSet, Verdict,
};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, MarketPolicy, ProofLevel, Registry, RuntimeProfile, ShardSpec,
    SourceAdapter, SourceId, SourceRef,
};
use onyx_miner::Miner;
use onyx_recipe_host::{Host, HOST_ID};
use onyx_source_btc::{
    fixture_registry, label_set_proof_level, load_label_set, BtcFixtureAdapter, BtcRules, SOURCE_ID,
};
use onyx_validator::Validator;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seed of the demo payer's test key (public on purpose: it holds nothing).
const DEMO_PAYER_SEED: &str = "demo payer (test key, holds nothing)";
/// Demo recipient (no key held anywhere).
const DEMO_PAY_TO: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";

fn key(s: &str) -> [u8; 32] {
    keccak256(s.as_bytes()).0
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Command-line switches.
struct Opts {
    paid: bool,
    dev: bool,
    live_facilitator: bool,
    allow_mainnet: bool,
    port: u16,
    /// `--trusted-proxy <cidr>`, repeatable (M7-L48-F2).
    trusted_proxies: Vec<onyx_api::Cidr>,
}

/// Settle the fixture round and return the API state built on its ledger,
/// and where payments are checked.
fn build(o: &Opts) -> Result<(ApiState, Hash32, String, String), Box<dyn std::error::Error>> {
    let (paid, dev, port) = (o.paid, o.dev, o.port);
    let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = dir.join("fixtures");
    let reg = fixture_registry();
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot()?;
    let source = adapter.load(&snapshot)?;
    let labels = load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json")?;
    let wasm = std::fs::read(dir.join("recipes/out/btc_entity_balance.wasm"))?;
    let cas = Arc::new(MemCas::new());
    cas.put(Kind::Recipe, &wasm)?;
    cas.put_label_set(&labels)?;
    cas.put(Kind::Blob, &[])?;
    let rights = reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .ok_or("source not registered")?
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
    let host = Host::new()?;

    // One honest round: two miners, one validator, unanimous.
    let a = Miner::new(key("demo miner a"))?;
    let b = Miner::new(key("demo miner b"))?;
    let wa = a.work(1, &spec, source.clone(), &reg, &*cas, &host)?;
    let wb = b.work(1, &spec, source.clone(), &reg, &*cas, &host)?;
    let v = Validator::new(key("demo validator"))?;
    let mut set = ValidatorSet::new();
    set.add(v.address, 5_000, 1_000)?;
    // M5 (L29-F4): unanimity counts only when a dispute panel could still be
    // drawn, so four bonded validators stand by and give no verdict.
    for i in 0..4 {
        let spare = Validator::new(key(&format!("demo spare validator {i}")))?;
        set.add(spare.address, 5_000, 1_000)?;
    }
    let salt = keccak256(b"demo canary salt");
    let mut ledger = Ledger::<BtcRules>::default();
    let cfg = RoundConfig {
        commit_close: 100,
        reveal_close: 200,
        min_bond: 1_000,
        validator_set_root: set.root(),
        canary_commit: canary_commit(&[], &salt).ok_or("canary list")?,
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
    // A coordinator opens a round only after the preflight run (SER-R2-F1).
    let pinned = source.clone();
    let source_of = move |_: &ShardSpec| Some(pinned.clone());
    let open_env = RecomputeEnv {
        reg: &reg,
        cas: &*cas,
        host: &host,
        source: &source_of,
    };
    let allowed: FailureAllowList = onyx_source_btc::valid_failure_codes().into_iter().collect();
    let mut round = Round::open_preflighted(
        1,
        vec![spec.clone()],
        &open_env,
        cfg,
        10,
        &mut ledger,
        &allowed,
    )?;
    let tag = round.tag();
    round.commit(50, &a.sign_commit(&wa, 1_000, &tag), &mut ledger)?;
    round.commit(51, &b.sign_commit(&wb, 1_000, &tag), &mut ledger)?;
    for (m, w) in [(&a, &wa), (&b, &wb)] {
        m.publish_rows(w, &*cas)?;
        round.reveal(
            150,
            m.address,
            spec.shard_id(),
            w.commitment.clone(),
            w.salt,
            &*cas,
        )?;
    }
    round.close(200)?;
    let mut val =
        Validation::<BtcRules>::new(&round, set, keccak256(b"demo beacon"), 201, &mut ledger)?;
    let seed = val.seed();
    let tasks = val.tasks_for(&v.address);
    for (s, m, _) in &tasks {
        val.commit_verdict(250, &v.commit(&seed, s, m, &Verdict::Agree))?;
    }
    for (s, m, _) in &tasks {
        val.reveal_verdict(350, v.address, *s, *m, Verdict::Agree, v.salt(&seed, s, m))?;
    }
    let none = |_: &ShardSpec| -> Option<Vec<u8>> { None };
    let env = RecomputeEnv {
        reg: &reg,
        cas: &*cas,
        host: &host,
        source: &none,
    };
    let settled = val.settle(400, Some((&[], &salt)), &env)?;
    if !ledger.apply(&settled) {
        return Err("settlement did not apply".into());
    }

    let roots = Arc::new(LedgerRoots::new());
    // The demo ledger settles now: the decay clock starts at this time. The
    // API serves a root only once it is final (M5), so the demo reads it at
    // the end of the challenge window, with no dispute opened.
    if roots.sync_round(&ledger, &round, 600, 400, now_unix()) != 1 {
        return Err("shard did not finalize".into());
    }
    let mut catalog = Catalog::new();
    // L2R2-F3: the shard is as strong as its weakest input. The reviewed test
    // label set is level 4, so this demo market opts in to level-4 label claims
    // (a real money market keeps the default, levels 1 and 3; advisory until
    // the level is bound on chain, ADR-0027 amendment 2).
    let label_level = label_set_proof_level(&fixtures, spec.label_set.as_ref().ok_or("labels")?);
    let mut meta = ShardMeta::from_spec(1, &spec, "btc.entity-flow", rights)
        .ok_or("rights do not match the spec")?
        .with_label_set_level(label_level);
    catalog.market_policy = MarketPolicy::of(&[
        ProofLevel::Recomputed,
        ProofLevel::SourceProven,
        ProofLevel::BondedClaim,
    ]);
    meta.leaf_count = Some(wa.commitment.leaf_count);
    catalog.add_shard(meta);
    catalog.add_market(
        "okx-net-flow-921080-921082",
        Market {
            round: 1,
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
    let payer = format!(
        "0x{}",
        hex::encode(exact_evm::address(&key(DEMO_PAYER_SEED)).ok_or("demo key")?)
    );
    let config = ApiConfig {
        allow_test_labels: dev,
        allow_mainnet: o.allow_mainnet,
        trusted_proxies: o.trusted_proxies.clone(),
        ..ApiConfig::default()
    };
    let network = BASE_SEPOLIA;
    let (facilitator, checked_by): (Arc<dyn Facilitator>, String) = if o.live_facilitator {
        let url = facilitator_url_from_env(network, o.allow_mainnet, |k| std::env::var(k).ok())?;
        // The whole settle patience (OPS-L2-F1); verify keeps its own 4 s.
        let http = HyperPost::for_facilitator(&config);
        (Arc::new(HttpFacilitator::new(url.clone(), http)), url)
    } else {
        (
            Arc::new(MockFacilitator::new([payer.clone()])),
            "local mock facilitator".into(),
        )
    };
    let payments = paid.then(|| Payments {
        terms: PaymentTerms {
            network: network.into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            pay_to: DEMO_PAY_TO.into(),
            max_timeout_seconds: 60,
            extra: Some(serde_json::json!({"name": "USDC", "version": "2"})),
        },
        facilitator,
    });
    let state = ApiState::new(
        catalog,
        roots,
        cas as Arc<dyn Cas + Send + Sync>,
        payments,
        Arc::new(now_unix),
        format!("http://127.0.0.1:{port}"),
        config,
    )?;
    Ok((state, spec.shard_id(), payer, checked_by))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let mut o = Opts {
        paid: flag("--paid"),
        dev: flag("--dev"),
        live_facilitator: flag("--live-facilitator"),
        allow_mainnet: flag("--allow-mainnet"),
        port: args
            .iter()
            .position(|a| a == "--port")
            .and_then(|i| args.get(i + 1))
            .map(|p| p.parse())
            .transpose()?
            .unwrap_or(8402),
        trusted_proxies: args
            .windows(2)
            .filter(|w| w[0] == "--trusted-proxy")
            .map(|w| w[1].parse())
            .collect::<Result<_, _>>()?,
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", o.port)).await?;
    // The bound port: `--port 0` picks a free one (scripts/agent-smoke.sh reads it from the
    // first line), and the payment offers name it.
    o.port = listener.local_addr()?.port();
    let (paid, dev, port) = (o.paid, o.dev, o.port);
    let (state, shard, payer, checked_by) = build(&o)?;
    println!(
        "onyx-api demo on http://127.0.0.1:{port} (payments: {})",
        if paid { checked_by.as_str() } else { "off" }
    );
    if !dev {
        println!("  without --dev the test-label shard is refused (403 test_labels)");
    }
    if paid {
        println!("  demo payer {payer} (test key: keccak256(\"{DEMO_PAYER_SEED}\"))");
    }
    println!("  GET /v1/health");
    println!("  GET /v1/metrics/btc.entity-flow?subject=entity:okx");
    println!("  GET /v1/proofs/1/{shard}/0");
    println!("  GET /v1/export/1/{shard}");
    println!("  GET /v1/settlement/okx-net-flow-921080-921082");
    axum::serve(
        listener,
        router(Arc::new(state)).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
