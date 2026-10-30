//! Level-2 round runner for any dated feed (NS3; moved from the Treasury
//! adapter's `round` module, which now calls it): several observers fetch the
//! same record, commit, reveal, the round settles in an
//! [`ObservationResults`] store, and the one claim it backs goes through
//! `accept_claim`. Each source supplies what is its own through [`Source`]:
//! the feed template, the fetcher, the registry entries and the names shown.
//!
//! This is a rehearsal on one machine: the observers are simulated (fixed
//! addresses and salts derived from their numbers, so the run is reproducible),
//! but each one's page bytes and fetch time are a real HTTPS response (live
//! or recorded). Independent observer processes use the same functions
//! ([`Observers::Independent`], [`drawn_round`], [`observer_turn`]).

use onyx_api::observations::{Attempt, SealedLog};
use onyx_coordinator::keyfile::{ReceiptKey, RevealKey};
use onyx_coordinator::observe::{
    absorb_posted_commits, cfg_hash, check_commit_allowed, draw_and_open, draw_commit,
    finish_reveals_sealed, keep_taking_reveals, random_salt, receive_reveals, record_round,
    sign_commit, sign_reveal, test_keys, ChainWait, Clock, DrawChain, DrawRecord, DrawSeed,
    NoInbox, ObservationOutcome, ObservationResults, ObservationRound, ObservationSettlement,
    ObserveConfig, Observer, ObserverSig, ObserverStatus, ReplayClock, RevealInbox, SignedReveal,
    UnfinishedRound,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    observation_commit, observe, Answer, Claim, Hash32, Observation, ObservationSpec, ProofLevel,
    Snapshot, SourceRef, Value, VerificationMethod,
};
use onyx_core::{FeedTemplate, Fetcher, StaticRegistry};

pub mod aligned;
pub mod keyed;
pub mod live;
pub mod manifest;
pub mod markets;
pub mod rehearse;
use serde_json::{json, Value as Json};

/// What a level-2 source supplies to the runner. Everything else (the draw,
/// commits, reveals, receipts, settlement, the results store) is the same for
/// every source.
pub trait Source {
    /// The metric's one canonical feed (L2SRC-R2-F1).
    fn template(&self) -> FeedTemplate;
    /// The one fetcher the template names.
    fn fetcher(&self) -> &dyn Fetcher;
    /// The feed instantiated for the subject of `date`, observed from
    /// `window_start`; refused when the date's rule does not allow it.
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String>;
    /// A registry that knows the metric, the dated subject and the source.
    fn registry(&self, date: &str) -> StaticRegistry;
    /// The reviewer's note when the template is registered.
    fn register_note(&self) -> &'static str;
    /// The label the rehearsal's FAKE observer keys are derived from
    /// (`observe::test_keys`).
    fn key_label(&self) -> &'static str;
    /// The source's name, terms page, quoted terms and the value's unit, for
    /// the website summary (constants of the adapter, never the summary JSON:
    /// L2SRC-R2-F3).
    fn shown(&self) -> Shown;
    /// `--record-date latest` of a live round (ROADMAP B2, B6): the newest
    /// date a window starting at `window_start` may observe, so the
    /// coordinator never picks which day is observed. Default: the newest of
    /// the last [`LATEST_LOOKBACK_DAYS`] days [`Self::spec`] accepts.
    fn latest(&self, window_start: u64) -> Option<String> {
        let today = i64::try_from(window_start / 86_400).ok()?;
        (0..=LATEST_LOOKBACK_DAYS)
            .map(|back| onyx_core::feed::civil_date(today - back))
            .find(|d| self.spec(d, window_start).is_ok())
    }
    /// The earliest time the source may show anything about `date`'s value
    /// (THREATS NL6-10): a market or a policy on it stops trading before
    /// then, less [`markets::CLOSE_MARGIN_SECS`]. Default: the start of
    /// `date` in UTC, since a day's count or reading starts to appear at the
    /// source during the day itself.
    fn earliest_public(&self, date: &str) -> Result<u64, String> {
        start_of_day(date)
    }
    /// How an observer fetches the source (CL3, ADR-0053 item 7): a manifest source's
    /// descriptor sets the cap and asks for JSON; an adapter's source reads at most
    /// [`onyx_fetch::MAX_BYTES_CEILING`] (1 MiB) with `Accept: */*`.
    fn fetch_limits(&self) -> onyx_fetch::Limits {
        onyx_fetch::Limits::observer_any()
    }
    /// Where a keyed source's key goes (ADR-0054, CL4); `None` for a public source (the
    /// default). A keyed source's observers fetch with their own key, at a private slot, and
    /// reveal a blinded page hash ([`keyed`]).
    fn key_placement(&self) -> Option<onyx_fetch::KeyPlacement> {
        None
    }
    /// A keyed source's seat rules: its source id and the hash of the terms a holder accepts
    /// ([`keyed::terms_hash`]); `None` for a public source.
    fn keyed_seat_rules(&self) -> Option<(String, Hash32)> {
        None
    }
    /// The round's timing and economics: [`config`], or [`keyed::keyed_config`] for a keyed
    /// source (`outlier_slash == miss_slash`, ADR-0054 item 7).
    fn observe_config(&self, spec: &ObservationSpec) -> ObserveConfig {
        if self.key_placement().is_some() {
            keyed::keyed_config(spec)
        } else {
            config(spec)
        }
    }
}

