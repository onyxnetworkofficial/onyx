//! Dated membership in the reserve serving check (ADR-0037, THREATS RES-2),
//! with its adversarial cases (back-dated membership, a shard across a
//! change, an address in two entities, a reorg at the range edge, forged
//! flows), and the client check on dated chains (ROADMAP D22).
//!
//! A synthetic world on real block heights: the committed header chain
//! (`fixtures/reserves/headers-969839-971207.json`) gives every block its
//! real hash; addresses, amounts and rows are synthetic (`btc:keep` ...), so
//! nothing here is a company figure. Two shards, 971,101-971,150 and
//! 971,151-971,199 (the scan's block), around one membership change at
//! 971,151:
//!
//! | address | entity | from | to |
//! |---|---|---|---|
//! | `btc:bee` | b | 971,000 | open |
//! | `btc:cee` | c | 971,000 | 971,150 (c's only address: c ends) |
//! | `btc:keep` | a | 971,000 | open |
//! | `btc:moved` | a, then b | 971,000 / 971,151 | 971,150 / open |
//! | `btc:new` | a | 971,151 | open (joins; coins received before count from 971,151) |
//! | `btc:old` | a | 971,000 | 971,150 (rotated out; still moves coins after) |

use onyx_core::{keccak256, Hash32, LabelSetRef};
use onyx_labels::dated::{DatedLabelSet, Membership};
use onyx_labels::{EntityKind, Label};
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use onyx_source_btc::reserve::{
    client_check, cross_checked_entities, flows_from_jsonl, serving_check, AnchorLabels,
    CrossChecks, FinalizedShard, FlowsFile, HeaderChain, ReserveError, ReserveLink, ReservePin,
    ScanRow, ScanSnapshot, SecondScan, ServingAnchor, ServingVerdict, SpanEnd,
};
use onyx_source_btc::{dated_label_set_from_json, verify_checksum, SCHEMA_ENTITY_RESERVE};
use std::collections::BTreeMap;
use std::path::PathBuf;

const START: u64 = 971_100;
const CHANGE: u64 = 971_151;
const SCAN_AT: u64 = 971_199;
const HEADERS: &str = "reserves/headers-969839-971207.json";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn headers() -> &'static HeaderChain {
    static H: std::sync::OnceLock<HeaderChain> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let fx = root().join("fixtures");
        verify_checksum(&fx, HEADERS).expect("checksum");
        HeaderChain::from_json(
            &std::fs::read_to_string(fx.join(HEADERS)).expect("read"),
            onyx_source_btc::node::MAINNET_MIN_BITS,
        )
        .expect("headers")
    })
}

fn recipe_id() -> Hash32 {
    keccak256(&std::fs::read(root().join("recipes/out/btc_entity_reserve.wasm")).expect("wasm"))
}

fn tip(h: u64) -> Hash32 {
    let mut b = [0u8; 32];
    hex::decode_to_slice(headers().hash_at(h).expect("header"), &mut b).expect("hex");
    Hash32(b)
}

fn m(addr: &str, entity: &str, from: u64, to: Option<u64>) -> Membership {
    Membership {
        label: Label {
            address: format!("btc:{addr}"),
            entity: format!("entity:{entity}"),
            kind: EntityKind::Exchange,
            confidence: onyx_core::Confidence(900_000),
            evidence: vec![],
        },
        from,
        to,
    }
}

fn dated() -> DatedLabelSet {
    let set = DatedLabelSet {
        name: "test.onyx.dated".into(),
        version: 1,
        published: 971_202,
        members: vec![
            m("bee", "b", 971_000, None),
            m("cee", "c", 971_000, Some(971_150)),
            m("keep", "a", 971_000, None),
            m("moved", "a", 971_000, Some(971_150)),
            m("moved", "b", CHANGE, None),
            m("new", "a", CHANGE, None),
            m("old", "a", 971_000, Some(971_150)),
        ],
    };
    set.check_structure().expect("canonical");
    set
}

/// Balances after block 971,100.
fn initial() -> BTreeMap<&'static str, i128> {
    [
        ("bee", 1_000),
        ("cee", 500),
        ("keep", 10_000),
        ("moved", 4_000),
        ("new", 2_000),
        ("old", 3_000),
    ]
    .into()
}

/// Per block, per address: (received, sent). Synthetic.
fn flows() -> BTreeMap<u64, BTreeMap<&'static str, (u128, u128)>> {
    let mut f: BTreeMap<u64, BTreeMap<&str, (u128, u128)>> = BTreeMap::new();
    for (h, a, rx, tx) in [
        (971_110, "moved", 700, 0),
        (971_120, "keep", 1_000, 0),
        (971_130, "old", 0, 100),
        (971_140, "new", 5_000, 0), // before it joins
        (971_155, "bee", 60, 0),
        (971_170, "keep", 0, 300),
        (971_175, "old", 0, 400), // after it left
        (971_180, "new", 200, 0),
        (971_190, "moved", 0, 50),
    ] {
        f.entry(h).or_default().insert(a, (rx, tx));
    }
    f
}

/// Reference model: an address's balance after block `h`.
fn bal(addr: &str, h: u64, fl: &BTreeMap<u64, BTreeMap<&str, (u128, u128)>>) -> i128 {
    let mut v = initial()[addr];
    if h <= START {
        return v;
    }
    for (_, b) in fl.range(START + 1..=h) {
        if let Some((rx, tx)) = b.get(addr) {
            v += *rx as i128 - *tx as i128;
        }
    }
    v
}

fn scan(set: &DatedLabelSet, fl: &BTreeMap<u64, BTreeMap<&str, (u128, u128)>>) -> ScanSnapshot {
    let owner = set.scan_owners(SCAN_AT);
    let rows = owner
        .iter()
        .filter_map(|(a, e)| {
            let v = bal(a.trim_start_matches("btc:"), SCAN_AT, fl);
            (v > 0).then(|| ScanRow {
                address: a.to_string(),
                entity: e.to_string(),
                utxos: 1,
                sats: v as u64,
            })
        })
        .collect();
    ScanSnapshot {
        version: 1,
        method: "synthetic scan (tests/reserve_dated.rs)".into(),
        height: SCAN_AT,
        block_hash: headers().hash_at(SCAN_AT).expect("h").into(),
        label_set_name: set.name.clone(),
        label_set_version: set.version,
        label_set_hash: format!("0x{}", hex::encode(set.content_hash().0)),
        addresses_scanned: owner.len() as u64,
        rows,
    }
}

