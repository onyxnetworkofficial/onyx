//! ROADMAP C11: sealed reveals wired end to end (ADR-0027 amendment 10, item
//! 12; amendment 11).
//!
//! Independent observers (their own keys, as in ROADMAP B2) post their signed
//! reveals ON CHAIN encrypted to the draw's reveal key (`postSealedReveal`);
//! the live runner (`run_and_save_with` with the coordinator's reveal key)
//! reads every drawn slot from one finalized block past `reveal_close`, opens
//! or dismisses each post, checks `revealLogComplete`, records a round without
//! a value with `recordAttempt`, and writes the round with its posts,
//! dismissals and record to the results store; the Rust client then checks
//! the served answer off chain (dismissals) and on chain (`drawRevealKey`,
//! `revealLogComplete`, every served post the chain's, `attemptMatches`), all
//! with its own drawn set, window and round id (C4-R1-2).
//!
//! Adversarial (THREATS C11-*): a coordinator reading before close, skipping a
//! slot (or a node hiding one), replaying a dismissal, a node serving a post
//! whose key proof fails, a run with another reveal key; a server skipping or
//! faking what a client must check (the sealed section, a post, the record, the
//! window); a client skipping a check is impossible through the API (the
//! sealed returns are an argument of every on-chain verifier).

use onyx_api::client::{
    honest_sealed_returns, sealed_calls, verify_attempts, verify_observation,
    verify_observation_onchain, verify_sealed_onchain, ObservationExpect, SealedRets, VerifyError,
};
use onyx_api::observations::{
    check, check_attempt, ObservationFeed, ObservationStore, SealedChecked, SealedLog, TemplatePins,
};
use onyx_coordinator::keyfile::{key_address, ReceiptKey, RevealKey};
use onyx_coordinator::observe::{
    test_keys, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObservationOutcome,
    ObservationResults, Observer, ObserverSig, ObserverStatus, OnchainDraw, OnchainFact,
    OnchainObservationCheck, Posted, PostedCommit, Recorded, RegistryDeployment, ReplayClock,
    RevealInbox, SignedReveal, SimulatedRegistry, TranscriptLogQuery,
};
use onyx_coordinator::sealed::{
    dismiss, posted_returns, public_point, seal, seal_bytes, sealed_len, Dismissal, Point,
    PostedSealed, RevealLogQuery, SealedPost,
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

/// Hex without a dependency of the adapter's.
mod hex {
    pub fn encode(b: impl AsRef<[u8]>) -> String {
        b.as_ref().iter().map(|x| format!("{x:02x}")).collect()
    }
    pub fn decode(s: &str) -> Result<Vec<u8>, std::num::ParseIntError> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect()
    }
}

/// Total public debt outstanding on 2026-10-08, in cents, as recorded.
const RECORDED_CENTS: i128 = 4_030_531_621_082_972;

/// What a test's chain view does wrong (an honest view by default).
#[derive(Clone, Copy, Default)]
struct Faults {
    /// The finalized block's time never passes this (a node behind).
    final_cap: Option<u64>,
    /// This observer's slot reads empty (a lagging node, or a coordinator
    /// that skips the slot).
    hide: Option<MinerAddress>,
    /// This observer's post comes back with a key proof that fails (a node
    /// that lies, C4-R3-2).
    bad_proof: Option<MinerAddress>,
}

/// One registry both the coordinator and the observers use, read through a
/// view that may lie.
#[derive(Clone)]
struct Chain {
    reg: Rc<RefCell<SimulatedRegistry>>,
    faults: Faults,
}

impl ChainView for Chain {
    fn height(&self) -> Result<u64, String> {
        self.reg.borrow().height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.reg.borrow().block_hash(h)
    }
}

impl DrawChain for Chain {
    fn deployment(&self) -> Result<Option<RegistryDeployment>, String> {
        self.reg.borrow().deployment()
    }
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.reg.borrow().draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.reg.borrow().draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.reg.borrow_mut().announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.reg.borrow_mut().fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
        self.reg.borrow().draw_of(f, a)
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.reg.borrow().draw_of_final(f, a)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
        c: &Hash32,
        s: &ObserverSig,
    ) -> Result<Posted, String> {
        self.reg.borrow_mut().post_commit(f, a, o, r, c, s)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
        c: &Hash32,
        s: &ObserverSig,
    ) -> Result<Posted, String> {
        self.reg.borrow_mut().post_receipt(f, a, o, r, c, s)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        let b = self.reg.borrow().log_block()?;
        Ok(b.map(|b| LogBlock {
            number: b.number,
            time: self.faults.final_cap.map_or(b.time, |t| b.time.min(t)),
        }))
    }
    fn posted_commit(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.reg.borrow().posted_commit(b, f, a, o, r)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.reg.borrow().transcript_complete(q)
    }
    fn draw_receipt_signer(&self, f: &OnchainFact, a: u32) -> Result<Option<[u8; 20]>, String> {
        self.reg.borrow().draw_receipt_signer(f, a)
    }
    fn draw_reveal_key(&self, f: &OnchainFact, a: u32) -> Result<Option<Point>, String> {
        self.reg.borrow().draw_reveal_key(f, a)
    }
    fn draw_time_locked(&self, f: &OnchainFact, a: u32) -> Result<bool, String> {
        self.reg.borrow().draw_time_locked(f, a)
    }
    fn sealed_post(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedSealed>, String> {
        if self.faults.hide == Some(*o) {
            return Ok(None);
        }
        let mut p = self.reg.borrow().sealed_post(b, f, a, o, r)?;
        if self.faults.bad_proof == Some(*o) {
            if let Some(p) = p.as_mut() {
                p.key_proof[51] ^= 1;
            }
        }
        Ok(p)
    }
    fn reveal_log_complete(&self, q: &RevealLogQuery) -> Result<bool, String> {
        self.reg.borrow().reveal_log_complete(q)
    }
    fn records_rounds(&self) -> bool {
        self.reg.borrow().records_rounds()
    }
    fn record_attempt(&mut self, q: &RevealLogQuery, r: &Hash32) -> Result<Recorded, String> {
        self.reg.borrow_mut().record_attempt(q, r)
    }
    fn record_observation(&mut self, rec: &OnchainObservationCheck) -> Result<Recorded, String> {
        self.reg.borrow_mut().record_observation(rec)
    }
}

