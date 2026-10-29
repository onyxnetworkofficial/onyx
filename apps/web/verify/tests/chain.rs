//! The browser's on-chain window check (ADR-0043): the exported window checks
//! out, its figures come from the checked leaves, every edit a JSON-only
//! attacker can make fails it, and the rerun of the shipped block input on
//! the canonical engine gives that block's pinned root.

use borsh::BorshDeserialize;
use onyx_chain_schema::ChainStatsRow;
use onyx_recipe_sdk::OutputRow;
use onyx_web_verify::chain::{chain_rerun, chain_stats_check};
use serde_json::Value;

const DOC: &str = include_str!("../../static/onchain/btc-chain-stats.json");

fn doc() -> Value {
    serde_json::from_str(DOC).expect("export")
}

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
}

fn leaf(d: &Value, day: usize, i: usize) -> (OutputRow, ChainStatsRow) {
    let hexed = d["days"][day]["leaves"][i].as_str().expect("leaf");
    let leaf = hex::decode(hexed.trim_start_matches("0x")).expect("hex");
    let row = OutputRow::try_from_slice(&leaf).expect("row");
    let v = ChainStatsRow::try_from_slice(&row.value).expect("stats row");
    (row, v)
}

fn put(d: &mut Value, day: usize, i: usize, mut row: OutputRow, v: &ChainStatsRow) {
    row.value = borsh::to_vec(v).expect("value");
    d["days"][day]["leaves"][i] = Value::String(format!(
        "0x{}",
        hex::encode(borsh::to_vec(&row).expect("row"))
    ));
}

#[test]
fn the_exported_window_checks_out_and_its_sums_are_the_rows() {
    let d = doc();
    let r = chain_stats_check(&d);
    assert_eq!(r["ok"], true, "{}", r["problems"]);
    assert_eq!(r["commitment_ok"], true);
    let blocks = r["blocks"].as_array().expect("blocks");
    let span = d["to"].as_u64().expect("to") - d["from"].as_u64().expect("from") + 1;
    assert_eq!(blocks.len() as u64, span);
    assert_eq!(r["window"]["blocks"].as_u64(), Some(span));
    // The window's transaction count is the sum of the leaves' rows.
    let mut txs = 0u64;
    for (k, day) in d["days"].as_array().expect("days").iter().enumerate() {
        for i in 0..day["leaves"].as_array().expect("leaves").len() {
            txs += leaf(&d, k, i).1.txs;
        }
    }
    assert_eq!(r["window"]["txs"].as_u64(), Some(txs));
    let day_txs: u64 = r["days"]
        .as_array()
        .expect("days")
        .iter()
        .map(|x| x["txs"].as_u64().expect("txs"))
        .sum();
    assert_eq!(day_txs, txs);
}

