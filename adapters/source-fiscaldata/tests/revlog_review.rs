//! Review REVLOG (adversarial review of the transcript log, ADR-0027
//! amendment 8). HIGH-1: a drawn observer that posts an in-phase commit on
//! chain and sends the coordinator nothing (or another commit) made every
//! honest transcript of the round fail `transcriptComplete` for good (the
//! reviewer's `test_PocObserverVeto`), and the runner aborted on
//! `AlreadyPosted`. Now the runner reads the log once the chain is past the
//! commit phase, absorbs the posted commit, counts the griefer as a
//! non-reveal, and asks `transcriptComplete` before it records the round.
//!
//! Review REVLOG-R2 (the reviewer's `poc_revlog_r2.rs`): a failed chain read
//! (a capped `eth_getLogs`) or an error after receipts were posted published
//! a failed round without the posted commit or the receipted reveals, which
//! voided the fact. Now the log is read from contract state, chain errors
//! are retried, and a run that cannot finish keeps its round unfinished (to
//! finish on the same draw), never publishing it as a failed round.
//!
//! Review REVLOG-R3 (the reviewer's `r3_poc_R3.rs`, strike 3): one stale
//! read and a retry that read the log again dropped a receipted reveal for
//! good, and the binaries dropped unfinished rounds (a new draw voided the
//! fact). Now the log is read once from one finalized block, checked with
//! `transcriptComplete` before any receipt, kept with the round and saved
//! with the book; the next run finishes an unfinished round on its draw.
//!
//! Review REVLOG-R4 (the reviewer's `r4_poc_R4.rs`, 0 blocking): a round
//! settled and dropped from the book before its results were written was
//! lost by a crash in that window (R4-1); and the runners took every reveal
//! at `commit_close + 1`, hiding that receipts wait ~20 minutes for the
//! finalized block, far past `reveal_close` (R4-3). Now the book keeps a
//! settled round until its results are written, and each reveal is checked
//! when it arrives, by the coordinator's clock, and saved before its
//! receipt.

use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    sign_commit, write_atomic, ChainView, ChainWait, Clock, DrawChain, DrawRecord, LogBlock,
    ObservationOutcome, ObserverSig, ObserverStatus, OnchainDraw, OnchainFact, Posted,
    PostedCommit, ReplayClock, SimulatedRegistry, TranscriptLogQuery,
};
use onyx_core::{keccak256, Hash32, MinerAddress, ObservationSpec};
use onyx_source_fiscaldata::round::{
    append_results, config, finish, observer_address, observer_key, results_store, run,
    run_and_save, run_waiting, store, transcript_query, Page, RoundRun, RunError, POOL_SIZE,
};
use onyx_source_fiscaldata::{recorded, spec, subject, template, RECORDED_WINDOW_START};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What a drawn observer does to the first observer's commit slot before the
/// runner posts it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Front {
    /// Posts ANOTHER commit, in the commit phase, signed with its own key
    /// (the griefer is the observer itself).
    Grief,
    /// A relayer posts the observer's own commit first.
    Relay,
}

/// A simulated registry where `front` happens to the first commit posted.
struct Griefed {
    inner: SimulatedRegistry,
    front: Front,
    /// Block time inside the round's commit phase.
    in_phase: u64,
    /// Block time once the commit phase is over on chain.
    after: u64,
    victim: Option<(MinerAddress, Hash32)>,
    /// Reads of a filled log slot that fail first (an RPC error; the
    /// reviewer's capped `eth_getLogs`).
    read_failures: Cell<u32>,
    /// `postReceipt` calls that succeed before `receipt_failures` fail.
    receipts_ok: u32,
    /// `postReceipt` calls that fail after the first `receipts_ok`.
    receipt_failures: u32,
    /// Reads of the griefer's filled slot that come back EMPTY (a lying or
    /// buggy node; review REVLOG-R3-1's stale read).
    stale_reads: Cell<u32>,
    /// The first reads served by a node behind the griefer's post (a load
    /// balancer): it does not have the block asked for.
    lagging_reads: Cell<u32>,
    /// That node's head: the block before the griefer's post.
    lag_head: Cell<u64>,
    /// The block every `commitLogOf` read named.
    reads_at: RefCell<Vec<u64>>,
    /// `transcriptComplete` answers false for a round with no reveal (the
    /// snapshot check before the first receipt).
    refused_snapshots: Cell<u32>,
}

