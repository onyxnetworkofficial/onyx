//! The intake rules of ADR-0036 amendment 1 (R-TI-*), one test per rule,
//! plus the adversarial cases in THREATS.md (TR-I-*).

use super::*;
use crate::{
    check_extends, compute, sort_resolutions, Close, MetricRule, Outcome, Resolution, TrackLog,
    TrackParams,
};
use onyx_core::{
    DataRights, KeyProvenance, MetricId, MetricInfo, ProofLevel, SourceKey, StaticRegistry,
    Subject, ValueKind,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};

const DAY: u64 = 86_400;
const T0: u64 = 1_790_000_000;
const ID: IntakeId = IntakeId {
    chain_id: 31_337,
    sink: [0x5e; 20],
    poster: [0x90; 20],
    first_nonce: 0,
};

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let m = |value_kind| MetricInfo {
        value_kind,
        needs_label_set: false,
    };
    r.add_metric("test.amount", m(ValueKind::UInt))
        .and_then(|r| r.add_metric("test.flag", m(ValueKind::Bool)))
        .expect("metrics");
    for a in ["a.one", "a.two", "a.three", "a.four", "a.five", "a.six"] {
        r.add_aggregate(a).expect("aggregate");
    }
    for s in ["vendor.a", "vendor.b", "chain.test"] {
        r.add_source(DataRights::public_chain(SourceId(s.into())))
            .expect("source");
    }
    r
}

fn params() -> TrackParams {
    let rule = |m: &str| MetricRule {
        metric: MetricId(m.into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    };
    TrackParams::new(vec![rule("test.amount"), rule("test.flag")]).with_intake(ID.id())
}

fn secret(name: &str) -> [u8; 32] {
    keccak256(name.as_bytes()).0
}

fn addr(name: &str) -> [u8; 20] {
    secret_address(&secret(name)).expect("key")
}

fn key(source: &str, name: &str, revoked: bool) -> TrackKey {
    TrackKey {
        source: SourceId(source.into()),
        signer: addr(name),
        revoked,
    }
}

fn book(mut keys: Vec<TrackKey>) -> KeyBook {
    keys.sort_by_key(TrackKey::id);
    KeyBook { keys }
}

/// The level-3 registry's reviewed keys: every key the tests register.
fn reviewed() -> &'static [SourceKey] {
    static KEYS: std::sync::OnceLock<Vec<SourceKey>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        [("vendor.a", "a1"), ("vendor.a", "a2"), ("vendor.b", "b1")]
            .iter()
            .map(|(source, name)| SourceKey {
                source: SourceId((*source).into()),
                signer: addr(name),
                provenance: KeyProvenance::WellKnown {
                    origin: format!("{}.example", source.replace('.', "-")),
                    document_hash: keccak256(name.as_bytes()),
                    fetched_unix: T0,
                },
            })
            .collect()
    })
}

fn base_book() -> KeyBook {
    book(vec![
        key("vendor.a", "a1", false),
        key("vendor.b", "b1", false),
    ])
}

/// A fact whose statements close at its `as_of` (rules have offset 0).
fn fact(subject: &str, day: u64) -> Fact {
    Fact {
        metric: MetricId("test.amount".into()),
        subject: Subject::Aggregate(subject.into()),
        as_of: T0 + day * DAY,
    }
}

/// `source` states `value` on `f`, signed by key `name` (registered or not).
fn signed_by(source: &str, name: &str, f: &Fact, value: u128) -> SignedStatement {
    let payload = StatementPayload {
        intake: ID.id(),
        key: key_id(&SourceId(source.into()), &addr(name)),
        source: SourceId(source.into()),
        fact: f.clone(),
        value: Value::UInt(value),
        confidence: Confidence(900_000),
    };
    let signature = eip191_sign_with(&secret(name), payload.message().as_bytes()).expect("sign");
    SignedStatement { payload, signature }
}

fn signed(source: &str, f: &Fact, value: u128) -> SignedStatement {
    let name = if source == "vendor.a" { "a1" } else { "b1" };
    signed_by(source, name, f, value)
}

fn anchor(block: u64, timestamp: u64) -> Anchor {
    Anchor {
        block,
        block_hash: keccak256(&block.to_be_bytes()),
        timestamp,
    }
}

/// An in-memory finalized record: posts in order and a head.
struct Mem {
    posts: Vec<Post>,
    head: Anchor,
    /// The record counts more posts than it can show (a hidden post).
    hidden: bool,
}

impl Mem {
    fn new() -> Self {
        Mem {
            posts: Vec::new(),
            head: anchor(0, T0),
            hidden: false,
        }
    }

    /// Post `h` in block `block` at `ts`, and move the head there.
    fn land(&mut self, h: &BatchHeader, block: u64, ts: u64) {
        self.posts.push(Post {
            seq: self.posts.len() as u64,
            header_hash: h.hash(),
            anchor: anchor(block, ts),
        });
        self.head = anchor(block, ts);
    }
}

impl FinalizedRecord for Mem {
    fn head(&self) -> Result<Anchor, String> {
        Ok(self.head)
    }

    fn posts(&self, head: &Anchor) -> Result<Vec<Post>, String> {
        if self.hidden {
            return Err("poster nonce counts more posts than shown".into());
        }
        Ok(self
            .posts
            .iter()
            .filter(|p| p.anchor.block <= head.block)
            .copied()
            .collect())
    }
}

fn intake() -> Intake {
    Intake::new(ID.id(), base_book(), &reg(), reviewed()).expect("intake")
}

/// Seal every pending close into its own batch (one close per batch), post
/// them in blocks `block`, `block + 1`, ... at `ts`, `ts + 1`, ..., sync.
/// Returns the first header.
fn post(i: &mut Intake, rec: &mut Mem, block: u64, ts: u64) -> BatchHeader {
    let first = i.seal().expect("something to seal");
    rec.land(&first, block, ts);
    let mut k = 1;
    while let Some(h) = i.seal() {
        rec.land(&h, block + k, ts + k);
        k += 1;
    }
    i.sync(rec).expect("sync");
    first
}

/// A client's check of `log` at the record's head.
fn check(i: &Intake, rec: &Mem, log: &[Statement]) -> Result<(), IntakeError> {
    let posts = rec.posts(&rec.head).map_err(IntakeError::Record)?;
    let batches = i.revealed_at(&rec.head);
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    verify_log(&v, &params(), &reg(), rec.head.timestamp, log)
}

/// A batch built by hand (a dishonest coordinator), with every row revealed.
fn forge(
    seq: u64,
    prev: Hash32,
    keys: &KeyBook,
    rows: &[SignedStatement],
    reveal_by: u64,
) -> RevealedBatch {
    let bytes: Vec<Vec<u8>> = rows.iter().map(SignedStatement::row).collect();
    let tree = MerkleTree::from_rows(&bytes);
    RevealedBatch {
        header: BatchHeader {
            intake: ID.id(),
            seq,
            prev,
            keys: keys.hash(),
            statements_root: tree.root(),
            statement_count: tree.leaf_count(),
            reveal_by,
        },
        statements: rows
            .iter()
            .enumerate()
            .map(|(k, s)| (s.clone(), tree.proof(k as u64).expect("proof")))
            .collect(),
    }
}

/// The track key of (source, signer) is the level-3 key of the same pair, so
/// q-TR-2's "the source's registered key" is one key across both lanes.
#[test]
fn the_key_id_is_the_level3_key_id() {
    let k = key("vendor.a", "a1", false);
    let l3 = SourceKey {
        source: k.source.clone(),
        signer: k.signer,
        provenance: KeyProvenance::WellKnown {
            origin: "data.vendor-a.example".into(),
            document_hash: keccak256(b"doc"),
            fetched_unix: T0,
        },
    };
    assert_eq!(k.id(), l3.id());
}

