//! Proof level 5: a source's track record (ADR-0036, docs/spec/proof-ladder.md "Level 5").
//!
//! A level-5 value is judged now and checked later. This crate keeps, per
//! (source, metric), the history of a source's statements and how each turned
//! out once a stronger answer (a [`Resolution`] at an accepted level 1-3) for
//! the same [`Fact`] exists. A statement is compared with that answer only,
//! never with other sources (agreement with peers rewards copying).
//!
//! Everything is integer, Borsh and Keccak-256 (ADR-0002, 0003, 0009), so every
//! machine computes the same [`TrackBook`] and the same [`TrackCommitment`].
//!
//! What a record must never do (ADR-0036 item 12): raise a proof level, settle
//! money, pick a value among sources, drive rewards or slashing. Nothing here
//! returns a level or a market policy; a level-5 value stays level 5 whatever
//! its source's record (`scripts/check-boundaries.sh`, TR-11).

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::proof::MAX_TOLERANCE;
use onyx_core::{
    keccak256, Confidence, Hash32, MetricId, ProofLevel, Registry, SourceId, Subject, Value,
    ValueKind,
};
use onyx_merkle::{MerkleTree, Proof};
use std::cmp::Ordering;

/// Domain tag of [`Fact::key`].
pub const FACT_DOMAIN: &[u8] = b"onyx-track-fact-v1";
/// Domain tag of [`Statement::hash`].
pub const STATEMENT_DOMAIN: &[u8] = b"onyx-track-statement-v1";
/// Domain tag of [`Resolution::hash`].
pub const RESOLUTION_DOMAIN: &[u8] = b"onyx-track-resolution-v1";
/// Domain tag of [`TrackParams::hash`].
pub const PARAMS_DOMAIN: &[u8] = b"onyx-track-params-v1";
/// Domain tag of [`statements_hash`].
pub const STATEMENTS_DOMAIN: &[u8] = b"onyx-track-statements-v1";
/// Domain tag of [`resolutions_hash`].
pub const RESOLUTIONS_DOMAIN: &[u8] = b"onyx-track-resolutions-v1";
/// Domain tag of [`TrackCommitment::hash`].
pub const COMMITMENT_DOMAIN: &[u8] = b"onyx-track-commitment-v1";

/// Resolved statements before a record is "established" (ADR-0016 item 6's
/// number, reused; PLAN q-TR-1).
pub const MIN_RESOLVED: u64 = 30;
/// Distinct subjects among resolved statements before a record is
/// "established" (ADR-0016 item 6's number, reused; PLAN q-TR-1).
pub const MIN_DISTINCT_SUBJECTS: u64 = 5;
/// Lowest confidence a statement may carry (TR-R1-3): a statement claims its
/// value is more likely right than not, so a source cannot score a perfect
/// Brier by stating known-wrong values at confidence 0.
pub const MIN_CONFIDENCE_PPM: u32 = 500_000;
/// Most statements, resolutions or rules in one computation (keeps every sum
/// inside `u128`, as `onyx_labels::score::MAX_FORECASTS`).
pub const MAX_ITEMS: usize = 1 << 24;
/// How long after the first answer that may judge an entry other answers at
/// its level still join it (and contest it if they differ); the entry is
/// judged once this window has passed (ADR-0036 amendment 3 item 6, THREATS
/// TR-C7-R1-6). Equal to `OnyxSignedBook.DISPUTE_WINDOW_SECS`.
pub const CONTEST_WINDOW_SECS: u64 = 3_600;
/// One Brier unit: scores are in units of 1e-12 (ppm squared).
pub const BRIER_ONE: u64 = 1_000_000_000_000;
/// The default resolver levels: 1 and 3, the money default's levels (a
/// colluding level-2 majority has no dispute path yet, ADR-0036 item 2).
pub const DEFAULT_RESOLVER_LEVELS: [ProofLevel; 2] =
    [ProofLevel::Recomputed, ProofLevel::SourceProven];

const PPM: u64 = Confidence::ONE as u64;
const MAX_COUNT: u64 = MAX_ITEMS as u64;

pub(crate) fn hash_with<T: BorshSerialize + ?Sized>(domain: &[u8], value: &T) -> Hash32 {
    let mut b = domain.to_vec();
    // Borsh into a Vec cannot fail for these types (no I/O, no maps).
    b.extend_from_slice(&borsh::to_vec(value).unwrap_or_default());
    keccak256(&b)
}

/// What is judged: one metric's value for one subject as of one time. No
/// source is part of it, so every source's statement meets the same answer.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Fact {
    /// Registered metric.
    pub metric: MetricId,
    /// What the value is about.
    pub subject: Subject,
    /// The time the value is "as of" (unix seconds).
    pub as_of: u64,
}

impl Fact {
    /// `keccak256("onyx-track-fact-v1" ‖ borsh(fact))`.
    pub fn key(&self) -> Hash32 {
        hash_with(FACT_DOMAIN, self)
    }
}

/// A value a source stated about a fact before it could be checked.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Statement {
    /// Registered source that stated it (an organisation or publisher, never a person).
    pub source: SourceId,
    /// The fact.
    pub fact: Fact,
    /// The stated value, of the metric's registered kind.
    pub value: Value,
    /// Stated probability, in ppm, that `value` will agree with the
    /// resolution: [`MIN_CONFIDENCE_PPM`]..=1,000,000.
    pub confidence: Confidence,
    /// When Onyx first committed the statement: the timestamp of the
    /// finalized block carrying its batch's post (unix seconds; ADR-0036
    /// amendment 1, [`intake`]; checked by [`intake::verify_log`]).
    pub committed_at: u64,
}

impl Statement {
    /// `keccak256("onyx-track-statement-v1" ‖ borsh(statement))`.
    pub fn hash(&self) -> Hash32 {
        hash_with(STATEMENT_DOMAIN, self)
    }

    fn order_key(&self) -> (&SourceId, &MetricId, Hash32, Hash32) {
        (
            &self.source,
            &self.fact.metric,
            self.fact.key(),
            self.hash(),
        )
    }
}

/// A stronger answer for a fact: a level 1-3 value that became available later.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Resolution {
    /// The fact.
    pub fact: Fact,
    /// The checked value.
    pub value: Value,
    /// Its proof level (must be one of the params' resolver levels).
    pub level: ProofLevel,
    /// Registered source the stronger answer comes from (the signing source at
    /// level 3, the input data's source at level 1). A resolution never judges
    /// a statement of its own source (TR-R1-7) nor of a source with the same
    /// declared operator (TR-R2-4).
    pub resolver: SourceId,
    /// Hash of the stronger answer (claim hash, level-2 result hash or level-3
    /// report hash). Non-zero.
    pub evidence: Hash32,
    /// Earliest time the checked value was public (unix seconds).
    pub public_from: u64,
}

impl Resolution {
    /// `keccak256("onyx-track-resolution-v1" ‖ borsh(resolution))`.
    pub fn hash(&self) -> Hash32 {
        hash_with(RESOLUTION_DOMAIN, self)
    }

    fn order_key(&self) -> (Hash32, Hash32) {
        (self.fact.key(), self.hash())
    }
}

/// When statements about a fact of a metric close (ADR-0036 amendment 4 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Close {
    /// The fact is a value as of a point in time, public no earlier than its
    /// `as_of`: statements close this many seconds after `as_of`.
    AfterAsOf(u64),
    /// The fact is a total over the period `[as_of - period_secs, as_of)`,
    /// whose data turns public piece by piece during the period (Bitcoin
    /// flows, block by block). `as_of` must be a multiple of `period_secs`, so
    /// with the rule it names exactly one period (the fact carries its period;
    /// for a whole number of days, UTC-aligned). Statements close `lead_secs`
    /// before the period starts, before any of its data can be public.
    PeriodStart {
        /// Length of the period (1..=[`MAX_PERIOD_SECS`]).
        period_secs: u64,
        /// How long before the period's start statements close (at most
        /// [`MAX_PERIOD_SECS`]): covers how early the period's first data can
        /// be public (for Bitcoin, a block timestamped up to 2 hours ahead).
        lead_secs: u64,
    },
}

/// Longest period (and lead) a [`Close::PeriodStart`] rule may name: 366 days.
pub const MAX_PERIOD_SECS: u64 = 366 * 86_400;

/// Per-metric rule (a reviewer decision, like a feed template).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MetricRule {
    /// The metric.
    pub metric: MetricId,
    /// When statements about its facts close.
    pub close: Close,
    /// Largest absolute difference that still agrees, in the value's own units
    /// (mantissa units for `Fixed`). Must be 0 for `Bool` and `Bytes`.
    pub tolerance: u128,
}

