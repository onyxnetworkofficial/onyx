//! Review R8 (strike 3 on the draw lifecycle): three more routes to the
//! fatal state, a draw with a fixed beacon and no published round (every
//! client refuses the fact for good: `ChainMismatch("a draw with a fixed
//! beacon and no published round")`):
//!
//! - R8-1: `open_round` marked the draw opened before the registry's record
//!   was checked; a mixed read (`drawOf` and `drawCount` from two blocks)
//!   failed the check, the run returned a bare Err and `run_and_save` saved
//!   a book without the draw. The next run announced draw 1.
//! - R8-2: a lost book stopped at an earlier chain draw with a FIXED beacon
//!   (settled history) and left the latest draw unfixed, for anyone to fix.
//! - R8-3: the first `drawOf` read in `fix_beacon_idempotent` took a stale
//!   answer without retrying.
//!
//! The lead's decision: one invariant, enforced in one place
//! (`ObservationResults::release_draw`, the only way a draw leaves the
//! book) and checked by a property test: [`property_the_book_never_loses_a_draw`]
//! drives the real runner over a simulated chain with crashes, stale and
//! mixed reads, sends that fail before or after landing, strangers calling
//! `fixBeacon`, blocks passing, restarts with other inputs and lost books,
//! and asserts after every run that a draw left the book only per the
//! invariant, and after the runs quiesce that no draw has a fixed beacon
//! without a published round.
//!
//! Review R9 (0 merge-blocking): F1, a lost book plus a stale `drawCount`
//! adopted draw 0 whose round the results store already published, and
//! draw 1 was left fixable (seed 17822): a draw the results store publishes
//! is never adopted, and adoption reads one final block showing the draw as
//! the chain's last; F2, the client check here stubbed `transcriptComplete`
//! (now the registry's own); F3, one runner per book (`lock_book`).

use onyx_api::client::{verify_attempts_onchain, verify_observation, ObservationExpect};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    draw_and_open, ChainView, ChainWait, DrawChain, DrawRecord, LogBlock, ObservationResults,
    ObserverSig, OnchainDraw, OnchainFact, Posted, PostedCommit, RegistryDeployment, ReplayClock,
    SimulatedRegistry, TranscriptLogQuery, BLOCKHASH_WINDOW,
};
use onyx_core::{Hash32, MinerAddress, ObservationSpec};
use onyx_source_fiscaldata::round::{
    append_attempt, append_results, config, note_results, rehearsal_pool, run_and_save, store,
    Page, RoundRun, RunError,
};
use onyx_source_fiscaldata::{recorded, spec, subject, template, RECORDED_WINDOW_START};
use serde_json::Value as Json;
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- helpers

fn sp() -> ObservationSpec {
    spec(&recorded().record_date, RECORDED_WINDOW_START).expect("spec")
}

fn fact() -> OnchainFact {
    store().onchain_fact(&sp()).expect("fact")
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
    let d = std::env::temp_dir().join(format!("onyx-r8-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
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

/// The client's full check of the fact from the results store: off chain,
/// then `verify_attempts_onchain` against the chain's `drawCount`, `drawOf`
/// of every draw below the value and `drawBudget`.
fn client_accepts(chain: &SimulatedRegistry, results: &Json, attempt: u32) -> Result<(), String> {
    client_check(chain, results, attempt, &[])
}

/// [`client_accepts`], with the published rounds of the draw attempts in
/// `excused` read as transcript-complete whatever the registry says (review
/// R10-2: to check that a lost-mid-round round is the ONLY reason the
/// client refuses). The error names the published rounds whose
/// `transcriptComplete` is false (`incomplete draws [..]`).
fn client_check(
    chain: &SimulatedRegistry,
    results: &Json,
    attempt: u32,
    excused: &[u32],
) -> Result<(), String> {
    let mut chain = chain.clone();
    let sp = sp();
    let fact = store().onchain_fact(&sp).expect("fact");
    chain.record_value(&fact);
    let now = config(&sp).reveal_close + 1;
    let pins = TemplatePins::from([(template().metric.0.clone(), template().id())]);
    let feed = ObservationStore::from_json(&results.to_string(), now, &pins)
        .map_err(|e| format!("the results store loads: {e:?}"))?;
    let r = feed
        .record(&subject(&recorded().record_date), None, None)
        .ok_or_else(|| format!("no record in {}", results))?;
    let all = feed.body_all(r, &feed.attempts_of(&r.checked.fact));
    let t = template();
    let a = verify_observation(
        &all,
        &ObservationExpect {
            template: &t,
            subject: &sp.subject,
            window_end: None,
            min_window_end: 0,
            now,
        },
    )
    .map_err(|e| format!("off-chain: {e:?}"))?
    .attempts
    .ok_or("outcome=all")?;
    let count = word(u64::from(chain.draw_count(&fact)?));
    let draws: Vec<Vec<u8>> = (0..attempt)
        .map(|i| draw_ret(&chain.draw_of(&fact, i).expect("read").expect("draw")))
        .collect();
    let (strikes, allowance) = chain.draw_budget(&fact)?;
    let mut budget = word(u64::from(strikes));
    budget.extend(word(u64::from(allowance)));
    // Review R9-F2: the registry's own `transcriptComplete` for every
    // published round (the simulated registry's transcript log), never a
    // stub.
    let mut incomplete = vec![];
    let complete: Vec<Vec<u8>> = a
        .attempts
        .iter()
        .map(|c| {
            let mut w = vec![0u8; 32];
            let ok = chain.transcript_complete(&c.log)?;
            if !ok {
                incomplete.push(c.draw.attempt);
            }
            w[31] = u8::from(ok || excused.contains(&c.draw.attempt));
            Ok(w)
        })
        .collect::<Result<_, String>>()?;
    verify_attempts_onchain(&a, &count, &draws, &budget, &complete, &unsealed_all(&a))
        .map(|_| ())
        .map_err(|e| format!("{e:?} (incomplete draws {incomplete:?})"))
}

/// Draws (attempt, beacon height) the results store publishes, rounds and
/// failed attempts alike.
fn published(results: &Option<Json>) -> Vec<(u32, u64)> {
    let Some(r) = results else { return vec![] };
    ["rounds", "attempts"]
        .iter()
        .filter_map(|k| r[*k].as_array())
        .flatten()
        .filter_map(|e| {
            Some((
                u32::try_from(e["draw"]["attempt"].as_u64()?).ok()?,
                e["draw"]["beaconHeight"].as_u64()?,
            ))
        })
        .collect()
}

/// The attempt of the value round the results store publishes, if any.
fn published_value(results: &Option<Json>) -> Option<u32> {
    let r = results.as_ref()?;
    r["rounds"]
        .as_array()?
        .first()
        .and_then(|e| u32::try_from(e["draw"]["attempt"].as_u64()?).ok())
}

/// The draw the book on disk at `path` holds (none without a book).
fn held_on_disk(path: &Path) -> Option<OnchainDraw> {
    if !path.exists() {
        return None;
    }
    let mut s = store();
    s.load_book(path).expect("the book on disk loads");
    s.held_draw(&sp())
}

// ------------------------------------------------- the simulated chain

/// A small deterministic generator (splitmix64): one seed, one run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// True with probability `per_mille` / 1000.
    fn chance(&mut self, per_mille: u32) -> bool {
        self.next() % 1000 < u64::from(per_mille)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

/// Fault rates, per mille of chain calls.
#[derive(Clone, Copy, Debug)]
struct Faults {
    /// The process dies (no further write of any kind).
    crash: u32,
    /// A read answers from an older block (a lagging node).
    stale: u32,
    /// A `DrawRecord`'s `drawCount` comes from another block (R8-1's mixed
    /// read).
    mixed: u32,
    /// A send fails and does not land.
    fail_before: u32,
    /// A send lands and reports an error (receipt timeout).
    fail_after: u32,
    /// A stranger calls the public `fixBeacon` on an unfixed draw.
    stranger: u32,
    /// Up to 400 blocks pass.
    blocks: u32,
}

const QUIET: Faults = Faults {
    crash: 0,
    stale: 0,
    mixed: 0,
    fail_before: 0,
    fail_after: 0,
    stranger: 60,
    blocks: 0,
};

/// The panic payload of a simulated crash.
struct Crash;

/// `SimulatedRegistry` behind an unreliable RPC, with strangers and time.
struct Sim {
    inner: RefCell<SimulatedRegistry>,
    /// Earlier states (one per mined transaction), for stale reads.
    history: RefCell<Vec<SimulatedRegistry>>,
    rng: RefCell<Rng>,
    faults: Cell<Faults>,
    fact: OnchainFact,
}

impl Sim {
    fn new(seed: u64) -> Self {
        Sim {
            inner: RefCell::new(SimulatedRegistry::new()),
            history: RefCell::new(vec![]),
            rng: RefCell::new(Rng(seed)),
            faults: Cell::new(QUIET),
            fact: fact(),
        }
    }
    fn chance(&self, per_mille: u32) -> bool {
        self.rng.borrow_mut().chance(per_mille)
    }
    fn snap(&self) {
        let mut h = self.history.borrow_mut();
        h.push(self.inner.borrow().clone());
        if h.len() > 48 {
            h.remove(0);
        }
    }
    fn mine(&self, n: u64) {
        self.inner.borrow_mut().height += n;
        self.snap();
    }
    /// Every chain call: maybe the process dies, a stranger fixes a beacon,
    /// or blocks pass.
    fn tick(&self) {
        let f = self.faults.get();
        if self.chance(f.crash) {
            std::panic::panic_any(Crash);
        }
        if self.chance(f.stranger) {
            let mut c = self.inner.borrow_mut();
            let n = c.draw_count(&self.fact).unwrap_or(0);
            let a = u32::try_from(self.rng.borrow_mut().below(u64::from(n))).unwrap_or(0);
            if let Ok(Some(r)) = c.draw_of(&self.fact, a) {
                // A stranger can only fix a beacon whose block exists.
                if r.beacon == Hash32::ZERO && c.height > r.beacon_height {
                    let d = OnchainDraw {
                        fact: self.fact,
                        attempt: a,
                        commit: r.commit,
                        beacon_height: r.beacon_height,
                    };
                    let _ = c.fix_beacon(&d);
                    drop(c);
                    self.snap();
                }
            }
        }
        if self.chance(f.blocks) {
            let n = 1 + self.rng.borrow_mut().below(400);
            self.mine(n);
        }
    }
    /// The state a read sees: the head, or (a lagging node) an older block.
    fn view<T>(&self, f: impl Fn(&SimulatedRegistry) -> T) -> T {
        self.tick();
        let h = self.history.borrow();
        if !h.is_empty() && self.chance(self.faults.get().stale) {
            let i = usize::try_from(self.rng.borrow_mut().below(h.len() as u64)).unwrap_or(0);
            return f(&h[i]);
        }
        drop(h);
        f(&self.inner.borrow())
    }
    /// R8-1's mixed read: `drawCount` from another block than `drawOf`.
    fn mix(&self, mut r: DrawRecord) -> DrawRecord {
        let h = self.history.borrow();
        if !h.is_empty() && self.chance(self.faults.get().mixed) {
            let i = usize::try_from(self.rng.borrow_mut().below(h.len() as u64)).unwrap_or(0);
            r.draw_count = h[i].draw_count(&self.fact).unwrap_or(0);
        }
        r
    }
    /// A send: fails before landing, lands, or lands and reports an error.
    fn send<T>(
        &self,
        f: impl FnOnce(&mut SimulatedRegistry) -> Result<T, String>,
    ) -> Result<T, String> {
        self.tick();
        let fl = self.faults.get();
        if self.chance(fl.fail_before) {
            return Err("send: connection reset".into());
        }
        let r = f(&mut self.inner.borrow_mut());
        self.snap();
        if r.is_ok() && self.chance(fl.fail_after) {
            return Err("send: receipt timeout".into());
        }
        r
    }
}

impl ChainView for Sim {
    fn height(&self) -> Result<u64, String> {
        self.view(|c| c.height())
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.view(|c| c.block_hash(h))
    }
}

impl DrawChain for Sim {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.view(|c| c.draw_count(f))
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.view(|c| c.draw_budget(f))
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.send(|c| c.announce(d))
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.send(|c| c.fix_beacon(d)).map(|r| self.mix(r))
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.view(|c| c.draw_of(f, attempt))
            .map(|r| r.map(|r| self.mix(r)))
    }
    /// A final block is consistent: the block and its state (an older one
    /// when the node lags).
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        self.view(|c| c.draw_of_final(f, attempt))
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
        self.send(|c| c.post_commit(f, attempt, o, round, commit, sig))
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
        self.send(|c| c.post_receipt(f, attempt, o, round, commit, sig))
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.view(|c| c.log_block())
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        o: &MinerAddress,
        round: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.view(|c| c.posted_commit(block, f, attempt, o, round))
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.view(|c| c.transcript_complete(q))
    }
}

// ------------------------------------------------------------- runners

/// How one run ended.
enum Outcome {
    Value(Box<RoundRun>),
    Stopped(String),
    Crashed,
}

/// The `level2-round` binary once: load the book, `run_and_save`, write
/// the results store (the simulated disk survives a crash; a crash may hit
/// before or after each results write).
fn real_run(
    sim: &mut Sim,
    book: &Path,
    results: &RefCell<Option<Json>>,
    crash_writes: u32,
    rng: &RefCell<Rng>,
) -> Outcome {
    let rec = recorded();
    let pages = pages();
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut s = store();
        s.load_book(book).expect("the book loads");
        // The results store as written so far (review R9-F1).
        let written = results.borrow().clone();
        let write = |add: &dyn Fn(Option<&Json>) -> Result<Json, String>| {
            if rng.borrow_mut().chance(crash_writes) {
                std::panic::panic_any(Crash);
            }
            let v = add(results.borrow().as_ref())?;
            *results.borrow_mut() = Some(v);
            if rng.borrow_mut().chance(crash_writes) {
                std::panic::panic_any(Crash);
            }
            Ok(())
        };
        run_and_save(
            &rec.record_date,
            RECORDED_WINDOW_START,
            &pages,
            &mut s,
            sim,
            book,
            written.as_ref(),
            &ChainWait::NOW,
            &ReplayClock::default(),
            &mut NoInbox,
            &mut |r| write(&|p| append_results(p, r)),
            &mut |a| write(&|p| append_attempt(p, a)),
        )
    }));
    match r {
        Ok(Ok(run)) => Outcome::Value(Box::new(run)),
        Ok(Err(RunError { error, .. })) => Outcome::Stopped(error),
        Err(p) if p.is::<Crash>() => Outcome::Crashed,
        Err(p) => std::panic::resume_unwind(p),
    }
}

