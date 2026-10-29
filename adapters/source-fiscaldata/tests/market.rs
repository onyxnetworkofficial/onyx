//! NS11 markets on a Treasury date (`onyx_level2::markets`): the fact a market
//! opens on is the registry fact the round records, and the value a results
//! store settled proves on chain (its result hash is the one
//! `OnyxObservedFacts.resultHash` rebuilds, fixtures/markets/observed-facts.json).
//! A market on a date a round may already observe, or a settlement from
//! another day's round or a changed store, is refused.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{result_hash_of, ObservationOutcome, RecordedDraw};
use onyx_core::Answer;
use onyx_level2::markets::{market_fact, settled_value};
use onyx_level2::{results_store, Source};
use onyx_source_fiscaldata::round::{recorded_draw, run, store, FiscalData, Page, RoundRun};
use onyx_source_fiscaldata::{recorded, RECORDED_WINDOW_START};

fn recorded_round() -> RoundRun {
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .into_iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body),
        })
        .collect();
    run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store(),
        &mut RecordedDraw::new(recorded_draw()),
    )
    .expect("recorded round")
}

/// The latest UTC midnight before the recorded window at which no round may
/// observe the recorded date yet: a valid trading close.
fn a_close_before_the_date_is_observable() -> u64 {
    let date = recorded().record_date;
    (1..=10)
        .map(|k| (RECORDED_WINDOW_START / 86_400 - k) * 86_400)
        .find(|&t| FiscalData.spec(&date, t).is_err())
        .expect("a day before the recorded window refuses the date")
}

#[test]
fn the_market_fact_is_the_rounds_registry_fact_and_its_value_proves() {
    let r = recorded_round();
    let date = recorded().record_date;
    let close = a_close_before_the_date_is_observable();
    let (fact, subject) = market_fact(&FiscalData, &date, close, RECORDED_WINDOW_START).unwrap();
    assert_eq!(subject, onyx_source_fiscaldata::subject(&date));
    let text = results_store(&FiscalData, &r).unwrap().to_string();
    let now = r.cfg.reveal_close + 60;
    let fact_hex = format!("{fact}");
    let (round, value) = settled_value(&FiscalData, &text, &subject, &fact_hex, now).unwrap();
    assert_eq!(round, r.round_id);
    let ObservationOutcome::Value(Answer::Number(settled)) = r.settlement.outcome().clone() else {
        panic!("the recorded round settles a number");
    };
    assert_eq!(value, settled);
    // What `OnyxObservedFacts.prove` rebuilds from (recorded spec, round, value).
    assert_eq!(
        result_hash_of(
            &r.spec.id(),
            &round,
            &ObservationOutcome::Value(Answer::Number(value))
        ),
        r.settlement.result_hash()
    );
}

#[test]
fn a_date_observable_before_the_close_or_not_by_the_observable_time_is_refused() {
    let date = recorded().record_date;
    let close = a_close_before_the_date_is_observable();
    // Trading would still be open while a round may observe the date.
    assert!(market_fact(
        &FiscalData,
        &date,
        RECORDED_WINDOW_START,
        RECORDED_WINDOW_START + 3_600
    )
    .unwrap_err()
    .contains("before trading closes"));
    // Nobody may observe it by the stated time.
    assert!(market_fact(&FiscalData, &date, close, close + 60)
        .unwrap_err()
        .contains("not observable"));
}

#[test]
fn another_days_round_another_fact_or_a_changed_store_never_settles() {
    let r = recorded_round();
    let date = recorded().record_date;
    let close = a_close_before_the_date_is_observable();
    let (fact, subject) = market_fact(&FiscalData, &date, close, RECORDED_WINDOW_START).unwrap();
    let text = results_store(&FiscalData, &r).unwrap().to_string();
    let now = r.cfg.reveal_close + 60;
    let fact_hex = format!("{fact}");
    let other_day = onyx_source_fiscaldata::subject("2026-10-07");
    assert!(
        settled_value(&FiscalData, &text, &other_day, &fact_hex, now)
            .unwrap_err()
            .contains("no settled round")
    );
    let other_fact = format!("0x{}", "11".repeat(32));
    assert!(
        settled_value(&FiscalData, &text, &subject, &other_fact, now)
            .unwrap_err()
            .contains("another fact")
    );
    // A store whose settled answer was changed fails the server's own check.
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let a = v["rounds"][0]["answer"].as_str().unwrap().to_string();
    let last = if a.ends_with('0') { '1' } else { '0' };
    v["rounds"][0]["answer"] = format!("{}{last}", &a[..a.len() - 1]).into();
    assert!(settled_value(&FiscalData, &v.to_string(), &subject, &fact_hex, now).is_err());
}