/// A flows file over `from..=to` (real hashes and parents), as
/// `scripts/stream-reserve-flows.py` writes one.
fn flows_file(from: u64, to: u64, fl: &BTreeMap<u64, BTreeMap<&str, (u128, u128)>>) -> FlowsFile {
    let text: Vec<String> = (from..=to)
        .map(|h| {
            let f: serde_json::Map<String, serde_json::Value> = fl
                .get(&h)
                .map(|b| {
                    b.iter()
                        .map(|(a, (rx, tx))| ((*a).to_string(), serde_json::json!([rx, tx])))
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!({
                "height": h,
                "hash": headers().hash_at(h).expect("h"),
                "prev": headers().hash_at(h - 1).expect("h"),
                "flows": f,
            })
            .to_string()
        })
        .collect();
    flows_from_jsonl(&text.join("\n")).expect("flows")
}

fn set_hash(w: &onyx_labels::LabelSet) -> Hash32 {
    LabelSetRef {
        name: w.name.clone(),
        version: w.version,
        content_hash: w.content_hash(),
    }
    .hash()
}

fn leaves(rows: &[EntityReserveRow]) -> Vec<Vec<u8>> {
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

/// A shard over `from..=to` running `window` on `base`, as an honest
/// rerun would give it (flows from the reference model).
fn shard(
    round: u64,
    from: u64,
    to: u64,
    window: &onyx_labels::LabelSet,
    base: &[EntityBase],
    base_ref: Hash32,
    fl: &BTreeMap<u64, BTreeMap<&str, (u128, u128)>>,
) -> FinalizedShard {
    let rows: Vec<EntityReserveRow> = base
        .iter()
        .map(|b| {
            let (mut rx, mut tx) = (0u128, 0u128);
            for (_, blk) in fl.range(from..=to) {
                for l in window.labels.iter().filter(|l| l.entity == b.entity) {
                    if let Some((r, t)) = blk.get(l.address.trim_start_matches("btc:")) {
                        rx += r;
                        tx += t;
                    }
                }
            }
            EntityReserveRow {
                entity: b.entity.clone(),
                base_height: from - 1,
                height: to,
                base_ref: base_ref.0,
                base_sats: b.balance_sats,
                received_sats: rx,
                sent_sats: tx,
                tx_count: 0,
                balance_sats: u64::try_from(i128::from(b.balance_sats) + rx as i128 - tx as i128)
                    .expect("non-negative"),
            }
        })
        .collect();
    let l = leaves(&rows);
    let params = ReserveParams {
        base_height: from - 1,
        base_ref: base_ref.0,
        base: base.to_vec(),
    };
    FinalizedShard {
        round,
        shard: keccak256(format!("dated {from}-{to} {round}").as_bytes()),
        root: MerkleTree::from_rows(&l).root(),
        recipe: recipe_id(),
        label_set_hash: set_hash(window),
        schema: keccak256(SCHEMA_ENTITY_RESERVE),
        from,
        to,
        tip: tip(to),
        params: Some(borsh::to_vec(&params).expect("params")),
        leaves: l,
    }
}

fn base_of(pairs: &[(&str, i128)]) -> Vec<EntityBase> {
    pairs
        .iter()
        .map(|(e, v)| EntityBase {
            entity: format!("entity:{e}"),
            balance_sats: u64::try_from(*v).expect("sats"),
        })
        .collect()
}

fn ends(s: &FinalizedShard) -> Vec<(String, u64)> {
    use borsh::BorshDeserialize;
    s.leaves
        .iter()
        .map(|l| {
            let r =
                EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                    .expect("r");
            (r.entity, r.balance_sats)
        })
        .collect()
}

/// The honest chain: shard A on the window before the change, shard B on
/// the window after it, B's base = A's end with the change applied.
fn honest(set: &DatedLabelSet) -> Vec<FinalizedShard> {
    let fl = flows();
    let s = scan(set, &fl);
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let h = |a: &str, at: u64| bal(a, at, &fl);
    let a = shard(
        1,
        START + 1,
        CHANGE - 1,
        &wa,
        &base_of(&[
            ("a", h("keep", START) + h("moved", START) + h("old", START)),
            ("b", h("bee", START)),
            ("c", h("cee", START)),
        ]),
        s.hash(),
        &fl,
    );
    let at = CHANGE - 1;
    let b = shard(
        2,
        CHANGE,
        SCAN_AT,
        &wb,
        &base_of(&[
            ("a", h("keep", at) + h("new", at)),
            ("b", h("bee", at) + h("moved", at)),
        ]),
        a.root,
        &fl,
    );
    vec![a, b]
}

fn no_cross(s: &ScanSnapshot) -> CrossChecks {
    CrossChecks {
        version: 1,
        scan_hash: s.hash().to_string(),
        label_set_hash: s.label_set_hash.clone(),
        checks: vec![],
    }
}

fn check(
    set: &DatedLabelSet,
    s: &ScanSnapshot,
    f: Option<&FlowsFile>,
    shards: &[FinalizedShard],
) -> Result<ServingVerdict, ReserveError> {
    let c = no_cross(s);
    serving_check(
        &ServingAnchor {
            scan: s,
            labels: AnchorLabels::Dated { set, flows: f },
            headers: headers(),
            start_height: START,
            recipe: recipe_id(),
            cross_checks: &c,
            second_scans: &[],
        },
        shards,
    )
}

fn all_served(v: &ServingVerdict) -> bool {
    v.shards.iter().all(|(_, _, r)| r.is_ok())
}

fn none_served(v: &ServingVerdict) -> bool {
    v.shards.iter().all(|(_, _, r)| r.is_err())
}

/// RES-2: the reserve at height H uses exactly the addresses valid at H. At
/// the scan, entity a is `keep` + `new` (17,900 sats), never `old`'s 2,500;
/// `new`'s coins received before it joined count from its first height;
/// `moved` leaves a and joins b with its 4,700; c, whose only address
/// left, has no row after the change. The check derives the start base
/// from the scan, the finalized flows and the change alone.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_a_rotated_out_address_stops_counting_after_its_range() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let shards = honest(&set);
    assert_eq!(
        ends(&shards[0]),
        [
            ("entity:a".to_string(), 18_600),
            ("entity:b".into(), 1_000),
            ("entity:c".into(), 500)
        ]
    );
    assert_eq!(
        ends(&shards[1]),
        [("entity:a".to_string(), 17_900), ("entity:b".into(), 5_710)]
    );
    // The scan still holds old's coins; they are not a's at 971,199.
    assert!(s
        .rows
        .iter()
        .any(|r| r.address == "btc:old" && r.sats == 2_500));
    let v = check(&set, &s, Some(&ff), &shards).expect("check");
    assert!(all_served(&v), "{:?}", v.shards);
    assert_eq!(
        v.start_base,
        Some(base_of(&[("a", 17_000), ("b", 1_000), ("c", 500)]))
    );
    // The undated reading of the same addresses would count old at the scan.
    let undated: i128 = ["keep", "new", "old"]
        .iter()
        .map(|a| bal(a, SCAN_AT, &fl))
        .sum();
    assert_eq!(undated, 17_900 + 2_500);
}

/// RES-2 adversarial: a shard across the membership change (one window for
/// 971,101-971,199) is refused, whatever its label set; nothing is served.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_a_shard_across_a_membership_change_is_refused() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    // Its label set: the members at its first block (old still counted).
    let w = set.window(START + 1, CHANGE - 1).expect("w");
    let one = shard(
        1,
        START + 1,
        SCAN_AT,
        &w,
        &base_of(&[("a", 17_000), ("b", 1_000), ("c", 500)]),
        s.hash(),
        &fl,
    );
    let v = check(&set, &s, Some(&ff), &[one]).expect("check");
    let e = v.shards[0].2.as_ref().expect_err("refused");
    assert!(
        e.contains("membership changes at height 971151")
            && e.contains("must start at every membership change"),
        "{e}"
    );
}

/// RES-2 adversarial (back-dated or stale membership): a shard after the
/// change that still runs the old window (rotated-out `old` counted), or a
/// window of another set that back-dates `new`, is another label set than
/// the anchor's window for those blocks, and the chain stops there.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_a_shard_on_another_window_is_refused() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let mut shards = honest(&set);
    let mut stale = shards[1].clone();
    stale.label_set_hash = set_hash(&set.window(START + 1, CHANGE - 1).expect("A"));
    shards[1] = stale;
    let v = check(&set, &s, Some(&ff), &shards).expect("check");
    assert!(none_served(&v));
    let e = v.shards[1].2.as_ref().expect_err("refused");
    assert!(
        e.contains("another label set than the committed scan's"),
        "{e}"
    );
    // A successor that back-dates `new` to 971,000 is refused as a version.
    let mut back = set.clone();
    back.version = 2;
    back.published = 971_300;
    back.members[5].from = 971_000;
    assert!(matches!(
        back.check_successor(&set),
        Err(onyx_labels::dated::DatedError::BackDated { .. })
    ));
}

