//! The browser's checks of the Ethereum window (ADR-0044, website /data
//! "Ethereum on chain"): the exported `eth-chain-stats` leaves against the
//! pinned commitment, the figures the page draws (computed here from the
//! checked rows, never read from the file), and a rerun of the recipe on one
//! block's full input on the canonical engine.

use super::{demo_profile, hex0x};
use borsh::BorshDeserialize;
use onyx_core::{keccak256, Hash32};
use onyx_eth_schema::{EthChainSource, EthChainStatsRow};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{Host, Outcome};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_eth::{eth_chain_stats_commitment, utc_date, EthShard};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// Pins of the website's Ethereum window, compiled into the verifier so
/// editing the data next to the page cannot move them. Written by
/// `scripts/refresh-eth-onchain.py --write-pins` from
/// `examples/export-eth-chain-stats.rs`, which runs every block of the window;
/// `tests/golden_eth_chain.rs` reruns three committed blocks always and a
/// sample of eight more when the fetched window is present, and
/// `scripts/recount-eth-chain-stats.py` agrees on every field of every block.
/// Public chain data only: no label set.
pub mod pins {
    /// `recipes/out/eth_chain_stats.wasm.keccak`.
    pub const ETH_CHAIN_RECIPE_ID: &str =
        "0x84773a98009c23c0289606b539bbf783f7c01b9dffa455bf9960ba718184046c";
    /// First block of the window.
    pub const ETH_CHAIN_FROM: u64 = 26_225_261;
    /// Last block of the window (finalized when fetched).
    pub const ETH_CHAIN_TO: u64 = 26_225_560;
    /// Hash of the window's last block.
    pub const ETH_CHAIN_TIP: &str =
        "f630b9d91bde21e09efd502b0bde3fb11113673255164eb1c834ac5d334dd76f";
    /// The window's commitment (`onyx_source_eth::eth_chain_stats_commitment`).
    pub const ETH_CHAIN_COMMITMENT: &str =
        "0xbd7c325d0c8d5b812d4ff59e14cc685c1e6cb618a31c75cf37c409b826c1589c";
    /// The block whose full input the page reruns (the window's smallest with at
    /// least 100 transactions and a blob transaction).
    pub const ETH_CHAIN_RERUN_HEIGHT: u64 = 26_225_368;
    /// Keccak-256 of that recipe input (`static/onchain/eth-rerun-input.b64.txt`).
    pub const ETH_CHAIN_RERUN_INPUT: &str =
        "0xf1aeb9f962f7ddca5d8481ab98704717cd71728de98be6e02323f3016a32cd07";
}

/// Most leaves one window may carry (a day of blocks is 7,200).
const MAX_BLOCKS: usize = 50_000;

/// Blocks per bar of the page's summed charts: 25 blocks, five minutes at 12 s a slot.
pub const GROUP_BLOCKS: usize = 25;

fn leaf_row(hex_leaf: &str) -> Option<(Vec<u8>, EthChainStatsRow)> {
    let leaf = hex::decode(hex_leaf.trim_start_matches("0x")).ok()?;
    let row = OutputRow::try_from_slice(&leaf).ok()?;
    let v = EthChainStatsRow::try_from_slice(&row.value).ok()?;
    (row.key == EthChainStatsRow::key(v.number)).then_some((leaf, v))
}

/// Integer share in parts per million (0 when the whole is 0).
fn ppm(part: u128, whole: u128) -> u64 {
    (part * 1_000_000)
        .checked_div(whole)
        .map_or(0, |v| u64::try_from(v).unwrap_or(u64::MAX))
}

/// Sums over checked rows (the window, or one day).
#[derive(Default)]
struct Sums {
    blocks: u64,
    first_time: u64,
    last_time: u64,
    txs: u64,
    failed: u64,
    creations: u64,
    gas_used: u128,
    gas_limit: u128,
    base_fee: u128,
    base_fee_min: u128,
    base_fee_max: u128,
    burned: u128,
    priority: u128,
    blob_txs: u64,
    blob_gas: u128,
    blob_fee: u128,
    value: u128,
}

