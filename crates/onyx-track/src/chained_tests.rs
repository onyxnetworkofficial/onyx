//! Chained (format-2) logs (ROADMAP C21, ADR-0036 amendment 6): the stream
//! check refuses what `check_extends` refuses, link by link, in one linear
//! pass; the two formats never check as each other.

use super::*;
use crate::example::{self, AS_OF, AS_OF_EARLY};
use crate::publication::{check_steps, log_check_cost, Publication};

/// A node's bound on one install (`onyx_api::track_serve::MAX_INSTALL_CHECK`).
const NODE_BOUND: u64 = 1 << 22;
use crate::{
    check_extends, compute, resolutions_hash, statements_hash, Close, Fact, MetricRule, OperatorOf,
    TrackCommitment, TrackLog, TrackParams,
};
use onyx_core::{Confidence, MetricId, ProofLevel, Subject, Value};

const METRIC: &str = "example.signed.reserve";
const T: u64 = 1_000_000;

fn params() -> TrackParams {
    TrackParams::new(vec![MetricRule {
        metric: MetricId(METRIC.into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }])
}

fn fact(day: u64) -> Fact {
    Fact {
        metric: MetricId(METRIC.into()),
        subject: Subject::Entity(format!(
            "entity:example-{}",
            (b'a' + (day % 6) as u8) as char
        )),
        as_of: T + day * 100,
    }
}

fn st(source: &str, day: u64, value: u128, committed_at: u64) -> Statement {
    Statement {
        source: SourceId(source.into()),
        fact: fact(day),
        value: Value::UInt(value),
        confidence: Confidence(800_000),
        committed_at,
    }
}

fn res(day: u64, level: ProofLevel, resolver: &str, public_from: u64, v: u128) -> Resolution {
    Resolution {
        fact: fact(day),
        value: Value::UInt(v),
        level,
        resolver: SourceId(resolver.into()),
        evidence: keccak256(&[day.to_be_bytes(), public_from.to_be_bytes()].concat()),
        public_from,
    }
}

/// A commitment over `s`/`r` with only the fields the log checks read set.
fn commitment(
    prev: Hash32,
    as_of: u64,
    params: &TrackParams,
    hashes: (Hash32, Hash32),
) -> TrackCommitment {
    TrackCommitment {
        prev,
        as_of,
        registry_root: Hash32([7; 32]),
        params_hash: params.hash(),
        statements_hash: hashes.0,
        resolutions_hash: hashes.1,
        entries_root: Hash32::ZERO,
        entry_count: 0,
        records_root: Hash32::ZERO,
        record_count: 0,
        metrics_root: Hash32::ZERO,
        metric_count: 0,
    }
}

fn sorted<T: Clone>(v: &[T], f: fn(&mut Vec<T>)) -> Vec<T> {
    let mut v = v.to_vec();
    f(&mut v);
    v
}

/// One link of a two-commitment scenario, both ways: format 1 (canonical
/// logs, `check_extends`) and format 2 (publication order, the stream).
struct Pair {
    prev_params: TrackParams,
    next_params: TrackParams,
    prev_as_of: u64,
    next_as_of: u64,
    old_s: Vec<Statement>,
    old_r: Vec<Resolution>,
    new_s: Vec<Statement>,
    new_r: Vec<Resolution>,
}

impl Pair {
    fn whole(&self) -> Result<(), ExtendError> {
        let ps = sorted(&self.old_s, sort_statements);
        let pr = sorted(&self.old_r, sort_resolutions);
        let all_s: Vec<Statement> = self.old_s.iter().chain(&self.new_s).cloned().collect();
        let all_r: Vec<Resolution> = self.old_r.iter().chain(&self.new_r).cloned().collect();
        let ns = sorted(&all_s, sort_statements);
        let nr = sorted(&all_r, sort_resolutions);
        let c1 = commitment(
            Hash32::ZERO,
            self.prev_as_of,
            &self.prev_params,
            (statements_hash(&ps), resolutions_hash(&pr)),
        );
        let c2 = commitment(
            c1.hash(),
            self.next_as_of,
            &self.next_params,
            (statements_hash(&ns), resolutions_hash(&nr)),
        );
        check_extends(
            &c1,
            TrackLog {
                params: &self.prev_params,
                statements: &ps,
                resolutions: &pr,
            },
            &c2,
            TrackLog {
                params: &self.next_params,
                statements: &ns,
                resolutions: &nr,
            },
        )
    }

    fn steps(&self) -> (Vec<Step>, Vec<Statement>, Vec<Resolution>) {
        let all_s: Vec<Statement> = self.old_s.iter().chain(&self.new_s).cloned().collect();
        let all_r: Vec<Resolution> = self.old_r.iter().chain(&self.new_r).cloned().collect();
        let c1 = commitment(
            Hash32::ZERO,
            self.prev_as_of,
            &self.prev_params,
            (
                chained_statements_hash(&self.old_s),
                chained_resolutions_hash(&self.old_r),
            ),
        );
        let c2 = commitment(
            c1.hash(),
            self.next_as_of,
            &self.next_params,
            (
                chained_statements_hash(&all_s),
                chained_resolutions_hash(&all_r),
            ),
        );
        let steps = vec![
            Step {
                commitment: c1,
                params: self.prev_params.clone(),
                statements: self.old_s.len() as u64,
                resolutions: self.old_r.len() as u64,
            },
            Step {
                commitment: c2,
                params: self.next_params.clone(),
                statements: all_s.len() as u64,
                resolutions: all_r.len() as u64,
            },
        ];
        (steps, all_s, all_r)
    }

    fn chained(&self) -> Result<(), PublicationError> {
        let (steps, s, r) = self.steps();
        check_chained(&steps, &s, &r, false).map(|_| ())
    }
}

/// A small deterministic generator (no floats, no clock).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn scenario(g: &mut Lcg) -> Pair {
    let prev_as_of = T + 1_000;
    let next_as_of = prev_as_of + 1 + g.below(500);
    let sources = ["example.a", "example.b", "example.c"];
    let levels = [ProofLevel::Recomputed, ProofLevel::SourceProven];
    // Earlier rows at or before the earlier as_of; new rows mostly after it,
    // some before (backdated statements, answers withheld), on facts the
    // earlier rows name or not, at either level.
    let (mut old_s, mut new_s, mut old_r, mut new_r) = (vec![], vec![], vec![], vec![]);
    for i in 0..(2 + g.below(5)) {
        let src = sources[g.below(3) as usize];
        old_s.push(st(
            src,
            g.below(6),
            u128::from(i % 3),
            prev_as_of - g.below(300),
        ));
    }
    for i in 0..(1 + g.below(4)) {
        let src = sources[g.below(3) as usize];
        let at = if g.below(12) == 0 {
            prev_as_of - g.below(100)
        } else {
            prev_as_of + 1 + g.below(300)
        };
        new_s.push(st(src, 6 + g.below(6), u128::from(i % 3) + 10, at));
    }
    for i in 0..(1 + g.below(3)) {
        let lvl = levels[g.below(2) as usize];
        old_r.push(res(
            g.below(6),
            lvl,
            "example.r",
            prev_as_of - g.below(300),
            u128::from(i % 2),
        ));
    }
    for i in 0..(1 + g.below(3)) {
        let lvl = levels[g.below(2) as usize];
        let at = if g.below(10) == 0 {
            prev_as_of - g.below(100)
        } else {
            prev_as_of + 1 + g.below(300)
        };
        new_r.push(res(
            g.below(12),
            lvl,
            "example.r",
            at,
            u128::from(i % 2) + 5,
        ));
    }
    let dedup = |v: &mut Vec<Statement>| {
        let mut seen = std::collections::BTreeSet::new();
        v.retain(|x| seen.insert(x.hash()));
    };
    dedup(&mut old_s);
    dedup(&mut new_s);
    new_s.retain(|x| !old_s.contains(x));
    let mut seen = std::collections::BTreeSet::new();
    old_r.retain(|x| seen.insert(x.hash()));
    new_r.retain(|x| seen.insert(x.hash()));
    // Publication order is arbitrary within a link: shuffle a little.
    if g.below(2) == 0 {
        old_s.reverse();
        new_r.reverse();
    }
    let mut prev_params = params();
    if g.below(4) == 0 {
        prev_params = prev_params.with_intake(Hash32([3; 32]));
    }
    let mut next_params = prev_params.clone();
    if g.below(5) == 0 {
        next_params = next_params.with_operators(vec![OperatorOf {
            source: SourceId(sources[g.below(3) as usize].into()),
            operator: Hash32([4; 32]),
        }]);
    }
    if g.below(10) == 0 {
        next_params.resolver_levels = vec![ProofLevel::SourceProven];
    }
    Pair {
        prev_params,
        next_params,
        prev_as_of,
        next_as_of,
        old_s,
        old_r,
        new_s,
        new_r,
    }
}

