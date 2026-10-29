//! ADR-0036 amendment 3 (ROADMAP C7): resolutions from on-chain facts only,
//! commitments on chain, affiliates; plus the adversarial cases in THREATS.md
//! (TR-5, TR-R2-4, TR-R2-5, TR-R1-11, TR-S-14, TR-C7-*).

use super::*;
use crate::{
    check_extends, compute, sort_statements, Close, ExtendError, MetricRule, OperatorOf, Outcome,
    TrackBook,
};
use onyx_core::{
    Confidence, DataRights, MetricInfo, SignedPayload, StaticRegistry, SubjectRule, ValueKind,
};

const T0: u64 = 1_790_000_000;
const HOUR: u64 = 3_600;
const METRIC: &str = "test.amount";
const FEED_METRIC: &str = "test.feed";
const RECIPE_METRIC: &str = "test.recipe";
const BOOK: [u8; 20] = [0xb0; 20];

fn sid(s: &str) -> SourceId {
    SourceId(s.into())
}

fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let m = |value_kind| MetricInfo {
        value_kind,
        needs_label_set: false,
    };
    r.add_metric(METRIC, m(ValueKind::UInt))
        .and_then(|r| r.add_metric(FEED_METRIC, m(ValueKind::Int)))
        .and_then(|r| r.add_metric(RECIPE_METRIC, m(ValueKind::UInt)))
        .and_then(|r| r.add_aggregate("a.one"))
        .and_then(|r| r.add_aggregate("a.two"))
        .expect("registry");
    for s in ["vendor.a", "vendor.b", "signer.x", "signer.y", "feed.page"] {
        r.add_source(DataRights::public_chain(sid(s)))
            .expect("source");
    }
    r
}

fn chain_id() -> Hash32 {
    FactChain {
        chain_id: 31_337,
        book: BOOK,
        registry: [0x7e; 20],
    }
    .id()
}

fn params() -> TrackParams {
    TrackParams::new(vec![
        MetricRule {
            metric: MetricId(METRIC.into()),
            close: Close::AfterAsOf(0),
            tolerance: 0,
        },
        MetricRule {
            metric: MetricId(FEED_METRIC.into()),
            close: Close::AfterAsOf(0),
            tolerance: 0,
        },
        MetricRule {
            metric: MetricId(RECIPE_METRIC.into()),
            close: Close::AfterAsOf(0),
            tolerance: 0,
        },
    ])
    .with_chain(chain_id())
    .with_level3_resolvers_only()
}

/// The default levels (1 and 3) on the test chain: refused in chain mode.
fn params_with_level1() -> TrackParams {
    let mut p = params();
    p.resolver_levels = crate::DEFAULT_RESOLVER_LEVELS.to_vec();
    p
}

fn fact(metric: &str, subject: &str, at: u64) -> Fact {
    Fact {
        metric: MetricId(metric.into()),
        subject: Subject::Aggregate(subject.into()),
        as_of: at,
    }
}

fn st(source: &str, f: &Fact, v: u128, at: u64) -> Statement {
    Statement {
        source: sid(source),
        fact: f.clone(),
        value: Value::UInt(v),
        confidence: Confidence(900_000),
        committed_at: at,
    }
}

/// A report as the book holds it (the signature is not checked here: the
/// book checked it before it settled the fact).
fn report(source: &str, f: &Fact, value: Value, seq: u64) -> SignedReport {
    SignedReport {
        payload: SignedPayload {
            source: sid(source),
            key: keccak256(source.as_bytes()),
            metric: f.metric.clone(),
            subject: f.subject.clone(),
            value,
            observed_unix: f.as_of,
            sequence: seq,
        },
        signature: [seq as u8; 65],
    }
}

fn settled(r: SignedReport, accepted_at: u64) -> Settled {
    Settled {
        report_hash: r.hash(),
        value_hash: keccak256(&borsh::to_vec(&r.payload.value).expect("borsh")),
        report: Some(r),
        accepted_at,
    }
}

/// A chain as a client's own node shows it at one head.
#[derive(Clone)]
struct Mock {
    chain: Hash32,
    head: u64,
    sources: Vec<SourceId>,
    operators: Vec<(SourceId, Hash32)>,
    settled: Vec<(Hash32, Settled)>,
    registered: Vec<(MetricId, Registered, Hash32)>,
    recorded: Vec<(Hash32, Recorded)>,
    registry: bool,
    /// A node that shows answers not yet usable at the head (a level-3 fact
    /// inside its dispute window, an observation recorded after the head).
    unfinal: bool,
}

impl Mock {
    fn new(head: u64) -> Self {
        Mock {
            chain: chain_id(),
            head,
            sources: ["vendor.a", "vendor.b", "signer.x", "signer.y"]
                .map(sid)
                .to_vec(),
            operators: Vec::new(),
            settled: Vec::new(),
            registered: Vec::new(),
            recorded: Vec::new(),
            registry: true,
            unfinal: false,
        }
    }

    fn settle(&mut self, r: SignedReport, accepted_at: u64) {
        let p = &r.payload;
        let key = book_fact_key(&p.source, &p.metric, &p.subject, p.observed_unix);
        self.settled.push((key, settled(r, accepted_at)));
    }

    fn at(&self, head: u64) -> Self {
        Mock {
            head,
            ..self.clone()
        }
    }
}