impl MetricRule {
    /// A rule whose statements close `close_after_secs` after the fact's `as_of`.
    pub fn after(metric: MetricId, close_after_secs: u64, tolerance: u128) -> Self {
        MetricRule {
            metric,
            close: Close::AfterAsOf(close_after_secs),
            tolerance,
        }
    }

    /// A rule for period totals: `[as_of - period_secs, as_of)`, closing
    /// `lead_secs` before the period's start.
    pub fn period_start(
        metric: MetricId,
        period_secs: u64,
        lead_secs: u64,
        tolerance: u128,
    ) -> Self {
        MetricRule {
            metric,
            close: Close::PeriodStart {
                period_secs,
                lead_secs,
            },
            tolerance,
        }
    }

    /// When statements about a fact with this `as_of` are revealed (sealed
    /// until then, ADR-0036 amendment 1 item 4): the close for `AfterAsOf`;
    /// `as_of`, the period's end, for `PeriodStart` (review TR-C9-R1-5: a
    /// forecast is never public while the period whose total it forecasts is
    /// still open, so nobody can steer the truth toward or away from it).
    pub fn reveal_at(&self, as_of: u64) -> Result<u64, TrackError> {
        let close = self.close_at(as_of)?;
        Ok(match self.close {
            Close::AfterAsOf(_) => close,
            Close::PeriodStart { .. } => as_of,
        })
    }

    /// When statements about a fact with this `as_of` close. `Overflow` past
    /// `u64`, `BadPeriod` for an `as_of` that names no whole period.
    pub fn close_at(&self, as_of: u64) -> Result<u64, TrackError> {
        match self.close {
            Close::AfterAsOf(secs) => as_of.checked_add(secs).ok_or(TrackError::Overflow),
            Close::PeriodStart {
                period_secs,
                lead_secs,
            } => {
                if period_secs == 0 || !as_of.is_multiple_of(period_secs) {
                    return Err(TrackError::BadPeriod);
                }
                as_of
                    .checked_sub(period_secs)
                    .and_then(|start| start.checked_sub(lead_secs))
                    .ok_or(TrackError::BadPeriod)
            }
        }
    }
}

/// The pinned parameters of a computation.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TrackParams {
    /// Levels a resolution may have: non-empty, strictly ascending, within
    /// levels 1-3 (levels 4 and 5 never resolve anything).
    pub resolver_levels: Vec<ProofLevel>,
    /// One rule per metric, strictly ascending by metric. A metric without a
    /// rule has no track record.
    pub rules: Vec<MetricRule>,
    /// The statement intake every statement comes through
    /// ([`intake::IntakeId::id`]; zero: none, the log is caller input as
    /// before C5). Pinned for the life of a commitment chain, so one chain
    /// cannot switch to another intake's posts (review TR-R3-3), and a client
    /// that sees it non-zero must run [`intake::verify_log`].
    pub intake: Hash32,
    /// The chain every resolution is read from ([`chain::ChainId::id`];
    /// zero: none, the resolution list is caller input as before C7). Pinned
    /// for the life of a commitment chain like `intake`; a client that sees it
    /// non-zero must run [`chain::verify_resolutions`] (ADR-0036 amendment 3).
    pub chain: Hash32,
    /// Declared operators, strictly ascending by source; a source not listed
    /// is its own operator. A resolution never judges a statement of a source
    /// with the resolver's operator (`SelfResolved`, THREATS TR-R2-4). With
    /// `chain` set it must equal the chain's declarations
    /// (`OnyxSignedBook.operatorOf`) for every source of the logs.
    pub operators: Vec<OperatorOf>,
}

/// A declared operator of a source (ADR-0036 amendment 3 item 5).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct OperatorOf {
    /// The source.
    pub source: SourceId,
    /// Its operator's id (non-zero; `OnyxSignedBook.operatorOf`).
    pub operator: Hash32,
}

impl TrackParams {
    /// Params with [`DEFAULT_RESOLVER_LEVELS`].
    pub fn new(rules: Vec<MetricRule>) -> Self {
        TrackParams {
            resolver_levels: DEFAULT_RESOLVER_LEVELS.to_vec(),
            rules,
            intake: Hash32::ZERO,
            chain: Hash32::ZERO,
            operators: Vec::new(),
        }
    }

    /// The same params, fed by intake `intake`.
    pub fn with_intake(mut self, intake: Hash32) -> Self {
        self.intake = intake;
        self
    }

    /// The same params with level 3 (signed sources) as the only resolver
    /// level, for a chain without an `OnyxRegistry` (ADR-0036 amendment 3).
    pub fn with_level3_resolvers_only(mut self) -> Self {
        self.resolver_levels = vec![ProofLevel::SourceProven];
        self
    }

    /// The same params, resolved from chain `chain` ([`chain::ChainId::id`]).
    pub fn with_chain(mut self, chain: Hash32) -> Self {
        self.chain = chain;
        self
    }

    /// The same params with these declared operators (sorted by source here).
    pub fn with_operators(mut self, mut operators: Vec<OperatorOf>) -> Self {
        operators.sort_by(|a, b| a.source.cmp(&b.source));
        self.operators = operators;
        self
    }

    /// The declared operator of `source`, if any.
    pub fn operator_of(&self, source: &SourceId) -> Option<Hash32> {
        self.operators
            .binary_search_by(|o| o.source.cmp(source))
            .ok()
            .map(|i| self.operators[i].operator)
    }

    /// True when `a` and `b` are one source or share a declared operator: an
    /// answer of one never judges a statement of the other (TR-R1-7, TR-R2-4).
    pub fn same_operator(&self, a: &SourceId, b: &SourceId) -> bool {
        a == b
            || matches!(
                (self.operator_of(a), self.operator_of(b)),
                (Some(x), Some(y)) if x == y
            )
    }

    /// `keccak256("onyx-track-params-v1" ‖ borsh(params))`.
    pub fn hash(&self) -> Hash32 {
        hash_with(PARAMS_DOMAIN, self)
    }

    /// The rule of `metric`, if any.
    pub fn rule(&self, metric: &MetricId) -> Option<&MetricRule> {
        self.rules
            .binary_search_by(|r| r.metric.cmp(metric))
            .ok()
            .map(|i| &self.rules[i])
    }

    /// When statements about `fact` close ([`MetricRule::close_at`]); `None`
    /// without a rule, on overflow or for an `as_of` naming no whole period.
    pub fn close_of(&self, fact: &Fact) -> Option<u64> {
        self.rule(&fact.metric)
            .and_then(|r| r.close_at(fact.as_of).ok())
    }

    /// When statements about `fact` are revealed ([`MetricRule::reveal_at`]);
    /// `None` as for [`TrackParams::close_of`].
    pub fn reveal_of(&self, fact: &Fact) -> Option<u64> {
        self.rule(&fact.metric)
            .and_then(|r| r.reveal_at(fact.as_of).ok())
    }

    fn accepts(&self, level: ProofLevel) -> bool {
        self.resolver_levels.contains(&level)
    }

    /// True if `self` keeps every rule, the resolver levels, the intake, the
    /// chain and every declared operator of `earlier` unchanged (it may only add
    /// rules for new metrics and operators; [`check_extends`] also refuses an
    /// operator added for a source the earlier logs already hold), so no
    /// outcome already committed can change (TR-R1-2).
    pub fn extends(&self, earlier: &TrackParams) -> bool {
        self.resolver_levels == earlier.resolver_levels
            && self.intake == earlier.intake
            && self.chain == earlier.chain
            && is_subset(&earlier.rules, &self.rules, |r| r.metric.clone())
            && is_subset(&earlier.operators, &self.operators, |o| o.source.clone())
    }

