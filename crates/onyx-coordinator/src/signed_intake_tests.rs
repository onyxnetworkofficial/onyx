//! ROADMAP C10: a coordinator posts every report it takes and runs its store
//! on the chain's clock (THREATS L3-F7, L3C-4, C10-2, C10-3, C10-4). The
//! chain here is a model: a second `SignedSources` standing for the book
//! (the differential traces show the two agree), with a clock, a censor and
//! a broken transport.

use super::*;
use crate::signed::SignedSources;
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    Claim, DataRights, EntityKind, EvidenceRef, KeyProvenance, MetricId, MetricInfo,
    Redistribution, SignedPayload, Snapshot, SourceId, SourceKey, SourceRef, StaticRegistry,
    Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

const SOURCE: &str = "example.signed";
const ORIGIN: &str = "data.example.org";
const METRIC: &str = "example.signed.reserve";
const T0: u64 = 1_800_000_000;

fn sk(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key")
}

fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}

fn sign(label: &str, p: SignedPayload) -> SignedReport {
    let digest = Message::from_digest(eip191_digest(p.message().as_bytes()).0);
    let (rid, bytes) = Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, &sk(label))
        .serialize_compact();
    let mut signature = [0u8; 65];
    signature[..64].copy_from_slice(&bytes);
    signature[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
    SignedReport {
        payload: p,
        signature,
    }
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: None,
        terms: String::new(),
    }
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
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .expect("registry");
    r.add_source(rights()).expect("source");
    r
}

fn source_key(label: &str) -> SourceKey {
    SourceKey {
        source: SourceId(SOURCE.into()),
        signer: address(&sk(label)),
        provenance: KeyProvenance::WellKnown {
            origin: ORIGIN.into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: 1,
        },
    }
}

fn claim_for(r: &SignedReport) -> Claim {
    let h = r.hash();
    Claim {
        metric: r.payload.metric.clone(),
        subject: r.payload.subject.clone(),
        value: r.payload.value.clone(),
        source: SourceRef {
            source: r.payload.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: r.payload.observed_unix,
                content_hash: h,
            },
        },
        method: VerificationMethod::SignedSource {
            key: r.payload.key,
            report: h,
        },
        label_set: None,
        rights: rights(),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}

fn payload(key: Hash32, value: u128, observed: u64, sequence: u64) -> SignedPayload {
    SignedPayload {
        source: SourceId(SOURCE.into()),
        key,
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(value),
        observed_unix: observed,
        sequence,
    }
}

/// Two stores with the same three keys: (coordinator's, chain model's), ids.
fn stores() -> (SignedSources, SignedSources, Vec<Hash32>) {
    let r = reg();
    let mut out = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let (mut s, admin) = SignedSources::with_reviewer("reviewer");
        ids = ["K0", "K1", "K2"]
            .iter()
            .map(|l| {
                s.register_key(&admin, source_key(l), &r, T0, "setup")
                    .expect("register")
            })
            .collect();
        out.push(s);
    }
    let b = out.pop().expect("two");
    let a = out.pop().expect("two");
    (a, b, ids)
}

/// The book, modelled: block time, a censored set, a broken transport.
struct Model {
    book: RefCell<SignedSources>,
    time: Cell<u64>,
    block: Cell<u64>,
    censored: RefCell<BTreeSet<Hash32>>,
    down: Cell<bool>,
    posts: Cell<u32>,
    /// Digests the book recorded (accepted, equivocation or evidence).
    recorded: RefCell<BTreeSet<Hash32>>,
}

impl Model {
    fn new(book: SignedSources) -> Self {
        Model {
            book: RefCell::new(book),
            time: Cell::new(T0 + 10),
            block: Cell::new(100),
            censored: RefCell::new(BTreeSet::new()),
            down: Cell::new(false),
            posts: Cell::new(0),
            recorded: RefCell::new(BTreeSet::new()),
        }
    }
}

fn book_error(e: &SignedError) -> &'static str {
    match e {
        SignedError::Replay => "Replay",
        SignedError::AlreadyStated => "AlreadyStated",
        SignedError::FromFuture => "FromFuture",
        SignedError::Stale => "Stale",
        SignedError::KeySuspended => "KeySuspended",
        SignedError::UnknownKey => "UnknownKey",
        SignedError::BadSignature => "BadSignature",
        _ => "BadPayload",
    }
}

impl SignedBookChain for &Model {
    fn latest(&self) -> Result<(u64, u64), String> {
        if self.down.get() {
            return Err("connection refused".into());
        }
        Ok((self.block.get(), self.time.get()))
    }