impl ChainView for Mock {
    fn chain(&self) -> Hash32 {
        self.chain
    }
    fn has_registry(&self) -> bool {
        self.registry
    }
    fn head(&self) -> Anchor {
        Anchor {
            block: self.head / 2,
            block_hash: keccak256(&self.head.to_le_bytes()),
            timestamp: self.head,
        }
    }
    fn book_sources(&self) -> Result<Vec<SourceId>, String> {
        Ok(self.sources.clone())
    }
    fn operator_of(&self, source: &SourceId) -> Result<Hash32, String> {
        Ok(self
            .operators
            .iter()
            .find(|(s, _)| s == source)
            .map_or(Hash32::ZERO, |(_, o)| *o))
    }
    fn settled(&self, fact: &Hash32) -> Result<Option<Settled>, String> {
        // A fact is settleable once its window has passed at the head.
        Ok(self
            .settled
            .iter()
            .find(|(k, s)| {
                k == fact && (self.unfinal || s.accepted_at + DISPUTE_WINDOW_SECS <= self.head)
            })
            .map(|(_, s)| s.clone()))
    }
    fn registered(&self, metric: &MetricId) -> Result<(Registered, Hash32), String> {
        Ok(self
            .registered
            .iter()
            .find(|(m, _, _)| m == metric)
            .map_or((Registered::None, Hash32::ZERO), |(_, k, t)| (*k, *t)))
    }
    fn recorded(
        &self,
        metric: &MetricId,
        subject: &Subject,
        window_end: u64,
    ) -> Result<Option<Recorded>, String> {
        // The registry keys a fact by its registered template (dated: no window).
        let (kind, template) = self.registered(metric)?;
        let dated = kind == Registered::DatedFeed;
        let key = registry_fact_key(
            &metric_key(metric),
            &template,
            &subject_key(subject),
            window_end,
            dated,
        );
        Ok(self
            .recorded
            .iter()
            .find(|(k, r)| *k == key && (self.unfinal || r.recorded_time <= self.head))
            .map(|(_, r)| *r))
    }
}

fn tlog<'a>(p: &'a TrackParams, s: &'a [Statement], r: &'a [Resolution]) -> TrackLog<'a> {
    TrackLog {
        params: p,
        statements: s,
        resolutions: r,
    }
}

fn pv<'a>(as_of: u64, s: &'a [Statement], r: &'a [Resolution]) -> Option<Prev<'a>> {
    Some(Prev {
        as_of,
        statements: s,
        resolutions: r,
    })
}

fn sorted(mut s: Vec<Statement>) -> Vec<Statement> {
    sort_statements(&mut s);
    s
}

/// Derive at the view's head and compute: the coordinator's whole path.
fn book_at(
    p: &TrackParams,
    view: &Mock,
    s: &[Statement],
    prev: Option<Prev<'_>>,
) -> (Vec<Resolution>, TrackBook) {
    let r = derive_resolutions(p, &reg(), view, s, prev, &Preimages::default()).expect("derive");
    let b = compute(p, &reg(), s, &r, view.head).expect("compute");
    (r, b)
}

fn outcome(b: &TrackBook, source: &str) -> Outcome {
    b.entries()
        .iter()
        .find(|e| e.source == sid(source))
        .expect("entry")
        .outcome
}

/// q-TR-3: a statement is judged by the level-3 answer the book settles on,
/// read from the chain; the resolution's time is when the chain made it
/// usable (acceptance plus the dispute window).
#[test]
fn a_statement_is_resolved_by_the_books_settled_report() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.b", &f, 8, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    let (r, b) = book_at(&params(), &view, &s, None);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].level, ProofLevel::SourceProven);
    assert_eq!(r[0].resolver, sid("signer.x"));
    assert_eq!(r[0].public_from, T0 + HOUR + DISPUTE_WINDOW_SECS);
    assert_eq!(
        r[0].evidence,
        report("signer.x", &f, Value::UInt(7), 1).hash()
    );
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Agreed);
    assert_eq!(outcome(&b, "vendor.b"), Outcome::Disagreed);
    // Inside the window nothing is settleable: unresolved.
    let early = view.at(T0 + HOUR + 10);
    let (r, b) = book_at(&params(), &early, &s, None);
    assert!(r.is_empty());
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Unresolved);
}

/// TR-5: omitting the answer that shows a favoured source wrong, or adding a
/// forged one, fails every client's check against its own chain reads.
#[test]
fn an_omitted_or_forged_resolution_fails_the_client_check() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(8), 1), T0 + HOUR);
    let p = params();
    let (honest, _) = book_at(&p, &view, &s, None);
    let check = |r: &[Resolution]| {
        verify_resolutions(
            &reg(),
            &view,
            view.head,
            tlog(&p, &s, r),
            None,
            &Preimages::default(),
        )
    };
    assert_eq!(check(&honest), Ok(()));
    // Omitted: the miss would stay Unresolved.
    assert_eq!(check(&[]), Err(ChainError::Mismatch));
    // Forged: an agreeing answer public before the close turns the miss Void.
    let mut forged = honest.clone();
    forged[0].value = Value::UInt(7);
    forged[0].public_from = T0;
    assert_eq!(check(&forged), Err(ChainError::Mismatch));
    // Added: a second, contradicting answer from another source (Contested).
    let mut added = honest.clone();
    added.push(Resolution {
        resolver: sid("signer.y"),
        value: Value::UInt(7),
        ..honest[0].clone()
    });
    sort_resolutions(&mut added);
    assert_eq!(check(&added), Err(ChainError::Mismatch));
    // A stale read (another head than the commitment's as_of) proves nothing.
    assert_eq!(
        verify_resolutions(
            &reg(),
            &view,
            view.head - 1,
            tlog(&p, &s, &honest),
            None,
            &Preimages::default()
        ),
        Err(ChainError::NotTheHead {
            as_of: view.head - 1,
            head: view.head
        })
    );
}

/// A view that hands back another report than the one the book settles on,
/// or one of another fact, is refused rather than believed.
#[test]
fn a_report_that_is_not_the_settled_one_is_refused() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let key = book_fact_key(&sid("signer.x"), &f.metric, &f.subject, f.as_of);
    let mut view = Mock::new(T0 + 3 * HOUR);
    let mut swapped = settled(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    swapped.report = Some(report("signer.x", &f, Value::UInt(9), 2));
    view.settled.push((key, swapped));
    assert_eq!(
        derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::BadSettled("report is not the settled one"))
    );
    let mut view = Mock::new(T0 + 3 * HOUR);
    let other = fact(METRIC, "a.one", T0 + 1);
    view.settled.push((
        key,
        settled(report("signer.x", &other, Value::UInt(7), 1), T0 + HOUR),
    ));
    assert_eq!(
        derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::BadSettled("report is of another fact"))
    );
}

