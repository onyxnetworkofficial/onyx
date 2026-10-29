//! Review REVLOG-R5 (High, pre-existing): losing an opened draw voided the
//! fact, and anyone could trigger it. `fixBeacon` is public and fixes a
//! beacon once: a stranger's call landing between the coordinator's `drawOf`
//! read and its own send made the send revert, and an RPC failure on the
//! reads after the coordinator's own send did the same; `draw_and_open`
//! returned Err with no failed attempt and no unfinished round, the book had
//! counted the draw, the next run announced draw 1, and draw 0 (a fixed
//! beacon, no published round) made every client refuse the fact. Also THREATS
//! REVLOG-R4-5: a crash between the announcement and the opened round, or
//! between a failed attempt's write and the book save, lost that draw's
//! round the same way.
//!
//! Each test is the attack or the crash, now ending with the fact verifying
//! for a client: `SimulatedRegistry::fix_beacon` reverts on a fixed beacon
//! like `OnyxRegistry.fixBeacon` (`BeaconAlreadyFixed`), so the race is real.

use onyx_api::client::{verify_attempts_onchain, verify_observation, ObservationExpect};
use onyx_api::observations::{Attempt, ObservationStore, TemplatePins};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    write_atomic, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObserverSig, OnchainDraw,
    OnchainFact, Posted, PostedCommit, ReplayClock, SimulatedRegistry, TranscriptLogQuery,
};
use onyx_core::{Hash32, MinerAddress};
use onyx_source_fiscaldata::round::{
    append_attempt, append_results, run_and_save, store, Page, RoundRun, RunError,
};
use onyx_source_fiscaldata::{recorded, subject, template, RECORDED_WINDOW_START};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// How `fixBeacon` goes wrong for the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Nothing goes wrong.
    None,
    /// The reviewer's PoC: the beacon IS fixed on chain (by the
    /// coordinator's own send), but the coordinator sees an error.
    FixedThenErr,
    /// A stranger's `fixBeacon` lands after the coordinator's read and
    /// before its send: the send reverts `BeaconAlreadyFixed`.
    StrangerFirst,
    /// The coordinator's send lands, then the next two reads fail (RPC).
    ReadsFailAfterSend,
    /// The process dies after the announcement, before `fixBeacon` (every
    /// later chain read fails too).
    CrashBeforeFix,
    /// The process dies right after its own `fixBeacon` landed.
    CrashAfterFix,
}

/// A `SimulatedRegistry` with one [`Fault`] on the next `fixBeacon`.
struct Faulty {
    inner: SimulatedRegistry,
    fault: Fault,
    failing_reads: Cell<u32>,
    dead: bool,
    announces: u32,
    own_reverts: u32,
}

impl Faulty {
    fn new(fault: Fault) -> Self {
        Faulty {
            inner: SimulatedRegistry::new(),
            fault,
            failing_reads: Cell::new(0),
            dead: false,
            announces: 0,
            own_reverts: 0,
        }
    }
    /// The next run: a fresh process with a working chain connection.
    fn restart(&mut self) {
        self.fault = Fault::None;
        self.dead = false;
        self.failing_reads.set(0);
    }
}