/// RES-2 adversarial (an address in two entities): `moved` belongs to a,
/// then b. Overlapping ranges (both at once) are refused as a set; one after
/// the other moves its balance at the change (checked in the first test:
/// b's base after the change is bee + moved).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_an_address_in_two_entities_at_once_is_refused() {
    let mut set = dated();
    set.members[3].to = Some(CHANGE); // moved: a until 971,151, b from 971,151
    assert!(set.check_structure().is_err());
    let fl = flows();
    // A scan naming the overlapping set is refused before anything else.
    let s = ScanSnapshot {
        label_set_hash: format!("0x{}", hex::encode(set.content_hash().0)),
        ..scan(&dated(), &fl)
    };
    let e = check(&set, &s, None, &[]).expect_err("refused");
    assert!(e.to_string().contains("two memberships overlap"), "{e}");
}

/// ADR-0037: the change needs each changing address's balance at it, rolled
/// back from the scan by a flows file on the header chain; the file must
/// agree with the finalized flows wherever it covers a shard, hold only
/// addresses of the set, and never take an address below zero.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_the_flows_file_must_cover_agree_and_stay_on_the_set() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let shards = honest(&set);
    // No flows file: the change cannot be applied; nothing served.
    let v = check(&set, &s, None, &shards).expect("check");
    assert!(none_served(&v));
    let e = v.shards[0].2.as_ref().expect_err("refused");
    assert!(
        e.contains("membership change at block 971151 cannot be applied")
            && e.contains("no flows file covers blocks 971151-971199"),
        "{e}"
    );
    // A file that starts after the change does not cover it.
    let late = flows_file(CHANGE + 1, SCAN_AT, &fl);
    let v = check(&set, &s, Some(&late), &shards).expect("check");
    assert!(none_served(&v));
    // A file that disagrees with the finalized flows of shard B (keep's
    // send is 1 sat smaller): refused.
    let mut off = fl.clone();
    off.get_mut(&971_170).expect("b").insert("keep", (0, 299));
    let ff = flows_file(CHANGE, SCAN_AT, &off);
    let v = check(&set, &s, Some(&ff), &shards).expect("check");
    assert!(none_served(&v));
    let e = v.shards[1].2.as_ref().expect_err("refused");
    assert!(
        e.contains("the flows file disagrees with the finalized flows of blocks 971151-971199"),
        "{e}"
    );
    // A flow at an address outside the set: the whole check refuses.
    let mut outside = fl.clone();
    outside
        .entry(971_160)
        .or_default()
        .insert("stranger", (1, 0));
    let ff = flows_file(CHANGE, SCAN_AT, &outside);
    let e = check(&set, &s, Some(&ff), &shards).expect_err("refused");
    assert!(e.to_string().contains("outside the set"), "{e}");
    // An address whose rolled-back balance dips below zero.
    let mut dip = fl.clone();
    dip.entry(971_185).or_default().insert("cee", (600, 0));
    let ff = flows_file(CHANGE, SCAN_AT, &dip);
    let s2 = scan(&set, &fl); // the scan still says cee holds 500
    let e = check(&set, &s2, Some(&ff), &shards).expect_err("refused");
    assert!(matches!(e, ReserveError::Negative(_)), "{e}");
}

/// RES-2 adversarial (reorg at the range edge): shard A ends at 971,150, the
/// last block before the change. A on a stale tip there is refused and the
/// chain stops (nothing served); next to the honest A only it is refused.
/// A flows file whose first block (971,151) names another parent is refused.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_a_reorg_at_the_range_edge_is_refused() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let honest = honest(&set);
    let mut stale = honest[0].clone();
    stale.round = 9;
    stale.shard = keccak256(b"A on a stale branch");
    stale.tip = keccak256(b"another block 971,150");
    let v = check(&set, &s, Some(&ff), &[stale.clone(), honest[1].clone()]).expect("check");
    assert!(none_served(&v));
    assert!(v.shards[0]
        .2
        .as_ref()
        .is_err_and(|e| e.contains("stale branch")));
    let v = check(
        &set,
        &s,
        Some(&ff),
        &[honest[0].clone(), honest[1].clone(), stale],
    )
    .expect("check");
    assert!(v.shards[..2].iter().all(|(_, _, r)| r.is_ok()));
    assert!(v.shards[2].2.is_err());
    let mut edge = ff.clone();
    edge.parent = "33".repeat(32);
    assert_eq!(
        check(&set, &s, Some(&edge), &honest).expect_err("refused"),
        ReserveError::StaleBranch(CHANGE - 1)
    );
}

