//! Review R6: three ways the coordinator's book and the registry could
//! disagree about a draw, each ending with a fixed beacon and no published
//! round, so every client refused the fact (`ChainMismatch("a draw with a
//! fixed beacon and no published round")`):
//!
//! - R6-B: a saved announced draw was forgotten after ONE stale `drawCount`
//!   read on restart; the next run settled draw 1 while anyone could fix draw 0.
//! - R6-A: an `announceDraw` that landed but whose send reported an error
//!   (receipt timeout) was forgotten (`unannounce_latest`); same outcome.
//!   With it, THREATS REVLOG-R5-2: a crash after `announceDraw` landed and
//!   before the first book save left the chain's draw out of the book.
//! - R6-D: the give-up check read `drawOf` and the height at "latest",
//!   possibly from two nodes; a lagging `drawOf` past the blockhash window
//!   gave up a draw whose beacon was fixed.
//!
//! Each test is the reviewer's PoC (or its sibling), now ending with the
//! client accepting the fact.

use onyx_api::client::{verify_attempts_onchain, verify_observation, ObservationExpect};
use onyx_api::observations::{Attempt, ObservationStore, TemplatePins};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    write_atomic, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObserverSig, OnchainDraw,
    OnchainFact, Posted, PostedCommit, ReplayClock, SimulatedRegistry, TranscriptLogQuery,
    BLOCKHASH_WINDOW,
};
use onyx_core::{Hash32, MinerAddress};
use onyx_source_fiscaldata::round::{
    append_results, run_and_save, store, Page, RoundRun, RunError,
};
use onyx_source_fiscaldata::{recorded, subject, template, RECORDED_WINDOW_START};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// How `fixBeacon` goes wrong for the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Nothing goes wrong.
    None,
    /// The process dies after the announcement, before `fixBeacon` (every
    /// later chain read fails too).
    CrashBeforeFix,
    /// R6-D: the coordinator's own send lands and reports an error, the
    /// chain moves past the blockhash window, and the latest `drawOf` comes
    /// from a lagging node (beacon zero).
    FixedThenStalePastWindow,
}

/// A `SimulatedRegistry` with one [`Fault`] on the next `fixBeacon`, an
/// `announceDraw` that may land and report an error, stale reads (lagging
/// nodes) and reads that fail.
struct Faulty {
    inner: SimulatedRegistry,
    fault: Fault,
    dead: bool,
    announces: u32,
    /// The next `announceDraw` lands, then reports a receipt timeout.
    announce_lands_err: bool,
    /// `drawCount` reads answered one short (a lagging node).
    stale_count: Cell<u32>,
    /// Latest `drawOf` reads answered with the beacon zero (a lagging node).
    stale_draw_of: Cell<u32>,
    /// Every drawOf read (latest and final) fails while set.
    reads_down: Cell<bool>,
    last: Option<OnchainDraw>,
}

impl Faulty {
    fn new(fault: Fault) -> Self {
        Faulty {
            inner: SimulatedRegistry::new(),
            fault,
            dead: false,
            announces: 0,
            announce_lands_err: false,
            stale_count: Cell::new(0),
            stale_draw_of: Cell::new(0),
            reads_down: Cell::new(false),
            last: None,
        }
    }
    /// The next run: a fresh process with a working chain connection.
    fn restart(&mut self) {
        self.fault = Fault::None;
        self.dead = false;
        self.stale_draw_of.set(0);
        self.reads_down.set(false);
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
        let n = self.inner.draw_count(f)?;
        let s = self.stale_count.get();
        if s > 0 {
            self.stale_count.set(s - 1);
            return Ok(n.saturating_sub(1));
        }
        Ok(n)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.inner.draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.announces += 1;
        self.last = Some(*d);
        self.inner.announce(d)?;
        if std::mem::replace(&mut self.announce_lands_err, false) {
            return Err("cast send: timed out waiting for the receipt".into());
        }
        Ok(())
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        match std::mem::replace(&mut self.fault, Fault::None) {
            Fault::None => self.inner.fix_beacon(d),
            Fault::CrashBeforeFix => {
                self.dead = true;
                Err("killed".into())
            }
            Fault::FixedThenStalePastWindow => {
                self.inner.fix_beacon(d)?;
                self.inner.height = d.beacon_height + BLOCKHASH_WINDOW + 1;
                self.stale_draw_of.set(1000);
                Err("cast send: timed out".into())
            }
        }
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        if self.reads_down.get() {
            return Err("drawOf: 503 from every node".into());
        }
        if self.dead {
            return Err("the process is gone".into());
        }
        let s = self.stale_draw_of.get();
        if s > 0 {
            self.stale_draw_of.set(s - 1);
            return self.inner.draw_of(f, attempt).map(|o| {
                o.map(|r| DrawRecord {
                    beacon: Hash32::ZERO,
                    ..r
                })
            });
        }
        self.inner.draw_of(f, attempt)
    }
    /// A pinned read: served by a node that has the block, or an error (a
    /// lagging node cannot answer for a block it lacks).
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        if self.reads_down.get() {
            return Err("drawOf: 503 from every node".into());
        }
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
    let d = std::env::temp_dir().join(format!("onyx-r6-{name}-{}", std::process::id()));
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

/// How many announced draws the saved book holds (`announced`).
fn announced_in_book(book: &Path) -> usize {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(book).expect("book")).expect("json");
    v["announced"].as_array().expect("announced").len()
}

