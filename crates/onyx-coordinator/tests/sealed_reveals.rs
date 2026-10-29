//! Sealed reveal posts and attempt records (ADR-0027 amendment 10, ROADMAP C4):
//! the protocol on a real round with the registry's rules simulated
//! (`SimulatedSealedLog`), and the adversarial cases of the C4 pass (THREATS
//! C4-*): a coordinator dropping sealed posts or dismissing a valid one, an
//! observer posting garbage or copying another's ephemeral key, early and late
//! posts, replay across rounds and attempts, and a sealed round that lists a
//! withheld reveal as revealed (REVLOG-5).

use onyx_coordinator::observe::{
    result_hash_of, sign_commit, test_keys, ObservationOutcome, ObservationResults,
    ObservationRound, ObserveConfig, Observer, SimulatedChain,
};
use onyx_coordinator::sealed::{
    check_dismissal, check_dismissal_posted, decrypt_with, dismiss, key_proof, key_proof_valid,
    open_post, open_posted, point_address, post_signed, public_point, seal, seal_bytes, sealed_len,
    valid_reveal, EntryStatus, Opened, PostedSealed, RevealContext, RevealLogQuery, SealError,
    SealedEntry, SealedPost, SimulatedSealedLog, MAX_PLAIN_BYTES, MAX_SEALED_BYTES,
};
use onyx_core::claim::Subject;
use onyx_core::{
    keccak256, observation_commit, Answer, DataRights, FeedTemplate, Hash32, MinerAddress,
    Observation, ObservationKind, ObservationSpec, Redistribution, SourceId, SubjectRule,
};

const FEED: &str = "example.page.usd_per_unit";
const COMMIT_CLOSE: u64 = 1_100;
const REVEAL_CLOSE: u64 = 1_200;
/// The coordinator's reveal key (a fixed test scalar; holds nothing anywhere).
const REVEAL_SECRET: [u8; 32] = [0x42; 32];