/// R-TI-1, TR-6 (impersonation): only a signature by an active key of the
/// statement's own source admits it.
#[test]
fn a_statement_enters_only_under_the_sources_registered_key() {
    let mut i = intake();
    let (p, r) = (params(), reg());
    let f = fact("a.one", 3);
    assert!(i.submit(&p, &r, signed("vendor.a", &f, 1), T0).is_ok());

    // vendor.b's key speaking for vendor.a.
    let mut other = signed_by("vendor.b", "b1", &f, 2);
    other.payload.source = SourceId("vendor.a".into());
    other.signature =
        eip191_sign_with(&secret("b1"), other.payload.message().as_bytes()).expect("sign");
    assert_eq!(i.submit(&p, &r, other, T0), Err(IntakeError::KeyNotSource));
    // A key nobody registered.
    assert_eq!(
        i.submit(&p, &r, signed_by("vendor.a", "stranger", &f, 3), T0),
        Err(IntakeError::UnknownKey)
    );
    // The registered key's id, signed by another key (the coordinator's, say).
    let mut forged = signed("vendor.a", &fact("a.two", 3), 4);
    forged.signature =
        eip191_sign_with(&secret("coordinator"), forged.payload.message().as_bytes())
            .expect("sign");
    assert_eq!(i.submit(&p, &r, forged, T0), Err(IntakeError::BadSignature));
    // A value changed after signing.
    let mut edited = signed("vendor.a", &fact("a.three", 3), 5);
    edited.payload.value = Value::UInt(6);
    assert_eq!(i.submit(&p, &r, edited, T0), Err(IntakeError::BadSignature));
    // A non-canonical signature (v = 29).
    let mut odd = signed("vendor.a", &fact("a.four", 3), 7);
    odd.signature[64] = 29;
    assert_eq!(i.submit(&p, &r, odd, T0), Err(IntakeError::BadSignature));
    // A key book naming an unregistered source is refused.
    let unknown = book(vec![key("vendor.z", "z1", false)]);
    assert!(matches!(
        Intake::new(ID.id(), unknown, &r, reviewed()),
        Err(IntakeError::Track(TrackError::BadSource(_)))
    ));
    // Inadmissible statements are refused like in compute (R-TR-1).
    let mut low = signed("vendor.a", &fact("a.five", 3), 8);
    low.payload.confidence = Confidence(400_000);
    low.signature =
        eip191_sign_with(&secret("a1"), low.payload.message().as_bytes()).expect("sign");
    assert_eq!(
        i.submit(&p, &r, low, T0),
        Err(IntakeError::Track(TrackError::BadConfidence))
    );

    // The client checks the same in a published batch: a forged batch with a
    // statement under a stranger's key, or with a stolen signature over
    // another value, is refused.
    let f = fact("a.six", 3);
    let head = anchor(40, T0 + 5 * DAY);
    let books = [base_book()];
    let refused = |row: SignedStatement| {
        let batch = forge(
            0,
            Hash32::ZERO,
            &base_book(),
            std::slice::from_ref(&row),
            f.as_of,
        );
        let posts = [Post {
            seq: 0,
            header_hash: batch.header.hash(),
            anchor: anchor(10, T0 + 100),
        }];
        let batches = [batch];
        let v = IntakeView {
            intake: ID.id(),
            head: &head,
            posts: &posts,
            batches: &batches,
            books: &books,
            reviewed: reviewed(),
            lives: None,
        };
        let log = vec![row.payload.statement(T0 + 100)];
        verify_log(&v, &p, &r, head.timestamp, &log)
    };
    assert_eq!(refused(signed("vendor.a", &f, 1)), Ok(()));
    assert_eq!(
        refused(signed_by("vendor.a", "stranger", &f, 1)),
        Err(IntakeError::UnknownKey)
    );
    let mut stolen = signed("vendor.a", &f, 1);
    stolen.payload.value = Value::UInt(2);
    assert_eq!(refused(stolen), Err(IntakeError::BadSignature));
}

/// R-TI-5, TR-6 (backdating): `committed_at` is the timestamp of the block
/// carrying the batch's post; a log with any other time fails the client
/// check, earlier or later.
#[test]
fn committed_at_is_the_post_block_time_never_the_callers() {
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&params(), &reg(), signed("vendor.a", &f, 1), T0)
        .expect("submit");
    // Not final yet: the head is below the post's block.
    let h = i.seal().expect("seal");
    rec.land(&h, 10, T0 + 100);
    rec.head = anchor(9, T0 + 90);
    assert!(i.log_at(&rec.head).is_empty());
    // Final, and past the close.
    rec.head = anchor(30, T0 + 3 * DAY);
    i.sync(&rec).expect("sync");
    let log = i.log_at(&rec.head);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].committed_at, T0 + 100);
    assert_eq!(check(&i, &rec, &log), Ok(()));
    for at in [T0 + 50, T0 + 101] {
        let mut moved = log.clone();
        moved[0].committed_at = at;
        assert_eq!(check(&i, &rec, &moved), Err(IntakeError::LogMismatch));
    }
    // The commitment's as_of must be the head's time.
    let posts = rec.posts(&rec.head).expect("posts");
    let batches = i.revealed_at(&rec.head);
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &params(), &reg(), rec.head.timestamp + 1, &log),
        Err(IntakeError::NotHead)
    );
}

/// R-TI-3 (late): a statement whose close is at or before the latest
/// finalized time is refused; one sealed before its close but posted after
/// it is committed at the post's time and judged `Late`.
#[test]
fn a_statement_after_the_close_is_refused_or_late() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    let close = f.as_of;
    assert_eq!(
        i.submit(&p, &r, signed("vendor.a", &f, 1), close),
        Err(IntakeError::PastClose)
    );
    i.submit(&p, &r, signed("vendor.a", &f, 1), close - 1)
        .expect("submit");
    post(&mut i, &mut rec, 20, close + 10);
    let log = i.log_at(&rec.head);
    assert_eq!(check(&i, &rec, &log), Ok(()));
    let b = compute(&p, &r, &log, &[], rec.head.timestamp).expect("compute");
    assert_eq!(b.entries()[0].outcome, Outcome::Late);
}

/// R-TI-6, TR-7 (copying): a statement stays sealed until its close. Before
/// it, the published batch carries only the root; a log revealing it early
/// fails the client check; the commitment after the close adds it although
/// it was committed before the earlier commitment (relaxed R-TR-8).
#[test]
fn sealed_statements_stay_hidden_until_their_close() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&p, &r, signed("vendor.a", &f, 1), T0)
        .expect("submit");
    i.submit(&p, &r, signed("vendor.b", &f, 1), T0)
        .expect("submit");
    let h = post(&mut i, &mut rec, 10, T0 + 100);
    assert_eq!(h.reveal_by, f.as_of);

    // Before the close: nothing revealed, the batch shows only its header.
    rec.head = anchor(20, f.as_of - 1);
    let early = i.log_at(&rec.head);
    assert!(early.is_empty());
    assert!(i.revealed_at(&rec.head)[0].statements.is_empty());
    assert_eq!(check(&i, &rec, &early), Ok(()));
    let c1 = compute(&p, &r, &early, &[], rec.head.timestamp)
        .expect("compute")
        .commitment(Hash32::ZERO);

    // Revealing them early (the published batch at the close, shown at an
    // earlier head) fails.
    let at_close = anchor(21, f.as_of);
    let revealed = i.revealed_at(&at_close);
    let posts = rec.posts(&rec.head).expect("posts");
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &revealed,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    let leaked = i.log_at(&at_close);
    assert_eq!(
        verify_log(&v, &p, &r, rec.head.timestamp, &leaked),
        Err(IntakeError::EarlyReveal)
    );

    // At the close: revealed, committed at the post time, and the next
    // commitment extends the first although T0 + 100 <= c1.as_of.
    rec.head = at_close;
    let log = i.log_at(&rec.head);
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|s| s.committed_at == T0 + 100));
    assert_eq!(check(&i, &rec, &log), Ok(()));
    let c2 = compute(&p, &r, &log, &[], rec.head.timestamp)
        .expect("compute")
        .commitment(c1.hash());
    let empty: [Statement; 0] = [];
    let l1 = TrackLog {
        params: &p,
        statements: &empty,
        resolutions: &[],
    };
    let l2 = TrackLog {
        params: &p,
        statements: &log,
        resolutions: &[],
    };
    assert_eq!(check_extends(&c1, l1, &c2, l2), Ok(()));
}

