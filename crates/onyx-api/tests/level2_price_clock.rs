//! L2API-R3-F1: the moment level-2 data becomes free is a pure function of
//! the registered template and the subject. For a dated feed the round opener
//! picks the window start within the lag, but every allowed window of one
//! record date prices from the same clock: the end of the earliest window the
//! lag allows (`FeedTemplate::decay_start`).
use onyx_api::catalog::PricingPolicy;
use onyx_api::observations::{check, Transcript};
use onyx_coordinator::observe::{round_id, test_keys, DrawSeed, ObserveConfig, Observer};
use onyx_core::claim::MetricId;
use onyx_core::{
    day_number, keccak256, Answer, DataRights, DateLag, FeedTemplate, Observation, ObservationKind,
    Redistribution, SourceId, Subject, SubjectRule,
};

const DAY: u64 = 86_400;

fn day(d: &str) -> u64 {
    u64::try_from(day_number(d).expect("date")).expect("positive") * DAY
}

fn subject(date: &str) -> Subject {
    Subject::Aggregate(format!("test.rec:{date}"))
}

fn template() -> FeedTemplate {
    FeedTemplate {
        feed: "test.rec.v1".into(),
        metric: MetricId("test.rec".into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/{date}.json".into(),
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
        subject: SubjectRule::Dated {
            prefix: "test.rec".into(),
            lag: DateLag {
                min_business_days: 1,
                max_calendar_days: 10,
                first_day: day_number("2026-01-01").expect("d"),
                last_day: day_number("2026-12-31").expect("d"),
                holidays: ["2026-05-25", "2026-07-03", "2026-09-07", "2026-10-12"]
                    .iter()
                    .map(|d| day_number(d).expect("d"))
                    .collect(),
            },
        },
    }
}

/// An honest round of `subject` observed in the window starting at `start`.
fn round(t: &FeedTemplate, subject: &Subject, start: u64) -> Transcript {
    let spec = t.instantiate(subject, start).expect("lag allows it");
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
        subject: subject.clone(),
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

#[test]
fn a_late_round_prices_from_the_same_clock_as_the_earliest() {
    let t = template();
    let s = subject("2026-10-08");
    // Record date Thu 2026-10-08: the earliest window is Sat 10-10, the
    // latest Sun 10-18 (the reviewer's L2API-R3-F1 case).
    let early = day("2026-10-10");
    let late = day("2026-10-18");
    let now = late + 1_000;
    let a = check(&t, &round(&t, &s, early), now).expect("earliest round verifies");
    let b = check(&t, &round(&t, &s, late), now).expect("latest round verifies");
    assert_eq!(a.fact, b.fact);
    assert_eq!(a.registry_fact, b.registry_fact);
    assert_eq!(a.settled_unix, early + 300, "the earliest window's end");
    assert_eq!(a.settled_unix, b.settled_unix, "a late round gains nothing");
    // A 7-day fresh window: free at the same moment, however late observed.
    let p = PricingPolicy {
        base_price_micro_usdc: 2_500,
        fresh_window_secs: 7 * DAY,
    };
    assert_eq!(p.price(now - a.settled_unix), 0);
    assert_eq!(p.price(now - b.settled_unix), 0);
}

/// Property: for random record dates and every window start the lag allows
/// (any second of each allowed day), the price clock is identical and equals
/// `decay_start`, a function of the template and the subject only.
#[test]
fn every_allowed_window_of_a_date_prices_from_one_clock() {
    let t = template();
    let SubjectRule::Dated { lag, .. } = &t.subject else {
        unreachable!()
    };
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let (first, last) = (lag.first_day, lag.last_day - 12);
    let mut dates_checked = 0;
    let mut rounds_checked = 0;
    for _ in 0..200 {
        let record = first
            + i64::try_from(next() % u64::try_from(last - first).expect("span")).expect("fits");
        let Some(earliest) = lag.earliest_window_day(record) else {
            continue; // weekend or holiday record: never observable
        };
        let date = {
            // Back to YYYY-MM-DD by searching the calendar (test only).
            let mut d = None;
            for y in 2026..=2026 {
                for m in 1..=12 {
                    for dd in 1..=31 {
                        let s = format!("{y:04}-{m:02}-{dd:02}");
                        if day_number(&s) == Some(record) {
                            d = Some(s);
                        }
                    }
                }
            }
            d.expect("a 2026 date")
        };
        let s = subject(&date);
        let clock = t
            .decay_start(&s, 0)
            .expect("a dated subject has a decay start");
        assert_eq!(
            clock,
            u64::try_from(earliest).expect("day") * DAY + t.window_secs
        );
        for k in 1..=i64::from(lag.max_calendar_days) {
            let wd = u64::try_from(record + k).expect("day");
            let start = wd * DAY + next() % (DAY - 1_000);
            if lag.check(record, start).is_err() {
                continue;
            }
            let tr = round(&t, &s, start);
            let now = tr.cfg.reveal_close + 1;
            let c = check(&t, &tr, now).expect("an allowed round verifies");
            assert_eq!(c.settled_unix, clock, "{date}: window day {k}");
            rounds_checked += 1;
        }
        dates_checked += 1;
    }
    assert!(dates_checked > 100, "{dates_checked}");
    assert!(rounds_checked > 500, "{rounds_checked}");
}
