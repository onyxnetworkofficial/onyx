//! Level-2 data in the query API (ADR-0027 amendment 4, docs/spec/delivery.md
//! §Observations): `GET /v1/observations/{metric}?subject=..` serves the value
//! a level-2 round settled together with the round's whole public transcript
//! (configuration, observers, commits, reveals). A client never trusts the
//! server for the value: from its OWN copy of the metric's registered feed
//! template it instantiates the spec, recomputes the round id, opens every
//! commit, re-aggregates the reveals into the result hash, and checks that the
//! served answer is the one that hash commits to
//! ([`crate::client::verify_observation`]). With a chain node it can then ask
//! `OnyxRegistry.observationLevel` whether the chain recorded exactly that
//! result for that fact (ADR-0030).
//!
//! The server holds the same transcripts it serves, loaded from a results
//! store file the level-2 round runner writes ([`ObservationStore`]). Every
//! transcript is checked against the store's template at load time with the
//! same function a client uses, so a server never serves a round that does
//! not verify; nothing derived (spec id, round id, result, fact keys) is read
//! from the file.
//!
//! Payments: an observation is billed to the revenue book `(OBSERVATION_ROUND,
//! registry fact key)` (`OnyxRevenue.registerObservation`, docs/spec/revenue.md
//! R-REV-12). That book has no miners, validators or label set, so its whole fee
//! goes to the treasury until observer payouts exist on chain (THREATS
//! L2API-3).

use borsh::BorshDeserialize;
use onyx_coordinator::observe::{
    fact_key, fact_key_dated, onchain_observation_check, pool_hash, rebuild_round, result_hash_of,
    round_id, DrawChain, DrawSeed, ObservationOutcome, ObserveConfig, Observer, ObserverSig,
    OnchainFact, OnchainObservationCheck, RegistryDeployment, TranscriptLogQuery,
};
use onyx_coordinator::sealed::{
    Dismissal, Point, PostedSealed, RevealContext, RevealLogQuery, MAX_SEALED_BYTES,
};
use onyx_core::onchain::{metric_key, registry_fact_key, subject_key, MetricKind};
use onyx_core::{
    keccak256, Answer, FeedTemplate, Hash32, MinerAddress, Observation, ObservationKind,
    ObservationSpec, Redistribution, Subject,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The round number of every observation revenue book: no settlement round
/// ever has it, so `(OBSERVATION_ROUND, fact)` never names a shard.
pub const OBSERVATION_ROUND: u64 = u64::MAX;

/// Version of the results store file this code reads.
pub const STORE_VERSION: u64 = 1;

/// The proof level every served observation has (re-observed, ADR-0027).
pub const OBSERVED_LEVEL: u8 = 2;

/// Keys of a value answer's `recheck` note (CL7, ADR-0057 item 6).
pub const RECHECK_KEYS: &[&str] = &["result", "valueHash", "recordedAt", "matches"];

/// The `recheck` note of a value answer (CL7, ADR-0057 item 6): the paired
/// `.recheck` metric's settled round of the same subject, its result hash,
/// the [`onyx_core::ladder::ladder_value_hash`] of its value, its settle time
/// and whether it is within the recheck template's tolerance of the value.
/// A recheck that disagrees is a note next to the value ("rechecked later:
/// the source now says otherwise"), never a fall: the value keeps its level
/// (ADR-0048). `None` when either answer is no number of its feed (an
/// unavailable recheck says nothing about the value) or the recheck template
/// is no numeric recheck.
pub fn recheck_note(
    value: &ObservationRecord,
    recheck_template: &FeedTemplate,
    recheck: &ObservationRecord,
) -> Option<onyx_coordinator::corrections::RecheckNote> {
    recheck_note_of(
        &value.transcript.answer,
        &value.transcript.subject,
        recheck_template,
        &recheck.transcript.answer,
        &recheck.checked,
    )
}

/// [`recheck_note`] from its parts: the value's answer and subject, and the
/// recheck round's answer and checked proof (a client passes two verified
/// answers, `client::verify_recheck`).
pub fn recheck_note_of(
    value: &Answer,
    subject: &Subject,
    recheck_template: &FeedTemplate,
    recheck: &Answer,
    rc: &Checked,
) -> Option<onyx_coordinator::corrections::RecheckNote> {
    let ObservationKind::Numeric { tolerance, .. } = recheck_template.kind else {
        return None;
    };
    if !matches!(
        recheck_template.subject,
        onyx_core::SubjectRule::Recheck { .. }
    ) || &rc.spec.subject != subject
        || rc.spec.metric != recheck_template.metric
    {
        return None;
    }
    let (Answer::Number(v), Answer::Number(r)) = (value, recheck) else {
        return None;
    };
    let rv = onyx_core::ladder::observed_value(&recheck_template.kind, recheck)?;
    Some(onyx_coordinator::corrections::RecheckNote {
        result: rc.result,
        value_hash: onyx_core::ladder::ladder_value_hash(&rv),
        recorded_at: rc.settled_unix,
        matches: onyx_coordinator::corrections::outcome(*v, *r, tolerance)
            == onyx_coordinator::corrections::Side::Yes,
    })
}

/// The JSON of a [`recheck_note`] (keys [`RECHECK_KEYS`]).
pub fn recheck_json(n: &onyx_coordinator::corrections::RecheckNote) -> Value {
    json!({
        "result": n.result.to_string(),
        "valueHash": n.value_hash.to_string(),
        "recordedAt": n.recorded_at,
        "matches": n.matches,
    })
}

/// The one-step ladder of a level-2 answer (ADR-0048): the value of the fact
/// (metric, subject, window end), level 2 on the round's result hash, at the
/// round's settle time. `None` when the answer is no value of the feed's kind.
pub fn own_ladder(
    spec: &ObservationSpec,
    answer: &Answer,
    result: Hash32,
    settled_unix: u64,
) -> Option<onyx_core::Ladder> {
    let value = onyx_core::ladder::observed_value(&spec.kind, answer)?;
    let key = onyx_core::ladder::value_key(
        &onyx_core::ladder::fact_key(&spec.metric, &spec.subject, spec.window_end),
        &onyx_core::ladder::ladder_value_hash(&value),
    );
    onyx_core::Ladder::new(
        key,
        onyx_core::ladder::Step {
            level: onyx_core::ProofLevel::ReObserved,
            evidence: result,
            at: settled_unix,
        },
    )
    .ok()
}

/// How far ahead of the checker's clock a round's reveal close may lie: a
/// round whose reveals are not closed yet is not settled, so it is never
/// served (L2API-R1-F2). The price clock does not use the reveal close at all
/// (L2API-R2-F2: it is the template-bound window end).
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;

/// `OnyxRegistry.observationLevel(bytes32,bytes32,uint64,bytes32,bytes32)`.
pub const OBSERVATION_LEVEL_CALL: &str =
    "OnyxRegistry.observationLevel(bytes32,bytes32,uint64,bytes32,bytes32)";

/// `OnyxRegistry.drawMatches(...)`: the round's draw is the fact's latest,
/// announced before its beacon with the client's own commitment to pool,
/// spec and config (L2RND-R1, L2RND-R3). A client needs it with level 2.
pub const DRAW_MATCHES_CALL: &str =
    "OnyxRegistry.drawMatches(bytes32,bytes32,uint64,uint32,bytes32,uint64,bytes32)";

/// `OnyxRegistry.drawBudget(bytes32,bytes32)`: the subject's draw bound
/// (L2RND-R2-F1). A client needs it with level 2.
pub const DRAW_BUDGET_CALL: &str = "OnyxRegistry.drawBudget(bytes32,bytes32)";

/// `OnyxRegistry.transcriptComplete(...)`: the served round leaves out no
/// commit posted in its commit phase and no receipted reveal (ADR-0027
/// amendment 8). A client asks it for every served round (the value round
/// and every attempt).
pub const TRANSCRIPT_COMPLETE_CALL: &str =
    "OnyxRegistry.transcriptComplete(bytes32,uint32,bytes32,uint64,uint64,bytes)";

/// One settled round's public transcript: everything a client needs to
/// rebuild its result from its own copy of the template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    /// The fact's subject.
    pub subject: Subject,
    /// The window the round observed from.
    pub window_start: u64,
    /// Timing and economics (part of the round id).
    pub cfg: ObserveConfig,
    /// The pool snapshot the observers were drawn from (L2-F2).
    pub pool: Vec<Observer>,
    /// The draw's public inputs (pool hash, beacon height and hash, attempt).
    pub draw: DrawSeed,
    /// Assigned observers, ascending by address (part of the round id): a
    /// client recomputes them as the draw of `pool` and refuses any other set.
    pub observers: Vec<Observer>,
    /// Every commit posted: observer, commit, the observer's signature
    /// (`observation_commit_message`, ATT-R1-F3).
    pub commits: Vec<(MinerAddress, Hash32, ObserverSig)>,
    /// Every reveal accepted: observer, observation, salt, the observer's
    /// signature (`observation_reveal_message`, ATT-R1-F3).
    pub reveals: Vec<(MinerAddress, Observation, Hash32, ObserverSig)>,
    /// The round's sealed reveal posts and their dismissals (ROADMAP C11);
    /// empty for a draw without a reveal key.
    pub sealed: SealedLog,
    /// The settled value.
    pub answer: Answer,
}

/// What a transcript proves once checked against a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The spec instantiated from the template.
    pub spec: ObservationSpec,
    /// The round id recomputed from spec, config and observers.
    pub round_id: Hash32,
    /// The result hash the reveals aggregate to, and the answer's.
    pub result: Hash32,
    /// The coordinator's fact key (`fact_key` / `fact_key_dated`).
    pub fact: Hash32,
    /// The on-chain fact key (`OnyxRegistry.factKeyOf`).
    pub registry_fact: Hash32,
    /// The pricing decay clock, a pure function of the registered template
    /// and the subject (`FeedTemplate::decay_start`, L2API-R3-F1): a dated
    /// subject ages from the end of the earliest window its lag allows, however
    /// late a round observes it; a fixed subject's fact is one window, which
    /// ages from its own `window_end`. No round input moves it (L2API-R2-F2).
    pub settled_unix: u64,
    /// The arguments of `OnyxRegistry.observationLevel`.
    pub onchain: OnchainObservationCheck,
    /// The round's draw seed; a client checks it on chain with
    /// `OnyxRegistry.drawMatches` (L2RND-R1).
    pub draw: DrawSeed,
    /// The `OnyxRegistry.transcriptComplete` query of the served round
    /// (ADR-0027 amendment 8): no commit or receipted reveal left out.
    pub log: TranscriptLogQuery,
    /// The round's sealed posts and dismissals, checked (ROADMAP C11), and
    /// its `revealLogComplete` query.
    pub reveal_log: SealedChecked,
}

/// Where the level-2 limits are explained (ROADMAP B4).
pub const LIMITS_DOCS: &str = "docs/spec/delivery.md#level-2-limits";

/// THREATS REVLOG-1, as stated on every level-2 answer and offer.
pub const REVLOG_1_TEXT: &str = "The coordinator sees each reveal's value before it signs \
the receipt, so it can refuse receipts for values it dislikes and drop those reveals (or drop \
observers that never posted their commit), steering the value or forcing a re-draw. Bounded by \
the fact's draw budget (3 draws, 3 more per registry-owner reopen); not provable to third \
parties.";

/// THREATS REVLOG-5, as stated on every level-2 answer and offer.
pub const REVLOG_5_TEXT: &str = "While a fact is not closed its rounds are served sealed: the \
revealed list stands in for the reveals and is the server's word, so a withheld receipted reveal \
can be listed as revealed with outcome noResult and transcriptComplete still passes. It is caught \
only once the fact closes and its rounds are served with their reveals.";

/// The known limits of a level-2 answer (ROADMAP B4, delivery.md §Level-2
/// limits): `[{id, applies, text, docs}]` for THREATS REVLOG-1 and
/// REVLOG-5, in that order, on every `/v1/observations` answer, its 402
/// offer, and every `/attempts` answer. `applies` says whether the limit
/// holds for the rounds of this answer: REVLOG-1 always; REVLOG-5 only when
/// `sealed` (the answer serves rounds without their reveals, its fact not
/// closed). The client requires exactly this list when `limits` is present.
pub fn limits_json(sealed: bool) -> Value {
    json!([
        {"id": "REVLOG-1", "applies": true, "text": REVLOG_1_TEXT, "docs": LIMITS_DOCS},
        {"id": "REVLOG-5", "applies": sealed, "text": REVLOG_5_TEXT, "docs": LIMITS_DOCS},
    ])
}