/// The rehearsal pool without its first member: another runner input.
fn drifted_pool() -> Vec<onyx_coordinator::observe::Observer> {
    let mut p = rehearsal_pool();
    p.remove(0);
    p
}

/// A restart with other inputs (a drifted pool) that draws and opens with
/// the book, then dies before its commit phase.
fn drifted_run(sim: &mut Sim, book: &Path, results: &RefCell<Option<Json>>) -> Outcome {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut s = store();
        s.load_book(book).expect("the book loads");
        // As every runner: the results store is read first (review R9-F1).
        note_results(&mut s, &sp(), results.borrow().as_ref(), None)?;
        draw_and_open(
            &mut s,
            sp(),
            config(&sp()),
            &drifted_pool(),
            sim,
            &ChainWait::NOW,
            &mut |s: &ObservationResults| s.save_book(book),
        )
    }));
    match r {
        Ok(Ok(_)) => Outcome::Crashed,
        Ok(Err(e)) => Outcome::Stopped(e),
        Err(p) if p.is::<Crash>() => Outcome::Crashed,
        Err(p) => std::panic::resume_unwind(p),
    }
}

/// The draw `old` the book held left it legitimately: its round or attempt
/// is published, or the chain (whose head is final here) shows it can never
/// land, or its beacon zero past the `blockhash` window.
fn left_per_invariant(
    old: &OnchainDraw,
    results: &Option<Json>,
    chain: &SimulatedRegistry,
) -> Result<(), String> {
    if published(results).contains(&(old.attempt, old.beacon_height)) {
        return Ok(());
    }
    let ours = |r: &DrawRecord| r.commit == old.commit && r.beacon_height == old.beacon_height;
    match chain.draw_of(&old.fact, old.attempt)? {
        Some(r) if !ours(&r) => Ok(()),
        None if chain.height >= old.beacon_height => Ok(()),
        Some(r)
            if r.beacon == Hash32::ZERO
                && chain.height > old.beacon_height + BLOCKHASH_WINDOW =>
        {
            Ok(())
        }
        other => Err(format!(
            "draw {} (beacon height {}) left the book unpublished while the chain shows {other:?} at height {}",
            old.attempt, old.beacon_height, chain.height
        )),
    }
}

/// The fatal state: every chain draw with a fixed beacon and no published
/// round (review R10-2: all of them, not the first only).
fn fatal(chain: &SimulatedRegistry, results: &Option<Json>) -> Vec<(u32, String)> {
    let f = fact();
    let n = chain.draw_count(&f).unwrap_or(0);
    let p = published(results);
    (0..n)
        .filter_map(|i| {
            let r = chain.draw_of(&f, i).ok()??;
            (r.beacon != Hash32::ZERO && !p.contains(&(i, r.beacon_height))).then(|| {
                (
                    i,
                    format!("draw {i}: a fixed beacon and no published round (drawCount {n})"),
                )
            })
        })
        .collect()
}

