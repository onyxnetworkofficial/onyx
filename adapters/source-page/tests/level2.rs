//! Level 2 end to end on the synthetic fixture pages: five observers run the
//! pinned JSON-pointer fetcher, commit, reveal, and the round settles.

use onyx_coordinator::observe::SimulatedChain;
use onyx_coordinator::observe::{
    sign_commit, sign_reveal, test_keys, ObservationOutcome, ObservationResults, ObserveConfig,
    Observer, ObserverStatus,
};
use onyx_core::{
    keccak256, observation_commit, observe, Answer, DataRights, FeedTemplate, Fetcher, Hash32,
    MetricId, ObservationKind, ObservationSpec, Redistribution, SourceId, Subject, SubjectRule,
};
use onyx_source_page::JsonPointerFetcher;

const PAGE: &[u8] = include_bytes!("../fixtures/example-page.json");
const LATER: &[u8] = include_bytes!("../fixtures/example-page-later.json");

fn spec(selector: &str, kind: ObservationKind) -> ObservationSpec {
    ObservationSpec {
        feed: "example.page.usd_per_unit".into(),
        metric: MetricId("example.page.usd_per_unit".into()),
        subject: Subject::Entity("entity:example".into()),
        fetcher: JsonPointerFetcher.id(),
        url: "https://example.org/page.json".into(),
        selector: selector.into(),
        kind,
        window_start: 1_000,
        window_end: 1_050,
        min_observers: 5,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("example.page".into()),
            attribution: None,
            terms: String::new(),
        },
    }
}

/// A store with `s` registered as its metric's one feed (L2SRC-R2-F1).
fn store_for(s: &ObservationSpec) -> ObservationResults {
    let (mut store, admin) = ObservationResults::new();
    store
        .register_feed(
            &admin,
            FeedTemplate {
                feed: s.feed.clone(),
                metric: s.metric.clone(),
                fetcher: s.fetcher,
                url_template: s.url.clone(),
                selector: s.selector.clone(),
                kind: s.kind,
                window_secs: s.window_end - s.window_start,
                min_observers: s.min_observers,
                rights: s.rights.clone(),
                subject: SubjectRule::Fixed(s.subject.clone()),
            },
            "synthetic fixture feed",
            0,
        )
        .expect("register");
    store
}

fn cfg() -> ObserveConfig {
    ObserveConfig {
        commit_close: 1_100,
        reveal_close: 1_200,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

/// Observer `i`'s fake test key and address (ascending with `i`).
fn key(i: u8) -> [u8; 32] {
    test_keys::ordered(i).0
}

fn addr(i: u8) -> [u8; 20] {
    test_keys::ordered(i).1
}

#[test]
fn five_observers_settle_a_price_and_slash_the_liar() {
    // $1.00 tolerance at 2 decimals: the later page (64123.80) still agrees.
    let s = spec(
        "/data/usd_per_unit",
        ObservationKind::Numeric {
            scale: 2,
            tolerance: 100,
        },
    );
    let observers: Vec<Observer> = (1..=5)
        .map(|i| Observer {
            address: addr(i),
            bond: 1_000,
        })
        .collect();
    let mut store = store_for(&s);
    // A five-member pool and a five-observer draw: the draw is the pool.
    let snapshot = store
        .announce_draw(&s, &cfg(), &observers, 10, &SimulatedChain::at(0))
        .expect("announce");
    let mut r = store
        .open_round(
            s.clone(),
            cfg(),
            snapshot,
            &SimulatedChain::with_beacon(Hash32([3; 32])),
        )
        .expect("open");
    let f = JsonPointerFetcher;
    let mut reveals = Vec::new();
    for i in 1..=5u8 {
        let page = if i == 4 { LATER } else { PAGE };
        let mut o = observe(&f, &s, Some(page), 1_010 + u64::from(i)).expect("observe");
        if i == 5 {
            // A liar reports a made-up price for the same page.
            o.answer = Answer::Number(7_000_000);
        }
        let salt = keccak256(&[i]);
        let (rid, attempt) = (r.round_id(), r.draw().attempt);
        let h = observation_commit(&rid, &addr(i), &o, &salt);
        let sig = sign_commit(&key(i), &rid, attempt, &h).expect("sign");
        r.commit(1_070, addr(i), h, sig).expect("commit");
        reveals.push((i, o, salt));
    }
    for (i, o, salt) in reveals {
        let sig = sign_reveal(&key(i), &r.round_id(), r.draw().attempt, &o, &salt).expect("sign");
        r.reveal(1_150, addr(i), o, salt, sig).expect("reveal");
    }
    let out = store.settle(&r, 1_201).expect("settle");
    assert_eq!(
        *out.outcome(),
        ObservationOutcome::Value(Answer::Number(6_412_345))
    );
    let statuses: Vec<ObserverStatus> = out.statuses().iter().map(|(_, s)| *s).collect();
    assert_eq!(
        statuses,
        vec![
            ObserverStatus::Agreed,
            ObserverStatus::Agreed,
            ObserverStatus::Agreed,
            ObserverStatus::Agreed,
            ObserverStatus::Outlier
        ]
    );
    assert!(out.entries().contains(&(addr(5), -100)));
}

#[test]
fn exact_and_unavailable_answers_follow_the_fetcher_contract() {
    let f = JsonPointerFetcher;
    let exact = spec("/data/status", ObservationKind::Exact { max_len: 16 });
    let o = observe(&f, &exact, Some(PAGE), 1_010).expect("status");
    assert_eq!(o.answer, Answer::Bytes(b"open".to_vec()));
    assert_eq!(o.page_hash, keccak256(PAGE));

    let num = |sel: &str| {
        observe(
            &f,
            &spec(
                sel,
                ObservationKind::Numeric {
                    scale: 2,
                    tolerance: 0,
                },
            ),
            Some(PAGE),
            1_010,
        )
        .expect("observe")
        .answer
    };
    assert_eq!(num("/data/units_listed"), Answer::Number(120_000));
    // A JSON float is refused, never rounded through f64.
    assert_eq!(num("/data/float_field"), Answer::Unavailable);
    // A selector that finds nothing, and a page that is not JSON.
    assert_eq!(num("/data/missing"), Answer::Unavailable);
    let broken = observe(
        &f,
        &spec(
            "/data/usd_per_unit",
            ObservationKind::Numeric {
                scale: 2,
                tolerance: 0,
            },
        ),
        Some(b"<html>maintenance</html>"),
        1_010,
    )
    .expect("observe");
    assert_eq!(
        (broken.answer, broken.page_hash),
        (Answer::Unavailable, Hash32::ZERO)
    );
}
