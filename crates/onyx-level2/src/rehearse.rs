//! A level-2 rehearsal of one recorded round, for any [`Source`] (NS3): the
//! body of the Treasury's `level2-round` example, shared so every daily source
//! runs the same draw on chain, writes the same results store and the same
//! website summary. See that example for the flags (`--book`, `--out`,
//! `--store`, `--new-store`, `--rpc`, `--registry`, `--coordinator`,
//! `--owner`, `--draw-out`, `--clock`, `--receipt-key`).

use crate::{
    append_attempt, append_results, check_new_store, rehearsal_pool, run_and_save_with, store,
    summary, Observers, Page, RoundRun, Source,
};
use onyx_api::observations::{Attempt, ObservationStore};
use onyx_coordinator::cast_chain::{
    clock_choice, parse_addr, receipt_key_from_args, CastChain, ClockChoice,
};
use onyx_coordinator::observe::{
    lock_book, write_atomic, ChainWait, Clock, DrawChain, DrawRecord, NoInbox, RecordedDraw,
    ReplayClock, SimulatedRegistry, SystemClock,
};

/// One recorded round: the day, its window, the observers' pages and the
/// server's `Date` headers, the response body, and (when a rehearsal on a
/// local chain wrote one) the draw record replayed without `--rpc`.
pub struct Recorded {
    /// The day the feed pins.
    pub day: String,
    /// The window the pages were fetched in.
    pub window_start: u64,
    /// One page per observer.
    pub pages: Vec<Page>,
    /// The server's `Date` header per page.
    pub http_dates: Vec<String>,
    /// The response body.
    pub body: Vec<u8>,
    /// The recorded draw, if any; without one and without `--rpc` the round
    /// runs on an in-memory registry ([`SimulatedRegistry`]).
    pub draw: Option<DrawRecord>,
}

