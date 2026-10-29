//! The serving check of `btc.entity-reserve` (THREATS RES-7 and RES-1,
//! ADR-0034 Amendment 1) on the committed reserve history: the 89 shards of
//! `apps/web/static/history/reserves-week.json` (leaves that rebuild their
//! roots), the committed scan at block 971,199, label set v2 and the
//! committed cross-checks (`fixtures/reserves/cross-checks.json`).
//!
//! The check is non-circular: the expected params of every shard are derived
//! from the committed scan and the finalized rows' flows, never from a spec
//! or a base file. These tests feed it the shards as a server holds them and
//! check what it serves.
//!
//! Also THREATS RES-9: the per-address check at every height of a
//! derivation window, in Rust and in the Python recount.

use borsh::BorshDeserialize;
use onyx_core::{keccak256, Hash32, LabelSetRef};
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use onyx_source_btc::reserve::{
    check_params_chain, check_running_balances, cross_checked_entities, flows_from_jsonl,
    serving_check, AnchorLabels, BlockFlows, CrossChecks, FinalizedShard, HeaderChain,
    ReserveError, ReserveShard, ScanSnapshot, SecondScan, ServingAnchor, MIN_CONFIRMATIONS,
};
use onyx_source_btc::{load_label_set, verify_checksum, SCHEMA_ENTITY_RESERVE};
use std::collections::BTreeMap;
use std::path::PathBuf;

const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const SCAN: &str = "reserves/scan-971199.json";
const BASE: &str = "reserves/base-969838.json";
const CROSS: &str = "reserves/cross-checks.json";
const HEADERS: &str = "reserves/headers-969839-971207.json";
const START: u64 = 969_838;
/// 1,000 BTC.
const FORGED_EXTRA: u64 = 100_000_000_000;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixtures() -> PathBuf {
    root().join("fixtures")
}

fn json(rel: &str) -> serde_json::Value {
    verify_checksum(&fixtures(), rel).expect("checksum");
    serde_json::from_slice(&std::fs::read(fixtures().join(rel)).expect("read")).expect("json")
}

fn labels() -> LabelSet {
    load_label_set(&fixtures(), LABELS_V2).expect("labels v2")
}

fn scan(rel: &str) -> ScanSnapshot {
    serde_json::from_value(json(rel)).expect("scan")
}

/// The committed header chain 969,839-971,207 (RES-4), proof of work checked.
fn headers() -> &'static HeaderChain {
    static H: std::sync::OnceLock<HeaderChain> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        verify_checksum(&fixtures(), HEADERS).expect("checksum");
        HeaderChain::from_json(
            &std::fs::read_to_string(fixtures().join(HEADERS)).expect("read"),
            onyx_source_btc::node::MAINNET_MIN_BITS,
        )
        .expect("headers")
    })
}

fn cross() -> CrossChecks {
    serde_json::from_value(json(CROSS)).expect("cross-checks")
}

fn recipe_id() -> Hash32 {
    keccak256(&std::fs::read(root().join("recipes/out/btc_entity_reserve.wasm")).expect("wasm"))
}

fn set_hash(set: &LabelSet) -> Hash32 {
    LabelSetRef {
        name: set.name.clone(),
        version: set.version,
        content_hash: set.content_hash(),
    }
    .hash()
}

fn committed_base() -> Vec<EntityBase> {
    json(BASE)["base"]
        .as_array()
        .expect("base")
        .iter()
        .map(|b| EntityBase {
            entity: b["entity"].as_str().expect("entity").into(),
            balance_sats: b["balance_sats"]
                .as_str()
                .expect("sats")
                .parse()
                .expect("int"),
        })
        .collect()
}

fn leaves_of(rows: &[EntityReserveRow]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|r| {
            borsh::to_vec(&OutputRow {
                key: r.entity.as_bytes().to_vec(),
                value: borsh::to_vec(r).expect("row"),
            })
            .expect("leaf")
        })
        .collect()
}

/// The tip hash of a range ending at `to`: the header chain's block at that
/// height (display bytes, as `BlockRange::tip_hash` holds it). The export
/// carries no tip hashes; every range must end on the chain (RES-4).
fn tip_at(to: u64) -> Hash32 {
    let mut b = [0u8; 32];
    hex::decode_to_slice(headers().hash_at(to).expect("a header"), &mut b).expect("hash");
    Hash32(b)
}

/// The export's shards as a server holds them, each with the honest params
/// (the committed start, then the previous shard's end): what the exporter
/// put in each spec. Rounds count up from 1.
fn honest_chain(set: &LabelSet) -> Vec<FinalizedShard> {
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/reserves-week.json")).expect("export"),
    )
    .expect("export json");
    let committed = scan(SCAN);
    let snap = committed.hash();
    let (mut height, mut state, mut link) = (START, committed_base(), snap);
    let mut out = Vec::new();
    for (k, s) in doc["shards"].as_array().expect("shards").iter().enumerate() {
        let leaves: Vec<Vec<u8>> = s["leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(|l| hex::decode(l.as_str().expect("leaf").trim_start_matches("0x")).expect("hex"))
            .collect();
        let root = MerkleTree::from_rows(&leaves).root();
        let (from, to) = (s["from"].as_u64().expect("f"), s["to"].as_u64().expect("t"));
        let params = ReserveParams {
            base_height: height,
            base_ref: link.0,
            base: state.clone(),
        };
        let rows: Vec<EntityReserveRow> = leaves
            .iter()
            .map(|l| {
                let o = OutputRow::try_from_slice(l).expect("row");
                EntityReserveRow::try_from_slice(&o.value).expect("reserve row")
            })
            .collect();
        state = rows
            .iter()
            .map(|r| EntityBase {
                entity: r.entity.clone(),
                balance_sats: r.balance_sats,
            })
            .collect();
        height = to;
        link = root;
        out.push(FinalizedShard {
            round: k as u64 + 1,
            shard: keccak256(format!("shard {from}-{to}").as_bytes()),
            root,
            recipe: recipe_id(),
            label_set_hash: set_hash(set),
            schema: keccak256(SCHEMA_ENTITY_RESERVE),
            from,
            to,
            tip: tip_at(to),
            params: Some(borsh::to_vec(&params).expect("params")),
            leaves,
        });
    }
    out
}