    fn post(&self, report: &SignedReport) -> Result<Landed, PostError> {
        self.posts.set(self.posts.get() + 1);
        if self.down.get() || self.censored.borrow().contains(&report.payload.digest()) {
            return Err(PostError::Unavailable("transaction not mined".into()));
        }
        self.block.set(self.block.get() + 1);
        let r = self.book.borrow_mut().accept_claim(
            &claim_for(report),
            report,
            &reg(),
            self.time.get(),
        );
        let outcome = match r {
            Ok(_) => PostOutcome::Accepted,
            Err(SignedError::Equivocation { .. }) => PostOutcome::Equivocation,
            Err(SignedError::FactDisputed) => PostOutcome::FactDisputed,
            Err(e) => return Err(PostError::Refused(book_error(&e).into())),
        };
        self.recorded.borrow_mut().insert(report.payload.digest());
        Ok(Landed {
            outcome,
            block: self.block.get(),
            block_time: self.time.get(),
        })
    }

    fn recorded(&self, digest: &Hash32) -> Result<bool, String> {
        if self.down.get() {
            return Err("connection refused".into());
        }
        Ok(self.recorded.borrow().contains(digest))
    }
}

const HOST: u64 = T0 + 10;

#[test]
fn every_report_taken_is_posted_whatever_the_store_says() {
    // L3-F7 / C10-2: coordinator A already accepted X locally; its store
    // calls Y (same fact, other value) an equivocation, but Y must reach the
    // chain all the same, and the chain is where the fact becomes disputed.
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    let y = sign("K1", payload(ids[1], 2, T0, 1));
    let t = c.take(&claim_for(&x), &x, &reg(), HOST).expect("take x");
    assert!(matches!(
        t.chain,
        PostStatus::Recorded {
            outcome: Some(PostOutcome::Accepted),
            ..
        }
    ));
    assert!(t.local.is_ok());
    let t = c.take(&claim_for(&y), &y, &reg(), HOST).expect("take y");
    assert!(matches!(t.local, Err(SignedError::Equivocation { .. })));
    assert!(
        matches!(
            t.chain,
            PostStatus::Recorded {
                outcome: Some(PostOutcome::Equivocation),
                ..
            }
        ),
        "the contradiction reached the book: {:?}",
        t.chain
    );
    assert_eq!(m.posts.get(), 2, "both posted");
    assert_eq!(c.log.for_fact(&signed_fact_key(&x.payload)).len(), 2);
}

#[test]
fn a_report_the_store_refuses_is_still_posted_and_the_book_decides() {
    // The store refuses a claim that does not match (a wrong claim value);
    // the report itself is validly signed, so it goes to the book (L3C-1:
    // anyone may post it; the sequence is burned either way).
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    let mut claim = claim_for(&x);
    claim.value = Value::UInt(99);
    let t = c.take(&claim, &x, &reg(), HOST).expect("take");
    assert!(t.local.is_err(), "the store refuses the claim");
    assert!(matches!(t.chain, PostStatus::Recorded { .. }));
}

#[test]
fn the_store_runs_on_the_latest_block_time_never_back() {
    // L3R-F8, L3C-4: `now` is the latest block's timestamp; a lagging RPC
    // (an older latest block) never steps the store's clock back.
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    m.time.set(T0 + 100);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0 + 50, 1));
    let t = c.take(&claim_for(&x), &x, &reg(), T0 + 100).expect("take");
    assert_eq!(t.now, T0 + 100);
    assert_eq!(c.store.clock(), T0 + 100);
    m.time.set(T0 + 90); // a load-balanced RPC a few blocks behind
    let z = sign("K2", payload(ids[2], 7, T0 + 60, 1));
    let t = c.take(&claim_for(&z), &z, &reg(), T0 + 100).expect("take");
    assert_eq!(t.now, T0 + 100, "never before the store's clock");
    assert!(t.local.is_ok(), "{:?}", t.local);
}

