//! Run the recorded level-2 round and print (or write) the website summary.
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-round -- --book PATH [--out PATH] [--store PATH [--new-store]]
//!   [--rpc URL --registry ADDR --coordinator ADDR [--owner ADDR] --draw-out PATH]`
//!
//! `cargo run -p onyx-source-fiscaldata --example level2-round -- --tag-store --store PATH --book PATH --rpc URL --registry ADDR --coordinator ADDR`
//!
//! `--owner ADDR` registers the feed's metric first (an unlocked anvil account).
//!
//! `--book PATH` (required) keeps the round book (attempts used, rounds
//! without a value) across runs; it is saved once the draw is announced,
//! before its beacon exists (L2RND-R1-F3), and after the round.
//! A round the run could not finish (the chain could not be read, the
//! commit phase was not final yet) is kept in the book and finished by the
//! next run on the same draw (review REVLOG-R3-2).
//!
//! The draw is announced on an `OnyxRegistry` and its beacon fixed from the
//! chain (L2RND-R1). With `--rpc` that is a local anvil (an unlocked
//! coordinator account; `scripts/level2-rehearsal.sh` sets it up) and
//! `--draw-out` writes the registry's record of the draw. Without `--rpc`
//! the recorded draw (`fixtures/.../draw.json`) is replayed.
//!
//! `--store PATH` also writes the results store the query API serves
//! (`ONYX_OBSERVATIONS`, ADR-0027 amendment 4), atomically. A missing
//! `--store` file stops the run unless `--new-store` is given (the first
//! run for this fact and deployment): a mistyped path must never read as a
//! store publishing nothing (review R10-3). The store names the deployment
//! (`onchain`: chain id, registry and the registry's deploy block hash,
//! review R11-1) and a run refuses a store naming another, or naming none
//! while the run has a chain (review R10-1). `--new-store` is refused when
//! the chain already shows draws of the fact and the book holds none
//! (review R11-3: not a first run).
//!
//! `--tag-store` is a mode of its own (review R11-2): it tags a store
//! written before that tag existed (or tagged before review R11) with the
//! chain's deployment, once every draw the store publishes matches the
//! chain's `drawOf` (attempt, beacon height, commitment, the same nonzero
//! beacon), and exits; any mismatch tags nothing. It sends nothing. It
//! needs `--book` and holds the book's lock while it tags; the chain may
//! show no draw past both the store's latest and the book's held draw
//! (review R12). A settled round
//! stays in the book until that write is done (review REVLOG-R4-1): if the
//! run dies first, the next run with the same `--book` publishes the same
//! round on the same draw (a store already holding it is left as it is).
//!
//! The pages are recorded, so the coordinator's clock is replayed on their
//! timeline: each observer's reveal arrives right after the commit phase
//! and is checked there (review REVLOG-R4-3); receipts wait for the log
//! block as on a live chain. The replay is used without `--rpc` and on a
//! local anvil (chain id 31337); any other chain stamps reveals with the
//! system clock unless `--clock replay` is given (`--clock replay|system`,
//! review REVLOG-R5).
//!
//! `--receipt-key FILE` (ROADMAP B3, THREATS REVLOG-4): the deployment's
//! own hot receipt key (a 32-byte hex key file, mode 0600). Without it the
//! run uses the public test receipt key, and only without `--rpc` or on a
//! local anvil (chain id 31337). The key must differ from `--coordinator`,
//! `--owner`, `--reporter ADDR` and every pool member, or the run refuses to
//! start; a key that differs from the registry's `receiptSigner` is set
//! (rotated) before the next draw is announced.
//!
//! A failed round (ATT-R2-F3) is written to `--store` before the book drops
//! it (THREATS REVLOG-R4-5); a draw announced but not opened is resumed by
//! the next run on the same draw (review REVLOG-R5).