fn spec() -> ObservationSpec {
    ObservationSpec {
        feed: FEED.into(),
        metric: onyx_core::claim::MetricId(FEED.into()),
        subject: Subject::Entity("entity:alpha".into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/usd_per_unit".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        },
        window_start: 1_000,
        window_end: 1_050,
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

fn cfg() -> ObserveConfig {
    ObserveConfig {
        commit_close: COMMIT_CLOSE,
        reveal_close: REVEAL_CLOSE,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

fn template() -> FeedTemplate {
    let s = spec();
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
        subject: SubjectRule::Fixed(s.subject.clone()),
    }
}

fn obs(v: i128) -> Observation {
    Observation {
        answer: Answer::Number(v),
        observed_at: 1_010,
        page_hash: keccak256(b"page"),
    }
}

/// A round of three drawn observers that each committed `values[i]` (posted on
/// chain inside the commit phase), a registry with the draw's reveal key, and
/// each observer's sealed post of its reveal (not posted yet).
struct Fx {
    store: ObservationResults,
    round: ObservationRound,
    fact: Hash32,
    chain: SimulatedSealedLog,
    keys: Vec<([u8; 32], MinerAddress)>,
    values: Vec<i128>,
    salts: Vec<Hash32>,
}

impl Fx {
    fn new(values: &[i128]) -> Self {
        let (mut store, admin) = ObservationResults::new();
        store
            .register_feed(&admin, template(), "test feed", 0)
            .expect("register");
        let keys: Vec<_> = (1..=values.len() as u8).map(test_keys::ordered).collect();
        let pool: Vec<Observer> = keys
            .iter()
            .map(|(_, a)| Observer {
                address: *a,
                bond: 1_000,
            })
            .collect();
        let snap = store
            .announce_draw(&spec(), &cfg(), &pool, 10, &SimulatedChain::at(0))
            .expect("announce");
        let mut round = store
            .open_round(
                spec(),
                cfg(),
                snap,
                &SimulatedChain::with_beacon(Hash32([3; 32])),
            )
            .expect("open");
        let fact = keccak256(b"registry fact");
        let mut chain = SimulatedSealedLog::default();
        let attempt = round.draw().attempt;
        chain
            .log
            .signers
            .insert((fact, attempt), test_keys::receipt_signer());
        chain.keys.insert(
            (fact, attempt),
            public_point(&REVEAL_SECRET).expect("reveal key"),
        );
        let rid = round.round_id();
        let salts: Vec<Hash32> = (0..values.len() as u8)
            .map(|i| keccak256(&[b's', i]))
            .collect();
        for (i, (sk, a)) in keys.iter().enumerate() {
            let h = observation_commit(&rid, a, &obs(values[i]), &salts[i]);
            let sig = sign_commit(sk, &rid, attempt, &h).expect("sign");
            round.commit(1_070, *a, h, sig).expect("commit");
            chain
                .log
                .post_commit(&fact, attempt, a, &rid, &h, &sig, 1_070)
                .expect("post commit");
        }
        Fx {
            store,
            round,
            fact,
            chain,
            keys,
            values: values.to_vec(),
            salts,
        }
    }

    fn attempt(&self) -> u32 {
        self.round.draw().attempt
    }

    fn rid(&self) -> Hash32 {
        self.round.round_id()
    }

    fn key(&self) -> [u8; 64] {
        public_point(&REVEAL_SECRET).expect("key")
    }

    /// Observer `i`'s sealed post of its own reveal.
    fn sealed(&self, i: usize) -> SealedPost {
        seal(
            &self.keys[i].0,
            &keccak256(&[b'r', i as u8]).0,
            &self.key(),
            &self.rid(),
            self.attempt(),
            &spec(),
            &obs(self.values[i]),
            &self.salts[i],
        )
        .expect("seal")
    }

    fn post(&mut self, p: &SealedPost, at: u64) -> Result<(), &'static str> {
        let (fact, attempt, rid) = (self.fact, self.attempt(), self.rid());
        self.chain.post_sealed(&fact, attempt, &rid, p, at)
    }

    fn ctx(&self, i: usize) -> (Hash32, Option<Hash32>) {
        let c = self
            .round
            .signed_commits()
            .into_iter()
            .find(|(a, _, _)| *a == self.keys[i].1)
            .map(|(_, h, _)| h);
        (self.rid(), c)
    }

    /// The coordinator's intake: open observer `i`'s post from the chain.
    fn open(&self, i: usize, p: &SealedPost) -> Opened {
        let s = spec();
        let (round_id, commit) = self.ctx(i);
        let ctx = RevealContext {
            round_id,
            attempt: self.attempt(),
            spec: &s,
            commit,
        };
        open_post(&REVEAL_SECRET, p, &ctx).expect("open")
    }

    fn query(&self, status: &[EntryStatus]) -> RevealLogQuery {
        RevealLogQuery {
            registry_fact: self.fact,
            attempt: self.attempt(),
            round_id: self.rid(),
            commit_open: spec().window_start,
            commit_close: COMMIT_CLOSE,
            reveal_close: REVEAL_CLOSE,
            entries: self
                .round
                .observers()
                .iter()
                .map(|o| {
                    let i = self
                        .keys
                        .iter()
                        .position(|(_, a)| *a == o.address)
                        .expect("drawn");
                    SealedEntry {
                        observer: o.address,
                        commit: self.ctx(i).1.unwrap_or(Hash32::ZERO),
                        status: status[i],
                    }
                })
                .collect(),
        }
    }
}

use EntryStatus::{Dismissed, NotRevealed, Revealed};

/// REVLOG-1 closed for sealed posts: every observer posts on chain; the
/// coordinator opens each post into the round's reveal (arriving at the post's
/// block time) and settles; the chain answers true only with every post revealed.
#[test]
fn c4_sealed_posts_settle_the_round_and_the_chain_requires_every_one() {
    let mut fx = Fx::new(&[100, 110, 120]);
    for i in 0..3 {
        let p = fx.sealed(i);
        fx.post(&p, 1_150).expect("post");
        match fx.open(i, &p) {
            Opened::Reveal((a, o, salt, sig)) => fx
                .round
                .reveal(1_150, a, o, salt, sig)
                .expect("reveal taken"),
            Opened::Dismissed(_) => panic!("a valid post was dismissed"),
        }
    }
    let s = fx.store.settle(&fx.round, 1_201).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::Value(Answer::Number(110)));
    assert!(fx.chain.complete(&fx.query(&[Revealed; 3])));
    // The coordinator drops any one post: the chain says no.
    for i in 0..3 {
        let mut st = [Revealed; 3];
        st[i] = NotRevealed;
        assert!(!fx.chain.complete(&fx.query(&st)), "post {i} dropped");
    }
}

/// C4-1: a coordinator that drops a sealed post (to steer the value or force
/// `NoResult`) cannot record the round, and a value round leaving it out fails
/// the client's `revealLogComplete`. Shown failing on the old rule: with the
/// post as a mere receipt-less reveal, `transcriptComplete` passes.
#[test]
fn c4_a_dropped_sealed_post_is_caught_where_a_dropped_spool_reveal_is_not() {
    let mut fx = Fx::new(&[100, 100, 900]);
    for i in 0..3 {
        let p = fx.sealed(i);
        fx.post(&p, 1_150).expect("post");
    }
    // The coordinator takes observers 2 and 3 only: {100, 900} is NoResult.
    let q = fx.query(&[NotRevealed, Revealed, Revealed]);
    assert!(!fx.chain.complete(&q));
    let nr = result_hash_of(
        &fx.round.spec_id(),
        &fx.rid(),
        &ObservationOutcome::NoResult,
    );
    assert_eq!(
        fx.chain.record_attempt(&q, &nr, 1_300),
        Err("IncompleteTranscript")
    );
    // The amendment-8 rule alone (no receipt was ever signed) accepts the drop.
    let old = fx.round.transcript_query(fx.fact);
    let mut old = old;
    old.entries[0].revealed = false;
    assert!(
        fx.chain.log.complete(&old),
        "REVLOG-1 as it was: undetected"
    );
}

/// C4-2: garbage (junk bytes, or a well-formed reveal that does not open the
/// commit) is dismissed with a proof anyone checks; the chain takes status 2
/// only for an in-phase post; nobody can turn a valid post into garbage.
#[test]
fn c4_garbage_is_dismissed_with_a_checkable_proof_and_a_valid_post_never_is() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let (rid, attempt, key) = (fx.rid(), fx.attempt(), fx.key());
    let junk = seal_bytes(
        &fx.keys[0].0,
        &fx.keys[0].1,
        &[7; 32],
        &key,
        &rid,
        attempt,
        b"not a reveal",
    )
    .expect("seal junk");
    // Observer 2 seals a signed reveal of ANOTHER value than it committed.
    let wrong = seal(
        &fx.keys[1].0,
        &[8; 32],
        &key,
        &rid,
        attempt,
        &spec(),
        &obs(555),
        &fx.salts[1],
    )
    .expect("seal");
    let good = fx.sealed(2);
    fx.post(&junk, 1_150).expect("post");
    fx.post(&wrong, 1_150).expect("post");
    fx.post(&good, 1_150).expect("post");
    let s = spec();
    for (i, p) in [(0, &junk), (1, &wrong)] {
        let Opened::Dismissed(d) = fx.open(i, p) else {
            panic!("garbage {i} taken as a reveal");
        };
        let (round_id, commit) = fx.ctx(i);
        let ctx = RevealContext {
            round_id,
            attempt,
            spec: &s,
            commit,
        };
        let plain = check_dismissal(&key, p, &ctx, &d).expect("a client checks the dismissal");
        if i == 0 {
            assert_eq!(plain, b"not a reveal");
        }
    }
    assert!(fx
        .chain
        .complete(&fx.query(&[Dismissed, Dismissed, Revealed])));
    // Dismissing the valid post: the proof is real, the dismissal is false.
    let (round_id, commit) = fx.ctx(2);
    let ctx = RevealContext {
        round_id,
        attempt,
        spec: &s,
        commit,
    };
    let d = dismiss(&REVEAL_SECRET, &good, &rid, attempt).expect("dismiss");
    assert_eq!(
        check_dismissal(&key, &good, &ctx, &d),
        Err(SealError::FalseDismissal)
    );
    // A made-up shared point, or a tampered proof, does not check.
    let mut fake = d;
    fake.shared = public_point(&[9; 32]).expect("point");
    assert_eq!(
        check_dismissal(&key, &good, &ctx, &fake),
        Err(SealError::BadProof)
    );
    for byte in [0usize, 40] {
        let mut t = d;
        t.proof[byte] ^= 1;
        assert_eq!(
            check_dismissal(&key, &good, &ctx, &t),
            Err(SealError::BadProof)
        );
    }
    // A dismissal under another key does not check either.
    let other = public_point(&[0x43; 32]).expect("point");
    assert_eq!(
        check_dismissal(&other, &good, &ctx, &d),
        Err(SealError::BadProof)
    );
    // The chain: status 2 for a slot with no post is refused.
    let mut fx2 = Fx::new(&[100, 100, 100]);
    let p = fx2.sealed(0);
    fx2.post(&p, 1_150).expect("post");
    assert!(!fx2
        .chain
        .complete(&fx2.query(&[Revealed, Dismissed, NotRevealed])));
}

/// C4-3: another drawn observer re-posts A's ephemeral key and ciphertext
/// under its own name (so that dismissing it would publish `S` and open A's
/// reveal): refused, it cannot sign the ephemeral key's proof. Relaying A's own
/// post unchanged is fine (it fills A's slot).
#[test]
fn c4_copying_another_observers_ephemeral_key_is_refused() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let a = fx.sealed(0);
    let (rid, attempt) = (fx.rid(), fx.attempt());
    // B signs the copied post with its own key; the key proof is A's.
    let msg = onyx_coordinator::sealed::sealed_reveal_message(
        &rid,
        attempt,
        &fx.keys[1].1,
        &a.ephemeral,
        &keccak256(&a.ciphertext),
    );
    let copied = SealedPost {
        observer: fx.keys[1].1,
        sig: onyx_labels::attest::eip191_sign_with(&fx.keys[1].0, msg.as_bytes()).expect("sign"),
        ..a.clone()
    };
    assert!(!post_signed(&copied, &rid, attempt));
    assert_eq!(fx.post(&copied, 1_150), Err("BadKeyProof"));
    // A griefer cannot fill A's slot with its own ciphertext either.
    let mut grief = a.clone();
    grief.ciphertext[0] ^= 1;
    assert_eq!(fx.post(&grief, 1_150), Err("BadSignature"));
    // Anyone relays A's post as signed: A's slot, once.
    fx.post(&a, 1_150).expect("relay");
    assert_eq!(fx.post(&a, 1_151), Err("AlreadyPosted"));
    // A dismissal of B's own garbage reveals an S that opens nothing of A's.
    let b = seal_bytes(
        &fx.keys[1].0,
        &fx.keys[1].1,
        &[5; 32],
        &fx.key(),
        &rid,
        attempt,
        b"junk",
    )
    .expect("seal");
    let d = dismiss(&REVEAL_SECRET, &b, &rid, attempt).expect("dismiss");
    let s = spec();
    let (round_id, commit) = fx.ctx(0);
    let ctx = RevealContext {
        round_id,
        attempt,
        spec: &s,
        commit,
    };
    assert!(valid_reveal(
        &decrypt_with(&d.shared, &a, &rid, attempt),
        &a.observer,
        &ctx
    )
    .is_none());
}