/// A shard over the same blocks as `s` whose params name `extra` more sats
/// for `entity` at the start: the rerun of such a spec (the recipe is linear
/// in its base, review RES-R1's PoC) gives the same flows and every balance
/// of that entity `extra` higher, and miner and validators agree on it.
fn forged(s: &FinalizedShard, entity: &str, extra: u64, round: u64) -> FinalizedShard {
    let mut p = ReserveParams::try_from_slice(s.params.as_deref().expect("params")).expect("p");
    let mut rows: Vec<EntityReserveRow> = s
        .leaves
        .iter()
        .map(|l| {
            let o = OutputRow::try_from_slice(l).expect("row");
            EntityReserveRow::try_from_slice(&o.value).expect("reserve row")
        })
        .collect();
    for b in p.base.iter_mut().filter(|b| b.entity == entity) {
        b.balance_sats += extra;
    }
    for r in rows.iter_mut().filter(|r| r.entity == entity) {
        r.base_sats += extra;
        r.balance_sats += extra;
    }
    let leaves = leaves_of(&rows);
    FinalizedShard {
        round,
        shard: keccak256(format!("forged {}-{} {round}", s.from, s.to).as_bytes()),
        root: MerkleTree::from_rows(&leaves).root(),
        params: Some(borsh::to_vec(&p).expect("params")),
        leaves,
        ..s.clone()
    }
}

fn anchor<'a>(
    s: &'a ScanSnapshot,
    set: &'a LabelSet,
    c: &'a CrossChecks,
    second: &'a [SecondScan],
) -> ServingAnchor<'a> {
    ServingAnchor {
        scan: s,
        labels: AnchorLabels::Undated(set),
        headers: headers(),
        start_height: START,
        recipe: recipe_id(),
        cross_checks: c,
        second_scans: second,
    }
}

/// R-RES-SERVE-1: the committed chain is served; the start base the check
/// derives from the scan and the finalized flows alone is the committed
/// base; OKX and BITB rows carry their cross-check, Binance and BitMEX none.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_serving_check_serves_the_committed_chain() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let shards = honest_chain(&set);
    assert_eq!(shards.len(), 89);
    let v = serving_check(&anchor(&s, &set, &c, &[]), &shards).expect("check");
    assert_eq!(v.start_base, Some(committed_base()), "derived, not read");
    for (round, _, rows) in &v.shards {
        let rows = rows
            .as_ref()
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_eq!(rows.len(), 4);
        for r in rows {
            let want = matches!(r.entity.as_str(), "entity:okx" | "entity:bitwise-bitb");
            assert_eq!(r.cross_check.is_some(), want, "{}", r.entity);
        }
    }
    assert_eq!(
        v.cross_checks["entity:okx"]
            .as_ref()
            .map(|x| x.basis.as_str()),
        Some("published-por-file")
    );
    assert_eq!(
        v.cross_checks["entity:bitwise-bitb"]
            .as_ref()
            .map(|x| x.basis.as_str()),
        Some("issuer-wallet-list")
    );
    assert_eq!(v.cross_checks["entity:binance"], None);
    assert_eq!(v.cross_checks["entity:bitmex"], None);
}

/// R-RES-SERVE-2 (review RES-R2: the exporter's use of `check_params_chain`
/// is circular): a forged-base shard checked against its OWN base passes
/// `check_params_chain`; the serving check, which derives the base from the
/// scan and the finalized flows, refuses it, and still serves the honest
/// shard of the same blocks.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_forged_base_passes_a_circular_check_and_the_serving_check_refuses_it() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let mut shards = honest_chain(&set);
    let f = forged(&shards[0], "entity:okx", FORGED_EXTRA, 1_000);
    // Circular: the spec's own base as the "committed start".
    let p = ReserveParams::try_from_slice(f.params.as_deref().expect("p")).expect("p");
    let rows: Vec<EntityReserveRow> = f
        .leaves
        .iter()
        .map(|l| {
            EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                .expect("r")
        })
        .collect();
    let one = ReserveShard {
        from: f.from,
        to: f.to,
        root: f.root,
        rows,
    };
    assert!(
        check_params_chain(START, &p.base, &s.hash(), std::slice::from_ref(&p), &[one]).is_ok()
    );
    shards.push(f);
    let v = serving_check(&anchor(&s, &set, &c, &[]), &shards).expect("check");
    let (_, _, forged_verdict) = v.shards.last().expect("forged");
    let why = forged_verdict.as_ref().expect_err("refused");
    assert!(why.contains("not the previous finalized end"), "{why}");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    // Only the forged first shard is finalized (no honest one for its
    // blocks): the chain cannot start on the derived base, so nothing is
    // served.
    let mut all = honest_chain(&set);
    all.remove(0);
    let first = forged(&honest_chain(&set)[0], "entity:okx", FORGED_EXTRA, 2_000);
    all.insert(0, first);
    let v = serving_check(&anchor(&s, &set, &c, &[]), &all).expect("check");
    assert!(
        v.shards.iter().all(|(_, _, r)| r.is_err()),
        "nothing served"
    );
    assert_eq!(v.start_base, None);
}

