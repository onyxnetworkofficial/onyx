//! Protocol rules of ADR-0036, one test per rule, plus the adversarial cases
//! in THREATS.md (TR-*, TR-R1-*).

use super::*;
use onyx_core::{
    DataRights, MarketPolicy, MetricInfo, StaticRegistry, Subject, ValueKind, VerificationMethod,
};

const DAY: u64 = 86_400;
const T0: u64 = 1_790_000_000;
const NOW: u64 = T0 + 400 * DAY;
/// The source of the level-1 and level-3 answers in these tests.
const RESOLVER: &str = "chain.test";

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let m = |value_kind| MetricInfo {
        value_kind,
        needs_label_set: false,
    };
    r.add_metric("test.amount", m(ValueKind::UInt))
        .and_then(|r| r.add_metric("test.price", m(ValueKind::Fixed { scale: 2 })))
        .and_then(|r| r.add_metric("test.flag", m(ValueKind::Bool)))
        .and_then(|r| r.add_metric("test.unruled", m(ValueKind::UInt)))
        .expect("metrics");
    for a in ["a.one", "a.two", "a.three", "a.four", "a.five", "a.six"] {
        r.add_aggregate(a).expect("aggregate");
    }
    for s in [
        "vendor.a", "vendor.b", "vendor.c", "vendor.d", "vendor.e", "vendor.f", "vendor.g",
        "vendor.h", RESOLVER,
    ] {
        r.add_source(DataRights::public_chain(SourceId(s.into())))
            .expect("source");
    }
    r
}

fn rule(m: &str, close_after_secs: u64, tolerance: u128) -> MetricRule {
    MetricRule {
        metric: MetricId(m.into()),
        close: Close::AfterAsOf(close_after_secs),
        tolerance,
    }
}

fn params() -> TrackParams {
    TrackParams::new(vec![
        rule("test.amount", DAY, 0),
        rule("test.flag", 0, 0),
        rule("test.price", 0, 50),
    ])
}

fn fact(metric: &str, subject: &str, day: u64) -> Fact {
    Fact {
        metric: MetricId(metric.into()),
        subject: Subject::Aggregate(subject.into()),
        as_of: T0 + day * DAY,
    }
}

fn st(source: &str, f: &Fact, value: Value, p: u32, at: u64) -> Statement {
    Statement {
        source: SourceId(source.into()),
        fact: f.clone(),
        value,
        confidence: Confidence(p),
        committed_at: at,
    }
}

fn res_by(by: &str, f: &Fact, value: Value, level: ProofLevel, public_from: u64) -> Resolution {
    Resolution {
        fact: f.clone(),
        value,
        level,
        resolver: SourceId(by.into()),
        evidence: keccak256(&borsh::to_vec(&(f, by, public_from)).expect("borsh")),
        public_from,
    }
}

fn res(f: &Fact, value: Value, level: ProofLevel, public_from: u64) -> Resolution {
    res_by(RESOLVER, f, value, level, public_from)
}

fn run(mut s: Vec<Statement>, mut r: Vec<Resolution>) -> TrackBook {
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    compute(&params(), &reg(), &s, &r, NOW).expect("compute")
}

fn rec<'a>(b: &'a TrackBook, source: &str, metric: &str) -> &'a Record {
    b.record(&SourceId(source.into()), &MetricId(metric.into()))
        .expect("record")
        .1
}

/// A source with `n` statements on `test.amount`, each resolved at level 1 a
/// day after its close; `wrong` of them disagree. Subjects cycle over `subjects`.
fn history(
    source: &str,
    n: u64,
    wrong: u64,
    subjects: &[&str],
) -> (Vec<Statement>, Vec<Resolution>) {
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in 0..n {
        let f = fact("test.amount", subjects[(d as usize) % subjects.len()], d);
        let stated = if d < wrong { 999 } else { 100 + u128::from(d) };
        s.push(st(source, &f, Value::UInt(stated), 900_000, f.as_of));
        r.push(res(
            &f,
            Value::UInt(100 + u128::from(d)),
            ProofLevel::Recomputed,
            f.as_of + 2 * DAY,
        ));
    }
    (s, r)
}

const SUBJECTS: [&str; 6] = ["a.one", "a.two", "a.three", "a.four", "a.five", "a.six"];

#[test]
fn agreement_with_a_later_stronger_answer_counts() {
    let (s, r) = history("vendor.a", 10, 3, &SUBJECTS);
    let b = run(s, r);
    let r = rec(&b, "vendor.a", "test.amount");
    assert_eq!((r.agreed, r.disagreed, r.resolved()), (7, 3, 10));
    assert_eq!(r.agreement_ppm(), Some(700_000));
    // p = 0.9: agreed (0.1)^2 = 1e10, disagreed (0.9)^2 = 81e10 (units 1e-12).
    assert_eq!(r.brier_sum_e12, 7 * 10_000_000_000 + 3 * 810_000_000_000);
    assert_eq!(b.entries().len(), 10);
    assert!(b.entries().iter().all(|e| e.resolution_level == 1));
}

/// TR-1: a record compares a source with the truth, never with its peers. A
/// majority of other sources stating a wrong value changes nothing.
#[test]
fn peers_never_change_an_outcome() {
    let (s, r) = history("vendor.a", 6, 0, &SUBJECTS);
    let alone = run(s.clone(), r.clone());
    let mut crowd = s.clone();
    for peer in ["vendor.b", "vendor.c", "vendor.d", "vendor.e"] {
        for x in &s {
            crowd.push(st(
                peer,
                &x.fact,
                Value::UInt(1),
                1_000_000,
                x.committed_at - 10,
            ));
        }
    }
    let with_crowd = run(crowd, r);
    let a = rec(&alone, "vendor.a", "test.amount");
    let b = rec(&with_crowd, "vendor.a", "test.amount");
    assert_eq!(
        (a.agreed, a.disagreed, a.brier_sum_e12),
        (b.agreed, b.disagreed, b.brier_sum_e12)
    );
    assert_eq!(
        b.agreed_first, b.agreed,
        "the wrong crowd was not first to agree"
    );
    assert_eq!(rec(&with_crowd, "vendor.b", "test.amount").agreed, 0);
}

/// TR-2: a new source starts with nothing; a rate is shown only past the
/// threshold (30 resolved over 5 subjects).
#[test]
fn a_new_source_starts_with_nothing() {
    let (s, r) = history("vendor.a", 29, 0, &SUBJECTS);
    let b = run(s, r.clone());
    assert!(b
        .record(
            &SourceId("vendor.b".into()),
            &MetricId("test.amount".into())
        )
        .is_none());
    assert!(
        !rec(&b, "vendor.a", "test.amount").established(),
        "29 resolved"
    );
    let (s30, r30) = history("vendor.a", 30, 0, &SUBJECTS[..4]);
    let b = run(s30, r30);
    let r = rec(&b, "vendor.a", "test.amount");
    assert_eq!((r.resolved(), r.distinct_subjects), (30, 4));
    assert!(!r.established(), "4 subjects");
    let (s30, r30) = history("vendor.a", 30, 0, &SUBJECTS[..5]);
    assert!(rec(&run(s30, r30), "vendor.a", "test.amount").established());
}

#[test]
fn late_statements_do_not_count() {
    let f = fact("test.amount", "a.one", 0);
    let close = f.as_of + DAY;
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 1_000_000, close + 1),
            st("vendor.b", &f, Value::UInt(5), 1_000_000, close),
        ],
        vec![res(&f, Value::UInt(5), ProofLevel::Recomputed, close + 10)],
    );
    let a = rec(&b, "vendor.a", "test.amount");
    assert_eq!((a.late, a.resolved(), a.brier_sum_e12), (1, 0, 0));
    assert_eq!(
        rec(&b, "vendor.b", "test.amount").agreed,
        1,
        "at the close is on time"
    );
}

/// C66 (ADR-0036 amendment 7): the daily close rule of a level-2 day closes an hour before
/// the UTC day starts; a statement after that is late and never counts.
#[test]
fn a_statement_after_the_daily_close_is_late() {
    let lead = 3_600;
    let params = TrackParams::new(vec![MetricRule::period_start(
        MetricId("test.amount".into()),
        DAY,
        lead,
        0,
    )]);
    let day_end = (T0 / DAY + 10) * DAY;
    let f = Fact {
        metric: MetricId("test.amount".into()),
        subject: Subject::Aggregate("a.one".into()),
        as_of: day_end,
    };
    let close = day_end - DAY - lead;
    let mut s = vec![
        st("vendor.a", &f, Value::UInt(5), 1_000_000, close + 1),
        st("vendor.b", &f, Value::UInt(5), 1_000_000, close),
    ];
    let mut r = vec![res(
        &f,
        Value::UInt(5),
        ProofLevel::Recomputed,
        day_end + DAY,
    )];
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    let b = compute(&params, &reg(), &s, &r, NOW).expect("compute");
    let a = rec(&b, "vendor.a", "test.amount");
    assert_eq!((a.late, a.agreed), (1, 0));
    let v = rec(&b, "vendor.b", "test.amount");
    assert_eq!((v.late, v.agreed), (0, 1), "at the close is on time");
}

