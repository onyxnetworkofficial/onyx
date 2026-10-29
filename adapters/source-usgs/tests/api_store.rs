//! The recorded earthquake round, served by the query API's router (NS17):
//! the results store the runner writes loads (every round re-checked against
//! the pinned template), `GET /v1/observations/{metric}?subject=...` answers
//! the day's count, and a client holding its own copy of the registered daily
//! template verifies it. Another day, a day the rule never allows, and a store
//! pinned to another template are refused.
#![allow(clippy::unwrap_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use onyx_api::catalog::Catalog;
use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_api::{router, ApiConfig, ApiState};
use onyx_coordinator::cas::MemCas;
use onyx_coordinator::observe::RecordedDraw;
use onyx_core::{Answer, Subject};
use onyx_level2::{results_store, run, store, Page};
use onyx_source_usgs::round::{recorded_draw, Usgs};
use onyx_source_usgs::{recorded, recorded_window_start, subject, template, METRIC};
use std::sync::Arc;
use tower::ServiceExt;

/// A buyer's clock one day after the recording.
fn now() -> u64 {
    recorded_window_start() + 86_400
}

fn pins() -> TemplatePins {
    TemplatePins::from([(METRIC.to_owned(), template().id())])
}

/// The store file `level2-round --store` writes for the recorded round, on
/// the draw the rehearsal's chain recorded.
fn store_text() -> String {
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let r = run(
        &Usgs,
        &rec.day,
        recorded_window_start(),
        &pages,
        &mut store(&Usgs),
        &mut RecordedDraw::new(recorded_draw()),
    )
    .unwrap();
    results_store(&Usgs, &r).unwrap().to_string()
}

fn app() -> axum::Router {
    let feed = ObservationStore::from_json(&store_text(), now(), &pins()).unwrap();
    let mut c = Catalog::new();
    c.add_observations(feed).unwrap();
    let t = now();
    router(Arc::new(
        ApiState::new(
            c,
            Arc::new(onyx_api::roots::LedgerRoots::default()),
            Arc::new(MemCas::new()),
            None,
            Arc::new(move || t),
            "http://localhost",
            ApiConfig::default(),
        )
        .unwrap(),
    ))
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let r = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn expect<'a>(t: &'a onyx_core::FeedTemplate, s: &'a Subject) -> ObservationExpect<'a> {
    ObservationExpect {
        template: t,
        subject: s,
        window_end: None,
        min_window_end: recorded_window_start(),
        now: now(),
    }
}

#[tokio::test]
async fn the_recorded_day_is_served_and_verifies_with_the_clients_template() {
    let day = recorded().day;
    let (status, body) = get(
        &app(),
        &format!("/v1/observations/{METRIC}?subject={}", subject(&day)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["value"], "9");
    let t = template();
    let s = Subject::Aggregate(subject(&day));
    let v = verify_observation(&body, &expect(&t, &s)).unwrap();
    assert_eq!(v.answer, Answer::Number(9));
    // A client asking about another day refuses this answer.
    let other = Subject::Aggregate(subject("2026-07-09"));
    assert!(verify_observation(&body, &expect(&t, &other)).is_err());
    // A changed value fails the client's rerun of the round.
    let mut forged = body.clone();
    forged["value"] = "10".into();
    assert!(verify_observation(&forged, &expect(&t, &s)).is_err());
}

#[tokio::test]
async fn another_day_and_a_malformed_day_are_not_found() {
    let app = app();
    for subj in [
        subject("2026-07-09"),
        subject("2026-7-08"),
        "usgs.earthquakes.m5_world".into(),
    ] {
        let (status, body) = get(&app, &format!("/v1/observations/{METRIC}?subject={subj}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{subj}: {body}");
        assert_eq!(body["error"]["code"], "unknown_subject", "{subj}");
    }
}

#[test]
fn a_store_is_refused_under_another_pinned_template() {
    let mut other = template();
    other.subject = onyx_core::SubjectRule::Daily {
        prefix: onyx_source_usgs::SUBJECT.into(),
        min_days: onyx_source_usgs::MIN_DAYS,
        max_days: onyx_source_usgs::MAX_DAYS + 1,
    };
    let pins = TemplatePins::from([(METRIC.to_owned(), other.id())]);
    assert!(ObservationStore::from_json(&store_text(), now(), &pins).is_err());
}
