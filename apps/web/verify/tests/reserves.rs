//! The browser's reserve-history check (ADR-0034): the exported week checks
//! out, and every edit a JSON-only attacker can make fails it.

use borsh::BorshDeserialize;
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::EntityReserveRow;
use onyx_web_verify::check_reserves;
use serde_json::Value;

/// The exported reserve week: label-derived, so left out of a public snapshot; read when a test
/// runs, and the tests are ignored without the `label-data` feature (pre-public loop PP13).
fn doc_text() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../static/history/reserves-week.json");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn check(d: &Value) -> Value {
    let out = check_reserves(&d.to_string()).unwrap_or_else(|_| panic!("check errored"));
    serde_json::from_str(&out).expect("json")
}

fn doc() -> Value {
    serde_json::from_str(&doc_text()).expect("export")
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_exported_week_checks_out() {
    let r = check(&doc());
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["commitment_ok"], true);
    assert_eq!(r["derived_base_ok"], true, "ROADMAP D3");
    let entities = r["entities"].as_array().expect("entities");
    assert_eq!(entities.len(), 4);
    // One point for the base plus one per week shard.
    let week = r["week_shards"].as_u64().expect("week shards") as usize;
    for e in entities {
        assert_eq!(e["points"].as_array().expect("points").len(), week + 1);
    }
}

/// Re-encode leaf `i` of shard `s` with its balance moved, rebuilding the
/// shard's root so only the chain (or the commitment) can catch it.
fn edit_balance(doc: &mut Value, s: usize, i: usize, delta: i64, fix_root: bool) {
    let hexed = doc["shards"][s]["leaves"][i]
        .as_str()
        .expect("leaf")
        .to_string();
    let leaf = hex::decode(hexed.trim_start_matches("0x")).expect("hex");
    let mut row = OutputRow::try_from_slice(&leaf).expect("row");
    let mut v = EntityReserveRow::try_from_slice(&row.value).expect("reserve row");
    v.balance_sats = (v.balance_sats as i64 + delta) as u64;
    v.received_sats = (v.received_sats as i128 + i128::from(delta)) as u128;
    row.value = borsh::to_vec(&v).expect("value");
    doc["shards"][s]["leaves"][i] = Value::String(format!(
        "0x{}",
        hex::encode(borsh::to_vec(&row).expect("row"))
    ));
    if fix_root {
        let leaves: Vec<Vec<u8>> = doc["shards"][s]["leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(|l| hex::decode(l.as_str().expect("l").trim_start_matches("0x")).expect("hex"))
            .collect();
        doc["shards"][s]["root"] = Value::String(MerkleTree::from_rows(&leaves).root().to_string());
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn every_json_only_edit_fails() {
    // A leaf edited alone: its root no longer matches.
    let mut d = doc();
    edit_balance(&mut d, 3, 0, 1, false);
    assert_eq!(check(&d)["ok"], false);
    // A self-consistent shard (root rebuilt, arithmetic kept): the next shard's
    // base and link no longer match, and the commitment moves.
    let mut d = doc();
    edit_balance(&mut d, 3, 0, 1, true);
    let r = check(&d);
    assert_eq!(r["ok"], false);
    assert_eq!(r["commitment_ok"], false);
    // The base moved by one satoshi.
    let mut d = doc();
    let b = d["base"]["rows"][0]["balance_sats"]
        .as_str()
        .expect("b")
        .parse::<u64>()
        .expect("n");
    d["base"]["rows"][0]["balance_sats"] = Value::String((b + 1).to_string());
    assert_eq!(check(&d)["ok"], false);
    // The scan totals moved.
    let mut d = doc();
    d["snapshot"]["totals"][1]["balance_sats"] = Value::String("1".into());
    assert_eq!(check(&d)["ok"], false);
    // A shard dropped, or two swapped.
    let mut d = doc();
    d["shards"].as_array_mut().expect("shards").remove(10);
    assert_eq!(check(&d)["ok"], false);
    let mut d = doc();
    d["shards"].as_array_mut().expect("shards").swap(10, 11);
    assert_eq!(check(&d)["ok"], false);
    // Another snapshot, recipe or week.
    for (k, v) in [
        ("/snapshot/hash", format!("0x{}", "11".repeat(32))),
        ("/recipe_id", format!("0x{}", "22".repeat(32))),
        ("/week/tip", "00".repeat(32)),
    ] {
        let mut d = doc();
        *d.pointer_mut(k).expect("field") = Value::String(v);
        assert_eq!(check(&d)["ok"], false, "{k}");
    }
}

/// ROADMAP D3 (THREATS RES-10): a whole chain on a forged base, edited the
/// only way that keeps every link (OKX's base and every OKX balance raised by
/// 1,000 BTC, every root rebuilt, every `base_ref` re-linked): the chain
/// checks against the export's own base (circular), and the client serving
/// check in the browser, which derives the base from the pinned scan and the
/// shards' flows, refuses it by name.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_forged_base_chain_fails_the_browsers_client_serving_check() {
    const EXTRA: u64 = 100_000_000_000;
    let mut d = doc();
    let okx = |v: &Value| v["entity"].as_str() == Some("entity:okx");
    for b in d["base"]["rows"].as_array_mut().expect("base") {
        if okx(b) {
            let n: u64 = b["balance_sats"].as_str().expect("s").parse().expect("n");
            b["balance_sats"] = Value::String((n + EXTRA).to_string());
        }
    }
    let mut prev: Option<String> = None;
    for s in d["shards"].as_array_mut().expect("shards") {
        let mut leaves: Vec<Vec<u8>> = s["leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(|l| hex::decode(l.as_str().expect("l").trim_start_matches("0x")).expect("hex"))
            .collect();
        for leaf in leaves.iter_mut() {
            let mut row = OutputRow::try_from_slice(leaf).expect("row");
            let mut v = EntityReserveRow::try_from_slice(&row.value).expect("reserve row");
            if v.entity == "entity:okx" {
                v.base_sats += EXTRA;
                v.balance_sats += EXTRA;
            }
            if let Some(p) = &prev {
                hex::decode_to_slice(p.trim_start_matches("0x"), &mut v.base_ref).expect("ref");
            }
            row.value = borsh::to_vec(&v).expect("value");
            *leaf = borsh::to_vec(&row).expect("leaf");
        }
        let root = MerkleTree::from_rows(&leaves).root().to_string();
        s["leaves"] = Value::Array(
            leaves
                .iter()
                .map(|l| Value::String(format!("0x{}", hex::encode(l))))
                .collect(),
        );
        s["root"] = Value::String(root.clone());
        prev = Some(root);
    }
    let r = check(&d);
    assert_eq!(r["ok"], false);
    assert_eq!(r["derived_base_ok"], false);
    let problems: Vec<&str> = r["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        problems.iter().all(|p| !p.starts_with("chain:")),
        "the links hold: {problems:?}"
    );
    assert!(
        problems.contains(&"client serving check: chain: 969839-969852: entity:okx: base is not the one derived from the committed scan and the finalized flows"),
        "{problems:?}"
    );
    // The other entities' bases still derive.
    assert!(
        !problems.iter().any(|p| p.contains("entity:binance")),
        "{problems:?}"
    );
}