/// TR-3: hedging never helps. Two different statements on one resolved fact
/// are a confident miss, even if one of them is right; a later different
/// statement counts too.
#[test]
fn equivocation_is_a_confident_miss() {
    let f = fact("test.amount", "a.one", 0);
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 600_000, f.as_of),
            st("vendor.a", &f, Value::UInt(6), 600_000, f.as_of + 5),
            st("vendor.b", &f, Value::UInt(5), 600_000, f.as_of),
            st("vendor.b", &f, Value::UInt(5), 900_000, f.as_of + 3 * DAY),
        ],
        vec![res(
            &f,
            Value::UInt(5),
            ProofLevel::Recomputed,
            f.as_of + 2 * DAY,
        )],
    );
    for s in ["vendor.a", "vendor.b"] {
        let r = rec(&b, s, "test.amount");
        assert_eq!(
            (r.equivocated, r.agreed, r.brier_sum_e12),
            (1, 0, u128::from(BRIER_ONE)),
            "{s}"
        );
    }
    assert_eq!(b.entries()[0].statements, 2);
}

/// TR-R1-10: an equivocation counts only once its fact resolves cleanly, so
/// unresolved hedges cannot inflate `resolved` or the subject count.
#[test]
fn an_equivocation_on_an_unresolved_fact_does_not_count_yet() {
    let f = fact("test.amount", "a.one", 0);
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 600_000, f.as_of),
            st("vendor.a", &f, Value::UInt(6), 600_000, f.as_of + 5),
        ],
        vec![],
    );
    let r = rec(&b, "vendor.a", "test.amount");
    assert_eq!(
        (
            r.unresolved,
            r.equivocated,
            r.resolved(),
            r.distinct_subjects
        ),
        (1, 0, 0, 0)
    );
}

#[test]
fn an_identical_restatement_is_one_statement() {
    let f = fact("test.amount", "a.one", 0);
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 700_000, f.as_of + 9),
            st("vendor.a", &f, Value::UInt(5), 700_000, f.as_of + 2),
        ],
        vec![res(
            &f,
            Value::UInt(5),
            ProofLevel::Recomputed,
            f.as_of + 2 * DAY,
        )],
    );
    let e = &b.entries()[0];
    assert_eq!(
        (e.outcome, e.committed_at, e.statements),
        (Outcome::Agreed, f.as_of + 2, 2)
    );
}

/// The truth was public before statements closed: nobody is scored.
#[test]
fn void_when_the_truth_was_public_before_the_close() {
    let f = fact("test.amount", "a.one", 0);
    let close = f.as_of + DAY;
    let b = run(
        vec![st("vendor.a", &f, Value::UInt(5), 1_000_000, f.as_of)],
        vec![res(&f, Value::UInt(5), ProofLevel::Recomputed, close)],
    );
    let r = rec(&b, "vendor.a", "test.amount");
    assert_eq!((r.void, r.resolved()), (1, 0));
}

#[test]
fn contested_resolutions_score_nobody_and_agreeing_ones_use_the_strongest() {
    let f = fact("test.amount", "a.one", 0);
    let g = fact("test.amount", "a.two", 0);
    let later = f.as_of + 3 * DAY;
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 1_000_000, f.as_of),
            st("vendor.a", &g, Value::UInt(7), 1_000_000, g.as_of),
        ],
        vec![
            res(&f, Value::UInt(5), ProofLevel::SourceProven, later),
            res_by(
                "vendor.h",
                &f,
                Value::UInt(6),
                ProofLevel::SourceProven,
                later,
            ),
            res(&g, Value::UInt(7), ProofLevel::SourceProven, later),
            res(&g, Value::UInt(7), ProofLevel::Recomputed, later + 1),
        ],
    );
    let r = rec(&b, "vendor.a", "test.amount");
    assert_eq!((r.contested, r.agreed, r.metric_resolved_facts), (1, 1, 1));
    let e = b.entries().iter().find(|e| e.fact == g).expect("g");
    assert_eq!(e.resolution_level, 1);
}

/// TR-R1-7: a weaker answer never contests a stronger one, and a source's own
/// signed answer never judges its statement.
#[test]
fn weaker_or_own_answers_never_judge() {
    let f = fact("test.amount", "a.one", 0);
    let g = fact("test.amount", "a.two", 0);
    let later = f.as_of + 3 * DAY;
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 1_000_000, f.as_of),
            st("vendor.a", &g, Value::UInt(9), 1_000_000, g.as_of),
            st("vendor.b", &g, Value::UInt(9), 1_000_000, g.as_of),
        ],
        vec![
            res(&f, Value::UInt(5), ProofLevel::Recomputed, later),
            res_by(
                "vendor.h",
                &f,
                Value::UInt(6),
                ProofLevel::SourceProven,
                later,
            ),
            res_by(
                "vendor.a",
                &g,
                Value::UInt(9),
                ProofLevel::SourceProven,
                later,
            ),
        ],
    );
    let a = rec(&b, "vendor.a", "test.amount");
    assert_eq!((a.agreed, a.contested, a.self_resolved), (1, 0, 1));
    assert_eq!(
        rec(&b, "vendor.b", "test.amount").agreed,
        1,
        "another source's answer judges everyone else"
    );
}

/// TR-4: levels 4 and 5 never resolve anything; level 2 only when the params
/// name it.
#[test]
fn only_levels_one_to_three_resolve() {
    let reg = reg();
    for bad in [ProofLevel::BondedClaim, ProofLevel::TrackRecord] {
        let mut p = params();
        p.resolver_levels = vec![ProofLevel::Recomputed, bad];
        assert_eq!(
            p.validate(&reg),
            Err(TrackError::BadParams("resolver levels"))
        );
    }
    let f = fact("test.amount", "a.one", 0);
    let r = vec![res(
        &f,
        Value::UInt(5),
        ProofLevel::ReObserved,
        f.as_of + 2 * DAY,
    )];
    assert_eq!(
        compute(&params(), &reg, &[], &r, NOW).err(),
        Some(TrackError::LevelNotAccepted(2))
    );
    let mut p = params();
    p.resolver_levels = vec![ProofLevel::Recomputed, ProofLevel::ReObserved];
    assert!(compute(&p, &reg, &[], &r, NOW).is_ok());
}

/// Honest confidence is the unique best report on the ppm grid (binary Brier
/// is strictly proper): for each true chance q, the expected score is lowest
/// at p = q.
#[test]
fn brier_rewards_honest_confidence() {
    for q in [
        0u64, 1, 100_000, 333_333, 500_000, 900_000, 999_999, 1_000_000,
    ] {
        let expected = |p: u64| {
            u128::from(q) * u128::from(brier_term(Outcome::Agreed, Confidence(p as u32)))
                + u128::from(PPM - q)
                    * u128::from(brier_term(Outcome::Disagreed, Confidence(p as u32)))
        };
        let at_q = expected(q);
        for p in [
            0u64, 1, 2, 50_000, 333_334, 499_999, 500_001, 999_998, 1_000_000,
        ] {
            if p != q {
                assert!(expected(p) > at_q, "q {q} p {p}");
            }
        }
        for d in [1, 1000] {
            if q >= d {
                assert!(expected(q - d) > at_q);
            }
            if q + d <= PPM {
                assert!(expected(q + d) > at_q);
            }
        }
    }
    assert_eq!(brier_term(Outcome::Late, Confidence(1)), 0);
}

/// TR-R1-3: a statement claims its value is more likely right than not. Junk
/// at confidence 0 (a perfect Brier for "I am wrong") is refused.
#[test]
fn a_statement_below_even_odds_is_refused() {
    let f = fact("test.amount", "a.one", 0);
    for p in [0, 1, MIN_CONFIDENCE_PPM - 1] {
        let s = st("vendor.a", &f, Value::UInt(u128::MAX), p, f.as_of);
        assert_eq!(
            compute(&params(), &reg(), &[s], &[], NOW).err(),
            Some(TrackError::BadConfidence),
            "{p}"
        );
    }
    let s = st("vendor.a", &f, Value::UInt(5), MIN_CONFIDENCE_PPM, f.as_of);
    assert!(compute(&params(), &reg(), &[s], &[], NOW).is_ok());
}