/// THREATS RES-13 residual, pinned: flows of an address outside its
/// membership range are rerun by no shard. A flows file that inflates
/// `old`'s sends after it left (+1,000, so its balance at the change reads
/// 1,000 higher), with shards built on the base it implies, is served: a's
/// reserve before the change is 1,000 higher, and the change removes it
/// again, so the landing still matches the scan. The honest shards are
/// refused next to such a file. Mitigation: the file is on the header chain
/// and recomputable from the blocks (TODO D11: rerun the changing
/// addresses' out-of-range flows as their own shards).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_13_residual_out_of_range_flows_are_the_flows_files_word() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let mut forged = fl.clone();
    forged
        .get_mut(&971_175)
        .expect("b")
        .insert("old", (0, 1_400));
    let ff = flows_file(CHANGE, SCAN_AT, &forged);
    // The honest shards do not match the base the forged file implies.
    let v = check(&set, &s, Some(&ff), &honest(&set)).expect("check");
    assert!(none_served(&v));
    // Shards built on it: A starts 1,000 higher for a and ends 1,000 higher.
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let a = shard(
        1,
        START + 1,
        CHANGE - 1,
        &wa,
        &base_of(&[("a", 18_000), ("b", 1_000), ("c", 500)]),
        s.hash(),
        &fl,
    );
    let b = shard(
        2,
        CHANGE,
        SCAN_AT,
        &wb,
        &base_of(&[("a", 18_000), ("b", 5_700)]),
        a.root,
        &fl,
    );
    assert_eq!(ends(&a)[0], ("entity:a".to_string(), 19_600));
    let v = check(&set, &s, Some(&ff), &[a, b]).expect("check");
    assert!(all_served(&v), "the residual: served");
}

/// THREATS RES-13 (review D1-R1 F1, the review's PoC ported): in-range flows
/// of a changing address are also the flows file's word per address. The
/// file is checked against the finalized rows only as per-entity sums, so a
/// file that moves bee's 60-sat receipt at 971,155 onto `moved` at 971,190
/// (entity b's sums unchanged) lowers `moved`'s balance at the change by 60:
/// with shards on the base it implies, 60 sats move from a to b before the
/// change, and it is served. TODO D11: rerun every flow of each changing
/// address from the change to the scan, per address.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_13_residual_in_range_flows_are_split_per_address_by_the_file() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let mut split = fl.clone();
    split.get_mut(&971_155).expect("b").remove("bee");
    split
        .get_mut(&971_190)
        .expect("b")
        .insert("moved", (60, 50));
    let ff = flows_file(CHANGE, SCAN_AT, &split);
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let a = shard(
        1,
        START + 1,
        CHANGE - 1,
        &wa,
        &base_of(&[("a", 16_940), ("b", 1_060), ("c", 500)]),
        s.hash(),
        &fl,
    );
    let b = shard(
        2,
        CHANGE,
        SCAN_AT,
        &wb,
        &base_of(&[("a", 18_000), ("b", 5_700)]),
        a.root,
        &fl,
    );
    let v = check(&set, &s, Some(&ff), &[a, b]).expect("check");
    assert!(all_served(&v), "the residual: served");
    assert_eq!(
        v.start_base,
        Some(base_of(&[("a", 16_940), ("b", 1_060), ("c", 500)]))
    );
}

/// Review D1-R1 F3: only heights up to the set's publication are frozen
/// against later versions, so a scan after the publication is refused, and
/// so is a shard reaching past it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_nothing_after_the_publication_is_used() {
    let fl = flows();
    let mut early = dated();
    early.published = 971_190;
    let s = scan(&early, &fl);
    let e = check(&early, &s, None, &[]).expect_err("refused");
    assert!(e.to_string().contains("after the set's publication"), "{e}");
    // Published at the scan: a shard past it (971,200-971,200) is refused.
    let mut at = dated();
    at.published = SCAN_AT;
    let s = scan(&at, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let mut shards = honest(&at);
    let b = shards[1].clone();
    let wb = at.window(CHANGE, SCAN_AT).expect("B");
    let ends_b: Vec<EntityBase> = ends(&b)
        .into_iter()
        .map(|(entity, balance_sats)| EntityBase {
            entity,
            balance_sats,
        })
        .collect();
    shards.push(shard(3, 971_200, 971_200, &wb, &ends_b, b.root, &fl));
    let v = check(&at, &s, Some(&ff), &shards).expect("check");
    assert!(v.shards[..2].iter().all(|(_, _, r)| r.is_ok()));
    let e = v.shards[2].2.as_ref().expect_err("refused");
    assert!(e.contains("reach past the label set's publication"), "{e}");
}

/// Review D1-R1 F7: two finalized shards of one range after the scan that
/// disagree on the flows (one also on another base) stop the chain there:
/// no later range is served on flows nothing compared.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_disagreement_after_the_scan_stops_the_chain_there() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let mut shards = honest(&set);
    let b = shards[1].clone();
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let ends_b: Vec<EntityBase> = ends(&b)
        .into_iter()
        .map(|(entity, balance_sats)| EntityBase {
            entity,
            balance_sats,
        })
        .collect();
    let p = shard(3, 971_200, 971_200, &wb, &ends_b, b.root, &fl);
    let mut other = fl.clone();
    other.entry(971_200).or_default().insert("keep", (5, 0));
    let mut plus = ends_b.clone();
    plus[0].balance_sats += 1;
    let p2 = shard(4, 971_200, 971_200, &wb, &plus, b.root, &other);
    let pe: Vec<EntityBase> = ends(&p)
        .into_iter()
        .map(|(entity, balance_sats)| EntityBase {
            entity,
            balance_sats,
        })
        .collect();
    let q = shard(5, 971_201, 971_202, &wb, &pe, p.root, &fl);
    shards.extend([p, p2, q]);
    let v = check(&set, &s, Some(&ff), &shards).expect("check");
    assert!(v.shards[..2].iter().all(|(_, _, r)| r.is_ok()));
    for i in [2, 4] {
        let e = v.shards[i].2.as_ref().expect_err("refused");
        assert!(
            e.contains("disagree on the flows: the chain stops there"),
            "{i}: {e}"
        );
    }
    assert!(v.shards[3].2.is_err());
}

