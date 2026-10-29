//! ATT-R1-F1 (post-merge review of the level-2 attempts feature): a round
//! that settles no value must reach the results store, and a re-draw's store
//! must keep it, end to end: runner -> store file -> API -> client -> the
//! chain's draw records.

use onyx_api::client::{
    verify_attempts, verify_attempts_onchain, verify_observation, ObservationExpect,
    VerifiedAttempts,
};
use onyx_api::observations::{ObservationFeed, ObservationStore, TemplatePins};
use onyx_coordinator::observe::{
    DrawChain, DrawRecord, ObservationOutcome, ObservationResults, SimulatedRegistry,
};
use onyx_source_fiscaldata::round::{append_results, results_store, run, store, Page, RoundRun};
use onyx_source_fiscaldata::{recorded, subject, template, RECORDED_WINDOW_START};
use serde_json::Value;

const NOW: u64 = RECORDED_WINDOW_START + 86_400;

fn pins() -> TemplatePins {
    TemplatePins::from([(template().metric.0.clone(), template().id())])
}

/// Every observer's fetch failed: the round settles `SourceUnavailable`.
fn outage_pages() -> (String, Vec<Page>) {
    let rec = recorded();
    let pages = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: None,
        })
        .collect();
    (rec.record_date, pages)
}

fn good_pages() -> Vec<Page> {
    recorded()
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect()
}

fn load(store: &Value) -> ObservationFeed {
    ObservationStore::from_json(&store.to_string(), NOW, &pins()).expect("loads")
}

fn word(v: u64) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// What `drawOf` returns for a draw the chain recorded.
fn draw_of(r: &DrawRecord) -> Vec<u8> {
    let mut v = r.commit.0.to_vec();
    v.extend(word(r.beacon_height));
    v.extend(word(r.announced_at));
    v.extend_from_slice(&r.beacon.0);
    v
}

/// The chain's answers for the fact of `run`: drawCount, one drawOf per
/// draw without a value (from the records the chain returned), drawBudget.
fn chain_answers(
    chain: &SimulatedRegistry,
    s: &ObservationResults,
    run: &RoundRun,
    without_value: &[&RoundRun],
) -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let fact = s.onchain_fact(&run.spec).expect("fact");
    let count = chain.draw_count(&fact).expect("count");
    let (strikes, allowance) = chain.draw_budget(&fact).expect("budget");
    let mut budget = word(u64::from(strikes));
    budget.extend(word(u64::from(allowance)));
    let draws = without_value
        .iter()
        .map(|r| draw_of(&r.onchain_draw))
        .collect();
    (word(u64::from(count)), draws, budget)
}

fn expect<'a>(t: &'a onyx_core::FeedTemplate, s: &'a onyx_core::Subject) -> ObservationExpect<'a> {
    ObservationExpect {
        template: t,
        subject: s,
        window_end: None,
        min_window_end: 0,
        now: NOW,
    }
}

/// A round without a value is returned by the runner, written to the store as
/// an attempt, served free (sealed: the fact may still settle) and checked by
/// a client against the chain's draw records.
#[test]
fn a_round_without_a_value_is_published_and_checks_on_chain() {
    let mut chain = SimulatedRegistry::new();
    let (date, pages) = outage_pages();
    let mut s = store();
    let r = run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("runs");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::SourceUnavailable
    );
    assert!(r.claim.is_none() && r.value().is_err());
    let feed = load(&results_store(&r).expect("store"));
    let fact = feed.attempt_fact(&subject(&date), None).expect("indexed");
    let free = feed.attempts_body(&feed.attempts_of(&fact), feed.fact_closed(&fact));
    let (t, subj) = (template(), r.spec.subject.clone());
    let v = verify_attempts(&free, &expect(&t, &subj)).expect("verifies");
    assert!(
        v.attempts[0].sealed,
        "a dated fact without a value is sealed"
    );
    let (count, draws, budget) = chain_answers(&chain, &s, &r, &[&r]);
    let tally = verify_attempts_onchain(
        &v,
        &count,
        &draws,
        &budget,
        &complete(&v),
        &unsealed_all(&v),
    )
    .expect("the chain agrees");
    assert_eq!(
        (tally.draws, tally.without_value, tally.published),
        (1, 1, 1)
    );
}

