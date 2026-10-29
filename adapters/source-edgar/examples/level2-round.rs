//! Run the recorded EDGAR level-2 rounds (one per company) and print (or
//! write) the website summary.
//!
//! `cargo run -p onyx-source-edgar --example level2-round -- --book PATH [--out PATH] [--store-dir DIR [--new-store]]`
//!
//! `cargo run -p onyx-source-edgar --example level2-round -- --tag-store --store-dir DIR --book PATH --rpc URL --registry ADDR --coordinator ADDR`
//!
//! `--store-dir DIR` also writes one results store per company
//! (`DIR/<key>.json`) for the query API (`ONYX_OBSERVATIONS`), atomically.
//! A missing store file stops the run unless `--new-store` is given (the
//! first run for these facts and this deployment): a mistyped directory
//! must never read as stores publishing nothing (review R10-3). Each store
//! names the deployment (`onchain`: chain id, registry and the registry's
//! deploy block hash, review R11-1) and a run refuses a store naming
//! another, or naming none while the run has a chain (review R10-1).
//! `--new-store` is refused for a company whose fact the chain already
//! shows draws of while the book holds none (review R11-3: not a first
//! run).
//!
//! `--tag-store` is a mode of its own (review R11-2): it tags every
//! company's store, written before that tag existed (or tagged before
//! review R11), with the chain's deployment once every draw each publishes
//! matches the chain's `drawOf` (attempt, beacon height, commitment, the
//! same nonzero beacon), and exits; a missing store or any mismatch tags
//! none. It sends nothing. It needs `--book` and holds the book's lock
//! while it tags; the chain may show no draw past both a store's latest
//! and the book's held draw (review R12). A settled round stays in the book until its store is written (review
//! REVLOG-R4-1): if the run dies first, the next run with the same `--book`
//! publishes the same round on the same draw (a store already holding it is
//! left as it is).
//!
//! The pages are recorded, so the coordinator's clock is replayed on their
//! timeline: each observer's reveal arrives right after the commit phase
//! and is checked there (review REVLOG-R4-3); receipts wait for the log
//! block as on a live chain. The replay is used without `--rpc` and on a
//! local anvil (chain id 31337); any other chain stamps reveals with the
//! system clock unless `--clock replay` is given (`--clock replay|system`,
//! review REVLOG-R5).
//!
//! A failed round (ATT-R2-F3) is written to its results store before the
//! book drops it (THREATS REVLOG-R4-5); a draw announced but not opened is
//! resumed by the next run on the same draw (review REVLOG-R5).
//!
//! `--book PATH` (required) keeps the non-value round book across runs
//! (AUD-2-F9, AUD-2R-F4): it is read before the rounds (a missing file is a
//! first run; a torn one stops the start) and written atomically after every
//! round, so neither a restart nor a crash resets the cap of rounds without a
//! value. It is also saved once each draw is announced, before its beacon
//! exists (L2RND-R1-F3).
//! A round the run could not finish (the chain could not be read, the
//! commit phase was not final yet) is kept in the book and finished by the
//! next run on the same draw (review REVLOG-R3-2).
//!
//! Each draw is announced on an `OnyxRegistry` and its beacon fixed from the
//! chain (L2RND-R1). With `--rpc URL --registry ADDR --coordinator ADDR` that
//! is a local anvil (`scripts/level2-rehearsal.sh`), and `--draw-out-dir DIR`
//! writes each company's draw record to `DIR/<key>.json`. Without `--rpc`
//! each recorded draw (`fixtures/<key>-.../draw.json`) is replayed.
//!
//! `--receipt-key FILE` (ROADMAP B3, THREATS REVLOG-4): the deployment's own
//! hot receipt key; without it only a replay or a local anvil runs, on the
//! public test key. It must differ from `--coordinator`, `--owner`,
//! `--reporter ADDR` and every pool member, or the run refuses to start.