/// C4-4: posts outside the reveal phase are ignored (no veto): one landing in
/// the commit phase (it would show the value to the coordinator before commits
/// close) or after `reveal_close` neither needs a reveal nor can be dismissed.
#[test]
fn c4_posts_outside_the_reveal_phase_are_ignored() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let early = fx.sealed(0);
    let late = fx.sealed(1);
    fx.post(&early, COMMIT_CLOSE).expect("post");
    fx.post(&late, REVEAL_CLOSE + 1).expect("post");
    let p = fx.sealed(2);
    fx.post(&p, REVEAL_CLOSE)
        .expect("post at reveal close counts");
    assert!(fx
        .chain
        .complete(&fx.query(&[NotRevealed, NotRevealed, Revealed])));
    assert!(!fx
        .chain
        .complete(&fx.query(&[NotRevealed, NotRevealed, NotRevealed])));
    assert!(!fx
        .chain
        .complete(&fx.query(&[Dismissed, NotRevealed, Revealed])));
    assert!(!fx
        .chain
        .complete(&fx.query(&[NotRevealed, Dismissed, Revealed])));
}

/// C4-5: a post for one round or attempt never counts for another: its
/// signatures name both, and a ciphertext re-signed by its own observer for
/// another round decrypts to garbage there (the key derivation binds the round),
/// so it is dismissed; the dismissal opens only that copy.
#[test]
fn c4_replay_across_rounds_and_attempts_is_refused() {
    let fx = Fx::new(&[100, 100, 100]);
    let p = fx.sealed(0);
    let (rid, attempt) = (fx.rid(), fx.attempt());
    assert!(post_signed(&p, &rid, attempt));
    assert!(!post_signed(&p, &rid, attempt + 1));
    assert!(!post_signed(&p, &keccak256(b"another round"), attempt));
    let mut chain = SimulatedSealedLog::default();
    let other_fact = keccak256(b"other fact");
    chain
        .log
        .signers
        .insert((other_fact, attempt + 1), test_keys::receipt_signer());
    chain.keys.insert((other_fact, attempt + 1), fx.key());
    assert_eq!(
        chain.post_sealed(&other_fact, attempt + 1, &rid, &p, 1_150),
        Err("BadSignature")
    );
    // The observer itself re-signs the same ciphertext for attempt + 1.
    let (sk, a) = fx.keys[0];
    let key_proof = key_proof(&keccak256(&[b'r', 0]).0, &rid, attempt + 1, &a).expect("proof");
    let sig = onyx_labels::attest::eip191_sign_with(
        &sk,
        onyx_coordinator::sealed::sealed_reveal_message(
            &rid,
            attempt + 1,
            &a,
            &p.ephemeral,
            &keccak256(&p.ciphertext),
        )
        .as_bytes(),
    )
    .expect("sign");
    let replay = SealedPost {
        key_proof,
        sig,
        ..p.clone()
    };
    chain
        .post_sealed(&other_fact, attempt + 1, &rid, &replay, 1_150)
        .expect("a well-signed replay lands");
    let s = spec();
    let (_, commit) = fx.ctx(0);
    let ctx = RevealContext {
        round_id: rid,
        attempt: attempt + 1,
        spec: &s,
        commit,
    };
    assert!(matches!(
        open_post(&REVEAL_SECRET, &replay, &ctx),
        Ok(Opened::Dismissed(_))
    ));
}

