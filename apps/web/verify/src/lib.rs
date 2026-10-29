//! Browser verifier (ADR-0020). Every export calls the function the network
//! itself runs: the canonical wasmi engine, the positional Merkle tree, the
//! EIP-191 and BIP-322 attestation checks, the header proof-of-work check and
//! the Brier term. Nothing is reimplemented in JavaScript. Values cross the
//! boundary as bytes, hex strings and JSON text so the page stays thin.

use borsh::BorshDeserialize;
use onyx_btc_schema::{BtcSource, EntityBlockFlowRow, EntityFlowRow, SERIES_NO_ACTIVITY};
use onyx_core::{
    keccak256, AddressProof, Confidence, EntityKind, Hash32, ProofOutcome, RuntimeProfile,
    StaticRegistry,
};
use onyx_labels::attest::{eip191_recover, AttestContext, Attestation};
use onyx_labels::LabelSet;
use onyx_merkle::{MerkleTree, Proof};
use onyx_recipe_host::{failure_leaf, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::{node, Bip322Proof, BtcAddressCanon};
use serde_json::json;
use wasm_bindgen::prelude::*;

/// Sealed reveal posts (ROADMAP C11): the browser's mirrors and check.
pub mod sealed;

/// The on-chain window (ADR-0043): `btc-chain-stats` leaves and a rerun.
pub mod chain;

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn bytes(s: &str) -> Result<Vec<u8>, JsError> {
    hex::decode(s.trim_start_matches("0x")).map_err(|e| JsError::new(&format!("bad hex: {e}")))
}

fn h32(s: &str) -> Result<Hash32, JsError> {
    let b = bytes(s)?;
    let a: [u8; 32] = b
        .try_into()
        .map_err(|_| JsError::new("expected 32 bytes"))?;
    Ok(Hash32(a))
}

/// Byte order Bitcoin tools display hashes in.
fn display(h: &[u8; 32]) -> String {
    let mut r = *h;
    r.reverse();
    hex::encode(r)
}

/// Runtime profile of the demo shards, as `examples/export-demo.rs` pins it.
/// Fixed here, not read from the data the page checks.
fn demo_profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

pub mod track_view;

/// Level-3 signed facts (ROADMAP C61): the client's own check of a `/v1/signed` body.
pub mod signed_view;

/// The Ethereum window (ADR-0044, ROADMAP P4a): `eth-chain-stats` leaves and a rerun.
pub mod eth_chain;
// ROADMAP D30 (PP3): a bought salted label row, checked against the page's pin.
pub mod salted_view;
// NS11: the live futures markets on Base Sepolia, read through the reader's RPC (ADR-0042 amendment 2).
pub mod markets_view;

/// Demo anchors. Until M6 writes roots to Base Sepolia, these constants stand
/// in for the chain: they are compiled into the verifier, so editing the data
/// files next to the page cannot move them (website review W1, W2).
pub mod pinned {
    /// Recipe the demo shard pins (`recipes/out/btc_entity_balance.wasm.keccak`).
    pub const RECIPE_ID: &str =
        "0xfdc80da278a1987a2e9a2a9df7eb556e0b2e3cc45bcfa05654344f0fd9036176";
    /// Keccak of the full recipe input for blocks 921,080-921,082 with test
    /// label set v2, 9,850 company labels
    /// (`fixtures/golden/btc-entity-balance-921080-921082.labels-v2.json`).
    pub const INPUT_KECCAK: &str =
        "0xcf6b11bc2fb9048f81a459ab8bacd16db32a07cba23d6a7cab0adf110996ba06";
    /// Root the canonical engine gives on that input (same golden file).
    pub const ROOT: &str = "0xec47330dc84941db5ee0aacf25f3b71722cb87f77c869946b1ab201983dd2359";
    /// Live range read from the node (export-demo with `ONYX_LIVE`), first height.
    pub const LIVE_FROM: u32 = 970_543;
    /// Live range, last height.
    pub const LIVE_TO: u32 = 970_566;
    /// Display hash of block 970,566.
    pub const LIVE_TIP: &str = "00000000000000000001cc9fe0d6d88e6dad6ab00dca0b99d8018157a32a5d15";
    /// Root the canonical engine gave on the live range with label set v2
    /// (export-demo; the Python recount agrees on every row).
    pub const LIVE_ROOT: &str =
        "0x6bdf585d596df0dcd51d366da14da181a79c572c072901dd74a57a33b2ce09fb";
    /// Recipe of the per-block series (`recipes/out/btc_entity_flow_series.wasm.keccak`).
    pub const SERIES_RECIPE_ID: &str =
        "0xca406e0be671f7fa0323992b01836297c1faaa4f2c7d7088f3014bbe9e908717";
    /// Root `btc-entity-flow-series` gave on the live range with label set v2
    /// (export-demo; the Python series recount agrees on every row, and
    /// `tests/golden_series.rs` reruns it when the live blocks are present).
    pub const LIVE_SERIES_ROOT: &str =
        "0xa4a0c7fb568541fe816d1f79b003eb9ecd53f87d575bd9238c088dc03064db25";
    /// A quiet live block (no labelled company moved coins): 970,551
    /// (SER-R2-F4). Its series is the single `SERIES_NO_ACTIVITY` leaf.
    pub const QUIET_HEIGHT: u32 = 970_551;
    /// Root `btc-entity-balance` gave on block 970,551 with label set v2: four
    /// all-zero range rows (export-demo; `tests/golden_series.rs` reruns it
    /// when the live blocks are present).
    pub const QUIET_ROOT: &str =
        "0x90a128ecb5a556c963ac8a9cc027a821c7d40ce065a4ec34a1ca0a40317b9d3b";
    /// Root of the single `SERIES_NO_ACTIVITY` failure leaf: the series root of
    /// any quiet range.
    pub const QUIET_SERIES_ROOT: &str =
        "0xc2ec55cf2363afeb83b050b364f8a02604d80bc9649aa87adcc173455f1555ec";
    /// Content hash of test label set v2 (9,850 company labels), the set the
    /// history shards run with (`fixtures/labels/test.onyx.entities.btc.v2.json`).
    pub const LABEL_SET_V2: &str =
        "0x372403dd0a10895830effc62c0b56e8de2801bad896d8e27cae0a1b2a4c915ea";
    /// One-week history: first height of the history read from the node
    /// (`examples/export-history.rs`; `tests/golden_history.rs` reruns it when
    /// the fetched blocks are present).
    pub const HISTORY_FROM: u32 = 969_839;
    /// Last height of the history.
    pub const HISTORY_TO: u32 = 970_846;
    /// Display hash of block 970,846, the history's tip.
    pub const HISTORY_TIP: &str =
        "000000000000000000011f4861feb9ad646c8dd6d8458d33ecccf344c8d44302";
    /// `onyx_source_btc::history::history_commitment` over the history's
    /// shards: both recipe ids, the label set, the tip, and each shard's range
    /// and roots. Editing any row, root, header or shard boundary moves it.
    pub const HISTORY_COMMITMENT: &str =
        "0xbda6327d2189e24b9927e1c91a16d51822f78e2250975898e7d35c71d1d487cc";
    /// Mining-pool block counts over the one-week history (ADR-0032): the
    /// `btc-pool-blocks` recipe id, the Keccak of the pinned pool table
    /// (the shards' params, from the MIT `bitcoin-data/mining-pools` list),
    /// and `onyx_source_btc::pools::pools_commitment` over one shard per UTC
    /// day of the history (`examples/export-pools.rs`; `tests/golden_pools.rs`
    /// reruns every day when the week's blocks are present). The range and
    /// tip are the history's (`HISTORY_*`).
    pub const POOLS_RECIPE_ID: &str =
        "0x1af9af156198c75b7ce1e8d648725be968065a37f7c104bc3daa9320cb0b538a";
    /// Keccak-256 of the pool table bytes.
    pub const POOLS_TABLE: &str =
        "0x9c84add53ed8ac3933948ef81455fcd24c5dad5ba8ffb5dcf72d2490945912cc";
    /// The pool history commitment.
    pub const POOLS_COMMITMENT: &str =
        "0x8421123c0290c9a41205415489f6f4da945cdceedc2681dfb81a4ca6656d8e4c";
    /// On-chain window (ADR-0043): `btc-chain-stats` run as one shard per
    /// block over the week and its tail, and
    /// `onyx_source_btc::chain::chain_stats_commitment` over every shard
    /// (`examples/export-chain-stats.rs`, rerun by `tests/golden_chain.rs`
    /// when the blocks are present; `scripts/recount-chain-stats.py` agrees
    /// on every field). Public chain data only: no label set.
    pub const CHAIN_RECIPE_ID: &str =
        "0xc14f0ea9cd979ca267e75683b498fc7fcb00cb506052f9ebfedebd557dd1255f";
    /// First height of the window.
    pub const CHAIN_FROM: u64 = 969_840;
    /// Last height of the window.
    pub const CHAIN_TO: u64 = 971_199;
    /// Display hash of the window's last block.
    pub const CHAIN_TIP: &str = "0000000000000000000040018970c37de87ac02d99e424a5f8e45064301cb536";
    /// The window's commitment.
    pub const CHAIN_COMMITMENT: &str =
        "0xfebe23c490fb33ab3a69ecc194fb2e4a7221f0c3544ad0bbaa32cc6fcc137a70";
    /// The block whose full input the page reruns (the window's smallest with at least 1,000 transactions).
    pub const CHAIN_RERUN_HEIGHT: u64 = 970_835;
    /// Keccak-256 of that recipe input (`static/onchain/rerun-input.b64.txt`).
    pub const CHAIN_RERUN_INPUT: &str =
        "0xffed97c12f78e741fcab9a8f137a783adc5d8437ce7e7c67f1743c97c08eb417";
    /// Reserve history over the week (ADR-0034): the `btc-entity-reserve`
    /// recipe id, the committed UTXO scan it lands on
    /// (`fixtures/reserves/scan-971199.json`: its hash, height and block), and
    /// `onyx_source_btc::reserve::reserve_commitment` over the base, the scan's
    /// entity totals and every shard (`examples/export-reserves.rs`;
    /// `tests/golden_reserve.rs` reruns the chain when the blocks are present).
    pub const RESERVES_RECIPE_ID: &str =
        "0xd0c1331f82f9a743548ca68de3693a2754ae35e6faef7e047a739ba939b49e34";
    /// Hash of the committed scan snapshot (`ScanSnapshot::hash`).
    pub const RESERVES_SNAPSHOT: &str =
        "0x01fd0e6a76c9bcdf722a498cbea2945a24ffd2227ab9991aab9523c84afad87b";
    /// Height the scan read.
    pub const RESERVES_SCAN_HEIGHT: u64 = 971_199;
    /// Display hash of block 971,199.
    pub const RESERVES_SCAN_BLOCK: &str =
        "0000000000000000000040018970c37de87ac02d99e424a5f8e45064301cb536";
    /// The reserve history commitment.
    pub const RESERVES_COMMITMENT: &str =
        "0x03ef38cde4f543eb1ed230bff10ced0d819634e8d3c23767876ec064893ac00b";
    /// Result hash of the recorded level-2 round on U.S. Treasury Fiscal Data
    /// (Debt to the Penny, 2026-10-08): `adapters/source-fiscaldata`, set from
    /// Onyx's own run. The browser recomputes it (L2SRC-R1-F3) and requires
    /// this value. Pinned in every build: a free public source's value ships
    /// whole (THREATS NS5-8, ADR-0048 amendment 1).
    pub const LEVEL2_RESULT: Option<&str> =
        Some("0xf65720c7a4c2358f3c8d8047066846994e117bc0daa307e29009ca2da3f2cec9");
    /// Spec id of that round's feed (record date 2026-10-08, its window).
    pub const LEVEL2_SPEC_ID: &str =
        "0x0cef170f5595649bfc63fa3a01be5f23d55012d31098f03cff92a76b4850ce24";
    /// Keccak-256 of the recorded response every observer saw
    /// (`fixtures/debt-to-penny-2026-10-08/response.json`).
    pub const LEVEL2_RESPONSE_KECCAK: &str =
        "0xf5dd7a5c490fe1584511f4629bb503fdb1a770af9c1b4c13fe07497c6d45bfc4";
    /// The daily sources' recorded rounds (NS3), set from Onyx's own
    /// rehearsal on a local anvil (`ONYX_L2_ONLY=daily
    /// scripts/level2-rehearsal.sh`): spec id, Keccak-256 of the recorded
    /// response, result hash (pinned in every build: free public sources,
    /// THREATS NS5-8).
    pub const USGS_SPEC_ID: &str =
        "0xd262eb2dbd9d7faada658fd9946b7246d7d18421a7249241810d8ea197c0f4ae";
    /// `adapters/source-usgs/fixtures/quakes-m5-2026-07-08/response.txt`.
    pub const USGS_RESPONSE_KECCAK: &str =
        "0xd2f8f61201b2b11a78d6e866abc9c3db2ae8631fa656bfe5cb53668255367afb";
    /// The USGS round's result hash.
    pub const USGS_RESULT: Option<&str> =
        Some("0x0b3ee5b15558f76d4eb6f494cf110a52fa81bb846fde58b9f5fba4f2c7c92cdc");
    /// The NCEI round's spec id.
    pub const NCEI_SPEC_ID: &str =
        "0xee5341cc3fab4bf5c295522155b75061e8e48985aae57134c67bf094480eeaa4";
    /// `adapters/source-ncei/fixtures/tmax-usw00003017-2026-09-01/response.json`.
    pub const NCEI_RESPONSE_KECCAK: &str =
        "0xc3090a97d656543d9f3c2cc71b40a1c27fce27a533511d535d2d501c3252a38f";
    /// The NCEI round's result hash.
    pub const NCEI_RESULT: Option<&str> =
        Some("0x3fe00cfc9220cfb4ab22a113cdf43d5306f4404d11eb58316611d163c56434a5");
    /// The recorded SEC EDGAR level-2 rounds (`adapters/source-edgar`,
    /// period 2026-03-31), set from Onyx's own run, one per company in
    /// `onyx_source_edgar::COMPANIES` order: (company key, spec id, Keccak-256
    /// of the recorded response, result hash). The browser recomputes the
    /// spec id, response hash and result hash and requires these. The result
    /// hash is `None` in a public build (SIG-R1-F2, [`result_pin`]).
    pub const EDGAR_ROUNDS: [(&str, &str, &str, Option<&str>); 2] = [
        (
            "mstr",
            "0x61d97bf6540fcc2b349c31a9fd60a2c33fa97453a3a9bdcd2f0d68168d3502f4",
            "0x7f9bfd10b5897c37b75efaabb9278a5f30d97160fbdaa3180ae3e2a2a09e8bad",
            super::result_pin("0x0011ce22675f820c7bf8460db0aa63e93be5595690fc7fd5d9723b4b7fd8d959"),
        ),
        (
            "riot",
            "0xbd3ecfe123b55dab9d21394d33dfe01b18cb5f90b6ed8de99622be3397c9560d",
            "0xbac3e181b65746eb1d3c9840f317c7075570317de17cb8c8ed79c92c56cbfd26",
            super::result_pin("0x2ac71477b46cc418a992c8e308bea4c8b244e5937442bcf1f75283fce055c320"),
        ),
    ];
}

/// A sold level-2 value's result pin (SIG-R1-F2): the result hash is an
/// unsalted function of the value, so a public build (`--features public`,
/// `ONYX_PUBLIC=1 scripts/build-web.sh`) holds none; evaluated at compile
/// time, the hex never reaches its WebAssembly. Free public sources are pinned
/// in every build (THREATS NS5-8).
const fn result_pin(h: &'static str) -> Option<&'static str> {
    if cfg!(feature = "public") {
        None
    } else {
        Some(h)
    }
}

/// The result hash a level-2 card shows, checked against its pin. Sealed
/// (SIG-R1-F2) a round carries no result hash and none is shown: the hash
/// gives the value away by enumeration. Unsealed, the recomputed hash must
/// equal the summary's and the pin, and a verifier without the pin (a public
/// build) refuses it.
fn sealed_or_result(
    sealed: bool,
    r: &serde_json::Value,
    pin: Option<&str>,
    recompute: impl FnOnce() -> Hash32,
    problems: &mut Vec<String>,
) -> String {
    if sealed {
        if r.get("result_hash").is_some() {
            problems.push("a sealed round ships its result hash".into());
        }
        return String::new();
    }
    let result = hex0x(&recompute().0);
    match pin {
        None => problems.push("this verifier holds no result pins (a public build)".into()),
        Some(p) if p != result || r["result_hash"].as_str() != Some(p) => {
            problems.push("the result hash does not recompute to the pinned one".into());
        }
        Some(_) => {}
    }
    result
}

/// The pinned demo anchors and the header difficulty floor, as JSON.
#[wasm_bindgen]
pub fn pinned_json() -> String {
    json!({
        "recipe_id": pinned::RECIPE_ID,
        "input_keccak": pinned::INPUT_KECCAK,
        "root": pinned::ROOT,
        "label_set": pinned::LABEL_SET_V2,
        "live": {
            "from": pinned::LIVE_FROM,
            "to": pinned::LIVE_TO,
            "tip": pinned::LIVE_TIP,
            "root": pinned::LIVE_ROOT,
            "series_root": pinned::LIVE_SERIES_ROOT,
            "series_recipe_id": pinned::SERIES_RECIPE_ID,
        },
        "history": {
            "from": pinned::HISTORY_FROM,
            "to": pinned::HISTORY_TO,
            "tip": pinned::HISTORY_TIP,
            "commitment": pinned::HISTORY_COMMITMENT,
        },
        "reserves": {
            "recipe_id": pinned::RESERVES_RECIPE_ID,
            "snapshot": pinned::RESERVES_SNAPSHOT,
            "scan_height": pinned::RESERVES_SCAN_HEIGHT,
            "scan_block": pinned::RESERVES_SCAN_BLOCK,
            "commitment": pinned::RESERVES_COMMITMENT,
        },
        "chain": {
            "recipe_id": pinned::CHAIN_RECIPE_ID,
            "from": pinned::CHAIN_FROM,
            "to": pinned::CHAIN_TO,
            "tip": pinned::CHAIN_TIP,
            "commitment": pinned::CHAIN_COMMITMENT,
            "rerun_height": pinned::CHAIN_RERUN_HEIGHT,
        },
        "pools": {
            "recipe_id": pinned::POOLS_RECIPE_ID,
            "table": pinned::POOLS_TABLE,
            "commitment": pinned::POOLS_COMMITMENT,
        },
        "min_bits": node::MAINNET_MIN_BITS,
    })
    .to_string()
}

/// Decode a recipe input: the blocks (height, hash, transaction count) and
/// the label set, from the bytes themselves. The page shows these instead of
/// the export's own fields (W1, W7). A label naming anything other than an
/// `entity:` id is refused: labels name companies only.
#[wasm_bindgen]
pub fn inspect_input(input: &[u8]) -> Result<String, JsError> {
    let i = RecipeInput::try_from_slice(input).map_err(|e| JsError::new(&e.to_string()))?;
    let src = BtcSource::try_from_slice(&i.source).map_err(|e| JsError::new(&e.to_string()))?;
    let set = LabelSet::try_from_slice(&i.labels).map_err(|e| JsError::new(&e.to_string()))?;
    if let Some(l) = set.labels.iter().find(|l| !l.entity.starts_with("entity:")) {
        return Err(JsError::new(&format!(
            "label {} does not name an entity",
            l.address
        )));
    }
    let blocks: Vec<_> = src
        .blocks
        .iter()
        .map(|b| json!({ "height": b.height, "hash": hex::encode(b.hash), "txs": b.txs.len() }))
        .collect();
    let labels: Vec<_> = set
        .labels
        .iter()
        .map(|l| {
            json!({
                "address": l.address,
                "entity": l.entity,
                "kind": format!("{:?}", l.kind),
                "confidence_ppm": l.confidence.0,
            })
        })
        .collect();
    Ok(json!({
        "input_keccak": hex0x(&keccak256(input).0),
        "source_hash": hex0x(&keccak256(&i.source).0),
        "chain": src.chain,
        "blocks": blocks,
        "label_set": {
            "name": set.name,
            "version": set.version,
            "content_hash": hex0x(&set.content_hash().0),
            "labels": labels,
        },
    })
    .to_string())
}

/// Canonical host id this verifier runs.
#[wasm_bindgen]
pub fn host_id() -> String {
    HOST_ID.into()
}

/// Minimum header difficulty (`bits`) pinned in the network code
/// (`onyx_source_btc::node::MAINNET_MIN_BITS`).
#[wasm_bindgen]
pub fn pinned_min_bits() -> u32 {
    node::MAINNET_MIN_BITS
}

/// Keccak-256 as 0x-hex.
#[wasm_bindgen]
pub fn keccak(data: &[u8]) -> String {
    hex0x(&keccak256(data).0)
}

/// Content hash of the source bytes inside a recipe input (Borsh `RecipeInput`):
/// what the shard's snapshot pins.
#[wasm_bindgen]
pub fn source_hash(input: &[u8]) -> Result<String, JsError> {
    let i = RecipeInput::try_from_slice(input).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(hex0x(&keccak256(&i.source).0))
}

fn flow_json(leaf: &[u8]) -> serde_json::Value {
    let row = OutputRow::try_from_slice(leaf)
        .ok()
        .and_then(|r| EntityFlowRow::try_from_slice(&r.value).ok());
    match row {
        Some(r) => json!({
            "leaf": hex0x(leaf),
            "entity": r.entity,
            "from_height": r.from_height,
            "to_height": r.to_height,
            "received_sats": r.received_sats.to_string(),
            "sent_sats": r.sent_sats.to_string(),
            "net_sats": r.net_sats.to_string(),
            "tx_count": r.tx_count,
        }),
        None => json!({ "leaf": hex0x(leaf) }),
    }
}

fn flow_row(leaf: &[u8]) -> Result<EntityFlowRow, String> {
    let row = OutputRow::try_from_slice(leaf).map_err(|e| format!("bad row: {e}"))?;
    EntityFlowRow::try_from_slice(&row.value).map_err(|e| format!("bad flow row: {e}"))
}

/// Per-block rows against the range rows (website /data). The range rows must
/// give `root` and cover `from..=to`; there must be one row set per height in
/// order, each row covering only its own block and with `net = received -
/// sent`; and for every entity the blocks must add up to its range row
/// (received, sent, net and tx count). Returns the decoded per-block values,
/// so the page draws only numbers the verifier read, and every problem found.
fn block_sums(
    range_rows: &[String],
    blocks: &[(u64, Vec<String>)],
    root: &str,
    from: u64,
    to: u64,
) -> Result<serde_json::Value, JsError> {
    let mut problems: Vec<String> = Vec::new();
    let leaves = range_rows
        .iter()
        .map(|r| bytes(r))
        .collect::<Result<Vec<_>, _>>()?;
    let got_root = hex0x(&MerkleTree::from_rows(&leaves).root().0);
    let root_ok = got_root == root;
    if !root_ok {
        problems.push("range rows do not give the pinned root".into());
    }
    let mut range = std::collections::BTreeMap::new();
    for l in &leaves {
        let r = flow_row(l).map_err(|e| JsError::new(&e))?;
        if r.from_height != from || r.to_height != to {
            problems.push(format!("{}: range row is not {from}-{to}", r.entity));
        }
        range.insert(r.entity.clone(), r);
    }
    let heights: Vec<u64> = blocks.iter().map(|(h, _)| *h).collect();
    if heights != (from..=to).collect::<Vec<_>>() {
        problems.push(format!("blocks are not exactly {from}-{to}"));
    }
    // entity -> (received, sent, net, txs) summed, and the per-block values.
    let mut sums: std::collections::BTreeMap<String, (u128, u128, i128, u64)> =
        std::collections::BTreeMap::new();
    let mut per_block = Vec::new();
    for (h, rows) in blocks {
        let mut vals = Vec::new();
        for leaf in rows {
            let r = flow_row(&bytes(leaf)?).map_err(|e| JsError::new(&e))?;
            if r.from_height != *h || r.to_height != *h {
                problems.push(format!(
                    "block {h}: a row covers {}-{}",
                    r.from_height, r.to_height
                ));
            }
            if i128::try_from(r.received_sats)
                .ok()
                .zip(i128::try_from(r.sent_sats).ok())
                .map(|(a, b)| a - b)
                != Some(r.net_sats)
            {
                problems.push(format!(
                    "block {h}: {} net is not received - sent",
                    r.entity
                ));
            }
            let e = sums.entry(r.entity.clone()).or_default();
            e.0 = e.0.saturating_add(r.received_sats);
            e.1 = e.1.saturating_add(r.sent_sats);
            e.2 = e.2.saturating_add(r.net_sats);
            e.3 = e.3.saturating_add(r.tx_count);
            vals.push(
                json!({"entity": r.entity, "received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(), "net_sats": r.net_sats.to_string(),
                "tx_count": r.tx_count}),
            );
        }
        per_block.push(json!({"height": h, "rows": vals}));
    }
    let mut entities = Vec::new();
    for (name, r) in &range {
        let s = sums.get(name).copied().unwrap_or_default();
        let checks = [
            ("received", r.received_sats as i128, s.0 as i128),
            ("sent", r.sent_sats as i128, s.1 as i128),
            ("net", r.net_sats, s.2),
            ("tx count", i128::from(r.tx_count), i128::from(s.3)),
        ];
        for (what, want, got) in checks {
            if want != got {
                problems.push(format!(
                    "{name}: blocks add up to {got} {what}, range row says {want}"
                ));
            }
        }
        entities.push(
            json!({"entity": name, "received_sats": r.received_sats.to_string(),
            "sent_sats": r.sent_sats.to_string(), "net_sats": r.net_sats.to_string(),
            "tx_count": r.tx_count}),
        );
    }
    for name in sums.keys().filter(|n| !range.contains_key(*n)) {
        problems.push(format!("{name}: in the blocks but not in the range rows"));
    }
    Ok(
        json!({"ok": problems.is_empty(), "root": got_root, "root_ok": root_ok,
        "entities": entities, "blocks": per_block, "problems": problems}),
    )
}

/// [`block_sums`] for the live range, against the pinned live root and
/// heights. `blocks_json` is `[{"height": h, "rows": [leaf hex, ...]}, ...]`.
#[wasm_bindgen]
pub fn check_block_sums(range_rows: Vec<String>, blocks_json: &str) -> Result<String, JsError> {
    let v: serde_json::Value =
        serde_json::from_str(blocks_json).map_err(|e| JsError::new(&format!("bad blocks: {e}")))?;
    let blocks = v
        .as_array()
        .ok_or_else(|| JsError::new("blocks must be a list"))?
        .iter()
        .map(|b| {
            let h = b["height"]
                .as_u64()
                .ok_or_else(|| JsError::new("block without height"))?;
            let rows = b["rows"]
                .as_array()
                .ok_or_else(|| JsError::new("block without rows"))?
                .iter()
                .map(|r| {
                    r.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| JsError::new("row is not hex"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((h, rows))
        })
        .collect::<Result<Vec<_>, JsError>>()?;
    Ok(block_sums(
        &range_rows,
        &blocks,
        pinned::LIVE_ROOT,
        u64::from(pinned::LIVE_FROM),
        u64::from(pinned::LIVE_TO),
    )?
    .to_string())
}

fn series_row(leaf: &[u8]) -> Result<(Vec<u8>, EntityBlockFlowRow), String> {
    let row = OutputRow::try_from_slice(leaf).map_err(|e| format!("bad row: {e}"))?;
    let v = EntityBlockFlowRow::try_from_slice(&row.value)
        .map_err(|e| format!("bad series row: {e}"))?;
    Ok((row.key, v))
}

/// Series rows (`btc-entity-flow-series`) against the range rows (website
/// /data, "Flows over time"). The range rows must give `range_root` and the
/// series rows `series_root`; each series row must carry the key its fields
/// imply, keys strictly ascending, heights inside `from..=to`, net equal to
/// received minus sent and a non-zero transaction count (R-SERIES-1); and for every entity
/// the rows must add up to its range row (R-SERIES-2). A series whose only leaf
/// is the `SERIES_NO_ACTIVITY` failure leaf is a range with no activity: it has
/// no rows, so every range row must be all zero (`no_activity` in the result,
/// SER-R1-F1). Returns the decoded points per entity, so the chart draws only
/// numbers the verifier read, and every problem found. Per-entity sums cannot
/// see flow moved between two blocks of one entity: only the pinned series
/// root (or a rerun) catches that (SER-R1-F3).
fn series_sums(
    range_rows: &[String],
    series_rows: &[String],
    range_root: &str,
    series_root: &str,
    from: u64,
    to: u64,
) -> Result<serde_json::Value, JsError> {
    let mut problems: Vec<String> = Vec::new();
    let leaves = |rows: &[String]| -> Result<Vec<Vec<u8>>, JsError> {
        rows.iter().map(|r| bytes(r)).collect()
    };
    let range_leaves = leaves(range_rows)?;
    let series_leaves = leaves(series_rows)?;
    let got_range = hex0x(&MerkleTree::from_rows(&range_leaves).root().0);
    let got_series = hex0x(&MerkleTree::from_rows(&series_leaves).root().0);
    let no_activity = series_leaves == [failure_leaf(Outcome::RecipeError(SERIES_NO_ACTIVITY))];
    let rows: &[Vec<u8>] = if no_activity { &[] } else { &series_leaves };
    let range_root_ok = got_range == range_root;
    let series_root_ok = got_series == series_root;
    if !range_root_ok {
        problems.push("range rows do not give the pinned root".into());
    }
    if !series_root_ok {
        problems.push("series rows do not give the pinned series root".into());
    }
    let mut range = std::collections::BTreeMap::new();
    for l in &range_leaves {
        let r = flow_row(l).map_err(|e| JsError::new(&e))?;
        if r.from_height != from || r.to_height != to {
            problems.push(format!("{}: range row is not {from}-{to}", r.entity));
        }
        range.insert(r.entity.clone(), r);
    }
    let mut sums: std::collections::BTreeMap<String, (u128, u128, i128, u64)> =
        std::collections::BTreeMap::new();
    let mut points: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    let mut last_key: Option<Vec<u8>> = None;
    for l in rows {
        let (key, r) = series_row(l).map_err(|e| JsError::new(&e))?;
        if key != EntityBlockFlowRow::key(&r.entity, r.height) {
            problems.push(format!(
                "{} {}: key does not match the row",
                r.entity, r.height
            ));
        }
        if last_key.as_ref().is_some_and(|k| *k >= key) {
            problems.push(format!("{} {}: rows out of order", r.entity, r.height));
        }
        last_key = Some(key);
        if !(from..=to).contains(&r.height) {
            problems.push(format!(
                "{}: height {} outside {from}-{to}",
                r.entity, r.height
            ));
        }
        if i128::try_from(r.received_sats)
            .ok()
            .zip(i128::try_from(r.sent_sats).ok())
            .map(|(a, b)| a - b)
            != Some(r.net_sats)
        {
            problems.push(format!(
                "{} {}: net is not received - sent",
                r.entity, r.height
            ));
        }
        if r.tx_count == 0 {
            problems.push(format!(
                "{} {}: a row with no transactions",
                r.entity, r.height
            ));
        }
        let e = sums.entry(r.entity.clone()).or_default();
        e.0 = e.0.saturating_add(r.received_sats);
        e.1 = e.1.saturating_add(r.sent_sats);
        e.2 = e.2.saturating_add(r.net_sats);
        e.3 = e.3.saturating_add(r.tx_count);
        points.entry(r.entity.clone()).or_default().push(
            json!({"height": r.height, "received_sats": r.received_sats.to_string(),
            "sent_sats": r.sent_sats.to_string(), "net_sats": r.net_sats.to_string(),
            "tx_count": r.tx_count}),
        );
    }
    let mut entities = Vec::new();
    for (name, r) in &range {
        let s = sums.get(name).copied().unwrap_or_default();
        let checks = [
            ("received", r.received_sats as i128, s.0 as i128),
            ("sent", r.sent_sats as i128, s.1 as i128),
            ("net", r.net_sats, s.2),
            ("tx count", i128::from(r.tx_count), i128::from(s.3)),
        ];
        for (what, want, got) in checks {
            if want != got {
                problems.push(format!(
                    "{name}: series sums to {got} {what}, range row says {want}"
                ));
            }
        }
        entities.push(json!({"entity": name,
            "total": {"received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(), "net_sats": r.net_sats.to_string(),
                "tx_count": r.tx_count},
            "points": points.remove(name).unwrap_or_default()}));
    }
    for name in points.keys() {
        problems.push(format!("{name}: in the series but not in the range rows"));
    }
    Ok(
        json!({"ok": problems.is_empty(), "no_activity": no_activity, "range_root": got_range,
        "range_root_ok": range_root_ok, "series_root": got_series,
        "series_root_ok": series_root_ok, "entities": entities, "problems": problems}),
    )
}

/// [`series_sums`] against pinned roots and heights: the live range, or the
/// pinned quiet block when the range rows name exactly that block
/// (SER-R2-F4). Either way every root compared is a pinned constant, so rows
/// that pick the quiet pins still have to hash to them.
#[wasm_bindgen]
pub fn check_series(range_rows: Vec<String>, series_rows: Vec<String>) -> Result<String, JsError> {
    let quiet = u64::from(pinned::QUIET_HEIGHT);
    let names_quiet = range_rows.first().is_some_and(|r| {
        bytes(r)
            .ok()
            .and_then(|l| flow_row(&l).ok())
            .is_some_and(|row| row.from_height == quiet && row.to_height == quiet)
    });
    let (range_root, series_root, from, to) = if names_quiet {
        (pinned::QUIET_ROOT, pinned::QUIET_SERIES_ROOT, quiet, quiet)
    } else {
        (
            pinned::LIVE_ROOT,
            pinned::LIVE_SERIES_ROOT,
            u64::from(pinned::LIVE_FROM),
            u64::from(pinned::LIVE_TO),
        )
    };
    Ok(series_sums(&range_rows, &series_rows, range_root, series_root, from, to)?.to_string())
}

/// The week's headers: proof of work at the pinned minimum difficulty, each
/// linking to the one before, the last one being the pinned tip. Returns the
/// blocks (height, time, hash), their times, and the tip in internal order.
type CheckedHeaders = (Vec<serde_json::Value>, Vec<u64>, [u8; 32]);

fn history_headers(
    doc: &serde_json::Value,
    from: u64,
    to: u64,
    problems: &mut Vec<String>,
) -> Result<CheckedHeaders, JsError> {
    let headers: Vec<String> = doc["headers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if headers.len() as u64 != to - from + 1 {
        problems.push(format!(
            "{} headers for {} blocks",
            headers.len(),
            to - from + 1
        ));
    }
    let mut blocks = Vec::new();
    let mut times = Vec::new();
    let mut prev: Option<[u8; 32]> = None;
    let mut tip = [0u8; 32];
    for (i, h) in headers.iter().enumerate() {
        let height = from + i as u64;
        let raw = bytes(h)?;
        match node::check_raw_header(&raw, node::MAINNET_MIN_BITS) {
            Ok((hash, parent)) => {
                if prev.is_some_and(|p| p != parent) {
                    problems.push(format!("block {height} does not link to the one before"));
                }
                prev = Some(hash);
                tip = hash;
                let time = raw
                    .get(68..72)
                    .and_then(|b| <[u8; 4]>::try_from(b).ok())
                    .map(u32::from_le_bytes)
                    .unwrap_or_default();
                times.push(u64::from(time));
                blocks.push(json!({"height": height, "time": time, "hash": display(&hash)}));
            }
            Err(e) => {
                times.push(0);
                problems.push(format!("block {height}: {e}"));
            }
        }
    }
    if display(&tip) != pinned::HISTORY_TIP {
        problems.push("the last header is not the pinned tip".into());
    }
    Ok((blocks, times, tip))
}

/// One-week history, the index (website /data): `examples/export-history.rs`
/// writes an index with every header, every shard's range and claimed roots,
/// the days, and per-day six-block buckets for the 7-day chart. What runs here:
/// - every header (proof of work, links, the pinned tip);
/// - the shards: contiguous, covering exactly the pinned heights;
/// - the commitment over the pinned recipe ids, the pinned label set, the tip
///   and every shard's range and claimed roots, against the pinned one, so the
///   claimed roots are the pinned ones;
/// - the days: contiguous, on shard boundaries, each dated by its first
///   block's header time; the buckets: contiguous six-block runs from each
///   day's first block.
///
/// The buckets' numbers are NOT checked here: [`history_day_check`] checks a
/// day's buckets against that day's leaves (the page says which days it has
/// checked). The recipe itself is not rerun in the browser.
fn history_index_check(index: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_source_btc::history::{history_commitment, utc_date, HistoryShard, BUCKET_BLOCKS};
    let mut problems: Vec<String> = Vec::new();
    let from = u64::from(pinned::HISTORY_FROM);
    let to = u64::from(pinned::HISTORY_TO);
    let field = |k: &str| index[k].as_str().unwrap_or_default().to_string();
    for (k, want) in [
        ("series_recipe_id", pinned::SERIES_RECIPE_ID),
        ("balance_recipe_id", pinned::RECIPE_ID),
    ] {
        if field(k) != want {
            problems.push(format!("{k} is not the pinned recipe"));
        }
    }
    if index["label_set"]["content_hash"].as_str() != Some(pinned::LABEL_SET_V2) {
        problems.push("label set is not the pinned v2 set".into());
    }
    let (blocks, times, tip) = history_headers(index, from, to, &mut problems)?;
    let empty = Vec::new();
    let mut next = from;
    let mut shards = Vec::new();
    let mut shard_out = Vec::new();
    for s in index["shards"].as_array().unwrap_or(&empty) {
        let (sf, st) = (
            s["from"].as_u64().unwrap_or(0),
            s["to"].as_u64().unwrap_or(0),
        );
        if sf != next || st < sf || st > to {
            problems.push(format!("shard {sf}-{st} does not continue at {next}"));
        }
        next = st.saturating_add(1);
        let series_root = h32(s["series_root"].as_str().unwrap_or_default())?;
        let balance_root = h32(s["balance_root"].as_str().unwrap_or_default())?;
        shard_out.push(json!({"from": sf, "to": st,
            "series_root": hex0x(&series_root.0), "balance_root": hex0x(&balance_root.0),
            "no_activity": s["no_activity"] == json!(true)}));
        shards.push(HistoryShard {
            from: sf,
            to: st,
            series_root,
            balance_root,
        });
    }
    if next != to + 1 {
        problems.push(format!(
            "shards end at {}, not {to}",
            next.saturating_sub(1)
        ));
    }
    let commitment = history_commitment(
        &h32(pinned::SERIES_RECIPE_ID)?,
        &h32(pinned::RECIPE_ID)?,
        &h32(pinned::LABEL_SET_V2)?,
        &tip,
        &shards,
    );
    let commitment_ok = hex0x(&commitment.0) == pinned::HISTORY_COMMITMENT;
    if !commitment_ok {
        problems.push("the shards do not give the pinned history commitment".into());
    }
    // Days on shard boundaries, dated by their first block; buckets.
    let time_of = |h: u64| -> u64 {
        h.checked_sub(from)
            .and_then(|i| times.get(i as usize))
            .copied()
            .unwrap_or_default()
    };
    // The days the headers give (`history::plan_days`' rule: a block starts a
    // new day only when its UTC date is later than the current day's; an
    // earlier-dated block stays in the current day). HIST-W1-F2: the index's
    // days must be exactly these, so a forged index cannot move a boundary.
    let mut want_days: Vec<(String, u64, u64)> = Vec::new();
    for h in from..=to {
        let date = utc_date(time_of(h));
        match want_days.last_mut() {
            Some((d, _, end)) if *d >= date => *end = h,
            _ => want_days.push((date, h, h)),
        }
    }
    let got_days: Vec<(String, u64, u64)> = index["days"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|d| {
            (
                d["date"].as_str().unwrap_or_default().to_string(),
                d["from"].as_u64().unwrap_or(0),
                d["to"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    if got_days != want_days {
        problems.push("the days are not the UTC days the headers give".into());
    }
    let mut next_day = from;
    let mut last_date = String::new();
    let mut days_out = Vec::new();
    for d in index["days"].as_array().unwrap_or(&empty) {
        let (df, dt) = (
            d["from"].as_u64().unwrap_or(0),
            d["to"].as_u64().unwrap_or(0),
        );
        let date = d["date"].as_str().unwrap_or_default().to_string();
        if df != next_day || dt < df || dt > to {
            problems.push(format!("day {date} does not continue at {next_day}"));
        }
        next_day = dt.saturating_add(1);
        if !shards.iter().any(|s| s.from == df) || !shards.iter().any(|s| s.to == dt) {
            problems.push(format!("day {date} does not start and end on shard edges"));
        }
        if utc_date(time_of(df)) != date || date <= last_date {
            problems.push(format!("day {date} is not its first block's UTC date"));
        }
        last_date = date.clone();
        if d["file"].as_str() != Some(&format!("{date}.json")) {
            problems.push(format!("day {date}: unexpected file name"));
        }
        let mut b = df;
        for k in d["buckets"].as_array().unwrap_or(&empty) {
            let (bf, bt) = (
                k["from"].as_u64().unwrap_or(0),
                k["to"].as_u64().unwrap_or(0),
            );
            if bf != b || bt != (b + BUCKET_BLOCKS - 1).min(dt) {
                problems.push(format!(
                    "day {date}: bucket {bf}-{bt} is not the next six blocks"
                ));
            }
            b = bt.saturating_add(1);
        }
        if b != dt + 1 {
            problems.push(format!("day {date}: buckets do not cover the day"));
        }
        let first_time = time_of(df);
        let last_time = time_of(dt);
        days_out.push(json!({"date": date, "from": df, "to": dt,
            "file": d["file"], "shards": d["shards"], "rows": d["rows"],
            "first_time": first_time, "last_time": last_time,
            "buckets": d["buckets"]}));
    }
    if next_day != to + 1 {
        problems.push("the days do not cover the history".into());
    }
    Ok(
        json!({"ok": problems.is_empty(), "commitment": hex0x(&commitment.0),
        "commitment_ok": commitment_ok, "from": from, "to": to,
        "tip": display(&tip), "blocks": blocks, "shards": shard_out,
        "days": days_out, "problems": problems}),
    )
}

/// One day of the one-week history: the day's leaves (`<date>.json`) against
/// the index, which must itself check out ([`history_index_check`]). What runs
/// here, on top of the index check:
/// - the day's shards are exactly the index's shards inside the day;
/// - each shard: [`series_sums`] against the index's roots, which the pinned
///   commitment binds (R-SERIES-1 and R-SERIES-2);
/// - the day's six-block buckets in the index equal the sums of the checked
///   rows (so the 7-day chart's numbers for this day are the leaves' numbers).
///
/// Returns the decoded points per entity, so the 1-day chart draws only
/// numbers read from checked leaves.
fn history_day_check(
    index: &serde_json::Value,
    expected: &str,
    day: &serde_json::Value,
) -> Result<serde_json::Value, JsError> {
    let mut problems: Vec<String> = Vec::new();
    let idx = history_index_check(index)?;
    if idx["ok"] != json!(true) {
        problems.push("the week's index does not check out".into());
    }
    let empty = Vec::new();
    // HIST-W1-F1: the file is checked for the slot the page asked about, never
    // for the date the file names itself (a day served at another day's URL).
    let date = expected.to_string();
    if day["date"].as_str() != Some(expected) {
        problems.push(format!(
            "day {date}: the file is for {}",
            day["date"].as_str().unwrap_or("no date")
        ));
    }
    let entry = idx["days"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .find(|d| d["date"].as_str() == Some(&date))
        .cloned();
    let Some(entry) = entry else {
        return Ok(json!({"ok": false, "date": date,
            "problems": [format!("day {date} is not in the index")]}));
    };
    let (df, dt) = (
        entry["from"].as_u64().unwrap_or(0),
        entry["to"].as_u64().unwrap_or(0),
    );
    if day["from"].as_u64() != Some(df) || day["to"].as_u64() != Some(dt) {
        problems.push(format!("day {date}: heights differ from the index"));
    }
    let want: Vec<&serde_json::Value> = idx["shards"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|s| (df..=dt).contains(&s["from"].as_u64().unwrap_or(0)))
        .collect();
    let got = day["shards"].as_array().unwrap_or(&empty);
    if got.len() != want.len() {
        problems.push(format!(
            "day {date}: {} shards, the index has {}",
            got.len(),
            want.len()
        ));
    }
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut points: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    let mut rows: Vec<EntityBlockFlowRow> = Vec::new();
    for (s, w) in got.iter().zip(&want) {
        let (sf, st) = (
            s["from"].as_u64().unwrap_or(0),
            s["to"].as_u64().unwrap_or(0),
        );
        if w["from"].as_u64() != Some(sf) || w["to"].as_u64() != Some(st) {
            problems.push(format!(
                "day {date}: shard {sf}-{st} is not the index's shard"
            ));
            continue;
        }
        let series = strings(&s["series_leaves"]);
        let range = strings(&s["balance_leaves"]);
        let r = series_sums(
            &range,
            &series,
            w["balance_root"].as_str().unwrap_or_default(),
            w["series_root"].as_str().unwrap_or_default(),
            sf,
            st,
        )?;
        for p in r["problems"].as_array().unwrap_or(&empty) {
            problems.push(format!(
                "shard {sf}-{st}: {}",
                p.as_str().unwrap_or_default()
            ));
        }
        for e in r["entities"].as_array().unwrap_or(&empty) {
            let name = e["entity"].as_str().unwrap_or_default().to_string();
            let list = points.entry(name).or_default();
            list.extend(e["points"].as_array().unwrap_or(&empty).iter().cloned());
        }
        if r["no_activity"] != json!(true) {
            for l in &series {
                let (_, row) = series_row(&bytes(l)?).map_err(|e| JsError::new(&e))?;
                rows.push(row);
            }
        }
    }
    // The index's buckets for this day against the checked rows; the buckets
    // returned are the ones recomputed from the rows (the page draws those).
    let mut buckets_ok = true;
    let mut checked_buckets = Vec::new();
    for k in entry["buckets"].as_array().unwrap_or(&empty) {
        let (bf, bt) = (
            k["from"].as_u64().unwrap_or(0),
            k["to"].as_u64().unwrap_or(0),
        );
        let mut sums: std::collections::BTreeMap<&str, (u128, u128, i128, u64)> =
            std::collections::BTreeMap::new();
        for r in rows.iter().filter(|r| (bf..=bt).contains(&r.height)) {
            let e = sums.entry(r.entity.as_str()).or_default();
            e.0 += r.received_sats;
            e.1 += r.sent_sats;
            e.2 += r.net_sats;
            e.3 += r.tx_count;
        }
        let want: Vec<serde_json::Value> = sums
            .iter()
            .map(|(name, v)| {
                json!({"entity": name, "received_sats": v.0.to_string(),
                "sent_sats": v.1.to_string(), "net_sats": v.2.to_string(), "tx_count": v.3})
            })
            .collect();
        checked_buckets.push(json!({"from": bf, "to": bt, "entities": want.clone()}));
        if k["entities"] != serde_json::Value::Array(want) {
            buckets_ok = false;
            problems.push(format!(
                "day {date}: bucket {bf}-{bt} is not the sum of its rows"
            ));
        }
    }
    let entities: Vec<serde_json::Value> = points
        .into_iter()
        .map(|(entity, pts)| json!({"entity": entity, "points": pts}))
        .collect();
    Ok(
        json!({"ok": problems.is_empty(), "date": date, "from": df, "to": dt,
        "rows": rows.len(), "buckets_ok": buckets_ok, "buckets": checked_buckets,
        "entities": entities, "problems": problems}),
    )
}

fn parse(text: &str) -> Result<serde_json::Value, JsError> {
    serde_json::from_str(text).map_err(|e| JsError::new(&format!("bad json: {e}")))
}

/// [`history_index_check`] on the index's JSON text.
#[wasm_bindgen]
pub fn check_history_index(index_json: &str) -> Result<String, JsError> {
    Ok(history_index_check(&parse(index_json)?)?.to_string())
}

/// [`history_day_check`] on the index's and one day's JSON text, for the day
/// `date` (the slot the page loaded the file for).
#[wasm_bindgen]
pub fn check_history_day(index_json: &str, date: &str, day_json: &str) -> Result<String, JsError> {
    Ok(history_day_check(&parse(index_json)?, date, &parse(day_json)?)?.to_string())
}

/// Check the one-week pool history export (`static/history/pools-week.json`)
/// against the pins: recipe id, pool table hash, range and tip; every day's
/// leaves rebuild that day's root, decode to `PoolBlocksRow`s on that day's
/// range that count each of its blocks exactly once; days are contiguous and
/// cover the history; and the commitment over the days' ranges and roots is
/// the pinned one. Rows drawn on the page come from the checked leaves only.
pub fn pools_check(doc: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_pool_schema::PoolBlocksRow;
    use onyx_source_btc::pools::{pools_commitment, PoolShard};
    let mut problems: Vec<String> = Vec::new();
    let hash = |v: &serde_json::Value| -> Option<[u8; 32]> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(v.as_str()?.trim_start_matches("0x"), &mut b).ok()?;
        Some(b)
    };
    if doc["recipe_id"].as_str() != Some(pinned::POOLS_RECIPE_ID) {
        problems.push("recipe id is not the pinned one".into());
    }
    if doc["table_hash"].as_str() != Some(pinned::POOLS_TABLE) {
        problems.push("pool table is not the pinned one".into());
    }
    if doc["tip"].as_str() != Some(pinned::HISTORY_TIP)
        || doc["from"].as_u64() != Some(u64::from(pinned::HISTORY_FROM))
        || doc["to"].as_u64() != Some(u64::from(pinned::HISTORY_TO))
    {
        problems.push("range or tip is not the history's".into());
    }
    let mut shards = Vec::new();
    let mut days_out = Vec::new();
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut next = u64::from(pinned::HISTORY_FROM);
    let days = doc["days"].as_array().cloned().unwrap_or_default();
    for d in &days {
        let date = d["date"].as_str().unwrap_or("?").to_string();
        let (Some(from), Some(to), Some(root)) =
            (d["from"].as_u64(), d["to"].as_u64(), hash(&d["root"]))
        else {
            problems.push(format!("day {date}: missing range or root"));
            continue;
        };
        if from != next || to < from {
            problems.push(format!("day {date}: not contiguous"));
        }
        next = to.saturating_add(1);
        let leaves: Option<Vec<Vec<u8>>> = d["leaves"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|l| {
                        l.as_str()
                            .and_then(|x| hex::decode(x.trim_start_matches("0x")).ok())
                    })
                    .collect()
            })
            .and_then(|v: Vec<Option<Vec<u8>>>| v.into_iter().collect());
        let mut ok = true;
        let mut rows = Vec::new();
        match leaves {
            Some(leaves) if !leaves.is_empty() => {
                if MerkleTree::from_rows(&leaves).root().0 != root {
                    ok = false;
                    problems.push(format!("day {date}: leaves do not rebuild its root"));
                }
                let mut count: u64 = 0;
                let mut last_key: Option<Vec<u8>> = None;
                for l in &leaves {
                    let parsed = OutputRow::try_from_slice(l).ok().and_then(|r| {
                        PoolBlocksRow::try_from_slice(&r.value)
                            .ok()
                            .map(|v| (r.key, v))
                    });
                    match parsed {
                        Some((key, v))
                            if key == v.pool.as_bytes()
                                && v.from_height == from
                                && v.to_height == to
                                && v.blocks > 0
                                && last_key.as_ref().is_none_or(|k| *k < key) =>
                        {
                            count = count.saturating_add(v.blocks);
                            last_key = Some(key);
                            rows.push(json!({"pool": v.pool, "blocks": v.blocks}));
                        }
                        _ => {
                            ok = false;
                            problems
                                .push(format!("day {date}: a row does not decode on its range"));
                        }
                    }
                }
                if count != to - from + 1 {
                    ok = false;
                    problems.push(format!(
                        "day {date}: rows count {count} blocks, the day has {}",
                        to - from + 1
                    ));
                }
            }
            _ => {
                ok = false;
                problems.push(format!("day {date}: no leaves"));
            }
        }
        if ok {
            for r in &rows {
                if let (Some(p), Some(b)) = (r["pool"].as_str(), r["blocks"].as_u64()) {
                    *totals.entry(p.to_string()).or_default() += b;
                }
            }
        }
        shards.push(PoolShard {
            from,
            to,
            root: Hash32(root),
        });
        days_out.push(json!({"date": date, "from": from, "to": to, "ok": ok, "rows": rows}));
    }
    if next != u64::from(pinned::HISTORY_TO) + 1 {
        problems.push("days do not cover the history".into());
    }
    let commitment = match (
        hash(&json!(pinned::POOLS_RECIPE_ID)),
        hash(&json!(pinned::POOLS_TABLE)),
        hash(&json!(pinned::HISTORY_TIP)),
    ) {
        (Some(r), Some(t), Some(tip)) => {
            pools_commitment(&Hash32(r), &Hash32(t), &tip, &shards).to_string()
        }
        _ => String::new(),
    };
    let commitment_ok = commitment == pinned::POOLS_COMMITMENT;
    if !commitment_ok {
        problems.push("the days' commitment is not the pinned one".into());
    }
    Ok(
        json!({"ok": problems.is_empty(), "commitment_ok": commitment_ok, "days": days_out,
        "totals": totals, "problems": problems}),
    )
}

/// [`pools_check`] on the export's JSON text.
#[wasm_bindgen]
pub fn check_pools(doc_json: &str) -> Result<String, JsError> {
    Ok(pools_check(&parse(doc_json)?)?.to_string())
}

/// Check the one-week reserve history export (`static/history/reserves-week.json`,
/// ADR-0034) against the pins: recipe id, label set v2, the scan snapshot
/// (hash, height, block) and the week's range and tip; every shard's leaves
/// rebuild its root and decode to one `EntityReserveRow` per entity; the
/// chain links (`onyx_source_btc::reserve::check_chain`: each shard on the
/// previous shard's end balances and root, the first on the base and the
/// snapshot); the week shards cover the history exactly; the chain ends at
/// the scan's block on exactly the scan's entity totals; each entity's base
/// is the one the client serving check derives from the pinned scan and the
/// shards' flows (ROADMAP D3 and D22, `reserve::derive_entity_span`, the
/// API client's code path); and the
/// commitment over base, totals and shards is the pinned one. Balances drawn
/// on the page come from checked rows only.
pub fn reserves_check(doc: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_reserve_schema::{EntityBase, EntityReserveRow};
    use onyx_source_btc::reserve::{
        check_chain, derive_entity_span, reserve_commitment, EntityLink, ReserveShard, SpanAnchor,
    };
    let mut problems: Vec<String> = Vec::new();
    let hash = |v: &serde_json::Value| -> Option<[u8; 32]> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(v.as_str()?.trim_start_matches("0x"), &mut b).ok()?;
        Some(b)
    };
    let amounts = |v: &serde_json::Value| -> Option<Vec<EntityBase>> {
        v.as_array()?
            .iter()
            .map(|b| {
                Some(EntityBase {
                    entity: b["entity"].as_str()?.to_string(),
                    balance_sats: b["balance_sats"].as_str()?.parse().ok()?,
                })
            })
            .collect()
    };
    if doc["recipe_id"].as_str() != Some(pinned::RESERVES_RECIPE_ID) {
        problems.push("recipe id is not the pinned one".into());
    }
    if doc["label_set"]["content_hash"].as_str() != Some(pinned::LABEL_SET_V2) {
        problems.push("label set is not v2".into());
    }
    let snap = &doc["snapshot"];
    if snap["hash"].as_str() != Some(pinned::RESERVES_SNAPSHOT)
        || snap["height"].as_u64() != Some(pinned::RESERVES_SCAN_HEIGHT)
        || snap["block_hash"].as_str() != Some(pinned::RESERVES_SCAN_BLOCK)
    {
        problems.push("scan snapshot is not the pinned one".into());
    }
    let (wfrom, wto) = (
        u64::from(pinned::HISTORY_FROM),
        u64::from(pinned::HISTORY_TO),
    );
    if doc["week"]["from"].as_u64() != Some(wfrom)
        || doc["week"]["to"].as_u64() != Some(wto)
        || doc["week"]["tip"].as_str() != Some(pinned::HISTORY_TIP)
    {
        problems.push("week range or tip is not the history's".into());
    }
    let base_height = wfrom - 1;
    if doc["base"]["height"].as_u64() != Some(base_height) {
        problems.push("base is not for the block before the week".into());
    }
    let base = amounts(&doc["base"]["rows"]).unwrap_or_default();
    let totals = amounts(&snap["totals"]).unwrap_or_default();
    if base.is_empty() || totals.is_empty() {
        problems.push("base or scan totals missing".into());
    }
    let mut shards: Vec<ReserveShard> = Vec::new();
    let mut week_end = None;
    for s in doc["shards"].as_array().cloned().unwrap_or_default() {
        let (Some(from), Some(to), Some(root)) =
            (s["from"].as_u64(), s["to"].as_u64(), hash(&s["root"]))
        else {
            problems.push("a shard misses its range or root".into());
            continue;
        };
        let in_week = s["week"].as_bool() == Some(true);
        if in_week != (to <= wto) || (from <= wto && to > wto) {
            problems.push(format!("{from}-{to}: crosses or mislabels the week's end"));
        }
        if to == wto {
            week_end = Some(shards.len());
        }
        let leaves: Option<Vec<Vec<u8>>> = s["leaves"].as_array().and_then(|a| {
            a.iter()
                .map(|l| {
                    l.as_str()
                        .and_then(|x| hex::decode(x.trim_start_matches("0x")).ok())
                })
                .collect()
        });
        let Some(leaves) = leaves.filter(|l| !l.is_empty()) else {
            problems.push(format!("{from}-{to}: no leaves"));
            continue;
        };
        if MerkleTree::from_rows(&leaves).root().0 != root {
            problems.push(format!("{from}-{to}: leaves do not rebuild its root"));
        }
        let rows: Option<Vec<EntityReserveRow>> = leaves
            .iter()
            .map(|l| {
                let r = OutputRow::try_from_slice(l).ok()?;
                let v = EntityReserveRow::try_from_slice(&r.value).ok()?;
                (r.key == v.entity.as_bytes()).then_some(v)
            })
            .collect();
        let Some(rows) = rows else {
            problems.push(format!("{from}-{to}: a row does not decode"));
            continue;
        };
        shards.push(ReserveShard {
            from,
            to,
            root: Hash32(root),
            rows,
        });
    }
    if week_end.is_none() {
        problems.push("no shard ends at the week's end".into());
    }
    let mut series: std::collections::BTreeMap<String, Vec<serde_json::Value>> = Default::default();
    match check_chain(
        base_height,
        &base,
        &Hash32(hash(&snap["hash"]).unwrap_or_default()),
        &shards,
    ) {
        Ok(end) => {
            if shards.last().map(|s| s.to) != Some(pinned::RESERVES_SCAN_HEIGHT) || end != totals {
                problems.push("the chain does not land on the scan".into());
            }
        }
        Err(e) => problems.push(e.to_string()),
    }
    // ROADMAP D3 (THREATS RES-10): the client's serving check, the same code
    // a distrustful API client runs (`reserve::derive_entity_span`, ROADMAP
    // D22): each entity's start base is derived from the pinned scan (its
    // height compiled in; its totals bound by the pinned commitment) and the
    // shards' flows, never read from the export, and every row's base must be
    // the derived one or the previous end. The week's set is undated (one
    // window); the first link's `base_ref` is the snapshot's (`check_chain`
    // above). The shards' tips against a header chain are ROADMAP D12.
    let mut derived_base_ok = !shards.is_empty() && base.len() == totals.len();
    let snapshot = hash(&json!(pinned::RESERVES_SNAPSHOT)).map(Hash32);
    if snapshot.is_none() {
        derived_base_ok = false;
        problems.push("the pinned scan hash does not parse".into());
    }
    for t in totals.iter().filter(|_| snapshot.is_some()) {
        let rows: Vec<Option<&EntityReserveRow>> = shards
            .iter()
            .map(|s| s.rows.iter().find(|r| r.entity == t.entity))
            .collect();
        let links: Vec<EntityLink<'_>> = shards
            .iter()
            .zip(&rows)
            .filter_map(|(s, r)| {
                r.map(|row| EntityLink {
                    from: s.from,
                    to: s.to,
                    root: s.root,
                    row,
                })
            })
            .collect();
        let derived = derive_entity_span(
            &SpanAnchor {
                scan_height: pinned::RESERVES_SCAN_HEIGHT,
                totals: &totals,
                dated: None,
            },
            &t.entity,
            &links,
        );
        let read = base.iter().find(|b| b.entity == t.entity);
        match derived {
            Ok(c)
                if links.len() == shards.len()
                    && c.start_height == base_height
                    && read.map(|b| b.balance_sats) == Some(c.start_base_sats) => {}
            Ok(_) => {
                derived_base_ok = false;
                problems.push(format!(
                    "{}: the export's base is not the one derived from the pinned scan and the shards' flows",
                    t.entity
                ));
            }
            Err(e) => {
                derived_base_ok = false;
                problems.push(format!("client serving check: {e}"));
            }
        }
    }
    for b in &base {
        series
            .entry(b.entity.clone())
            .or_default()
            .push(json!({"height": base_height, "balance_sats": b.balance_sats.to_string()}));
    }
    for s in shards.iter().take(week_end.map_or(0, |i| i + 1)) {
        for r in &s.rows {
            series.entry(r.entity.clone()).or_default().push(json!({"height": s.to,
                "balance_sats": r.balance_sats.to_string(), "received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(), "tx_count": r.tx_count}));
        }
    }
    let commitment = match (
        hash(&doc["recipe_id"]),
        hash(&json!(pinned::LABEL_SET_V2)),
        hash(&snap["hash"]),
        hash(&json!(pinned::RESERVES_SCAN_BLOCK)),
    ) {
        (Some(r), Some(l), Some(sn), Some(mut blk)) => {
            blk.reverse();
            reserve_commitment(
                &Hash32(r),
                &Hash32(l),
                &Hash32(sn),
                pinned::RESERVES_SCAN_HEIGHT,
                &blk,
                base_height,
                &base,
                &totals,
                &shards,
            )
            .to_string()
        }
        _ => String::new(),
    };
    let commitment_ok = commitment == pinned::RESERVES_COMMITMENT;
    if !commitment_ok {
        problems.push("the reserve commitment is not the pinned one".into());
    }
    let entities: Vec<serde_json::Value> = series
        .into_iter()
        .map(|(entity, points)| json!({"entity": entity, "points": points}))
        .collect();
    Ok(
        json!({"ok": problems.is_empty(), "commitment_ok": commitment_ok,
        "derived_base_ok": derived_base_ok,
        "base_height": base_height, "scan_height": pinned::RESERVES_SCAN_HEIGHT,
        "shards": shards.len(), "week_shards": week_end.map_or(0, |i| i + 1),
        "entities": entities, "scan_totals": totals.iter().map(|t| json!({"entity": t.entity,
            "balance_sats": t.balance_sats.to_string()})).collect::<Vec<_>>(),
        "problems": problems}),
    )
}

/// [`reserves_check`] on the export's JSON text.
#[wasm_bindgen]
pub fn check_reserves(doc_json: &str) -> Result<String, JsError> {
    Ok(reserves_check(&parse(doc_json)?)?.to_string())
}

/// Mirror of `onyx_coordinator::observe::round_id` (the coordinator is not
/// built for wasm32): Keccak-256 of `"onyx-observe-round-v2" ‖ spec_id ‖
/// borsh(config) ‖ borsh(observers) ‖ draw_hash`. `config` is (commit_close,
/// reveal_close, reward, outlier_slash, miss_slash); observers are
/// (address, bond), ascending; `draw_hash` is [`level2_draw_hash`]. A native
/// test pins it to the coordinator.
pub fn level2_round_id(
    spec_id: &Hash32,
    config: (u64, u64, u128, u128, u128),
    observers: &[([u8; 20], u128)],
    draw_hash: &Hash32,
) -> Hash32 {
    let mut b = b"onyx-observe-round-v2".to_vec();
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(&borsh::to_vec(&config).unwrap_or_default());
    b.extend_from_slice(&borsh::to_vec(observers).unwrap_or_default());
    b.extend_from_slice(&draw_hash.0);
    keccak256(&b)
}

/// Mirror of `onyx_coordinator::observe::pool_hash`: Keccak-256 of
/// `"onyx-observer-pool-v1" ‖ borsh(pool)`, members (address, bond).
pub fn level2_pool_hash(pool: &[([u8; 20], u128)]) -> Hash32 {
    let mut b = b"onyx-observer-pool-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(pool).unwrap_or_default());
    keccak256(&b)
}

/// Mirror of `onyx_coordinator::observe::cfg_hash`: Keccak-256 of
/// `"onyx-observe-cfg-v1" ‖ borsh(config)`, `config` as in [`level2_round_id`].
pub fn level2_cfg_hash(config: (u64, u64, u128, u128, u128)) -> Hash32 {
    let mut b = b"onyx-observe-cfg-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(&config).unwrap_or_default());
    keccak256(&b)
}

/// Mirror of `onyx_coordinator::observe::draw_commit` and
/// `OnyxRegistry.drawCommit` (L2RND-R3): Keccak-256 of
/// `"onyx-draw-commit-v1" ‖ pool_hash ‖ spec_id ‖ cfg_hash`.
pub fn level2_draw_commit(pool: &Hash32, spec_id: &Hash32, cfg_hash: &Hash32) -> Hash32 {
    let mut b = b"onyx-draw-commit-v1".to_vec();
    b.extend_from_slice(&pool.0);
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(&cfg_hash.0);
    keccak256(&b)
}

/// Mirror of `onyx_coordinator::observe::draw_hash`: Keccak-256 of
/// `"onyx-observe-draw-v1" ‖ borsh(pool_hash, beacon_height, beacon,
/// attempt)`.
pub fn level2_draw_hash(
    pool: &Hash32,
    beacon_height: u64,
    beacon: &Hash32,
    attempt: u32,
) -> Hash32 {
    let mut b = b"onyx-observe-draw-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&(pool, beacon_height, beacon, attempt)).unwrap_or_default(),
    );
    keccak256(&b)
}

/// Mirror of `onyx_coordinator::observe::draw_observers` (L2-F2): members
/// whose bond covers `min_bond` get the key Keccak-256 of
/// `"onyx-observe-draw-v1" ‖ spec_id ‖ draw_hash ‖ address`; the `size`
/// smallest keys are drawn, returned ascending by address. Empty when fewer
/// than `size` members are eligible.
pub fn level2_draw(
    spec_id: &Hash32,
    pool: &[([u8; 20], u128)],
    draw_hash: &Hash32,
    size: usize,
    min_bond: u128,
) -> Vec<([u8; 20], u128)> {
    let mut keyed: Vec<(Hash32, ([u8; 20], u128))> = pool
        .iter()
        .filter(|o| o.1 >= min_bond)
        .map(|o| {
            let mut b = b"onyx-observe-draw-v1".to_vec();
            b.extend_from_slice(&spec_id.0);
            b.extend_from_slice(&draw_hash.0);
            b.extend_from_slice(&o.0);
            (keccak256(&b), *o)
        })
        .collect();
    if size == 0 || keyed.len() < size {
        return Vec::new();
    }
    keyed.sort_unstable_by_key(|a| a.0);
    let mut drawn: Vec<([u8; 20], u128)> = keyed.into_iter().take(size).map(|(_, o)| o).collect();
    drawn.sort_by_key(|o| o.0);
    drawn
}

/// The draw part of a level-2 summary check (L2-F2): read the summary's pool
/// snapshot and draw (`"pool"`, `"draw"`), check the pool is a valid
/// snapshot (1..=1024 members, strictly ascending), recompute the draw of
/// `size` observers from it, and require `observers` to be exactly that
/// draw. Returns the draw hash (for the round id) and the pool size.
///
/// The draw must also carry the registry's record of it (`"announcement"`,
/// L2RND-R1): `OnyxRegistry.drawOf` and `drawCount` as read back after
/// `fixBeacon`. The page requires it to be the same draw (attempt, beacon
/// height, beacon, and the commitment to this pool, spec and config the page
/// recomputes, L2RND-R3), announced in a block below the beacon height, and
/// the fact's latest attempt (`draw_count == attempt + 1`). For the recorded
/// rehearsals that registry ran on a local test chain (anvil, chain 31337)
/// that no longer exists, so the page checks the record's consistency; it
/// cannot ask that chain. A client with a node asks `drawMatches` and
/// `transcriptComplete` (ADR-0027 amendment 8) itself.
fn level2_draw_check(
    r: &serde_json::Value,
    spec_id: &Hash32,
    size: usize,
    config: (u64, u64, u128, u128, u128),
    observers: &[([u8; 20], u128)],
    problems: &mut Vec<String>,
) -> Result<(Hash32, usize, u32), JsError> {
    let min_bond = config.3;
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let empty = Vec::new();
    let mut pool: Vec<([u8; 20], u128)> = Vec::new();
    for m in r["pool"].as_array().unwrap_or(&empty) {
        let a: [u8; 20] = bytes(&s(&m["observer"]))?
            .try_into()
            .map_err(|_| JsError::new("pool member is not 20 bytes"))?;
        pool.push((a, s(&m["bond"]).parse::<u128>().unwrap_or(0)));
    }
    if pool.is_empty() || pool.len() > 1_024 || !pool.windows(2).all(|w| w[0].0 < w[1].0) {
        problems
            .push("the observer pool is empty, too large, or not distinct and ascending".into());
    }
    let d = &r["draw"];
    let beacon: [u8; 32] = bytes(&s(&d["beacon"]))?
        .try_into()
        .map_err(|_| JsError::new("beacon is not 32 bytes"))?;
    let attempt = d["attempt"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n < 64)
        .unwrap_or_else(|| {
            problems.push("the draw attempt is missing or out of range".into());
            0
        });
    let beacon_height = d["beacon_height"].as_u64().unwrap_or(0);
    let pool_hash = level2_pool_hash(&pool);
    let a = &d["announcement"];
    let n = |k: &str| a[k].as_u64();
    let announced_ok = n("chain_id") == Some(31_337)
        && s(&a["registry"])
            .strip_prefix("0x")
            .is_some_and(|h| h.len() == 40 && h.bytes().all(|c| c.is_ascii_hexdigit()))
        && n("attempt") == Some(u64::from(attempt))
        && n("draw_count") == Some(u64::from(attempt) + 1)
        && n("beacon_height") == Some(beacon_height)
        && n("announced_at").is_some_and(|at| at < beacon_height)
        && s(&a["commit"])
            == hex0x(&level2_draw_commit(&pool_hash, spec_id, &level2_cfg_hash(config)).0)
        && s(&a["beacon"]) == hex0x(&beacon);
    if !announced_ok {
        problems.push(
            "the draw is not the registry's announced latest draw (announced before its beacon height, committed to this pool, spec and config, beacon fixed from the block hash)".into(),
        );
    }
    let draw_hash = level2_draw_hash(&pool_hash, beacon_height, &Hash32(beacon), attempt);
    if level2_draw(spec_id, &pool, &draw_hash, size, min_bond) != observers {
        problems.push("the observers are not the ones drawn from the pool with the beacon".into());
    }
    // The fact's draws, from the checked announcement: `draw_count` must be
    // `attempt + 1` (the round is the latest draw), so every earlier draw
    // ended without a value (SEC-R2-F1).
    Ok((draw_hash, pool.len(), attempt + 1))
}

/// Mirror of `observe::observation_commit_message` (ATT-R1-F3): what an
/// observer signs to post its commit.
pub fn level2_commit_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    commit: &Hash32,
) -> String {
    format!(
        "Onyx observe commit v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\ncommit: {commit}",
        hex::encode(observer)
    )
}

/// Mirror of `observe::observation_reveal_message` (ATT-R1-F3): what an
/// observer signs to reveal.
pub fn level2_reveal_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &[u8; 20],
    observation: &onyx_core::Observation,
    salt: &Hash32,
) -> String {
    let o = keccak256(&borsh::to_vec(observation).unwrap_or_default());
    format!(
        "Onyx observe reveal v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\nobservation: {o}\nsalt: {salt}",
        hex::encode(observer)
    )
}

/// Whether `sig_hex` is `a`'s canonical EIP-191 signature of `msg`.
fn level2_signed_by(msg: &str, sig_hex: &str, a: &[u8; 20]) -> bool {
    hex::decode(sig_hex.strip_prefix("0x").unwrap_or("-"))
        .ok()
        .and_then(|b| <[u8; 65]>::try_from(b).ok())
        .and_then(|sig| onyx_labels::attest::eip191_recover(msg.as_bytes(), &sig))
        == Some(*a)
}

/// The per-observer checks both level-2 cards share. Unsealed (`value` is
/// the settled value and the page hash): each reveal is from an assigned
/// observer, once, fetched in the window, saw the page, opens its commit, and
/// both commit and reveal carry the observer's own signature (ATT-R1-F3).
/// Sealed (`value` is `None`, a public build, ATT-R2-F1): each entry of
/// `commits` is from an assigned observer, once, with the observer's
/// signature on its commit; nothing about the answer is shipped. Returns how
/// many entries passed.
fn level2_observer_checks(
    entries: &[serde_json::Value],
    feed: &onyx_core::ObservationSpec,
    round_id: &Hash32,
    attempt: u32,
    observers: &[([u8; 20], u128)],
    value: Option<(i128, Hash32)>,
    problems: &mut Vec<String>,
) -> Result<usize, JsError> {
    use onyx_core::{observation_commit, Answer, Observation};
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let mut seen = std::collections::BTreeSet::new();
    let mut checked = 0;
    for v in entries {
        let who = s(&v["observer"]);
        let a: [u8; 20] = bytes(&who)?
            .try_into()
            .map_err(|_| JsError::new("observer is not 20 bytes"))?;
        if !observers.iter().any(|(o, _)| *o == a) || !seen.insert(a) {
            problems.push(format!(
                "observer {who} is not an assigned observer, or appears twice"
            ));
        }
        let commit = h32(&s(&v["commit"]))?;
        if !level2_signed_by(
            &level2_commit_message(round_id, attempt, &a, &commit),
            &s(&v["commit_sig"]),
            &a,
        ) {
            problems.push(format!("observer {who}'s commit is not signed by its key"));
            continue;
        }
        let Some((value, page_hash)) = value else {
            checked += 1;
            continue;
        };
        let at = v["observed_unix"].as_u64().unwrap_or(0);
        if at < feed.window_start || at > feed.window_end {
            problems.push(format!("observer {who} fetched outside the window"));
        }
        if h32(&s(&v["page_hash"]))? != page_hash {
            problems.push(format!("observer {who} saw another page"));
        }
        let o = Observation {
            answer: Answer::Number(value),
            observed_at: at,
            page_hash,
        };
        let salt = h32(&s(&v["salt"]))?;
        if observation_commit(round_id, &a, &o, &salt) != commit {
            problems.push(format!("observer {who}'s sealed commit does not recompute"));
        } else if !level2_signed_by(
            &level2_reveal_message(round_id, attempt, &a, &o, &salt),
            &s(&v["reveal_sig"]),
            &a,
        ) {
            problems.push(format!("observer {who}'s reveal is not signed by its key"));
        } else {
            checked += 1;
        }
    }
    Ok(checked)
}

/// Mirror of `ObservationSettlement::result_hash` for a round that settled
/// the numeric value `value`: Keccak-256 of `"onyx-observe-result-v1" ‖
/// borsh(spec_id, round_id, Outcome::Value(Answer::Number(value)))`.
pub fn level2_result_hash(spec_id: &Hash32, round_id: &Hash32, value: i128) -> Hash32 {
    // `ObservationOutcome::Value` is variant 0; its payload is the Answer.
    #[derive(borsh::BorshSerialize)]
    enum Outcome {
        Value(onyx_core::Answer),
    }
    let mut b = b"onyx-observe-result-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&(
            spec_id,
            round_id,
            Outcome::Value(onyx_core::Answer::Number(value)),
        ))
        .unwrap_or_default(),
    );
    keccak256(&b)
}

/// Check the recorded level-2 round summary (`static/level2/*.json`)
/// (L2SRC-R1-F3): the feed rebuilt from its record date and window is the
/// canonical feed and has the pinned spec id; the response's Keccak-256 is the
/// pinned one; the pinned fetcher, rerun on it, gives the shown value; the
/// observers are distinct, ascending and as many as the feed needs, each
/// fetched in the window, saw that response, and has a sealed commit that
/// recomputes; the round id is recomputed from the feed, the config and the
/// observers' bonds; and the result hash is recomputed from the feed, the
/// round id and the settled value and must equal the pinned one. A summary
/// rebuilt around another date, page, value or observer set fails on a pin.
/// Everything the website shows comes from this check's output, never from
/// the summary (L2SRC-R2-F3): the value, record date, subject and window from
/// the rebuilt feed, the observer and agreement counts from the checked
/// reveals, and the source's name, terms link, terms quote and attribution
/// from the adapter's constants. A summary whose own copies of those fields
/// differ also fails, so a forged link or licence never shows as checked.
/// Not rechecked: that the response is what the source served (that is what
/// several independent observers vouch for), and that the observers are
/// independent (they are simulated on one machine).
/// One level-2 source as the browser checks its recorded round: the canonical
/// feed for a date and window, the template and the one fetcher, how the
/// shown value reads, the pins set from Onyx's own run, and the constants a
/// card shows (never the summary's own copies, L2SRC-R2-F3).
struct FeedCheck<'a> {
    spec: &'a dyn Fn(&str, u64) -> Result<onyx_core::ObservationSpec, String>,
    template: onyx_core::FeedTemplate,
    fetcher: &'a dyn onyx_core::Fetcher,
    parse_shown: &'a dyn Fn(&str) -> Option<i128>,
    spec_id_pin: &'a str,
    response_pin: &'a str,
    result_pin: Option<&'a str>,
    source_name: &'a str,
    terms_url: &'a str,
    terms_quote: &'a str,
    unit: &'a str,
}

fn level2_round_check(r: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_source_fiscaldata as f;
    feed_round_check(
        &FeedCheck {
            spec: &f::spec,
            template: f::template(),
            fetcher: &f::FiscalDataFetcher,
            parse_shown: &f::parse_cents,
            spec_id_pin: pinned::LEVEL2_SPEC_ID,
            response_pin: pinned::LEVEL2_RESPONSE_KECCAK,
            result_pin: pinned::LEVEL2_RESULT,
            source_name: f::SOURCE_NAME,
            terms_url: f::TERMS_URL,
            terms_quote: f::TERMS_QUOTE,
            unit: "USD",
        },
        r,
    )
}

/// The daily sources' recorded rounds (NS3, ADR-0027 amendment 17): the same
/// checks as [`level2_round_check`], each with its own source and pins.
fn daily_round_check(source: &str, r: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_source_ncei as n;
    use onyx_source_usgs as u;
    let count = |s: &str| u::parse_count(s.as_bytes());
    let c = match source {
        "usgs" => FeedCheck {
            spec: &u::spec,
            template: u::template(),
            fetcher: &u::UsgsCountFetcher,
            parse_shown: &count,
            spec_id_pin: pinned::USGS_SPEC_ID,
            response_pin: pinned::USGS_RESPONSE_KECCAK,
            result_pin: pinned::USGS_RESULT,
            source_name: u::SOURCE_NAME,
            terms_url: u::TERMS_URL,
            terms_quote: u::TERMS_QUOTE,
            unit: "earthquakes",
        },
        "ncei" => FeedCheck {
            spec: &n::spec,
            template: n::template(),
            fetcher: &n::GhcndFetcher,
            parse_shown: &n::parse_tenths,
            spec_id_pin: pinned::NCEI_SPEC_ID,
            response_pin: pinned::NCEI_RESPONSE_KECCAK,
            result_pin: pinned::NCEI_RESULT,
            source_name: n::SOURCE_NAME,
            terms_url: n::TERMS_URL,
            terms_quote: n::TERMS_QUOTE,
            unit: "degrees Celsius",
        },
        other => return Err(JsError::new(&format!("no daily source {other}"))),
    };
    feed_round_check(&c, r)
}

/// [`daily_round_check`] on a summary's JSON text (`static/level2/usgs-quakes.json`,
/// `static/level2/ncei-tmax.json`).
#[wasm_bindgen]
pub fn check_daily_round(source: &str, summary_json: &str) -> Result<String, JsError> {
    Ok(daily_round_check(source, &parse(summary_json)?)?.to_string())
}

fn feed_round_check(
    fc: &FeedCheck<'_>,
    r: &serde_json::Value,
) -> Result<serde_json::Value, JsError> {
    use onyx_core::Answer;
    let mut problems: Vec<String> = Vec::new();
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let u128_of = |v: &serde_json::Value| s(v).parse::<u128>().ok();
    let date = s(&r["feed"]["record_date"]);
    let window_start = r["feed"]["window_start"].as_u64().unwrap_or(0);
    let feed = match (fc.spec)(&date, window_start) {
        Ok(f) => f,
        Err(e) => {
            return Ok(json!({"ok": false, "problems": [format!("no canonical feed: {e}")]}));
        }
    };
    let spec_id = feed.id();
    if hex0x(&spec_id.0) != fc.spec_id_pin || hex0x(&spec_id.0) != s(&r["feed"]["spec_id"]) {
        problems.push("the feed is not the pinned feed".into());
    }
    if feed.url != s(&r["feed"]["url"])
        || feed.window_end != r["feed"]["window_end"].as_u64().unwrap_or(0)
    {
        problems.push("the feed's url or window is not the pinned feed's".into());
    }
    // A public build ships the round sealed (ATT-R2-F1): no value, page or
    // reveal, only the signed commits; the value is sold by the paid API.
    let sealed = r["sealed"] == json!(true);
    let body = s(&r["response_body"]).into_bytes();
    let page_hash = keccak256(&body);
    let shown = s(&r["value"]);
    let mut value = 0;
    if !sealed {
        if hex0x(&page_hash.0) != fc.response_pin {
            problems.push("the recorded response is not the pinned response".into());
        }
        value = match fc.fetcher.extract(&feed, &body) {
            Answer::Number(m) => m,
            _ => {
                problems.push("the fetcher reads no value from the recorded response".into());
                0
            }
        };
        if (fc.parse_shown)(&shown) != Some(value) {
            problems.push(format!(
                "the shown value {shown} is not what the fetcher reads"
            ));
        }
    }
    let empty = Vec::new();
    let reveals = r[if sealed { "commits" } else { "reveals" }]
        .as_array()
        .unwrap_or(&empty);
    let bonds = r["bonds"].as_array().unwrap_or(&empty);
    if reveals.len() != usize::from(feed.min_observers) || bonds.len() != reveals.len() {
        problems.push(format!(
            "{} observers, the feed needs {}",
            reveals.len(),
            feed.min_observers
        ));
    }
    // Observers and bonds, ascending and distinct (the round's own order).
    let mut observers: Vec<([u8; 20], u128)> = Vec::new();
    for b in bonds {
        let a: [u8; 20] = bytes(&s(&b["observer"]))?
            .try_into()
            .map_err(|_| JsError::new("observer is not 20 bytes"))?;
        observers.push((a, u128_of(&b["bond"]).unwrap_or(0)));
    }
    if !observers.windows(2).all(|w| w[0].0 < w[1].0) {
        problems.push("observers are not distinct and ascending".into());
    }
    let c = &r["config"];
    let config = (
        c["commit_close"].as_u64().unwrap_or(0),
        c["reveal_close"].as_u64().unwrap_or(0),
        u128_of(&c["reward"]).unwrap_or(0),
        u128_of(&c["outlier_slash"]).unwrap_or(0),
        u128_of(&c["miss_slash"]).unwrap_or(0),
    );
    let (draw_hash, pool_size, draws) = level2_draw_check(
        r,
        &spec_id,
        usize::from(feed.min_observers),
        config,
        &observers,
        &mut problems,
    )?;
    let round_id = level2_round_id(&spec_id, config, &observers, &draw_hash);
    if hex0x(&round_id.0) != s(&r["round_id"]) {
        problems.push("the round id does not recompute".into());
    }
    let attempt = r["draw"]["attempt"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(u32::MAX);
    let checked = level2_observer_checks(
        reveals,
        &feed,
        &round_id,
        attempt,
        &observers,
        (!sealed).then_some((value, page_hash)),
        &mut problems,
    )?;
    let agreed = r["agreed"].as_u64().unwrap_or(0);
    if !sealed && agreed != reveals.len() as u64 {
        problems.push("not every observer agreed".into());
    }
    // ROADMAP C11: a round of a draw with a reveal key carries its sealed
    // posts, dismissals and the chain's answers to the page's own calls
    // (`sealed_reveals`); every recorded rehearsal round has none.
    let sealed_reveals = {
        let t = fc.template.clone();
        let registry_fact = onyx_core::onchain::registry_fact_key(
            &onyx_core::onchain::metric_key(&t.metric),
            &t.id(),
            &onyx_core::onchain::subject_key(&feed.subject),
            feed.window_end,
            t.is_dated(),
        );
        let mut commits = std::collections::BTreeMap::new();
        for e in reveals {
            if let (Ok(a), Ok(c)) = (bytes(&s(&e["observer"])), h32(&s(&e["commit"]))) {
                if let Ok(a) = <[u8; 20]>::try_from(a) {
                    commits.insert(a, c);
                }
            }
        }
        let revealed: std::collections::BTreeSet<[u8; 20]> = commits.keys().copied().collect();
        let drawn: Vec<[u8; 20]> = observers.iter().map(|(a, _)| *a).collect();
        sealed::check(
            &r["sealed_reveals"],
            &sealed::Round {
                spec: &feed,
                far_round: sealed::far_round(&t, &feed, config.1),
                round_id,
                attempt,
                registry_fact,
                window: (config.0, config.1),
                observers: &drawn,
                commits: &commits,
                revealed: (!sealed).then_some(&revealed),
                result: level2_result_hash(&spec_id, &round_id, value),
                value: true,
            },
            &mut problems,
        )
    };
    // The summary's own copies of what the card shows must be the checked or
    // pinned values (L2SRC-R2-F3); the card renders only the checked output.
    let attribution = fc.template.rights.attribution.clone().unwrap_or_default();
    let subject = match &feed.subject {
        onyx_core::Subject::Aggregate(s) => s.clone(),
        _ => String::new(),
    };
    for (field, shown, want) in [
        (
            "source name",
            s(&r["source"]["name"]),
            fc.source_name.to_string(),
        ),
        (
            "terms link",
            s(&r["source"]["terms_url"]),
            fc.terms_url.to_string(),
        ),
        (
            "terms quote",
            s(&r["source"]["terms_quote"]),
            fc.terms_quote.to_string(),
        ),
        (
            "attribution",
            s(&r["source"]["attribution"]),
            attribution.clone(),
        ),
        (
            "metric",
            s(&r["feed"]["metric"]),
            fc.template.metric.0.clone(),
        ),
        ("subject", s(&r["feed"]["subject"]), subject.clone()),
        (
            "selector",
            s(&r["feed"]["selector"]),
            fc.template.selector.clone(),
        ),
        ("unit", s(&r["unit"]), fc.unit.to_string()),
    ] {
        if shown != want {
            problems.push(format!("the summary's {field} is not the pinned {field}"));
        }
    }
    if r["observers"].as_u64() != Some(u64::from(feed.min_observers))
        || r["proof_level"].as_u64() != Some(2)
    {
        problems.push("the summary's observer count or level is not the checked one".into());
    }
    // Every assigned observer revealed this value, so a strict majority of all
    // observers agreed: the round settled `Value(value)`.
    // Sealed, the result hash is only compared with the pin: the reveals
    // that rebuild it are what the paid API sells.
    let result = sealed_or_result(
        sealed,
        r,
        fc.result_pin,
        || level2_result_hash(&spec_id, &round_id, value),
        &mut problems,
    );
    Ok(json!({
        "ok": problems.is_empty(),
        "sealed": sealed,
        "value": if sealed { String::new() } else { shown },
        "commits_checked": checked,
        "page_hash": if sealed { String::new() } else { hex0x(&page_hash.0) },
        "result_hash": result,
        "record_date": date,
        "subject": subject,
        "window_start": feed.window_start,
        "window_end": feed.window_end,
        "observers": feed.min_observers,
        "pool_size": pool_size,
        "draws": draws,
        "without_value": draws - 1,
        "agreed": checked,
        "source_name": fc.source_name,
        "terms_url": fc.terms_url,
        "terms_quote": fc.terms_quote,
        "attribution": attribution,
        "sealed_reveals": sealed_reveals,
        "problems": problems
    }))
}

/// [`level2_round_check`] on the summary's JSON text.
#[wasm_bindgen]
pub fn check_level2_round(summary_json: &str) -> Result<String, JsError> {
    Ok(level2_round_check(&parse(summary_json)?)?.to_string())
}

/// A strict non-negative decimal integer (no sign, no leading zero, no point).
fn strict_units(s: &str) -> Option<i128> {
    let ok = !s.is_empty()
        && s.bytes().all(|c| c.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'));
    if ok {
        s.parse().ok()
    } else {
        None
    }
}

/// Check one company's recorded EDGAR round against its pins (the same
/// checks as [`level2_round_check`]): the canonical feed rebuilt from the
/// period end and window, the pinned response, the fetcher's value, the
/// distinct ascending observers, every sealed commit, the round id and the
/// result hash. Everything shown comes from this output and the adapter's
/// constants, never from the summary.
fn edgar_round_check(
    c: &onyx_source_edgar::Company,
    pins: (&str, &str, &str, Option<&str>),
    r: &serde_json::Value,
) -> Result<serde_json::Value, JsError> {
    use onyx_core::{Answer, Fetcher};
    use onyx_source_edgar::{metric, selector_template, spec, subject, EdgarFetcher};
    let (_, pin_spec, pin_page, pin_result) = pins;
    let mut problems: Vec<String> = Vec::new();
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let u128_of = |v: &serde_json::Value| s(v).parse::<u128>().ok();
    let date = s(&r["feed"]["period_end"]);
    let window_start = r["feed"]["window_start"].as_u64().unwrap_or(0);
    let feed = match spec(c, &date, window_start) {
        Ok(f) => f,
        Err(e) => {
            return Ok(
                json!({"ok": false, "key": c.key, "problems": [format!("no canonical feed: {e}")]}),
            );
        }
    };
    let spec_id = feed.id();
    if hex0x(&spec_id.0) != pin_spec || hex0x(&spec_id.0) != s(&r["feed"]["spec_id"]) {
        problems.push("the feed is not the pinned feed".into());
    }
    if feed.url != s(&r["feed"]["url"])
        || feed.selector != s(&r["feed"]["selector"])
        || feed.window_end != r["feed"]["window_end"].as_u64().unwrap_or(0)
    {
        problems.push("the feed's url, selector or window is not the pinned feed's".into());
    }
    // Sealed in a public build (ATT-R2-F1), as in `level2_round_check`.
    let sealed = r["sealed"] == json!(true);
    let body = s(&r["response_body"]).into_bytes();
    let page_hash = keccak256(&body);
    let shown = s(&r["value"]);
    let mut value = 0;
    if !sealed {
        if hex0x(&page_hash.0) != pin_page {
            problems.push("the recorded response is not the pinned response".into());
        }
        value = match EdgarFetcher.extract(&feed, &body) {
            Answer::Number(m) => m,
            _ => {
                problems.push("the fetcher reads no value from the recorded response".into());
                0
            }
        };
        if strict_units(&shown) != Some(value) {
            problems.push(format!(
                "the shown value {shown} is not what the fetcher reads"
            ));
        }
    }
    let empty = Vec::new();
    let reveals = r[if sealed { "commits" } else { "reveals" }]
        .as_array()
        .unwrap_or(&empty);
    let bonds = r["bonds"].as_array().unwrap_or(&empty);
    if reveals.len() != usize::from(feed.min_observers) || bonds.len() != reveals.len() {
        problems.push(format!(
            "{} observers, the feed needs {}",
            reveals.len(),
            feed.min_observers
        ));
    }
    let mut observers: Vec<([u8; 20], u128)> = Vec::new();
    for b in bonds {
        let a: [u8; 20] = bytes(&s(&b["observer"]))?
            .try_into()
            .map_err(|_| JsError::new("observer is not 20 bytes"))?;
        observers.push((a, u128_of(&b["bond"]).unwrap_or(0)));
    }
    if !observers.windows(2).all(|w| w[0].0 < w[1].0) {
        problems.push("observers are not distinct and ascending".into());
    }
    let cfg = &r["config"];
    let config = (
        cfg["commit_close"].as_u64().unwrap_or(0),
        cfg["reveal_close"].as_u64().unwrap_or(0),
        u128_of(&cfg["reward"]).unwrap_or(0),
        u128_of(&cfg["outlier_slash"]).unwrap_or(0),
        u128_of(&cfg["miss_slash"]).unwrap_or(0),
    );
    let (draw_hash, pool_size, draws) = level2_draw_check(
        r,
        &spec_id,
        usize::from(feed.min_observers),
        config,
        &observers,
        &mut problems,
    )?;
    let round_id = level2_round_id(&spec_id, config, &observers, &draw_hash);
    if hex0x(&round_id.0) != s(&r["round_id"]) {
        problems.push("the round id does not recompute".into());
    }
    let attempt = r["draw"]["attempt"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(u32::MAX);
    let checked = level2_observer_checks(
        reveals,
        &feed,
        &round_id,
        attempt,
        &observers,
        (!sealed).then_some((value, page_hash)),
        &mut problems,
    )?;
    if !sealed && r["agreed"].as_u64() != Some(reveals.len() as u64) {
        problems.push("not every observer agreed".into());
    }
    for (field, shown, want) in [
        ("company key", s(&r["company"]["key"]), c.key.to_string()),
        ("company name", s(&r["company"]["name"]), c.name.to_string()),
        ("CIK", s(&r["company"]["cik"]), c.cik.to_string()),
        ("unit", s(&r["company"]["unit"]), c.unit.to_string()),
        ("metric", s(&r["feed"]["metric"]), metric(c)),
        ("subject", s(&r["feed"]["subject"]), subject(c, &date)),
        (
            "selector template",
            s(&r["feed"]["selector"]).replacen(&date, "{date}", 1),
            selector_template(c),
        ),
    ] {
        if shown != want {
            problems.push(format!("the summary's {field} is not the pinned {field}"));
        }
    }
    if r["observers"].as_u64() != Some(u64::from(feed.min_observers))
        || r["proof_level"].as_u64() != Some(2)
    {
        problems.push("the summary's observer count or level is not the checked one".into());
    }
    let result = sealed_or_result(
        sealed,
        r,
        pin_result,
        || level2_result_hash(&spec_id, &round_id, value),
        &mut problems,
    );
    let ok = problems.is_empty();
    Ok(json!({
        "ok": ok,
        "key": c.key,
        "name": c.name,
        "cik": c.cik,
        "unit": c.unit,
        "unit_meaning": c.unit_meaning,
        "filings_url": format!("https://www.sec.gov/cgi-bin/browse-edgar?action=getcompany&CIK={}&type=10-Q", c.cik),
        "sealed": sealed,
        "value": if ok && !sealed { shown } else { String::new() },
        "period_end": date,
        "window_start": feed.window_start,
        "window_end": feed.window_end,
        "observers": feed.min_observers,
        "pool_size": pool_size,
        "draws": draws,
        "without_value": draws - 1,
        "agreed": checked,
        "result_hash": result,
        "problems": problems,
    }))
}

/// Check the recorded SEC EDGAR rounds (`static/level2/sec-holdings.json`):
/// one entry per pinned company, in order, each passing
/// [`edgar_round_check`]. The source's name, terms and attribution come from
/// the adapter's constants; the summary's own copies must equal them.
fn edgar_rounds_check(doc: &serde_json::Value) -> Result<serde_json::Value, JsError> {
    use onyx_source_edgar::{rights, COMPANIES, SOURCE_NAME, TERMS_QUOTE, TERMS_URL};
    let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let mut problems: Vec<String> = Vec::new();
    let attribution = rights().attribution.unwrap_or_default();
    for (field, shown, want) in [
        (
            "source name",
            s(&doc["source"]["name"]),
            SOURCE_NAME.to_string(),
        ),
        (
            "terms link",
            s(&doc["source"]["terms_url"]),
            TERMS_URL.to_string(),
        ),
        (
            "terms quote",
            s(&doc["source"]["terms_quote"]),
            TERMS_QUOTE.to_string(),
        ),
        (
            "attribution",
            s(&doc["source"]["attribution"]),
            attribution.clone(),
        ),
    ] {
        if shown != want {
            problems.push(format!("the summary's {field} is not the pinned {field}"));
        }
    }
    let empty = Vec::new();
    let entries = doc["companies"].as_array().unwrap_or(&empty);
    if entries.len() != COMPANIES.len() {
        problems.push(format!(
            "{} company rounds, {} pinned",
            entries.len(),
            COMPANIES.len()
        ));
    }
    let mut companies = Vec::new();
    for ((c, pins), e) in COMPANIES.iter().zip(pinned::EDGAR_ROUNDS).zip(entries) {
        if pins.0 != c.key {
            return Err(JsError::new("pins out of order"));
        }
        let out = edgar_round_check(c, pins, e)?;
        if out["ok"] != json!(true) {
            problems.push(format!("{}: the round does not check", c.key));
        }
        companies.push(out);
    }
    Ok(json!({
        "ok": problems.is_empty(),
        "source_name": SOURCE_NAME,
        "terms_url": TERMS_URL,
        "terms_quote": TERMS_QUOTE,
        "attribution": attribution,
        "companies": companies,
        "problems": problems,
    }))
}

/// Check the spot bitcoin ETF holdings from SEC filings
/// (`static/level2/etf-holdings.json`, ROADMAP P2c, THREATS ETF-4): the
/// verifier recomputes the whole summary from the recorded EDGAR responses
/// compiled into it (each checked against its SHA-256, each observer's fetch
/// through the pinned fetcher on the canonical feed, flows from consecutive
/// filed periods) and accepts the shipped file only if it is exactly that
/// summary. Returns `{ok, problems, summary}`; on a mismatch `summary` is
/// null, so the page shows nothing from the shipped file.
fn etf_holdings_check(doc: &serde_json::Value) -> serde_json::Value {
    match onyx_source_edgar::etf::site_summary() {
        Ok(want) if &want == doc => {
            serde_json::json!({"ok": true, "problems": [], "summary": want})
        }
        Ok(_) => serde_json::json!({
            "ok": false,
            "problems": ["the ETF holdings file differs from the recorded filings"],
            "summary": null,
        }),
        Err(e) => serde_json::json!({"ok": false, "problems": [e], "summary": null}),
    }
}

/// A salted (format-3) label answer checked against `pinned`, the content hash the page pins
/// (ROADMAP D30). Returns `salted_view::check`'s JSON.
#[wasm_bindgen]
pub fn check_salted_answer(answer_json: &str, pinned: &str) -> Result<String, JsError> {
    Ok(salted_view::check(&parse(answer_json)?, pinned).to_string())
}

/// The compiled-in content hash of the synthetic salted demo set.
#[wasm_bindgen]
pub fn salted_test_pin() -> String {
    salted_view::SALTED_TEST_PIN.to_string()
}

/// Whether a guessed row confirms against a sibling hash when hashed unsalted (format 2).
#[wasm_bindgen]
pub fn salted_guess_confirms(guess_row_hex: &str, sibling: &str) -> Result<bool, JsError> {
    salted_view::guess_confirms(guess_row_hex, sibling).map_err(|e| JsError::new(&e))
}

/// [`etf_holdings_check`] on the file's JSON text.
#[wasm_bindgen]
pub fn check_etf_holdings(summary_json: &str) -> Result<String, JsError> {
    Ok(etf_holdings_check(&parse(summary_json)?).to_string())
}

/// [`edgar_rounds_check`] on the summary's JSON text.
#[wasm_bindgen]
pub fn check_edgar_rounds(summary_json: &str) -> Result<String, JsError> {
    Ok(edgar_rounds_check(&parse(summary_json)?)?.to_string())
}

/// Run a recipe on the canonical engine (wasmi, same fuel and limits as a
/// validator) with the demo profile. Returns JSON: outcome, fuel, recipe id,
/// root, and the decoded rows.
#[wasm_bindgen]
pub fn run_recipe(recipe: &[u8], input: &[u8]) -> Result<String, JsError> {
    // Not named `wasm`: wasm-bindgen's generated glue uses that name for the module.
    let id = keccak256(recipe);
    let host = Host::new().map_err(|e| JsError::new(&e.to_string()))?;
    let r = host
        .run(&id, recipe, input, &demo_profile())
        .map_err(|e| JsError::new(&e.to_string()))?;
    let rows: Vec<_> = r.leaves.iter().map(|l| flow_json(l)).collect();
    Ok(json!({
        "outcome": format!("{:?}", r.outcome),
        "fuel": r.fuel_used.to_string(),
        "recipe_id": hex0x(&id.0),
        "root": hex0x(&r.root().0),
        "rows": rows,
    })
    .to_string())
}

/// Decode an entity-flow leaf (Borsh `OutputRow` of an `EntityFlowRow`) to JSON.
#[wasm_bindgen]
pub fn decode_flow_leaf(leaf_hex: &str) -> Result<String, JsError> {
    Ok(flow_json(&bytes(leaf_hex)?).to_string())
}

/// Merkle root (ADR-0003) over leaves given as hex rows.
#[wasm_bindgen]
pub fn merkle_root(rows_hex: Vec<String>) -> Result<String, JsError> {
    let rows = rows_hex
        .iter()
        .map(|r| bytes(r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(hex0x(&MerkleTree::from_rows(&rows).root().0))
}

/// Check that `row_hex` is row `index` of `leaf_count` under `root_hex`.
#[wasm_bindgen]
pub fn verify_proof(
    root_hex: &str,
    row_hex: &str,
    index: u32,
    leaf_count: u32,
    siblings_hex: Vec<String>,
) -> Result<bool, JsError> {
    let proof = Proof {
        index: u64::from(index),
        leaf_count: u64::from(leaf_count),
        siblings: siblings_hex
            .iter()
            .map(|s| h32(s))
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(onyx_merkle::verify(
        &h32(root_hex)?,
        &bytes(row_hex)?,
        &proof,
    ))
}

/// Recover the EIP-191 signer of `message` (strict: low s, v in {27, 28}).
/// Returns the 0x address, or an error if the signature is not acceptable.
#[wasm_bindgen]
pub fn recover_eip191(message: &str, sig_hex: &str) -> Result<String, JsError> {
    let sig: [u8; 65] = bytes(sig_hex)?
        .try_into()
        .map_err(|_| JsError::new("signature must be 65 bytes"))?;
    eip191_recover(message.as_bytes(), &sig)
        .map(|a| hex0x(&a))
        .ok_or_else(|| JsError::new("signature refused"))
}

/// Entity key of the demo attestation: derived from a public test label, it
/// holds nothing. Pinned here so the page does not trust the data's key.
fn demo_entity_key() -> Result<[u8; 20], JsError> {
    let sk = secp256k1::SecretKey::from_slice(&keccak256(b"onyx test entity key 1").0)
        .map_err(|e| JsError::new(&e.to_string()))?;
    let pk = sk
        .public_key(&secp256k1::Secp256k1::new())
        .serialize_uncompressed();
    let mut a = [0u8; 20];
    a.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
    Ok(a)
}

/// Address of the demo entity key pinned in this verifier.
#[wasm_bindgen]
pub fn demo_entity_key_address() -> Result<String, JsError> {
    Ok(hex0x(&demo_entity_key()?))
}

/// Full attestation check (ADR-0015: BIP-322 by the address and EIP-191 by
/// a registered entity key) against the demo registry pinned in this code:
/// domain `onyx.devnet`, `entity:example`, the demo key. Judged over BTC
/// blocks `from..=to`. Returns "valid" or the refusal reason.
#[wasm_bindgen]
pub fn verify_demo_attestation(
    statement: &str,
    address_sig: &str,
    entity_key_hex: &str,
    entity_sig_hex: &str,
    from: u32,
    to: u32,
) -> Result<String, JsError> {
    let key = demo_entity_key()?;
    let mut reg = StaticRegistry::new();
    reg.set_domain("onyx.devnet")
        .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(Bip322Proof)))
        .and_then(|r| r.add_entity_key("entity:example", key))
        .map_err(|e| JsError::new(&e.to_string()))?;
    let att = Attestation {
        statement: statement.into(),
        address_sig: address_sig.into(),
        entity_key: bytes(entity_key_hex)?
            .try_into()
            .map_err(|_| JsError::new("entity key must be 20 bytes"))?,
        entity_sig: bytes(entity_sig_hex)?
            .try_into()
            .map_err(|_| JsError::new("signature must be 65 bytes"))?,
    };
    let ranges = [("btc", u64::from(from), u64::from(to))];
    Ok(match att.verify(&AttestContext { ranges: &ranges }, &reg) {
        Ok(_) => "valid".into(),
        Err(e) => e.to_string(),
    })
}

/// BIP-322 check of one message signature by one BTC address, with the
/// network's policy (ADR-0015). Returns "Valid", "Invalid" or "Inconclusive".
#[wasm_bindgen]
pub fn bip322_verdict(address: &str, message: &str, sig: &str) -> String {
    match Bip322Proof.verify(address, message.as_bytes(), sig) {
        ProofOutcome::Valid => "Valid",
        ProofOutcome::Invalid => "Invalid",
        ProofOutcome::Inconclusive => "Inconclusive",
    }
    .into()
}

/// Check a raw 80-byte header: proof of work and at least the pinned minimum
/// difficulty. Returns JSON with the block hash and parent hash in display order.
#[wasm_bindgen]
pub fn check_header(header_hex: &str) -> Result<String, JsError> {
    let (hash, parent) = node::check_raw_header(&bytes(header_hex)?, node::MAINNET_MIN_BITS)
        .map_err(|e| JsError::new(&e))?;
    Ok(json!({ "hash": display(&hash), "parent": display(&parent) }).to_string())
}

/// Brier term (units of 1e-12) for a label naming an entity with confidence
/// `p_ppm`, over `candidates` registered entities (ADR-0016).
#[wasm_bindgen]
pub fn brier_term(p_ppm: u32, named_is_owner: bool, candidates: u32) -> String {
    onyx_labels::score::brier_term(Confidence(p_ppm), named_is_owner, u64::from(candidates))
        .to_string()
}

/// Run the BIP-322 registry vectors compiled into the network code (the same
/// list hashed into the registry root) and return each verdict as JSON.
#[wasm_bindgen]
pub fn run_bip322_vectors() -> String {
    let fmt = |o: ProofOutcome| format!("{o:?}");
    let rows: Vec<_> = Bip322Proof
        .vectors()
        .iter()
        .map(|v| {
            json!({
                "body": v.body,
                "variant": v.signature.get(..3).unwrap_or(""),
                "expect": fmt(v.expect),
                "got": fmt(Bip322Proof.verify(v.body, v.message.as_bytes(), v.signature)),
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

#[cfg(test)]
mod tests {
    use super::pinned;

    /// A file that holds label-derived data (left out of a public snapshot while the label rights
    /// are pending, docs/ops/public-snapshot-exclude.txt), read when a test runs; the tests that
    /// read one are ignored without the `label-data` feature (pre-public loop PP13).
    fn label_data(rel: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn pins_match_the_repo() {
        let golden =
            label_data("../../../fixtures/golden/btc-entity-balance-921080-921082.labels-v2.json");
        assert!(golden.contains(&format!("\"input_keccak\": \"{}\"", pinned::INPUT_KECCAK)));
        assert!(golden.contains(&format!("\"recipe_id\": \"{}\"", pinned::RECIPE_ID)));
        assert!(golden.contains(&format!("\"root\": \"{}\"", pinned::ROOT)));
        let id = include_str!("../../../../recipes/out/btc_entity_balance.wasm.keccak");
        assert_eq!(id.trim(), pinned::RECIPE_ID);
        let series = include_str!("../../../../recipes/out/btc_entity_flow_series.wasm.keccak");
        assert_eq!(series.trim(), pinned::SERIES_RECIPE_ID);
        let golden = label_data(
            "../../../fixtures/golden/btc-entity-flow-series-921080-921082.labels-v2.json",
        );
        assert!(golden.contains(&format!("\"recipe_id\": \"{}\"", pinned::SERIES_RECIPE_ID)));
    }

    fn series_leaf(entity: &str, height: u64, rec: u128, sent: u128, txs: u64) -> String {
        use borsh::BorshSerialize;
        let row = onyx_btc_schema::EntityBlockFlowRow {
            entity: entity.into(),
            height,
            received_sats: rec,
            sent_sats: sent,
            net_sats: rec as i128 - sent as i128,
            tx_count: txs,
        };
        let out = onyx_recipe_sdk::OutputRow {
            key: onyx_btc_schema::EntityBlockFlowRow::key(entity, height),
            value: borsh::to_vec(&row).expect("value"),
        };
        let mut b = Vec::new();
        out.serialize(&mut b).expect("row");
        super::hex0x(&b)
    }

    #[test]
    fn series_adds_up_and_tampering_is_caught() {
        let range = vec![
            leaf("entity:a", 10, 12, 700, 250, 4),
            leaf("entity:b", 10, 12, 0, 0, 0),
        ];
        let series = vec![
            series_leaf("entity:a", 10, 500, 0, 1),
            series_leaf("entity:a", 11, 0, 250, 2),
            series_leaf("entity:a", 12, 200, 0, 1),
        ];
        let (rr, sr) = (root(&range), root(&series));
        let ok = super::series_sums(&range, &series, &rr, &sr, 10, 12).expect("sums");
        assert_eq!(ok["ok"], true, "{ok}");
        assert_eq!(
            ok["entities"][0]["points"].as_array().map(Vec::len),
            Some(3)
        );
        // A row left out: the series root moves and received no longer adds up.
        let mut gap = series.clone();
        gap.remove(0);
        let bad = super::series_sums(&range, &gap, &rr, &sr, 10, 12).expect("sums");
        assert_eq!(bad["ok"], false);
        assert_eq!(bad["series_root_ok"], false);
        assert!(bad["problems"].to_string().contains("received"), "{bad}");
        // Out of order, outside the range, or an entity the range lacks.
        let swapped = vec![series[1].clone(), series[0].clone(), series[2].clone()];
        let out = super::series_sums(&range, &swapped, &rr, &root(&swapped), 10, 12).expect("s");
        assert!(
            out["problems"].to_string().contains("out of order"),
            "{out}"
        );
        let wide = vec![series_leaf("entity:a", 13, 700, 250, 4)];
        let w = super::series_sums(&range, &wide, &rr, &root(&wide), 10, 12).expect("s");
        assert!(w["problems"].to_string().contains("outside"), "{w}");
        let extra = [series.clone(), vec![series_leaf("entity:c", 11, 1, 0, 1)]].concat();
        let x = super::series_sums(&range, &extra, &rr, &root(&extra), 10, 12).expect("s");
        assert!(x["problems"].to_string().contains("entity:c"), "{x}");
        // SER-R1 review cases: a duplicated row, a row moved to another height
        // or to another entity, all caught.
        let dup = vec![
            series[0].clone(),
            series[0].clone(),
            series[1].clone(),
            series[2].clone(),
        ];
        assert_eq!(
            super::series_sums(&range, &dup, &rr, &sr, 10, 12).expect("s")["ok"],
            false
        );
        let moved = vec![
            series_leaf("entity:a", 10, 500, 0, 1),
            series_leaf("entity:a", 11, 200, 250, 3),
        ];
        assert_eq!(
            super::series_sums(&range, &moved, &rr, &sr, 10, 12).expect("s")["ok"],
            false
        );
        let other = vec![
            series[0].clone(),
            series[1].clone(),
            series_leaf("entity:b", 12, 200, 0, 1),
        ];
        assert_eq!(
            super::series_sums(&range, &other, &rr, &sr, 10, 12).expect("s")["ok"],
            false
        );
        // A sum-preserving shift between two blocks of one entity adds up:
        // only the pinned series root catches it (SER-R1-F3).
        let shifted = vec![
            series_leaf("entity:a", 10, 501, 0, 1),
            series_leaf("entity:a", 11, 0, 250, 2),
            series_leaf("entity:a", 12, 199, 0, 1),
        ];
        let sh = super::series_sums(&range, &shifted, &rr, &sr, 10, 12).expect("s");
        assert_eq!(sh["ok"], false);
        assert_eq!(sh["series_root_ok"], false);
        assert_eq!(sh["problems"].as_array().map(Vec::len), Some(1), "{sh}");
    }

    /// SER-R2-F4: the real quiet block 970,551 (exported with `ONYX_LIVE`)
    /// checks against the pinned quiet roots as an all-zero series; the same
    /// rows with one byte changed do not.
    #[test]
    fn the_pinned_quiet_block_is_an_all_zero_series() {
        let quiet: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/live-quiet.json"))
                .expect("fixture");
        let range: Vec<String> = quiet["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r["leaf"].as_str().expect("leaf").to_string())
            .collect();
        let leaf = quiet["series"]["no_activity_leaf"]
            .as_str()
            .expect("leaf")
            .to_string();
        let ok: serde_json::Value = serde_json::from_str(
            &super::check_series(range.clone(), vec![leaf.clone()]).expect("check"),
        )
        .expect("json");
        assert_eq!(ok["ok"], true, "{ok}");
        assert_eq!(ok["no_activity"], true);
        // Flip one hex digit inside an amount (as the e2e forger does), so the
        // row still decodes but no longer hashes to the pinned root.
        let mut edited = range.clone();
        let at = edited[0].len() - 52;
        let flipped = if edited[0].as_bytes()[at] == b'0' {
            "1"
        } else {
            "0"
        };
        edited[0].replace_range(at..=at, flipped);
        let bad: serde_json::Value =
            serde_json::from_str(&super::check_series(edited, vec![leaf]).expect("check"))
                .expect("json");
        assert_eq!(bad["ok"], false, "{bad}");
    }

    /// SER-R1-F1: a quiet range commits the NO_ACTIVITY failure leaf; the
    /// verifier reads it as an all-zero series, and refuses it when a range
    /// row says there was activity.
    #[test]
    fn no_activity_is_an_all_zero_series() {
        let quiet = vec![super::hex0x(&super::failure_leaf(
            super::Outcome::RecipeError(onyx_btc_schema::SERIES_NO_ACTIVITY),
        ))];
        let zero = vec![leaf("entity:a", 10, 12, 0, 0, 0)];
        let ok =
            super::series_sums(&zero, &quiet, &root(&zero), &root(&quiet), 10, 12).expect("sums");
        assert_eq!(ok["ok"], true, "{ok}");
        assert_eq!(ok["no_activity"], true);
        assert_eq!(
            ok["entities"][0]["points"].as_array().map(Vec::len),
            Some(0)
        );
        let busy = vec![leaf("entity:a", 10, 12, 5, 0, 1)];
        let bad =
            super::series_sums(&busy, &quiet, &root(&busy), &root(&quiet), 10, 12).expect("sums");
        assert_eq!(bad["ok"], false, "{bad}");
    }

    fn leaf(entity: &str, from: u64, to: u64, rec: u128, sent: u128, txs: u64) -> String {
        use borsh::BorshSerialize;
        let row = onyx_btc_schema::EntityFlowRow {
            entity: entity.into(),
            from_height: from,
            to_height: to,
            received_sats: rec,
            sent_sats: sent,
            net_sats: rec as i128 - sent as i128,
            tx_count: txs,
            end_balance_sats: None,
        };
        let out = onyx_recipe_sdk::OutputRow {
            key: borsh::to_vec(&row.entity).expect("key"),
            value: borsh::to_vec(&row).expect("value"),
        };
        let mut b = Vec::new();
        out.serialize(&mut b).expect("row");
        super::hex0x(&b)
    }

    fn root(rows: &[String]) -> String {
        let leaves: Vec<Vec<u8>> = rows.iter().map(|r| super::bytes(r).expect("hex")).collect();
        super::hex0x(&onyx_merkle::MerkleTree::from_rows(&leaves).root().0)
    }

    #[test]
    fn blocks_add_up_to_the_range_and_an_omission_is_caught() {
        let range = vec![leaf("entity:a", 10, 12, 700, 250, 4)];
        let r = root(&range);
        let blocks = vec![
            (10, vec![leaf("entity:a", 10, 10, 500, 0, 1)]),
            (11, vec![leaf("entity:a", 11, 11, 0, 250, 2)]),
            (12, vec![leaf("entity:a", 12, 12, 200, 0, 1)]),
        ];
        let ok = super::block_sums(&range, &blocks, &r, 10, 12).expect("sums");
        assert_eq!(ok["ok"], true, "{ok}");
        // A block left out: received no longer adds up, and the heights have a gap.
        let mut gap = blocks.clone();
        gap.remove(0);
        let bad = super::block_sums(&range, &gap, &r, 10, 12).expect("sums");
        assert_eq!(bad["ok"], false);
        assert!(bad["problems"].to_string().contains("received"), "{bad}");
        // A wrong root, or a row covering more than its block, is caught too.
        let wrong = super::block_sums(&range, &blocks, &root(&blocks[0].1), 10, 12).expect("sums");
        assert_eq!(wrong["root_ok"], false);
        let mut wide = blocks.clone();
        wide[1].1 = vec![leaf("entity:a", 10, 11, 0, 250, 2)];
        assert_eq!(
            super::block_sums(&range, &wide, &r, 10, 12).expect("sums")["ok"],
            false
        );
    }

    /// The committed one-week history: its index and its last day.
    fn index() -> serde_json::Value {
        serde_json::from_str(&label_data("../static/history/week/index.json")).expect("index json")
    }
    fn last_day() -> serde_json::Value {
        serde_json::from_str(&label_data("../static/history/week/2026-10-10.json"))
            .expect("day json")
    }

    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn the_week_index_checks_out() {
        let r = super::history_index_check(&index()).expect("check");
        assert_eq!(r["problems"], serde_json::json!([]));
        assert_eq!(r["ok"], true);
        assert_eq!(r["commitment_ok"], true);
        assert_eq!(r["blocks"].as_array().map(Vec::len), Some(1_008));
        assert!(r["days"].as_array().map_or(0, Vec::len) >= 7);
    }

    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn a_day_checks_out_against_the_index() {
        let r = super::history_day_check(&index(), "2026-10-10", &last_day()).expect("check");
        assert_eq!(r["problems"], serde_json::json!([]));
        assert_eq!(r["ok"], true);
        assert_eq!(r["buckets_ok"], true);
        let points: usize = r["entities"]
            .as_array()
            .expect("entities")
            .iter()
            .map(|e| e["points"].as_array().map_or(0, Vec::len))
            .sum();
        assert_eq!(serde_json::json!(points), r["rows"]);
    }

    /// HIST-W1-F1: an honest day served at another day's URL, with that other
    /// day's buckets edited in the index, is named as failed for the slot it
    /// was loaded for (the buckets are not bound by the commitment).
    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn a_day_served_in_another_days_slot_fails() {
        let i = index();
        let r = super::history_day_check(&i, "2026-10-09", &last_day()).expect("check");
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(r["date"], "2026-10-09");
        assert!(
            r["problems"]
                .to_string()
                .contains("the file is for 2026-10-10"),
            "{r}"
        );
        // The day's own slot still checks out, and its buckets are the ones
        // recomputed from its checked rows.
        let ok = super::history_day_check(&i, "2026-10-10", &last_day()).expect("check");
        assert_eq!(ok["ok"], true, "{ok}");
        let days = i["days"].as_array().expect("days");
        assert_eq!(ok["buckets"], days[days.len() - 1]["buckets"]);
    }

    /// HIST-W1-F2: a forged index that moves a day boundary to another shard
    /// edge (the later day starting later, the earlier day absorbing its first
    /// hours) fails the index check, which recomputes the days from the headers.
    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn a_moved_day_boundary_fails() {
        let mut i = index();
        let shards: Vec<(u64, u64)> = i["shards"]
            .as_array()
            .expect("shards")
            .iter()
            .map(|s| (s["from"].as_u64().expect("f"), s["to"].as_u64().expect("t")))
            .collect();
        let days = i["days"].as_array_mut().expect("days");
        let last = days.len() - 1;
        let start = days[last]["from"].as_u64().expect("from");
        // The next shard edge inside the last day.
        let (_, edge) = *shards
            .iter()
            .find(|(f, _)| *f == start)
            .expect("the day's first shard");
        assert!(
            edge < days[last]["to"].as_u64().expect("to"),
            "a day of one shard"
        );
        days[last - 1]["to"] = serde_json::json!(edge);
        days[last]["from"] = serde_json::json!(edge + 1);
        let r = super::history_index_check(&i).expect("check");
        assert_eq!(r["ok"], false, "{r}");
        assert!(
            r["problems"]
                .to_string()
                .contains("the days are not the UTC days the headers give"),
            "{r}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn an_edited_history_fails() {
        let day = |i: &serde_json::Value, d: &serde_json::Value| {
            let date = d["date"].as_str().expect("date").to_string();
            super::history_day_check(i, &date, d).expect("check")
        };
        let idx = |i: &serde_json::Value| super::history_index_check(i).expect("check");
        // A changed number in a leaf (still a valid row): its root moves.
        let mut leaf = last_day();
        let shard = leaf["shards"]
            .as_array()
            .expect("shards")
            .iter()
            .position(|s| s["series_leaves"].as_array().map_or(0, Vec::len) > 1)
            .expect("a shard with rows");
        let l = leaf["shards"][shard]["series_leaves"][0]
            .as_str()
            .expect("leaf")
            .to_string();
        let mut b = hex::decode(&l).expect("hex");
        let last = b.len() - 1;
        b[last] ^= 1;
        leaf["shards"][shard]["series_leaves"][0] = serde_json::json!(hex::encode(b));
        assert_eq!(day(&index(), &leaf)["ok"], false);
        // A changed bucket: the index alone cannot see it, the day check does.
        let mut bucket = index();
        let days = bucket["days"].as_array().expect("days").len();
        let k = bucket["days"][days - 1]["buckets"]
            .as_array()
            .expect("buckets")
            .iter()
            .position(|k| k["entities"].as_array().is_some_and(|e| !e.is_empty()))
            .expect("a bucket with flows");
        bucket["days"][days - 1]["buckets"][k]["entities"][0]["net_sats"] = serde_json::json!("1");
        assert_eq!(idx(&bucket)["ok"], true);
        let r = day(&bucket, &last_day());
        assert_eq!(r["ok"], false);
        assert_eq!(r["buckets_ok"], false);
        // A dropped shard.
        let mut gap = index();
        gap["shards"].as_array_mut().expect("shards").remove(3);
        assert_eq!(idx(&gap)["ok"], false);
        // A header from elsewhere breaks the chain.
        let mut hdr = index();
        let other = hdr["headers"][1].clone();
        hdr["headers"][7] = other;
        assert_eq!(idx(&hdr)["ok"], false);
        // A day file under another date.
        let mut moved = last_day();
        moved["date"] = serde_json::json!("2026-10-09");
        assert_eq!(day(&index(), &moved)["ok"], false);
    }

    /// HIST-R1: a self-consistent fake (flow moved between two blocks of one
    /// entity, its shard root in the index and the day's buckets recomputed so
    /// every sum still adds up) fails only on the commitment compiled into
    /// this verifier. Whoever ships the page can replace both; see the page.
    #[test]
    #[cfg_attr(
        not(feature = "label-data"),
        ignore = "needs the label data, not public yet"
    )]
    fn a_self_consistent_fake_fails_only_on_the_pinned_commitment() {
        use borsh::{BorshDeserialize, BorshSerialize};
        let decode = |l: &serde_json::Value| {
            let o = super::OutputRow::try_from_slice(
                &hex::decode(l.as_str().expect("leaf")).expect("hex"),
            )
            .expect("row");
            let r = super::EntityBlockFlowRow::try_from_slice(&o.value).expect("flow");
            (o, r)
        };
        let mut d = last_day();
        let mut i = index();
        let (si, ri, rj) = d["shards"]
            .as_array()
            .expect("shards")
            .iter()
            .enumerate()
            .find_map(|(si, s)| {
                let rows: Vec<_> = s["series_leaves"].as_array()?.iter().map(decode).collect();
                (0..rows.len())
                    .flat_map(|a| (a + 1..rows.len()).map(move |b| (a, b)))
                    .find(|&(a, b)| {
                        rows[a].1.entity == rows[b].1.entity && rows[a].1.received_sats > 1000
                    })
                    .map(|(a, b)| (si, a, b))
            })
            .expect("two rows of one entity in one shard");
        let mut rows: Vec<_> = d["shards"][si]["series_leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(decode)
            .collect();
        rows[ri].1.received_sats -= 1000;
        rows[ri].1.net_sats -= 1000;
        rows[rj].1.received_sats += 1000;
        rows[rj].1.net_sats += 1000;
        let leaves: Vec<Vec<u8>> = rows
            .iter()
            .map(|(o, r)| {
                let mut value = Vec::new();
                r.serialize(&mut value).expect("ser");
                let mut b = Vec::new();
                super::OutputRow {
                    key: o.key.clone(),
                    value,
                }
                .serialize(&mut b)
                .expect("ser");
                b
            })
            .collect();
        d["shards"][si]["series_leaves"] =
            serde_json::json!(leaves.iter().map(hex::encode).collect::<Vec<_>>());
        let from = d["shards"][si]["from"].clone();
        let new_root = super::hex0x(&super::MerkleTree::from_rows(&leaves).root().0);
        for s in i["shards"].as_array_mut().expect("shards") {
            if s["from"] == from {
                s["series_root"] = serde_json::json!(new_root);
            }
        }
        // Recompute the day's buckets from the edited rows.
        let all: Vec<super::EntityBlockFlowRow> = d["shards"]
            .as_array()
            .expect("shards")
            .iter()
            .flat_map(|s| s["series_leaves"].as_array().cloned().unwrap_or_default())
            .map(|l| decode(&l).1)
            .collect();
        let days = i["days"].as_array().expect("days").len();
        for k in i["days"][days - 1]["buckets"]
            .as_array_mut()
            .expect("buckets")
        {
            let (bf, bt) = (k["from"].as_u64().expect("f"), k["to"].as_u64().expect("t"));
            let mut sums: std::collections::BTreeMap<&str, (u128, u128, i128, u64)> =
                std::collections::BTreeMap::new();
            for r in all.iter().filter(|r| (bf..=bt).contains(&r.height)) {
                let e = sums.entry(r.entity.as_str()).or_default();
                e.0 += r.received_sats;
                e.1 += r.sent_sats;
                e.2 += r.net_sats;
                e.3 += r.tx_count;
            }
            k["entities"] = serde_json::Value::Array(
                sums.iter()
                    .map(|(n, v)| {
                        serde_json::json!({"entity": n,
                        "received_sats": v.0.to_string(), "sent_sats": v.1.to_string(),
                        "net_sats": v.2.to_string(), "tx_count": v.3})
                    })
                    .collect(),
            );
        }
        let r = super::history_index_check(&i).expect("check");
        assert_eq!(
            r["problems"],
            serde_json::json!(["the shards do not give the pinned history commitment"])
        );
        let r = super::history_day_check(&i, "2026-10-10", &d).expect("check");
        assert_eq!(
            r["problems"],
            serde_json::json!(["the week's index does not check out"])
        );
    }

    /// The recorded level-2 round checks out in the browser verifier, and any
    /// edit to the value, a commit, a fetch time or the response is caught.
    #[test]
    fn the_level2_round_checks_and_edits_fail() {
        let text = include_str!("../../static/level2/debt-to-penny.json");
        let r: serde_json::Value = serde_json::from_str(text).expect("json");
        let check = |v: &serde_json::Value| super::level2_round_check(v).expect("check");
        let ok = check(&r);
        assert_eq!(ok["ok"], serde_json::json!(true), "{ok}");
        assert_eq!(ok["commits_checked"], serde_json::json!(5));
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let edits: Vec<Edit> = vec![
            Box::new(|v| v["value"] = "40305316210830.72".into()),
            Box::new(|v| v["reveals"][2]["commit"] = format!("0x{}", "11".repeat(32)).into()),
            Box::new(|v| v["reveals"][0]["observed_unix"] = 1_791_696_000u64.into()),
            Box::new(|v| {
                let b = v["response_body"]
                    .as_str()
                    .unwrap_or_default()
                    .replace("40305316210829.72", "30305316210829.72");
                v["response_body"] = b.into();
            }),
            Box::new(|v| v["result_hash"] = format!("0x{}", "22".repeat(32)).into()),
            Box::new(|v| v["feed"]["window_start"] = 1_791_696_361u64.into()),
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut t = r.clone();
            edit(&mut t);
            assert_eq!(check(&t)["ok"], serde_json::json!(false), "edit {i} passed");
        }
    }

    /// ETF-4: the shipped ETF holdings file checks, and any edit (a value, a
    /// date, an accession, a dropped fund, the source text) fails.
    #[test]
    fn the_etf_holdings_check_and_edits_fail() {
        let text = include_str!("../../static/level2/etf-holdings.json");
        let doc: serde_json::Value = serde_json::from_str(text).expect("json");
        let ok = super::etf_holdings_check(&doc);
        assert_eq!(ok["ok"], serde_json::json!(true), "{ok}");
        assert_eq!(ok["summary"], doc);
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let edits: Vec<(&str, Edit)> = vec![
            (
                "value",
                Box::new(|d| d["funds"][0]["holdings"][0]["held_sats"] = "1".into()),
            ),
            (
                "btc",
                Box::new(|d| d["funds"][2]["holdings"][0]["held_btc"] = "1.00000000".into()),
            ),
            (
                "date",
                Box::new(|d| d["funds"][0]["holdings"][0]["period_end"] = "2024-12-30".into()),
            ),
            (
                "accession",
                Box::new(|d| d["funds"][1]["holdings"][0]["accn"] = "0000000000-26-000001".into()),
            ),
            (
                "flow",
                Box::new(|d| d["funds"][0]["flows"][0]["change_sats"] = "0".into()),
            ),
            (
                "terms",
                Box::new(|d| d["source"]["terms_url"] = "https://evil.example/terms".into()),
            ),
            (
                "dropped",
                Box::new(|d| {
                    d["funds"].as_array_mut().expect("funds").pop();
                }),
            ),
            (
                "note",
                Box::new(|d| d["flows_note"] = "Daily flows.".into()),
            ),
        ];
        for (name, edit) in edits {
            let mut d = doc.clone();
            edit(&mut d);
            let c = super::etf_holdings_check(&d);
            assert_eq!(c["ok"], serde_json::json!(false), "{name}");
            assert!(c["summary"].is_null(), "{name}");
        }
    }

    /// SEC-5: the recorded EDGAR rounds check, and every edit (value, page,
    /// commit, window, period, company, source text, a dropped company)
    /// fails.
    #[test]
    fn the_edgar_rounds_check_and_edits_fail() {
        let text = include_str!("../../static/level2/sec-holdings.json");
        let r: serde_json::Value = serde_json::from_str(text).expect("json");
        let check = |v: &serde_json::Value| super::edgar_rounds_check(v).expect("check");
        let ok = check(&r);
        assert_eq!(ok["ok"], serde_json::json!(true), "{ok}");
        assert_eq!(ok["companies"][0]["value"], "762099");
        assert_eq!(ok["companies"][1]["value"], "15679");
        assert_eq!(ok["companies"][0]["agreed"], serde_json::json!(3));
        // SEC-R2-F1: the shown draw counts come from the checked announcement.
        assert_eq!(ok["companies"][0]["draws"], serde_json::json!(1));
        assert_eq!(ok["companies"][0]["without_value"], serde_json::json!(0));
        // A draw count that is not the round's attempt + 1 fails the check.
        let mut later = r.clone();
        later["companies"][0]["draw"]["announcement"]["draw_count"] = 2.into();
        assert_eq!(check(&later)["ok"], serde_json::json!(false));
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let edits: Vec<Edit> = vec![
            Box::new(|v| v["companies"][0]["value"] = "762100".into()),
            Box::new(|v| v["companies"][0]["value"] = "0762099".into()),
            Box::new(|v| {
                v["companies"][1]["reveals"][1]["commit"] = format!("0x{}", "11".repeat(32)).into()
            }),
            Box::new(|v| {
                v["companies"][0]["reveals"][0]["observed_unix"] = 1_791_753_000u64.into()
            }),
            Box::new(|v| {
                let b = v["companies"][0]["response_body"]
                    .as_str()
                    .unwrap_or_default()
                    .replace("762099", "862099");
                v["companies"][0]["response_body"] = b.into();
            }),
            Box::new(|v| v["companies"][0]["feed"]["period_end"] = "2025-12-31".into()),
            Box::new(|v| v["companies"][0]["company"]["name"] = "Someone Else".into()),
            Box::new(|v| {
                v["companies"][1]["result_hash"] = format!("0x{}", "22".repeat(32)).into()
            }),
            Box::new(|v| v["source"]["terms_url"] = "https://evil.example/terms".into()),
            Box::new(|v| {
                if let Some(a) = v["companies"].as_array_mut() {
                    a.truncate(1);
                }
            }),
            Box::new(|v| {
                if let Some(a) = v["companies"].as_array_mut() {
                    a.swap(0, 1);
                }
            }),
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut t = r.clone();
            edit(&mut t);
            assert_eq!(check(&t)["ok"], serde_json::json!(false), "edit {i} passed");
        }
    }

    /// The browser's mirrors give the coordinator's own result hashes for the
    /// recorded EDGAR rounds.
    #[test]
    fn the_edgar_pins_are_the_coordinators_results() {
        use onyx_coordinator::observe::RecordedDraw;
        use onyx_source_edgar::round::{recorded_draw, run, store, Page};
        let mut st = store();
        for (rec, pins) in onyx_source_edgar::recorded()
            .iter()
            .zip(super::pinned::EDGAR_ROUNDS)
        {
            let pages: Vec<Page> = rec
                .fetches
                .iter()
                .map(|f| Page {
                    fetched_unix: f.fetched_unix,
                    body: Some(f.body.clone()),
                })
                .collect();
            let r = run(
                &rec.company,
                &rec.period_end,
                rec.window_start,
                &pages,
                &mut st,
                &mut RecordedDraw::new(recorded_draw(rec.company.key)),
            )
            .expect("round");
            assert_eq!(
                Some(super::hex0x(&r.settlement.result_hash().0).as_str()),
                pins.3
            );
            assert_eq!(super::hex0x(&r.spec.id().0), pins.1);
            assert_eq!(super::hex0x(&onyx_core::keccak256(&rec.body).0), pins.2);
        }
    }
}