/// R-RES-SERVE-3: the chain must reach the committed scan from the pinned
/// start; a missing shard, an overlapping range or a shard of another
/// recipe, label set or start stops or skips what it should.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_chain_must_reach_the_scan_and_tile_the_blocks() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let a = anchor(&s, &set, &c, &[]);
    // The last shard (it lands on the scan) is not finalized: nothing served.
    let mut shards = honest_chain(&set);
    shards.pop();
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards.iter().all(|(_, _, r)| r
        .as_ref()
        .is_err_and(|e| e.contains("does not reach the committed scan"))));
    // A gap in the middle: nothing served.
    let mut shards = honest_chain(&set);
    shards.remove(40);
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards.iter().all(|(_, _, r)| r.is_err()));
    // A shard whose params are not in the store, another recipe, another
    // label set: refused, and with the honest chain intact the rest is served.
    let mut shards = honest_chain(&set);
    let mut extra = shards[5].clone();
    extra.round = 500;
    extra.params = None;
    let mut other_recipe = shards[6].clone();
    other_recipe.round = 501;
    other_recipe.recipe = keccak256(b"another recipe");
    let mut other_set = shards[7].clone();
    other_set.round = 502;
    other_set.label_set_hash = keccak256(b"another set");
    shards.extend([extra, other_recipe, other_set]);
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    for (i, want) in [
        (89, "not in the store"),
        (90, "another recipe"),
        (91, "another label set"),
    ] {
        let e = v.shards[i].2.as_ref().expect_err("refused");
        assert!(e.contains(want), "{e}");
    }
    // Mutation L4: each range rule on its own, and right params with rows
    // that do not add up. A copy with its rows edited and re-rooted.
    let edit = |s: &FinalizedShard, round: u64, f: &dyn Fn(&mut EntityReserveRow)| {
        let rows: Vec<EntityReserveRow> = s
            .leaves
            .iter()
            .map(|l| {
                let mut r = EntityReserveRow::try_from_slice(
                    &OutputRow::try_from_slice(l).expect("o").value,
                )
                .expect("r");
                f(&mut r);
                r
            })
            .collect();
        let leaves = leaves_of(&rows);
        FinalizedShard {
            round,
            shard: keccak256(format!("edited {round}").as_bytes()),
            root: MerkleTree::from_rows(&leaves).root(),
            leaves,
            ..s.clone()
        }
    };
    let mut shards = honest_chain(&set);
    let mut at_start = edit(&shards[0], 700, &|r| r.base_height = START - 1);
    at_start.from = START;
    let mut reversed = edit(&shards[8], 701, &|_| {});
    reversed.to = reversed.from - 1;
    let wrong_base_height = edit(&shards[9], 702, &|r| r.base_height -= 1);
    let not_adding_up = edit(&shards[12], 703, &|r| {
        if r.entity == "entity:okx" {
            r.balance_sats += 1;
        }
    });
    shards.extend([at_start, reversed, wrong_base_height, not_adding_up]);
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    for (i, want) in [
        (89, "is not after the served chain's start"),
        (90, "is not after the served chain's start"),
        (91, "rows name another range than the source snapshot"),
        (92, "not the previous finalized end"),
    ] {
        let e = v.shards[i].2.as_ref().expect_err("refused");
        assert!(e.contains(want), "{i}: {e}");
    }
    // An overlapping range starting where another does: neither is on the
    // chain, so the chain stops before the scan and nothing is served.
    let mut shards = honest_chain(&set);
    let mut o = shards[10].clone();
    o.round = 600;
    o.to = shards[11].to;
    // On the header chain (RES-4), so only the overlap is wrong with it.
    o.tip = tip_at(o.to);
    let rows: Vec<EntityReserveRow> = o
        .leaves
        .iter()
        .map(|l| {
            let mut r =
                EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                    .expect("r");
            r.height = o.to;
            r
        })
        .collect();
    o.leaves = leaves_of(&rows);
    o.root = MerkleTree::from_rows(&o.leaves).root();
    shards.push(o);
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards.iter().all(|(_, _, r)| r.is_err()));
}