/// REVLOG-5 (C4-6): a NoResult round recorded on chain before it is served
/// sealed. The record pins every observer's commit and status and the result:
/// served sealed with the record, a withheld reveal listed as revealed passes
/// (the stated limit), but the round can never be unsealed consistently: with
/// all reveals its result is a value, without the withheld one its entries
/// differ; `attemptMatches` is false either way, a proof on chain.
#[test]
fn c4_an_attempt_record_binds_a_sealed_round_and_catches_it_at_unseal() {
    let mut fx = Fx::new(&[100, 100, 900]);
    for i in 0..3 {
        let p = fx.sealed(i);
        fx.post(&p, 1_150).expect("post");
    }
    let nr = result_hash_of(
        &fx.round.spec_id(),
        &fx.rid(),
        &ObservationOutcome::NoResult,
    );
    let listed = fx.query(&[Revealed; 3]);
    assert_eq!(
        fx.chain.record_attempt(&listed, &nr, REVEAL_CLOSE),
        Err("TooEarly")
    );
    // The coordinator aggregates without observer 1's 100 (NoResult) but lists
    // it as revealed: the chain cannot tell while the round is sealed.
    fx.chain
        .record_attempt(&listed, &nr, 1_300)
        .expect("recorded");
    assert!(fx.chain.attempt_matches(&listed, &nr));
    assert_eq!(
        fx.chain.record_attempt(&listed, &nr, 1_301),
        Err("AlreadyRecorded")
    );
    // Unsealed with every reveal: the client recomputes a value.
    for i in 0..3 {
        let p = fx.sealed(i);
        if let Opened::Reveal((a, o, salt, sig)) = fx.open(i, &p) {
            fx.round.reveal(1_150, a, o, salt, sig).expect("reveal");
        }
    }
    let s = fx.store.settle(&fx.round, 1_301).expect("settle");
    assert_eq!(*s.outcome(), ObservationOutcome::Value(Answer::Number(100)));
    assert!(!fx.chain.attempt_matches(&listed, &s.result_hash()));
    // Unsealed without it: the entries differ.
    assert!(!fx
        .chain
        .attempt_matches(&fx.query(&[NotRevealed, Revealed, Revealed]), &nr));
}

