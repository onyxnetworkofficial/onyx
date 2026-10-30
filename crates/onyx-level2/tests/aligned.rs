//! CL8 (ADR-0058): an aligned multi-venue feed from bucketed venue manifests, end to end in
//! process: six loopback fake venues (`fixtures/aligned-demo`: three honest, one 3% high, one
//! with a skewed clock, one halted), five observers per venue through `onyx_fetch`, each venue's
//! round on the in-memory registry, its results store read back as a client reads it, and the
//! aligned answer with its four clocks, band, flag and client check. No third-party call; the
//! anvil version with the chain clock is `scripts/level2-aligned-anvil.sh`.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{ObservationOutcome, SimulatedRegistry};
use onyx_core::aligned::{Reason, Status, VenueInput};
use onyx_core::Answer;
use onyx_fetch::fixture::{FixtureServer, Reply};
use onyx_level2::aligned::{
    align, answer_json, check_answer_json, lapsed, load_composite, venue_input, venue_round,
    CompositeManifest, OnChain, RecordedAt, VenueRound,
};
use onyx_level2::manifest::{ClearedHosts, ManifestSource};
use onyx_level2::{append_results, run, store, Page, Source};
use std::path::{Path, PathBuf};

/// 2026-10-27T00:00:00Z, the hourly bucket the fixtures record.
const B0: u64 = 1_793_059_200;
/// The first window the venues allow: the bucket's end plus 300 s.
const WS: u64 = B0 + 3_600 + 300;
/// Well after every round settled.
const NOW: u64 = B0 + 86_400;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn demo() -> PathBuf {
    repo().join("fixtures/aligned-demo")
}

fn composite() -> CompositeManifest {
    load_composite(&demo().join("composite.json"), &demo()).unwrap()
}

fn recording(src: &ManifestSource) -> Vec<u8> {
    std::fs::read(
        demo()
            .join("sources")
            .join(&src.manifest().id)
            .join(&src.manifest().recording),
    )
    .unwrap()
}

/// One venue's round from its loopback fake venue: the results store JSON and the page.
fn round_of(src: &ManifestSource, body: &[u8]) -> (String, Vec<u8>, ObservationOutcome) {
    let spec = src.spec(&B0.to_string(), WS).unwrap();
    let t = onyx_fetch::check_target(&spec.url).unwrap();
    let server =
        FixtureServer::start(vec![(t.path_and_query, Reply::json(body.to_vec()))]).unwrap();
    let f = server.fetch(src.fetch_limits());
    let pages: Vec<Page> = (0..5u64)
        .map(|i| Page {
            fetched_unix: WS + 10 + 20 * i,
            body: f.get_blocking(&spec.url).ok(),
        })
        .collect();
    assert!(pages.iter().all(|p| p.body.as_deref() == Some(body)));
    let mut chain = SimulatedRegistry::new();
    let mut st = store(src);
    let r = run(src, &B0.to_string(), WS, &pages, &mut st, &mut chain).unwrap();
    let json = append_results(src, None, &r).unwrap();
    (
        serde_json::to_string(&json).unwrap(),
        body.to_vec(),
        r.settlement.outcome().clone(),
    )
}

/// What the demo would read on anvil (here: a value round recorded a block after its window).
fn recorded(round: &VenueRound) -> OnChain {
    match round {
        VenueRound::Value { fact, .. } => OnChain::Recorded {
            level2: true,
            at: RecordedAt {
                block: 4_242,
                time: fact.window_end + 400,
            },
        },
        _ => OnChain::Nothing,
    }
}

fn inputs(cm: &CompositeManifest) -> Vec<(String, VenueInput)> {
    cm.venues
        .iter()
        .map(|src| {
            let (store, page, _) = round_of(src, &recording(src));
            let round = venue_round(src, B0, Some(&store), Some(&page), NOW).unwrap();
            (
                src.template().metric.0,
                venue_input(&round, recorded(&round), true).unwrap(),
            )
        })
        .collect()
}