/// R-TI-6 (completeness): past its close a batch must show every statement;
/// a coordinator that withholds one past its close can never add it later
/// either (`Backdated`).
#[test]
fn a_withheld_reveal_is_caught() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let (f, g) = (fact("a.one", 2), fact("a.two", 5));
    i.submit(&p, &r, signed("vendor.a", &f, 1), T0)
        .expect("submit");
    i.submit(&p, &r, signed("vendor.a", &g, 2), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);

    // Between the two closes: f revealed, g still sealed; fine.
    rec.head = anchor(20, T0 + 3 * DAY);
    let log = i.log_at(&rec.head);
    assert_eq!(log.len(), 1);
    assert_eq!(check(&i, &rec, &log), Ok(()));

    // One close per batch: f and g were sealed apart, so withholding f once
    // its own close has passed is refused at once (TR-R3-1).
    let mut batches = i.revealed_at(&rec.head);
    assert_eq!((batches.len(), batches[0].header.reveal_by), (2, f.as_of));
    batches[0].statements.clear();
    let posts = rec.posts(&rec.head).expect("posts");
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, rec.head.timestamp, &[]),
        Err(IntakeError::Incomplete)
    );

    // Withholding f past its close in a commitment (undetectable then, g
    // still sealed) makes every later commitment that shows f refused.
    let t1 = T0 + 3 * DAY;
    let c1 = compute(&p, &r, &[], &[], t1)
        .expect("compute")
        .commitment(Hash32::ZERO);
    let t2 = T0 + 4 * DAY;
    let late = i.log_at(&anchor(25, t2));
    let c2 = compute(&p, &r, &late, &[], t2)
        .expect("compute")
        .commitment(c1.hash());
    let none: [Statement; 0] = [];
    assert_eq!(
        check_extends(
            &c1,
            TrackLog {
                params: &p,
                statements: &none,
                resolutions: &[]
            },
            &c2,
            TrackLog {
                params: &p,
                statements: &late,
                resolutions: &[]
            }
        ),
        Err(crate::ExtendError::Backdated)
    );
}

/// R-TI-8 (every post counts): a post the intake did not seal (a hedge
/// committed and never shown), a record that counts more posts than it
/// shows, posts without batches and broken header chains are refused.
#[test]
fn a_hidden_post_is_caught() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    i.submit(&p, &r, signed("vendor.a", &fact("a.one", 2), 1), T0)
        .expect("submit");
    let h0 = post(&mut i, &mut rec, 10, T0 + 100);

    // A second post the intake never sealed.
    let mut hedge = h0.clone();
    hedge.seq = 1;
    hedge.prev = h0.hash();
    hedge.statements_root = keccak256(b"the other value");
    let mut bad = Mem {
        posts: rec.posts.clone(),
        head: rec.head,
        hidden: false,
    };
    bad.land(&hedge, 11, T0 + 110);
    assert_eq!(i.sync(&bad), Err(IntakeError::HiddenPost));
    // A client given the posts but not that batch.
    let posts = bad.posts(&bad.head).expect("posts");
    let batches = i.revealed_at(&bad.head);
    let v = IntakeView {
        intake: ID.id(),
        head: &bad.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, bad.head.timestamp, &[]),
        Err(IntakeError::HiddenPost)
    );
    // A record whose own count shows more posts (the poster's nonce).
    rec.hidden = true;
    assert!(matches!(i.sync(&rec), Err(IntakeError::Record(_))));
    rec.hidden = false;

    // A second batch whose header does not chain to the first.
    i.submit(&p, &r, signed("vendor.b", &fact("a.two", 2), 1), T0)
        .expect("submit");
    let mut h1 = i.seal().expect("seal");
    h1.prev = Hash32::ZERO;
    rec.land(&h1, 12, T0 + 120);
    assert_eq!(i.sync(&rec), Err(IntakeError::PostMismatch));
    let unchained = forge(1, Hash32::ZERO, &base_book(), &[], 0);
    let mut posts = rec.posts(&rec.head).expect("posts");
    posts[1].header_hash = unchained.header.hash();
    let batches = vec![i.revealed_at(&rec.head)[0].clone(), unchained];
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, rec.head.timestamp, &[]),
        Err(IntakeError::BadChain("header"))
    );
    // Posts out of block order.
    posts[1].anchor = anchor(5, T0 + 120);
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, rec.head.timestamp, &[]),
        Err(IntakeError::BadChain("post order"))
    );
}

/// R-TI-2 (replay): a payload is taken once, pending or sealed, and only by
/// the intake it was signed for; a forged batch carrying it twice fails.
#[test]
fn replayed_statements_are_refused() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let s = signed("vendor.a", &fact("a.one", 2), 1);
    i.submit(&p, &r, s.clone(), T0).expect("submit");
    assert_eq!(i.submit(&p, &r, s.clone(), T0), Err(IntakeError::Replay));
    post(&mut i, &mut rec, 10, T0 + 100);
    assert_eq!(i.submit(&p, &r, s.clone(), T0), Err(IntakeError::Replay));

    // Signed for another deployment.
    let other = IntakeId {
        chain_id: 84_532,
        ..ID
    };
    let mut j = Intake::new(other.id(), base_book(), &r, reviewed()).expect("intake");
    assert_eq!(
        j.submit(&p, &r, s.clone(), T0),
        Err(IntakeError::WrongIntake)
    );

    // A forged batch with the same statement twice.
    let twice = forge(
        0,
        Hash32::ZERO,
        &base_book(),
        &[s.clone(), s.clone()],
        s.payload.fact.as_of,
    );
    let head = anchor(40, T0 + 5 * DAY);
    let posts = [Post {
        seq: 0,
        header_hash: twice.header.hash(),
        anchor: anchor(10, T0 + 100),
    }];
    let books = [base_book()];
    let batches = [twice];
    let v = IntakeView {
        intake: ID.id(),
        head: &head,
        posts: &posts,
        batches: &batches,
        books: &books,
        reviewed: reviewed(),
        lives: None,
    };
    let log = vec![s.payload.statement(T0 + 100)];
    assert_eq!(
        verify_log(&v, &p, &r, head.timestamp, &log),
        Err(IntakeError::Replay)
    );
}