/// REVLOG-5: the record itself refuses to leave out a sealed post, and a
/// receipted reveal must be listed revealed (amendment 8 rules carried over).
#[test]
fn c4_an_attempt_record_never_leaves_out_a_post() {
    let mut fx = Fx::new(&[100, 400, 900]);
    let p = fx.sealed(0);
    fx.post(&p, 1_150).expect("post");
    let nr = result_hash_of(
        &fx.round.spec_id(),
        &fx.rid(),
        &ObservationOutcome::NoResult,
    );
    assert_eq!(
        fx.chain
            .record_attempt(&fx.query(&[NotRevealed; 3]), &nr, 1_300),
        Err("IncompleteTranscript")
    );
    let mut bad = fx.query(&[Revealed, NotRevealed, NotRevealed]);
    bad.commit_close = bad.reveal_close;
    assert_eq!(fx.chain.record_attempt(&bad, &nr, 1_300), Err("BadWindow"));
    fx.chain
        .record_attempt(&fx.query(&[Revealed, NotRevealed, NotRevealed]), &nr, 1_300)
        .expect("record");
}

/// Known limit, amendment 10 item 8b (C4-7): the coordinator holds the reveal
/// key, so it can open a post the moment it lands, before `reveal_close`, and
/// tell a colluding observer what the others answered. It cannot drop or change
/// the post. Closed only by the time-lock layer (ROADMAP C12).
#[test]
fn known_limit_the_coordinator_opens_a_post_before_reveal_close() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let p = fx.sealed(0);
    fx.post(&p, COMMIT_CLOSE + 1).expect("post");
    assert!(matches!(fx.open(0, &p), Opened::Reveal(_)));
}

/// Sizes: a numeric reveal seals in well under the registry's cap; more than
/// the cap is refused before anything is signed.
#[test]
fn c4_sealed_sizes() {
    let fx = Fx::new(&[100, 100, 100]);
    assert_eq!(fx.sealed(0).ciphertext.len(), 154);
    let (rid, attempt) = (fx.rid(), fx.attempt());
    assert_eq!(
        seal_bytes(
            &fx.keys[0].0,
            &fx.keys[0].1,
            &[7; 32],
            &fx.key(),
            &rid,
            attempt,
            &[0u8; MAX_SEALED_BYTES + 1],
        ),
        Err(SealError::TooLong)
    );
    assert_eq!(
        seal_bytes(
            &fx.keys[0].0,
            &fx.keys[0].1,
            &[0; 32],
            &fx.key(),
            &rid,
            attempt,
            b"x",
        ),
        Err(SealError::BadSecret)
    );
}

