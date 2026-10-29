//! ROADMAP D22: the client's list check on a dated label set
//! (`onyx_reserve_serve::client::link_requests`). Synthetic set, scan and
//! list (no row is fetched here; the chain check itself is
//! `onyx-source-btc` `tests/reserve_dated.rs`):
//!
//! | address | entity | from | to |
//! |---|---|---|---|
//! | `btc:aa` | z | 971,100 | open |
//! | `btc:bb` | a | 971,151 | open (joins: `a` sorts first, so `z`'s leaf index moves) |
//! | `btc:cc` | zz | 971,100 | 971,150 (exits) |
//!
//! Listed ranges: A 971,101-971,150, B 971,151-971,199 (the scan's height:
//! the landing), C 971,200-971,201 (the set's publication). Real heights, so
//! the client's own header chain (`fixtures/reserves/headers-969839-971207.json`)
//! confirms the pin and the answer before any fetch (review D22-R1-1).

use onyx_core::{keccak256, Confidence, Hash32};
use onyx_labels::dated::{DatedLabelSet, Membership};
use onyx_labels::{EntityKind, Label};
use onyx_reserve_serve::client::{link_requests, Limits, DEFAULT_SPAN_BUDGET};
use onyx_reserve_serve::GATE;
use onyx_source_btc::reserve::{HeaderChain, ReservePin, ScanRow, ScanSnapshot};
use serde_json::{json, Value};

const HEADERS: &str = "reserves/headers-969839-971207.json";

fn headers() -> &'static HeaderChain {
    static H: std::sync::OnceLock<HeaderChain> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let fx = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"));
        onyx_source_btc::verify_checksum(&fx, HEADERS).expect("checksum");
        HeaderChain::from_json(
            &std::fs::read_to_string(fx.join(HEADERS)).expect("read"),
            onyx_source_btc::node::MAINNET_MIN_BITS,
        )
        .expect("headers")
    })
}

fn m(addr: &str, entity: &str, from: u64, to: Option<u64>) -> Membership {
    Membership {
        label: Label {
            address: format!("btc:{addr}"),
            entity: format!("entity:{entity}"),
            kind: EntityKind::Exchange,
            confidence: Confidence(900_000),
            evidence: vec![],
        },
        from,
        to,
    }
}

fn pin() -> ReservePin {
    pin_on(headers().hash_at(971_199).expect("h"))
}

/// The pin of a scan at 971,199 naming `block` as its block.
fn pin_on(block: &str) -> ReservePin {
    let set = DatedLabelSet {
        name: "test.onyx.dated".into(),
        version: 1,
        published: 971_201,
        members: vec![
            m("aa", "z", 971_100, None),
            m("bb", "a", 971_151, None),
            m("cc", "zz", 971_100, Some(971_150)),
        ],
    };
    set.check_structure().expect("canonical");
    let row = |a: &str, e: &str, sats: u64| ScanRow {
        address: format!("btc:{a}"),
        entity: format!("entity:{e}"),
        utxos: 1,
        sats,
    };
    let scan = ScanSnapshot {
        version: 1,
        method: "synthetic (tests/client_dated.rs)".into(),
        height: 971_199,
        block_hash: block.into(),
        label_set_name: set.name.clone(),
        label_set_version: 1,
        label_set_hash: format!("0x{}", hex::encode(set.content_hash().0)),
        addresses_scanned: 3,
        rows: vec![row("aa", "z", 10), row("bb", "a", 5), row("cc", "zz", 3)],
    };
    ReservePin::from_dated_scan(&scan, &set, &[], None, Hash32::ZERO, &scan.hash(), None)
        .expect("pin")
}

fn root(name: &str) -> Hash32 {
    keccak256(name.as_bytes())
}

fn list(ranges: &[(&str, u64, u64)]) -> Value {
    json!({
        "gate": GATE,
        "shards": ranges
            .iter()
            .enumerate()
            .map(|(i, (n, from, to))| json!({
                "round": i as u64 + 1,
                "shard": keccak256(format!("shard {n}").as_bytes()).to_string(),
                "from": from,
                "to": to,
                "root": root(n).to_string(),
            }))
            .collect::<Vec<_>>(),
    })
}

fn abc() -> Value {
    list(&[
        ("A", 971_101, 971_150),
        ("B", 971_151, 971_199),
        ("C", 971_200, 971_201),
    ])
}

fn answer(range: &str, index: u64) -> Value {
    json!({"root": root(range).to_string(), "index": index})
}

fn with(
    p: &ReservePin,
    l: &Value,
    entity: &str,
    a: &Value,
    limits: Limits,
) -> Result<Vec<(u64, u64)>, String> {
    Ok(
        link_requests(l, p, &format!("entity:{entity}"), a, headers(), &limits)?
            .into_iter()
            .map(|r| (r.round, r.index))
            .collect(),
    )
}