/// R-TI-7, TR-I-4: keys rotate forward. A revoked key signs nothing new and
/// its pending statements are dropped, but what it stated in a final batch
/// stands (no erasing misses by "losing" a key); a book cannot un-revoke or
/// drop a key, nor give a signer to a second source.
#[test]
fn keys_rotate_forward_and_never_retroactively() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 3);
    i.submit(&p, &r, signed_by("vendor.a", "a1", &f, 999), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);

    // Rotation: a1 revoked, a2 added.
    let rotated = book(vec![
        key("vendor.a", "a1", true),
        key("vendor.a", "a2", false),
        key("vendor.b", "b1", false),
    ]);
    assert_eq!(i.set_keys(rotated.clone(), &r, reviewed()), Ok(0));
    let g = fact("a.two", 3);
    assert_eq!(
        i.submit(&p, &r, signed_by("vendor.a", "a1", &g, 1), T0 + 200),
        Err(IntakeError::KeyRevoked)
    );
    i.submit(&p, &r, signed_by("vendor.a", "a2", &g, 1), T0 + 200)
        .expect("new key");
    let h1 = post(&mut i, &mut rec, 20, T0 + 300);
    assert_eq!(h1.keys, rotated.hash());

    // The miss a1 stated before the rotation is still in the log and judged.
    rec.head = anchor(40, T0 + 4 * DAY);
    let log = i.log_at(&rec.head);
    assert_eq!(log.len(), 2);
    assert_eq!(check(&i, &rec, &log), Ok(()));
    let mut res = vec![Resolution {
        fact: f.clone(),
        value: Value::UInt(100),
        level: ProofLevel::Recomputed,
        resolver: SourceId("chain.test".into()),
        evidence: keccak256(b"answer"),
        // Judged once its contest window has passed (amendment 3 item 6).
        public_from: T0 + 4 * DAY - crate::CONTEST_WINDOW_SECS,
    }];
    sort_resolutions(&mut res);
    let b = compute(&p, &r, &log, &res, rec.head.timestamp).expect("compute");
    let rec_a = b
        .record(
            &SourceId("vendor.a".into()),
            &MetricId("test.amount".into()),
        )
        .expect("record")
        .1;
    assert_eq!(rec_a.disagreed, 1);

    // Revoking a key drops its pending statements.
    i.submit(
        &p,
        &r,
        signed_by("vendor.a", "a2", &fact("a.three", 5), 1),
        T0 + 400,
    )
    .expect("submit");
    let revoked_all = book(vec![
        key("vendor.a", "a1", true),
        key("vendor.a", "a2", true),
        key("vendor.b", "b1", false),
    ]);
    assert_eq!(i.set_keys(revoked_all, &r, reviewed()), Ok(1));
    assert!(i.seal().is_none());

    // Never back: un-revoking (every key kept) or dropping a key; one
    // signer, one source.
    let unrevoked = book(vec![
        key("vendor.a", "a1", false),
        key("vendor.a", "a2", true),
        key("vendor.b", "b1", false),
    ]);
    assert_eq!(
        i.set_keys(unrevoked, &r, reviewed()),
        Err(IntakeError::KeysNotForward)
    );
    assert_eq!(
        i.set_keys(base_book(), &r, reviewed()),
        Err(IntakeError::KeysNotForward)
    );
    assert_eq!(
        i.set_keys(book(vec![key("vendor.b", "b1", false)]), &r, reviewed()),
        Err(IntakeError::KeysNotForward)
    );
    let shared = book(vec![
        key("vendor.a", "a1", false),
        key("vendor.b", "a1", false),
    ]);
    assert_eq!(
        shared.validate(&r, reviewed()),
        Err(IntakeError::BadKeys("signer under two sources"))
    );

    // A published book for a later batch that un-revokes a key fails the client.
    let posts = rec.posts(&rec.head).expect("posts");
    let mut batches = i.revealed_at(&rec.head);
    let first = forge(0, Hash32::ZERO, &rotated, &[], 0);
    let second = forge(1, first.header.hash(), &base_book(), &[], 0);
    batches[0] = first;
    batches[1] = second;
    let fake_posts: Vec<Post> = posts
        .iter()
        .zip(&batches)
        .map(|(p, b)| Post {
            header_hash: b.header.hash(),
            ..*p
        })
        .collect();
    let books = [rotated, base_book()];
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &fake_posts,
        batches: &batches,
        books: &books,
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, rec.head.timestamp, &[]),
        Err(IntakeError::KeysNotForward)
    );
}

/// R-TI-3 (floods), TR-I-5: pending statements are capped per source (other
/// sources still get in), and volume cannot dilute a miss: statements on
/// unresolved facts count nothing and a second value on a fact is a
/// confident miss.
#[test]
fn a_flood_is_capped_and_cannot_dilute_a_miss() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    for d in 0..MAX_PENDING_PER_SOURCE as u64 {
        i.submit(&p, &r, signed("vendor.a", &fact("a.one", 10 + d), 1), T0)
            .expect("under the cap");
    }
    assert_eq!(
        i.submit(&p, &r, signed("vendor.a", &fact("a.two", 10), 1), T0),
        Err(IntakeError::Flood)
    );
    assert!(i
        .submit(&p, &r, signed("vendor.b", &fact("a.two", 10), 1), T0)
        .is_ok());

    // Dilution: one miss on a resolved fact, 200 statements on facts that
    // never resolve, a hedge on another resolved fact.
    let mut j = intake();
    let mut rec = Mem::new();
    let (f, g) = (fact("a.one", 2), fact("a.two", 2));
    j.submit(&p, &r, signed("vendor.a", &f, 999), T0)
        .expect("miss");
    for d in 0..200 {
        j.submit(&p, &r, signed("vendor.a", &fact("a.three", 3 + d), 1), T0)
            .expect("filler");
    }
    j.submit(&p, &r, signed("vendor.a", &g, 100), T0)
        .expect("hedge 1");
    j.submit(&p, &r, signed("vendor.a", &g, 101), T0)
        .expect("hedge 2");
    post(&mut j, &mut rec, 10, T0 + 100);
    rec.head = anchor(500, T0 + 300 * DAY);
    let log = j.log_at(&rec.head);
    assert_eq!(check(&j, &rec, &log), Ok(()));
    let answer = |fact: &Fact| Resolution {
        fact: fact.clone(),
        value: Value::UInt(100),
        level: ProofLevel::Recomputed,
        resolver: SourceId("chain.test".into()),
        evidence: keccak256(&borsh::to_vec(fact).expect("borsh")),
        public_from: T0 + 3 * DAY,
    };
    let mut res = vec![answer(&f), answer(&g)];
    sort_resolutions(&mut res);
    let b = compute(&p, &r, &log, &res, rec.head.timestamp).expect("compute");
    let a = b
        .record(
            &SourceId("vendor.a".into()),
            &MetricId("test.amount".into()),
        )
        .expect("record")
        .1;
    assert_eq!((a.agreed, a.disagreed, a.equivocated), (0, 1, 1));
    assert_eq!(a.unresolved, 200);
    assert_eq!(a.agreement_ppm(), Some(0));
    assert!(!a.established());
}

/// Review TR-R3-1: a hedge (two values on one fact) cannot be kept sealed
/// behind a far `reveal_by`: every statement of a batch must have the
/// batch's close, so both values are due at that close and score
/// `Equivocated`.
#[test]
fn a_far_reveal_by_cannot_hide_a_hedge() {
    let (p, r) = (params(), reg());
    let f = fact("a.one", 2);
    let (a, b) = (signed("vendor.a", &f, 100), signed("vendor.a", &f, 101));
    let head = anchor(40, T0 + 5 * DAY);
    let books = [base_book()];
    let verify = |batch: RevealedBatch, log: &[Statement]| {
        let posts = [Post {
            seq: 0,
            header_hash: batch.header.hash(),
            anchor: anchor(10, T0 + 100),
        }];
        let batches = [batch];
        let v = IntakeView {
            intake: ID.id(),
            head: &head,
            posts: &posts,
            batches: &batches,
            books: &books,
            reviewed: reviewed(),
            lives: None,
        };
        verify_log(&v, &p, &r, head.timestamp, log)
    };
    // The forged header: both values sealed, reveal_by far away, only the
    // right one revealed after the truth is out.
    let mut forged = forge(
        0,
        Hash32::ZERO,
        &base_book(),
        &[a.clone(), b.clone()],
        u64::MAX,
    );
    forged.statements.retain(|(s, _)| s == &a);
    assert_eq!(
        verify(forged, &[a.payload.statement(T0 + 100)]),
        Err(IntakeError::BadChain("reveal_by"))
    );
    // The honest seal: a far-future statement goes into its own batch, so it
    // cannot postpone the hedge's reveal.
    let mut i = intake();
    let mut rec = Mem::new();
    i.submit(&p, &r, a.clone(), T0).expect("a");
    i.submit(&p, &r, b, T0).expect("b");
    i.submit(&p, &r, signed("vendor.a", &fact("a.two", 900), 1), T0)
        .expect("far");
    post(&mut i, &mut rec, 10, T0 + 100);
    rec.head = anchor(40, T0 + 5 * DAY);
    let batches = i.revealed_at(&rec.head);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].statements.len(), 2);
    let log = i.log_at(&rec.head);
    assert_eq!(check(&i, &rec, &log), Ok(()));
    let mut res = vec![Resolution {
        fact: f.clone(),
        value: Value::UInt(100),
        level: ProofLevel::Recomputed,
        resolver: SourceId("chain.test".into()),
        evidence: keccak256(b"answer"),
        public_from: T0 + 3 * DAY,
    }];
    sort_resolutions(&mut res);
    let book = compute(&p, &r, &log, &res, rec.head.timestamp).expect("compute");
    assert_eq!(book.entries()[0].outcome, Outcome::Equivocated);
    // Dropping the wrong value from the honest batch is refused too.
    let mut cut = i.revealed_at(&rec.head);
    cut[0].statements.retain(|(s, _)| s == &a);
    let posts = rec.posts(&rec.head).expect("posts");
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &cut,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(
            &v,
            &p,
            &r,
            rec.head.timestamp,
            &[a.payload.statement(T0 + 100)]
        ),
        Err(IntakeError::Incomplete)
    );
}

