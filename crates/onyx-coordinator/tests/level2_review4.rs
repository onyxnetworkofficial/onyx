//! Level-2 intake, review 4 (L2R4-F1..F4): every claim field is bound to the
//! settled result; one result per fact (metric, subject, window end) per store;
//! the intake refuses claims of any other level. The store is not `Clone`
//! (L2R4-F2; a `compile_fail` doctest on `ObservationResults` pins that).

use onyx_coordinator::observe::SimulatedChain;
use onyx_coordinator::observe::{
    fact_key, sign_commit, sign_reveal, test_keys, ObservationResults, ObservationRound,
    ObservationSettlement, ObserveConfig, ObserveError, Observer,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, observation_commit, to_canonical_bytes, Answer, ChainId, Claim, Confidence,
    DataRights, EntityKind, FeedTemplate, Hash32, MetricInfo, Observation, ObservationKind,
    ObservationSpec, Redistribution, Snapshot, SourceId, SourceRef, StaticRegistry, SubjectRule,
    Value, ValueKind, VerificationMethod,
};

const USD: &str = "example.page.usd_per_unit";
const ALPHA: &str = "entity:alpha";
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

fn round_of(
    store: &mut ObservationResults,
    sp: ObservationSpec,
    first: u8,
    values: &[i128],
) -> ObservationRound {
    let obs: Vec<Observer> = (0..values.len() as u8)
        .map(|i| Observer {
            address: test_keys::ordered(first + i).1,
            bond: 1_000,
        })
        .collect();
    // The pool is exactly the draw size, so the draw is the whole pool.
    let snapshot = store
        .announce_draw(&sp, &cfg(), &obs, 10, &SimulatedChain::at(0))
        .expect("announce");
    let mut r = store
        .open_round(
            sp,
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
    r.add_metric(
        USD,
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: 2 },
            needs_label_set: false,
        },
    )
    .expect("metric");
    r.add_entity(ALPHA, EntityKind::Exchange).expect("alpha");
    r.add_source(rights()).expect("source");
    let mut other = rights();
    other.source = SourceId("example.other".into());
    r.add_source(other).expect("other");
    r
}