/// The demo's six venues: A, B, C honest; D 3% high; E's clock skewed (its candle is not on the
/// grid, so its round is Unavailable); F halted (zero volume: Unavailable). The lower median of
/// A..D is A's close, the band shows D, the flag is raised, every included venue states its four
/// clocks, and the client check accepts exactly this answer.
#[test]
fn an_aligned_feed_from_six_loopback_venues_end_to_end() {
    let cm = composite();
    assert_eq!(cm.venues.len(), 6);
    let ins = inputs(&cm);
    let a = align(&cm, B0, &ins).unwrap();
    assert_eq!(a.status, Status::Aligned);
    assert_eq!(a.value, Some(6_701_250));
    assert_eq!(a.band, Some((6_701_150, 6_902_288)));
    assert!(a.disagreement);
    let reasons: Vec<Option<Reason>> = a.venues.iter().map(|l| l.outcome.err()).collect();
    assert_eq!(
        reasons,
        vec![
            None,
            None,
            None,
            None,
            Some(Reason::Unavailable),
            Some(Reason::Unavailable)
        ]
    );
    let j = answer_json(&cm, &a);
    assert_eq!(j["asOf"], B0 + 3_600);
    assert_eq!(j["value"], "67012.50");
    for v in j["venues"].as_array().unwrap().iter().take(4) {
        assert_eq!(v["venueTime"], B0, "read from the venue's own page");
        assert!(v["observedFrom"].as_u64().unwrap() >= WS);
        assert!(v["observedTo"].as_u64().unwrap() <= WS + 120);
        assert!(v["recordedAt"]["block"].as_u64().unwrap() > 0);
    }
    check_answer_json(&cm, &ins, &j).unwrap();
    let mut forged = j.clone();
    forged["venues"].as_array_mut().unwrap().remove(3);
    assert!(check_answer_json(&cm, &ins, &forged).is_err());
}

/// THREATS CL8-1, CL8-11 at the round: the skewed venue's and the halted venue's observers all
/// answer Unavailable, so neither is ever a value.
#[test]
fn a_skewed_or_halted_venue_settles_unavailable() {
    let cm = composite();
    for (i, src) in cm.venues.iter().enumerate() {
        let (_, _, outcome) = round_of(src, &recording(src));
        match i {
            0..=3 => assert!(matches!(
                outcome,
                ObservationOutcome::Value(Answer::Number(_))
            )),
            _ => assert_eq!(outcome, ObservationOutcome::SourceUnavailable, "{i}"),
        }
    }
}

/// A value round counts only once the chain recorded it; without the record the venue is
/// `not_observed` (ROADMAP C11), and a page that no agreeing observer fetched is refused.
#[test]
fn a_venue_counts_only_when_recorded_and_only_with_its_own_page() {
    let cm = composite();
    let src = &cm.venues[0];
    let (store, page, _) = round_of(src, &recording(src));
    let round = venue_round(src, B0, Some(&store), Some(&page), NOW).unwrap();
    assert_eq!(
        venue_input(&round, OnChain::Nothing, false).unwrap(),
        VenueInput::NotObserved
    );
    assert_eq!(
        venue_input(&round, OnChain::Nothing, true).unwrap(),
        VenueInput::Lapsed
    );
    assert!(matches!(
        venue_input(&round, recorded(&round), false).unwrap(),
        VenueInput::Recorded(_)
    ));
    let mut other = page.clone();
    other.push(b'\n');
    assert!(venue_round(src, B0, Some(&store), Some(&other), NOW).is_err());
    // Without a page the venue time is the bucket start the manifest's assert pinned.
    let VenueRound::Value { fact, .. } = venue_round(src, B0, Some(&store), None, NOW).unwrap()
    else {
        panic!("value");
    };
    assert_eq!(fact.venue_time, B0);
    // Another venue's store, or another bucket, is not this venue's fact.
    let (store_b, _, _) = round_of(&cm.venues[1], &recording(&cm.venues[1]));
    assert!(venue_round(src, B0, Some(&store_b), None, NOW).is_err());
    assert_eq!(
        venue_round(src, B0 + 3_600, Some(&store), None, NOW).unwrap(),
        VenueRound::NotObserved
    );
    assert_eq!(
        venue_round(src, B0, None, None, NOW).unwrap(),
        VenueRound::NotObserved
    );
}

/// THREATS CL8-12 (rights bypass): the demo venues load only under the fixture's synthetic terms
/// table; under the project's own table (no market venue cleared, docs/ops/market-venues.md)
/// every venue manifest and the composite are refused, and a venue on a blocked host never loads.
#[test]
fn a_venue_without_cleared_terms_never_loads() {
    let project = ClearedHosts::from_repo(&repo()).unwrap();
    for v in ["a", "b", "c", "d", "e", "f"] {
        let p = demo().join(format!("sources/demo.venue_{v}.aaa_usd.1h/manifest.json"));
        let b = std::fs::read(&p).unwrap();
        let e = onyx_level2::manifest::load(&b, &project).unwrap_err();
        assert!(e.to_string().contains("not cleared"), "{e}");
    }
    assert!(load_composite(&demo().join("composite.json"), &repo()).is_err());
    // A venue on the fixture table's blocked host (S89) is refused there too.
    let fixture = ClearedHosts::from_repo(&demo()).unwrap();
    let b = std::fs::read(demo().join("sources/demo.venue_a.aaa_usd.1h/manifest.json")).unwrap();
    let blocked = String::from_utf8(b)
        .unwrap()
        .replace("venue-a.example.org", "venue-g.example.org")
        .replace("\"S80\"", "\"S89\"");
    assert!(onyx_level2::manifest::load_with(blocked.as_bytes(), &fixture, false).is_err());
    // No real host is in the project's table for a bucketed (market) manifest.
    assert!(project
        .hosts
        .iter()
        .all(|(h, _)| h == "api.fiscaldata.treasury.gov"));
}

