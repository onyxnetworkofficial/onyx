//! Early-contributor record tests (ADR-0065, THREATS EARLY): one positive and
//! one negative fixture per kind, relayed work, order and repeats of logs,
//! the UTC day boundary, the exclusions, `contracts.json` against the
//! deployment documents, the event signatures against the contract sources,
//! and the CSV built into real bulk versions. No chain reads.

use super::*;
use onyx_bulk::input::{Format, Records};
use onyx_bulk::manifest::{Manifest, NewVersion, ShardEntry};
use onyx_bulk::row::{encode_row, key_of, MAX_ROW_BYTES};
use onyx_bulk::shard::{check_shard, encode_shard, ShardBuilder};
use onyx_core::{DataRights, Redistribution, SourceId};
use onyx_storage::abi::Tok;
use std::path::PathBuf;

/// 2026-11-14.
pub(crate) const DAY: u32 = 20_771;
/// 2026-11-14 00:00:00 UTC; block [`FROM`] is at it, one block a second.
pub(crate) const T0: u64 = DAY as u64 * 86_400;
pub(crate) const FROM: u64 = 100;

pub(crate) const SETTLE: [u8; 20] = [0x11; 20];
pub(crate) const DISPUTES: [u8; 20] = [0x12; 20];
pub(crate) const BOOK: [u8; 20] = [0x13; 20];
pub(crate) const STORAGE: [u8; 20] = [0x14; 20];
pub(crate) const STAKES: [u8; 20] = [0x15; 20];
pub(crate) const CORR: [u8; 20] = [0x16; 20];
pub(crate) const REGISTRY: [u8; 20] = [0x17; 20];

pub(crate) const MINER_A: [u8; 20] = [0xa1; 20];
pub(crate) const MINER_B: [u8; 20] = [0xa2; 20];
pub(crate) const VALIDATOR: [u8; 20] = [0xb1; 20];
pub(crate) const VALIDATOR_2: [u8; 20] = [0xb2; 20];
pub(crate) const SIGNER: [u8; 20] = [0xc1; 20];
pub(crate) const SIGNER_2: [u8; 20] = [0xc2; 20];
pub(crate) const REVIEWER: [u8; 20] = [0xc9; 20];
pub(crate) const RELAYER: [u8; 20] = [0xd1; 20];
pub(crate) const OPERATOR: [u8; 20] = [0xe1; 20];
pub(crate) const PAYER: [u8; 20] = [0xe2; 20];
pub(crate) const REPORTER: [u8; 20] = [0xe3; 20];
pub(crate) const SUBMITTER: [u8; 20] = [0xf1; 20];
pub(crate) const BETTOR_NO: [u8; 20] = [0xf2; 20];
pub(crate) const BETTOR_YES: [u8; 20] = [0xf3; 20];
pub(crate) const OPENER: [u8; 20] = [0xf4; 20];

pub(crate) fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub(crate) fn test_contracts_json(storage_from: u64) -> String {
    let e = |name: &str, abi: &str, a: &[u8; 20], from: Option<u64>| {
        let mut v = json!({"name": name, "abi": abi, "address": addr_text(a), "doc": "test"});
        if let Some(f) = from {
            v["scan_from"] = json!(f);
            v["scan_from_is"] = json!("deploy");
            v["block_doc"] = json!("test");
            v["block_quote"] = json!(format!("block {f}"));
        }
        v
    };
    json!({
        "format": 1,
        "chain_id": 31337,
        "network": "test",
        "contracts": [
            e("OnyxSettlement", "settlement-v1", &SETTLE, Some(FROM)),
            e("OnyxDisputes", "disputes-v1", &DISPUTES, Some(FROM)),
            e("OnyxSignedBook", "signed-book-v1", &BOOK, Some(FROM)),
            e("OnyxStorage", "storage-v3", &STORAGE, Some(storage_from)),
            e("OnyxStakes", "stakes-v1", &STAKES, Some(FROM)),
            e("OnyxCorrections", "corrections-v1", &CORR, Some(FROM)),
            e("OnyxRegistry", "registry-v1", &REGISTRY, None),
        ]
    })
    .to_string()
}

pub(crate) fn contracts() -> Contracts {
    Contracts::parse(test_contracts_json(FROM).as_bytes()).expect("contracts")
}

pub(crate) fn word(v: u128) -> [u8; 32] {
    abi::word_u128(v)
}

