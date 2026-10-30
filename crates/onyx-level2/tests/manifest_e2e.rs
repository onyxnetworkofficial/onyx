//! CL3 local end-to-end demo (ADR-0053): a reviewed manifest reaches level 2 with no Rust of
//! its own. A local fixture server answers the manifest's own URL with its recording; five
//! observers fetch it through `onyx_fetch` (every transport rule runs; only the last hop is the
//! loopback fixture); the round runs on the in-memory registry and its claim is accepted at
//! level 2. No live third-party call.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{ObservationOutcome, ObserverStatus, SimulatedRegistry};
use onyx_core::{Answer, ProofLevel, Value};
use onyx_fetch::fixture::{FixtureServer, Reply};
use onyx_level2::manifest::ManifestSource;
use onyx_level2::{run, store, Page, Source};
use std::path::{Path, PathBuf};

/// 2026-10-29 00:00 UTC: record date 2026-10-27 is observable.
const START: u64 = 1_793_232_000;
const DAY: &str = "2026-10-27";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source() -> ManifestSource {
    ManifestSource::from_file(
        &repo().join("sources/us.treasury.tga_total_deposits/manifest.json"),
        &repo(),
    )
    .unwrap()
}

fn recording() -> Vec<u8> {
    std::fs::read(repo().join("sources/us.treasury.tga_total_deposits/recording/2026-10-27.json"))
        .unwrap()
}

/// The path and query of the instantiated URL (what the fixture answers).
fn path_of(url: &str) -> String {
    let rest = url.strip_prefix("https://").unwrap();
    rest[rest.find('/').unwrap()..].to_string()
}

fn pages(src: &ManifestSource, server: &FixtureServer, urls: &[String]) -> Vec<Page> {
    let f = server.fetch(src.fetch_limits());
    urls.iter()
        .enumerate()
        .map(|(i, url)| Page {
            fetched_unix: START + 10 + 30 * i as u64,
            body: f.get_blocking(url).ok(),
        })
        .collect()
}

#[test]
fn a_manifest_source_reaches_level_two_end_to_end_from_a_local_fixture() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let server =
        FixtureServer::start(vec![(path_of(&spec.url), Reply::json(recording()))]).unwrap();
    let p = pages(&src, &server, &vec![spec.url.clone(); 5]);
    assert!(p
        .iter()
        .all(|p| p.body.as_deref() == Some(&recording()[..])));
    // Every observer sent the neutral request a live observer sends.
    let seen = server.seen();
    assert_eq!(seen.requests.len(), 5);
    for r in &seen.requests {
        let r = r.to_ascii_lowercase();
        assert!(r.contains("host: api.fiscaldata.treasury.gov\r\n"), "{r}");
        assert!(r.contains("user-agent: onyx-observer/1\r\n"), "{r}");
        assert!(r.contains("accept: application/json\r\n"), "{r}");
    }
    let mut chain = SimulatedRegistry::new();
    let mut st = store(&src);
    let r = run(&src, DAY, START, &p, &mut st, &mut chain).unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(321_642))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .all(|(_, s)| *s == ObserverStatus::Agreed));
    let claim = r.value().unwrap();
    assert_eq!(claim.value, Value::Int(321_642));
    assert_eq!(
        st.accept_claim(claim, &src.registry(DAY)).unwrap(),
        ProofLevel::ReObserved
    );
}

/// The same round when the source misbehaves for some observers: a reordered list (another
/// row first), a redirect and an oversize body give no value for those observers; the honest
/// majority still settles the value, never the moved row.
#[test]
fn observers_that_get_a_moved_row_or_a_bad_answer_never_move_the_value() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let good = recording();
    let moved = String::from_utf8(good.clone())
        .unwrap()
        .replace(
            r#"{"record_date":"2026-10-27","account_type":"Total TGA Deposits (Table II)","open_today_bal":"321642"}"#,
            r#"{"record_date":"2026-10-27","account_type":"Total TGA Withdrawals (Table II) (-)","open_today_bal":"289813"}"#,
        );
    assert_ne!(moved.as_bytes(), &good[..]);
    let mut huge = Reply::json(vec![b' '; 4096]);
    huge.repeat = 8; // 32 KiB > the manifest's 16 KiB cap
    let server = FixtureServer::start(vec![
        (path_of(&spec.url), Reply::json(good.clone())),
        ("/moved".into(), Reply::json(moved.into_bytes())),
        (
            "/redirect".into(),
            Reply::status(302, &[("location", "https://169.254.169.254/")]),
        ),
        ("/huge".into(), huge),
    ])
    .unwrap();
    let base = "https://api.fiscaldata.treasury.gov";
    let urls = vec![
        spec.url.clone(),
        format!("{base}/moved"),
        spec.url.clone(),
        format!("{base}/redirect"),
        spec.url.clone(),
    ];
    let p = pages(&src, &server, &urls);
    assert!(p[3].body.is_none(), "a redirect is no answer");
    // One more observer whose answer is over the cap.
    let f = server.fetch(src.fetch_limits());
    assert!(f.get_blocking(&format!("{base}/huge")).is_err());
    let mut chain = SimulatedRegistry::new();
    let r = run(&src, DAY, START, &p, &mut store(&src), &mut chain).unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(321_642))
    );
    assert_eq!(r.value().unwrap().value, Value::Int(321_642));
}