impl ChainView for Faulty {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for Faulty {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.inner.draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.announces += 1;
        self.inner.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        match std::mem::replace(&mut self.fault, Fault::None) {
            Fault::None => self.inner.fix_beacon(d),
            Fault::FixedThenErr => {
                self.inner.fix_beacon(d)?;
                Err("fixBeacon(bytes32,uint32) reverted".into())
            }
            Fault::StrangerFirst => {
                self.inner.fix_beacon(d)?; // the stranger's call
                let own = self.inner.fix_beacon(d);
                if own.is_err() {
                    self.own_reverts += 1;
                }
                own
            }
            Fault::ReadsFailAfterSend => {
                self.inner.fix_beacon(d)?;
                self.failing_reads.set(2);
                Err("drawOf: connection reset".into())
            }
            Fault::CrashBeforeFix => {
                self.dead = true;
                Err("killed".into())
            }
            Fault::CrashAfterFix => {
                self.inner.fix_beacon(d)?;
                self.dead = true;
                Err("killed".into())
            }
        }
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        if self.dead {
            return Err("the process is gone".into());
        }
        let n = self.failing_reads.get();
        if n > 0 {
            self.failing_reads.set(n - 1);
            return Err("drawOf: connection reset".into());
        }
        self.inner.draw_of(f, attempt)
    }
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        if self.dead {
            return Err("the process is gone".into());
        }
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
        self.inner.post_receipt(f, attempt, o, round, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.inner.log_block()
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

fn word(v: u64) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

fn draw_ret(r: &DrawRecord) -> Vec<u8> {
    let mut v = r.commit.0.to_vec();
    v.extend(word(r.beacon_height));
    v.extend(word(r.announced_at));
    v.extend_from_slice(&r.beacon.0);
    v
}

fn pages() -> Vec<Page> {
    recorded()
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect()
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("onyx-r5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// The results store at `path` with `add` appended (the binaries' write).
fn write_store(
    path: &Path,
    add: impl FnOnce(Option<&serde_json::Value>) -> Result<serde_json::Value, String>,
) -> Result<(), String> {
    let prev: Option<serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .map(|t| serde_json::from_str(&t).expect("json"));
    let v = add(prev.as_ref())?;
    write_atomic(path, &format!("{v}\n"))
}

/// One run from the book, as the `level2-round` binary does it.
fn run_once(
    chain: &mut Faulty,
    book: &Path,
    results: &Path,
    pages: &[Page],
    publish_failed: &mut dyn FnMut(&Attempt) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    let rec = recorded();
    let mut s = store();
    s.load_book(book).expect("the book loads");
    let written: Option<serde_json::Value> = std::fs::read_to_string(results)
        .ok()
        .map(|t| serde_json::from_str(&t).expect("results json"));
    run_and_save(
        &rec.record_date,
        RECORDED_WINDOW_START,
        pages,
        &mut s,
        chain,
        book,
        written.as_ref(),
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut NoInbox,
        &mut |r| write_store(results, |p| append_results(p, r)),
        publish_failed,
    )
}

/// The client's full check of the fact from the results store the runs
/// wrote: off chain, then `verify_attempts_onchain` against the chain's
/// `drawCount`, `drawOf` of every draw below the value and `drawBudget`.
fn client_accepts(chain: &mut Faulty, results: &Path, value: &RoundRun) {
    let fact = store().onchain_fact(&value.spec).expect("fact");
    chain.inner.record_value(&fact);
    let now = value.cfg.reveal_close + 1;
    let pins = TemplatePins::from([(template().metric.0.clone(), template().id())]);
    let feed = ObservationStore::from_json(
        &std::fs::read_to_string(results).expect("results"),
        now,
        &pins,
    )
    .expect("the results store loads");
    let r = feed
        .record(&subject(&recorded().record_date), None, None)
        .expect("record");
    let all = feed.body_all(r, &feed.attempts_of(&r.checked.fact));
    let t = template();
    let a = verify_observation(
        &all,
        &ObservationExpect {
            template: &t,
            subject: &value.spec.subject,
            window_end: None,
            min_window_end: 0,
            now,
        },
    )
    .expect("off-chain checks pass")
    .attempts
    .expect("outcome=all");
    let count = word(u64::from(chain.inner.draw_count(&fact).expect("count")));
    let draws: Vec<Vec<u8>> = (0..value.draw.attempt)
        .map(|i| draw_ret(&chain.inner.draw_of(&fact, i).expect("read").expect("draw")))
        .collect();
    let (strikes, allowance) = chain.inner.draw_budget(&fact).expect("budget");
    let mut budget = word(u64::from(strikes));
    budget.extend(word(u64::from(allowance)));
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    verify_attempts_onchain(
        &a,
        &count,
        &draws,
        &budget,
        &vec![yes; a.attempts.len()],
        &unsealed_all(&a),
    )
    .expect("the client accepts the fact");
}

/// The reviewer's PoC (`r5_a_failed_fix_beacon_after_the_beacon_is_fixed_voids_the_fact`):
/// the beacon of draw 0 is fixed on chain and the coordinator's
/// `fix_beacon` returns Err. Before: no attempt, no unfinished round, run 2
/// announced draw 1 and the client refused the fact for good. Now the draw
/// is read back, the fixed beacon is success, and draw 0 settles the value.
#[test]
fn r5_a_failed_fix_beacon_after_the_beacon_is_fixed_voids_the_fact() {
    let d = dir("poc");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::FixedThenErr);
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
        .expect("the fixed beacon is success: the run settles");
    assert_eq!(r.draw.attempt, 0, "the same draw, not a new one");
    r.value().expect("a value");
    let fact = store().onchain_fact(&r.spec).expect("fact");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1));
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// A stranger's `fixBeacon` lands between the coordinator's `drawOf` read
/// and its send: the coordinator's own send reverts (`BeaconAlreadyFixed`,
/// as `OnyxRegistry` does), and the beacon the stranger fixed is success.
#[test]
fn r5_a_stranger_fixing_the_beacon_first_is_success() {
    let d = dir("stranger");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::StrangerFirst);
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
        .expect("the stranger's beacon is success");
    assert_eq!(chain.own_reverts, 1, "the coordinator's send reverted");
    assert_eq!(r.draw.attempt, 0);
    r.value().expect("a value");
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// The coordinator's own `fixBeacon` lands, then the reads after it fail
/// twice: they are retried (`ChainWait`), and the draw is opened.
#[test]
fn r5_an_rpc_failure_after_the_own_send_is_retried() {
    let d = dir("rpc");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::ReadsFailAfterSend);
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
        .expect("the reads are retried");
    assert_eq!(chain.failing_reads.get(), 0, "both failed reads were hit");
    assert_eq!(r.draw.attempt, 0);
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// THREATS REVLOG-R4-5 part 1: the process dies after the announcement,
/// before (or right after) `fixBeacon`. The book holds the announced draw,
/// so the restart resumes draw 0 (no second announcement) and settles it.
#[test]
fn r5_a_crash_after_the_announcement_resumes_the_same_draw() {
    for fault in [Fault::CrashBeforeFix, Fault::CrashAfterFix] {
        let d = dir(&format!("{fault:?}"));
        let (book, results) = (d.join("book.json"), d.join("results.json"));
        let mut chain = Faulty::new(fault);
        let e = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
            .expect_err("the process died");
        assert!(e.error.contains("stays announced"), "{fault:?}: {e}");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&book).expect("book")).expect("json");
        assert_eq!(saved["announced"].as_array().map(Vec::len), Some(1));
        chain.restart();
        let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
            .expect("the restart resumes the draw");
        assert_eq!(r.draw.attempt, 0, "{fault:?}: draw 0 resumed");
        assert_eq!(chain.announces, 1, "{fault:?}: announced once");
        let fact = store().onchain_fact(&r.spec).expect("fact");
        assert_eq!(chain.inner.draw_count(&fact), Ok(1));
        client_accepts(&mut chain, &results, &r);
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// THREATS REVLOG-R4-5 part 2: a run fails after its beacon (two pages for
/// three observers), so its round is a failed attempt (ATT-R2-F3). The book
/// keeps it until it is written; a crash after the write (or before it)
/// and before the book drops it publishes it again on restart, once; the
/// next run draws again and the client accepts the fact.
#[test]
fn r5_a_crash_between_the_failed_attempt_write_and_the_book_save_loses_nothing() {
    for written in [true, false] {
        let d = dir(&format!("failed-{written}"));
        let (book, results) = (d.join("book.json"), d.join("results.json"));
        let mut chain = Faulty::new(Fault::None);
        let good = pages();
        let e = run_once(&mut chain, &book, &results, &good[..2], &mut |a| {
            if written {
                write_store(&results, |p| append_attempt(p, a))?;
            }
            Err("killed".into())
        })
        .expect_err("the run fails after its beacon");
        assert!(e.attempt.is_some(), "a failed attempt");
        assert!(e.error.contains("the book keeps it"), "{e}");
        assert_eq!(results.exists(), written);
        // Restart: the kept failed attempt is published (again), then dropped.
        let e = run_once(&mut chain, &book, &results, &good, &mut |a| {
            write_store(&results, |p| append_attempt(p, a))
        })
        .expect_err("publishes the failed round first");
        assert_eq!(e.attempt.map(|a| a.draw.attempt), Some(0));
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&book).expect("book")).expect("json");
        assert_eq!(saved["failed"].as_array().map(Vec::len), Some(0));
        assert_eq!(saved["unfinished"].as_array().map(Vec::len), Some(0));
        // The next run draws again and settles the value.
        let r = run_once(&mut chain, &book, &results, &good, &mut |_| {
            panic!("nothing failed")
        })
        .expect("the re-draw settles");
        assert_eq!(r.draw.attempt, 1);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&results).expect("r")).expect("json");
        assert_eq!(
            v["attempts"].as_array().map(Vec::len),
            Some(1),
            "the failed round is stored once"
        );
        client_accepts(&mut chain, &results, &r);
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Review REVLOG-R5 (info): the results store takes one entry per draw. An
/// identical entry appended again is a no-op (a retried publication); a
/// second, different entry for the same draw (here the value round's draw
/// published again as a failed attempt) is refused at the write.
#[test]
fn r5_the_results_store_refuses_a_second_entry_for_one_draw() {
    let d = dir("append");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::None);
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("settles");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&results).expect("r")).expect("json");
    assert_eq!(
        append_results(Some(&stored), &r).expect("identical: no-op"),
        stored
    );
    let other = onyx_source_fiscaldata::round::attempt(&r);
    let e = append_attempt(Some(&stored), &other).expect_err("a second entry for draw 0");
    assert!(e.contains("second, different entry"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

/// ROADMAP C11: what the registry answers to each published round's sealed
/// calls when its draw has no reveal key (no sealed post) and the round is
/// recorded.
fn unsealed_all(a: &onyx_api::client::VerifiedAttempts) -> Vec<onyx_api::client::SealedRets> {
    a.attempts
        .iter()
        .map(|c| onyx_api::client::honest_sealed_returns(&c.reveal_log, None, true, true))
        .collect()
}