pub(crate) fn aword(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

pub(crate) fn h(n: u8) -> Hash32 {
    Hash32([n; 32])
}

pub(crate) fn tu(v: u64) -> Hash32 {
    Hash32(word(u128::from(v)))
}

pub(crate) fn ta(a: &[u8; 20]) -> Hash32 {
    Hash32(aword(a))
}

pub(crate) fn tp(s: &str) -> Hash32 {
    abi::topic(s)
}

pub(crate) fn tx_of(n: u64) -> Hash32 {
    keccak256(&n.to_be_bytes())
}

/// A log at `block`, position `idx`, in transaction `tx`.
pub(crate) fn lgt(
    addr: [u8; 20],
    block: u64,
    idx: u64,
    tx: u64,
    topics: Vec<Hash32>,
    data: Vec<u8>,
) -> ChainLog {
    ChainLog {
        address: addr,
        topics,
        data,
        block,
        block_time: T0 + block - FROM,
        tx: tx_of(tx),
        log_index: idx,
        from: None,
    }
}

/// A log in its own transaction.
pub(crate) fn lg(
    addr: [u8; 20],
    block: u64,
    idx: u64,
    topics: Vec<Hash32>,
    words: Vec<[u8; 32]>,
) -> ChainLog {
    lgt(addr, block, idx, block * 1000 + idx, topics, words.concat())
}

pub(crate) fn committed(b: u64, i: u64, round: u64, shard: Hash32, miner: [u8; 20]) -> ChainLog {
    lg(
        SETTLE,
        b,
        i,
        vec![tp(sig::COMMITTED), tu(round), shard, ta(&miner)],
        vec![[7; 32]],
    )
}

pub(crate) fn revealed(
    b: u64,
    i: u64,
    round: u64,
    shard: Hash32,
    miner: [u8; 20],
    root: Hash32,
) -> ChainLog {
    lg(
        SETTLE,
        b,
        i,
        vec![tp(sig::REVEALED), tu(round), shard, ta(&miner)],
        vec![root.0],
    )
}

pub(crate) fn verdict(
    b: u64,
    i: u64,
    round: u64,
    shard: Hash32,
    v: [u8; 20],
    kind: u8,
) -> ChainLog {
    lg(
        SETTLE,
        b,
        i,
        vec![tp(sig::VERDICT_REVEALED), tu(round), shard, ta(&v)],
        vec![word(u128::from(kind))],
    )
}

pub(crate) fn finalized(b: u64, i: u64, round: u64, shard: Hash32, root: Hash32) -> ChainLog {
    lg(
        SETTLE,
        b,
        i,
        vec![tp(sig::FINALIZED), tu(round), shard],
        vec![root.0, [9; 32]],
    )
}

pub(crate) fn released(b: u64, i: u64, round: u64, shard: Hash32) -> ChainLog {
    lg(
        DISPUTES,
        b,
        i,
        vec![tp(sig::RELEASED), tu(round), shard],
        vec![],
    )
}

pub(crate) fn key_registered(
    b: u64,
    i: u64,
    key: Hash32,
    signer: [u8; 20],
    by: [u8; 20],
) -> ChainLog {
    let data = abi::encode(&[
        Tok::Bytes(b"Example Person Source".to_vec()),
        Tok::Bytes(b"example.org".to_vec()),
        Tok::Word([5; 32]),
        abi::addr(&by),
        Tok::Bytes(b"reviewed, contact jane".to_vec()),
    ]);
    lgt(
        BOOK,
        b,
        i,
        b * 1000 + i,
        vec![tp(sig::KEY_REGISTERED), key, ta(&signer)],
        data,
    )
}

pub(crate) fn report_tx(
    b: u64,
    i: u64,
    tx: u64,
    digest: Hash32,
    key: Hash32,
    outcome: u8,
) -> ChainLog {
    lgt(
        BOOK,
        b,
        i,
        tx,
        vec![tp(sig::REPORT_RECORDED), digest, key, h(0xfa)],
        [word(3), [0x3e; 32], word(u128::from(outcome))].concat(),
    )
}

pub(crate) fn report(b: u64, i: u64, digest: Hash32, key: Hash32, outcome: u8) -> ChainLog {
    report_tx(b, i, b * 1000 + i, digest, key, outcome)
}

pub(crate) fn withdrawn(b: u64, i: u64, tx: u64, digest: Hash32) -> ChainLog {
    lgt(BOOK, b, i, tx, vec![tp(sig::WITHDRAWN), digest], vec![])
}

pub(crate) fn equivocated(b: u64, i: u64, tx: u64, first: Hash32, second: Hash32) -> ChainLog {
    lgt(
        BOOK,
        b,
        i,
        tx,
        vec![tp(sig::EQUIVOCATED), h(0xfa), first, second],
        word(1).to_vec(),
    )
}

pub(crate) fn key_event(s: &str, b: u64, i: u64, key: Hash32, t: u64) -> ChainLog {
    let data = abi::encode(&[
        abi::uint(u128::from(t)),
        abi::addr(&REVIEWER),
        Tok::Bytes(b"why".to_vec()),
    ]);
    lgt(BOOK, b, i, b * 1000 + i, vec![tp(s), key], data)
}

pub(crate) fn activated(b: u64, i: u64, node: Hash32, op: [u8; 20]) -> ChainLog {
    lg(
        STORAGE,
        b,
        i,
        vec![tp(sig::ACTIVATED), node, tu(1), ta(&op)],
        vec![],
    )
}

pub(crate) fn deal(b: u64, i: u64, id: u64, payer: [u8; 20]) -> ChainLog {
    let data = abi::encode(&[
        Tok::Word([1; 32]),
        Tok::Word([2; 32]),
        abi::uint(1),
        abi::uint(100),
        abi::uint(3),
        abi::uint(2),
        abi::uint(9),
        Tok::Word([3; 32]),
        abi::uint(24),
        Tok::Bytes(b"example.publisher".to_vec()),
        Tok::Bytes(b"btc.price".to_vec()),
        Tok::Bytes(b"https://example.org".to_vec()),
    ]);
    lgt(
        STORAGE,
        b,
        i,
        b * 1000 + i,
        vec![tp(sig::DEAL_CREATED), tu(id), ta(&payer)],
        data,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn answered(
    b: u64,
    i: u64,
    deal: u64,
    epoch: u64,
    slot: u32,
    node: Hash32,
    passed: bool,
    paid: u128,
) -> ChainLog {
    lg(
        STORAGE,
        b,
        i,
        vec![tp(sig::ANSWERED), tu(deal), tu(epoch), node],
        vec![
            word(u128::from(slot)),
            word(0),
            word(u128::from(passed)),
            word(paid),
        ],
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn slashed(
    b: u64,
    i: u64,
    tx: u64,
    node: Hash32,
    deal: u64,
    epoch: u64,
    slot: u32,
    cause: u8,
) -> ChainLog {
    lgt(
        STORAGE,
        b,
        i,
        tx,
        vec![tp(sig::SLASHED), node, tu(deal)],
        [
            word(u128::from(epoch)),
            word(u128::from(slot)),
            word(0),
            word(98),
            word(u128::from(epoch + 1)),
            word(u128::from(cause)),
        ]
        .concat(),
    )
}

pub(crate) fn miss_reward(
    b: u64,
    i: u64,
    tx: u64,
    node: Hash32,
    reporter: [u8; 20],
    amount: u128,
) -> ChainLog {
    lgt(
        STORAGE,
        b,
        i,
        tx,
        vec![tp(sig::MISS_REWARD), node, ta(&reporter)],
        word(amount).to_vec(),
    )
}

pub(crate) fn submitted(b: u64, i: u64, claim: u64, who: [u8; 20]) -> ChainLog {
    submitted_on(b, i, claim, h(0xfb), who, 5)
}

/// A claim on `fact` with `value` (kind Observed).
pub(crate) fn submitted_on(
    b: u64,
    i: u64,
    claim: u64,
    fact: Hash32,
    who: [u8; 20],
    value: u128,
) -> ChainLog {
    lg(
        STAKES,
        b,
        i,
        vec![tp(sig::SUBMITTED), tu(claim), fact, ta(&who)],
        vec![word(value), word(0)],
    )
}

pub(crate) fn decided(b: u64, i: u64, claim: u64, state: u8, by: u8) -> ChainLog {
    lg(
        STAKES,
        b,
        i,
        vec![tp(sig::DECIDED), tu(claim)],
        vec![
            word(u128::from(state)),
            word(u128::from(by)),
            word(1),
            word(0),
        ],
    )
}

pub(crate) fn opened(b: u64, i: u64, market: u64, opener: [u8; 20]) -> ChainLog {
    lg(
        CORR,
        b,
        i,
        vec![tp(sig::MARKET_OPENED), tu(market), h(0xfc), h(0xfd)],
        vec![aword(&opener), word(5), word(1), word(900), word(990)],
    )
}

pub(crate) fn bet(b: u64, i: u64, market: u64, who: [u8; 20], side: u8, amount: u128) -> ChainLog {
    lg(
        CORR,
        b,
        i,
        vec![tp(sig::BET), tu(market), ta(&who)],
        vec![word(u128::from(side)), word(amount)],
    )
}

pub(crate) fn resolved(b: u64, i: u64, market: u64, winner: u8) -> ChainLog {
    lg(
        CORR,
        b,
        i,
        vec![tp(sig::RESOLVED), tu(market)],
        vec![word(4), word(u128::from(winner)), word(1)],
    )
}

pub(crate) fn case_opened(b: u64, i: u64, case: Hash32, round: u64, shard: Hash32) -> ChainLog {
    lg(
        DISPUTES,
        b,
        i,
        vec![tp(sig::CASE_OPENED), case, tu(round), shard],
        vec![word(1)],
    )
}

pub(crate) fn case_closed(b: u64, i: u64, case: Hash32, status: u8) -> ChainLog {
    lg(
        DISPUTES,
        b,
        i,
        vec![tp(sig::CASE_CLOSED), case],
        vec![word(u128::from(status))],
    )
}

pub(crate) fn shard_voided(b: u64, i: u64, round: u64, shard: Hash32) -> ChainLog {
    lg(
        SETTLE,
        b,
        i,
        vec![tp(sig::SHARD_VOIDED), tu(round), shard],
        vec![],
    )
}

pub(crate) fn round_voided(b: u64, i: u64, round: u64) -> ChainLog {
    lg(SETTLE, b, i, vec![tp(sig::ROUND_VOIDED), tu(round)], vec![])
}

pub(crate) fn market_voided(b: u64, i: u64, market: u64) -> ChainLog {
    lg(
        CORR,
        b,
        i,
        vec![tp(sig::VOIDED), tu(market)],
        vec![word(1), word(1)],
    )
}

/// Apply `logs` over `from..=to` to a fresh state.
pub(crate) fn run_range(from: u64, to: u64, logs: &[ChainLog]) -> Result<Vec<Row>, EarlyError> {
    let c = contracts();
    State::new(&c).apply(&c, from, to, logs, &Views::default())
}

pub(crate) fn run(logs: &[ChainLog]) -> Vec<Row> {
    run_range(FROM, FROM + 1000, logs).expect("apply")
}

pub(crate) fn close_for(c: &Contracts, from: u64, to: u64) -> DayClose {
    DayClose {
        from_block: from,
        to_block: to,
        last_block_hash: h(0xbb),
        contracts_hash: c.hash,
        exclusions_hash: h(0xee),
        late: None,
    }
}

pub(crate) fn who(rows: &[Row], kind: Kind) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| r.who.clone())
        .collect();
    v.sort();
    v
}

/// One day with every kind, a revoke, and a few logs that give no row.
pub(crate) fn full_day() -> Vec<ChainLog> {
    let (shard, root) = (h(0x51), h(0x52));
    let (key, d1, d2) = (h(0x61), h(0x71), h(0x72));
    let node = h(0x81);
    vec![
        committed(100, 0, 7, shard, MINER_A),
        committed(100, 1, 7, shard, MINER_B),
        revealed(101, 0, 7, shard, MINER_B, root),
        revealed(101, 1, 7, shard, MINER_A, root),
        verdict(102, 0, 7, shard, VALIDATOR, 0),
        finalized(103, 0, 7, shard, root),
        released(104, 0, 7, shard),
        key_registered(105, 0, key, SIGNER, REVIEWER),
        report(106, 0, d1, key, 1),
        report(106, 1, d2, key, 1),
        equivocated(107, 0, 1070, d2, h(0x73)),
        withdrawn(107, 1, 1070, d2),
        activated(108, 0, node, OPERATOR),
        deal(108, 1, 1, PAYER),
        answered(109, 0, 1, 2, 0, node, true, 5),
        answered(109, 1, 1, 2, 1, node, false, 0),
        slashed(110, 0, 1100, node, 1, 2, 1, 3),
        miss_reward(110, 1, 1100, node, REPORTER, 9),
        submitted(111, 0, 4, SUBMITTER),
        decided(112, 0, 4, 3, 1),
        opened(113, 0, 1, OPENER),
        bet(114, 0, 1, BETTOR_NO, 2, 30),
        bet(114, 1, 1, BETTOR_YES, 1, 45),
        resolved(115, 0, 1, 2),
    ]
}

pub(crate) fn day_csv(logs: &[ChainLog]) -> String {
    let c = contracts();
    State::new(&c)
        .record_day(
            &c,
            logs,
            &Views::default(),
            DAY,
            &close_for(&c, FROM, FROM + 1000),
        )
        .expect("day")
        .csv
}

pub(crate) fn rights() -> DataRights {
    DataRights {
        license: LICENSE.into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("test.early-record".into()),
        attribution: None,
        terms: String::new(),
    }
}

/// Build `csv` into a bulk version after `prev`, as `onyx-publish build`
/// does: every record checked against the schema, shards cut by key.
pub(crate) fn build(csv: &str, prev: Option<&Manifest>, created: u64) -> Manifest {
    let schema = record_schema();
    let mut r = Records::new(csv.as_bytes(), schema.clone(), Format::Csv).expect("header");
    let mut b = ShardBuilder::new(1 << 20);
    let mut shards: Vec<Vec<Vec<u8>>> = Vec::new();
    while let Some((line, cells)) = r.next_record().expect("record") {
        let row = encode_row(&schema, &cells).unwrap_or_else(|e| panic!("line {line}: {e}"));
        assert!(row.len() < MAX_ROW_BYTES);
        let key = key_of(&schema, &cells).expect("key");
        if let Some(p) = prev {
            assert!(key >= p.header.key_max, "keys ascend across versions");
        }
        if let Some(full) = b.push(row, key).expect("sorted by key") {
            shards.push(full);
        }
    }
    if let Some(last) = b.finish() {
        shards.push(last);
    }
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    for s in &shards {
        let file = encode_shard(s);
        let c = check_shard(&schema, &file).expect("shard");
        entries.push(ShardEntry::new(entries.len() as u32, &c.summary));
    }
    Manifest::build(
        NewVersion {
            dataset: DATASET.into(),
            publisher: SourceId("test.early-record".into()),
            created_unix: created,
            schema,
            rights: rights(),
        },
        prev,
        entries,
    )
    .expect("manifest")
}

// ------------------------------------------------------------------ item 1: each kind

#[test]
fn l1_root_and_verdict_need_finalized_and_released() {
    let (shard, root) = (h(0x51), h(0x52));
    let work = vec![
        committed(100, 0, 7, shard, MINER_A),
        committed(100, 1, 7, shard, MINER_B),
        revealed(101, 0, 7, shard, MINER_A, root),
        // A reveal of another root is never paid.
        revealed(101, 1, 7, shard, MINER_B, h(0x53)),
        verdict(102, 0, 7, shard, VALIDATOR, 0),
        // Only Agree (kind 0) counts.
        verdict(102, 1, 7, shard, VALIDATOR_2, 1),
    ];
    // Revealed, no Finalized, no Released: no row.
    assert!(run(&work).is_empty());
    // Finalized but not Released: no row yet; the state keeps the work.
    let c = contracts();
    let mut s = State::new(&c);
    let mut day1 = work.clone();
    day1.push(finalized(103, 0, 7, shard, root));
    assert!(s
        .apply(&c, FROM, 103, &day1, &Views::default())
        .expect("apply")
        .is_empty());
    let back = State::from_json(&s.to_json()).expect("state");
    assert_eq!(back, s, "the pending work survives a save");
    let mut s = back;
    // Released in a later range: the rows appear.
    let rows = s
        .apply(
            &c,
            104,
            110,
            &[released(104, 0, 7, shard)],
            &Views::default(),
        )
        .expect("apply");
    assert_eq!(who(&rows, Kind::L1Root), vec![addr_text(&MINER_A)]);
    assert_eq!(who(&rows, Kind::L1Verdict), vec![addr_text(&VALIDATOR)]);
    let r = rows
        .iter()
        .find(|r| r.kind == Kind::L1Root)
        .expect("root row");
    assert_eq!(r.subject, format!("7:{}", h32_text(&shard)));
    assert_eq!(r.contract, addr_text(&DISPUTES));
    assert_eq!((r.accept.block, r.work.block), (104, 101));
    assert_eq!(r.detail, "winner");
    // Released without its Finalized log stops the join.
    let mut bad = work.clone();
    bad.push(released(104, 0, 7, shard));
    assert!(matches!(
        run_range(FROM, 200, &bad),
        Err(EarlyError::Join(_))
    ));
}

#[test]
fn only_the_earliest_commit_is_the_winner() {
    let (shard, root) = (h(0x51), h(0x52));
    let rows = run(&[
        committed(100, 0, 7, shard, MINER_A),
        committed(100, 1, 7, shard, MINER_B),
        // B reveals first, but A committed first: A is paid (OnyxRound.earliest).
        revealed(101, 0, 7, shard, MINER_B, root),
        revealed(101, 1, 7, shard, MINER_A, root),
        finalized(103, 0, 7, shard, root),
        released(104, 0, 7, shard),
    ]);
    let d: BTreeMap<String, String> = rows
        .iter()
        .filter(|r| r.kind == Kind::L1Root)
        .map(|r| (r.who.clone(), r.detail.clone()))
        .collect();
    assert_eq!(d[&addr_text(&MINER_A)], "winner");
    assert_eq!(d[&addr_text(&MINER_B)], "also");
    // A reveal whose Committed log is missing stops the join.
    let e = run_range(
        FROM,
        200,
        &[
            committed(100, 0, 7, shard, MINER_A),
            revealed(101, 0, 7, shard, MINER_B, root),
            finalized(103, 0, 7, shard, root),
            released(104, 0, 7, shard),
        ],
    );
    assert!(matches!(e, Err(EarlyError::Join(_))));
}

#[test]
fn storage_proof_needs_a_passed_answer() {
    let node = h(0x81);
    let rows = run(&[
        activated(100, 0, node, OPERATOR),
        deal(100, 1, 1, PAYER),
        answered(101, 0, 1, 2, 0, node, true, 5),
        answered(101, 1, 1, 2, 1, node, false, 0),
    ]);
    assert_eq!(rows.len(), 1, "passed = false gives no row");
    let r = &rows[0];
    assert_eq!(r.kind, Kind::StorageProof);
    assert_eq!(r.who, addr_text(&OPERATOR));
    assert_eq!(r.subject, format!("1:2:0:{}", h32_text(&node)));
    assert_eq!(r.counterparty.as_deref(), Some(addr_text(&PAYER).as_str()));
    assert_eq!((r.amount, r.detail.as_str()), (Some(5), "other"));
    // An answer by a node whose Activated log is missing stops the join.
    let e = run_range(
        FROM,
        200,
        &[
            deal(100, 1, 1, PAYER),
            answered(101, 0, 1, 2, 0, node, true, 5),
        ],
    );
    assert!(matches!(e, Err(EarlyError::Join(_))));
}

#[test]
fn a_self_paid_deal_is_marked_self() {
    let node = h(0x81);
    let rows = run(&[
        activated(100, 0, node, OPERATOR),
        deal(100, 1, 1, OPERATOR),
        answered(101, 0, 1, 2, 0, node, true, 5),
    ]);
    assert_eq!(rows[0].detail, "self");
    assert_eq!(
        rows[0].counterparty.as_deref(),
        Some(addr_text(&OPERATOR).as_str())
    );
}

#[test]
fn a_miss_report_credits_the_reporter_with_the_slashed_slot() {
    let node = h(0x81);
    let base = vec![activated(100, 0, node, OPERATOR), deal(100, 1, 1, PAYER)];
    let mut ok = base.clone();
    ok.extend([
        slashed(110, 0, 1100, node, 1, 2, 1, 3),
        miss_reward(110, 1, 1100, node, REPORTER, 9),
    ]);
    let rows = run(&ok);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(
        (r.kind, r.who.as_str()),
        (Kind::StorageMissReport, addr_text(&REPORTER).as_str())
    );
    assert_eq!(r.subject, format!("1:2:1:{}", h32_text(&node)));
    assert_eq!((r.amount, r.detail.as_str()), (Some(9), "reward"));
    // A miss the node's own operator settled pays the treasury: no MissReward, no row.
    let mut own = base.clone();
    own.push(slashed(110, 0, 1100, node, 1, 2, 1, 3));
    assert!(run(&own).is_empty());
    // A MissReward without the Slashed log of its transaction stops the join.
    let mut bad = base;
    bad.extend([
        slashed(110, 0, 1100, node, 1, 2, 1, 3),
        miss_reward(111, 0, 1110, node, REPORTER, 9),
    ]);
    assert!(matches!(
        run_range(FROM, 200, &bad),
        Err(EarlyError::Join(_))
    ));
}

#[test]
fn signed_report_needs_an_accepted_outcome() {
    let key = h(0x61);
    let rows = run(&[
        key_registered(100, 0, key, SIGNER, REVIEWER),
        report(101, 0, h(0x71), key, 1),
        // Equivocation and FactDisputed: recorded, never accepted.
        report(101, 1, h(0x72), key, 2),
        report(101, 2, h(0x74), key, 3),
    ]);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind, rows[0].subject.as_str()),
        (Kind::SignedReport, h32_text(&h(0x71)).as_str())
    );
    assert_eq!(rows[0].who, addr_text(&SIGNER));
    // A report under a key whose KeyRegistered log is missing stops the join.
    let e = run_range(FROM, 200, &[report(101, 0, h(0x71), key, 1)]);
    assert!(matches!(e, Err(EarlyError::Join(_))));
}

#[test]
fn stake_correct_needs_a_correct_decision() {
    let rows = run(&[
        submitted(100, 0, 4, SUBMITTER),
        decided(101, 0, 4, 3, 1),
        submitted(102, 0, 5, SUBMITTER),
        // Wrong (State 4): no row.
        decided(103, 0, 5, 4, 1),
    ]);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(
        (r.kind, r.subject.as_str(), r.detail.as_str()),
        (Kind::StakeCorrect, "4", "by=Observation")
    );
    assert_eq!(
        (r.who.as_str(), r.work.block, r.accept.block),
        (addr_text(&SUBMITTER).as_str(), 100, 101)
    );
    assert!(matches!(
        run_range(FROM, 200, &[decided(101, 0, 9, 3, 1)]),
        Err(EarlyError::Join(_))
    ));
}

#[test]
fn correction_found_needs_a_no_bet_in_a_market_no_won() {
    let rows = run(&[
        opened(100, 0, 1, OPENER),
        bet(101, 0, 1, BETTOR_NO, 2, 30),
        // A Yes bet in a market No won gives no row.
        bet(101, 1, 1, BETTOR_YES, 1, 45),
        resolved(102, 0, 1, 2),
        opened(103, 0, 2, OPENER),
        // A No bet in a market Yes won gives no row.
        bet(104, 0, 2, BETTOR_NO, 2, 10),
        resolved(105, 0, 2, 1),
    ]);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(
        (r.kind, r.who.as_str()),
        (Kind::CorrectionFound, addr_text(&BETTOR_NO).as_str())
    );
    assert_eq!(
        (r.subject.as_str(), r.amount, r.detail.as_str()),
        ("1", Some(30), "no_won")
    );
    assert_eq!(r.counterparty.as_deref(), Some(addr_text(&OPENER).as_str()));
}

#[test]
fn a_withdrawn_report_gets_a_revoke_row() {
    let c = contracts();
    let key = h(0x61);
    let (d1, d2) = (h(0x71), h(0x72));
    let mut s = State::new(&c);
    // Day D: the accepted report is the version's first row (KeyRegistered gives none).
    let v1 = s
        .record_day(
            &c,
            &[
                key_registered(100, 0, key, SIGNER, REVIEWER),
                report(101, 0, d1, key, 1),
            ],
            &Views::default(),
            DAY,
            &close_for(&c, FROM, FROM + 86_399),
        )
        .expect("day 1");
    let k1 = i128::from(DAY) << 24;
    assert_eq!(v1.keys, vec![(Kind::SignedReport, h32_text(&d1), k1)]);
    // Day D + 1: it is withdrawn; another report is accepted, then
    // equivocated and withdrawn in one transaction: one revoke row each.
    let b = FROM + 86_400;
    let rows = s
        .apply(
            &c,
            b,
            b + 100,
            &[
                withdrawn(b, 0, 1, d1),
                report(b + 1, 0, d2, key, 1),
                equivocated(b + 2, 0, 9, d2, h(0x73)),
                withdrawn(b + 2, 1, 9, d2),
                // A second withdrawal of the same report changes nothing.
                withdrawn(b + 3, 0, 10, d1),
            ],
            &Views::default(),
        )
        .expect("day 2");
    let revokes: Vec<(&str, &str, Option<&Revokes>)> = rows
        .iter()
        .filter(|r| r.kind == Kind::Revoke)
        .map(|r| (r.subject.as_str(), r.detail.as_str(), r.revokes.as_ref()))
        .collect();
    let (s1, s2) = (h32_text(&d1), h32_text(&d2));
    assert_eq!(
        revokes,
        vec![
            (s1.as_str(), "withdrawn", Some(&Revokes::Key(k1))),
            (
                s2.as_str(),
                "equivocated",
                Some(&Revokes::SameVersion(Kind::SignedReport, s2.clone()))
            ),
        ]
    );
    // The same-version revoke names the key of its report's row.
    let v2 = write_day(DAY + 1, &rows, &close_for(&c, b, b + 100)).expect("write");
    let k2 = (i128::from(DAY + 1) << 24) + 1;
    assert_eq!(v2.keys, vec![(Kind::SignedReport, s2.clone(), k2)]);
    let line = v2
        .csv
        .lines()
        .find(|l| l.contains(",revoke,") && l.contains(&s2))
        .expect("revoke line");
    assert!(line.contains(&format!(",{k2},1")), "{line}");
    assert!(
        line.contains(&addr_text(&SIGNER)),
        "the revoke names the report's signer"
    );
}

#[test]
fn a_compromised_or_retired_key_revokes_from_its_time() {
    let c = contracts();
    let (k, k2) = (h(0x61), h(0x62));
    let since = T0 + 1000;
    let logs = vec![
        key_registered(100, 0, k, SIGNER, REVIEWER),
        key_registered(100, 1, k2, SIGNER_2, REVIEWER),
        // Key k: recorded at since - 900, since - 100 (inside the clock skew), since.
        report(FROM + 100, 0, h(0x71), k, 1),
        report(FROM + 900, 0, h(0x72), k, 1),
        report(FROM + 1000, 0, h(0x73), k, 1),
        // Key k2: recorded at since - 900, since - 100, since - 50.
        report(FROM + 100, 1, h(0x74), k2, 1),
        report(FROM + 900, 1, h(0x75), k2, 1),
        report(FROM + 950, 0, h(0x76), k2, 1),
        key_event(sig::KEY_COMPROMISED, FROM + 1100, 0, k, since),
        key_event(sig::KEY_RETIRED, FROM + 1100, 1, k2, since),
    ];
    let revoked = |views: &Views| -> Vec<(String, String)> {
        let rows = State::new(&c)
            .apply(&c, FROM, FROM + 2000, &logs, views)
            .expect("apply");
        let mut v: Vec<(String, String)> = rows
            .iter()
            .filter(|r| r.kind == Kind::Revoke)
            .map(|r| (r.subject.clone(), r.detail.clone()))
            .collect();
        v.sort();
        v
    };
    let t = |n: u8| h32_text(&h(n));
    // Without observed times: a report recorded up to 300 s before the time
    // may be observed after it, so it is revoked too.
    assert_eq!(
        revoked(&Views::default()),
        vec![
            (t(0x72), "compromised".into()),
            (t(0x73), "compromised".into()),
            (t(0x75), "retired".into()),
            (t(0x76), "retired".into()),
        ]
    );
    // With observed times (OnyxSignedBook.report) the rule is exact:
    // compromise from `since`, observed or accepted (R-SIG-6a); retirement
    // from `at`, observed (L3R5-F1).
    let mut views = Views::default();
    views.observed.insert(t(0x71), T0 + 50);
    views.observed.insert(t(0x72), since + 5);
    views.observed.insert(t(0x73), since - 10);
    views.observed.insert(t(0x74), T0 + 50);
    views.observed.insert(t(0x75), since - 120);
    views.observed.insert(t(0x76), since);
    assert_eq!(
        revoked(&views),
        vec![
            (t(0x72), "compromised".into()),
            (t(0x73), "compromised".into()),
            (t(0x76), "retired".into()),
        ]
    );
}

#[test]
fn a_second_finalized_marks_the_shard_cased_and_the_run_goes_on() {
    // OnyxDisputes.challenge then _finish -> dEnd emits a second Finalized
    // (OnyxSettlementExt.sol:341). Here its CaseOpened log is not read: the
    // second Finalized alone marks the shard cased. No row, no error.
    let (x, y, root) = (h(0x51), h(0x54), h(0x52));
    let c = contracts();
    let mut s = State::new(&c);
    let rows = s
        .apply(
            &c,
            FROM,
            200,
            &[
                committed(100, 0, 7, x, MINER_A),
                committed(100, 1, 7, y, MINER_B),
                revealed(101, 0, 7, x, MINER_A, root),
                revealed(101, 1, 7, y, MINER_B, root),
                finalized(103, 0, 7, x, root),
                finalized(103, 1, 7, y, root),
                finalized(110, 0, 7, x, h(0x53)),
                released(111, 0, 7, y),
            ],
            &Views::default(),
        )
        .expect("no error");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind, rows[0].who.clone()),
        (Kind::L1Root, addr_text(&MINER_B))
    );
    assert!(s.shards.is_empty(), "the cased shard left the cache");
    // It can never be released: a Released for it is a missing-log error.
    let mut t = s.clone();
    assert!(t
        .apply(&c, 201, 210, &[released(201, 0, 7, x)], &Views::default())
        .is_err());
}

