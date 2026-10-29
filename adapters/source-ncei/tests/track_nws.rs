//! The NWS forecaster through the track-record intake (north-star loop NL4,
//! ADR-0036 amendment 8): the recorded forecast becomes signed statements on
//! the station's coming days, sealed batches post to a record, statements stay
//! sealed until their day ends, and final level-2 rounds score them. A day
//! whose close has passed is refused, an open round gives no score, and a
//! declared common operator would turn every entry self-resolved (the testnet
//! exception of THREATS TR-R2-4 is that none is declared).
#![allow(clippy::unwrap_used)]

use onyx_core::{
    keccak256, Confidence, DataRights, KeyProvenance, MetricId, MetricInfo, SourceId, SourceKey,
    StaticRegistry, Value, ValueKind,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use onyx_source_ncei::nws::{self, FORECAST_SOURCE};
use onyx_source_ncei::{rights, subject, METRIC, SOURCE, SUBJECT};
use onyx_track::daily::{daily_fact, daily_rule, resolution_of, Recorded, DAY};
use onyx_track::intake::{
    key_id, Anchor, FinalizedRecord, Intake, IntakeError, IntakeId, KeyBook, Post, SignedStatement,
    StatementPayload, TrackKey,
};
use onyx_track::publication::Publication;
use onyx_track::wire::{check_record_body, record_body, Expect, Served};
use onyx_track::{compute, sort_resolutions, OperatorOf, Outcome, TrackParams};

/// 2026-10-28 06:00 UTC: when the recorded forecast is stated.
const T0: u64 = 1_793_167_200;

fn relay_secret() -> [u8; 32] {
    keccak256(b"onyx-track-nws-test-relay").0
}

fn relay() -> [u8; 20] {
    secret_address(&relay_secret()).unwrap()
}

fn days() -> Vec<String> {
    nws::parse_max_temperature(nws::RECORDED)
        .unwrap()
        .highs
        .into_iter()
        .map(|h| h.day)
        .collect()
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: 1 },
            needs_label_set: false,
        },
    )
    .unwrap();
    for d in days() {
        r.add_aggregate(&subject(&d)).unwrap();
    }
    r.add_source(DataRights {
        source: SourceId(FORECAST_SOURCE.into()),
        ..rights()
    })
    .unwrap();
    r.add_source(rights()).unwrap();
    r
}

fn reviewed() -> Vec<SourceKey> {
    vec![SourceKey {
        source: SourceId(FORECAST_SOURCE.into()),
        signer: relay(),
        provenance: KeyProvenance::WellKnown {
            origin: "onyx.cryptagraph.com".into(),
            document_hash: keccak256(b"relay review record"),
            fetched_unix: T0,
        },
    }]
}

const ID: IntakeId = IntakeId {
    chain_id: 84_532,
    sink: [0xc5; 20],
    poster: [0x70; 20],
    first_nonce: 0,
};

fn params() -> TrackParams {
    TrackParams::new(vec![daily_rule(MetricId(METRIC.into()), 5)])
        .with_level2_resolvers_only()
        .with_intake(ID.id())
}

fn sign(day: &str, tenths: i128) -> SignedStatement {
    let source = SourceId(FORECAST_SOURCE.into());
    let payload = StatementPayload {
        intake: ID.id(),
        key: key_id(&source, &relay()),
        source,
        fact: daily_fact(MetricId(METRIC.into()), SUBJECT, &subject(day)).unwrap(),
        value: Value::Fixed {
            mantissa: tenths,
            scale: 1,
        },
        confidence: Confidence(500_000),
    };
    let signature = eip191_sign_with(&relay_secret(), payload.message().as_bytes()).unwrap();
    SignedStatement { payload, signature }
}

/// A record in memory: posts land in blocks, the head moves on demand.
struct Mem {
    head: Anchor,
    posts: Vec<Post>,
}

impl FinalizedRecord for Mem {
    fn head(&self) -> Result<Anchor, String> {
        Ok(self.head)
    }
    fn posts(&self, head: &Anchor) -> Result<Vec<Post>, String> {
        Ok(self
            .posts
            .iter()
            .filter(|p| p.anchor.block <= head.block)
            .copied()
            .collect())
    }
}

fn anchor(block: u64, timestamp: u64) -> Anchor {
    Anchor {
        block,
        block_hash: keccak256(&block.to_be_bytes()),
        timestamp,
    }
}

/// The relay's run at [`T0`]: every recorded day whose close is still ahead,
/// sealed and posted one batch per day a minute later. Returns the intake.
fn stated() -> (Intake, Mem, Vec<String>) {
    let (r, p) = (reg(), params());
    let book = KeyBook {
        keys: vec![TrackKey {
            source: SourceId(FORECAST_SOURCE.into()),
            signer: relay(),
            revoked: false,
        }],
    };
    let mut i = Intake::new(ID.id(), book, &r, &reviewed()).unwrap();
    let mut on_time = Vec::new();
    for h in nws::parse_max_temperature(nws::RECORDED).unwrap().highs {
        match i.submit(&p, &r, sign(&h.day, h.tenths), T0) {
            Ok(_) => on_time.push(h.day),
            // The 27th and the 28th closed before 06:00 UTC on the 28th.
            Err(IntakeError::PastClose) => assert!(h.day.as_str() <= "2026-10-28", "{}", h.day),
            Err(e) => panic!("{}: {e}", h.day),
        }
    }
    let mut rec = Mem {
        head: anchor(10, T0),
        posts: Vec::new(),
    };
    let mut seq = 0;
    while let Some(h) = i.seal() {
        rec.posts.push(Post {
            seq,
            header_hash: h.hash(),
            anchor: anchor(11 + seq, T0 + 60 + seq),
        });
        seq += 1;
    }
    (i, rec, on_time)
}

