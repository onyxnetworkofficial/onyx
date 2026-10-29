//! Golden test of `eth-chain-stats` (ADR-0044). Always: a synthetic
//! two-block source runs on the canonical engine to the checked-in golden
//! root and rows, byte-identical across runs and with the native build; the
//! three committed mainnet blocks (`fixtures/eth/blocks-26225268-26225270`)
//! pass every adapter check and rerun to the website window's leaves for
//! those blocks; edited answers are refused; the website's committed window
//! rebuilds its commitment from its leaves, and its shipped block input
//! reruns to that block's root. With the gitignored window
//! (`ONYX_ETH_DIR`, default `fixtures-live/eth`, fetched by
//! `scripts/refresh-eth-onchain.py`, from any node), its manifest must name the
//! committed window's chain, range and tip, and a sample of blocks (first, last, the shipped one and every 50th) is
//! rebuilt and rerun to the committed leaves; without it that test says so
//! and returns. Regenerate the golden file with
//! `ONYX_BLESS=1 cargo test -p onyx-source-eth --test golden_eth_chain`
//! (only with a recipe change), and the window with
//! `examples/export-eth-chain-stats.rs`.

use borsh::BorshDeserialize;
use onyx_core::{keccak256, ChainId, Hash32, Registry, RuntimeProfile, Snapshot};
use onyx_eth_schema::decode;
use onyx_eth_schema::synthetic::{self, HeaderSpec, TxSpec};
use onyx_eth_schema::{EthChainSource, EthChainStatsRow, ETH_MAINNET, ETH_SCHEMA_VERSION};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_eth::{
    cross_checked, eth_block, eth_chain_stats_commitment, fixture_registry_eth, header_rlp,
    load_eth_range, spec_binds_snapshot, stats_spec, EthAdapterError, EthShard, Window, CHAIN_ID,
};
use serde_json::Value;
use std::path::PathBuf;

const GOLDEN: &str = "fixtures/golden/eth-chain-stats-synthetic.json";
const WINDOW: &str = "apps/web/static/onchain/eth-chain-stats.json";
const RERUN: &str = "apps/web/static/onchain/eth-rerun-input.b64.txt";
const SUBSET: &str = "fixtures/eth/blocks-26225268-26225270";
const MANIFEST: &str = "fixtures/eth/window-26225261-26225560.manifest.json";

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
    let w = std::fs::read(root().join("recipes/out/eth_chain_stats.wasm"))
        .expect("built recipe; run scripts/build-recipes.sh");
    let id = keccak256(&w);
    let rec = std::fs::read_to_string(root().join("recipes/out/eth_chain_stats.wasm.keccak"))
        .expect("keccak file");
    assert_eq!(rec.trim(), id.to_string(), "id file out of date");
    (w, id)
}

