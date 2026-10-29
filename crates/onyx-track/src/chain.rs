//! Resolutions read from the chain only, and commitments on it (ADR-0036
//! amendment 3, ROADMAP C7, PLAN q-TR-3; THREATS TR-5, TR-R2-4, TR-R2-5,
//! TR-R1-11, TR-S-14).
//!
//! With `TrackParams.chain` set, a commitment's resolution list is never an
//! input. [`derive_resolutions`] builds it from what one chain shows at one
//! finalized head ([`ChainView`]), for the facts the statement log names:
//! - level 3: a report `OnyxSignedBook.settleable` returns for the fact
//!   (the report itself from its `postReport` calldata or, when relayed
//!   through a contract, from [`Preimages::reports`], checked here against the
//!   settled report and value hashes);
//! - level 2: an `OnyxRegistry` observation recorded for the fact, opened by
//!   its preimage (template, spec, round, outcome), checked here against the
//!   registered template and the recorded spec and result hashes. Preimages
//!   are the only caller input, and an on-chain answer without one is refused
//!   (`Withheld`), never skipped;
//! - level 1: params accepting it are refused (fail closed) until a level-1
//!   reader exists (ROADMAP C40; TR-C7-R2-1).
//!
//! Every client reruns [`verify_resolutions`] against its own chain reads, so
//! omitting or forging a resolution fails the check (TR-5). The same view's
//! declared operators must equal the params' ([`check_operators`], TR-R2-4).
//!
//! Commitments go to an append-only log in `OnyxSignedBook`
//! (`postTrackCommitment`): a client reads the latest at a finalized block and
//! refuses any other ([`check_latest`]), and a known commitment must be in
//! the log ([`check_in_log`]), so a stale commitment or a fork after a known
//! one is refused (TR-R2-5, TR-R1-11, TR-S-14).
//!
//! Nothing here returns a level or a market policy (ADR-0036 item 12).

use crate::intake::Anchor;
use crate::{
    check_fact, check_source, hash_with, sort_resolutions, Fact, Resolution, Statement,
    TrackCommitment, TrackError, TrackLog, TrackParams,
};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::onchain::{metric_key, registry_fact_key, subject_key, MetricKind};
use onyx_core::{
    keccak256, Answer, FeedTemplate, Hash32, MetricId, ObservationKind, ObservationSpec,
    ProofLevel, Registry, SignedReport, SourceId, Subject, Value,
};

/// Domain tag of [`FactChain::id`].
pub const CHAIN_DOMAIN: &[u8] = b"onyx-track-chain-v1";
/// `OnyxSignedBook.DISPUTE_WINDOW_SECS`: a level-3 fact is usable this long
/// after its acceptance.
pub const DISPUTE_WINDOW_SECS: u64 = 3_600;
/// Borsh length of a [`TrackCommitment`]
/// (`OnyxSignedBook.TRACK_COMMITMENT_BYTES`).
pub const COMMITMENT_BYTES: usize = 288;
/// Domain of `ObservationSettlement::result_hash`
/// (`onyx_coordinator::observe::result_hash_of`).
pub const OBSERVE_RESULT_DOMAIN: &[u8] = b"onyx-observe-result-v1";

/// The chain resolutions are read from: its id and the two contracts
/// (`registry` zero: none, so no level-1 or level-2 fact exists on it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct FactChain {
    /// EVM chain id.
    pub chain_id: u64,
    /// `OnyxSignedBook` (level-3 books, operators, commitment logs).
    pub book: [u8; 20],
    /// `OnyxRegistry` (level-2 observations), or zero.
    pub registry: [u8; 20],
}

impl FactChain {
    /// `keccak256("onyx-track-chain-v1" ‖ borsh(self))`: `TrackParams.chain`.
    pub fn id(&self) -> Hash32 {
        hash_with(CHAIN_DOMAIN, self)
    }
}

/// How `OnyxRegistry.metrics` registers a metric (`Kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    /// Not registered (or no registry): no level-1 or level-2 fact.
    None,
    /// Level 1 (a recipe).
    Recipe,
    /// Level 2, one fact per window end.
    Feed,
    /// Level 2, one fact per dated subject.
    DatedFeed,
}