/// R-RES-SERVE-5 (review RESAPI-R1 F2, THREATS RES-4): the landing is
/// bound to the scan's block, not only its height. A landing shard over
/// another branch (same heights, another tip hash; synthetic) is refused
/// and is no link: alone, nothing is served; next to the honest landing,
/// only it is refused.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_landing_must_end_on_the_scans_block() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let a = anchor(&s, &set, &c, &[]);
    let honest = honest_chain(&set);
    let landing = honest.last().expect("landing").clone();
    assert_eq!(landing.to, s.height);
    let mut other = landing.clone();
    other.round = 700;
    other.shard = keccak256(b"the landing on another branch");
    other.tip = keccak256(b"another block at the scan's height");
    // Only the other branch's landing is finalized: nothing is served.
    let mut shards = honest.clone();
    shards.pop();
    shards.push(other.clone());
    let v = serving_check(&a, &shards).expect("check");
    assert!(
        v.shards.iter().all(|(_, _, r)| r.is_err()),
        "nothing served"
    );
    let e = v
        .shards
        .last()
        .expect("other")
        .2
        .as_ref()
        .expect_err("refused");
    assert!(e.contains("another block than the committed scan's"), "{e}");
    // Both: the honest chain is served, the other landing refused.
    let mut shards = honest;
    shards.push(other);
    let v = serving_check(&a, &shards).expect("check");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    assert!(v.shards[89].2.is_err());
}

/// R-RES-XCHK-1 (THREATS RES-1): a cross-check counts only for the scan and
/// label set it was recorded for, with every compared address exact; a
/// configured second-node re-scan counts per entity only where it
/// reconciles address by address. (Here the committed 971,202 scan is
/// configured as if it came from a second node; it is the same node, which
/// is why the demo configures none: the server cannot tell nodes apart.)
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn cross_checks_count_only_for_this_scan_and_where_they_agree() {
    let set = labels();
    let s = scan(SCAN);
    let mut c = cross();
    let a = |c: &CrossChecks, second: &[SecondScan]| {
        cross_checked_entities(&anchor(&s, &set, c, second)).expect("cross")
    };
    let none_of = |m: &BTreeMap<String, _>| -> Vec<String> {
        let m: &BTreeMap<String, Option<_>> = m;
        m.iter()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.clone())
            .collect::<Vec<String>>()
    };
    assert_eq!(none_of(&a(&c, &[])), ["entity:binance", "entity:bitmex"]);
    // A record for another scan does not count.
    let mut other = c.clone();
    other.scan_hash = format!("0x{}", "11".repeat(32));
    assert_eq!(none_of(&a(&other, &[])).len(), 4);
    // A record with one address off does not count.
    c.checks[0].addresses_exact -= 1;
    assert_eq!(
        none_of(&a(&c, &[])),
        ["entity:binance", "entity:bitmex", "entity:bitwise-bitb"]
    );
    c = cross();
    // A configured re-scan that reconciles covers every entity.
    let later = scan("reserves/scan-971202.json");
    let flows = flows("reserves/flows-971200-971202.jsonl");
    let second = vec![SecondScan {
        scan: later.clone(),
        flows: flows.clone(),
    }];
    let m = a(&c, &second);
    assert!(none_of(&m).is_empty());
    assert_eq!(
        m["entity:binance"].as_ref().map(|x| x.basis.as_str()),
        Some("second-node-rescan")
    );
    // Drop one Binance address's flow: Binance no longer reconciles.
    let owner: BTreeMap<&str, &str> = set
        .labels
        .iter()
        .map(|l| (l.address.as_str(), l.entity.as_str()))
        .collect();
    let mut bad = flows.clone();
    let victim = bad
        .iter()
        .find(|(addr, (rx, tx))| owner[addr.as_str()] == "entity:binance" && rx != tx)
        .map(|(addr, _)| addr.clone())
        .expect("a moved Binance address");
    bad.remove(&victim);
    let m = a(
        &c,
        &[SecondScan {
            scan: later.clone(),
            flows: bad,
        }],
    );
    assert_eq!(none_of(&m), ["entity:binance"]);
    // The same file as the committed scan is not a second scan.
    let m = a(
        &c,
        &[SecondScan {
            scan: s.clone(),
            flows: BTreeMap::new(),
        }],
    );
    assert_eq!(none_of(&m), ["entity:binance", "entity:bitmex"]);

    // Mutation L4: a record that compared no address does not count.
    let mut empty = cross();
    empty.checks[0].addresses_compared = 0;
    empty.checks[0].addresses_exact = 0;
    assert_eq!(
        none_of(&a(&empty, &[])),
        ["entity:binance", "entity:bitmex", "entity:bitwise-bitb"]
    );
    // Mutation L4: a re-scan at the same block (another method note) is
    // compared address by address, with no flows in between.
    let mut same = s.clone();
    same.method = "a re-scan of the same block".into();
    let at = |scan: &ScanSnapshot, flows: BTreeMap<String, (u128, u128)>| {
        a(
            &c,
            &[SecondScan {
                scan: scan.clone(),
                flows,
            }],
        )
    };
    assert!(none_of(&at(&same, BTreeMap::new())).is_empty());
    let mut off = same.clone();
    let i = off
        .rows
        .iter()
        .position(|r| r.entity == "entity:binance")
        .expect("a Binance row");
    off.rows[i].sats += 1;
    assert_eq!(none_of(&at(&off, BTreeMap::new())), ["entity:binance"]);
    // Refused outright (nothing added): another block at the same height,
    // another label set hash, flows at the same height, a lower height.
    let base = ["entity:binance", "entity:bitmex"];
    let mut other_block = same.clone();
    other_block.block_hash = "11".repeat(32);
    assert_eq!(none_of(&at(&other_block, BTreeMap::new())), base);
    let mut other_set = same.clone();
    other_set.label_set_hash = format!("0x{}", "22".repeat(32));
    assert_eq!(none_of(&at(&other_set, BTreeMap::new())), base);
    let one_flow: BTreeMap<String, (u128, u128)> = [(same.rows[0].address.clone(), (0, 0))].into();
    assert_eq!(none_of(&at(&same, one_flow)), base);
    let mut lower = same.clone();
    lower.height -= 1;
    assert_eq!(none_of(&at(&lower, BTreeMap::new())), base);
}