/// Assemble and run one shard twice; returns leaves, fuel and the input.
fn run(source: Vec<u8>, snapshot: Snapshot) -> (Vec<Vec<u8>>, u64, Vec<u8>) {
    let (w, id) = wasm();
    let reg = fixture_registry_eth();
    let spec = stats_spec(id, profile(), snapshot);
    assert_eq!(spec.validate(&reg), Ok(()));
    assert!(
        spec_binds_snapshot(&spec),
        "params pin the snapshot's range and tip"
    );
    let input = assemble_input(&spec, source, None, &[], &reg)
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

fn snapshot_of(src: &EthChainSource, bytes: &[u8]) -> Snapshot {
    let last = &src.blocks[src.blocks.len() - 1];
    Snapshot::BlockRange {
        chain: ChainId(CHAIN_ID.into()),
        from_height: src.blocks[0].number,
        to_height: last.number,
        tip_hash: Hash32(last.hash),
        content_hash: keccak256(bytes),
    }
}

fn row(leaf: &[u8]) -> EthChainStatsRow {
    let r = OutputRow::try_from_slice(leaf).expect("row");
    let v = EthChainStatsRow::try_from_slice(&r.value).expect("stats row");
    assert_eq!(r.key, EthChainStatsRow::key(v.number));
    v
}

const T0: u64 = 1_792_396_499;

/// Two linked synthetic blocks over every transaction type, with real hashes and roots.
fn synthetic() -> EthChainSource {
    let spec = |number, parent_hash, timestamp, base_fee, blob| HeaderSpec {
        parent_hash,
        number,
        gas_limit: 60_000_000,
        gas_used: 30_000_000,
        timestamp,
        base_fee,
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        blob: Some(blob),
    };
    let t = |tx_type, to, value, max_fee, max_priority, blobs| TxSpec {
        tx_type,
        to,
        value,
        max_fee,
        max_priority,
        blobs,
    };
    let a = Some([0xaa; 20]);
    // The parent's excess is 14 blobs above 172,147,094, so the child's (EIP-7918, BPO2) is that.
    let parent = synthetic::header(&spec(
        26_000_000,
        [3; 32],
        T0,
        40_000_000,
        (0, 172_147_094 + 14 * (1 << 17)),
    ));
    let next = |h: &[u8]| {
        decode::next_excess_blob_gas(&decode::header(h).expect("header")).expect("excess")
    };
    let b1 = synthetic::block(
        spec(
            26_000_001,
            onyx_eth_schema::keccak256(&parent),
            T0 + 12,
            40_000_000,
            (3 << 17, next(&parent)),
        ),
        &[
            (t(0, a, 10u128.pow(18), 41_000_000, 0, 0), true, 21_000),
            (t(1, None, 0, 40_000_000, 0, 0), true, 90_000),
            (t(2, a, 5, 90_000_000, 2_000_000, 0), false, 60_000),
            (t(3, a, 0, 45_000_000, 1_000_000, 3), true, 21_000),
            (t(4, a, 7, 40_500_000, 1_000_000, 0), true, 46_000),
        ],
    );
    // Block 1 used 238,000 gas of 60 M: the base fee falls.
    let base2 = 40_000_000 - 40_000_000 * (30_000_000 - 238_000) / 30_000_000 / 8;
    let b2 = synthetic::block(
        spec(26_000_002, b1.hash, T0 + 36, base2, (0, next(&b1.header))),
        &[(t(2, a, 1, 50_000_000, 3, 0), true, 21_000)],
    );
    EthChainSource {
        schema_version: ETH_SCHEMA_VERSION,
        chain: ETH_MAINNET.into(),
        parent_header: parent,
        blocks: vec![b1, b2],
    }
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

#[test]
fn a_synthetic_source_gives_the_golden_rows() {
    let src = synthetic();
    let bytes = borsh::to_vec(&src).expect("source");
    let (leaves, fuel, input) = run(bytes.clone(), snapshot_of(&src, &bytes));
    // Native build agrees with the WebAssembly.
    let native = eth_chain_stats::run(RecipeInput::try_from_slice(&input).expect("input"))
        .expect("native run");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("leaf"))
        .collect();
    assert_eq!(native_leaves, leaves, "native and WebAssembly rows");
    let rows: Vec<EthChainStatsRow> = leaves.iter().map(|l| row(l)).collect();
    // Spot definitions (R-ETH-2) before comparing with the golden file.
    let a = &rows[0];
    assert_eq!(
        (a.txs, a.failed_txs, a.contract_creations, a.blob_txs),
        (5, 1, 1, 1)
    );
    assert_eq!(a.gas_used, 238_000);
    assert_eq!(a.burned_wei, 40_000_000 * 238_000);
    assert_eq!(
        a.priority_fees_wei,
        1_000_000 * 21_000 + 2_000_000 * 60_000 + 1_000_000 * 21_000 + 500_000 * 46_000
    );
    assert_eq!(a.blob_gas_used, 3 << 17);
    assert_eq!(a.blob_fee_wei, 2_502_320 * (3 << 17));
    // The reverted type-2 transaction's 5 wei is not sent (P4A-R2-1).
    assert_eq!(a.value_wei, 10u128.pow(18) + 7);
    let root = MerkleTree::from_rows(&leaves).root();
    let doc = serde_json::json!({
        "recipe_id": wasm().1.to_string(),
        "input_keccak": keccak256(&input).to_string(),
        "root": root.to_string(),
        "fuel": fuel,
        "leaves": leaves.iter().map(|l| hex0x(l)).collect::<Vec<_>>(),
    });
    let path = root_path(GOLDEN);
    if std::env::var("ONYX_BLESS").ok().as_deref() == Some("1") {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&doc).expect("json") + "\n",
        )
        .expect("bless");
    }
    let want: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("golden file; bless once"))
            .expect("golden json");
    for k in ["recipe_id", "input_keccak", "root", "leaves"] {
        assert_eq!(doc[k], want[k], "golden {k}");
    }
}

fn root_path(rel: &str) -> PathBuf {
    root().join(rel)
}

fn window_doc() -> Value {
    serde_json::from_slice(&std::fs::read(root_path(WINDOW)).expect("window")).expect("json")
}