impl ChainView for Griefed {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

fn key_of(a: &MinerAddress) -> [u8; 32] {
    (1..=POOL_SIZE)
        .find(|i| observer_address(*i) == *a)
        .map(observer_key)
        .expect("a pool member")
}

impl DrawChain for Griefed {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.inner.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.inner.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.inner.fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.inner.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, attempt)
    }
    fn post_commit(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        if self.victim.is_none() {
            self.inner.time = self.in_phase;
            let (c, s) = match self.front {
                Front::Grief => {
                    let c = keccak256(b"posted only, never revealed");
                    let s = sign_commit(&key_of(observer), round_id, attempt, &c).ok_or("sign")?;
                    (c, s)
                }
                Front::Relay => (*commit, *sig),
            };
            assert_eq!(
                self.inner
                    .post_commit(fact, attempt, observer, round_id, &c, &s)?,
                Posted::Now
            );
            self.victim = Some((*observer, c));
            self.lag_head.set(self.inner.height - 1);
            self.inner.time = self.after;
        }
        self.inner
            .post_commit(fact, attempt, observer, round_id, commit, sig)
    }
    fn post_receipt(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        if self.receipts_ok > 0 {
            self.receipts_ok -= 1;
        } else if self.receipt_failures > 0 {
            self.receipt_failures -= 1;
            return Err("postReceipt: connection reset".into());
        }
        self.inner
            .post_receipt(fact, attempt, observer, round_id, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.inner.log_block()
    }
    fn posted_commit(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.reads_at.borrow_mut().push(block);
        if self.lagging_reads.get() > 0 {
            self.lagging_reads.set(self.lagging_reads.get() - 1);
            if block > self.lag_head.get() {
                return Err(format!(
                    "header not found: block {block} (this node's head is {})",
                    self.lag_head.get()
                ));
            }
        }
        let p = self
            .inner
            .posted_commit(block, fact, attempt, observer, round_id)?;
        if p.is_some() && self.stale_reads.get() > 0 && self.victim.map(|v| v.0) == Some(*observer)
        {
            self.stale_reads.set(self.stale_reads.get() - 1);
            return Ok(None);
        }
        if p.is_some() && self.read_failures.get() > 0 {
            self.read_failures.set(self.read_failures.get() - 1);
            return Err("commitLogOf: connection reset".into());
        }
        Ok(p)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        let ok = self.inner.transcript_complete(q)?;
        if !ok && q.entries.iter().all(|e| !e.revealed) {
            self.refused_snapshots.set(self.refused_snapshots.get() + 1);
        }
        Ok(ok)
    }
}

fn pages() -> (String, Vec<Page>) {
    let rec = recorded();
    let pages = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    (rec.record_date, pages)
}

fn griefed(front: Front, date: &str) -> Griefed {
    let sp = spec(date, RECORDED_WINDOW_START).expect("spec");
    let cfg = config(&sp);
    Griefed {
        inner: SimulatedRegistry::new(),
        front,
        in_phase: sp.window_start + 1,
        after: cfg.commit_close + 1,
        victim: None,
        read_failures: Cell::new(0),
        receipts_ok: 0,
        receipt_failures: 0,
        stale_reads: Cell::new(0),
        lagging_reads: Cell::new(0),
        lag_head: Cell::new(0),
        reads_at: RefCell::new(vec![]),
        refused_snapshots: Cell::new(0),
    }
}