/// ROADMAP C21: on 3,000 generated two-commitment scenarios (backdated
/// statements, answers before and after the earlier `as_of` at both levels,
/// operators declared later, sealed and unsealed params, changed resolver
/// levels), the stream check accepts exactly what `check_extends` accepts.
#[test]
fn the_stream_check_refuses_what_check_extends_refuses() {
    let mut g = Lcg(0x5eed);
    let (mut ok, mut refused) = (0, 0);
    for i in 0..3_000 {
        let p = scenario(&mut g);
        let whole = p.whole();
        let chained = p.chained();
        assert_eq!(
            whole.is_ok(),
            chained.is_ok(),
            "scenario {i}: check_extends {whole:?}, stream {chained:?}"
        );
        // A single refusal names the same rule.
        if let (Err(a), Err(PublicationError::Extend(1, b))) = (&whole, &chained) {
            if !matches!(a, ExtendError::LogMismatch) {
                assert!(
                    a == b
                        || matches!(
                            (a, b),
                            (ExtendError::Backdated, ExtendError::ResolutionRewrites)
                                | (ExtendError::ResolutionRewrites, ExtendError::Backdated)
                        ),
                    "scenario {i}: {a:?} vs {b:?}"
                );
            }
        }
        if whole.is_ok() {
            ok += 1;
        } else {
            refused += 1;
        }
    }
    assert!(ok > 300 && refused > 300, "{ok} passed, {refused} refused");
}