#[test]
fn tolerance_applies_to_numbers_only() {
    let f = fact("test.price", "a.one", 0);
    let fixed = |m| Value::Fixed {
        mantissa: m,
        scale: 2,
    };
    let b = run(
        vec![
            st("vendor.a", &f, fixed(1_050), 500_000, f.as_of),
            st("vendor.b", &f, fixed(1_051), 500_000, f.as_of),
            st("vendor.c", &f, fixed(949), 500_000, f.as_of - 1),
        ],
        vec![res(&f, fixed(1_000), ProofLevel::SourceProven, f.as_of + 1)],
    );
    assert_eq!(rec(&b, "vendor.a", "test.price").agreed, 1);
    assert_eq!(rec(&b, "vendor.b", "test.price").disagreed, 1);
    assert_eq!(rec(&b, "vendor.c", "test.price").disagreed, 1);
    let mut p = params();
    p.rules[1].tolerance = 1; // test.flag is Bool
    assert_eq!(p.validate(&reg()), Err(TrackError::BadParams("tolerance")));
    assert!(agrees(&Value::Int(-5), &Value::Int(i128::MAX), u128::MAX));
    assert!(!agrees(
        &fixed(1),
        &Value::Fixed {
            mantissa: 1,
            scale: 3
        },
        10
    ));
}

/// R-EST-2 (ADR-0050): an estimate never judges a forecast, as resolver or as
/// the fact resolved.
#[test]
fn a_resolution_from_an_estimate_is_refused() {
    let reg = reg();
    let f = fact("test.amount", "a.one", 0);
    let mut r = res(&f, Value::UInt(5), ProofLevel::Recomputed, NOW);
    assert!(compute(&params(), &reg, &[], &[r.clone()], NOW).is_ok());
    r.resolver = SourceId("estimate.model".into());
    assert_eq!(
        compute(&params(), &reg, &[], &[r], NOW).err(),
        Some(TrackError::EstimateResolution)
    );
    let mut r = res(&f, Value::UInt(5), ProofLevel::Recomputed, NOW);
    r.fact.metric = MetricId("estimate.test.amount".into());
    assert_eq!(
        compute(&params(), &reg, &[], &[r], NOW).err(),
        Some(TrackError::EstimateResolution)
    );
}

#[test]
fn refuses_inadmissible_or_unordered_input() {
    let reg = reg();
    let f = fact("test.amount", "a.one", 0);
    let ok = st("vendor.a", &f, Value::UInt(5), 600_000, f.as_of);
    let cases: Vec<(Statement, TrackError)> = vec![
        (
            Statement {
                source: SourceId("vendor.unknown".into()),
                ..ok.clone()
            },
            TrackError::BadSource(SourceId("vendor.unknown".into())),
        ),
        (
            Statement {
                fact: fact("test.unruled", "a.one", 0),
                ..ok.clone()
            },
            TrackError::NoRule(MetricId("test.unruled".into())),
        ),
        (
            Statement {
                fact: Fact {
                    subject: Subject::Aggregate("a.nope".into()),
                    ..f.clone()
                },
                ..ok.clone()
            },
            TrackError::BadSubject,
        ),
        (
            Statement {
                value: Value::Int(5),
                ..ok.clone()
            },
            TrackError::BadValue,
        ),
        (
            Statement {
                confidence: Confidence(1_000_001),
                ..ok.clone()
            },
            TrackError::BadConfidence,
        ),
        (
            Statement {
                committed_at: NOW + 1,
                ..ok.clone()
            },
            TrackError::FromTheFuture,
        ),
        (
            Statement {
                fact: Fact {
                    as_of: u64::MAX,
                    ..f.clone()
                },
                committed_at: 0,
                ..ok.clone()
            },
            TrackError::Overflow,
        ),
    ];
    for (s, want) in cases {
        assert_eq!(compute(&params(), &reg, &[s], &[], NOW).err(), Some(want));
    }
    let other = st("vendor.b", &f, Value::UInt(5), 600_000, f.as_of);
    assert_eq!(
        compute(&params(), &reg, &[other.clone(), ok.clone()], &[], NOW).err(),
        Some(TrackError::NotCanonical("statements"))
    );
    assert_eq!(
        compute(&params(), &reg, &[ok.clone(), ok.clone()], &[], NOW).err(),
        Some(TrackError::NotCanonical("statements"))
    );
    let mut r0 = res(&f, Value::UInt(5), ProofLevel::Recomputed, NOW);
    r0.evidence = Hash32::ZERO;
    assert_eq!(
        compute(&params(), &reg, &[], &[r0], NOW).err(),
        Some(TrackError::ZeroEvidence)
    );
    let stranger = res_by("vendor.zz", &f, Value::UInt(5), ProofLevel::Recomputed, NOW);
    assert_eq!(
        compute(&params(), &reg, &[], &[stranger], NOW).err(),
        Some(TrackError::BadSource(SourceId("vendor.zz".into())))
    );
}

/// Same inputs, same commitment, in whatever order the caller collected them.
#[test]
fn the_computation_is_deterministic() {
    let (mut s, mut r) = history("vendor.a", 12, 4, &SUBJECTS);
    let (s2, r2) = history("vendor.b", 12, 1, &SUBJECTS);
    s.extend(s2);
    r.extend(r2);
    let a = run(s.clone(), r.clone()).commitment(Hash32::ZERO);
    s.reverse();
    r.reverse();
    let b = run(s, r).commitment(Hash32::ZERO);
    assert_eq!(a, b);
    assert_eq!(a.hash(), b.hash());
    assert_eq!((a.record_count, a.metric_count), (2, 1));
}

/// TR-7: copying another source earns the agreement, never the "first".
#[test]
fn a_copier_is_never_first() {
    let f = fact("test.amount", "a.one", 0);
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 900_000, f.as_of - 100),
            st("vendor.b", &f, Value::UInt(5), 900_000, f.as_of - 50),
            st("vendor.c", &f, Value::UInt(5), 900_000, f.as_of - 100),
        ],
        vec![res(
            &f,
            Value::UInt(5),
            ProofLevel::Recomputed,
            f.as_of + 2 * DAY,
        )],
    );
    assert_eq!(rec(&b, "vendor.a", "test.amount").agreed_first, 1);
    assert_eq!(
        rec(&b, "vendor.c", "test.amount").agreed_first,
        1,
        "a tie is first"
    );
    let copier = rec(&b, "vendor.b", "test.amount");
    assert_eq!((copier.agreed, copier.agreed_first), (1, 0));
}

/// TR-8: Sybil guessers. Eight names each guess a different pattern of a
/// yes/no fact; one of them looks good by luck, and every record says it is
/// one of eight sources on the metric, with the facts it covered.
#[test]
fn sybil_guessers_show_as_best_of_n() {
    let names = [
        "vendor.a", "vendor.b", "vendor.c", "vendor.d", "vendor.e", "vendor.f", "vendor.g",
        "vendor.h",
    ];
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in 0..3u64 {
        let f = fact("test.flag", SUBJECTS[d as usize], d);
        for (k, n) in names.iter().enumerate() {
            s.push(st(
                n,
                &f,
                Value::Bool((k >> d) & 1 == 1),
                1_000_000,
                f.as_of,
            ));
        }
        r.push(res(
            &f,
            Value::Bool(true),
            ProofLevel::SourceProven,
            f.as_of + 1,
        ));
    }
    let b = run(s, r);
    let lucky = rec(&b, "vendor.h", "test.flag");
    assert_eq!(
        (
            lucky.agreed,
            lucky.metric_sources,
            lucky.metric_resolved_facts
        ),
        (3, 8, 3)
    );
    assert!(!lucky.established());
    assert!(b.records().iter().all(|r| r.metric_sources == 8));
    let (_, m) = b.metric(&MetricId("test.flag".into())).expect("summary");
    assert_eq!((m.sources, m.resolved_facts), (8, 3));
}

fn proven_history(b: &TrackBook, source: &str, metric: &str) -> (u64, Vec<(Entry, Proof)>) {
    let (_, r) = b
        .record(&SourceId(source.into()), &MetricId(metric.into()))
        .expect("record");
    let rows = (r.entries_from..r.entries_from + r.entries_count)
        .map(|i| {
            (
                b.entries()[i as usize].clone(),
                b.entry_proof(i).expect("proof"),
            )
        })
        .collect();
    (r.entries_from, rows)
}

fn neighbour(b: &TrackBook, i: Option<u64>) -> Option<(Entry, Proof)> {
    i.filter(|i| (*i as usize) < b.entries().len()).map(|i| {
        (
            b.entries()[i as usize].clone(),
            b.entry_proof(i).expect("proof"),
        )
    })
}

fn as_ref(n: &Option<(Entry, Proof)>) -> Option<Proven<'_, Entry>> {
    n.as_ref().map(|(e, p)| (e, p))
}

fn three_sources() -> TrackBook {
    let (mut s, r) = history("vendor.b", 5, 2, &SUBJECTS);
    for src in ["vendor.a", "vendor.c"] {
        let (s2, _) = history(src, 5, 1, &SUBJECTS);
        s.extend(s2);
    }
    run(s, r)
}

fn summary_of(b: &TrackBook, metric: &str) -> (MetricSummary, Proof) {
    let (i, m) = b.metric(&MetricId(metric.into())).expect("summary");
    (m.clone(), b.metric_proof(i).expect("proof"))
}