/// Review TR-R3-2: a key the coordinator adds itself (not a reviewed
/// level-3 key of the source) is refused by the intake and by the client.
#[test]
fn a_key_outside_the_level3_registry_is_refused() {
    let (p, r) = (params(), reg());
    let planted = book(vec![
        key("vendor.a", "a1", false),
        key("vendor.a", "coordinator", false),
        key("vendor.b", "b1", false),
    ]);
    assert_eq!(
        planted.validate(&r, reviewed()),
        Err(IntakeError::BadKeys("not a reviewed level-3 key"))
    );
    let mut i = intake();
    assert_eq!(
        i.set_keys(planted.clone(), &r, reviewed()),
        Err(IntakeError::BadKeys("not a reviewed level-3 key"))
    );
    // A published batch under that book fails the client.
    let f = fact("a.one", 2);
    let s = signed_by("vendor.a", "coordinator", &f, 1);
    let batch = forge(0, Hash32::ZERO, &planted, std::slice::from_ref(&s), f.as_of);
    let head = anchor(40, T0 + 5 * DAY);
    let posts = [Post {
        seq: 0,
        header_hash: batch.header.hash(),
        anchor: anchor(10, T0 + 100),
    }];
    let batches = [batch];
    let books = [planted];
    let v = IntakeView {
        intake: ID.id(),
        head: &head,
        posts: &posts,
        batches: &batches,
        books: &books,
        reviewed: reviewed(),
        lives: None,
    };
    assert_eq!(
        verify_log(&v, &p, &r, head.timestamp, &[s.payload.statement(T0 + 100)]),
        Err(IntakeError::BadKeys("not a reviewed level-3 key"))
    );
}

/// Review TR-R3-3: the params pin the intake, so a commitment chain cannot
/// be checked against (or switch to) another intake's posts, and later
/// params cannot change it.
#[test]
fn the_params_pin_the_intake() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    i.submit(&p, &r, signed("vendor.a", &fact("a.one", 2), 1), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);
    rec.head = anchor(40, T0 + 5 * DAY);
    let log = i.log_at(&rec.head);
    assert_eq!(check(&i, &rec, &log), Ok(()));
    let posts = rec.posts(&rec.head).expect("posts");
    let batches = i.revealed_at(&rec.head);
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    let other = params().with_intake(keccak256(b"another intake"));
    assert_eq!(
        verify_log(&v, &other, &r, rec.head.timestamp, &log),
        Err(IntakeError::WrongIntake)
    );
    assert!(!other.extends(&p));
    // The intake refuses statements under params of another intake.
    let mut j = intake();
    assert_eq!(
        j.submit(&other, &r, signed("vendor.a", &fact("a.two", 2), 1), T0),
        Err(IntakeError::WrongIntake)
    );
}

/// Review TR-R4-1: the relaxed R-TR-8 lets `check_extends` take a new
/// statement dated into a committed period while its fact was still open, so
/// with an intake `verify_log` is the guard: a statement made after the answer
/// and given an earlier post's time is in no posted batch and fails the
/// client's check, while the honest later log passes.
#[test]
fn a_statement_inserted_after_the_answer_fails_verify_log() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&p, &r, signed("vendor.a", &f, 1), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);

    // prev at day 1: f still sealed.
    rec.head = anchor(11, T0 + DAY);
    let log1 = i.log_at(&rec.head);
    assert!(log1.is_empty());
    assert_eq!(check(&i, &rec, &log1), Ok(()));
    let c1 = compute(&p, &r, &log1, &[], rec.head.timestamp)
        .expect("compute")
        .commitment(Hash32::ZERO);

    // next at day 5: the answer (7) public on day 3.
    rec.head = anchor(30, T0 + 5 * DAY);
    let answer = Resolution {
        fact: f.clone(),
        value: Value::UInt(7),
        level: ProofLevel::Recomputed,
        resolver: SourceId("chain.test".into()),
        evidence: keccak256(b"answer"),
        public_from: T0 + 3 * DAY,
    };
    let honest = i.log_at(&rec.head);
    assert_eq!(check(&i, &rec, &honest), Ok(()));
    // vendor.b signs the answer after it is public; the coordinator dates
    // it at the batch's post time.
    let mut forged = honest.clone();
    forged.push(signed("vendor.b", &f, 7).payload.statement(T0 + 100));
    sort_statements(&mut forged);
    let c2 = compute(
        &p,
        &r,
        &forged,
        std::slice::from_ref(&answer),
        rec.head.timestamp,
    )
    .expect("compute")
    .commitment(c1.hash());
    assert_eq!(
        check_extends(
            &c1,
            TrackLog {
                params: &p,
                statements: &log1,
                resolutions: &[]
            },
            &c2,
            TrackLog {
                params: &p,
                statements: &forged,
                resolutions: std::slice::from_ref(&answer)
            }
        ),
        Ok(())
    );
    assert_eq!(check(&i, &rec, &forged), Err(IntakeError::LogMismatch));
}

/// Review TR-R4-5: a batch posted twice, or two batches posted out of order,
/// break the `seq`/`prev` chain; a client sees the duplicate either as a post
/// without a batch or as a header out of place.
#[test]
fn a_batch_posted_twice_or_out_of_order_is_refused() {
    let (p, r) = (params(), reg());
    let mut i = intake();
    let (f, g) = (fact("a.one", 2), fact("a.two", 3));
    i.submit(&p, &r, signed("vendor.a", &f, 1), T0)
        .expect("submit");
    i.submit(&p, &r, signed("vendor.a", &g, 2), T0)
        .expect("submit");
    let (h0, h1) = (i.seal().expect("seal"), i.seal().expect("seal"));
    // Before both closes: nothing is due, so only the chain is judged.
    let head = anchor(40, T0 + DAY);
    let run = |order: &[&BatchHeader], batches: &[RevealedBatch]| {
        let mut rec = Mem::new();
        for (k, h) in order.iter().enumerate() {
            rec.land(h, 10 + k as u64, T0 + 100 + k as u64);
        }
        rec.head = head;
        let posts = rec.posts(&head).expect("posts");
        let v = IntakeView {
            intake: ID.id(),
            head: &head,
            posts: &posts,
            batches,
            books: i.books(),
            reviewed: reviewed(),
            lives: None,
        };
        verify_log(&v, &p, &r, head.timestamp, &[])
    };
    let full = |h: &BatchHeader| RevealedBatch {
        header: h.clone(),
        statements: Vec::new(),
    };
    // Twice: one batch per post is impossible without repeating a header.
    assert_eq!(
        run(&[&h0, &h0, &h1], &[full(&h0), full(&h1)]),
        Err(IntakeError::HiddenPost)
    );
    assert_eq!(
        run(&[&h0, &h0], &[full(&h0), full(&h0)]),
        Err(IntakeError::BadChain("header"))
    );
    // Out of order.
    assert_eq!(
        run(&[&h1, &h0], &[full(&h1), full(&h0)]),
        Err(IntakeError::BadChain("header"))
    );
    // A sync of a duplicate post fails at the intake as well.
    let mut rec = Mem::new();
    rec.land(&h0, 10, T0 + 100);
    rec.land(&h0, 11, T0 + 101);
    assert_eq!(i.sync(&rec), Err(IntakeError::PostMismatch));
}