/// Seven pool members, each with its own key.
fn keys() -> Vec<[u8; 32]> {
    (0..7)
        .map(|i| test_keys::secret("c11 observer", i))
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

fn feed() -> ObservationSpec {
    spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec")
}

fn reveal_key() -> RevealKey {
    RevealKey::from_secret(test_keys::secret("c11 reveal", 0)).expect("reveal key")
}

/// What one drawn observer process does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    /// Posts its sealed reveal once the reveal phase opens.
    Honest,
    /// Posts junk under its own key (a griefer, C4-4).
    Garbage,
    /// Posts a signed reveal of another value than it committed.
    Lie,
    /// Commits, then never posts.
    Withhold,
    /// Posts during the commit phase (ignored, C4-5).
    Early,
    /// Posts after `reveal_close` (ignored).
    Late,
}

/// The observer processes, run from the inbox's reads (the coordinator's
/// clock): at the first read each drawn observer commits on chain inside the
/// commit phase, then posts its sealed reveal (by its act); at the last read,
/// past `reveal_close`, a late observer posts.
struct Processes {
    reg: Rc<RefCell<SimulatedRegistry>>,
    acts: Vec<Act>,
    turns: Vec<([u8; 32], MinerAddress, SignedReveal)>,
    drawn: Vec<MinerAddress>,
    round: Hash32,
    attempt: u32,
}

impl Processes {
    fn sealed(&self, i: usize, key: &Point) -> SealedPost {
        let (sk, me, (_, o, salt, _)) = &self.turns[i];
        let sp = feed();
        let eph = keccak256(&[b'e', i as u8]).0;
        match self.acts[i] {
            Act::Garbage => seal_bytes(
                sk,
                me,
                &eph,
                key,
                &self.round,
                self.attempt,
                &vec![7u8; sealed_len(&sp)],
            ),
            Act::Lie => {
                let mut other = o.clone();
                other.answer = Answer::Number(3_030_531_621_082_972);
                seal(sk, &eph, key, &self.round, self.attempt, &sp, &other, salt)
            }
            _ => seal(sk, &eph, key, &self.round, self.attempt, &sp, o, salt),
        }
        .expect("seal")
    }
}

impl RevealInbox for Processes {
    fn take(&mut self, round: &Hash32, observers: &[MinerAddress], now: u64) -> Vec<SignedReveal> {
        let sp = feed();
        let cfg = config(&sp);
        let fact = store().onchain_fact(&sp).expect("fact");
        if self.turns.is_empty() {
            let mut reg = self.reg.borrow_mut();
            let n = reg.draw_count(&fact).expect("count");
            let rec = reg.draw_of(&fact, n - 1).expect("read").expect("a draw");
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
                self.round = id;
                self.attempt = seed.attempt;
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
                self.turns.push((k, me, reveal));
            }
            let key = reg
                .draw_reveal_key(&fact, self.attempt)
                .expect("read")
                .expect("the draw has a reveal key");
            drop(reg);
            for i in 0..self.turns.len() {
                let at = match self.acts[i] {
                    Act::Early => sp.window_start + 30,
                    Act::Withhold | Act::Late => continue,
                    _ => now,
                };
                let p = self.sealed(i, &key);
                let mut reg = self.reg.borrow_mut();
                reg.time = at;
                reg.post_sealed(&fact, self.attempt, &self.round, &p)
                    .expect("postSealedReveal");
            }
        }
        if now > cfg.reveal_close {
            let key = public_point(reveal_key().secret()).expect("point");
            for i in 0..self.turns.len() {
                if self.acts[i] == Act::Late
                    && self
                        .reg
                        .borrow()
                        .sealed
                        .sealed_post(&fact.registry_key(), self.attempt, &self.turns[i].1, round)
                        .is_none()
                {
                    let p = self.sealed(i, &key);
                    let mut reg = self.reg.borrow_mut();
                    reg.time = now;
                    reg.post_sealed(&fact, self.attempt, &self.round, &p)
                        .expect("a late post lands");
                }
            }
        }
        // The chain moves on with the coordinator's clock.
        self.reg.borrow_mut().time = now;
        vec![]
    }
}

/// A finished (or failed) live round and what it left behind.
struct Run {
    result: Result<RoundRun, String>,
    reg: Rc<RefCell<SimulatedRegistry>>,
    drawn: Vec<MinerAddress>,
    /// The results store the run wrote (none when it did not publish).
    written: Option<Value>,
    book: Value,
}

fn suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Run one live round with `acts` per drawn observer, the coordinator's
/// reveal key `key` (`None`: the configured one), on a chain read through
/// `faults`; `fail_publish` makes the results-store write fail (the book then
/// keeps the settled round).
fn run(acts: Vec<Act>, key: Option<RevealKey>, faults: Faults, fail_publish: bool) -> Run {
    let reg = Rc::new(RefCell::new(SimulatedRegistry::new()));
    let receipt = test_keys::secret("c11 receipt", 0);
    {
        let mut r = reg.borrow_mut();
        r.receipt_signer = key_address(&receipt).expect("key");
        r.reveal_key = Some(reveal_key().point());
        r.records = true;
    }
    let mut inbox = Processes {
        reg: reg.clone(),
        acts,
        turns: vec![],
        drawn: vec![],
        round: Hash32::ZERO,
        attempt: 0,
    };
    let dir = std::env::temp_dir().join(format!("onyx-c11-{}-{}", std::process::id(), suffix()));
    std::fs::create_dir_all(&dir).expect("dir");
    let book = dir.join("book.json");
    let mut s: ObservationResults = store();
    let mut chain = Chain {
        reg: reg.clone(),
        faults,
    };
    let key = key.unwrap_or_else(reveal_key);
    let written = RefCell::new(None);
    let result = run_and_save_with(
        &recorded().record_date,
        RECORDED_WINDOW_START,
        Observers::Independent(&pool()),
        &mut s,
        &mut chain,
        &book,
        None,
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut inbox,
        &ReceiptKey::from_file(&receipt_file(&dir, &receipt)).expect("receipt key"),
        Some(&key),
        &mut |r| {
            if fail_publish {
                return Err("the results store is not writable".into());
            }
            *written.borrow_mut() = Some(append_results(None, r)?);
            Ok(())
        },
        &mut |_| Ok(()),
    )
    .map_err(|e| e.error);
    let book_json: Value = std::fs::read_to_string(&book)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        result,
        reg,
        drawn: inbox.drawn,
        written: written.into_inner(),
        book: book_json,
    }
}