/// HIGH-1, fixed: the griefer's posted commit is the one the round serves,
/// the griefer is a non-reveal (`Unrevealed`, never slashed), the other
/// observers settle the value, and the chain's `transcriptComplete` is true
/// for the round the coordinator records and for the round a client
/// rebuilds from the API answer. Before the fix the runner aborted on the
/// filled slot, and the transcript with the off-chain commit was refused
/// for good (asserted below as the old behaviour).
#[test]
fn a_griefing_observer_cannot_veto_the_round() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Grief, &date);
    let mut s = store();
    let r =
        run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("the run survives");
    let (griefer, posted) = chain.victim.expect("a griefer posted");
    let served = r
        .commits
        .iter()
        .find(|(a, _, _)| *a == griefer)
        .map(|(_, c, _)| *c);
    assert_eq!(served, Some(posted), "the posted commit was absorbed");
    assert!(r.reveals.iter().all(|(a, _, _, _)| *a != griefer));
    let status = r
        .settlement
        .statuses()
        .iter()
        .find(|(a, _)| *a == griefer)
        .map(|(_, st)| *st);
    assert_eq!(status, Some(ObserverStatus::Unrevealed));
    assert!(
        matches!(r.settlement.outcome(), ObservationOutcome::Value(_)),
        "the two honest observers settle the value: {:?}",
        r.settlement.outcome()
    );
    let fact = s.onchain_fact(&r.spec).expect("fact");
    let q = transcript_query(&r, fact.registry_key());
    assert!(
        chain.inner.log.complete(&q),
        "the recorded round is complete"
    );
    // The old behaviour: the round as sent off chain is refused for good.
    let mut old = q.clone();
    let e = old
        .entries
        .iter_mut()
        .find(|e| e.observer == griefer)
        .expect("drawn");
    e.commit = keccak256(b"what the griefer sent off chain");
    assert!(!chain.inner.log.complete(&old));
    // A client's own rebuild from the API answer passes the chain's check.
    let now = r.cfg.reveal_close + 1;
    let pins = TemplatePins::from([(template().metric.0.clone(), template().id())]);
    let feed =
        ObservationStore::from_json(&results_store(&r).expect("store").to_string(), now, &pins)
            .expect("loads");
    let rec = feed.record(&subject(&date), None, None).expect("record");
    let t = template();
    let v = verify_observation(
        &feed.body(rec),
        &ObservationExpect {
            template: &t,
            subject: &r.spec.subject,
            window_end: None,
            min_window_end: 0,
            now,
        },
    )
    .expect("the client verifies");
    assert!(chain.inner.log.complete(&v.checked.log));
    assert!(v
        .checked
        .log
        .entries
        .iter()
        .any(|e| e.observer == griefer && e.commit == posted && !e.revealed));
}

/// HIGH-1: a slot someone filled first with the observer's own commit is
/// success (the runner aborted on `AlreadyPosted`).
#[test]
fn a_commit_relayed_first_is_success() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Relay, &date);
    let mut s = store();
    let r =
        run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("the run survives");
    let (relayed, _) = chain.victim.expect("relayed");
    assert!(r.reveals.iter().any(|(a, _, _, _)| *a == relayed));
    let fact = s.onchain_fact(&r.spec).expect("fact");
    assert!(chain
        .inner
        .log
        .complete(&transcript_query(&r, fact.registry_key())));
}

/// The coordinator never reads the log while a post could still land in
/// the commit phase: a chain whose confirmed time is not past
/// `commit_close` is polled, and when the wait runs out the run stops
/// before any reveal (and so before any receipt) with its round UNFINISHED,
/// not published as a failed attempt (review REVLOG-R2 LOW-4: waiting
/// never burns one of the fact's draws). Finished later, on the same draw,
/// it settles the value.
#[test]
fn waiting_for_the_commit_phase_never_burns_a_draw() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Relay, &date);
    let close = chain.after;
    chain.after = chain.in_phase;
    let mut s = store();
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("waits");
    assert!(e.error.contains("not past commit close"), "{}", e.error);
    let (r, chain) = finish_later(&date, &mut s, chain, e, |c| c.inner.time = close);
    assert!(matches!(
        r.settlement.outcome(),
        ObservationOutcome::Value(_)
    ));
    assert_eq!(r.draw.attempt, 0, "the same draw");
    let _ = chain;
}

/// `e` left its round unfinished and published nothing; `heal` the chain,
/// [`finish`] the round and check the chain's log takes it.
fn finish_later(
    date: &str,
    s: &mut onyx_coordinator::observe::ObservationResults,
    mut chain: Griefed,
    e: RunError,
    heal: impl FnOnce(&mut Griefed),
) -> (onyx_source_fiscaldata::round::RoundRun, Griefed) {
    assert!(
        e.attempt.is_none(),
        "no failed round to publish: {}",
        e.error
    );
    let u = *e.unfinished.expect("the round is kept unfinished");
    let fact = s.onchain_fact(u.round.spec()).expect("fact");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1), "no re-draw");
    heal(&mut chain);
    let r = finish(date, s, u, &mut chain, &ChainWait::NOW).expect("finished on the same draw");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1), "still one draw");
    assert!(chain
        .inner
        .log
        .complete(&transcript_query(&r, fact.registry_key())));
    (r, chain)
}