/// Key of the level-2 limits in a 402 offer's `extensions` (B4).
pub const LIMITS_EXTENSION: &str = "limits";

/// The x402 v2 extension value for `limits` (a [`limits_json`] list), in
/// the shape spec §5.1.2 requires of every `extensions` entry: `info`, an
/// object of extension data (`{limits}`), and `schema`, a JSON Schema
/// (draft 2020-12, as the spec's `bazaar` extension uses) of `info`. Purely
/// informational: no facilitator acts on it, and the server ignores an
/// echoed copy in the `PaymentPayload`.
pub fn limits_extension(limits: &Value) -> Value {
    json!({
        "info": { "limits": limits },
        "schema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "limits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "applies": { "type": "boolean" },
                            "text": { "type": "string" },
                            "docs": { "type": "string" }
                        },
                        "required": ["id", "applies", "text", "docs"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["limits"],
            "additionalProperties": false
        }
    })
}

/// Most draws one fact may take (`OnyxRegistry.MAX_DRAW_ATTEMPTS`).
pub const MAX_DRAW_ATTEMPTS: u32 = 64;

/// A published round that settled no value (`SourceUnavailable` or
/// `NoResult`): a [`Transcript`] without an answer (SEC-R2-F1, L2-F2). The
/// server publishes every such round of a fact, so a client accounts for the
/// draws that did not reach a value, not only the one that did. Every commit
/// and reveal is signed by its drawn observer, so a published round is one
/// the observers played, not one the server made up (ATT-R1-F3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// The fact's subject.
    pub subject: Subject,
    /// The window the round observed from.
    pub window_start: u64,
    /// Timing and economics (part of the round id).
    pub cfg: ObserveConfig,
    /// The pool snapshot the observers were drawn from.
    pub pool: Vec<Observer>,
    /// The draw's public inputs.
    pub draw: DrawSeed,
    /// Assigned observers, ascending by address.
    pub observers: Vec<Observer>,
    /// Every commit posted: observer, commit, the observer's signature
    /// (`observation_commit_message`, ATT-R1-F3).
    pub commits: Vec<(MinerAddress, Hash32, ObserverSig)>,
    /// Every reveal accepted: observer, observation, salt, the observer's
    /// signature (`observation_reveal_message`, ATT-R1-F3).
    pub reveals: Vec<(MinerAddress, Observation, Hash32, ObserverSig)>,
    /// The round's sealed reveal posts, their dismissals and its attempt
    /// record (ROADMAP C11); empty for a round from before C11.
    pub sealed: SealedLog,
}

impl Attempt {
    /// An opened round as published when its run failed after the draw's
    /// beacon was fixed (ATT-R2-F3): its signed commits and reveals so far,
    /// so a client accounts for the draw and the fact's re-draw still
    /// verifies (`verify_attempts_onchain`). Err when the reveals so far
    /// would settle a value (the run failed after the reveals): such a round
    /// is never published as a failed attempt. Leaving its reveals out would
    /// drop reveals whose receipts are on chain (clients refuse it, REVLOG-3,
    /// review REVLOG-R2 MEDIUM-2), and keeping them would give a value no
    /// round recorded away for free; the coordinator finishes recording it.
    pub fn of_failed_round(
        template: &FeedTemplate,
        round: &onyx_coordinator::observe::ObservationRound,
    ) -> Result<Attempt, String> {
        let spec = round.spec();
        let a = Attempt {
            subject: spec.subject.clone(),
            window_start: spec.window_start,
            cfg: round.config(),
            pool: round.pool().to_vec(),
            draw: round.draw(),
            observers: round.observers().to_vec(),
            commits: round.signed_commits(),
            reveals: round.signed_reveals(),
            sealed: SealedLog::of_round(round, None),
        };
        check_attempt(template, &a, a.cfg.reveal_close).map_err(|e| {
            format!(
                "not a failed attempt ({e}): finish recording the round, never drop its reveals"
            )
        })?;
        Ok(a)
    }
}

impl Transcript {
    /// The round without its answer.
    pub fn attempt(&self) -> Attempt {
        Attempt {
            subject: self.subject.clone(),
            window_start: self.window_start,
            cfg: self.cfg,
            pool: self.pool.clone(),
            draw: self.draw,
            observers: self.observers.clone(),
            commits: self.commits.clone(),
            reveals: self.reveals.clone(),
            sealed: self.sealed.clone(),
        }
    }
}

/// What an [`Attempt`] proves once checked against a template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedAttempt {
    /// `SourceUnavailable` or `NoResult`, as the reveals aggregate.
    pub outcome: ObservationOutcome,
    /// The round id recomputed from spec, config, observers and draw.
    pub round_id: Hash32,
    /// The result hash of the outcome.
    pub result: Hash32,
    /// The coordinator's fact key.
    pub fact: Hash32,
    /// The on-chain fact key (`OnyxRegistry.factKeyOf`).
    pub registry_fact: Hash32,
    /// The round's window end.
    pub window_end: u64,
    /// The draw commitment inputs (`draw_commit`) and keys.
    pub onchain: OnchainObservationCheck,
    /// The round's draw seed; a client checks it with `OnyxRegistry.drawOf`.
    pub draw: DrawSeed,
    /// The round was served sealed (ATT-R1-F2): its reveals were withheld, so
    /// `outcome` (and `result`, its hash) is the server's word, not rebuilt
    /// from reveals. Draw, observers, commits and round id are still checked.
    pub sealed: bool,
    /// The `OnyxRegistry.transcriptComplete` query of the served round; for
    /// a sealed round, its `revealed` list stands for the reveals.
    pub log: TranscriptLogQuery,
    /// The round's sealed posts, dismissals and record, checked (ROADMAP
    /// C11), and its `revealLogComplete` / `attemptMatches` query.
    pub reveal_log: SealedChecked,
}

/// The outcome's name in a response (`value`, `sourceUnavailable`,
/// `noResult`).
pub fn outcome_text(o: &ObservationOutcome) -> &'static str {
    match o {
        ObservationOutcome::Value(_) => "value",
        ObservationOutcome::SourceUnavailable => "sourceUnavailable",
        ObservationOutcome::NoResult => "noResult",
    }
}

/// `OnyxRegistry.drawRevealKey(bytes32,uint32)`: the reveal key a draw
/// snapshotted (ROADMAP C11). A client asks it for every served round: a draw
/// with a key must come with its sealed posts.
pub const DRAW_REVEAL_KEY_CALL: &str = "OnyxRegistry.drawRevealKey(bytes32,uint32)";

/// `OnyxRegistry.drawTimeLocked(bytes32,uint32)` (ROADMAP C51): whether the
/// draw was announced with time-locks required. A client asks it for every
/// served round: a time-locked draw must be served with its time-lock read.
pub const DRAW_TIME_LOCKED_CALL: &str = "OnyxRegistry.drawTimeLocked(bytes32,uint32)";

/// `OnyxRegistry.revealLogComplete(...)`: the served round accounts for every
/// sealed post in its reveal phase, revealed or dismissed (ADR-0027 amendment
/// 10 item 6). A client asks it for every round of a draw with a reveal key.
pub const REVEAL_LOG_COMPLETE_CALL: &str =
    "OnyxRegistry.revealLogComplete(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes)";

/// `OnyxRegistry.attemptMatches(...)`: the coordinator recorded exactly this
/// round before serving it (amendment 10 item 7, REVLOG-5). A client requires
/// it for every round served sealed and every round without a value of a draw
/// with a reveal key.
pub const ATTEMPT_MATCHES_CALL: &str =
    "OnyxRegistry.attemptMatches(bytes32,uint32,bytes32,uint64,uint64,uint64,bytes32,bytes)";

/// A served round's sealed reveal posts, their dismissals and its attempt
/// record (ADR-0027 amendment 10, ROADMAP C11), as the coordinator read them
/// from one block past `reveal_close`. Empty (the default) for a draw without
/// a reveal key and a round from before C11.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SealedLog {
    /// The draw's reveal key (`drawRevealKey`); `None` for a draw without one.
    pub reveal_key: Option<Point>,
    /// Every sealed post in the round's reveal phase (`commit_close`,
    /// `reveal_close`], ascending by observer, as the registry holds it
    /// (`sealedRevealOf`, `sealedKeyProofOf`).
    pub posts: Vec<(MinerAddress, PostedSealed)>,
    /// The posts that are garbage, each with its proof (`S` and the
    /// Chaum-Pedersen proof), ascending by observer.
    pub dismissals: Vec<(MinerAddress, Dismissal)>,
    /// The digest the coordinator recorded with `recordAttempt` for a round
    /// without a value (`attemptDigest`); `None` for a value round.
    pub record: Option<Hash32>,
    /// A TIME-LOCKED draw's read (ROADMAP C51, ADR-0027 amendment 14): the
    /// lock round, its beacon signature and the time-lock opening of each
    /// dismissal whose time-lock opened. `None` for a draw that is not
    /// time-locked; a client requires it to match the chain's `drawTimeLocked`.
    pub time_lock: Option<onyx_coordinator::timelock::TimeLockRead>,
}

impl SealedLog {
    /// What `round` serves (ROADMAP C11): its sealed snapshot's reveal key,
    /// in-phase posts and dismissals, and `record`, the attempt digest it was
    /// recorded with (none for a value round).
    pub fn of_round(
        round: &onyx_coordinator::observe::ObservationRound,
        record: Option<Hash32>,
    ) -> SealedLog {
        let cfg = round.config();
        match round.sealed_snapshot() {
            None => SealedLog {
                record,
                ..SealedLog::default()
            },
            Some(s) => SealedLog {
                reveal_key: Some(s.reveal_key),
                posts: s
                    .in_phase(cfg.commit_close, cfg.reveal_close)
                    .cloned()
                    .collect(),
                dismissals: s.dismissals.clone(),
                record,
                time_lock: s.timelock.clone(),
            },
        }
    }

    fn is_empty(&self) -> bool {
        *self == SealedLog::default()
    }
}

/// What a round's [`SealedLog`] proves once checked (ROADMAP C11), and the
/// `revealLogComplete` / `attemptMatches` query a client asks the chain:
/// built from the client's own draw (every drawn observer), the round's
/// config (the window) and the recomputed round id (review C4-R1-2), never
/// from what the server says about them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedChecked {
    /// The query: every drawn observer with its served commit and status.
    pub query: RevealLogQuery,
    /// The reveal key the served round names (`None`: none). The client
    /// requires the chain's `drawRevealKey` to be exactly this.
    pub reveal_key: Option<Point>,
    /// The served posts; the client requires each to be the chain's.
    pub posts: Vec<(MinerAddress, PostedSealed)>,
    /// The result hash the round is recorded with (`attemptMatches`).
    pub result: Hash32,
    /// A value round (recorded with `recordObservation`, not `recordAttempt`).
    pub value: bool,
    /// Served sealed: its outcome and `revealed` list are the server's word
    /// until a record pins them (REVLOG-5).
    pub served_sealed: bool,
    /// Served as a time-locked draw (ROADMAP C51): the client requires the
    /// chain's `drawTimeLocked` to be exactly this.
    pub time_locked: bool,
}