    /// Shape and registry check.
    pub fn validate(&self, reg: &dyn Registry) -> Result<(), TrackError> {
        if self.resolver_levels.is_empty()
            || !self.resolver_levels.windows(2).all(|w| w[0] < w[1])
            || self
                .resolver_levels
                .iter()
                .any(|l| !l.meets(ProofLevel::SourceProven))
        {
            return Err(TrackError::BadParams("resolver levels"));
        }
        if self.rules.len() > MAX_ITEMS || self.operators.len() > MAX_ITEMS {
            return Err(TrackError::TooMany);
        }
        if !self.operators.windows(2).all(|w| w[0].source < w[1].source)
            || self.operators.iter().any(|o| o.operator == Hash32::ZERO)
        {
            return Err(TrackError::BadParams("operators"));
        }
        if !self.rules.windows(2).all(|w| w[0].metric < w[1].metric) {
            return Err(TrackError::BadParams("rules not strictly ascending"));
        }
        for r in &self.rules {
            let info = reg
                .metric(&r.metric.0)
                .ok_or_else(|| TrackError::UnknownMetric(r.metric.clone()))?;
            let exact = matches!(info.value_kind, ValueKind::Bool | ValueKind::Bytes);
            if r.tolerance > MAX_TOLERANCE || (exact && r.tolerance != 0) {
                return Err(TrackError::BadParams("tolerance"));
            }
            if let Close::PeriodStart {
                period_secs,
                lead_secs,
            } = r.close
            {
                if period_secs == 0 || period_secs > MAX_PERIOD_SECS || lead_secs > MAX_PERIOD_SECS
                {
                    return Err(TrackError::BadParams("period"));
                }
            }
        }
        Ok(())
    }
}

/// How one source's statement about one fact turned out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Outcome {
    /// Within the metric's tolerance of the resolution.
    Agreed,
    /// Beyond it.
    Disagreed,
    /// The source stated a resolved fact more than once with a different
    /// value or confidence: a confident miss.
    Equivocated,
    /// The source's earliest statement came after the fact's close. Not counted.
    Late,
    /// The resolution was public at or before the fact's close. Not counted.
    Void,
    /// Resolutions at the strongest level present disagree. Not counted.
    Contested,
    /// No resolution yet. Not counted.
    Unresolved,
    /// The only resolutions come from the stating source itself or a source
    /// with its declared operator (TR-R2-4). Not counted.
    SelfResolved,
}

impl Outcome {
    /// True for the outcomes that count as resolved (agreed, disagreed, equivocated).
    pub fn counted(self) -> bool {
        matches!(
            self,
            Outcome::Agreed | Outcome::Disagreed | Outcome::Equivocated
        )
    }
}

/// One (source, fact): a row of the history tree.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Entry {
    /// The source.
    pub source: SourceId,
    /// The fact.
    pub fact: Fact,
    /// How it turned out.
    pub outcome: Outcome,
    /// Hash of the source's earliest statement on the fact.
    pub statement: Hash32,
    /// Distinct statements the source made on the fact (more than 1 only when equivocated).
    pub statements: u64,
    /// Earliest commit time of the source's statements on the fact.
    pub committed_at: u64,
    /// Confidence of the earliest statement.
    pub confidence: Confidence,
    /// Hash of the resolution used (zero if none, contested or self-resolved).
    pub resolution: Hash32,
    /// Its level number (0 if none, contested or self-resolved).
    pub resolution_level: u8,
    /// Agreed, and no other source stated an agreeing value earlier.
    pub first: bool,
    /// Brier term in units of 1e-12 (0 when not counted).
    pub brier_e12: u64,
    /// How the naive persistence forecast did on this entry (C9, PLAN q-TR-5):
    /// `None` unless the entry is counted and a last known value existed by
    /// its close.
    pub baseline: Baseline,
}

/// The naive baseline of a counted entry: persistence, the last value known
/// at the entry's close (C9, PLAN q-TR-5, R-TR-11), on period metrics only.
/// Its fact is the period of the same metric and subject that ended last by
/// the close (no older one), named by an on-time statement, and judged uncontested by the answers
/// public by the close other than the stating source's own or its operator's.
/// The baseline agrees when that value agrees with the entry's judging answer
/// under the metric's tolerance. Display only: it never changes an outcome, a
/// level or money.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Baseline {
    /// Not counted, or no last known value by the close.
    None,
    /// The last known value agreed with the answer.
    Agreed,
    /// It did not.
    Disagreed,
}

/// One (source, metric): a row of the record tree.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Record {
    /// The source.
    pub source: SourceId,
    /// The metric.
    pub metric: MetricId,
    /// Index of its first entry in the history tree.
    pub entries_from: u64,
    /// Number of its entries (contiguous).
    pub entries_count: u64,
    /// Outcome counts.
    pub agreed: u64,
    /// Agreed and first to state an agreeing value.
    pub agreed_first: u64,
    /// Outcome counts.
    pub disagreed: u64,
    /// Outcome counts.
    pub equivocated: u64,
    /// Outcome counts (not counted as resolved).
    pub late: u64,
    /// Outcome counts (not counted as resolved).
    pub void: u64,
    /// Outcome counts (not counted as resolved).
    pub contested: u64,
    /// Outcome counts (not counted as resolved).
    pub unresolved: u64,
    /// Outcome counts (not counted as resolved).
    pub self_resolved: u64,
    /// Sum of the Brier terms of the resolved entries, units of 1e-12.
    pub brier_sum_e12: u128,
    /// Distinct subjects among the resolved entries.
    pub distinct_subjects: u64,
    /// Earliest commit time over all its entries.
    pub first_committed_at: u64,
    /// Latest commit time over all its entries.
    pub last_committed_at: u64,
    /// Sources with any entry on this metric ("one of N"); equals the
    /// metric's committed [`MetricSummary`].
    pub metric_sources: u64,
    /// Facts of this metric with an uncontested resolution (coverage); equals
    /// the metric's committed [`MetricSummary`].
    pub metric_resolved_facts: u64,
    /// Resolved entries with a persistence baseline ([`Baseline`], C9).
    pub baseline_entries: u64,
    /// Of those, the entries where the source agreed.
    pub baseline_source_agreed: u64,
    /// Of those, the entries where the last known value agreed.
    pub baseline_agreed: u64,
}

impl Record {
    /// Agreed + disagreed + equivocated (saturating on an unverified record).
    pub fn resolved(&self) -> u64 {
        self.agreed
            .saturating_add(self.disagreed)
            .saturating_add(self.equivocated)
    }

    /// Agreed share of resolved in ppm, floored; `None` with nothing resolved
    /// or on overflow (an unverified record).
    pub fn agreement_ppm(&self) -> Option<u64> {
        let n = self.resolved();
        if n == 0 {
            return None;
        }
        self.agreed.checked_mul(PPM).map(|a| a / n)
    }

    /// Mean Brier term in units of 1e-12, floored; `None` with nothing resolved.
    /// For display only: compare records with [`Record::cmp_brier`].
    pub fn mean_brier_e12(&self) -> Option<u128> {
        let n = self.resolved();
        (n > 0).then(|| self.brier_sum_e12 / u128::from(n))
    }

    /// Mean Brier compared exactly by cross-multiplying (lower is better);
    /// `None` if either has nothing resolved or on overflow. Secondary to the
    /// agreement count in any ranking (TR-R1-3).
    pub fn cmp_brier(&self, other: &Record) -> Option<Ordering> {
        let (a, b) = (self.resolved(), other.resolved());
        if a == 0 || b == 0 {
            return None;
        }
        let x = self.brier_sum_e12.checked_mul(u128::from(b))?;
        let y = other.brier_sum_e12.checked_mul(u128::from(a))?;
        Some(x.cmp(&y))
    }

    /// Skill against persistence (C9, PLAN q-TR-5): the source's agreement
    /// rate minus the last known value's, over the entries with a baseline, in
    /// ppm (-1,000,000..=1,000,000), floored. `None` without such an entry or
    /// on an inconsistent (unverified) record. Positive: the source did better
    /// than repeating the last known value. Display only, beside the counts.
    pub fn skill_ppm(&self) -> Option<i64> {
        let n = self.baseline_entries;
        if n == 0 || self.baseline_source_agreed > n || self.baseline_agreed > n {
            return None;
        }
        let diff = i128::from(self.baseline_source_agreed) - i128::from(self.baseline_agreed);
        i64::try_from((diff * i128::from(PPM)).div_euclid(i128::from(n))).ok()
    }

    /// Enough resolved statements over enough subjects to show a rate
    /// ([`MIN_RESOLVED`], [`MIN_DISTINCT_SUBJECTS`]). Below it, show counts only.
    pub fn established(&self) -> bool {
        self.resolved() >= MIN_RESOLVED && self.distinct_subjects >= MIN_DISTINCT_SUBJECTS
    }

    /// Every count within what one computation can produce (TR-R1-9).
    fn in_bounds(&self) -> bool {
        let counts = [
            self.entries_count,
            self.agreed,
            self.agreed_first,
            self.disagreed,
            self.equivocated,
            self.late,
            self.void,
            self.contested,
            self.unresolved,
            self.self_resolved,
            self.distinct_subjects,
            self.metric_sources,
            self.metric_resolved_facts,
            self.baseline_entries,
            self.baseline_source_agreed,
            self.baseline_agreed,
        ];
        counts.iter().all(|c| *c <= MAX_COUNT)
            && self.brier_sum_e12 <= u128::from(MAX_COUNT) * u128::from(BRIER_ONE)
    }
}