/// A level-3 fact the book settles on at the head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// The accepted report (payload and signature) when the reader could take
    /// it from a direct `postReport` call; `None` when it was relayed through
    /// a contract (anyone may relay a signed report), and then a
    /// [`Preimages::reports`] entry must give it (TR-C7-6).
    pub report: Option<SignedReport>,
    /// `settleable`'s report hash.
    pub report_hash: Hash32,
    /// `settleable`'s value hash (`keccak256(borsh(value))`).
    pub value_hash: Hash32,
    /// `factView`'s `acceptedAt` (block time).
    pub accepted_at: u64,
}

/// A level-2 observation `OnyxRegistry` holds at the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// The template it was recorded under.
    pub template: Hash32,
    /// Its window end (the fact's `as_of`).
    pub window_end: u64,
    /// The recorded spec id.
    pub spec: Hash32,
    /// The recorded result hash.
    pub result: Hash32,
    /// Timestamp of the block that recorded it.
    pub recorded_time: u64,
}

/// One chain's state at one finalized head, as a client's own node shows it.
/// Implementations: `onyx_coordinator::track_chain::CastFacts` (`cast`), and
/// test doubles. Every read is at [`ChainView::head`].
pub trait ChainView {
    /// [`FactChain::id`] of the chain read.
    fn chain(&self) -> Hash32;
    /// The finalized head every read is at.
    fn head(&self) -> Anchor;
    /// True when the chain has an `OnyxRegistry` (else no level-1 or level-2
    /// fact can be read and accepting those levels is refused).
    fn has_registry(&self) -> bool;
    /// Every source with a key ever registered in the book (`KeyRegistered`).
    fn book_sources(&self) -> Result<Vec<SourceId>, String>;
    /// `operatorOf(keccak256(source))` (zero: none declared).
    fn operator_of(&self, source: &SourceId) -> Result<Hash32, String>;
    /// The report the book settles book fact `fact` on, if `settleable`.
    fn settled(&self, fact: &Hash32) -> Result<Option<Settled>, String>;
    /// `metrics(metric_key(metric))`: how the registry computes `metric`.
    fn registered(&self, metric: &MetricId) -> Result<(Registered, Hash32), String>;
    /// `OnyxRegistry.observationOf(metric key, subject key, window end)`, if
    /// a result is recorded.
    fn recorded(
        &self,
        metric: &MetricId,
        subject: &Subject,
        window_end: u64,
    ) -> Result<Option<Recorded>, String>;
}

/// Borsh mirror of `onyx_coordinator::observe::ObservationOutcome`, the last
/// field of a level-2 result hash's preimage.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ObservedOutcome {
    /// A strict majority agreed on this answer.
    Value(Answer),
    /// A strict majority found the source unavailable (no value).
    SourceUnavailable,
    /// No majority (no value).
    NoResult,
}

/// The only caller input of a derivation: the bytes behind hashes the chain
/// holds. Each is checked against its on-chain hash, so a preimage can only
/// open what the chain recorded; a recorded fact without one is `Withheld`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preimages {
    /// Settled level-3 reports the reader could not take from calldata.
    pub reports: Vec<SignedReport>,
    /// Recorded level-2 observations.
    pub observations: Vec<Level2Preimage>,
}

/// What opens a recorded level-2 observation: its registered template, the
/// spec instantiated from it, the round id and the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level2Preimage {
    /// The feed template (`OnyxRegistry.metrics(metric).template` is its id).
    pub template: FeedTemplate,
    /// The round's spec (`template.instantiate(subject, window_start)`).
    pub spec: ObservationSpec,
    /// The round id.
    pub round: Hash32,
    /// The settled outcome.
    pub outcome: ObservedOutcome,
}

impl Level2Preimage {
    /// `keccak256("onyx-observe-result-v1" ‖ borsh(spec id, round, outcome))`.
    pub fn result_hash(&self) -> Hash32 {
        let mut b = OBSERVE_RESULT_DOMAIN.to_vec();
        b.extend_from_slice(
            &borsh::to_vec(&(self.spec.id(), self.round, &self.outcome)).unwrap_or_default(),
        );
        keccak256(&b)
    }
}