/// TR-C7-1: a source cannot halt the record by getting a malformed fact
/// settled on chain (a value of another kind, or a resolver Onyx does not
/// register): it is skipped, deterministically, by every client.
#[test]
fn a_malformed_onchain_fact_is_skipped_never_fatal() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::Bool(true), 1), T0 + HOUR);
    view.sources.push(sid("unregistered.z"));
    view.settle(report("unregistered.z", &f, Value::UInt(1), 1), T0 + HOUR);
    view.settle(report("signer.y", &f, Value::UInt(7), 1), T0 + HOUR);
    let (r, b) = book_at(&params(), &view, &s, None);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].resolver, sid("signer.y"));
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Agreed);
}

/// TR-C7-2: on-chain facts nobody stated add nothing (the derivation is
/// bounded by the statement log, so spam in the book does not grow the log).
#[test]
fn facts_nobody_stated_add_nothing() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    for d in 1..50 {
        let g = fact(METRIC, "a.two", T0 + d);
        view.settle(report("signer.x", &g, Value::UInt(1), d), T0 + HOUR);
    }
    let (r, b) = book_at(&params(), &view, &s, None);
    assert!(r.is_empty());
    assert_eq!(b.metrics()[0].resolved_facts, 0);
}

/// Level 1 cannot be read yet: params accepting it are refused in chain mode
/// (fail closed, ROADMAP C40), whatever the metrics, never left unresolved as
/// if no level-1 answer existed (TR-C7-7, TR-C7-R2-1).
#[test]
fn level1_is_refused_until_a_reader_exists() {
    let f = fact(RECIPE_METRIC, "a.one", T0);
    let g = fact(METRIC, "a.one", T0);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.registered
        .push((f.metric.clone(), Registered::Recipe, keccak256(b"recipe")));
    for s in [
        sorted(vec![st("vendor.a", &f, 7, T0)]),
        sorted(vec![st("vendor.a", &g, 7, T0)]),
        Vec::new(),
    ] {
        assert_eq!(
            derive_resolutions(
                &params_with_level1(),
                &reg(),
                &view,
                &s,
                None,
                &Preimages::default()
            ),
            Err(ChainError::Level1Unsupported)
        );
    }
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    assert_eq!(
        derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()),
        Ok(vec![])
    );
}

fn obs(p: &Level2Preimage) -> Preimages {
    Preimages {
        reports: Vec::new(),
        observations: vec![p.clone()],
    }
}

fn feed_template() -> FeedTemplate {
    FeedTemplate {
        feed: "test.feed.page".into(),
        metric: MetricId(FEED_METRIC.into()),
        fetcher: keccak256(b"fetcher"),
        url_template: "https://feed.example.org/value".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 0,
            tolerance: 0,
        },
        window_secs: HOUR,
        min_observers: 3,
        rights: DataRights::public_chain(sid("feed.page")),
        subject: SubjectRule::Fixed(Subject::Aggregate("a.one".into())),
    }
}

/// A level-2 view: the registry records `answer` for the feed fact at `as_of`.
fn level2_view(as_of: u64, outcome: ObservedOutcome) -> (Mock, Level2Preimage, Hash32) {
    let t = feed_template();
    let spec = t
        .instantiate(&Subject::Aggregate("a.one".into()), as_of - HOUR)
        .expect("spec");
    let pre = Level2Preimage {
        template: t.clone(),
        spec,
        round: keccak256(b"round"),
        outcome,
    };
    let key = registry_fact_key(
        &metric_key(&MetricId(FEED_METRIC.into())),
        &t.id(),
        &subject_key(&Subject::Aggregate("a.one".into())),
        as_of,
        false,
    );
    let mut view = Mock::new(as_of + 2 * HOUR);
    view.registered
        .push((MetricId(FEED_METRIC.into()), Registered::Feed, t.id()));
    view.recorded.push((
        key,
        Recorded {
            template: t.id(),
            window_end: as_of,
            spec: pre.spec.id(),
            result: pre.result_hash(),
            recorded_time: as_of + HOUR,
        },
    ));
    (view, pre, key)
}

fn feed_statement(at: u64, v: i128) -> Vec<Statement> {
    let f = fact(FEED_METRIC, "a.one", at);
    sorted(vec![Statement {
        source: sid("vendor.a"),
        fact: f,
        value: Value::Int(v),
        confidence: Confidence(900_000),
        committed_at: at,
    }])
}

/// Level 2 (when accepted): a recorded observation is opened by its preimage
/// and checked against the registered template and the recorded hashes; a
/// recorded one without its preimage is an omission (`Withheld`).
#[test]
fn a_recorded_observation_resolves_only_through_its_preimage() {
    let at = T0 - T0 % HOUR;
    let mut p = params();
    p.resolver_levels = vec![ProofLevel::ReObserved, ProofLevel::SourceProven];
    let s = feed_statement(at, 42);
    let (view, pre, key) = level2_view(at, ObservedOutcome::Value(Answer::Number(42)));
    let r = derive_resolutions(&p, &reg(), &view, &s, None, &obs(&pre)).expect("derive");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].level, ProofLevel::ReObserved);
    assert_eq!(r[0].resolver, sid("feed.page"));
    assert_eq!(r[0].value, Value::Int(42));
    assert_eq!(r[0].evidence, pre.result_hash());
    assert_eq!(r[0].public_from, at + HOUR);
    // Without the preimage: refused, never silently unresolved.
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::Withheld(key))
    );
    // A preimage with another outcome does not open the recorded result.
    let mut lie = pre.clone();
    lie.outcome = ObservedOutcome::Value(Answer::Number(41));
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &obs(&lie)),
        Err(ChainError::Withheld(key))
    );
    // Level 2 not accepted (the default): the observation is not read.
    assert_eq!(
        derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()),
        Ok(vec![])
    );
    // A round that settled no value resolves nothing.
    let (view, pre, _) = level2_view(at, ObservedOutcome::SourceUnavailable);
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &obs(&pre)),
        Ok(vec![])
    );
}

