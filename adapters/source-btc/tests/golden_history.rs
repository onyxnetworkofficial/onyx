//! The website's one-week history (`apps/web/static/history/week/`: an
//! `index.json` and one leaf file per UTC day): with the fetched blocks, rerun
//! every shard of both recipes and check that the export's leaves, its day
//! and shard boundaries and the pinned commitment all come back. The blocks
//! are gitignored and the rerun takes minutes, so this test runs only when
//! `ONYX_HISTORY_DIR` names them:
//!
//! ```text
//! scripts/fetch-btc-node.sh indexer 969839 970846 fixtures-live/week
//! ONYX_HISTORY_DIR=fixtures-live/week cargo test --release -p onyx-source-btc --test golden_history
//! ```
//!
//! Without it the test says so and returns. Always checked: the export's own
//! commitment equals the pinned one (`apps/web/verify` `pinned::HISTORY_COMMITMENT`).

use borsh::BorshDeserialize;
use onyx_btc_schema::BtcSource;
use onyx_core::{
    keccak256, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef,
    StaticRegistry,
};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, HOST_ID};
use onyx_source_btc::history::{history_commitment, plan_days, HistoryShard, PLAN_FUEL_TARGET};
use onyx_source_btc::{
    fixture_registry_series, fixture_registry_v2, load_label_set, node, SCHEMA_ENTITY_FLOW_SERIES,
    SOURCE_ID,
};
use std::path::PathBuf;

const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
/// As the website verifier pins them (`apps/web/verify` `pinned::HISTORY_*`).
const HISTORY_FROM: u64 = 969_839;
const HISTORY_TO: u64 = 970_846;
/// Display hash of block 970,846 (`pinned::HISTORY_TIP`).
const HISTORY_TIP: &str = "000000000000000000011f4861feb9ad646c8dd6d8458d33ecccf344c8d44302";
const HISTORY_COMMITMENT: &str =
    "0xbda6327d2189e24b9927e1c91a16d51822f78e2250975898e7d35c71d1d487cc";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
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

fn h32(s: &str) -> Hash32 {
    let b = hex::decode(s.trim_start_matches("0x")).expect("hex");
    Hash32(b.try_into().expect("32 bytes"))
}

fn week_dir() -> PathBuf {
    root().join("apps/web/static/history/week")
}

fn read(path: PathBuf) -> serde_json::Value {
    let text = std::fs::read_to_string(&path).expect("history export");
    serde_json::from_str(&text).expect("json")
}

fn index() -> serde_json::Value {
    read(week_dir().join("index.json"))
}

/// A shard's `(series leaves, balance leaves)`, keyed by its first height.
type ShardLeaves = std::collections::BTreeMap<u64, (Vec<Vec<u8>>, Vec<Vec<u8>>)>;

/// Every day's leaf file, keyed by shard start: `(series leaves, balance leaves)`.
fn day_leaves(index: &serde_json::Value) -> ShardLeaves {
    let mut out = std::collections::BTreeMap::new();
    for d in index["days"].as_array().expect("days") {
        let day = read(week_dir().join(d["file"].as_str().expect("file")));
        assert_eq!(day["date"], d["date"]);
        for s in day["shards"].as_array().expect("shards") {
            out.insert(
                s["from"].as_u64().expect("from"),
                (leaves(&s["series_leaves"]), leaves(&s["balance_leaves"])),
            );
        }
    }
    out
}

fn leaves(v: &serde_json::Value) -> Vec<Vec<u8>> {
    v.as_array()
        .expect("leaves")
        .iter()
        .map(|l| hex::decode(l.as_str().expect("hex")).expect("hex"))
        .collect()
}

/// The index's shards with roots computed from the day files' leaves (and
/// equal to the index's claimed roots).
fn shards(doc: &serde_json::Value) -> Vec<HistoryShard> {
    let by_from = day_leaves(doc);
    doc["shards"]
        .as_array()
        .expect("shards")
        .iter()
        .map(|s| {
            let from = s["from"].as_u64().expect("from");
            let (series, balance) = by_from.get(&from).expect("leaves for every shard");
            let shard = HistoryShard {
                from,
                to: s["to"].as_u64().expect("to"),
                series_root: MerkleTree::from_rows(series).root(),
                balance_root: MerkleTree::from_rows(balance).root(),
            };
            assert_eq!(
                s["series_root"].as_str(),
                Some(shard.series_root.to_string().as_str())
            );
            assert_eq!(
                s["balance_root"].as_str(),
                Some(shard.balance_root.to_string().as_str())
            );
            shard
        })
        .collect()
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_export_gives_the_pinned_commitment() {
    let doc = index();
    assert_eq!(doc["from"].as_u64(), Some(HISTORY_FROM));
    assert_eq!(doc["to"].as_u64(), Some(HISTORY_TO));
    let labels = load_label_set(&root().join("fixtures"), LABELS_V2).expect("labels v2");
    let series =
        std::fs::read_to_string(root().join("recipes/out/btc_entity_flow_series.wasm.keccak"))
            .expect("series id");
    let balance =
        std::fs::read_to_string(root().join("recipes/out/btc_entity_balance.wasm.keccak"))
            .expect("balance id");
    let mut tip = hex::decode(doc["tip"].as_str().expect("tip")).expect("hex");
    tip.reverse();
    let c = history_commitment(
        &h32(series.trim()),
        &h32(balance.trim()),
        &labels.content_hash(),
        &tip.try_into().expect("32 bytes"),
        &shards(&doc),
    );
    assert_eq!(c.to_string(), HISTORY_COMMITMENT);
    assert_eq!(doc["commitment"].as_str(), Some(HISTORY_COMMITMENT));
}

fn spec(
    id: Hash32,
    schema: &[u8],
    reg: &StaticRegistry,
    snapshot: onyx_core::Snapshot,
    set: &onyx_labels::LabelSet,
) -> ShardSpec {
    ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: set.name.clone(),
            version: set.version,
            content_hash: set.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(schema),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("registered")
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    }
}

