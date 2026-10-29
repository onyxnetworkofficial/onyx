//! Level-2 round runner (feature `round`): several observers fetch the same
//! company filing data, commit, reveal, the round settles in an
//! [`ObservationResults`] store, and the one claim it backs goes through
//! `accept_claim`.
//!
//! This is a rehearsal on one machine: the observers are simulated (fixed
//! addresses and salts derived from their numbers, so the run is reproducible),
//! but each one's page bytes and fetch time are a real HTTPS response (live
//! or recorded).

use crate::{registry, rights, spec, template, Company, EdgarFetcher, COMPANIES};
use onyx_api::observations::Attempt;
use onyx_coordinator::keyfile::ReceiptKey;
use onyx_coordinator::observe::{
    absorb_posted_commits, cfg_hash, check_commit_allowed, draw_and_open, draw_commit,
    finish_reveals, keep_taking_reveals, random_salt, receive_reveals, sign_commit, sign_reveal,
    test_keys, ChainWait, Clock, DrawChain, DrawRecord, DrawSeed, FeedAdmin, NoInbox,
    ObservationOutcome, ObservationResults, ObservationRound, ObservationSettlement, ObserveConfig,
    Observer, ObserverSig, ObserverStatus, ReplayClock, RevealInbox, SignedReveal, UnfinishedRound,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    observation_commit, observe, Answer, Claim, Hash32, Observation, ObservationSpec, ProofLevel,
    Snapshot, SourceRef, Value, VerificationMethod,
};
use serde_json::{json, Value as Json};

/// One observer's page: when it fetched, and the body (`None`: no answer).
#[derive(Clone, Debug)]
pub struct Page {
    /// Unix seconds of the fetch (must fall inside the feed's window).
    pub fetched_unix: u64,
    /// Response body, or `None` if the request failed.
    pub body: Option<Vec<u8>>,
}

/// Everything a round produced.
#[derive(Debug)]
pub struct RoundRun {
    /// The feed.
    pub spec: ObservationSpec,
    /// Timing and economics.
    pub cfg: ObserveConfig,
    /// The simulated bonded pool snapshot the observers were drawn from.
    pub pool: Vec<Observer>,
    /// The draw's public inputs (L2-F2).
    pub draw: DrawSeed,
    /// The draw as the registry recorded it (L2RND-R1).
    pub onchain_draw: DrawRecord,
    /// Drawn observers, ascending by address.
    pub observers: Vec<Observer>,
    /// The round id commits are bound to.
    pub round_id: Hash32,
    /// Every commit, ascending by observer: observer, commit, the observer's
    /// signature (ATT-R1-F3).
    pub commits: Vec<([u8; 20], Hash32, ObserverSig)>,
    /// Every reveal, ascending by observer: observer, observation, salt, the
    /// observer's signature.
    pub reveals: Vec<([u8; 20], Observation, Hash32, ObserverSig)>,
    /// The settlement recorded in the store.
    pub settlement: ObservationSettlement,
    /// The claim the settlement backs, accepted at level 2; `None` when the
    /// round settled no value (ATT-R1-F1: the run is returned whatever the
    /// outcome, so its round is published as an attempt).
    pub claim: Option<Claim>,
}

impl RoundRun {
    /// The level-2 claim, or why there is none (the round's outcome).
    pub fn value(&self) -> Result<&Claim, String> {
        self.claim
            .as_ref()
            .ok_or_else(|| format!("round settled no value: {:?}", self.settlement.outcome()))
    }
}

/// Why a run returned no [`RoundRun`]. `attempt` is the opened round when
/// the run failed after the draw's beacon was fixed on chain and before any
/// reveal (ATT-R2-F3): publish it ([`append_attempt`]) so clients account
/// for the draw and the fact's re-draw still verifies (`run_and_save` does,
/// through `publish_failed`, before the book drops it: THREATS
/// REVLOG-R4-5); `None` when the beacon is not fixed yet (the draw stays
/// announced in the store and the book, and the next run resumes it, review
/// REVLOG-R5), or when the round is `unfinished`.
#[derive(Debug)]
pub struct RunError {
    /// What failed.
    pub error: String,
    /// The opened round, to publish as an attempt.
    pub attempt: Option<Box<Attempt>>,
    /// A round that must not be published as a failed attempt (review
    /// REVLOG-R2): the chain could not be read or written, or its commit
    /// phase was not over on chain yet, or reveals were taken (their
    /// receipts may be on chain). Keep it and [`finish`] it on the same
    /// draw once the chain answers; that never burns a draw.
    pub unfinished: Option<Box<Unfinished>>,
}

/// An opened round the run could not finish (reviews REVLOG-R2,
/// REVLOG-R3-2): the round (with its log snapshot once read), the draw the
/// registry recorded and the observers' signed reveals. The store keeps it
/// and saves it with the round book; the next run finishes it on the same
/// draw.
pub use onyx_coordinator::observe::UnfinishedRound as Unfinished;

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error)
    }
}

impl std::error::Error for RunError {}

impl From<String> for RunError {
    fn from(error: String) -> Self {
        RunError {
            error,
            attempt: None,
            unfinished: None,
        }
    }
}