/// The preimage's template must be the registered one, and its spec that
/// template's: a coordinator cannot open a recorded result with a spec it
/// made up (another URL) even if it hashes to what was recorded.
#[test]
fn a_level2_preimage_must_be_the_registered_templates() {
    let at = T0 - T0 % HOUR;
    let mut p = params();
    p.resolver_levels = vec![ProofLevel::ReObserved, ProofLevel::SourceProven];
    let s = feed_statement(at, 42);
    let (mut view, pre, key) = level2_view(at, ObservedOutcome::Value(Answer::Number(42)));
    // The coordinator recorded a spec with another URL under the right template.
    let mut forged = pre.clone();
    forged.spec.url = "https://elsewhere.example.org/value".into();
    view.recorded[0].1.spec = forged.spec.id();
    view.recorded[0].1.result = forged.result_hash();
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &obs(&forged)),
        Err(ChainError::BadPreimage("spec is not the template's"))
    );
    // A template other than the registered one.
    let (view, pre, _) = level2_view(at, ObservedOutcome::Value(Answer::Number(42)));
    let mut other = pre.clone();
    other.template.selector = "/other".into();
    other.spec.selector = "/other".into();
    let mut v2 = view.clone();
    v2.recorded[0].1.spec = other.spec.id();
    v2.recorded[0].1.result = other.result_hash();
    assert_eq!(
        derive_resolutions(&p, &reg(), &v2, &s, None, &obs(&other)),
        Err(ChainError::Withheld(key))
    );
}

/// R-TR-8 holds for chain-derived resolutions: a fact first usable after the
/// previous commitment enters the next one, a later answer on a fact the
/// previous one resolved joins it without changing any judged entry (it does
/// not contest the committed outcome), and the next commitment extends the
/// previous one.
#[test]
fn chain_resolutions_extend_the_previous_commitment() {
    let f = fact(METRIC, "a.one", T0);
    let g = fact(METRIC, "a.two", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.a", &g, 5, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    let p = params();
    let (r1, b1) = book_at(&p, &view, &s, None);
    let c1 = b1.commitment(Hash32::ZERO);
    // Later: signer.y contradicts f (taken, but f's entry was judged first),
    // g gets its answer.
    let mut later = view.at(T0 + 9 * HOUR);
    later.settle(report("signer.y", &f, Value::UInt(1), 1), T0 + 4 * HOUR);
    later.settle(report("signer.x", &g, Value::UInt(5), 2), T0 + 4 * HOUR);
    let (r2, b2) = book_at(&p, &later, &s, pv(c1.as_of, &s, &r1));
    assert_eq!(r2.len(), 3);
    assert_eq!(b2.entries()[..1], b1.entries()[..1], "f's entry unchanged");
    assert_eq!(outcome(&b2, "vendor.a"), Outcome::Agreed);
    assert!(b2.entries().iter().all(|e| e.outcome == Outcome::Agreed));
    let c2 = b2.commitment(c1.hash());
    assert_eq!(
        check_extends(&c1, tlog(&p, &s, &r1), &c2, tlog(&p, &s, &r2)),
        Ok(())
    );
    assert_eq!(
        verify_resolutions(
            &reg(),
            &later,
            c2.as_of,
            tlog(&p, &s, &r2),
            Some((c1.as_of, tlog(&p, &s, &r1))),
            &Preimages::default()
        ),
        Ok(())
    );
}

/// TR-C7-3, review TR-C7-R1-2/3: an answer usable at the previous
/// commitment's `as_of` and missing from it is never taken later (so the
/// registry growing, or a coordinator adding it late, cannot halt or change
/// the chain); the omission itself is found by the chain walk, which checks
/// the previous commitment at its own head.
#[test]
fn an_omission_in_the_previous_commitment_is_caught_by_the_chain_walk() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(8), 1), T0 + HOUR);
    let p = params();
    // The coordinator's c1 leaves the settled answer out.
    let b1 = compute(&p, &reg(), &s, &[], view.head).expect("compute");
    let c1 = b1.commitment(Hash32::ZERO);
    let later = view.at(T0 + 9 * HOUR);
    let (r2, b2) = book_at(&p, &later, &s, pv(c1.as_of, &s, &[]));
    assert!(r2.is_empty(), "never taken later");
    let c2 = b2.commitment(c1.hash());
    // The latest alone checks out against c1's log ...
    assert_eq!(
        verify_resolutions(
            &reg(),
            &later,
            c2.as_of,
            tlog(&p, &s, &r2),
            Some((c1.as_of, tlog(&p, &s, &[]))),
            &Preimages::default()
        ),
        Ok(())
    );
    // ... the walk from the first entry does not.
    let r = reg();
    let none = Preimages::default();
    let steps = [
        ChainStep {
            reg: &r,
            view: &view,
            commitment: &c1,
            log: tlog(&p, &s, &[]),
            preimages: &none,
            at: c1.hash(),
        },
        ChainStep {
            reg: &r,
            view: &later,
            commitment: &c2,
            log: tlog(&p, &s, &r2),
            preimages: &none,
            at: c2.hash(),
        },
    ];
    let head = LogHead {
        count: 2,
        latest: c2.hash(),
        as_of: c2.as_of,
    };
    assert_eq!(verify_chain(&steps, &head), Err(ChainError::Mismatch));
    // The honest chain passes the walk.
    let (h1, hb1) = book_at(&p, &view, &s, None);
    let hc1 = hb1.commitment(Hash32::ZERO);
    let (h2, hb2) = book_at(&p, &later, &s, pv(hc1.as_of, &s, &h1));
    let hc2 = hb2.commitment(hc1.hash());
    let honest = [
        ChainStep {
            reg: &r,
            view: &view,
            commitment: &hc1,
            log: tlog(&p, &s, &h1),
            preimages: &none,
            at: hc1.hash(),
        },
        ChainStep {
            reg: &r,
            view: &later,
            commitment: &hc2,
            log: tlog(&p, &s, &h2),
            preimages: &none,
            at: hc2.hash(),
        },
    ];
    let hhead = LogHead {
        count: 2,
        latest: hc2.hash(),
        as_of: hc2.as_of,
    };
    assert_eq!(verify_chain(&honest, &hhead), Ok(()));
    // A step that is not the log's entry, or a missing step, is refused.
    let mut wrong = honest;
    wrong[0].at = keccak256(b"other");
    assert_eq!(verify_chain(&wrong, &hhead), Err(ChainError::NotTheLogs(0)));
    assert_eq!(
        verify_chain(&wrong[1..], &hhead),
        Err(ChainError::NotLatest)
    );
}