// ---------------------------------------------------------------- C9: level-3 key statuses (TR-R4-4)

/// A client's check of `log` with the level-3 statuses `lives` (one per post)
/// and params `p`.
fn check_lives(
    i: &Intake,
    rec: &Mem,
    p: &TrackParams,
    lives: Option<&[KeyLives]>,
    log: &[Statement],
) -> Result<(), IntakeError> {
    let posts = rec.posts(&rec.head).map_err(IntakeError::Record)?;
    let batches = i.revealed_at(&rec.head);
    let v = IntakeView {
        intake: ID.id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives,
    };
    verify_log(&v, p, &reg(), rec.head.timestamp, log)
}

/// The statuses of `book` at `block`, with `a1` (vendor.a's key) at `life`.
fn lives_with(book: &KeyBook, block: u64, life: KeyLife) -> KeyLives {
    let a1 = key_id(&SourceId("vendor.a".into()), &addr("a1"));
    let mut l = KeyLives::all_active(book, block);
    for k in l.keys.iter_mut() {
        if k.0 == a1 {
            k.1 = life;
        }
    }
    l
}

/// C9 (TR-R4-4): a key the level-3 book does not show active at a post's
/// block signs nothing that counts in that batch, whatever the coordinator's
/// book says. The batch is still complete and proven; only the log differs.
#[test]
fn a_key_not_active_in_the_level3_book_stops_counting() {
    for life in [
        KeyLife::Compromised,
        KeyLife::Retired,
        KeyLife::Suspended,
        KeyLife::Unknown,
    ] {
        let mut i = intake();
        let mut rec = Mem::new();
        let f = fact("a.one", 2);
        for s in [signed("vendor.a", &f, 1), signed("vendor.b", &f, 2)] {
            i.submit(&params(), &reg(), s, T0).expect("submit");
        }
        let h = post(&mut i, &mut rec, 10, T0 + 100);
        rec.head = anchor(30, T0 + 3 * DAY);
        i.sync(&rec).expect("sync");
        let lives = vec![lives_with(&i.books()[0], 10, life)];
        assert_eq!(h.statement_count, 2);
        let log = i.log_with(&rec.head, &lives).expect("log");
        assert_eq!(log.len(), 1, "{life:?}");
        assert_eq!(log[0].source.0, "vendor.b");
        let p = params();
        assert_eq!(check_lives(&i, &rec, &p, Some(&lives), &log), Ok(()));
        // The coordinator's own log (its flags still active) is refused.
        let all = i.log_at(&rec.head);
        assert_eq!(all.len(), 2);
        assert_eq!(
            check_lives(&i, &rec, &p, Some(&lives), &all),
            Err(IntakeError::LogMismatch),
            "{life:?}"
        );
    }
}

/// C9 (TR-R4-4, revocation race): the status is read at each post's own
/// block. A batch posted before the compromise stands; the next batch, posted
/// after it, loses the key's statements, even though the coordinator never
/// revoked it. Statements already final are never undone (amendment 1 item 5).
#[test]
fn a_revocation_counts_from_the_first_post_after_it() {
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    let g = fact("a.two", 3);
    i.submit(&params(), &reg(), signed("vendor.a", &f, 1), T0)
        .expect("submit f");
    post(&mut i, &mut rec, 10, T0 + 100);
    i.submit(&params(), &reg(), signed("vendor.a", &g, 1), T0 + 200)
        .expect("submit g");
    post(&mut i, &mut rec, 12, T0 + 300);
    rec.head = anchor(40, T0 + 4 * DAY);
    i.sync(&rec).expect("sync");
    let book = &i.books()[0];
    let lives = vec![
        lives_with(book, 10, KeyLife::Active),
        lives_with(book, 12, KeyLife::Compromised),
    ];
    let log = i.log_with(&rec.head, &lives).expect("log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].fact, f);
    assert_eq!(check_lives(&i, &rec, &params(), Some(&lives), &log), Ok(()));
}

/// C9 (TR-R4-4): with a level-3 book on chain the statuses are required, one
/// list per post, each exactly the batch's book at the post's block.
#[test]
fn key_statuses_must_be_the_books_at_each_post() {
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&params(), &reg(), signed("vendor.a", &f, 1), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);
    rec.head = anchor(30, T0 + 3 * DAY);
    i.sync(&rec).expect("sync");
    let log = i.log_at(&rec.head);
    let chained = params().with_chain(keccak256(b"a fact chain"));
    // No statuses with a chain pinned: refused, never "all active".
    assert_eq!(
        check_lives(&i, &rec, &chained, None, &log),
        Err(IntakeError::BadKeys("level-3 key statuses required"))
    );
    let book = &i.books()[0];
    let good = KeyLives::all_active(book, 10);
    assert_eq!(
        check_lives(&i, &rec, &chained, Some(std::slice::from_ref(&good)), &log),
        Ok(())
    );
    let mut wrong_block = good.clone();
    wrong_block.block = 9;
    let mut missing = good.clone();
    missing.keys.pop();
    let mut extra = good.clone();
    extra
        .keys
        .push((keccak256(b"another key"), KeyLife::Active));
    for bad in [wrong_block, missing, extra] {
        assert_eq!(
            check_lives(&i, &rec, &chained, Some(std::slice::from_ref(&bad)), &log),
            Err(IntakeError::BadKeys(
                "key statuses are not the book's at the post"
            )),
            "{bad:?}"
        );
        assert_eq!(i.log_with(&rec.head, &[bad]), None);
    }
    assert_eq!(
        check_lives(
            &i,
            &rec,
            &chained,
            Some(&[good.clone(), good.clone()]),
            &log
        ),
        Err(IntakeError::BadKeys("one key status list per post"))
    );
    assert_eq!(i.log_with(&rec.head, &[good.clone(), good]), None);
}

// ---------------------------------------------------------------- C9: intake receipts (TR-I-6)

/// An intake whose poster key the tests hold (it signs receipts).
fn receipt_id() -> IntakeId {
    IntakeId {
        poster: addr("poster"),
        ..ID
    }
}

fn receipt_params() -> TrackParams {
    let mut p = params();
    p.intake = receipt_id().id();
    p
}

fn signed_for_receipts(source: &str, f: &Fact, value: u128) -> SignedStatement {
    let name = if source == "vendor.a" { "a1" } else { "b1" };
    let payload = StatementPayload {
        intake: receipt_id().id(),
        key: key_id(&SourceId(source.into()), &addr(name)),
        source: SourceId(source.into()),
        fact: f.clone(),
        value: Value::UInt(value),
        confidence: Confidence(900_000),
    };
    let signature = eip191_sign_with(&secret(name), payload.message().as_bytes()).expect("sign");
    SignedStatement { payload, signature }
}

fn sign_receipt(r: Receipt, by: &str) -> SignedReceipt {
    let signature = eip191_sign_with(&secret(by), r.message().as_bytes()).expect("sign");
    SignedReceipt {
        receipt: r,
        signature,
    }
}

fn receipt_status(
    i: &Intake,
    rec: &Mem,
    r: &SignedReceipt,
    s: &SignedStatement,
    inclusion: Option<(u64, &Proof)>,
) -> Result<ReceiptStatus, ReceiptError> {
    let posts = rec.posts(&rec.head).expect("posts");
    let batches = i.revealed_at(&rec.head);
    let v = IntakeView {
        intake: receipt_id().id(),
        head: &rec.head,
        posts: &posts,
        batches: &batches,
        books: i.books(),
        reviewed: reviewed(),
        lives: None,
    };
    check_receipt(
        &receipt_id(),
        &v,
        &receipt_params(),
        &reg(),
        r,
        s,
        inclusion,
    )
}

