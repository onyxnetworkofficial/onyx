//! Golden test of `btc-chain-stats` (ADR-0043). Always: a synthetic two-block
//! source runs on the canonical engine to the checked-in golden root and
//! rows, byte-identical across runs and with the native build; the website's
//! committed window rebuilds its commitment from its leaves, and its shipped
//! block input reruns to that block's root. With the gitignored live blocks
//! (`ONYX_CHAIN_DIRS`, a colon list; default `fixtures-live/week` and
//! `fixtures-live/tail`), a sample of the window's blocks (first, last, the
//! shipped one and every 50th) is rebuilt from raw blocks and rerun to the
//! committed leaves, and the shipped input is the one those blocks give;
//! without them that test says so and returns. Regenerate the golden file
//! with `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden_chain`
//! (only with a recipe change), and the window with
//! `examples/export-chain-stats.rs`.

use borsh::BorshDeserialize;
use onyx_chain_schema::{
    ChainBlock, ChainInput, ChainSource, ChainStatsRow, ChainTx, TxOut, CHAIN_SCHEMA_VERSION,
};
use onyx_core::{keccak256, ChainId, Hash32, Registry, RuntimeProfile, ShardSpec, Snapshot};
use onyx_core::{SourceId, SourceRef};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::chain::{chain_stats_commitment, load_chain_range, ChainShard};
use onyx_source_btc::node::{NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::{fixture_registry_chain, CHAIN_ID, SCHEMA_CHAIN_STATS, SOURCE_ID};
use serde_json::Value;
use std::path::PathBuf;

const GOLDEN: &str = "golden/btc-chain-stats-synthetic.json";
const WINDOW: &str = "apps/web/static/onchain/btc-chain-stats.json";
const RERUN: &str = "apps/web/static/onchain/rerun-input.b64.txt";

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

fn wasm() -> (Vec<u8>, Hash32) {
    let w = std::fs::read(root().join("recipes/out/btc_chain_stats.wasm"))
        .expect("built recipe; run scripts/build-recipes.sh");
    let id = keccak256(&w);
    let rec = std::fs::read_to_string(root().join("recipes/out/btc_chain_stats.wasm.keccak"))
        .expect("keccak file");
    assert_eq!(rec.trim(), id.to_string(), "id file out of date");
    (w, id)
}

fn spec(id: Hash32, snapshot: Snapshot) -> ShardSpec {
    let reg = fixture_registry_chain();
    let spec = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: None,
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(SCHEMA_CHAIN_STATS),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("source")
            .hash(),
        params: vec![],
        registry_root: reg.root(),
    };
    assert_eq!(spec.validate(&reg), Ok(()));
    spec
}

/// Assemble and run one shard twice; returns leaves, fuel and the input.
fn run(source: Vec<u8>, snapshot: Snapshot) -> (Vec<Vec<u8>>, u64, Vec<u8>) {
    let (w, id) = wasm();
    let reg = fixture_registry_chain();
    let input = assemble_input(&spec(id, snapshot), source, None, &[], &reg)
        .expect("assemble")
        .bytes;
    let host = Host::new().expect("host");
    let r = host.run(&id, &w, &input, &profile()).expect("run");
    assert_eq!(r.outcome, Outcome::Ok);
    let again = host.run(&id, &w, &input, &profile()).expect("run");
    assert_eq!(r.leaves, again.leaves, "deterministic");
    assert!(r.fuel_used * 2 <= 4_000_000_000, "within half the fuel");
    (r.leaves, r.fuel_used, input)
}

fn sha256d(b: &[u8]) -> [u8; 32] {
    btc_chain_stats::sha256d(b)
}