use onyx_api::observations::{Attempt, ObservationStore};
use onyx_coordinator::cast_chain::{
    clock_choice, parse_addr, receipt_key_from_args, CastChain, ClockChoice,
};
use onyx_coordinator::observe::NoInbox;
use onyx_coordinator::observe::{
    lock_book, write_atomic, ChainWait, Clock, DrawChain, RecordedDraw, ReplayClock, SystemClock,
};
use onyx_source_edgar::recorded;
use onyx_source_edgar::round::{
    append_attempt, append_results, check_new_store, load_book, recorded_draw, rehearsal_pool,
    run_and_save_with, site_summary, store, summary, tag_stores, Page, RoundRun,
};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let store_dir = args
        .iter()
        .position(|a| a == "--store-dir")
        .map(|i| args.get(i + 1).cloned().ok_or("--store-dir needs a path"))
        .transpose()?;
    let book = args
        .iter()
        .position(|a| a == "--book")
        .map(|i| args.get(i + 1).cloned().ok_or("--book needs a path"))
        .transpose()?;
    if args.iter().any(|a| a == "--tag-store") {
        return tag_mode(&args, store_dir, book);
    }
    let book = book.ok_or("--book PATH is required: the non-value round book (AUD-2R-F4)")?;
    let draw_dir = args
        .iter()
        .position(|a| a == "--draw-out-dir")
        .map(|i| {
            args.get(i + 1)
                .cloned()
                .ok_or("--draw-out-dir needs a path")
        })
        .transpose()?;
    // Review R9-F3: one runner per book (held until the process exits),
    // taken before anything is sent (review R10-4).
    let _lock = lock_book(std::path::Path::new(&book))?;
    let new_store = args.iter().any(|a| a == "--new-store");
    let mut cast = CastChain::from_args(&args)?;
    // ROADMAP B3: the receipt key (`--receipt-key FILE`, else the public
    // test key on a replay or a local anvil only) is checked before
    // anything is sent.
    let pool: Vec<[u8; 20]> = rehearsal_pool().iter().map(|o| o.address).collect();
    let receipt_key = receipt_key_from_args(&args, cast.as_mut(), &pool)?;
    // `--owner ADDR` registers each company's metric first (an unlocked anvil account).
    if let (Some(c), Some(i)) = (&cast, args.iter().position(|a| a == "--owner")) {
        let o = args.get(i + 1).ok_or("--owner needs an address")?;
        let owner = parse_addr(o)?;
        for co in &onyx_source_edgar::COMPANIES {
            c.register_metric(&owner, &onyx_source_edgar::template(co))?;
        }
    }
    let clock_flag = args
        .iter()
        .position(|a| a == "--clock")
        .map(|i| {
            args.get(i + 1)
                .cloned()
                .ok_or("--clock needs replay or system")
        })
        .transpose()?;
    let chain_id = cast.as_ref().map(CastChain::chain_id).transpose()?;
    let (replay_clock, system_clock) = (ReplayClock::default(), SystemClock);
    let clock: &dyn Clock = match clock_choice(chain_id, clock_flag.as_deref())? {
        ClockChoice::Replay => &replay_clock,
        ClockChoice::System => &system_clock,
    };
    let mut s = store();
    load_book(&mut s, &book)?;
    let mut rounds = Vec::new();
    for rec in recorded() {
        let pages: Vec<Page> = rec
            .fetches
            .iter()
            .map(|f| Page {
                fetched_unix: f.fetched_unix,
                body: Some(f.body.clone()),
            })
            .collect();
        let dates: Vec<String> = rec.fetches.iter().map(|f| f.http_date.clone()).collect();
        // The replay is read only without a chain: a rehearsal writes it.
        let mut replay;
        let chain: &mut dyn DrawChain = match cast.as_mut() {
            Some(c) => c,
            None => {
                replay = RecordedDraw::new(recorded_draw(rec.company.key));
                &mut replay
            }
        };
        // Review R10-1, R11-1: the deployment every store this run writes
        // names.
        let onchain = chain.deployment()?;
        // Review REVLOG-R4-1: the company's results store is written
        // (atomically) before its round leaves the book.
        let mut publish = |r: &RoundRun| -> Result<(), String> {
            if let Some(dir) = &store_dir {
                let path = format!("{dir}/{}.json", rec.company.key);
                // ATT-R1-F1: an existing store is appended to, never replaced.
                let prev = read_store(&path, new_store)?;
                let next = ObservationStore::tagged(
                    append_results(&rec.company, prev.as_ref(), r)?,
                    onchain,
                )
                .map_err(|e| format!("{path}: {e}"))?;
                let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
                write_atomic(std::path::Path::new(&path), &format!("{text}\n"))?;
                eprintln!("wrote the results store {path}");
            }
            Ok(())
        };
        // ATT-R2-F3, THREATS REVLOG-R4-5: a round opened on a fixed beacon
        // is published as an attempt even when its run failed, before the
        // book drops it.
        let mut publish_failed = |a: &Attempt| -> Result<(), String> {
            if let Some(dir) = &store_dir {
                let path = format!("{dir}/{}.json", rec.company.key);
                let prev = read_store(&path, new_store)?;
                let next = ObservationStore::tagged(
                    append_attempt(&rec.company, prev.as_ref(), a)?,
                    onchain,
                )
                .map_err(|e| format!("{path}: {e}"))?;
                let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
                write_atomic(std::path::Path::new(&path), &format!("{text}\n"))?;
                eprintln!("wrote the failed round to the results store {path}");
            }
            Ok(())
        };
        // Review R9-F1: the company's results store as written so far, so a
        // lost book never adopts a draw whose round it already publishes.
        let written = match &store_dir {
            Some(dir) => read_store(&format!("{dir}/{}.json", rec.company.key), new_store)?,
            None => None,
        };
        // Review R11-3: `--new-store` (no store file) only on a first run.
        if store_dir.is_some() && written.is_none() {
            check_new_store(
                &rec.company,
                &s,
                &rec.period_end,
                rec.window_start,
                chain,
                &ChainWait::DEFAULT,
            )?;
        }
        let r = match run_and_save_with(
            &rec.company,
            &rec.period_end,
            rec.window_start,
            &pages,
            &mut s,
            chain,
            &book,
            written.as_ref(),
            &ChainWait::DEFAULT,
            clock,
            // Reveals sent again after a restart (REVLOG-R5-1d): the in-process
            // observers hand theirs over directly; independent observers (ROADMAP B2)
            // get an inbox of their own.
            &mut NoInbox,
            &receipt_key,
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
        if let Some(dir) = &draw_dir {
            let path = format!("{dir}/{}.json", rec.company.key);
            let text =
                serde_json::to_string(&r.onchain_draw.to_json()).map_err(|e| e.to_string())?;
            std::fs::write(&path, format!("{text}\n")).map_err(|e| e.to_string())?;
            eprintln!("wrote the draw record {path}");
        }
        rounds.push(summary(
            &rec.company,
            &r,
            &rec.period_end,
            &dates,
            &rec.body,
            // ATT-R2-F1: a round without a value is sealed (its fact is not
            // closed when the run ends).
            r.claim.is_none(),
        ));
    }
    let text = serde_json::to_string_pretty(&site_summary(&rounds)).map_err(|e| e.to_string())?;
    match args.iter().position(|a| a == "--out") {
        Some(i) => {
            let path = args.get(i + 1).ok_or("--out needs a path")?;
            std::fs::write(path, format!("{text}\n")).map_err(|e| e.to_string())?;
            eprintln!("wrote {path}");
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// Review R11-2: `--tag-store`, a mode of its own: check every draw each
/// company's store in `store_dir` publishes against the chain, tag them
/// all (or none), exit.
fn tag_mode(
    args: &[String],
    store_dir: Option<String>,
    book: Option<String>,
) -> Result<(), String> {
    if args.iter().any(|a| a == "--new-store") {
        return Err("--tag-store tags existing results stores: drop --new-store".into());
    }
    let dir = store_dir.ok_or("--tag-store needs --store-dir DIR (existing results stores)")?;
    // One runner per book: never tag a store a run is writing (review R12:
    // the book is required, and its held draws bound the chain's counts).
    let book = book.ok_or("--tag-store needs --book PATH: the runner's book, locked while the stores are tagged (review R12)")?;
    let _lock = lock_book(std::path::Path::new(&book))?;
    let mut held = store();
    load_book(&mut held, &book)?;
    let cast = CastChain::from_args(args)?
        .ok_or("--tag-store needs --rpc: a store is tagged with the chain's deployment")?;
    for (key, n) in tag_stores(std::path::Path::new(&dir), &cast, &held)? {
        eprintln!(
            "checked the {n} draw(s) the results store {dir}/{key}.json publishes against the chain and tagged it with this deployment"
        );
    }
    eprintln!("run again without --tag-store");
    Ok(())
}

/// The results store at `path` (review R10-3: missing only with `--new-store`).
fn read_store(path: &str, new_store: bool) -> Result<Option<serde_json::Value>, String> {
    ObservationStore::read_file(std::path::Path::new(path), new_store)
}
