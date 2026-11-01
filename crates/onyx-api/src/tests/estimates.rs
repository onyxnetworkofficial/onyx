use super::*;
use onyx_core::{ErrorBand, Estimate, MetricId, Subject, Value};

const EST: &str = "estimate.test.daily_mean";

fn est(subject: &str, as_of: u64, error: ErrorBand) -> Estimate {
    Estimate {
        metric: MetricId(EST.into()),
        subject: Subject::Aggregate(subject.into()),
        as_of,
        value: Value::Fixed {
            mantissa: 1_234,
            scale: 2,
        },
        method: "onyx.mean-ci95.v1".into(),
        method_hash: keccak256(b"the method's text"),
        error,
        inputs: vec![keccak256(b"input")],
    }
}

fn band() -> ErrorBand {
    ErrorBand::Interval {
        lo: Value::Fixed {
            mantissa: 1_000,
            scale: 2,
        },
        hi: Value::Fixed {
            mantissa: 1_500,
            scale: 2,
        },
        coverage_ppm: 950_000,
    }
}

#[test]
fn a_checked_catalog_refuses_an_estimate_metric() {
    let f = build(Setup::default());
    let mut c = f.state.catalog.write().expect("lock");
    // A shard spec cannot be listed under an estimate metric...
    let m = meta(9, 100, b"params est", 1);
    let mut hand = m.clone();
    hand.metric = EST.into();
    c.add_shard(hand.clone());
    assert!(c.shard(hand.round, &hand.shard).is_none(), "never added");
    assert!(c.for_metric(EST, None).is_empty());
    // ...and an estimate cannot be filed under a checked metric.
    let mut wrong = est("test.net", 1, band());
    wrong.metric = MetricId("test.flow".into());
    assert!(c.add_estimates(vec![wrong]).is_err());
    // A malformed estimate is refused whole.
    let mut bad = est("test.net", 1, band());
    bad.method_hash = Hash32::ZERO;
    assert!(c
        .add_estimates(vec![est("test.other", 1, band()), bad])
        .is_err());
    assert!(!c.has_estimates(EST), "nothing added on a refusal");
}

#[test]
fn from_spec_refuses_an_estimate_metric() {
    let spec = onyx_core::ShardSpec {
        recipe: None,
        runtime: None,
        label_set: None,
        source: source_ref(100),
        schema: keccak256(b"schema"),
        rights_hash: rights().hash(),
        params: b"p".to_vec(),
        registry_root: keccak256(b"registry"),
    };
    assert!(ShardMeta::from_spec(1, &spec, "test.flow", rights()).is_some());
    assert!(ShardMeta::from_spec(1, &spec, EST, rights()).is_none());
}

#[tokio::test(start_paused = true)]
async fn estimates_route_answers_method_and_error() {
    let f = fixture(false, None);
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_estimates(vec![
            est("test.net", 10, band()),
            // An older estimate of the same subject never replaces a newer one.
            est("test.net", 5, ErrorBand::Unknown),
            est("test.unknown", 10, ErrorBand::Unknown),
        ])
        .expect("estimates");
    let (s, h, b) = get(&f, &format!("/v1/estimates/{EST}?subject=test.net"), &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["value"], "12.34");
    assert_eq!(b["level"], "E");
    assert!(b["proofLevel"].is_null() && b["ladder"].is_null());
    assert_eq!(b["checked"], false);
    assert_eq!(b["method"], "onyx.mean-ci95.v1");
    assert_eq!(b["error"]["kind"], "interval");
    assert_eq!(b["error"]["lo"], "10.00");
    assert_eq!(b["error"]["hi"], "15.00");
    assert_eq!(b["error"]["coveragePpm"], 950_000);
    assert_eq!(b["asOf"], 10);
    assert!(h.get(header::ETAG).is_some());
    // R-EST-3: an unknown error is stated, never left out.
    let (s, _, b) = get(
        &f,
        &format!("/v1/estimates/{EST}?subject=test.unknown"),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["error"]["kind"], "unknown");
    // A subject with no estimate.
    let (s, _, b) = get(&f, &format!("/v1/estimates/{EST}?subject=test.none"), &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(code(&b), Some("unknown_subject"));
}

/// The "API refuses to mix them" test (NL3 Done-when): an estimate never
/// answers on a checked route, and a checked metric never on the
/// estimates route.
#[tokio::test(start_paused = true)]
async fn a_value_route_refuses_an_estimate_metric() {
    let f = fixture(false, None);
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_estimates(vec![est("test.net", 10, band())])
        .expect("estimates");
    for uri in [
        format!("/v1/metrics/{EST}?subject=test.net"),
        format!("/v1/observations/{EST}?subject=test.net"),
    ] {
        let (s, _, b) = get(&f, &uri, &[]).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}: {b}");
        assert_eq!(code(&b), Some("unknown_metric"), "{uri}");
    }
    let (s, _, b) = get(&f, "/v1/estimates/test.flow?subject=entity:a", &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(code(&b), Some("unknown_metric"));
}
