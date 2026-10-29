//! The client's reserve serving check (ROADMAP D3, THREATS RES-10, ADR-0034
//! Amendment 2) on the committed reserve history: the 89 shards of
//! `apps/web/static/history/reserves-week.json`, the committed scan at block
//! 971,199 and label set v2.
//!
//! The server's `servingCheck` is the operator's claim. `reserve::client_check`
//! needs only what the client pins itself (the scan's hash, height, block and
//! totals, the label set, the recipe), its own header chain (ROADMAP D22:
//! `fixtures/reserves/headers-969839-971207.json`) and links it proved itself
//! (roots final on its own node, bindings recomputed, leaves under their
//! roots). These tests hand it links as a client would hold them after those
//! checks, built by an operator who skipped the serving check: forged bases,
//! a swapped scan, another label set version, a stale branch; and spans from
//! the landing (D22), on the committed scan or a newer one.

use borsh::BorshDeserialize;
use onyx_core::{keccak256, Hash32, LabelSetRef};
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::{EntityBase, EntityReserveRow};
use onyx_source_btc::reserve::{
    check_chain, client_check, HeaderChain, ReserveError, ReserveLink, ReservePin, ReserveShard,
    ScanRow, ScanSnapshot, SpanEnd,
};
use onyx_source_btc::{load_label_set, verify_checksum, SCHEMA_ENTITY_RESERVE};
use std::path::PathBuf;

const LABELS_V1: &str = "labels/test.onyx.entities.btc.v1.json";
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const SCAN: &str = "reserves/scan-971199.json";
const BASE: &str = "reserves/base-969838.json";
const HEADERS: &str = "reserves/headers-969839-971207.json";
const START: u64 = 969_838;
/// 1,000 BTC.
const FORGED_EXTRA: u64 = 100_000_000_000;
/// `ScanSnapshot::hash` of the committed scan, as the website pins it
/// (`apps/web/verify` `RESERVES_SNAPSHOT`): what a client trusts.
const TRUSTED_SCAN_HASH: &str =
    "0x01fd0e6a76c9bcdf722a498cbea2945a24ffd2227ab9991aab9523c84afad87b";
const ENTITIES: [&str; 4] = [
    "entity:binance",
    "entity:bitmex",
    "entity:bitwise-bitb",
    "entity:okx",
];

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn json(rel: &str) -> serde_json::Value {
    let fx = root().join("fixtures");
    verify_checksum(&fx, rel).expect("checksum");
    serde_json::from_slice(&std::fs::read(fx.join(rel)).expect("read")).expect("json")
}

fn labels(rel: &str) -> LabelSet {
    load_label_set(&root().join("fixtures"), rel).expect("labels")
}

/// The client's own header chain (proof of work checked).
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

/// `client_check` against the client's own headers.
fn check(
    p: &ReservePin,
    entity: &str,
    links: &[ReserveLink],
) -> Result<onyx_source_btc::reserve::ClientVerdict, ReserveError> {
    client_check(p, entity, links, headers())
}

