//! Proof level 3, lane (a): source-signed data (ADR-0028,
//! docs/spec/proof-ladder.md "Level 3").
//!
//! [`SignedSources`] is the coordinator's registry of source keys and its
//! book of signed reports. It is the one intake for
//! `VerificationMethod::SignedSource` claims ([`SignedSources::accept_claim`],
//! [`SignedSources::decode_claim`]): it verifies the EIP-191 signature against
//! a registered key of the claim's source, refuses replays and stale or future
//! reports, binds every claim field to the signed payload, and keeps a fact
//! book (ADR-0028 amendments 1 to 3):
//!
//! - A key has a **life cycle** that only moves forward, `Active` ->
//!   `Retired` -> `Compromised` (any state may jump to `Compromised`), and a
//!   separate **suspension** flag set by an equivocation. Reinstating clears
//!   the flag and never touches the life cycle, so no reviewer action can
//!   bring a retired or compromised key back (L3R2-F1).
//! - A fact is (source, metric, subject, observed time). It is `Accepted`
//!   (one report backs it) or `Disputed` (the source's keys signed different
//!   values for it; no value counts). The same value again is a harmless
//!   restatement and is not stored. A different value disputes the fact,
//!   withdraws the accepted report and suspends every key involved; a key
//!   that joins an open dispute with a value differing from any report in it
//!   is suspended too (L3R3-F2).
//! - Marking a key compromised withdraws what it stated from the compromise
//!   point on. Nothing is restored automatically: a dispute stays disputed
//!   and a suspended key stays suspended until a reviewer acts (amendment 3,
//!   L3R3-F1, F3). Only [`SignedSources::resolve_fact`] and
//!   [`SignedSources::reinstate_key`] clear them.
//! - Every registry action (registering, retiring, compromising a key) and
//!   every recovery needs a [`Reviewer`] issued with the store and is
//!   written to an append-only decision log (L3R2-F2, F6, L3R3-F4).
//! - Consumers settle only on [`SignedSources::settleable`] facts: accepted
//!   for at least `DISPUTE_WINDOW_SECS` (L3R2-F7).

use borsh::BorshDeserialize;
use onyx_core::signed::{SignedFormError, REPORT_EVIDENCE};
use onyx_core::{
    from_canonical_bytes, keccak256, CanonicalError, Claim, ClaimError, EvidenceRef, Hash32,
    ProofLevel, Registry, SignedPayload, SignedReport, Snapshot, SourceId, SourceKey,
    VerificationMethod,
};
use onyx_labels::attest::eip191_recover;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// Most seconds a report's observation time may lie ahead of the
/// coordinator's clock.
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;
/// Oldest report the intake takes in, in seconds before now (7 days). Bounds
/// how long a stolen key's backdated reports could be fed in, and keeps the
/// sequence book useful.
pub const MAX_REPORT_AGE_SECS: u64 = 7 * 86_400;
/// How long around a key's retirement its reports still count (1 hour): a
/// retired key's report must be observed in `[at - grace, at)` and arrive
/// before `at + grace`. Enough to deliver reports signed just before the
/// rotation; a retired key stolen later can neither sign new facts nor
/// backdate a week (L3R-F1, L3R2-F4).
pub const RETIRED_GRACE_SECS: u64 = 3_600;
/// Sequence numbers are `0..u64::MAX`; the top value is reserved (L3R-F8).
pub const RESERVED_SEQUENCE: u64 = u64::MAX;
/// How long an accepted fact must stand before a consumer may settle on it
/// (1 hour): time for a contradicting report to arrive and dispute it
/// (L3R2-F7).
pub const DISPUTE_WINDOW_SECS: u64 = 3_600;
/// Reports observed longer ago than this are refused as stale by every
/// intake path, so their sequence and report entries can be dropped
/// (evidence excepted, L3R2-F5).
pub const PRUNE_AFTER_SECS: u64 = MAX_REPORT_AGE_SECS + MAX_CLOCK_SKEW_SECS;

/// Life cycle of a registered key. It only moves forward (`Active` ->
/// `Retired` -> `Compromised`, or straight to `Compromised`); suspension is a
/// separate flag ([`KeyState::suspended_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStatus {
    /// Signs reports.
    Active,
    /// Rotated out at `at`: only reports observed in `[at - grace, at)` count,
    /// and only until `at + RETIRED_GRACE_SECS` (coordinator clock).
    Retired {
        /// First refused observation time.
        at: u64,
    },
    /// Compromised: no report signed by it is accepted any more, whatever its
    /// stated time (a thief can backdate `observed_unix`).
    Compromised {
        /// When the compromise is believed to have started.
        since: u64,
        /// When it was first recorded here.
        recorded_at: u64,
    },
}

impl KeyStatus {
    /// Position in the life cycle; it never decreases.
    pub fn rank(&self) -> u8 {
        match self {
            KeyStatus::Active => 0,
            KeyStatus::Retired { .. } => 1,
            KeyStatus::Compromised { .. } => 2,
        }
    }
}

/// A key's life cycle and suspension, as the store holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyState {
    /// Life cycle.
    pub status: KeyStatus,
    /// Set by an equivocation, cleared only by a reviewer's
    /// [`SignedSources::reinstate_key`]; independent of the life cycle.
    pub suspended_at: Option<u64>,
}

/// Why a key is suspended: the disputed facts and sequence equivocations it
/// took part in (a record for the reviewer). Nothing clears a suspension but
/// a reviewer's reinstatement (L3R3-F1).
#[derive(Clone, Debug)]
struct Suspension {
    at: u64,
    causes: BTreeSet<Hash32>,
}

/// What the store remembers about one key.
#[derive(Clone, Debug)]
struct KeyEntry {
    key: SourceKey,
    status: KeyStatus,
    suspension: Option<Suspension>,
}

/// A validly signed report the store has seen (accepted or not), as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Recorded {
    report: SignedReport,
    recorded_at: u64,
}

/// An accepted report, as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// The report.
    pub report: SignedReport,
    /// When the intake (or a resolution) accepted it (coordinator clock).
    pub accepted_at: u64,
}

/// What the intake returns for an accepted claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intake {
    /// Always `ProofLevel::SourceProven`.
    pub level: ProofLevel,
    /// The fact the report states ([`signed_fact_key`]).
    pub fact: Hash32,
    /// The accepted report's hash.
    pub report: Hash32,
}

/// Where a fact stands in the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactStatus {
    /// One report backs the fact.
    Accepted(Hash32),
    /// The source's keys signed different values for the fact: no value
    /// counts until it resolves. Holds the conflicting reports, at most one
    /// per key, in arrival order.
    Disputed(Vec<Hash32>),
}

/// Two signed payloads that contradict each other: portable evidence that the
/// source (or whoever holds its keys) signed both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Equivocation {
    /// The report seen first.
    pub first: SignedReport,
    /// The contradicting report.
    pub second: SignedReport,
    /// Same sequence, or same fact (source, metric, subject, time).
    pub kind: EquivocationKind,
}

/// Which rule an equivocation broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquivocationKind {
    /// One key, one sequence number, two payloads.
    Sequence,
    /// One source, one fact, two values (across any of its keys).
    Fact,
}

/// The capability every reviewer action needs (L3R2-F6). Issued only with
/// its store ([`SignedSources::with_reviewer`]); not `Clone`, not
/// constructible elsewhere, and refused by any other store.
#[derive(Debug)]
pub struct Reviewer {
    store: u64,
    name: String,
}

impl Reviewer {
    /// The name recorded with this reviewer's decisions.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// One entry of the append-only decision log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// The reviewer's name.
    pub by: String,
    /// Coordinator time of the decision.
    pub at: u64,
    /// What was decided.
    pub action: DecisionAction,
    /// The reviewer's reason.
    pub reason: String,
}

/// A recorded decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionAction {
    /// Key registered (L3R3-F4).
    Register {
        /// Key id.
        key: Hash32,
        /// The source it signs for.
        source: SourceId,
    },
    /// Key retired at `at`.
    Retire {
        /// Key id.
        key: Hash32,
        /// Retirement time.
        at: u64,
        /// Accepted reports observed at or after `at`, withdrawn to
        /// re-review (L3R5-F1).
        withdrawn: Vec<Hash32>,
    },
    /// Key marked compromised from `since` (the effective, earliest value).
    Compromise {
        /// Key id.
        key: Hash32,
        /// Compromise start.
        since: u64,
        /// Accepted reports withdrawn to re-review.
        withdrawn: Vec<Hash32>,
    },
    /// Suspension cleared by a reviewer.
    Reinstate {
        /// Key id.
        key: Hash32,
    },
    /// Disputed fact settled by a reviewer (`None`: cleared).
    ResolveFact {
        /// Fact.
        fact: Hash32,
        /// Report kept, if any.
        keep: Option<Hash32>,
    },
}

