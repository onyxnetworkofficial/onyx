//! Review L2API-R2: the level-2 price clock is the template-bound window end,
//! and every check applies the commit- and reveal-time rules a round applies.
use onyx_api::catalog::PricingPolicy;
use onyx_api::observations::{check, ObservationError, Transcript};
use onyx_coordinator::observe::{
    round_id, sign_commit, test_keys, DrawSeed, ObserveConfig, Observer,
};
use onyx_core::claim::MetricId;
use onyx_core::{
    keccak256, Answer, DataRights, FeedTemplate, Hash32, Observation, ObservationKind,
    Redistribution, SourceId, Subject, SubjectRule,
};

const START: u64 = 1_200;

fn subject() -> Subject {
    Subject::Aggregate("test.index".into())
}

fn template(kind: ObservationKind) -> FeedTemplate {
    FeedTemplate {
        feed: "test.index.v1".into(),
        metric: MetricId("test.index".into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/index.json".into(),
        selector: "/value".into(),
        kind,
        window_secs: 300,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("test.page".into()),
            attribution: Some("Source: test page".into()),
            terms: String::new(),
        },
        subject: SubjectRule::Fixed(subject()),
    }
}

fn numeric() -> FeedTemplate {
    template(ObservationKind::Numeric {
        scale: 2,
        tolerance: 0,
    })
}

fn cfg(commit_close: u64, reveal_close: u64) -> ObserveConfig {
    ObserveConfig {
        commit_close,
        reveal_close,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

fn observers() -> Vec<Observer> {
    (1..=3u8)
        .map(|i| Observer {
            address: test_keys::ordered(i).1,
            bond: 1_000,
        })
        .collect()
}

/// A transcript in which every observer reveals `obs` (keys the test holds).
fn forge(t: &FeedTemplate, cfg: ObserveConfig, obs: Observation) -> Transcript {
    let spec = t.instantiate(&subject(), START).expect("spec");
    let observers = observers();
    // The pool is exactly the draw size, so the draw is the whole pool.
    let draw = DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
    let rid = round_id(&spec.id(), &cfg, &observers, &draw);
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
        window_start: START,
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

fn number(observed_at: u64, page_hash: Hash32) -> Observation {
    Observation {
        answer: Answer::Number(4200),
        observed_at,
        page_hash,
    }
}

fn refused(r: Result<onyx_api::observations::Checked, ObservationError>) -> bool {
    matches!(r, Err(ObservationError::Transcript(_)))
}

/// L2API-R2-F2: the reviewer's case. A round with a day-long reveal phase and
/// an honest short round of the same window settle at the same price clock
/// (the window end), so the slow round is free exactly when the honest one is.
#[test]
fn the_price_clock_is_the_window_end_whatever_the_round_config() {
    let t = numeric();
    let we = START + 300;
    let honest = forge(&t, cfg(we, we + 60), number(START + 10, keccak256(b"page")));
    let late = forge(
        &t,
        cfg(we + 600, we + 600 + 86_400),
        number(START + 10, keccak256(b"page")),
    );
    let now = we + 600 + 86_400;
    let h = check(&t, &honest, now).expect("honest round");
    let l = check(&t, &late, now).expect("slow round");
    assert_eq!(h.settled_unix, we);
    assert_eq!(l.settled_unix, we, "no config moves the clock");
    let p = PricingPolicy {
        base_price_micro_usdc: 2_500,
        fresh_window_secs: 3_600,
    };
    assert_eq!(p.price(now - h.settled_unix), 0);
    assert_eq!(
        p.price(now - l.settled_unix),
        0,
        "the slow round decays on the same schedule"
    );
    // Inside the fresh window both are paid, at the same moment.
    let early = we + 1_000;
    assert_eq!(p.price(early - h.settled_unix), 2_500);
    assert_eq!(p.price(early - l.settled_unix), 2_500);
}

/// A round whose reveals are not closed yet is still never served.
#[test]
fn a_round_settling_in_the_future_is_not_served() {
    let t = numeric();
    let we = START + 300;
    let r = forge(
        &t,
        cfg(we + 60, we + 180),
        number(START + 10, keccak256(b"page")),
    );
    assert!(matches!(
        check(&t, &r, we + 180 - 400),
        Err(ObservationError::NotSettledYet { .. })
    ));
    assert!(check(&t, &r, we + 180).is_ok());
}

/// L2API-R2-F1: reveals `ObservationRound::reveal` refuses never verify.
#[test]
fn reveals_no_round_could_accept_are_refused() {
    let t = numeric();
    let we = START + 300;
    let c = cfg(we + 60, we + 180);
    let now = we + 1_000;
    // observed a day after the window ended
    assert!(refused(check(
        &t,
        &forge(&t, c, number(we + 86_400, keccak256(b"page"))),
        now
    )));
    // observed before the window started
    assert!(refused(check(
        &t,
        &forge(&t, c, number(START - 1, keccak256(b"page"))),
        now
    )));
    // a value with a zero page hash
    assert!(refused(check(
        &t,
        &forge(&t, c, number(START + 10, Hash32::ZERO)),
        now
    )));
    // a byte answer on a numeric feed
    let bytes = Observation {
        answer: Answer::Bytes(b"4200".to_vec()),
        observed_at: START + 10,
        page_hash: keccak256(b"page"),
    };
    assert!(refused(check(&t, &forge(&t, c, bytes), now)));
    // Exact bytes longer than the feed's max_len
    let exact = template(ObservationKind::Exact { max_len: 4 });
    let long = Observation {
        answer: Answer::Bytes(b"too long".to_vec()),
        observed_at: START + 10,
        page_hash: keccak256(b"page"),
    };
    assert!(refused(check(&exact, &forge(&exact, c, long), now)));
    // ...while the same shapes inside the rules verify.
    assert!(check(
        &t,
        &forge(&t, c, number(START + 10, keccak256(b"page"))),
        now
    )
    .is_ok());
    let fits = Observation {
        answer: Answer::Bytes(b"ok".to_vec()),
        observed_at: START + 10,
        page_hash: keccak256(b"page"),
    };
    assert!(check(&exact, &forge(&exact, c, fits), now).is_ok());
}

/// L2API-R2-F3: commits must be an assigned observer's, once, ascending.
#[test]
fn commits_from_strangers_twice_or_out_of_order_are_refused() {
    let t = numeric();
    let we = START + 300;
    let c = cfg(we + 60, we + 180);
    let now = we + 1_000;
    let good = forge(&t, c, number(START + 10, keccak256(b"page")));
    assert!(check(&t, &good, now).is_ok());

    // Signed commits, so each case is refused for its own rule.
    let spec = t
        .instantiate(&good.subject, good.window_start)
        .expect("spec");
    let rid = round_id(&spec.id(), &good.cfg, &good.observers, &good.draw);
    let signed = |i: u8, h: Hash32| {
        let (sk, a) = test_keys::ordered(i);
        (
            a,
            h,
            sign_commit(&sk, &rid, good.draw.attempt, &h).expect("sign"),
        )
    };
    let mut stranger = good.clone();
    stranger.commits.push(signed(9, keccak256(b"x")));
    assert!(refused(check(&t, &stranger, now)));

    let mut twice = good.clone();
    twice.commits.insert(1, signed(1, keccak256(b"other")));
    assert!(refused(check(&t, &twice, now)));

    let mut reversed = good.clone();
    reversed.commits.reverse();
    assert!(refused(check(&t, &reversed, now)));
}