/// Review C4-R1-1 (High, fixed): the key proof was an ECDSA signature by the
/// ephemeral key, and ECDSA lets anyone make a signature that recovers to a
/// point Q related to another observer's R_A without knowing Q's secret. A
/// drawn observer B posted garbage under Q; the coordinator's honest dismissal
/// published `S_B = k·Q`, from which `S_A = r·S_B + e·K` opened A's post. The
/// attack is replayed here up to the post, which the Schnorr proof now refuses.
#[test]
fn c4_r1_1_a_key_the_poster_does_not_hold_never_lands() {
    use onyx_labels::attest::{eip191_digest, eip191_recover};
    use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
    use secp256k1::{Message, PublicKey, Scalar, Secp256k1};
    let secp = Secp256k1::new();
    let mut fx = Fx::new(&[100, 100, 100]);
    let (rid, attempt) = (fx.rid(), fx.attempt());
    let a = fx.sealed(0);
    let (b_sk, b) = fx.keys[1];
    // B's forged "signature" over a message of its choice: r = x(R_A), s = 1.
    let m = format!("any text B chooses for round {rid} attempt {attempt}");
    let e = eip191_digest(m.as_bytes());
    let ra = a.ephemeral;
    let mut compact = [0u8; 64];
    compact[..32].copy_from_slice(&ra[..32]);
    compact[63] = 1;
    let recid = RecoveryId::from_i32(i32::from(ra[63] & 1)).expect("recid");
    let rsig = RecoverableSignature::from_compact(&compact, recid).expect("sig");
    let q = secp
        .recover_ecdsa(&Message::from_digest(e.0), &rsig)
        .expect("recover");
    let mut qp = [0u8; 64];
    qp.copy_from_slice(&q.serialize_uncompressed()[1..]);
    let mut forged = [0u8; 65];
    forged[..64].copy_from_slice(&compact);
    forged[64] = 27 + (ra[63] & 1);
    // The old check (ECDSA recovers to addr(Q)) accepted it.
    assert_eq!(
        eip191_recover(m.as_bytes(), &forged),
        Some(point_address(&qp))
    );
    // What a dismissal of Q would have given anyone: S_A from S_B.
    let d = {
        let as_b = SealedPost {
            observer: b,
            ephemeral: qp,
            ciphertext: vec![0x42; 64],
            key_proof: [0; 52],
            sig: [0; 65],
        };
        dismiss(&REVEAL_SECRET, &as_b, &rid, attempt).expect("dismiss")
    };
    let to_pk = |p: &[u8; 64]| {
        let mut u = [4u8; 65];
        u[1..].copy_from_slice(p);
        PublicKey::from_slice(&u).expect("point")
    };
    let mut rb = [0u8; 32];
    rb.copy_from_slice(&ra[..32]);
    let s_a = to_pk(&d.shared)
        .mul_tweak(&secp, &Scalar::from_be_bytes(rb).expect("scalar"))
        .expect("mul")
        .combine(
            &to_pk(&fx.key())
                .mul_tweak(&secp, &Scalar::from_be_bytes(e.0).expect("scalar"))
                .expect("mul"),
        )
        .expect("add");
    let mut s_a_pt = [0u8; 64];
    s_a_pt.copy_from_slice(&s_a.serialize_uncompressed()[1..]);
    let s = spec();
    let (round_id, commit) = fx.ctx(0);
    let ctx = RevealContext {
        round_id,
        attempt,
        spec: &s,
        commit,
    };
    assert!(
        valid_reveal(&decrypt_with(&s_a_pt, &a, &rid, attempt), &a.observer, &ctx).is_some(),
        "the oracle the old check allowed is real"
    );
    // Now: B cannot post Q. Not with A's proof, not with a proof for a key it holds.
    let ct = vec![0x42u8; 64];
    let sig = onyx_labels::attest::eip191_sign_with(
        &b_sk,
        onyx_coordinator::sealed::sealed_reveal_message(&rid, attempt, &b, &qp, &keccak256(&ct))
            .as_bytes(),
    )
    .expect("sign");
    for proof in [
        a.key_proof,
        key_proof(&[7; 32], &rid, attempt, &b).expect("proof"),
    ] {
        assert!(!key_proof_valid(&qp, &rid, attempt, &b, &proof));
        let post = SealedPost {
            observer: b,
            ephemeral: qp,
            ciphertext: ct.clone(),
            key_proof: proof,
            sig,
        };
        assert_eq!(fx.post(&post, 1_150), Err("BadKeyProof"));
    }
    // A's own proof checks only for A in this round and attempt.
    assert!(key_proof_valid(
        &ra,
        &rid,
        attempt,
        &a.observer,
        &a.key_proof
    ));
    assert!(!key_proof_valid(&ra, &rid, attempt, &b, &a.key_proof));
    assert!(!key_proof_valid(
        &ra,
        &rid,
        attempt + 1,
        &a.observer,
        &a.key_proof
    ));
}

/// Review C4-R1-3: the registry keeps no signatures of a post, so the
/// coordinator opens, and a client checks a dismissal of, the post as read
/// from chain state (`sealedRevealOf`).
#[test]
fn c4_r1_3_posts_are_opened_and_dismissals_checked_from_chain_state() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let (rid, attempt, key) = (fx.rid(), fx.attempt(), fx.key());
    let good = fx.sealed(0);
    let junk = seal_bytes(
        &fx.keys[1].0,
        &fx.keys[1].1,
        &[7; 32],
        &key,
        &rid,
        attempt,
        b"junk",
    )
    .expect("seal");
    fx.post(&good, 1_150).expect("post");
    fx.post(&junk, 1_150).expect("post");
    let s = spec();
    for i in 0..2 {
        let observer = fx.keys[i].1;
        let posted = fx
            .chain
            .sealed_post(&fx.fact, attempt, &observer, &rid)
            .expect("on chain")
            .clone();
        let (round_id, commit) = fx.ctx(i);
        let ctx = RevealContext {
            round_id,
            attempt,
            spec: &s,
            commit,
        };
        match (
            i,
            open_posted(&REVEAL_SECRET, &observer, &posted, &ctx).expect("open"),
        ) {
            (0, Opened::Reveal(r)) => assert_eq!(r.0, observer),
            (1, Opened::Dismissed(d)) => {
                assert_eq!(
                    check_dismissal_posted(&key, &observer, &posted, &ctx, &d).expect("checks"),
                    b"junk"
                );
            }
            (i, o) => panic!("post {i} opened as {o:?}"),
        }
    }
}