/// Review REVLOG-R2 HIGH-1 (was the reviewer's `poc_getlogs_failure_is_a_veto`):
/// reading the griefer's posted commit failed (Base Sepolia's public RPC
/// refuses `eth_getLogs` ranges over 200 blocks; other providers cap
/// results, which a griefer fills), so the run failed and published a
/// failed attempt WITHOUT the posted commit, which clients refuse with
/// every later value of the fact. Now the commit and its signature are read
/// from contract state (`commitLogOf`, forge
/// `test_TheLogIsReadFromStateNotEvents`), a failed read is retried, and a
/// read that keeps failing leaves the round unfinished: nothing is
/// published until it is read, then the round finishes on the same draw.
#[test]
fn a_failed_log_read_is_a_wait_never_a_veto() {
    let (date, pages) = pages();
    // Transient: two failed reads, three tries.
    let mut chain = griefed(Front::Grief, &date);
    chain.read_failures.set(2);
    let mut s = store();
    let r = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("retried");
    let (griefer, posted) = chain.victim.expect("a griefer posted");
    assert!(r
        .commits
        .iter()
        .any(|(a, c, _)| *a == griefer && *c == posted));
    let fact = s.onchain_fact(&r.spec).expect("fact");
    assert!(chain
        .inner
        .log
        .complete(&transcript_query(&r, fact.registry_key())));
    // Lasting: the round is kept, never published without the post.
    let mut chain = griefed(Front::Grief, &date);
    chain.read_failures.set(1_000);
    let mut s = store();
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("the log cannot be read");
    assert!(e.error.contains("commitLogOf"), "{}", e.error);
    let (r, chain) = finish_later(&date, &mut s, chain, e, |c| c.read_failures.set(0));
    let (griefer, posted) = chain.victim.expect("a griefer posted");
    assert!(r
        .commits
        .iter()
        .any(|(a, c, _)| *a == griefer && *c == posted));
    assert!(matches!(
        r.settlement.outcome(),
        ObservationOutcome::Value(_)
    ));
}

/// Review REVLOG-R2 MEDIUM-2 (was the reviewer's
/// `poc_reveal_error_after_receipts_voids_the_attempt`), runner level: the
/// last `postReceipt` fails after two receipts are on chain. A retry
/// finishes it; when it keeps failing the round is kept unfinished with
/// every reveal (never published as a failed attempt that drops receipted
/// reveals) and finished on the same draw. `Attempt::of_failed_round`
/// refuses such a round outright. The observer-side half (a resent,
/// badly signed or late reveal is never an error) is
/// `observe::tests::an_observer_cannot_stop_the_round_after_receipts`.
#[test]
fn an_error_after_receipts_never_voids_the_attempt() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Relay, &date);
    chain.receipts_ok = 2;
    chain.receipt_failures = 1;
    let mut s = store();
    run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("a failed postReceipt is retried");
    let mut chain = griefed(Front::Relay, &date);
    chain.receipts_ok = 2;
    chain.receipt_failures = 1_000;
    let mut s = store();
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("postReceipt keeps failing");
    assert!(e.error.contains("postReceipt"), "{}", e.error);
    let u = e.unfinished.as_ref().expect("unfinished");
    // Two receipts on chain, the third reveal taken: a majority of 5.
    assert_eq!(u.round.signed_reveals().len(), 3, "every reveal kept");
    assert!(
        !chain.inner.log.complete(
            &onyx_api::observations::check_attempt(
                &template(),
                &onyx_api::observations::Attempt {
                    subject: u.round.spec().subject.clone(),
                    window_start: u.round.spec().window_start,
                    cfg: u.round.config(),
                    pool: u.round.pool().to_vec(),
                    draw: u.round.draw(),
                    observers: u.round.observers().to_vec(),
                    commits: u.round.signed_commits(),
                    reveals: vec![],
                    sealed: Default::default(),
                },
                u.round.config().reveal_close + 1,
            )
            .expect("checks off chain")
            .log
        ),
        "the old failed attempt (reveals cleared) is refused on chain: the PoC"
    );
    assert!(
        onyx_api::observations::Attempt::of_failed_round(&template(), &u.round).is_err(),
        "a round whose reveals settle a value is never a failed attempt"
    );
    let (r, _) = finish_later(&date, &mut s, chain, e, |c| c.receipt_failures = 0);
    assert_eq!(r.reveals.len(), r.observers.len(), "no reveal dropped");
    assert!(matches!(
        r.settlement.outcome(),
        ObservationOutcome::Value(_)
    ));
}

/// A fresh book path for one test.
fn book_path(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-revlog-r3-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).expect("temp dir");
    d.join("book.json")
}

