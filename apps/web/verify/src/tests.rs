//! Browser verifier tests (ADR-0020): the pinned values, series and day checks, and edited or
//! faked inputs that must fail.

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
    let golden =
        label_data("../../../fixtures/golden/btc-entity-flow-series-921080-921082.labels-v2.json");
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
    let ok = super::series_sums(&zero, &quiet, &root(&zero), &root(&quiet), 10, 12).expect("sums");
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["no_activity"], true);
    assert_eq!(
        ok["entities"][0]["points"].as_array().map(Vec::len),
        Some(0)
    );
    let busy = vec![leaf("entity:a", 10, 12, 5, 0, 1)];
    let bad = super::series_sums(&busy, &quiet, &root(&busy), &root(&quiet), 10, 12).expect("sums");
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
    serde_json::from_str(&label_data("../static/history/week/2026-10-10.json")).expect("day json")
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
        let o =
            super::OutputRow::try_from_slice(&hex::decode(l.as_str().expect("leaf")).expect("hex"))
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
        Box::new(|v| v["companies"][0]["reveals"][0]["observed_unix"] = 1_791_753_000u64.into()),
        Box::new(|v| {
            let b = v["companies"][0]["response_body"]
                .as_str()
                .unwrap_or_default()
                .replace("762099", "862099");
            v["companies"][0]["response_body"] = b.into();
        }),
        Box::new(|v| v["companies"][0]["feed"]["period_end"] = "2025-12-31".into()),
        Box::new(|v| v["companies"][0]["company"]["name"] = "Someone Else".into()),
        Box::new(|v| v["companies"][1]["result_hash"] = format!("0x{}", "22".repeat(32)).into()),
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