/// Two linked blocks at 970,000 and 970,001 with real header hashes.
fn synthetic() -> ChainSource {
    let header = |prev: [u8; 32], time: u32| {
        let mut h = [0u8; 80];
        h[0] = 2;
        h[4..36].copy_from_slice(&prev);
        h[68..72].copy_from_slice(&time.to_le_bytes());
        h[72..76].copy_from_slice(&0x1702_1ef0u32.to_le_bytes());
        h
    };
    let p2tr = {
        let mut s = vec![7u8; 34];
        s[0] = 0x51;
        s[1] = 0x20;
        s
    };
    let out = |s: &[u8], v| TxOut {
        script: s.to_vec(),
        value: v,
    };
    let txs = |extra: u64| {
        vec![
            ChainTx {
                base_size: 200,
                total_size: 236,
                inputs: vec![],
                outputs: vec![
                    out(&p2tr, 312_500_000 + 3_000 + extra),
                    out(b"\x6a\x01x", 0),
                ],
            },
            ChainTx {
                base_size: 250,
                total_size: 250,
                inputs: vec![ChainInput {
                    value: 10_000 + extra,
                    script: b"\x76\xa9a".to_vec(),
                    created_height: 969_990,
                }],
                outputs: vec![out(b"\x00\x14b", 6_000), out(b"\x76\xa9a", 3_000)],
            },
            ChainTx {
                base_size: 100,
                total_size: 300,
                inputs: vec![ChainInput {
                    value: 50_000,
                    script: p2tr.clone(),
                    created_height: 969_999,
                }],
                outputs: vec![out(b"\x00\x14b", 48_000)],
            },
        ]
    };
    let parent = header([3; 32], 1_791_000_000);
    let mut prev = sha256d(&parent);
    let mut blocks = Vec::new();
    for (k, t) in [1_791_000_600u32, 1_791_000_500].into_iter().enumerate() {
        let h = header(prev, t);
        let hash = sha256d(&h);
        let mut display = hash;
        display.reverse();
        blocks.push(ChainBlock {
            height: 970_000 + k as u64,
            hash: display,
            header: h,
            txs: txs(k as u64 * 500),
        });
        prev = hash;
    }
    ChainSource {
        schema_version: CHAIN_SCHEMA_VERSION,
        chain: CHAIN_ID.into(),
        parent_header: parent,
        blocks,
    }
}

fn snapshot_of(src: &ChainSource, bytes: &[u8]) -> Snapshot {
    Snapshot::BlockRange {
        chain: ChainId(CHAIN_ID.into()),
        from_height: src.blocks[0].height,
        to_height: src.blocks[src.blocks.len() - 1].height,
        tip_hash: Hash32(src.blocks[src.blocks.len() - 1].hash),
        content_hash: keccak256(bytes),
    }
}

fn row(leaf: &[u8]) -> ChainStatsRow {
    let r = OutputRow::try_from_slice(leaf).expect("row");
    let v = ChainStatsRow::try_from_slice(&r.value).expect("stats row");
    assert_eq!(r.key, ChainStatsRow::key(v.height));
    v
}

#[test]
fn a_synthetic_source_gives_the_golden_rows() {
    let src = synthetic();
    let bytes = borsh::to_vec(&src).expect("source");
    let (leaves, fuel, input) = run(bytes.clone(), snapshot_of(&src, &bytes));
    // Native build agrees with the WebAssembly.
    let native = btc_chain_stats::run(RecipeInput::try_from_slice(&input).expect("input"))
        .expect("native run");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("leaf"))
        .collect();
    assert_eq!(native_leaves, leaves);
    let rows: Vec<ChainStatsRow> = leaves.iter().map(|l| row(l)).collect();
    // The second block is 100 s older than the first (allowed) and its
    // first spend is 500 sat larger, all of it fee.
    assert_eq!(rows[0].interval_secs, 600);
    assert_eq!(rows[1].interval_secs, -100);
    assert_eq!(rows[0].fees_sats, 3_000);
    assert_eq!(rows[1].fees_sats, 3_500);
    assert_eq!(rows[1].coin_blocks_destroyed, 10_500 * 11 + 50_000 * 2);
    let got = serde_json::json!({
        "recipe_id": wasm().1.to_string(),
        "root": MerkleTree::from_rows(&leaves).root().to_string(),
        "fuel": fuel,
        "rows": rows.iter().map(|r| serde_json::json!({
            "height": r.height, "txs": r.txs, "fees_sats": r.fees_sats,
            "fee_rate_median_msat_vb": r.fee_rate_median_msat_vb,
            "fee_rate_avg_msat_vb": r.fee_rate_avg_msat_vb, "size_bytes": r.size_bytes,
            "weight_wu": r.weight_wu, "active_scripts": r.active_scripts,
            "issued_sats": r.issued_sats, "segwit_txs": r.segwit_txs,
        })).collect::<Vec<_>>(),
    });
    let path = root().join("fixtures").join(GOLDEN);
    if std::env::var("ONYX_BLESS").is_ok() {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&got).expect("json") + "\n",
        )
        .expect("bless");
    }
    let want: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("golden file")).expect("golden");
    assert_eq!(got, want, "golden differs; bless only with a recipe change");
}

fn window() -> Value {
    serde_json::from_slice(&std::fs::read(root().join(WINDOW)).expect("window")).expect("json")
}

fn window_leaves(doc: &Value) -> Vec<Vec<u8>> {
    doc["days"]
        .as_array()
        .expect("days")
        .iter()
        .flat_map(|d| d["leaves"].as_array().expect("leaves").clone())
        .map(|l| hex::decode(l.as_str().expect("s").trim_start_matches("0x")).expect("hex"))
        .collect()
}