fn scan() -> ScanSnapshot {
    serde_json::from_value(json(SCAN)).expect("scan")
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

fn trusted() -> Hash32 {
    let mut b = [0u8; 32];
    hex::decode_to_slice(&TRUSTED_SCAN_HASH[2..], &mut b).expect("hex");
    Hash32(b)
}

fn pin() -> ReservePin {
    let p =
        ReservePin::from_scan(&scan(), &labels(LABELS_V2), recipe_id(), &trusted()).expect("pin");
    assert_eq!(
        p.scan_hash.to_string(),
        TRUSTED_SCAN_HASH,
        "the trusted pin"
    );
    p
}

fn committed_base() -> Vec<EntityBase> {
    json(BASE)["base"]
        .as_array()
        .expect("base")
        .iter()
        .map(|b| EntityBase {
            entity: b["entity"].as_str().expect("entity").into(),
            balance_sats: b["balance_sats"].as_str().expect("s").parse().expect("n"),
        })
        .collect()
}

fn rows_of(leaves: &[Vec<u8>]) -> Vec<EntityReserveRow> {
    leaves
        .iter()
        .map(|l| {
            EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                .expect("row")
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

/// One finalized shard: its range, tip, every leaf and its root.
#[derive(Clone)]
struct Shard {
    round: u64,
    from: u64,
    to: u64,
    tip: Hash32,
    leaves: Vec<Vec<u8>>,
    root: Hash32,
    recipe: Hash32,
    label_set: Hash32,
}

/// The header chain's block at `to` (the landing's is the scan's block).
fn tip_at(to: u64) -> Hash32 {
    let mut b = [0u8; 32];
    hex::decode_to_slice(headers().hash_at(to).expect("header"), &mut b).expect("block hash");
    Hash32(b)
}

/// The committed chain as the chain finalized it.
fn honest() -> Vec<Shard> {
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/reserves-week.json")).expect("export"),
    )
    .expect("export json");
    let set = labels(LABELS_V2);
    doc["shards"]
        .as_array()
        .expect("shards")
        .iter()
        .enumerate()
        .map(|(k, x)| {
            let leaves: Vec<Vec<u8>> = x["leaves"]
                .as_array()
                .expect("leaves")
                .iter()
                .map(|l| {
                    hex::decode(l.as_str().expect("leaf").trim_start_matches("0x")).expect("hex")
                })
                .collect();
            let to = x["to"].as_u64().expect("to");
            Shard {
                round: k as u64 + 1,
                from: x["from"].as_u64().expect("from"),
                to,
                tip: tip_at(to),
                root: MerkleTree::from_rows(&leaves).root(),
                leaves,
                recipe: recipe_id(),
                label_set: set_hash(&set),
            }
        })
        .collect()
}

/// Rebuild `shards[from..]` so each starts on the previous one's end and
/// root (as the recipe would rerun them on those params): `edit` changes the
/// rows of shard `from` first, the later shards follow it. The first shard
/// of a rebuilt chain starts on `start_ref` when `from` is 0.
fn rechain(
    shards: &mut [Shard],
    from: usize,
    start_ref: Option<Hash32>,
    edit: impl Fn(&mut EntityReserveRow),
) {
    let mut prev: Option<(Hash32, Vec<EntityReserveRow>)> = match from {
        0 => None,
        k => Some((shards[k - 1].root, rows_of(&shards[k - 1].leaves))),
    };
    for (k, s) in shards.iter_mut().enumerate().skip(from) {
        let mut rows = rows_of(&s.leaves);
        for (i, r) in rows.iter_mut().enumerate() {
            match &prev {
                Some((root, p)) => {
                    r.base_ref = root.0;
                    r.base_sats = p[i].balance_sats;
                }
                None => {
                    if let Some(h) = start_ref {
                        r.base_ref = h.0;
                    }
                }
            }
            if k == from {
                edit(r);
            }
            r.balance_sats =
                (i128::from(r.base_sats) + r.received_sats as i128 - r.sent_sats as i128) as u64;
        }
        s.leaves = leaves_of(&rows);
        s.root = MerkleTree::from_rows(&s.leaves).root();
        prev = Some((s.root, rows));
    }
}

/// The links a client holds for `entity` after proving each one.
fn links(shards: &[Shard], entity: &str) -> Vec<ReserveLink> {
    let i = ENTITIES.iter().position(|e| *e == entity).expect("entity");
    shards
        .iter()
        .map(|s| ReserveLink {
            round: s.round,
            shard: keccak256(format!("shard {}-{} {}", s.from, s.to, s.round).as_bytes()),
            root: s.root,
            recipe: s.recipe,
            label_set_hash: s.label_set,
            schema: keccak256(SCHEMA_ENTITY_RESERVE),
            from: s.from,
            to: s.to,
            tip: s.tip,
            index: i as u64,
            leaf: s.leaves[i].clone(),
        })
        .collect()
}

fn refused(r: Result<impl std::fmt::Debug, ReserveError>, why: &str) {
    match r {
        Ok(v) => panic!("accepted, wanted a refusal with {why:?}: {v:?}"),
        Err(e) => assert!(e.to_string().contains(why), "{e}"),
    }
}

/// R-RES-CLIENT-1: the committed chain passes the client's check for every
/// entity, and the start base it derives from the pinned scan and the flows
/// alone is the committed base (the same base the operator's check derives).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_client_check_derives_the_committed_base_from_the_scan() {
    let p = pin();
    let shards = honest();
    assert_eq!(shards.len(), 89);
    for (e, want) in ENTITIES.iter().zip(committed_base()) {
        let v = check(&p, e, &links(&shards, e)).expect("passes");
        assert_eq!(want.entity, *e);
        assert_eq!(v.chain.start_base_sats, want.balance_sats, "{e}: derived");
        assert_eq!(v.chain.start_height, START);
        assert_eq!(v.chain.anchor, SpanEnd::Landing(88));
        assert_eq!(v.rows.len(), 89);
    }
}

/// ROADMAP D22 (THREATS D3-R2-2): a span from the landing. The links need
/// not start at the chain's start: any suffix that reaches the landing
/// derives every base from the scan's total and the flows, so a check pays
/// only the links between the answer and the landing (one for an answer at
/// the landing, 89 for the week's oldest), and every base it derives is the
/// committed chain's.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_a_span_from_the_landing_pays_only_the_links_to_the_answer() {
    let p = pin();
    let shards = honest();
    for e in ENTITIES {
        let l = links(&shards, e);
        for k in [0, 1, 40, 87, 88] {
            let v = check(&p, e, &l[k..]).expect("a span from the landing");
            assert_eq!(v.chain.anchor, SpanEnd::Landing(88 - k));
            assert_eq!(v.chain.start_height, shards[k].from - 1);
            let row =
                &rows_of(&shards[k].leaves)[ENTITIES.iter().position(|x| *x == e).expect("e")];
            assert_eq!(v.chain.start_base_sats, row.base_sats, "{e} at link {k}");
            assert_eq!(v.rows.len(), 89 - k, "paid rows");
        }
    }
    // A forged middle link is still refused in a suffix that holds it, and
    // a span that does not reach the landing has no anchor.
    let mut s = honest();
    rechain(&mut s, 60, None, |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")[50..]),
        "base is not the previous link's end balance",
    );
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")[60..]),
        "base is not the one derived from the committed scan and the finalized flows",
    );
    refused(
        check(&p, "entity:okx", &links(&shards, "entity:okx")[40..60]),
        "no link ends at the committed scan's height",
    );
}

