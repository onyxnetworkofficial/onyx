//! Proof level 2 tests (ADR-0027): observation specs, the observer draw, commits and reveals,
//! the strict-majority settle, the challenge and slash paths, and every review finding's exploit.

use super::*;
use onyx_core::{DataRights, ObservationKind, Redistribution, SourceId};

const WINDOW: (u64, u64) = (1_000, 1_050);
const SALT: Hash32 = Hash32([7; 32]);

fn spec(kind: ObservationKind) -> ObservationSpec {
    ObservationSpec {
        feed: "example.page.value".into(),
        metric: onyx_core::MetricId("example.page.value".into()),
        subject: onyx_core::Subject::Entity("entity:example-exchange".into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/value".into(),
        kind,
        window_start: WINDOW.0,
        window_end: WINDOW.1,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("example.page".into()),
            attribution: None,
            terms: String::new(),
        },
    }
}

const NUM: ObservationKind = ObservationKind::Numeric {
    scale: 2,
    tolerance: 50,
};
const EXACT: ObservationKind = ObservationKind::Exact { max_len: 16 };

/// A store with `spec(kind)`'s template registered (L2SRC-R2-F1).
fn store_for(kind: ObservationKind) -> ObservationResults {
    store_for_n(kind, 3)
}

/// A store whose template draws `n` observers.
fn store_for_n(kind: ObservationKind, n: u16) -> ObservationResults {
    let mut s = spec(kind);
    s.min_observers = n;
    let (mut store, admin) = ObservationResults::new();
    store
        .register_feed(&admin, template_of_spec(&s), "test feed", 0)
        .expect("register");
    store
}

fn template_of_spec(s: &ObservationSpec) -> FeedTemplate {
    FeedTemplate {
        feed: s.feed.clone(),
        metric: s.metric.clone(),
        fetcher: s.fetcher,
        url_template: s.url.clone(),
        selector: s.selector.clone(),
        kind: s.kind,
        window_secs: s.window_end - s.window_start,
        min_observers: s.min_observers,
        rights: s.rights.clone(),
        subject: onyx_core::SubjectRule::Fixed(s.subject.clone()),
    }
}

