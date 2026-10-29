//! Review 4 (L2API-R4): the store's template is pinned to the reviewed
//! registration (F1). The lag is not part of `ObservationSpec`, so a store
//! template with another lag rebuilds the same spec, round id and result;
//! before the pin it loaded and the server priced it from a later decay
//! start. Also the coverage edges of `DateLag::earliest_window_day`.
use onyx_api::client::{verify_observation, ObservationExpect, VerifyError};
use onyx_api::observations::{
    check, check_registered, registry_metric_calldata, ObservationError, ObservationStore,
    TemplatePins, Transcript,
};
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

fn template(min_business_days: u16) -> FeedTemplate {
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
                min_business_days,
                max_calendar_days: 10,
                first_day: day_number("2026-01-01").expect("d"),
                last_day: day_number("2026-12-31").expect("d"),
                holidays: vec![day_number("2026-10-12").expect("d")],
            },
        },
    }
}

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
fn a_store_template_with_another_lag_is_refused_at_load() {
    let registered = template(1);
    let forged = template(5); // the operator edits only the store's lag
    let s = Subject::Aggregate("test.rec:2026-10-08".into());
    // A round the opener held to the last allowed day (Sun 10-18).
    let tr = round(&registered, &s, day("2026-10-18"));
    let now = tr.cfg.reveal_close + 1;
    let a = check(&registered, &tr, now).expect("registered");
    let b = check(&forged, &tr, now).expect("the same round checks under the forged template");
    // Same spec, round and result: only the template id tells them apart, and
    // the forged lag would move the server's clock by 7 days.
    assert_eq!(
        (a.spec.id(), a.round_id, a.result),
        (b.spec.id(), b.round_id, b.result)
    );
    assert_ne!(registered.id(), forged.id());
    assert_eq!(b.settled_unix - a.settled_unix, 7 * DAY);
    let pins = TemplatePins::from([(registered.metric.0.clone(), registered.id())]);
    let forged_store =
        ObservationStore::to_json(&forged, std::slice::from_ref(&tr), None).to_string();
    assert!(matches!(
        ObservationStore::from_json(&forged_store, now, &pins),
        Err(ObservationError::TemplateNotPinned { .. })
    ));
    // No pin for the metric: refused too (fail closed).
    let honest =
        ObservationStore::to_json(&registered, std::slice::from_ref(&tr), None).to_string();
    assert!(matches!(
        ObservationStore::from_json(&honest, now, &TemplatePins::new()),
        Err(ObservationError::TemplateNotPinned { .. })
    ));
    // The pinned template loads, and prices from the earliest window.
    let feed = ObservationStore::from_json(&honest, now, &pins).expect("pinned store loads");
    let rec = feed
        .record("test.rec:2026-10-08", None, None)
        .expect("record");
    assert_eq!(rec.checked.settled_unix, day("2026-10-10") + 300);
    // The client still refuses a forged body (defence in depth).
    let mut forged_feed = onyx_api::observations::ObservationFeed::new(forged).expect("feed");
    forged_feed
        .add(tr, now)
        .expect("an unpinned feed would add it");
    let body = forged_feed.body(
        forged_feed
            .record("test.rec:2026-10-08", None, None)
            .expect("rec"),
    );
    let r = verify_observation(
        &body,
        &ObservationExpect {
            template: &registered,
            subject: &s,
            window_end: None,
            min_window_end: 0,
            now,
        },
    );
    assert!(matches!(r, Err(VerifyError::Observation(_))), "{r:?}");
}

/// The on-chain registration check: kind, template and registeredAt words.
#[test]
fn the_registry_must_hold_exactly_the_pinned_template_and_kind() {
    let t = template(1);
    let word = |v: u64| {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&v.to_be_bytes());
        w
    };
    let ret = |kind: u64, tpl: [u8; 32], at: u64| [word(kind), tpl, word(at)].concat();
    let dated = onyx_core::onchain::MetricKind::of(&t) as u64;
    assert_eq!(dated, 3);
    check_registered(&t, &ret(dated, t.id().0, 42)).expect("registered");
    assert!(
        check_registered(&t, &ret(2, t.id().0, 42)).is_err(),
        "Feed kind for a dated template"
    );
    assert!(
        check_registered(&t, &ret(dated, template(5).id().0, 42)).is_err(),
        "other template"
    );
    assert!(
        check_registered(&t, &ret(dated, t.id().0, 0)).is_err(),
        "not registered"
    );
    assert!(
        check_registered(&t, &ret(dated, t.id().0, 42)[..64]).is_err(),
        "short"
    );
    let mut dirty = ret(dated, t.id().0, 42);
    dirty[0] = 1;
    assert!(check_registered(&t, &dirty).is_err(), "kind above uint8");
    // The getter's selector and argument.
    let d = registry_metric_calldata(&t);
    assert_eq!(&d[..4], &keccak256(b"metrics(bytes32)").0[..4]);
    assert_eq!(&d[4..], &onyx_core::onchain::metric_key(&t.metric).0);
}

/// `earliest_window_day` vs `check` at the coverage edges.
#[test]
fn earliest_window_day_is_the_first_day_check_accepts_at_every_edge() {
    let t = template(1);
    let SubjectRule::Dated { lag, .. } = &t.subject else {
        unreachable!()
    };
    // Every day of the coverage (and 15 days each side): earliest is the
    // minimum day check accepts, and decay_start agrees.
    for rec in (lag.first_day - 15)..=(lag.last_day + 15) {
        let brute = (-20..=40i64)
            .map(|k| rec + k)
            .filter(|d| *d >= 0)
            .find(|d| {
                lag.check(rec, u64::try_from(*d).expect("day") * DAY)
                    .is_ok()
            });
        assert_eq!(lag.earliest_window_day(rec), brute, "record {rec}");
        // Any second of the day agrees with midnight.
        if let Some(d) = brute {
            let base = u64::try_from(d).expect("day") * DAY;
            assert!(lag.check(rec, base + DAY - 1).is_ok());
            assert!(lag.check(rec, base - 1).is_err(), "record {rec}");
        }
    }
    // Dec 31 record: no window inside the coverage, so no decay start and no
    // instantiation.
    let s = Subject::Aggregate("test.rec:2026-12-31".into());
    assert_eq!(t.decay_start(&s, 0), None);
    for k in 1..=10 {
        assert!(t.instantiate(&s, day("2026-12-31") + k * DAY).is_err());
    }
    let s = Subject::Aggregate("test.rec:2027-01-04".into());
    assert_eq!(t.decay_start(&s, 0), None);
}