/// Context per metric, committed in its own tree so "one of N sources" and
/// coverage are checkable against the commitment (TR-R1-5).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MetricSummary {
    /// The metric.
    pub metric: MetricId,
    /// Sources with any entry on it.
    pub sources: u64,
    /// Its facts with an uncontested resolution.
    pub resolved_facts: u64,
}

/// Refused input.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrackError {
    /// Malformed params.
    #[error("bad params: {0}")]
    BadParams(&'static str),
    /// A metric that is not registered.
    #[error("unknown metric {0:?}")]
    UnknownMetric(MetricId),
    /// A metric without a rule in the params (fail closed).
    #[error("no track-record rule for metric {0:?}")]
    NoRule(MetricId),
    /// More than [`MAX_ITEMS`].
    #[error("too many items")]
    TooMany,
    /// Not strictly ascending in the canonical order.
    #[error("not canonical: {0}")]
    NotCanonical(&'static str),
    /// A source (or resolver) that is not registered or whose rights forbid ingest.
    #[error("source not admissible: {0:?}")]
    BadSource(SourceId),
    /// A subject the registry refuses.
    #[error("bad subject")]
    BadSubject,
    /// A malformed value or one of the wrong kind for its metric.
    #[error("bad value")]
    BadValue,
    /// Confidence outside [`MIN_CONFIDENCE_PPM`]..=1,000,000.
    #[error("bad confidence")]
    BadConfidence,
    /// A resolution at a level the params do not accept.
    #[error("resolution level {0} not accepted")]
    LevelNotAccepted(u8),
    /// A resolution with zero evidence.
    #[error("resolution without evidence")]
    ZeroEvidence,
    /// A resolution whose resolver or fact is an estimate: an estimate never
    /// judges a forecast (R-EST-2, ADR-0050).
    #[error("an estimate never resolves a fact")]
    EstimateResolution,
    /// A statement committed, or a resolution public, after the computation's `as_of`.
    #[error("statement committed or resolution public after as_of")]
    FromTheFuture,
    /// `as_of + close_after_secs` overflows.
    #[error("close time overflows")]
    Overflow,
    /// A fact of a period metric whose `as_of` is not a multiple of the
    /// rule's period, or whose period would start before time 0.
    #[error("as_of names no whole period of the metric's rule")]
    BadPeriod,
}

pub(crate) fn check_source(reg: &dyn Registry, source: &SourceId) -> Result<(), TrackError> {
    match reg.source_rights(source) {
        Some(rights) if rights.check_ingest().is_ok() => Ok(()),
        _ => Err(TrackError::BadSource(source.clone())),
    }
}

pub(crate) fn check_fact(
    params: &TrackParams,
    reg: &dyn Registry,
    fact: &Fact,
    value: &Value,
) -> Result<u64, TrackError> {
    let rule = params
        .rule(&fact.metric)
        .ok_or_else(|| TrackError::NoRule(fact.metric.clone()))?;
    let info = reg
        .metric(&fact.metric.0)
        .ok_or_else(|| TrackError::UnknownMetric(fact.metric.clone()))?;
    if !fact.subject.is_valid(reg) {
        return Err(TrackError::BadSubject);
    }
    if value.validate().is_err() || !value.is_kind(info.value_kind) {
        return Err(TrackError::BadValue);
    }
    rule.close_at(fact.as_of)
}

/// Is `s` admissible under `params` and `reg` (R-TR-1, without the
/// `committed_at` and order checks)? Returns its close time. The one rule for
/// [`compute`] and the intake ([`intake::Intake::submit`]).
pub fn admissible(
    params: &TrackParams,
    reg: &dyn Registry,
    s: &Statement,
) -> Result<u64, TrackError> {
    check_source(reg, &s.source)?;
    if s.confidence.0 > Confidence::ONE || s.confidence.0 < MIN_CONFIDENCE_PPM {
        return Err(TrackError::BadConfidence);
    }
    check_fact(params, reg, &s.fact, &s.value)
}

/// True if `stated` agrees with `resolved` under `tolerance` (same kind;
/// numbers within the tolerance, everything else exact).
pub fn agrees(stated: &Value, resolved: &Value, tolerance: u128) -> bool {
    match (stated, resolved) {
        (Value::Int(a), Value::Int(b)) => a.abs_diff(*b) <= tolerance,
        (Value::UInt(a), Value::UInt(b)) => a.abs_diff(*b) <= tolerance,
        (
            Value::Fixed {
                mantissa: a,
                scale: sa,
            },
            Value::Fixed {
                mantissa: b,
                scale: sb,
            },
        ) => sa == sb && a.abs_diff(*b) <= tolerance,
        _ => stated == resolved,
    }
}

/// Binary Brier term in units of 1e-12 for a counted outcome with stated
/// confidence `p` (ppm): agreed `(1 - p)^2`, disagreed `p^2`, equivocated 1.
pub fn brier_term(outcome: Outcome, p: Confidence) -> u64 {
    let p = u64::from(p.0).min(PPM);
    match outcome {
        Outcome::Agreed => (PPM - p) * (PPM - p),
        Outcome::Disagreed => p * p,
        Outcome::Equivocated => BRIER_ONE,
        _ => 0,
    }
}

/// `keccak256("onyx-track-statements-v1" ‖ borsh(statements))`.
pub fn statements_hash(statements: &[Statement]) -> Hash32 {
    hash_with(STATEMENTS_DOMAIN, statements)
}

/// `keccak256("onyx-track-resolutions-v1" ‖ borsh(resolutions))`.
pub fn resolutions_hash(resolutions: &[Resolution]) -> Hash32 {
    hash_with(RESOLUTIONS_DOMAIN, resolutions)
}

/// Sort statements into the canonical order (source, metric, fact key,
/// statement hash) and drop exact duplicates.
pub fn sort_statements(statements: &mut Vec<Statement>) {
    // Each key once per row (two Keccak hashes): a comparison sort that
    // recomputed them ran about 4 log n hashes per row (review C20-R2-1). Rows
    // with equal keys are equal (the key ends with the row's hash), so an
    // unstable sort gives the same order.
    statements.sort_by_cached_key(|s| {
        let (a, b, c, d) = s.order_key();
        (a.clone(), b.clone(), c, d)
    });
    statements.dedup();
}

/// Sort resolutions into the canonical order (fact key, resolution hash) and
/// drop exact duplicates.
pub fn sort_resolutions(resolutions: &mut Vec<Resolution>) {
    resolutions.sort_by_cached_key(Resolution::order_key);
    resolutions.dedup();
}

/// One resolution at the strongest level present for its fact.
struct Answer<'a> {
    resolver: &'a SourceId,
    value: &'a Value,
    level: u8,
    hash: Hash32,
    public_from: u64,
}

/// Every resolution of one fact (never empty).
struct FactState<'a> {
    answers: Vec<Answer<'a>>,
}

/// Which answers judge an entry (R-TR-2, ADR-0036 amendment 3 item 6).
enum Judges<'a, 'b> {
    /// No answer it may be judged by.
    None,
    /// The first window is still open at `as_of`: judged in a later commitment.
    Pending,
    /// The answers that judge it: the strongest level among those it may be
    /// judged by, public within [`CONTEST_WINDOW_SECS`] of the earliest of
    /// them; `at` is that earliest time.
    Set {
        level: u8,
        at: u64,
        answers: Vec<&'b Answer<'a>>,
    },
}

impl<'a> FactState<'a> {
    /// The answers that judge an entry whose source may be judged by the
    /// answers `keep` passes (not its own or its operator's). Strongest level
    /// first (a weaker answer never contests a stronger one, TR-R1-7), then
    /// the first window of that level: a later answer never changes an entry
    /// judged before it (TR-R1-1), and answers inside the window contest each
    /// other, so being first is not enough to judge alone (TR-C7-R1-6).
    fn judges<'b>(&'b self, keep: impl Fn(&Answer<'a>) -> bool, as_of: u64) -> Judges<'a, 'b> {
        let usable: Vec<&'b Answer<'a>> = self.answers.iter().filter(|a| keep(a)).collect();
        let Some(level) = usable.iter().map(|a| a.level).min() else {
            return Judges::None;
        };
        let strongest: Vec<&'b Answer<'a>> =
            usable.into_iter().filter(|a| a.level == level).collect();
        let at = strongest.iter().map(|a| a.public_from).min().unwrap_or(0);
        let close = at.saturating_add(CONTEST_WINDOW_SECS);
        if close > as_of {
            return Judges::Pending;
        }
        Judges::Set {
            level,
            at,
            answers: strongest
                .into_iter()
                .filter(|a| a.public_from <= close)
                .collect(),
        }
    }

