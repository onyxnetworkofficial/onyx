//! Time alignment of market data (ADR-0058, contributor loop CL8): the bucket rule
//! (`SubjectRule::Bucketed`), the lower median with its band and disagreement flag over venue
//! facts (`onyx_core::aligned`), and the four clocks every answer states.
//!
//! Golden vectors: `fixtures/aligned/vectors.json` (the bucket rule's accepts and refusals, the
//! composite's id, and every alignment's answer, clocks and answer id). Regenerate only with an
//! ADR change: `ONYX_BLESS=1 cargo test -p onyx-core --test aligned`.

use onyx_core::aligned::{
    aligned, check_answer, lower_median, spread, AlignError, AlignedAnswer, Composite, Reason,
    Status, VenueFact, VenueInput, VenueRef, DISAGREE_PPM, MIN_VENUES,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::feed::{Bucket, BUCKET_SIZES, MAX_BUCKET_RANGE_SECS, MIN_BUCKET_DELAY_SECS};
use onyx_core::onchain::MetricKind;
use onyx_core::{
    keccak256, DataRights, FeedTemplate, ObservationKind, Redistribution, SourceId, SubjectRule,
    TemplateError,
};
use serde_json::{json, Value as Json};

/// 2026-10-27T00:00:00Z: the hourly bucket every case names.
const B0: u64 = 1_793_059_200;
const H: u64 = 3_600;

fn vectors_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/aligned/vectors.json")
}

fn template(min_delay_secs: u32, max_delay_secs: u32, url: &str) -> FeedTemplate {
    FeedTemplate {
        feed: "venue.a.aaa_usd.1h".into(),
        metric: MetricId("venue.a.aaa_usd.1h".into()),
        fetcher: keccak256(b"fetcher"),
        url_template: url.into(),
        selector: "/candles/0/close".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 120,
        min_observers: 5,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("venue.a".into()),
            attribution: None,
            terms: String::new(),
        },
        subject: SubjectRule::Bucketed {
            prefix: "venue.a.aaa_usd.1h".into(),
            bucket_secs: 3_600,
            min_delay_secs,
            max_delay_secs,
        },
    }
}

const URL: &str = "https://a.venue.example/v1/candles?pair=AAA-USD&interval=1h&start={bucket_start}&start_ms={bucket_start_ms}";

fn hourly() -> FeedTemplate {
    template(300, 3_900, URL)
}

fn bucket_subject(start: &str) -> Subject {
    Subject::Aggregate(format!("venue.a.aaa_usd.1h:{start}"))
}