/// A client checks a record and its whole history against the commitment.
#[test]
fn a_client_verifies_a_record_and_its_history() {
    let b = three_sources();
    let c = b.commitment(Hash32::ZERO);
    let (ri, r) = b
        .record(
            &SourceId("vendor.b".into()),
            &MetricId("test.amount".into()),
        )
        .expect("rec");
    let rp = b.record_proof(ri).expect("proof");
    let (m, mp) = summary_of(&b, "test.amount");
    assert!(verify_record(&c, (r, &rp)));
    let (from, rows) = proven_history(&b, "vendor.b", "test.amount");
    let refs: Vec<Proven<'_, Entry>> = rows.iter().map(|(e, p)| (e, p)).collect();
    let before = neighbour(&b, from.checked_sub(1));
    let after = neighbour(&b, Some(from + r.entries_count));
    let check = |rec: &Record, refs: &[Proven<'_, Entry>], before, after| {
        verify_history(&c, (rec, &rp), (&m, &mp), refs, before, after)
    };
    assert_eq!(check(r, &refs, as_ref(&before), as_ref(&after)), Ok(()));

    // Dropping a miss from the history, or leaving out a neighbour, is caught.
    assert_eq!(
        check(r, &refs[1..], as_ref(&before), as_ref(&after)),
        Err(VerifyError::Incomplete)
    );
    assert_eq!(
        check(r, &refs, None, as_ref(&after)),
        Err(VerifyError::Incomplete)
    );
    // Another source's entry passed off as this one's.
    let mut swapped = refs.clone();
    swapped[0] = as_ref(&after).expect("after");
    assert!(matches!(
        check(r, &swapped, as_ref(&before), as_ref(&after)),
        Err(VerifyError::Entry(_))
    ));
    // An edited record is not in the commitment.
    let mut better = r.clone();
    better.disagreed = 0;
    assert_eq!(
        check(&better, &refs, as_ref(&before), as_ref(&after)),
        Err(VerifyError::Record)
    );
}

/// TR-9: a server that commits an inflated record over honest entries is
/// caught by the recount.
#[test]
fn an_inflated_record_fails_the_recount() {
    let b = three_sources();
    let mut records = b.records().to_vec();
    let (ri, _) = b
        .record(
            &SourceId("vendor.b".into()),
            &MetricId("test.amount".into()),
        )
        .expect("rec");
    records[ri as usize].agreed += records[ri as usize].disagreed;
    records[ri as usize].disagreed = 0;
    let tree = MerkleTree::from_rows(&records.iter().map(row).collect::<Vec<_>>());
    let mut c = b.commitment(Hash32::ZERO);
    c.records_root = tree.root();
    let r = &records[ri as usize];
    let rp = tree.proof(ri).expect("proof");
    let (m, mp) = summary_of(&b, "test.amount");
    let (from, rows) = proven_history(&b, "vendor.b", "test.amount");
    let refs: Vec<Proven<'_, Entry>> = rows.iter().map(|(e, p)| (e, p)).collect();
    let before = neighbour(&b, from.checked_sub(1));
    let after = neighbour(&b, Some(from + r.entries_count));
    assert_eq!(
        verify_history(
            &c,
            (r, &rp),
            (&m, &mp),
            &refs,
            as_ref(&before),
            as_ref(&after)
        ),
        Err(VerifyError::Recount)
    );
}

/// TR-R1-6: a forged history that repeats agreed entries to look established
/// is refused (fact keys must strictly ascend in a record's range).
#[test]
fn a_padded_history_is_refused() {
    let b = three_sources();
    let (_, honest) = b
        .record(
            &SourceId("vendor.a".into()),
            &MetricId("test.amount".into()),
        )
        .expect("rec");
    let own: Vec<Entry> = b.entries()[..5].to_vec();
    let entries: Vec<Entry> = (0..30).map(|k| own[k % 5].clone()).collect();
    let etree = MerkleTree::from_rows(&entries.iter().map(row).collect::<Vec<_>>());
    let mut r = tally(&honest.source, &honest.metric, 0, &entries);
    r.metric_sources = honest.metric_sources;
    r.metric_resolved_facts = honest.metric_resolved_facts;
    let rtree = MerkleTree::from_rows(&[row(&r)]);
    let mut c = b.commitment(Hash32::ZERO);
    c.entries_root = etree.root();
    c.entry_count = 30;
    c.records_root = rtree.root();
    c.record_count = 1;
    let proofs: Vec<Proof> = (0..30).map(|i| etree.proof(i).expect("proof")).collect();
    let refs: Vec<Proven<'_, Entry>> = entries.iter().zip(&proofs).collect();
    let (m, mp) = summary_of(&b, "test.amount");
    assert_eq!(
        verify_history(
            &c,
            (&r, &rtree.proof(0).expect("proof")),
            (&m, &mp),
            &refs,
            None,
            None
        ),
        Err(VerifyError::Entry(5))
    );
}

/// TR-R1-5: "one of N sources" is checked against the committed metric
/// summary; a record claiming to be the only source is refused.
#[test]
fn a_hidden_crowd_is_caught_by_the_metric_summary() {
    let b = three_sources();
    let (ri, r) = b
        .record(
            &SourceId("vendor.b".into()),
            &MetricId("test.amount".into()),
        )
        .expect("rec");
    assert_eq!(r.metric_sources, 3);
    let mut alone = b.records().to_vec();
    alone[ri as usize].metric_sources = 1;
    let tree = MerkleTree::from_rows(&alone.iter().map(row).collect::<Vec<_>>());
    let mut c = b.commitment(Hash32::ZERO);
    c.records_root = tree.root();
    let (m, mp) = summary_of(&b, "test.amount");
    let (from, rows) = proven_history(&b, "vendor.b", "test.amount");
    let refs: Vec<Proven<'_, Entry>> = rows.iter().map(|(e, p)| (e, p)).collect();
    let before = neighbour(&b, from.checked_sub(1));
    let after = neighbour(&b, Some(from + r.entries_count));
    assert_eq!(
        verify_history(
            &c,
            (&alone[ri as usize], &tree.proof(ri).expect("proof")),
            (&m, &mp),
            &refs,
            as_ref(&before),
            as_ref(&after)
        ),
        Err(VerifyError::Summary)
    );
}

/// TR-R1-9: helpers on an unverified record never overflow, and a record with
/// counts no computation can produce is refused.
#[test]
fn out_of_bounds_records_are_refused_and_helpers_never_overflow() {
    let b = three_sources();
    let c = b.commitment(Hash32::ZERO);
    let mut r = b.records()[0].clone();
    r.agreed = u64::MAX;
    r.brier_sum_e12 = u128::MAX;
    assert_eq!(r.resolved(), u64::MAX);
    assert_eq!(r.agreement_ppm(), None);
    assert_eq!(r.cmp_brier(&b.records()[1]), None);
    let tree = MerkleTree::from_rows(&[row(&r)]);
    let forged = TrackCommitment {
        records_root: tree.root(),
        record_count: 1,
        ..c
    };
    assert!(!verify_record(
        &forged,
        (&r, &tree.proof(0).expect("proof"))
    ));
}

struct Era {
    params: TrackParams,
    statements: Vec<Statement>,
    resolutions: Vec<Resolution>,
    commitment: TrackCommitment,
}

impl Era {
    fn new(
        params: TrackParams,
        mut statements: Vec<Statement>,
        mut resolutions: Vec<Resolution>,
        as_of: u64,
        prev: Hash32,
    ) -> Era {
        sort_statements(&mut statements);
        sort_resolutions(&mut resolutions);
        let commitment = compute(&params, &reg(), &statements, &resolutions, as_of)
            .expect("compute")
            .commitment(prev);
        Era {
            params,
            statements,
            resolutions,
            commitment,
        }
    }

    fn log(&self) -> TrackLog<'_> {
        TrackLog {
            params: &self.params,
            statements: &self.statements,
            resolutions: &self.resolutions,
        }
    }
}

fn extends(a: &Era, b: &Era) -> Result<(), ExtendError> {
    check_extends(&a.commitment, a.log(), &b.commitment, b.log())
}