#[test]
fn c10_4_a_chain_clock_far_from_the_host_is_refused_and_nothing_changes() {
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    // A lying RPC: block time an hour ahead (beyond drift plus skew).
    m.time.set(HOST + MAX_CHAIN_AHEAD_SECS + 1);
    let e = c.take(&claim_for(&x), &x, &reg(), HOST).expect_err("ahead");
    assert!(matches!(e, TakeError::ChainAhead { .. }), "{e}");
    // Within the sequencer drift is fine (Base may run that far ahead).
    assert_eq!(
        check_chain_clock(HOST, HOST + MAX_SEQUENCER_DRIFT_SECS),
        Ok(())
    );
    // A stale RPC: the latest block ten minutes old.
    m.time.set(HOST - MAX_CHAIN_BEHIND_SECS - 1);
    let e = c
        .take(&claim_for(&x), &x, &reg(), HOST)
        .expect_err("behind");
    assert!(matches!(e, TakeError::ChainBehind { .. }), "{e}");
    // Nothing posted, logged or stored, and the store's clock did not move
    // (the far-future time never wedges it, L3C-4).
    assert_eq!(m.posts.get(), 0);
    assert!(c.log.entries.is_empty());
    assert_eq!(c.store.clock(), T0);
    m.down.set(true);
    assert!(matches!(
        c.take(&claim_for(&x), &x, &reg(), HOST),
        Err(TakeError::Chain(_))
    ));
}

#[test]
fn c10_3_a_post_that_does_not_land_stays_pending_and_is_retried() {
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    m.censored.borrow_mut().insert(x.payload.digest());
    let t = c.take(&claim_for(&x), &x, &reg(), HOST).expect("take");
    assert!(matches!(t.chain, PostStatus::Pending { since, .. } if since == HOST));
    assert!(t.local.is_ok(), "the pre-check still runs");
    assert_eq!(c.log.oldest_pending(), Some(HOST));
    // Still censored: still pending, the first time kept.
    assert_eq!(c.retry_pending(HOST + 30), Ok(1));
    assert_eq!(c.log.oldest_pending(), Some(HOST));
    // The censor lets go: the retry lands it.
    m.censored.borrow_mut().clear();
    assert_eq!(c.retry_pending(HOST + 60), Ok(0));
    assert!(matches!(
        c.log.entry(&x.payload.digest()).expect("entry").status,
        PostStatus::Recorded { .. }
    ));
    assert_eq!(c.log.oldest_pending(), None);
}

#[test]
fn a_replay_found_recorded_counts_as_landed() {
    // Another coordinator (or an earlier try whose answer was lost) posted
    // the payload first: the book says Replay, and the payload is in it.
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    let (other, _, _) = stores();
    let mut first = BookIntake::new(&m, other, 31_337, [9; 20]);
    first.take(&claim_for(&x), &x, &reg(), HOST).expect("first");
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let t = c.take(&claim_for(&x), &x, &reg(), HOST).expect("second");
    assert_eq!(
        t.chain,
        PostStatus::Recorded {
            outcome: None,
            block: 0,
            block_time: 0
        }
    );
    // A payload refused for its time is posted again later (FromFuture).
    let ahead = sign("K2", payload(ids[2], 3, HOST + 400, 1));
    let t = c
        .take(&claim_for(&ahead), &ahead, &reg(), HOST)
        .expect("take");
    assert_eq!(
        t.chain,
        PostStatus::Refused {
            error: "FromFuture".into()
        }
    );
    m.time.set(HOST + 200);
    let t = c
        .take(&claim_for(&ahead), &ahead, &reg(), HOST + 200)
        .expect("again");
    assert!(
        matches!(t.chain, PostStatus::Recorded { .. }),
        "{:?}",
        t.chain
    );
}