/// Review REVLOG-R3-1 (the reviewer's `r3_stale_read_then_retry_is_a_permanent_veto`):
/// ONE stale read of the griefer's posted commit made the first pass take
/// the griefer's off-chain reveal and post a RECEIPT for its off-chain
/// commit C1; every retry re-read the log, absorbed C2 and dropped the
/// receipted reveal, so `transcriptComplete` stayed false for good and the
/// fact was void. Now the log is read once from one block, and before the
/// snapshot is kept the chain's `transcriptComplete` must accept the round
/// with no reveal: the stale answer is refused there, BEFORE any receipt,
/// the read is tried again and the round finishes on its one draw. No
/// receipt ever names C1.
#[test]
fn r3_a_stale_read_is_caught_before_any_receipt() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Grief, &date);
    chain.stale_reads.set(1);
    let mut s = store();
    let r = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("the stale read is refused and read again");
    let (griefer, posted) = chain.victim.expect("griefer posted");
    let fact = s.onchain_fact(&r.spec).expect("fact");
    assert_eq!(chain.stale_reads.get(), 0, "the stale answer was served");
    assert_eq!(
        chain.refused_snapshots.get(),
        1,
        "and refused by the snapshot check, before any receipt"
    );
    assert_eq!(
        chain
            .inner
            .log
            .receipt(&fact.registry_key(), 0, &griefer, &r.round_id),
        None,
        "no receipt for the griefer's off-chain commit"
    );
    assert!(r
        .commits
        .iter()
        .any(|(a, c, _)| *a == griefer && *c == posted));
    assert!(r.reveals.iter().all(|(a, _, _, _)| *a != griefer));
    assert!(matches!(
        r.settlement.outcome(),
        ObservationOutcome::Value(_)
    ));
    let reads = chain.reads_at.borrow();
    assert!(
        reads.windows(2).all(|w| w[0] == w[1]),
        "every read named one block: {reads:?}"
    );
    assert!(chain
        .inner
        .log
        .complete(&transcript_query(&r, fact.registry_key())));
    assert_eq!(chain.inner.draw_count(&fact), Ok(1), "one draw");
}

/// Review REVLOG-R3-1: a node that keeps answering stale leaves the round
/// unfinished with NO receipt on chain (nothing was taken before the
/// snapshot), and the next run finishes it on the same draw.
#[test]
fn r3_a_lasting_stale_read_receipts_nothing_and_waits() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Grief, &date);
    chain.stale_reads.set(1_000);
    let mut s = store();
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("never accepted");
    assert!(e.error.contains("disagrees"), "{}", e.error);
    let u = e.unfinished.as_ref().expect("unfinished");
    assert!(u.round.log_snapshot().is_none(), "no snapshot kept");
    assert!(u.round.signed_reveals().is_empty(), "no reveal taken");
    let fact = s.onchain_fact(u.round.spec()).expect("fact");
    for o in u.round.observers() {
        assert_eq!(
            chain
                .inner
                .log
                .receipt(&fact.registry_key(), 0, &o.address, &u.round.round_id()),
            None,
            "no receipt posted"
        );
    }
    chain.stale_reads.set(0);
    let r = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("the next run finishes it");
    assert_eq!(r.draw.attempt, 0, "the same draw");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1));
    assert!(matches!(
        r.settlement.outcome(),
        ObservationOutcome::Value(_)
    ));
}

/// Review REVLOG-R3-1: two reads that would have hit different blocks. The
/// old runner computed "head minus confirmations" for each call, so behind
/// a load balancer one read could come from a node whose head is before
/// the griefer's post (an empty slot: a stale answer). Now one block N is
/// picked and every `commitLogOf` names it: the node behind does not have
/// N and errors, the read is retried, and the post is absorbed.
#[test]
fn r3_two_nodes_two_blocks_one_read_block() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Grief, &date);
    chain.lagging_reads.set(1);
    let mut s = store();
    let r = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("finishes");
    let (griefer, posted) = chain.victim.expect("griefer posted");
    let fact = s.onchain_fact(&r.spec).expect("fact");
    let n = *chain.reads_at.borrow().first().expect("read");
    let lag = chain.lag_head.get();
    // What the two blocks hold: the old per-call read on the node behind
    // saw the slot empty.
    assert_eq!(
        chain
            .inner
            .posted_commit(lag, &fact, 0, &griefer, &r.round_id)
            .expect("read"),
        None,
        "at the lagging node's head the slot is empty"
    );
    assert_eq!(
        chain
            .inner
            .posted_commit(n, &fact, 0, &griefer, &r.round_id)
            .expect("read")
            .map(|p| p.commit),
        Some(posted),
        "at N it holds the griefer's post"
    );
    assert!(chain.reads_at.borrow().iter().all(|b| *b == n), "one block");
    assert_eq!(chain.lagging_reads.get(), 0, "the node behind answered");
    assert_eq!(
        chain.refused_snapshots.get(),
        0,
        "with an error, not a stale state"
    );
    assert!(r
        .commits
        .iter()
        .any(|(a, c, _)| *a == griefer && *c == posted));
    assert_eq!(
        chain
            .inner
            .log
            .receipt(&fact.registry_key(), 0, &griefer, &r.round_id),
        None
    );
}