#[test]
fn coming_days_are_stated_and_closed_days_refused() {
    let (_, rec, on_time) = stated();
    assert_eq!(
        on_time,
        [
            "2026-10-29",
            "2026-10-30",
            "2026-10-31",
            "2026-11-01",
            "2026-11-02",
            "2026-11-03"
        ]
    );
    assert_eq!(rec.posts.len(), 6, "one batch per day");
}

#[test]
fn statements_stay_sealed_until_their_day_ends_then_score_from_final_rounds() {
    let (r, p) = (reg(), params());
    let (mut i, mut rec, _) = stated();
    // Before the 29th ends nothing is revealed.
    rec.head = anchor(20, T0 + DAY);
    let head = i.sync(&rec).unwrap();
    assert!(i.log_at(&head).is_empty());
    // 2026-10-31 01:00 UTC: the 29th and the 30th have ended.
    let end29 = daily_fact(MetricId(METRIC.into()), SUBJECT, &subject("2026-10-29"))
        .unwrap()
        .as_of;
    rec.head = anchor(30, end29 + DAY + 3_600);
    let head = i.sync(&rec).unwrap();
    let log = i.log_at(&head);
    assert_eq!(log.len(), 2);
    assert!(log
        .iter()
        .all(|s| s.committed_at > T0 && s.committed_at <= T0 + 120));

    // NCEI rounds 40 days later: the 29th observed 17.6 °C (forecast 17.8: within 0.5),
    // the 30th 18.0 °C (forecast 16.1: off); a third round still open gives nothing.
    let resolution = |day: &str, tenths: i128, rec: Option<Recorded>| {
        let fact = daily_fact(MetricId(METRIC.into()), SUBJECT, &subject(day)).unwrap();
        resolution_of(
            fact.clone(),
            Value::Fixed {
                mantissa: tenths,
                scale: 1,
            },
            keccak256(day.as_bytes()),
            rec,
            SourceId(SOURCE.into()),
        )
    };
    let late = end29 + 40 * DAY;
    let mut res = vec![
        resolution("2026-10-29", 176, Some(Recorded::level2(late))).unwrap(),
        resolution("2026-10-30", 180, Some(Recorded::level2(late + DAY))).unwrap(),
    ];
    assert!(
        resolution("2026-10-31", 160, None).is_err(),
        "an open round gives no score"
    );
    sort_resolutions(&mut res);
    // The commitment is computed at the record's final head (its time is the as-of).
    let now = late + 2 * DAY;
    rec.head = anchor(40, now);
    let head = i.sync(&rec).unwrap();
    let log = i.log_at(&head);
    assert_eq!(log.len(), 6, "every stated day has ended");
    let book = compute(&p, &r, &log, &res, now).unwrap();
    let mut publication = Publication::chained();
    let c = publication
        .push_with(&p, &log, &res, &book, Some(i.published(&head)), Some(&r))
        .unwrap();
    let served = Served {
        book: &book,
        commitment: &c,
        params: &p,
        chain: None,
    };
    let body = record_body(
        &served,
        &SourceId(FORECAST_SOURCE.into()),
        &MetricId(METRIC.into()),
    )
    .expect("record");
    // The site's e2e serves this body as the node would (apps/web/tests/ns16.mjs): bless with
    // ONYX_BLESS=1 after a deliberate change.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/web/tests/fixtures/track-nws-record.json"
    );
    let text = format!("{body:#}\n");
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(fixture, &text).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(fixture).unwrap(),
        text,
        "bless with ONYX_BLESS=1"
    );
    let checked = check_record_body(
        &body,
        &Expect {
            source: FORECAST_SOURCE,
            metric: METRIC,
            min_as_of: 0,
            known: None,
            latest: Some(c.hash()),
        },
    )
    .unwrap();
    let rec5 = &checked.record;
    assert_eq!(
        (
            rec5.agreed,
            rec5.disagreed,
            rec5.unresolved,
            rec5.self_resolved
        ),
        (1, 1, 4, 0)
    );
    // Two scored days: too few to score (no rate, no Brier) until 30.
    assert!(!rec5.established());

    // Had both NOAA offices been declared one operator, every entry would be
    // self-resolved: the exception this testnet record runs under.
    let op = keccak256(b"noaa");
    let declared = p.clone().with_operators(vec![
        OperatorOf {
            source: SourceId(FORECAST_SOURCE.into()),
            operator: op,
        },
        OperatorOf {
            source: SourceId(SOURCE.into()),
            operator: op,
        },
    ]);
    let book2 = compute(&declared, &r, &log, &res, now).unwrap();
    let outcomes: Vec<Outcome> = book2.entries().iter().map(|e| e.outcome).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == Outcome::SelfResolved)
            .count(),
        2,
        "both resolved days self-resolved: {outcomes:?}"
    );
    assert!(!outcomes
        .iter()
        .any(|o| matches!(o, Outcome::Agreed | Outcome::Disagreed)));
}