/// TR-10: history cannot be rewritten. A later commitment keeps every earlier
/// statement and resolution, and a statement new to it cannot be backdated
/// into a period already committed once its fact had closed (R-TR-8).
#[test]
fn history_cannot_be_rewritten() {
    let (s1, r1) = history("vendor.a", 4, 2, &SUBJECTS);
    let t1 = T0 + 10 * DAY;
    let t2 = t1 + DAY;
    let e1 = Era::new(params(), s1.clone(), r1.clone(), t1, Hash32::ZERO);
    let next =
        |s: Vec<Statement>, r: Vec<Resolution>| Era::new(params(), s, r, t2, e1.commitment.hash());

    let f = fact("test.amount", "a.one", 20);
    let mut s2 = s1.clone();
    s2.push(st("vendor.a", &f, Value::UInt(1), 600_000, t1 + 1));
    assert_eq!(extends(&e1, &next(s2.clone(), r1.clone())), Ok(()));

    // Dropping an old miss.
    let missed: Vec<Statement> = s2
        .iter()
        .filter(|s| s.value != Value::UInt(999))
        .cloned()
        .collect();
    assert_eq!(
        extends(&e1, &next(missed, r1.clone())),
        Err(ExtendError::StatementDropped)
    );
    // Backdating a new statement into the committed period, on a fact
    // whose close had passed by then (day 5, close day 6 <= t1).
    let closed = fact("test.amount", "a.two", 5);
    let mut back = s1.clone();
    back.push(st("vendor.a", &closed, Value::UInt(1), 600_000, t1));
    assert_eq!(
        extends(&e1, &next(back, r1.clone())),
        Err(ExtendError::Backdated)
    );
    // A statement committed by t1 on a fact still open then was sealed until
    // its close (ADR-0036 amendment 1 item 4): it may enter later, but only
    // with an intake pinned, whose anchor check bounds its time
    // (intake::verify_log); without one it is backdated (review TR-R4-1).
    let mut sealed = s1.clone();
    sealed.push(st("vendor.a", &f, Value::UInt(1), 600_000, t1));
    assert_eq!(
        extends(&e1, &next(sealed.clone(), r1.clone())),
        Err(ExtendError::Backdated)
    );
    let pinned = params().with_intake(Hash32([9; 32]));
    let p1 = Era::new(pinned.clone(), s1.clone(), r1.clone(), t1, Hash32::ZERO);
    let p2 = Era::new(pinned, sealed, r1.clone(), t2, p1.commitment.hash());
    assert_eq!(extends(&p1, &p2), Ok(()));
    // Removing a resolution.
    let mut fewer = e1.resolutions.clone();
    fewer.remove(0);
    assert_eq!(
        extends(&e1, &next(s2.clone(), fewer)),
        Err(ExtendError::ResolutionDropped)
    );
    // Not chained, not later, or a log that is not the committed one.
    let mut unchained = next(s2.clone(), r1.clone());
    unchained.commitment.prev = Hash32::ZERO;
    assert_eq!(extends(&e1, &unchained), Err(ExtendError::NotChained));
    let same_time = Era::new(params(), s1.clone(), r1.clone(), t1, e1.commitment.hash());
    assert_eq!(extends(&e1, &same_time), Err(ExtendError::TimeNotForward));
    let mut wrong_log = next(s2, r1);
    wrong_log.statements = e1.statements.clone();
    assert_eq!(extends(&e1, &wrong_log), Err(ExtendError::LogMismatch));
}

/// Review TR-R4-1 (R-TR-8 relaxed by amendment 1 item 4): without an intake
/// (`params.intake` zero) `committed_at` is caller input, so a statement new
/// to a later commitment must still be committed after the earlier `as_of`
/// whatever its close. Otherwise a coordinator could add, once the answer is
/// public, a statement dated into an already committed period on a fact
/// still open then, and score a hit it never made. The relaxed rule
/// (`max(committed_at, close)`) holds only with an intake pinned, where
/// `intake::verify_log` bounds `committed_at` by the post's block.
#[test]
fn a_statement_cannot_be_inserted_after_the_answer_without_an_intake() {
    let (s1, r1) = history("vendor.a", 4, 2, &SUBJECTS);
    let t1 = T0 + 10 * DAY;
    // Open at t1: as_of day 20, close day 21; the answer is public on day 22.
    let f = fact("test.amount", "a.one", 20);
    let answer = res(
        &f,
        Value::UInt(7),
        ProofLevel::Recomputed,
        f.as_of + 2 * DAY,
    );
    let t2 = T0 + 30 * DAY;
    // Inserted at t2, after the answer, dated day 5 (inside e1's period).
    let forged = st("vendor.a", &f, Value::UInt(7), 900_000, T0 + 5 * DAY);
    let (mut s2, mut r2) = (s1.clone(), r1.clone());
    s2.push(forged);
    r2.push(answer);
    for (p, want) in [
        (params(), Err(ExtendError::Backdated)),
        // With an intake the rule is relaxed; verify_log is the guard
        // (`intake::tests::a_statement_inserted_after_the_answer_fails_verify_log`).
        (params().with_intake(Hash32([9; 32])), Ok(())),
    ] {
        let e1 = Era::new(p.clone(), s1.clone(), r1.clone(), t1, Hash32::ZERO);
        let e2 = Era::new(p, s2.clone(), r2.clone(), t2, e1.commitment.hash());
        assert_eq!(extends(&e1, &e2), want);
    }
}

/// TR-R1-1: a later commitment cannot erase a committed miss by appending a
/// contradicting resolution (Contested) or an early copy (Void); a fact first
/// resolved later must be resolved by an answer public after the earlier
/// commitment. Since ADR-0036 amendment 3 item 6 a later answer at the same
/// level may join a resolved fact (it judges only entries left self-resolved),
/// so the contradicting one is taken and changes nothing committed; an early
/// copy or a stronger answer is still refused.
#[test]
fn an_appended_resolution_cannot_erase_a_miss() {
    let (s1, r1) = history("vendor.a", 2, 1, &SUBJECTS);
    let t1 = T0 + 10 * DAY;
    let t2 = t1 + DAY;
    let e1 = Era::new(params(), s1.clone(), r1.clone(), t1, Hash32::ZERO);
    let f = s1[0].fact.clone();
    let late = res_by(
        "vendor.h",
        &f,
        Value::UInt(999),
        ProofLevel::Recomputed,
        t1 + 1,
    );
    let mut r2 = r1.clone();
    r2.push(late);
    let e2 = Era::new(params(), s1.clone(), r2.clone(), t2, e1.commitment.hash());
    assert_eq!(extends(&e1, &e2), Ok(()));
    let mut sorted = r2;
    sort_resolutions(&mut sorted);
    let before = run(s1.clone(), r1.clone());
    let after = run(s1.clone(), sorted);
    assert_eq!(
        before.entries(),
        after.entries(),
        "the committed miss stands"
    );
    assert_eq!(
        rec(&after, "vendor.a", "test.amount").disagreed,
        1,
        "not contested by the later answer"
    );
    let early = res_by(
        "vendor.h",
        &f,
        Value::UInt(100),
        ProofLevel::Recomputed,
        f.as_of,
    );
    let mut r2 = r1.clone();
    r2.push(early);
    let e2 = Era::new(params(), s1.clone(), r2, t2, e1.commitment.hash());
    assert_eq!(extends(&e1, &e2), Err(ExtendError::ResolutionRewrites));
    // A fact resolved at level 3 cannot gain a stronger (level-1) answer later:
    // the strongest level would change every entry on it.
    let h = fact("test.amount", "a.five", 9);
    let s3 = vec![st("vendor.a", &h, Value::UInt(1), 600_000, h.as_of)];
    let r3 = vec![res(
        &h,
        Value::UInt(2),
        ProofLevel::SourceProven,
        h.as_of + DAY,
    )];
    let e1h = Era::new(params(), s3.clone(), r3.clone(), t1, Hash32::ZERO);
    let mut r4 = r3.clone();
    r4.push(res_by(
        "vendor.h",
        &h,
        Value::UInt(1),
        ProofLevel::Recomputed,
        t1 + 1,
    ));
    let e2h = Era::new(params(), s3, r4, t2, e1h.commitment.hash());
    assert_eq!(extends(&e1h, &e2h), Err(ExtendError::ResolutionRewrites));
    // A new fact resolved by an answer public only after t1 is fine; one
    // public before t1 was withheld and is refused.
    let g = fact("test.amount", "a.six", 5);
    let mut s2 = s1.clone();
    s2.push(st("vendor.a", &g, Value::UInt(1), 600_000, g.as_of));
    let e1 = Era::new(params(), s2.clone(), r1.clone(), t1, Hash32::ZERO);
    for (public_from, want) in [(t1 + 1, Ok(())), (t1, Err(ExtendError::ResolutionRewrites))] {
        let mut r2 = r1.clone();
        r2.push(res(&g, Value::UInt(1), ProofLevel::Recomputed, public_from));
        let e2 = Era::new(params(), s2.clone(), r2, t2, e1.commitment.hash());
        assert_eq!(extends(&e1, &e2), want, "{public_from}");
    }
}

/// TR-R1-2: the params of a later commitment keep every committed rule and
/// the resolver levels; only rules for new metrics may be added.
#[test]
fn params_cannot_change_a_committed_rule() {
    let (s1, r1) = history("vendor.a", 3, 2, &SUBJECTS);
    let t1 = T0 + 10 * DAY;
    let e1 = Era::new(params(), s1.clone(), r1.clone(), t1, Hash32::ZERO);
    let mut wide = params();
    wide.rules[0].tolerance = 10_000;
    let e2 = Era::new(wide, s1.clone(), r1.clone(), t1 + DAY, e1.commitment.hash());
    assert_eq!(extends(&e1, &e2), Err(ExtendError::ParamsChanged));
    let mut levels = params();
    levels.resolver_levels = vec![ProofLevel::Recomputed];
    let e2 = Era::new(
        levels,
        s1.clone(),
        r1.clone(),
        t1 + DAY,
        e1.commitment.hash(),
    );
    assert_eq!(extends(&e1, &e2), Err(ExtendError::ParamsChanged));
    let mut more = params();
    more.rules.push(rule("test.unruled", 0, 0));
    let e2 = Era::new(more, s1, r1, t1 + DAY, e1.commitment.hash());
    assert_eq!(extends(&e1, &e2), Ok(()));
}