/// The example chain as chained logs: it checks; its commitments differ from
/// format 1's in the two log hashes only; and neither format's commitments
/// check under the other's rule (domain-separated hashes).
#[test]
fn chained_logs_carry_the_example_and_never_check_as_whole_logs() {
    let (p, reg) = (example::params(), example::registry());
    let mut chained = Publication::chained();
    let mut whole = Publication::new();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        chained.push(&p, &s, &r, &book, None).expect("push chained");
        whole.push(&p, &s, &r, &book, None).expect("push whole");
    }
    chained.check().expect("chained checks");
    let (a, b) = (&chained.steps[0].commitment, &whole.steps[0].commitment);
    assert_ne!(a.statements_hash, b.statements_hash);
    assert_ne!(a.resolutions_hash, b.resolutions_hash);
    let mut c = a.clone();
    c.statements_hash = b.statements_hash;
    c.resolutions_hash = b.resolutions_hash;
    assert_eq!(&c, b, "only the two log hashes differ");
    assert_eq!(
        check_steps(&chained.steps, &chained.statements, &chained.resolutions).err(),
        Some(PublicationError::Log(0))
    );
    assert_eq!(
        check_chained(&whole.steps, &whole.statements, &whole.resolutions, false).err(),
        Some(PublicationError::Log(0))
    );
    // The canonical log comes back for the rerun.
    let (s, r) = check_chained(
        &chained.steps,
        &chained.statements,
        &chained.resolutions,
        true,
    )
    .expect("checks");
    assert_eq!((s, r), example::logs(AS_OF));
}

/// A row twice, a changed row, rows moved across a link, a row of the wrong
/// kind, or a stream cut short: refused.
#[test]
fn a_tampered_stream_is_refused() {
    let (p, reg) = (example::params(), example::registry());
    let mut pubn = Publication::chained();
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        pubn.push(&p, &s, &r, &book, None).expect("push");
    }
    let (steps, s, r) = (&pubn.steps, &pubn.statements, &pubn.resolutions);
    let n0 = steps[0].statements as usize;
    // Changed row in the first link.
    let mut x = s.clone();
    x[0].committed_at += 1;
    assert_eq!(
        check_chained(steps, &x, r, false).err(),
        Some(PublicationError::Log(0))
    );
    // A row of the second link moved into the first (same set, other split).
    let mut y = s.clone();
    y.swap(n0 - 1, n0);
    assert!(check_chained(steps, &y, r, false).is_err());
    // A row twice (replacing another).
    let mut z = s.clone();
    z[n0] = z[0].clone();
    assert_eq!(
        check_chained(steps, &z, r, false).err(),
        Some(PublicationError::Log(1))
    );
    // Wrong kind at a position, and a cut stream.
    let mut c = ChainedCheck::new(steps.clone(), false).expect("start");
    assert_eq!(c.next_kind(), Some(Kind::Statement));
    assert_eq!(c.resolution(&r[0]).err(), Some(PublicationError::Counts(0)));
    c.statement(&s[0]).expect("first row");
    assert!(c.finish().is_err());
    // Params that do not hash to the link, and a link not chained.
    let mut bad = steps.clone();
    bad[1].params = bad[1].params.clone().with_intake(Hash32([1; 32]));
    assert_eq!(
        ChainedCheck::new(bad, false).err(),
        Some(PublicationError::Params(1))
    );
    let mut unchained = steps.clone();
    unchained[1].commitment.prev = Hash32([1; 32]);
    assert_eq!(
        ChainedCheck::new(unchained, false).err(),
        Some(PublicationError::Extend(1, ExtendError::NotChained))
    );
}

