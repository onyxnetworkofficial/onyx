//! AUD-2R-F3 / REG-F-F3 vectors: the metric keys, their recipe ids and minimum
//! levels the registry's owner registers in one `registerMetric` call (the floor
//! is keyed by recipe, REG-F-F2). `contracts/test/OnyxRegistry.t.sol` reads the
//! same file and registers each one.
//! Regenerate with `ONYX_BLESS=1 cargo test -p onyx-source-btc --test metric_min_levels`.
use onyx_core::claim::MetricId;
use onyx_core::onchain::metric_key;
use onyx_core::ProofLevel;
use onyx_source_btc::{
    input_level, METRIC_ENTITY_RESERVE, METRIC_MIN_LEVELS, METRIC_POOL_BLOCKS, POOL_TABLE_LEVEL,
    RESERVE_BASE_LEVEL,
};

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/registry/metric-min-levels.json"
);

/// The recipe a floored metric runs: the committed build's id (keccak of the wasm).
fn metric_recipe_id(metric: &str) -> onyx_core::Hash32 {
    let name = match metric {
        METRIC_POOL_BLOCKS => "btc_pool_blocks",
        METRIC_ENTITY_RESERVE => "btc_entity_reserve",
        _ => panic!("add the recipe of {metric}"),
    };
    let wasm = std::fs::read(format!(
        "{}/../../recipes/out/{name}.wasm",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("built recipe; run scripts/build-recipes.sh");
    onyx_core::keccak256(&wasm)
}

#[test]
fn metric_min_level_vectors_match_the_file() {
    let ids: Vec<_> = METRIC_MIN_LEVELS.iter().map(|(m, _)| *m).collect();
    let keys: Vec<_> = ids
        .iter()
        .map(|m| format!("0x{}", hex::encode(metric_key(&MetricId((*m).into())).0)))
        .collect();
    let levels: Vec<_> = METRIC_MIN_LEVELS.iter().map(|(_, l)| l.number()).collect();
    let recipes: Vec<_> = ids
        .iter()
        .map(|m| format!("0x{}", hex::encode(metric_recipe_id(m).0)))
        .collect();
    let doc = serde_json::json!({
        "metricIds": ids,
        "metricKeys": keys,
        "recipeIds": recipes,
        "levels": levels,
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(PATH, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(PATH)
        .expect("fixtures/registry/metric-min-levels.json missing; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "metric minimum levels drifted");
}

/// The pool metric's floor is the pool table's level, the reserve metric's
/// its base's (ADR-0034 Amendment 1); both are outside the money policy: no
/// market settles on a pool or reserve row.
#[test]
fn floored_metrics_sit_at_their_unchecked_inputs_level() {
    assert_eq!(
        METRIC_MIN_LEVELS,
        [
            (METRIC_POOL_BLOCKS, POOL_TABLE_LEVEL),
            (METRIC_ENTITY_RESERVE, RESERVE_BASE_LEVEL)
        ]
    );
    for l in [POOL_TABLE_LEVEL, RESERVE_BASE_LEVEL] {
        assert_eq!(l, ProofLevel::BondedClaim);
        assert!(!onyx_core::MarketPolicy::default().accepts(l));
    }
}

/// Only pool-blocks and reserve specs get an input level.
#[test]
fn the_input_level_comes_from_the_specs_schema() {
    use onyx_core::{keccak256, DataRights, ShardSpec, Snapshot, SourceId, SourceRef};
    let src = SourceId("btc.mainnet".into());
    let mut spec = ShardSpec {
        recipe: None,
        runtime: None,
        label_set: None,
        source: SourceRef {
            source: src.clone(),
            snapshot: Snapshot::Block {
                chain: onyx_core::ChainId("btc.mainnet".into()),
                height: 1,
                block_hash: keccak256(b"b"),
            },
        },
        schema: keccak256(onyx_source_btc::SCHEMA_POOL_BLOCKS),
        rights_hash: DataRights::public_chain(src).hash(),
        params: vec![],
        registry_root: keccak256(b"r"),
    };
    assert_eq!(input_level(&spec), Some(POOL_TABLE_LEVEL));
    spec.schema = keccak256(onyx_source_btc::SCHEMA_ENTITY_RESERVE);
    assert_eq!(input_level(&spec), Some(RESERVE_BASE_LEVEL));
    spec.schema = keccak256(onyx_source_btc::SCHEMA_ENTITY_FLOW_SERIES);
    assert_eq!(input_level(&spec), None);
}
