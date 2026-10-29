//! The browser's pool-history check (ADR-0032): the exported week checks
//! out, and every edit a JSON-only attacker can make fails it.

use borsh::BorshDeserialize;
use onyx_merkle::MerkleTree;
use onyx_pool_schema::PoolBlocksRow;
use onyx_recipe_sdk::OutputRow;
use onyx_web_verify::check_pools;
use serde_json::Value;

const DOC: &str = include_str!("../../static/history/pools-week.json");

fn check(d: &Value) -> Value {
    let out = check_pools(&d.to_string()).unwrap_or_else(|_| panic!("check errored"));
    serde_json::from_str(&out).expect("json")
}

fn doc() -> Value {
    serde_json::from_str(DOC).expect("export")
}

#[test]
fn the_exported_week_checks_out() {
    let r = check(&doc());
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["commitment_ok"], true);
    let total: u64 = r["totals"]
        .as_object()
        .expect("totals")
        .values()
        .filter_map(Value::as_u64)
        .sum();
    assert_eq!(total, 1_008);
    assert_eq!(r["days"].as_array().expect("days").len(), 7);
}

/// Re-encode a leaf of day `d` with `blocks` changed, keeping it decodable.
fn edit_leaf(doc: &mut Value, d: usize, i: usize, blocks_delta: i64) {
    let hexed = doc["days"][d]["leaves"][i]
        .as_str()
        .expect("leaf")
        .to_string();
    let leaf = hex::decode(hexed.trim_start_matches("0x")).expect("hex");
    let mut row = OutputRow::try_from_slice(&leaf).expect("row");
    let mut v = PoolBlocksRow::try_from_slice(&row.value).expect("pool row");
    v.blocks = (v.blocks as i64 + blocks_delta) as u64;
    row.value = borsh::to_vec(&v).expect("value");
    doc["days"][d]["leaves"][i] = Value::String(format!(
        "0x{}",
        hex::encode(borsh::to_vec(&row).expect("row"))
    ));
}

#[test]
fn every_json_only_edit_fails() {
    // A leaf edited alone: its root no longer matches.
    let mut d = doc();
    edit_leaf(&mut d, 2, 0, 1);
    assert_eq!(check(&d)["ok"], false);
    // A self-consistent forgery: move one block between two pools and rebuild
    // the day's root. Only the pinned commitment catches it.
    let mut d = doc();
    edit_leaf(&mut d, 2, 0, 1);
    edit_leaf(&mut d, 2, 1, -1);
    let leaves: Vec<Vec<u8>> = d["days"][2]["leaves"]
        .as_array()
        .expect("leaves")
        .iter()
        .map(|l| hex::decode(l.as_str().expect("s").trim_start_matches("0x")).expect("hex"))
        .collect();
    d["days"][2]["root"] = Value::String(MerkleTree::from_rows(&leaves).root().to_string());
    let r = check(&d);
    assert_eq!(r["ok"], false);
    assert_eq!(r["commitment_ok"], false);
    // Two days swapped, one dropped, one duplicated.
    let mut d = doc();
    let days = d["days"].as_array_mut().expect("days");
    days.swap(1, 2);
    assert_eq!(check(&d)["ok"], false);
    let mut d = doc();
    d["days"].as_array_mut().expect("days").remove(3);
    assert_eq!(check(&d)["ok"], false);
    let mut d = doc();
    let dup = d["days"][4].clone();
    d["days"].as_array_mut().expect("days").insert(4, dup);
    assert_eq!(check(&d)["ok"], false);
    // Another recipe, table or tip.
    for (k, v) in [("recipe_id", "0x00"), ("table_hash", "0x00"), ("tip", "00")] {
        let mut d = doc();
        d[k] = Value::String(v.into());
        assert_eq!(check(&d)["ok"], false, "{k}");
    }
    // The informational `rows` are never trusted: editing them changes nothing
    // the page draws (it draws the checked leaves).
    let mut d = doc();
    d["days"][0]["rows"][0]["blocks"] = Value::from(999);
    let r = check(&d);
    assert_eq!(r["ok"], true);
    assert_ne!(r["days"][0]["rows"][0]["blocks"], 999);
}