/// Review REVLOG-R3-1: once the snapshot is kept (and receipts are on
/// chain), nothing reads the log again, across a restart too: the round is
/// saved with the book, loaded by a fresh store, and finished with a node
/// that now answers every read stale. No read is made; the snapshot (with
/// the griefer's posted commit) is reused.
#[test]
fn r3_the_snapshot_is_never_read_again() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Grief, &date);
    chain.receipts_ok = 1;
    chain.receipt_failures = 1_000;
    let mut s = store();
    let book = book_path("snapshot");
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("postReceipt keeps failing");
    let u = e.unfinished.as_ref().expect("unfinished");
    let snap = u.round.log_snapshot().expect("snapshot kept").clone();
    s.save_book(&book).expect("saved");
    let mut fresh = store();
    fresh.load_book(&book).expect("loads");
    assert_eq!(
        fresh
            .unfinished(u.round.spec())
            .and_then(|k| k.round.log_snapshot()),
        Some(&snap),
        "the snapshot travels with the book"
    );
    chain.receipt_failures = 0;
    chain.stale_reads.set(1_000);
    let reads = chain.reads_at.borrow().len();
    let r = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut fresh,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect("finished from the saved snapshot");
    assert_eq!(chain.reads_at.borrow().len(), reads, "no read of the log");
    let (griefer, posted) = chain.victim.expect("griefer");
    assert!(r
        .commits
        .iter()
        .any(|(a, c, _)| *a == griefer && *c == posted));
    let fact = fresh.onchain_fact(&r.spec).expect("fact");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1));
    assert!(chain
        .inner
        .log
        .complete(&transcript_query(&r, fact.registry_key())));
}

/// Review REVLOG-R3-2 (the reviewer's
/// `r3_an_unfinished_round_dropped_by_the_binary_burns_and_voids`): the
/// operator binaries dropped `RunError::unfinished`, so the next run
/// announced a NEW draw and the first draw (beacon fixed, no published
/// round) voided the fact. Now the store keeps the round and saves it with
/// the book: the next run, in the same process or after a restart, finishes
/// the SAME draw (attempt 0, `drawCount` 1).
#[test]
fn r3_an_unfinished_round_is_finished_by_the_next_run_on_the_same_draw() {
    let (date, pages) = pages();
    for restart in [false, true] {
        let mut chain = griefed(Front::Relay, &date);
        chain.read_failures.set(1_000);
        let mut s = store();
        let e = run_waiting(
            &date,
            RECORDED_WINDOW_START,
            &pages,
            &mut s,
            &mut chain,
            &ChainWait::NOW,
        )
        .expect_err("cannot read the log");
        assert!(e.unfinished.is_some() && e.attempt.is_none());
        drop(e); // what level2-round.rs did: `return Err(e.error)`
        if restart {
            let book = book_path("restart");
            s.save_book(&book).expect("saved");
            s = store();
            s.load_book(&book).expect("loads");
        }
        chain.read_failures.set(0);
        let r = run_waiting(
            &date,
            RECORDED_WINDOW_START,
            &pages,
            &mut s,
            &mut chain,
            &ChainWait::NOW,
        )
        .expect("the next run");
        let fact = s.onchain_fact(&r.spec).expect("fact");
        assert_eq!(r.draw.attempt, 0, "the same draw (restart: {restart})");
        assert_eq!(chain.inner.draw_count(&fact), Ok(1));
        assert!(s.unfinished(&r.spec).is_none(), "nothing left unfinished");
        assert!(chain
            .inner
            .log
            .complete(&transcript_query(&r, fact.registry_key())));
    }
}