/// Review TR-C7-R1-1: a statement on a fact no earlier statement named, whose
/// answer was public before the previous commitment, is taken with that old
/// answer (Late or Void, never scored) and does not halt the chain.
#[test]
fn a_new_statement_on_an_old_fact_does_not_halt_the_chain() {
    let f = fact(METRIC, "a.one", T0);
    let g = fact(METRIC, "a.two", T0);
    let s1 = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &g, Value::UInt(5), 1), T0 + HOUR);
    let p = params();
    let (r1, b1) = book_at(&p, &view, &s1, None);
    let c1 = b1.commitment(Hash32::ZERO);
    let later = view.at(T0 + 9 * HOUR);
    // vendor.b states on g after c1 (Late: after g's close).
    let s2 = sorted(vec![
        st("vendor.a", &f, 7, T0),
        st("vendor.b", &g, 5, T0 + 4 * HOUR),
    ]);
    let r2 = derive_resolutions(
        &p,
        &reg(),
        &later,
        &s2,
        pv(c1.as_of, &s1, &r1),
        &Preimages::default(),
    )
    .expect("not halted");
    assert_eq!(r2.len(), 1);
    let b2 = compute(&p, &reg(), &s2, &r2, later.head).expect("compute");
    assert_eq!(outcome(&b2, "vendor.b"), Outcome::Late);
    let c2 = b2.commitment(c1.hash());
    assert_eq!(
        check_extends(&c1, tlog(&p, &s1, &r1), &c2, tlog(&p, &s2, &r2)),
        Ok(())
    );
}

/// Review TR-C7-R1-8: without a registry on the chain, accepting level 1 or
/// 2 is refused (a recipe metric would read as unregistered, fail open).
#[test]
fn levels_1_and_2_need_a_registry() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.registry = false;
    let mut level2 = params();
    level2.resolver_levels = vec![ProofLevel::ReObserved, ProofLevel::SourceProven];
    assert_eq!(
        derive_resolutions(&level2, &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::NoRegistry)
    );
    assert_eq!(
        derive_resolutions(
            &params_with_level1(),
            &reg(),
            &view,
            &s,
            None,
            &Preimages::default()
        ),
        Err(ChainError::Level1Unsupported)
    );
    assert!(derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()).is_ok());
}

/// Chain mode is pinned: params of another chain, or none, are refused, and a
/// later commitment cannot switch chains (TR-R3-3's rule for the intake).
#[test]
fn the_chain_is_pinned_in_the_params() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let view = Mock::new(T0 + 3 * HOUR);
    let mut none = params();
    none.chain = Hash32::ZERO;
    assert_eq!(
        derive_resolutions(&none, &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::NotThisChain)
    );
    let other = params().with_chain(
        FactChain {
            chain_id: 8453,
            book: BOOK,
            registry: [0x7e; 20],
        }
        .id(),
    );
    assert_eq!(
        derive_resolutions(&other, &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::NotThisChain)
    );
    assert!(!other.extends(&params()));
    assert!(!none.extends(&params()));
}

/// TR-R2-4: an affiliate (another source id, same declared operator) never
/// judges its operator's statements, neither to make them agree nor to
/// contest a miss; other sources are still judged by it.
#[test]
fn an_affiliate_never_resolves_its_operators_statements() {
    let f = fact(METRIC, "a.one", T0);
    let op = keccak256(b"operator of vendor.a");
    let p = params().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
    ]);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.b", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.operators = vec![(sid("vendor.a"), op), (sid("signer.x"), op)];
    // Only the affiliate answers, agreeing: self-resolved for vendor.a.
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    let (_, b) = book_at(&p, &view, &s, None);
    assert_eq!(outcome(&b, "vendor.a"), Outcome::SelfResolved);
    assert_eq!(outcome(&b, "vendor.b"), Outcome::Agreed);
    // An independent answer shows vendor.a wrong; the affiliate's contradicting
    // answer does not make the miss Contested.
    let mut view2 = Mock::new(T0 + 3 * HOUR);
    view2.operators = view.operators.clone();
    view2.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    view2.settle(report("signer.y", &f, Value::UInt(9), 1), T0 + HOUR);
    let (r, b) = book_at(&p, &view2, &s, None);
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Disagreed);
    assert_eq!(outcome(&b, "vendor.b"), Outcome::Contested);
    assert_eq!(
        verify_resolutions(
            &reg(),
            &view2,
            view2.head,
            tlog(&p, &s, &r),
            None,
            &Preimages::default()
        ),
        Ok(())
    );
}

/// TR-C7-4 (self-shield): a source's affiliate (or the source itself) cannot
/// keep a miss uncounted by answering first. Its answer leaves the source's
/// entry self-resolved only until an independent answer arrives, which then
/// judges it; entries already judged by the first answer stay as committed.
#[test]
fn an_affiliates_first_answer_does_not_shield_a_miss() {
    let f = fact(METRIC, "a.one", T0);
    let op = keccak256(b"operator of vendor.a");
    let p = params().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
    ]);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.b", &f, 8, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.operators = vec![(sid("vendor.a"), op), (sid("signer.x"), op)];
    // The affiliate rushes an answer agreeing with vendor.b, not vendor.a.
    view.settle(report("signer.x", &f, Value::UInt(8), 1), T0 + HOUR);
    let (r1, b1) = book_at(&p, &view, &s, None);
    assert_eq!(outcome(&b1, "vendor.a"), Outcome::SelfResolved);
    assert_eq!(outcome(&b1, "vendor.b"), Outcome::Agreed);
    let c1 = b1.commitment(Hash32::ZERO);
    // An independent answer later: vendor.a is judged (its miss counts),
    // vendor.b keeps its committed outcome even though the answers differ.
    let mut later = view.at(T0 + 9 * HOUR);
    later.settle(report("signer.y", &f, Value::UInt(9), 1), T0 + 4 * HOUR);
    let (r2, b2) = book_at(&p, &later, &s, pv(c1.as_of, &s, &r1));
    assert_eq!(outcome(&b2, "vendor.a"), Outcome::Disagreed);
    assert_eq!(outcome(&b2, "vendor.b"), Outcome::Agreed);
    let vb = |b: &TrackBook| {
        b.entries()
            .iter()
            .find(|e| e.source == sid("vendor.b"))
            .cloned()
    };
    assert_eq!(vb(&b1), vb(&b2), "vendor.b's committed entry is unchanged");
    let c2 = b2.commitment(c1.hash());
    assert_eq!(
        check_extends(&c1, tlog(&p, &s, &r1), &c2, tlog(&p, &s, &r2)),
        Ok(())
    );
    assert_eq!(
        verify_resolutions(
            &reg(),
            &later,
            c2.as_of,
            tlog(&p, &s, &r2),
            Some((c1.as_of, tlog(&p, &s, &r1))),
            &Preimages::default()
        ),
        Ok(())
    );
}