#[test]
fn every_history_shard_reruns_to_the_export() {
    let Some(dir) = std::env::var_os("ONYX_HISTORY_DIR").filter(|d| !d.is_empty()) else {
        eprintln!(
            "skipped: set ONYX_HISTORY_DIR to the fetched blocks {HISTORY_FROM}-{HISTORY_TO} \
             (scripts/fetch-btc-node.sh) to rerun the history"
        );
        return;
    };
    let dir = root().join(PathBuf::from(dir));
    let doc = index();
    let by_from = day_leaves(&doc);
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    // The last shard must end at the pinned tip (HIST-R1-F4).
    let tip_pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: Some((HISTORY_TO, HISTORY_TIP.to_string())),
    };
    // Each block file's header is the header the export (and the page) shows.
    let headers = doc["headers"].as_array().expect("headers");
    assert_eq!(headers.len() as u64, HISTORY_TO - HISTORY_FROM + 1);
    for (i, h) in (HISTORY_FROM..=HISTORY_TO).enumerate() {
        let raw = std::fs::read_to_string(dir.join(format!("{h}.raw.hex"))).expect("raw hex");
        assert_eq!(
            raw.trim().get(..160),
            headers[i].as_str(),
            "block {h}: file header differs from the export"
        );
    }
    let labels = load_label_set(&root().join("fixtures"), LABELS_V2).expect("labels v2");
    let host = Host::new().expect("host");
    let swasm = std::fs::read(root().join("recipes/out/btc_entity_flow_series.wasm")).expect("w");
    let bwasm = std::fs::read(root().join("recipes/out/btc_entity_balance.wasm")).expect("w");
    // The planner gives the export's day and shard boundaries.
    let blocks: Vec<(u64, u64, u64)> = (HISTORY_FROM..=HISTORY_TO)
        .zip(headers)
        .map(|(h, header)| {
            let (src, _) = node::load_node_range(&dir, h, h, &pin).expect("block");
            let b = BtcSource::try_from_slice(&src).expect("source");
            let n: usize = b
                .blocks
                .iter()
                .flat_map(|b| b.txs.iter())
                .map(|t| t.spent.len() + t.created.len())
                .sum();
            let raw = hex::decode(header.as_str().expect("header")).expect("hex");
            let time = u32::from_le_bytes(raw[68..72].try_into().expect("4 bytes"));
            (h, n as u64, u64::from(time))
        })
        .collect();
    let days = plan_days(&blocks, PLAN_FUEL_TARGET);
    let exported_days: Vec<(String, u64, u64)> = doc["days"]
        .as_array()
        .expect("days")
        .iter()
        .map(|d| {
            (
                d["date"].as_str().expect("date").to_string(),
                d["from"].as_u64().expect("from"),
                d["to"].as_u64().expect("to"),
            )
        })
        .collect();
    let planned_days: Vec<(String, u64, u64)> =
        days.iter().map(|d| (d.0.clone(), d.1, d.2)).collect();
    assert_eq!(planned_days, exported_days, "day boundaries");
    let plan: Vec<(u64, u64)> = days.iter().flat_map(|d| d.3.iter().copied()).collect();
    let exported: Vec<(u64, u64)> = shards(&doc).iter().map(|s| (s.from, s.to)).collect();
    assert_eq!(plan, exported, "shard boundaries");
    for s in doc["shards"].as_array().expect("shards") {
        let (from, to) = (s["from"].as_u64().expect("f"), s["to"].as_u64().expect("t"));
        let shard_pin = if to == HISTORY_TO { &tip_pin } else { &pin };
        let (source, snapshot) = node::load_node_range(&dir, from, to, shard_pin).expect("range");
        for (wasm, schema, reg, key) in [
            (
                &swasm,
                SCHEMA_ENTITY_FLOW_SERIES,
                fixture_registry_series(),
                "series_leaves",
            ),
            (
                &bwasm,
                b"onyx.btc.entity-flow-row.v1".as_slice(),
                fixture_registry_v2(),
                "balance_leaves",
            ),
        ] {
            let id = keccak256(wasm);
            let sp = spec(id, schema, &reg, snapshot.clone(), &labels);
            let input = assemble_input(&sp, source.clone(), Some(&labels), &[], &reg)
                .expect("assemble")
                .bytes;
            let r = host.run(&id, wasm, &input, &profile()).expect("run");
            assert!(
                r.fuel_used <= profile().fuel_limit / 2,
                "{from}-{to} {key}: fuel {} over half",
                r.fuel_used
            );
            let (series, balance) = by_from.get(&from).expect("leaves");
            let want = if key == "series_leaves" {
                series
            } else {
                balance
            };
            assert_eq!(&r.leaves, want, "{from}-{to} {key}");
        }
        eprintln!("history shard {from}-{to}: both recipes rerun to the export");
    }
}