/// Check a round's sealed log (ROADMAP C11; the same function on the server at
/// load and in the client): with no reveal key, no post or dismissal; else
/// every post of a drawn observer, in the round's reveal phase, strictly
/// ascending, its stored key proof valid for this round (the registry checked
/// it; a server cannot serve another `R`, C4-R3-2); every dismissal of a
/// posted observer whose reveal is not served (or listed), strictly
/// ascending, and checking against the reveal key: the proof fixes the
/// decryption, and its plaintext must NOT be a valid reveal opening the served
/// commit (a false dismissal, C4-2, or one replayed from another round,
/// attempt or observer, C11-3, is refused); every post revealed or
/// dismissed. Then the query of every drawn observer (statuses from
/// `revealed` and the dismissals) and, for a round without a value, a served
/// record must be its digest with `result`; a value round carries none.
#[allow(clippy::too_many_arguments)]
fn check_sealed_log(
    template: &FeedTemplate,
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    draw: &DrawSeed,
    round_id: Hash32,
    registry_fact: Hash32,
    observers: &[Observer],
    commits: &[(MinerAddress, Hash32, ObserverSig)],
    revealed: &BTreeSet<MinerAddress>,
    log: &SealedLog,
    result: Hash32,
    value: bool,
) -> Result<SealedChecked, ObservationError> {
    let bad = |m: &str| ObservationError::Transcript(format!("sealed posts: {m}"));
    let drawn: BTreeSet<MinerAddress> = observers.iter().map(|o| o.address).collect();
    let served: BTreeMap<MinerAddress, Hash32> = commits.iter().map(|(a, h, _)| (*a, *h)).collect();
    if log.reveal_key.is_none() && (!log.posts.is_empty() || !log.dismissals.is_empty()) {
        return Err(bad("posts or dismissals without a reveal key"));
    }
    if !log.posts.windows(2).all(|w| w[0].0 < w[1].0)
        || !log.dismissals.windows(2).all(|w| w[0].0 < w[1].0)
    {
        return Err(bad("posts or dismissals not strictly ascending"));
    }
    let posts: BTreeMap<MinerAddress, &PostedSealed> =
        log.posts.iter().map(|(a, p)| (*a, p)).collect();
    for (a, p) in &log.posts {
        if !drawn.contains(a) {
            return Err(bad("a post of an observer not drawn"));
        }
        if p.posted_at <= cfg.commit_close || p.posted_at > cfg.reveal_close {
            return Err(bad("a post outside the reveal phase"));
        }
        if p.ciphertext.is_empty()
            || p.ciphertext.len() > MAX_SEALED_BYTES
            || (log.time_lock.is_some()
                && p.ciphertext.len() <= onyx_coordinator::sealed::TIME_LOCKED_HEAD_BYTES)
        {
            return Err(bad("a ciphertext the registry never takes"));
        }
        if !onyx_coordinator::sealed::key_proof_valid(
            &p.ephemeral,
            &round_id,
            draw.attempt,
            a,
            &p.key_proof,
        ) {
            return Err(bad("a post whose key proof fails"));
        }
    }
    // ROADMAP C51: a time-locked round names the lock round of ITS window, a
    // signature of it that verifies against the pinned quicknet key, and its
    // openings in order, each of a dismissal; every dismissal is then checked
    // as a time-locked one (an opening must re-encrypt to the post's time-lock
    // byte for byte, `open_canonical`; without one, the time-lock must not
    // open under the signature). ROADMAP C52: it names the far round the
    // checker derives from its OWN template (the fact's last observable day),
    // and a dismissal also stands when the far copy does not re-encrypt from
    // the unwrapped body key.
    let locked = match &log.time_lock {
        None => None,
        Some(t) => {
            use onyx_coordinator::timelock::TimeLock as _;
            if log.reveal_key.is_none() {
                return Err(bad("a time-lock without a reveal key"));
            }
            let q = onyx_coordinator::timelock::QuicknetTimeLock::pinned()
                .map_err(|e| bad(&e.to_string()))?;
            if onyx_coordinator::timelock::lock_round(q.chain(), cfg.reveal_close)
                != Some(t.lock_round)
            {
                return Err(bad(
                    "a time-lock read for another lock round than the window's",
                ));
            }
            if !q.verify_signature(t.lock_round, &t.signature) {
                return Err(bad("a lock-round signature that is not drand quicknet's"));
            }
            let rounds = onyx_coordinator::timelock::LockRounds::of_round(
                q.chain(),
                template,
                spec,
                cfg.reveal_close,
            )
            .map_err(|e| bad(&format!("no far round for this round ({e})")))?;
            if t.far_round != rounds.far() {
                return Err(bad(
                    "a time-lock read for another far round than the template's",
                ));
            }
            if !t.openings.windows(2).all(|w| w[0].0 < w[1].0)
                || !t
                    .openings
                    .iter()
                    .all(|(a, _)| log.dismissals.iter().any(|(d, _)| d == a))
            {
                return Err(bad("time-lock openings out of order or of no dismissal"));
            }
            Some((q, t, rounds))
        }
    };
    let mut dismissed = BTreeSet::new();
    if let Some(key) = &log.reveal_key {
        for (a, d) in &log.dismissals {
            let p = posts
                .get(a)
                .ok_or_else(|| bad("a dismissal without its post"))?;
            if revealed.contains(a) {
                return Err(bad("a dismissal of a revealed observer"));
            }
            let ctx = RevealContext {
                round_id,
                attempt: draw.attempt,
                spec,
                commit: served.get(a).copied(),
            };
            match &locked {
                None => {
                    onyx_coordinator::sealed::check_dismissal_posted(key, a, p, &ctx, d)
                        .map_err(|e| bad(&format!("a dismissal that does not check ({e})")))?;
                }
                Some((q, t, rounds)) => {
                    let opening = t.openings.iter().find(|(o, _)| o == a).map(|(_, o)| *o);
                    onyx_coordinator::timelock::check_timelocked_dismissal(
                        q,
                        key,
                        a,
                        p,
                        &ctx,
                        *rounds,
                        Some(&t.signature),
                        &onyx_coordinator::timelock::TimedDismissal {
                            dismissal: *d,
                            opening,
                        },
                    )
                    .map_err(|e| {
                        bad(&format!(
                            "a time-locked dismissal that does not check ({e})"
                        ))
                    })?;
                }
            }
            dismissed.insert(*a);
        }
    }
    if posts
        .keys()
        .any(|a| !revealed.contains(a) && !dismissed.contains(a))
    {
        return Err(bad("a post neither revealed nor dismissed"));
    }
    // Review C11-R1-3: a draw with a reveal key is sealed only. Every observer
    // the round serves (or lists) as revealed must have an in-phase post, which
    // the client then requires to be the chain's: a coordinator that saw every
    // value cannot add a late post's, an early post's or an off-chain reveal.
    if log.reveal_key.is_some() && revealed.iter().any(|a| !posts.contains_key(a)) {
        return Err(bad(
            "a revealed observer without an in-phase sealed post (a keyed draw is sealed only)",
        ));
    }
    // Review C51-R2-1: in a time-locked draw every REVEALED post holds a
    // time-lock that opens canonically under the verified lock-round signature
    // (anyone checks it; no `S` needed). The coordinator takes a reveal only
    // through the lock round (`open_timelocked`), so a plain amendment 10 post,
    // one locked to another round, or junk in front is never served as revealed:
    // the registry's length rule cannot tell, this check can.
    if let Some((q, t, _)) = &locked {
        for a in revealed {
            let opens = posts
                .get(a)
                .and_then(|p| p.ciphertext.get(..onyx_coordinator::timelock::TLOCK_LEN))
                .and_then(|l| <onyx_coordinator::timelock::Tlock>::try_from(l).ok())
                .and_then(|l| {
                    onyx_coordinator::timelock::open_canonical(q, t.lock_round, &t.signature, &l)
                })
                .is_some();
            if !opens {
                return Err(bad(
                    "a revealed post whose time-lock does not open under the lock round",
                ));
            }
        }
    }
    let query = onyx_coordinator::sealed::reveal_log_query(
        registry_fact,
        spec,
        cfg,
        draw.attempt,
        round_id,
        observers,
        commits,
        revealed,
        &dismissed,
    );
    match (value, log.record) {
        (true, Some(_)) => return Err(bad("a value round with an attempt record")),
        (false, Some(d)) if d != query.digest(&result) => {
            return Err(bad("the record is not this round's digest"))
        }
        _ => {}
    }
    Ok(SealedChecked {
        query,
        reveal_key: log.reveal_key,
        posts: log.posts.clone(),
        result,
        value,
        served_sealed: false,
        time_locked: log.time_lock.is_some(),
    })
}

/// The coordinator's and the registry's fact keys of `spec`.
fn fact_keys(
    template: &FeedTemplate,
    spec: &ObservationSpec,
    subject: &Subject,
) -> (Hash32, Hash32) {
    let dated = template.is_dated();
    let fact = if dated {
        fact_key_dated(spec)
    } else {
        fact_key(spec)
    };
    let registry_fact = registry_fact_key(
        &metric_key(&template.metric),
        &template.id(),
        &subject_key(subject),
        spec.window_end,
        dated,
    );
    (fact, registry_fact)
}

/// Check a round that settled no value against `template` at `now`, with
/// every rule [`check`] applies to a value round (clock, round parameters,
/// draw, commits, reveals, aggregation). Refuses a round whose reveals settle
/// a value: that round is served as the fact's value, never as an attempt.
pub fn check_attempt(
    template: &FeedTemplate,
    a: &Attempt,
    now: u64,
) -> Result<CheckedAttempt, ObservationError> {
    let revealed: BTreeSet<MinerAddress> = a.reveals.iter().map(|(o, _, _, _)| *o).collect();
    check_attempt_as(template, a, now, &revealed, None)
}

/// [`check_attempt`] with `revealed` the observers whose reveals are served
/// or, for a round served sealed, listed; and `sealed_outcome` the outcome a
/// sealed round is served with (its result is that outcome's hash).
fn check_attempt_as(
    template: &FeedTemplate,
    a: &Attempt,
    now: u64,
    revealed: &BTreeSet<MinerAddress>,
    sealed_outcome: Option<ObservationOutcome>,
) -> Result<CheckedAttempt, ObservationError> {
    if a.cfg.reveal_close > now.saturating_add(MAX_CLOCK_SKEW_SECS) {
        return Err(ObservationError::NotSettledYet {
            reveal_close: a.cfg.reveal_close,
            now,
        });
    }
    if a.draw.attempt >= MAX_DRAW_ATTEMPTS {
        return Err(ObservationError::Transcript(
            "draw attempt out of range".into(),
        ));
    }
    let (onchain, outcome) = rebuild_round(
        template,
        MetricKind::of(template),
        &template.id(),
        &a.subject,
        a.window_start,
        &a.cfg,
        &a.pool,
        &a.draw,
        &a.observers,
        &a.commits,
        &a.reveals,
    )
    .map_err(|e| ObservationError::Transcript(e.to_string()))?;
    if matches!(outcome, ObservationOutcome::Value(_)) {
        return Err(ObservationError::Malformed(
            "an attempt whose reveals settle a value".into(),
        ));
    }
    let spec = template
        .instantiate(&a.subject, a.window_start)
        .map_err(|e| ObservationError::Transcript(e.to_string()))?;
    let (fact, registry_fact) = fact_keys(template, &spec, &a.subject);
    let rid = round_id(&spec.id(), &a.cfg, &a.observers, &a.draw);
    let log = TranscriptLogQuery::of(
        registry_fact,
        &spec,
        &a.cfg,
        &a.draw,
        rid,
        &a.observers,
        &a.commits,
        revealed,
    );
    let mut onchain = onchain;
    let (outcome, served_sealed) = match sealed_outcome {
        Some(o) => {
            onchain.result = result_hash_of(&onchain.spec_id, &rid, &o);
            (o, true)
        }
        None => (outcome, false),
    };
    let mut reveal_log = check_sealed_log(
        template,
        &spec,
        &a.cfg,
        &a.draw,
        rid,
        registry_fact,
        &a.observers,
        &a.commits,
        revealed,
        &a.sealed,
        onchain.result,
        false,
    )?;
    reveal_log.served_sealed = served_sealed;
    Ok(CheckedAttempt {
        log,
        outcome,
        round_id: rid,
        result: onchain.result,
        fact,
        registry_fact,
        window_end: spec.window_end,
        onchain,
        draw: a.draw,
        sealed: served_sealed,
        reveal_log,
    })
}

/// Check a round served sealed (ATT-R1-F2): `a` without reveals (its
/// `reveals` must be empty), `revealed` the observers the server says
/// revealed, `outcome` the outcome it says the round settled. Every rule of
/// [`check_attempt`] that does not read a reveal applies (clock, round
/// parameters, draw, commits); `revealed` must be committed observers in
/// strictly ascending order; `outcome` must not be a value. The outcome is
/// bound to the round id by its result hash, but nothing proves the reveals
/// aggregate to it: that is the price of not leaking a value a later round of
/// the same fact may sell.
pub fn check_sealed_attempt(
    template: &FeedTemplate,
    a: &Attempt,
    revealed: &[MinerAddress],
    outcome: ObservationOutcome,
    now: u64,
) -> Result<CheckedAttempt, ObservationError> {
    if !a.reveals.is_empty() {
        return Err(ObservationError::Malformed(
            "a sealed round with reveals".into(),
        ));
    }
    if matches!(outcome, ObservationOutcome::Value(_)) {
        return Err(ObservationError::Malformed(
            "a sealed round that settled a value".into(),
        ));
    }
    let committed: BTreeSet<MinerAddress> = a.commits.iter().map(|(o, _, _)| *o).collect();
    if revealed.windows(2).any(|w| w[0] >= w[1]) || revealed.iter().any(|o| !committed.contains(o))
    {
        return Err(ObservationError::Transcript(
            "revealed: not committed observers in ascending order".into(),
        ));
    }
    // With no reveals the rebuild checks everything but the aggregation; the
    // listed observers stand for the reveals and the served outcome for the
    // rebuilt one (ROADMAP C11: a record pins both, the client requires it).
    let listed: BTreeSet<MinerAddress> = revealed.iter().copied().collect();
    check_attempt_as(template, a, now, &listed, Some(outcome))
}