/// An observer's observation of `page` for `src`: [`observe`], and for a keyed source the
/// private form ([`keyed::private_observation`]: window-start time, blinded page hash).
pub fn observe_page(
    src: &dyn Source,
    spec: &ObservationSpec,
    page: &Page,
) -> Result<Observation, String> {
    let o = observe(src.fetcher(), spec, page.body.as_deref(), page.fetched_unix)
        .map_err(|e| format!("observe: {e}"))?;
    if src.key_placement().is_some() {
        keyed::private_observation(spec, o)
    } else {
        Ok(o)
    }
}

/// `date` (YYYY-MM-DD) at 00:00 UTC, in Unix seconds.
pub fn start_of_day(date: &str) -> Result<u64, String> {
    let day = onyx_core::feed::day_number(date).ok_or(format!("{date}: not a date"))?;
    u64::try_from(day)
        .map(|d| d * 86_400)
        .map_err(|_| format!("{date}: before 1970"))
}

/// How far back [`Source::latest`] looks by default (a daily rule's
/// `max_days` is at most this).
pub const LATEST_LOOKBACK_DAYS: i64 = 400;

/// What the website shows about a source.
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    /// Source name.
    pub name: &'static str,
    /// The terms page.
    pub terms_url: &'static str,
    /// The terms, quoted.
    pub terms_quote: &'static str,
    /// The value's unit.
    pub unit: &'static str,
}

/// The value's decimal places: the template's numeric scale (0 otherwise).
pub fn scale_of(t: &FeedTemplate) -> u8 {
    match t.kind {
        onyx_core::ObservationKind::Numeric { scale, .. } => scale,
        _ => 0,
    }
}