/// `ONYX_R8_TRACE=1`: each run's outcome on stderr.
fn trace(seed: u64, at: &str, o: &Outcome) {
    if std::env::var_os("ONYX_R8_TRACE").is_some() {
        let what = match o {
            Outcome::Value(r) => format!(
                "settled draw {} ({:?})",
                r.draw.attempt,
                r.settlement.outcome()
            ),
            Outcome::Stopped(e) => format!("stopped: {e}"),
            Outcome::Crashed => "crashed".into(),
        };
        eprintln!("seed {seed} {at}: {what}");
    }
}

/// How a case ended, for the summary.
#[derive(Default, Debug)]
struct Tally {
    cases: u32,
    runs: u32,
    crashes: u32,
    lost_books: u32,
    drifted: u32,
    values: u32,
    no_value: u32,
    /// Cases with a book lost after its round's log snapshot (receipts may
    /// be on chain): THREATS REVLOG-R5-1d, REVLOG-R9-F2.
    lost_mid_round: u32,
    /// Of those, cases that ended in the fatal state (a fixed beacon
    /// without a published round).
    lost_mid_round_fatal: u32,
    /// Of those, cases whose published value the client refuses on
    /// `transcriptComplete` (a receipted reveal left out).
    lost_mid_round_incomplete: u32,
    releases_checked: u32,
}

impl Tally {
    fn add(&mut self, t: &Tally) {
        self.cases += t.cases;
        self.runs += t.runs;
        self.crashes += t.crashes;
        self.lost_books += t.lost_books;
        self.drifted += t.drifted;
        self.values += t.values;
        self.no_value += t.no_value;
        self.lost_mid_round += t.lost_mid_round;
        self.lost_mid_round_fatal += t.lost_mid_round_fatal;
        self.lost_mid_round_incomplete += t.lost_mid_round_incomplete;
        self.releases_checked += t.releases_checked;
    }
}

/// One case: `steps` faulty runs, then quiet runs until the fact settles
/// a value or stops for good. Err: the invariant broke.
fn case(seed: u64, steps: u32, t: &mut Tally) -> Result<(), String> {
    let d = dir(&format!("p{seed}"));
    let book = d.join("book.json");
    let results = RefCell::new(None::<Json>);
    let mut sim = Sim::new(seed);
    let mut rng = Rng(seed ^ 0x5EED);
    let wrng = RefCell::new(Rng(seed ^ 0xC0FFEE));
    // Fault mix per case, so some cases are calm and some are storms.
    let faults = Faults {
        crash: [0, 10, 30, 60][usize::try_from(rng.below(4)).unwrap_or(0)],
        stale: [0, 100, 300][usize::try_from(rng.below(3)).unwrap_or(0)],
        mixed: [0, 100, 400][usize::try_from(rng.below(3)).unwrap_or(0)],
        fail_before: [0, 50, 150][usize::try_from(rng.below(3)).unwrap_or(0)],
        fail_after: [0, 50, 150][usize::try_from(rng.below(3)).unwrap_or(0)],
        stranger: [0, 50, 200][usize::try_from(rng.below(3)).unwrap_or(0)],
        blocks: [0, 10, 40][usize::try_from(rng.below(3)).unwrap_or(0)],
    };
    let mut held = None;
    let mut lost_mid_round = false;
    // The draws whose rounds were lost after their log snapshot.
    let mut lost_rounds: Vec<u32> = vec![];
    let mut lost_any = false;
    let check = |held: &mut Option<OnchainDraw>,
                 lost: &bool,
                 sim: &Sim,
                 results: &RefCell<Option<Json>>,
                 t: &mut Tally,
                 at: &str|
     -> Result<(), String> {
        let now = held_on_disk(&book);
        if let Some(old) = held.as_ref() {
            let same = now.as_ref().is_some_and(|n| {
                n.attempt == old.attempt
                    && n.commit == old.commit
                    && n.beacon_height == old.beacon_height
            });
            if !same {
                t.releases_checked += 1;
                left_per_invariant(old, &results.borrow(), &sim.inner.borrow())
                    .map_err(|e| format!("seed {seed} {at}: {e}"))?;
            }
        }
        // Every fixed chain draw is published or held by the book on disk
        // (a book that was never lost: a lost one is R8-2's case, checked
        // at quiescence).
        if !*lost {
            let c = sim.inner.borrow();
            let f = fact();
            let p = published(&results.borrow());
            for i in 0..c.draw_count(&f).unwrap_or(0) {
                let Ok(Some(r)) = c.draw_of(&f, i) else {
                    continue;
                };
                let in_book = now.as_ref().is_some_and(|n| {
                    n.attempt == i && n.commit == r.commit && n.beacon_height == r.beacon_height
                });
                if r.beacon != Hash32::ZERO && !p.contains(&(i, r.beacon_height)) && !in_book {
                    return Err(format!(
                        "seed {seed} {at}: draw {i} has a fixed beacon, no published round, and the book on disk does not hold it"
                    ));
                }
            }
        }
        *held = now;
        Ok(())
    };
    let mut value = None;
    for step in 0..steps {
        sim.faults.set(faults);
        if rng.chance(60) {
            // A lost book: the invariant cannot hold a draw it never sees;
            // R8-2 governs what the next run does with the chain's draws.
            // A book lost after its round's log snapshot (receipts may be
            // on chain) loses the receipted reveals no transcript can then
            // leave out: unrecoverable until the observers resend them
            // (THREATS REVLOG-R5-1d); counted, not asserted.
            if book.exists() {
                let mut s = store();
                s.load_book(&book).expect("loads");
                if let Some(u) = s
                    .unfinished(&sp())
                    .filter(|u| u.round.log_snapshot().is_some())
                {
                    lost_mid_round = true;
                    lost_rounds.push(u.round.draw().attempt);
                }
            }
            let _ = std::fs::remove_file(&book);
            lost_any = true;
            held = None;
            t.lost_books += 1;
            if std::env::var_os("ONYX_R8_TRACE").is_some() {
                eprintln!("seed {seed} step {step}: the book is lost");
            }
        }
        let o = if rng.chance(120) {
            t.drifted += 1;
            if std::env::var_os("ONYX_R8_TRACE").is_some() {
                eprintln!("seed {seed} step {step}: drifted inputs");
            }
            drifted_run(&mut sim, &book, &results)
        } else {
            real_run(&mut sim, &book, &results, faults.crash, &wrng)
        };
        t.runs += 1;
        trace(seed, &format!("step {step}"), &o);
        sim.faults.set(QUIET);
        check(
            &mut held,
            &lost_any,
            &sim,
            &results,
            t,
            &format!("step {step}"),
        )?;
        match o {
            Outcome::Value(r) if r.claim.is_some() => {
                value = Some(r);
                break;
            }
            Outcome::Value(_) => {}
            Outcome::Crashed => t.crashes += 1,
            Outcome::Stopped(_) => {}
        }
    }
    let mut last = String::new();
    for q in 0..24 {
        if value.is_some() {
            break;
        }
        let o = real_run(&mut sim, &book, &results, 0, &wrng);
        t.runs += 1;
        trace(seed, &format!("quiet {q}"), &o);
        check(
            &mut held,
            &lost_any,
            &sim,
            &results,
            t,
            &format!("quiet {q}"),
        )?;
        match o {
            Outcome::Value(r) if r.claim.is_some() => value = Some(r),
            Outcome::Value(_) | Outcome::Crashed => {}
            Outcome::Stopped(e) => {
                if e.contains("DrawsExhausted")
                    || e.contains("closed")
                    || e.contains("already publishes this fact's value")
                {
                    last = e;
                    break;
                }
                if e.contains("run with the inputs it announced") {
                    // The operator follows the error: runs the inputs the
                    // chain's draw was announced with.
                    let _ = drifted_run(&mut sim, &book, &results);
                    check(
                        &mut held,
                        &lost_any,
                        &sim,
                        &results,
                        t,
                        &format!("quiet {q} drifted"),
                    )?;
                } else {
                    sim.mine(300);
                }
                last = e;
            }
        }
    }
    let chain = sim.inner.borrow().clone();
    let _ = std::fs::remove_dir_all(&d);
    t.cases += 1;
    if lost_mid_round {
        t.lost_mid_round += 1;
    }
    let fatal = fatal(&chain, &results.borrow());
    if !fatal.is_empty() {
        // Review R9-F2: a book lost after its round's log snapshot is the
        // documented limit (THREATS REVLOG-R5-1d): counted, never passed
        // off as a success, and only on the lost round's own draw. Review
        // R10-2: EVERY fatal draw must be such a lost round's; any other
        // fails the case.
        if fatal.iter().all(|(at, _)| lost_rounds.contains(at)) {
            t.lost_mid_round_fatal += 1;
            return Ok(());
        }
        let f: Vec<&str> = fatal.iter().map(|(_, f)| f.as_str()).collect();
        return Err(format!(
            "seed {seed} ({faults:?}): {}; lost mid-round: {lost_rounds:?}; last error: {last}; book holds {held:?}",
            f.join("; ")
        ));
    }
    // The value this run settled, or one a run published before its book
    // was lost (the next run then stops: the fact is settled).
    match value
        .map(|v| v.draw.attempt)
        .or_else(|| published_value(&results.borrow()))
    {
        Some(attempt) => {
            t.values += 1;
            let r = results.borrow().clone().expect("results");
            match client_accepts(&chain, &r, attempt) {
                Ok(()) => {}
                // Review R9-F2: the same limit seen by the client's real
                // `transcriptComplete` check (a receipted reveal left with
                // the lost book): counted with the lost-mid-round cases.
                // Review R10-2: only when the client accepts the fact with
                // the lost rounds' transcripts read as complete, so the
                // incomplete round is a lost one and nothing else is wrong.
                Err(e)
                    if lost_mid_round
                        && e.contains("transcriptComplete")
                        && client_check(&chain, &r, attempt, &lost_rounds).is_ok() =>
                {
                    t.lost_mid_round_incomplete += 1;
                }
                Err(e) => {
                    return Err(format!(
                        "seed {seed}: the client refuses the fact: {e}; lost mid-round: {lost_rounds:?}"
                    ));
                }
            }
        }
        None => t.no_value += 1,
    }
    Ok(())
}