#[test]
fn every_case_path_gives_no_row_and_leaves_the_cache() {
    let root = h(0x52);
    let sh = |n: u8| h(0x90 + n);
    let ca = |n: u8| h(0xa0 + n);
    let mut logs = Vec::new();
    for n in 0..7u8 {
        let b = 100 + u64::from(n) * 10;
        let round = if n == 6 { 8 } else { 7 };
        logs.push(committed(b, 0, round, sh(n), MINER_A));
        logs.push(revealed(b + 1, 0, round, sh(n), MINER_A, root));
        logs.push(verdict(b + 2, 0, round, sh(n), VALIDATOR, 0));
    }
    let b = 200;
    logs.extend([
        // Challenge upheld: the case ends Finalized with the same root.
        finalized(b, 0, 7, sh(0), root),
        case_opened(b + 1, 0, ca(0), 7, sh(0)),
        finalized(b + 2, 0, 7, sh(0), root),
        case_closed(b + 2, 1, ca(0), 3),
        // Challenge overturned: Finalized with another root.
        finalized(b + 3, 0, 7, sh(1), root),
        case_opened(b + 4, 0, ca(1), 7, sh(1)),
        finalized(b + 5, 0, 7, sh(1), h(0x53)),
        case_closed(b + 5, 1, ca(1), 3),
        // Challenge ending NoValidRoot: no second Finalized.
        finalized(b + 6, 0, 7, sh(2), root),
        case_opened(b + 7, 0, ca(2), 7, sh(2)),
        case_closed(b + 8, 0, ca(2), 4),
        // A Pending shard rerun by a case: its only Finalized is the case's end.
        case_opened(b + 9, 0, ca(3), 7, sh(3)),
        finalized(b + 10, 0, 7, sh(3), root),
        case_closed(b + 10, 1, ca(3), 3),
        // A rerun that expired: Unresolved.
        case_opened(b + 11, 0, ca(4), 7, sh(4)),
        case_closed(b + 12, 0, ca(4), 6),
        // A voided shard and a voided round.
        shard_voided(b + 13, 0, 7, sh(5)),
        round_voided(b + 14, 0, 8),
    ]);
    let c = contracts();
    let mut s = State::new(&c);
    let rows = s
        .apply(&c, FROM, 300, &logs, &Views::default())
        .expect("no error");
    assert_eq!(
        rows,
        vec![],
        "a shard with a case or a void is never released"
    );
    assert!(
        s.shards.is_empty() && s.cases.is_empty(),
        "nothing left in the cache"
    );
    // A case closed without its CaseOpened log stops the join.
    assert!(matches!(
        run_range(FROM, 300, &[case_closed(150, 0, ca(9), 3)]),
        Err(EarlyError::Join(_))
    ));
}

#[test]
fn a_later_correct_claim_with_another_value_supersedes_the_earlier_one() {
    let c = contracts();
    let (f, g, k) = (h(0xd1), h(0xd2), h(0xd3));
    let mut s = State::new(&c);
    // Day D: fact f decided Correct by Vote (value 5); fact k by Vote (5).
    let d = s
        .record_day(
            &c,
            &[
                submitted_on(100, 0, 4, f, SUBMITTER, 5),
                decided(101, 0, 4, 3, 2),
                submitted_on(102, 0, 8, k, SUBMITTER, 5),
                decided(103, 0, 8, 3, 2),
            ],
            &Views::default(),
            DAY,
            &close_for(&c, FROM, FROM + 86_399),
        )
        .expect("day 1");
    let base = i128::from(DAY) << 24;
    assert_eq!(
        d.keys,
        vec![
            (Kind::StakeCorrect, "4".into(), base),
            (Kind::StakeCorrect, "8".into(), base + 1)
        ]
    );
    // Day D + 1: an Observed claim on f with another value (6) is decided
    // Correct: the day-D row is superseded. On g, both claims fall on the
    // same day. On k the observed value is the same (5): no revoke.
    let b = FROM + 86_400;
    let rows = s
        .apply(
            &c,
            b,
            b + 100,
            &[
                submitted_on(b, 0, 5, f, BETTOR_NO, 6),
                decided(b + 1, 0, 5, 3, 1),
                submitted_on(b + 2, 0, 6, g, SUBMITTER, 5),
                decided(b + 3, 0, 6, 3, 2),
                submitted_on(b + 4, 0, 7, g, BETTOR_NO, 9),
                decided(b + 5, 0, 7, 3, 1),
                submitted_on(b + 6, 0, 9, k, BETTOR_NO, 5),
                decided(b + 7, 0, 9, 3, 1),
            ],
            &Views::default(),
        )
        .expect("day 2");
    let revokes: Vec<(&str, &str, &str, Option<&Revokes>)> = rows
        .iter()
        .filter(|r| r.kind == Kind::Revoke)
        .map(|r| {
            (
                r.subject.as_str(),
                r.who.as_str(),
                r.detail.as_str(),
                r.revokes.as_ref(),
            )
        })
        .collect();
    let sub = addr_text(&SUBMITTER);
    assert_eq!(
        revokes,
        vec![
            ("4", sub.as_str(), "superseded", Some(&Revokes::Key(base))),
            (
                "6",
                sub.as_str(),
                "superseded",
                Some(&Revokes::SameVersion(Kind::StakeCorrect, "6".into()))
            ),
        ]
    );
    let v = write_day(DAY + 1, &rows, &close_for(&c, b, b + 100)).expect("write");
    let k6 = v.keys.iter().find(|(_, s, _)| s == "6").expect("claim 6").2;
    assert!(v
        .csv
        .lines()
        .any(|l| l.contains(",revoke,") && l.ends_with(&format!(",superseded,chain,{k6},1"))));
    // Same value, observed: no revoke for claim 8.
    assert!(!rows
        .iter()
        .any(|r| r.kind == Kind::Revoke && r.subject == "8"));
    // A Correct claim on a fact final since an observation is impossible on
    // chain (AlreadyFinal): the join refuses it.
    let mut t = s.clone();
    t.record_keys(&v.keys).expect("keys");
    assert!(t
        .apply(
            &c,
            b + 101,
            b + 200,
            &[
                submitted_on(b + 101, 0, 10, f, SUBMITTER, 7),
                decided(b + 102, 0, 10, 3, 2)
            ],
            &Views::default()
        )
        .is_err());
}

#[test]
fn a_second_compromise_moves_since_earlier() {
    let c = contracts();
    let k = h(0x61);
    let t = |n: u8| h32_text(&h(n));
    let mut views = Views::default();
    for (n, at) in [(0x71u8, 100u64), (0x72, 500), (0x73, 1000)] {
        views.observed.insert(t(n), T0 + at);
    }
    let rows = State::new(&c)
        .apply(
            &c,
            FROM,
            FROM + 2000,
            &[
                key_registered(100, 0, k, SIGNER, REVIEWER),
                report(FROM + 100, 0, h(0x71), k, 1),
                report(FROM + 500, 0, h(0x72), k, 1),
                report(FROM + 1000, 0, h(0x73), k, 1),
                key_event(sig::KEY_COMPROMISED, FROM + 1100, 0, k, T0 + 1000),
                // The reviewer moves `since` earlier (markCompromised).
                key_event(sig::KEY_COMPROMISED, FROM + 1200, 0, k, T0 + 400),
            ],
            &views,
        )
        .expect("apply");
    let revoked: Vec<(String, u64)> = rows
        .iter()
        .filter(|r| r.kind == Kind::Revoke)
        .map(|r| (r.subject.clone(), r.accept.block))
        .collect();
    assert_eq!(
        revoked,
        vec![(t(0x73), FROM + 1100), (t(0x72), FROM + 1200)]
    );
}