impl Sums {
    fn add(&mut self, r: &EthChainStatsRow) {
        if self.blocks == 0 {
            self.first_time = r.time;
            self.base_fee_min = r.base_fee_wei;
        }
        self.blocks += 1;
        self.last_time = r.time;
        self.txs += r.txs;
        self.failed += r.failed_txs;
        self.creations += r.contract_creations;
        self.gas_used += u128::from(r.gas_used);
        self.gas_limit += u128::from(r.gas_limit);
        self.base_fee += r.base_fee_wei;
        self.base_fee_min = self.base_fee_min.min(r.base_fee_wei);
        self.base_fee_max = self.base_fee_max.max(r.base_fee_wei);
        self.burned += r.burned_wei;
        self.priority += r.priority_fees_wei;
        self.blob_txs += r.blob_txs;
        self.blob_gas += u128::from(r.blob_gas_used);
        self.blob_fee += r.blob_fee_wei;
        self.value += r.value_wei;
    }

    fn json(&self) -> Value {
        json!({
            "blocks": self.blocks,
            "first_time": self.first_time,
            "last_time": self.last_time,
            "txs": self.txs,
            "failed_txs": self.failed,
            "contract_creations": self.creations,
            "gas_used": self.gas_used.to_string(),
            "fullness_ppm": ppm(self.gas_used, self.gas_limit),
            "mean_base_fee_wei": (self.base_fee / u128::from(self.blocks.max(1))).to_string(),
            "min_base_fee_wei": self.base_fee_min.to_string(),
            "max_base_fee_wei": self.base_fee_max.to_string(),
            "burned_wei": self.burned.to_string(),
            "priority_fees_wei": self.priority.to_string(),
            "blob_txs": self.blob_txs,
            "blob_gas_used": self.blob_gas.to_string(),
            "blob_fee_wei": self.blob_fee.to_string(),
            "value_wei": self.value.to_string(),
        })
    }
}