/// Who observes a round (ROADMAP B2).
#[derive(Clone, Copy, Debug)]
pub enum Observers<'a> {
    /// Simulated in this process, one per page, signing with the
    /// rehearsal pool's public test keys ([`rehearsal_pool`]): recorded
    /// rehearsals and tests.
    Simulated(&'a [Page]),
    /// Independent processes with their own keys, drawn from this bonded
    /// pool snapshot (examples/level2-observer.rs): each reads the draw from
    /// the registry itself, fetches the source itself, posts its own signed
    /// commit on chain and sends its signed reveal to the coordinator's
    /// inbox (`onyx_coordinator::observer_link`). The coordinator holds none
    /// of their keys: it absorbs their commits from the transcript log and
    /// takes their reveals from the inbox.
    Independent(&'a [Observer]),
}

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
    /// The round's sealed posts and dismissals (ROADMAP C11), and the attempt
    /// record a round without a value was recorded with on chain: what the
    /// results store serves with the round.
    pub sealed: SealedLog,
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

/// A store with `src`'s template registered (L2SRC-R2-F1). In the
/// rehearsal the adapter registers it itself, as the reviewer's admin path
/// would after the source review; the capability is dropped, so nothing else
/// can be registered in this store.
pub fn store(src: &dyn Source) -> ObservationResults {
    let (mut store, admin) = ObservationResults::new();
    store
        .register_feed(&admin, src.template(), src.register_note(), 0)
        .expect("the adapter's template is valid");
    store
}

/// [`run_clocked`] one round, saving the round book at `book` once the
/// draw is announced (before its beacon exists, L2RND-R1-F3), with each
/// reveal as it arrives and with the log snapshot (before any receipt,
/// reviews REVLOG-R3-1 and REVLOG-R4-3), and after a round that did not
/// settle (AUD-2R-F4). A settled round is handed to `publish` (write the
/// results store, durably) and only then dropped from the book and the book
/// saved (review REVLOG-R4-1): until then the book on disk holds the round
/// unsettled, so a crash before the results are written makes the next run
/// with this book publish the same round on the same draw, never announce a
/// new one. `publish` may run again for a round it already wrote (a crash
/// after the write): [`append_results`] adds nothing twice. A failed attempt goes to `publish_failed` the same way
/// (THREATS REVLOG-R4-5): saved in the book first, written, then dropped. The book is
/// read with [`ObservationResults::load_book`] before the first round.
/// `results` is the results store as written before this run (`None`: none
/// written yet): its draws of the fact are noted first
/// ([`note_results`], review R9-F1), so a lost book never adopts a chain
/// draw whose round is already published.
/// `inbox` (THREATS REVLOG-R5-1d): reveals observers send while the reveal
/// window is open by `clock`, read after the in-process ones and, when the
/// run resumes a round kept in the book, before it is finished
/// ([`keep_taking_reveals`]); a restart inside the window keeps taking
/// reveals. [`NoInbox`] when nothing sends any.
#[allow(clippy::too_many_arguments)]
pub fn run_and_save(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    book: &std::path::Path,
    results: Option<&Json>,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    publish: &mut dyn FnMut(&RoundRun) -> Result<(), String>,
    publish_failed: &mut dyn FnMut(&Attempt) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    run_and_save_with(
        src,
        record_date,
        window_start,
        Observers::Simulated(pages),
        store,
        chain,
        book,
        results,
        wait,
        clock,
        inbox,
        &ReceiptKey::public_test(),
        None,
        publish,
        publish_failed,
    )
}

/// [`run_and_save`] with its `observers` (simulated from pages, or
/// independent processes, ROADMAP B2), the deployment's `receipt_key`
/// (ROADMAP B3: the public test key only in local rehearsals,
/// [`ReceiptKey::for_chain`]) and the coordinator's `reveal_key` (ROADMAP C11:
/// the draw's sealed posts are opened with it; `None`: a coordinator without
/// one, whose draws take no sealed post).
#[allow(clippy::too_many_arguments)]
pub fn run_and_save_with(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    observers: Observers<'_>,
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    book: &std::path::Path,
    results: Option<&Json>,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    receipt_key: &ReceiptKey,
    reveal_key: Option<&RevealKey>,
    publish: &mut dyn FnMut(&RoundRun) -> Result<(), String>,
    publish_failed: &mut dyn FnMut(&Attempt) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    let sp = src.spec(record_date, window_start)?;
    let onchain = chain.deployment()?;
    note_results(src, store, &sp, results, onchain)?;
    let r = match run_with(
        src,
        record_date,
        window_start,
        observers,
        store,
        chain,
        wait,
        clock,
        inbox,
        receipt_key.secret(),
        reveal_key.map(RevealKey::secret),
        &mut |s| s.save_book(book),
    ) {
        Ok(r) => r,
        Err(mut e) => {
            // THREATS REVLOG-R4-5: the book is saved first, holding a failed
            // round (with its commits) as a failed attempt to publish; it
            // is written (`publish_failed`) and only then dropped from the
            // book. A crash in between publishes it again (idempotent).
            if let Err(se) = store.save_book(book) {
                e.error = format!("{}; the book was not saved: {se}", e.error);
                return Err(e);
            }
            if let Some(a) = e.attempt.as_deref() {
                match publish_failed(a) {
                    Ok(()) => {
                        drop_failed(store, &sp);
                        if let Err(se) = store.save_book(book) {
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
    store.save_book(book)?;
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
    src: &dyn Source,
    store: &mut ObservationResults,
    sp: &ObservationSpec,
    results: Option<&Json>,
    onchain: Option<onyx_coordinator::observe::RegistryDeployment>,
) -> Result<(), String> {
    let draws = match results {
        Some(v) => onyx_api::observations::ObservationStore::published_draws(
            v,
            &src.template(),
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

/// Review R11-3: `--new-store` found no results store file: refused when
/// the chain already shows draws of the fact and the book `store` holds
/// none ([`onyx_coordinator::observe::new_store_allowed`]).
pub fn check_new_store(
    src: &dyn Source,
    store: &ObservationResults,
    record_date: &str,
    window_start: u64,
    chain: &dyn DrawChain,
    wait: &ChainWait,
) -> Result<(), String> {
    let sp = src.spec(record_date, window_start)?;
    onyx_coordinator::observe::new_store_allowed(store, &sp, chain, wait)
}

/// Rehearsal key of observer `i`: a FAKE test key, `keccak256("onyx test
/// observer key " ‖ label ‖ " " ‖ i)` with the source's [`Source::key_label`]
/// (`observe::test_keys`). Anyone can recompute it; it holds nothing on any
/// network. Each simulated observer signs its own commit and reveal with it
/// (ATT-R1-F3).
pub fn observer_key(src: &dyn Source, i: u8) -> [u8; 32] {
    test_keys::secret(src.key_label(), u32::from(i))
}

/// Rehearsal address of observer `i` (its key's address).
pub fn observer_address(src: &dyn Source, i: u8) -> [u8; 20] {
    test_keys::address(src.key_label(), u32::from(i))
}

/// The rehearsal key of pool member `a`.
fn key_of(src: &dyn Source, a: &[u8; 20]) -> Option<[u8; 32]> {
    (1..=POOL_SIZE)
        .find(|i| observer_address(src, *i) == *a)
        .map(|i| observer_key(src, i))
}

/// Size of the simulated bonded observer pool the rehearsal draws from
/// (larger than the draw, L2-F2).
pub const POOL_SIZE: u8 = 9;
/// The simulated pool: [`POOL_SIZE`] bonded observers, ascending by address.
pub fn rehearsal_pool(src: &dyn Source) -> Vec<Observer> {
    let mut pool: Vec<Observer> = (1..=POOL_SIZE)
        .map(|i| Observer {
            address: observer_address(src, i),
            bond: 1_000,
        })
        .collect();
    pool.sort_by_key(|o| o.address);
    pool
}

/// A bonded pool snapshot from JSON `[{"address": "0x…", "bond": 1000}, …]`
/// (ROADMAP B2): the file the coordinator draws from and each independent
/// observer checks the registry's draw against (the draw commitment binds
/// its hash, so an observer holding another snapshot refuses to commit).
/// Sorted ascending by address; refused unless it is a valid pool
/// (`check_pool`: 1..=1024 distinct members).
pub fn parse_pool(text: &str) -> Result<Vec<Observer>, String> {
    let v: Json = serde_json::from_str(text).map_err(|e| format!("pool: {e}"))?;
    let mut pool = v
        .as_array()
        .ok_or("pool: a JSON array of {address, bond}")?
        .iter()
        .map(|m| {
            let a = m
                .get("address")
                .and_then(Json::as_str)
                .ok_or("pool: a member without an address")?;
            let address =
                onyx_coordinator::cast_chain::parse_addr(a).map_err(|e| format!("pool: {e}"))?;
            let bond = m
                .get("bond")
                .and_then(Json::as_u64)
                .ok_or("pool: a member without a whole-number bond")?;
            Ok(Observer {
                address,
                bond: u128::from(bond),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    pool.sort_by_key(|o| o.address);
    onyx_coordinator::observe::check_pool(&pool)
        .map_err(|e| format!("pool: {e} (empty, over 1,024 members, or a member twice)"))?;
    Ok(pool)
}

/// The round an independent observer joins (ROADMAP B2), computed by the
/// observer alone: its own pool snapshot, the feed's canonical config
/// ([`config`]) and the registry's `drawOf` it read itself. The draw must
/// commit to exactly this pool, spec and config, with its beacon fixed and
/// announced before its height ([`check_commit_allowed`], ATT-R1-F4);
/// anything else is an error and the observer does not commit. Returns the
/// draw seed, the drawn observers and the round id when `me` is drawn,
/// `None` when it is not.
pub fn drawn_round(
    spec: &ObservationSpec,
    pool: &[Observer],
    draw_of: &DrawRecord,
    me: &[u8; 20],
) -> Result<Option<(DrawSeed, Vec<Observer>, Hash32)>, String> {
    drawn_round_with(spec, &config(spec), pool, draw_of, me)
}

/// [`drawn_round`] under the source's own round config ([`Source::observe_config`]; a keyed
/// source's differs).
pub fn drawn_round_with(
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    pool: &[Observer],
    draw_of: &DrawRecord,
    me: &[u8; 20],
) -> Result<Option<(DrawSeed, Vec<Observer>, Hash32)>, String> {
    let cfg = *cfg;
    let seed = DrawSeed::for_pool(pool, draw_of.beacon_height, draw_of.beacon, draw_of.attempt);
    check_commit_allowed(
        &seed,
        &draw_commit(&seed.pool, &spec.id(), &cfg_hash(&cfg)),
        draw_of,
    )
    .map_err(|e| format!("the registry's draw is not one to commit to: {e}"))?;
    let observers = onyx_coordinator::observe::draw_observers(
        &spec.id(),
        pool,
        &seed,
        usize::from(spec.min_observers),
        cfg.outlier_slash,
    );
    if observers.is_empty() {
        return Err("the pool has too few bonded members for this draw".into());
    }
    if !observers.iter().any(|o| o.address == *me) {
        return Ok(None);
    }
    let id = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &seed);
    Ok(Some((seed, observers, id)))
}

/// An independent observer's commit and reveal (ROADMAP B2): its
/// observation of `page` (fetched by the observer itself, inside the window),
/// a fresh random salt (ATT-R2-F2) and both signatures with its own key.
/// Returns the commit, the commit's signature and the signed reveal.
pub fn observer_turn(
    src: &dyn Source,
    spec: &ObservationSpec,
    round_id: &Hash32,
    attempt: u32,
    secret: &[u8; 32],
    page: &Page,
) -> Result<(Hash32, ObserverSig, SignedReveal), String> {
    let me = onyx_coordinator::keyfile::key_address(secret).ok_or("not a valid secp256k1 key")?;
    let o = observe_page(src, spec, page)?;
    let salt = random_salt().map_err(|e| format!("salt: {e}"))?;
    let h = observation_commit(round_id, &me, &o, &salt);
    let cs = sign_commit(secret, round_id, attempt, &h).ok_or("bad key")?;
    let rs = sign_reveal(secret, round_id, attempt, &o, &salt).ok_or("bad key")?;
    Ok((h, cs, (me, o, salt, rs)))
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

/// Run one round for `record_date` over `pages` (one per observer) and
/// settle it in `store`, waiting on the chain per [`ChainWait::DEFAULT`],
/// on a [`ReplayClock`] (the recorded pages' timeline: each observer
/// reveals right after the commit phase).
pub fn run(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
) -> Result<RoundRun, RunError> {
    run_waiting(
        src,
        record_date,
        window_start,
        pages,
        store,
        chain,
        &ChainWait::DEFAULT,
    )
}

/// [`run`], waiting on the chain per `wait`.
pub fn run_waiting(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    run_clocked(
        src,
        record_date,
        window_start,
        pages,
        store,
        chain,
        wait,
        &ReplayClock::default(),
    )
}

/// [`run`] with the deployment's own `receipt_key` (ROADMAP B3; the deploy
/// run's level-2 round, examples/demo-local.rs).
pub fn run_receipted(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    receipt_key: &ReceiptKey,
) -> Result<RoundRun, RunError> {
    clocked_with(
        src,
        record_date,
        window_start,
        pages,
        store,
        chain,
        &ChainWait::DEFAULT,
        &ReplayClock::default(),
        receipt_key,
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
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
) -> Result<RoundRun, RunError> {
    clocked_with(
        src,
        record_date,
        window_start,
        pages,
        store,
        chain,
        wait,
        clock,
        &ReceiptKey::public_test(),
    )
}

#[allow(clippy::too_many_arguments)]
fn clocked_with(
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
    receipt_key: &ReceiptKey,
) -> Result<RoundRun, RunError> {
    let r = run_with(
        src,
        record_date,
        window_start,
        Observers::Simulated(pages),
        store,
        chain,
        wait,
        clock,
        &mut NoInbox,
        receipt_key.secret(),
        None,
        &mut |_| Ok(()),
    )
    .inspect_err(|e| {
        // Without a book the caller publishes the failed attempt it gets.
        if e.attempt.is_some() {
            if let Ok(sp) = src.spec(record_date, window_start) {
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
    src: &dyn Source,
    record_date: &str,
    window_start: u64,
    observers: Observers<'_>,
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
    inbox: &mut dyn RevealInbox,
    receipt_key: &[u8; 32],
    reveal_secret: Option<&[u8; 32]>,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<RoundRun, RunError> {
    let spec = src.spec(record_date, window_start)?;
    // THREATS REVLOG-R4-5: a failed attempt the book keeps (its write did
    // not happen, or the run stopped before the book dropped it) is
    // published first; its draw gets no successor until then.
    if let Some(u) = store.failed_attempt(&spec) {
        let a = Attempt::of_failed_round(&src.template(), &u.round)?;
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
        return finish_with(
            src,
            record_date,
            store,
            u,
            chain,
            wait,
            receipt_key,
            reveal_secret,
            save,
        );
    }
    let cfg = src.observe_config(&spec);
    let pool = match observers {
        Observers::Simulated(_) => rehearsal_pool(src),
        Observers::Independent(p) => p.to_vec(),
    };
    // The observers are drawn (L2-F2, L2RND-R1): the draw is announced on
    // chain before its beacon height, the book saved, the beacon fixed from
    // the chain's block hash, and the round opened with it.
    let (mut round, onchain_draw) =
        draw_and_open(store, spec.clone(), cfg, &pool, chain, wait, save)?;
    // ATT-R2-F3: from here the draw's beacon is fixed on chain, so a run
    // that fails still returns its opened round: published as an attempt
    // when it failed before any reveal, otherwise unfinished.
    let committed = match observers {
        Observers::Simulated(pages) => commit_phase(
            src,
            pages,
            store,
            &spec,
            cfg,
            &mut round,
            &onchain_draw,
            chain,
            wait,
        ),
        // ROADMAP B2: independent observers read the draw from the
        // registry, post their own commits on chain (absorbed from the
        // transcript log once the commit phase is final) and send their
        // reveals to `inbox`; nothing is handed over in process.
        Observers::Independent(_) => Ok(vec![]),
    };
    match committed {
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
                Ok(_) => finish_with(
                    src,
                    record_date,
                    store,
                    u,
                    chain,
                    wait,
                    receipt_key,
                    reveal_secret,
                    save,
                ),
                Err(error) => Err(unfinished(store, u, error)),
            }
        }
        Err(error) => Err(failed_before_reveals(
            src,
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
    src: &dyn Source,
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
        .and_then(|_| Attempt::of_failed_round(&src.template(), &round));
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
    src: &dyn Source,
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
        let o = observe_page(src, spec, page).map_err(|e| format!("observer {i}: {e}"))?;
        // Each observer draws its own random salt and keeps it (ATT-R2-F2).
        let s = random_salt().map_err(|e| format!("observer {i} salt: {e}"))?;
        // Each simulated observer signs with its own (fake) key.
        let sk = key_of(src, &a).ok_or_else(|| format!("observer {i}: not a pool member"))?;
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
    src: &dyn Source,
    record_date: &str,
    store: &mut ObservationResults,
    u: Unfinished,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    let r = finish_with(
        src,
        record_date,
        store,
        u,
        chain,
        wait,
        &test_keys::receipt_secret(),
        None,
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
    src: &dyn Source,
    record_date: &str,
    store: &mut ObservationResults,
    mut u: Unfinished,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    receipt_key: &[u8; 32],
    reveal_secret: Option<&[u8; 32]>,
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
            // (`u.reveals`), not at the receipt's. ROADMAP C11: a draw with
            // a reveal key has its sealed posts read and opened here too.
            finish_reveals_sealed(
                &mut u.round,
                chain,
                &fact,
                Some(&src.template()),
                receipt_key,
                reveal_secret,
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
        .and_then(|()| {
            record(
                src,
                record_date,
                store,
                &u.round,
                u.onchain_draw,
                chain,
                wait,
            )
        });
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
    src: &dyn Source,
    record_date: &str,
    store: &mut ObservationResults,
    round: &ObservationRound,
    onchain_draw: DrawRecord,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, String> {
    let spec = round.spec().clone();
    let cfg = round.config();
    let settlement = store
        .settle_once(round, cfg.reveal_close + 1)
        .map_err(|e| format!("settle: {e}"))?;
    // ROADMAP C11: recorded on chain before it is served (a chain that
    // records rounds: the live coordinator's), the record served with it.
    let fact = store.onchain_fact(&spec).map_err(|e| e.to_string())?;
    let digest = record_round(store, round, &settlement, chain, &fact, wait)?;
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
        sealed: SealedLog::of_round(round, digest),
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
        // Scale 0 is a whole number (`Value::Int`, R-VAL-3: `Fixed` needs a
        // scale of 1 or more).
        value: match scale_of(&src.template()) {
            0 => Value::Int(mantissa),
            scale => Value::Fixed { mantissa, scale },
        },
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
        rights: src.template().rights,
        evidence: vec![],
        confidence: None,
    };
    let level = store
        .accept_claim(&claim, &src.registry(record_date))
        .map_err(|e| format!("accept_claim: {e}"))?;
    if level != ProofLevel::ReObserved {
        return Err(format!("accepted at {level:?}, not level 2"));
    }
    run.claim = Some(claim);
    Ok(run)
}

/// The run's `transcriptComplete` query (ADR-0027 amendment 8) under the
/// registry fact key `registry_fact`: the round as it is published. The
/// coordinator asks it before `recordObservation` (review REVLOG HIGH-1).
pub fn transcript_query(
    run: &RoundRun,
    registry_fact: Hash32,
) -> onyx_coordinator::observe::TranscriptLogQuery {
    onyx_coordinator::observe::TranscriptLogQuery::of(
        registry_fact,
        &run.spec,
        &run.cfg,
        &run.draw,
        run.round_id,
        &run.observers,
        &run.commits,
        &run.reveals.iter().map(|(a, _, _, _)| *a).collect(),
    )
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
        sealed: run.sealed.clone(),
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
pub fn results_store(src: &dyn Source, run: &RoundRun) -> Result<Json, String> {
    append_results(src, None, run)
}

/// The results store `prev` (or a new one, for this metric's template) with the run's
/// round added: a value round as the fact's round, a round without a value
/// as an attempt (SEC-R2-F1), served free at
/// `/v1/observations/{metric}/attempts`. Earlier rounds are kept, never
/// replaced (ATT-R1-F1), so a re-draw's store still holds every earlier
/// draw's round and `verify_attempts_onchain` passes for the fact.
pub fn append_results(
    src: &dyn Source,
    prev: Option<&Json>,
    run: &RoundRun,
) -> Result<Json, String> {
    use onyx_api::observations::ObservationStore;
    let (rounds, attempts) = match run.settlement.outcome() {
        ObservationOutcome::Value(_) => (vec![transcript(run)?], vec![]),
        _ => (vec![], vec![attempt(run)]),
    };
    match prev {
        None => Ok(ObservationStore::to_json_with_attempts(
            &src.template(),
            &rounds,
            &attempts,
            None,
        )),
        Some(p) => ObservationStore::append(p, &src.template(), &rounds, &attempts)
            .map_err(|e| e.to_string()),
    }
}

/// The results store `prev` (or a new one) with `a` added as an attempt: the
/// opened round of a run that failed after its beacon was fixed
/// ([`RunError::attempt`], ATT-R2-F3).
pub fn append_attempt(src: &dyn Source, prev: Option<&Json>, a: &Attempt) -> Result<Json, String> {
    use onyx_api::observations::ObservationStore;
    let attempts = std::slice::from_ref(a);
    match prev {
        None => Ok(ObservationStore::to_json_with_attempts(
            &src.template(),
            &[],
            attempts,
            None,
        )),
        Some(p) => {
            ObservationStore::append(p, &src.template(), &[], attempts).map_err(|e| e.to_string())
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

/// Render `mantissa` at `scale` decimals exactly.
pub fn decimal(mantissa: i128, scale: u8) -> String {
    let neg = mantissa < 0;
    let digits = mantissa.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = padded.split_at(padded.len() - scale);
    let sign = if neg { "-" } else { "" };
    if scale == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    }
}

/// The website's summary of a run (all values from the run itself). With
/// `sealed` (ATT-R2-F1) the answer is withheld
/// ([`onyx_api::observations::seal_site_round`]): pass it for a round
/// without a value whose fact is not closed, as the free `/attempts` answer
/// seals it.
pub fn summary(
    src: &dyn Source,
    run: &RoundRun,
    record_date: &str,
    http_dates: &[String],
    body: &[u8],
    sealed: bool,
) -> Json {
    let value = match run.claim.as_ref().map(|c| &c.value) {
        Some(Value::Fixed { mantissa, scale }) => decimal(*mantissa, *scale),
        Some(Value::Int(n)) => n.to_string(),
        Some(other) => format!("{other:?}"),
        // ATT-R1-F1: a round without a value names its outcome.
        None => format!("{:?}", run.settlement.outcome()),
    };
    let statuses: Vec<Json> = run
        .settlement
        .statuses()
        .iter()
        .map(|(a, s)| json!({ "observer": hex0x(a), "status": format!("{s:?}") }))
        .collect();
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
        "rehearsal": "Observers simulated on one machine; each fetched the source itself over HTTPS.",
        "source": {
            "id": run.spec.rights.source.0,
            "name": src.shown().name,
            "terms_url": src.shown().terms_url,
            "terms_quote": src.shown().terms_quote,
            "license": run.spec.rights.license,
            "attribution": run.spec.rights.attribution,
        },
        "feed": {
            "metric": metric,
            "subject": subject,
            "record_date": record_date,
            "url": run.spec.url,
            "selector": run.spec.selector,
            "scale": scale_of(&src.template()),
            "window_start": run.spec.window_start,
            "window_end": run.spec.window_end,
            "min_observers": run.spec.min_observers,
            "spec_id": hex0x(&run.spec.id().0),
            "fetcher_id": hex0x(&run.spec.fetcher.0),
        },
        // What the browser needs to recompute the round id and result hash
        // (L2SRC-R1-F3): the round's timing and economics, and each
        // observer's bond.
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
        "unit": src.shown().unit,
        "observers": run.observers.len(),
        "agreed": agreed,
        "statuses": statuses,
        "reveals": reveals,
        "result_hash": hex0x(&run.settlement.result_hash().0),
        "proof_level": 2,
        "response_body": String::from_utf8_lossy(body),
    });
    // ADR-0054 items 2, 3 and 6: a keyed source's page is never published (it can echo the
    // key or the account), and its value carries the keyed evidence and the shown odds.
    if src.key_placement().is_some() {
        if let Some(m) = v.as_object_mut() {
            m.remove("response_body");
            m.insert(
                "keyed".into(),
                keyed::keyed_shown(run.pool.len(), run.spec.min_observers),
            );
        }
    }
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