/// Every leaf of the website window, by block number.
fn window_leaves(doc: &Value) -> std::collections::BTreeMap<u64, String> {
    doc["days"]
        .as_array()
        .expect("days")
        .iter()
        .flat_map(|d| d["leaves"].as_array().cloned().unwrap_or_default())
        .map(|l| {
            let s = l.as_str().expect("leaf").to_string();
            let b = hex::decode(s.trim_start_matches("0x")).expect("hex");
            (row(&b).number, s)
        })
        .collect()
}

#[test]
fn committed_mainnet_blocks_rerun_to_the_window_leaves() {
    let w = Window::open(&root_path(SUBSET)).expect("subset");
    let leaves = window_leaves(&window_doc());
    let mut one_by_one = Vec::new();
    for n in 26_225_268..=26_225_270u64 {
        let (bytes, snap) = load_eth_range(&w, n, n).expect("adapter checks");
        let (l, _, _) = run(bytes, snap);
        assert_eq!(l.len(), 1);
        assert_eq!(
            Some(&hex0x(&l[0])),
            leaves.get(&n),
            "block {n}: the window's leaf"
        );
        one_by_one.push(l[0].clone());
    }
    // R-ETH-3: the three blocks as one shard give the same rows.
    let (bytes, snap) = load_eth_range(&w, 26_225_268, 26_225_270).expect("range");
    let (l, _, _) = run(bytes, snap);
    assert_eq!(l, one_by_one);
    // The subset's answers are the committed window manifest's.
    let full: Value =
        serde_json::from_slice(&std::fs::read(root_path(MANIFEST)).expect("manifest"))
            .expect("json");
    for (k, v) in w.manifest["sha256"].as_object().expect("sha") {
        assert_eq!(&full["sha256"][k], v, "{k}");
    }
}

fn answers(n: u64) -> (Value, Value, Vec<u8>) {
    let w = Window::open(&root_path(SUBSET)).expect("subset");
    let b = w.answer(&format!("{n}.block.json.gz")).expect("block");
    let r = w
        .answer(&format!("{n}.receipts.json.gz"))
        .expect("receipts");
    let p = header_rlp(
        &w.answer(&format!("{}.block.json.gz", n - 1))
            .expect("parent"),
    )
    .expect("rlp");
    (b, r, p)
}

#[test]
fn edited_answers_are_refused() {
    // 26,225,269 holds blob transactions (receipts 1, 2 and 30).
    let n = 26_225_269;
    let (b, r, p) = answers(n);
    assert!(eth_block(n, &b, &r, &p).is_ok());
    let refused = |edit: &dyn Fn(&mut Value, &mut Value), why: &str| {
        let (mut b, mut r) = (b.clone(), r.clone());
        edit(&mut b, &mut r);
        match eth_block(n, &b, &r, &p) {
            Err(EthAdapterError::Answer { what, .. }) => assert!(what.contains(why), "{what}"),
            other => panic!("not refused ({why}): {:?}", other.map(|_| ())),
        }
    };
    let blob_ix = r
        .as_array()
        .expect("receipts")
        .iter()
        .position(|x| x.get("blobGasPrice").is_some())
        .expect("a blob receipt in 26,225,269");
    refused(
        &|b, _| b["transactions"][3]["value"] = "0x123456789abcdef".into(),
        "does not hash to its hash",
    );
    refused(&|b, _| b["gasUsed"] = "0x1".into(), "header does not hash");
    refused(
        &|b, _| b["blockAccessListHash"] = "0x00".into(),
        "unknown block field",
    );
    refused(&|_, r| r[5]["gasUsed"] = "0x1".into(), "gasUsed");
    refused(
        &|_, r| r[5]["effectiveGasPrice"] = "0x1".into(),
        "effectiveGasPrice",
    );
    refused(
        &|_, r| r[blob_ix]["blobGasPrice"] = "0x1".into(),
        "blobGasPrice",
    );
    refused(
        &|_, r| {
            r[7]["status"] = if r[7]["status"] == "0x1" {
                "0x0".into()
            } else {
                "0x1".into()
            }
        },
        "receipts root",
    );
    refused(
        &|_, r| {
            r.as_array_mut().expect("list").pop();
        },
        "one receipt per transaction",
    );
    refused(
        &|b, r| {
            b["transactions"].as_array_mut().expect("txs").swap(1, 2);
            r.as_array_mut().expect("list").swap(1, 2);
        },
        "receipt 1",
    );
    // Another block's parent.
    let (_, _, other_parent) = answers(n + 1);
    match eth_block(n, &b, &r, &other_parent) {
        Err(EthAdapterError::Answer { what, .. }) => assert!(what.contains("child")),
        other => panic!("not refused: {:?}", other.map(|_| ())),
    }
}