impl From<RunError> for String {
    fn from(e: RunError) -> Self {
        e.error
    }
}

/// A store with every company's template registered (L2SRC-R2-F1). In the
/// rehearsal the adapter registers them itself, as the reviewer's admin path
/// would after the source review (fixtures/PROVENANCE.md); the capability is
/// dropped, so nothing else can be registered in this store.
pub fn store() -> ObservationResults {
    store_with_admin().0
}

/// [`store`] and its admin capability, for reviewer actions such as
/// reopening a fact closed after the cap of rounds without a value
/// (SEC-R2-F1).
pub fn store_with_admin() -> (ObservationResults, FeedAdmin) {
    let (mut store, admin) = ObservationResults::new();
    for c in &COMPANIES {
        store
            .register_feed(
                &admin,
                template(c),
                "SEC EDGAR companyconcept: rights reviewed (fixtures/PROVENANCE.md), feed pinned",
                0,
            )
            .expect("the adapter's templates are valid");
    }
    (store, admin)
}

/// Read the round book at `path` into `store`, if the file exists
/// (AUD-2-F9): [`ObservationResults::load_book`].
pub fn load_book(store: &mut ObservationResults, path: &str) -> Result<(), String> {
    store.load_book(std::path::Path::new(path))
}

/// Write `store`'s round book to `path` atomically (AUD-2-F9, AUD-2R-F4):
/// [`ObservationResults::save_book`].
pub fn save_book(store: &ObservationResults, path: &str) -> Result<(), String> {
    store.save_book(std::path::Path::new(path))
}

/// [`run_clocked`] one round, saving the book once the draw is announced
/// (before its beacon exists, L2RND-R1-F3), with each reveal as it arrives
/// and with the log snapshot (before any receipt, reviews REVLOG-R3-1 and
/// REVLOG-R4-3), and after a round that did not settle (AUD-2R-F4). A
/// settled round (a value or not: a round without a value counts toward the
/// cap) is handed to `publish` (write the results store, durably) and only
/// then dropped from the book and the book saved (review REVLOG-R4-1):
/// until then the book on disk holds the round unsettled, so a crash before
/// the results are written makes the next run with this book publish the
/// same round on the same draw (and count it once), never announce a new
/// one. `publish` may run again for a round it already wrote (a crash after
/// the write): [`append_results`] adds nothing twice. A failed attempt goes to `publish_failed` the same way
/// (THREATS REVLOG-R4-5): saved in the book first, written, then dropped.
/// `results` is the results store as written before this run (`None`: none
/// written yet): its draws of the fact are noted first ([`note_results`],
/// review R9-F1), so a lost book never adopts a chain draw whose round is
/// already published.
/// `inbox` (THREATS REVLOG-R5-1d): reveals observers send while the reveal
/// window is open by `clock`, read after the in-process ones and, when the
/// run resumes a round kept in the book, before it is finished
/// ([`keep_taking_reveals`]); a restart inside the window keeps taking
/// reveals. [`NoInbox`] when nothing sends any.
#[allow(clippy::too_many_arguments)]
pub fn run_and_save(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    book: &str,
    results: Option<&Json>,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    publish: &mut dyn FnMut(&RoundRun) -> Result<(), String>,
    publish_failed: &mut dyn FnMut(&Attempt) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    run_and_save_with(
        c,
        period_end,
        window_start,
        pages,
        store,
        chain,
        book,
        results,
        wait,
        clock,
        inbox,
        &ReceiptKey::public_test(),
        publish,
        publish_failed,
    )
}

/// [`run_and_save`] signing receipts with the deployment's `receipt_key`
/// (ROADMAP B3: the public test key only in local rehearsals,
/// [`ReceiptKey::for_chain`]).
#[allow(clippy::too_many_arguments)]
pub fn run_and_save_with(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    book: &str,
    results: Option<&Json>,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    receipt_key: &ReceiptKey,
    publish: &mut dyn FnMut(&RoundRun) -> Result<(), String>,
    publish_failed: &mut dyn FnMut(&Attempt) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    let sp = spec(c, period_end, window_start)?;
    let onchain = chain.deployment()?;
    note_results(c, store, &sp, results, onchain)?;
    let r = match run_with(
        c,
        period_end,
        window_start,
        pages,
        store,
        chain,
        wait,
        clock,
        inbox,
        receipt_key.secret(),
        &mut |s| save_book(s, book),
    ) {
        Ok(r) => r,
        Err(mut e) => {
            // THREATS REVLOG-R4-5: the book is saved first, holding a failed
            // round (with its commits) as a failed attempt to publish; it
            // is written (`publish_failed`) and only then dropped from the
            // book. A crash in between publishes it again (idempotent).
            if let Err(se) = save_book(store, book) {
                e.error = format!("{}; the book was not saved: {se}", e.error);
                return Err(e);
            }
            if let Some(a) = e.attempt.as_deref() {
                match publish_failed(a) {
                    Ok(()) => {
                        drop_failed(store, &sp);
                        if let Err(se) = save_book(store, book) {
                            e.error = format!("{}; the book was not saved after the failed round was written: {se}", e.error);
                        }
                    }
                    Err(pe) => {
                        e.error = format!("{}; writing the failed round: {pe}; the book keeps it: run again with the same book to publish it", e.error);
                    }
                }
            }
            return Err(e);
        }
    };
    if let Err(e) = publish(&r) {
        return Err(RunError {
            error: format!(
                "writing the settled round's results: {e}; the book keeps the round: run again with the same book to publish it on the same draw"
            ),
            attempt: None,
            unfinished: store.unfinished(&r.spec).cloned().map(Box::new),
        });
    }
    store
        .published(&r.spec, &r.round_id)
        .map_err(|e| e.to_string())?;
    save_book(store, book)?;
    Ok(r)
}