/// Mutation L4: `flows_from_jsonl` (the second-scan flows file the reserve
/// server loads) sums every block's flows per address, and refuses a block
/// that does not follow the one before.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_flows_file_parses_and_must_be_contiguous() {
    let rel = "reserves/flows-971200-971202.jsonl";
    let text = std::fs::read_to_string(fixtures().join(rel)).expect("flows");
    let f = flows_from_jsonl(&text).expect("parse");
    assert_eq!(f.flows, flows(rel));
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("line"))
        .collect();
    assert_eq!(f.height, 971_202);
    assert_eq!(Some(f.hash.as_str()), lines[2]["hash"].as_str());
    assert_eq!(Some(f.parent.as_str()), lines[0]["prev"].as_str());
    let join = |ls: &[serde_json::Value]| {
        ls.iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut gap = lines.clone();
    gap[1]["height"] = serde_json::json!(971_202);
    assert!(flows_from_jsonl(&join(&gap)).is_err());
    let mut fork = lines.clone();
    fork[2]["prev"] = serde_json::json!("00".repeat(32));
    assert!(flows_from_jsonl(&join(&fork)).is_err());
    // Blank lines are skipped.
    assert_eq!(flows_from_jsonl(&format!("\n{text}\n\n")), Ok(f));
}

fn flows(rel: &str) -> BTreeMap<String, (u128, u128)> {
    verify_checksum(&fixtures(), rel).expect("checksum");
    let mut out: BTreeMap<String, (u128, u128)> = BTreeMap::new();
    for line in std::fs::read_to_string(fixtures().join(rel))
        .expect("flows")
        .lines()
    {
        let b: serde_json::Value = serde_json::from_str(line).expect("line");
        for (a, v) in b["flows"].as_object().expect("flows") {
            let e = out.entry(format!("btc:{a}")).or_default();
            e.0 += u128::from(v[0].as_u64().expect("rx"));
            e.1 += u128::from(v[1].as_u64().expect("tx"));
        }
    }
    out
}

/// Per-block flows of a committed flows file, keyed `btc:<address>`.
fn blocks(rel: &str) -> Vec<BlockFlows> {
    verify_checksum(&fixtures(), rel).expect("checksum");
    std::fs::read_to_string(fixtures().join(rel))
        .expect("flows")
        .lines()
        .map(|line| {
            let b: serde_json::Value = serde_json::from_str(line).expect("line");
            let f = b["flows"]
                .as_object()
                .expect("flows")
                .iter()
                .map(|(a, v)| {
                    (
                        format!("btc:{a}"),
                        (
                            u128::from(v[0].as_u64().expect("rx")),
                            u128::from(v[1].as_u64().expect("tx")),
                        ),
                    )
                })
                .collect();
            (b["height"].as_u64().expect("height"), f)
        })
        .collect()
}

fn python_derive(
    snap: &ScanSnapshot,
    lines: &[serde_json::Value],
    base_height: u64,
) -> std::process::Output {
    let dir =
        std::env::temp_dir().join(format!("onyx-res9-{}-{}", std::process::id(), lines.len()));
    std::fs::create_dir_all(&dir).expect("dir");
    let sp = dir.join("scan.json");
    std::fs::write(&sp, serde_json::to_vec(snap).expect("json")).expect("write");
    let fp = dir.join("flows.jsonl");
    let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();
    std::fs::write(&fp, text).expect("write");
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/recount-entity-reserve.py"))
        .arg("derive")
        .arg(&sp)
        .arg(fixtures().join(LABELS_V2))
        .arg(&fp)
        .arg(base_height.to_string())
        .arg(dir.join("base.json"))
        .output()
        .expect("python3 runs");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// R-RES-NEG-3 (THREATS RES-9) on real data: the later scan rolled back
/// block by block over the committed flows never goes negative at any
/// address (Rust and Python), and Python's base equals the earlier scan.
/// An address the scan left out that sent coins and got them back inside
/// the window passes the base-height check (its net flow is 0) and fails
/// this one; a flow at an address outside the set is refused by both.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_9_every_height_is_checked_per_address_in_rust_and_python() {
    let set = labels();
    let later = scan("reserves/scan-971202.json");
    let real = blocks("reserves/flows-971200-971202.jsonl");
    assert_eq!(check_running_balances(&later, &set, &real), Ok(()));
    let lines: Vec<serde_json::Value> =
        std::fs::read_to_string(fixtures().join("reserves/flows-971200-971202.jsonl"))
            .expect("flows")
            .lines()
            .map(|l| serde_json::from_str(l).expect("line"))
            .collect();
    let out = python_derive(&later, &lines, 971_199);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // An omitted address (no scan row) that sends 5 sats at 971,201 and
    // receives them back at 971,202.
    let held: std::collections::BTreeSet<&str> =
        later.rows.iter().map(|r| r.address.as_str()).collect();
    let omitted = set
        .labels
        .iter()
        .map(|l| l.address.clone())
        .find(|a| !held.contains(a.as_str()) && real.iter().all(|(_, f)| !f.contains_key(a)))
        .expect("an empty, unmoved label address");
    let mut dip = real.clone();
    dip[1].1.insert(omitted.clone(), (0, 5));
    dip[2].1.insert(omitted.clone(), (5, 0));
    let err = check_running_balances(&later, &set, &dip).expect_err("caught");
    assert!(err.to_string().contains(&omitted), "{err}");
    let mut dip_lines = lines.clone();
    let bare = omitted.trim_start_matches("btc:");
    dip_lines[1]["flows"][bare] = serde_json::json!([0, 5]);
    dip_lines[2]["flows"][bare] = serde_json::json!([5, 0]);
    let out = python_derive(&later, &dip_lines, 971_199);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("negative after block"));
    // Outside the set.
    let mut outside = real.clone();
    outside[0]
        .1
        .insert("btc:1BitcoinEaterAddressDontSendf59kuE".into(), (1, 0));
    assert!(check_running_balances(&later, &set, &outside).is_err());
    let mut outside_lines = lines.clone();
    outside_lines[0]["flows"]["1BitcoinEaterAddressDontSendf59kuE"] = serde_json::json!([1, 0]);
    let out = python_derive(&later, &outside_lines, 971_199);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("outside the label set"));
}