/// ROADMAP D22: a newer scan can be pinned. The chain starts on the scan at
/// 971,199 (the first `base_ref`); a client that trusts a scan at the end
/// of link 60 checks links 60-70 for an answer at link 70 (11 rows), where
/// D3 refused every chain whose first `base_ref` was not its pin's. The
/// newer scan here is synthetic per address (one row per entity holding the
/// committed chain's balance at that height, so it reconciles at the entity
/// level only); the per-address case is `reserve_dated.rs`
/// `d22_a_newer_reconciled_scan_can_be_pinned`.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_a_newer_scan_pins_a_span_without_the_chains_start() {
    let set = labels(LABELS_V2);
    let shards = honest();
    let at = shards[60].to;
    let ends = rows_of(&shards[60].leaves);
    let mut rows: Vec<ScanRow> = ends
        .iter()
        .filter(|r| r.balance_sats > 0)
        .map(|r| ScanRow {
            address: set
                .labels
                .iter()
                .filter(|l| l.entity == r.entity)
                .map(|l| l.address.clone())
                .min()
                .expect("an address"),
            entity: r.entity.clone(),
            utxos: 1,
            sats: r.balance_sats,
        })
        .collect();
    rows.sort_by(|a, b| a.address.cmp(&b.address));
    let newer = ScanSnapshot {
        version: 1,
        method: "synthetic newer scan (tests/reserve_client.rs)".into(),
        height: at,
        block_hash: headers().hash_at(at).expect("h").into(),
        rows,
        ..scan()
    };
    let p60 = ReservePin::from_scan(&newer, &set, recipe_id(), &newer.hash()).expect("pin");
    for e in ENTITIES {
        let l = links(&shards, e);
        let v = check(&p60, e, &l[60..=70]).expect("the newer pin");
        assert_eq!(v.chain.anchor, SpanEnd::Landing(0));
        assert_eq!(v.rows.len(), 11);
        check(&p60, e, &l[50..=60]).expect("before it too");
        check(&p60, e, &l).expect("and the whole chain");
    }
    let mut s = honest();
    rechain(&mut s, 0, None, |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    refused(
        check(&p60, "entity:okx", &links(&s, "entity:okx")[60..=70]),
        "base is not the one derived from the committed scan and the finalized flows",
    );
}