/// R6-A (the reviewer's PoC): `announceDraw` lands but the send reports an
/// error (receipt timeout). The draw was forgotten; a stranger fixed draw 0
/// and the next run settled draw 1. Now `drawOf` shows the draw landed: the
/// run keeps it, saves the book and finishes on draw 0.
#[test]
fn r6_a_landed_announce_reported_as_error_voids_the_fact() {
    let d = dir("a");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::None);
    chain.announce_lands_err = true;
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(()))
        .expect("the landed draw is kept and run");
    assert_eq!(r.draw.attempt, 0);
    assert_eq!(chain.announces, 1, "never announced again");
    let fact = store().onchain_fact(&r.spec).expect("fact");
    assert_eq!(chain.inner.draw_count(&fact), Ok(1));
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// R6-A, the send's error and no read: the process dies right after the
/// timed-out send. The book is saved holding the draw before anything else,
/// so the restart resumes it even after a stranger fixed its beacon.
#[test]
fn r6_a_an_unconfirmed_announce_stays_in_the_book() {
    let d = dir("a-dead");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::None);
    chain.announce_lands_err = true;
    chain.reads_down.set(true);
    let e =
        run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("unconfirmed");
    assert!(e.error.contains("stays in the book"), "{}", e.error);
    assert_eq!(announced_in_book(&book), 1, "draw 0 kept");
    let d0 = chain.last.expect("announced");
    chain
        .inner
        .fix_beacon(&d0)
        .expect("a stranger fixes draw 0");
    chain.restart();
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("run 2");
    assert_eq!((r.draw.attempt, chain.announces), (0, 1));
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// THREATS REVLOG-R5-2: the process dies after `announceDraw` landed and
/// BEFORE the book was first saved (the book on disk is the one before the
/// run: none). A stranger fixes draw 0. The restart adopts the chain's draw
/// 0 (its commitment is the one this runner announces) instead of drawing 1.
#[test]
fn r5_2_a_crash_before_the_first_book_save_adopts_the_chains_draw() {
    let d = dir("r5-2");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::CrashBeforeFix);
    let _ = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("crash");
    std::fs::remove_file(&book).expect("the first save never happened");
    let d0 = chain.last.expect("announced");
    chain
        .inner
        .fix_beacon(&d0)
        .expect("a stranger fixes draw 0");
    chain.restart();
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("run 2");
    assert_eq!((r.draw.attempt, chain.announces), (0, 1), "draw 0 adopted");
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// R6-B (the reviewer's PoC): the book holds announced draw 0 (crash before
/// `fixBeacon`); the restart reads ONE stale `drawCount` (0). The draw was
/// forgotten, the re-announcement reverted `BadAttempt`, the book was saved
/// without it, a stranger fixed draw 0, the next run settled draw 1. Now
/// `drawOf` confirms the draw and the restart resumes it.
#[test]
fn r6_b_one_stale_draw_count_on_resume_forgets_the_draw() {
    let d = dir("b");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::CrashBeforeFix);
    let _ = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("crash");
    let d0 = chain.last.expect("announced");
    chain.restart();
    chain.stale_count.set(1);
    chain
        .inner
        .fix_beacon(&d0)
        .expect("a stranger fixes draw 0");
    let r =
        run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("draw 0 resumed");
    assert_eq!((r.draw.attempt, chain.announces), (0, 1));
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// R6-B: a stale `drawCount` and no `drawOf` answer at all: the run stops
/// ("the chain is behind the book") and the saved book still holds draw 0;
/// the next run resumes it.
#[test]
fn r6_b_a_stale_count_without_draw_of_keeps_the_draw() {
    let d = dir("b-down");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::CrashBeforeFix);
    let _ = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("crash");
    chain.restart();
    chain.stale_count.set(1);
    chain.reads_down.set(true);
    let e = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("behind");
    assert!(e.error.contains("behind the book"), "{}", e.error);
    assert_eq!(announced_in_book(&book), 1, "draw 0 never dropped");
    assert_eq!(chain.announces, 1);
    chain.restart();
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("run 3");
    assert_eq!((r.draw.attempt, chain.announces), (0, 1));
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// R6-D (the reviewer's PoC): the coordinator's own `fixBeacon` lands but
/// reports an error; the latest `drawOf` comes from a lagging node (beacon
/// zero) while the height is past the blockhash window. The draw was given
/// up though fixed, and the next run settled draw 1. Now the check reads
/// `drawOf` at one final block, which shows the beacon fixed: the draw stays
/// announced and the next run opens it.
#[test]
fn r6_d_stale_draw_of_past_the_window_gives_up_a_fixed_draw() {
    let d = dir("d");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Faulty::new(Fault::FixedThenStalePastWindow);
    let e = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect_err("run1");
    assert!(
        e.error.contains("stays announced") && !e.error.contains("given up"),
        "{}",
        e.error
    );
    chain.restart();
    let r = run_once(&mut chain, &book, &results, &pages(), &mut |_| Ok(())).expect("run 2");
    assert_eq!((r.draw.attempt, chain.announces), (0, 1));
    client_accepts(&mut chain, &results, &r);
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