/// TR-R2-4: the params' operators must be the book's declarations for every
/// source of the log, so a coordinator can neither hide an affiliation nor
/// invent one; every stating source needs a key in the book (its operator was
/// fixed before it could state).
#[test]
fn the_params_operators_must_be_the_books() {
    let f = fact(METRIC, "a.one", T0);
    let op = keccak256(b"operator");
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    view.operators = vec![(sid("vendor.a"), op), (sid("signer.x"), op)];
    let declared = params().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
    ]);
    let r = derive_resolutions(&declared, &reg(), &view, &s, None, &Preimages::default())
        .expect("derive");
    assert_eq!(check_operators(&declared, &view, &s, &r), Ok(()));
    // Hidden: the params leave the affiliation out (signer.x would judge vendor.a).
    let hidden = params();
    assert_eq!(
        check_operators(&hidden, &view, &s, &r),
        Err(ChainError::OperatorMismatch(sid("signer.x")))
    );
    // Invented: grouping an independent source the book never grouped.
    let invented = declared.clone().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.y"),
            operator: op,
        },
    ]);
    assert_eq!(
        check_operators(&invented, &view, &s, &r),
        Err(ChainError::OperatorMismatch(sid("signer.y")))
    );
    // A stating source with no key in the book.
    view.sources.retain(|x| *x != sid("vendor.a"));
    assert_eq!(
        check_operators(&declared, &view, &s, &r),
        Err(ChainError::NotInBook(sid("vendor.a")))
    );
}

/// TR-R2-4 (append-only): an operator may be declared only for a source the
/// earlier logs never held; grouping a source already in the log would turn
/// committed outcomes into `SelfResolved`. Removing one is a params change.
#[test]
fn a_late_grouping_cannot_rewrite_committed_outcomes() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(8), 1), T0 + HOUR);
    let p1 = params();
    let (r1, b1) = book_at(&p1, &view, &s, None);
    assert_eq!(outcome(&b1, "vendor.a"), Outcome::Disagreed);
    let c1 = b1.commitment(Hash32::ZERO);
    let op = keccak256(b"op");
    let p2 = params().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
    ]);
    let later = view.at(T0 + 9 * HOUR);
    let r2 = derive_resolutions(
        &p2,
        &reg(),
        &later,
        &s,
        pv(c1.as_of, &s, &r1),
        &Preimages::default(),
    )
    .expect("derive");
    let b2 = compute(&p2, &reg(), &s, &r2, later.head).expect("compute");
    assert_eq!(
        outcome(&b2, "vendor.a"),
        Outcome::SelfResolved,
        "the miss would vanish"
    );
    let c2 = b2.commitment(c1.hash());
    assert_eq!(
        check_extends(&c1, tlog(&p1, &s, &r1), &c2, tlog(&p2, &s, &r2)),
        Err(ExtendError::OperatorRegrouped)
    );
    // A new source may be grouped before it appears.
    let p3 = params().with_operators(vec![OperatorOf {
        source: sid("signer.y"),
        operator: op,
    }]);
    let b3 = compute(&p3, &reg(), &s, &r2, later.head).expect("compute");
    let c3 = b3.commitment(c1.hash());
    assert_eq!(
        check_extends(&c1, tlog(&p1, &s, &r1), &c3, tlog(&p3, &s, &r2)),
        Ok(())
    );
    // Once declared, an operator never changes or disappears.
    assert!(!params().extends(&p3));
    // A zero operator or an unsorted list is malformed.
    let mut bad = p3.clone();
    bad.operators[0].operator = Hash32::ZERO;
    assert_eq!(
        bad.validate(&reg()),
        Err(TrackError::BadParams("operators"))
    );
}

/// TR-R2-5: the served commitment must be the latest of its on-chain log;
/// TR-R1-11 / TR-S-14: a known commitment must be in the log, so a fork
/// chained to it cannot be the latest.
#[test]
fn only_the_latest_onchain_commitment_is_accepted() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let view = Mock::new(T0 + 3 * HOUR);
    let (_, b1) = book_at(&params(), &view, &s, None);
    let c1 = b1.commitment(Hash32::ZERO);
    let (_, b2) = book_at(&params(), &view.at(T0 + 9 * HOUR), &s, None);
    let c2 = b2.commitment(c1.hash());
    let head = LogHead {
        count: 2,
        latest: c2.hash(),
        as_of: c2.as_of,
    };
    assert_eq!(check_latest(&c2, &head), Ok(()));
    // The older one is no longer the latest (rollback).
    assert_eq!(check_latest(&c1, &head), Err(ChainError::NotLatest));
    // A fork chained to c1 that never reached the log.
    let fork = b2.commitment(c1.hash());
    let mut forked = fork.clone();
    forked.entries_root = keccak256(b"rewritten");
    assert_eq!(check_latest(&forked, &head), Err(ChainError::NotLatest));
    assert_eq!(
        check_latest(
            &c1,
            &LogHead {
                count: 0,
                latest: Hash32::ZERO,
                as_of: 0
            }
        ),
        Err(ChainError::NotLatest)
    );
    assert_eq!(check_in_log(&c1, &c1.hash()), Ok(()));
    assert_eq!(check_in_log(&forked, &c2.hash()), Err(ChainError::NotInLog));
}

fn write_or_check(path: &str, text: &str) {
    let full = format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(std::path::Path::new(&full).parent().expect("parent"))
            .expect("mkdir");
        std::fs::write(&full, text).expect("write");
    }
    let on_disk = std::fs::read_to_string(&full)
        .unwrap_or_else(|_| panic!("{path} missing; run with ONYX_BLESS=1"));
    assert_eq!(on_disk, text, "{path} drifted from ADR-0036 amendment 3");
}