// ------------------------------------------------- RES-4 (ADR-0037): depth

fn rows_of(s: &FinalizedShard) -> Vec<EntityReserveRow> {
    s.leaves
        .iter()
        .map(|l| {
            EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                .expect("r")
        })
        .collect()
}

/// RES-4: the committed header chain (own node's raw blocks to 971,199,
/// 971,200-971,207 from two public explorers that agree, three of them also
/// matching our own node's flows file) passes proof of work and links; a
/// changed nonce, a missing header, a harder floor or another version fail.
/// Confirmations count the block itself (Bitcoin Core): the tip has 1.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_the_header_chain_is_checked_for_work_and_links() {
    let h = headers();
    assert_eq!((h.first(), h.tip()), (969_839, 971_207));
    assert_eq!(h.confirmations(971_199, &scan(SCAN).block_hash), Ok(9));
    let later = scan("reserves/scan-971202.json");
    assert_eq!(
        h.confirmations(971_202, &later.block_hash),
        Ok(MIN_CONFIRMATIONS)
    );
    let tip = h.hash_at(971_207).expect("tip").to_string();
    assert_eq!(h.confirmations(971_207, &tip), Ok(1));
    assert_eq!(
        h.confirmations(971_199, &"00".repeat(32)),
        Err(ReserveError::StaleBranch(971_199))
    );
    assert_eq!(
        h.confirmations(971_208, &tip),
        Err(ReserveError::NoHeaders(971_208))
    );
    assert_eq!(
        h.confirmations(969_838, &tip),
        Err(ReserveError::NoHeaders(969_838))
    );
    assert_eq!(
        h.require_confirmed(971_203, h.hash_at(971_203).expect("h"), MIN_CONFIRMATIONS),
        Err(ReserveError::Unconfirmed {
            height: 971_203,
            have: 5,
            need: 6
        })
    );
    let text = std::fs::read_to_string(fixtures().join(HEADERS)).expect("read");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("json");
    let floor = onyx_source_btc::node::MAINNET_MIN_BITS;
    let with = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut d = doc.clone();
        f(&mut d);
        HeaderChain::from_json(&d.to_string(), floor)
    };
    let nonce = with(&|d| {
        let x = d["headers"][1300].as_str().expect("h").to_string();
        d["headers"][1300] = serde_json::json!(format!("{}00000000", &x[..152]));
    });
    assert!(nonce.is_err_and(|e| e.to_string().contains("proof of work")));
    let gap = with(&|d| {
        d["headers"].as_array_mut().expect("a").remove(10);
    });
    assert!(gap.is_err_and(|e| e.to_string().contains("does not link")));
    let v2 = with(&|d| d["version"] = serde_json::json!(2));
    assert!(v2.is_err());
    // A floor harder than these blocks' own target (bits 0x17021ef0).
    let hard = HeaderChain::from_json(&text, 0x1701_0000);
    assert!(hard.is_err_and(|e| e.to_string().contains("easier than the pinned difficulty")));
}