/// ROADMAP D22 (THREATS RES-C6, RES-4): every link's tip must be the
/// client's header chain's block at its height, not only the landing's: a
/// link over a stale branch before the landing is refused; and a pin whose
/// scan the client's headers cannot confirm six deep is refused (that case
/// is the pin's check; a link under six confirmations, which this chain
/// cannot have before its scan, is `reserve_dated.rs`'s block 971,202).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_every_links_tip_must_be_on_the_clients_header_chain() {
    let p = pin();
    let mut s = honest();
    s[40].tip = tip_at(s[40].to - 1);
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "a stale branch (RES-4)",
    );
    let short = headers().truncated(p.scan_height + 4).expect("short");
    refused(
        client_check(&p, "entity:okx", &links(&honest(), "entity:okx"), &short),
        "6 needed",
    );
}

/// R-RES-CLIENT-2 (RES-10, "operator skips the check"): a forged-base
/// shard the chain finalized (miner and validators rerun any base) is served
/// by an operator who skips the serving check; its row verifies against its
/// root, the circular check passes, and the client refuses it: in the
/// middle of the chain (its base is not the previous end), as a whole chain
/// raised by 1,000 BTC from the start (its base is not the one derived from
/// the scan), and as a forged last link.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_forged_base_is_refused_by_the_client_whatever_the_operator_serves() {
    let p = pin();
    // One forged shard in the middle, the rest honest after it (a dishonest
    // operator lists the forged shard in place of the honest one).
    let mut s = honest();
    rechain(&mut s, 30, None, |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "entity:okx: base is not the previous link's end balance",
    );
    // A whole chain on a forged start: every link consistent with the one
    // before (the operator's circular check passes it), landing 1,000 BTC
    // above the scan.
    let mut s = honest();
    rechain(&mut s, 0, None, |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    let forged_start: Vec<EntityBase> = committed_base()
        .into_iter()
        .map(|mut b| {
            if b.entity == "entity:okx" {
                b.balance_sats += FORGED_EXTRA;
            }
            b
        })
        .collect();
    let chain: Vec<ReserveShard> = s
        .iter()
        .map(|x| ReserveShard {
            from: x.from,
            to: x.to,
            root: x.root,
            rows: rows_of(&x.leaves),
        })
        .collect();
    assert!(
        check_chain(START, &forged_start, &p.scan_hash, &chain).is_ok(),
        "circular: the chain checks against its own base"
    );
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "base is not the one derived from the committed scan and the finalized flows",
    );
    // The other entities' rows in the same forged shards are honest.
    check(&p, "entity:bitwise-bitb", &links(&s, "entity:bitwise-bitb")).expect("bitb");
    // A forged landing: the last link's base raised, its balance with it.
    let mut s = honest();
    rechain(&mut s, 88, None, |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "base is not the previous link's end balance",
    );
}