fn write_composite(dir: &Path, body: &serde_json::Value) -> PathBuf {
    let p = dir.join("composite.json");
    std::fs::write(&p, serde_json::to_vec(body).unwrap()).unwrap();
    p
}

fn base_composite() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(demo().join("composite.json")).unwrap()).unwrap()
}

/// The composite file's own rules: strict JSON, no unknown field, one unit and scale (CL8-3),
/// each venue once (CL8-10), a venue id names a manifest of the repository.
#[test]
fn composite_files_are_checked() {
    let tmp = std::env::temp_dir().join(format!("onyx-cl8-composite-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let load = |v: serde_json::Value| load_composite(&write_composite(&tmp, &v), &demo());
    assert!(load(base_composite()).is_ok());
    let mut unit = base_composite();
    unit["unit"] = "USDT".into();
    assert!(load(unit).is_err(), "another unit");
    let mut scale = base_composite();
    scale["scale"] = 8.into();
    assert!(load(scale).is_err(), "another scale");
    let mut twice = base_composite();
    twice["venues"]
        .as_array_mut()
        .unwrap()
        .push("demo.venue_a.aaa_usd.1h".into());
    assert!(load(twice).is_err(), "a venue twice");
    let mut path = base_composite();
    path["venues"][0] = "../demo.venue_a.aaa_usd.1h".into();
    assert!(load(path).is_err(), "a path, not an id");
    let mut extra = base_composite();
    extra["weights"] = serde_json::json!([1, 1, 1, 1, 1, 5]);
    assert!(
        load(extra).is_err(),
        "unknown field (no weights: one venue, one vote)"
    );
    let mut low = base_composite();
    low["min_venues"] = 2.into();
    assert!(load(low).is_err(), "below MIN_VENUES");
    let p = tmp.join("dup.json");
    std::fs::write(&p, br#"{"v":1,"v":1}"#).unwrap();
    assert!(load_composite(&p, &demo()).is_err(), "duplicate key");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The coordinator never picks the bucket: the newest bucket a window may observe is a function
/// of the window and the template (`Source::latest`).
#[test]
fn the_latest_bucket_is_the_templates_choice() {
    let cm = composite();
    let src = &cm.venues[0];
    assert_eq!(src.latest(WS).as_deref(), Some(B0.to_string().as_str()));
    assert_eq!(
        src.latest(WS - 1).as_deref(),
        Some((B0 - 3_600).to_string().as_str())
    );
    assert_eq!(src.earliest_public(&B0.to_string()).unwrap(), B0);
    assert!(src.spec(&B0.to_string(), WS - 1).is_err());
}

/// Review CL8-R1-2 (THREATS CL8-4): the operator serves a store without the disagreeing venue's
/// value (no store, or only an older failed attempt), so the client's own inputs would show it
/// `not_observed`. The client asks the chain for every venue that is not recorded: a result on
/// chain refuses the store; and a value round whose recorded result is another is refused too.
#[test]
fn the_client_refuses_a_store_that_hides_a_recorded_venue() {
    let cm = composite();
    let src = &cm.venues[3];
    let (store, page, _) = round_of(src, &recording(src));
    let round = venue_round(src, B0, Some(&store), Some(&page), NOW).unwrap();
    let on_chain = recorded(&round);
    let hidden = venue_round(src, B0, None, None, NOW).unwrap();
    assert_eq!(hidden, VenueRound::NotObserved);
    assert!(venue_input(&hidden, on_chain, false).is_err());
    assert!(venue_input(&hidden, on_chain, true).is_err());
    assert!(venue_input(&VenueRound::Unavailable, on_chain, true).is_err());
    let OnChain::Recorded { at, .. } = on_chain else {
        panic!("recorded");
    };
    assert!(venue_input(&round, OnChain::Recorded { level2: false, at }, true).is_err());
}

/// A bucket lapses only after the reveal close of a round opened at its last allowed window.
#[test]
fn a_bucket_lapses_after_its_last_round_could_close() {
    let cm = composite();
    let src = &cm.venues[0];
    assert!(!lapsed(src, B0, WS).unwrap());
    assert!(!lapsed(src, B0, B0 + 3_600 + 3_900).unwrap());
    assert!(lapsed(src, B0, NOW).unwrap());
}