fn is_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// Check the window export (`static/onchain/eth-chain-stats.json`) against
/// the pins: recipe id, range and tip; every leaf decodes to an
/// `EthChainStatsRow` keyed by its number, numbers run exactly over the
/// pinned range, block by block, each linked to the block before by its
/// interval (each row's time minus the previous row's), days in date order
/// with each block on its day; one shard per block, and the commitment over
/// every shard's range and root is the pinned one. Returns the per-block
/// values and the window and per-day sums the page draws.
pub fn eth_chain_check(doc: &Value) -> Value {
    let mut problems: Vec<String> = Vec::new();
    if doc["recipe_id"].as_str() != Some(pins::ETH_CHAIN_RECIPE_ID) {
        problems.push("recipe id is not the pinned one".into());
    }
    if doc["tip"].as_str() != Some(pins::ETH_CHAIN_TIP)
        || doc["from"].as_u64() != Some(pins::ETH_CHAIN_FROM)
        || doc["to"].as_u64() != Some(pins::ETH_CHAIN_TO)
    {
        problems.push("range or tip is not the pinned one".into());
    }
    let mut shards = Vec::new();
    let mut blocks = Vec::new();
    let mut days_out = Vec::new();
    let mut window = Sums::default();
    let mut next = pins::ETH_CHAIN_FROM;
    let mut prev: Option<u64> = None;
    let mut tip = String::new();
    let mut last_date = String::new();
    let mut seen = 0usize;
    let mut checked: Vec<EthChainStatsRow> = Vec::new();
    let empty = Vec::new();
    for d in doc["days"].as_array().unwrap_or(&empty) {
        let date = d["date"].as_str().unwrap_or("?").to_string();
        if !is_date(&date) {
            problems.push(format!("day {date}: not a YYYY-MM-DD date"));
        }
        if date <= last_date {
            problems.push(format!("day {date}: not after the day before"));
        }
        if d["leaves"].as_array().is_none_or(|l| l.is_empty()) {
            problems.push(format!("day {date}: no blocks"));
        }
        last_date = date.clone();
        let mut sums = Sums::default();
        let mut day_ok = true;
        let (from, mut to) = (next, next);
        for l in d["leaves"].as_array().unwrap_or(&empty) {
            seen += 1;
            if seen > MAX_BLOCKS {
                problems.push("too many blocks".into());
                break;
            }
            let Some((leaf, r)) = l.as_str().and_then(leaf_row) else {
                day_ok = false;
                problems.push(format!("day {date}: a leaf does not decode"));
                continue;
            };
            if r.number != next || utc_date(r.time) != date {
                day_ok = false;
                problems.push(format!("day {date}: block {} out of place", r.number));
            }
            if let Some(t) = prev {
                if r.time.checked_sub(t) != Some(r.interval_secs) {
                    day_ok = false;
                    problems.push(format!("block {}: interval does not link", r.number));
                }
            }
            to = r.number;
            next = r.number.saturating_add(1);
            shards.push(EthShard {
                from: r.number,
                to: r.number,
                root: MerkleTree::from_rows(&[leaf]).root(),
            });
            tip = hex::encode(r.hash);
            prev = Some(r.time);
            sums.add(&r);
            window.add(&r);
            checked.push(r.clone());
            blocks.push(json!({
                "number": r.number,
                "hash": hex::encode(r.hash),
                "time": r.time,
                "interval_secs": r.interval_secs,
                "txs": r.txs,
                "failed_txs": r.failed_txs,
                "contract_creations": r.contract_creations,
                "gas_used": r.gas_used,
                "gas_limit": r.gas_limit,
                "fullness_ppm": r.fullness_ppm,
                "base_fee_wei": r.base_fee_wei.to_string(),
                "burned_wei": r.burned_wei.to_string(),
                "priority_fees_wei": r.priority_fees_wei.to_string(),
                "blob_txs": r.blob_txs,
                "blob_gas_used": r.blob_gas_used,
                "blob_base_fee_wei": r.blob_base_fee_wei.to_string(),
                "blob_fee_wei": r.blob_fee_wei.to_string(),
                "value_wei": r.value_wei.to_string(),
            }));
        }
        let mut v = sums.json();
        v["date"] = json!(date);
        v["from"] = json!(from);
        v["to"] = json!(to);
        v["ok"] = json!(day_ok && sums.blocks > 0);
        days_out.push(v);
    }
    if next != pins::ETH_CHAIN_TO.saturating_add(1) || shards.is_empty() {
        problems.push("the days do not cover the pinned range".into());
    }
    if tip != pins::ETH_CHAIN_TIP {
        problems.push("the last block is not the pinned tip".into());
    }
    let commitment = match (
        hex::decode(pins::ETH_CHAIN_RECIPE_ID.trim_start_matches("0x")),
        hex::decode(pins::ETH_CHAIN_TIP),
    ) {
        (Ok(r), Ok(t)) if r.len() == 32 && t.len() == 32 => {
            let mut rid = [0u8; 32];
            rid.copy_from_slice(&r);
            let mut tp = [0u8; 32];
            tp.copy_from_slice(&t);
            hex0x(&eth_chain_stats_commitment(&Hash32(rid), &tp, &shards).0)
        }
        _ => String::new(),
    };
    let commitment_ok = commitment == pins::ETH_CHAIN_COMMITMENT;
    // Bars: consecutive runs of GROUP_BLOCKS checked rows (the last may be shorter).
    let groups: Vec<Value> = checked
        .chunks(GROUP_BLOCKS)
        .map(|g| {
            let mut s = Sums::default();
            g.iter().for_each(|r| s.add(r));
            let mut v = s.json();
            v["from"] = json!(g[0].number);
            v["to"] = json!(g[g.len() - 1].number);
            v
        })
        .collect();
    if !commitment_ok {
        problems.push("the blocks' commitment is not the pinned one".into());
    }
    json!({
        "ok": problems.is_empty(),
        "commitment": commitment,
        "commitment_ok": commitment_ok,
        "recipe_id": pins::ETH_CHAIN_RECIPE_ID,
        "from": pins::ETH_CHAIN_FROM,
        "to": pins::ETH_CHAIN_TO,
        "tip": pins::ETH_CHAIN_TIP,
        "rerun_number": pins::ETH_CHAIN_RERUN_HEIGHT,
        "window": window.json(),
        "days": days_out,
        "group_blocks": GROUP_BLOCKS,
        "groups": groups,
        "blocks": blocks,
        "problems": problems,
    })
}