/// RES-20 (review D1-R2): the floor is a quarter of the real difficulty, so
/// headers checked against it alone could be mined four times cheaper than
/// the blocks they stand in for. Inside a difficulty period every header must
/// keep the bits of the one before; at a period's first block the target
/// moves at most 4x either way. A real mainnet header of another period
/// (block 921,081, bits 0x1701eb21: valid proof of work, above the floor)
/// spliced into the chain is refused for its bits before its link is read.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_20_headers_keep_their_period_difficulty() {
    use onyx_source_btc::node::{permitted_bits_transition as ok, DIFFICULTY_PERIOD};
    let text = std::fs::read_to_string(fixtures().join(HEADERS)).expect("read");
    let mut doc: serde_json::Value = serde_json::from_str(&text).expect("json");
    let other = json("btc/blocks-921080-921082.json")["blocks"][1]["header"]
        .as_str()
        .expect("header")
        .to_string();
    assert_eq!(&other[144..152], "21eb0117", "bits 0x1701eb21");
    doc["headers"][10] = serde_json::json!(other);
    let e = HeaderChain::from_json(&doc.to_string(), onyx_source_btc::node::MAINNET_MIN_BITS)
        .expect_err("another period's difficulty");
    assert!(e.to_string().contains("969849: difficulty bits"), "{e}");
    assert!(e.to_string().contains("retarget rule"), "{e}");
    // A header claiming difficulty 1 (bits 0x1d00ffff) is under the floor.
    let mut cheap: serde_json::Value = serde_json::from_str(&text).expect("json");
    let x = cheap["headers"][10].as_str().expect("h").to_string();
    cheap["headers"][10] = serde_json::json!(format!("{}ffff001d{}", &x[..144], &x[152..]));
    let e = HeaderChain::from_json(&cheap.to_string(), onyx_source_btc::node::MAINNET_MIN_BITS)
        .expect_err("low work");
    assert!(
        e.to_string().contains("easier than the pinned difficulty"),
        "{e}"
    );
    // The rule itself, around the committed chain's bits.
    let b = 0x1702_1ef0;
    let mid = 969_849;
    assert!(ok(mid, b, b));
    assert!(
        !ok(mid, b, onyx_source_btc::node::MAINNET_MIN_BITS),
        "the floor mid-period"
    );
    assert!(!ok(mid, b, 0x1701_eb21), "harder mid-period");
    let start = 482 * DIFFICULTY_PERIOD;
    assert!(ok(start, b, 0x1708_7bc0), "4x easier");
    assert!(!ok(start, b, 0x1708_7bc1), "more than 4x easier");
    assert!(ok(start, b, 0x1700_87bc), "4x harder");
    assert!(!ok(start, b, 0x1700_87bb), "more than 4x harder");
    assert!(
        !ok(start + 1, b, 0x1700_87bc),
        "only at the period's first block"
    );
}

/// RES-4: the committed scan is used only with 6 confirmations. Cut the
/// header chain at 971,203 (5 on top of nothing: 971,199..971,203) and the
/// whole check refuses; at 971,204 it serves the committed chain.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_the_scan_needs_six_confirmations() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let shards = honest_chain(&set);
    let six = headers().truncated(971_204).expect("cut");
    let v = serving_check(
        &ServingAnchor {
            headers: &six,
            ..anchor(&s, &set, &c, &[])
        },
        &shards,
    )
    .expect("6 confirmations");
    assert!(v.shards.iter().all(|(_, _, r)| r.is_ok()));
    let five = headers().truncated(971_203).expect("cut");
    assert_eq!(
        serving_check(
            &ServingAnchor {
                headers: &five,
                ..anchor(&s, &set, &c, &[])
            },
            &shards,
        ),
        Err(ReserveError::Unconfirmed {
            height: 971_199,
            have: 5,
            need: 6
        })
    );
    // A re-scan counts as a cross-check only with 6 confirmations too.
    let second = vec![SecondScan {
        scan: scan("reserves/scan-971202.json"),
        flows: flows("reserves/flows-971200-971202.jsonl"),
    }];
    let count = |h: &HeaderChain| {
        cross_checked_entities(&ServingAnchor {
            headers: h,
            ..anchor(&s, &set, &c, &second)
        })
        .expect("cross")
        .values()
        .filter(|v| v.is_some())
        .count()
    };
    assert_eq!(count(headers()), 4, "971,202 has 6");
    assert_eq!(
        count(&headers().truncated(971_206).expect("cut")),
        2,
        "5: ignored"
    );
}

/// RES-4 (reorg before the landing): every range must end on the header
/// chain's block, not only the landing. A shard whose last block is on a
/// stale branch is refused; alone (no honest shard of those blocks) the
/// chain stops there and nothing is served; next to the honest one only it
/// is refused. A range past the header chain has no evidence and is refused.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_a_stale_branch_before_the_landing_is_refused() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let a = anchor(&s, &set, &c, &[]);
    let honest = honest_chain(&set);
    let mut stale = honest[40].clone();
    stale.round = 900;
    stale.shard = keccak256(b"blocks of shard 40 on a stale branch");
    stale.tip = keccak256(b"a block reorged out at this height");
    let mut alone = honest.clone();
    alone[40] = stale.clone();
    let v = serving_check(&a, &alone).expect("check");
    assert!(
        v.shards.iter().all(|(_, _, r)| r.is_err()),
        "nothing served"
    );
    let e = v.shards[40].2.as_ref().expect_err("refused");
    assert!(e.contains("a stale branch (RES-4)"), "{e}");
    let mut both = honest.clone();
    both.push(stale);
    let v = serving_check(&a, &both).expect("check");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    assert!(v.shards[89]
        .2
        .as_ref()
        .is_err_and(|e| e.contains("stale branch")));
    // The chain cut before shard 40's last block: no evidence for it.
    let to = honest[40].to;
    let cut = headers().truncated(to - 1).expect("cut");
    let short = ServingAnchor { headers: &cut, ..a };
    // (the scan itself then has no header either: refused outright)
    assert_eq!(
        serving_check(&short, &honest),
        Err(ReserveError::NoHeaders(971_199))
    );
}