/// Seal a website round summary (an entry of `static/level2/*.json`,
/// ATT-R2-F1): drop the value, the reveals (observations, page hashes,
/// salts), the statuses and the raw page, keep the signed `commits`
/// (observer, commit, commit signature) and mark it `sealed`. What the free
/// `/attempts` answer withholds for a fact that is not closed, and what a
/// public website build withholds for a value still sold by the paid API;
/// the website then checks the draw, the round id and every commit
/// signature. The result hash goes too (SIG-R1-F2): it is an unsalted
/// function of the value, so a small value is found by enumeration.
pub fn seal_site_round(v: &mut Value, commits: Vec<Value>) {
    if let Some(m) = v.as_object_mut() {
        for k in [
            "reveals",
            "statuses",
            "agreed",
            "response_body",
            "result_hash",
        ] {
            m.remove(k);
        }
        m.insert("value".into(), json!(""));
        m.insert("sealed".into(), json!(true));
        m.insert("commits".into(), json!(commits));
    }
}

/// Why a transcript or a store is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObservationError {
    /// The transcript does not rebuild into a result under this template.
    #[error("the transcript does not verify: {0}")]
    Transcript(String),
    /// The reveals aggregate to another value than the served answer.
    #[error("the served answer is not the value the reveals settle")]
    ValueMismatch,
    /// A field is missing or not well formed.
    #[error("malformed: {0}")]
    Malformed(String),
    /// The round's reveal close (its settled time) is after the checker's
    /// clock: not settled yet, or a price clock set in the future.
    #[error("the round settles in the future (reveal close {reveal_close}, now {now})")]
    NotSettledYet {
        /// The transcript's reveal close.
        reveal_close: u64,
        /// The checker's clock.
        now: u64,
    },
    /// The store's template is not the one pinned (or registered on chain) for
    /// its metric (L2API-R4-F1): its lag, window or rights could move the
    /// price clock without changing any spec, round id or result.
    #[error("the store's template for {metric} is not the pinned one: {why}")]
    TemplateNotPinned {
        /// The store's metric.
        metric: String,
        /// No pin, another id, or the registry's answer.
        why: String,
    },
}

/// The template ids a server will load, by metric (L2API-R4-F1): every store
/// must carry exactly the pinned template of its metric. The operator sets the
/// pins from the reviewed registration (`OnyxRegistry.metrics`), not from the
/// store file, and [`check_registered`] re-reads them on chain when the store
/// names a registry.
pub type TemplatePins = BTreeMap<String, Hash32>;

/// Calldata of the public getter `OnyxRegistry.metrics(bytes32)` for the
/// template's metric.
pub fn registry_metric_calldata(template: &FeedTemplate) -> Vec<u8> {
    let mut d = keccak256(b"metrics(bytes32)").0[..4].to_vec();
    d.extend_from_slice(&metric_key(&template.metric).0);
    d
}

/// Check the return of `OnyxRegistry.metrics(metricKey)` (three ABI words:
/// `kind` uint8, `template` bytes32, `registeredAt` uint64): the metric must be
/// registered with exactly this template and the kind its subject rule implies
/// (`MetricKind::of`, REG-R2-F2).
pub fn check_registered(template: &FeedTemplate, ret: &[u8]) -> Result<(), ObservationError> {
    let refuse = |why: &str| ObservationError::TemplateNotPinned {
        metric: template.metric.0.clone(),
        why: why.into(),
    };
    if ret.len() != 96 {
        return Err(refuse("the registry answer is not three words"));
    }
    let word = |i: usize| &ret[i * 32..(i + 1) * 32];
    let small = |w: &[u8]| -> Option<u64> {
        w[..24]
            .iter()
            .all(|b| *b == 0)
            .then(|| u64::from_be_bytes(w[24..].try_into().unwrap_or([0; 8])))
    };
    let kind = small(word(0)).ok_or_else(|| refuse("kind is not a uint8"))?;
    let registered_at = small(word(2)).ok_or_else(|| refuse("registeredAt is not a uint64"))?;
    if registered_at == 0 {
        return Err(refuse("the metric is not registered on chain"));
    }
    if kind != MetricKind::of(template) as u64 {
        return Err(refuse("registered with another kind"));
    }
    if word(1) != template.id().0.as_slice() {
        return Err(refuse("registered with another template"));
    }
    Ok(())
}

/// Read `OnyxRegistry.metrics` for `feed` at `latest` through `rpc` and
/// [`check_registered`] it. Only for a store that names a registry on the
/// node's own chain (`chain_id`); a pointer to another chain is refused.
pub async fn verify_registered(
    rpc: &dyn crate::roots::JsonRpc,
    chain_id: u64,
    registry: Option<([u8; 20], Hash32)>,
    feed: &ObservationFeed,
) -> Result<(), ObservationError> {
    let Some(p) = &feed.onchain else {
        return Ok(());
    };
    let refuse = |why: String| ObservationError::TemplateNotPinned {
        metric: feed.template.metric.0.clone(),
        why,
    };
    if p.chain_id != chain_id {
        return Err(refuse(format!(
            "the store names chain {}, the node reads chain {chain_id}",
            p.chain_id
        )));
    }
    // The registry the store names must be the deployment's, and its code the
    // pinned build (AUD-2-F8): a store file alone never chooses the contract
    // the node trusts or the address it serves to clients.
    let Some((address, code_hash)) = registry else {
        return Err(refuse(
            "the store names a registry but the deployment manifest has none".into(),
        ));
    };
    if p.registry != address {
        return Err(refuse(format!(
            "the store names registry {}, the deployment's is {}",
            hex0x(&p.registry),
            hex0x(&address)
        )));
    }
    let code = rpc
        .call("eth_getCode", json!([hex0x(&p.registry), "latest"]))
        .await
        .map_err(|e| refuse(format!("registry code read failed: {e}")))?;
    let code = bytes_of(&code, "registry code").map_err(|e| refuse(e.to_string()))?;
    if code.is_empty() || onyx_core::keccak256(&code) != code_hash {
        return Err(refuse(
            "the registry's runtime code is not the pinned build (registryCodeHash)".into(),
        ));
    }
    let call = json!([
        {"to": hex0x(&p.registry), "data": hex0x(&registry_metric_calldata(&feed.template))},
        "latest"
    ]);
    let ret = rpc
        .call("eth_call", call)
        .await
        .map_err(|e| refuse(format!("registry read failed: {e}")))?;
    let bytes = bytes_of(&ret, "metrics return").map_err(|e| refuse(e.to_string()))?;
    check_registered(&feed.template, &bytes)
}

/// Check `t` against `template` (the caller's own copy) at the checker's
/// clock `now`: apply every rule a round must meet to open (config, observer
/// count and order, bonds: `check_round_params`, L2API-R1-F1), refuse a
/// reveal close after `now` + [`MAX_CLOCK_SKEW_SECS`] (L2API-R1-F2),
/// instantiate the spec, open every commit, re-aggregate the reveals, and bind
/// `t.answer` to the result hash. The same check runs on the server (at load)
/// and in the client. Without `OnyxRegistry.observationLevel` it proves the
/// transcript is consistent and well formed, not who the observers were
/// (THREATS L2-F2, L2API-1).
pub fn check(
    template: &FeedTemplate,
    t: &Transcript,
    now: u64,
) -> Result<Checked, ObservationError> {
    if t.cfg.reveal_close > now.saturating_add(MAX_CLOCK_SKEW_SECS) {
        return Err(ObservationError::NotSettledYet {
            reveal_close: t.cfg.reveal_close,
            now,
        });
    }
    let onchain = onchain_observation_check(
        template,
        MetricKind::of(template),
        &template.id(),
        &t.subject,
        t.window_start,
        &t.cfg,
        &t.pool,
        &t.draw,
        &t.observers,
        &t.commits,
        &t.reveals,
    )
    .map_err(|e| ObservationError::Transcript(e.to_string()))?;
    let spec = template
        .instantiate(&t.subject, t.window_start)
        .map_err(|e| ObservationError::Transcript(e.to_string()))?;
    let spec_id = spec.id();
    let rid = round_id(&spec_id, &t.cfg, &t.observers, &t.draw);
    let served = result_hash_of(&spec_id, &rid, &ObservationOutcome::Value(t.answer.clone()));
    if served != onchain.result {
        return Err(ObservationError::ValueMismatch);
    }
    let (fact, registry_fact) = fact_keys(template, &spec, &t.subject);
    let log = TranscriptLogQuery::of(
        registry_fact,
        &spec,
        &t.cfg,
        &t.draw,
        rid,
        &t.observers,
        &t.commits,
        &t.reveals.iter().map(|(o, _, _, _)| *o).collect(),
    );
    let settled_unix = template
        .decay_start(&t.subject, spec.window_end)
        .ok_or_else(|| ObservationError::Transcript("no decay start for this subject".into()))?;
    let reveal_log = check_sealed_log(
        template,
        &spec,
        &t.cfg,
        &t.draw,
        rid,
        registry_fact,
        &t.observers,
        &t.commits,
        &t.reveals.iter().map(|(o, _, _, _)| *o).collect(),
        &t.sealed,
        served,
        true,
    )?;
    Ok(Checked {
        reveal_log,
        settled_unix,
        round_id: rid,
        result: served,
        fact,
        registry_fact,
        spec,
        onchain,
        draw: t.draw,
        log,
    })
}

/// The text a subject is asked for by (`?subject=`): its inner id, e.g.
/// `us.treasury.federal_debt:2026-10-08` or `entity:okx`.
pub fn subject_text(s: &Subject) -> &str {
    match s {
        Subject::Entity(x)
        | Subject::Address(x)
        | Subject::Instrument(x)
        | Subject::Aggregate(x) => x,
    }
}

/// `answer` as text: a decimal at the feed's scale for a number, `0x` hex for
/// bytes. Bound by the client (it recomputes it from the answer).
pub fn answer_text(kind: ObservationKind, answer: &Answer) -> String {
    match (kind, answer) {
        (ObservationKind::Numeric { scale, .. }, Answer::Number(n)) => decimal(*n, scale),
        (_, Answer::Bytes(b)) => format!("0x{}", hex::encode(b)),
        (_, Answer::Number(n)) => n.to_string(),
        (_, Answer::Unavailable) => "unavailable".into(),
    }
}

/// `mantissa` at `scale` decimals, exactly (no floats, ADR-0009).
pub fn decimal(mantissa: i128, scale: u8) -> String {
    let neg = mantissa < 0;
    let digits = mantissa.unsigned_abs().to_string();
    let scale = usize::from(scale);
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (int, frac) = padded.split_at(padded.len() - scale);
    let sign = if neg { "-" } else { "" };
    if scale == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    }
}

/// Where a client may check the fact on chain (a pointer only: the client
/// uses its own node and its own recomputed arguments).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnchainPointer {
    /// Chain id.
    pub chain_id: u64,
    /// `OnyxRegistry` address.
    pub registry: [u8; 20],
}

/// One served round: its transcript and what it proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationRecord {
    /// The transcript.
    pub transcript: Transcript,
    /// Checked at load.
    pub checked: Checked,
}

/// One published round without a value: its transcript and what it proves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRecord {
    /// The transcript.
    pub attempt: Attempt,
    /// Checked at load.
    pub checked: CheckedAttempt,
}

/// A level-2 metric the server serves: its registered template and the
/// rounds it settled, checked at load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationFeed {
    /// The metric's registered feed template.
    pub template: FeedTemplate,
    /// Records by (subject text, window end). Add through [`Self::add`]: it
    /// keeps the fact and subject indexes below in step (AUD-2-F10).
    pub records: BTreeMap<(String, u64), ObservationRecord>,
    /// Where the facts are recorded on chain, when they are.
    pub onchain: Option<OnchainPointer>,
    /// Fact keys of the records added through [`Self::add`].
    facts: BTreeSet<Hash32>,
    /// Subject text -> the subject that owns it, for the records added
    /// through [`Self::add`] and [`Self::add_attempt`].
    subjects: BTreeMap<String, Subject>,
    /// Fact key -> the draw attempt of its value round.
    value_attempts: BTreeMap<Hash32, u32>,
    /// Fact key -> attempt -> a round without a value (SEC-R2-F1).
    attempts: BTreeMap<Hash32, BTreeMap<u32, AttemptRecord>>,
    /// (subject text, window end) -> fact key, for the attempts.
    attempt_facts: BTreeMap<(String, u64), Hash32>,
}

