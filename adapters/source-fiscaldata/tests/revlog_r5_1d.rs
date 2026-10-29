//! THREATS REVLOG-R5-1d (ROADMAP B5): a coordinator restarted while a
//! round's reveal window is still open keeps taking reveals. Before, a run
//! that found the round kept in its book went straight to `finish_with`: the
//! reveals the crash lost (taken in memory, not yet saved) were never taken
//! again although the window was open by the coordinator's clock, so those
//! observers counted as non-reveals.
//!
//! Each test drives the real runner (`run_and_save`) on a simulated
//! registry. The crash is the state the book holds after it: the round with
//! the reveals saved so far (the intake saves after each one, before any
//! receipt, so a book with two of five reveals and no log snapshot is what
//! a crash between the second and third save leaves). The observers keep
//! their signed reveals and send them again; the restarted run reads them
//! from its `RevealInbox`.

use onyx_api::client::{verify_attempts_onchain, verify_observation, ObservationExpect};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::observe::{
    write_atomic, ChainView, ChainWait, Clock, DrawChain, DrawRecord, LogBlock, NoInbox,
    ObserverSig, OnchainDraw, OnchainFact, Posted, PostedCommit, ReplayClock, RevealInbox,
    SignedReveal, SimulatedRegistry, TranscriptLogQuery,
};
use onyx_core::{Hash32, MinerAddress};
use onyx_source_fiscaldata::round::{
    append_results, run_and_save, store, Page, RoundRun, RunError,
};
use onyx_source_fiscaldata::{recorded, spec, subject, template, RECORDED_WINDOW_START};
use std::path::{Path, PathBuf};

/// A simulated registry whose finalized-block read can be cut (the node
/// down, or the process stopping right after the intake).
struct Chain {
    inner: SimulatedRegistry,
    down: bool,
}