/// Review D1-R1 F5: a re-scan that disagrees at an address counts against
/// every entity the address ever belongs to (`moved`: a, then b).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_rescan_disagreement_counts_against_every_entity_of_the_address() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let c = no_cross(&s);
    let mut again = s.clone();
    again.method = "a re-scan of the same block on another node".into();
    let clean = vec![SecondScan {
        scan: again.clone(),
        flows: BTreeMap::new(),
    }];
    let mut off = again.clone();
    let i = off
        .rows
        .iter()
        .position(|r| r.address == "btc:moved")
        .expect("moved");
    off.rows[i].sats += 1;
    let bad = vec![SecondScan {
        scan: off,
        flows: BTreeMap::new(),
    }];
    let cross = |second: &[SecondScan]| {
        cross_checked_entities(&ServingAnchor {
            scan: &s,
            labels: AnchorLabels::Dated {
                set: &set,
                flows: None,
            },
            headers: headers(),
            start_height: START,
            recipe: recipe_id(),
            cross_checks: &c,
            second_scans: second,
        })
        .expect("cross")
    };
    let m = cross(&clean);
    assert!(m.values().all(Option::is_some), "{m:?}");
    let m = cross(&bad);
    let none: Vec<&str> = m
        .iter()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.as_str())
        .collect();
    assert_eq!(none, ["entity:a", "entity:b"]);
}

/// Review D1-R1 F6: a dated set file must spell every address canonically
/// (bech32 lowercase), so the overlap rule cannot be passed with an
/// upper-case copy of the same script in another entity.
#[test]
fn a_dated_set_file_refuses_a_second_spelling_of_an_address() {
    let doc = |a2: &str| {
        serde_json::json!({
            "name": "test.onyx.dated",
            "version": 1,
            "published": 971_202,
            "members": [
                {"address": a2, "entity": "entity:b",
                 "kind": "Exchange", "confidence_ppm": 900000, "from": 971000, "to": null},
                {"address": "btc:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", "entity": "entity:a",
                 "kind": "Exchange", "confidence_ppm": 900000, "from": 971000, "to": null},
            ],
        })
        .to_string()
    };
    assert!(dated_label_set_from_json(&doc("btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")).is_ok());
    let e = dated_label_set_from_json(&doc("btc:BC1QAR0SRRR7XFKVY5L643LYDNW9RE59GTZZWF5MDQ"))
        .expect_err("refused");
    assert!(e.to_string().contains("address not canonical"), "{e}");
}

/// Review D1-R2 (range edges): `from` is the first height a membership
/// counts and `to` the last, both inclusive, so the change at 971,151 must
/// be a shard's first block exactly. Split one block late (A to 971,151) or
/// one block early (B from 971,150) and the shard holding the change is
/// refused; split right at it, with A itself in two pieces (a run of two
/// shards inside one window), all three are served.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_2_range_edges_are_inclusive_and_exact() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let h = |a: &str, at: u64| bal(a, at, &fl);
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let base_a = |at: u64| {
        base_of(&[
            ("a", h("keep", at) + h("moved", at) + h("old", at)),
            ("b", h("bee", at)),
            ("c", h("cee", at)),
        ])
    };
    let base_b = |at: u64| {
        base_of(&[
            ("a", h("keep", at) + h("new", at)),
            ("b", h("bee", at) + h("moved", at)),
        ])
    };
    let changes_inside = |v: &ServingVerdict, i: usize| {
        let e = v.shards[i].2.as_ref().expect_err("refused");
        assert!(e.contains("membership changes at height 971151"), "{e}");
    };
    // One block late: A ends on the change's first block.
    let late = shard(1, START + 1, CHANGE, &wa, &base_a(START), s.hash(), &fl);
    let v = check(&set, &s, Some(&ff), &[late]).expect("check");
    changes_inside(&v, 0);
    // One block early: B starts on the last block before the change.
    let a = shard(1, START + 1, CHANGE - 2, &wa, &base_a(START), s.hash(), &fl);
    let early = shard(
        2,
        CHANGE - 1,
        SCAN_AT,
        &wb,
        &base_b(CHANGE - 2),
        a.root,
        &fl,
    );
    let v = check(&set, &s, Some(&ff), &[a, early]).expect("check");
    assert!(none_served(&v));
    changes_inside(&v, 1);
    // Exactly at the change, A in two pieces: all served, B on A2's root.
    let a1 = shard(1, START + 1, 971_120, &wa, &base_a(START), s.hash(), &fl);
    let a2 = shard(2, 971_121, CHANGE - 1, &wa, &base_a(971_120), a1.root, &fl);
    let b = shard(3, CHANGE, SCAN_AT, &wb, &base_b(CHANGE - 1), a2.root, &fl);
    let v = check(&set, &s, Some(&ff), &[a1, a2, b]).expect("check");
    assert!(all_served(&v), "{:?}", v.shards);
    // The open range (`bee`, b) and the closed one ending at 971,150
    // (`cee`, c's last row is A2's) as windows: c is in A's, not B's.
    let ents = |w: &onyx_labels::LabelSet| -> Vec<String> {
        w.labels.iter().map(|l| l.entity.clone()).collect()
    };
    assert!(ents(&wa).contains(&"entity:c".to_string()));
    assert!(!ents(&wb).contains(&"entity:c".to_string()));
    assert!(wb.labels.iter().any(|l| l.address == "btc:bee"));
}

// ------------------------------------------- the client check (ROADMAP D22)

/// Blocks of the shard after the scan (C): 971,200-971,202, ending at the
/// set's publication, with six confirmations in the committed headers.
const AFTER_TO: u64 = 971_202;

/// The client's pin over the dated set, its scan and the flows file, each
/// trusted by hash (THREATS D3-R2-3, D22-2).
fn dated_pin(set: &DatedLabelSet, s: &ScanSnapshot, ff: &FlowsFile) -> ReservePin {
    ReservePin::from_dated_scan(
        s,
        set,
        &[],
        Some(ff.clone()),
        recipe_id(),
        &s.hash(),
        Some(&ff.hash()),
    )
    .expect("pin")
}

/// The scan at height `h` of the reference model (every address of the
/// set, filed as `scan_owners(h)` files it).
fn scan_at(
    set: &DatedLabelSet,
    fl: &BTreeMap<u64, BTreeMap<&str, (u128, u128)>>,
    h: u64,
) -> ScanSnapshot {
    let owner = set.scan_owners(h);
    let rows = owner
        .iter()
        .filter_map(|(a, e)| {
            let v = bal(a.trim_start_matches("btc:"), h, fl);
            (v > 0).then(|| ScanRow {
                address: a.to_string(),
                entity: e.to_string(),
                utxos: 1,
                sats: v as u64,
            })
        })
        .collect();
    ScanSnapshot {
        version: 1,
        method: format!("synthetic scan at {h} (tests/reserve_dated.rs)"),
        height: h,
        block_hash: headers().hash_at(h).expect("h").into(),
        label_set_name: set.name.clone(),
        label_set_version: set.version,
        label_set_hash: format!("0x{}", hex::encode(set.content_hash().0)),
        addresses_scanned: owner.len() as u64,
        rows,
    }
}