/// TR-11 (ADR-0036 item 12): no record changes a level or reaches money. A
/// level-5 value stays level 5 with a perfect, established record, and the
/// money default still refuses it.
#[test]
fn a_perfect_record_never_changes_a_level() {
    let (s, r) = history("vendor.a", 40, 0, &SUBJECTS);
    let b = run(s, r);
    let r = rec(&b, "vendor.a", "test.amount");
    assert!(r.established());
    assert_eq!(r.agreement_ppm(), Some(1_000_000));
    assert_eq!(
        VerificationMethod::CalibratedJudgment.proof_level(),
        ProofLevel::TrackRecord
    );
    assert!(!MarketPolicy::money_default().accepts(ProofLevel::TrackRecord));
    assert!(!MarketPolicy::default().accepts(ProofLevel::TrackRecord));
}

#[test]
fn records_compare_by_exact_mean_brier() {
    let (mut s, mut r) = history("vendor.a", 3, 1, &SUBJECTS);
    let (s2, r2) = history("vendor.b", 9, 3, &SUBJECTS);
    s.extend(s2);
    r.extend(r2);
    let b = run(s, r);
    let a = rec(&b, "vendor.a", "test.amount");
    let bb = rec(&b, "vendor.b", "test.amount");
    assert_eq!(
        a.cmp_brier(bb),
        Some(Ordering::Equal),
        "1 of 3 and 3 of 9 wrong"
    );
    assert_eq!(a.mean_brier_e12(), bb.mean_brier_e12());
    let empty = Record {
        agreed: 0,
        disagreed: 0,
        equivocated: 0,
        ..a.clone()
    };
    assert_eq!(empty.cmp_brier(a), None);
    assert_eq!(empty.agreement_ppm(), None);
}

/// TR-R2-1: a source's own answer never contests the answer that judges it.
/// A level-3 signer that stated a wrong value cannot sign a level-3 answer
/// agreeing with itself to turn its miss into `Contested`.
#[test]
fn an_own_answer_cannot_contest_a_miss() {
    let f = fact("test.amount", "a.one", 0);
    let later = f.as_of + 3 * DAY;
    let b = run(
        vec![
            st("vendor.a", &f, Value::UInt(5), 900_000, f.as_of),
            st("vendor.b", &f, Value::UInt(6), 900_000, f.as_of),
        ],
        vec![
            res(&f, Value::UInt(6), ProofLevel::SourceProven, later),
            res_by(
                "vendor.a",
                &f,
                Value::UInt(5),
                ProofLevel::SourceProven,
                later,
            ),
        ],
    );
    let a = rec(&b, "vendor.a", "test.amount");
    assert_eq!((a.disagreed, a.contested), (1, 0), "own answer ignored");
    assert_eq!(a.brier_sum_e12, 810_000_000_000);
    // Others still see two disagreeing answers at the strongest level.
    assert_eq!(rec(&b, "vendor.b", "test.amount").contested, 1);
}

/// TR-R2-2: a resolution cannot claim to be public only after the commitment
/// that includes it (a far-future `public_from` would dodge `Void` and the
/// withheld-resolution check of `check_extends`).
#[test]
fn a_resolution_public_after_as_of_is_refused() {
    let f = fact("test.amount", "a.one", 0);
    let s = vec![st("vendor.a", &f, Value::UInt(5), 900_000, f.as_of)];
    for (public_from, ok) in [(NOW, true), (NOW + 1, false), (u64::MAX, false)] {
        let r = vec![res(&f, Value::UInt(5), ProofLevel::Recomputed, public_from)];
        let got = compute(&params(), &reg(), &s, &r, NOW);
        assert_eq!(got.is_ok(), ok, "{public_from}");
        if !ok {
            assert_eq!(got.err(), Some(TrackError::FromTheFuture));
        }
    }
}

// ---------------------------------------------------------------- C9: period facts

/// 2026-09-22 00:00 UTC: a UTC day boundary (`T0` is not one).
const D0: u64 = 20_718 * DAY;
/// Bitcoin's lead in these tests: a block may be timestamped up to 2 h ahead.
const LEAD: u64 = 7_200;

fn period_reg() -> StaticRegistry {
    let mut r = reg();
    r.add_metric(
        "test.flow",
        MetricInfo {
            value_kind: ValueKind::Bytes,
            needs_label_set: false,
        },
    )
    .expect("flow metric");
    r
}

fn period_params() -> TrackParams {
    let mut rules = params().rules;
    rules.push(MetricRule::period_start(
        MetricId("test.flow".into()),
        DAY,
        LEAD,
        0,
    ));
    rules.sort_by(|a, b| a.metric.cmp(&b.metric));
    TrackParams::new(rules)
}

/// A flow fact for the UTC day ending `day` days after `D0`.
fn flow(subject: &str, day: u64) -> Fact {
    Fact {
        metric: MetricId("test.flow".into()),
        subject: Subject::Aggregate(subject.into()),
        as_of: D0 + day * DAY,
    }
}

fn run_period(mut s: Vec<Statement>, mut r: Vec<Resolution>) -> TrackBook {
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    compute(&period_params(), &period_reg(), &s, &r, NOW).expect("compute")
}

/// C9 (TR-13): a period fact closes `lead` before its period starts, so a
/// statement made once the period's first block could be public is `Late`,
/// and one made before is judged.
#[test]
fn a_period_fact_closes_before_its_period_starts() {
    let f = flow("a.one", 10);
    let start = f.as_of - DAY;
    let close = start - LEAD;
    assert_eq!(period_params().close_of(&f), Some(close));
    let rows = Value::Bytes(vec![1, 2, 3]);
    let answer = res(&f, rows.clone(), ProofLevel::Recomputed, f.as_of + 3_600);
    for (at, want) in [
        (close - DAY, Outcome::Agreed),
        (close, Outcome::Agreed),
        (close + 1, Outcome::Late),
        (start, Outcome::Late),
        (f.as_of, Outcome::Late),
    ] {
        let b = run_period(
            vec![st("vendor.a", &f, rows.clone(), 900_000, at)],
            vec![answer.clone()],
        );
        assert_eq!(
            b.entries()[0].outcome,
            want,
            "committed at close{:+}",
            at as i64 - close as i64
        );
    }
    // The truth public at or before the close (a wrong fact date, a leak) is `Void`.
    let early = res(&f, rows.clone(), ProofLevel::Recomputed, close);
    let b = run_period(
        vec![st("vendor.a", &f, rows, 900_000, close - DAY)],
        vec![early],
    );
    assert_eq!(b.entries()[0].outcome, Outcome::Void);
}

/// C9: the fact carries its period through its aligned `as_of`; one that is
/// not a whole period (or starts before time 0) is refused, for statements and
/// resolutions alike, never judged against a shifted period.
#[test]
fn a_period_fact_must_name_a_whole_period() {
    let mut f = flow("a.one", 10);
    f.as_of += 1;
    let v = Value::Bytes(vec![1]);
    let s = vec![st("vendor.a", &f, v.clone(), 900_000, D0)];
    assert_eq!(
        compute(&period_params(), &period_reg(), &s, &[], NOW).err(),
        Some(TrackError::BadPeriod)
    );
    let ok = flow("a.one", 10);
    let s = vec![st("vendor.a", &ok, v.clone(), 900_000, D0)];
    let r = vec![res(&f, v.clone(), ProofLevel::Recomputed, ok.as_of)];
    assert_eq!(
        compute(&period_params(), &period_reg(), &s, &r, NOW).err(),
        Some(TrackError::BadPeriod)
    );
    // A period that would start before time 0.
    let zero = Fact {
        as_of: DAY,
        ..flow("a.one", 0)
    };
    assert_eq!(period_params().close_of(&zero), None);
    let s = vec![st("vendor.a", &zero, v, 900_000, 0)];
    assert_eq!(
        compute(&period_params(), &period_reg(), &s, &[], NOW).err(),
        Some(TrackError::BadPeriod)
    );
}

/// C9: a period rule names a period of 1 s to 366 days and a lead of at most
/// 366 days.
#[test]
fn a_period_rule_is_bounded() {
    let with = |period_secs, lead_secs| {
        let mut p = period_params();
        for r in p.rules.iter_mut() {
            if r.metric.0 == "test.flow" {
                r.close = Close::PeriodStart {
                    period_secs,
                    lead_secs,
                };
            }
        }
        p.validate(&period_reg())
    };
    assert_eq!(with(DAY, LEAD), Ok(()));
    assert_eq!(with(MAX_PERIOD_SECS, MAX_PERIOD_SECS), Ok(()));
    for (p, l) in [(0, 0), (MAX_PERIOD_SECS + 1, 0), (DAY, MAX_PERIOD_SECS + 1)] {
        assert_eq!(with(p, l), Err(TrackError::BadParams("period")), "{p} {l}");
    }
}