use onyx_api::observations::{Attempt, ObservationStore};
use onyx_coordinator::cast_chain::{
    clock_choice, parse_addr, receipt_key_from_args, CastChain, ClockChoice,
};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    lock_book, write_atomic, ChainWait, Clock, DrawChain, RecordedDraw, ReplayClock, SystemClock,
};
use onyx_source_fiscaldata::round::{
    append_attempt, append_results, check_new_store, recorded_draw, rehearsal_pool,
    run_and_save_with, store, summary, tag_store, Observers, Page, RoundRun,
};
use onyx_source_fiscaldata::{recorded, RECORDED_BODY, RECORDED_WINDOW_START};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
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
        return tag_mode(&args, flag("--store")?, flag("--book")?);
    }
    let book = flag("--book")?.ok_or("--book PATH is required: the round book (L2RND-R1-F3)")?;
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let dates: Vec<String> = rec.fetches.iter().map(|f| f.http_date.clone()).collect();
    // Review R9-F3: one runner per book (held until the process exits),
    // taken before anything is sent (review R10-4).
    let _lock = lock_book(std::path::Path::new(&book))?;
    let mut cast = CastChain::from_args(&args)?;
    // ROADMAP B3: the receipt key is checked before anything is sent.
    let pool: Vec<[u8; 20]> = rehearsal_pool().iter().map(|o| o.address).collect();
    let receipt_key = receipt_key_from_args(&args, cast.as_mut(), &pool)?;
    if let (Some(c), Some(owner)) = (&cast, flag("--owner")?) {
        c.register_metric(&parse_addr(&owner)?, &onyx_source_fiscaldata::template())?;
    }
    let chain_id = cast.as_ref().map(CastChain::chain_id).transpose()?;
    let (replay_clock, system_clock) = (ReplayClock::default(), SystemClock);
    let clock: &dyn Clock = match clock_choice(chain_id, flag("--clock")?.as_deref())? {
        ClockChoice::Replay => &replay_clock,
        ClockChoice::System => &system_clock,
    };
    // The replay is read only without a chain: a rehearsal writes it.
    let mut replay;
    let chain: &mut dyn DrawChain = match cast.as_mut() {
        Some(c) => c,
        None => {
            replay = RecordedDraw::new(recorded_draw());
            &mut replay
        }
    };
    let mut store = store();
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
            &store,
            &rec.record_date,
            RECORDED_WINDOW_START,
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
            let next = ObservationStore::tagged(append_results(prev.as_ref(), r)?, onchain)
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
            let next = ObservationStore::tagged(append_attempt(prev.as_ref(), a)?, onchain)
                .map_err(|e| format!("{path}: {e}"))?;
            let s = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
            write_atomic(std::path::Path::new(path), &format!("{s}\n"))?;
            eprintln!("wrote the failed round to the results store {path}");
        }
        Ok(())
    };
    let r = match run_and_save_with(
        &rec.record_date,
        RECORDED_WINDOW_START,
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
        &r,
        &rec.record_date,
        &dates,
        RECORDED_BODY,
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

/// Review R11-2: `--tag-store`, a mode of its own: check every draw the
/// store at `store_path` publishes against the chain, tag it, exit.
fn tag_mode(
    args: &[String],
    store_path: Option<String>,
    book: Option<String>,
) -> Result<(), String> {
    if args.iter().any(|a| a == "--new-store") {
        return Err("--tag-store tags an existing results store: drop --new-store".into());
    }
    let path = store_path.ok_or("--tag-store needs --store PATH (an existing results store)")?;
    // One runner per book: never tag a store a run is writing (review R12:
    // the book is required, and its held draw bounds the chain's count).
    let book = book.ok_or("--tag-store needs --book PATH: the runner's book, locked while the store is tagged (review R12)")?;
    let _lock = lock_book(std::path::Path::new(&book))?;
    let mut held = store();
    held.load_book(std::path::Path::new(&book))?;
    let cast = CastChain::from_args(args)?
        .ok_or("--tag-store needs --rpc: the store is tagged with the chain's deployment")?;
    let n = tag_store(std::path::Path::new(&path), &cast, &held)?;
    eprintln!(
        "checked the {n} draw(s) the results store {path} publishes against the chain and tagged it with this deployment; run again without --tag-store"
    );
    Ok(())
}

/// The results store at `path` (review R10-3: missing only with `--new-store`).
fn read_store(path: &str, new_store: bool) -> Result<Option<serde_json::Value>, String> {
    ObservationStore::read_file(std::path::Path::new(path), new_store)
}