/// Per entity of `window`, the summed balance of its addresses after `h`.
fn window_base(window: &onyx_labels::LabelSet, h: u64) -> Vec<EntityBase> {
    let fl = flows();
    let mut t: BTreeMap<String, i128> = BTreeMap::new();
    for l in &window.labels {
        *t.entry(l.entity.clone()).or_default() +=
            bal(l.address.trim_start_matches("btc:"), h, &fl);
    }
    t.into_iter()
        .map(|(entity, v)| EntityBase {
            entity,
            balance_sats: u64::try_from(v).expect("sats"),
        })
        .collect()
}

/// The honest chain over any dated set of this world: A before the
/// change, B from it to the scan, C after the scan; every base the window's
/// addresses' balances (the serving check's rebase).
fn honest_abc(set: &DatedLabelSet, start_ref: Hash32) -> Vec<FinalizedShard> {
    let fl = flows();
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let wc = set.window(SCAN_AT + 1, AFTER_TO).expect("C");
    let a = shard(
        1,
        START + 1,
        CHANGE - 1,
        &wa,
        &window_base(&wa, START),
        start_ref,
        &fl,
    );
    let b = shard(
        2,
        CHANGE,
        SCAN_AT,
        &wb,
        &window_base(&wb, CHANGE - 1),
        a.root,
        &fl,
    );
    let c = shard(
        3,
        SCAN_AT + 1,
        AFTER_TO,
        &wc,
        &window_base(&wc, SCAN_AT),
        b.root,
        &fl,
    );
    vec![a, b, c]
}

/// The entity's link in a shard, as a client holds it after proving it:
/// the leaf at the entity's index in that shard's window.
fn link_of(sh: &FinalizedShard, entity: &str) -> ReserveLink {
    use borsh::BorshDeserialize;
    let index = sh
        .leaves
        .iter()
        .position(|l| OutputRow::try_from_slice(l).expect("o").key == entity.as_bytes())
        .expect("the entity's row");
    ReserveLink {
        round: sh.round,
        shard: sh.shard,
        root: sh.root,
        recipe: sh.recipe,
        label_set_hash: sh.label_set_hash,
        schema: sh.schema,
        from: sh.from,
        to: sh.to,
        tip: sh.tip,
        index: index as u64,
        leaf: sh.leaves[index].clone(),
    }
}

fn links_of(shards: &[&FinalizedShard], entity: &str) -> Vec<ReserveLink> {
    shards.iter().map(|s| link_of(s, entity)).collect()
}

fn base_row(sh: &FinalizedShard, entity: &str) -> u64 {
    use borsh::BorshDeserialize;
    sh.leaves
        .iter()
        .map(|l| {
            EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                .expect("row")
        })
        .find(|r| r.entity == entity)
        .expect("row")
        .base_sats
}

fn refused<T: std::fmt::Debug>(r: Result<T, ReserveError>, why: &str) {
    match r {
        Ok(v) => panic!("accepted, wanted a refusal with {why:?}: {v:?}"),
        Err(e) => assert!(e.to_string().contains(why), "{e}"),
    }
}

/// THREATS D3-R2-3 FIXED (ROADMAP D22; was `d22_the_client_check_fails_closed_on_a_dated_chain`):
/// the client check reads dated sets. On the honest dated chain (the
/// operator's serving check serves every row) the client, from its own pin
/// over the dated set, the scan and the flows file, checks each link's
/// window hash and the entity's window index, applies the membership change
/// at 971,151 from the flows file, checks every tip on its own header chain
/// with six confirmations, and derives the same first base the serving
/// check does: for `a` and `b` across the change; for `c`, whose only
/// address leaves at the change, on A alone (anchored where it exits); and
/// for an answer after the scan (C). An answer at the landing costs one link.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_the_client_check_passes_an_honest_dated_chain() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let shards = honest(&set);
    let v = check(&set, &s, Some(&ff), &shards).expect("check");
    assert!(all_served(&v), "{:?}", v.shards);
    let abc = honest_abc(&set, s.hash());
    assert_eq!(abc[..2], shards[..], "the same honest chain");
    let (a, b, c) = (&abc[0], &abc[1], &abc[2]);
    assert_ne!(a.label_set_hash, b.label_set_hash, "two windows");
    let pin = dated_pin(&set, &s, &ff);
    let h = headers();
    for e in ["entity:a", "entity:b"] {
        let v = client_check(&pin, e, &links_of(&[a, b], e), h).expect("passes");
        assert_eq!(v.chain.anchor, SpanEnd::Landing(1));
        assert_eq!(v.chain.start_height, START);
        assert_eq!(v.chain.start_base_sats, base_row(a, e), "{e}: derived");
        // The span from the landing: an answer at B pays one link.
        let v = client_check(&pin, e, &links_of(&[b], e), h).expect("one link");
        assert_eq!(v.chain.start_base_sats, base_row(b, e));
        // After the scan: B and C, or C's span from the landing.
        let v = client_check(&pin, e, &links_of(&[a, b, c], e), h).expect("after");
        assert_eq!(v.rows.len(), 3);
        refused(
            client_check(&pin, e, &links_of(&[c], e), h),
            "no link ends at the committed scan's height",
        );
    }
    // The change is applied, not skipped: B's base is not A's end for `a`.
    let end_a = {
        use borsh::BorshDeserialize;
        EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(&a.leaves[0]).expect("o").value)
            .expect("row")
            .balance_sats
    };
    assert_ne!(end_a, base_row(b, "entity:a"));
    // `c` leaves at the change: A's end is `cee`'s balance after 971,150
    // (the scan rolled back with the flows file), and B holds no `c` row.
    let v = client_check(&pin, "entity:c", &links_of(&[a], "entity:c"), h).expect("c exits");
    assert_eq!(v.chain.anchor, SpanEnd::Exit);
    assert_eq!(v.chain.start_base_sats, base_row(a, "entity:c"));
    let mut lc = links_of(&[a], "entity:c");
    lc.push(link_of(b, "entity:a"));
    refused(
        client_check(&pin, "entity:c", &lc, h),
        "the entity has no address in these blocks",
    );
}