/// RES-4: a shard after the scan is served only once its last block has 6
/// confirmations. The post-scan shard here is 971,200-971,202 with the real
/// per-entity flows of the committed flows file; it lands on the committed
/// 971,202 scan's totals exactly. With the header chain at 971,207 (6
/// confirmations) it is served; cut at 971,206 (5) it is refused and the
/// chain up to the scan is still served.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_a_shard_after_the_scan_needs_six_confirmations() {
    let set = labels();
    let s = scan(SCAN);
    let c = cross();
    let mut shards = honest_chain(&set);
    let landing = shards.last().expect("landing").clone();
    let owner: BTreeMap<&str, &str> = set
        .labels
        .iter()
        .map(|l| (l.address.as_str(), l.entity.as_str()))
        .collect();
    let mut per: BTreeMap<&str, (u128, u128)> = BTreeMap::new();
    for (a, (rx, tx)) in flows("reserves/flows-971200-971202.jsonl") {
        let e = per.entry(owner[a.as_str()]).or_default();
        e.0 += rx;
        e.1 += tx;
    }
    let base = rows_of(&landing);
    let rows: Vec<EntityReserveRow> = base
        .iter()
        .map(|r| {
            let (rx, tx) = per.get(r.entity.as_str()).copied().unwrap_or((0, 0));
            EntityReserveRow {
                entity: r.entity.clone(),
                base_height: 971_199,
                height: 971_202,
                base_ref: landing.root.0,
                base_sats: r.balance_sats,
                received_sats: rx,
                sent_sats: tx,
                tx_count: 0,
                balance_sats: u64::try_from(i128::from(r.balance_sats) + rx as i128 - tx as i128)
                    .expect("non-negative"),
            }
        })
        .collect();
    // The real flows land on the later scan's totals.
    let later = scan("reserves/scan-971202.json");
    let ends: Vec<EntityBase> = rows
        .iter()
        .map(|r| EntityBase {
            entity: r.entity.clone(),
            balance_sats: r.balance_sats,
        })
        .collect();
    assert_eq!(ends, later.entity_totals(&set).expect("totals"));
    let params = ReserveParams {
        base_height: 971_199,
        base_ref: landing.root.0,
        base: base
            .iter()
            .map(|r| EntityBase {
                entity: r.entity.clone(),
                balance_sats: r.balance_sats,
            })
            .collect(),
    };
    let leaves = leaves_of(&rows);
    shards.push(FinalizedShard {
        round: 90,
        shard: keccak256(b"after the scan 971200-971202"),
        root: MerkleTree::from_rows(&leaves).root(),
        from: 971_200,
        to: 971_202,
        tip: tip_at(971_202),
        params: Some(borsh::to_vec(&params).expect("params")),
        leaves,
        ..landing.clone()
    });
    let v = serving_check(&anchor(&s, &set, &c, &[]), &shards).expect("check");
    assert!(
        v.shards.iter().all(|(_, _, r)| r.is_ok()),
        "6 confirmations"
    );
    let five = headers().truncated(971_206).expect("cut");
    let v = serving_check(
        &ServingAnchor {
            headers: &five,
            ..anchor(&s, &set, &c, &[])
        },
        &shards,
    )
    .expect("check");
    assert!(v.shards[..89].iter().all(|(_, _, r)| r.is_ok()));
    let e = v.shards[89].2.as_ref().expect_err("refused");
    assert!(
        e.contains("block 971202 has 5 confirmations") && e.contains("6 needed"),
        "{e}"
    );
}

/// RES-4: a flows file is used only on the header chain and confirmed: its
/// blocks are the chain's, its first block's parent is the chain's block
/// before, and its last block has 6 confirmations.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_a_flows_file_must_be_on_the_header_chain_and_confirmed() {
    let rel = "reserves/flows-971200-971202.jsonl";
    let text = std::fs::read_to_string(fixtures().join(rel)).expect("flows");
    let f = flows_from_jsonl(&text).expect("parse");
    assert_eq!(f.blocks.len(), 3);
    assert_eq!(f.check_on_chain(headers(), MIN_CONFIRMATIONS), Ok(()));
    let five = headers().truncated(971_206).expect("cut");
    assert_eq!(
        f.check_on_chain(&five, MIN_CONFIRMATIONS),
        Err(ReserveError::Unconfirmed {
            height: 971_202,
            have: 5,
            need: 6
        })
    );
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("line"))
        .collect();
    let join = |ls: &[serde_json::Value]| {
        ls.iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    // Another block at 971,201 (and its child names it): a stale branch.
    let mut fork = lines.clone();
    fork[1]["hash"] = serde_json::json!("11".repeat(32));
    fork[2]["prev"] = serde_json::json!("11".repeat(32));
    let ff = flows_from_jsonl(&join(&fork)).expect("contiguous");
    assert_eq!(
        ff.check_on_chain(headers(), MIN_CONFIRMATIONS),
        Err(ReserveError::StaleBranch(971_201))
    );
    // A file whose first block names another parent (a reorg at its edge).
    let mut edge = lines.clone();
    edge[0]["prev"] = serde_json::json!("22".repeat(32));
    let ff = flows_from_jsonl(&join(&edge)).expect("parse");
    assert_eq!(
        ff.check_on_chain(headers(), MIN_CONFIRMATIONS),
        Err(ReserveError::StaleBranch(971_199))
    );
}