/// Review R9-F1: note in `store` the draws of `sp`'s fact that the results
/// store `results` publishes (none without one,
/// [`ObservationResults::note_published_draws`]): a chain draw the book
/// lacks is never adopted when its round is already published. `onchain`
/// is the deployment the run announces on ([`DrawChain::deployment`]): a
/// results store naming another (or none, when `onchain` is `Some`) is
/// refused (review R10-1, R11-1).
pub fn note_results(
    c: &Company,
    store: &mut ObservationResults,
    sp: &ObservationSpec,
    results: Option<&Json>,
    onchain: Option<onyx_coordinator::observe::RegistryDeployment>,
) -> Result<(), String> {
    let draws = match results {
        Some(v) => onyx_api::observations::ObservationStore::published_draws(
            v,
            &template(c),
            &sp.subject,
            sp.window_start,
            onchain,
        )
        .map_err(|e| format!("the results store: {e}"))?,
        None => vec![],
    };
    // A fact whose value the results store publishes is settled: unless the
    // book still keeps that round (a crash after the write: the run
    // publishes it again, unchanged), no draw is announced or adopted for
    // it again (a book lost after the write).
    if let Some(v) = draws.iter().find(|d| d.value) {
        if !store
            .kept_round(sp)
            .is_some_and(|u| u.round.draw().attempt == v.draw.attempt)
        {
            return Err(format!(
                "the results store already publishes this fact's value (draw {}): the fact is settled, nothing to run",
                v.draw.attempt
            ));
        }
    }
    let seeds: Vec<_> = draws.iter().map(|d| d.draw).collect();
    store
        .note_published_draws(sp, &seeds)
        .map_err(|e| e.to_string())
}

/// Review R11-3: `--new-store` found no results store file for company
/// `c`: refused when the chain already shows draws of its fact and the
/// book `store` holds none
/// ([`onyx_coordinator::observe::new_store_allowed`]).
pub fn check_new_store(
    c: &Company,
    store: &ObservationResults,
    period_end: &str,
    window_start: u64,
    chain: &dyn DrawChain,
    wait: &ChainWait,
) -> Result<(), String> {
    let sp = spec(c, period_end, window_start)?;
    onyx_coordinator::observe::new_store_allowed(store, &sp, chain, wait)
}

/// Review R11-2: `--tag-store`: every company's results store
/// (`dir/<key>.json`) must exist and every draw each publishes match
/// `chain`; only then is each tagged with `chain`'s deployment
/// ([`onyx_api::observations::ObservationStore::tag_files`]). A missing
/// store or any mismatch tags none. Returns (key, draws published) per
/// company.
///
/// Review R12: `book` (the runner's book, locked by the caller) gives each
/// company's recorded fact and the draw it holds: the chain may show no
/// draw past both the store's and the book's.
pub fn tag_stores(
    dir: &std::path::Path,
    chain: &dyn DrawChain,
    book: &ObservationResults,
) -> Result<Vec<(&'static str, usize)>, String> {
    let recs = crate::recorded();
    let mut files = vec![];
    for c in &COMPANIES {
        let mut runner = vec![];
        for rec in recs.iter().filter(|r| r.company.key == c.key) {
            let sp = spec(c, &rec.period_end, rec.window_start)?;
            let fact = book.onchain_fact(&sp).map_err(|e| e.to_string())?;
            runner.push((fact, book.held_draw(&sp).map(|d| d.attempt)));
        }
        files.push((dir.join(format!("{}.json", c.key)), template(c), runner));
    }
    let n = onyx_api::observations::ObservationStore::tag_files(&files, chain)?;
    Ok(COMPANIES.iter().map(|c| c.key).zip(n).collect())
}

/// Company `key`'s recorded draw as the rehearsal's local test chain
/// (anvil) recorded it (`scripts/level2-rehearsal.sh`), for replaying the
/// recorded round without that chain (L2RND-R1).
pub fn recorded_draw(key: &str) -> DrawRecord {
    let text = match key {
        "mstr" => include_str!("../fixtures/mstr-2026-03-31/draw.json"),
        _ => include_str!("../fixtures/riot-2026-03-31/draw.json"),
    };
    let v = serde_json::from_str(text).expect("committed draw record parses");
    DrawRecord::from_json(&v).expect("committed draw record is well formed")
}