/// R-RES-CLIENT-3 (RES-10, "swapped scan"): an operator builds and
/// finalizes a chain on another scan (a forged file with OKX 1,000 BTC
/// higher; its hash in the first link's `base_ref`) that lands on that
/// scan's totals. Against the client's own pin it is refused. A client that
/// took the scan from the operator would have accepted it: the pin must be
/// one the client trusts (this test pins the repository's hash).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_chain_on_a_swapped_scan_is_refused() {
    let p = pin();
    let set = labels(LABELS_V2);
    let mut other = scan();
    let i = other
        .rows
        .iter()
        .position(|r| r.entity == "entity:okx")
        .expect("an okx row");
    other.rows[i].sats += FORGED_EXTRA;
    // A client that pins the hash it trusts cannot be handed this scan.
    refused(
        ReservePin::from_scan(&other, &set, recipe_id(), &trusted()),
        "not the one the client trusts",
    );
    // One that took the operator's hash as "trusted" (the mistake the
    // argument exists to prevent) gets the operator's anchor.
    let swapped = ReservePin::from_scan(&other, &set, recipe_id(), &other.hash())
        .expect("a well-formed scan");
    assert_ne!(swapped.scan_hash.to_string(), TRUSTED_SCAN_HASH);
    let mut s = honest();
    rechain(&mut s, 0, Some(swapped.scan_hash), |r| {
        if r.entity == "entity:okx" {
            r.base_sats += FORGED_EXTRA;
        }
    });
    let l = links(&s, "entity:okx");
    refused(
        check(&p, "entity:okx", &l),
        "base is not the one derived from the committed scan and the finalized flows",
    );
    // The residual: the operator's own scan file anchors its own chain.
    check(&swapped, "entity:okx", &l).expect("only the pin tells them apart");
    // The honest chain is refused against the swapped pin, too.
    refused(
        check(&swapped, "entity:okx", &links(&honest(), "entity:okx")),
        "base is not the one derived from the committed scan and the finalized flows",
    );
}

/// R-RES-CLIENT-4 ("wrong label set version"): links bound to another
/// label set (v1, or any other content) are refused, and a pin cannot be
/// built from a scan over another label set version.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_chain_over_another_label_set_version_is_refused() {
    let p = pin();
    let v1 = labels(LABELS_V1);
    let mut s = honest();
    s[10].label_set = set_hash(&v1);
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "another label set than the pinned scan's",
    );
    refused(
        ReservePin::from_scan(&scan(), &v1, recipe_id(), &trusted()),
        "label set",
    );
    // An entity the pinned set does not name.
    refused(
        check(&p, "entity:coinbase", &links(&honest(), "entity:okx")),
        "not an entity of the pinned label set",
    );
}

/// R-RES-CLIENT-5: the rest of the binding and the chain's shape: another
/// recipe, a landing on another block at the scan's height (RESAPI-R1-2), a
/// missing link, a chain that stops before the scan, another entity's leaf,
/// a row naming another range than its shard.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_client_check_binds_recipe_landing_block_and_tiling() {
    let p = pin();
    let ok = honest();
    let mut s = ok.clone();
    s[5].recipe = keccak256(b"another reserve build");
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "another recipe than the pinned btc-entity-reserve build",
    );
    let mut s = ok.clone();
    s[88].tip = keccak256(b"another block at 971199");
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "ends on another block than the committed scan's",
    );
    let mut l = links(&ok, "entity:okx");
    l.remove(20);
    refused(
        check(&p, "entity:okx", &l),
        "does not start after the previous link",
    );
    let l = links(&ok, "entity:okx");
    refused(
        check(&p, "entity:okx", &l[..88]),
        "no link ends at the committed scan's height",
    );
    let mut l = links(&ok, "entity:okx");
    l[3].leaf = ok[3].leaves[0].clone();
    refused(check(&p, "entity:okx", &l), "not the entity's row");
    let mut l = links(&ok, "entity:okx");
    l[3].index = 0;
    refused(check(&p, "entity:okx", &l), "not the entity's leaf index");
    // A row whose u128 flows would wrap in a signed sum.
    let mut s = ok.clone();
    rechain(&mut s, 50, None, |r| {
        if r.entity == "entity:okx" {
            r.received_sats += 1u128 << 127;
            r.sent_sats += 1u128 << 127;
        }
    });
    refused(
        check(&p, "entity:okx", &links(&s, "entity:okx")),
        "flows above the money supply",
    );
    // A link naming one more block: its tip is not the header chain's
    // block there (D22), and with that tip too, its row names another range.
    let mut l = links(&ok, "entity:okx");
    l[7].to += 1;
    refused(check(&p, "entity:okx", &l), "a stale branch");
    l[7].tip = tip_at(l[7].to);
    refused(
        check(&p, "entity:okx", &l),
        "the row names another range than its shard",
    );
}