impl ObservationFeed {
    /// A feed for `template` (validated) with no records.
    pub fn new(template: FeedTemplate) -> Result<Self, ObservationError> {
        template
            .validate()
            .map_err(|e| ObservationError::Malformed(format!("template: {e}")))?;
        Ok(ObservationFeed {
            template,
            records: BTreeMap::new(),
            onchain: None,
            facts: BTreeSet::new(),
            subjects: BTreeMap::new(),
            value_attempts: BTreeMap::new(),
            attempts: BTreeMap::new(),
            attempt_facts: BTreeMap::new(),
        })
    }

    /// The metric's name (`template.metric`).
    pub fn metric(&self) -> &str {
        &self.template.metric.0
    }

    /// Check `t` at `now` and add it. Refuses a transcript that does not
    /// verify, a second round for the same fact, and a subject whose text
    /// another subject variant already uses.
    /// Indexed by fact key and subject text, so a load of n rounds costs
    /// O(n log n), not O(n^2) (AUD-2-F10).
    pub fn add(&mut self, t: Transcript, now: u64) -> Result<(), ObservationError> {
        let checked = check(&self.template, &t, now)?;
        let text = subject_text(&t.subject).to_owned();
        let other_subject = self.subjects.get(&text).is_some_and(|s| *s != t.subject);
        if self.facts.contains(&checked.fact) || other_subject {
            return Err(ObservationError::Malformed(
                "a second round for one fact, or one subject text for two subjects".into(),
            ));
        }
        // The value round is the fact's latest draw (`drawMatches`): no
        // published attempt may come at or after it.
        let later = self
            .attempts
            .get(&checked.fact)
            .and_then(|m| m.keys().next_back())
            .is_some_and(|a| *a >= checked.draw.attempt);
        if later {
            return Err(ObservationError::Malformed(
                "a round without a value at or after the value round's draw".into(),
            ));
        }
        // ATT-R1-F2: a fixed fact closes on its first settled round, and its
        // rounds without a value are served with their reveals for free; a
        // value for it would be one a free answer already gave away.
        if !self.template.is_dated() && self.attempts.contains_key(&checked.fact) {
            return Err(ObservationError::Malformed(
                "a value round for a fixed fact that settled a round without a value".into(),
            ));
        }
        self.facts.insert(checked.fact);
        self.value_attempts
            .insert(checked.fact, checked.draw.attempt);
        self.subjects.insert(text.clone(), t.subject.clone());
        self.records.insert(
            (text, checked.spec.window_end),
            ObservationRecord {
                transcript: t,
                checked,
            },
        );
        Ok(())
    }

    /// Check `a` (a round that settled no value) at `now` and add it.
    /// Refuses a round that does not verify, a second round for one (fact,
    /// attempt), a round at or after the fact's value round, and a subject
    /// whose text another subject variant already uses.
    pub fn add_attempt(&mut self, a: Attempt, now: u64) -> Result<(), ObservationError> {
        let checked = check_attempt(&self.template, &a, now)?;
        let text = subject_text(&a.subject).to_owned();
        if self.subjects.get(&text).is_some_and(|s| *s != a.subject) {
            return Err(ObservationError::Malformed(
                "one subject text for two subjects".into(),
            ));
        }
        if self
            .value_attempts
            .get(&checked.fact)
            .is_some_and(|v| *v <= checked.draw.attempt || !self.template.is_dated())
        {
            return Err(ObservationError::Malformed(
                "a round without a value at or after the value round's draw".into(),
            ));
        }
        let per_fact = self.attempts.entry(checked.fact).or_default();
        if per_fact.contains_key(&checked.draw.attempt) {
            return Err(ObservationError::Malformed(
                "a second round for one draw attempt".into(),
            ));
        }
        self.subjects.insert(text.clone(), a.subject.clone());
        self.attempt_facts
            .insert((text, checked.window_end), checked.fact);
        per_fact.insert(
            checked.draw.attempt,
            AttemptRecord {
                attempt: a,
                checked,
            },
        );
        Ok(())
    }

    /// The published rounds without a value of `fact`, by ascending attempt.
    pub fn attempts_of(&self, fact: &Hash32) -> Vec<AttemptRecord> {
        self.attempts
            .get(fact)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The fact of `subject`'s rounds without a value: at `window_end` if
    /// given, else the newest window's.
    pub fn attempt_fact(&self, subject: &str, window_end: Option<u64>) -> Option<Hash32> {
        match window_end {
            Some(w) => self.attempt_facts.get(&(subject.to_owned(), w)).copied(),
            None => self
                .attempt_facts
                .range((subject.to_owned(), 0)..=(subject.to_owned(), u64::MAX))
                .next_back()
                .map(|(_, f)| *f),
        }
    }

    /// Whether `fact` has a served value round.
    pub fn has_value(&self, fact: &Hash32) -> bool {
        self.value_attempts.contains_key(fact)
    }

    /// Whether `fact` is closed for good, so the reveals of its rounds can
    /// never give away a value sold later (ATT-R1-F2): it has a served value,
    /// or it is a fixed fact with a round without a value (a fixed fact
    /// closes on its first settled round, and [`Self::add`] refuses a value
    /// for it after one). A dated or period fact without a value is never
    /// closed here: a capped one can be reopened (`reopenDraws`), so its
    /// rounds are served sealed.
    pub fn fact_closed(&self, fact: &Hash32) -> bool {
        self.has_value(fact) || (!self.template.is_dated() && self.attempts.contains_key(fact))
    }

    /// The record for `subject`: at `window_end` if given, else the newest
    /// whose window ends at or before `as_of` (all when `None`), like a
    /// shard's `as_of` (L2API-R3-F3).
    pub fn record(
        &self,
        subject: &str,
        window_end: Option<u64>,
        as_of: Option<u64>,
    ) -> Option<&ObservationRecord> {
        match window_end {
            Some(w) => self.records.get(&(subject.to_owned(), w)),
            None => self
                .records
                .range((subject.to_owned(), 0)..=(subject.to_owned(), as_of.unwrap_or(u64::MAX)))
                .next_back()
                .map(|(_, r)| r),
        }
    }

    /// Whether the source's rights let the server deliver it at all.
    pub fn redistribution(&self) -> Redistribution {
        self.template.rights.redistribution
    }

    /// The template and on-chain pointer without the records: what a
    /// request needs besides its one record, so serving one costs nothing in
    /// the store's size (AUD-2-F4).
    pub fn head(&self) -> ObservationFeed {
        ObservationFeed {
            template: self.template.clone(),
            records: BTreeMap::new(),
            onchain: self.onchain,
            facts: BTreeSet::new(),
            subjects: BTreeMap::new(),
            value_attempts: BTreeMap::new(),
            attempts: BTreeMap::new(),
            attempt_facts: BTreeMap::new(),
        }
    }

    /// The response body for `r` (docs/spec/delivery.md §Observations).
    pub fn body(&self, r: &ObservationRecord) -> Value {
        let t = &r.transcript;
        let c = &r.checked;
        let rights = &self.template.rights;
        let mut v = json!({
            "metric": self.metric(),
            "subject": hex0x(&borsh::to_vec(&t.subject).unwrap_or_default()),
            "subjectText": subject_text(&t.subject),
            "windowStart": t.window_start,
            "windowEnd": c.spec.window_end,
            "config": config_json(&t.cfg),
            "pool": observers_json(&t.pool),
            "draw": draw_json(&t.draw),
            "observers": observers_json(&t.observers),
            "commits": commits_json(&t.commits),
            "reveals": reveals_json(&t.reveals),
            "answer": hex0x(&borsh::to_vec(&t.answer).unwrap_or_default()),
            "value": answer_text(self.template.kind, &t.answer),
            "proofLevel": OBSERVED_LEVEL,
            "ladder": own_ladder(&c.spec, &t.answer, c.result, c.settled_unix)
                .map(|l| crate::ladder::to_json(&l, crate::ladder::OBSERVATION_RECORD)),
            "templateId": self.template.id().to_string(),
            "specId": c.spec.id().to_string(),
            "roundId": c.round_id.to_string(),
            "resultHash": c.result.to_string(),
            "factKey": c.fact.to_string(),
            "registryFactKey": c.registry_fact.to_string(),
            "settledUnix": c.settled_unix,
            "rights": hex0x(&borsh::to_vec(rights).unwrap_or_default()),
            "license": rights.license,
            "attribution": rights.attribution,
            "onchain": self.onchain.map(|p| json!({
                "chainId": p.chain_id,
                "registry": hex0x(&p.registry),
                "call": OBSERVATION_LEVEL_CALL,
                // L2RND-R3-F3: level 2 alone is not enough; the client asks
                // all four (`client::verify_observation_onchain`).
                "drawMatches": DRAW_MATCHES_CALL,
                "drawBudget": DRAW_BUDGET_CALL,
                "transcriptComplete": TRANSCRIPT_COMPLETE_CALL,
                "drawRevealKey": DRAW_REVEAL_KEY_CALL,
                "revealLogComplete": REVEAL_LOG_COMPLETE_CALL,
            })),
            // A value's fact is closed: its rounds come with their reveals.
            "limits": limits_json(false),
        });
        // ROADMAP C11: the round's sealed posts and dismissals, when it has any.
        if !t.sealed.is_empty() {
            v["sealed"] = sealed_json(&t.sealed);
        }
        v
    }

    /// [`Self::body`] with every published round of the fact that settled no
    /// value (`?outcome=all`, SEC-R2-F1): `attempts`, by ascending draw
    /// attempt, each before the value round's attempt.
    pub fn body_all(&self, r: &ObservationRecord, attempts: &[AttemptRecord]) -> Value {
        let mut v = self.body(r);
        v["attempts"] = json!(attempts
            .iter()
            .map(attempt_response_json)
            .collect::<Vec<_>>());
        v
    }

    /// The free answer of `/v1/observations/{metric}/attempts` for a fact
    /// that settled no value (delivery.md §Observations): every published
    /// round of it, by ascending draw attempt. With their reveals only when
    /// the fact is `closed` ([`Self::fact_closed`]); else sealed (ATT-R1-F2).
    pub fn attempts_body(&self, attempts: &[AttemptRecord], closed: bool) -> Value {
        let round = if closed {
            attempt_response_json
        } else {
            sealed_attempt_json
        };
        let first = attempts.first().map(|a| &a.checked);
        json!({
            "metric": self.metric(),
            "subjectText": attempts.first().map(|a| subject_text(&a.attempt.subject)),
            "templateId": self.template.id().to_string(),
            "factKey": first.map(|c| c.fact.to_string()),
            "registryFactKey": first.map(|c| c.registry_fact.to_string()),
            "attempts": attempts.iter().map(round).collect::<Vec<_>>(),
            "onchain": self.onchain.map(|p| json!({
                "chainId": p.chain_id,
                "registry": hex0x(&p.registry),
                "drawCount": DRAW_COUNT_CALL,
                "drawOf": DRAW_OF_CALL,
                "drawBudget": DRAW_BUDGET_CALL,
                "transcriptComplete": TRANSCRIPT_COMPLETE_CALL,
                "drawRevealKey": DRAW_REVEAL_KEY_CALL,
                "revealLogComplete": REVEAL_LOG_COMPLETE_CALL,
                "attemptMatches": ATTEMPT_MATCHES_CALL,
                "drawTimeLocked": DRAW_TIME_LOCKED_CALL,
            })),
            "limits": limits_json(!closed),
        })
    }
}

/// `OnyxRegistry.drawCount(bytes32)`: draws announced for a fact.
pub const DRAW_COUNT_CALL: &str = "OnyxRegistry.drawCount(bytes32)";

/// `OnyxRegistry.drawOf(bytes32,uint32)`: a fact's announced draw.
pub const DRAW_OF_CALL: &str = "OnyxRegistry.drawOf(bytes32,uint32)";

/// Calldata of `OnyxRevenue.registerObservation(fact)` (R-REV-12): opens the
/// revenue book `(OBSERVATION_ROUND, fact)` that sales of a level-2 fact are
/// credited to. A reporter `Registrar` maps receipts of `OBSERVATION_ROUND`
/// skipped with why 4 to this call (`crate::reporter::Registrar`).
pub fn register_observation_calldata(fact: &Hash32) -> Vec<u8> {
    let mut d = onyx_core::keccak256(b"registerObservation(bytes32)").0[..4].to_vec();
    d.extend_from_slice(&fact.0);
    d
}

/// The results store file the level-2 round runner writes and the server
/// loads: the metric's template and each settled round's transcript. Derived
/// values are not stored; they are recomputed at load.
pub struct ObservationStore;

impl ObservationStore {
    /// The JSON of a store for `template` with `rounds` (what a runner writes).
    pub fn to_json(
        template: &FeedTemplate,
        rounds: &[Transcript],
        onchain: Option<OnchainPointer>,
    ) -> Value {
        Self::to_json_with_attempts(template, rounds, &[], onchain)
    }

