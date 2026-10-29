//! The example track record (ROADMAP C6): invented sources and values on the
//! example level-3 metric `example.signed.reserve`, which has a reviewed
//! close-time rule (docs/spec/proof-ladder.md "Close-time rules"; the Bitcoin
//! flow metrics have the only production rule, C9, and no record yet).
//!
//! No production metric produces a track record yet, so the website shows this
//! example, labelled "example" everywhere, beside a plain empty state. Every id
//! here starts with `example`: a server refuses to publish such ids unless it is
//! built for tests (`onyx_api::track_serve`), so this data is never served as real.
//!
//! Two stating sources forecast the reserve of six invented entities; the
//! example level-3 signer (`example.signed`) later publishes the checked value.
//! `example.forecast` has an established record (36 statements: 31 agreed, 3
//! disagreed, 1 late, 1 unresolved); `example.guess` has too few resolved
//! statements to show a rate (one of them equivocated). Two commitments are
//! built, the second chained to and extending the first, so clients can be
//! tested against a stale commitment served as the latest (TR-R2-5).

use crate::{
    compute, sort_resolutions, sort_statements, Close, Fact, MetricRule, Resolution, Statement,
    TrackBook, TrackCommitment, TrackError, TrackParams,
};
use onyx_core::{
    keccak256, Confidence, DataRights, EntityKind, Hash32, MetricId, MetricInfo, ProofLevel,
    Redistribution, SourceId, StaticRegistry, Subject, Value, ValueKind,
};

/// The example level-3 metric.
pub const METRIC: &str = "example.signed.reserve";
/// The example level-3 signer: its signed reports resolve the facts.
pub const SIGNER: &str = "example.signed";
/// A source with an established example record.
pub const FORECASTER: &str = "example.forecast";
/// A source with a record below the threshold (counts only, no rate).
pub const GUESSER: &str = "example.guess";
/// The invented entities the example states about.
pub const ENTITIES: [&str; 6] = [
    "entity:example-a",
    "entity:example-b",
    "entity:example-c",
    "entity:example-d",
    "entity:example-e",
    "entity:example-f",
];

const DAY: u64 = 86_400;
/// 2026-09-01 00:00 UTC: the first fact's `as_of`.
pub const T0: u64 = 1_788_220_800;
/// Facts, one a day.
pub const DAYS: u64 = 36;
/// The earlier commitment's `as_of` (day 20).
pub const AS_OF_EARLY: u64 = T0 + 20 * DAY;
/// The latest commitment's `as_of` (day 36).
pub const AS_OF: u64 = T0 + DAYS * DAY;

/// True for an id the example uses (`example` or `example.*`, `entity:example-*`).
pub fn is_example_id(id: &str) -> bool {
    id == "example" || id.starts_with("example.") || id.starts_with("entity:example")
}

fn rights(source: &str) -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(source.into()),
        attribution: None,
        terms: String::new(),
    }
}

/// The example registry: the metric, the six entities and the three sources.
pub fn registry() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let mut add = || -> Result<(), onyx_core::RegistryError> {
        r.add_metric(
            METRIC,
            MetricInfo {
                value_kind: ValueKind::UInt,
                needs_label_set: false,
            },
        )?;
        for e in ENTITIES {
            r.add_entity(e, EntityKind::Exchange)?;
        }
        for s in [SIGNER, FORECASTER, GUESSER] {
            r.add_source(rights(s))?;
        }
        Ok(())
    };
    // Fixed, valid ids: the registry accepts them (tested).
    let _ = add();
    r
}