/// Error from the level-3 registry or intake.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignedError {
    /// The claim is malformed or its rights are not the registry's.
    #[error(transparent)]
    Claim(#[from] ClaimError),
    /// Malformed key, payload or report.
    #[error(transparent)]
    Form(#[from] SignedFormError),
    /// Not a `SignedSource` claim.
    #[error("not a signed-source claim")]
    NotSignedSource,
    /// The key's source is not in the registry.
    #[error("source not registered: {0}")]
    UnknownSource(String),
    /// A key with this id is already registered.
    #[error("key already registered")]
    KeyAlreadyRegistered,
    /// The source's keys were published under another origin; a different
    /// origin for the same source needs a new registry review.
    #[error("key origin differs from the source's registered keys")]
    OriginMismatch,
    /// The origin already publishes keys for another source (L3R-F7).
    #[error("origin already publishes keys for another source")]
    OriginTaken,
    /// This signer (private key) is already registered, under this or another
    /// source, in any life-cycle state: one signer, one registration, ever
    /// (L3R4-F1).
    #[error("signer already registered")]
    SignerTaken,
    /// No key with this id.
    #[error("unknown key")]
    UnknownKey,
    /// The key does not sign for the payload's source.
    #[error("key does not belong to the payload's source")]
    KeySourceMismatch,
    /// The key is retired and the report is outside its window.
    #[error("key retired")]
    KeyRetired,
    /// The key is marked compromised.
    #[error("key compromised")]
    KeyCompromised,
    /// The key is suspended after an equivocation.
    #[error("key suspended after an equivocation")]
    KeySuspended,
    /// The key is not suspended (nothing to reinstate).
    #[error("key not suspended")]
    NotSuspended,
    /// Only an active key can be retired; the life cycle never goes back.
    #[error("key is not active")]
    NotActive,
    /// A retirement or compromise time after now.
    #[error("time is in the future")]
    TimeInFuture,
    /// The capability was not issued with this store.
    #[error("not a reviewer of this store")]
    NotReviewer,
    /// The signature does not recover to the key's signer.
    #[error("signature does not match the key")]
    BadSignature,
    /// Observed more than `MAX_REPORT_AGE_SECS` ago.
    #[error("report too old")]
    Stale,
    /// Observed more than `MAX_CLOCK_SKEW_SECS` in the future.
    #[error("report from the future")]
    FromFuture,
    /// The sequence `u64::MAX` is reserved.
    #[error("sequence reserved")]
    SequenceReserved,
    /// This (key, sequence) already carries this exact payload.
    #[error("sequence already used for this payload")]
    Replay,
    /// The source already stated this fact with this value: a harmless
    /// restatement, not stored.
    #[error("fact already stated with this value")]
    AlreadyStated,
    /// The fact is disputed; recorded as evidence, nothing accepted.
    #[error("fact disputed: waiting for re-review")]
    FactDisputed,
    /// The fact is not disputed (nothing to resolve).
    #[error("fact not disputed")]
    NotDisputed,
    /// The chosen report is not one of the dispute's, or its key may not
    /// sign it now (life cycle or suspension).
    #[error("report cannot resolve this dispute")]
    BadResolution,
    /// The source signed a different payload for the same sequence or fact;
    /// recorded, the fact disputed and the keys involved suspended.
    #[error("equivocation: the source signed two different payloads")]
    Equivocation {
        /// The fact the contradiction is about (the second report's).
        fact: Hash32,
        /// Accepted reports withdrawn by it.
        withdrawn: Vec<Hash32>,
    },
    /// The claim does not match the signed payload.
    #[error("claim not backed by the signed report: {0}")]
    ClaimNotBacked(&'static str),
    /// Bytes are not a canonical encoding.
    #[error("not a canonical encoding")]
    NotCanonical,
    /// `now` is earlier than a time this store already acted at: the clock
    /// is chain time (the latest block's timestamp) and never steps back
    /// (L3R-F8).
    #[error("clock went back: {now} is before {clock}")]
    ClockWentBack {
        /// The time given.
        now: u64,
        /// The latest time the store acted at.
        clock: u64,
    },
}

/// Keccak-256 of `"onyx-signed-fact-v1" ‖ borsh(source, metric, subject,
/// observed_unix)`: the fact a signed report states.
pub fn signed_fact_key(p: &SignedPayload) -> Hash32 {
    let mut b = b"onyx-signed-fact-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&(&p.source, &p.metric, &p.subject, p.observed_unix)).unwrap_or_default(),
    );
    keccak256(&b)
}

/// The suspension cause of a sequence equivocation (one key, one sequence,
/// two payloads): only a reviewer clears it.
fn sequence_cause(key: &Hash32, sequence: u64) -> Hash32 {
    let mut b = b"onyx-signed-sequence-equivocation-v1".to_vec();
    b.extend_from_slice(&key.0);
    b.extend_from_slice(&sequence.to_be_bytes());
    keccak256(&b)
}

/// May `entry` sign a report observed at `observed`, arriving at `now`?
/// The single rule for the intake and for resolutions (spec R-SIG-3).
fn permits(entry: &KeyEntry, observed: u64, now: u64) -> Result<(), SignedError> {
    match entry.status {
        KeyStatus::Active => {}
        KeyStatus::Retired { at }
            if observed < at
                && observed >= at.saturating_sub(RETIRED_GRACE_SECS)
                && now < at.saturating_add(RETIRED_GRACE_SECS) => {}
        KeyStatus::Retired { .. } => return Err(SignedError::KeyRetired),
        KeyStatus::Compromised { .. } => return Err(SignedError::KeyCompromised),
    }
    if entry.suspension.is_some() {
        return Err(SignedError::KeySuspended);
    }
    Ok(())
}

static NEXT_STORE: AtomicU64 = AtomicU64::new(1);

/// The coordinator's key registry and signed-report book for level 3.
///
/// Deliberately not `Clone`: one book per coordinator, so nobody can try a
/// report against copies and keep the one they like (as `ObservationResults`,
/// L2R4-F2). Across coordinators the book of record is the chain:
/// `contracts/src/OnyxSignedBook.sol` holds the same key registry, sequence,
/// payload and fact books on block time, and gives this store's results on
/// every differential trace (ADR-0028 amendment 6, THREATS L3-F7;
/// `tests/level3_chain_book.rs`). This store is the coordinator's pre-check
/// and model; what a consumer settles on is the chain's `settleable`.
///
/// ```compile_fail
/// fn copy(s: &onyx_coordinator::signed::SignedSources) {
///     let _: onyx_coordinator::signed::SignedSources = s.clone();
/// }
/// ```
#[derive(Debug)]
pub struct SignedSources {
    /// Process-unique id binding [`Reviewer`]s to this store.
    id: u64,
    keys: BTreeMap<Hash32, KeyEntry>,
    /// Source -> origin its keys were published under.
    origins: BTreeMap<SourceId, String>,
    /// Origin -> the one source publishing keys there (L3R-F7).
    origin_owner: BTreeMap<String, SourceId>,
    /// Signer address -> the one key id registered for it, never removed: a
    /// signer is registered once, under one source, for good (L3R4-F1).
    signers: BTreeMap<[u8; 20], Hash32>,
    /// (key, sequence) -> report hash: the replay guard together with the
    /// 7-day intake window. Entries older than the window are pruned (a
    /// report that old is refused as stale anyway, L3R2-F5).
    seqs: BTreeMap<(Hash32, u64), Hash32>,
    /// Payload digest -> report hash for every recorded report: a payload the
    /// store has already recorded (as accepted, evidence or equivocation) is a
    /// replay, whoever relays it (L3R4-F2). Pruned with the report book.
    payloads: BTreeMap<Hash32, Hash32>,
    /// Validly signed reports seen (accepted or not), within the window plus
    /// every report still needed as evidence.
    reports: BTreeMap<Hash32, Recorded>,
    /// (observed time, report) for the pruning sweep.
    by_time: BTreeSet<(u64, Hash32)>,
    /// Reports past the window kept only as evidence (in an open dispute or
    /// on the re-review list); released by the next sweep after their
    /// dispute or review ends (L3R3-F6).
    held: BTreeSet<(u64, Hash32)>,
    /// A dispute or review entry ended since the last sweep.
    held_dirty: bool,
    /// Fact -> status.
    facts: BTreeMap<Hash32, FactStatus>,
    /// Report hash -> accepted report (withdrawn reports leave this map).
    accepted: BTreeMap<Hash32, Accepted>,
    /// Reports withdrawn from `accepted`, waiting for a reviewer.
    review: BTreeSet<Hash32>,
    equivocations: Vec<Equivocation>,
    /// Append-only log of reviewer decisions.
    decisions: Vec<Decision>,
    /// The latest `now` any action used: the clock only moves forward
    /// (L3R-F8).
    clock: u64,
}

impl Default for SignedSources {
    fn default() -> Self {
        Self::new()
    }
}

impl SignedSources {
    /// Empty intake-only store (no reviewer can act on it).
    pub fn new() -> Self {
        Self {
            id: NEXT_STORE.fetch_add(1, Ordering::Relaxed),
            keys: BTreeMap::new(),
            origins: BTreeMap::new(),
            origin_owner: BTreeMap::new(),
            signers: BTreeMap::new(),
            seqs: BTreeMap::new(),
            payloads: BTreeMap::new(),
            reports: BTreeMap::new(),
            by_time: BTreeSet::new(),
            held: BTreeSet::new(),
            held_dirty: false,
            facts: BTreeMap::new(),
            accepted: BTreeMap::new(),
            review: BTreeSet::new(),
            equivocations: Vec::new(),
            decisions: Vec::new(),
            clock: 0,
        }
    }

    /// Empty store and the one [`Reviewer`] capability for it: the
    /// coordinator's admin path keeps the capability; the intake needs none.
    pub fn with_reviewer(name: &str) -> (Self, Reviewer) {
        let store = Self::new();
        let reviewer = Reviewer {
            store: store.id,
            name: name.to_string(),
        };
        (store, reviewer)
    }

    fn authorize(&self, by: &Reviewer) -> Result<(), SignedError> {
        if by.store == self.id {
            Ok(())
        } else {
            Err(SignedError::NotReviewer)
        }
    }

    /// Refuse a step back of the clock (L3R-F8): every time rule
    /// (freshness, retirement grace, `accepted_at`, the dispute window,
    /// pruning) runs on one monotonic clock. Callers pass chain time, the
    /// latest block's timestamp, as the on-chain book uses (ADR-0028
    /// amendment 6). The clock moves only when an action changes the store
    /// ([`Self::advance`], [`Self::prune`]): a refused action moves nothing
    /// (review C2-R1, finding 4).
    fn check_clock(&self, now: u64) -> Result<(), SignedError> {
        if now < self.clock {
            return Err(SignedError::ClockWentBack {
                now,
                clock: self.clock,
            });
        }
        Ok(())
    }

    /// Move the clock to `now` (checked by [`Self::check_clock`] first).
    fn advance(&mut self, now: u64) {
        self.clock = self.clock.max(now);
    }

    /// The latest time this store acted at (its monotonic clock).
    pub fn clock(&self) -> u64 {
        self.clock
    }

    fn log(&mut self, by: &str, at: u64, action: DecisionAction, reason: &str) {
        // Every reviewer action that changes the store logs once, at its
        // `now`: the clock moves there.
        self.advance(at);
        self.decisions.push(Decision {
            by: by.to_string(),
            at,
            action,
            reason: reason.to_string(),
        });
    }

    /// Register a reviewed key (a reviewer action, logged; L3R3-F4). The
    /// key's source must be in `reg`, the provenance well formed, every key
    /// of one source published under the same origin, an origin used by one
    /// source only, and a signer registered once, under one source, in any
    /// life-cycle state (L3R4-F1: a compromise of a signer can never leave
    /// it active elsewhere). Returns the key id.
    pub fn register_key(
        &mut self,
        by: &Reviewer,
        key: SourceKey,
        reg: &dyn Registry,
        now: u64,
        reason: &str,
    ) -> Result<Hash32, SignedError> {
        self.authorize(by)?;
        self.check_clock(now)?;
        key.validate()?;
        reg.source_rights(&key.source)
            .ok_or_else(|| SignedError::UnknownSource(key.source.0.clone()))?;
        let id = key.id();
        if self.keys.contains_key(&id) {
            return Err(SignedError::KeyAlreadyRegistered);
        }
        if self.signers.contains_key(&key.signer) {
            return Err(SignedError::SignerTaken);
        }
        let origin = key.provenance.origin().to_string();
        if let Some(o) = self.origins.get(&key.source) {
            if *o != origin {
                return Err(SignedError::OriginMismatch);
            }
        }
        if let Some(owner) = self.origin_owner.get(&origin) {
            if *owner != key.source {
                return Err(SignedError::OriginTaken);
            }
        }
        self.origins.insert(key.source.clone(), origin.clone());
        self.origin_owner.insert(origin, key.source.clone());
        self.signers.insert(key.signer, id);
        let source = key.source.clone();
        self.keys.insert(
            id,
            KeyEntry {
                key,
                status: KeyStatus::Active,
                suspension: None,
            },
        );
        self.log(
            &by.name,
            now,
            DecisionAction::Register { key: id, source },
            reason,
        );
        Ok(id)
    }

    /// Rotate an active key out at `at` (not after `now`). Reports observed
    /// in `[at - grace, at)` count until `at + grace`; nothing else. Only an
    /// active key can be retired, once: a wrong `at` is corrected only by a
    /// compromise (L3R2-F4). Reports it signed that were accepted while it
    /// was active but are observed at or after `at` (future-dated by up to
    /// the clock skew, or under a retroactive `at`) stop counting: they go
    /// to the re-review list and their facts are cleared (R-SIG-3,
    /// L3R5-F1).
    pub fn retire_key(
        &mut self,
        by: &Reviewer,
        id: &Hash32,
        at: u64,
        now: u64,
        reason: &str,
    ) -> Result<(), SignedError> {
        self.authorize(by)?;
        self.check_clock(now)?;
        if at > now {
            return Err(SignedError::TimeInFuture);
        }
        let e = self.keys.get_mut(id).ok_or(SignedError::UnknownKey)?;
        if e.status != KeyStatus::Active {
            return Err(SignedError::NotActive);
        }
        e.status = KeyStatus::Retired { at };
        let late: Vec<Hash32> = self
            .accepted
            .iter()
            .filter(|(_, a)| a.report.payload.key == *id && a.report.payload.observed_unix >= at)
            .map(|(h, _)| *h)
            .collect();
        let mut withdrawn = Vec::new();
        for h in &late {
            if self.withdraw(h) {
                withdrawn.push(*h);
            }
        }
        self.facts
            .retain(|_, f| !matches!(f, FactStatus::Accepted(h) if late.contains(h)));
        self.held_dirty = true;
        self.log(
            &by.name,
            now,
            DecisionAction::Retire {
                key: *id,
                at,
                withdrawn,
            },
            reason,
        );
        Ok(())
    }

    /// Mark a key compromised from `since` (not after `now`; a later call can
    /// only move `since` earlier). From any life-cycle state; nothing signed
    /// by it is accepted any more. Everything it stated from `since` on
    /// (observed or recorded at or after `since`: a thief can backdate the
    /// observation time, not the recording time) is withdrawn: accepted
    /// reports go to the re-review list and its reports leave open disputes.
    /// An open dispute stays disputed (even with no reports left) and every
    /// suspension stays until a reviewer resolves the fact and reinstates the
    /// keys (amendment 3, L3R3-F1; amendment 4: no dispute ends without a
    /// reviewer). Returns the newly withdrawn accepted reports.
    pub fn mark_compromised(
        &mut self,
        by: &Reviewer,
        id: &Hash32,
        since: u64,
        now: u64,
        reason: &str,
    ) -> Result<Vec<Hash32>, SignedError> {
        self.authorize(by)?;
        self.check_clock(now)?;
        if since > now {
            return Err(SignedError::TimeInFuture);
        }
        let e = self.keys.get_mut(id).ok_or(SignedError::UnknownKey)?;
        let (since, recorded_at) = match e.status {
            KeyStatus::Compromised {
                since: s0,
                recorded_at,
            } => (s0.min(since), recorded_at),
            _ => (since, now),
        };
        e.status = KeyStatus::Compromised { since, recorded_at };
        let tainted: BTreeSet<Hash32> = self
            .reports
            .iter()
            .filter(|(_, r)| {
                r.report.payload.key == *id
                    && (r.report.payload.observed_unix >= since || r.recorded_at >= since)
            })
            .map(|(h, _)| *h)
            .chain(
                self.accepted
                    .iter()
                    .filter(|(_, a)| {
                        a.report.payload.key == *id
                            && (a.report.payload.observed_unix >= since || a.accepted_at >= since)
                    })
                    .map(|(h, _)| *h),
            )
            .collect();
        let mut withdrawn = Vec::new();
        for h in &tainted {
            if self.accepted.remove(h).is_some() {
                self.review.insert(*h);
                withdrawn.push(*h);
            }
        }
        // An accepted fact backed by the stolen key is freed so the source's
        // new key can state it (R-SIG-6a). A disputed fact stays disputed,
        // even with no reports left, until a reviewer resolves it: no dispute
        // ends without a reviewer (invariant 2, L3R4 info).
        let mut emptied = Vec::new();
        for (fact, status) in self.facts.iter_mut() {
            match status {
                FactStatus::Accepted(h) if tainted.contains(h) => emptied.push(*fact),
                FactStatus::Disputed(list) => list.retain(|h| !tainted.contains(h)),
                FactStatus::Accepted(_) => {}
            }
        }
        for fact in emptied {
            self.facts.remove(&fact);
        }
        self.held_dirty = true;
        self.log(
            &by.name,
            now,
            DecisionAction::Compromise {
                key: *id,
                since,
                withdrawn: withdrawn.clone(),
            },
            reason,
        );
        Ok(withdrawn)
    }

    /// Reviewer decision on a disputed fact: `keep` one of its reports as the
    /// accepted value (its key must be allowed to sign it now: not
    /// suspended, active or within its retirement window, L3R2-F6), or `None`
    /// to clear the fact so the source can state it again.
    pub fn resolve_fact(
        &mut self,
        by: &Reviewer,
        fact: &Hash32,
        keep: Option<Hash32>,
        now: u64,
        reason: &str,
    ) -> Result<(), SignedError> {
        self.authorize(by)?;
        self.check_clock(now)?;
        let Some(FactStatus::Disputed(list)) = self.facts.get(fact) else {
            return Err(SignedError::NotDisputed);
        };
        self.held_dirty = true;
        let Some(keep) = keep else {
            self.facts.remove(fact);
            self.log(
                &by.name,
                now,
                DecisionAction::ResolveFact {
                    fact: *fact,
                    keep: None,
                },
                reason,
            );
            return Ok(());
        };
        if !list.contains(&keep) {
            return Err(SignedError::BadResolution);
        }
        let report = self
            .report_of(&keep)
            .cloned()
            .ok_or(SignedError::BadResolution)?;
        let entry = self
            .keys
            .get(&report.payload.key)
            .ok_or(SignedError::BadResolution)?;
        permits(entry, report.payload.observed_unix, now)
            .map_err(|_| SignedError::BadResolution)?;
        self.facts.insert(*fact, FactStatus::Accepted(keep));
        self.accepted.insert(
            keep,
            Accepted {
                report,
                accepted_at: now,
            },
        );
        self.review.remove(&keep);
        self.log(
            &by.name,
            now,
            DecisionAction::ResolveFact {
                fact: *fact,
                keep: Some(keep),
            },
            reason,
        );
        Ok(())
    }

    /// Reviewer decision: clear a key's suspension. The life cycle is
    /// untouched: a retired or compromised key stays so (L3R2-F1).
    pub fn reinstate_key(
        &mut self,
        by: &Reviewer,
        id: &Hash32,
        now: u64,
        reason: &str,
    ) -> Result<(), SignedError> {
        self.authorize(by)?;
        self.check_clock(now)?;
        let e = self.keys.get_mut(id).ok_or(SignedError::UnknownKey)?;
        if e.suspension.take().is_none() {
            return Err(SignedError::NotSuspended);
        }
        self.log(
            &by.name,
            now,
            DecisionAction::Reinstate { key: *id },
            reason,
        );
        Ok(())
    }

    /// A key's life cycle, if registered.
    pub fn key_status(&self, id: &Hash32) -> Option<KeyStatus> {
        self.keys.get(id).map(|e| e.status)
    }

    /// A key's life cycle and suspension, if registered.
    pub fn key_state(&self, id: &Hash32) -> Option<KeyState> {
        self.keys.get(id).map(|e| KeyState {
            status: e.status,
            suspended_at: e.suspension.as_ref().map(|s| s.at),
        })
    }

    /// Whether a key is suspended.
    pub fn is_suspended(&self, id: &Hash32) -> bool {
        self.keys.get(id).is_some_and(|e| e.suspension.is_some())
    }

    /// An accepted report by its hash (withdrawn reports are not).
    pub fn accepted(&self, report: &Hash32) -> Option<&Accepted> {
        self.accepted.get(report)
    }

    /// Where a fact stands; consumers must treat `Disputed` as having no
    /// value and settle only on [`settleable`](Self::settleable) facts.
    pub fn fact_status(&self, fact: &Hash32) -> Option<&FactStatus> {
        self.facts.get(fact)
    }

    /// The accepted report behind `fact` once it has stood for
    /// `DISPUTE_WINDOW_SECS`: the only value a consumer may settle on
    /// (L3R2-F7).
    pub fn settleable(&self, fact: &Hash32, now: u64) -> Option<&Accepted> {
        match self.facts.get(fact) {
            Some(FactStatus::Accepted(h)) => self
                .accepted
                .get(h)
                .filter(|a| now >= a.accepted_at.saturating_add(DISPUTE_WINDOW_SECS)),
            _ => None,
        }
    }

    /// Accepted reports withdrawn and waiting for a reviewer.
    pub fn review_list(&self) -> &BTreeSet<Hash32> {
        &self.review
    }

    /// Equivocations seen so far (evidence against the signing source).
    pub fn equivocations(&self) -> &[Equivocation] {
        &self.equivocations
    }

    /// The append-only decision log (every reviewer decision).
    pub fn decisions(&self) -> &[Decision] {
        &self.decisions
    }

    /// Sizes of the sequence book and the report book (memory bound, L3R2-F5).
    pub fn book_sizes(&self) -> (usize, usize) {
        (self.seqs.len(), self.reports.len())
    }

    /// The report behind a hash, from the report book or the accepted map.
    fn report_of(&self, h: &Hash32) -> Option<&SignedReport> {
        self.reports
            .get(h)
            .map(|r| &r.report)
            .or_else(|| self.accepted.get(h).map(|a| &a.report))
    }

    /// Drop sequence and report entries observed more than
    /// `PRUNE_AFTER_SECS` before `now` (any report that old is refused as
    /// stale), except reports still needed as evidence: in an open dispute
    /// or on the re-review list. Those are held and released by the next
    /// sweep after their dispute or review ends (L3R3-F6). Accepted reports
    /// live on in the accepted map. Runs at every intake; callable directly.
    ///
    /// Bounds (L3R3-F6): the sequence and report books hold at most one
    /// window (`PRUNE_AFTER_SECS`) of reports plus the held evidence; the
    /// fact book and the accepted map hold one entry per accepted fact (the
    /// data set itself, restatements are not stored) and grow with what the
    /// sources publish; the re-review list grows only by compromises and
    /// equivocations. The clock never steps back (L3R-F8): an earlier `now`
    /// prunes nothing, and every other action refuses it (`ClockWentBack`),
    /// so a pruned report can never be replayed against an emptied entry.
    pub fn prune(&mut self, now: u64) {
        // A time before the clock prunes nothing (L3R-F8): a step back can
        // never drop an entry a later replay would need.
        if now < self.clock {
            return;
        }
        self.clock = now;
        let cutoff = now.saturating_sub(PRUNE_AFTER_SECS);
        let due = self.by_time.first().is_some_and(|(t, _)| *t < cutoff);
        if !due && !(self.held_dirty && !self.held.is_empty()) {
            return;
        }
        self.held_dirty = false;
        let in_dispute: BTreeSet<Hash32> = self
            .facts
            .values()
            .filter_map(|f| match f {
                FactStatus::Disputed(l) => Some(l.iter().copied()),
                FactStatus::Accepted(_) => None,
            })
            .flatten()
            .collect();
        let released: Vec<(u64, Hash32)> = self
            .held
            .iter()
            .filter(|(_, h)| !in_dispute.contains(h) && !self.review.contains(h))
            .copied()
            .collect();
        for e in released {
            self.held.remove(&e);
            self.drop_report(&e.1);
        }
        while let Some(&(t, h)) = self.by_time.first() {
            if t >= cutoff {
                break;
            }
            self.by_time.pop_first();
            if in_dispute.contains(&h) || self.review.contains(&h) {
                self.held.insert((t, h));
                continue;
            }
            self.drop_report(&h);
        }
    }

    /// Forget a report and its sequence entry.
    fn drop_report(&mut self, h: &Hash32) {
        if let Some(rec) = self.reports.remove(h) {
            let k = (rec.report.payload.key, rec.report.payload.sequence);
            if self.seqs.get(&k) == Some(h) {
                self.seqs.remove(&k);
            }
            let d = rec.report.payload.digest();
            if self.payloads.get(&d) == Some(h) {
                self.payloads.remove(&d);
            }
        }
    }

    /// The one intake for level-3 signed-source claims (ADR-0028). Checks, in
    /// order: the claim's own rules; the method names this report and key;
    /// the payload shape and the reserved sequence; the key is registered for
    /// the payload's source and may sign it now (life cycle, retirement
    /// window, suspension); the signature; freshness; every claim field
    /// equals the signed payload; then the sequence book (exact replay, or a
    /// different payload under a used sequence = equivocation) and the fact
    /// book (same value = restatement, not stored; different value =
    /// equivocation; disputed = evidence only). Records the report on
    /// success.
    pub fn accept_claim(
        &mut self,
        claim: &Claim,
        report: &SignedReport,
        reg: &dyn Registry,
        now: u64,
    ) -> Result<Intake, SignedError> {
        self.check_clock(now)?;
        claim.validate(reg)?;
        let VerificationMethod::SignedSource { key, report: rh } = &claim.method else {
            return Err(SignedError::NotSignedSource);
        };
        let p = &report.payload;
        let report_hash = report.hash();
        if *rh != report_hash {
            return Err(SignedError::ClaimNotBacked(
                "report hash is not this report's",
            ));
        }
        if *key != p.key {
            return Err(SignedError::ClaimNotBacked("key is not the report's key"));
        }
        p.validate()?;
        if p.sequence == RESERVED_SEQUENCE {
            return Err(SignedError::SequenceReserved);
        }
        let entry = self.keys.get(&p.key).ok_or(SignedError::UnknownKey)?;
        if entry.key.source != p.source {
            return Err(SignedError::KeySourceMismatch);
        }
        permits(entry, p.observed_unix, now)?;
        if eip191_recover(p.message().as_bytes(), &report.signature) != Some(entry.key.signer) {
            return Err(SignedError::BadSignature);
        }
        if p.observed_unix > now.saturating_add(MAX_CLOCK_SKEW_SECS) {
            return Err(SignedError::FromFuture);
        }
        if now.saturating_sub(p.observed_unix) > MAX_REPORT_AGE_SECS {
            return Err(SignedError::Stale);
        }
        bind_claim(claim, p, report_hash)?;
        // Only a validly signed, fresh report bound to its claim moves the
        // clock and prunes (review C2-R1, finding 4).
        self.prune(now);

        // A payload already recorded (accepted, evidence, or either side of
        // an equivocation), under any signature, is a replay and changes
        // nothing (L3R4-F2).
        if self.payloads.contains_key(&p.digest()) {
            return Err(SignedError::Replay);
        }
        // Sequence book: one payload per (key, sequence); another payload
        // under a used sequence is an equivocation.
        if let Some(prev) = self.seqs.get(&(p.key, p.sequence)).copied() {
            self.record(report_hash, report, now);
            return self.equivocate_sequence(prev, report, now);
        }
        // Fact book, before anything is accepted (L3R-F6): one value per
        // (source, metric, subject, time) across all of the source's keys.
        let fact = signed_fact_key(p);
        match self.facts.get(&fact).cloned() {
            Some(FactStatus::Accepted(prev)) => {
                let same_value = self
                    .report_of(&prev)
                    .is_some_and(|r| r.payload.value == p.value);
                if same_value {
                    // A restatement (re-publish, or two keys during a
                    // rotation): harmless and not stored (L3R-F2, L3R2-F5).
                    return Err(SignedError::AlreadyStated);
                }
                self.record(report_hash, report, now);
                self.equivocate_fact(fact, prev, report, now)
            }
            Some(FactStatus::Disputed(list)) => {
                // At most one report per key in a dispute: the same value
                // again is a restatement; another value from the same key is
                // that key contradicting itself.
                let own = list
                    .iter()
                    .find(|h| self.report_of(h).is_some_and(|r| r.payload.key == p.key))
                    .copied();
                match own {
                    Some(h)
                        if self
                            .report_of(&h)
                            .is_some_and(|r| r.payload.value == p.value) =>
                    {
                        Err(SignedError::AlreadyStated)
                    }
                    Some(h) => {
                        self.record(report_hash, report, now);
                        if let Some(first) = self.report_of(&h).cloned() {
                            self.equivocations.push(Equivocation {
                                first,
                                second: report.clone(),
                                kind: EquivocationKind::Fact,
                            });
                        }
                        self.suspend(&p.key, fact, now);
                        Err(SignedError::Equivocation {
                            fact,
                            withdrawn: Vec::new(),
                        })
                    }
                    None => {
                        // A new key joins the dispute. Its report is kept as
                        // evidence; if its value differs from any report in
                        // the dispute it took a side in a contradiction and
                        // is suspended like the keys already in it
                        // (L3R3-F2).
                        self.record(report_hash, report, now);
                        let differs = list
                            .iter()
                            .filter_map(|h| self.report_of(h).cloned())
                            .find(|r| r.payload.value != p.value);
                        if let Some(FactStatus::Disputed(l)) = self.facts.get_mut(&fact) {
                            l.push(report_hash);
                        }
                        match differs {
                            Some(first) => {
                                self.equivocations.push(Equivocation {
                                    first,
                                    second: report.clone(),
                                    kind: EquivocationKind::Fact,
                                });
                                self.suspend(&p.key, fact, now);
                                Err(SignedError::Equivocation {
                                    fact,
                                    withdrawn: Vec::new(),
                                })
                            }
                            None => Err(SignedError::FactDisputed),
                        }
                    }
                }
            }
            None => {
                self.record(report_hash, report, now);
                self.facts.insert(fact, FactStatus::Accepted(report_hash));
                self.accepted.insert(
                    report_hash,
                    Accepted {
                        report: report.clone(),
                        accepted_at: now,
                    },
                );
                Ok(Intake {
                    level: ProofLevel::SourceProven,
                    fact,
                    report: report_hash,
                })
            }
        }
    }

    /// [`accept_claim`](Self::accept_claim) from canonical bytes of the claim
    /// and of the report.
    pub fn decode_claim(
        &mut self,
        claim_bytes: &[u8],
        report_bytes: &[u8],
        reg: &dyn Registry,
        now: u64,
    ) -> Result<(Claim, SignedReport, Intake), SignedError> {
        let claim: Claim = from_canonical_bytes(claim_bytes)
            .map_err(|_: CanonicalError| SignedError::NotCanonical)?;
        let report = decode_report(report_bytes)?;
        let intake = self.accept_claim(&claim, &report, reg, now)?;
        Ok((claim, report, intake))
    }

    /// Keep a validly signed report and claim its (key, sequence).
    fn record(&mut self, hash: Hash32, report: &SignedReport, now: u64) {
        self.seqs
            .entry((report.payload.key, report.payload.sequence))
            .or_insert(hash);
        self.payloads.entry(report.payload.digest()).or_insert(hash);
        if let std::collections::btree_map::Entry::Vacant(v) = self.reports.entry(hash) {
            v.insert(Recorded {
                report: report.clone(),
                recorded_at: now,
            });
            self.by_time.insert((report.payload.observed_unix, hash));
        }
    }

    /// Suspend a key over `cause` (a disputed fact or a sequence
    /// equivocation). The life cycle is untouched.
    fn suspend(&mut self, id: &Hash32, cause: Hash32, now: u64) {
        if let Some(e) = self.keys.get_mut(id) {
            e.suspension
                .get_or_insert_with(|| Suspension {
                    at: now,
                    causes: BTreeSet::new(),
                })
                .causes
                .insert(cause);
        }
    }

    /// Withdraw an accepted report to re-review; whether it was accepted.
    fn withdraw(&mut self, h: &Hash32) -> bool {
        if self.accepted.remove(h).is_some() {
            self.review.insert(*h);
            true
        } else {
            false
        }
    }

    /// Two values for one fact: the fact becomes disputed, the accepted report
    /// is withdrawn, and every key involved is suspended (L3R-F3).
    fn equivocate_fact(
        &mut self,
        fact: Hash32,
        prev: Hash32,
        second: &SignedReport,
        now: u64,
    ) -> Result<Intake, SignedError> {
        if let Some(first) = self.report_of(&prev).cloned() {
            self.suspend(&first.payload.key, fact, now);
            self.equivocations.push(Equivocation {
                first,
                second: second.clone(),
                kind: EquivocationKind::Fact,
            });
        }
        self.suspend(&second.payload.key, fact, now);
        let withdrawn = if self.withdraw(&prev) {
            vec![prev]
        } else {
            Vec::new()
        };
        self.facts
            .insert(fact, FactStatus::Disputed(vec![prev, second.hash()]));
        Err(SignedError::Equivocation { fact, withdrawn })
    }

    /// One key, one sequence, two payloads (R-SIG-6, last bullet). The key
    /// is suspended (only a reviewer clears that); the report accepted under
    /// that sequence is withdrawn and its fact disputed (with both reports
    /// when they state the same fact). The second report then meets the
    /// fact book on its own fact like any report, short of being accepted
    /// (L3R5-F2): a different value from another key's accepted report
    /// disputes that fact and suspends that key too.
    fn equivocate_sequence(
        &mut self,
        prev: Hash32,
        second: &SignedReport,
        now: u64,
    ) -> Result<Intake, SignedError> {
        let second_hash = second.hash();
        let second_fact = signed_fact_key(&second.payload);
        let mut fact = second_fact;
        let mut handled = None;
        if let Some(first) = self.report_of(&prev).cloned() {
            self.equivocations.push(Equivocation {
                first: first.clone(),
                second: second.clone(),
                kind: EquivocationKind::Sequence,
            });
            let first_fact = signed_fact_key(&first.payload);
            if self.facts.get(&first_fact) == Some(&FactStatus::Accepted(prev)) {
                let mut list = vec![prev];
                if second_fact == first_fact {
                    list.push(second_hash);
                }
                self.facts.insert(first_fact, FactStatus::Disputed(list));
                fact = first_fact;
                handled = Some(first_fact);
            }
        }
        self.suspend(
            &second.payload.key,
            sequence_cause(&second.payload.key, second.payload.sequence),
            now,
        );
        let mut withdrawn = Vec::new();
        if self.withdraw(&prev) {
            withdrawn.push(prev);
        }
        if handled != Some(second_fact) {
            withdrawn.extend(self.contradict(second_fact, second, now));
        }
        Err(SignedError::Equivocation { fact, withdrawn })
    }

    /// The fact book for a report that is recorded but never accepted (the
    /// second payload of a sequence equivocation): a different value from
    /// another key's accepted report disputes the fact and suspends every key
    /// involved; a dispute it does not belong to yet takes it as evidence
    /// (suspending it if its value differs); the same value, an unknown
    /// fact, or a dispute it is already in change nothing more. Returns the
    /// accepted reports withdrawn.
    fn contradict(&mut self, fact: Hash32, report: &SignedReport, now: u64) -> Vec<Hash32> {
        let hash = report.hash();
        match self.facts.get(&fact).cloned() {
            Some(FactStatus::Accepted(h)) => {
                let Some(first) = self.report_of(&h).cloned() else {
                    return Vec::new();
                };
                if first.payload.value == report.payload.value {
                    return Vec::new();
                }
                self.suspend(&first.payload.key, fact, now);
                self.suspend(&report.payload.key, fact, now);
                self.equivocations.push(Equivocation {
                    first,
                    second: report.clone(),
                    kind: EquivocationKind::Fact,
                });
                let withdrawn = if self.withdraw(&h) {
                    vec![h]
                } else {
                    Vec::new()
                };
                self.facts.insert(fact, FactStatus::Disputed(vec![h, hash]));
                withdrawn
            }
            Some(FactStatus::Disputed(list)) => {
                let own = list.iter().any(|x| {
                    self.report_of(x)
                        .is_some_and(|r| r.payload.key == report.payload.key)
                });
                if own || list.contains(&hash) {
                    return Vec::new();
                }
                let differs = list
                    .iter()
                    .filter_map(|x| self.report_of(x).cloned())
                    .find(|r| r.payload.value != report.payload.value);
                if let Some(FactStatus::Disputed(l)) = self.facts.get_mut(&fact) {
                    l.push(hash);
                }
                if let Some(first) = differs {
                    self.equivocations.push(Equivocation {
                        first,
                        second: report.clone(),
                        kind: EquivocationKind::Fact,
                    });
                    self.suspend(&report.payload.key, fact, now);
                }
                Vec::new()
            }
            None => Vec::new(),
        }
    }
}

/// Canonical bytes of a signed report (no trailing bytes).
pub fn decode_report(bytes: &[u8]) -> Result<SignedReport, SignedError> {
    let mut rest = bytes;
    let r = SignedReport::deserialize(&mut rest).map_err(|_| SignedError::NotCanonical)?;
    if !rest.is_empty() || borsh::to_vec(&r).map_err(|_| SignedError::NotCanonical)? != bytes {
        return Err(SignedError::NotCanonical);
    }
    Ok(r)
}

/// Every claim field a signed payload could be reused for must be exactly the
/// payload's (docs/spec/proof-ladder.md "Level 3", table).
fn bind_claim(claim: &Claim, p: &SignedPayload, report_hash: Hash32) -> Result<(), SignedError> {
    if claim.value != p.value {
        return Err(SignedError::ClaimNotBacked("value is not the signed value"));
    }
    if claim.metric != p.metric {
        return Err(SignedError::ClaimNotBacked(
            "metric is not the signed metric",
        ));
    }
    if claim.subject != p.subject {
        return Err(SignedError::ClaimNotBacked(
            "subject is not the signed subject",
        ));
    }
    if claim.source.source != p.source {
        return Err(SignedError::ClaimNotBacked(
            "source is not the signing source",
        ));
    }
    let snapshot = Snapshot::Document {
        published_unix: p.observed_unix,
        content_hash: report_hash,
    };
    if claim.source.snapshot != snapshot {
        return Err(SignedError::ClaimNotBacked(
            "snapshot must be the observation time and this report",
        ));
    }
    if claim.label_set.is_some() {
        return Err(SignedError::ClaimNotBacked(
            "a signed value depends on no label set",
        ));
    }
    let evidence = [EvidenceRef {
        kind: REPORT_EVIDENCE.into(),
        content_hash: report_hash,
    }];
    if claim.evidence != evidence {
        return Err(SignedError::ClaimNotBacked(
            "evidence must be exactly the signed report",
        ));
    }
    if claim.confidence.is_some() {
        return Err(SignedError::ClaimNotBacked(
            "objective claim has no confidence",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{
        to_canonical_bytes, DataRights, EntityKind, KeyProvenance, MetricId, MetricInfo,
        Redistribution, SourceRef, StaticRegistry, Subject, Value, ValueKind,
    };
    use onyx_labels::attest::eip191_digest;
    use secp256k1::{Message, Secp256k1, SecretKey};

    const NOW: u64 = 1_700_000_000;

    fn sk(label: &str) -> SecretKey {
        SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key")
    }

    fn address(sk: &SecretKey) -> [u8; 20] {
        let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
        let h = keccak256(&pk.serialize_uncompressed()[1..]);
        let mut a = [0u8; 20];
        a.copy_from_slice(&h.0[12..]);
        a
    }

    fn sign(sk: &SecretKey, msg: &[u8]) -> [u8; 65] {
        let digest = Message::from_digest(eip191_digest(msg).0);
        let (rid, bytes) = Secp256k1::new()
            .sign_ecdsa_recoverable(&digest, sk)
            .serialize_compact();
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&bytes);
        out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
        out
    }

    fn rights(id: &str) -> DataRights {
        DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId(id.into()),
            attribution: None,
            terms: String::new(),
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.add_metric(
            "example.signed.reserve",
            MetricInfo {
                value_kind: ValueKind::UInt,
                needs_label_set: false,
            },
        )
        .and_then(|r| {
            r.add_metric(
                "example.signed.other",
                MetricInfo {
                    value_kind: ValueKind::UInt,
                    needs_label_set: false,
                },
            )
        })
        .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
        .and_then(|r| r.add_entity("entity:other", EntityKind::Exchange))
        .expect("registry");
        for id in ["example.signed", "example.other"] {
            r.add_source(rights(id)).expect("source");
        }
        r
    }

    fn source_key(source: &str, signer: &SecretKey, origin: &str) -> SourceKey {
        SourceKey {
            source: SourceId(source.into()),
            signer: address(signer),
            provenance: KeyProvenance::WellKnown {
                origin: origin.into(),
                document_hash: keccak256(b"keys document"),
                fetched_unix: NOW - 10_000,
            },
        }
    }

    struct World {
        reg: StaticRegistry,
        store: SignedSources,
        admin: Reviewer,
        signer: SecretKey,
        key: Hash32,
    }

    fn world() -> World {
        let reg = reg();
        let (mut store, admin) = SignedSources::with_reviewer("test reviewer");
        let signer = sk("onyx test signed source key 1");
        let key = store
            .register_key(
                &admin,
                source_key("example.signed", &signer, "data.example.org"),
                &reg,
                NOW,
                "test",
            )
            .expect("register");
        World {
            reg,
            store,
            admin,
            signer,
            key,
        }
    }

    fn payload(key: Hash32, seq: u64, value: u128, observed: u64) -> SignedPayload {
        SignedPayload {
            source: SourceId("example.signed".into()),
            key,
            metric: MetricId("example.signed.reserve".into()),
            subject: Subject::Entity("entity:example".into()),
            value: Value::UInt(value),
            observed_unix: observed,
            sequence: seq,
        }
    }

    fn report(sk: &SecretKey, p: SignedPayload) -> SignedReport {
        let signature = sign(sk, p.message().as_bytes());
        SignedReport {
            payload: p,
            signature,
        }
    }

    /// The claim the intake expects for `r`.
    fn claim_for(r: &SignedReport) -> Claim {
        let h = r.hash();
        Claim {
            metric: r.payload.metric.clone(),
            subject: r.payload.subject.clone(),
            value: r.payload.value.clone(),
            source: SourceRef {
                source: r.payload.source.clone(),
                snapshot: Snapshot::Document {
                    published_unix: r.payload.observed_unix,
                    content_hash: h,
                },
            },
            method: VerificationMethod::SignedSource {
                key: r.payload.key,
                report: h,
            },
            label_set: None,
            rights: rights(&r.payload.source.0),
            evidence: vec![EvidenceRef {
                kind: REPORT_EVIDENCE.into(),
                content_hash: h,
            }],
            confidence: None,
        }
    }

    fn accept(w: &mut World, r: &SignedReport) -> Result<ProofLevel, SignedError> {
        let c = claim_for(r);
        w.store.accept_claim(&c, r, &w.reg, NOW).map(|i| i.level)
    }

    fn is_equivocation<T>(r: &Result<T, SignedError>) -> bool {
        matches!(r, Err(SignedError::Equivocation { .. }))
    }

    #[test]
    fn a_signed_report_is_level_three() {
        let mut w = world();
        let r = report(&w.signer, payload(w.key, 1, 42, NOW - 60));
        assert_eq!(accept(&mut w, &r), Ok(ProofLevel::SourceProven));
        assert!(w.store.accepted(&r.hash()).is_some());
        // Byte path.
        let r2 = report(&w.signer, payload(w.key, 2, 43, NOW - 30));
        let cb = to_canonical_bytes(&claim_for(&r2)).expect("claim bytes");
        let rb = borsh::to_vec(&r2).expect("report bytes");
        let (c, back, intake) = w
            .store
            .decode_claim(&cb, &rb, &w.reg, NOW)
            .expect("decode path");
        assert_eq!(intake.fact, signed_fact_key(&r2.payload));
        assert_eq!(intake.report, r2.hash());
        assert_eq!((c, back), (claim_for(&r2), r2));
        // Trailing bytes are not canonical.
        let mut long = borsh::to_vec(&report(&w.signer, payload(w.key, 3, 1, NOW))).expect("rb");
        long.push(0);
        assert_eq!(decode_report(&long), Err(SignedError::NotCanonical));
    }

    #[test]
    fn a_forged_signature_is_refused() {
        let mut w = world();
        let mut r = report(&w.signer, payload(w.key, 1, 42, NOW));
        // Payload edited after signing.
        r.payload.value = Value::UInt(4_200);
        assert_eq!(accept(&mut w, &r), Err(SignedError::BadSignature));
        // Garbage signature.
        let mut r = report(&w.signer, payload(w.key, 1, 42, NOW));
        r.signature[10] ^= 1;
        assert_eq!(accept(&mut w, &r), Err(SignedError::BadSignature));
        // High-s twin (EIP-2): same signer, refused by the canonical-form rule.
        let r = report(&w.signer, payload(w.key, 1, 42, NOW));
        let mut high = r.clone();
        let n: [u8; 32] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
            0xd0, 0x36, 0x41, 0x41,
        ];
        let mut s = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(n[i]) - i16::from(r.signature[32 + i]) - borrow;
            borrow = i16::from(d < 0);
            s[i] = u8::try_from(d.rem_euclid(256)).expect("byte");
        }
        high.signature[32..64].copy_from_slice(&s);
        high.signature[64] = if r.signature[64] == 27 { 28 } else { 27 };
        assert_eq!(accept(&mut w, &high), Err(SignedError::BadSignature));
        // Nothing was recorded by the refusals.
        assert_eq!(accept(&mut w, &r), Ok(ProofLevel::SourceProven));
    }

    #[test]
    fn a_wrong_key_is_refused() {
        let mut w = world();
        // Signed by another secret, naming the registered key.
        let thief = sk("not the source");
        let r = report(&thief, payload(w.key, 1, 42, NOW));
        assert_eq!(accept(&mut w, &r), Err(SignedError::BadSignature));
        // Naming a key nobody registered.
        let unregistered = source_key("example.signed", &thief, "data.example.org").id();
        let r = report(&thief, payload(unregistered, 1, 42, NOW));
        assert_eq!(accept(&mut w, &r), Err(SignedError::UnknownKey));
    }

    #[test]
    fn a_retired_key_signs_only_the_past_and_a_compromised_key_nothing() {
        let mut w = world();
        let before = report(&w.signer, payload(w.key, 1, 42, NOW - 100));
        let after = report(&w.signer, payload(w.key, 2, 43, NOW - 10));
        w.store
            .retire_key(&w.admin, &w.key, NOW - 50, NOW, "rotation")
            .expect("retire");
        assert_eq!(accept(&mut w, &after), Err(SignedError::KeyRetired));
        assert_eq!(accept(&mut w, &before), Ok(ProofLevel::SourceProven));
        // Compromise: everything refused, whatever the stated time, and the
        // accepted report observed after `since` is listed for review.
        let review = w
            .store
            .mark_compromised(&w.admin, &w.key, NOW - 200, NOW, "stolen")
            .expect("compromise");
        assert_eq!(review, vec![before.hash()]);
        let backdated = report(&w.signer, payload(w.key, 3, 99, NOW - 5_000));
        assert_eq!(accept(&mut w, &backdated), Err(SignedError::KeyCompromised));
        // Retiring never relaxes a compromise.
        assert_eq!(
            w.store.retire_key(&w.admin, &w.key, NOW, NOW, "late"),
            Err(SignedError::NotActive)
        );
        assert!(matches!(
            w.store.key_status(&w.key),
            Some(KeyStatus::Compromised { .. })
        ));
    }

    #[test]
    fn a_used_sequence_is_refused_but_order_is_free() {
        let mut w = world();
        let r5 = report(&w.signer, payload(w.key, 5, 42, NOW - 60));
        assert_eq!(accept(&mut w, &r5), Ok(ProofLevel::SourceProven));
        // Exact replay.
        assert_eq!(accept(&mut w, &r5), Err(SignedError::Replay));
        // An earlier, unused sequence for another fact still counts: no
        // high-water mark, so a later report delivered first censors nothing
        // (L3R-F5).
        let r3 = report(&w.signer, payload(w.key, 3, 43, NOW - 30));
        assert_eq!(accept(&mut w, &r3), Ok(ProofLevel::SourceProven));
        // The reserved top sequence is refused (L3R-F8).
        let top = report(&w.signer, payload(w.key, u64::MAX, 1, NOW - 20));
        assert_eq!(accept(&mut w, &top), Err(SignedError::SequenceReserved));
        // A second signature over the same payload (another nonce k) is still
        // a replay of that sequence, not a new report.
        let p = payload(w.key, 5, 42, NOW - 60);
        let digest = Message::from_digest(eip191_digest(p.message().as_bytes()).0);
        let mut other_sig = None;
        for extra in 0u8..50 {
            let (rid, bytes) = Secp256k1::new()
                .sign_ecdsa_recoverable_with_noncedata(&digest, &w.signer, &[extra; 32])
                .serialize_compact();
            let mut s = [0u8; 65];
            s[..64].copy_from_slice(&bytes);
            s[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
            if s != r5.signature {
                other_sig = Some(s);
                break;
            }
        }
        let twin = SignedReport {
            payload: p,
            signature: other_sig.expect("a second signature"),
        };
        assert_ne!(twin.hash(), r5.hash());
        assert_eq!(accept(&mut w, &twin), Err(SignedError::Replay));
        assert!(w.store.equivocations().is_empty());
        // Stale and future reports.
        let mut w = world();
        let old = report(
            &w.signer,
            payload(w.key, 1, 1, NOW - MAX_REPORT_AGE_SECS - 1),
        );
        assert_eq!(accept(&mut w, &old), Err(SignedError::Stale));
        let future = report(
            &w.signer,
            payload(w.key, 1, 1, NOW + MAX_CLOCK_SKEW_SECS + 1),
        );
        assert_eq!(accept(&mut w, &future), Err(SignedError::FromFuture));
    }

    #[test]
    fn two_values_for_one_fact_or_one_sequence_suspend_the_key() {
        let mut w = world();
        let a = report(&w.signer, payload(w.key, 1, 42, NOW - 60));
        assert_eq!(accept(&mut w, &a), Ok(ProofLevel::SourceProven));
        // Same fact (metric, subject, time), new sequence, other value.
        let b = report(&w.signer, payload(w.key, 2, 4_200, NOW - 60));
        let got = w.store.accept_claim(&claim_for(&b), &b, &w.reg, NOW);
        assert_eq!(
            got,
            Err(SignedError::Equivocation {
                fact: signed_fact_key(&a.payload),
                withdrawn: vec![a.hash()],
            })
        );
        let ev = &w.store.equivocations()[0];
        assert_eq!(ev.kind, EquivocationKind::Fact);
        assert_eq!((&ev.first, &ev.second), (&a, &b));
        assert!(w.store.is_suspended(&w.key));
        assert_eq!(w.store.key_status(&w.key), Some(KeyStatus::Active));
        // The fact is disputed and no value for it stays accepted.
        let fact = signed_fact_key(&a.payload);
        assert_eq!(
            w.store.fact_status(&fact),
            Some(&FactStatus::Disputed(vec![a.hash(), b.hash()]))
        );
        assert!(w.store.accepted(&a.hash()).is_none());
        assert!(w.store.review_list().contains(&a.hash()));
        // Suspended: even a clean new report is refused.
        let c = report(&w.signer, payload(w.key, 3, 7, NOW - 10));
        assert_eq!(accept(&mut w, &c), Err(SignedError::KeySuspended));
        // Same sequence, other payload.
        let mut w = world();
        let a = report(&w.signer, payload(w.key, 1, 42, NOW - 60));
        assert_eq!(accept(&mut w, &a), Ok(ProofLevel::SourceProven));
        let b = report(&w.signer, payload(w.key, 1, 43, NOW - 50));
        assert!(is_equivocation(&accept(&mut w, &b)));
        assert_eq!(w.store.equivocations()[0].kind, EquivocationKind::Sequence);
        // A rotated key of the same source stating the same fact again also
        // equivocates (the fact book is per source, not per key).
        let mut w = world();
        let a = report(&w.signer, payload(w.key, 1, 42, NOW - 60));
        assert_eq!(accept(&mut w, &a), Ok(ProofLevel::SourceProven));
        let new_sk = sk("onyx test signed source key 2");
        let k2 = w
            .store
            .register_key(
                &w.admin,
                source_key("example.signed", &new_sk, "data.example.org"),
                &w.reg,
                NOW,
                "test",
            )
            .expect("rotate in");
        let b = report(&new_sk, payload(k2, 1, 43, NOW - 60));
        assert!(is_equivocation(&accept(&mut w, &b)));
        // Both keys involved are suspended (L3R-F3).
        for k in [w.key, k2] {
            assert!(w.store.is_suspended(&k));
        }
    }

    #[test]
    fn every_claim_field_is_bound_to_the_payload() {
        let mut w = world();
        let r = report(&w.signer, payload(w.key, 1, 42, NOW - 60));
        let base = claim_for(&r);
        let mut cases: Vec<(&str, Claim)> = Vec::new();
        let mut c = base.clone();
        c.value = Value::UInt(43);
        cases.push(("value", c));
        let mut c = base.clone();
        c.metric = MetricId("example.signed.other".into());
        cases.push(("metric", c));
        let mut c = base.clone();
        c.subject = Subject::Entity("entity:other".into());
        cases.push(("subject", c));
        let mut c = base.clone();
        c.source.source = SourceId("example.other".into());
        c.rights = rights("example.other");
        cases.push(("source", c));
        let mut c = base.clone();
        c.source.snapshot = Snapshot::Document {
            published_unix: NOW,
            content_hash: r.hash(),
        };
        cases.push(("snapshot time", c));
        let mut c = base.clone();
        c.source.snapshot = Snapshot::Document {
            published_unix: r.payload.observed_unix,
            content_hash: keccak256(b"other doc"),
        };
        cases.push(("snapshot hash", c));
        let mut c = base.clone();
        c.evidence = vec![];
        cases.push(("evidence missing", c));
        let mut c = base.clone();
        c.evidence.push(EvidenceRef {
            kind: "tx".into(),
            content_hash: keccak256(b"x"),
        });
        cases.push(("evidence extra", c));
        let mut c = base.clone();
        c.method = VerificationMethod::SignedSource {
            key: r.payload.key,
            report: keccak256(b"another report"),
        };
        cases.push(("method report", c));
        let mut c = base.clone();
        c.method = VerificationMethod::SignedSource {
            key: keccak256(b"another key"),
            report: r.hash(),
        };
        cases.push(("method key", c));
        for (what, c) in cases {
            let got = w.store.accept_claim(&c, &r, &w.reg, NOW).map(|i| i.level);
            assert!(
                matches!(got, Err(SignedError::ClaimNotBacked(_))),
                "{what}: {got:?}"
            );
        }
        // Another method is not this intake's.
        let mut c = base.clone();
        c.method = VerificationMethod::OfficialSourceMatch;
        assert_eq!(
            w.store.accept_claim(&c, &r, &w.reg, NOW).map(|i| i.level),
            Err(SignedError::NotSignedSource)
        );
        // None of the refusals consumed the sequence.
        assert_eq!(accept(&mut w, &r), Ok(ProofLevel::SourceProven));
    }

    #[test]
    fn a_report_never_crosses_sources() {
        let mut w = world();
        // Source B registers its own key under its own origin.
        let b_sk = sk("onyx test other source key");
        let kb = w
            .store
            .register_key(
                &w.admin,
                source_key("example.other", &b_sk, "feed.other.example"),
                &w.reg,
                NOW,
                "test",
            )
            .expect("register b");
        // A's key used for a payload that names source B.
        let mut p = payload(w.key, 1, 42, NOW);
        p.source = SourceId("example.other".into());
        let r = report(&w.signer, p);
        assert_eq!(accept(&mut w, &r), Err(SignedError::KeySourceMismatch));
        // B's genuine report presented as a claim about source A.
        let mut p = payload(kb, 1, 42, NOW);
        p.source = SourceId("example.other".into());
        let rb = report(&b_sk, p);
        let mut c = claim_for(&rb);
        c.source.source = SourceId("example.signed".into());
        c.rights = rights("example.signed");
        assert!(matches!(
            w.store.accept_claim(&c, &rb, &w.reg, NOW),
            Err(SignedError::ClaimNotBacked(_))
        ));
        // A key for B published under A's origin is refused at registration;
        // a key for an unregistered source too.
        let x = sk("x");
        assert_eq!(
            w.store.register_key(
                &w.admin,
                source_key("example.other", &x, "data.example.org"),
                &w.reg,
                NOW,
                "test"
            ),
            Err(SignedError::OriginMismatch)
        );
        assert!(matches!(
            w.store.register_key(
                &w.admin,
                source_key("example.nowhere", &x, "nowhere.example"),
                &w.reg,
                NOW,
                "test"
            ),
            Err(SignedError::UnknownSource(_))
        ));
        assert_eq!(
            w.store.register_key(
                &w.admin,
                source_key("example.signed", &w.signer, "data.example.org"),
                &w.reg,
                NOW,
                "test"
            ),
            Err(SignedError::KeyAlreadyRegistered)
        );
    }

    #[test]
    fn claim_decode_refuses_signed_source_claims() {
        let w = world();
        let r = report(&w.signer, payload(w.key, 1, 42, NOW));
        let bytes = to_canonical_bytes(&claim_for(&r)).expect("bytes");
        assert!(matches!(
            Claim::decode(&bytes, &w.reg),
            Err(onyx_core::ClaimDecodeError::NeedsSignedSourceIntake)
        ));
    }
}
