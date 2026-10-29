//! ROADMAP C51: time-locked sealed posts wired end to end (ADR-0027 amendment
//! 14; THREATS C4-7, C11-R2-1, C12-7, C12-9, C51-*).
//!
//! The registry requires time-locks (`requireTimeLock`, one way), so the draw
//! is keyed and time-locked (`drawTimeLocked`). Independent observers seal
//! with drand/tlock's real IBE (`QuicknetTimeLock`) to the lock round of the
//! window's `reveal_close`; the live runner reads every drawn slot from one
//! finalized block past `reveal_close`, then waits for that round's beacon
//! signature (from a fixture of PUBLISHED quicknet rounds, verified against the
//! pinned key) and only then opens or dismisses each post; dismissals keep
//! their time-lock openings, the round book and the served round carry them,
//! and the Rust client re-encrypts each opening byte for byte
//! (`open_canonical`) or checks a time-lock that does not open under the
//! verified signature, and asks the chain `drawTimeLocked`.
//!
//! Adversarial: no beacon (drand down or late), a forged signature, another
//! round's signature: the round WAITS and nothing is opened; posts locked to
//! another round, or plain amendment 10 posts in a time-locked draw, are
//! dismissed (never opened early); a server that hides the time-lock, forges
//! the signature, changes or drops an opening, or names another lock round is
//! refused. No network: every signature comes from
//! `fixtures/tlock/drand/quicknet-c51-rounds.json`.

use onyx_api::client::{verify_attempts, verify_sealed_onchain, ObservationExpect, SealedRets};
use onyx_api::observations::{ObservationFeed, ObservationStore, SealedChecked, TemplatePins};
use onyx_coordinator::keyfile::{key_address, ReceiptKey, RevealKey};
use onyx_coordinator::observe::{
    test_keys, ChainWait, DrawChain, ObservationOutcome, ObservationResults, Observer, ReplayClock,
    RevealInbox, SignedReveal, SimulatedRegistry,
};
use onyx_coordinator::sealed::{
    posted_returns, seal, seal_bytes, sealed_len, Point, SealedPost, TIME_LOCKED_HEAD_BYTES,
};
use onyx_coordinator::timelock::{
    lock_round, seal_bytes_timelocked, seal_timelocked, BeaconChain, FixtureBeacon, LockRounds,
    LockSecrets, QuicknetTimeLock, TimeLock, TLOCK_LEN,
};
use onyx_core::{keccak256, Answer, Hash32, MinerAddress, ObservationSpec};
use onyx_source_fiscaldata::round::{
    append_results, config, drawn_round, observer_turn, parse_pool, run_and_save_with, store,
    Observers, Page, RoundRun,
};
use onyx_source_fiscaldata::{recorded, spec, template, RECORDED_BODY, RECORDED_WINDOW_START};
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

mod hex {
    pub fn encode(b: impl AsRef<[u8]>) -> String {
        b.as_ref().iter().map(|x| format!("{x:02x}")).collect()
    }
}

/// Total public debt outstanding on 2026-10-08, in cents, as recorded.
const RECORDED_CENTS: i128 = 4_030_531_621_082_972;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/tlock/drand/quicknet-c51-rounds.json"
));

fn fixture() -> FixtureBeacon {
    FixtureBeacon::from_json(FIXTURE).expect("the C51 fixture parses")
}

fn feed() -> ObservationSpec {
    spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec")
}

/// The lock round of the recorded window: published, in the fixture.
fn rho() -> u64 {
    lock_round(&BeaconChain::quicknet(), config(&feed()).reveal_close).expect("a lock round")
}

/// The recorded window's lock and far rounds (ROADMAP C52: the far round from
/// the template, the record date's last observable day).
fn rounds_of(reveal_close: u64) -> LockRounds {
    LockRounds::of_round(&BeaconChain::quicknet(), &template(), &feed(), reveal_close)
        .expect("lock rounds")
}

fn keys() -> Vec<[u8; 32]> {
    (0..7)
        .map(|i| test_keys::secret("c51 observer", i))
        .collect()
}