/// Review C4-R3-2: the coordinator never dismisses a post read from state
/// whose key proof does not check, so a node lying about `sealedRevealOf`
/// (serving another observer's `R`, or a point related to it, in a garbage
/// slot) gets no `S` back: without the re-check the dismissal would open the
/// other observer's reveal, as in C4-R1-1.
#[test]
fn c4_r3_2_a_post_from_state_without_a_valid_key_proof_is_never_dismissed() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let (rid, attempt, key) = (fx.rid(), fx.attempt(), fx.key());
    let a = fx.sealed(0);
    fx.post(&a, 1_150).expect("post");
    let s = spec();
    let b = fx.keys[1].1;
    let (round_id, commit) = fx.ctx(1);
    let ctx = RevealContext {
        round_id,
        attempt,
        spec: &s,
        commit,
    };
    // A lying node serves B's slot as A's ephemeral key with junk, and A's
    // proof (for A's context) or none at all.
    for proof in [a.key_proof, [0u8; 52]] {
        let fake = PostedSealed {
            ephemeral: a.ephemeral,
            key_proof: proof,
            posted_at: 1_150,
            ciphertext: vec![0x55; a.ciphertext.len()],
        };
        assert_eq!(
            open_posted(&REVEAL_SECRET, &b, &fake, &ctx),
            Err(SealError::BadPost)
        );
        // Without the re-check, the dismissal's `S` opens A's post.
        let d = dismiss(
            &REVEAL_SECRET,
            &SealedPost::from_posted(&b, &fake),
            &rid,
            attempt,
        )
        .expect("dismiss");
        let (ra, ca) = fx.ctx(0);
        let actx = RevealContext {
            round_id: ra,
            attempt,
            spec: &s,
            commit: ca,
        };
        let opened = decrypt_with(&d.shared, &a, &rid, attempt);
        assert!(valid_reveal(&opened, &a.observer, &actx).is_some());
        assert_eq!(
            check_dismissal_posted(&key, &b, &fake, &ctx, &d),
            Err(SealError::BadPost)
        );
    }
    // The real post from state still opens.
    let posted = fx
        .chain
        .sealed_post(&fx.fact, attempt, &a.observer, &rid)
        .expect("on chain")
        .clone();
    assert_eq!(posted.key_proof, a.key_proof);
    let (ra, ca) = fx.ctx(0);
    let actx = RevealContext {
        round_id: ra,
        attempt,
        spec: &s,
        commit: ca,
    };
    assert!(matches!(
        open_posted(&REVEAL_SECRET, &a.observer, &posted, &actx),
        Ok(Opened::Reveal(_))
    ));
}

/// Review C4-R1-2: the registry takes entries only strictly ascending (no
/// observer twice); the drawn set itself is the client's check.
#[test]
fn c4_r1_2_entries_must_be_strictly_ascending() {
    let mut fx = Fx::new(&[100, 100, 100]);
    let p = fx.sealed(0);
    fx.post(&p, 1_150).expect("post");
    let ok = fx.query(&[Revealed, NotRevealed, NotRevealed]);
    assert!(fx.chain.complete(&ok));
    let mut swapped = ok.clone();
    swapped.entries.swap(0, 1);
    assert!(!fx.chain.complete(&swapped));
    let mut twice = ok.clone();
    twice.entries[1] = twice.entries[0];
    assert!(!fx.chain.complete(&twice));
}

