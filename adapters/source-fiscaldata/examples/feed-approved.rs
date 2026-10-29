//! Feed one approved submission into the pinned observer round (PRODUCT-PLAN.md U4).
//!
//! `cargo run -p onyx-source-fiscaldata --example feed-approved -- JOURNAL SUBMISSION_ID --out STORE.json`
//! (a round on the recorded fetches), `... JOURNAL SUBMISSION_ID --date` (print the approved record
//! date), or `... JOURNAL SUBMISSION_ID --from-store STORE.json` (record a
//! LIVE observer round's evidence, pre-public loop PP9: `scripts/feed-live.sh` runs the round).
//!
//! Reads the submission from the local review journal (stop the node first, ADR-0039), refuses it
//! unless it is approved and asks for the adapter's pinned Debt to the Penny feed
//! ([`onyx_source_fiscaldata::intake_feed`]), and runs the observer round on the recorded
//! observer fetches of the record date (a simulated registry; this example never touches a chain
//! or the network). A record date without a recording stops here: its round needs live observers
//! (`scripts/level2-observers.sh`). Writes the results store the query API serves and prints the
//! evidence: receipt id, record date, round id, result hash. Output is local operator data.
//!
//! The feed ledger, `JOURNAL.feed.json` ([`ledger_path`], created when missing), makes retries safe: a submission already fed returns its
//! recorded evidence and runs nothing; a failure is recorded with its reason, and after
//! [`MAX_FEED_ATTEMPTS`] failures the worker stops trying it. The node reads the same file at
//! startup and shows a fed receipt's round id, result hash and record date in its public status.

use onyx_api::submit::{SubmitLimits, SubmitStore};
use onyx_coordinator::observe::SimulatedRegistry;
use onyx_source_fiscaldata::intake_feed::{
    approved_record_date, feed_approved, ledger_path, FeedLedger, FeedStep, MAX_FEED_ATTEMPTS,
};
use onyx_source_fiscaldata::round::{results_store, store, Page};
use onyx_source_fiscaldata::{recorded, RECORDED_WINDOW_START};
use serde_json::json;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: feed-approved JOURNAL SUBMISSION_ID (--out STORE.json | --date | --from-store STORE.json)";
    let (journal, id, mode) = match args.as_slice() {
        [j, i, f, o] if f == "--out" => (j, i, Mode::Recorded(o.clone())),
        [j, i, f] if f == "--date" => (j, i, Mode::Date),
        [j, i, f, s] if f == "--from-store" => (j, i, Mode::Live(s.clone())),
        _ => return Err(usage.into()),
    };
    if !Path::new(journal).is_file() {
        return Err("the existing submission journal is required".into());
    }
    let ledger_file = ledger_path(Path::new(journal));
    let mut ledger = FeedLedger::load(&ledger_file)?;
    let journal = SubmitStore::open(journal, None, SubmitLimits::default())?;
    let item = journal.review_item(id)?;
    if let Mode::Date = mode {
        println!("{}", approved_record_date(&item)?);
        return Ok(());
    }
    match ledger.step(&item.id, &item.hash)? {
        FeedStep::AlreadyFed(e) => {
            println!("already fed, nothing run: {e:?}");
            return Ok(());
        }
        FeedStep::GiveUp(reason) => {
            return Err(
                format!("{id}: {MAX_FEED_ATTEMPTS} attempts failed, last: {reason}").into(),
            );
        }
        FeedStep::Run => {}
    }
    // Not attempts: a submission that is not approved or not the pinned feed, and a date without a
    // recording, are refused before the round and use no attempt.
    let date = approved_record_date(&item)?;
    if let Mode::Live(store) = &mode {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let text = std::fs::read_to_string(store)?;
        let e = onyx_source_fiscaldata::intake_feed::fed_from_store(&item, &text, now)?;
        ledger.fed(&e);
        ledger.save(&ledger_file)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "submission": e.id, "recordDate": e.record_date, "roundId": e.round_id,
                "result": e.result, "value": e.value, "registry": e.registry, "store": store,
                "note": "a live observer round: observers fetched the source themselves; the draw is on the registry of that chain"
            }))?
        );
        return Ok(());
    }
    let Mode::Recorded(out) = &mode else {
        return Err(usage.into());
    };
    if recorded().record_date != date {
        return Err(format!(
            "no recorded observer fetches for {date} (the recording is {}); run live observers with scripts/level2-observers.sh",
            recorded().record_date
        )
        .into());
    }
    if Path::new(out).exists() {
        return Err(format!("{out} exists; give a new file").into());
    }
    match run(&item, out) {
        Ok(fed) => {
            ledger.fed(&fed.evidence("simulated"));
            ledger.save(&ledger_file)?;
            print_evidence(&fed, out)
        }
        Err(e) => {
            let n = ledger.failed(&item.id, &e.to_string());
            ledger.save(&ledger_file)?;
            Err(format!("attempt {n} of {MAX_FEED_ATTEMPTS} failed: {e}").into())
        }
    }
}

fn run(
    item: &onyx_api::submit::review::ReviewItem,
    out: &str,
) -> Result<onyx_source_fiscaldata::intake_feed::FedRound, Box<dyn std::error::Error>> {
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let mut results = store();
    let mut chain = SimulatedRegistry::new();
    let fed = feed_approved(
        item,
        RECORDED_WINDOW_START,
        &pages,
        &mut results,
        &mut chain,
    )?;
    std::fs::write(out, results_store(&fed.run)?.to_string())?;
    Ok(fed)
}

fn print_evidence(
    fed: &onyx_source_fiscaldata::intake_feed::FedRound,
    out: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "submission": fed.submission_id,
            "metric": onyx_source_fiscaldata::template().metric.0,
            "templateId": onyx_source_fiscaldata::template().id().to_string(),
            "subject": onyx_source_fiscaldata::subject(&fed.record_date),
            "submissionHash": fed.submission_hash,
            "recordDate": fed.record_date,
            "roundId": fed.round_id.to_string(),
            "result": fed.result.to_string(),
            "value": fed.value,
            "store": out,
            "note": "checked by the observers' round, not by the submission; a simulated registry, nothing on chain"
        }))?
    );
    Ok(())
}

enum Mode {
    /// A round on the recorded observer fetches, written to this store.
    Recorded(String),
    /// Print the approved record date only.
    Date,
    /// Record the evidence of a live round from its store (the store names its chain).
    Live(String),
}
