//! The browser's Ethereum window check (ADR-0044): the exported window checks
//! out, its figures come from the checked leaves, every edit a JSON-only
//! attacker can make fails it, and the rerun of the shipped block input on
//! the canonical engine gives that block's pinned root.

use borsh::BorshDeserialize;
use onyx_eth_schema::EthChainStatsRow;
use onyx_recipe_sdk::OutputRow;
use onyx_web_verify::eth_chain::{eth_chain_check, eth_chain_rerun, pins};
use serde_json::Value;

const DOC: &str = include_str!("../../static/onchain/eth-chain-stats.json");

fn doc() -> Value {
    serde_json::from_str(DOC).expect("export")
}

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
}

fn leaf(d: &Value, i: usize) -> (OutputRow, EthChainStatsRow) {
    let hexed = d["days"][0]["leaves"][i].as_str().expect("leaf");
    let leaf = hex::decode(hexed.trim_start_matches("0x")).expect("hex");
    let row = OutputRow::try_from_slice(&leaf).expect("row");
    let v = EthChainStatsRow::try_from_slice(&row.value).expect("stats row");
    (row, v)
}

fn put(d: &mut Value, i: usize, mut row: OutputRow, v: &EthChainStatsRow) {
    row.value = borsh::to_vec(v).expect("value");
    d["days"][0]["leaves"][i] = Value::String(format!(
        "0x{}",
        hex::encode(borsh::to_vec(&row).expect("row"))
    ));
}

#[test]
fn the_exported_window_checks_out_and_its_sums_are_the_rows() {
    let d = doc();
    let r = eth_chain_check(&d);
    assert_eq!(r["ok"], true, "{}", r["problems"]);
    assert_eq!(r["commitment_ok"], true);
    let n = d["days"][0]["leaves"].as_array().expect("leaves").len();
    assert_eq!(n as u64, pins::ETH_CHAIN_TO - pins::ETH_CHAIN_FROM + 1);
    assert_eq!(r["blocks"].as_array().expect("blocks").len(), n);
    let (mut txs, mut burned, mut value) = (0u64, 0u128, 0u128);
    for i in 0..n {
        let v = leaf(&d, i).1;
        txs += v.txs;
        burned += v.burned_wei;
        value += v.value_wei;
    }
    assert_eq!(r["window"]["txs"].as_u64(), Some(txs));
    assert_eq!(
        r["window"]["burned_wei"].as_str(),
        Some(burned.to_string().as_str())
    );
    assert_eq!(
        r["window"]["value_wei"].as_str(),
        Some(value.to_string().as_str())
    );
    assert_eq!(r["days"][0]["txs"].as_u64(), Some(txs));
    // The bars: consecutive groups of 25 blocks that add up to the window.
    let groups = r["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), n.div_ceil(25));
    assert_eq!(
        groups
            .iter()
            .map(|g| g["txs"].as_u64().expect("txs"))
            .sum::<u64>(),
        txs
    );
    assert_eq!(groups[0]["from"].as_u64(), Some(pins::ETH_CHAIN_FROM));
    assert_eq!(
        groups[groups.len() - 1]["to"].as_u64(),
        Some(pins::ETH_CHAIN_TO)
    );
}

#[test]
fn every_json_only_edit_of_the_window_fails() {
    let fails = |d: &Value, why: &str| {
        let r = eth_chain_check(d);
        assert_eq!(r["ok"], false, "{why}");
    };
    // One field of one row, re-encoded so it still decodes.
    let mut d = doc();
    let (row, mut v) = leaf(&d, 40);
    v.burned_wei += 1;
    put(&mut d, 40, row, &v);
    fails(&d, "a field");
    assert_eq!(eth_chain_check(&d)["commitment_ok"], false);
    // A row moved to another number (key and row together).
    let mut d = doc();
    let (mut row, mut v) = leaf(&d, 40);
    v.number += 1;
    row.key = EthChainStatsRow::key(v.number);
    put(&mut d, 40, row, &v);
    fails(&d, "a moved row");
    // A time moved (the interval no longer links).
    let mut d = doc();
    let (row, mut v) = leaf(&d, 40);
    v.time += 1;
    put(&mut d, 40, row, &v);
    fails(&d, "a time");
    // A leaf dropped, duplicated, two swapped, one undecodable.
    let mut d = doc();
    d["days"][0]["leaves"]
        .as_array_mut()
        .expect("leaves")
        .remove(7);
    fails(&d, "a dropped leaf");
    let mut d = doc();
    let l = d["days"][0]["leaves"][7].clone();
    d["days"][0]["leaves"]
        .as_array_mut()
        .expect("leaves")
        .insert(7, l);
    fails(&d, "a duplicated leaf");
    let mut d = doc();
    d["days"][0]["leaves"]
        .as_array_mut()
        .expect("leaves")
        .swap(7, 8);
    fails(&d, "two swapped leaves");
    let mut d = doc();
    d["days"][0]["leaves"][9] = Value::String("0x00".into());
    fails(&d, "an undecodable leaf");
    // The day renamed, an empty day added, the range or tip or recipe changed.
    let mut d = doc();
    d["days"][0]["date"] = Value::String("2026-10-18".into());
    fails(&d, "a day renamed");
    let mut d = doc();
    d["days"]
        .as_array_mut()
        .expect("days")
        .push(serde_json::json!({"date": "2026-10-20", "leaves": []}));
    fails(&d, "an empty day");
    let mut d = doc();
    d["to"] = Value::from(pins::ETH_CHAIN_TO + 1);
    fails(&d, "the range");
    let mut d = doc();
    d["recipe_id"] = Value::String(format!("0x{}", "11".repeat(32)));
    fails(&d, "the recipe id");
}

fn shipped() -> (Vec<u8>, Vec<u8>) {
    let wasm = std::fs::read(root().join("recipes/out/eth_chain_stats.wasm")).expect("recipe");
    let b64 =
        std::fs::read_to_string(root().join("apps/web/static/onchain/eth-rerun-input.b64.txt"))
            .expect("input");
    let input = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())
        .expect("base64");
    (wasm, input)
}

#[test]
fn the_shipped_block_reruns_to_its_pinned_root() {
    let (wasm, input) = shipped();
    let r = eth_chain_rerun(&wasm, &input, &doc());
    assert_eq!(r["ok"], true, "{}", r["problems"]);
    assert_eq!(r["number"].as_u64(), Some(pins::ETH_CHAIN_RERUN_HEIGHT));
    assert!(r["txs_in_input"].as_u64().expect("txs") >= 100);
    assert!(r["row"]["blob_txs"].as_u64().expect("blobs") >= 1);
}

#[test]
fn an_edited_input_recipe_or_window_fails_the_rerun() {
    let (wasm, input) = shipped();
    let d = doc();
    // One byte of the input (inside the source).
    let mut bad = input.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 1;
    assert_eq!(eth_chain_rerun(&wasm, &bad, &d)["ok"], false);
    // Another recipe.
    let other = std::fs::read(root().join("recipes/out/test_clock.wasm")).expect("other recipe");
    assert_eq!(eth_chain_rerun(&other, &input, &d)["ok"], false);
    // A window that does not check out.
    let mut e = d.clone();
    e["days"][0]["leaves"]
        .as_array_mut()
        .expect("leaves")
        .remove(3);
    assert_eq!(eth_chain_rerun(&wasm, &input, &e)["ok"], false);
}