fn pool() -> Vec<Observer> {
    let text = serde_json::to_string(
        &keys()
            .iter()
            .map(|k| {
                serde_json::json!({
                    "address": format!("0x{}", hex::encode(key_address(k).expect("key"))),
                    "bond": 1_000,
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("json");
    parse_pool(&text).expect("pool")
}

fn reveal_key() -> RevealKey {
    RevealKey::from_secret(test_keys::secret("c51 reveal", 0)).expect("reveal key")
}

/// What one drawn observer process posts.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    /// Its signed reveal, time-locked to the lock round.
    Honest,
    /// Junk behind a VALID time-lock (dismissed with the opening).
    Garbage,
    /// Junk of the same length with no valid time-lock in front.
    Raw,
    /// Its true signed reveal as an amendment 10 post, no time-lock (an
    /// observer that lets the coordinator read early: dismissed, never opened).
    Plain,
    /// Its signed reveal time-locked to an EARLIER round (already out at
    /// reveal close: dismissed, never opened).
    WrongRound,
    /// Commits, never posts.
    Withhold,
    /// ROADMAP C52: its signed reveal behind a valid time-lock, but with a far
    /// copy of ANOTHER key (the round could not unseal without the
    /// coordinator): dismissed at intake with its opening.
    BadFar,
}

struct Processes {
    reg: Rc<RefCell<SimulatedRegistry>>,
    acts: Vec<Act>,
    drawn: Vec<MinerAddress>,
    posted: bool,
}

fn secrets(i: usize) -> LockSecrets {
    let e = keccak256(&[b'e', u8::try_from(i).expect("small")]).0;
    let x = keccak256(&[b'x', u8::try_from(i).expect("small")]).0;
    let mut tau = [0u8; 16];
    let mut sigma = [0u8; 16];
    tau.copy_from_slice(&x[..16]);
    sigma.copy_from_slice(&x[16..]);
    let mut kappa = [0u8; 16];
    kappa.copy_from_slice(&keccak256(&[b'k', u8::try_from(i).expect("small")]).0[..16]);
    LockSecrets {
        ephemeral: e,
        tau,
        sigma,
        kappa,
    }
}

#[allow(clippy::too_many_arguments)]
fn post_for(
    act: Act,
    i: usize,
    sk: &[u8; 32],
    me: &MinerAddress,
    key: &Point,
    round: &Hash32,
    attempt: u32,
    reveal: &SignedReveal,
    now: u64,
) -> SealedPost {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let sp = feed();
    let rc = rounds_of(config(&sp).reveal_close);
    let (_, o, salt, _) = reveal;
    let s = secrets(i);
    match act {
        Act::Honest => {
            seal_timelocked(&q, sk, &s, key, round, attempt, rc, now, &sp, o, salt).expect("seal")
        }
        Act::Garbage => seal_bytes_timelocked(
            &q,
            sk,
            me,
            &s,
            key,
            round,
            attempt,
            rc,
            now,
            &vec![7u8; sealed_len(&sp)],
        )
        .expect("seal"),
        Act::Raw => seal_bytes(
            sk,
            me,
            &s.ephemeral,
            key,
            round,
            attempt,
            &vec![9u8; TIME_LOCKED_HEAD_BYTES + sealed_len(&sp)],
        )
        .expect("seal"),
        // ROADMAP C52: the registry refuses a plain 154-byte post in a
        // time-locked draw (272 bytes come before the body), so the colluding
        // observer pads its amendment 10 plaintext to a time-locked post's
        // length; it lands and is still never opened early.
        Act::Plain => {
            assert!(
                seal(sk, &s.ephemeral, key, round, attempt, &sp, o, salt)
                    .expect("seal")
                    .ciphertext
                    .len()
                    <= TIME_LOCKED_HEAD_BYTES,
                "an unpadded amendment 10 post is too short to land"
            );
            // Padded to a time-locked post's length: `seal` pads to the
            // spec's sealed length, so an exact spec of 284 bytes gives 426.
            let padded = ObservationSpec {
                kind: onyx_core::ObservationKind::Exact {
                    max_len: u16::try_from(TIME_LOCKED_HEAD_BYTES + sealed_len(&sp) - 142)
                        .expect("small"),
                },
                ..sp.clone()
            };
            let p = seal(sk, &s.ephemeral, key, round, attempt, &padded, o, salt).expect("seal");
            assert_eq!(p.ciphertext.len(), TIME_LOCKED_HEAD_BYTES + sealed_len(&sp));
            p
        }
        // Locked as if reveal close were 30 s earlier: ten rounds before the
        // lock round (its seal-time check passes on this test clock).
        Act::WrongRound => seal_timelocked(
            &q,
            sk,
            &s,
            key,
            round,
            attempt,
            rounds_of(config(&sp).reveal_close - 30),
            now - 60,
            &sp,
            o,
            salt,
        )
        .expect("seal"),
        Act::BadFar => {
            use onyx_coordinator::timelock::{body_key, far_copy, seal_parts, wrap_key};
            let honest = seal_timelocked(&q, sk, &s, key, round, attempt, rc, now, &sp, o, salt)
                .expect("seal");
            let lock: [u8; TLOCK_LEN] = honest.ciphertext[..TLOCK_LEN].try_into().expect("tl");
            let good: [u8; TLOCK_LEN] = honest.ciphertext[TLOCK_LEN..2 * TLOCK_LEN]
                .try_into()
                .expect("tf");
            let chain = onyx_coordinator::timelock::QUICKNET_CHAIN_HASH;
            let eph = honest.ephemeral;
            let k = |far: &[u8; TLOCK_LEN]| {
                body_key(&s.kappa, &eph, &lock, far, rc, &chain, round, attempt, me)
            };
            let plain = onyx_coordinator::sealed::stream_xor(
                &k(&good),
                &honest.ciphertext[TIME_LOCKED_HEAD_BYTES..],
            );
            let bad = far_copy(&q, rc.far(), &[0x66; 16]).expect("far");
            let shared = onyx_coordinator::sealed::shared_point(&s.ephemeral, key).expect("S");
            let wrap = wrap_key(
                &shared, &s.tau, &eph, &lock, &bad, rc, &chain, round, attempt, me,
            );
            let w: Vec<u8> = wrap.iter().zip(s.kappa).map(|(a, b)| a ^ b).collect();
            seal_parts(
                sk,
                me,
                &s.ephemeral,
                &eph,
                round,
                attempt,
                &[
                    &lock,
                    &bad,
                    &w,
                    &onyx_coordinator::sealed::stream_xor(&k(&bad), &plain),
                ],
            )
            .expect("seal")
        }
        Act::Withhold => unreachable!("posts nothing"),
    }
}

impl RevealInbox for Processes {
    fn take(&mut self, round: &Hash32, observers: &[MinerAddress], now: u64) -> Vec<SignedReveal> {
        let sp = feed();
        let fact = store().onchain_fact(&sp).expect("fact");
        if !self.posted {
            self.posted = true;
            let mut reg = self.reg.borrow_mut();
            let n = reg.draw_count(&fact).expect("count");
            let rec = reg.draw_of(&fact, n - 1).expect("read").expect("a draw");
            let mut turns = vec![];
            for k in keys() {
                let me = key_address(&k).expect("key");
                let Some((seed, drawn, id)) =
                    drawn_round(&sp, &pool(), &rec, &me).expect("the draw")
                else {
                    continue;
                };
                assert_eq!(id, *round);
                assert_eq!(
                    drawn.iter().map(|o| o.address).collect::<Vec<_>>(),
                    observers
                );
                let page = Page {
                    fetched_unix: sp.window_start + 10,
                    body: Some(RECORDED_BODY.to_vec()),
                };
                let (commit, sig, reveal) =
                    observer_turn(&sp, &id, seed.attempt, &k, &page).expect("turn");
                reg.time = sp.window_start + 20;
                reg.post_commit(&fact, seed.attempt, &me, &id, &commit, &sig)
                    .expect("postCommit in phase");
                self.drawn.push(me);
                turns.push((k, me, seed.attempt, reveal));
            }
            let attempt = turns[0].2;
            assert!(
                reg.draw_time_locked(&fact, attempt).expect("read"),
                "the draw is time-locked"
            );
            let key = reg
                .draw_reveal_key(&fact, attempt)
                .expect("read")
                .expect("a reveal key");
            for (i, (k, me, _, reveal)) in turns.iter().enumerate() {
                if self.acts[i] == Act::Withhold {
                    continue;
                }
                let p = post_for(self.acts[i], i, k, me, &key, round, attempt, reveal, now);
                reg.time = now;
                reg.post_sealed(&fact, attempt, round, &p)
                    .expect("postSealedReveal");
            }
        }
        self.reg.borrow_mut().time = now;
        vec![]
    }
}

/// A live round and what it left behind; the book stays on disk so a later
/// run can finish the same draw ([`Harness::again`]).
struct Harness {
    reg: Rc<RefCell<SimulatedRegistry>>,
    /// What the coordinator's beacon source holds on the next run.
    beacon: Option<FixtureBeacon>,
    inbox: Processes,
    dir: std::path::PathBuf,
    written: Rc<RefCell<Option<Value>>>,
}

fn suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

impl Harness {
    fn new(acts: Vec<Act>, beacon: Option<FixtureBeacon>) -> Self {
        let reg = Rc::new(RefCell::new(SimulatedRegistry::new()));
        {
            let mut r = reg.borrow_mut();
            r.receipt_signer = key_address(&receipt()).expect("key");
            r.reveal_key = Some(reveal_key().point());
            r.records = true;
            r.require_time_lock();
        }
        let dir =
            std::env::temp_dir().join(format!("onyx-c51-{}-{}", std::process::id(), suffix()));
        std::fs::create_dir_all(&dir).expect("dir");
        Harness {
            beacon,
            inbox: Processes {
                reg: reg.clone(),
                acts,
                drawn: vec![],
                posted: false,
            },
            reg,
            dir,
            written: Rc::new(RefCell::new(None)),
        }
    }

    /// One run (a fresh store reading the book, as a restarted coordinator).
    fn go(&mut self) -> Result<RoundRun, String> {
        let book = self.dir.join("book.json");
        let mut s: ObservationResults = store();
        if book.exists() {
            s.load_book(&book).expect("the book loads");
        }
        let mut chain = Shared(self.reg.clone(), self.beacon.clone());
        let written = self.written.clone();
        let rk = reveal_key();
        let r = run_and_save_with(
            &recorded().record_date,
            RECORDED_WINDOW_START,
            Observers::Independent(&pool()),
            &mut s,
            &mut chain,
            &book,
            None,
            &ChainWait::NOW,
            &ReplayClock::default(),
            &mut self.inbox,
            &ReceiptKey::from_file(&receipt_file(&self.dir)).expect("receipt key"),
            Some(&rk),
            &mut |r| {
                *written.borrow_mut() = Some(append_results(None, r)?);
                Ok(())
            },
            &mut |_| Ok(()),
        )
        .map_err(|e| e.error);
        r
    }

    fn book(&self) -> Value {
        std::fs::read_to_string(self.dir.join("book.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The runner's chain: the shared registry, and the coordinator's beacon
/// source (fixture rounds, or none).
struct Shared(Rc<RefCell<SimulatedRegistry>>, Option<FixtureBeacon>);

macro_rules! fwd {
    ($($name:ident(&self $(, $a:ident: $t:ty)*) -> $r:ty;)*) => {
        $(fn $name(&self $(, $a: $t)*) -> $r { self.0.borrow().$name($($a),*) })*
    };
}
macro_rules! fwd_mut {
    ($($name:ident(&mut self $(, $a:ident: $t:ty)*) -> $r:ty;)*) => {
        $(fn $name(&mut self $(, $a: $t)*) -> $r { self.0.borrow_mut().$name($($a),*) })*
    };
}

impl onyx_coordinator::observe::ChainView for Shared {
    fwd! {
        height(&self) -> Result<u64, String>;
        block_hash(&self, h: u64) -> Result<Option<Hash32>, String>;
    }
}

use onyx_coordinator::observe::{
    DrawRecord, LogBlock, ObserverSig, OnchainDraw, OnchainFact, OnchainObservationCheck, Posted,
    PostedCommit, Recorded, RegistryDeployment, TranscriptLogQuery,
};
use onyx_coordinator::sealed::{PostedSealed, RevealLogQuery};

impl DrawChain for Shared {
    fwd! {
        deployment(&self) -> Result<Option<RegistryDeployment>, String>;
        draw_count(&self, f: &OnchainFact) -> Result<u32, String>;
        draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String>;
        draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String>;
        draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String>;
        log_block(&self) -> Result<Option<LogBlock>, String>;
        posted_commit(&self, b: u64, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32) -> Result<Option<PostedCommit>, String>;
        transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String>;
        draw_receipt_signer(&self, f: &OnchainFact, a: u32) -> Result<Option<[u8; 20]>, String>;
        draw_reveal_key(&self, f: &OnchainFact, a: u32) -> Result<Option<Point>, String>;
        draw_time_locked(&self, f: &OnchainFact, a: u32) -> Result<bool, String>;
        sealed_post(&self, b: u64, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32) -> Result<Option<PostedSealed>, String>;
        reveal_log_complete(&self, q: &RevealLogQuery) -> Result<bool, String>;
        records_rounds(&self) -> bool;
    }
    fwd_mut! {
        announce(&mut self, d: &OnchainDraw) -> Result<(), String>;
        fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String>;
        post_commit(&mut self, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32, c: &Hash32, s: &ObserverSig) -> Result<Posted, String>;
        post_receipt(&mut self, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32, c: &Hash32, s: &ObserverSig) -> Result<Posted, String>;
        record_attempt(&mut self, q: &RevealLogQuery, r: &Hash32) -> Result<Recorded, String>;
        record_observation(&mut self, rec: &OnchainObservationCheck) -> Result<Recorded, String>;
    }
    fn beacon(&self) -> Option<&dyn onyx_coordinator::timelock::BeaconSource> {
        self.1
            .as_ref()
            .map(|b| b as &dyn onyx_coordinator::timelock::BeaconSource)
    }
}

fn receipt() -> [u8; 32] {
    test_keys::secret("c51 receipt", 0)
}

fn receipt_file(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("receipt.key");
    std::fs::write(&p, hex::encode(receipt())).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    p
}

/// The checker's clock: just past the round.
fn now() -> u64 {
    config(&feed()).reveal_close + 10
}

fn served(store: &Value) -> ObservationFeed {
    let t = template();
    let pins = TemplatePins::from([(t.metric.0.clone(), t.id())]);
    ObservationStore::from_json(&store.to_string(), now(), &pins).expect("the server loads it")
}

fn expect<'a>(
    t: &'a onyx_core::FeedTemplate,
    subject: &'a onyx_core::Subject,
) -> ObservationExpect<'a> {
    ObservationExpect {
        template: t,
        subject,
        window_end: None,
        min_window_end: 0,
        now: now(),
    }
}

fn word(b: bool) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[31] = u8::from(b);
    w
}

/// What the (simulated) registry answers to the client's sealed calls for `c`.
fn chain_returns(reg: &SimulatedRegistry, c: &SealedChecked) -> SealedRets {
    let q = &c.query;
    SealedRets {
        time_locked: word(
            reg.sealed
                .time_locked
                .contains(&(q.registry_fact, q.attempt)),
        ),
        reveal_key: reg
            .sealed
            .keys
            .get(&(q.registry_fact, q.attempt))
            .map_or(vec![0u8; 64], |k| k.to_vec()),
        reveal_log: word(reg.sealed.complete_with(&reg.log, q)),
        matches: (!c.value).then(|| word(reg.sealed.attempt_matches(q, &c.result))),
        posts: c
            .posts
            .iter()
            .map(|(a, _)| {
                posted_returns(
                    reg.sealed
                        .sealed_post(&q.registry_fact, q.attempt, a, &q.round_id),
                )
            })
            .collect(),
    }
}

fn subject_text() -> &'static str {
    match feed().subject {
        onyx_core::Subject::Aggregate(s) => Box::leak(s.into_boxed_str()),
        _ => unreachable!("an aggregate subject"),
    }
}

/// The served earlier rounds of the fact (sealed: the fact is not closed).
fn attempts_body(h: &Harness) -> Value {
    let f = served(h.written.borrow().as_ref().expect("written"));
    let attempts = f.attempts_of(&f.attempt_fact(subject_text(), None).expect("fact"));
    f.attempts_body(&attempts, false)
}

fn client(body: &Value, reg: &SimulatedRegistry) -> Result<(), String> {
    let t = template();
    let sp = feed();
    let a =
        verify_attempts(body, &expect(&t, &sp.subject)).map_err(|e| format!("off chain: {e}"))?;
    for c in &a.attempts {
        verify_sealed_onchain(&c.reveal_log, &chain_returns(reg, &c.reveal_log))
            .map_err(|e| format!("on chain: {e}"))?;
    }
    Ok(())
}

#[test]
fn c51_the_fixture_holds_the_recorded_windows_lock_round_and_every_entry_verifies() {
    let q = QuicknetTimeLock::pinned().expect("pinned");
    let f = fixture();
    let rounds: Vec<u64> = f.rounds().collect();
    assert!(
        rounds.contains(&rho()),
        "lock round {} in the fixture",
        rho()
    );
    for r in rounds {
        let sig = onyx_coordinator::timelock::BeaconSource::signature(&f, r)
            .expect("read")
            .expect("held");
        assert!(q.verify_signature(r, &sig), "round {r} is quicknet's");
        assert!(!q.verify_signature(r + 1, &sig), "round {r}'s only");
    }
    // Another chain's file is refused; a duplicated round is refused.
    let other = FIXTURE.replace(
        "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971",
        "dbd506d6ef76e5f386f41c651dcb808c5bcbd75471cc4eafa3f4df7ad4e4c493",
    );
    assert!(FixtureBeacon::from_json(&other).is_err());
}

/// ROADMAP C51: five drawn observers post time-locked sealed reveals; the
/// coordinator reads every slot past reveal close, takes the lock round's
/// signature from the fixture (verified against the pinned key), opens each
/// post and settles the recorded value; the served value round names the time
/// lock and passes the client off and on chain (`drawTimeLocked` true).
#[test]
fn c51_a_time_locked_round_settles_with_the_lock_rounds_signature() {
    let mut h = Harness::new(vec![Act::Honest; 5], Some(fixture()));
    let r = h.go().expect("the round runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert_eq!(
        r.reveals.len(),
        5,
        "every reveal came from a time-locked post"
    );
    let t = r.sealed.time_lock.as_ref().expect("served time-locked");
    assert_eq!(t.lock_round, rho());
    assert!(t.openings.is_empty() && r.sealed.dismissals.is_empty());
    for (_, p) in &r.sealed.posts {
        assert_eq!(
            p.ciphertext.len(),
            TIME_LOCKED_HEAD_BYTES + sealed_len(&feed())
        );
    }
    let f = served(h.written.borrow().as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let body = f.body(rec);
    assert_eq!(body["sealed"]["timeLock"]["lockRound"], rho());
    let tpl = template();
    let sp = feed();
    let v = onyx_api::client::verify_observation(&body, &expect(&tpl, &sp.subject))
        .expect("the client checks it");
    assert!(v.checked.reveal_log.time_locked);
    let reg = h.reg.borrow();
    assert_eq!(
        verify_sealed_onchain(
            &v.checked.reveal_log,
            &chain_returns(&reg, &v.checked.reveal_log)
        ),
        Ok(())
    );
    // A forged (or another round's) signature is refused even when no
    // dismissal needs it: the served read must be the lock round's.
    let mut forged = body.clone();
    forged["sealed"]["timeLock"]["beacon"] =
        Value::from(format!("0x{}", hex::encode(sig_of(rho() + 1))));
    assert!(matches!(
        onyx_api::client::verify_observation(&forged, &expect(&tpl, &sp.subject)),
        Err(onyx_api::client::VerifyError::Observation(e)) if e.contains("signature")
    ));
    // Served without its time-lock: off chain the posts then look like amendment
    // 10 posts revealed by the server's word, and the chain's drawTimeLocked
    // refuses it.
    let mut bare = body.clone();
    bare["sealed"]
        .as_object_mut()
        .expect("sealed")
        .remove("timeLock");
    let v2 = onyx_api::client::verify_observation(&bare, &expect(&tpl, &sp.subject))
        .expect("off chain alone cannot tell");
    assert!(matches!(
        verify_sealed_onchain(&v2.checked.reveal_log, &chain_returns(&reg, &v2.checked.reveal_log)),
        Err(onyx_api::client::VerifyError::ChainMismatch(e)) if e.starts_with("drawTimeLocked")
    ));
}

/// Garbage in a time-locked draw: junk behind a valid time-lock is dismissed
/// WITH its opening (a client re-encrypts it byte for byte, no signature
/// needed); junk without a valid time-lock, a plain amendment 10 post and a post
/// locked to an earlier round are dismissed WITHOUT one (a client checks they
/// do not open under the verified lock-round signature). None is ever opened
/// as a reveal, so a post that skips or shortens the time-lock gives the
/// coordinator nothing early. Recorded with `recordAttempt`; served and checked.
#[test]
fn c51_dismissals_carry_openings_and_every_way_around_the_time_lock_is_dismissed() {
    let acts = vec![
        Act::Garbage,
        Act::Raw,
        Act::Plain,
        Act::WrongRound,
        Act::Honest,
    ];
    let mut h = Harness::new(acts.clone(), Some(fixture()));
    let r = h.go().expect("the round runs");
    assert_eq!(*r.settlement.outcome(), ObservationOutcome::NoResult);
    assert_eq!(r.reveals.len(), 1, "only the honest time-locked post opens");
    assert_eq!(r.sealed.dismissals.len(), 4);
    let t = r.sealed.time_lock.as_ref().expect("time-locked");
    assert_eq!(
        t.openings.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
        vec![h.inbox.drawn[0]],
        "only the junk behind a valid time-lock has an opening"
    );
    assert!(r.sealed.record.is_some(), "recordAttempt before serving");
    let body = attempts_body(&h);
    let reg = h.reg.borrow();
    assert_eq!(client(&body, &reg), Ok(()));
    let tl = |b: &mut Value| b["attempts"][0]["sealed"]["timeLock"].take();
    // The opening changed (tau or sigma): it no longer re-encrypts.
    let mut changed = body.clone();
    let tau = changed["attempts"][0]["sealed"]["timeLock"]["openings"][0]["tau"]
        .as_str()
        .expect("tau")
        .to_owned();
    let flipped = format!(
        "{}{}",
        &tau[..tau.len() - 1],
        if tau.ends_with('0') { '1' } else { '0' }
    );
    changed["attempts"][0]["sealed"]["timeLock"]["openings"][0]["tau"] = Value::from(flipped);
    assert!(client(&changed, &reg)
        .expect_err("refused")
        .contains("time-lock"));
    // The opening dropped: the time-lock opens under the signature, so the
    // coordinator owed it (a false dismissal).
    let mut dropped = body.clone();
    dropped["attempts"][0]["sealed"]["timeLock"]["openings"] = Value::Array(vec![]);
    assert!(client(&dropped, &reg).is_err());
    // A forged lock-round signature, or another (published) round's.
    let mut forged = body.clone();
    let other = onyx_coordinator::timelock::BeaconSource::signature(&fixture(), rho() + 1)
        .expect("read")
        .expect("held");
    forged["attempts"][0]["sealed"]["timeLock"]["beacon"] =
        Value::from(format!("0x{}", hex::encode(other)));
    assert!(client(&forged, &reg)
        .expect_err("refused")
        .contains("signature"));
    // Another lock round named (with that round's real signature).
    let mut moved = forged.clone();
    moved["attempts"][0]["sealed"]["timeLock"]["lockRound"] = Value::from(rho() + 1);
    assert!(client(&moved, &reg)
        .expect_err("refused")
        .contains("lock round"));
    // The time-lock hidden: the dismissals no longer check as amendment 10
    // ones, or the chain's drawTimeLocked refuses it.
    let mut hidden = body.clone();
    let _ = tl(&mut hidden);
    hidden["attempts"][0]["sealed"]
        .as_object_mut()
        .expect("sealed")
        .remove("timeLock");
    assert!(client(&hidden, &reg).is_err());
    // A time-lock read claimed for a round of a draw that is not time-locked
    // is refused on chain (simulated by the chain's answer).
    let t2 = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t2, &sp.subject)).expect("checks");
    let c = &a.attempts[0].reveal_log;
    let mut rets = chain_returns(&reg, c);
    rets.time_locked = word(false);
    assert!(verify_sealed_onchain(c, &rets).is_err());
}

/// THREATS C12-4 (drand late or down): without a beacon source, or with one
/// that has not published the lock round, the round WAITS: the run stops with
/// the round unfinished in the book and no post opened (no sealed snapshot is
/// kept, nothing recorded); a later run with the round published finishes the
/// SAME draw and loses nothing.
#[test]
fn c51_without_the_lock_round_the_round_waits_and_a_late_beacon_loses_nothing() {
    let mut h = Harness::new(vec![Act::Honest; 5], None);
    let e = h.go().expect_err("no beacon source: waits");
    assert!(e.contains("waits"), "{e}");
    // A source that does not hold the lock round yet (drand late).
    h.beacon = Some(FixtureBeacon::of([(rho() - 1, sig_of(rho() - 1))]));
    let e = h.go().expect_err("not published: waits");
    assert!(e.contains("waits") && e.contains(&rho().to_string()), "{e}");
    assert_eq!(h.book()["version"], 8);
    {
        let reg = h.reg.borrow();
        let fact = store().onchain_fact(&feed()).expect("fact");
        assert_eq!(reg.draw_count(&fact).expect("count"), 1, "no new draw");
        assert!(reg.observations.is_empty(), "nothing recorded");
    }
    let mut kept = store();
    kept.load_book(&h.dir.join("book.json")).expect("loads");
    let u = kept
        .kept_round(&feed())
        .expect("the round is kept unfinished");
    assert!(
        u.round.sealed_snapshot().is_none(),
        "nothing read or opened"
    );
    // drand catches up: the same draw settles.
    h.beacon = Some(fixture());
    let r = h.go().expect("finishes on the same draw");
    assert_eq!(r.draw.attempt, 0);
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
}

fn sig_of(round: u64) -> Vec<u8> {
    onyx_coordinator::timelock::BeaconSource::signature(&fixture(), round)
        .expect("read")
        .expect("held")
}

/// A forged signature, another round's real signature and junk bytes under
/// the lock round's number are all refused by the pinned key: the round waits,
/// nothing is opened (the source is never trusted).
#[test]
fn c51_a_forged_or_wrong_round_signature_never_opens_anything() {
    let mut forged = sig_of(rho());
    forged[47] ^= 1;
    for bad in [
        sig_of(rho() + 1),
        sig_of(rho() - 1),
        forged,
        vec![0u8; 48],
        vec![],
    ] {
        let mut h = Harness::new(
            vec![Act::Honest; 5],
            Some(FixtureBeacon::of([(rho(), bad)])),
        );
        let e = h.go().expect_err("refused: waits");
        assert!(
            e.contains("not the pinned chain's") && e.contains("waits"),
            "{e}"
        );
        let mut kept = store();
        kept.load_book(&h.dir.join("book.json")).expect("loads");
        assert!(kept
            .kept_round(&feed())
            .expect("kept")
            .round
            .sealed_snapshot()
            .is_none());
    }
}

/// The round book keeps a time-locked snapshot (book version 8, ROADMAP C52):
/// the lock round, the far round, its signature and the openings, re-checked
/// when read back; a book whose opening, signature or far round was tampered
/// with is refused, and so is the same round read as version 7 (a C51
/// time-locked round has no far copy).
#[test]
fn c51_the_round_book_keeps_the_time_lock_read() {
    let mut h = Harness::new(
        vec![
            Act::Garbage,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Honest,
        ],
        Some(fixture()),
    );
    // The results store cannot be written: the settled round stays in the book.
    let book = h.dir.join("book.json");
    let mut s: ObservationResults = store();
    let mut chain = Shared(h.reg.clone(), h.beacon.clone());
    let rk = reveal_key();
    let res = run_and_save_with(
        &recorded().record_date,
        RECORDED_WINDOW_START,
        Observers::Independent(&pool()),
        &mut s,
        &mut chain,
        &book,
        None,
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut h.inbox,
        &ReceiptKey::from_file(&receipt_file(&h.dir)).expect("receipt key"),
        Some(&rk),
        &mut |_| Err("the results store is not writable".into()),
        &mut |_| Ok(()),
    );
    assert!(res.is_err());
    let b = h.book();
    assert_eq!(b["version"], 8);
    let mut fresh = store();
    fresh.absorb_non_value_book(&b).expect("the book loads");
    let kept = fresh.kept_round(&feed()).expect("kept");
    let snap = kept.round.sealed_snapshot().expect("snapshot");
    let t = snap.timelock.as_ref().expect("its time-lock read");
    assert_eq!(t.lock_round, rho());
    assert_eq!(t.far_round, rounds_of(config(&feed()).reveal_close).far());
    let mut v7 = b.clone();
    v7["version"] = Value::from(7);
    let e = store()
        .absorb_non_value_book(&v7)
        .expect_err("a C51 time-locked round");
    assert!(format!("{e}").contains("far-date copy"), "{e}");
    assert_eq!(t.openings.len(), 1);
    let saved = b["unfinished"][0].as_str().expect("hex").to_owned();
    let bytes = hex_decode(&saved[2..]);
    for (needle, off) in [
        (t.openings[0].1.secret.to_vec(), 5),
        (t.signature.clone(), 5),
        // The far round (8 bytes LE in Borsh): one round later is refused by
        // the store, which derives it from the template.
        (t.far_round.to_le_bytes().to_vec(), 0),
    ] {
        let at = bytes
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .expect("in the saved round");
        let mut bad = bytes.clone();
        bad[at + off] ^= 1;
        let mut tampered = b.clone();
        tampered["unfinished"][0] = Value::from(format!("0x{}", hex::encode(bad)));
        let e = store()
            .absorb_non_value_book(&tampered)
            .expect_err("tampered");
        if off == 0 {
            // Review C52-R1-5: refused for the far round itself.
            assert!(
                format!("{e:?}").contains("not the registered template's"),
                "{e:?}"
            );
        }
    }
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// THREATS C11-R2-1 class: with time-locks required, the registry announces no
/// draw without a reveal key, so the runner cannot fall back to spool reveals
/// (or amendment 10 posts) by running without one.
#[test]
fn c51_a_coordinator_without_a_reveal_key_cannot_draw_once_time_locks_are_required() {
    let mut reg = SimulatedRegistry::new();
    reg.receipt_signer = key_address(&receipt()).expect("key");
    reg.require_time_lock();
    let fact = store().onchain_fact(&feed()).expect("fact");
    let d = OnchainDraw {
        fact,
        attempt: 0,
        commit: keccak256(b"commit"),
        beacon_height: 5,
    };
    assert_eq!(
        reg.announce(&d),
        Err("announceDraw: NoRevealKey".to_string())
    );
    reg.reveal_key = Some(reveal_key().point());
    reg.announce(&d).expect("keyed: announced");
    assert!(reg.draw_time_locked(&fact, 0).expect("read"));
}

/// A chain view that forwards `draw_reveal_key` but not `drawTimeLocked`.
struct KeyOnly(Rc<RefCell<SimulatedRegistry>>, Option<FixtureBeacon>);

impl onyx_coordinator::observe::ChainView for KeyOnly {
    fwd! {
        height(&self) -> Result<u64, String>;
        block_hash(&self, h: u64) -> Result<Option<Hash32>, String>;
    }
}

impl DrawChain for KeyOnly {
    fwd! {
        deployment(&self) -> Result<Option<RegistryDeployment>, String>;
        draw_count(&self, f: &OnchainFact) -> Result<u32, String>;
        draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String>;
        draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String>;
        draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String>;
        log_block(&self) -> Result<Option<LogBlock>, String>;
        posted_commit(&self, b: u64, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32) -> Result<Option<PostedCommit>, String>;
        transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String>;
        draw_receipt_signer(&self, f: &OnchainFact, a: u32) -> Result<Option<[u8; 20]>, String>;
        draw_reveal_key(&self, f: &OnchainFact, a: u32) -> Result<Option<Point>, String>;
        sealed_post(&self, b: u64, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32) -> Result<Option<PostedSealed>, String>;
        reveal_log_complete(&self, q: &RevealLogQuery) -> Result<bool, String>;
        records_rounds(&self) -> bool;
    }
    fwd_mut! {
        announce(&mut self, d: &OnchainDraw) -> Result<(), String>;
        fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String>;
        post_commit(&mut self, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32, c: &Hash32, s: &ObserverSig) -> Result<Posted, String>;
        post_receipt(&mut self, f: &OnchainFact, a: u32, o: &MinerAddress, r: &Hash32, c: &Hash32, s: &ObserverSig) -> Result<Posted, String>;
        record_attempt(&mut self, q: &RevealLogQuery, r: &Hash32) -> Result<Recorded, String>;
        record_observation(&mut self, rec: &OnchainObservationCheck) -> Result<Recorded, String>;
    }
    fn beacon(&self) -> Option<&dyn onyx_coordinator::timelock::BeaconSource> {
        self.1
            .as_ref()
            .map(|b| b as &dyn onyx_coordinator::timelock::BeaconSource)
    }
}

/// Review C51-R1-3: a chain view that answers `drawRevealKey` but not
/// `drawTimeLocked` stops the run before any post is opened or any round is
/// recorded (by default it would have opened time-locked posts amendment 10
/// style, dismissed every honest one and recorded that round for good).
#[test]
fn c51_r1_3_a_view_that_cannot_tell_time_locked_draws_stops_the_run() {
    let mut h = Harness::new(vec![Act::Honest; 5], Some(fixture()));
    let book = h.dir.join("book.json");
    let mut s: ObservationResults = store();
    let mut chain = KeyOnly(h.reg.clone(), h.beacon.clone());
    let rk = reveal_key();
    let Err(e) = run_and_save_with(
        &recorded().record_date,
        RECORDED_WINDOW_START,
        Observers::Independent(&pool()),
        &mut s,
        &mut chain,
        &book,
        None,
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut h.inbox,
        &ReceiptKey::from_file(&receipt_file(&h.dir)).expect("receipt key"),
        Some(&rk),
        &mut |_| Ok(()),
        &mut |_| Ok(()),
    ) else {
        panic!("refused");
    };
    assert!(e.error.contains("drawTimeLocked"), "{}", e.error);
    let reg = h.reg.borrow();
    let fact = store().onchain_fact(&feed()).expect("fact");
    assert_eq!(reg.sealed.attempt_record(&fact.registry_key(), 0), None);
    assert!(reg.observations.is_empty());
}

/// Review C51-R2-1: the registry's length rule cannot tell a time-locked post
/// from any other ciphertext over 128 bytes (a plain amendment 10 post of this
/// feed is 154 bytes and lands in a time-locked draw: see the dismissals test
/// above). A coordinator that took such a post as a REVEAL (it reads a plain
/// post at once) would serve a round no dismissal check touches; the client
/// now requires every revealed post's time-lock to open canonically under the
/// verified lock-round signature, so it is refused off chain.
#[test]
fn c51_r2_1_a_revealed_post_whose_time_lock_does_not_open_is_refused() {
    let mut h = Harness::new(vec![Act::Honest; 5], Some(fixture()));
    h.go().expect("the round runs");
    let f = served(h.written.borrow().as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let body = f.body(rec);
    let tpl = template();
    let sp = feed();
    onyx_api::client::verify_observation(&body, &expect(&tpl, &sp.subject))
        .expect("the honest round checks");
    let ct = body["sealed"]["posts"][0]["ciphertext"]
        .as_str()
        .expect("ciphertext")
        .to_owned();
    let len = (ct.len() - 2) / 2;
    // Junk of the same length (no time-lock at all), and the honest post with
    // its time-lock zeroed (the body kept).
    let junk = format!("0x{}", "09".repeat(len));
    let zeroed = format!("0x{}{}", "00".repeat(TLOCK_LEN), &ct[2 + 2 * TLOCK_LEN..]);
    for bad in [junk, zeroed] {
        let mut b = body.clone();
        b["sealed"]["posts"][0]["ciphertext"] = Value::from(bad);
        let e = onyx_api::client::verify_observation(&b, &expect(&tpl, &sp.subject))
            .expect_err("a revealed post without a time-lock that opens");
        assert!(
            format!("{e:?}").contains("time-lock does not open"),
            "{e:?}"
        );
    }
}

/// ROADMAP C52 end to end (THREATS C52-3): four observers post honest
/// time-locked reveals with their far copies, one posts its valid reveal
/// behind a far copy of another key. The live runner dismisses that post at
/// intake (with its opening: the near time-lock opens) and settles the value
/// from the other four; the served round names the far round, the one every
/// client derives from its own template (the record date's last observable
/// day); a server naming another far round, or none, is refused; the
/// dismissal stands although the dismissed body is a valid reveal.
#[test]
fn c52_a_live_round_dismisses_a_bad_far_copy_and_serves_the_templates_far_round() {
    let acts = vec![
        Act::BadFar,
        Act::Honest,
        Act::Honest,
        Act::Honest,
        Act::Honest,
    ];
    let mut h = Harness::new(acts, Some(fixture()));
    let r = h.go().expect("the round runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert_eq!(r.reveals.len(), 4);
    assert_eq!(r.sealed.dismissals.len(), 1);
    let far = rounds_of(config(&feed()).reveal_close).far();
    let t = r.sealed.time_lock.as_ref().expect("time-locked");
    assert_eq!(t.far_round, far);
    assert_eq!(t.openings.len(), 1, "the near time-lock opened");
    // The far round is a day past the record date's last observable day's
    // latest reveal close (2026-10-18 + window, grace, reveal phase).
    let q = BeaconChain::quicknet();
    let latest = onyx_coordinator::timelock::latest_reveal_close(&template(), &feed())
        .expect("latest close");
    assert!(q.round_time(far).expect("t") >= latest + 86_400);
    let f = served(h.written.borrow().as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let body = f.body(rec);
    assert_eq!(body["sealed"]["timeLock"]["farRound"], far);
    let tpl = template();
    let sp = feed();
    let v = onyx_api::client::verify_observation(&body, &expect(&tpl, &sp.subject))
        .expect("the client checks it, the far-copy dismissal included");
    let reg = h.reg.borrow();
    assert_eq!(
        verify_sealed_onchain(
            &v.checked.reveal_log,
            &chain_returns(&reg, &v.checked.reveal_log)
        ),
        Ok(())
    );
    for bad in [Value::from(far + 1), Value::from(far - 1), Value::Null] {
        let mut lie = body.clone();
        if bad.is_null() {
            lie["sealed"]["timeLock"]
                .as_object_mut()
                .expect("time lock")
                .remove("farRound");
        } else {
            lie["sealed"]["timeLock"]["farRound"] = bad;
        }
        assert!(
            onyx_api::client::verify_observation(&lie, &expect(&tpl, &sp.subject)).is_err(),
            "another far round, or none, is refused"
        );
    }
    // Review C52-R2-3: every post cut to 272 bytes (a time-lock and a far
    // copy, no body) is refused by the client's own length rule: each
    // revealed post's time-lock still opens and the dismissal still checks, so
    // nothing else would.
    let mut lie = body.clone();
    for p in lie["sealed"]["posts"].as_array_mut().expect("posts") {
        let ct = p["ciphertext"].as_str().expect("ct").to_string();
        p["ciphertext"] = Value::from(ct[..2 + 2 * 272].to_string());
    }
    let e = onyx_api::client::verify_observation(&lie, &expect(&tpl, &sp.subject))
        .expect_err("posts without a body are refused");
    assert!(
        format!("{e:?}").contains("never takes"),
        "the length rule refuses them: {e:?}"
    );
}