/// Review C4-R2-1: every sealed reveal of a feed has ONE length (a numeric
/// value or `Unavailable`; exact answers of any length up to `max_len`), so a
/// ciphertext's length shows nothing about the answer; padding that is not all
/// zeros is garbage. C4-R2-2: an exact answer of `MAX_VALUE_BYTES` seals within
/// the registry's cap.
#[test]
fn c4_r2_1_sealed_posts_of_a_feed_all_have_one_length() {
    let fx = Fx::new(&[100, 100, 100]);
    let (rid, attempt, key) = (fx.rid(), fx.attempt(), fx.key());
    let (sk, a) = fx.keys[0];
    let numeric = spec();
    let unavailable = Observation {
        answer: Answer::Unavailable,
        observed_at: 1_010,
        page_hash: Hash32::ZERO,
    };
    let len_of = |s: &ObservationSpec, o: &Observation| {
        seal(&sk, &[7; 32], &key, &rid, attempt, s, o, &Hash32([1; 32]))
            .expect("seal")
            .ciphertext
            .len()
    };
    assert_eq!(len_of(&numeric, &obs(100)), sealed_len(&numeric));
    assert_eq!(len_of(&numeric, &obs(i128::MAX)), sealed_len(&numeric));
    assert_eq!(len_of(&numeric, &unavailable), sealed_len(&numeric));
    let mut exact = spec();
    exact.kind = ObservationKind::Exact { max_len: 1024 };
    let bytes = |n: usize| Observation {
        answer: Answer::Bytes(vec![b'y'; n]),
        observed_at: 1_010,
        page_hash: keccak256(b"page"),
    };
    for n in [1, 2, 3, 500, 1024] {
        assert_eq!(
            len_of(&exact, &bytes(n)),
            sealed_len(&exact),
            "answer of {n} bytes"
        );
    }
    assert_eq!(len_of(&exact, &unavailable), sealed_len(&exact));
    assert_eq!(sealed_len(&exact), MAX_PLAIN_BYTES);
    // Non-zero padding: garbage, dismissed.
    let salt = Hash32([1; 32]);
    let o = obs(100);
    let commit = observation_commit(&rid, &a, &o, &salt);
    let sig = onyx_coordinator::observe::sign_reveal(&sk, &rid, attempt, &o, &salt).expect("sign");
    let mut plain = borsh::to_vec(&onyx_coordinator::sealed::Plain {
        observation: o,
        salt,
        sig,
    })
    .expect("borsh");
    let ctx = RevealContext {
        round_id: rid,
        attempt,
        spec: &numeric,
        commit: Some(commit),
    };
    assert!(valid_reveal(&plain, &a, &ctx).is_some());
    plain.push(0);
    assert!(
        valid_reveal(&plain, &a, &ctx).is_none(),
        "longer than the feed's length"
    );
    plain.pop();
    let mut exact_plain = plain.clone();
    exact_plain.resize(sealed_len(&exact), 0);
    let ectx = RevealContext {
        spec: &exact,
        ..ctx
    };
    assert!(
        valid_reveal(&exact_plain, &a, &ectx).is_none(),
        "a number is not an exact answer"
    );
    let mut padded = borsh::to_vec(&onyx_coordinator::sealed::Plain {
        observation: bytes(3),
        salt,
        sig: onyx_coordinator::observe::sign_reveal(&sk, &rid, attempt, &bytes(3), &salt)
            .expect("sign"),
    })
    .expect("borsh");
    padded.resize(sealed_len(&exact), 0);
    let ectx = RevealContext {
        commit: Some(observation_commit(&rid, &a, &bytes(3), &salt)),
        ..ectx
    };
    assert!(valid_reveal(&padded, &a, &ectx).is_some());
    let last = padded.len() - 1;
    padded[last] = 1;
    assert!(
        valid_reveal(&padded, &a, &ectx).is_none(),
        "non-zero padding"
    );
}

/// Review C4-R2-3: well-formed forged key proofs for an `R` whose secret the
/// forger does not hold never verify: random `(T, z)`, `R`'s own proof for `−R`
/// or for `R + G`, and any point with `R.x >= n` (refused outright).
#[test]
fn c4_r2_3_forged_key_proofs_never_verify() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};
    let secp = Secp256k1::new();
    let (rid, attempt) = (keccak256(b"round"), 0u32);
    let obsr = [9u8; 20];
    let r = keccak256(b"ephemeral").0;
    let rp = public_point(&r).expect("point");
    let proof = key_proof(&r, &rid, attempt, &obsr).expect("proof");
    assert!(key_proof_valid(&rp, &rid, attempt, &obsr, &proof));
    for i in 0..256u32 {
        let mut f = [0u8; 52];
        f[..32].copy_from_slice(&keccak256(&i.to_be_bytes()).0);
        f[32..].copy_from_slice(&keccak256(&(i + 1_000).to_be_bytes()).0[..20]);
        assert!(!key_proof_valid(&rp, &rid, attempt, &obsr, &f));
    }
    let to_pt = |k: &PublicKey| {
        let mut p = [0u8; 64];
        p.copy_from_slice(&k.serialize_uncompressed()[1..]);
        p
    };
    let mut u = [4u8; 65];
    u[1..].copy_from_slice(&rp);
    let rk = PublicKey::from_slice(&u).expect("point");
    let neg = to_pt(&rk.negate(&secp));
    let mut one = [0u8; 32];
    one[31] = 1;
    let g = SecretKey::from_slice(&one).expect("one").public_key(&secp);
    let plus_g = to_pt(&rk.combine(&g).expect("add"));
    assert!(!key_proof_valid(&neg, &rid, attempt, &obsr, &proof));
    assert!(!key_proof_valid(&plus_g, &rid, attempt, &obsr, &proof));
    // A point with x >= n (x in [n, p)): never accepted, whatever the proof.
    let n: [u8; 32] = [
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFE, 0xBA, 0xAE, 0xDC, 0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36,
        0x41, 0x41,
    ];
    let high = (0u8..=255)
        .find_map(|i| {
            let mut c = [2u8; 33];
            c[1..].copy_from_slice(&n);
            c[32] = c[32].wrapping_add(i);
            PublicKey::from_slice(&c).ok()
        })
        .expect("a point with x >= n");
    let hp = to_pt(&high);
    assert!(hp[..32] >= n[..]);
    assert!(!key_proof_valid(&hp, &rid, attempt, &obsr, &proof));
}