    /// [`Self::to_json`] with the rounds that settled no value (`attempts`,
    /// SEC-R2-F1); the key is left out when there are none.
    pub fn to_json_with_attempts(
        template: &FeedTemplate,
        rounds: &[Transcript],
        attempts: &[Attempt],
        onchain: Option<OnchainPointer>,
    ) -> Value {
        let mut v = json!({
            "version": STORE_VERSION,
            "template": hex0x(&borsh::to_vec(template).unwrap_or_default()),
            "onchain": onchain.map(|p| json!({ "chainId": p.chain_id, "registry": hex0x(&p.registry) })),
            "rounds": rounds.iter().map(transcript_json).collect::<Vec<_>>(),
        });
        if !attempts.is_empty() {
            v["attempts"] = json!(attempts.iter().map(attempt_json).collect::<Vec<_>>());
        }
        v
    }

    /// Review R9-F1: the draws the results store `store` (as
    /// [`Self::append`] writes it, for `template`) publishes for one fact,
    /// a round or a failed attempt each: the entries of `subject` (and of
    /// `window_start` for an undated template, as [`Self::append`] keys a
    /// draw). A runner hands them to
    /// `ObservationResults::note_published_draws` before a run, so a lost
    /// book never adopts a chain draw whose round is already published,
    /// and never draws again for a fact whose value is published.
    ///
    /// Review R10-1: `onchain` is the deployment the runner announces draws
    /// on (`None`: a replay without a chain). A store naming another
    /// deployment ([`Self::tag_of`]) is refused: a fresh chain gives the
    /// same beacon heights, so another deployment's draws would pass for
    /// this one's. A store naming none is refused when `onchain` is `Some`
    /// (a store written before review R10, or by a replay): the operator
    /// tags it (`--tag-store`, [`Self::check_on_chain`] then
    /// [`Self::tagged`]). Review R11-1: the deployment includes the
    /// registry's deploy block hash (every fresh rehearsal anvil has the
    /// same chain id and registry address); a store tagged before review
    /// R11 (no deploy block) is refused the same way and tagged again.
    pub fn published_draws(
        store: &Value,
        template: &FeedTemplate,
        subject: &Subject,
        window_start: u64,
        onchain: Option<RegistryDeployment>,
    ) -> Result<Vec<PublishedDraw>, ObservationError> {
        let want = Self::to_json_with_attempts(template, &[], &[], None);
        if store["version"].as_u64() != Some(STORE_VERSION) || store["template"] != want["template"]
        {
            return Err(ObservationError::Malformed(
                "published draws: another store version or template".into(),
            ));
        }
        let named = Self::tag_of(store)?;
        if named != onchain.map(StoreTag::of) {
            return Err(ObservationError::Malformed(format!(
                "published draws: the results store names {}, this run's deployment is {} (review R10-1, R11-1: another deployment's store, or an untagged one, is never read as this one's; tag this registry's store with --tag-store)",
                tag_text(named),
                tag_text(onchain.map(StoreTag::of))
            )));
        }
        let subject = hex0x(&borsh::to_vec(subject).unwrap_or_default());
        let mut out = vec![];
        for key in ["rounds", "attempts"] {
            for e in store[key].as_array().map(Vec::as_slice).unwrap_or_default() {
                if e["subject"].as_str() != Some(subject.as_str())
                    || (!template.is_dated() && e["windowStart"].as_u64() != Some(window_start))
                {
                    continue;
                }
                out.push(PublishedDraw {
                    draw: attempt_of(e)?.draw,
                    value: key == "rounds",
                });
            }
        }
        Ok(out)
    }

