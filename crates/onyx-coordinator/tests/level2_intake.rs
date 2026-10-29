//! Level-2 claim intake (L2R3-F1, F3, F5): a settled round backs exactly one
//! claim shape. A settled value cannot be reused for another metric, another
//! subject, another time or another snapshot, and a feed (one window) has one
//! result per store.

use onyx_coordinator::observe::SimulatedChain;
use onyx_coordinator::observe::{
    sign_commit, sign_reveal, test_keys, ObservationResults, ObservationRound,
    ObservationSettlement, ObserveConfig, ObserveError, Observer,
};
use onyx_core::claim::{EvidenceRef, LabelSetRef, MetricId, Subject};
use onyx_core::{
    keccak256, observation_commit, to_canonical_bytes, Answer, Claim, ClaimDecodeError, DataRights,
    EntityKind, FeedTemplate, Hash32, MetricInfo, Observation, ObservationKind, ObservationSpec,
    ProofLevel, Redistribution, Snapshot, SourceId, SourceRef, StaticRegistry, SubjectRule, Value,
    ValueKind, VerificationMethod,
};

const USD: &str = "example.page.usd_per_unit";
const EUR: &str = "example.page.eur_per_unit";
const ALPHA: &str = "entity:alpha";
const BETA: &str = "entity:beta";
const WINDOW_END: u64 = 1_050;

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.page".into()),
        attribution: None,
        terms: String::new(),
    }
}

fn spec() -> ObservationSpec {
    ObservationSpec {
        feed: USD.into(),
        metric: MetricId(USD.into()),
        subject: Subject::Entity(ALPHA.into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/usd_per_unit".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        },
        window_start: 1_000,
        window_end: WINDOW_END,
        min_observers: 3,
        rights: rights(),
    }
}

/// A store with `spec()`'s feed registered as its metric's template
/// (L2SRC-R2-F1).
fn store() -> ObservationResults {
    let (mut s, admin) = ObservationResults::new();
    s.register_feed(&admin, template(), "test feed", 0)
        .expect("register");
    s
}

fn template() -> FeedTemplate {
    let s = spec();
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
    }
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

/// A round of `spec()` whose observers (addresses starting at `first`)
/// reveal `values`.
fn round(store: &mut ObservationResults, first: u8, values: &[i128]) -> ObservationRound {
    let obs: Vec<Observer> = (0..values.len() as u8)
        .map(|i| Observer {
            address: test_keys::ordered(first + i).1,
            bond: 1_000,
        })
        .collect();
    // The pool is exactly the draw size, so the draw is the whole pool.
    let snapshot = store
        .announce_draw(&spec(), &cfg(), &obs, 10, &SimulatedChain::at(0))
        .expect("announce");
    let mut r = store
        .open_round(
            spec(),
            cfg(),
            snapshot,
            &SimulatedChain::with_beacon(Hash32([3; 32])),
        )
        .expect("open");
    let salt = Hash32([9; 32]);
    let os: Vec<Observation> = values
        .iter()
        .map(|v| Observation {
            answer: Answer::Number(*v),
            observed_at: 1_010,
            page_hash: keccak256(b"page"),
        })
        .collect();
    let (rid, attempt) = (r.round_id(), r.draw().attempt);
    for (i, o) in os.iter().enumerate() {
        let (sk, a) = test_keys::ordered(first + i as u8);
        let h = observation_commit(&rid, &a, o, &salt);
        let sig = sign_commit(&sk, &rid, attempt, &h).expect("sign");
        r.commit(1_070, a, h, sig).expect("commit");
    }
    for (i, o) in os.into_iter().enumerate() {
        let (sk, a) = test_keys::ordered(first + i as u8);
        let sig = sign_reveal(&sk, &rid, attempt, &o, &salt).expect("sign");
        r.reveal(1_150, a, o, salt, sig).expect("reveal");
    }
    r
}

fn registry() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    for m in [USD, EUR] {
        r.add_metric(
            m,
            MetricInfo {
                value_kind: ValueKind::Fixed { scale: 2 },
                needs_label_set: false,
            },
        )
        .expect("metric");
    }
    r.add_entity(ALPHA, EntityKind::Exchange).expect("alpha");
    r.add_entity(BETA, EntityKind::Exchange).expect("beta");
    r.add_source(rights()).expect("source");
    r
}

/// The one claim shape `s` backs.
fn honest(s: &ObservationSettlement) -> Claim {
    Claim {
        metric: MetricId(USD.into()),
        subject: Subject::Entity(ALPHA.into()),
        value: Value::Fixed {
            mantissa: 12_345,
            scale: 2,
        },
        source: SourceRef {
            source: SourceId("example.page".into()),
            snapshot: Snapshot::Document {
                published_unix: WINDOW_END,
                content_hash: s.result_hash(),
            },
        },
        method: VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: s.result_hash(),
        },
        label_set: None,
        rights: rights(),
        evidence: vec![],
        confidence: None,
    }
}