    /// The fact counts as resolved for the metric summary: its answers (all
    /// of them) judge, and agree.
    fn uncontested(&self, as_of: u64) -> bool {
        match self.judges(|_| true, as_of) {
            Judges::Set { answers, .. } => answers.windows(2).all(|w| w[0].value == w[1].value),
            _ => false,
        }
    }
}

/// The result of one computation: every entry, record and metric summary, with their trees.
#[derive(Clone, Debug)]
pub struct TrackBook {
    as_of: u64,
    registry_root: Hash32,
    params_hash: Hash32,
    statements_hash: Hash32,
    resolutions_hash: Hash32,
    entries: Vec<Entry>,
    records: Vec<Record>,
    metrics: Vec<MetricSummary>,
    entry_tree: MerkleTree,
    record_tree: MerkleTree,
    metric_tree: MerkleTree,
}

/// The commitment to one computation, chained to the one before.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TrackCommitment {
    /// Hash of the previous commitment (zero for the first).
    pub prev: Hash32,
    /// Every statement was committed at or before this time.
    pub as_of: u64,
    /// Registry the inputs were judged against (it only grows; not compared
    /// across commitments, it never changes an outcome).
    pub registry_root: Hash32,
    /// [`TrackParams::hash`].
    pub params_hash: Hash32,
    /// [`statements_hash`] of the statement log.
    pub statements_hash: Hash32,
    /// [`resolutions_hash`] of the resolution list.
    pub resolutions_hash: Hash32,
    /// Root of the history tree (Borsh [`Entry`] rows).
    pub entries_root: Hash32,
    /// Rows in it.
    pub entry_count: u64,
    /// Root of the record tree (Borsh [`Record`] rows).
    pub records_root: Hash32,
    /// Rows in it.
    pub record_count: u64,
    /// Root of the metric tree (Borsh [`MetricSummary`] rows, by metric).
    pub metrics_root: Hash32,
    /// Rows in it.
    pub metric_count: u64,
}

impl TrackCommitment {
    /// `keccak256("onyx-track-commitment-v1" ‖ borsh(commitment))`.
    pub fn hash(&self) -> Hash32 {
        hash_with(COMMITMENT_DOMAIN, self)
    }
}

fn row<T: BorshSerialize>(t: &T) -> Vec<u8> {
    borsh::to_vec(t).unwrap_or_default()
}

/// Tally one (source, metric)'s contiguous entries into its record. The
/// `metric_*` fields are left 0 (they need every source's entries).
fn tally(source: &SourceId, metric: &MetricId, from: u64, entries: &[Entry]) -> Record {
    let mut r = Record {
        source: source.clone(),
        metric: metric.clone(),
        entries_from: from,
        entries_count: entries.len() as u64,
        agreed: 0,
        agreed_first: 0,
        disagreed: 0,
        equivocated: 0,
        late: 0,
        void: 0,
        contested: 0,
        unresolved: 0,
        self_resolved: 0,
        brier_sum_e12: 0,
        distinct_subjects: 0,
        first_committed_at: entries.iter().map(|e| e.committed_at).min().unwrap_or(0),
        last_committed_at: entries.iter().map(|e| e.committed_at).max().unwrap_or(0),
        metric_sources: 0,
        metric_resolved_facts: 0,
        baseline_entries: 0,
        baseline_source_agreed: 0,
        baseline_agreed: 0,
    };
    let mut subjects: Vec<&Subject> = Vec::new();
    for e in entries {
        match e.outcome {
            Outcome::Agreed => {
                r.agreed += 1;
                r.agreed_first += u64::from(e.first);
            }
            Outcome::Disagreed => r.disagreed += 1,
            Outcome::Equivocated => r.equivocated += 1,
            Outcome::Late => r.late += 1,
            Outcome::Void => r.void += 1,
            Outcome::Contested => r.contested += 1,
            Outcome::Unresolved => r.unresolved += 1,
            Outcome::SelfResolved => r.self_resolved += 1,
        }
        if e.outcome.counted() {
            r.brier_sum_e12 += u128::from(e.brier_e12);
            subjects.push(&e.fact.subject);
        }
        if e.baseline != Baseline::None {
            r.baseline_entries += 1;
            r.baseline_agreed += u64::from(e.baseline == Baseline::Agreed);
            r.baseline_source_agreed += u64::from(e.outcome == Outcome::Agreed);
        }
    }
    subjects.sort();
    subjects.dedup();
    r.distinct_subjects = subjects.len() as u64;
    r
}

/// The answers of each resolved fact, sorted by fact key (R-TR-2): only the
/// resolutions at the strongest level present count, so a weaker answer never
/// contests a stronger one. Whether they agree is decided per stating source,
/// without its own answers (TR-R2-1).
fn fact_states<'a>(
    resolutions: &'a [Resolution],
    rkeys: &[(Hash32, Hash32)],
) -> Vec<(Hash32, &'a MetricId, FactState<'a>)> {
    let mut facts = Vec::new();
    let mut i = 0;
    while i < resolutions.len() {
        let key = rkeys[i].0;
        let mut j = i;
        while j < resolutions.len() && rkeys[j].0 == key {
            j += 1;
        }
        let state = FactState {
            answers: (i..j)
                .map(|k| Answer {
                    resolver: &resolutions[k].resolver,
                    value: &resolutions[k].value,
                    level: resolutions[k].level.number(),
                    hash: rkeys[k].1,
                    public_from: resolutions[k].public_from,
                })
                .collect(),
        };
        facts.push((key, &resolutions[i].fact.metric, state));
        i = j;
    }
    facts
}

/// One fact a statement names: (metric, subject, as_of, earliest commit time
/// of any statement on it, fact key), sorted by the first three.
type Named<'a> = (&'a MetricId, &'a Subject, u64, u64, Hash32);

/// Facts named by a statement committed at or before the fact's own close
/// (on time): a `Late` statement never makes a baseline (review TR-C9-R1-1,
/// TR-C9-R1-2: a held and late-sealed statement cannot choose one).
fn named_facts<'a>(
    params: &TrackParams,
    statements: &'a [Statement],
    skeys: &[(&SourceId, &MetricId, Hash32, Hash32)],
) -> Vec<Named<'a>> {
    let mut v: Vec<Named<'a>> = statements
        .iter()
        .zip(skeys)
        .filter(|(s, _)| {
            params
                .close_of(&s.fact)
                .is_some_and(|c| s.committed_at <= c)
        })
        .map(|(s, k)| {
            (
                &s.fact.metric,
                &s.fact.subject,
                s.fact.as_of,
                s.committed_at,
                k.2,
            )
        })
        .collect();
    v.sort();
    // Keep the earliest commit per fact (sorted, so the first of each).
    v.dedup_by(|b, a| (a.0, a.1, a.2) == (b.0, b.1, b.2));
    v
}