#[test]
fn a_prune_keeps_output_identical() {
    let c = contracts();
    let key = h(0x61);
    let (d1, d2, f) = (h(0x71), h(0x72), h(0xd1));
    let day = |n: u64| FROM + n * 86_400;
    let days: Vec<Vec<ChainLog>> = vec![
        vec![
            key_registered(day(0), 0, key, SIGNER, REVIEWER),
            report(day(0) + 1, 0, d1, key, 1),
            report(day(0) + 2, 0, d2, key, 1),
            submitted_on(day(0) + 3, 0, 4, f, SUBMITTER, 5),
            decided(day(0) + 4, 0, 4, 3, 2),
            opened(day(0) + 5, 0, 1, OPENER),
            bet(day(0) + 6, 0, 1, BETTOR_NO, 2, 30),
        ],
        vec![
            withdrawn(day(1), 0, 1, d1),
            submitted_on(day(1) + 1, 0, 5, f, BETTOR_NO, 6),
            decided(day(1) + 2, 0, 5, 3, 1),
            market_voided(day(1) + 3, 0, 1),
            key_event(sig::KEY_COMPROMISED, day(1) + 4, 0, key, T0),
        ],
        vec![withdrawn(day(2), 0, 2, d1), withdrawn(day(2) + 1, 0, 3, d2)],
    ];
    let mut pruned = State::new(&c);
    let mut kept = State::new(&c);
    for (i, logs) in days.iter().enumerate() {
        let close = close_for(&c, day(i as u64), day(i as u64) + 86_399);
        let rd = DAY + i as u32;
        let a = pruned
            .record_day(&c, logs, &Views::default(), rd, &close)
            .expect("pruned");
        let rows = kept
            .apply(
                &c,
                close.from_block,
                close.to_block,
                logs,
                &Views::default(),
            )
            .expect("kept");
        let b = write_day(rd, &rows, &close).expect("write");
        kept.record_keys(&b.keys).expect("keys");
        assert_eq!(a.csv, b.csv, "day {i}");
        if i == 1 {
            // Pruned: revoked reports and the fact final since an observation.
            assert!(pruned.reports.is_empty() && pruned.facts.is_empty());
            assert_eq!(kept.reports.len(), 2);
            assert_eq!(kept.facts.len(), 1);
            assert!(pruned.markets.is_empty() && kept.markets.is_empty());
            assert!(pruned.to_json().len() < kept.to_json().len());
        }
    }
    let back = State::from_json(&pruned.to_json()).expect("state");
    assert_eq!(back, pruned);
}

// ------------------------------------------------------------------ item 2: relayed work

#[test]
fn a_relayed_commit_credits_the_signer() {
    // The miner signed the commit; a relayer sent every transaction, and the
    // reader knows it (`from`). The row names the miner the contract checked,
    // never the sender.
    let (shard, root) = (h(0x51), h(0x52));
    let relayed = |l: ChainLog| ChainLog {
        from: Some(RELAYER),
        ..l
    };
    let logs: Vec<ChainLog> = vec![
        relayed(committed(100, 0, 7, shard, MINER_A)),
        relayed(revealed(101, 0, 7, shard, MINER_A, root)),
        relayed(verdict(102, 0, 7, shard, VALIDATOR, 0)),
        relayed(finalized(103, 0, 7, shard, root)),
        relayed(released(104, 0, 7, shard)),
    ];
    let rows = run(&logs);
    assert_eq!(rows.len(), 2);
    assert_eq!(who(&rows, Kind::L1Root), vec![addr_text(&MINER_A)]);
    assert_eq!(who(&rows, Kind::L1Verdict), vec![addr_text(&VALIDATOR)]);
    let csv = day_csv(&logs);
    assert!(
        !csv.contains(&hex::encode(RELAYER)),
        "the sender is never credited"
    );
    // The same logs without a known sender give the same bytes.
    let plain: Vec<ChainLog> = logs
        .iter()
        .map(|l| ChainLog {
            from: None,
            ..l.clone()
        })
        .collect();
    assert_eq!(day_csv(&plain), csv);
}

#[test]
fn a_relayed_report_credits_the_key_signer() {
    // The reviewer registered the key (its address is in the event data); a
    // relayer posted the signed report. The row names the key's signer.
    let key = h(0x61);
    let logs = vec![
        key_registered(100, 0, key, SIGNER, REVIEWER),
        report(101, 0, h(0x71), key, 1),
    ];
    let rows = run(&logs);
    assert_eq!(who(&rows, Kind::SignedReport), vec![addr_text(&SIGNER)]);
    let csv = day_csv(&logs);
    assert!(!csv.contains(&hex::encode(REVIEWER)) && !csv.contains(&hex::encode(RELAYER)));
}

// ------------------------------------------------------------------ item 3: order and repeats

#[test]
fn shuffled_and_duplicated_logs_give_the_same_csv_and_root() {
    let logs = full_day();
    let a = day_csv(&logs);
    let mut shuffled: Vec<ChainLog> = logs.iter().rev().cloned().collect();
    shuffled.extend(logs.iter().step_by(2).cloned());
    shuffled.rotate_left(7);
    let b = day_csv(&shuffled);
    assert_eq!(a, b);
    assert_eq!(
        build(&a, None, T0).header.root(),
        build(&b, None, T0).header.root()
    );
    // Every kind is in it, sorted by accept block and log, the day_close last.
    let kinds: Vec<&str> = a
        .lines()
        .skip(1)
        .map(|l| l.split(',').nth(2).unwrap_or(""))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "l1_root",
            "l1_root",
            "l1_verdict",
            "signed_report",
            "signed_report",
            "revoke",
            "storage_proof",
            "storage_miss_report",
            "stake_correct",
            "correction_found",
            "day_close"
        ]
    );
    // Two different logs at one position stop the join.
    let mut two = logs.clone();
    two.push(bet(114, 0, 1, BETTOR_YES, 2, 1));
    assert!(matches!(
        run_range(FROM, FROM + 1000, &two),
        Err(EarlyError::Log(_))
    ));
}

// ------------------------------------------------------------------ item 4: the UTC day

#[test]
fn a_block_at_midnight_belongs_to_the_new_day() {
    assert_eq!(utc_day(T0), DAY);
    assert_eq!(utc_day(T0 - 1), DAY - 1);
    assert_eq!(day_text(DAY), "2026-11-14");
    assert_eq!(day_text(DAY - 1), "2026-11-13");
    assert_eq!(day_text(0), "1970-01-01");
    assert_eq!(day_text(11_016), "2000-02-29");
    assert_eq!(day_text(19_783), "2024-03-01");
    let c = contracts();
    let key = h(0x61);
    // Block FROM is at 00:00:00 of DAY.
    let rows = run(&[
        key_registered(FROM, 0, key, SIGNER, REVIEWER),
        report(FROM, 1, h(0x71), key, 1),
    ]);
    assert_eq!(rows[0].accept.time, T0);
    let v = write_day(DAY, &rows, &close_for(&c, FROM, FROM + 10)).expect("new day");
    assert!(v
        .csv
        .lines()
        .nth(1)
        .expect("row")
        .contains(",2026-11-14,signed_report,"));
    assert!(matches!(
        write_day(DAY - 1, &rows, &close_for(&c, FROM, FROM + 10)),
        Err(EarlyError::Write(_))
    ));
    // One second earlier is the day before.
    let mut late = rows[0].clone();
    late.accept.time = T0 - 1;
    assert!(matches!(
        write_day(DAY, &[late.clone()], &close_for(&c, FROM, FROM + 10)),
        Err(EarlyError::Write(_))
    ));
    assert!(write_day(DAY - 1, &[late], &close_for(&c, FROM, FROM + 10)).is_ok());
}

// ------------------------------------------------------------------ item 7: successors

#[test]
fn version_d_plus_1_is_a_successor_and_a_changed_row_fails() {
    let c = contracts();
    let mut s = State::new(&c);
    let logs = full_day();
    let d = s
        .record_day(
            &c,
            &logs,
            &Views::default(),
            DAY,
            &close_for(&c, FROM, FROM + 86_399),
        )
        .expect("day 1");
    let b = FROM + 86_400;
    let next = vec![submitted(b, 0, 6, SUBMITTER), decided(b + 1, 0, 6, 3, 2)];
    let d1 = s
        .record_day(
            &c,
            &next,
            &Views::default(),
            DAY + 1,
            &close_for(&c, b, b + 86_399),
        )
        .expect("day 2");
    assert!(d1.csv.contains(",by=Vote,"));
    let m0 = build(&d.csv, None, T0);
    let m1 = build(&d1.csv, Some(&m0), T0 + 86_400);
    m1.check_successor(&m0).expect("D + 1 follows D");
    // A changed earlier row gives another D, which D + 1 does not follow.
    let changed = d.csv.replacen(",winner,", ",also,", 1);
    assert_ne!(changed, d.csv);
    let forged = build(&changed, None, T0);
    assert!(m1.check_successor(&forged).is_err());
}

// ------------------------------------------------------------------ item 8: exclusions

#[test]
fn role_and_fixture_addresses_are_excluded() {
    let c = contracts();
    let calls = role_calls(&c, 0x1234);
    let g: Vec<(&str, &str)> = calls
        .iter()
        .map(|r| (r.contract.as_str(), r.getter))
        .collect();
    assert!(g.contains(&("OnyxSettlement", "coordinator()")));
    assert!(g.contains(&("OnyxRegistry", "receiptSigner()")));
    assert!(g.contains(&("OnyxSignedBook", "reviewer()")));
    assert!(g.contains(&("OnyxStorage", "treasury()")));
    assert!(
        !g.iter().any(|(n, _)| *n == "OnyxDisputes"),
        "disputes has no role getter"
    );
    // What slice 2's eth_calls would answer: one address per call, one unset.
    let answer = |i: usize| format!("0x{}", hex::encode(aword(&[0x40 + i as u8; 20])));
    let mut roles: Vec<Exclusion> = Vec::new();
    for (i, r) in calls.iter().enumerate() {
        let a = if r.getter == "receiptSigner()" {
            format!("0x{}", "0".repeat(64))
        } else {
            answer(i)
        };
        roles.extend(r.exclusion(&a).expect("answer"));
    }
    assert_eq!(roles.len(), calls.len() - 1, "an unset role adds nothing");
    let mut wanted = fixed_exclusions();
    wanted.extend(roles.clone());
    let none = BTreeSet::new();
    let day1 = exclusion_changes(&BTreeMap::new(), &wanted, &none, DAY)
        .expect("day 1")
        .rows;
    assert!(day1.iter().all(|r| r.action == Action::Add));
    let force1 = exclusions_in_force(&day1).expect("replay");
    for a in ANVIL_ACCOUNTS
        .iter()
        .chain(PLACEHOLDERS.iter().map(|(a, _)| a))
    {
        assert_eq!(force1[*a].reason, Reason::Fixture, "{a}");
    }
    let coord = roles
        .iter()
        .find(|x| x.source == "OnyxSettlement.coordinator()")
        .expect("coordinator");
    assert_eq!(force1[&coord.address].reason, Reason::Role);
    // Nothing changed: no rows. A role that moves: one remove, one add.
    assert!(exclusion_changes(&force1, &wanted, &none, DAY + 1)
        .expect("same")
        .rows
        .is_empty());
    let moved = Exclusion {
        address: addr_text(&[0x7f; 20]),
        ..coord.clone()
    };
    let wanted2: Vec<Exclusion> = wanted
        .iter()
        .filter(|x| x.address != coord.address)
        .cloned()
        .chain([moved.clone()])
        .collect();
    let day2 = exclusion_changes(&force1, &wanted2, &none, DAY + 1)
        .expect("day 2")
        .rows;
    let actions: Vec<(Action, &str)> = day2
        .iter()
        .map(|r| (r.action, r.exclusion.address.as_str()))
        .collect();
    assert_eq!(
        actions,
        vec![
            (Action::Remove, coord.address.as_str()),
            (Action::Add, moved.address.as_str())
        ]
    );
    assert_eq!(day2[0].key, i128::from(DAY + 1) << 24);
    let mut all = day1.clone();
    all.extend(day2.clone());
    let force2 = exclusions_in_force(&all).expect("replay");
    assert!(!force2.contains_key(&coord.address) && force2.contains_key(&moved.address));
    assert_ne!(exclusions_hash(&force1), exclusions_hash(&force2));
    // Append-only: the first version's rows are a prefix of the history.
    assert_eq!(&all[..day1.len()], &day1[..]);
    let csv = exclusions_csv(&all).expect("csv");
    assert!(csv.starts_with("key,day,address,action,reason,source\n"));
    assert!(csv.contains(&format!(
        ",{},remove,role,OnyxSettlement.coordinator()\n",
        coord.address
    )));
    // Replays that break the rules are refused.
    let mut twice = day1.clone();
    twice.push(ExclusionRow {
        key: i128::from(DAY + 1) << 24,
        ..day1[0].clone()
    });
    assert!(exclusions_in_force(&twice).is_err());
    // A role address that becomes a publisher key (a signer of the book):
    // skipped, never refused, and removed when it was in force; the next
    // run is quiet again.
    let publishers: BTreeSet<String> = [moved.address.clone()].into();
    let later = exclusion_changes(&force2, &wanted2, &publishers, DAY + 2).expect("no error");
    assert_eq!(later.skipped, vec![moved.clone()]);
    let acts: Vec<(Action, &str)> = later
        .rows
        .iter()
        .map(|r| (r.action, r.exclusion.address.as_str()))
        .collect();
    assert_eq!(acts, vec![(Action::Remove, moved.address.as_str())]);
    all.extend(later.rows.clone());
    let force3 = exclusions_in_force(&all).expect("replay");
    let again = exclusion_changes(&force3, &wanted2, &publishers, DAY + 3).expect("no error");
    assert!(again.rows.is_empty() && again.skipped == vec![moved]);
}

#[test]
fn the_join_knows_the_publisher_signers() {
    let c = contracts();
    let mut s = State::new(&c);
    s.apply(
        &c,
        FROM,
        200,
        &[key_registered(100, 0, h(0x61), SIGNER, REVIEWER)],
        &Views::default(),
    )
    .expect("apply");
    assert_eq!(s.publisher_signers(), BTreeSet::from([addr_text(&SIGNER)]));
}

#[test]
fn fixture_sources_name_lines_that_hold_the_address() {
    for (a, src) in PLACEHOLDERS {
        let (file, line) = src.split_once(':').expect("file:line");
        let n: usize = line.parse().expect("line");
        let text = read(file);
        let l = text
            .lines()
            .nth(n - 1)
            .unwrap_or_else(|| panic!("{src}: no such line"));
        assert!(
            l.to_ascii_lowercase().contains(a),
            "{src} does not hold {a}: {l}"
        );
    }
    // recheck-anvil.sh names anvil accounts 0 to 2 by their checksummed form.
    let s = read("scripts/recheck-anvil.sh").to_ascii_lowercase();
    for a in &ANVIL_ACCOUNTS[..3] {
        assert!(s.contains(a), "{a}");
    }
}

#[test]
fn anvil_account_0_is_the_public_dev_key() {
    // Anvil's published account-0 key (allowlisted in scripts/check-secrets.sh).
    let k: [u8; 32] =
        hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
            .expect("hex")
            .try_into()
            .expect("32 bytes");
    let a = onyx_bulk::anchor::address_of(&k).expect("address");
    assert_eq!(addr_text(&a), ANVIL_ACCOUNTS[0]);
}

#[test]
fn the_fixed_list_names_no_live_address() {
    let doc = read("docs/ops/deployment.md").to_ascii_lowercase();
    let book = read("docs/STORAGE-NETWORK.md").to_ascii_lowercase();
    for x in fixed_exclusions() {
        assert!(
            !doc.contains(&x.address) && !book.contains(&x.address),
            "{}",
            x.address
        );
        assert!(parse_address(&x.address).is_some());
    }
}