/// C9: a commitment may not take a period statement it could have revealed
/// already (sealed until the period's end, review TR-C9-R1-5).
#[test]
fn a_period_statement_cannot_be_backdated_into_a_committed_log() {
    let f = flow("a.one", 10);
    let close = f.as_of - DAY - LEAD;
    let p = period_params().with_intake(keccak256(b"test intake"));
    let reg = period_reg();
    let prev_as_of = f.as_of + 10;
    let first = compute(&p, &reg, &[], &[], prev_as_of).expect("first");
    let c0 = first.commitment(Hash32::ZERO);
    let empty = TrackLog {
        params: &p,
        statements: &[],
        resolutions: &[],
    };
    for (at, ok) in [(close - 5, false), (f.as_of + 11, true)] {
        let s = vec![st("vendor.a", &f, Value::Bytes(vec![1]), 900_000, at)];
        let next = compute(&p, &reg, &s, &[], f.as_of + 20).expect("next");
        let c1 = next.commitment(c0.hash());
        let got = check_extends(
            &c0,
            empty,
            &c1,
            TrackLog {
                params: &p,
                statements: &s,
                resolutions: &[],
            },
        );
        assert_eq!(got.is_ok(), ok, "{got:?}");
    }
}

// ---------------------------------------------------------------- C9: skill against persistence

/// The flow value of `a.one` for day `d`: 100 until day 4, 200 from day 5.
fn level_shift(d: u64) -> Value {
    Value::Bytes(if d < 5 { vec![100] } else { vec![200] })
}

/// The answer for flow day `d`: public an hour after the day ends.
fn flow_answer(f: &Fact, d: u64) -> Resolution {
    res(f, level_shift(d), ProofLevel::Recomputed, f.as_of + 3_600)
}

/// Days 0..10 of `a.one`'s flow, answered an hour after each day ends, and
/// two sources stating on time (a day before each close).
fn persistence_world() -> (Vec<Statement>, Vec<Resolution>) {
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in 0..10 {
        let f = flow("a.one", d);
        let on_time = f.as_of - 2 * DAY - LEAD;
        r.push(flow_answer(&f, d));
        // vendor.a repeats the last value known at its close (day d - 2).
        let last = level_shift(d.saturating_sub(2));
        s.push(st("vendor.a", &f, last, 900_000, on_time));
        // vendor.b knows better.
        s.push(st("vendor.b", &f, level_shift(d), 900_000, on_time));
    }
    (s, r)
}

/// C9 (PLAN q-TR-5, TR-I-5): the record shows how the source did against
/// persistence, the last value known at each close. Repeating that value
/// earns agreements but no skill; seeing the change coming does.
#[test]
fn skill_is_measured_against_the_last_known_value() {
    let (s, r) = persistence_world();
    let b = run_period(s, r);
    let copier = rec(&b, "vendor.a", "test.flow");
    let seer = rec(&b, "vendor.b", "test.flow");
    // Day d's flow closes 2 h before the day starts; day d-1's answer only
    // comes after that, so the last value known is day d-2's: days 0 and 1
    // have none.
    assert_eq!(
        (copier.agreed, copier.resolved(), copier.baseline_entries),
        (8, 10, 8)
    );
    assert_eq!(
        (copier.baseline_source_agreed, copier.baseline_agreed),
        (6, 6)
    );
    assert_eq!(copier.skill_ppm(), Some(0));
    assert_eq!((seer.agreed, seer.baseline_entries), (10, 8));
    assert_eq!((seer.baseline_source_agreed, seer.baseline_agreed), (8, 6));
    assert_eq!(seer.skill_ppm(), Some(250_000));
    let days: Vec<Baseline> = b
        .entries()
        .iter()
        .filter(|e| e.source.0 == "vendor.b")
        .map(|e| (e.fact.as_of, e.baseline))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    use Baseline::{Agreed as A, Disagreed as D, None as N};
    assert_eq!(days, vec![N, N, A, A, A, D, D, A, A, A]);
    // The recount covers the baseline counts.
    let (ri, r) = b
        .record(&SourceId("vendor.b".into()), &MetricId("test.flow".into()))
        .expect("record");
    let c = b.commitment(Hash32::ZERO);
    let rp = b.record_proof(ri).expect("proof");
    let (m, mp) = summary_of(&b, "test.flow");
    let (from, rows) = proven_history(&b, "vendor.b", "test.flow");
    let refs: Vec<Proven<'_, Entry>> = rows.iter().map(|(e, p)| (e, p)).collect();
    let before = neighbour(&b, from.checked_sub(1));
    let after = neighbour(&b, Some(from + r.entries_count));
    assert_eq!(
        verify_history(
            &c,
            (r, &rp),
            (&m, &mp),
            &refs,
            as_ref(&before),
            as_ref(&after)
        ),
        Ok(())
    );
    let mut boasting = r.clone();
    boasting.baseline_agreed = 0;
    assert_eq!(
        verify_history(
            &c,
            (&boasting, &rp),
            (&m, &mp),
            &refs,
            as_ref(&before),
            as_ref(&after)
        ),
        Err(VerifyError::Record)
    );
}

/// C9: point facts (an `AfterAsOf` rule) have no period grid, so "the last
/// value" is undefined and no entry has a baseline.
#[test]
fn point_facts_have_no_baseline() {
    let (s, r) = history("vendor.a", 10, 3, &["a.one"]);
    let b = run(s, r);
    let a = rec(&b, "vendor.a", "test.amount");
    assert_eq!(
        (a.resolved(), a.baseline_entries, a.skill_ppm()),
        (10, 0, None)
    );
}

/// Review TR-C9-R1-2: a copier stating only every few days cannot make
/// persistence look stale. Only the period that ended last by the close is a
/// baseline.
#[test]
fn sparse_stating_cannot_stale_the_baseline() {
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in [0, 8] {
        let f = flow("a.one", d);
        r.push(flow_answer(&f, d));
        // Repeats the true last value known (day 6's, from outside).
        s.push(st(
            "vendor.a",
            &f,
            level_shift(d.saturating_sub(2)),
            900_000,
            f.as_of - 2 * DAY - LEAD,
        ));
    }
    let b = run_period(s, r);
    let a = rec(&b, "vendor.a", "test.flow");
    assert_eq!((a.agreed, a.baseline_entries, a.skill_ppm()), (2, 0, None));
}

/// Review TR-C9-R2-1: a copier that skips the latest period it could know
/// (stating every third day) cannot fall back to an older baseline. Before
/// the fix the 2-period window took day 4 for day 7 (day 5 unnamed), so
/// repeating day 5's value, the last one known at day 7's close, scored
/// +100 points. The baseline is the latest period ended by the close or none.
#[test]
fn a_skipping_copier_cannot_pick_an_older_baseline() {
    let on_time = |f: &Fact| f.as_of - 2 * DAY - LEAD;
    let mut s = Vec::new();
    let mut r = Vec::new();
    for d in [4, 7] {
        let f = flow("a.one", d);
        r.push(flow_answer(&f, d));
        // Repeats the last value known at the close (day d - 2's).
        s.push(st("vendor.a", &f, level_shift(d - 2), 900_000, on_time(&f)));
    }
    let d5 = flow("a.one", 5);
    r.push(flow_answer(&d5, 5));
    let b = run_period(s.clone(), r.clone());
    let a = rec(&b, "vendor.a", "test.flow");
    assert_eq!((a.agreed, a.baseline_entries, a.skill_ppm()), (2, 0, None));
    // Once anyone names day 5 on time, it is the copier's baseline: skill 0.
    s.push(st("vendor.b", &d5, level_shift(5), 900_000, on_time(&d5)));
    let b = run_period(s, r);
    let a = rec(&b, "vendor.a", "test.flow");
    assert_eq!(
        (
            a.baseline_entries,
            a.baseline_source_agreed,
            a.baseline_agreed
        ),
        (1, 1, 1)
    );
    assert_eq!(a.skill_ppm(), Some(0));
}