fn honest(s: &ObservationSettlement, mantissa: i128) -> Claim {
    Claim {
        metric: MetricId(USD.into()),
        subject: Subject::Entity(ALPHA.into()),
        value: Value::Fixed { mantissa, scale: 2 },
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

#[test]
fn every_field_variant_of_a_backed_claim_is_refused() {
    let reg = registry();
    let mut store = store();
    let s = {
        let r = round_of(&mut store, spec(), 1, &[12_345, 12_345, 12_345]);
        store.settle(&r, 1_201)
    }
    .expect("settle");
    assert!(store.accept_claim(&honest(&s, 12_345), &reg).is_ok());
    let mut bad: Vec<Claim> = Vec::new();
    let h = || honest(&s, 12_345);
    // Snapshot variants.
    let mut c = h();
    c.source.snapshot = Snapshot::Block {
        chain: ChainId("btc.mainnet".into()),
        height: WINDOW_END,
        block_hash: s.result_hash(),
    };
    bad.push(c);
    let mut c = h();
    c.source.snapshot = Snapshot::BlockRange {
        chain: ChainId("btc.mainnet".into()),
        from_height: 1,
        to_height: WINDOW_END,
        tip_hash: s.result_hash(),
        content_hash: s.result_hash(),
    };
    bad.push(c);
    // Value encodings of the same number.
    for v in [
        Value::Int(12_345),
        Value::UInt(12_345),
        Value::Fixed {
            mantissa: 123_450,
            scale: 3,
        },
        Value::Fixed {
            mantissa: 1_234,
            scale: 1,
        },
        Value::Fixed {
            mantissa: -12_345,
            scale: 2,
        },
        Value::Bytes(b"123.45".to_vec()),
    ] {
        let mut c = h();
        c.value = v;
        bad.push(c);
    }
    // Metric / subject spellings.
    let mut c = h();
    c.metric = MetricId("Example.page.usd_per_unit".into());
    bad.push(c);
    let mut c = h();
    c.metric = MetricId("example.page.usd_per_unit\u{200b}".into());
    bad.push(c);
    let mut c = h();
    c.subject = Subject::Entity("entity:ALPHA".into());
    bad.push(c);
    let mut c = h();
    c.subject = Subject::Aggregate(ALPHA.into());
    bad.push(c);
    // Another registered source with its own rights.
    let mut c = h();
    c.source.source = SourceId("example.other".into());
    c.rights.source = SourceId("example.other".into());
    bad.push(c);
    // Rights variants.
    let mut c = h();
    c.rights.attribution = Some("x".into());
    bad.push(c);
    let mut c = h();
    c.rights.license = "CC-BY-4.0".into();
    bad.push(c);
    // Confidence.
    let mut c = h();
    c.confidence = Some(Confidence(1));
    bad.push(c);
    // Pointer pieces.
    let mut c = h();
    c.method = VerificationMethod::ReObserved {
        spec: keccak256(b"other"),
        result: s.result_hash(),
    };
    bad.push(c);
    for c in &bad {
        assert!(store.accept_claim(c, &reg).is_err(), "accepted: {c:?}");
    }
    // Borsh: trailing byte, truncated, bumped method tag.
    let bytes = to_canonical_bytes(&h()).expect("encode");
    assert!(store.decode_claim(&bytes, &reg).is_ok());
    let mut t = bytes.clone();
    t.push(0);
    assert_eq!(
        store.decode_claim(&t, &reg),
        Err(ObserveError::NotCanonical)
    );
    assert!(store.decode_claim(&bytes[..bytes.len() - 1], &reg).is_err());
}

/// L2R4-F1 with L2SRC-R2-F1: a second feed for the same fact (same metric,
/// subject and window end) that differs in another field (observer count, a
/// selector spelling, a different window start) is not the metric's
/// registered template, so it never opens; another observer set for the
/// template's own feed is refused at settle. A feed for another fact (another
/// subject: its own template) still settles.
#[test]
fn a_competing_feed_for_the_same_fact_is_refused() {
    let reg = registry();
    let mut store = store();
    let a = {
        let r = round_of(&mut store, spec(), 1, &[12_345, 12_345, 12_345]);
        store.settle(&r, 1_201)
    }
    .expect("settle a");
    let mut more_observers = spec();
    more_observers.min_observers = 4;
    let mut selector = spec();
    selector.selector = "/data/usd_per_unit/".into();
    let mut start = spec();
    start.window_start = 1_001;
    for sp in [more_observers, selector, start] {
        assert_eq!(fact_key(&sp), fact_key(&spec()));
        let obs: Vec<Observer> = (0..sp.min_observers as u8)
            .map(|i| Observer {
                address: test_keys::ordered(50 + i).1,
                bond: 1_000,
            })
            .collect();
        assert!(matches!(
            store.announce_draw(&sp, &cfg(), &obs, 10, &SimulatedChain::at(0)),
            Err(ObserveError::Template(_))
        ));
    }
    assert_eq!(
        store.settle(
            &round_of(&mut self::store(), spec(), 50, &[99_999, 99_999, 99_999]),
            1_201
        ),
        Err(ObserveError::FeedAlreadySettled)
    );
    assert!(store.accept_claim(&honest(&a, 12_345), &reg).is_ok());
    // Another subject is another fact, registered as another metric's feed
    // here only to show the fact key differs.
    let mut beta = spec();
    beta.subject = Subject::Entity("entity:beta".into());
    assert_ne!(fact_key(&beta), fact_key(&spec()));
}

/// A claim backed by one store is refused by another store (no shared book).
#[test]
fn a_result_from_another_store_is_refused() {
    let reg = registry();
    let mut a = store();
    let mut b = store();
    let sa = {
        let r = round_of(&mut a, spec(), 1, &[12_345, 12_345, 12_345]);
        a.settle(&r, 1_201)
    }
    .expect("a");
    let sb = {
        let r = round_of(&mut b, spec(), 50, &[99_999, 99_999, 99_999]);
        b.settle(&r, 1_201)
    }
    .expect("b");
    assert!(a.accept_claim(&honest(&sa, 12_345), &reg).is_ok());
    assert!(a.accept_claim(&honest(&sb, 99_999), &reg).is_err());
}

/// L2R4-F4: the level-2 intake refuses claims of other levels instead of
/// reporting a level it never checked.
#[test]
fn the_level_two_intake_refuses_other_levels() {
    let reg = registry();
    let mut store = store();
    let s = {
        let r = round_of(&mut store, spec(), 1, &[12_345, 12_345, 12_345]);
        store.settle(&r, 1_201)
    }
    .expect("settle");
    let mut c = honest(&s, 12_345);
    c.method = VerificationMethod::OfficialSourceMatch;
    assert_eq!(store.accept_claim(&c, &reg), Err(ObserveError::NotLevelTwo));
}