    /// Review R10-3: the results store file at `path`, as a runner reads it
    /// before a run and before each write. A missing file is `None` only
    /// with `new_store` (the operator's `--new-store`: a fact's first run on
    /// a deployment), else an error: a mistyped path read as an empty store
    /// would let a lost book adopt a draw whose round the real store
    /// publishes (REVLOG-R9-F1).
    pub fn read_file(path: &std::path::Path, new_store: bool) -> Result<Option<Value>, String> {
        let shown = path.display();
        match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t)
                .map(Some)
                .map_err(|e| format!("{shown}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && new_store => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
                "{shown}: no such results store; check the path, or pass --new-store on a fact's first run on this deployment (review R10-3)"
            )),
            Err(e) => Err(format!("{shown}: {e}")),
        }
    }

    /// The deployment the store `store` names (`onchain`: chain id and
    /// registry), `None` when it names none: the pointer a client is
    /// served.
    pub fn onchain_of(store: &Value) -> Result<Option<OnchainPointer>, ObservationError> {
        Ok(Self::tag_of(store)?.map(|t| t.pointer))
    }

    /// The deployment tag of `store` (`onchain`: chain id, registry and,
    /// since review R11-1, `deployBlock`), `None` when it names none.
    pub fn tag_of(store: &Value) -> Result<Option<StoreTag>, ObservationError> {
        match &store["onchain"] {
            Value::Null => Ok(None),
            p => {
                only(p, &["chainId", "registry", "deployBlock"], "onchain")?;
                Ok(Some(StoreTag {
                    pointer: OnchainPointer {
                        chain_id: p["chainId"]
                            .as_u64()
                            .ok_or_else(|| mal("onchain.chainId"))?,
                        registry: addr(&p["registry"], "onchain.registry")?,
                    },
                    deploy_block: match &p["deployBlock"] {
                        Value::Null => None,
                        b => Some(h32_of(b, "onchain.deployBlock")?),
                    },
                }))
            }
        }
    }

    /// Review R10-1: `store` naming the deployment `onchain` (a runner tags
    /// every store it writes with the chain it announces draws on). A store
    /// naming none is tagged; one naming the same is returned as it is; one
    /// naming another deployment is refused. `None` leaves the store as it
    /// is. Review R11-1: a store tagged before review R11 with this chain
    /// and registry but no deploy block takes the deploy block (`--tag-store`
    /// only: a run refuses such a store first, [`Self::published_draws`]).
    pub fn tagged(
        mut store: Value,
        onchain: Option<RegistryDeployment>,
    ) -> Result<Value, ObservationError> {
        let Some(d) = onchain else {
            return Ok(store);
        };
        let want = StoreTag::of(d);
        match Self::tag_of(&store)? {
            Some(named) if named == want => Ok(store),
            Some(named) if named.pointer == want.pointer && named.deploy_block.is_none() => {
                store["onchain"] = want.json();
                Ok(store)
            }
            Some(named) => Err(ObservationError::Malformed(format!(
                "the results store names {}, not {}",
                tag_text(Some(named)),
                tag_text(Some(want))
            ))),
            None => {
                store["onchain"] = want.json();
                Ok(store)
            }
        }
    }

    /// Review R11-2: every draw the results store `store` (for `template`)
    /// publishes, a round or a failed attempt each, with the arguments the
    /// registry keys it by and the commitment its announcement must hold.
    pub fn store_draws(
        store: &Value,
        template: &FeedTemplate,
    ) -> Result<Vec<StoreDraw>, ObservationError> {
        let want = Self::to_json_with_attempts(template, &[], &[], None);
        if store["version"].as_u64() != Some(STORE_VERSION) || store["template"] != want["template"]
        {
            return Err(ObservationError::Malformed(
                "store draws: another store version or template".into(),
            ));
        }
        let mut out = vec![];
        for key in ["rounds", "attempts"] {
            for e in store[key].as_array().map(Vec::as_slice).unwrap_or_default() {
                let a = attempt_of(e)?;
                let spec = template
                    .instantiate(&a.subject, a.window_start)
                    .map_err(|e| ObservationError::Transcript(e.to_string()))?;
                out.push(StoreDraw {
                    fact: OnchainFact {
                        metric_key: metric_key(&spec.metric),
                        subject_key: subject_key(&spec.subject),
                        window_end: spec.window_end,
                        dated: template.is_dated(),
                        template: template.id(),
                    },
                    commit: onyx_coordinator::observe::draw_commit(
                        &a.draw.pool,
                        &spec.id(),
                        &onyx_coordinator::observe::cfg_hash(&a.cfg),
                    ),
                    draw: a.draw,
                    value: key == "rounds",
                });
            }
        }
        Ok(out)
    }

    /// Review R11-2: the check `--tag-store` runs before it tags `store`
    /// with `chain`'s deployment: every draw the store publishes
    /// ([`Self::store_draws`]) must be the chain's `drawOf` at that attempt,
    /// with the same beacon height, the same commitment and the same
    /// nonzero beacon. Any difference (another deployment's store, or a node
    /// that has not caught up) refuses the whole store; nothing is tagged.
    /// Returns how many draws were checked.
    ///
    /// Review R12 (REVLOG-R12-1): the chain must also hold no draw PAST the
    /// store's. For every fact the store publishes and every fact of
    /// `runner` (the runner's facts, each with the attempt its book holds,
    /// if any), the chain's `drawCount`
    /// ([`onyx_coordinator::observe::draw_count_seen`]: the larger of the
    /// latest and a final block's) may exceed neither the store's latest
    /// attempt + 1 nor the book's held attempt + 1. An empty store on a
    /// chain that already shows draws of its fact (a lost or mistyped store
    /// path, another registry's store) is refused, not tagged.
    pub fn check_on_chain(
        store: &Value,
        template: &FeedTemplate,
        runner: &[RunnerFact],
        chain: &dyn DrawChain,
    ) -> Result<usize, String> {
        let draws = Self::store_draws(store, template).map_err(|e| e.to_string())?;
        let mut facts: RunnerFacts = runner.to_vec();
        for d in &draws {
            match facts.iter_mut().find(|(f, _)| *f == d.fact) {
                Some((_, held)) => *held = (*held).max(Some(d.draw.attempt)),
                None => facts.push((d.fact, Some(d.draw.attempt))),
            }
        }
        for (fact, known) in &facts {
            let latest = draws
                .iter()
                .filter(|d| d.fact == *fact)
                .map(|d| d.draw.attempt)
                .max();
            let allowed = (*known).max(latest).map_or(0, |a| a.saturating_add(1));
            let n = onyx_coordinator::observe::draw_count_seen(
                chain,
                fact,
                &onyx_coordinator::observe::ChainWait::DEFAULT,
            )?;
            if n > allowed {
                return Err(format!(
                    "the chain shows {n} draw(s) of a fact whose latest draw the results store publishes (or the book holds) is {}: draws past the store's (a lost or mistyped store, or another registry's; review R12); nothing tagged",
                    latest.max(*known).map_or("none".to_string(), |a| a.to_string())
                ));
            }
        }
        for d in &draws {
            let a = d.draw.attempt;
            let Some(rec) = chain.draw_of(&d.fact, a)? else {
                return Err(format!(
                    "the results store publishes draw {a}, this chain has no draw {a} of the fact (review R11-2: not this registry's store)"
                ));
            };
            let why = if rec.attempt != a {
                Some(format!("attempt {} read for {a}", rec.attempt))
            } else if rec.beacon_height != d.draw.beacon_height {
                Some(format!(
                    "beacon height {} on chain, {} in the store",
                    rec.beacon_height, d.draw.beacon_height
                ))
            } else if rec.commit != d.commit {
                Some(format!(
                    "commitment {} on chain, {} from the store",
                    rec.commit, d.commit
                ))
            } else if d.draw.beacon == Hash32::ZERO || rec.beacon != d.draw.beacon {
                Some(format!(
                    "beacon {} on chain, {} in the store",
                    rec.beacon, d.draw.beacon
                ))
            } else {
                None
            };
            if let Some(why) = why {
                return Err(format!(
                    "the results store's draw {a} is not this chain's: {why} (review R11-2: another registry's store, or a node behind the chain; nothing tagged)"
                ));
            }
        }
        Ok(draws.len())
    }

    /// Review R11-2: `--tag-store`, a mode of its own (the runners exit
    /// after it): each results store file of `files` (path and template)
    /// must exist, and every draw each publishes must match `chain`
    /// ([`Self::check_on_chain`]); only then is each tagged with `chain`'s
    /// deployment ([`Self::tagged`]) and written atomically. Any missing
    /// file or mismatch refuses all of them: nothing is written. Returns
    /// how many draws each publishes. `runner` per file: the runner's facts
    /// and the draw its book holds of each ([`Self::check_on_chain`]). A
    /// file the tag leaves unchanged is not written again (review R12).
    pub fn tag_files(
        files: &[(std::path::PathBuf, FeedTemplate, RunnerFacts)],
        chain: &dyn DrawChain,
    ) -> Result<Vec<usize>, String> {
        let Some(d) = chain.deployment()? else {
            return Err(
                "--tag-store needs --rpc: a results store is tagged with the chain's deployment"
                    .into(),
            );
        };
        let mut tagged = vec![];
        for (path, template, runner) in files {
            let shown = path.display();
            let v = Self::read_file(path, false)?.ok_or(format!("{shown}: no results store"))?;
            let n = Self::check_on_chain(&v, template, runner, chain)
                .map_err(|e| format!("{shown}: {e}"))?;
            let t = Self::tagged(v.clone(), Some(d)).map_err(|e| format!("{shown}: {e}"))?;
            tagged.push((path, (t != v).then_some(t), n));
        }
        let mut counts = vec![];
        for (path, t, n) in tagged {
            if let Some(t) = t {
                let text = serde_json::to_string_pretty(&t).map_err(|e| e.to_string())?;
                onyx_coordinator::observe::write_atomic(path, &format!("{text}\n"))?;
            }
            counts.push(n);
        }
        Ok(counts)
    }

    /// Add `rounds` and `attempts` to the store `prev` (as a runner wrote it
    /// before, for `template`): rounds are appended, never replaced or
    /// dropped (ATT-R1-F1), so a re-draw's store keeps every earlier draw's
    /// round and a client can match each draw on chain. `prev` must be a
    /// store of this version and template; the loader
    /// ([`Self::from_json`]) still checks every round and refuses a second
    /// round for one draw. Idempotent (review REVLOG-R4-1): an entry equal to
    /// one already in `prev` is not added again, so a runner that crashed
    /// after writing the store and publishes the same round again on restart
    /// leaves the store as it was.
    pub fn append(
        prev: &Value,
        template: &FeedTemplate,
        rounds: &[Transcript],
        attempts: &[Attempt],
    ) -> Result<Value, ObservationError> {
        let add = Self::to_json_with_attempts(template, rounds, attempts, None);
        if prev["version"].as_u64() != Some(STORE_VERSION) || prev["template"] != add["template"] {
            return Err(ObservationError::Malformed(
                "append: another store version or template".into(),
            ));
        }
        let mut out = prev.clone();
        for key in ["rounds", "attempts"] {
            let Some(more) = add[key].as_array() else {
                continue;
            };
            let list = match out.get_mut(key) {
                Some(Value::Array(a)) => a,
                Some(_) => return Err(mal(key)),
                None => {
                    out[key] = json!([]);
                    out[key].as_array_mut().ok_or_else(|| mal(key))?
                }
            };
            for m in more {
                if !list.contains(m) {
                    list.push(m.clone());
                }
            }
        }
        // Review REVLOG-R5 (info): one entry per draw. An identical entry
        // was skipped above (a retried publication); a second, DIFFERENT
        // entry for a draw already in the store (a round and a failed
        // attempt of one draw, or two versions of one round) is refused
        // here, at the write, not only when the store is loaded.
        let dated = template.is_dated();
        let mut seen = BTreeSet::new();
        for key in ["rounds", "attempts"] {
            for e in out[key].as_array().map(Vec::as_slice).unwrap_or_default() {
                let window = if dated {
                    &Value::Null
                } else {
                    &e["windowStart"]
                };
                let draw = format!("{}|{window}|{}", e["subject"], e["draw"]["attempt"]);
                if !seen.insert(draw) {
                    return Err(ObservationError::Malformed(
                        "append: a second, different entry for one draw".into(),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Parse and check a store at the server's clock `now`: its template must
    /// be the one pinned for its metric (`pins`, L2API-R4-F1), every round must
    /// verify against it, and none may settle after `now` (L2API-R1-F2). A
    /// source whose rights forbid redistribution is refused here, so it never
    /// enters the catalog.
    pub fn from_json(
        text: &str,
        now: u64,
        pins: &TemplatePins,
    ) -> Result<ObservationFeed, ObservationError> {
        let v: Value = serde_json::from_str(text)
            .map_err(|e| ObservationError::Malformed(format!("store: {e}")))?;
        only(
            &v,
            &["version", "template", "onchain", "rounds", "attempts"],
            "store",
        )?;
        if v["version"].as_u64() != Some(STORE_VERSION) {
            return Err(ObservationError::Malformed(format!(
                "store version (this build reads {STORE_VERSION})"
            )));
        }
        let template: FeedTemplate = borsh_of(&v["template"], "template")?;
        match pins.get(&template.metric.0) {
            None => {
                return Err(ObservationError::TemplateNotPinned {
                    metric: template.metric.0.clone(),
                    why: "no template is pinned for this metric".into(),
                })
            }
            Some(id) if *id != template.id() => {
                return Err(ObservationError::TemplateNotPinned {
                    metric: template.metric.0.clone(),
                    why: format!("the store has {}, the pin is {id}", template.id()),
                })
            }
            Some(_) => {}
        }
        let mut feed = ObservationFeed::new(template)?;
        if feed.redistribution() == Redistribution::Forbidden {
            return Err(ObservationError::Malformed(
                "the source's rights forbid redistribution".into(),
            ));
        }
        feed.onchain = Self::onchain_of(&v)?;
        for r in v["rounds"].as_array().ok_or_else(|| mal("rounds"))? {
            transcript_store_keys(r)?;
            feed.add(transcript_of(r)?, now)?;
        }
        if !v["attempts"].is_null() {
            for a in v["attempts"].as_array().ok_or_else(|| mal("attempts"))? {
                only(a, &ROUND_KEYS[..ROUND_KEYS.len() - 1], "attempts[]")?;
                feed.add_attempt(attempt_of(a)?, now)?;
            }
        }
        Ok(feed)
    }
}

/// The deployment a results store names (review R10-1, R11-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreTag {
    /// Chain id and registry.
    pub pointer: OnchainPointer,
    /// The registry's deploy block hash; `None` in a store tagged before
    /// review R11.
    pub deploy_block: Option<Hash32>,
}

impl StoreTag {
    /// The tag of deployment `d`.
    pub fn of(d: RegistryDeployment) -> StoreTag {
        StoreTag {
            pointer: OnchainPointer {
                chain_id: d.chain_id,
                registry: d.registry,
            },
            deploy_block: Some(d.deploy_block),
        }
    }

    fn json(&self) -> Value {
        let mut v = json!({
            "chainId": self.pointer.chain_id,
            "registry": hex0x(&self.pointer.registry),
        });
        if let Some(b) = self.deploy_block {
            v["deployBlock"] = json!(b.to_string());
        }
        v
    }
}

/// Review R12: a fact a runner announces draws of, with the attempt its
/// book holds of it (`None`: none), for `--tag-store`'s draw-count check
/// ([`ObservationStore::check_on_chain`]).
pub type RunnerFact = (OnchainFact, Option<u32>);
/// A runner's facts ([`RunnerFact`]).
pub type RunnerFacts = Vec<RunnerFact>;

/// A draw a results store publishes, as the registry must hold it
/// ([`ObservationStore::store_draws`], review R11-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreDraw {
    /// The registry's fact arguments.
    pub fact: OnchainFact,
    /// The draw (attempt, beacon height, beacon, pool hash).
    pub draw: DrawSeed,
    /// `draw_commit` of the entry's pool, spec and config.
    pub commit: Hash32,
    /// Published as the fact's value round.
    pub value: bool,
}

/// A deployment as an error message names it.
fn tag_text(t: Option<StoreTag>) -> String {
    t.map_or("no deployment".into(), |t| {
        format!(
            "chain {} registry {} deploy block {}",
            t.pointer.chain_id,
            hex0x(&t.pointer.registry),
            t.deploy_block
                .map_or("(none: tagged before review R11)".into(), |b| b.to_string())
        )
    })
}

fn mal(what: &str) -> ObservationError {
    ObservationError::Malformed(what.into())
}

pub(crate) fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Every key of `obj` is in `allowed`, and `obj` is an object.
pub(crate) fn only(obj: &Value, allowed: &[&str], what: &str) -> Result<(), ObservationError> {
    let map = obj.as_object().ok_or_else(|| mal(what))?;
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ObservationError::Malformed(format!(
            "unexpected field {what}.{k}"
        ))),
        None => Ok(()),
    }
}

pub(crate) fn bytes_of(v: &Value, what: &str) -> Result<Vec<u8>, ObservationError> {
    let s = v.as_str().ok_or_else(|| mal(what))?;
    hex::decode(s.strip_prefix("0x").ok_or_else(|| mal(what))?).map_err(|_| mal(what))
}

pub(crate) fn h32_of(v: &Value, what: &str) -> Result<Hash32, ObservationError> {
    let b = bytes_of(v, what)?;
    Ok(Hash32(b.try_into().map_err(|_| mal(what))?))
}

fn addr(v: &Value, what: &str) -> Result<[u8; 20], ObservationError> {
    bytes_of(v, what)?.try_into().map_err(|_| mal(what))
}

fn sig_of(v: &Value, what: &str) -> Result<ObserverSig, ObservationError> {
    bytes_of(v, what)?.try_into().map_err(|_| mal(what))
}

/// Exact Borsh: the whole input, no trailing bytes.
pub(crate) fn borsh_of<T: BorshDeserialize>(v: &Value, what: &str) -> Result<T, ObservationError> {
    let b = bytes_of(v, what)?;
    let mut s = b.as_slice();
    let t = T::deserialize(&mut s).map_err(|_| mal(what))?;
    if !s.is_empty() {
        return Err(mal(what));
    }
    Ok(t)
}

fn u128_of(v: &Value, what: &str) -> Result<u128, ObservationError> {
    let s = v.as_str().ok_or_else(|| mal(what))?;
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(mal(what));
    }
    s.parse().map_err(|_| mal(what))
}

fn config_json(c: &ObserveConfig) -> Value {
    json!({
        "commitClose": c.commit_close,
        "revealClose": c.reveal_close,
        "reward": c.reward.to_string(),
        "outlierSlash": c.outlier_slash.to_string(),
        "missSlash": c.miss_slash.to_string(),
    })
}

fn observers_json(o: &[Observer]) -> Value {
    json!(o
        .iter()
        .map(|o| json!({ "address": hex0x(&o.address), "bond": o.bond.to_string() }))
        .collect::<Vec<_>>())
}

fn draw_json(d: &DrawSeed) -> Value {
    json!({
        "beaconHeight": d.beacon_height,
        "beacon": d.beacon.to_string(),
        "attempt": d.attempt,
    })
}

fn commits_json(c: &[(MinerAddress, Hash32, ObserverSig)]) -> Value {
    json!(c
        .iter()
        .map(|(a, h, sig)| json!({
            "observer": hex0x(a),
            "commit": h.to_string(),
            "sig": hex0x(sig),
        }))
        .collect::<Vec<_>>())
}

fn reveals_json(r: &[(MinerAddress, Observation, Hash32, ObserverSig)]) -> Value {
    json!(r
        .iter()
        .map(|(a, o, s, sig)| json!({
            "observer": hex0x(a),
            "observation": hex0x(&borsh::to_vec(o).unwrap_or_default()),
            "salt": s.to_string(),
            "sig": hex0x(sig),
        }))
        .collect::<Vec<_>>())
}

fn attempt_json(a: &Attempt) -> Value {
    let mut v = json!({
        "subject": hex0x(&borsh::to_vec(&a.subject).unwrap_or_default()),
        "windowStart": a.window_start,
        "config": config_json(&a.cfg),
        "pool": observers_json(&a.pool),
        "draw": draw_json(&a.draw),
        "observers": observers_json(&a.observers),
        "commits": commits_json(&a.commits),
        "reveals": reveals_json(&a.reveals),
    });
    // ROADMAP C11: only a round with sealed posts or a record carries it, so
    // every store and answer from before is unchanged.
    if !a.sealed.is_empty() {
        v["sealed"] = sealed_json(&a.sealed);
    }
    v
}