/// C9 (TR-I-6): the intake signs a receipt with a posting deadline at or
/// before the close; the source proves it kept before anything is public
/// (inclusion under the posted root), and anyone can after the reveal.
#[test]
fn a_kept_receipt_is_provable_before_the_reveal() {
    let mut i = Intake::new(receipt_id().id(), base_book(), &reg(), reviewed()).expect("intake");
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    let s = signed_for_receipts("vendor.a", &f, 1);
    let digest = i
        .submit(&receipt_params(), &reg(), s.clone(), T0)
        .expect("submit");
    // A deadline past the close promises nothing: refused.
    assert_eq!(i.receipt(&digest, f.as_of + 1), None);
    let r = sign_receipt(i.receipt(&digest, f.as_of).expect("receipt"), "poster");
    let h = i.seal().expect("seal");
    rec.land(&h, 10, T0 + 100);
    i.sync(&rec).expect("sync");
    // Sealed: nothing revealed; the intake's inclusion proof shows it kept.
    let (seq, proof) = i.inclusion(&digest).expect("inclusion");
    assert!(i.revealed_at(&rec.head)[0].statements.is_empty());
    let kept = ReceiptStatus::Kept {
        seq: 0,
        committed_at: T0 + 100,
    };
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, Some((seq, &proof))),
        Ok(kept)
    );
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, None),
        Ok(ReceiptStatus::Pending)
    );
    // A proof of another row, or for another batch, is refused.
    let other = Proof {
        index: 1,
        ..proof.clone()
    };
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, Some((seq, &other))),
        Err(ReceiptError::BadInclusion)
    );
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, Some((seq + 1, &proof))),
        Err(ReceiptError::BadInclusion)
    );
    // After the close the batch is revealed: anyone sees it kept.
    rec.head = anchor(30, T0 + 3 * DAY);
    assert_eq!(receipt_status(&i, &rec, &r, &s, None), Ok(kept));
    // The receipt changes no commit time: the log keeps the post's time.
    assert_eq!(i.log_at(&rec.head)[0].committed_at, T0 + 100);
}

/// C9 (TR-I-6): a statement taken and posted late, or never, shows the
/// receipt broken, signed by the intake's own poster: attributable to it.
#[test]
fn a_broken_receipt_is_attributable() {
    let mut i = Intake::new(receipt_id().id(), base_book(), &reg(), reviewed()).expect("intake");
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    let s = signed_for_receipts("vendor.a", &f, 1);
    let digest = i
        .submit(&receipt_params(), &reg(), s.clone(), T0)
        .expect("submit");
    let deadline = T0 + DAY;
    let r = sign_receipt(i.receipt(&digest, deadline).expect("receipt"), "poster");
    // Before the deadline, unposted: pending. Past the deadline and the close,
    // never posted: broken.
    rec.head = anchor(5, T0 + 10);
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, None),
        Ok(ReceiptStatus::Pending)
    );
    rec.head = anchor(20, f.as_of);
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, None),
        Ok(ReceiptStatus::Broken)
    );
    // Posted after the deadline (before the close): late, and visible as such.
    let h = i.seal().expect("seal");
    rec.land(&h, 21, deadline + 1);
    rec.head = anchor(30, T0 + 3 * DAY);
    i.sync(&rec).expect("sync");
    assert_eq!(
        receipt_status(&i, &rec, &r, &s, None),
        Ok(ReceiptStatus::PostedLate {
            seq: 0,
            committed_at: deadline + 1
        })
    );
}

/// C9 (TR-I-6): only the poster's signature over this statement's digest,
/// with a deadline at or before its close, is a receipt.
#[test]
fn a_receipt_is_the_posters_for_this_statement() {
    let i = Intake::new(receipt_id().id(), base_book(), &reg(), reviewed()).expect("intake");
    let rec = Mem::new();
    let f = fact("a.one", 2);
    let s = signed_for_receipts("vendor.a", &f, 1);
    let other = signed_for_receipts("vendor.a", &f, 2);
    let receipt = Receipt {
        intake: receipt_id().id(),
        digest: s.payload.digest(),
        deadline: f.as_of,
    };
    let good = sign_receipt(receipt.clone(), "poster");
    assert_eq!(
        receipt_status(&i, &rec, &good, &s, None),
        Ok(ReceiptStatus::Pending)
    );
    assert_eq!(
        receipt_status(&i, &rec, &sign_receipt(receipt.clone(), "a1"), &s, None),
        Err(ReceiptError::BadSignature)
    );
    assert_eq!(
        receipt_status(&i, &rec, &good, &other, None),
        Err(ReceiptError::WrongReceipt)
    );
    let far = sign_receipt(
        Receipt {
            deadline: f.as_of + 1,
            ..receipt.clone()
        },
        "poster",
    );
    assert_eq!(
        receipt_status(&i, &rec, &far, &s, None),
        Err(ReceiptError::BadDeadline)
    );
    let elsewhere = sign_receipt(
        Receipt {
            intake: ID.id(),
            ..receipt
        },
        "poster",
    );
    assert_eq!(
        receipt_status(&i, &rec, &elsewhere, &s, None),
        Err(ReceiptError::WrongReceipt)
    );
}

// ---------------------------------------------------------------- C9 review TR-C9-R1

/// Review TR-C9-R1-3: a kept receipt stays kept when the statement is shown
/// with another signature over the same payload (the receipt binds the
/// payload; the batch row is checked under its own root).
#[test]
fn a_kept_receipt_survives_a_second_signature() {
    let mut i = Intake::new(receipt_id().id(), base_book(), &reg(), reviewed()).expect("intake");
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    let s = signed_for_receipts("vendor.a", &f, 1);
    let digest = i
        .submit(&receipt_params(), &reg(), s.clone(), T0)
        .expect("submit");
    let r = sign_receipt(i.receipt(&digest, f.as_of).expect("receipt"), "poster");
    let h = i.seal().expect("seal");
    rec.land(&h, 10, T0 + 100);
    rec.head = anchor(30, T0 + 3 * DAY);
    i.sync(&rec).expect("sync");
    let mut resigned = s.clone();
    resigned.signature[10] ^= 1;
    let kept = Ok(ReceiptStatus::Kept {
        seq: 0,
        committed_at: T0 + 100,
    });
    assert_eq!(receipt_status(&i, &rec, &r, &s, None), kept);
    assert_eq!(receipt_status(&i, &rec, &r, &resigned, None), kept);
}

/// Review TR-C9-R1-8: in chain mode the coordinator cannot censor with its
/// own flag either: a key revoked in its book but active in the level-3 book
/// at the post is refused.
#[test]
fn a_revoked_flag_on_an_active_key_is_refused() {
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&params(), &reg(), signed("vendor.b", &f, 2), T0)
        .expect("submit");
    i.set_keys(
        book(vec![
            key("vendor.a", "a1", true),
            key("vendor.b", "b1", false),
        ]),
        &reg(),
        reviewed(),
    )
    .expect("revoke a1");
    post(&mut i, &mut rec, 10, T0 + 100);
    rec.head = anchor(30, T0 + 3 * DAY);
    i.sync(&rec).expect("sync");
    let log = i.log_at(&rec.head);
    let chained = params().with_chain(keccak256(b"a fact chain"));
    let active = KeyLives::all_active(&i.books()[0], 10);
    assert_eq!(
        check_lives(
            &i,
            &rec,
            &chained,
            Some(std::slice::from_ref(&active)),
            &log
        ),
        Err(IntakeError::BadKeys(
            "revoked key active in the level-3 book"
        ))
    );
    let compromised = lives_with(&i.books()[0], 10, KeyLife::Compromised);
    assert_eq!(
        check_lives(&i, &rec, &chained, Some(&[compromised]), &log),
        Ok(())
    );
}