fn settled() -> (ObservationResults, ObservationSettlement) {
    let mut store = store();
    let r = round(&mut store, 1, &[12_345, 12_345, 12_345]);
    let s = store.settle(&r, 1_201).expect("settle");
    (store, s)
}

#[test]
fn the_honest_claim_is_accepted_at_level_two() {
    let (store, s) = settled();
    assert_eq!(
        store.accept_claim(&honest(&s), &registry()),
        Ok(ProofLevel::ReObserved)
    );
}

/// L2R3-F1 (review probe `probe_settled_value_backs_unrelated_metric_subject_and_time`):
/// one settled USD round no longer backs a claim on another metric, another
/// subject, another time, or another snapshot hash.
#[test]
fn a_settled_value_backs_no_other_metric_subject_time_or_snapshot() {
    let reg = registry();
    let (store, s) = settled();
    let not_backed = |c: &Claim| {
        assert!(
            matches!(
                store.accept_claim(c, &reg),
                Err(ObserveError::ClaimNotBacked(_))
            ),
            "{c:?}"
        )
    };

    let mut c = honest(&s);
    c.metric = MetricId(EUR.into());
    not_backed(&c);

    let mut c = honest(&s);
    c.subject = Subject::Entity(BETA.into());
    not_backed(&c);

    for published_unix in [WINDOW_END - 1, WINDOW_END + 1, 9_999_999_999] {
        let mut c = honest(&s);
        c.source.snapshot = Snapshot::Document {
            published_unix,
            content_hash: s.result_hash(),
        };
        not_backed(&c);
    }

    let mut c = honest(&s);
    c.source.snapshot = Snapshot::Document {
        published_unix: WINDOW_END,
        content_hash: keccak256(b"anything"),
    };
    not_backed(&c);

    let mut c = honest(&s);
    c.evidence = vec![EvidenceRef {
        kind: "page".into(),
        content_hash: keccak256(b"page"),
    }];
    not_backed(&c);

    // Another value, even within the feed's tolerance, is not the settled value.
    let mut c = honest(&s);
    c.value = Value::Fixed {
        mantissa: 12_346,
        scale: 2,
    };
    not_backed(&c);
}

/// L2R3-F1: a level-2 value depends on no label set.
#[test]
fn a_level_two_claim_carries_no_label_set() {
    let mut reg = registry();
    reg.add_metric(
        "example.page.labelled",
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: 2 },
            needs_label_set: true,
        },
    )
    .expect("metric");
    let (store, s) = settled();
    let mut c = honest(&s);
    c.label_set = Some(LabelSetRef {
        name: "test.set".into(),
        version: 1,
        content_hash: keccak256(b"set"),
    });
    c.metric = MetricId("example.page.labelled".into());
    assert!(matches!(
        store.accept_claim(&c, &reg),
        Err(ObserveError::ClaimNotBacked(_))
    ));
}

/// L2R3-F3: `Claim::decode` refuses level-2 claims; the observation intake
/// decodes and checks them.
#[test]
fn level_two_bytes_enter_only_through_the_intake() {
    let reg = registry();
    let (store, s) = settled();
    let bytes = to_canonical_bytes(&honest(&s)).expect("encode");
    assert!(matches!(
        Claim::decode(&bytes, &reg),
        Err(ClaimDecodeError::NeedsObservationIntake)
    ));
    assert_eq!(store.decode_claim(&bytes, &reg), Ok(honest(&s)));
    let mut other = honest(&s);
    other.subject = Subject::Entity(BETA.into());
    let bytes = to_canonical_bytes(&other).expect("encode");
    assert!(matches!(
        store.decode_claim(&bytes, &reg),
        Err(ObserveError::ClaimNotBacked(_))
    ));
}

/// L2R3-F5 (review probes `probe_entries_can_be_produced_twice` and
/// `probe_clone_drops_a_reveal`): a round settles only through a store
/// (`ObservationRound::settle` is crate-private, so "settle outside any store"
/// no longer compiles), once per round, and one feed (one window) has one
/// result per store: a second observer set for the same window is refused.
#[test]
fn one_result_per_round_and_per_feed() {
    let mut store = store();
    let r = round(&mut store, 1, &[12_345, 12_345, 12_345]);
    store.settle(&r, 1_201).expect("first");
    assert_eq!(store.settle(&r, 1_202), Err(ObserveError::AlreadySettled));
    assert_eq!(
        store.settle(&r.clone(), 1_300),
        Err(ObserveError::AlreadySettled)
    );
    // Same feed and window, other observers, other answers: refused.
    // Opened elsewhere: this store refuses a second draw for a settled fact.
    let competing = round(&mut self::store(), 50, &[99_999, 99_999, 99_999]);
    assert_ne!(competing.round_id(), r.round_id());
    assert_eq!(
        store.settle(&competing, 1_201),
        Err(ObserveError::FeedAlreadySettled)
    );
}
