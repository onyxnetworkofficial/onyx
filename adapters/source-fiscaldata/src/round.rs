//! Level-2 round runner (feature `round`) for Debt to the Penny: the shared
//! runner ([`onyx_level2`], NS3) with this adapter's template, fetcher and
//! registry ([`FiscalData`]). Several observers fetch the same record, commit,
//! reveal, the round settles in an [`ObservationResults`] store, and the one
//! claim it backs goes through `accept_claim`.
//!
//! This is a rehearsal on one machine: the observers are simulated (fixed
//! addresses and salts derived from their numbers, so the run is reproducible),
//! but each one's page bytes and fetch time are a real HTTPS response (live
//! or recorded).

use crate::{registry, spec, template, FiscalDataFetcher};
use onyx_api::observations::Attempt;
use onyx_coordinator::keyfile::ReceiptKey;
use onyx_coordinator::keyfile::RevealKey;
use onyx_coordinator::observe::{
    ChainWait, Clock, DrawChain, DrawRecord, ObservationResults, Observer, ObserverSig,
    RevealInbox, SignedReveal,
};
use onyx_core::{FeedTemplate, Fetcher, Hash32, ObservationSpec, StaticRegistry};
use onyx_level2 as l2;
use serde_json::Value as Json;

pub use l2::{
    attempt, config, decimal, drawn_round, parse_pool, transcript, transcript_query, Observers,
    Page, RoundRun, RunError, Unfinished, POOL_SIZE,
};

/// This adapter as a level-2 source of the shared runner.
#[derive(Clone, Copy, Debug, Default)]
pub struct FiscalData;

impl l2::Source for FiscalData {
    fn template(&self) -> FeedTemplate {
        template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &FiscalDataFetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        spec(date, window_start)
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        registry(date)
    }
    fn register_note(&self) -> &'static str {
        "Debt to the Penny: rights reviewed (fixtures/PROVENANCE.md), feed pinned"
    }
    fn key_label(&self) -> &'static str {
        "fiscaldata rehearsal"
    }
    fn shown(&self) -> l2::Shown {
        l2::Shown {
            name: crate::SOURCE_NAME,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: "USD",
        }
    }
    fn latest(&self, window_start: u64) -> Option<String> {
        crate::latest_record_date(window_start)
    }
}

/// One of [`crate::series::ALL`] as a level-2 source (north-star loop NL25).
#[derive(Clone, Copy, Debug)]
pub struct FiscalSeries(pub &'static crate::series::Series);

impl l2::Source for FiscalSeries {
    fn template(&self) -> FeedTemplate {
        self.0.template()
    }
    fn fetcher(&self) -> &dyn Fetcher {
        &crate::series::SeriesFetcher
    }
    fn spec(&self, date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        self.0.spec(date, window_start)
    }
    fn registry(&self, date: &str) -> StaticRegistry {
        self.0.registry(date)
    }
    fn register_note(&self) -> &'static str {
        self.0.note
    }
    fn key_label(&self) -> &'static str {
        "fiscaldata series rehearsal"
    }
    fn shown(&self) -> l2::Shown {
        l2::Shown {
            name: self.0.name,
            terms_url: crate::TERMS_URL,
            terms_quote: crate::TERMS_QUOTE,
            unit: self.0.unit,
        }
    }
    fn latest(&self, window_start: u64) -> Option<String> {
        crate::latest_record_date(window_start)
    }
}

/// The source the live examples run: Debt to the Penny, or with `ONYX_L2_SERIES=<id>` one of
/// [`crate::series::ALL`] (`dts.tga_close`).
pub fn from_env() -> Result<Box<dyn l2::Source>, String> {
    match std::env::var("ONYX_L2_SERIES") {
        Err(_) => Ok(Box::new(FiscalData)),
        Ok(id) if id.is_empty() => Ok(Box::new(FiscalData)),
        Ok(id) => crate::series::by_id(&id)
            .map(|s| Box::new(FiscalSeries(s)) as Box<dyn l2::Source>)
            .ok_or_else(|| format!("ONYX_L2_SERIES={id}: not a series of this source")),
    }
}

/// A store with this metric's template registered ([`l2::store`]).
pub fn store() -> ObservationResults {
    l2::store(&FiscalData)
}

/// [`l2::run_and_save`] for this source.
#[allow(clippy::too_many_arguments)]
pub fn run_and_save(
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
    l2::run_and_save(
        &FiscalData,
        record_date,
        window_start,
        pages,
        store,
        chain,
        book,
        results,
        wait,
        clock,
        inbox,
        publish,
        publish_failed,
    )
}

/// [`l2::run_and_save_with`] for this source.
#[allow(clippy::too_many_arguments)]
pub fn run_and_save_with(
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
    l2::run_and_save_with(
        &FiscalData,
        record_date,
        window_start,
        observers,
        store,
        chain,
        book,
        results,
        wait,
        clock,
        inbox,
        receipt_key,
        reveal_key,
        publish,
        publish_failed,
    )
}

/// [`l2::note_results`] for this source.
pub fn note_results(
    store: &mut ObservationResults,
    sp: &ObservationSpec,
    results: Option<&Json>,
    onchain: Option<onyx_coordinator::observe::RegistryDeployment>,
) -> Result<(), String> {
    l2::note_results(&FiscalData, store, sp, results, onchain)
}