/// Silence the simulated crashes' panic messages (other panics print).
fn quiet_crashes() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !info.payload().is::<Crash>() {
            prev(info);
        }
    }));
}

/// THE PROPERTY TEST (review R8): over `ONYX_R8_CASES` seeds (default
/// 20,000, about 50 s on the dev box; review R9) from `ONYX_R8_SEED`
/// (default 1), each case runs the real runner up to 6 times under a random
/// fault mix, then quietly until the fact settles or stops for good, and
/// asserts (1) after every run, a draw the book on disk held and no longer
/// holds was published or is proven on chain unable to land or be fixed,
/// and (2) at quiescence, no chain draw has a fixed beacon without a
/// published round, and a settled value passes the client's full check
/// (with the registry's real `transcriptComplete`, review R9-F2). The one
/// exception, counted and reported, never passed: a book lost after its
/// round's log snapshot (THREATS REVLOG-R5-1d), on that round's own draw.
/// Long runs: `ONYX_R8_CASES=100000 cargo test -p onyx-source-fiscaldata
/// --test revlog_r8 property -- --nocapture` (`ONYX_R8_TRACE=1` traces a
/// seed).
#[test]
fn property_the_book_never_loses_a_draw() {
    quiet_crashes();
    let cases: u64 = std::env::var("ONYX_R8_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let first: u64 = std::env::var("ONYX_R8_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let failures: Vec<String> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|k| {
                sc.spawn(move || {
                    let mut t = Tally::default();
                    let mut fails = vec![];
                    let mut seed = first + k;
                    while seed < first + cases {
                        if let Err(e) = case(seed, 6, &mut t) {
                            fails.push(e);
                        }
                        seed += threads;
                    }
                    (t, fails)
                })
            })
            .collect();
        let mut all = Tally::default();
        let mut fails = vec![];
        for h in hs {
            let (t, f) = h.join().expect("thread");
            all.add(&t);
            fails.extend(f);
        }
        eprintln!("R8 property: {all:?}");
        fails
    });
    for f in failures.iter().take(8) {
        eprintln!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} of {cases} cases broke the draw invariant; first: {}",
        failures.len(),
        failures[0]
    );
}

// ------------------------------------------------------ the R8 PoCs

/// A chain whose `fixBeacon` read-back (or every `drawOf`, `persistent`)
/// mixes `drawCount` from one block earlier: R8-1.
struct CountLag {
    inner: SimulatedRegistry,
    persistent: bool,
}