/// Why resolutions or a commitment were refused against the chain.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// The params or the input refused.
    #[error(transparent)]
    Track(#[from] TrackError),
    /// `params.chain` is zero or not the chain the view reads.
    #[error("params are not resolved from this chain")]
    NotThisChain,
    /// The commitment's `as_of` is not the view's head time.
    #[error("as_of {as_of} is not the head's time {head}")]
    NotTheHead {
        /// The commitment's `as_of`.
        as_of: u64,
        /// The head's timestamp.
        head: u64,
    },
    /// A chain read failed.
    #[error("chain read: {0}")]
    Read(String),
    /// The params accept level 1 and no level-1 reader exists yet (fail
    /// closed, ROADMAP C40). Refused for the params themselves, never per
    /// metric: a metric registered as a recipe after its statements were
    /// committed would otherwise halt every later commitment (TR-C7-R2-1).
    #[error("level-1 resolutions cannot be read from the chain yet")]
    Level1Unsupported,
    /// The chain settled or recorded an answer for a stated fact (book or
    /// registry fact key) and no preimage opens it (an omission, TR-5).
    #[error("on-chain fact {0} without its preimage")]
    Withheld(Hash32),
    /// A preimage that does not open its recorded observation.
    #[error("level-2 preimage: {0}")]
    BadPreimage(&'static str),
    /// A settled report that does not match what the book settles on, or a
    /// view that contradicts itself.
    #[error("level-3 book: {0}")]
    BadSettled(&'static str),
    /// Level 2 is accepted on a chain with no registry (fail closed).
    #[error("level 2 accepted without a registry on the chain")]
    NoRegistry,
    /// A commitment of the chain walk does not extend the one before.
    #[error("commitment {0} does not extend the one before: {1}")]
    NotExtended(usize, crate::ExtendError),
    /// A commitment of the chain walk is not chained or not its log's entry.
    #[error("commitment {0} is not the log's entry")]
    NotTheLogs(usize),
    /// A stating source without a key in the book (its operator could still
    /// be declared after it stated).
    #[error("stating source {0:?} has no key in the book")]
    NotInBook(SourceId),
    /// The params' operator of a source differs from the book's declaration.
    #[error("operator of {0:?} differs from the book's")]
    OperatorMismatch(SourceId),
    /// The log's resolutions are not the ones the chain gives (an omission,
    /// an addition or a change).
    #[error("resolutions differ from the chain's")]
    Mismatch,
    /// The commitment is not the latest of its on-chain log.
    #[error("not the latest commitment on chain")]
    NotLatest,
    /// The known commitment is not in the on-chain log at its index.
    #[error("the known commitment is not in the on-chain log")]
    NotInLog,
}

fn read<T>(r: Result<T, String>) -> Result<T, ChainError> {
    r.map_err(ChainError::Read)
}

/// `onyx_coordinator::signed::signed_fact_key`: the book's key of
/// (source, metric, subject, observed time),
/// `keccak256("onyx-signed-fact-v1" ‖ borsh(source, metric, subject, observed))`.
pub fn book_fact_key(source: &SourceId, metric: &MetricId, subject: &Subject, at: u64) -> Hash32 {
    let mut b = b"onyx-signed-fact-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(&(source, metric, subject, at)).unwrap_or_default());
    keccak256(&b)
}

/// The value a settled level-2 answer stands for (as `accept_claim` matches a
/// claim): bytes for an exact feed, `Int` at scale 0, else `Fixed` at the
/// feed's scale. `None` for anything else.
pub fn level2_value(kind: &ObservationKind, answer: &Answer) -> Option<Value> {
    onyx_core::ladder::observed_value(kind, answer)
}

/// A derived resolution counts only if `compute` would take it (R-TR-1); one
/// it would refuse is skipped, so a malformed on-chain fact (a resolver Onyx
/// does not register, a value of another kind) can never halt the record.
fn usable(params: &TrackParams, reg: &dyn Registry, r: &Resolution) -> bool {
    r.evidence != Hash32::ZERO
        && check_source(reg, &r.resolver).is_ok()
        && check_fact(params, reg, &r.fact, &r.value).is_ok()
}

fn level3(
    view: &dyn ChainView,
    sources: &[SourceId],
    fact: &Fact,
    head: u64,
    reports: &[SignedReport],
    out: &mut Vec<Resolution>,
) -> Result<(), ChainError> {
    for s in sources {
        let key = book_fact_key(s, &fact.metric, &fact.subject, fact.as_of);
        let Some(st) = read(view.settled(&key))? else {
            continue;
        };
        let report = match st.report {
            Some(r) => r,
            None => reports
                .iter()
                .find(|r| r.hash() == st.report_hash)
                .cloned()
                .ok_or(ChainError::Withheld(key))?,
        };
        let p = &report.payload;
        if report.hash() != st.report_hash
            || keccak256(&borsh::to_vec(&p.value).unwrap_or_default()) != st.value_hash
        {
            return Err(ChainError::BadSettled("report is not the settled one"));
        }
        if p.source != *s
            || p.metric != fact.metric
            || p.subject != fact.subject
            || p.observed_unix != fact.as_of
        {
            return Err(ChainError::BadSettled("report is of another fact"));
        }
        let public_from = st
            .accepted_at
            .checked_add(DISPUTE_WINDOW_SECS)
            .ok_or(ChainError::BadSettled("accepted time"))?;
        if public_from > head {
            return Err(ChainError::BadSettled("settled before its window ended"));
        }
        out.push(Resolution {
            fact: fact.clone(),
            value: p.value.clone(),
            level: ProofLevel::SourceProven,
            resolver: s.clone(),
            evidence: st.report_hash,
            public_from,
        });
    }
    Ok(())
}

fn level2(
    view: &dyn ChainView,
    template: Hash32,
    dated: bool,
    fact: &Fact,
    head: u64,
    preimages: &[Level2Preimage],
    out: &mut Vec<Resolution>,
) -> Result<(), ChainError> {
    let key = registry_fact_key(
        &metric_key(&fact.metric),
        &template,
        &subject_key(&fact.subject),
        fact.as_of,
        dated,
    );
    let Some(rec) = read(view.recorded(&fact.metric, &fact.subject, fact.as_of))? else {
        return Ok(());
    };
    // A dated feed's key ignores the window: another `as_of`, another fact.
    if rec.window_end != fact.as_of {
        return Ok(());
    }
    if rec.recorded_time > head || rec.template != template {
        return Err(ChainError::BadSettled("recorded observation"));
    }
    let pre = preimages
        .iter()
        .find(|p| {
            p.template.id() == template && p.spec.id() == rec.spec && p.result_hash() == rec.result
        })
        .ok_or(ChainError::Withheld(key))?;
    let spec = &pre.spec;
    if pre.template.id() != template
        || (MetricKind::of(&pre.template) == MetricKind::DatedFeed) != dated
    {
        return Err(ChainError::BadPreimage("not the registered template"));
    }
    if pre.template.matches(spec).is_err() {
        return Err(ChainError::BadPreimage("spec is not the template's"));
    }
    if spec.metric != fact.metric || spec.subject != fact.subject || spec.window_end != fact.as_of {
        return Err(ChainError::BadPreimage("spec is of another fact"));
    }
    let ObservedOutcome::Value(answer) = &pre.outcome else {
        return Ok(());
    };
    let Some(value) = level2_value(&spec.kind, answer) else {
        return Ok(());
    };
    out.push(Resolution {
        fact: fact.clone(),
        value,
        level: ProofLevel::ReObserved,
        resolver: spec.rights.source.clone(),
        evidence: rec.result,
        public_from: rec.recorded_time,
    });
    Ok(())
}

/// The previous commitment's `as_of` and log (what a derivation extends).
#[derive(Clone, Copy, Debug)]
pub struct Prev<'a> {
    /// Its `as_of`.
    pub as_of: u64,
    /// Its statements.
    pub statements: &'a [Statement],
    /// Its resolutions.
    pub resolutions: &'a [Resolution],
}

/// The resolution list of a commitment at `view`'s head (ADR-0036 amendment 3
/// items 1-2, 6): the previous commitment's resolutions unchanged, plus, for
/// each fact `statements` name, every new answer the chain gives at an
/// accepted level. For a fact `prev` resolved, only answers no stronger than
/// its weakest committed one, public after `prev.as_of`; for a fact `prev`'s
/// statements named, only answers public after `prev.as_of` (one usable
/// earlier and missing from `prev` is an omission there, which only a check
/// of `prev` at its own head finds: [`verify_chain`]); for a fact first named
/// now, any answer. Canonical order. `pre` opens settled reports a reader
/// could not take from calldata and recorded level-2 observations; nothing
/// else is caller input.
pub fn derive_resolutions(
    params: &TrackParams,
    reg: &dyn Registry,
    view: &dyn ChainView,
    statements: &[Statement],
    prev: Option<Prev<'_>>,
    pre: &Preimages,
) -> Result<Vec<Resolution>, ChainError> {
    params.validate(reg)?;
    if params.chain == Hash32::ZERO || params.chain != view.chain() {
        return Err(ChainError::NotThisChain);
    }
    let wants = |l: ProofLevel| params.resolver_levels.contains(&l);
    // Fail closed before the first commitment (TR-C7-7, TR-C7-R2-1): level-1
    // rows cannot be read, so params accepting level 1 are refused outright.
    // Refusing per stated fact of a recipe metric halted the whole record once
    // the registry registered a recipe for a metric already stated on.
    if wants(ProofLevel::Recomputed) {
        return Err(ChainError::Level1Unsupported);
    }
    // Fail closed (TR-C7-R1-8): without a registry nobody can tell a level-2
    // metric from an unregistered one.
    if !view.has_registry() && wants(ProofLevel::ReObserved) {
        return Err(ChainError::NoRegistry);
    }
    let head = view.head().timestamp;
    let (prev_as_of, prev_st, prev_res) = prev.map_or((0, &[][..], &[][..]), |p| {
        (p.as_of, p.statements, p.resolutions)
    });
    // Per fact the previous commitment resolved: its levels and answer hashes.
    let mut resolved: Vec<(Hash32, u8)> = prev_res
        .iter()
        .map(|r| (r.fact.key(), r.level.number()))
        .collect();
    resolved.sort();
    let weakest = |key: &Hash32| {
        let at = resolved.partition_point(|(k, _)| k < key);
        resolved[at..]
            .iter()
            .take_while(|(k, _)| k == key)
            .map(|(_, l)| *l)
            .max()
    };
    let mut known: Vec<Hash32> = prev_res.iter().map(Resolution::hash).collect();
    known.sort();
    let mut named: Vec<Hash32> = prev_st.iter().map(|s| s.fact.key()).collect();
    named.sort();
    named.dedup();
    let mut facts: Vec<(Hash32, &Fact)> =
        statements.iter().map(|s| (s.fact.key(), &s.fact)).collect();
    facts.sort_by_key(|f| f.0);
    facts.dedup_by_key(|f| f.0);
    let sources = if wants(ProofLevel::SourceProven) {
        let mut s = read(view.book_sources())?;
        s.sort();
        s.dedup();
        s
    } else {
        Vec::new()
    };
    let mut out: Vec<Resolution> = prev_res.to_vec();
    for (key, fact) in facts {
        // A resolved fact is read again: a later answer no stronger than its
        // weakest committed one judges only entries left self-resolved or
        // pending (amendment 3 item 6, TR-C7-4); a stronger one could change a
        // judged entry, so it is not taken.
        let floor = weakest(&key);
        let seen = floor.is_some() || named.binary_search(&key).is_ok();
        let mut found = Vec::new();
        let (kind, template) = read(view.registered(&fact.metric))?;
        match kind {
            Registered::Feed | Registered::DatedFeed if wants(ProofLevel::ReObserved) => {
                let dated = kind == Registered::DatedFeed;
                level2(
                    view,
                    template,
                    dated,
                    fact,
                    head,
                    &pre.observations,
                    &mut found,
                )?;
            }
            _ => {}
        }
        if wants(ProofLevel::SourceProven) {
            level3(view, &sources, fact, head, &pre.reports, &mut found)?;
        }
        for r in found.into_iter().filter(|r| usable(params, reg, r)) {
            let stronger = floor.is_some_and(|l| r.level.number() < l);
            let early = prev.is_some() && seen && r.public_from <= prev_as_of;
            if stronger || early || known.binary_search(&r.hash()).is_ok() {
                continue;
            }
            out.push(r);
        }
    }
    sort_resolutions(&mut out);
    Ok(out)
}

/// The params' operators against the book (ADR-0036 amendment 3 item 5):
/// every stating source has a key in the book (so its operator was fixed
/// before it could state), and for every source of the log, stating,
/// resolving or listed, the params declare exactly the book's operator when
/// the source has a book key, and none when it has not.
pub fn check_operators(
    params: &TrackParams,
    view: &dyn ChainView,
    statements: &[Statement],
    resolutions: &[Resolution],
) -> Result<(), ChainError> {
    let mut book = read(view.book_sources())?;
    book.sort();
    let mut stating: Vec<&SourceId> = statements.iter().map(|s| &s.source).collect();
    stating.sort();
    stating.dedup();
    if let Some(s) = stating.iter().find(|s| book.binary_search(s).is_err()) {
        return Err(ChainError::NotInBook((*s).clone()));
    }
    let mut all = stating;
    all.extend(resolutions.iter().map(|r| &r.resolver));
    all.extend(params.operators.iter().map(|o| &o.source));
    all.sort();
    all.dedup();
    for s in all {
        // Only a source with a book key has a fixed operator: its declaration
        // precedes its first key, so precedes anything it signed. A keyless
        // source (a level-2 feed's source) could be declared after it resolved
        // and regrouping it would rewrite committed outcomes; its declaration
        // is not used and the params list none for it (TR-C7-R1-4).
        let on_chain = if book.binary_search(s).is_ok() {
            read(view.operator_of(s))?
        } else {
            Hash32::ZERO
        };
        if params.operator_of(s).unwrap_or(Hash32::ZERO) != on_chain {
            return Err(ChainError::OperatorMismatch(s.clone()));
        }
    }
    Ok(())
}

/// The client check of a chain-resolved commitment (ADR-0036 amendment 3
/// item 3): `as_of` is the view's head time, the operators match the book,
/// and the log's resolutions are exactly [`derive_resolutions`] at that head
/// from its statements and the previous commitment's log (`prev`: its `as_of`
/// and log, `None` for the first). With [`crate::check_extends`] (and, with an
/// intake, [`crate::intake::verify_log`]) a rerun of `compute` on the log then
/// rests on chain data only.
pub fn verify_resolutions(
    reg: &dyn Registry,
    view: &dyn ChainView,
    as_of: u64,
    log: TrackLog<'_>,
    prev: Option<(u64, TrackLog<'_>)>,
    pre: &Preimages,
) -> Result<(), ChainError> {
    let head = view.head().timestamp;
    if as_of != head {
        return Err(ChainError::NotTheHead { as_of, head });
    }
    check_operators(log.params, view, log.statements, log.resolutions)?;
    let derived = derive_resolutions(
        log.params,
        reg,
        view,
        log.statements,
        prev.map(|(t, l)| Prev {
            as_of: t,
            statements: l.statements,
            resolutions: l.resolutions,
        }),
        pre,
    )?;
    if derived != log.resolutions {
        return Err(ChainError::Mismatch);
    }
    Ok(())
}

/// One entry of a commitment log, with what checks it: the registry it was
/// computed with (its `registry_root`), the chain at its head, its log, the
/// preimages, and the log's hash at its index (`trackCommitmentAt`).
pub struct ChainStep<'a> {
    /// The registry of this commitment (the one its `registry_root` names).
    pub reg: &'a dyn Registry,
    /// The chain at this commitment's head.
    pub view: &'a dyn ChainView,
    /// The commitment.
    pub commitment: &'a TrackCommitment,
    /// Its log.
    pub log: TrackLog<'a>,
    /// Its preimages.
    pub preimages: &'a Preimages,
    /// `trackCommitmentAt(poster, track, index)` at a finalized block.
    pub at: Hash32,
}

/// The whole check of a chain-resolved record (ADR-0036 amendment 3 item 3,
/// review TR-C7-R1-3): every entry of the on-chain log from the first, each
/// at its own head. Each is the log's entry at its index, chained to the one
/// before (the first to zero), computed with the registry it names, its
/// resolutions rederived from the chain (`verify_resolutions`, which alone
/// takes the previous log on trust), extending the one before
/// (`check_extends`), and equal to a rerun of `compute`; the last is the log's
/// latest (`check_latest`). Checking only the latest entry would accept an
/// earlier commitment's forged resolutions, which every later one carries.
pub fn verify_chain(steps: &[ChainStep<'_>], head: &LogHead) -> Result<(), ChainError> {
    if steps.is_empty() || steps.len() as u64 != head.count {
        return Err(ChainError::NotLatest);
    }
    for (i, st) in steps.iter().enumerate() {
        let c = st.commitment;
        let prev = i.checked_sub(1).map(|j| &steps[j]);
        let chained = prev.map_or(Hash32::ZERO, |p| p.commitment.hash());
        if st.at != c.hash() || c.prev != chained || st.reg.root() != c.registry_root {
            return Err(ChainError::NotTheLogs(i));
        }
        verify_resolutions(
            st.reg,
            st.view,
            c.as_of,
            st.log,
            prev.map(|p| (p.commitment.as_of, p.log)),
            st.preimages,
        )?;
        if let Some(p) = prev {
            crate::check_extends(p.commitment, p.log, c, st.log)
                .map_err(|e| ChainError::NotExtended(i, e))?;
        }
        let book = crate::compute(
            st.log.params,
            st.reg,
            st.log.statements,
            st.log.resolutions,
            c.as_of,
        )?;
        if book.commitment(chained) != *c {
            return Err(ChainError::Mismatch);
        }
    }
    check_latest(steps[steps.len() - 1].commitment, head)
}

/// Where a commitment chain is published: `OnyxSignedBook` at `book` on
/// `chain_id`, the log of `poster` under `track`. A client pins all four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitmentLog {
    /// EVM chain id.
    pub chain_id: u64,
    /// The book.
    pub book: [u8; 20],
    /// The poster whose log it is.
    pub poster: [u8; 20],
    /// The track id (non-zero).
    pub track: Hash32,
}

/// `trackHead(poster, track)` at a finalized block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogHead {
    /// Commitments in the log.
    pub count: u64,
    /// The latest one's hash (zero: empty).
    pub latest: Hash32,
    /// Its `as_of`.
    pub as_of: u64,
}

/// The bytes `postTrackCommitment` takes: the Borsh commitment
/// ([`COMMITMENT_BYTES`] long; the book hashes them with the commitment domain).
pub fn commitment_bytes(c: &TrackCommitment) -> Vec<u8> {
    borsh::to_vec(c).unwrap_or_default()
}

/// The served commitment is the latest of its on-chain log (read at a
/// finalized block), so a server can neither hold a client on an older one
/// (TR-R2-5) nor show one the log does not hold.
pub fn check_latest(c: &TrackCommitment, head: &LogHead) -> Result<(), ChainError> {
    if head.count == 0 || c.hash() != head.latest || c.as_of != head.as_of {
        return Err(ChainError::NotLatest);
    }
    Ok(())
}

/// A commitment the client saw before is the log's entry `at` (read with
/// `trackCommitmentAt(poster, track, index)`): the log is one chain, so the
/// latest descends from it and no fork after it can be the latest (TR-R1-11,
/// TR-S-14).
pub fn check_in_log(known: &TrackCommitment, at: &Hash32) -> Result<(), ChainError> {
    if known.hash() != *at {
        return Err(ChainError::NotInLog);
    }
    Ok(())
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