/// The persistence baseline of `source`'s entry on `fact` closing at `close`
/// (R-TR-11): the last value known at `close`, for a period rule only (point
/// facts have no grid, so "the last value" is not defined). Its fact is the
/// one period of the same metric and subject that ended last at or before
/// `close` (`as_of' = close - close % period`), if a statement named it on
/// time and the answers public by `close`, not `source`'s or its operator's,
/// judge it uncontested (contest window passed by `close`); its value. `None`
/// otherwise: there is no fallback to an older period, so a source cannot
/// choose a staler baseline by leaving the latest one unnamed (reviews
/// TR-C9-R1-2, TR-C9-R2-1). One fact is looked at. Every input is frozen
/// once the entry is judged: that fact's reveal (its `as_of`) is at or before
/// `close`, so R-TR-8 admits no later statement on it, and no later answer is
/// public by `close` on a fact the earlier log named.
fn persistence<'a, 'b>(
    params: &TrackParams,
    named: &[Named<'_>],
    state_of: &impl Fn(&Hash32) -> Option<&'b FactState<'a>>,
    source: &SourceId,
    fact: &Fact,
    close: u64,
) -> Option<&'a Value>
where
    'a: 'b,
{
    let Close::PeriodStart { period_secs, .. } = params.rule(&fact.metric)?.close else {
        return None;
    };
    let latest = close.checked_sub(close.checked_rem(period_secs)?)?;
    if latest >= fact.as_of {
        return None;
    }
    let key = (&fact.metric, &fact.subject, latest);
    let at = named.partition_point(|n| (n.0, n.1, n.2) < key);
    let n = named.get(at).filter(|n| (n.0, n.1, n.2) == key)?;
    let keep = |a: &Answer<'_>| a.public_from <= close && !params.same_operator(a.resolver, source);
    match state_of(&n.4)?.judges(keep, close) {
        Judges::Set { answers, .. } if answers.windows(2).all(|w| w[0].value == w[1].value) => {
            answers.first().map(|a| a.value)
        }
        _ => None,
    }
}

/// Compute every entry, record and metric summary (ADR-0036 items 3-8).
///
/// `statements` must be strictly ascending by (source, metric, fact key,
/// statement hash) and `resolutions` by (fact key, resolution hash)
/// ([`sort_statements`], [`sort_resolutions`]); every statement must be
/// committed at or before `as_of`.
pub fn compute(
    params: &TrackParams,
    reg: &dyn Registry,
    statements: &[Statement],
    resolutions: &[Resolution],
    as_of: u64,
) -> Result<TrackBook, TrackError> {
    params.validate(reg)?;
    if statements.len() > MAX_ITEMS || resolutions.len() > MAX_ITEMS {
        return Err(TrackError::TooMany);
    }
    // Statements: admissible and canonical.
    let mut closes = Vec::with_capacity(statements.len());
    for s in statements {
        let close = admissible(params, reg, s)?;
        if s.committed_at > as_of {
            return Err(TrackError::FromTheFuture);
        }
        closes.push(close);
    }
    let skeys: Vec<_> = statements.iter().map(Statement::order_key).collect();
    if !skeys.windows(2).all(|w| w[0] < w[1]) {
        return Err(TrackError::NotCanonical("statements"));
    }
    // Resolutions: admissible and canonical.
    for r in resolutions {
        if !params.accepts(r.level) {
            return Err(TrackError::LevelNotAccepted(r.level.number()));
        }
        if r.evidence == Hash32::ZERO {
            return Err(TrackError::ZeroEvidence);
        }
        if r.resolver.is_estimate() || r.fact.metric.is_estimate() {
            return Err(TrackError::EstimateResolution);
        }
        // An answer is in a commitment only once it is public (TR-R2-2).
        if r.public_from > as_of {
            return Err(TrackError::FromTheFuture);
        }
        check_source(reg, &r.resolver)?;
        check_fact(params, reg, &r.fact, &r.value)?;
    }
    let rkeys: Vec<_> = resolutions.iter().map(Resolution::order_key).collect();
    if !rkeys.windows(2).all(|w| w[0] < w[1]) {
        return Err(TrackError::NotCanonical("resolutions"));
    }
    let facts = fact_states(resolutions, &rkeys);
    let state_of = |key: &Hash32| {
        facts
            .binary_search_by(|f| f.0.cmp(key))
            .ok()
            .map(|i| &facts[i].2)
    };

    // Every fact a statement names, with its earliest commit time, by
    // (metric, subject, as_of): where an entry's persistence baseline is
    // looked up (C9, R-TR-11).
    let named = named_facts(params, statements, &skeys);

    // One entry per (source, fact) (R-TR-3), with the time its judging
    // answers became usable (`u64::MAX` when none judges it).
    let mut entries: Vec<Entry> = Vec::new();
    let mut judged_at: Vec<u64> = Vec::new();
    let mut i = 0;
    while i < statements.len() {
        let (source, _, key, _) = skeys[i];
        let mut j = i;
        while j < statements.len() && skeys[j].0 == source && skeys[j].2 == key {
            j += 1;
        }
        let group = &statements[i..j];
        // Earliest statement; ties go to the lowest hash (canonical order).
        let mut first = 0;
        for (k, s) in group.iter().enumerate() {
            if s.committed_at < group[first].committed_at {
                first = k;
            }
        }
        let earliest = &group[first];
        let close = closes[i];
        let tolerance = params
            .rule(&earliest.fact.metric)
            .map_or(0, |r| r.tolerance);
        let same = group
            .iter()
            .all(|s| s.value == earliest.value && s.confidence == earliest.confidence);
        let (mut outcome, mut resolution, mut level) = (Outcome::Unresolved, Hash32::ZERO, 0u8);
        let mut judged_value: Option<&Value> = None;
        let mut judged = u64::MAX;
        if earliest.committed_at > close {
            outcome = Outcome::Late;
        } else {
            if let Some(state) = state_of(&key) {
                // Never judged, and never contested, by its own answer
                // (TR-R1-7, TR-R2-1).
                // Nor by an affiliate's: one operator, one source (TR-R2-4).
                // Only the answers usable first judge it (amendment 3 item 6):
                // an answer that arrives later never changes an entry already
                // judged, and judges one its own or its affiliates' earlier
                // answers left self-resolved (TR-C7-4).
                match state.judges(|a| !params.same_operator(a.resolver, source), as_of) {
                    Judges::None => outcome = Outcome::SelfResolved,
                    Judges::Pending => {}
                    Judges::Set { answers, .. }
                        if answers.iter().any(|b| b.value != answers[0].value) =>
                    {
                        outcome = Outcome::Contested;
                    }
                    Judges::Set {
                        level: l,
                        at,
                        answers,
                    } => {
                        let a = answers[0];
                        let value = a.value;
                        resolution = a.hash;
                        level = l;
                        let public_from = at;
                        judged = public_from;
                        outcome = if public_from <= close {
                            Outcome::Void
                        } else if !same {
                            Outcome::Equivocated
                        } else if agrees(&earliest.value, value, tolerance) {
                            Outcome::Agreed
                        } else {
                            Outcome::Disagreed
                        };
                        judged_value = Some(value);
                    }
                }
            }
        }
        let baseline = match judged_value {
            Some(v) if outcome.counted() => {
                persistence(params, &named, &state_of, source, &earliest.fact, close).map_or(
                    Baseline::None,
                    |last| {
                        if agrees(last, v, tolerance) {
                            Baseline::Agreed
                        } else {
                            Baseline::Disagreed
                        }
                    },
                )
            }
            _ => Baseline::None,
        };
        entries.push(Entry {
            source: source.clone(),
            fact: earliest.fact.clone(),
            outcome,
            statement: skeys[i + first].3,
            statements: group.len() as u64,
            committed_at: earliest.committed_at,
            confidence: earliest.confidence,
            resolution,
            resolution_level: level,
            first: false,
            brier_e12: brier_term(outcome, earliest.confidence),
            baseline,
        });
        judged_at.push(judged);
        i = j;
    }

    // First to agree (R-TR-5): an agreed entry is first unless another agreed
    // entry on the fact was committed earlier and judged no later (so an entry
    // judged in a later commitment never takes `first` from one judged before).
    let mut agreed: Vec<(Hash32, u64, u64, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.outcome == Outcome::Agreed)
        .map(|(k, e)| (e.fact.key(), judged_at[k], e.committed_at, k))
        .collect();
    agreed.sort();
    let mut g = 0;
    while g < agreed.len() {
        let mut h = g;
        while h < agreed.len() && agreed[h].0 == agreed[g].0 {
            h += 1;
        }
        // Sweep the fact's agreed entries by judging time, a tie block at a time.
        let mut earliest_commit = u64::MAX;
        let mut b = g;
        while b < h {
            let mut c = b;
            while c < h && agreed[c].1 == agreed[b].1 {
                earliest_commit = earliest_commit.min(agreed[c].2);
                c += 1;
            }
            for item in &agreed[b..c] {
                entries[item.3].first = item.2 <= earliest_commit;
            }
            b = c;
        }
        g = h;
    }

    // Records per (source, metric); entries are already in that order.
    let mut records: Vec<Record> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let mut j = i;
        while j < entries.len()
            && entries[j].source == entries[i].source
            && entries[j].fact.metric == entries[i].fact.metric
        {
            j += 1;
        }
        records.push(tally(
            &entries[i].source,
            &entries[i].fact.metric,
            i as u64,
            &entries[i..j],
        ));
        i = j;
    }
    // Context per metric: how many sources, how many resolved facts.
    let mut metric_ids: Vec<&MetricId> = records.iter().map(|r| &r.metric).collect();
    let mut resolved_per: Vec<&MetricId> = facts
        .iter()
        .filter(|f| f.2.uncontested(as_of))
        .map(|f| f.1)
        .collect();
    metric_ids.sort();
    resolved_per.sort();
    let count = |v: &[&MetricId], m: &MetricId| {
        (v.partition_point(|x| *x <= m) - v.partition_point(|x| *x < m)) as u64
    };
    let mut all_metrics = metric_ids.clone();
    all_metrics.extend(resolved_per.iter().copied());
    all_metrics.sort();
    all_metrics.dedup();
    let metrics: Vec<MetricSummary> = all_metrics
        .iter()
        .map(|m| MetricSummary {
            metric: (*m).clone(),
            sources: count(&metric_ids, m),
            resolved_facts: count(&resolved_per, m),
        })
        .collect();
    for r in records.iter_mut() {
        if let Ok(k) = metrics.binary_search_by(|s| s.metric.cmp(&r.metric)) {
            r.metric_sources = metrics[k].sources;
            r.metric_resolved_facts = metrics[k].resolved_facts;
        }
    }

    let tree = |rows: Vec<Vec<u8>>| MerkleTree::from_rows(&rows);
    Ok(TrackBook {
        as_of,
        registry_root: reg.root(),
        params_hash: params.hash(),
        statements_hash: statements_hash(statements),
        resolutions_hash: resolutions_hash(resolutions),
        entry_tree: tree(entries.iter().map(row).collect()),
        record_tree: tree(records.iter().map(row).collect()),
        metric_tree: tree(metrics.iter().map(row).collect()),
        entries,
        records,
        metrics,
    })
}