fn receipt_file(dir: &std::path::Path, secret: &[u8; 32]) -> std::path::PathBuf {
    let p = dir.join("receipt.key");
    std::fs::write(&p, hex::encode(secret)).expect("write");
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

/// The server's feed loaded from a results store (every round re-checked).
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

/// What the (simulated) registry answers to the client's sealed calls for
/// `c`: computed from the registry's state with the client's own query.
fn chain_returns(reg: &SimulatedRegistry, c: &SealedChecked) -> SealedRets {
    let q = &c.query;
    let word = |b: bool| {
        let mut w = vec![0u8; 32];
        w[31] = u8::from(b);
        w
    };
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
                let (r, p) = posted_returns(reg.sealed.sealed_post(
                    &q.registry_fact,
                    q.attempt,
                    a,
                    &q.round_id,
                ));
                (r, p)
            })
            .collect(),
    }
}

fn status(r: &RoundRun, a: &MinerAddress) -> ObserverStatus {
    r.settlement
        .statuses()
        .iter()
        .find(|(x, _)| x == a)
        .map(|(_, s)| *s)
        .expect("drawn")
}

/// ROADMAP C11: five drawn observers post sealed reveals on chain; the
/// coordinator, holding none of their keys and receiving nothing through the
/// spool, reads every slot from one block past reveal close, opens each post
/// into a reveal arriving at its block time, and settles the recorded value;
/// `revealLogComplete` holds, the value is recorded with `recordObservation`,
/// and the served answer (posts included, no dismissal) passes the client's
/// checks off and on chain.
#[test]
fn c11_sealed_reveals_settle_a_live_round_and_the_client_checks_them() {
    let run = run(vec![Act::Honest; 5], None, Faults::default(), false);
    let r = run.result.expect("the round runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert_eq!(r.reveals.len(), 5, "every reveal came from a sealed post");
    assert_eq!(r.sealed.posts.len(), 5);
    assert!(r.sealed.dismissals.is_empty() && r.sealed.record.is_none());
    assert_eq!(r.sealed.reveal_key, Some(reveal_key().point()));
    let fact = store().onchain_fact(&feed()).expect("fact");
    let reg = run.reg.borrow();
    for a in &run.drawn {
        assert!(
            reg.log
                .receipt(&fact.registry_key(), 0, a, &r.round_id)
                .is_none(),
            "no receipt: the post is the receipt"
        );
    }
    assert_eq!(
        reg.observations.len(),
        1,
        "recordObservation before serving"
    );
    // The served answer.
    let f = served(run.written.as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let body = f.body(rec);
    assert_eq!(body["sealed"]["posts"].as_array().map(Vec::len), Some(5));
    let t = template();
    let sp = feed();
    let v = verify_observation(&body, &expect(&t, &sp.subject)).expect("the client checks it");
    let rets = chain_returns(&reg, &v.checked.reveal_log);
    let yes = {
        let mut w = [0u8; 32];
        w[31] = 1;
        w
    };
    let mut two = [0u8; 32];
    two[31] = 2;
    let mut budget = [0u8; 64];
    budget[63] = 3;
    let complete = {
        let mut w = [0u8; 32];
        w[31] = u8::from(reg.transcript_complete(&v.checked.log).expect("answer"));
        w
    };
    assert_eq!(
        verify_observation_onchain(&v, &two, &yes, &budget, &complete, &rets),
        Ok(())
    );
    // The client's query is its own: every drawn observer, the config's
    // window, the recomputed round id.
    let q = &v.checked.reveal_log.query;
    let cfg = config(&sp);
    assert_eq!(q.round_id, r.round_id);
    assert_eq!(
        (q.commit_open, q.commit_close, q.reveal_close),
        (sp.window_start, cfg.commit_close, cfg.reveal_close)
    );
    let mut drawn = run.drawn.clone();
    drawn.sort();
    assert_eq!(
        q.entries.iter().map(|e| e.observer).collect::<Vec<_>>(),
        drawn,
        "every drawn observer, ascending"
    );
}

/// ROADMAP C11 with griefers: a post of junk and a signed reveal of another
/// value than the observer committed are dismissed with proofs, a commit-phase
/// post and a withheld one are left out; three honest posts still settle the
/// value. The dismissals are served, and the client checks each (the proof
/// fixes the decryption; the plaintext is no valid reveal).
#[test]
fn c11_garbage_is_dismissed_with_a_proof_the_client_checks() {
    let acts = vec![Act::Garbage, Act::Lie, Act::Honest, Act::Early, Act::Honest];
    let run = run(acts.clone(), None, Faults::default(), false);
    let r = run.result.expect("the round runs");
    // Two in-phase garbage posts: dismissed. The early post is out of phase
    // (C4-5): neither revealed nor dismissed, the observer unrevealed.
    assert_eq!(
        r.sealed
            .dismissals
            .iter()
            .map(|(a, _)| *a)
            .collect::<Vec<_>>(),
        vec![run.drawn[0], run.drawn[1]]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(r.reveals.len(), 2);
    for (i, a) in run.drawn.iter().enumerate() {
        let want = match acts[i] {
            Act::Honest => ObserverStatus::NoCall,
            _ => ObserverStatus::Unrevealed,
        };
        // Two honest reveals of five drawn: no quorum, NoResult.
        assert_eq!(status(&r, a), want, "{:?}", acts[i]);
    }
    assert_eq!(*r.settlement.outcome(), ObservationOutcome::NoResult);
    // A round without a value: recorded with `recordAttempt` before serving.
    let digest = r.sealed.record.expect("recorded");
    let reg = run.reg.borrow();
    let fact = store().onchain_fact(&feed()).expect("fact");
    assert_eq!(
        reg.sealed.attempt_record(&fact.registry_key(), 0),
        Some(digest)
    );
    // Served (the fact is not closed: sealed, its reveals withheld) and checked.
    let f = served(run.written.as_ref().expect("written"));
    let attempts = f.attempts_of(&f.attempt_fact(subject_text(), None).expect("fact"));
    let body = f.attempts_body(&attempts, false);
    assert!(body["attempts"][0].get("reveals").is_none(), "sealed");
    assert_eq!(
        body["attempts"][0]["sealed"]["dismissals"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("the client checks it");
    let c = &a.attempts[0].reveal_log;
    assert!(c.served_sealed);
    assert_eq!(verify_sealed_onchain(c, &chain_returns(&reg, c)), Ok(()));
}

fn subject_text() -> &'static str {
    match feed().subject {
        onyx_core::Subject::Aggregate(s) => Box::leak(s.into_boxed_str()),
        _ => unreachable!("an aggregate subject"),
    }
}

/// ROADMAP C11: the round book keeps the sealed snapshot with its dismissals
/// (book version 6): a run whose results-store write fails leaves the settled
/// round in the book, and a fresh store reads it back, re-checking every
/// dismissal; a book whose dismissal was tampered with is refused.
#[test]
fn c11_the_round_book_keeps_the_dismissals() {
    let run = run(
        vec![
            Act::Garbage,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Honest,
        ],
        None,
        Faults::default(),
        true,
    );
    assert!(run.result.is_err(), "the write failed");
    assert_eq!(run.book["version"], 8);
    let mut fresh = store();
    fresh
        .absorb_non_value_book(&run.book)
        .expect("the book loads");
    let kept = fresh
        .kept_round(&feed())
        .expect("the settled round is kept");
    let snap = kept.round.sealed_snapshot().expect("its sealed snapshot");
    assert_eq!(snap.dismissals.len(), 1);
    assert_eq!(snap.dismissals[0].0, run.drawn[0]);
    // Tamper with the dismissal's proof inside the saved round: refused.
    let mut bad = run.book.clone();
    let h = bad["unfinished"][0].as_str().expect("hex").to_owned();
    let mut b = hex::decode(&h[2..]).expect("hex");
    let d = snap.dismissals[0].1;
    let at = b
        .windows(64)
        .position(|w| w == d.proof)
        .expect("the proof is in the saved round");
    b[at + 40] ^= 1;
    bad["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b)));
    assert!(store().absorb_non_value_book(&bad).is_err());
}

/// C11-1 (coordinator reading before close): a node whose finalized block is
/// not past reveal close stops the run before any slot is read (a post could
/// still land in the window); the round stays unfinished with no sealed
/// snapshot, and nothing is recorded.
#[test]
fn c11_1_the_coordinator_never_reads_posts_before_reveal_close() {
    let rc = config(&feed()).reveal_close;
    let run = run(
        vec![Act::Honest; 5],
        None,
        Faults {
            final_cap: Some(rc),
            ..Faults::default()
        },
        false,
    );
    let e = run.result.expect_err("stops");
    assert!(e.contains("not past reveal close"), "{e}");
    let mut fresh = store();
    fresh.absorb_non_value_book(&run.book).expect("book");
    let kept = fresh.kept_round(&feed()).expect("kept unfinished");
    assert!(kept.round.sealed_snapshot().is_none());
    assert!(run.written.is_none());
}

/// C11-2 (skipping a slot): a node that hides one drawn observer's post at the
/// read block (or a coordinator that leaves the slot unread) is caught before
/// anything is kept: the chain's `revealLogComplete` refuses the round without
/// that post. Nothing is opened for good, nothing recorded or written.
#[test]
fn c11_2_a_skipped_slot_is_caught_before_anything_is_kept() {
    let hidden = {
        // The drawn set is the draw's: take a drawn observer of a clean run.
        let clean = run(vec![Act::Honest; 5], None, Faults::default(), false);
        clean.drawn[2]
    };
    let run = run(
        vec![Act::Honest; 5],
        None,
        Faults {
            hide: Some(hidden),
            ..Faults::default()
        },
        false,
    );
    let e = run.result.expect_err("refused");
    assert!(e.contains("revealLogComplete"), "{e}");
    assert!(run.written.is_none());
    let mut fresh = store();
    fresh.absorb_non_value_book(&run.book).expect("book");
    assert!(fresh
        .kept_round(&feed())
        .expect("kept")
        .round
        .sealed_snapshot()
        .is_none());
}

/// C4-R3-2 wired (a node that lies about a post): a post read back with a key
/// proof that fails is never opened or dismissed (its dismissal would publish
/// `S = k·R` for an `R` its poster may not hold); the run stops, nothing kept.
#[test]
fn c11_a_post_whose_key_proof_fails_is_never_opened() {
    let target = run(vec![Act::Honest; 5], None, Faults::default(), false).drawn[0];
    let run = run(
        vec![
            Act::Garbage,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Honest,
        ],
        None,
        Faults {
            bad_proof: Some(target),
            ..Faults::default()
        },
        false,
    );
    let e = run.result.expect_err("refused");
    assert!(e.contains("key proof"), "{e}");
    assert!(run.written.is_none());
}

/// ROADMAP C11: a run holding another reveal key than the draw snapshotted
/// (rotated since the draw was announced) stops before reading any post.
#[test]
fn c11_a_run_with_another_reveal_key_stops_before_reading() {
    let other = RevealKey::from_secret(test_keys::secret("c11 reveal", 1)).expect("key");
    let run = run(vec![Act::Honest; 5], Some(other), Faults::default(), false);
    // The registry snapshots its configured key at announce (the test's
    // registry is set up with the configured one), so the run's differs.
    let e = run.result.expect_err("refused");
    assert!(e.contains("snapshotted the reveal key"), "{e}");
}

/// A late post (after reveal close) is ignored: not a veto, the observer is
/// unrevealed, and the round still settles and is complete on chain.
#[test]
fn c11_a_late_post_is_ignored() {
    let run = run(
        vec![
            Act::Late,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Withhold,
        ],
        None,
        Faults::default(),
        false,
    );
    let r = run.result.expect("the round runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert_eq!(status(&r, &run.drawn[0]), ObserverStatus::Unrevealed);
    assert_eq!(r.sealed.posts.len(), 3, "only in-phase posts are served");
}

// ------------------------------------------------- what a server may not do

/// The served sealed round of [`c11_garbage_is_dismissed_with_a_proof_the_client_checks`]
/// (an attempt with two dismissals and a record), with its registry.
fn sealed_attempt() -> (Value, Rc<RefCell<SimulatedRegistry>>, Vec<MinerAddress>) {
    let run = run(
        vec![Act::Garbage, Act::Lie, Act::Honest, Act::Early, Act::Honest],
        None,
        Faults::default(),
        false,
    );
    let f = served(run.written.as_ref().expect("written"));
    let attempts = f.attempts_of(&f.attempt_fact(subject_text(), None).expect("fact"));
    (f.attempts_body(&attempts, false), run.reg, run.drawn)
}

fn client(body: &Value, reg: &SimulatedRegistry) -> Result<(), VerifyError> {
    let t = template();
    let sp = feed();
    let a = verify_attempts(body, &expect(&t, &sp.subject))?;
    for c in &a.attempts {
        verify_sealed_onchain(&c.reveal_log, &chain_returns(reg, &c.reveal_log))?;
    }
    Ok(())
}

/// C11-3 (replaying a dismissal): a dismissal moved to another observer's
/// garbage post, or carried into another round or attempt, does not check (the
/// DLEQ challenge binds key, `R`, `S`, round, attempt and observer); a
/// dismissal of a VALID post is false (its plaintext opens the commit).
#[test]
fn c11_3_a_replayed_or_false_dismissal_is_refused() {
    let (body, reg, _) = sealed_attempt();
    assert_eq!(client(&body, &reg.borrow()), Ok(()));
    let d = |b: &Value, i: usize| b["attempts"][0]["sealed"]["dismissals"][i].clone();
    // Swap the two dismissals' proofs (same round, other observer).
    let mut swapped = body.clone();
    let (d0, d1) = (d(&body, 0), d(&body, 1));
    swapped["attempts"][0]["sealed"]["dismissals"][0]["shared"] = d1["shared"].clone();
    swapped["attempts"][0]["sealed"]["dismissals"][0]["proof"] = d1["proof"].clone();
    swapped["attempts"][0]["sealed"]["dismissals"][1]["shared"] = d0["shared"].clone();
    swapped["attempts"][0]["sealed"]["dismissals"][1]["proof"] = d0["proof"].clone();
    assert!(matches!(
        client(&swapped, &reg.borrow()),
        Err(VerifyError::Observation(e)) if e.contains("dismissal")
    ));
    // The same post's dismissal made for another round or another attempt
    // (the honest `S`, a real proof, bound to the other context): refused.
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("ok");
    let c = &a.attempts[0];
    let garbage = c
        .reveal_log
        .query
        .entries
        .iter()
        .find(|e| e.status == onyx_coordinator::sealed::EntryStatus::Dismissed)
        .expect("a dismissed observer")
        .observer;
    let gpost = c
        .reveal_log
        .posts
        .iter()
        .find(|(o, _)| *o == garbage)
        .map(|(_, p)| SealedPost::from_posted(&garbage, p))
        .expect("its post");
    let at = body["attempts"][0]["sealed"]["dismissals"]
        .as_array()
        .expect("list")
        .iter()
        .position(|x| x["observer"].as_str() == Some(&format!("0x{}", hex::encode(garbage))))
        .expect("its dismissal");
    for (round, attempt) in [
        (keccak256(b"another round"), c.draw.attempt),
        (c.round_id, c.draw.attempt + 1),
    ] {
        let replayed = dismiss(reveal_key().secret(), &gpost, &round, attempt).expect("proof");
        assert_eq!(
            replayed.shared,
            dismiss(reveal_key().secret(), &gpost, &c.round_id, c.draw.attempt)
                .expect("proof")
                .shared,
            "the same S: only the proof's context differs"
        );
        let mut moved = body.clone();
        moved["attempts"][0]["sealed"]["dismissals"][at]["proof"] =
            serde_json::json!(format!("0x{}", hex::encode(replayed.proof)));
        let e = client(&moved, &reg.borrow()).expect_err("a replayed proof");
        assert!(format!("{e}").contains("dismissal"), "{e}");
    }
    // A false dismissal: the coordinator's own honest proof for a VALID post.
    let honest = c
        .reveal_log
        .query
        .entries
        .iter()
        .find(|e| e.status == onyx_coordinator::sealed::EntryStatus::Revealed)
        .expect("a revealed observer")
        .observer;
    let post = c
        .reveal_log
        .posts
        .iter()
        .find(|(o, _)| *o == honest)
        .map(|(_, p)| p.clone())
        .expect("its post");
    let fake: Dismissal = dismiss(
        reveal_key().secret(),
        &SealedPost::from_posted(&honest, &post),
        &c.round_id,
        c.draw.attempt,
    )
    .expect("a real proof");
    let mut falsely = body.clone();
    let list = falsely["attempts"][0]["sealed"]["dismissals"]
        .as_array_mut()
        .expect("list");
    list.push(serde_json::json!({
        "observer": format!("0x{}", hex::encode(honest)),
        "shared": format!("0x{}", hex::encode(fake.shared)),
        "proof": format!("0x{}", hex::encode(fake.proof)),
    }));
    list.sort_by_key(|x| x["observer"].as_str().unwrap_or_default().to_owned());
    let rv = falsely["attempts"][0]["revealed"]
        .as_array_mut()
        .expect("revealed");
    rv.retain(|x| x.as_str() != Some(&format!("0x{}", hex::encode(honest))));
    let e = client(&falsely, &reg.borrow()).expect_err("a false dismissal");
    assert!(format!("{e}").contains("dismissal"), "{e}");
}

/// C11-4 (a server faking a post): it serves, under an observer's name, a
/// garbage post of its own (its own ephemeral key, a valid key proof, an
/// honest dismissal of it), dropping the observer's valid post. Off chain
/// everything checks; the client refuses it on chain (`sealedRevealOf` is not
/// the served post). Without that check the drop would pass.
#[test]
fn c11_4_a_post_the_chain_does_not_hold_is_refused() {
    let (body, reg, _) = sealed_attempt();
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("ok");
    let c = &a.attempts[0];
    let victim = c
        .reveal_log
        .query
        .entries
        .iter()
        .find(|e| e.status == onyx_coordinator::sealed::EntryStatus::Revealed)
        .expect("a revealed observer")
        .observer;
    // The server's own post under the victim's name: anyone can make the key
    // proof (it needs the ephemeral secret only); the observer signature is
    // not served (the registry keeps none), so nothing off chain tells.
    let eph = keccak256(b"server ephemeral").0;
    let junk = seal_bytes(
        &test_keys::secret("server", 0),
        &victim,
        &eph,
        &reveal_key().point(),
        &c.round_id,
        c.draw.attempt,
        &vec![1u8; sealed_len(&sp)],
    )
    .expect("seal");
    let d = dismiss(reveal_key().secret(), &junk, &c.round_id, c.draw.attempt).expect("proof");
    let mut fake = body.clone();
    let v = format!("0x{}", hex::encode(victim));
    for p in fake["attempts"][0]["sealed"]["posts"]
        .as_array_mut()
        .expect("posts")
    {
        if p["observer"].as_str() == Some(&v) {
            p["ephemeral"] = serde_json::json!(format!("0x{}", hex::encode(junk.ephemeral)));
            p["keyProof"] = serde_json::json!(format!("0x{}", hex::encode(junk.key_proof)));
            p["ciphertext"] = serde_json::json!(format!("0x{}", hex::encode(&junk.ciphertext)));
        }
    }
    let list = fake["attempts"][0]["sealed"]["dismissals"]
        .as_array_mut()
        .expect("list");
    list.push(serde_json::json!({
        "observer": v,
        "shared": format!("0x{}", hex::encode(d.shared)),
        "proof": format!("0x{}", hex::encode(d.proof)),
    }));
    list.sort_by_key(|x| x["observer"].as_str().unwrap_or_default().to_owned());
    fake["attempts"][0]["revealed"]
        .as_array_mut()
        .expect("revealed")
        .retain(|x| x.as_str() != Some(&v));
    // The record no longer matches the served round either; drop it so only
    // the post check stands (a server would serve no record).
    fake["attempts"][0]["sealed"]["record"] = Value::Null;
    let fa = verify_attempts(&fake, &expect(&t, &sp.subject)).expect("off chain it checks");
    let fc = &fa.attempts[0].reveal_log;
    let rets = chain_returns(&reg.borrow(), fc);
    assert_eq!(
        verify_sealed_onchain(fc, &rets),
        Err(VerifyError::ChainMismatch(
            "sealedRevealOf: a served sealed post is not the chain's"
        ))
    );
    // A chain that echoed the served post would have let it through (the
    // check is what catches it), and the record would still refuse it.
    let echo = honest_sealed_returns(fc, Some(reveal_key().point()), true, false);
    assert!(matches!(
        verify_sealed_onchain(fc, &echo),
        Err(VerifyError::ChainMismatch(e)) if e.starts_with("attemptMatches")
    ));
}

/// C11-5 (a server leaving the sealed section out, or a key mismatch): a
/// round of a draw with a reveal key served without its posts is refused (the
/// chain's `drawRevealKey` is not the served one); posts served for a key the
/// draw does not have are refused too.
#[test]
fn c11_5_a_sealed_draw_served_without_its_posts_is_refused() {
    let (body, reg, _) = sealed_attempt();
    let mut bare = body.clone();
    bare["attempts"][0]
        .as_object_mut()
        .expect("attempt")
        .remove("sealed");
    // Off chain the dismissed observers' posts are gone too: the round is a
    // plain sealed attempt (two observers listed revealed, none dismissed).
    let t = template();
    let sp = feed();
    let a = verify_attempts(&bare, &expect(&t, &sp.subject)).expect("off chain it checks");
    let c = &a.attempts[0].reveal_log;
    assert_eq!(
        verify_sealed_onchain(c, &chain_returns(&reg.borrow(), c)),
        Err(VerifyError::ChainMismatch(
            "drawRevealKey: the round's reveal key is not the draw's (a sealed draw served without its posts, or posts for a draw without a key)"
        ))
    );
    // Another key served: refused off chain already (the posts and
    // dismissals do not check under it) or on chain.
    let mut other = body.clone();
    other["attempts"][0]["sealed"]["revealKey"] = serde_json::json!(format!(
        "0x{}",
        hex::encode(public_point(&test_keys::secret("c11 reveal", 9)).expect("point"))
    ));
    assert!(client(&other, &reg.borrow()).is_err());
}

/// C11-6 (REVLOG-5, a record for every sealed round): a round served sealed
/// must match the chain's record of exactly this round: listed observers,
/// outcome, window and round id, all the client's own. A server that lists a
/// withheld observer as revealed, changes the outcome, or a coordinator that
/// recorded another window, is refused; a served record that is not the
/// round's digest is refused off chain.
#[test]
fn c11_6_a_sealed_round_needs_its_record() {
    let (body, reg, _) = sealed_attempt();
    // Listing the early (out-of-phase, unrevealed) observer as revealed.
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("ok");
    let c = &a.attempts[0].reveal_log;
    let unrevealed = c
        .query
        .entries
        .iter()
        .find(|e| e.status == onyx_coordinator::sealed::EntryStatus::NotRevealed)
        .expect("an unrevealed observer")
        .observer;
    let mut listed = body.clone();
    let rv = listed["attempts"][0]["revealed"]
        .as_array_mut()
        .expect("revealed");
    rv.push(serde_json::json!(format!("0x{}", hex::encode(unrevealed))));
    rv.sort_by_key(|x| x.as_str().unwrap_or_default().to_owned());
    listed["attempts"][0]["sealed"]["record"] = Value::Null;
    // Review C11-R1-3: refused off chain already, a keyed draw being sealed
    // only (the observer has no in-phase post).
    let e = client(&listed, &reg.borrow()).expect_err("listed without a post");
    assert!(format!("{e}").contains("sealed only"), "{e}");
    // Another outcome (SourceUnavailable for NoResult): only the record tells.
    let mut outcome = body.clone();
    outcome["attempts"][0]["outcome"] = serde_json::json!("sourceUnavailable");
    outcome["attempts"][0]["sealed"]["record"] = Value::Null;
    outcome["attempts"][0]["resultHash"] =
        serde_json::json!(onyx_coordinator::observe::result_hash_of(
            &feed().id(),
            &a.attempts[0].round_id,
            &ObservationOutcome::SourceUnavailable,
        )
        .to_string());
    let e = client(&outcome, &reg.borrow()).expect_err("no record of that round");
    assert!(format!("{e}").contains("attemptMatches"), "{e}");
    // The served record is not the round's digest: refused off chain.
    let mut wrong = body.clone();
    wrong["attempts"][0]["sealed"]["record"] =
        serde_json::json!(keccak256(b"another record").to_string());
    assert!(matches!(
        verify_attempts(&wrong, &expect(&t, &sp.subject)),
        Err(VerifyError::Observation(e)) if e.contains("record")
    ));
    // A coordinator that records the draw with ANOTHER window: the chain
    // cannot tell (it knows no config), so such a record lands; the client's
    // query uses the config's window, so it never matches such a record, and a
    // chain answering false is refused.
    let r = reg.borrow();
    let mut shifted = c.query.clone();
    shifted.reveal_close -= 1;
    assert!(
        r.sealed.complete_with(&r.log, &shifted),
        "the chain cannot tell the window"
    );
    assert_ne!(shifted.digest(&c.result), c.query.digest(&c.result));
    let mut rets = chain_returns(&r, c);
    rets.matches = Some(vec![0u8; 32]);
    assert!(matches!(
        verify_sealed_onchain(c, &rets),
        Err(VerifyError::ChainMismatch(e)) if e.starts_with("attemptMatches")
    ));
}

/// Review C11-R2-3 (fail closed): a round without a value of a keyed draw
/// needs its record however it is served: served WITH its reveals (as a
/// closed fact's, so not "sealed"), without its record or its sealed section,
/// it is still refused when the chain holds no record of exactly that round.
#[test]
fn c11_r2_3_a_keyed_round_served_with_its_reveals_still_needs_its_record() {
    let run = run(
        vec![Act::Garbage, Act::Lie, Act::Honest, Act::Early, Act::Honest],
        None,
        Faults::default(),
        false,
    );
    let f = served(run.written.as_ref().expect("written"));
    let attempts = f.attempts_of(&f.attempt_fact(subject_text(), None).expect("fact"));
    let mut body = f.attempts_body(&attempts, true);
    body["attempts"][0]["sealed"]["record"] = Value::Null;
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("off chain it checks");
    let c = &a.attempts[0].reveal_log;
    assert!(!c.served_sealed && !c.value, "served with its reveals");
    let reg = run.reg.borrow();
    verify_sealed_onchain(c, &chain_returns(&reg, c)).expect("the chain holds its record");
    let mut rets = chain_returns(&reg, c);
    rets.matches = Some(vec![0u8; 32]);
    assert!(matches!(
        verify_sealed_onchain(c, &rets),
        Err(VerifyError::ChainMismatch(e)) if e.starts_with("attemptMatches")
    ));
    // Without its `attemptMatches` return at all: malformed, never a pass.
    rets.matches = None;
    assert!(matches!(
        verify_sealed_onchain(c, &rets),
        Err(VerifyError::Malformed(_))
    ));
}

/// The client's on-chain step cannot be skipped through the API: every
/// on-chain verifier takes the sealed returns, shaped by `sealed_calls`; a
/// missing or extra return is malformed, never a pass.
#[test]
fn c11_the_sealed_returns_are_not_optional() {
    let (body, reg, _) = sealed_attempt();
    let t = template();
    let sp = feed();
    let a = verify_attempts(&body, &expect(&t, &sp.subject)).expect("ok");
    let c = &a.attempts[0].reveal_log;
    let calls = sealed_calls(c);
    assert_eq!(calls.posts.len(), c.posts.len());
    assert!(calls.matches.is_some(), "a round without a value");
    let mut rets = chain_returns(&reg.borrow(), c);
    rets.posts.pop();
    assert!(matches!(
        verify_sealed_onchain(c, &rets),
        Err(VerifyError::Malformed(_))
    ));
    let mut rets = chain_returns(&reg.borrow(), c);
    rets.matches = None;
    assert!(matches!(
        verify_sealed_onchain(c, &rets),
        Err(VerifyError::Malformed(_))
    ));
    let count = {
        let mut w = [0u8; 32];
        w[31] = 1;
        w
    };
    let mut budget = [0u8; 64];
    budget[31] = 1;
    budget[63] = 3;
    let draw0 = {
        let r = reg.borrow();
        let fact = store().onchain_fact(&feed()).expect("fact");
        let d = r.draw_of(&fact, 0).expect("read").expect("draw");
        let mut w = d.commit.0.to_vec();
        let word = |v: u64| {
            let mut x = [0u8; 32];
            x[24..].copy_from_slice(&v.to_be_bytes());
            x
        };
        w.extend(word(d.beacon_height));
        w.extend(word(d.announced_at));
        w.extend(d.beacon.0);
        w
    };
    let yes = {
        let mut w = vec![0u8; 32];
        w[31] = 1;
        w
    };
    assert!(onyx_api::client::verify_attempts_onchain(
        &a,
        &count,
        std::slice::from_ref(&draw0),
        &budget,
        std::slice::from_ref(&yes),
        &[],
    )
    .is_err());
    let tally = onyx_api::client::verify_attempts_onchain(
        &a,
        &count,
        &[draw0],
        &budget,
        &[yes],
        &[chain_returns(&reg.borrow(), c)],
    )
    .expect("the fact's draws and the sealed round check");
    assert_eq!(tally.published, 1);
    // The checked value round's sealed log is checked by `check` too (the
    // server's own load): a value round may carry no record.
    let _ = (check, check_attempt, SealedLog::default());
}

/// Review C11-R1-3 (ex-post inclusion): the coordinator decrypts a LATE post
/// (after reveal close), sees its value and serves its observer as revealed,
/// reveal and all, with an honest record of that round. A keyed draw is sealed
/// only: a revealed observer without an in-phase post is refused off chain.
#[test]
fn c11_r1_3_a_late_post_cannot_be_served_as_a_reveal() {
    let run = run(
        vec![
            Act::Late,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Honest,
        ],
        None,
        Faults::default(),
        false,
    );
    let r = run.result.expect("the round runs");
    let late = run.drawn[0];
    assert_eq!(status(&r, &late), ObserverStatus::Unrevealed);
    let f = served(run.written.as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let mut body = f.body(rec);
    // The late observer's reveal, as its late post decrypts (the coordinator
    // holds the reveal key): its own signed reveal opening its commit.
    let reg = run.reg.borrow();
    let fact = store().onchain_fact(&feed()).expect("fact");
    let post = reg
        .sealed
        .sealed_post(&fact.registry_key(), 0, &late, &r.round_id)
        .expect("the late post is on chain")
        .clone();
    let ctx_commit = r
        .commits
        .iter()
        .find(|(a, _, _)| *a == late)
        .map(|(_, h, _)| *h);
    let sp = feed();
    let opened = onyx_coordinator::sealed::open_posted(
        reveal_key().secret(),
        &late,
        &post,
        &onyx_coordinator::sealed::RevealContext {
            round_id: r.round_id,
            attempt: 0,
            spec: &sp,
            commit: ctx_commit,
        },
    )
    .expect("opens");
    let onyx_coordinator::sealed::Opened::Reveal((a, o, salt, sig)) = opened else {
        panic!("the late post is a valid reveal");
    };
    let rv = body["reveals"].as_array_mut().expect("reveals");
    rv.push(serde_json::json!({
        "observer": format!("0x{}", hex::encode(a)),
        "observation": format!("0x{}", hex::encode(borsh_vec(&o))),
        "salt": salt.to_string(),
        "sig": format!("0x{}", hex::encode(sig)),
    }));
    rv.sort_by_key(|x| x["observer"].as_str().unwrap_or_default().to_owned());
    let t = template();
    let e = verify_observation(&body, &expect(&t, &sp.subject)).expect_err("refused");
    let e = format!("{e}");
    // Either the reveal-time or value checks or the sealed-only rule refuse
    // it; the sealed-only rule refuses it whatever the reveal says.
    assert!(e.contains("sealed") || e.contains("value"), "{e}");
}

/// Review C11-R1-7: in a VALUE round (no record), a server that drops an
/// observer's reveal and serves, under its name, a garbage post of its own with
/// an honest dismissal is refused by the post comparison alone.
#[test]
fn c11_r1_7_a_value_round_with_a_substituted_post_is_refused_by_the_post_check() {
    let run = run(vec![Act::Honest; 5], None, Faults::default(), false);
    let r = run.result.expect("the round runs");
    let f = served(run.written.as_ref().expect("written"));
    let rec = f.records.values().next().expect("the value round");
    let body = f.body(rec);
    let mut drawn = run.drawn.clone();
    drawn.sort();
    let victim = drawn[0];
    let v = format!("0x{}", hex::encode(victim));
    let sp = feed();
    let eph = keccak256(b"server ephemeral").0;
    let junk = seal_bytes(
        &test_keys::secret("server", 0),
        &victim,
        &eph,
        &reveal_key().point(),
        &r.round_id,
        0,
        &vec![1u8; sealed_len(&sp)],
    )
    .expect("seal");
    let d = dismiss(reveal_key().secret(), &junk, &r.round_id, 0).expect("proof");
    let mut fake = body.clone();
    fake["reveals"]
        .as_array_mut()
        .expect("reveals")
        .retain(|x| x["observer"].as_str() != Some(&v));
    for p in fake["sealed"]["posts"].as_array_mut().expect("posts") {
        if p["observer"].as_str() == Some(&v) {
            p["ephemeral"] = serde_json::json!(format!("0x{}", hex::encode(junk.ephemeral)));
            p["keyProof"] = serde_json::json!(format!("0x{}", hex::encode(junk.key_proof)));
            p["ciphertext"] = serde_json::json!(format!("0x{}", hex::encode(&junk.ciphertext)));
        }
    }
    fake["sealed"]["dismissals"] = serde_json::json!([{
        "observer": v,
        "shared": format!("0x{}", hex::encode(d.shared)),
        "proof": format!("0x{}", hex::encode(d.proof)),
    }]);
    // Four equal reveals still settle the same value: the result hash and
    // every derived field are unchanged.
    let t = template();
    let fv = verify_observation(&fake, &expect(&t, &sp.subject)).expect("off chain it checks");
    let c = &fv.checked.reveal_log;
    assert!(c.value, "a value round: no record to catch it");
    assert_eq!(
        verify_sealed_onchain(c, &chain_returns(&run.reg.borrow(), c)),
        Err(VerifyError::ChainMismatch(
            "sealedRevealOf: a served sealed post is not the chain's"
        ))
    );
}

fn borsh_vec(o: &onyx_core::Observation) -> Vec<u8> {
    onyx_core::to_canonical_bytes(o).expect("borsh")
}

/// Review C51-R2 (book migration): a version-6 book (written before ROADMAP
/// C51: its sealed snapshot has no time-lock field) still loads, with the same
/// kept round and dismissals as the version-7 book it was derived from; a
/// version this code does not know (8) is refused with a clear error.
#[test]
fn c51_r2_a_version_6_book_with_a_sealed_round_still_loads() {
    let run = run(
        vec![
            Act::Garbage,
            Act::Honest,
            Act::Honest,
            Act::Honest,
            Act::Honest,
        ],
        None,
        Faults::default(),
        true,
    );
    assert_eq!(run.book["version"], 8);
    let mut fresh = store();
    fresh.absorb_non_value_book(&run.book).expect("v8 loads");
    // ROADMAP C52: a round without a time-lock has the same bytes in versions
    // 7 and 8, so the same book read as version 7 loads too.
    let mut v7 = run.book.clone();
    v7["version"] = serde_json::json!(7);
    store()
        .absorb_non_value_book(&v7)
        .expect("a version-7 book without a time-lock loads");
    let kept = fresh.kept_round(&feed()).expect("kept").clone();
    assert!(kept
        .round
        .sealed_snapshot()
        .expect("sealed")
        .timelock
        .is_none());
    // Version 7 (and 8) appends `timelock: Option<_>` (None: one 0 byte) to the
    // snapshot, which ends the round; the round is followed by the draw record
    // (116 bytes, fixed) and the reveals (empty for a sealed draw: 4 bytes).
    assert!(kept.reveals.is_empty());
    let mut v6 = run.book.clone();
    let h = v6["unfinished"][0].as_str().expect("hex").to_owned();
    let mut b = hex::decode(&h[2..]).expect("hex");
    let at = b.len() - 116 - 4 - 1;
    assert_eq!(b[at], 0, "the None of the time-lock");
    b.remove(at);
    v6["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b)));
    v6["version"] = serde_json::json!(6);
    let mut old = store();
    old.absorb_non_value_book(&v6)
        .expect("a version-6 book loads");
    let k6 = old.kept_round(&feed()).expect("kept from v6");
    assert_eq!(k6.round.sealed_snapshot(), kept.round.sealed_snapshot());
    assert_eq!(k6.onchain_draw, kept.onchain_draw);
    // The v6 bytes read as v7 or v8 do not decode; v9 is unknown.
    for v in [7, 8] {
        let mut mixed = v6.clone();
        mixed["version"] = serde_json::json!(v);
        assert!(store().absorb_non_value_book(&mixed).is_err());
    }
    let mut v9 = run.book.clone();
    v9["version"] = serde_json::json!(9);
    let e = store().absorb_non_value_book(&v9).expect_err("unknown");
    assert!(format!("{e}").contains("unknown book version"), "{e}");
}