fn cfg() -> ObserveConfig {
    ObserveConfig {
        commit_close: 1_100,
        reveal_close: 1_200,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

/// Test observer `i`: a fake key whose address starts with byte `i`
/// (`test_keys::ordered`), so `addr(1) < addr(2) < ...`.
fn addr(i: u8) -> MinerAddress {
    test_keys::ordered(i).1
}

/// Commit and reveal signed with the test key of the observer (an
/// all-zero signature for an address that has none).
impl ObservationRound {
    fn commit_as(
        &mut self,
        now: u64,
        observer: MinerAddress,
        hash: Hash32,
    ) -> Result<(), ObserveError> {
        let sig = test_keys::ordered_secret(&observer)
            .and_then(|sk| sign_commit(&sk, &self.round_id, self.draw.attempt, &hash))
            .unwrap_or([0; 65]);
        self.commit(now, observer, hash, sig)
    }

    fn reveal_as(
        &mut self,
        now: u64,
        observer: MinerAddress,
        observation: Observation,
        salt: Hash32,
    ) -> Result<(), ObserveError> {
        let sig = test_keys::ordered_secret(&observer)
            .and_then(|sk| sign_reveal(&sk, &self.round_id, self.draw.attempt, &observation, &salt))
            .unwrap_or([0; 65]);
        self.reveal(now, observer, observation, salt, sig)
    }
}

fn beacon() -> Hash32 {
    keccak256(b"test beacon")
}

/// The draw of a pool that is exactly the intended observers.
fn seed_of(pool: &[Observer]) -> DrawSeed {
    DrawSeed {
        pool: pool_hash(pool),
        beacon_height: 10,
        beacon: beacon(),
        attempt: 0,
    }
}

/// Open a round whose pool is `obs` (all drawn when the spec needs that many).
fn open_all(
    spec: ObservationSpec,
    cfg: ObserveConfig,
    obs: Vec<Observer>,
) -> Result<ObservationRound, ObserveError> {
    let seed = seed_of(&obs);
    ObservationRound::open(spec, cfg, obs, seed)
}

/// Announce, then open, a round in `store` from pool `pool`.
fn open_in(
    store: &mut ObservationResults,
    spec: ObservationSpec,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
) -> Result<ObservationRound, ObserveError> {
    let snapshot = store.announce_draw(&spec, &cfg, &pool, 10, &SimulatedChain::at(0))?;
    store.open_round(spec, cfg, snapshot, &SimulatedChain::with_beacon(beacon()))
}

fn round(kind: ObservationKind, n: u8) -> ObservationRound {
    let obs = (1..=n)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut s = spec(kind);
    s.min_observers = u16::from(n);
    open_all(s, cfg(), obs).expect("open")
}

fn obs(answer: Answer) -> Observation {
    let page_hash = if answer == Answer::Unavailable {
        Hash32::ZERO
    } else {
        keccak256(b"page")
    };
    Observation {
        answer,
        observed_at: 1_030,
        page_hash,
    }
}

/// Each observer commits at 1_070 and reveals at 1_150.
fn play(r: &mut ObservationRound, answers: &[(u8, Answer)]) {
    for (i, a) in answers {
        let h = observation_commit(&r.round_id(), &addr(*i), &obs(a.clone()), &SALT);
        r.commit_as(1_070, addr(*i), h).expect("commit");
    }
    for (i, a) in answers {
        r.reveal_as(1_150, addr(*i), obs(a.clone()), SALT)
            .expect("reveal");
    }
}

fn status_of(s: &ObservationSettlement, i: u8) -> ObserverStatus {
    s.statuses()
        .iter()
        .find(|(a, _)| *a == addr(i))
        .map(|(_, st)| *st)
        .expect("status")
}

#[test]
fn honest_majority_settles_the_median_and_slashes_the_minority_liar() {
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[
            (1, Answer::Number(6_412_345)),
            (2, Answer::Number(6_412_350)),
            (3, Answer::Number(6_412_340)),
            (4, Answer::Number(6_412_349)),
            (5, Answer::Number(9_000_000)), // liar
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(
        *s.outcome(),
        ObservationOutcome::Value(Answer::Number(6_412_345))
    );
    for i in 1..=4 {
        assert_eq!(status_of(&s, i), ObserverStatus::Agreed);
    }
    assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
    assert_eq!(
        s.entries(),
        [
            (addr(1), 10),
            (addr(2), 10),
            (addr(3), 10),
            (addr(4), 10),
            (addr(5), -100)
        ]
    );
    assert_ne!(s.result_hash(), Hash32::ZERO);
}

#[test]
fn exact_feed_needs_identical_bytes_from_a_strict_majority() {
    let mut r = round(EXACT, 3);
    play(
        &mut r,
        &[
            (1, Answer::Bytes(b"open".to_vec())),
            (2, Answer::Bytes(b"open".to_vec())),
            (3, Answer::Bytes(b"closed".to_vec())),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(
        *s.outcome(),
        ObservationOutcome::Value(Answer::Bytes(b"open".to_vec()))
    );
    assert_eq!(status_of(&s, 3), ObserverStatus::Outlier);
}

#[test]
fn copier_cannot_reveal_a_copied_commit_or_change_its_answer_late() {
    let mut r = round(EXACT, 3);
    let honest = obs(Answer::Bytes(b"open".to_vec()));
    let h1 = observation_commit(&r.round_id(), &addr(1), &honest, &SALT);
    r.commit_as(1_070, addr(1), h1).expect("commit 1");
    // The copier posts observer 1's hash, hoping to reveal whatever wins.
    // Accepted (L2R-F5), but the hash binds observer 1's address, so the
    // copier can never open it.
    r.commit_as(1_071, addr(3), h1).expect("copied hash");
    let h2 = observation_commit(&r.round_id(), &addr(2), &honest, &SALT);
    r.commit_as(1_073, addr(2), h2).expect("commit 2");
    // No reveal is visible before commits close.
    assert_eq!(
        r.reveal_as(1_090, addr(1), honest.clone(), SALT),
        Err(ObserveError::WrongPhase)
    );
    r.reveal_as(1_150, addr(1), honest.clone(), SALT)
        .expect("reveal 1");
    r.reveal_as(1_150, addr(2), honest.clone(), SALT)
        .expect("reveal 2");
    // Having seen the answer, the copier tries to reveal it: it does not
    // match its commit, and after reveal_close nothing is accepted at all.
    assert_eq!(
        r.reveal_as(1_160, addr(3), honest.clone(), SALT),
        Err(ObserveError::HashMismatch)
    );
    assert_eq!(
        r.reveal_as(1_201, addr(3), honest.clone(), SALT),
        Err(ObserveError::WrongPhase)
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(status_of(&s, 3), ObserverStatus::Unrevealed);
    // SIG-R1-F1: no slash on the coordinator's word alone.
    assert!(s.entries().iter().all(|(a, _)| *a != addr(3)));
}

#[test]
fn source_unavailable_consensus_slashes_nobody_who_said_so() {
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Number(6_412_345)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
    // L2R-F2: nobody is paid for an outage; nobody who said so is slashed.
    for i in 1..=3 {
        assert_eq!(status_of(&s, i), ObserverStatus::NoCall);
    }
    // L2R2-F4: a value while the majority found the source down costs the
    // small slash. A silent member is absent, not slashed (SIG-R1-F1).
    assert_eq!(status_of(&s, 4), ObserverStatus::Missed);
    assert_eq!(status_of(&s, 5), ObserverStatus::Absent);
    assert_eq!(s.entries(), [(addr(4), -20)]);
}

#[test]
fn unavailable_while_a_value_settles_costs_the_small_slash() {
    let mut r = round(EXACT, 3);
    play(
        &mut r,
        &[
            (1, Answer::Bytes(b"open".to_vec())),
            (2, Answer::Bytes(b"open".to_vec())),
            (3, Answer::Unavailable),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(status_of(&s, 3), ObserverStatus::Missed);
    assert!(s.entries().contains(&(addr(3), -20)));
}

#[test]
fn a_tie_or_a_plurality_is_no_result_and_judges_no_revealer() {
    // 4 observers, 2 vs 2: no strict majority of 4.
    let mut r = round(EXACT, 4);
    play(
        &mut r,
        &[
            (1, Answer::Bytes(b"open".to_vec())),
            (2, Answer::Bytes(b"open".to_vec())),
            (3, Answer::Bytes(b"closed".to_vec())),
            (4, Answer::Bytes(b"closed".to_vec())),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
    assert!(s
        .statuses()
        .iter()
        .all(|(_, st)| *st == ObserverStatus::NoCall));
    assert!(s.entries().is_empty());
}

#[test]
fn silent_observers_never_let_a_minority_decide() {
    // 5 assigned, only 2 reveal (and agree): 2 < quorum(5) = 3.
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[(1, Answer::Number(100)), (2, Answer::Number(100))],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
    assert_eq!(status_of(&s, 1), ObserverStatus::NoCall);
    assert_eq!(status_of(&s, 3), ObserverStatus::Absent);
    // SIG-R1-F1: silence is not slashed on the coordinator's word.
    assert!(s.entries().is_empty());
}

#[test]
fn too_few_observers_never_open() {
    let obs = vec![
        Observer {
            address: addr(1),
            bond: 1_000,
        },
        Observer {
            address: addr(2),
            bond: 1_000,
        },
    ];
    assert_eq!(
        open_all(spec(NUM), cfg(), obs).err(),
        Some(ObserveError::TooFewObservers { have: 2, need: 3 })
    );
}

#[test]
fn open_checks_order_bonds_and_timing() {
    let three = |bond| -> Vec<Observer> {
        (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond,
            })
            .collect()
    };
    assert_eq!(
        open_all(spec(NUM), cfg(), three(99)).err(),
        Some(ObserveError::TooFewObservers { have: 0, need: 3 })
    );
    let mut unsorted = three(1_000);
    unsorted.swap(0, 1);
    assert_eq!(
        open_all(spec(NUM), cfg(), unsorted).err(),
        Some(ObserveError::BadPool)
    );
    let bad_cfg = |f: &dyn Fn(&mut ObserveConfig)| {
        let mut c = cfg();
        f(&mut c);
        open_all(spec(NUM), c, three(1_000)).is_err()
    };
    assert!(bad_cfg(&|c| c.commit_close = WINDOW.1 - 1));
    assert!(bad_cfg(
        &|c| c.commit_close = WINDOW.1 + MAX_COMMIT_GRACE_SECS + 1
    ));
    assert!(bad_cfg(
        &|c| c.reveal_close = c.commit_close + MIN_REVEAL_SECS - 1
    ));
    assert!(bad_cfg(&|c| c.miss_slash = 0));
    assert!(bad_cfg(&|c| c.outlier_slash = c.miss_slash - 1));
    // L2R-F7: every amount must fit a ledger delta; no silent clamp.
    assert!(bad_cfg(&|c| c.reward = u128::MAX));
    assert!(bad_cfg(&|c| c.reward = i128::MAX.unsigned_abs() + 1));
    assert!(!bad_cfg(&|c| c.reward = i128::MAX.unsigned_abs()));
    assert!(bad_cfg(&|c| {
        c.miss_slash = u128::MAX;
        c.outlier_slash = u128::MAX;
    }));
    let mut forbidden = spec(NUM);
    forbidden.rights.redistribution = Redistribution::Forbidden;
    assert!(open_all(forbidden, cfg(), three(1_000)).is_err());
}

#[test]
fn phases_members_and_reveal_shape_are_enforced() {
    let mut r = round(EXACT, 3);
    let o = obs(Answer::Bytes(b"open".to_vec()));
    let h = observation_commit(&r.round_id(), &addr(1), &o, &SALT);
    assert_eq!(r.commit_as(999, addr(1), h), Err(ObserveError::WrongPhase));
    assert_eq!(
        r.commit_as(1_101, addr(1), h),
        Err(ObserveError::WrongPhase)
    );
    assert_eq!(
        r.commit_as(1_070, addr(9), h),
        Err(ObserveError::NotObserver)
    );
    r.commit_as(1_070, addr(1), h).expect("commit");
    assert_eq!(
        r.commit_as(1_071, addr(1), keccak256(b"x")),
        Err(ObserveError::AlreadyCommitted)
    );
    assert_eq!(
        r.reveal_as(1_150, addr(2), o.clone(), SALT),
        Err(ObserveError::NoCommit)
    );
    // A reveal outside the window is refused even when it matches its commit.
    let late = Observation {
        observed_at: 1_061,
        ..o.clone()
    };
    let hl = observation_commit(&r.round_id(), &addr(2), &late, &SALT);
    r.commit_as(1_070, addr(2), hl).expect("commit 2");
    assert!(matches!(
        r.reveal_as(1_150, addr(2), late, SALT),
        Err(ObserveError::Observation(ObservationError::OutsideWindow))
    ));
    r.reveal_as(1_150, addr(1), o.clone(), SALT)
        .expect("reveal");
    assert_eq!(
        r.reveal_as(1_151, addr(1), o, SALT),
        Err(ObserveError::AlreadyRevealed)
    );
    assert_eq!(r.settle(1_200).err(), Some(ObserveError::WrongPhase));
}

/// The round of `round(EXACT, 3)` with every observer committed and
/// revealed, and a simulated log whose draw has the test receipt signer.
fn logged_round() -> (ObservationRound, SimulatedTranscriptLog, Hash32) {
    let mut r = round(EXACT, 3);
    let rid = r.round_id();
    for i in 1..=3 {
        let o = obs(Answer::Bytes(b"open".to_vec()));
        r.commit_as(
            1_070,
            addr(i),
            observation_commit(&rid, &addr(i), &o, &SALT),
        )
        .expect("commit");
        r.reveal_as(1_150, addr(i), o, SALT).expect("reveal");
    }
    let fact = keccak256(b"registry fact");
    let mut log = SimulatedTranscriptLog::default();
    log.signers
        .insert((fact, r.draw.attempt), test_keys::receipt_signer());
    (r, log, fact)
}

#[test]
fn a_withheld_commit_or_receipted_reveal_is_caught() {
    // ADR-0027 amendment 8 (SIG-R1-F1).
    let (r, mut log, fact) = logged_round();
    let rid = r.round_id();
    let full = r.transcript_query(fact);
    assert!(log.complete(&full), "nothing logged: complete");
    let (a1, c1, s1) = r.signed_commits()[0];
    // Observer 1 posts its commit inside the commit phase.
    log.post_commit(&fact, 0, &a1, &rid, &c1, &s1, 1_080)
        .expect("post");
    assert_eq!(
        log.post_commit(&fact, 0, &a1, &rid, &c1, &s1, 1_080),
        Err("AlreadyPosted")
    );
    assert!(log.complete(&full));
    // A server that leaves the posted commit out is caught.
    let mut q = full.clone();
    q.entries[0].commit = Hash32::ZERO;
    q.entries[0].revealed = false;
    assert!(!log.complete(&q), "withheld commit");
    // Receipt: the coordinator's key says it holds observer 1's reveal.
    let receipt = r
        .reveal_receipt(&test_keys::receipt_secret(), &a1)
        .expect("receipt");
    assert!(reveal_receipt_signed(
        &test_keys::receipt_signer(),
        &rid,
        0,
        &a1,
        &c1,
        &receipt
    ));
    log.post_receipt(&fact, 0, &a1, &rid, &c1, &receipt)
        .expect("post receipt");
    assert!(log.complete(&full));
    let mut q = full.clone();
    q.entries[0].revealed = false;
    assert!(!log.complete(&q), "withheld reveal");
    // Only the draw's receipt signer counts; only the observer posts its commit.
    let (a2, c2, _) = r.signed_commits()[1];
    let forged = sign_reveal_receipt(&test_keys::ordered(2).0, &rid, 0, &a2, &c2).expect("sign");
    assert_eq!(
        log.post_receipt(&fact, 0, &a2, &rid, &c2, &forged),
        Err("BadSignature")
    );
    assert_eq!(
        log.post_commit(&fact, 0, &a2, &rid, &c2, &s1, 1_080),
        Err("BadSignature")
    );
    assert_eq!(
        log.post_commit(&fact, 1, &a1, &rid, &c1, &s1, 1_080),
        Err("NoDraw")
    );
}

#[test]
fn posts_outside_the_round_never_veto_it() {
    // An observer cannot block a round by posting: a commit posted
    // before the window or after commit close, or for another round, is
    // not the round's; a commit posted in time is the one the round uses
    // (`absorb_posted_commit`), so the coordinator can always serve it.
    let (mut r, mut log, fact) = logged_round();
    let rid = r.round_id();
    let sk3 = test_keys::ordered(3).0;
    let a3 = addr(3);
    let other = keccak256(b"another commit");
    let sig = sign_commit(&sk3, &rid, 0, &other).expect("sign");
    let late = sig;
    // Late (after commit close): ignored by the chain's check and refused by the round.
    log.post_commit(&fact, 0, &a3, &rid, &other, &late, 1_101)
        .expect("post");
    assert!(log.complete(&r.transcript_query(fact)));
    assert_eq!(
        r.absorb_posted_commit(1_101, a3, other, late),
        Err(ObserveError::WrongPhase)
    );
    assert_eq!(
        r.absorb_posted_commit(WINDOW.0 - 1, a3, other, late),
        Err(ObserveError::WrongPhase)
    );
    // For another round: another slot, never this round's.
    let rid2 = keccak256(b"other round");
    let sig2 = sign_commit(&sk3, &rid2, 0, &other).expect("sign");
    log.post_commit(&fact, 0, &a3, &rid2, &other, &sig2, 1_080)
        .expect("post");
    assert!(log.complete(&r.transcript_query(fact)));
    // In time, for this round, but another commit than the one sent off
    // chain (an observer that equivocates): the posted one replaces it
    // and its reveal (of the old commit) is dropped.
    let mut log2 = SimulatedTranscriptLog::default();
    log2.signers.insert((fact, 0), test_keys::receipt_signer());
    log2.post_commit(&fact, 0, &a3, &rid, &other, &sig, 1_090)
        .expect("post");
    assert!(!log2.complete(&r.transcript_query(fact)));
    r.absorb_posted_commit(1_090, a3, other, sig)
        .expect("absorb");
    let q = r.transcript_query(fact);
    assert!(log2.complete(&q), "the round now serves the posted commit");
    assert_eq!(q.entries[2].commit, other);
    assert!(!q.entries[2].revealed);
    assert!(r
        .reveal_receipt(&test_keys::receipt_secret(), &a3)
        .is_none());
    assert_eq!(
        r.absorb_posted_commit(1_090, [9; 20], other, sig),
        Err(ObserveError::NotObserver)
    );
}

/// Review REVLOG HIGH-1 (the reviewer's `test_PocObserverVeto`, silent
/// variant): observer 3 posts an in-phase commit on chain and never
/// commits off chain. Before the fix nothing read the log, so the honest
/// transcript failed `transcriptComplete` for good. Now the coordinator
/// absorbs the posted commit once the chain is past commit close, the
/// griefer is a non-reveal, and the round is complete on chain.
#[test]
fn a_silent_griefer_post_is_absorbed_never_a_veto() {
    let mut r = round(EXACT, 3);
    let rid = r.round_id();
    let fact = OnchainFact {
        metric_key: keccak256(b"metric"),
        subject_key: keccak256(b"subject"),
        window_end: WINDOW.1,
        dated: false,
        template: keccak256(b"template"),
    };
    let k = fact.registry_key();
    let mut reg = SimulatedRegistry::new();
    reg.log.signers.insert((k, 0), test_keys::receipt_signer());
    let o = obs(Answer::Bytes(b"open".to_vec()));
    for i in 1..=2 {
        r.commit_as(
            1_070,
            addr(i),
            observation_commit(&rid, &addr(i), &o, &SALT),
        )
        .expect("commit");
    }
    let a3 = addr(3);
    let c3 = keccak256(b"posted only");
    let s3 = sign_commit(&test_keys::ordered(3).0, &rid, 0, &c3).expect("sign");
    reg.log
        .post_commit(&k, 0, &a3, &rid, &c3, &s3, 1_080)
        .expect("post");
    // A non-drawn key's post for this round is never asked for.
    let (sk9, a9) = test_keys::ordered(9);
    let s9 = sign_commit(&sk9, &rid, 0, &c3).expect("sign");
    reg.log
        .post_commit(&k, 0, &a9, &rid, &c3, &s9, 1_080)
        .expect("post");
    let before = r.clone();
    assert!(
        !reg.log.complete(&before.transcript_query(k)),
        "the old behaviour: refused for good"
    );
    // Not while a post could still land inside the commit phase.
    reg.time = r.cfg.commit_close;
    let e = absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW).expect_err("too early");
    assert!(e.contains("not past commit close"), "{e}");
    reg.time = r.cfg.commit_close + 1;
    assert_eq!(
        absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW),
        Ok(vec![a3])
    );
    for i in 1..=2 {
        let sk = test_keys::ordered(i).0;
        let sig = sign_reveal(&sk, &rid, 0, &o, &SALT).expect("sign");
        let took = take_reveal(
            &mut r,
            &mut reg,
            &fact,
            &test_keys::receipt_secret(),
            1_150,
            addr(i),
            o.clone(),
            SALT,
            sig,
        );
        assert_eq!(took, Ok(true));
    }
    // The griefer's reveal of anything else is a non-reveal, no receipt.
    let sig = sign_reveal(&test_keys::ordered(3).0, &rid, 0, &o, &SALT).expect("sign");
    let took = take_reveal(
        &mut r,
        &mut reg,
        &fact,
        &test_keys::receipt_secret(),
        1_150,
        a3,
        o.clone(),
        SALT,
        sig,
    );
    assert_eq!(took, Ok(false));
    assert_eq!(reg.log.receipt(&k, 0, &a3, &rid), None);
    assert_eq!(check_transcript_logged(&r, &reg, &fact), Ok(()));
    let q = r.transcript_query(k);
    assert_eq!(q.entries[2].commit, c3);
    assert!(!q.entries[2].revealed);
    // A server that then drops the absorbed commit is caught.
    let mut dropped = q.clone();
    dropped.entries[2].commit = Hash32::ZERO;
    assert!(!reg.log.complete(&dropped));
}

/// Review REVLOG-R2 MEDIUM-2 (the reviewer's
/// `poc_reveal_error_after_receipts_voids_the_attempt`): nothing an
/// observer sends stops the round once receipts are on chain. A second
/// reveal is ignored (the first stays, its receipt is posted again as
/// success), and a badly signed, mismatched or late reveal is a
/// non-reveal without a receipt; the round still settles and is complete
/// on chain.
#[test]
fn an_observer_cannot_stop_the_round_after_receipts() {
    let mut r = round(EXACT, 3);
    let rid = r.round_id();
    let fact = OnchainFact {
        metric_key: keccak256(b"metric"),
        subject_key: keccak256(b"subject"),
        window_end: WINDOW.1,
        dated: false,
        template: keccak256(b"template"),
    };
    let k = fact.registry_key();
    let mut reg = SimulatedRegistry::new();
    reg.log.signers.insert((k, 0), test_keys::receipt_signer());
    let o = obs(Answer::Bytes(b"open".to_vec()));
    for i in 1..=3 {
        r.commit_as(
            1_070,
            addr(i),
            observation_commit(&rid, &addr(i), &o, &SALT),
        )
        .expect("commit");
    }
    reg.time = r.cfg.commit_close + 1;
    assert_eq!(
        absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW),
        Ok(vec![])
    );
    let rk = test_keys::receipt_secret();
    let sig = |i: u8| sign_reveal(&test_keys::ordered(i).0, &rid, 0, &o, &SALT).expect("sign");
    let mut take = |r: &mut ObservationRound, now, i: u8, salt, s| {
        take_reveal(r, &mut reg, &fact, &rk, now, addr(i), o.clone(), salt, s)
    };
    assert_eq!(take(&mut r, 1_150, 1, SALT, sig(1)), Ok(true));
    // The same reveal again, and another one: ignored, never an error.
    assert_eq!(take(&mut r, 1_150, 1, SALT, sig(1)), Ok(true));
    assert_eq!(take(&mut r, 1_150, 1, keccak256(b"x"), sig(1)), Ok(true));
    // Observer 2 signs with another key, then a reveal of another salt:
    // non-reveals; its valid reveal is still taken afterwards.
    assert_eq!(take(&mut r, 1_150, 2, SALT, sig(3)), Ok(false));
    assert_eq!(take(&mut r, 1_150, 2, keccak256(b"y"), sig(2)), Ok(false));
    assert_eq!(take(&mut r, 1_150, 2, SALT, sig(2)), Ok(true));
    // Observer 3 reveals after the reveal phase: a non-reveal.
    let late = r.cfg.reveal_close + 1;
    assert_eq!(take(&mut r, late, 3, SALT, sig(3)), Ok(false));
    assert_eq!(reg.log.receipt(&k, 0, &addr(3), &rid), None);
    assert_eq!(
        reg.log.receipt(&k, 0, &addr(1), &rid),
        Some(observation_commit(&rid, &addr(1), &o, &SALT))
    );
    assert_eq!(r.signed_reveals().len(), 2);
    assert_eq!(check_transcript_logged(&r, &reg, &fact), Ok(()));
}

#[test]
fn transcript_complete_calldata_layout() {
    let (r, _, fact) = logged_round();
    let q = r.transcript_query(fact);
    let d = q.calldata();
    // selector, 5 words, offset, length, 3 * 53 bytes padded to 160.
    assert_eq!(d.len(), 4 + 7 * 32 + 160);
    assert_eq!(d[4 + 6 * 32 - 1], 0xc0);
    assert_eq!(d[4 + 7 * 32 - 1], 159);
    assert_eq!(
        &d[4 + 7 * 32..4 + 7 * 32 + 159],
        q.packed_entries().as_slice()
    );
}

#[test]
fn commits_and_reveals_need_the_observers_own_signature() {
    // ATT-R1-F3: the coordinator cannot post or open a commit for an
    // observer whose key it does not hold.
    let mut r = round(EXACT, 3);
    let rid = r.round_id();
    let o = obs(Answer::Bytes(b"open".to_vec()));
    let h = observation_commit(&rid, &addr(1), &o, &SALT);
    let sk1 = test_keys::ordered(1).0;
    let sk2 = test_keys::ordered(2).0;
    let good = sign_commit(&sk1, &rid, 0, &h).expect("sign");
    for bad in [
        [0u8; 65],
        sign_commit(&sk2, &rid, 0, &h).expect("sign"), // another observer's key
        sign_commit(&sk1, &rid, 1, &h).expect("sign"), // another attempt
        sign_commit(&sk1, &keccak256(b"other round"), 0, &h).expect("sign"),
        sign_commit(&sk1, &rid, 0, &keccak256(b"other")).expect("sign"),
    ] {
        assert_eq!(
            r.commit(1_070, addr(1), h, bad),
            Err(ObserveError::BadSignature)
        );
    }
    // A high-s twin of a valid signature is refused (canonical form only).
    let mut twin = good;
    let n = hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
        .expect("hex");
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(n[i]) - i16::from(good[32 + i]) - borrow;
        twin[32 + i] = d.rem_euclid(256) as u8;
        borrow = i16::from(d < 0);
    }
    twin[64] = if good[64] == 27 { 28 } else { 27 };
    assert_eq!(
        r.commit(1_070, addr(1), h, twin),
        Err(ObserveError::BadSignature)
    );
    r.commit(1_070, addr(1), h, good).expect("signed commit");
    assert_eq!(r.signed_commits(), vec![(addr(1), h, good)]);
    let rgood = sign_reveal(&sk1, &rid, 0, &o, &SALT).expect("sign");
    let other = obs(Answer::Bytes(b"shut".to_vec()));
    for bad in [
        [0u8; 65],
        sign_reveal(&sk2, &rid, 0, &o, &SALT).expect("sign"),
        sign_reveal(&sk1, &rid, 1, &o, &SALT).expect("sign"),
        sign_reveal(&sk1, &rid, 0, &other, &SALT).expect("sign"),
        sign_reveal(&sk1, &rid, 0, &o, &keccak256(b"salt")).expect("sign"),
        good,
    ] {
        assert_eq!(
            r.reveal(1_150, addr(1), o.clone(), SALT, bad),
            Err(ObserveError::BadSignature)
        );
    }
    r.reveal(1_150, addr(1), o.clone(), SALT, rgood)
        .expect("signed reveal");
    assert_eq!(r.signed_reveals(), vec![(addr(1), o, SALT, rgood)]);
}

#[test]
fn a_tolerant_majority_is_always_inside_the_band() {
    // Property: if a strict majority's numbers are within `tolerance` of each
    // other, every one of them agrees, whatever the minority reveals.
    let tol: u128 = 50;
    let kind = ObservationKind::Numeric {
        scale: 0,
        tolerance: tol,
    };
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for _ in 0..2_000 {
        let members = 3 + next(9) as usize;
        let q = quorum(members);
        let base = next(10_000) as i128;
        let mut answers: Vec<Answer> = (0..q)
            .map(|_| Answer::Number(base + next(51) as i128))
            .collect();
        let honest = answers.clone();
        for _ in q..members {
            answers.push(match next(3) {
                0 => Answer::Unavailable,
                1 => Answer::Number(next(1_000_000) as i128 - 500_000),
                _ => Answer::Number(base + next(200) as i128 - 100),
            });
        }
        let (outcome, center) = aggregate(kind, members, &answers);
        let ObservationOutcome::Value(v) = outcome else {
            panic!("honest majority got no value: {answers:?}");
        };
        for h in &honest {
            assert_eq!(
                judge(kind, &v, h, center),
                Judgment::Agrees,
                "{h:?} outside band {center}"
            );
        }
    }
}

#[test]
fn settlement_is_independent_of_reveal_order() {
    let answers = [
        (1, Answer::Number(100)),
        (2, Answer::Number(120)),
        (3, Answer::Number(90)),
        (4, Answer::Number(5_000)),
    ];
    let mut a = round(NUM, 4);
    play(&mut a, &answers);
    let mut rev = answers.clone();
    rev.reverse();
    let mut b = round(NUM, 4);
    play(&mut b, &rev);
    assert_eq!(a.settle(1_201), b.settle(1_201));
}

/// L2R-F1: honest answers drift apart within the window (0, 30, 60 with a
/// tolerance of 50). Whichever side a 2-of-5 minority joins, the honest
/// observer at the far edge is between one and two tolerances from the
/// center: no call, never slashed; the minority gains nothing over honesty.
#[test]
fn a_minority_cannot_choose_which_honest_observer_is_slashed() {
    for (attack, edge) in [(60i128, 1u8), (0, 3)] {
        let mut r = round(
            ObservationKind::Numeric {
                scale: 0,
                tolerance: 50,
            },
            5,
        );
        play(
            &mut r,
            &[
                (1, Answer::Number(0)),
                (2, Answer::Number(30)),
                (3, Answer::Number(60)),
                (4, Answer::Number(attack)),
                (5, Answer::Number(attack)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(status_of(&s, edge), ObserverStatus::NoCall);
        assert!(!s.entries().iter().any(|(a, d)| *a == addr(edge) && *d < 0));
        assert!(s
            .statuses
            .iter()
            .all(|(_, st)| *st != ObserverStatus::Outlier));
    }
    // A real liar, beyond twice the tolerance, is still slashed.
    let mut r = round(
        ObservationKind::Numeric {
            scale: 0,
            tolerance: 50,
        },
        5,
    );
    play(
        &mut r,
        &[
            (1, Answer::Number(0)),
            (2, Answer::Number(30)),
            (3, Answer::Number(60)),
            (4, Answer::Number(30)),
            (5, Answer::Number(131)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
}

/// L2R-F2 and L2R3-F4: a lazy majority answering `Unavailable` is paid
/// nothing, and observers who fetched and corroborate each other (two or
/// more values within the tolerance) are not slashed: a lazy cartel cannot
/// censor a feed and slash the honest minority for free.
#[test]
fn an_unavailable_majority_is_paid_nothing() {
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Number(100)),
            (5, Answer::Number(130)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
    assert!(s.entries().is_empty(), "{:?}", s.entries());
    assert_eq!(status_of(&s, 4), ObserverStatus::NoCall);
    assert_eq!(status_of(&s, 5), ObserverStatus::NoCall);
}

/// L2R2-F4 kept under L2R3-F4: a LONE value in an outage (no other value
/// within the tolerance) is a stale guess and pays the small slash, so
/// guessing never beats an honest `Unavailable`. Two lone values that
/// disagree corroborate nothing.
#[test]
fn a_lone_value_in_an_outage_costs_the_small_slash() {
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Number(100)),
            (5, Answer::Number(900)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
    assert_eq!(s.entries(), [(addr(4), -20), (addr(5), -20)]);
    let mut r = round(EXACT, 5);
    play(
        &mut r,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Bytes(b"open".to_vec())),
            (5, Answer::Bytes(b"open".to_vec())),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert!(s.entries().is_empty(), "identical bytes corroborate");
}

/// L2R-F4: a minority inside the band moves the settled value, but only
/// within the honest majority's own range.
#[test]
fn a_minority_in_the_band_stays_inside_the_honest_range() {
    let kind = ObservationKind::Numeric {
        scale: 0,
        tolerance: 50,
    };
    // Honest 100, 100, 150; minority 150, 150.
    let (out, _) = aggregate(kind, 5, &[100, 100, 150, 150, 150].map(Answer::Number));
    let ObservationOutcome::Value(Answer::Number(v)) = out else {
        panic!("no value");
    };
    assert!((100..=150).contains(&v));
}

/// L2R-F5: an observer who saw another's commit hash first and posted it
/// cannot block the owner, and cannot reveal it either.
#[test]
fn front_running_a_seen_commit_hash_fails() {
    let mut r = round(NUM, 3);
    let o = obs(Answer::Number(5));
    let h1 = observation_commit(&r.round_id(), &addr(1), &o, &SALT);
    r.commit_as(1_050, addr(3), h1)
        .expect("griefer posts the hash");
    r.commit_as(1_051, addr(1), h1)
        .expect("the owner still commits");
    r.reveal_as(1_150, addr(1), o.clone(), SALT)
        .expect("owner reveals");
    assert_eq!(
        r.reveal_as(1_150, addr(3), o, SALT),
        Err(ObserveError::HashMismatch)
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(status_of(&s, 3), ObserverStatus::Unrevealed);
}

/// SIG-R1-F1 (known limit, THREATS SEC-R2-F1-P2): a coordinator that
/// withholds two of three honest signed reveals turns a unanimous round
/// into `NoResult` (a re-draw). The withheld observers show `Unrevealed`
/// and are neither paid nor slashed: a missing reveal is the coordinator's
/// word alone. `att_r1_attempts::a_withheld_receipted_reveal_is_caught_on_chain`
/// is the same attack end to end.
#[test]
fn a_withheld_honest_reveal_is_never_slashed() {
    let mut r = round(NUM, 3);
    let o = obs(Answer::Number(5));
    for i in 1..=3 {
        let h = observation_commit(&r.round_id(), &addr(i), &o, &SALT);
        r.commit_as(1_050, addr(i), h).expect("commit");
    }
    r.reveal_as(1_150, addr(1), o, SALT).expect("reveal");
    let s = r.settle(1_201).expect("settle");
    assert_eq!(s.outcome(), &ObservationOutcome::NoResult);
    for i in 2..=3 {
        assert_eq!(status_of(&s, i), ObserverStatus::Unrevealed);
        assert!(s.entries().iter().all(|(a, _)| *a != addr(i)));
    }
}

/// L2R-F6: two rounds of the same feed have different round ids; a reveal
/// copied from round A never opens a commit in round B.
#[test]
fn a_reveal_from_another_round_of_the_same_feed_never_counts() {
    let three: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let a = open_all(spec(NUM), cfg(), three.clone()).expect("a");
    let mut later = cfg();
    later.reveal_close += 1;
    let mut b = open_all(spec(NUM), later, three).expect("b");
    assert_eq!(a.spec_id(), b.spec_id());
    assert_ne!(a.round_id(), b.round_id());
    let o = obs(Answer::Number(77));
    // Observer 1 opened its commit in round A; round B was committed to
    // round A's hash (a copy). It does not open in B.
    let ha = observation_commit(&a.round_id(), &addr(1), &o, &SALT);
    b.commit_as(1_070, addr(1), ha).expect("commit");
    assert_eq!(
        b.reveal_as(1_150, addr(1), o, SALT),
        Err(ObserveError::HashMismatch)
    );
}

/// L2R-F3: a level-2 claim is backed only by the settlement it names, only
/// when that round settled a value, and only for that exact value.
#[test]
fn a_level_two_claim_needs_the_settled_value_of_its_round() {
    let mut r = round(NUM, 3);
    play(
        &mut r,
        &[
            (1, Answer::Number(6_412_345)),
            (2, Answer::Number(6_412_345)),
            (3, Answer::Number(6_412_350)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    let method = VerificationMethod::ReObserved {
        spec: s.spec_id(),
        result: s.result_hash(),
    };
    let value = Value::Fixed {
        mantissa: 6_412_345,
        scale: 2,
    };
    assert_eq!(s.backs(&spec(NUM), &method, &value), Ok(()));
    let refused = |m: &VerificationMethod, v: &Value| s.backs(&spec(NUM), m, v).is_err();
    // Another value, another scale, another kind.
    assert!(refused(
        &method,
        &Value::Fixed {
            mantissa: 6_412_350,
            scale: 2
        }
    ));
    assert!(refused(
        &method,
        &Value::Fixed {
            mantissa: 6_412_345,
            scale: 3
        }
    ));
    assert!(refused(&method, &Value::Int(6_412_345)));
    // A pointer to another result, or to nothing.
    assert!(refused(
        &VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: keccak256(b"made up"),
        },
        &value
    ));
    assert!(refused(&VerificationMethod::OfficialSourceMatch, &value));
    // A round that settled no value backs no claim, whatever it names.
    let mut down = round(NUM, 3);
    play(
        &mut down,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
        ],
    );
    let d = down.settle(1_201).expect("settle");
    let m = VerificationMethod::ReObserved {
        spec: d.spec_id(),
        result: d.result_hash(),
    };
    assert_eq!(
        d.backs(&spec(NUM), &m, &value),
        Err(ObserveError::ClaimNotBacked("the round settled no value"))
    );
    // Exact feeds compare bytes.
    let mut e = round(EXACT, 3);
    play(
        &mut e,
        &[
            (1, Answer::Bytes(b"open".to_vec())),
            (2, Answer::Bytes(b"open".to_vec())),
            (3, Answer::Bytes(b"open".to_vec())),
        ],
    );
    let es = e.settle(1_201).expect("settle");
    let em = VerificationMethod::ReObserved {
        spec: es.spec_id(),
        result: es.result_hash(),
    };
    assert_eq!(
        es.backs(&spec(EXACT), &em, &Value::Bytes(b"open".to_vec())),
        Ok(())
    );
    assert!(es
        .backs(&spec(EXACT), &em, &Value::Bytes(b"shut".to_vec()))
        .is_err());
}

fn registry() -> onyx_core::StaticRegistry {
    let mut r = onyx_core::StaticRegistry::new();
    r.add_metric(
        "example.page.value",
        onyx_core::MetricInfo {
            value_kind: onyx_core::ValueKind::Fixed { scale: 2 },
            needs_label_set: false,
        },
    )
    .expect("metric");
    r.add_entity("entity:example-exchange", onyx_core::EntityKind::Exchange)
        .expect("entity");
    r.add_source(spec(NUM).rights).expect("source");
    let mut other = spec(NUM).rights;
    other.source = SourceId("other.page".into());
    r.add_source(other).expect("other source");
    r
}

fn claim(method: VerificationMethod, mantissa: i128, source: &str) -> Claim {
    let mut rights = spec(NUM).rights;
    rights.source = SourceId(source.into());
    let content_hash = match &method {
        VerificationMethod::ReObserved { result, .. } => *result,
        _ => keccak256(b"page"),
    };
    Claim {
        metric: onyx_core::claim::MetricId("example.page.value".into()),
        subject: onyx_core::claim::Subject::Entity("entity:example-exchange".into()),
        value: Value::Fixed { mantissa, scale: 2 },
        source: onyx_core::SourceRef {
            source: SourceId(source.into()),
            snapshot: onyx_core::Snapshot::Document {
                published_unix: WINDOW.1,
                content_hash,
            },
        },
        method,
        label_set: None,
        rights,
        evidence: vec![],
        confidence: None,
    }
}

fn settled_into(store: &mut ObservationResults, answers: &[(u8, Answer)]) -> ObservationSettlement {
    let n = answers.len() as u8;
    let pool: Vec<Observer> = (1..=n)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut sp = spec(NUM);
    sp.min_observers = u16::from(n);
    let mut r = open_in(store, sp, cfg(), pool).expect("open in the store");
    play(&mut r, answers);
    store.settle(&r, 1_201).expect("settle into the store")
}

/// L2R2-F1: a claim is accepted only through the store, and only for a
/// result this coordinator settled itself: a settlement from anywhere else,
/// an unknown pointer, another value, a round with no value, or another
/// source's rights are all refused.
#[test]
fn claims_are_accepted_only_for_results_settled_in_this_store() {
    let reg = registry();
    let mut store = store_for(NUM);
    let good = settled_into(
        &mut store,
        &[
            (1, Answer::Number(12_345)),
            (2, Answer::Number(12_345)),
            (3, Answer::Number(12_350)),
        ],
    );
    let m = |s: &ObservationSettlement| VerificationMethod::ReObserved {
        spec: s.spec_id(),
        result: s.result_hash(),
    };
    assert_eq!(
        store.accept_claim(&claim(m(&good), 12_345, "example.page"), &reg),
        Ok(onyx_core::ProofLevel::ReObserved)
    );
    // Another value of the same result.
    assert!(store
        .accept_claim(&claim(m(&good), 12_350, "example.page"), &reg)
        .is_err());
    // A pointer this store never settled: made up, or settled by another
    // coordinator's store (a "forged" settlement never reaches this one).
    let mut elsewhere = store_for_n(NUM, 4);
    let foreign = settled_into(
        &mut elsewhere,
        &[
            (1, Answer::Number(999)),
            (2, Answer::Number(999)),
            (3, Answer::Number(999)),
            (4, Answer::Number(999)),
        ],
    );
    assert_eq!(
        store.accept_claim(&claim(m(&foreign), 999, "example.page"), &reg),
        Err(ObserveError::ClaimNotBacked(
            "no round settled with this result"
        ))
    );
    let made_up = VerificationMethod::ReObserved {
        spec: good.spec_id(),
        result: keccak256(b"made up"),
    };
    assert!(store
        .accept_claim(&claim(made_up, 12_345, "example.page"), &reg)
        .is_err());
    // A round that settled no value backs nothing (its own store: this
    // store already holds a result for the feed, L2R3-F5).
    let mut downs = store_for_n(NUM, 5);
    let down = settled_into(
        &mut downs,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Unavailable),
            (5, Answer::Unavailable),
        ],
    );
    assert_eq!(
        downs.accept_claim(&claim(m(&down), 0, "example.page"), &reg),
        Err(ObserveError::ClaimNotBacked("the round settled no value"))
    );
    // Registered rights, but not the feed's source.
    assert_eq!(
        store.accept_claim(&claim(m(&good), 12_345, "other.page"), &reg),
        Err(ObserveError::ClaimNotBacked("source is not the feed's"))
    );
    // A malformed claim fails validation first.
    let zero = VerificationMethod::ReObserved {
        spec: good.spec_id(),
        result: Hash32::ZERO,
    };
    assert!(matches!(
        store.accept_claim(&claim(zero, 12_345, "example.page"), &reg),
        Err(ObserveError::Claim(_))
    ));
    assert_eq!(store.get(&good.result_hash()), Some(&good));
}

/// L2SRC-R2-F1: rounds open only for a registered metric and only as its
/// template; registration needs this store's admin, is recorded, and is
/// final.
#[test]
fn feeds_open_only_as_their_registered_template() {
    let (mut store, admin) = ObservationResults::new();
    let three = || {
        (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        open_in(&mut store, spec(NUM), cfg(), three()).err(),
        Some(ObserveError::UnknownFeed)
    );
    let (_other, other_admin) = ObservationResults::new();
    let t = template_of_spec(&spec(NUM));
    assert_eq!(
        store.register_feed(&other_admin, t.clone(), "x", 1),
        Err(ObserveError::NotFeedAdmin)
    );
    store
        .register_feed(&admin, t.clone(), "reviewed", 7)
        .expect("register");
    assert_eq!(
        store.register_feed(&admin, t.clone(), "again", 8),
        Err(ObserveError::FeedAlreadyRegistered)
    );
    assert_eq!(store.feed_decisions().len(), 1);
    assert_eq!(store.feed_decisions()[0].at, 7);
    assert_eq!(store.feed_decisions()[0].template, t.id());
    assert!(open_in(&mut store, spec(NUM), cfg(), three()).is_ok());
    for (name, f) in [
        ("fetcher", {
            let mut f = spec(NUM);
            f.fetcher = keccak256(b"another fetcher");
            f
        }),
        ("url", {
            let mut f = spec(NUM);
            f.url = "https://evil.example/page.json".into();
            f
        }),
        ("selector", {
            let mut f = spec(NUM);
            f.selector = "/data/other".into();
            f
        }),
        ("window", {
            let mut f = spec(NUM);
            f.window_end += 1;
            f
        }),
        ("observers", {
            let mut f = spec(NUM);
            f.min_observers = 2;
            f
        }),
    ] {
        assert!(
            matches!(
                open_in(&mut store, f, cfg(), three()),
                Err(ObserveError::Template(_))
            ),
            "{name}"
        );
    }
}

/// L2R2-F1: a round is settled into the store once, so its entries are
/// produced once.
#[test]
fn a_round_settles_into_the_store_once() {
    let mut store = store_for(NUM);
    let pool: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut r = open_in(&mut store, spec(NUM), cfg(), pool).expect("open");
    play(
        &mut r,
        &[
            (1, Answer::Number(5)),
            (2, Answer::Number(5)),
            (3, Answer::Number(5)),
        ],
    );
    store.settle(&r, 1_201).expect("first");
    assert_eq!(store.settle(&r, 1_202), Err(ObserveError::AlreadySettled));
}

/// Review REVLOG-R3-3: a retry after the round settled (the run died
/// after `settle`) gets the recorded settlement back from
/// `settle_once`; the same round id with other reveals stays refused.
#[test]
fn settle_once_is_idempotent_only_for_the_same_result() {
    let mut store = store_for(NUM);
    let pool: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut r = open_in(&mut store, spec(NUM), cfg(), pool).expect("open");
    play(
        &mut r,
        &[
            (1, Answer::Number(5)),
            (2, Answer::Number(5)),
            (3, Answer::Number(7)),
        ],
    );
    let first = store.settle_once(&r, 1_201).expect("first");
    assert_eq!(store.settle_once(&r, 1_201), Ok(first.clone()), "a retry");
    let mut other = r.clone();
    other.reveals.remove(&addr(3));
    assert_eq!(
        store.settle_once(&other, 1_201),
        Err(ObserveError::AlreadySettled),
        "another result for the same round"
    );
}

/// Review REVLOG-R3-2: an unfinished round travels with the book
/// (version 3) into a fresh store, which then refuses a new draw for its
/// fact and hands the same round back; a tampered round is refused at
/// load, never trusted from bytes.
#[test]
fn an_unfinished_round_travels_with_the_book_and_is_rechecked() {
    let mut store = store_for(NUM);
    let pool: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut r = open_in(&mut store, spec(NUM), cfg(), pool.clone()).expect("open");
    play(&mut r, &[(1, Answer::Number(5))]);
    r.log = Some(LogSnapshot {
        block: Some(LogBlock {
            number: 40,
            time: r.cfg.commit_close + 1,
        }),
        posted: vec![],
    });
    let draw = DrawRecord {
        chain_id: 31_337,
        registry: [0; 20],
        attempt: 0,
        draw_count: 1,
        announced_at: 1,
        commit: draw_commit(&r.draw.pool, &r.spec_id, &cfg_hash(&r.cfg)),
        beacon_height: r.draw.beacon_height,
        beacon: r.draw.beacon,
    };
    store
        .keep_unfinished(UnfinishedRound {
            round: r.clone(),
            onchain_draw: draw,
            reveals: vec![],
        })
        .expect("kept");
    assert_eq!(
        store
            .announce_draw(&spec(NUM), &cfg(), &pool, 99, &SimulatedChain::at(0))
            .err(),
        Some(ObserveError::RoundUnfinished),
        "no new draw while the round is unfinished"
    );
    let book = store.non_value_book().expect("writable");
    assert_eq!(book["version"], 8);
    let mut fresh = store_for(NUM);
    fresh.absorb_non_value_book(&book).expect("loads");
    let back = fresh.kept_round(&spec(NUM)).expect("the same round");
    assert_eq!(back.round.round_id, r.round_id);
    assert_eq!(back.round.log, r.log);
    assert_eq!(back.round.signed_reveals(), r.signed_reveals());
    // The restored round settles in the fresh store (its draw restored).
    fresh
        .settle(&back.round, 1_201)
        .expect("settles after a restart");
    // Tampered: one byte of the saved round flipped.
    let mut bad = book.clone();
    let h = bad["unfinished"][0].as_str().expect("hex").to_owned();
    let mut b = hex::decode(&h[2..]).expect("hex");
    let n = b.len();
    b[n - 40] ^= 1;
    bad["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b)));
    assert!(store_for(NUM).absorb_non_value_book(&bad).is_err());
    // Review REVLOG-R4-3: a version-3 book is still read; its reveals
    // carry no arrival time and come back at `commit_close + 1`, the time
    // the version-3 runners took every reveal at.
    let sr = r.signed_reveals();
    assert_eq!(sr.len(), 1);
    let mut v3 = book.clone();
    v3["version"] = 3.into();
    let mut bytes = borsh::to_vec(&r).expect("round");
    // ROADMAP C11: a version-6 round ends with its sealed snapshot
    // (`None`, one byte); books up to version 5 have none.
    assert_eq!(bytes.pop(), Some(0));
    let v5_round = bytes.clone();
    let v7_round = bytes.clone();
    bytes.extend(borsh::to_vec(&draw).expect("draw"));
    bytes.extend(borsh::to_vec(&sr).expect("reveals"));
    v3["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(bytes)));
    // A version-5 book (no sealed snapshot) still loads, unchanged.
    let mut v5 = book.clone();
    v5["version"] = 5.into();
    let mut b5 = v5_round;
    b5.extend(borsh::to_vec(&draw).expect("draw"));
    b5.extend(borsh::to_vec(&Vec::<ArrivedReveal>::new()).expect("reveals"));
    v5["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b5)));
    let mut five = store_for(NUM);
    five.absorb_non_value_book(&v5)
        .expect("a version-5 book loads");
    let back5 = five.kept_round(&spec(NUM)).expect("kept");
    assert_eq!(back5.round.round_id, r.round_id);
    assert!(back5.round.sealed.is_none());
    // ROADMAP C52: a version-7 book loads (a round without a time-lock
    // has the same bytes), but a version-7 TIME-LOCKED snapshot (its
    // posts carry no far-date copy; never deployed) is refused.
    let mut v7 = book.clone();
    v7["version"] = 7.into();
    store_for(NUM)
        .absorb_non_value_book(&v7)
        .expect("a version-7 book without a time-lock loads");
    #[derive(BorshSerialize)]
    struct V7Read {
        lock_round: u64,
        signature: Vec<u8>,
        openings: Vec<(MinerAddress, crate::timelock::TlockOpening)>,
    }
    #[derive(BorshSerialize)]
    struct V7Snap {
        block: LogBlock,
        reveal_key: crate::sealed::Point,
        posts: Vec<(MinerAddress, PostedSealed)>,
        dismissals: Vec<(MinerAddress, crate::sealed::Dismissal)>,
        timelock: Option<V7Read>,
    }
    let snap7 = |timelock| V7Snap {
        block: LogBlock {
            number: 9,
            time: r.cfg.reveal_close + 1,
        },
        reveal_key: [7; 64],
        posts: vec![],
        dismissals: vec![],
        timelock,
    };
    for (timelock, loads) in [
        (None, true),
        (
            Some(V7Read {
                lock_round: 1,
                signature: vec![1; 48],
                openings: vec![],
            }),
            false,
        ),
    ] {
        let mut b7 = v7_round.clone();
        b7.push(1);
        b7.extend(borsh::to_vec(&snap7(timelock)).expect("snapshot"));
        b7.extend(borsh::to_vec(&draw).expect("draw"));
        b7.extend(borsh::to_vec(&Vec::<ArrivedReveal>::new()).expect("reveals"));
        let mut book7 = v7.clone();
        book7["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b7)));
        let got = store_for(NUM).absorb_non_value_book(&book7);
        if loads {
            // It decodes; the snapshot's own checks then judge it (a
            // made-up key and no log here, so it is refused there).
            assert!(!format!("{got:?}").contains("far-date copy"), "{got:?}");
        } else {
            let e = got.expect_err("a version-7 time-locked round is refused");
            assert!(format!("{e}").contains("far-date copy"), "{e}");
        }
    }
    let mut old = store_for(NUM);
    old.absorb_non_value_book(&v3)
        .expect("a version-3 book loads");
    let back = old.kept_round(&spec(NUM)).expect("kept");
    assert_eq!(
        back.reveals,
        vec![ArrivedReveal {
            reveal: sr[0].clone(),
            arrived: r.cfg.commit_close + 1,
        }]
    );
}

/// A drawn round (observers 1..=3, each committed at 1_070 to `Number(5)`)
/// kept unfinished in `store`, and each observer's signed reveal.
fn kept_round(store: &mut ObservationResults) -> (UnfinishedRound, Vec<SignedReveal>) {
    let pool: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut r = open_in(store, spec(NUM), cfg(), pool).expect("open");
    let mut reveals = vec![];
    for i in 1..=3 {
        let o = obs(Answer::Number(5));
        let h = observation_commit(&r.round_id(), &addr(i), &o, &SALT);
        r.commit_as(1_070, addr(i), h).expect("commit");
        let sk = test_keys::ordered_secret(&addr(i)).expect("key");
        let sig = sign_reveal(&sk, &r.round_id, r.draw.attempt, &o, &SALT).expect("sig");
        reveals.push((addr(i), o, SALT, sig));
    }
    let u = UnfinishedRound {
        onchain_draw: DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: 0,
            draw_count: 1,
            announced_at: 1,
            commit: draw_commit(&r.draw.pool, &r.spec_id, &cfg_hash(&r.cfg)),
            beacon_height: r.draw.beacon_height,
            beacon: r.draw.beacon,
        },
        round: r,
        reveals: vec![],
    };
    store.keep_unfinished(u.clone()).expect("kept");
    (u, reveals)
}

/// Review REVLOG-R4-3: a reveal is stamped and phase-checked when it
/// ARRIVES (the coordinator's clock), so a receipt signed long after
/// `reveal_close` (the finalized-block wait) still takes it; a reveal
/// arriving outside the reveal phase, unsigned, from someone not drawn,
/// or a second one is a non-reveal.
#[test]
fn a_reveal_is_checked_at_its_arrival_not_at_its_receipt() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let (close, end) = (cfg().commit_close, cfg().reveal_close);
    assert!(
        !u.receive_reveal(reveals[0].clone(), close),
        "still the commit phase"
    );
    assert!(
        !u.receive_reveal(reveals[0].clone(), end + 1),
        "after reveal_close"
    );
    let mut forged = reveals[0].clone();
    forged.3 = reveals[1].3;
    assert!(
        !u.receive_reveal(forged, close + 1),
        "not the observer's signature"
    );
    let mut stranger = reveals[0].clone();
    stranger.0 = addr(9);
    assert!(!u.receive_reveal(stranger, close + 1), "not drawn");
    assert!(u.receive_reveal(reveals[0].clone(), close + 1));
    assert!(
        !u.receive_reveal(reveals[0].clone(), close + 2),
        "a second one"
    );
    assert!(
        u.receive_reveal(reveals[1].clone(), end),
        "on the last second"
    );
    assert_eq!(
        u.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
        vec![close + 1, end]
    );
    // The receipts wait for a finalized block 1,178 s past the commit
    // phase (Base Sepolia, measured): taken at their arrival, both count.
    let mut reg = SimulatedRegistry::new();
    let fact = store.onchain_fact(&spec(NUM)).expect("fact");
    reg.log
        .signers
        .insert((fact.registry_key(), 0), test_keys::receipt_signer());
    reg.time = close + 1_178;
    let mut round = u.round.clone();
    finish_reveals(
        &mut round,
        &mut reg,
        &fact,
        &test_keys::receipt_secret(),
        &u.reveals,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("finished");
    assert_eq!(
        round.reveals.len(),
        2,
        "both reveals taken after reveal_close"
    );
    // Taken at the receipt's time instead, the same reveal is late.
    let mut late = u.round.clone();
    late.log = round.log.clone();
    let (a, o, salt, sig) = reveals[2].clone();
    assert_eq!(
        take_reveal(
            &mut late,
            &mut reg,
            &fact,
            &test_keys::receipt_secret(),
            close + 1_178,
            a,
            o,
            salt,
            sig
        ),
        Ok(false)
    );
}

/// A test inbox: each reveal reaches the coordinator at its time.
struct Timed(Vec<(u64, SignedReveal)>);

impl RevealInbox for Timed {
    fn take(&mut self, _: &Hash32, _: &[MinerAddress], now: u64) -> Vec<SignedReveal> {
        let (due, later) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(|(t, _)| *t <= now);
        self.0 = later;
        due.into_iter().map(|(_, r)| r).collect()
    }
}

/// `store` saved to a book and read back into a fresh store (a
/// restart), with its kept round.
fn restarted(store: &ObservationResults) -> (ObservationResults, UnfinishedRound) {
    let book = store.non_value_book().expect("writable");
    let mut fresh = store_for(NUM);
    fresh.absorb_non_value_book(&book).expect("the book loads");
    let back = fresh.kept_round(&spec(NUM)).expect("kept");
    (fresh, back)
}

/// THREATS REVLOG-R5-1d (ROADMAP B5): a coordinator restarted while the
/// round's reveal window is open keeps taking reveals. The window (the
/// round's config) and the reveal taken before the crash come back from
/// the book; reveals sent after the restart are taken by the clock until
/// `reveal_close`, each kept and saved before the next; the intake stops
/// once every drawn observer revealed. A replayed reveal keeps the first
/// (and its arrival time); a forged one is a non-reveal.
#[test]
fn a_restarted_round_keeps_taking_reveals_while_its_window_is_open() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let (close, end) = (cfg().commit_close, cfg().reveal_close);
    assert!(u.receive_reveal(reveals[0].clone(), close + 1));
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    assert_eq!(back.round.config(), cfg(), "the window comes back");
    assert_eq!(back.reveals.len(), 1, "the reveal taken before the crash");
    let mut forged = reveals[2].clone();
    forged.3 = reveals[1].3;
    let mut inbox = Timed(vec![
        (close + 30, reveals[0].clone()), // a replay
        (close + 30, forged),
        (close + 30, reveals[1].clone()),
        // Read on the poll step (every REVEAL_POLL_SECS from close + 30).
        (end - 6, reveals[2].clone()),
    ]);
    let clock = ReplayClock::at(close + 30);
    let mut saved = vec![];
    let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |s| {
        saved.push(s.kept_round(&spec(NUM)).map_or(0, |k| k.reveals.len()));
        Ok(())
    })
    .expect("the intake runs");
    assert_eq!(taken, 2);
    assert_eq!(saved, vec![2, 3], "each reveal saved before the next");
    assert_eq!(
        back.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
        vec![close + 1, close + 30, end - 6],
        "the replay kept the first arrival"
    );
    assert_eq!(
        clock.now(),
        end - 6,
        "every drawn observer revealed: no wait"
    );
    assert_eq!(
        fresh.kept_round(&spec(NUM)).map(|k| k.reveals),
        Some(back.reveals.clone()),
        "the store keeps what was taken"
    );
    // The receipts come later: every reveal counts at its arrival.
    let mut reg = SimulatedRegistry::new();
    let fact = fresh.onchain_fact(&spec(NUM)).expect("fact");
    reg.log
        .signers
        .insert((fact.registry_key(), 0), test_keys::receipt_signer());
    reg.time = close + 1_178;
    let mut round = back.round.clone();
    finish_reveals(
        &mut round,
        &mut reg,
        &fact,
        &test_keys::receipt_secret(),
        &back.reveals,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("finished");
    assert_eq!(round.reveals.len(), 3, "no reveal lost to the restart");
}

/// Review B5-R2-1: the book's reveals are not re-checked when it loads
/// (`restore_unfinished`), and the intake refuses a second reveal from
/// an observer and stops once every drawn observer has one. A saved
/// entry that is not a reveal (a corrupted or edited book: a forged
/// signature, an arrival outside the window) must not hold that
/// observer's slot: after a restart the observer's real reveal, sent
/// again inside the window, is taken, and the book keeps only checked
/// reveals.
#[test]
fn a_bad_reveal_in_the_book_does_not_hold_the_observers_slot() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let (close, end) = (cfg().commit_close, cfg().reveal_close);
    assert!(u.receive_reveal(reveals[0].clone(), close + 1));
    let mut forged = reveals[1].clone();
    forged.3 = reveals[0].3;
    u.reveals.push(ArrivedReveal {
        reveal: forged,
        arrived: close + 1,
    });
    u.reveals.push(ArrivedReveal {
        reveal: reveals[2].clone(),
        arrived: end + 5,
    });
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    assert_eq!(back.reveals.len(), 3, "the book loads as saved");
    let mut inbox = Timed(vec![
        (close + 30, reveals[1].clone()),
        (close + 30, reveals[2].clone()),
    ]);
    let clock = ReplayClock::at(close + 30);
    let mut saved = vec![];
    let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |s| {
        saved.push(s.kept_round(&spec(NUM)).map_or(0, |k| k.reveals.len()));
        Ok(())
    })
    .expect("the intake runs");
    assert_eq!(taken, 2, "both observers' real reveals are taken");
    assert_eq!(saved, vec![1, 2, 3], "the bad entries leave the book first");
    assert_eq!(
        back.reveals
            .iter()
            .map(|a| (a.reveal.clone(), a.arrived))
            .collect::<Vec<_>>(),
        vec![
            (reveals[0].clone(), close + 1),
            (reveals[1].clone(), close + 30),
            (reveals[2].clone(), close + 30),
        ]
    );
}

/// REVLOG-R5-1d, bounded: an observer that never reveals (a griefer)
/// holds the intake only until `reveal_close`; a reveal reaching the
/// coordinator after it is a non-reveal. A restart after the window took
/// nothing and does not wait.
#[test]
fn the_restarted_intake_ends_at_reveal_close() {
    let mut store = store_for(NUM);
    let (u, reveals) = kept_round(&mut store);
    let (close, end) = (cfg().commit_close, cfg().reveal_close);
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    let mut inbox = Timed(vec![
        (close + 10, reveals[0].clone()),
        (end, reveals[1].clone()),
        (end + 1, reveals[2].clone()),
    ]);
    let clock = ReplayClock::at(close + 10);
    let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |_| Ok(()))
        .expect("the intake runs");
    assert_eq!(taken, 2, "on time: the first two; the last one late");
    assert_eq!(clock.now(), end + 1, "waited out the window, no longer");
    assert_eq!(
        back.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
        vec![close + 10, end]
    );
    // After the window: nothing taken, no wait.
    let (mut again, mut late) = restarted(&store);
    let clock = ReplayClock::at(end + 50);
    let mut inbox = vec![reveals[0].clone()];
    assert_eq!(
        keep_taking_reveals(&mut again, &mut late, &mut inbox, &clock, &mut |_| Ok(())),
        Ok(0)
    );
    assert_eq!(clock.now(), end + 50);
    assert!(late.reveals.is_empty());
}

/// REVLOG-R5-1d: a round settled in this store (waiting for its results
/// to be written, REVLOG-R4-1) takes no more reveals, whatever the clock:
/// its result is fixed. A failed save stops the intake with the reveal
/// not counted as saved (the caller keeps the round unfinished).
#[test]
fn a_settled_round_takes_no_more_reveals_and_a_failed_save_stops_the_intake() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let close = cfg().commit_close;
    assert!(u.receive_reveal(reveals[0].clone(), close + 1));
    assert!(u.receive_reveal(reveals[1].clone(), close + 1));
    let mut r = u.round.clone();
    for ar in &u.reveals {
        let (a, o, s, sig) = ar.reveal.clone();
        r.reveal(close + 1, a, o, s, sig).expect("reveal");
    }
    store.settle(&r, cfg().reveal_close + 1).expect("settles");
    u.round = r;
    let mut inbox = vec![reveals[2].clone()];
    let clock = ReplayClock::at(close + 2);
    assert_eq!(
        keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
        Ok(0)
    );
    assert_eq!(inbox.len(), 1, "the inbox is not read for a settled round");
    // A failed save: Err, so the run keeps the round unfinished.
    let mut store = store_for(NUM);
    let (u, reveals) = kept_round(&mut store);
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    let mut inbox = vec![reveals[0].clone()];
    let e = keep_taking_reveals(
        &mut fresh,
        &mut back,
        &mut inbox,
        &ReplayClock::at(close + 1),
        &mut |_| Err("disk full".into()),
    )
    .expect_err("the save failed");
    assert!(e.contains("disk full"), "{e}");
}

/// Review B5-M1: once the round holds its log snapshot (saved before the
/// first receipt), a restart takes no reveal, even with the clock back
/// inside the window (a replayed clock starts at 0, a wall clock can step
/// back): its receipts, settlement or on-chain record may exist. Review
/// B5-L2: an inbox that cannot deliver is not waited on.
#[test]
fn a_round_with_its_log_snapshot_takes_no_reveal_after_a_restart() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let close = cfg().commit_close;
    assert!(u.receive_reveal(reveals[0].clone(), close + 1));
    u.round.log = Some(LogSnapshot {
        block: None,
        posted: vec![],
    });
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    assert!(
        back.round.log_snapshot().is_some(),
        "the snapshot is in the book"
    );
    let clock = ReplayClock::at(close + 1);
    let mut inbox = vec![reveals[2].clone()];
    assert_eq!(
        keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |_| Ok(())),
        Ok(0)
    );
    assert_eq!(inbox.len(), 1, "not read");
    assert_eq!(back.reveals.len(), 1);
    // NoInbox: nothing can come, so the window is not waited out.
    let mut store = store_for(NUM);
    let (u, _) = kept_round(&mut store);
    store.keep_unfinished(u).expect("kept");
    let (mut fresh, mut back) = restarted(&store);
    let clock = ReplayClock::at(close + 1);
    assert_eq!(
        keep_taking_reveals(&mut fresh, &mut back, &mut NoInbox, &clock, &mut |_| Ok(())),
        Ok(0)
    );
    assert_eq!(
        clock.now(),
        close + 1,
        "no wait for an inbox that cannot deliver"
    );
}

/// Review REVLOG-R4-1: a settled round stays kept until its results are
/// written; the book is not saved meanwhile (on disk it holds the round
/// unsettled); only a settled round is dropped as published.
#[test]
fn a_settled_round_leaves_the_book_only_once_published() {
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let dir = std::env::temp_dir().join(format!("onyx-r4-pub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("book.json");
    assert_eq!(
        store.published(&spec(NUM), &u.round.round_id),
        Err(ObserveError::RoundNotSettled),
        "an unsettled round is never dropped"
    );
    store.save_book(&path).expect("an unsettled round is saved");
    for r in reveals {
        u.round
            .reveal(cfg().commit_close + 1, r.0, r.1, r.2, r.3)
            .expect("reveal");
    }
    store.settle(&u.round, 1_201).expect("settles");
    assert!(store.unfinished(&spec(NUM)).is_some(), "still kept");
    assert!(store.awaiting_publication());
    let err = store.save_book(&path).expect_err("not saved meanwhile");
    assert!(err.contains("waits for its results"), "{err}");
    let mut fresh = store_for(NUM);
    fresh.load_book(&path).expect("the saved book");
    assert!(
        fresh.unfinished(&spec(NUM)).is_some(),
        "on disk the round is unsettled: a restart publishes it again"
    );
    store
        .published(&spec(NUM), &u.round.round_id)
        .expect("published");
    assert!(store.unfinished(&spec(NUM)).is_none());
    store.save_book(&path).expect("saved once published");
    let _ = std::fs::remove_dir_all(&dir);
}

/// SEC-R1-F1: only a value outcome is recorded on chain, and a fixed
/// subject's window is closed by any outcome (a dated or period subject
/// stays open after a non-value round: `onyx_source_edgar` tests).
#[test]
fn only_value_outcomes_are_recorded_on_chain_and_fixed_windows_close() {
    let mut store = store_for(NUM);
    let three: Vec<Observer> = (1..=3)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut out = open_in(&mut store, spec(NUM), cfg(), three.clone()).expect("open");
    play(
        &mut out,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
        ],
    );
    let s = store.settle(&out, 1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
    assert_eq!(store.onchain_record(&s.result_hash()), None);
    assert_eq!(
        open_in(&mut store, spec(NUM), cfg(), three).err(),
        Some(ObserveError::FeedAlreadySettled),
        "a fixed subject's window is one fact, closed by any outcome"
    );
    let mut fresh = store_for(NUM);
    let v = settled_into(
        &mut fresh,
        &[
            (1, Answer::Number(5)),
            (2, Answer::Number(5)),
            (3, Answer::Number(5)),
        ],
    );
    let rec = fresh.onchain_record(&v.result_hash()).expect("a value");
    assert_eq!(rec.result, v.result_hash());
    assert_eq!(rec.spec_id, v.spec_id());
}

/// L2R2-F6: `backs` takes the feed and checks its id, so a caller cannot
/// read a scale-2 value (123.45) as the integer 12345.
#[test]
fn backs_reads_kind_and_scale_from_the_feed_it_checks() {
    let mut r = round(NUM, 3);
    play(
        &mut r,
        &[
            (1, Answer::Number(12_345)),
            (2, Answer::Number(12_345)),
            (3, Answer::Number(12_345)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    let m = VerificationMethod::ReObserved {
        spec: s.spec_id(),
        result: s.result_hash(),
    };
    let int_feed = spec(ObservationKind::Numeric {
        scale: 0,
        tolerance: 50,
    });
    assert_eq!(
        s.backs(&int_feed, &m, &Value::Int(12_345)),
        Err(ObserveError::ClaimNotBacked(
            "feed is not this round's feed"
        ))
    );
    assert_eq!(
        s.backs(
            &spec(NUM),
            &m,
            &Value::Fixed {
                mantissa: 12_345,
                scale: 2
            }
        ),
        Ok(())
    );
}

/// L2R2-F4: during an outage a stale guess costs the small slash, so
/// guessing never beats an honest `Unavailable` (which is never slashed).
#[test]
fn a_stale_guess_in_an_outage_costs_the_small_slash() {
    let mut r = round(NUM, 5);
    play(
        &mut r,
        &[
            (1, Answer::Unavailable),
            (2, Answer::Unavailable),
            (3, Answer::Unavailable),
            (4, Answer::Unavailable),
            (5, Answer::Number(100)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
    assert_eq!(status_of(&s, 5), ObserverStatus::Missed);
    assert_eq!(status_of(&s, 1), ObserverStatus::NoCall);
    assert_eq!(s.entries(), [(addr(5), -20)]);
}

/// L2R2-F5: with honest answers drifting between one and two tolerances,
/// a far minority that forces `NoResult` is still slashed, because a
/// strict majority clusters within twice the tolerance. An honest split
/// with no such majority judges nobody.
#[test]
fn a_far_minority_cannot_force_a_free_no_result() {
    let kind = ObservationKind::Numeric {
        scale: 0,
        tolerance: 50,
    };
    let mut r = round(kind, 5);
    play(
        &mut r,
        &[
            (1, Answer::Number(0)),
            (2, Answer::Number(40)),
            (3, Answer::Number(90)),
            (4, Answer::Number(10_000)),
            (5, Answer::Number(10_000)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
    for i in 1..=3 {
        assert_eq!(status_of(&s, i), ObserverStatus::NoCall);
    }
    assert_eq!(status_of(&s, 4), ObserverStatus::Outlier);
    assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
    assert_eq!(s.entries(), [(addr(4), -100), (addr(5), -100)]);
    // 2 vs 2 with one silent member: no strict majority clusters, so the
    // far side is not judged (it may be the honest side).
    let mut split = round(kind, 5);
    play(
        &mut split,
        &[
            (1, Answer::Number(0)),
            (2, Answer::Number(0)),
            (3, Answer::Number(1_000)),
            (4, Answer::Number(1_000)),
        ],
    );
    let s = split.settle(1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
    assert!(s
        .statuses()
        .iter()
        .all(|(_, st)| *st != ObserverStatus::Outlier));
}

/// L2R2-F5 property: an honest strict majority whose numbers lie within
/// twice the tolerance of each other is never slashed as an outlier,
/// whatever the minority reveals, with or without a settled value.
#[test]
fn an_honest_majority_within_twice_the_tolerance_is_never_an_outlier() {
    let tol: u128 = 50;
    let kind = ObservationKind::Numeric {
        scale: 0,
        tolerance: tol,
    };
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for _ in 0..500 {
        let members = 3 + next(7) as u8;
        let q = quorum(usize::from(members)) as u8;
        let base = next(10_000) as i128;
        let mut answers: Vec<(u8, Answer)> = (1..=q)
            .map(|i| (i, Answer::Number(base + next(2 * tol as u64 + 1) as i128)))
            .collect();
        for i in q + 1..=members {
            answers.push((
                i,
                match next(3) {
                    0 => Answer::Unavailable,
                    1 => Answer::Number(next(1_000_000) as i128 - 500_000),
                    _ => Answer::Number(base + next(400) as i128 - 200),
                },
            ));
        }
        let mut r = round(kind, members);
        play(&mut r, &answers);
        let s = r.settle(1_201).expect("settle");
        for i in 1..=q {
            assert_ne!(status_of(&s, i), ObserverStatus::Outlier, "{answers:?}");
        }
    }
}

fn pool_of(n: u8, bond: u128) -> Vec<Observer> {
    (1..=n)
        .map(|i| Observer {
            address: addr(i),
            bond,
        })
        .collect()
}

/// L2-F2: the observers are the draw of the announced pool and beacon,
/// never a set the coordinator chose; a pool member who was not drawn
/// cannot commit, and a consumer refuses a hand-picked set.
#[test]
fn observers_are_the_draw_and_a_hand_picked_set_is_refused() {
    let mut store = store_for(NUM);
    let pool = pool_of(9, 1_000);
    let snapshot = store
        .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
        .expect("announce");
    let mut r = store
        .open_round(
            spec(NUM),
            cfg(),
            snapshot.clone(),
            &SimulatedChain::with_beacon(beacon()),
        )
        .expect("open");
    let drawn: Vec<MinerAddress> = r.observers().iter().map(|o| o.address).collect();
    assert_eq!(drawn.len(), 3);
    let again = draw_observers(&spec(NUM).id(), &snapshot, &r.draw(), 3, 100);
    assert_eq!(again, r.observers(), "the draw is a pure function");
    let outsider = (1..=9u8)
        .map(addr)
        .find(|a| !drawn.contains(a))
        .expect("one");
    let h = observation_commit(&r.round_id(), &outsider, &obs(Answer::Number(1)), &SALT);
    assert_eq!(
        r.commit_as(1_070, outsider, h),
        Err(ObserveError::NotObserver)
    );
    // A consumer recomputes the draw and refuses any other set.
    let mut picked = r.observers().to_vec();
    picked[0] = Observer {
        address: outsider,
        bond: 1_000,
    };
    picked.sort_by_key(|o| o.address);
    assert_eq!(
        check_draw(&spec(NUM), &cfg(), &snapshot, &r.draw(), &picked),
        Err(ObserveError::NotTheDraw)
    );
    assert_eq!(
        check_draw(&spec(NUM), &cfg(), &snapshot, &r.draw(), r.observers()),
        Ok(())
    );
    // Another beacon draws another set (with overwhelming probability).
    let other = DrawSeed {
        beacon: keccak256(b"other beacon"),
        ..r.draw()
    };
    assert_ne!(
        draw_observers(&spec(NUM).id(), &snapshot, &other, 3, 100),
        drawn
            .iter()
            .map(|a| Observer {
                address: *a,
                bond: 1_000
            })
            .collect::<Vec<_>>()
    );
}

/// L2-F2 grinding: one announcement per fact and attempt, a beacon height
/// that does not exist yet, and an open that must match the announcement;
/// a draw opens once.
#[test]
fn a_draw_is_announced_once_before_its_beacon_and_opens_once() {
    let mut store = store_for(NUM);
    let pool = pool_of(9, 1_000);
    assert_eq!(
        store
            .announce_draw(&spec(NUM), &cfg(), &pool, 40, &SimulatedChain::at(40))
            .err(),
        Some(ObserveError::BeaconNotFuture)
    );
    assert_eq!(
        store
            .open_round(
                spec(NUM),
                cfg(),
                pool.clone(),
                &SimulatedChain::with_beacon(beacon())
            )
            .err(),
        Some(ObserveError::DrawNotAnnounced)
    );
    let snapshot = store
        .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
        .expect("announce");
    assert_eq!(
        store
            .announce_draw(&spec(NUM), &cfg(), &pool, 60, &SimulatedChain::at(55))
            .err(),
        Some(ObserveError::DrawAlreadyAnnounced),
        "no second announcement after the beacon is known"
    );
    let mut smaller = snapshot.clone();
    smaller.pop();
    assert_eq!(
        store
            .open_round(
                spec(NUM),
                cfg(),
                smaller,
                &SimulatedChain::with_beacon(beacon())
            )
            .err(),
        Some(ObserveError::DrawMismatch)
    );
    let mut later = cfg();
    later.reveal_close += 1;
    assert_eq!(
        store
            .open_round(
                spec(NUM),
                later,
                snapshot.clone(),
                &SimulatedChain::with_beacon(beacon())
            )
            .err(),
        Some(ObserveError::DrawMismatch)
    );
    store
        .open_round(
            spec(NUM),
            cfg(),
            snapshot.clone(),
            &SimulatedChain::with_beacon(beacon()),
        )
        .expect("open");
    assert_eq!(
        store
            .open_round(
                spec(NUM),
                cfg(),
                snapshot,
                &SimulatedChain::with_beacon(keccak256(b"x"))
            )
            .err(),
        Some(ObserveError::DrawMismatch),
        "a draw opens once: no second beacon"
    );
}

/// Exposure (L31-F2 lesson): a bond sits on as many open rounds as it
/// covers slashes; the snapshot counts free bond, and settling releases it.
#[test]
fn a_bond_is_never_committed_twice() {
    let mut a = store_for(NUM);
    // One bond covers exactly one slash of 100.
    let pool = pool_of(3, 100);
    let snap = a
        .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
        .expect("a");
    let mut r = a
        .open_round(
            spec(NUM),
            cfg(),
            snap,
            &SimulatedChain::with_beacon(beacon()),
        )
        .expect("open a");
    assert_eq!(a.exposure(&addr(1)), 100);
    // The same store, another feed's fact: the free bond is now 0.
    let mut other = spec(NUM);
    other.window_start += 10_000;
    other.window_end += 10_000;
    let mut other_cfg = cfg();
    other_cfg.commit_close += 10_000;
    other_cfg.reveal_close += 10_000;
    let snap2 = a
        .announce_draw(&other, &other_cfg, &pool, 50, &SimulatedChain::at(40))
        .expect("announce b");
    assert!(snap2.iter().all(|o| o.bond == 0), "free bond is counted");
    assert!(matches!(
        a.open_round(
            other.clone(),
            other_cfg,
            snap2,
            &SimulatedChain::with_beacon(beacon())
        ),
        Err(ObserveError::TooFewObservers { have: 0, .. })
    ));
    play(
        &mut r,
        &[
            (1, Answer::Number(5)),
            (2, Answer::Number(5)),
            (3, Answer::Number(5)),
        ],
    );
    a.settle(&r, 1_201).expect("settle a");
    assert_eq!(a.exposure(&addr(1)), 0, "settling releases the slash");
}
/// A chain whose latest reads fail and whose final block `block` shows
/// `record` at every attempt (mutation L4): the final-block arms of
/// `gone_proof` and `confirm_announced`.
struct FinalView {
    block: u64,
    record: Option<DrawRecord>,
}

impl ChainView for FinalView {
    fn height(&self) -> Result<u64, String> {
        Ok(self.block)
    }
    fn block_hash(&self, _: u64) -> Result<Option<Hash32>, String> {
        Err("unused".into())
    }
}

impl DrawChain for FinalView {
    fn draw_count(&self, _: &OnchainFact) -> Result<u32, String> {
        Err("unused".into())
    }
    fn draw_budget(&self, _: &OnchainFact) -> Result<(u32, u32), String> {
        Err("unused".into())
    }
    fn announce(&mut self, _: &OnchainDraw) -> Result<(), String> {
        Err("unused".into())
    }
    fn fix_beacon(&mut self, _: &OnchainDraw) -> Result<DrawRecord, String> {
        Err("unused".into())
    }
    fn draw_of(&self, _: &OnchainFact, _: u32) -> Result<Option<DrawRecord>, String> {
        Err("the latest block is not read here".into())
    }
    fn draw_of_final(&self, _: &OnchainFact, _: u32) -> Result<(u64, Option<DrawRecord>), String> {
        Ok((self.block, self.record))
    }
    fn post_commit(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &ObserverSig,
    ) -> Result<Posted, String> {
        Err("unused".into())
    }
    fn post_receipt(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &ObserverSig,
    ) -> Result<Posted, String> {
        Err("unused".into())
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        Err("unused".into())
    }
    fn posted_commit(
        &self,
        _: u64,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        Err("unused".into())
    }
    fn transcript_complete(&self, _: &TranscriptLogQuery) -> Result<bool, String> {
        Err("unused".into())
    }
}

/// Mutation L4: what ONE final block proves about the book's draw
/// (beacon height 1,000; its `blockhash` window ends at 1,256).
#[test]
fn final_block_proofs_at_their_edges() {
    let draw = OnchainDraw {
        fact: OnchainFact {
            metric_key: keccak256(b"metric"),
            subject_key: keccak256(b"subject"),
            window_end: 0,
            dated: false,
            template: Hash32::ZERO,
        },
        attempt: 2,
        commit: keccak256(b"commit"),
        beacon_height: 1_000,
    };
    let ours = |beacon: Hash32| DrawRecord {
        chain_id: 31_337,
        registry: [0; 20],
        attempt: 2,
        draw_count: 3,
        announced_at: 990,
        commit: draw.commit,
        beacon_height: 1_000,
        beacon,
    };
    let other = DrawRecord {
        commit: keccak256(b"another draw"),
        ..ours(Hash32::ZERO)
    };
    let at = |block, record| FinalView { block, record };
    let w = ChainWait::NOW;
    let window_end = 1_000 + BLOCKHASH_WINDOW;
    // Announced, beacon zero: fixable up to the window's end, not after.
    assert!(gone_proof(&at(window_end, Some(ours(Hash32::ZERO))), &draw, &w).is_err());
    assert!(matches!(
        gone_proof(&at(window_end + 1, Some(ours(Hash32::ZERO))), &draw, &w),
        Ok(Release::Unfixable(_))
    ));
    assert!(gone_proof(&at(window_end + 1, Some(ours(beacon()))), &draw, &w).is_err());
    // Another draw at its attempt: it never lands.
    assert!(matches!(
        gone_proof(&at(500, Some(other)), &draw, &w),
        Ok(Release::NeverLands(_))
    ));
    // None: never lands from its beacon height on, not before.
    assert!(matches!(
        gone_proof(&at(1_000, None), &draw, &w),
        Ok(Release::NeverLands(_))
    ));
    assert!(gone_proof(&at(999, None), &draw, &w).is_err());
    // The same final block decides `confirm_announced` when the latest
    // block cannot be read.
    assert!(matches!(
        confirm_announced(&at(1_100, Some(ours(Hash32::ZERO))), &draw, &w),
        Confirm::Landed
    ));
    assert!(matches!(
        confirm_announced(&at(1_100, Some(other)), &draw, &w),
        Confirm::Gone(Release::NeverLands(_))
    ));
    assert!(matches!(
        confirm_announced(&at(1_000, None), &draw, &w),
        Confirm::Gone(Release::NeverLands(_))
    ));
    assert!(matches!(
        confirm_announced(&at(999, None), &draw, &w),
        Confirm::Unknown(_)
    ));
}

/// Mutation L4: a pool too small for the draw draws nobody (never a
/// partial set); the refusal counts only members with the minimum bond;
/// a held draw keeps the chain's count from passing it.
#[test]
fn draw_sizes_bonds_and_held_counts() {
    let seed = |pool: &[Observer]| DrawSeed {
        pool: pool_hash(pool),
        beacon_height: 50,
        beacon: beacon(),
        attempt: 0,
    };
    let id = spec(NUM).id();
    let two = pool_of(2, 1_000);
    assert!(draw_observers(&id, &two, &seed(&two), 3, 100).is_empty());
    assert!(draw_observers(&id, &two, &seed(&two), 0, 100).is_empty());
    assert_eq!(draw_observers(&id, &two, &seed(&two), 2, 100).len(), 2);
    // Three members, one under the minimum bond: two eligible of three needed.
    let mut pool = pool_of(2, 1_000);
    pool.push(Observer {
        address: addr(3),
        bond: 99,
    });
    assert_eq!(
        check_draw(&spec(NUM), &cfg(), &pool, &seed(&pool), &[]),
        Err(ObserveError::TooFewObservers { have: 2, need: 3 })
    );
    // The book holds attempt 0: the chain's count may be 1 (that draw),
    // not 2 (a later one would make the book forget it).
    let mut store = store_for(NUM);
    store
        .announce_draw(
            &spec(NUM),
            &cfg(),
            &pool_of(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    assert_eq!(
        store.sync_draws(&spec(NUM), 2),
        Err(ObserveError::DrawHeld(0))
    );
    assert_eq!(store.sync_draws(&spec(NUM), 1), Ok(()));
}

/// Mutation L4: a scale-0 feed's value is an integer; a `Fixed` value
/// with scale 0 (not a valid value) never backs a claim.
#[test]
fn a_scale_zero_feed_is_backed_only_by_an_integer() {
    let int = ObservationKind::Numeric {
        scale: 0,
        tolerance: 50,
    };
    let mut r = round(int, 3);
    play(
        &mut r,
        &[
            (1, Answer::Number(12_345)),
            (2, Answer::Number(12_345)),
            (3, Answer::Number(12_345)),
        ],
    );
    let s = r.settle(1_201).expect("settle");
    let m = VerificationMethod::ReObserved {
        spec: s.spec_id(),
        result: s.result_hash(),
    };
    assert_eq!(s.backs(&spec(int), &m, &Value::Int(12_345)), Ok(()));
    assert!(s
        .backs(
            &spec(int),
            &m,
            &Value::Fixed {
                mantissa: 12_345,
                scale: 0
            }
        )
        .is_err());
}
/// Mutation L4: `release_draw` checks every proof against the book's own
/// draw, whoever built it: a final block that shows the draw, a final
/// block below its beacon height, or a round id that is not the draw's
/// round releases nothing, and the draw stays held.
#[test]
fn release_draw_refuses_proofs_that_do_not_hold() {
    let mut store = store_for(NUM);
    store
        .announce_draw(
            &spec(NUM),
            &cfg(),
            &pool_of(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    let d = store.held_draw(&spec(NUM)).expect("held");
    let ours = DrawRecord {
        chain_id: 31_337,
        registry: [0; 20],
        attempt: d.attempt,
        draw_count: d.attempt + 1,
        announced_at: 40,
        commit: d.commit,
        beacon_height: d.beacon_height,
        beacon: Hash32::ZERO,
    };
    let refused = |store: &mut ObservationResults, why: Release| {
        matches!(
            store.release_draw(&spec(NUM), why),
            Err(ObserveError::NoReleaseProof(_))
        )
    };
    let bh = d.beacon_height;
    assert!(refused(
        &mut store,
        Release::NeverLands(FinalRead {
            block: bh + 10,
            record: Some(ours),
        })
    ));
    assert!(refused(
        &mut store,
        Release::NeverLands(FinalRead {
            block: bh - 1,
            record: None,
        })
    ));
    assert!(refused(
        &mut store,
        Release::Unfixable(FinalRead {
            block: bh + BLOCKHASH_WINDOW,
            record: Some(ours),
        })
    ));
    assert!(refused(
        &mut store,
        Release::Published(keccak256(b"no such round"))
    ));
    // Another draw with a zero beacon past the window is no proof for ours.
    let other = DrawRecord {
        commit: keccak256(b"another draw"),
        ..ours
    };
    assert!(refused(
        &mut store,
        Release::Unfixable(FinalRead {
            block: bh + BLOCKHASH_WINDOW + 1,
            record: Some(other),
        })
    ));
    assert_eq!(store.held_draw(&spec(NUM)), Some(d), "the draw stays");
    // The proof that does hold releases it: nothing at its attempt as of
    // a final block at its beacon height.
    assert_eq!(
        store.release_draw(
            &spec(NUM),
            Release::NeverLands(FinalRead {
                block: bh,
                record: None,
            })
        ),
        Ok(())
    );
    assert_eq!(store.held_draw(&spec(NUM)), None);
    // A draw opened here is never released as one that never landed.
    let mut store = store_for(NUM);
    let snapshot = store
        .announce_draw(
            &spec(NUM),
            &cfg(),
            &pool_of(9, 1_000),
            50,
            &SimulatedChain::at(40),
        )
        .expect("announce");
    store
        .open_round(
            spec(NUM),
            cfg(),
            snapshot,
            &SimulatedChain::with_beacon(beacon()),
        )
        .expect("open");
    let d = store.held_draw(&spec(NUM)).expect("held");
    assert!(refused(
        &mut store,
        Release::NeverLands(FinalRead {
            block: d.beacon_height,
            record: None,
        })
    ));
    assert!(refused(&mut store, Release::NeverSent));
    assert_eq!(store.held_draw(&spec(NUM)), Some(d));
}

// ---- ROADMAP E8 (THREATS MUT-2): the draw invariant's `Published`
// proof, `published`, `unwritable_draw` and `settle`'s round checks,
// directly (before, only the `revlog_r8` property test reached them).

/// A round opened in `store` from three observers, each answering 5,
/// not kept.
fn opened_and_played(store: &mut ObservationResults) -> ObservationRound {
    let mut r = open_in(store, spec(NUM), cfg(), pool_of(3, 1_000)).expect("open");
    play(
        &mut r,
        &[
            (1, Answer::Number(5)),
            (2, Answer::Number(5)),
            (3, Answer::Number(5)),
        ],
    );
    r
}

/// `kept_round`, revealed and settled in `store`.
fn kept_and_settled(store: &mut ObservationResults) -> UnfinishedRound {
    let (mut u, reveals) = kept_round(store);
    for r in reveals {
        u.round
            .reveal(cfg().commit_close + 1, r.0, r.1, r.2, r.3)
            .expect("reveal");
    }
    store.settle(&u.round, 1_201).expect("settles");
    u
}

fn released(store: &mut ObservationResults, id: Hash32) -> Result<(), ObserveError> {
    store.release_draw(&spec(NUM), Release::Published(id))
}

/// `Published` releases the book's draw only for that draw's round
/// (the kept one if any, else the one opened here), and only once it is
/// settled here or kept as a failed attempt.
#[test]
fn a_published_release_needs_the_draws_own_round_settled_or_failed() {
    // Opened here, not kept.
    let mut store = store_for(NUM);
    let r = opened_and_played(&mut store);
    assert!(
        matches!(
            released(&mut store, r.round_id),
            Err(ObserveError::NoReleaseProof(_))
        ),
        "not settled yet"
    );
    // Another round settled here is not this draw's round.
    let foreign = keccak256(b"another settled round");
    store.rounds.insert(foreign);
    assert!(matches!(
        released(&mut store, foreign),
        Err(ObserveError::NoReleaseProof(_))
    ));
    assert!(store.held_draw(&spec(NUM)).is_some());
    store.settle(&r, 1_201).expect("settle");
    assert_eq!(released(&mut store, r.round_id), Ok(()));
    assert_eq!(store.held_draw(&spec(NUM)), None);

    // Kept and settled: released, and the kept round leaves the book.
    let mut store = store_for(NUM);
    let u = kept_and_settled(&mut store);
    assert_eq!(released(&mut store, u.round.round_id), Ok(()));
    assert!(store.unfinished(&spec(NUM)).is_none());
    assert_eq!(store.held_draw(&spec(NUM)), None);

    // Kept, unsettled, not failed: no proof.
    let mut store = store_for(NUM);
    let (u, _) = kept_round(&mut store);
    assert!(matches!(
        released(&mut store, u.round.round_id),
        Err(ObserveError::NoReleaseProof(_))
    ));
    // A settled round id that is not the kept one: no proof either.
    store.rounds.insert(foreign);
    assert!(matches!(
        released(&mut store, foreign),
        Err(ObserveError::NoReleaseProof(_))
    ));
    assert!(store.unfinished(&spec(NUM)).is_some());

    // Kept as a failed attempt (never settled): another id releases
    // nothing; its own id releases it and clears the failed mark.
    let mut store = store_for(NUM);
    let (u, _) = kept_round(&mut store);
    store.keep_failed(u.clone()).expect("failed");
    store.attempt_published(&spec(NUM), &foreign);
    assert!(store.failed_attempt(&spec(NUM)).is_some());
    assert!(store.held_draw(&spec(NUM)).is_some());
    store.attempt_published(&spec(NUM), &u.round.round_id);
    assert!(store.failed_attempt(&spec(NUM)).is_none());
    assert!(store.unfinished(&spec(NUM)).is_none());
    assert!(store.failed.is_empty());
    assert_eq!(store.held_draw(&spec(NUM)), None);

    // A failed mark for a round kept on another attempt is no proof.
    let mut store = store_for(NUM);
    let (mut u, _) = kept_round(&mut store);
    store.keep_failed(u.clone()).expect("failed");
    let fact = store.fact_key_of(&spec(NUM)).expect("fact");
    u.round.draw.attempt += 1;
    store.unfinished.insert(fact, u.clone());
    let opened = store.opened.values().next().copied().expect("opened");
    assert!(matches!(
        released(&mut store, opened),
        Err(ObserveError::NoReleaseProof(_))
    ));
}

/// `published` drops only the held draw's own settled round; a settled
/// round that is not it changes nothing.
#[test]
fn published_drops_only_the_held_draws_round() {
    let mut store = store_for(NUM);
    let r = opened_and_played(&mut store);
    store.settle(&r, 1_201).expect("settle");
    assert!(store.unwritable_draw().is_some(), "settled, not published");
    assert_eq!(store.published(&spec(NUM), &r.round_id), Ok(()));
    assert_eq!(store.held_draw(&spec(NUM)), None);
    assert_eq!(store.unwritable_draw(), None);
    assert_eq!(
        store.published(&spec(NUM), &r.round_id),
        Ok(()),
        "idempotent"
    );

    // A kept, unsettled round; another round id settled here.
    let mut store = store_for(NUM);
    let (u, _) = kept_round(&mut store);
    let other = keccak256(b"settled elsewhere");
    store.rounds.insert(other);
    assert_eq!(store.published(&spec(NUM), &other), Ok(()));
    assert!(store.unfinished(&spec(NUM)).is_some());
    assert!(store.held_draw(&spec(NUM)).is_some());
    assert_eq!(
        store.published(&spec(NUM), &u.round.round_id),
        Err(ObserveError::RoundNotSettled)
    );
}

/// The book can be written while every held draw is its fact's latest
/// and none has a settled round that is neither kept nor published.
#[test]
fn unwritable_draw_names_each_unwritable_state() {
    let mut store = store_for(NUM);
    store
        .announce_draw(
            &spec(NUM),
            &cfg(),
            &pool_of(3, 1_000),
            10,
            &SimulatedChain::at(0),
        )
        .expect("announce");
    assert_eq!(store.unwritable_draw(), None, "announced only");
    let fact = store.fact_key_of(&spec(NUM)).expect("fact");
    store.draws.insert(fact, 2);
    let why = store.unwritable_draw().expect("not the latest");
    assert!(why.contains("not its fact's latest"), "{why}");

    // Kept and settled: written as kept.
    let mut store = store_for(NUM);
    let _ = kept_and_settled(&mut store);
    assert_eq!(store.unwritable_draw(), None);
    // Settled and not kept: not writable until published.
    let mut store = store_for(NUM);
    let r = opened_and_played(&mut store);
    assert_eq!(store.unwritable_draw(), None, "opened, unsettled");
    store.settle(&r, 1_201).expect("settle");
    let why = store.unwritable_draw().expect("unwritable");
    assert!(why.contains("not kept and is not published"), "{why}");
}

/// Only the round opened here from its announced draw settles: a round
/// whose id, spec, config, pool or beacon height differs from the draw's
/// is refused, each on its own.
#[test]
fn settle_checks_the_round_against_its_draw() {
    let mut store = store_for(NUM);
    let r = opened_and_played(&mut store);
    let edits: [&dyn Fn(&mut ObservationRound); 5] = [
        &|x| x.round_id = keccak256(b"another id"),
        &|x| x.spec_id = keccak256(b"another spec"),
        &|x| x.cfg.reward += 1,
        &|x| x.pool[0].bond += 1,
        &|x| x.draw.beacon_height += 1,
    ];
    for e in edits {
        let mut x = r.clone();
        e(&mut x);
        assert_eq!(store.settle(&x, 1_201), Err(ObserveError::DrawMismatch));
    }
    assert!(store.settle(&r, 1_201).is_ok());
}

/// A chain proof is checked against every field of the book's draw: a
/// record that differs in its attempt, commitment or beacon height alone
/// is another draw, so the book's never lands.
#[test]
fn never_lands_compares_every_field_of_the_draw() {
    let base = |store: &mut ObservationResults| {
        store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(9, 1_000),
                50,
                &SimulatedChain::at(40),
            )
            .expect("announce");
        let d = store.held_draw(&spec(NUM)).expect("held");
        DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: d.attempt,
            draw_count: d.attempt + 1,
            announced_at: 40,
            commit: d.commit,
            beacon_height: d.beacon_height,
            beacon: Hash32::ZERO,
        }
    };
    let edits: [&dyn Fn(&mut DrawRecord); 3] = [
        &|x| x.attempt += 1,
        &|x| x.commit = keccak256(b"another"),
        &|x| x.beacon_height += 1,
    ];
    for e in edits {
        let mut store = store_for(NUM);
        let mut rec = base(&mut store);
        let block = rec.beacon_height;
        e(&mut rec);
        assert_eq!(
            store.release_draw(
                &spec(NUM),
                Release::NeverLands(FinalRead {
                    block,
                    record: Some(rec),
                })
            ),
            Ok(())
        );
        assert_eq!(store.held_draw(&spec(NUM)), None);
    }
}

/// `Unfixable` releases a fixed-subject draw only past the `blockhash`
/// window with the book's own draw at a zero beacon, and counts it as
/// released (announced on chain), not as never sent.
#[test]
fn unfixable_releases_only_past_the_window_with_a_zero_beacon() {
    let setup = |store: &mut ObservationResults| {
        store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(9, 1_000),
                50,
                &SimulatedChain::at(40),
            )
            .expect("announce");
        let d = store.held_draw(&spec(NUM)).expect("held");
        DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: d.attempt,
            draw_count: d.attempt + 1,
            announced_at: 40,
            commit: d.commit,
            beacon_height: d.beacon_height,
            beacon: Hash32::ZERO,
        }
    };
    let mut store = store_for(NUM);
    let rec = setup(&mut store);
    let past = rec.beacon_height + BLOCKHASH_WINDOW + 1;
    let fixed = DrawRecord {
        beacon: beacon(),
        ..rec
    };
    assert!(matches!(
        store.release_draw(
            &spec(NUM),
            Release::Unfixable(FinalRead {
                block: past,
                record: Some(fixed),
            })
        ),
        Err(ObserveError::NoReleaseProof(_))
    ));
    assert_eq!(
        store.release_draw(
            &spec(NUM),
            Release::Unfixable(FinalRead {
                block: past,
                record: Some(rec),
            })
        ),
        Ok(())
    );
    assert_eq!(store.held_draw(&spec(NUM)), None);
    let fact = store.fact_key_of(&spec(NUM)).expect("fact");
    assert!(store.released.contains(&(fact, rec.attempt)));
    assert!(
        store.announcements.contains_key(&(fact, rec.attempt)),
        "counted: it was announced"
    );
}

/// ROADMAP B2: independent observers send their signed reveals through
/// the spool ([`crate::observer_link`]). The intake takes each drawn
/// observer's reveal as it arrives; a forged one (another observer's
/// signature), one from an observer not drawn, and a rewritten second
/// reveal (a replay with another answer) are non-reveals; the files stay
/// in the spool, so a coordinator restarted inside the window reads them
/// again without the observers resending (REVLOG-R5-1d); a reveal written
/// after `reveal_close` is never taken.
#[test]
fn b2_reveals_through_the_spool_are_checked_and_survive_a_restart() {
    use crate::observer_link::{reveal_path, send_reveal, DirInbox};
    let dir = std::env::temp_dir().join(format!("onyx-b2-spool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = store_for(NUM);
    let (mut u, reveals) = kept_round(&mut store);
    let (close, end) = (cfg().commit_close, cfg().reveal_close);
    let round = u.round.round_id;
    // Observer 1 reveals; observer 2's file holds a forgery (observer 3's
    // signature); a stranger (not drawn) writes a valid reveal of its own.
    send_reveal(&dir, &round, 0, &reveals[0]).expect("send");
    let mut forged = reveals[1].clone();
    forged.3 = reveals[2].3;
    send_reveal(&dir, &round, 0, &forged).expect("send");
    let (sk9, a9) = test_keys::ordered(9);
    let o9 = obs(Answer::Number(6));
    let s9 = sign_reveal(&sk9, &round, 0, &o9, &SALT).expect("sig");
    send_reveal(&dir, &round, 0, &(a9, o9, SALT, s9)).expect("send");
    let mut inbox = DirInbox::open(&dir).expect("spool");
    let clock = ReplayClock::at(close + 1);
    // Observer 3 never reveals: the intake ends at reveal_close.
    let taken = keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(()))
        .expect("intake");
    assert_eq!(taken, 1, "only observer 1's reveal is one");
    assert_eq!(u.reveals.len(), 1);
    assert_eq!(u.reveals[0].reveal, reveals[0]);
    assert_eq!(u.reveals[0].arrived, close + 1);
    assert!(
        clock.now() > end,
        "waited out the window for observers 2 and 3"
    );

    // Restart inside the window: observer 2 now sends its real reveal and
    // observer 1 rewrites its file with another answer (a replay).
    let mut store = store_for(NUM);
    let (mut u, _) = kept_round(&mut store);
    send_reveal(&dir, &round, 0, &reveals[1]).expect("send");
    let clock = ReplayClock::at(close + 10);
    let mut inbox = DirInbox::open(&dir).expect("spool");
    assert_eq!(
        keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
        Ok(2),
        "the spool still holds observer 1's reveal: read again after the restart"
    );
    let mut other = reveals[0].clone();
    other.1 = obs(Answer::Number(7));
    let sk1 = test_keys::ordered_secret(&addr(1)).expect("key");
    other.3 = sign_reveal(&sk1, &round, 0, &other.1, &SALT).expect("sig");
    send_reveal(&dir, &round, 0, &other).expect("send");
    assert_eq!(
        inbox.take(&round, &[addr(1)], 0),
        vec![other.clone()],
        "the rewritten file is read"
    );
    assert!(
        !u.receive_reveal(other, close + 20),
        "the first reveal is kept"
    );
    assert_eq!(u.reveals.len(), 2);

    // After reveal_close nothing is taken, even with every file there.
    send_reveal(&dir, &round, 0, &reveals[2]).expect("send");
    let mut store = store_for(NUM);
    let (mut u, _) = kept_round(&mut store);
    let clock = ReplayClock::at(end + 1);
    assert_eq!(
        keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
        Ok(0)
    );
    assert!(u.reveals.is_empty());
    assert!(reveal_path(&dir, &addr(3), &round).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// ROADMAP B3 (THREATS REVLOG-4): the receipt key is rotated for new
/// draws only. A round whose draw snapshotted the old key is never
/// receipted with the new one: the run stops before the log read and
/// any receipt, the round stays unfinished, and the draw's own key
/// finishes it.
#[test]
fn b3_a_rotated_receipt_key_never_receipts_an_earlier_draw() {
    let mut store = store_for(NUM);
    let (u, reveals) = kept_round(&mut store);
    let (close, _) = (cfg().commit_close, cfg().reveal_close);
    let mut u = u;
    for r in &reveals {
        assert!(u.receive_reveal(r.clone(), close + 1));
    }
    let fact = store.onchain_fact(&spec(NUM)).expect("fact");
    let mut reg = SimulatedRegistry::new();
    let old = test_keys::secret("b3 rotation", 0);
    let new = test_keys::secret("b3 rotation", 1);
    reg.log.signers.insert(
        (fact.registry_key(), 0),
        secret_address(&old).expect("addr"),
    );
    // The deployment rotated to `new` after this draw was announced.
    reg.receipt_signer = secret_address(&new).expect("addr");
    reg.time = close + 1;
    let mut round = u.round.clone();
    let e = finish_reveals(
        &mut round,
        &mut reg,
        &fact,
        &new,
        &u.reveals,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect_err("another key than the draw's");
    assert!(e.contains("rotated"), "{e}");
    assert!(round.log.is_none(), "stopped before the log read");
    let receipts = |reg: &SimulatedRegistry| {
        (1..=3)
            .filter(|i| {
                reg.log
                    .receipt(&fact.registry_key(), 0, &addr(*i), &u.round.round_id)
                    .is_some()
            })
            .count()
    };
    assert_eq!(receipts(&reg), 0, "no receipt was posted");
    finish_reveals(
        &mut round,
        &mut reg,
        &fact,
        &old,
        &u.reveals,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("the draw's own key finishes it");
    assert_eq!(round.reveals.len(), 3);
    assert_eq!(receipts(&reg), 3);
}