fn hex32(s: &str) -> [u8; 32] {
    let mut b = [0u8; 32];
    hex::decode_to_slice(s.trim_start_matches("0x"), &mut b).expect("32 bytes");
    b
}

/// The committed window: one leaf per block, exactly the range, and the
/// commitment rebuilt from the leaves is the file's. Its recipe is the
/// committed one.
#[test]
fn the_committed_window_rebuilds_its_commitment() {
    let doc = window();
    assert_eq!(
        doc["recipe_id"].as_str(),
        Some(wasm().1.to_string().as_str())
    );
    let leaves = window_leaves(&doc);
    let (from, to) = (
        doc["from"].as_u64().expect("from"),
        doc["to"].as_u64().expect("to"),
    );
    let rows: Vec<ChainStatsRow> = leaves.iter().map(|l| row(l)).collect();
    assert_eq!(
        rows.iter().map(|r| r.height).collect::<Vec<_>>(),
        (from..=to).collect::<Vec<_>>()
    );
    let shards: Vec<ChainShard> = leaves
        .iter()
        .zip(&rows)
        .map(|(l, r)| ChainShard {
            from: r.height,
            to: r.height,
            root: MerkleTree::from_rows(std::slice::from_ref(l)).root(),
        })
        .collect();
    let tip = rows.last().expect("tip").hash;
    assert_eq!(hex::encode(tip), doc["tip"].as_str().expect("tip"));
    let c = chain_stats_commitment(
        &Hash32(hex32(doc["recipe_id"].as_str().expect("id"))),
        &tip,
        &shards,
    );
    assert_eq!(
        c.to_string(),
        doc["commitment"].as_str().expect("commitment")
    );
    assert!(doc["python_recount"]
        .as_str()
        .expect("recount")
        .contains(&format!("all 22 fields of all {} blocks", to - from + 1)));
}

fn rerun_input() -> Vec<u8> {
    let b64 = std::fs::read_to_string(root().join(RERUN)).expect("rerun input");
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim()).expect("b64")
}

/// The shipped block input (what the browser reruns) runs to the root the
/// window holds for that block.
#[test]
fn the_shipped_block_input_reruns_to_its_root() {
    let doc = window();
    let input = rerun_input();
    assert_eq!(
        keccak256(&input).to_string(),
        doc["rerun"]["input_keccak"].as_str().expect("k")
    );
    let (w, id) = wasm();
    let r = Host::new()
        .expect("host")
        .run(&id, &w, &input, &profile())
        .expect("run");
    assert_eq!(r.outcome, Outcome::Ok);
    let h = doc["rerun"]["height"].as_u64().expect("height");
    let leaf = window_leaves(&doc)
        .into_iter()
        .find(|l| row(l).height == h)
        .expect("leaf of the rerun block");
    assert_eq!(r.leaves, vec![leaf]);
    assert_eq!(
        r.root().to_string(),
        doc["rerun"]["root"].as_str().expect("root")
    );
}

/// With the live blocks: a sample of the window rebuilt from raw blocks and
/// rerun gives the committed leaves, and the shipped input is the one the
/// blocks give for its height.
#[test]
fn live_blocks_rerun_to_the_committed_window() {
    let dirs: Vec<PathBuf> = std::env::var("ONYX_CHAIN_DIRS")
        .map(|v| v.split(':').map(PathBuf::from).collect())
        .unwrap_or_else(|_| {
            vec![
                root().join("fixtures-live/week"),
                root().join("fixtures-live/tail"),
            ]
        });
    let doc = window();
    let (from, to) = (
        doc["from"].as_u64().expect("from"),
        doc["to"].as_u64().expect("to"),
    );
    if !dirs
        .iter()
        .any(|d| d.join(format!("{from}.raw.hex")).is_file())
    {
        eprintln!("live blocks not present ({dirs:?}): set ONYX_CHAIN_DIRS to rerun the window");
        return;
    }
    let leaves = window_leaves(&doc);
    let rerun_h = doc["rerun"]["height"].as_u64().expect("height");
    let mut sample: Vec<u64> = (from..=to).step_by(50).collect();
    sample.extend([to, rerun_h]);
    sample.sort_unstable();
    sample.dedup();
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: None,
    };
    for h in sample {
        let (source, snapshot) = load_chain_range(&dirs, h, h, &pin).expect("blocks load");
        let (got, _, input) = run(source, snapshot);
        assert_eq!(got, vec![leaves[(h - from) as usize].clone()], "block {h}");
        if h == rerun_h {
            assert_eq!(input, rerun_input(), "the shipped input is the chain's");
        }
    }
}