/// TR-S-14 on chained logs: a stronger answer added later to rewrite a
/// committed outcome, a statement backdated into a committed period, an
/// operator declared for a source already held: refused at the row (or the
/// link) that does it.
#[test]
fn chained_logs_keep_the_extension_rules() {
    let base = Pair {
        prev_params: params(),
        next_params: params(),
        prev_as_of: T + 1_000,
        next_as_of: T + 2_000,
        old_s: vec![st("example.a", 1, 5, T + 50)],
        old_r: vec![res(1, ProofLevel::SourceProven, "example.r", T + 200, 5)],
        new_s: vec![st("example.a", 2, 5, T + 1_500)],
        new_r: vec![res(2, ProofLevel::SourceProven, "example.r", T + 1_600, 5)],
    };
    assert_eq!(base.chained(), Ok(()));
    assert_eq!(base.whole(), Ok(()));
    let stronger = Pair {
        new_r: vec![res(1, ProofLevel::Recomputed, "example.r", T + 1_600, 6)],
        ..clone(&base)
    };
    assert_eq!(
        stronger.chained(),
        Err(PublicationError::Extend(1, ExtendError::ResolutionRewrites))
    );
    let backdated = Pair {
        new_s: vec![st("example.a", 3, 5, T + 900)],
        ..clone(&base)
    };
    assert_eq!(
        backdated.chained(),
        Err(PublicationError::Extend(1, ExtendError::Backdated))
    );
    let regrouped = Pair {
        next_params: params().with_operators(vec![OperatorOf {
            source: SourceId("example.a".into()),
            operator: Hash32([4; 32]),
        }]),
        ..clone(&base)
    };
    assert_eq!(
        regrouped.chained(),
        Err(PublicationError::Extend(1, ExtendError::OperatorRegrouped))
    );
    let dropped_rule = Pair {
        next_params: TrackParams::new(vec![]),
        ..clone(&base)
    };
    assert_eq!(
        dropped_rule.chained(),
        Err(PublicationError::Extend(1, ExtendError::ParamsChanged))
    );
}

fn clone(p: &Pair) -> Pair {
    Pair {
        prev_params: p.prev_params.clone(),
        next_params: p.next_params.clone(),
        prev_as_of: p.prev_as_of,
        next_as_of: p.next_as_of,
        old_s: p.old_s.clone(),
        old_r: p.old_r.clone(),
        new_s: p.new_s.clone(),
        new_r: p.new_r.clone(),
    }
}

/// TR-C20-R2-2's TODO: a long chain over a large log checks in one linear
/// pass. 2,000 links over 20,000 statements and 20,000 answers: format 1
/// would hash 2,000 whole logs (80 million row hashes, far past a node's
/// bound), format 2 takes each row once.
#[test]
fn a_long_chain_checks_in_one_linear_pass() {
    let p = params();
    let (links, per) = (2_000u64, 10u64);
    let mut steps: Vec<Step> = Vec::new();
    let (mut s, mut r) = (Vec::new(), Vec::new());
    let mut prev = Hash32::ZERO;
    // Running heads (hashing every prefix again would make the setup itself
    // links times rows).
    let (mut hs, mut hr) = (Head::default(), Head::default());
    for i in 0..links {
        let as_of = T + 10_000 + i * 10;
        for k in 0..per {
            let day = 1_000 + i * per + k;
            let x = st("example.a", day, 1, as_of - 1);
            let y = res(day, ProofLevel::SourceProven, "example.r", as_of - 1, 1);
            hs.absorb(STATEMENT_ROW_DOMAIN, &x);
            hr.absorb(RESOLUTION_ROW_DOMAIN, &y);
            s.push(x);
            r.push(y);
        }
        let c = commitment(
            prev,
            as_of,
            &p,
            (
                hs.hash(STATEMENTS_V2_DOMAIN),
                hr.hash(RESOLUTIONS_V2_DOMAIN),
            ),
        );
        prev = c.hash();
        steps.push(Step {
            commitment: c,
            params: p.clone(),
            statements: s.len() as u64,
            resolutions: r.len() as u64,
        });
    }
    let rows = (s.len() + r.len()) as u64;
    assert!(log_check_cost(steps.len(), rows) > 10 * NODE_BOUND);
    assert_eq!(
        crate::publication::chained_check_cost(steps.len(), rows),
        rows + links
    );
    check_chained(&steps, &s, &r, false).expect("checks");
    // The same stream fed page by page from any cut gives the same result.
    let mut c = ChainedCheck::new(steps.clone(), false).expect("start");
    for pos in (0..rows).step_by(4_096) {
        for row in rows_from(&steps, &s, &r, pos, 4_096) {
            c.row(&row).expect("row");
        }
    }
    assert!(c.done());
}