/// R-CHAIN-5: every figure the page draws, per day and for the window, recomputed here from the
/// leaves by its definition (integers, rounded down); and each block's drawn fields are its row's.
#[test]
fn every_drawn_figure_is_its_definition_over_the_rows() {
    let d = doc();
    let r = chain_stats_check(&d);
    assert_eq!(r["ok"], true, "{}", r["problems"]);
    let days = d["days"].as_array().expect("days");
    let per_day: Vec<Vec<ChainStatsRow>> = (0..days.len())
        .map(|k| {
            (0..days[k]["leaves"].as_array().expect("leaves").len())
                .map(|i| leaf(&d, k, i).1)
                .collect()
        })
        .collect();
    let all: Vec<ChainStatsRow> = per_day.iter().flatten().cloned().collect();
    let expect = |rows: &[ChainStatsRow], got: &Value, what: &str| {
        let n = rows.len() as u128;
        let sum = |f: &dyn Fn(&ChainStatsRow) -> u128| rows.iter().map(f).sum::<u128>();
        let txs = sum(&|x| u128::from(x.txs));
        let (ins, outs) = (
            sum(&|x| u128::from(x.inputs)),
            sum(&|x| u128::from(x.outputs)),
        );
        let interval: i128 = rows.iter().map(|x| i128::from(x.interval_secs)).sum();
        let issued: i128 = rows.iter().map(|x| i128::from(x.issued_sats)).sum();
        let want = serde_json::json!({
            "blocks": n as u64,
            "first_time": rows[0].time,
            "last_time": rows[rows.len() - 1].time,
            "txs": txs as u64,
            "inputs": ins as u64,
            "outputs": outs as u64,
            "mean_size_bytes": (sum(&|x| u128::from(x.size_bytes)) / n).to_string(),
            "fullness_ppm": (sum(&|x| u128::from(x.weight_wu)) * 1_000_000 / (n * 4_000_000)) as u64,
            "mean_interval_secs": (interval / n as i128).to_string(),
            "subsidy_sats": sum(&|x| u128::from(x.subsidy_sats)).to_string(),
            "fees_sats": sum(&|x| u128::from(x.fees_sats)).to_string(),
            "issued_sats": issued.to_string(),
            "transfer_sats": sum(&|x| x.transfer_sats).to_string(),
            "segwit_ppm": (sum(&|x| u128::from(x.segwit_txs)) * 1_000_000 / (txs - n)) as u64,
            "taproot_inputs_ppm": (sum(&|x| u128::from(x.taproot_inputs)) * 1_000_000 / ins) as u64,
            "taproot_outputs_ppm": (sum(&|x| u128::from(x.taproot_outputs)) * 1_000_000 / outs) as u64,
            "coin_blocks_destroyed": sum(&|x| x.coin_blocks_destroyed).to_string(),
        });
        for (key, v) in want.as_object().expect("object") {
            assert_eq!(&got[key], v, "{what}: {key}");
        }
    };
    expect(&all, &r["window"], "window");
    let out_days = r["days"].as_array().expect("days");
    assert_eq!(out_days.len(), per_day.len());
    for (rows, got) in per_day.iter().zip(out_days) {
        expect(rows, got, got["date"].as_str().unwrap_or("?"));
        assert_eq!(got["from"].as_u64(), Some(rows[0].height));
        assert_eq!(got["to"].as_u64(), Some(rows[rows.len() - 1].height));
    }
    // Each block's drawn values are its own row's.
    for (b, x) in r["blocks"].as_array().expect("blocks").iter().zip(&all) {
        assert_eq!(b["height"].as_u64(), Some(x.height));
        assert_eq!(b["time"].as_u64(), Some(u64::from(x.time)));
        assert_eq!(b["interval_secs"].as_i64(), Some(x.interval_secs));
        assert_eq!(b["txs"].as_u64(), Some(x.txs));
        assert_eq!(b["weight_wu"].as_u64(), Some(x.weight_wu));
        assert_eq!(
            b["fees_sats"].as_str(),
            Some(x.fees_sats.to_string().as_str())
        );
        assert_eq!(
            b["fee_rate_median_msat_vb"].as_u64(),
            Some(x.fee_rate_median_msat_vb)
        );
        assert_eq!(
            b["fee_rate_avg_msat_vb"].as_u64(),
            Some(x.fee_rate_avg_msat_vb)
        );
        assert_eq!(b["active_scripts"].as_u64(), Some(x.active_scripts));
        assert_eq!(b["hash"].as_str(), Some(hex::encode(x.hash).as_str()));
    }
    assert_eq!(r["blocks"].as_array().map(Vec::len), Some(all.len()));
}