// ------------------------------------------------------------------ item 9: contracts.json

/// `| label | `0x...` |` rows of the "Contracts on Base Sepolia" section.
pub(crate) fn deployment_rows() -> BTreeMap<String, String> {
    let doc = read("docs/ops/deployment.md");
    let sec = doc
        .split("## Contracts on Base Sepolia")
        .nth(1)
        .expect("section")
        .split("\n## ")
        .next()
        .expect("section end");
    let mut m = BTreeMap::new();
    for l in sec.lines() {
        let Some(rest) = l.strip_prefix("| ") else {
            continue;
        };
        let Some((label, tail)) = rest.rsplit_once(" | `0x") else {
            continue;
        };
        let Some(a) = tail.strip_suffix("` |") else {
            continue;
        };
        m.insert(label.to_string(), format!("0x{}", a.to_ascii_lowercase()));
    }
    m
}

/// One line per paragraph of `text`, whitespace folded.
pub(crate) fn folded(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The paragraph right above the table that holds the row `label` in
/// deployment.md (the text that introduces that table), folded.
pub(crate) fn intro_of_row(doc: &str, label: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with(&format!("| {label} | `")))
        .unwrap_or_else(|| panic!("row {label:?}"));
    let mut i = at;
    while i > 0 && lines[i - 1].starts_with('|') {
        i -= 1;
    }
    while i > 0 && lines[i - 1].trim().is_empty() {
        i -= 1;
    }
    let end = i;
    while i > 0 && !lines[i - 1].trim().is_empty() && !lines[i - 1].starts_with('|') {
        i -= 1;
    }
    folded(&lines[i..end].join(" "))
}

#[test]
fn contracts_json_matches_the_deployment_tables() {
    let real = Contracts::load(&repo().join("deploy/early-record/contracts.json"))
        .expect("contracts.json");
    assert_eq!(
        (real.file.chain_id, real.file.network.as_str()),
        (84_532, "Base Sepolia")
    );
    let rows = deployment_rows();
    for e in &real.file.contracts {
        let doc = read(&e.doc);
        match &e.doc_row {
            Some(label) => {
                assert_eq!(e.doc, "docs/ops/deployment.md");
                assert_eq!(
                    rows.get(label),
                    Some(&e.address),
                    "{}: row {label:?}",
                    e.name
                );
            }
            // A second deployment of one contract has a name of its own in
            // the list (`OnyxStorage (v4)`); the document names the contract.
            None => assert!(
                doc.contains(&format!(
                    "`{}` `{}`",
                    e.name.split(" (").next().unwrap_or_default(),
                    e.address
                )),
                "{} in {}",
                e.name,
                e.doc
            ),
        }
        // A block is tied to this contract's own documentation: a quote that
        // names its address (or deploy transaction), or the paragraph that
        // introduces the table holding its row; or the quoted block of the
        // contract named in bound_by.
        if let (Some(q), Some(d)) = (&e.block_quote, &e.block_doc) {
            let q = folded(q);
            let b = e.scan_from.expect("scan_from").to_string();
            assert!(q.contains(&b), "{}: the quote holds {b}", e.name);
            let text = folded(&read(d));
            assert!(text.contains(&q), "{}: quote not in {d}", e.name);
            let names_it =
                q.contains(&e.address) || e.deploy_tx.as_deref().is_some_and(|t| q.contains(t));
            let intro = match &e.doc_row {
                Some(label) if d == &e.doc => intro_of_row(&read(d), label).contains(&q),
                _ => false,
            };
            assert!(
                names_it || intro,
                "{}: the quote is not tied to its own row",
                e.name
            );
            if e.scan_from_is == Some(ScanFrom::Deploy) {
                assert!(
                    names_it,
                    "{}: a deploy block names the address or the deploy tx",
                    e.name
                );
            }
        }
        if let Some(by) = &e.bound_by {
            let o = real
                .file
                .contracts
                .iter()
                .find(|o| &o.name == by)
                .expect("bound_by");
            assert!(
                o.block_quote.is_some() && o.scan_from == e.scan_from,
                "{}",
                e.name
            );
            assert_eq!(e.scan_from_is, Some(ScanFrom::Bound), "{}", e.name);
        }
        if e.abi.counted() {
            assert!(e.why.is_some(), "{}: why its block holds", e.name);
        }
        if let Some(t) = &e.deploy_tx {
            assert!(
                folded(&doc).contains(t.as_str()),
                "{}: deploy tx in {}",
                e.name,
                e.doc
            );
        }
    }
    // The deploy kind only where the documents state a deploy block.
    let deploy: BTreeSet<&str> = real
        .file
        .contracts
        .iter()
        .filter(|e| e.scan_from_is == Some(ScanFrom::Deploy))
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        deploy,
        BTreeSet::from(["OnyxCorrections", "OnyxStorage", "OnyxStorage (v4)"])
    );
    // Every Onyx contract the tables list is in contracts.json (EARLY-3).
    let listed: BTreeSet<&str> = real
        .file
        .contracts
        .iter()
        .map(|e| e.address.as_str())
        .collect();
    for (label, a) in &rows {
        if label.starts_with("Onyx") || label.starts_with("tONYX") {
            assert!(
                listed.contains(a.as_str()),
                "{label} ({a}) is not in contracts.json"
            );
        }
    }
    let storage = read("docs/STORAGE-NETWORK.md");
    let live = storage
        .split("| Base Sepolia | `OnyxStorage` `")
        .nth(1)
        .and_then(|s| s.split('`').next())
        .expect("the live OnyxStorage row");
    assert!(
        listed.contains(live),
        "the live OnyxStorage is in contracts.json"
    );
    assert_eq!(real.first_block(), Some(50_736_980));
}

/// The addresses of the data publishers deployment.md lists (rows "Bulk
/// publisher ..."; the operator's demo publisher row is "Bulk demo
/// publisher ...").
pub(crate) fn data_publishers() -> Vec<String> {
    let v: Vec<String> = deployment_rows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("Bulk publisher "))
        .map(|(_, a)| a)
        .collect();
    assert!(!v.is_empty(), "deployment.md lists a data publisher");
    v
}

#[test]
fn the_reviewed_list_names_the_demo_publisher_and_no_data_publisher() {
    let f = ReviewedFile::parse(read("deploy/early-record/exclusions.json").as_bytes())
        .expect("exclusions.json");
    let rows = deployment_rows();
    for k in &f.operator_keys {
        assert_eq!(k.doc, "docs/ops/deployment.md");
        assert_eq!(rows.get(&k.doc_row), Some(&k.address), "{}", k.doc_row);
        assert!(
            !k.doc_row.starts_with("Bulk publisher "),
            "a data publisher"
        );
    }
    assert!(f.operator_keys.iter().any(|k| k
        .doc_row
        .starts_with("Bulk demo publisher `synthetic-demo`")));
    // Every operator test key deployment.md lists is excluded from the record's first day.
    let first_day = f.operator_keys[0].from_day.clone();
    for (label, a) in rows
        .iter()
        .filter(|(l, _)| l.starts_with("Operator test key `"))
    {
        assert!(
            f.operator_keys
                .iter()
                .any(|k| &k.address == a && &k.doc_row == label && k.from_day == first_day),
            "{label} is not excluded from {first_day}"
        );
    }
    assert!(
        f.operator_keys.len() > 20,
        "the operator's test keys are listed"
    );
    // The record's own key is the one deployment.md lists for it, and no data publisher.
    let rec = f
        .record_publisher
        .as_ref()
        .expect("the record's key is set");
    let label = rows
        .keys()
        .find(|l| l.starts_with("Record publisher `early-record`"))
        .expect("deployment.md lists the record publisher");
    assert_eq!(rows.get(label), Some(&rec.address));
    assert!(
        !data_publishers().contains(&rec.address),
        "a data publisher"
    );
    // No shipped file of the record names a data publisher's signer.
    let mut shipped: Vec<String> = std::fs::read_dir(repo().join("deploy/early-record"))
        .expect("dir")
        .map(|e| e.expect("entry").path())
        .map(|p| std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .collect();
    shipped.push(read("crates/onyx-api/src/early.rs"));
    for a in data_publishers() {
        let bare = a.trim_start_matches("0x");
        for text in &shipped {
            assert!(
                !text.to_ascii_lowercase().contains(bare),
                "a data publisher is named"
            );
        }
        assert!(fixed_exclusions().iter().all(|x| x.address != a));
    }
    // The reviewed key is excluded even though it signs for a publisher;
    // a role answer naming the same signer is not.
    let demo = f.operator_keys[0].address.clone();
    let publishers = BTreeSet::from([demo.clone()]);
    // The demo key alone here; the record's own key is checked below.
    let demo_only = ReviewedFile {
        operator_keys: vec![f.operator_keys[0].clone()],
        record_publisher: None,
        ..f.clone()
    };
    let mut wanted = demo_only.exclusions_on(DAY);
    wanted.push(Exclusion {
        address: demo.clone(),
        reason: Reason::Role,
        source: "OnyxRevenue.reporter()".into(),
    });
    let ch = exclusion_changes(&BTreeMap::new(), &wanted, &publishers, DAY).expect("rows");
    assert_eq!(ch.rows.len(), 1);
    assert_eq!(
        (
            ch.rows[0].exclusion.reason,
            ch.rows[0].exclusion.address.as_str()
        ),
        (Reason::OperatorKey, demo.as_str())
    );
    assert_eq!(ch.skipped.len(), 1);
    // The record's own key, once set, is an operator key too.
    let own = ReviewedFile {
        record_publisher: Some(RecordKey {
            address: addr_text(&[0x5a; 20]),
            from_day: day_text(DAY + 1),
        }),
        ..f.clone()
    };
    let is_own = |x: &Vec<Exclusion>| {
        x.iter()
            .any(|e| e.source == RECORD_PUBLISHER_SOURCE && e.reason == Reason::OperatorKey)
    };
    assert!(is_own(&own.exclusions_on(DAY + 1)));
    // Each entry only on its own days: not before from_day, not from until_day.
    assert!(!is_own(&own.exclusions_on(DAY)));
    let first = parse_day(&f.operator_keys[0].from_day).expect("from_day");
    assert!(
        demo_only.exclusions_on(first - 1).is_empty() && demo_only.exclusions_on(first).len() == 1
    );
    let mut ends = demo_only.clone();
    ends.operator_keys[0].until_day = Some(day_text(DAY));
    assert!(ends.exclusions_on(DAY).is_empty() && ends.exclusions_on(DAY - 1).len() == 1);
    let mut back = ends.clone();
    back.operator_keys[0].until_day = Some(back.operator_keys[0].from_day.clone());
    assert!(ReviewedFile::parse(&serde_json::to_vec(&back).expect("json")).is_err());
    // Refusals: a bad address, a repeat, a zero record key.
    let mut twice = f.clone();
    twice.operator_keys.push(f.operator_keys[0].clone());
    assert!(ReviewedFile::parse(&serde_json::to_vec(&twice).expect("json")).is_err());
    let zero = ReviewedFile {
        record_publisher: Some(RecordKey {
            address: addr_text(&[0; 20]),
            from_day: day_text(DAY),
        }),
        ..f
    };
    assert!(ReviewedFile::parse(&serde_json::to_vec(&zero).expect("json")).is_err());
}

#[test]
fn contracts_file_refusals() {
    let base: Value = serde_json::from_str(&test_contracts_json(FROM)).expect("json");
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        Contracts::parse(v.to_string().as_bytes())
    };
    assert!(bad(&|_| {}).is_ok());
    assert!(
        bad(&|v| v["contracts"][0]["address"] = json!(addr_text(&SETTLE).to_ascii_uppercase()))
            .is_err()
    );
    assert!(bad(&|v| v["contracts"][0]["address"] = json!(addr_text(&[0; 20]))).is_err());
    assert!(bad(&|v| v["contracts"][1]["address"] = json!(addr_text(&SETTLE))).is_err());
    assert!(bad(&|v| {
        let o = v["contracts"][2].as_object_mut().expect("obj");
        o.remove("scan_from");
        o.remove("scan_from_is");
        o.remove("block_doc");
        o.remove("block_quote");
    })
    .is_err());
    // A quote without the number, a deploy block without a quote, a bound
    // by an unlisted or unquoted contract, or by another block.
    assert!(bad(&|v| v["contracts"][0]["block_quote"] = json!("block 7")).is_err());
    let bound = |v: &mut Value, by: &str, from: u64| {
        let o = v["contracts"][1].as_object_mut().expect("obj");
        o.remove("block_doc");
        o.remove("block_quote");
        o.insert("scan_from_is".into(), json!("bound"));
        o.insert("bound_by".into(), json!(by));
        o.insert("scan_from".into(), json!(from));
    };
    assert!(bad(&|v| bound(v, "OnyxSettlement", FROM)).is_ok());
    assert!(bad(&|v| bound(v, "OnyxNothing", FROM)).is_err());
    assert!(bad(&|v| bound(v, "OnyxSettlement", FROM + 1)).is_err());
    assert!(bad(&|v| bound(v, "OnyxRegistry", FROM)).is_err());
    assert!(bad(&|v| {
        bound(v, "OnyxSettlement", FROM);
        v["contracts"][1]["scan_from_is"] = json!("deploy");
    })
    .is_err());
    assert!(bad(&|v| v["contracts"][6]["abi"] = json!("stakes-v1")).is_err());
    assert!(bad(&|v| {
        let a = v["contracts"].as_array_mut().expect("list");
        a.remove(1);
    })
    .is_err());
    assert!(bad(&|v| v["contracts"][0]["extra"] = json!(1)).is_err());
    assert!(bad(&|v| v["format"] = json!(2)).is_err());
    assert!(bad(&|v| v["contracts"][0]["deploy_tx"] = json!("0x12")).is_err());
    // The reader's filter: counted contracts and their event topics only.
    let (addrs, topics) = contracts().filter();
    assert!(!addrs.contains(&addr_text(&REGISTRY)));
    assert_eq!(addrs.len(), 6);
    assert!(topics.contains(&h32_text(&tp(sig::RELEASED))));
}