/// [`eth_chain_check`] on the export's JSON text.
#[wasm_bindgen]
pub fn check_eth_chain_stats(doc_json: &str) -> Result<String, JsError> {
    let doc: Value =
        serde_json::from_str(doc_json).map_err(|e| JsError::new(&format!("bad JSON: {e}")))?;
    Ok(eth_chain_check(&doc).to_string())
}

/// Rerun `eth-chain-stats` here on one block's full recipe input and compare
/// with the window: the recipe must be the pinned one, the input the pinned
/// rerun input (its block decoded from the bytes themselves), the run must
/// end Ok, and its root must equal that block's root in the checked window
/// (`eth_chain_check` of `doc`, which must pass). Every failure is a
/// problem, never an error.
pub fn eth_chain_rerun(recipe: &[u8], input: &[u8], doc: &Value) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let id = keccak256(recipe);
    if hex0x(&id.0) != pins::ETH_CHAIN_RECIPE_ID {
        problems.push("not the pinned recipe".into());
    }
    if hex0x(&keccak256(input).0) != pins::ETH_CHAIN_RERUN_INPUT {
        problems.push("not the pinned rerun input".into());
    }
    let src = RecipeInput::try_from_slice(input)
        .ok()
        .and_then(|i| EthChainSource::try_from_slice(&i.source).ok());
    let numbers: Vec<u64> = src
        .as_ref()
        .map(|s| s.blocks.iter().map(|b| b.number).collect())
        .unwrap_or_default();
    if numbers != [pins::ETH_CHAIN_RERUN_HEIGHT] {
        problems.push(format!("the input holds blocks {numbers:?}"));
    }
    if eth_chain_check(doc)["ok"] != true {
        problems.push("the window does not check out".into());
    }
    let mut want = String::new();
    for d in doc["days"].as_array().into_iter().flatten() {
        for l in d["leaves"].as_array().into_iter().flatten() {
            if let Some((leaf, row)) = l.as_str().and_then(leaf_row) {
                if row.number == pins::ETH_CHAIN_RERUN_HEIGHT {
                    want = hex0x(&MerkleTree::from_rows(&[leaf]).root().0);
                }
            }
        }
    }
    let run = Host::new().map_err(|e| e.to_string()).and_then(|h| {
        h.run(&id, recipe, input, &demo_profile())
            .map_err(|e| e.to_string())
    });
    let (outcome, fuel, root, row) = match run {
        Ok(r) => {
            let row = r
                .leaves
                .first()
                .and_then(|l| leaf_row(&hex::encode(l)))
                .map(|(_, v)| {
                    json!({"number": v.number, "txs": v.txs, "gas_used": v.gas_used,
                        "burned_wei": v.burned_wei.to_string(),
                        "priority_fees_wei": v.priority_fees_wei.to_string(),
                        "blob_txs": v.blob_txs})
                });
            (
                format!("{:?}", r.outcome),
                r.fuel_used.to_string(),
                hex0x(&r.root().0),
                row,
            )
        }
        Err(e) => (format!("not run: {e}"), "0".into(), String::new(), None),
    };
    if outcome != format!("{:?}", Outcome::Ok) {
        problems.push(format!("the recipe ended with {outcome}"));
    }
    if root != want {
        problems.push("the rerun's root is not the window's root for this block".into());
    }
    json!({
        "ok": problems.is_empty(),
        "outcome": outcome,
        "fuel": fuel,
        "recipe_id": hex0x(&id.0),
        "number": pins::ETH_CHAIN_RERUN_HEIGHT,
        "input_bytes": input.len(),
        "txs_in_input": src.as_ref().map_or(0, |s| s.blocks.iter().map(|b| b.txs.len()).sum::<usize>()),
        "root": root,
        "row": row,
        "problems": problems,
    })
}

/// [`eth_chain_rerun`] for the page: the recipe and input bytes, the window's JSON text.
#[wasm_bindgen]
pub fn rerun_eth_chain_block(
    recipe: &[u8],
    input: &[u8],
    doc_json: &str,
) -> Result<String, JsError> {
    let doc: Value =
        serde_json::from_str(doc_json).map_err(|e| JsError::new(&format!("bad JSON: {e}")))?;
    Ok(eth_chain_rerun(recipe, input, &doc).to_string())
}