#[test]
fn every_json_only_edit_of_the_window_fails() {
    // One field of one row, re-encoded so it still decodes.
    let mut d = doc();
    let (row, mut v) = leaf(&d, 1, 3);
    v.fees_sats += 1;
    put(&mut d, 1, 3, row, &v);
    let r = chain_stats_check(&d);
    assert_eq!(r["ok"], false);
    assert_eq!(r["commitment_ok"], false);
    // A row moved to another height (key and row together).
    let mut d = doc();
    let (mut row, mut v) = leaf(&d, 1, 3);
    v.height += 1;
    row.key = ChainStatsRow::key(v.height);
    put(&mut d, 1, 3, row, &v);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    // A time edited (the interval no longer links).
    let mut d = doc();
    let (row, mut v) = leaf(&d, 2, 5);
    v.time += 60;
    put(&mut d, 2, 5, row, &v);
    let r = chain_stats_check(&d);
    assert_eq!(r["ok"], false);
    // A leaf dropped, duplicated, a day removed, two days swapped.
    let mut d = doc();
    d["days"][2]["leaves"].as_array_mut().expect("l").remove(0);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    let mut d = doc();
    let dup = d["days"][2]["leaves"][0].clone();
    d["days"][2]["leaves"]
        .as_array_mut()
        .expect("l")
        .insert(0, dup);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    let mut d = doc();
    d["days"].as_array_mut().expect("days").remove(4);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    let mut d = doc();
    d["days"].as_array_mut().expect("days").swap(1, 2);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    // P3A-R1-1: an empty day inserted between two real ones, or a date that is no date.
    let mut d = doc();
    let empty = serde_json::json!({"date": format!("{}~", d["days"][1]["date"].as_str().expect("date")), "leaves": []});
    d["days"].as_array_mut().expect("days").insert(2, empty);
    assert_eq!(chain_stats_check(&d)["ok"], false);
    let mut d = doc();
    let last = d["days"].as_array().expect("days").len();
    d["days"]
        .as_array_mut()
        .expect("days")
        .push(serde_json::json!({"date": "2099-12-31", "leaves": []}));
    assert_eq!(
        chain_stats_check(&d)["ok"],
        false,
        "an empty day after the last"
    );
    assert!(last > 0);
    // A block moved to the next day's date.
    let mut d = doc();
    d["days"][3]["date"] = Value::String("2099-01-01".into());
    assert_eq!(chain_stats_check(&d)["ok"], false);
    // Another recipe, range or tip.
    for (k, v) in [
        ("recipe_id", Value::String("0x00".into())),
        ("tip", Value::String("00".into())),
        ("from", Value::from(1)),
        ("to", Value::from(1)),
    ] {
        let mut d = doc();
        d[k] = v;
        assert_eq!(chain_stats_check(&d)["ok"], false, "{k}");
    }
    // Fields the page could read but the check ignores change nothing drawn.
    let mut d = doc();
    d["max_fuel"] = Value::from(1);
    d["days"][0]["txs"] = Value::from(999_999_999u64);
    let r = chain_stats_check(&d);
    assert_eq!(r["ok"], true);
    assert_ne!(r["days"][0]["txs"].as_u64(), Some(999_999_999));
}

#[test]
fn the_shipped_block_reruns_to_its_pinned_root() {
    let d = doc();
    let wasm = std::fs::read(root().join("recipes/out/btc_chain_stats.wasm")).expect("recipe");
    let b64 = std::fs::read_to_string(root().join("apps/web/static/onchain/rerun-input.b64.txt"))
        .expect("rerun input");
    let input = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())
        .expect("base64");
    let r = chain_rerun(&wasm, &input, &d);
    assert_eq!(r["ok"], true, "{}", r["problems"]);
    assert_eq!(r["outcome"], "Ok");
    assert_eq!(r["root"], d["rerun"]["root"]);
    // Another input (one byte changed) or another recipe is refused.
    for at in [input.len() - 1, input.len() / 2] {
        let mut bad = input.clone();
        bad[at] ^= 1;
        assert_eq!(chain_rerun(&wasm, &bad, &d)["ok"], false, "byte {at}");
    }
    let mut other = wasm.clone();
    other.push(0);
    assert_eq!(chain_rerun(&other, &input, &d)["ok"], false);
}