/// Review C21-R2-3: chains of 3 to 5 links (rows added at every link, some
/// backdated or answered early, operators declared later for statement
/// sources and for the resolver, sealed and unsealed params), checked from
/// every link as a client asking `since` does: the stream check accepts and
/// refuses exactly what format 1's `check_steps` does on the same rows, on
/// every run.
#[test]
fn every_run_of_a_long_chain_checks_as_format_one_does() {
    let mut g = Lcg(0xc212);
    let sources = ["example.a", "example.b", "example.c", "example.r"];
    let levels = [ProofLevel::Recomputed, ProofLevel::SourceProven];
    let (mut ok, mut refused) = (0u32, 0u32);
    for case in 0..1_000 {
        let links = 3 + g.below(3);
        let mut params_now = params();
        if g.below(4) == 0 {
            params_now = params_now.with_intake(Hash32([3; 32]));
        }
        let (mut s, mut r): (Vec<Statement>, Vec<Resolution>) = (vec![], vec![]);
        let mut seen_s = std::collections::BTreeSet::new();
        let mut seen_r = std::collections::BTreeSet::new();
        let (mut one, mut two): (Vec<Step>, Vec<Step>) = (vec![], vec![]);
        let (mut prev1, mut prev2) = (Hash32::ZERO, Hash32::ZERO);
        let mut as_of = T + 1_000;
        for link in 0..links {
            let before = as_of;
            as_of += 1 + g.below(400);
            for i in 0..(1 + g.below(4)) {
                let at = if link > 0 && g.below(10) == 0 {
                    before - g.below(100)
                } else {
                    before + 1 + g.below(as_of - before)
                };
                let x = st(
                    sources[g.below(3) as usize],
                    g.below(10),
                    u128::from(i % 3) + 10 * u128::from(link),
                    at,
                );
                if seen_s.insert(x.hash()) {
                    s.push(x);
                }
            }
            for i in 0..(g.below(3)) {
                let at = if link > 0 && g.below(8) == 0 {
                    before - g.below(100)
                } else {
                    before + 1 + g.below(as_of - before)
                };
                let y = res(
                    g.below(12),
                    levels[g.below(2) as usize],
                    "example.r",
                    at,
                    u128::from(i % 2) + 5 * u128::from(link),
                );
                if seen_r.insert(y.hash()) {
                    r.push(y);
                }
            }
            if link > 0 && g.below(4) == 0 {
                let mut ops = params_now.operators.clone();
                let src = SourceId(sources[g.below(4) as usize].into());
                if params_now.operator_of(&src).is_none() {
                    ops.push(OperatorOf {
                        source: src,
                        operator: Hash32([4; 32]),
                    });
                    params_now = params_now.with_operators(ops);
                }
            }
            let c1 = commitment(
                prev1,
                as_of,
                &params_now,
                (
                    statements_hash(&sorted(&s, sort_statements)),
                    resolutions_hash(&sorted(&r, sort_resolutions)),
                ),
            );
            let c2 = commitment(
                prev2,
                as_of,
                &params_now,
                (chained_statements_hash(&s), chained_resolutions_hash(&r)),
            );
            (prev1, prev2) = (c1.hash(), c2.hash());
            let (n, m) = (s.len() as u64, r.len() as u64);
            one.push(Step {
                commitment: c1,
                params: params_now.clone(),
                statements: n,
                resolutions: m,
            });
            two.push(Step {
                commitment: c2,
                params: params_now.clone(),
                statements: n,
                resolutions: m,
            });
        }
        for from in 0..one.len() {
            let a = check_steps(&one[from..], &s, &r).map(|_| ());
            let b = check_chained(&two[from..], &s, &r, false).map(|_| ());
            assert_eq!(
                a.is_ok(),
                b.is_ok(),
                "case {case} from {from}: check_steps {a:?}, stream {b:?}"
            );
            // A refusal names the same link.
            if let (Err(PublicationError::Extend(i, _)), Err(PublicationError::Extend(j, _))) =
                (&a, &b)
            {
                assert_eq!(i, j, "case {case} from {from}: {a:?} vs {b:?}");
            }
            if a.is_ok() {
                ok += 1;
            } else {
                refused += 1;
            }
        }
    }
    assert!(
        ok > 300 && refused > 300,
        "{ok} runs passed, {refused} refused"
    );
}