/// ROADMAP D22: every forgery of a dated span fails closed. Wrong window
/// hash; a membership change without its delta (B's base A's end, alone or
/// with A's base raised so the chain still lands on the scan); a tip off
/// the header chain, or under six confirmations; a span that skips a link;
/// a stale pin (its block off the header chain, or under six
/// confirmations); a flows file the client does not trust, or none; a
/// dated version without its history; a forged base at an exit.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_every_forged_dated_span_fails_closed() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let abc = honest_abc(&set, s.hash());
    let (a, b, c) = (&abc[0], &abc[1], &abc[2]);
    let pin = dated_pin(&set, &s, &ff);
    let h = headers();
    let e = "entity:a";
    // Wrong window hash: B bound to A's window.
    let mut l = links_of(&[a, b], e);
    l[1].label_set_hash = a.label_set_hash;
    refused(
        client_check(&pin, e, &l, h),
        "another label set than the pinned scan's",
    );
    // A membership change without its delta: B rerun on A's end.
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let a_end: Vec<EntityBase> = ends(a)
        .into_iter()
        .filter(|(x, _)| x != "entity:c")
        .map(|(entity, balance_sats)| EntityBase {
            entity,
            balance_sats,
        })
        .collect();
    let b_flat = shard(2, CHANGE, SCAN_AT, &wb, &a_end, a.root, &fl);
    refused(
        client_check(&pin, e, &links_of(&[a, &b_flat], e), h),
        "base is not the previous link's end balance plus the membership change",
    );
    // ... with A raised by the delta, so the forged chain lands on the scan.
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let delta = i128::from(base_row(b, e)) - i128::from(a_end[0].balance_sats);
    let mut raised = window_base(&wa, START);
    raised[0].balance_sats = u64::try_from(i128::from(raised[0].balance_sats) + delta).expect("+");
    let a_up = shard(1, START + 1, CHANGE - 1, &wa, &raised, s.hash(), &fl);
    let up_end: Vec<EntityBase> = ends(&a_up)
        .into_iter()
        .filter(|(x, _)| x != "entity:c")
        .map(|(entity, balance_sats)| EntityBase {
            entity,
            balance_sats,
        })
        .collect();
    let b_up = shard(2, CHANGE, SCAN_AT, &wb, &up_end, a_up.root, &fl);
    assert_eq!(ends(&b_up)[0], ends(b)[0], "a lands on the scan's total");
    refused(
        client_check(&pin, e, &links_of(&[&a_up, &b_up], e), h),
        "base is not the one derived from the committed scan and the finalized flows",
    );
    // A tip off the header chain (another real block), before the landing.
    let mut l = links_of(&[a, b], e);
    l[0].tip = tip(CHANGE - 2);
    refused(client_check(&pin, e, &l, h), "a stale branch");
    // A tip under six confirmations: C on headers that end one block early.
    let short = h.truncated(AFTER_TO + 4).expect("truncated");
    refused(
        client_check(&pin, e, &links_of(&[b, c], e), &short),
        "block 971202 has 5 confirmations in the header chain; 6 needed",
    );
    client_check(&pin, e, &links_of(&[a, b], e), &short).expect("B is deep enough");
    // A span that skips a link.
    refused(
        client_check(&pin, e, &links_of(&[a, c], e), h),
        "does not start after the previous link",
    );
    // A stale pin: its scan's block is not the header chain's at its
    // height, or not deep enough.
    let mut stale = s.clone();
    stale.block_hash = headers().hash_at(SCAN_AT - 1).expect("h").into();
    let p = ReservePin::from_dated_scan(
        &stale,
        &set,
        &[],
        Some(ff.clone()),
        recipe_id(),
        &stale.hash(),
        Some(&ff.hash()),
    )
    .expect("a well-formed pin");
    refused(
        client_check(&p, e, &links_of(&[a, b], e), h),
        "is not the header chain's block at that height",
    );
    refused(
        client_check(
            &pin,
            e,
            &links_of(&[a, b], e),
            &h.truncated(SCAN_AT + 4).expect("t"),
        ),
        "6 needed",
    );
    // The flows file: one the client does not trust, a forged one, or none.
    let mut forged = ff.clone();
    forged.blocks[4].flows.insert("btc:new".into(), (1, 0));
    refused(
        ReservePin::from_dated_scan(
            &s,
            &set,
            &[],
            Some(forged),
            recipe_id(),
            &s.hash(),
            Some(&ff.hash()),
        ),
        "the flows file's hash is not the one the client trusts",
    );
    refused(
        ReservePin::from_dated_scan(
            &s,
            &set,
            &[],
            Some(ff.clone()),
            recipe_id(),
            &s.hash(),
            None,
        ),
        "the flows file's hash is not the one the client trusts",
    );
    // Review D22-R1-3: the hash commits the blocks; summary fields that
    // disagree with them are refused.
    let mut lying = ff.clone();
    lying.flows.insert("btc:keep".into(), (1, 0));
    assert_eq!(lying.hash(), ff.hash());
    refused(
        ReservePin::from_dated_scan(
            &s,
            &set,
            &[],
            Some(lying),
            recipe_id(),
            &s.hash(),
            Some(&ff.hash()),
        ),
        "the per-address totals are not the blocks' sum",
    );
    // A trusted flows file with a block off the client's headers.
    let mut off = ff.clone();
    off.blocks[3].hash = headers().hash_at(CHANGE + 10).expect("h").into();
    let p_off = ReservePin::from_dated_scan(
        &s,
        &set,
        &[],
        Some(off.clone()),
        recipe_id(),
        &s.hash(),
        Some(&off.hash()),
    )
    .expect("a well-formed pin");
    refused(
        client_check(&p_off, e, &links_of(&[b], e), h),
        "is not the header chain's block at that height",
    );
    // The browser's path (no dated set: one window) refuses the dated
    // chain at the change rather than accept it.
    let rows: Vec<EntityReserveRow> = [a, b]
        .iter()
        .map(|sh| {
            use borsh::BorshDeserialize;
            EntityReserveRow::try_from_slice(
                &OutputRow::try_from_slice(&sh.leaves[0]).expect("o").value,
            )
            .expect("row")
        })
        .collect();
    let el: Vec<onyx_source_btc::reserve::EntityLink<'_>> = [a, b]
        .iter()
        .zip(&rows)
        .map(|(sh, row)| onyx_source_btc::reserve::EntityLink {
            from: sh.from,
            to: sh.to,
            root: sh.root,
            row,
        })
        .collect();
    refused(
        onyx_source_btc::reserve::derive_entity_span(
            &onyx_source_btc::reserve::SpanAnchor {
                scan_height: SCAN_AT,
                totals: &pin.totals,
                dated: None,
            },
            e,
            &el,
        ),
        "base is not",
    );
    let bare = ReservePin::from_dated_scan(&s, &set, &[], None, recipe_id(), &s.hash(), None)
        .expect("no flows file");
    refused(
        client_check(&bare, e, &links_of(&[a, b], e), h),
        "cannot be applied",
    );
    client_check(&bare, e, &links_of(&[b], e), h).expect("no change in the span");
    // A dated version above 1 without its history (review D1-R1 F2).
    let mut v2 = set.clone();
    v2.version = 2;
    refused(
        ReservePin::from_dated_scan(&s, &v2, &[], None, recipe_id(), &s.hash(), None),
        "history",
    );
    // A forged base at `c`'s exit (its addresses hold less than the row).
    let mut wrong = window_base(&wa, START);
    wrong[2].balance_sats += 1;
    let a_c = shard(1, START + 1, CHANGE - 1, &wa, &wrong, s.hash(), &fl);
    refused(
        client_check(&pin, "entity:c", &links_of(&[&a_c], "entity:c"), h),
        "base is not the one derived",
    );
}

