//! The browser's checks of the on-chain window (ADR-0043, website /data
//! "Bitcoin on chain"): the exported `btc-chain-stats` leaves against the
//! pinned commitment, the per-day figures the page draws (computed here from
//! the checked rows, never read from the file), and a rerun of the recipe on
//! one block's full input on the canonical engine.

use super::{demo_profile, hex0x, parse, pinned};
use borsh::BorshDeserialize;
use onyx_chain_schema::{ChainSource, ChainStatsRow};
use onyx_core::{keccak256, Hash32};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{Host, Outcome};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::chain::{chain_stats_commitment, ChainShard};
use onyx_source_btc::history::utc_date;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// Most leaves one window may carry (a month of blocks is about 4,400).
const MAX_BLOCKS: usize = 20_000;

fn leaf_row(hex_leaf: &str) -> Option<(Vec<u8>, ChainStatsRow)> {
    let leaf = hex::decode(hex_leaf.trim_start_matches("0x")).ok()?;
    let row = OutputRow::try_from_slice(&leaf).ok()?;
    let v = ChainStatsRow::try_from_slice(&row.value).ok()?;
    (row.key == ChainStatsRow::key(v.height)).then_some((leaf, v))
}

/// Integer share in parts per million (0 when the whole is 0).
fn ppm(part: u128, whole: u128) -> u64 {
    (part * 1_000_000)
        .checked_div(whole)
        .map_or(0, |v| u64::try_from(v).unwrap_or(u64::MAX))
}

/// Sums of one day (or the window) over checked rows.
#[derive(Default)]
struct Sums {
    blocks: u64,
    first_time: u32,
    last_time: u32,
    txs: u64,
    inputs: u64,
    outputs: u64,
    size: u128,
    weight: u128,
    interval: i128,
    subsidy: u128,
    fees: u128,
    issued: i128,
    transfer: u128,
    segwit: u64,
    tr_in: u64,
    tr_out: u64,
    destroyed: u128,
}

impl Sums {
    fn add(&mut self, r: &ChainStatsRow) {
        if self.blocks == 0 {
            self.first_time = r.time;
        }
        self.blocks += 1;
        self.last_time = r.time;
        self.txs += r.txs;
        self.inputs += r.inputs;
        self.outputs += r.outputs;
        self.size += u128::from(r.size_bytes);
        self.weight += u128::from(r.weight_wu);
        self.interval += i128::from(r.interval_secs);
        self.subsidy += u128::from(r.subsidy_sats);
        self.fees += u128::from(r.fees_sats);
        self.issued += i128::from(r.issued_sats);
        self.transfer += r.transfer_sats;
        self.segwit += r.segwit_txs;
        self.tr_in += r.taproot_inputs;
        self.tr_out += r.taproot_outputs;
        self.destroyed += r.coin_blocks_destroyed;
    }

    fn json(&self) -> Value {
        let b = u128::from(self.blocks.max(1));
        json!({
            "blocks": self.blocks,
            "first_time": self.first_time,
            "last_time": self.last_time,
            "txs": self.txs,
            "inputs": self.inputs,
            "outputs": self.outputs,
            "mean_size_bytes": (self.size / b).to_string(),
            "fullness_ppm": ppm(self.weight, b * 4_000_000),
            "mean_interval_secs": (self.interval / i128::from(self.blocks.max(1))).to_string(),
            "subsidy_sats": self.subsidy.to_string(),
            "fees_sats": self.fees.to_string(),
            "issued_sats": self.issued.to_string(),
            "transfer_sats": self.transfer.to_string(),
            "segwit_ppm": ppm(u128::from(self.segwit), u128::from(self.txs.saturating_sub(self.blocks))),
            "taproot_inputs_ppm": ppm(u128::from(self.tr_in), u128::from(self.inputs)),
            "taproot_outputs_ppm": ppm(u128::from(self.tr_out), u128::from(self.outputs)),
            "coin_blocks_destroyed": self.destroyed.to_string(),
        })
    }
}