/// Review REVLOG-R3-3: `finish` again after the round already settled
/// (a run that settled, then died before its result was used, retried from
/// the saved unfinished round) is done with the recorded settlement, not
/// `AlreadySettled` for good.
#[test]
fn r3_finish_is_idempotent_after_settle() {
    let (date, pages) = pages();
    let mut chain = griefed(Front::Relay, &date);
    chain.receipts_ok = 2;
    chain.receipt_failures = 1_000;
    let mut s = store();
    let e = run_waiting(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &ChainWait::NOW,
    )
    .expect_err("postReceipt keeps failing");
    let u = *e.unfinished.expect("unfinished");
    let saved = u.clone();
    chain.receipt_failures = 0;
    let first = finish(&date, &mut s, u, &mut chain, &ChainWait::NOW).expect("settles");
    let again = finish(&date, &mut s, saved, &mut chain, &ChainWait::NOW)
        .expect("done, not AlreadySettled");
    assert_eq!(again.settlement, first.settlement);
    assert_eq!(again.claim, first.claim);
}

/// Review REVLOG-R4-1 (the reviewer's
/// `r4_settled_round_lost_between_book_save_and_results_write`): settling
/// dropped the round from the book and `run_and_save` saved the book BEFORE
/// the binary wrote `--store`, so a crash in that window lost a settled round
/// whose receipts are on chain; the next run announced a NEW draw (attempt
/// 1), and draw 0, fixed on chain without a published round, made clients
/// refuse the fact. Now the book on disk keeps the round, unsettled, until
/// `publish` has written the results: the next run publishes the SAME round
/// on the same draw (attempt 0, `drawCount` 1, counted once). A crash after
/// the write publishes it again and the results store is left as it was.
#[test]
fn r4_a_settled_round_survives_a_crash_before_its_results_are_written() {
    let (date, pages) = pages();
    let book = book_path("r4-publish");
    let _ = std::fs::remove_file(&book);
    let results = book.with_file_name("results.json");
    let _ = std::fs::remove_file(&results);
    let sp = spec(&date, RECORDED_WINDOW_START).expect("spec");
    let write = |r: &RoundRun| -> Result<(), String> {
        let prev: Option<serde_json::Value> = std::fs::read_to_string(&results)
            .ok()
            .map(|t| serde_json::from_str(&t).expect("results json"));
        let v = append_results(prev.as_ref(), r)?;
        write_atomic(&results, &format!("{v}\n"))
    };
    let mut chain = SimulatedRegistry::new();
    let run_once = |chain: &mut SimulatedRegistry,
                    publish: &mut dyn FnMut(&RoundRun) -> Result<(), String>| {
        // Each run is a fresh process: the store comes from the book alone.
        let mut s = store();
        s.load_book(&book).expect("loads");
        let written: Option<serde_json::Value> = std::fs::read_to_string(&results)
            .ok()
            .map(|t| serde_json::from_str(&t).expect("results json"));
        run_and_save(
            &date,
            RECORDED_WINDOW_START,
            &pages,
            &mut s,
            chain,
            &book,
            written.as_ref(),
            &ChainWait::NOW,
            &ReplayClock::default(),
            &mut NoInbox,
            publish,
            &mut |_| Ok(()),
        )
    };
    // Run 1 settles the value, then dies before its results are written
    // (the PoC's window): the store is dropped unsaved.
    let e =
        run_once(&mut chain, &mut |_| Err("killed before the write".into())).expect_err("crashed");
    let first = e.unfinished.expect("the settled round is kept");
    let mut s = store();
    s.load_book(&book).expect("loads");
    assert!(
        s.unfinished(&sp).is_some(),
        "the book on disk holds the round, unsettled"
    );
    // Run 2 publishes it, then dies after the write, before the book drops it.
    let mut crash_after = |r: &RoundRun| -> Result<(), String> {
        write(r)?;
        Err("killed after the write".into())
    };
    run_once(&mut chain, &mut crash_after).expect_err("crashed again");
    // Run 3 publishes it again and drops it from the book.
    let r = run_once(&mut chain, &mut |r| write(r)).expect("published");
    let fact = s.onchain_fact(&r.spec).expect("fact");
    assert_eq!(r.draw.attempt, 0, "the same draw, never a new one");
    assert_eq!(chain.draw_count(&fact), Ok(1));
    assert_eq!(r.round_id, first.round.round_id());
    r.value().expect("the value");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&results).expect("results")).expect("json");
    assert_eq!(
        v["rounds"].as_array().map(Vec::len),
        Some(1),
        "written once"
    );
    let mut s = store();
    s.load_book(&book).expect("loads");
    assert!(s.unfinished(&sp).is_none(), "published: gone from the book");
    let _ = std::fs::remove_dir_all(book.parent().expect("dir"));
}