/// [`l2::check_new_store`] for this source.
pub fn check_new_store(
    store: &ObservationResults,
    record_date: &str,
    window_start: u64,
    chain: &dyn DrawChain,
    wait: &ChainWait,
) -> Result<(), String> {
    l2::check_new_store(&FiscalData, store, record_date, window_start, chain, wait)
}

/// Review R11-2: `--tag-store`: the results store at `path` is tagged with
/// `chain`'s deployment once every draw it publishes matches the chain
/// ([`onyx_api::observations::ObservationStore::tag_files`]). Returns how
/// many draws it publishes. Review R12: `book` (the runner's book, locked
/// by the caller) gives the recorded fact and the draw it holds: the chain
/// may show no draw past both the store's and the book's.
pub fn tag_store(
    path: &std::path::Path,
    chain: &dyn DrawChain,
    book: &ObservationResults,
) -> Result<usize, String> {
    let sp = spec(&crate::recorded().record_date, crate::RECORDED_WINDOW_START)?;
    let fact = book.onchain_fact(&sp).map_err(|e| e.to_string())?;
    let held = book.held_draw(&sp).map(|d| d.attempt);
    let n = onyx_api::observations::ObservationStore::tag_files(
        &[(path.to_path_buf(), template(), vec![(fact, held)])],
        chain,
    )?;
    Ok(n.iter().sum())
}

/// The recorded round's draw as the rehearsal's local test chain (anvil)
/// recorded it (`scripts/level2-rehearsal.sh`), for replaying the recorded
/// round without that chain (L2RND-R1).
pub fn recorded_draw() -> DrawRecord {
    let v = serde_json::from_str(include_str!(
        "../fixtures/debt-to-penny-2026-10-08/draw.json"
    ))
    .expect("committed draw record parses");
    DrawRecord::from_json(&v).expect("committed draw record is well formed")
}

/// Rehearsal key of observer `i` ([`l2::observer_key`], label
/// "fiscaldata rehearsal"): a FAKE test key that holds nothing on any network.
pub fn observer_key(i: u8) -> [u8; 32] {
    l2::observer_key(&FiscalData, i)
}

/// Rehearsal address of observer `i` (its key's address).
pub fn observer_address(i: u8) -> [u8; 20] {
    l2::observer_address(&FiscalData, i)
}

/// The simulated pool: [`POOL_SIZE`] bonded observers, ascending by address.
pub fn rehearsal_pool() -> Vec<Observer> {
    l2::rehearsal_pool(&FiscalData)
}

/// [`l2::observer_turn`] for this source.
pub fn observer_turn(
    spec: &ObservationSpec,
    round_id: &Hash32,
    attempt: u32,
    secret: &[u8; 32],
    page: &Page,
) -> Result<(Hash32, ObserverSig, SignedReveal), String> {
    l2::observer_turn(&FiscalData, spec, round_id, attempt, secret, page)
}

/// [`l2::run`] for this source.
pub fn run(
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
) -> Result<RoundRun, RunError> {
    l2::run(&FiscalData, record_date, window_start, pages, store, chain)
}

/// [`l2::run_waiting`] for this source.
pub fn run_waiting(
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    l2::run_waiting(
        &FiscalData,
        record_date,
        window_start,
        pages,
        store,
        chain,
        wait,
    )
}

/// [`l2::run_receipted`] for this source.
pub fn run_receipted(
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    receipt_key: &ReceiptKey,
) -> Result<RoundRun, RunError> {
    l2::run_receipted(
        &FiscalData,
        record_date,
        window_start,
        pages,
        store,
        chain,
        receipt_key,
    )
}

/// [`l2::run_clocked`] for this source.
pub fn run_clocked(
    record_date: &str,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    clock: &dyn Clock,
) -> Result<RoundRun, RunError> {
    l2::run_clocked(
        &FiscalData,
        record_date,
        window_start,
        pages,
        store,
        chain,
        wait,
        clock,
    )
}

/// [`l2::finish`] for this source.
pub fn finish(
    record_date: &str,
    store: &mut ObservationResults,
    u: Unfinished,
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
) -> Result<RoundRun, RunError> {
    l2::finish(&FiscalData, record_date, store, u, chain, wait)
}

/// [`l2::results_store`] for this source.
pub fn results_store(run: &RoundRun) -> Result<Json, String> {
    l2::results_store(&FiscalData, run)
}

/// [`l2::append_results`] for this source.
pub fn append_results(prev: Option<&Json>, run: &RoundRun) -> Result<Json, String> {
    l2::append_results(&FiscalData, prev, run)
}

/// [`l2::append_attempt`] for this source.
pub fn append_attempt(prev: Option<&Json>, a: &Attempt) -> Result<Json, String> {
    l2::append_attempt(&FiscalData, prev, a)
}

/// [`l2::summary`] for this source.
pub fn summary(
    run: &RoundRun,
    record_date: &str,
    http_dates: &[String],
    body: &[u8],
    sealed: bool,
) -> Json {
    l2::summary(&FiscalData, run, record_date, http_dates, body, sealed)
}