/// A [`SealedLog`] as stores and answers carry it (`sealed`): `revealKey`
/// (`x ‖ y`, or null), `posts` (`observer`, `ephemeral`, `keyProof`,
/// `postedAt`, `ciphertext`), `dismissals` (`observer`, `shared`, `proof`)
/// and `record` (the attempt digest, or null).
pub fn sealed_json(l: &SealedLog) -> Value {
    let mut v = json!({
        "revealKey": l.reveal_key.map(|k| hex0x(&k)),
        "posts": l.posts.iter().map(|(a, p)| json!({
            "observer": hex0x(a),
            "ephemeral": hex0x(&p.ephemeral),
            "keyProof": hex0x(&p.key_proof),
            "postedAt": p.posted_at,
            "ciphertext": hex0x(&p.ciphertext),
        })).collect::<Vec<_>>(),
        "dismissals": l.dismissals.iter().map(|(a, d)| json!({
            "observer": hex0x(a),
            "shared": hex0x(&d.shared),
            "proof": hex0x(&d.proof),
        })).collect::<Vec<_>>(),
        "record": l.record.map(|r| r.to_string()),
    });
    // ROADMAP C51: only a time-locked draw's log carries `timeLock`, so logs
    // from before are unchanged.
    if let Some(t) = &l.time_lock {
        v["timeLock"] = json!({
            "lockRound": t.lock_round,
            "farRound": t.far_round,
            "beacon": hex0x(&t.signature),
            "openings": t.openings.iter().map(|(a, o)| json!({
                "observer": hex0x(a),
                "sigma": hex0x(&o.sigma),
                "tau": hex0x(&o.secret),
            })).collect::<Vec<_>>(),
        });
    }
    v
}

/// Parse `sealed` (strict keys; absent: an empty log).
pub(crate) fn sealed_log_of(v: &Value) -> Result<SealedLog, ObservationError> {
    if v.is_null() {
        return Ok(SealedLog::default());
    }
    only(
        v,
        &["revealKey", "posts", "dismissals", "record", "timeLock"],
        "sealed",
    )?;
    let fixed = |x: &Value, what: &str, n: usize| -> Result<Vec<u8>, ObservationError> {
        let b = bytes_of(x, what)?;
        if b.len() == n {
            Ok(b)
        } else {
            Err(mal(what))
        }
    };
    let point = |x: &Value, what: &str| -> Result<Point, ObservationError> {
        fixed(x, what, 64)?.try_into().map_err(|_| mal(what))
    };
    let reveal_key = match &v["revealKey"] {
        Value::Null => None,
        k => Some(point(k, "sealed.revealKey")?),
    };
    let posts = v["posts"]
        .as_array()
        .ok_or_else(|| mal("sealed.posts"))?
        .iter()
        .map(|p| {
            only(
                p,
                &[
                    "observer",
                    "ephemeral",
                    "keyProof",
                    "postedAt",
                    "ciphertext",
                ],
                "sealed.posts[]",
            )?;
            Ok((
                addr(&p["observer"], "sealed post observer")?,
                PostedSealed {
                    ephemeral: point(&p["ephemeral"], "sealed post ephemeral")?,
                    key_proof: fixed(&p["keyProof"], "sealed post keyProof", 52)?
                        .try_into()
                        .map_err(|_| mal("sealed post keyProof"))?,
                    posted_at: p["postedAt"]
                        .as_u64()
                        .ok_or_else(|| mal("sealed post postedAt"))?,
                    ciphertext: bytes_of(&p["ciphertext"], "sealed post ciphertext")?,
                },
            ))
        })
        .collect::<Result<Vec<_>, ObservationError>>()?;
    let dismissals = v["dismissals"]
        .as_array()
        .ok_or_else(|| mal("sealed.dismissals"))?
        .iter()
        .map(|d| {
            only(d, &["observer", "shared", "proof"], "sealed.dismissals[]")?;
            Ok((
                addr(&d["observer"], "dismissal observer")?,
                Dismissal {
                    shared: point(&d["shared"], "dismissal shared")?,
                    proof: fixed(&d["proof"], "dismissal proof", 64)?
                        .try_into()
                        .map_err(|_| mal("dismissal proof"))?,
                },
            ))
        })
        .collect::<Result<Vec<_>, ObservationError>>()?;
    let record = match &v["record"] {
        Value::Null => None,
        r => Some(h32_of(r, "sealed.record")?),
    };
    let time_lock = match v.get("timeLock") {
        None => None,
        Some(t) => {
            only(
                t,
                &["lockRound", "farRound", "beacon", "openings"],
                "sealed.timeLock",
            )?;
            let sixteen = |x: &Value, what: &str| -> Result<[u8; 16], ObservationError> {
                fixed(x, what, 16)?.try_into().map_err(|_| mal(what))
            };
            let openings = t["openings"]
                .as_array()
                .ok_or_else(|| mal("sealed.timeLock.openings"))?
                .iter()
                .map(|o| {
                    only(
                        o,
                        &["observer", "sigma", "tau"],
                        "sealed.timeLock.openings[]",
                    )?;
                    Ok((
                        addr(&o["observer"], "opening observer")?,
                        onyx_coordinator::timelock::TlockOpening {
                            sigma: sixteen(&o["sigma"], "opening sigma")?,
                            secret: sixteen(&o["tau"], "opening tau")?,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, ObservationError>>()?;
            Some(onyx_coordinator::timelock::TimeLockRead {
                lock_round: t["lockRound"]
                    .as_u64()
                    .ok_or_else(|| mal("sealed.timeLock.lockRound"))?,
                far_round: t["farRound"]
                    .as_u64()
                    .ok_or_else(|| mal("sealed.timeLock.farRound"))?,
                signature: fixed(&t["beacon"], "sealed.timeLock.beacon", 48)?,
                openings,
            })
        }
    };
    let l = SealedLog {
        reveal_key,
        posts,
        dismissals,
        record,
        time_lock,
    };
    // One encoding per log: an empty one is written as no key at all.
    if l.is_empty() {
        return Err(mal("sealed: empty (leave the key out)"));
    }
    Ok(l)
}

fn transcript_json(t: &Transcript) -> Value {
    let mut v = attempt_json(&t.attempt());
    v["answer"] = json!(hex0x(&borsh::to_vec(&t.answer).unwrap_or_default()));
    v
}

/// A draw a results store publishes ([`ObservationStore::published_draws`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishedDraw {
    /// The draw.
    pub draw: DrawSeed,
    /// Published as the fact's value round (`rounds`), not as an attempt.
    pub value: bool,
}

/// An attempt as served: its transcript and what it rebuilds to.
fn attempt_response_json(r: &AttemptRecord) -> Value {
    let mut v = attempt_json(&r.attempt);
    v["windowEnd"] = json!(r.checked.window_end);
    v["outcome"] = json!(outcome_text(&r.checked.outcome));
    v["roundId"] = json!(r.checked.round_id.to_string());
    v["resultHash"] = json!(r.checked.result.to_string());
    v
}

/// An attempt as served before its fact is closed (ATT-R1-F2): no reveals
/// (observation, page hash, salt), only who revealed.
fn sealed_attempt_json(r: &AttemptRecord) -> Value {
    let mut v = attempt_response_json(r);
    if let Some(m) = v.as_object_mut() {
        m.remove("reveals");
    }
    v["revealed"] = json!(r
        .attempt
        .reveals
        .iter()
        .map(|(a, _, _, _)| hex0x(a))
        .collect::<Vec<_>>());
    v
}

/// Keys of a sealed served attempt (ATT-R1-F2): [`ATTEMPT_RESPONSE_KEYS`]
/// with `revealed` (observer addresses) in place of `reveals`.
pub const SEALED_ATTEMPT_KEYS: &[&str] = &[
    "subject",
    "windowStart",
    "config",
    "pool",
    "draw",
    "observers",
    "commits",
    "sealed",
    "revealed",
    "windowEnd",
    "outcome",
    "roundId",
    "resultHash",
];

/// The round fields of a sealed served attempt (reveals empty) and its
/// `revealed` observers (`v` may carry other keys; the caller checks which).
pub(crate) fn sealed_attempt_of(
    v: &Value,
) -> Result<(Attempt, Vec<MinerAddress>), ObservationError> {
    let revealed = v["revealed"]
        .as_array()
        .ok_or_else(|| mal("revealed"))?
        .iter()
        .map(|o| addr(o, "revealed observer"))
        .collect::<Result<Vec<_>, ObservationError>>()?;
    let mut w = v.clone();
    w["reveals"] = json!([]);
    Ok((attempt_of(&w)?, revealed))
}

/// Keys of a served attempt (`attempts[]` of a response).
pub const ATTEMPT_RESPONSE_KEYS: &[&str] = &[
    "subject",
    "windowStart",
    "config",
    "pool",
    "draw",
    "observers",
    "commits",
    "reveals",
    "sealed",
    "windowEnd",
    "outcome",
    "roundId",
    "resultHash",
];

/// The transcript fields of a store round or a response body (`v` may carry
/// other keys; the caller checks which).
pub(crate) fn transcript_of(v: &Value) -> Result<Transcript, ObservationError> {
    let a = attempt_of(v)?;
    Ok(Transcript {
        subject: a.subject,
        window_start: a.window_start,
        cfg: a.cfg,
        pool: a.pool,
        draw: a.draw,
        observers: a.observers,
        commits: a.commits,
        reveals: a.reveals,
        sealed: a.sealed,
        answer: borsh_of(&v["answer"], "answer")?,
    })
}

/// The round fields of a store round, a store attempt or a served attempt
/// (`v` may carry other keys; the caller checks which).
pub(crate) fn attempt_of(v: &Value) -> Result<Attempt, ObservationError> {
    let c = &v["config"];
    only(
        c,
        &[
            "commitClose",
            "revealClose",
            "reward",
            "outlierSlash",
            "missSlash",
        ],
        "config",
    )?;
    let cfg = ObserveConfig {
        commit_close: c["commitClose"]
            .as_u64()
            .ok_or_else(|| mal("config.commitClose"))?,
        reveal_close: c["revealClose"]
            .as_u64()
            .ok_or_else(|| mal("config.revealClose"))?,
        reward: u128_of(&c["reward"], "config.reward")?,
        outlier_slash: u128_of(&c["outlierSlash"], "config.outlierSlash")?,
        miss_slash: u128_of(&c["missSlash"], "config.missSlash")?,
    };
    let arr = |key: &str| -> Result<&Vec<Value>, ObservationError> {
        v[key].as_array().ok_or_else(|| mal(key))
    };
    let members = |key: &str| -> Result<Vec<Observer>, ObservationError> {
        arr(key)?
            .iter()
            .map(|o| {
                only(o, &["address", "bond"], key)?;
                Ok(Observer {
                    address: addr(&o["address"], "observer address")?,
                    bond: u128_of(&o["bond"], "observer bond")?,
                })
            })
            .collect()
    };
    let observers = members("observers")?;
    let pool = members("pool")?;
    let d = &v["draw"];
    only(d, &["beaconHeight", "beacon", "attempt"], "draw")?;
    // The pool hash is never read: it is recomputed from the served pool.
    let draw = DrawSeed {
        pool: pool_hash(&pool),
        beacon_height: d["beaconHeight"]
            .as_u64()
            .ok_or_else(|| mal("draw.beaconHeight"))?,
        beacon: h32_of(&d["beacon"], "draw.beacon")?,
        attempt: d["attempt"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| mal("draw.attempt"))?,
    };
    let commits = arr("commits")?
        .iter()
        .map(|o| {
            only(o, &["observer", "commit", "sig"], "commits[]")?;
            Ok((
                addr(&o["observer"], "commit observer")?,
                h32_of(&o["commit"], "commit")?,
                sig_of(&o["sig"], "commit sig")?,
            ))
        })
        .collect::<Result<Vec<_>, ObservationError>>()?;
    let reveals = arr("reveals")?
        .iter()
        .map(|o| {
            only(o, &["observer", "observation", "salt", "sig"], "reveals[]")?;
            Ok((
                addr(&o["observer"], "reveal observer")?,
                borsh_of::<Observation>(&o["observation"], "observation")?,
                h32_of(&o["salt"], "salt")?,
                sig_of(&o["sig"], "reveal sig")?,
            ))
        })
        .collect::<Result<Vec<_>, ObservationError>>()?;
    Ok(Attempt {
        subject: borsh_of(&v["subject"], "subject")?,
        window_start: v["windowStart"]
            .as_u64()
            .ok_or_else(|| mal("windowStart"))?,
        cfg,
        pool,
        draw,
        observers,
        commits,
        reveals,
        sealed: sealed_log_of(&v["sealed"])?,
    })
}

/// Keys of a store round; a store attempt has all but the last (`answer`).
const ROUND_KEYS: &[&str] = &[
    "subject",
    "windowStart",
    "config",
    "pool",
    "draw",
    "observers",
    "commits",
    "reveals",
    "sealed",
    "answer",
];

fn transcript_store_keys(v: &Value) -> Result<(), ObservationError> {
    only(v, ROUND_KEYS, "rounds[]")
}
