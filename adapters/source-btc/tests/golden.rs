//! M1 golden test: the real recipe on real blocks gives byte-identical output
//! across repeated runs, compilation modes, threads, and a native build of the
//! same source; the output matches the checked-in golden file; recipes that use
//! floats or a clock are rejected. Regenerate the golden file with
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden` (only with a recipe change).
//! The label set v2 golden (the website demo's anchor) is a separate file, blessed
//! on its own with `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden golden_v2`.

use borsh::BorshDeserialize;
use onyx_btc_schema::{BalanceParams, EntityBase, EntityFlowRow};
use onyx_core::StaticRegistry;
use onyx_core::{
    keccak256, Commitment, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceAdapter,
    SourceId, SourceRef,
};
use onyx_labels::LabelSet;
use onyx_recipe_host::{assemble_input, Host, HostError, Outcome, RunResult, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::{
    fixture_registry, fixture_registry_v2, load_label_set, BtcFixtureAdapter, SOURCE_ID,
};
use std::path::PathBuf;

const BLOCKS: &str = "btc/blocks-921080-921082.json";
const LABELS: &str = "labels/test.onyx.entities.btc.v1.json";
const GOLDEN: &str = "golden/btc-entity-balance-921080-921082.json";
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const GOLDEN_V2: &str = "golden/btc-entity-balance-921080-921082.labels-v2.json";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixtures() -> PathBuf {
    root().join("fixtures")
}

fn recipe(name: &str) -> Vec<u8> {
    std::fs::read(root().join("recipes/out").join(name))
        .expect("built recipe; run scripts/build-recipes.sh")
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

struct Setup {
    wasm: Vec<u8>,
    id: Hash32,
    input: Vec<u8>,
    spec: ShardSpec,
}

fn setup(params: Vec<u8>) -> Setup {
    setup_with(params, load_label_set(&fixtures(), LABELS).expect("labels"))
}

fn setup_with(params: Vec<u8>, labels: LabelSet) -> Setup {
    setup_with_registry(params, labels, fixture_registry())
}

fn setup_with_registry(params: Vec<u8>, labels: LabelSet, reg: StaticRegistry) -> Setup {
    let wasm = recipe("btc_entity_balance.wasm");
    let id = keccak256(&wasm);
    let recorded =
        std::fs::read_to_string(root().join("recipes/out/btc_entity_balance.wasm.keccak"))
            .expect("keccak file");
    assert_eq!(
        recorded.trim(),
        id.to_string(),
        "recipe id file out of date"
    );

    let adapter = BtcFixtureAdapter::new(fixtures(), BLOCKS);
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let pin = LabelSetRef {
        name: labels.name.clone(),
        version: labels.version,
        content_hash: labels.content_hash(),
    };
    let rights = reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .expect("registered source")
        .hash();
    let spec = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: Some(pin),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: rights,
        params,
        registry_root: reg.root(),
    };
    let assembled = assemble_input(&spec, source, Some(&labels), &[], &reg).expect("assemble");
    assert!(
        assembled.dropped.is_empty(),
        "fixture labels are below full confidence"
    );
    let input = assembled.bytes;
    assert_eq!(spec.validate(&reg), Ok(()));
    Setup {
        wasm,
        id,
        input,
        spec,
    }
}

fn run(host: &Host, s: &Setup) -> RunResult {
    host.run(&s.id, &s.wasm, &s.input, &profile()).expect("run")
}

fn rows(r: &RunResult) -> Vec<EntityFlowRow> {
    r.leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l).expect("row");
            EntityFlowRow::try_from_slice(&row.value).expect("flow row")
        })
        .collect()
}

fn golden_json(s: &Setup, r: &RunResult) -> String {
    let rows: Vec<serde_json::Value> = rows(r)
        .iter()
        .map(|x| {
            serde_json::json!({
                "entity": x.entity,
                "from_height": x.from_height,
                "to_height": x.to_height,
                "received_sats": x.received_sats.to_string(),
                "sent_sats": x.sent_sats.to_string(),
                "net_sats": x.net_sats.to_string(),
                "tx_count": x.tx_count,
                "end_balance_sats": x.end_balance_sats.map(|v| v.to_string()),
            })
        })
        .collect();
    let doc = serde_json::json!({
        "recipe_id": s.id.to_string(),
        "input_keccak": keccak256(&s.input).to_string(),
        "shard_id": s.spec.shard_id().to_string(),
        "root": r.root().to_string(),
        "leaf_count": r.leaves.len(),
        "rows": rows,
    });
    serde_json::to_string_pretty(&doc).expect("json") + "\n"
}

/// M1 acceptance: same recipe + same fixture + same label set = byte-identical output.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn golden_output_is_byte_identical_everywhere() {
    let s = setup(Vec::new());
    let host = Host::new().expect("host");
    let first = run(&host, &s);
    assert_eq!(first.outcome, Outcome::Ok);

    // Repeated runs.
    for _ in 0..2 {
        assert_eq!(run(&host, &s), first);
    }
    // Advisory fast path (wasmtime, serial and parallel compilation) agrees on
    // outcome and leaves (fuel is engine-specific).
    for parallel in [false, true] {
        let fast = onyx_recipe_host::fast::FastHost::with_parallel_compilation(parallel)
            .expect("fast host")
            .allowing(s.id)
            .run(&s.id, &s.wasm, &s.input, &profile())
            .expect("fast run");
        assert_eq!((fast.outcome, &fast.leaves), (first.outcome, &first.leaves));
    }
    // Concurrent runs on several threads.
    let shared = std::sync::Arc::new(s);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let s = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || run(&Host::new().expect("host"), &s))
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("join"), first);
    }
    let s = std::sync::Arc::try_unwrap(shared).ok().expect("unshared");

    // Native build of the same recipe source gives the same leaves.
    let input = RecipeInput::try_from_slice(&s.input).expect("input");
    let native = btc_entity_balance::run(input).expect("native run");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("encode"))
        .collect();
    assert_eq!(native_leaves, first.leaves);

    // Fuel headroom: at most half the budget, so fuel accounting drift between
    // wasmtime versions cannot flip a pass into a trap (R-DET-4).
    assert!(
        first.fuel_used <= profile().fuel_limit / 2,
        "fuel used {}",
        first.fuel_used
    );

    // The commitment built from the run matches the shard it was assigned.
    let c = Commitment {
        root: first.root(),
        leaf_count: first.leaves.len() as u64,
        recipe: s.spec.recipe,
        runtime: s.spec.runtime.clone(),
        label_set: s.spec.label_set.clone(),
        source: s.spec.source.clone(),
        schema: s.spec.schema,
        rights_hash: s.spec.rights_hash,
    };
    assert_eq!(c.validate(), Ok(()));
    assert!(c.matches_shard(&s.spec));
    // The match is discriminating: the same rows committed under another label
    // set version or runtime would not match this shard (R-GOLD-2).
    let mut other_labels = c.clone();
    if let Some(l) = other_labels.label_set.as_mut() {
        l.version += 1;
    }
    assert!(!other_labels.matches_shard(&s.spec));
    let mut other_runtime = c.clone();
    if let Some(r) = other_runtime.runtime.as_mut() {
        r.max_call_depth -= 1;
    }
    assert!(!other_runtime.matches_shard(&s.spec));

    // Checked-in golden file.
    let text = golden_json(&s, &first);
    let path = fixtures().join(GOLDEN);
    if std::env::var_os("ONYX_BLESS").is_some() {
        // Never re-bless in CI: a blessed run would pass against its own output (R-GOLD-2).
        assert!(
            std::env::var_os("CI").is_none(),
            "ONYX_BLESS must not be set in CI"
        );
        std::fs::write(&path, &text).expect("write golden");
    }
    let on_disk = std::fs::read_to_string(&path).expect("golden file; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "golden output changed");
}

fn check_golden(path: &str, text: &str) {
    let path = fixtures().join(path);
    if std::env::var_os("ONYX_BLESS").is_some() {
        // Never re-bless in CI: a blessed run would pass against its own output (R-GOLD-2).
        assert!(
            std::env::var_os("CI").is_none(),
            "ONYX_BLESS must not be set in CI"
        );
        std::fs::write(&path, text).expect("write golden");
    }
    let on_disk = std::fs::read_to_string(&path).expect("golden file; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "golden output changed");
}

/// Label set v2 (9,850 company labels) on the same blocks: the anchor the
/// website verifier pins. One row per labelled company, the canonical engine
/// and the native build agree, and fuel stays within half the budget.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn golden_v2_labels_output() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let companies: std::collections::BTreeSet<&str> =
        labels.labels.iter().map(|l| l.entity.as_str()).collect();
    let n = companies.len();
    let s = setup_with_registry(Vec::new(), labels.clone(), fixture_registry_v2());
    let host = Host::new().expect("host");
    let first = run(&host, &s);
    assert_eq!(first.outcome, Outcome::Ok);
    assert_eq!(first.leaves.len(), n, "one row per labelled company");
    let input = RecipeInput::try_from_slice(&s.input).expect("input");
    let native = btc_entity_balance::run(input).expect("native run");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("encode"))
        .collect();
    assert_eq!(native_leaves, first.leaves);
    assert!(
        first.fuel_used <= profile().fuel_limit / 2,
        "fuel used {}",
        first.fuel_used
    );
    check_golden(GOLDEN_V2, &golden_json(&s, &first));
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn okx_rows_are_sensible() {
    let s = setup(Vec::new());
    let r = run(&Host::new().expect("host"), &s);
    let rows = rows(&r);
    assert_eq!(rows.len(), 2, "one row per labelled entity");
    // R-KEY-ORD-1: rows are in entity-id order.
    assert_eq!(rows[0].entity, "entity:bitwise-bitb");
    assert_eq!(rows[1].entity, "entity:okx");
    let okx = rows
        .iter()
        .find(|x| x.entity == "entity:okx")
        .expect("okx row");
    assert!(
        okx.tx_count > 0,
        "43 OKX addresses are active in this range"
    );
    assert_eq!(
        okx.net_sats,
        okx.received_sats as i128 - okx.sent_sats as i128
    );
    assert_eq!(okx.from_height, 921_080);
    assert_eq!(okx.to_height, 921_082);
    assert_eq!(okx.end_balance_sats, None);
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn base_balance_is_applied_and_inconsistent_base_fails_canonically() {
    let probe = rows(&run(&Host::new().expect("host"), &setup(Vec::new())));
    let okx = probe
        .iter()
        .find(|x| x.entity == "entity:okx")
        .expect("okx");
    // Base must cover exactly the labelled entities, sorted by entity id.
    let base = |okx: u64| {
        borsh::to_vec(&BalanceParams {
            base: vec![
                EntityBase {
                    entity: "entity:bitwise-bitb".into(),
                    balance_sats: 0,
                },
                EntityBase {
                    entity: "entity:okx".into(),
                    balance_sats: okx,
                },
            ],
        })
        .expect("params")
    };
    // R-RCP-BASE-1: a partial base is rejected instead of fabricating balances.
    let partial = borsh::to_vec(&BalanceParams {
        base: vec![EntityBase {
            entity: "entity:bitwise-bitb".into(),
            balance_sats: 5,
        }],
    })
    .expect("params");
    let r = run(&Host::new().expect("host"), &setup(partial));
    assert_eq!(
        r.outcome,
        Outcome::RecipeError(btc_entity_balance::code::BAD_PARAMS)
    );
    let big = 10_000_000_000_000u64; // 100,000 BTC, more than any plausible outflow here
    let r = run(&Host::new().expect("host"), &setup(base(big)));
    let with_base = rows(&r);
    let o = with_base
        .iter()
        .find(|x| x.entity == "entity:okx")
        .expect("okx");
    assert_eq!(
        o.end_balance_sats,
        Some((big as i128 + okx.net_sats) as u128)
    );
    if okx.net_sats < 0 {
        let r = run(&Host::new().expect("host"), &setup(base(0)));
        assert_eq!(
            r.outcome,
            Outcome::RecipeError(btc_entity_balance::code::NEGATIVE_BALANCE)
        );
        assert_eq!(r.leaves, vec![onyx_recipe_host::failure_leaf(r.outcome)]);
    }
}

/// M1 acceptance: a recipe is rejected if it uses floats, a clock or randomness.
#[test]
fn rust_recipes_using_floats_clock_or_randomness_are_rejected() {
    let host = Host::new().expect("host");
    let float = recipe("test_float.wasm");
    assert!(matches!(
        host.run(&keccak256(&float), &float, b"", &profile()),
        Err(HostError::Invalid(_))
    ));
    let clock = recipe("test_clock.wasm");
    assert!(matches!(
        host.run(&keccak256(&clock), &clock, b"", &profile()),
        Err(HostError::ForbiddenImport(_))
    ));
    let rand = recipe("test_rand.wasm");
    assert!(matches!(
        host.run(&keccak256(&rand), &rand, b"", &profile()),
        Err(HostError::ForbiddenImport(_))
    ));
}

/// North star item 6 / M2 acceptance: a label-dependent metric is computed
/// against 2 label sets and published as per-set values plus a min-max band.
/// The second set keeps every other OKX address of the fixture set (test data).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn net_flow_spread_across_two_label_sets() {
    use onyx_labels::spread::{spread, SetValues};
    use onyx_labels::version::diff;

    let full = load_label_set(&fixtures(), LABELS).expect("labels");
    let mut okx_seen = 0usize;
    let half = LabelSet {
        name: "test.onyx.entities.btc.half".into(),
        version: 1,
        labels: full
            .labels
            .iter()
            .filter(|l| {
                if l.entity != "entity:okx" {
                    return true;
                }
                okx_seen += 1;
                okx_seen % 2 == 1
            })
            .cloned()
            .collect(),
    };
    assert_eq!(diff(&full, &half).expect("canonical").removed.len(), 50);

    let host = Host::new().expect("host");
    let mut inputs: Vec<SetValues> = Vec::new();
    for set in [&full, &half] {
        let s = setup_with(Vec::new(), set.clone());
        let leaves = run(&host, &s).leaves;
        let spec = &s.spec;
        let commitment = Commitment {
            root: onyx_merkle::MerkleTree::from_rows(&leaves).root(),
            leaf_count: leaves.len() as u64,
            recipe: spec.recipe,
            runtime: spec.runtime.clone(),
            label_set: spec.label_set.clone(),
            source: spec.source.clone(),
            schema: spec.schema,
            rights_hash: spec.rights_hash,
        };
        let values = SetValues::from_reveal(set, &commitment, &leaves, |l| {
            let row = OutputRow::try_from_slice(l).ok()?;
            let r = EntityFlowRow::try_from_slice(&row.value).ok()?;
            Some((r.entity.as_bytes().to_vec(), r.net_sats))
        })
        .expect("rows decode");
        inputs.push(values);
    }
    inputs.sort_by_key(SetValues::labels_hash);
    let spread_out = spread(&inputs).expect("spread");
    let band = &spread_out.rows;
    let okx = band
        .iter()
        .find(|r| r.key == b"entity:okx")
        .expect("okx row");
    let full_at = spread_out
        .sets
        .iter()
        .position(|m| m.set.name == full.name)
        .expect("full set");
    assert_eq!(okx.values[full_at], Some(148_783_915), "golden value");
    assert_eq!(okx.present(), 2);
    // Both values match an independent Python recount (scripts/recount-entity-flow.py).
    assert_eq!((okx.min, okx.max), (148_783_915, 2_706_271_883));
    eprintln!(
        "OKX net sats per set {:?}, band [{}, {}]",
        okx.values, okx.min, okx.max
    );
    let bitb = band
        .iter()
        .find(|r| r.key == b"entity:bitwise-bitb")
        .expect("bitb row");
    assert_eq!((bitb.min, bitb.max), (0, 0));
}