#[test]
fn the_post_log_round_trips_and_refuses_bad_files() {
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 84_532, [9; 20]);
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    let y = sign("K1", payload(ids[1], 2, T0, 1));
    c.take(&claim_for(&x), &x, &reg(), HOST).expect("x");
    m.censored.borrow_mut().insert(y.payload.digest());
    c.take(&claim_for(&y), &y, &reg(), HOST).expect("y");
    let v = c.log.to_json();
    let back = PostLog::from_json(&v).expect("parse");
    assert_eq!(back, c.log);
    let mut dup = v.clone();
    let first = dup["reports"][0].clone();
    dup["reports"].as_array_mut().expect("list").push(first);
    assert!(PostLog::from_json(&dup).is_err(), "duplicate payload");
    let mut bad = v.clone();
    bad["reports"][0]["chain"]["status"] = json!("landed");
    assert!(PostLog::from_json(&bad).is_err(), "unknown status");
    let mut bad = v.clone();
    bad["version"] = json!(2);
    assert!(PostLog::from_json(&bad).is_err(), "version");
    let mut bad = v;
    bad["reports"][0]["payload"] = json!("0x00");
    assert!(PostLog::from_json(&bad).is_err(), "payload");
    let dir = std::env::temp_dir().join(format!("onyx-post-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("posts.json");
    c.log.write(&path).expect("write");
    let read: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    assert_eq!(PostLog::from_json(&read).expect("parse"), c.log);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn book_errors_are_read_from_revert_data_only() {
    let replay = format!(
        "server returned an error response: error code 3: execution reverted, data: \"0x{}\"",
        hex::encode(error_selector("Replay"))
    );
    assert_eq!(book_error_in(&replay), Some("Replay"));
    assert_eq!(book_error_in("timed out waiting for receipt"), None);
    assert_eq!(
        book_error_in("Replay"),
        None,
        "a name alone is not revert data"
    );
    // Every name is the contract's: selectors are distinct.
    let mut s: Vec<[u8; 4]> = BOOK_ERRORS.iter().map(|n| error_selector(n)).collect();
    s.sort_unstable();
    s.dedup();
    assert_eq!(s.len(), BOOK_ERRORS.len());
}

#[test]
fn c10_r1_1_a_refusal_that_may_clear_is_retried() {
    // Review C10-R1-1: Y, signed for an observation time ahead of the chain,
    // is refused FromFuture; once the chain's time catches up, the retry
    // lands it without anyone taking it again. A final refusal is not retried.
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let y = sign("K1", payload(ids[1], 2, HOST + 400, 1));
    let t = c.take(&claim_for(&y), &y, &reg(), HOST).expect("take");
    assert_eq!(
        t.chain,
        PostStatus::Refused {
            error: "FromFuture".into()
        }
    );
    assert!(retryable("FromFuture") && !retryable("BadSignature") && !retryable("Stale"));
    assert_eq!(c.retry_pending(HOST), Ok(0), "still refused, not pending");
    m.time.set(HOST + 200);
    assert_eq!(c.retry_pending(HOST + 200), Ok(0));
    assert!(matches!(
        c.log.entry(&y.payload.digest()).expect("entry").status,
        PostStatus::Recorded { .. }
    ));
    let posts = m.posts.get();
    // A stale report (final refusal) is never posted again by a retry.
    m.time.set(HOST + 8 * 86_400);
    let old = sign("K2", payload(ids[2], 3, HOST, 1));
    let t = c
        .take(&claim_for(&old), &old, &reg(), HOST + 8 * 86_400)
        .expect("take");
    assert_eq!(
        t.chain,
        PostStatus::Refused {
            error: "Stale".into()
        }
    );
    let after_take = m.posts.get();
    assert_eq!(after_take, posts + 1);
    c.retry_pending(HOST + 8 * 86_400).expect("retry");
    assert_eq!(m.posts.get(), after_take, "Stale is final");
}

#[test]
fn c10_r2_1_an_oversized_report_is_refused_before_any_post() {
    // The largest valid shape is far under the bound: 128-byte dotted ids
    // and a MAX_VALUE_BYTES value, so no valid report or contradiction of
    // a valid fact is ever refused by it.
    let dotted = format!("{}.{}", "a".repeat(63), "b".repeat(64));
    assert_eq!(dotted.len(), onyx_core::ids::MAX_DOTTED_LEN);
    let biggest = SignedPayload {
        source: SourceId(dotted.clone()),
        key: Hash32([1; 32]),
        metric: MetricId(dotted.clone()),
        subject: Subject::Aggregate(dotted),
        value: Value::Bytes(vec![0xAB; onyx_core::value::MAX_VALUE_BYTES]),
        observed_unix: T0,
        sequence: u64::MAX,
    };
    assert!(biggest.validate().is_ok());
    let n = borsh::to_vec(&biggest).expect("borsh").len();
    assert!(n * 2 < MAX_POST_PAYLOAD_BYTES, "{n}");
    // A key holder signs a payload too large to send: it would pass the
    // gas estimate, stay pending for good and stop every level-3 answer
    // (the API's breaker). Refused first: nothing posted, logged or stored.
    let (mine, book, ids) = stores();
    let m = Model::new(book);
    let mut c = BookIntake::new(&m, mine, 31_337, [9; 20]);
    let mut p = payload(ids[0], 1, T0, 1);
    p.subject = Subject::Entity(format!("entity:{}", "x".repeat(MAX_POST_PAYLOAD_BYTES)));
    let big = sign("K0", p);
    let e = c
        .take(&claim_for(&big), &big, &reg(), HOST)
        .expect_err("oversized");
    assert!(matches!(e, TakeError::Oversized { .. }), "{e}");
    assert_eq!(m.posts.get(), 0);
    assert!(c.log.entries.is_empty());
    assert_eq!(c.log.oldest_pending(), None);
    // A normal report still goes through.
    let x = sign("K0", payload(ids[0], 1, T0, 1));
    let t = c.take(&claim_for(&x), &x, &reg(), HOST).expect("take");
    assert!(matches!(t.chain, PostStatus::Recorded { .. }));
}
