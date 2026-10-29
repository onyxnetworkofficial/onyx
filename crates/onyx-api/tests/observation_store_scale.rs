//! AUD-2-F4 and F10: a `/v1/observations` request must not cost more as the
//! level-2 store grows (it cloned the whole feed under the catalog lock: 78.7 ms
//! per free 404 at 50k rounds), and loading a store must not be quadratic
//! (40k rounds took 17.3 s).
use axum::body::Body;
use axum::http::{Request, StatusCode};
use onyx_api::catalog::Catalog;
use onyx_api::observations::{check, ObservationFeed, ObservationRecord, Transcript};
use onyx_api::{router, ApiConfig, ApiState};
use onyx_coordinator::cas::MemCas;
use onyx_coordinator::observe::{round_id, test_keys, DrawSeed, ObserveConfig, Observer};
use onyx_core::claim::MetricId;
use onyx_core::{
    keccak256, Answer, DataRights, FeedTemplate, Observation, ObservationKind, Redistribution,
    SourceId, Subject, SubjectRule,
};
use std::sync::Arc;
use std::time::Instant;
use tower::ServiceExt;

fn subject() -> Subject {
    Subject::Aggregate("test.index".into())
}

fn template() -> FeedTemplate {
    FeedTemplate {
        feed: "test.index.v1".into(),
        metric: MetricId("test.index".into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/index.json".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("test.page".into()),
            attribution: None,
            terms: String::new(),
        },
        subject: SubjectRule::Fixed(subject()),
    }
}

fn round(t: &FeedTemplate, start: u64) -> Transcript {
    let spec = t.instantiate(&subject(), start).expect("spec");
    let cfg = ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    };
    let observers: Vec<Observer> = (1..=3u8)
        .map(|i| Observer {
            address: test_keys::ordered(i).1,
            bond: 1_000,
        })
        .collect();
    // The pool is exactly the draw size, so the draw is the whole pool.
    let draw = DrawSeed::for_pool(&observers, 10, onyx_core::Hash32([3; 32]), 0);
    let rid = round_id(&spec.id(), &cfg, &observers, &draw);
    let obs = Observation {
        answer: Answer::Number(4200),
        observed_at: start + 10,
        page_hash: keccak256(b"page"),
    };
    let mut commits = vec![];
    let mut reveals = vec![];
    for o in &observers {
        let salt = keccak256(&o.address);
        let (c, r) = test_keys::signed(
            &test_keys::ordered_secret(&o.address).expect("test key"),
            &rid,
            draw.attempt,
            &obs,
            &salt,
        );
        commits.push(c);
        reveals.push(r);
    }
    Transcript {
        subject: subject(),
        window_start: start,
        cfg,
        pool: observers.clone(),
        draw,
        observers,
        commits,
        reveals,
        sealed: Default::default(),
        answer: obs.answer,
    }
}

fn feed(n: u64) -> ObservationFeed {
    let t = template();
    let mut f = ObservationFeed::new(t.clone()).expect("feed");
    let now = 10_000_000_000;
    for k in 0..n {
        let tr = round(&t, 1_200 + k * 300);
        let checked = check(&t, &tr, now).expect("checks");
        f.records.insert(
            ("test.index".into(), checked.spec.window_end),
            ObservationRecord {
                transcript: tr,
                checked,
            },
        );
    }
    f
}

/// A server over a store of `n` rounds.
fn app_of(n: u64) -> axum::Router {
    let mut c = Catalog::new();
    c.add_observations(feed(n)).expect("adds");
    let state = Arc::new(
        ApiState::new(
            c,
            Arc::new(onyx_api::roots::LedgerRoots::default()),
            Arc::new(MemCas::new()),
            None,
            Arc::new(|| 10_000_000_000),
            "http://localhost",
            ApiConfig::default(),
        )
        .expect("state"),
    );
    router(state)
}