impl ChainView for Chain {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for Chain {
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
        if self.down {
            return Err("connection refused".into());
        }
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
    let d = std::env::temp_dir().join(format!("onyx-r5-1d-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// One run from the book, as the `level2-round` binary does it, with the
/// coordinator's clock and reveal inbox.
fn run_once(
    chain: &mut Chain,
    book: &Path,
    results: &Path,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
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
        &pages(),
        &mut s,
        chain,
        book,
        written.as_ref(),
        &ChainWait::NOW,
        clock,
        inbox,
        &mut |r| {
            let prev: Option<serde_json::Value> = std::fs::read_to_string(results)
                .ok()
                .map(|t| serde_json::from_str(&t).expect("json"));
            write_atomic(results, &format!("{}\n", append_results(prev.as_ref(), r)?))
        },
        &mut |_| panic!("no round fails here"),
    )
}

/// The crash: the first run takes every in-process reveal and stops before
/// the log snapshot (the node is down), then the book is cut back to the
/// first `kept` reveals, as a crash between two reveal saves leaves it.
/// Returns the reveals the crash lost (the observers still hold them) and
/// the round's window.
fn crash_mid_intake(
    chain: &mut Chain,
    book: &Path,
    results: &Path,
    kept: usize,
) -> (Vec<SignedReveal>, u64, u64) {
    chain.down = true;
    let e = run_once(chain, book, results, &ReplayClock::default(), &mut NoInbox)
        .expect_err("stopped before the log snapshot");
    assert!(e.unfinished.is_some(), "kept unfinished: {e}");
    chain.down = false;
    let sp = spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec");
    let mut s = store();
    s.load_book(book).expect("loads");
    let mut u = s.kept_round(&sp).expect("the book keeps the round");
    assert!(u.round.log_snapshot().is_none(), "no receipt was signed");
    assert_eq!(
        u.reveals.len(),
        pages().len(),
        "every reveal taken before the stop"
    );
    let lost: Vec<SignedReveal> = u
        .reveals
        .split_off(kept)
        .into_iter()
        .map(|a| a.reveal)
        .collect();
    s.keep_unfinished(u.clone()).expect("kept");
    s.save_book(book).expect("saved");
    let cfg = u.round.config();
    (lost, cfg.commit_close, cfg.reveal_close)
}

/// The client's full check of the fact from the results store: off chain,
/// then `verify_attempts_onchain` against the chain.
fn client_accepts(chain: &mut Chain, results: &Path, value: &RoundRun) {
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
    let word = |v: u64| {
        let mut w = vec![0u8; 32];
        w[24..].copy_from_slice(&v.to_be_bytes());
        w
    };
    let count = word(u64::from(chain.inner.draw_count(&fact).expect("count")));
    let (strikes, allowance) = chain.inner.draw_budget(&fact).expect("budget");
    let mut budget = word(u64::from(strikes));
    budget.extend(word(u64::from(allowance)));
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    verify_attempts_onchain(
        &a,
        &count,
        &[],
        &budget,
        &vec![yes; a.attempts.len()],
        &unsealed_all(&a),
    )
    .expect("the client accepts the fact");
}

/// The fix: restarted inside the window, the run takes the reveals the
/// observers send again (a replay of one taken before the crash keeps the
/// first), and the round settles with every observer revealed. Before the
/// fix the restarted run finished with the two saved reveals.
#[test]
fn r5_1d_a_restart_inside_the_reveal_window_keeps_taking_reveals() {
    let d = dir("inside");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Chain {
        inner: SimulatedRegistry::new(),
        down: false,
    };
    let (lost, close, _) = crash_mid_intake(&mut chain, &book, &results, 2);
    assert_eq!(lost.len(), pages().len() - 2);
    let mut resent = lost.clone();
    let sp = spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec");
    let mut s = store();
    s.load_book(&book).expect("loads");
    let first = s.kept_round(&sp).expect("kept").reveals[0].clone();
    resent.insert(0, first.reveal.clone()); // a replay after the restart
    let r = run_once(
        &mut chain,
        &book,
        &results,
        &ReplayClock::at(close + 30),
        &mut resent,
    )
    .expect("the restarted run settles");
    assert_eq!(
        r.reveals.len(),
        pages().len(),
        "no reveal lost to the restart"
    );
    r.value().expect("a value");
    client_accepts(&mut chain, &results, &r);
    let _ = std::fs::remove_dir_all(&d);
}

/// Restarted after `reveal_close`: what the observers send now is late (a
/// non-reveal), the run finishes with the saved reveals and does not wait.
#[test]
fn r5_1d_a_restart_after_the_reveal_window_takes_nothing_late() {
    let d = dir("after");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Chain {
        inner: SimulatedRegistry::new(),
        down: false,
    };
    let (mut lost, _, end) = crash_mid_intake(&mut chain, &book, &results, 3);
    let clock = ReplayClock::at(end + 1);
    let r = run_once(&mut chain, &book, &results, &clock, &mut lost).expect("settles");
    assert_eq!(r.reveals.len(), 3, "late reveals are non-reveals");
    assert_eq!(clock.now(), end + 1, "no wait after the window");
    let _ = std::fs::remove_dir_all(&d);
}

/// A second stop during the resumed intake loses nothing taken: each
/// reveal taken after the restart is saved with the book before the next,
/// so the third run (after the window) finishes with all of them.
#[test]
fn r5_1d_reveals_taken_after_a_restart_survive_the_next_one() {
    let d = dir("twice");
    let (book, results) = (d.join("book.json"), d.join("results.json"));
    let mut chain = Chain {
        inner: SimulatedRegistry::new(),
        down: false,
    };
    let (lost, close, end) = crash_mid_intake(&mut chain, &book, &results, 1);
    chain.down = true;
    let mut some = lost[..2].to_vec();
    run_once(
        &mut chain,
        &book,
        &results,
        &ReplayClock::at(close + 5),
        &mut some,
    )
    .expect_err("stopped again before the log snapshot");
    chain.down = false;
    let r = run_once(
        &mut chain,
        &book,
        &results,
        &ReplayClock::at(end + 1),
        &mut NoInbox,
    )
    .expect("settles");
    assert_eq!(
        r.reveals.len(),
        3,
        "the one before and the two after the restart"
    );
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