impl TrackBook {
    /// The book of committed rows, without a registry (ROADMAP C20: a node
    /// serving a coordinator's publication has no registry to run [`compute`]
    /// with). `Some` only when the rows' trees, counts and the commitment's
    /// other fields give back exactly `c`; the rows are then what `c` commits
    /// to, and a client that holds the registry reruns [`compute`] on the
    /// served logs to check that they are also what the logs give.
    pub fn from_committed(
        c: &TrackCommitment,
        entries: Vec<Entry>,
        records: Vec<Record>,
        metrics: Vec<MetricSummary>,
    ) -> Option<TrackBook> {
        let tree = |rows: Vec<Vec<u8>>| MerkleTree::from_rows(&rows);
        let book = TrackBook {
            as_of: c.as_of,
            registry_root: c.registry_root,
            params_hash: c.params_hash,
            statements_hash: c.statements_hash,
            resolutions_hash: c.resolutions_hash,
            entry_tree: tree(entries.iter().map(row).collect()),
            record_tree: tree(records.iter().map(row).collect()),
            metric_tree: tree(metrics.iter().map(row).collect()),
            entries,
            records,
            metrics,
        };
        // Lookups binary-search the rows: they must be in the computed order.
        let sorted = book
            .records
            .windows(2)
            .all(|w| (&w[0].source, &w[0].metric) < (&w[1].source, &w[1].metric))
            && book.metrics.windows(2).all(|w| w[0].metric < w[1].metric);
        (sorted && book.commitment(c.prev) == *c).then_some(book)
    }

    /// Every (source, fact) entry, sorted by (source, metric, fact key).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every (source, metric) record, sorted by (source, metric).
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Every metric summary, sorted by metric.
    pub fn metrics(&self) -> &[MetricSummary] {
        &self.metrics
    }

    /// The record of (source, metric), with its index.
    pub fn record(&self, source: &SourceId, metric: &MetricId) -> Option<(u64, &Record)> {
        self.records
            .binary_search_by(|r| (&r.source, &r.metric).cmp(&(source, metric)))
            .ok()
            .map(|i| (i as u64, &self.records[i]))
    }

    /// The summary of `metric`, with its index.
    pub fn metric(&self, metric: &MetricId) -> Option<(u64, &MetricSummary)> {
        self.metrics
            .binary_search_by(|s| s.metric.cmp(metric))
            .ok()
            .map(|i| (i as u64, &self.metrics[i]))
    }

    /// Proof of record `index` against `records_root`.
    pub fn record_proof(&self, index: u64) -> Option<Proof> {
        self.record_tree.proof(index)
    }

    /// Proof of entry `index` against `entries_root`.
    pub fn entry_proof(&self, index: u64) -> Option<Proof> {
        self.entry_tree.proof(index)
    }

    /// Proof of metric summary `index` against `metrics_root`.
    pub fn metric_proof(&self, index: u64) -> Option<Proof> {
        self.metric_tree.proof(index)
    }

    /// The commitment, chained to `prev` (zero for the first).
    pub fn commitment(&self, prev: Hash32) -> TrackCommitment {
        TrackCommitment {
            prev,
            as_of: self.as_of,
            registry_root: self.registry_root,
            params_hash: self.params_hash,
            statements_hash: self.statements_hash,
            resolutions_hash: self.resolutions_hash,
            entries_root: self.entry_tree.root(),
            entry_count: self.entry_tree.leaf_count(),
            records_root: self.record_tree.root(),
            record_count: self.record_tree.leaf_count(),
            metrics_root: self.metric_tree.root(),
            metric_count: self.metric_tree.leaf_count(),
        }
    }
}

/// Why a client refused a record or its history.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The record is not under `records_root` at the proof's index, or its
    /// counts are out of bounds.
    #[error("record not in the commitment")]
    Record,
    /// The metric summary is not under `metrics_root` or does not match the record.
    #[error("metric summary not in the commitment or not the record's")]
    Summary,
    /// An entry is missing, out of place, out of order, not under
    /// `entries_root`, or not this record's.
    #[error("entry {0} not in the record's range")]
    Entry(u64),
    /// The range is not closed: a neighbour is missing or belongs to the same record.
    #[error("history range not complete")]
    Incomplete,
    /// The entries do not add up to the record.
    #[error("recount differs from the record")]
    Recount,
}

/// A row and its proof.
pub type Proven<'a, T> = (&'a T, &'a Proof);

fn entry_at(c: &TrackCommitment, (e, p): Proven<'_, Entry>, index: u64) -> bool {
    p.index == index
        && p.leaf_count == c.entry_count
        && onyx_merkle::verify(&c.entries_root, &row(e), p)
}

/// Check one record against a commitment (ADR-0036 item 10a). Its counts must
/// be within what one computation can produce (TR-R1-9).
pub fn verify_record(c: &TrackCommitment, (r, p): Proven<'_, Record>) -> bool {
    r.in_bounds()
        && p.leaf_count == c.record_count
        && onyx_merkle::verify(&c.records_root, &row(r), p)
}

/// Check a metric summary against a commitment.
pub fn verify_summary(c: &TrackCommitment, (s, p): Proven<'_, MetricSummary>) -> bool {
    p.leaf_count == c.metric_count && onyx_merkle::verify(&c.metrics_root, &row(s), p)
}

/// Check a record and its whole history (ADR-0036 item 10b): its metric's
/// committed summary matches it, every entry of its range is given in order
/// with proofs and strictly ascending fact keys, the entries just outside the
/// range (`None` only at the tree's ends) belong to another (source, metric),
/// and a recount equals the record. This proves the record adds up over its
/// committed history; that each entry's outcome is right needs a rerun of
/// [`compute`] on the published logs (item 10c).
pub fn verify_history(
    c: &TrackCommitment,
    record: Proven<'_, Record>,
    summary: Proven<'_, MetricSummary>,
    entries: &[Proven<'_, Entry>],
    before: Option<Proven<'_, Entry>>,
    after: Option<Proven<'_, Entry>>,
) -> Result<(), VerifyError> {
    let r = record.0;
    if !verify_record(c, record) {
        return Err(VerifyError::Record);
    }
    let s = summary.0;
    if !verify_summary(c, summary)
        || s.metric != r.metric
        || s.sources != r.metric_sources
        || s.resolved_facts != r.metric_resolved_facts
    {
        return Err(VerifyError::Summary);
    }
    let from = r.entries_from;
    let end = from
        .checked_add(r.entries_count)
        .ok_or(VerifyError::Incomplete)?;
    if r.entries_count == 0 || entries.len() as u64 != r.entries_count || end > c.entry_count {
        return Err(VerifyError::Incomplete);
    }
    let mine = |e: &Entry| e.source == r.source && e.fact.metric == r.metric;
    let mut last_key: Option<Hash32> = None;
    for (k, pe) in entries.iter().enumerate() {
        let index = from + k as u64;
        let key = pe.0.fact.key();
        if !entry_at(c, *pe, index) || !mine(pe.0) || last_key.is_some_and(|l| l >= key) {
            return Err(VerifyError::Entry(index));
        }
        last_key = Some(key);
    }
    let outside = |n: Option<Proven<'_, Entry>>, index: Option<u64>| match (n, index) {
        (None, None) => true,
        (Some(pe), Some(i)) => entry_at(c, pe, i) && !mine(pe.0),
        _ => false,
    };
    let before_index = from.checked_sub(1);
    let after_index = (end < c.entry_count).then_some(end);
    if !outside(before, before_index) || !outside(after, after_index) {
        return Err(VerifyError::Incomplete);
    }
    let rows: Vec<Entry> = entries.iter().map(|(e, _)| (*e).clone()).collect();
    let mut t = tally(&r.source, &r.metric, from, &rows);
    t.metric_sources = r.metric_sources;
    t.metric_resolved_facts = r.metric_resolved_facts;
    if t != *r {
        return Err(VerifyError::Recount);
    }
    Ok(())
}

