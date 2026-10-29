//! The recorded Debt to the Penny round, served by the query API: the
//! results store the runner writes loads (every round re-checked against the
//! template), and a client holding its own copy of the registered template
//! verifies the served value (ADR-0027 amendment 4).

use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_api::observations::ObservationStore;
use onyx_coordinator::observe::SimulatedRegistry;
use onyx_core::{Answer, Subject};
use onyx_source_fiscaldata::round::{results_store, run, store, Page};
use onyx_source_fiscaldata::{recorded, subject, template, RECORDED_WINDOW_START};

/// A checker's clock one day after the recording (the round settled minutes
/// after its window opened).
const NOW: u64 = RECORDED_WINDOW_START + 86_400;

fn pages() -> (String, Vec<Page>) {
    let rec = recorded();
    let pages = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    (rec.record_date, pages)
}

/// The operator's pin: the reviewed registration of the Treasury metric.
fn pins() -> onyx_api::observations::TemplatePins {
    onyx_api::observations::TemplatePins::from([(template().metric.0.clone(), template().id())])
}

#[test]
fn the_recorded_round_is_served_and_verifies_with_the_clients_template() {
    let mut chain = SimulatedRegistry::new();
    let (date, pages) = pages();
    let mut s = store();
    let r = run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("round");
    let text = results_store(&r).expect("store").to_string();
    let feed = ObservationStore::from_json(&text, NOW, &pins()).expect("loads and re-checks");
    let subj = Subject::Aggregate(subject(&date));
    let rec = feed.record(&subject(&date), None, None).expect("record");
    // The store's checks reach the same result the coordinator settled.
    assert_eq!(rec.checked.result, r.settlement.result_hash());
    let body = feed.body(rec);
    assert_eq!(body["value"], "40305316210829.72");
    let t = template();
    let v = verify_observation(
        &body,
        &ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: RECORDED_WINDOW_START,
            now: NOW,
        },
    )
    .expect("verifies");
    assert_eq!(v.answer, Answer::Number(4_030_531_621_082_972));
    assert_eq!(v.checked.result, r.settlement.result_hash());
    assert_ne!(v.checked.registry_fact, onyx_core::Hash32::ZERO);
}

#[test]
fn a_store_for_another_template_does_not_verify_for_this_client() {
    let mut chain = SimulatedRegistry::new();
    let (date, pages) = pages();
    let mut s = store();
    let r = run(&date, RECORDED_WINDOW_START, &pages, &mut s, &mut chain).expect("round");
    let feed =
        ObservationStore::from_json(&results_store(&r).expect("store").to_string(), NOW, &pins())
            .expect("loads");
    let body = feed.body(feed.record(&subject(&date), None, None).expect("record"));
    // A client whose registered template names another field.
    let mut t = template();
    t.selector = "/data/0/debt_held_public_amt".into();
    let subj = Subject::Aggregate(subject(&date));
    assert!(verify_observation(
        &body,
        &ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: RECORDED_WINDOW_START,
            now: NOW,
        },
    )
    .is_err());
}