/// Rehearsal key of observer `i`: a FAKE test key, `keccak256("onyx test
/// observer key edgar rehearsal " ‖ i)` (`observe::test_keys`). Anyone can recompute
/// it; it holds nothing on any network. Each simulated observer signs its own
/// commit and reveal with it (ATT-R1-F3).
pub fn observer_key(i: u8) -> [u8; 32] {
    test_keys::secret("edgar rehearsal", u32::from(i))
}

/// Rehearsal address of observer `i` (its key's address).
pub fn observer_address(i: u8) -> [u8; 20] {
    test_keys::address("edgar rehearsal", u32::from(i))
}

/// The rehearsal key of pool member `a`.
fn key_of(a: &[u8; 20]) -> Option<[u8; 32]> {
    (1..=POOL_SIZE)
        .find(|i| observer_address(*i) == *a)
        .map(observer_key)
}

/// Size of the simulated bonded observer pool the rehearsal draws from
/// (larger than the draw, L2-F2).
pub const POOL_SIZE: u8 = 9;
/// The simulated pool: [`POOL_SIZE`] bonded observers, ascending by address.
pub fn rehearsal_pool() -> Vec<Observer> {
    let mut pool: Vec<Observer> = (1..=POOL_SIZE)
        .map(|i| Observer {
            address: observer_address(i),
            bond: 1_000,
        })
        .collect();
    pool.sort_by_key(|o| o.address);
    pool
}