/// The example params: the reviewed rule of the example metric (close at
/// `as_of`, exact agreement), default resolver levels, no intake.
pub fn params() -> TrackParams {
    TrackParams::new(vec![MetricRule {
        metric: MetricId(METRIC.into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }])
}

fn fact(day: u64) -> Fact {
    Fact {
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity(ENTITIES[(day % 6) as usize].into()),
        as_of: T0 + day * DAY,
    }
}

/// The signed value of day `day` (what the signer publishes later).
fn truth(day: u64) -> u128 {
    1_000_000 + 1_000 * u128::from(day % 6) + u128::from(day)
}

fn statement(source: &str, day: u64, value: u128, p: u32, committed_at: u64) -> Statement {
    Statement {
        source: SourceId(source.into()),
        fact: fact(day),
        value: Value::UInt(value),
        confidence: Confidence(p),
        committed_at,
    }
}

/// Every statement and resolution of the example, before any `as_of` filter.
fn all() -> (Vec<Statement>, Vec<Resolution>) {
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in 0..DAYS {
        let f = fact(d);
        let t = truth(d);
        // The forecaster states an hour before the fact; day 7 an hour after
        // (late); days 3, 12 and 25 miss by 5,000.
        let (at, v) = match d {
            7 => (f.as_of + 3_600, t),
            3 | 12 | 25 => (f.as_of - 3_600, t + 5_000),
            _ => (f.as_of - 3_600, t),
        };
        s.push(statement(FORECASTER, d, v, 800_000, at));
        // The guesser states days 20-29 ten minutes before; days 21, 23, 27
        // miss, and day 28 it states twice with different values (equivocated).
        if (20..30).contains(&d) {
            let v = if matches!(d, 21 | 23 | 27) {
                t + 9_000
            } else {
                t
            };
            s.push(statement(GUESSER, d, v, 950_000, f.as_of - 600));
            if d == 28 {
                s.push(statement(GUESSER, d, t + 1, 950_000, f.as_of - 300));
            }
        }
        // The signer publishes ten minutes after the fact, except the last
        // day (not published yet: unresolved).
        if d + 1 < DAYS {
            let public_from = f.as_of + 600;
            r.push(Resolution {
                fact: f.clone(),
                value: Value::UInt(t),
                level: ProofLevel::SourceProven,
                resolver: SourceId(SIGNER.into()),
                evidence: keccak256(
                    &[b"onyx-track-example-report".as_slice(), &d.to_be_bytes()].concat(),
                ),
                public_from,
            });
        }
    }
    (s, r)
}

/// The statement log and resolution list as known at `as_of`, in canonical
/// order: statements committed and resolutions public at or before it.
pub fn logs(as_of: u64) -> (Vec<Statement>, Vec<Resolution>) {
    let (s, r) = all();
    let mut s: Vec<Statement> = s.into_iter().filter(|x| x.committed_at <= as_of).collect();
    let mut r: Vec<Resolution> = r.into_iter().filter(|x| x.public_from <= as_of).collect();
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    (s, r)
}

/// The example logs at `as_of` plus one level-1 answer on the last day's fact
/// (from the signer, public at `as_of`): logs a server does not serve free,
/// since a level-1 value is sold by the paid API (ADR-0036 amendment 5 item 2;
/// tests of `onyx_api::track_serve`).
pub fn logs_with_a_level1_answer(as_of: u64) -> (Vec<Statement>, Vec<Resolution>) {
    let (s, mut r) = logs(as_of);
    if let Some(f) = s.iter().map(|x| x.fact.clone()).max_by_key(|f| f.as_of) {
        r.push(Resolution {
            fact: f,
            value: Value::UInt(1),
            level: ProofLevel::Recomputed,
            resolver: SourceId(SIGNER.into()),
            evidence: keccak256(b"onyx-track-example-level1"),
            public_from: as_of,
        });
    }
    crate::sort_resolutions(&mut r);
    (s, r)
}

/// The book at `as_of`.
pub fn book(as_of: u64) -> Result<TrackBook, TrackError> {
    let (s, r) = logs(as_of);
    compute(&params(), &registry(), &s, &r, as_of)
}

/// The two example commitments: at [`AS_OF_EARLY`] (the first, `prev` zero)
/// and at [`AS_OF`] (chained to it), with their books.
pub fn chain() -> Result<[(TrackCommitment, TrackBook); 2], TrackError> {
    let early = book(AS_OF_EARLY)?;
    let c1 = early.commitment(Hash32::ZERO);
    let latest = book(AS_OF)?;
    let c2 = latest.commitment(c1.hash());
    Ok([(c1, early), (c2, latest)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{check_extends, verify_history, Outcome, TrackLog};
    use onyx_core::Registry;

    fn rec<'a>(b: &'a TrackBook, source: &str) -> &'a crate::Record {
        b.record(&SourceId(source.into()), &MetricId(METRIC.into()))
            .expect("record")
            .1
    }

    /// The example is what its docs say: one established record, one below
    /// the threshold, every outcome the website explains present.
    #[test]
    fn the_example_has_the_documented_records() {
        let reg = registry();
        assert!(params().validate(&reg).is_ok());
        assert!(reg.metric(METRIC).is_some());
        let [(_, early), (_, latest)] = chain().expect("chain");
        let f = rec(&latest, FORECASTER);
        assert_eq!(
            (f.agreed, f.disagreed, f.late, f.unresolved, f.resolved()),
            (31, 3, 1, 1, 34)
        );
        assert!(f.established());
        assert_eq!(f.agreed_first, 31);
        let g = rec(&latest, GUESSER);
        assert_eq!((g.agreed, g.disagreed, g.equivocated), (6, 3, 1));
        assert!(!g.established());
        assert_eq!(g.agreed_first, 1, "day 25: the forecaster missed it");
        assert_eq!((f.metric_sources, f.metric_resolved_facts), (2, 35));
        assert!(latest
            .entries()
            .iter()
            .any(|e| e.outcome == Outcome::Equivocated));
        // The early book has fewer statements: a stale commitment shows less.
        assert!(rec(&early, FORECASTER).entries_count < f.entries_count);
        assert!(early
            .record(&SourceId(GUESSER.into()), &MetricId(METRIC.into()))
            .is_some());
    }

    /// The latest commitment extends the earlier one, and each record's history checks.
    #[test]
    fn the_example_chain_extends_and_every_history_checks() {
        let [(c1, early), (c2, latest)] = chain().expect("chain");
        let (s1, r1) = logs(AS_OF_EARLY);
        let (s2, r2) = logs(AS_OF);
        let p = params();
        let log = |statements, resolutions| TrackLog {
            params: &p,
            statements,
            resolutions,
        };
        assert_eq!(
            check_extends(&c1, log(&s1, &r1), &c2, log(&s2, &r2)),
            Ok(())
        );
        for (c, b) in [(&c1, &early), (&c2, &latest)] {
            for (i, r) in b.records().iter().enumerate() {
                let rp = b.record_proof(i as u64).expect("proof");
                let (mi, m) = b.metric(&r.metric).expect("summary");
                let mp = b.metric_proof(mi).expect("proof");
                let proofs: Vec<_> = (r.entries_from..r.entries_from + r.entries_count)
                    .map(|k| b.entry_proof(k).expect("proof"))
                    .collect();
                let es: Vec<_> = (0..proofs.len())
                    .map(|k| (&b.entries()[r.entries_from as usize + k], &proofs[k]))
                    .collect();
                let nb = |k: Option<u64>| k.map(|k| (k, b.entry_proof(k).expect("proof")));
                let before = nb(r.entries_from.checked_sub(1));
                let end = r.entries_from + r.entries_count;
                let after = nb((end < c.entry_count).then_some(end));
                let pb = before.as_ref().map(|(k, p)| (&b.entries()[*k as usize], p));
                let pa = after.as_ref().map(|(k, p)| (&b.entries()[*k as usize], p));
                assert_eq!(verify_history(c, (r, &rp), (m, &mp), &es, pb, pa), Ok(()));
            }
        }
    }

    #[test]
    fn every_example_id_is_marked_example() {
        for id in [METRIC, SIGNER, FORECASTER, GUESSER] {
            assert!(is_example_id(id), "{id}");
        }
        for e in ENTITIES {
            assert!(is_example_id(e), "{e}");
        }
        for id in [
            "examples.x",
            "crypto.btc.entity_flow",
            "entity:okx",
            "us.example.x",
        ] {
            assert!(!is_example_id(id), "{id}");
        }
    }
}