#[test]
fn logs_from_elsewhere_or_out_of_place_are_refused() {
    let c = Contracts::parse(test_contracts_json(150).as_bytes()).expect("contracts");
    let apply = |logs: &[ChainLog]| State::new(&c).apply(&c, FROM, 300, logs, &Views::default());
    // Before its contract's scan_from: contracts.json is wrong.
    assert!(matches!(
        apply(&[activated(120, 0, h(0x81), OPERATOR)]),
        Err(EarlyError::Log(_))
    ));
    assert!(apply(&[activated(150, 0, h(0x81), OPERATOR)]).is_ok());
    // From an address contracts.json does not list.
    let mut l = activated(160, 0, h(0x81), OPERATOR);
    l.address = [0x99; 20];
    assert!(matches!(apply(&[l]), Err(EarlyError::Log(_))));
    // Outside the range.
    assert!(matches!(
        apply(&[activated(301, 0, h(0x81), OPERATOR)]),
        Err(EarlyError::Log(_))
    ));
    // A known event of the wrong shape.
    let mut l = activated(160, 0, h(0x81), OPERATOR);
    l.topics.pop();
    assert!(matches!(apply(&[l]), Err(EarlyError::Log(_))));
    // Block times that disagree inside a block or go back.
    let mut a = activated(160, 0, h(0x81), OPERATOR);
    let mut b = deal(160, 1, 1, PAYER);
    b.block_time += 1;
    assert!(apply(&[a.clone(), b.clone()]).is_err());
    a.block_time = b.block_time + 10;
    b.block = 161;
    assert!(apply(&[a, b]).is_err());
    // A range that does not start at the cursor.
    assert!(State::new(&c)
        .apply(&c, FROM + 1, 300, &[], &Views::default())
        .is_err());
    // A state built with another contracts.json.
    let other = contracts();
    assert!(State::new(&other)
        .apply(&c, FROM, 300, &[], &Views::default())
        .is_err());
    // An unknown event of a known contract is not read.
    let mut l = activated(160, 0, h(0x81), OPERATOR);
    l.topics[0] = tp("Removed(bytes32,uint64,bool)");
    assert_eq!(apply(&[l]).expect("ignored"), vec![]);
}

#[test]
fn from_rpc_reads_a_log_and_refuses_removed() {
    let l = report(101, 2, h(0x71), h(0x61), 1);
    let v = json!({
        "address": addr_text(&l.address),
        "topics": l.topics.iter().map(h32_text).collect::<Vec<_>>(),
        "data": format!("0x{}", hex::encode(&l.data)),
        "blockNumber": format!("0x{:x}", l.block),
        "transactionHash": h32_text(&l.tx),
        "logIndex": "0x2",
        "removed": false,
    });
    assert_eq!(ChainLog::from_rpc(&v, Some(l.block_time)).expect("log"), l);
    assert!(
        ChainLog::from_rpc(&v, None).is_err(),
        "a block time is needed"
    );
    let mut t = v.clone();
    t["blockTimestamp"] = json!(format!("0x{:x}", l.block_time));
    assert_eq!(ChainLog::from_rpc(&t, None).expect("log"), l);
    assert!(ChainLog::from_rpc(&t, Some(l.block_time + 1)).is_err());
    let mut r = v;
    r["removed"] = json!(true);
    assert!(ChainLog::from_rpc(&r, Some(l.block_time)).is_err());
}

// ------------------------------------------------------------------ signatures

/// One event parameter of a contract source: type (enums as `uint8`),
/// name, indexed.
pub(crate) struct Param {
    ty: String,
    name: String,
    indexed: bool,
}

/// The canonical signature and parameters of `event name(...)` in `file`.
pub(crate) fn sol_event(file: &str, name: &str) -> (String, Vec<Param>) {
    let src = read(&format!("contracts/src/{file}"));
    let start = src
        .find(&format!("event {name}("))
        .unwrap_or_else(|| panic!("event {name} in {file}"));
    let body = &src[start + name.len() + 7..];
    let body = &body[..body.find(");").expect("end")];
    let enums = [
        "Outcome",
        "EquivocationKind",
        "Kind",
        "State",
        "By",
        "ShardStatus",
    ];
    let mut ps = Vec::new();
    for p in body.split(',') {
        let w: Vec<&str> = p.split_whitespace().collect();
        let t = w[0].rsplit('.').next().unwrap_or(w[0]);
        ps.push(Param {
            ty: if enums.contains(&t) {
                "uint8".to_string()
            } else {
                t.to_string()
            },
            name: w[w.len() - 1].to_string(),
            indexed: w.contains(&"indexed"),
        });
    }
    let types: Vec<&str> = ps.iter().map(|p| p.ty.as_str()).collect();
    (format!("{name}({})", types.join(",")), ps)
}

/// Every event the rules read: (ABI, source file, signature).
pub(crate) const READ: [(Abi, &str, &str); 26] = [
    (Abi::SettlementV1, "OnyxLedger.sol", sig::COMMITTED),
    (Abi::SettlementV1, "OnyxLedger.sol", sig::REVEALED),
    (Abi::SettlementV1, "OnyxLedger.sol", sig::VERDICT_REVEALED),
    (Abi::SettlementV1, "OnyxLedger.sol", sig::FINALIZED),
    (Abi::SettlementV1, "OnyxLedger.sol", sig::SHARD_VOIDED),
    (Abi::SettlementV1, "OnyxLedger.sol", sig::ROUND_VOIDED),
    (Abi::DisputesV1, "OnyxDisputes.sol", sig::RELEASED),
    (Abi::DisputesV1, "OnyxDisputes.sol", sig::CASE_OPENED),
    (Abi::DisputesV1, "OnyxDisputes.sol", sig::CASE_CLOSED),
    (Abi::SignedBookV1, "OnyxSignedBook.sol", sig::KEY_REGISTERED),
    (
        Abi::SignedBookV1,
        "OnyxSignedBook.sol",
        sig::REPORT_RECORDED,
    ),
    (Abi::SignedBookV1, "OnyxSignedBook.sol", sig::WITHDRAWN),
    (Abi::SignedBookV1, "OnyxSignedBook.sol", sig::EQUIVOCATED),
    (
        Abi::SignedBookV1,
        "OnyxSignedBook.sol",
        sig::KEY_COMPROMISED,
    ),
    (Abi::SignedBookV1, "OnyxSignedBook.sol", sig::KEY_RETIRED),
    (Abi::StorageV3, "OnyxStorage.sol", sig::ACTIVATED),
    (Abi::StorageV3, "OnyxStorage.sol", sig::DEAL_CREATED),
    (Abi::StorageV3, "OnyxStorage.sol", sig::ANSWERED),
    (Abi::StorageV3, "OnyxStorage.sol", sig::SLASHED),
    (Abi::StorageV3, "OnyxStorage.sol", sig::MISS_REWARD),
    (Abi::StakesV1, "OnyxStakes.sol", sig::SUBMITTED),
    (Abi::StakesV1, "OnyxStakes.sol", sig::DECIDED),
    (
        Abi::CorrectionsV1,
        "OnyxCorrections.sol",
        sig::MARKET_OPENED,
    ),
    (Abi::CorrectionsV1, "OnyxCorrections.sol", sig::BET),
    (Abi::CorrectionsV1, "OnyxCorrections.sol", sig::RESOLVED),
    (Abi::CorrectionsV1, "OnyxCorrections.sol", sig::VOIDED),
];

#[test]
fn event_topics_match_the_contract_sources() {
    let mut read_by_rules = BTreeSet::new();
    for a in [
        Abi::SettlementV1,
        Abi::DisputesV1,
        Abi::SignedBookV1,
        Abi::StorageV3,
        Abi::StakesV1,
        Abi::CorrectionsV1,
    ] {
        read_by_rules.extend(a.events().iter().map(|e| (a, *e)));
    }
    let listed: BTreeSet<(Abi, &str)> = READ.iter().map(|(a, _, s)| (*a, *s)).collect();
    assert_eq!(
        read_by_rules, listed,
        "every event the rules read is checked here"
    );
    for (_, file, s) in READ {
        let name = &s[..s.find('(').expect("(")];
        let (canon, _) = sol_event(file, name);
        assert_eq!(canon, s, "{file}");
    }
    // Two pinned topics (`cast keccak`), and the indexer's own pin.
    assert_eq!(
        h32_text(&tp(sig::RELEASED)),
        "0x16e2d3fb536c8813749505cfa6dc78a7e54581a66506792618a5987c3133e383"
    );
    assert_eq!(
        h32_text(&tp(sig::REPORT_RECORDED)),
        "0x14657c52904a709688cb2c9863ad729c80a0277420c705c25180ea89e3f747b7"
    );
    assert_eq!(h32_text(&tp(sig::FINALIZED)), crate::roots::FINALIZED_TOPIC);
}

/// A log of `s` encoded by hand from the source's parameter list, every
/// field a distinct value: parameter `i` holds 100 + i (an integer, a bool
/// holds 1), the address `[0x20 + i; 20]`, the word `[0x40 + i; 32]` or the
/// string `s<i>`. Returns the log and the value text of each parameter.
pub(crate) fn hand_encoded(
    abi_tag: Abi,
    file: &str,
    s: &str,
) -> (ChainLog, BTreeMap<String, String>) {
    let name = &s[..s.find('(').expect("(")];
    let (_, ps) = sol_event(file, name);
    let mut topics = vec![tp(s)];
    let mut toks = Vec::new();
    let mut vals = BTreeMap::new();
    for (i, p) in ps.iter().enumerate() {
        let n = u8::try_from(i).expect("few params");
        let (w, text) = match p.ty.as_str() {
            "address" => (aword(&[0x20 + n; 20]), addr_text(&[0x20 + n; 20])),
            "bytes32" => ([0x40 + n; 32], h32_text(&h(0x40 + n))),
            "bool" => (word(1), "true".to_string()),
            "string" | "bytes" => ([0; 32], format!("s{i}")),
            _ => (word(100 + u128::from(n)), (100 + u128::from(n)).to_string()),
        };
        vals.insert(p.name.clone(), text);
        match (p.indexed, p.ty.as_str()) {
            (true, _) => topics.push(Hash32(w)),
            (false, "string" | "bytes") => toks.push(Tok::Bytes(format!("s{i}").into_bytes())),
            (false, _) => toks.push(Tok::Word(w)),
        }
    }
    let addr = *[SETTLE, DISPUTES, BOOK, STORAGE, STAKES, CORR]
        .iter()
        .zip([
            Abi::SettlementV1,
            Abi::DisputesV1,
            Abi::SignedBookV1,
            Abi::StorageV3,
            Abi::StakesV1,
            Abi::CorrectionsV1,
        ])
        .find(|(_, a)| *a == abi_tag)
        .map(|(x, _)| x)
        .expect("address");
    (lgt(addr, 150, 0, 1, topics, abi::encode(&toks)), vals)
}

#[test]
fn every_event_field_is_read_from_its_own_topic_or_data_word() {
    for (a, file, s) in READ {
        let (l, v) = hand_encoded(a, file, s);
        let g = |k: &str| {
            v.get(k)
                .unwrap_or_else(|| panic!("{s}: no parameter {k}"))
                .clone()
        };
        let n = |k: &str| g(k).parse::<u64>().unwrap_or_else(|_| panic!("{s}: {k}"));
        let u8_ = |k: &str| u8::try_from(n(k)).expect("u8");
        let u = |k: &str| u128::from(n(k));
        let rs = || format!("{}:{}", g("round"), g("shard"));
        let want = match s {
            sig::COMMITTED => Ev::Committed {
                rs: rs(),
                miner: g("miner"),
            },
            sig::REVEALED => Ev::Revealed {
                rs: rs(),
                miner: g("miner"),
                root: g("root"),
            },
            sig::VERDICT_REVEALED => Ev::Verdict {
                rs: rs(),
                validator: g("validator"),
                kind: u8_("kind"),
            },
            sig::FINALIZED => Ev::Finalized {
                rs: rs(),
                root: g("root"),
            },
            sig::SHARD_VOIDED => Ev::ShardVoided { rs: rs() },
            sig::ROUND_VOIDED => Ev::RoundVoided { round: n("round") },
            sig::RELEASED => Ev::Released { rs: rs() },
            sig::CASE_OPENED => Ev::CaseOpened {
                case: g("caseId"),
                rs: rs(),
            },
            sig::CASE_CLOSED => Ev::CaseClosed { case: g("caseId") },
            sig::KEY_REGISTERED => Ev::KeyRegistered {
                key: g("key"),
                signer: g("signer"),
            },
            sig::REPORT_RECORDED => Ev::Report {
                digest: g("digest"),
                key: g("key"),
                outcome: u8_("outcome"),
            },
            sig::WITHDRAWN => Ev::Withdrawn {
                digest: g("digest"),
            },
            sig::EQUIVOCATED => Ev::Equivocated {
                first: g("first"),
                second: g("second"),
            },
            sig::KEY_COMPROMISED => Ev::Compromised {
                key: g("key"),
                since: n("since"),
            },
            sig::KEY_RETIRED => Ev::Retired {
                key: g("key"),
                at: n("at"),
            },
            sig::ACTIVATED => Ev::Activated {
                node: g("nodeId"),
                operator: g("operator"),
            },
            sig::DEAL_CREATED => Ev::Deal {
                deal: n("dealId"),
                payer: g("payer"),
            },
            sig::ANSWERED => Ev::Answered {
                subject: format!(
                    "{}:{}:{}:{}",
                    g("dealId"),
                    g("epoch"),
                    g("slot"),
                    g("nodeId")
                ),
                deal: n("dealId"),
                node: g("nodeId"),
                passed: g("passed") == "true",
                paid: u("paid"),
            },
            sig::SLASHED => Ev::Slashed {
                node: g("nodeId"),
                subject: format!(
                    "{}:{}:{}:{}",
                    g("dealId"),
                    g("epoch"),
                    g("slot"),
                    g("nodeId")
                ),
                deal: n("dealId"),
                cause: u8_("cause"),
            },
            sig::MISS_REWARD => Ev::MissReward {
                node: g("nodeId"),
                reporter: g("reporter"),
                amount: u("amount"),
            },
            sig::SUBMITTED => Ev::Submitted {
                claim: g("claimId"),
                fact: g("fact"),
                submitter: g("submitter"),
                value: h32_text(&Hash32(word(u("value")))),
            },
            sig::DECIDED => Ev::Decided {
                claim: g("claimId"),
                state: u8_("state"),
                by: u8_("by"),
            },
            sig::MARKET_OPENED => Ev::Opened {
                market: g("marketId"),
                opener: g("opener"),
            },
            sig::BET => Ev::Bet {
                market: g("marketId"),
                who: g("who"),
                side: u8_("side"),
                amount: u("amount"),
            },
            sig::RESOLVED => Ev::Resolved {
                market: g("marketId"),
                winner: u8_("winner"),
            },
            sig::VOIDED => Ev::MarketVoided {
                market: g("marketId"),
            },
            _ => panic!("{s} has no expected decoding"),
        };
        assert_eq!(decode(a, &l).expect("decodes"), Some(want), "{s}");
    }
}