/// `reqs` requests for an unknown subject, each a 404.
async fn ask_404(app: &axum::Router, reqs: u32) {
    for _ in 0..reqs {
        let r = app
            .clone()
            .oneshot(
                Request::get("/v1/observations/test.index?subject=nope")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("resp");
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
}

/// ROADMAP E12: this compared the wall time of 200 404s at 1 round with 200
/// at 50k, run one after the other, so a load spike in either run could
/// fail it. Now both servers answer in alternating chunks (load slows both
/// alike) and the time counted is this thread's CPU time (the router runs
/// on it: one current-thread runtime). The bound is unchanged.
#[tokio::test]
async fn a_404_costs_the_same_at_1_round_or_50k() {
    const REQS: u32 = 200;
    const CHUNK: u32 = 20;
    let (small_app, big_app) = (app_of(1), app_of(50_000));
    let wall = Instant::now();
    let (mut small_ns, mut big_ns) = (0u128, 0u128);
    for _ in 0..REQS / CHUNK {
        let t0 = cpu_ns(wall);
        ask_404(&big_app, CHUNK).await;
        let t1 = cpu_ns(wall);
        ask_404(&small_app, CHUNK).await;
        let t2 = cpu_ns(wall);
        big_ns += t1 - t0;
        small_ns += t2 - t1;
    }
    // Microseconds per request (integers only: the workspace refuses floats).
    let (small, big) = (
        small_ns / 1_000 / u128::from(REQS),
        big_ns / 1_000 / u128::from(REQS),
    );
    eprintln!("per-404 us: 1 round = {small}, 50k rounds = {big}");
    // Before the fix: 78.7 ms at 50k against 0.006 ms at 1 round.
    assert!(big < small * 5 + 1_000, "small {small} us, big {big} us");
}

/// Nanoseconds this thread has run on a CPU (Linux: `/proc/thread-self/schedstat`), so time
/// spent waiting for a CPU on a loaded machine is not counted; elsewhere, wall time.
fn cpu_ns(wall: Instant) -> u128 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<u128>().ok())
        .unwrap_or_else(|| wall.elapsed().as_nanos())
}

/// AUD-2-F10: the cost of one `add()` does not grow with the store. The same
/// number of rounds is added to a store of 40k rounds and to one that starts
/// empty, in alternating chunks of 100, and the two totals are compared.
///
/// E10 review: this compared the wall time of loading 10k and 40k rounds
/// (`40k < 7 x 10k + 50 ms`), which a load spike in either run failed (seen
/// once in a check-all under load). Now both stores take their chunks in the
/// same moments, so load slows both alike, and the time counted is CPU time.
/// The rounds are signed before any timing. With the old scan over every
/// record back in `add()` the big store's adds cost several times the small
/// one's (the check fails); with the index they cost about the same.
#[test]
fn store_load_is_not_quadratic_in_rounds() {
    const BIG: u64 = 40_000;
    const PROBES: u64 = 2_000;
    const CHUNK: usize = 100;
    let t = template();
    let rounds = |from: u64, n: u64| -> Vec<Transcript> {
        (from..from + n)
            .map(|k| round(&t, 1_200 + k * 300))
            .collect()
    };
    let mut big = ObservationFeed::new(t.clone()).expect("feed");
    for r in rounds(0, BIG) {
        big.add(r, 10_000_000_000).expect("adds");
    }
    let mut small = ObservationFeed::new(t.clone()).expect("feed");
    let big_probes = rounds(BIG, PROBES);
    let small_probes = rounds(0, PROBES);
    let wall = Instant::now();
    let (mut big_ns, mut small_ns) = (0u128, 0u128);
    for (b, s) in big_probes.chunks(CHUNK).zip(small_probes.chunks(CHUNK)) {
        let t0 = cpu_ns(wall);
        for r in b {
            big.add(r.clone(), 10_000_000_000).expect("adds");
        }
        let t1 = cpu_ns(wall);
        for r in s {
            small.add(r.clone(), 10_000_000_000).expect("adds");
        }
        let t2 = cpu_ns(wall);
        big_ns += t1 - t0;
        small_ns += t2 - t1;
    }
    let (b, s) = (big_ns / 1_000_000, small_ns / 1_000_000);
    eprintln!("add() of {PROBES} rounds: into {BIG} = {b} ms, into 0..{PROBES} = {s} ms");
    // Linear: about 1x (a few more B-tree levels). The old scan: each add
    // visited every record, about 40k at the big store against 1k on
    // average at the small one.
    assert!(
        big_ns < small_ns * 3 / 2,
        "{PROBES} adds cost {b} ms into a store of {BIG} rounds, {s} ms into an empty one"
    );
}