/// Round timing for a feed: commits close 60 s after the window, reveals 120 s later.
pub fn config(spec: &ObservationSpec) -> ObserveConfig {
    let commit_close = spec.window_end + 60;
    ObserveConfig {
        commit_close,
        reveal_close: commit_close + 120,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

/// Run one round for company `c`'s period `period_end` over `pages` (one
/// per observer) and settle it in `store`, waiting on the chain per
/// [`ChainWait::DEFAULT`], on a [`ReplayClock`] (the recorded pages'
/// timeline: each observer reveals right after the commit phase).
pub fn run(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
) -> Result<RoundRun, RunError> {
    run_waiting(
        c,
        period_end,
        window_start,
        pages,
        store,
        chain,
        &ChainWait::DEFAULT,
    )
}

/// [`run`], waiting on the chain per `wait`.
pub fn run_waiting(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    run_clocked(
        c,
        period_end,
        window_start,
        pages,
        store,
        chain,
        wait,
        &ReplayClock::default(),
    )
}

/// [`run`], waiting on the chain per `wait`, each reveal stamped with
/// `clock` as it arrives (review REVLOG-R4-3: a live coordinator passes
/// [`onyx_coordinator::observe::SystemClock`]). Without a book: the settled
/// round is the caller's to publish, and is not kept in `store`. Not for a
/// runner that keeps a book (review R9-F3): the store releases the round's
/// draw as published before anything is written, skipping the guard
/// [`run_and_save`] keeps (write first, then release); replays and tests
/// only.
#[allow(clippy::too_many_arguments)]
pub fn run_clocked(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
) -> Result<RoundRun, RunError> {
    let r = run_with(
        c,
        period_end,
        window_start,
        pages,
        store,
        chain,
        wait,
        clock,
        &mut NoInbox,
        &test_keys::receipt_secret(),
        &mut |_| Ok(()),
    )
    .inspect_err(|e| {
        // Without a book the caller publishes the failed attempt it gets.
        if e.attempt.is_some() {
            if let Ok(sp) = spec(c, period_end, window_start) {
                drop_failed(store, &sp);
            }
        }
    })?;
    store
        .published(&r.spec, &r.round_id)
        .map_err(|e| e.to_string())?;
    Ok(r)
}

/// Drop the kept failed attempt of `spec`'s fact once it is published
/// (THREATS REVLOG-R4-5).
fn drop_failed(store: &mut ObservationResults, spec: &ObservationSpec) {
    if let Some(id) = store.failed_attempt(spec).map(|u| u.round.round_id()) {
        store.attempt_published(spec, &id);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_with(
    c: &Company,
    period_end: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    receipt_key: &[u8; 32],
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    let spec = spec(c, period_end, window_start)?;
    // THREATS REVLOG-R4-5: a failed attempt the book keeps (its write did
    // not happen, or the run stopped before the book dropped it) is
    // published first; its draw gets no successor until then.
    if let Some(u) = store.failed_attempt(&spec) {
        let a = Attempt::of_failed_round(&template(c), &u.round)?;
        return Err(RunError {
            error: format!(
                "draw {} failed before its reveals and is not published yet: it is published now; run again for the next draw",
                u.round.draw().attempt
            ),
            attempt: Some(Box::new(a)),
            unfinished: None,
        });
    }
    // Review REVLOG-R3-2: a fact whose round is unfinished (kept in the
    // store, saved with the book) finishes that round on its draw; it never
    // gets a new draw. Review REVLOG-R4-1: that includes a round settled
    // before its results were written: it is published again.
    // THREATS REVLOG-R5-1d: a round kept by a run that stopped inside its
    // reveal window keeps taking reveals until the window closes (by this
    // run's clock); observers send again what the stop lost.
    if let Some(mut u) = store.kept_round(&spec) {
        if let Err(error) = keep_taking_reveals(store, &mut u, inbox, clock, save) {
            return Err(unfinished(store, u, error));
        }
        return finish_with(c, period_end, store, u, chain, wait, receipt_key, save);
    }
    let cfg = config(&spec);
    // The observers are drawn (L2-F2, L2RND-R1): the draw is announced on
    // chain before its beacon height, the book saved, the beacon fixed from
    // the chain's block hash, and the round opened with it.
    let (mut round, onchain_draw) = draw_and_open(
        store,
        spec.clone(),
        cfg,
        &rehearsal_pool(),
        chain,
        wait,
        save,
    )?;
    // ATT-R2-F3: from here the draw's beacon is fixed on chain, so a run
    // that fails still returns its opened round: published as an attempt
    // when it failed before any reveal, otherwise unfinished.
    match commit_phase(
        pages,
        store,
        &spec,
        cfg,
        &mut round,
        &onchain_draw,
        chain,
        wait,
    ) {
        Ok(reveals) => {
            // Review REVLOG-R4-3: the reveals are taken as they arrive, by
            // the coordinator's clock, and saved before any receipt; the
            // receipts wait for the finalized log snapshot.
            let mut u = Unfinished {
                round,
                onchain_draw,
                reveals: vec![],
            };
            // REVLOG-R5-1d: then the inbox, while the window is open and
            // a drawn observer has not revealed.
            match receive_reveals(store, &mut u, reveals, clock, save)
                .and_then(|()| keep_taking_reveals(store, &mut u, inbox, clock, save))
            {
                Ok(_) => finish_with(c, period_end, store, u, chain, wait, receipt_key, save),
                Err(error) => Err(unfinished(store, u, error)),
            }
        }
        Err(error) => Err(failed_before_reveals(
            c,
            store,
            round,
            onchain_draw,
            chain,
            wait,
            error,
        )),
    }
}

/// A run that failed in the commit phase. Its round is published as a
/// failed attempt with the commits posted in its phase (review REVLOG
/// HIGH-1), once the chain's confirmed time is past commit close and the log
/// was read; if the chain cannot be read (review REVLOG-R2 HIGH-1) or the
/// phase is not over, it is unfinished, never published without them.
fn failed_before_reveals(
    c: &Company,
    store: &mut ObservationResults,
    mut round: ObservationRound,
    onchain_draw: DrawRecord,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    error: String,
) -> RunError {
    let absorbed = store
        .onchain_fact(round.spec())
        .map_err(|e| e.to_string())
        .and_then(|f| absorb_posted_commits(&mut round, &*chain, &f, wait))
        .and_then(|_| Attempt::of_failed_round(&template(c), &round));
    match absorbed {
        Ok(a) => {
            // THREATS REVLOG-R4-5: kept (and saved with the book) until it
            // is written, so a crash first never loses the draw's round.
            let kept = store.keep_failed(Unfinished {
                round,
                onchain_draw,
                reveals: vec![],
            });
            RunError {
                error: match kept {
                    Ok(()) => error,
                    Err(k) => format!("{error}; the failed round is not kept in the book: {k}"),
                },
                attempt: Some(Box::new(a)),
                unfinished: None,
            }
        }
        Err(e) => {
            let u = Unfinished {
                round,
                onchain_draw,
                reveals: vec![],
            };
            // Review REVLOG-R3-2: kept in the store, saved with the book.
            let kept = store.keep_unfinished(u.clone());
            RunError {
                error: format!("{error}; the round is unfinished ({e}){}: finish it on the same draw, never publish it as a failed round", kept.err().map(|k| format!("; not kept: {k}")).unwrap_or_default()),
                attempt: None,
                unfinished: Some(Box::new(u)),
            }
        }
    }
}

/// The opened round's commit phase: each simulated observer fetches,
/// commits and posts its commit on chain; returns their signed reveals.
#[allow(clippy::too_many_arguments)]
fn commit_phase(
    pages: &[Page],
    store: &ObservationResults,
    spec: &ObservationSpec,
    cfg: ObserveConfig,
    round: &mut ObservationRound,
    onchain_draw: &DrawRecord,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<Vec<SignedReveal>, String> {
    let fact = store.onchain_fact(spec).map_err(|e| e.to_string())?;
    let observers = round.observers().to_vec();
    if observers.len() != pages.len() {
        return Err(format!(
            "{} pages for {} drawn observers",
            pages.len(),
            observers.len()
        ));
    }
    let round_id = round.round_id();
    // ATT-R1-F4: each observer commits only once the registry shows this
    // draw with its beacon fixed (in this rehearsal the observers read the
    // record the chain returned; a real observer reads `drawOf` itself).
    let seed = round.draw();
    check_commit_allowed(
        &seed,
        &draw_commit(&seed.pool, &spec.id(), &cfg_hash(&cfg)),
        onchain_draw,
    )
    .map_err(|e| format!("observers refuse to commit: {e}"))?;
    let mut reveals = Vec::new();
    for ((i, page), drawn) in (1..=u8::MAX).zip(pages).zip(&observers) {
        let a = drawn.address;
        let o = observe(&EdgarFetcher, spec, page.body.as_deref(), page.fetched_unix)
            .map_err(|e| format!("observer {i}: {e}"))?;
        // Each observer draws its own random salt and keeps it (ATT-R2-F2).
        let s = random_salt().map_err(|e| format!("observer {i} salt: {e}"))?;
        // Each simulated observer signs with its own (fake) key.
        let sk = key_of(&a).ok_or_else(|| format!("observer {i}: not a pool member"))?;
        let h = observation_commit(&round_id, &a, &o, &s);
        let sig = sign_commit(&sk, &round_id, seed.attempt, &h)
            .ok_or_else(|| format!("observer {i}: bad key"))?;
        round
            .commit(page.fetched_unix, a, h, sig)
            .map_err(|e| format!("observer {i} commit: {e}"))?;
        // ADR-0027 amendment 8: each observer posts its commit on chain too.
        // A slot someone filled first is no error (review REVLOG HIGH-1):
        // what it holds is absorbed before any reveal.
        wait.retry(|| chain.post_commit(&fact, seed.attempt, &a, &round_id, &h, &sig))
            .map_err(|e| format!("observer {i} postCommit: {e}"))?;
        let rs = sign_reveal(&sk, &round_id, seed.attempt, &o, &s)
            .ok_or_else(|| format!("observer {i}: bad key"))?;
        reveals.push((a, o, s, rs));
    }
    Ok(reveals)
}

/// Finish an opened round (reviews REVLOG-R2, REVLOG-R3): read the log
/// once (or reuse the round's snapshot), take the reveals with their
/// receipts, check `transcriptComplete`, settle and accept the claim. Any
/// error keeps the round [`unfinished`](RunError::unfinished), in the store
/// too, to finish again later on the same draw; it is never published as a
/// failed round. Idempotent after settling (review REVLOG-R3-3). Without a
/// book, like [`run_clocked`]: the settled round is not kept in `store`
/// (replays and tests only, review R9-F3).
pub fn finish(
    c: &Company,
    period_end: &str,
    store: &mut ObservationResults,
    u: Unfinished,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    let r = finish_with(
        c,
        period_end,
        store,
        u,
        chain,
        wait,
        &test_keys::receipt_secret(),
        &mut |_| Ok(()),
    )?;
    store
        .published(&r.spec, &r.round_id)
        .map_err(|e| e.to_string())?;
    Ok(r)
}

/// `u` could not finish because of `error`: keep it in the store (saved
/// with the book) and return it unfinished, to finish on the same draw.
fn unfinished(store: &mut ObservationResults, u: Unfinished, error: String) -> RunError {
    let kept = match store.keep_unfinished(u.clone()) {
        Ok(()) => String::new(),
        Err(e) => format!(" (not kept in the store: {e})"),
    };
    RunError {
        error: format!("{error}: the round is unfinished{kept}; finish it on the same draw"),
        attempt: None,
        unfinished: Some(Box::new(u)),
    }
}

/// [`finish`], with `save` called once the log snapshot is kept in the
/// store, before the first receipt is signed (review REVLOG-R3-1).
#[allow(clippy::too_many_arguments)]
fn finish_with(
    c: &Company,
    period_end: &str,
    store: &mut ObservationResults,
    mut u: Unfinished,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    receipt_key: &[u8; 32],
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    // The caller's round replaces the copy the store keeps, once it is kept
    // again below; until then the book keeps the copy (review R8: a round
    // leaves the book only once published).
    let draw = u.onchain_draw;
    let reveals = u.reveals.clone();
    let done = store
        .onchain_fact(u.round.spec())
        .map_err(|e| e.to_string())
        .and_then(|fact| {
            // Review REVLOG-R4-3: each reveal is checked at its arrival time
            // (`u.reveals`), not at the receipt's.
            finish_reveals(
                &mut u.round,
                chain,
                &fact,
                receipt_key,
                &reveals,
                wait,
                &mut |r| {
                    store
                        .keep_unfinished(UnfinishedRound {
                            round: r.clone(),
                            onchain_draw: draw,
                            reveals: reveals.clone(),
                        })
                        .map_err(|e| e.to_string())?;
                    save(store)
                },
            )
        })
        .and_then(|()| record(c, period_end, store, &u.round, u.onchain_draw));
    match done {
        // Review REVLOG-R4-1: the settled round stays kept until its
        // results are written (`ObservationResults::published`).
        Ok(run) => match store.keep_unfinished(u.clone()) {
            Ok(()) => Ok(run),
            Err(e) => Err(unfinished(
                store,
                u,
                format!("keeping the settled round: {e}"),
            )),
        },
        Err(error) => Err(unfinished(store, u, error)),
    }
}

/// Settle a round whose transcript is complete on chain and accept its
/// claim.
fn record(
    c: &Company,
    period_end: &str,
    store: &mut ObservationResults,
    round: &ObservationRound,
    onchain_draw: DrawRecord,
) -> Result<RoundRun, String> {
    let spec = round.spec().clone();
    let cfg = round.config();
    let settlement = store
        .settle_once(round, cfg.reveal_close + 1)
        .map_err(|e| format!("settle: {e}"))?;
    let mut run = RoundRun {
        spec: spec.clone(),
        cfg,
        pool: round.pool().to_vec(),
        draw: round.draw(),
        onchain_draw,
        observers: round.observers().to_vec(),
        round_id: round.round_id(),
        commits: round.signed_commits(),
        reveals: round.signed_reveals(),
        settlement,
        claim: None,
    };
    // ATT-R1-F1: a round without a value is returned too, so its round is
    // published (`append_results`) and a client sees every draw.
    let ObservationOutcome::Value(Answer::Number(mantissa)) = run.settlement.outcome().clone()
    else {
        return Ok(run);
    };
    let settlement = &run.settlement;
    let claim = Claim {
        metric: spec.metric.clone(),
        subject: spec.subject.clone(),
        // A numeric feed at scale 0 is claimed as an integer (observe.rs).
        value: Value::Int(mantissa),
        source: SourceRef {
            source: spec.rights.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: spec.window_end,
                content_hash: settlement.result_hash(),
            },
        },
        method: VerificationMethod::ReObserved {
            spec: spec.id(),
            result: settlement.result_hash(),
        },
        label_set: None,
        rights: rights(),
        evidence: vec![],
        confidence: None,
    };
    let level = store
        .accept_claim(&claim, &registry(c, period_end))
        .map_err(|e| format!("accept_claim: {e}"))?;
    if level != ProofLevel::ReObserved {
        return Err(format!("accepted at {level:?}, not level 2"));
    }
    run.claim = Some(claim);
    Ok(run)
}

/// The run's public round without its answer: what a round that settled no
/// value is published as (SEC-R2-F1): every commit the round holds (one
/// posted on chain in the commit phase replaces the observer's own, review
/// REVLOG HIGH-1) and every reveal it took.
pub fn attempt(run: &RoundRun) -> onyx_api::observations::Attempt {
    onyx_api::observations::Attempt {
        subject: run.spec.subject.clone(),
        window_start: run.spec.window_start,
        cfg: run.cfg,
        pool: run.pool.clone(),
        draw: run.draw,
        observers: run.observers.clone(),
        // Ascending by observer address, as `ObservationRound::commit` keeps
        // them and every check requires (L2API-R2-F3).
        commits: run.commits.clone(),
        reveals: run.reveals.clone(),
        // The EDGAR runner opens no sealed post (its draws carry no reveal
        // key; THREATS SEC-4: rehearsal only): ROADMAP C30.
        sealed: onyx_api::observations::SealedLog::default(),
    }
}

/// The run's public transcript, for the query API's results store
/// (`onyx_api::observations`, ADR-0027 amendment 4).
pub fn transcript(run: &RoundRun) -> Result<onyx_api::observations::Transcript, String> {
    let ObservationOutcome::Value(answer) = run.settlement.outcome().clone() else {
        return Err("the round settled no value".into());
    };
    let a = attempt(run);
    Ok(onyx_api::observations::Transcript {
        subject: a.subject,
        window_start: a.window_start,
        cfg: a.cfg,
        pool: a.pool,
        draw: a.draw,
        observers: a.observers,
        commits: a.commits,
        reveals: a.reveals,
        sealed: a.sealed,
        answer,
    })
}

/// The results store file the query API loads (`ONYX_OBSERVATIONS`) for
/// one run: [`append_results`] to an empty store.
pub fn results_store(c: &Company, run: &RoundRun) -> Result<Json, String> {
    append_results(c, None, run)
}

/// The results store `prev` (or a new one, for the company metric's template) with the run's
/// round added: a value round as the fact's round, a round without a value
/// as an attempt (SEC-R2-F1), served free at
/// `/v1/observations/{metric}/attempts`. Earlier rounds are kept, never
/// replaced (ATT-R1-F1), so a re-draw's store still holds every earlier
/// draw's round and `verify_attempts_onchain` passes for the fact.
pub fn append_results(c: &Company, prev: Option<&Json>, run: &RoundRun) -> Result<Json, String> {
    use onyx_api::observations::ObservationStore;
    let (rounds, attempts) = match run.settlement.outcome() {
        ObservationOutcome::Value(_) => (vec![transcript(run)?], vec![]),
        _ => (vec![], vec![attempt(run)]),
    };
    match prev {
        None => Ok(ObservationStore::to_json_with_attempts(
            &template(c),
            &rounds,
            &attempts,
            None,
        )),
        Some(p) => {
            ObservationStore::append(p, &template(c), &rounds, &attempts).map_err(|e| e.to_string())
        }
    }
}

/// The results store `prev` (or a new one) with `a` added as an attempt: the
/// opened round of a run that failed after its beacon was fixed
/// ([`RunError::attempt`], ATT-R2-F3).
pub fn append_attempt(c: &Company, prev: Option<&Json>, a: &Attempt) -> Result<Json, String> {
    use onyx_api::observations::ObservationStore;
    let attempts = std::slice::from_ref(a);
    match prev {
        None => Ok(ObservationStore::to_json_with_attempts(
            &template(c),
            &[],
            attempts,
            None,
        )),
        Some(p) => {
            ObservationStore::append(p, &template(c), &[], attempts).map_err(|e| e.to_string())
        }
    }
}

fn hex0x(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// The website's summary of one company's run (all values from the run
/// itself; the website re-checks it, `apps/web/verify` `check_edgar_rounds`).
/// With `sealed` the answer is withheld (ATT-R2-F1,
/// [`onyx_api::observations::seal_site_round`]): pass it for a round without
/// a value whose fact is not closed.
pub fn summary(
    c: &Company,
    run: &RoundRun,
    period_end: &str,
    http_dates: &[String],
    body: &[u8],
    sealed: bool,
) -> Json {
    let value = match run.claim.as_ref().map(|c| &c.value) {
        Some(Value::Int(n)) => n.to_string(),
        Some(other) => format!("{other:?}"),
        // ATT-R1-F1: a round without a value names its outcome.
        None => format!("{:?}", run.settlement.outcome()),
    };
    let agreed = run
        .settlement
        .statuses()
        .iter()
        .filter(|(_, s)| *s == ObserverStatus::Agreed)
        .count();
    let reveals: Vec<Json> = run
        .reveals
        .iter()
        .zip(http_dates)
        .map(|((a, o, s, rsig), d)| {
            let csig = run
                .commits
                .iter()
                .find(|(c, _, _)| c == a)
                .map(|(_, _, sig)| hex0x(sig));
            json!({
                "observer": hex0x(a),
                "observed_unix": o.observed_at,
                "http_date": d,
                "page_hash": hex0x(&o.page_hash.0),
                "commit": hex0x(&observation_commit(&run.round_id, a, o, s).0),
                "commit_sig": csig,
                "salt": hex0x(&s.0),
                "reveal_sig": hex0x(rsig),
            })
        })
        .collect();
    let MetricId(metric) = &run.spec.metric;
    let Subject::Aggregate(subject) = &run.spec.subject else {
        unreachable!("this feed's subject is an aggregate")
    };
    let mut v = json!({
        "company": { "key": c.key, "name": c.name, "cik": c.cik, "unit": c.unit },
        "feed": {
            "metric": metric,
            "subject": subject,
            "period_end": period_end,
            "url": run.spec.url,
            "selector": run.spec.selector,
            "window_start": run.spec.window_start,
            "window_end": run.spec.window_end,
            "min_observers": run.spec.min_observers,
            "spec_id": hex0x(&run.spec.id().0),
            "fetcher_id": hex0x(&run.spec.fetcher.0),
        },
        "config": {
            "commit_close": run.cfg.commit_close,
            "reveal_close": run.cfg.reveal_close,
            "reward": run.cfg.reward.to_string(),
            "outlier_slash": run.cfg.outlier_slash.to_string(),
            "miss_slash": run.cfg.miss_slash.to_string(),
        },
        "bonds": run.observers.iter().map(|o| json!({ "observer": hex0x(&o.address), "bond": o.bond.to_string() })).collect::<Vec<_>>(),
        // The draw (L2-F2): the pool snapshot and the beacon the observers
        // were drawn with, so the browser can recompute the draw.
        "pool": run.pool.iter().map(|o| json!({ "observer": hex0x(&o.address), "bond": o.bond.to_string() })).collect::<Vec<_>>(),
        "draw": {
            "beacon_height": run.draw.beacon_height,
            "beacon": hex0x(&run.draw.beacon.0),
            "attempt": run.draw.attempt,
            // The registry's record of the draw (L2RND-R1): announced on a
            // local test chain before the beacon height, beacon fixed from
            // that chain's block hash.
            "announcement": run.onchain_draw.to_json(),
        },
        "round_id": hex0x(&run.round_id.0),
        "value": value,
        "observers": run.observers.len(),
        "agreed": agreed,
        "reveals": reveals,
        "result_hash": hex0x(&run.settlement.result_hash().0),
        "proof_level": 2,
        "response_body": String::from_utf8_lossy(body),
    });
    if sealed {
        let commits = run
            .commits
            .iter()
            .map(|(a, h, sig)| {
                json!({ "observer": hex0x(a), "commit": hex0x(&h.0), "commit_sig": hex0x(sig) })
            })
            .collect();
        onyx_api::observations::seal_site_round(&mut v, commits);
    }
    v
}

/// The website's file: every company's checked round, plus the source's
/// constants (the website shows its own pinned copies, never these).
pub fn site_summary(rounds: &[Json]) -> Json {
    json!({
        "rehearsal": "Observers simulated on one machine; each fetched EDGAR itself over HTTPS.",
        "source": {
            "id": crate::SOURCE,
            "name": crate::SOURCE_NAME,
            "terms_url": crate::TERMS_URL,
            "terms_quote": crate::TERMS_QUOTE,
            "attribution": rights().attribution,
        },
        "proof_level": 2,
        "companies": rounds,
    })
}