#[test]
fn role_calls_have_the_pinned_selectors() {
    // `cast sig` of each getter.
    let pinned = [
        ("coordinator()", "0x0a009097"),
        ("owner()", "0x8da5cb5b"),
        ("receiptSigner()", "0x9e43c343"),
        ("reviewer()", "0xa54044f8"),
        ("treasury()", "0x61d027b3"),
        ("minter()", "0x07546172"),
        ("reporter()", "0x010ec441"),
    ];
    let real = Contracts::load(&repo().join("deploy/early-record/contracts.json"))
        .expect("contracts.json");
    let calls = role_calls(&real, 53_000_000);
    for (g, s) in pinned {
        let c = calls
            .iter()
            .find(|c| c.getter == g)
            .unwrap_or_else(|| panic!("{g}"));
        assert_eq!(c.params[0]["data"], json!(s), "{g}");
        assert_eq!(c.params[1], json!("0x328b740"));
        assert_eq!(c.params[0]["to"], json!(c.to));
    }
    // Each getter is declared in the named contract's own source (the
    // settlement's roles live in OnyxLedger, which it extends).
    let file_of = |a: Abi| match a {
        Abi::SettlementV1 => "OnyxLedger.sol",
        Abi::RegistryV1 => "OnyxRegistry.sol",
        Abi::SignedBookV1 => "OnyxSignedBook.sol",
        Abi::StorageV3 | Abi::StorageV4 => "OnyxStorage.sol",
        Abi::StakesV1 => "OnyxStakes.sol",
        Abi::CorrectionsV1 => "OnyxCorrections.sol",
        Abi::PanelOracleV1 => "OnyxPanelOracle.sol",
        Abi::ReobserveV1 => "OnyxReobserve.sol",
        Abi::TestTokenV1 => "OnyxTestToken.sol",
        Abi::RevenueV1 => "OnyxRevenue.sol",
        other => panic!("{other:?} has no roles"),
    };
    let mut seen = 0;
    for c in &calls {
        let e = real.get(&parse_address(&c.to).expect("to")).expect("entry");
        let src = read(&format!("contracts/src/{}", file_of(e.abi)));
        let n = c.getter.trim_end_matches("()");
        assert!(
            src.contains(&format!("public immutable {n};"))
                || src.contains(&format!("public {n};"))
                || src.contains(&format!("function {n}() external view")),
            "{}.{n} in {}",
            c.contract,
            file_of(e.abi)
        );
        seen += 1;
    }
    // 16 of list 1, and the treasury of the second storage contract.
    assert_eq!(seen, 17, "every role call of the live contracts is checked");
    let r = &calls[0];
    assert!(r.exclusion("0x12").is_err());
    assert!(
        r.exclusion(&format!("0x{}", hex::encode([0xffu8; 32])))
            .is_err(),
        "not an address"
    );
}

// ------------------------------------------------------------------ schema and limits

#[test]
fn schema_files_match_the_code() {
    for (file, s) in [
        ("deploy/early-record/schema.json", record_schema()),
        (
            "deploy/early-record/exclusions-schema.json",
            exclusions_schema(),
        ),
    ] {
        let text = read(file);
        assert_eq!(
            text,
            schema_file(&s),
            "{file}: regenerate from early::schema_file"
        );
        let parsed =
            Schema::from_json(&serde_json::from_str(&text).expect("json")).expect("schema");
        assert_eq!(parsed, s);
        s.validate().expect("valid");
    }
    // The exclusions CSV parses under its schema.
    let rows = exclusion_changes(&BTreeMap::new(), &fixed_exclusions(), &BTreeSet::new(), DAY)
        .expect("rows")
        .rows;
    let csv = exclusions_csv(&rows).expect("csv");
    let mut r = Records::new(csv.as_bytes(), exclusions_schema(), Format::Csv).expect("header");
    let mut n = 0;
    while r.next_record().expect("record").is_some() {
        n += 1;
    }
    assert_eq!(n, ANVIL_ACCOUNTS.len() + PLACEHOLDERS.len());
}

#[test]
fn worst_case_rows_fit_the_bulk_limits() {
    let c = contracts();
    let big = Ref {
        block: u64::MAX,
        log: u64::MAX,
        tx: h32_text(&h(0xff)),
        time: T0,
    };
    let r = Row {
        kind: Kind::StorageMissReport,
        who: addr_text(&[0xff; 20]),
        subject: format!(
            "{}:{}:{}:{}",
            u64::MAX,
            u64::MAX,
            u32::MAX,
            h32_text(&h(0xff))
        ),
        contract: addr_text(&[0xff; 20]),
        accept: big.clone(),
        work: big,
        counterparty: Some(addr_text(&[0xff; 20])),
        amount: Some(i128::MAX as u128),
        detail: "reward".into(),
        check: CHECK_CHAIN,
        revokes: None,
    };
    let close = DayClose {
        from_block: 0,
        to_block: u64::MAX,
        ..close_for(&c, 0, 0)
    };
    let v = write_day(DAY, std::slice::from_ref(&r), &close).expect("fits");
    build(&v.csv, None, T0);
    assert!(v.csv.lines().all(|l| l.len() < 1024), "every line is short");
    // Over i128, or a text cell over the cap, is refused rather than cut.
    let mut over = r.clone();
    over.amount = Some(u128::MAX);
    assert!(write_day(DAY, &[over], &close).is_err());
    let mut long = r;
    long.subject = "x".repeat(MAX_TEXT_CELL + 1);
    assert!(write_day(DAY, &[long], &close).is_err());
}

#[test]
fn rows_hold_only_addresses_hashes_and_codes() {
    // EARLY-12: the record copies no string from any event (key sources,
    // origins, reasons, deal names, URLs) and names no person.
    let csv = day_csv(&full_day());
    for (i, l) in csv.lines().enumerate().skip(1) {
        assert!(
            l.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:.=;-,".contains(&b)),
            "line {i}: {l}"
        );
    }
    for s in ["Example", "example", "jane", "btc.price", "https"] {
        assert!(!csv.contains(s), "{s}");
    }
}

// ------------------------------------------------------------------ a second storage contract, a newer list

/// The second storage contract (`storage-v4`).
pub(crate) const STORAGE4: [u8; 20] = [0x18; 20];

/// `l` as the second storage contract emits it.
pub(crate) fn v4(mut l: ChainLog) -> ChainLog {
    l.address = STORAGE4;
    l
}

/// The list after `first`: the same contracts and a `storage-v4` contract
/// read from block `v4_from`, in force from `from_day`; `first` is its
/// `contracts-1.json`.
pub(crate) fn list_2_json_of(first: &str, from_day: u32, v4_from: u64) -> String {
    let mut v: Value = serde_json::from_str(first).expect("json");
    v["contracts"].as_array_mut().expect("list").push(json!({
        "name": "OnyxStorage (v4)", "abi": "storage-v4", "address": addr_text(&STORAGE4),
        "doc": "test", "scan_from": v4_from, "scan_from_is": "deploy", "block_doc": "test",
        "block_quote": format!("block {v4_from}"),
    }));
    v["from_day"] = json!(day_text(from_day));
    v["previous"] = json!({
        "file": "contracts-1.json",
        "keccak256": h32_text(&keccak256(first.as_bytes())),
    });
    v.to_string()
}

pub(crate) fn list_2_json(from_day: u32, v4_from: u64) -> String {
    list_2_json_of(&test_contracts_json(FROM), from_day, v4_from)
}

fn with_list_1(json: &str) -> Result<Contracts, EarlyError> {
    Contracts::parse_with(json.as_bytes(), &mut |f| match f {
        "contracts-1.json" => Ok(test_contracts_json(FROM).into_bytes()),
        _ => Err(EarlyError::Contracts(format!("no file {f}"))),
    })
}

fn list_2(from_day: u32, v4_from: u64) -> Contracts {
    with_list_1(&list_2_json(from_day, v4_from)).expect("list 2")
}

/// Both storage contracts in one list with no history.
fn both_storages() -> Contracts {
    let mut v: Value = serde_json::from_str(&list_2_json(DAY, FROM)).expect("json");
    let o = v.as_object_mut().expect("obj");
    o.remove("from_day");
    o.remove("previous");
    Contracts::parse(v.to_string().as_bytes()).expect("both")
}

#[test]
fn a_list_takes_a_second_storage_contract_and_refuses_a_conflict() {
    let one = contracts();
    let two = list_2(DAY + 2, FROM + 90_000);
    let name = |a: Abi| two.of(a).map(|e| e.address.clone());
    assert_eq!(name(Abi::StorageV3), Some(addr_text(&STORAGE)));
    assert_eq!(name(Abi::StorageV4), Some(addr_text(&STORAGE4)));
    // The same five events and the same role getter: nothing new is asked
    // of the node but one more address. `DealAnchor` is not read.
    assert_eq!(Abi::StorageV4.events(), Abi::StorageV3.events());
    assert_eq!(Abi::StorageV4.roles(), Abi::StorageV3.roles());
    let (addrs, topics) = two.filter();
    assert_eq!(addrs.len(), 7);
    assert!(addrs.contains(&addr_text(&STORAGE4)));
    assert_eq!(topics, one.filter().1);
    assert!(!topics.contains(&h32_text(&tp("DealAnchor(uint64,bytes32,bool)"))));
    assert!(!one.filter().0.contains(&addr_text(&STORAGE4)));
    // The history: each day has one list.
    assert!(one.previous.is_none() && one.from_day.is_none());
    assert_eq!(two.first().hash, one.hash);
    assert_eq!(two.from_day, Some(DAY + 2));
    assert_eq!(two.on(DAY).hash, one.hash);
    assert_eq!(two.on(DAY + 1).hash, one.hash);
    assert_eq!(two.on(DAY + 2).hash, two.hash);
    assert_eq!(two.on(DAY + 900).hash, two.hash);
    assert_eq!(one.on(DAY + 900).hash, one.hash);
    assert_ne!(one.hash, two.hash);
    assert!(two.find(&h32_text(&one.hash)).is_some());
    assert!(one.find(&h32_text(&two.hash)).is_none());
    assert_eq!(two.after(two.first()).map(|c| c.hash), Some(two.hash));
    assert!(two.after(&two).is_none());
    let added: Vec<&str> = two
        .added_since(two.first())
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(added, ["OnyxStorage (v4)"]);
    assert!(two.added_since(&two).is_empty());

    let base: Value = serde_json::from_str(&list_2_json(DAY + 2, FROM + 90_000)).expect("json");
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        with_list_1(&v.to_string())
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let refused = |f: &dyn Fn(&mut Value), why: &str| {
        let e = bad(f).expect_err(why);
        assert!(e.contains(why), "{e}");
    };
    assert_eq!(bad(&|_| {}), Ok(()));
    // The conflict: two contracts with one counted ABI tag. The join keeps
    // one set of ids per tag, so their deals and nodes would mix.
    let twice = "a second contract with a counted ABI";
    refused(&|v| v["contracts"][7]["abi"] = json!("storage-v3"), twice);
    refused(
        &|v| {
            let mut e = v["contracts"][7].clone();
            e["address"] = json!(addr_text(&[0x19; 20]));
            v["contracts"].as_array_mut().expect("list").push(e);
        },
        twice,
    );
    refused(
        &|v| v["contracts"][7]["address"] = json!(addr_text(&STORAGE)),
        "listed twice",
    );
    refused(
        &|v| v["contracts"][7]["abi"] = json!("storage-v5"),
        "storage-v5",
    );
    // A newer list only adds, and an earlier list never changes.
    let together = "from_day and previous go together";
    refused(&|v| v["from_day"] = Value::Null, together);
    refused(&|v| v["previous"] = Value::Null, together);
    refused(&|v| v["from_day"] = json!("2026-11-31"), "from_day");
    refused(
        &|v| v["previous"]["keccak256"] = json!(h32_text(&h(1))),
        "never changes",
    );
    for f in ["../contracts-1.json", "a/contracts-1.json", ".hidden", ""] {
        refused(&|v| v["previous"]["file"] = json!(f), "same folder");
    }
    refused(
        &|v| v["previous"]["file"] = json!("contracts-0.json"),
        "no file",
    );
    let gone = "is gone or changed";
    refused(
        &|v| {
            v["contracts"].as_array_mut().expect("list").remove(3);
        },
        gone,
    );
    refused(
        &|v| {
            v["contracts"][3]["scan_from"] = json!(FROM + 1);
            v["contracts"][3]["block_quote"] = json!(format!("block {}", FROM + 1));
        },
        gone,
    );
    refused(&|v| v["chain_id"] = json!(1), "another chain");
    // A list that names an earlier one is not parsed alone.
    assert!(Contracts::parse(base.to_string().as_bytes()).is_err());
    // A third list: its first day is after the second's.
    let second = base.to_string();
    let third = |from_day: u32| {
        let mut v = base.clone();
        v["from_day"] = json!(day_text(from_day));
        v["previous"] = json!({
            "file": "contracts-2.json",
            "keccak256": h32_text(&keccak256(second.as_bytes())),
        });
        Contracts::parse_with(v.to_string().as_bytes(), &mut |f| {
            Ok(match f {
                "contracts-2.json" => second.clone().into_bytes(),
                _ => test_contracts_json(FROM).into_bytes(),
            })
        })
    };
    assert!(third(DAY + 2).is_err() && third(DAY + 1).is_err());
    let three = third(DAY + 5).expect("list 3");
    assert_eq!(three.on(DAY + 1).hash, one.hash);
    assert_eq!(three.on(DAY + 4).hash, two.hash);
    assert_eq!(three.on(DAY + 5).hash, three.hash);
    assert_eq!(three.after(three.first()).map(|c| c.hash), Some(two.hash));
    assert!(three.added_since(three.first()).len() == 1 && three.added_since(&two).is_empty());
}