/// A simulated registry on a live clock: every read of the log block moves
/// the clock 5 s (one poll), and `finalized` lags `lag` seconds behind the
/// head (Base Sepolia, measured: 1,178 s). Posts land at the clock's time;
/// each receipt records the clock and checks the book on disk already holds
/// that observer's reveal.
struct Lagging {
    inner: SimulatedRegistry,
    clock: Rc<ReplayClock>,
    lag: u64,
    book: std::path::PathBuf,
    spec: ObservationSpec,
    receipts_at: RefCell<Vec<u64>>,
    unsaved: Cell<u32>,
}

impl ChainView for Lagging {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for Lagging {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.inner.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.inner.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.inner.fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.inner.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, attempt)
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        self.inner.time = self.clock.now();
        self.inner.post_commit(f, attempt, o, round, commit, sig)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        self.receipts_at.borrow_mut().push(self.clock.now());
        let mut s = store();
        s.load_book(&self.book).expect("the book loads");
        let saved = s
            .unfinished(&self.spec)
            .is_some_and(|u| u.reveals.iter().any(|a| a.reveal.0 == *o));
        if !saved {
            self.unsaved.set(self.unsaved.get() + 1);
        }
        self.inner.post_receipt(f, attempt, o, round, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.clock.advance(5);
        Ok(Some(LogBlock {
            number: self.inner.height,
            time: self.clock.now().saturating_sub(self.lag),
        }))
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.inner.posted_commit(block, f, attempt, o, round)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.inner.transcript_complete(q)
    }
}

/// Review REVLOG-R4-3 (Medium, launch-gating): a run on a live clock with
/// the measured Base Sepolia finality lag (1,178 s between `latest` and
/// `finalized`) still settles the value. Receipts can be signed only once
/// the finalized block is past `commit_close`, about 20 minutes later and
/// far past `reveal_close` (`commit_close + 120`); each reveal is stamped and
/// checked when it ARRIVES, in the reveal phase, and is on disk with the
/// book before its receipt. The runners used to hide the lag by taking every
/// reveal at `commit_close + 1` whenever the receipt was signed.
#[test]
fn r4_a_live_clock_run_with_the_measured_finality_lag_yields_a_value() {
    let (date, pages) = pages();
    let sp = spec(&date, RECORDED_WINDOW_START).expect("spec");
    let cfg = config(&sp);
    let book = book_path("r4-live");
    let _ = std::fs::remove_file(&book);
    let start = pages.iter().map(|p| p.fetched_unix).max().expect("pages");
    assert!(
        start <= cfg.commit_close,
        "the observers fetch in the window"
    );
    let clock = Rc::new(ReplayClock::at(start));
    let mut chain = Lagging {
        inner: SimulatedRegistry::new(),
        clock: clock.clone(),
        lag: 1_178,
        book: book.clone(),
        spec: sp.clone(),
        receipts_at: RefCell::new(vec![]),
        unsaved: Cell::new(0),
    };
    let mut s = store();
    s.load_book(&book).expect("first run");
    let wait = ChainWait {
        tries: 3,
        polls: 1_000,
        pause: std::time::Duration::ZERO,
    };
    let mut published_at = None;
    let r = run_and_save(
        &date,
        RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
        &book,
        None,
        &wait,
        &*clock,
        &mut NoInbox,
        &mut |_| {
            published_at = Some(clock.now());
            Ok(())
        },
        &mut |_| Ok(()),
    )
    .expect("the round finishes");
    assert_eq!(r.draw.attempt, 0);
    assert_eq!(r.reveals.len(), r.observers.len(), "every reveal counts");
    assert!(
        matches!(r.settlement.outcome(), ObservationOutcome::Value(_)),
        "{:?}",
        r.settlement.outcome()
    );
    r.value().expect("a level-2 value");
    let receipts = chain.receipts_at.borrow().clone();
    assert_eq!(receipts.len(), r.observers.len());
    assert!(
        receipts.iter().all(|t| *t > cfg.commit_close + 1_178),
        "every receipt waited for the finalized block: {receipts:?}"
    );
    assert!(
        receipts.iter().all(|t| *t > cfg.reveal_close),
        "so every receipt came after reveal_close {}",
        cfg.reveal_close
    );
    assert_eq!(
        chain.unsaved.get(),
        0,
        "each reveal saved before its receipt"
    );
    assert!(published_at.is_some_and(|t| t > cfg.reveal_close));
    let _ = std::fs::remove_dir_all(book.parent().expect("dir"));
}