/// ROADMAP D22 (THREATS D3-R2-2): a newer scan reconciled with the chain's
/// can be pinned. The chain was built on the scan at 971,199 (its first
/// `base_ref`); a client pins the scan at 971,150 instead (it reconciles
/// with the later one through the flows file, address by address) and
/// checks B with its landing A only: the bases follow from the newer scan.
/// An entity that joins after the newer scan (`d`, a variant set where
/// `new` is its own entity from 971,151) is anchored where it joins.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_a_newer_reconciled_scan_can_be_pinned() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let abc = honest_abc(&set, s.hash());
    let (a, b) = (&abc[0], &abc[1]);
    let s150 = scan_at(&set, &fl, CHANGE - 1);
    assert!(
        onyx_source_btc::reserve::reconcile(&s150, &s, &ff.flows)
            .expect("reconcile")
            .is_empty(),
        "the two scans reconcile at every address"
    );
    let pin150 = dated_pin(&set, &s150, &ff);
    let h = headers();
    for e in ["entity:a", "entity:b"] {
        let v = client_check(&pin150, e, &links_of(&[a, b], e), h).expect("newer pin");
        assert_eq!(v.chain.anchor, SpanEnd::Landing(0));
        assert_eq!(v.chain.start_base_sats, base_row(a, e));
        refused(
            client_check(&pin150, e, &links_of(&[b], e), h),
            "no link ends at the committed scan's height 971150",
        );
    }
    // Entry: `new` as entity `d` from the change.
    let mut sd = set.clone();
    for m in sd
        .members
        .iter_mut()
        .filter(|m| m.label.address == "btc:new")
    {
        m.label.entity = "entity:d".into();
    }
    sd.check_structure().expect("canonical");
    let d150 = scan_at(&sd, &fl, CHANGE - 1);
    let pd = dated_pin(&sd, &d150, &ff);
    let chain = honest_abc(&sd, keccak256(b"an older scan"));
    let v = client_check(&pd, "entity:d", &links_of(&[&chain[1]], "entity:d"), h).expect("d joins");
    assert_eq!(v.chain.anchor, SpanEnd::Entry);
    assert_eq!(v.chain.start_base_sats, base_row(&chain[1], "entity:d"));
    assert_eq!(
        v.chain.start_base_sats, 7_000,
        "new's 2,000 plus 5,000 received before it joined"
    );
    // A forged joining base is refused.
    let wb = sd.window(CHANGE, SCAN_AT).expect("B");
    let mut up = window_base(&wb, CHANGE - 1);
    let i = up.iter().position(|x| x.entity == "entity:d").expect("d");
    up[i].balance_sats += 1;
    let forged = shard(2, CHANGE, SCAN_AT, &wb, &up, chain[0].root, &fl);
    refused(
        client_check(&pd, "entity:d", &links_of(&[&forged], "entity:d"), h),
        "base is not the one derived",
    );
}

/// Review D22-R2-1 and D22-R2-2: a flows file under a hash the client
/// trusts must still agree with the finalized flows of every link it covers
/// (as the serving check requires), and hold one block per height. The file
/// here says `new` received 1,200 at 971,180 (the finalized rerun: 200), so
/// its balance after 971,150 is 1,000 short and `a`'s delta at the change
/// is 1,000 lower. A chain whose A base is raised by 1,000 then lands on
/// the scan exactly (B's base and end are the honest ones): without the
/// cross-check the client accepted A's balance, 1,000 too high.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_r2_a_trusted_flows_file_must_agree_with_the_finalized_flows() {
    let set = dated();
    let fl = flows();
    let s = scan(&set, &fl);
    let mut fl2 = flows();
    fl2.get_mut(&971_180)
        .expect("block")
        .insert("new", (1_200, 0));
    let ff2 = flows_file(CHANGE, SCAN_AT, &fl2);
    let pin2 = dated_pin(&set, &s, &ff2);
    let h = headers();
    let e = "entity:a";
    let wa = set.window(START + 1, CHANGE - 1).expect("A");
    let wb = set.window(CHANGE, SCAN_AT).expect("B");
    let mut raised = window_base(&wa, START);
    assert_eq!(raised[0].entity, e);
    raised[0].balance_sats += 1_000;
    let a_up = shard(1, START + 1, CHANGE - 1, &wa, &raised, s.hash(), &fl);
    let b2 = shard(
        2,
        CHANGE,
        SCAN_AT,
        &wb,
        &window_base(&wb, CHANGE - 1),
        a_up.root,
        &fl,
    );
    let abc = honest_abc(&set, s.hash());
    assert_eq!(ends(&b2), ends(&abc[1]), "B lands on the scan's totals");
    refused(
        client_check(&pin2, e, &links_of(&[&a_up, &b2], e), h),
        "the trusted flows file disagrees with the finalized flows",
    );
    // The honest chain is refused under that file too (fail closed), and
    // passes under the file that agrees with it.
    refused(
        client_check(&pin2, e, &links_of(&[&abc[0], &abc[1]], e), h),
        "the trusted flows file disagrees with the finalized flows",
    );
    let ff = flows_file(CHANGE, SCAN_AT, &fl);
    let pin = dated_pin(&set, &s, &ff);
    client_check(&pin, e, &links_of(&[&abc[0], &abc[1]], e), h).expect("honest");
    refused(
        client_check(&pin, e, &links_of(&[&a_up, &b2], e), h),
        "base is not the one derived",
    );
    // A block repeated under a trusted hash (summary kept consistent) would
    // count `new`'s 200 twice: refused when the pin is built.
    let mut twice = ff.clone();
    let i = twice
        .blocks
        .iter()
        .position(|b| b.height == 971_180)
        .expect("block");
    let dup = twice.blocks[i].clone();
    twice.blocks.insert(i + 1, dup);
    twice.flows.get_mut("btc:new").expect("new").0 += 200;
    refused(
        ReservePin::from_dated_scan(
            &s,
            &set,
            &[],
            Some(twice.clone()),
            recipe_id(),
            &s.hash(),
            Some(&twice.hash()),
        ),
        "blocks are not contiguous",
    );
}