/// The bytes `postTrackCommitment` takes are 288 long with `prev` first and
/// `as_of` next; the book hashes them to `TrackCommitment::hash`
/// (fixtures/track/commitment-vectors.json, read by
/// contracts/test/OnyxSignedBookTrack.t.sol).
#[test]
fn commitment_vectors() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.b", &f, 8, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    let mut prev = Hash32::ZERO;
    let mut rows = Vec::new();
    for k in 0..3u64 {
        // Computed under the default levels (1 and 3) as when the vectors were
        // written; chain mode now refuses level 1 (TR-C7-R2-1), and the
        // vectors pin only the commitment encoding, so the resolutions come
        // from a level-3 derivation of the same chain.
        let at = view.at(T0 + (3 + k) * HOUR);
        let (r, _) = book_at(&params(), &at, &s, None);
        let b = compute(&params_with_level1(), &reg(), &s, &r, at.head).expect("compute");
        let c = b.commitment(prev);
        let bytes = commitment_bytes(&c);
        assert_eq!(bytes.len(), COMMITMENT_BYTES);
        assert_eq!(&bytes[..32], &c.prev.0);
        assert_eq!(bytes[32..40], c.as_of.to_le_bytes());
        let mut pre = crate::COMMITMENT_DOMAIN.to_vec();
        pre.extend_from_slice(&bytes);
        assert_eq!(keccak256(&pre), c.hash());
        rows.push(format!(
            "    {{\n      \"asOf\": {},\n      \"borsh\": \"0x{}\",\n      \"hash\": \"{}\"\n    }}",
            c.as_of,
            hex_lower(&bytes),
            c.hash()
        ));
        prev = c.hash();
    }
    let text = format!(
        "{{\n  \"commitments\": [\n{}\n  ],\n  \"count\": {},\n  \"note\": \"ADR-0036 amendment 3: Borsh TrackCommitment bytes and their hash (onyx_track::chain tests); OnyxSignedBook.postTrackCommitment hashes them alike\"\n}}\n",
        rows.join(",\n"),
        rows.len()
    );
    write_or_check("fixtures/track/commitment-vectors.json", &text);
}

fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// R-TR-5 with answers that arrive later (amendment 3 item 6): an entry judged
/// in a later commitment never takes `first` from one judged before, so the
/// earlier entry's committed flag stands.
#[test]
fn a_later_judged_entry_never_takes_first_from_an_earlier_one() {
    let f = fact(METRIC, "a.one", T0);
    let op = keccak256(b"operator of vendor.a");
    let p = params().with_operators(vec![
        OperatorOf {
            source: sid("vendor.a"),
            operator: op,
        },
        OperatorOf {
            source: sid("signer.x"),
            operator: op,
        },
    ]);
    // vendor.a stated first; only its affiliate answers at first.
    let s = sorted(vec![
        st("vendor.a", &f, 7, T0 - 10),
        st("vendor.b", &f, 7, T0),
    ]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    let (r1, b1) = book_at(&p, &view, &s, None);
    let first = |b: &TrackBook, src: &str| {
        b.entries()
            .iter()
            .find(|e| e.source == sid(src))
            .map(|e| (e.outcome, e.first))
    };
    assert_eq!(first(&b1, "vendor.b"), Some((Outcome::Agreed, true)));
    let mut later = view.at(T0 + 9 * HOUR);
    later.settle(report("signer.y", &f, Value::UInt(7), 1), T0 + 4 * HOUR);
    let (_, b2) = book_at(&p, &later, &s, pv(T0 + 3 * HOUR, &s, &r1));
    assert_eq!(first(&b2, "vendor.b"), Some((Outcome::Agreed, true)));
    assert_eq!(first(&b2, "vendor.a"), Some((Outcome::Agreed, true)));
}

/// TR-C7-6: anyone may relay a signed report through a contract, so a reader
/// may not find it in direct `postReport` calldata. The report then comes as a
/// preimage, bound by the settled report hash; without one the commitment is
/// refused (`Withheld`), and a preimage of another report opens nothing.
#[test]
fn a_relayed_report_resolves_through_its_preimage_only() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let r = report("signer.x", &f, Value::UInt(7), 1);
    let key = book_fact_key(&r.payload.source, &f.metric, &f.subject, f.as_of);
    let mut view = Mock::new(T0 + 3 * HOUR);
    let mut relayed = settled(r.clone(), T0 + HOUR);
    relayed.report = None;
    view.settled.push((key, relayed));
    let p = params();
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::Withheld(key))
    );
    let wrong = Preimages {
        reports: vec![report("signer.x", &f, Value::UInt(8), 1)],
        observations: Vec::new(),
    };
    assert_eq!(
        derive_resolutions(&p, &reg(), &view, &s, None, &wrong),
        Err(ChainError::Withheld(key))
    );
    let given = Preimages {
        reports: vec![r.clone()],
        observations: Vec::new(),
    };
    let got = derive_resolutions(&p, &reg(), &view, &s, None, &given).expect("derive");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].evidence, r.hash());
}

/// Review TR-C7-R1-6: answers inside the contest window of the first one
/// judge together (a fast wrong signer does not judge alone; a disagreement
/// is `Contested`), an entry waits until that window has passed (pending,
/// `Unresolved`), and an answer after it changes nothing.
#[test]
fn answers_inside_the_contest_window_judge_together() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 6 * HOUR);
    // signer.x settles first with a wrong value; signer.y half an hour later.
    view.settle(report("signer.x", &f, Value::UInt(9), 1), T0 + HOUR);
    view.settle(report("signer.y", &f, Value::UInt(7), 1), T0 + HOUR + 1_800);
    let (_, b) = book_at(&params(), &view, &s, None);
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Contested);
    // Before the first window has passed the entry waits.
    let (r, b) = book_at(&params(), &view.at(T0 + 2 * HOUR + 600), &s, None);
    assert_eq!(r.len(), 1);
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Unresolved);
    // An answer past the window does not join.
    let mut view = Mock::new(T0 + 6 * HOUR);
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    view.settle(report("signer.y", &f, Value::UInt(9), 1), T0 + 2 * HOUR + 1);
    let (_, b) = book_at(&params(), &view, &s, None);
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Agreed);
}