/// Every counted storage event of the second contract gives the row the
/// first contract's gives, under its own address; the two contracts' deal
/// and node ids never meet.
#[test]
fn storage_v4_work_is_counted_like_v3_and_kept_apart() {
    let c = both_storages();
    let node = h(0x81);
    let run =
        |logs: &[ChainLog]| State::new(&c).apply(&c, FROM, FROM + 1000, logs, &Views::default());
    // The same node id and the same deal id on both contracts, with other
    // operators and payers.
    let mut logs = vec![
        activated(108, 0, node, OPERATOR),
        deal(108, 1, 0, PAYER),
        v4(activated(108, 2, node, MINER_A)),
        v4(deal(108, 3, 0, MINER_A)),
        // A log of the new event: not read.
        lgt(
            STORAGE4,
            108,
            4,
            108_003,
            vec![tp("DealAnchor(uint64,bytes32,bool)"), tu(0)],
            [[7u8; 32], word(1)].concat(),
        ),
        answered(109, 0, 0, 2, 0, node, true, 5),
        v4(answered(109, 1, 0, 2, 0, node, true, 6)),
        v4(answered(109, 2, 0, 2, 1, node, false, 0)),
        v4(slashed(110, 0, 1100, node, 0, 2, 1, 3)),
        v4(miss_reward(110, 1, 1100, node, REPORTER, 9)),
    ];
    let rows = run(&logs).expect("apply");
    let cell = |r: &Row| {
        (
            r.kind,
            r.contract.clone(),
            r.who.clone(),
            r.counterparty.clone().unwrap_or_default(),
            r.detail.clone(),
            r.amount.unwrap_or_default(),
            r.subject.clone(),
        )
    };
    let subject = |slot: u32| format!("0:2:{slot}:{}", h32_text(&node));
    let (three, four) = (addr_text(&STORAGE), addr_text(&STORAGE4));
    assert_eq!(
        rows.iter().map(cell).collect::<Vec<_>>(),
        vec![
            (
                Kind::StorageProof,
                three.clone(),
                addr_text(&OPERATOR),
                addr_text(&PAYER),
                "other".to_string(),
                5,
                subject(0)
            ),
            (
                Kind::StorageProof,
                four.clone(),
                addr_text(&MINER_A),
                addr_text(&MINER_A),
                "self".to_string(),
                6,
                subject(0)
            ),
            (
                Kind::StorageMissReport,
                four.clone(),
                addr_text(&REPORTER),
                addr_text(&MINER_A),
                "reward".to_string(),
                9,
                subject(1)
            ),
        ]
    );
    // The version takes both proofs of slot 0 (same subject, other contract).
    let v = write_day(DAY, &rows, &close_for(&c, FROM, FROM + 1000)).expect("version");
    assert_eq!(v.rows, 4);
    // Each contract needs its own earlier logs: a node or a deal of the
    // other contract does not stand in, and a repeat is refused per contract.
    let only_v3 = [
        activated(108, 0, node, OPERATOR),
        deal(108, 1, 0, PAYER),
        v4(activated(108, 2, node, MINER_A)),
        v4(answered(109, 1, 0, 2, 0, node, true, 6)),
    ];
    assert!(matches!(run(&only_v3), Err(EarlyError::Join(m)) if m.contains("DealCreated")));
    let no_node = [
        activated(108, 0, node, OPERATOR),
        v4(deal(108, 3, 0, MINER_A)),
        v4(answered(109, 1, 0, 2, 0, node, true, 6)),
    ];
    assert!(matches!(run(&no_node), Err(EarlyError::Join(m)) if m.contains("Activated")));
    let reward_only = [
        v4(activated(108, 2, node, MINER_A)),
        v4(deal(108, 3, 0, MINER_A)),
        slashed(110, 0, 1100, node, 0, 2, 1, 3),
        v4(miss_reward(110, 1, 1100, node, REPORTER, 9)),
    ];
    assert!(matches!(run(&reward_only), Err(EarlyError::Join(m)) if m.contains("Slashed")));
    logs.push(v4(activated(111, 0, node, MINER_B)));
    assert!(matches!(run(&logs), Err(EarlyError::Join(m)) if m.contains("activated twice")));
    logs.pop();
    logs.push(v4(deal(111, 0, 0, PAYER)));
    assert!(matches!(run(&logs), Err(EarlyError::Join(m)) if m.contains("created twice")));
    // A malformed event of the second contract is refused like the first's.
    let mut short = v4(activated(108, 2, node, MINER_A));
    short.topics.pop();
    assert!(matches!(run(&[short]), Err(EarlyError::Log(_))));
    // The saved state: the first contract's facts where they always were,
    // the second's under its address; and without a second contract the
    // state has no new field at all.
    let mut s = State::new(&c);
    s.apply(&c, FROM, FROM + 1000, &logs[..5], &Views::default())
        .expect("apply");
    let j: Value = serde_json::from_slice(&s.to_json()).expect("json");
    let (n, f) = (h32_text(&node), addr_text(&STORAGE4));
    assert_eq!(j["operators"], json!({ n.clone(): addr_text(&OPERATOR) }));
    assert_eq!(j["payers"], json!({ "0": addr_text(&PAYER) }));
    assert_eq!(
        j["operators_at"],
        json!({ format!("{f}:{n}"): addr_text(&MINER_A) })
    );
    assert_eq!(
        j["payers_at"],
        json!({ format!("{f}:0"): addr_text(&MINER_A) })
    );
    assert_eq!(State::from_json(&s.to_json()).expect("back"), s);
    let one = contracts();
    let mut old = State::new(&one);
    old.apply(&one, FROM, FROM + 1000, &full_day(), &Views::default())
        .expect("apply");
    let text = String::from_utf8(old.to_json()).expect("utf8");
    assert!(!text.contains("operators_at") && !text.contains("payers_at"));
}

/// The first storage contract through its history, then the second one from
/// its deploy block: days built with the first list keep every byte when the
/// file has a newer list, and the first version of the newer list holds the
/// new contract's rows from its `scan_from` block on.
#[test]
fn a_v3_then_v4_history_keeps_old_days_and_counts_the_new_contract() {
    let one = contracts();
    let (b2, b3) = (FROM + 86_400, FROM + 2 * 86_400);
    let v4_from = b2 + 500;
    let two = list_2(DAY + 2, v4_from);
    let (node, node4) = (h(0x81), h(0x84));
    let day1 = full_day();
    // Day 2: the first contract works; the second is deployed and works too.
    let day2_v3 = vec![
        answered(b2 + 10, 0, 1, 3, 0, node, true, 6),
        answered(v4_from + 60, 0, 1, 4, 0, node, true, 6),
    ];
    let day2_v4 = [
        v4(activated(v4_from, 0, node4, MINER_A)),
        v4(deal(v4_from, 1, 0, PAYER)),
        v4(answered(v4_from + 50, 0, 0, 2, 0, node4, true, 7)),
        v4(answered(v4_from + 51, 0, 0, 2, 1, node4, false, 0)),
    ];
    let day3 = vec![
        v4(answered(b3 + 5, 0, 0, 3, 0, node4, true, 8)),
        answered(b3 + 6, 0, 1, 5, 0, node, true, 6),
        v4(slashed(b3 + 7, 0, 77, node4, 0, 3, 1, 3)),
        v4(miss_reward(b3 + 7, 1, 77, node4, REPORTER, 9)),
    ];
    let v = Views::default();
    // As published: the first list alone.
    let mut a = State::new(&one);
    let a1 = a
        .record_day(&one, &day1, &v, DAY, &close_for(&one, FROM, b2 - 1))
        .expect("day 1");
    let a1_state = a.to_json();
    let a2 = a
        .record_day(&one, &day2_v3, &v, DAY + 1, &close_for(&one, b2, b3 - 1))
        .expect("day 2");
    // With the newer list in the file: the same bytes on both days.
    let mut b = State::new(two.first());
    assert_eq!(b, State::new(&one));
    let c1 = two.on(DAY);
    assert_eq!(b.late(c1, FROM).expect("late"), None);
    let b1 = b
        .record_day(c1, &day1, &v, DAY, &close_for(c1, FROM, b2 - 1))
        .expect("day 1");
    assert_eq!((b1.csv.as_str(), b.to_json()), (a1.csv.as_str(), a1_state));
    let c2 = two.on(DAY + 1);
    let b2v = b
        .record_day(c2, &day2_v3, &v, DAY + 1, &close_for(c2, b2, b3 - 1))
        .expect("day 2");
    assert_eq!(
        (b2v.csv.as_str(), b.to_json()),
        (a2.csv.as_str(), a.to_json())
    );
    assert!(!b2v.csv.contains(&addr_text(&STORAGE4)));
    // A log of the contract the first list does not name stops a day built
    // with it; the reader never asks for one (its filter has no such address).
    let mut early = a.clone();
    assert!(matches!(
        early.record_day(&one, &day3, &v, DAY + 2, &close_for(&one, b3, b3 + 86_399)),
        Err(EarlyError::Log(m)) if m.contains("does not list")
    ));
    assert_eq!(early, a);

    // Day 3, the newer list's first day. What the node gives for the late
    // range holds the first contract's logs of day 2 again.
    let c3 = two.on(DAY + 2);
    let late = b.late(c3, b3).expect("late").expect("a late range");
    assert_eq!(
        late,
        Late {
            from_block: v4_from,
            contracts: BTreeSet::from([addr_text(&STORAGE4)]),
        }
    );
    let read: Vec<ChainLog> = [&day2_v3[1..], &day2_v4[..], &day3[..]].concat();
    let logs = late.keep(b3, read.clone());
    assert_eq!(logs.len(), read.len() - 1);
    let close = DayClose {
        late: Some(late.clone()),
        ..close_for(c3, b3, b3 + 86_399)
    };
    let before = b.clone();
    // The first contract's earlier log is refused, not counted twice; and
    // the close must name the late range.
    assert!(matches!(
        b.clone().record_day(c3, &read, &v, DAY + 2, &close),
        Err(EarlyError::Log(m)) if m.contains("outside the range")
    ));
    assert!(b
        .clone()
        .record_day(c3, &logs, &v, DAY + 2, &close_for(c3, b3, b3 + 86_399))
        .is_err());
    let b3v = b.record_day(c3, &logs, &v, DAY + 2, &close).expect("day 3");
    let lines: Vec<&str> = b3v.csv.lines().skip(1).collect();
    let four = addr_text(&STORAGE4);
    assert_eq!(lines.len(), 5, "{}", b3v.csv);
    // The late proof of day 2 comes first, in day 3's version, with its own block.
    assert!(lines[0].contains(&format!(
        ",2026-11-16,storage_proof,{},",
        addr_text(&MINER_A)
    )));
    assert!(lines[0].contains(&four) && lines[0].contains(&format!(",{},0,", v4_from + 50)));
    assert!(lines[1].contains(",storage_proof,") && lines[1].contains(&four));
    assert!(lines[2].contains(",storage_proof,") && lines[2].contains(&addr_text(&STORAGE)));
    assert!(lines[3].contains(",storage_miss_report,") && lines[3].contains(&four));
    assert!(lines[4].contains(",day_close,"));
    assert!(lines[4].contains(&format!(",{b3}-{},", b3 + 86_399)));
    assert!(lines[4].contains(&format!("contracts={};", h32_text(&two.hash))));
    assert!(a2
        .csv
        .contains(&format!("contracts={};", h32_text(&one.hash))));
    assert_eq!(b.contracts, h32_text(&two.hash));
    assert_eq!(b.late(c3, b.next_block).expect("late"), None);
    // Keys ascend, so day 3's version follows day 2's.
    let m1 = build(&b1.csv, None, T0);
    let m2 = build(&b2v.csv, Some(&m1), T0 + 86_400);
    let m3 = build(&b3v.csv, Some(&m2), T0 + 2 * 86_400);
    m3.check_successor(&m2).expect("day 3 follows day 2");
    // A late row is taken only from a contract the list adds, and only from
    // the late range on.
    let rows = before
        .clone()
        .apply(c3, b3, b3 + 86_399, &logs, &v)
        .expect("rows");
    let mut wrong = rows.clone();
    wrong[0].contract = addr_text(&STORAGE);
    assert!(matches!(
        write_day(DAY + 2, &wrong, &close),
        Err(EarlyError::Write(_))
    ));
    let mut wrong = rows.clone();
    wrong[0].accept.block = v4_from - 1;
    assert!(matches!(
        write_day(DAY + 2, &wrong, &close),
        Err(EarlyError::Write(_))
    ));
    assert!(write_day(DAY + 2, &rows, &close_for(c3, b3, b3 + 86_399)).is_err());
    assert_eq!(
        write_day(DAY + 2, &rows, &close).expect("same").csv,
        b3v.csv
    );
    // The same history again gives the same bytes; a state of a list that is
    // not in the file's history is refused.
    let mut again = before.clone();
    let shuffled: Vec<ChainLog> = logs.iter().rev().cloned().collect();
    assert_eq!(
        again
            .record_day(c3, &shuffled, &v, DAY + 2, &close)
            .expect("again")
            .csv,
        b3v.csv
    );
    assert_eq!(again.to_json(), b.to_json());
    let other = Contracts::parse(test_contracts_json(150).as_bytes()).expect("other");
    assert!(matches!(
        State::new(&other).apply(c3, 150, b3, &[], &v),
        Err(EarlyError::Join(_))
    ));
    // A newer state never goes back to the list before.
    assert!(matches!(
        b.clone().apply(&one, b.next_block, b.next_block, &[], &v),
        Err(EarlyError::Join(_))
    ));
}

/// The hash of the first list, `deploy/early-record/contracts-1.json`:
/// every `day_close` up to 2026-11-19 names it.
const LIST_1_KECCAK: &str = "0xa5b599c58c19f77f4629277c503570ec8b62dcbad303924065972d9c1fb4e976";

#[test]
fn the_shipped_list_keeps_the_first_list_and_adds_storage_v4() {
    let head = Contracts::load(&repo().join("deploy/early-record/contracts.json"))
        .expect("contracts.json");
    let first = head.first();
    assert_eq!(h32_text(&first.hash), LIST_1_KECCAK);
    assert_eq!(
        h32_text(&keccak256(
            &std::fs::read(repo().join("deploy/early-record/contracts-1.json")).expect("list 1")
        )),
        LIST_1_KECCAK
    );
    assert!(first.previous.is_none() && first.from_day.is_none());
    assert_eq!(head.previous.as_deref().map(|p| p.hash), Some(first.hash));
    assert_eq!(head.from_day, parse_day("2026-11-20"));
    assert_eq!(
        h32_text(&head.on(parse_day("2026-11-19").expect("day")).hash),
        LIST_1_KECCAK
    );
    assert_eq!(
        head.on(parse_day("2026-11-20").expect("day")).hash,
        head.hash
    );
    // Every entry of the first list is kept as it was, in its place.
    let n = first.file.contracts.len();
    assert_eq!(&head.file.contracts[..n], &first.file.contracts[..]);
    assert_eq!(head.file.contracts.len(), n + 1);
    let added = head.added_since(first);
    assert_eq!(added.len(), 1);
    let e = added[0];
    assert_eq!(
        (
            e.name.as_str(),
            e.abi,
            e.address.as_str(),
            e.scan_from,
            e.scan_from_is
        ),
        (
            "OnyxStorage (v4)",
            Abi::StorageV4,
            "0x80833ee1b5f14349c6d41e75259974d29cd86e6b",
            Some(60_983_049),
            Some(ScanFrom::Deploy)
        )
    );
    assert_eq!(
        head.of(Abi::StorageV3).map(|e| e.address.as_str()),
        Some("0x9244eeaf5053f90ca2fbce157a3446f828c5ee43")
    );
}