/// Check the on-chain window export (`static/onchain/btc-chain-stats.json`)
/// against the pins: recipe id, range and tip; every leaf decodes to a
/// `ChainStatsRow` keyed by its height, heights run exactly over the pinned
/// range, block by block, each linked to the block before it by its interval
/// (each row's time minus the previous row's), days in date order with each
/// block on or before its day's date; one shard per block, and the
/// commitment over every shard's range and root is the pinned one. Returns
/// the per-block values and the per-day and window sums the page draws.
pub fn chain_stats_check(doc: &Value) -> Value {
    let mut problems: Vec<String> = Vec::new();
    if doc["recipe_id"].as_str() != Some(pinned::CHAIN_RECIPE_ID) {
        problems.push("recipe id is not the pinned one".into());
    }
    if doc["tip"].as_str() != Some(pinned::CHAIN_TIP)
        || doc["from"].as_u64() != Some(pinned::CHAIN_FROM)
        || doc["to"].as_u64() != Some(pinned::CHAIN_TO)
    {
        problems.push("range or tip is not the pinned one".into());
    }
    let mut shards = Vec::new();
    let mut blocks = Vec::new();
    let mut days_out = Vec::new();
    let mut window = Sums::default();
    let mut next = pinned::CHAIN_FROM;
    let mut prev: Option<u32> = None;
    let mut tip = String::new();
    let mut last_date = String::new();
    let mut leaves_seen = 0usize;
    let empty = Vec::new();
    for d in doc["days"].as_array().unwrap_or(&empty) {
        let date = d["date"].as_str().unwrap_or("?").to_string();
        let well_formed = date.len() == 10
            && date.bytes().enumerate().all(|(i, c)| {
                if i == 4 || i == 7 {
                    c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            });
        if !well_formed {
            problems.push(format!("day {date}: not a YYYY-MM-DD date"));
        }
        if date <= last_date {
            problems.push(format!("day {date}: not after the day before"));
        }
        // P3A-R1-1: a day with no block would draw an empty bar no row backs.
        if d["leaves"].as_array().is_none_or(|l| l.is_empty()) {
            problems.push(format!("day {date}: no blocks"));
        }
        last_date = date.clone();
        let mut sums = Sums::default();
        let mut day_ok = true;
        let (from, mut to) = (next, next);
        for (k, l) in d["leaves"].as_array().unwrap_or(&empty).iter().enumerate() {
            leaves_seen += 1;
            if leaves_seen > MAX_BLOCKS {
                problems.push("too many blocks".into());
                break;
            }
            let Some((leaf, r)) = l.as_str().and_then(leaf_row) else {
                day_ok = false;
                problems.push(format!("day {date}: a leaf does not decode"));
                continue;
            };
            let r_date = utc_date(u64::from(r.time));
            if r.height != next || r_date > date || (k == 0 && r_date != date) {
                day_ok = false;
                problems.push(format!("day {date}: block {} out of place", r.height));
            }
            if let Some(t) = prev {
                if i64::from(r.time) - i64::from(t) != r.interval_secs {
                    day_ok = false;
                    problems.push(format!("block {}: interval does not link", r.height));
                }
            }
            to = r.height;
            next = r.height.saturating_add(1);
            shards.push(ChainShard {
                from: r.height,
                to: r.height,
                root: MerkleTree::from_rows(&[leaf]).root(),
            });
            tip = hex::encode(r.hash);
            prev = Some(r.time);
            sums.add(&r);
            window.add(&r);
            blocks.push(json!({
                "height": r.height,
                "hash": hex::encode(r.hash),
                "time": r.time,
                "interval_secs": r.interval_secs,
                "txs": r.txs,
                "weight_wu": r.weight_wu,
                "fees_sats": r.fees_sats.to_string(),
                "fee_rate_median_msat_vb": r.fee_rate_median_msat_vb,
                "fee_rate_avg_msat_vb": r.fee_rate_avg_msat_vb,
                "active_scripts": r.active_scripts,
            }));
        }
        let mut v = sums.json();
        v["date"] = json!(date);
        v["from"] = json!(from);
        v["to"] = json!(to);
        v["ok"] = json!(day_ok && sums.blocks > 0);
        days_out.push(v);
    }
    if next != pinned::CHAIN_TO.saturating_add(1) || shards.is_empty() {
        problems.push("the days do not cover the pinned range".into());
    }
    if tip != pinned::CHAIN_TIP {
        problems.push("the last block is not the pinned tip".into());
    }
    let commitment = match (
        hex::decode(pinned::CHAIN_RECIPE_ID.trim_start_matches("0x")),
        hex::decode(pinned::CHAIN_TIP),
    ) {
        (Ok(r), Ok(t)) if r.len() == 32 && t.len() == 32 => {
            let mut rid = [0u8; 32];
            rid.copy_from_slice(&r);
            let mut tp = [0u8; 32];
            tp.copy_from_slice(&t);
            hex0x(&chain_stats_commitment(&Hash32(rid), &tp, &shards).0)
        }
        _ => String::new(),
    };
    let commitment_ok = commitment == pinned::CHAIN_COMMITMENT;
    if !commitment_ok {
        problems.push("the blocks' commitment is not the pinned one".into());
    }
    json!({
        "ok": problems.is_empty(),
        "commitment": commitment,
        "commitment_ok": commitment_ok,
        "recipe_id": pinned::CHAIN_RECIPE_ID,
        "from": pinned::CHAIN_FROM,
        "to": pinned::CHAIN_TO,
        "window": window.json(),
        "days": days_out,
        "blocks": blocks,
        "problems": problems,
    })
}

/// [`chain_stats_check`] on the export's JSON text.
#[wasm_bindgen]
pub fn check_chain_stats(doc_json: &str) -> Result<String, JsError> {
    Ok(chain_stats_check(&parse(doc_json)?).to_string())
}

/// Rerun `btc-chain-stats` here on one block's full recipe input and compare
/// with the window: the recipe must be the pinned one, the input the pinned
/// rerun input (its block decoded from the bytes themselves), the run must
/// end Ok, and its root must equal that block's root in the checked window
/// (`chain_stats_check` of `doc`, which must pass). Returns the outcome, fuel,
/// root and the rerun's row; every failure is a problem, never an error.
pub fn chain_rerun(recipe: &[u8], input: &[u8], doc: &Value) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let id = keccak256(recipe);
    if hex0x(&id.0) != pinned::CHAIN_RECIPE_ID {
        problems.push("not the pinned recipe".into());
    }
    if hex0x(&keccak256(input).0) != pinned::CHAIN_RERUN_INPUT {
        problems.push("not the pinned rerun input".into());
    }
    let src = RecipeInput::try_from_slice(input)
        .ok()
        .and_then(|i| ChainSource::try_from_slice(&i.source).ok());
    let heights: Vec<u64> = src
        .as_ref()
        .map(|s| s.blocks.iter().map(|b| b.height).collect())
        .unwrap_or_default();
    if heights != [pinned::CHAIN_RERUN_HEIGHT] {
        problems.push(format!("the input holds blocks {heights:?}"));
    }
    if chain_stats_check(doc)["ok"] != true {
        problems.push("the window does not check out".into());
    }
    let mut want = String::new();
    for d in doc["days"].as_array().into_iter().flatten() {
        for l in d["leaves"].as_array().into_iter().flatten() {
            if let Some((leaf, row)) = l.as_str().and_then(leaf_row) {
                if row.height == pinned::CHAIN_RERUN_HEIGHT {
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
                    json!({"height": v.height, "txs": v.txs, "fees_sats": v.fees_sats.to_string(),
                        "fee_rate_median_msat_vb": v.fee_rate_median_msat_vb,
                        "active_scripts": v.active_scripts})
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
        "height": pinned::CHAIN_RERUN_HEIGHT,
        "input_bytes": input.len(),
        "txs_in_input": src.as_ref().map_or(0, |s| s.blocks.iter().map(|b| b.txs.len()).sum::<usize>()),
        "root": root,
        "row": row,
        "problems": problems,
    })
}

/// [`chain_rerun`] for the page: the recipe and input bytes, the window's JSON text.
#[wasm_bindgen]
pub fn rerun_chain_block(recipe: &[u8], input: &[u8], doc_json: &str) -> Result<String, JsError> {
    Ok(chain_rerun(recipe, input, &parse(doc_json)?).to_string())
}