/// Run `rec` for `src` with the command-line `args`.
pub fn main_for(src: &dyn Source, rec: Recorded, args: &[String]) -> Result<(), String> {
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| {
                args.get(i + 1)
                    .cloned()
                    .ok_or(format!("{name} needs a path"))
            })
            .transpose()
    };
    if args.iter().any(|a| a == "--tag-store") {
        return Err("--tag-store is the Treasury example's own mode".into());
    }
    let book = flag("--book")?.ok_or("--book PATH is required: the round book (L2RND-R1-F3)")?;
    let pages = rec.pages.clone();
    let dates = rec.http_dates.clone();
    // Review R9-F3: one runner per book (held until the process exits),
    // taken before anything is sent (review R10-4).
    let _lock = lock_book(std::path::Path::new(&book))?;
    let mut cast = CastChain::from_args(args)?;
    // ROADMAP B3: the receipt key is checked before anything is sent.
    let pool: Vec<[u8; 20]> = rehearsal_pool(src).iter().map(|o| o.address).collect();
    let receipt_key = receipt_key_from_args(args, cast.as_mut(), &pool)?;
    if let (Some(c), Some(owner)) = (&cast, flag("--owner")?) {
        c.register_metric(&parse_addr(&owner)?, &src.template())?;
    }
    let chain_id = cast.as_ref().map(CastChain::chain_id).transpose()?;
    let (replay_clock, system_clock) = (ReplayClock::default(), SystemClock);
    let clock: &dyn Clock = match clock_choice(chain_id, flag("--clock")?.as_deref())? {
        ClockChoice::Replay => &replay_clock,
        ClockChoice::System => &system_clock,
    };
    // The replay is read only without a chain: a rehearsal writes it.
    let (mut replay, mut simulated);
    let chain: &mut dyn DrawChain = match (cast.as_mut(), rec.draw) {
        (Some(c), _) => c,
        (None, Some(d)) => {
            replay = RecordedDraw::new(d);
            &mut replay
        }
        (None, None) => {
            simulated = SimulatedRegistry::new();
            &mut simulated
        }
    };
    let mut store = store(src);
    store.load_book(std::path::Path::new(&book))?;
    let store_path = flag("--store")?;
    let new_store = args.iter().any(|a| a == "--new-store");
    // Review R10-1, R11-1: the deployment every store this run writes names.
    let onchain = chain.deployment()?;
    // Review R9-F1: the results store as written so far, so a lost book
    // never adopts a draw whose round it already publishes.
    let written = match &store_path {
        Some(p) => read_store(p, new_store)?,
        None => None,
    };
    // Review R11-3: `--new-store` (no store file) only on a first run.
    if store_path.is_some() && written.is_none() {
        check_new_store(
            src,
            &store,
            &rec.day,
            rec.window_start,
            chain,
            &ChainWait::DEFAULT,
        )?;
    }
    // Review REVLOG-R4-1: the results store is written (atomically) before
    // the round leaves the book.
    let mut publish = |r: &RoundRun| -> Result<(), String> {
        if let Some(path) = &store_path {
            // ATT-R1-F1: an existing store is appended to, never replaced, so
            // a re-draw keeps every earlier draw's round.
            let prev = read_store(path, new_store)?;
            let next = ObservationStore::tagged(append_results(src, prev.as_ref(), r)?, onchain)
                .map_err(|e| format!("{path}: {e}"))?;
            let s = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
            write_atomic(std::path::Path::new(path), &format!("{s}\n"))?;
            eprintln!("wrote the results store {path}");
        }
        Ok(())
    };
    // ATT-R2-F3, THREATS REVLOG-R4-5: a round opened on a fixed beacon is
    // published as an attempt even when its run failed, before the book
    // drops it.
    let mut publish_failed = |a: &Attempt| -> Result<(), String> {
        if let Some(path) = &store_path {
            let prev = read_store(path, new_store)?;
            let next = ObservationStore::tagged(append_attempt(src, prev.as_ref(), a)?, onchain)
                .map_err(|e| format!("{path}: {e}"))?;
            let s = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
            write_atomic(std::path::Path::new(path), &format!("{s}\n"))?;
            eprintln!("wrote the failed round to the results store {path}");
        }
        Ok(())
    };
    let r = match run_and_save_with(
        src,
        &rec.day,
        rec.window_start,
        Observers::Simulated(&pages),
        &mut store,
        chain,
        std::path::Path::new(&book),
        written.as_ref(),
        &ChainWait::DEFAULT,
        clock,
        // Reveals sent again after a restart (REVLOG-R5-1d): the in-process
        // observers hand theirs over directly; independent observers (ROADMAP B2)
        // get an inbox of their own.
        &mut NoInbox,
        &receipt_key,
        // The recorded rehearsal's draws carry no reveal key (ROADMAP C11:
        // the live coordinator, examples/level2-coordinator.rs, has one).
        None,
        &mut publish,
        &mut publish_failed,
    ) {
        Ok(r) => r,
        Err(e) => {
            // Review REVLOG-R3-2: an unfinished round is kept in the
            // book (saved by `run_and_save`); the next run with this book
            // finishes it on the same draw instead of announcing a new one.
            if e.unfinished.is_some() {
                eprintln!("the round is unfinished and saved in the book {book}; run again with the same --book to finish it on the same draw");
            }
            return Err(e.error);
        }
    };
    if let Some(path) = flag("--draw-out")? {
        let s = serde_json::to_string(&r.onchain_draw.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(&path, format!("{s}\n")).map_err(|e| e.to_string())?;
        eprintln!("wrote the draw record {path}");
    }
    let text = serde_json::to_string_pretty(&summary(
        src,
        &r,
        &rec.day,
        &dates,
        &rec.body,
        // ATT-R2-F1: a round without a value is sealed (its fact is not
        // closed when the run ends).
        r.claim.is_none(),
    ))
    .map_err(|e| e.to_string())?;
    match flag("--out")? {
        Some(path) => {
            std::fs::write(&path, format!("{text}\n")).map_err(|e| e.to_string())?;
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// The results store at `path` (review R10-3: missing only with `--new-store`).
fn read_store(path: &str, new_store: bool) -> Result<Option<serde_json::Value>, String> {
    ObservationStore::read_file(std::path::Path::new(path), new_store)
}