impl ChainView for CountLag {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for CountLag {
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
        let mut r = self.inner.fix_beacon(d)?;
        r.draw_count -= 1;
        Ok(r)
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        let r = self.inner.draw_of(f, attempt)?;
        Ok(r.map(|mut r| {
            if self.persistent && r.beacon != Hash32::ZERO {
                r.draw_count -= 1;
            }
            r
        }))
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, a)
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

/// One `level2-round` run over `chain`, writing `results`.
fn run_on(
    chain: &mut dyn DrawChain,
    book: &Path,
    results: &RefCell<Option<Json>>,
) -> Result<RoundRun, RunError> {
    let rec = recorded();
    let mut s = store();
    s.load_book(book).expect("the book loads");
    let written = results.borrow().clone();
    run_and_save(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages(),
        &mut s,
        chain,
        book,
        written.as_ref(),
        &ChainWait::NOW,
        &ReplayClock::default(),
        &mut NoInbox,
        &mut |r| {
            let v = append_results(results.borrow().as_ref(), r)?;
            *results.borrow_mut() = Some(v);
            Ok(())
        },
        &mut |a| {
            let v = append_attempt(results.borrow().as_ref(), a)?;
            *results.borrow_mut() = Some(v);
            Ok(())
        },
    )
}

/// The reviewer's PoC (`r8_lagging_draw_count_after_fix_drops_the_opened_draw_from_the_book`):
/// `fixBeacon`'s read-back mixed `drawCount` from one block earlier; the
/// record check failed AFTER `open_round` marked the draw opened, and
/// `run_and_save` saved a book without draw 0: run 2 settled draw 1 and the
/// client refused the fact. Now the record is checked before anything is
/// opened and read again (one block): run 1 settles draw 0.
#[test]
fn r8_1_lagging_draw_count_after_fix_drops_the_opened_draw_from_the_book() {
    let d = dir("r8-1");
    let book = d.join("book.json");
    let results = RefCell::new(None);
    let mut chain = CountLag {
        inner: SimulatedRegistry::new(),
        persistent: false,
    };
    let r = run_on(&mut chain, &book, &results).expect("run 1 settles on draw 0");
    assert_eq!(r.draw.attempt, 0, "draw 0, never drawn past");
    assert_eq!(chain.inner.draw_count(&fact()), Ok(1));
    client_accepts(
        &chain.inner,
        results.borrow().as_ref().expect("results"),
        r.draw.attempt,
    )
    .expect("the client accepts the fact");
    let _ = std::fs::remove_dir_all(&d);
}

/// R8-1 with every read mixed during run 1 (the record never matches): the
/// run stops, the book on disk still holds draw 0 (announced: the opening
/// changed nothing), and run 2, on a sound node, resumes draw 0.
#[test]
fn r8_1_a_record_that_never_matches_keeps_the_draw_in_the_book() {
    let d = dir("r8-1b");
    let book = d.join("book.json");
    let results = RefCell::new(None);
    let mut chain = CountLag {
        inner: SimulatedRegistry::new(),
        persistent: true,
    };
    let e = run_on(&mut chain, &book, &results).expect_err("the record never matches");
    assert!(
        e.error.contains("registry's draw is not the round's"),
        "{}",
        e.error
    );
    let held = held_on_disk(&book).expect("the book on disk holds draw 0");
    assert_eq!(held.attempt, 0);
    let mut s = store();
    s.load_book(&book).expect("loads");
    assert_eq!(
        s.pending_draw(&sp()).map(|d| d.attempt),
        Some(0),
        "announced, to resume"
    );
    chain.persistent = false;
    let r = run_on(&mut chain, &book, &results).expect("run 2 resumes draw 0");
    assert_eq!(r.draw.attempt, 0);
    assert_eq!(chain.inner.draw_count(&fact()), Ok(1), "no draw 1");
    client_accepts(
        &chain.inner,
        results.borrow().as_ref().expect("results"),
        r.draw.attempt,
    )
    .expect("the client accepts the fact");
    let _ = std::fs::remove_dir_all(&d);
}

/// The reviewer's PoC (`r8_lost_book_never_adopts_a_draw_after_a_fixed_one`):
/// book A opened draw 0 (its beacon fixed); its next run announced draw 1
/// and the book was lost. A fresh book stopped at draw 0 (fixed) and left
/// draw 1 unfixed for anyone to fix. Now draw 0, an earlier draw with a
/// fixed beacon, is settled history (it got a successor only once
/// released): the fresh book draws past it and adopts draw 1 (same inputs).
#[test]
fn r8_2_lost_book_adopts_the_latest_draw_after_a_fixed_one() {
    let sp = sp();
    let cfg = config(&sp);
    let mut chain = SimulatedRegistry::new();
    let mut a = store();
    draw_and_open(
        &mut a,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0 opens");
    let mut b = store();
    b.sync_draws(&sp, 1).expect("sync");
    let h = chain.height().expect("height") + 3;
    b.announce_draw(&sp, &cfg, &rehearsal_pool(), h, &chain)
        .expect("announce");
    let d1 = b.latest_draw(&sp).expect("draw 1");
    chain.announce(&d1).expect("draw 1 lands");
    let mut fresh = store();
    fresh
        .note_published_draws(&sp, &[])
        .expect("no results store");
    let (round, rec) = draw_and_open(
        &mut fresh,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("the fresh book adopts draw 1");
    assert_eq!((round.draw().attempt, rec.attempt), (1, 1));
    assert_eq!(rec.commit, d1.commit);
    assert_ne!(
        rec.beacon,
        Hash32::ZERO,
        "draw 1's beacon fixed by its own book"
    );
    assert_eq!(chain.draw_count(&fact()), Ok(2), "no draw 2");
}

/// R8-2 in the pending-draw branch: the book holds draw 0 pending (never
/// landed, its beacon height long past), the chain holds draws 0 and 1 of
/// another book (draw 0 fixed: settled history). The book's draw is
/// released on the final-block proof (another draw at its attempt) and the
/// latest chain draw is adopted (same inputs) rather than drawn past.
#[test]
fn r8_2_the_pending_branch_adopts_the_latest_draw_too() {
    let sp = sp();
    let cfg = config(&sp);
    let mut chain = SimulatedRegistry::new();
    // The book's draw 0, saved and never sent, with another pool.
    let mut book = store();
    book.announce_draw(&sp, &cfg, &drifted_pool(), 5, &chain)
        .expect("announce");
    // Another book: draw 0 opened (fixed), draw 1 announced.
    let mut other = store();
    draw_and_open(
        &mut other,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0 opens");
    let mut o2 = store();
    o2.sync_draws(&sp, 1).expect("sync");
    let h = chain.height().expect("height") + 3;
    o2.announce_draw(&sp, &cfg, &rehearsal_pool(), h, &chain)
        .expect("announce");
    let d1 = o2.latest_draw(&sp).expect("draw 1");
    chain.announce(&d1).expect("lands");
    book.note_published_draws(&sp, &[])
        .expect("no results store");
    let (_, rec) = draw_and_open(
        &mut book,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("adopts draw 1");
    assert_eq!(rec.attempt, 1);
    assert_eq!(rec.commit, d1.commit);
}

/// R8-3: the first `drawOf` read answered from a lagging node (no draw
/// yet) was taken as the answer: `fix_beacon_idempotent` failed without a
/// retry. A stranger had already fixed the beacon. Now the stale answer is
/// retried, the fixed beacon is success, and the coordinator sends nothing.
#[test]
fn r8_3_a_stale_first_draw_of_is_retried() {
    use onyx_coordinator::observe::fix_beacon_idempotent;
    struct Lagging {
        inner: SimulatedRegistry,
        stale: Cell<u32>,
        sends: u32,
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
            self.sends += 1;
            self.inner.fix_beacon(d)
        }
        fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
            let n = self.stale.get();
            if n > 0 {
                self.stale.set(n - 1);
                return Ok(None);
            }
            self.inner.draw_of(f, a)
        }
        fn draw_of_final(
            &self,
            f: &OnchainFact,
            a: u32,
        ) -> Result<(u64, Option<DrawRecord>), String> {
            self.inner.draw_of_final(f, a)
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
            self.inner.post_commit(f, a, o, r, c, s)
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
            self.inner.post_receipt(f, a, o, r, c, s)
        }
        fn log_block(&self) -> Result<Option<LogBlock>, String> {
            self.inner.log_block()
        }
        fn posted_commit(
            &self,
            b: u64,
            f: &OnchainFact,
            a: u32,
            o: &MinerAddress,
            r: &Hash32,
        ) -> Result<Option<PostedCommit>, String> {
            self.inner.posted_commit(b, f, a, o, r)
        }
        fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
            self.inner.transcript_complete(q)
        }
    }
    let sp = sp();
    let cfg = config(&sp);
    let mut s = store();
    let mut chain = Lagging {
        inner: SimulatedRegistry::new(),
        stale: Cell::new(0),
        sends: 0,
    };
    s.announce_draw(&sp, &cfg, &rehearsal_pool(), 3, &chain)
        .expect("announce");
    let d = s.latest_draw(&sp).expect("draw");
    chain.inner.announce(&d).expect("lands");
    chain.inner.fix_beacon(&d).expect("a stranger fixes it");
    chain.stale.set(1);
    let r = fix_beacon_idempotent(&mut chain, &d, &ChainWait::NOW).expect("fixed is success");
    assert_ne!(r.beacon, Hash32::ZERO);
    assert_eq!(
        chain.sends, 0,
        "no own fixBeacon: no revert, no wasted send"
    );
}

// ------------------------------------------------------ review R9

/// A chain whose reads come from `old` (a node lagging behind the head) or
/// from the head, per read kind; sends go to the head.
struct Split {
    head: SimulatedRegistry,
    old: Option<SimulatedRegistry>,
    count_old: bool,
    of_old: bool,
    final_old: bool,
}

impl Split {
    fn read(&self, old: bool) -> &SimulatedRegistry {
        match (&self.old, old) {
            (Some(o), true) => o,
            _ => &self.head,
        }
    }
}

impl ChainView for Split {
    fn height(&self) -> Result<u64, String> {
        self.head.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.head.block_hash(h)
    }
}

impl DrawChain for Split {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        self.read(self.count_old).draw_count(f)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        self.read(self.count_old).draw_budget(f)
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        self.head.announce(d)
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.head.fix_beacon(d)
    }
    fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
        self.read(self.of_old).draw_of(f, a)
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.read(self.final_old).draw_of_final(f, a)
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
        self.head.post_commit(f, a, o, r, c, s)
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
        self.head.post_receipt(f, a, o, r, c, s)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.head.log_block()
    }
    fn posted_commit(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.head.posted_commit(b, f, a, o, r)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.head.transcript_complete(q)
    }
}

