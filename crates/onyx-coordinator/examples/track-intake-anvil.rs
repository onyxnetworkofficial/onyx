//! ROADMAP C5 on a local anvil (ADR-0036 amendment 1): signed statements go
//! through the track-record intake, batch headers are posted as calldata
//! from a dedicated poster to a sink, `committed_at` comes from the blocks
//! that carry them, sealed statements stay hidden until their close, and a
//! client rechecks the log from its own reads of the chain. Then the attacks:
//! a revoked key, a replay, a statement past its close, a backdated log and a
//! hidden post. ROADMAP C20: both commitments go into a publication file as a
//! node reads it, and a client reruns `compute` and `verify_log` on the logs
//! served from it.
//!
//! Run by `scripts/track-intake-anvil.sh` (which starts the anvil):
//! `cargo run -p onyx-coordinator --example track-intake-anvil -- --rpc URL
//! --cast PATH`. Local only: chain id 31337 on a loopback URL, anvil's public
//! unlocked dev account 3 posts; the sources' signing keys are derived from
//! fixed labels at run time (test keys, never real ones).

use onyx_coordinator::cast_chain::{is_loopback_rpc, Finality};
use onyx_coordinator::track_chain::{write_publication, CastRecord};
use onyx_core::{
    keccak256, Confidence, DataRights, Hash32, KeyProvenance, MetricId, MetricInfo, SourceId,
    SourceKey, StaticRegistry, Subject, Value, ValueKind,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use onyx_track::intake::{
    key_id, verify_log, FinalizedRecord, Intake, IntakeError, IntakeId, IntakeView, KeyBook,
    SignedStatement, StatementPayload, TrackKey,
};
use onyx_track::publication::Publication;
use onyx_track::wire::{
    chained_log_body, check_log_body, check_registry_body, log_body, log_posts_page, log_rows_page,
    registry_body, LogExpect, PagedLog,
};
use onyx_track::{check_extends, compute, Close, Fact, MetricRule, Outcome, TrackLog, TrackParams};
use std::path::PathBuf;

/// Anvil's public dev account 3 (unlocked on every anvil).
const POSTER: &str = "90f79bf6eb2c4f870365e785982e1f101e93b906";
/// Any address without code.
const SINK: [u8; 20] = [0xc5; 20];
const METRIC: &str = "example.track.amount";

fn secret(label: &str) -> [u8; 32] {
    keccak256(format!("onyx-track-anvil-test-key:{label}").as_bytes()).0
}

fn addr(label: &str) -> [u8; 20] {
    secret_address(&secret(label)).expect("test key")
}

fn key(source: &str, label: &str, revoked: bool) -> TrackKey {
    TrackKey {
        source: SourceId(source.into()),
        signer: addr(label),
        revoked,
    }
}

/// The level-3 registry's reviewed keys of the two example sources (the
/// provenance is an example review record).
fn reviewed() -> Vec<SourceKey> {
    [
        ("example.vendor-a", "a1"),
        ("example.vendor-a", "a2"),
        ("example.vendor-b", "b1"),
    ]
    .iter()
    .map(|(source, label)| SourceKey {
        source: SourceId((*source).into()),
        signer: addr(label),
        provenance: KeyProvenance::WellKnown {
            origin: format!("{}.example.org", source.replace('.', "-")),
            document_hash: keccak256(label.as_bytes()),
            fetched_unix: 1,
        },
    })
    .collect()
}

fn book(mut keys: Vec<TrackKey>) -> KeyBook {
    keys.sort_by_key(TrackKey::id);
    KeyBook { keys }
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .expect("metric");
    for a in [
        "example.one",
        "example.two",
        "example.three",
        "example.four",
    ] {
        r.add_aggregate(a).expect("aggregate");
    }
    for s in ["example.vendor-a", "example.vendor-b"] {
        r.add_source(DataRights::public_chain(SourceId(s.into())))
            .expect("source");
    }
    r
}

fn fact(subject: &str, as_of: u64) -> Fact {
    Fact {
        metric: MetricId(METRIC.into()),
        subject: Subject::Aggregate(subject.into()),
        as_of,
    }
}

fn sign(intake: Hash32, source: &str, label: &str, f: &Fact, value: u128) -> SignedStatement {
    let payload = StatementPayload {
        intake,
        key: key_id(&SourceId(source.into()), &addr(label)),
        source: SourceId(source.into()),
        fact: f.clone(),
        value: Value::UInt(value),
        confidence: Confidence(800_000),
    };
    let signature = eip191_sign_with(&secret(label), payload.message().as_bytes()).expect("sign");
    SignedStatement { payload, signature }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn check(
    i: &Intake,
    rec: &CastRecord,
    params: &TrackParams,
    reg: &StaticRegistry,
    reviewed: &[SourceKey],
    log: &[onyx_track::Statement],
) -> Result<onyx_track::intake::Anchor, String> {
    // The client's own reads of the chain.
    let head = rec.head()?;
    let posts = rec.posts(&head)?;
    let batches = i.revealed_at(&head);
    let v = IntakeView {
        intake: i.id(),
        head: &head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed,
        lives: None,
    };
    verify_log(&v, params, reg, head.timestamp, log).map_err(|e| e.to_string())?;
    Ok(head)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let rpc = arg(&args, "--rpc").ok_or("--rpc URL")?;
    let cast = PathBuf::from(arg(&args, "--cast").unwrap_or_else(|| "cast".into()));
    if !is_loopback_rpc(&rpc) {
        return Err("local anvil only: the RPC must be a loopback URL".into());
    }
    let work = std::env::temp_dir();
    let mut poster = [0u8; 20];
    hex::decode_to_slice(POSTER, &mut poster).map_err(|e| e.to_string())?;
    let mut rec = CastRecord {
        cast,
        rpc,
        work,
        id: IntakeId {
            chain_id: 31_337,
            sink: SINK,
            poster,
            first_nonce: 0,
        },
        finality: Finality::Depth(2),
        txs: Vec::new(),
    };
    let chain = rec.run(&["chain-id".into()])?;
    if chain.trim() != "31337" {
        return Err(format!("not a local anvil (chain id {chain})"));
    }
    let latest = rec.block("latest")?;
    rec.id.first_nonce = rec.nonce_at(latest.block)?;
    let mine = |rec: &CastRecord, n: u32| -> Result<(), String> {
        for _ in 0..n {
            rec.rpc("evm_mine", &[])?;
        }
        Ok(())
    };
    let warp = |rec: &CastRecord, secs: u64| -> Result<(), String> {
        rec.rpc("evm_increaseTime", &[secs.to_string()])?;
        rec.rpc("evm_mine", &[])?;
        Ok(())
    };

    let r = reg();
    let p = TrackParams::new(vec![MetricRule {
        metric: MetricId(METRIC.into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }])
    .with_intake(rec.id.id());
    let rv = reviewed();
    let (va, vb) = ("example.vendor-a", "example.vendor-b");
    let book1 = book(vec![key(va, "a1", false), key(vb, "b1", false)]);
    let mut i = Intake::new(rec.id.id(), book1, &r, &rv).map_err(|e| e.to_string())?;

    // 1. Two sources state on facts that close in one and two hours of chain time.
    let now = rec.head()?.timestamp;
    let (f1, f2) = (
        fact("example.one", now + 3_600),
        fact("example.two", now + 7_200),
    );
    for s in [
        sign(i.id(), va, "a1", &f1, 100),
        sign(i.id(), vb, "b1", &f1, 120),
        sign(i.id(), va, "a1", &f2, 5),
    ] {
        i.submit(&p, &r, s, now).map_err(|e| e.to_string())?;
    }
    // One close per batch: f1's two statements, then f2's.
    let h0 = i.seal().ok_or("nothing sealed")?;
    let tx0 = rec.post(&h0.hash())?;
    while let Some(h) = i.seal() {
        rec.post(&h.hash())?;
    }
    mine(&rec, 3)?;
    let head = i.sync(&rec).map_err(|e| e.to_string())?;
    let receipt = rec.rpc(
        "eth_getTransactionReceipt",
        &[format!("\"0x{}\"", hex::encode(tx0.0))],
    )?;
    let post_block = onyx_coordinator::track_chain::parse_block(&rec.rpc(
        "eth_getBlockByNumber",
        &[
            format!("\"{}\"", receipt["blockNumber"].as_str().ok_or("receipt")?),
            "false".into(),
        ],
    )?)?;
    println!(
        "batch 0 (2 statements closing at {}): header {} posted in block {} at {}",
        h0.reveal_by,
        h0.hash().to_hex(),
        post_block.block,
        post_block.timestamp
    );
    // Sealed: before the closes the log is empty, and the client agrees.
    let sealed_log = i.log_at(&head);
    if !sealed_log.is_empty() {
        return Err("statements revealed before their close".into());
    }
    let head1 = check(&i, &rec, &p, &r, &rv, &sealed_log)?;
    let book1 = compute(&p, &r, &sealed_log, &[], head1.timestamp).map_err(|e| e.to_string())?;
    let c1 = book1.commitment(Hash32::ZERO);
    println!(
        "commitment 1 at chain time {}: 0 statements revealed (sealed until close)",
        head1.timestamp
    );

    // 2. Rotation: a1 revoked, a2 in. a1 signs nothing new; a replay is refused.
    let book2 = book(vec![
        key(va, "a1", true),
        key(va, "a2", false),
        key(vb, "b1", false),
    ]);
    i.set_keys(book2, &r, &rv).map_err(|e| e.to_string())?;
    let f3 = fact("example.three", now + 7_200);
    let t = rec.head()?.timestamp;
    match i.submit(&p, &r, sign(i.id(), va, "a1", &f3, 7), t) {
        Err(IntakeError::KeyRevoked) => println!("revoked key a1: refused"),
        other => return Err(format!("revoked key admitted: {other:?}")),
    }
    match i.submit(&p, &r, sign(i.id(), vb, "b1", &f1, 120), t) {
        Err(IntakeError::Replay) => println!("replayed statement: refused"),
        other => return Err(format!("replay admitted: {other:?}")),
    }
    i.submit(&p, &r, sign(i.id(), va, "a2", &f3, 7), t)
        .map_err(|e| e.to_string())?;
    // A statement on a fact closing in 60 s, sealed before the close and
    // posted after it (the chain clock moves on): judged Late.
    let f4 = fact("example.four", t + 60);
    i.submit(&p, &r, sign(i.id(), vb, "b1", &f4, 1), t)
        .map_err(|e| e.to_string())?;
    warp(&rec, 120)?;
    match i.submit(
        &p,
        &r,
        sign(i.id(), va, "a2", &f4, 1),
        rec.head()?.timestamp + 120,
    ) {
        Err(IntakeError::PastClose) => println!("statement after its close: refused"),
        other => return Err(format!("past-close statement admitted: {other:?}")),
    }
    while let Some(h) = i.seal() {
        rec.post(&h.hash())?;
    }
    mine(&rec, 3)?;
    i.sync(&rec).map_err(|e| e.to_string())?;

    // 3. Past every close: everything revealed, committed at its block's time.
    warp(&rec, 3 * 3_600)?;
    mine(&rec, 3)?;
    let head = i.sync(&rec).map_err(|e| e.to_string())?;
    let log = i.log_at(&head);
    let head2 = check(&i, &rec, &p, &r, &rv, &log)?;
    if log.len() != 5 {
        return Err(format!("expected 5 revealed statements, got {}", log.len()));
    }
    let batch0: Vec<_> = log.iter().filter(|s| s.fact == f1).collect();
    if batch0.len() != 2
        || batch0
            .iter()
            .any(|s| s.committed_at != post_block.timestamp)
    {
        return Err("committed_at is not the post's block time".into());
    }
    let book_out = compute(&p, &r, &log, &[], head2.timestamp).map_err(|e| e.to_string())?;
    let late: Vec<_> = book_out
        .entries()
        .iter()
        .filter(|e| e.outcome == Outcome::Late)
        .collect();
    if late.len() != 1 || late[0].fact != f4 {
        return Err("the statement posted after its close is not Late".into());
    }
    let c2 = book_out.commitment(c1.hash());
    let empty: [onyx_track::Statement; 0] = [];
    check_extends(
        &c1,
        TrackLog {
            params: &p,
            statements: &empty,
            resolutions: &[],
        },
        &c2,
        TrackLog {
            params: &p,
            statements: &log,
            resolutions: &[],
        },
    )
    .map_err(|e| e.to_string())?;
    println!(
        "commitment 2 at chain time {}: 5 statements revealed, 1 Late, extends commitment 1",
        head2.timestamp
    );

    // ROADMAP C20: the two commitments as the coordinator publishes them, each
    // with the intake at its head; written where a node reads them, read
    // back, served as `GET /v1/track/log` serves them, and checked by a
    // client with its own registry and reviewed keys.
    let mut pubn = Publication::new();
    pubn.push(&p, &sealed_log, &[], &book1, Some(i.published(&head1)))
        .map_err(|e| e.to_string())?;
    let c2b = pubn
        .push(&p, &log, &[], &book_out, Some(i.published(&head2)))
        .map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join(format!("onyx-track-intake-anvil-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("publication.borsh");
    write_publication(&path, &pubn)?;
    let back: Publication = borsh::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let body = log_body(&back, 0).ok_or("no log body")?;
    let e = LogExpect {
        known: Some(c1.hash()),
        registry: Some(&r),
        reviewed: Some(&rv),
        ..Default::default()
    };
    let got = check_log_body(&body, &e).map_err(|e| e.to_string())?;
    if got.commitment != c2b
        || c2b != c2
        || !got.recomputed
        || !got.intake_checked
        || got.links != 2
    {
        return Err("published logs did not rerun to commitment 2".into());
    }
    println!(
        "publication: 2 commitments written and read back; logs rerun to commitment 2, intake checked, extends commitment 1"
    );

    // ROADMAP C21/C23: the same two computations as chained logs carrying
    // their registry, written, read back and checked page by page by a client
    // that rebuilds the served registry and checks its root first.
    let mut chained = Publication::chained();
    let k1 = chained
        .push_with(
            &p,
            &sealed_log,
            &[],
            &book1,
            Some(i.published(&head1)),
            Some(&r),
        )
        .map_err(|e| e.to_string())?;
    let k2 = chained
        .push_with(
            &p,
            &log,
            &[],
            &book_out,
            Some(i.published(&head2)),
            Some(&r),
        )
        .map_err(|e| e.to_string())?;
    let path2 = dir.join("chained.borsh");
    write_publication(&path2, &chained)?;
    let back: Publication = borsh::from_slice(&std::fs::read(&path2).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let reg_served = check_registry_body(
        &registry_body(&back).ok_or("no registry body")?,
        &k2,
        &onyx_core::NoRules,
    )
    .map_err(|e| e.to_string())?;
    let e2 = LogExpect {
        known: Some(k1.hash()),
        registry: Some(&reg_served),
        reviewed: Some(&rv),
        ..Default::default()
    };
    let mut paged = PagedLog::start(&chained_log_body(&back, 0).ok_or("no head")?, &e2)
        .map_err(|e| e.to_string())?;
    while let Some(pos) = paged.next_rows() {
        paged
            .take_rows(&log_rows_page(&back, 0, pos).ok_or("no rows page")?)
            .map_err(|e| e.to_string())?;
    }
    while let Some(n) = paged.next_posts() {
        paged
            .take_posts(&log_posts_page(&back, n).ok_or("no posts page")?)
            .map_err(|e| e.to_string())?;
    }
    let got = paged.finish(&e2).map_err(|e| e.to_string())?;
    if got.commitment != k2 || !got.recomputed || !got.intake_checked || got.links != 2 {
        return Err("chained logs did not rerun to their commitment 2".into());
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "chained publication: written and read back; paged logs rerun with the served registry (root checked), intake checked"
    );

    // 4. A backdated log fails the client's check.
    let mut back = log.clone();
    back[0].committed_at -= 1;
    match check(&i, &rec, &p, &r, &rv, &back) {
        Err(e) if e.contains("log differs") => println!("backdated log: refused"),
        other => return Err(format!("backdated log accepted: {other:?}")),
    }

    // 5. A hidden post (a hedge from the poster the intake never shows)
    // fails every read of the record.
    rec.run(&[
        "send".into(),
        "--unlocked".into(),
        "--from".into(),
        format!("0x{POSTER}"),
        format!("0x{}", hex::encode(SINK)),
        format!("0x{}", hex::encode(keccak256(b"a hedge").0)),
    ])?;
    mine(&rec, 3)?;
    match i.sync(&rec) {
        Err(IntakeError::Record(e)) if e.contains("hidden post") => {
            println!("hidden post: refused ({e})")
        }
        other => return Err(format!("hidden post not caught: {other:?}")),
    }
    println!("TRACK INTAKE ANVIL PASSED");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("track-intake-anvil: {e}");
        std::process::exit(1);
    }
}