fn spans(l: &Value, entity: &str, a: &Value, budget: usize) -> Result<Vec<(u64, u64)>, String> {
    let limits = Limits {
        budget,
        ..Limits::default()
    };
    with(&pin(), l, entity, a, limits)
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_the_list_check_takes_each_links_index_from_its_window() {
    let abc = abc();
    let b = DEFAULT_SPAN_BUDGET;
    // `z` is leaf 0 before `a` joins and leaf 1 after: A to the landing.
    assert_eq!(
        spans(&abc, "z", &answer("A", 0), b),
        Ok(vec![(1, 0), (2, 1)])
    );
    let e = spans(&abc, "z", &answer("A", 1), b).expect_err("index");
    assert!(e.contains("leaf index"), "{e}");
    // `zz` exits after A: its span stops there (anchored where it leaves).
    assert_eq!(spans(&abc, "zz", &answer("A", 1), b), Ok(vec![(1, 1)]));
    // `a` from the landing; after it, back to the landing.
    assert_eq!(spans(&abc, "a", &answer("B", 0), b), Ok(vec![(2, 0)]));
    assert_eq!(
        spans(&abc, "a", &answer("C", 0), b),
        Ok(vec![(2, 0), (3, 0)])
    );
    // `a` has no address in A: no row to answer with there.
    let e = spans(&abc, "a", &answer("A", 0), b).expect_err("absent");
    assert!(e.contains("leaf index"), "{e}");
    // Over the client's budget: refused before any fetch.
    let e = spans(&abc, "z", &answer("A", 0), 1).expect_err("budget");
    assert!(e.contains("budget of 1 links"), "{e}");
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_the_list_check_refuses_a_range_across_a_change_or_past_publication() {
    let across = list(&[("A", 971_101, 971_160), ("B", 971_161, 971_199)]);
    let e = spans(&across, "z", &answer("A", 0), DEFAULT_SPAN_BUDGET).expect_err("across");
    assert!(e.contains("membership change"), "{e}");
    let past = list(&[
        ("A", 971_101, 971_150),
        ("B", 971_151, 971_199),
        ("C", 971_200, 971_202),
    ]);
    let e = spans(&past, "z", &answer("C", 1), DEFAULT_SPAN_BUDGET).expect_err("past");
    assert!(e.contains("publication"), "{e}");
    let e = spans(&past, "q", &answer("B", 0), DEFAULT_SPAN_BUDGET).expect_err("entity");
    assert!(e.contains("not an entity of the pinned label set"), "{e}");
}

/// Review D22-R1-1 and D22-R1-2: what the client can refuse for free is
/// refused before the first paid fetch: an answer older than its
/// `min_as_of`, an answer its headers cannot confirm six deep, a stale pin
/// (its scan's block not the header chain's), a list longer than it parses.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_cheap_refusals_come_before_any_fetch() {
    let l = abc();
    let p = pin();
    let lim = |min_as_of: u64, max_listed: usize| Limits {
        min_as_of,
        max_listed,
        ..Limits::default()
    };
    let e = with(&p, &l, "z", &answer("A", 0), lim(971_199, 1 << 20)).expect_err("stale");
    assert!(e.contains("before min_as_of 971199"), "{e}");
    assert!(with(&p, &l, "z", &answer("B", 1), lim(971_199, 1 << 20)).is_ok());
    let deep = list(&[
        ("A", 971_101, 971_150),
        ("B", 971_151, 971_199),
        ("C", 971_200, 971_203),
    ]);
    let e = with(&p, &deep, "z", &answer("C", 1), Limits::default()).expect_err("depth");
    assert!(e.contains("not 6 confirmations deep"), "{e}");
    let stale = pin_on(headers().hash_at(971_198).expect("h"));
    let e = with(&stale, &l, "z", &answer("B", 1), Limits::default()).expect_err("pin");
    assert!(e.contains("the pin is not on the client's headers"), "{e}");
    let e = with(&p, &l, "z", &answer("B", 1), lim(0, 2)).expect_err("long");
    assert!(e.contains("more than the client parses (2)"), "{e}");
    // No cliff at the old `MAX_LINKS` (16,384): a 20,002-range list parses,
    // and an answer at its landing costs one link.
    let mut long: Vec<(String, u64, u64)> = (0..20_000u64)
        .map(|i| (format!("old {i}"), i * 2 + 1, i * 2 + 2))
        .collect();
    long.push(("A".into(), 40_001, 971_150));
    long.push(("B".into(), 971_151, 971_199));
    let named: Vec<(&str, u64, u64)> = long.iter().map(|(n, f, t)| (n.as_str(), *f, *t)).collect();
    assert_eq!(
        with(&p, &list(&named), "z", &answer("B", 1), Limits::default()),
        Ok(vec![(20_002, 1)])
    );
}

/// Review D22-R2-3: `max_listed` bounds the list's body before it is
/// parsed (`parse_served`), not only the entries of a `Value` already in
/// memory.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn d22_r2_the_served_body_is_bounded_before_it_is_parsed() {
    use onyx_reserve_serve::client::{parse_served, MAX_LISTED_ENTRY_BYTES};
    let body = serde_json::to_vec(&abc()).expect("json");
    let l = parse_served(&body, &Limits::default()).expect("parses");
    assert_eq!(l, abc());
    assert_eq!(
        with(&pin(), &l, "a", &answer("B", 0), Limits::default()),
        Ok(vec![(2, 0)])
    );
    let tight = Limits {
        max_listed: 2,
        ..Limits::default()
    };
    // Not even JSON: refused on its length alone, before any parsing.
    let junk = vec![b'['; 2 * MAX_LISTED_ENTRY_BYTES + 1025];
    let e = parse_served(&junk, &tight).expect_err("too long");
    assert!(e.contains("more than 2 entries take"), "{e}");
    assert!(parse_served(&junk[..10], &tight)
        .expect_err("not JSON")
        .contains("not JSON"));
}