/// A failed draw, then a value draw: the store is appended, never replaced,
/// so it still holds draw 0's round and the client's on-chain check passes
/// for the honest multi-draw fact. A store that kept only the value round
/// (the old behaviour) fails that check.
#[test]
fn a_redraw_store_keeps_the_earlier_round_and_verifies_on_chain() {
    let mut chain = SimulatedRegistry::new();
    let (date, outage) = outage_pages();
    let mut s = store();
    let first = run(&date, RECORDED_WINDOW_START, &outage, &mut s, &mut chain).expect("runs");
    let store0 = results_store(&first).expect("store");
    let second = run(
        &date,
        RECORDED_WINDOW_START,
        &good_pages(),
        &mut s,
        &mut chain,
    )
    .expect("re-draw");
    assert_eq!(second.draw.attempt, 1, "the value round is the second draw");
    second.value().expect("a value");
    // `recordObservation`: the value draw stops counting against the subject.
    chain.record_value(&s.onchain_fact(&second.spec).expect("fact"));
    let store1 = append_results(Some(&store0), &second).expect("append");
    assert_eq!(store1["attempts"].as_array().map(Vec::len), Some(1));
    assert_eq!(store1["rounds"].as_array().map(Vec::len), Some(1));
    let feed = load(&store1);
    let rec = feed.record(&subject(&date), None, None).expect("record");
    let all = feed.body_all(rec, &feed.attempts_of(&rec.checked.fact));
    let (t, subj) = (template(), second.spec.subject.clone());
    let v = verify_observation(&all, &expect(&t, &subj)).expect("verifies");
    let a: VerifiedAttempts = v.attempts.expect("outcome=all");
    assert_eq!(a.attempts.len(), 1);
    assert!(
        !a.attempts[0].sealed,
        "the fact has a value: reveals are served"
    );
    let (count, draws, budget) = chain_answers(&chain, &s, &second, &[&first]);
    let tally = verify_attempts_onchain(
        &a,
        &count,
        &draws,
        &budget,
        &complete(&a),
        &unsealed_all(&a),
    )
    .expect("the honest multi-draw fact passes");
    assert_eq!(
        (tally.draws, tally.without_value, tally.published),
        (2, 1, 1)
    );
    // Replacing the store with the value round alone hides draw 0.
    let replaced = load(&results_store(&second).expect("store"));
    let rec = replaced
        .record(&subject(&date), None, None)
        .expect("record");
    let all = replaced.body_all(rec, &replaced.attempts_of(&rec.checked.fact));
    let v = verify_observation(&all, &expect(&t, &subj)).expect("verifies");
    let a = v.attempts.expect("outcome=all");
    assert!(verify_attempts_onchain(
        &a,
        &count,
        &draws,
        &budget,
        &complete(&a),
        &unsealed_all(&a)
    )
    .is_err());
    // Appending the same round again (a runner publishing it again after a
    // crash, review REVLOG-R4-1) leaves the store as it was.
    let twice = append_results(Some(&store1), &second).expect("append");
    assert_eq!(twice, store1, "append is idempotent");
    assert!(ObservationStore::from_json(&twice.to_string(), NOW, &pins()).is_ok());
    // A store holding the same draw's round twice is refused at load.
    let mut dup = store1.clone();
    let again = dup["rounds"][0].clone();
    dup["rounds"].as_array_mut().expect("rounds").push(again);
    assert!(ObservationStore::from_json(&dup.to_string(), NOW, &pins()).is_err());
    // A store of another template is refused.
    let mut other = store0.clone();
    other["template"] = "0x00".into();
    assert!(append_results(Some(&other), &second).is_err());
}

/// `transcriptComplete` returns of an untampered log: true for every
/// published round (ADR-0027 amendment 8).
fn complete(a: &onyx_api::client::VerifiedAttempts) -> Vec<Vec<u8>> {
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    vec![yes; a.attempts.len()]
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