/// The chain review R9-F1 needs: draw 0 opened by a book (its beacon fixed;
/// its round published), then draw 1 announced with the same inputs by a
/// later run of that book, which was then lost. Returns the chain before
/// draw 1 (what a lagging node still serves), the chain, draw 0's seed and
/// draw 1.
fn published_then_lost() -> (
    SimulatedRegistry,
    SimulatedRegistry,
    onyx_coordinator::observe::DrawSeed,
    OnchainDraw,
) {
    let sp = sp();
    let cfg = config(&sp);
    let mut chain = SimulatedRegistry::new();
    let mut a = store();
    a.note_published_draws(&sp, &[])
        .expect("no results store yet");
    let (round0, _) = draw_and_open(
        &mut a,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0 opens");
    let old = chain.clone();
    let mut b = store();
    b.sync_draws(&sp, 1).expect("sync");
    let h = chain.height().expect("height") + 3;
    b.announce_draw(&sp, &cfg, &rehearsal_pool(), h, &chain)
        .expect("announce");
    let d1 = b.latest_draw(&sp).expect("draw 1");
    chain.announce(&d1).expect("draw 1 lands");
    (old, chain, round0.draw(), d1)
}

/// Review R9-F1 (a), the reviewer's seed 17822 as a fixed scenario: a lost
/// book, and a node behind the head (`drawCount` 1, `drawOf` and the final
/// block before draw 1, all consistent). Before the fix the fresh book
/// adopted draw 0, whose round the results store already publishes, and
/// opened it again (the results store then refused the second round for
/// good) while draw 1 stayed fixable by anyone. Now a draw the results
/// store publishes is never adopted: the run follows it, and once the node
/// catches up draw 1 is adopted and opened on its own beacon.
#[test]
fn r9_f1_a_lost_book_never_adopts_a_published_draw() {
    let sp = sp();
    let cfg = config(&sp);
    let (old, head, seed0, d1) = published_then_lost();
    let d = dir("r9-f1a");
    let book = d.join("book.json");
    let mut chain = Split {
        head,
        old: Some(old),
        count_old: true,
        of_old: true,
        final_old: true,
    };
    let mut opened = vec![];
    for run in 0..4 {
        if run > 0 {
            chain.old = None;
        }
        let mut c = store();
        c.load_book(&book).expect("loads");
        c.note_published_draws(&sp, &[seed0]).expect("noted");
        match draw_and_open(
            &mut c,
            sp.clone(),
            cfg,
            &rehearsal_pool(),
            &mut chain,
            &ChainWait::NOW,
            &mut |s: &ObservationResults| s.save_book(&book),
        ) {
            Ok((round, rec)) => {
                opened.push((round.draw().attempt, rec.commit, rec.beacon_height));
                break;
            }
            Err(e) => assert!(!e.contains("panicked"), "{e}"),
        }
    }
    assert_eq!(
        opened,
        vec![(1, d1.commit, d1.beacon_height)],
        "draw 0 is never opened again; draw 1 is adopted"
    );
    assert_eq!(chain.head.draw_count(&fact()), Ok(2), "no draw 2");
    let r1 = chain
        .head
        .draw_of(&fact(), 1)
        .expect("read")
        .expect("draw 1");
    assert_ne!(r1.beacon, Hash32::ZERO, "draw 1's beacon fixed by its book");
    let _ = std::fs::remove_dir_all(&d);
}

/// Review R9-F1 (b): adoption reads ONE final block showing the draw as the
/// chain's last. A node behind the head answers `drawCount` 1 and `drawOf(0)`
/// with draw 0's count 1, while the final block holds draws 0 and 1 (and
/// the results store does not publish draw 0: a lost results store, operator
/// error). Before the fix draw 0 was adopted and opened on the stale record;
/// now the final block's count (2) is not the count read (1): nothing is
/// adopted, and the run stops to be run again.
#[test]
fn r9_f1_adoption_needs_one_block_showing_the_last_draw() {
    let sp = sp();
    let cfg = config(&sp);
    let (old, head, _, _) = published_then_lost();
    let mut chain = Split {
        head,
        old: Some(old),
        count_old: true,
        of_old: true,
        final_old: false,
    };
    let mut c = store();
    c.note_published_draws(&sp, &[]).expect("noted");
    let e = draw_and_open(
        &mut c,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect_err("not adopted from a stale read");
    assert!(
        e.contains("one final block shows it as the chain's last"),
        "{e}"
    );
    assert_eq!(c.held_draw(&sp), None, "nothing adopted");
    assert_eq!(chain.head.draw_count(&fact()), Ok(2), "no draw 2");
}

/// Review R9-F1: a lost book after a VALUE was published (a crash after the
/// results write) never draws again: the run stops, the fact is settled.
#[test]
fn r9_f1_a_published_value_stops_a_lost_book() {
    let d = dir("r9-f1c");
    let book = d.join("book.json");
    let results = RefCell::new(None);
    let mut chain = SimulatedRegistry::new();
    let r = run_on(&mut chain, &book, &results).expect("draw 0 settles");
    assert!(r.claim.is_some(), "a value");
    std::fs::remove_file(&book).expect("the book is lost");
    let e = run_on(&mut chain, &book, &results).expect_err("settled");
    assert!(
        e.error.contains("already publishes this fact's value"),
        "{}",
        e.error
    );
    assert_eq!(chain.draw_count(&fact()), Ok(1), "no draw 1");
    let _ = std::fs::remove_dir_all(&d);
}

/// The property test's cases the reviewer named (R9-F1 seed 17822, R9-F2
/// seed 10976), run on their own: neither breaks the invariant (the
/// faults' trajectories follow the code, so these guard the seeds, the
/// scenarios above guard the routes).
#[test]
fn r9_reviewer_seeds_hold() {
    quiet_crashes();
    let mut t = Tally::default();
    for seed in [17_822, 10_976] {
        case(seed, 6, &mut t).expect("the invariant holds");
    }
}

/// Review R9-F3: one runner per book: a second lock on the same book fails
/// while the first is held, and succeeds once it is dropped.
#[test]
fn r9_f3_one_runner_per_book() {
    let d = dir("r9-f3");
    let book = d.join("book.json");
    let first = onyx_coordinator::observe::lock_book(&book).expect("first runner");
    let e = onyx_coordinator::observe::lock_book(&book).expect_err("second runner");
    assert!(e.contains("another runner holds this book"), "{e}");
    drop(first);
    onyx_coordinator::observe::lock_book(&book).expect("free again");
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------ review R10

/// Review R10-1 (the reviewer's PoC, ported): a results store of ANOTHER
/// deployment (same fact, same attempt, same beacon height: fresh chains
/// give the same heights) made a lost book follow the chain's latest draw
/// as if published, drawing past it while its beacon was unfixed; a
/// stranger then fixed it: a fixed beacon without a published round, which
/// every client refuses. Now a published draw matches the chain's only with
/// the same nonzero beacon too: the run stops, nothing is drawn past.
#[test]
fn r10_1_a_foreign_results_store_never_draws_past_an_unfixed_draw() {
    let sp = sp();
    let cfg = config(&sp);
    // Deployment A: draw 0 opened (beacon fixed); its seed is what A's
    // results store publishes.
    let mut chain_a = SimulatedRegistry::new();
    let mut a = store();
    a.note_published_draws(&sp, &[]).expect("noted");
    let (round0, _) = draw_and_open(
        &mut a,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain_a,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("A draw 0");
    let seed_a0 = round0.draw();
    assert_ne!(seed_a0.beacon, Hash32::ZERO);
    // Deployment B: a book announces draw 0 at the same beacon height, then
    // is lost.
    let mut chain_b = SimulatedRegistry::new();
    let mut b = store();
    b.sync_draws(&sp, 0).expect("sync");
    b.announce_draw(
        &sp,
        &cfg,
        &rehearsal_pool(),
        seed_a0.beacon_height,
        &chain_b,
    )
    .expect("announce");
    let d0 = b.latest_draw(&sp).expect("d0");
    chain_b.announce(&d0).expect("B draw 0 lands");
    // A fresh book, the operator's results store is A's.
    let mut c = store();
    c.note_published_draws(&sp, &[seed_a0]).expect("noted");
    let e = draw_and_open(
        &mut c,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain_b,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .map(|_| ())
    .expect_err("a foreign published draw stops the run");
    assert!(e.contains("another registry"), "{e}");
    let n = chain_b.draw_count(&fact()).expect("count");
    let r0 = chain_b.draw_of(&fact(), 0).expect("read").expect("draw 0");
    assert_eq!(n, 1, "B's draw 0 is never drawn past");
    assert_eq!(r0.beacon, Hash32::ZERO);
    // With B's own (empty) results store the same lost book adopts draw 0.
    let mut c = store();
    c.note_published_draws(&sp, &[]).expect("noted");
    let (r, _) = draw_and_open(
        &mut c,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain_b,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("B's draw 0 adopted");
    assert_eq!(r.draw().attempt, 0);
}

/// Review R10-1 (b): runners tag the results store with their deployment
/// (`onchain`: chain id, registry) and `published_draws` refuses a store
/// naming another deployment, or naming none while the run has a chain; a
/// run on deployment B with A's results store stops before anything is
/// sent.
#[test]
fn r10_1_a_results_store_of_another_deployment_is_refused() {
    let pa = RegistryDeployment {
        chain_id: 31337,
        registry: [0xa; 20],
        deploy_block: Hash32([1; 32]),
    };
    let pb = RegistryDeployment {
        chain_id: 31337,
        registry: [0xb; 20],
        deploy_block: Hash32([1; 32]),
    };
    let sp = sp();
    let untagged = ObservationStore::to_json_with_attempts(&template(), &[], &[], None);
    let tag_a = ObservationStore::tagged(untagged.clone(), Some(pa)).expect("tagged");
    let draws = |v: &Json, p| {
        ObservationStore::published_draws(v, &template(), &sp.subject, sp.window_start, p)
    };
    assert!(draws(&tag_a, Some(pa)).is_ok());
    assert!(draws(&untagged, None).is_ok());
    assert!(draws(&tag_a, Some(pb)).is_err(), "another deployment");
    assert!(draws(&tag_a, None).is_err(), "a chain's store in a replay");
    assert!(draws(&untagged, Some(pb)).is_err(), "untagged with a chain");
    // Tagging: an untagged store takes the tag, the same tag is kept,
    // another is refused.
    assert_eq!(
        ObservationStore::tagged(tag_a.clone(), Some(pa)).expect("same"),
        tag_a
    );
    assert!(ObservationStore::tagged(tag_a.clone(), Some(pb)).is_err());
    assert_eq!(
        ObservationStore::tagged(tag_a.clone(), None).expect("none"),
        tag_a
    );

    // End to end: A settles the fact, its store tagged A.
    let d = dir("r10-1b");
    let mut chain_a = SimulatedRegistry::new();
    chain_a.deployment = Some(pa);
    let results = RefCell::new(None);
    run_on(&mut chain_a, &d.join("a-book.json"), &results).expect("A runs");
    let store_a = ObservationStore::tagged(results.borrow().clone().expect("written"), Some(pa))
        .expect("tag");
    // B, a fresh chain with the same heights, and A's store: refused, B
    // untouched.
    let mut chain_b = SimulatedRegistry::new();
    chain_b.deployment = Some(pb);
    let foreign = RefCell::new(Some(store_a.clone()));
    let e = run_on(&mut chain_b, &d.join("b-book.json"), &foreign)
        .map(|_| ())
        .expect_err("another deployment's store");
    assert!(e.error.contains("review R10-1"), "{}", e.error);
    assert_eq!(chain_b.draw_count(&fact()).expect("count"), 0);
    // The untagged store on B: refused too.
    let old = RefCell::new(results.borrow().clone());
    let e = run_on(&mut chain_b, &d.join("b-book.json"), &old)
        .map(|_| ())
        .expect_err("an untagged store with a chain");
    assert!(e.error.contains("review R10-1"), "{}", e.error);
    // A's own store on A: read (the fact is settled).
    let own = RefCell::new(Some(store_a));
    let e = run_on(&mut chain_a, &d.join("a-book.json"), &own)
        .map(|_| ())
        .expect_err("settled");
    assert!(e.error.contains("already publishes"), "{}", e.error);
}

/// Review R10-3: a mistyped results store path never reads as a store
/// publishing nothing: a missing file is an error unless the operator
/// passes `--new-store` (the runners' `read_store`).
#[test]
fn r10_3_a_missing_results_store_needs_new_store() {
    let d = dir("r10-3");
    let missing = d.join("typo.json");
    let e = ObservationStore::read_file(&missing, false).expect_err("missing");
    assert!(e.contains("--new-store"), "{e}");
    assert_eq!(
        ObservationStore::read_file(&missing, true).expect("new"),
        None
    );
    let written = ObservationStore::to_json_with_attempts(&template(), &[], &[], None);
    std::fs::write(&missing, written.to_string()).expect("write");
    assert_eq!(
        ObservationStore::read_file(&missing, false).expect("exists"),
        Some(written)
    );
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------ review R11

/// The rehearsal anvils' registry (anvil account 1, nonce 0): every fresh
/// anvil has it at chain id 31337.
fn anvil_deployment(deploy_block: u8) -> RegistryDeployment {
    let h = "8464135c8f25da09e49bc8782676a84730c318bc";
    let mut registry = [0u8; 20];
    for (i, b) in registry.iter_mut().enumerate() {
        *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).expect("hex");
    }
    RegistryDeployment {
        chain_id: 31337,
        registry,
        deploy_block: Hash32([deploy_block; 32]),
    }
}

/// Review R11-1 (the reviewer's PoC, ported): every rehearsal anvil
/// deploys OnyxRegistry from anvil account 1 at nonce 0, so every fresh
/// anvil is (31337, 0x8464...318bc): another anvil's results store passed
/// the deployment tag check. Two simulated chains here are identical
/// (same heights, same beacons: even the beacon match of R10-1 (a) passes)
/// and differ only by the registry's deploy block hash, which the tag now
/// holds: another anvil's store is refused, and nothing is sent.
#[test]
fn r11_1_anvil_rehearsals_have_distinct_tags() {
    let (pa, pb) = (anvil_deployment(1), anvil_deployment(2));
    assert_eq!((pa.chain_id, pa.registry), (pb.chain_id, pb.registry));
    let sp = sp();
    let d = dir("r11-1");
    let mut chain_a = SimulatedRegistry::new();
    chain_a.deployment = Some(pa);
    let results = RefCell::new(None);
    run_on(&mut chain_a, &d.join("a-book.json"), &results).expect("A runs");
    let store_a = ObservationStore::tagged(results.borrow().clone().expect("written"), Some(pa))
        .expect("tag");
    let draws = |v: &Json, p| {
        ObservationStore::published_draws(v, &template(), &sp.subject, sp.window_start, p)
    };
    assert!(!draws(&store_a, Some(pa)).expect("its own").is_empty());
    let e = draws(&store_a, Some(pb)).expect_err("another anvil's store");
    assert!(e.to_string().contains("R11-1"), "{e}");
    // End to end on the second anvil: refused before anything is sent.
    let mut chain_b = SimulatedRegistry::new();
    chain_b.deployment = Some(pb);
    let foreign = RefCell::new(Some(store_a.clone()));
    let e = run_on(&mut chain_b, &d.join("b-book.json"), &foreign)
        .map(|_| ())
        .expect_err("another anvil's store");
    assert!(e.error.contains("R11-1"), "{}", e.error);
    assert_eq!(chain_b.draw_count(&fact()), Ok(0));
    // A store tagged before review R11 (chain and registry only) is refused
    // too; `tagged` (the `--tag-store` mode, after its chain check) adds the
    // deploy block, and only for the same chain and registry.
    let mut r10 = store_a.clone();
    r10["onchain"]
        .as_object_mut()
        .expect("tag")
        .remove("deployBlock");
    assert!(draws(&r10, Some(pa)).is_err(), "no deploy block");
    assert_eq!(
        ObservationStore::tagged(r10.clone(), Some(pa)).expect("retag"),
        store_a
    );
    let other = RegistryDeployment {
        registry: [0xb; 20],
        ..pa
    };
    assert!(ObservationStore::tagged(r10, Some(other)).is_err());
    assert!(ObservationStore::tagged(store_a, Some(pb)).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// Review R11-2: `--tag-store` is a mode of its own that tags a store only
/// after every draw it publishes matches the chain's `drawOf` (attempt,
/// beacon height, commitment, the same nonzero beacon); any mismatch tags
/// nothing and leaves the file as it was.
#[test]
fn r11_2_tag_store_checks_every_published_draw() {
    use onyx_source_fiscaldata::round::tag_store;
    let d = dir("r11-2");
    let path = d.join("store.json");
    let pa = anvil_deployment(1);
    let mut chain_a = SimulatedRegistry::new();
    chain_a.deployment = Some(pa);
    let results = RefCell::new(None);
    run_on(&mut chain_a, &d.join("book.json"), &results).expect("A settles");
    let untagged = results.borrow().clone().expect("written");
    let put = |v: &Json| std::fs::write(&path, v.to_string()).expect("write");
    let on_disk = || -> Json {
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json")
    };
    let refused = |chain: &SimulatedRegistry, want: &str| {
        put(&untagged);
        let e = tag_store(&path, chain, &store()).expect_err("refused");
        assert!(e.contains(want), "{e}");
        assert_eq!(on_disk(), untagged, "nothing tagged");
    };
    // A missing store, and a chain without a deployment: refused.
    let e = tag_store(&path, &chain_a, &store()).expect_err("missing");
    assert!(e.contains("no such results store"), "{e}");
    put(&untagged);
    let e = tag_store(&path, &SimulatedRegistry::new(), &store()).expect_err("no chain");
    assert!(e.contains("--rpc"), "{e}");
    // A chain without the draw.
    let mut empty = SimulatedRegistry::new();
    empty.deployment = Some(anvil_deployment(2));
    refused(&empty, "has no draw 0");
    // The store's draw on chain B with another commitment (another pool).
    let entry = ObservationStore::store_draws(&untagged, &template()).expect("draws")[0];
    let mut other = SimulatedRegistry::new();
    other.deployment = Some(anvil_deployment(3));
    let mut b = store();
    b.sync_draws(&sp(), 0).expect("sync");
    b.announce_draw(
        &sp(),
        &config(&sp()),
        &drifted_pool(),
        entry.draw.beacon_height,
        &other,
    )
    .expect("announce");
    let db = b.latest_draw(&sp()).expect("draw");
    other.announce(&db).expect("lands");
    other.fix_beacon(&db).expect("fixed");
    refused(&other, "commitment");
    // The same draw announced but not fixed yet: a zero beacon.
    let mut unfixed = SimulatedRegistry::new();
    unfixed.deployment = Some(anvil_deployment(4));
    let same = OnchainDraw {
        fact: entry.fact,
        attempt: 0,
        commit: entry.commit,
        beacon_height: entry.draw.beacon_height,
    };
    unfixed.announce(&same).expect("lands");
    refused(&unfixed, "beacon");
    // A store whose beacon differs from the chain's.
    let mut forged = untagged.clone();
    forged["rounds"][0]["draw"]["beacon"] = serde_json::json!(Hash32([7; 32]).to_string());
    put(&forged);
    let e = tag_store(&path, &chain_a, &store()).expect_err("another beacon");
    assert!(e.contains("beacon"), "{e}");
    assert_eq!(on_disk(), forged);
    // Its own chain: checked (one draw) and tagged; a run then reads it.
    put(&untagged);
    assert_eq!(tag_store(&path, &chain_a, &store()), Ok(1));
    let tagged = on_disk();
    assert_eq!(
        ObservationStore::tag_of(&tagged).expect("tag"),
        Some(onyx_api::observations::StoreTag::of(pa))
    );
    let sp = sp();
    assert_eq!(
        ObservationStore::published_draws(
            &tagged,
            &template(),
            &sp.subject,
            sp.window_start,
            Some(pa)
        )
        .expect("read")
        .len(),
        1
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// Review R11-3: `--new-store` left on after the first run, with a lost
/// book and a missing (or mistyped) store: refused when the chain already
/// shows draws of the fact and the book holds none; a real first run, and
/// a first run whose book holds its draw (stopped before the store was
/// written), go on.
#[test]
fn r11_3_new_store_is_refused_once_the_chain_has_draws() {
    use onyx_source_fiscaldata::round::check_new_store;
    let rec = recorded();
    let check = |book: &ObservationResults, chain: &SimulatedRegistry| {
        check_new_store(
            book,
            &rec.record_date,
            RECORDED_WINDOW_START,
            chain,
            &ChainWait::NOW,
        )
    };
    let fresh = SimulatedRegistry::new();
    assert_eq!(check(&store(), &fresh), Ok(()), "a first run");
    // The book announced draw 0 and stopped before writing the store.
    let mut chain = SimulatedRegistry::new();
    let mut held = store();
    held.announce_draw(&sp(), &config(&sp()), &rehearsal_pool(), 3, &chain)
        .expect("announce");
    chain
        .announce(&held.latest_draw(&sp()).expect("draw"))
        .expect("lands");
    assert_eq!(check(&held, &chain), Ok(()), "the book holds its draw");
    // A settled fact, its book lost, `--new-store` with no store file.
    let d = dir("r11-3");
    let mut settled = SimulatedRegistry::new();
    run_on(&mut settled, &d.join("book.json"), &RefCell::new(None)).expect("settles");
    let e = check(&store(), &settled).expect_err("not a first run");
    assert!(e.contains("review R11-3"), "{e}");
    let e = check(&store(), &chain).expect_err("an unheld chain draw");
    assert!(e.contains("review R11-3"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

/// A [`SimulatedRegistry`] behind a node that shows a draw's beacon as
/// zero (before its `fixBeacon`) for the next `zero` `drawOf` reads.
struct ZeroBeacon {
    inner: SimulatedRegistry,
    zero: Cell<u32>,
    /// Review R12: `drawCount` at `latest` answers 0 (a node behind every
    /// announcement); final-block reads are the chain's.
    count_zero: bool,
}

impl ChainView for ZeroBeacon {
    fn height(&self) -> Result<u64, String> {
        self.inner.height()
    }
    fn block_hash(&self, h: u64) -> Result<Option<Hash32>, String> {
        self.inner.block_hash(h)
    }
}

impl DrawChain for ZeroBeacon {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        if self.count_zero {
            return Ok(0);
        }
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
    fn draw_of(&self, f: &OnchainFact, a: u32) -> Result<Option<DrawRecord>, String> {
        let r = self.inner.draw_of(f, a)?;
        let n = self.zero.get();
        if n == 0 {
            return Ok(r);
        }
        self.zero.set(n - 1);
        Ok(r.map(|r| DrawRecord {
            beacon: Hash32::ZERO,
            ..r
        }))
    }
    fn draw_of_final(&self, f: &OnchainFact, a: u32) -> Result<(u64, Option<DrawRecord>), String> {
        self.inner.draw_of_final(f, a)
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
        self.inner.post_commit(f, a, o, r, c, s)
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
        self.inner.post_receipt(f, a, o, r, c, s)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        self.inner.log_block()
    }
    fn posted_commit(
        &self,
        b: u64,
        f: &OnchainFact,
        a: u32,
        o: &MinerAddress,
        r: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        self.inner.posted_commit(b, f, a, o, r)
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        self.inner.transcript_complete(q)
    }
}

/// Review R11-4: a lost book, the results store publishing draw 0 (its
/// beacon fixed), and a node that still shows draw 0's beacon as zero.
/// Before the fix the run stopped at once with "is the results store of
/// another registry?"; now the draw is read again (`wait.retry`): once the
/// node catches up the run follows the published draw and draws 1; a node
/// that never does stops the run with a message naming both causes.
#[test]
fn r11_4_a_lagging_zero_beacon_is_read_again() {
    let sp = sp();
    let cfg = config(&sp);
    // Draw 0 opened (its beacon fixed) and published by a book since lost.
    let mut base = SimulatedRegistry::new();
    let mut a = store();
    a.note_published_draws(&sp, &[]).expect("noted");
    let (round0, _) = draw_and_open(
        &mut a,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut base,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0 opens");
    let seed0 = round0.draw();
    assert_ne!(seed0.beacon, Hash32::ZERO);
    let run = |zero: u32| {
        let mut chain = ZeroBeacon {
            inner: base.clone(),
            zero: Cell::new(zero),
            count_zero: false,
        };
        let mut c = store();
        c.note_published_draws(&sp, &[seed0]).expect("noted");
        let r = draw_and_open(
            &mut c,
            sp.clone(),
            cfg,
            &rehearsal_pool(),
            &mut chain,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .map(|(round, _)| round.draw().attempt);
        (r, chain.inner.draw_count(&fact()).expect("count"))
    };
    // Lagging for the first read and one retry: followed, draw 1 opens.
    assert_eq!(run(2), (Ok(1), 2));
    // Never catching up: stopped, nothing drawn past, both causes named.
    let (e, n) = run(1_000);
    let e = e.expect_err("stopped");
    assert!(e.contains("has not seen fixBeacon"), "{e}");
    assert!(e.contains("another registry"), "{e}");
    assert_eq!(n, 1, "draw 0 is never drawn past");
}

/// Review R12 (REVLOG-R12-1): `--tag-store` checked only the draws a store
/// publishes, so an EMPTY store (a lost or mistyped path) was tagged on a
/// chain that already holds draws of its fact. Now the chain's `drawCount`
/// may not pass the store's latest draw nor the book's held draw; and
/// (REVLOG-R12-3) a store the tag leaves unchanged is not rewritten.
#[test]
fn r12_1_tag_store_refuses_a_chain_past_the_store() {
    use onyx_source_fiscaldata::round::tag_store;
    let d = dir("r12-1");
    let path = d.join("store.json");
    let mut chain = SimulatedRegistry::new();
    chain.deployment = Some(anvil_deployment(1));
    let results = RefCell::new(None);
    run_on(&mut chain, &d.join("book.json"), &results).expect("draw 0 settles");
    assert_eq!(chain.draw_count(&fact()), Ok(1));
    let on_disk = || std::fs::read_to_string(&path).expect("read");
    let empty = ObservationStore::to_json_with_attempts(&template(), &[], &[], None).to_string();
    std::fs::write(&path, &empty).expect("write");
    let e = tag_store(&path, &chain, &store()).expect_err("an empty store on a drawn fact");
    assert!(e.contains("review R12"), "{e}");
    assert_eq!(on_disk(), empty, "nothing tagged");
    // A book holding draw 0 (a first run stopped before its store write):
    // the chain is not past it, so the empty store is tagged.
    let mut held = store();
    held.announce_draw(
        &sp(),
        &config(&sp()),
        &rehearsal_pool(),
        3,
        &SimulatedRegistry::new(),
    )
    .expect("announce");
    assert_eq!(tag_store(&path, &chain, &held), Ok(0));
    let tagged = on_disk();
    assert_ne!(tagged, empty);
    // Its settled store: tagged. Tagged again (compact on disk): unchanged,
    // so not rewritten.
    let full = results.borrow().clone().expect("written");
    std::fs::write(&path, full.to_string()).expect("write");
    assert_eq!(tag_store(&path, &chain, &store()), Ok(1));
    let compact = on_disk().parse::<Json>().expect("json").to_string();
    std::fs::write(&path, &compact).expect("write");
    assert_eq!(tag_store(&path, &chain, &store()), Ok(1));
    assert_eq!(on_disk(), compact, "an unchanged store is not rewritten");
    let _ = std::fs::remove_dir_all(&d);
}

/// Review R12 (REVLOG-R12-2): `--new-store` let a book through that held
/// ANY draw, and read `drawCount` at `latest` only. Now the book must hold
/// the chain's latest draw, and the count is also read at a final block:
/// a node answering 0 at `latest` does not let a lost book through.
#[test]
fn r12_2_new_store_needs_the_chains_latest_draw_in_the_book() {
    use onyx_coordinator::observe::new_store_allowed;
    let sp = sp();
    let cfg = config(&sp);
    // Draw 0 opened by book `a`; another book follows the chain to draw 1.
    let mut chain = SimulatedRegistry::new();
    let mut a = store();
    a.note_published_draws(&sp, &[]).expect("noted");
    let (round0, _) = draw_and_open(
        &mut a,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 0 opens");
    // Book `a` holds the chain's latest draw: a first run, allowed.
    assert_eq!(new_store_allowed(&a, &sp, &chain, &ChainWait::NOW), Ok(()));
    let mut b = store();
    b.note_published_draws(&sp, &[round0.draw()])
        .expect("noted");
    draw_and_open(
        &mut b,
        sp.clone(),
        cfg,
        &rehearsal_pool(),
        &mut chain,
        &ChainWait::NOW,
        &mut |_| Ok(()),
    )
    .expect("draw 1 opens");
    assert_eq!(chain.draw_count(&fact()), Ok(2));
    let e = new_store_allowed(&a, &sp, &chain, &ChainWait::NOW).expect_err("holds draw 0 of 2");
    assert!(e.contains("R12") && e.contains("draw 0"), "{e}");
    assert_eq!(new_store_allowed(&b, &sp, &chain, &ChainWait::NOW), Ok(()));
    // A node answering drawCount 0 at `latest`: the final block shows 2.
    let lagging = ZeroBeacon {
        inner: chain.clone(),
        zero: Cell::new(0),
        count_zero: true,
    };
    let e =
        new_store_allowed(&store(), &sp, &lagging, &ChainWait::NOW).expect_err("a lagging count");
    assert!(e.contains("2 draw(s)"), "{e}");
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