/// Why a later commitment does not extend an earlier one.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtendError {
    /// `next.prev` is not the earlier commitment's hash.
    #[error("not chained to the earlier commitment")]
    NotChained,
    /// `next.as_of` is not after the earlier `as_of`.
    #[error("as_of did not move forward")]
    TimeNotForward,
    /// A log or the params do not match the committed hash.
    #[error("log does not match its commitment")]
    LogMismatch,
    /// The later params change a rule, the resolver levels or the intake
    /// (TR-R1-2, TR-R3-3).
    #[error("params changed a committed rule")]
    ParamsChanged,
    /// An earlier statement is missing or changed (history rewritten).
    #[error("an earlier statement is missing")]
    StatementDropped,
    /// An earlier resolution is missing or changed.
    #[error("an earlier resolution is missing")]
    ResolutionDropped,
    /// A statement new to the later log was revealable already at the earlier
    /// `as_of`: committed then and closed then (backdated or withheld).
    #[error("a new statement is backdated")]
    Backdated,
    /// A resolution new to the later log was public before the earlier
    /// `as_of`, or is stronger than a fact's committed answers (TR-R1-1).
    #[error("a new resolution would rewrite a committed outcome")]
    ResolutionRewrites,
    /// The later params declare an operator for a source the earlier logs
    /// already hold (as a stating source or a resolver): grouping it now would
    /// turn committed outcomes into `SelfResolved` (TR-R2-4).
    #[error("an operator declared for a source already in the log")]
    OperatorRegrouped,
}

/// The inputs of one commitment.
#[derive(Clone, Copy, Debug)]
pub struct TrackLog<'a> {
    /// Params.
    pub params: &'a TrackParams,
    /// Statement log, canonical order.
    pub statements: &'a [Statement],
    /// Resolution list, canonical order.
    pub resolutions: &'a [Resolution],
}

fn is_subset<T: PartialEq, K: Ord>(old: &[T], new: &[T], key: impl Fn(&T) -> K) -> bool {
    let mut j = 0;
    for o in old {
        let k = key(o);
        while j < new.len() && key(&new[j]) < k {
            j += 1;
        }
        if j >= new.len() || new[j] != *o {
            return false;
        }
        j += 1;
    }
    true
}

/// Check that `next` extends `prev` (ADR-0036 item 9): chained, later, the
/// same params (rules may only be added), every earlier statement and
/// resolution kept unchanged, every new statement revealable only after
/// `prev.as_of` (`max(committed_at, close) > prev.as_of`: sealed until its
/// close or made later, amendment 1 item 4; the intake's [`intake::verify_log`]
/// bounds `committed_at` itself), no operator declared for a source the
/// earlier logs hold, and every new resolution public after `prev.as_of`
/// (unless no earlier statement or resolution names its fact) and, for a fact
/// `prev` resolved, no stronger than its weakest committed answer (amendment
/// 3 item 6). So no entry `prev` judged can change except by the source's own
/// later statement. Logs must be in canonical order.
pub fn check_extends(
    prev: &TrackCommitment,
    prev_log: TrackLog<'_>,
    next: &TrackCommitment,
    next_log: TrackLog<'_>,
) -> Result<(), ExtendError> {
    if next.prev != prev.hash() {
        return Err(ExtendError::NotChained);
    }
    if next.as_of <= prev.as_of {
        return Err(ExtendError::TimeNotForward);
    }
    let matches = |c: &TrackCommitment, l: &TrackLog<'_>| {
        l.params.hash() == c.params_hash
            && statements_hash(l.statements) == c.statements_hash
            && resolutions_hash(l.resolutions) == c.resolutions_hash
    };
    if !matches(prev, &prev_log) || !matches(next, &next_log) {
        return Err(ExtendError::LogMismatch);
    }
    if !next_log.params.extends(prev_log.params) {
        return Err(ExtendError::ParamsChanged);
    }
    // An operator may be declared only for a source the earlier logs never
    // held: a grouping must never reach back into committed outcomes.
    let mut held: Vec<&SourceId> = prev_log
        .statements
        .iter()
        .map(|s| &s.source)
        .chain(prev_log.resolutions.iter().map(|r| &r.resolver))
        .collect();
    held.sort();
    held.dedup();
    if next_log.params.operators.iter().any(|o| {
        prev_log.params.operator_of(&o.source).is_none() && held.binary_search(&&o.source).is_ok()
    }) {
        return Err(ExtendError::OperatorRegrouped);
    }
    let skey = |s: &Statement| {
        let (a, b, c, d) = s.order_key();
        (a.clone(), b.clone(), c, d)
    };
    if !is_subset(prev_log.statements, next_log.statements, skey) {
        return Err(ExtendError::StatementDropped);
    }
    if !is_subset(
        prev_log.resolutions,
        next_log.resolutions,
        Resolution::order_key,
    ) {
        return Err(ExtendError::ResolutionDropped);
    }
    // Statements new to `next`: revealable only after `prev.as_of` (sealed
    // until the close, amendment 1 item 4). Without a rule the close is the
    // commit time itself (fail closed: compute refuses such a log anyway).
    // Only with an intake pinned: without one `committed_at` is caller input
    // and nothing bounds it, so the pre-C5 rule `committed_at > prev.as_of`
    // stands (review TR-R4-1: else a statement dated into a committed period
    // could be added once the answer is public).
    let mut old: Vec<Hash32> = prev_log.statements.iter().map(Statement::hash).collect();
    old.sort();
    let sealed = next_log.params.intake != Hash32::ZERO;
    let reveal = |s: &Statement| {
        let at = match next_log.params.reveal_of(&s.fact) {
            Some(c) if sealed => c,
            _ => s.committed_at,
        };
        s.committed_at.max(at)
    };
    if next_log
        .statements
        .iter()
        .any(|s| old.binary_search(&s.hash()).is_err() && reveal(s) <= prev.as_of)
    {
        return Err(ExtendError::Backdated);
    }
    // Resolutions new to `next` (amendment 3 item 6). For a fact `prev`
    // resolved: public after `prev.as_of` and no stronger than its weakest
    // committed answer, so no entry `prev` judged can change (it was judged by
    // answers of a level at most that weak whose window closed by
    // `prev.as_of`; TR-R1-1). For a fact `prev`'s statements named: public
    // after `prev.as_of` (an earlier one was withheld). For a fact no earlier
    // statement named, any time: `prev` holds no entry on it, so nothing
    // committed changes (TR-C7-R1-1).
    let mut old_r: Vec<(Hash32, Hash32, u8)> = prev_log
        .resolutions
        .iter()
        .map(|r| {
            let (k, h) = r.order_key();
            (k, h, r.level.number())
        })
        .collect();
    old_r.sort();
    let weakest_before = |fact: &Hash32| {
        let at = old_r.partition_point(|(k, _, _)| k < fact);
        old_r[at..]
            .iter()
            .take_while(|(k, _, _)| k == fact)
            .map(|(_, _, l)| *l)
            .max()
    };
    let mut named: Vec<Hash32> = prev_log.statements.iter().map(|s| s.fact.key()).collect();
    named.sort();
    named.dedup();
    for r in next_log.resolutions {
        let (fact, h) = r.order_key();
        if old_r
            .binary_search_by(|(k, x, _)| (k, x).cmp(&(&fact, &h)))
            .is_ok()
        {
            continue;
        }
        let weakest = weakest_before(&fact);
        let stronger = weakest.is_some_and(|l| r.level.number() < l);
        let known = weakest.is_some() || named.binary_search(&fact).is_ok();
        if stronger || (known && r.public_from <= prev.as_of) {
            return Err(ExtendError::ResolutionRewrites);
        }
    }
    Ok(())
}

pub mod chain;
pub mod chained;
pub mod daily;
pub mod example;
pub mod intake;
pub mod publication;
#[cfg(feature = "wire")]
pub mod wire;

#[cfg(test)]
mod tests;