/// Review TR-C9-R1-5: a period statement is sealed until its period ends,
/// not only until its close: nobody can steer the day's total toward or away
/// from a forecast already public.
#[test]
fn a_period_batch_stays_sealed_until_the_period_ends() {
    let mut r = reg();
    r.add_metric(
        "test.flow",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .expect("flow");
    let mut p = params();
    p.rules.push(MetricRule::period_start(
        MetricId("test.flow".into()),
        DAY,
        7_200,
        0,
    ));
    p.rules.sort_by(|a, b| a.metric.cmp(&b.metric));
    let day_end = (T0 / DAY + 10) * DAY;
    let close = day_end - DAY - 7_200;
    let f = Fact {
        metric: MetricId("test.flow".into()),
        subject: Subject::Aggregate("a.one".into()),
        as_of: day_end,
    };
    let mut i = intake();
    let mut rec = Mem::new();
    i.submit(&p, &r, signed("vendor.a", &f, 5), close - DAY)
        .expect("submit");
    let h = post(&mut i, &mut rec, 10, close - DAY + 60);
    assert_eq!(h.reveal_by, day_end);
    for (t, n) in [(close + 1, 0), (day_end - 1, 0), (day_end, 1)] {
        rec.head = anchor(20 + t % 1000, t);
        assert_eq!(i.log_at(&rec.head).len(), n, "at {t}");
        assert!(i.revealed_at(&rec.head)[0].statements.len() == n);
    }
}

/// ROADMAP C20 (THREATS TR-S-13): the intake is published beside each
/// commitment; a client runs `verify_log` on the served view and refuses a
/// view that drops or reveals early what the log holds.
#[cfg(feature = "wire")]
#[test]
fn the_published_intake_gives_the_served_log() {
    use crate::publication::{Publication, PublicationError};
    use crate::wire::{check_log_body, log_body, LogError, LogExpect};
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    i.submit(&p, &r, signed("vendor.a", &f, 1), T0)
        .expect("submit");
    i.submit(&p, &r, signed("vendor.b", &f, 2), T0)
        .expect("submit");
    post(&mut i, &mut rec, 10, T0 + 100);
    let mut pubn = Publication::new();
    // Before the close: nothing revealed, the header only.
    rec.head = anchor(20, f.as_of - 1);
    let early = i.log_at(&rec.head);
    let b1 = compute(&p, &r, &early, &[], rec.head.timestamp).expect("compute");
    let view = i.published(&rec.head);
    assert_eq!((view.posts.len(), view.revealed_rows()), (1, 0));
    pubn.push(&p, &early, &[], &b1, Some(view)).expect("push");
    // At the close: both revealed.
    rec.head = anchor(21, f.as_of);
    let log = i.log_at(&rec.head);
    let b2 = compute(&p, &r, &log, &[], rec.head.timestamp).expect("compute");
    pubn.push(&p, &log, &[], &b2, Some(i.published(&rec.head)))
        .expect("push");
    assert!(pubn.check().is_ok());
    // A publication without its intake, or at another head, is refused.
    let mut none = pubn.clone();
    none.intake = None;
    assert_eq!(none.check().err(), Some(PublicationError::Intake));

    let e = LogExpect {
        registry: Some(&r),
        reviewed: Some(reviewed()),
        ..Default::default()
    };
    let body = log_body(&pubn, 0).expect("body");
    let got = check_log_body(&body, &e).expect("checks");
    assert!(got.recomputed && got.has_intake && got.intake_checked);
    assert_eq!(got.statements.len(), 2);
    // Without the reviewed keys the intake is said unchecked, not passed.
    let no_keys = LogExpect {
        reviewed: None,
        ..e
    };
    assert!(
        !check_log_body(&body, &no_keys)
            .expect("checks")
            .intake_checked
    );

    // A server that hides a revealed row from the published batch.
    let mut cut = pubn.clone();
    if let Some(v) = cut.intake.as_mut() {
        v.batches[0].statements.pop();
    }
    assert_eq!(
        check_log_body(&log_body(&cut, 0).expect("body"), &e).err(),
        Some(LogError::Intake(IntakeError::Incomplete))
    );
    // A server that serves the early commitment's view beside the latest log.
    let mut stale = pubn;
    if let Some(v) = stale.intake.as_mut() {
        v.batches = i.revealed_at(&anchor(20, f.as_of - 1));
    }
    assert_eq!(
        check_log_body(&log_body(&stale, 0).expect("body"), &e).err(),
        Some(LogError::Intake(IntakeError::Incomplete))
    );
}

/// ROADMAP C21: with chained logs the intake comes in pages of whole posts;
/// a client checks every post's batch with `verify_log` once all are in, and
/// refuses a page that drops a revealed row or names another commitment.
#[cfg(feature = "wire")]
#[test]
fn a_chained_log_pages_the_intake_and_verifies_it() {
    use crate::publication::Publication;
    use crate::wire::{
        chained_log_body, log_posts_page, log_rows_page, LogError, LogExpect, PagedLog,
    };
    let (p, r) = (params(), reg());
    let mut i = intake();
    let mut rec = Mem::new();
    let f = fact("a.one", 2);
    for (n, src) in ["vendor.a", "vendor.b"].into_iter().enumerate() {
        i.submit(&p, &r, signed(src, &f, n as u128 + 1), T0)
            .expect("submit");
        post(&mut i, &mut rec, 10 + n as u64, T0 + 100 + n as u64);
    }
    rec.head = anchor(21, f.as_of);
    let log = i.log_at(&rec.head);
    let book = compute(&p, &r, &log, &[], rec.head.timestamp).expect("compute");
    let mut pubn = Publication::chained();
    pubn.push_with(&p, &log, &[], &book, Some(i.published(&rec.head)), Some(&r))
        .expect("push");
    pubn.check().expect("node check");
    let e = LogExpect {
        registry: Some(&r),
        reviewed: Some(reviewed()),
        ..Default::default()
    };
    let run = |pubn: &Publication| -> Result<crate::wire::CheckedLog, LogError> {
        let mut l = PagedLog::start(&chained_log_body(pubn, 0).expect("head"), &e)?;
        while let Some(pos) = l.next_rows() {
            l.take_rows(&log_rows_page(pubn, 0, pos).expect("rows"))?;
        }
        while let Some(n) = l.next_posts() {
            l.take_posts(&log_posts_page(pubn, n).expect("posts"))?;
        }
        l.finish(&e)
    };
    let got = run(&pubn).expect("checks");
    assert!(got.recomputed && got.has_intake && got.intake_checked);
    assert_eq!(got.statements.len(), 2);
    // A page that hides a revealed row: the intake no longer gives the log.
    let mut cut = pubn.clone();
    if let Some(v) = cut.intake.as_mut() {
        v.batches[1].statements.pop();
    }
    assert_eq!(
        run(&cut).err(),
        Some(LogError::Intake(IntakeError::Incomplete))
    );
    // A posts page for another commitment.
    let mut l = PagedLog::start(&chained_log_body(&pubn, 0).expect("head"), &e).expect("start");
    while let Some(pos) = l.next_rows() {
        l.take_rows(&log_rows_page(&pubn, 0, pos).expect("rows"))
            .expect("rows");
    }
    let mut other = log_posts_page(&pubn, 0).expect("posts");
    other["commitmentHash"] = serde_json::json!(Hash32([1; 32]).to_string());
    assert_eq!(l.take_posts(&other).err(), Some(LogError::Commitment));
    // Review C21-R2-4: a page cut short (one post where both fit) is
    // taken, the next page then refused: a server cannot make its client ask
    // once per post.
    let mut one = log_posts_page(&pubn, 0).expect("posts");
    let all: Vec<(crate::intake::Post, crate::intake::RevealedBatch)> = borsh::from_slice(
        &hex::decode(one["posts"].as_str().expect("hex").trim_start_matches("0x")).expect("hex"),
    )
    .expect("borsh");
    assert_eq!(all.len(), 2);
    one["posts"] = serde_json::json!(format!(
        "0x{}",
        hex::encode(borsh::to_vec(&all[..1].to_vec()).expect("borsh"))
    ));
    l.take_posts(&one).expect("a short page is taken");
    assert_eq!(
        l.take_posts(&log_posts_page(&pubn, 1).expect("posts"))
            .err(),
        Some(LogError::Malformed("posts pages cut short"))
    );
    // Finishing with a post missing is refused.
    assert!(l.finish(&e).is_err());
}