/// One bucket-rule case: (name, template, subject suffix, window start).
type RuleCase = (&'static str, FeedTemplate, String, u64);

fn rule_cases() -> Vec<RuleCase> {
    let end = B0 + H;
    let t = hourly;
    let s = |x: u64| x.to_string();
    let mut daily = template(300, 3_900, URL);
    if let SubjectRule::Bucketed { bucket_secs, .. } = &mut daily.subject {
        *bucket_secs = 86_400;
    }
    vec![
        (
            "first second of the observable range",
            t(),
            s(B0),
            end + 300,
        ),
        (
            "last second of the observable range",
            t(),
            s(B0),
            end + 3_900,
        ),
        (
            "one second before the bucket settles (late prints)",
            t(),
            s(B0),
            end + 299,
        ),
        ("inside the bucket itself", t(), s(B0), B0 + 1_800),
        ("one second after the range", t(), s(B0), end + 3_901),
        ("a start off the grid", t(), s(B0 + 1), end + 300),
        (
            "a start with a leading zero",
            t(),
            format!("0{B0}"),
            end + 300,
        ),
        ("a start with a sign", t(), format!("+{B0}"), end + 300),
        ("a start in milliseconds", t(), s(B0 * 1_000), end + 300),
        (
            "a date instead of a start",
            t(),
            "2026-10-27".into(),
            end + 300,
        ),
        (
            "a daily bucket on the day grid",
            daily.clone(),
            s(B0),
            B0 + 86_400 + 300,
        ),
        (
            "an hour start on a daily grid",
            daily,
            s(B0 + H),
            B0 + 2 * 86_400,
        ),
    ]
}

fn rule_result(t: &FeedTemplate, subject: &Subject, window_start: u64) -> Json {
    match t.instantiate(subject, window_start) {
        Ok(spec) => {
            json!({ "url": spec.url, "window_end": spec.window_end, "spec_id": spec.id().to_string() })
        }
        Err(e) => json!({ "error": e.to_string() }),
    }
}

fn venue(metric: &str, venue: &str, host: &str) -> VenueRef {
    VenueRef {
        metric: metric.into(),
        venue: venue.into(),
        host: host.into(),
        terms_row: format!("row-{venue}"),
        unit: "USD".into(),
        scale: 2,
        bucket_secs: 3_600,
        redistribution: Redistribution::Allowed,
    }
}

fn composite() -> Composite {
    Composite {
        metric: "aligned.aaa_usd.1h".into(),
        unit: "USD".into(),
        scale: 2,
        bucket_secs: 3_600,
        venues: ["a", "b", "c", "d", "e"]
            .iter()
            .map(|v| {
                venue(
                    &format!("venue.{v}.aaa_usd.1h"),
                    &format!("venue.{v}"),
                    &format!("{v}.venue.example"),
                )
            })
            .collect(),
        min_venues: MIN_VENUES,
        disagree_ppm: DISAGREE_PPM,
    }
}

/// An honest fact: the venue returned this bucket, the window opened 300 s after it closed,
/// observers fetched inside the window, the chain recorded it after.
fn fact(value: i128) -> VenueFact {
    let ws = B0 + H + 300;
    VenueFact {
        value,
        venue_time: B0,
        window_start: ws,
        window_end: ws + 120,
        observed_from: ws + 10,
        observed_to: ws + 100,
        recorded_block: 4_242,
        recorded_at: ws + 400,
    }
}

fn rec(values: [i128; 5]) -> Vec<VenueInput> {
    values
        .iter()
        .map(|v| VenueInput::Recorded(fact(*v)))
        .collect()
}

fn inputs(c: &Composite, ins: Vec<VenueInput>) -> Vec<(String, VenueInput)> {
    c.venues.iter().map(|v| v.metric.clone()).zip(ins).collect()
}

const P: i128 = 6_701_250; // 67012.50 USD

fn alignment_cases() -> Vec<(&'static str, Vec<VenueInput>)> {
    let r = |v| VenueInput::Recorded(fact(v));
    let skew = |v, f: &dyn Fn(&mut VenueFact)| {
        let mut x = fact(v);
        f(&mut x);
        VenueInput::Recorded(x)
    };
    use VenueInput::{Lapsed, NoResult, NotObserved, RightsWithheld, Unavailable};
    vec![
        (
            "five venues agree, with a tie",
            rec([P, P + 50, P - 150, P - 50, P]),
        ),
        (
            "one far venue (3% high)",
            rec([P, P + 50, P * 103 / 100, P - 50, P]),
        ),
        (
            "a wash print at the bucket edge moves one thin venue's close 0.8%",
            rec([P, P + 50, P - 50, P * 1_008 / 1_000, P]),
        ),
        (
            "two venues pushed the same way do not move the median",
            rec([P, P * 103 / 100, P * 103 / 100, P - 50, P]),
        ),
        (
            "empty bucket: no venue observed yet (pending)",
            vec![NotObserved; 5],
        ),
        ("every venue lapsed", vec![Lapsed; 5]),
        (
            "a paid subset: three of five observed, two still observable (pending)",
            vec![r(P), r(P * 2), r(P * 2), NotObserved, NotObserved],
        ),
        (
            "the same subset once the others lapsed: no value",
            vec![r(P), r(P * 2), r(P * 2), Lapsed, Lapsed],
        ),
        (
            "three of five included, two unavailable: a majority",
            vec![r(P), r(P + 10), r(P - 10), Unavailable, Unavailable],
        ),
        (
            "two of five included is never a value",
            vec![r(P), r(P + 10), Unavailable, Unavailable, NoResult],
        ),
        (
            "two venues only: insufficient",
            vec![r(P), r(P + 10), Lapsed, Unavailable, NoResult],
        ),
        (
            "a halted venue is listed unavailable",
            vec![r(P), Unavailable, r(P + 20), r(P - 20), r(P)],
        ),
        (
            "a venue clock 5 s fast",
            vec![
                r(P),
                r(P),
                skew(P * 2, &|f| f.venue_time = B0 + 5),
                r(P + 10),
                r(P - 10),
            ],
        ),
        (
            "a venue answering the previous bucket",
            vec![
                r(P),
                r(P),
                skew(P, &|f| f.venue_time = B0 - H),
                r(P + 10),
                r(P - 10),
            ],
        ),
        (
            "a fact observed before the bucket closed (late prints)",
            vec![
                r(P),
                r(P),
                skew(P, &|f| {
                    f.window_start = B0 + H - 60;
                    f.observed_from = B0 + H - 50;
                }),
                r(P + 10),
                r(P - 10),
            ],
        ),
        (
            "a fact recorded before its window closed",
            vec![
                r(P),
                r(P),
                skew(P, &|f| f.recorded_at = f.window_end - 1),
                r(P + 10),
                r(P - 10),
            ],
        ),
        (
            "a reveal outside its window",
            vec![
                r(P),
                r(P),
                skew(P, &|f| f.observed_to = f.window_end + 1),
                r(P + 10),
                r(P - 10),
            ],
        ),
        (
            "even count: the lower median",
            vec![r(P), r(P + 1_000), r(P + 1_000), r(P), RightsWithheld],
        ),
        ("all equal: no spread", rec([P; 5])),
        (
            "exactly at the threshold: no flag",
            vec![
                r(1_000_000),
                r(1_000_000),
                r(1_005_000),
                Unavailable,
                Unavailable,
            ],
        ),
        (
            "one unit over the threshold: flag",
            vec![
                r(1_000_000),
                r(1_000_000),
                r(1_005_001),
                Unavailable,
                Unavailable,
            ],
        ),
        (
            "a zero median with a spread",
            vec![r(-1), r(0), r(1), Unavailable, Unavailable],
        ),
        (
            "every reason at once",
            vec![
                Lapsed,
                Unavailable,
                NoResult,
                RightsWithheld,
                skew(P, &|f| f.venue_time = 0),
            ],
        ),
    ]
}

fn dec(m: i128, scale: u8) -> String {
    let neg = m < 0;
    let a = m.unsigned_abs().to_string();
    let s = usize::from(scale);
    let a = format!("{a:0>width$}", width = s + 1);
    let (i, f) = a.split_at(a.len() - s);
    format!(
        "{}{i}{}{f}",
        if neg { "-" } else { "" },
        if s > 0 { "." } else { "" }
    )
}

/// The answer as the vectors (and every client) show it: the four clocks per venue.
fn answer_json(a: &AlignedAnswer) -> Json {
    let venues: Vec<Json> = a
        .venues
        .iter()
        .map(|l| match &l.outcome {
            Ok(f) => json!({
                "metric": l.metric, "venue": l.venue, "included": true,
                "value": dec(f.value, a.scale),
                "venueTime": f.venue_time,
                "observedFrom": f.observed_from, "observedTo": f.observed_to,
                "recordedAt": { "block": f.recorded_block, "time": f.recorded_at },
            }),
            Err(r) => json!({ "metric": l.metric, "venue": l.venue, "included": false, "reason": r.code() }),
        })
        .collect();
    json!({
        "status": match a.status {
            Status::Aligned => "aligned",
            Status::Insufficient => "insufficient",
            Status::Pending => "pending",
        },
        "bucket": { "start": a.bucket.start, "end": a.bucket.end },
        "asOf": a.as_of(),
        "value": a.value.map(|v| dec(v, a.scale)),
        "band": a.band.map(|(lo, hi)| json!({ "min": dec(lo, a.scale), "max": dec(hi, a.scale) })),
        "spreadPpm": a.spread_ppm.map(|p| p.to_string()),
        "disagreement": a.disagreement,
        "unit": a.unit,
        "rights": format!("{:?}", a.rights).to_lowercase(),
        "venues": venues,
        "answerId": a.id().to_string(),
    })
}

fn render() -> String {
    let rules: Vec<Json> = rule_cases()
        .iter()
        .map(|(name, t, start, ws)| {
            let subject = bucket_subject(start);
            json!({
                "name": name,
                "subject_rule": format!("{:?}", t.subject),
                "template_id": t.id().to_string(),
                "subject": format!("venue.a.aaa_usd.1h:{start}"),
                "window_start": ws,
                "result": rule_result(t, &subject, *ws),
            })
        })
        .collect();
    let c = composite();
    let aligns: Vec<Json> = alignment_cases()
        .into_iter()
        .map(|(name, ins)| {
            let a = aligned(&c, B0, &inputs(&c, ins)).expect("aligned");
            json!({ "name": name, "answer": answer_json(&a) })
        })
        .collect();
    let v = json!({
        "about": "Golden vectors of time alignment (ADR-0058, contributor loop CL8): the bucketed subject rule, the composite id, and each alignment's answer (lower median, band, disagreement flag, the four clocks, answer id). Regenerate only with an ADR change: ONYX_BLESS=1 cargo test -p onyx-core --test aligned.",
        "bucket_rule": rules,
        "composite": { "metric": c.metric, "id": c.id().to_string(), "min_venues": c.min_venues, "disagree_ppm": c.disagree_ppm },
        "bucket_start": B0,
        "alignments": aligns,
    });
    format!("{}\n", serde_json::to_string_pretty(&v).expect("json"))
}

#[test]
fn aligned_golden_vectors_match() {
    let text = render();
    if std::env::var("ONYX_BLESS").is_ok() {
        std::fs::create_dir_all(vectors_path().parent().expect("dir")).expect("mkdir");
        std::fs::write(vectors_path(), &text).expect("write");
    }
    let file = std::fs::read_to_string(vectors_path()).expect("fixtures/aligned/vectors.json");
    assert_eq!(
        file, text,
        "the vectors moved: regenerate only with an ADR change"
    );
}

// ---- The bucket rule (ADR-0058 item 2) ----

/// A bucketed subject names one closed bucket of the UTC grid; the URL is filled with its start
/// in seconds and milliseconds.
#[test]
fn bucketed_subjects_fill_the_url_with_the_bucket_start() {
    let t = hourly();
    t.validate().expect("valid");
    let spec = t
        .instantiate(&bucket_subject(&B0.to_string()), B0 + H + 300)
        .expect("in range");
    assert_eq!(
        spec.url,
        format!("https://a.venue.example/v1/candles?pair=AAA-USD&interval=1h&start={B0}&start_ms={B0}000")
    );
    assert!(t.matches(&spec).is_ok());
    assert_eq!(
        t.bucket_of(&bucket_subject(&B0.to_string())),
        Some(Bucket {
            start: B0,
            end: B0 + H
        })
    );
    for bad in [
        format!("{}", B0 + 1),
        format!("0{B0}"),
        format!("+{B0}"),
        "2026-10-27".into(),
        String::new(),
        "18446744073709551615".into(),
    ] {
        assert!(t.bucket_of(&bucket_subject(&bad)).is_none(), "{bad}");
        assert!(t.instantiate(&bucket_subject(&bad), B0 + H + 300).is_err());
    }
    let other = Subject::Aggregate(format!("venue.b.aaa_usd.1h:{B0}"));
    assert!(t.instantiate(&other, B0 + H + 300).is_err());
}

/// THREATS CL8-5: a bucket is observable only after its delay (late prints and corrections have
/// settled), and never while it is still open.
#[test]
fn a_bucket_is_observable_only_after_its_delay() {
    let t = hourly();
    let s = bucket_subject(&B0.to_string());
    let end = B0 + H;
    assert!(t.instantiate(&s, end + 299).is_err());
    assert!(t.instantiate(&s, end + 300).is_ok());
    assert!(t.instantiate(&s, end).is_err());
    assert!(t.instantiate(&s, B0 + 1_800).is_err());
    assert!(t.instantiate(&s, B0).is_err());
    assert!(t.instantiate(&s, end + 3_900).is_ok());
    assert!(matches!(
        t.instantiate(&s, end + 3_901),
        Err(TemplateError::DateNotObservable(_))
    ));
    // The least delay is a minute, whatever the manifest says.
    assert!(template(MIN_BUCKET_DELAY_SECS - 1, 3_600, URL)
        .validate()
        .is_err());
    template(MIN_BUCKET_DELAY_SECS, 3_600, URL)
        .validate()
        .expect("a minute");
    // The price clock and the lifecycle are the template's, never a round's.
    assert_eq!(t.decay_start(&s, 0), Some(end + 300 + 120));
    assert_eq!(t.latest_window_start(&s, 0), Some(end + 3_900));
    assert_eq!(
        t.last_observable_day(&s),
        i64::try_from((end + 3_900) / 86_400).ok()
    );
}

/// THREATS CL8-6: the observable range is short, so a coordinator that waits for a venue's
/// correction has at most six hours to pick from.
#[test]
fn the_bucketed_observable_range_is_short() {
    template(300, 300 + MAX_BUCKET_RANGE_SECS, URL)
        .validate()
        .expect("six hours");
    assert!(template(300, 301 + MAX_BUCKET_RANGE_SECS, URL)
        .validate()
        .is_err());
    assert!(template(400, 300, URL).validate().is_err(), "min > max");
    assert!(
        template(604_000, 604_801, URL).validate().is_err(),
        "past a week"
    );
    for (secs, ok) in [
        (60, false),
        (900, false),
        (3_600, true),
        (86_400, true),
        (604_800, false),
    ] {
        let mut t = hourly();
        if let SubjectRule::Bucketed { bucket_secs, .. } = &mut t.subject {
            *bucket_secs = secs;
        }
        assert_eq!(t.validate().is_ok(), ok, "{secs}");
        assert_eq!(BUCKET_SIZES.contains(&secs), ok);
    }
}

/// The URL holds one or two bucket placeholders and nothing dated; no other template holds one.
#[test]
fn bucket_placeholders_only_in_a_bucketed_url() {
    for bad in [
        "https://a.venue.example/v1/candles",
        "https://a.venue.example/v1/candles?s={bucket_start}&e={bucket_start}&x={bucket_start_ms}",
        "https://a.venue.example/v1/candles?d={date}&s={bucket_start}",
        "http://a.venue.example/v1/candles?s={bucket_start}",
    ] {
        assert!(template(300, 3_900, bad).validate().is_err(), "{bad}");
    }
    let mut sel = hourly();
    sel.selector = "/candles/{bucket_start}/close".into();
    assert!(sel.validate().is_err());
    let mut daily = hourly();
    daily.url_template = "https://a.venue.example/v1/candles?d={date}&s={bucket_start}".into();
    daily.subject = SubjectRule::Daily {
        prefix: "p".into(),
        min_days: 1,
        max_days: 5,
    };
    assert!(daily.validate().is_err(), "a daily feed with a bucket hole");
}

/// The rule is appended: every earlier variant's Borsh tag is unchanged (no live template id
/// moves), a bucketed template is one fact per bucket (`DatedFeed` on chain), and every field
/// is in the template id.
#[test]
fn the_bucketed_rule_is_appended_and_pinned() {
    let tag = |r: &SubjectRule| borsh::to_vec(r).expect("borsh")[0];
    let daily = SubjectRule::Daily {
        prefix: "p".into(),
        min_days: 1,
        max_days: 2,
    };
    assert_eq!(tag(&daily), 3);
    assert_eq!(tag(&hourly().subject), 4);
    assert!(hourly().is_dated());
    assert_eq!(MetricKind::of(&hourly()), MetricKind::DatedFeed);
    let base = hourly().id();
    assert_ne!(template(301, 3_900, URL).id(), base);
    assert_ne!(template(300, 3_901, URL).id(), base);
    let mut p = hourly();
    if let SubjectRule::Bucketed { prefix, .. } = &mut p.subject {
        *prefix = "venue.b.aaa_usd.1h".into();
    }
    assert_ne!(p.id(), base);
}

// ---- The alignment rule (ADR-0058 items 5 and 6) ----

fn run(ins: Vec<VenueInput>) -> AlignedAnswer {
    let c = composite();
    aligned(&c, B0, &inputs(&c, ins)).expect("aligned")
}

fn reasons(a: &AlignedAnswer) -> Vec<Option<Reason>> {
    a.venues.iter().map(|l| l.outcome.err()).collect()
}

/// THREATS CL8-2, CL8-9: one far venue (a manipulated thin venue, a wash print at the bucket's
/// edge) does not move the lower median of five; the band shows it and the flag is raised. It
/// stays in the answer with its own value: nothing is trimmed.
#[test]
fn one_far_venue_does_not_move_the_median_and_raises_the_flag() {
    let calm = run(rec([P, P + 50, P - 50, P - 50, P]));
    assert_eq!(calm.value, Some(P));
    assert!(!calm.disagreement);
    for far in [P * 103 / 100, P * 1_008 / 1_000, P / 2, i128::MAX / 2] {
        let a = run(rec([P, P + 50, P - 50, far, P]));
        assert_eq!(a.value, Some(P), "{far}");
        assert!(a.disagreement, "{far}");
        assert_eq!(a.band, Some((P.min(far).min(P - 50), far.max(P + 50))));
        assert_eq!(a.included().count(), 5, "shown, never trimmed");
    }
    // Two of five pushed the same way still do not move it (a third would: the stated limit).
    let two = run(rec([P, P * 103 / 100, P * 103 / 100, P - 50, P]));
    assert_eq!(two.value, Some(P));
    let three = run(rec([P, P * 103 / 100, P * 103 / 100, P * 103 / 100, P]));
    assert_eq!(
        three.value,
        Some(P * 103 / 100),
        "a majority moves the median"
    );
}

/// THREATS CL8-8: a venue whose clock disagrees with the bucket (another bucket, a skewed stamp)
/// or whose fact's clocks contradict each other is listed `clock_mismatch`, never counted.
#[test]
fn a_venue_whose_clocks_disagree_is_not_counted() {
    let cases = alignment_cases();
    for name in [
        "a venue clock 5 s fast",
        "a venue answering the previous bucket",
        "a fact observed before the bucket closed (late prints)",
        "a fact recorded before its window closed",
        "a reveal outside its window",
    ] {
        let ins = cases
            .iter()
            .find(|(n, _)| *n == name)
            .expect("case")
            .1
            .clone();
        let a = run(ins);
        assert_eq!(reasons(&a)[2], Some(Reason::ClockMismatch), "{name}");
        assert_eq!(a.included().count(), 4, "{name}");
        assert_eq!(a.value, Some(P), "{name}");
    }
}

/// Empty buckets, halted venues, ties, the threshold's edge and a zero median.
#[test]
fn edge_cases_of_the_median_band_and_flag() {
    use VenueInput::*;
    let empty = run(vec![NotObserved; 5]);
    assert_eq!(empty.status, Status::Pending);
    assert_eq!(
        (empty.value, empty.band, empty.disagreement),
        (None, None, false)
    );
    assert_eq!(empty.as_of(), B0 + H);
    let two = run(vec![
        Recorded(fact(P)),
        Recorded(fact(P * 2)),
        NotObserved,
        Unavailable,
        NoResult,
    ]);
    assert_eq!(
        two.status,
        Status::Pending,
        "two venues can still be observed"
    );
    assert_eq!(two.value, None);
    let halted = run(vec![
        Recorded(fact(P)),
        Unavailable,
        Recorded(fact(P + 20)),
        Recorded(fact(P - 20)),
        Recorded(fact(P)),
    ]);
    assert_eq!(reasons(&halted)[1], Some(Reason::Unavailable));
    assert_eq!(halted.value, Some(P));
    // Even count: the lower of the two middle values.
    let even = run(vec![
        Recorded(fact(P + 1_000)),
        Recorded(fact(P)),
        Recorded(fact(P + 1_000)),
        Recorded(fact(P)),
        RightsWithheld,
    ]);
    assert_eq!(even.value, Some(P));
    assert_eq!(lower_median(&[1, 2, 3, 4]), Some(2));
    assert_eq!(lower_median(&[]), None);
    // The flag's edge: exactly 5,000 ppm is not "exceeds"; one unit more is.
    assert_eq!(
        spread(1_000_000, 1_005_000, 1_000_000, DISAGREE_PPM),
        (5_000, false)
    );
    assert_eq!(
        spread(1_000_000, 1_005_001, 1_000_000, DISAGREE_PPM),
        (5_001, true)
    );
    assert_eq!(spread(-1, 1, 0, DISAGREE_PPM), (u64::MAX, true));
    assert_eq!(spread(0, 0, 0, DISAGREE_PPM), (0, false));
    assert_eq!(
        spread(i128::MIN, i128::MAX, 0, DISAGREE_PPM),
        (u64::MAX, true)
    );
    assert_eq!(spread(-P, -P, -P, DISAGREE_PPM), (0, false));
    // Exact where `(max - min) × 10^6` passes 128 bits: 40 ppm of a huge median is no
    // disagreement, 10,000 ppm is.
    let e = |k: u32| 10i128.pow(k);
    assert_eq!(spread(0, 4 * e(32), e(37), DISAGREE_PPM), (40, false));
    assert_eq!(spread(0, e(33), e(35), DISAGREE_PPM), (10_000, true));
    assert_eq!(spread(-e(37), e(37), e(37), 1_000_000), (2_000_000, true));
    assert_eq!(
        spread(i128::MAX - 5 * e(32), i128::MAX, i128::MAX, DISAGREE_PPM),
        (2, false)
    );
    // Ties never depend on the venues' order.
    let a = run(rec([P, P + 50, P - 150, P - 50, P]));
    let b = run(rec([P - 150, P, P, P + 50, P - 50]));
    assert_eq!((a.value, a.band), (b.value, b.band));
}

/// THREATS CL8-3: venues in another unit or scale (a stablecoin quote next to a currency quote)
/// never load together.
#[test]
fn a_composite_with_two_units_does_not_load() {
    let mut c = composite();
    c.venues[1].unit = "USDT".into();
    assert!(matches!(c.validate(), Err(AlignError::Unit(_))));
    let mut c = composite();
    c.venues[1].scale = 8;
    assert!(matches!(c.validate(), Err(AlignError::Unit(_))));
    let mut c = composite();
    c.venues[1].bucket_secs = 86_400;
    assert!(matches!(c.validate(), Err(AlignError::Unit(_))));
}

/// THREATS CL8-10: one venue counts once: the same venue (or host) under two feeds is refused,
/// as is a composite too small to need agreement.
#[test]
fn a_composite_listing_one_venue_twice_does_not_load() {
    let mut c = composite();
    c.venues[4].venue = "venue.a".into();
    assert!(c.validate().is_err());
    let mut c = composite();
    c.venues[4].host = "a.venue.example".into();
    assert!(c.validate().is_err());
    let mut c = composite();
    c.venues.swap(0, 1);
    assert!(c.validate().is_err(), "unsorted");
    let mut c = composite();
    c.min_venues = 2;
    assert!(c.validate().is_err(), "below MIN_VENUES");
    let mut c = composite();
    c.venues.truncate(2);
    assert!(c.validate().is_err(), "min_venues above the venues listed");
    let mut c = composite();
    c.disagree_ppm = 0;
    assert!(c.validate().is_err());
}

/// THREATS CL8-7, CL8-12: a restricted venue makes the whole composite restricted; a venue whose
/// terms forbid redistribution never loads.
#[test]
fn a_composite_with_a_restricted_venue_is_restricted() {
    let c = composite();
    assert_eq!(c.rights(), Redistribution::Allowed);
    let mut r = composite();
    r.venues[3].redistribution = Redistribution::Restricted;
    assert_eq!(r.rights(), Redistribution::Restricted);
    let a = aligned(&r, B0, &inputs(&r, rec([P; 5]))).expect("aligned");
    assert_eq!(a.rights, Redistribution::Restricted);
    // Even when the restricted venue's data is withheld from the answer, the answer stays
    // restricted: the strictest listed venue wins.
    let mut ins = rec([P; 5]);
    ins[3] = VenueInput::RightsWithheld;
    let a = aligned(&r, B0, &inputs(&r, ins)).expect("aligned");
    assert_eq!(a.rights, Redistribution::Restricted);
    let mut f = composite();
    f.venues[2].redistribution = Redistribution::Forbidden;
    assert!(matches!(f.validate(), Err(AlignError::Forbidden(_))));
    assert!(aligned(&f, B0, &inputs(&f, rec([P; 5]))).is_err());
}

/// THREATS CL8-4: the venue list is the composite's: an operator that leaves out a disagreeing
/// venue, adds one or reorders them is refused, and the client check refuses an answer missing a
/// listed venue or with any field moved.
#[test]
fn the_client_refuses_a_composite_missing_a_listed_venue() {
    let c = composite();
    let ins = inputs(&c, rec([P, P + 50, P * 103 / 100, P - 50, P]));
    let mut short = ins.clone();
    short.remove(2);
    assert!(matches!(
        aligned(&c, B0, &short),
        Err(AlignError::MissingVenue(_))
    ));
    let mut extra = ins.clone();
    extra.push(("venue.z.aaa_usd.1h".into(), VenueInput::Recorded(fact(P))));
    assert!(matches!(
        aligned(&c, B0, &extra),
        Err(AlignError::UnknownVenue(_))
    ));
    let mut swapped = ins.clone();
    swapped.swap(0, 1);
    assert!(aligned(&c, B0, &swapped).is_err());
    assert!(matches!(
        aligned(&c, B0 + 1, &ins),
        Err(AlignError::OffGrid(_))
    ));

    let honest = aligned(&c, B0, &ins).expect("aligned");
    check_answer(&c, &ins, &honest).expect("honest answer");
    // Drop the disagreeing venue from the answer.
    let mut dropped = honest.clone();
    dropped.venues.remove(2);
    assert!(check_answer(&c, &ins, &dropped).is_err());
    // Mark it unavailable instead.
    let mut hidden = honest.clone();
    hidden.venues[2].outcome = Err(Reason::Unavailable);
    assert!(check_answer(&c, &ins, &hidden).is_err());
    // Clear the flag, narrow the band, move the value, a clock, the rights or the bucket.
    type Edit = Box<dyn Fn(&mut AlignedAnswer)>;
    let edits: Vec<Edit> = vec![
        Box::new(|a| a.disagreement = false),
        Box::new(|a| a.band = Some((P - 50, P + 50))),
        Box::new(|a| a.value = Some(P + 1)),
        Box::new(|a| {
            if let Ok(f) = &mut a.venues[0].outcome {
                f.observed_from += 1;
            }
        }),
        Box::new(|a| {
            if let Ok(f) = &mut a.venues[1].outcome {
                f.recorded_block += 1;
            }
        }),
        Box::new(|a| a.rights = Redistribution::Restricted),
        Box::new(|a| {
            a.bucket = Bucket {
                start: B0 + H,
                end: B0 + 2 * H,
            }
        }),
        Box::new(|a| a.status = Status::Insufficient),
    ];
    for (i, e) in edits.iter().enumerate() {
        let mut a = honest.clone();
        e(&mut a);
        assert!(check_answer(&c, &ins, &a).is_err(), "edit {i}");
    }
    // Another composite's answer is refused too.
    let mut other = composite();
    other.disagree_ppm = 10_000;
    assert!(check_answer(&other, &ins, &honest).is_err());
}

/// The answer states the four clocks (ADR-0058 item 4): the bucket and `asOf` on the UTC grid,
/// each venue's own time, the observers' range, the chain's block and time; nothing is "now".
#[test]
fn every_answer_states_its_four_clocks() {
    let a = run(rec([P, P + 50, P - 150, P - 50, P]));
    let j = answer_json(&a);
    assert_eq!(j["bucket"]["start"], B0);
    assert_eq!(j["bucket"]["end"], B0 + H);
    assert_eq!(j["asOf"], B0 + H);
    for v in j["venues"].as_array().expect("venues") {
        for k in ["venueTime", "observedFrom", "observedTo"] {
            assert!(v[k].is_u64(), "{k}");
        }
        assert!(v["recordedAt"]["block"].is_u64() && v["recordedAt"]["time"].is_u64());
        assert_eq!(v["venueTime"], B0);
    }
    let text = j.to_string();
    for word in ["now", "Now", "latest"] {
        assert!(!text.contains(word), "{word}");
    }
}

/// ADR-0058 item 3 (THREATS CL8-1): a bucketed template is served by a descriptor that pins the
/// bucket's start, reads a number with tolerance 0, and asks for no date; a bucket-start assert
/// never serves a template of another rule.
#[test]
fn a_bucketed_template_needs_a_bucket_start_assert() {
    use onyx_core::feed_json::{Access, Assert, Expect, JsonDescriptor, TimeForm, ValueRule};
    let a = |p: &str, e| Assert {
        pointer: p.into(),
        expect: e,
    };
    let d = |asserts| JsonDescriptor {
        version: 1,
        pointer: "/candles/0/close".into(),
        value: ValueRule::DecimalString,
        unit: "USD".into(),
        asserts,
        max_bytes: 4_096,
        access: Access::Public,
    };
    let good = d(vec![
        a("/candles", Expect::ArrayLen(1)),
        a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
        a("/candles/0/volume", Expect::Positive),
    ]);
    good.validate().expect("valid");
    let with = |d: &JsonDescriptor| {
        let mut t = hourly();
        t.fetcher = d.fetcher_id();
        t
    };
    good.check_template(&with(&good)).expect("bucketed");
    // No bucket-start assert: the row is pinned by something else, the bucket is not.
    let no_start = d(vec![
        a("/candles", Expect::ArrayLen(1)),
        a("/candles/0/pair", Expect::Str("AAA-USD".into())),
    ]);
    assert!(no_start.check_template(&with(&no_start)).is_err());
    // A date assert with a bucketed subject.
    let dated = d(vec![
        a("/candles/0/day", Expect::SubjectDate),
        a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
    ]);
    assert!(dated.check_template(&with(&dated)).is_err());
    // A tolerance: observers of a closed bucket agree exactly.
    let mut tol = with(&good);
    tol.kind = ObservationKind::Numeric {
        scale: 2,
        tolerance: 1,
    };
    assert!(good.check_template(&tol).is_err());
    // A bucket-start assert on a daily template.
    let mut daily = with(&good);
    daily.url_template = "https://a.venue.example/v1/candles?d={date}".into();
    daily.subject = SubjectRule::Daily {
        prefix: "p".into(),
        min_days: 1,
        max_days: 5,
    };
    daily.validate().expect("a valid daily template");
    assert!(good.check_template(&daily).is_err());
    // The new asserts move the fetcher id; existing descriptors' bytes do not change (the
    // shared vectors, fixtures/json-fetcher/vectors.json, pin them).
    let ms = d(vec![
        a("/candles", Expect::ArrayLen(1)),
        a("/candles/0/t", Expect::BucketStart(TimeForm::UnixMillis)),
        a("/candles/0/volume", Expect::Positive),
    ]);
    assert_ne!(ms.fetcher_id(), good.fetcher_id());
}

/// Review CL8-R1-1: whoever pays for rounds (or the coordinator) never picks the subset of venues
/// that counts. With two of five venues manipulated and only three observed, there is no value
/// while the others can still be observed, and none once they lapsed; three included of five
/// observed venues (two honestly unavailable) is a value.
#[test]
fn a_subset_of_observed_venues_cannot_set_the_median() {
    use VenueInput::*;
    let r = |v| Recorded(fact(v));
    let paid = run(vec![r(P), r(P * 2), r(P * 2), NotObserved, NotObserved]);
    assert_eq!((paid.status, paid.value), (Status::Pending, None));
    let late = run(vec![r(P), r(P * 2), r(P * 2), Lapsed, Lapsed]);
    assert_eq!((late.status, late.value), (Status::Insufficient, None));
    assert_eq!(reasons(&late)[3], Some(Reason::Lapsed));
    let half = run(vec![r(P), r(P + 10), Unavailable, Unavailable, NoResult]);
    assert_eq!(half.value, None, "two of five");
    let most = run(vec![r(P), r(P + 10), r(P - 10), Unavailable, Unavailable]);
    assert_eq!((most.status, most.value), (Status::Aligned, Some(P)));
}

/// Review CL8-R1-3: one reviewed party counts once: two feeds cleared under the same terms row
/// (two hosts or source names of one operator) do not load together.
#[test]
fn two_hosts_of_one_terms_row_do_not_load() {
    let mut c = composite();
    c.venues[4].terms_row = c.venues[0].terms_row.clone();
    assert!(c.validate().is_err());
}

/// Review CL8-R1-4, CL8-R1-5: a bucketed descriptor pins the bucket's start and a positive volume
/// on the value's own object, so another row of a list or an empty bucket never gives the value.
#[test]
fn bucket_start_must_pin_the_value_row() {
    use onyx_core::feed_json::{Access, Assert, Expect, JsonDescriptor, TimeForm, ValueRule};
    let a = |p: &str, e| Assert {
        pointer: p.into(),
        expect: e,
    };
    let d = |pointer: &str, asserts| JsonDescriptor {
        version: 1,
        pointer: pointer.into(),
        value: ValueRule::DecimalString,
        unit: "USD".into(),
        asserts,
        max_bytes: 4_096,
        access: Access::Public,
    };
    let with = |d: &JsonDescriptor| {
        let mut t = hourly();
        t.fetcher = d.fetcher_id();
        t.selector = d.pointer.clone();
        t
    };
    // The value of row 1 with the start of row 0.
    let other_row = d(
        "/candles/1/close",
        vec![
            a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
            a("/candles/1/pair", Expect::Str("AAA-USD".into())),
            a("/candles/1/volume", Expect::Positive),
        ],
    );
    other_row.validate().expect("a valid descriptor");
    assert!(other_row.check_template(&with(&other_row)).is_err());
    // No volume assert.
    let no_volume = d(
        "/candles/0/close",
        vec![
            a("/candles", Expect::ArrayLen(1)),
            a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
        ],
    );
    assert!(no_volume.check_template(&with(&no_volume)).is_err());
    // A volume of another object.
    let elsewhere = d(
        "/candles/0/close",
        vec![
            a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
            a("/totals/volume", Expect::Positive),
        ],
    );
    assert!(elsewhere.check_template(&with(&elsewhere)).is_err());
    let good = d(
        "/candles/0/close",
        vec![
            a("/candles/0/t", Expect::BucketStart(TimeForm::UnixSecs)),
            a("/candles/0/volume", Expect::Positive),
        ],
    );
    good.check_template(&with(&good)).expect("same row");
}