/// C9 adversarial: a source cannot make persistence look bad with its own (or
/// an affiliate's) answer on the earlier fact; that answer is left out of its
/// baseline, as of its judging (TR-R2-1, TR-R2-4).
#[test]
fn an_own_answer_never_sets_the_baseline() {
    let d0 = flow("a.one", 0);
    let d2 = flow("a.one", 2);
    let on_time = |f: &Fact| f.as_of - 2 * DAY - LEAD;
    let s = vec![
        st(
            "vendor.b",
            &d0,
            Value::Bytes(vec![7]),
            900_000,
            on_time(&d0),
        ),
        st(
            "vendor.a",
            &d2,
            Value::Bytes(vec![1]),
            900_000,
            on_time(&d2),
        ),
        st(
            "vendor.b",
            &d2,
            Value::Bytes(vec![1]),
            900_000,
            on_time(&d2),
        ),
    ];
    let r = vec![
        // vendor.a's own wrong answer on day 0 is the only one.
        res_by(
            "vendor.a",
            &d0,
            Value::Bytes(vec![7]),
            ProofLevel::SourceProven,
            d0.as_of + 60,
        ),
        res(
            &d2,
            Value::Bytes(vec![1]),
            ProofLevel::SourceProven,
            d2.as_of + 60,
        ),
    ];
    let p = TrackParams {
        resolver_levels: vec![ProofLevel::SourceProven],
        ..period_params()
    };
    let go = |p: &TrackParams, mut s: Vec<Statement>, mut r: Vec<Resolution>| {
        sort_statements(&mut s);
        sort_resolutions(&mut r);
        compute(p, &period_reg(), &s, &r, NOW).expect("compute")
    };
    let b = go(&p, s.clone(), r.clone());
    assert_eq!(rec(&b, "vendor.a", "test.flow").baseline_entries, 0);
    // For vendor.b, vendor.a's answer is an independent one.
    let other = rec(&b, "vendor.b", "test.flow");
    assert_eq!((other.baseline_entries, other.baseline_agreed), (1, 0));
    // An affiliate's answer is left out the same way.
    let p = p.with_operators(vec![
        OperatorOf {
            source: SourceId("vendor.a".into()),
            operator: keccak256(b"op"),
        },
        OperatorOf {
            source: SourceId("vendor.c".into()),
            operator: keccak256(b"op"),
        },
    ]);
    let mut r2 = r;
    r2[0].resolver = SourceId("vendor.c".into());
    let b = go(&p, s, r2);
    assert_eq!(rec(&b, "vendor.a", "test.flow").baseline_entries, 0);
}

/// C9 adversarial (review TR-C9-R1-1): what a committed entry's baseline read
/// is frozen. Later statements on earlier facts (late, so never a baseline,
/// however many) and a later answer on the baseline fact (public after the
/// close) change nothing, and the later commitment still extends the earlier.
#[test]
fn a_committed_baseline_never_changes() {
    let d0 = flow("a.one", 0);
    let d1 = flow("a.one", 1);
    let d2 = flow("a.one", 2);
    let on_time = |f: &Fact| f.as_of - 2 * DAY - LEAD;
    let mut s = vec![
        st(
            "vendor.b",
            &d0,
            Value::Bytes(vec![1]),
            900_000,
            on_time(&d0),
        ),
        st(
            "vendor.a",
            &d2,
            Value::Bytes(vec![1]),
            900_000,
            on_time(&d2),
        ),
    ];
    let mut r = vec![
        res(
            &d0,
            Value::Bytes(vec![1]),
            ProofLevel::Recomputed,
            d0.as_of + 60,
        ),
        res(
            &d2,
            Value::Bytes(vec![1]),
            ProofLevel::Recomputed,
            d2.as_of + 60,
        ),
    ];
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    let early = d2.as_of + DAY;
    let p = period_params();
    let b0 = compute(&p, &period_reg(), &s, &r, early).expect("b0");
    let a0 = rec(&b0, "vendor.a", "test.flow").clone();
    assert_eq!((a0.baseline_entries, a0.baseline_agreed), (1, 1));
    // Later: late statements name day 1 and 300 earlier days (the PoC of
    // TR-C9-R1-1 flooded a lookback window this way), and day 1 and day 0 get
    // answers that would make persistence miss if they counted.
    let mut s1 = s.clone();
    s1.push(st(
        "vendor.c",
        &d1,
        Value::Bytes(vec![9]),
        900_000,
        early + 1,
    ));
    for k in 1..=300u64 {
        let f = Fact {
            as_of: d0.as_of - k * DAY,
            ..d0.clone()
        };
        s1.push(st(
            "vendor.c",
            &f,
            Value::Bytes(vec![9]),
            900_000,
            early + 1,
        ));
    }
    let mut r1 = r.clone();
    r1.push(res(
        &d1,
        Value::Bytes(vec![9]),
        ProofLevel::Recomputed,
        d1.as_of + 60,
    ));
    r1.push(res_by(
        "vendor.d",
        &d0,
        Value::Bytes(vec![9]),
        ProofLevel::Recomputed,
        early + 1,
    ));
    sort_statements(&mut s1);
    sort_resolutions(&mut r1);
    let b1 = compute(&p, &period_reg(), &s1, &r1, early + DAY).expect("b1");
    let a1 = rec(&b1, "vendor.a", "test.flow");
    assert_eq!(
        (a1.baseline_entries, a1.baseline_agreed, a1.skill_ppm()),
        (a0.baseline_entries, a0.baseline_agreed, a0.skill_ppm())
    );
    let c0 = b0.commitment(Hash32::ZERO);
    let c1 = b1.commitment(c0.hash());
    let log = |s, r| TrackLog {
        params: &p,
        statements: s,
        resolutions: r,
    };
    assert_eq!(check_extends(&c0, log(&s, &r), &c1, log(&s1, &r1)), Ok(()));
}

/// C9: the skill helper never overflows or divides by zero, floors, and
/// refuses an inconsistent (unverified) record.
#[test]
fn skill_is_an_exact_floored_integer() {
    let (s, r) = persistence_world();
    let b = run_period(s, r);
    let mut x = rec(&b, "vendor.a", "test.flow").clone();
    (
        x.baseline_entries,
        x.baseline_source_agreed,
        x.baseline_agreed,
    ) = (3, 0, 1);
    assert_eq!(x.skill_ppm(), Some(-333_334));
    (
        x.baseline_entries,
        x.baseline_source_agreed,
        x.baseline_agreed,
    ) = (3, 3, 0);
    assert_eq!(x.skill_ppm(), Some(1_000_000));
    x.baseline_entries = 0;
    assert_eq!(x.skill_ppm(), None);
    (
        x.baseline_entries,
        x.baseline_source_agreed,
        x.baseline_agreed,
    ) = (2, 3, 0);
    assert_eq!(x.skill_ppm(), None);
    (
        x.baseline_entries,
        x.baseline_source_agreed,
        x.baseline_agreed,
    ) = (u64::MAX, u64::MAX, 0);
    assert_eq!(x.skill_ppm(), Some(1_000_000));
    // Unresolved or uncounted entries never carry a baseline.
    assert!(b
        .entries()
        .iter()
        .all(|e| e.outcome.counted() || e.baseline == Baseline::None));
}

/// Review TR-C9-R1-5: a period statement stays sealed until its period ends,
/// not only until its close, so no forecast is public while the total it
/// forecasts can still be moved.
#[test]
fn a_period_statement_is_revealed_at_the_periods_end() {
    let f = flow("a.one", 10);
    let p = period_params();
    assert_eq!(p.close_of(&f), Some(f.as_of - DAY - LEAD));
    assert_eq!(p.reveal_of(&f), Some(f.as_of));
    let point = fact("test.amount", "a.one", 3);
    assert_eq!(p.reveal_of(&point), p.close_of(&point));
    // A sealed statement committed before the close may enter any commitment
    // up to the period's end (it was sealed), not after it.
    let p = p.with_intake(keccak256(b"test intake"));
    let reg = period_reg();
    let committed = f.as_of - DAY - LEAD - 10;
    let s = vec![st(
        "vendor.a",
        &f,
        Value::Bytes(vec![1]),
        900_000,
        committed,
    )];
    let empty = TrackLog {
        params: &p,
        statements: &[],
        resolutions: &[],
    };
    for (prev_as_of, ok) in [(f.as_of - 1, true), (f.as_of, false)] {
        let c0 = compute(&p, &reg, &[], &[], prev_as_of)
            .expect("first")
            .commitment(Hash32::ZERO);
        let c1 = compute(&p, &reg, &s, &[], f.as_of + 10)
            .expect("next")
            .commitment(c0.hash());
        let got = check_extends(
            &c0,
            empty,
            &c1,
            TrackLog {
                params: &p,
                statements: &s,
                resolutions: &[],
            },
        );
        assert_eq!(got.is_ok(), ok, "{prev_as_of}: {got:?}");
    }
}

/// Review C20-R2-1: the canonical sorts compute each row's key once; the
/// order (and the dropped duplicates) is the comparison sort's.
#[test]
fn the_cached_key_sorts_give_the_canonical_order() {
    let (s, r) = example::logs(example::AS_OF);
    let mut shuffled: Vec<Statement> = s.iter().rev().cloned().collect();
    shuffled.extend(s.iter().take(3).cloned());
    let mut want = shuffled.clone();
    want.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
    want.dedup();
    sort_statements(&mut shuffled);
    assert_eq!(shuffled, want);
    assert_eq!(shuffled, s);
    let mut rs: Vec<Resolution> = r.iter().rev().cloned().collect();
    rs.extend(r.iter().take(2).cloned());
    sort_resolutions(&mut rs);
    assert_eq!(rs, r);
}