#[test]
fn a_file_that_is_not_the_manifest_answer_is_refused() {
    let dir = std::env::temp_dir().join(format!("onyx-eth-subset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    for e in std::fs::read_dir(root_path(SUBSET)).expect("subset") {
        let e = e.expect("entry");
        std::fs::copy(e.path(), dir.join(e.file_name())).expect("copy");
    }
    // Swap block 26,225,269's answer for 26,225,270's: right format, wrong bytes.
    std::fs::copy(
        dir.join("26225270.block.json.gz"),
        dir.join("26225269.block.json.gz"),
    )
    .expect("swap");
    let w = Window::open(&dir).expect("open");
    assert!(matches!(
        load_eth_range(&w, 26_225_268, 26_225_270),
        Err(EthAdapterError::Checksum(_))
    ));
    assert!(load_eth_range(&w, 26_225_268, 26_225_268).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_website_window_rebuilds_its_commitment_and_reruns_its_block() {
    let doc = window_doc();
    let (w, id) = wasm();
    assert_eq!(
        doc["recipe_id"].as_str(),
        Some(hex0x(&id.0).as_str()),
        "window made by this recipe"
    );
    let leaves = window_leaves(&doc);
    let (from, to) = (
        doc["from"].as_u64().expect("from"),
        doc["to"].as_u64().expect("to"),
    );
    assert_eq!(
        leaves.keys().copied().collect::<Vec<_>>(),
        (from..=to).collect::<Vec<_>>()
    );
    let shards: Vec<EthShard> = leaves
        .iter()
        .map(|(n, l)| EthShard {
            from: *n,
            to: *n,
            root: MerkleTree::from_rows(&[hex::decode(l.trim_start_matches("0x")).expect("hex")])
                .root(),
        })
        .collect();
    let mut tip = [0u8; 32];
    tip.copy_from_slice(&hex::decode(doc["tip"].as_str().expect("tip")).expect("hex"));
    assert_eq!(
        hex0x(&eth_chain_stats_commitment(&id, &tip, &shards).0),
        doc["commitment"].as_str().expect("commitment")
    );
    assert_eq!(
        row(&hex::decode(leaves[&to].trim_start_matches("0x")).expect("hex")).hash,
        tip
    );
    assert!(doc["python_recount"]
        .as_str()
        .expect("recount")
        .contains("match"));
    // The shipped input: the pinned block, and it reruns to the window's root.
    let b64 = std::fs::read_to_string(root_path(RERUN)).expect("rerun input");
    let input = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())
        .expect("b64");
    let n = doc["rerun"]["number"].as_u64().expect("number");
    assert_eq!(
        doc["rerun"]["input_keccak"].as_str(),
        Some(hex0x(&keccak256(&input).0).as_str())
    );
    let src =
        EthChainSource::try_from_slice(&RecipeInput::try_from_slice(&input).expect("input").source)
            .expect("source");
    assert_eq!(
        src.blocks.iter().map(|b| b.number).collect::<Vec<_>>(),
        vec![n]
    );
    let r = Host::new()
        .expect("host")
        .run(&id, &w, &input, &profile())
        .expect("run");
    assert_eq!(r.outcome, Outcome::Ok);
    assert_eq!(
        hex0x(&r.root().0),
        doc["rerun"]["root"].as_str().expect("root")
    );
    assert_eq!(Some(&hex0x(&r.leaves[0])), leaves.get(&n));
    let reg = fixture_registry_eth();
    assert!(reg
        .metric(onyx_source_eth::METRIC_ETH_CHAIN_STATS)
        .is_some());
}

#[test]
fn the_live_window_reruns_to_the_committed_leaves() {
    let dir = std::env::var("ONYX_ETH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root_path("fixtures-live/eth"));
    if !dir.join("MANIFEST.json").exists() {
        eprintln!(
            "no live window at {} (scripts/refresh-eth-onchain.py fetches it): skipped",
            dir.display()
        );
        return;
    }
    let w = Window::open(&dir).expect("window");
    let full: Value =
        serde_json::from_slice(&std::fs::read(root_path(MANIFEST)).expect("manifest"))
            .expect("json");
    same_window(&full, &w.manifest).expect("the live window is the committed window's blocks");
    if w.manifest != full {
        eprintln!(
            "the live window's answers come from {} (not the committed manifest's bytes): checking its blocks",
            w.manifest["rpc"]
        );
    }
    let doc = window_doc();
    let leaves = window_leaves(&doc);
    let (from, to) = (
        doc["from"].as_u64().expect("from"),
        doc["to"].as_u64().expect("to"),
    );
    let rerun = doc["rerun"]["number"].as_u64().expect("rerun");
    let mut sample: Vec<u64> = (from..=to).step_by(50).chain([to, rerun]).collect();
    sample.sort_unstable();
    sample.dedup();
    for n in &sample {
        let (bytes, snap) = load_eth_range(&w, *n, *n).expect("adapter checks");
        let (l, _, input) = run(bytes, snap);
        assert_eq!(Some(&hex0x(&l[0])), leaves.get(n), "block {n}");
        if *n == rerun {
            let b64 = std::fs::read_to_string(root_path(RERUN)).expect("rerun input");
            assert_eq!(
                b64.trim(),
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &input)
            );
        }
    }
    eprintln!(
        "{} sample blocks rebuilt from the live window and rerun",
        sample.len()
    );
}

/// P4A-R2-2: a live window may come from any node (the project owner's own, say): its answers' bytes and
/// SHA-256 differ from the committed manifest's (another client prints other JSON), but it must
/// name the same chain, range and tip. The blocks themselves are then rebuilt from its answers,
/// checked against the chain's hashes and rerun to the committed leaves.
fn same_window(committed: &Value, live: &Value) -> Result<(), String> {
    for k in ["chain", "parent", "from", "to"] {
        if committed[k] != live[k] {
            return Err(format!("`{k}` is {}, committed {}", live[k], committed[k]));
        }
    }
    let tip = |m: &Value| m["tip"].as_str().map(str::to_ascii_lowercase);
    if tip(live).is_none() || tip(live) != tip(committed) {
        return Err(format!(
            "tip {}, committed {}",
            live["tip"], committed["tip"]
        ));
    }
    Ok(())
}

#[test]
fn p4a_r2_a_window_from_another_node_is_accepted_by_range_and_tip() {
    let full: Value =
        serde_json::from_slice(&std::fs::read(root_path(MANIFEST)).expect("manifest"))
            .expect("json");
    assert_eq!(same_window(&full, &full), Ok(()));
    let mut own = full.clone();
    own["rpc"] = "http://127.0.0.1:8545".into();
    own["check_rpc"] = Value::Null;
    own["cross_checked"] = Value::Array(vec![]);
    own["sha256"] = serde_json::json!({});
    own["fetched_at"] = "2026-10-20T00:00:00Z".into();
    assert_eq!(
        same_window(&full, &own),
        Ok(()),
        "another node's bytes, same blocks"
    );
    for (k, v) in [
        ("tip", Value::from(hex0x(&[0; 32]))),
        ("to", Value::from(26_225_561u64)),
        ("from", Value::from(26_225_262u64)),
        ("chain", Value::from("eth.sepolia")),
    ] {
        let mut bad = own.clone();
        bad[k] = v;
        assert!(same_window(&full, &bad).is_err(), "{k}");
    }
    let mut no_tip = own;
    no_tip.as_object_mut().expect("manifest").remove("tip");
    assert!(same_window(&full, &no_tip).is_err());
}

/// P4A-R1-1: a shard whose snapshot (and so params) names another tip or range than the
/// source's blocks fails in the recipe, so rows cannot be labelled as other blocks.
#[test]
fn a_snapshot_naming_other_blocks_fails_the_run() {
    let w = Window::open(&root_path(SUBSET)).expect("subset");
    let (bytes, snap) = load_eth_range(&w, 26_225_269, 26_225_269).expect("source");
    let (wasm, id) = wasm();
    let reg = fixture_registry_eth();
    let Snapshot::BlockRange {
        chain,
        from_height,
        to_height,
        tip_hash,
        content_hash,
    } = snap
    else {
        panic!("range");
    };
    let other_tip = Snapshot::BlockRange {
        chain: chain.clone(),
        from_height,
        to_height,
        tip_hash: Hash32([7; 32]),
        content_hash,
    };
    let other_range = Snapshot::BlockRange {
        chain,
        from_height: from_height + 1,
        to_height: to_height + 1,
        tip_hash,
        content_hash,
    };
    for snap in [other_tip, other_range] {
        let spec = stats_spec(id, profile(), snap);
        let input = assemble_input(&spec, bytes.clone(), None, &[], &reg)
            .expect("assemble")
            .bytes;
        let r = Host::new()
            .expect("host")
            .run(&id, &wasm, &input, &profile())
            .expect("run");
        assert_eq!(
            r.outcome,
            Outcome::RecipeError(eth_chain_stats::code::BAD_PARAMS)
        );
    }
}

/// P4A-R1-3: the committed window was confirmed by a second operator on another host, and a
/// folder without that confirmation is refused by `cross_checked`.
#[test]
fn the_window_was_cross_checked_by_a_second_operator() {
    let full: Value =
        serde_json::from_slice(&std::fs::read(root_path(MANIFEST)).expect("manifest"))
            .expect("json");
    let host = |k: &str| {
        full[k]
            .as_str()
            .expect(k)
            .split('/')
            .nth(2)
            .expect("host")
            .to_string()
    };
    assert_ne!(host("rpc"), host("check_rpc"));
    let confirmed: Vec<(u64, String)> = full["cross_checked"]
        .as_array()
        .expect("list")
        .iter()
        .map(|c| {
            (
                c["number"].as_u64().expect("n"),
                c["hash"].as_str().expect("h").to_string(),
            )
        })
        .collect();
    let (from, to) = (
        full["from"].as_u64().expect("from"),
        full["to"].as_u64().expect("to"),
    );
    for n in [from - 1, from, to] {
        assert!(
            confirmed.iter().any(|(m, _)| *m == n),
            "block {n} confirmed"
        );
    }
    assert!(confirmed
        .iter()
        .any(|(m, h)| *m == to && h == full["tip"].as_str().expect("tip")));
    assert_eq!(window_doc()["fetch"]["second_operator_confirmed"], true);
    // The three-block subset carries the window's confirmations, not its own: refused.
    let w = Window::open(&root_path(SUBSET)).expect("subset");
    assert!(cross_checked(&w).is_err());
    // With its own confirmations it passes; the same host twice does not.
    let dir = std::env::temp_dir().join(format!("onyx-eth-cross-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    for e in std::fs::read_dir(root_path(SUBSET)).expect("subset") {
        let e = e.expect("entry");
        std::fs::copy(e.path(), dir.join(e.file_name())).expect("copy");
    }
    let mut m = w.manifest.clone();
    m["cross_checked"] = serde_json::Value::Array(
        [26_225_267u64, 26_225_268, 26_225_270]
            .iter()
            .map(|n| {
                let b = w.answer(&format!("{n}.block.json.gz")).expect("block");
                serde_json::json!({"number": n, "hash": b["hash"]})
            })
            .collect(),
    );
    std::fs::write(
        dir.join("MANIFEST.json"),
        serde_json::to_vec(&m).expect("json"),
    )
    .expect("write");
    assert_eq!(cross_checked(&Window::open(&dir).expect("open")), Ok(()));
    m["check_rpc"] = m["rpc"].clone();
    std::fs::write(
        dir.join("MANIFEST.json"),
        serde_json::to_vec(&m).expect("json"),
    )
    .expect("write");
    assert!(cross_checked(&Window::open(&dir).expect("open")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Re-seal a block answer after an edit: its `hash` becomes the Keccak of its re-encoded header,
/// so only the rule under test can refuse it.
fn reseal(b: &mut Value) {
    let h = onyx_eth_schema::keccak256(&header_rlp(b).expect("header"));
    b["hash"] = hex0x(&h).into();
}

/// The block answer of `n` from the committed subset.
fn block_answer(n: u64) -> Value {
    Window::open(&root_path(SUBSET))
        .expect("subset")
        .answer(&format!("{n}.block.json.gz"))
        .expect("block")
}

/// Insert a leading zero into a quantity's hex digits: `0x1f` -> `0x01f`.
fn leading_zero(v: &Value) -> Value {
    let s = v.as_str().expect("quantity");
    format!("0x0{}", &s[2..]).into()
}

/// P4A-R2-3: each adapter check refuses its own edit of a real block (26,225,269: blob
/// transactions, a contract creation, logs), including the derived receipt fields, the logs
/// under the receipts root, JSON quirks, and the header rules on a re-sealed parent.
#[test]
fn p4a_r2_every_adapter_check_refuses_its_edit() {
    let n = 26_225_269;
    let (b, r, p) = answers(n);
    assert!(eth_block(n, &b, &r, &p).is_ok());
    let why = |b: &Value, r: &Value, p: &[u8]| match eth_block(n, b, r, p) {
        Err(EthAdapterError::Answer { what, .. }) => what,
        other => panic!("not refused: {:?}", other.map(|_| ())),
    };
    let refused = |edit: &dyn Fn(&mut Value, &mut Value), want: &str| {
        let (mut b, mut r) = (b.clone(), r.clone());
        edit(&mut b, &mut r);
        let w = why(&b, &r, &p);
        assert!(w.contains(want), "want `{want}`, got `{w}`");
    };
    let rs = r.as_array().expect("receipts");
    let creation = rs
        .iter()
        .position(|x| !x["contractAddress"].is_null())
        .expect("a creation in 26,225,269");
    let call = rs
        .iter()
        .position(|x| {
            x["contractAddress"].is_null() && !x["logs"].as_array().expect("logs").is_empty()
        })
        .expect("a call with logs");
    let blob = rs
        .iter()
        .position(|x| x.get("blobGasUsed").is_some())
        .expect("a blob receipt");
    // The logs are under the receipts root: a log's data, a topic, a dropped log.
    refused(
        &|_, r| r[call]["logs"][0]["data"] = "0x01".into(),
        "receipts root",
    );
    refused(
        &|_, r| r[call]["logs"][0]["topics"][0] = hex0x(&[7; 32]).into(),
        "receipts root",
    );
    refused(
        &|_, r| {
            r[call]["logs"].as_array_mut().expect("logs").pop();
        },
        "receipts root",
    );
    // Derived receipt fields the JSON prints, against the hashed bytes.
    refused(
        &|_, r| r[call]["contractAddress"] = hex0x(&[1; 20]).into(),
        "contractAddress",
    );
    refused(
        &|_, r| r[creation]["contractAddress"] = Value::Null,
        "contractAddress",
    );
    refused(
        &|_, r| r[call]["transactionHash"] = hex0x(&[2; 32]).into(),
        "transactionHash",
    );
    refused(
        &|_, r| r[blob]["blobGasUsed"] = "0x20000".into(),
        "blobGasUsed",
    );
    // JSON quirks: a quantity with a leading zero (in a transaction and a receipt), an
    // upper-case 0X prefix. Mixed-case hex digits are the same bytes, so they are accepted.
    refused(
        &|b, _| b["transactions"][0]["nonce"] = leading_zero(&b["transactions"][0]["nonce"]),
        "minimal quantity",
    );
    refused(
        &|_, r| r[call]["cumulativeGasUsed"] = leading_zero(&r[call]["cumulativeGasUsed"]),
        "minimal quantity",
    );
    refused(
        &|b, _| {
            b["gasLimit"] = b["gasLimit"]
                .as_str()
                .expect("q")
                .replacen("0x", "0X", 1)
                .into()
        },
        "lacks 0x",
    );
    let mut upper = b.clone();
    upper["logsBloom"] = upper["logsBloom"]
        .as_str()
        .expect("bloom")
        .to_uppercase()
        .replacen("0X", "0x", 1)
        .into();
    upper["transactions"][1]["input"] = upper["transactions"][1]["input"]
        .as_str()
        .expect("input")
        .to_uppercase()
        .replacen("0X", "0x", 1)
        .into();
    assert!(
        eth_block(n, &upper, &r, &p).is_ok(),
        "mixed-case hex is the same bytes"
    );
    // A blob field on a block without them: a Shanghai-shaped header (no blob fields), re-sealed,
    // under blob transactions; and a Cancun field without Shanghai's.
    let mut shanghai = b.clone();
    for k in [
        "blobGasUsed",
        "excessBlobGas",
        "parentBeaconBlockRoot",
        "requestsHash",
    ] {
        shanghai.as_object_mut().expect("block").remove(k);
    }
    reseal(&mut shanghai);
    assert!(why(&shanghai, &r, &p).contains("blobGasPrice"));
    let mut gap = b.clone();
    gap.as_object_mut()
        .expect("block")
        .remove("withdrawalsRoot");
    assert!(why(&gap, &r, &p).contains("prefix"));
    // Header rules on a re-sealed parent: the child links to it, its hashes and roots hold, and
    // only the rule under test breaks.
    let parent_json = block_answer(n - 1);
    let child_of = |edit: &dyn Fn(&mut Value)| {
        let mut pj = parent_json.clone();
        edit(&mut pj);
        reseal(&mut pj);
        let prlp = header_rlp(&pj).expect("parent rlp");
        let mut c = b.clone();
        c["parentHash"] = pj["hash"].clone();
        reseal(&mut c);
        why(&c, &r, &prlp)
    };
    // A parent at exactly its gas target: EIP-1559 keeps its base fee, which is not the child's.
    let at_target = |pj: &mut Value| {
        let limit =
            u64::from_str_radix(&pj["gasLimit"].as_str().expect("q")[2..], 16).expect("hex");
        pj["gasUsed"] = format!("{:#x}", limit / 2).into();
    };
    assert!(child_of(&at_target).contains("base fee"));
    // A parent whose excess blob gas is one more: the child's is no longer EIP-7918's.
    let more_excess = |pj: &mut Value| {
        let x =
            u64::from_str_radix(&pj["excessBlobGas"].as_str().expect("q")[2..], 16).expect("hex");
        pj["excessBlobGas"] = format!("{:#x}", x + 1).into();
    };
    assert!(child_of(&more_excess).contains("excess blob gas"));
    // A re-sealed block whose gas used is not its receipts' last cumulative gas.
    let mut more_gas = b.clone();
    let used = u64::from_str_radix(&b["gasUsed"].as_str().expect("q")[2..], 16).expect("hex");
    more_gas["gasUsed"] = format!("{:#x}", used + 1).into();
    reseal(&mut more_gas);
    assert!(why(&more_gas, &r, &p).contains("gas used"));
}

/// P4A-R2-3: the recipe itself refuses edited bytes of a real block (the adapter is not trusted):
/// a transaction, a log inside a receipt, the header, the parent header, a dropped receipt.
#[test]
fn p4a_r2_real_block_edits_fail_in_the_recipe() {
    use eth_chain_stats::code;
    let w = Window::open(&root_path(SUBSET)).expect("subset");
    let (bytes, snap) = load_eth_range(&w, 26_225_268, 26_225_269).expect("source");
    let src = EthChainSource::try_from_slice(&bytes).expect("source");
    // The recipe's input as the host gives it, built directly: `assemble_input` would already
    // refuse edited bytes by the snapshot's content hash, and the recipe must refuse them alone.
    let params = onyx_source_eth::params_for(&snap);
    let run_native = |s: &EthChainSource| {
        eth_chain_stats::run(RecipeInput {
            abi_version: onyx_recipe_sdk::ABI_VERSION,
            source: borsh::to_vec(s).expect("source"),
            labels: vec![],
            params: params.clone(),
        })
        .map(|o| o.rows.len())
    };
    assert_eq!(run_native(&src), Ok(2));
    let fails = |f: &dyn Fn(&mut EthChainSource)| {
        let mut s = src.clone();
        f(&mut s);
        run_native(&s).err()
    };
    let last = |v: &mut Vec<u8>| {
        let i = v.len() - 1;
        v[i] ^= 1;
    };
    // The last byte of a receipt with logs is inside its last log's data (or topic).
    let with_logs = src.blocks[1]
        .receipts
        .iter()
        .position(|r| r.len() > 300)
        .expect("a receipt with logs");
    assert_eq!(
        fails(&|s| last(&mut s.blocks[1].receipts[with_logs])),
        Some(code::BAD_ROOT)
    );
    assert_eq!(
        fails(&|s| last(&mut s.blocks[1].txs[0])),
        Some(code::BAD_ROOT)
    );
    assert_eq!(
        fails(&|s| last(&mut s.blocks[0].header)),
        Some(code::BAD_HEADER)
    );
    assert_eq!(
        fails(&|s| last(&mut s.parent_header)),
        Some(code::BAD_HEADER)
    );
    assert_eq!(
        fails(&|s| {
            s.blocks[1].receipts.pop();
        }),
        Some(code::BAD_SOURCE)
    );
    // A receipt's status flipped (0x01 -> 0x80 changes the payload length, so re-encode).
    assert_eq!(
        fails(&|s| {
            let r = &mut s.blocks[1].receipts[0];
            let typed = r[0] < 0xc0;
            let body = if typed { &r[1..] } else { &r[..] };
            let items = onyx_eth_schema::rlp::list(body).expect("receipt");
            let mut p = Vec::new();
            let status = items[0].u64().expect("status");
            onyx_eth_schema::rlp::encode_uint(&mut p, u128::from(1 - status));
            for it in &items[1..] {
                match it {
                    onyx_eth_schema::rlp::Item::Bytes(b) => {
                        onyx_eth_schema::rlp::encode_bytes(&mut p, b)
                    }
                    onyx_eth_schema::rlp::Item::List(l) => {
                        onyx_eth_schema::rlp::encode_list(&mut p, l)
                    }
                }
            }
            let mut out = if typed { vec![r[0]] } else { Vec::new() };
            onyx_eth_schema::rlp::encode_list(&mut out, &p);
            *r = out;
        }),
        Some(code::BAD_ROOT)
    );
}
