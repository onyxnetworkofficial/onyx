//! Golden test of `btc-entity-flow-series` (per-entity flows for every block):
//! byte-identical output across runs, engines and a native build; the checked-in
//! golden file; an independent Python recount of every row; and R-SERIES-2:
//! each entity's rows add up to its `btc-entity-balance` row for the same range
//! and label set. With the gitignored live blocks (`ONYX_LIVE_DIR`, default
//! `fixtures-live/node`) the same checks run on the 24 live blocks the website
//! shows; without them that test says so and returns.
//! Regenerate the golden file with
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden_series` (only with a recipe change).

use borsh::BorshDeserialize;
use onyx_btc_schema::{EntityBlockFlowRow, EntityFlowRow};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, Snapshot, SourceAdapter,
    SourceId, SourceRef, StaticRegistry,
};
use onyx_labels::LabelSet;
use onyx_recipe_host::{assemble_input, Host, Outcome, RunResult, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::{
    fixture_registry_series, fixture_registry_v2, load_label_set, node, BtcFixtureAdapter,
    SCHEMA_ENTITY_FLOW_SERIES, SOURCE_ID,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

const BLOCKS: &str = "btc/blocks-921080-921082.json";
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const GOLDEN: &str = "golden/btc-entity-flow-series-921080-921082.labels-v2.json";
/// Root of `btc-entity-balance` on the live range with label set v2, as the
/// website verifier pins it (`apps/web/verify`, `pinned::LIVE_ROOT`).
const LIVE_RANGE_ROOT: &str = "0x6bdf585d596df0dcd51d366da14da181a79c572c072901dd74a57a33b2ce09fb";
/// Root of this recipe on the live range with label set v2, as the website
/// verifier pins it (`apps/web/verify`, `pinned::LIVE_SERIES_ROOT`).
const LIVE_SERIES_ROOT: &str = "0xa4a0c7fb568541fe816d1f79b003eb9ecd53f87d575bd9238c088dc03064db25";
/// A quiet live block (no labelled company moved coins) and the roots the
/// website verifier pins for it (`apps/web/verify` `pinned::QUIET_*`, SER-R2-F4).
const QUIET_HEIGHT: u64 = 970_551;
const QUIET_RANGE_ROOT: &str = "0x90a128ecb5a556c963ac8a9cc027a821c7d40ce065a4ec34a1ca0a40317b9d3b";
const QUIET_SERIES_ROOT: &str =
    "0xc2ec55cf2363afeb83b050b364f8a02604d80bc9649aa87adcc173455f1555ec";
const LIVE_FROM: u64 = 970_543;
const LIVE_TO: u64 = 970_566;
/// Last height of the 12-block round shard the preflight must pass.
const LIVE_HALF_TO: u64 = 970_554;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixtures() -> PathBuf {
    root().join("fixtures")
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

fn recipe(name: &str) -> (Vec<u8>, Hash32) {
    let wasm = std::fs::read(root().join("recipes/out").join(format!("{name}.wasm")))
        .expect("built recipe; run scripts/build-recipes.sh");
    let id = keccak256(&wasm);
    let recorded = std::fs::read_to_string(
        root()
            .join("recipes/out")
            .join(format!("{name}.wasm.keccak")),
    )
    .expect("keccak file");
    assert_eq!(
        recorded.trim(),
        id.to_string(),
        "{name}: id file out of date"
    );
    (wasm, id)
}

struct Shard {
    wasm: Vec<u8>,
    id: Hash32,
    input: Vec<u8>,
    spec: ShardSpec,
}

fn shard(
    name: &str,
    schema: &[u8],
    reg: &StaticRegistry,
    source: Vec<u8>,
    snapshot: Snapshot,
    labels: &LabelSet,
) -> Shard {
    let (wasm, id) = recipe(name);
    let spec = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(schema),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("registered source")
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    assert_eq!(spec.validate(reg), Ok(()));
    let input = assemble_input(&spec, source, Some(labels), &[], reg)
        .expect("assemble")
        .bytes;
    Shard {
        wasm,
        id,
        input,
        spec,
    }
}

fn run(host: &Host, s: &Shard) -> RunResult {
    let r = host.run(&s.id, &s.wasm, &s.input, &profile()).expect("run");
    assert_eq!(r.outcome, Outcome::Ok);
    r
}

fn series_rows(r: &RunResult) -> Vec<(Vec<u8>, EntityBlockFlowRow)> {
    r.leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l).expect("row");
            let v = EntityBlockFlowRow::try_from_slice(&row.value).expect("series row");
            (row.key, v)
        })
        .collect()
}

fn range_rows(r: &RunResult) -> Vec<EntityFlowRow> {
    r.leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l).expect("row");
            EntityFlowRow::try_from_slice(&row.value).expect("flow row")
        })
        .collect()
}

/// The series as the Python recount prints it.
fn series_json(rows: &[(Vec<u8>, EntityBlockFlowRow)]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|(_, r)| {
            serde_json::json!({
                "entity": r.entity,
                "height": r.height,
                "received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(),
                "net_sats": r.net_sats.to_string(),
                "tx_count": r.tx_count,
            })
        })
        .collect()
}

/// R-SERIES-1 and R-SERIES-2: every row's key is (entity, height) in order,
/// heights stay in the range, net = received - sent, no all-zero row, and the
/// rows of each entity add up to its range row (an entity the range row has
/// but the series lacks must have zero flow and zero transactions).
fn check_sums(rows: &[(Vec<u8>, EntityBlockFlowRow)], range: &[EntityFlowRow], from: u64, to: u64) {
    assert!(rows.windows(2).all(|w| w[0].0 < w[1].0), "keys ascend");
    let mut sums: BTreeMap<&str, (u128, u128, i128, u64)> = BTreeMap::new();
    for (key, r) in rows {
        assert_eq!(*key, EntityBlockFlowRow::key(&r.entity, r.height));
        assert!(
            (from..=to).contains(&r.height),
            "height {} in range",
            r.height
        );
        assert_eq!(r.net_sats, r.received_sats as i128 - r.sent_sats as i128);
        assert!(r.tx_count > 0, "only touched blocks get a row");
        let e = sums.entry(r.entity.as_str()).or_default();
        e.0 += r.received_sats;
        e.1 += r.sent_sats;
        e.2 += r.net_sats;
        e.3 += r.tx_count;
    }
    for r in range {
        let s = sums.remove(r.entity.as_str()).unwrap_or_default();
        assert_eq!(
            (r.received_sats, r.sent_sats, r.net_sats, r.tx_count),
            s,
            "{}: per-block rows add up to the range row",
            r.entity
        );
    }
    assert!(
        sums.is_empty(),
        "series names an entity the range lacks: {sums:?}"
    );
}

fn python_recount(args: &[String]) -> serde_json::Value {
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/recount-entity-flow-series.py"))
        .args(args)
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "recount failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("recount json")
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn series_golden_v2_labels() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let adapter = BtcFixtureAdapter::new(fixtures(), BLOCKS);
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let series = shard(
        "btc_entity_flow_series",
        SCHEMA_ENTITY_FLOW_SERIES,
        &fixture_registry_series(),
        source.clone(),
        snapshot.clone(),
        &labels,
    );
    let host = Host::new().expect("host");
    let first = run(&host, &series);

    // Repeated runs, the advisory fast path, and a native build agree.
    assert_eq!(run(&host, &series), first);
    for parallel in [false, true] {
        let fast = onyx_recipe_host::fast::FastHost::with_parallel_compilation(parallel)
            .expect("fast host")
            .allowing(series.id)
            .run(&series.id, &series.wasm, &series.input, &profile())
            .expect("fast run");
        assert_eq!((fast.outcome, &fast.leaves), (first.outcome, &first.leaves));
    }
    let native =
        btc_entity_flow_series::run(RecipeInput::try_from_slice(&series.input).expect("input"))
            .expect("native run");
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

    // R-SERIES-2 against btc-entity-balance on the same blocks and labels.
    let balance = shard(
        "btc_entity_balance",
        b"onyx.btc.entity-flow-row.v1",
        &fixture_registry_v2(),
        source,
        snapshot,
        &labels,
    );
    let range_run = run(&host, &balance);
    eprintln!(
        "fixture: series fuel {} rows {}, range recipe fuel {}",
        first.fuel_used,
        first.leaves.len(),
        range_run.fuel_used
    );
    let range = range_rows(&range_run);
    let rows = series_rows(&first);
    assert!(
        !rows.is_empty(),
        "labelled companies are active in these blocks"
    );
    check_sums(&rows, &range, 921_080, 921_082);

    // Independent Python recount, row for row.
    let recount = python_recount(&[
        fixtures().join(BLOCKS).display().to_string(),
        fixtures().join(LABELS_V2).display().to_string(),
    ]);
    assert_eq!(recount, serde_json::Value::Array(series_json(&rows)));

    // Checked-in golden file.
    let doc = serde_json::json!({
        "recipe_id": series.id.to_string(),
        "input_keccak": keccak256(&series.input).to_string(),
        "shard_id": series.spec.shard_id().to_string(),
        "root": first.root().to_string(),
        "leaf_count": first.leaves.len(),
        "fuel": first.fuel_used,
        "rows": series_json(&rows),
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
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

/// The series and the range recipe on the 24 live blocks the website shows.
#[test]
fn series_on_the_live_blocks() {
    let dir = std::env::var_os("ONYX_LIVE_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("fixtures-live/node"));
    if !dir.join(format!("{LIVE_FROM}.raw.hex")).exists() {
        eprintln!(
            "skipped: no live blocks at {} (set ONYX_LIVE_DIR to the fetched node files)",
            dir.display()
        );
        return;
    }
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    let (source, snapshot) = node::load_node_range(&dir, LIVE_FROM, LIVE_TO, &pin).expect("live");
    let host = Host::new().expect("host");
    let balance = shard(
        "btc_entity_balance",
        b"onyx.btc.entity-flow-row.v1",
        &fixture_registry_v2(),
        source.clone(),
        snapshot.clone(),
        &labels,
    );
    let range_run = run(&host, &balance);
    assert_eq!(
        range_run.root().to_string(),
        LIVE_RANGE_ROOT,
        "website live root"
    );
    let series = shard(
        "btc_entity_flow_series",
        SCHEMA_ENTITY_FLOW_SERIES,
        &fixture_registry_series(),
        source,
        snapshot,
        &labels,
    );
    let t = std::time::Instant::now();
    let first = run(&host, &series);
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(
        first.root().to_string(),
        LIVE_SERIES_ROOT,
        "website live series root (SER-R1-F4)"
    );
    let rows = series_rows(&first);
    check_sums(&rows, &range_rows(&range_run), LIVE_FROM, LIVE_TO);
    let recount = python_recount(&[
        dir.display().to_string(),
        fixtures().join(LABELS_V2).display().to_string(),
        LIVE_FROM.to_string(),
        LIVE_TO.to_string(),
    ]);
    assert_eq!(recount, serde_json::Value::Array(series_json(&rows)));
    eprintln!(
        "live series: root {} rows {} fuel {} wasmi {:.3} s (range recipe fuel {})",
        first.root(),
        rows.len(),
        first.fuel_used,
        secs,
        range_run.fuel_used
    );
    // The series costs about what the range recipe costs on the same input:
    // label decoding dominates. On this 24-block display range with label set
    // v2 both sit above half the budget, so the round preflight refuses it as
    // one shard (SER-1, SER-R1-F2): a round over these blocks uses two
    // 12-block shards, and the first must pass at or under half the budget.
    assert!(
        first.fuel_used < profile().fuel_limit,
        "fuel used {}",
        first.fuel_used
    );
    let cas = onyx_coordinator::cas::MemCas::new();
    {
        use onyx_coordinator::cas::{Cas, CasExt, Kind};
        cas.put(Kind::Recipe, &series.wasm).expect("recipe");
        cas.put_label_set(&labels).expect("labels");
        cas.put(Kind::Blob, &[]).expect("params");
    }
    let preflight = |s: &Shard, source: Vec<u8>| {
        onyx_coordinator::recompute::preflight(
            &s.spec,
            source,
            &fixture_registry_series(),
            &cas,
            &host,
            &onyx_labels::attest::AttestCache::new(),
            &onyx_source_btc::valid_failure_codes().into_iter().collect(),
        )
    };
    let whole = node::load_node_range(&dir, LIVE_FROM, LIVE_TO, &pin).expect("live");
    assert!(
        matches!(
            preflight(&series, whole.0),
            Err(onyx_coordinator::recompute::PreflightError::OverBudget { .. })
        ),
        "the 24-block v2 display range is over half the budget: no round offers it as one shard"
    );
    let (half_src, half_snap) =
        node::load_node_range(&dir, LIVE_FROM, LIVE_HALF_TO, &pin).expect("live half");
    let half = shard(
        "btc_entity_flow_series",
        SCHEMA_ENTITY_FLOW_SERIES,
        &fixture_registry_series(),
        half_src.clone(),
        half_snap,
        &labels,
    );
    let half_fuel = preflight(&half, half_src).expect("a 12-block shard passes the preflight");
    assert!(
        half_fuel <= profile().fuel_limit / 2,
        "12-block shard fuel {half_fuel}"
    );
    eprintln!("live 12-block series shard (first half): fuel {half_fuel} (<= half the budget)");
    // SER-R2-F4: the second 12-block half passes too.
    let (rest_src, rest_snap) =
        node::load_node_range(&dir, LIVE_HALF_TO + 1, LIVE_TO, &pin).expect("live second half");
    let rest = shard(
        "btc_entity_flow_series",
        SCHEMA_ENTITY_FLOW_SERIES,
        &fixture_registry_series(),
        rest_src.clone(),
        rest_snap,
        &labels,
    );
    let rest_fuel =
        preflight(&rest, rest_src).expect("the second 12-block shard passes the preflight");
    assert!(
        rest_fuel <= profile().fuel_limit / 2,
        "second 12-block shard fuel {rest_fuel}"
    );
    eprintln!("live 12-block series shard (second half): fuel {rest_fuel} (<= half the budget)");
    // SER-R2-F4: the quiet block the website checks gives the pinned roots:
    // all-zero range rows, and the single NO_ACTIVITY leaf as the series.
    let (q_src, q_snap) =
        node::load_node_range(&dir, QUIET_HEIGHT, QUIET_HEIGHT, &pin).expect("quiet block");
    let q_balance = shard(
        "btc_entity_balance",
        b"onyx.btc.entity-flow-row.v1",
        &fixture_registry_v2(),
        q_src.clone(),
        q_snap.clone(),
        &labels,
    );
    assert_eq!(run(&host, &q_balance).root().to_string(), QUIET_RANGE_ROOT);
    let q_series = shard(
        "btc_entity_flow_series",
        SCHEMA_ENTITY_FLOW_SERIES,
        &fixture_registry_series(),
        q_src,
        q_snap,
        &labels,
    );
    let q_run = host
        .run(&q_series.id, &q_series.wasm, &q_series.input, &profile())
        .expect("quiet run");
    assert_eq!(
        q_run.outcome,
        Outcome::RecipeError(onyx_btc_schema::SERIES_NO_ACTIVITY)
    );
    assert_eq!(q_run.root().to_string(), QUIET_SERIES_ROOT);
    assert!(
        first.fuel_used <= range_run.fuel_used + range_run.fuel_used / 20,
        "series fuel {} vs range {}",
        first.fuel_used,
        range_run.fuel_used
    );
}