/// Review TR-C7-R1-5: the strongest level is chosen per entry after the
/// source's own and its affiliates' answers are left out, so its own stronger
/// answer cannot outrank the independent ones that judge it.
#[test]
fn a_sources_own_stronger_answer_does_not_outrank_independent_ones() {
    let f = fact(METRIC, "a.one", T0);
    let mut s = vec![st("vendor.a", &f, 7, T0), st("vendor.b", &f, 9, T0)];
    s = sorted(s);
    let at = T0 + 2 * HOUR;
    let own = Resolution {
        fact: f.clone(),
        value: Value::UInt(7),
        level: ProofLevel::ReObserved,
        resolver: sid("vendor.a"),
        evidence: keccak256(b"own level 2"),
        public_from: at,
    };
    let other = Resolution {
        fact: f.clone(),
        value: Value::UInt(9),
        level: ProofLevel::SourceProven,
        resolver: sid("signer.x"),
        evidence: keccak256(b"independent level 3"),
        public_from: at,
    };
    let mut r = vec![own, other];
    sort_resolutions(&mut r);
    let mut p = params();
    p.chain = Hash32::ZERO;
    p.resolver_levels = vec![ProofLevel::ReObserved, ProofLevel::SourceProven];
    let b = compute(&p, &reg(), &s, &r, at + 2 * HOUR).expect("compute");
    // vendor.a is judged (and missed) by signer.x's level-3 answer.
    assert_eq!(outcome(&b, "vendor.a"), Outcome::Disagreed);
    // vendor.b is judged by the stronger level-2 answer.
    assert_eq!(outcome(&b, "vendor.b"), Outcome::Disagreed);
}

/// Review TR-C7-R1-4: a declaration for a source without a book key (a
/// level-2 feed's source, which may be declared after it resolved) is not
/// used, so it can neither halt the chain nor regroup committed outcomes.
#[test]
fn a_keyless_sources_declaration_is_not_used() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.operators = vec![(sid("feed.page"), keccak256(b"late"))];
    let r = vec![Resolution {
        fact: f.clone(),
        value: Value::UInt(7),
        level: ProofLevel::ReObserved,
        resolver: sid("feed.page"),
        evidence: keccak256(b"obs"),
        public_from: T0 + HOUR,
    }];
    assert_eq!(check_operators(&params(), &view, &s, &r), Ok(()));
    let listed = params().with_operators(vec![OperatorOf {
        source: sid("feed.page"),
        operator: keccak256(b"late"),
    }]);
    assert_eq!(
        check_operators(&listed, &view, &s, &r),
        Err(ChainError::OperatorMismatch(sid("feed.page")))
    );
}

/// Review TR-C7-R1-2: a resolver the Onyx registry adds after a commitment
/// had its answer skipped; the later derivation skips it too (it was usable
/// on chain before the previous `as_of`), instead of halting.
#[test]
fn registry_growth_does_not_halt_the_chain() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + 3 * HOUR);
    view.sources.push(sid("late.source"));
    view.settle(report("late.source", &f, Value::UInt(7), 1), T0 + HOUR);
    let p = params();
    let (r1, b1) = book_at(&p, &view, &s, None);
    assert!(r1.is_empty(), "not registered yet: skipped");
    let c1 = b1.commitment(Hash32::ZERO);
    let mut grown = reg();
    grown
        .add_source(DataRights::public_chain(sid("late.source")))
        .expect("source");
    let r2 = derive_resolutions(
        &p,
        &grown,
        &view.at(T0 + 9 * HOUR),
        &s,
        pv(c1.as_of, &s, &r1),
        &Preimages::default(),
    )
    .expect("not halted");
    assert!(r2.is_empty());
}

/// Review TR-C7-R2-1: a metric the registry registers as a recipe after its
/// statements were committed (the level-5 use: early statements, the level-1
/// answer later) halted every later commitment of the whole record while the
/// params accepted level 1. Chain mode now refuses level 1 in the params
/// themselves (fail closed before the first commitment), so a later recipe
/// registration changes nothing.
#[test]
fn a_metric_registered_as_a_recipe_later_does_not_halt_the_chain() {
    let f = fact(RECIPE_METRIC, "a.one", T0);
    let g = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0), st("vendor.b", &g, 7, T0)]);
    let view = Mock::new(T0 + 3 * HOUR);
    let p = params();
    let (r1, b1) = book_at(&p, &view, &s, None);
    let c1 = b1.commitment(Hash32::ZERO);
    let mut later = view.at(T0 + 9 * HOUR);
    later
        .registered
        .push((f.metric.clone(), Registered::Recipe, keccak256(b"recipe")));
    assert!(
        derive_resolutions(
            &p,
            &reg(),
            &later,
            &s,
            pv(c1.as_of, &s, &r1),
            &Preimages::default()
        )
        .is_ok(),
        "a recipe registered later halted the chain"
    );
}

/// Review TR-C7-R2-3: an answer the view shows before it was usable at the
/// head (a level-3 fact still inside its dispute window, an observation
/// recorded after the head) is refused, never taken: its `public_from` would
/// lie after `as_of` and the derivation would differ from an honest node's.
#[test]
fn an_answer_not_yet_usable_at_the_head_is_refused() {
    let f = fact(METRIC, "a.one", T0);
    let s = sorted(vec![st("vendor.a", &f, 7, T0)]);
    let mut view = Mock::new(T0 + HOUR + 10);
    view.unfinal = true;
    view.settle(report("signer.x", &f, Value::UInt(7), 1), T0 + HOUR);
    assert_eq!(
        derive_resolutions(&params(), &reg(), &view, &s, None, &Preimages::default()),
        Err(ChainError::BadSettled("settled before its window ended"))
    );
    let at = T0 - T0 % HOUR;
    let mut p = params();
    p.resolver_levels = vec![ProofLevel::ReObserved, ProofLevel::SourceProven];
    let (view, pre, _) = level2_view(at, ObservedOutcome::Value(Answer::Number(42)));
    let mut early = view.at(at + HOUR - 1);
    early.unfinal = true;
    assert_eq!(
        derive_resolutions(
            &p,
            &reg(),
            &early,
            &feed_statement(at, 42),
            None,
            &obs(&pre)
        ),
        Err(ChainError::BadSettled("recorded observation"))
    );
}
